use gpui::{
    App, EventEmitter, FocusHandle, Focusable, Pixels, PromptButton, PromptHandle, PromptLevel,
    PromptResponse, RenderablePromptHandle, Window, div, prelude::*,
};
use settings::{Settings, SettingsStore};
use theme_settings::ThemeSettings;
use ui::{FluentBuilder, TintColor, prelude::*};
use workspace::WorkspaceSettings;

pub fn init(cx: &mut App) {
    process_settings(cx);

    cx.observe_global::<SettingsStore>(process_settings)
        .detach();
}

fn process_settings(cx: &mut App) {
    let settings = WorkspaceSettings::get_global(cx);
    if settings.use_system_prompts && cfg!(not(any(target_os = "linux", target_os = "freebsd"))) {
        cx.reset_prompt_builder();
    } else {
        cx.set_prompt_builder(zed_prompt_renderer);
    }
}

fn clean_message(message: impl AsRef<str>) -> String {
    message
        .as_ref()
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Use this function in conjunction with [App::set_prompt_builder] to force
/// GPUI to use the internal prompt system.
fn zed_prompt_renderer(
    level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    actions: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let renderer = cx.new({
        |cx| ZedPromptRenderer {
            _level: level,
            message: clean_message(message),
            actions: actions.iter().map(|a| a.label().to_string()).collect(),
            focus: cx.focus_handle(),
            active_action_id: 0,
            detail: detail.map(|detail| clean_message(detail)),
        }
    });

    handle.with_view(renderer, window, cx)
}

pub struct ZedPromptRenderer {
    _level: PromptLevel,
    message: String,
    actions: Vec<String>,
    focus: FocusHandle,
    active_action_id: usize,
    detail: Option<String>,
}

impl ZedPromptRenderer {
    fn confirm(&mut self, _: &menu::Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(PromptResponse(self.active_action_id));
    }

    fn cancel(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.actions.iter().position(|a| a == "Cancel") {
            cx.emit(PromptResponse(ix));
        }
    }

    fn select_first(
        &mut self,
        _: &menu::SelectFirst,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_action_id = self.actions.len().saturating_sub(1);
        cx.notify();
    }

    fn select_last(&mut self, _: &menu::SelectLast, _window: &mut Window, cx: &mut Context<Self>) {
        self.active_action_id = 0;
        cx.notify();
    }

    fn select_next(&mut self, _: &menu::SelectNext, _window: &mut Window, cx: &mut Context<Self>) {
        self.active_action_id = (self.active_action_id + 1) % self.actions.len();
        cx.notify();
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_action_id > 0 {
            self.active_action_id -= 1;
        } else {
            self.active_action_id = self.actions.len().saturating_sub(1);
        }
        cx.notify();
    }
}

impl Render for ZedPromptRenderer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = ThemeSettings::get_global(cx);
        let font_size: Pixels = settings.ui_font_size(cx).into();

        let dialog = v_flex()
            .key_context("Prompt")
            .cursor_default()
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .w_80()
            .p_4()
            .gap_4()
            .elevation_3(cx)
            .overflow_hidden()
            .font_family(settings.ui_font.family.clone())
            .child(
                div().w_full()
                    .text_size(font_size)
                    .text_color(Color::Default.color(cx))
                    .child(self.message.clone())
            )
            .children(self.detail.clone().map(|detail| {
                div().w_full()
                    .text_xs()
                    .text_color(Color::Muted.color(cx))
                    .child(detail)
            }))
            .child(
                v_flex()
                    .gap_1()
                    .children(self.actions.iter().enumerate().map(|(ix, action)| {
                        Button::new(ix, action.clone())
                            .full_width()
                            .style(ButtonStyle::Outlined)
                            .when(ix == self.active_action_id, |s| {
                                s.style(ButtonStyle::Tinted(TintColor::Accent))
                            })
                            .tab_index(ix as isize)
                            .on_click(cx.listener(move |_, _, _window, cx| {
                                cx.emit(PromptResponse(ix));
                            }))
                    })),
            );

        div()
            .size_full()
            .occlude()
            .bg(gpui::black().opacity(0.2))
            .child(
                v_flex()
                    .size_full()
                    .absolute()
                    .top_0()
                    .left_0()
                    .items_center()
                    .justify_center()
                    .child(dialog),
            )
    }
}

impl EventEmitter<PromptResponse> for ZedPromptRenderer {}

impl Focusable for ZedPromptRenderer {
    fn focus_handle(&self, _: &crate::App) -> FocusHandle {
        self.focus.clone()
    }
}
