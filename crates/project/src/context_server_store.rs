pub mod extension;
pub mod registry;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use collections::HashMap;
use context_server::{ContextServer, ContextServerCommand, ContextServerId};
use credentials_provider::CredentialsProvider;
use futures::future::Either;
use gpui::{App, AsyncApp, Context, Entity, EventEmitter, Subscription, WeakEntity, actions};
use registry::ContextServerDescriptorRegistry;
use remote::RemoteClient;
use rpc::{AnyProtoClient, TypedEnvelope, proto};
use settings::{Settings as _, SettingsStore};
use util::{ResultExt as _, rel_path::RelPath};

use crate::{
    Project,
    project_settings::{ContextServerSettings, OAuthClientSettings, ProjectSettings},
    worktree_store::WorktreeStore,
};

pub fn init(cx: &mut App) {
    extension::init(cx);
}

actions!(
    context_server,
    [
        /// Restarts the context server.
        Restart
    ]
);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContextServerStatus {
    Starting,
    Running,
    Stopped,
    Error(Arc<str>),
    /// The server returned 401 and OAuth authorization is needed. The UI
    /// should show an "Authenticate" button.
    AuthRequired,
    /// The server has a pre-registered OAuth client_id, but a client_secret
    /// is needed and not available in settings or the keychain.
    ClientSecretRequired {
        error: Option<Arc<str>>,
    },
    /// The OAuth browser flow is in progress — the user has been redirected
    /// to the authorization server and we're waiting for the callback.
    Authenticating,
}

impl ContextServerStatus {
    fn from_state(_state: &ContextServerState) -> Self {
        ContextServerStatus::Stopped
    }
}

// VELIPSO: dead code, delete
#[allow(dead_code)]
enum ContextServerState {
    Whatever {
        server: Arc<ContextServer>,
        configuration: Arc<ContextServerConfiguration>,
    },
}

impl ContextServerState {
    pub fn server(&self) -> Arc<ContextServer> {
        match self {
            ContextServerState::Whatever { server, .. } => server.clone(),
        }
    }

    pub fn configuration(&self) -> Arc<ContextServerConfiguration> {
        match self {
            ContextServerState::Whatever { configuration, .. } => configuration.clone(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ContextServerConfiguration {
    Custom {
        command: ContextServerCommand,
        remote: bool,
    },
    Extension {
        command: ContextServerCommand,
        settings: serde_json::Value,
        remote: bool,
    },
    Http {
        url: url::Url,
        headers: HashMap<String, String>,
        timeout: Option<u64>,
        oauth: Option<OAuthClientSettings>,
    },
}

impl ContextServerConfiguration {
    pub fn command(&self) -> Option<&ContextServerCommand> {
        match self {
            ContextServerConfiguration::Custom { command, .. } => Some(command),
            ContextServerConfiguration::Extension { command, .. } => Some(command),
            ContextServerConfiguration::Http { .. } => None,
        }
    }

    pub fn has_static_auth_header(&self) -> bool {
        match self {
            ContextServerConfiguration::Http { headers, .. } => headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("authorization")),
            _ => false,
        }
    }

    pub fn remote(&self) -> bool {
        match self {
            ContextServerConfiguration::Custom { remote, .. } => *remote,
            ContextServerConfiguration::Extension { remote, .. } => *remote,
            ContextServerConfiguration::Http { .. } => false,
        }
    }

    pub async fn from_settings(
        settings: ContextServerSettings,
        id: ContextServerId,
        registry: Entity<ContextServerDescriptorRegistry>,
        worktree_store: Entity<WorktreeStore>,
        cx: &AsyncApp,
    ) -> Option<Self> {
        const EXTENSION_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

        match settings {
            ContextServerSettings::Stdio {
                enabled: _,
                command,
                remote,
            } => Some(ContextServerConfiguration::Custom { command, remote }),
            ContextServerSettings::Extension {
                enabled: _,
                settings,
                remote,
            } => {
                let descriptor =
                    cx.update(|cx| registry.read(cx).context_server_descriptor(&id.0))?;

                let command_future = descriptor.command(worktree_store, cx);
                let timeout_future = cx.background_executor().timer(EXTENSION_COMMAND_TIMEOUT);

                match futures::future::select(command_future, timeout_future).await {
                    Either::Left((Ok(command), _)) => Some(ContextServerConfiguration::Extension {
                        command,
                        settings,
                        remote,
                    }),
                    Either::Left((Err(e), _)) => {
                        log::error!(
                            "Failed to create context server configuration from settings: {e:#}"
                        );
                        None
                    }
                    Either::Right(_) => {
                        log::error!(
                            "Timed out resolving command for extension context server {id}"
                        );
                        None
                    }
                }
            }
            ContextServerSettings::Http {
                enabled: _,
                url,
                headers: auth,
                timeout,
                oauth,
            } => {
                let url = url::Url::parse(&url).log_err()?;
                Some(ContextServerConfiguration::Http {
                    url,
                    headers: auth,
                    timeout,
                    oauth,
                })
            }
        }
    }
}

pub type ContextServerFactory =
    Box<dyn Fn(ContextServerId, Arc<ContextServerConfiguration>) -> Arc<ContextServer>>;

enum ContextServerStoreState {
    Local {
        downstream_client: Option<(u64, AnyProtoClient)>,
    },
    Remote {},
}

pub struct ContextServerStore {
    state: ContextServerStoreState,
    context_server_settings: HashMap<Arc<str>, ContextServerSettings>,
    servers: HashMap<ContextServerId, ContextServerState>,
    server_ids: Vec<ContextServerId>,
    worktree_store: Entity<WorktreeStore>,
    _subscriptions: Vec<Subscription>,
}

pub struct ServerStatusChangedEvent {
    pub server_id: ContextServerId,
    pub status: ContextServerStatus,
}

impl EventEmitter<ServerStatusChangedEvent> for ContextServerStore {}

impl ContextServerStore {
    pub fn local(
        worktree_store: Entity<WorktreeStore>,
        _weak_project: Option<WeakEntity<Project>>,
        _headless: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_internal(
            worktree_store,
            ContextServerStoreState::Local {
                downstream_client: None,
            },
            cx,
        )
    }

    pub fn remote(
        _project_id: u64,
        _upstream_client: Entity<RemoteClient>,
        worktree_store: Entity<WorktreeStore>,
        _weak_project: Option<WeakEntity<Project>>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_internal(worktree_store, ContextServerStoreState::Remote {}, cx)
    }

    pub fn init_headless(session: &AnyProtoClient) {
        session.add_entity_request_handler(Self::handle_get_context_server_command);
    }

    pub fn shared(&mut self, project_id: u64, client: AnyProtoClient) {
        if let ContextServerStoreState::Local {
            downstream_client, ..
        } = &mut self.state
        {
            *downstream_client = Some((project_id, client));
        }
    }

    pub fn is_remote_project(&self) -> bool {
        matches!(self.state, ContextServerStoreState::Remote { .. })
    }

    /// Returns all configured context server ids, excluding the ones that are disabled
    pub fn configured_server_ids(&self) -> Vec<ContextServerId> {
        self.context_server_settings
            .iter()
            .filter(|(_, settings)| settings.enabled())
            .map(|(id, _)| ContextServerId(id.clone()))
            .collect()
    }

    fn new_internal(
        worktree_store: Entity<WorktreeStore>,
        state: ContextServerStoreState,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![cx.observe_global::<SettingsStore>(move |this, cx| {
            let settings =
                &Self::resolve_project_settings(&this.worktree_store, cx).context_servers;
            let settings_changed = &this.context_server_settings != settings;

            if settings_changed {
                this.context_server_settings = settings.clone();
            }
        })];

        Self {
            state,
            _subscriptions: subscriptions,
            context_server_settings: Self::resolve_project_settings(&worktree_store, cx)
                .context_servers
                .clone(),
            worktree_store,
            servers: HashMap::default(),
            server_ids: Default::default(),
        }
    }

    pub fn get_server(&self, id: &ContextServerId) -> Option<Arc<ContextServer>> {
        self.servers.get(id).map(|state| state.server())
    }

    pub fn get_running_server(&self, _id: &ContextServerId) -> Option<Arc<ContextServer>> {
        None
    }

    pub fn status_for_server(&self, id: &ContextServerId) -> Option<ContextServerStatus> {
        self.servers.get(id).map(ContextServerStatus::from_state)
    }

    pub fn configuration_for_server(
        &self,
        id: &ContextServerId,
    ) -> Option<Arc<ContextServerConfiguration>> {
        self.servers.get(id).map(|state| state.configuration())
    }

    /// Returns a sorted slice of available unique context server IDs. Within the
    /// slice, context servers which have `mcp-server-` as a prefix in their ID will
    /// appear after servers that do not have this prefix in their ID.
    pub fn server_ids(&self) -> &[ContextServerId] {
        self.server_ids.as_slice()
    }

    pub fn running_servers(&self) -> Vec<Arc<ContextServer>> {
        self.servers
            .values()
            .filter_map(|state| {
                let ContextServerState::Whatever { server, .. } = state;
                Some(server.clone())
            })
            .collect()
    }

    pub fn start_server(&mut self, _server: Arc<ContextServer>, _cx: &mut Context<Self>) {
        // do nothing
    }

    pub fn stop_server(&mut self, _id: &ContextServerId, _cx: &mut Context<Self>) -> Result<()> {
        Ok(())
    }

    pub async fn create_context_server(
        _this: WeakEntity<Self>,
        _id: ContextServerId,
        _configuration: Arc<ContextServerConfiguration>,
        _cx: &mut AsyncApp,
    ) -> Result<(Arc<ContextServer>, Arc<ContextServerConfiguration>)> {
        anyhow::bail!("Context servers are disabled")
    }

    async fn handle_get_context_server_command(
        _this: Entity<Self>,
        _envelope: TypedEnvelope<proto::GetContextServerCommand>,
        _cx: AsyncApp,
    ) -> Result<proto::ContextServerCommand> {
        anyhow::bail!("Context servers are disabled")
    }

    fn resolve_project_settings<'a>(
        worktree_store: &'a Entity<WorktreeStore>,
        cx: &'a App,
    ) -> &'a ProjectSettings {
        let location = worktree_store
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .map(|worktree| settings::SettingsLocation {
                worktree_id: worktree.read(cx).id(),
                path: RelPath::empty(),
            });
        ProjectSettings::get(location, cx)
    }

    /// Initiate the OAuth browser flow for a server in the `AuthRequired` state.
    ///
    /// This starts a loopback HTTP callback server on an ephemeral port, builds
    /// the authorization URL, opens the user's browser, waits for the callback,
    /// exchanges the code for tokens, persists them in the keychain, and restarts
    /// the server with the new token provider.
    pub fn authenticate_server(
        &mut self,
        _id: &ContextServerId,
        _cx: &mut Context<Self>,
    ) -> Result<()> {
        anyhow::bail!("Context servers are disabled")
    }

    /// Store the client secret and proceed with authentication.
    pub fn submit_client_secret(
        &mut self,
        _id: &ContextServerId,
        _secret: String,
        _cx: &mut Context<Self>,
    ) -> Result<()> {
        anyhow::bail!("Context servers are disabled")
    }

    pub async fn store_client_secret(
        _credentials_provider: &Arc<dyn CredentialsProvider>,
        _server_url: &url::Url,
        _secret: &str,
        _cx: &AsyncApp,
    ) -> Result<()> {
        anyhow::bail!("Context servers are disabled")
    }

    /// Log out of an OAuth-authenticated MCP server: clear the stored OAuth
    /// session from the keychain and stop the server.
    pub fn logout_server(&mut self, _id: &ContextServerId, _cx: &mut Context<Self>) -> Result<()> {
        anyhow::bail!("Context servers are disabled")
    }
}
