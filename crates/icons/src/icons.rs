use std::sync::Arc;

use serde::{Deserialize, Serialize};
use strum::{EnumIter, EnumString, IntoStaticStr};

#[derive(
    Debug, PartialEq, Eq, Copy, Clone, EnumIter, EnumString, IntoStaticStr, Serialize, Deserialize,
)]
#[strum(serialize_all = "snake_case")]
pub enum IconName {
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUpRight,
    ArrowUp,
    Backspace,
    CaseSensitive,
    Chat,
    Check,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    ChevronUpDown,
    Circle,
    Close,
    Command,
    Control,
    Copy,
    Dash,
    Debug,
    Eraser,
    Escape,
    File,
    FileLock,
    FileTextOutlined,
    Folder,
    GenericClose,
    GenericMaximize,
    GenericMinimize,
    GenericRestore,
    GitBranch,
    Info,
    Keyboard,
    Link,
    ListTodo,
    LoadCircle,
    MagnifyingGlass,
    Maximize,
    Menu,
    Minimize,
    Option,
    PageDown,
    PageUp,
    Pencil,
    PlayFilled,
    Plus,
    Quote,
    Regex,
    Replace,
    ReplaceAll,
    ReplaceNext,
    ReplyArrowRight,
    Return,
    RotateCcw,
    RotateCw,
    Screen,
    SelectAll,
    Settings,
    Shift,
    Sliders,
    Space,
    Sparkle,
    Split,
    Star,
    Stop,
    Tab,
    TextWrap,
    TextUnwrap,
    Trash,
    Undo,
    Warning,
    WholeWord,
    XCircle,
    ZedAssistant,
}

impl IconName {
    /// Returns the path to this icon.
    pub fn path(&self) -> Arc<str> {
        let file_stem: &'static str = self.into();
        format!("icons/{file_stem}.svg").into()
    }
}
