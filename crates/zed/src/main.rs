// Disable command line from opening on release mode
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod zed;

// Ensure the binary name stays in sync with APP_NAME so that the paths used
// at runtime (data dir, config dir, etc.) match what the binary is called.
const _: () = assert!(
    paths::APP_NAME_LOWERCASE
        .as_bytes()
        .eq_ignore_ascii_case(env!("CARGO_BIN_NAME").as_bytes()),
    "paths::APP_NAME_LOWERCASE must match the binary name. \
     Forks: update APP_NAME in crates/paths/src/paths.rs when renaming the binary.",
);

use anyhow::{Context as _, Result};
use cli::FORCE_CLI_MODE_ENV_VAR_NAME;
use client::{Client, ProxySettings, RefreshLlmTokenListener, UserStore};
use collections::HashMap;
use editor::Editor;
use fs::{Fs, RealFs};
use futures::StreamExt;
use git::GitHostingProviderRegistry;
use gpui::{
    App, AppContext, Application, AssetSource, AsyncApp, QuitMode, TaskExt, UpdateGlobal as _,
};
use gpui_platform;

use gpui_tokio::Tokio;
use language::LanguageRegistry;
use reqwest_client::ReqwestClient;

use assets::Assets;
use parking_lot::Mutex;
use project::trusted_worktrees;
use release_channel::{AppCommitSha, AppVersion};
use session::{AppSession, Session};
use settings::{Settings, SettingsStore, watch_config_file};
use std::{
    env,
    io::{self, IsTerminal},
    path::Path,
    process::{self, Command, Stdio},
    sync::{Arc, LazyLock},
};
use theme::{ActiveTheme, GlobalTheme, ThemeRegistry};
use theme_settings::load_user_theme;
use util::ResultExt;
use uuid::Uuid;
use workspace::{AppState, WorkspaceSettings, WorkspaceStore};
use zed::{
    OpenListener, OpenRequest, RawOpenRequest, app_menus, build_window_options,
    derive_paths_with_position, handle_cli_connection, handle_keymap_file_changes,
    initialize_workspace, open_paths_with_positions,
};

use crate::zed::OpenRequestKind;
use crate::zed::arg_listener::{ArgListenerResult, handle_args, handle_args_exit};

#[cfg(debug_assertions)]
use ui::prelude::IconName;
#[cfg(debug_assertions)]
use strum::IntoEnumIterator;

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn build_application() -> Application {
    let platform = gpui_platform::current_platform(false);
    if std::env::var("ZED_EXPERIMENTAL_A11Y").as_deref() == Ok("1") {
        Application::with_platform(platform)
    } else {
        Application::new_inaccessible(platform)
    }
}

fn files_not_created_on_launch(errors: HashMap<io::ErrorKind, Vec<&Path>>) {
    let message = "Zil failed to launch";
    let error_details = errors
        .into_iter()
        .flat_map(|(kind, paths)| {
            #[allow(unused_mut)] // for non-unix platforms
            let mut error_kind_details = match paths.len() {
                0 => return None,
                1 => format!(
                    "{kind} when creating directory {:?}",
                    paths.first().expect("match arm checks for a single entry")
                ),
                _many => format!("{kind} when creating directories {paths:?}"),
            };

            #[cfg(unix)]
            {
                if kind == io::ErrorKind::PermissionDenied {
                    error_kind_details.push_str("\n\nConsider using chown and chmod tools for altering the directories permissions if your user has corresponding rights.\
                        \nFor example, `sudo chown $(whoami):staff ~/.config` and `chmod +uwrx ~/.config`");
                }
            }

            Some(error_kind_details)
        })
        .collect::<Vec<_>>().join("\n\n");

    eprintln!("{message}: {error_details}");
    build_application()
        .with_quit_mode(QuitMode::Explicit)
        .run(move |cx| {
            if let Ok(window) = cx.open_window(gpui::WindowOptions::default(), |_, cx| {
                cx.new(|_| gpui::Empty)
            }) {
                window
                    .update(cx, |_, window, cx| {
                        let response = window.prompt(
                            gpui::PromptLevel::Critical,
                            message,
                            Some(&error_details),
                            &["Exit"],
                            cx,
                        );

                        cx.spawn_in(window, async move |_, cx| {
                            response.await?;
                            cx.update(|_, cx| cx.quit())
                        })
                        .detach_and_log_err(cx);
                    })
                    .log_err();
            } else {
                fail_to_open_window(anyhow::anyhow!("{message}: {error_details}"), cx)
            }
        })
}

fn fail_to_open_window_async(e: anyhow::Error, cx: &mut AsyncApp) {
    cx.update(|cx| fail_to_open_window(e, cx));
}

fn fail_to_open_window(e: anyhow::Error, _cx: &mut App) {
    eprintln!(
        "Zed failed to open a window: {e:?}. See https://zed.dev/docs/linux for troubleshooting steps."
    );
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        process::exit(1);
    }

    // Maybe unify this with gpui::platform::linux::platform::ResultExt::notify_err(..)?
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        use ashpd::desktop::notification::{Notification, NotificationProxy, Priority};
        _cx.spawn(async move |_cx| {
            let Ok(proxy) = NotificationProxy::new().await else {
                process::exit(1);
            };

            let notification_id = "dev.zed.Oops";
            proxy
                .add_notification(
                    notification_id,
                    Notification::new("Zed failed to launch")
                        .body(Some(
                            format!(
                                "{e:?}. See https://zed.dev/docs/linux for troubleshooting steps."
                            )
                            .as_str(),
                        ))
                        .priority(Priority::High)
                        .icon(ashpd::desktop::Icon::with_names(&[
                            "dialog-question-symbolic",
                        ])),
                )
                .await
                .ok();

            process::exit(1);
        })
        .detach();
    }
}

fn spawn_in_background(args: &[String]) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|err| format!("Failed to find current executable: {err}"))?;

    let mut command = Command::new(exe);

    command
        .args(args.iter())
        .env("ZIL_BACKGROUND_CHILD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        // Put the GUI child in a new process group so terminal signals
        // like Ctrl-C don't hit both the launcher and the GUI.
        command.process_group(0);
    }

    command
        .spawn()
        .map_err(|err| format!("Failed to spawn Zil in background: {err}"))?;

    Ok(())
}

fn main() {
    #[cfg(unix)]
    util::prevent_root_execution();

    let mut args = Vec::<String>::new();
    let mut foreground = false;
    let mut rest_files = false;

    for arg in std::env::args().skip(1) {
        if rest_files {
            args.push(arg);
        } else {
            if arg == "--" {
                rest_files = true;
            } else if arg == "-f" {
                foreground = true;
            } else {
                args.push(arg);
            }
        }
    }

    #[cfg(all(not(debug_assertions), target_os = "windows"))]
    unsafe {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

        if foreground {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }

    let file_errors = init_paths();
    if !file_errors.is_empty() {
        files_not_created_on_launch(file_errors);
        return;
    }

    let in_terminal = stdout_is_a_pty();
    let is_background_child = std::env::var_os("ZIL_BACKGROUND_CHILD").is_some();
    if in_terminal && !is_background_child && !foreground {
        if let Err(err) = spawn_in_background(&args) {
            eprintln!("Failed to spawn in background: {err}");
        } else {
            // successfully spawned in background
            return;
        }
    }
    
    let mut args_rx = match handle_args(&args) {
        Ok(ArgListenerResult::Create(rx)) => rx,
        Ok(ArgListenerResult::Exit) => {
            return;
        },
        Err(err) => {
            eprintln!("{}", err);
            return;
        },
    };

    zlog::init();

    if in_terminal {
        zlog::init_output_stdout();
    } else {
        let result = zlog::init_output_file(paths::log_file(), Some(paths::old_log_file()));
        if let Err(err) = result {
            eprintln!("Could not open log file: {}... Defaulting to stdout", err);
            zlog::init_output_stdout();
        };
    }

    let version = option_env!("ZED_BUILD_ID");
    let app_commit_sha =
        option_env!("ZED_COMMIT_SHA").map(|commit_sha| AppCommitSha::new(commit_sha.to_string()));
    let app_version = AppVersion::load(env!("CARGO_PKG_VERSION"), version, app_commit_sha.clone());

    rayon::ThreadPoolBuilder::new()
        .num_threads(std::thread::available_parallelism().map_or(1, |n| n.get().div_ceil(2)))
        .stack_size(10 * 1024 * 1024)
        .thread_name(|ix| format!("RayonWorker{}", ix))
        .build_global()
        .unwrap();

    log::info!(
        "========== starting zil version {}, sha {} ==========",
        app_version,
        app_commit_sha
            .as_ref()
            .map(|sha| sha.short())
            .as_deref()
            .unwrap_or("unknown"),
    );

    #[cfg(debug_assertions)]
    {
        // verify that IconName <-> Asset mapping is one to one
        for name in IconName::iter() {
            Assets.assert_exists(&name.path());
        }
        for asset_name in Assets.list("icons/").unwrap().iter() {
            let mut found = false;
            for name in IconName::iter() {
                if name.path().as_ref() == asset_name.as_ref() {
                    found = true;
                    break;
                }
            }
            assert!(found, "Unknown icon: {asset_name}");
        }
    }

    #[cfg(windows)]
    check_for_conpty_dll();

    let app = build_application().with_assets(Assets);

    let session_id = Uuid::new_v4().to_string();
    let session = app.background_executor().spawn(Session::new(
        session_id.clone(),
    ));

    let (open_listener, mut open_rx) = OpenListener::new();

    let git_hosting_provider_registry = Arc::new(GitHostingProviderRegistry::new());
    let git_binary_path =
        if cfg!(target_os = "macos") && option_env!("ZED_BUNDLE").as_deref() == Some("true") {
            app.path_for_auxiliary_executable("git")
                .context("could not find git binary path")
                .log_err()
        } else {
            None
        };
    if let Some(git_binary_path) = &git_binary_path {
        log::info!("Using git binary path: {:?}", git_binary_path);
    }

    let fs = Arc::new(RealFs::new(git_binary_path, app.background_executor()));
    let (user_keymap_file_rx, user_keymap_watcher) = watch_config_file(
        &app.background_executor(),
        fs.clone(),
        paths::keymap_file().clone(),
    );

    app.on_open_urls({
        let open_listener = open_listener.clone();
        move |urls| {
            open_listener.open(RawOpenRequest {
                urls,
                ..Default::default()
            })
        }
    });
    app.on_reopen(move |cx| {
        if let Some(app_state) = AppState::try_global(cx) {
            cx.spawn({
                async move |cx| {
                    if let Err(e) = restore_or_create_workspace(app_state, cx).await {
                        fail_to_open_window_async(e, cx)
                    }
                }
            })
            .detach();
        }
    });

    app.run(move |cx| {
        trusted_worktrees::init(HashMap::default(), cx);
        menu::init();
        zed_actions::init();

        release_channel::init(app_version, cx);
        gpui_tokio::init(cx);
        if let Some(app_commit_sha) = app_commit_sha {
            AppCommitSha::set_global(app_commit_sha, cx);
        }
        settings::init(cx);
        zlog_settings::init(cx);
        zed::watch_settings_files(fs.clone(), cx);
        handle_keymap_file_changes(user_keymap_file_rx, user_keymap_watcher, cx);

        let user_agent = format!(
            "Zed/{} ({}; {})",
            AppVersion::global(cx),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        let proxy_url = ProxySettings::get_global(cx).proxy_url();
        let http = {
            let _guard = Tokio::handle(cx).enter();

            ReqwestClient::proxy_and_user_agent(proxy_url, &user_agent)
                .expect("could not start HTTP client")
        };
        cx.set_http_client(Arc::new(http));

        <dyn Fs>::set_global(fs.clone(), cx);

        GitHostingProviderRegistry::set_global(git_hosting_provider_registry, cx);
        git_hosting_providers::init(cx);

        OpenListener::set_global(cx, open_listener.clone());

        let client = Client::production(cx);
        cx.set_http_client(client.http_client());
        let languages = LanguageRegistry::new(fs.clone(), cx.background_executor().clone());
        let languages = Arc::new(languages);
        let language_reload_task = languages.clone().reload_languages_from_config(cx);
        cx.foreground_executor().block_on(language_reload_task);

        languages::init(languages.clone(), fs.clone(), cx);
        let user_store = cx.new(|cx| UserStore::new(client.clone(), cx));
        let workspace_store = cx.new(|cx| WorkspaceStore::new(client.clone(), cx));

        Client::set_global(client.clone(), cx);

        zed::init(cx);
        project::Project::init(&client, cx);
        client::init(&client, cx);

        let session = cx.foreground_executor().block_on(session);

        let telemetry = client.telemetry();
        telemetry.start(
            Some("asdf".to_string()),
            Some("asdf".to_string()),
            session.id().to_owned(),
            cx,
        );

        let app_session = cx.new(|cx| AppSession::new(session, cx));

        let app_state = Arc::new(AppState {
            languages,
            client: client.clone(),
            user_store,
            fs: fs.clone(),
            build_window_options,
            workspace_store,
            session: app_session,
        });
        AppState::set_global(app_state.clone(), cx);

        theme_settings::init(theme::LoadThemes::All(Box::new(Assets)), cx);
        command_palette::init(cx);

        language_model::init(cx);
        RefreshLlmTokenListener::register(
            app_state.client.clone(),
            app_state.user_store.clone(),
            cx,
        );
        zed::remote_debug::init(cx);

        load_embedded_fonts(cx);

        editor::init(cx);
        image_viewer::init(cx);

        workspace::init(app_state.clone(), cx);
        ui_prompt::init(cx);

        go_to_line::init(cx);
        cx.observe_new(open_path_prompt::OpenPathPrompt::register).detach();
        cx.observe_new(open_path_prompt::OpenPathPrompt::register_new_path).detach();
        tab_switcher::init(cx);
        search::init(cx);
        cx.set_global(workspace::PaneSearchBarCallbacks {
            setup_search_bar: |languages, toolbar, window, cx| {
                let search_bar = cx.new(|cx| search::BufferSearchBar::new(languages, window, cx));
                toolbar.update(cx, |toolbar, cx| {
                    toolbar.add_item(search_bar, window, cx);
                });
            },
            wrap_div_with_search_actions: search::buffer_search::register_pane_search_actions,
        });
        encoding_selector::init(cx);
        language_selector::init(cx);
        line_ending_selector::init(cx);
        settings_profile_selector::init(cx);
        language_tools::init(cx);
        notifications::init(cx);
        title_bar::init(cx);
        settings_ui::init(cx);
        keymap_editor::init(cx);
        inspector_ui::init(app_state.clone(), cx);
        which_key::init(cx);

        cx.observe_global::<SettingsStore>({
            let http = app_state.client.http_client();
            let client = app_state.client.clone();
            move |cx| {
                for &mut window in cx.windows().iter_mut() {
                    let background_appearance = cx.theme().window_background_appearance();
                    window
                        .update(cx, |_, window, _| {
                            window.set_background_appearance(background_appearance)
                        })
                        .ok();
                }

                cx.set_text_rendering_mode(
                    match WorkspaceSettings::get_global(cx).text_rendering_mode {
                        settings::TextRenderingMode::PlatformDefault => {
                            gpui::TextRenderingMode::PlatformDefault
                        }
                        settings::TextRenderingMode::Subpixel => gpui::TextRenderingMode::Subpixel,
                        settings::TextRenderingMode::Grayscale => {
                            gpui::TextRenderingMode::Grayscale
                        }
                    },
                );

                let new_host = &client::ClientSettings::get_global(cx).server_url;
                if &http.base_url() != new_host {
                    http.set_base_url(new_host);
                    if client.status().borrow().is_connected() {
                        client.reconnect(&cx.to_async());
                    }
                }
            }
        })
        .detach();
        app_state.languages.set_theme(cx.theme().clone());
        cx.observe_global::<GlobalTheme>({
            let languages = app_state.languages.clone();
            move |cx| {
                languages.set_theme(cx.theme().clone());
            }
        })
        .detach();

        let fs = app_state.fs.clone();
        load_user_themes_in_background(fs.clone(), cx);
        watch_themes(fs.clone(), cx);

        let menus = app_menus(cx);
        cx.set_menus(menus);

        initialize_workspace(app_state.clone(), cx);

        cx.activate(true);

        cx.spawn({
            let client = app_state.client.clone();
            async move |cx| authenticate(client, cx).await
        })
        .detach_and_log_err(cx);

        let app_state = app_state.clone();

        cx.spawn(async move |cx| {
            while let Some(urls) = open_rx.next().await {
                cx.update(|cx| {
                    if let Some(request) = OpenRequest::parse(urls, cx).log_err() {
                        handle_open_request(request, app_state.clone(), cx);
                    }
                });
            }
        })
        .detach();

        cx.on_app_quit(|cx| {
            cx.background_executor().spawn(async move {
                handle_args_exit();
            })
        })
        .detach();

        cx.spawn(async move |_cx| {
            while let Some(args) = args_rx.next().await {
                let urls: Vec<_> = args
                    .iter()
                    .map(|arg| parse_url_arg(arg))
                    .collect();

                if urls.is_empty() {
                    open_listener.open(RawOpenRequest {
                        urls: vec!["zed://open".to_string()],
                        ..Default::default()
                    })
                } else {
                    open_listener.open(RawOpenRequest {
                        urls,
                        ..Default::default()
                    })
                }
            }
        })
        .detach();
    });
}

fn handle_open_request(request: OpenRequest, app_state: Arc<AppState>, cx: &mut App) {
    if let Some(kind) = request.kind {
        match kind {
            OpenRequestKind::CliConnection(connection) => {
                cx.spawn(async move |cx| handle_cli_connection(connection, app_state, cx).await)
                    .detach();
            }
            OpenRequestKind::FocusApp => {
                cx.spawn(async move |cx| {
                    if workspace::activate_any_workspace_window(cx).is_some() {
                        return anyhow::Ok(());
                    }
                    restore_or_create_workspace(app_state, cx).await
                })
                .detach_and_log_err(cx);
            }
            OpenRequestKind::DockMenuAction { index } => {
                cx.perform_dock_menu_action(index);
            }
            OpenRequestKind::Setting { setting_path } => {
                // zed://settings/languages/$(language)/tab_size  - DONT SUPPORT
                // zed://settings/languages/Rust/tab_size  - SUPPORT
                // languages.$(language).tab_size
                // [ languages $(language) tab_size]
                cx.spawn(async move |cx| {
                    let workspace =
                        workspace::get_any_active_multi_workspace(app_state, cx.clone()).await?;

                    workspace.update(cx, |_, window, cx| match setting_path {
                        None => window.dispatch_action(Box::new(zed_actions::OpenSettings), cx),
                        Some(setting_path) => window.dispatch_action(
                            Box::new(zed_actions::OpenSettingsAt { path: setting_path }),
                            cx,
                        ),
                    })
                })
                .detach_and_log_err(cx);
            }
        }

        return;
    }

    let mut task = None;
    if !request.open_paths.is_empty() {
        let app_state = app_state.clone();
        let base_open_options = zed::open_options_for_request(
            request.open_behavior,
            cx,
        );
        task = Some(cx.spawn(async move |cx| {
            let paths_with_position =
                derive_paths_with_position(app_state.fs.as_ref(), request.open_paths).await;
            let (_window, results) = open_paths_with_positions(
                &paths_with_position,
                app_state,
                workspace::OpenOptions {
                    ..base_open_options
                },
                cx,
            )
            .await?;
            for result in results.into_iter().flatten() {
                if let Err(err) = result {
                    log::error!("Error opening path: {err:#}");
                }
            }
            anyhow::Ok(())
        }));
    }

    if let Some(task) = task {
        cx.spawn(async move |cx| {
            if let Err(err) = task.await {
                fail_to_open_window_async(err, cx);
            }
        })
        .detach();
    }
}

async fn authenticate(client: Arc<Client>, cx: &AsyncApp) -> Result<()> {
    if stdout_is_a_pty() {
        if client::IMPERSONATE_LOGIN.is_some() {
            client.sign_in_with_optional_connect(false, cx).await?;
        } else if client.has_credentials(cx).await {
            client.sign_in_with_optional_connect(true, cx).await?;
        }
    } else if client.has_credentials(cx).await {
        client.sign_in_with_optional_connect(true, cx).await?;
    }

    Ok(())
}

pub(crate) async fn restore_or_create_workspace(
    app_state: Arc<AppState>,
    cx: &mut AsyncApp,
) -> Result<()> {
    cx.update(|cx| {
        workspace::open_new(
            Default::default(),
            app_state,
            cx,
            |workspace, window, cx| {
                Editor::new_file(workspace, &Default::default(), window, cx);
            },
        )
    })
    .await?;

    Ok(())
}

fn copy_asset_dir(prefix: &str, target_dir: &Path) -> io::Result<()> {
    let Ok(asset_paths) = Assets.list(prefix) else {
        return Ok(());
    };
    for asset_path in asset_paths {
        let asset_path = asset_path.as_str();

        let relative_path = asset_path
            .strip_prefix(prefix)
            .unwrap()
            .trim_start_matches('/');

        let target_path = target_dir.join(relative_path);

        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let Ok(Some(asset)) = Assets.load(asset_path) else {
            continue;
        };

        if !target_path.exists() {
            std::fs::write(&target_path, asset)?;
        }
    }

    Ok(())
}

fn init_paths() -> HashMap<io::ErrorKind, Vec<&'static Path>> {
    [
        paths::config_dir(),
        paths::languages_dir(),
        paths::grammars_dir(),
        paths::debug_adapters_dir(),
        paths::logs_dir(),
        paths::temp_dir(),
        paths::hang_traces_dir(),
    ]
    .into_iter()
    .fold(HashMap::default(), |mut errors, path| {
        if !path.exists() {
            let result = std::fs::create_dir_all(path).and_then(|_| {
                if path == paths::languages_dir() {
                    copy_asset_dir("languages", path)
                } else if path == paths::grammars_dir() {
                    copy_asset_dir("grammars", path)
                } else {
                    Ok(())
                }
            });

            if let Err(e) = result {
                errors.entry(e.kind()).or_insert_with(Vec::new).push(path);
            }
        }

        errors
    })
}

pub(crate) static FORCE_CLI_MODE: LazyLock<bool> = LazyLock::new(|| {
    let env_var = std::env::var(FORCE_CLI_MODE_ENV_VAR_NAME).ok().is_some();
    unsafe { std::env::remove_var(FORCE_CLI_MODE_ENV_VAR_NAME) };
    env_var
});

fn stdout_is_a_pty() -> bool {
    !*FORCE_CLI_MODE && io::stdout().is_terminal()
}

fn parse_url_arg(arg: &str) -> String {
    match std::fs::canonicalize(Path::new(&arg)) {
        Ok(path) => format!("file://{}", path.display()),
        Err(_) => {
            if arg.starts_with("file://")
                || arg.starts_with("zed://")
                || arg.starts_with("zed-cli://")
                || arg.starts_with("ssh://")
            {
                arg.into()
            } else {
                format!("file://{arg}")
            }
        }
    }
}

fn load_embedded_fonts(cx: &App) {
    let asset_source = cx.asset_source();
    let font_paths = asset_source.list("fonts").unwrap();
    let embedded_fonts = Mutex::new(Vec::new());
    let executor = cx.background_executor();

    cx.foreground_executor().block_on(executor.scoped(|scope| {
        for font_path in &font_paths {
            if !font_path.ends_with(".ttf") {
                continue;
            }

            scope.spawn(async {
                let font_bytes = asset_source.load(font_path).unwrap().unwrap();
                embedded_fonts.lock().push(font_bytes);
            });
        }
    }));

    cx.text_system()
        .add_fonts(embedded_fonts.into_inner())
        .unwrap();
}

/// Spawns a background task to load the user themes from the themes directory.
fn load_user_themes_in_background(fs: Arc<dyn fs::Fs>, cx: &mut App) {
    cx.spawn({
        let fs = fs.clone();
        async move |cx| {
            let theme_registry = cx.update(|cx| ThemeRegistry::global(cx));
            let themes_dir = paths::themes_dir().as_ref();
            match fs
                .metadata(themes_dir)
                .await
                .ok()
                .flatten()
                .map(|m| m.is_dir)
            {
                Some(is_dir) => {
                    anyhow::ensure!(is_dir, "Themes dir path {themes_dir:?} is not a directory")
                }
                None => {
                    fs.create_dir(themes_dir).await.with_context(|| {
                        format!("Failed to create themes dir at path {themes_dir:?}")
                    })?;
                }
            }

            let mut theme_paths = fs
                .read_dir(themes_dir)
                .await
                .with_context(|| format!("reading themes from {themes_dir:?}"))?;

            while let Some(theme_path) = theme_paths.next().await {
                let Some(theme_path) = theme_path.log_err() else {
                    continue;
                };
                let Some(bytes) = fs.load_bytes(&theme_path).await.log_err() else {
                    continue;
                };

                load_user_theme(&theme_registry, &bytes).log_err();
            }

            cx.update(theme_settings::reload_theme);
            anyhow::Ok(())
        }
    })
    .detach_and_log_err(cx);
}

/// Spawns a background task to watch the themes directory for changes.
fn watch_themes(fs: Arc<dyn fs::Fs>, cx: &mut App) {
    use std::time::Duration;
    cx.spawn(async move |cx| {
        let (mut events, _) = fs
            .watch(paths::themes_dir(), Duration::from_millis(100))
            .await;

        while let Some(paths) = events.next().await {
            for event in paths {
                if fs.metadata(&event.path).await.ok().flatten().is_some_and(|m| !m.is_dir) {
                    let theme_registry = cx.update(|cx| ThemeRegistry::global(cx));
                    if let Some(bytes) = fs.load_bytes(&event.path).await.log_err()
                        && load_user_theme(&theme_registry, &bytes).log_err().is_some()
                    {
                        cx.update(theme_settings::reload_theme);
                    }
                }
            }
        }
    })
    .detach()
}

fn _dump_all_gpui_actions() {
    #[derive(Debug, serde::Serialize)]
    struct ActionDef {
        name: &'static str,
        human_name: String,
        schema: Option<serde_json::Value>,
        deprecated_aliases: &'static [&'static str],
        deprecation_message: Option<&'static str>,
        documentation: Option<&'static str>,
    }
    let mut generator = settings::KeymapFile::action_schema_generator();
    let mut actions = gpui::generate_list_of_all_registered_actions()
        .map(|action| {
            let schema = (action.json_schema)(&mut generator)
                .map(|s| serde_json::to_value(s).expect("Failed to serialize action schema"));
            ActionDef {
                name: action.name,
                human_name: command_palette::humanize_action_name(action.name),
                schema,
                deprecated_aliases: action.deprecated_aliases,
                deprecation_message: action.deprecation_message,
                documentation: action.documentation,
            }
        })
        .collect::<Vec<ActionDef>>();

    actions.sort_by_key(|a| a.name);

    let schema_definitions = serde_json::to_value(generator.definitions())
        .expect("Failed to serialize schema definitions");

    let output = serde_json::json!({
        "actions": actions,
        "schema_definitions": schema_definitions,
    });

    io::Write::write(
        &mut std::io::stdout(),
        serde_json::to_string_pretty(&output).unwrap().as_bytes(),
    )
    .unwrap();
}

#[cfg(target_os = "windows")]
fn check_for_conpty_dll() {
    use windows::{
        Win32::{Foundation::FreeLibrary, System::LibraryLoader::LoadLibraryW},
        core::w,
    };

    if let Ok(hmodule) = unsafe { LoadLibraryW(w!("conpty.dll")) } {
        unsafe {
            FreeLibrary(hmodule)
                .context("Failed to free conpty.dll")
                .log_err();
        }
    } else {
        log::warn!("Failed to load conpty.dll. Terminal will work with reduced functionality.");
    }
}
