//! HTTP route handlers for the `impresspress/llm` feature block.
//!
//! Both endpoints (`/b/llm/api/chat`, `/b/llm/api/chat/stream`) route through
//! the `wafer-run/llm` service block via `ctx.call_block`. They persist user
//! and assistant messages via `impresspress/messages`, resolve the provider +
//! model via [`LlmBlock::resolve_provider`], and translate the
//! `ChatChunk` stream returned by the service into either a buffered JSON
//! response or a Server-Sent Events stream.

use std::sync::Arc;

use futures::StreamExt;
use wafer_core::clients::{
    config, database as db,
    llm::{
        self as llm_client, ChatChunk, ChatContent, ChatMessage, ChatParams, ChatRequest, ChatRole,
        ChunkDelta, LoadModelRequest, StatusRequest, UnloadModelRequest,
    },
    NativeTypedFrameStream,
};
use wafer_run::{
    context::Context, InputStream, Message, MetaEntry, OutputSink, OutputStream,
    META_RESP_CONTENT_TYPE,
};

use super::{
    messages_create, messages_list,
    provider_admin::ProviderAdmin,
    providers::config::{ProviderConfig, ProviderProtocol},
    schema::{config_to_row, row_to_config, TABLE as PROVIDERS_TABLE},
    LlmBlock, DEFAULT_PROVIDER,
};
use crate::{
    http::{err_bad_request, err_internal, err_not_found, ok_json},
    util::path_param,
};

/// Legacy default provider block name that must be replaced with the first
/// enabled provider from `impresspress__llm__providers` before the request
/// reaches the `wafer-run/llm` service.
const LEGACY_PROVIDER_BLOCK: &str = DEFAULT_PROVIDER;

#[derive(serde::Deserialize)]
struct ChatRequestBody {
    thread_id: String,
    message: String,
    provider: Option<String>,
    model: Option<String>,
}

/// Map a stored message-role string to a [`ChatRole`].
///
/// "user", "assistant", "system" map to their matching variants; anything
/// else falls back to [`ChatRole::User`].
fn role_from_str(role: &str) -> ChatRole {
    match role {
        "assistant" => ChatRole::Assistant,
        "system" => ChatRole::System,
        // "user" or any unknown role — coerce to User rather than dropping.
        _ => ChatRole::User,
    }
}

/// Build a text-content `ChatMessage` for the given role.
///
/// `ChatRole::Tool` is unreachable via `role_from_str` (it coerces to
/// `User`), but if it ever bubbles up here a tool-result message would
/// require a `tool_call_id` we don't have — so coerce it to a user turn
/// rather than emit an invalid Tool message.
fn build_text_message(role: ChatRole, content: String) -> ChatMessage {
    let role = match role {
        ChatRole::Tool => ChatRole::User,
        other => other,
    };
    ChatMessage {
        role,
        content: ChatContent::Text(content),
        tool_call_id: None,
        tool_calls: Vec::new(),
    }
}

/// Convert stored message history into the `ChatMessage` vector the service
/// interface expects. Non-text entries (or entries missing `role`) are
/// skipped silently.
fn history_to_messages(history: &[serde_json::Value]) -> Vec<ChatMessage> {
    history
        .iter()
        .filter_map(|m| {
            let role = m
                .get("data")
                .and_then(|d| d.get("role"))
                .or_else(|| m.get("role"))
                .and_then(|r| r.as_str())?;
            let content = m
                .get("data")
                .and_then(|d| d.get("content"))
                .or_else(|| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("");
            Some(build_text_message(role_from_str(role), content.to_string()))
        })
        .collect()
}

/// Resolve a legacy `impresspress/provider-llm` default into a concrete
/// backend_id by reading the in-memory provider cache (loaded at `Init` and
/// refreshed on every provider CRUD write) via the [`ProviderAdmin`] handle.
/// Returns `Err` if no enabled provider is configured.
fn resolve_backend_id(block: &LlmBlock, provider_block: &str) -> Result<String, &'static str> {
    if provider_block != LEGACY_PROVIDER_BLOCK {
        // `provider_block` is the backend_id directly (non-legacy path).
        return Ok(provider_block.to_string());
    }

    block
        .provider_admin
        .providers_snapshot()
        .into_iter()
        .find(|cfg| cfg.enabled)
        .map(|cfg| cfg.name)
        .ok_or("no enabled provider configured")
}

/// Common prelude for both chat handlers: parse the body, persist the user
/// message, load history, resolve provider + model, build the `ChatRequest`,
/// and call `wafer-run/llm` via the typed client.
///
/// Returns the typed `ChatChunk` stream from the service on success, or a
/// ready-to-return error stream on any failure.
async fn dispatch_chat(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> Result<DispatchOutcome, OutputStream> {
    let raw = input.collect_to_bytes().await;
    let ChatRequestBody {
        thread_id,
        message,
        provider,
        model,
    } = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return Err(err_bad_request(&format!("Invalid body: {e}"))),
    };

    // 1. Persist the user message before calling the model.
    let _ = messages_create(ctx, msg, &thread_id, "user", &message).await;

    // 2. Load prior history (which now includes the just-written user msg).
    let history = messages_list(ctx, msg, &thread_id).await;
    let messages = history_to_messages(&history);

    // 3. Resolve the provider block / model via the block's existing logic.
    let (provider_block, resolved_model) = block
        .resolve_provider(ctx, &thread_id, provider.as_deref(), model.as_deref())
        .await;

    // 4. Map the legacy `impresspress/provider-llm` default into a concrete
    //    backend_id (first enabled provider). Non-legacy values pass through.
    let backend_id = match resolve_backend_id(block, &provider_block) {
        Ok(id) => id,
        Err(e) => return Err(err_internal("resolve_backend_id failed", e)),
    };

    // 5. Build the service request and dispatch via the typed client.
    let chat_req = ChatRequest {
        backend_id,
        model: resolved_model.clone(),
        messages,
        params: ChatParams::default(),
        tools: Vec::new(),
        extra: serde_json::Value::Null,
    };
    let stream = match llm_client::chat_stream(ctx, &chat_req).await {
        Ok(s) => s,
        Err(e) => return Err(err_internal("llm chat dispatch", e.message)),
    };
    Ok(DispatchOutcome {
        thread_id,
        model: resolved_model,
        stream,
    })
}

/// Result of the shared chat prelude — owns the typed stream plus the
/// metadata the buffered + streaming handlers need to echo back.
struct DispatchOutcome {
    thread_id: String,
    /// Resolved model string — what we asked the service to run. Returned to
    /// the client so the UI can label the assistant message with the actual
    /// model used (the service does not echo it back in the chunk stream).
    model: String,
    stream: NativeTypedFrameStream<ChatChunk>,
}

/// Cap (in bytes) on the assistant reply we'll buffer in the JSON chat path.
/// A misbehaving model that streams indefinitely can otherwise hold an entire
/// response in memory before responding. SSE callers (`/chat/stream`) are
/// unaffected — they forward each chunk as it arrives.
const MAX_BUFFERED_RESPONSE_BYTES: usize = 1024 * 1024;

/// Buffered chat handler: collects the full `ChatChunk` stream, concatenates
/// all text deltas, persists the assistant message, and returns a JSON body.
pub(super) async fn handle_chat(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let DispatchOutcome {
        thread_id,
        model: model_used,
        mut stream,
    } = match dispatch_chat(block, ctx, msg, input).await {
        Ok(x) => x,
        Err(err) => return err,
    };

    // Drain the typed `ChatChunk` stream, concatenating `ChunkDelta::Text`
    // bytes into the assistant reply. Propagate any error terminal as a 500.
    let mut content = String::new();
    let mut truncated = false;
    while let Some(item) = stream.next().await {
        let chunk = match item {
            Ok(c) => c,
            Err(e) => return err_internal("llm service error", e.message),
        };
        match chunk.delta {
            ChunkDelta::Text(s) => {
                if content.len() + s.len() > MAX_BUFFERED_RESPONSE_BYTES {
                    // Stop appending but keep draining so the stream can
                    // close cleanly and any usage frame still flows through.
                    truncated = true;
                    continue;
                }
                content.push_str(&s);
            }
            // Tool-call and empty deltas are ignored in the buffered path.
            ChunkDelta::ToolCallStart { .. }
            | ChunkDelta::ToolCallArguments { .. }
            | ChunkDelta::ToolCallComplete { .. }
            | ChunkDelta::Empty => {}
        }
    }
    if truncated {
        tracing::warn!(
            cap = MAX_BUFFERED_RESPONSE_BYTES,
            "llm buffered response exceeded cap — truncated"
        );
    }

    // Persist the assistant reply.
    let saved = messages_create(ctx, msg, &thread_id, "assistant", &content).await;
    let message_id = saved
        .as_ref()
        .and_then(|v| {
            v.get("id")
                .or_else(|| v.get("data").and_then(|d| d.get("id")))
        })
        .and_then(|id| id.as_str())
        .unwrap_or("")
        .to_string();

    ok_json(&serde_json::json!({
        "content": content,
        "message_id": message_id,
        "model": model_used,
        "truncated": truncated,
    }))
}

/// SSE streaming chat handler: forwards each `ChatChunk` (as its JSON
/// encoding) to the HTTP response as a `data:` frame, then persists the
/// accumulated assistant text to the messages block at natural
/// end-of-stream — see [`sse_chat_response`].
pub(super) async fn handle_chat_stream(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    // Run the shared prelude. On success we own the typed `ChatChunk`
    // stream; we re-emit each chunk as JSON SSE with a body-level
    // content-type.
    let DispatchOutcome {
        thread_id,
        model: _,
        stream,
    } = match dispatch_chat(block, ctx, msg, input).await {
        Ok(x) => x,
        Err(err) => return err,
    };

    // The SSE producer runs in a spawned task, so it can't borrow `ctx` or
    // `msg`. `Context::clone_arc()` yields an owned handle that crosses the
    // spawn boundary, and `Message` is `Clone` — `messages_create` only
    // reads the forwarded auth identity off it.
    sse_chat_response(stream, ctx.clone_arc(), msg.clone(), thread_id)
}

/// Terminal SSE frame for natural end-of-stream, letting clients distinguish
/// it from a transport-level disconnect.
const SSE_DONE_FRAME: &[u8] = b"data: [DONE]\n\n";

/// Terminal SSE frame emitted when the service stream yields an error or an
/// item fails to JSON-encode, so the consumer sees a clean SSE event instead
/// of an abrupt disconnect.
const SSE_ERROR_FRAME: &[u8] = b"event: error\ndata: {}\n\n";

/// Encode one typed item as an SSE `data: <json>\n\n` frame.
///
/// Returns `None` when JSON encoding fails; callers emit [`SSE_ERROR_FRAME`]
/// and terminate. Shared by [`sse_json_response`] and [`sse_chat_response`]
/// so the SSE wire format cannot drift between the generic and the
/// chat-finalizing paths.
fn sse_json_frame<T: serde::Serialize>(item: &T) -> Option<Vec<u8>> {
    let json = serde_json::to_vec(item).ok()?;
    let mut frame = Vec::with_capacity(json.len() + 8);
    frame.extend_from_slice(b"data: ");
    frame.extend_from_slice(&json);
    frame.extend_from_slice(b"\n\n");
    Some(frame)
}

/// Send the `text/event-stream` content-type as a mid-stream meta event so
/// the HTTP listener writes the SSE header before the first `data:` frame.
/// A send failure only means the consumer already dropped the stream; the
/// producer's next `send_chunk` surfaces that, so it is ignored here.
async fn send_sse_content_type(sink: &OutputSink) {
    let _ = sink
        .send_meta(MetaEntry {
            key: META_RESP_CONTENT_TYPE.to_string(),
            value: "text/event-stream".to_string(),
        })
        .await;
}

/// SSE wrapper for the chat endpoint: frames each [`ChatChunk`] exactly like
/// [`sse_json_response`] while accumulating `ChunkDelta::Text` deltas, then
/// persists the assistant turn via [`messages_create`] at natural
/// end-of-stream (immediately before the terminal `data: [DONE]` frame, so a
/// client that refetches history on `[DONE]` sees the new message).
///
/// Accumulation mirrors [`handle_chat`]: text deltas are concatenated up to
/// [`MAX_BUFFERED_RESPONSE_BYTES`] (an overflowing delta stops accumulation
/// with a warning at end-of-stream, while frames keep flowing to the
/// client), and tool-call/empty deltas are forwarded but not accumulated. A
/// service error or encode failure terminates the stream with an error frame
/// and skips persistence — the same outcome as `handle_chat`, which returns
/// a 500 without persisting when the stream errors.
///
/// Generic over the chunk stream (rather than taking
/// [`NativeTypedFrameStream`]`<ChatChunk>` directly, whose constructor is
/// private to wafer-core) so tests can drive it with a scripted stream. The
/// `MaybeSend` bound keeps it compilable on wasm32, where
/// [`OutputStream::from_producer`] does not require `Send`.
fn sse_chat_response<S>(
    stream: S,
    ctx: Arc<dyn Context>,
    msg: Message,
    thread_id: String,
) -> OutputStream
where
    S: futures::Stream<Item = Result<ChatChunk, wafer_run::WaferError>>
        + wafer_run::MaybeSend
        + Unpin
        + 'static,
{
    OutputStream::from_producer(move |sink, _cancel| async move {
        send_sse_content_type(&sink).await;

        let mut stream = stream;
        let mut content = String::new();
        let mut truncated = false;
        while let Some(item) = stream.next().await {
            let Ok(chunk) = item else {
                let _ = sink.send_chunk(SSE_ERROR_FRAME.to_vec()).await;
                return;
            };
            if let ChunkDelta::Text(s) = &chunk.delta {
                if content.len() + s.len() > MAX_BUFFERED_RESPONSE_BYTES {
                    // Stop accumulating (same skip-the-delta semantics as
                    // `handle_chat`) but keep forwarding frames — the client
                    // still receives the full stream.
                    truncated = true;
                } else {
                    content.push_str(s);
                }
            }
            let Some(frame) = sse_json_frame(&chunk) else {
                let _ = sink.send_chunk(SSE_ERROR_FRAME.to_vec()).await;
                return;
            };
            if sink.send_chunk(frame).await.is_err() {
                return;
            }
        }
        if truncated {
            tracing::warn!(
                cap = MAX_BUFFERED_RESPONSE_BYTES,
                "llm streamed response exceeded persistence cap — stored assistant message truncated"
            );
        }

        // Natural end-of-stream: persist the assistant turn before
        // signalling `[DONE]`, so a client that refetches history on
        // `[DONE]` already sees the new message. Persistence failure is
        // non-fatal here, exactly as in `handle_chat` (`messages_create`
        // logs and returns `None`).
        let _ = messages_create(ctx.as_ref(), &msg, &thread_id, "assistant", &content).await;

        let _ = sink.send_chunk(SSE_DONE_FRAME.to_vec()).await;
    })
}

/// Stream a typed frame stream to the client as JSON Server-Sent Events.
///
/// Emits the `text/event-stream` content-type as a mid-stream meta event so
/// the HTTP listener writes the SSE header before the first `data:` frame,
/// then re-encodes each typed item as a `data: <json>\n\n` frame. A service
/// or encode error becomes a terminal `event: error\ndata: {}\n\n` frame; a
/// natural end-of-stream becomes a terminal `data: [DONE]\n\n` frame so
/// clients can distinguish it from a transport-level disconnect.
///
/// Used by the model-load endpoint; the chat-stream endpoint uses
/// [`sse_chat_response`], which shares the same frame encoding via
/// [`sse_json_frame`] and the same terminal frames so the wire format can't
/// drift between them.
fn sse_json_response<T>(stream: NativeTypedFrameStream<T>) -> OutputStream
where
    T: serde::Serialize + serde::de::DeserializeOwned + Unpin + Send + 'static,
{
    OutputStream::from_producer(move |sink, _cancel| async move {
        send_sse_content_type(&sink).await;

        let mut stream = stream;
        while let Some(item) = stream.next().await {
            // A mid-stream service error or a JSON-encode failure both
            // terminate the stream with a final `event: error` frame, so the
            // consumer sees a clean SSE event instead of an abrupt disconnect.
            let Some(frame) = item.ok().and_then(|v| sse_json_frame(&v)) else {
                let _ = sink.send_chunk(SSE_ERROR_FRAME.to_vec()).await;
                return;
            };
            if sink.send_chunk(frame).await.is_err() {
                return;
            }
        }
        let _ = sink.send_chunk(SSE_DONE_FRAME.to_vec()).await;
    })
}

// ---------------------------------------------------------------------------
// Provider CRUD (admin-only)
// ---------------------------------------------------------------------------
//
// These endpoints back the LLM admin UI's provider management. All writes
// reload the in-memory `ProviderLlmService` from the DB so chat requests
// pick up the new configuration without restarting the process.

/// Body shape for `POST /b/llm/api/providers` and `PATCH /b/llm/api/providers/:id`.
///
/// Every field is optional so the same struct can serve both create (which
/// validates required fields after parsing) and patch.
#[derive(serde::Deserialize, Default)]
struct ProviderBody {
    name: Option<String>,
    protocol: Option<String>,
    endpoint: Option<String>,
    key_var: Option<String>,
    models: Option<Vec<String>>,
    enabled: Option<bool>,
}

/// Path prefix preceding the provider id in the JSON API routes.
const PROVIDERS_PREFIX: &str = "/b/llm/api/providers/";

/// Render a `ProviderConfig` as the JSON shape returned by list/create/update.
fn provider_to_json(id: &str, cfg: &ProviderConfig) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "name": cfg.name,
        "protocol": cfg.protocol.as_str(),
        "endpoint": cfg.endpoint,
        "key_var": cfg.key_var,
        "models": cfg.models,
        "enabled": cfg.enabled,
    })
}

/// Reload all enabled providers from the DB and push the snapshot into the
/// in-memory provider router via [`ProviderAdmin::configure`].
///
/// This is the single choke point where stored rows become live
/// `ProviderConfig`s: rows are decoded via [`row_to_config`] (which never
/// yields an `api_key`) and each config's `key_var` is resolved into
/// `api_key` here, via the config client, before `configure()`. Secret
/// rotation therefore takes effect on the next reload (boot or any provider
/// CRUD write), not per chat request.
///
/// Shared by the provider CRUD handlers, `LlmBlock::lifecycle(Init)`, and
/// the one-shot legacy-provider migration (which is why it takes the
/// provider-admin handle rather than the whole block).
///
/// Errors are returned to the caller; callers translate to 500. We do not
/// silently swallow — a failure here means the in-memory service is stale
/// and the admin needs to know.
pub(in crate::blocks::llm) async fn reload_provider_service(
    ctx: &dyn Context,
    provider_admin: &dyn ProviderAdmin,
) -> Result<(), String> {
    let records = db::list_all(ctx, PROVIDERS_TABLE, vec![])
        .await
        .map_err(|e| format!("provider reload list failed: {e}"))?;
    let mut configs: Vec<ProviderConfig> = Vec::with_capacity(records.len());
    for rec in &records {
        match row_to_config(rec) {
            Ok(mut cfg) if cfg.enabled => {
                resolve_provider_key(ctx, &mut cfg).await;
                configs.push(cfg);
            }
            Ok(_) => {} // disabled — skip
            Err(e) => {
                // A malformed row should not poison the whole reload —
                // drop just that one.
                tracing::warn!("skipping malformed provider row {}: {e}", rec.id);
            }
        }
    }
    provider_admin.configure(configs);
    Ok(())
}

/// Resolve a provider's `key_var` into its plaintext `api_key` via the
/// config client. `key_var` takes precedence over any inline `api_key`;
/// with no `key_var` the config is left untouched.
///
/// Resolution failure (unset var, empty value, denied read) is logged and
/// leaves `api_key` as-is — the provider then runs unauthenticated, and the
/// per-protocol encoder decides whether that's an error (`MissingApiKey` →
/// 401) on the next chat call. Local OpenAI-compatible servers legitimately
/// run without a key.
async fn resolve_provider_key(ctx: &dyn Context, cfg: &mut ProviderConfig) {
    let Some(var) = cfg.key_var.as_deref() else {
        return;
    };
    match config::get(ctx, var).await {
        Ok(value) if !value.is_empty() => cfg.api_key = Some(value),
        Ok(_) => tracing::warn!(
            "provider '{}': key_var `{var}` is set but empty — provider will run unauthenticated",
            cfg.name
        ),
        Err(e) => tracing::warn!(
            "provider '{}': failed to resolve key_var `{var}`: {e} — provider will run unauthenticated",
            cfg.name
        ),
    }
}

/// `GET /b/llm/api/providers` — list all rows. Admin-only.
pub(super) async fn list_providers(
    _block: &LlmBlock,
    ctx: &dyn Context,
    _msg: &Message,
) -> OutputStream {
    let records = match db::list_all(ctx, PROVIDERS_TABLE, vec![]).await {
        Ok(r) => r,
        Err(e) => return err_internal("Database error", e),
    };
    let providers: Vec<serde_json::Value> = records
        .iter()
        .filter_map(|rec| {
            row_to_config(rec)
                .ok()
                .map(|cfg| provider_to_json(&rec.id, &cfg))
        })
        .collect();
    ok_json(&serde_json::json!({ "providers": providers }))
}

/// `POST /b/llm/api/providers` — create. Body must include `name`,
/// `protocol`, `endpoint`. `key_var`, `models`, `enabled` optional. Admin-only.
pub(super) async fn create_provider(
    block: &LlmBlock,
    ctx: &dyn Context,
    _msg: &Message,
    input: InputStream,
) -> OutputStream {
    let raw = input.collect_to_bytes().await;
    let body: ProviderBody = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    let Some(name) = body
        .name
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return err_bad_request("`name` is required");
    };
    let Some(protocol_str) = body.protocol.as_deref().filter(|s| !s.is_empty()) else {
        return err_bad_request("`protocol` is required");
    };
    let Some(protocol) = ProviderProtocol::parse(protocol_str) else {
        return err_bad_request(&format!(
            "invalid `protocol` `{protocol_str}` — expected `open_ai`, `anthropic`, or `open_ai_compatible`"
        ));
    };
    let Some(endpoint) = body
        .endpoint
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return err_bad_request("`endpoint` is required");
    };

    let mut cfg = ProviderConfig::new(name, protocol, endpoint);
    if let Some(k) = body.key_var.filter(|s| !s.is_empty()) {
        cfg.key_var = Some(k);
    }
    if let Some(m) = body.models {
        cfg.models = m;
    }
    if let Some(e) = body.enabled {
        cfg.enabled = e;
    }

    let mut data = config_to_row(&cfg);
    crate::util::stamp_created(&mut data);

    let record = match db::create(ctx, PROVIDERS_TABLE, data).await {
        Ok(r) => r,
        Err(e) => return err_internal("Database error", e),
    };

    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return err_internal("reload_provider_service failed", e);
    }

    ok_json(&provider_to_json(&record.id, &cfg))
}

/// `PATCH /b/llm/api/providers/:id` — partial update. Admin-only.
pub(super) async fn update_provider(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let id = path_param(msg, "id", PROVIDERS_PREFIX).to_string();
    if id.is_empty() {
        return err_bad_request("Missing provider ID");
    }

    let raw = input.collect_to_bytes().await;
    let body: ProviderBody = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Load existing record so we can apply the patch on top of stored values.
    let existing = match db::get(ctx, PROVIDERS_TABLE, &id).await {
        Ok(r) => r,
        Err(e) if e.code == wafer_run::ErrorCode::NotFound => {
            return err_not_found("Provider not found")
        }
        Err(e) => return err_internal("Database error", e),
    };
    let mut cfg = match row_to_config(&existing) {
        Ok(c) => c,
        Err(e) => return err_internal("Stored provider row invalid", e),
    };

    if let Some(n) = body.name.filter(|s| !s.is_empty()) {
        cfg.name = n;
    }
    if let Some(p) = body.protocol.as_deref().filter(|s| !s.is_empty()) {
        match ProviderProtocol::parse(p) {
            Some(parsed) => cfg.protocol = parsed,
            None => {
                return err_bad_request(&format!(
                    "invalid `protocol` `{p}` — expected `open_ai`, `anthropic`, or `open_ai_compatible`"
                ))
            }
        }
    }
    if let Some(e) = body.endpoint.filter(|s| !s.is_empty()) {
        cfg.endpoint = e;
    }
    if let Some(k) = body.key_var {
        cfg.key_var = if k.is_empty() { None } else { Some(k) };
    }
    if let Some(m) = body.models {
        cfg.models = m;
    }
    if let Some(e) = body.enabled {
        cfg.enabled = e;
    }

    let mut data = config_to_row(&cfg);
    crate::util::stamp_updated(&mut data);

    let record = match db::update(ctx, PROVIDERS_TABLE, &id, data).await {
        Ok(r) => r,
        Err(e) if e.code == wafer_run::ErrorCode::NotFound => {
            return err_not_found("Provider not found")
        }
        Err(e) => return err_internal("Database error", e),
    };

    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return err_internal("reload_provider_service failed", e);
    }

    ok_json(&provider_to_json(&record.id, &cfg))
}

/// `DELETE /b/llm/api/providers/:id` — remove. Admin-only.
pub(super) async fn delete_provider(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let id = path_param(msg, "id", PROVIDERS_PREFIX).to_string();
    if id.is_empty() {
        return err_bad_request("Missing provider ID");
    }
    match db::delete(ctx, PROVIDERS_TABLE, &id).await {
        Ok(()) => {}
        Err(e) if e.code == wafer_run::ErrorCode::NotFound => {
            return err_not_found("Provider not found")
        }
        Err(e) => return err_internal("Database error", e),
    }

    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return err_internal("reload_provider_service failed", e);
    }

    ok_json(&serde_json::json!({ "deleted": true }))
}

// ---------------------------------------------------------------------------
// Models endpoints (aggregated via wafer-run/llm service block)
// ---------------------------------------------------------------------------
//
// The service block aggregates `list_models` across every registered
// `LlmService` impl in its router. `status` / `load` / `unload` are
// per-(backend_id, model_id) ops forwarded verbatim. These handlers only
// marshal HTTP ⇄ service-block JSON — no business logic here.

/// Extract `(backend_id, model_id)` from
/// `/b/llm/api/models/{backend_id}/{model_id}[/suffix]`.
///
/// Prefers the router-supplied path variables when available, falling back
/// to splitting `msg.path()` on `/`. Both ids may contain
/// backend-specific characters (`-`, `_`, `.`, but not `/`), so a single
/// split-on-`/` round yields the right segments.
fn extract_model_path(msg: &Message) -> (String, String) {
    let backend_var = msg.var("backend_id");
    let model_var = msg.var("model_id");
    if !backend_var.is_empty() && !model_var.is_empty() {
        return (backend_var.to_string(), model_var.to_string());
    }
    let path = msg.path();
    let suffix = path.strip_prefix("/b/llm/api/models/").unwrap_or("");
    let mut parts = suffix.splitn(3, '/');
    let backend = parts.next().unwrap_or("").to_string();
    let model = parts.next().unwrap_or("").to_string();
    (backend, model)
}

/// `GET /b/llm/api/models` — aggregated list across all registered LLM
/// backends. Authenticated (any logged-in user).
pub(super) async fn list_models(
    _block: &LlmBlock,
    ctx: &dyn Context,
    _msg: &Message,
) -> OutputStream {
    match llm_client::list_models(ctx).await {
        Ok(models) => ok_json(&serde_json::json!({ "models": models })),
        Err(e) => err_internal("llm list_models failed", e.message),
    }
}

/// `GET /b/llm/api/models/:backend_id/:model_id/status` — per-(backend, model)
/// status. Authenticated.
pub(super) async fn model_status(
    _block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let (backend_id, model_id) = extract_model_path(msg);
    if backend_id.is_empty() || model_id.is_empty() {
        return err_bad_request("Missing backend_id or model_id");
    }
    let req = StatusRequest {
        backend_id,
        model_id,
    };
    match llm_client::status(ctx, &req).await {
        Ok(status) => ok_json(&serde_json::json!({ "status": status })),
        Err(e) => err_internal("llm status failed", e.message),
    }
}

/// `POST /b/llm/api/models/:backend_id/:model_id/load` — start a model
/// load, streaming `LoadProgress` events as SSE. Admin-only.
pub(super) async fn load_model(
    _block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let (backend_id, model_id) = extract_model_path(msg);
    if backend_id.is_empty() || model_id.is_empty() {
        return err_bad_request("Missing backend_id or model_id");
    }
    let req = LoadModelRequest {
        backend_id,
        model_id,
    };
    let stream = match llm_client::load_model_stream(ctx, &req).await {
        Ok(s) => s,
        Err(e) => return err_internal("llm load_model failed", e.message),
    };

    sse_json_response(stream)
}

/// `POST /b/llm/api/models/:backend_id/:model_id/unload` — buffered unload.
/// Admin-only.
pub(super) async fn unload_model(
    _block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let (backend_id, model_id) = extract_model_path(msg);
    if backend_id.is_empty() || model_id.is_empty() {
        return err_bad_request("Missing backend_id or model_id");
    }
    let req = UnloadModelRequest {
        backend_id,
        model_id,
    };
    match llm_client::unload_model(ctx, &req).await {
        Ok(()) => ok_json(&serde_json::json!({ "unloaded": true })),
        Err(e) => err_internal("llm unload_model failed", e.message),
    }
}

/// `POST /b/llm/api/providers/:id/discover-models` — call the provider's
/// `/v1/models` endpoint, persist the discovered list back to the row, and
/// return the new model list. Admin-only.
pub(super) async fn discover_models(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let id = path_param(msg, "id", PROVIDERS_PREFIX).to_string();
    if id.is_empty() {
        return err_bad_request("Missing provider ID");
    }

    // Resolve the provider name from the row — discover_models is keyed by
    // provider name (== ProviderConfig::name), not by row id.
    let existing = match db::get(ctx, PROVIDERS_TABLE, &id).await {
        Ok(r) => r,
        Err(e) if e.code == wafer_run::ErrorCode::NotFound => {
            return err_not_found("Provider not found")
        }
        Err(e) => return err_internal("Database error", e),
    };
    let mut cfg = match row_to_config(&existing) {
        Ok(c) => c,
        Err(e) => return err_internal("Stored provider row invalid", e),
    };

    // Make sure the in-memory service knows about this provider — discover
    // looks up by name, and the service may be empty if the process just
    // started or the row is disabled (and so was excluded from the last
    // configure call).
    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return err_internal("reload_provider_service failed", e);
    }

    let models = match block.provider_admin.discover_models(&cfg.name).await {
        Ok(m) => m,
        Err(e) => return err_internal("discover_models failed", format!("{e:?}")),
    };
    cfg.models = models.into_iter().map(|m| m.model_id).collect();

    let mut data = config_to_row(&cfg);
    crate::util::stamp_updated(&mut data);
    if let Err(e) = db::update(ctx, PROVIDERS_TABLE, &id, data).await {
        return err_internal("Database error", e);
    }

    if let Err(e) = reload_provider_service(ctx, block.provider_admin.as_ref()).await {
        return err_internal("reload_provider_service failed", e);
    }

    ok_json(&serde_json::json!({ "models": cfg.models }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wafer_run::{context::Context, streams::output::TerminalNotResponse, ErrorCode};

    use super::*;
    use crate::blocks::llm::{provider_admin::NoopProviderAdmin, providers::ProviderLlmService};

    /// Minimal Context that panics on `call_block` — the bad-request test
    /// must reject before any block dispatch.
    #[derive(Clone)]
    struct PanicCtx;

    #[async_trait::async_trait]
    impl Context for PanicCtx {
        async fn call_block(
            &self,
            _block_name: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            panic!("call_block must not be invoked on a parse-error path");
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }
        fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    fn stub_block() -> LlmBlock {
        // The parse-error tests reject before reaching the provider-admin
        // surface, so the no-op handle suffices.
        LlmBlock::new(Arc::new(NoopProviderAdmin))
    }

    /// One recorded `call_block` invocation on a [`RecordingCtx`].
    struct RecordedCall {
        block_name: String,
        msg: Message,
        body: Vec<u8>,
    }

    /// Context that records every `call_block` invocation (block name,
    /// message, drained input body) and answers with a canned OK JSON body.
    /// `clone_arc` hands out a handle sharing the same call log, so a test
    /// can inspect calls made through the cloned Arc.
    #[derive(Clone, Default)]
    struct RecordingCtx {
        calls: Arc<std::sync::Mutex<Vec<RecordedCall>>>,
    }

    impl RecordingCtx {
        fn calls(&self) -> std::sync::MutexGuard<'_, Vec<RecordedCall>> {
            self.calls.lock().expect("call log lock")
        }
    }

    #[async_trait::async_trait]
    impl Context for RecordingCtx {
        async fn call_block(
            &self,
            block_name: &str,
            msg: Message,
            input: InputStream,
        ) -> OutputStream {
            let body = input.collect_to_bytes().await;
            self.calls().push(RecordedCall {
                block_name: block_name.to_string(),
                msg,
                body,
            });
            OutputStream::respond(br#"{"id":"entry-1"}"#.to_vec())
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }
        fn clone_arc(&self) -> Arc<dyn Context> {
            Arc::new(self.clone())
        }
    }

    // -----------------------------------------------------------------
    // sse_chat_response — streamed assistant-turn persistence
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn sse_chat_response_persists_assistant_turn_at_done() {
        let ctx = RecordingCtx::default();
        let msg = Message::new("create:/b/llm/api/chat/stream");
        let chunks: Vec<Result<ChatChunk, wafer_run::WaferError>> =
            vec![Ok(ChatChunk::text("Hel")), Ok(ChatChunk::text("lo"))];

        let out = sse_chat_response(
            futures::stream::iter(chunks),
            ctx.clone_arc(),
            msg,
            "thread-1".to_string(),
        );
        let buf = out.collect_buffered().await.expect("stream completes");
        let body = String::from_utf8(buf.body).expect("SSE body is utf8");

        assert!(
            body.ends_with("data: [DONE]\n\n"),
            "expected terminal [DONE] frame, got: {body}"
        );
        assert!(
            body.contains("Hel") && body.contains("lo"),
            "both text deltas must be forwarded as frames, got: {body}"
        );
        assert!(
            buf.meta
                .iter()
                .any(|m| m.key == META_RESP_CONTENT_TYPE && m.value == "text/event-stream"),
            "content-type meta must announce text/event-stream"
        );

        let calls = ctx.calls();
        assert_eq!(
            calls.len(),
            1,
            "expected exactly one persistence call, got {}",
            calls.len()
        );
        let call = &calls[0];
        assert_eq!(call.block_name, "impresspress/messages");
        assert_eq!(
            call.msg.get_meta("req.resource"),
            "/b/messages/api/contexts/thread-1/entries"
        );
        let body_json: serde_json::Value =
            serde_json::from_slice(&call.body).expect("persistence body is JSON");
        assert_eq!(body_json["role"], "assistant");
        assert_eq!(body_json["content"], "Hello");
    }

    #[tokio::test]
    async fn sse_chat_response_skips_persistence_when_stream_errors() {
        let ctx = RecordingCtx::default();
        let msg = Message::new("create:/b/llm/api/chat/stream");
        let chunks: Vec<Result<ChatChunk, wafer_run::WaferError>> = vec![
            Ok(ChatChunk::text("partial")),
            Err(wafer_run::WaferError::new(
                ErrorCode::Internal,
                "backend died",
            )),
        ];

        let out = sse_chat_response(
            futures::stream::iter(chunks),
            ctx.clone_arc(),
            msg,
            "thread-1".to_string(),
        );
        let buf = out
            .collect_buffered()
            .await
            .expect("producer auto-completes");
        let body = String::from_utf8(buf.body).expect("SSE body is utf8");

        assert!(
            body.ends_with("event: error\ndata: {}\n\n"),
            "expected terminal error frame, got: {body}"
        );
        assert!(!body.contains("[DONE]"), "no [DONE] after an error frame");
        assert!(
            ctx.calls().is_empty(),
            "an errored stream must not persist an assistant turn (mirrors handle_chat)"
        );
    }

    #[tokio::test]
    async fn sse_chat_response_caps_persisted_content() {
        let ctx = RecordingCtx::default();
        let msg = Message::new("create:/b/llm/api/chat/stream");
        let head = "a".repeat(MAX_BUFFERED_RESPONSE_BYTES);
        let chunks: Vec<Result<ChatChunk, wafer_run::WaferError>> =
            vec![Ok(ChatChunk::text(head)), Ok(ChatChunk::text("overflow"))];

        let out = sse_chat_response(
            futures::stream::iter(chunks),
            ctx.clone_arc(),
            msg,
            "thread-1".to_string(),
        );
        let buf = out.collect_buffered().await.expect("stream completes");
        let body = String::from_utf8(buf.body).expect("SSE body is utf8");

        // The overflowing delta is still forwarded to the client...
        assert!(
            body.contains("overflow"),
            "frames keep flowing past the cap"
        );
        assert!(body.ends_with("data: [DONE]\n\n"), "still ends with [DONE]");

        // ...but the persisted assistant message stops at the cap.
        let calls = ctx.calls();
        assert_eq!(calls.len(), 1, "exactly one persistence call");
        let body_json: serde_json::Value =
            serde_json::from_slice(&calls[0].body).expect("persistence body is JSON");
        let content = body_json["content"].as_str().expect("content is a string");
        assert_eq!(
            content.len(),
            MAX_BUFFERED_RESPONSE_BYTES,
            "persisted content stops at the cap (overflowing delta skipped)"
        );
        assert!(
            !content.contains("overflow"),
            "the overflowing delta must not be persisted"
        );
    }

    #[tokio::test]
    async fn handle_chat_returns_bad_request_on_invalid_json() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = Message::new("create:/b/llm/api/chat");
        let input = InputStream::from_bytes(b"not json".to_vec());

        let out = handle_chat(&block, &ctx, &msg, input).await;
        let result = out.collect_buffered().await;
        match result {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(
                    e.message.contains("Invalid body"),
                    "expected Invalid body message, got: {}",
                    e.message
                );
            }
            other => panic!("expected InvalidArgument error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_chat_stream_returns_bad_request_on_invalid_json() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = Message::new("create:/b/llm/api/chat/stream");
        let input = InputStream::from_bytes(b"{".to_vec());

        let out = handle_chat_stream(&block, &ctx, &msg, input).await;
        let result = out.collect_buffered().await;
        match result {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
            }
            other => panic!("expected InvalidArgument error, got {other:?}"),
        }
    }

    #[test]
    fn role_from_str_maps_known_roles() {
        assert_eq!(role_from_str("user"), ChatRole::User);
        assert_eq!(role_from_str("assistant"), ChatRole::Assistant);
        assert_eq!(role_from_str("system"), ChatRole::System);
    }

    #[test]
    fn role_from_str_unknown_falls_back_to_user() {
        assert_eq!(role_from_str("tool"), ChatRole::User);
        assert_eq!(role_from_str(""), ChatRole::User);
        assert_eq!(role_from_str("random"), ChatRole::User);
    }

    #[test]
    fn history_to_messages_prefers_data_object() {
        let history = vec![serde_json::json!({
            "data": { "role": "user", "content": "hi" }
        })];
        let msgs = history_to_messages(&history);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, ChatRole::User);
        assert!(
            matches!(&msgs[0].content, wafer_block::wire::llm::ChatContent::Text(t) if t == "hi")
        );
    }

    #[test]
    fn history_to_messages_falls_back_to_flat_fields() {
        let history = vec![serde_json::json!({
            "role": "assistant",
            "content": "yes"
        })];
        let msgs = history_to_messages(&history);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, ChatRole::Assistant);
        assert!(
            matches!(&msgs[0].content, wafer_block::wire::llm::ChatContent::Text(t) if t == "yes")
        );
    }

    // -----------------------------------------------------------------
    // Provider CRUD tests
    // -----------------------------------------------------------------
    //
    // These cover the three admin/parse paths that don't need a DB or an
    // HTTP backend: admin-guard denial, JSON-parse errors, and the
    // path-extraction helper. End-to-end tests (DB write + service reload)
    // live in the integration suite.

    fn admin_msg(action: &str, path: &str) -> Message {
        let mut m = Message::new(format!("{action}:{path}"));
        m.set_meta(wafer_run::META_REQ_ACTION, action);
        m.set_meta(wafer_run::META_REQ_RESOURCE, path);
        m.set_meta(wafer_run::META_AUTH_USER_ID, "admin-user");
        m.set_meta("auth.user_roles", "admin");
        m
    }

    fn user_msg(action: &str, path: &str) -> Message {
        let mut m = Message::new(format!("{action}:{path}"));
        m.set_meta(wafer_run::META_REQ_ACTION, action);
        m.set_meta(wafer_run::META_REQ_RESOURCE, path);
        m.set_meta(wafer_run::META_AUTH_USER_ID, "regular-user");
        m.set_meta("auth.user_roles", "user");
        m
    }

    #[tokio::test]
    async fn create_provider_returns_bad_request_on_invalid_json() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("create", "/b/llm/api/providers");
        let input = InputStream::from_bytes(b"not json".to_vec());

        let out = create_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(
                    e.message.contains("Invalid body"),
                    "expected Invalid body, got: {}",
                    e.message
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_provider_requires_name() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("create", "/b/llm/api/providers");
        let input =
            InputStream::from_bytes(br#"{"protocol":"open_ai","endpoint":"https://x"}"#.to_vec());

        let out = create_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(e.message.contains("name"), "got: {}", e.message);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_provider_rejects_unknown_protocol() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("create", "/b/llm/api/providers");
        let input = InputStream::from_bytes(
            br#"{"name":"x","protocol":"openai","endpoint":"https://x"}"#.to_vec(),
        );

        let out = create_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(e.message.contains("protocol"), "got: {}", e.message);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_provider_requires_id() {
        let block = stub_block();
        let ctx = PanicCtx;
        // Path has no id segment after the prefix.
        let msg = admin_msg("update", "/b/llm/api/providers/");
        let input = InputStream::from_bytes(b"{}".to_vec());

        let out = update_provider(&block, &ctx, &msg, input).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(e.message.contains("provider ID"), "got: {}", e.message);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_provider_requires_id() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("delete", "/b/llm/api/providers/");

        let out = delete_provider(&block, &ctx, &msg).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn extract_provider_id_from_path() {
        // Direct id at end of path
        let mut m = Message::new("update:/b/llm/api/providers/abc123");
        m.set_meta(wafer_run::META_REQ_RESOURCE, "/b/llm/api/providers/abc123");
        assert_eq!(path_param(&m, "id", PROVIDERS_PREFIX), "abc123");

        // Id followed by a sub-resource (discover-models)
        let mut m2 = Message::new("create:/b/llm/api/providers/abc123/discover-models");
        m2.set_meta(
            wafer_run::META_REQ_RESOURCE,
            "/b/llm/api/providers/abc123/discover-models",
        );
        assert_eq!(path_param(&m2, "id", PROVIDERS_PREFIX), "abc123");

        // Empty when no id provided
        let mut m3 = Message::new("delete:/b/llm/api/providers/");
        m3.set_meta(wafer_run::META_REQ_RESOURCE, "/b/llm/api/providers/");
        assert_eq!(path_param(&m3, "id", PROVIDERS_PREFIX), "");

        // `msg.var("id")` takes precedence
        let mut m4 = Message::new("update:/b/llm/api/providers/from-path");
        m4.set_meta(
            wafer_run::META_REQ_RESOURCE,
            "/b/llm/api/providers/from-path",
        );
        m4.set_meta(
            format!("{}id", wafer_run::META_REQ_PARAM_PREFIX),
            "from-var",
        );
        assert_eq!(path_param(&m4, "id", PROVIDERS_PREFIX), "from-var");
    }

    #[test]
    fn provider_to_json_shape() {
        let cfg = ProviderConfig::new(
            "openai-main",
            ProviderProtocol::OpenAi,
            "https://api.openai.com/v1",
        )
        .with_key_var("IMPRESSPRESS__LLM__OPENAI_KEY")
        .with_models(vec!["gpt-4o".into()]);
        let v = provider_to_json("row-1", &cfg);
        assert_eq!(v["id"], "row-1");
        assert_eq!(v["name"], "openai-main");
        assert_eq!(v["protocol"], "open_ai");
        assert_eq!(v["endpoint"], "https://api.openai.com/v1");
        assert_eq!(v["key_var"], "IMPRESSPRESS__LLM__OPENAI_KEY");
        assert_eq!(v["models"], serde_json::json!(["gpt-4o"]));
        assert_eq!(v["enabled"], true);
        assert!(
            v.get("api_key").is_none(),
            "api_key must never appear in API output"
        );
    }

    // -----------------------------------------------------------------
    // Models endpoints tests
    // -----------------------------------------------------------------
    //
    // These cover the paths that don't need a live `wafer-run/llm`
    // dispatch: admin-guard denial for `load`/`unload`, bad-request on
    // missing path vars, and the path-extraction helper.

    #[tokio::test]
    async fn load_model_requires_path_vars() {
        let block = stub_block();
        let ctx = PanicCtx;
        // Admin but missing segments after the prefix.
        let msg = admin_msg("create", "/b/llm/api/models//load");

        let out = load_model(&block, &ctx, &msg).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
                assert!(
                    e.message.contains("backend_id") || e.message.contains("model_id"),
                    "got: {}",
                    e.message
                );
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unload_model_requires_path_vars() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = admin_msg("create", "/b/llm/api/models/openai/");

        let out = unload_model(&block, &ctx, &msg).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn model_status_requires_path_vars() {
        let block = stub_block();
        let ctx = PanicCtx;
        let msg = user_msg("retrieve", "/b/llm/api/models//status");

        let out = model_status(&block, &ctx, &msg).await;
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::InvalidArgument);
            }
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }

    #[test]
    fn extract_model_path_from_suffix() {
        // Straight {backend_id}/{model_id}/status
        let mut m = Message::new("retrieve:/b/llm/api/models/openai/gpt-4o/status");
        m.set_meta(
            wafer_run::META_REQ_RESOURCE,
            "/b/llm/api/models/openai/gpt-4o/status",
        );
        assert_eq!(
            extract_model_path(&m),
            ("openai".to_string(), "gpt-4o".to_string())
        );

        // Load sub-resource with a model id containing dots/dashes
        let mut m2 = Message::new("create:/b/llm/api/models/webllm/llama-3.1-8b/load");
        m2.set_meta(
            wafer_run::META_REQ_RESOURCE,
            "/b/llm/api/models/webllm/llama-3.1-8b/load",
        );
        assert_eq!(
            extract_model_path(&m2),
            ("webllm".to_string(), "llama-3.1-8b".to_string())
        );

        // Missing model_id
        let mut m3 = Message::new("create:/b/llm/api/models/openai/");
        m3.set_meta(wafer_run::META_REQ_RESOURCE, "/b/llm/api/models/openai/");
        let (b, m_id) = extract_model_path(&m3);
        assert_eq!(b, "openai");
        assert_eq!(m_id, "");

        // Router-provided path variables take precedence over the path string.
        let mut m4 = Message::new("create:/b/llm/api/models/from-path/ignored/load");
        m4.set_meta(
            wafer_run::META_REQ_RESOURCE,
            "/b/llm/api/models/from-path/ignored/load",
        );
        m4.set_meta(
            format!("{}backend_id", wafer_run::META_REQ_PARAM_PREFIX),
            "from-var",
        );
        m4.set_meta(
            format!("{}model_id", wafer_run::META_REQ_PARAM_PREFIX),
            "var-model",
        );
        assert_eq!(
            extract_model_path(&m4),
            ("from-var".to_string(), "var-model".to_string())
        );
    }

    #[test]
    fn history_to_messages_skips_entries_without_role() {
        let history = vec![
            serde_json::json!({ "content": "orphan" }),
            serde_json::json!({ "role": "system", "content": "kept" }),
        ];
        let msgs = history_to_messages(&history);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, ChatRole::System);
    }

    // -----------------------------------------------------------------
    // reload_provider_service — key_var resolution
    // -----------------------------------------------------------------

    /// End-to-end reload over a real in-memory DB + config block:
    /// a row whose `key_var` resolves gets its `api_key` populated, a row
    /// without `key_var` stays unauthenticated, and an unresolvable
    /// `key_var` degrades to no key (warn) instead of failing the reload.
    #[tokio::test]
    async fn reload_provider_service_resolves_key_var_into_api_key() {
        use wafer_core::{
            interfaces::config::service::ConfigService,
            service_blocks::config::{ConfigBlock, EnvConfigService},
        };

        use crate::test_support::TestContext;

        let mut ctx = TestContext::with_admin().await;
        {
            use crate::blocks::llm::migrations;
            let sqlite: Vec<&str> = migrations::SQLITE_MIGRATIONS
                .iter()
                .map(|(_, sql)| *sql)
                .collect();
            crate::migration_helper::apply_migrations(
                &ctx,
                "impresspress/llm",
                &sqlite,
                migrations::POSTGRES_MIGRATIONS,
            )
            .await
            .expect("apply llm migrations");
        }

        let config_svc = Arc::new(EnvConfigService::new());
        config_svc.set("IMPRESSPRESS__LLM__OPENAI_KEY", "sk-resolved");
        ctx.register_block("wafer-run/config", Arc::new(ConfigBlock::new(config_svc)));

        for cfg in [
            ProviderConfig::new(
                "with-key-var",
                ProviderProtocol::OpenAi,
                "https://api.openai.com/v1",
            )
            .with_key_var("IMPRESSPRESS__LLM__OPENAI_KEY"),
            ProviderConfig::new(
                "no-key-var",
                ProviderProtocol::OpenAiCompatible,
                "http://localhost:11434/v1",
            ),
            ProviderConfig::new(
                "unresolvable-key-var",
                ProviderProtocol::OpenAi,
                "https://api.openai.com/v1",
            )
            .with_key_var("IMPRESSPRESS__LLM__TEST_MISSING_KEY"),
        ] {
            let mut data = config_to_row(&cfg);
            crate::util::stamp_created(&mut data);
            db::create(&ctx, PROVIDERS_TABLE, data)
                .await
                .expect("create provider row");
        }

        let svc = ProviderLlmService::new();
        reload_provider_service(&ctx, &svc)
            .await
            .expect("reload succeeds");

        let by_name = |name: &str| {
            svc.providers_snapshot()
                .into_iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("provider '{name}' missing from snapshot"))
        };
        assert_eq!(
            by_name("with-key-var").api_key.as_deref(),
            Some("sk-resolved"),
            "key_var must resolve into api_key at reload"
        );
        assert_eq!(by_name("no-key-var").api_key, None);
        assert_eq!(
            by_name("unresolvable-key-var").api_key,
            None,
            "unresolvable key_var degrades to no key, not a reload failure"
        );
    }
}
