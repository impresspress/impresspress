//! The browser boot funnel: everything a service-worker runtime does between
//! `db_init()` and `store_wafer()`, for any app built on this crate.
//!
//! The order is fixed here so no consumer keeps a copy of it. Before this
//! module existed it lived privately in `impresspress-web`, and every other
//! browser app had to hand-copy it, which is the "hand-maintained orderings"
//! defect `impresspress_core::builder::boot` exists to remove.
//!
//! ```text
//! prepare_worker()                       tracing + queued deferral, once
//! db_init()                              sql.js + OPFS
//! BrowserServices::new(page_engines)     the services every runtime shares
//! ┌ services.builder().await?            pre-configured ImpresspressBuilder
//! │   … consumer: extra_block, add_route, block_config (its CSP) …
//! │ services.build(builder)?             -> (Wafer, BrowserBootHooks)
//! │   … consumer: the gap — the built Wafer before it is sealed,
//! │       e.g. wafer.add_block_config("wafer-run/router", …) …
//! └ boot(&mut wafer, &hooks.with_app_seed(seed)).await?
//! store_wafer(wafer)
//! ```
//!
//! The bracketed part is one runtime; a consumer that rebuilds (the sandbox's
//! activation swap in `impresspress-web`) runs it again over the same
//! [`BrowserServices`], so the rebuilt runtime keeps the seeded JWT secret and
//! the loaded config.
//!
//! What stays the consumer's: its own blocks, routes and security headers
//! (`wafer-run/security-headers` takes one `block_config`, the last one
//! wins), anything it changes in the gap, and its app-specific seeds — an
//! [`AppSeed`], which is optional.

mod hooks;
#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use hooks::{AppSeed, BrowserBootHooks};
use impresspress_core::builder::{self, ImpresspressBuilder};
use wafer_core::interfaces::{
    config::service::ConfigService,
    crypto::service::CryptoService,
    image::service::ImageService,
    llm::service::LlmService,
    vector::service::{EmbeddingService, VectorService},
};
use wasm_bindgen::JsValue;

/// What every service-worker boot sets before anything else, and before its
/// `is_initialized()` check: the console tracing subscriber and the queued
/// deferral mode.
///
/// Tracing first: `tracing`'s default dispatcher discards every event, so
/// until the subscriber is installed each `warn!`/`error!` in
/// `impresspress-core`, `wafer-run` and this crate goes nowhere — and a
/// framework failure the code deliberately survives (a malformed LLM chunk, a
/// refused subrequest) leaves a console with nothing in it. Idempotent, and a
/// subscriber someone installed first is kept.
///
/// Then deferral: work a handler defers joins its own request's
/// after-response queue, which [`crate::dispatch_fetch`] hands the service
/// worker as `after` to keep alive with `event.waitUntil`. The default, a
/// bare `spawn_local`, is tied to no event, so nothing kept the worker alive
/// for it. See `impresspress_core::deferred`.
pub fn prepare_worker() {
    let _ = crate::init_console_tracing();
    impresspress_core::deferred::set_mode(impresspress_core::deferred::DeferMode::Queued);
}

/// The `pageEngines` option of `initialize()`: the page-side model engines the
/// bundle ships, as URL paths, rendered by `impresspress-bundle` from its one
/// list (`PAGE_ENGINES` in `sw.js.tmpl`). The runtime's pages load exactly
/// these, so the runtime publishes what the bundle actually shipped rather
/// than a list of its own. `options` is the object `sw.js` passes to
/// `initialize()`.
///
/// Missing or malformed — a `sw.js` rendered before the option existed — is
/// NO engines, with a console warning: the pages then carry no engine
/// scripts, and `bridge.js` refuses each model request at once ("no open page
/// runs the … engine") rather than the runtime failing to boot.
pub fn page_engines_option(options: &JsValue) -> Vec<String> {
    let value = js_sys::Reflect::get(options, &JsValue::from_str("pageEngines"))
        .unwrap_or(JsValue::UNDEFINED);
    let parsed: Result<Vec<String>, String> = if value.is_undefined() || value.is_null() {
        Err("initialize() was not handed any".to_string())
    } else {
        js_sys::JSON::stringify(&value)
            .ok()
            .and_then(|json| json.as_string())
            .ok_or_else(|| "the option is not JSON-serializable".to_string())
            .and_then(|json| serde_json::from_str(&json).map_err(|e| e.to_string()))
    };
    parsed.unwrap_or_else(|reason| {
        web_sys::console::warn_1(
            &format!(
                "impresspress: no page engine scripts ({reason}); pages will load no model \
                 engine and every LLM, image and embedding request will be refused — \
                 rebuild the bundle with a current impresspress-bundle"
            )
            .into(),
        );
        Vec::new()
    })
}

/// The browser platform services, constructed once per service worker and
/// shared by every runtime it builds.
///
/// Sharing is what keeps a rebuild cheap and, more importantly, what keeps the
/// seeded JWT secret and the loaded config alive across a swap: the crypto
/// service, `ConfigService` and config source the new runtime gets are the
/// same allocations the old one held.
pub struct BrowserServices {
    /// `initialize({ pageEngines })` ([`page_engines_option`]), comma-joined
    /// and published as `impresspress_core::ui::PAGE_ENGINE_SCRIPTS_CONFIG_KEY`
    /// by the boot hook. Every page the runtime renders loads them: they are
    /// what answers the requests the browser LLM, image and embedding services
    /// post to a page.
    page_engine_scripts: String,
    config_svc: Arc<dyn ConfigService>,
    /// The runtime's per-block [`wafer_run::ConfigSource`]. Built empty and
    /// filled by [`BrowserBootHooks`] once the variables table exists — see
    /// `SharedConfigSource` for why the browser is the one target that cannot
    /// supply this at build time.
    config_source: Arc<impresspress_core::config_source::SharedConfigSource>,
    /// Held as the concrete type (not `Arc<dyn CryptoService>`) so
    /// [`BrowserBootHooks`] can rotate the JWT secret through `set_jwt_secret`
    /// after admin's migration seeds it.
    crypto: Arc<crate::crypto::BrowserCryptoService>,
    llm: Arc<dyn LlmService>,
    image: Arc<dyn ImageService>,
    vector: Arc<dyn VectorService>,
    embedding: Arc<dyn EmbeddingService>,
}

impl BrowserServices {
    /// Construct the browser services once. `page_engine_scripts` is
    /// [`page_engines_option`]'s answer. `BrowserEmbeddingService::new` is the
    /// only fallible one.
    pub fn new(page_engine_scripts: Vec<String>) -> Result<Self, String> {
        let config_svc: Arc<dyn ConfigService> =
            Arc::new(wafer_core::service_blocks::config::EnvConfigService::new());

        // Empty until `seed_after_admin_init` publishes into it. See the field
        // comment: this is the browser's answer to "build first, learn the
        // config second", and without it every block that declares a required
        // key with an empty default fails init permanently.
        let config_source = Arc::new(impresspress_core::config_source::SharedConfigSource::new());

        // JWT secret can't be loaded yet (the variables table doesn't exist
        // until admin's migration runs). Construct the concrete
        // `BrowserCryptoService` so we keep a typed handle for
        // `set_jwt_secret` in the boot hook; the same allocation is what the
        // builder receives as `Arc<dyn CryptoService>`, so the rotation is
        // observed by every block through the service it already holds.
        let crypto = Arc::new(crate::crypto::BrowserCryptoService::new(String::new()));

        let llm: Arc<dyn LlmService> = Arc::new(crate::llm::BrowserLlmService::new());
        let image: Arc<dyn ImageService> = Arc::new(crate::image::BrowserImageService::new());
        let vector: Arc<dyn VectorService> = Arc::new(crate::vector::BrowserVectorService::new());
        // Logged as well as returned: this is the one fallible service, the
        // error surfaces to JS as an opaque `initialize()` rejection, and the
        // console line is what tells an operator *which* service failed.
        let embedding: Arc<dyn EmbeddingService> =
            match crate::vector::BrowserEmbeddingService::new() {
                Ok(svc) => Arc::new(svc),
                Err(e) => {
                    web_sys::console::error_1(&format!("BrowserEmbeddingService init: {e}").into());
                    return Err(e);
                }
            };

        Ok(Self {
            page_engine_scripts: page_engine_scripts.join(","),
            config_svc,
            config_source,
            crypto,
            llm,
            image,
            vector,
            embedding,
        })
    }

    /// An [`ImpresspressBuilder`] carrying the browser services, the block
    /// settings an earlier boot left in the database, the infrastructure
    /// config (`infrastructure_config`) on both config surfaces, and the
    /// shared config source. The consumer adds its own blocks, routes and
    /// block config, then hands it to [`Self::build`].
    ///
    /// Nothing may be SEEDED here: the `impresspress__admin__variables` /
    /// `impresspress__admin__block_settings` tables are created by admin's
    /// lazy `lifecycle(Init)`, and admin can't run until the wafer is built
    /// and sealed. The schema-drift class of bug (#210/#211) came from
    /// short-cutting that chicken-and-egg with `CREATE TABLE IF NOT EXISTS`
    /// pre-creates that duplicated admin's migration schema by hand. Admin's
    /// migration is the single source of schema truth; seeding waits for
    /// [`BrowserBootHooks`], after `init_block(admin)`.
    ///
    /// READING what an earlier boot created is another matter, and it has to
    /// happen here, as Cloudflare's builds do: every block's Init — admin's
    /// first — decides whether to run its migrations from the migration state
    /// in block_settings, which it reads off the config snapshot
    /// (`migration_helper::apply_pending`). Built with an empty snapshot,
    /// admin found no state on every boot and re-ran all of its migrations
    /// over a database it had already migrated. `load` never writes, and on a
    /// fresh profile — no table yet — it answers empty settings.
    pub async fn builder(&self) -> Result<ImpresspressBuilder, JsValue> {
        let db = crate::make_database_service();
        let initial_block_settings = impresspress_core::platform_state::block_settings::load(&db)
            .await
            .map_err(|e| JsValue::from_str(&format!("load block settings: {e}")))?;
        let initial_config = infrastructure_config(initial_block_settings.to_config_json());

        // The services' own `SharedConfigSource`, EMPTY at this point and
        // filled by the boot hook once admin's migration has created the
        // variables table.
        //
        // It used to be a permanently-empty `StaticConfigSource`, on the
        // premise that impresspress blocks read their keys through
        // `config_client::get` (`wafer-run/config` → ConfigService) rather
        // than the Init payload, so the source did not matter. The premise is
        // true and the conclusion was wrong: the runtime resolves a block's
        // DECLARED keys through this source *before* calling its
        // `lifecycle(Init)` at all, and a required key it cannot resolve is
        // `InitError::Permanent`. `impresspress/products` declares one
        // (`IMPRESSPRESS__PRODUCTS__WEBHOOK_SECRET`, auto-generated, no
        // default), so every browser bundle answered `412
        // FailedPrecondition` on every products route — the block never
        // reached the code that would have read the value.
        let cfg_source: Arc<dyn wafer_run::ConfigSource> = self.config_source.clone();
        let crypto_svc: Arc<dyn CryptoService> = self.crypto.clone();

        // Both config surfaces, holding only the block settings read above,
        // the migration consent and the request-log policy. The browser cannot
        // know a single VARIABLE at build time: the `variables` table does not
        // exist until admin's migration runs, so every value arrives through
        // `BrowserBootHooks::seed_after_admin_init`, which publishes it onto
        // both surfaces at once via `RuntimeConfig::republish`.
        //
        // `fill_config_service` rather than a closure that ignores its
        // argument: this handle is filled by `set`, so the map must be written
        // through it, or the keys above would reach the snapshot and be
        // dropped from the async surface, silently.
        let config_svc = self.config_svc.clone();
        let (with_config, ()) = initial_config.install(
            ImpresspressBuilder::new()
                .database(crate::make_database_service())
                .storage(crate::make_storage_service()),
            |map| (builder::fill_config_service(config_svc, map), ()),
        );

        Ok(with_config
            .crypto(crypto_svc)
            .network(crate::make_network_service())
            .logger(crate::make_console_logger())
            .llm_service("browser", self.llm.clone())
            .image_service("browser", self.image.clone())
            .vector_service(self.vector.clone())
            .embedding_service(self.embedding.clone())
            .block_settings(initial_block_settings)
            .config_source(cfg_source))
    }

    /// Build the runtime `builder` describes and install the service worker's
    /// asset loader, returning it unsealed together with the hooks [`boot`]
    /// runs it under.
    ///
    /// The gap between this and [`boot`] is the consumer's: the returned
    /// `Wafer` is built but not sealed, so whatever it changes there — a
    /// block registered with `register_block`, the outer
    /// `wafer-run/router`'s route table replaced with `add_block_config` —
    /// is what `boot` seals and initializes.
    pub fn build(
        &self,
        builder: ImpresspressBuilder,
    ) -> Result<(wafer_run::Wafer, BrowserBootHooks), JsValue> {
        let block_settings_handle = builder.block_settings_handle();
        // The router verifies JWTs against this secret. It's empty at build
        // time (the variables table doesn't exist yet), so grab the handle and
        // rotate it to the seeded value in `seed_after_admin_init`, alongside
        // the crypto service that signs with it.
        let jwt_secret_handle = builder.jwt_secret_handle();

        let mut wafer = builder
            .build()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        wafer.set_asset_loader(&crate::make_sw_asset_loader());

        let hooks = BrowserBootHooks {
            db: crate::make_database_service(),
            config_svc: self.config_svc.clone(),
            config_source: self.config_source.clone(),
            block_settings_handle,
            jwt_secret_handle,
            crypto: self.crypto.clone(),
            page_engine_scripts: self.page_engine_scripts.clone(),
            app_seed: None,
        };
        Ok((wafer, hooks))
    }
}

/// Run the shared boot funnel over a runtime [`BrowserServices::build`] built:
/// grants → seal → `init_block(admin)` → the seed hook → the remaining blocks
/// (`impresspress_core::builder::boot`).
///
/// admin's `lifecycle(Init)` runs FIRST so its migrations create the canonical
/// `impresspress__admin__variables` + `block_settings` tables before
/// [`BrowserBootHooks`] reads them — admin's migration is the single source
/// of schema truth (the #210/#211 schema-drift lesson). The hook then seeds
/// and publishes into the services the wafer already holds, all over
/// `BrowserDatabaseService`.
///
/// On success the runtime is ready for the consumer to `store_wafer` (or
/// `replace_wafer`).
pub async fn boot(wafer: &mut wafer_run::Wafer, hooks: &BrowserBootHooks) -> Result<(), JsValue> {
    builder::boot(
        wafer,
        hooks,
        // The admin permissions page is served in the browser too, so the
        // rows it writes are this runtime's deployment-owned grants. On a
        // fresh profile the table does not exist yet and `load` degrades to
        // no grants.
        builder::GrantSource::Database(&hooks.db),
        // A browser runtime is long-lived and inspectable (and rebuilt on
        // demand), so one broken block must not wedge the whole page.
        builder::InitPolicy::Tolerant,
    )
    .await
    .map_err(|e| JsValue::from_str(&format!("boot: {e}")))?;
    Ok(())
}

/// The config both surfaces start every browser build from: the block settings
/// read back from the database (`block_settings_json`), the migration consent
/// and the request-log policy.
fn infrastructure_config(block_settings_json: String) -> builder::RuntimeConfig {
    let mut config = builder::RuntimeConfig::new();
    config
        .both(
            impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY,
            block_settings_json,
        )
        // The browser applies its own pending migrations, as a native boot
        // does: loading a new bundle IS deploying it. There is no operator
        // step, no `/_deploy/init`, and no other place a browser install
        // could ever apply a changed migration, so without it a database
        // created by an older bundle would log "schema drift" and keep its
        // old schema for good. The gate still skips a block whose recorded
        // hash matches the code, so a migration runs once per change, not
        // once per boot.
        .both(impresspress_core::migration_helper::RUN_MIGRATIONS_KEY, "1")
        // A browser build is one person's local instance, and its database
        // persists by exporting the WHOLE file on every flush: a
        // `request_logs` row for every request would grow every later flush
        // without bound, and each logged request would cost an export of its
        // own after its reply. Server errors are what the log is for here.
        // Infrastructure config (`IMPRESSPRESS_*`, never stored in the
        // database), so no admin setting reaches it.
        .both(
            impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY,
            "errors",
        );
    config
}
