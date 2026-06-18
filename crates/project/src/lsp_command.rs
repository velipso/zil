use crate::{
    DocumentColor,
    DocumentHighlight, DocumentSymbol, Hover, HoverBlock, HoverBlockKind,
    Location,
    LocationLink, PrepareRenameResponse,
    ProjectTransaction,
    lsp_store::{LocalLspStore, LspFoldingRange, LspStore},
};
use anyhow::{Context as _, Result};
use async_trait::async_trait;
use client::proto::{self, PeerId};
use gpui::{App, AsyncApp, Entity, SharedString, Task, TaskExt, prelude::FluentBuilder};
use language::{
    Anchor, Bias, Buffer, CachedLspAdapter,
    PointUtf16, ToPointUtf16, Unclipped,
    language_settings::{LanguageSettings},
    point_from_lsp, point_to_lsp,
    proto::{
        deserialize_anchor, deserialize_anchor_range, deserialize_version, serialize_anchor,
        serialize_anchor_range, serialize_version,
    },
    range_from_lsp,
};
use lsp::{
    AdapterServerCapabilities,
    DocumentHighlightKind, LanguageServer, LanguageServerId, LinkedEditingRangeServerCapabilities,
    OneOf, RenameOptions, ServerCapabilities,
};

use std::{
    cmp::Reverse, mem, ops::Range, path::Path, sync::Arc,
};
use text::{BufferId};

pub fn lsp_formatting_options(settings: &LanguageSettings) -> lsp::FormattingOptions {
    lsp::FormattingOptions {
        tab_size: settings.tab_size.into(),
        insert_spaces: !settings.hard_tabs,
        trim_trailing_whitespace: Some(settings.remove_trailing_whitespace_on_save),
        trim_final_newlines: Some(settings.ensure_final_newline_on_save),
        insert_final_newline: Some(settings.ensure_final_newline_on_save),
        ..lsp::FormattingOptions::default()
    }
}

pub fn file_path_to_lsp_url(path: &Path) -> Result<lsp::Uri> {
    match lsp::Uri::from_file_path(path) {
        Ok(url) => Ok(url),
        Err(()) => anyhow::bail!("Invalid file path provided to LSP request: {path:?}"),
    }
}

pub(crate) fn make_text_document_identifier(path: &Path) -> Result<lsp::TextDocumentIdentifier> {
    Ok(lsp::TextDocumentIdentifier {
        uri: file_path_to_lsp_url(path)?,
    })
}

pub(crate) fn make_lsp_text_document_position(
    path: &Path,
    position: PointUtf16,
) -> Result<lsp::TextDocumentPositionParams> {
    Ok(lsp::TextDocumentPositionParams {
        text_document: make_text_document_identifier(path)?,
        position: point_to_lsp(position),
    })
}

#[async_trait(?Send)]
pub trait LspCommand: 'static + Sized + Send + std::fmt::Debug {
    type Response: 'static + Default + Send + std::fmt::Debug;
    type LspRequest: 'static + Send + lsp::request::Request;
    type ProtoRequest: 'static + Send + proto::RequestMessage;

    fn display_name(&self) -> &str;

    fn status(&self) -> Option<String> {
        None
    }

    fn to_lsp_params_or_response(
        &self,
        path: &Path,
        buffer: &Buffer,
        language_server: &Arc<LanguageServer>,
        cx: &App,
    ) -> Result<
        LspParamsOrResponse<<Self::LspRequest as lsp::request::Request>::Params, Self::Response>,
    > {
        if self.check_capabilities(language_server.adapter_server_capabilities()) {
            Ok(LspParamsOrResponse::Params(self.to_lsp(
                path,
                buffer,
                language_server,
                cx,
            )?))
        } else {
            Ok(LspParamsOrResponse::Response(Default::default()))
        }
    }

    /// When false, `to_lsp_params_or_response` default implementation will return the default response.
    fn check_capabilities(&self, _: AdapterServerCapabilities) -> bool;

    fn to_lsp(
        &self,
        path: &Path,
        buffer: &Buffer,
        language_server: &Arc<LanguageServer>,
        cx: &App,
    ) -> Result<<Self::LspRequest as lsp::request::Request>::Params>;

    async fn response_from_lsp(
        self,
        message: <Self::LspRequest as lsp::request::Request>::Result,
        lsp_store: Entity<LspStore>,
        buffer: Entity<Buffer>,
        server_id: LanguageServerId,
        cx: AsyncApp,
    ) -> Result<Self::Response>;

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> Self::ProtoRequest;

    async fn from_proto(
        message: Self::ProtoRequest,
        lsp_store: Entity<LspStore>,
        buffer: Entity<Buffer>,
        cx: AsyncApp,
    ) -> Result<Self>;

    fn response_to_proto(
        response: Self::Response,
        lsp_store: &mut LspStore,
        peer_id: PeerId,
        buffer_version: &clock::Global,
        cx: &mut App,
    ) -> <Self::ProtoRequest as proto::RequestMessage>::Response;

    async fn response_from_proto(
        self,
        message: <Self::ProtoRequest as proto::RequestMessage>::Response,
        lsp_store: Entity<LspStore>,
        buffer: Entity<Buffer>,
        cx: AsyncApp,
    ) -> Result<Self::Response>;

    fn buffer_id_from_proto(message: &Self::ProtoRequest) -> Result<BufferId>;
}

pub enum LspParamsOrResponse<P, R> {
    Params(P),
    Response(R),
}

#[derive(Debug)]
pub(crate) struct PrepareRename {
    pub position: PointUtf16,
}

#[derive(Debug)]
pub(crate) struct PerformRename {
    pub position: PointUtf16,
    pub new_name: String,
    pub push_to_history: bool,
}

#[derive(Debug)]
pub(crate) struct GetDocumentHighlights {
    pub position: PointUtf16,
}

#[derive(Debug, Copy, Clone)]
pub(crate) struct GetDocumentSymbols;

#[derive(Clone, Debug)]
pub(crate) struct GetHover {
    pub position: PointUtf16,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SemanticTokensFull {
    pub for_server: Option<LanguageServerId>,
}

#[derive(Debug, Clone)]
pub(crate) struct SemanticTokensDelta {
    pub previous_result_id: SharedString,
}

#[derive(Debug)]
pub(crate) enum SemanticTokensResponse {
    Full {
        data: Vec<u32>,
        result_id: Option<SharedString>,
    },
    Delta {
        edits: Vec<SemanticTokensEdit>,
        result_id: Option<SharedString>,
    },
}

impl Default for SemanticTokensResponse {
    fn default() -> Self {
        Self::Delta {
            edits: Vec::new(),
            result_id: None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct SemanticTokensEdit {
    pub start: u32,
    pub delete_count: u32,
    pub data: Vec<u32>,
}

#[derive(Debug, Copy, Clone)]
pub(crate) struct GetDocumentColor;

#[derive(Debug, Copy, Clone)]
pub(crate) struct GetFoldingRanges;

#[derive(Debug)]
pub(crate) struct LinkedEditingRange {
    pub position: Anchor,
}

#[async_trait(?Send)]
impl LspCommand for PrepareRename {
    type Response = PrepareRenameResponse;
    type LspRequest = lsp::request::PrepareRenameRequest;
    type ProtoRequest = proto::PrepareRename;

    fn display_name(&self) -> &str {
        "Prepare rename"
    }

    fn check_capabilities(&self, capabilities: AdapterServerCapabilities) -> bool {
        capabilities
            .server_capabilities
            .rename_provider
            .is_some_and(|capability| match capability {
                OneOf::Left(enabled) => enabled,
                OneOf::Right(options) => options.prepare_provider.unwrap_or(false),
            })
    }

    fn to_lsp_params_or_response(
        &self,
        path: &Path,
        buffer: &Buffer,
        language_server: &Arc<LanguageServer>,
        cx: &App,
    ) -> Result<LspParamsOrResponse<lsp::TextDocumentPositionParams, PrepareRenameResponse>> {
        let rename_provider = language_server
            .adapter_server_capabilities()
            .server_capabilities
            .rename_provider;
        match rename_provider {
            Some(lsp::OneOf::Right(RenameOptions {
                prepare_provider: Some(true),
                ..
            })) => Ok(LspParamsOrResponse::Params(self.to_lsp(
                path,
                buffer,
                language_server,
                cx,
            )?)),
            Some(lsp::OneOf::Right(_)) => Ok(LspParamsOrResponse::Response(
                PrepareRenameResponse::OnlyUnpreparedRenameSupported,
            )),
            Some(lsp::OneOf::Left(true)) => Ok(LspParamsOrResponse::Response(
                PrepareRenameResponse::OnlyUnpreparedRenameSupported,
            )),
            _ => anyhow::bail!("Rename not supported"),
        }
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<lsp::TextDocumentPositionParams> {
        make_lsp_text_document_position(path, self.position)
    }

    async fn response_from_lsp(
        self,
        message: Option<lsp::PrepareRenameResponse>,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        _: LanguageServerId,
        cx: AsyncApp,
    ) -> Result<PrepareRenameResponse> {
        buffer.read_with(&cx, |buffer, _| match message {
            Some(lsp::PrepareRenameResponse::Range(range))
            | Some(lsp::PrepareRenameResponse::RangeWithPlaceholder { range, .. }) => {
                let Range { start, end } = range_from_lsp(range);
                if buffer.clip_point_utf16(start, Bias::Left) == start.0
                    && buffer.clip_point_utf16(end, Bias::Left) == end.0
                {
                    Ok(PrepareRenameResponse::Success(
                        buffer.anchor_after(start)..buffer.anchor_before(end),
                    ))
                } else {
                    Ok(PrepareRenameResponse::InvalidPosition)
                }
            }
            Some(lsp::PrepareRenameResponse::DefaultBehavior { .. }) => {
                let snapshot = buffer.snapshot();
                let (range, _) = snapshot.surrounding_word(self.position, None);
                let range = snapshot.anchor_after(range.start)..snapshot.anchor_before(range.end);
                Ok(PrepareRenameResponse::Success(range))
            }
            None => Ok(PrepareRenameResponse::InvalidPosition),
        })
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> proto::PrepareRename {
        proto::PrepareRename {
            project_id,
            buffer_id: buffer.remote_id().into(),
            position: Some(language::proto::serialize_anchor(
                &buffer.anchor_before(self.position),
            )),
            version: serialize_version(&buffer.version()),
        }
    }

    async fn from_proto(
        message: proto::PrepareRename,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<Self> {
        let position = message
            .position
            .and_then(deserialize_anchor)
            .context("invalid position")?;
        buffer
            .update(&mut cx, |buffer, _| {
                buffer.wait_for_version(deserialize_version(&message.version))
            })
            .await?;

        Ok(Self {
            position: buffer.read_with(&cx, |buffer, _| position.to_point_utf16(buffer)),
        })
    }

    fn response_to_proto(
        response: PrepareRenameResponse,
        _: &mut LspStore,
        _: PeerId,
        buffer_version: &clock::Global,
        _: &mut App,
    ) -> proto::PrepareRenameResponse {
        match response {
            PrepareRenameResponse::Success(range) => proto::PrepareRenameResponse {
                can_rename: true,
                only_unprepared_rename_supported: false,
                start: Some(language::proto::serialize_anchor(&range.start)),
                end: Some(language::proto::serialize_anchor(&range.end)),
                version: serialize_version(buffer_version),
            },
            PrepareRenameResponse::OnlyUnpreparedRenameSupported => proto::PrepareRenameResponse {
                can_rename: false,
                only_unprepared_rename_supported: true,
                start: None,
                end: None,
                version: vec![],
            },
            PrepareRenameResponse::InvalidPosition => proto::PrepareRenameResponse {
                can_rename: false,
                only_unprepared_rename_supported: false,
                start: None,
                end: None,
                version: vec![],
            },
        }
    }

    async fn response_from_proto(
        self,
        message: proto::PrepareRenameResponse,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<PrepareRenameResponse> {
        if message.can_rename {
            buffer
                .update(&mut cx, |buffer, _| {
                    buffer.wait_for_version(deserialize_version(&message.version))
                })
                .await?;
            if let (Some(start), Some(end)) = (
                message.start.and_then(deserialize_anchor),
                message.end.and_then(deserialize_anchor),
            ) {
                Ok(PrepareRenameResponse::Success(start..end))
            } else {
                anyhow::bail!(
                    "Missing start or end position in remote project PrepareRenameResponse"
                );
            }
        } else if message.only_unprepared_rename_supported {
            Ok(PrepareRenameResponse::OnlyUnpreparedRenameSupported)
        } else {
            Ok(PrepareRenameResponse::InvalidPosition)
        }
    }

    fn buffer_id_from_proto(message: &proto::PrepareRename) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

#[async_trait(?Send)]
impl LspCommand for PerformRename {
    type Response = ProjectTransaction;
    type LspRequest = lsp::request::Rename;
    type ProtoRequest = proto::PerformRename;

    fn display_name(&self) -> &str {
        "Rename"
    }

    fn check_capabilities(&self, capabilities: AdapterServerCapabilities) -> bool {
        capabilities
            .server_capabilities
            .rename_provider
            .is_some_and(|capability| match capability {
                OneOf::Left(enabled) => enabled,
                OneOf::Right(_) => true,
            })
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<lsp::RenameParams> {
        Ok(lsp::RenameParams {
            text_document_position: make_lsp_text_document_position(path, self.position)?,
            new_name: self.new_name.clone(),
            work_done_progress_params: Default::default(),
        })
    }

    async fn response_from_lsp(
        self,
        message: Option<lsp::WorkspaceEdit>,
        lsp_store: Entity<LspStore>,
        buffer: Entity<Buffer>,
        server_id: LanguageServerId,
        mut cx: AsyncApp,
    ) -> Result<ProjectTransaction> {
        if let Some(edit) = message {
            let (_, lsp_server) =
                language_server_for_buffer(&lsp_store, &buffer, server_id, &mut cx)?;
            LocalLspStore::deserialize_workspace_edit(
                lsp_store,
                edit,
                self.push_to_history,
                lsp_server,
                &mut cx,
            )
            .await
        } else {
            Ok(ProjectTransaction::default())
        }
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> proto::PerformRename {
        proto::PerformRename {
            project_id,
            buffer_id: buffer.remote_id().into(),
            position: Some(language::proto::serialize_anchor(
                &buffer.anchor_before(self.position),
            )),
            new_name: self.new_name.clone(),
            version: serialize_version(&buffer.version()),
        }
    }

    async fn from_proto(
        message: proto::PerformRename,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<Self> {
        let position = message
            .position
            .and_then(deserialize_anchor)
            .context("invalid position")?;
        buffer
            .update(&mut cx, |buffer, _| {
                buffer.wait_for_version(deserialize_version(&message.version))
            })
            .await?;
        Ok(Self {
            position: buffer.read_with(&cx, |buffer, _| position.to_point_utf16(buffer)),
            new_name: message.new_name,
            push_to_history: false,
        })
    }

    fn response_to_proto(
        response: ProjectTransaction,
        lsp_store: &mut LspStore,
        peer_id: PeerId,
        _: &clock::Global,
        cx: &mut App,
    ) -> proto::PerformRenameResponse {
        let transaction = lsp_store.buffer_store().update(cx, |buffer_store, cx| {
            buffer_store.serialize_project_transaction_for_peer(response, peer_id, cx)
        });
        proto::PerformRenameResponse {
            transaction: Some(transaction),
        }
    }

    async fn response_from_proto(
        self,
        message: proto::PerformRenameResponse,
        lsp_store: Entity<LspStore>,
        _: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<ProjectTransaction> {
        let message = message.transaction.context("missing transaction")?;
        lsp_store
            .update(&mut cx, |lsp_store, cx| {
                lsp_store.buffer_store().update(cx, |buffer_store, cx| {
                    buffer_store.deserialize_project_transaction(message, self.push_to_history, cx)
                })
            })
            .await
    }

    fn buffer_id_from_proto(message: &proto::PerformRename) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

fn language_server_for_buffer(
    lsp_store: &Entity<LspStore>,
    buffer: &Entity<Buffer>,
    server_id: LanguageServerId,
    cx: &mut AsyncApp,
) -> Result<(Arc<CachedLspAdapter>, Arc<LanguageServer>)> {
    lsp_store
        .update(cx, |lsp_store, cx| {
            buffer.update(cx, |buffer, cx| {
                lsp_store
                    .language_server_for_local_buffer(buffer, server_id, cx)
                    .map(|(adapter, server)| (adapter.clone(), server.clone()))
            })
        })
        .context("no language server found for buffer")
}

pub async fn location_links_from_proto(
    proto_links: Vec<proto::LocationLink>,
    lsp_store: Entity<LspStore>,
    mut cx: AsyncApp,
) -> Result<Vec<LocationLink>> {
    let mut links = Vec::new();

    for link in proto_links {
        links.push(location_link_from_proto(link, lsp_store.clone(), &mut cx).await?)
    }

    Ok(links)
}

pub fn location_link_from_proto(
    link: proto::LocationLink,
    lsp_store: Entity<LspStore>,
    cx: &mut AsyncApp,
) -> Task<Result<LocationLink>> {
    cx.spawn(async move |cx| {
        let origin = match link.origin {
            Some(origin) => {
                let buffer_id = BufferId::new(origin.buffer_id)?;
                let buffer = lsp_store
                    .update(cx, |lsp_store, cx| {
                        lsp_store.wait_for_remote_buffer(buffer_id, cx)
                    })
                    .await?;
                let start = origin
                    .start
                    .and_then(deserialize_anchor)
                    .context("missing origin start")?;
                let end = origin
                    .end
                    .and_then(deserialize_anchor)
                    .context("missing origin end")?;
                buffer
                    .update(cx, |buffer, _| buffer.wait_for_anchors([start, end]))
                    .await?;
                Some(Location {
                    buffer,
                    range: start..end,
                })
            }
            None => None,
        };

        let target = link.target.context("missing target")?;
        let buffer_id = BufferId::new(target.buffer_id)?;
        let buffer = lsp_store
            .update(cx, |lsp_store, cx| {
                lsp_store.wait_for_remote_buffer(buffer_id, cx)
            })
            .await?;
        let start = target
            .start
            .and_then(deserialize_anchor)
            .context("missing target start")?;
        let end = target
            .end
            .and_then(deserialize_anchor)
            .context("missing target end")?;
        buffer
            .update(cx, |buffer, _| buffer.wait_for_anchors([start, end]))
            .await?;
        let target = Location {
            buffer,
            range: start..end,
        };
        Ok(LocationLink { origin, target })
    })
}

pub async fn location_links_from_lsp(
    message: Option<lsp::GotoDefinitionResponse>,
    lsp_store: Entity<LspStore>,
    buffer: Entity<Buffer>,
    server_id: LanguageServerId,
    workspace_only: bool,
    mut cx: AsyncApp,
) -> Result<Vec<LocationLink>> {
    let message = match message {
        Some(message) => message,
        None => return Ok(Vec::new()),
    };

    let mut unresolved_links = Vec::new();
    match message {
        lsp::GotoDefinitionResponse::Scalar(loc) => {
            unresolved_links.push((None, loc.uri, loc.range));
        }

        lsp::GotoDefinitionResponse::Array(locs) => {
            unresolved_links.extend(locs.into_iter().map(|l| (None, l.uri, l.range)));
        }

        lsp::GotoDefinitionResponse::Link(links) => {
            unresolved_links.extend(links.into_iter().map(|l| {
                (
                    l.origin_selection_range,
                    l.target_uri,
                    l.target_selection_range,
                )
            }));
        }
    }

    let (_, language_server) = language_server_for_buffer(&lsp_store, &buffer, server_id, &mut cx)?;
    let mut definitions = Vec::new();
    for (origin_range, target_uri, target_range) in unresolved_links {
        if workspace_only
            && !lsp_store.update(&mut cx, |this, cx| {
                use util::paths::UrlExt as _;
                let worktree_store = this.worktree_store().read(cx);
                let path_style = worktree_store.path_style();
                let Ok(abs_path) = target_uri.clone().to_file_path_ext(path_style) else {
                    return false;
                };
                worktree_store
                    .find_worktree(&abs_path, cx)
                    .is_some_and(|(worktree, _)| {
                        let worktree = worktree.read(cx);
                        worktree.is_visible() && !worktree.is_single_file()
                    })
            })
        {
            continue;
        }

        let target_buffer_handle = lsp_store
            .update(&mut cx, |this, cx| {
                this.open_local_buffer_via_lsp(target_uri, language_server.server_id(), cx)
            })
            .await?;

        cx.update(|cx| {
            let origin_location = origin_range.map(|origin_range| {
                let origin_buffer = buffer.read(cx);
                let origin_start =
                    origin_buffer.clip_point_utf16(point_from_lsp(origin_range.start), Bias::Left);
                let origin_end =
                    origin_buffer.clip_point_utf16(point_from_lsp(origin_range.end), Bias::Left);
                Location {
                    buffer: buffer.clone(),
                    range: origin_buffer.anchor_after(origin_start)
                        ..origin_buffer.anchor_before(origin_end),
                }
            });

            let target_buffer = target_buffer_handle.read(cx);
            let target_start =
                target_buffer.clip_point_utf16(point_from_lsp(target_range.start), Bias::Left);
            let target_end =
                target_buffer.clip_point_utf16(point_from_lsp(target_range.end), Bias::Left);
            let target_location = Location {
                buffer: target_buffer_handle,
                range: target_buffer.anchor_after(target_start)
                    ..target_buffer.anchor_before(target_end),
            };

            definitions.push(LocationLink {
                origin: origin_location,
                target: target_location,
            })
        });
    }
    Ok(definitions)
}

pub async fn location_link_from_lsp(
    link: lsp::LocationLink,
    lsp_store: &Entity<LspStore>,
    buffer: &Entity<Buffer>,
    server_id: LanguageServerId,
    cx: &mut AsyncApp,
) -> Result<LocationLink> {
    let (_, language_server) = language_server_for_buffer(lsp_store, buffer, server_id, cx)?;

    let (origin_range, target_uri, target_range) = (
        link.origin_selection_range,
        link.target_uri,
        link.target_selection_range,
    );

    let target_buffer_handle = lsp_store
        .update(cx, |lsp_store, cx| {
            lsp_store.open_local_buffer_via_lsp(target_uri, language_server.server_id(), cx)
        })
        .await?;

    Ok(cx.update(|cx| {
        let origin_location = origin_range.map(|origin_range| {
            let origin_buffer = buffer.read(cx);
            let origin_start =
                origin_buffer.clip_point_utf16(point_from_lsp(origin_range.start), Bias::Left);
            let origin_end =
                origin_buffer.clip_point_utf16(point_from_lsp(origin_range.end), Bias::Left);
            Location {
                buffer: buffer.clone(),
                range: origin_buffer.anchor_after(origin_start)
                    ..origin_buffer.anchor_before(origin_end),
            }
        });

        let target_buffer = target_buffer_handle.read(cx);
        let target_start =
            target_buffer.clip_point_utf16(point_from_lsp(target_range.start), Bias::Left);
        let target_end =
            target_buffer.clip_point_utf16(point_from_lsp(target_range.end), Bias::Left);
        let target_location = Location {
            buffer: target_buffer_handle,
            range: target_buffer.anchor_after(target_start)
                ..target_buffer.anchor_before(target_end),
        };

        LocationLink {
            origin: origin_location,
            target: target_location,
        }
    }))
}

pub fn location_links_to_proto(
    links: Vec<LocationLink>,
    lsp_store: &mut LspStore,
    peer_id: PeerId,
    cx: &mut App,
) -> Vec<proto::LocationLink> {
    links
        .into_iter()
        .map(|definition| location_link_to_proto(definition, lsp_store, peer_id, cx))
        .collect()
}

pub fn location_link_to_proto(
    location: LocationLink,
    lsp_store: &mut LspStore,
    peer_id: PeerId,
    cx: &mut App,
) -> proto::LocationLink {
    let origin = location.origin.map(|origin| {
        lsp_store
            .buffer_store()
            .update(cx, |buffer_store, cx| {
                buffer_store.create_buffer_for_peer(&origin.buffer, peer_id, cx)
            })
            .detach_and_log_err(cx);

        let buffer_id = origin.buffer.read(cx).remote_id().into();
        proto::Location {
            start: Some(serialize_anchor(&origin.range.start)),
            end: Some(serialize_anchor(&origin.range.end)),
            buffer_id,
        }
    });

    lsp_store
        .buffer_store()
        .update(cx, |buffer_store, cx| {
            buffer_store.create_buffer_for_peer(&location.target.buffer, peer_id, cx)
        })
        .detach_and_log_err(cx);

    let buffer_id = location.target.buffer.read(cx).remote_id().into();
    let target = proto::Location {
        start: Some(serialize_anchor(&location.target.range.start)),
        end: Some(serialize_anchor(&location.target.range.end)),
        buffer_id,
    };

    proto::LocationLink {
        origin,
        target: Some(target),
    }
}

#[async_trait(?Send)]
impl LspCommand for GetDocumentHighlights {
    type Response = Vec<DocumentHighlight>;
    type LspRequest = lsp::request::DocumentHighlightRequest;
    type ProtoRequest = proto::GetDocumentHighlights;

    fn display_name(&self) -> &str {
        "Get document highlights"
    }

    fn check_capabilities(&self, capabilities: AdapterServerCapabilities) -> bool {
        capabilities
            .server_capabilities
            .document_highlight_provider
            .is_some_and(|capability| match capability {
                OneOf::Left(supported) => supported,
                OneOf::Right(_options) => true,
            })
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<lsp::DocumentHighlightParams> {
        Ok(lsp::DocumentHighlightParams {
            text_document_position_params: make_lsp_text_document_position(path, self.position)?,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
    }

    async fn response_from_lsp(
        self,
        lsp_highlights: Option<Vec<lsp::DocumentHighlight>>,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        _: LanguageServerId,
        cx: AsyncApp,
    ) -> Result<Vec<DocumentHighlight>> {
        Ok(buffer.read_with(&cx, |buffer, _| {
            let mut lsp_highlights = lsp_highlights.unwrap_or_default();
            lsp_highlights.sort_unstable_by_key(|h| (h.range.start, Reverse(h.range.end)));
            lsp_highlights
                .into_iter()
                .map(|lsp_highlight| {
                    let start = buffer
                        .clip_point_utf16(point_from_lsp(lsp_highlight.range.start), Bias::Left);
                    let end = buffer
                        .clip_point_utf16(point_from_lsp(lsp_highlight.range.end), Bias::Left);
                    DocumentHighlight {
                        range: buffer.anchor_after(start)..buffer.anchor_before(end),
                        kind: lsp_highlight
                            .kind
                            .unwrap_or(lsp::DocumentHighlightKind::READ),
                    }
                })
                .collect()
        }))
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> proto::GetDocumentHighlights {
        proto::GetDocumentHighlights {
            project_id,
            buffer_id: buffer.remote_id().into(),
            position: Some(language::proto::serialize_anchor(
                &buffer.anchor_before(self.position),
            )),
            version: serialize_version(&buffer.version()),
        }
    }

    async fn from_proto(
        message: proto::GetDocumentHighlights,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<Self> {
        let position = message
            .position
            .and_then(deserialize_anchor)
            .context("invalid position")?;
        buffer
            .update(&mut cx, |buffer, _| {
                buffer.wait_for_version(deserialize_version(&message.version))
            })
            .await?;
        Ok(Self {
            position: buffer.read_with(&cx, |buffer, _| position.to_point_utf16(buffer)),
        })
    }

    fn response_to_proto(
        response: Vec<DocumentHighlight>,
        _: &mut LspStore,
        _: PeerId,
        _: &clock::Global,
        _: &mut App,
    ) -> proto::GetDocumentHighlightsResponse {
        let highlights = response
            .into_iter()
            .map(|highlight| proto::DocumentHighlight {
                start: Some(serialize_anchor(&highlight.range.start)),
                end: Some(serialize_anchor(&highlight.range.end)),
                kind: match highlight.kind {
                    DocumentHighlightKind::TEXT => proto::document_highlight::Kind::Text.into(),
                    DocumentHighlightKind::WRITE => proto::document_highlight::Kind::Write.into(),
                    DocumentHighlightKind::READ => proto::document_highlight::Kind::Read.into(),
                    _ => proto::document_highlight::Kind::Text.into(),
                },
            })
            .collect();
        proto::GetDocumentHighlightsResponse { highlights }
    }

    async fn response_from_proto(
        self,
        message: proto::GetDocumentHighlightsResponse,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<Vec<DocumentHighlight>> {
        let mut highlights = Vec::new();
        for highlight in message.highlights {
            let start = highlight
                .start
                .and_then(deserialize_anchor)
                .context("missing target start")?;
            let end = highlight
                .end
                .and_then(deserialize_anchor)
                .context("missing target end")?;
            buffer
                .update(&mut cx, |buffer, _| buffer.wait_for_anchors([start, end]))
                .await?;
            let kind = match proto::document_highlight::Kind::from_i32(highlight.kind) {
                Some(proto::document_highlight::Kind::Text) => DocumentHighlightKind::TEXT,
                Some(proto::document_highlight::Kind::Read) => DocumentHighlightKind::READ,
                Some(proto::document_highlight::Kind::Write) => DocumentHighlightKind::WRITE,
                None => DocumentHighlightKind::TEXT,
            };
            highlights.push(DocumentHighlight {
                range: start..end,
                kind,
            });
        }
        Ok(highlights)
    }

    fn buffer_id_from_proto(message: &proto::GetDocumentHighlights) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

#[async_trait(?Send)]
impl LspCommand for GetDocumentSymbols {
    type Response = Vec<DocumentSymbol>;
    type LspRequest = lsp::request::DocumentSymbolRequest;
    type ProtoRequest = proto::GetDocumentSymbols;

    fn display_name(&self) -> &str {
        "Get document symbols"
    }

    fn check_capabilities(&self, capabilities: AdapterServerCapabilities) -> bool {
        capabilities
            .server_capabilities
            .document_symbol_provider
            .is_some_and(|capability| match capability {
                OneOf::Left(supported) => supported,
                OneOf::Right(_options) => true,
            })
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<lsp::DocumentSymbolParams> {
        Ok(lsp::DocumentSymbolParams {
            text_document: make_text_document_identifier(path)?,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
    }

    async fn response_from_lsp(
        self,
        lsp_symbols: Option<lsp::DocumentSymbolResponse>,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: LanguageServerId,
        _: AsyncApp,
    ) -> Result<Vec<DocumentSymbol>> {
        let Some(lsp_symbols) = lsp_symbols else {
            return Ok(Vec::new());
        };

        let symbols = match lsp_symbols {
            lsp::DocumentSymbolResponse::Flat(symbol_information) => symbol_information
                .into_iter()
                .map(|lsp_symbol| DocumentSymbol {
                    name: lsp_symbol.name,
                    kind: lsp_symbol.kind,
                    range: range_from_lsp(lsp_symbol.location.range),
                    selection_range: range_from_lsp(lsp_symbol.location.range),
                    children: Vec::new(),
                })
                .collect(),
            lsp::DocumentSymbolResponse::Nested(nested_responses) => {
                fn convert_symbol(lsp_symbol: lsp::DocumentSymbol) -> DocumentSymbol {
                    DocumentSymbol {
                        name: lsp_symbol.name,
                        kind: lsp_symbol.kind,
                        range: range_from_lsp(lsp_symbol.range),
                        selection_range: range_from_lsp(lsp_symbol.selection_range),
                        children: lsp_symbol
                            .children
                            .map(|children| {
                                children.into_iter().map(convert_symbol).collect::<Vec<_>>()
                            })
                            .unwrap_or_default(),
                    }
                }
                nested_responses.into_iter().map(convert_symbol).collect()
            }
        };
        Ok(symbols)
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> proto::GetDocumentSymbols {
        proto::GetDocumentSymbols {
            project_id,
            buffer_id: buffer.remote_id().into(),
            version: serialize_version(&buffer.version()),
        }
    }

    async fn from_proto(
        message: proto::GetDocumentSymbols,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<Self> {
        buffer
            .update(&mut cx, |buffer, _| {
                buffer.wait_for_version(deserialize_version(&message.version))
            })
            .await?;
        Ok(Self)
    }

    fn response_to_proto(
        response: Vec<DocumentSymbol>,
        _: &mut LspStore,
        _: PeerId,
        _: &clock::Global,
        _: &mut App,
    ) -> proto::GetDocumentSymbolsResponse {
        let symbols = response
            .into_iter()
            .map(|symbol| {
                fn convert_symbol_to_proto(symbol: DocumentSymbol) -> proto::DocumentSymbol {
                    proto::DocumentSymbol {
                        name: symbol.name.clone(),
                        kind: unsafe { mem::transmute::<lsp::SymbolKind, i32>(symbol.kind) },
                        start: Some(proto::PointUtf16 {
                            row: symbol.range.start.0.row,
                            column: symbol.range.start.0.column,
                        }),
                        end: Some(proto::PointUtf16 {
                            row: symbol.range.end.0.row,
                            column: symbol.range.end.0.column,
                        }),
                        selection_start: Some(proto::PointUtf16 {
                            row: symbol.selection_range.start.0.row,
                            column: symbol.selection_range.start.0.column,
                        }),
                        selection_end: Some(proto::PointUtf16 {
                            row: symbol.selection_range.end.0.row,
                            column: symbol.selection_range.end.0.column,
                        }),
                        children: symbol
                            .children
                            .into_iter()
                            .map(convert_symbol_to_proto)
                            .collect(),
                    }
                }
                convert_symbol_to_proto(symbol)
            })
            .collect::<Vec<_>>();

        proto::GetDocumentSymbolsResponse { symbols }
    }

    async fn response_from_proto(
        self,
        message: proto::GetDocumentSymbolsResponse,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: AsyncApp,
    ) -> Result<Vec<DocumentSymbol>> {
        let mut symbols = Vec::with_capacity(message.symbols.len());
        for serialized_symbol in message.symbols {
            fn deserialize_symbol_with_children(
                serialized_symbol: proto::DocumentSymbol,
            ) -> Result<DocumentSymbol> {
                let kind =
                    unsafe { mem::transmute::<i32, lsp::SymbolKind>(serialized_symbol.kind) };

                let start = serialized_symbol.start.context("invalid start")?;
                let end = serialized_symbol.end.context("invalid end")?;

                let selection_start = serialized_symbol
                    .selection_start
                    .context("invalid selection start")?;
                let selection_end = serialized_symbol
                    .selection_end
                    .context("invalid selection end")?;

                Ok(DocumentSymbol {
                    name: serialized_symbol.name,
                    kind,
                    range: Unclipped(PointUtf16::new(start.row, start.column))
                        ..Unclipped(PointUtf16::new(end.row, end.column)),
                    selection_range: Unclipped(PointUtf16::new(
                        selection_start.row,
                        selection_start.column,
                    ))
                        ..Unclipped(PointUtf16::new(selection_end.row, selection_end.column)),
                    children: serialized_symbol
                        .children
                        .into_iter()
                        .filter_map(|symbol| deserialize_symbol_with_children(symbol).ok())
                        .collect::<Vec<_>>(),
                })
            }

            symbols.push(deserialize_symbol_with_children(serialized_symbol)?);
        }

        Ok(symbols)
    }

    fn buffer_id_from_proto(message: &proto::GetDocumentSymbols) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

#[async_trait(?Send)]
impl LspCommand for GetHover {
    type Response = Option<Hover>;
    type LspRequest = lsp::request::HoverRequest;
    type ProtoRequest = proto::GetHover;

    fn display_name(&self) -> &str {
        "Get hover"
    }

    fn check_capabilities(&self, capabilities: AdapterServerCapabilities) -> bool {
        match capabilities.server_capabilities.hover_provider {
            Some(lsp::HoverProviderCapability::Simple(enabled)) => enabled,
            Some(lsp::HoverProviderCapability::Options(_)) => true,
            None => false,
        }
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<lsp::HoverParams> {
        Ok(lsp::HoverParams {
            text_document_position_params: make_lsp_text_document_position(path, self.position)?,
            work_done_progress_params: Default::default(),
        })
    }

    async fn response_from_lsp(
        self,
        message: Option<lsp::Hover>,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        _: LanguageServerId,
        cx: AsyncApp,
    ) -> Result<Self::Response> {
        let Some(hover) = message else {
            return Ok(None);
        };

        let (language, range) = buffer.read_with(&cx, |buffer, _| {
            (
                buffer.language().cloned(),
                hover.range.map(|range| {
                    let token_start =
                        buffer.clip_point_utf16(point_from_lsp(range.start), Bias::Left);
                    let token_end = buffer.clip_point_utf16(point_from_lsp(range.end), Bias::Left);
                    buffer.anchor_after(token_start)..buffer.anchor_before(token_end)
                }),
            )
        });

        fn hover_blocks_from_marked_string(marked_string: lsp::MarkedString) -> Option<HoverBlock> {
            let block = match marked_string {
                lsp::MarkedString::String(content) => HoverBlock {
                    text: content,
                    kind: HoverBlockKind::Markdown,
                },
                lsp::MarkedString::LanguageString(lsp::LanguageString { language, value }) => {
                    HoverBlock {
                        text: value,
                        kind: HoverBlockKind::Code { language },
                    }
                }
            };
            if block.text.is_empty() {
                None
            } else {
                Some(block)
            }
        }

        let contents = match hover.contents {
            lsp::HoverContents::Scalar(marked_string) => {
                hover_blocks_from_marked_string(marked_string)
                    .into_iter()
                    .collect()
            }
            lsp::HoverContents::Array(marked_strings) => marked_strings
                .into_iter()
                .filter_map(hover_blocks_from_marked_string)
                .collect(),
            lsp::HoverContents::Markup(markup_content) => vec![HoverBlock {
                text: markup_content.value,
                kind: if markup_content.kind == lsp::MarkupKind::Markdown {
                    HoverBlockKind::Markdown
                } else {
                    HoverBlockKind::PlainText
                },
            }],
        };

        Ok(Some(Hover {
            contents,
            range,
            language,
        }))
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> Self::ProtoRequest {
        proto::GetHover {
            project_id,
            buffer_id: buffer.remote_id().into(),
            position: Some(language::proto::serialize_anchor(
                &buffer.anchor_before(self.position),
            )),
            version: serialize_version(&buffer.version),
        }
    }

    async fn from_proto(
        message: Self::ProtoRequest,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<Self> {
        let position = message
            .position
            .and_then(deserialize_anchor)
            .context("invalid position")?;
        buffer
            .update(&mut cx, |buffer, _| {
                buffer.wait_for_version(deserialize_version(&message.version))
            })
            .await?;
        Ok(Self {
            position: buffer.read_with(&cx, |buffer, _| position.to_point_utf16(buffer)),
        })
    }

    fn response_to_proto(
        response: Self::Response,
        _: &mut LspStore,
        _: PeerId,
        _: &clock::Global,
        _: &mut App,
    ) -> proto::GetHoverResponse {
        if let Some(response) = response {
            let (start, end) = if let Some(range) = response.range {
                (
                    Some(language::proto::serialize_anchor(&range.start)),
                    Some(language::proto::serialize_anchor(&range.end)),
                )
            } else {
                (None, None)
            };

            let contents = response
                .contents
                .into_iter()
                .map(|block| proto::HoverBlock {
                    text: block.text,
                    is_markdown: block.kind == HoverBlockKind::Markdown,
                    language: if let HoverBlockKind::Code { language } = block.kind {
                        Some(language)
                    } else {
                        None
                    },
                })
                .collect();

            proto::GetHoverResponse {
                start,
                end,
                contents,
            }
        } else {
            proto::GetHoverResponse {
                start: None,
                end: None,
                contents: Vec::new(),
            }
        }
    }

    async fn response_from_proto(
        self,
        message: proto::GetHoverResponse,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<Self::Response> {
        let contents: Vec<_> = message
            .contents
            .into_iter()
            .map(|block| HoverBlock {
                text: block.text,
                kind: if let Some(language) = block.language {
                    HoverBlockKind::Code { language }
                } else if block.is_markdown {
                    HoverBlockKind::Markdown
                } else {
                    HoverBlockKind::PlainText
                },
            })
            .collect();
        if contents.is_empty() {
            return Ok(None);
        }

        let language = buffer.read_with(&cx, |buffer, _| buffer.language().cloned());
        let range = if let (Some(start), Some(end)) = (message.start, message.end) {
            language::proto::deserialize_anchor(start)
                .and_then(|start| language::proto::deserialize_anchor(end).map(|end| start..end))
        } else {
            None
        };
        if let Some(range) = range.as_ref() {
            buffer
                .update(&mut cx, |buffer, _| {
                    buffer.wait_for_anchors([range.start, range.end])
                })
                .await?;
        }

        Ok(Some(Hover {
            contents,
            range,
            language,
        }))
    }

    fn buffer_id_from_proto(message: &Self::ProtoRequest) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

#[async_trait(?Send)]
impl LspCommand for SemanticTokensFull {
    type Response = SemanticTokensResponse;
    type LspRequest = lsp::SemanticTokensFullRequest;
    type ProtoRequest = proto::SemanticTokens;

    fn display_name(&self) -> &str {
        "Semantic tokens full"
    }

    fn check_capabilities(&self, capabilities: AdapterServerCapabilities) -> bool {
        capabilities
            .server_capabilities
            .semantic_tokens_provider
            .as_ref()
            .is_some_and(|semantic_tokens_provider| {
                let options = match semantic_tokens_provider {
                    lsp::SemanticTokensServerCapabilities::SemanticTokensOptions(opts) => opts,
                    lsp::SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(
                        opts,
                    ) => &opts.semantic_tokens_options,
                };

                match options.full {
                    Some(lsp::SemanticTokensFullOptions::Bool(is_supported)) => is_supported,
                    Some(lsp::SemanticTokensFullOptions::Delta { .. }) => true,
                    None => false,
                }
            })
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<lsp::SemanticTokensParams> {
        Ok(lsp::SemanticTokensParams {
            text_document: lsp::TextDocumentIdentifier {
                uri: file_path_to_lsp_url(path)?,
            },
            partial_result_params: Default::default(),
            work_done_progress_params: Default::default(),
        })
    }

    async fn response_from_lsp(
        self,
        message: Option<lsp::SemanticTokensResult>,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: LanguageServerId,
        _: AsyncApp,
    ) -> anyhow::Result<SemanticTokensResponse> {
        match message {
            Some(lsp::SemanticTokensResult::Tokens(tokens)) => Ok(SemanticTokensResponse::Full {
                data: tokens.data,
                result_id: tokens.result_id.map(SharedString::new),
            }),
            Some(lsp::SemanticTokensResult::Partial(_)) => {
                anyhow::bail!(
                    "Unexpected semantic tokens response with partial result for inlay hints"
                )
            }
            None => Ok(Default::default()),
        }
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> proto::SemanticTokens {
        proto::SemanticTokens {
            project_id,
            buffer_id: buffer.remote_id().into(),
            version: serialize_version(&buffer.version()),
            for_server: self.for_server.map(|id| id.to_proto()),
        }
    }

    async fn from_proto(
        message: proto::SemanticTokens,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<Self> {
        buffer
            .update(&mut cx, |buffer, _| {
                buffer.wait_for_version(deserialize_version(&message.version))
            })
            .await?;

        Ok(Self {
            for_server: message
                .for_server
                .map(|id| LanguageServerId::from_proto(id)),
        })
    }

    fn response_to_proto(
        response: SemanticTokensResponse,
        _: &mut LspStore,
        _: PeerId,
        buffer_version: &clock::Global,
        _: &mut App,
    ) -> proto::SemanticTokensResponse {
        match response {
            SemanticTokensResponse::Full { data, result_id } => proto::SemanticTokensResponse {
                data,
                edits: Vec::new(),
                result_id: result_id.map(|s| s.to_string()),
                version: serialize_version(buffer_version),
            },
            SemanticTokensResponse::Delta { edits, result_id } => proto::SemanticTokensResponse {
                data: Vec::new(),
                edits: edits
                    .into_iter()
                    .map(|edit| proto::SemanticTokensEdit {
                        start: edit.start,
                        delete_count: edit.delete_count,
                        data: edit.data,
                    })
                    .collect(),
                result_id: result_id.map(|s| s.to_string()),
                version: serialize_version(buffer_version),
            },
        }
    }

    async fn response_from_proto(
        self,
        message: proto::SemanticTokensResponse,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> anyhow::Result<SemanticTokensResponse> {
        buffer
            .update(&mut cx, |buffer, _| {
                buffer.wait_for_version(deserialize_version(&message.version))
            })
            .await?;

        Ok(SemanticTokensResponse::Full {
            data: message.data,
            result_id: message.result_id.map(SharedString::new),
        })
    }

    fn buffer_id_from_proto(message: &proto::SemanticTokens) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

#[async_trait(?Send)]
impl LspCommand for SemanticTokensDelta {
    type Response = SemanticTokensResponse;
    type LspRequest = lsp::SemanticTokensFullDeltaRequest;
    type ProtoRequest = proto::SemanticTokens;

    fn display_name(&self) -> &str {
        "Semantic tokens delta"
    }

    fn check_capabilities(&self, capabilities: AdapterServerCapabilities) -> bool {
        capabilities
            .server_capabilities
            .semantic_tokens_provider
            .as_ref()
            .is_some_and(|semantic_tokens_provider| {
                let options = match semantic_tokens_provider {
                    lsp::SemanticTokensServerCapabilities::SemanticTokensOptions(opts) => opts,
                    lsp::SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(
                        opts,
                    ) => &opts.semantic_tokens_options,
                };

                match options.full {
                    Some(lsp::SemanticTokensFullOptions::Delta { delta }) => delta.unwrap_or(false),
                    // `full: true` (instead of `full: { delta: true }`) means no support for delta.
                    _ => false,
                }
            })
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<lsp::SemanticTokensDeltaParams> {
        Ok(lsp::SemanticTokensDeltaParams {
            text_document: lsp::TextDocumentIdentifier {
                uri: file_path_to_lsp_url(path)?,
            },
            previous_result_id: self.previous_result_id.clone().map(|s| s.to_string()),
            partial_result_params: Default::default(),
            work_done_progress_params: Default::default(),
        })
    }

    async fn response_from_lsp(
        self,
        message: Option<lsp::SemanticTokensFullDeltaResult>,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: LanguageServerId,
        _: AsyncApp,
    ) -> anyhow::Result<SemanticTokensResponse> {
        match message {
            Some(lsp::SemanticTokensFullDeltaResult::Tokens(tokens)) => {
                Ok(SemanticTokensResponse::Full {
                    data: tokens.data,
                    result_id: tokens.result_id.map(SharedString::new),
                })
            }
            Some(lsp::SemanticTokensFullDeltaResult::TokensDelta(delta)) => {
                Ok(SemanticTokensResponse::Delta {
                    edits: delta
                        .edits
                        .into_iter()
                        .map(|e| SemanticTokensEdit {
                            start: e.start,
                            delete_count: e.delete_count,
                            data: e.data.unwrap_or_default(),
                        })
                        .collect(),
                    result_id: delta.result_id.map(SharedString::new),
                })
            }
            Some(lsp::SemanticTokensFullDeltaResult::PartialTokensDelta { .. }) => {
                anyhow::bail!(
                    "Unexpected semantic tokens response with partial result for inlay hints"
                )
            }
            None => Ok(Default::default()),
        }
    }

    fn to_proto(&self, _: u64, _: &Buffer) -> proto::SemanticTokens {
        unimplemented!("Delta requests are never initialted on the remote client side")
    }

    async fn from_proto(
        _: proto::SemanticTokens,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: AsyncApp,
    ) -> Result<Self> {
        unimplemented!("Delta requests are never initialted on the remote client side")
    }

    fn response_to_proto(
        response: SemanticTokensResponse,
        _: &mut LspStore,
        _: PeerId,
        buffer_version: &clock::Global,
        _: &mut App,
    ) -> proto::SemanticTokensResponse {
        match response {
            SemanticTokensResponse::Full { data, result_id } => proto::SemanticTokensResponse {
                data,
                edits: Vec::new(),
                result_id: result_id.map(|s| s.to_string()),
                version: serialize_version(buffer_version),
            },
            SemanticTokensResponse::Delta { edits, result_id } => proto::SemanticTokensResponse {
                data: Vec::new(),
                edits: edits
                    .into_iter()
                    .map(|edit| proto::SemanticTokensEdit {
                        start: edit.start,
                        delete_count: edit.delete_count,
                        data: edit.data,
                    })
                    .collect(),
                result_id: result_id.map(|s| s.to_string()),
                version: serialize_version(buffer_version),
            },
        }
    }

    async fn response_from_proto(
        self,
        message: proto::SemanticTokensResponse,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> anyhow::Result<SemanticTokensResponse> {
        buffer
            .update(&mut cx, |buffer, _| {
                buffer.wait_for_version(deserialize_version(&message.version))
            })
            .await?;

        Ok(SemanticTokensResponse::Full {
            data: message.data,
            result_id: message.result_id.map(SharedString::new),
        })
    }

    fn buffer_id_from_proto(message: &proto::SemanticTokens) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

impl LinkedEditingRange {
    pub fn check_server_capabilities(capabilities: ServerCapabilities) -> bool {
        let Some(linked_editing_options) = capabilities.linked_editing_range_provider else {
            return false;
        };
        if let LinkedEditingRangeServerCapabilities::Simple(false) = linked_editing_options {
            return false;
        }
        true
    }
}

#[async_trait(?Send)]
impl LspCommand for LinkedEditingRange {
    type Response = Vec<Range<Anchor>>;
    type LspRequest = lsp::request::LinkedEditingRange;
    type ProtoRequest = proto::LinkedEditingRange;

    fn display_name(&self) -> &str {
        "Linked editing range"
    }

    fn check_capabilities(&self, capabilities: AdapterServerCapabilities) -> bool {
        Self::check_server_capabilities(capabilities.server_capabilities)
    }

    fn to_lsp(
        &self,
        path: &Path,
        buffer: &Buffer,
        _server: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<lsp::LinkedEditingRangeParams> {
        let position = self.position.to_point_utf16(&buffer.snapshot());
        Ok(lsp::LinkedEditingRangeParams {
            text_document_position_params: make_lsp_text_document_position(path, position)?,
            work_done_progress_params: Default::default(),
        })
    }

    async fn response_from_lsp(
        self,
        message: Option<lsp::LinkedEditingRanges>,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        _server_id: LanguageServerId,
        cx: AsyncApp,
    ) -> Result<Vec<Range<Anchor>>> {
        if let Some(lsp::LinkedEditingRanges { mut ranges, .. }) = message {
            ranges.sort_by_key(|range| range.start);

            Ok(buffer.read_with(&cx, |buffer, _| {
                ranges
                    .into_iter()
                    .map(|range| {
                        let start =
                            buffer.clip_point_utf16(point_from_lsp(range.start), Bias::Left);
                        let end = buffer.clip_point_utf16(point_from_lsp(range.end), Bias::Left);
                        buffer.anchor_before(start)..buffer.anchor_after(end)
                    })
                    .collect()
            }))
        } else {
            Ok(vec![])
        }
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> proto::LinkedEditingRange {
        proto::LinkedEditingRange {
            project_id,
            buffer_id: buffer.remote_id().to_proto(),
            position: Some(serialize_anchor(&self.position)),
            version: serialize_version(&buffer.version()),
        }
    }

    async fn from_proto(
        message: proto::LinkedEditingRange,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<Self> {
        let position = message.position.context("invalid position")?;
        buffer
            .update(&mut cx, |buffer, _| {
                buffer.wait_for_version(deserialize_version(&message.version))
            })
            .await?;
        let position = deserialize_anchor(position).context("invalid position")?;
        buffer
            .update(&mut cx, |buffer, _| buffer.wait_for_anchors([position]))
            .await?;
        Ok(Self { position })
    }

    fn response_to_proto(
        response: Vec<Range<Anchor>>,
        _: &mut LspStore,
        _: PeerId,
        buffer_version: &clock::Global,
        _: &mut App,
    ) -> proto::LinkedEditingRangeResponse {
        proto::LinkedEditingRangeResponse {
            items: response
                .into_iter()
                .map(|range| proto::AnchorRange {
                    start: Some(serialize_anchor(&range.start)),
                    end: Some(serialize_anchor(&range.end)),
                })
                .collect(),
            version: serialize_version(buffer_version),
        }
    }

    async fn response_from_proto(
        self,
        message: proto::LinkedEditingRangeResponse,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<Vec<Range<Anchor>>> {
        buffer
            .update(&mut cx, |buffer, _| {
                buffer.wait_for_version(deserialize_version(&message.version))
            })
            .await?;
        let items: Vec<Range<Anchor>> = message
            .items
            .into_iter()
            .filter_map(|range| {
                let start = deserialize_anchor(range.start?)?;
                let end = deserialize_anchor(range.end?)?;
                Some(start..end)
            })
            .collect();
        for range in &items {
            buffer
                .update(&mut cx, |buffer, _| {
                    buffer.wait_for_anchors([range.start, range.end])
                })
                .await?;
        }
        Ok(items)
    }

    fn buffer_id_from_proto(message: &proto::LinkedEditingRange) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

#[async_trait(?Send)]
impl LspCommand for GetDocumentColor {
    type Response = Vec<DocumentColor>;
    type LspRequest = lsp::request::DocumentColor;
    type ProtoRequest = proto::GetDocumentColor;

    fn display_name(&self) -> &str {
        "Document color"
    }

    fn check_capabilities(&self, server_capabilities: AdapterServerCapabilities) -> bool {
        server_capabilities
            .server_capabilities
            .color_provider
            .as_ref()
            .is_some_and(|capability| match capability {
                lsp::ColorProviderCapability::Simple(supported) => *supported,
                lsp::ColorProviderCapability::ColorProvider(..) => true,
                lsp::ColorProviderCapability::Options(..) => true,
            })
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<lsp::DocumentColorParams> {
        Ok(lsp::DocumentColorParams {
            text_document: make_text_document_identifier(path)?,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
    }

    async fn response_from_lsp(
        self,
        message: Vec<lsp::ColorInformation>,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: LanguageServerId,
        _: AsyncApp,
    ) -> Result<Self::Response> {
        Ok(message
            .into_iter()
            .map(|color| DocumentColor {
                lsp_range: color.range,
                color: color.color,
                resolved: false,
                color_presentations: Vec::new(),
            })
            .collect())
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> Self::ProtoRequest {
        proto::GetDocumentColor {
            project_id,
            buffer_id: buffer.remote_id().to_proto(),
            version: serialize_version(&buffer.version()),
        }
    }

    async fn from_proto(
        _: Self::ProtoRequest,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: AsyncApp,
    ) -> Result<Self> {
        Ok(Self {})
    }

    fn response_to_proto(
        response: Self::Response,
        _: &mut LspStore,
        _: PeerId,
        buffer_version: &clock::Global,
        _: &mut App,
    ) -> proto::GetDocumentColorResponse {
        proto::GetDocumentColorResponse {
            colors: response
                .into_iter()
                .map(|color| {
                    let start = point_from_lsp(color.lsp_range.start).0;
                    let end = point_from_lsp(color.lsp_range.end).0;
                    proto::ColorInformation {
                        red: color.color.red,
                        green: color.color.green,
                        blue: color.color.blue,
                        alpha: color.color.alpha,
                        lsp_range_start: Some(proto::PointUtf16 {
                            row: start.row,
                            column: start.column,
                        }),
                        lsp_range_end: Some(proto::PointUtf16 {
                            row: end.row,
                            column: end.column,
                        }),
                    }
                })
                .collect(),
            version: serialize_version(buffer_version),
        }
    }

    async fn response_from_proto(
        self,
        message: proto::GetDocumentColorResponse,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: AsyncApp,
    ) -> Result<Self::Response> {
        Ok(message
            .colors
            .into_iter()
            .filter_map(|color| {
                let start = color.lsp_range_start?;
                let start = PointUtf16::new(start.row, start.column);
                let end = color.lsp_range_end?;
                let end = PointUtf16::new(end.row, end.column);
                Some(DocumentColor {
                    resolved: false,
                    color_presentations: Vec::new(),
                    lsp_range: lsp::Range {
                        start: point_to_lsp(start),
                        end: point_to_lsp(end),
                    },
                    color: lsp::Color {
                        red: color.red,
                        green: color.green,
                        blue: color.blue,
                        alpha: color.alpha,
                    },
                })
            })
            .collect())
    }

    fn buffer_id_from_proto(message: &Self::ProtoRequest) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}

#[async_trait(?Send)]
impl LspCommand for GetFoldingRanges {
    type Response = Vec<LspFoldingRange>;
    type LspRequest = lsp::request::FoldingRangeRequest;
    type ProtoRequest = proto::GetFoldingRanges;

    fn display_name(&self) -> &str {
        "Folding ranges"
    }

    fn check_capabilities(&self, server_capabilities: AdapterServerCapabilities) -> bool {
        server_capabilities
            .server_capabilities
            .folding_range_provider
            .as_ref()
            .is_some_and(|capability| match capability {
                lsp::FoldingRangeProviderCapability::Simple(supported) => *supported,
                lsp::FoldingRangeProviderCapability::FoldingProvider(..)
                | lsp::FoldingRangeProviderCapability::Options(..) => true,
            })
    }

    fn to_lsp(
        &self,
        path: &Path,
        _: &Buffer,
        _: &Arc<LanguageServer>,
        _: &App,
    ) -> Result<lsp::FoldingRangeParams> {
        Ok(lsp::FoldingRangeParams {
            text_document: make_text_document_identifier(path)?,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
    }

    async fn response_from_lsp(
        self,
        message: Option<Vec<lsp::FoldingRange>>,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        _: LanguageServerId,
        cx: AsyncApp,
    ) -> Result<Self::Response> {
        let snapshot = buffer.read_with(&cx, |buffer, _| buffer.snapshot());
        let max_point = snapshot.max_point_utf16();
        Ok(message
            .unwrap_or_default()
            .into_iter()
            .filter(|range| range.start_line < range.end_line)
            .filter(|range| range.start_line <= max_point.row && range.end_line <= max_point.row)
            .map(|folding_range| {
                let start_col = folding_range.start_character.unwrap_or(u32::MAX);
                let end_col = folding_range.end_character.unwrap_or(u32::MAX);
                let start = snapshot.clip_point_utf16(
                    Unclipped(PointUtf16::new(folding_range.start_line, start_col)),
                    Bias::Right,
                );
                let end = snapshot.clip_point_utf16(
                    Unclipped(PointUtf16::new(folding_range.end_line, end_col)),
                    Bias::Left,
                );
                let start = snapshot.anchor_after(start);
                let end = snapshot.anchor_before(end);
                let collapsed_text = folding_range
                    .collapsed_text
                    .filter(|t| !t.is_empty())
                    .map(|t| SharedString::from(crate::lsp_store::collapse_newlines(&t, " ")));
                LspFoldingRange {
                    range: start..end,
                    collapsed_text,
                }
            })
            .collect())
    }

    fn to_proto(&self, project_id: u64, buffer: &Buffer) -> Self::ProtoRequest {
        proto::GetFoldingRanges {
            project_id,
            buffer_id: buffer.remote_id().to_proto(),
            version: serialize_version(&buffer.version()),
        }
    }

    async fn from_proto(
        _: Self::ProtoRequest,
        _: Entity<LspStore>,
        _: Entity<Buffer>,
        _: AsyncApp,
    ) -> Result<Self> {
        Ok(Self)
    }

    fn response_to_proto(
        response: Self::Response,
        _: &mut LspStore,
        _: PeerId,
        buffer_version: &clock::Global,
        _: &mut App,
    ) -> proto::GetFoldingRangesResponse {
        let mut ranges = Vec::with_capacity(response.len());
        let mut collapsed_texts = Vec::with_capacity(response.len());
        for folding_range in response {
            ranges.push(serialize_anchor_range(folding_range.range));
            collapsed_texts.push(
                folding_range
                    .collapsed_text
                    .map(|t| t.to_string())
                    .unwrap_or_default(),
            );
        }
        proto::GetFoldingRangesResponse {
            ranges,
            collapsed_texts,
            version: serialize_version(buffer_version),
        }
    }

    async fn response_from_proto(
        self,
        message: proto::GetFoldingRangesResponse,
        _: Entity<LspStore>,
        buffer: Entity<Buffer>,
        mut cx: AsyncApp,
    ) -> Result<Self::Response> {
        buffer
            .update(&mut cx, |buffer, _| {
                buffer.wait_for_version(deserialize_version(&message.version))
            })
            .await?;
        message
            .ranges
            .into_iter()
            .zip(
                message
                    .collapsed_texts
                    .into_iter()
                    .map(Some)
                    .chain(std::iter::repeat(None)),
            )
            .map(|(range, collapsed_text)| {
                Ok(LspFoldingRange {
                    range: deserialize_anchor_range(range)?,
                    collapsed_text: collapsed_text
                        .filter(|t| !t.is_empty())
                        .map(SharedString::from),
                })
            })
            .collect()
    }

    fn buffer_id_from_proto(message: &Self::ProtoRequest) -> Result<BufferId> {
        BufferId::new(message.buffer_id)
    }
}
