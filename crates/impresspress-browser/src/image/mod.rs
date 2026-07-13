//! Browser image service — `ImageService` impl driving transformers.js
//! pipelines via a SW↔page postMessage bridge. Parallel in shape to
//! `crate::llm`.

#[cfg(target_arch = "wasm32")]
pub mod bridge;
pub mod catalog;
#[cfg(target_arch = "wasm32")]
pub mod service;

pub use catalog::{default_catalog, ModelCatalog};
#[cfg(target_arch = "wasm32")]
pub use service::BrowserImageService;
