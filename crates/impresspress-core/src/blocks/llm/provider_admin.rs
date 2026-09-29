//! `ProviderAdmin` — the provider-management seam the LLM feature block holds.
//!
//! The block's chat / model-listing / status traffic goes through the
//! `wafer-run/llm` service block via `ctx.call_block` (the
//! `MultiBackendLlmService` router), so `LlmBlock` does NOT need a concrete
//! `LlmService` handle. What it does need directly is the *provider-admin*
//! surface — `configure`, `providers_snapshot`, `discover_models` — used by
//! the provider CRUD endpoints and `lifecycle(Init)` to keep the in-memory
//! router in sync with the DB.
//!
//! Splitting this surface into its own trait lets `LlmBlock` hold
//! `Arc<dyn ProviderAdmin>` instead of the concrete, `reqwest`/`tokio`-backed
//! `ProviderLlmService`. That shrinks the `llm` cargo feature to "native
//! provider backend" and lets the block (`block-llm`) compile on wasm32,
//! where [`NoopProviderAdmin`] stands in — browser targets configure their
//! providers entirely in `BrowserLlmService`, so the admin surface is a no-op
//! there.

use async_trait::async_trait;
use wafer_core::interfaces::llm::service::{LlmError, ModelInfo};

use super::providers::config::ProviderConfig;

/// Provider-management operations the LLM feature block drives directly.
///
/// `MaybeSend + MaybeSync` mirrors the `LlmService` bound: `Send + Sync` on
/// native, unbounded on wasm32 (where the block is single-threaded and the
/// futures need not be `Send`).
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub trait ProviderAdmin: wafer_run::MaybeSend + wafer_run::MaybeSync {
    /// Whether this runtime holds a provider router that can be configured
    /// at all.
    ///
    /// This is the *capability* question, and the provider CRUD handlers ask
    /// it before they touch the database: a runtime that answers `false`
    /// refuses create / update / delete / discover with `Unimplemented`
    /// (501) rather than persisting a row that nothing will ever load.
    /// [`configure`](ProviderAdmin::configure) answering `Err` is the same
    /// fact discovered one step later, on the reload — the two must not
    /// disagree, which is what `routes::providers`'s `inert_router_tests`
    /// pins for both implementations.
    fn manages_providers(&self) -> bool;

    /// Replace the live provider set in the in-memory router. Called on the
    /// block's `lifecycle(Init)` and after every provider CRUD write so the
    /// next chat request routes against the current configuration.
    ///
    /// `Err(LlmError::NotSupported)` when there is no router to configure.
    /// This used to return `()`, so [`NoopProviderAdmin`] silently accepted
    /// every configuration and the CRUD handlers answered a green 200 over a
    /// router that had done nothing with it.
    fn configure(&self, providers: Vec<ProviderConfig>) -> Result<(), LlmError>;

    /// Read-only snapshot of the configured providers. Used to resolve the
    /// legacy default-provider alias into a concrete enabled backend_id
    /// without re-reading the DB on every request.
    fn providers_snapshot(&self) -> Vec<ProviderConfig>;

    /// Query a provider's `/v1/models` endpoint and return the discovered
    /// model list, caching it for subsequent `list_models` aggregation.
    async fn discover_models(&self, provider_name: &str) -> Result<Vec<ModelInfo>, LlmError>;
}

/// No-op `ProviderAdmin` for targets without the native HTTP provider backend
/// (`feature = "llm"` off, e.g. wasm32 / browser). The browser path configures
/// its providers inside `BrowserLlmService`; the feature block's provider CRUD
/// and discovery endpoints are admin-only and have no browser surface.
///
/// It is inert, and it says so: `manages_providers` is `false`, `configure`
/// and `discover_models` report `NotSupported`, and `providers_snapshot` is
/// empty. Saying so is the whole point — an inert handle whose `configure`
/// returned `()` let the CRUD handlers persist a provider row and answer 200
/// while nothing downstream had been told about it.
///
/// The routes stay **declared** on every target regardless: which handle a
/// deployment holds is a runtime fact, and `blocks::all_block_infos` builds
/// the published surface from a `NoopProviderAdmin` block, so deriving the
/// endpoint set from the handle would delete the provider rows from
/// `llm.openapi.json` and `llm.endpoints.json` in every snapshot build and
/// leave the published contract describing the snapshot's build rather than
/// any real deployment.
pub struct NoopProviderAdmin;

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl ProviderAdmin for NoopProviderAdmin {
    fn manages_providers(&self) -> bool {
        false
    }

    fn configure(&self, _providers: Vec<ProviderConfig>) -> Result<(), LlmError> {
        Err(LlmError::NotSupported)
    }

    fn providers_snapshot(&self) -> Vec<ProviderConfig> {
        Vec::new()
    }

    async fn discover_models(&self, _provider_name: &str) -> Result<Vec<ModelInfo>, LlmError> {
        Err(LlmError::NotSupported)
    }
}
