//! Browser-side `VectorService` and `EmbeddingService` implementations.
//!
//! Backed by sql.js (vectors stored as BLOBs in the shared OPFS database)
//! and Transformers.js running in the page via the SW↔page bridge.

pub mod score;
pub mod sql;

// `service` depends on the wasm32-only `bridge` module (sql.js JS interop).
// Pure-Rust pieces (sql, score) compile on native for unit testing; the
// VectorService impl itself is wasm32-only — same gating as `database.rs`.
#[cfg(target_arch = "wasm32")]
pub mod service;

#[cfg(target_arch = "wasm32")]
pub use service::BrowserVectorService;

// `embedding` and `embedding_bridge` also depend on the wasm32-only `bridge`.
#[cfg(target_arch = "wasm32")]
pub mod embedding;
#[cfg(target_arch = "wasm32")]
pub mod embedding_bridge;

#[cfg(target_arch = "wasm32")]
pub use embedding::BrowserEmbeddingService;
