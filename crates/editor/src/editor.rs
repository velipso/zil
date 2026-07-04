#![allow(rustdoc::private_intra_doc_links)]
//! This is the place where everything editor-related is stored (data-wise) and displayed (ui-wise).
//! The main point of interest in this crate is [`Editor`] type, which is used in every other Zed part as a user input element.
//! It comes in different flavors: single line, multiline and a fixed height one.
//!
//! Editor contains of multiple large submodules:
//! * [`element`] — the place where all rendering happens
//! * [`display_map`] - chunks up text in the editor into the logical blocks, establishes coordinates and mapping between each of them.
//!   Contains all metadata related to text transformations (folds, fake inlay text insertions, soft wraps, tab markup, etc.).
//!
//! All other submodules and structs are mostly concerned with holding editor data about the way it displays current buffer region(s).
//!
//! If you're looking to improve Vim mode, you should check out Vim crate that wraps Editor and overrides its behavior.
pub mod actions;
pub mod blink_manager;
pub mod display_map;
mod document_colors;
mod document_symbols;
mod editor_settings;
mod element;
mod fold;
mod folding_ranges;
mod highlight_matching_bracket;
pub mod hover_links;
mod indent_guides;
mod inlays;
pub mod items;
mod mouse_context_menu;
pub mod movement;
pub mod scroll;
mod selections_collection;
pub mod semantic_tokens;

#[cfg(test)]
mod editor_block_comment_tests;
#[cfg(test)]
mod editor_tests;
#[cfg(any(test, feature = "test-support"))]
pub mod test;

mod clipboard;
mod config;
mod input;
mod navigation;
mod selection;

pub(crate) use actions::*;
pub use clipboard::ClipboardSelection;
pub use display_map::{
    ChunkRenderer, ChunkRendererContext, DisplayPoint, FoldPlaceholder, HighlightKey,
    NavigationOverlayKey, SemanticTokenHighlight,
};
pub use editor_settings::{
    CurrentLineHighlight,
    DocumentColorsRenderMode, EditorSettings, ScrollBeyondLastLine,
    ScrollbarAxes, SearchSettings,
};
pub use element::{
    CursorLayout, EditorElement, HighlightedRange, HighlightedRangeLine, PointForPosition,
    render_breadcrumb_text,
};
pub use inlays::Inlay;
pub use items::MAX_TAB_TITLE_LEN;
pub use multi_buffer::{
    Anchor, AnchorRangeExt, BufferOffset, ExcerptRange, MBTextSummary, MultiBuffer,
    MultiBufferOffset, MultiBufferOffsetUtf16, MultiBufferSnapshot, PathKey, RowInfo, ToOffset,
    ToPoint,
};
pub use text::Bias;

use ::git::status::FileStatus;
use aho_corasick::{AhoCorasick, AhoCorasickBuilder, BuildError};
use anyhow::{Context as _, Result};
use blink_manager::BlinkManager;
use client::parse_zed_link;
use clock::ReplicaId;
use collections::{BTreeMap, HashMap, HashSet, VecDeque};
use convert_case::{Case, Casing};
use display_map::*;
use document_colors::LspColorData;
use element::{LineWithInvisibles, PositionMap};
use futures::{
    FutureExt,
    future::Shared,
};
use gpui::{
    Action, AnyElement, App, AppContext, AsyncWindowContext, Background, Bounds,
    ClipboardEntry, ClipboardItem, Context, DispatchPhase, Entity, EntityId, EntityInputHandler,
    EventEmitter, FocusHandle, FocusOutEvent, Focusable, FontId, FontStyle,
    HighlightStyle, Hsla, KeyContext, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    PaintQuad, ParentElement, Pixels, PressureStage, Render, SharedString,
    Size, Styled, Subscription, Task, TextRun, TextStyle, TextStyleRefinement, UTF16Selection,
    UnderlineStyle, WeakEntity, WeakFocusHandle, Window, div, point,
    prelude::*, px, relative, size,
};
use hover_links::{HoverLink, HoveredLinkState, find_file};
use indent_guides::ActiveIndentGuidesState;
use inlays::{InlaySplice};
use itertools::{Either, Itertools};
use language::{
    AutoindentMode, BlockCommentConfig, Buffer, BufferRow,
    BufferSnapshot, Capability, CharKind, CodeLabel, CursorShape,
    HighlightedText, IndentKind, IndentSize, Language,
    LanguageAwareStyling, LanguageName, LanguageScope, OffsetRangeExt,
    OutlineItem, Point, Selection, SelectionGoal, TextObject, TransactionId, TreeSitterOptions,
    language_settings::LanguageSettings,
};
use mouse_context_menu::MouseContextMenu;
use movement::TextLayoutDetails;
use multi_buffer::{ExcerptBoundaryInfo, MultiBufferPoint, MultiBufferRow};
use project::{
    DocumentHighlight,
    PrepareRenameResponse, Project, ProjectItem, ProjectPath,
    ProjectTransaction,
    lsp_store::{
        BufferSemanticTokens,
        OpenLspBufferHandle, RefreshForServer,
    },
    project_settings::{ProjectSettings},
};
use rand::seq::SliceRandom;
use regex::Regex;
use rpc::{ErrorCode, ErrorExt};
use scroll::{Autoscroll, OngoingScroll, ScrollAnchor, ScrollManager, SharedScrollAnchor};
use selections_collection::{MutableSelectionsCollection, SelectionsCollection};
use serde::{Deserialize, Serialize};
use settings::{
    GitGutterSetting, RelativeLineNumbers, Settings, SettingsLocation, SettingsStore,
    update_settings_file,
};
use smallvec::SmallVec;
use std::{
    any::{Any, TypeId},
    borrow::Cow,
    cell::RefCell,
    cmp::{self, Ordering},
    collections::hash_map,
    iter::Peekable,
    mem,
    num::NonZeroU32,
    ops::{Deref, Not, Range, RangeInclusive},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use text::{BufferId, OffsetUtf16, Rope, ToPoint as _};
use theme::{
    ActiveTheme, GlobalTheme, PlayerColor, StatusColors, SyntaxTheme, Theme,
};
use theme_settings::{ThemeSettings, observe_buffer_font_size_adjustment};
use ui::{ContextMenu, Disclosure, prelude::*};
use ui_input::ErasedEditor;
use util::{RangeExt, ResultExt, maybe, post_inc};
use workspace::{
    CollaboratorId, Item as WorkspaceItem, ItemNavHistory,
    RestoreOnStartupBehavior, SplitDirection,
    TabBarSettings, ViewId, Workspace, WorkspaceId, WorkspaceSettings,
    item::{ItemBufferKind, ItemHandle},
    notifications::{DetachAndPromptErr, NotifyTaskExt},
    searchable::SearchEvent,
};
pub use zed_actions::editor::RevealInFileManager;
use zed_actions::editor::{MoveDown, MoveUp};

use crate::{
    hover_links::{find_url, find_url_from_range},
    scroll::ScrollOffset,
    selections_collection::resolve_selections_wrapping_blocks,
    semantic_tokens::SemanticTokenState,
};

pub const FILE_HEADER_HEIGHT: u32 = 2;
pub const BUFFER_HEADER_PADDING: Rems = rems(0.25);
pub const MULTI_BUFFER_EXCERPT_HEADER_HEIGHT: u32 = 1;
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
const MAX_LINE_LEN: usize = 1024;
const MIN_NAVIGATION_HISTORY_ROW_DELTA: i64 = 10;
const MAX_SELECTION_HISTORY_LEN: usize = 1024;
pub(crate) const CURSORS_VISIBLE_FOR: Duration = Duration::from_millis(2000);
pub const SELECTION_HIGHLIGHT_DEBOUNCE_TIMEOUT: Duration = Duration::from_millis(100);

pub(crate) const SCROLL_CENTER_TOP_BOTTOM_DEBOUNCE_TIMEOUT: Duration = Duration::from_secs(1);
pub const LSP_REQUEST_DEBOUNCE_TIMEOUT: Duration = Duration::from_millis(50);

pub(crate) const MINIMAP_FONT_SIZE: AbsoluteLength = AbsoluteLength::Pixels(px(2.));

pub enum ActiveDebugLine {}
pub enum DebugStackFrameLine {}

pub enum ConflictsOuter {}
pub enum ConflictsOurs {}
pub enum ConflictsTheirs {}
pub enum ConflictsOursMarker {}
pub enum ConflictsTheirsMarker {}

pub struct HunkAddedColor;
pub struct HunkRemovedColor;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Navigated {
    Yes,
    No,
}

impl Navigated {
    pub fn from_bool(yes: bool) -> Navigated {
        if yes { Navigated::Yes } else { Navigated::No }
    }
}

pub fn init(cx: &mut App) {
    cx.set_global(breadcrumbs::RenderBreadcrumbText(render_breadcrumb_text));

    workspace::register_project_item::<Editor>(cx);
    workspace::FollowableViewRegistry::register::<Editor>(cx);

    cx.observe_new(
        |workspace: &mut Workspace, _: Option<&mut Window>, _cx: &mut Context<Workspace>| {
            workspace.register_action(Editor::new_file);
            workspace.register_action(Editor::new_file_split);
            workspace.register_action(Editor::new_file_vertical);
            workspace.register_action(Editor::new_file_horizontal);
            workspace.register_action(Editor::cancel_language_server_work);
            workspace.register_action(Editor::toggle_focus);
        },
    )
    .detach();

    cx.on_action(move |_: &workspace::NewFile, cx| {
        let app_state = workspace::AppState::global(cx);
        workspace::open_new(
            Default::default(),
            app_state,
            cx,
            |workspace, window, cx| Editor::new_file(workspace, &Default::default(), window, cx),
        )
        .detach_and_log_err(cx);
    })
    .on_action(move |_: &workspace::NewWindow, cx| {
        let app_state = workspace::AppState::global(cx);
        workspace::open_new(
            Default::default(),
            app_state,
            cx,
            |workspace, window, cx| {
                cx.activate(true);
                Editor::new_file(workspace, &Default::default(), window, cx)
            },
        )
        .detach_and_log_err(cx);
    });
    _ = ui_input::ERASED_EDITOR_FACTORY.set(|window, cx| {
        Arc::new(ErasedEditorImpl(
            cx.new(|cx| Editor::single_line(window, cx)),
        )) as Arc<dyn ErasedEditor>
    });
}

pub struct SearchWithinRange;

#[derive(Clone, Debug, PartialEq)]
pub enum SelectPhase {
    Begin {
        position: DisplayPoint,
        display_point: DisplayPoint,
        add: bool,
        click_count: usize,
    },
    BeginColumnar {
        position: DisplayPoint,
        display_point: Option<DisplayPoint>,
        reset: bool,
        goal_column: u32,
    },
    Extend {
        position: DisplayPoint,
        click_count: usize,
    },
    Update {
        position: DisplayPoint,
        goal_column: u32,
        scroll_delta: gpui::Point<f32>,
    },
    End,
}

#[derive(Clone, Debug)]
pub enum SelectMode {
    Character,
    Word(Range<Anchor>),
    Line(Range<Anchor>),
    All,
}

#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub enum SizingBehavior {
    /// The editor will layout itself using `size_full` and will include the vertical
    /// scroll margin as requested by user settings.
    #[default]
    Default,
    /// The editor will layout itself using `size_full`, but will not have any
    /// vertical overscroll.
    ExcludeOverscrollMargin,
    /// The editor will request a vertical size according to its content and will be
    /// layouted without a vertical scroll margin.
    SizeByContent,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EditorMode {
    SingleLine,
    AutoHeight {
        min_lines: usize,
        max_lines: Option<usize>,
    },
    Full {
        /// When set to `true`, the editor will scale its UI elements with the buffer font size.
        scale_ui_elements_with_buffer_font_size: bool,
        /// When set to `true`, the editor will render a background for the active line.
        show_active_line_background: bool,
        /// Determines the sizing behavior for this editor
        sizing_behavior: SizingBehavior,
    },
    Minimap {
        parent: WeakEntity<Editor>,
    },
}

impl EditorMode {
    pub fn full() -> Self {
        Self::Full {
            scale_ui_elements_with_buffer_font_size: true,
            show_active_line_background: true,
            sizing_behavior: SizingBehavior::Default,
        }
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        matches!(self, Self::Full { .. })
    }

    #[inline]
    pub fn is_auto_height(&self) -> bool {
        matches!(self, Self::AutoHeight { .. })
    }

    #[inline]
    pub fn is_single_line(&self) -> bool {
        matches!(self, Self::SingleLine { .. })
    }

    #[inline]
    fn is_minimap(&self) -> bool {
        matches!(self, Self::Minimap { .. })
    }

    #[inline]
    fn could_have_scrollbars(&self) -> bool {
        self.is_full()
    }

    #[inline]
    fn could_have_minimap(&self) -> bool {
        self.is_full()
    }
}

#[derive(Clone)]
pub struct EditorStyle {
    pub background: Hsla,
    pub border: Hsla,
    pub local_player: PlayerColor,
    pub text: TextStyle,
    pub scrollbar_width: Pixels,
    pub syntax: Arc<SyntaxTheme>,
    pub status: StatusColors,
    pub unnecessary_code_fade: f32,
}

impl Default for EditorStyle {
    fn default() -> Self {
        Self {
            background: Hsla::default(),
            border: Hsla::default(),
            local_player: PlayerColor::default(),
            text: TextStyle::default(),
            scrollbar_width: Pixels::default(),
            syntax: Default::default(),
            // HACK: Status colors don't have a real default.
            // We should look into removing the status colors from the editor
            // style and retrieve them directly from the theme.
            status: StatusColors::dark(),
            unnecessary_code_fade: Default::default(),
        }
    }
}

pub struct ContextMenuOptions {
    pub min_entries_visible: usize,
    pub max_entries_visible: usize,
    pub placement: Option<ContextMenuPlacement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuPlacement {
    Above,
    Below,
}

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug, Default)]
struct EditorActionId(usize);

impl EditorActionId {
    pub fn post_inc(&mut self) -> Self {
        let answer = self.0;

        *self = Self(answer + 1);

        Self(answer)
    }
}

// type GetFieldEditorTheme = dyn Fn(&theme::Theme) -> theme::FieldEditor;
// type OverrideTextStyle = dyn Fn(&EditorStyle) -> Option<HighlightStyle>;

type BackgroundHighlight = (
    Arc<dyn Fn(&usize, &Theme) -> Hsla + Send + Sync>,
    Arc<[Range<Anchor>]>,
);
type GutterHighlight = (fn(&App) -> Hsla, Vec<Range<Anchor>>);

#[derive(Default)]
struct ScrollbarMarkerState {
    scrollbar_size: Size<Pixels>,
    dirty: bool,
    markers: Arc<[PaintQuad]>,
    pending_refresh: Option<Task<Result<()>>>,
}

impl ScrollbarMarkerState {
    fn should_refresh(&self, scrollbar_size: Size<Pixels>) -> bool {
        self.pending_refresh.is_none() && (self.scrollbar_size != scrollbar_size || self.dirty)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BreadcrumbsVisibility {
    setting_configuration: bool,
    toggle_override: bool,
}

impl BreadcrumbsVisibility {
    fn from_settings(cx: &App) -> Self {
        Self::new(EditorSettings::get_global(cx).toolbar.breadcrumbs)
    }

    fn new(setting_configuration: bool) -> Self {
        Self {
            setting_configuration,
            toggle_override: false,
        }
    }

    fn settings_visibility(&self) -> bool {
        self.setting_configuration
    }

    fn visible(&self) -> bool {
        self.setting_configuration ^ self.toggle_override
    }

    fn toggle_visibility(&self) -> Self {
        Self {
            setting_configuration: self.setting_configuration,
            toggle_override: !self.toggle_override,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferSerialization {
    All,
    NonDirtyBuffers,
}

impl BufferSerialization {
    fn new(restore_unsaved_buffers: bool) -> Self {
        if restore_unsaved_buffers {
            Self::All
        } else {
            Self::NonDirtyBuffers
        }
    }
}

/// Addons allow storing per-editor state in other crates (e.g. Vim)
pub trait Addon: 'static {
    fn extend_key_context(&self, _: &mut KeyContext, _: &App) {}

    fn render_buffer_header_controls(
        &self,
        _: &ExcerptBoundaryInfo,
        _: &language::BufferSnapshot,
        _: &Window,
        _: &App,
    ) -> Option<AnyElement> {
        None
    }

    fn extend_buffer_header_context_menu(
        &self,
        menu: ui::ContextMenu,
        _: &language::BufferSnapshot,
        _: &mut Window,
        _: &mut App,
    ) -> ui::ContextMenu {
        menu
    }

    fn override_status_for_buffer_id(&self, _: BufferId, _: &App) -> Option<FileStatus> {
        None
    }

    fn to_any(&self) -> &dyn std::any::Any;

    fn to_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }
}

struct ChangeLocation {
    current: Option<Vec<Anchor>>,
    original: Vec<Anchor>,
}
impl ChangeLocation {
    fn locations(&self) -> &[Anchor] {
        self.current.as_ref().unwrap_or(&self.original)
    }
}

/// A set of caret positions, registered when the editor was edited.
pub struct ChangeList {
    changes: Vec<ChangeLocation>,
    /// Currently "selected" change.
    position: Option<usize>,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Direction {
    Prev,
    Next,
}

impl ChangeList {
    pub fn new() -> Self {
        Self {
            changes: Vec::new(),
            position: None,
        }
    }

    /// Moves to the next change in the list (based on the direction given) and returns the caret positions for the next change.
    /// If reaches the end of the list in the direction, returns the corresponding change until called for a different direction.
    pub fn next_change(&mut self, count: usize, direction: Direction) -> Option<&[Anchor]> {
        if self.changes.is_empty() {
            return None;
        }

        let prev = self.position.unwrap_or(self.changes.len());
        let next = if direction == Direction::Prev {
            prev.saturating_sub(count)
        } else {
            (prev + count).min(self.changes.len() - 1)
        };
        self.position = Some(next);
        self.changes.get(next).map(|change| change.locations())
    }

    /// Adds a new change to the list, resetting the change list position.
    pub fn push_to_change_list(&mut self, group: bool, new_positions: Vec<Anchor>) {
        self.position.take();
        if let Some(last) = self.changes.last_mut()
            && group
        {
            last.current = Some(new_positions)
        } else {
            self.changes.push(ChangeLocation {
                original: new_positions,
                current: None,
            });
        }
    }

    pub fn last(&self) -> Option<&[Anchor]> {
        self.changes.last().map(|change| change.locations())
    }

    pub fn last_before_grouping(&self) -> Option<&[Anchor]> {
        self.changes.last().map(|change| change.original.as_slice())
    }

    pub fn invert_last_group(&mut self) {
        if let Some(last) = self.changes.last_mut()
            && let Some(current) = last.current.as_mut()
        {
            mem::swap(&mut last.original, current);
        }
    }
}

enum SelectionDragState {
    /// State when no drag related activity is detected.
    None,
    /// State when the mouse is down on a selection that is about to be dragged.
    ReadyToDrag {
        selection: Selection<Anchor>,
        click_position: gpui::Point<Pixels>,
        mouse_down_time: Instant,
    },
    /// State when the mouse is dragging the selection in the editor.
    Dragging {
        selection: Selection<Anchor>,
        drop_cursor: Selection<Anchor>,
        hide_drop_cursor: bool,
    },
}

struct ColumnarSelectionState {
    selection_tail: Anchor,
    display_point: Option<DisplayPoint>,
    base_selections: Arc<[Selection<Anchor>]>,
}

/// Represents a button that shows up when hovering over lines in the gutter that don't have
/// any button on them already (like a bookmark, breakpoint or run indicator).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GutterHoverButton {
    display_row: DisplayRow,
    /// There's a small debounce between hovering over the line and showing the indicator.
    /// We don't want to show the indicator when moving the mouse from editor to e.g. project panel.
    is_active: bool,
}

/// Zed's primary implementation of text input, allowing users to edit a [`MultiBuffer`].
///
/// See the [module level documentation](self) for more information.
pub struct Editor {
    focus_handle: FocusHandle,
    last_focused_descendant: Option<WeakFocusHandle>,
    /// The text buffer being edited
    buffer: Entity<MultiBuffer>,
    /// Map of how text in the buffer should be displayed.
    /// Handles soft wraps, folds, fake inlay text insertions, etc.
    pub display_map: Entity<DisplayMap>,
    placeholder_display_map: Option<Entity<DisplayMap>>,
    pub selections: SelectionsCollection,
    /// Manages the scroll position for the given editor.
    ///
    /// Whenever you want to modify the scroll position of the editor, you should
    /// usually use the existing available APIs as opposed to directly interacting
    /// with the scroll manager.
    pub(crate) scroll_manager: ScrollManager,
    /// When inline assist editors are linked, they all render cursors because
    /// typing enters text into each of them, even the ones that aren't focused.
    pub(crate) show_cursor_when_unfocused: bool,
    columnar_display_point: Option<DisplayPoint>,
    columnar_selection_state: Option<ColumnarSelectionState>,
    add_selections_state: Option<AddSelectionsState>,
    select_next_state: Option<SelectNextState>,
    select_prev_state: Option<SelectNextState>,
    selection_history: SelectionHistory,
    defer_selection_effects: bool,
    deferred_selection_effects_state: Option<DeferredSelectionEffectsState>,
    select_syntax_node_history: SelectSyntaxNodeHistory,
    ime_transaction: Option<TransactionId>,
    hard_wrap: Option<usize>,
    project: Option<Entity<Project>>,
    semantics_provider: Option<Rc<dyn SemanticsProvider>>,
    blink_manager: Entity<BlinkManager>,
    show_cursor_names: bool,
    pub show_local_selections: bool,
    mode: EditorMode,
    breadcrumbs_visibility: BreadcrumbsVisibility,
    show_gutter: bool,
    offset_content: bool,
    disable_expand_excerpt_buttons: bool,
    enable_lsp_data: bool,
    needs_initial_data_update: bool,
    enable_mouse_wheel_zoom: bool,
    show_line_numbers: Option<bool>,
    soft_wrap: Option<bool>,
    use_relative_line_numbers: Option<bool>,
    show_git_diff_gutter: Option<bool>,
    buffers_with_disabled_indent_guides: HashSet<BufferId>,
    highlight_order: usize,
    highlighted_rows: HashMap<TypeId, Vec<RowHighlight>>,
    background_highlights: HashMap<HighlightKey, BackgroundHighlight>,
    navigation_overlays: HashMap<NavigationOverlayKey, Arc<[NavigationTargetOverlay]>>,
    gutter_highlights: HashMap<TypeId, GutterHighlight>,
    scrollbar_marker_state: ScrollbarMarkerState,
    active_indent_guides_state: ActiveIndentGuidesState,
    nav_history: Option<ItemNavHistory>,
    mouse_context_menu: Option<MouseContextMenu>,
    quick_selection_highlight_task: Option<(Range<Anchor>, Task<()>)>,
    debounced_selection_highlight_task: Option<(Range<Anchor>, Task<()>)>,
    debounced_selection_highlight_complete: bool,
    last_selection_from_search: bool,
    document_highlights_task: Option<Task<()>>,
    pending_rename: Option<RenameState>,
    searchable: bool,
    cursor_shape: CursorShape,
    /// Whether the cursor is offset one character to the left when something is
    /// selected (needed for vim visual mode)
    cursor_offset_on_selection: bool,
    current_line_highlight: Option<CurrentLineHighlight>,
    /// Whether to collapse search match ranges to just their start position.
    /// When true, navigating to a match positions the cursor at the match
    /// without selecting the matched text.
    collapse_matches: bool,
    autoindent_mode: Option<AutoindentMode>,
    workspace: Option<(WeakEntity<Workspace>, Option<WorkspaceId>)>,
    input_enabled: bool,
    expects_character_input: bool,
    use_modal_editing: bool,
    read_only: bool,
    leader_id: Option<CollaboratorId>,
    remote_id: Option<ViewId>,
    pending_mouse_down: Option<Rc<RefCell<Option<MouseDownEvent>>>>,
    prev_pressure_stage: Option<PressureStage>,
    gutter_hovered: bool,
    hovered_link_state: Option<HoveredLinkState>,
    in_leading_whitespace: bool,
    next_color_inlay_id: usize,
    _subscriptions: Vec<Subscription>,
    pixel_position_of_newest_cursor: Option<gpui::Point<Pixels>>,
    gutter_dimensions: GutterDimensions,
    style: Option<EditorStyle>,
    text_style_refinement: Option<TextStyleRefinement>,
    next_editor_action_id: EditorActionId,
    editor_actions: Rc<
        RefCell<BTreeMap<EditorActionId, Box<dyn Fn(&Editor, &mut Window, &mut Context<Self>)>>>,
    >,
    use_selection_highlight: bool,
    auto_replace_emoji_shortcode: bool,
    buffer_serialization: Option<BufferSerialization>,
    custom_context_menu: Option<
        Box<
            dyn 'static
                + Fn(
                    &mut Self,
                    DisplayPoint,
                    &mut Window,
                    &mut Context<Self>,
                ) -> Option<Entity<ui::ContextMenu>>,
        >,
    >,
    last_bounds: Option<Bounds<Pixels>>,
    last_position_map: Option<Rc<PositionMap>>,
    expect_bounds_change: Option<Bounds<Pixels>>,
    gutter_hover_button: (Option<GutterHoverButton>, Option<Task<()>>),
    previous_search_ranges: Option<Arc<[Range<Anchor>]>>,
    breadcrumb_header: Option<String>,
    focused_block: Option<FocusedBlock>,
    next_scroll_position: NextScrollCursorCenterTopBottom,
    addons: HashMap<TypeId, Box<dyn Addon>>,
    registered_buffers: HashMap<BufferId, OpenLspBufferHandle>,
    selection_mark_mode: bool,
    _scroll_cursor_center_top_bottom_task: Task<()>,
    minimap: Option<Entity<Self>>,
    pub change_list: ChangeList,

    selection_drag_state: SelectionDragState,
    colors: Option<LspColorData>,
    post_scroll_update: Task<()>,
    refresh_colors_task: Task<()>,
    use_document_folding_ranges: bool,
    refresh_folding_ranges_task: Task<()>,
    folding_newlines: Task<()>,
    select_next_is_case_sensitive: Option<bool>,
    pub lookup_key: Option<Box<dyn Any + Send + Sync>>,
    on_local_selections_changed:
        Option<Box<dyn Fn(Point, &mut Window, &mut Context<Self>) + 'static>>,
    suppress_selection_callback: bool,
    applicable_language_settings: HashMap<Option<LanguageName>, LanguageSettings>,
    bracket_fetched_tree_sitter_chunks: HashMap<Range<text::Anchor>, HashSet<Range<BufferRow>>>,
    semantic_token_state: SemanticTokenState,
    pub(crate) refresh_matching_bracket_highlights_task: Task<()>,
    refresh_document_symbols_task: Shared<Task<()>>,
    lsp_document_symbols: HashMap<BufferId, Vec<OutlineItem<text::Anchor>>>,
    refresh_outline_symbols_at_cursor_at_cursor_task: Task<()>,
    outline_symbols_at_cursor: Option<(BufferId, Vec<OutlineItem<Anchor>>)>,
    sticky_headers_task: Task<()>,
    sticky_headers: Option<Vec<OutlineItem<Anchor>>>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
enum NextScrollCursorCenterTopBottom {
    #[default]
    Center,
    Top,
    Bottom,
}

impl NextScrollCursorCenterTopBottom {
    fn next(&self) -> Self {
        match self {
            Self::Center => Self::Top,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Center,
        }
    }
}

#[derive(Clone)]
pub struct EditorSnapshot {
    pub mode: EditorMode,
    show_gutter: bool,
    offset_content: bool,
    show_line_numbers: Option<bool>,
    show_git_diff_gutter: Option<bool>,
    pub display_snapshot: DisplaySnapshot,
    pub placeholder_display_snapshot: Option<DisplaySnapshot>,
    is_focused: bool,
    scroll_anchor: SharedScrollAnchor,
    ongoing_scroll: OngoingScroll,
    current_line_highlight: CurrentLineHighlight,
    gutter_hovered: bool,
    semantic_tokens_enabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NavigationTargetOverlay {
    pub target_range: Range<Anchor>,
    pub label: NavigationOverlayLabel,
    pub covered_text_range: Option<Range<Anchor>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NavigationOverlayLabel {
    pub text: SharedString,
    pub text_color: Hsla,
    pub x_offset: Pixels,
    pub scale_factor: f32,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct GutterDimensions {
    pub left_padding: Pixels,
    pub right_padding: Pixels,
    pub width: Pixels,
    pub margin: Pixels,
}

impl GutterDimensions {
    fn default_with_margin(font_id: FontId, font_size: Pixels, cx: &App) -> Self {
        Self {
            margin: Self::default_gutter_margin(font_id, font_size, cx),
            ..Default::default()
        }
    }

    fn default_gutter_margin(font_id: FontId, font_size: Pixels, cx: &App) -> Pixels {
        -cx.text_system().descent(font_id, font_size)
    }
    /// The full width of the space taken up by the gutter.
    pub fn full_width(&self) -> Pixels {
        self.margin + self.width
    }
}

struct CharacterDimensions {
    em_width: Pixels,
    em_advance: Pixels,
    line_height: Pixels,
}

#[derive(Debug)]
pub struct RemoteSelection {
    pub replica_id: ReplicaId,
    pub selection: Selection<Anchor>,
    pub cursor_shape: CursorShape,
    pub collaborator_id: CollaboratorId,
    pub line_mode: bool,
    pub user_name: Option<SharedString>,
    pub color: PlayerColor,
}

#[derive(Clone, Debug)]
struct SelectionHistoryEntry {
    selections: Arc<[Selection<Anchor>]>,
    select_next_state: Option<SelectNextState>,
    select_prev_state: Option<SelectNextState>,
    add_selections_state: Option<AddSelectionsState>,
}

#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
enum SelectionHistoryMode {
    #[default]
    Normal,
    Undoing,
    Redoing,
    Skipping,
}

#[derive(Debug)]
/// SelectionEffects controls the side-effects of updating the selection.
///
/// The default behaviour does "what you mostly want":
/// - it pushes to the nav history if the cursor moved by >10 lines
/// - it scrolls to fit
///
/// You might want to modify these behaviours. For example when doing a "jump"
/// like go to definition, we always want to add to nav history; but when scrolling
/// in vim mode we never do.
///
/// Similarly, you might want to disable scrolling if you don't want the viewport to
/// move.
#[derive(Clone)]
pub struct SelectionEffects {
    nav_history: Option<bool>,
    scroll: Option<Autoscroll>,
    from_search: bool,
}

impl Default for SelectionEffects {
    fn default() -> Self {
        Self {
            nav_history: None,
            scroll: Some(Autoscroll::fit()),
            from_search: false,
        }
    }
}
impl SelectionEffects {
    pub fn scroll(scroll: Autoscroll) -> Self {
        Self {
            scroll: Some(scroll),
            ..Default::default()
        }
    }

    pub fn no_scroll() -> Self {
        Self {
            scroll: None,
            ..Default::default()
        }
    }

    pub fn nav_history(self, nav_history: bool) -> Self {
        Self {
            nav_history: Some(nav_history),
            ..self
        }
    }

    pub fn from_search(self, from_search: bool) -> Self {
        Self {
            from_search,
            ..self
        }
    }
}

struct DeferredSelectionEffectsState {
    changed: bool,
    effects: SelectionEffects,
    old_cursor_position: Anchor,
    history_entry: SelectionHistoryEntry,
}

#[derive(Default)]
struct SelectionHistory {
    #[allow(clippy::type_complexity)]
    selections_by_transaction:
        HashMap<TransactionId, (Arc<[Selection<Anchor>]>, Option<Arc<[Selection<Anchor>]>>)>,
    mode: SelectionHistoryMode,
    undo_stack: VecDeque<SelectionHistoryEntry>,
    redo_stack: VecDeque<SelectionHistoryEntry>,
}

impl SelectionHistory {
    #[track_caller]
    fn insert_transaction(
        &mut self,
        transaction_id: TransactionId,
        selections: Arc<[Selection<Anchor>]>,
    ) {
        if selections.is_empty() {
            log::error!(
                "SelectionHistory::insert_transaction called with empty selections. Caller: {}",
                std::panic::Location::caller()
            );
            return;
        }
        self.selections_by_transaction
            .insert(transaction_id, (selections, None));
    }

    #[allow(clippy::type_complexity)]
    fn transaction(
        &self,
        transaction_id: TransactionId,
    ) -> Option<&(Arc<[Selection<Anchor>]>, Option<Arc<[Selection<Anchor>]>>)> {
        self.selections_by_transaction.get(&transaction_id)
    }

    #[allow(clippy::type_complexity)]
    fn transaction_mut(
        &mut self,
        transaction_id: TransactionId,
    ) -> Option<&mut (Arc<[Selection<Anchor>]>, Option<Arc<[Selection<Anchor>]>>)> {
        self.selections_by_transaction.get_mut(&transaction_id)
    }

    fn push(&mut self, entry: SelectionHistoryEntry) {
        if !entry.selections.is_empty() {
            match self.mode {
                SelectionHistoryMode::Normal => {
                    self.push_undo(entry);
                    self.redo_stack.clear();
                }
                SelectionHistoryMode::Undoing => self.push_redo(entry),
                SelectionHistoryMode::Redoing => self.push_undo(entry),
                SelectionHistoryMode::Skipping => {}
            }
        }
    }

    fn push_undo(&mut self, entry: SelectionHistoryEntry) {
        if self
            .undo_stack
            .back()
            .is_none_or(|e| e.selections != entry.selections)
        {
            self.undo_stack.push_back(entry);
            if self.undo_stack.len() > MAX_SELECTION_HISTORY_LEN {
                self.undo_stack.pop_front();
            }
        }
    }

    fn push_redo(&mut self, entry: SelectionHistoryEntry) {
        if self
            .redo_stack
            .back()
            .is_none_or(|e| e.selections != entry.selections)
        {
            self.redo_stack.push_back(entry);
            if self.redo_stack.len() > MAX_SELECTION_HISTORY_LEN {
                self.redo_stack.pop_front();
            }
        }
    }
}

#[derive(Clone, Copy)]
pub struct RowHighlightOptions {
    pub autoscroll: bool,
    pub include_gutter: bool,
}

impl Default for RowHighlightOptions {
    fn default() -> Self {
        Self {
            autoscroll: Default::default(),
            include_gutter: true,
        }
    }
}

struct RowHighlight {
    index: usize,
    range: Range<Anchor>,
    color: Hsla,
    options: RowHighlightOptions,
    type_id: TypeId,
}

#[derive(Clone, Debug)]
struct AddSelectionsState {
    groups: Vec<AddSelectionsGroup>,
}

#[derive(Clone, Debug)]
struct AddSelectionsGroup {
    above: bool,
    stack: Vec<usize>,
}

#[derive(Clone)]
struct SelectNextState {
    query: AhoCorasick,
    wordwise: bool,
    done: bool,
}

impl std::fmt::Debug for SelectNextState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(std::any::type_name::<Self>())
            .field("wordwise", &self.wordwise)
            .field("done", &self.done)
            .finish()
    }
}

#[doc(hidden)]
pub struct RenameState {
    pub range: Range<Anchor>,
    pub old_name: Arc<str>,
    pub editor: Entity<Editor>,
    block_id: CustomBlockId,
}

// selections, scroll behavior, was newest selection reversed
type SelectSyntaxNodeHistoryState = (
    Box<[Selection<Anchor>]>,
    SelectSyntaxNodeScrollBehavior,
    bool,
);

#[derive(Default)]
struct SelectSyntaxNodeHistory {
    stack: Vec<SelectSyntaxNodeHistoryState>,
    // disable temporarily to allow changing selections without losing the stack
    pub disable_clearing: bool,
}

impl SelectSyntaxNodeHistory {
    pub fn try_clear(&mut self) {
        if !self.disable_clearing {
            self.stack.clear();
        }
    }

    pub fn push(&mut self, selection: SelectSyntaxNodeHistoryState) {
        self.stack.push(selection);
    }

    pub fn pop(&mut self) -> Option<SelectSyntaxNodeHistoryState> {
        self.stack.pop()
    }
}

enum SelectSyntaxNodeScrollBehavior {
    CursorTop,
    FitSelection,
    CursorBottom,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NavigationData {
    cursor_anchor: Anchor,
    cursor_position: Point,
    scroll_anchor: ScrollAnchor,
    scroll_top_row: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GotoDefinitionKind {
    Symbol,
    Declaration,
    Type,
    Implementation,
}

pub enum FormatTarget {
    Buffers(HashSet<Entity<Buffer>>),
    Ranges(Vec<Range<MultiBufferPoint>>),
}

pub(crate) struct FocusedBlock {
    id: BlockId,
    focus_handle: WeakFocusHandle,
}

#[derive(Clone, Debug)]
pub enum JumpData {
    MultiBufferRow {
        row: MultiBufferRow,
        line_offset_from_top: u32,
    },
    MultiBufferPoint {
        anchor: language::Anchor,
        position: Point,
        line_offset_from_top: u32,
    },
}

pub enum MultibufferSelectionMode {
    First,
    All,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RewrapOptions {
    pub override_language_settings: bool,
    pub preserve_existing_whitespace: bool,
    pub line_length: Option<usize>,
}

impl Editor {
    pub fn is_lsp_relevant(&self, file: Option<&Arc<dyn language::File>>, cx: &App) -> bool {
        let Some(project) = self.project() else {
            return false;
        };
        let Some(buffer_file) = project::File::from_dyn(file) else {
            return false;
        };
        let Some(entry_id) = buffer_file.project_entry_id() else {
            return false;
        };
        let project = project.read(cx);
        let Some(buffer_worktree) = project.worktree_for_id(buffer_file.worktree_id(cx), cx) else {
            return false;
        };
        let Some(worktree_entry) = buffer_worktree.read(cx).entry_for_id(entry_id) else {
            return false;
        };
        !worktree_entry.is_ignored
    }

    pub fn visible_buffers(&self, cx: &mut Context<Editor>) -> Vec<Entity<Buffer>> {
        let display_snapshot = self.display_snapshot(cx);
        let visible_range = self.multi_buffer_visible_range(&display_snapshot, cx);
        let multi_buffer = self.buffer().read(cx);
        display_snapshot
            .buffer_snapshot()
            .range_to_buffer_ranges(visible_range)
            .into_iter()
            .filter(|(_, excerpt_visible_range, _)| !excerpt_visible_range.is_empty())
            .filter_map(|(buffer_snapshot, _, _)| multi_buffer.buffer(buffer_snapshot.remote_id()))
            .collect()
    }

    pub fn visible_buffer_ranges(
        &self,
        cx: &mut Context<Editor>,
    ) -> Vec<(
        BufferSnapshot,
        Range<BufferOffset>,
        ExcerptRange<text::Anchor>,
    )> {
        let display_snapshot = self.display_snapshot(cx);
        let visible_range = self.multi_buffer_visible_range(&display_snapshot, cx);
        display_snapshot
            .buffer_snapshot()
            .range_to_buffer_ranges(visible_range)
            .into_iter()
            .filter(|(_, excerpt_visible_range, _)| !excerpt_visible_range.is_empty())
            .collect()
    }

    pub fn text_layout_details(&self, window: &mut Window, cx: &mut App) -> TextLayoutDetails {
        TextLayoutDetails {
            text_system: window.text_system().clone(),
            editor_style: self.style.clone().unwrap_or_else(|| self.create_style(cx)),
            rem_size: window.rem_size(),
            scroll_anchor: self.scroll_manager.shared_scroll_anchor(cx),
            visible_rows: self.visible_line_count(),
            vertical_scroll_margin: self.scroll_manager.vertical_scroll_margin,
        }
    }

    pub fn single_line(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let buffer = cx.new(|cx| Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        Self::new(EditorMode::SingleLine, buffer, None, window, cx)
    }

    pub fn multi_line(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let buffer = cx.new(|cx| Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        Self::new(EditorMode::full(), buffer, None, window, cx)
    }

    pub fn auto_height(
        min_lines: usize,
        max_lines: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let buffer = cx.new(|cx| Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        Self::new(
            EditorMode::AutoHeight {
                min_lines,
                max_lines: Some(max_lines),
            },
            buffer,
            None,
            window,
            cx,
        )
    }

    /// Creates a new auto-height editor with a minimum number of lines but no maximum.
    /// The editor grows as tall as needed to fit its content.
    pub fn auto_height_unbounded(
        min_lines: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let buffer = cx.new(|cx| Buffer::local("", cx));
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        Self::new(
            EditorMode::AutoHeight {
                min_lines,
                max_lines: None,
            },
            buffer,
            None,
            window,
            cx,
        )
    }

    pub fn for_buffer(
        buffer: Entity<Buffer>,
        project: Option<Entity<Project>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let buffer = cx.new(|cx| MultiBuffer::singleton(buffer, cx));
        Self::new(EditorMode::full(), buffer, project, window, cx)
    }

    pub fn for_multibuffer(
        buffer: Entity<MultiBuffer>,
        project: Option<Entity<Project>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(EditorMode::full(), buffer, project, window, cx)
    }

    pub fn clone(&self, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut clone = Self::new(
            self.mode.clone(),
            self.buffer.clone(),
            self.project.clone(),
            window,
            cx,
        );
        let my_snapshot = self.display_map.update(cx, |display_map, cx| {
            let snapshot = display_map.snapshot(cx);
            clone.display_map.update(cx, |display_map, cx| {
                display_map.set_state(&snapshot, cx);
            });
            snapshot
        });
        let clone_snapshot = clone.display_map.update(cx, |map, cx| map.snapshot(cx));
        clone.folds_did_change(cx);
        clone.selections.clone_state(&self.selections);
        clone
            .scroll_manager
            .clone_state(&self.scroll_manager, &my_snapshot, &clone_snapshot, cx);
        clone.searchable = self.searchable;
        clone.read_only = self.read_only;
        clone.buffers_with_disabled_indent_guides =
            self.buffers_with_disabled_indent_guides.clone();
        clone.enable_mouse_wheel_zoom = self.enable_mouse_wheel_zoom;
        clone.enable_lsp_data = self.enable_lsp_data;
        clone.needs_initial_data_update = self.enable_lsp_data;
        clone
    }

    pub fn new(
        mode: EditorMode,
        buffer: Entity<MultiBuffer>,
        project: Option<Entity<Project>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Editor::new_internal(mode, buffer, project, None, window, cx)
    }

    pub fn refresh_sticky_headers(
        &mut self,
        display_snapshot: &DisplaySnapshot,
        cx: &mut Context<Editor>,
    ) {
        if !self.mode.is_full() {
            return;
        }
        let multi_buffer = display_snapshot.buffer_snapshot().clone();
        let scroll_anchor = self
            .scroll_manager
            .native_anchor(display_snapshot, cx)
            .anchor;
        let Some(buffer_snapshot) = multi_buffer.as_singleton() else {
            return;
        };

        let buffer = buffer_snapshot.clone();
        let Some((buffer_visible_start, _)) = multi_buffer.anchor_to_buffer_anchor(scroll_anchor)
        else {
            return;
        };
        let buffer_visible_start = buffer_visible_start.to_point(&buffer);
        let max_row = buffer.max_point().row;
        let start_row = buffer_visible_start.row.min(max_row);
        let end_row = (buffer_visible_start.row + 10).min(max_row);

        let syntax = self.style(cx).syntax.clone();
        let background_task = cx.background_spawn(async move {
            buffer
                .outline_items_containing(
                    Point::new(start_row, 0)..Point::new(end_row, 0),
                    true,
                    Some(syntax.as_ref()),
                )
                .into_iter()
                .filter_map(|outline_item| {
                    Some(OutlineItem {
                        depth: outline_item.depth,
                        range: multi_buffer
                            .buffer_anchor_range_to_anchor_range(outline_item.range)?,
                        source_range_for_text: multi_buffer.buffer_anchor_range_to_anchor_range(
                            outline_item.source_range_for_text,
                        )?,
                        text: outline_item.text,
                        highlight_ranges: outline_item.highlight_ranges,
                        name_ranges: outline_item.name_ranges,
                        body_range: outline_item.body_range.and_then(|range| {
                            multi_buffer.buffer_anchor_range_to_anchor_range(range)
                        }),
                        annotation_range: outline_item.annotation_range.and_then(|range| {
                            multi_buffer.buffer_anchor_range_to_anchor_range(range)
                        }),
                    })
                })
                .collect()
        });
        self.sticky_headers_task = cx.spawn(async move |this, cx| {
            let sticky_headers = background_task.await;
            this.update(cx, |this, cx| {
                if this.sticky_headers.as_ref() != Some(&sticky_headers) {
                    this.sticky_headers = Some(sticky_headers);
                    cx.notify();
                }
            })
            .ok();
        });
    }

    fn new_internal(
        mode: EditorMode,
        multi_buffer: Entity<MultiBuffer>,
        project: Option<Entity<Project>>,
        display_map: Option<Entity<DisplayMap>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        debug_assert!(
            display_map.is_none() || mode.is_minimap(),
            "Providing a display map for a new editor is only intended for the minimap and might have unintended side effects otherwise!"
        );

        let full_mode = mode.is_full();
        let is_minimap = mode.is_minimap();
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let editor = cx.entity().downgrade();
        let fold_placeholder = FoldPlaceholder {
            constrain_width: false,
            render: Arc::new(move |fold_id, fold_range, cx| {
                let editor = editor.clone();
                FoldPlaceholder::fold_element(fold_id, cx)
                    .cursor_pointer()
                    .child("⋯")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(move |_, _window, cx| {
                        editor
                            .update(cx, |editor, cx| {
                                editor.unfold_ranges(
                                    &[fold_range.start..fold_range.end],
                                    true,
                                    false,
                                    cx,
                                );
                                cx.stop_propagation();
                            })
                            .ok();
                    })
                    .into_any()
            }),
            merge_adjacent: true,
            ..FoldPlaceholder::default()
        };
        let display_map = display_map.unwrap_or_else(|| {
            cx.new(|cx| {
                DisplayMap::new(
                    multi_buffer.clone(),
                    style.font(),
                    font_size,
                    None,
                    FILE_HEADER_HEIGHT,
                    MULTI_BUFFER_EXCERPT_HEADER_HEIGHT,
                    fold_placeholder,
                    cx,
                )
            })
        });

        let selections = SelectionsCollection::new();

        let blink_manager = cx.new(|cx| {
            let mut blink_manager = BlinkManager::new(
                CURSOR_BLINK_INTERVAL,
                |cx| EditorSettings::get_global(cx).cursor_blink,
                cx,
            );
            if is_minimap {
                blink_manager.disable(cx);
            }
            blink_manager
        });

        let mut project_subscriptions = Vec::new();
        if full_mode && let Some(project) = project.as_ref() {
            project_subscriptions.push(cx.subscribe_in(
                project,
                window,
                |editor, _, event, window, cx| match event {
                    project::Event::RefreshSemanticTokens {
                        server_id,
                        request_id,
                    } => {
                        editor.refresh_semantic_tokens(
                            None,
                            Some(RefreshForServer {
                                server_id: *server_id,
                                request_id: *request_id,
                            }),
                            cx,
                        );
                    }
                    project::Event::LanguageServerRemoved(_) => {
                        editor.registered_buffers.clear();
                        editor.register_visible_buffers(cx);
                        editor.invalidate_semantic_tokens(None);
                        editor.update_lsp_data(None, window, cx);
                    }
                    project::Event::LanguageServerBufferRegistered { buffer_id, .. } => {
                        let buffer_id = *buffer_id;
                        if editor.buffer().read(cx).buffer(buffer_id).is_some() {
                            editor.register_buffer(buffer_id, cx);
                            editor.update_lsp_data(Some(buffer_id), window, cx);
                            editor.refresh_document_highlights(cx);
                        }
                    }

                    project::Event::EntryRenamed(transaction, project_path, abs_path) => {
                        let Some(workspace) = editor.workspace() else {
                            return;
                        };
                        let Some(active_editor) = workspace.read(cx).active_item_as::<Self>(cx)
                        else {
                            return;
                        };

                        if active_editor.entity_id() == cx.entity_id() {
                            let entity_id = cx.entity_id();
                            workspace.update(cx, |this, cx| {
                                this.panes_mut()
                                    .iter_mut()
                                    .filter(|pane| pane.entity_id() != entity_id)
                                    .for_each(|p| {
                                        p.update(cx, |pane, _| {
                                            pane.nav_history_mut().rename_item(
                                                entity_id,
                                                project_path.clone(),
                                                abs_path.clone().into(),
                                            );
                                        })
                                    });
                            });

                            Self::open_transaction_for_hidden_buffers(
                                workspace,
                                transaction.clone(),
                                "Rename".to_string(),
                                window,
                                cx,
                            );
                        }
                    }

                    project::Event::WorkspaceEditApplied(transaction) => {
                        let Some(workspace) = editor.workspace() else {
                            return;
                        };
                        let Some(active_editor) = workspace.read(cx).active_item_as::<Self>(cx)
                        else {
                            return;
                        };

                        if active_editor.entity_id() == cx.entity_id() {
                            Self::open_transaction_for_hidden_buffers(
                                workspace,
                                transaction.clone(),
                                "LSP Edit".to_string(),
                                window,
                                cx,
                            );
                        }
                    }

                    _ => {}
                },
            ));
        }

        let focus_handle = cx.focus_handle();
        if !is_minimap {
            cx.on_focus(&focus_handle, window, Self::handle_focus)
                .detach();
            cx.on_focus_in(&focus_handle, window, Self::handle_focus_in)
                .detach();
            cx.on_focus_out(&focus_handle, window, Self::handle_focus_out)
                .detach();
            cx.on_blur(&focus_handle, window, Self::handle_blur)
                .detach();
            cx.observe_pending_input(window, Self::observe_pending_input)
                .detach();
        }

        let mut editor = Self {
            focus_handle,
            show_cursor_when_unfocused: false,
            last_focused_descendant: None,
            buffer: multi_buffer.clone(),
            display_map: display_map.clone(),
            placeholder_display_map: None,
            selections,
            scroll_manager: ScrollManager::new(cx),
            columnar_display_point: None,
            columnar_selection_state: None,
            add_selections_state: None,
            select_next_state: None,
            select_prev_state: None,
            selection_history: SelectionHistory::default(),
            defer_selection_effects: false,
            deferred_selection_effects_state: None,
            select_syntax_node_history: SelectSyntaxNodeHistory::default(),
            ime_transaction: None,
            hard_wrap: None,
            semantics_provider: project
                .as_ref()
                .map(|project| Rc::new(project.downgrade()) as _),
            project,
            blink_manager: blink_manager.clone(),
            show_local_selections: true,
            offset_content: !matches!(mode, EditorMode::SingleLine),
            breadcrumbs_visibility: BreadcrumbsVisibility::from_settings(cx),
            show_gutter: full_mode,
            show_line_numbers: (!full_mode).then_some(false),
            soft_wrap: None,
            use_relative_line_numbers: None,
            disable_expand_excerpt_buttons: !full_mode,
            enable_lsp_data: full_mode,
            needs_initial_data_update: full_mode,
            enable_mouse_wheel_zoom: full_mode,
            show_git_diff_gutter: None,
            buffers_with_disabled_indent_guides: HashSet::default(),
            highlight_order: 0,
            highlighted_rows: HashMap::default(),
            background_highlights: HashMap::default(),
            navigation_overlays: HashMap::default(),
            gutter_highlights: HashMap::default(),
            scrollbar_marker_state: ScrollbarMarkerState::default(),
            active_indent_guides_state: ActiveIndentGuidesState::default(),
            nav_history: None,
            mouse_context_menu: None,
            quick_selection_highlight_task: None,
            debounced_selection_highlight_task: None,
            debounced_selection_highlight_complete: false,
            last_selection_from_search: false,
            document_highlights_task: None,
            pending_rename: None,
            searchable: !is_minimap,
            cursor_shape: EditorSettings::get_global(cx)
                .cursor_shape
                .unwrap_or_default(),
            cursor_offset_on_selection: false,
            current_line_highlight: None,
            autoindent_mode: Some(AutoindentMode::EachLine),
            collapse_matches: false,
            workspace: None,
            input_enabled: !is_minimap,
            expects_character_input: !is_minimap,
            use_modal_editing: full_mode,
            read_only: is_minimap,
            use_selection_highlight: true,
            auto_replace_emoji_shortcode: false,
            leader_id: None,
            remote_id: None,
            pending_mouse_down: None,
            prev_pressure_stage: None,
            hovered_link_state: None,
            gutter_hovered: false,
            pixel_position_of_newest_cursor: None,
            last_bounds: None,
            last_position_map: None,
            expect_bounds_change: None,
            gutter_dimensions: GutterDimensions::default(),
            style: None,
            show_cursor_names: false,
            next_editor_action_id: EditorActionId::default(),
            editor_actions: Rc::default(),
            in_leading_whitespace: false,
            custom_context_menu: None,
            buffer_serialization: is_minimap.not().then(|| {
                BufferSerialization::new(
                    ProjectSettings::get_global(cx)
                        .session
                        .restore_unsaved_buffers,
                )
            }),

            gutter_hover_button: (None, None),
            _subscriptions: (!is_minimap)
                .then(|| {
                    vec![
                        cx.observe(&multi_buffer, Self::on_buffer_changed),
                        cx.subscribe_in(&multi_buffer, window, Self::on_buffer_event),
                        cx.observe_in(&display_map, window, Self::on_display_map_changed),
                        cx.observe(&blink_manager, |_, _, cx| cx.notify()),
                        cx.observe_global_in::<SettingsStore>(window, Self::settings_changed),
                        cx.observe_global_in::<GlobalTheme>(window, Self::theme_changed),
                        observe_buffer_font_size_adjustment(cx, |_, cx| cx.notify()),
                        cx.observe_window_activation(window, |editor, window, cx| {
                            let active = window.is_window_active();
                            editor.blink_manager.update(cx, |blink_manager, cx| {
                                if active {
                                    blink_manager.enable(cx);
                                } else {
                                    blink_manager.disable(cx);
                                }
                            });
                        }),
                    ]
                })
                .unwrap_or_default(),
            colors: None,
            refresh_colors_task: Task::ready(()),
            use_document_folding_ranges: false,
            refresh_folding_ranges_task: Task::ready(()),
            next_color_inlay_id: 0,
            post_scroll_update: Task::ready(()),
            previous_search_ranges: None,
            breadcrumb_header: None,
            focused_block: None,
            next_scroll_position: NextScrollCursorCenterTopBottom::default(),
            addons: HashMap::default(),
            registered_buffers: HashMap::default(),
            _scroll_cursor_center_top_bottom_task: Task::ready(()),
            selection_mark_mode: false,
            text_style_refinement: None,
            minimap: None,
            change_list: ChangeList::new(),
            mode,
            selection_drag_state: SelectionDragState::None,
            folding_newlines: Task::ready(()),
            lookup_key: None,
            select_next_is_case_sensitive: None,
            on_local_selections_changed: None,
            suppress_selection_callback: false,
            applicable_language_settings: HashMap::default(),
            semantic_token_state: SemanticTokenState::new(cx, full_mode),
            bracket_fetched_tree_sitter_chunks: HashMap::default(),
            refresh_matching_bracket_highlights_task: Task::ready(()),
            refresh_document_symbols_task: Task::ready(()).shared(),
            lsp_document_symbols: HashMap::default(),
            refresh_outline_symbols_at_cursor_at_cursor_task: Task::ready(()),
            outline_symbols_at_cursor: None,
            sticky_headers_task: Task::ready(()),
            sticky_headers: None,
        };

        if is_minimap {
            return editor;
        }

        editor.applicable_language_settings = editor.fetch_applicable_language_settings(cx);

        editor._subscriptions.extend(project_subscriptions);

        editor._subscriptions.push(cx.subscribe_in(
            &cx.entity(),
            window,
            |editor, _, e: &EditorEvent, window, cx| match e {
                EditorEvent::ScrollPositionChanged { local, .. } => {
                    if *local {
                        let snapshot = editor.snapshot(window, cx);
                        let new_anchor = editor
                            .scroll_manager
                            .native_anchor(&snapshot.display_snapshot, cx);
                        editor.update_restoration_data(cx, move |data| {
                            data.scroll_position = (
                                new_anchor.top_row(snapshot.buffer_snapshot()),
                                new_anchor.offset,
                            );
                        });

                        editor.update_data_on_scroll(true, window, cx);
                    }
                    editor.refresh_sticky_headers(&editor.snapshot(window, cx), cx);
                }
                EditorEvent::Edited { .. } => {
                    let display_map = editor.display_snapshot(cx);
                    let selections = editor.selections.all_adjusted_display(&display_map);
                    let pop_state = editor
                        .change_list
                        .last()
                        .map(|previous| {
                            previous.len() == selections.len()
                                && previous.iter().enumerate().all(|(ix, p)| {
                                    p.to_display_point(&display_map).row()
                                        == selections[ix].head().row()
                                })
                        })
                        .unwrap_or(false);
                    let new_positions = selections
                        .into_iter()
                        .map(|s| display_map.display_point_to_anchor(s.head(), Bias::Left))
                        .collect();
                    editor
                        .change_list
                        .push_to_change_list(pop_state, new_positions);
                }
                _ => (),
            },
        ));

        // skip adding the initial selection to selection history
        editor.selection_history.mode = SelectionHistoryMode::Skipping;
        editor.end_selection(window, cx);
        editor.selection_history.mode = SelectionHistoryMode::Normal;

        if full_mode {
            editor.minimap = if editor.should_show_minimap(cx) {
                Some(editor.create_minimap(window, cx))
            } else {
                None
            };
            editor.colors = Some(LspColorData::new(cx));
            editor.use_document_folding_ranges = true;

            let buffer = multi_buffer.read(cx).as_singleton();
            editor.register_buffer(buffer.read(cx).remote_id(), cx);
        }

        editor
    }

    pub fn display_snapshot(&self, cx: &mut App) -> DisplaySnapshot {
        self.display_map.update(cx, |map, cx| map.snapshot(cx))
    }

    pub fn deploy_mouse_context_menu(
        &mut self,
        position: gpui::Point<Pixels>,
        context_menu: Entity<ContextMenu>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mouse_context_menu = Some(MouseContextMenu::new(
            self,
            crate::mouse_context_menu::MenuPosition::PinnedToScreen(position),
            context_menu,
            window,
            cx,
        ));
    }

    pub fn mouse_menu_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.mouse_context_menu
            .as_ref()
            .is_some_and(|menu| menu.context_menu.focus_handle(cx).is_focused(window))
    }

    pub fn key_context(&self, window: &mut Window, cx: &mut App) -> KeyContext {
        self.key_context_internal(false, window, cx)
    }

    fn key_context_internal(
        &self,
        _has_active_edit_prediction: bool, // VELIPSO: remove
        window: &mut Window,
        cx: &mut App,
    ) -> KeyContext {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("Editor");
        let mode = match self.mode {
            EditorMode::SingleLine => "single_line",
            EditorMode::AutoHeight { .. } => "auto_height",
            EditorMode::Minimap { .. } => "minimap",
            EditorMode::Full { .. } => "full",
        };

        key_context.set("mode", mode);
        if self.pending_rename.is_some() {
            key_context.add("renaming");
        }

        // Disable vim contexts when a sub-editor (e.g. rename/inline assistant) is focused.
        if !self.focus_handle(cx).contains_focused(window, cx)
            || (self.is_focused(window) || self.mouse_menu_is_focused(window, cx))
        {
            for addon in self.addons.values() {
                addon.extend_key_context(&mut key_context, cx)
            }
        }

        let singleton_buffer = self.buffer.read(cx).as_singleton();
        if let Some(extension) = singleton_buffer.read(cx).file().and_then(|file| {
            Some(
                file.full_path(cx)
                    .extension()?
                    .to_string_lossy()
                    .to_lowercase(),
            )
        }) {
            key_context.set("extension", extension);
        }

        if self.in_leading_whitespace {
            key_context.add("in_leading_whitespace");
        }
        key_context.set("edit_prediction_mode", "eager"); // VELIPSO: remove

        if self.selection_mark_mode {
            key_context.add("selection_mode");
        }

        let disjoint = self.selections.disjoint_anchors();
        if matches!(
            &self.mode,
            EditorMode::SingleLine | EditorMode::AutoHeight { .. }
        ) && let [selection] = disjoint
            && selection.start == selection.end
        {
            let snapshot = self.snapshot(window, cx);
            let snapshot = snapshot.buffer_snapshot();
            let caret_offset = selection.end.to_offset(snapshot);

            if caret_offset == MultiBufferOffset(0) {
                key_context.add("start_of_input");
            }

            if caret_offset == snapshot.len() {
                key_context.add("end_of_input");
            }
        }
        key_context
    }

    pub fn last_bounds(&self) -> Option<&Bounds<Pixels>> {
        self.last_bounds.as_ref()
    }

    pub fn working_directory(&self, cx: &App) -> Option<PathBuf> {
        let buffer = self.buffer().read(cx).as_singleton();
        if let Some(file) = buffer.read(cx).file().and_then(|f| f.as_local())
            && let Some(dir) = file.abs_path(cx).parent()
        {
            return Some(dir.to_owned());
        }

        None
    }

    pub fn target_file_abs_path(&self, cx: &mut Context<Self>) -> Option<PathBuf> {
        self.active_buffer(cx).and_then(|buffer| {
            let buffer = buffer.read(cx);
            if let Some(project_path) = buffer.project_path(cx) {
                let project = self.project()?.read(cx);
                project.absolute_path(&project_path, cx)
            } else {
                buffer
                    .file()
                    .and_then(|file| file.as_local().map(|file| file.abs_path(cx)))
            }
        })
    }

    pub fn new_file(
        workspace: &mut Workspace,
        _: &workspace::NewFile,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::new_in_workspace(workspace, window, cx).detach_and_prompt_err(
            "Failed to create buffer",
            window,
            cx,
            |e, _, _| match e.error_code() {
                ErrorCode::RemoteUpgradeRequired => Some(format!(
                "The remote instance of Zed does not support this yet. It must be upgraded to {}",
                e.error_tag("required").unwrap_or("the latest version")
            )),
                _ => None,
            },
        );
    }

    pub fn new_in_workspace(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Task<Result<Entity<Editor>>> {
        let project = workspace.project().clone();
        let create = project.update(cx, |project, cx| project.create_buffer(None, true, cx));

        cx.spawn_in(window, async move |workspace, cx| {
            let buffer = create.await?;
            workspace.update_in(cx, |workspace, window, cx| {
                let editor =
                    cx.new(|cx| Editor::for_buffer(buffer, Some(project.clone()), window, cx));
                workspace.add_item_to_active_pane(Box::new(editor.clone()), None, true, window, cx);
                editor
            })
        })
    }

    fn new_file_vertical(
        workspace: &mut Workspace,
        _: &workspace::NewFileSplitVertical,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::new_file_in_direction(workspace, SplitDirection::vertical(cx), window, cx)
    }

    fn new_file_horizontal(
        workspace: &mut Workspace,
        _: &workspace::NewFileSplitHorizontal,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::new_file_in_direction(workspace, SplitDirection::horizontal(cx), window, cx)
    }

    fn new_file_split(
        workspace: &mut Workspace,
        action: &workspace::NewFileSplit,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::new_file_in_direction(workspace, action.0, window, cx)
    }

    fn new_file_in_direction(
        workspace: &mut Workspace,
        direction: SplitDirection,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let project = workspace.project().clone();
        let create = project.update(cx, |project, cx| project.create_buffer(None, true, cx));

        cx.spawn_in(window, async move |workspace, cx| {
            let buffer = create.await?;
            workspace.update_in(cx, move |workspace, window, cx| {
                workspace.split_item(
                    direction,
                    Box::new(
                        cx.new(|cx| Editor::for_buffer(buffer, Some(project.clone()), window, cx)),
                    ),
                    window,
                    cx,
                )
            })?;
            anyhow::Ok(())
        })
        .detach_and_prompt_err("Failed to create buffer", window, cx, |e, _, _| {
            match e.error_code() {
                ErrorCode::RemoteUpgradeRequired => Some(format!(
                "The remote instance of Zed does not support this yet. It must be upgraded to {}",
                e.error_tag("required").unwrap_or("the latest version")
            )),
                _ => None,
            }
        });
    }

    pub fn leader_id(&self) -> Option<CollaboratorId> {
        self.leader_id
    }

    pub fn buffer(&self) -> &Entity<MultiBuffer> {
        &self.buffer
    }

    pub fn project(&self) -> Option<&Entity<Project>> {
        self.project.as_ref()
    }

    pub fn workspace(&self) -> Option<Entity<Workspace>> {
        self.workspace.as_ref()?.0.upgrade()
    }

    /// Detaches a task and shows an error notification in the workspace if available,
    /// otherwise just logs the error.
    pub fn detach_and_notify_err<R, E>(
        &self,
        task: Task<Result<R, E>>,
        window: &mut Window,
        cx: &mut App,
    ) where
        E: std::fmt::Debug + std::fmt::Display + 'static,
        R: 'static,
    {
        if let Some(workspace) = self.workspace() {
            task.detach_and_notify_err(workspace.downgrade(), window, cx);
        } else {
            task.detach_and_log_err(cx);
        }
    }

    pub fn title<'a>(&self, cx: &'a App) -> Cow<'a, str> {
        self.buffer().read(cx).title(cx)
    }

    pub fn snapshot(&self, window: &Window, cx: &mut App) -> EditorSnapshot {
        let display_snapshot = self.display_map.update(cx, |map, cx| map.snapshot(cx));

        EditorSnapshot {
            mode: self.mode.clone(),
            show_gutter: self.show_gutter,
            offset_content: self.offset_content,
            show_line_numbers: self.show_line_numbers,
            show_git_diff_gutter: self.show_git_diff_gutter,
            semantic_tokens_enabled: self.semantic_token_state.enabled(),
            scroll_anchor: self.scroll_manager.shared_scroll_anchor(cx),
            display_snapshot,
            placeholder_display_snapshot: self
                .placeholder_display_map
                .as_ref()
                .map(|display_map| display_map.update(cx, |map, cx| map.snapshot(cx))),
            ongoing_scroll: self.scroll_manager.ongoing_scroll(),
            is_focused: self.focus_handle.is_focused(window),
            current_line_highlight: self
                .current_line_highlight
                .unwrap_or_else(|| EditorSettings::get_global(cx).current_line_highlight),
            gutter_hovered: self.gutter_hovered,
        }
    }

    pub fn language_at<T: ToOffset>(&self, point: T, cx: &App) -> Option<Arc<Language>> {
        self.buffer.read(cx).language_at(point, cx)
    }

    pub fn file_at<T: ToOffset>(&self, point: T, cx: &App) -> Option<Arc<dyn language::File>> {
        self.buffer.read(cx).read(cx).file_at(point).cloned()
    }

    pub fn active_buffer(&self, cx: &App) -> Option<Entity<Buffer>> {
        let multibuffer = self.buffer.read(cx);
        let snapshot = multibuffer.snapshot(cx);
        let (anchor, _) =
            snapshot.anchor_to_buffer_anchor(self.selections.newest_anchor().head())?;
        multibuffer.buffer(anchor.buffer_id)
    }

    pub fn mode(&self) -> &EditorMode {
        &self.mode
    }

    pub fn set_mode(&mut self, mode: EditorMode) {
        self.mode = mode;
    }

    pub fn set_custom_context_menu(
        &mut self,
        f: impl 'static
        + Fn(
            &mut Self,
            DisplayPoint,
            &mut Window,
            &mut Context<Self>,
        ) -> Option<Entity<ui::ContextMenu>>,
    ) {
        self.custom_context_menu = Some(Box::new(f))
    }

    pub fn semantics_provider(&self) -> Option<Rc<dyn SemanticsProvider>> {
        self.semantics_provider.clone()
    }

    pub fn set_semantics_provider(&mut self, provider: Option<Rc<dyn SemanticsProvider>>) {
        self.semantics_provider = provider;
    }

    pub fn placeholder_text(&self, cx: &mut App) -> Option<String> {
        self.placeholder_display_map
            .as_ref()
            .map(|display_map| display_map.update(cx, |map, cx| map.snapshot(cx)).text())
    }

    pub fn set_placeholder_text(
        &mut self,
        placeholder_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let multibuffer = cx
            .new(|cx| MultiBuffer::singleton(cx.new(|cx| Buffer::local(placeholder_text, cx)), cx));

        let style = window.text_style();

        self.placeholder_display_map = Some(cx.new(|cx| {
            DisplayMap::new(
                multibuffer,
                style.font(),
                style.font_size.to_pixels(window.rem_size()),
                None,
                FILE_HEADER_HEIGHT,
                MULTI_BUFFER_EXCERPT_HEADER_HEIGHT,
                Default::default(),
                cx,
            )
        }));
        cx.notify();
    }

    pub fn set_cursor_shape(&mut self, cursor_shape: CursorShape, cx: &mut Context<Self>) {
        self.cursor_shape = cursor_shape;

        // Disrupt blink for immediate user feedback that the cursor shape has changed
        self.blink_manager.update(cx, BlinkManager::show_cursor);

        cx.notify();
    }

    pub fn show_cursor(&mut self, cx: &mut Context<Self>) {
        self.blink_manager.update(cx, BlinkManager::show_cursor);
    }

    pub fn cursor_shape(&self) -> CursorShape {
        self.cursor_shape
    }

    pub fn set_cursor_offset_on_selection(&mut self, set_cursor_offset_on_selection: bool) {
        self.cursor_offset_on_selection = set_cursor_offset_on_selection;
    }

    pub fn set_current_line_highlight(
        &mut self,
        current_line_highlight: Option<CurrentLineHighlight>,
    ) {
        self.current_line_highlight = current_line_highlight;
    }

    pub fn set_collapse_matches(&mut self, collapse_matches: bool) {
        self.collapse_matches = collapse_matches;
    }

    pub fn range_for_match<T: std::marker::Copy>(&self, range: &Range<T>) -> Range<T> {
        if self.collapse_matches {
            return range.start..range.start;
        }
        range.clone()
    }

    pub fn clip_at_line_ends(&mut self, cx: &mut Context<Self>) -> bool {
        self.display_map.read(cx).clip_at_line_ends
    }

    pub fn set_clip_at_line_ends(&mut self, clip: bool, cx: &mut Context<Self>) {
        if self.display_map.read(cx).clip_at_line_ends != clip {
            self.display_map
                .update(cx, |map, _| map.clip_at_line_ends = clip);
        }
    }

    pub fn capability(&self, cx: &App) -> Capability {
        if self.read_only {
            Capability::ReadOnly
        } else {
            self.buffer.read(cx).capability()
        }
    }

    pub fn read_only(&self, cx: &App) -> bool {
        self.read_only || self.buffer.read(cx).read_only()
    }

    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    pub fn set_use_selection_highlight(&mut self, highlight: bool) {
        self.use_selection_highlight = highlight;
    }

    pub fn set_should_serialize(&mut self, should_serialize: bool, cx: &App) {
        self.buffer_serialization = should_serialize.then(|| {
            BufferSerialization::new(
                ProjectSettings::get_global(cx)
                    .session
                    .restore_unsaved_buffers,
            )
        })
    }

    fn should_serialize_buffer(&self) -> bool {
        self.buffer_serialization.is_some()
    }

    pub fn set_use_modal_editing(&mut self, to: bool) {
        self.use_modal_editing = to;
    }

    pub fn use_modal_editing(&self) -> bool {
        self.use_modal_editing
    }

    /// Inserted text is normalized to LF line endings before being applied.
    /// Normalize before measuring inserted text for post-edit offsets.
    pub fn edit<I, S, T>(&mut self, edits: I, cx: &mut Context<Self>)
    where
        I: IntoIterator<Item = (Range<S>, T)>,
        S: ToOffset,
        T: Into<Arc<str>>,
    {
        if self.read_only(cx) {
            return;
        }

        self.buffer
            .update(cx, |buffer, cx| buffer.edit(edits, None, cx));
    }

    pub fn edit_with_autoindent<I, S, T>(&mut self, edits: I, cx: &mut Context<Self>)
    where
        I: IntoIterator<Item = (Range<S>, T)>,
        S: ToOffset,
        T: Into<Arc<str>>,
    {
        if self.read_only(cx) {
            return;
        }

        self.buffer.update(cx, |buffer, cx| {
            buffer.edit(edits, self.autoindent_mode.clone(), cx)
        });
    }

    pub fn edit_with_block_indent<I, S, T>(
        &mut self,
        edits: I,
        original_indent_columns: Vec<Option<u32>>,
        cx: &mut Context<Self>,
    ) where
        I: IntoIterator<Item = (Range<S>, T)>,
        S: ToOffset,
        T: Into<Arc<str>>,
    {
        if self.read_only(cx) {
            return;
        }

        self.buffer.update(cx, |buffer, cx| {
            buffer.edit(
                edits,
                Some(AutoindentMode::Block {
                    original_indent_columns,
                }),
                cx,
            )
        });
    }

    pub fn cancel(&mut self, _: &Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.selection_mark_mode = false;
        self.selection_drag_state = SelectionDragState::None;

        if self.dismiss_menus_and_popups(true, window, cx) {
            cx.notify();
            return;
        }

        if self.mode.is_full()
            && self.change_selections(Default::default(), window, cx, |s| s.try_cancel())
        {
            cx.notify();
            return;
        }

        cx.propagate();
    }

    pub fn dismiss_menus_and_popups(
        &mut self,
        _is_user_requested: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut dismissed = false;
        dismissed |= self.take_rename(false, window, cx).is_some();
        dismissed |= self.mouse_context_menu.take().is_some();
        dismissed
    }

    fn open_transaction_for_hidden_buffers(
        workspace: Entity<Workspace>,
        transaction: ProjectTransaction,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if transaction.0.is_empty() {
            return;
        }

        let edited_buffers_already_open = {
            let other_editors: Vec<Entity<Editor>> = workspace
                .read(cx)
                .panes()
                .iter()
                .flat_map(|pane| pane.read(cx).items_of_type::<Editor>())
                .filter(|editor| editor.entity_id() != cx.entity_id())
                .collect();

            transaction.0.keys().all(|buffer| {
                other_editors.iter().any(|editor| {
                    let multi_buffer = editor.read(cx).buffer();
                    let singleton = multi_buffer.read(cx).as_singleton();
                    singleton.entity_id() == buffer.entity_id()
                })
            })
        };
        if !edited_buffers_already_open {
            let workspace = workspace.downgrade();
            cx.defer_in(window, move |_, window, cx| {
                cx.spawn_in(window, async move |editor, cx| {
                    Self::open_project_transaction(&editor, workspace, transaction, title, cx)
                        .await
                        .ok()
                })
                .detach();
            });
        }
    }

    pub async fn open_project_transaction(
        editor: &WeakEntity<Editor>,
        workspace: WeakEntity<Workspace>,
        transaction: ProjectTransaction,
        title: String,
        cx: &mut AsyncWindowContext,
    ) -> Result<()> {
        let mut entries = transaction.0.into_iter().collect::<Vec<_>>();
        cx.update(|_, cx| {
            entries.sort_unstable_by_key(|(buffer, _)| {
                buffer.read(cx).file().map(|f| f.path().clone())
            });
        })?;
        if entries.is_empty() {
            return Ok(());
        }

        // If the project transaction's edits are all contained within this editor, then
        // avoid opening a new editor to display them.

        if let [(buffer, transaction)] = &*entries {
            let cursor_excerpt = editor.update(cx, |editor, cx| {
                let snapshot = editor.buffer().read(cx).snapshot(cx);
                let head = editor.selections.newest_anchor().head();
                let (buffer_snapshot, excerpt_range) = snapshot.excerpt_containing(head..head)?;
                if buffer_snapshot.remote_id() != buffer.read(cx).remote_id() {
                    return None;
                }
                Some(excerpt_range)
            })?;

            if let Some(excerpt_range) = cursor_excerpt {
                let all_edits_within_excerpt = buffer.read_with(cx, |buffer, _| {
                    let excerpt_range = excerpt_range.context.to_offset(buffer);
                    buffer
                        .edited_ranges_for_transaction::<usize>(transaction)
                        .all(|range| {
                            excerpt_range.start <= range.start && excerpt_range.end >= range.end
                        })
                });

                if all_edits_within_excerpt {
                    return Ok(());
                }
            }
        }

        let mut ranges_to_highlight = Vec::new();
        let excerpt_buffer = cx.new(|cx| {
            let mut multibuffer = MultiBuffer::new(Capability::ReadWrite).with_title(title);
            for (buffer_handle, transaction) in &entries {
                let edited_ranges = buffer_handle
                    .read(cx)
                    .edited_ranges_for_transaction::<Point>(transaction)
                    .collect::<Vec<_>>();
                multibuffer.set_excerpts_for_path(
                    PathKey::for_buffer(buffer_handle, cx),
                    buffer_handle.clone(),
                    edited_ranges.clone(),
                    cx,
                );
                let snapshot = multibuffer.snapshot(cx);
                let buffer_snapshot = buffer_handle.read(cx).snapshot();
                ranges_to_highlight.extend(edited_ranges.into_iter().filter_map(|range| {
                    let text_range = buffer_snapshot.anchor_range_inside(range);
                    let start = snapshot.anchor_in_buffer(text_range.start)?;
                    let end = snapshot.anchor_in_buffer(text_range.end)?;
                    Some(start..end)
                }));
            }
            multibuffer.push_transaction(entries.iter().map(|(b, t)| (b, t)), cx);
            multibuffer
        });

        workspace.update_in(cx, |workspace, window, cx| {
            let project = workspace.project().clone();
            let editor =
                cx.new(|cx| Editor::for_multibuffer(excerpt_buffer, Some(project), window, cx));
            workspace.add_item_to_active_pane(Box::new(editor.clone()), None, true, window, cx);
            editor.update(cx, |editor, cx| {
                editor.highlight_background(
                    HighlightKey::Editor,
                    &ranges_to_highlight,
                    |_, theme| theme.colors().editor_highlighted_line_background,
                    cx,
                );
            });
        })?;

        Ok(())
    }

    pub fn has_mouse_context_menu(&self) -> bool {
        self.mouse_context_menu.is_some()
    }

    fn refresh_document_highlights(&mut self, cx: &mut Context<Self>) -> Option<()> {
        if self.pending_rename.is_some() {
            return None;
        }

        let provider = self.semantics_provider.clone()?;
        let buffer = self.buffer.read(cx);
        let newest_selection = self.selections.newest_anchor().clone();
        let cursor_position = newest_selection.head();
        let (cursor_buffer, cursor_buffer_position) =
            buffer.text_anchor_for_position(cursor_position, cx)?;
        let (tail_buffer, tail_buffer_position) =
            buffer.text_anchor_for_position(newest_selection.tail(), cx)?;
        if cursor_buffer != tail_buffer {
            return None;
        }

        let snapshot = cursor_buffer.read(cx).snapshot();
        let word_ranges = cx.background_spawn(async move {
            // this might look odd to put on the background thread, but
            // `surrounding_word` can be quite expensive as it calls into
            // tree-sitter language scopes
            let (start_word_range, _) = snapshot.surrounding_word(cursor_buffer_position);
            let (end_word_range, _) = snapshot.surrounding_word(tail_buffer_position);
            (start_word_range, end_word_range)
        });

        let debounce = EditorSettings::get_global(cx).lsp_highlight_debounce.0;
        self.document_highlights_task = Some(cx.spawn(async move |this, cx| {
            let (start_word_range, end_word_range) = word_ranges.await;
            if start_word_range != end_word_range {
                this.update(cx, |this, cx| {
                    this.document_highlights_task.take();
                    this.clear_background_highlights(HighlightKey::DocumentHighlightRead, cx);
                    this.clear_background_highlights(HighlightKey::DocumentHighlightWrite, cx);
                })
                .ok();
                return;
            }
            cx.background_executor()
                .timer(Duration::from_millis(debounce))
                .await;

            let highlights = if let Some(highlights) = cx.update(|cx| {
                provider.document_highlights(&cursor_buffer, cursor_buffer_position, cx)
            }) {
                highlights.await.log_err()
            } else {
                None
            };

            if let Some(highlights) = highlights {
                this.update(cx, |this, cx| {
                    if this.pending_rename.is_some() {
                        return;
                    }

                    let buffer = this.buffer.read(cx);
                    if buffer
                        .text_anchor_for_position(cursor_position, cx)
                        .is_none_or(|(buffer, _)| buffer != cursor_buffer)
                    {
                        return;
                    }

                    let mut write_ranges = Vec::new();
                    let mut read_ranges = Vec::new();
                    let multibuffer_snapshot = buffer.snapshot(cx);
                    for highlight in highlights {
                        for range in
                            multibuffer_snapshot.buffer_range_to_excerpt_ranges(highlight.range)
                        {
                            if highlight.kind == lsp::DocumentHighlightKind::WRITE {
                                write_ranges.push(range);
                            } else {
                                read_ranges.push(range);
                            }
                        }
                    }

                    this.highlight_background(
                        HighlightKey::DocumentHighlightRead,
                        &read_ranges,
                        |_, theme| theme.colors().editor_document_highlight_read_background,
                        cx,
                    );
                    this.highlight_background(
                        HighlightKey::DocumentHighlightWrite,
                        &write_ranges,
                        |_, theme| theme.colors().editor_document_highlight_write_background,
                        cx,
                    );
                    cx.notify();
                })
                .log_err();
            }
        }));
        None
    }

    fn prepare_highlight_query_from_selection(
        &mut self,
        snapshot: &DisplaySnapshot,
        cx: &mut Context<Editor>,
    ) -> Option<(String, Range<Anchor>)> {
        if matches!(self.mode, EditorMode::SingleLine) {
            return None;
        }
        if !self.use_selection_highlight || !EditorSettings::get_global(cx).selection_highlight {
            return None;
        }
        // When the current selection was set by search navigation, suppress selection
        // occurrence highlights to avoid confusing non-matching occurrences with actual
        // search results (e.g. `^something` matches 3 line-start occurrences, but a
        // literal highlight would also mark a mid-line "something" that never matched
        // the regex). A manual selection made by the user clears this flag, restoring
        // the normal occurrence-highlight behavior.
        if self.last_selection_from_search
            && self.has_background_highlights(HighlightKey::BufferSearchHighlights)
        {
            return None;
        }
        if self.selections.count() != 1 || self.selections.line_mode() {
            return None;
        }
        let selection = self.selections.newest::<Point>(&snapshot);
        // If the selection spans multiple rows OR it is empty
        if selection.start.row != selection.end.row
            || selection.start.column == selection.end.column
        {
            return None;
        }
        let selection_anchor_range = selection.range().to_anchors(snapshot.buffer_snapshot());
        let query = snapshot
            .buffer_snapshot()
            .text_for_range(selection_anchor_range.clone())
            .collect::<String>();
        if query.trim().is_empty() {
            return None;
        }
        Some((query, selection_anchor_range))
    }
    fn update_selection_occurrence_highlights(
        &mut self,
        multi_buffer_snapshot: MultiBufferSnapshot,
        query_text: String,
        query_range: Range<Anchor>,
        multi_buffer_range_to_query: Range<Point>,
        use_debounce: bool,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> Task<()> {
        cx.spawn_in(window, async move |editor, cx| {
            if use_debounce {
                cx.background_executor()
                    .timer(SELECTION_HIGHLIGHT_DEBOUNCE_TIMEOUT)
                    .await;
            }
            let match_task = cx.background_spawn(async move {
                let buffer_ranges = multi_buffer_snapshot
                    .range_to_buffer_ranges(
                        multi_buffer_range_to_query.start..multi_buffer_range_to_query.end,
                    )
                    .into_iter()
                    .filter(|(_, excerpt_visible_range, _)| !excerpt_visible_range.is_empty());
                let mut match_ranges = Vec::new();
                let Ok(regex) = project::search::SearchQuery::text(
                    query_text,
                    false,
                    false,
                    false,
                    Default::default(),
                    Default::default(),
                    false,
                    None,
                ) else {
                    return Vec::default();
                };
                let query_range = query_range.to_anchors(&multi_buffer_snapshot);
                for (buffer_snapshot, search_range, _) in buffer_ranges {
                    match_ranges.extend(
                        regex
                            .search(
                                &buffer_snapshot,
                                Some(search_range.start.0..search_range.end.0),
                            )
                            .await
                            .into_iter()
                            .filter_map(|match_range| {
                                let match_start = buffer_snapshot
                                    .anchor_after(search_range.start + match_range.start);
                                let match_end = buffer_snapshot
                                    .anchor_before(search_range.start + match_range.end);
                                {
                                    let range = multi_buffer_snapshot
                                        .anchor_in_buffer(match_start)?
                                        ..multi_buffer_snapshot.anchor_in_buffer(match_end)?;
                                    Some(range).filter(|match_anchor_range| {
                                        match_anchor_range != &query_range
                                    })
                                }
                            }),
                    );
                }
                match_ranges
            });
            let match_ranges = match_task.await;
            editor
                .update_in(cx, |editor, _, cx| {
                    if use_debounce {
                        editor.clear_background_highlights(HighlightKey::SelectedTextHighlight, cx);
                        editor.debounced_selection_highlight_complete = true;
                    } else if editor.debounced_selection_highlight_complete {
                        return;
                    }
                    if !match_ranges.is_empty() {
                        editor.highlight_background(
                            HighlightKey::SelectedTextHighlight,
                            &match_ranges,
                            |_, theme| theme.colors().editor_document_highlight_bracket_background,
                            cx,
                        )
                    }
                })
                .log_err();
        })
    }
    fn refresh_outline_symbols_at_cursor(&mut self, cx: &mut Context<Editor>) {
        if !self.lsp_data_enabled() {
            return;
        }
        let cursor = self.selections.newest_anchor().head();
        let multi_buffer_snapshot = self.buffer().read(cx).snapshot(cx);

        if self.uses_lsp_document_symbols(cursor, &multi_buffer_snapshot, cx) {
            self.outline_symbols_at_cursor =
                self.lsp_symbols_at_cursor(cursor, &multi_buffer_snapshot, cx);
            cx.emit(EditorEvent::OutlineSymbolsChanged);
            cx.notify();
        } else {
            let syntax = cx.theme().syntax().clone();
            let background_task = cx.background_spawn(async move {
                multi_buffer_snapshot.symbols_containing(cursor, Some(&syntax))
            });
            self.refresh_outline_symbols_at_cursor_at_cursor_task =
                cx.spawn(async move |this, cx| {
                    let symbols = background_task.await;
                    this.update(cx, |this, cx| {
                        this.outline_symbols_at_cursor = symbols;
                        cx.emit(EditorEvent::OutlineSymbolsChanged);
                        cx.notify();
                    })
                    .ok();
                });
        }
    }
    fn refresh_selected_text_highlights(
        &mut self,
        snapshot: &DisplaySnapshot,
        on_buffer_edit: bool,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) {
        let Some((query_text, query_range)) =
            self.prepare_highlight_query_from_selection(snapshot, cx)
        else {
            self.clear_background_highlights(HighlightKey::SelectedTextHighlight, cx);
            self.quick_selection_highlight_task.take();
            self.debounced_selection_highlight_task.take();
            self.debounced_selection_highlight_complete = false;
            return;
        };
        let display_snapshot = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let multi_buffer_snapshot = self.buffer().read(cx).snapshot(cx);
        let query_changed = self
            .quick_selection_highlight_task
            .as_ref()
            .is_none_or(|(prev_anchor_range, _)| prev_anchor_range != &query_range);
        if query_changed {
            self.debounced_selection_highlight_complete = false;
        }
        if on_buffer_edit || query_changed {
            self.quick_selection_highlight_task = Some((
                query_range.clone(),
                self.update_selection_occurrence_highlights(
                    snapshot.buffer.clone(),
                    query_text.clone(),
                    query_range.clone(),
                    self.multi_buffer_visible_range(&display_snapshot, cx),
                    false,
                    window,
                    cx,
                ),
            ));
        }
        if on_buffer_edit
            || self
                .debounced_selection_highlight_task
                .as_ref()
                .is_none_or(|(prev_anchor_range, _)| prev_anchor_range != &query_range)
        {
            let multi_buffer_start = multi_buffer_snapshot
                .anchor_before(MultiBufferOffset(0))
                .to_point(&multi_buffer_snapshot);
            let multi_buffer_end = multi_buffer_snapshot
                .anchor_after(multi_buffer_snapshot.len())
                .to_point(&multi_buffer_snapshot);
            let multi_buffer_full_range = multi_buffer_start..multi_buffer_end;
            self.debounced_selection_highlight_task = Some((
                query_range.clone(),
                self.update_selection_occurrence_highlights(
                    snapshot.buffer.clone(),
                    query_text,
                    query_range,
                    multi_buffer_full_range,
                    true,
                    window,
                    cx,
                ),
            ));
        }
    }

    pub fn multi_buffer_visible_range(
        &self,
        display_snapshot: &DisplaySnapshot,
        cx: &App,
    ) -> Range<Point> {
        let visible_start = self
            .scroll_manager
            .native_anchor(display_snapshot, cx)
            .anchor
            .to_point(display_snapshot.buffer_snapshot())
            .to_display_point(display_snapshot);

        let mut target_end = visible_start;
        *target_end.row_mut() += self.visible_line_count().unwrap_or(0.).ceil() as u32;

        visible_start.to_point(display_snapshot)
            ..display_snapshot
                .clip_point(target_end, Bias::Right)
                .to_point(display_snapshot)
    }

    pub fn display_cursor_names(
        &mut self,
        _: &DisplayCursorNames,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_cursor_names(window, cx);
    }

    fn show_cursor_names(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_cursor_names = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(CURSORS_VISIBLE_FOR).await;
            this.update(cx, |this, cx| {
                this.show_cursor_names = false;
                cx.notify()
            })
            .ok()
        })
        .detach();
    }

    fn handle_modifiers_changed(
        &mut self,
        modifiers: Modifiers,
        position_map: &PositionMap,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_selection_mode(&modifiers, position_map, window, cx);

        let mouse_position = window.mouse_position();
        if !position_map.text_hitbox.is_hovered(window) {
            if self.gutter_hover_button.0.is_some() {
                cx.notify();
            }
            return;
        }

        self.update_hovered_link(
            position_map.point_for_position(mouse_position),
            Some(mouse_position),
            &position_map.snapshot,
            modifiers,
            window,
            cx,
        )
    }

    fn update_selection_mode(
        &mut self,
        modifiers: &Modifiers,
        position_map: &PositionMap,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // if the user clicks, drags, *then* presses alt, switch to columnar mode
        if modifiers.alt && self.selections.pending_anchor().is_some() {
            let mouse_position = window.mouse_position();
            let point_for_position = position_map.point_for_position(mouse_position);
            let position = point_for_position.previous_valid;

            self.select(
                SelectPhase::BeginColumnar {
                    position,
                    display_point: None,
                    reset: false,
                    goal_column: point_for_position.exact_unclipped.column(),
                },
                window,
                cx,
            );
        }
    }

    fn current_user_player_color(&self, cx: &mut App) -> PlayerColor {
        if self.read_only(cx) {
            cx.theme().players().read_only()
        } else {
            self.style.as_ref().unwrap().local_player
        }
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.transact(window, cx, |this, window, cx| {
            this.select_all(&SelectAll, window, cx);
            this.insert("", window, cx);
        });
    }

    pub fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }

        self.transact(window, cx, |this, window, cx| {
            let display_map = this.display_map.update(cx, |map, cx| map.snapshot(cx));
            let mut selections = this.selections.all::<MultiBufferPoint>(&display_map);
            for selection in &mut selections {
                if selection.is_empty() {
                    let old_head = selection.head();
                    let mut new_head =
                        movement::left(&display_map, old_head.to_display_point(&display_map))
                            .to_point(&display_map);
                    if let Some((buffer, line_buffer_range)) = display_map
                        .buffer_snapshot()
                        .buffer_line_for_row(MultiBufferRow(old_head.row))
                    {
                        let indent_size = buffer.indent_size_for_line(line_buffer_range.start.row);
                        let indent_len = match indent_size.kind {
                            IndentKind::Space => this.tab_size(cx),
                            IndentKind::Tab => 1,
                        };
                        if old_head.column <= indent_size.len && old_head.column > 0 {
                            new_head = cmp::min(
                                new_head,
                                MultiBufferPoint::new(
                                    old_head.row,
                                    ((old_head.column - 1) / indent_len) * indent_len,
                                ),
                            );
                        }
                    }

                    selection.set_head(new_head, SelectionGoal::None);
                }
            }

            this.change_selections(Default::default(), window, cx, |s| s.select(selections));
            this.insert("", window, cx);
        });
    }

    pub fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        self.transact(window, cx, |this, window, cx| {
            this.change_selections(Default::default(), window, cx, |s| {
                s.move_with(&mut |map, selection| {
                    if selection.is_empty() {
                        let cursor = movement::right(map, selection.head());
                        selection.end = cursor;
                        selection.reversed = true;
                        selection.goal = SelectionGoal::None;
                    }
                })
            });
            this.insert("", window, cx);
        });
    }

    pub fn insert_tab(&mut self, _: &InsertTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        self.handle_input("\t", window, cx);
    }

    pub fn indent(&mut self, _: &Indent, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        if self.mode.is_single_line() {
            cx.propagate();
            return;
        }

        let mut selections = self.selections.all::<Point>(&self.display_snapshot(cx));
        let mut prev_edited_row = 0;
        let mut row_delta = 0;
        let mut edits = Vec::new();
        let buffer = self.buffer.read(cx);
        let snapshot = buffer.snapshot(cx);
        for selection in &mut selections {
            if selection.start.row != prev_edited_row {
                row_delta = 0;
            }
            prev_edited_row = selection.end.row;

            row_delta =
                Self::indent_selection(buffer, &snapshot, selection, &mut edits, row_delta, cx);
        }

        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |b, cx| b.edit(edits, None, cx));
            this.change_selections(Default::default(), window, cx, |s| s.select(selections));
        });
    }

    fn indent_selection(
        buffer: &MultiBuffer,
        snapshot: &MultiBufferSnapshot,
        selection: &mut Selection<Point>,
        edits: &mut Vec<(Range<Point>, String)>,
        delta_for_start_row: u32,
        cx: &App,
    ) -> u32 {
        let tab_size = buffer.tab_size(cx).get();
        let indent_kind = if buffer.hard_tabs(cx) {
            IndentKind::Tab
        } else {
            IndentKind::Space
        };
        let mut start_row = selection.start.row;
        let mut end_row = selection.end.row + 1;

        // If a selection ends at the beginning of a line, don't indent
        // that last line.
        if selection.end.column == 0 && selection.end.row > selection.start.row {
            end_row -= 1;
        }

        // Avoid re-indenting a row that has already been indented by a
        // previous selection, but still update this selection's column
        // to reflect that indentation.
        if delta_for_start_row > 0 {
            start_row += 1;
            selection.start.column += delta_for_start_row;
            if selection.end.row == selection.start.row {
                selection.end.column += delta_for_start_row;
            }
        }

        let mut delta_for_end_row = 0;
        let has_multiple_rows = start_row + 1 != end_row;
        for row in start_row..end_row {
            let current_indent = snapshot.indent_size_for_line(MultiBufferRow(row));
            let indent_delta = match (current_indent.kind, indent_kind) {
                (IndentKind::Space, IndentKind::Space) => {
                    let columns_to_next_tab_stop = tab_size - (current_indent.len % tab_size);
                    IndentSize::spaces(columns_to_next_tab_stop)
                }
                (IndentKind::Tab, IndentKind::Space) => IndentSize::spaces(tab_size),
                (_, IndentKind::Tab) => IndentSize::tab(),
            };

            let start = if has_multiple_rows || current_indent.len < selection.start.column {
                0
            } else {
                selection.start.column
            };
            let row_start = Point::new(row, start);
            edits.push((
                row_start..row_start,
                indent_delta.chars().collect::<String>(),
            ));

            // Update this selection's endpoints to reflect the indentation.
            if row == selection.start.row {
                selection.start.column += indent_delta.len;
            }
            if row == selection.end.row {
                selection.end.column += indent_delta.len;
                delta_for_end_row = indent_delta.len;
            }
        }

        if selection.start.row == selection.end.row {
            delta_for_start_row + delta_for_end_row
        } else {
            delta_for_end_row
        }
    }

    pub fn outdent(&mut self, _: &Outdent, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        if self.mode.is_single_line() {
            cx.propagate();
            return;
        }

        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let selections = self.selections.all::<Point>(&display_map);
        let mut deletion_ranges = Vec::new();
        let mut last_outdent = None;
        {
            let buffer = self.buffer.read(cx);
            let snapshot = buffer.snapshot(cx);
            for selection in &selections {
                let tab_size = buffer.tab_size(cx);
                let mut rows = selection.spanned_rows(false, &display_map);

                // Avoid re-outdenting a row that has already been outdented by a
                // previous selection.
                if let Some(last_row) = last_outdent
                    && last_row == rows.start
                {
                    rows.start = rows.start.next_row();
                }
                let has_multiple_rows = rows.len() > 1;
                for row in rows.iter_rows() {
                    let indent_size = snapshot.indent_size_for_line(row);
                    if indent_size.len > 0 {
                        let deletion_len = indent_size.outdent_len(tab_size);
                        let start = if has_multiple_rows
                            || deletion_len > selection.start.column
                            || indent_size.len < selection.start.column
                        {
                            0
                        } else {
                            selection.start.column - deletion_len
                        };
                        deletion_ranges.push(
                            Point::new(row.0, start)..Point::new(row.0, start + deletion_len),
                        );
                        last_outdent = Some(row);
                    }
                }
            }
        }

        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |buffer, cx| {
                let empty_str: Arc<str> = Arc::default();
                buffer.edit(
                    deletion_ranges
                        .into_iter()
                        .map(|range| (range, empty_str.clone())),
                    None,
                    cx,
                );
            });
            let selections = this
                .selections
                .all::<MultiBufferOffset>(&this.display_snapshot(cx));
            this.change_selections(Default::default(), window, cx, |s| s.select(selections));
        });
    }

    pub fn autoindent(&mut self, _: &AutoIndent, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        if self.mode.is_single_line() {
            cx.propagate();
            return;
        }

        let selections = self
            .selections
            .all::<MultiBufferOffset>(&self.display_snapshot(cx))
            .into_iter()
            .map(|s| s.range());

        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |buffer, cx| {
                buffer.autoindent_ranges(selections, cx);
            });
            let selections = this
                .selections
                .all::<MultiBufferOffset>(&this.display_snapshot(cx));
            this.change_selections(Default::default(), window, cx, |s| s.select(selections));
        });
    }

    pub fn delete_line(&mut self, _: &DeleteLine, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let selections = self.selections.all::<Point>(&display_map);

        let mut new_cursors = Vec::new();
        let mut edit_ranges = Vec::new();
        let mut selections = selections.iter().peekable();
        while let Some(selection) = selections.next() {
            let mut rows = selection.spanned_rows(false, &display_map);

            // Accumulate contiguous regions of rows that we want to delete.
            while let Some(next_selection) = selections.peek() {
                let next_rows = next_selection.spanned_rows(false, &display_map);
                if next_rows.start <= rows.end {
                    rows.end = next_rows.end;
                    selections.next().unwrap();
                } else {
                    break;
                }
            }

            let buffer = display_map.buffer_snapshot();
            let mut edit_start = ToOffset::to_offset(&Point::new(rows.start.0, 0), buffer);
            let (edit_end, target_row) = if buffer.max_point().row >= rows.end.0 {
                // If there's a line after the range, delete the \n from the end of the row range
                (
                    ToOffset::to_offset(&Point::new(rows.end.0, 0), buffer),
                    rows.end,
                )
            } else {
                // If there isn't a line after the range, delete the \n from the line before the
                // start of the row range
                edit_start = edit_start.saturating_sub_usize(1);
                (buffer.len(), rows.start.previous_row())
            };

            let text_layout_details = self.text_layout_details(window, cx);
            let x = display_map.x_for_display_point(
                selection.head().to_display_point(&display_map),
                &text_layout_details,
            );
            let row = Point::new(target_row.0, 0)
                .to_display_point(&display_map)
                .row();
            let column = display_map.display_column_for_x(row, x, &text_layout_details);

            new_cursors.push((
                selection.id,
                buffer.anchor_after(DisplayPoint::new(row, column).to_point(&display_map)),
                SelectionGoal::None,
            ));
            edit_ranges.push(edit_start..edit_end);
        }

        self.transact(window, cx, |this, window, cx| {
            let buffer = this.buffer.update(cx, |buffer, cx| {
                let empty_str: Arc<str> = Arc::default();
                buffer.edit(
                    edit_ranges
                        .into_iter()
                        .map(|range| (range, empty_str.clone())),
                    None,
                    cx,
                );
                buffer.snapshot(cx)
            });
            let new_selections = new_cursors
                .into_iter()
                .map(|(id, cursor, goal)| {
                    let cursor = cursor.to_point(&buffer);
                    Selection {
                        id,
                        start: cursor,
                        end: cursor,
                        reversed: false,
                        goal,
                    }
                })
                .collect();

            this.change_selections(Default::default(), window, cx, |s| {
                s.select(new_selections);
            });
        });
    }

    pub fn join_lines_impl(
        &mut self,
        insert_whitespace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }
        let mut row_ranges = Vec::<Range<MultiBufferRow>>::new();
        for selection in self.selections.all::<Point>(&self.display_snapshot(cx)) {
            let start = MultiBufferRow(selection.start.row);
            // Treat single line selections as if they include the next line. Otherwise this action
            // would do nothing for single line selections individual cursors.
            let end = if selection.start.row == selection.end.row {
                MultiBufferRow(selection.start.row + 1)
            } else if selection.end.column == 0 {
                // If the selection ends at the start of a line, it's logically at the end of the
                // previous line (plus its newline).
                // Don't include the end line unless there's only one line selected.
                if selection.start.row + 1 == selection.end.row {
                    MultiBufferRow(selection.end.row)
                } else {
                    MultiBufferRow(selection.end.row - 1)
                }
            } else {
                MultiBufferRow(selection.end.row)
            };

            if let Some(last_row_range) = row_ranges.last_mut()
                && start <= last_row_range.end
            {
                last_row_range.end = end;
                continue;
            }
            row_ranges.push(start..end);
        }

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let mut cursor_positions = Vec::new();
        for row_range in &row_ranges {
            let anchor = snapshot.anchor_before(Point::new(
                row_range.end.previous_row().0,
                snapshot.line_len(row_range.end.previous_row()),
            ));
            cursor_positions.push(anchor..anchor);
        }

        self.transact(window, cx, |this, window, cx| {
            for row_range in row_ranges.into_iter().rev() {
                for row in row_range.iter_rows().rev() {
                    let end_of_line = Point::new(row.0, snapshot.line_len(row));
                    let next_line_row = row.next_row();
                    let indent = snapshot.indent_size_for_line(next_line_row);
                    let mut join_start_column = indent.len;

                    if let Some(language_scope) =
                        snapshot.language_scope_at(Point::new(next_line_row.0, indent.len))
                    {
                        let line_end =
                            Point::new(next_line_row.0, snapshot.line_len(next_line_row));
                        let line_text_after_indent = snapshot
                            .text_for_range(Point::new(next_line_row.0, indent.len)..line_end)
                            .collect::<String>();

                        if !line_text_after_indent.is_empty() {
                            let block_prefix = language_scope
                                .block_comment()
                                .map(|c| c.prefix.as_ref())
                                .filter(|p| !p.is_empty());
                            let doc_prefix = language_scope
                                .documentation_comment()
                                .map(|c| c.prefix.as_ref())
                                .filter(|p| !p.is_empty());
                            let all_prefixes = language_scope
                                .line_comment_prefixes()
                                .iter()
                                .map(|p| p.as_ref())
                                .chain(block_prefix)
                                .chain(doc_prefix)
                                .chain(language_scope.unordered_list().iter().map(|p| p.as_ref()));

                            let mut longest_prefix_len = None;
                            for prefix in all_prefixes {
                                let trimmed = prefix.trim_end();
                                if line_text_after_indent.starts_with(trimmed) {
                                    let candidate_len =
                                        if line_text_after_indent.starts_with(prefix) {
                                            prefix.len()
                                        } else {
                                            trimmed.len()
                                        };
                                    if longest_prefix_len.map_or(true, |len| candidate_len > len) {
                                        longest_prefix_len = Some(candidate_len);
                                    }
                                }
                            }

                            if let Some(prefix_len) = longest_prefix_len {
                                join_start_column =
                                    join_start_column.saturating_add(prefix_len as u32);
                            }
                        }
                    }

                    let start_of_next_line = Point::new(next_line_row.0, join_start_column);

                    let replace = if snapshot.line_len(next_line_row) > join_start_column
                        && insert_whitespace
                    {
                        " "
                    } else {
                        ""
                    };

                    this.buffer.update(cx, |buffer, cx| {
                        buffer.edit([(end_of_line..start_of_next_line, replace)], None, cx)
                    });
                }
            }

            this.change_selections(Default::default(), window, cx, |s| {
                s.select_anchor_ranges(cursor_positions)
            });
        });
    }

    pub fn join_lines(&mut self, _: &JoinLines, window: &mut Window, cx: &mut Context<Self>) {
        self.join_lines_impl(true, window, cx);
    }

    pub fn sort_lines_case_sensitive(
        &mut self,
        _: &SortLinesCaseSensitive,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_immutable_lines(window, cx, |lines| lines.sort())
    }

    pub fn sort_lines_by_length(
        &mut self,
        _: &SortLinesByLength,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_immutable_lines(window, cx, |lines| {
            lines.sort_by_key(|&line| line.chars().count())
        })
    }

    pub fn sort_lines_case_insensitive(
        &mut self,
        _: &SortLinesCaseInsensitive,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_immutable_lines(window, cx, |lines| {
            lines.sort_by_key(|line| line.to_lowercase())
        })
    }

    pub fn unique_lines_case_insensitive(
        &mut self,
        _: &UniqueLinesCaseInsensitive,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_immutable_lines(window, cx, |lines| {
            let mut seen = HashSet::default();
            lines.retain(|line| seen.insert(line.to_lowercase()));
        })
    }

    pub fn unique_lines_case_sensitive(
        &mut self,
        _: &UniqueLinesCaseSensitive,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_immutable_lines(window, cx, |lines| {
            let mut seen = HashSet::default();
            lines.retain(|line| seen.insert(*line));
        })
    }

    fn enable_wrap_selections_in_tag(&self, cx: &App) -> bool {
        let snapshot = self.buffer.read(cx).snapshot(cx);
        for selection in self.selections.disjoint_anchors_arc().iter() {
            if snapshot
                .language_at(selection.start)
                .and_then(|lang| lang.config().wrap_characters.as_ref())
                .is_some()
            {
                return true;
            }
        }
        false
    }

    fn wrap_selections_in_tag(
        &mut self,
        _: &WrapSelectionsInTag,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }

        let snapshot = self.buffer.read(cx).snapshot(cx);

        let mut edits = Vec::new();
        let mut boundaries = Vec::new();

        for selection in self
            .selections
            .all_adjusted(&self.display_snapshot(cx))
            .iter()
        {
            let Some(wrap_config) = snapshot
                .language_at(selection.start)
                .and_then(|lang| lang.config().wrap_characters.clone())
            else {
                continue;
            };

            let open_tag = format!("{}{}", wrap_config.start_prefix, wrap_config.start_suffix);
            let close_tag = format!("{}{}", wrap_config.end_prefix, wrap_config.end_suffix);

            let start_before = snapshot.anchor_before(selection.start);
            let end_after = snapshot.anchor_after(selection.end);

            edits.push((start_before..start_before, open_tag));
            edits.push((end_after..end_after, close_tag));

            boundaries.push((
                start_before,
                end_after,
                wrap_config.start_prefix.len(),
                wrap_config.end_suffix.len(),
            ));
        }

        if edits.is_empty() {
            return;
        }

        self.transact(window, cx, |this, window, cx| {
            let buffer = this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
                buffer.snapshot(cx)
            });

            let mut new_selections = Vec::with_capacity(boundaries.len() * 2);
            for (start_before, end_after, start_prefix_len, end_suffix_len) in
                boundaries.into_iter()
            {
                let open_offset = start_before.to_offset(&buffer) + start_prefix_len;
                let close_offset = end_after
                    .to_offset(&buffer)
                    .saturating_sub_usize(end_suffix_len);
                new_selections.push(open_offset..open_offset);
                new_selections.push(close_offset..close_offset);
            }

            this.change_selections(Default::default(), window, cx, |s| {
                s.select_ranges(new_selections);
            });

            this.request_autoscroll(Autoscroll::fit(), cx);
        });
    }

    pub fn toggle_read_only(
        &mut self,
        _: &workspace::ToggleReadOnlyFile,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let buffer = self.buffer.read(cx).as_singleton();
        buffer.update(cx, |buffer, cx| {
            buffer.set_capability(
                match buffer.capability() {
                    Capability::ReadWrite => Capability::Read,
                    Capability::Read => Capability::ReadWrite,
                    Capability::ReadOnly => Capability::ReadOnly,
                },
                cx,
            );
        })
    }

    pub fn reload_file(&mut self, _: &ReloadFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.project.clone() else {
            return;
        };
        let task = self.reload(project, window, cx);
        self.detach_and_notify_err(task, window, cx);
    }

    pub fn align_selections(
        &mut self,
        _: &crate::actions::AlignSelections,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }

        let display_snapshot = self.display_snapshot(cx);

        struct CursorData {
            anchor: Anchor,
            point: Point,
        }
        let cursor_data: Vec<CursorData> = self
            .selections
            .disjoint_anchors()
            .iter()
            .map(|selection| {
                let anchor = if selection.reversed {
                    selection.head()
                } else {
                    selection.tail()
                };
                CursorData {
                    anchor: anchor,
                    point: anchor.to_point(&display_snapshot.buffer_snapshot()),
                }
            })
            .collect();

        let rows_anchors_count: Vec<usize> = cursor_data
            .iter()
            .map(|cursor| cursor.point.row)
            .chunk_by(|&row| row)
            .into_iter()
            .map(|(_, group)| group.count())
            .collect();
        let max_columns = rows_anchors_count.iter().max().copied().unwrap_or(0);
        let mut rows_column_offset = vec![0; rows_anchors_count.len()];
        let mut edits = Vec::new();

        for column_idx in 0..max_columns {
            let mut cursor_index = 0;

            // Calculate target_column => position that the selections will go
            let mut target_column = 0;
            for (row_idx, cursor_count) in rows_anchors_count.iter().enumerate() {
                // Skip rows that don't have this column
                if column_idx >= *cursor_count {
                    cursor_index += cursor_count;
                    continue;
                }

                let point = &cursor_data[cursor_index + column_idx].point;
                let adjusted_column = point.column + rows_column_offset[row_idx];
                if adjusted_column > target_column {
                    target_column = adjusted_column;
                }
                cursor_index += cursor_count;
            }

            // Collect edits for this column
            cursor_index = 0;
            for (row_idx, cursor_count) in rows_anchors_count.iter().enumerate() {
                // Skip rows that don't have this column
                if column_idx >= *cursor_count {
                    cursor_index += *cursor_count;
                    continue;
                }

                let point = &cursor_data[cursor_index + column_idx].point;
                let spaces_needed = target_column - point.column - rows_column_offset[row_idx];
                if spaces_needed > 0 {
                    let anchor = cursor_data[cursor_index + column_idx]
                        .anchor
                        .bias_left(&display_snapshot);
                    edits.push((anchor..anchor, " ".repeat(spaces_needed as usize)));
                }
                rows_column_offset[row_idx] += spaces_needed;

                cursor_index += *cursor_count;
            }
        }

        if !edits.is_empty() {
            self.transact(window, cx, |editor, _window, cx| {
                editor.edit(edits, cx);
            });
        }
    }

    pub fn reverse_lines(&mut self, _: &ReverseLines, window: &mut Window, cx: &mut Context<Self>) {
        self.manipulate_immutable_lines(window, cx, |lines| lines.reverse())
    }

    pub fn shuffle_lines(&mut self, _: &ShuffleLines, window: &mut Window, cx: &mut Context<Self>) {
        self.manipulate_immutable_lines(window, cx, |lines| lines.shuffle(&mut rand::rng()))
    }

    pub fn rotate_selections_forward(
        &mut self,
        _: &RotateSelectionsForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rotate_selections(window, cx, false)
    }

    pub fn rotate_selections_backward(
        &mut self,
        _: &RotateSelectionsBackward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rotate_selections(window, cx, true)
    }

    fn rotate_selections(&mut self, window: &mut Window, cx: &mut Context<Self>, reverse: bool) {
        if self.read_only(cx) {
            return;
        }
        let display_snapshot = self.display_snapshot(cx);
        let selections = self.selections.all::<MultiBufferOffset>(&display_snapshot);

        if selections.len() < 2 {
            return;
        }

        let (edits, new_selections) = {
            let buffer = self.buffer.read(cx).read(cx);
            let has_selections = selections.iter().any(|s| !s.is_empty());
            if has_selections {
                let mut selected_texts: Vec<String> = selections
                    .iter()
                    .map(|selection| {
                        buffer
                            .text_for_range(selection.start..selection.end)
                            .collect()
                    })
                    .collect();

                if reverse {
                    selected_texts.rotate_left(1);
                } else {
                    selected_texts.rotate_right(1);
                }

                let mut offset_delta: i64 = 0;
                let mut new_selections = Vec::new();
                let edits: Vec<_> = selections
                    .iter()
                    .zip(selected_texts.iter())
                    .map(|(selection, new_text)| {
                        let old_len = (selection.end.0 - selection.start.0) as i64;
                        let new_len = new_text.len() as i64;
                        let adjusted_start =
                            MultiBufferOffset((selection.start.0 as i64 + offset_delta) as usize);
                        let adjusted_end =
                            MultiBufferOffset((adjusted_start.0 as i64 + new_len) as usize);

                        new_selections.push(Selection {
                            id: selection.id,
                            start: adjusted_start,
                            end: adjusted_end,
                            reversed: selection.reversed,
                            goal: selection.goal,
                        });

                        offset_delta += new_len - old_len;
                        (selection.start..selection.end, new_text.clone())
                    })
                    .collect();
                (edits, new_selections)
            } else {
                let mut all_rows: Vec<u32> = selections
                    .iter()
                    .map(|selection| buffer.offset_to_point(selection.start).row)
                    .collect();
                all_rows.sort_unstable();
                all_rows.dedup();

                if all_rows.len() < 2 {
                    return;
                }

                let line_ranges: Vec<Range<MultiBufferOffset>> = all_rows
                    .iter()
                    .map(|&row| {
                        let start = Point::new(row, 0);
                        let end = Point::new(row, buffer.line_len(MultiBufferRow(row)));
                        buffer.point_to_offset(start)..buffer.point_to_offset(end)
                    })
                    .collect();

                let mut line_texts: Vec<String> = line_ranges
                    .iter()
                    .map(|range| buffer.text_for_range(range.clone()).collect())
                    .collect();

                if reverse {
                    line_texts.rotate_left(1);
                } else {
                    line_texts.rotate_right(1);
                }

                let edits = line_ranges
                    .iter()
                    .zip(line_texts.iter())
                    .map(|(range, new_text)| (range.clone(), new_text.clone()))
                    .collect();

                let num_rows = all_rows.len();
                let row_to_index: std::collections::HashMap<u32, usize> = all_rows
                    .iter()
                    .enumerate()
                    .map(|(i, &row)| (row, i))
                    .collect();

                // Compute new line start offsets after rotation (handles CRLF)
                let newline_len = line_ranges[1].start.0 - line_ranges[0].end.0;
                let first_line_start = line_ranges[0].start.0;
                let mut new_line_starts: Vec<usize> = vec![first_line_start];
                for text in line_texts.iter().take(num_rows - 1) {
                    let prev_start = *new_line_starts.last().unwrap();
                    new_line_starts.push(prev_start + text.len() + newline_len);
                }

                let new_selections = selections
                    .iter()
                    .map(|selection| {
                        let point = buffer.offset_to_point(selection.start);
                        let old_index = row_to_index[&point.row];
                        let new_index = if reverse {
                            (old_index + num_rows - 1) % num_rows
                        } else {
                            (old_index + 1) % num_rows
                        };
                        let new_offset =
                            MultiBufferOffset(new_line_starts[new_index] + point.column as usize);
                        Selection {
                            id: selection.id,
                            start: new_offset,
                            end: new_offset,
                            reversed: selection.reversed,
                            goal: selection.goal,
                        }
                    })
                    .collect();

                (edits, new_selections)
            }
        };

        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
            });
            this.change_selections(Default::default(), window, cx, |s| {
                s.select(new_selections);
            });
        });
    }

    fn manipulate_lines<M>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        mut manipulate: M,
    ) where
        M: FnMut(&str) -> LineManipulationResult,
    {
        if self.read_only(cx) {
            return;
        }

        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = self.buffer.read(cx).snapshot(cx);

        let mut edits = Vec::new();

        let selections = self.selections.all::<Point>(&display_map);
        let mut selections = selections.iter().peekable();
        let mut contiguous_row_selections = Vec::new();
        let mut new_selections = Vec::new();
        let mut added_lines = 0;
        let mut removed_lines = 0;

        while let Some(selection) = selections.next() {
            let (start_row, end_row) = consume_contiguous_rows(
                &mut contiguous_row_selections,
                selection,
                &display_map,
                &mut selections,
            );

            let start_point = Point::new(start_row.0, 0);
            let end_point = Point::new(
                end_row.previous_row().0,
                buffer.line_len(end_row.previous_row()),
            );
            let text = buffer
                .text_for_range(start_point..end_point)
                .collect::<String>();

            let LineManipulationResult {
                new_text,
                line_count_before,
                line_count_after,
            } = manipulate(&text);

            edits.push((start_point..end_point, new_text));

            // Selections must change based on added and removed line count
            let start_row =
                MultiBufferRow(start_point.row + added_lines as u32 - removed_lines as u32);
            let end_row = MultiBufferRow(start_row.0 + line_count_after.saturating_sub(1) as u32);
            new_selections.push(Selection {
                id: selection.id,
                start: start_row,
                end: end_row,
                goal: SelectionGoal::None,
                reversed: selection.reversed,
            });

            if line_count_after > line_count_before {
                added_lines += line_count_after - line_count_before;
            } else if line_count_before > line_count_after {
                removed_lines += line_count_before - line_count_after;
            }
        }

        self.transact(window, cx, |this, window, cx| {
            let buffer = this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
                buffer.snapshot(cx)
            });

            // Recalculate offsets on newly edited buffer
            let new_selections = new_selections
                .iter()
                .map(|s| {
                    let start_point = Point::new(s.start.0, 0);
                    let end_point = Point::new(s.end.0, buffer.line_len(s.end));
                    Selection {
                        id: s.id,
                        start: buffer.point_to_offset(start_point),
                        end: buffer.point_to_offset(end_point),
                        goal: s.goal,
                        reversed: s.reversed,
                    }
                })
                .collect();

            this.change_selections(Default::default(), window, cx, |s| {
                s.select(new_selections);
            });

            this.request_autoscroll(Autoscroll::fit(), cx);
        });
    }

    fn manipulate_immutable_lines<Fn>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        mut callback: Fn,
    ) where
        Fn: FnMut(&mut Vec<&str>),
    {
        self.manipulate_lines(window, cx, |text| {
            let mut lines: Vec<&str> = text.split('\n').collect();
            let line_count_before = lines.len();

            callback(&mut lines);

            LineManipulationResult {
                new_text: lines.join("\n"),
                line_count_before,
                line_count_after: lines.len(),
            }
        });
    }

    pub fn set_tab_size(&mut self, tab_size: NonZeroU32, cx: &mut Context<Self>) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.set_tab_size(tab_size, cx);
        });
    }

    pub fn tab_width_1(
        &mut self,
        _: &TabWidth1,
        _: &mut Window,
        cx: &mut Context<Self>
    ) {
        self.set_tab_size(NonZeroU32::new(1).unwrap(), cx);
    }

    pub fn tab_width_2(
        &mut self,
        _: &TabWidth2,
        _: &mut Window,
        cx: &mut Context<Self>
    ) {
        self.set_tab_size(NonZeroU32::new(2).unwrap(), cx);
    }

    pub fn tab_width_3(
        &mut self,
        _: &TabWidth3,
        _: &mut Window,
        cx: &mut Context<Self>
    ) {
        self.set_tab_size(NonZeroU32::new(3).unwrap(), cx);
    }

    pub fn tab_width_4(
        &mut self,
        _: &TabWidth4,
        _: &mut Window,
        cx: &mut Context<Self>
    ) {
        self.set_tab_size(NonZeroU32::new(4).unwrap(), cx);
    }

    pub fn tab_width_5(
        &mut self,
        _: &TabWidth5,
        _: &mut Window,
        cx: &mut Context<Self>
    ) {
        self.set_tab_size(NonZeroU32::new(5).unwrap(), cx);
    }

    pub fn tab_width_6(
        &mut self,
        _: &TabWidth6,
        _: &mut Window,
        cx: &mut Context<Self>
    ) {
        self.set_tab_size(NonZeroU32::new(6).unwrap(), cx);
    }

    pub fn tab_width_7(
        &mut self,
        _: &TabWidth7,
        _: &mut Window,
        cx: &mut Context<Self>
    ) {
        self.set_tab_size(NonZeroU32::new(7).unwrap(), cx);
    }

    pub fn tab_width_8(
        &mut self,
        _: &TabWidth8,
        _: &mut Window,
        cx: &mut Context<Self>
    ) {
        self.set_tab_size(NonZeroU32::new(8).unwrap(), cx);
    }

    pub fn use_tabs(
        &mut self,
        _: &UseTabs,
        _: &mut Window,
        cx: &mut Context<Self>
    ) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.set_hard_tabs(true, cx);
        });
    }

    pub fn use_spaces(
        &mut self,
        _: &UseSpaces,
        _: &mut Window,
        cx: &mut Context<Self>
    ) {
        self.buffer.update(cx, |buffer, cx| {
            buffer.set_hard_tabs(false, cx);
        });
    }

    pub fn tab_size(&self, cx: &App) -> u32 {
        self.buffer.read_with(cx, |mb, cx| mb.tab_size(cx).get())
    }

    pub fn hard_tabs(&self, cx: &App) -> bool {
        self.buffer.read_with(cx, |mb, cx| mb.hard_tabs(cx))
    }

    pub fn convert_indentation_to_spaces(
        &mut self,
        _: &ConvertIndentationToSpaces,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }

        self.use_spaces(&UseSpaces, window, cx);

        let tab_size = self.tab_size(cx) as usize;
        let buffer = self.buffer.read(cx).snapshot(cx);
        let max_point = buffer.max_point();

        let mut edits = Vec::new();

        for row in 0..=max_point.row {
            let row = MultiBufferRow(row);
            let line_len = buffer.line_len(row);

            if line_len == 0 {
                continue;
            }

            let line_start = Point::new(row.0, 0);
            let line_end = Point::new(row.0, line_len);
            let line = buffer.text_for_range(line_start..line_end).collect::<String>();

            let mut old_indent_len = 0;
            let mut col = 0usize;

            for ch in line.chars() {
                match ch {
                    ' ' => {
                        old_indent_len += 1;
                        col += 1;
                    }
                    '\t' => {
                        old_indent_len += 1;

                        let spaces_len = tab_size - (col % tab_size);
                        col += spaces_len;
                    }
                    _ => break,
                }
            }

            let old_indent = &line[..old_indent_len];
            let new_indent = " ".repeat(col);

            if old_indent != new_indent {
                edits.push((
                    Point::new(row.0, 0)..Point::new(row.0, old_indent_len as u32),
                    new_indent,
                ));
            }
        }

        if edits.is_empty() {
            return;
        }

        self.transact(window, cx, |this, _window, cx| {
            this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
            });
        });
    }

    pub fn convert_indentation_to_tabs(
        &mut self,
        _: &ConvertIndentationToTabs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }

        self.use_tabs(&UseTabs, window, cx);

        let tab_size = self.tab_size(cx) as usize;
        let buffer = self.buffer.read(cx).snapshot(cx);
        let max_point = buffer.max_point();

        let mut edits = Vec::new();

        for row in 0..=max_point.row {
            let row = MultiBufferRow(row);
            let line_len = buffer.line_len(row);

            if line_len == 0 {
                continue;
            }

            let line_start = Point::new(row.0, 0);
            let line_end = Point::new(row.0, line_len);
            let line = buffer.text_for_range(line_start..line_end).collect::<String>();

            let mut old_indent_len = 0;
            let mut col = 0usize;

            for ch in line.chars() {
                match ch {
                    ' ' => {
                        old_indent_len += 1;
                        col += 1;
                    }
                    '\t' => {
                        old_indent_len += 1;
                        col += tab_size - (col % tab_size);
                    }
                    _ => break,
                }
            }

            if old_indent_len == 0 {
                continue;
            }

            let tab_count = col / tab_size;
            let space_count = col % tab_size;

            let mut new_indent = String::with_capacity(tab_count + space_count);
            if tab_count > 0 {
                new_indent.push_str(&"\t".repeat(tab_count));
            }
            if space_count > 0 {
                new_indent.push_str(&" ".repeat(space_count));
            }

            let old_indent = &line[..old_indent_len];

            if old_indent != new_indent {
                edits.push((
                    Point::new(row.0, 0)..Point::new(row.0, old_indent_len as u32),
                    new_indent,
                ));
            }
        }

        if edits.is_empty() {
            return;
        }

        self.transact(window, cx, |this, _window, cx| {
            this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
            });
        });
    }

    pub fn convert_to_upper_case(
        &mut self,
        _: &ConvertToUpperCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| text.to_uppercase())
    }

    pub fn convert_to_lower_case(
        &mut self,
        _: &ConvertToLowerCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| text.to_lowercase())
    }

    pub fn convert_to_title_case(
        &mut self,
        _: &ConvertToTitleCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| {
            Self::convert_text_case(text, Case::Title)
        })
    }

    pub fn convert_to_snake_case(
        &mut self,
        _: &ConvertToSnakeCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| {
            Self::convert_text_case(text, Case::Snake)
        })
    }

    pub fn convert_to_kebab_case(
        &mut self,
        _: &ConvertToKebabCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| {
            Self::convert_text_case(text, Case::Kebab)
        })
    }

    pub fn convert_to_upper_camel_case(
        &mut self,
        _: &ConvertToUpperCamelCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| {
            Self::convert_text_case(text, Case::UpperCamel)
        })
    }

    pub fn convert_to_lower_camel_case(
        &mut self,
        _: &ConvertToLowerCamelCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| {
            Self::convert_text_case(text, Case::Camel)
        })
    }

    pub fn convert_to_opposite_case(
        &mut self,
        _: &ConvertToOppositeCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| {
            text.chars()
                .fold(String::with_capacity(text.len()), |mut t, c| {
                    if c.is_uppercase() {
                        t.extend(c.to_lowercase());
                    } else {
                        t.extend(c.to_uppercase());
                    }
                    t
                })
        })
    }

    pub fn convert_to_sentence_case(
        &mut self,
        _: &ConvertToSentenceCase,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| {
            Self::convert_text_case(text, Case::Sentence)
        })
    }

    pub fn toggle_case(&mut self, _: &ToggleCase, window: &mut Window, cx: &mut Context<Self>) {
        self.manipulate_text(window, cx, |text| {
            let has_upper_case_characters = text.chars().any(|c| c.is_uppercase());
            if has_upper_case_characters {
                text.to_lowercase()
            } else {
                text.to_uppercase()
            }
        })
    }

    pub fn convert_to_rot13(
        &mut self,
        _: &ConvertToRot13,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| {
            text.chars()
                .map(|c| match c {
                    'A'..='M' | 'a'..='m' => ((c as u8) + 13) as char,
                    'N'..='Z' | 'n'..='z' => ((c as u8) - 13) as char,
                    _ => c,
                })
                .collect()
        })
    }

    fn convert_text_case(text: &str, case: Case) -> String {
        text.lines()
            .map(|line| {
                let trimmed_start = line.trim_start();
                let leading = &line[..line.len() - trimmed_start.len()];
                let trimmed = trimmed_start.trim_end();
                let trailing = &trimmed_start[trimmed.len()..];
                format!("{}{}{}", leading, trimmed.to_case(case), trailing)
            })
            .join("\n")
    }

    pub fn convert_to_rot47(
        &mut self,
        _: &ConvertToRot47,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.manipulate_text(window, cx, |text| {
            text.chars()
                .map(|c| {
                    let code_point = c as u32;
                    if code_point >= 33 && code_point <= 126 {
                        return char::from_u32(33 + ((code_point + 14) % 94)).unwrap();
                    }
                    c
                })
                .collect()
        })
    }

    pub fn convert_to_base64(
        &mut self,
        _: &ConvertToBase64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use base64::Engine as _;
        self.manipulate_text(window, cx, |text| {
            base64::engine::general_purpose::STANDARD.encode(text)
        })
    }

    pub fn convert_from_base64(
        &mut self,
        _: &ConvertFromBase64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use base64::Engine as _;
        self.manipulate_text(
            window,
            cx,
            |text| match base64::engine::general_purpose::STANDARD.decode(text) {
                Ok(bytes) => String::from_utf8(bytes).unwrap_or_else(|_| text.to_string()),
                Err(_) => text.to_string(),
            },
        )
    }

    fn manipulate_text<Fn>(&mut self, window: &mut Window, cx: &mut Context<Self>, mut callback: Fn)
    where
        Fn: FnMut(&str) -> String,
    {
        if self.read_only(cx) {
            return;
        }
        let buffer = self.buffer.read(cx).snapshot(cx);

        let mut new_selections = Vec::new();
        let mut edits = Vec::new();

        for selection in self.selections.all_adjusted(&self.display_snapshot(cx)) {
            let selection_is_empty = selection.is_empty();

            let (start, end) = if selection_is_empty {
                let (word_range, _) = buffer.surrounding_word(selection.start);
                (word_range.start, word_range.end)
            } else {
                (
                    buffer.point_to_offset(selection.start),
                    buffer.point_to_offset(selection.end),
                )
            };

            let old_text = buffer.text_for_range(start..end).collect::<String>();
            let new_text = callback(&old_text);

            new_selections.push(Selection {
                start: buffer.anchor_before(start),
                end: buffer.anchor_after(end),
                goal: SelectionGoal::None,
                id: selection.id,
                reversed: selection.reversed,
            });

            if new_text != old_text {
                edits.push((start..end, new_text));
            }
        }

        if edits.is_empty() {
            return;
        }

        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
            });

            this.change_selections(Default::default(), window, cx, |s| {
                s.select(new_selections);
            });

            this.request_autoscroll(Autoscroll::fit(), cx);
        });
    }

    pub fn move_selection_on_drop(
        &mut self,
        selection: &Selection<Anchor>,
        target: DisplayPoint,
        is_cut: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = display_map.buffer_snapshot();
        let mut edits = Vec::new();
        let insert_point = display_map
            .clip_point(target, Bias::Left)
            .to_point(&display_map);
        let text = buffer
            .text_for_range(selection.start..selection.end)
            .collect::<String>();
        if is_cut {
            edits.push(((selection.start..selection.end), String::new()));
        }
        let insert_anchor = buffer.anchor_before(insert_point);
        edits.push(((insert_anchor..insert_anchor), text));
        let last_edit_start = insert_anchor.bias_left(buffer);
        let last_edit_end = insert_anchor.bias_right(buffer);
        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
            });
            this.change_selections(Default::default(), window, cx, |s| {
                s.select_anchor_ranges([last_edit_start..last_edit_end]);
            });
        });
    }

    pub fn clear_selection_drag_state(&mut self) {
        self.selection_drag_state = SelectionDragState::None;
    }

    pub fn duplicate(
        &mut self,
        upwards: bool,
        whole_lines: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }

        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = display_map.buffer_snapshot();
        let selections = self.selections.all::<Point>(&display_map);

        let mut edits = Vec::new();
        let mut selections_iter = selections.iter().peekable();
        while let Some(selection) = selections_iter.next() {
            let mut rows = selection.spanned_rows(false, &display_map);
            // duplicate line-wise
            if whole_lines || selection.start == selection.end {
                // Avoid duplicating the same lines twice.
                while let Some(next_selection) = selections_iter.peek() {
                    let next_rows = next_selection.spanned_rows(false, &display_map);
                    if next_rows.start < rows.end {
                        rows.end = next_rows.end;
                        selections_iter.next().unwrap();
                    } else {
                        break;
                    }
                }

                // Copy the text from the selected row region and splice it either at the start
                // or end of the region.
                let start = Point::new(rows.start.0, 0);
                let end = Point::new(
                    rows.end.previous_row().0,
                    buffer.line_len(rows.end.previous_row()),
                );

                let mut text = buffer.text_for_range(start..end).collect::<String>();

                let insert_location = if upwards {
                    // When duplicating upward, we need to insert before the current line.
                    // If we're on the last line and it doesn't end with a newline,
                    // we need to add a newline before the duplicated content.
                    let needs_leading_newline = rows.end.0 >= buffer.max_point().row
                        && buffer.max_point().column > 0
                        && !text.ends_with('\n');

                    if needs_leading_newline {
                        text.insert(0, '\n');
                        end
                    } else {
                        text.push('\n');
                        Point::new(rows.start.0, 0)
                    }
                } else {
                    text.push('\n');
                    start
                };
                edits.push((insert_location..insert_location, text));
            } else {
                // duplicate character-wise
                let start = selection.start;
                let end = selection.end;
                let text = buffer.text_for_range(start..end).collect::<String>();
                edits.push((selection.end..selection.end, text));
            }
        }

        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |buffer, cx| {
                buffer.edit(edits, None, cx);
            });

            // When duplicating upward with whole lines, move the cursor to the duplicated line
            if upwards && whole_lines {
                let display_map = this.display_map.update(cx, |map, cx| map.snapshot(cx));

                this.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
                    let mut new_ranges = Vec::new();
                    let selections = s.all::<Point>(&display_map);
                    let mut selections_iter = selections.iter().peekable();

                    while let Some(first_selection) = selections_iter.next() {
                        // Group contiguous selections together to find the total row span
                        let mut group_selections = vec![first_selection];
                        let mut rows = first_selection.spanned_rows(false, &display_map);

                        while let Some(next_selection) = selections_iter.peek() {
                            let next_rows = next_selection.spanned_rows(false, &display_map);
                            if next_rows.start < rows.end {
                                rows.end = next_rows.end;
                                group_selections.push(selections_iter.next().unwrap());
                            } else {
                                break;
                            }
                        }

                        let row_count = rows.end.0 - rows.start.0;

                        // Move all selections in this group up by the total number of duplicated rows
                        for selection in group_selections {
                            let new_start = Point::new(
                                selection.start.row.saturating_sub(row_count),
                                selection.start.column,
                            );

                            let new_end = Point::new(
                                selection.end.row.saturating_sub(row_count),
                                selection.end.column,
                            );

                            new_ranges.push(new_start..new_end);
                        }
                    }

                    s.select_ranges(new_ranges);
                });
            }

            this.request_autoscroll(Autoscroll::fit(), cx);
        });
    }

    pub fn duplicate_line_up(
        &mut self,
        _: &DuplicateLineUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.duplicate(true, true, window, cx);
    }

    pub fn duplicate_line_down(
        &mut self,
        _: &DuplicateLineDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.duplicate(false, true, window, cx);
    }

    pub fn duplicate_selection(
        &mut self,
        _: &DuplicateSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.duplicate(false, false, window, cx);
    }

    pub fn move_line_up(&mut self, _: &MoveLineUp, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        if self.mode.is_single_line() {
            cx.propagate();
            return;
        }

        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = self.buffer.read(cx).snapshot(cx);

        let mut edits = Vec::new();
        let mut unfold_ranges = Vec::new();
        let mut refold_creases = Vec::new();

        let selections = self.selections.all::<Point>(&display_map);
        let mut selections = selections.iter().peekable();
        let mut contiguous_row_selections = Vec::new();
        let mut new_selections = Vec::new();

        while let Some(selection) = selections.next() {
            // Find all the selections that span a contiguous row range
            let (start_row, end_row) = consume_contiguous_rows(
                &mut contiguous_row_selections,
                selection,
                &display_map,
                &mut selections,
            );

            // Move the text spanned by the row range to be before the line preceding the row range
            if start_row.0 > 0 {
                let range_to_move = Point::new(
                    start_row.previous_row().0,
                    buffer.line_len(start_row.previous_row()),
                )
                    ..Point::new(
                        end_row.previous_row().0,
                        buffer.line_len(end_row.previous_row()),
                    );
                let insertion_point = display_map
                    .prev_line_boundary(Point::new(start_row.previous_row().0, 0))
                    .0;

                // Don't move lines across excerpts
                if buffer
                    .excerpt_containing(insertion_point..range_to_move.end)
                    .is_some()
                {
                    let text = buffer
                        .text_for_range(range_to_move.clone())
                        .flat_map(|s| s.chars())
                        .skip(1)
                        .chain(['\n'])
                        .collect::<String>();

                    edits.push((
                        buffer.anchor_after(range_to_move.start)
                            ..buffer.anchor_before(range_to_move.end),
                        String::new(),
                    ));
                    let insertion_anchor = buffer.anchor_after(insertion_point);
                    edits.push((insertion_anchor..insertion_anchor, text));

                    let row_delta = range_to_move.start.row - insertion_point.row + 1;

                    // Move selections up
                    new_selections.extend(contiguous_row_selections.drain(..).map(
                        |mut selection| {
                            selection.start.row -= row_delta;
                            selection.end.row -= row_delta;
                            selection
                        },
                    ));

                    // Move folds up
                    unfold_ranges.push(range_to_move.clone());
                    for fold in display_map.folds_in_range(
                        buffer.anchor_before(range_to_move.start)
                            ..buffer.anchor_after(range_to_move.end),
                    ) {
                        let mut start = fold.range.start.to_point(&buffer);
                        let mut end = fold.range.end.to_point(&buffer);
                        start.row -= row_delta;
                        end.row -= row_delta;
                        refold_creases.push(Crease::simple(start..end, fold.placeholder.clone()));
                    }
                }
            }

            // If we didn't move line(s), preserve the existing selections
            new_selections.append(&mut contiguous_row_selections);
        }

        self.transact(window, cx, |this, window, cx| {
            this.unfold_ranges(&unfold_ranges, true, true, cx);
            this.buffer.update(cx, |buffer, cx| {
                for (range, text) in edits {
                    buffer.edit([(range, text)], None, cx);
                }
            });
            this.fold_creases(refold_creases, true, window, cx);
            this.change_selections(Default::default(), window, cx, |s| {
                s.select(new_selections);
            })
        });
    }

    pub fn move_line_down(
        &mut self,
        _: &MoveLineDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }
        if self.mode.is_single_line() {
            cx.propagate();
            return;
        }

        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let buffer = self.buffer.read(cx).snapshot(cx);

        let mut edits = Vec::new();
        let mut unfold_ranges = Vec::new();
        let mut refold_creases = Vec::new();

        let selections = self.selections.all::<Point>(&display_map);
        let mut selections = selections.iter().peekable();
        let mut contiguous_row_selections = Vec::new();
        let mut new_selections = Vec::new();

        while let Some(selection) = selections.next() {
            // Find all the selections that span a contiguous row range
            let (start_row, end_row) = consume_contiguous_rows(
                &mut contiguous_row_selections,
                selection,
                &display_map,
                &mut selections,
            );

            // Move the text spanned by the row range to be after the last line of the row range
            if end_row.0 <= buffer.max_point().row {
                let range_to_move =
                    MultiBufferPoint::new(start_row.0, 0)..MultiBufferPoint::new(end_row.0, 0);
                let insertion_point = display_map
                    .next_line_boundary(MultiBufferPoint::new(end_row.0, 0))
                    .0;

                // Don't move lines across excerpt boundaries
                if buffer
                    .excerpt_containing(range_to_move.start..insertion_point)
                    .is_some()
                {
                    let mut text = String::from("\n");
                    text.extend(buffer.text_for_range(range_to_move.clone()));
                    text.pop(); // Drop trailing newline
                    edits.push((
                        buffer.anchor_after(range_to_move.start)
                            ..buffer.anchor_before(range_to_move.end),
                        String::new(),
                    ));
                    let insertion_anchor = buffer.anchor_after(insertion_point);
                    edits.push((insertion_anchor..insertion_anchor, text));

                    let row_delta = insertion_point.row - range_to_move.end.row + 1;

                    // Move selections down
                    new_selections.extend(contiguous_row_selections.drain(..).map(
                        |mut selection| {
                            selection.start.row += row_delta;
                            selection.end.row += row_delta;
                            selection
                        },
                    ));

                    // Move folds down
                    unfold_ranges.push(range_to_move.clone());
                    for fold in display_map.folds_in_range(
                        buffer.anchor_before(range_to_move.start)
                            ..buffer.anchor_after(range_to_move.end),
                    ) {
                        let mut start = fold.range.start.to_point(&buffer);
                        let mut end = fold.range.end.to_point(&buffer);
                        start.row += row_delta;
                        end.row += row_delta;
                        refold_creases.push(Crease::simple(start..end, fold.placeholder.clone()));
                    }
                }
            }

            // If we didn't move line(s), preserve the existing selections
            new_selections.append(&mut contiguous_row_selections);
        }

        self.transact(window, cx, |this, window, cx| {
            this.unfold_ranges(&unfold_ranges, true, true, cx);
            this.buffer.update(cx, |buffer, cx| {
                for (range, text) in edits {
                    buffer.edit([(range, text)], None, cx);
                }
            });
            this.fold_creases(refold_creases, true, window, cx);
            this.change_selections(Default::default(), window, cx, |s| s.select(new_selections));
        });
    }

    pub fn transpose(&mut self, _: &Transpose, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        let text_layout_details = &self.text_layout_details(window, cx);
        self.transact(window, cx, |this, window, cx| {
            let edits = this.change_selections(Default::default(), window, cx, |s| {
                let mut edits: Vec<(Range<MultiBufferOffset>, String)> = Default::default();
                s.move_with(&mut |display_map, selection| {
                    if !selection.is_empty() {
                        return;
                    }

                    let mut head = selection.head();
                    let mut transpose_offset = head.to_offset(display_map, Bias::Right);
                    if head.column() == display_map.line_len(head.row()) {
                        transpose_offset = display_map
                            .buffer_snapshot()
                            .clip_offset(transpose_offset.saturating_sub_usize(1), Bias::Left);
                    }

                    if transpose_offset == MultiBufferOffset(0) {
                        return;
                    }

                    *head.column_mut() += 1;
                    head = display_map.clip_point(head, Bias::Right);
                    let goal = SelectionGoal::HorizontalPosition(
                        display_map
                            .x_for_display_point(head, text_layout_details)
                            .into(),
                    );
                    selection.collapse_to(head, goal);

                    let transpose_start = display_map
                        .buffer_snapshot()
                        .clip_offset(transpose_offset.saturating_sub_usize(1), Bias::Left);
                    if edits.last().is_none_or(|e| e.0.end <= transpose_start) {
                        let transpose_end = display_map
                            .buffer_snapshot()
                            .clip_offset(transpose_offset + 1usize, Bias::Right);
                        if let Some(ch) = display_map
                            .buffer_snapshot()
                            .chars_at(transpose_start)
                            .next()
                        {
                            edits.push((transpose_start..transpose_offset, String::new()));
                            edits.push((transpose_end..transpose_end, ch.to_string()));
                        }
                    }
                });
                edits
            });
            this.buffer
                .update(cx, |buffer, cx| buffer.edit(edits, None, cx));
            let selections = this
                .selections
                .all::<MultiBufferOffset>(&this.display_snapshot(cx));
            this.change_selections(Default::default(), window, cx, |s| {
                s.select(selections);
            });
        });
    }

    pub fn undo(&mut self, _: &Undo, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }

        if let Some(transaction_id) = self.buffer.update(cx, |buffer, cx| buffer.undo(cx)) {
            if let Some((selections, _)) =
                self.selection_history.transaction(transaction_id).cloned()
            {
                self.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
                    s.select_anchors(selections.to_vec());
                });
            } else {
                log::error!(
                    "No entry in selection_history found for undo. \
                     This may correspond to a bug where undo does not update the selection. \
                     If this is occurring, please add details to \
                     https://github.com/zed-industries/zed/issues/22692"
                );
            }
            self.request_autoscroll(Autoscroll::fit(), cx);
            self.unmark_text(window, cx);
            cx.emit(EditorEvent::Edited { transaction_id });
            cx.emit(EditorEvent::TransactionUndone { transaction_id });
        }
    }

    pub fn redo(&mut self, _: &Redo, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }

        if let Some(transaction_id) = self.buffer.update(cx, |buffer, cx| buffer.redo(cx)) {
            if let Some((_, Some(selections))) =
                self.selection_history.transaction(transaction_id).cloned()
            {
                self.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
                    s.select_anchors(selections.to_vec());
                });
            } else {
                log::error!(
                    "No entry in selection_history found for redo. \
                     This may correspond to a bug where undo does not update the selection. \
                     If this is occurring, please add details to \
                     https://github.com/zed-industries/zed/issues/22692"
                );
            }
            self.request_autoscroll(Autoscroll::fit(), cx);
            self.unmark_text(window, cx);
            cx.emit(EditorEvent::Edited { transaction_id });
        }
    }

    pub fn finalize_last_transaction(&mut self, cx: &mut Context<Self>) {
        self.buffer
            .update(cx, |buffer, cx| buffer.finalize_last_transaction(cx));
    }

    pub fn group_until_transaction(&mut self, tx_id: TransactionId, cx: &mut Context<Self>) {
        self.buffer
            .update(cx, |buffer, cx| buffer.group_until_transaction(tx_id, cx));
    }

    pub fn rename(
        &mut self,
        _: &Rename,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        use language::ToOffset as _;

        if self.read_only(cx) {
            return None;
        }
        let provider = self.semantics_provider.clone()?;
        let selection = self.selections.newest_anchor().clone();
        let (cursor_buffer, cursor_buffer_position) = self
            .buffer
            .read(cx)
            .text_anchor_for_position(selection.head(), cx)?;
        let (tail_buffer, cursor_buffer_position_end) = self
            .buffer
            .read(cx)
            .text_anchor_for_position(selection.tail(), cx)?;
        if tail_buffer != cursor_buffer {
            return None;
        }

        let snapshot = cursor_buffer.read(cx).snapshot();
        let cursor_buffer_offset = cursor_buffer_position.to_offset(&snapshot);
        let cursor_buffer_offset_end = cursor_buffer_position_end.to_offset(&snapshot);
        let prepare_rename = provider.range_for_rename(&cursor_buffer, cursor_buffer_position, cx);
        drop(snapshot);

        Some(cx.spawn_in(window, async move |this, cx| {
            let rename_range = prepare_rename.await?;
            if let Some(rename_range) = rename_range {
                this.update_in(cx, |this, window, cx| {
                    let snapshot = cursor_buffer.read(cx).snapshot();
                    let rename_buffer_range = rename_range.to_offset(&snapshot);
                    let cursor_offset_in_rename_range =
                        cursor_buffer_offset.saturating_sub(rename_buffer_range.start);
                    let cursor_offset_in_rename_range_end =
                        cursor_buffer_offset_end.saturating_sub(rename_buffer_range.start);

                    this.take_rename(false, window, cx);
                    let buffer = this.buffer.read(cx).read(cx);
                    let cursor_offset = selection.head().to_offset(&buffer);
                    let rename_start =
                        cursor_offset.saturating_sub_usize(cursor_offset_in_rename_range);
                    let rename_end = rename_start + rename_buffer_range.len();
                    let range = buffer.anchor_before(rename_start)..buffer.anchor_after(rename_end);
                    let mut old_highlight_id = None;
                    let old_name: Arc<str> = buffer
                        .chunks(
                            rename_start..rename_end,
                            LanguageAwareStyling {
                                tree_sitter: true,
                                diagnostics: true,
                            },
                        )
                        .map(|chunk| {
                            if old_highlight_id.is_none() {
                                old_highlight_id = chunk.syntax_highlight_id;
                            }
                            chunk.text
                        })
                        .collect::<String>()
                        .into();

                    drop(buffer);

                    // Position the selection in the rename editor so that it matches the current selection.
                    this.show_local_selections = false;
                    let rename_editor = cx.new(|cx| {
                        let mut editor = Editor::single_line(window, cx);
                        editor.buffer.update(cx, |buffer, cx| {
                            buffer.edit(
                                [(MultiBufferOffset(0)..MultiBufferOffset(0), old_name.clone())],
                                None,
                                cx,
                            )
                        });
                        let cursor_offset_in_rename_range =
                            MultiBufferOffset(cursor_offset_in_rename_range);
                        let cursor_offset_in_rename_range_end =
                            MultiBufferOffset(cursor_offset_in_rename_range_end);
                        let rename_selection_range = match cursor_offset_in_rename_range
                            .cmp(&cursor_offset_in_rename_range_end)
                        {
                            Ordering::Equal => {
                                editor.select_all(&SelectAll, window, cx);
                                return editor;
                            }
                            Ordering::Less => {
                                cursor_offset_in_rename_range..cursor_offset_in_rename_range_end
                            }
                            Ordering::Greater => {
                                cursor_offset_in_rename_range_end..cursor_offset_in_rename_range
                            }
                        };
                        if rename_selection_range.end.0 > old_name.len() {
                            editor.select_all(&SelectAll, window, cx);
                        } else {
                            editor.change_selections(Default::default(), window, cx, |s| {
                                s.select_ranges([rename_selection_range]);
                            });
                        }
                        editor
                    });
                    cx.subscribe(&rename_editor, |_, _, e: &EditorEvent, cx| {
                        if e == &EditorEvent::Focused {
                            cx.emit(EditorEvent::FocusedIn)
                        }
                    })
                    .detach();

                    let write_highlights =
                        this.clear_background_highlights(HighlightKey::DocumentHighlightWrite, cx);
                    let read_highlights =
                        this.clear_background_highlights(HighlightKey::DocumentHighlightRead, cx);
                    let ranges = write_highlights
                        .iter()
                        .flat_map(|(_, ranges)| ranges.iter())
                        .chain(read_highlights.iter().flat_map(|(_, ranges)| ranges.iter()))
                        .cloned()
                        .collect();

                    this.highlight_text(
                        HighlightKey::Rename,
                        ranges,
                        HighlightStyle {
                            fade_out: Some(0.6),
                            ..Default::default()
                        },
                        cx,
                    );
                    let rename_focus_handle = rename_editor.focus_handle(cx);
                    window.focus(&rename_focus_handle, cx);
                    let block_id = this.insert_blocks(
                        [BlockProperties {
                            style: BlockStyle::Flex,
                            placement: BlockPlacement::Below(range.start),
                            height: Some(1),
                            render: Arc::new({
                                let rename_editor = rename_editor.clone();
                                move |cx: &mut BlockContext| {
                                    let mut text_style = cx.editor_style.text.clone();
                                    if let Some(highlight_style) = old_highlight_id
                                        .and_then(|h| cx.editor_style.syntax.get(h).cloned())
                                    {
                                        text_style = text_style.highlight(highlight_style);
                                    }
                                    div()
                                        .block_mouse_except_scroll()
                                        .pl(cx.anchor_x)
                                        .child(EditorElement::new(
                                            &rename_editor,
                                            EditorStyle {
                                                background: cx.theme().system().transparent,
                                                local_player: cx.editor_style.local_player,
                                                text: text_style,
                                                scrollbar_width: cx.editor_style.scrollbar_width,
                                                syntax: cx.editor_style.syntax.clone(),
                                                status: cx.editor_style.status.clone(),
                                                ..EditorStyle::default()
                                            },
                                        ))
                                        .into_any_element()
                                }
                            }),
                            priority: 0,
                        }],
                        Some(Autoscroll::fit()),
                        cx,
                    )[0];
                    this.pending_rename = Some(RenameState {
                        range,
                        old_name,
                        editor: rename_editor,
                        block_id,
                    });
                })?;
            }

            Ok(())
        }))
    }

    pub fn confirm_rename(
        &mut self,
        _: &ConfirmRename,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<()>>> {
        if self.read_only(cx) {
            return None;
        }
        let rename = self.take_rename(false, window, cx)?;
        let workspace = self.workspace()?.downgrade();
        let (buffer, start) = self
            .buffer
            .read(cx)
            .text_anchor_for_position(rename.range.start, cx)?;
        let (end_buffer, _) = self
            .buffer
            .read(cx)
            .text_anchor_for_position(rename.range.end, cx)?;
        if buffer != end_buffer {
            return None;
        }

        let old_name = rename.old_name;
        let new_name = rename.editor.read(cx).text(cx);

        let rename = self.semantics_provider.as_ref()?.perform_rename(
            &buffer,
            start,
            new_name.clone(),
            cx,
        )?;

        Some(cx.spawn_in(window, async move |editor, cx| {
            let project_transaction = rename.await?;
            Self::open_project_transaction(
                &editor,
                workspace,
                project_transaction,
                format!("Rename: {} → {}", old_name, new_name),
                cx,
            )
            .await?;

            editor.update(cx, |editor, cx| {
                editor.refresh_document_highlights(cx);
            })?;
            Ok(())
        }))
    }

    fn take_rename(
        &mut self,
        moving_cursor: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<RenameState> {
        let rename = self.pending_rename.take()?;
        if rename.editor.focus_handle(cx).is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }

        self.remove_blocks(
            [rename.block_id].into_iter().collect(),
            Some(Autoscroll::fit()),
            cx,
        );
        self.clear_highlights(HighlightKey::Rename, cx);
        self.show_local_selections = true;

        if moving_cursor {
            let cursor_in_rename_editor = rename.editor.update(cx, |editor, cx| {
                editor
                    .selections
                    .newest::<MultiBufferOffset>(&editor.display_snapshot(cx))
                    .head()
            });

            // Update the selection to match the position of the selection inside
            // the rename editor.
            let snapshot = self.buffer.read(cx).read(cx);
            let rename_range = rename.range.to_offset(&snapshot);
            let cursor_in_editor = snapshot
                .clip_offset(rename_range.start + cursor_in_rename_editor, Bias::Left)
                .min(rename_range.end);
            drop(snapshot);

            self.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
                s.select_ranges(vec![cursor_in_editor..cursor_in_editor])
            });
        } else {
            self.refresh_document_highlights(cx);
        }

        Some(rename)
    }

    pub fn pending_rename(&self) -> Option<&RenameState> {
        self.pending_rename.as_ref()
    }

    fn restart_language_server(
        &mut self,
        _: &RestartLanguageServer,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.project.clone() {
            self.buffer.update(cx, |multi_buffer, cx| {
                project.update(cx, |project, cx| {
                    project.restart_language_servers_for_buffers(
                        multi_buffer.all_buffers().into_iter().collect(),
                        HashSet::default(),
                        cx,
                    );
                });
            })
        }
    }

    fn stop_language_server(
        &mut self,
        _: &StopLanguageServer,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(project) = self.project.clone() {
            self.buffer.update(cx, |multi_buffer, cx| {
                project.update(cx, |project, cx| {
                    project.stop_language_servers_for_buffers(
                        multi_buffer.all_buffers().into_iter().collect(),
                        HashSet::default(),
                        cx,
                    );
                });
            });
        }
    }

    fn cancel_language_server_work(
        workspace: &mut Workspace,
        _: &actions::CancelLanguageServerWork,
        _: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let project = workspace.project();
        let buffers = workspace
            .active_item(cx)
            .and_then(|item| item.act_as::<Editor>(cx))
            .map_or(HashSet::default(), |editor| {
                editor.read(cx).buffer.read(cx).all_buffers()
            });
        project.update(cx, |project, cx| {
            project.cancel_language_server_work_for_buffers(buffers, cx);
        });
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    pub fn transact(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>),
    ) -> Option<TransactionId> {
        self.with_selection_effects_deferred(window, cx, |this, window, cx| {
            this.start_transaction_at(Instant::now(), window, cx);
            update(this, window, cx);
            this.end_transaction_at(Instant::now(), cx)
        })
    }

    pub fn start_transaction_at(
        &mut self,
        now: Instant,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<TransactionId> {
        self.end_selection(window, cx);
        if let Some(tx_id) = self
            .buffer
            .update(cx, |buffer, cx| buffer.start_transaction_at(now, cx))
        {
            self.selection_history
                .insert_transaction(tx_id, self.selections.disjoint_anchors_arc());
            cx.emit(EditorEvent::TransactionBegun {
                transaction_id: tx_id,
            });
            Some(tx_id)
        } else {
            None
        }
    }

    pub fn end_transaction_at(
        &mut self,
        now: Instant,
        cx: &mut Context<Self>,
    ) -> Option<TransactionId> {
        if let Some(transaction_id) = self
            .buffer
            .update(cx, |buffer, cx| buffer.end_transaction_at(now, cx))
        {
            if let Some((_, end_selections)) =
                self.selection_history.transaction_mut(transaction_id)
            {
                *end_selections = Some(self.selections.disjoint_anchors_arc());
            } else {
                log::error!("unexpectedly ended a transaction that wasn't started by this editor");
            }

            cx.emit(EditorEvent::Edited { transaction_id });
            Some(transaction_id)
        } else {
            None
        }
    }

    pub fn modify_transaction_selection_history(
        &mut self,
        transaction_id: TransactionId,
        modify: impl FnOnce(&mut (Arc<[Selection<Anchor>]>, Option<Arc<[Selection<Anchor>]>>)),
    ) -> bool {
        self.selection_history
            .transaction_mut(transaction_id)
            .map(modify)
            .is_some()
    }

    pub fn toggle_focus(
        workspace: &mut Workspace,
        _: &actions::ToggleFocus,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let Some(item) = workspace.recent_active_item_by_type::<Self>(cx) else {
            return;
        };
        workspace.activate_item(&item, true, true, window, cx);
    }

    pub fn set_gutter_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if hovered != self.gutter_hovered {
            self.gutter_hovered = hovered;
            cx.notify();
        }
    }

    pub fn insert_blocks(
        &mut self,
        blocks: impl IntoIterator<Item = BlockProperties<Anchor>>,
        autoscroll: Option<Autoscroll>,
        cx: &mut Context<Self>,
    ) -> Vec<CustomBlockId> {
        let blocks = self
            .display_map
            .update(cx, |display_map, cx| display_map.insert_blocks(blocks, cx));
        if let Some(autoscroll) = autoscroll {
            self.request_autoscroll(autoscroll, cx);
        }
        cx.notify();
        blocks
    }

    pub fn resize_blocks(
        &mut self,
        heights: HashMap<CustomBlockId, u32>,
        autoscroll: Option<Autoscroll>,
        cx: &mut Context<Self>,
    ) {
        self.display_map
            .update(cx, |display_map, cx| display_map.resize_blocks(heights, cx));
        if let Some(autoscroll) = autoscroll {
            self.request_autoscroll(autoscroll, cx);
        }
        cx.notify();
    }

    pub fn replace_blocks(
        &mut self,
        renderers: HashMap<CustomBlockId, RenderBlock>,
        autoscroll: Option<Autoscroll>,
        cx: &mut Context<Self>,
    ) {
        self.display_map
            .update(cx, |display_map, _cx| display_map.replace_blocks(renderers));
        if let Some(autoscroll) = autoscroll {
            self.request_autoscroll(autoscroll, cx);
        }
        cx.notify();
    }

    pub fn remove_blocks(
        &mut self,
        block_ids: HashSet<CustomBlockId>,
        autoscroll: Option<Autoscroll>,
        cx: &mut Context<Self>,
    ) {
        self.display_map.update(cx, |display_map, cx| {
            display_map.remove_blocks(block_ids, cx)
        });
        if let Some(autoscroll) = autoscroll {
            self.request_autoscroll(autoscroll, cx);
        }
        cx.notify();
    }

    pub fn row_for_block(
        &self,
        block_id: CustomBlockId,
        cx: &mut Context<Self>,
    ) -> Option<DisplayRow> {
        self.display_map
            .update(cx, |map, cx| map.row_for_block(block_id, cx))
    }

    pub(crate) fn set_focused_block(&mut self, focused_block: FocusedBlock) {
        self.focused_block = Some(focused_block);
    }

    pub(crate) fn take_focused_block(&mut self) -> Option<FocusedBlock> {
        self.focused_block.take()
    }

    pub fn longest_row(&self, cx: &mut App) -> DisplayRow {
        self.display_map
            .update(cx, |map, cx| map.snapshot(cx))
            .longest_row()
    }

    pub fn max_point(&self, cx: &mut App) -> DisplayPoint {
        self.display_map
            .update(cx, |map, cx| map.snapshot(cx))
            .max_point()
    }

    pub fn text(&self, cx: &App) -> String {
        self.buffer.read(cx).read(cx).text()
    }

    pub fn is_empty(&self, cx: &App) -> bool {
        self.buffer.read(cx).read(cx).is_empty()
    }

    pub fn text_option(&self, cx: &App) -> Option<String> {
        let text = self.text(cx);
        let text = text.trim();

        if text.is_empty() {
            return None;
        }

        Some(text.to_string())
    }

    pub fn set_text(
        &mut self,
        text: impl Into<Arc<str>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.transact(window, cx, |this, _, cx| {
            this.buffer
                .read(cx)
                .as_singleton()
                .update(cx, |buffer, cx| buffer.set_text(text, cx));
        });
    }

    pub fn display_text(&self, cx: &mut App) -> String {
        self.display_map
            .update(cx, |map, cx| map.snapshot(cx))
            .text()
    }

    fn create_minimap(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Self> {
        const MINIMAP_FONT_WEIGHT: gpui::FontWeight = gpui::FontWeight::BLACK;
        const MINIMAP_FONT_FAMILY: SharedString = SharedString::new_static(".ZedMono");

        let mut minimap = Editor::new_internal(
            EditorMode::Minimap {
                parent: cx.weak_entity(),
            },
            self.buffer.clone(),
            None,
            Some(self.display_map.clone()),
            window,
            cx,
        );
        let my_snapshot = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let minimap_snapshot = minimap.display_map.update(cx, |map, cx| map.snapshot(cx));
        minimap.scroll_manager.clone_state(
            &self.scroll_manager,
            &my_snapshot,
            &minimap_snapshot,
            cx,
        );
        minimap.set_text_style_refinement(TextStyleRefinement {
            font_size: Some(MINIMAP_FONT_SIZE),
            font_weight: Some(MINIMAP_FONT_WEIGHT),
            font_family: Some(MINIMAP_FONT_FAMILY),
            ..Default::default()
        });
        cx.new(|_| minimap)
    }

    pub fn minimap(&self) -> Option<&Entity<Self>> {
        self.minimap.as_ref()
    }

    pub fn set_masked(&mut self, masked: bool, cx: &mut Context<Self>) {
        if self.display_map.read(cx).masked != masked {
            self.display_map.update(cx, |map, _| map.masked = masked);
        }
        cx.notify()
    }

    fn target_file<'a>(&self, cx: &'a App) -> Option<&'a dyn language::LocalFile> {
        self.active_buffer(cx)?
            .read(cx)
            .file()
            .and_then(|f| f.as_local())
    }

    fn reveal_in_finder(
        &mut self,
        _: &RevealInFileManager,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.target_file_abs_path(cx) {
            cx.reveal_path(&path);
        }
    }

    fn copy_path(
        &mut self,
        _: &zed_actions::workspace::CopyPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.target_file_abs_path(cx)
            && let Some(path) = path.to_str()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(path.to_string()));
        } else {
            cx.propagate();
        }
    }

    fn copy_relative_path(
        &mut self,
        _: &zed_actions::workspace::CopyRelativePath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.active_buffer(cx).and_then(|buffer| {
            let project = self.project()?.read(cx);
            let path = buffer.read(cx).file()?.path();
            let path = path.display(project.path_style(cx));
            Some(path)
        }) {
            cx.write_to_clipboard(ClipboardItem::new_string(path.to_string()));
        } else {
            cx.propagate();
        }
    }

    pub fn copy_file_name_without_extension(
        &mut self,
        _: &CopyFileNameWithoutExtension,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(file_stem) = self.active_buffer(cx).and_then(|buffer| {
            let file = buffer.read(cx).file()?;
            file.path().file_stem()
        }) {
            cx.write_to_clipboard(ClipboardItem::new_string(file_stem.to_string()));
        }
    }

    pub fn copy_file_name(&mut self, _: &CopyFileName, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(file_name) = self.active_buffer(cx).and_then(|buffer| {
            let file = buffer.read(cx).file()?;
            Some(file.file_name(cx))
        }) {
            cx.write_to_clipboard(ClipboardItem::new_string(file_name.to_string()));
        }
    }

    pub fn copy_file_location(
        &mut self,
        _: &CopyFileLocation,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selection = self.selections.newest::<Point>(&self.display_snapshot(cx));

        let start_line = selection.start.row + 1;
        let end_line = selection.end.row + 1;

        let end_line = if selection.end.column == 0 && end_line > start_line {
            end_line - 1
        } else {
            end_line
        };

        if let Some(file_location) = self.active_buffer(cx).and_then(|buffer| {
            let project = self.project()?.read(cx);
            let file = buffer.read(cx).file()?;
            let path = file.path().display(project.path_style(cx));

            let location = if start_line == end_line {
                format!("{path}:{start_line}")
            } else {
                format!("{path}:{start_line}-{end_line}")
            };
            Some(location)
        }) {
            cx.write_to_clipboard(ClipboardItem::new_string(file_location));
        }
    }

    pub fn insert_uuid_v4(
        &mut self,
        _: &InsertUuidV4,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_uuid(UuidVersion::V4, window, cx);
    }

    pub fn insert_uuid_v7(
        &mut self,
        _: &InsertUuidV7,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_uuid(UuidVersion::V7, window, cx);
    }

    fn insert_uuid(&mut self, version: UuidVersion, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx) {
            return;
        }
        self.transact(window, cx, |this, _window, cx| {
            let edits = this
                .selections
                .all::<Point>(&this.display_snapshot(cx))
                .into_iter()
                .map(|selection| {
                    let uuid = match version {
                        UuidVersion::V4 => uuid::Uuid::new_v4(),
                        UuidVersion::V7 => uuid::Uuid::now_v7(),
                    };

                    (selection.range(), uuid.to_string())
                });
            this.edit(edits, cx);
        });
    }

    /// Adds a row highlight for the given range. If a row has multiple highlights, the
    /// last highlight added will be used.
    ///
    /// If the range ends at the beginning of a line, then that line will not be highlighted.
    pub fn highlight_rows<T: 'static>(
        &mut self,
        range: Range<Anchor>,
        color: Hsla,
        options: RowHighlightOptions,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.buffer().read(cx).snapshot(cx);
        let row_highlights = self.highlighted_rows.entry(TypeId::of::<T>()).or_default();
        let ix = row_highlights.binary_search_by(|highlight| {
            Ordering::Equal
                .then_with(|| highlight.range.start.cmp(&range.start, &snapshot))
                .then_with(|| highlight.range.end.cmp(&range.end, &snapshot))
        });

        if let Err(mut ix) = ix {
            let index = post_inc(&mut self.highlight_order);

            // If this range intersects with the preceding highlight, then merge it with
            // the preceding highlight. Otherwise insert a new highlight.
            let mut merged = false;
            if ix > 0 {
                let prev_highlight = &mut row_highlights[ix - 1];
                if prev_highlight
                    .range
                    .end
                    .cmp(&range.start, &snapshot)
                    .is_ge()
                {
                    ix -= 1;
                    if prev_highlight.range.end.cmp(&range.end, &snapshot).is_lt() {
                        prev_highlight.range.end = range.end;
                    }
                    merged = true;
                    prev_highlight.index = index;
                    prev_highlight.color = color;
                    prev_highlight.options = options;
                }
            }

            if !merged {
                row_highlights.insert(
                    ix,
                    RowHighlight {
                        range,
                        index,
                        color,
                        options,
                        type_id: TypeId::of::<T>(),
                    },
                );
            }

            // If any of the following highlights intersect with this one, merge them.
            while let Some(next_highlight) = row_highlights.get(ix + 1) {
                let highlight = &row_highlights[ix];
                if next_highlight
                    .range
                    .start
                    .cmp(&highlight.range.end, &snapshot)
                    .is_le()
                {
                    if next_highlight
                        .range
                        .end
                        .cmp(&highlight.range.end, &snapshot)
                        .is_gt()
                    {
                        row_highlights[ix].range.end = next_highlight.range.end;
                    }
                    row_highlights.remove(ix + 1);
                } else {
                    break;
                }
            }
        }
    }

    /// Remove any highlighted row ranges of the given type that intersect the
    /// given ranges.
    pub fn remove_highlighted_rows<T: 'static>(
        &mut self,
        ranges_to_remove: Vec<Range<Anchor>>,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.buffer().read(cx).snapshot(cx);
        let row_highlights = self.highlighted_rows.entry(TypeId::of::<T>()).or_default();
        let mut ranges_to_remove = ranges_to_remove.iter().peekable();
        row_highlights.retain(|highlight| {
            while let Some(range_to_remove) = ranges_to_remove.peek() {
                match range_to_remove.end.cmp(&highlight.range.start, &snapshot) {
                    Ordering::Less | Ordering::Equal => {
                        ranges_to_remove.next();
                    }
                    Ordering::Greater => {
                        match range_to_remove.start.cmp(&highlight.range.end, &snapshot) {
                            Ordering::Less | Ordering::Equal => {
                                return false;
                            }
                            Ordering::Greater => break,
                        }
                    }
                }
            }

            true
        })
    }

    /// Clear all anchor ranges for a certain highlight context type, so no corresponding rows will be highlighted.
    pub fn clear_row_highlights<T: 'static>(&mut self) {
        self.highlighted_rows.remove(&TypeId::of::<T>());
    }

    /// For a highlight given context type, gets all anchor ranges that will be used for row highlighting.
    pub fn highlighted_rows<T: 'static>(&self) -> impl '_ + Iterator<Item = (Range<Anchor>, Hsla)> {
        self.highlighted_rows
            .get(&TypeId::of::<T>())
            .map_or(&[] as &[_], |vec| vec.as_slice())
            .iter()
            .map(|highlight| (highlight.range.clone(), highlight.color))
    }

    /// Merges all anchor ranges for all context types ever set, picking the last highlight added in case of a row conflict.
    /// Returns a map of display rows that are highlighted and their corresponding highlight color.
    /// Allows to ignore certain kinds of highlights.
    pub fn highlighted_display_rows(
        &self,
        window: &mut Window,
        cx: &mut App,
    ) -> BTreeMap<DisplayRow, LineHighlight> {
        let snapshot = self.snapshot(window, cx);
        let mut used_highlight_orders = HashMap::default();
        self.highlighted_rows
            .values()
            .flat_map(|highlighted_rows| highlighted_rows.iter())
            .fold(
                BTreeMap::<DisplayRow, LineHighlight>::new(),
                |mut unique_rows, highlight| {
                    let start = highlight.range.start.to_display_point(&snapshot);
                    let end = highlight.range.end.to_display_point(&snapshot);
                    let start_row = start.row().0;
                    let end_row = if !highlight.range.end.is_max() && end.column() == 0 {
                        end.row().0.saturating_sub(1)
                    } else {
                        end.row().0
                    };
                    for row in start_row..=end_row {
                        let used_index =
                            used_highlight_orders.entry(row).or_insert(highlight.index);
                        if highlight.index >= *used_index {
                            *used_index = highlight.index;
                            unique_rows.insert(
                                DisplayRow(row),
                                LineHighlight {
                                    include_gutter: highlight.options.include_gutter,
                                    border: None,
                                    background: highlight.color.into(),
                                    type_id: Some(highlight.type_id),
                                },
                            );
                        }
                    }
                    unique_rows
                },
            )
    }

    pub fn highlighted_display_row_for_autoscroll(
        &self,
        snapshot: &DisplaySnapshot,
    ) -> Option<DisplayRow> {
        self.highlighted_rows
            .values()
            .flat_map(|highlighted_rows| highlighted_rows.iter())
            .filter_map(|highlight| {
                if highlight.options.autoscroll {
                    Some(highlight.range.start.to_display_point(snapshot).row())
                } else {
                    None
                }
            })
            .min()
    }

    pub fn set_search_within_ranges(&mut self, ranges: &[Range<Anchor>], cx: &mut Context<Self>) {
        self.highlight_background(
            HighlightKey::SearchWithinRange,
            ranges,
            |_, colors| colors.colors().editor_document_highlight_read_background,
            cx,
        )
    }

    pub fn set_breadcrumb_header(&mut self, new_header: String) {
        self.breadcrumb_header = Some(new_header);
    }

    pub fn clear_search_within_ranges(&mut self, cx: &mut Context<Self>) {
        self.clear_background_highlights(HighlightKey::SearchWithinRange, cx);
    }

    pub fn highlight_background(
        &mut self,
        key: HighlightKey,
        ranges: &[Range<Anchor>],
        color_fetcher: impl Fn(&usize, &Theme) -> Hsla + Send + Sync + 'static,
        cx: &mut Context<Self>,
    ) {
        self.background_highlights
            .insert(key, (Arc::new(color_fetcher), Arc::from(ranges)));
        self.scrollbar_marker_state.dirty = true;
        cx.notify();
    }

    pub fn clear_background_highlights(
        &mut self,
        key: HighlightKey,
        cx: &mut Context<Self>,
    ) -> Option<BackgroundHighlight> {
        let text_highlights = self.background_highlights.remove(&key)?;
        if !text_highlights.1.is_empty() {
            self.scrollbar_marker_state.dirty = true;
            cx.notify();
        }
        Some(text_highlights)
    }

    pub fn highlight_gutter<T: 'static>(
        &mut self,
        ranges: impl Into<Vec<Range<Anchor>>>,
        color_fetcher: fn(&App) -> Hsla,
        cx: &mut Context<Self>,
    ) {
        self.gutter_highlights
            .insert(TypeId::of::<T>(), (color_fetcher, ranges.into()));
        cx.notify();
    }

    pub fn clear_gutter_highlights<T: 'static>(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<GutterHighlight> {
        cx.notify();
        self.gutter_highlights.remove(&TypeId::of::<T>())
    }

    pub fn insert_gutter_highlight<T: 'static>(
        &mut self,
        range: Range<Anchor>,
        color_fetcher: fn(&App) -> Hsla,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.buffer().read(cx).snapshot(cx);
        let mut highlights = self
            .gutter_highlights
            .remove(&TypeId::of::<T>())
            .map(|(_, highlights)| highlights)
            .unwrap_or_default();
        let ix = highlights.binary_search_by(|highlight| {
            Ordering::Equal
                .then_with(|| highlight.start.cmp(&range.start, &snapshot))
                .then_with(|| highlight.end.cmp(&range.end, &snapshot))
        });
        if let Err(ix) = ix {
            highlights.insert(ix, range);
        }
        self.gutter_highlights
            .insert(TypeId::of::<T>(), (color_fetcher, highlights));
    }

    pub fn remove_gutter_highlights<T: 'static>(
        &mut self,
        ranges_to_remove: Vec<Range<Anchor>>,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.buffer().read(cx).snapshot(cx);
        let Some((color_fetcher, mut gutter_highlights)) =
            self.gutter_highlights.remove(&TypeId::of::<T>())
        else {
            return;
        };
        let mut ranges_to_remove = ranges_to_remove.iter().peekable();
        gutter_highlights.retain(|highlight| {
            while let Some(range_to_remove) = ranges_to_remove.peek() {
                match range_to_remove.end.cmp(&highlight.start, &snapshot) {
                    Ordering::Less | Ordering::Equal => {
                        ranges_to_remove.next();
                    }
                    Ordering::Greater => {
                        match range_to_remove.start.cmp(&highlight.end, &snapshot) {
                            Ordering::Less | Ordering::Equal => {
                                return false;
                            }
                            Ordering::Greater => break,
                        }
                    }
                }
            }

            true
        });
        self.gutter_highlights
            .insert(TypeId::of::<T>(), (color_fetcher, gutter_highlights));
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn all_text_highlights(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<(HighlightStyle, Vec<Range<DisplayPoint>>)> {
        let snapshot = self.snapshot(window, cx);
        self.display_map.update(cx, |display_map, _| {
            display_map
                .all_text_highlights()
                .map(|(_, highlight)| {
                    let (style, ranges) = highlight.as_ref();
                    (
                        *style,
                        ranges
                            .iter()
                            .map(|range| range.clone().to_display_points(&snapshot))
                            .collect(),
                    )
                })
                .collect()
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn all_text_background_highlights(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<(Range<DisplayPoint>, Hsla)> {
        let snapshot = self.snapshot(window, cx);
        let buffer = &snapshot.buffer_snapshot();
        let start = buffer.anchor_before(MultiBufferOffset(0));
        let end = buffer.anchor_after(buffer.len());
        self.sorted_background_highlights_in_range(start..end, &snapshot, cx.theme())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn sorted_background_highlights_in_range(
        &self,
        search_range: Range<Anchor>,
        display_snapshot: &DisplaySnapshot,
        theme: &Theme,
    ) -> Vec<(Range<DisplayPoint>, Hsla)> {
        let mut res = self.background_highlights_in_range(search_range, display_snapshot, theme);
        res.sort_by(|a, b| {
            a.0.start
                .cmp(&b.0.start)
                .then_with(|| a.0.end.cmp(&b.0.end))
                .then_with(|| a.1.cmp(&b.1))
        });
        res
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn search_background_highlights(&mut self, cx: &mut Context<Self>) -> Vec<Range<Point>> {
        let snapshot = self.buffer().read(cx).snapshot(cx);

        let highlights = self
            .background_highlights
            .get(&HighlightKey::BufferSearchHighlights);

        if let Some((_color, ranges)) = highlights {
            ranges
                .iter()
                .map(|range| range.start.to_point(&snapshot)..range.end.to_point(&snapshot))
                .collect_vec()
        } else {
            vec![]
        }
    }

    pub fn has_background_highlights(&self, key: HighlightKey) -> bool {
        self.background_highlights
            .get(&key)
            .is_some_and(|(_, highlights)| !highlights.is_empty())
    }

    /// Returns all background highlights for a given range.
    ///
    /// The order of highlights is not deterministic, do sort the ranges if needed for the logic.
    pub fn background_highlights_in_range(
        &self,
        search_range: Range<Anchor>,
        display_snapshot: &DisplaySnapshot,
        theme: &Theme,
    ) -> Vec<(Range<DisplayPoint>, Hsla)> {
        let mut results = Vec::new();
        for (color_fetcher, ranges) in self.background_highlights.values() {
            let start_ix = match ranges.binary_search_by(|probe| {
                let cmp = probe
                    .end
                    .cmp(&search_range.start, &display_snapshot.buffer_snapshot());
                if cmp.is_gt() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }) {
                Ok(i) | Err(i) => i,
            };
            for (index, range) in ranges[start_ix..].iter().enumerate() {
                if range
                    .start
                    .cmp(&search_range.end, &display_snapshot.buffer_snapshot())
                    .is_ge()
                {
                    break;
                }

                let color = color_fetcher(&(start_ix + index), theme);
                let start = range.start.to_display_point(display_snapshot);
                let end = range.end.to_display_point(display_snapshot);
                results.push((start..end, color))
            }
        }
        results
    }

    pub fn gutter_highlights_in_range(
        &self,
        search_range: Range<Anchor>,
        display_snapshot: &DisplaySnapshot,
        cx: &App,
    ) -> Vec<(Range<DisplayPoint>, Hsla)> {
        let mut results = Vec::new();
        for (color_fetcher, ranges) in self.gutter_highlights.values() {
            let color = color_fetcher(cx);
            let start_ix = match ranges.binary_search_by(|probe| {
                let cmp = probe
                    .end
                    .cmp(&search_range.start, &display_snapshot.buffer_snapshot());
                if cmp.is_gt() {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }) {
                Ok(i) | Err(i) => i,
            };
            for range in &ranges[start_ix..] {
                if range
                    .start
                    .cmp(&search_range.end, &display_snapshot.buffer_snapshot())
                    .is_ge()
                {
                    break;
                }

                let start = range.start.to_display_point(display_snapshot);
                let end = range.end.to_display_point(display_snapshot);
                results.push((start..end, color))
            }
        }
        results
    }

    /// Get the text ranges corresponding to the redaction query
    pub fn redacted_ranges(
        &self,
        search_range: Range<Anchor>,
        display_snapshot: &DisplaySnapshot,
        cx: &App,
    ) -> Vec<Range<DisplayPoint>> {
        display_snapshot
            .buffer_snapshot()
            .redacted_ranges(search_range, |file| {
                if let Some(file) = file {
                    file.is_private()
                        && EditorSettings::get(
                            Some(SettingsLocation {
                                worktree_id: file.worktree_id(cx),
                                path: file.path().as_ref(),
                            }),
                            cx,
                        )
                        .redact_private_values
                } else {
                    false
                }
            })
            .map(|range| {
                range.start.to_display_point(display_snapshot)
                    ..range.end.to_display_point(display_snapshot)
            })
            .collect()
    }

    pub fn highlight_text_key(
        &mut self,
        key: HighlightKey,
        ranges: Vec<Range<Anchor>>,
        style: HighlightStyle,
        merge: bool,
        cx: &mut Context<Self>,
    ) {
        self.display_map.update(cx, |map, cx| {
            map.highlight_text(key, ranges, style, merge, cx);
        });
        cx.notify();
    }

    pub fn highlight_text(
        &mut self,
        key: HighlightKey,
        ranges: Vec<Range<Anchor>>,
        style: HighlightStyle,
        cx: &mut Context<Self>,
    ) {
        self.display_map.update(cx, |map, cx| {
            map.highlight_text(key, ranges, style, false, cx)
        });
        cx.notify();
    }

    pub fn text_highlights<'a>(
        &'a self,
        key: HighlightKey,
        cx: &'a App,
    ) -> Option<(HighlightStyle, &'a [Range<Anchor>])> {
        self.display_map.read(cx).text_highlights(key)
    }

    pub fn set_navigation_overlays(
        &mut self,
        key: NavigationOverlayKey,
        overlays: Vec<NavigationTargetOverlay>,
        cx: &mut Context<Self>,
    ) {
        let buffer_snapshot = self.buffer.read(cx).snapshot(cx);
        let mut covered_text_ranges = overlays
            .iter()
            .filter_map(|overlay| overlay.covered_text_range.clone())
            .collect::<Vec<_>>();
        covered_text_ranges.sort_by(|left, right| {
            left.start
                .cmp(&right.start, &buffer_snapshot)
                .then_with(|| left.end.cmp(&right.end, &buffer_snapshot))
        });

        self.display_map.update(cx, |map, cx| {
            map.clear_highlights(HighlightKey::NavigationOverlay(key));
            if !covered_text_ranges.is_empty() {
                map.highlight_text(
                    HighlightKey::NavigationOverlay(key),
                    covered_text_ranges,
                    HighlightStyle {
                        fade_out: Some(1.0),
                        ..Default::default()
                    },
                    false,
                    cx,
                );
            }
        });

        if overlays.is_empty() {
            self.navigation_overlays.remove(&key);
        } else {
            self.navigation_overlays.insert(key, Arc::from(overlays));
        }

        cx.notify();
    }

    pub fn clear_navigation_overlays(&mut self, key: NavigationOverlayKey, cx: &mut Context<Self>) {
        let removed = self.navigation_overlays.remove(&key).is_some();
        let cleared = self.display_map.update(cx, |map, _| {
            map.clear_highlights(HighlightKey::NavigationOverlay(key))
        });
        if removed || cleared {
            cx.notify();
        }
    }

    pub(crate) fn navigation_overlay_sets(
        &self,
    ) -> &HashMap<NavigationOverlayKey, Arc<[NavigationTargetOverlay]>> {
        &self.navigation_overlays
    }

    pub fn clear_highlights(&mut self, key: HighlightKey, cx: &mut Context<Self>) {
        let cleared = self
            .display_map
            .update(cx, |map, _| map.clear_highlights(key));
        if cleared {
            cx.notify();
        }
    }

    pub fn clear_highlights_with(
        &mut self,
        f: &mut dyn FnMut(&HighlightKey) -> bool,
        cx: &mut Context<Self>,
    ) {
        let cleared = self
            .display_map
            .update(cx, |map, _| map.clear_highlights_with(f));
        if cleared {
            cx.notify();
        }
    }

    pub fn show_local_cursors(&self, window: &mut Window, cx: &mut App) -> bool {
        (self.read_only(cx) || self.blink_manager.read(cx).visible())
            && self.focus_handle.is_focused(window)
    }

    pub fn set_show_cursor_when_unfocused(&mut self, is_enabled: bool, cx: &mut Context<Self>) {
        self.show_cursor_when_unfocused = is_enabled;
        cx.notify();
    }

    fn on_buffer_changed(&mut self, _: Entity<MultiBuffer>, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn on_buffer_event(
        &mut self,
        _multibuffer: &Entity<MultiBuffer>,
        event: &multi_buffer::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            multi_buffer::Event::Edited {
                edited_buffer,
                source: _,
            } => {
                self.scrollbar_marker_state.dirty = true;
                self.active_indent_guides_state.dirty = true;
                self.refresh_single_line_folds(window, cx);
                let snapshot = self.snapshot(window, cx);
                self.refresh_matching_bracket_highlights(&snapshot, cx);
                self.refresh_outline_symbols_at_cursor(cx);
                self.refresh_sticky_headers(&snapshot, cx);

                if let Some(buffer) = edited_buffer {
                    if buffer.read(cx).file().is_none() {
                        cx.emit(EditorEvent::TitleChanged);
                    }

                    if self.project.is_some() {
                        let buffer_id = buffer.read(cx).remote_id();
                        self.register_buffer(buffer_id, cx);
                        self.update_lsp_data(Some(buffer_id), window, cx);
                    }
                }

                cx.emit(EditorEvent::BufferEdited);
                cx.emit(SearchEvent::MatchesInvalidated);
            }
            multi_buffer::Event::BufferRangesUpdated {
                buffer,
                ranges,
                path_key,
            } => {
                self.refresh_document_highlights(cx);
                let buffer_id = buffer.read(cx).remote_id();
                self.register_visible_buffers(cx);
                self.update_lsp_data(Some(buffer_id), window, cx);
                self.bracket_fetched_tree_sitter_chunks
                    .retain(|range, _| range.start.buffer_id != buffer_id);
                self.refresh_selected_text_highlights(&self.display_snapshot(cx), true, window, cx);
                self.semantic_token_state.invalidate_buffer(&buffer_id);
                cx.emit(EditorEvent::BufferRangesUpdated {
                    buffer: buffer.clone(),
                    ranges: ranges.clone(),
                    path_key: path_key.clone(),
                });
            }
            multi_buffer::Event::BuffersRemoved { removed_buffer_ids } => {
                for buffer_id in removed_buffer_ids {
                    self.registered_buffers.remove(buffer_id);
                    self.semantic_token_state.invalidate_buffer(buffer_id);
                    self.lsp_document_symbols.remove(buffer_id);
                    self.display_map.update(cx, |display_map, cx| {
                        display_map.invalidate_semantic_highlights(*buffer_id);
                        display_map.clear_lsp_folding_ranges(*buffer_id, cx);
                    });
                }

                self.display_map.update(cx, |display_map, cx| {
                    display_map.unfold_buffers(removed_buffer_ids.iter().copied(), cx);
                });

                cx.emit(EditorEvent::BuffersRemoved {
                    removed_buffer_ids: removed_buffer_ids.clone(),
                });
            }
            multi_buffer::Event::BuffersEdited { buffer_ids } => {
                self.display_map.update(cx, |map, cx| {
                    map.unfold_buffers(buffer_ids.iter().copied(), cx)
                });
                cx.emit(EditorEvent::BuffersEdited {
                    buffer_ids: buffer_ids.clone(),
                });
            }
            multi_buffer::Event::Reparsed(buffer_id) => {
                self.refresh_selected_text_highlights(&self.display_snapshot(cx), true, window, cx);

                cx.emit(EditorEvent::Reparsed(*buffer_id));
            }
            multi_buffer::Event::LanguageChanged(buffer_id, is_fresh_language) => {
                if !is_fresh_language {
                    self.registered_buffers.remove(&buffer_id);
                }
                cx.emit(EditorEvent::Reparsed(*buffer_id));
                cx.notify();
            }
            multi_buffer::Event::DirtyChanged => cx.emit(EditorEvent::DirtyChanged),
            multi_buffer::Event::Saved => cx.emit(EditorEvent::Saved),
            multi_buffer::Event::FileHandleChanged => {
                cx.emit(EditorEvent::TitleChanged);
                cx.emit(EditorEvent::FileHandleChanged);
            }
            multi_buffer::Event::Reloaded | multi_buffer::Event::BufferDiffChanged => {
                cx.emit(EditorEvent::TitleChanged)
            }
            _ => {}
        };
    }

    fn on_display_map_changed(
        &mut self,
        _: Entity<DisplayMap>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.notify();
    }

    fn fetch_applicable_language_settings(
        &self,
        cx: &App,
    ) -> HashMap<Option<LanguageName>, LanguageSettings> {
        if !self.mode.is_full() {
            return HashMap::default();
        }

        self.buffer().read(cx).all_buffers().into_iter().fold(
            HashMap::default(),
            |mut acc, buffer| {
                let buffer = buffer.read(cx);
                let language = buffer.language().map(|language| language.name());
                if let hash_map::Entry::Vacant(v) = acc.entry(language) {
                    v.insert(LanguageSettings::for_buffer(&buffer, cx).into_owned());
                }
                acc
            },
        )
    }

    fn settings_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let new_language_settings = self.fetch_applicable_language_settings(cx);
        let language_settings_changed = new_language_settings != self.applicable_language_settings;
        self.applicable_language_settings = new_language_settings;

        let old_cursor_shape = self.cursor_shape;
        let old_breadcrumbs_visible = self.breadcrumbs_visible();

        {
            let editor_settings = EditorSettings::get_global(cx);
            self.scroll_manager.vertical_scroll_margin = editor_settings.vertical_scroll_margin;
            if self.breadcrumbs_visibility.settings_visibility()
                != editor_settings.toolbar.breadcrumbs
            {
                self.breadcrumbs_visibility =
                    BreadcrumbsVisibility::new(editor_settings.toolbar.breadcrumbs);
            }
            self.cursor_shape = editor_settings.cursor_shape.unwrap_or_default();

            if self.mode.could_have_minimap() {
                self.set_show_minimap(editor_settings.minimap.show, window, cx);
            }
        }

        if old_cursor_shape != self.cursor_shape {
            cx.emit(EditorEvent::CursorShapeChanged);
        }

        if old_breadcrumbs_visible != self.breadcrumbs_visible() {
            cx.emit(EditorEvent::BreadcrumbsChanged);
        }

        let restore_unsaved_buffers = {
            let project_settings = ProjectSettings::get_global(cx);
            project_settings.session.restore_unsaved_buffers
        };
        self.buffer_serialization = self
            .should_serialize_buffer()
            .then(|| BufferSerialization::new(restore_unsaved_buffers));

        if self.mode.is_full() {
            if language_settings_changed {
                self.clear_disabled_lsp_folding_ranges(window, cx);
                self.refresh_document_symbols(None, cx);
            }

            if let Some(_) = self.colors.as_mut().and_then(|colors| {
                colors.render_mode_updated(EditorSettings::get_global(cx).lsp_document_colors)
            }) {
                self.refresh_document_colors(None, window, cx);
            }

            let new_semantic_token_rules = ProjectSettings::get_global(cx)
                .global_lsp_settings
                .semantic_token_rules
                .clone();
            let semantic_token_rules_changed = self
                .semantic_token_state
                .update_rules(new_semantic_token_rules);
            if language_settings_changed || semantic_token_rules_changed {
                self.invalidate_semantic_tokens(None);
                self.refresh_semantic_tokens(None, None, cx);
            }
        }

        cx.notify();
    }

    fn theme_changed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if !self.mode.is_full() {
            return;
        }

        self.invalidate_semantic_tokens(None);
        self.refresh_semantic_tokens(None, None, cx);
        self.refresh_outline_symbols_at_cursor(cx);
    }

    pub fn set_searchable(&mut self, searchable: bool) {
        self.searchable = searchable;
    }

    pub fn searchable(&self) -> bool {
        self.searchable
    }

    pub fn open_excerpts_in_split(
        &mut self,
        _: &OpenExcerptsSplit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_excerpts_common(None, true, window, cx)
    }

    pub fn open_excerpts(&mut self, _: &OpenExcerpts, window: &mut Window, cx: &mut Context<Self>) {
        self.open_excerpts_common(None, false, window, cx)
    }

    pub(crate) fn open_excerpts_common(
        &mut self,
        _jump_data: Option<JumpData>,
        _split: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.propagate();
    }

    fn selection_replacement_ranges(
        &self,
        range: Range<MultiBufferOffsetUtf16>,
        cx: &mut App,
    ) -> Vec<Range<MultiBufferOffsetUtf16>> {
        let selections = self
            .selections
            .all::<MultiBufferOffsetUtf16>(&self.display_snapshot(cx));
        let newest_selection = selections
            .iter()
            .max_by_key(|selection| selection.id)
            .unwrap();
        let start_delta = range.start.0.0 as isize - newest_selection.start.0.0 as isize;
        let end_delta = range.end.0.0 as isize - newest_selection.end.0.0 as isize;
        let snapshot = self.buffer.read(cx).read(cx);
        selections
            .into_iter()
            .map(|mut selection| {
                selection.start.0.0 =
                    (selection.start.0.0 as isize).saturating_add(start_delta) as usize;
                selection.end.0.0 = (selection.end.0.0 as isize).saturating_add(end_delta) as usize;
                snapshot.clip_offset_utf16(selection.start, Bias::Left)
                    ..snapshot.clip_offset_utf16(selection.end, Bias::Right)
            })
            .collect()
    }

    /// Copy the highlighted chunks to the clipboard as JSON. The format is an array of lines,
    /// with each line being an array of {text, highlight} objects.
    fn copy_highlight_json(
        &mut self,
        _: &CopyHighlightJson,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[derive(Serialize)]
        struct Chunk<'a> {
            text: String,
            highlight: Option<&'a str>,
        }

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let mut selection = self.selections.newest::<Point>(&self.display_snapshot(cx));
        let max_point = snapshot.max_point();

        let range = if self.selections.line_mode() {
            selection.start = Point::new(selection.start.row, 0);
            selection.end = cmp::min(max_point, Point::new(selection.end.row + 1, 0));
            selection.goal = SelectionGoal::None;
            selection.range()
        } else if selection.is_empty() {
            Point::new(0, 0)..max_point
        } else {
            selection.range()
        };

        let chunks = snapshot.chunks(
            range,
            LanguageAwareStyling {
                tree_sitter: true,
                diagnostics: true,
            },
        );
        let mut lines = Vec::new();
        let mut line: VecDeque<Chunk> = VecDeque::new();

        let Some(style) = self.style.as_ref() else {
            return;
        };

        for chunk in chunks {
            let highlight = chunk
                .syntax_highlight_id
                .and_then(|id| style.syntax.get_capture_name(id));

            let mut chunk_lines = chunk.text.split('\n').peekable();
            while let Some(text) = chunk_lines.next() {
                let mut merged_with_last_token = false;
                if let Some(last_token) = line.back_mut()
                    && last_token.highlight == highlight
                {
                    last_token.text.push_str(text);
                    merged_with_last_token = true;
                }

                if !merged_with_last_token {
                    line.push_back(Chunk {
                        text: text.into(),
                        highlight,
                    });
                }

                if chunk_lines.peek().is_some() {
                    if line.len() > 1 && line.front().unwrap().text.is_empty() {
                        line.pop_front();
                    }
                    if line.len() > 1 && line.back().unwrap().text.is_empty() {
                        line.pop_back();
                    }

                    lines.push(mem::take(&mut line));
                }
            }
        }

        if line.iter().any(|chunk| !chunk.text.is_empty()) {
            lines.push(line);
        }

        let Some(lines) = serde_json::to_string_pretty(&lines).log_err() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(lines));
    }

    pub fn open_context_menu(
        &mut self,
        _: &OpenContextMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_autoscroll(Autoscroll::newest(), cx);
        let position = self
            .selections
            .newest_display(&self.display_snapshot(cx))
            .start;
        mouse_context_menu::deploy_context_menu(self, None, position, window, cx);
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    fn handle_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::Focused);

        if let Some(descendant) = self
            .last_focused_descendant
            .take()
            .and_then(|descendant| descendant.upgrade())
        {
            window.focus(&descendant, cx);
        } else {
            self.blink_manager.update(cx, BlinkManager::enable);
            self.show_cursor_names(window, cx);
            self.buffer.update(cx, |buffer, cx| {
                buffer.finalize_last_transaction(cx);
                if self.leader_id.is_none() {
                    buffer.set_active_selections(
                        &self.selections.disjoint_anchors_arc(),
                        self.selections.line_mode(),
                        self.cursor_shape,
                        cx,
                    );
                }
            });

            if cx.is_cursor_visible()
                && let Some(position_map) = self.last_position_map.clone()
            {
                EditorElement::mouse_moved(
                    self,
                    &MouseMoveEvent {
                        position: window.mouse_position(),
                        pressed_button: None,
                        modifiers: window.modifiers(),
                    },
                    &position_map,
                    None,
                    window,
                    cx,
                );
            }
        }
    }

    fn handle_focus_in(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::FocusedIn)
    }

    fn handle_focus_out(
        &mut self,
        event: FocusOutEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if event.blurred != self.focus_handle {
            self.last_focused_descendant = Some(event.blurred);
        }
        self.selection_drag_state = SelectionDragState::None;
    }

    pub fn handle_blur(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.blink_manager.update(cx, BlinkManager::disable);
        self.buffer
            .update(cx, |buffer, cx| buffer.remove_active_selections(cx));
        cx.emit(EditorEvent::Blurred);
        cx.notify();
    }

    pub fn register_action_renderer(
        &mut self,
        listener: impl Fn(&Editor, &mut Window, &mut Context<Editor>) + 'static,
    ) -> Subscription {
        let id = self.next_editor_action_id.post_inc();
        self.editor_actions
            .borrow_mut()
            .insert(id, Box::new(listener));

        let editor_actions = self.editor_actions.clone();
        Subscription::new(move || {
            editor_actions.borrow_mut().remove(&id);
        })
    }

    pub fn register_action<A: Action>(
        &mut self,
        listener: impl Fn(&A, &mut Window, &mut App) + 'static,
    ) -> Subscription {
        let id = self.next_editor_action_id.post_inc();
        let listener = Arc::new(listener);
        self.editor_actions.borrow_mut().insert(
            id,
            Box::new(move |_, window, _| {
                let listener = listener.clone();
                window.on_action(TypeId::of::<A>(), move |action, phase, window, cx| {
                    let action = action.downcast_ref().unwrap();
                    if phase == DispatchPhase::Bubble {
                        listener(action, window, cx)
                    }
                })
            }),
        );

        let editor_actions = self.editor_actions.clone();
        Subscription::new(move || {
            editor_actions.borrow_mut().remove(&id);
        })
    }

    pub fn file_header_size(&self) -> u32 {
        FILE_HEADER_HEIGHT
    }

    pub fn restore(
        &mut self,
        revert_changes: HashMap<BufferId, Vec<(Range<text::Anchor>, Rope)>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer().update(cx, |multi_buffer, cx| {
            for (buffer_id, changes) in revert_changes {
                if let Some(buffer) = multi_buffer.buffer(buffer_id) {
                    buffer.update(cx, |buffer, cx| {
                        buffer.edit(
                            changes
                                .into_iter()
                                .map(|(range, text)| (range, text.to_string())),
                            None,
                            cx,
                        );
                    });
                }
            }
        });
        let selections = self
            .selections
            .all::<MultiBufferOffset>(&self.display_snapshot(cx));
        self.change_selections(SelectionEffects::no_scroll(), window, cx, |s| {
            s.select(selections);
        });
    }

    pub fn to_pixel_point(
        &mut self,
        source: Anchor,
        editor_snapshot: &EditorSnapshot,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<gpui::Point<Pixels>> {
        let source_point = source.to_display_point(editor_snapshot);
        self.display_to_pixel_point(source_point, editor_snapshot, window, cx)
    }

    pub fn display_to_pixel_point(
        &mut self,
        source: DisplayPoint,
        editor_snapshot: &EditorSnapshot,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<gpui::Point<Pixels>> {
        let line_height = self.style(cx).text.line_height_in_pixels(window.rem_size());
        let text_layout_details = self.text_layout_details(window, cx);
        let scroll_top = text_layout_details
            .scroll_anchor
            .scroll_position(editor_snapshot)
            .y;

        if source.row().as_f64() < scroll_top.floor() {
            return None;
        }
        let source_x = editor_snapshot.x_for_display_point(source, &text_layout_details);
        let source_y = line_height * (source.row().as_f64() - scroll_top) as f32;
        Some(gpui::Point::new(source_x, source_y))
    }

    pub fn register_addon<T: Addon>(&mut self, instance: T) {
        if self.mode.is_minimap() {
            return;
        }
        self.addons
            .insert(std::any::TypeId::of::<T>(), Box::new(instance));
    }

    pub fn unregister_addon<T: Addon>(&mut self) {
        self.addons.remove(&std::any::TypeId::of::<T>());
    }

    pub fn addon<T: Addon>(&self) -> Option<&T> {
        let type_id = std::any::TypeId::of::<T>();
        self.addons
            .get(&type_id)
            .and_then(|item| item.to_any().downcast_ref::<T>())
    }

    pub fn addon_mut<T: Addon>(&mut self) -> Option<&mut T> {
        let type_id = std::any::TypeId::of::<T>();
        self.addons
            .get_mut(&type_id)
            .and_then(|item| item.to_any_mut()?.downcast_mut::<T>())
    }

    fn character_dimensions(&self, window: &mut Window, cx: &mut App) -> CharacterDimensions {
        let text_layout_details = self.text_layout_details(window, cx);
        let style = &text_layout_details.editor_style;
        let font_id = window.text_system().resolve_font(&style.text.font());
        let font_size = style.text.font_size.to_pixels(window.rem_size());
        let line_height = style.text.line_height_in_pixels(window.rem_size());
        let em_width = window.text_system().em_width(font_id, font_size).unwrap();
        let em_advance = window.text_system().em_advance(font_id, font_size).unwrap();

        CharacterDimensions {
            em_width,
            em_advance,
            line_height,
        }
    }

    fn lsp_data_enabled(&self) -> bool {
        self.enable_lsp_data && self.mode().is_full()
    }

    fn update_lsp_data(
        &mut self,
        for_buffer: Option<BufferId>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.lsp_data_enabled() {
            return;
        }

        self.refresh_semantic_tokens(for_buffer, None, cx);
        self.refresh_document_colors(for_buffer, window, cx);
        self.refresh_folding_ranges(for_buffer, window, cx);
        self.refresh_document_symbols(for_buffer, cx);
    }

    fn register_visible_buffers(&mut self, cx: &mut Context<Self>) {
        if !self.lsp_data_enabled() {
            return;
        }
        let visible_buffers: Vec<_> = self
            .visible_buffers(cx)
            .into_iter()
            .filter(|buffer| self.is_lsp_relevant(buffer.read(cx).file(), cx))
            .collect();
        for visible_buffer in visible_buffers {
            self.register_buffer(visible_buffer.read(cx).remote_id(), cx);
        }
    }

    fn register_buffer(&mut self, buffer_id: BufferId, cx: &mut Context<Self>) {
        if !self.lsp_data_enabled() {
            return;
        }

        if !self.registered_buffers.contains_key(&buffer_id)
            && let Some(project) = self.project.as_ref()
        {
            if let Some(buffer) = self.buffer.read(cx).buffer(buffer_id) {
                project.update(cx, |project, cx| {
                    self.registered_buffers.insert(
                        buffer_id,
                        project.register_buffer_with_language_servers(&buffer, cx),
                    );
                });
            } else {
                self.registered_buffers.remove(&buffer_id);
            }
        }
    }

    fn create_style(&self, cx: &App) -> EditorStyle {
        let settings = ThemeSettings::get_global(cx);

        let mut text_style = match self.mode {
            EditorMode::SingleLine | EditorMode::AutoHeight { .. } => TextStyle {
                color: cx.theme().colors().editor_foreground,
                font_family: settings.ui_font.family.clone(),
                font_features: settings.ui_font.features.clone(),
                font_fallbacks: settings.ui_font.fallbacks.clone(),
                font_size: rems(0.875).into(),
                font_weight: settings.ui_font.weight,
                line_height: relative(settings.buffer_line_height.value()),
                ..Default::default()
            },
            EditorMode::Full { .. } | EditorMode::Minimap { .. } => TextStyle {
                color: cx.theme().colors().editor_foreground,
                font_family: settings.buffer_font.family.clone(),
                font_features: settings.buffer_font.features.clone(),
                font_fallbacks: settings.buffer_font.fallbacks.clone(),
                font_size: settings.buffer_font_size(cx).into(),
                font_weight: settings.buffer_font.weight,
                line_height: relative(settings.buffer_line_height.value()),
                ..Default::default()
            },
        };
        if let Some(text_style_refinement) = &self.text_style_refinement {
            text_style.refine(text_style_refinement)
        }

        let background = match self.mode {
            EditorMode::SingleLine => cx.theme().system().transparent,
            EditorMode::AutoHeight { .. } => cx.theme().system().transparent,
            EditorMode::Full { .. } => cx.theme().colors().editor_background,
            EditorMode::Minimap { .. } => cx.theme().colors().editor_background.opacity(0.7),
        };

        EditorStyle {
            background,
            border: cx.theme().colors().border,
            local_player: cx.theme().players().local(),
            text: text_style,
            scrollbar_width: EditorElement::SCROLLBAR_WIDTH,
            syntax: cx.theme().syntax().clone(),
            status: cx.theme().status().clone(),
            unnecessary_code_fade: settings.unnecessary_code_fade,
        }
    }

    fn breadcrumbs_inner(&self, cx: &App) -> Option<Vec<HighlightedText>> {
        let multibuffer = self.buffer().read(cx);
        let (buffer_id, symbols) = self.outline_symbols_at_cursor.as_ref()?;
        let buffer = multibuffer.buffer(*buffer_id)?;

        let buffer = buffer.read(cx);
        // In a multi-buffer layout, we don't want to include the filename in the breadcrumbs
        let mut breadcrumbs = {
            let text = self.breadcrumb_header.clone().unwrap_or_else(|| {
                buffer
                    .snapshot()
                    .resolve_file_path(
                        self.project
                            .as_ref()
                            .map(|project| project.read(cx).visible_worktrees(cx).count() > 1)
                            .unwrap_or_default(),
                        cx,
                    )
                    .unwrap_or_else(|| {
                        multibuffer.title(cx).to_string()
                    })
            });
            vec![HighlightedText {
                text: text.into(),
                highlights: vec![],
            }]
        };

        breadcrumbs.extend(symbols.iter().map(|symbol| HighlightedText {
            text: symbol.text.clone().into(),
            highlights: symbol.highlight_ranges.clone(),
        }));
        Some(breadcrumbs)
    }

    pub fn disable_mouse_wheel_zoom(&mut self) {
        self.enable_mouse_wheel_zoom = false;
    }

    fn update_data_on_scroll(
        &mut self,
        debounce: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if debounce {
            self.post_scroll_update = cx.spawn_in(window, async move |editor, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                editor
                    .update_in(cx, |editor, window, cx| {
                        editor.do_update_data_on_scroll(window, cx);
                    })
                    .ok();
            });
        } else {
            self.post_scroll_update = Task::ready(());
            self.do_update_data_on_scroll(window, cx);
        }
    }

    fn do_update_data_on_scroll(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        self.register_visible_buffers(cx);

        if self.needs_initial_data_update {
            self.needs_initial_data_update = false;
            self.update_lsp_data(None, window, cx);
        }
    }

    /// Returns the current cursor's vertical offset, in display rows, from the
    /// top of the visible viewport.
    /// Returns `None` if the cursor is not currently on screen.
    pub fn cursor_top_offset(&self, cx: &mut Context<Self>) -> Option<ScrollOffset> {
        let visible = self.visible_line_count()?;
        let display_map = self.display_map.update(cx, |map, cx| map.snapshot(cx));
        let scroll_top = self.scroll_manager.scroll_position(&display_map, cx).y;
        let cursor_display_row = self
            .selections
            .newest::<Point>(&display_map)
            .head()
            .to_display_point(&display_map)
            .row()
            .as_f64();

        match cursor_display_row - scroll_top {
            offset if offset < 0.0 || offset >= visible => None,
            offset => Some(offset),
        }
    }
}

pub trait SemanticsProvider {
    fn hover(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Option<Task<Option<Vec<project::Hover>>>>;

    fn semantic_tokens(
        &self,
        buffer: Entity<Buffer>,
        refresh: Option<RefreshForServer>,
        cx: &mut App,
    ) -> Option<Shared<Task<std::result::Result<BufferSemanticTokens, Arc<anyhow::Error>>>>>;

    fn supports_semantic_tokens(&self, buffer: &Entity<Buffer>, cx: &mut App) -> bool;

    fn document_highlights(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Option<Task<Result<Vec<DocumentHighlight>>>>;

    fn range_for_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Task<Result<Option<Range<text::Anchor>>>>;

    fn perform_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        new_name: String,
        cx: &mut App,
    ) -> Option<Task<Result<ProjectTransaction>>>;
}

impl SemanticsProvider for WeakEntity<Project> {
    fn hover(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Option<Task<Option<Vec<project::Hover>>>> {
        self.update(cx, |project, cx| project.hover(buffer, position, cx))
            .ok()
    }

    fn document_highlights(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Option<Task<Result<Vec<DocumentHighlight>>>> {
        self.update(cx, |project, cx| {
            project.document_highlights(buffer, position, cx)
        })
        .ok()
    }

    fn supports_semantic_tokens(&self, buffer: &Entity<Buffer>, cx: &mut App) -> bool {
        self.update(cx, |project, cx| {
            buffer.update(cx, |buffer, cx| {
                project.any_language_server_supports_semantic_tokens(buffer, cx)
            })
        })
        .unwrap_or(false)
    }

    fn semantic_tokens(
        &self,
        buffer: Entity<Buffer>,
        refresh: Option<RefreshForServer>,
        cx: &mut App,
    ) -> Option<Shared<Task<std::result::Result<BufferSemanticTokens, Arc<anyhow::Error>>>>> {
        self.update(cx, |this, cx| {
            this.lsp_store().update(cx, |lsp_store, cx| {
                lsp_store.semantic_tokens(buffer, refresh, cx)
            })
        })
        .ok()
    }

    fn range_for_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        cx: &mut App,
    ) -> Task<Result<Option<Range<text::Anchor>>>> {
        let Some(this) = self.upgrade() else {
            return Task::ready(Ok(None));
        };

        this.update(cx, |project, cx| {
            let buffer = buffer.clone();
            let task = project.prepare_rename(buffer.clone(), position, cx);
            cx.spawn(async move |_, cx| {
                Ok(match task.await? {
                    PrepareRenameResponse::Success(range) => Some(range),
                    PrepareRenameResponse::InvalidPosition => None,
                    PrepareRenameResponse::OnlyUnpreparedRenameSupported => {
                        // Fallback on using TreeSitter info to determine identifier range
                        buffer.read_with(cx, |buffer, _| {
                            let snapshot = buffer.snapshot();
                            let (range, kind) = snapshot.surrounding_word(position);
                            if kind != Some(CharKind::Word) {
                                return None;
                            }
                            Some(
                                snapshot.anchor_before(range.start)
                                    ..snapshot.anchor_after(range.end),
                            )
                        })
                    }
                })
            })
        })
    }

    fn perform_rename(
        &self,
        buffer: &Entity<Buffer>,
        position: text::Anchor,
        new_name: String,
        cx: &mut App,
    ) -> Option<Task<Result<ProjectTransaction>>> {
        self.update(cx, |project, cx| {
            project.perform_rename(buffer.clone(), position, new_name, cx)
        })
        .ok()
    }
}

fn consume_contiguous_rows(
    contiguous_row_selections: &mut Vec<Selection<Point>>,
    selection: &Selection<Point>,
    display_map: &DisplaySnapshot,
    selections: &mut Peekable<std::slice::Iter<Selection<Point>>>,
) -> (MultiBufferRow, MultiBufferRow) {
    contiguous_row_selections.push(selection.clone());
    let start_row = starting_row(selection, display_map);
    let mut end_row = ending_row(selection, display_map);

    while let Some(next_selection) = selections.peek() {
        if next_selection.start.row <= end_row.0 {
            end_row = ending_row(next_selection, display_map);
            contiguous_row_selections.push(selections.next().unwrap().clone());
        } else {
            break;
        }
    }
    (start_row, end_row)
}

fn starting_row(selection: &Selection<Point>, display_map: &DisplaySnapshot) -> MultiBufferRow {
    if selection.start.column > 0 {
        MultiBufferRow(display_map.prev_line_boundary(selection.start).0.row)
    } else {
        MultiBufferRow(selection.start.row)
    }
}

fn ending_row(next_selection: &Selection<Point>, display_map: &DisplaySnapshot) -> MultiBufferRow {
    if next_selection.end.column > 0 || next_selection.is_empty() {
        MultiBufferRow(display_map.next_line_boundary(next_selection.end).0.row + 1)
    } else {
        MultiBufferRow(next_selection.end.row)
    }
}

impl EditorSnapshot {
    pub fn language_at<T: ToOffset>(&self, position: T) -> Option<&Arc<Language>> {
        self.display_snapshot
            .buffer_snapshot()
            .language_at(position)
    }

    pub fn is_focused(&self) -> bool {
        self.is_focused
    }

    pub fn placeholder_text(&self) -> Option<String> {
        self.placeholder_display_snapshot
            .as_ref()
            .map(|display_map| display_map.text())
    }

    pub fn scroll_position(&self) -> gpui::Point<ScrollOffset> {
        self.scroll_anchor.scroll_position(&self.display_snapshot)
    }

    pub fn max_line_number_width(&self, style: &EditorStyle, window: &mut Window) -> Pixels {
        let digit_count = self.widest_line_number().ilog10() + 1;
        column_pixels(style, digit_count as usize, window)
    }

    pub fn gutter_dimensions(
        &self,
        font_id: FontId,
        font_size: Pixels,
        style: &EditorStyle,
        window: &mut Window,
        cx: &App,
    ) -> GutterDimensions {
        if self.show_gutter
            && let Some(ch_width) = cx.text_system().ch_width(font_id, font_size).log_err()
            && let Some(ch_advance) = cx.text_system().ch_advance(font_id, font_size).log_err()
        {
            let show_git_gutter = self.show_git_diff_gutter.unwrap_or_else(|| {
                matches!(
                    ProjectSettings::get_global(cx).git.git_gutter,
                    GitGutterSetting::TrackedFiles
                )
            });
            let gutter_settings = EditorSettings::get_global(cx).gutter;
            let show_line_numbers = self
                .show_line_numbers
                .unwrap_or(gutter_settings.line_numbers);
            let line_gutter_width = if show_line_numbers {
                // Avoid flicker-like gutter resizes when the line number gains another digit by
                // only resizing the gutter on files with > 10**min_line_number_digits lines.
                let min_width_for_number_on_gutter =
                    ch_advance * gutter_settings.min_line_number_digits as f32;
                self.max_line_number_width(style, window)
                    .max(min_width_for_number_on_gutter)
            } else {
                0.0.into()
            };

            let left_padding = Pixels::ZERO
                + if show_git_gutter && show_line_numbers {
                    ch_width * 2.0
                } else if show_git_gutter || show_line_numbers {
                    ch_width
                } else {
                    px(0.)
                };

            let shows_folds = gutter_settings.folds;

            let right_padding = if shows_folds && show_line_numbers {
                ch_width * 4.0
            } else if shows_folds {
                ch_width * 3.0
            } else if show_line_numbers {
                ch_width
            } else {
                px(0.)
            };

            GutterDimensions {
                left_padding,
                right_padding,
                width: line_gutter_width + left_padding + right_padding,
                margin: GutterDimensions::default_gutter_margin(font_id, font_size, cx),
            }
        } else if self.offset_content {
            GutterDimensions::default_with_margin(font_id, font_size, cx)
        } else {
            GutterDimensions::default()
        }
    }

    /// Returns the line delta from `base` to `line` in the multibuffer, ignoring wrapped lines.
    ///
    /// This is positive if `base` is before `line`.
    fn relative_line_delta(
        &self,
        current_selection_head: DisplayRow,
        first_visible_row: DisplayRow,
        consider_wrapped_lines: bool,
    ) -> i64 {
        let current_selection_head = current_selection_head.as_display_point().to_point(self);
        let first_visible_row = first_visible_row.as_display_point().to_point(self);

        if consider_wrapped_lines {
            let wrap_snapshot = self.wrap_snapshot();
            let base_wrap_row = wrap_snapshot
                .make_wrap_point(current_selection_head, Bias::Left)
                .row();
            let wrap_row = wrap_snapshot
                .make_wrap_point(first_visible_row, Bias::Left)
                .row();

            wrap_row.0 as i64 - base_wrap_row.0 as i64
        } else {
            let fold_snapshot = self.fold_snapshot();
            let base_fold_row = fold_snapshot
                .to_fold_point(self.to_inlay_point(current_selection_head), Bias::Left)
                .row();
            let fold_row = fold_snapshot
                .to_fold_point(self.to_inlay_point(first_visible_row), Bias::Left)
                .row();

            fold_row as i64 - base_fold_row as i64
        }
    }

    /// Returns the unsigned relative line number to display for each row in `rows`.
    ///
    /// Wrapped rows are excluded from the hashmap if `count_relative_lines` is `false`.
    pub fn calculate_relative_line_numbers(
        &self,
        rows: &Range<DisplayRow>,
        current_selection_head: DisplayRow,
        count_wrapped_lines: bool,
    ) -> HashMap<DisplayRow, u32> {
        let initial_offset =
            self.relative_line_delta(current_selection_head, rows.start, count_wrapped_lines);

        self.row_infos(rows.start)
            .take(rows.len())
            .enumerate()
            .map(|(i, row_info)| (DisplayRow(rows.start.0 + i as u32), row_info))
            .filter(|(_row, row_info)| {
                row_info.buffer_row.is_some()
                    || (count_wrapped_lines && row_info.wrapped_buffer_row.is_some())
            })
            .enumerate()
            .filter_map(|(i, (row, _row_info))| {
                // We want to ensure here that the current line has absolute
                // numbering, even if we are in a soft-wrapped line. With the
                // exception that if we are in a deleted line, we should number this
                // relative with 0, as otherwise it would have no line number at all
                let relative_line_number = (initial_offset + i as i64).unsigned_abs() as u32;

                (relative_line_number != 0)
                .then_some((row, relative_line_number))
            })
            .collect()
    }
}

pub fn column_pixels(style: &EditorStyle, column: usize, window: &Window) -> Pixels {
    let font_size = style.text.font_size.to_pixels(window.rem_size());
    let layout = window.text_system().shape_line(
        SharedString::from(" ".repeat(column)),
        font_size,
        &[TextRun {
            len: column,
            font: style.text.font(),
            color: Hsla::default(),
            ..Default::default()
        }],
        None,
    );

    layout.width
}

impl Deref for EditorSnapshot {
    type Target = DisplaySnapshot;

    fn deref(&self) -> &Self::Target {
        &self.display_snapshot
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorEvent {
    /// Emitted when the stored review comments change (added, removed, or updated).
    ReviewCommentsChanged {
        /// The new total count of review comments.
        total_count: usize,
    },
    InputIgnored {
        text: Arc<str>,
    },
    InputHandled {
        utf16_range_to_replace: Option<Range<isize>>,
        text: Arc<str>,
    },
    BufferRangesUpdated {
        buffer: Entity<Buffer>,
        path_key: PathKey,
        ranges: Vec<ExcerptRange<text::Anchor>>,
    },
    BuffersRemoved {
        removed_buffer_ids: Vec<BufferId>,
    },
    BuffersEdited {
        buffer_ids: Vec<BufferId>,
    },
    BufferFoldToggled {
        ids: Vec<BufferId>,
        folded: bool,
    },
    BufferEdited,
    Edited {
        transaction_id: clock::Lamport,
    },
    Reparsed(BufferId),
    Focused,
    FocusedIn,
    Blurred,
    DirtyChanged,
    Saved,
    TitleChanged,
    FileHandleChanged,
    SelectionsChanged {
        local: bool,
    },
    ScrollPositionChanged {
        local: bool,
        autoscroll: bool,
    },
    TransactionUndone {
        transaction_id: clock::Lamport,
    },
    TransactionBegun {
        transaction_id: clock::Lamport,
    },
    CursorShapeChanged,
    BreadcrumbsChanged,
    OutlineSymbolsChanged,
    PushedToNavHistory {
        anchor: Anchor,
        is_deactivate: bool,
    },
}

impl EventEmitter<EditorEvent> for Editor {}

impl Focusable for Editor {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        EditorElement::new(&cx.entity(), self.create_style(cx))
    }
}

trait SelectionExt {
    fn display_range(&self, map: &DisplaySnapshot) -> Range<DisplayPoint>;
    fn spanned_rows(
        &self,
        include_end_if_at_line_start: bool,
        map: &DisplaySnapshot,
    ) -> Range<MultiBufferRow>;
}

impl<T: ToPoint + ToOffset> SelectionExt for Selection<T> {
    fn display_range(&self, map: &DisplaySnapshot) -> Range<DisplayPoint> {
        let start = self
            .start
            .to_point(map.buffer_snapshot())
            .to_display_point(map);
        let end = self
            .end
            .to_point(map.buffer_snapshot())
            .to_display_point(map);
        if self.reversed {
            end..start
        } else {
            start..end
        }
    }

    fn spanned_rows(
        &self,
        include_end_if_at_line_start: bool,
        map: &DisplaySnapshot,
    ) -> Range<MultiBufferRow> {
        let start = self.start.to_point(map.buffer_snapshot());
        let mut end = self.end.to_point(map.buffer_snapshot());
        if !include_end_if_at_line_start && start.row != end.row && end.column == 0 {
            end.row -= 1;
        }

        let buffer_start = map.prev_line_boundary(start).0;
        let buffer_end = map.next_line_boundary(end).0;
        MultiBufferRow(buffer_start.row)..MultiBufferRow(buffer_end.row + 1)
    }
}

#[derive(Clone)]
struct ErasedEditorImpl(Entity<Editor>);

impl ui_input::ErasedEditor for ErasedEditorImpl {
    fn text(&self, cx: &App) -> String {
        self.0.read(cx).text(cx)
    }

    fn set_text(&self, text: &str, window: &mut Window, cx: &mut App) {
        self.0.update(cx, |this, cx| {
            this.set_text(text, window, cx);
        })
    }

    fn clear(&self, window: &mut Window, cx: &mut App) {
        self.0.update(cx, |this, cx| this.clear(window, cx));
    }

    fn set_placeholder_text(&self, text: &str, window: &mut Window, cx: &mut App) {
        self.0.update(cx, |this, cx| {
            this.set_placeholder_text(text, window, cx);
        });
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.0.read(cx).focus_handle(cx)
    }

    fn render(&self, _: &mut Window, cx: &App) -> AnyElement {
        let settings = ThemeSettings::get_global(cx);
        let theme_color = cx.theme().colors();

        let text_style = TextStyle {
            font_family: settings.ui_font.family.clone(),
            font_features: settings.ui_font.features.clone(),
            font_size: rems(0.875).into(),
            font_weight: settings.ui_font.weight,
            font_style: FontStyle::Normal,
            line_height: relative(1.2),
            color: theme_color.text,
            ..Default::default()
        };
        let editor_style = EditorStyle {
            background: theme_color.ghost_element_background,
            local_player: cx.theme().players().local(),
            syntax: cx.theme().syntax().clone(),
            text: text_style,
            ..Default::default()
        };
        EditorElement::new(&self.0, editor_style).into_any()
    }

    fn as_any(&self) -> &dyn Any {
        &self.0
    }

    fn move_selection_to_end(&self, window: &mut Window, cx: &mut App) {
        self.0.update(cx, |editor, cx| {
            let editor_offset = editor.buffer().read(cx).len(cx);
            editor.change_selections(
                SelectionEffects::scroll(Autoscroll::Next),
                window,
                cx,
                |s| s.select_ranges(Some(editor_offset..editor_offset)),
            );
        });
    }

    fn subscribe(
        &self,
        mut callback: Box<dyn FnMut(ui_input::ErasedEditorEvent, &mut Window, &mut App) + 'static>,
        window: &mut Window,
        cx: &mut App,
    ) -> Subscription {
        window.subscribe(&self.0, cx, move |_, event: &EditorEvent, window, cx| {
            let event = match event {
                EditorEvent::BufferEdited => ui_input::ErasedEditorEvent::BufferEdited,
                EditorEvent::Blurred => ui_input::ErasedEditorEvent::Blurred,
                _ => return,
            };
            (callback)(event, window, cx);
        })
    }

    fn set_masked(&self, masked: bool, _window: &mut Window, cx: &mut App) {
        self.0.update(cx, |editor, cx| {
            editor.set_masked(masked, cx);
        });
    }
}

pub fn styled_runs_for_code_label<'a>(
    label: &'a CodeLabel,
    syntax_theme: &'a theme::SyntaxTheme,
    local_player: &'a theme::PlayerColor,
) -> impl 'a + Iterator<Item = (Range<usize>, HighlightStyle)> {
    let fade_out = HighlightStyle {
        fade_out: Some(0.35),
        ..Default::default()
    };

    if label.runs.is_empty() {
        let desc_start = label.filter_range.end;
        let fade_run =
            (desc_start < label.text.len()).then(|| (desc_start..label.text.len(), fade_out));
        return Either::Left(fade_run.into_iter());
    }

    let mut prev_end = label.filter_range.end;
    Either::Right(
        label
            .runs
            .iter()
            .enumerate()
            .flat_map(move |(ix, (range, highlight_id))| {
                let style = if *highlight_id == language::HighlightId::TABSTOP_INSERT_ID {
                    HighlightStyle {
                        color: Some(local_player.cursor),
                        ..Default::default()
                    }
                } else if *highlight_id == language::HighlightId::TABSTOP_REPLACE_ID {
                    HighlightStyle {
                        background_color: Some(local_player.selection),
                        ..Default::default()
                    }
                } else if let Some(style) = syntax_theme.get(*highlight_id).cloned() {
                    style
                } else {
                    return Default::default();
                };

                let mut runs = SmallVec::<[(Range<usize>, HighlightStyle); 3]>::new();
                let muted_style = style.highlight(fade_out);
                if range.start >= label.filter_range.end {
                    if range.start > prev_end {
                        runs.push((prev_end..range.start, fade_out));
                    }
                    runs.push((range.clone(), muted_style));
                } else if range.end <= label.filter_range.end {
                    runs.push((range.clone(), style));
                } else {
                    runs.push((range.start..label.filter_range.end, style));
                    runs.push((label.filter_range.end..range.end, muted_style));
                }
                prev_end = cmp::max(prev_end, range.end);

                if ix + 1 == label.runs.len() && label.text.len() > prev_end {
                    runs.push((prev_end..label.text.len(), fade_out));
                }

                runs
            }),
    )
}

pub trait RangeToAnchorExt: Sized {
    fn to_anchors(self, snapshot: &MultiBufferSnapshot) -> Range<Anchor>;

    fn to_display_points(self, snapshot: &EditorSnapshot) -> Range<DisplayPoint> {
        let anchor_range = self.to_anchors(&snapshot.buffer_snapshot());
        anchor_range.start.to_display_point(snapshot)..anchor_range.end.to_display_point(snapshot)
    }
}

impl<T: ToOffset> RangeToAnchorExt for Range<T> {
    fn to_anchors(self, snapshot: &MultiBufferSnapshot) -> Range<Anchor> {
        let start_offset = self.start.to_offset(snapshot);
        let end_offset = self.end.to_offset(snapshot);
        if start_offset == end_offset {
            snapshot.anchor_before(start_offset)..snapshot.anchor_before(end_offset)
        } else {
            snapshot.anchor_after(self.start)..snapshot.anchor_before(self.end)
        }
    }
}

pub trait RowExt {
    fn as_f64(&self) -> f64;

    fn next_row(&self) -> Self;

    fn previous_row(&self) -> Self;

    fn minus(&self, other: Self) -> u32;
}

impl RowExt for DisplayRow {
    fn as_f64(&self) -> f64 {
        self.0 as _
    }

    fn next_row(&self) -> Self {
        Self(self.0 + 1)
    }

    fn previous_row(&self) -> Self {
        Self(self.0.saturating_sub(1))
    }

    fn minus(&self, other: Self) -> u32 {
        self.0 - other.0
    }
}

impl RowExt for MultiBufferRow {
    fn as_f64(&self) -> f64 {
        self.0 as _
    }

    fn next_row(&self) -> Self {
        Self(self.0 + 1)
    }

    fn previous_row(&self) -> Self {
        Self(self.0.saturating_sub(1))
    }

    fn minus(&self, other: Self) -> u32 {
        self.0 - other.0
    }
}

trait RowRangeExt {
    type Row;

    fn len(&self) -> usize;

    fn iter_rows(&self) -> impl DoubleEndedIterator<Item = Self::Row>;
}

impl RowRangeExt for Range<MultiBufferRow> {
    type Row = MultiBufferRow;

    fn len(&self) -> usize {
        (self.end.0 - self.start.0) as usize
    }

    fn iter_rows(&self) -> impl DoubleEndedIterator<Item = MultiBufferRow> {
        (self.start.0..self.end.0).map(MultiBufferRow)
    }
}

impl RowRangeExt for Range<DisplayRow> {
    type Row = DisplayRow;

    fn len(&self) -> usize {
        (self.end.0 - self.start.0) as usize
    }

    fn iter_rows(&self) -> impl DoubleEndedIterator<Item = DisplayRow> {
        (self.start.0..self.end.0).map(DisplayRow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineHighlight {
    pub background: Background,
    pub border: Option<gpui::Hsla>,
    pub include_gutter: bool,
    pub type_id: Option<TypeId>,
}

struct LineManipulationResult {
    pub new_text: String,
    pub line_count_before: usize,
    pub line_count_after: usize,
}
