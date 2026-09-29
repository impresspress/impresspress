//! Document chunking and optional contextual retrieval.
//!
//! This module provides the building blocks used by the `POST
//! /b/vector/api/ingest` route to split a document into embedding-sized
//! chunks and (optionally) prepend a short LLM-generated context summary
//! to each chunk before embedding.
//!
//! The chunker is intentionally simple: we whitespace-split as a token
//! proxy. `vector/pages.rs::handle_ingest` ratio-adjusts the threshold by
//! the embedder's BPE token count when available, so the whitespace
//! approximation only sets the chunk shape, not its real BPE budget.
//!
//! `add_context` runs one LLM call per ingest (not per chunk — that would
//! be N round-trips per document) to produce a document-level context
//! summary, then prepends that summary to every chunk. This is a
//! simplification of Anthropic's per-chunk contextual retrieval recipe
//! and trades some precision for one wire call instead of N.
//!
//! ### Where the degradation logs land
//!
//! All four of `add_context`'s degradation paths log through `tracing`, which
//! reaches a subscriber on the two targets that run this code today: the
//! native server installs one in `impresspress-native::log_init` (from
//! `impresspress_server::build_native_runtime`), and the browser installs one in
//! `impresspress-browser::logger::init_console_tracing` (from
//! `impresspress-web::initialize`). The Cloudflare Worker installs **no**
//! `tracing` subscriber — it has a `LoggerService` (`ConsoleLoggerService`),
//! which is a different channel — so a `tracing` event is dropped there.
//! `block-vector` is not in that target's default feature set, so nothing
//! runs this path on a Worker today; if that changes, the subscriber is what
//! has to be installed first, or these logs are decoration.

use wafer_block::wire::llm::{
    ChatContent, ChatMessage, ChatParams, ChatRequest, ChatRole, ChunkDelta,
};
use wafer_core::clients::llm;
use wafer_run::{context::Context, InputStream, Message, WaferError};

use crate::llm_target::{DefaultTarget, ResolvedTarget, TargetGap, DEFAULT_MAX_TOKENS_VAR};

/// Approximate max tokens per chunk. We use whitespace-split as a proxy
/// for tokenization — close enough for bge-m3 / MiniLM at this
/// granularity, and avoids pulling a tokenizer crate into the ingest path.
pub const DEFAULT_CHUNK_TOKENS: usize = 512;

/// Fraction of overlap between adjacent chunks.
///
/// 10% is a safe default: enough to keep entity mentions and sentence
/// boundaries intact across splits without blowing up the number of
/// chunks an average document produces.
pub const DEFAULT_OVERLAP_RATIO: f32 = 0.10;

/// Split `text` into overlapping chunks of approximately `chunk_tokens`
/// tokens, with adjacent chunks sharing `chunk_tokens * overlap_ratio`
/// tokens on each boundary.
///
/// Tokens are approximated by whitespace-splitting — see the module doc.
/// Empty input returns an empty vec; input shorter than `chunk_tokens`
/// returns a single chunk with the original whitespace collapsed.
pub fn chunk(text: &str, chunk_tokens: usize, overlap_ratio: f32) -> Vec<String> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    if tokens.len() <= chunk_tokens {
        return vec![tokens.join(" ")];
    }

    // Guard against overlap_ratio >= 1.0 — that would give us a
    // non-advancing stride and an infinite loop. `stride = 1` is the
    // degenerate-but-terminating choice when overlap ties or exceeds the
    // chunk size.
    let overlap = ((chunk_tokens as f32) * overlap_ratio).round() as usize;
    let stride = chunk_tokens.saturating_sub(overlap).max(1);

    let mut out = Vec::new();
    let mut start = 0usize;
    while start < tokens.len() {
        let end = (start + chunk_tokens).min(tokens.len());
        out.push(tokens[start..end].join(" "));
        if end == tokens.len() {
            break;
        }
        start += stride;
    }
    out
}

/// Prepend a short LLM-generated context summary to each chunk.
///
/// Cost model: one LLM call per ingest (not per chunk). The call sees the
/// full `document` and is asked for a 1–2 sentence summary; that single
/// summary is prepended to every chunk. This is cheaper than the
/// per-chunk Anthropic Contextual Retrieval recipe (which makes N LLM
/// calls per document) at the cost of less per-chunk specificity.
///
/// Degrades — `chunks` is returned unchanged, and the reason is logged —
/// when:
///   * no LLM is configured (`IMPRESSPRESS__LLM__DEFAULT_MODEL` empty, or the
///     llm block is not registered at all),
///   * the llm block published a target carrying no usable output-token
///     budget (see [`default_llm_target`], which logs the three cases apart),
///   * the chat call errors (transport failure, backend refusal, …),
///   * the LLM returns no text (empty stream).
///
/// The ingest must not fail because the contextual step couldn't run; the
/// raw chunks are still useful for retrieval.
///
/// The summary is asked for under the deployment's whole chat budget
/// (`IMPRESSPRESS__LLM__DEFAULT_MAX_TOKENS`), not a smaller one of its own: a
/// budget is a ceiling, the prompt asks for one or two sentences, and a
/// second variable for the same quantity is a knob whose only job is to be
/// out of step with the first. What the model bills for is the tokens it
/// actually emits.
///
/// Not gated on the `llm` cargo feature. It used to be, with a no-op twin
/// under `cfg(not(feature = "llm"))`, so a build without that feature — every
/// wasm32 build, where `block-vector` is enabled and `llm` cannot be —
/// silently linked a function that returned `chunks` untouched while the
/// ingest reported success. Nothing in this body needs the feature: it
/// reaches the llm block through `wafer_core::clients::llm::chat` and
/// `ctx.call_block`, both unconditional. The feature gates the *native
/// provider backend* (`ProviderLlmService`, reqwest + tokio), which is a
/// different question from whether an ingest can ask a registered llm block
/// for a summary.
pub async fn add_context(
    ctx: &dyn Context,
    document: &str,
    chunks: Vec<String>,
) -> Result<Vec<String>, WaferError> {
    if chunks.is_empty() {
        return Ok(chunks);
    }
    // `default_llm_target` has already logged which of its three cases this
    // is; a second line here could only repeat one of them, and the one it
    // used to repeat was wrong for two.
    let Some(target) = default_llm_target(ctx).await else {
        return Ok(chunks);
    };

    let request = ChatRequest {
        backend_id: target.provider,
        model: target.model,
        messages: vec![
            ChatMessage {
                role: ChatRole::System,
                content: ChatContent::Text(CONTEXTUAL_SYSTEM_PROMPT.into()),
                tool_call_id: None,
                tool_calls: Vec::new(),
            },
            ChatMessage {
                role: ChatRole::User,
                content: ChatContent::Text(document.into()),
                tool_call_id: None,
                tool_calls: Vec::new(),
            },
        ],
        // The summary call needs an output-token budget like any other:
        // Anthropic-protocol providers refuse a request without one, and the
        // llm block publishes the deployment's alongside the target.
        params: ChatParams {
            max_tokens: Some(target.max_tokens),
            ..Default::default()
        },
        tools: Vec::new(),
        extra: serde_json::Value::Null,
    };

    let response_chunks = match llm::chat(ctx, &request).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "contextual retrieval LLM call failed; skipping");
            return Ok(chunks);
        }
    };

    let mut context = String::new();
    for chunk in response_chunks {
        if let ChunkDelta::Text(t) = chunk.delta {
            context.push_str(&t);
        }
    }
    let context = context.trim();
    if context.is_empty() {
        // The fourth degradation path, and the last one that was silent: the
        // model answered with no text at all. Same log level as the
        // no-default-model path — expected enough not to be a warning, but an
        // ingest that quietly stopped being contextual is not something an
        // operator should have to infer from the retrieval quality.
        tracing::debug!("contextual retrieval skipped: the model returned no text");
        return Ok(chunks);
    }

    Ok(chunks
        .into_iter()
        .map(|c| format!("{context}\n\n{c}"))
        .collect())
}

/// Fetch the default LLM target via the llm block's internal discovery route.
///
/// Returns `None` in three cases, each logged as the thing it actually is:
/// the llm block is not registered or refused, no provider/model is
/// configured, or the block answered with a target carrying no usable
/// output-token budget. All three degrade the same way in [`add_context`] —
/// raw chunks, no failure — so the log line is the only place the difference
/// survives, and "no default LLM model configured" was being written for all
/// of them.
///
/// Going through `ctx.call_block(...)` rather than a direct in-process
/// function call is what keeps the vector block independent of the llm block
/// at the type/dep level — and it is why this whole path needs no cargo
/// feature: the edge is a runtime dispatch, resolved against what is
/// registered. [`DefaultTarget`] is the shape of the answer, shared with the
/// publisher because it depends on neither block. The budget travels inside
/// it because it is the llm block's own configuration variable — read there,
/// under that block's identity.
async fn default_llm_target(ctx: &dyn Context) -> Option<ResolvedTarget> {
    let resource = DefaultTarget::RESOURCE;
    let mut msg = Message::new(format!("retrieve:{resource}"));
    msg.set_meta("req.action", "retrieve");
    msg.set_meta("req.resource", resource);
    msg.set_meta("http.method", "GET");
    msg.set_meta("http.path", resource);

    let out = ctx
        .call_block("impresspress/llm", msg, InputStream::empty())
        .await;
    let buf = match out.collect_buffered().await {
        Ok(buf) => buf,
        Err(e) => {
            tracing::debug!(
                error = ?e,
                "contextual retrieval skipped: the llm block is not registered or refused"
            );
            return None;
        }
    };
    let target: DefaultTarget = match serde_json::from_slice(&buf.body) {
        Ok(target) => target,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "contextual retrieval skipped: the llm block's default-target body did not decode"
            );
            return None;
        }
    };
    match target.resolve() {
        Ok(resolved) => Some(resolved),
        Err(TargetGap::NotConfigured) => {
            tracing::debug!("contextual retrieval skipped: no default LLM model configured");
            None
        }
        Err(TargetGap::MissingBudget) => {
            // Not an operator's misconfiguration: the route always publishes a
            // budget. Reported as what it is — the two sides of this contract
            // disagreeing — rather than as a variable nobody has to set.
            tracing::warn!(
                var = DEFAULT_MAX_TOKENS_VAR,
                "contextual retrieval skipped: the llm block published a target with no usable \
                 max-token budget"
            );
            None
        }
    }
}

const CONTEXTUAL_SYSTEM_PROMPT: &str = "\
You are summarizing a document so retrieval excerpts from it are easier to \
understand out of context. Return one or two short sentences describing what \
the document is about and who or what it concerns. Do not preface with \
\"This document\" or \"Summary:\" — write the description plainly. No \
markdown, no bullet points, no quotes around the answer.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_empty_returns_empty() {
        assert!(chunk("", 10, 0.1).is_empty());
    }

    #[test]
    fn chunk_small_text_single_chunk() {
        let c = chunk("hello world", 10, 0.1);
        assert_eq!(c, vec!["hello world"]);
    }

    #[test]
    fn chunk_long_text_overlaps() {
        let text = (1..=20)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        // chunk_tokens=8, overlap_ratio=0.25 → overlap=2, stride=6
        let c = chunk(&text, 8, 0.25);
        assert!(c.len() >= 3);
        // First chunk: w1..w8.
        assert_eq!(c[0], "w1 w2 w3 w4 w5 w6 w7 w8");
        // Second chunk starts at w7 (overlap of 2 from the first chunk).
        assert!(c[1].starts_with("w7 w8 w9"));
    }

    #[test]
    fn chunk_no_overlap_when_ratio_zero() {
        let text = (1..=20)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        // chunk_tokens=5, overlap_ratio=0 → stride=5, 20 tokens / 5 = 4 chunks.
        let c = chunk(&text, 5, 0.0);
        assert_eq!(c.len(), 4);
        assert!(c[1].starts_with("w6"));
    }
}

// ---------------------------------------------------------------------------
// Tests: contextual retrieval is a runtime capability, not a build-time one.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod contextual_retrieval_tests {
    use std::sync::Arc;

    use wafer_block::{
        common::ServiceOp,
        wire::llm::{ChatChunk, FinishReason},
    };
    use wafer_run::{
        Block, BlockCategory, BlockInfo, ErrorCode, InputStream, LifecycleEvent, Message,
        OutputStream, WaferError,
    };

    use super::*;
    use crate::test_support::TestContext;

    /// A fixture that runs `add_context` under the vector block's real
    /// identity: its own declared `requires` allowlist, which
    /// `Wafer::make_block_context` installs on every context this block's
    /// code runs in and `RuntimeContext::dispatch_call` enforces above every
    /// other permission check.
    ///
    /// Sourced from `VectorBlock::new().info()`, never re-listed here — the
    /// whole point is that the test and the runtime read the same
    /// declaration. A test that called `add_context` on a bare
    /// `TestContext` would pass while production answered
    /// `PermissionDenied`, which is exactly how this path shipped broken:
    /// `add_context` reaches two blocks (`impresspress/llm` for the default
    /// target, `wafer-run/llm` for the completion) and the allowlist named
    /// neither, so the permission denial was swallowed at the `.ok()?` and
    /// logged as "no default LLM model configured".
    async fn vector_ctx() -> TestContext {
        TestContext::with_vector()
            .await
            .running_as("impresspress/vector")
    }

    /// Output-token budget the stub target publishes. Any positive number:
    /// which one reaches the provider is
    /// `a_contextual_ingest_reaches_an_anthropic_provider`'s question, and it
    /// asks the real block.
    const STUB_MAX_TOKENS: u32 = 4096;

    /// Stub `impresspress/llm` feature block. `default_llm_target` reads one
    /// internal route off it and nothing else; anything else errors loudly so
    /// a test cannot silently exercise an unscripted path.
    ///
    /// It answers with [`DefaultTarget`] — the same type the real block
    /// serializes — rather than a hand-written JSON literal. A test double
    /// that describes the body in its own words is free to describe it
    /// differently, and this one did: the field the route grew was missing
    /// here, and every test in this module would have gone on passing while
    /// reporting a cause that was not true.
    struct StubDefaultTargetBlock {
        /// `None` publishes [`DefaultTarget::unconfigured`], which is how a
        /// runtime with the llm block registered but no model configured
        /// answers.
        target: Option<(&'static str, &'static str)>,
    }

    #[async_trait::async_trait]
    impl Block for StubDefaultTargetBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new(
                "impresspress/llm",
                "0.0.1",
                "http@v1",
                "stub llm feature block for contextual-retrieval tests",
            )
        }

        async fn handle(
            &self,
            _ctx: &dyn Context,
            msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            assert_eq!(msg.path(), DefaultTarget::RESOURCE);
            let body = match self.target {
                Some((provider, model)) => {
                    DefaultTarget::configured(provider, model, STUB_MAX_TOKENS)
                }
                None => DefaultTarget::unconfigured(),
            };
            OutputStream::respond(serde_json::to_vec(&body).expect("serialize default-target body"))
        }

        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _e: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// Stub `wafer-run/llm` service block streaming one scripted summary.
    struct StubChatBlock {
        summary: &'static str,
    }

    #[async_trait::async_trait]
    impl Block for StubChatBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new(
                "wafer-run/llm",
                "0.0.1",
                "llm@v1",
                "stub llm service block for contextual-retrieval tests",
            )
            .category(BlockCategory::Service)
        }

        async fn handle(
            &self,
            _ctx: &dyn Context,
            msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            match msg.kind.as_str() {
                ServiceOp::LLM_CHAT => {
                    let frames: Vec<Vec<u8>> = vec![
                        wafer_block::codec::encode(&ChatChunk::text(self.summary))
                            .expect("encode text chunk"),
                        wafer_block::codec::encode(&ChatChunk::finish(FinishReason::Stop, None))
                            .expect("encode finish chunk"),
                    ];
                    OutputStream::from_producer(move |sink, _cancel| async move {
                        for frame in frames {
                            if sink.send_chunk(frame).await.is_err() {
                                return;
                            }
                        }
                        let _ = sink.complete(Vec::new()).await;
                    })
                }
                other => OutputStream::error(WaferError::new(
                    ErrorCode::Unimplemented,
                    format!("StubChatBlock: unhandled op {other}"),
                )),
            }
        }

        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _e: LifecycleEvent,
        ) -> Result<(), WaferError> {
            Ok(())
        }
    }

    /// The point of the whole module: with an llm block and a default model
    /// configured, every chunk carries the document-level summary.
    ///
    /// This is what a build without the `llm` cargo feature could not do,
    /// because `add_context` was `cfg`-gated and such a build linked a no-op
    /// twin that returned `chunks` unchanged. Nothing in the real body needs
    /// that feature — it reaches the llm block through
    /// `wafer_core::clients::llm::chat` and `ctx.call_block`, both
    /// unconditional — so the gate's only effect was silently degrading
    /// wasm32 ingests.
    #[tokio::test]
    async fn add_context_prepends_the_summary_to_every_chunk() {
        let mut ctx = vector_ctx().await;
        ctx.register_block(
            "impresspress/llm",
            Arc::new(StubDefaultTargetBlock {
                target: Some(("openai", "gpt-4o-mini")),
            }),
        );
        ctx.register_block(
            "wafer-run/llm",
            Arc::new(StubChatBlock {
                summary: "A quarterly report about widget sales.",
            }),
        );

        let out = add_context(&ctx, "the document", vec!["one".into(), "two".into()])
            .await
            .expect("add_context never fails the ingest");

        assert_eq!(
            out,
            vec![
                "A quarterly report about widget sales.\n\none".to_string(),
                "A quarterly report about widget sales.\n\ntwo".to_string(),
            ]
        );
    }

    /// The three degradation paths stay degradations: the ingest keeps its
    /// chunks and never fails.
    #[tokio::test]
    async fn add_context_degrades_when_no_default_model_is_configured() {
        let mut ctx = vector_ctx().await;
        ctx.register_block(
            "impresspress/llm",
            Arc::new(StubDefaultTargetBlock { target: None }),
        );

        let out = add_context(&ctx, "the document", vec!["one".into()])
            .await
            .expect("add_context never fails the ingest");

        assert_eq!(out, vec!["one".to_string()]);
    }

    #[tokio::test]
    async fn add_context_degrades_when_the_llm_block_is_absent() {
        let ctx = vector_ctx().await;

        let out = add_context(&ctx, "the document", vec!["one".into()])
            .await
            .expect("add_context never fails the ingest");

        assert_eq!(out, vec!["one".to_string()]);
    }

    #[tokio::test]
    async fn add_context_degrades_when_the_model_returns_no_text() {
        let mut ctx = vector_ctx().await;
        ctx.register_block(
            "impresspress/llm",
            Arc::new(StubDefaultTargetBlock {
                target: Some(("openai", "gpt-4o-mini")),
            }),
        );
        ctx.register_block("wafer-run/llm", Arc::new(StubChatBlock { summary: "   " }));

        let out = add_context(&ctx, "the document", vec!["one".into()])
            .await
            .expect("add_context never fails the ingest");

        assert_eq!(out, vec!["one".to_string()]);
    }

    /// The summary call reaches a real Anthropic-protocol provider.
    ///
    /// Every test above stubs both hops, so none of them encodes a provider
    /// request — and `add_context` sent `ChatParams::default()`, whose
    /// `max_tokens` is `None`, which Anthropic's Messages API requires. The
    /// encoder refused it, `llm::chat` returned an error, and the ingest took
    /// its "LLM call failed" degradation: raw chunks, a `warn!` nobody reads,
    /// and a green suite. So this drives the real `impresspress/llm` block
    /// (which is what publishes the budget alongside the target) and the real
    /// `ProviderLlmService` behind `wafer-run/llm`, each in its own frame:
    /// the real llm block reads its own `IMPRESSPRESS__LLM__*` variables as
    /// itself, not as the vector block that called it.
    #[cfg(feature = "llm")]
    #[tokio::test]
    async fn a_contextual_ingest_reaches_an_anthropic_provider() {
        use crate::blocks::llm::{
            provider_admin::NoopProviderAdmin, providers::fake_provider::FakeProvider,
            DEFAULT_MODEL_VAR, DEFAULT_PROVIDER_VAR,
        };

        let fake = FakeProvider::anthropic("A report about widget sales.").await;
        let mut ctx = vector_ctx().await;
        ctx.set_config(DEFAULT_PROVIDER_VAR, fake.backend_id());
        ctx.set_config(DEFAULT_MODEL_VAR, fake.model());
        ctx.set_config(DEFAULT_MAX_TOKENS_VAR, "321");
        ctx.register_block(
            "impresspress/llm",
            Arc::new(crate::blocks::llm::LlmBlock::new(Arc::new(
                NoopProviderAdmin,
            ))),
        );
        ctx.register_block("wafer-run/llm", fake.llm_service_block());

        let out = add_context(&ctx, "the document", vec!["one".into()])
            .await
            .expect("add_context never fails the ingest");

        assert_eq!(
            out,
            vec!["A report about widget sales.\n\none".to_string()],
            "the provider's summary must reach the chunks"
        );
        let requests = fake.requests();
        assert_eq!(
            requests.len(),
            1,
            "exactly one request reached the provider"
        );
        assert_eq!(
            requests[0]["max_tokens"], 321,
            "the budget the llm block published must be what is sent"
        );
    }

    /// The three degradation tests above cannot see this on their own: a
    /// contextual ingest that is refused by the allowlist degrades to exactly
    /// the same raw chunks a missing model produces, which is what made the
    /// original defect invisible. So the allowlist is pinned directly.
    ///
    /// Both names are load-bearing and neither is a hard dependency —
    /// `add_context` degrades when they are absent. What it must not do is be
    /// refused when they are *present*.
    #[test]
    fn the_block_declares_every_target_contextual_retrieval_reaches() {
        let requires = wafer_run::Block::info(&crate::blocks::vector::VectorBlock::new())
            .call_allowlist()
            .unwrap_or_default();
        for target in ["impresspress/llm", "wafer-run/llm"] {
            assert!(
                requires.iter().any(|r| r == target),
                "`ingestion::add_context` calls `{target}`, so the block must \
                 declare it — `RuntimeContext::dispatch_call` refuses an \
                 undeclared target before any grant check, and the refusal is \
                 swallowed into a 'no default LLM model configured' log. \
                 Declared: {requires:?}"
            );
        }
    }
}
