//! The `build()` block-registration method for [`ImpresspressBuilder`].
//!
//! A second `impl ImpresspressBuilder` block (Rust allows inherent impls to be
//! split across files within the same module tree). This is where every
//! service block, middleware block, feature block, router, and flow is wired
//! into the [`Wafer`] runtime.

use std::sync::Arc;

use wafer_run::{RuntimeError, Wafer};

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

#[cfg(feature = "native-embedding")]
use super::boot::register_vector_block;
use super::ImpresspressBuilder;
use crate::{
    blocks::{router::ImpresspressRouterBlock, storage::ImpresspressStorageBlock},
    features::FeatureConfig,
};

impl ImpresspressBuilder {
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

        // 2. Seed the shared JWT secret from config before registering the
        // config block. Native/Cloudflare have the value now, so the router
        // reads it as-is. The browser build has no secret yet (auto-generated
        // into the variables table during boot); it grabbed `jwt_secret_handle`
        // before `build()` and rotates this same lock once seeding runs, so the
        // router's per-request read then sees the real value.
        *self
            .jwt_secret
            .write()
            .expect("builder jwt_secret RwLock poisoned during build") = config
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
            self.jwt_secret.clone(),
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
