use std::{num::{NonZeroUsize, NonZeroU32}, time::Duration};

use collections::HashMap;
use serde::Deserialize;
pub use settings::{
    ActionName, AutosaveSetting, InactiveOpacity,
    PaneSplitDirectionHorizontal, PaneSplitDirectionVertical, RegisterSetting,
    Settings,
};

#[derive(RegisterSetting)]
pub struct WorkspaceSettings {
    pub active_pane_modifiers: ActivePanelModifiers,
    pub pane_split_direction_horizontal: settings::PaneSplitDirectionHorizontal,
    pub pane_split_direction_vertical: settings::PaneSplitDirectionVertical,
    pub centered_layout: settings::CenteredLayoutSettings,
    pub confirm_quit: bool,
    pub autosave: AutosaveSetting,
    pub drop_target_size: f32,
    pub use_system_path_prompts: bool,
    pub use_system_prompts: bool,
    pub command_aliases: HashMap<String, ActionName>,
    pub max_tabs: Option<NonZeroUsize>,
    pub when_closing_with_no_tabs: settings::CloseWindowWhenNoItems,
    pub on_last_window_closed: settings::OnLastWindowClosed,
    pub text_rendering_mode: settings::TextRenderingMode,
    pub close_on_file_delete: bool,
    pub use_system_window_tabs: bool,
    pub window_decorations: settings::WindowDecorations,
    pub focus_follows_mouse: FocusFollowsMouse,
    pub default_tab_size: NonZeroU32,
    pub default_hard_tabs: bool,
}

#[derive(Copy, Clone, Deserialize)]
pub struct FocusFollowsMouse {
    pub enabled: bool,
    pub debounce: Duration,
}

#[derive(Copy, Clone, PartialEq, Debug, Default)]
pub struct ActivePanelModifiers {
    /// Size of the border surrounding the active pane.
    /// When set to 0, the active pane doesn't have any border.
    /// The border is drawn inset.
    ///
    /// Default: `0.0`
    // TODO: make this not an option, it is never None
    pub border_size: Option<f32>,
    /// Opacity of inactive panels.
    /// When set to 1.0, the inactive panes have the same opacity as the active one.
    /// If set to 0, the inactive panes content will not be visible at all.
    /// Values are clamped to the [0.0, 1.0] range.
    ///
    /// Default: `1.0`
    // TODO: make this not an option, it is never None
    pub inactive_opacity: Option<InactiveOpacity>,
}

#[derive(Deserialize, RegisterSetting)]
pub struct TabBarSettings {
    pub show: bool,
    pub show_nav_history_buttons: bool,
    pub show_tab_bar_buttons: bool,
    pub show_tab_bar_stacked: bool,
}

impl Settings for WorkspaceSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let workspace = &content.workspace;
        Self {
            active_pane_modifiers: ActivePanelModifiers {
                border_size: Some(
                    workspace
                        .active_pane_modifiers
                        .unwrap()
                        .border_size
                        .unwrap(),
                ),
                inactive_opacity: Some(
                    workspace
                        .active_pane_modifiers
                        .unwrap()
                        .inactive_opacity
                        .unwrap(),
                ),
            },
            pane_split_direction_horizontal: workspace.pane_split_direction_horizontal.unwrap(),
            pane_split_direction_vertical: workspace.pane_split_direction_vertical.unwrap(),
            centered_layout: workspace.centered_layout.unwrap(),
            confirm_quit: workspace.confirm_quit.unwrap(),
            autosave: workspace.autosave.unwrap(),
            drop_target_size: workspace.drop_target_size.unwrap(),
            use_system_path_prompts: workspace.use_system_path_prompts.unwrap(),
            use_system_prompts: workspace.use_system_prompts.unwrap(),
            command_aliases: workspace.command_aliases.clone(),
            max_tabs: workspace.max_tabs,
            when_closing_with_no_tabs: workspace.when_closing_with_no_tabs.unwrap(),
            on_last_window_closed: workspace.on_last_window_closed.unwrap(),
            text_rendering_mode: workspace.text_rendering_mode.unwrap(),
            close_on_file_delete: workspace.close_on_file_delete.unwrap(),
            use_system_window_tabs: workspace.use_system_window_tabs.unwrap(),
            window_decorations: workspace.window_decorations.unwrap(),
            focus_follows_mouse: FocusFollowsMouse {
                enabled: workspace
                    .focus_follows_mouse
                    .unwrap()
                    .enabled
                    .unwrap_or(false),
                debounce: Duration::from_millis(
                    workspace
                        .focus_follows_mouse
                        .unwrap()
                        .debounce_ms
                        .unwrap_or(250),
                ),
            },
            default_tab_size: workspace.default_tab_size.unwrap(),
            default_hard_tabs: workspace.default_hard_tabs.unwrap(),
        }
    }
}

impl Settings for TabBarSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let tab_bar = content.tab_bar.clone().unwrap();
        TabBarSettings {
            show: tab_bar.show.unwrap(),
            show_nav_history_buttons: tab_bar.show_nav_history_buttons.unwrap(),
            show_tab_bar_buttons: tab_bar.show_tab_bar_buttons.unwrap(),
            show_tab_bar_stacked: tab_bar.show_tab_bar_stacked.unwrap(),
        }
    }
}
