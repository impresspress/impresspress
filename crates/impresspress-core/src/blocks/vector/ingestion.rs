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

// `blocks::llm` is feature-gated (`reqwest/stream` it pulls in for the SSE
// provider conflicts with `worker`'s `wasm-streams` on wasm32). When the
// feature is off we degrade silently — same fallback path as "no LLM
// configured at runtime".
#[cfg(feature = "llm")]
use wafer_block::wire::llm::{ChatContent, ChatMessage, ChatRequest, ChatRole, ChunkDelta};
#[cfg(feature = "llm")]
use wafer_core::clients::llm;
use wafer_run::{context::Context, WaferError};
#[cfg(feature = "llm")]
use wafer_run::{InputStream, Message};

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
/// Degrades silently — `chunks` is returned unchanged — when:
///   * no LLM is configured (`IMPRESSPRESS__LLM__DEFAULT_MODEL` empty),
///   * the chat call errors (block not registered, transport failure, …),
///   * the LLM returns no text (empty stream).
///
/// The ingest must not fail because the contextual step couldn't run; the
/// raw chunks are still useful for retrieval.
#[cfg(not(feature = "llm"))]
pub async fn add_context(
    ctx: &dyn Context,
    document: &str,
    chunks: Vec<String>,
) -> Result<Vec<String>, WaferError> {
    // wasm32 / no-LLM build: degrade silently — same fallback path as the
    // `llm` feature build takes when no provider is configured.
    let _ = (ctx, document);
    Ok(chunks)
}

#[cfg(feature = "llm")]
pub async fn add_context(
    ctx: &dyn Context,
    document: &str,
    chunks: Vec<String>,
) -> Result<Vec<String>, WaferError> {
    if chunks.is_empty() {
        return Ok(chunks);
    }
    let Some((provider, model)) = default_llm_target(ctx).await else {
        tracing::debug!("contextual retrieval skipped: no default LLM model configured");
        return Ok(chunks);
    };

    let request = ChatRequest {
        backend_id: provider,
        model,
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
        params: Default::default(),
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
        return Ok(chunks);
    }

    Ok(chunks
        .into_iter()
        .map(|c| format!("{context}\n\n{c}"))
        .collect())
}

/// Fetch the default `(provider, model)` LLM target via the llm block's
/// internal discovery route. Returns `None` when no model is configured or
/// when the llm block isn't registered — both cases trigger the same
/// "degrade silently" fallback in `add_context`.
///
/// Going through `ctx.call_block(...)` rather than a direct in-process
/// function call is what keeps the vector block independent of the llm
/// block at the type/dep level — that's the dependency edge Phase 0b PR-2
/// will turn into a Cargo feature gate.
#[cfg(feature = "llm")]
async fn default_llm_target(ctx: &dyn Context) -> Option<(String, String)> {
    let resource = "/b/llm/api/internal/default-target";
    let mut msg = Message::new(format!("retrieve:{resource}"));
    msg.set_meta("req.action", "retrieve");
    msg.set_meta("req.resource", resource);
    msg.set_meta("http.method", "GET");
    msg.set_meta("http.path", resource);

    let out = ctx
        .call_block("impresspress/llm", msg, InputStream::empty())
        .await;
    let buf = out.collect_buffered().await.ok()?;
    let body: serde_json::Value = serde_json::from_slice(&buf.body).ok()?;
    let provider = body.get("provider")?.as_str()?.to_string();
    let model = body.get("model")?.as_str()?.to_string();
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    Some((provider, model))
}

#[cfg(feature = "llm")]
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
