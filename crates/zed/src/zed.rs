mod app_menus;
#[cfg(target_os = "macos")]
pub(crate) mod mac_only_instance;
mod migrate;
#[cfg(target_os = "macos")]
pub(crate) mod move_to_applications;
mod open_listener;
mod open_url_modal;
mod quick_action_bar;
pub mod remote_debug;
#[cfg(all(target_os = "macos", feature = "visual-tests"))]
pub mod visual_tests;
#[cfg(target_os = "windows")]
pub(crate) mod windows_only_instance;

use anyhow::Context as _;
pub use app_menus::*;
use assets::Assets;

use breadcrumbs::Breadcrumbs;
use client::zed_urls;
use editor::{Editor, MultiBuffer};
use extension_host::ExtensionStore;
use feature_flags::{FeatureFlagAppExt as _, PanicFeatureFlag};
use fs::Fs;
use futures::{StreamExt, channel::mpsc, select_biased};
use gpui::{
    Action, App, AppContext as _, ClipboardItem, Context, DismissEvent, Element, Entity,
    FocusHandle, Focusable, Image, ImageFormat, KeyBinding, ParentElement, PathPromptOptions,
    PromptLevel, ReadGlobal, SharedString, Size, Task, TaskExt, TitlebarOptions, UpdateGlobal,
    WeakEntity, Window, WindowBounds, WindowHandle, WindowKind, WindowOptions, actions,
    image_cache, img, point, px, retain_all,
};
use image_viewer::ImageInfo;
use language::Capability;
use language_onboarding::BasedPyrightBanner;
use language_tools::lsp_button::{self, LspButton};
use language_tools::lsp_log_view::LspLogToolbarItemView;
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use migrate::{MigrationBanner, MigrationEvent, MigrationNotification, MigrationType};
use migrator::migrate_keymap;
pub use open_listener::*;
use paths::{
    local_settings_file_relative_path,
    local_tasks_file_relative_path,
};
use project::{ProjectItem};
use quick_action_bar::QuickActionBar;
use release_channel::{AppCommitSha, AppVersion, ReleaseChannel};
use rope::Rope;
use settings::{
    BaseKeymap, DEFAULT_KEYMAP_PATH, InvalidSettingsError, KeybindSource, KeymapFile,
    KeymapFileLoadResult, MigrationStatus, Settings, SettingsFile, SettingsStore,
    initial_project_settings_content, initial_tasks_content,
    update_settings_file,
};

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{self, AtomicBool},
};
use theme::{ActiveTheme, SystemAppearance, ThemeRegistry, deserialize_icon_theme};
use theme_settings::{ThemeSettings, load_user_theme};
use ui::{Navigable, NavigableEntry, PopoverMenuHandle, TintColor, prelude::*};
use util::markdown::MarkdownString;
use util::rel_path::RelPath;
use util::{ResultExt, asset_str};
use uuid::Uuid;
use workspace::notifications::{NotificationId, dismiss_app_notification, show_app_notification};

use workspace::{
    AppState, MultiWorkspace, NewFile, NewWindow, Workspace, WorkspaceSettings,
    create_and_open_local_file, notifications::simple_message_notification::MessageNotification,
    open_new,
};
use workspace::{
    CloseIntent, CloseProject, CloseWindow, RestoreBanner, with_active_or_new_workspace,
};
use workspace::{Pane};
use zed_actions::{
    About, OpenAccountSettings, OpenBrowser, OpenDocs, OpenServerSettings, OpenSettingsFile,
    OpenZedUrl, Quit,
};

const DOCS_URL: &str = "https://zed.dev/docs/";

pub struct CrashHandler(pub Arc<crashes::Client>);

impl gpui::Global for CrashHandler {}

actions!(
    zed,
    [
        /// Opens the element inspector for debugging UI.
        DebugElements,
        /// Hides the application window.
        Hide,
        /// Hides all other application windows.
        HideOthers,
        /// Minimizes the current window.
        Minimize,
        /// Opens the default settings file.
        OpenDefaultSettings,
        /// Opens project-specific settings file.
        OpenProjectSettingsFile,
        /// Opens the project tasks configuration.
        OpenProjectTasks,
        /// Opens the tasks panel.
        OpenTasks,
        /// Opens debug tasks configuration.
        OpenDebugTasks,
        /// Shows the default semantic token rules (read-only).
        ShowDefaultSemanticTokenRules,
        /// Resets the application database.
        ResetDatabase,
        /// Shows all hidden windows.
        ShowAll,
        /// Toggles fullscreen mode.
        ToggleFullScreen,
        /// Zooms the window.
        Zoom,
        /// Triggers a test panic for debugging.
        TestPanic,
        /// Triggers a hard crash for debugging.
        TestCrash,
    ]
);

actions!(
    dev,
    [
        /// Opens a prompt to enter a URL to open.
        OpenUrlPrompt,
    ]
);

pub fn init(cx: &mut App) {
    #[cfg(target_os = "macos")]
    cx.on_action(|_: &Hide, cx| cx.hide());
    #[cfg(target_os = "macos")]
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    #[cfg(target_os = "macos")]
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(quit);

    cx.on_action(|_: &RestoreBanner, cx| title_bar::restore_banner(cx));

    cx.observe_flag::<PanicFeatureFlag, _>({
        let mut added = false;
        move |flag, cx| {
            if added || !*flag {
                return;
            }
            added = true;
            cx.on_action(|_: &TestPanic, _| panic!("Ran the TestPanic action"))
                .on_action(|_: &TestCrash, _| {
                    unsafe extern "C" {
                        fn puts(s: *const i8);
                    }
                    unsafe {
                        puts(0xabad1d3a as *const i8);
                    }
                });
        }
    })
    .detach();
    cx.on_action(|&zed_actions::OpenKeymapFile, cx| {
        with_active_or_new_workspace(cx, |_, window, cx| {
            open_settings_file(
                paths::keymap_file(),
                || settings::initial_keymap_content().as_ref().into(),
                window,
                cx,
            );
        });
    })
    .on_action(|_: &OpenSettingsFile, cx| {
        with_active_or_new_workspace(cx, |_, window, cx| {
            open_settings_file(
                paths::settings_file(),
                || settings::initial_user_settings_content().as_ref().into(),
                window,
                cx,
            );
        });
    })
    .on_action(|_: &OpenAccountSettings, cx| {
        with_active_or_new_workspace(cx, |_, _, cx| {
            cx.open_url(&zed_urls::account_url(cx));
        });
    })
    .on_action(|_: &OpenTasks, cx| {
        with_active_or_new_workspace(cx, |_, window, cx| {
            open_settings_file(
                paths::tasks_file(),
                || settings::initial_tasks_content().as_ref().into(),
                window,
                cx,
            );
        });
    })
    .on_action(|_: &OpenDebugTasks, cx| {
        with_active_or_new_workspace(cx, |_, window, cx| {
            open_settings_file(
                paths::debug_scenarios_file(),
                || settings::initial_debug_tasks_content().as_ref().into(),
                window,
                cx,
            );
        });
    })
    .on_action(|_: &ShowDefaultSemanticTokenRules, cx| {
        with_active_or_new_workspace(cx, |workspace, window, cx| {
            open_bundled_file(
                workspace,
                settings::default_semantic_token_rules(),
                "Default Semantic Token Rules",
                "JSONC",
                window,
                cx,
            );
        });
    })
    .on_action(|_: &OpenDefaultSettings, cx| {
        with_active_or_new_workspace(cx, |workspace, window, cx| {
            open_bundled_file(
                workspace,
                settings::default_settings(),
                "Default Settings",
                "JSON",
                window,
                cx,
            );
        });
    })
    .on_action(|_: &zed_actions::OpenDefaultKeymap, cx| {
        with_active_or_new_workspace(cx, |workspace, window, cx| {
            open_bundled_file(
                workspace,
                settings::default_keymap(),
                "Default Key Bindings",
                "JSON",
                window,
                cx,
            );
        });
    })
    .on_action(|_: &About, cx| {
        open_about_window(cx);
    });
}

fn bind_on_window_closed(cx: &mut App) -> Option<gpui::Subscription> {
    #[cfg(target_os = "macos")]
    {
        WorkspaceSettings::get_global(cx)
            .on_last_window_closed
            .is_quit_app()
            .then(|| {
                cx.on_window_closed(|cx, _window_id| {
                    if cx.windows().is_empty() {
                        cx.quit();
                    }
                })
            })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        }))
    }
}

pub fn build_window_options(display_uuid: Option<Uuid>, cx: &mut App) -> WindowOptions {
    let display = display_uuid.and_then(|uuid| {
        cx.displays()
            .into_iter()
            .find(|display| display.uuid().ok() == Some(uuid))
    });
    let app_id = ReleaseChannel::global(cx).app_id();
    let window_decorations = match std::env::var("ZED_WINDOW_DECORATIONS") {
        Ok(val) if val == "server" => gpui::WindowDecorations::Server,
        Ok(val) if val == "client" => gpui::WindowDecorations::Client,
        _ => match WorkspaceSettings::get_global(cx).window_decorations {
            settings::WindowDecorations::Server => gpui::WindowDecorations::Server,
            settings::WindowDecorations::Client => gpui::WindowDecorations::Client,
        },
    };

    let use_system_window_tabs = WorkspaceSettings::get_global(cx).use_system_window_tabs;

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    static APP_ICON: std::sync::LazyLock<Option<std::sync::Arc<image::RgbaImage>>> =
        std::sync::LazyLock::new(|| {
            // this shouldn't fail since decode is checked in build.rs
            const BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/app_icon.png"));
            util::maybe!({
                let image = image::ImageReader::new(std::io::Cursor::new(BYTES))
                    .with_guessed_format()?
                    .decode()?
                    .into();
                anyhow::Ok(Arc::new(image))
            })
            .log_err()
        });

    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(point(px(9.0), px(9.0))),
        }),
        window_bounds: None,
        focus: false,
        show: false,
        kind: WindowKind::Normal,
        is_movable: true,
        display_id: display.map(|display| display.id()),
        window_background: cx.theme().window_background_appearance(),
        app_id: Some(app_id.to_owned()),
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        icon: APP_ICON.as_ref().cloned(),
        window_decorations: Some(window_decorations),
        window_min_size: Some(gpui::Size {
            width: px(360.0),
            height: px(240.0),
        }),
        tabbing_identifier: if use_system_window_tabs {
            Some(String::from("zed"))
        } else {
            None
        },
        ..Default::default()
    }
}

pub fn initialize_workspace(app_state: Arc<AppState>, cx: &mut App) {
    let mut _on_close_subscription = bind_on_window_closed(cx);
    cx.observe_global::<SettingsStore>(move |cx| {
        // A 1.92 regression causes unused-assignment to trigger on this variable.
        _ = _on_close_subscription.is_some();
        _on_close_subscription = bind_on_window_closed(cx);
    })
    .detach();

    init_cursor_hide_mode(cx);

    cx.observe_new(|_multi_workspace: &mut MultiWorkspace, window, cx| {
        let Some(window) = window else {
            return;
        };

        #[cfg(feature = "track-project-leak")]
        {
            let multi_workspace_handle = cx.weak_entity();
            let workspace_handle = _multi_workspace.workspace().downgrade();
            let project_handle = _multi_workspace.workspace().read(cx).project().downgrade();
            let window_id_2 = window.window_handle().window_id();
            cx.on_window_closed(move |cx, window_id| {
                let multi_workspace_handle = multi_workspace_handle.clone();
                let workspace_handle = workspace_handle.clone();
                let project_handle = project_handle.clone();
                if window_id != window_id_2 {
                    return;
                }
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(1500))
                        .await;

                    multi_workspace_handle.assert_released();
                    workspace_handle.assert_released();
                    project_handle.assert_released();
                })
                .detach();
            })
            .detach();
        }

        let multi_workspace_handle = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            multi_workspace_handle
                .update(cx, |multi_workspace, cx| {
                    // We'll handle closing asynchronously
                    multi_workspace.close_window(&CloseWindow, window, cx);
                    false
                })
                .unwrap_or(true)
        });

        let multi_workspace_handle = cx.entity();
        cx.subscribe_in(
            &multi_workspace_handle,
            window,
            |this, _multi_workspace, event: &workspace::MultiWorkspaceEvent, window, cx| {
                let workspace::MultiWorkspaceEvent::ActiveWorkspaceChanged { source_workspace } =
                    event
                else {
                    return;
                };

                let active_workspace = this.workspace().clone();
                let source_workspace = source_workspace.clone();
                active_workspace.update(cx, |workspace, cx| {
                    ensure_agent_panel_for_workspace(workspace, source_workspace, window, cx)
                        .detach_and_log_err(cx);
                });
            },
        )
        .detach();
    })
    .detach();

    cx.observe_new(move |workspace: &mut Workspace, window, cx| {
        let Some(window) = window else {
            return;
        };

        let workspace_handle = cx.entity();
        let center_pane = workspace.active_pane().clone();
        initialize_pane(workspace, &center_pane, window, cx);

        cx.subscribe_in(&workspace_handle, window, {
            move |workspace, _, event, window, cx| match event {
                workspace::Event::PaneAdded(pane) => {
                    initialize_pane(workspace, pane, window, cx);
                }
                workspace::Event::OpenBundledFile {
                    text,
                    title,
                    language,
                } => open_bundled_file(workspace, text.clone(), title, language, window, cx),
                _ => {}
            }
        })
        .detach();

        #[cfg(not(any(test, target_os = "macos")))]
        initialize_file_watcher(window, cx);

        if let Some(specs) = window.gpu_specs() {
            log::info!("Using GPU: {:?}", specs);
            show_software_emulation_warning_if_needed(specs.clone(), window, cx);
            if let Some(crash_client) = cx.try_global::<CrashHandler>() {
                crashes::set_gpu_info(&crash_client.0, specs);
            }
        }

        let active_file_name = cx.new(|_| workspace::active_file_name::ActiveFileName::new());
        let active_buffer_encoding =
            cx.new(|_| encoding_selector::ActiveBufferEncoding::new(workspace));
        let active_buffer_language =
            cx.new(|_| language_selector::ActiveBufferLanguage::new(workspace));
        let active_toolchain_language =
            cx.new(|cx| toolchain_selector::ActiveToolchain::new(workspace, window, cx));
        let image_info = cx.new(|_cx| ImageInfo::new(workspace));

        let lsp_button_menu_handle = PopoverMenuHandle::default();
        let lsp_button =
            cx.new(|cx| LspButton::new(workspace, lsp_button_menu_handle.clone(), window, cx));
        workspace.register_action({
            move |_, _: &lsp_button::ToggleMenu, window, cx| {
                lsp_button_menu_handle.toggle(window, cx);
            }
        });

        let cursor_position =
            cx.new(|_| go_to_line::cursor_position::CursorPosition::new(workspace));
        let line_ending_indicator =
            cx.new(|_| line_ending_selector::LineEndingIndicator::default());
        workspace.status_bar().update(cx, |status_bar, cx| {
            status_bar.add_left_item(lsp_button, window, cx);
            status_bar.add_left_item(active_file_name, window, cx);
            status_bar.add_right_item(active_buffer_encoding, window, cx);
            status_bar.add_right_item(active_buffer_language, window, cx);
            status_bar.add_right_item(active_toolchain_language, window, cx);
            status_bar.add_right_item(line_ending_indicator, window, cx);
            status_bar.add_right_item(cursor_position, window, cx);
            status_bar.add_right_item(image_info, window, cx);
        });

        let panels_task = Task::ready(anyhow::Ok(())); // VELIPSO: remove
        workspace.set_panels_task(panels_task);
        register_actions(app_state.clone(), workspace, window, cx);

        if !workspace.has_active_modal(window, cx) {
            workspace.focus_handle(cx).focus(window, cx);
        }
    })
    .detach();
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[allow(unused)]
fn initialize_file_watcher(window: &mut Window, cx: &mut Context<Workspace>) {
    if let Err(e) = fs::fs_watcher::global(|_| {}) {
        let message = format!(
            db::indoc! {r#"
            inotify_init returned {}

            This may be due to system-wide limits on inotify instances. For troubleshooting see: https://zed.dev/docs/linux
            "#},
            e
        );
        let prompt = window.prompt(
            PromptLevel::Critical,
            "Could not start inotify",
            Some(&message),
            &["Troubleshoot and Quit"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if prompt.await == Ok(0) {
                cx.update(|cx| {
                    cx.open_url("https://zed.dev/docs/linux#could-not-start-inotify");
                    cx.quit();
                });
            }
        })
        .detach()
    }
}

#[cfg(target_os = "windows")]
#[allow(unused)]
fn initialize_file_watcher(window: &mut Window, cx: &mut Context<Workspace>) {
    if let Err(e) = fs::fs_watcher::global(|_| {}) {
        let message = format!(
            db::indoc! {r#"
            ReadDirectoryChangesW initialization failed: {}

            This may occur on network filesystems and WSL paths. For troubleshooting see: https://zed.dev/docs/windows
            "#},
            e
        );
        let prompt = window.prompt(
            PromptLevel::Critical,
            "Could not start ReadDirectoryChangesW",
            Some(&message),
            &["Troubleshoot and Quit"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if prompt.await == Ok(0) {
                cx.update(|cx| {
                    cx.open_url("https://zed.dev/docs/windows");
                    cx.quit()
                });
            }
        })
        .detach()
    }
}

fn show_software_emulation_warning_if_needed(
    specs: gpui::GpuSpecs,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if specs.is_software_emulated && std::env::var("ZED_ALLOW_EMULATED_GPU").is_err() {
        let (graphics_api, docs_url, open_url) = if cfg!(target_os = "windows") {
            (
                "DirectX",
                "https://zed.dev/docs/windows",
                "https://zed.dev/docs/windows",
            )
        } else {
            (
                "Vulkan",
                "https://zed.dev/docs/linux",
                "https://zed.dev/docs/linux#zed-fails-to-open-windows",
            )
        };
        let message = format!(
            db::indoc! {r#"
            Zed uses {} for rendering and requires a compatible GPU.

            Currently you are using a software emulated GPU ({}) which
            will result in awful performance.

            For troubleshooting see: {}
            Set ZED_ALLOW_EMULATED_GPU=1 env var to permanently override.
            "#},
            graphics_api, specs.device_name, docs_url
        );
        let prompt = window.prompt(
            PromptLevel::Critical,
            "Unsupported GPU",
            Some(&message),
            &["Skip", "Troubleshoot and Quit"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if prompt.await == Ok(1) {
                cx.update(|cx| {
                    cx.open_url(open_url);
                    cx.quit();
                });
            }
        })
        .detach()
    }
}

fn ensure_agent_panel_for_workspace(
    _workspace: &mut Workspace,
    _source_workspace: Option<WeakEntity<Workspace>>,
    _window: &mut Window,
    _cx: &mut Context<Workspace>,
) -> Task<anyhow::Result<()>> {
    // VELIPSO: delete function
    Task::ready(Ok(()))
}

fn register_actions(
    app_state: Arc<AppState>,
    workspace: &mut Workspace,
    _: &mut Window,
    cx: &mut Context<Workspace>,
) {
    workspace
        .register_action(|_, _: &OpenDocs, _, cx| cx.open_url(DOCS_URL))
        .register_action(|_, _: &Minimize, window, _| {
            window.minimize_window();
        })
        .register_action(|_, _: &Zoom, window, _| {
            window.zoom_window();
        })
        .register_action(|_, _: &ToggleFullScreen, window, _| {
            window.toggle_fullscreen();
        })
        .register_action(|_, action: &OpenZedUrl, _, cx| {
            OpenListener::global(cx).open(RawOpenRequest {
                urls: vec![action.url.clone()],
                ..Default::default()
            })
        })
        .register_action(|workspace, _: &OpenUrlPrompt, window, cx| {
            workspace.toggle_modal(window, cx, |window, cx| {
                open_url_modal::OpenUrlModal::new(window, cx)
            });
        })
        .register_action(|workspace, action: &OpenBrowser, _window, cx| {
            // Parse and validate the URL to ensure it's properly formatted
            match url::Url::parse(&action.url) {
                Ok(parsed_url) => {
                    // Use the parsed URL's string representation which is properly escaped
                    cx.open_url(parsed_url.as_str());
                }
                Err(e) => {
                    workspace.show_error(
                        &anyhow::anyhow!(
                            "Opening this URL in a browser failed because the URL is invalid: {}\n\nError was: {e}",
                            action.url
                        ),
                        cx,
                    );
                }
            }
        })
        .register_action(|workspace, action: &workspace::Open, window, cx| {
            workspace::prompt_for_open_path_and_open(
                workspace,
                workspace.app_state().clone(),
                PathPromptOptions {
                    files: true,
                    directories: true,
                    multiple: true,
                    prompt: None,
                },
                action.create_new_window,
                window,
                cx,
            );
        })
        .register_action(|workspace, _: &workspace::OpenFiles, window, cx| {
            let directories = cx.can_select_mixed_files_and_dirs();
            workspace::prompt_for_open_path_and_open(
                workspace,
                workspace.app_state().clone(),
                PathPromptOptions {
                    files: true,
                    directories,
                    multiple: true,
                    prompt: None,
                },
                true,
                window,
                cx,
            );
        })
        .register_action({
            let fs = app_state.fs.clone();
            move |_, action: &zed_actions::IncreaseUiFontSize, _window, cx| {
                if action.persist {
                    update_settings_file(fs.clone(), cx, move |settings, cx| {
                        let ui_font_size = ThemeSettings::get_global(cx).ui_font_size(cx) + px(1.0);
                        let _ = settings
                            .theme
                            .ui_font_size
                            .insert(f32::from(theme_settings::clamp_font_size(ui_font_size)).into());
                    });
                } else {
                    theme_settings::adjust_ui_font_size(cx, |size| size + px(1.0));
                }
            }
        })
        .register_action({
            let fs = app_state.fs.clone();
            move |_, action: &zed_actions::DecreaseUiFontSize, _window, cx| {
                if action.persist {
                    update_settings_file(fs.clone(), cx, move |settings, cx| {
                        let ui_font_size = ThemeSettings::get_global(cx).ui_font_size(cx) - px(1.0);
                        let _ = settings
                            .theme
                            .ui_font_size
                            .insert(f32::from(theme_settings::clamp_font_size(ui_font_size)).into());
                    });
                } else {
                    theme_settings::adjust_ui_font_size(cx, |size| size - px(1.0));
                }
            }
        })
        .register_action({
            let fs = app_state.fs.clone();
            move |_, action: &zed_actions::ResetUiFontSize, _window, cx| {
                if action.persist {
                    update_settings_file(fs.clone(), cx, move |settings, _| {
                        settings.theme.ui_font_size = None;
                    });
                } else {
                    theme_settings::reset_ui_font_size(cx);
                }
            }
        })
        .register_action({
            let fs = app_state.fs.clone();
            move |_, action: &zed_actions::IncreaseBufferFontSize, _window, cx| {
                if action.persist {
                    update_settings_file(fs.clone(), cx, move |settings, cx| {
                        let buffer_font_size =
                            ThemeSettings::get_global(cx).buffer_font_size(cx) + px(1.0);
                        let _ = settings
                            .theme
                            .buffer_font_size
                            .insert(f32::from(theme_settings::clamp_font_size(buffer_font_size)).into());
                    });
                } else {
                    theme_settings::increase_buffer_font_size(cx);
                }
            }
        })
        .register_action({
            let fs = app_state.fs.clone();
            move |_, action: &zed_actions::DecreaseBufferFontSize, _window, cx| {
                if action.persist {
                    update_settings_file(fs.clone(), cx, move |settings, cx| {
                        let buffer_font_size =
                            ThemeSettings::get_global(cx).buffer_font_size(cx) - px(1.0);
                        let _ = settings
                            .theme
                            .buffer_font_size
                            .insert(f32::from(theme_settings::clamp_font_size(buffer_font_size)).into());
                    });
                } else {
                    theme_settings::decrease_buffer_font_size(cx);
                }
            }
        })
        .register_action({
            let fs = app_state.fs.clone();
            move |_, action: &zed_actions::ResetBufferFontSize, _window, cx| {
                if action.persist {
                    update_settings_file(fs.clone(), cx, move |settings, _| {
                        settings.theme.buffer_font_size = None;
                    });
                } else {
                    theme_settings::reset_buffer_font_size(cx);
                }
            }
        })
        .register_action({
            let fs = app_state.fs.clone();
            move |_, action: &zed_actions::ResetAllZoom, _window, cx| {
                if action.persist {
                    update_settings_file(fs.clone(), cx, move |settings, _| {
                        settings.theme.ui_font_size = None;
                        settings.theme.buffer_font_size = None;
                        settings.theme.agent_ui_font_size = None;
                        settings.theme.agent_buffer_font_size = None;
                    });
                } else {
                    theme_settings::reset_ui_font_size(cx);
                    theme_settings::reset_buffer_font_size(cx);
                    theme_settings::reset_agent_ui_font_size(cx);
                    theme_settings::reset_agent_buffer_font_size(cx);
                }
            }
        })
        .register_action(open_project_settings_file)
        .register_action(open_project_tasks_file)
        .register_action({
            let app_state = app_state.clone();
            move |_, _: &NewWindow, _, cx| {
                open_new(
                    Default::default(),
                    app_state.clone(),
                    cx,
                    |workspace, window, cx| {
                        cx.activate(true);
                        // Create buffer synchronously to avoid flicker
                        let project = workspace.project().clone();
                        let buffer = project.update(cx, |project, cx| {
                            project.create_local_buffer("", None, true, cx)
                        });
                        let editor = cx.new(|cx| {
                            Editor::for_buffer(buffer, Some(project), window, cx)
                        });
                        workspace.add_item_to_active_pane(
                            Box::new(editor),
                            None,
                            true,
                            window,
                            cx,
                        );
                    },
                )
                .detach();
            }
        })
        .register_action({
            let app_state = app_state.clone();
            move |workspace, _: &CloseProject, window, cx| {
                let Some(window_handle) = window.window_handle().downcast::<MultiWorkspace>() else {
                    return;
                };
                let app_state = app_state.clone();
                let old_group_key = workspace.project_group_key(cx);
                cx.spawn_in(window, async move |this, cx| {
                    let should_continue = this
                        .update_in(cx, |workspace, window, cx| {
                            workspace.prepare_to_close(
                                CloseIntent::ReplaceWindow,
                                window,
                                cx,
                            )
                        })?
                        .await?;
                    if should_continue {
                        let task = cx.update(|_window, cx| {
                            open_new(
                                workspace::OpenOptions {
                                    requesting_window: Some(window_handle),
                                    ..Default::default()
                                },
                                app_state,
                                cx,
                                |workspace, window, cx| {
                                    cx.activate(true);
                                    let project = workspace.project().clone();
                                    let buffer = project.update(cx, |project, cx| {
                                        project.create_local_buffer("", None, true, cx)
                                    });
                                    let editor = cx.new(|cx| {
                                        Editor::for_buffer(buffer, Some(project), window, cx)
                                    });
                                    workspace.add_item_to_active_pane(
                                        Box::new(editor),
                                        None,
                                        true,
                                        window,
                                        cx,
                                    );
                                },
                            )
                        })?;
                        task.await?;
                        window_handle.update(cx, |mw, window, cx| {
                            mw.remove_project_group(&old_group_key, window, cx)
                        })?.await.log_err();
                        Ok::<(), anyhow::Error>(())
                    } else {
                        Ok(())
                    }
                })
                .detach_and_log_err(cx);
            }
        })
        .register_action({
            let app_state = app_state.clone();
            move |_, _: &NewFile, _, cx| {
                open_new(
                    Default::default(),
                    app_state.clone(),
                    cx,
                    |workspace, window, cx| {
                        Editor::new_file(workspace, &Default::default(), window, cx)
                    },
                )
                .detach_and_log_err(cx);
            }
        });

    if workspace.project().read(cx).is_via_remote_server() {
        workspace.register_action({
            move |workspace, _: &OpenServerSettings, window, cx| {
                let open_server_settings = workspace
                    .project()
                    .update(cx, |project, cx| project.open_server_settings(cx));

                cx.spawn_in(window, async move |workspace, cx| {
                    let buffer = open_server_settings.await?;

                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            workspace.open_path(
                                buffer
                                    .read(cx)
                                    .project_path(cx)
                                    .expect("Settings file must have a location"),
                                None,
                                true,
                                window,
                                cx,
                            )
                        })?
                        .await?;

                    anyhow::Ok(())
                })
                .detach_and_log_err(cx);
            }
        });
    }
}

fn initialize_pane(
    workspace: &Workspace,
    pane: &Entity<Pane>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let workspace_handle = cx.weak_entity();
    pane.update(cx, |pane, cx| {
        pane.toolbar().update(cx, |toolbar, cx| {
            let breadcrumbs = cx.new(|_| Breadcrumbs::new());
            toolbar.add_item(breadcrumbs, window, cx);
            let buffer_search_bar = cx.new(|cx| {
                search::BufferSearchBar::new(
                    Some(workspace.project().read(cx).languages().clone()),
                    window,
                    cx,
                )
            });
            toolbar.add_item(buffer_search_bar.clone(), window, cx);
            let quick_action_bar =
                cx.new(|cx| QuickActionBar::new(buffer_search_bar, cx));
            toolbar.add_item(quick_action_bar, window, cx);
            let lsp_log_item = cx.new(|_| LspLogToolbarItemView::new());
            toolbar.add_item(lsp_log_item, window, cx);
            let dap_log_item = cx.new(|_| debugger_tools::DapLogToolbarItemView::new());
            toolbar.add_item(dap_log_item, window, cx);
            let syntax_tree_item = cx.new(|_| language_tools::SyntaxTreeToolbarItemView::new());
            toolbar.add_item(syntax_tree_item, window, cx);
            let migration_banner =
                cx.new(|inner_cx| MigrationBanner::new(workspace_handle.clone(), inner_cx));
            toolbar.add_item(migration_banner, window, cx);
            let highlights_tree_item =
                cx.new(|_| language_tools::HighlightsTreeToolbarItemView::new());
            toolbar.add_item(highlights_tree_item, window, cx);
            let basedpyright_banner = cx.new(|cx| BasedPyrightBanner::new(workspace, cx));
            toolbar.add_item(basedpyright_banner, window, cx);
            let image_view_toolbar = cx.new(|_| image_viewer::ImageViewToolbarControls::new());
            toolbar.add_item(image_view_toolbar, window, cx);
        })
    });
}

fn open_about_window(cx: &mut App) {
    fn about_window_icon(release_channel: ReleaseChannel) -> Arc<Image> {
        let bytes = match release_channel {
            ReleaseChannel::Dev => include_bytes!("../resources/app-icon-dev.png").as_slice(),
            ReleaseChannel::Nightly => {
                include_bytes!("../resources/app-icon-nightly.png").as_slice()
            }
            ReleaseChannel::Preview => {
                include_bytes!("../resources/app-icon-preview.png").as_slice()
            }
            ReleaseChannel::Stable => include_bytes!("../resources/app-icon.png").as_slice(),
        };

        Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec()))
    }

    struct AboutWindow {
        focus_handle: FocusHandle,
        ok_entry: NavigableEntry,
        copy_entry: NavigableEntry,
        app_icon: Arc<Image>,
        message: SharedString,
        commit: Option<SharedString>,
        full_version: SharedString,
    }

    impl AboutWindow {
        fn new(cx: &mut Context<Self>) -> Self {
            let release_channel = ReleaseChannel::global(cx);
            let release_channel_name = release_channel.display_name();
            let full_version: SharedString = AppVersion::global(cx).to_string().into();
            let version = env!("CARGO_PKG_VERSION");

            let debug = if cfg!(debug_assertions) {
                "(debug)"
            } else {
                ""
            };
            let message: SharedString = format!("{release_channel_name} {version} {debug}").into();
            let commit = AppCommitSha::try_global(cx)
                .map(|sha| sha.full())
                .filter(|commit| !commit.is_empty())
                .map(SharedString::from);

            Self {
                focus_handle: cx.focus_handle(),
                ok_entry: NavigableEntry::focusable(cx),
                copy_entry: NavigableEntry::focusable(cx),
                app_icon: about_window_icon(release_channel),
                message,
                commit,
                full_version,
            }
        }

        fn copy_details(&self, window: &mut Window, cx: &mut Context<Self>) {
            let content = match self.commit.as_ref() {
                Some(commit) => {
                    format!(
                        "{}\nCommit: {}\nVersion: {}",
                        self.message, commit, self.full_version
                    )
                }
                None => format!("{}\nVersion: {}", self.message, self.full_version),
            };
            cx.write_to_clipboard(ClipboardItem::new_string(content));
            window.remove_window();
        }
    }

    impl Render for AboutWindow {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let ok_is_focused = self.ok_entry.focus_handle.contains_focused(window, cx);
            let copy_is_focused = self.copy_entry.focus_handle.contains_focused(window, cx);

            Navigable::new(
                v_flex()
                    .id("about-window")
                    .track_focus(&self.focus_handle)
                    .on_action(cx.listener(|_, _: &menu::Cancel, window, _cx| {
                        window.remove_window();
                    }))
                    .min_w_0()
                    .size_full()
                    .bg(cx.theme().colors().editor_background)
                    .text_color(cx.theme().colors().text)
                    .p_4()
                    .when(cfg!(target_os = "macos"), |this| this.pt_10())
                    .gap_4()
                    .text_center()
                    .justify_between()
                    .child(
                        v_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(img(self.app_icon.clone()).size_16().flex_none())
                            .child(Headline::new(self.message.clone()))
                            .when_some(self.commit.clone(), |this, commit| {
                                this.child(
                                    Label::new("Commit")
                                        .color(Color::Muted)
                                        .size(LabelSize::XSmall),
                                )
                                .child(Label::new(commit).size(LabelSize::Small))
                            })
                            .child(
                                Label::new("Version")
                                    .color(Color::Muted)
                                    .size(LabelSize::XSmall),
                            )
                            .child(Label::new(self.full_version.clone()).size(LabelSize::Small)),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .track_focus(&self.ok_entry.focus_handle)
                                    .on_action(cx.listener(|_, _: &menu::Confirm, window, _cx| {
                                        window.remove_window();
                                    }))
                                    .child(
                                        Button::new("ok", "Ok")
                                            .full_width()
                                            .style(ButtonStyle::OutlinedGhost)
                                            .toggle_state(ok_is_focused)
                                            .selected_style(ButtonStyle::Tinted(TintColor::Accent))
                                            .on_click(cx.listener(|_, _, window, _cx| {
                                                window.remove_window();
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .track_focus(&self.copy_entry.focus_handle)
                                    .on_action(cx.listener(
                                        |this, _: &menu::Confirm, window, cx| {
                                            this.copy_details(window, cx);
                                        },
                                    ))
                                    .child(
                                        Button::new("copy", "Copy")
                                            .full_width()
                                            .style(ButtonStyle::Tinted(TintColor::Accent))
                                            .toggle_state(copy_is_focused)
                                            .selected_style(ButtonStyle::Tinted(TintColor::Accent))
                                            .on_click(cx.listener(|this, _event, window, cx| {
                                                this.copy_details(window, cx);
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .track_focus(&self.copy_entry.focus_handle)
                                    .on_action(cx.listener(
                                        |this, _: &menu::Confirm, window, cx| {
                                            this.copy_details(window, cx);
                                        },
                                    ))
                                    .child(
                                        Button::new("licenses", "Licenses")
                                            .full_width()
                                            .style(ButtonStyle::Tinted(TintColor::Accent))
                                            .toggle_state(copy_is_focused)
                                            .selected_style(ButtonStyle::Tinted(TintColor::Accent))
                                            .on_click(cx.listener(|_this, _event, window, cx| {
                                                with_active_or_new_workspace(cx, |workspace, window, cx| {
                                                    open_bundled_file(
                                                        workspace,
                                                        asset_str::<Assets>("licenses.txt"),
                                                        "Open Source License Attribution",
                                                        "Plain Text",
                                                        window,
                                                        cx,
                                                    );
                                                });
                                                window.remove_window();
                                            })),
                                    )
                            ),
                    )
                    .into_any_element(),
            )
            .entry(self.ok_entry.clone())
            .entry(self.copy_entry.clone())
        }
    }

    impl Focusable for AboutWindow {
        fn focus_handle(&self, _cx: &App) -> FocusHandle {
            self.ok_entry.focus_handle.clone()
        }
    }

    // Don't open about window twice
    if let Some(existing) = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<AboutWindow>())
    {
        existing
            .update(cx, |about_window, window, cx| {
                window.activate_window();
                about_window.ok_entry.focus_handle.focus(window, cx);
            })
            .log_err();
        return;
    }

    let window_size = Size {
        width: px(440.),
        height: px(300.),
    };

    cx.open_window(
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("About Zed".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.), px(12.))),
            }),
            window_bounds: Some(WindowBounds::centered(window_size, cx)),
            is_resizable: false,
            is_minimizable: false,
            kind: WindowKind::Floating,
            app_id: Some(ReleaseChannel::global(cx).app_id().to_owned()),
            ..Default::default()
        },
        |window, cx| {
            let about_window = cx.new(AboutWindow::new);
            let focus_handle = about_window.read(cx).ok_entry.focus_handle.clone();
            window.activate_window();
            focus_handle.focus(window, cx);
            about_window
        },
    )
    .log_err();
}

static WAITING_QUIT_CONFIRMATION: AtomicBool = AtomicBool::new(false);
fn quit(_: &Quit, cx: &mut App) {
    if WAITING_QUIT_CONFIRMATION.load(atomic::Ordering::Acquire) {
        return;
    }

    let should_confirm = WorkspaceSettings::get_global(cx).confirm_quit;
    cx.spawn(async move |cx| {
        let mut workspace_windows: Vec<WindowHandle<MultiWorkspace>> = cx.update(|cx| {
            cx.windows()
                .into_iter()
                .filter_map(|window| window.downcast::<MultiWorkspace>())
                .collect::<Vec<_>>()
        });

        // If multiple windows have unsaved changes, and need a save prompt,
        // prompt in the active window before switching to a different window.
        cx.update(|cx| {
            workspace_windows.sort_by_key(|window| window.is_active(cx) == Some(false));
        });

        if should_confirm && let Some(multi_workspace) = workspace_windows.first() {
            let answer = multi_workspace
                .update(cx, |_, window, cx| {
                    window.prompt(
                        PromptLevel::Info,
                        "Are you sure you want to quit?",
                        None,
                        &["Quit", "Cancel"],
                        cx,
                    )
                })
                .log_err();

            if let Some(answer) = answer {
                WAITING_QUIT_CONFIRMATION.store(true, atomic::Ordering::Release);
                let answer = answer.await.ok();
                WAITING_QUIT_CONFIRMATION.store(false, atomic::Ordering::Release);
                if answer != Some(0) {
                    return Ok(());
                }
            }
        }

        // If the user cancels any save prompt, then keep the app open.
        for window in &workspace_windows {
            let window = *window;
            let workspaces = window
                .update(cx, |multi_workspace, _, _cx| {
                    multi_workspace.workspaces().cloned().collect::<Vec<_>>()
                })
                .log_err();

            let Some(workspaces) = workspaces else {
                continue;
            };

            for workspace in workspaces {
                if let Some(should_close) = window
                    .update(cx, |multi_workspace, window, cx| {
                        multi_workspace.activate(workspace.clone(), None, window, cx);
                        window.activate_window();
                        workspace.update(cx, |workspace, cx| {
                            workspace.prepare_to_close(CloseIntent::Quit, window, cx)
                        })
                    })
                    .log_err()
                {
                    if !should_close.await? {
                        return Ok(());
                    }
                }
            }
        }
        // Flush all pending workspace serialization before quitting so that
        // session_id/window_id are up-to-date in the database.
        let mut flush_tasks = Vec::new();
        for window in &workspace_windows {
            window
                .update(cx, |multi_workspace, window, cx| {
                    for workspace in multi_workspace.workspaces() {
                        flush_tasks.push(workspace.update(cx, |workspace, cx| {
                            workspace.flush_serialization(window, cx)
                        }));
                    }
                    flush_tasks.append(&mut multi_workspace.take_pending_removal_tasks());
                    flush_tasks.push(multi_workspace.flush_serialization());
                })
                .log_err();
        }
        futures::future::join_all(flush_tasks).await;

        cx.update(|cx| cx.quit());
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

fn notify_settings_errors(result: settings::SettingsParseResult, is_user: bool, cx: &mut App) {
    if let settings::ParseStatus::Failed { error: err } = &result.parse_status {
        let settings_type = if is_user { "user" } else { "global" };
        log::error!("Failed to load {} settings: {err}", settings_type);
    }

    let error = match result.parse_status {
        settings::ParseStatus::Failed { error } => Some(anyhow::format_err!(error)),
        settings::ParseStatus::Success => None,
        settings::ParseStatus::Unchanged => return,
    };
    let id = NotificationId::Named(format!("failed-to-parse-settings-{is_user}").into());

    let showed_parse_error = match error {
        Some(error) => {
            if let Some(InvalidSettingsError::LocalSettings { .. }) =
                error.downcast_ref::<InvalidSettingsError>()
            {
                false
                // Local settings errors are displayed by the projects
            } else {
                show_app_notification(id, cx, move |cx| {
                    cx.new(|cx| {
                        MessageNotification::new(format!("Invalid user settings file\n{error}"), cx)
                            .primary_message("Open Settings File")
                            .primary_icon(IconName::Settings)
                            .primary_on_click(|window, cx| {
                                window.dispatch_action(
                                    zed_actions::OpenSettingsFile.boxed_clone(),
                                    cx,
                                );
                                cx.emit(DismissEvent);
                            })
                    })
                });
                true
            }
        }
        None => {
            dismiss_app_notification(&id, cx);
            false
        }
    };
    let id = NotificationId::Named(format!("failed-to-migrate-settings-{is_user}").into());

    match result.migration_status {
        settings::MigrationStatus::Succeeded | settings::MigrationStatus::NotNeeded => {
            dismiss_app_notification(&id, cx);
        }
        settings::MigrationStatus::Failed { error: err } => {
            if !showed_parse_error {
                show_app_notification(id, cx, move |cx| {
                    cx.new(|cx| {
                        MessageNotification::new(
                            format!(
                                "Failed to migrate settings\n\
                                {err}"
                            ),
                            cx,
                        )
                        .primary_message("Open Settings File")
                        .primary_icon(IconName::Settings)
                        .primary_on_click(|window, cx| {
                            window.dispatch_action(zed_actions::OpenSettingsFile.boxed_clone(), cx);
                            cx.emit(DismissEvent);
                        })
                    })
                });
            }
        }
    };
}

#[derive(Copy, Clone, Debug, settings::RegisterSetting)]
struct CursorHideModeSetting(gpui::CursorHideMode);

impl Settings for CursorHideModeSetting {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        Self(match content.hide_mouse.unwrap_or_default() {
            settings::HideMouseMode::Never => gpui::CursorHideMode::Never,
            settings::HideMouseMode::OnTyping => gpui::CursorHideMode::OnTyping,
            settings::HideMouseMode::OnTypingAndAction => gpui::CursorHideMode::OnTypingAndAction,
        })
    }
}

fn init_cursor_hide_mode(cx: &mut App) {
    let apply = |cx: &mut App| cx.set_cursor_hide_mode(CursorHideModeSetting::get_global(cx).0);
    apply(cx);
    cx.observe_global::<SettingsStore>(apply).detach();
}

pub fn watch_settings_files(fs: Arc<dyn fs::Fs>, cx: &mut App) {
    MigrationNotification::set_global(cx.new(|_| MigrationNotification), cx);

    SettingsStore::update_global(cx, move |store, cx| {
        store.watch_settings_files(fs, cx, |settings_file, result, cx| {
            let is_user = matches!(settings_file, SettingsFile::User);
            let migrating_in_memory =
                matches!(&result.migration_status, MigrationStatus::Succeeded);
            notify_settings_errors(result, is_user, cx);
            if let Some(notifier) = MigrationNotification::try_global(cx) {
                notifier.update(cx, |_, cx| {
                    cx.emit(MigrationEvent::ContentChanged {
                        migration_type: MigrationType::Settings,
                        migrating_in_memory,
                    });
                });
            }
        });
    });
}

pub fn handle_keymap_file_changes(
    mut user_keymap_file_rx: mpsc::UnboundedReceiver<String>,
    user_keymap_watcher: gpui::Task<()>,
    cx: &mut App,
) {
    let (base_keymap_tx, mut base_keymap_rx) = mpsc::unbounded();
    let (keyboard_layout_tx, mut keyboard_layout_rx) = mpsc::unbounded();
    let mut old_base_keymap = *BaseKeymap::get_global(cx);

    cx.observe_global::<SettingsStore>(move |cx| {
        let new_base_keymap = *BaseKeymap::get_global(cx);

        if new_base_keymap != old_base_keymap {
            old_base_keymap = new_base_keymap;
            base_keymap_tx.unbounded_send(()).unwrap();
        }
    })
    .detach();

    #[cfg(target_os = "windows")]
    {
        let mut current_layout_id = cx.keyboard_layout().id().to_string();
        cx.on_keyboard_layout_change(move |cx| {
            let next_layout_id = cx.keyboard_layout().id();
            if next_layout_id != current_layout_id {
                current_layout_id = next_layout_id.to_string();
                keyboard_layout_tx.unbounded_send(()).ok();
            }
        })
        .detach();
    }

    #[cfg(not(target_os = "windows"))]
    {
        let mut current_mapping = cx.keyboard_mapper().get_key_equivalents().cloned();
        cx.on_keyboard_layout_change(move |cx| {
            let next_mapping = cx.keyboard_mapper().get_key_equivalents();
            if current_mapping.as_ref() != next_mapping {
                current_mapping = next_mapping.cloned();
                keyboard_layout_tx.unbounded_send(()).ok();
            }
        })
        .detach();
    }

    load_default_keymap(cx);

    struct KeymapParseErrorNotification;
    let notification_id = NotificationId::unique::<KeymapParseErrorNotification>();

    cx.spawn(async move |cx| {
        let _user_keymap_watcher = user_keymap_watcher;
        let mut user_keymap_content = String::new();
        let mut migrating_in_memory = false;
        loop {
            select_biased! {
                _ = base_keymap_rx.next() => {},
                _ = keyboard_layout_rx.next() => {},
                content = user_keymap_file_rx.next() => {
                    if let Some(content) = content {
                        if let Ok(Some(migrated_content)) = migrate_keymap(&content) {
                            user_keymap_content = migrated_content;
                            migrating_in_memory = true;
                        } else {
                            user_keymap_content = content;
                            migrating_in_memory = false;
                        }
                    }
                }
            };
            cx.update(|cx| {
                if let Some(notifier) = MigrationNotification::try_global(cx) {
                    notifier.update(cx, |_, cx| {
                        cx.emit(MigrationEvent::ContentChanged {
                            migration_type: MigrationType::Keymap,
                            migrating_in_memory,
                        });
                    });
                }
                let load_result = KeymapFile::load(&user_keymap_content, cx);
                match load_result {
                    KeymapFileLoadResult::Success { key_bindings } => {
                        reload_keymaps(cx, key_bindings);
                        dismiss_app_notification(&notification_id.clone(), cx);
                    }
                    KeymapFileLoadResult::SomeFailedToLoad {
                        key_bindings,
                        error_message,
                    } => {
                        if !key_bindings.is_empty() {
                            reload_keymaps(cx, key_bindings);
                        }
                        show_keymap_file_load_error(notification_id.clone(), error_message, cx);
                    }
                    KeymapFileLoadResult::JsonParseFailure { error } => {
                        show_keymap_file_json_error(notification_id.clone(), &error, cx)
                    }
                }
            });
        }
    })
    .detach();
}

fn show_keymap_file_json_error(
    notification_id: NotificationId,
    error: &anyhow::Error,
    cx: &mut App,
) {
    let message: SharedString =
        format!("JSON parse error in keymap file. Bindings not reloaded.\n\n{error}").into();
    show_app_notification(notification_id, cx, move |cx| {
        cx.new(|cx| {
            MessageNotification::new(message.clone(), cx)
                .primary_message("Open Keymap File")
                .primary_icon(IconName::Settings)
                .primary_on_click(|window, cx| {
                    window.dispatch_action(zed_actions::OpenKeymapFile.boxed_clone(), cx);
                    cx.emit(DismissEvent);
                })
        })
    });
}

fn show_keymap_file_load_error(
    notification_id: NotificationId,
    error_message: MarkdownString,
    cx: &mut App,
) {
    show_markdown_app_notification(
        notification_id,
        error_message,
        "Open Keymap File".into(),
        |window, cx| {
            window.dispatch_action(zed_actions::OpenKeymapFile.boxed_clone(), cx);
            cx.emit(DismissEvent);
        },
        cx,
    )
}

fn show_markdown_app_notification<F>(
    notification_id: NotificationId,
    message: MarkdownString,
    primary_button_message: SharedString,
    primary_button_on_click: F,
    cx: &mut App,
) where
    F: 'static + Send + Sync + Fn(&mut Window, &mut Context<MessageNotification>),
{
    let markdown = cx.new(|cx| Markdown::new(message.0.into(), None, None, cx));
    let primary_button_on_click = Arc::new(primary_button_on_click);

    show_app_notification(notification_id, cx, move |cx| {
        let markdown = markdown.clone();
        let primary_button_message = primary_button_message.clone();
        let primary_button_on_click = primary_button_on_click.clone();

        cx.new(move |cx| {
            MessageNotification::new_from_builder(cx, move |window, cx| {
                image_cache(retain_all("notification-cache"))
                    .child(div().text_ui(cx).child(MarkdownElement::new(
                        markdown.clone(),
                        MarkdownStyle::themed(MarkdownFont::Editor, window, cx),
                    )))
                    .into_any()
            })
            .primary_message(primary_button_message)
            .primary_icon(IconName::Settings)
            .primary_on_click_arc(primary_button_on_click)
        })
    })
}

fn reload_keymaps(cx: &mut App, mut user_key_bindings: Vec<KeyBinding>) {
    cx.clear_key_bindings();
    load_default_keymap(cx);

    for key_binding in &mut user_key_bindings {
        key_binding.set_meta(KeybindSource::User.meta());
    }
    cx.bind_keys(user_key_bindings);

    let menus = app_menus(cx);
    cx.set_menus(menus);
    // On Windows, this is set in the `update_jump_list` method of the `HistoryManager`.
    #[cfg(not(target_os = "windows"))]
    cx.set_dock_menu(vec![gpui::MenuItem::action(
        "New Window",
        workspace::NewWindow,
    )]);
    // todo: nicer api here?
    keymap_editor::KeymapEventChannel::trigger_keymap_changed(cx);
}

pub fn load_default_keymap(cx: &mut App) {
    let base_keymap = *BaseKeymap::get_global(cx);
    if base_keymap == BaseKeymap::None {
        return;
    }

    cx.bind_keys(
        KeymapFile::load_asset(DEFAULT_KEYMAP_PATH, Some(KeybindSource::Default), cx).unwrap(),
    );

    if let Some(asset_path) = base_keymap.asset_path() {
        cx.bind_keys(KeymapFile::load_asset(asset_path, Some(KeybindSource::Base), cx).unwrap());
    }
}

fn open_project_settings_file(
    workspace: &mut Workspace,
    _: &OpenProjectSettingsFile,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    open_local_file(
        workspace,
        local_settings_file_relative_path(),
        initial_project_settings_content(),
        window,
        cx,
    )
}

fn open_project_tasks_file(
    workspace: &mut Workspace,
    _: &OpenProjectTasks,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    open_local_file(
        workspace,
        local_tasks_file_relative_path(),
        initial_tasks_content(),
        window,
        cx,
    )
}

fn open_local_file(
    workspace: &mut Workspace,
    settings_relative_path: &'static RelPath,
    initial_contents: Cow<'static, str>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let project = workspace.project().clone();
    let worktree = project
        .read(cx)
        .visible_worktrees(cx)
        .find_map(|tree| tree.read(cx).root_entry()?.is_dir().then_some(tree));
    if let Some(worktree) = worktree {
        let tree_id = worktree.read(cx).id();
        cx.spawn_in(window, async move |workspace, cx| {
            // Check if the file actually exists on disk (even if it's excluded from worktree)
            let file_exists = {
                let full_path = worktree.read_with(cx, |tree, _| {
                    tree.abs_path().join(settings_relative_path.as_std_path())
                });

                let fs = project.read_with(cx, |project, _| project.fs().clone());

                fs.metadata(&full_path)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|metadata| !metadata.is_dir && !metadata.is_fifo)
            };

            if !file_exists {
                if let Some(dir_path) = settings_relative_path.parent()
                    && worktree.read_with(cx, |tree, _| tree.entry_for_path(dir_path).is_none())
                {
                    project
                        .update(cx, |project, cx| {
                            project.create_entry((tree_id, dir_path), true, cx)
                        })
                        .await
                        .context("worktree was removed")?;
                }

                if worktree.read_with(cx, |tree, _| {
                    tree.entry_for_path(settings_relative_path).is_none()
                }) {
                    project
                        .update(cx, |project, cx| {
                            project.create_entry((tree_id, settings_relative_path), false, cx)
                        })
                        .await
                        .context("worktree was removed")?;
                }
            }

            let editor = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.open_path((tree_id, settings_relative_path), None, true, window, cx)
                })?
                .await?
                .downcast::<Editor>()
                .context("unexpected item type: expected editor item")?;

            editor
                .downgrade()
                .update(cx, |editor, cx| {
                    let buffer = editor.buffer().read(cx).as_singleton();
                    if buffer.read(cx).is_empty() {
                        buffer.update(cx, |buffer, cx| {
                            buffer.edit([(0..0, initial_contents)], None, cx)
                        });
                    }
                })
                .ok();

            anyhow::Ok(())
        })
        .detach();
    } else {
        struct NoOpenFolders;

        workspace.show_notification(NotificationId::unique::<NoOpenFolders>(), cx, |cx| {
            cx.new(|cx| MessageNotification::new("This project has no folders open.", cx))
        })
    }
}

fn open_bundled_file(
    workspace: &mut Workspace,
    text: Cow<'static, str>,
    title: &'static str,
    language: &'static str,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let existing = workspace.items_of_type::<Editor>(cx).find(|editor| {
        editor.read_with(cx, |editor, cx| {
            editor.read_only(cx)
                && editor.title(cx).as_ref() == title
                && editor
                    .buffer()
                    .read(cx)
                    .as_singleton()
                    .read(cx).file().is_none()
        })
    });
    if let Some(existing) = existing {
        workspace.activate_item(&existing, true, true, window, cx);
        return;
    }

    let language = workspace.app_state().languages.language_for_name(language);
    cx.spawn_in(window, async move |workspace, cx| {
        let language = language.await.log_err();
        workspace
            .update_in(cx, move |workspace, window, cx| {
                let project = workspace.project().clone();
                let buffer = project.update(cx, move |project, cx| {
                    project.create_buffer(language, false, cx)
                });
                cx.spawn_in(window, async move |workspace, cx| {
                    let buffer = buffer.await?;
                    buffer.update(cx, |buffer, cx| {
                        buffer.set_text(text.into_owned(), cx);
                        buffer.set_capability(Capability::ReadOnly, cx);
                    });
                    let buffer =
                        cx.new(|cx| MultiBuffer::singleton(buffer, cx).with_title(title.into()));
                    workspace.update_in(cx, |workspace, window, cx| {
                        workspace.add_item_to_active_pane(
                            Box::new(cx.new(|cx| {
                                let mut editor = Editor::for_multibuffer(
                                    buffer,
                                    Some(project.clone()),
                                    window,
                                    cx,
                                );
                                editor.set_read_only(true);
                                editor.set_should_serialize(false, cx);
                                editor.set_breadcrumb_header(title.into());
                                editor
                            })),
                            None,
                            true,
                            window,
                            cx,
                        )
                    })
                })
            })?
            .await
    })
    .detach_and_log_err(cx);
}

fn open_settings_file(
    abs_path: &'static Path,
    default_content: impl FnOnce() -> Rope + Send + 'static,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    cx.spawn_in(window, async move |workspace, cx| {
        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.with_local_or_wsl_workspace(window, cx, move |workspace, window, cx| {
                    let project = workspace.project().clone();

                    cx.spawn_in(window, async move |workspace, cx| {
                        let config_dir = project
                            .update(cx, |project, cx| {
                                project.try_windows_path_to_wsl(paths::config_dir().as_path(), cx)
                            })
                            .await?;
                        // Set up a dedicated worktree for settings, since
                        // otherwise we're dropping and re-starting LSP servers
                        // for each file inside on every settings file
                        // close/open

                        // TODO: Do note that all other external files (e.g.
                        // drag and drop from OS) still have their worktrees
                        // released on file close, causing LSP servers'
                        // restarts.
                        let (_worktree, _) = project
                            .update(cx, |project, cx| {
                                project.find_or_create_worktree(&config_dir, false, cx)
                            })
                            .await?;

                        workspace
                            .update_in(cx, |_, window, cx| {
                                create_and_open_local_file(abs_path, window, cx, default_content)
                            })?
                            .await?;
                        anyhow::Ok(())
                    })
                })
            })?
            .await?
            .await?;
        anyhow::Ok(())
    })
    .detach_and_log_err(cx);
}

/// Eagerly loads the active theme and icon theme based on the selections in the
/// theme settings.
///
/// This fast path exists to load these themes as soon as possible so the user
/// doesn't see the default themes while waiting on extensions to load.
pub(crate) fn eager_load_active_theme_and_icon_theme(fs: Arc<dyn Fs>, cx: &mut App) {
    let extension_store = ExtensionStore::global(cx);
    let theme_registry = ThemeRegistry::global(cx);
    let theme_settings = ThemeSettings::get_global(cx);
    let appearance = SystemAppearance::global(cx).0;

    enum LoadTarget {
        Theme(PathBuf),
        IconTheme((PathBuf, PathBuf)),
    }

    let theme_name = theme_settings.theme.name(appearance);
    let icon_theme_name = theme_settings.icon_theme.name(appearance);
    let themes_to_load = [
        theme_registry
            .get(&theme_name.0)
            .is_err()
            .then(|| {
                extension_store
                    .read(cx)
                    .path_to_extension_theme(&theme_name.0)
            })
            .flatten()
            .map(LoadTarget::Theme),
        theme_registry
            .get_icon_theme(&icon_theme_name.0)
            .is_err()
            .then(|| {
                extension_store
                    .read(cx)
                    .path_to_extension_icon_theme(&icon_theme_name.0)
            })
            .flatten()
            .map(LoadTarget::IconTheme),
    ];

    enum ReloadTarget {
        Theme,
        IconTheme,
    }

    let executor = cx.background_executor();
    let reload_tasks = parking_lot::Mutex::new(Vec::with_capacity(themes_to_load.len()));

    let mut themes_to_load = themes_to_load.into_iter().flatten().peekable();

    if themes_to_load.peek().is_none() {
        return;
    }

    cx.foreground_executor().block_on(executor.scoped(|scope| {
        for load_target in themes_to_load {
            let theme_registry = &theme_registry;
            let reload_tasks = &reload_tasks;
            let fs = fs.clone();

            scope.spawn(async move {
                match load_target {
                    LoadTarget::Theme(theme_path) => {
                        if let Some(bytes) = fs.load_bytes(&theme_path).await.log_err()
                            && load_user_theme(theme_registry, &bytes).log_err().is_some()
                        {
                            reload_tasks.lock().push(ReloadTarget::Theme);
                        }
                    }
                    LoadTarget::IconTheme((icon_theme_path, icons_root_path)) => {
                        if let Some(bytes) = fs.load_bytes(&icon_theme_path).await.log_err()
                            && let Some(icon_theme_family) =
                                deserialize_icon_theme(&bytes).log_err()
                            && theme_registry
                                .load_icon_theme(icon_theme_family, &icons_root_path)
                                .log_err()
                                .is_some()
                        {
                            reload_tasks.lock().push(ReloadTarget::IconTheme);
                        }
                    }
                }
            });
        }
    }));

    for reload_target in reload_tasks.into_inner() {
        match reload_target {
            ReloadTarget::Theme => theme_settings::reload_theme(cx),
            ReloadTarget::IconTheme => theme_settings::reload_icon_theme(cx),
        };
    }
}
