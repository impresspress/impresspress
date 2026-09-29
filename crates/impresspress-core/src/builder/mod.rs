//! ImpresspressBuilder — unified WAFER runtime setup for all platforms.
//!
//! Each platform (native, browser WASM, Cloudflare Workers) provides its own
//! service implementations and calls the builder. The builder handles all
//! common registration: service blocks, middleware, feature blocks, router, flow.
//!
//! Split by concern across the `builder/` submodules:
//! - this module — the [`ImpresspressBuilder`] struct, its `Default` impl, and
//!   the fluent config/setter methods.
//! - [`registration`] — the `build()` block-registration method (a second
//!   `impl ImpresspressBuilder` block).
//! - [`boot`] — the one post-build lifecycle: [`boot`], [`InitPolicy`],
//!   [`GrantSource`], [`BootHooks`], and the native-embedding
//!   `register_vector_block` helper.
//! - [`config`] — [`RuntimeConfig`], the one owner of both config surfaces.

use std::{collections::HashMap, sync::Arc};

use wafer_core::interfaces::{
    config::service::ConfigService, crypto::service::CryptoService,
    database::service::DatabaseService, image::service::ImageService, llm::service::LlmService,
    logger::service::LoggerService, network::service::NetworkService,
    storage::service::StorageService,
};
use wafer_run::Block;

use crate::{features::BlockSettings, ExtraRoute, RouteAccess};

mod boot;
mod config;
mod prepared;
mod registration;

pub use boot::{
    boot, BlockInitOutcome, BootHooks, BootReport, GrantSource, InitPolicy, StepOutcome,
    DEPLOY_RESPONSE_SCHEMA_VERSION, PREPARE_RUNTIME_PLAN_KEY,
};
pub use config::{fill_config_service, RuntimeConfig, ServiceOnlyKey};
pub use prepared::PreparedPlanExporter;
/// Re-exported so `impresspress-cloudflare`'s wasm test lane can assert what
/// this crate's own test code cannot: that a runtime built for wasm32 really
/// does carry all six middleware blocks. See
/// [`registration::register_middleware_blocks`].
#[cfg(feature = "wasm")]
pub use registration::register_discovered_blocks;
#[cfg(all(test, feature = "block-fastembed"))]
pub(crate) use registration::required_model_cache_dir;
/// The alias list the builder installs, for `test_support::TestContext` to
/// resolve calls through.
#[cfg(any(test, feature = "test-support"))]
pub(crate) use registration::{granted_llm_router, SERVICE_ALIASES};
pub use registration::{register_middleware_blocks, MIDDLEWARE_BLOCKS};

pub struct ImpresspressBuilder {
    database: Option<Arc<dyn DatabaseService>>,
    storage: Option<Arc<dyn StorageService>>,
    config: Option<Arc<dyn ConfigService>>,
    /// The synchronous `ctx.config_get` surface, installed on the `Wafer` at
    /// the end of `build()`. Set together with `config` by
    /// [`RuntimeConfig::install`] — never separately.
    config_snapshot: HashMap<String, String>,
    crypto: Option<Arc<dyn CryptoService>>,
    network: Option<Arc<dyn NetworkService>>,
    logger: Option<Arc<dyn LoggerService>>,
    block_settings: Arc<std::sync::RwLock<BlockSettings>>,
    /// JWT secret shared with the router behind a lock so builds that only
    /// learn it *after* `build()` (browser/OPFS — the secret is auto-generated
    /// into the variables table during boot) can populate it via
    /// [`Self::jwt_secret_handle`]. Native/Cloudflare read it from config at
    /// build time and never rotate. `build()` seeds it from
    /// `WAFER_RUN__AUTH__JWT_SECRET`.
    jwt_secret: Arc<std::sync::RwLock<String>>,
    block_configs: Vec<(String, serde_json::Value)>,
    /// Overrides applied after built-in flow registration. Needed when an
    /// application intentionally replaces a config that `register_site_main`
    /// installs near the end of `build()`.
    final_block_configs: Vec<(String, serde_json::Value)>,
    /// Deployment-owned external WRAP grants. Block-declared grants remain
    /// sourced from `BlockInfo`; these are applied before `Wafer::seal()`.
    wrap_grants: Vec<wafer_run::ResourceGrant>,
    /// Deployment-owned grants imported from a prepared plan (for example
    /// admin-created grants captured from D1 during candidate preparation).
    deployment_wrap_grants: Vec<wafer_run::ResourceGrant>,
    extra_blocks: Vec<(String, Arc<dyn Block>)>,
    /// Additional LLM backends to register on the `MultiBackendLlmService`
    /// router backing `wafer-run/llm`. Each entry is `(label, service)` and
    /// follows the same order semantics as `MultiBackendLlmService::register`:
    /// first match on `claims_backend` wins. On native builds with the `llm`
    /// feature enabled, `"provider"` is auto-registered first, so HTTP
    /// providers (OpenAI/Anthropic/etc.) take precedence over any backend
    /// added here for overlapping `backend_id`s.
    extra_llm_services: Vec<(String, Arc<dyn LlmService>)>,
    /// Additional `ImageService` backends to register on the
    /// `MultiBackendImageService` router backing `wafer-run/image`. Same
    /// shape and order semantics as `extra_llm_services`. No built-in
    /// provider on native — the prototype's only backend is
    /// `BrowserImageService` from `impresspress-web`.
    extra_image_services: Vec<(String, Arc<dyn ImageService>)>,
    /// Routes registered by downstream projects via `add_route`. Checked
    /// after built-in `ROUTES` — built-ins always win on prefix collision.
    extra_routes: Vec<ExtraRoute>,
    /// Filesystem path to the SQLite database.
    ///
    /// Only used by the `native-embedding` feature to open a dedicated
    /// `rusqlite::Connection` for `SqliteVecService`. Kept as `Option<String>`
    /// (rather than feature-gated) so platforms can always pass it; the
    /// field is simply ignored when the feature is off.
    sqlite_db_path: Option<String>,
    /// Directory the native ONNX embedding model's weights are cached in
    /// (downloaded there on first use).
    ///
    /// Only used by the `block-fastembed` feature (`impresspress/fastembed`,
    /// which `native-embedding` implies). Kept as `Option` (rather than
    /// feature-gated) so platforms can always pass it; a build with the
    /// feature on and no directory is refused.
    model_cache_dir: Option<std::path::PathBuf>,
    /// Browser-side `VectorService` and `EmbeddingService`, each registered on
    /// its own: `build()` registers `wafer-run/vector` over the vector
    /// service and `impresspress/transformers-embed` over the embedding
    /// service. The native `register_vector_block` path is gated behind the
    /// `native-embedding` feature.
    extra_vector_service: Option<Arc<dyn wafer_core::interfaces::vector::service::VectorService>>,
    extra_embedding_service:
        Option<Arc<dyn wafer_core::interfaces::vector::service::EmbeddingService>>,
    /// Per-block env-config source consulted on first init. Defaults to an
    /// empty [`wafer_run::StaticConfigSource`] if unset — sufficient for
    /// blocks that declare no required config or that read their config from
    /// `RuntimeContext::block_configs` (composite/uses). Native consumers
    /// should pass `EnvConfigSource`; cloudflare consumers pass
    /// `D1ConfigSource`.
    config_source: Option<Arc<dyn wafer_run::ConfigSource>>,
}

impl Default for ImpresspressBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ImpresspressBuilder {
    pub fn new() -> Self {
        Self {
            database: None,
            storage: None,
            config: None,
            config_snapshot: HashMap::new(),
            crypto: None,
            network: None,
            logger: None,
            block_settings: Arc::new(std::sync::RwLock::new(BlockSettings::from_map(
                HashMap::new(),
            ))),
            jwt_secret: Arc::new(std::sync::RwLock::new(String::new())),
            block_configs: Vec::new(),
            final_block_configs: Vec::new(),
            wrap_grants: Vec::new(),
            deployment_wrap_grants: Vec::new(),
            extra_blocks: Vec::new(),
            extra_llm_services: Vec::new(),
            extra_image_services: Vec::new(),
            extra_routes: Vec::new(),
            sqlite_db_path: None,
            model_cache_dir: None,
            extra_vector_service: None,
            extra_embedding_service: None,
            config_source: None,
        }
    }

    pub fn database(mut self, svc: Arc<dyn DatabaseService>) -> Self {
        self.database = Some(svc);
        self
    }

    pub fn storage(mut self, svc: Arc<dyn StorageService>) -> Self {
        self.storage = Some(svc);
        self
    }

    /// Both config surfaces at once. Private on purpose: the only way in is
    /// [`RuntimeConfig::install`], which cannot hand over one surface without
    /// the other.
    fn with_config_surfaces(
        mut self,
        svc: Arc<dyn ConfigService>,
        snapshot: HashMap<String, String>,
    ) -> Self {
        self.config = Some(svc);
        self.config_snapshot = snapshot;
        self
    }

    pub fn crypto(mut self, svc: Arc<dyn CryptoService>) -> Self {
        self.crypto = Some(svc);
        self
    }

    pub fn network(mut self, svc: Arc<dyn NetworkService>) -> Self {
        self.network = Some(svc);
        self
    }

    pub fn logger(mut self, svc: Arc<dyn LoggerService>) -> Self {
        self.logger = Some(svc);
        self
    }

    /// Set the initial [`BlockSettings`] for the runtime.
    ///
    /// The settings are stored behind an `Arc<RwLock<…>>` so that consumers
    /// who can't fully populate them at `build()` time can update the snapshot
    /// later via [`Self::block_settings_handle`] (e.g. the browser/OPFS build
    /// reads block_settings rows from the DB only AFTER `init_block(admin)`
    /// has created the table). Builds that have the final settings up-front
    /// can just call this once and ignore the handle.
    pub fn block_settings(self, settings: BlockSettings) -> Self {
        *self
            .block_settings
            .write()
            .expect("BlockSettings RwLock poisoned during build configuration") = settings;
        self
    }

    /// Return a shared handle to the runtime's [`BlockSettings`] snapshot.
    ///
    /// Use this when block_settings can only be loaded *after* the wafer is
    /// built and `init_block(admin)` has created the backing table. Writes
    /// through the handle are visible to the router's `FeatureConfig`
    /// (which holds the same `Arc<RwLock<BlockSettings>>`), so the blocks
    /// [`boot`] initializes after the seed hook see enablement state that
    /// matches the loaded rows. The handle remains valid for the lifetime of
    /// the wafer.
    pub fn block_settings_handle(&self) -> Arc<std::sync::RwLock<BlockSettings>> {
        self.block_settings.clone()
    }

    /// Return a shared handle to the runtime's JWT secret.
    ///
    /// Same rationale as [`Self::block_settings_handle`]: the browser/OPFS
    /// build only learns `WAFER_RUN__AUTH__JWT_SECRET` after `init_block(admin)`
    /// seeds it, so it grabs this handle before `build()` and writes the real
    /// value through it once seeding runs. The router holds the same
    /// `Arc<RwLock<String>>` and reads it per request, so verification always
    /// uses the current secret — matching the crypto service, which is rotated
    /// the same way in the same boot hook. Builds that have the secret in
    /// config up-front never need to touch this.
    pub fn jwt_secret_handle(&self) -> Arc<std::sync::RwLock<String>> {
        self.jwt_secret.clone()
    }

    pub fn extra_block(mut self, name: impl Into<String>, block: Arc<dyn Block>) -> Self {
        self.extra_blocks.push((name.into(), block));
        self
    }

    /// Register an additional `LlmService` backend on the router backing
    /// `wafer-run/llm`. The `label` is used in log/tracing output and must be
    /// unique across registrations (collision is not enforced — later
    /// registrations simply lose to earlier ones on overlapping
    /// `backend_id`s). The backend itself decides which `backend_id`s it
    /// claims via `claims_backend`.
    ///
    /// On native builds with the `llm` feature enabled, the built-in
    /// `"provider"` backend is registered first (in `build()`) and therefore
    /// takes precedence over services added via this method for overlapping
    /// `backend_id`s. This is the expected ordering: HTTP providers win over
    /// browser-only backends on native.
    ///
    /// On wasm32 builds (where the `llm` feature is off), the router is still
    /// created and the `wafer-run/llm` service block is still registered —
    /// it just contains only the backends passed in via this setter.
    pub fn llm_service(mut self, label: impl Into<String>, service: Arc<dyn LlmService>) -> Self {
        self.extra_llm_services.push((label.into(), service));
        self
    }

    /// Register an additional `ImageService` backend on the router backing
    /// `wafer-run/image`. Mirrors `llm_service` — `label` is for tracing,
    /// dispatch is by `claims_backend`. Order semantics: first
    /// `claims_backend` match wins.
    pub fn image_service(
        mut self,
        label: impl Into<String>,
        service: Arc<dyn ImageService>,
    ) -> Self {
        self.extra_image_services.push((label.into(), service));
        self
    }

    /// Inject a browser-side `VectorService` (e.g. `BrowserVectorService` from
    /// `impresspress-browser`). `build()` registers `wafer-run/vector` over it.
    /// Mutually exclusive with the `native-embedding` feature path — both
    /// produce `wafer-run/vector`, and `build()` fails on the second register.
    pub fn vector_service(
        mut self,
        svc: Arc<dyn wafer_core::interfaces::vector::service::VectorService>,
    ) -> Self {
        self.extra_vector_service = Some(svc);
        self
    }

    /// Inject a browser-side `EmbeddingService` (e.g. `BrowserEmbeddingService`
    /// from `impresspress-browser`). `build()` registers
    /// `impresspress/transformers-embed` over it. Refused in a build with the
    /// `block-fastembed` feature, whose `impresspress/fastembed` already serves
    /// `embedding@v1`.
    pub fn embedding_service(
        mut self,
        svc: Arc<dyn wafer_core::interfaces::vector::service::EmbeddingService>,
    ) -> Self {
        self.extra_embedding_service = Some(svc);
        self
    }

    /// Supply the runtime's [`wafer_run::ConfigSource`] for lazy per-block
    /// env-config loading. If not provided, defaults to an empty
    /// [`wafer_run::StaticConfigSource`] — sufficient for blocks that declare
    /// no required config or that read their config from
    /// `RuntimeContext::block_configs` (composite/uses). Native consumers
    /// should pass `EnvConfigSource`; cloudflare consumers pass
    /// `D1ConfigSource`.
    pub fn config_source(mut self, source: Arc<dyn wafer_run::ConfigSource>) -> Self {
        self.config_source = Some(source);
        self
    }

    pub fn block_config(mut self, name: impl Into<String>, config: serde_json::Value) -> Self {
        self.block_configs.push((name.into(), config));
        self
    }

    /// Apply a block config after built-in flow registration has completed.
    ///
    /// Use this for deliberate final overrides such as an application's
    /// top-level router catch-all. Unlike post-build mutation, this declaration
    /// is visible to prepared-plan export/import and target parity checks.
    pub fn final_block_config(
        mut self,
        name: impl Into<String>,
        config: serde_json::Value,
    ) -> Self {
        self.final_block_configs.push((name.into(), config));
        self
    }

    /// Supply deployment-owned external WRAP grants without a runtime DB read.
    pub fn wrap_grants(mut self, grants: Vec<wafer_run::ResourceGrant>) -> Self {
        self.wrap_grants = grants;
        self
    }

    /// Register a downstream-project route that dispatches to a custom block.
    ///
    /// Built-in impresspress routes take priority — an extra route with the same
    /// prefix as a built-in (e.g. `/b/auth/`) is ignored. To disable a
    /// built-in route, turn off its feature flag.
    ///
    /// `access` declares the auth tier:
    /// - [`RouteAccess::Public`] — no auth check.
    /// - [`RouteAccess::Authenticated`] — a request with no identity is sent
    ///   to login (a browser page) or refused `401` (an API call).
    /// - [`RouteAccess::Admin`] — as `Authenticated`, and an identity without
    ///   the `admin` role is refused `403`.
    pub fn add_route(
        mut self,
        prefix: impl Into<String>,
        block_name: impl Into<String>,
        access: RouteAccess,
    ) -> Self {
        self.extra_routes
            .push(ExtraRoute::new(prefix, block_name, access));
        self
    }

    /// Register a route whose block's endpoint declarations ARE its
    /// authorization model: `access` is a floor, and any path under `prefix`
    /// that the block has not declared a `BlockEndpoint` for requires a
    /// logged-in caller.
    ///
    /// [`Self::add_route`] is the right call for a catch-all block that
    /// serves paths it never declares. This one is for a block whose code the
    /// host did not write — the dev sandbox's compiled guests — where an
    /// undeclared path must not inherit a permissive prefix tier. See
    /// [`ExtraRoute`]'s doc comment.
    pub fn add_refined_route(
        mut self,
        prefix: impl Into<String>,
        block_name: impl Into<String>,
        access: RouteAccess,
    ) -> Self {
        self.extra_routes
            .push(ExtraRoute::refined(prefix, block_name, access));
        self
    }

    /// Set the filesystem path to the SQLite database file.
    ///
    /// Only consumed by the `native-embedding` feature to open a second
    /// `rusqlite::Connection` for the `SqliteVecService` backing
    /// `wafer-run/vector`. SQLite supports multi-connection access in WAL
    /// mode, so sharing the underlying file is safe. Without this path,
    /// `native-embedding` cannot register the vector runtime block — the
    /// `build()` call will return an error.
    pub fn sqlite_db_path(mut self, path: impl Into<String>) -> Self {
        self.sqlite_db_path = Some(path.into());
        self
    }

    /// Set the directory the native ONNX embedding model is cached in.
    ///
    /// Consumed by the `block-fastembed` feature (which `native-embedding`
    /// implies), whose `FastembedService` downloads the model there on first
    /// use. Without it, a build with the feature on returns an error: the
    /// directory is the embedder's to choose, and `wafer-block-fastembed`
    /// reads none of its own.
    pub fn model_cache_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.model_cache_dir = Some(dir.into());
        self
    }
}
