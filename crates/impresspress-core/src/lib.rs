//! impresspress-core — shared platform abstraction for impresspress.
//!
//! Contains impresspress feature blocks, the shared request pipeline, routing table,
//! feature config trait, and the auth-token policy (`crypto` module, on top of
//! `wafer_block_crypto::primitives`) used by both the Cloudflare Worker and
//! native standalone binary.

// `clippy::arc_with_non_send_sync` is stated once here, crate-wide and
// target-scoped, rather than repeated at every `Arc::new`.
//
// wafer-run's service and block traits are bounded on
// `wafer_block::compat::{MaybeSend, MaybeSync}`. Those are `Send`/`Sync` on
// native, and on wasm32 they are *unbounded* blanket markers
// (`impl<T: ?Sized> MaybeSend for T`), so `dyn Block`, `dyn Context`,
// `dyn StorageService`, `dyn LlmService`, `dyn ProviderAdmin` and every other
// such object is `!Send + !Sync` on that target by construction.
//
// The SMART POINTER is forced at every site the lint reaches, by one of two
// mechanisms — that is the claim this allow rests on, and it is narrower than
// "the code is all API-shaped".
//
//  1. Most sites feed an `Arc<dyn _>` parameter directly:
//     `wafer_run::Wafer::register_block` and the
//     `wafer_core::service_blocks::*::register_with` constructors take one by
//     value, so the value is either already such an `Arc` or a concrete type
//     built as `Arc` purely to coerce into one. `Rc` cannot: `Rc<T> as
//     Arc<dyn Trait>` does not compile (E0605). The concrete half is often
//     this crate's own — `ImpresspressRouterBlock`, `TransformersEmbedBlock`,
//     `ImpresspressStorageBlock` — but the `Arc` around it is not.
//  2. A few are struct FIELDS with no API parameter behind them —
//     `BlockState::ctx`, `DevShared`'s handles. Those are forced by the
//     NATIVE build instead: this crate is dual-target, `AuthService` and
//     friends are `MaybeSend + MaybeSync` which is real `Send + Sync` off
//     wasm32, and an `Rc` field would make the owning type fail that bound.
//
// So on wasm32 the pointer is never the free choice the lint assumes, and on
// that single-threaded target none of these is making a cross-thread claim to
// be wrong about.
//
// On native the same bounds resolve to real `Send + Sync`, the lint is
// accurate, and this allow does not apply — which is why it is `cfg_attr`'d on
// the same `target_arch = "wasm32"` predicate `wafer_block::compat` itself
// switches on, and not a blanket allow.
#![cfg_attr(
    target_arch = "wasm32",
    expect(
        clippy::arc_with_non_send_sync,
        reason = "on this single-threaded target the `Arc` is forced by the \
                  trait-object bounds, not chosen over an `Rc`"
    )
)]

pub mod after_response;
pub mod blocks;
pub mod builder;
pub mod cache;
pub mod cache_key;
pub mod config_generation;
pub mod config_source;
pub mod config_vars;
pub mod crypto;
pub mod csrf;
pub mod db_read;
pub mod deferred;
pub mod endpoint_match;
pub mod features;
pub mod flows;
#[cfg(test)]
mod htmx_guard;
pub mod http;
pub mod isolate_cell;
pub mod kv;
pub mod llm_target;
pub mod llm_wire;
pub mod log_level;
pub mod log_line;
pub mod metrics;
pub mod migration_helper;
pub mod multipart;
pub mod pipeline;
pub mod platform_state;
pub mod prepared_plan;
pub mod release_inventory;
pub mod routing;
pub mod secret_tables;
pub mod sqlite_text_error;
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
    PreparedApplication, PreparedBlockImplementation, PreparedBlockRuntime, PreparedGrantWrite,
    PreparedPlanError, PreparedReleaseAssets, PreparedResourceGrant, PreparedResourceType,
    PreparedRoute, PreparedRouteAccess, PreparedRuntimePlan, PreparedRuntimePlanSummary,
    PreparedRuntimeStructure, WaferLockIdentity, PREPARED_APPLICATION_BUILD_SHA256_VAR,
    PREPARED_APPLICATION_ID_VAR, PREPARED_PLAN_HASH_VAR, PREPARED_PLAN_MODULE_SHA256_VAR,
    PREPARED_RUNTIME_PLAN_SCHEMA_VERSION, PREPARED_WAFER_LOCK_IDENTITY_JSON_VAR,
    RELEASE_ASSET_KEYS_SHA256_VAR, RELEASE_ASSET_MANIFEST_SHA256_VAR, UNBOUND_CONFIG_GENERATION,
};
pub use routing::{ExtraRoute, RouteAccess};
