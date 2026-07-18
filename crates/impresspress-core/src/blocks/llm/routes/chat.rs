//! Chat request handling.
//!
//! Both the buffered and streaming chat endpoints share [`dispatch_chat`]:
//! parse the body, persist the user message, load history, resolve the
//! provider + model, and call `wafer-run/llm` via the typed client. The
//! buffered handler ([`handle_chat`]) drains the resulting `ChatChunk`
//! stream itself; the streaming handler ([`handle_chat_stream`]) hands it
//! off to [`super::streaming::sse_chat_response`], which owns the SSE
//! framing.

use futures::StreamExt;
use wafer_core::clients::{
    llm::{
        self as llm_client, ChatChunk, ChatContent, ChatMessage, ChatParams, ChatRequest, ChatRole,
        ChunkDelta,
    },
    NativeTypedFrameStream,
};
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::streaming::sse_chat_response;
use crate::{
    blocks::llm::{messages_create, messages_list, LlmBlock, DEFAULT_PROVIDER},
    http::{err_bad_request, err_internal, ok_json},
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
///
/// [`ProviderAdmin`]: crate::blocks::llm::provider_admin::ProviderAdmin
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
///
/// Shared with [`super::streaming::sse_chat_response`], which applies the
/// same cap to the persisted (not the forwarded) assistant text.
pub(super) const MAX_BUFFERED_RESPONSE_BYTES: usize = 1024 * 1024;

/// Buffered chat handler: collects the full `ChatChunk` stream, concatenates
/// all text deltas, persists the assistant message, and returns a JSON body.
pub(in crate::blocks::llm) async fn handle_chat(
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
pub(in crate::blocks::llm) async fn handle_chat_stream(
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

#[cfg(test)]
mod tests {
    use wafer_run::{streams::output::TerminalNotResponse, ErrorCode};

    use super::*;
    use crate::blocks::llm::routes::test_support::{stub_block, PanicCtx};

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
}
