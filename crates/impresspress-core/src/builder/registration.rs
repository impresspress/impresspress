//! The `build()` block-registration method for [`ImpresspressBuilder`].
//!
//! A second `impl ImpresspressBuilder` block (Rust allows inherent impls to be
//! split across files within the same module tree). This is where every
//! service block, middleware block, feature block, router, and flow is wired
//! into the [`Wafer`] runtime.

use std::sync::Arc;

use wafer_run::{RuntimeError, Wafer};

// The single list of `wafer-run/*` middleware blocks this runtime carries.
//
// Two things come out of it. The `use <krate> as _;` anchors force linker
// inclusion of each wafer-block-* crate, so its `register_static_block!`
// entry lands in `STATIC_BLOCK_REGISTRATIONS` — without the anchor the linker
// drops the .o file and the entry never appears. And `WAFER_STATIC_BLOCKS`,
// which the macro emits into this module, carries the same entries by value
// for targets where link-time collection does not work: empty off wasm32,
// one per named crate on it. `build()` hands it to
// `Wafer::register_static_blocks` unconditionally (step 5).
//
// Adding a middleware block is therefore one edit, here. The named crate must
// itself invoke `register_static_block!`; a crate anchored for some other
// reason has no `__WAFER_STATIC_BLOCK` and fails to compile on wasm32.
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
use crate::{blocks::router::ImpresspressRouterBlock, features::FeatureConfig};

/// The six `wafer-run/*` middleware blocks, by the name each crate's
/// `register_static_block!` gives it.
///
/// Written once here and read by `MIDDLEWARE_BLOCKS`'s two tests, so a block
/// added to the anchor list above and not to this one — or renamed — is a
/// failure rather than a silently missing middleware.
pub const MIDDLEWARE_BLOCKS: &[&str] = &[
    "wafer-run/cors",
    "wafer-run/inspector",
    "wafer-run/readonly-guard",
    "wafer-run/router",
    "wafer-run/security-headers",
    "wafer-run/web",
];

/// Step 5 of [`ImpresspressBuilder::build`], as a free function.
///
/// Off wasm32 `WAFER_STATIC_BLOCKS` is empty and `Wafer::new` has already
/// installed the six through linkme, so this is a no-op. On wasm32 linkme
/// writes into a link section that does not exist, so this call is the ONLY
/// thing that registers them.
///
/// It is a free function rather than an inline statement so a **wasm32 test**
/// can run it: `impresspress-core` cannot compile its own test code for that
/// target (`--all-targets` pulls tokio/mio, which do not build there), so an
/// assertion about the wasm32 arm written here would document rather than
/// gate. `impresspress-cloudflare` has an executable wasm lane and calls this
/// from `middleware_blocks_tests`. Same shape as `blocks::rate_limit`'s
/// ungated helper, and for the same reason.
pub fn register_middleware_blocks(wafer: &mut Wafer) -> Result<(), RuntimeError> {
    wafer.register_static_blocks(WAFER_STATIC_BLOCKS)
}

impl ImpresspressBuilder {
    pub fn build(self) -> Result<Wafer, RuntimeError> {
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

        // Read the middleware config for the site-main flow now, before
        // `config` is moved into the config service block below. Used at step
        // 12 to configure the wafer-run/cors and security-headers steps.
        let cors_allowed_origins =
            config.get_default(crate::config_vars::CORS_ALLOWED_ORIGINS_KEY, "");
        let csp_directives = config.get_default(
            crate::config_vars::CSP_DIRECTIVES_KEY,
            crate::config_vars::DEFAULT_CSP_DIRECTIVES,
        );

        // 3. Create runtime. No `init_timeout` cap, and no block of this repo
        // declares an init budget, so every Init runs as long as it takes: a
        // block's Init runs its migrations, a retry replays them all from the
        // start, and a budget shorter than the slowest one on a large database
        // (admin's and messages' backfills on Postgres) would keep that block
        // from ever initializing.
        let config_source = self
            .config_source
            .clone()
            .unwrap_or_else(|| Arc::new(wafer_run::StaticConfigSource::default()));
        let mut wafer = Wafer::new(config_source)?;
        wafer.set_admin_block(crate::blocks::admin::ADMIN_BLOCK_ID);

        // 4. Register service blocks
        let config_db = database.clone();
        wafer_core::service_blocks::database::register_with(&mut wafer, database)?;
        wafer.register_block("wafer-run/storage", crate::blocks::storage::create(storage))?;
        for (alias, target) in SERVICE_ALIASES {
            wafer
                .add_alias(*alias, *target)
                .map_err(|e| RuntimeError::Config(format!("add_alias {alias}: {e}")))?;
        }

        // impresspress owns this block: the `variables` table is the config
        // store, so an admin write is visible to the next read instead of after
        // the next restart. See `blocks::config`.
        crate::blocks::config::register_with(&mut wafer, config, config_db)?;
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
        let mut llm_router = granted_llm_router();

        #[cfg(feature = "llm")]
        let provider_llm_svc = {
            // A client that cannot be built with its SSRF-revalidating
            // redirect policy is a build failure, not a degraded service:
            // the policy is what stops a compromised provider endpoint from
            // redirecting the request onto internal addresses.
            let svc = Arc::new(
                crate::blocks::llm::providers::ProviderLlmService::try_new()
                    .map_err(|e| RuntimeError::Config(format!("provider LLM service: {e}")))?,
            );
            llm_router.register("provider", svc.clone());
            svc
        };

        for (label, svc) in self.extra_llm_services {
            llm_router.register(label, svc);
        }

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
        wafer_core::service_blocks::image::register_with(&mut wafer, Arc::new(image_router))?;

        // 4b. Register the `wafer-run/vector` runtime block when the
        // `native-embedding` feature is on. `impresspress/vector` lists it
        // under `optional_requires`: without this registration the runtime
        // still boots, and the block's calls to it answer `Unimplemented`.
        #[cfg(feature = "native-embedding")]
        register_vector_block(&mut wafer, self.sqlite_db_path.as_deref())?;

        // Browser path: an injected vector service backs `wafer-run/vector`,
        // an injected embedding service backs the
        // `impresspress/transformers-embed` feature block. Each is registered
        // on its own service, on every target: the condition is the injection,
        // not the build. An injected vector service in a `native-embedding`
        // build fails on register (both produce `wafer-run/vector`).
        if let Some(vec_svc) = self.extra_vector_service {
            wafer_core::service_blocks::vector::register_with(&mut wafer, vec_svc)?;
        }
        if let Some(emb_svc) = self.extra_embedding_service {
            one_embedding_block(cfg!(feature = "block-fastembed"))?;
            wafer.register_block(
                "impresspress/transformers-embed".to_string(),
                Arc::new(crate::blocks::transformers_embed::TransformersEmbedBlock::new(emb_svc)),
            )?;
        }

        // 5. The wafer-run/* middleware blocks (cors, inspector, readonly-guard,
        // router, security-headers, web) self-register via `register_static_block!`
        // in their respective wafer-block-* crates. The `use_static_blocks!`
        // invocation at the top of this file is the ONE place they are named.
        //
        // On native the linker collects them: the anchors pull each crate's .o
        // file in so its linkme distributed-slice entry lands in the binary,
        // and `Wafer::new` has already installed them by the time we get here.
        // linkme writes into a link section wasm32 does not have, so there the
        // slice stays empty — which is what `WAFER_STATIC_BLOCKS` is for. The
        // same macro emits it into this module: empty wherever linkme works,
        // one entry per anchored crate on wasm32. So this call is
        // unconditional and a no-op off wasm32.
        //
        // This used to be a second, hand-written `#[cfg(target_arch =
        // "wasm32")]` block of six `register_block` calls mirroring the anchor
        // list, with nothing keeping the two in step — a block added to one
        // and not the other was a middleware silently missing from every
        // browser and Worker build.
        register_middleware_blocks(&mut wafer)?;

        // 5a. Register every zero-arg impresspress feature block (`impresspress/*`)
        // from the single manifest in `crate::blocks`. The same call runs on
        // native and wasm32 — there is no longer a linkme (native) /
        // hand-synced-list (wasm32) split. Previously native relied on
        // per-block `register_static_block!` (linkme) and wasm32 on a separate
        // `register_all_static_blocks` list; both are gone. The four
        // non-zero-arg blocks (`fastembed`, `llm`, framework `auth`,
        // `transformers-embed`) are registered explicitly.
        crate::blocks::register_feature_blocks(&mut wafer)?;

        // `impresspress/fastembed` caches its model under the embedder's
        // directory, so it is registered here with it rather than from the
        // zero-arg manifest.
        #[cfg(feature = "block-fastembed")]
        wafer.register_block(
            crate::blocks::fastembed::FastembedBlock::BLOCK_NAME,
            Arc::new(crate::blocks::fastembed::FastembedBlock::new(
                required_model_cache_dir(self.model_cache_dir.as_deref(), "block-fastembed")?,
            )),
        )?;

        // Admin is registered here rather than from the manifest: its
        // constructor takes the same `Arc<RwLock<BlockSettings>>` handed to
        // the router below as `Arc<dyn FeatureConfig>`, so the block toggle
        // can update the snapshot the router reads per request instead of
        // only writing the table. Without it the toggle is inert on native
        // until the process restarts.
        crate::blocks::register_admin(&mut wafer, self.block_settings.clone())?;

        wafer.add_block_config(
            "wafer-run/inspector",
            serde_json::json!({ "allow_anonymous": false }),
        );

        // 5b. Apply platform-specific block configs.
        //
        // By reference: step 12 needs the same list again, because
        // `register_site_main` has to MERGE into what a consumer declared for
        // the two middleware blocks rather than replace it, and the runtime
        // offers no way to read a block's config back off the `Wafer`.
        for (name, config) in &self.block_configs {
            wafer.add_block_config(name, config.clone());
        }

        // 6. Register the framework AuthBlock — not in the feature-block
        //    manifest because its constructor takes `Arc<dyn AuthService>`. The
        //    wrapped AuthServiceImpl picks up its Context handle when the
        //    runtime fires the block's lifecycle(Init) event.
        crate::blocks::register_auth(&mut wafer)?;

        // 6b. Register LlmBlock — not in the feature-block manifest because its
        //     constructor takes `Arc<dyn ProviderAdmin>`.
        //
        //     The `llm` feature alone decides the handle. With it, the
        //     concrete `ProviderLlmService` — already on the router under
        //     `"provider"` — doubles as the provider-admin handle; without it
        //     the runtime cannot manage providers and says so through
        //     `NoopProviderAdmin`, whose `manages_providers()` is `false`.
        //
        //     This used to be two registration sites split on
        //     `target_arch`, which was the same decision written twice: the
        //     wasm32 arm passed the no-op, and `llm` (tokio + reqwest, not
        //     `Send` on wasm32) can never be on there anyway, so it took the
        //     `not(feature = "llm")` branch of the arm it was distinguished
        //     from. A `block-llm`-without-`llm` build — every wasm32 one, and
        //     a native one that asked for it — reaches the same handle by the
        //     same line now, and the browser's `LlmService` still arrives
        //     through `ImpresspressBuilder::llm_service` on the router either
        //     way.
        #[cfg(feature = "block-llm")]
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

        // WebMCP refusals are *mostly* structural — a defect in a block's
        // own AgentTool declarations (an unrepresentable schema, a malformed
        // path template, etc.) that holds independent of caller and of
        // `effective_auth` — with one exception: `DuplicateToolName`. Tool-
        // name uniqueness is a property of the auth-filtered manifest a
        // caller actually receives, not of the deployment as a whole, so
        // that one reason is counted per-manifest against the callers who
        // can see both colliding endpoints — see
        // `wafer_core::discovery::generate_webmcp_report`'s doc comment
        // ("Refusals are the same for every caller — with one exception").
        //
        // This boot-time pass makes a `DuplicateToolName` collision
        // *visible* — logged once, for an operator to find — it does not
        // *prevent* it. The per-manifest census is fail-open at every tier
        // below the collision: a caller whose manifest sees only one of the
        // two colliding endpoints still gets that name published normally,
        // even though it is contested at a higher tier (a low-privilege
        // endpoint can silently squat a name a high-privilege one also
        // claims). What prevents it is `Wafer::seal()`: since wafer-run
        // 61e68a0 (#324) it counts agent-tool names across every registered
        // block and refuses boot with `RuntimeError::DuplicateToolNames`.
        // Every impresspress runtime seals (native boot, the Cloudflare
        // isolate cache, deploy init), so a colliding deploy never serves a
        // request. This pass is the net behind that gate — it runs first,
        // so the collision is logged alongside the boot error, and it still
        // covers a caller that hands `generate_webmcp_report` declarations
        // which never passed through `seal()`.
        //
        // The per-request manifest handler (`pipeline::handle_request`, `GET
        // /b/webmcp/manifest.json`) uses the silent `_report` form and
        // discards the refusal list because re-deriving and
        // `tracing::warn!`-ing the same facts on every anonymous GET of an
        // unauthenticated, `no-store` route is pure log amplification driven
        // by whoever is looping requests. This is the one place refusals are
        // computed and logged — once, here, from the same `block_infos`
        // snapshot the router below is built from — so an operator who
        // annotated an endpoint and is wondering why no tool (or no
        // `outputSchema`) appeared can still find out (the diagnostic moved,
        // it was not dropped).
        //
        // Lifetime: `build()` runs once per `Wafer` construction. On
        // Cloudflare Workers that is once per isolate, not once globally —
        // `impresspress-cloudflare/src/runtime_cache.rs` builds the Wafer
        // once per isolate and caches it in a thread_local, reused across
        // that isolate's requests and rebuilt only when the KV
        // config-version stamp moves. Still bounded, and a large reduction
        // from once per anonymous request.
        //
        // `caller` below is NOT arbitrary — it must be `AuthLevel::Admin`,
        // the top of the auth hierarchy. Because the per-manifest
        // `DuplicateToolName` census filters endpoints by
        // `auth_rank(effective_auth) <= ceiling`, an `Admin` ceiling makes
        // that filter true for every endpoint, so this one boot-time pass
        // still sees every collision that exists anywhere (the auth filter
        // is monotone in caller rank — see the doc comment cited above).
        // Any lower placeholder would silently miss collisions that only
        // become visible to a higher-privilege caller. `effective_auth`
        // (`|_block, ep| ep.auth`) is genuinely arbitrary here, though,
        // *because* `caller` is pinned to the ceiling: with the filter above
        // trivially true for every endpoint regardless of which resolver
        // computed it, using the router's real `routing::effective_access`
        // instead would change nothing this pass reports. Both only gate
        // which already-admitted tools reach the *served* manifest (the
        // `Value` half of the return, discarded here), never the refusal
        // list itself.
        let (_, webmcp_refusals) = wafer_core::discovery::generate_webmcp_report(
            &block_infos,
            wafer_run::AuthLevel::Admin,
            |_block, ep| ep.auth,
        );
        for refusal in &webmcp_refusals {
            tracing::warn!(
                block = %refusal.block,
                method = %refusal.method,
                path = %refusal.path,
                tool = %refusal.tool_name,
                scope = %refusal.scope,
                reason = %refusal.reason,
                "webmcp: endpoint opted in to agent-tool exposure but was refused — see \
                 `scope` for whether the whole tool or just one field was dropped"
            );
        }

        // The dev sandbox's page-scoped manifest (`GET
        // /b/dev/api/tools.json`) is projected from a curated selection
        // list rather than from `agent_tool` annotations, so its refusals
        // are invisible to the pass above — a `SELECTIONS` row naming a
        // block this build did not compile in, or a typo in one, refuses
        // there and nowhere else. Same reasoning as above applies to where
        // it is logged: that route re-derives its document on every GET (it
        // is `no-store`, and the page fetches it on load and again behind
        // its "Refresh tools" button), so logging per request would emit the
        // same static facts as many times as the page is asked. Computed and
        // logged here instead, once, from the same `block_infos` snapshot —
        // and skipped entirely when the dev block is not registered, since
        // a runtime that never serves the route has nothing to report.
        #[cfg(feature = "block-dev")]
        crate::blocks::dev::tools::log_selection_refusals(&block_infos);

        // Shared with the body-limit block below, which judges its audit
        // rows' paths against the same declarations the router routes by.
        let extra_routes = Arc::new(self.extra_routes);
        let router = ImpresspressRouterBlock::with_extra_routes_arc(
            self.jwt_secret.clone(),
            feature_config,
            block_infos.clone(),
            extra_routes.clone(),
        );
        wafer.register_block(crate::blocks::router::ROUTER_BLOCK_ID, Arc::new(router))?;
        wafer.add_block_config(crate::blocks::router::ROUTER_BLOCK_ID, routes_cfg);

        // The site-main flow names this in a step ahead of the router, so it
        // is registered wherever that flow runs — every target, no feature
        // gate. No config: the cap is a constant and the marker is on the
        // message; the declarations are only for its audit rows' paths.
        wafer.register_block(
            crate::blocks::body_limit::BLOCK_NAME,
            Arc::new(crate::blocks::body_limit::BodyLimitBlock::new(
                block_infos,
                extra_routes,
            )),
        )?;

        // 11. Auto-discover WASM blocks and flow JSON files under cwd.
        //     Only available when compiled with the `wasm` feature (wasmi interpreter).
        #[cfg(feature = "wasm")]
        {
            let cwd = std::env::current_dir().map_err(|e| {
                RuntimeError::Config(format!("failed to get current directory: {e}"))
            })?;
            register_discovered_blocks(&mut wafer, &cwd)?;
        }

        // 12. Register site-main flow, configuring its wafer-run/cors and
        // security-headers steps from the shared config read at step 2 —
        // merged into whatever the consumer declared for those blocks at step
        // 5b, never replacing it (see `flows::site_main_block_configs`).
        crate::flows::register_site_main(
            &mut wafer,
            &cors_allowed_origins,
            &csp_directives,
            &self.block_configs,
        )?;

        // Consumer-declared final overrides intentionally run after
        // `register_site_main`, which may replace earlier router/web configs.
        for (name, config) in self.final_block_configs {
            wafer.add_block_config(&name, config);
        }

        // Deployment-owned grants must be installed before the caller seals
        // the runtime. In prepared mode these come from the immutable plan;
        // native/dynamic callers may set them directly on the builder.
        let mut external_grants = self.wrap_grants;
        external_grants.extend(self.deployment_wrap_grants);
        if !external_grants.is_empty() {
            wafer.add_wrap_grants(external_grants)?;
        }

        // 13. The synchronous `ctx.config_get` surface, from the same
        // `RuntimeConfig` that produced the async `ConfigService` at step 1.
        // Installed here rather than by each target after `build()` so the two
        // surfaces cannot be filled from different literals — see
        // `builder::RuntimeConfig`.
        super::config::write_snapshot(&mut wafer, self.config_snapshot);

        Ok(wafer)
    }
}

/// Refuse an injected embedding service in a build whose
/// `impresspress/fastembed` already serves `embedding@v1`.
///
/// `impresspress/vector` resolves THE block declaring `embedding@v1`
/// (`blocks::vector::pages::embedding_block_for_model`), so a runtime serves
/// embeddings from one block; with two, which one embedded a text would depend
/// on registration order.
fn one_embedding_block(fastembed_registered: bool) -> Result<(), RuntimeError> {
    if fastembed_registered {
        return Err(RuntimeError::Config(
            "an embedding service was injected with ImpresspressBuilder::embedding_service, \
             but this build has the block-fastembed feature, whose impresspress/fastembed \
             block already serves embedding@v1; inject one or the other"
                .to_string(),
        ));
    }
    Ok(())
}

/// The embedder's model cache directory, or the build error naming the
/// builder call that supplies it. `feature` is the Cargo feature that needs
/// it, so the error says why the directory is asked for.
#[cfg(feature = "block-fastembed")]
pub(crate) fn required_model_cache_dir<'a>(
    dir: Option<&'a std::path::Path>,
    feature: &str,
) -> Result<&'a std::path::Path, RuntimeError> {
    dir.ok_or_else(|| {
        RuntimeError::Config(format!(
            "the `{feature}` feature is enabled but no model cache directory was \
             provided to ImpresspressBuilder — call .model_cache_dir(...) before \
             .build()"
        ))
    })
}

/// Register the WASM blocks under `root/blocks/**/target/block.wasm` and the
/// flows under `root/flows/**/*.json` — the deployment directory's own
/// blocks, which `ImpresspressBuilder::build` discovers under the working
/// directory.
///
/// These are blocks the operator built and placed there, so each is loaded
/// with [`WasmiBlock::load_approving_declaration`]: the capabilities its
/// `BlockInfo` declares are its bound, narrowed only by its `capabilities`
/// block config. A block loaded without a stated bound runs with
/// `BlockCapabilities::none()`. The fuel and memory limits are the runtime's
/// own ([`Wafer::resource_limits`]), the ones every other WASM block runs
/// under. A module that cannot be read or loaded is skipped with a warning;
/// one that loads but cannot register fails the build.
///
/// [`WasmiBlock::load_approving_declaration`]: wafer_run::wasm::WasmiBlock::load_approving_declaration
#[cfg(feature = "wasm")]
pub fn register_discovered_blocks(
    wafer: &mut Wafer,
    root: &std::path::Path,
) -> Result<(), RuntimeError> {
    use wafer_block::Block;
    use wafer_run::{
        discovery::{discover_flows, discover_wasm_blocks},
        wasm::WasmiBlock,
    };

    for wasm_path in &discover_wasm_blocks(&root.join("blocks")) {
        let bytes = match std::fs::read(wasm_path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(path = %wasm_path.display(), error = %e, "failed to read WASM block — skipping");
                continue;
            }
        };
        let block = match WasmiBlock::load_approving_declaration(&bytes, wafer.resource_limits()) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(path = %wasm_path.display(), error = %e, "failed to load WASM block — skipping");
                continue;
            }
        };
        let name = block.info().name.clone();
        tracing::info!(name = %name, path = %wasm_path.display(), "discovered WASM block");
        wafer
            .register_block(&name, Arc::new(block))
            .map_err(|e| RuntimeError::Wasm(format!("auto-discovered block '{name}': {e}")))?;
    }

    for flow_path in &discover_flows(&root.join("flows")) {
        let json = match std::fs::read_to_string(flow_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %flow_path.display(), error = %e, "failed to read flow JSON — skipping");
                continue;
            }
        };
        match wafer.add_flow_json(&json) {
            Ok(()) => tracing::info!(path = %flow_path.display(), "discovered flow"),
            Err(e) => {
                tracing::warn!(path = %flow_path.display(), error = %e, "failed to load flow JSON — skipping");
            }
        }
    }
    Ok(())
}

/// The short names every target's runtime answers for a service block, as
/// `(alias, target)`: `call_block("db", ..)` reaches `wafer-run/database`.
///
/// One list, read by [`ImpresspressBuilder::build`] and by
/// `test_support::TestContext`, so a test resolves a call the way the runtime
/// it models does.
pub(crate) const SERVICE_ALIASES: &[(&str, &str)] = &[
    ("db", "wafer-run/database"),
    ("storage", "wafer-run/storage"),
];

/// The `wafer-run/llm` router with no backend yet, carrying
/// [`llm_router_grants`]: what `build()` registers backends on, and what
/// `test_support::TestContext` collects the deployment's grants from.
pub(crate) fn granted_llm_router() -> wafer_core::interfaces::llm::router::MultiBackendLlmService {
    let mut router = wafer_core::interfaces::llm::router::MultiBackendLlmService::new();
    for grant in llm_router_grants() {
        router.grant(grant);
    }
    router
}

/// Who may use `wafer-run/llm`, declared on its router.
///
/// The llm handler authorizes every op against a model resource in
/// `wafer-run/llm`'s own namespace, and only that block can grant one.
/// `impresspress/llm` serves the model admin pages, which load and unload
/// models (Write); `impresspress/vector` only chats (Read).
fn llm_router_grants() -> Vec<wafer_run::ResourceGrant> {
    let models = format!(
        "{}*",
        wafer_block::wrap::resource_prefix(wafer_core::service_blocks::llm::LlmBlock::NAME)
    );
    vec![
        wafer_run::ResourceGrant::read_write("impresspress/llm", &models)
            .typed(wafer_run::ResourceType::Llm),
        #[cfg(feature = "block-vector")]
        wafer_run::ResourceGrant::read("impresspress/vector", &models)
            .typed(wafer_run::ResourceType::Llm),
    ]
}

/// The router grants, checked by the real `wafer-run/llm` handler behind a
/// fixture that enforces WRAP: without them every call from the two blocks
/// that use the router is refused.
#[cfg(all(test, feature = "llm", feature = "block-vector"))]
mod llm_router_grant_tests {
    use std::sync::Arc;

    use wafer_core::clients::llm::{self, StatusRequest, UnloadModelRequest};
    use wafer_run::ErrorCode;

    use crate::{
        blocks::llm::{
            provider_admin::ProviderAdmin,
            providers::{
                config::{ProviderConfig, ProviderProtocol},
                ProviderLlmService,
            },
        },
        test_support::TestContext,
    };

    async fn as_caller(caller: &str) -> TestContext {
        let svc = Arc::new(ProviderLlmService::try_new().expect("provider service"));
        svc.configure(vec![ProviderConfig::new(
            "local",
            ProviderProtocol::OpenAiCompatible,
            "https://llm.example",
        )
        .with_models(vec!["m".to_string()])])
            .expect("configure");
        let mut router = super::granted_llm_router();
        router.register("provider", svc);
        let block: Arc<dyn wafer_run::Block> = Arc::new(
            wafer_core::service_blocks::llm::LlmBlock::new(Arc::new(router)),
        );
        let mut ctx = TestContext::new().await;
        ctx.register_block("wafer-run/llm", block);
        ctx.running_as(caller)
    }

    fn status_req() -> StatusRequest {
        StatusRequest {
            backend_id: "local".into(),
            model_id: "m".into(),
        }
    }

    fn unload_req() -> UnloadModelRequest {
        UnloadModelRequest {
            backend_id: "local".into(),
            model_id: "m".into(),
        }
    }

    #[tokio::test]
    async fn the_llm_and_vector_blocks_reach_the_router_and_nothing_else_does() {
        let vector = as_caller("impresspress/vector").await;
        assert!(
            llm::status(&vector, &status_req()).await.is_ok(),
            "vector reads"
        );
        let refused = llm::unload_model(&vector, &unload_req())
            .await
            .expect_err("vector may not unload");
        assert_eq!(refused.code, ErrorCode::PermissionDenied);

        let admin_pages = as_caller("impresspress/llm").await;
        assert!(llm::status(&admin_pages, &status_req()).await.is_ok());
        if let Err(e) = llm::unload_model(&admin_pages, &unload_req()).await {
            assert_ne!(
                e.code,
                ErrorCode::PermissionDenied,
                "impresspress/llm writes: {e:?}"
            );
        }

        let other = as_caller("impresspress/files").await;
        let refused = llm::status(&other, &status_req())
            .await
            .expect_err("an ungranted block is refused");
        assert_eq!(refused.code, ErrorCode::PermissionDenied);
    }
}

#[cfg(test)]
mod one_embedding_block_tests {
    use super::one_embedding_block;

    /// An injected embedder is refused only where `impresspress/fastembed`
    /// already serves `embedding@v1`. A guard: `build()` passes it the
    /// `block-fastembed` feature, which no default lane compiles.
    #[test]
    fn an_injected_embedder_is_refused_only_beside_fastembed() {
        assert!(one_embedding_block(false).is_ok());
        let err = one_embedding_block(true).expect_err("two embedding blocks");
        assert!(err.to_string().contains("block-fastembed"), "{err}");
    }
}

#[cfg(test)]
mod static_block_list_tests {
    use super::*;

    /// `WAFER_STATIC_BLOCKS` carries exactly what link-time collection cannot
    /// reach on this target: nothing off wasm32, every anchored crate's block
    /// on it.
    ///
    /// Only the native half is asserted here, and it is the half this crate
    /// can execute: `impresspress-core` cannot compile test code for wasm32
    /// at all (`--all-targets` pulls the tokio/mio dev-dependencies, which do
    /// not build there), so a `cfg(target_arch = "wasm32")` arm in this file
    /// would document rather than gate — see `carry-forward` and F7. The
    /// wasm32 half — that a runtime built on that target really does carry all
    /// six — is asserted where it can run, in
    /// `impresspress-cloudflare::middleware_blocks_tests`, through the same
    /// [`register_middleware_blocks`] this crate's `build()` calls.
    #[test]
    fn wafer_static_blocks_holds_what_the_linker_could_not_collect() {
        let names: Vec<&str> = WAFER_STATIC_BLOCKS.iter().map(|r| r.name).collect();

        assert!(
            names.is_empty(),
            "off wasm32 linkme has already collected these, so the by-value \
             list must be empty or every middleware block would register \
             twice: {names:?}"
        );
    }

    /// And on native, where the linker does the collecting, the anchor list
    /// yields those same six. `Wafer::new` runs `load_inventory_blocks`, so
    /// they are present before `build()` registers anything of ours.
    ///
    /// Together with the test above this is the whole invariant: one list,
    /// the same six blocks on both paths. A crate dropped from the anchor
    /// list fails here, and so does one whose block is named something other
    /// than `MIDDLEWARE_BLOCKS` says.
    #[test]
    fn the_anchor_list_yields_the_middleware_blocks_on_native() {
        let wafer =
            wafer_run::Wafer::new(std::sync::Arc::new(wafer_run::StaticConfigSource::default()))
                .expect("Wafer::new with no lockfile");

        for name in MIDDLEWARE_BLOCKS {
            assert!(
                wafer.has_block(name),
                "{name} is not registered — is its crate still in the \
                 `use_static_blocks!` anchor list?"
            );
        }
    }
}
