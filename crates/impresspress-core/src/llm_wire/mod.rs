//! Vendor chat-completion **wire formats**: request-body serialization and
//! streaming response decoding, with no HTTP client, no provider
//! configuration and no block machinery attached.
//!
//! These live at the crate root rather than under `blocks::llm::providers`
//! because their consumers do not share a feature set. The native
//! `ProviderLlmService` needs `feature = "llm"` (reqwest + tokio, neither of
//! which compiles on `wasm32-unknown-unknown`); `impresspress-browser`'s
//! WebLLM service speaks the same OpenAI chunk JSON over a postMessage bridge
//! and takes `impresspress-core` with **no** `block-*` feature at all. Gating
//! the codec behind either of those is what produced the second copy inside
//! the browser adapter, and the two copies then drifted.
//!
//! Nothing here reaches back into `blocks::`; the dependency runs one way.

pub mod openai;
/// SSE transport framing, shared by the OpenAI and Anthropic decoders.
///
/// Crate-private: the out-of-crate consumer is `impresspress-browser`, which
/// reaches [`openai::OpenAiSseDecoder`] and never the framing beneath it (its
/// bridge hands over one already-de-framed chunk at a time). Publish it when
/// something outside the crate frames its own SSE.
pub(crate) mod sse;
