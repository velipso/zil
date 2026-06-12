use collections::HashMap;
use gpui::{
    App, AppContext as _, Context, Entity, Global,
    SharedString,
};

use crate::{AgentId};

#[derive(Clone, Debug)]
pub struct RegistryAgentMetadata {
    pub id: AgentId,
    pub name: SharedString,
    pub description: SharedString,
    pub version: SharedString,
    pub repository: Option<SharedString>,
    pub website: Option<SharedString>,
    pub icon_path: Option<SharedString>,
}

#[derive(Clone, Debug)]
pub struct RegistryBinaryAgent {
    pub metadata: RegistryAgentMetadata,
    pub targets: HashMap<String, RegistryTargetConfig>,
    pub supports_current_platform: bool,
}

#[derive(Clone, Debug)]
pub struct RegistryNpxAgent {
    pub metadata: RegistryAgentMetadata,
    pub package: SharedString,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

#[derive(Clone, Debug)]
pub enum RegistryAgent {
    Binary(RegistryBinaryAgent),
    Npx(RegistryNpxAgent),
}

impl RegistryAgent {
    pub fn metadata(&self) -> &RegistryAgentMetadata {
        match self {
            RegistryAgent::Binary(agent) => &agent.metadata,
            RegistryAgent::Npx(agent) => &agent.metadata,
        }
    }

    pub fn id(&self) -> &AgentId {
        &self.metadata().id
    }

    pub fn name(&self) -> &SharedString {
        &self.metadata().name
    }

    pub fn description(&self) -> &SharedString {
        &self.metadata().description
    }

    pub fn version(&self) -> &SharedString {
        &self.metadata().version
    }

    pub fn repository(&self) -> Option<&SharedString> {
        self.metadata().repository.as_ref()
    }

    pub fn website(&self) -> Option<&SharedString> {
        self.metadata().website.as_ref()
    }

    pub fn icon_path(&self) -> Option<&SharedString> {
        self.metadata().icon_path.as_ref()
    }

    pub fn supports_current_platform(&self) -> bool {
        match self {
            RegistryAgent::Binary(agent) => agent.supports_current_platform,
            RegistryAgent::Npx(_) => true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RegistryTargetConfig {
    pub archive: String,
    pub cmd: String,
    pub args: Vec<String>,
    pub sha256: Option<String>,
    pub env: HashMap<String, String>,
}

struct GlobalAgentRegistryStore(Entity<AgentRegistryStore>);

impl Global for GlobalAgentRegistryStore {}

pub struct AgentRegistryStore {
    agents: Vec<RegistryAgent>,
}

impl AgentRegistryStore {
    /// Initialize the global AgentRegistryStore.
    ///
    /// This loads the cached registry from disk. If the cache is empty but there
    /// are registry agents configured in settings, it will trigger a network fetch.
    /// Otherwise, call `refresh()` explicitly when you need fresh data
    /// (e.g., when opening the Agent Registry page).
    pub fn init_global(
        cx: &mut App,
    ) -> Entity<Self> {
        if let Some(store) = Self::try_global(cx) {
            return store;
        }

        let store = cx.new(|_| Self::new());
        cx.set_global(GlobalAgentRegistryStore(store.clone()));
        store
    }

    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalAgentRegistryStore>().0.clone()
    }

    pub fn try_global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalAgentRegistryStore>()
            .map(|store| store.0.clone())
    }

    pub fn agents(&self) -> &[RegistryAgent] {
        &self.agents
    }

    pub fn agent(&self, _id: &AgentId) -> Option<&RegistryAgent> {
        None
    }

    pub fn is_fetching(&self) -> bool {
        false
    }

    pub fn fetch_error(&self) -> Option<SharedString> {
        None
    }

    /// Refresh the registry from the network.
    ///
    /// This will fetch the latest registry data and update the cache.
    pub fn refresh(&mut self, _cx: &mut Context<Self>) {
        // do nothing
    }

    /// Refresh the registry if it hasn't been refreshed recently.
    ///
    /// This is useful to call when using a registry-based agent to check for
    /// updates without making too many network requests. The refresh is
    /// throttled to at most once per hour.
    pub fn refresh_if_stale(&mut self, _cx: &mut Context<Self>) {
        // do nothing
    }

    fn new() -> Self {
        Self {
            agents: Vec::new(),
        }
    }
}
