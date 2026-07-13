pub mod migrations;
pub mod pages;
pub mod provider_admin;
pub mod providers;
pub mod routes;
pub mod schema;
pub mod ui;

use std::sync::Arc;

use wafer_core::clients::{config, database as db};
use wafer_run::{
    context::Context, Block, BlockEndpoint, BlockInfo, ConfigVar, HttpMethod, InputStream,
    InstanceMode, LifecycleEvent, LifecycleType, Message, OutputStream, WaferError,
};

use self::provider_admin::ProviderAdmin;
use crate::{
    endpoint_match::{self, EndpointRoute},
    http::{err_bad_request, err_internal, err_not_found, ok_json},
    util::json_map,
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
}

/// Method + path-template dispatch table, mirroring `info().endpoints`.
/// Sub-resource templates (`.../discover-models`, `.../load`, `.../status`)
/// precede the generic `.../{id}` / `.../models` templates so the specific
/// route wins (the old `ends_with` ordering). `{id}`/`{backend_id}`/
/// `{model_id}` are bound into `req.param.*`.
const ROUTES: &[EndpointRoute<Route>] = &[
    // UI pages
    EndpointRoute::new(HttpMethod::Get, "/b/llm/", Route::ChatPage),
    EndpointRoute::new(HttpMethod::Get, "/b/llm/threads/{id}", Route::ThreadPage),
    EndpointRoute::new(HttpMethod::Get, "/b/llm/settings", Route::SettingsPage),
    EndpointRoute::new(HttpMethod::Get, "/b/llm/providers", Route::ProvidersPage),
    EndpointRoute::new(HttpMethod::Get, "/b/llm/models", Route::ModelsPage),
    // Chat API
    EndpointRoute::new(HttpMethod::Post, "/b/llm/api/chat", Route::Chat),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/llm/api/chat/stream",
        Route::ChatStream,
    ),
    // Provider CRUD (specific sub-resource first)
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/llm/api/providers/{id}/discover-models",
        Route::DiscoverModels,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/llm/api/providers",
        Route::ListProviders,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/llm/api/providers",
        Route::CreateProvider,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/b/llm/api/providers/{id}",
        Route::UpdateProvider,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/llm/api/providers/{id}",
        Route::DeleteProvider,
    ),
    // Models endpoints (specific sub-resources first)
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/llm/api/models/{backend_id}/{model_id}/status",
        Route::ModelStatus,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/llm/api/models/{backend_id}/{model_id}/load",
        Route::LoadModel,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/llm/api/models/{backend_id}/{model_id}/unload",
        Route::UnloadModel,
    ),
    EndpointRoute::new(HttpMethod::Get, "/b/llm/api/models", Route::ListModels),
    // Config
    EndpointRoute::new(HttpMethod::Get, "/b/llm/api/config", Route::GetConfig),
    EndpointRoute::new(HttpMethod::Post, "/b/llm/api/config", Route::PostConfig),
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
    pub fn new(provider_admin: Arc<dyn ProviderAdmin>) -> Self {
        Self { provider_admin }
    }
}

pub(crate) const SETTINGS_TABLE: &str = "impresspress__llm__settings";

pub(super) const DEFAULT_PROVIDER_VAR: &str = "IMPRESSPRESS__LLM__DEFAULT_PROVIDER";
pub(super) const DEFAULT_MODEL_VAR: &str = "IMPRESSPRESS__LLM__DEFAULT_MODEL";
pub(super) const DEFAULT_PROVIDER: &str = "impresspress/provider-llm";

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

/// Call the messages block to create an entry in a context.
pub(super) async fn messages_create(
    ctx: &dyn Context,
    original_msg: &Message,
    context_id: &str,
    role: &str,
    content: &str,
) -> Option<serde_json::Value> {
    // Serializing a plain `{kind, role, content}` map can only fail on a JSON
    // serializer bug. Surface it via tracing rather than sending an empty
    // body to the messages block, which would 400 with a confusing error.
    let body = match serde_json::to_vec(&serde_json::json!({
        "kind": "message",
        "role": role,
        "content": content,
    })) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("messages_create: failed to encode entry body: {e}");
            return None;
        }
    };

    let resource = format!("/b/messages/api/contexts/{context_id}/entries");
    let mut msg = crate::util::block_request("create", "POST", &resource, original_msg);
    msg.set_meta("req.content_type", "application/json");

    let out = ctx
        .call_block("impresspress/messages", msg, InputStream::from_bytes(body))
        .await;
    if let Ok(buf) = out.collect_buffered().await {
        return serde_json::from_slice::<serde_json::Value>(&buf.body).ok();
    }
    None
}

/// Call the messages block to list entries in a context.
pub(super) async fn messages_list(
    ctx: &dyn Context,
    original_msg: &Message,
    context_id: &str,
) -> Vec<serde_json::Value> {
    let resource = format!("/b/messages/api/contexts/{context_id}/entries?kind=message");
    let msg = crate::util::block_request("retrieve", "GET", &resource, original_msg);

    let out = ctx
        .call_block("impresspress/messages", msg, InputStream::empty())
        .await;
    if let Ok(buf) = out.collect_buffered().await {
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf.body) {
            if let Some(records) = v.get("records").and_then(|r| r.as_array()) {
                return records.clone();
            }
        }
    }
    vec![]
}

// ---------------------------------------------------------------------------
// Handler implementations
// ---------------------------------------------------------------------------

impl LlmBlock {
    /// Resolve which provider block and model to use for a request.
    pub(super) async fn resolve_provider(
        &self,
        ctx: &dyn Context,
        thread_id: &str,
        req_provider: Option<&str>,
        req_model: Option<&str>,
    ) -> (String, String) {
        // Check per-thread override first
        let thread_setting = self.get_thread_setting(ctx, thread_id).await;

        let provider_block = thread_setting
            .as_ref()
            .and_then(|s| s.data.get("provider_block").and_then(|v| v.as_str()))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| req_provider.map(|s| s.to_string()))
            .unwrap_or_else(|| {
                // Will be filled below from config
                String::new()
            });

        let model = thread_setting
            .as_ref()
            .and_then(|s| s.data.get("model").and_then(|v| v.as_str()))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| req_model.map(|s| s.to_string()))
            .unwrap_or_default();

        let default_provider =
            config::get_default(ctx, DEFAULT_PROVIDER_VAR, DEFAULT_PROVIDER).await;
        let default_model = config::get_default(ctx, DEFAULT_MODEL_VAR, "").await;

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

        (final_provider, final_model)
    }

    /// Get the per-thread settings record from the DB, if any.
    ///
    /// Returns the whole [`db::Record`] (not just its `data`) so callers that
    /// need the record id for a follow-up update don't have to re-query.
    async fn get_thread_setting(&self, ctx: &dyn Context, thread_id: &str) -> Option<db::Record> {
        db::get_by_field(
            ctx,
            SETTINGS_TABLE,
            "thread_id",
            serde_json::Value::String(thread_id.to_string()),
        )
        .await
        .ok()
    }

    // --- Config ---

    /// Inter-block discovery: returns the default `(provider, model)` target
    /// other blocks should use when they have no caller-supplied preference.
    ///
    /// Wire format:
    /// * `200 {"provider": "...", "model": "..."}` when configured
    /// * `200 {"provider": null, "model": null}` when no model is configured
    ///   (callers should take a degraded path — same contract as the previous
    ///   in-process `default_target()` returning `None`).
    async fn handle_default_target(&self, ctx: &dyn Context) -> OutputStream {
        let provider = config::get_default(ctx, DEFAULT_PROVIDER_VAR, DEFAULT_PROVIDER).await;
        let model = config::get_default(ctx, DEFAULT_MODEL_VAR, "").await;
        if model.is_empty() || provider.is_empty() {
            return ok_json(&serde_json::json!({
                "provider": serde_json::Value::Null,
                "model": serde_json::Value::Null,
            }));
        }
        ok_json(&serde_json::json!({
            "provider": provider,
            "model": model,
        }))
    }

    async fn handle_get_config(&self, ctx: &dyn Context) -> OutputStream {
        let default_provider =
            config::get_default(ctx, DEFAULT_PROVIDER_VAR, DEFAULT_PROVIDER).await;
        let default_model = config::get_default(ctx, DEFAULT_MODEL_VAR, "").await;
        ok_json(&serde_json::json!({
            "default_provider": default_provider,
            "default_model": default_model,
        }))
    }

    async fn handle_post_config(&self, ctx: &dyn Context, input: InputStream) -> OutputStream {
        #[derive(serde::Deserialize)]
        struct ConfigUpdate {
            thread_id: Option<String>,
            default_provider: Option<String>,
            default_model: Option<String>,
            provider_block: Option<String>,
            model: Option<String>,
        }

        let raw = input.collect_to_bytes().await;
        let body: ConfigUpdate = match serde_json::from_slice(&raw) {
            Ok(b) => b,
            Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
        };

        // Per-thread override update
        if let Some(thread_id) = body.thread_id {
            let existing = self.get_thread_setting(ctx, &thread_id).await;

            if let Some(record) = existing {
                // Update the existing record in place — the single fetch above
                // already gave us both the id and the current data.
                let mut data = record.data;
                if let Some(pb) = body.provider_block {
                    data.insert("provider_block".to_string(), serde_json::json!(pb));
                }
                if let Some(m) = body.model {
                    data.insert("model".to_string(), serde_json::json!(m));
                }
                crate::util::stamp_updated(&mut data);
                match db::update(ctx, SETTINGS_TABLE, &record.id, data).await {
                    Ok(r) => return ok_json(&r),
                    Err(e) => return err_internal("Database error", e),
                }
            } else {
                // Create new per-thread setting
                let mut data = json_map(serde_json::json!({
                    "thread_id": thread_id,
                    "provider_block": body.provider_block.unwrap_or_default(),
                    "model": body.model.unwrap_or_default(),
                }));
                crate::util::stamp_created(&mut data);
                match db::create(ctx, SETTINGS_TABLE, data).await {
                    Ok(r) => return ok_json(&r),
                    Err(e) => return err_internal("Database error", e),
                }
            }
        }

        // Global default update would go via the config system (admin only),
        // but here we just acknowledge since config writes go through wafer-run/config.
        if body.default_provider.is_some() || body.default_model.is_some() {
            return err_bad_request(
                "Global default provider/model must be set via environment variables: IMPRESSPRESS__LLM__DEFAULT_PROVIDER and IMPRESSPRESS__LLM__DEFAULT_MODEL",
            );
        }

        ok_json(&serde_json::json!({"updated": true}))
    }

    // Models aggregation now lives in `routes::list_models`, sourcing data
    // from the `wafer-run/llm` service block via `ctx.call_block`. The
    // legacy `/b/provider-llm/api/models` proxy was removed in Task 16.
}

// ---------------------------------------------------------------------------
// Block trait implementation
// ---------------------------------------------------------------------------

#[wafer_block::wafer_async_trait]
impl Block for LlmBlock {
    fn info(&self) -> BlockInfo {
        use wafer_run::AuthLevel;

        BlockInfo::new(
            "impresspress/llm",
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
        .endpoints(vec![
            BlockEndpoint::post("/b/llm/api/chat")
                .summary("Send a chat message")
                .auth(AuthLevel::Authenticated),
            BlockEndpoint::post("/b/llm/api/chat/stream")
                .summary("Send a chat message (SSE streaming)")
                .auth(AuthLevel::Authenticated),
            BlockEndpoint::get("/b/llm/api/providers")
                .summary("List configured LLM providers")
                .auth(AuthLevel::Admin),
            BlockEndpoint::post("/b/llm/api/providers")
                .summary("Create LLM provider")
                .auth(AuthLevel::Admin),
            BlockEndpoint::patch("/b/llm/api/providers/{id}")
                .summary("Update LLM provider")
                .auth(AuthLevel::Admin),
            BlockEndpoint::delete("/b/llm/api/providers/{id}")
                .summary("Delete LLM provider")
                .auth(AuthLevel::Admin),
            BlockEndpoint::post("/b/llm/api/providers/{id}/discover-models")
                .summary("Discover provider models via /v1/models")
                .auth(AuthLevel::Admin),
            BlockEndpoint::get("/b/llm/api/models")
                .summary("List available models (aggregated across backends)")
                .auth(AuthLevel::Authenticated),
            BlockEndpoint::get("/b/llm/api/models/{backend_id}/{model_id}/status")
                .summary("Model status (ready / loading / unloaded)")
                .auth(AuthLevel::Authenticated),
            BlockEndpoint::post("/b/llm/api/models/{backend_id}/{model_id}/load")
                .summary("Load a model (SSE progress)")
                .auth(AuthLevel::Admin),
            BlockEndpoint::post("/b/llm/api/models/{backend_id}/{model_id}/unload")
                .summary("Unload a model")
                .auth(AuthLevel::Admin),
            BlockEndpoint::get("/b/llm/api/config")
                .summary("Get default provider/model config")
                .auth(AuthLevel::Authenticated),
            BlockEndpoint::post("/b/llm/api/config")
                .summary("Update per-thread provider/model override")
                .auth(AuthLevel::Authenticated),
            // Chat UI is reached from the ADMIN sidebar (nav_groups::admin
            // "Communication" group); the pre-refactor `handle()` gated every
            // non-API page on `is_admin`, so the chat UI was admin-only in
            // practice. Declaring it `Admin` (and the thread permalink too)
            // makes that the single declared, centrally-enforced policy —
            // preserving the exact prior auth outcome (the declared
            // `Authenticated` was drift the old blanket gate overrode).
            BlockEndpoint::get("/b/llm/")
                .summary("Chat UI")
                .auth(AuthLevel::Admin),
            BlockEndpoint::get("/b/llm/threads/{id}")
                .summary("Chat UI (thread permalink)")
                .auth(AuthLevel::Admin),
            BlockEndpoint::get("/b/llm/settings")
                .summary("LLM settings page")
                .auth(AuthLevel::Admin),
            BlockEndpoint::get("/b/llm/providers")
                .summary("Providers admin")
                .auth(AuthLevel::Admin),
            BlockEndpoint::get("/b/llm/models")
                .summary("Models admin")
                .auth(AuthLevel::Admin),
        ])
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
        // declared HTTP endpoint, so it stays a handler-owned guard ahead of
        // the matcher.
        if msg.action() == "retrieve" && msg.path() == "/b/llm/api/internal/default-target" {
            if ctx.caller_id().is_none() {
                return crate::http::err_not_found("not found");
            }
            return self.handle_default_target(ctx).await;
        }

        // Auth is enforced centrally by `route_to_block` from the declared
        // endpoint `AuthLevel` (chat/config/models-list → Authenticated; UI
        // pages, provider CRUD, model load/unload → Admin). The block holds
        // no `user_id`/`is_admin` preamble and the provider/model handlers no
        // longer re-check `is_admin`. `{id}`/`{backend_id}`/`{model_id}` are
        // bound into `req.param.*` for the handlers' `path_param` readers.
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
            "impresspress/llm",
            migrations::SQLITE_MIGRATIONS,
            migrations::POSTGRES_MIGRATIONS,
        )
        .await?;
        if matches!(event.event_type, LifecycleType::Init) {
            // Always load enabled providers into the in-memory service on
            // startup so chat dispatch finds them without waiting for an
            // admin CRUD write. Non-fatal if it fails — admins can trigger
            // a reload via any provider write.
            if let Err(e) = routes::reload_provider_service(ctx, self.provider_admin.as_ref()).await
            {
                tracing::warn!("initial provider reload failed: {e}");
            }
        }
        Ok(())
    }
}
