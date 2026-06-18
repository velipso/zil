//! The logic, responsible for managing [`Inlay`]s in the editor.
//!
//! Inlays are "not real" text that gets mixed into the "real" buffer's text.
//! They are attached to a certain [`Anchor`], and display certain contents (usually, strings)
//! between real text around that anchor.
//!
//! Inlay examples in Zed:
//! * inlay hints, received from LSP
//! * inline values, shown in the debugger
//! * inline predictions, showing the Zeta/Copilot/etc. predictions
//! * document color values, if configured to be displayed as inlays
//! * ... anything else, potentially.
//!
//! Editor uses [`crate::DisplayMap`] and [`crate::display_map::InlayMap`] to manage what's rendered inside the editor, using
//! [`InlaySplice`] to update this state.
use std::sync::OnceLock;

use gpui::{Hsla, Rgba};
use multi_buffer::Anchor;
use project::{InlayHint, InlayId};
use text::Rope;

/// A splice to send into the `inlay_map` for updating the visible inlays on the screen.
/// "Visible" inlays may not be displayed in the buffer right away, but those are ready to be displayed on further buffer scroll, pane item activations, etc. right away without additional LSP queries or settings changes.
/// The data in the cache is never used directly for displaying inlays on the screen, to avoid races with updates from LSP queries and sync overhead.
/// Splice is picked to help avoid extra hint flickering and "jumps" on the screen.
#[derive(Debug, Default)]
pub struct InlaySplice {
    pub to_remove: Vec<InlayId>,
    pub to_insert: Vec<Inlay>,
}

#[derive(Debug, Clone)]
pub struct Inlay {
    pub id: InlayId,
    // TODO this could be an ExcerptAnchor
    pub position: Anchor,
    pub content: InlayContent,
}

#[derive(Debug, Clone)]
pub enum InlayContent {
    Text(text::Rope),
    Color(Hsla),
}

impl Inlay {
    pub fn hint(id: InlayId, position: Anchor, hint: &InlayHint) -> Self {
        let mut text = hint.text();
        let needs_right_padding = hint.padding_right && !text.ends_with(" ");
        let needs_left_padding = hint.padding_left && !text.starts_with(" ");
        if needs_right_padding {
            text.push(" ");
        }
        if needs_left_padding {
            text.push_front(" ");
        }
        Self {
            id,
            position,
            content: InlayContent::Text(text),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn mock_hint(id: usize, position: Anchor, text: impl Into<Rope>) -> Self {
        Self {
            id: InlayId::Hint(id),
            position,
            content: InlayContent::Text(text.into()),
        }
    }

    pub fn color(id: usize, position: Anchor, color: Rgba) -> Self {
        Self {
            id: InlayId::Color(id),
            position,
            content: InlayContent::Color(color.into()),
        }
    }

    pub fn debugger<T: Into<Rope>>(id: usize, position: Anchor, text: T) -> Self {
        Self {
            id: InlayId::DebuggerValue(id),
            position,
            content: InlayContent::Text(text.into()),
        }
    }

    pub fn repl_result<T: Into<Rope>>(id: usize, position: Anchor, text: T) -> Self {
        Self {
            id: InlayId::ReplResult(id),
            position,
            content: InlayContent::Text(text.into()),
        }
    }

    pub fn text(&self) -> &Rope {
        static COLOR_TEXT: OnceLock<Rope> = OnceLock::new();
        match &self.content {
            InlayContent::Text(text) => text,
            InlayContent::Color(_) => COLOR_TEXT.get_or_init(|| Rope::from("◼")),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn get_color(&self) -> Option<Hsla> {
        match self.content {
            InlayContent::Color(color) => Some(color),
            _ => None,
        }
    }
}
