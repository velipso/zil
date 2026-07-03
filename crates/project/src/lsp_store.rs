//! LSP store provides unified access to the language server protocol.
//! The consumers of LSP store can interact with language servers without knowing exactly which language server they're interacting with.
//!
//! # Local/Remote LSP Stores
//! This module is split up into three distinct parts:
//! - [`LocalLspStore`], which is ran on the host machine (either project host or SSH host), that manages the lifecycle of language servers.
//! - [`RemoteLspStore`], which is ran on the remote machine (project guests) which is mostly about passing through the requests via RPC.
//!   The remote stores don't really care about which language server they're running against - they don't usually get to decide which language server is going to responsible for handling their request.
//! - [`LspStore`], which unifies the two under one consistent interface for interacting with language servers.
//!
//! Most of the interesting work happens at the local layer, as bulk of the complexity is with managing the lifecycle of language servers. The actual implementation of the LSP protocol is handled by [`lsp`] crate.
mod document_colors;
mod document_symbols;
mod folding_ranges;
pub mod log_store;
pub mod lsp_ext_command;
mod semantic_tokens;

use self::document_colors::DocumentColorData;
use self::document_symbols::DocumentSymbolsData;
use crate::{
    Hover,
    ManifestProvidersStore, Project, ProjectPath, ProjectTransaction,
    Symbol,
    buffer_store::{BufferStore, BufferStoreEvent},
    environment::ProjectEnvironment,
    lsp_command::*,
    lsp_store::{
        folding_ranges::FoldingRangeData,
        log_store::{GlobalLogStore, LanguageServerKind},
        semantic_tokens::{SemanticTokenConfig, SemanticTokensData},
    },
    manifest_tree::{
        LanguageServerTree, LanguageServerTreeNode, LaunchDisposition, ManifestQueryDelegate,
        ManifestTree,
    },
    project_settings::{BinarySettings, LspSettings, ProjectSettings},
    toolchain_store::{LocalToolchainStore, ToolchainStoreEvent},
    trusted_worktrees::{PathTrust, TrustedWorktrees, TrustedWorktreesEvent},
    worktree_store::{WorktreeStore, WorktreeStoreEvent},
    yarn::YarnPathStore,
};
use anyhow::{Context as _, Result, anyhow};
use async_trait::async_trait;
use client::proto;
use clock::Global;
use collections::{BTreeMap, BTreeSet, HashMap, HashSet, btree_map};
use futures::{
    Future, FutureExt, StreamExt,
    future::{Shared, join_all},
    select,
    stream::FuturesUnordered,
};
use globset::{Glob, GlobBuilder, GlobMatcher, GlobSet, GlobSetBuilder};
use gpui::{
    App, AppContext, AsyncApp, Context, Entity, EventEmitter, PromptLevel, SharedString,
    Subscription, Task, WeakEntity,
};
use http_client::HttpClient;
use itertools::Itertools as _;
use language::{
    Bias, BinaryStatus, Buffer, CachedLspAdapter, Capability, CodeLabel,
    File as _, Language, LanguageName, LanguageRegistry, LocalFile,
    LspAdapter, LspAdapterDelegate, LspInstaller, ManifestDelegate, ManifestName, ModelineSettings, PointUtf16, TextBufferSnapshot, ToOffset,
    Toolchain, Transaction, Unclipped,
    language_settings::{
        AllLanguageSettings, LanguageSettings,
        all_language_settings,
    },
    modeline, point_to_lsp,
    range_from_lsp,
};
use lsp::{
    DidChangeWatchedFilesRegistrationOptions, Edit, FileOperationFilter, FileOperationPatternKind,
    FileOperationRegistrationOptions, FileRename, FileSystemWatcher, LanguageServer,
    LanguageServerBinary, LanguageServerBinaryOptions, LanguageServerId, LanguageServerName,
    LanguageServerSelector, LspRequestFuture, MessageActionItem, MessageType, OneOf,
    RenameFilesParams, TextDocumentSyncSaveOptions, Uri, WillRenameFiles,
    WorkDoneProgressCancelParams, WorkspaceFolder, notification::DidRenameFiles,
};
use parking_lot::Mutex;
use postage::{sink::Sink, stream::Stream, watch};
use rpc::AnyProtoClient;
use serde::Serialize;
use serde_json::Value;
use settings::{Settings, SettingsLocation, SettingsStore};
use std::{
    cmp::Reverse,
    collections::hash_map,
    convert::TryInto,
    ffi::OsStr,
    num::NonZeroU32,
    ops::{ControlFlow, Range},
    path::{self, Path, PathBuf},
    sync::{
        Arc,
        atomic::{self, AtomicUsize},
    },
    time::{Duration, Instant},
    vec,
};
use sum_tree::Dimensions;
use text::{Anchor, BufferId};

use util::{
    ResultExt as _, debug_panic, defer, maybe, merge_json_value_into,
    paths::{PathStyle, SanitizedPath, UrlExt},
    redact::redact_command,
    rel_path::RelPath,
};

pub use document_colors::DocumentColors;
pub use folding_ranges::LspFoldingRange;
pub use fs::*;
pub use language::Location;
pub use semantic_tokens::{
    BufferSemanticToken, BufferSemanticTokens, RefreshForServer, SemanticTokenStylizer, TokenType,
};

pub use worktree::{
    Entry, EntryKind, FS_WATCH_LATENCY, File, LocalWorktree, PathChange, ProjectEntryId,
    UpdatedEntriesSet, UpdatedGitRepositoriesSet, Worktree, WorktreeId, WorktreeSettings,
};

const SERVER_LAUNCHING_BEFORE_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
pub const SERVER_PROGRESS_THROTTLE_TIMEOUT: Duration = Duration::from_millis(100);
const SERVER_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10);
static NEXT_PROMPT_REQUEST_ID: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum ProgressToken {
    Number(i32),
    String(SharedString),
}

impl std::fmt::Display for ProgressToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(number) => write!(f, "{number}"),
            Self::String(string) => write!(f, "{string}"),
        }
    }
}

impl ProgressToken {
    fn from_lsp(value: lsp::NumberOrString) -> Self {
        match value {
            lsp::NumberOrString::Number(number) => Self::Number(number),
            lsp::NumberOrString::String(string) => Self::String(SharedString::new(string)),
        }
    }

    fn to_lsp(&self) -> lsp::NumberOrString {
        match self {
            Self::Number(number) => lsp::NumberOrString::Number(*number),
            Self::String(string) => lsp::NumberOrString::String(string.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OpenLspBufferHandle(Entity<OpenLspBuffer>);

struct OpenLspBuffer(Entity<Buffer>);

#[derive(Clone)]
struct UnifiedLanguageServer {
    id: LanguageServerId,
    project_roots: HashSet<Arc<RelPath>>,
}

/// Settings that affect language server identity.
///
/// Dynamic settings (`LspSettings::settings`) are excluded because they can be
/// updated via `workspace/didChangeConfiguration` without restarting the server.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct LanguageServerSeedSettings {
    binary: Option<BinarySettings>,
    initialization_options: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct LanguageServerSeed {
    worktree_id: WorktreeId,
    name: LanguageServerName,
    toolchain: Option<Toolchain>,
    settings: LanguageServerSeedSettings,
}

#[derive(Default, Debug)]
struct DynamicRegistrations {
    did_change_watched_files: HashMap<String, Vec<FileSystemWatcher>>,
}

pub struct LocalLspStore {
    weak: WeakEntity<LspStore>,
    pub worktree_store: Entity<WorktreeStore>,
    toolchain_store: Entity<LocalToolchainStore>,
    http_client: Arc<dyn HttpClient>,
    environment: Entity<ProjectEnvironment>,
    fs: Arc<dyn Fs>,
    languages: Arc<LanguageRegistry>,
    language_server_ids: HashMap<LanguageServerSeed, UnifiedLanguageServer>,
    yarn: Entity<YarnPathStore>,
    pub language_servers: HashMap<LanguageServerId, LanguageServerState>,
    last_workspace_edits_by_language_server: HashMap<LanguageServerId, ProjectTransaction>,
    language_server_watched_paths: HashMap<LanguageServerId, LanguageServerWatchedPaths>,
    watched_manifest_filenames: HashSet<ManifestName>,
    language_server_paths_watched_for_rename:
        HashMap<LanguageServerId, RenamePathsWatchedForServer>,
    language_server_dynamic_registrations: HashMap<LanguageServerId, DynamicRegistrations>,
    supplementary_language_servers:
        HashMap<LanguageServerId, (LanguageServerName, Arc<LanguageServer>)>,
    buffer_snapshots: HashMap<BufferId, HashMap<LanguageServerId, Vec<LspBufferSnapshot>>>, // buffer_id -> server_id -> vec of snapshots
    _subscription: gpui::Subscription,
    lsp_tree: LanguageServerTree,
    registered_buffers: HashMap<BufferId, usize>,
    buffers_opened_in_servers: HashMap<BufferId, HashSet<LanguageServerId>>,
    restricted_worktrees_tasks: HashMap<WorktreeId, (Subscription, watch::Receiver<bool>)>,
}

impl LocalLspStore {
    /// Returns the running language server for the given ID. Note if the language server is starting, it will not be returned.
    pub fn running_language_server_for_id(
        &self,
        id: LanguageServerId,
    ) -> Option<&Arc<LanguageServer>> {
        let language_server_state = self.language_servers.get(&id)?;

        match language_server_state {
            LanguageServerState::Running { server, .. } => Some(server),
            LanguageServerState::Starting { .. } => None,
        }
    }

    fn get_or_insert_language_server(
        &mut self,
        worktree_handle: &Entity<Worktree>,
        delegate: Arc<LocalLspAdapterDelegate>,
        disposition: &Arc<LaunchDisposition>,
        language_name: &LanguageName,
        cx: &mut App,
    ) -> LanguageServerId {
        let key = LanguageServerSeed {
            worktree_id: worktree_handle.read(cx).id(),
            name: disposition.server_name.clone(),
            settings: LanguageServerSeedSettings {
                binary: disposition.settings.binary.clone(),
                initialization_options: disposition.settings.initialization_options.clone(),
            },
            toolchain: disposition.toolchain.clone(),
        };
        if let Some(state) = self.language_server_ids.get_mut(&key) {
            state.project_roots.insert(disposition.path.path.clone());
            state.id
        } else {
            let adapter = self
                .languages
                .lsp_adapters(language_name)
                .into_iter()
                .find(|adapter| adapter.name() == disposition.server_name)
                .expect("To find LSP adapter");
            let new_language_server_id = self.start_language_server(
                worktree_handle,
                delegate,
                adapter,
                disposition.settings.clone(),
                key.clone(),
                language_name.clone(),
                cx,
            );
            if let Some(state) = self.language_server_ids.get_mut(&key) {
                state.project_roots.insert(disposition.path.path.clone());
            } else {
                debug_assert!(
                    false,
                    "Expected `start_language_server` to ensure that `key` exists in a map"
                );
            }
            new_language_server_id
        }
    }

    fn start_language_server(
        &mut self,
        worktree_handle: &Entity<Worktree>,
        delegate: Arc<LocalLspAdapterDelegate>,
        adapter: Arc<CachedLspAdapter>,
        settings: Arc<LspSettings>,
        key: LanguageServerSeed,
        language_name: LanguageName,
        cx: &mut App,
    ) -> LanguageServerId {
        let worktree = worktree_handle.read(cx);

        let worktree_id = worktree.id();
        let worktree_abs_path = worktree.abs_path();
        let toolchain = key.toolchain.clone();
        let override_options = settings.initialization_options.clone();

        let stderr_capture = Arc::new(Mutex::new(Some(String::new())));

        let server_id = self.languages.next_language_server_id();
        log::trace!(
            "attempting to start language server {:?}, path: {worktree_abs_path:?}, id: {server_id}",
            adapter.name.0
        );

        let wait_until_worktree_trust =
            TrustedWorktrees::try_get_global(cx).and_then(|trusted_worktrees| {
                let can_trust = trusted_worktrees.update(cx, |trusted_worktrees, cx| {
                    trusted_worktrees.can_trust(&self.worktree_store, worktree_id, cx)
                });
                if can_trust {
                    self.restricted_worktrees_tasks.remove(&worktree_id);
                    None
                } else {
                    match self.restricted_worktrees_tasks.entry(worktree_id) {
                        hash_map::Entry::Occupied(o) => Some(o.get().1.clone()),
                        hash_map::Entry::Vacant(v) => {
                            let (mut tx, rx) = watch::channel::<bool>();
                            let lsp_store = self.weak.clone();
                            let subscription = cx.subscribe(&trusted_worktrees, move |_, e, cx| {
                                if let TrustedWorktreesEvent::Trusted(_, trusted_paths) = e {
                                    if trusted_paths.contains(&PathTrust::Worktree(worktree_id)) {
                                        tx.blocking_send(true).ok();
                                        lsp_store
                                            .update(cx, |lsp_store, _| {
                                                if let Some(local_lsp_store) =
                                                    lsp_store.as_local_mut()
                                                {
                                                    local_lsp_store
                                                        .restricted_worktrees_tasks
                                                        .remove(&worktree_id);
                                                }
                                            })
                                            .ok();
                                    }
                                }
                            });
                            v.insert((subscription, rx.clone()));
                            Some(rx)
                        }
                    }
                }
            });
        let update_binary_status = wait_until_worktree_trust.is_none();

        let binary = self.get_language_server_binary(
            worktree_abs_path.clone(),
            adapter.clone(),
            settings,
            toolchain.clone(),
            delegate.clone(),
            wait_until_worktree_trust,
            cx,
        );
        let pending_workspace_folders = Arc::<Mutex<BTreeSet<Uri>>>::default();

        let pending_server = cx.spawn({
            let adapter = adapter.clone();
            let server_name = adapter.name.clone();
            let stderr_capture = stderr_capture.clone();
            #[cfg(any(test, feature = "test-support"))]
            let lsp_store = self.weak.clone();
            let pending_workspace_folders = pending_workspace_folders.clone();
            async move |cx| {
                let binary = binary.await?;
                #[cfg(any(test, feature = "test-support"))]
                if let Some(server) = lsp_store
                    .update(&mut cx.clone(), |this, cx| {
                        this.languages.create_fake_language_server(
                            server_id,
                            &server_name,
                            binary.clone(),
                            &mut cx.to_async(),
                        )
                    })
                    .ok()
                    .flatten()
                {
                    return Ok(server);
                }

                lsp::LanguageServer::new(
                    stderr_capture,
                    server_id,
                    server_name,
                    binary,
                    &worktree_abs_path,
                    Some(pending_workspace_folders),
                    cx,
                )
            }
        });

        let startup = {
            let server_name = adapter.name.0.clone();
            let delegate = delegate as Arc<dyn LspAdapterDelegate>;
            let key = key.clone();
            let adapter = adapter.clone();
            let lsp_store = self.weak.clone();
            let pending_workspace_folders = pending_workspace_folders.clone();
            let settings_location = SettingsLocation {
                worktree_id,
                path: RelPath::empty(),
            };
            let augments_syntax_tokens = AllLanguageSettings::get(Some(settings_location), cx)
                .language(Some(settings_location), Some(&language_name), cx)
                .semantic_tokens
                .use_tree_sitter();
            cx.spawn(async move |cx| {
                let result = async {
                    let language_server = pending_server.await?;

                    let workspace_config = Self::workspace_configuration_for_adapter(
                        adapter.adapter.clone(),
                        &delegate,
                        toolchain,
                        None,
                        cx,
                    )
                    .await?;

                    let mut initialization_options = Self::initialization_options_for_adapter(
                        adapter.adapter.clone(),
                        &delegate,
                        cx,
                    )
                    .await?;

                    match (&mut initialization_options, override_options) {
                        (Some(initialization_options), Some(override_options)) => {
                            merge_json_value_into(override_options, initialization_options);
                        }
                        (None, override_options) => initialization_options = override_options,
                        _ => {}
                    }

                    let initialization_params = cx.update(|cx| {
                        let mut params = language_server.default_initialize_params(
                            augments_syntax_tokens,
                            cx,
                        );
                        params.initialization_options = initialization_options;
                        adapter.adapter.prepare_initialize_params(params, cx)
                    })?;

                    Self::setup_lsp_messages(
                        lsp_store.clone(),
                        &language_server,
                        delegate.clone(),
                        adapter.clone(),
                    );

                    let did_change_configuration_params = lsp::DidChangeConfigurationParams {
                        settings: workspace_config,
                    };
                    let language_server = cx
                        .update(|cx| {
                            let request_timeout = ProjectSettings::get_global(cx)
                                .global_lsp_settings
                                .get_request_timeout();

                            language_server.initialize(
                                initialization_params,
                                Arc::new(did_change_configuration_params.clone()),
                                request_timeout,
                                cx,
                            )
                        })
                        .await
                        .inspect_err(|_| {
                            if let Some(lsp_store) = lsp_store.upgrade() {
                                lsp_store.update(cx, |lsp_store, cx| {
                                    lsp_store.cleanup_lsp_data(server_id);
                                    cx.emit(LspStoreEvent::LanguageServerRemoved(server_id))
                                });
                            }
                        })?;

                    language_server.notify::<lsp::notification::DidChangeConfiguration>(
                        did_change_configuration_params,
                    )?;

                    anyhow::Ok(language_server)
                }
                .await;

                match result {
                    Ok(server) => {
                        lsp_store
                            .update(cx, |lsp_store, cx| {
                                lsp_store.insert_newly_running_language_server(
                                    adapter,
                                    server.clone(),
                                    server_id,
                                    key,
                                    language_name,
                                    pending_workspace_folders,
                                    cx,
                                );
                            })
                            .ok();
                        stderr_capture.lock().take();
                        Some(server)
                    }

                    Err(err) => {
                        let log = stderr_capture.lock().take().unwrap_or_default();
                        delegate.update_status(
                            adapter.name(),
                            BinaryStatus::Failed {
                                error: if log.is_empty() {
                                    format!("{err:#}")
                                } else {
                                    format!("{err:#}\n-- stderr --\n{log}")
                                },
                            },
                        );
                        log::error!(
                            "Failed to start language server {server_name:?}: {}",
                            redact_command(&format!("{err:?}"))
                        );
                        if !log.is_empty() {
                            log::error!("server stderr: {}", redact_command(&log));
                        }
                        None
                    }
                }
            })
        };
        let state = LanguageServerState::Starting {
            startup,
            pending_workspace_folders,
        };

        if update_binary_status {
            self.languages
                .update_lsp_binary_status(adapter.name(), BinaryStatus::Starting);
        }

        self.language_servers.insert(server_id, state);
        self.language_server_ids
            .entry(key)
            .or_insert(UnifiedLanguageServer {
                id: server_id,
                project_roots: Default::default(),
            });
        server_id
    }

    fn get_language_server_binary(
        &self,
        worktree_abs_path: Arc<Path>,
        adapter: Arc<CachedLspAdapter>,
        settings: Arc<LspSettings>,
        toolchain: Option<Toolchain>,
        delegate: Arc<dyn LspAdapterDelegate>,
        wait_until_worktree_trust: Option<watch::Receiver<bool>>,
        cx: &mut App,
    ) -> Task<Result<LanguageServerBinary>> {
        if let Some(settings) = &settings.binary
            && let Some(path) = settings.path.as_ref().map(PathBuf::from)
        {
            let settings = settings.clone();
            let languages = self.languages.clone();
            return cx.background_spawn(async move {
                if let Some(mut wait_until_worktree_trust) = wait_until_worktree_trust {
                    let already_trusted =  *wait_until_worktree_trust.borrow();
                    if !already_trusted {
                        log::info!(
                            "Waiting for worktree {worktree_abs_path:?} to be trusted, before starting language server {}",
                            adapter.name(),
                        );
                        while let Some(worktree_trusted) = wait_until_worktree_trust.recv().await {
                            if worktree_trusted {
                                break;
                            }
                        }
                        log::info!(
                            "Worktree {worktree_abs_path:?} is trusted, starting language server {}",
                            adapter.name(),
                        );
                    }
                    languages
                        .update_lsp_binary_status(adapter.name(), BinaryStatus::Starting);
                }
                let mut env = delegate.shell_env().await;
                env.extend(settings.env.unwrap_or_default());

                Ok(LanguageServerBinary {
                    path: delegate.resolve_relative_path(path),
                    env: Some(env),
                    arguments: settings
                        .arguments
                        .unwrap_or_default()
                        .iter()
                        .map(Into::into)
                        .collect(),
                })
            });
        }

        #[cfg(any(test, feature = "test-support"))]
        if !adapter.adapter.is_extension() && self.languages.has_fake_lsp_server(&adapter.name) {
            let language_server_name = adapter.name.clone();
            let languages = self.languages.clone();
            return cx.spawn(async move |_| {
                if let Some(mut wait_until_worktree_trust) = wait_until_worktree_trust {
                    let already_trusted = *wait_until_worktree_trust.borrow();
                    if !already_trusted {
                        log::info!(
                            "Waiting for worktree {worktree_abs_path:?} to be trusted, before starting language server {language_server_name}",
                        );
                        while let Some(worktree_trusted) = wait_until_worktree_trust.recv().await {
                            if worktree_trusted {
                                break;
                            }
                        }
                        log::info!(
                            "Worktree {worktree_abs_path:?} is trusted, starting language server {language_server_name}",
                        );
                    }
                    languages.update_lsp_binary_status(
                        language_server_name.clone(),
                        BinaryStatus::Starting,
                    );
                }

                Ok(LanguageServerBinary {
                    path: PathBuf::from(format!("/fake/lsp/{language_server_name}")),
                    arguments: Vec::new(),
                    env: None,
                })
            });
        }

        if cfg!(any(test, feature = "test-support")) && !adapter.adapter.is_extension() {
            return Task::ready(Err(anyhow!(
                "language server binary lookup for {:?} is disabled in tests; register a fake language server or configure an explicit binary",
                adapter.name
            )));
        }

        let lsp_binary_options = LanguageServerBinaryOptions {
            allow_path_lookup: !settings
                .binary
                .as_ref()
                .and_then(|b| b.ignore_system_version)
                .unwrap_or_default(),
            pre_release: settings
                .fetch
                .as_ref()
                .and_then(|f| f.pre_release)
                .unwrap_or(false),
        };

        cx.spawn(async move |cx| {
            if let Some(mut wait_until_worktree_trust) = wait_until_worktree_trust {
                let already_trusted = *wait_until_worktree_trust.borrow();
                if !already_trusted {
                    log::info!(
                        "Waiting for worktree {worktree_abs_path:?} to be trusted, \
                        before starting language server {}",
                        adapter.name(),
                    );
                    while let Some(worktree_trusted) = wait_until_worktree_trust.recv().await {
                        if worktree_trusted {
                            break;
                        }
                    }
                    log::info!(
                        "Worktree {worktree_abs_path:?} is trusted, starting language server {}",
                        adapter.name(),
                    );
                }
            }

            let (existing_binary, maybe_download_binary) = adapter
                .clone()
                .get_language_server_command(delegate.clone(), toolchain, lsp_binary_options, cx)
                .await
                .await;

            delegate.update_status(adapter.name.clone(), BinaryStatus::None);

            let mut binary = match (existing_binary, maybe_download_binary) {
                (binary, None) => binary?,
                (Err(_), Some(downloader)) => downloader.await?,
                (Ok(existing_binary), Some(downloader)) => {
                    let mut download_timeout = cx
                        .background_executor()
                        .timer(SERVER_DOWNLOAD_TIMEOUT)
                        .fuse();
                    let mut downloader = downloader.fuse();
                    futures::select! {
                        _ = download_timeout => {
                            // Return existing binary and kick the existing work to the background.
                            cx.spawn(async move |_| downloader.await).detach();
                            Ok(existing_binary)
                        },
                        downloaded_or_existing_binary = downloader => {
                            // If download fails, this results in the existing binary.
                            downloaded_or_existing_binary
                        }
                    }?
                }
            };
            let mut shell_env = delegate.shell_env().await;

            shell_env.extend(binary.env.unwrap_or_default());

            if let Some(settings) = settings.binary.as_ref() {
                if let Some(arguments) = &settings.arguments {
                    binary.arguments = arguments.iter().map(Into::into).collect();
                }
                if let Some(env) = &settings.env {
                    shell_env.extend(env.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            }

            binary.env = Some(shell_env);
            Ok(binary)
        })
    }

    fn setup_lsp_messages(
        lsp_store: WeakEntity<LspStore>,
        language_server: &LanguageServer,
        delegate: Arc<dyn LspAdapterDelegate>,
        adapter: Arc<CachedLspAdapter>,
    ) {
        let name = language_server.name();
        let server_id = language_server.server_id();
        language_server
            .on_request::<lsp::request::WorkspaceConfiguration, _, _>({
                let adapter = adapter.adapter.clone();
                let delegate = delegate.clone();
                let this = lsp_store.clone();
                move |params, cx| {
                    let adapter = adapter.clone();
                    let delegate = delegate.clone();
                    let this = this.clone();
                    let mut cx = cx.clone();
                    async move {
                        let toolchain_for_id = this
                            .update(&mut cx, |this, _| {
                                this.as_local()?.language_server_ids.iter().find_map(
                                    |(seed, value)| {
                                        (value.id == server_id).then(|| seed.toolchain.clone())
                                    },
                                )
                            })?
                            .context("Expected the LSP store to be in a local mode")?;

                        let mut scope_uri_to_workspace_config = BTreeMap::new();
                        for item in &params.items {
                            let scope_uri = item.scope_uri.clone();
                            let std::collections::btree_map::Entry::Vacant(new_scope_uri) =
                                scope_uri_to_workspace_config.entry(scope_uri.clone())
                            else {
                                // We've already queried workspace configuration of this URI.
                                continue;
                            };
                            let workspace_config = Self::workspace_configuration_for_adapter(
                                adapter.clone(),
                                &delegate,
                                toolchain_for_id.clone(),
                                scope_uri,
                                &mut cx,
                            )
                            .await?;
                            new_scope_uri.insert(workspace_config);
                        }

                        Ok(params
                            .items
                            .into_iter()
                            .filter_map(|item| {
                                let workspace_config =
                                    scope_uri_to_workspace_config.get(&item.scope_uri)?;
                                if let Some(section) = &item.section {
                                    Some(
                                        workspace_config
                                            .get(section)
                                            .cloned()
                                            .unwrap_or(serde_json::Value::Null),
                                    )
                                } else {
                                    Some(workspace_config.clone())
                                }
                            })
                            .collect())
                    }
                }
            })
            .detach();

        language_server
            .on_request::<lsp::request::WorkspaceFoldersRequest, _, _>({
                let this = lsp_store.clone();
                move |_, cx| {
                    let this = this.clone();
                    let cx = cx.clone();
                    async move {
                        let Some(server) =
                            this.read_with(&cx, |this, _| this.language_server_for_id(server_id))?
                        else {
                            return Ok(None);
                        };
                        let root = server.workspace_folders();
                        Ok(Some(
                            root.into_iter()
                                .map(|uri| WorkspaceFolder {
                                    uri,
                                    name: Default::default(),
                                })
                                .collect(),
                        ))
                    }
                }
            })
            .detach();
        // Even though we don't have handling for these requests, respond to them to
        // avoid stalling any language server like `gopls` which waits for a response
        // to these requests when initializing.
        language_server
            .on_request::<lsp::request::WorkDoneProgressCreate, _, _>({
                let this = lsp_store.clone();
                move |params, cx| {
                    let this = this.clone();
                    let mut cx = cx.clone();
                    async move {
                        this.update(&mut cx, |this, _| {
                            if let Some(status) = this.language_server_statuses.get_mut(&server_id)
                            {
                                status
                                    .progress_tokens
                                    .insert(ProgressToken::from_lsp(params.token));
                            }
                        })?;

                        Ok(())
                    }
                }
            })
            .detach();

        language_server
            .on_request::<lsp::request::RegisterCapability, _, _>({
                let lsp_store = lsp_store.clone();
                move |params, cx| {
                    let lsp_store = lsp_store.clone();
                    let mut cx = cx.clone();
                    async move {
                        lsp_store
                            .update(&mut cx, |lsp_store, cx| {
                                if lsp_store.as_local().is_some() {
                                    match lsp_store
                                        .register_server_capabilities(server_id, params, cx)
                                    {
                                        Ok(()) => {}
                                        Err(e) => {
                                            log::error!(
                                                "Failed to register server capabilities: {e:#}"
                                            );
                                        }
                                    };
                                }
                            })
                            .ok();
                        Ok(())
                    }
                }
            })
            .detach();

        language_server
            .on_request::<lsp::request::UnregisterCapability, _, _>({
                let lsp_store = lsp_store.clone();
                move |params, cx| {
                    let lsp_store = lsp_store.clone();
                    let mut cx = cx.clone();
                    async move {
                        lsp_store
                            .update(&mut cx, |lsp_store, cx| {
                                if lsp_store.as_local().is_some() {
                                    match lsp_store
                                        .unregister_server_capabilities(server_id, params, cx)
                                    {
                                        Ok(()) => {}
                                        Err(e) => {
                                            log::error!(
                                                "Failed to unregister server capabilities: {e:#}"
                                            );
                                        }
                                    }
                                }
                            })
                            .ok();
                        Ok(())
                    }
                }
            })
            .detach();

        language_server
            .on_request::<lsp::request::ApplyWorkspaceEdit, _, _>({
                let this = lsp_store.clone();
                move |params, cx| {
                    let mut cx = cx.clone();
                    let this = this.clone();
                    async move {
                        LocalLspStore::on_lsp_workspace_edit(
                            this.clone(),
                            params,
                            server_id,
                            &mut cx,
                        )
                        .await
                    }
                }
            })
            .detach();

        language_server
            .on_request::<lsp::request::SemanticTokensRefresh, _, _>({
                let lsp_store = lsp_store.clone();
                let request_id = Arc::new(AtomicUsize::new(0));
                move |(), cx| {
                    let lsp_store = lsp_store.clone();
                    let request_id = request_id.clone();
                    let mut cx = cx.clone();
                    async move {
                        let _ = lsp_store
                            .update(&mut cx, |_lsp_store, cx| {
                                let request_id =
                                    Some(request_id.fetch_add(1, atomic::Ordering::AcqRel));
                                cx.emit(LspStoreEvent::RefreshSemanticTokens {
                                    server_id,
                                    request_id,
                                });
                            });
                        Ok(())
                    }
                }
            })
            .detach();

        language_server
            .on_request::<lsp::request::ShowMessageRequest, _, _>({
                let this = lsp_store.clone();
                let name = name.to_string();
                let adapter = adapter.clone();
                move |params, cx| {
                    let this = this.clone();
                    let name = name.to_string();
                    let adapter = adapter.clone();
                    let mut cx = cx.clone();
                    async move {
                        let actions = params.actions.unwrap_or_default();
                        let message = params.message.clone();
                        let (tx, rx) = async_channel::bounded::<MessageActionItem>(1);
                        let level = match params.typ {
                            lsp::MessageType::ERROR => PromptLevel::Critical,
                            lsp::MessageType::WARNING => PromptLevel::Warning,
                            _ => PromptLevel::Info,
                        };
                        let request = LanguageServerPromptRequest::new(
                            level,
                            params.message,
                            actions,
                            name.clone(),
                            tx,
                        );

                        let did_update = this
                            .update(&mut cx, |_, cx| {
                                cx.emit(LspStoreEvent::LanguageServerPrompt(request));
                            })
                            .is_ok();
                        if did_update {
                            let response = rx.recv().await.ok();
                            if let Some(ref selected_action) = response {
                                let context = language::PromptResponseContext {
                                    message,
                                    selected_action: selected_action.clone(),
                                };
                                adapter.process_prompt_response(&context, &mut cx)
                            }

                            Ok(response)
                        } else {
                            Ok(None)
                        }
                    }
                }
            })
            .detach();
        language_server
            .on_notification::<lsp::notification::ShowMessage, _>({
                let this = lsp_store.clone();
                let name = name.to_string();
                move |params, cx| {
                    let this = this.clone();
                    let name = name.to_string();
                    let mut cx = cx.clone();

                    let (tx, _) = async_channel::bounded(1);
                    let level = match params.typ {
                        lsp::MessageType::ERROR => PromptLevel::Critical,
                        lsp::MessageType::WARNING => PromptLevel::Warning,
                        _ => PromptLevel::Info,
                    };
                    let request =
                        LanguageServerPromptRequest::new(level, params.message, vec![], name, tx);

                    let _ = this.update(&mut cx, |_, cx| {
                        cx.emit(LspStoreEvent::LanguageServerPrompt(request));
                    });
                }
            })
            .detach();

        language_server
            .on_notification::<lsp::notification::Progress, _>({
                let this = lsp_store.clone();
                move |params, cx| {
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |this, cx| {
                            this.on_lsp_progress(
                                params,
                                server_id,
                                cx,
                            );
                        });
                    }
                }
            })
            .detach();

        language_server
            .on_notification::<lsp::notification::LogMessage, _>({
                let this = lsp_store.clone();
                move |params, cx| {
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |_, cx| {
                            cx.emit(LspStoreEvent::LanguageServerLog(
                                server_id,
                                LanguageServerLogType::Log(params.typ),
                                params.message,
                            ));
                        });
                    }
                }
            })
            .detach();

        language_server
            .on_notification::<lsp::notification::LogTrace, _>({
                let this = lsp_store.clone();
                move |params, cx| {
                    let mut cx = cx.clone();
                    if let Some(this) = this.upgrade() {
                        this.update(&mut cx, |_, cx| {
                            cx.emit(LspStoreEvent::LanguageServerLog(
                                server_id,
                                LanguageServerLogType::Trace {
                                    verbose_info: params.verbose,
                                },
                                params.message,
                            ));
                        });
                    }
                }
            })
            .detach();
    }

    fn shutdown_language_servers_on_quit(&mut self) -> impl Future<Output = ()> + use<> {
        let shutdown_futures = self
            .language_servers
            .drain()
            .map(|(_, server_state)| Self::shutdown_server(server_state))
            .collect::<Vec<_>>();

        async move {
            join_all(shutdown_futures).await;
        }
    }

    async fn shutdown_server(server_state: LanguageServerState) -> anyhow::Result<()> {
        match server_state {
            LanguageServerState::Running { server, .. } => {
                if let Some(shutdown) = server.shutdown() {
                    shutdown.await;
                }
            }
            LanguageServerState::Starting { startup, .. } => {
                if let Some(server) = startup.await
                    && let Some(shutdown) = server.shutdown()
                {
                    shutdown.await;
                }
            }
        }
        Ok(())
    }

    fn language_servers_for_worktree(
        &self,
        worktree_id: WorktreeId,
    ) -> impl Iterator<Item = &Arc<LanguageServer>> {
        self.language_server_ids
            .iter()
            .filter_map(move |(seed, state)| {
                if seed.worktree_id != worktree_id {
                    return None;
                }

                if let Some(LanguageServerState::Running { server, .. }) =
                    self.language_servers.get(&state.id)
                {
                    Some(server)
                } else {
                    None
                }
            })
    }

    fn language_server_ids_for_project_path(
        &self,
        project_path: ProjectPath,
        language: &Language,
        cx: &mut App,
    ) -> Vec<LanguageServerId> {
        let Some(worktree) = self
            .worktree_store
            .read(cx)
            .worktree_for_id(project_path.worktree_id, cx)
        else {
            return Vec::new();
        };
        let delegate: Arc<dyn ManifestDelegate> =
            Arc::new(ManifestQueryDelegate::new(worktree.read(cx).snapshot()));

        self.lsp_tree
            .get(
                project_path,
                language.name(),
                language.manifest(),
                &delegate,
                cx,
            )
            .collect::<Vec<_>>()
    }

    fn language_server_ids_for_buffer(
        &self,
        buffer: &Buffer,
        cx: &mut App,
    ) -> Vec<LanguageServerId> {
        if let Some((file, language)) = File::from_dyn(buffer.file()).zip(buffer.language()) {
            let worktree_id = file.worktree_id(cx);

            let path: Arc<RelPath> = file
                .path()
                .parent()
                .map(Arc::from)
                .unwrap_or_else(|| file.path().clone());
            let worktree_path = ProjectPath { worktree_id, path };
            self.language_server_ids_for_project_path(worktree_path, language, cx)
        } else {
            Vec::new()
        }
    }

    fn language_servers_for_buffer<'a>(
        &'a self,
        buffer: &'a Buffer,
        cx: &'a mut App,
    ) -> impl Iterator<Item = (&'a Arc<CachedLspAdapter>, &'a Arc<LanguageServer>)> {
        self.language_server_ids_for_buffer(buffer, cx)
            .into_iter()
            .filter_map(|server_id| match self.language_servers.get(&server_id)? {
                LanguageServerState::Running {
                    adapter, server, ..
                } => Some((adapter, server)),
                _ => None,
            })
    }

    fn initialize_buffer(&mut self, buffer_handle: &Entity<Buffer>, cx: &mut Context<LspStore>) {
        let buffer = buffer_handle.read(cx);

        let file = buffer.file().cloned();

        let Some(file) = File::from_dyn(file.as_ref()) else {
            return;
        };
        if !file.is_local() {
            return;
        }
        let path = ProjectPath::from_file(file, cx);
        let worktree_id = file.worktree_id(cx);
        let language = buffer.language().cloned();

        let Some(language) = language else {
            return;
        };
        let Some(snapshot) = self
            .worktree_store
            .read(cx)
            .worktree_for_id(worktree_id, cx)
            .map(|worktree| worktree.read(cx).snapshot())
        else {
            return;
        };
        let delegate: Arc<dyn ManifestDelegate> = Arc::new(ManifestQueryDelegate::new(snapshot));

        for server_id in
            self.lsp_tree
                .get(path, language.name(), language.manifest(), &delegate, cx)
        {
            let server = self
                .language_servers
                .get(&server_id)
                .and_then(|server_state| {
                    if let LanguageServerState::Running { server, .. } = server_state {
                        Some(server.clone())
                    } else {
                        None
                    }
                });
            let server = match server {
                Some(server) => server,
                None => continue,
            };

            buffer_handle.update(cx, |buffer, cx| {
                buffer.set_completion_triggers(
                    server.server_id(),
                    server
                        .capabilities()
                        .completion_provider
                        .as_ref()
                        .and_then(|provider| {
                            provider
                                .trigger_characters
                                .as_ref()
                                .map(|characters| characters.iter().cloned().collect())
                        })
                        .unwrap_or_default(),
                    cx,
                );
            });
        }
    }

    pub(crate) fn reset_buffer(&mut self, buffer: &Entity<Buffer>, old_file: &File, cx: &mut App) {
        buffer.update(cx, |buffer, cx| {
            let Some(language) = buffer.language() else {
                return;
            };
            let path = ProjectPath {
                worktree_id: old_file.worktree_id(cx),
                path: old_file.path.clone(),
            };
            for server_id in self.language_server_ids_for_project_path(path, language, cx) {
                buffer.set_completion_triggers(server_id, Default::default(), cx);
            }
        });
    }

    fn register_language_server_for_invisible_worktree(
        &mut self,
        worktree: &Entity<Worktree>,
        language_server_id: LanguageServerId,
        cx: &mut App,
    ) {
        let worktree = worktree.read(cx);
        let worktree_id = worktree.id();
        debug_assert!(!worktree.is_visible());
        let Some(mut origin_seed) = self
            .language_server_ids
            .iter()
            .find_map(|(seed, state)| (state.id == language_server_id).then(|| seed.clone()))
        else {
            return;
        };
        origin_seed.worktree_id = worktree_id;
        self.language_server_ids
            .entry(origin_seed)
            .or_insert_with(|| UnifiedLanguageServer {
                id: language_server_id,
                project_roots: Default::default(),
            });
    }

    fn register_buffer_with_language_servers(
        &mut self,
        buffer_handle: &Entity<Buffer>,
        only_register_servers: HashSet<LanguageServerSelector>,
        cx: &mut Context<LspStore>,
    ) {
        let buffer = buffer_handle.read(cx);
        let buffer_id = buffer.remote_id();

        let Some(file) = File::from_dyn(buffer.file()) else {
            return;
        };
        if !file.is_local() {
            return;
        }

        let abs_path = file.abs_path(cx);
        let Some(uri) = file_path_to_lsp_url(&abs_path).log_err() else {
            return;
        };
        let initial_snapshot = buffer.text_snapshot();
        let worktree_id = file.worktree_id(cx);

        let Some(language) = buffer.language().cloned() else {
            return;
        };
        let path: Arc<RelPath> = file
            .path()
            .parent()
            .map(Arc::from)
            .unwrap_or_else(|| file.path().clone());
        let Some(worktree) = self
            .worktree_store
            .read(cx)
            .worktree_for_id(worktree_id, cx)
        else {
            return;
        };
        let language_name = language.name();
        let (reused, delegate, servers) = self
            .reuse_existing_language_server(&self.lsp_tree, &worktree, &language_name, cx)
            .map(|(delegate, apply)| (true, delegate, apply(&mut self.lsp_tree)))
            .unwrap_or_else(|| {
                let lsp_delegate = LocalLspAdapterDelegate::from_local_lsp(self, &worktree, cx);
                let delegate: Arc<dyn ManifestDelegate> =
                    Arc::new(ManifestQueryDelegate::new(worktree.read(cx).snapshot()));

                let servers = self
                    .lsp_tree
                    .walk(
                        ProjectPath { worktree_id, path },
                        language.name(),
                        language.manifest(),
                        &delegate,
                        cx,
                    )
                    .collect::<Vec<_>>();
                (false, lsp_delegate, servers)
            });
        let servers_and_adapters = servers
            .into_iter()
            .filter_map(|server_node| {
                if reused && server_node.server_id().is_none() {
                    return None;
                }
                if !only_register_servers.is_empty() {
                    if let Some(server_id) = server_node.server_id()
                        && !only_register_servers.contains(&LanguageServerSelector::Id(server_id))
                    {
                        return None;
                    }
                    if let Some(name) = server_node.name()
                        && !only_register_servers.contains(&LanguageServerSelector::Name(name))
                    {
                        return None;
                    }
                }

                let server_id = server_node.server_id_or_init(|disposition| {
                    let path = &disposition.path;

                    {
                        let uri = Uri::from_file_path(worktree.read(cx).absolutize(&path.path));

                        let server_id = self.get_or_insert_language_server(
                            &worktree,
                            delegate.clone(),
                            disposition,
                            &language_name,
                            cx,
                        );

                        if let Some(state) = self.language_servers.get(&server_id)
                            && let Ok(uri) = uri
                        {
                            state.add_workspace_folder(uri);
                        };
                        server_id
                    }
                })?;
                let server_state = self.language_servers.get(&server_id)?;
                if let LanguageServerState::Running {
                    server, adapter, ..
                } = server_state
                {
                    Some((server.clone(), adapter.clone()))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for (server, adapter) in servers_and_adapters {
            buffer_handle.update(cx, |buffer, cx| {
                buffer.set_completion_triggers(
                    server.server_id(),
                    server
                        .capabilities()
                        .completion_provider
                        .as_ref()
                        .and_then(|provider| {
                            provider
                                .trigger_characters
                                .as_ref()
                                .map(|characters| characters.iter().cloned().collect())
                        })
                        .unwrap_or_default(),
                    cx,
                );
            });

            let snapshot = LspBufferSnapshot {
                version: 0,
                snapshot: initial_snapshot.clone(),
            };

            let mut registered = false;
            self.buffer_snapshots
                .entry(buffer_id)
                .or_default()
                .entry(server.server_id())
                .or_insert_with(|| {
                    registered = true;
                    server.register_buffer(
                        uri.clone(),
                        adapter.language_id(&language.name()),
                        0,
                        initial_snapshot.text(),
                    );

                    vec![snapshot]
                });

            self.buffers_opened_in_servers
                .entry(buffer_id)
                .or_default()
                .insert(server.server_id());
            if registered {
                cx.emit(LspStoreEvent::LanguageServerUpdate {
                    language_server_id: server.server_id(),
                    name: None,
                });
            }
        }
    }

    fn reuse_existing_language_server<'lang_name>(
        &self,
        server_tree: &LanguageServerTree,
        worktree: &Entity<Worktree>,
        language_name: &'lang_name LanguageName,
        cx: &mut App,
    ) -> Option<(
        Arc<LocalLspAdapterDelegate>,
        impl FnOnce(&mut LanguageServerTree) -> Vec<LanguageServerTreeNode> + use<'lang_name>,
    )> {
        if worktree.read(cx).is_visible() {
            return None;
        }

        let worktree_store = self.worktree_store.read(cx);
        let servers = server_tree
            .instances
            .iter()
            .filter(|(worktree_id, _)| {
                worktree_store
                    .worktree_for_id(**worktree_id, cx)
                    .is_some_and(|worktree| worktree.read(cx).is_visible())
            })
            .flat_map(|(worktree_id, servers)| {
                servers
                    .roots
                    .values()
                    .flatten()
                    .map(move |(_, (server_node, server_languages))| {
                        (worktree_id, server_node, server_languages)
                    })
                    .filter(|(_, _, server_languages)| server_languages.contains(language_name))
                    .map(|(worktree_id, server_node, _)| {
                        (
                            *worktree_id,
                            LanguageServerTreeNode::from(Arc::downgrade(server_node)),
                        )
                    })
            })
            .fold(HashMap::default(), |mut acc, (worktree_id, server_node)| {
                acc.entry(worktree_id)
                    .or_insert_with(Vec::new)
                    .push(server_node);
                acc
            })
            .into_values()
            .max_by_key(|servers| servers.len())?;

        let worktree_id = worktree.read(cx).id();
        let apply = move |tree: &mut LanguageServerTree| {
            for server_node in &servers {
                tree.register_reused(worktree_id, language_name.clone(), server_node.clone());
            }
            servers
        };

        let delegate = LocalLspAdapterDelegate::from_local_lsp(self, worktree, cx);
        Some((delegate, apply))
    }

    pub(crate) fn unregister_old_buffer_from_language_servers(
        &mut self,
        buffer: &Entity<Buffer>,
        old_file: &File,
        cx: &mut App,
    ) {
        let old_path = match old_file.as_local() {
            Some(local) => local.abs_path(cx),
            None => return,
        };

        let Ok(file_url) = lsp::Uri::from_file_path(old_path.as_path()) else {
            return;
        };
        self.unregister_buffer_from_language_servers(buffer, &file_url, cx);
    }

    pub(crate) fn unregister_buffer_from_language_servers(
        &mut self,
        buffer: &Entity<Buffer>,
        file_url: &lsp::Uri,
        cx: &mut App,
    ) {
        buffer.update(cx, |buffer, cx| {
            let mut snapshots = self.buffer_snapshots.remove(&buffer.remote_id());

            for (_, language_server) in self.language_servers_for_buffer(buffer, cx) {
                if snapshots
                    .as_mut()
                    .is_some_and(|map| map.remove(&language_server.server_id()).is_some())
                {
                    language_server.unregister_buffer(file_url.clone());
                }
            }
        });
    }

    fn buffer_snapshot_for_lsp_version(
        &mut self,
        buffer: &Entity<Buffer>,
        server_id: LanguageServerId,
        version: Option<i32>,
        cx: &App,
    ) -> Result<TextBufferSnapshot> {
        const OLD_VERSIONS_TO_RETAIN: i32 = 10;

        if let Some(version) = version {
            let buffer_id = buffer.read(cx).remote_id();
            let snapshots = if let Some(snapshots) = self
                .buffer_snapshots
                .get_mut(&buffer_id)
                .and_then(|m| m.get_mut(&server_id))
            {
                snapshots
            } else if version == 0 {
                // Some language servers report version 0 even if the buffer hasn't been opened yet.
                // We detect this case and treat it as if the version was `None`.
                return Ok(buffer.read(cx).text_snapshot());
            } else {
                anyhow::bail!("no snapshots found for buffer {buffer_id} and server {server_id}");
            };

            let found_snapshot = snapshots
                    .binary_search_by_key(&version, |e| e.version)
                    .map(|ix| snapshots[ix].snapshot.clone())
                    .map_err(|_| {
                        anyhow!("snapshot not found for buffer {buffer_id} server {server_id} at version {version}")
                    })?;

            snapshots.retain(|snapshot| snapshot.version + OLD_VERSIONS_TO_RETAIN >= version);
            Ok(found_snapshot)
        } else {
            Ok((buffer.read(cx)).text_snapshot())
        }
    }

    pub async fn deserialize_text_edits(
        this: Entity<LspStore>,
        buffer_to_edit: Entity<Buffer>,
        edits: Vec<lsp::TextEdit>,
        push_to_history: bool,
        _: Arc<CachedLspAdapter>,
        language_server: Arc<LanguageServer>,
        cx: &mut AsyncApp,
    ) -> Result<Option<Transaction>> {
        let edits = this
            .update(cx, |this, cx| {
                this.as_local_mut().unwrap().edits_from_lsp(
                    &buffer_to_edit,
                    edits,
                    language_server.server_id(),
                    None,
                    cx,
                )
            })
            .await?;

        let transaction = buffer_to_edit.update(cx, |buffer, cx| {
            buffer.finalize_last_transaction();
            buffer.start_transaction();
            for (range, text) in edits {
                buffer.edit([(range, text)], None, cx);
            }

            if buffer.end_transaction(cx).is_some() {
                let transaction = buffer.finalize_last_transaction().unwrap().clone();
                if !push_to_history {
                    buffer.forget_transaction(transaction.id);
                }
                Some(transaction)
            } else {
                None
            }
        });

        Ok(transaction)
    }

    #[allow(clippy::type_complexity)]
    pub fn edits_from_lsp(
        &mut self,
        buffer: &Entity<Buffer>,
        lsp_edits: impl 'static + Send + IntoIterator<Item = lsp::TextEdit>,
        server_id: LanguageServerId,
        version: Option<i32>,
        cx: &mut Context<LspStore>,
    ) -> Task<Result<Vec<(Range<Anchor>, Arc<str>)>>> {
        let snapshot = self.buffer_snapshot_for_lsp_version(buffer, server_id, version, cx);
        cx.background_spawn(async move {
            let snapshot = snapshot?;
            let mut lsp_edits = lsp_edits
                .into_iter()
                .map(|edit| (range_from_lsp(edit.range), edit.new_text))
                .collect::<Vec<_>>();

            lsp_edits.sort_unstable_by_key(|(range, _)| (range.start, range.end));

            let mut lsp_edits = lsp_edits.into_iter().peekable();
            let mut edits = Vec::new();
            while let Some((range, mut new_text)) = lsp_edits.next() {
                // Clip invalid ranges provided by the language server.
                let mut range = snapshot.clip_point_utf16(range.start, Bias::Left)
                    ..snapshot.clip_point_utf16(range.end, Bias::Left);

                // Combine any LSP edits that are adjacent.
                //
                // Also, combine LSP edits that are separated from each other by only
                // a newline. This is important because for some code actions,
                // Rust-analyzer rewrites the entire buffer via a series of edits that
                // are separated by unchanged newline characters.
                //
                // In order for the diffing logic below to work properly, any edits that
                // cancel each other out must be combined into one.
                while let Some((next_range, next_text)) = lsp_edits.peek() {
                    if next_range.start.0 > range.end {
                        if next_range.start.0.row > range.end.row + 1
                            || next_range.start.0.column > 0
                            || snapshot.clip_point_utf16(
                                Unclipped(PointUtf16::new(range.end.row, u32::MAX)),
                                Bias::Left,
                            ) > range.end
                        {
                            break;
                        }
                        new_text.push('\n');
                    }
                    range.end = snapshot.clip_point_utf16(next_range.end, Bias::Left);
                    new_text.push_str(next_text);
                    lsp_edits.next();
                }

                // For multiline edits, perform a diff of the old and new text so that
                // we can identify the changes more precisely, preserving the locations
                // of any anchors positioned in the unchanged regions.
                if range.end.row > range.start.row {
                    let offset = range.start.to_offset(&snapshot);
                    let old_text = snapshot.text_for_range(range).collect::<String>();
                    let range_edits = language::text_diff(old_text.as_str(), &new_text);
                    edits.extend(range_edits.into_iter().map(|(range, replacement)| {
                        (
                            snapshot.anchor_after(offset + range.start)
                                ..snapshot.anchor_before(offset + range.end),
                            replacement,
                        )
                    }));
                } else if range.end == range.start {
                    let anchor = snapshot.anchor_after(range.start);
                    edits.push((anchor..anchor, new_text.into()));
                } else {
                    let edit_start = snapshot.anchor_after(range.start);
                    let edit_end = snapshot.anchor_before(range.end);
                    edits.push((edit_start..edit_end, new_text.into()));
                }
            }

            Ok(edits)
        })
    }

    pub(crate) async fn deserialize_workspace_edit(
        this: Entity<LspStore>,
        edit: lsp::WorkspaceEdit,
        push_to_history: bool,
        language_server: Arc<LanguageServer>,
        cx: &mut AsyncApp,
    ) -> Result<ProjectTransaction> {
        let fs = this.read_with(cx, |this, _| this.as_local().unwrap().fs.clone());

        let mut operations = Vec::new();
        if let Some(document_changes) = edit.document_changes {
            match document_changes {
                lsp::DocumentChanges::Edits(edits) => {
                    operations.extend(edits.into_iter().map(lsp::DocumentChangeOperation::Edit))
                }
                lsp::DocumentChanges::Operations(ops) => operations = ops,
            }
        } else if let Some(changes) = edit.changes {
            operations.extend(changes.into_iter().map(|(uri, edits)| {
                lsp::DocumentChangeOperation::Edit(lsp::TextDocumentEdit {
                    text_document: lsp::OptionalVersionedTextDocumentIdentifier {
                        uri,
                        version: None,
                    },
                    edits: edits.into_iter().map(Edit::Plain).collect(),
                })
            }));
        }

        let mut project_transaction = ProjectTransaction::default();
        for operation in operations {
            match operation {
                lsp::DocumentChangeOperation::Op(lsp::ResourceOp::Create(op)) => {
                    let abs_path = op
                        .uri
                        .to_file_path()
                        .map_err(|()| anyhow!("can't convert URI to path"))?;

                    if let Some(parent_path) = abs_path.parent() {
                        fs.create_dir(parent_path).await?;
                    }
                    if abs_path.ends_with("/") {
                        fs.create_dir(&abs_path).await?;
                    } else {
                        fs.create_file(
                            &abs_path,
                            op.options
                                .map(|options| fs::CreateOptions {
                                    overwrite: options.overwrite.unwrap_or(false),
                                    ignore_if_exists: options.ignore_if_exists.unwrap_or(false),
                                })
                                .unwrap_or_default(),
                        )
                        .await?;
                    }
                }

                lsp::DocumentChangeOperation::Op(lsp::ResourceOp::Rename(op)) => {
                    let source_abs_path = op
                        .old_uri
                        .to_file_path()
                        .map_err(|()| anyhow!("can't convert URI to path"))?;
                    let target_abs_path = op
                        .new_uri
                        .to_file_path()
                        .map_err(|()| anyhow!("can't convert URI to path"))?;

                    let options = fs::RenameOptions {
                        overwrite: op
                            .options
                            .as_ref()
                            .and_then(|options| options.overwrite)
                            .unwrap_or(false),
                        ignore_if_exists: op
                            .options
                            .as_ref()
                            .and_then(|options| options.ignore_if_exists)
                            .unwrap_or(false),
                        create_parents: true,
                    };

                    fs.rename(&source_abs_path, &target_abs_path, options)
                        .await?;
                }

                lsp::DocumentChangeOperation::Op(lsp::ResourceOp::Delete(op)) => {
                    let abs_path = op
                        .uri
                        .to_file_path()
                        .map_err(|()| anyhow!("can't convert URI to path"))?;
                    let options = op
                        .options
                        .map(|options| fs::RemoveOptions {
                            recursive: options.recursive.unwrap_or(false),
                            ignore_if_not_exists: options.ignore_if_not_exists.unwrap_or(false),
                        })
                        .unwrap_or_default();
                    if abs_path.ends_with("/") {
                        fs.remove_dir(&abs_path, options).await?;
                    } else {
                        fs.remove_file(&abs_path, options).await?;
                    }
                }

                lsp::DocumentChangeOperation::Edit(op) => {
                    let buffer_to_edit = this
                        .update(cx, |this, cx| {
                            this.open_local_buffer_via_lsp(
                                op.text_document.uri.clone(),
                                language_server.server_id(),
                                cx,
                            )
                        })
                        .await?;

                    let edits = this
                        .update(cx, |this, cx| {
                            let local = this.as_local_mut().unwrap();

                            let mut edits = vec![];
                            for edit in op.edits {
                                match edit {
                                    Edit::Plain(edit) => {
                                        if !edits.contains(&edit) {
                                            edits.push(edit)
                                        }
                                    }
                                    Edit::Annotated(edit) => {
                                        if !edits.contains(&edit.text_edit) {
                                            edits.push(edit.text_edit)
                                        }
                                    }
                                }
                            }

                            local.edits_from_lsp(
                                &buffer_to_edit,
                                edits,
                                language_server.server_id(),
                                op.text_document.version,
                                cx,
                            )
                        })
                        .await?;

                    let transaction = buffer_to_edit.update(cx, |buffer, cx| {
                        buffer.finalize_last_transaction();
                        buffer.start_transaction();
                        for (range, text) in edits {
                            buffer.edit([(range, text)], None, cx);
                        }

                        buffer.end_transaction(cx).and_then(|transaction_id| {
                            if push_to_history {
                                buffer.finalize_last_transaction();
                                buffer.get_transaction(transaction_id).cloned()
                            } else {
                                buffer.forget_transaction(transaction_id)
                            }
                        })
                    });
                    if let Some(transaction) = transaction {
                        project_transaction.0.insert(buffer_to_edit, transaction);
                    }
                }
            }
        }

        Ok(project_transaction)
    }

    async fn on_lsp_workspace_edit(
        this: WeakEntity<LspStore>,
        params: lsp::ApplyWorkspaceEditParams,
        server_id: LanguageServerId,
        cx: &mut AsyncApp,
    ) -> Result<lsp::ApplyWorkspaceEditResponse> {
        let this = this.upgrade().context("project project closed")?;
        let language_server = this
            .read_with(cx, |this, _| this.language_server_for_id(server_id))
            .context("language server not found")?;
        let transaction = Self::deserialize_workspace_edit(
            this.clone(),
            params.edit,
            true,
            language_server.clone(),
            cx,
        )
        .await
        .log_err();
        this.update(cx, |this, cx| {
            if let Some(transaction) = transaction {
                cx.emit(LspStoreEvent::WorkspaceEditApplied(transaction.clone()));

                this.as_local_mut()
                    .unwrap()
                    .last_workspace_edits_by_language_server
                    .insert(server_id, transaction);
            }
        });
        Ok(lsp::ApplyWorkspaceEditResponse {
            applied: true,
            failed_change: None,
            failure_reason: None,
        })
    }

    fn remove_worktree(
        &mut self,
        id_to_remove: WorktreeId,
        cx: &mut Context<LspStore>,
    ) -> Vec<LanguageServerId> {
        self.restricted_worktrees_tasks.remove(&id_to_remove);

        let mut servers_to_remove = BTreeSet::default();
        let mut servers_to_preserve = HashSet::default();
        for (seed, state) in &self.language_server_ids {
            if seed.worktree_id == id_to_remove {
                servers_to_remove.insert(state.id);
            } else {
                servers_to_preserve.insert(state.id);
            }
        }
        servers_to_remove.retain(|server_id| !servers_to_preserve.contains(server_id));
        self.language_server_ids.retain(|seed, state| {
            seed.worktree_id != id_to_remove && !servers_to_remove.contains(&state.id)
        });
        self.lsp_tree.instances.remove(&id_to_remove);
        for server_id_to_remove in &servers_to_remove {
            self.language_server_watched_paths
                .remove(server_id_to_remove);
            self.language_server_paths_watched_for_rename
                .remove(server_id_to_remove);
            self.last_workspace_edits_by_language_server
                .remove(server_id_to_remove);
            self.language_servers.remove(server_id_to_remove);
            for buffer_servers in self.buffers_opened_in_servers.values_mut() {
                buffer_servers.remove(server_id_to_remove);
            }
            cx.emit(LspStoreEvent::LanguageServerRemoved(*server_id_to_remove));
        }
        servers_to_remove.into_iter().collect()
    }

    fn rebuild_watched_paths_inner<'a>(
        &'a self,
        language_server_id: LanguageServerId,
        watchers: impl Iterator<Item = &'a FileSystemWatcher>,
        cx: &mut Context<LspStore>,
    ) -> LanguageServerWatchedPathsBuilder {
        let worktrees = self
            .worktree_store
            .read(cx)
            .worktrees()
            .filter_map(|worktree| {
                self.language_servers_for_worktree(worktree.read(cx).id())
                    .find(|server| server.server_id() == language_server_id)
                    .map(|_| worktree)
            })
            .collect::<Vec<_>>();

        let mut worktree_globs = HashMap::default();
        let mut abs_globs = HashMap::default();
        log::trace!(
            "Processing new watcher paths for language server with id {}",
            language_server_id
        );

        for watcher in watchers {
            if let Some((worktree, literal_prefix, pattern)) =
                Self::worktree_and_path_for_file_watcher(&worktrees, watcher, cx)
            {
                worktree.update(cx, |worktree, _| {
                    if let Some((tree, glob)) =
                        worktree.as_local_mut().zip(Glob::new(&pattern).log_err())
                    {
                        tree.add_path_prefix_to_scan(literal_prefix);
                        worktree_globs
                            .entry(tree.id())
                            .or_insert_with(GlobSetBuilder::new)
                            .add(glob);
                    }
                });
            } else {
                let (path, pattern) = match &watcher.glob_pattern {
                    lsp::GlobPattern::String(s) => {
                        let watcher_path = SanitizedPath::new(s);
                        let path = glob_literal_prefix(watcher_path.as_path());
                        let pattern = watcher_path
                            .as_path()
                            .strip_prefix(&path)
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|e| {
                                debug_panic!(
                                    "Failed to strip prefix for string pattern: {}, with prefix: {}, with error: {}",
                                    s,
                                    path.display(),
                                    e
                                );
                                watcher_path.as_path().to_string_lossy().into_owned()
                            });
                        (path, pattern)
                    }
                    lsp::GlobPattern::Relative(rp) => {
                        let Ok(mut base_uri) = match &rp.base_uri {
                            lsp::OneOf::Left(workspace_folder) => &workspace_folder.uri,
                            lsp::OneOf::Right(base_uri) => base_uri,
                        }
                        .to_file_path() else {
                            continue;
                        };

                        let path = glob_literal_prefix(Path::new(&rp.pattern));
                        let pattern = Path::new(&rp.pattern)
                            .strip_prefix(&path)
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_else(|e| {
                                debug_panic!(
                                    "Failed to strip prefix for relative pattern: {}, with prefix: {}, with error: {}",
                                    rp.pattern,
                                    path.display(),
                                    e
                                );
                                rp.pattern.clone()
                            });
                        base_uri.push(path);
                        (base_uri, pattern)
                    }
                };

                if let Some(glob) = Glob::new(&pattern).log_err() {
                    if !path
                        .components()
                        .any(|c| matches!(c, path::Component::Normal(_)))
                    {
                        // For an unrooted glob like `**/Cargo.toml`, watch it within each worktree,
                        // rather than adding a new watcher for `/`.
                        for worktree in &worktrees {
                            worktree_globs
                                .entry(worktree.read(cx).id())
                                .or_insert_with(GlobSetBuilder::new)
                                .add(glob.clone());
                        }
                    } else {
                        abs_globs
                            .entry(path.into())
                            .or_insert_with(GlobSetBuilder::new)
                            .add(glob);
                    }
                }
            }
        }

        let mut watch_builder = LanguageServerWatchedPathsBuilder::default();
        for (worktree_id, builder) in worktree_globs {
            if let Ok(globset) = builder.build() {
                watch_builder.watch_worktree(worktree_id, globset);
            }
        }
        for (abs_path, builder) in abs_globs {
            if let Ok(globset) = builder.build() {
                watch_builder.watch_abs_path(abs_path, globset);
            }
        }
        watch_builder
    }

    fn worktree_and_path_for_file_watcher(
        worktrees: &[Entity<Worktree>],
        watcher: &FileSystemWatcher,
        cx: &App,
    ) -> Option<(Entity<Worktree>, Arc<RelPath>, String)> {
        worktrees.iter().find_map(|worktree| {
            let tree = worktree.read(cx);
            let worktree_root_path = tree.abs_path();
            let path_style = tree.path_style();
            match &watcher.glob_pattern {
                lsp::GlobPattern::String(s) => {
                    let watcher_path = SanitizedPath::new(s);
                    let relative = watcher_path
                        .as_path()
                        .strip_prefix(&worktree_root_path)
                        .ok()?;
                    let literal_prefix = glob_literal_prefix(relative);
                    Some((
                        worktree.clone(),
                        RelPath::new(&literal_prefix, path_style).ok()?.into_arc(),
                        relative.to_string_lossy().into_owned(),
                    ))
                }
                lsp::GlobPattern::Relative(rp) => {
                    let base_uri = match &rp.base_uri {
                        lsp::OneOf::Left(workspace_folder) => &workspace_folder.uri,
                        lsp::OneOf::Right(base_uri) => base_uri,
                    }
                    .to_file_path()
                    .ok()?;
                    let relative = base_uri.strip_prefix(&worktree_root_path).ok()?;
                    let mut literal_prefix = relative.to_owned();
                    literal_prefix.push(glob_literal_prefix(Path::new(&rp.pattern)));
                    Some((
                        worktree.clone(),
                        RelPath::new(&literal_prefix, path_style).ok()?.into_arc(),
                        rp.pattern.clone(),
                    ))
                }
            }
        })
    }

    fn rebuild_watched_paths(
        &mut self,
        language_server_id: LanguageServerId,
        cx: &mut Context<LspStore>,
    ) {
        let Some(registrations) = self
            .language_server_dynamic_registrations
            .get(&language_server_id)
        else {
            return;
        };

        let watch_builder = self.rebuild_watched_paths_inner(
            language_server_id,
            registrations.did_change_watched_files.values().flatten(),
            cx,
        );
        let watcher = watch_builder.build(self.fs.clone(), language_server_id, cx);
        self.language_server_watched_paths
            .insert(language_server_id, watcher);

        cx.notify();
    }

    fn on_lsp_did_change_watched_files(
        &mut self,
        language_server_id: LanguageServerId,
        registration_id: &str,
        params: DidChangeWatchedFilesRegistrationOptions,
        cx: &mut Context<LspStore>,
    ) {
        let registrations = self
            .language_server_dynamic_registrations
            .entry(language_server_id)
            .or_default();

        registrations
            .did_change_watched_files
            .insert(registration_id.to_string(), params.watchers);

        self.rebuild_watched_paths(language_server_id, cx);
    }

    fn on_lsp_unregister_did_change_watched_files(
        &mut self,
        language_server_id: LanguageServerId,
        registration_id: &str,
        cx: &mut Context<LspStore>,
    ) {
        let registrations = self
            .language_server_dynamic_registrations
            .entry(language_server_id)
            .or_default();

        if registrations
            .did_change_watched_files
            .remove(registration_id)
            .is_some()
        {
            log::info!(
                "language server {}: unregistered workspace/DidChangeWatchedFiles capability with id {}",
                language_server_id,
                registration_id
            );
        } else {
            log::warn!(
                "language server {}: failed to unregister workspace/DidChangeWatchedFiles capability with id {}. not registered.",
                language_server_id,
                registration_id
            );
        }

        self.rebuild_watched_paths(language_server_id, cx);
    }

    async fn initialization_options_for_adapter(
        adapter: Arc<dyn LspAdapter>,
        delegate: &Arc<dyn LspAdapterDelegate>,
        cx: &mut AsyncApp,
    ) -> Result<Option<serde_json::Value>> {
        let Some(mut initialization_config) =
            adapter.clone().initialization_options(delegate, cx).await?
        else {
            return Ok(None);
        };

        for other_adapter in delegate.registered_lsp_adapters() {
            if other_adapter.name() == adapter.name() {
                continue;
            }
            if let Ok(Some(target_config)) = other_adapter
                .clone()
                .additional_initialization_options(adapter.name(), delegate)
                .await
            {
                merge_json_value_into(target_config.clone(), &mut initialization_config);
            }
        }

        Ok(Some(initialization_config))
    }

    async fn workspace_configuration_for_adapter(
        adapter: Arc<dyn LspAdapter>,
        delegate: &Arc<dyn LspAdapterDelegate>,
        toolchain: Option<Toolchain>,
        requested_uri: Option<Uri>,
        cx: &mut AsyncApp,
    ) -> Result<serde_json::Value> {
        let mut workspace_config = adapter
            .clone()
            .workspace_configuration(delegate, toolchain, requested_uri, cx)
            .await?;

        for other_adapter in delegate.registered_lsp_adapters() {
            if other_adapter.name() == adapter.name() {
                continue;
            }
            if let Ok(Some(target_config)) = other_adapter
                .clone()
                .additional_workspace_configuration(adapter.name(), delegate, cx)
                .await
            {
                merge_json_value_into(target_config.clone(), &mut workspace_config);
            }
        }

        Ok(workspace_config)
    }

    fn language_server_for_id(&self, id: LanguageServerId) -> Option<Arc<LanguageServer>> {
        if let Some(LanguageServerState::Running { server, .. }) = self.language_servers.get(&id) {
            Some(server.clone())
        } else if let Some((_, server)) = self.supplementary_language_servers.get(&id) {
            Some(Arc::clone(server))
        } else {
            None
        }
    }
}

fn notify_server_capabilities_updated(server: &LanguageServer, cx: &mut Context<LspStore>) {
    if let Some(_capabilities) = serde_json::to_string(&server.capabilities()).ok() {
        cx.emit(LspStoreEvent::LanguageServerUpdate {
            language_server_id: server.server_id(),
            name: Some(server.name()),
        });
    }
}

pub(crate) enum LspStoreMode {
    Local(LocalLspStore),   // ssh host and collab host
}

pub struct LspStore {
    mode: LspStoreMode,
    downstream_client: Option<(AnyProtoClient, u64)>,
    buffer_store: Entity<BufferStore>,
    worktree_store: Entity<WorktreeStore>,
    pub languages: Arc<LanguageRegistry>,
    pub language_server_statuses: BTreeMap<LanguageServerId, LanguageServerStatus>,
    active_entry: Option<ProjectEntryId>,
    _maintain_workspace_config: (Task<Result<()>>, watch::Sender<()>),
    _maintain_buffer_languages: Task<()>,
    pub lsp_server_capabilities: HashMap<LanguageServerId, lsp::ServerCapabilities>,
    semantic_token_config: SemanticTokenConfig,
    lsp_data: HashMap<BufferId, BufferLspData>,
    buffer_reload_tasks: HashMap<BufferId, Task<anyhow::Result<()>>>,
    default_tab_settings: Option<(NonZeroU32, bool)>, // (default_tab_size, default_hard_tabs)
}

#[derive(Debug)]
pub struct BufferLspData {
    buffer_version: Global,
    document_colors: Option<DocumentColorData>,
    semantic_tokens: Option<SemanticTokensData>,
    folding_ranges: Option<FoldingRangeData>,
    document_symbols: Option<DocumentSymbolsData>,
}

impl BufferLspData {
    fn new(buffer: &Entity<Buffer>, cx: &mut App) -> Self {
        Self {
            buffer_version: buffer.read(cx).version(),
            document_colors: None,
            semantic_tokens: None,
            folding_ranges: None,
            document_symbols: None,
        }
    }

    fn remove_server_data(&mut self, for_server: LanguageServerId) {
        if let Some(document_colors) = &mut self.document_colors {
            document_colors.remove_server_data(for_server);
        }

        if let Some(semantic_tokens) = &mut self.semantic_tokens {
            semantic_tokens.remove_server_data(for_server);
        }

        if let Some(folding_ranges) = &mut self.folding_ranges {
            folding_ranges.ranges.remove(&for_server);
        }

        if let Some(document_symbols) = &mut self.document_symbols {
            document_symbols.remove_server_data(for_server);
        }
    }
}

#[derive(Debug)]
pub enum LspStoreEvent {
    LanguageServerAdded(LanguageServerId, LanguageServerName, Option<WorktreeId>),
    LanguageServerRemoved(LanguageServerId),
    LanguageServerUpdate {
        language_server_id: LanguageServerId,
        name: Option<LanguageServerName>,
    },
    LanguageServerLog(LanguageServerId, LanguageServerLogType, String),
    LanguageServerPrompt(LanguageServerPromptRequest),
    LanguageDetected {
        buffer: Entity<Buffer>,
        new_language: Option<Arc<Language>>,
    },
    Notification(String),
    RefreshSemanticTokens {
        server_id: LanguageServerId,
        request_id: Option<usize>,
    },
    WorkspaceEditApplied(ProjectTransaction),
}

#[derive(Clone, Debug, Serialize)]
pub struct LanguageServerStatus {
    pub name: LanguageServerName,
    pub language_name: Option<LanguageName>,
    pub server_version: Option<SharedString>,
    pub server_readable_version: Option<SharedString>,
    pub pending_work: BTreeMap<ProgressToken, LanguageServerProgress>,
    pub progress_tokens: HashSet<ProgressToken>,
    pub worktree: Option<WorktreeId>,
    pub binary: Option<LanguageServerBinary>,
    pub configuration: Option<Value>,
    pub workspace_folders: BTreeSet<Uri>,
    pub process_id: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SymbolLocation {
    InProject(ProjectPath),
    OutsideProject {
        abs_path: Arc<Path>,
        signature: [u8; 32],
    },
}

fn should_log_lsp_request_failure(message: &str) -> bool {
    // content modified is a weird failure mode of rust-analyzer
    // where requests are denied before its loaded a project
    message.ends_with("content modified") || message.ends_with("server cancelled the request")
}

impl LspStore {
    pub fn init(_client: &AnyProtoClient) {
    }

    pub fn as_local(&self) -> Option<&LocalLspStore> {
        match &self.mode {
            LspStoreMode::Local(local_lsp_store) => Some(local_lsp_store),
        }
    }

    pub fn as_local_mut(&mut self) -> Option<&mut LocalLspStore> {
        match &mut self.mode {
            LspStoreMode::Local(local_lsp_store) => Some(local_lsp_store),
        }
    }

    pub fn new_local(
        buffer_store: Entity<BufferStore>,
        worktree_store: Entity<WorktreeStore>,
        toolchain_store: Entity<LocalToolchainStore>,
        environment: Entity<ProjectEnvironment>,
        manifest_tree: Entity<ManifestTree>,
        languages: Arc<LanguageRegistry>,
        http_client: Arc<dyn HttpClient>,
        fs: Arc<dyn Fs>,
        cx: &mut Context<Self>,
    ) -> Self {
        let yarn = YarnPathStore::new(fs.clone(), cx);
        cx.subscribe(&buffer_store, Self::on_buffer_store_event)
            .detach();
        cx.subscribe(&worktree_store, Self::on_worktree_store_event)
            .detach();
        cx.subscribe(&toolchain_store, Self::on_toolchain_store_event)
            .detach();
        cx.observe_global::<SettingsStore>(Self::on_settings_changed)
            .detach();
        subscribe_to_binary_statuses(&languages, cx).detach();

        let _maintain_workspace_config = {
            let (sender, receiver) = watch::channel();
            (Self::maintain_workspace_config(receiver, cx), sender)
        };

        Self {
            mode: LspStoreMode::Local(LocalLspStore {
                weak: cx.weak_entity(),
                worktree_store: worktree_store.clone(),

                supplementary_language_servers: Default::default(),
                languages: languages.clone(),
                language_server_ids: Default::default(),
                language_servers: Default::default(),
                last_workspace_edits_by_language_server: Default::default(),
                language_server_watched_paths: Default::default(),
                language_server_paths_watched_for_rename: Default::default(),
                language_server_dynamic_registrations: Default::default(),
                buffer_snapshots: Default::default(),
                environment,
                http_client,
                fs,
                yarn,
                _subscription: cx.on_app_quit(|this, _| {
                    this.as_local_mut()
                        .unwrap()
                        .shutdown_language_servers_on_quit()
                }),
                lsp_tree: LanguageServerTree::new(
                    manifest_tree,
                    languages.clone(),
                    toolchain_store.clone(),
                ),
                toolchain_store,
                registered_buffers: HashMap::default(),
                buffers_opened_in_servers: HashMap::default(),
                restricted_worktrees_tasks: HashMap::default(),
                watched_manifest_filenames: ManifestProvidersStore::global(cx)
                    .manifest_file_names(),
            }),
            downstream_client: None,
            buffer_store,
            worktree_store,
            languages: languages.clone(),
            language_server_statuses: Default::default(),
            lsp_server_capabilities: HashMap::default(),
            semantic_token_config: SemanticTokenConfig::new(cx),
            lsp_data: HashMap::default(),
            buffer_reload_tasks: HashMap::default(),
            active_entry: None,
            default_tab_settings: None,
            _maintain_workspace_config,
            _maintain_buffer_languages: Self::maintain_buffer_languages(languages, cx),
        }
    }

    fn on_buffer_store_event(
        &mut self,
        _: Entity<BufferStore>,
        event: &BufferStoreEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            BufferStoreEvent::BufferAdded(buffer) => {
                self.on_buffer_added(buffer, cx).log_err();
            }
            BufferStoreEvent::BufferChangedFilePath { buffer, old_file } => {
                let buffer_id = buffer.read(cx).remote_id();
                if let Some(local) = self.as_local_mut()
                    && let Some(old_file) = File::from_dyn(old_file.as_ref())
                {
                    local.reset_buffer(buffer, old_file, cx);

                    if local.registered_buffers.contains_key(&buffer_id) {
                        local.unregister_old_buffer_from_language_servers(buffer, old_file, cx);
                    }
                }

                self.detect_language_for_buffer(buffer, cx);
                if let Some(local) = self.as_local_mut() {
                    local.initialize_buffer(buffer, cx);
                    if local.registered_buffers.contains_key(&buffer_id) {
                        local.register_buffer_with_language_servers(buffer, HashSet::default(), cx);
                    }
                }
            }
            _ => {}
        }
    }

    fn on_worktree_store_event(
        &mut self,
        _: Entity<WorktreeStore>,
        event: &WorktreeStoreEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorktreeStoreEvent::WorktreeAdded(worktree) => {
                if !worktree.read(cx).is_local() {
                    return;
                }
                cx.subscribe(worktree, |this, worktree, event, cx| match event {
                    worktree::Event::UpdatedEntries(changes) => {
                        this.update_local_worktree_language_servers(&worktree, changes, cx);
                    }
                    worktree::Event::UpdatedGitRepositories(_)
                    | worktree::Event::DeletedEntry(_)
                    | worktree::Event::Deleted
                    | worktree::Event::UpdatedRootRepoCommonDir { .. } => {}
                })
                .detach()
            }
            WorktreeStoreEvent::WorktreeRemoved(_, id) => self.remove_worktree(*id, cx),
            WorktreeStoreEvent::WorktreeReleased(..)
            | WorktreeStoreEvent::WorktreeOrderChanged
            | WorktreeStoreEvent::WorktreeUpdatedGitRepositories(..)
            | WorktreeStoreEvent::WorktreeDeletedEntry(..)
            | WorktreeStoreEvent::WorktreeUpdatedRootRepoCommonDir(..)
            | WorktreeStoreEvent::WorktreeUpdateSent(..) 
            | WorktreeStoreEvent::WorktreeUpdatedEntries(..) => {}
        }
    }

    fn on_toolchain_store_event(
        &mut self,
        _: Entity<LocalToolchainStore>,
        event: &ToolchainStoreEvent,
        _: &mut Context<Self>,
    ) {
        if let ToolchainStoreEvent::ToolchainActivated = event {
            self.request_workspace_config_refresh()
        }
    }

    fn request_workspace_config_refresh(&mut self) {
        *self._maintain_workspace_config.1.borrow_mut() = ();
    }

    fn on_buffer_event(
        &mut self,
        buffer: Entity<Buffer>,
        event: &language::BufferEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            language::BufferEvent::Edited { .. } => {
                self.on_buffer_edited(buffer, cx);
            }

            language::BufferEvent::Saved => {
                self.on_buffer_saved(buffer, cx);
            }

            language::BufferEvent::Reloaded => {
                self.on_buffer_reloaded(buffer, cx);
            }

            _ => {}
        }
    }

    fn on_buffer_added(&mut self, buffer: &Entity<Buffer>, cx: &mut Context<Self>) -> Result<()> {
        buffer
            .read(cx)
            .set_language_registry(self.languages.clone());

        cx.subscribe(buffer, |this, buffer, event, cx| {
            this.on_buffer_event(buffer, event, cx);
        })
        .detach();

        self.parse_modeline(buffer, cx);
        self.detect_language_for_buffer(buffer, cx);
        if let Some((default_tab_size, default_hard_tabs)) = self.default_tab_settings {
            Self::detect_tab_settings(&buffer, default_tab_size, default_hard_tabs, cx);
        }
        if let Some(local) = self.as_local_mut() {
            local.initialize_buffer(buffer, cx);
        }

        Ok(())
    }

    fn on_buffer_reloaded(&mut self, buffer: Entity<Buffer>, cx: &mut Context<Self>) {
        if self.parse_modeline(&buffer, cx) {
            self.detect_language_for_buffer(&buffer, cx);
            if let Some((default_tab_size, default_hard_tabs)) = self.default_tab_settings {
                Self::detect_tab_settings(&buffer, default_tab_size, default_hard_tabs, cx);
            }
        }
    }

    fn detect_tab_settings(
        buffer: &Entity<Buffer>,
        default_tab_size: NonZeroU32,
        default_hard_tabs: bool,
        cx: &mut App,
    ) {
        buffer.update(cx, |buffer, _cx| {
            buffer.detect_tab_settings(default_tab_size, default_hard_tabs);
        });
    }

    pub fn on_update_default_tab_settings(
        &mut self,
        default_tab_size: NonZeroU32,
        default_hard_tabs: bool,
        cx: &mut App
    ) {
        let was_none = self.default_tab_settings.is_none();
        self.default_tab_settings = Some((default_tab_size, default_hard_tabs));
        if was_none {
            // if this is our first load of the settings, then re-detect open buffers
            self.buffer_store.update(cx, |buffer_store, cx| {
                for buffer in buffer_store.buffers() {
                    Self::detect_tab_settings(&buffer, default_tab_size, default_hard_tabs, cx);
                }
            });
        }
    }

    pub(crate) fn register_buffer_with_language_servers(
        &mut self,
        buffer: &Entity<Buffer>,
        only_register_servers: HashSet<LanguageServerSelector>,
        ignore_refcounts: bool,
        cx: &mut Context<Self>,
    ) -> OpenLspBufferHandle {
        let buffer_id = buffer.read(cx).remote_id();
        let handle = OpenLspBufferHandle(cx.new(|_| OpenLspBuffer(buffer.clone())));
        if let Some(local) = self.as_local_mut() {
            let refcount = local.registered_buffers.entry(buffer_id).or_insert(0);
            if !ignore_refcounts {
                *refcount += 1;
            }

            // We run early exits on non-existing buffers AFTER we mark the buffer as registered in order to handle buffer saving.
            // When a new unnamed buffer is created and saved, we will start loading it's language. Once the language is loaded, we go over all "language-less" buffers and try to fit that new language
            // with them. However, we do that only for the buffers that we think are open in at least one editor; thus, we need to keep tab of unnamed buffers as well, even though they're not actually registered with any language
            // servers in practice (we don't support non-file URI schemes in our LSP impl).
            let Some(file) = File::from_dyn(buffer.read(cx).file()) else {
                return handle;
            };
            if !file.is_local() {
                return handle;
            }

            if ignore_refcounts || *refcount == 1 {
                local.register_buffer_with_language_servers(buffer, only_register_servers, cx);
            }
            if !ignore_refcounts {
                cx.observe_release(&handle.0, move |lsp_store, buffer, cx| {
                    let refcount = {
                        let local = lsp_store.as_local_mut().unwrap();
                        let Some(refcount) = local.registered_buffers.get_mut(&buffer_id) else {
                            debug_panic!("bad refcounting");
                            return;
                        };

                        *refcount -= 1;
                        *refcount
                    };
                    if refcount == 0 {
                        lsp_store.lsp_data.remove(&buffer_id);
                        lsp_store.buffer_reload_tasks.remove(&buffer_id);
                        let local = lsp_store.as_local_mut().unwrap();
                        local.registered_buffers.remove(&buffer_id);

                        local.buffers_opened_in_servers.remove(&buffer_id);
                        if let Some(file) = File::from_dyn(buffer.0.read(cx).file()).cloned() {
                            local.unregister_old_buffer_from_language_servers(&buffer.0, &file, cx);
                        }
                    }
                })
                .detach();
            }
        } else {
            // Our remote connection got closed
        }
        handle
    }

    fn maintain_buffer_languages(
        languages: Arc<LanguageRegistry>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let mut subscription = languages.subscribe();
        let mut prev_reload_count = languages.reload_count();
        cx.spawn(async move |this, cx| {
            while let Some(()) = subscription.next().await {
                if let Some(this) = this.upgrade() {
                    // If the language registry has been reloaded, then remove and
                    // re-assign the languages on all open buffers.
                    let reload_count = languages.reload_count();
                    if reload_count > prev_reload_count {
                        prev_reload_count = reload_count;
                        this.update(cx, |this, cx| {
                            this.buffer_store.clone().update(cx, |buffer_store, cx| {
                                for buffer in buffer_store.buffers() {
                                    if let Some(f) = File::from_dyn(buffer.read(cx).file()).cloned()
                                    {
                                        buffer.update(cx, |buffer, cx| {
                                            buffer.set_language_async(None, cx)
                                        });
                                        if let Some(local) = this.as_local_mut() {
                                            local.reset_buffer(&buffer, &f, cx);

                                            if local
                                                .registered_buffers
                                                .contains_key(&buffer.read(cx).remote_id())
                                                && let Some(file_url) =
                                                    file_path_to_lsp_url(&f.abs_path(cx)).log_err()
                                            {
                                                local.unregister_buffer_from_language_servers(
                                                    &buffer, &file_url, cx,
                                                );
                                            }
                                        }
                                    }
                                }
                            });
                        });
                    }

                    this.update(cx, |this, cx| {
                        let mut plain_text_buffers = Vec::new();
                        let mut buffers_with_language = Vec::new();
                        let mut buffers_with_unknown_injections = Vec::new();
                        for handle in this.buffer_store.read(cx).buffers() {
                            let buffer = handle.read(cx);
                            if buffer.language().is_none()
                                || buffer.language() == Some(&*language::PLAIN_TEXT)
                            {
                                plain_text_buffers.push(handle);
                            } else {
                                if buffer.contains_unknown_injections() {
                                    buffers_with_unknown_injections.push(handle.clone());
                                }
                                buffers_with_language.push(handle);
                            }
                        }

                        // Deprioritize the invisible worktrees so main worktrees' language servers can be started first,
                        // and reused later in the invisible worktrees.
                        plain_text_buffers.sort_by_key(|buffer| {
                            Reverse(
                                File::from_dyn(buffer.read(cx).file())
                                    .map(|file| file.worktree.read(cx).is_visible()),
                            )
                        });

                        for buffer in plain_text_buffers {
                            this.detect_language_for_buffer(&buffer, cx);
                            if let Some(local) = this.as_local_mut() {
                                local.initialize_buffer(&buffer, cx);
                                if local
                                    .registered_buffers
                                    .contains_key(&buffer.read(cx).remote_id())
                                {
                                    local.register_buffer_with_language_servers(
                                        &buffer,
                                        HashSet::default(),
                                        cx,
                                    );
                                }
                            }
                        }

                        // Also register buffers that already have a language with
                        // any newly-available language servers (e.g., from extensions
                        // that finished loading after buffers were restored).
                        if let Some(local) = this.as_local_mut() {
                            for buffer in buffers_with_language {
                                if local
                                    .registered_buffers
                                    .contains_key(&buffer.read(cx).remote_id())
                                {
                                    local.register_buffer_with_language_servers(
                                        &buffer,
                                        HashSet::default(),
                                        cx,
                                    );
                                }
                            }
                        }

                        for buffer in buffers_with_unknown_injections {
                            buffer.update(cx, |buffer, cx| buffer.reparse(cx, false));
                        }
                    });
                }
            }
        })
    }

    fn parse_modeline(&mut self, buffer_handle: &Entity<Buffer>, cx: &mut Context<Self>) -> bool {
        let buffer = buffer_handle.read(cx);
        let content = buffer.as_rope();

        let modeline_settings = {
            let settings_store = cx.global::<SettingsStore>();
            let modeline_lines = settings_store
                .raw_user_settings()
                .and_then(|s| s.content.modeline_lines)
                .or(settings_store.raw_default_settings().modeline_lines)
                .unwrap_or(5);

            const MAX_MODELINE_BYTES: usize = 1024;

            let first_bytes =
                content.clip_offset(content.len().min(MAX_MODELINE_BYTES), Bias::Left);
            let mut first_lines = Vec::new();
            let mut lines = content.chunks_in_range(0..first_bytes).lines();
            for _ in 0..modeline_lines {
                if let Some(line) = lines.next() {
                    first_lines.push(line.to_string());
                } else {
                    break;
                }
            }
            let first_lines_ref: Vec<_> = first_lines.iter().map(|line| line.as_str()).collect();

            let last_start =
                content.clip_offset(content.len().saturating_sub(MAX_MODELINE_BYTES), Bias::Left);
            let mut last_lines = Vec::new();
            let mut lines = content
                .reversed_chunks_in_range(last_start..content.len())
                .lines();
            for _ in 0..modeline_lines {
                if let Some(line) = lines.next() {
                    last_lines.push(line.to_string());
                } else {
                    break;
                }
            }
            let last_lines_ref: Vec<_> =
                last_lines.iter().rev().map(|line| line.as_str()).collect();
            modeline::parse_modeline(&first_lines_ref, &last_lines_ref)
        };

        log::debug!("Parsed modeline settings: {:?}", modeline_settings);

        buffer_handle.update(cx, |buffer, _cx| buffer.set_modeline(modeline_settings))
    }

    fn detect_language_for_buffer(
        &mut self,
        buffer_handle: &Entity<Buffer>,
        cx: &mut Context<Self>,
    ) -> Option<language::AvailableLanguage> {
        // If the buffer has a language, set it and start the language server if we haven't already.
        let buffer = buffer_handle.read(cx);
        let file = buffer.file()?;
        let content = buffer.as_rope();
        let modeline_settings = buffer.modeline().map(Arc::as_ref);

        let available_language = if let Some(ModelineSettings {
            mode: Some(mode_name),
            ..
        }) = modeline_settings
        {
            self.languages
                .available_language_for_modeline_name(mode_name)
        } else {
            self.languages.language_for_file(file, Some(content), cx)
        };
        if let Some(available_language) = &available_language {
            if let Some(Ok(Ok(new_language))) = self
                .languages
                .load_language(available_language)
                .now_or_never()
            {
                self.set_language_for_buffer(buffer_handle, new_language, cx);
            }
        } else {
            cx.emit(LspStoreEvent::LanguageDetected {
                buffer: buffer_handle.clone(),
                new_language: None,
            });
        }

        available_language
    }

    pub(crate) fn set_language_for_buffer(
        &mut self,
        buffer_entity: &Entity<Buffer>,
        new_language: Arc<Language>,
        cx: &mut Context<Self>,
    ) {
        let buffer = buffer_entity.read(cx);
        let buffer_file = buffer.file().cloned();
        let buffer_id = buffer.remote_id();
        if let Some(local_store) = self.as_local_mut()
            && local_store.registered_buffers.contains_key(&buffer_id)
            && let Some(abs_path) =
                File::from_dyn(buffer_file.as_ref()).map(|file| file.abs_path(cx))
            && let Some(file_url) = file_path_to_lsp_url(&abs_path).log_err()
        {
            local_store.unregister_buffer_from_language_servers(buffer_entity, &file_url, cx);
        }
        buffer_entity.update(cx, |buffer, cx| {
            if buffer
                .language()
                .is_none_or(|old_language| !Arc::ptr_eq(old_language, &new_language))
            {
                buffer.set_language_async(Some(new_language.clone()), cx);
            }
        });

        cx.emit(LspStoreEvent::LanguageDetected {
            buffer: buffer_entity.clone(),
            new_language: Some(new_language),
        })
    }

    pub fn buffer_store(&self) -> Entity<BufferStore> {
        self.buffer_store.clone()
    }

    pub fn set_active_entry(&mut self, active_entry: Option<ProjectEntryId>) {
        self.active_entry = active_entry;
    }

    pub fn request_lsp<R>(
        &mut self,
        buffer: Entity<Buffer>,
        server: LanguageServerToQuery,
        request: R,
        cx: &mut Context<Self>,
    ) -> Task<Result<R::Response>>
    where
        R: LspCommand,
        <R::LspRequest as lsp::request::Request>::Result: Send,
        <R::LspRequest as lsp::request::Request>::Params: Send,
    {
        let Some(language_server) = buffer.update(cx, |buffer, cx| match server {
            LanguageServerToQuery::FirstCapable => self.as_local().and_then(|local| {
                local
                    .language_servers_for_buffer(buffer, cx)
                    .find(|(_, server)| {
                        request.check_capabilities(server.adapter_server_capabilities())
                    })
                    .map(|(_, server)| server.clone())
            }),
            LanguageServerToQuery::Other(id) => self
                .language_server_for_local_buffer(buffer, id, cx)
                .and_then(|(_, server)| {
                    request
                        .check_capabilities(server.adapter_server_capabilities())
                        .then(|| Arc::clone(server))
                }),
        }) else {
            return Task::ready(Ok(Default::default()));
        };

        let file = File::from_dyn(buffer.read(cx).file()).and_then(File::as_local);

        let Some(file) = file else {
            return Task::ready(Ok(Default::default()));
        };

        let lsp_params = match request.to_lsp_params_or_response(
            &file.abs_path(cx),
            buffer.read(cx),
            &language_server,
            cx,
        ) {
            Ok(LspParamsOrResponse::Params(lsp_params)) => lsp_params,
            Ok(LspParamsOrResponse::Response(response)) => return Task::ready(Ok(response)),
            Err(err) => {
                let message = format!(
                    "{} via {} failed: {}",
                    request.display_name(),
                    language_server.name(),
                    err
                );
                if should_log_lsp_request_failure(&message) {
                    log::warn!("{message}");
                }
                return Task::ready(Err(anyhow!(message)));
            }
        };

        let status = request.status();
        let request_timeout = ProjectSettings::get_global(cx)
            .global_lsp_settings
            .get_request_timeout();

        cx.spawn(async move |this, cx| {
            let lsp_request = language_server.request::<R::LspRequest>(lsp_params, request_timeout);

            let id = lsp_request.id();
            let _cleanup = if status.is_some() {
                cx.update(|cx| {
                    this.update(cx, |this, cx| {
                        this.on_lsp_work_start(
                            language_server.server_id(),
                            ProgressToken::Number(id),
                            LanguageServerProgress {
                                is_cancellable: false,
                                title: None,
                                message: status.clone(),
                                percentage: None,
                                last_update_at: cx.background_executor().now(),
                            },
                            cx,
                        );
                    })
                })
                .log_err();

                Some(defer(|| {
                    cx.update(|cx| {
                        this.update(cx, |this, cx| {
                            this.on_lsp_work_end(
                                language_server.server_id(),
                                ProgressToken::Number(id),
                                cx,
                            );
                        })
                    })
                    .log_err();
                }))
            } else {
                None
            };

            let result = lsp_request.await.into_response();

            let response = result.map_err(|err| {
                let message = format!(
                    "{} via {} failed: {}",
                    request.display_name(),
                    language_server.name(),
                    err
                );
                if should_log_lsp_request_failure(&message) {
                    log::warn!("{message}");
                }
                anyhow::anyhow!(message)
            })?;

            request
                .response_from_lsp(
                    response,
                    this.upgrade().context("no app context")?,
                    buffer,
                    language_server.server_id(),
                    cx.clone(),
                )
                .await
        })
    }

    fn on_settings_changed(&mut self, cx: &mut Context<Self>) {
        let mut language_formatters_to_check = Vec::new();
        for buffer in self.buffer_store.read(cx).buffers() {
            let buffer = buffer.read(cx);
            let settings = LanguageSettings::for_buffer(buffer, cx);
            if buffer.language().is_some() {
                let buffer_file = File::from_dyn(buffer.file());
                language_formatters_to_check.push((
                    buffer_file.map(|f| f.worktree_id(cx)),
                    settings.into_owned(),
                ));
            }
        }

        self.request_workspace_config_refresh();

        let new_semantic_token_rules = crate::project_settings::ProjectSettings::get_global(cx)
            .global_lsp_settings
            .semantic_token_rules
            .clone();
        self.semantic_token_config
            .update_rules(new_semantic_token_rules);
        // Always clear cached stylizers so that changes to language-specific
        // semantic token rules (e.g. from extension install/uninstall) are
        // picked up. Stylizers are recreated lazily, so this is cheap.
        self.semantic_token_config.clear_stylizers();

        let new_global_semantic_tokens_mode =
            all_language_settings(None, cx).defaults.semantic_tokens;
        if self
            .semantic_token_config
            .update_global_mode(new_global_semantic_tokens_mode)
        {
            self.restart_all_language_servers(cx);
        }

        cx.notify();
    }

    fn refresh_server_tree(&mut self, cx: &mut Context<Self>) {
        let buffer_store = self.buffer_store.clone();
        let Some(local) = self.as_local_mut() else {
            return;
        };
        let mut adapters = BTreeMap::default();
        let get_adapter = {
            let languages = local.languages.clone();
            let environment = local.environment.clone();
            let weak = local.weak.clone();
            let worktree_store = local.worktree_store.clone();
            let http_client = local.http_client.clone();
            let fs = local.fs.clone();
            move |worktree_id, cx: &mut App| {
                let worktree = worktree_store.read(cx).worktree_for_id(worktree_id, cx)?;
                Some(LocalLspAdapterDelegate::new(
                    languages.clone(),
                    &environment,
                    weak.clone(),
                    &worktree,
                    http_client.clone(),
                    fs.clone(),
                    cx,
                ))
            }
        };

        let mut messages_to_report = Vec::new();
        let (new_tree, to_stop) = {
            let mut rebase = local.lsp_tree.rebase();
            let buffers = buffer_store
                .read(cx)
                .buffers()
                .filter_map(|buffer| {
                    let raw_buffer = buffer.read(cx);
                    if !local
                        .registered_buffers
                        .contains_key(&raw_buffer.remote_id())
                    {
                        return None;
                    }
                    let file = File::from_dyn(raw_buffer.file()).cloned()?;
                    let language = raw_buffer.language().cloned()?;
                    Some((file, language, raw_buffer.remote_id()))
                })
                .sorted_by_key(|(file, _, _)| Reverse(file.worktree.read(cx).is_visible()));
            for (file, language, _buffer_id) in buffers {
                let worktree_id = file.worktree_id(cx);
                let Some(worktree) = local
                    .worktree_store
                    .read(cx)
                    .worktree_for_id(worktree_id, cx)
                else {
                    continue;
                };

                if let Some((_, apply)) = local.reuse_existing_language_server(
                    rebase.server_tree(),
                    &worktree,
                    &language.name(),
                    cx,
                ) {
                    (apply)(rebase.server_tree());
                } else if let Some(lsp_delegate) = adapters
                    .entry(worktree_id)
                    .or_insert_with(|| get_adapter(worktree_id, cx))
                    .clone()
                {
                    let delegate =
                        Arc::new(ManifestQueryDelegate::new(worktree.read(cx).snapshot()));
                    let path = file
                        .path()
                        .parent()
                        .map(Arc::from)
                        .unwrap_or_else(|| file.path().clone());
                    let worktree_path = ProjectPath { worktree_id, path };
                    let _abs_path = file.abs_path(cx);
                    let nodes = rebase
                        .walk(
                            worktree_path,
                            language.name(),
                            language.manifest(),
                            delegate.clone(),
                            cx,
                        )
                        .collect::<Vec<_>>();
                    for node in nodes {
                        let server_id = node.server_id_or_init(|disposition| {
                            let path = &disposition.path;
                            let uri = Uri::from_file_path(worktree.read(cx).absolutize(&path.path));
                            let key = LanguageServerSeed {
                                worktree_id,
                                name: disposition.server_name.clone(),
                                settings: LanguageServerSeedSettings {
                                    binary: disposition.settings.binary.clone(),
                                    initialization_options: disposition
                                        .settings
                                        .initialization_options
                                        .clone(),
                                },
                                toolchain: local.toolchain_store.read(cx).active_toolchain(
                                    path.worktree_id,
                                    &path.path,
                                    language.name(),
                                ),
                            };
                            local.language_server_ids.remove(&key);

                            let server_id = local.get_or_insert_language_server(
                                &worktree,
                                lsp_delegate.clone(),
                                disposition,
                                &language.name(),
                                cx,
                            );
                            if let Some(state) = local.language_servers.get(&server_id)
                                && let Ok(uri) = uri
                            {
                                state.add_workspace_folder(uri);
                            };
                            server_id
                        });

                        if let Some(language_server_id) = server_id {
                            messages_to_report.push(LspStoreEvent::LanguageServerUpdate {
                                language_server_id,
                                name: node.name(),
                            });
                        }
                    }
                } else {
                    continue;
                }
            }
            rebase.finish()
        };
        for message in messages_to_report {
            cx.emit(message);
        }
        local.lsp_tree = new_tree;
        for (id, _) in to_stop {
            self.stop_local_language_server(id, cx).detach();
        }
    }

    pub fn hover(
        &mut self,
        buffer: &Entity<Buffer>,
        position: PointUtf16,
        cx: &mut Context<Self>,
    ) -> Task<Option<Vec<Hover>>> {
        let all_actions_task = self.request_multiple_lsp_locally(
            buffer,
            Some(position),
            GetHover { position },
            cx,
        );
        cx.background_spawn(async move {
            Some(
                all_actions_task
                    .await
                    .into_iter()
                    .filter_map(|(_, hover)| remove_empty_hover_blocks(hover?))
                    .collect::<Vec<Hover>>(),
            )
        })
    }

    pub fn on_buffer_edited(
        &mut self,
        buffer: Entity<Buffer>,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let language_servers: Vec<_> = buffer.update(cx, |buffer, cx| {
            Some(
                self.as_local()?
                    .language_servers_for_buffer(buffer, cx)
                    .map(|i| i.1.clone())
                    .collect(),
            )
        })?;

        let buffer = buffer.read(cx);
        let file = File::from_dyn(buffer.file())?;
        let abs_path = file.as_local()?.abs_path(cx);
        let uri = lsp::Uri::from_file_path(&abs_path)
            .ok()
            .with_context(|| format!("Failed to convert path to URI: {}", abs_path.display()))
            .log_err()?;
        let next_snapshot = buffer.text_snapshot();
        for language_server in language_servers {
            let language_server = language_server.clone();

            let buffer_snapshots = self
                .as_local_mut()?
                .buffer_snapshots
                .get_mut(&buffer.remote_id())
                .and_then(|m| m.get_mut(&language_server.server_id()))?;
            let previous_snapshot = buffer_snapshots.last()?;

            let build_incremental_change = || {
                buffer
                    .edits_since::<Dimensions<PointUtf16, usize>>(
                        previous_snapshot.snapshot.version(),
                    )
                    .map(|edit| {
                        let edit_start = edit.new.start.0;
                        let edit_end = edit_start + (edit.old.end.0 - edit.old.start.0);
                        let new_text = next_snapshot
                            .text_for_range(edit.new.start.1..edit.new.end.1)
                            .collect();
                        lsp::TextDocumentContentChangeEvent {
                            range: Some(lsp::Range::new(
                                point_to_lsp(edit_start),
                                point_to_lsp(edit_end),
                            )),
                            range_length: None,
                            text: new_text,
                        }
                    })
                    .collect()
            };

            let document_sync_kind = language_server
                .capabilities()
                .text_document_sync
                .as_ref()
                .and_then(|sync| match sync {
                    lsp::TextDocumentSyncCapability::Kind(kind) => Some(*kind),
                    lsp::TextDocumentSyncCapability::Options(options) => options.change,
                });

            let content_changes: Vec<_> = match document_sync_kind {
                Some(lsp::TextDocumentSyncKind::FULL) => {
                    vec![lsp::TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: next_snapshot.text(),
                    }]
                }
                Some(lsp::TextDocumentSyncKind::INCREMENTAL) => build_incremental_change(),
                _ => {
                    #[cfg(any(test, feature = "test-support"))]
                    {
                        build_incremental_change()
                    }

                    #[cfg(not(any(test, feature = "test-support")))]
                    {
                        continue;
                    }
                }
            };

            let next_version = previous_snapshot.version + 1;
            buffer_snapshots.push(LspBufferSnapshot {
                version: next_version,
                snapshot: next_snapshot.clone(),
            });

            language_server
                .notify::<lsp::notification::DidChangeTextDocument>(
                    lsp::DidChangeTextDocumentParams {
                        text_document: lsp::VersionedTextDocumentIdentifier::new(
                            uri.clone(),
                            next_version,
                        ),
                        content_changes,
                    },
                )
                .ok();
        }

        None
    }

    pub fn on_buffer_saved(
        &mut self,
        buffer: Entity<Buffer>,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let file = File::from_dyn(buffer.read(cx).file())?;
        let worktree_id = file.worktree_id(cx);
        let abs_path = file.as_local()?.abs_path(cx);
        let text_document = lsp::TextDocumentIdentifier {
            uri: file_path_to_lsp_url(&abs_path).log_err()?,
        };
        let local = self.as_local()?;

        for server in local.language_servers_for_worktree(worktree_id) {
            if let Some(include_text) = include_text(server.as_ref()) {
                let text = if include_text {
                    Some(buffer.read(cx).text())
                } else {
                    None
                };
                server
                    .notify::<lsp::notification::DidSaveTextDocument>(
                        lsp::DidSaveTextDocumentParams {
                            text_document: text_document.clone(),
                            text,
                        },
                    )
                    .ok();
            }
        }

        None
    }

    async fn refresh_workspace_configurations(lsp_store: &WeakEntity<Self>, cx: &mut AsyncApp) {
        maybe!(async move {
            let mut refreshed_servers = HashSet::default();
            let servers = lsp_store
                .update(cx, |lsp_store, cx| {
                    let local = lsp_store.as_local()?;

                    let servers = local
                        .language_server_ids
                        .iter()
                        .filter_map(|(seed, state)| {
                            let worktree = lsp_store
                                .worktree_store
                                .read(cx)
                                .worktree_for_id(seed.worktree_id, cx);
                            let delegate: Arc<dyn LspAdapterDelegate> =
                                worktree.map(|worktree| {
                                    LocalLspAdapterDelegate::new(
                                        local.languages.clone(),
                                        &local.environment,
                                        cx.weak_entity(),
                                        &worktree,
                                        local.http_client.clone(),
                                        local.fs.clone(),
                                        cx,
                                    )
                                })?;
                            let server_id = state.id;

                            let states = local.language_servers.get(&server_id)?;

                            match states {
                                LanguageServerState::Starting { .. } => None,
                                LanguageServerState::Running {
                                    adapter, server, ..
                                } => {
                                    let adapter = adapter.clone();
                                    let server = server.clone();
                                    refreshed_servers.insert(server.name());
                                    let toolchain = seed.toolchain.clone();
                                    Some(cx.spawn(async move |_, cx| {
                                        let settings =
                                            LocalLspStore::workspace_configuration_for_adapter(
                                                adapter.adapter.clone(),
                                                &delegate,
                                                toolchain,
                                                None,
                                                cx,
                                            )
                                            .await
                                            .ok()?;
                                        server
                                            .notify::<lsp::notification::DidChangeConfiguration>(
                                                lsp::DidChangeConfigurationParams { settings },
                                            )
                                            .ok()?;
                                        Some(())
                                    }))
                                }
                            }
                        })
                        .collect::<Vec<_>>();

                    Some(servers)
                })
                .ok()
                .flatten()?;

            log::debug!("Refreshing workspace configurations for servers {refreshed_servers:?}");
            // TODO this asynchronous job runs concurrently with extension (de)registration and may take enough time for a certain extension
            // to stop and unregister its language server wrapper.
            // This is racy : an extension might have already removed all `local.language_servers` state, but here we `.clone()` and hold onto it anyway.
            // This now causes errors in the logs, we should find a way to remove such servers from the processing everywhere.
            let _: Vec<Option<()>> = join_all(servers).await;

            Some(())
        })
        .await;
    }

    fn maintain_workspace_config(
        mut external_refresh_requests: watch::Receiver<()>,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        // Multiple things can happen when a workspace environment (selected toolchain + settings) change:
        // - We might shut down a language server if it's no longer enabled for a given language (and there are no buffers using it otherwise).
        // - We might also shut it down when the workspace configuration of all of the users of a given language server converges onto that of the other.
        // - In the same vein, we might also decide to start a new language server if the workspace configuration *diverges* from the other.
        // - In the easiest case (where we're not wrangling the lifetime of a language server anyhow), if none of the roots of a single language server diverge in their configuration,
        // but it is still different to what we had before, we're gonna send out a workspace configuration update.
        //
        // Settings-store changes reach this loop via `on_settings_changed` -> `request_workspace_config_refresh`,
        // which writes to `external_refresh_requests`. Observing `SettingsStore` here as well would cause every
        // settings change to drive the loop twice and emit duplicate `workspace/didChangeConfiguration` notifications.
        cx.spawn(async move |this, cx| {
            while let Some(()) = external_refresh_requests.next().await {
                this.update(cx, |this, cx| {
                    this.refresh_server_tree(cx);
                })
                .ok();

                Self::refresh_workspace_configurations(&this, cx).await;
            }

            anyhow::Ok(())
        })
    }

    pub fn running_language_servers_for_local_buffer<'a>(
        &'a self,
        buffer: &Buffer,
        cx: &mut App,
    ) -> impl Iterator<Item = (&'a Arc<CachedLspAdapter>, &'a Arc<LanguageServer>)> {
        let local = self.as_local();
        let language_server_ids = local
            .map(|local| local.language_server_ids_for_buffer(buffer, cx))
            .unwrap_or_default();

        language_server_ids
            .into_iter()
            .filter_map(
                move |server_id| match local?.language_servers.get(&server_id)? {
                    LanguageServerState::Running {
                        adapter, server, ..
                    } => Some((adapter, server)),
                    _ => None,
                },
            )
    }

    pub fn language_servers_for_local_buffer(
        &self,
        buffer: &Buffer,
        cx: &mut App,
    ) -> Vec<LanguageServerId> {
        let local = self.as_local();
        local
            .map(|local| local.language_server_ids_for_buffer(buffer, cx))
            .unwrap_or_default()
    }

    pub fn language_server_for_local_buffer<'a>(
        &'a self,
        buffer: &'a Buffer,
        server_id: LanguageServerId,
        cx: &'a mut App,
    ) -> Option<(&'a Arc<CachedLspAdapter>, &'a Arc<LanguageServer>)> {
        self.as_local()?
            .language_servers_for_buffer(buffer, cx)
            .find(|(_, s)| s.server_id() == server_id)
    }

    fn remove_worktree(&mut self, id_to_remove: WorktreeId, cx: &mut Context<Self>) {
        if let Some(local) = self.as_local_mut() {
            let to_remove = local.remove_worktree(id_to_remove, cx);
            for server in to_remove {
                self.language_server_statuses.remove(&server);
            }
        }
    }

    pub fn shared(
        &mut self,
        project_id: u64,
        downstream_client: AnyProtoClient,
        _: &mut Context<Self>,
    ) {
        self.downstream_client = Some((downstream_client.clone(), project_id));

        for (server_id, status) in &self.language_server_statuses {
            if let Some(server) = self.language_server_for_id(*server_id) {
                downstream_client
                    .send(proto::StartLanguageServer {
                        project_id,
                        server: Some(proto::LanguageServer {
                            id: server_id.to_proto(),
                            name: status.name.to_string(),
                            worktree_id: status.worktree.map(|id| id.to_proto()),
                            language_name: status
                                .language_name
                                .as_ref()
                                .map(|name| name.to_proto()),
                        }),
                        capabilities: serde_json::to_string(&server.capabilities())
                            .expect("serializing server LSP capabilities"),
                    })
                    .log_err();
            }
        }
    }

    pub(crate) fn set_language_server_statuses_from_proto(
        &mut self,
        project: WeakEntity<Project>,
        language_servers: Vec<proto::LanguageServer>,
        server_capabilities: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let lsp_logs = cx
            .try_global::<GlobalLogStore>()
            .map(|lsp_store| lsp_store.0.clone());

        self.language_server_statuses = language_servers
            .into_iter()
            .zip(server_capabilities)
            .map(|(server, server_capabilities)| {
                let server_id = LanguageServerId(server.id as usize);
                if let Ok(server_capabilities) = serde_json::from_str(&server_capabilities) {
                    self.lsp_server_capabilities
                        .insert(server_id, server_capabilities);
                }

                let name = LanguageServerName::from_proto(server.name);
                let worktree = server.worktree_id.map(WorktreeId::from_proto);
                let language_name = server.language_name.map(LanguageName::from_proto);

                if let Some(lsp_logs) = &lsp_logs {
                    lsp_logs.update(cx, |lsp_logs, cx| {
                        lsp_logs.add_language_server(
                            // Only remote clients get their language servers set from proto
                            LanguageServerKind::Remote {
                                project: project.clone(),
                            },
                            server_id,
                            Some(name.clone()),
                            worktree,
                            None,
                            cx,
                        );
                    });
                }

                if let Some(ref lang_name) = language_name {
                    self.try_register_remote_adapter_locally(&name, lang_name);
                }

                (
                    server_id,
                    LanguageServerStatus {
                        name,
                        language_name: language_name,
                        server_version: None,
                        server_readable_version: None,
                        pending_work: Default::default(),
                        progress_tokens: Default::default(),
                        worktree,
                        binary: None,
                        configuration: None,
                        workspace_folders: BTreeSet::new(),
                        process_id: None,
                    },
                )
            })
            .collect();
    }

    fn try_register_remote_adapter_locally(
        &self,
        server_name: &LanguageServerName,
        language_name: &LanguageName,
    ) {
        let already_registered = self
            .languages
            .lsp_adapters(language_name)
            .iter()
            .any(|adapter| adapter.name() == *server_name);

        if already_registered {
            return;
        }

        if let Some(adapter) = self.languages.load_available_lsp_adapter(server_name) {
            log::info!(
                "Registering LSP adapter '{}' for language '{}' on local client",
                server_name.0,
                language_name.0
            );
            self.languages
                .register_lsp_adapter(language_name.clone(), adapter.adapter.clone());
        } else {
            log::warn!(
                "LSP adapter '{}' for language '{}' not available locally",
                server_name.0,
                language_name.0
            );
        }
    }

    pub fn open_buffer_for_symbol(
        &mut self,
        symbol: &Symbol,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Buffer>>> {
        if let Some(local) = self.as_local() {
            let is_valid = local.language_server_ids.iter().any(|(seed, state)| {
                seed.worktree_id == symbol.source_worktree_id
                    && state.id == symbol.source_language_server_id
                    && symbol.language_server_name == seed.name
            });
            if !is_valid {
                return Task::ready(Err(anyhow!(
                    "language server for worktree and language not found"
                )));
            };

            let symbol_abs_path = match &symbol.path {
                SymbolLocation::InProject(project_path) => self
                    .worktree_store
                    .read(cx)
                    .absolutize(&project_path, cx)
                    .context("no such worktree"),
                SymbolLocation::OutsideProject {
                    abs_path,
                    signature: _,
                } => Ok(abs_path.to_path_buf()),
            };
            let symbol_abs_path = match symbol_abs_path {
                Ok(abs_path) => abs_path,
                Err(err) => return Task::ready(Err(err)),
            };
            let symbol_uri = if let Ok(uri) = lsp::Uri::from_file_path(symbol_abs_path) {
                uri
            } else {
                return Task::ready(Err(anyhow!("invalid symbol path")));
            };

            self.open_local_buffer_via_lsp(symbol_uri, symbol.source_language_server_id, cx)
        } else {
            Task::ready(Err(anyhow!("no upstream client or local store")))
        }
    }

    pub(crate) fn open_local_buffer_via_lsp(
        &mut self,
        abs_path: lsp::Uri,
        language_server_id: LanguageServerId,
        cx: &mut Context<Self>,
    ) -> Task<Result<Entity<Buffer>>> {
        let path_style = self.worktree_store.read(cx).path_style();
        cx.spawn(async move |lsp_store, cx| {
            // Escape percent-encoded string.
            let current_scheme = abs_path.scheme().to_owned();
            // Uri is immutable, so we can't modify the scheme

            let abs_path = abs_path
                .to_file_path_ext(path_style)
                .map_err(|()| anyhow!("can't convert URI to path"))?;
            let p = abs_path.clone();
            let yarn_worktree = lsp_store
                .update(cx, move |lsp_store, cx| match lsp_store.as_local() {
                    Some(local_lsp_store) => local_lsp_store.yarn.update(cx, |_, cx| {
                        cx.spawn(async move |this, cx| {
                            let t = this
                                .update(cx, |this, cx| this.process_path(&p, &current_scheme, cx))
                                .ok()?;
                            t.await
                        })
                    }),
                    None => Task::ready(None),
                })?
                .await;
            let (worktree_root_target, known_relative_path) =
                if let Some((zip_root, relative_path)) = yarn_worktree {
                    (zip_root, Some(relative_path))
                } else {
                    (Arc::<Path>::from(abs_path.as_path()), None)
                };
            let worktree = lsp_store.update(cx, |lsp_store, cx| {
                lsp_store.worktree_store.update(cx, |worktree_store, cx| {
                    worktree_store.find_worktree(&worktree_root_target, cx)
                })
            })?;
            let (worktree, relative_path, source_ws) = if let Some(result) = worktree {
                let relative_path = known_relative_path.unwrap_or_else(|| result.1.clone());
                (result.0, relative_path, None)
            } else {
                let worktree = lsp_store
                    .update(cx, |lsp_store, cx| {
                        lsp_store.worktree_store.update(cx, |worktree_store, cx| {
                            worktree_store.create_worktree(&worktree_root_target, false, cx)
                        })
                    })?
                    .await?;
                let worktree_root = worktree.read_with(cx, |worktree, _| worktree.abs_path());
                let source_ws = if worktree.read_with(cx, |worktree, _| worktree.is_local()) {
                    lsp_store
                        .update(cx, |lsp_store, cx| {
                            if let Some(local) = lsp_store.as_local_mut() {
                                local.register_language_server_for_invisible_worktree(
                                    &worktree,
                                    language_server_id,
                                    cx,
                                )
                            }
                            match lsp_store.language_server_statuses.get(&language_server_id) {
                                Some(status) => status.worktree,
                                None => None,
                            }
                        })
                        .ok()
                        .flatten()
                        .zip(Some(worktree_root.clone()))
                } else {
                    None
                };
                let relative_path = if let Some(known_path) = known_relative_path {
                    known_path
                } else {
                    RelPath::new(abs_path.strip_prefix(worktree_root)?, PathStyle::local())?
                        .into_arc()
                };
                (worktree, relative_path, source_ws)
            };
            let project_path = ProjectPath {
                worktree_id: worktree.read_with(cx, |worktree, _| worktree.id()),
                path: relative_path,
            };
            let buffer = lsp_store
                .update(cx, |lsp_store, cx| {
                    lsp_store.buffer_store().update(cx, |buffer_store, cx| {
                        buffer_store.open_buffer(project_path, cx)
                    })
                })?
                .await?;
            // we want to adhere to the read-only settings of the worktree we came from in case we opened an invisible one
            if let Some((source_ws, worktree_root)) = source_ws {
                buffer.update(cx, |buffer, cx| {
                    let settings = WorktreeSettings::get(
                        Some(
                            (&ProjectPath {
                                worktree_id: source_ws,
                                path: Arc::from(RelPath::empty()),
                            })
                                .into(),
                        ),
                        cx,
                    );
                    let is_read_only = settings.is_std_path_read_only(&worktree_root);
                    if is_read_only {
                        buffer.set_capability(Capability::ReadOnly, cx);
                    }
                });
            }
            Ok(buffer)
        })
    }

    fn local_lsp_servers_for_buffer(
        &self,
        buffer: &Entity<Buffer>,
        cx: &mut Context<Self>,
    ) -> Vec<LanguageServerId> {
        let Some(local) = self.as_local() else {
            return Vec::new();
        };

        let snapshot = buffer.read(cx).snapshot();

        buffer.update(cx, |buffer, cx| {
            local
                .language_servers_for_buffer(buffer, cx)
                .map(|(_, server)| server.server_id())
                .filter(|server_id| {
                    self.as_local().is_none_or(|local| {
                        local
                            .buffers_opened_in_servers
                            .get(&snapshot.remote_id())
                            .is_some_and(|servers| servers.contains(server_id))
                    })
                })
                .collect()
        })
    }

    fn request_multiple_lsp_locally<P, R>(
        &mut self,
        buffer: &Entity<Buffer>,
        position: Option<P>,
        request: R,
        cx: &mut Context<Self>,
    ) -> Task<Vec<(LanguageServerId, R::Response)>>
    where
        P: ToOffset,
        R: LspCommand + Clone,
        <R::LspRequest as lsp::request::Request>::Result: Send,
        <R::LspRequest as lsp::request::Request>::Params: Send,
    {
        let Some(local) = self.as_local() else {
            return Task::ready(Vec::new());
        };

        let snapshot = buffer.read(cx).snapshot();
        let scope = position.and_then(|position| snapshot.language_scope_at(position));

        let server_ids = buffer.update(cx, |buffer, cx| {
            local
                .language_servers_for_buffer(buffer, cx)
                .filter(|(adapter, _)| {
                    scope
                        .as_ref()
                        .map(|scope| scope.language_allowed(&adapter.name))
                        .unwrap_or(true)
                })
                .map(|(_, server)| server.server_id())
                .filter(|server_id| {
                    self.as_local().is_none_or(|local| {
                        local
                            .buffers_opened_in_servers
                            .get(&snapshot.remote_id())
                            .is_some_and(|servers| servers.contains(server_id))
                    })
                })
                .collect::<Vec<_>>()
        });

        let mut response_results = server_ids
            .into_iter()
            .map(|server_id| {
                let task = self.request_lsp(
                    buffer.clone(),
                    LanguageServerToQuery::Other(server_id),
                    request.clone(),
                    cx,
                );
                async move { (server_id, task.await) }
            })
            .collect::<FuturesUnordered<_>>();

        cx.background_spawn(async move {
            let mut responses = Vec::with_capacity(response_results.len());
            while let Some((server_id, response_result)) = response_results.next().await {
                match response_result {
                    Ok(response) => responses.push((server_id, response)),
                    // rust-analyzer likes to error with this when its still loading up
                    Err(e) if format!("{e:#}").ends_with("content modified") => (),
                    Err(e) => log::error!("Error handling response for request {request:?}: {e:#}"),
                }
            }
            responses
        })
    }

    pub fn language_server_statuses(
        &self,
    ) -> impl DoubleEndedIterator<Item = (LanguageServerId, &LanguageServerStatus)> {
        self.language_server_statuses
            .iter()
            .map(|(key, value)| (*key, value))
    }

    #[cfg(feature = "test-support")]
    pub fn has_language_server_seed_for_worktree(&self, worktree_id: WorktreeId) -> bool {
        self.as_local().is_some_and(|local| {
            local
                .language_server_ids
                .keys()
                .any(|seed| seed.worktree_id == worktree_id)
        })
    }

    pub(super) fn did_rename_entry(
        &self,
        worktree_id: WorktreeId,
        old_path: &Path,
        new_path: &Path,
        is_dir: bool,
    ) {
        maybe!({
            let local_store = self.as_local()?;

            let old_uri = lsp::Uri::from_file_path(old_path)
                .ok()
                .map(|uri| uri.to_string())?;
            let new_uri = lsp::Uri::from_file_path(new_path)
                .ok()
                .map(|uri| uri.to_string())?;

            for language_server in local_store.language_servers_for_worktree(worktree_id) {
                let Some(filter) = local_store
                    .language_server_paths_watched_for_rename
                    .get(&language_server.server_id())
                else {
                    continue;
                };

                if filter.should_send_did_rename(&old_uri, is_dir) {
                    language_server
                        .notify::<DidRenameFiles>(RenameFilesParams {
                            files: vec![FileRename {
                                old_uri: old_uri.clone(),
                                new_uri: new_uri.clone(),
                            }],
                        })
                        .ok();
                }
            }
            Some(())
        });
    }

    pub(super) fn will_rename_entry(
        this: WeakEntity<Self>,
        worktree_id: WorktreeId,
        old_path: &Path,
        new_path: &Path,
        is_dir: bool,
        cx: AsyncApp,
    ) -> Task<ProjectTransaction> {
        let old_uri = lsp::Uri::from_file_path(old_path)
            .ok()
            .map(|uri| uri.to_string());
        let new_uri = lsp::Uri::from_file_path(new_path)
            .ok()
            .map(|uri| uri.to_string());
        cx.spawn(async move |cx| {
            let mut tasks = vec![];
            this.update(cx, |this, cx| {
                let local_store = this.as_local()?;
                let old_uri = old_uri?;
                let new_uri = new_uri?;
                for language_server in local_store.language_servers_for_worktree(worktree_id) {
                    let Some(filter) = local_store
                        .language_server_paths_watched_for_rename
                        .get(&language_server.server_id())
                    else {
                        continue;
                    };

                    if !filter.should_send_will_rename(&old_uri, is_dir) {
                        continue;
                    }
                    let request_timeout = ProjectSettings::get_global(cx)
                        .global_lsp_settings
                        .get_request_timeout();

                    let apply_edit = cx.spawn({
                        let old_uri = old_uri.clone();
                        let new_uri = new_uri.clone();
                        let language_server = language_server.clone();
                        async move |this, cx| {
                            let edit = language_server
                                .request::<WillRenameFiles>(
                                    RenameFilesParams {
                                        files: vec![FileRename { old_uri, new_uri }],
                                    },
                                    request_timeout,
                                )
                                .await
                                .into_response()
                                .context("will rename files")
                                .log_err()
                                .flatten()?;

                            LocalLspStore::deserialize_workspace_edit(
                                this.upgrade()?,
                                edit,
                                false,
                                language_server.clone(),
                                cx,
                            )
                            .await
                            .ok()
                        }
                    });
                    tasks.push(apply_edit);
                }
                Some(())
            })
            .ok()
            .flatten();
            let mut merged_transaction = ProjectTransaction::default();
            for task in tasks {
                // Await on tasks sequentially so that the order of application of edits is deterministic
                // (at least with regards to the order of registration of language servers)
                if let Some(transaction) = task.await {
                    for (buffer, buffer_transaction) in transaction.0 {
                        merged_transaction.0.insert(buffer, buffer_transaction);
                    }
                }
            }
            merged_transaction
        })
    }

    fn lsp_notify_abs_paths_changed(
        &mut self,
        server_id: LanguageServerId,
        changes: Vec<PathEvent>,
    ) {
        maybe!({
            let server = self.language_server_for_id(server_id)?;
            let changes = changes
                .into_iter()
                .filter_map(|event| {
                    let typ = match event.kind? {
                        PathEventKind::Created => lsp::FileChangeType::CREATED,
                        PathEventKind::Removed => lsp::FileChangeType::DELETED,
                        PathEventKind::Changed | PathEventKind::Rescan => {
                            lsp::FileChangeType::CHANGED
                        }
                    };
                    Some(lsp::FileEvent {
                        uri: file_path_to_lsp_url(&event.path).log_err()?,
                        typ,
                    })
                })
                .collect::<Vec<_>>();
            if !changes.is_empty() {
                server
                    .notify::<lsp::notification::DidChangeWatchedFiles>(
                        lsp::DidChangeWatchedFilesParams { changes },
                    )
                    .ok();
            }
            Some(())
        });
    }

    pub fn language_server_for_id(&self, id: LanguageServerId) -> Option<Arc<LanguageServer>> {
        self.as_local()?.language_server_for_id(id)
    }

    fn on_lsp_progress(
        &mut self,
        progress_params: lsp::ProgressParams,
        language_server_id: LanguageServerId,
        cx: &mut Context<Self>,
    ) {
        match progress_params.value {
            lsp::ProgressParamsValue::WorkDone(progress) => {
                self.handle_work_done_progress(
                    progress,
                    language_server_id,
                    ProgressToken::from_lsp(progress_params.token),
                    cx,
                );
            }
        }
    }

    fn handle_work_done_progress(
        &mut self,
        progress: lsp::WorkDoneProgress,
        language_server_id: LanguageServerId,
        token: ProgressToken,
        cx: &mut Context<Self>,
    ) {
        let language_server_status =
            if let Some(status) = self.language_server_statuses.get_mut(&language_server_id) {
                status
            } else {
                return;
            };

        if !language_server_status.progress_tokens.contains(&token) {
            return;
        }

        match progress {
            lsp::WorkDoneProgress::Begin(report) => {
                self.on_lsp_work_start(
                    language_server_id,
                    token.clone(),
                    LanguageServerProgress {
                        title: Some(report.title),
                        is_cancellable: report.cancellable.unwrap_or(false),
                        message: report.message.clone(),
                        percentage: report.percentage.map(|p| p as usize),
                        last_update_at: cx.background_executor().now(),
                    },
                    cx,
                );
            }
            lsp::WorkDoneProgress::Report(report) => self.on_lsp_work_progress(
                language_server_id,
                token,
                LanguageServerProgress {
                    title: None,
                    is_cancellable: report.cancellable.unwrap_or(false),
                    message: report.message,
                    percentage: report.percentage.map(|p| p as usize),
                    last_update_at: cx.background_executor().now(),
                },
                cx,
            ),
            lsp::WorkDoneProgress::End(_) => {
                language_server_status.progress_tokens.remove(&token);
                self.on_lsp_work_end(language_server_id, token.clone(), cx);
            }
        }
    }

    fn on_lsp_work_start(
        &mut self,
        language_server_id: LanguageServerId,
        token: ProgressToken,
        progress: LanguageServerProgress,
        cx: &mut Context<Self>,
    ) {
        if let Some(status) = self.language_server_statuses.get_mut(&language_server_id) {
            status.pending_work.insert(token.clone(), progress.clone());
            cx.notify();
        }
        cx.emit(LspStoreEvent::LanguageServerUpdate {
            language_server_id,
            name: self
                .language_server_adapter_for_id(language_server_id)
                .map(|adapter| adapter.name()),
        })
    }

    fn on_lsp_work_progress(
        &mut self,
        language_server_id: LanguageServerId,
        token: ProgressToken,
        progress: LanguageServerProgress,
        cx: &mut Context<Self>,
    ) {
        let mut did_update = false;
        if let Some(status) = self.language_server_statuses.get_mut(&language_server_id) {
            match status.pending_work.entry(token.clone()) {
                btree_map::Entry::Vacant(entry) => {
                    entry.insert(progress.clone());
                    did_update = true;
                }
                btree_map::Entry::Occupied(mut entry) => {
                    let entry = entry.get_mut();
                    if (progress.last_update_at - entry.last_update_at)
                        >= SERVER_PROGRESS_THROTTLE_TIMEOUT
                    {
                        entry.last_update_at = progress.last_update_at;
                        if progress.message.is_some() {
                            entry.message = progress.message.clone();
                        }
                        if progress.percentage.is_some() {
                            entry.percentage = progress.percentage;
                        }
                        if progress.is_cancellable != entry.is_cancellable {
                            entry.is_cancellable = progress.is_cancellable;
                        }
                        did_update = true;
                    }
                }
            }
        }

        if did_update {
            cx.emit(LspStoreEvent::LanguageServerUpdate {
                language_server_id,
                name: self
                    .language_server_adapter_for_id(language_server_id)
                    .map(|adapter| adapter.name()),
            })
        }
    }

    fn on_lsp_work_end(
        &mut self,
        language_server_id: LanguageServerId,
        token: ProgressToken,
        cx: &mut Context<Self>,
    ) {
        if let Some(status) = self.language_server_statuses.get_mut(&language_server_id) {
            status.pending_work.remove(&token);
        }

        cx.emit(LspStoreEvent::LanguageServerUpdate {
            language_server_id,
            name: self
                .language_server_adapter_for_id(language_server_id)
                .map(|adapter| adapter.name()),
        })
    }

    pub fn environment_for_buffer(
        &self,
        buffer: &Entity<Buffer>,
        cx: &mut Context<Self>,
    ) -> Shared<Task<Option<HashMap<String, String>>>> {
        if let Some(environment) = &self.as_local().map(|local| local.environment.clone()) {
            environment.update(cx, |env, cx| {
                env.buffer_environment(buffer, &self.worktree_store, cx)
            })
        } else {
            Task::ready(None).shared()
        }
    }

    async fn shutdown_language_server(
        server_state: Option<LanguageServerState>,
        name: LanguageServerName,
        cx: &mut AsyncApp,
    ) {
        let server = match server_state {
            Some(LanguageServerState::Starting { startup, .. }) => {
                let mut timer = cx
                    .background_executor()
                    .timer(SERVER_LAUNCHING_BEFORE_SHUTDOWN_TIMEOUT)
                    .fuse();

                select! {
                    server = startup.fuse() => server,
                    () = timer => {
                        log::info!("timeout waiting for language server {name} to finish launching before stopping");
                        None
                    },
                }
            }

            Some(LanguageServerState::Running { server, .. }) => Some(server),

            None => None,
        };

        let Some(server) = server else { return };
        if let Some(shutdown) = server.shutdown() {
            shutdown.await;
        }
    }

    // Returns a list of all of the worktrees which no longer have a language server and the root path
    // for the stopped server
    fn stop_local_language_server(
        &mut self,
        server_id: LanguageServerId,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let local = match &mut self.mode {
            LspStoreMode::Local(local) => local,
        };

        // Remove this server ID from all entries in the given worktree.
        local
            .language_server_ids
            .retain(|_, state| state.id != server_id);
        self.buffer_store.update(cx, |buffer_store, cx| {
            for buffer in buffer_store.buffers() {
                buffer.update(cx, |buffer, cx| {
                    buffer.set_completion_triggers(server_id, Default::default(), cx);
                });
            }
        });

        let local = self.as_local_mut().unwrap();
        local.language_server_watched_paths.remove(&server_id);

        let server_state = local.language_servers.remove(&server_id);
        self.cleanup_lsp_data(server_id);
        let name = self
            .language_server_statuses
            .remove(&server_id)
            .map(|status| status.name)
            .or_else(|| {
                if let Some(LanguageServerState::Running { adapter, .. }) = server_state.as_ref() {
                    Some(adapter.name())
                } else {
                    None
                }
            });

        if let Some(name) = name {
            log::info!("stopping language server {name}");
            self.languages
                .update_lsp_binary_status(name.clone(), BinaryStatus::Stopping);
            cx.notify();

            return cx.spawn(async move |lsp_store, cx| {
                Self::shutdown_language_server(server_state, name.clone(), cx).await;
                lsp_store
                    .update(cx, |lsp_store, cx| {
                        lsp_store
                            .languages
                            .update_lsp_binary_status(name, BinaryStatus::Stopped);
                        cx.emit(LspStoreEvent::LanguageServerRemoved(server_id));
                        cx.notify();
                    })
                    .ok();
            });
        }

        if server_state.is_some() {
            cx.emit(LspStoreEvent::LanguageServerRemoved(server_id));
        }
        Task::ready(())
    }

    pub fn stop_all_language_servers(&mut self, cx: &mut Context<Self>) {
        self.shutdown_all_language_servers(cx).detach();
    }

    pub fn shutdown_all_language_servers(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some(local) = self.as_local_mut() else {
            return Task::ready(());
        };
        let language_servers_to_stop = local
            .language_server_ids
            .values()
            .map(|state| state.id)
            .collect();
        local.lsp_tree.remove_nodes(&language_servers_to_stop);
        let tasks = language_servers_to_stop
            .into_iter()
            .map(|server| self.stop_local_language_server(server, cx))
            .collect::<Vec<_>>();
        cx.background_spawn(async move {
            futures::future::join_all(tasks).await;
        })
    }

    pub fn restart_all_language_servers(&mut self, cx: &mut Context<Self>) {
        let buffers = self.buffer_store.read(cx).buffers().collect();
        self.restart_language_servers_for_buffers(buffers, HashSet::default(), cx);
    }

    pub fn restart_language_servers_for_buffers(
        &mut self,
        buffers: Vec<Entity<Buffer>>,
        only_restart_servers: HashSet<LanguageServerSelector>,
        cx: &mut Context<Self>,
    ) {
        let stop_task = if only_restart_servers.is_empty() {
            self.stop_local_language_servers_for_buffers(&buffers, HashSet::default(), cx)
        } else {
            self.stop_local_language_servers_for_buffers(&[], only_restart_servers.clone(), cx)
        };
        cx.spawn(async move |lsp_store, cx| {
            stop_task.await;
            lsp_store.update(cx, |lsp_store, cx| {
                for buffer in buffers {
                    lsp_store.register_buffer_with_language_servers(
                        &buffer,
                        only_restart_servers.clone(),
                        true,
                        cx,
                    );
                }
            })
        })
        .detach();
    }

    pub fn stop_language_servers_for_buffers(
        &mut self,
        buffers: Vec<Entity<Buffer>>,
        also_stop_servers: HashSet<LanguageServerSelector>,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let task =
            self.stop_local_language_servers_for_buffers(&buffers, also_stop_servers, cx);
        cx.background_spawn(async move {
            task.await;
            Ok(())
        })
    }

    fn stop_local_language_servers_for_buffers(
        &mut self,
        buffers: &[Entity<Buffer>],
        also_stop_servers: HashSet<LanguageServerSelector>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let Some(local) = self.as_local_mut() else {
            return Task::ready(());
        };
        let mut language_server_names_to_stop = BTreeSet::default();
        let mut language_servers_to_stop = also_stop_servers
            .into_iter()
            .flat_map(|selector| match selector {
                LanguageServerSelector::Id(id) => Some(id),
                LanguageServerSelector::Name(name) => {
                    language_server_names_to_stop.insert(name);
                    None
                }
            })
            .collect::<BTreeSet<_>>();

        let mut covered_worktrees = HashSet::default();
        for buffer in buffers {
            buffer.update(cx, |buffer, cx| {
                language_servers_to_stop.extend(local.language_server_ids_for_buffer(buffer, cx));
                if let Some(worktree_id) = buffer.file().map(|f| f.worktree_id(cx))
                    && covered_worktrees.insert(worktree_id)
                {
                    language_server_names_to_stop.retain(|name| {
                        let old_ids_count = language_servers_to_stop.len();
                        let all_language_servers_with_this_name = local
                            .language_server_ids
                            .iter()
                            .filter_map(|(seed, state)| seed.name.eq(name).then(|| state.id));
                        language_servers_to_stop.extend(all_language_servers_with_this_name);
                        old_ids_count == language_servers_to_stop.len()
                    });
                }
            });
        }
        for name in language_server_names_to_stop {
            language_servers_to_stop.extend(
                local
                    .language_server_ids
                    .iter()
                    .filter_map(|(seed, v)| seed.name.eq(&name).then(|| v.id)),
            );
        }

        local.lsp_tree.remove_nodes(&language_servers_to_stop);
        let tasks = language_servers_to_stop
            .into_iter()
            .map(|server| self.stop_local_language_server(server, cx))
            .collect::<Vec<_>>();

        cx.background_spawn(futures::future::join_all(tasks).map(|_| ()))
    }

    fn insert_newly_running_language_server(
        &mut self,
        adapter: Arc<CachedLspAdapter>,
        language_server: Arc<LanguageServer>,
        server_id: LanguageServerId,
        key: LanguageServerSeed,
        language_name: LanguageName,
        workspace_folders: Arc<Mutex<BTreeSet<Uri>>>,
        cx: &mut Context<Self>,
    ) {
        let Some(local) = self.as_local_mut() else {
            return;
        };
        // If the language server for this key doesn't match the server id, don't store the
        // server. Which will cause it to be dropped, killing the process
        if local
            .language_server_ids
            .get(&key)
            .map(|state| state.id != server_id)
            .unwrap_or(false)
        {
            return;
        }

        // Update language_servers collection with Running variant of LanguageServerState
        // indicating that the server is up and running and ready
        let workspace_folders = workspace_folders.lock().clone();
        language_server.set_workspace_folders(workspace_folders);

        local.language_servers.insert(
            server_id,
            LanguageServerState::Running {
                adapter: adapter.clone(),
                server: language_server.clone(),
            },
        );
        local
            .languages
            .update_lsp_binary_status(adapter.name(), BinaryStatus::None);
        if let Some(file_ops_caps) = language_server
            .capabilities()
            .workspace
            .as_ref()
            .and_then(|ws| ws.file_operations.as_ref())
        {
            let did_rename_caps = file_ops_caps.did_rename.as_ref();
            let will_rename_caps = file_ops_caps.will_rename.as_ref();
            if did_rename_caps.or(will_rename_caps).is_some() {
                let watcher = RenamePathsWatchedForServer::default()
                    .with_did_rename_patterns(did_rename_caps)
                    .with_will_rename_patterns(will_rename_caps);
                local
                    .language_server_paths_watched_for_rename
                    .insert(server_id, watcher);
            }
        }

        self.language_server_statuses.insert(
            server_id,
            LanguageServerStatus {
                name: language_server.name(),
                language_name: Some(language_name.clone()),
                server_version: language_server.version(),
                server_readable_version: language_server.readable_version(),
                pending_work: Default::default(),
                progress_tokens: Default::default(),
                worktree: Some(key.worktree_id),
                binary: Some(language_server.binary().clone()),
                configuration: Some(language_server.configuration().clone()),
                workspace_folders: language_server.workspace_folders(),
                process_id: language_server.process_id(),
            },
        );

        cx.emit(LspStoreEvent::LanguageServerAdded(
            server_id,
            language_server.name(),
            Some(key.worktree_id),
        ));

        let server_capabilities = language_server.capabilities();
        if let Some((downstream_client, project_id)) = self.downstream_client.as_ref() {
            downstream_client
                .send(proto::StartLanguageServer {
                    project_id: *project_id,
                    server: Some(proto::LanguageServer {
                        id: server_id.to_proto(),
                        name: language_server.name().to_string(),
                        worktree_id: Some(key.worktree_id.to_proto()),
                        language_name: Some(language_name.to_proto()),
                    }),
                    capabilities: serde_json::to_string(&server_capabilities)
                        .expect("serializing server LSP capabilities"),
                })
                .log_err();
        }
        self.lsp_server_capabilities
            .insert(server_id, server_capabilities);

        // Tell the language server about every open buffer in the worktree that matches the language.
        // Also check for buffers in worktrees that reused this server
        let mut worktrees_using_server = vec![key.worktree_id];
        if let Some(local) = self.as_local() {
            // Find all worktrees that have this server in their language server tree
            for (worktree_id, servers) in &local.lsp_tree.instances {
                if *worktree_id != key.worktree_id {
                    for server_map in servers.roots.values() {
                        if server_map
                            .values()
                            .any(|(node, _)| node.id() == Some(server_id))
                        {
                            worktrees_using_server.push(*worktree_id);
                        }
                    }
                }
            }
        }

        let mut buffer_paths_registered = Vec::new();
        self.buffer_store.clone().update(cx, |buffer_store, cx| {
            let mut lsp_adapters = HashMap::default();
            for buffer_handle in buffer_store.buffers() {
                let buffer = buffer_handle.read(cx);
                let file = match File::from_dyn(buffer.file()) {
                    Some(file) => file,
                    None => continue,
                };
                let language = match buffer.language() {
                    Some(language) => language,
                    None => continue,
                };

                if !worktrees_using_server.contains(&file.worktree.read(cx).id())
                    || !lsp_adapters
                        .entry(language.name())
                        .or_insert_with(|| self.languages.lsp_adapters(&language.name()))
                        .iter()
                        .any(|a| a.name == key.name)
                {
                    continue;
                }
                // didOpen
                let file = match file.as_local() {
                    Some(file) => file,
                    None => continue,
                };

                let local = self.as_local_mut().unwrap();

                let buffer_id = buffer.remote_id();
                if local.registered_buffers.contains_key(&buffer_id) {
                    let abs_path = file.abs_path(cx);
                    let uri = match lsp::Uri::from_file_path(&abs_path) {
                        Ok(uri) => uri,
                        Err(()) => {
                            log::error!("failed to convert path to URI: {:?}", abs_path);
                            continue;
                        }
                    };

                    let versions = local
                        .buffer_snapshots
                        .entry(buffer_id)
                        .or_default()
                        .entry(server_id)
                        .and_modify(|_| {
                            assert!(
                            false,
                            "There should not be an existing snapshot for a newly inserted buffer"
                        )
                        })
                        .or_insert_with(|| {
                            vec![LspBufferSnapshot {
                                version: 0,
                                snapshot: buffer.text_snapshot(),
                            }]
                        });

                    let snapshot = versions.last().unwrap();
                    let version = snapshot.version;
                    let initial_snapshot = &snapshot.snapshot;
                    language_server.register_buffer(
                        uri,
                        adapter.language_id(&language.name()),
                        version,
                        initial_snapshot.text(),
                    );
                    buffer_paths_registered.push((buffer_id, abs_path));
                    local
                        .buffers_opened_in_servers
                        .entry(buffer_id)
                        .or_default()
                        .insert(server_id);
                }
                buffer_handle.update(cx, |buffer, cx| {
                    buffer.set_completion_triggers(
                        server_id,
                        language_server
                            .capabilities()
                            .completion_provider
                            .as_ref()
                            .and_then(|provider| {
                                provider
                                    .trigger_characters
                                    .as_ref()
                                    .map(|characters| characters.iter().cloned().collect())
                            })
                            .unwrap_or_default(),
                        cx,
                    )
                });
            }
        });

        for (_buffer_id, _abs_path) in buffer_paths_registered {
            cx.emit(LspStoreEvent::LanguageServerUpdate {
                language_server_id: server_id,
                name: Some(adapter.name()),
            });
        }

        cx.notify();
    }

    pub(crate) fn cancel_language_server_work_for_buffers(
        &mut self,
        buffers: impl IntoIterator<Item = Entity<Buffer>>,
        cx: &mut Context<Self>,
    ) {
        if let Some(local) = self.as_local() {
            let servers = buffers
                .into_iter()
                .flat_map(|buffer| {
                    buffer.update(cx, |buffer, cx| {
                        local.language_server_ids_for_buffer(buffer, cx).into_iter()
                    })
                })
                .collect::<HashSet<_>>();
            for server_id in servers {
                self.cancel_language_server_work(server_id, None, cx);
            }
        }
    }

    pub(crate) fn cancel_language_server_work(
        &mut self,
        server_id: LanguageServerId,
        token_to_cancel: Option<ProgressToken>,
        _cx: &mut Context<Self>,
    ) {
        if let Some(local) = self.as_local() {
            let status = self.language_server_statuses.get(&server_id);
            let server = local.language_servers.get(&server_id);
            if let Some((LanguageServerState::Running { server, .. }, status)) = server.zip(status)
            {
                for (token, progress) in &status.pending_work {
                    if let Some(token_to_cancel) = token_to_cancel.as_ref()
                        && token != token_to_cancel
                    {
                        continue;
                    }
                    if progress.is_cancellable {
                        server
                            .notify::<lsp::notification::WorkDoneProgressCancel>(
                                WorkDoneProgressCancelParams {
                                    token: token.to_lsp(),
                                },
                            )
                            .ok();
                    }
                }
            }
        }
    }

    pub(crate) fn supplementary_language_servers(
        &self,
    ) -> impl '_ + Iterator<Item = (LanguageServerId, LanguageServerName)> {
        self.as_local().into_iter().flat_map(|local| {
            local
                .supplementary_language_servers
                .iter()
                .map(|(id, (name, _))| (*id, name.clone()))
        })
    }

    pub fn language_server_adapter_for_id(
        &self,
        id: LanguageServerId,
    ) -> Option<Arc<CachedLspAdapter>> {
        if let Some(local) = self.as_local()
            && let Some(LanguageServerState::Running { adapter, .. }) =
                local.language_servers.get(&id)
        {
            return Some(adapter.clone());
        }
        // In remote (SSH/collab) mode there are no local `language_servers`, but
        // `language_server_statuses` is kept in sync with the upstream and carries each
        // server's registered name, which is enough to look the adapter up in the registry.
        let name = &self.language_server_statuses.get(&id)?.name;
        self.languages.adapter_for_name(name)
    }

    pub(super) fn update_local_worktree_language_servers(
        &mut self,
        worktree_handle: &Entity<Worktree>,
        changes: &[(Arc<RelPath>, ProjectEntryId, PathChange)],
        cx: &mut Context<Self>,
    ) {
        if changes.is_empty() {
            return;
        }

        let Some(local) = self.as_local() else { return };

        let worktree_id = worktree_handle.read(cx).id();
        let mut language_server_ids = local
            .language_server_ids
            .iter()
            .filter_map(|(seed, v)| seed.worktree_id.eq(&worktree_id).then(|| v.id))
            .collect::<Vec<_>>();
        language_server_ids.sort();
        language_server_ids.dedup();

        // let abs_path = worktree_handle.read(cx).abs_path();
        for server_id in &language_server_ids {
            if let Some(LanguageServerState::Running { server, .. }) =
                local.language_servers.get(server_id)
                && let Some(watched_paths) = local
                    .language_server_watched_paths
                    .get(server_id)
                    .and_then(|paths| paths.worktree_paths.get(&worktree_id))
            {
                let params = lsp::DidChangeWatchedFilesParams {
                    changes: changes
                        .iter()
                        .filter_map(|(path, _, change)| {
                            if !watched_paths.is_match(path.as_std_path()) {
                                return None;
                            }
                            let typ = match change {
                                PathChange::Loaded => return None,
                                PathChange::Added => lsp::FileChangeType::CREATED,
                                PathChange::Removed => lsp::FileChangeType::DELETED,
                                PathChange::Updated => lsp::FileChangeType::CHANGED,
                                PathChange::AddedOrUpdated => lsp::FileChangeType::CHANGED,
                            };
                            let uri = lsp::Uri::from_file_path(
                                worktree_handle.read(cx).absolutize(&path),
                            )
                            .ok()?;
                            Some(lsp::FileEvent { uri, typ })
                        })
                        .collect(),
                };
                if !params.changes.is_empty() {
                    server
                        .notify::<lsp::notification::DidChangeWatchedFiles>(params)
                        .ok();
                }
            }
        }
        for (path, _, _) in changes {
            if let Some(file_name) = path.file_name()
                && local.watched_manifest_filenames.contains(file_name)
            {
                self.request_workspace_config_refresh();
                break;
            }
        }
    }

    fn cleanup_lsp_data(&mut self, for_server: LanguageServerId) {
        self.lsp_server_capabilities.remove(&for_server);
        self.semantic_token_config.remove_server_data(for_server);
        for lsp_data in self.lsp_data.values_mut() {
            lsp_data.remove_server_data(for_server);
        }
        if let Some(local) = self.as_local_mut() {
            for buffer_servers in local.buffers_opened_in_servers.values_mut() {
                buffer_servers.remove(&for_server);
            }
        }
    }

    fn register_server_capabilities(
        &mut self,
        server_id: LanguageServerId,
        params: lsp::RegistrationParams,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let server = self
            .language_server_for_id(server_id)
            .with_context(|| format!("no server {server_id} found"))?;
        for reg in params.registrations {
            match reg.method.as_str() {
                "workspace/didChangeWatchedFiles" => {
                    if let Some(options) = reg.register_options {
                        let notify = if let Some(local_lsp_store) = self.as_local_mut() {
                            let caps = serde_json::from_value(options)?;
                            local_lsp_store
                                .on_lsp_did_change_watched_files(server_id, &reg.id, caps, cx);
                            true
                        } else {
                            false
                        };
                        if notify {
                            notify_server_capabilities_updated(&server, cx);
                        }
                    }
                }
                "workspace/didChangeConfiguration" => {
                    // Ignore payload since we notify clients of setting changes unconditionally, relying on them pulling the latest settings.
                }
                "workspace/didChangeWorkspaceFolders" => {
                    // In this case register options is an empty object, we can ignore it
                    let caps = lsp::WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Right(reg.id)),
                    };
                    server.update_capabilities(|capabilities| {
                        capabilities
                            .workspace
                            .get_or_insert_default()
                            .workspace_folders = Some(caps);
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "workspace/symbol" => {
                    let options = parse_register_capabilities(reg)?;
                    server.update_capabilities(|capabilities| {
                        capabilities.workspace_symbol_provider = Some(options);
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "workspace/fileOperations" => {
                    if let Some(options) = reg.register_options {
                        let caps = serde_json::from_value(options)?;
                        server.update_capabilities(|capabilities| {
                            capabilities
                                .workspace
                                .get_or_insert_default()
                                .file_operations = Some(caps);
                        });
                        notify_server_capabilities_updated(&server, cx);
                    }
                }
                "workspace/executeCommand" => {
                    if let Some(options) = reg.register_options {
                        let options = serde_json::from_value(options)?;
                        server.update_capabilities(|capabilities| {
                            capabilities.execute_command_provider = Some(options);
                        });
                        notify_server_capabilities_updated(&server, cx);
                    }
                }
                "textDocument/rangeFormatting" => {
                    let options = parse_register_capabilities(reg)?;
                    server.update_capabilities(|capabilities| {
                        capabilities.document_range_formatting_provider = Some(options);
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/formatting" => {
                    let options = parse_register_capabilities(reg)?;
                    server.update_capabilities(|capabilities| {
                        capabilities.document_formatting_provider = Some(options);
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/rename" => {
                    let options = parse_register_capabilities(reg)?;
                    server.update_capabilities(|capabilities| {
                        capabilities.rename_provider = Some(options);
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/documentSymbol" => {
                    let options = parse_register_capabilities(reg)?;
                    server.update_capabilities(|capabilities| {
                        capabilities.document_symbol_provider = Some(options);
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/definition" => {
                    let options = parse_register_capabilities(reg)?;
                    server.update_capabilities(|capabilities| {
                        capabilities.definition_provider = Some(options);
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/hover" => {
                    let options = parse_register_capabilities(reg)?;
                    let provider = match options {
                        OneOf::Left(value) => lsp::HoverProviderCapability::Simple(value),
                        OneOf::Right(caps) => caps,
                    };
                    server.update_capabilities(|capabilities| {
                        capabilities.hover_provider = Some(provider);
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/didChange" => {
                    if let Some(sync_kind) = reg
                        .register_options
                        .and_then(|opts| opts.get("syncKind").cloned())
                        .map(serde_json::from_value::<lsp::TextDocumentSyncKind>)
                        .transpose()?
                    {
                        server.update_capabilities(|capabilities| {
                            let mut sync_options =
                                Self::take_text_document_sync_options(capabilities);
                            sync_options.change = Some(sync_kind);
                            capabilities.text_document_sync =
                                Some(lsp::TextDocumentSyncCapability::Options(sync_options));
                        });
                        notify_server_capabilities_updated(&server, cx);
                    }
                }
                "textDocument/didSave" => {
                    if let Some(include_text) = reg
                        .register_options
                        .map(|opts| {
                            let transpose = opts
                                .get("includeText")
                                .cloned()
                                .map(serde_json::from_value::<Option<bool>>)
                                .transpose();
                            match transpose {
                                Ok(value) => Ok(value.flatten()),
                                Err(e) => Err(e),
                            }
                        })
                        .transpose()?
                    {
                        server.update_capabilities(|capabilities| {
                            let mut sync_options =
                                Self::take_text_document_sync_options(capabilities);
                            sync_options.save =
                                Some(TextDocumentSyncSaveOptions::SaveOptions(lsp::SaveOptions {
                                    include_text,
                                }));
                            capabilities.text_document_sync =
                                Some(lsp::TextDocumentSyncCapability::Options(sync_options));
                        });
                        notify_server_capabilities_updated(&server, cx);
                    }
                }
                "textDocument/documentColor" => {
                    let options = parse_register_capabilities(reg)?;
                    let provider = match options {
                        OneOf::Left(value) => lsp::ColorProviderCapability::Simple(value),
                        OneOf::Right(caps) => caps,
                    };
                    server.update_capabilities(|capabilities| {
                        capabilities.color_provider = Some(provider);
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/foldingRange" => {
                    let options = parse_register_capabilities(reg)?;
                    let provider = match options {
                        OneOf::Left(value) => lsp::FoldingRangeProviderCapability::Simple(value),
                        OneOf::Right(caps) => caps,
                    };
                    server.update_capabilities(|capabilities| {
                        capabilities.folding_range_provider = Some(provider);
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                _ => log::warn!("unhandled capability registration: {reg:?}"),
            }
        }

        Ok(())
    }

    fn unregister_server_capabilities(
        &mut self,
        server_id: LanguageServerId,
        params: lsp::UnregistrationParams,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let server = self
            .language_server_for_id(server_id)
            .with_context(|| format!("no server {server_id} found"))?;
        for unreg in params.unregisterations.iter() {
            match unreg.method.as_str() {
                "workspace/didChangeWatchedFiles" => {
                    let notify = if let Some(local_lsp_store) = self.as_local_mut() {
                        local_lsp_store
                            .on_lsp_unregister_did_change_watched_files(server_id, &unreg.id, cx);
                        true
                    } else {
                        false
                    };
                    if notify {
                        notify_server_capabilities_updated(&server, cx);
                    }
                }
                "workspace/didChangeConfiguration" => {
                    // Ignore payload since we notify clients of setting changes unconditionally, relying on them pulling the latest settings.
                }
                "workspace/didChangeWorkspaceFolders" => {
                    server.update_capabilities(|capabilities| {
                        capabilities
                            .workspace
                            .get_or_insert_with(|| lsp::WorkspaceServerCapabilities {
                                workspace_folders: None,
                                file_operations: None,
                            })
                            .workspace_folders = None;
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "workspace/symbol" => {
                    server.update_capabilities(|capabilities| {
                        capabilities.workspace_symbol_provider = None
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "workspace/fileOperations" => {
                    server.update_capabilities(|capabilities| {
                        capabilities
                            .workspace
                            .get_or_insert_with(|| lsp::WorkspaceServerCapabilities {
                                workspace_folders: None,
                                file_operations: None,
                            })
                            .file_operations = None;
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "workspace/executeCommand" => {
                    server.update_capabilities(|capabilities| {
                        capabilities.execute_command_provider = None;
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/rangeFormatting" => {
                    server.update_capabilities(|capabilities| {
                        capabilities.document_range_formatting_provider = None
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/formatting" => {
                    server.update_capabilities(|capabilities| {
                        capabilities.document_formatting_provider = None;
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/rename" => {
                    server.update_capabilities(|capabilities| capabilities.rename_provider = None);
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/definition" => {
                    server.update_capabilities(|capabilities| {
                        capabilities.definition_provider = None;
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/completion" => {
                    server.update_capabilities(|capabilities| {
                        capabilities.completion_provider = None;
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/hover" => {
                    server.update_capabilities(|capabilities| {
                        capabilities.hover_provider = None;
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/didChange" => {
                    server.update_capabilities(|capabilities| {
                        let mut sync_options = Self::take_text_document_sync_options(capabilities);
                        sync_options.change = None;
                        capabilities.text_document_sync =
                            Some(lsp::TextDocumentSyncCapability::Options(sync_options));
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/didSave" => {
                    server.update_capabilities(|capabilities| {
                        let mut sync_options = Self::take_text_document_sync_options(capabilities);
                        sync_options.save = None;
                        capabilities.text_document_sync =
                            Some(lsp::TextDocumentSyncCapability::Options(sync_options));
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/documentColor" => {
                    server.update_capabilities(|capabilities| {
                        capabilities.color_provider = None;
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                "textDocument/foldingRange" => {
                    server.update_capabilities(|capabilities| {
                        capabilities.folding_range_provider = None;
                    });
                    notify_server_capabilities_updated(&server, cx);
                }
                _ => log::warn!("unhandled capability unregistration: {unreg:?}"),
            }
        }

        Ok(())
    }

    fn take_text_document_sync_options(
        capabilities: &mut lsp::ServerCapabilities,
    ) -> lsp::TextDocumentSyncOptions {
        match capabilities.text_document_sync.take() {
            Some(lsp::TextDocumentSyncCapability::Options(sync_options)) => sync_options,
            Some(lsp::TextDocumentSyncCapability::Kind(sync_kind)) => {
                let mut sync_options = lsp::TextDocumentSyncOptions::default();
                sync_options.change = Some(sync_kind);
                sync_options
            }
            None => lsp::TextDocumentSyncOptions::default(),
        }
    }

    pub fn downstream_client(&self) -> Option<(AnyProtoClient, u64)> {
        self.downstream_client.clone()
    }

    pub fn worktree_store(&self) -> Entity<WorktreeStore> {
        self.worktree_store.clone()
    }

    /// Gets what's stored in the LSP data for the given buffer.
    pub fn current_lsp_data(&mut self, buffer_id: BufferId) -> Option<&mut BufferLspData> {
        self.lsp_data.get_mut(&buffer_id)
    }

    /// Gets the most recent LSP data for the given buffer: if the data is absent or out of date,
    /// new [`BufferLspData`] will be created to replace the previous state.
    pub fn latest_lsp_data(&mut self, buffer: &Entity<Buffer>, cx: &mut App) -> &mut BufferLspData {
        let (buffer_id, buffer_version) =
            buffer.read_with(cx, |buffer, _| (buffer.remote_id(), buffer.version()));
        let lsp_data = self
            .lsp_data
            .entry(buffer_id)
            .or_insert_with(|| BufferLspData::new(buffer, cx));
        if buffer_version.changed_since(&lsp_data.buffer_version) {
            // To send delta requests for semantic tokens, the previous tokens
            // need to be kept between buffer changes.
            let semantic_tokens = lsp_data.semantic_tokens.take();
            *lsp_data = BufferLspData::new(buffer, cx);
            lsp_data.semantic_tokens = semantic_tokens;
        }
        lsp_data
    }
}

// Registration with registerOptions as null, should fallback to true.
// https://github.com/microsoft/vscode-languageserver-node/blob/d90a87f9557a0df9142cfb33e251cfa6fe27d970/client/src/common/client.ts#L2133
fn parse_register_capabilities<T: serde::de::DeserializeOwned>(
    reg: lsp::Registration,
) -> Result<OneOf<bool, T>> {
    Ok(match reg.register_options {
        Some(options) => OneOf::Right(serde_json::from_value::<T>(options)?),
        None => OneOf::Left(true),
    })
}

fn subscribe_to_binary_statuses(
    languages: &Arc<LanguageRegistry>,
    cx: &mut Context<'_, LspStore>,
) -> Task<()> {
    let mut server_statuses = languages.language_server_binary_statuses();
    cx.spawn(async move |lsp_store, cx| {
        while let Some((server_name, binary_status)) = server_statuses.next().await {
            if lsp_store
                .update(cx, |_, cx| {
                    let _binary_status = match binary_status {
                        BinaryStatus::None => proto::ServerBinaryStatus::None,
                        BinaryStatus::CheckingForUpdate => {
                            proto::ServerBinaryStatus::CheckingForUpdate
                        }
                        BinaryStatus::Downloading => proto::ServerBinaryStatus::Downloading,
                        BinaryStatus::Starting => proto::ServerBinaryStatus::Starting,
                        BinaryStatus::Stopping => proto::ServerBinaryStatus::Stopping,
                        BinaryStatus::Stopped => proto::ServerBinaryStatus::Stopped,
                        BinaryStatus::Failed { error: _ } => {
                            proto::ServerBinaryStatus::Failed
                        }
                    };
                    cx.emit(LspStoreEvent::LanguageServerUpdate {
                        // Binary updates are about the binary that might not have any language server id at that point.
                        // Reuse `LanguageServerUpdate` for them and provide a fake id that won't be used on the receiver side.
                        language_server_id: LanguageServerId(0),
                        name: Some(server_name),
                    });
                })
                .is_err()
            {
                break;
            }
        }
    })
}

impl EventEmitter<LspStoreEvent> for LspStore {}

fn remove_empty_hover_blocks(mut hover: Hover) -> Option<Hover> {
    hover
        .contents
        .retain(|hover_block| !hover_block.text.trim().is_empty());
    if hover.contents.is_empty() {
        None
    } else {
        Some(hover)
    }
}

#[derive(Debug)]
pub enum LanguageServerToQuery {
    /// Query language servers in order of users preference, up until one capable of handling the request is found.
    FirstCapable,
    /// Query a specific language server.
    Other(LanguageServerId),
}

#[derive(Default)]
struct RenamePathsWatchedForServer {
    did_rename: Vec<RenameActionPredicate>,
    will_rename: Vec<RenameActionPredicate>,
}

impl RenamePathsWatchedForServer {
    fn with_did_rename_patterns(
        mut self,
        did_rename: Option<&FileOperationRegistrationOptions>,
    ) -> Self {
        if let Some(did_rename) = did_rename {
            self.did_rename = did_rename
                .filters
                .iter()
                .filter_map(|filter| filter.try_into().log_err())
                .collect();
        }
        self
    }
    fn with_will_rename_patterns(
        mut self,
        will_rename: Option<&FileOperationRegistrationOptions>,
    ) -> Self {
        if let Some(will_rename) = will_rename {
            self.will_rename = will_rename
                .filters
                .iter()
                .filter_map(|filter| filter.try_into().log_err())
                .collect();
        }
        self
    }

    fn should_send_did_rename(&self, path: &str, is_dir: bool) -> bool {
        self.did_rename.iter().any(|pred| pred.eval(path, is_dir))
    }
    fn should_send_will_rename(&self, path: &str, is_dir: bool) -> bool {
        self.will_rename.iter().any(|pred| pred.eval(path, is_dir))
    }
}

impl TryFrom<&FileOperationFilter> for RenameActionPredicate {
    type Error = globset::Error;
    fn try_from(ops: &FileOperationFilter) -> Result<Self, globset::Error> {
        Ok(Self {
            kind: ops.pattern.matches.clone(),
            glob: GlobBuilder::new(&ops.pattern.glob)
                .case_insensitive(
                    ops.pattern
                        .options
                        .as_ref()
                        .is_some_and(|ops| ops.ignore_case.unwrap_or(false)),
                )
                .build()?
                .compile_matcher(),
        })
    }
}
struct RenameActionPredicate {
    glob: GlobMatcher,
    kind: Option<FileOperationPatternKind>,
}

impl RenameActionPredicate {
    // Returns true if language server should be notified
    fn eval(&self, path: &str, is_dir: bool) -> bool {
        self.kind.as_ref().is_none_or(|kind| {
            let expected_kind = if is_dir {
                FileOperationPatternKind::Folder
            } else {
                FileOperationPatternKind::File
            };
            kind == &expected_kind
        }) && self.glob.is_match(path)
    }
}

#[derive(Default)]
struct LanguageServerWatchedPaths {
    worktree_paths: HashMap<WorktreeId, GlobSet>,
    abs_paths: HashMap<Arc<Path>, (GlobSet, Task<()>)>,
}

#[derive(Default)]
struct LanguageServerWatchedPathsBuilder {
    worktree_paths: HashMap<WorktreeId, GlobSet>,
    abs_paths: HashMap<Arc<Path>, GlobSet>,
}

impl LanguageServerWatchedPathsBuilder {
    fn watch_worktree(&mut self, worktree_id: WorktreeId, glob_set: GlobSet) {
        self.worktree_paths.insert(worktree_id, glob_set);
    }
    fn watch_abs_path(&mut self, path: Arc<Path>, glob_set: GlobSet) {
        self.abs_paths.insert(path, glob_set);
    }
    fn build(
        self,
        fs: Arc<dyn Fs>,
        language_server_id: LanguageServerId,
        cx: &mut Context<LspStore>,
    ) -> LanguageServerWatchedPaths {
        let lsp_store = cx.weak_entity();

        const LSP_ABS_PATH_OBSERVE: Duration = Duration::from_millis(100);
        let abs_paths = self
            .abs_paths
            .into_iter()
            .map(|(abs_path, globset)| {
                let task = cx.spawn({
                    let abs_path = abs_path.clone();
                    let fs = fs.clone();

                    let lsp_store = lsp_store.clone();
                    async move |_, cx| {
                        maybe!(async move {
                            let mut push_updates = fs.watch(&abs_path, LSP_ABS_PATH_OBSERVE).await;
                            while let Some(update) = push_updates.0.next().await {
                                let action = lsp_store
                                    .update(cx, |this, _| {
                                        let Some(local) = this.as_local() else {
                                            return ControlFlow::Break(());
                                        };
                                        let Some(watcher) = local
                                            .language_server_watched_paths
                                            .get(&language_server_id)
                                        else {
                                            return ControlFlow::Break(());
                                        };
                                        let (globs, _) = watcher.abs_paths.get(&abs_path).expect(
                                            "Watched abs path is not registered with a watcher",
                                        );
                                        let matching_entries = update
                                            .into_iter()
                                            .filter(|event| globs.is_match(&event.path))
                                            .collect::<Vec<_>>();
                                        this.lsp_notify_abs_paths_changed(
                                            language_server_id,
                                            matching_entries,
                                        );
                                        ControlFlow::Continue(())
                                    })
                                    .ok()?;

                                if action.is_break() {
                                    break;
                                }
                            }
                            Some(())
                        })
                        .await;
                    }
                });
                (abs_path, (globset, task))
            })
            .collect();
        LanguageServerWatchedPaths {
            worktree_paths: self.worktree_paths,
            abs_paths,
        }
    }
}

struct LspBufferSnapshot {
    version: i32,
    snapshot: TextBufferSnapshot,
}

/// A prompt requested by LSP server.
#[derive(Clone, Debug)]
pub struct LanguageServerPromptRequest {
    pub id: usize,
    pub level: PromptLevel,
    pub message: String,
    pub actions: Vec<MessageActionItem>,
    pub lsp_name: String,
    pub(crate) response_channel: async_channel::Sender<MessageActionItem>,
}

impl LanguageServerPromptRequest {
    pub fn new(
        level: PromptLevel,
        message: String,
        actions: Vec<MessageActionItem>,
        lsp_name: String,
        response_channel: async_channel::Sender<MessageActionItem>,
    ) -> Self {
        let id = NEXT_PROMPT_REQUEST_ID.fetch_add(1, atomic::Ordering::AcqRel);
        LanguageServerPromptRequest {
            id,
            level,
            message,
            actions,
            lsp_name,
            response_channel,
        }
    }
    pub async fn respond(self, index: usize) -> Option<()> {
        if let Some(response) = self.actions.into_iter().nth(index) {
            self.response_channel.send(response).await.ok()
        } else {
            None
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test(
        level: PromptLevel,
        message: String,
        actions: Vec<MessageActionItem>,
        lsp_name: String,
    ) -> Self {
        let (tx, _rx) = async_channel::unbounded();
        LanguageServerPromptRequest::new(level, message, actions, lsp_name, tx)
    }
}
impl PartialEq for LanguageServerPromptRequest {
    fn eq(&self, other: &Self) -> bool {
        self.message == other.message && self.actions == other.actions
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LanguageServerLogType {
    Log(MessageType),
    Trace { verbose_info: Option<String> },
    Rpc { received: bool },
}

pub enum LanguageServerState {
    Starting {
        startup: Task<Option<Arc<LanguageServer>>>,
        /// List of language servers that will be added to the workspace once it's initialization completes.
        pending_workspace_folders: Arc<Mutex<BTreeSet<Uri>>>,
    },

    Running {
        adapter: Arc<CachedLspAdapter>,
        server: Arc<LanguageServer>,
    },
}

impl LanguageServerState {
    fn add_workspace_folder(&self, uri: Uri) {
        match self {
            LanguageServerState::Starting {
                pending_workspace_folders,
                ..
            } => {
                pending_workspace_folders.lock().insert(uri);
            }
            LanguageServerState::Running { server, .. } => {
                server.add_workspace_folder(uri);
            }
        }
    }
    fn _remove_workspace_folder(&self, uri: Uri) {
        match self {
            LanguageServerState::Starting {
                pending_workspace_folders,
                ..
            } => {
                pending_workspace_folders.lock().remove(&uri);
            }
            LanguageServerState::Running { server, .. } => server.remove_workspace_folder(uri),
        }
    }
}

impl std::fmt::Debug for LanguageServerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LanguageServerState::Starting { .. } => {
                f.debug_struct("LanguageServerState::Starting").finish()
            }
            LanguageServerState::Running { .. } => {
                f.debug_struct("LanguageServerState::Running").finish()
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LanguageServerProgress {
    pub is_cancellable: bool,
    pub title: Option<String>,
    pub message: Option<String>,
    pub percentage: Option<usize>,
    #[serde(skip_serializing)]
    pub last_update_at: Instant,
}

#[derive(Clone, Debug)]
pub enum CompletionDocumentation {
    /// There is no documentation for this completion.
    Undocumented,
    /// A single line of documentation.
    SingleLine(SharedString),
    /// Multiple lines of plain text documentation.
    MultiLinePlainText(SharedString),
    /// Markdown documentation.
    MultiLineMarkdown(SharedString),
    /// Both single line and multiple lines of plain text documentation.
    SingleLineAndMultiLinePlainText {
        single_line: SharedString,
        plain_text: Option<SharedString>,
    },
}

impl CompletionDocumentation {
    #[cfg(any(test, feature = "test-support"))]
    pub fn text(&self) -> SharedString {
        match self {
            CompletionDocumentation::Undocumented => "".into(),
            CompletionDocumentation::SingleLine(s) => s.clone(),
            CompletionDocumentation::MultiLinePlainText(s) => s.clone(),
            CompletionDocumentation::MultiLineMarkdown(s) => s.clone(),
            CompletionDocumentation::SingleLineAndMultiLinePlainText { single_line, .. } => {
                single_line.clone()
            }
        }
    }
}

impl From<lsp::Documentation> for CompletionDocumentation {
    fn from(docs: lsp::Documentation) -> Self {
        match docs {
            lsp::Documentation::String(text) => {
                if text.lines().count() <= 1 {
                    CompletionDocumentation::SingleLine(text.trim().to_string().into())
                } else {
                    CompletionDocumentation::MultiLinePlainText(text.into())
                }
            }

            lsp::Documentation::MarkupContent(lsp::MarkupContent { kind, value }) => match kind {
                lsp::MarkupKind::PlainText => {
                    if value.lines().count() <= 1 {
                        CompletionDocumentation::SingleLine(value.into())
                    } else {
                        CompletionDocumentation::MultiLinePlainText(value.into())
                    }
                }

                lsp::MarkupKind::Markdown => {
                    CompletionDocumentation::MultiLineMarkdown(value.into())
                }
            },
        }
    }
}

pub enum ResolvedHint {
    Resolving(Shared<Task<()>>),
}

pub fn glob_literal_prefix(glob: &Path) -> PathBuf {
    glob.components()
        .take_while(|component| match component {
            path::Component::Normal(part) => !part.to_string_lossy().contains(['*', '?', '{', '}']),
            _ => true,
        })
        .collect()
}

pub struct SshLspAdapter {
    name: LanguageServerName,
    binary: LanguageServerBinary,
    initialization_options: Option<String>,
}

impl SshLspAdapter {
    pub fn new(
        name: LanguageServerName,
        binary: LanguageServerBinary,
        initialization_options: Option<String>,
    ) -> Self {
        Self {
            name,
            binary,
            initialization_options,
        }
    }
}

impl LspInstaller for SshLspAdapter {
    type BinaryVersion = ();
    async fn check_if_user_installed(
        &self,
        _: &Arc<dyn LspAdapterDelegate>,
        _: Option<Toolchain>,
        _: &AsyncApp,
    ) -> Option<LanguageServerBinary> {
        Some(self.binary.clone())
    }

    async fn cached_server_binary(
        &self,
        _: PathBuf,
        _: &dyn LspAdapterDelegate,
    ) -> Option<LanguageServerBinary> {
        None
    }

    async fn fetch_latest_server_version(
        &self,
        _: &Arc<dyn LspAdapterDelegate>,
        _: bool,
        _: &mut AsyncApp,
    ) -> Result<()> {
        anyhow::bail!("SshLspAdapter does not support fetch_latest_server_version")
    }

    fn fetch_server_binary(
        &self,
        _: (),
        _: PathBuf,
        _: &Arc<dyn LspAdapterDelegate>,
    ) -> impl Send + Future<Output = Result<LanguageServerBinary>> + use<> {
        async { anyhow::bail!("SshLspAdapter does not support fetch_server_binary") }
    }
}

#[async_trait(?Send)]
impl LspAdapter for SshLspAdapter {
    fn name(&self) -> LanguageServerName {
        self.name.clone()
    }

    async fn initialization_options(
        self: Arc<Self>,
        _: &Arc<dyn LspAdapterDelegate>,
        _: &mut AsyncApp,
    ) -> Result<Option<serde_json::Value>> {
        let Some(options) = &self.initialization_options else {
            return Ok(None);
        };
        let result = serde_json::from_str(options)?;
        Ok(result)
    }
}

pub fn language_server_settings<'a>(
    delegate: &'a dyn LspAdapterDelegate,
    language: &LanguageServerName,
    cx: &'a App,
) -> Option<&'a LspSettings> {
    language_server_settings_for(
        SettingsLocation {
            worktree_id: delegate.worktree_id(),
            path: RelPath::empty(),
        },
        language,
        cx,
    )
}

pub fn language_server_settings_for<'a>(
    location: SettingsLocation<'a>,
    language: &LanguageServerName,
    cx: &'a App,
) -> Option<&'a LspSettings> {
    ProjectSettings::get(Some(location), cx).lsp.get(language)
}

pub struct LocalLspAdapterDelegate {
    lsp_store: WeakEntity<LspStore>,
    worktree: worktree::Snapshot,
    fs: Arc<dyn Fs>,
    http_client: Arc<dyn HttpClient>,
    language_registry: Arc<LanguageRegistry>,
    load_shell_env_task: Shared<Task<Option<HashMap<String, String>>>>,
}

impl LocalLspAdapterDelegate {
    pub fn new(
        language_registry: Arc<LanguageRegistry>,
        environment: &Entity<ProjectEnvironment>,
        lsp_store: WeakEntity<LspStore>,
        worktree: &Entity<Worktree>,
        http_client: Arc<dyn HttpClient>,
        fs: Arc<dyn Fs>,
        cx: &mut App,
    ) -> Arc<Self> {
        let load_shell_env_task =
            environment.update(cx, |env, cx| env.worktree_environment(worktree.clone(), cx));

        Arc::new(Self {
            lsp_store,
            worktree: worktree.read(cx).snapshot(),
            fs,
            http_client,
            language_registry,
            load_shell_env_task,
        })
    }

    pub fn from_local_lsp(
        local: &LocalLspStore,
        worktree: &Entity<Worktree>,
        cx: &mut App,
    ) -> Arc<Self> {
        Self::new(
            local.languages.clone(),
            &local.environment,
            local.weak.clone(),
            worktree,
            local.http_client.clone(),
            local.fs.clone(),
            cx,
        )
    }
}

#[async_trait]
impl LspAdapterDelegate for LocalLspAdapterDelegate {
    fn show_notification(&self, message: &str, cx: &mut App) {
        self.lsp_store
            .update(cx, |_, cx| {
                cx.emit(LspStoreEvent::Notification(message.to_owned()))
            })
            .ok();
    }

    fn http_client(&self) -> Arc<dyn HttpClient> {
        self.http_client.clone()
    }

    fn worktree_id(&self) -> WorktreeId {
        self.worktree.id()
    }

    fn worktree_root_path(&self) -> &Path {
        self.worktree.abs_path().as_ref()
    }

    fn resolve_relative_path(&self, path: PathBuf) -> PathBuf {
        self.worktree.resolve_relative_path(path)
    }

    async fn shell_env(&self) -> HashMap<String, String> {
        let task = self.load_shell_env_task.clone();
        task.await.unwrap_or_default()
    }

    async fn which(&self, command: &OsStr) -> Option<PathBuf> {
        let mut worktree_abs_path = self.worktree_root_path().to_path_buf();
        if self.fs.is_file(&worktree_abs_path).await {
            worktree_abs_path.pop();
        }

        let env = self.shell_env().await;

        let shell_path = env.get("PATH").cloned();

        which::which_in(command, shell_path.as_ref(), worktree_abs_path).ok()
    }

    async fn try_exec(&self, command: LanguageServerBinary) -> Result<()> {
        let mut working_dir = self.worktree_root_path().to_path_buf();
        if self.fs.is_file(&working_dir).await {
            working_dir.pop();
        }
        let output = util::command::new_command(&command.path)
            .args(command.arguments)
            .envs(command.env.clone().unwrap_or_default())
            .current_dir(working_dir)
            .output()
            .await?;

        anyhow::ensure!(
            output.status.success(),
            "{}, stdout: {:?}, stderr: {:?}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    fn update_status(&self, server_name: LanguageServerName, status: language::BinaryStatus) {
        self.language_registry
            .update_lsp_binary_status(server_name, status);
    }

    fn registered_lsp_adapters(&self) -> Vec<Arc<dyn LspAdapter>> {
        self.language_registry
            .all_lsp_adapters()
            .into_iter()
            .map(|adapter| adapter.adapter.clone() as Arc<dyn LspAdapter>)
            .collect()
    }

    async fn read_text_file(&self, path: &RelPath) -> Result<String> {
        let entry = self
            .worktree
            .entry_for_path(path)
            .with_context(|| format!("no worktree entry for path {path:?}"))?;
        let abs_path = self.worktree.absolutize(&entry.path);
        self.fs.load(&abs_path).await
    }
}

pub(crate) fn collapse_newlines(text: &str, separator: &str) -> String {
    text.lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .join(separator)
}

fn include_text(server: &lsp::LanguageServer) -> Option<bool> {
    match server.capabilities().text_document_sync.as_ref()? {
        lsp::TextDocumentSyncCapability::Options(opts) => match opts.save.as_ref()? {
            // Server wants didSave but didn't specify includeText.
            lsp::TextDocumentSyncSaveOptions::Supported(true) => Some(false),
            // Server doesn't want didSave at all.
            lsp::TextDocumentSyncSaveOptions::Supported(false) => None,
            // Server provided SaveOptions.
            lsp::TextDocumentSyncSaveOptions::SaveOptions(save_options) => {
                Some(save_options.include_text.unwrap_or(false))
            }
        },
        // We do not have any save info. Kind affects didChange only.
        lsp::TextDocumentSyncCapability::Kind(_) => None,
    }
}

/// Completion items are displayed in a `UniformList`.
/// Usually, those items are single-line strings, but in LSP responses,
/// completion items `label`, `detail` and `label_details.description` may contain newlines or long spaces.
/// Many language plugins construct these items by joining these parts together, and we may use `CodeLabel::fallback_for_completion` that uses `label` at least.
/// All that may lead to a newline being inserted into resulting `CodeLabel.text`, which will force `UniformList` to bloat each entry to occupy more space,
/// breaking the completions menu presentation.
///
/// Sanitize the text to ensure there are no newlines, or, if there are some, remove them and also remove long space sequences if there were newlines.
pub fn ensure_uniform_list_compatible_label(label: &mut CodeLabel) {
    let mut new_text = String::with_capacity(label.text.len());
    let mut offset_map = vec![0; label.text.len() + 1];
    let mut last_char_was_space = false;
    let mut new_idx = 0;
    let chars = label.text.char_indices().fuse();
    let mut newlines_removed = false;

    for (idx, c) in chars {
        offset_map[idx] = new_idx;

        match c {
            '\n' if last_char_was_space => {
                newlines_removed = true;
            }
            '\t' | ' ' if last_char_was_space => {}
            '\n' if !last_char_was_space => {
                new_text.push(' ');
                new_idx += 1;
                last_char_was_space = true;
                newlines_removed = true;
            }
            ' ' | '\t' => {
                new_text.push(' ');
                new_idx += 1;
                last_char_was_space = true;
            }
            _ => {
                new_text.push(c);
                new_idx += c.len_utf8();
                last_char_was_space = false;
            }
        }
    }
    offset_map[label.text.len()] = new_idx;

    // Only modify the label if newlines were removed.
    if !newlines_removed {
        return;
    }

    let last_index = new_idx;
    let mut run_ranges_errors = Vec::new();
    label.runs.retain_mut(|(range, _)| {
        match offset_map.get(range.start) {
            Some(&start) => range.start = start,
            None => {
                run_ranges_errors.push(range.clone());
                return false;
            }
        }

        match offset_map.get(range.end) {
            Some(&end) => range.end = end,
            None => {
                run_ranges_errors.push(range.clone());
                range.end = last_index;
            }
        }
        true
    });
    if !run_ranges_errors.is_empty() {
        log::error!(
            "Completion label has errors in its run ranges: {run_ranges_errors:?}, label text: {}",
            label.text
        );
    }

    let mut wrong_filter_range = None;
    if label.filter_range == (0..label.text.len()) {
        label.filter_range = 0..new_text.len();
    } else {
        let mut original_filter_range = Some(label.filter_range.clone());
        match offset_map.get(label.filter_range.start) {
            Some(&start) => label.filter_range.start = start,
            None => {
                wrong_filter_range = original_filter_range.take();
                label.filter_range.start = last_index;
            }
        }

        match offset_map.get(label.filter_range.end) {
            Some(&end) => label.filter_range.end = end,
            None => {
                wrong_filter_range = original_filter_range.take();
                label.filter_range.end = last_index;
            }
        }
    }
    if let Some(wrong_filter_range) = wrong_filter_range {
        log::error!(
            "Completion label has an invalid filter range: {wrong_filter_range:?}, label text: {}",
            label.text
        );
    }

    label.text = new_text;
}
