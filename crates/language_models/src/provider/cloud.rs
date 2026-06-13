use anyhow::Result;
use client::{
    Client, RefreshLlmTokenListener, TelemetrySettings, UserStore, global_llm_token,
};
use cloud_api_client::LlmApiToken;
use cloud_api_types::OrganizationId;
use futures::FutureExt;
use futures::StreamExt;
use futures::future::BoxFuture;
use gpui::{AnyView, App, AppContext, Context, Entity, Subscription, Task, TaskExt};
use language_model::{
    AuthenticateError, FastModeConfirmation, IconOrSvg, LanguageModel, LanguageModelProvider,
    LanguageModelProviderId, LanguageModelProviderName, LanguageModelProviderState,
    ZED_CLOUD_PROVIDER_ID, ZED_CLOUD_PROVIDER_NAME,
};
use language_models_cloud::{CloudLlmTokenProvider, CloudModelProvider};
use rand::{Rng as _, SeedableRng as _, rngs::StdRng};
use release_channel::AppVersion;

use settings::SettingsStore;
pub use settings::ZedDotDevAvailableModel as AvailableModel;
pub use settings::ZedDotDevAvailableProvider as AvailableProvider;
use std::sync::Arc;
use std::time::Duration;
use ui::{prelude::*};

const PROVIDER_ID: LanguageModelProviderId = ZED_CLOUD_PROVIDER_ID;
const PROVIDER_NAME: LanguageModelProviderName = ZED_CLOUD_PROVIDER_NAME;
const MODELS_REFRESH_DEBOUNCE: Duration = Duration::from_secs(5 * 60);

struct ClientTokenProvider {
    client: Arc<Client>,
    llm_api_token: LlmApiToken,
    user_store: Entity<UserStore>,
}

impl CloudLlmTokenProvider for ClientTokenProvider {
    type AuthContext = Option<OrganizationId>;

    fn auth_context(&self, cx: &impl AppContext) -> Self::AuthContext {
        self.user_store.read_with(cx, |user_store, _| {
            user_store
                .current_organization()
                .map(|organization| organization.id.clone())
        })
    }

    fn cached_token(
        &self,
        organization_id: Self::AuthContext,
    ) -> BoxFuture<'static, Result<String>> {
        let client = self.client.clone();
        let llm_api_token = self.llm_api_token.clone();
        Box::pin(async move {
            client
                .cached_llm_token(&llm_api_token, organization_id)
                .await
        })
    }

    fn refresh_token(
        &self,
        organization_id: Self::AuthContext,
    ) -> BoxFuture<'static, Result<String>> {
        let client = self.client.clone();
        let llm_api_token = self.llm_api_token.clone();
        Box::pin(async move {
            client
                .refresh_llm_token(&llm_api_token, organization_id)
                .await
        })
    }

    fn has_data_retention_consent(&self, cx: &impl AppContext) -> bool {
        cx.read_global(|settings_store: &SettingsStore, _| {
            settings_store
                .get::<TelemetrySettings>(None)
                .anthropic_retention
        })
    }
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct ZedDotDevSettings {
    pub available_models: Vec<AvailableModel>,
}

pub struct CloudLanguageModelProvider {
    state: Entity<State>,
    _maintain_client_status: Task<()>,
}

pub struct State {
    client: Arc<Client>,
    user_store: Entity<UserStore>,
    status: client::Status,
    provider: Entity<CloudModelProvider<ClientTokenProvider>>,
    pending_models_refresh: Option<Task<()>>,
    _user_store_subscription: Subscription,
    _settings_subscription: Subscription,
    _llm_token_subscription: Subscription,
    _provider_subscription: Subscription,
    _cloud_reconnect_task: Task<()>,
}

impl State {
    fn new(
        client: Arc<Client>,
        user_store: Entity<UserStore>,
        status: client::Status,
        cx: &mut Context<Self>,
    ) -> Self {
        let refresh_llm_token_listener = RefreshLlmTokenListener::global(cx);
        let token_provider = Arc::new(ClientTokenProvider {
            client: client.clone(),
            llm_api_token: global_llm_token(cx),
            user_store: user_store.clone(),
        });

        let provider = cx.new(|cx| {
            CloudModelProvider::new(
                token_provider.clone(),
                client.http_client(),
                Some(AppVersion::global(cx)),
            )
        });

        let cloud_reconnect_task = cx.spawn({
            let client = client.clone();
            async move |this, cx| {
                let mut connection_id_rx = client.cloud_connection_id();
                while let Some(connection_id) = connection_id_rx.next().await {
                    // The initial value `0` means no connection has been
                    // established since this `Client` was created; only real
                    // reconnects trigger a refresh.
                    if connection_id == 0 {
                        continue;
                    }
                    if this
                        .update(cx, |this, cx| this.schedule_debounced_models_refresh(cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });

        Self {
            client: client.clone(),
            user_store: user_store.clone(),
            status,
            pending_models_refresh: None,
            _provider_subscription: cx.observe(&provider, |_, _, cx| cx.notify()),
            provider,
            _user_store_subscription: cx.subscribe(
                &user_store,
                move |this, _user_store, event, cx| match event {
                    client::user::Event::PrivateUserInfoUpdated => {
                        let status = *client.status().borrow();
                        if status.is_signed_out() {
                            return;
                        }

                        this.refresh_models(cx);
                    }
                    _ => {}
                },
            ),
            _settings_subscription: cx.observe_global::<SettingsStore>(|_, cx| {
                cx.notify();
            }),
            _llm_token_subscription: cx.subscribe(
                &refresh_llm_token_listener,
                move |this, _listener, _event, cx| {
                    this.refresh_models(cx);
                },
            ),
            _cloud_reconnect_task: cloud_reconnect_task,
        }
    }

    fn is_signed_out(&self, cx: &App) -> bool {
        self.status.is_signed_out() || self.user_store.read(cx).current_user().is_none()
    }

    fn refresh_models(&mut self, cx: &mut Context<Self>) {
        self.provider.update(cx, |provider, cx| {
            provider.refresh_models(cx).detach_and_log_err(cx);
        });
    }

    /// Schedules a model list refresh, replacing any previously scheduled
    /// refresh.
    fn schedule_debounced_models_refresh(&mut self, cx: &mut Context<Self>) {
        self.pending_models_refresh = Some(cx.spawn(async move |this, cx| {
            #[cfg(any(test, feature = "test-support"))]
            let mut rng = StdRng::seed_from_u64(0);
            #[cfg(not(any(test, feature = "test-support")))]
            let mut rng = StdRng::from_os_rng();
            let jitter = Duration::from_millis(
                rng.random_range(0..MODELS_REFRESH_DEBOUNCE.as_millis() as u64),
            );
            cx.background_executor()
                .timer(MODELS_REFRESH_DEBOUNCE + jitter)
                .await;
            this.update(cx, |this, cx| this.refresh_models(cx)).ok();
        }));
    }
}

impl CloudLanguageModelProvider {
    pub fn new(user_store: Entity<UserStore>, client: Arc<Client>, cx: &mut App) -> Self {
        let mut status_rx = client.status();
        let status = *status_rx.borrow();

        let state = cx.new(|cx| State::new(client.clone(), user_store.clone(), status, cx));

        let state_ref = state.downgrade();
        let maintain_client_status = cx.spawn(async move |cx| {
            while let Some(status) = status_rx.next().await {
                if let Some(this) = state_ref.upgrade() {
                    _ = this.update(cx, |this, cx| {
                        if this.status != status {
                            this.status = status;
                            if status.is_signed_out() {
                                this.provider.update(cx, |provider, cx| {
                                    provider.clear_models();
                                    cx.notify();
                                });
                            }
                            cx.notify();
                        }
                    });
                } else {
                    break;
                }
            }
        });

        Self {
            state,
            _maintain_client_status: maintain_client_status,
        }
    }
}

impl LanguageModelProviderState for CloudLanguageModelProvider {
    type ObservableEntity = State;

    fn observable_entity(&self) -> Option<Entity<Self::ObservableEntity>> {
        Some(self.state.clone())
    }
}

impl LanguageModelProvider for CloudLanguageModelProvider {
    fn id(&self) -> LanguageModelProviderId {
        PROVIDER_ID
    }

    fn name(&self) -> LanguageModelProviderName {
        PROVIDER_NAME
    }

    fn icon(&self) -> IconOrSvg {
        IconOrSvg::Icon(IconName::AiZed)
    }

    fn default_model(&self, cx: &App) -> Option<Arc<dyn LanguageModel>> {
        let state = self.state.read(cx);
        let provider = state.provider.read(cx);
        let model = provider.default_model()?;
        Some(provider.create_model(model))
    }

    fn default_fast_model(&self, cx: &App) -> Option<Arc<dyn LanguageModel>> {
        let state = self.state.read(cx);
        let provider = state.provider.read(cx);
        let model = provider.default_fast_model()?;
        Some(provider.create_model(model))
    }

    fn recommended_models(&self, cx: &App) -> Vec<Arc<dyn LanguageModel>> {
        let state = self.state.read(cx);
        let provider = state.provider.read(cx);
        provider
            .recommended_models()
            .iter()
            .map(|model| provider.create_model(model))
            .collect()
    }

    fn provided_models(&self, cx: &App) -> Vec<Arc<dyn LanguageModel>> {
        let state = self.state.read(cx);
        let provider = state.provider.read(cx);
        provider
            .models()
            .iter()
            .map(|model| provider.create_model(model))
            .collect()
    }

    fn is_authenticated(&self, cx: &App) -> bool {
        let state = self.state.read(cx);
        !state.is_signed_out(cx)
    }

    fn authenticate(&self, cx: &mut App) -> Task<Result<(), AuthenticateError>> {
        if self.is_authenticated(cx) {
            return Task::ready(Ok(()));
        }
        let mut status = self.state.read(cx).client.status();
        let mut current_user = self.state.read(cx).user_store.read(cx).watch_current_user();
        if !status.borrow().is_signing_in() {
            return Task::ready(Ok(()));
        }
        cx.background_spawn(async move {
            while status.borrow().is_signing_in() {
                status.next().await;
            }
            while current_user.borrow().is_none() {
                let current_status = *status.borrow();
                if !matches!(
                    current_status,
                    client::Status::Authenticated
                        | client::Status::Reauthenticated
                        | client::Status::Connected { .. }
                ) {
                    return Err(AuthenticateError::Other(anyhow::anyhow!(
                        "sign-in did not complete: {current_status:?}"
                    )));
                }
                futures::select_biased! {
                    _ = current_user.next().fuse() => {},
                    _ = status.next().fuse() => {},
                }
            }
            Ok(())
        })
    }

    fn configuration_view(
        &self,
        _target_agent: language_model::ConfigurationViewTargetAgent,
        _: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        cx.new(|_| ConfigurationView::new(self.state.clone()))
            .into()
    }

    fn reset_credentials(&self, _cx: &mut App) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn fast_mode_confirmation(&self, _cx: &App) -> Option<FastModeConfirmation> {
        Some(FastModeConfirmation {
            title: "Enable Fast Mode for Zed?".into(),
            message: "Fast mode routes requests through the upstream provider's fast mode or priority tier. The \
                upstream provider's premium per-token pricing applies and is passed through to \
                your Zed billing."
                .into(),
        })
    }
}

struct ConfigurationView {
}

impl ConfigurationView {
    fn new(_state: Entity<State>) -> Self {
        Self {
        }
    }
}

impl Render for ConfigurationView {
    fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
