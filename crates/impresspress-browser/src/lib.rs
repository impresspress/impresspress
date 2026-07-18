//! Browser platform services + Service-Worker plumbing for WAFER-in-browser
//! applications.
//!
//! This crate is the browser half of Impresspress's framework layer. It provides
//! factory functions for platform services (sql.js database, OPFS storage,
//! fetch network, browser crypto, console logger, SW asset loader), an async
//! `db_init` helper, and thread_local SW runtime plumbing
//! (`store_wafer`/`dispatch_request`/`is_initialized`). Consumers compose these
//! in their own `#[wasm_bindgen]` entrypoints using any app-level builder.
//!
//! Native web-bundle tooling (the `sw.js`/`loader.js`/`index.html` templates
//! and the wasm-pack output bundler) lives in the sibling `impresspress-bundle`
//! crate, so this crate stays a pure wasm32 cdylib that the native `impresspress`
//! CLI never has to compile.

// Pure-Rust modules — available on all targets (native + wasm32).
// openai_codec is pure Rust and tested on native; the rest of `llm` is too
// (stubs today, real impls later behind wasm32 cfg inside the module).
pub mod image;
pub mod llm;
pub mod vector;

// Pure param/row codec for the sql.js bridge edge — split out of the
// wasm32-only `database` module so it unit-tests on the host. Only consumed by
// the wasm32 `database` module, so it's compiled on wasm32 (real use) or under
// `test` (host unit tests) — not on plain native builds, where it'd be dead.
#[cfg(any(target_arch = "wasm32", test))]
mod db_codec;

// Pure opaque list-cursor codec for the OPFS `storage` bridge edge — split out
// of the wasm32-only `storage` module (same rationale as `db_codec`) so it
// unit-tests on the host. Compiled on wasm32 (real use) or under `test` (host
// unit tests), never on plain native builds where it'd be dead.
#[cfg(any(target_arch = "wasm32", test))]
mod storage_cursor;

// wasm32-only — use wasm-bindgen, web-sys, js-sys.
#[cfg(target_arch = "wasm32")]
pub mod asset_loader;
#[cfg(target_arch = "wasm32")]
pub mod bridge;
#[cfg(target_arch = "wasm32")]
pub mod convert;
#[cfg(target_arch = "wasm32")]
pub mod crypto;
#[cfg(target_arch = "wasm32")]
pub mod database;
#[cfg(target_arch = "wasm32")]
pub mod logger;
#[cfg(target_arch = "wasm32")]
pub mod network;
#[cfg(target_arch = "wasm32")]
pub mod runtime;
#[cfg(target_arch = "wasm32")]
pub mod storage;

#[cfg(target_arch = "wasm32")]
pub use asset_loader::make_sw_asset_loader;
#[cfg(target_arch = "wasm32")]
pub use crypto::make_crypto_service;
#[cfg(target_arch = "wasm32")]
pub use database::make_database_service;
#[cfg(target_arch = "wasm32")]
pub use logger::make_console_logger;
#[cfg(target_arch = "wasm32")]
pub use network::make_network_service;
#[cfg(target_arch = "wasm32")]
pub use runtime::{dispatch_request, is_initialized, store_wafer};
#[cfg(target_arch = "wasm32")]
pub use storage::make_storage_service;

/// Load sql.js WASM and open (or create) the OPFS-backed database.
/// Call once at startup, before constructing platform services. Wraps
/// `bridge::dbInit()`.
///
/// Not idempotent: each call re-loads sql.js and reassigns the module-level
/// `_db` handle inside bridge.js, losing any in-memory state written after
/// a prior call. Consumers should guard with `is_initialized()` before
/// calling this on a re-entry path.
///
/// Propagates a rejected `dbInit()` promise (sql.js WASM failed to load,
/// OPFS unavailable, etc.) as `Err` instead of letting it panic the Service
/// Worker — see `bridge::dbInit`'s `#[wasm_bindgen(catch)]`.
#[cfg(target_arch = "wasm32")]
pub async fn db_init() -> Result<(), wasm_bindgen::JsValue> {
    bridge::dbInit().await?;
    Ok(())
}
