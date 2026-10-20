//! Browser platform services + Service-Worker plumbing for WAFER-in-browser
//! applications.
//!
//! This crate is the browser half of Impresspress's framework layer. It provides
//! factory functions for platform services (sql.js database, OPFS storage,
//! fetch network, browser crypto, console logger, SW asset loader), an async
//! `db_init` helper, the boot funnel that turns those services into a booted
//! runtime (the `boot` module), and thread_local SW runtime plumbing
//! (`store_wafer`/`dispatch_request`/`is_initialized`). Consumers call these
//! from their own `#[wasm_bindgen]` entrypoints, adding their own blocks,
//! routes and seeds at the seams `boot` leaves them.
//!
//! One entrypoint of the framework's `sw.js` is this crate's rather than the
//! consumer's: [`sandbox_mode::compiled_sandbox_mode`], which the worker
//! imports beside the consumer's `initialize` and `handle_request`. Linking
//! this crate is what exports it from every browser cdylib; a consumer
//! neither defines nor re-exports it (see [`sandbox_mode`]).
//!
//! Native web-bundle tooling (the `sw.js`/`loader.js`/`index.html` templates
//! and the wasm-pack output bundler) lives in the sibling `impresspress-bundle`
//! crate, so this crate stays a pure wasm32 cdylib that the native `impresspress`
//! CLI never has to compile.

// Pure-Rust modules — available on all targets (native + wasm32). The
// wasm32-only halves are cfg-gated inside each module.
pub mod image;
pub mod llm;
pub mod sandbox_mode;
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

// The storage service's bounded read cache — pure bookkeeping, split out of the
// wasm32-only `storage` module (same rationale as `storage_cursor`) so its
// eviction and invalidation rules unit-test on the host.
#[cfg(any(target_arch = "wasm32", test))]
mod storage_cache;

// wasm32-only — use wasm-bindgen, web-sys, js-sys.
#[cfg(target_arch = "wasm32")]
pub mod asset_loader;
// The boot funnel every browser app runs between `db_init` and
// `store_wafer`; see its module doc for the sequence.
#[cfg(target_arch = "wasm32")]
pub mod boot;
#[cfg(target_arch = "wasm32")]
pub mod bridge;
#[cfg(target_arch = "wasm32")]
pub mod convert;
#[cfg(target_arch = "wasm32")]
pub mod crypto;
#[cfg(target_arch = "wasm32")]
pub mod database;
#[cfg(target_arch = "wasm32")]
pub mod flush_scope;
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
pub use logger::{init_console_tracing, make_console_logger};
#[cfg(target_arch = "wasm32")]
pub use network::make_network_service;
#[cfg(target_arch = "wasm32")]
pub use runtime::{
    current_wafer, dispatch_fetch, dispatch_request, is_initialized, replace_wafer, restore_wafer,
    store_wafer,
};
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
/// Propagates a rejected `dbInit()` promise as `Err` instead of letting it
/// panic the Service Worker — see `bridge::dbInit`'s `#[wasm_bindgen(catch)]`.
/// The rejected value is passed through untouched: a database that could not
/// be read is an `Error` named `DatabaseUnreadable` (`dbInit` in
/// `js/bridge.js`), and `sw.js` keys on that `name` to report the failure as
/// its non-erasing `open` stage rather than `initialize` (`STAGE_OPEN` in
/// `impresspress-bundle`'s `sw.js.tmpl`), so it must reach `initialize()`'s
/// caller as the same object, never re-wrapped or turned into a string.
#[cfg(target_arch = "wasm32")]
pub async fn db_init() -> Result<(), wasm_bindgen::JsValue> {
    let opened = bridge::dbInit().await;
    // Whatever the schema cache knew described the database this replaced.
    database::forget_schema();
    opened?;
    // The database in memory is now the one on disk.
    flush_scope::loaded_from_disk();
    Ok(())
}
