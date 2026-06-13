use std::sync::Arc;

use gpui::{AnyElement, App, IntoElement, RenderOnce, Window};
use ui::{prelude::*};

#[derive(IntoElement, RegisterComponent)]
pub struct EndTrialUpsell {
}

impl EndTrialUpsell {
    pub fn new(_dismiss_upsell: Arc<dyn Fn(&mut Window, &mut App)>) -> Self {
        Self { }
    }
}

impl RenderOnce for EndTrialUpsell {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
    }
}

impl Component for EndTrialUpsell {
    fn scope() -> ComponentScope {
        ComponentScope::Onboarding
    }

    fn name() -> &'static str {
        "End of Trial Upsell Banner"
    }

    fn sort_name() -> &'static str {
        "End of Trial Upsell Banner"
    }

    fn description() -> &'static str {
        "A banner shown in the agent panel when a user's trial has ended, \
        inviting them to upgrade to a paid plan to continue using the agent."
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        div().into_any_element()
    }
}
