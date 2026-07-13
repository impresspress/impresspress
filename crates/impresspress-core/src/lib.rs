//! impresspress-core — shared platform abstraction for impresspress.
//!
//! Contains impresspress feature blocks, the shared request pipeline, routing table,
//! feature config trait, and the auth-token policy (`crypto` module, on top of
//! `wafer_block_crypto::primitives`) used by both the Cloudflare Worker and
//! native standalone binary.

pub mod admin_schema;
pub mod blocks;
pub mod boot;
pub mod builder;
pub mod cache;
pub mod cache_key;
pub mod config_source;
pub mod config_vars;
pub mod crypto;
pub mod deploy_init;
pub mod endpoint_match;
pub mod features;
pub mod flows;
pub mod http;
pub mod kv;
pub mod messages_schema;
pub mod migration_helper;
pub mod multipart;
pub mod pipeline;
pub mod routing;
pub mod ui;
pub mod util;

// Exposed to the `tests/` integration-test crates (and any consumer that
// wants the shared `TestContext` harness) behind the `test-support` feature,
// in addition to the crate's own `#[cfg(test)]` unit tests. Gating it on a
// feature — rather than `#[cfg(test)]` only — is what lets the integration
// tests reuse `TestContext` instead of re-implementing it.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use features::FeatureConfig;
pub use migration_helper::db_backend;
pub use pipeline::handle_request;
pub use routing::{ExtraRoute, RouteAccess};
