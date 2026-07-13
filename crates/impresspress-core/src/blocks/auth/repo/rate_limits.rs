//! Table declaration for `wafer_run__auth__rate_limits`.
//!
//! Sliding-window counters keyed by user/IP. Row-level helpers live
//! alongside the caller in `blocks/rate_limit.rs`, which only references
//! this table on `wasm32` (the native code path uses the in-memory
//! `UserRateLimiter`). The const is platform-agnostic so we keep it
//! always-defined and silence the dead-code warning here.
#[allow(dead_code)]
pub(crate) const TABLE: &str = "wafer_run__auth__rate_limits";
