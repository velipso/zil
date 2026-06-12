use std::sync::Arc;

use crate::{ZedPredictUpsell};
use ai_onboarding::EditPredictionOnboarding;
use client::{Client, UserStore};
use db::kvp::Dismissable;
use gpui::{
    ClickEvent, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, MouseDownEvent, Render,
    linear_color_stop, linear_gradient,
};
use ui::prelude::*;
use workspace::{ModalView, Workspace};

#[macro_export]
macro_rules! onboarding_event {
    ($name:expr) => {
        telemetry::event!($name, source = "Edit Prediction Onboarding");
    };
    ($name:expr, $($key:ident $(= $value:expr)?),+ $(,)?) => {
        telemetry::event!($name, source = "Edit Prediction Onboarding", $($key $(= $value)?),+);
    };
}

/// Introduces user to Zed's Edit Prediction feature
pub struct ZedPredictModal {
    onboarding: Entity<EditPredictionOnboarding>,
    focus_handle: FocusHandle,
}

impl ZedPredictModal {
    pub fn toggle(
        _workspace: &mut Workspace,
        _user_store: Entity<UserStore>,
        _client: Arc<Client>,
        _window: &mut Window,
        _cx: &mut Context<Workspace>,
    ) {
        // VELIPSO: remove
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        ZedPredictUpsell::set_dismissed(true, cx);
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for ZedPredictModal {}

impl Focusable for ZedPredictModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for ZedPredictModal {
    fn on_before_dismiss(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> workspace::DismissDecision {
        ZedPredictUpsell::set_dismissed(true, cx);
        workspace::DismissDecision::Dismiss(true)
    }
}

impl Render for ZedPredictModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let window_height = window.viewport_size().height;
        let max_height = window_height - px(200.);
        let color = cx.theme().colors();

        v_flex()
            .id("edit-prediction-onboarding")
            .key_context("ZedPredictModal")
            .relative()
            .w(px(550.))
            .h_full()
            .max_h(max_height)
            .p_1()
            .gap_2()
            .elevation_3(cx)
            .track_focus(&self.focus_handle(cx))
            .overflow_hidden()
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(|_, _: &menu::Cancel, _window, cx| {
                onboarding_event!("Cancelled", trigger = "Action");
                cx.emit(DismissEvent);
            }))
            .on_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                this.focus_handle.focus(window, cx);
            }))
            .child(
                div()
                    .p_3()
                    .size_full()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .rounded(px(5.))
                    .bg(linear_gradient(
                        360.,
                        linear_color_stop(color.panel_background, 1.0),
                        linear_color_stop(color.editor_background, 0.45),
                    ))
                    .child(self.onboarding.clone()),
            )
            .child(h_flex().absolute().top_3().right_3().child(
                IconButton::new("cancel", IconName::Close).on_click(cx.listener(
                    |_, _: &ClickEvent, _window, cx| {
                        onboarding_event!("Cancelled", trigger = "X click");
                        cx.emit(DismissEvent);
                    },
                )),
            ))
    }
}
