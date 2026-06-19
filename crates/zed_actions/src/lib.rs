use gpui::{Action, actions};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// If the zed binary doesn't use anything in this crate, it will be optimized away
// and the actions won't initialize. So we just provide an empty initialization function
// to be called from main.
//
// These may provide relevant context:
// https://github.com/rust-lang/rust/issues/47384
// https://github.com/mmastrac/rust-ctor/issues/280
pub fn init() {}

/// Opens a URL in the system's default web browser.
#[derive(Clone, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct OpenBrowser {
    pub url: String,
}

/// Opens a zed:// URL within the application.
#[derive(Clone, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct OpenZedUrl {
    pub url: String,
}

/// Opens the keymap to either add a keybinding or change an existing one
#[derive(PartialEq, Clone, Default, Action, JsonSchema, Serialize, Deserialize)]
#[action(namespace = zed, no_json, no_register)]
pub struct ChangeKeybinding {
    pub action: String,
}

actions!(
    zed,
    [
        /// Opens the settings editor.
        #[action(deprecated_aliases = ["zed_actions::OpenSettingsEditor"])]
        OpenSettings,
        /// Opens the settings JSON file.
        #[action(deprecated_aliases = ["zed_actions::OpenSettings"])]
        OpenSettingsFile,
        /// Opens project-specific settings.
        #[action(deprecated_aliases = ["zed_actions::OpenProjectSettings"])]
        OpenProjectSettings,
        /// Opens the default keymap file.
        OpenDefaultKeymap,
        /// Opens the user keymap file.
        #[action(deprecated_aliases = ["zed_actions::OpenKeymap"])]
        OpenKeymapFile,
        /// Opens the keymap editor.
        #[action(deprecated_aliases = ["zed_actions::OpenKeymapEditor"])]
        OpenKeymap,
        /// Opens account settings.
        OpenAccountSettings,
        /// Opens server settings.
        OpenServerSettings,
        /// Quits the application.
        Quit,
        /// Shows information about Zed.
        About,
        /// Opens the documentation website.
        OpenDocs,
        /// Views open source licenses.
        OpenLicenses,
        /// Opens the Zed status page.
        OpenStatusPage,
        /// Opens the performance profiler.
        OpenPerformanceProfiler,
        /// Opens the onboarding view.
        OpenOnboarding,
        /// Shows the auto-update notification for testing.
        ShowUpdateNotification,
    ]
);

/// Opens the ACP registry.
#[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct AcpRegistry;

/// Show call diagnostics and connection quality statistics.
#[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = collab)]
#[serde(deny_unknown_fields)]
pub struct ShowCallStats;

/// Decreases the font size in the editor buffer.
#[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct DecreaseBufferFontSize {
    #[serde(default)]
    pub persist: bool,
}

/// Increases the font size in the editor buffer.
#[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct IncreaseBufferFontSize {
    #[serde(default)]
    pub persist: bool,
}

/// Opens the settings editor at a specific path.
#[derive(PartialEq, Clone, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct OpenSettingsAt {
    /// A path to a specific setting (e.g. `theme.mode`)
    pub path: String,
}

/// Resets the buffer font size to the default value.
#[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct ResetBufferFontSize {
    #[serde(default)]
    pub persist: bool,
}

/// Decreases the font size of the user interface.
#[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct DecreaseUiFontSize {
    #[serde(default)]
    pub persist: bool,
}

/// Increases the font size of the user interface.
#[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct IncreaseUiFontSize {
    #[serde(default)]
    pub persist: bool,
}

/// Resets the UI font size to the default value.
#[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct ResetUiFontSize {
    #[serde(default)]
    pub persist: bool,
}

/// Resets all zoom levels (UI and buffer font sizes, including in the agent panel) to their default values.
#[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct ResetAllZoom {
    #[serde(default)]
    pub persist: bool,
}

pub mod editor {
    use gpui::actions;
    actions!(
        editor,
        [
            /// Moves cursor up.
            MoveUp,
            /// Moves cursor down.
            MoveDown,
            /// Reveals the current file in the system file manager.
            RevealInFileManager,
        ]
    );
}

pub mod dev {
    use gpui::actions;

    actions!(
        dev,
        [
            /// Toggles the developer inspector for debugging UI elements.
            ToggleInspector
        ]
    );
}

pub mod remote_debug {
    use gpui::actions;

    actions!(
        remote_debug,
        [
            /// Simulates a disconnection from the remote server for testing purposes.
            /// This will trigger the reconnection logic.
            SimulateDisconnect,
            /// Simulates a timeout/slow connection to the remote server for testing purposes.
            /// This will cause heartbeat failures and trigger reconnection.
            SimulateTimeout,
            /// Simulates a timeout/slow connection to the remote server for testing purposes.
            /// This will cause heartbeat failures and attempting a reconnection while having exhausted all attempts.
            SimulateTimeoutExhausted,
        ]
    );
}

pub mod workspace {
    use gpui::actions;

    actions!(
        workspace,
        [
            #[action(deprecated_aliases = ["editor::CopyPath", "outline_panel::CopyPath", "project_panel::CopyPath"])]
            CopyPath,
            #[action(deprecated_aliases = ["editor::CopyRelativePath", "outline_panel::CopyRelativePath", "project_panel::CopyRelativePath"])]
            CopyRelativePath,
            /// Opens the selected file with the system's default application.
            #[action(deprecated_aliases = ["project_panel::OpenWithSystem"])]
            OpenWithSystem,
        ]
    );
}

pub mod toast {
    use gpui::actions;

    actions!(
        toast,
        [
            /// Runs the action associated with a toast notification.
            RunAction
        ]
    );
}

pub mod command_palette {
    use gpui::actions;

    actions!(
        command_palette,
        [
            /// Toggles the command palette.
            Toggle,
        ]
    );
}

pub mod theme {
    use gpui::actions;

    actions!(theme, [ToggleMode]);
}

pub mod theme_selector {
    use gpui::Action;
    use schemars::JsonSchema;
    use serde::Deserialize;

    /// Toggles the theme selector interface.
    #[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
    #[action(namespace = theme_selector)]
    #[serde(deny_unknown_fields)]
    pub struct Toggle {
        /// A list of theme names to filter the theme selector down to.
        pub themes_filter: Option<Vec<String>>,
    }
}

pub mod icon_theme_selector {
    use gpui::Action;
    use schemars::JsonSchema;
    use serde::Deserialize;

    /// Toggles the icon theme selector interface.
    #[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
    #[action(namespace = icon_theme_selector)]
    #[serde(deny_unknown_fields)]
    pub struct Toggle {
        /// A list of icon theme names to filter the theme selector down to.
        pub themes_filter: Option<Vec<String>>,
    }
}

pub mod search {
    use gpui::actions;
    actions!(
        search,
        [
            /// Toggles searching in ignored files.
            ToggleIncludeIgnored
        ]
    );
}
pub mod buffer_search {
    use gpui::{Action, actions};
    use schemars::JsonSchema;
    use serde::Deserialize;

    /// Opens the buffer search interface with the specified configuration.
    #[derive(PartialEq, Clone, Deserialize, JsonSchema, Action)]
    #[action(namespace = buffer_search)]
    #[serde(deny_unknown_fields)]
    pub struct Deploy {
        #[serde(default = "util::serde::default_true")]
        pub focus: bool,
        #[serde(default)]
        pub replace_enabled: bool,
        #[serde(default)]
        pub selection_search_enabled: bool,
    }

    impl Deploy {
        pub fn find() -> Self {
            Self {
                focus: true,
                replace_enabled: false,
                selection_search_enabled: false,
            }
        }

        pub fn replace() -> Self {
            Self {
                focus: true,
                replace_enabled: true,
                selection_search_enabled: false,
            }
        }
    }

    actions!(
        buffer_search,
        [
            /// Deploys the search and replace interface.
            DeployReplace,
            /// Dismisses the search bar.
            Dismiss,
            /// Focuses back on the editor.
            FocusEditor,
            /// Sets the search query from the selection or word under cursor.
            UseSelectionForFind,
        ]
    );
}
pub mod settings_profile_selector {
    use gpui::Action;
    use schemars::JsonSchema;
    use serde::Deserialize;

    #[derive(PartialEq, Clone, Default, Debug, Deserialize, JsonSchema, Action)]
    #[action(namespace = settings_profile_selector)]
    pub struct Toggle;
}

/// Opens the recent projects interface.
#[derive(PartialEq, Clone, Deserialize, Default, JsonSchema, Action)]
#[action(namespace = projects)]
#[serde(deny_unknown_fields)]
pub struct OpenRecent {
    #[serde(default)]
    pub create_new_window: bool,
}

/// Where to spawn the task in the UI.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RevealTarget {
    /// In the central pane group, "main" editor area.
    Center,
    /// In the terminal dock, "regular" terminal items' place.
    #[default]
    Dock,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WslConnectionOptions {
    pub distro_name: String,
    pub user: Option<String>,
}

#[cfg(target_os = "windows")]
pub mod wsl_actions {
    use gpui::Action;
    use schemars::JsonSchema;
    use serde::Deserialize;

    /// Opens a folder inside Wsl.
    #[derive(PartialEq, Clone, Deserialize, Default, JsonSchema, Action)]
    #[action(namespace = projects)]
    #[serde(deny_unknown_fields)]
    pub struct OpenFolderInWsl {
        #[serde(default)]
        pub create_new_window: bool,
    }

    /// Open a wsl distro.
    #[derive(PartialEq, Clone, Deserialize, Default, JsonSchema, Action)]
    #[action(namespace = projects)]
    #[serde(deny_unknown_fields)]
    pub struct OpenWsl {
        #[serde(default)]
        pub create_new_window: bool,
    }
}
