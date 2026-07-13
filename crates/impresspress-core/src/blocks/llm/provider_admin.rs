//! `ProviderAdmin` — the provider-management seam the LLM feature block holds.
//!
//! The block's chat / model-listing / status traffic goes through the
//! `wafer-run/llm` service block via `ctx.call_block` (the
//! `MultiBackendLlmService` router), so `LlmBlock` does NOT need a concrete
//! `LlmService` handle. What it does need directly is the *provider-admin*
//! surface — `configure`, `providers_snapshot`, `discover_models` — used by
//! the provider CRUD endpoints, `lifecycle(Init)`, and the legacy-provider
//! migration to keep the in-memory router in sync with the DB.
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
    /// Replace the live provider set in the in-memory router. Called on the
    /// block's `lifecycle(Init)` and after every provider CRUD write so the
    /// next chat request routes against the current configuration.
    fn configure(&self, providers: Vec<ProviderConfig>);

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
/// and discovery endpoints are admin-only and have no browser surface, so they
/// degrade cleanly — `configure` and `providers_snapshot` are inert, and
/// `discover_models` reports `NotSupported`.
pub struct NoopProviderAdmin;

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl ProviderAdmin for NoopProviderAdmin {
    fn configure(&self, _providers: Vec<ProviderConfig>) {}

    fn providers_snapshot(&self) -> Vec<ProviderConfig> {
        Vec::new()
    }

    async fn discover_models(&self, _provider_name: &str) -> Result<Vec<ModelInfo>, LlmError> {
        Err(LlmError::NotSupported)
    }
}
