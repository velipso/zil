use std::fmt::Display;
use std::num;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

use crate::{DelayMs, serialize_f32_with_two_decimal_places};

#[with_fallible_options]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct EditorSettingsContent {
    /// Whether the cursor blinks in the editor.
    ///
    /// Default: true
    pub cursor_blink: Option<bool>,
    /// Cursor shape for the default editor.
    /// Can be "bar", "block", "underline", or "hollow".
    ///
    /// Default: bar
    pub cursor_shape: Option<CursorShape>,
    /// How to highlight the current line in the editor.
    ///
    /// Default: all
    pub current_line_highlight: Option<CurrentLineHighlight>,
    /// Whether to highlight all occurrences of the selected text in an editor.
    ///
    /// Default: true
    pub selection_highlight: Option<bool>,
    /// Whether the text selection should have rounded corners.
    ///
    /// Default: true
    pub rounded_selection: Option<bool>,
    /// The debounce delay before querying highlights from the language
    /// server based on the current cursor location.
    ///
    /// Default: 75
    pub lsp_highlight_debounce: Option<DelayMs>,
    /// Whether to show the informational hover box when moving the mouse
    /// over symbols in the editor.
    ///
    /// Default: true
    pub hover_popover_enabled: Option<bool>,
    /// Time to wait in milliseconds before showing the informational hover box.
    /// This delay also applies to auto signature help when `auto_signature_help` is enabled.
    ///
    /// Default: 300
    pub hover_popover_delay: Option<DelayMs>,
    /// Whether the hover popover sticks when the mouse moves toward it,
    /// allowing interaction with its contents before it disappears.
    ///
    /// Default: true
    pub hover_popover_sticky: Option<bool>,
    /// Time to wait in milliseconds before hiding the hover popover
    /// after the mouse moves away from the hover target.
    /// Only applies when `hover_popover_sticky` is enabled.
    ///
    /// Default: 300
    pub hover_popover_hiding_delay: Option<DelayMs>,
    /// Toolbar related settings
    pub toolbar: Option<ToolbarContent>,
    /// Scrollbar related settings
    pub scrollbar: Option<ScrollbarContent>,
    /// Minimap related settings
    pub minimap: Option<MinimapContent>,
    /// Gutter related settings
    pub gutter: Option<GutterContent>,
    /// Soft wrap related settings
    pub soft_wrap: Option<bool>,
    /// Character counts to show rulers in the editor.
    ///
    /// Default: []
    pub rulers: Option<Vec<usize>>,
    /// Indent guide related settings.
    pub indent_guides: Option<IndentGuidesContent>,
    /// Whether the editor will scroll beyond the last line.
    ///
    /// Default: one_page
    pub scroll_beyond_last_line: Option<ScrollBeyondLastLine>,
    /// The number of lines to keep above/below the cursor when auto-scrolling.
    ///
    /// Default: 3.
    #[serde(serialize_with = "crate::serialize_optional_f32_with_two_decimal_places")]
    pub vertical_scroll_margin: Option<f32>,
    /// Whether to scroll when clicking near the edge of the visible text area.
    ///
    /// Default: false
    pub autoscroll_on_clicks: Option<bool>,
    /// The number of characters to keep on either side when scrolling with the mouse.
    ///
    /// Default: 5.
    #[serde(serialize_with = "crate::serialize_optional_f32_with_two_decimal_places")]
    pub horizontal_scroll_margin: Option<f32>,
    /// Scroll sensitivity multiplier. This multiplier is applied
    /// to both the horizontal and vertical delta values while scrolling.
    ///
    /// Default: 1.0
    #[serde(serialize_with = "crate::serialize_optional_f32_with_two_decimal_places")]
    pub scroll_sensitivity: Option<f32>,
    /// Whether to zoom the editor font size with the mouse wheel
    /// while holding the primary modifier key (Cmd on macOS, Ctrl on other platforms).
    ///
    /// Default: false
    pub mouse_wheel_zoom: Option<bool>,
    /// Scroll sensitivity multiplier for fast scrolling. This multiplier is applied
    /// to both the horizontal and vertical delta values while scrolling. Fast scrolling
    /// happens when a user holds the alt or option key while scrolling.
    ///
    /// Default: 4.0
    #[serde(serialize_with = "crate::serialize_optional_f32_with_two_decimal_places")]
    pub fast_scroll_sensitivity: Option<f32>,
    /// Settings for sticking scopes to the top of the editor.
    ///
    /// Default: sticky scroll is disabled
    pub sticky_scroll: Option<StickyScrollContent>,
    /// Whether the line numbers on editors gutter are relative or not.
    /// When "enabled" shows relative number of buffer lines, when "wrapped" shows
    /// relative number of display lines.
    ///
    /// Default: "disabled"
    pub relative_line_numbers: Option<RelativeLineNumbers>,
    /// When to populate a new search's query based on the text under the cursor.
    ///
    /// Default: always
    pub seed_search_query_from_cursor: Option<SeedQuerySetting>,
    pub use_smartcase_search: Option<bool>,
    /// Hide the values of variables in `private` files, as defined by the
    /// private_files setting. This only changes the visual representation,
    /// the values are still present in the file and can be selected / copied / pasted
    ///
    /// Default: false
    pub redact_private_values: Option<bool>,

    /// Whether to enable middle-click paste on Linux
    ///
    /// Default: true
    pub middle_click_paste: Option<bool>,

    /// Whether the editor search results will loop
    ///
    /// Default: true
    pub search_wrap: Option<bool>,

    /// Defaults to use when opening a new buffer and project search items.
    ///
    /// Default: nothing is enabled
    pub search: Option<SearchSettingsContent>,

    /// The minimum APCA perceptual contrast to maintain when
    /// rendering text over highlight backgrounds in the editor.
    ///
    /// Values range from 0 to 106. Set to 0 to disable adjustments.
    /// Default: 45
    #[schemars(range(min = 0, max = 106))]
    pub minimum_contrast_for_highlights: Option<MinimumContrast>,

    /// Drag and drop related settings
    pub drag_and_drop_selection: Option<DragAndDropSelectionContent>,

    /// How to render LSP `textDocument/documentColor` colors in the editor.
    ///
    /// Default: [`DocumentColorsRenderMode::Inlay`]
    pub lsp_document_colors: Option<DocumentColorsRenderMode>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    PartialEq,
    Eq,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum RelativeLineNumbers {
    Disabled,
    Enabled,
    Wrapped,
}

#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    PartialEq,
    Eq,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum CompletionDetailAlignment {
    #[default]
    Left,
    Right,
}

impl RelativeLineNumbers {
    pub fn enabled(&self) -> bool {
        match self {
            RelativeLineNumbers::Enabled | RelativeLineNumbers::Wrapped => true,
            RelativeLineNumbers::Disabled => false,
        }
    }
    pub fn wrapped(&self) -> bool {
        match self {
            RelativeLineNumbers::Enabled | RelativeLineNumbers::Disabled => false,
            RelativeLineNumbers::Wrapped => true,
        }
    }
}

// Toolbar related settings
#[with_fallible_options]
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Eq)]
pub struct ToolbarContent {
    /// Whether to display breadcrumbs in the editor toolbar.
    ///
    /// Default: true
    pub breadcrumbs: Option<bool>,
    /// Whether to display quick action buttons in the editor toolbar.
    ///
    /// Default: true
    pub quick_actions: Option<bool>,
    /// Whether to show the selections menu in the editor toolbar.
    ///
    /// Default: true
    pub selections_menu: Option<bool>,
    /// Whether to display Agent review buttons in the editor toolbar.
    /// Only applicable while reviewing a file edited by the Agent.
    ///
    /// Default: true
    pub agent_review: Option<bool>,
    /// Whether to display code action buttons in the editor toolbar.
    ///
    /// Default: false
    pub code_actions: Option<bool>,
}

/// Scrollbar related settings
#[with_fallible_options]
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Default)]
pub struct ScrollbarContent {
    /// Whether to show the horizontal scrollbar in the editor.
    ///
    /// Default: true
    pub show_horizontal: Option<bool>,
    /// Whether to show the horizontal scrollbar in the editor.
    ///
    /// Default: true
    pub show_vertical: Option<bool>,    
    /// Whether to show buffer search result indicators in the scrollbar.
    ///
    /// Default: true
    pub search_results: Option<bool>,
    /// Whether to show selected text occurrences in the scrollbar.
    ///
    /// Default: true
    pub selected_text: Option<bool>,
    /// Whether to show selected symbol occurrences in the scrollbar.
    ///
    /// Default: true
    pub selected_symbol: Option<bool>,
    /// Whether to show cursor positions in the scrollbar.
    ///
    /// Default: true
    pub cursors: Option<bool>,
}

/// Sticky scroll related settings
#[with_fallible_options]
#[derive(Clone, Default, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq)]
pub struct StickyScrollContent {
    /// Whether sticky scroll is enabled.
    ///
    /// Default: false
    pub enabled: Option<bool>,
}

/// Minimap related settings
#[with_fallible_options]
#[derive(Clone, Default, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq)]
pub struct MinimapContent {
    /// When to show the minimap in the editor.
    ///
    /// Default: false
    pub show: Option<bool>,

    /// Maximum number of columns to display in the minimap.
    ///
    /// Default: 80
    pub max_width_columns: Option<num::NonZeroU32>,
}

/// Gutter related settings
#[with_fallible_options]
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Eq)]
pub struct GutterContent {
    /// Whether to show line numbers in the gutter.
    ///
    /// Default: true
    pub line_numbers: Option<bool>,
    /// Minimum number of characters to reserve space for in the gutter.
    ///
    /// Default: 4
    pub min_line_number_digits: Option<usize>,
    /// Whether to show fold buttons in the gutter.
    ///
    /// Default: true
    pub folds: Option<bool>,
}

/// The settings for indent guides.
#[with_fallible_options]
#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct IndentGuidesContent {
    /// Whether to display indent guides in the editor.
    ///
    /// Default: true
    pub enabled: Option<bool>,
    /// The width of the indent guides in pixels, between 1 and 10.
    ///
    /// Default: 1
    pub line_width: Option<u32>,
    /// The width of the active indent guide in pixels, between 1 and 10.
    ///
    /// Default: 1
    pub active_line_width: Option<u32>,
    /// Determines how indent guides are colored.
    ///
    /// Default: Fixed
    pub coloring: Option<IndentGuideColoring>,
    /// Determines how indent guide backgrounds are colored.
    ///
    /// Default: Disabled
    pub background_coloring: Option<IndentGuideBackgroundColoring>,
}

/// Determines how indent guides are colored.
#[derive(
    Default,
    Debug,
    Copy,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum IndentGuideColoring {
    /// Do not render any lines for indent guides.
    Disabled,
    /// Use the same color for all indentation levels.
    #[default]
    Fixed,
    /// Use a different color for each indentation level.
    IndentAware,
}

/// Determines how indent guide backgrounds are colored.
#[derive(
    Default,
    Debug,
    Copy,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum IndentGuideBackgroundColoring {
    /// Do not render any background for indent guides.
    #[default]
    Disabled,
    /// Use a different color for each indentation level.
    IndentAware,
}

/// How to render LSP `textDocument/documentColor` colors in the editor.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    PartialEq,
    Eq,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum DocumentColorsRenderMode {
    /// Do not query and render document colors.
    None,
    /// Render document colors as inlay hints near the color text.
    #[default]
    Inlay,
    /// Draw a border around the color text.
    Border,
    /// Draw a background behind the color text.
    Background,
}

#[derive(
    Copy,
    Clone,
    Debug,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum CurrentLineHighlight {
    // Don't highlight the current line.
    None,
    // Highlight the gutter area.
    Gutter,
    // Highlight the editor area.
    Line,
    // Highlight the full line.
    All,
}

/// When to populate a new search's query based on the text under the cursor.
#[derive(
    Copy,
    Clone,
    Debug,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum SeedQuerySetting {
    /// Always populate the search query with the word under the cursor.
    Always,
    /// Only populate the search query when there is text selected.
    Selection,
    /// Never populate the search query
    Never,
}

/// Whether the editor will scroll beyond the last line.
///
/// Default: one_page
#[derive(
    Copy,
    Clone,
    Debug,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    PartialEq,
    Eq,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum ScrollBeyondLastLine {
    /// The editor will not scroll beyond the last line.
    Off,

    /// The editor will scroll beyond the last line by one page.
    OnePage,

    /// The editor will scroll beyond the last line by the same number of lines as vertical_scroll_margin.
    VerticalScrollMargin,
}

/// The shape of a selection cursor.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    /// A vertical bar
    #[default]
    Bar,
    /// A block that surrounds the following character
    Block,
    /// An underline that runs along the following character
    Underline,
    /// A box drawn around the following character
    Hollow,
}

/// Default options for buffer and project search items.
#[with_fallible_options]
#[derive(Clone, Default, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Eq)]
pub struct SearchSettingsContent {
    /// Whether to show the project search button in the status bar.
    pub button: Option<bool>,
    /// Whether to only match on whole words.
    pub whole_word: Option<bool>,
    /// Whether to match case sensitively.
    pub case_sensitive: Option<bool>,
    /// Whether to include gitignored files in search results.
    pub include_ignored: Option<bool>,
    /// Whether to interpret the search query as a regular expression.
    pub regex: Option<bool>,
    /// Whether to center the cursor on each search match when navigating.
    pub center_on_match: Option<bool>,
}

/// Whether to allow drag and drop text selection in buffer.
#[with_fallible_options]
#[derive(Clone, Default, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Eq)]
pub struct DragAndDropSelectionContent {
    /// When true, enables drag and drop text selection in buffer.
    ///
    /// Default: true
    pub enabled: Option<bool>,

    /// The delay in milliseconds that must elapse before drag and drop is allowed. Otherwise, a new text selection is created.
    ///
    /// Default: 300
    pub delay: Option<DelayMs>,
}

/// Minimum APCA perceptual contrast for text over highlight backgrounds.
///
/// Valid range: 0.0 to 106.0
/// Default: 45.0
#[derive(
    Clone,
    Copy,
    Debug,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    PartialEq,
    PartialOrd,
    derive_more::FromStr,
)]
#[serde(transparent)]
pub struct MinimumContrast(
    #[serde(serialize_with = "crate::serialize_f32_with_two_decimal_places")] pub f32,
);

impl Display for MinimumContrast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.1}", self.0)
    }
}

impl From<f32> for MinimumContrast {
    fn from(x: f32) -> Self {
        Self(x)
    }
}

/// Opacity of the inactive panes. 0 means transparent, 1 means opaque.
///
/// Valid range: 0.0 to 1.0
/// Default: 1.0
#[derive(
    Clone,
    Copy,
    Debug,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    PartialEq,
    PartialOrd,
    derive_more::FromStr,
)]
#[serde(transparent)]
pub struct InactiveOpacity(
    #[serde(serialize_with = "serialize_f32_with_two_decimal_places")] pub f32,
);

impl Display for InactiveOpacity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.1}", self.0)
    }
}

impl From<f32> for InactiveOpacity {
    fn from(x: f32) -> Self {
        Self(x)
    }
}

/// Centered layout related setting (left/right).
///
/// Valid range: 0.0 to 0.4
/// Default: 2.0
#[derive(
    Clone,
    Copy,
    Debug,
    Serialize,
    Deserialize,
    MergeFrom,
    PartialEq,
    PartialOrd,
    derive_more::FromStr,
)]
#[serde(transparent)]
pub struct CenteredPaddingSettings(
    #[serde(serialize_with = "serialize_f32_with_two_decimal_places")] pub f32,
);

impl CenteredPaddingSettings {
    pub const MIN_PADDING: f32 = 0.0;
    // This is an f64 so serde_json can give a type hint without random numbers in the back
    pub const DEFAULT_PADDING: f64 = 0.2;
    pub const MAX_PADDING: f32 = 0.4;
}

impl Display for CenteredPaddingSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.2}", self.0)
    }
}

impl From<f32> for CenteredPaddingSettings {
    fn from(x: f32) -> Self {
        Self(x)
    }
}

impl Default for CenteredPaddingSettings {
    fn default() -> Self {
        Self(Self::DEFAULT_PADDING as f32)
    }
}

impl schemars::JsonSchema for CenteredPaddingSettings {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CenteredPaddingSettings".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        use schemars::json_schema;
        json_schema!({
            "type": "number",
            "minimum": Self::MIN_PADDING,
            "maximum": Self::MAX_PADDING,
            "default": Self::DEFAULT_PADDING,
            "description": "Centered layout related setting (left/right)."
        })
    }
}
