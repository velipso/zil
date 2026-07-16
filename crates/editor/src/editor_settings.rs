use core::num;
use language::CursorShape;
pub use settings::{
    CurrentLineHighlight, DelayMs,
    DocumentColorsRenderMode,
    ScrollBeyondLastLine, SeedQuerySetting,
};
use settings::{RegisterSetting, RelativeLineNumbers, Settings};

/// Imports from the VSCode settings at
/// https://code.visualstudio.com/docs/reference/default-settings
#[derive(Clone, RegisterSetting)]
pub struct EditorSettings {
    pub cursor_blink: bool,
    pub cursor_shape: Option<CursorShape>,
    pub current_line_highlight: CurrentLineHighlight,
    pub selection_highlight: bool,
    pub rounded_selection: bool,
    pub lsp_highlight_debounce: DelayMs,
    pub toolbar: Toolbar,
    pub scrollbar: Scrollbar,
    pub minimap: Minimap,
    pub gutter: Gutter,
    pub soft_wrap: bool,
    pub rulers: Vec<usize>,
    pub indent_guides: IndentGuides,
    pub scroll_beyond_last_line: ScrollBeyondLastLine,
    pub vertical_scroll_margin: f64,
    pub autoscroll_on_clicks: bool,
    pub horizontal_scroll_margin: f32,
    pub scroll_sensitivity: f32,
    pub mouse_wheel_zoom: bool,
    pub fast_scroll_sensitivity: f32,
    pub sticky_scroll: StickyScroll,
    pub relative_line_numbers: RelativeLineNumbers,
    pub seed_search_query_from_cursor: SeedQuerySetting,
    pub use_smartcase_search: bool,
    pub middle_click_paste: bool,
    pub search_wrap: bool,
    pub search: SearchSettings,
    pub drag_and_drop_selection: DragAndDropSelection,
    pub lsp_document_colors: DocumentColorsRenderMode,
    pub minimum_contrast_for_highlights: f32,
    pub trim_whitespace_on_save: bool,
    pub ensure_eof_newline_on_save: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StickyScroll {
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolbar {
    pub breadcrumbs: bool,
    pub quick_actions: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Scrollbar {
    pub show_horizontal: bool,
    pub show_vertical: bool,
    pub selected_text: bool,
    pub selected_symbol: bool,
    pub search_results: bool,
    pub cursors: bool,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Minimap {
    pub show: bool,
    pub max_width_columns: num::NonZeroU32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Gutter {
    pub min_line_number_digits: usize,
    pub line_numbers: bool,
    pub folds: bool,
}

/// The settings for indent guides.
#[derive(Debug, Clone, PartialEq)]
pub struct IndentGuides {
    /// Whether to display indent guides in the editor.
    ///
    /// Default: true
    pub enabled: bool,
    /// The width of the indent guides in pixels, between 1 and 10.
    ///
    /// Default: 1
    pub line_width: u32,
    /// The width of the active indent guide in pixels, between 1 and 10.
    ///
    /// Default: 1
    pub active_line_width: u32,
    /// Determines how indent guides are colored.
    ///
    /// Default: Fixed
    pub coloring: settings::IndentGuideColoring,
    /// Determines how indent guide backgrounds are colored.
    ///
    /// Default: Disabled
    pub background_coloring: settings::IndentGuideBackgroundColoring,
}

impl IndentGuides {
    /// Returns the clamped line width in pixels for an indent guide based on
    /// whether it is active, or `None` when line coloring is disabled.
    pub fn visible_line_width(&self, active: bool) -> Option<u32> {
        if self.coloring == settings::IndentGuideColoring::Disabled {
            return None;
        }
        let width = if active {
            self.active_line_width
        } else {
            self.line_width
        };
        Some(width.clamp(1, 10))
    }
}

/// Forcefully enable or disable the scrollbar for each axis
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ScrollbarAxes {
    /// When false, forcefully disables the horizontal scrollbar. Otherwise, obey other settings.
    ///
    /// Default: true
    pub horizontal: bool,

    /// When false, forcefully disables the vertical scrollbar. Otherwise, obey other settings.
    ///
    /// Default: true
    pub vertical: bool,
}

/// Whether to allow drag and drop text selection in buffer.
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct DragAndDropSelection {
    /// When true, enables drag and drop text selection in buffer.
    ///
    /// Default: true
    pub enabled: bool,

    /// The delay in milliseconds that must elapse before drag and drop is allowed. Otherwise, a new text selection is created.
    ///
    /// Default: 300
    pub delay: DelayMs,
}

/// Default options for buffer and project search items.
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct SearchSettings {
    /// Whether to only match on whole words.
    pub whole_word: bool,
    /// Whether to match case sensitively.
    pub case_sensitive: bool,
    /// Whether to include gitignored files in search results.
    pub include_ignored: bool,
    /// Whether to interpret the search query as a regular expression.
    pub regex: bool,
    /// Whether to center the cursor on each search match when navigating.
    pub center_on_match: bool,
}

impl Settings for EditorSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let editor = content.editor.clone();
        let scrollbar = editor.scrollbar.unwrap();
        let minimap = editor.minimap.unwrap();
        let gutter = editor.gutter.unwrap();
        let indent_guides = editor.indent_guides.unwrap();
        let toolbar = editor.toolbar.unwrap();
        let search = editor.search.unwrap();
        let drag_and_drop_selection = editor.drag_and_drop_selection.unwrap();
        let sticky_scroll = editor.sticky_scroll.unwrap();
        Self {
            cursor_blink: editor.cursor_blink.unwrap(),
            cursor_shape: editor.cursor_shape.map(Into::into),
            current_line_highlight: editor.current_line_highlight.unwrap(),
            selection_highlight: editor.selection_highlight.unwrap(),
            rounded_selection: editor.rounded_selection.unwrap(),
            lsp_highlight_debounce: editor.lsp_highlight_debounce.unwrap(),
            toolbar: Toolbar {
                breadcrumbs: toolbar.breadcrumbs.unwrap(),
                quick_actions: toolbar.quick_actions.unwrap(),
            },
            scrollbar: Scrollbar {
                show_horizontal: scrollbar.show_horizontal.unwrap(),
                show_vertical: scrollbar.show_vertical.unwrap(),
                selected_text: scrollbar.selected_text.unwrap(),
                selected_symbol: scrollbar.selected_symbol.unwrap(),
                search_results: scrollbar.search_results.unwrap(),
                cursors: scrollbar.cursors.unwrap(),
            },
            minimap: Minimap {
                show: minimap.show.unwrap(),
                max_width_columns: minimap.max_width_columns.unwrap(),
            },
            gutter: Gutter {
                min_line_number_digits: gutter.min_line_number_digits.unwrap(),
                line_numbers: gutter.line_numbers.unwrap(),
                folds: gutter.folds.unwrap(),
            },
            soft_wrap: editor.soft_wrap.unwrap(),
            rulers: editor.rulers.unwrap(),
            indent_guides: IndentGuides {
                enabled: indent_guides.enabled.unwrap(),
                line_width: indent_guides.line_width.unwrap(),
                active_line_width: indent_guides.active_line_width.unwrap(),
                coloring: indent_guides.coloring.unwrap(),
                background_coloring: indent_guides.background_coloring.unwrap(),
            },
            scroll_beyond_last_line: editor.scroll_beyond_last_line.unwrap(),
            vertical_scroll_margin: editor.vertical_scroll_margin.unwrap() as f64,
            autoscroll_on_clicks: editor.autoscroll_on_clicks.unwrap(),
            horizontal_scroll_margin: editor.horizontal_scroll_margin.unwrap(),
            scroll_sensitivity: editor.scroll_sensitivity.unwrap(),
            mouse_wheel_zoom: editor.mouse_wheel_zoom.unwrap(),
            fast_scroll_sensitivity: editor.fast_scroll_sensitivity.unwrap(),
            sticky_scroll: StickyScroll {
                enabled: sticky_scroll.enabled.unwrap(),
            },
            relative_line_numbers: editor.relative_line_numbers.unwrap(),
            seed_search_query_from_cursor: editor.seed_search_query_from_cursor.unwrap(),
            use_smartcase_search: editor.use_smartcase_search.unwrap(),
            middle_click_paste: editor.middle_click_paste.unwrap(),
            search_wrap: editor.search_wrap.unwrap(),
            search: SearchSettings {
                whole_word: search.whole_word.unwrap(),
                case_sensitive: search.case_sensitive.unwrap(),
                include_ignored: search.include_ignored.unwrap(),
                regex: search.regex.unwrap(),
                center_on_match: search.center_on_match.unwrap(),
            },
            drag_and_drop_selection: DragAndDropSelection {
                enabled: drag_and_drop_selection.enabled.unwrap(),
                delay: drag_and_drop_selection.delay.unwrap(),
            },
            lsp_document_colors: editor.lsp_document_colors.unwrap(),
            minimum_contrast_for_highlights: editor.minimum_contrast_for_highlights.unwrap().0,
            trim_whitespace_on_save: editor.trim_whitespace_on_save.unwrap(),
            ensure_eof_newline_on_save: editor.ensure_eof_newline_on_save.unwrap(),
        }
    }
}
