pub mod assets;
pub mod contracts;
#[cfg(test)]
mod error_mapping_tests;
pub mod migrations;
pub mod pages;
pub mod provider_admin;
pub mod providers;
pub(crate) mod repo;
pub mod routes;
pub mod schema;
pub mod ui;

use std::sync::Arc;

use wafer_core::clients::config;
use wafer_run::{
    context::Context, Block, BlockInfo, ConfigVar, HttpMethod, InputStream, InputType,
    InstanceMode, LifecycleEvent, LifecycleType, Message, OutputStream, WaferError,
};

use self::provider_admin::ProviderAdmin;
use crate::{
    blocks::{
        crud,
        messages::contracts::{EntryKind, EntryRole},
    },
    endpoint_match::{self, request_schema_of, response_schema_of, EndpointRoute},
    http::{err_bad_request, err_not_found, ok_json},
    llm_target::{DefaultTarget, DEFAULT_MAX_TOKENS_VAR},
};

/// In-block dispatch targets, one per declared HTTP endpoint.
#[derive(Clone, Copy)]
enum Route {
    ChatPage,
    ThreadPage,
    SettingsPage,
    ProvidersPage,
    ModelsPage,
    Chat,
    ChatStream,
    DiscoverModels,
    ListProviders,
    CreateProvider,
    UpdateProvider,
    DeleteProvider,
    ModelStatus,
    LoadModel,
    UnloadModel,
    ListModels,
    GetConfig,
    PostConfig,
    DeleteConfig,
}

/// The block's HTTP surface: what `handle()` dispatches on and what
/// `info().endpoints` is generated from. Sub-resource templates
/// (`.../discover-models`, `.../load`, `.../status`) precede the generic
/// `.../{id}` / `.../models` templates so the specific route wins.
/// `{id}`/`{backend_id}`/`{model_id}` are bound into `req.param.*`.
///
/// The chat UI is reached from the ADMIN sidebar (nav_groups::admin
/// "Communication" group); the pre-refactor `handle()` gated every non-API
/// page on `is_admin`, so the pages are declared `Admin` to keep that exact
/// outcome as the single, centrally enforced policy.
const ROUTES: &[EndpointRoute<Route>] = &[
    // UI pages
    EndpointRoute::admin(HttpMethod::Get, "/b/llm/", Route::ChatPage).summary("Chat UI"),
    EndpointRoute::admin(HttpMethod::Get, "/b/llm/threads/{id}", Route::ThreadPage)
        .summary("Chat UI (thread permalink)"),
    EndpointRoute::admin(HttpMethod::Get, "/b/llm/settings", Route::SettingsPage)
        .summary("LLM settings page"),
    EndpointRoute::admin(HttpMethod::Get, "/b/llm/providers", Route::ProvidersPage)
        .summary("Providers admin"),
    EndpointRoute::admin(HttpMethod::Get, "/b/llm/models", Route::ModelsPage)
        .summary("Models admin"),
    // Chat API
    EndpointRoute::authenticated(HttpMethod::Post, "/b/llm/api/chat", Route::Chat)
        .summary("Send a chat message")
        .input(request_schema_of::<contracts::ChatRequest>)
        .output(response_schema_of::<contracts::ChatResponse>),
    // Same request as `/api/chat`; the response is `text/event-stream`, one
    // `data:` frame per `ChatChunk`, then `data: [DONE]` (or `event: error`).
    // No `.output(..)`: it would publish an `application/json` schema for a
    // body this endpoint never sends, and the frame type is wafer-run's
    // `ChatChunk`, which carries no JsonSchema derive to mirror.
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/llm/api/chat/stream",
        Route::ChatStream,
    )
    .summary("Send a chat message (SSE streaming)")
    .input(request_schema_of::<contracts::ChatRequest>),
    // Provider CRUD (specific sub-resource first)
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/llm/api/providers/{id}/discover-models",
        Route::DiscoverModels,
    )
    .summary("Discover provider models via /v1/models")
    .path_params(provider_id_path_schema)
    .output(response_schema_of::<contracts::DiscoveredModelsResponse>),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/llm/api/providers",
        Route::ListProviders,
    )
    .summary("List configured LLM providers")
    .output(response_schema_of::<contracts::ProviderListResponse>),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/llm/api/providers",
        Route::CreateProvider,
    )
    .summary("Create LLM provider")
    .input(request_schema_of::<contracts::CreateProviderRequest>)
    .output(response_schema_of::<contracts::ProviderView>),
    EndpointRoute::admin(
        HttpMethod::Patch,
        "/b/llm/api/providers/{id}",
        Route::UpdateProvider,
    )
    .summary("Update LLM provider")
    .path_params(provider_id_path_schema)
    .input(request_schema_of::<contracts::UpdateProviderRequest>)
    .output(response_schema_of::<contracts::ProviderView>),
    EndpointRoute::admin(
        HttpMethod::Delete,
        "/b/llm/api/providers/{id}",
        Route::DeleteProvider,
    )
    .summary("Delete LLM provider")
    .path_params(provider_id_path_schema)
    .output(response_schema_of::<contracts::ProviderDeleteResponse>),
    // Models (specific sub-resources first)
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/llm/api/models/{backend_id}/{model_id}/status",
        Route::ModelStatus,
    )
    .summary("Model status (ready / loading / unloaded)")
    .path_params(model_path_schema)
    .output(response_schema_of::<contracts::ModelStatusResponse>),
    // Takes no body; answers `text/event-stream`, one `data:` frame per
    // `LoadProgress`, then `data: [DONE]`. No `.output(..)` for the same
    // reason as `/api/chat/stream`.
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/llm/api/models/{backend_id}/{model_id}/load",
        Route::LoadModel,
    )
    .summary("Load a model (SSE progress)")
    .path_params(model_path_schema),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/llm/api/models/{backend_id}/{model_id}/unload",
        Route::UnloadModel,
    )
    .summary("Unload a model")
    .path_params(model_path_schema)
    .output(response_schema_of::<contracts::ModelUnloadResponse>),
    EndpointRoute::authenticated(HttpMethod::Get, "/b/llm/api/models", Route::ListModels)
        .summary("List available models (aggregated across backends)")
        .output(response_schema_of::<contracts::ModelListResponse>),
    // Config.
    //
    // The read is `Authenticated`: it answers the two deployment-wide
    // defaults (`IMPRESSPRESS__LLM__DEFAULT_PROVIDER` /
    // `..._DEFAULT_MODEL`), which every caller of the `Authenticated`
    // `/b/llm/api/chat` is already chatting against — no per-user row, no
    // credential.
    //
    // The two writes are `Admin`. A thread override is keyed by `thread_id`
    // alone and neither handler takes an identity, so at `Authenticated`
    // any logged-in caller could pin ANY thread to any configured backend —
    // or delete any override — including threads they cannot read.
    //
    // The delete's only caller is the admin settings page
    // (`pages::settings_page`, itself `Admin`), which lists the overrides
    // read-only with one `hx-delete` per row. Nothing in this repo calls the
    // POST at all: no page posts to it and no JS fetches it, so today it is
    // published API surface (it carries a request schema and reaches the
    // generated SDK) with no in-tree consumer. OPEN: either an admin UI
    // grows a form that creates an override — the reason the endpoint
    // exists — or the endpoint goes, and with it the only writer of
    // `repo::settings`. `Admin` is the right tier under either answer, so
    // that question is not settled here.
    EndpointRoute::authenticated(HttpMethod::Get, "/b/llm/api/config", Route::GetConfig)
        .summary("Get default provider/model config")
        .output(response_schema_of::<contracts::LlmConfigResponse>),
    EndpointRoute::admin(HttpMethod::Post, "/b/llm/api/config", Route::PostConfig)
        .summary("Update per-thread provider/model override")
        .input(request_schema_of::<contracts::ConfigUpdateRequest>)
        .output(response_schema_of::<contracts::ConfigUpdateResponse>),
    EndpointRoute::admin(
        HttpMethod::Delete,
        "/b/llm/api/config/{id}",
        Route::DeleteConfig,
    )
    .summary("Remove a per-thread provider/model override")
    .path_params(override_id_path_schema)
    .output(response_schema_of::<contracts::ConfigDeleteResponse>),
];

/// LLM feature block. Owns the provider admin UI + chat thread persistence.
///
/// Chat requests go through `ctx.call_block("wafer-run/llm", ...)` — the
/// service block registered at app startup with a `MultiBackendLlmService`
/// router. The block never holds a concrete `LlmService`; it only drives the
/// [`ProviderAdmin`] seam (provider CRUD, discovery, and the
/// `lifecycle(Init)` configure step) against that same router's in-memory
/// provider set. Holding `Arc<dyn ProviderAdmin>` rather than the concrete,
/// `reqwest`/`tokio`-backed `ProviderLlmService` keeps the block buildable on
/// wasm32 (where a [`NoopProviderAdmin`](provider_admin::NoopProviderAdmin)
/// stands in and the browser configures providers in `BrowserLlmService`).
pub struct LlmBlock {
    /// Provider-admin handle for the in-memory router the chat dispatcher
    /// routes to. The provider CRUD endpoints reload it from the DB after
    /// each successful write so the next chat call sees the updated
    /// configuration.
    pub(crate) provider_admin: Arc<dyn ProviderAdmin>,
}

impl LlmBlock {
    /// The name the block registers and reports under.
    pub const BLOCK_NAME: &'static str = "impresspress/llm";

    pub fn new(provider_admin: Arc<dyn ProviderAdmin>) -> Self {
        Self { provider_admin }
    }
}

pub(super) const DEFAULT_PROVIDER_VAR: &str = "IMPRESSPRESS__LLM__DEFAULT_PROVIDER";
pub(super) const DEFAULT_MODEL_VAR: &str = "IMPRESSPRESS__LLM__DEFAULT_MODEL";
pub(super) const DEFAULT_PROVIDER: &str = "impresspress/provider-llm";

/// The variable name the provider form suggests for a provider's API key.
///
/// An example, not a declared key: an admin names whichever variable holds
/// the key in the provider's `key_var`, and `routes::reload_provider_service`
/// resolves it into the in-memory provider.
pub(super) const EXAMPLE_KEY_VAR: &str = "IMPRESSPRESS__LLM__OPENAI_KEY";

/// Output-token budget used when a chat request names none.
///
/// Anthropic's Messages API requires `max_tokens` on every request, so a
/// request that carries none is refused by the encoder before it reaches the
/// provider (`providers::anthropic::EncodeError::MissingMaxTokens`). Every
/// protocol therefore gets a budget from here, which also bounds
/// OpenAI-protocol replies — those are unbounded when the field is absent.
pub(super) const DEFAULT_MAX_TOKENS: u32 = 4096;

/// The output-token budget for a request that names none: the configured
/// [`DEFAULT_MAX_TOKENS_VAR`], or [`DEFAULT_MAX_TOKENS`] when it is unset,
/// unparseable, or zero.
///
/// Zero is rejected rather than forwarded: Anthropic answers `400` for
/// `max_tokens: 0`, so honouring it would turn a mis-typed variable into a
/// provider error on every chat instead of a logged fallback. A failed read
/// is returned, not answered with the built-in default.
pub(super) async fn default_max_tokens(ctx: &dyn Context) -> Result<u32, WaferError> {
    let raw = config::get_default(ctx, DEFAULT_MAX_TOKENS_VAR, "").await?;
    if raw.is_empty() {
        return Ok(DEFAULT_MAX_TOKENS);
    }
    Ok(match raw.parse::<u32>() {
        Ok(value) if value > 0 => value,
        _ => {
            tracing::warn!(
                var = DEFAULT_MAX_TOKENS_VAR,
                value = %raw,
                fallback = DEFAULT_MAX_TOKENS,
                "llm max-token budget is not a positive integer — using the built-in default"
            );
            DEFAULT_MAX_TOKENS
        }
    })
}

// The previous in-process `default_target()` helper has moved to a
// `GET /b/llm/api/internal/default-target` route — see
// `handle_default_target` below. Other blocks (e.g. vector contextual
// retrieval) now fetch the target via `ctx.call_block("impresspress/llm", ...)`
// rather than importing this module directly. That keeps the cross-block
// dependency at the wire level (call_block) instead of the link level
// (Rust use-path), which is what unblocks per-block Cargo features in
// Phase 0b PR-2.

// ---------------------------------------------------------------------------
// Inter-block call helpers
// ---------------------------------------------------------------------------

/// One thread in the chat sidebar, as `impresspress/messages` reports it.
///
/// Not a published contract: it is the decoded shape of another block's
/// response, and the three fields are exactly what the sidebar renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ContextView {
    /// The context id, which is the llm thread id.
    pub id: String,
    /// Display title. Empty renders as "Untitled".
    pub title: String,
    /// RFC 3339 timestamp the list is ordered by.
    pub updated_at: String,
}

/// Read one column out of a messages-block record as the wire delivers it.
///
/// The `database.list` envelope puts the column map under `data`; the
/// top-level fallback is kept from `history_to_messages`, which has carried
/// it since before the entries list went through `call_block`. Shared so the
/// two readers of a messages record (the chat page's bootstrap carrier and
/// the model-history builder) cannot drift apart on it.
pub(super) fn record_field<'a>(record: &'a serde_json::Value, field: &str) -> &'a str {
    record
        .get("data")
        .and_then(|data| data.get(field))
        .or_else(|| record.get(field))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
}

/// The messages block's decoded answer, or the error it terminated with.
///
/// Shared by every call this module makes, so a read and a write cannot end
/// up classifying the same transport failure differently: whatever the callee
/// refused with (its `NotFound` for an unknown thread, a WRAP
/// `PermissionDenied`) is carried back as it stands, and only a stream that
/// ended some other way, or a body that is not JSON, is minted here.
async fn answer_of(out: OutputStream, what: &str) -> Result<serde_json::Value, WaferError> {
    let buffered = out
        .collect_buffered()
        .await
        .map_err(|terminal| match terminal {
            wafer_run::streams::output::TerminalNotResponse::Error(error) => error,
            other => WaferError::new(
                wafer_run::ErrorCode::Internal,
                format!("{what}: the messages block did not answer: {other:?}"),
            ),
        })?;
    serde_json::from_slice(&buffered.body).map_err(|error| {
        WaferError::new(
            wafer_run::ErrorCode::Internal,
            format!("{what}: could not decode the messages block's answer: {error}"),
        )
    })
}

/// The records of a `{records: [...], total_count: n}` list answer, or the
/// error the callee terminated with. An answer with no `records` array is an
/// internal failure, not an empty list: the sidebar and the history would
/// otherwise claim a thread holds nothing.
async fn records_of(out: OutputStream, what: &str) -> Result<Vec<serde_json::Value>, WaferError> {
    answer_of(out, what)
        .await?
        .get("records")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .ok_or_else(|| {
            WaferError::new(
                wafer_run::ErrorCode::Internal,
                format!("{what}: the messages block's answer has no `records` array"),
            )
        })
}

/// Call the messages block to list the caller's threads — the chat page's
/// sidebar.
///
/// `page_size=50` is the cap the page's own `db::list` used, and the messages
/// block orders contexts by `updated_at` descending for every caller, so the
/// set is the one the direct read produced except for the owner filter the
/// block applies (`rest.rs::list_contexts`): the sidebar is owner-scoped now,
/// as it is for every other caller of that route.
pub(super) async fn messages_list_contexts(
    ctx: &dyn Context,
    original_msg: &Message,
) -> Result<Vec<ContextView>, WaferError> {
    let resource = "/b/messages/api/contexts?page_size=50";
    let msg = crate::util::block_request("retrieve", "GET", resource, original_msg);

    let records = records_of(
        ctx.call_block("impresspress/messages", msg, InputStream::empty())
            .await,
        "thread list",
    )
    .await?;

    Ok(records
        .iter()
        .map(|record| ContextView {
            id: record
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            title: record_field(record, "title").to_string(),
            updated_at: record_field(record, "updated_at").to_string(),
        })
        .collect())
}

/// Call the messages block to create an entry in a context, returning the id
/// of the stored entry.
///
/// `role` is the messages block's own [`EntryRole`], not a string: the entry
/// this writes is replayed to the model by
/// `routes::chat::history_to_messages`, and a role neither side agreed on
/// was replayed as the *user* (B20).
///
/// It used to return `Option`, collapsing an encode bug, an unreachable
/// messages block and an undecodable answer into the same `None` the two
/// callers then discarded with `let _ =`. So a turn could fail to store while
/// the model was still called and the request still ended in a normal
/// completion — and the *next* request, which rebuilds the model's history
/// from that store, could not see the turn the answer belonged to. Returning
/// the id is what makes "it was stored" and "here is what was stored"
/// inseparable: there is no longer a value a caller can publish for a write
/// that did not happen.
pub(super) async fn messages_create(
    ctx: &dyn Context,
    original_msg: &Message,
    context_id: &str,
    role: EntryRole,
    content: &str,
) -> Result<String, WaferError> {
    // Serializing a plain `{kind, role, content}` map can only fail on a JSON
    // serializer bug, but sending an empty body to the messages block would
    // 400 with a confusing error, so it stops here.
    let body = serde_json::to_vec(&serde_json::json!({
        "kind": EntryKind::Message,
        "role": role,
        "content": content,
    }))
    .map_err(|error| {
        WaferError::new(
            wafer_run::ErrorCode::Internal,
            format!("entry write: could not encode the entry body: {error}"),
        )
    })?;

    let resource = format!("/b/messages/api/contexts/{context_id}/entries");
    let mut msg = crate::util::block_request("create", "POST", &resource, original_msg);
    msg.set_meta("req.content_type", "application/json");

    let answer = answer_of(
        ctx.call_block("impresspress/messages", msg, InputStream::from_bytes(body))
            .await,
        "entry write",
    )
    .await?;
    // `{"id": …}` is the flat shape; `database.create` answers wrap the row
    // under `data`. Both spellings reach this module (`record_field` carries
    // the same pair for the read half).
    answer
        .get("id")
        .or_else(|| answer.get("data").and_then(|data| data.get("id")))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            WaferError::new(
                wafer_run::ErrorCode::Internal,
                "entry write: the messages block named no stored entry",
            )
        })
}

/// Call the messages block to list entries in a context.
///
/// Shared by the chat page's bootstrap carrier and the model-history builder.
///
/// It used to end in `.unwrap_or_default()`, and that is the swallow that let
/// a dead read ship: when `util::block_request` put `path?query` into
/// `req.resource`, this call matched no route and 404'd on every request, so
/// the chat had no history and the sidebar was empty — and nothing failed.
/// The two callers want different things from the error (the page renders it,
/// the chat prelude refuses to prompt a paid provider with no history), so it
/// is theirs to decide, not this function's to discard.
pub(super) async fn messages_list(
    ctx: &dyn Context,
    original_msg: &Message,
    context_id: &str,
) -> Result<Vec<serde_json::Value>, WaferError> {
    let resource = format!("/b/messages/api/contexts/{context_id}/entries?kind=message");
    let msg = crate::util::block_request("retrieve", "GET", &resource, original_msg);

    let out = ctx
        .call_block("impresspress/messages", msg, InputStream::empty())
        .await;
    records_of(out, "entry list").await
}

// ---------------------------------------------------------------------------
// Handler implementations
// ---------------------------------------------------------------------------

impl LlmBlock {
    /// Resolve which provider block and model to use for a request.
    ///
    /// Returns `Err` when the per-thread override cannot be read. Falling
    /// back to the global default on a database outage would route a
    /// thread's traffic to a backend its owner had pinned away from, and the
    /// caller would never learn.
    pub(super) async fn resolve_provider(
        &self,
        ctx: &dyn Context,
        thread_id: &str,
        req_provider: Option<&str>,
        req_model: Option<&str>,
    ) -> Result<(String, String), WaferError> {
        // Check per-thread override first
        let thread_setting = repo::settings::find_for_thread(ctx, thread_id).await?;

        let provider_block = thread_setting
            .as_ref()
            .map(|setting| setting.provider_block.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| req_provider.map(str::to_string))
            .unwrap_or_else(|| {
                // Will be filled below from config
                String::new()
            });

        let model = thread_setting
            .as_ref()
            .map(|setting| setting.model.as_str())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| req_model.map(str::to_string))
            .unwrap_or_default();

        let default_provider =
            config::get_default(ctx, DEFAULT_PROVIDER_VAR, DEFAULT_PROVIDER).await?;
        let default_model = config::get_default(ctx, DEFAULT_MODEL_VAR, "").await?;

        let final_provider = if provider_block.is_empty() {
            default_provider
        } else {
            provider_block
        };

        let final_model = if model.is_empty() {
            default_model
        } else {
            model
        };

        Ok((final_provider, final_model))
    }

    /// `DELETE /b/llm/api/config/{id}` — remove one per-thread override. The
    /// settings page renders a delete control for every override row; this
    /// is the route it targets. That control swaps the answer over its own
    /// row (`hx-target="closest tr"`, `outerHTML`), so an `HX-Request` gets an
    /// empty HTML body — the row goes — while an API caller gets the JSON
    /// receipt.
    async fn handle_delete_config(&self, ctx: &dyn Context, msg: &Message) -> OutputStream {
        let id = match crud::path_id(msg, "Override") {
            Ok(value) => value.to_string(),
            Err(response) => return response,
        };
        match repo::settings::delete(ctx, &id).await {
            Ok(()) if crate::ui::is_htmx(msg) => crate::ui::html_response(maud::html! {}),
            Ok(()) => ok_json(&contracts::ConfigDeleteResponse { deleted: true }),
            Err(e) => crud::db_error(e, "Override not found", "Database error"),
        }
    }

    // --- Config ---

    /// Inter-block discovery: returns the default target other blocks should
    /// use when they have no caller-supplied preference, as a
    /// [`DefaultTarget`] — the one type both sides of this route serde, so
    /// the body cannot be described differently at each end.
    ///
    /// Answers `200` either way: [`DefaultTarget::unconfigured`] when no
    /// provider or model is set, which callers take a degraded path on (the
    /// same contract as the previous in-process `default_target()` returning
    /// `None`).
    ///
    /// `max_tokens` travels with the target rather than being read by the
    /// caller: [`DEFAULT_MAX_TOKENS_VAR`] is this block's own variable, and a
    /// caller that has to reach a completion needs a budget for it —
    /// Anthropic-protocol providers refuse a request that carries none.
    async fn handle_default_target(&self, ctx: &dyn Context) -> OutputStream {
        match Self::default_target(ctx).await {
            Ok(target) => ok_json(&target),
            Err(e) => crud::db_error_internal(e, "Could not read the default llm target"),
        }
    }

    /// [`Self::handle_default_target`]'s answer; a failed read is returned
    /// rather than reported as an unconfigured target.
    async fn default_target(ctx: &dyn Context) -> Result<DefaultTarget, WaferError> {
        let provider = config::get_default(ctx, DEFAULT_PROVIDER_VAR, DEFAULT_PROVIDER).await?;
        let model = config::get_default(ctx, DEFAULT_MODEL_VAR, "").await?;
        if model.is_empty() || provider.is_empty() {
            return Ok(DefaultTarget::unconfigured());
        }
        Ok(DefaultTarget::configured(
            &provider,
            &model,
            default_max_tokens(ctx).await?,
        ))
    }

    async fn handle_get_config(&self, ctx: &dyn Context) -> OutputStream {
        let defaults = async {
            Ok::<_, WaferError>(contracts::LlmConfigResponse {
                default_provider: config::get_default(ctx, DEFAULT_PROVIDER_VAR, DEFAULT_PROVIDER)
                    .await?,
                default_model: config::get_default(ctx, DEFAULT_MODEL_VAR, "").await?,
            })
        };
        match defaults.await {
            Ok(response) => ok_json(&response),
            Err(e) => crud::db_error_internal(e, "Could not read the default llm target"),
        }
    }

    /// `POST /b/llm/api/config`. Three outcomes, two of them successful:
    /// a body naming a global default is refused first (those come from
    /// the environment); otherwise a `thread_id` creates or updates that
    /// thread's override and returns the row as
    /// [`contracts::ThreadOverrideView`]; anything else is acknowledged
    /// without a write.
    async fn handle_post_config(&self, ctx: &dyn Context, input: InputStream) -> OutputStream {
        use contracts::{ConfigAcknowledgement, ConfigUpdateResponse, ThreadOverrideView};

        let raw = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return OutputStream::error(e),
        };
        let body: contracts::ConfigUpdateRequest = match serde_json::from_slice(&raw) {
            Ok(b) => b,
            Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
        };

        // The global defaults come from the environment, never from this
        // endpoint, and the published contract says sending one is refused.
        // Checked before the thread branch: a request carrying both a
        // `thread_id` and a default is refused whole, not stripped of the
        // default and written as an override.
        if body.default_provider.is_some() || body.default_model.is_some() {
            return err_bad_request(
                "Global default provider/model must be set via environment variables: IMPRESSPRESS__LLM__DEFAULT_PROVIDER and IMPRESSPRESS__LLM__DEFAULT_MODEL",
            );
        }

        // Per-thread override update. The lookup's error is NOT "no
        // override": treating it as one used to write a second row for a
        // thread that already had one.
        if let Some(thread_id) = body.thread_id {
            let existing = match repo::settings::find_for_thread(ctx, &thread_id).await {
                Ok(existing) => existing,
                Err(e) => return crud::db_error_internal(e, "Database error"),
            };

            let written = match existing {
                // Update the existing row in place — the single fetch above
                // already gave us both the id and the current values.
                Some(row) => {
                    repo::settings::update(
                        ctx,
                        &row,
                        body.provider_block.as_deref(),
                        body.model.as_deref(),
                    )
                    .await
                }
                None => {
                    repo::settings::insert(
                        ctx,
                        &thread_id,
                        body.provider_block.as_deref().unwrap_or_default(),
                        body.model.as_deref().unwrap_or_default(),
                    )
                    .await
                }
            };
            return match written {
                Ok(row) => ok_json(&ConfigUpdateResponse::Override(ThreadOverrideView::from(
                    &row,
                ))),
                Err(e) => crud::db_error_internal(e, "Database error"),
            };
        }

        ok_json(&ConfigUpdateResponse::Acknowledged(ConfigAcknowledgement {
            updated: true,
        }))
    }

    // Models aggregation now lives in `routes::list_models`, sourcing data
    // from the `wafer-run/llm` service block via `ctx.call_block`. The
    // legacy `/b/provider-llm/api/models` proxy was removed in Task 16.
}

// ---------------------------------------------------------------------------
// Block trait implementation
// ---------------------------------------------------------------------------

/// Path parameters of `DELETE /b/llm/api/config/{id}`.
fn override_id_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id"],
        "properties": {
            "id": {
                "type": "string",
                "description": "Override row id, as returned by `POST /b/llm/api/config`."
            }
        }
    })
}

/// Path-parameter schema for the `/b/llm/api/providers/{id}…` routes.
///
/// Hand-written rather than derived: every handler reads the id with
/// `msg.var("id")` by name, so a struct declared only to feed a derived
/// path-params schema would have no runtime user (the `tickets` /
/// `messages` precedent).
fn provider_id_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id"],
        "properties": {
            "id": {
                "type": "string",
                "description": "Provider row id, as returned by `GET /b/llm/api/providers`."
            }
        }
    })
}

/// Path-parameter schema for the `/b/llm/api/models/{backend_id}/{model_id}…`
/// routes. Hand-written for the same reason as [`provider_id_path_schema`]:
/// `routes::models::extract_model_path` reads both by name.
fn model_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["backend_id", "model_id"],
        "properties": {
            "backend_id": {
                "type": "string",
                "description": "Backend (provider name) hosting the model, as listed by `GET /b/llm/api/models`."
            },
            "model_id": {
                "type": "string",
                "description": "Model id within that backend."
            }
        }
    })
}

#[wafer_block::wafer_async_trait]
impl Block for LlmBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            Self::BLOCK_NAME,
            "0.0.1",
            "http-handler@v1",
            "LLM orchestrator — routes to provider or local backends",
        )
        .instance_mode(InstanceMode::Singleton)
        .requires(vec![
            "impresspress/messages".into(),
            "wafer-run/llm".into(),
            "wafer-run/database".into(),
            "wafer-run/config".into(),
        ])
        // Tables (`impresspress__llm__settings`, `impresspress__llm__providers`)
        // are owned by `migrations/001_llm_schema.{sqlite,postgres}.sql` and
        // applied via `migrations::apply` in `lifecycle(Init)` below. No
        // `.collections(...)` declaration — schema is no longer materialised
        // implicitly via `ensure_table` on first insert.
        .category(wafer_run::BlockCategory::Feature)
        .description(
            "LLM orchestrator. Routes chat requests to provider-llm or local-llm backends, \
             manages thread history via the messages block, and provides the main chat UI.",
        )
        .endpoints(endpoint_match::declare(ROUTES))
        .config_keys(vec![
            ConfigVar::new(
                DEFAULT_PROVIDER_VAR,
                "Default LLM provider block (impresspress/provider-llm or impresspress/local-llm)",
                DEFAULT_PROVIDER,
            )
            .name("Default Provider"),
            ConfigVar::new(
                DEFAULT_MODEL_VAR,
                "Default model to use (empty = provider default)",
                "",
            )
            .name("Default Model")
            .optional(),
            ConfigVar::new(
                DEFAULT_MAX_TOKENS_VAR,
                "Largest reply, in output tokens, a chat request that names no \
                 budget of its own may generate. Anthropic-protocol providers \
                 refuse a request without one; OpenAI-protocol providers are \
                 capped by it too, where the field's absence would otherwise \
                 leave the reply unbounded. The usable ceiling belongs to the \
                 model, not to this setting: a value above what the configured \
                 model accepts is refused by the provider (Anthropic answers \
                 400), so raise it against the model you actually run.",
                &DEFAULT_MAX_TOKENS.to_string(),
            )
            .name("Default Max Tokens")
            .input_type(InputType::Number),
        ])
        .can_disable(true)
        .default_enabled(true)
    }

    async fn handle(
        &self,
        ctx: &dyn Context,
        mut msg: Message,
        input: InputStream,
    ) -> OutputStream {
        // Inter-block discovery endpoint: returns the configured default
        // `(provider, model)` target. Only accessible from another block (the
        // caller_id is set by `ctx.call_block`); never reachable from external
        // HTTP because the shared pipeline strips the caller id. It is NOT a
        // declared HTTP endpoint (declaring it would publish it), so it stays
        // a handler-owned guard ahead of the matcher; this is the one path
        // read in this block outside `endpoint_match::dispatch`.
        if msg.action() == "retrieve" && msg.path() == DefaultTarget::RESOURCE {
            if ctx.caller_id().is_none() {
                return crate::http::err_not_found("not found");
            }
            return self.handle_default_target(ctx).await;
        }

        // Auth is enforced centrally by `route_to_block` from the declared
        // endpoint `AuthLevel` (chat, the config READ and models-list →
        // Authenticated; UI pages, provider CRUD, model load/unload and the
        // two config WRITES → Admin). The block holds
        // no `user_id`/`is_admin` preamble and the provider/model handlers no
        // longer re-check `is_admin`. `{id}`/`{backend_id}`/`{model_id}` are
        // bound into `req.param.*` for the handlers' `msg.var` readers.
        let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
            return err_not_found("not found");
        };
        match route {
            Route::ChatPage | Route::ThreadPage => pages::page(ctx, &msg).await,
            Route::SettingsPage => pages::settings_page(ctx, &msg).await,
            Route::ProvidersPage => ui::providers_page(self, ctx, &msg).await,
            Route::ModelsPage => ui::models_page(self, ctx, &msg).await,
            Route::Chat => routes::handle_chat(self, ctx, &msg, input).await,
            Route::ChatStream => routes::handle_chat_stream(self, ctx, &msg, input).await,
            Route::DiscoverModels => routes::discover_models(self, ctx, &msg).await,
            Route::ListProviders => routes::list_providers(self, ctx, &msg).await,
            Route::CreateProvider => routes::create_provider(self, ctx, &msg, input).await,
            Route::UpdateProvider => routes::update_provider(self, ctx, &msg, input).await,
            Route::DeleteProvider => routes::delete_provider(self, ctx, &msg).await,
            Route::ModelStatus => routes::model_status(self, ctx, &msg).await,
            Route::LoadModel => routes::load_model(self, ctx, &msg).await,
            Route::UnloadModel => routes::unload_model(self, ctx, &msg).await,
            Route::ListModels => routes::list_models(self, ctx, &msg).await,
            Route::GetConfig => self.handle_get_config(ctx).await,
            Route::PostConfig => self.handle_post_config(ctx, input).await,
            Route::DeleteConfig => self.handle_delete_config(ctx, &msg).await,
        }
    }

    async fn lifecycle(
        &self,
        ctx: &dyn Context,
        event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        // Schema migrations first — must run before any row-level work below,
        // otherwise the provider reload would hit ensure_table fallback
        // paths instead of the indexed table. `lifecycle_init` no-ops on
        // non-Init events.
        crate::migration_helper::lifecycle_init(
            ctx,
            &event,
            Self::BLOCK_NAME,
            migrations::SQLITE_MIGRATIONS,
            migrations::POSTGRES_MIGRATIONS,
        )
        .await?;
        if matches!(event.event_type, LifecycleType::Init) {
            // Load enabled providers into the in-memory service on startup
            // so chat dispatch finds them without waiting for an admin CRUD
            // write. Non-fatal if it fails — admins can trigger a reload via
            // any provider write.
            //
            // Skipped entirely on a runtime with no configurable provider
            // router (a `NoopProviderAdmin` handle: browser and any other
            // build without the native provider backend). There, "no reload
            // happened" is the correct state and not a degradation, so it
            // must not be reported as one — `configure` now answers
            // `NotSupported` rather than silently accepting, and warning on
            // every boot about a capability the deployment never had would
            // be noise an operator has to learn to ignore.
            if self.provider_admin.manages_providers() {
                if let Err(e) =
                    routes::reload_provider_service(ctx, self.provider_admin.as_ref()).await
                {
                    tracing::warn!("initial provider reload failed: {e}");
                }
            } else {
                tracing::debug!(
                    "provider reload skipped: this runtime has no configurable provider router"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod table_tests {
    use std::sync::Arc;

    use super::*;

    /// `info().endpoints` is generated from `ROUTES`; nothing else declares
    /// an endpoint for this block.
    #[test]
    fn info_endpoints_come_from_the_table() {
        let block = LlmBlock::new(Arc::new(provider_admin::NoopProviderAdmin));
        let declared = block.info().endpoints;
        assert_eq!(declared.len(), ROUTES.len());
        for (ep, row) in declared.iter().zip(ROUTES) {
            assert_eq!(ep.method, row.method, "{}", row.template);
            assert_eq!(ep.path, row.template);
            assert_eq!(ep.auth, row.auth, "{}", row.template);
        }
    }
}

#[cfg(test)]
mod config_tests {
    use std::sync::Arc;

    use wafer_run::{streams::output::TerminalNotResponse, ErrorCode, InputStream};

    use super::*;
    use crate::test_support::{admin_msg, output_json, TestContext};

    fn block() -> LlmBlock {
        LlmBlock::new(Arc::new(provider_admin::NoopProviderAdmin))
    }

    /// The output-token budget comes from the declared variable, and a value
    /// that is not a positive integer falls back instead of reaching a
    /// provider as garbage (or as `max_tokens: 0`, which Anthropic answers
    /// `400` to).
    #[tokio::test]
    async fn the_max_token_budget_is_read_from_its_variable() {
        let mut ctx = TestContext::with_llm().await;
        assert_eq!(
            default_max_tokens(&ctx).await.expect("read"),
            DEFAULT_MAX_TOKENS,
            "an unset variable is the built-in default"
        );

        ctx.set_config(DEFAULT_MAX_TOKENS_VAR, "1500");
        assert_eq!(default_max_tokens(&ctx).await.expect("read"), 1500);

        for bad in ["0", "-1", "lots", "4096.5", " 4096"] {
            ctx.set_config(DEFAULT_MAX_TOKENS_VAR, bad);
            assert_eq!(
                default_max_tokens(&ctx).await.expect("read"),
                DEFAULT_MAX_TOKENS,
                "{bad:?} is not a usable budget and must fall back"
            );
        }
    }

    /// An operator changes the budget on the admin Variables screen, which
    /// renders `info().config_keys`. A variable the block reads but does not
    /// declare is unreachable there — and its default would be a number
    /// hardcoded in one handler, which is what the project's config rules
    /// exist to prevent.
    #[test]
    fn the_block_declares_the_max_token_variable() {
        let declared = block().info().config_keys;
        let var = declared
            .iter()
            .find(|v| v.key == DEFAULT_MAX_TOKENS_VAR)
            .expect("the llm block must declare the variable it reads");
        assert_eq!(var.input_type, InputType::Number);
        assert_eq!(
            var.default,
            DEFAULT_MAX_TOKENS.to_string(),
            "the declared default and the fallback are one number"
        );
    }

    /// The settings page renders `hx-delete="/b/llm/api/config/{id}"` for
    /// every per-thread override; that request must reach a route that
    /// removes the row, not the block's 404 fallback.
    #[tokio::test]
    async fn delete_config_removes_the_thread_override() {
        let ctx = TestContext::with_llm().await;
        let created = output_json(
            block()
                .handle_post_config(
                    &ctx,
                    body(serde_json::json!({
                        "thread_id": "t1",
                        "provider_block": "openai-main",
                        "model": "gpt-4o",
                    })),
                )
                .await,
        )
        .await;
        let id = created["id"].as_str().expect("row id").to_string();

        let out = block()
            .handle(
                &ctx,
                admin_msg("delete", &format!("/b/llm/api/config/{id}")),
                InputStream::from_bytes(Vec::new()),
            )
            .await;

        assert_eq!(
            output_json(out).await["deleted"],
            serde_json::json!(true),
            "the settings page's delete button must reach a route"
        );
        let rows = repo::settings::list_all(&ctx)
            .await
            .expect("list overrides");
        assert!(rows.rows.is_empty(), "the override row must be gone");
    }

    fn body(value: serde_json::Value) -> InputStream {
        InputStream::from_bytes(serde_json::to_vec(&value).expect("serialize body"))
    }

    fn sorted_keys(value: &serde_json::Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap_or_else(|| panic!("expected an object, got {value}"))
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    const OVERRIDE_FIELDS: [&str; 6] = [
        "created_at",
        "id",
        "model",
        "provider_block",
        "thread_id",
        "updated_at",
    ];

    /// The override row is published as a flat view, not the database
    /// layer's `{id, data: {…}}` envelope the untyped handler echoed.
    #[tokio::test]
    async fn post_config_creates_a_flat_thread_override_view() {
        let ctx = TestContext::with_llm().await;

        let out = output_json(
            block()
                .handle_post_config(
                    &ctx,
                    body(serde_json::json!({
                        "thread_id": "t1",
                        "provider_block": "openai-main",
                        "model": "gpt-4o",
                    })),
                )
                .await,
        )
        .await;

        assert_eq!(
            sorted_keys(&out),
            OVERRIDE_FIELDS,
            "the wire field set must equal ThreadOverrideView's; a `data` key means \
             the raw row is being echoed again"
        );
        assert_eq!(out["thread_id"], "t1");
        assert_eq!(out["provider_block"], "openai-main");
        assert_eq!(out["model"], "gpt-4o");
        assert!(
            out["id"].as_str().is_some_and(|id| !id.is_empty()),
            "the row id must be published so the settings page can address it"
        );
        for field in ["created_at", "updated_at"] {
            let value = out[field]
                .as_str()
                .unwrap_or_else(|| panic!("{field} must be a string, got {}", out[field]));
            assert!(!value.is_empty(), "{field} must be set");
            chrono::DateTime::parse_from_rfc3339(value).unwrap_or_else(|e| {
                panic!("{field} must be RFC 3339 as the schema promises, got {value:?}: {e}")
            });
        }
    }

    #[tokio::test]
    async fn post_config_updates_the_existing_override_in_place() {
        let ctx = TestContext::with_llm().await;

        let first = output_json(
            block()
                .handle_post_config(
                    &ctx,
                    body(serde_json::json!({
                        "thread_id": "t1",
                        "provider_block": "openai-main",
                        "model": "gpt-4o",
                    })),
                )
                .await,
        )
        .await;
        let second = output_json(
            block()
                .handle_post_config(
                    &ctx,
                    body(serde_json::json!({ "thread_id": "t1", "model": "gpt-4o-mini" })),
                )
                .await,
        )
        .await;

        assert_eq!(sorted_keys(&second), OVERRIDE_FIELDS);
        assert_eq!(
            second["id"], first["id"],
            "a second write updates the same row"
        );
        assert_eq!(
            second["provider_block"], "openai-main",
            "fields absent from the request are retained"
        );
        assert_eq!(second["model"], "gpt-4o-mini");
    }

    /// Without `thread_id` there is nothing to write: the handler
    /// acknowledges and changes nothing. Pinned because the response schema
    /// publishes this branch alongside the override view.
    #[tokio::test]
    async fn post_config_without_a_thread_only_acknowledges() {
        let ctx = TestContext::with_llm().await;

        let out = output_json(
            block()
                .handle_post_config(&ctx, body(serde_json::json!({ "model": "gpt-4o" })))
                .await,
        )
        .await;

        assert_eq!(out, serde_json::json!({ "updated": true }));
        let rows = repo::settings::list_all(&ctx)
            .await
            .expect("list overrides");
        assert!(
            rows.rows.is_empty(),
            "an acknowledgement must not have written an override"
        );
    }

    #[tokio::test]
    async fn post_config_refuses_global_defaults() {
        let ctx = TestContext::with_llm().await;

        let out = block()
            .handle_post_config(&ctx, body(serde_json::json!({ "default_model": "gpt-4o" })))
            .await;

        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(
                    e.message.contains(DEFAULT_MODEL_VAR),
                    "the refusal must point at the variable to set instead, got: {}",
                    e.message
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    /// The published description says sending a global default is refused.
    /// That must hold on the thread branch too: an override request that
    /// also carries a default is refused whole, not silently stripped of the
    /// default and written.
    #[tokio::test]
    async fn post_config_refuses_global_defaults_even_with_a_thread() {
        for value in [
            serde_json::json!({ "thread_id": "t1", "default_model": "gpt-4o" }),
            serde_json::json!({ "thread_id": "t1", "default_provider": "openai-main" }),
        ] {
            let ctx = TestContext::with_llm().await;

            let out = block().handle_post_config(&ctx, body(value.clone())).await;

            match out.collect_buffered().await {
                Err(TerminalNotResponse::Error(e)) => {
                    assert_eq!(e.code, ErrorCode::InvalidArgument, "{value}");
                    assert!(
                        e.message.contains(DEFAULT_PROVIDER_VAR)
                            && e.message.contains(DEFAULT_MODEL_VAR),
                        "{value}: the refusal must point at the variables to set instead, got: {}",
                        e.message
                    );
                }
                other => panic!("{value}: expected InvalidArgument, got {other:?}"),
            }
            let rows = repo::settings::list_all(&ctx)
                .await
                .expect("list overrides");
            assert!(
                rows.rows.is_empty(),
                "{value}: a refused request must not have written an override"
            );
        }
    }

    /// A settings-table read that FAILS is not "no override".
    ///
    /// `get_thread_setting` ended in `.ok()`, so a database outage looked
    /// exactly like an absent row: `handle_post_config` took its "create"
    /// branch and wrote a SECOND override for a thread that already had one,
    /// leaving two rows the `thread_id` lookup then picks between
    /// arbitrarily; and `resolve_provider` silently fell back to the global
    /// default provider and model, billing a thread's traffic to a backend
    /// its owner had pinned away from.
    #[tokio::test]
    async fn a_failing_settings_read_is_an_error_not_an_absent_override() {
        let ctx = TestContext::with_llm().await;
        output_json(
            block()
                .handle_post_config(
                    &ctx,
                    body(serde_json::json!({
                        "thread_id": "t1",
                        "provider_block": "openai-main",
                        "model": "gpt-4o",
                    })),
                )
                .await,
        )
        .await;

        let failing = crate::test_support::FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.list", repo::settings::TABLE)],
        );

        match block()
            .handle_post_config(
                &failing,
                body(serde_json::json!({ "thread_id": "t1", "model": "gpt-4o-mini" })),
            )
            .await
            .collect_buffered()
            .await
        {
            Err(TerminalNotResponse::Error(e)) => assert_eq!(e.code, ErrorCode::Internal),
            other => panic!("a failed lookup must not be treated as an absent row: {other:?}"),
        }
        let rows = repo::settings::list_all(&ctx)
            .await
            .expect("list overrides");
        assert_eq!(
            rows.rows.len(),
            1,
            "the failed lookup must not have created a second override for t1"
        );

        assert_eq!(
            block()
                .resolve_provider(&failing, "t1", None, None)
                .await
                .expect_err("the outage surfaces")
                .code,
            ErrorCode::Internal,
            "a failed settings read must not fall back to the default provider/model"
        );
    }

    #[tokio::test]
    async fn get_config_publishes_the_defaults() {
        let mut ctx = TestContext::with_llm().await;
        ctx.set_config(DEFAULT_PROVIDER_VAR, "openai-main");
        ctx.set_config(DEFAULT_MODEL_VAR, "gpt-4o");

        let out = output_json(block().handle_get_config(&ctx).await).await;

        assert_eq!(
            out,
            serde_json::json!({ "default_provider": "openai-main", "default_model": "gpt-4o" })
        );
    }
}

#[cfg(test)]
mod access_tests {
    use std::sync::Arc;

    use super::*;
    use crate::test_support::{anon_msg, output_http_status, output_json, Session, TestContext};

    /// A context that routes `/b/llm/*` to the real block and can sign people
    /// in, so each request below presents a real token.
    async fn ctx() -> TestContext {
        let mut ctx = TestContext::with_llm()
            .await
            .with_auth_added()
            .await
            .with_sign_in_added();
        ctx.register_block(
            "impresspress/llm",
            Arc::new(LlmBlock::new(Arc::new(provider_admin::NoopProviderAdmin))),
        );
        ctx
    }

    async fn signed_in(ctx: &TestContext, role: &str) -> Session {
        let email = format!("{role}@example.com");
        ctx.seed_account(&email, "correct-horse-battery-staple", role)
            .await;
        ctx.sign_in(&email, "correct-horse-battery-staple").await
    }

    fn override_body(thread_id: &str) -> InputStream {
        InputStream::from_bytes(
            serde_json::to_vec(&serde_json::json!({
                "thread_id": thread_id,
                "provider_block": "attacker-proxy",
                "model": "gpt-4o",
            }))
            .expect("serialize body"),
        )
    }

    /// A thread override is keyed by `thread_id` alone and the handler takes
    /// no identity, so `Authenticated` meant any logged-in caller could pin
    /// any thread — one they cannot even read — to any configured backend.
    /// The write is admin-only, enforced by the router.
    ///
    /// Driven through `TestContext::request` with a real token, so both the
    /// credential check and the access gate are the ones production runs.
    #[tokio::test]
    async fn a_non_admin_cannot_pin_a_thread_to_a_backend() {
        let ctx = ctx().await;
        let member = signed_in(&ctx, "user").await;

        assert_eq!(
            output_http_status(
                ctx.request_with_input(
                    member.bearer(anon_msg("create", "/b/llm/api/config")),
                    override_body("someone-elses-thread"),
                )
                .await
            )
            .await,
            403,
        );
        assert!(
            repo::settings::list_all(&ctx)
                .await
                .expect("list overrides")
                .rows
                .is_empty(),
            "the refused request must not have written an override"
        );
    }

    /// The delete is the same decision from the other side: without it, any
    /// logged-in caller could drop the admin's override for any thread.
    #[tokio::test]
    async fn a_non_admin_cannot_delete_an_override() {
        let ctx = ctx().await;
        let admin = signed_in(&ctx, "admin").await;
        let member = signed_in(&ctx, "user").await;

        let created = output_json(
            ctx.request_with_input(
                admin.bearer(anon_msg("create", "/b/llm/api/config")),
                override_body("t1"),
            )
            .await,
        )
        .await;
        let id = created["id"].as_str().expect("row id").to_string();

        assert_eq!(
            output_http_status(
                ctx.request(member.bearer(anon_msg("delete", &format!("/b/llm/api/config/{id}"),)))
                    .await
            )
            .await,
            403,
        );
        assert_eq!(
            repo::settings::list_all(&ctx)
                .await
                .expect("list overrides")
                .rows
                .len(),
            1,
            "the refused delete must have left the override in place"
        );
    }

    /// The read stays `Authenticated`: it publishes the two deployment-wide
    /// defaults, which every caller of the equally-`Authenticated`
    /// `/b/llm/api/chat` is already chatting against.
    #[tokio::test]
    async fn the_config_read_is_still_open_to_any_logged_in_caller() {
        let ctx = ctx().await;
        let member = signed_in(&ctx, "user").await;
        assert_eq!(
            output_http_status(
                ctx.request(member.bearer(anon_msg("retrieve", "/b/llm/api/config")))
                    .await
            )
            .await,
            200,
        );
    }
}
