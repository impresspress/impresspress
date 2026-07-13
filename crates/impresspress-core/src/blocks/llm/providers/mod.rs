//! Provider configuration + the concrete native `ProviderLlmService`.
//!
//! `config` (the pure `ProviderConfig` / `ProviderProtocol` types) compiles
//! on every target — the feature block's schema, routes, and legacy-migration
//! all need it, including on wasm32 where the block runs against a browser LLM
//! backend.
//!
//! The HTTP provider implementation (`service::ProviderLlmService` and its
//! per-protocol codecs) is gated on `feature = "llm"`: it uses `reqwest` +
//! `tokio` for SSE streaming, neither of which compiles on
//! `wasm32-unknown-unknown`. Browser targets supply their own
//! `BrowserLlmService` (from `impresspress-web`) on the shared
//! `MultiBackendLlmService` router instead.

pub mod config;

#[cfg(feature = "llm")]
pub mod anthropic;
#[cfg(feature = "llm")]
pub mod openai;
#[cfg(feature = "llm")]
pub mod openai_compatible;
#[cfg(feature = "llm")]
mod service;
#[cfg(feature = "llm")]
pub(crate) mod sse;

#[cfg(feature = "llm")]
pub use service::ProviderLlmService;
