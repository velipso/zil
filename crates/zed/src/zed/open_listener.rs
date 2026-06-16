use crate::handle_open_request;
use crate::restore_or_create_workspace;
use anyhow::{Context as _, Result, anyhow};
use cli::{CliRequest, CliResponse, CliResponseSink};
use cli::{IpcHandshake, ipc};
use client::{ZedLink, parse_zed_link};
use editor::Editor;
use fs::Fs;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::channel::{mpsc, oneshot};
use futures::future;

use futures::{FutureExt, StreamExt};
use gpui::{App, AsyncApp, Global, TaskExt, WindowHandle};
use recent_projects::{navigate_to_positions};
use remote::{RemoteConnectionOptions, WslConnectionOptions};
use settings::Settings;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use util::ResultExt;
use util::debug_panic;
use util::paths::PathWithPosition;
use workspace::PathList;
use workspace::item::ItemHandle;
use workspace::{AppState, MultiWorkspace, OpenOptions, OpenResult, SerializedWorkspaceLocation};

#[derive(Default, Debug)]
pub struct OpenRequest {
    pub kind: Option<OpenRequestKind>,
    pub open_paths: Vec<String>,
    pub diff_paths: Vec<[String; 2]>,
    pub diff_all: bool,
    pub dev_container: bool,
    pub open_channel_notes: Vec<(u64, Option<String>)>,
    pub join_channel: Option<u64>,
    pub remote_connection: Option<RemoteConnectionOptions>,
    pub open_behavior: Option<cli::OpenBehavior>,
}

pub enum OpenRequestKind {
    CliConnection(
        (
            mpsc::UnboundedReceiver<CliRequest>,
            Box<dyn CliResponseSink>,
        ),
    ),
    FocusApp,
    DockMenuAction {
        index: usize,
    },
    BuiltinJsonSchema {
        schema_path: String,
    },
    Setting {
        /// `None` opens settings without navigating to a specific path.
        setting_path: Option<String>,
    },
}

impl std::fmt::Debug for OpenRequestKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CliConnection(_) => write!(f, "CliConnection(..)"),
            Self::FocusApp => write!(f, "FocusApp"),
            Self::DockMenuAction { index } => f
                .debug_struct("DockMenuAction")
                .field("index", index)
                .finish(),
            Self::BuiltinJsonSchema { schema_path } => f
                .debug_struct("BuiltinJsonSchema")
                .field("schema_path", schema_path)
                .finish(),
            Self::Setting { setting_path } => f
                .debug_struct("Setting")
                .field("setting_path", setting_path)
                .finish(),
        }
    }
}

impl OpenRequest {
    pub fn is_focus_app_only(&self) -> bool {
        matches!(self.kind, Some(OpenRequestKind::FocusApp))
            && self.open_paths.is_empty()
            && self.diff_paths.is_empty()
            && self.remote_connection.is_none()
            && self.join_channel.is_none()
            && self.open_channel_notes.is_empty()
    }

    pub fn parse(request: RawOpenRequest, cx: &App) -> Result<Self> {
        let mut this = Self::default();

        this.diff_paths = request.diff_paths;
        this.diff_all = request.diff_all;
        this.dev_container = request.dev_container;
        this.open_behavior = request.open_behavior;
        if let Some(wsl) = request.wsl {
            let (user, distro_name) = if let Some((user, distro)) = wsl.split_once('@') {
                if user.is_empty() {
                    anyhow::bail!("user is empty in wsl argument");
                }
                (Some(user.to_string()), distro.to_string())
            } else {
                (None, wsl)
            };
            this.remote_connection = Some(RemoteConnectionOptions::Wsl(WslConnectionOptions {
                distro_name,
                user,
            }));
        }

        for url in request.urls {
            if let Some(server_name) = url.strip_prefix("zed-cli://") {
                this.kind = Some(OpenRequestKind::CliConnection(connect_to_cli(server_name)?));
            } else if let Some(action_index) = url.strip_prefix("zed-dock-action://") {
                this.kind = Some(OpenRequestKind::DockMenuAction {
                    index: action_index.parse()?,
                });
            } else if let Some(file) = url.strip_prefix("file://") {
                this.parse_file_path(file)
            } else if let Some(file) = url.strip_prefix("zed://file") {
                this.parse_file_path(file)
            } else if url == "zed://" || url == "zed://open" || url == "zed://open/" {
                this.kind = Some(OpenRequestKind::FocusApp);
            } else if let Some(schema_path) = url.strip_prefix("zed://schemas/") {
                this.kind = Some(OpenRequestKind::BuiltinJsonSchema {
                    schema_path: schema_path.to_string(),
                });
            } else if url == "zed://settings" || url == "zed://settings/" {
                this.kind = Some(OpenRequestKind::Setting { setting_path: None });
            } else if let Some(setting_path) = url.strip_prefix("zed://settings/") {
                this.kind = Some(OpenRequestKind::Setting {
                    setting_path: Some(setting_path.to_string()),
                });
            } else if let Some(zed_link) = parse_zed_link(&url, cx) {
                match zed_link {
                    ZedLink::Channel { channel_id } => {
                        this.join_channel = Some(channel_id);
                    }
                    ZedLink::ChannelNotes {
                        channel_id,
                        heading,
                    } => {
                        this.open_channel_notes.push((channel_id, heading));
                    }
                }
            } else {
                log::error!("unhandled url: {}", url);
            }
        }

        Ok(this)
    }

    fn parse_file_path(&mut self, file: &str) {
        if let Some(decoded) = urlencoding::decode(file).log_err() {
            self.open_paths.push(decoded.into_owned())
        }
    }
}

#[derive(Clone)]
pub struct OpenListener(UnboundedSender<RawOpenRequest>);

#[derive(Default)]
pub struct RawOpenRequest {
    pub urls: Vec<String>,
    pub diff_paths: Vec<[String; 2]>,
    pub diff_all: bool,
    pub dev_container: bool,
    pub wsl: Option<String>,
    pub open_behavior: Option<cli::OpenBehavior>,
}

impl Global for OpenListener {}

impl OpenListener {
    pub fn new() -> (Self, UnboundedReceiver<RawOpenRequest>) {
        let (tx, rx) = mpsc::unbounded();
        (OpenListener(tx), rx)
    }

    pub fn open(&self, request: RawOpenRequest) {
        self.0
            .unbounded_send(request)
            .context("no listener for open requests")
            .log_err();
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub fn listen_for_cli_connections(opener: OpenListener) -> Result<()> {
    use release_channel::RELEASE_CHANNEL_NAME;
    use std::os::unix::net::UnixDatagram;

    let sock_path = paths::data_dir().join(format!("zed-{}.sock", *RELEASE_CHANNEL_NAME));
    // remove the socket if the process listening on it has died
    if let Err(e) = UnixDatagram::unbound()?.connect(&sock_path)
        && e.kind() == std::io::ErrorKind::ConnectionRefused
    {
        std::fs::remove_file(&sock_path)?;
    }
    let listener = UnixDatagram::bind(&sock_path)?;
    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while let Ok(len) = listener.recv(&mut buf) {
            opener.open(RawOpenRequest {
                urls: vec![String::from_utf8_lossy(&buf[..len]).to_string()],
                ..Default::default()
            });
        }
    });
    Ok(())
}

fn connect_to_cli(
    server_name: &str,
) -> Result<(
    mpsc::UnboundedReceiver<CliRequest>,
    Box<dyn CliResponseSink>,
)> {
    let handshake_tx = ipc::IpcSender::<IpcHandshake>::connect(server_name.to_string())
        .context("error connecting to cli")?;
    let (request_tx, request_rx) = ipc::channel::<CliRequest>()?;
    let (response_tx, response_rx) = ipc::channel::<CliResponse>()?;

    handshake_tx
        .send(IpcHandshake {
            requests: request_tx,
            responses: response_rx,
        })
        .context("error sending ipc handshake")?;

    let (async_request_tx, async_request_rx) = futures::channel::mpsc::unbounded::<CliRequest>();
    thread::spawn(move || {
        while let Ok(cli_request) = request_rx.recv() {
            if async_request_tx.unbounded_send(cli_request).is_err() {
                break;
            }
        }
        anyhow::Ok(())
    });

    Ok((async_request_rx, Box::new(response_tx)))
}

pub async fn open_paths_with_positions(
    path_positions: &[PathWithPosition],
    _diff_paths: &[[String; 2]],
    _diff_all: bool,
    app_state: Arc<AppState>,
    open_options: workspace::OpenOptions,
    cx: &mut AsyncApp,
) -> Result<(
    WindowHandle<MultiWorkspace>,
    Vec<Option<Result<Box<dyn ItemHandle>>>>,
)> {
    let paths = path_positions
        .iter()
        .map(|path_with_position| path_with_position.path.clone())
        .collect::<Vec<_>>();

    let OpenResult {
        window: multi_workspace,
        opened_items: mut items,
        ..
    } = cx
        .update(|cx| workspace::open_paths(&paths, app_state.clone(), open_options, cx))
        .await?;

    for (item, path) in items.iter_mut().zip(&paths) {
        if let Some(Err(error)) = item {
            *error = anyhow!("error opening {path:?}: {error:#}");
        }
    }

    let items_for_navigation = items
        .iter()
        .map(|item| item.as_ref().and_then(|r| r.as_ref().ok()).cloned())
        .collect::<Vec<_>>();
    navigate_to_positions(&multi_workspace, items_for_navigation, path_positions, cx);

    Ok((multi_workspace, items))
}

pub async fn handle_cli_connection(
    (mut requests, responses): (
        mpsc::UnboundedReceiver<CliRequest>,
        Box<dyn CliResponseSink>,
    ),
    app_state: Arc<AppState>,
    cx: &mut AsyncApp,
) {
    if let Some(request) = requests.next().await {
        match request {
            CliRequest::Open {
                urls,
                paths,
                diff_paths,
                diff_all,
                wait,
                wsl,
                mut open_behavior,
                env,
                user_data_dir: _,
                dev_container,
                cwd,
            } => {
                if !urls.is_empty() {
                    cx.update(|cx| {
                        match OpenRequest::parse(
                            RawOpenRequest {
                                urls,
                                diff_paths,
                                diff_all,
                                dev_container,
                                wsl,
                                open_behavior: Some(open_behavior),
                            },
                            cx,
                        ) {
                            Ok(open_request) => {
                                cx.activate(true);
                                handle_open_request(open_request, app_state.clone(), cx);
                                responses.send(CliResponse::Exit { status: 0 }).log_err();
                            }
                            Err(e) => {
                                responses
                                    .send(CliResponse::Stderr {
                                        message: format!("{e}"),
                                    })
                                    .log_err();
                                responses.send(CliResponse::Exit { status: 1 }).log_err();
                            }
                        };
                    });
                    return;
                }

                if open_behavior == cli::OpenBehavior::Default {
                    match resolve_open_behavior(
                        &paths,
                        &app_state,
                        responses.as_ref(),
                        &mut requests,
                        cx,
                    )
                    .await
                    {
                        Some(settings::CliDefaultOpenBehavior::ExistingWindow) => {
                            open_behavior = cli::OpenBehavior::ExistingWindow;
                        }
                        Some(settings::CliDefaultOpenBehavior::NewWindow) => {
                            open_behavior = cli::OpenBehavior::Classic;
                        }
                        None => {}
                    }
                }

                cx.update(|cx| cx.activate(true));

                let open_workspace_result = open_workspaces(
                    paths,
                    diff_paths,
                    diff_all,
                    open_behavior,
                    responses.as_ref(),
                    wait,
                    dev_container,
                    app_state.clone(),
                    env,
                    cwd,
                    cx,
                )
                .await;

                let status = if open_workspace_result.is_err() { 1 } else { 0 };
                responses.send(CliResponse::Exit { status }).log_err();
            }
            CliRequest::SetOpenBehavior { .. } => {
                // We handle this case in a situation-specific way in
                // resolve_open_behavior
                debug_panic!("unexpected SetOpenBehavior message");
            }
        }
    }
}

/// Resolves the CLI open behavior when no explicit flag (`-n`, `-e`, `--reuse`)
/// was given. May prompt the user interactively on first run.
///
/// Returns `Some(behavior)` to override the default, or `None` if no override
/// is needed (e.g. no existing windows, paths already in a workspace, or the
/// user has already configured `cli_default_open_behavior` in settings).
async fn resolve_open_behavior(
    paths: &[String],
    app_state: &Arc<AppState>,
    responses: &dyn CliResponseSink,
    requests: &mut mpsc::UnboundedReceiver<CliRequest>,
    cx: &mut AsyncApp,
) -> Option<settings::CliDefaultOpenBehavior> {
    let has_existing_windows = cx.update(|cx| {
        cx.windows()
            .iter()
            .any(|window| window.downcast::<MultiWorkspace>().is_some())
    });

    if !has_existing_windows {
        return None;
    }

    if !paths.is_empty() {
        let paths_as_pathbufs: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
        let paths_in_existing_workspace = cx.update(|cx| {
            for window in cx.windows() {
                if let Some(multi_workspace) = window.downcast::<MultiWorkspace>() {
                    if let Ok(multi_workspace) = multi_workspace.read(cx) {
                        for workspace in multi_workspace.workspaces() {
                            let project = workspace.read(cx).project().read(cx);
                            if project
                                .visibility_for_paths(&paths_as_pathbufs, false, cx)
                                .is_some()
                            {
                                return true;
                            }
                        }
                    }
                }
            }
            false
        });

        if paths_in_existing_workspace {
            return None;
        }
    }

    if !paths.is_empty() {
        let has_directory =
            futures::future::join_all(paths.iter().map(|p| app_state.fs.is_dir(Path::new(p))))
                .await
                .into_iter()
                .any(|is_dir| is_dir);

        if !has_directory {
            return None;
        }
    }

    let settings_text = app_state
        .fs
        .load(paths::settings_file())
        .await
        .unwrap_or_default();

    if settings_text.contains("cli_default_open_behavior") {
        return None;
    }

    responses.send(CliResponse::PromptOpenBehavior).log_err()?;

    if let Some(CliRequest::SetOpenBehavior { behavior }) = requests.next().await {
        let behavior = match behavior {
            cli::CliBehaviorSetting::ExistingWindow => {
                settings::CliDefaultOpenBehavior::ExistingWindow
            }
            cli::CliBehaviorSetting::NewWindow => settings::CliDefaultOpenBehavior::NewWindow,
        };

        let fs = app_state.fs.clone();
        cx.update(|cx| {
            settings::update_settings_file(fs, cx, move |content, _cx| {
                content.workspace.cli_default_open_behavior = Some(behavior);
            });
        });

        return Some(behavior);
    }

    None
}

pub(crate) fn open_options_for_request(
    open_behavior: Option<cli::OpenBehavior>,
    location: &SerializedWorkspaceLocation,
    cx: &App,
) -> workspace::OpenOptions {
    open_behavior.map_or_else(workspace::OpenOptions::default, |open_behavior| {
        open_options_for_behavior(open_behavior, location, cx)
    })
}

pub(crate) fn open_options_for_behavior(
    open_behavior: cli::OpenBehavior,
    location: &SerializedWorkspaceLocation,
    cx: &App,
) -> workspace::OpenOptions {
    // If reuse flag is passed, open a new workspace in an existing window.
    let requesting_window = if open_behavior == cli::OpenBehavior::Reuse {
        workspace::workspace_windows_for_location(location, cx)
            .into_iter()
            .next()
    } else {
        None
    };
    workspace::OpenOptions {
        workspace_matching: match open_behavior {
            cli::OpenBehavior::AlwaysNew | cli::OpenBehavior::Reuse => {
                workspace::WorkspaceMatching::None
            }
            cli::OpenBehavior::Add => workspace::WorkspaceMatching::MatchSubdirectory,
            _ => workspace::WorkspaceMatching::MatchExact,
        },
        add_dirs_to_sidebar: match open_behavior {
            cli::OpenBehavior::ExistingWindow => true,
            // For the default value, we consult the settings to decide
            // whether to open in a new window or existing window.
            cli::OpenBehavior::Default => {
                workspace::WorkspaceSettings::get_global(cx).cli_default_open_behavior
                    == settings::CliDefaultOpenBehavior::ExistingWindow
            }
            _ => false,
        },
        requesting_window,
        ..Default::default()
    }
}

async fn open_workspaces(
    paths: Vec<String>,
    diff_paths: Vec<[String; 2]>,
    diff_all: bool,
    open_behavior: cli::OpenBehavior,
    responses: &dyn CliResponseSink,
    wait: bool,
    dev_container: bool,
    app_state: Arc<AppState>,
    env: Option<collections::HashMap<String, String>>,
    cwd: Option<PathBuf>,
    cx: &mut AsyncApp,
) -> Result<()> {
    if paths.is_empty() && diff_paths.is_empty() && open_behavior != cli::OpenBehavior::AlwaysNew {
        return restore_or_create_workspace(app_state, cx).await;
    }

    let grouped_locations: Vec<(SerializedWorkspaceLocation, PathList)> =
        if paths.is_empty() && diff_paths.is_empty() {
            Vec::new()
        } else {
            vec![(
                SerializedWorkspaceLocation::Local,
                PathList::new(&paths.into_iter().map(PathBuf::from).collect::<Vec<_>>()),
            )]
        };

    if grouped_locations.is_empty() {
        cx.update(|cx| {
            let open_options = OpenOptions {
                env,
                ..Default::default()
            };
            workspace::open_new(open_options, app_state, cx, |workspace, window, cx| {
                Editor::new_file(workspace, &Default::default(), window, cx)
            })
            .detach_and_log_err(cx);
        });
        return Ok(());
    }
    // If there are paths to open, open a workspace for each grouping of paths
    let mut errored = false;

    for (location, workspace_paths) in grouped_locations {
        let base_open_options =
            cx.update(|cx| open_options_for_behavior(open_behavior, &location, cx));
        let open_options = workspace::OpenOptions {
            wait,
            env: env.clone(),
            open_in_dev_container: dev_container,
            ..base_open_options
        };

        match location {
            SerializedWorkspaceLocation::Local => {
                let workspace_paths = workspace_paths
                    .paths()
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect();

                let workspace_failed_to_open = open_local_workspace(
                    workspace_paths,
                    diff_paths.clone(),
                    diff_all,
                    open_options,
                    cwd.clone(),
                    responses,
                    &app_state,
                    cx,
                )
                .await;

                if workspace_failed_to_open {
                    errored = true
                }
            }
        }
    }

    anyhow::ensure!(!errored, "failed to open a workspace");

    Ok(())
}

async fn open_local_workspace(
    mut workspace_paths: Vec<String>,
    diff_paths: Vec<[String; 2]>,
    diff_all: bool,
    open_options: workspace::OpenOptions,
    cwd: Option<PathBuf>,
    responses: &dyn CliResponseSink,
    app_state: &Arc<AppState>,
    cx: &mut AsyncApp,
) -> bool {
    let user_provided_paths = !workspace_paths.is_empty();

    // When only diff paths are provided (no regular paths), add the CLI's
    // working directory so the workspace opens with the right context.
    // Note: must use the CLI process's cwd (forwarded via `cli_cwd`), not
    // `std::env::current_dir()`, since the Zed app process's cwd is typically
    // `/` on macOS bundles or the launch dir of an already-running instance.
    if !user_provided_paths
        && !diff_paths.is_empty()
        && let Some(cwd) = cwd
    {
        workspace_paths.push(cwd.to_string_lossy().to_string());
    }

    let paths_with_position =
        derive_paths_with_position(app_state.fs.as_ref(), workspace_paths).await;

    let (workspace, items) = match open_paths_with_positions(
        &paths_with_position,
        &diff_paths,
        diff_all,
        app_state.clone(),
        open_options.clone(),
        cx,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            let paths = paths_with_position
                .iter()
                .map(|p| p.path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            log::error!("failed to open workspace [{paths}]: {error:#}");
            responses
                .send(CliResponse::Stderr {
                    message: format!("error opening [{paths}]: {error:#}"),
                })
                .log_err();
            return true;
        }
    };

    let mut errored = false;
    let mut item_release_futures = Vec::new();
    let mut subscriptions = Vec::new();
    // If --wait flag is used with no paths, or a directory, then wait until
    // the entire workspace is closed.
    if open_options.wait {
        let mut wait_for_window_close = paths_with_position.is_empty() && diff_paths.is_empty();
        if user_provided_paths {
            for path_with_position in &paths_with_position {
                if app_state.fs.is_dir(&path_with_position.path).await {
                    wait_for_window_close = true;
                    break;
                }
            }
        }

        if wait_for_window_close {
            let (release_tx, release_rx) = oneshot::channel();
            item_release_futures.push(release_rx);
            subscriptions.push(workspace.update(cx, |_, _, cx| {
                cx.on_release(move |_, _| {
                    let _ = release_tx.send(());
                })
            }));
        }
    }

    for item in items {
        match item {
            Some(Ok(item)) => {
                if open_options.wait {
                    let (release_tx, release_rx) = oneshot::channel();
                    item_release_futures.push(release_rx);
                    subscriptions.push(Ok(cx.update(|cx| {
                        item.on_release(
                            cx,
                            Box::new(move |_| {
                                release_tx.send(()).ok();
                            }),
                        )
                    })));
                }
            }
            Some(Err(err)) => {
                log::error!("{err:#}");
                responses
                    .send(CliResponse::Stderr {
                        message: format!("{err:#}"),
                    })
                    .log_err();
                errored = true;
            }
            None => {}
        }
    }

    if open_options.wait {
        let wait = async move {
            let _subscriptions = subscriptions;
            let _ = future::try_join_all(item_release_futures).await;
        }
        .fuse();
        futures::pin_mut!(wait);

        let background = cx.background_executor().clone();
        loop {
            // Repeatedly check if CLI is still open to avoid wasting resources
            // waiting for files or workspaces to close.
            let mut timer = background.timer(Duration::from_secs(1)).fuse();
            futures::select_biased! {
                _ = wait => break,
                _ = timer => {
                    if responses.send(CliResponse::Ping).is_err() {
                        break;
                    }
                }
            }
        }
    }

    errored
}

pub async fn derive_paths_with_position(
    fs: &dyn Fs,
    path_strings: impl IntoIterator<Item = impl AsRef<str>>,
) -> Vec<PathWithPosition> {
    let path_strings: Vec<_> = path_strings.into_iter().collect();
    let mut result = Vec::with_capacity(path_strings.len());
    for path_str in path_strings {
        let original_path = Path::new(path_str.as_ref());
        let mut parsed = PathWithPosition::parse_str(path_str.as_ref());

        // If the unparsed path string actually points to a file, use that file instead of parsing out the line/col number.
        // Note: The colon syntax is also used to open NTFS alternate data streams (e.g., `file.txt:stream`), which would cause issues.
        // However, the colon is not valid in NTFS file names, so we can just skip this logic.
        if !cfg!(windows)
            && parsed.row.is_some()
            && parsed.path != original_path
            && fs.is_file(original_path).await
        {
            parsed = PathWithPosition::from_path(original_path.to_path_buf());
        }

        if let Ok(canonicalized) = fs.canonicalize(&parsed.path).await {
            parsed.path = canonicalized;
        }

        result.push(parsed);
    }
    result
}
