//! Browser LLM — `LlmService` impl driving WebLLM's MLCEngine via a
//! SW↔page postMessage bridge. See `docs/superpowers/specs/2026-04-20-
//! llm-service-extraction-design.md`.

#[cfg(target_arch = "wasm32")]
pub mod bridge;
pub mod catalog;
pub mod openai_codec;
#[cfg(target_arch = "wasm32")]
pub mod service;

pub use catalog::{default_catalog, ModelCatalog};
#[cfg(target_arch = "wasm32")]
pub use service::BrowserLlmService;
