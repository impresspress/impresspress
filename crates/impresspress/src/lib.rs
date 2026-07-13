//! impresspress — unified CLI binary.
//!
//! This crate's primary output is the `impresspress` binary; the lib exists
//! to expose the `cli` module to integration tests in `tests/`.

pub mod cli;

/// Precompiled impresspress-web wasm, baked at build time. The CLI's sealed
/// × web flow uses this as the default when `IMPRESSPRESS_WEB_WASM` is unset.
pub static IMPRESSPRESS_WEB_WASM: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/impresspress-web.wasm"));

/// Precompiled impresspress-web JS glue, baked at build time. The CLI's sealed
/// × web flow uses this as the default when `IMPRESSPRESS_WEB_JS` is unset.
pub static IMPRESSPRESS_WEB_JS: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/impresspress-web.js"));
