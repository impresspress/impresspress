//! ImpresspressBuilder — unified WAFER runtime setup for all platforms.
//!
//! Each platform (native, browser WASM, Cloudflare Workers) provides its own
//! service implementations and calls the builder. The builder handles all
//! common registration: service blocks, middleware, feature blocks, router, flow.

use std::{collections::HashMap, sync::Arc};

// Force linker inclusion of wafer-block-* crates so their linkme
// distributed-slice entries land in the binary. Without these `use as _`
// anchors the linker excludes the crate's .o file entirely and the
// register_static_block! entries never appear in STATIC_BLOCK_REGISTRATIONS.
wafer_block::use_static_blocks!(
    wafer_block_cors,
    wafer_block_inspector,
    wafer_block_readonly_guard,
    wafer_block_router,
    wafer_block_security_headers,
    wafer_block_web,
);
use wafer_core::interfaces::{
    config::service::ConfigService, crypto::service::CryptoService,
    database::service::DatabaseService, image::service::ImageService, llm::service::LlmService,
    logger::service::LoggerService, network::service::NetworkService,
    storage::service::StorageService,
};
use wafer_run::{Block, RuntimeError, Wafer};

use crate::{
    blocks::{router::ImpresspressRouterBlock, storage::ImpresspressStorageBlock},
    features::{BlockSettings, FeatureConfig},
    ExtraRoute, RouteAccess,
};

pub struct ImpresspressBuilder {
    database: Option<Arc<dyn DatabaseService>>,
    storage: Option<Arc<dyn StorageService>>,
    config: Option<Arc<dyn ConfigService>>,
    crypto: Option<Arc<dyn CryptoService>>,
    network: Option<Arc<dyn NetworkService>>,
    logger: Option<Arc<dyn LoggerService>>,
    block_settings: Arc<std::sync::RwLock<BlockSettings>>,
    block_configs: Vec<(String, serde_json::Value)>,
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
    /// Browser-side `VectorService` + `EmbeddingService`. When both are
    /// `Some`, `build()` registers `wafer-run/vector` (with the pair) and
    /// `impresspress/transformers-embed` (with the embedding service). The
    /// native `register_vector_block` path is gated behind the
    /// `native-embedding` feature and remains unaffected.
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
            crypto: None,
            network: None,
            logger: None,
            block_settings: Arc::new(std::sync::RwLock::new(BlockSettings::from_map(
                HashMap::new(),
            ))),
            block_configs: Vec::new(),
            extra_blocks: Vec::new(),
            extra_llm_services: Vec::new(),
            extra_image_services: Vec::new(),
            extra_routes: Vec::new(),
            sqlite_db_path: None,
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

    pub fn config(mut self, svc: Arc<dyn ConfigService>) -> Self {
        self.config = Some(svc);
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
    /// (which holds the same `Arc<RwLock<BlockSettings>>`), so a follow-up
    /// `init_all_blocks()` sees enablement state that matches the loaded
    /// rows. The handle remains valid for the lifetime of the wafer.
    pub fn block_settings_handle(&self) -> Arc<std::sync::RwLock<BlockSettings>> {
        self.block_settings.clone()
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
    /// `impresspress-browser`). When both `vector_service` and `embedding_service`
    /// are provided, `build()` registers `wafer-run/vector` with the pair and
    /// `impresspress/transformers-embed` with the embedding half. Mutually
    /// exclusive with the `native-embedding` feature path — both produce
    /// `wafer-run/vector` and would conflict on register.
    pub fn vector_service(
        mut self,
        svc: Arc<dyn wafer_core::interfaces::vector::service::VectorService>,
    ) -> Self {
        self.extra_vector_service = Some(svc);
        self
    }

    /// Inject a browser-side `EmbeddingService` (e.g. `BrowserEmbeddingService`
    /// from `impresspress-browser`). See `vector_service` for full semantics.
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

    /// Register a downstream-project route that dispatches to a custom block.
    ///
    /// Built-in impresspress routes take priority — an extra route with the same
    /// prefix as a built-in (e.g. `/b/auth/`) is ignored. To disable a
    /// built-in route, turn off its feature flag.
    ///
    /// `access` declares the auth tier:
    /// - [`RouteAccess::Public`] — no auth check.
    /// - [`RouteAccess::Authenticated`] — rejects empty user_id with 403.
    /// - [`RouteAccess::Admin`] — requires the `admin` role or 403.
    pub fn add_route(
        mut self,
        prefix: impl Into<String>,
        block_name: impl Into<String>,
        access: RouteAccess,
    ) -> Self {
        self.extra_routes.push(ExtraRoute {
            prefix: prefix.into(),
            block_name: block_name.into(),
            access,
        });
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

    pub fn build(self) -> Result<(Wafer, Arc<ImpresspressStorageBlock>), RuntimeError> {
        // 1. Validate required services
        let database = self
            .database
            .ok_or_else(|| RuntimeError::Config("database service required".into()))?;
        let storage = self
            .storage
            .ok_or_else(|| RuntimeError::Config("storage service required".into()))?;
        let config = self
            .config
            .ok_or_else(|| RuntimeError::Config("config service required".into()))?;
        let crypto = self
            .crypto
            .ok_or_else(|| RuntimeError::Config("crypto service required".into()))?;
        let network = self
            .network
            .ok_or_else(|| RuntimeError::Config("network service required".into()))?;
        let logger = self
            .logger
            .ok_or_else(|| RuntimeError::Config("logger service required".into()))?;

        // 2. Read JWT secret before registering config block
        let jwt_secret = config
            .get(crate::blocks::auth::JWT_SECRET_KEY)
            .unwrap_or_default();

        // 3. Create runtime
        let config_source = self
            .config_source
            .clone()
            .unwrap_or_else(|| Arc::new(wafer_run::StaticConfigSource::default()));
        let mut wafer = Wafer::new(config_source)?;
        wafer.set_admin_block("impresspress/admin");

        // 4. Register service blocks
        wafer_core::service_blocks::database::register_with(&mut wafer, database)?;
        wafer
            .add_alias("db", "wafer-run/database")
            .map_err(|e| RuntimeError::Config(format!("add_alias db: {e}")))?;

        // `Arc::from(&'static str)` allocates the inline buffer once; no
        // `String::to_string` round-trip needed for a literal identifier.
        let admin_block_id: Arc<str> = Arc::from("impresspress/admin");
        let storage_block = crate::blocks::storage::create(storage, admin_block_id);
        wafer.register_block("wafer-run/storage", storage_block.clone())?;
        wafer
            .add_alias("storage", "wafer-run/storage")
            .map_err(|e| RuntimeError::Config(format!("add_alias storage: {e}")))?;

        wafer_core::service_blocks::config::register_with(&mut wafer, config)?;
        wafer_core::service_blocks::crypto::register_with(&mut wafer, crypto)?;

        wafer_core::service_blocks::network::register_with(&mut wafer, network)?;

        wafer_core::service_blocks::logger::register_with(&mut wafer, logger)?;

        // 4c. Construct the LLM service + router and register `wafer-run/llm`.
        //     The feature block `impresspress/llm` receives `provider_llm_svc`
        //     via its constructor for admin CRUD and `lifecycle(Init)`
        //     configure. Chat/model-listing requests from the feature block
        //     go through `ctx.call_block("wafer-run/llm", ...)`, which hits
        //     the `MultiBackendLlmService` router registered here.
        //
        //     On native (`llm` feature on) the HTTP `ProviderLlmService` is
        //     auto-registered under `"provider"` first — reqwest-based
        //     providers aren't Send-safe on wasm32, so the `llm` feature
        //     gates them. Additional backends passed via
        //     `.llm_service(label, svc)` are registered after `"provider"`
        //     and lose to it on overlapping `backend_id`s.
        //
        //     On wasm32 (`llm` feature off) the router is built empty and
        //     populated purely from `.llm_service(...)` entries (typically a
        //     `BrowserLlmService` from `impresspress-web`). If no backends are
        //     registered, the router is still installed — its
        //     `claims_backend` returns false for all ids and produces clean
        //     `unknown backend_id` errors via the standard router dispatch.
        let mut llm_router = wafer_core::interfaces::llm::router::MultiBackendLlmService::new();

        #[cfg(feature = "llm")]
        let provider_llm_svc = {
            let svc = Arc::new(crate::blocks::llm::providers::ProviderLlmService::new());
            llm_router.register("provider", svc.clone());
            svc
        };

        for (label, svc) in self.extra_llm_services {
            llm_router.register(label, svc);
        }

        // `MultiBackendLlmService` holds `dyn LlmService` backends, which only
        // require `MaybeSend + MaybeSync` (real `Send + Sync` on native, a
        // no-op marker on wasm32 — see wafer_block::compat), so this `Arc`
        // doesn't promise cross-thread safety on wasm32; wasm32 is
        // single-threaded.
        #[allow(clippy::arc_with_non_send_sync)]
        wafer_core::service_blocks::llm::register_with(&mut wafer, Arc::new(llm_router))?;

        // 4a-bis. Build the image router and register the service block
        // backing `wafer-run/image`. Mirrors the LLM path above — no built-in
        // native provider for the prototype; backends are populated entirely
        // from `.image_service(...)` entries (typically a `BrowserImageService`
        // from `impresspress-web`).
        let mut image_router =
            wafer_core::interfaces::image::router::MultiBackendImageService::new();
        for (label, svc) in self.extra_image_services {
            image_router.register(label, svc);
        }
        // `MultiBackendImageService` holds `dyn ImageService` backends, which
        // only require `MaybeSend + MaybeSync` (real `Send + Sync` on native,
        // a no-op marker on wasm32 — see wafer_block::compat), so this `Arc`
        // doesn't promise cross-thread safety on wasm32; wasm32 is
        // single-threaded.
        #[allow(clippy::arc_with_non_send_sync)]
        wafer_core::service_blocks::image::register_with(&mut wafer, Arc::new(image_router))?;

        // 4b. Register the `wafer-run/vector` runtime block when the
        // `native-embedding` feature is on. `impresspress/vector` declares
        // `requires=["wafer-run/vector"]`, so without this registration
        // dependency resolution fails at startup.
        #[cfg(feature = "native-embedding")]
        register_vector_block(&mut wafer, self.sqlite_db_path.as_deref())?;

        // Browser path: when callers (typically `impresspress-web`) inject vector
        // + embedding services, register the runtime block + transformers
        // embed feature block. Mutually exclusive with `native-embedding` —
        // both producing `wafer-run/vector` would conflict on register.
        if let (Some(vec_svc), Some(emb_svc)) =
            (self.extra_vector_service, self.extra_embedding_service)
        {
            wafer_core::service_blocks::vector::register_with(
                &mut wafer,
                vec_svc,
                emb_svc.clone(),
            )?;
            #[cfg(target_arch = "wasm32")]
            {
                // `TransformersEmbedBlock` only requires `MaybeSend + MaybeSync`
                // (a no-op marker on wasm32 — see wafer_block::compat), so this
                // `Arc` doesn't promise cross-thread safety; wasm32 is
                // single-threaded and this whole block is wasm32-only.
                #[allow(clippy::arc_with_non_send_sync)]
                wafer.register_block(
                    "impresspress/transformers-embed".to_string(),
                    Arc::new(
                        crate::blocks::transformers_embed::TransformersEmbedBlock::new(emb_svc),
                    ),
                )?;
            }
        }

        // 5. The wafer-run/* middleware blocks (cors, inspector, readonly-guard,
        // router, security-headers, web) self-register via `register_static_block!`
        // in their respective wafer-block-* crates. The `use wafer_block_xxx as _`
        // anchors at the top of this file ensure the linker includes those crate
        // .o files so the linkme distributed-slice entries land in the binary.
        //
        // linkme's distributed_slice does not work on wasm32 (its link-section
        // attributes only target ELF/Mach-O/PE — see linkme-impl/src/declaration.rs
        // for the target_os match), so on wasm32 the auto-registration is a no-op.
        // Register the six middleware blocks explicitly when targeting wasm32.
        #[cfg(target_arch = "wasm32")]
        {
            wafer.register_block(
                "wafer-run/cors",
                Arc::new(wafer_block_cors::CorsBlock::new()),
            )?;
            wafer.register_block(
                "wafer-run/inspector",
                Arc::new(wafer_block_inspector::InspectorBlock::new()),
            )?;
            wafer.register_block(
                "wafer-run/readonly-guard",
                Arc::new(wafer_block_readonly_guard::ReadonlyGuardBlock::new()),
            )?;
            wafer.register_block(
                "wafer-run/router",
                Arc::new(wafer_block_router::RouterBlock::new()),
            )?;
            wafer.register_block(
                "wafer-run/security-headers",
                Arc::new(wafer_block_security_headers::SecurityHeadersBlock::new()),
            )?;
            wafer.register_block("wafer-run/web", Arc::new(wafer_block_web::WebBlock::new()))?;
        }

        // 5a. Register every zero-arg impresspress feature block (`impresspress/*`)
        // from the single manifest in `crate::blocks`. The same call runs on
        // native and wasm32 — there is no longer a linkme (native) /
        // hand-synced-list (wasm32) split. Previously native relied on
        // per-block `register_static_block!` (linkme) and wasm32 on a separate
        // `register_all_static_blocks` list; both are gone. The three
        // non-zero-arg blocks (`llm`, framework `auth`, wasm32
        // `transformers-embed`) are registered explicitly below.
        crate::blocks::register_feature_blocks(&mut wafer)?;

        wafer.add_block_config(
            "wafer-run/inspector",
            serde_json::json!({ "allow_anonymous": false }),
        );

        // 5b. Apply platform-specific block configs
        for (name, config) in self.block_configs {
            wafer.add_block_config(&name, config);
        }

        // 6. Register the framework AuthBlock — not in the feature-block
        //    manifest because its constructor takes `Arc<dyn AuthService>`. The
        //    wrapped AuthServiceImpl picks up its Context handle when the
        //    runtime fires the block's lifecycle(Init) event.
        crate::blocks::register_auth(&mut wafer)?;

        // 6b. Register LlmBlock — not in the feature-block manifest because its
        //     constructor takes `Arc<dyn ProviderAdmin>`.
        //
        //     Native (`not(wasm32)`): with `feature = "llm"` the concrete
        //     `ProviderLlmService` (already on the router under `"provider"`)
        //     doubles as the provider-admin handle; a native
        //     `block-llm`-without-`llm` build falls back to the no-op.
        #[cfg(all(feature = "block-llm", not(target_arch = "wasm32")))]
        {
            use crate::blocks::llm::provider_admin::ProviderAdmin;
            // `provider_llm_svc` is already registered on the router under
            // `"provider"` (it was cloned there); this is its last use, so move
            // rather than clone it into the provider-admin handle.
            #[cfg(feature = "llm")]
            let provider_admin: Arc<dyn ProviderAdmin> = provider_llm_svc;
            #[cfg(not(feature = "llm"))]
            let provider_admin: Arc<dyn ProviderAdmin> =
                Arc::new(crate::blocks::llm::provider_admin::NoopProviderAdmin);
            crate::blocks::register_llm(&mut wafer, provider_admin)?;
        }

        // 6c. Register LlmBlock on wasm32 against a browser-supplied
        //     `LlmService` (installed on the router via
        //     `ImpresspressBuilder::llm_service`). Provider CRUD / discovery have no
        //     browser surface, so a `NoopProviderAdmin` stands in for the native
        //     HTTP `ProviderLlmService`.
        #[cfg(all(feature = "block-llm", target_arch = "wasm32"))]
        crate::blocks::register_llm(
            &mut wafer,
            Arc::new(crate::blocks::llm::provider_admin::NoopProviderAdmin),
        )?;

        // 7. Extra platform-specific blocks
        for (name, block) in self.extra_blocks {
            wafer.register_block(&name, block)?;
        }

        // 10. Build and register the impresspress router.
        //     Collect BlockInfo from the registry AFTER all blocks are registered
        //     so that the discovery endpoints (/openapi.json, /.well-known/agent.json)
        //     see the full set. Wafer is the single source of truth — no parallel
        //     HashMap needed.
        // Pass the shared lock directly — the router's Arc<dyn FeatureConfig>
        // sees post-build mutations via the same RwLock. See the doc comment
        // on `block_settings_handle()` for why this matters.
        let feature_config: Arc<dyn FeatureConfig> = self.block_settings.clone();
        let block_infos = wafer.block_infos();
        let routes_cfg = crate::routing::routes_config(&block_infos);
        let router = ImpresspressRouterBlock::with_extra_routes(
            jwt_secret,
            feature_config,
            block_infos,
            self.extra_routes,
        );
        // `ImpresspressRouterBlock` holds `Arc<dyn FeatureConfig>`, which only
        // requires `MaybeSend + MaybeSync` (real `Send + Sync` on native, a
        // no-op marker on wasm32 — see wafer_block::compat), so this `Arc`
        // doesn't promise cross-thread safety on wasm32; wasm32 is
        // single-threaded.
        #[allow(clippy::arc_with_non_send_sync)]
        wafer.register_block("impresspress/router", Arc::new(router))?;
        wafer.add_block_config("impresspress/router", routes_cfg);

        // 11. Auto-discover WASM blocks from cwd/blocks/**/target/block.wasm
        //     and flow JSON files from cwd/flows/**/*.json.
        //     Only available when compiled with the `wasm` feature (wasmi interpreter).
        #[cfg(feature = "wasm")]
        {
            use std::sync::Arc;

            use wafer_run::{
                discovery::{discover_flows, discover_wasm_blocks},
                wasm::WasmiBlock,
            };

            let cwd = std::env::current_dir().map_err(|e| {
                RuntimeError::Config(format!("failed to get current directory: {e}"))
            })?;

            // Discover and load WASM blocks.
            let wasm_paths = discover_wasm_blocks(&cwd.join("blocks"));
            for wasm_path in &wasm_paths {
                let bytes = match std::fs::read(wasm_path) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(path = %wasm_path.display(), error = %e, "failed to read WASM block — skipping");
                        continue;
                    }
                };
                let block = match WasmiBlock::load_from_bytes(&bytes) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(path = %wasm_path.display(), error = %e, "failed to load WASM block — skipping");
                        continue;
                    }
                };
                let name = block.info().name.clone();
                tracing::info!(name = %name, path = %wasm_path.display(), "discovered WASM block");
                wafer.register_block(&name, Arc::new(block)).map_err(|e| {
                    RuntimeError::Wasm(format!("auto-discovered block '{name}': {e}"))
                })?;
            }

            // Discover and load flow JSON files.
            let flow_paths = discover_flows(&cwd.join("flows"));
            for flow_path in &flow_paths {
                let json = match std::fs::read_to_string(flow_path) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(path = %flow_path.display(), error = %e, "failed to read flow JSON — skipping");
                        continue;
                    }
                };
                match wafer.add_flow_json(&json) {
                    Ok(()) => {
                        tracing::info!(path = %flow_path.display(), "discovered flow");
                    }
                    Err(e) => {
                        tracing::warn!(path = %flow_path.display(), error = %e, "failed to load flow JSON — skipping");
                    }
                }
            }
        }

        // 12. Register site-main flow
        crate::flows::register_site_main(&mut wafer)?;

        Ok((wafer, storage_block))
    }
}

/// Call after `wafer.start()` or `wafer.seal()` to inject
/// collected WRAP grants into the storage block for cross-block access control.
pub fn post_start(wafer: &Wafer, storage_block: &ImpresspressStorageBlock) {
    storage_block.update_wrap_grants(wafer.wrap_grants());
}

/// Per-target boot I/O for [`boot`]. Implemented by each platform to supply
/// the one step that genuinely differs between them: what to seed and which
/// shared snapshots to publish into once the admin block's `Init` has created
/// the variables / block_settings tables.
///
/// Everything else around it — the invariant `seal → admin-first init →
/// (seed) → init_all_blocks → post_start` ordering — is owned by [`boot`], so
/// the two stateless targets (Cloudflare, browser) collapse to a hook impl
/// plus the platform-specific request/response plumbing.
#[wafer_block::wafer_async_trait]
pub trait BootHooks {
    /// Runs AFTER `init_block(admin)` (so the admin migration has created the
    /// `impresspress__admin__variables` + `block_settings` tables) and BEFORE
    /// `init_all_blocks` (so a block depending on a seeded `auto_generate` key
    /// can't lose the `HashMap::keys()` race and permanent-fail on a missing
    /// secret — the impresspress #209 regression class).
    ///
    /// Implementations call [`crate::boot::seed_auto_generated`] /
    /// [`crate::boot::seed_and_load_variables`] /
    /// [`crate::features::load_and_seed_block_settings`] as appropriate and
    /// publish the results into the shared `ConfigService` / `BlockSettings`
    /// handle / crypto secret the runtime already holds.
    ///
    /// Errors abort the boot — return `Err` only for a genuinely fatal
    /// condition (e.g. the JWT secret can't be read on a target that needs it
    /// before any request). Best-effort per-key seed failures should be logged
    /// and swallowed inside the impl.
    async fn seed_after_admin_init(&self, wafer: &Wafer) -> Result<(), String>;
}

/// Target-agnostic boot orchestrator owning the invariant post-build lifecycle
/// sequence shared by the stateless targets (Cloudflare Workers, browser WASM):
///
/// 1. `seal()` — finalize composite/uses/capability/snapshot wiring.
/// 2. `init_block(admin)` FIRST — admin's migrations create the variables /
///    block_settings tables before any other block's `Init` writes to them,
///    and before the seed step reads them. Failure is logged-and-tolerated
///    (matching `init_all_blocks`' resilience contract) so one broken block
///    can't wedge the whole runtime.
/// 3. `hooks.seed_after_admin_init` — seed + publish (see [`BootHooks`]).
/// 4. `init_all_blocks()` — eager-init the rest; admin is a slot-cached no-op.
/// 5. `post_start()` — inject WRAP grants into the storage block.
///
/// The caller must have already wired the pre-seal bits its platform needs
/// (`set_config_snapshot`, `set_asset_loader`, any post-build block
/// registration) onto `wafer` before calling this. After it returns, the
/// caller dispatches requests / stores the runtime handle as appropriate.
///
/// Native uses this funnel too, then runs the native-only
/// [`Wafer::run_start_lifecycle`] + [`Wafer::bind_all`] steps afterwards: its
/// HTTP-listener block binds the TCP socket in the `Start`-lifecycle `bind()`
/// pass, which the stateless targets omit (they dispatch per-request via
/// `wafer.run`). Native still seeds pre-wafer — its immutable
/// `Argon2JwtCryptoService` and config snapshot need the variables before
/// `build()` — so its `seed_after_admin_init` is a no-op, mirroring how
/// Cloudflare reads its config pre-build and only runs the auto-gen pass here.
pub async fn boot(
    wafer: &mut Wafer,
    storage_block: &ImpresspressStorageBlock,
    hooks: &dyn BootHooks,
) -> Result<(), RuntimeError> {
    wafer.seal().await?;

    // Admin first — its Init creates the variables / block_settings tables the
    // seed step reads, and migration 002's `block` column the auto-gen seeder
    // writes. Tolerated-and-logged, like every other init.
    if let Err(e) = wafer.init_block(crate::blocks::admin::ADMIN_BLOCK_ID).await {
        tracing::warn!(error = %e, "admin block Init failed before seeding");
    }

    hooks
        .seed_after_admin_init(wafer)
        .await
        .map_err(RuntimeError::Config)?;

    wafer.init_all_blocks().await;

    post_start(wafer, storage_block);
    Ok(())
}

/// Register the `wafer-run/vector` runtime block backed by native
/// `SqliteVecService` + `FastembedService`.
///
/// - Opens a dedicated `rusqlite::Connection` at `db_path`. SQLite supports
///   multi-connection access with WAL, so sharing the DB file with the
///   platform's `DatabaseService` connection is safe.
/// - `FastembedService::default_model()` triggers an ONNX model download on
///   first run. Failure is logged but does not abort startup — the vector
///   runtime block simply won't be registered, and any attempt to use it
///   will fail via the normal dependency-resolution path.
///
/// This function is only compiled when the `native-embedding` feature is on;
/// the `impresspress/vector` feature block registration in `impresspress-core` is
/// gated by the same feature so the two stay in sync.
#[cfg(feature = "native-embedding")]
fn register_vector_block(wafer: &mut Wafer, db_path: Option<&str>) -> Result<(), RuntimeError> {
    use wafer_block_fastembed::FastembedService;
    use wafer_block_sqlite::vector::SqliteVecService;
    use wafer_core::interfaces::vector::service::{EmbeddingService, VectorService};

    let Some(db_path) = db_path else {
        return Err(RuntimeError::Config(
            "native-embedding feature is enabled but no sqlite_db_path was \
             provided to ImpresspressBuilder — call .sqlite_db_path(...) before \
             .build()"
                .to_string(),
        ));
    };

    // Dedicated connection for the vector service — see module docs on
    // `sqlite_db_path` for why a second connection is fine.
    let vec_conn = rusqlite::Connection::open(db_path).map_err(|e| {
        RuntimeError::Config(format!(
            "failed to open SQLite connection at '{db_path}' for vector service: {e}"
        ))
    })?;
    let vec_svc: Arc<dyn VectorService> = Arc::new(SqliteVecService::new(vec_conn));

    let emb_svc: Arc<dyn EmbeddingService> = match FastembedService::default_model() {
        Ok(svc) => Arc::new(svc),
        Err(e) => {
            // Model download can fail offline or on first-run with restricted
            // egress. Log and skip registration so the rest of the runtime
            // boots; `impresspress/vector` registration will fail dep resolution
            // with a clearer error than a half-wired block would.
            tracing::warn!(
                error = ?e,
                "fastembed model unavailable — skipping wafer-run/vector registration"
            );
            return Ok(());
        }
    };

    wafer_core::service_blocks::vector::register_with(wafer, vec_svc, emb_svc)
}
