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
        ChunkDelta, FinishReason,
    },
    NativeTypedFrameStream,
};
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::streaming::sse_chat_response;
use crate::{
    blocks::{
        llm::{
            contracts, default_max_tokens, messages_create, messages_list, record_field, LlmBlock,
            DEFAULT_PROVIDER,
        },
        messages::contracts::EntryRole,
    },
    http::{err_bad_request, err_unavailable, ok_json},
};

/// Legacy default provider block name that must be replaced with the first
/// enabled provider from `impresspress__llm__providers` before the request
/// reaches the `wafer-run/llm` service.
const LEGACY_PROVIDER_BLOCK: &str = DEFAULT_PROVIDER;

/// The messages block's role as the LLM service's [`ChatRole`].
///
/// Total by construction, which is the whole of B20's fix. The function this
/// replaces matched `"assistant"` and `"system"` and sent **everything else**
/// to [`ChatRole::User`] — including `"agent"`, the one role the messages
/// composer offers. So an entry an agent posted came back to the model as
/// the user's own next instruction, and adding a role to the messages block
/// would have silently done the same thing again. A new [`EntryRole`]
/// variant now fails to compile here instead.
fn chat_role(role: EntryRole) -> ChatRole {
    match role {
        EntryRole::User => ChatRole::User,
        EntryRole::Assistant => ChatRole::Assistant,
        EntryRole::System => ChatRole::System,
    }
}

/// Build a text-content `ChatMessage` for the given role.
///
/// `ChatRole::Tool` is unreachable via [`chat_role`] (no [`EntryRole`] maps
/// to it), but if it ever bubbles up here a tool-result message would
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
/// interface expects.
///
/// An entry whose `role` is not an [`EntryRole`] is skipped, which is what
/// already happened to the rows this can still see: `role`'s column default
/// was `''` before the messages block typed it, and an empty role has always
/// been dropped here. Skipping is deliberately not the same as the old
/// fallback — an unreadable role must not become a *user* turn, because that
/// puts words the user never wrote into the model's input.
fn history_to_messages(history: &[serde_json::Value]) -> Vec<ChatMessage> {
    history
        .iter()
        .filter_map(|entry| {
            let role: EntryRole = serde_json::from_value(serde_json::Value::String(
                record_field(entry, "role").to_string(),
            ))
            .ok()?;
            Some(build_text_message(
                chat_role(role),
                record_field(entry, "content").to_string(),
            ))
        })
        .collect()
}

/// Resolve a legacy `impresspress/provider-llm` default into a concrete
/// backend_id by reading the in-memory provider cache (loaded at `Init` and
/// refreshed on every provider CRUD write) via the [`ProviderAdmin`] handle.
/// With no enabled provider, that is the deployment's configuration, not a
/// fault: the answer is a 503 telling the caller what an admin has to do.
///
/// [`ProviderAdmin`]: crate::blocks::llm::provider_admin::ProviderAdmin
fn resolve_backend_id(block: &LlmBlock, provider_block: &str) -> Result<String, OutputStream> {
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
        .ok_or_else(|| {
            err_unavailable(
                "No LLM provider is enabled on this deployment: an admin must add \
                 and enable one on the LLM Providers page (/b/llm/providers)",
            )
        })
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
    let raw = input
        .collect_to_bytes()
        .await
        .map_err(OutputStream::error)?;
    let contracts::ChatRequest {
        thread_id,
        message,
        provider,
        model,
        max_tokens,
    } = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return Err(err_bad_request(&format!("Invalid body: {e}"))),
    };
    // A caller asking for zero output tokens is asking for no answer at all,
    // and Anthropic answers `400` to it. Refused here, before the turn is
    // stored, rather than after a round-trip to be told the same thing.
    if max_tokens == Some(0) {
        return Err(err_bad_request("max_tokens must be greater than zero"));
    }

    // 1. Persist the user message before calling the model — and refuse if it
    //    did not land. A turn that was not stored must not be followed by a
    //    model call: the next request rebuilds the history from the store, so
    //    the question this answer belongs to would simply be gone, and the
    //    operator would have paid for the completion anyway. This is also the
    //    prelude's first read of the thread, so the messages block's
    //    `NotFound` for an unknown `thread_id` is the caller's 404 here.
    if let Err(error) = messages_create(ctx, msg, &thread_id, EntryRole::User, &message).await {
        return Err(crate::blocks::crud::db_error(
            error,
            "Thread not found",
            "Chat message",
        ));
    }

    // 2. Load prior history (which now includes the just-written user msg).
    //    A history read that FAILED is not an empty conversation: prompting a
    //    paid provider with no context because the store was unreachable
    //    charges for an answer to the wrong question.
    let history = match messages_list(ctx, msg, &thread_id).await {
        Ok(history) => history,
        Err(e) => return Err(crate::blocks::crud::db_error_internal(e, "Chat history")),
    };
    let messages = history_to_messages(&history);

    // 3. Resolve the provider block / model via the block's existing logic.
    //    An unreadable per-thread override is an error, not a silent fall
    //    back to the global default: the caller pinned a backend and would
    //    otherwise be billed to another one without ever learning.
    let (provider_block, resolved_model) = match block
        .resolve_provider(ctx, &thread_id, provider.as_deref(), model.as_deref())
        .await
    {
        Ok(resolved) => resolved,
        Err(e) => {
            return Err(crate::blocks::crud::db_error_internal(
                e,
                "resolve_provider failed",
            ))
        }
    };

    // 4. Map the legacy `impresspress/provider-llm` default into a concrete
    //    backend_id (first enabled provider). Non-legacy values pass through.
    let backend_id = match resolve_backend_id(block, &provider_block) {
        Ok(id) => id,
        Err(refusal) => return Err(refusal),
    };

    // 5. Build the service request and dispatch via the typed client.
    //
    //    Every request carries an output-token budget: the caller's, or the
    //    deployment default. Anthropic's Messages API requires `max_tokens`,
    //    so a request without one never reaches the provider — the encoder
    //    refuses it (`providers::anthropic::EncodeError::MissingMaxTokens`)
    //    and the caller sees a 500. The same budget bounds OpenAI-protocol
    //    replies, which are unbounded when the field is absent.
    let max_tokens = match max_tokens {
        Some(requested) => requested,
        None => default_max_tokens(ctx).await.map_err(|e| {
            crate::blocks::crud::db_error_internal(e, "Could not read the max-token budget")
        })?,
    };
    let chat_req = ChatRequest {
        backend_id,
        model: resolved_model.clone(),
        messages,
        params: ChatParams {
            max_tokens: Some(max_tokens),
            ..ChatParams::default()
        },
        tools: Vec::new(),
        extra: serde_json::Value::Null,
    };
    let stream = match llm_client::chat_stream(ctx, &chat_req).await {
        Ok(s) => s,
        Err(e) => {
            return Err(crate::blocks::crud::db_error_internal(
                e,
                "llm chat dispatch",
            ))
        }
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
    // The model stopped because it hit the output-token budget, not because
    // it had finished. Both decoders report it (`FinishReason::Length`), and
    // it is the other way a published reply can be a fragment — invisible
    // from the text alone, which ends mid-sentence but looks like an answer.
    let mut budget_exhausted = false;
    while let Some(item) = stream.next().await {
        let chunk = match item {
            Ok(c) => c,
            Err(e) => return crate::blocks::crud::db_error_internal(e, "llm service error"),
        };
        budget_exhausted |= chunk.finish_reason == Some(FinishReason::Length);
        match chunk.delta {
            ChunkDelta::Text(s) => {
                if truncated || content.len() + s.len() > MAX_BUFFERED_RESPONSE_BYTES {
                    // Stop appending — for good, not just for this delta — but
                    // keep draining so the stream can close cleanly and any
                    // usage frame still flows through. Resuming on the next
                    // delta that happens to fit would splice the tail of the
                    // answer onto its head with the middle missing, which
                    // reads as a complete (and wrong) reply rather than a
                    // truncated one.
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
    if budget_exhausted {
        tracing::warn!(
            "llm reply stopped at the output-token budget \
             (IMPRESSPRESS__LLM__DEFAULT_MAX_TOKENS or the request's own max_tokens)"
        );
    }
    // One flag for "what you are reading is not the whole answer", whichever
    // ceiling ended it: the 1 MiB buffering cap here, or the model's own
    // token budget upstream.
    let truncated = truncated || budget_exhausted;

    // Persist the assistant reply. The model has already answered and has
    // already been paid for, but no status line has been written yet, so this
    // path still owns the one channel that can say the turn was not kept —
    // and it used to publish `message_id: ""` in a 200 instead, which renders
    // an answer that disappears on the client's next history refetch. (The
    // streaming path reaches the same decision through an SSE `error` frame,
    // because its status line is long since committed; see
    // `super::streaming::sse_chat_response`.)
    let message_id =
        match messages_create(ctx, msg, &thread_id, EntryRole::Assistant, &content).await {
            Ok(id) => id,
            Err(error) => {
                tracing::error!(
                    thread_id = %thread_id,
                    reply_bytes = content.len(),
                    error = %error,
                    "llm assistant turn was answered but could not be stored"
                );
                // The thread was written to moments ago, so a `NotFound` here is
                // a fault rather than the caller's missing row.
                return crate::blocks::crud::db_error_internal(error, "Chat reply");
            }
        };

    ok_json(&contracts::ChatResponse {
        content,
        message_id,
        model: model_used,
        truncated,
    })
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

    /// The buffered reply is the contract's four fields and nothing else.
    ///
    /// The fixture registers `impresspress/messages`, which `info().requires`
    /// lists as a dependency of this block. It used to omit it and the test
    /// still passed, because the history read swallowed its 404 into an empty
    /// list and `message_id` was published as `""` — the same swallow that let
    /// a permanently-404ing history read merge. A history read that fails now
    /// refuses, so the fixture has to carry the block the deployment does.
    #[tokio::test]
    async fn handle_chat_publishes_exactly_the_contract_fields() {
        let (ctx, thread_id, _chat_calls) = chat_fixture().await;

        let body = crate::test_support::output_json(
            handle_chat(
                &stub_block(),
                &ctx,
                &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
                chat_body(&thread_id),
            )
            .await,
        )
        .await;

        let mut got: Vec<&str> = body
            .as_object()
            .expect("chat response object")
            .keys()
            .map(String::as_str)
            .collect();
        got.sort_unstable();
        assert_eq!(
            got,
            ["content", "message_id", "model", "truncated"],
            "the wire field set must equal ChatResponse's"
        );
        assert_eq!(body["content"], "Hello");
        assert_eq!(body["model"], "stub-model");
        assert_eq!(body["truncated"], false);
        assert!(
            body["message_id"].as_str().is_some_and(|id| !id.is_empty()),
            "the stored assistant turn's id must be published, got {}",
            body["message_id"]
        );
    }

    /// A context the chat handlers can run end to end against: the messages
    /// block registered for real, a stub `wafer-run/llm` that answers
    /// `"Hello"`, and one seeded thread. Returns the thread id and the stub's
    /// chat counter, which is how a test proves the model was *not* called.
    async fn chat_fixture() -> (
        crate::test_support::TestContext,
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use wafer_core::clients::llm::FinishReason;

        chat_fixture_answering(vec![
            ChatChunk::text("Hel"),
            ChatChunk::text("lo"),
            ChatChunk::finish(FinishReason::Stop, None),
        ])
        .await
    }

    /// [`chat_fixture`] with the stub provider's answer scripted by the
    /// caller, for the tests that care what the deltas look like.
    async fn chat_fixture_answering(
        chat_chunks: Vec<ChatChunk>,
    ) -> (
        crate::test_support::TestContext,
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::sync::Arc;

        use crate::blocks::llm::{
            routes::test_support::StubLlmServiceBlock, DEFAULT_MODEL_VAR, DEFAULT_PROVIDER_VAR,
        };

        let mut ctx = crate::test_support::TestContext::with_llm().await;
        register_messages_block(&mut ctx).await;
        ctx.set_config(DEFAULT_PROVIDER_VAR, "stub-backend");
        ctx.set_config(DEFAULT_MODEL_VAR, "stub-model");
        let chat_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        ctx.register_block(
            "wafer-run/llm",
            Arc::new(StubLlmServiceBlock {
                chat_chunks,
                chat_calls: chat_calls.clone(),
                ..Default::default()
            }),
        );
        let thread = crate::blocks::messages::service::create_context(
            &ctx.fixture(),
            "user-a",
            "conversation",
            "T",
            "",
            "",
            None,
            None,
        )
        .await
        .expect("seed a thread");
        (ctx, thread.id, chat_calls)
    }

    /// The body a chat request carries for `thread_id`.
    fn chat_body(thread_id: &str) -> InputStream {
        InputStream::from_bytes(
            serde_json::to_vec(&serde_json::json!({
                "thread_id": thread_id,
                "message": "hi",
            }))
            .expect("body"),
        )
    }

    // -----------------------------------------------------------------------
    // The provider actually encodes the request
    // -----------------------------------------------------------------------
    //
    // Every test above stubs `wafer-run/llm`, so nothing in them ever encodes
    // a provider request — and an Anthropic request without `max_tokens` is
    // refused at encode time. That is why a block whose every chat through an
    // Anthropic-protocol provider failed shipped with a green suite. The
    // tests below run the real `ProviderLlmService` and the real Anthropic
    // encoder against a loopback provider, so the budget is observable as the
    // value on the wire rather than as the absence of an error.

    /// The output-token budget the fixture configures. Deliberately not
    /// [`crate::blocks::llm::DEFAULT_MAX_TOKENS`]: a test asserting the
    /// built-in default cannot tell a handler that reads the variable from
    /// one that hardcodes the same number.
    #[cfg(feature = "llm")]
    const FIXTURE_MAX_TOKENS: u32 = 321;

    /// What the fake provider answers, so a test can assert the reply came
    /// back through the decoder rather than merely that nothing failed.
    #[cfg(feature = "llm")]
    const FIXTURE_REPLY: &str = "Hi there";

    /// A chat fixture whose `wafer-run/llm` is the production service block
    /// wrapping a real [`ProviderLlmService`], routed to `fake` — a loopback
    /// provider speaking one of the three wire protocols. Returns the thread
    /// id; the fake's recorded request bodies are what the assertions read.
    #[cfg(feature = "llm")]
    async fn fixture_for(
        fake: &crate::blocks::llm::providers::fake_provider::FakeProvider,
    ) -> (crate::test_support::TestContext, String) {
        use crate::{
            blocks::llm::{DEFAULT_MODEL_VAR, DEFAULT_PROVIDER_VAR},
            llm_target::DEFAULT_MAX_TOKENS_VAR,
        };

        let mut ctx = crate::test_support::TestContext::with_llm().await;
        register_messages_block(&mut ctx).await;
        ctx.set_config(DEFAULT_PROVIDER_VAR, fake.backend_id());
        ctx.set_config(DEFAULT_MODEL_VAR, fake.model());
        ctx.set_config(DEFAULT_MAX_TOKENS_VAR, &FIXTURE_MAX_TOKENS.to_string());
        ctx.register_block("wafer-run/llm", fake.llm_service_block());
        let thread = crate::blocks::messages::service::create_context(
            &ctx.fixture(),
            "user-a",
            "conversation",
            "T",
            "",
            "",
            None,
            None,
        )
        .await
        .expect("seed a thread");
        (ctx, thread.id)
    }

    /// [`fixture_for`] an Anthropic-protocol provider, the one that refuses a
    /// request with no budget at all.
    #[cfg(feature = "llm")]
    async fn anthropic_fixture() -> (
        crate::test_support::TestContext,
        String,
        crate::blocks::llm::providers::fake_provider::FakeProvider,
    ) {
        use crate::blocks::llm::providers::fake_provider::FakeProvider;

        let fake = FakeProvider::anthropic(FIXTURE_REPLY).await;
        let (ctx, thread_id) = fixture_for(&fake).await;
        (ctx, thread_id, fake)
    }

    /// A chat through an Anthropic-protocol provider answers, and the request
    /// carries the configured output-token budget.
    ///
    /// The buffered handler sent `ChatParams::default()`, whose `max_tokens`
    /// is `None`, and Anthropic's Messages API requires the field — so the
    /// encoder refused every request before it left the process and the
    /// caller got a 500. Nothing reached the provider, which is why the
    /// recorded request list is asserted as well as the reply.
    #[cfg(feature = "llm")]
    #[tokio::test]
    async fn a_chat_reaches_an_anthropic_provider_with_the_configured_max_tokens() {
        let (ctx, thread_id, fake) = anthropic_fixture().await;

        let body = crate::test_support::output_json(
            handle_chat(
                &stub_block(),
                &ctx,
                &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
                chat_body(&thread_id),
            )
            .await,
        )
        .await;

        assert_eq!(
            body["content"], FIXTURE_REPLY,
            "the provider's answer must come back to the caller"
        );
        let requests = fake.requests();
        assert_eq!(
            requests.len(),
            1,
            "exactly one request reached the provider"
        );
        assert_eq!(
            requests[0]["max_tokens"], FIXTURE_MAX_TOKENS,
            "the request must carry the configured budget, not a hardcoded one"
        );
    }

    /// The SSE path shares the prelude, so it must carry the budget too — and
    /// it fails differently: the client receives `event: error` after the
    /// status line is already committed, never a 500.
    #[cfg(feature = "llm")]
    #[tokio::test]
    async fn a_streamed_chat_reaches_an_anthropic_provider_with_the_configured_max_tokens() {
        let (ctx, thread_id, fake) = anthropic_fixture().await;

        let out = handle_chat_stream(
            &stub_block(),
            &ctx,
            &crate::test_support::auth_msg("create", "/b/llm/api/chat/stream", "user-a"),
            chat_body(&thread_id),
        )
        .await;
        let buf = out
            .collect_buffered()
            .await
            .expect("the SSE stream completes");
        let sse = String::from_utf8(buf.body).expect("SSE body is utf8");

        assert!(
            sse.contains(FIXTURE_REPLY),
            "the provider's answer must be forwarded as a frame, got: {sse}"
        );
        assert!(
            sse.ends_with("data: [DONE]\n\n"),
            "a refused request ends in `event: error`, got: {sse}"
        );
        let requests = fake.requests();
        assert_eq!(
            requests.len(),
            1,
            "exactly one request reached the provider"
        );
        assert_eq!(requests[0]["max_tokens"], FIXTURE_MAX_TOKENS);
    }

    /// A caller may ask for a budget of its own, and that is what is sent.
    #[cfg(feature = "llm")]
    #[tokio::test]
    async fn a_caller_supplied_max_tokens_overrides_the_configured_default() {
        let (ctx, thread_id, fake) = anthropic_fixture().await;
        let body = InputStream::from_bytes(
            serde_json::to_vec(&serde_json::json!({
                "thread_id": thread_id,
                "message": "hi",
                "max_tokens": 77,
            }))
            .expect("body"),
        );

        let out = handle_chat(
            &stub_block(),
            &ctx,
            &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
            body,
        )
        .await;

        assert_eq!(crate::test_support::output_http_status(out).await, 200);
        assert_eq!(fake.requests()[0]["max_tokens"], 77);
    }

    /// Zero tokens is a request for no answer at all — Anthropic answers 400
    /// to it — so it is refused here, before the user turn is stored and
    /// before the provider is paid for a round-trip.
    #[cfg(feature = "llm")]
    #[tokio::test]
    async fn a_zero_max_tokens_is_refused_without_reaching_the_provider() {
        let (ctx, thread_id, fake) = anthropic_fixture().await;
        let body = InputStream::from_bytes(
            serde_json::to_vec(&serde_json::json!({
                "thread_id": thread_id,
                "message": "hi",
                "max_tokens": 0,
            }))
            .expect("body"),
        );

        let out = handle_chat(
            &stub_block(),
            &ctx,
            &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
            body,
        )
        .await;

        assert_eq!(crate::test_support::output_http_status(out).await, 400);
        assert!(
            fake.requests().is_empty(),
            "nothing may reach the provider for a request that cannot be answered"
        );
        assert!(
            messages_list(
                &ctx,
                &crate::test_support::auth_msg("retrieve", "/b/llm/api/chat", "user-a"),
                &thread_id,
            )
            .await
            .expect("the history read succeeds")
            .is_empty(),
            "the refused turn must not be stored"
        );
    }

    /// The budget reaches an OpenAI-protocol provider as
    /// `max_completion_tokens`, and `max_tokens` is nowhere on the wire.
    ///
    /// This is the field the same fix would otherwise have broken: making the
    /// budget always present means an OpenAI *reasoning* model — selectable
    /// from `/v1/models` discovery like any other — starts refusing every
    /// chat with `unsupported_parameter` if the deprecated spelling goes out.
    /// The encoder tests pin the body; this pins that a real chat request
    /// travelling through the real provider service arrives that way.
    ///
    /// The fixture's provider declares no `max_tokens_field`, so this is also
    /// what "no override" means: the protocol decides.
    #[cfg(feature = "llm")]
    #[tokio::test]
    async fn a_chat_to_an_openai_provider_sends_max_completion_tokens() {
        use crate::blocks::llm::providers::fake_provider::FakeProvider;

        let fake = FakeProvider::openai(FIXTURE_REPLY).await;
        let (ctx, thread_id) = fixture_for(&fake).await;

        let body = crate::test_support::output_json(
            handle_chat(
                &stub_block(),
                &ctx,
                &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
                chat_body(&thread_id),
            )
            .await,
        )
        .await;

        assert_eq!(body["content"], FIXTURE_REPLY);
        let requests = fake.requests();
        assert_eq!(
            requests.len(),
            1,
            "exactly one request reached the provider"
        );
        assert_eq!(requests[0]["max_completion_tokens"], FIXTURE_MAX_TOKENS);
        assert!(
            requests[0].get("max_tokens").is_none(),
            "OpenAI's reasoning models refuse `max_tokens`, got: {}",
            requests[0]
        );
    }

    /// The inverse for an OpenAI-*compatible* server: `max_tokens` is the
    /// spelling Ollama, vLLM and the hosted gateways know, and a budget that
    /// landed in a field they ignore would be an uncapped reply with no sign
    /// anything was wrong. No `max_tokens_field` is declared here either.
    #[cfg(feature = "llm")]
    #[tokio::test]
    async fn a_chat_to_an_openai_compatible_provider_sends_max_tokens() {
        use crate::blocks::llm::providers::fake_provider::FakeProvider;

        let fake = FakeProvider::openai_compatible(FIXTURE_REPLY).await;
        let (ctx, thread_id) = fixture_for(&fake).await;

        let body = crate::test_support::output_json(
            handle_chat(
                &stub_block(),
                &ctx,
                &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
                chat_body(&thread_id),
            )
            .await,
        )
        .await;

        assert_eq!(body["content"], FIXTURE_REPLY);
        let requests = fake.requests();
        assert_eq!(
            requests.len(),
            1,
            "exactly one request reached the provider"
        );
        assert_eq!(requests[0]["max_tokens"], FIXTURE_MAX_TOKENS);
        assert!(
            requests[0].get("max_completion_tokens").is_none(),
            "a compatible server gets the only spelling it knows, got: {}",
            requests[0]
        );
    }

    /// The protocol is the default, not the whole answer: a provider that
    /// declares `max_tokens_field` sends that spelling instead.
    ///
    /// Azure OpenAI is configured on `open_ai_compatible` — it is not OpenAI's
    /// own `/v1` surface — and its *reasoning* deployments accept only
    /// `max_completion_tokens`. Without a per-provider override that operator
    /// had no way to reach one: the protocol's spelling is the one thing the
    /// admin form did not let them change, and every chat turn came back 400.
    #[cfg(feature = "llm")]
    #[tokio::test]
    async fn a_compatible_provider_declaring_max_completion_tokens_sends_that() {
        use crate::{
            blocks::llm::providers::fake_provider::FakeProvider, llm_wire::openai::MaxTokensField,
        };

        let fake = FakeProvider::openai_compatible(FIXTURE_REPLY)
            .await
            .requiring_max_tokens_field(MaxTokensField::MaxCompletionTokens);
        let (ctx, thread_id) = fixture_for(&fake).await;

        let body = crate::test_support::output_json(
            handle_chat(
                &stub_block(),
                &ctx,
                &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
                chat_body(&thread_id),
            )
            .await,
        )
        .await;

        assert_eq!(body["content"], FIXTURE_REPLY);
        let requests = fake.requests();
        assert_eq!(
            requests.len(),
            1,
            "exactly one request reached the provider"
        );
        assert_eq!(
            requests[0]["max_completion_tokens"], FIXTURE_MAX_TOKENS,
            "the declared field must carry the budget, got: {}",
            requests[0]
        );
        assert!(
            requests[0].get("max_tokens").is_none(),
            "never both spellings — the override replaces, it does not add: {}",
            requests[0]
        );
    }

    /// And the override runs the other way too, so it is a property of the
    /// provider rather than a second name for "reasoning model": a provider on
    /// OpenAI's *own* protocol that declares `max_tokens` sends `max_tokens`.
    #[cfg(feature = "llm")]
    #[tokio::test]
    async fn an_openai_provider_declaring_max_tokens_sends_that() {
        use crate::{
            blocks::llm::providers::fake_provider::FakeProvider, llm_wire::openai::MaxTokensField,
        };

        let fake = FakeProvider::openai(FIXTURE_REPLY)
            .await
            .requiring_max_tokens_field(MaxTokensField::MaxTokens);
        let (ctx, thread_id) = fixture_for(&fake).await;

        let body = crate::test_support::output_json(
            handle_chat(
                &stub_block(),
                &ctx,
                &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
                chat_body(&thread_id),
            )
            .await,
        )
        .await;

        assert_eq!(body["content"], FIXTURE_REPLY);
        let requests = fake.requests();
        assert_eq!(requests[0]["max_tokens"], FIXTURE_MAX_TOKENS);
        assert!(
            requests[0].get("max_completion_tokens").is_none(),
            "the override replaces the protocol's spelling, got: {}",
            requests[0]
        );
    }

    /// Once the reply passes the buffering cap, nothing after it is kept.
    ///
    /// The check was per delta, so a delta that overflowed was skipped and the
    /// *next, smaller* one was appended again — the stored and returned
    /// `content` then joined the head of the answer to a later fragment with
    /// the middle missing, and read as a complete reply. What is published
    /// must be a prefix of what the model said.
    #[tokio::test]
    async fn text_after_the_cap_is_never_spliced_back_on() {
        // Fits; then a delta that overflows; then one small enough to fit in
        // the room the overflowing delta did not use.
        let head = "a".repeat(MAX_BUFFERED_RESPONSE_BYTES - 10);
        let (ctx, thread_id, _chat_calls) = chat_fixture_answering(vec![
            ChatChunk::text(head.clone()),
            ChatChunk::text("B".repeat(100)),
            ChatChunk::text("tail"),
        ])
        .await;

        let body = crate::test_support::output_json(
            handle_chat(
                &stub_block(),
                &ctx,
                &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
                chat_body(&thread_id),
            )
            .await,
        )
        .await;

        let content = body["content"].as_str().expect("content is a string");
        assert_eq!(
            content, head,
            "content must stop at the last delta that fitted"
        );
        assert!(
            !content.contains("tail"),
            "a delta after the cap must not be spliced onto the prefix"
        );
        assert_eq!(body["truncated"], true, "and the reply says it is partial");
    }

    /// A reply the model cut at the budget says so.
    ///
    /// The budget this PR introduces is a ceiling every chat now carries, so
    /// hitting it is an ordinary outcome — and `handle_chat` read only
    /// `chunk.delta`, dropping the `finish_reason` beside it. The answer came
    /// back `truncated: false`: a fragment ending mid-sentence, published as
    /// a complete reply, with nothing anywhere to say otherwise. Both
    /// decoders already produce `FinishReason::Length` from the provider's
    /// own terminal field.
    #[tokio::test]
    async fn a_reply_stopped_by_the_token_budget_is_reported_as_truncated() {
        use wafer_core::clients::llm::FinishReason;

        let (ctx, thread_id, _chat_calls) = chat_fixture_answering(vec![
            ChatChunk::text("The three causes are, first"),
            ChatChunk::finish(FinishReason::Length, None),
        ])
        .await;

        let body = crate::test_support::output_json(
            handle_chat(
                &stub_block(),
                &ctx,
                &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
                chat_body(&thread_id),
            )
            .await,
        )
        .await;

        assert_eq!(body["content"], "The three causes are, first");
        assert_eq!(
            body["truncated"], true,
            "a reply cut at the budget is not a complete answer"
        );
    }

    /// The flag stays off for a reply the model finished on its own, so
    /// `truncated` keeps meaning something.
    #[tokio::test]
    async fn a_reply_the_model_finished_is_not_reported_as_truncated() {
        let (ctx, thread_id, _chat_calls) = chat_fixture().await;

        let body = crate::test_support::output_json(
            handle_chat(
                &stub_block(),
                &ctx,
                &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
                chat_body(&thread_id),
            )
            .await,
        )
        .await;

        assert_eq!(body["truncated"], false);
    }

    /// A user turn the store refused must not be followed by a model call.
    ///
    /// The write was `let _ = messages_create(..)`, so an unreachable
    /// messages block cost the operator a paid completion and handed the user
    /// an answer to a turn that was never recorded — and the *next* request,
    /// which rebuilds the history from that store, could not see the question
    /// the answer belonged to. Asserting the handler's status would not have
    /// caught it: only the provider's own call count can say the model was
    /// never reached.
    #[tokio::test]
    async fn a_user_turn_that_could_not_be_stored_never_reaches_the_model() {
        use crate::blocks::llm::routes::test_support::MessagesWriteFails;

        let (ctx, thread_id, chat_calls) = chat_fixture().await;
        let ctx = MessagesWriteFails::after(ctx.clone_arc(), 0);

        let out = handle_chat(
            &stub_block(),
            &ctx,
            &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
            chat_body(&thread_id),
        )
        .await;

        assert_eq!(
            crate::test_support::output_http_status(out).await,
            500,
            "a turn that was not stored must refuse, not answer"
        );
        assert_eq!(
            chat_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the model must not be called for a turn the store refused"
        );
    }

    /// A chat request naming a thread that does not exist is that caller's
    /// 404, not a 500: the messages block already answers `NotFound` for it,
    /// and the write is the first thing in the prelude that asks.
    #[tokio::test]
    async fn a_chat_request_for_an_unknown_thread_is_a_404() {
        let (ctx, _thread_id, chat_calls) = chat_fixture().await;

        let out = handle_chat(
            &stub_block(),
            &ctx,
            &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
            chat_body("no-such-thread"),
        )
        .await;

        assert_eq!(crate::test_support::output_http_status(out).await, 404);
        assert_eq!(
            chat_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no thread, no model call"
        );
    }

    /// Another user's thread is a 404 on both chat endpoints, reached the way
    /// a browser reaches them: through the router, as the signed-in caller.
    ///
    /// The owner check is not in this block — the prelude's first read of
    /// the thread is a write through `impresspress/messages`, which refuses a
    /// thread the caller does not own. So what is asserted is the whole
    /// chain: the refusal becomes the caller's 404, the model is never
    /// called, and the owner's thread gains no turn.
    #[tokio::test]
    async fn a_chat_on_another_users_thread_is_a_404_and_never_reaches_the_model() {
        use std::sync::Arc;

        use crate::blocks::messages::service::{self, ListEntriesParams};

        let (mut ctx, thread_id, chat_calls) = chat_fixture().await;
        ctx.register_block("impresspress/llm", Arc::new(stub_block()));

        let paths = ["/b/llm/api/chat", "/b/llm/api/chat/stream"];
        let mut refused = Vec::new();
        for path in paths {
            let out = ctx
                .dispatch_resolved_with_input(
                    crate::test_support::auth_msg("create", path, "user-b"),
                    chat_body(&thread_id),
                )
                .await;
            let status = crate::test_support::output_http_status(out).await;
            if status != 404 {
                refused.push(format!("{path}: {status}"));
            }
        }
        assert!(
            refused.is_empty(),
            "user-b must not be able to post into user-a's thread, expected 404 on: {refused:?}"
        );

        assert_eq!(
            chat_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a refused thread must not be paid for"
        );
        let entries = service::list_entries(
            &ctx.fixture(),
            &thread_id,
            &ListEntriesParams {
                kind: None,
                role: None,
                page_size: 100,
                offset: 0,
            },
        )
        .await
        .expect("read the owner's thread");
        assert!(
            entries.records.is_empty(),
            "user-a's thread gained turns it never wrote: {:?}",
            entries.records
        );

        // The same dispatch as the owner succeeds on both paths, so the 404s
        // above are the owner check and not a route that failed to match.
        let mut failed = Vec::new();
        for path in paths {
            let out = ctx
                .dispatch_resolved_with_input(
                    crate::test_support::auth_msg("create", path, "user-a"),
                    chat_body(&thread_id),
                )
                .await;
            let status = crate::test_support::output_http_status(out).await;
            if status != 200 {
                failed.push(format!("{path}: {status}"));
            }
        }
        assert!(
            failed.is_empty(),
            "the thread's owner must be able to chat on both paths, got: {failed:?}"
        );
    }

    /// The assistant turn the store refused is not a 200.
    ///
    /// `handle_chat` used to publish `message_id: ""` and a `content` the
    /// store had just declined to keep, so the client rendered an answer that
    /// vanished on its next history refetch. The model has already been paid
    /// for by this point, but the buffered path has not written a status line
    /// yet, so it still owns the one channel that can say so.
    #[tokio::test]
    async fn an_assistant_turn_that_could_not_be_stored_is_not_a_200() {
        use crate::blocks::llm::routes::test_support::MessagesWriteFails;

        let (ctx, thread_id, chat_calls) = chat_fixture().await;
        // Let the user turn land; fail the assistant turn that follows it.
        let ctx = MessagesWriteFails::after(ctx.clone_arc(), 1);

        let out = handle_chat(
            &stub_block(),
            &ctx,
            &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
            chat_body(&thread_id),
        )
        .await;

        assert_eq!(crate::test_support::output_http_status(out).await, 500);
        assert_eq!(
            chat_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the model was reached — this is the write after it that failed"
        );
    }

    /// Apply the messages block's migrations into `ctx` and register the
    /// block, so `ctx.call_block("impresspress/messages", ..)` reaches the
    /// real handlers — the shape `info().requires` declares.
    async fn register_messages_block(ctx: &mut crate::test_support::TestContext) {
        use std::sync::Arc;

        let sqlite: Vec<&str> = crate::blocks::messages::migrations::SQLITE_MIGRATIONS
            .iter()
            .map(|(_, sql)| *sql)
            .collect();
        crate::migration_helper::apply_migrations(
            ctx,
            "impresspress/messages",
            &sqlite,
            crate::blocks::messages::migrations::POSTGRES_MIGRATIONS,
        )
        .await
        .expect("apply messages migrations in the chat fixture");
        ctx.register_block(
            "impresspress/messages",
            Arc::new(crate::blocks::messages::MessagesBlock::new()),
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

    /// B20. The messages block documents `agent` as a role, its composer is
    /// the only control in the tree that offers one, and every entry stored
    /// with it was replayed to the model as if the *user* had written it —
    /// so an agent's own turn came back as the user's next instruction.
    ///
    /// The four roles a stored entry can hold map onto three `ChatRole`s,
    /// and `agent` is an alias of `assistant`, not of `user`.
    #[test]
    fn an_agent_turn_is_replayed_as_an_assistant_turn() {
        let history: Vec<serde_json::Value> = ["user", "assistant", "agent", "system"]
            .iter()
            .map(|role| serde_json::json!({ "role": role, "content": "t" }))
            .collect();

        let roles: Vec<ChatRole> = history_to_messages(&history)
            .iter()
            .map(|m| m.role)
            .collect();

        assert_eq!(
            roles,
            vec![
                ChatRole::User,
                ChatRole::Assistant,
                ChatRole::Assistant,
                ChatRole::System,
            ],
            "an entry stored with the messages block's `agent` role must not \
             be replayed to the model as a user turn"
        );
    }

    /// B20, end to end and through the real wire.
    ///
    /// A human posts `role=agent` — the value the messages composer offered
    /// — into a thread, and the next chat request rebuilds its history from
    /// that column. Before `EntryRole`, `role_from_str` sent every value it
    /// did not recognise to `ChatRole::User`, so the agent's own turn came
    /// back to the model as the user's next instruction. The unit test above
    /// pins the mapping; this pins that the two blocks agree on the value
    /// travelling between them.
    #[tokio::test]
    async fn an_entry_posted_as_agent_reaches_the_model_as_an_assistant_turn() {
        use crate::blocks::messages::{service, test_support::ctx_with_messages};

        // The thread is staged by the messages block; the post and the
        // history read are the llm block's calls into it.
        let ctx = ctx_with_messages()
            .await
            .running_as(crate::blocks::llm::LlmBlock::BLOCK_NAME);
        let thread = service::create_context(
            &ctx.clone()
                .running_as(crate::blocks::messages::MessagesBlock::BLOCK_NAME),
            "user-a",
            "conversation",
            "T",
            "",
            "",
            None,
            None,
        )
        .await
        .expect("create the thread");

        // Posted the way the composer posts it: through the messages block's
        // own HTTP surface, not through a repo call that could not see the
        // request parsing.
        let mut post = crate::util::block_request(
            "create",
            "POST",
            &format!("/b/messages/api/contexts/{}/entries", thread.id),
            &crate::test_support::auth_msg("create", "/b/llm/api/chat", "user-a"),
        );
        post.set_meta("req.content_type", "application/json");
        let body = serde_json::to_vec(&serde_json::json!({
            "kind": "message",
            "role": "agent",
            "content": "I did the thing",
        }))
        .expect("body");
        let stored = ctx
            .call_block("impresspress/messages", post, InputStream::from_bytes(body))
            .await;
        assert_eq!(
            crate::test_support::output_json(stored).await["data"]["role"],
            "assistant"
        );

        let history = messages_list(
            &ctx,
            &crate::test_support::auth_msg("retrieve", "/b/llm/api/chat", "user-a"),
            &thread.id,
        )
        .await
        .expect("the history read succeeds");
        assert_eq!(
            history_to_messages(&history)
                .iter()
                .map(|m| m.role)
                .collect::<Vec<_>>(),
            vec![ChatRole::Assistant],
            "an entry the agent posted must not reach the model as a user turn"
        );
    }

    #[test]
    fn every_entry_role_has_its_own_chat_role() {
        assert_eq!(chat_role(EntryRole::User), ChatRole::User);
        assert_eq!(chat_role(EntryRole::Assistant), ChatRole::Assistant);
        assert_eq!(chat_role(EntryRole::System), ChatRole::System);
    }

    /// The replacement for `role_from_str_unknown_falls_back_to_user`, which
    /// asserted that `"tool"`, `""` and `"random"` were all replayed as the
    /// *user*. That fallback is the bug: an entry whose role cannot be read
    /// is left out of the history rather than attributed to the person who
    /// did not write it.
    #[test]
    fn an_unreadable_role_is_left_out_of_the_history() {
        for role in ["tool", "", "random"] {
            let history = vec![serde_json::json!({ "role": role, "content": "t" })];
            assert!(
                history_to_messages(&history).is_empty(),
                "role {role:?} must not be replayed as a user turn"
            );
        }
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
