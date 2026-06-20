#[cfg(not(windows))]
use anyhow::Context as _;
use anyhow::{Result, bail};
use futures_lite::future::yield_now;
use log::trace;

use futures::{
    FutureExt,
    channel::mpsc::{UnboundedReceiver, unbounded},
};

use itertools::Itertools as _;

use async_channel::{Receiver, Sender};
use collections::{HashMap, VecDeque};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use settings::Settings;
use task::{HideStrategy, Shell, SpawnInTerminal};
use theme::{ActiveTheme, Theme};
use urlencoding;
use util::{paths::PathStyle, truncate_and_trailoff};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{
    borrow::Cow,
    cmp::{self, min},
    fmt::{self, Display, Formatter},
    ops::{BitOr, BitOrAssign, Deref, Range as StdRange},
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;
use vte::ansi::{Attr, Handler, Processor, StdSyncHandler};
pub use vte::ansi::{Color, NamedColor, Rgb};

use gpui::{
    App, AppContext as _, BackgroundExecutor, Bounds, ClipboardItem, Context, EventEmitter, Hsla,
    Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point as GpuiPoint, Rgba, ScrollWheelEvent, Size, Task, TouchPhase, Window, actions, black, px,
};

#[derive(Clone, Debug)]
pub struct Search;
pub struct SettingsCursorShape;
pub struct AlternateScroll;

pub fn is_default_background_color(color: Color) -> bool {
    matches!(color, Color::Named(NamedColor::Background))
}

pub fn is_app_chosen_exact_color(color: Color) -> bool {
    matches!(color, Color::Spec(_) | Color::Indexed(16..=255))
}

pub type AnsiSpans = Vec<(StdRange<usize>, Option<Color>)>;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ParsedAnsiText {
    pub text: String,
    pub foreground_spans: AnsiSpans,
    pub background_spans: AnsiSpans,
}

pub fn parse_ansi_text(input: &[u8]) -> ParsedAnsiText {
    let mut handler = StyledAnsiTextHandler::default();
    let mut processor = Processor::<StdSyncHandler>::default();
    processor.advance(&mut handler, input);
    handler.finish()
}

pub fn strip_ansi_text(input: &[u8]) -> String {
    let mut handler = PlainAnsiTextHandler::default();
    let mut processor = Processor::<StdSyncHandler>::default();
    processor.advance(&mut handler, input);
    handler.text
}

#[derive(Default)]
struct StyledAnsiTextHandler {
    text: String,
    foreground_spans: AnsiSpans,
    background_spans: AnsiSpans,
    current_foreground_range_start: usize,
    current_background_range_start: usize,
    current_foreground_color: Option<Color>,
    current_background_color: Option<Color>,
}

impl StyledAnsiTextHandler {
    fn finish(mut self) -> ParsedAnsiText {
        if self.current_foreground_range_start < self.text.len() {
            self.foreground_spans.push((
                self.current_foreground_range_start..self.text.len(),
                self.current_foreground_color,
            ));
        }

        if self.current_background_range_start < self.text.len() {
            self.background_spans.push((
                self.current_background_range_start..self.text.len(),
                self.current_background_color,
            ));
        }

        ParsedAnsiText {
            text: self.text,
            foreground_spans: self.foreground_spans,
            background_spans: self.background_spans,
        }
    }

    fn break_foreground_span(&mut self, color: Option<Color>) {
        self.foreground_spans.push((
            self.current_foreground_range_start..self.text.len(),
            self.current_foreground_color,
        ));
        self.current_foreground_color = color;
        self.current_foreground_range_start = self.text.len();
    }

    fn break_background_span(&mut self, color: Option<Color>) {
        self.background_spans.push((
            self.current_background_range_start..self.text.len(),
            self.current_background_color,
        ));
        self.current_background_color = color;
        self.current_background_range_start = self.text.len();
    }
}

impl Handler for StyledAnsiTextHandler {
    fn input(&mut self, c: char) {
        self.text.push(c);
    }

    fn linefeed(&mut self) {
        self.text.push('\n');
    }

    fn put_tab(&mut self, count: u16) {
        self.text.extend(std::iter::repeat_n('\t', count as usize));
    }

    fn terminal_attribute(&mut self, attr: Attr) {
        match attr {
            Attr::Foreground(color) => {
                self.break_foreground_span(Some(color));
            }
            Attr::Background(color) => {
                self.break_background_span(Some(color));
            }
            Attr::Reset => {
                self.break_foreground_span(None);
                self.break_background_span(None);
            }
            _ => {}
        }
    }
}

#[derive(Default)]
struct PlainAnsiTextHandler {
    text: String,
    line_start: usize,
}

impl Handler for PlainAnsiTextHandler {
    fn input(&mut self, c: char) {
        self.text.push(c);
    }

    fn linefeed(&mut self) {
        self.text.push('\n');
        self.line_start = self.text.len();
    }

    fn carriage_return(&mut self) {
        self.text.truncate(self.line_start);
    }

    fn put_tab(&mut self, count: u16) {
        self.text.extend(std::iter::repeat_n('\t', count as usize));
    }
}

#[derive(Default, Debug, Clone, Eq, PartialEq)]
pub struct Cell;

pub struct RenderableCells;

#[derive(Debug, Clone)]
pub struct IndexedCell {
    pub point: Point,
    pub cell: Cell,
}

impl Deref for IndexedCell {
    type Target = Cell;

    #[inline]
    fn deref(&self) -> &Cell {
        &self.cell
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Modes(u32);

impl Modes {
    pub const NONE: Self = Self(0);
    pub const APP_CURSOR: Self = Self(1 << 0);
    pub const APP_KEYPAD: Self = Self(1 << 1);
    pub const SHOW_CURSOR: Self = Self(1 << 2);
    pub const LINE_WRAP: Self = Self(1 << 3);
    pub const ORIGIN: Self = Self(1 << 4);
    pub const INSERT: Self = Self(1 << 5);
    pub const LINE_FEED_NEW_LINE: Self = Self(1 << 6);
    pub const FOCUS_IN_OUT: Self = Self(1 << 7);
    pub const ALTERNATE_SCROLL: Self = Self(1 << 8);
    pub const BRACKETED_PASTE: Self = Self(1 << 9);
    pub const SGR_MOUSE: Self = Self(1 << 10);
    pub const UTF8_MOUSE: Self = Self(1 << 11);
    pub const ALT_SCREEN: Self = Self(1 << 12);
    pub const MOUSE_REPORT_CLICK: Self = Self(1 << 13);
    pub const MOUSE_DRAG: Self = Self(1 << 14);
    pub const MOUSE_MOTION: Self = Self(1 << 15);
    pub const VI: Self = Self(1 << 16);
    pub const MOUSE_MODE: Self =
        Self(Self::MOUSE_REPORT_CLICK.0 | Self::MOUSE_DRAG.0 | Self::MOUSE_MOTION.0);

    pub const fn empty() -> Self {
        Self::NONE
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
}

impl BitOr for Modes {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Modes {
    fn bitor_assign(&mut self, rhs: Self) {
        self.insert(rhs);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    pub shape: CursorShape,
    pub point: Point,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
    HollowBlock,
    Hidden,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Point {
    pub line: i32,
    pub column: usize,
}

impl Point {
    pub fn new(line: i32, column: usize) -> Self {
        Self { line, column }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Range {
    start: Point,
    end: Point,
}

impl Range {
    pub fn new(start: Point, end: Point) -> Self {
        Self { start, end }
    }

    pub fn start(&self) -> Point {
        self.start
    }

    pub fn end(&self) -> Point {
        self.end
    }

    pub fn contains(&self, point: Point) -> bool {
        self.start <= point && point <= self.end
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectionRange {
    pub start: Point,
    pub end: Point,
    pub is_block: bool,
}

impl SelectionRange {
    pub fn point_range(self) -> Range {
        Range::new(self.start, self.end)
    }
}

// TODO: Un-pub
#[derive(Clone)]
pub struct Content {
    pub cells: Vec<IndexedCell>,
    pub mode: Modes,
    pub display_offset: usize,
    pub selection_text: Option<String>,
    pub selection: Option<SelectionRange>,
    pub cursor: Cursor,
    pub cursor_char: char,
    pub terminal_bounds: TerminalBounds,
    pub last_hovered_word: Option<HoveredWord>,
    pub scrolled_to_top: bool,
    pub scrolled_to_bottom: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HoveredWord {
    pub word: String,
    pub word_match: Range,
    pub id: usize,
}

impl Default for Content {
    fn default() -> Self {
        Content {
            cells: Default::default(),
            mode: Default::default(),
            display_offset: Default::default(),
            selection_text: Default::default(),
            selection: Default::default(),
            cursor: Cursor {
                shape: CursorShape::Block,
                point: Point::new(0, 0),
            },
            cursor_char: Default::default(),
            terminal_bounds: Default::default(),
            last_hovered_word: None,
            scrolled_to_top: false,
            scrolled_to_bottom: false,
        }
    }
}

/// Inserts Zed-specific environment variables for terminal sessions.
/// Used by both local terminals and remote terminals (via SSH).
pub fn insert_zed_terminal_env(
    env: &mut HashMap<String, String>,
    version: &impl std::fmt::Display,
) {
    env.insert("ZED_TERM".to_string(), "true".to_string());
    env.insert("TERM_PROGRAM".to_string(), "zed".to_string());
    env.insert("TERM".to_string(), "xterm-256color".to_string());
    env.insert("COLORTERM".to_string(), "truecolor".to_string());
    env.insert("TERM_PROGRAM_VERSION".to_string(), version.to_string());
}

///Upward flowing events, for changing the title and such
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    TitleChanged,
    BreadcrumbsChanged,
    CloseTerminal,
    Bell,
    Wakeup,
    BlinkChanged(bool),
    SelectionsChanged,
    NewNavigationTarget(Option<MaybeNavigationTarget>),
    Open(MaybeNavigationTarget),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathLikeTarget {
    /// File system path, absolute or relative, existing or not.
    /// Might have line and column number(s) attached as `file.rs:1:23`
    pub maybe_path: String,
    /// Current working directory of the terminal
    pub terminal_dir: Option<PathBuf>,
}

/// A string inside terminal, potentially useful as a URI that can be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaybeNavigationTarget {
    /// HTTP, git, etc. string determined by the `URL_REGEX` regex.
    Url(String),
    /// File system path, absolute or relative, existing or not.
    /// Might have line and column number(s) attached as `file.rs:1:23`
    PathLike(PathLikeTarget),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalBounds {
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub bounds: Bounds<Pixels>,
}

impl TerminalBounds {
    pub fn new(line_height: Pixels, cell_width: Pixels, bounds: Bounds<Pixels>) -> Self {
        todo!("TerminalBounds::new");
    }

    pub fn num_lines(&self) -> usize {
        todo!("TerminalBounds::num_lines");
    }

    pub fn num_columns(&self) -> usize {
        todo!("TerminalBounds::num_columns");
    }

    pub fn height(&self) -> Pixels {
        todo!("TerminalBounds::height");
    }

    pub fn width(&self) -> Pixels {
        todo!("TerminalBounds::width");
    }

    pub fn cell_width(&self) -> Pixels {
        todo!("TerminalBounds::cell_width");
    }

    pub fn line_height(&self) -> Pixels {
        todo!("TerminalBounds::line_height");
    }
}

impl Default for TerminalBounds {
    fn default() -> Self {
        todo!("TerminalBounds::default");
    }
}

#[derive(Error, Debug)]
pub struct TerminalError {
    pub directory: Option<PathBuf>,
    pub program: Option<String>,
    pub args: Option<Vec<String>>,
    pub title_override: Option<String>,
    pub source: std::io::Error,
}

impl Display for TerminalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        todo!("TerminalError::fmt");
    }
}

pub const MAX_SCROLL_HISTORY_LINES: usize = 100_000;

pub struct TerminalBuilder;

impl TerminalBuilder {
    pub fn new_display_only(
        cursor_shape: SettingsCursorShape,
        alternate_scroll: AlternateScroll,
        max_scroll_history_lines: Option<usize>,
        window_id: u64,
        background_executor: &BackgroundExecutor,
        path_style: PathStyle,
    ) -> TerminalBuilder {
        todo!("TerminalBuilder::new_display_only");
    }

    pub fn new_display_only_with_bounds(
        cursor_shape: SettingsCursorShape,
        alternate_scroll: AlternateScroll,
        max_scroll_history_lines: Option<usize>,
        window_id: u64,
        background_executor: &BackgroundExecutor,
        path_style: PathStyle,
        terminal_bounds: TerminalBounds,
    ) -> TerminalBuilder {
        todo!("TerminalBuilder::new_display_only_with_bounds");
    }

    pub fn new(
        working_directory: Option<PathBuf>,
        task: Option<TaskState>,
        shell: Shell,
        mut env: HashMap<String, String>,
        cursor_shape: SettingsCursorShape,
        alternate_scroll: AlternateScroll,
        max_scroll_history_lines: Option<usize>,
        path_hyperlink_regexes: Vec<String>,
        path_hyperlink_timeout_ms: u64,
        is_remote_terminal: bool,
        window_id: u64,
        completion_tx: Option<Sender<Option<ExitStatus>>>,
        cx: &App,
        activation_script: Vec<String>,
        path_style: PathStyle,
    ) -> Task<Result<TerminalBuilder>> {
        todo!("TerminalBuilder::new");
    }

    pub fn subscribe(mut self, cx: &Context<Terminal>) -> Terminal {
        todo!("TerminalBuilder::subscribe");
    }
}

pub struct Terminal {
    pub matches: Vec<Range>,
    pub last_content: Content,
    pub selection_head: Option<Point>,
    pub breadcrumb_text: String,
}

#[derive(Debug)]
pub struct TaskState {
    pub status: TaskStatus,
    pub completion_rx: Receiver<Option<ExitStatus>>,
    pub spawned_task: SpawnInTerminal,
}

/// A status of the current terminal tab's task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    /// The task had been started, but got cancelled or somehow otherwise it did not
    /// report its exit code before the terminal event loop was shut down.
    Unknown,
    /// The task is started and running currently.
    Running,
    /// After the start, the task stopped running and reported its error code back.
    Completed { success: bool },
}

impl Terminal {
    pub fn selection_started(&self) -> bool {
        todo!("Terminal::selection_started");
    }

    pub fn last_content(&self) -> &Content {
        todo!("Terminal::last_content");
    }

    pub fn set_cursor_shape(&mut self, cursor_shape: SettingsCursorShape) {
        todo!("Terminal::set_cursor_shape");
    }

    pub fn write_output(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        todo!("Terminal::write_output");
    }

    pub fn total_lines(&self) -> usize {
        todo!("Terminal::total_lines");
    }

    pub fn viewport_lines(&self) -> usize {
        todo!("Terminal::viewport_lines");
    }

    //To test:
    //- Activate match on terminal (scrolling and selection)
    //- Editor search snapping behavior

    pub fn activate_match(&mut self, index: usize) {
        todo!("Terminal::activate_match");
    }

    pub fn select_matches(&mut self, matches: &[Range]) {
        todo!("Terminal::select_matches");
    }

    pub fn select_all(&mut self) {
        todo!("Terminal::select_all");
    }

    pub fn copy(&mut self, keep_selection: Option<bool>) {
        todo!("Terminal::copy");
    }

    pub fn clear(&mut self) {
        todo!("Terminal::clear");
    }

    pub fn scroll_line_up(&mut self) {
        todo!("Terminal::scroll_line_up");
    }

    pub fn scroll_up_by(&mut self, lines: usize) {
        todo!("Terminal::scroll_up_by");
    }

    pub fn scroll_line_down(&mut self) {
        todo!("Terminal::scroll_line_down");
    }

    pub fn scroll_down_by(&mut self, lines: usize) {
        todo!("Terminal::scroll_down_by");
    }

    pub fn scroll_page_up(&mut self) {
        todo!("Terminal::scroll_page_up");
    }

    pub fn scroll_page_down(&mut self) {
        todo!("Terminal::scroll_page_down");
    }

    pub fn scroll_to_top(&mut self) {
        todo!("scroll_to_top");
    }

    pub fn scroll_to_bottom(&mut self) {
        todo!("Terminal::scroll_to_bottom");
    }

    pub fn scrolled_to_top(&self) -> bool {
        todo!("Terminal::scrolled_to_top");
    }

    pub fn scrolled_to_bottom(&self) -> bool {
        todo!("Terminal::scrolled_to_bottom");
    }

    ///Resize the terminal and the PTY.
    pub fn set_size(&mut self, new_bounds: TerminalBounds) {
        todo!("Terminal::set_size");
    }

    pub fn input(&mut self, input: impl Into<Cow<'static, [u8]>>) {
        todo!("Terminal::input");
    }

    pub fn toggle_vi_mode(&mut self) {
        todo!("Terminal::toggle_vi_mode");
    }

    pub fn vi_motion(&mut self, keystroke: &Keystroke) {
        todo!("Terminal::");
    }

    pub fn try_keystroke(&mut self, keystroke: &Keystroke, option_as_meta: bool) -> bool {
        todo!("Terminal::");
    }

    pub fn try_modifiers_change(
        &mut self,
        modifiers: &Modifiers,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        todo!("Terminal::");
    }

    ///Paste text into the terminal
    pub fn paste(&mut self, text: &str) {
        todo!("Terminal::");
    }

    pub fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        todo!("Terminal::");
    }

    pub fn get_content(&self) -> String {
        todo!("Terminal::");
    }

    pub fn last_n_non_empty_lines(&self, n: usize) -> Vec<String> {
        todo!("Terminal::");
    }

    pub fn focus_in(&self) {
        todo!("Terminal::");
    }

    pub fn focus_out(&mut self) {
        todo!("Terminal::");
    }

    pub fn mouse_mode(&self, shift: bool) -> bool {
        todo!("Terminal::");
    }

    pub fn mouse_move(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        todo!("Terminal::");
    }

    pub fn select_word_at_event_position(&mut self, e: &MouseDownEvent) {
        todo!("Terminal::");
    }

    pub fn mouse_drag(
        &mut self,
        e: &MouseMoveEvent,
        region: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        todo!("Terminal::");
    }

    pub fn mouse_down(&mut self, e: &MouseDownEvent, _cx: &mut Context<Self>) {
        todo!("Terminal::");
    }

    pub fn mouse_up(&mut self, e: &MouseUpEvent, cx: &Context<Self>) {
        todo!("Terminal::");
    }

    ///Scroll the terminal
    pub fn scroll_wheel(&mut self, e: &ScrollWheelEvent, scroll_multiplier: f32) {
        todo!("Terminal::");
    }

    pub fn find_matches(&self, searcher: Search, cx: &Context<Self>) -> Task<Vec<Range>> {
        todo!("Terminal::");
    }

    pub fn working_directory(&self) -> Option<PathBuf> {
        todo!("Terminal::");
    }

    /// Normalizes the command name of the foreground process, if one is known.
    pub fn foreground_process_command_name(&self) -> Option<String> {
        todo!("Terminal::");
    }

    pub fn title(&self, truncate: bool) -> String {
        todo!("Terminal::");
    }

    pub fn kill_active_task(&mut self) {
        todo!("Terminal::");
    }

    pub fn pid(&self) -> Option<sysinfo::Pid> {
        todo!("Terminal::");
    }

    pub fn task(&self) -> Option<&TaskState> {
        todo!("Terminal::");
    }

    pub fn wait_for_completed_task(&self, cx: &App) -> Task<Option<ExitStatus>> {
        todo!("Terminal::");
    }

    pub fn vi_mode_enabled(&self) -> bool {
        todo!("Terminal::");
    }

    pub fn clone_builder(&self, cx: &App, cwd: Option<PathBuf>) -> Task<Result<TerminalBuilder>> {
        todo!("Terminal::");
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        todo!("Terminal::drop");
    }
}

impl EventEmitter<Event> for Terminal {}

/// Converts an 8 bit ANSI color to its GPUI equivalent.
/// Accepts `usize` for compatibility with the `alacritty::Colors` interface,
/// Other than that use case, should only be called with values in the `[0,255]` range
pub fn get_color_at_index(index: usize, theme: &Theme) -> Hsla {
    todo!("get_color_at_index");
}

pub fn rgba_color(r: u8, g: u8, b: u8) -> Hsla {
    todo!("rgba_color");
}
