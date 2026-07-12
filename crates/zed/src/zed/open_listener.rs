use anyhow::{Context as _, Result, anyhow};
use editor::Editor;
use fs::Fs;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures::channel::mpsc;

use gpui::{App, AsyncApp, Global, WindowHandle};
use remote::{RemoteConnectionOptions, WslConnectionOptions};
use std::path::Path;
use std::sync::Arc;
use util::ResultExt;
use util::paths::PathWithPosition;
use workspace::item::ItemHandle;
use workspace::{AppState, MultiWorkspace, OpenResult};

#[derive(Default, Debug)]
pub struct OpenRequest {
    pub kind: Option<OpenRequestKind>,
    pub open_paths: Vec<String>,
    pub remote_connection: Option<RemoteConnectionOptions>,
    pub open_behavior: Option<cli::OpenBehavior>,
}

pub enum OpenRequestKind {
    FocusApp,
}

impl std::fmt::Debug for OpenRequestKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FocusApp => write!(f, "FocusApp"),
        }
    }
}

impl OpenRequest {
    pub fn parse(request: RawOpenRequest, _: &App) -> Result<Self> {
        let mut this = Self::default();

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
            if let Some(file) = url.strip_prefix("file://") {
                this.parse_file_path(file)
            } else if url == "zed://" || url == "zed://open" || url == "zed://open/" {
                this.kind = Some(OpenRequestKind::FocusApp);
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

pub fn navigate_to_positions(
    window: &WindowHandle<MultiWorkspace>,
    items: impl IntoIterator<Item = Option<Box<dyn workspace::item::ItemHandle>>>,
    positions: &[PathWithPosition],
    cx: &mut AsyncApp,
) {
    for (item, path) in items.into_iter().zip(positions) {
        let Some(item) = item else {
            continue;
        };
        let Some(row) = path.row else {
            continue;
        };
        if let Some(active_editor) = item.downcast::<Editor>() {
            window
                .update(cx, |_, window, cx| {
                    active_editor.update(cx, |editor, cx| {
                        let row = row.saturating_sub(1);
                        let col = path.column.unwrap_or(0).saturating_sub(1);
                        let buffer = editor.buffer().read(cx).as_singleton();
                        let buffer_snapshot = buffer.read(cx).snapshot();
                        let point = buffer_snapshot.point_from_external_input(row, col);
                        editor.go_to_singleton_buffer_point(point, window, cx);
                    });
                })
                .ok();
        }
    }
}

pub async fn open_paths_with_positions(
    path_positions: &[PathWithPosition],
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

pub(crate) fn open_options_for_request(
    open_behavior: Option<cli::OpenBehavior>,
    cx: &App,
) -> workspace::OpenOptions {
    open_behavior.map_or_else(workspace::OpenOptions::default, |open_behavior| {
        open_options_for_behavior(open_behavior, cx)
    })
}

pub(crate) fn open_options_for_behavior(
    open_behavior: cli::OpenBehavior,
    cx: &App,
) -> workspace::OpenOptions {
    // If reuse flag is passed, open a new workspace in an existing window.
    let requesting_window = if open_behavior == cli::OpenBehavior::Reuse {
        workspace::workspace_windows_for_location(cx)
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
        requesting_window,
        ..Default::default()
    }
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
