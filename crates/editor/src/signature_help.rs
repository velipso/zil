use crate::actions::ShowSignatureHelp;
use crate::hover_popover::open_markdown_url;
use crate::{Editor, EditorSettings, ToggleAutoSignatureHelp, hover_markdown_style};
use gpui::{
    App, Context, Entity, HighlightStyle, MouseButton, ScrollHandle, Size, StyledText, Task,
    TextStyle, Window, combine_highlights,
};

use markdown::{CopyButtonVisibility, Markdown, MarkdownElement};
use settings::Settings;
use std::ops::Range;
use std::time::Duration;
use text::Rope;
use theme_settings::ThemeSettings;
use ui::{
    ActiveTheme, AnyElement, ButtonCommon, ButtonStyle, Clickable, FluentBuilder, IconButton,
    IconButtonShape, IconName, IconSize, InteractiveElement, IntoElement, Label, LabelCommon,
    LabelSize, ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled, StyledExt,
    WithScrollbar, div, relative,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignatureHelpHiddenBy {
    AutoClose,
    Escape,
}

impl Editor {
    pub fn toggle_auto_signature_help_menu(
        &mut self,
        _: &ToggleAutoSignatureHelp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.auto_signature_help = self
            .auto_signature_help
            .map(|auto_signature_help| !auto_signature_help)
            .or_else(|| Some(!EditorSettings::get_global(cx).auto_signature_help));
        match self.auto_signature_help {
            Some(true) => {
                self.show_signature_help(&ShowSignatureHelp, window, cx);
            }
            Some(false) => {
                self.hide_signature_help(cx, SignatureHelpHiddenBy::AutoClose);
            }
            None => {}
        }
    }

    pub(super) fn hide_signature_help(
        &mut self,
        cx: &mut Context<Self>,
        signature_help_hidden_by: SignatureHelpHiddenBy,
    ) -> bool {
        if self.signature_help_state.is_shown() {
            self.signature_help_state.task = None;
            self.signature_help_state.hide(signature_help_hidden_by);
            cx.notify();
            true
        } else {
            false
        }
    }

    pub fn auto_signature_help_enabled(&self, cx: &App) -> bool {
        if let Some(auto_signature_help) = self.auto_signature_help {
            auto_signature_help
        } else {
            EditorSettings::get_global(cx).auto_signature_help
        }
    }

    pub fn show_signature_help(
        &mut self,
        _: &ShowSignatureHelp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_signature_help_impl(false, window, cx);
    }

    fn show_signature_help_impl(
        &mut self,
        use_delay: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending_rename.is_some() {
            return;
        }

        // If there's an already running signature
        // help task, this will drop it.
        self.signature_help_state.task = None;

        let position = self.selections.newest_anchor().head();
        let Some((buffer, buffer_position)) =
            self.buffer.read(cx).text_anchor_for_position(position, cx)
        else {
            return;
        };
        let Some(lsp_store) = self.project().map(|p| p.read(cx).lsp_store()) else {
            return;
        };
        let lsp_task = lsp_store.update(cx, |lsp_store, cx| {
            lsp_store.signature_help(&buffer, buffer_position, cx)
        });
        let language = self.language_at(position, cx);

        let signature_help_delay_ms = if use_delay {
            EditorSettings::get_global(cx).hover_popover_delay.0
        } else {
            0
        };

        self.signature_help_state
            .set_task(cx.spawn_in(window, async move |editor, cx| {
                if signature_help_delay_ms > 0 {
                    cx.background_executor()
                        .timer(Duration::from_millis(signature_help_delay_ms))
                        .await;
                }

                let signature_help = lsp_task.await;

                editor
                    .update(cx, |editor, cx| {
                        let Some(mut signature_help) =
                            signature_help.unwrap_or_default().into_iter().next()
                        else {
                            editor
                                .signature_help_state
                                .hide(SignatureHelpHiddenBy::AutoClose);
                            return;
                        };

                        if let Some(language) = language {
                            for signature in &mut signature_help.signatures {
                                let text = Rope::from(signature.label.as_ref());
                                let highlights = language
                                    .highlight_text(&text, 0..signature.label.len())
                                    .into_iter()
                                    .flat_map(|(range, highlight_id)| {
                                        Some((range, *cx.theme().syntax().get(highlight_id)?))
                                    });
                                signature.highlights =
                                    combine_highlights(signature.highlights.clone(), highlights)
                                        .collect();
                            }
                        }
                        let settings = ThemeSettings::get_global(cx);
                        let style = TextStyle {
                            color: cx.theme().colors().text,
                            font_family: settings.buffer_font.family.clone(),
                            font_fallbacks: settings.buffer_font.fallbacks.clone(),
                            font_features: settings.buffer_font.features.clone(),
                            font_size: settings.buffer_font_size(cx).into(),
                            font_weight: settings.buffer_font.weight,
                            line_height: relative(settings.buffer_line_height.value()),
                            ..TextStyle::default()
                        };
                        let scroll_handle = ScrollHandle::new();
                        let signatures = signature_help
                            .signatures
                            .into_iter()
                            .map(|s| SignatureHelp {
                                label: s.label,
                                documentation: s.documentation,
                                highlights: s.highlights,
                                active_parameter: s.active_parameter,
                                parameter_documentation: s
                                    .active_parameter
                                    .and_then(|idx| s.parameters.get(idx))
                                    .and_then(|param| param.documentation.clone()),
                            })
                            .collect::<Vec<_>>();

                        if signatures.is_empty() {
                            editor
                                .signature_help_state
                                .hide(SignatureHelpHiddenBy::AutoClose);
                            return;
                        }

                        let current_signature = signature_help
                            .active_signature
                            .min(signatures.len().saturating_sub(1));

                        let signature_help_popover = SignatureHelpPopover {
                            style,
                            signatures,
                            current_signature,
                            scroll_handle,
                        };
                        editor
                            .signature_help_state
                            .set_popover(signature_help_popover);
                        cx.notify();
                    })
                    .ok();
            }));
    }
}

#[derive(Default, Debug)]
pub struct SignatureHelpState {
    task: Option<Task<()>>,
    popover: Option<SignatureHelpPopover>,
    hidden_by: Option<SignatureHelpHiddenBy>,
}

impl SignatureHelpState {
    fn set_task(&mut self, task: Task<()>) {
        self.task = Some(task);
        self.hidden_by = None;
    }

    #[cfg(test)]
    pub fn popover(&self) -> Option<&SignatureHelpPopover> {
        self.popover.as_ref()
    }

    pub fn popover_mut(&mut self) -> Option<&mut SignatureHelpPopover> {
        self.popover.as_mut()
    }

    fn set_popover(&mut self, popover: SignatureHelpPopover) {
        self.popover = Some(popover);
        self.hidden_by = None;
    }

    fn hide(&mut self, hidden_by: SignatureHelpHiddenBy) {
        if self.hidden_by.is_none() {
            self.popover = None;
            self.hidden_by = Some(hidden_by);
        }
    }

    pub fn is_shown(&self) -> bool {
        self.popover.is_some()
    }

    pub fn has_multiple_signatures(&self) -> bool {
        self.popover
            .as_ref()
            .is_some_and(|popover| popover.signatures.len() > 1)
    }
}

#[cfg(test)]
impl SignatureHelpState {
    pub fn task(&self) -> Option<&Task<()>> {
        self.task.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SignatureHelp {
    pub(crate) label: SharedString,
    documentation: Option<Entity<Markdown>>,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    active_parameter: Option<usize>,
    parameter_documentation: Option<Entity<Markdown>>,
}

#[derive(Clone, Debug)]
pub struct SignatureHelpPopover {
    pub style: TextStyle,
    pub signatures: Vec<SignatureHelp>,
    pub current_signature: usize,
    scroll_handle: ScrollHandle,
}

impl SignatureHelpPopover {
    pub fn render(
        &mut self,
        max_size: Size<Pixels>,
        window: &mut Window,
        cx: &mut Context<Editor>,
    ) -> AnyElement {
        let Some(signature) = self.signatures.get(self.current_signature) else {
            return div().into_any_element();
        };

        let main_content = div()
            .occlude()
            .p_2()
            .child(
                div()
                    .id("signature_help_container")
                    .overflow_y_scroll()
                    .max_w(max_size.width)
                    .max_h(max_size.height)
                    .track_scroll(&self.scroll_handle)
                    .child(
                        StyledText::new(signature.label.clone()).with_default_highlights(
                            &self.style,
                            signature.highlights.iter().cloned(),
                        ),
                    )
                    .when_some(
                        signature.parameter_documentation.clone(),
                        |this, param_doc| {
                            this.child(div().h_px().bg(cx.theme().colors().border_variant).my_1())
                                .child(
                                    MarkdownElement::new(
                                        param_doc,
                                        hover_markdown_style(window, cx),
                                    )
                                    .code_block_renderer(markdown::CodeBlockRenderer::Default {
                                        copy_button_visibility: CopyButtonVisibility::Hidden,
                                        wrap_button_visibility:
                                            markdown::WrapButtonVisibility::Hidden,
                                        border: false,
                                    })
                                    .on_url_click(open_markdown_url),
                                )
                        },
                    )
                    .when_some(signature.documentation.clone(), |this, description| {
                        this.child(div().h_px().bg(cx.theme().colors().border_variant).my_1())
                            .child(
                                MarkdownElement::new(description, hover_markdown_style(window, cx))
                                    .code_block_renderer(markdown::CodeBlockRenderer::Default {
                                        copy_button_visibility: CopyButtonVisibility::Hidden,
                                        wrap_button_visibility:
                                            markdown::WrapButtonVisibility::Hidden,
                                        border: false,
                                    })
                                    .on_url_click(open_markdown_url),
                            )
                    }),
            )
            .vertical_scrollbar_for(&self.scroll_handle, window, cx);

        let controls = if self.signatures.len() > 1 {
            let prev_button = IconButton::new("signature_help_prev", IconName::ChevronUp)
                .shape(IconButtonShape::Square)
                .style(ButtonStyle::Subtle)
                .icon_size(IconSize::Small)
                .tooltip(move |_window, cx| {
                    ui::Tooltip::for_action("Previous Signature", &crate::SignatureHelpPrevious, cx)
                })
                .on_click(cx.listener(|editor, _, window, cx| {
                    editor.signature_help_prev(&crate::SignatureHelpPrevious, window, cx);
                }));

            let next_button = IconButton::new("signature_help_next", IconName::ChevronDown)
                .shape(IconButtonShape::Square)
                .style(ButtonStyle::Subtle)
                .icon_size(IconSize::Small)
                .tooltip(move |_window, cx| {
                    ui::Tooltip::for_action("Next Signature", &crate::SignatureHelpNext, cx)
                })
                .on_click(cx.listener(|editor, _, window, cx| {
                    editor.signature_help_next(&crate::SignatureHelpNext, window, cx);
                }));

            let page = Label::new(format!(
                "{}/{}",
                self.current_signature + 1,
                self.signatures.len()
            ))
            .size(LabelSize::Small);

            Some(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_0p5()
                    .px_0p5()
                    .py_0p5()
                    .children([
                        prev_button.into_any_element(),
                        div().child(page).into_any_element(),
                        next_button.into_any_element(),
                    ])
                    .into_any_element(),
            )
        } else {
            None
        };
        div()
            .elevation_2(cx)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .flex()
            .flex_row()
            .when_some(controls, |this, controls| {
                this.children(vec![
                    div().flex().items_end().child(controls),
                    div().w_px().bg(cx.theme().colors().border_variant),
                ])
            })
            .child(main_content)
            .into_any_element()
    }
}
