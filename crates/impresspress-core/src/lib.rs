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
pub mod config_probe;
pub mod config_vars;
pub mod crypto;
pub mod csrf;
pub mod deploy_init;
pub mod endpoint_match;
pub mod features;
pub mod flows;
pub mod http;
pub mod isolate_cell;
pub mod kv;
pub mod log_level;
pub mod messages_schema;
pub mod metrics;
pub mod migration_helper;
pub mod multipart;
pub mod pipeline;
pub mod prepared_plan;
pub mod release_inventory;
pub mod routing;
pub mod ssrf;
pub mod streaming;
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
pub use isolate_cell::{IdentityCache, IsolateCell};
pub use migration_helper::db_backend;
pub use pipeline::handle_request;
pub use prepared_plan::{
    PreparedApplication, PreparedBlockImplementation, PreparedBlockRuntime, PreparedPlanError,
    PreparedReleaseAssets, PreparedResourceGrant, PreparedResourceType, PreparedRoute,
    PreparedRouteAccess, PreparedRuntimePlan, PreparedRuntimePlanSummary, PreparedRuntimeStructure,
    WaferLockIdentity, PREPARED_APPLICATION_BUILD_SHA256_VAR, PREPARED_APPLICATION_ID_VAR,
    PREPARED_PLAN_HASH_VAR, PREPARED_PLAN_MODULE_SHA256_VAR, PREPARED_RUNTIME_PLAN_SCHEMA_VERSION,
    PREPARED_WAFER_LOCK_IDENTITY_JSON_VAR, RELEASE_ASSET_KEYS_SHA256_VAR,
    RELEASE_ASSET_MANIFEST_SHA256_VAR, UNBOUND_CONFIG_GENERATION,
};
pub use routing::{ExtraRoute, RouteAccess};
