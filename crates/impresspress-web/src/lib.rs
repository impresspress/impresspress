//! Impresspress app compiled to WASM for running in the browser via Service Worker.
//!
//! Thin wasm-bindgen wrapper around the `impresspress-browser` framework. Uses
//! `ImpresspressBuilder` (from `impresspress-core`) to wire up the full Impresspress
//! block suite + the app-specific `BrowserLlmService`.

use std::sync::Arc;

use impresspress_core::builder::{self, ImpresspressBuilder};
use wafer_core::interfaces::config::service::ConfigService;
use wasm_bindgen::prelude::*;

pub mod config;

const IMPRESSPRESS_CSP: &str = concat!(
    "default-src 'self'; ",
    "script-src 'self' 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval' https://cdn.jsdelivr.net; ",
    "style-src 'self' 'unsafe-inline'; ",
    "img-src 'self' data: blob: https:; ",
    "font-src 'self' https:; ",
    "connect-src 'self' https://cdn.jsdelivr.net https://esm.run https://huggingface.co ",
        "https://raw.githubusercontent.com https://*.huggingface.co https://*.hf.co https://*.xethub.hf.co; ",
    "frame-ancestors 'none'; ",
    "base-uri 'self'; ",
    "form-action 'self'",
);

#[wasm_bindgen]
pub async fn initialize() -> Result<(), JsValue> {
    if impresspress_browser::is_initialized() {
        return Ok(());
    }

    impresspress_browser::db_init().await?;

    // ── Phase 1 ─────────────────────────────────────────────────────────────
    // Build the runtime with EMPTY config + EMPTY block_settings + EMPTY
    // ConfigSource. We can't fill any of these from OPFS yet because the
    // `impresspress__admin__variables` / `impresspress__admin__block_settings`
    // tables only exist after admin's lazy `lifecycle(Init)` runs its
    // migrations — and admin can't run until the wafer is built and sealed.
    //
    // The schema-drift class of bug (#210/#211) came from this crate trying
    // to short-cut that chicken-and-egg with `CREATE TABLE IF NOT EXISTS`
    // pre-creates that duplicated the admin migration schema by hand. Any
    // drift between the two schemas was silent until the first per-block
    // `migration_helper::write_state` upserted into the stale table and
    // failed on a missing column, taking the whole runtime with it.
    //
    // The proper fix is what the native CLI and Cloudflare runner already
    // do: defer seeding until *after* `init_block(admin)`. Admin's migration
    // is the single source of schema truth; this crate just reads back what
    // it created.

    let config_svc: Arc<dyn ConfigService> =
        Arc::new(wafer_core::service_blocks::config::EnvConfigService::new());
    // Empty initial BlockSettings — every block defaults to enabled. We rewrite
    // this via the handle below in Phase 3 once the real settings are loaded.
    let initial_block_settings =
        impresspress_core::features::BlockSettings::from_map(std::collections::HashMap::new());
    // Empty StaticConfigSource: blocks that look up their declared keys via
    // the runtime's ConfigSource at lifecycle(Init) payload-build time will
    // see nothing. That's fine because impresspress blocks read their keys via
    // `config_client::get` (which hits `wafer-run/config` → ConfigService)
    // rather than the Init payload, and we populate `config_svc` in Phase 3
    // below before triggering any block's Init.
    let cfg_source: Arc<dyn wafer_run::ConfigSource> =
        Arc::new(wafer_run::StaticConfigSource::default());

    let browser_llm: Arc<dyn wafer_core::interfaces::llm::service::LlmService> =
        Arc::new(impresspress_browser::llm::BrowserLlmService::new());
    let browser_image: Arc<dyn wafer_core::interfaces::image::service::ImageService> =
        Arc::new(impresspress_browser::image::BrowserImageService::new());
    let browser_vector: Arc<dyn wafer_core::interfaces::vector::service::VectorService> =
        Arc::new(impresspress_browser::vector::BrowserVectorService::new());
    let browser_embedding: Arc<dyn wafer_core::interfaces::vector::service::EmbeddingService> =
        match impresspress_browser::vector::BrowserEmbeddingService::new() {
            Ok(svc) => Arc::new(svc),
            Err(e) => {
                web_sys::console::error_1(&format!("BrowserEmbeddingService init: {e}").into());
                return Err(JsValue::from_str(&e));
            }
        };

    // JWT secret can't be loaded yet (variables table doesn't exist).
    // Construct the concrete `BrowserCryptoService` so we keep a typed Arc
    // for `set_jwt_secret` in Phase 3; the same Arc-coerced trait object
    // gets handed to the builder. Both Arcs point at the same allocation,
    // so the rotation is observed by every block via the existing service.
    let crypto_concrete = Arc::new(impresspress_browser::crypto::BrowserCryptoService::new(
        String::new(),
    ));
    let crypto_svc: Arc<dyn wafer_core::interfaces::crypto::service::CryptoService> =
        crypto_concrete.clone();

    let builder = ImpresspressBuilder::new()
        .database(impresspress_browser::make_database_service())
        .storage(impresspress_browser::make_storage_service())
        .config(config_svc.clone())
        .crypto(crypto_svc)
        .network(impresspress_browser::make_network_service())
        .logger(impresspress_browser::make_console_logger())
        .llm_service("browser", browser_llm)
        .image_service("browser", browser_image)
        .vector_service(browser_vector)
        .embedding_service(browser_embedding)
        .block_settings(initial_block_settings)
        .block_config(
            "wafer-run/security-headers",
            serde_json::json!({ "csp": IMPRESSPRESS_CSP }),
        )
        .config_source(cfg_source);
    let block_settings_handle = builder.block_settings_handle();
    // The router verifies JWTs against this secret. It's empty at build time
    // (the variables table doesn't exist yet), so grab the handle and rotate
    // it to the seeded value in `seed_after_admin_init`, alongside the crypto
    // service that signs with it.
    let jwt_secret_handle = builder.jwt_secret_handle();

    let (mut wafer, storage_block) = builder
        .build()
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    wafer.set_asset_loader(&impresspress_browser::make_sw_asset_loader());

    // ── Phase 2 ─────────────────────────────────────────────────────────────
    // Run the shared boot funnel: seal → init_block(admin) →
    // seed_after_admin_init → init_all_blocks → post_start.
    //
    // admin's `lifecycle(Init)` runs FIRST so its migrations create the
    // canonical `impresspress__admin__variables` + `block_settings` tables
    // before the seed hook reads them — admin's migration is the single source
    // of schema truth (the #210/#211 schema-drift lesson). The hook then seeds
    // + publishes into the services the wafer already holds (see
    // `BrowserBootHooks`), all over `BrowserDatabaseService` rather than the
    // old bridge raw-SQL strings.
    let hooks = BrowserBootHooks {
        db: impresspress_browser::make_database_service(),
        config_svc: config_svc.clone(),
        block_settings_handle: block_settings_handle.clone(),
        jwt_secret_handle: jwt_secret_handle.clone(),
        crypto: crypto_concrete.clone(),
    };
    builder::boot(&mut wafer, &storage_block, &hooks)
        .await
        .map_err(|e| JsValue::from_str(&format!("boot: {e}")))?;

    web_sys::console::log_1(&"impresspress: WAFER runtime started".into());

    impresspress_browser::store_wafer(wafer).map_err(|e| JsValue::from_str(&e.to_string()))?;

    Ok(())
}

/// [`BootHooks`](impresspress_core::builder::BootHooks) impl for the browser
/// target. After `init_block(admin)` has created the variables /
/// block_settings tables, this seeds them (auto-gen + JWT + browser-only
/// defaults) and the #222 block-settings hash-gate, then publishes the loaded
/// state into the services the wafer already holds:
///  - `config_svc` — the same `Arc<dyn ConfigService>` (mutated via `.set()`).
///  - `block_settings_handle` — the same `Arc<RwLock<BlockSettings>>` the
///    router's `FeatureConfig` reads, so the write is visible to the
///    subsequent `init_all_blocks()` and every later request.
///  - `crypto` — the concrete `BrowserCryptoService`, rotated to the real JWT
///    secret so any not-yet-initialised block signs/verifies with it.
///
/// `db` is a fresh `BrowserDatabaseService` handle; the service is a stateless
/// unit struct over global OPFS, so it points at the same database the wafer
/// uses.
struct BrowserBootHooks {
    db: Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
    config_svc: Arc<dyn ConfigService>,
    block_settings_handle: Arc<std::sync::RwLock<impresspress_core::features::BlockSettings>>,
    jwt_secret_handle: Arc<std::sync::RwLock<String>>,
    crypto: Arc<impresspress_browser::crypto::BrowserCryptoService>,
}

#[wafer_block::wafer_async_trait]
impl builder::BootHooks for BrowserBootHooks {
    async fn seed_after_admin_init(&self, _wafer: &wafer_run::Wafer) -> Result<(), String> {
        let vars = config::seed_and_load_variables(&self.db).await?;
        web_sys::console::log_1(
            &format!(
                "impresspress: {} variables loaded from database",
                vars.len()
            )
            .into(),
        );
        let features = config::load_block_settings(&self.db).await?;

        for (key, value) in &vars {
            self.config_svc.set(key, value);
        }
        self.config_svc.set(
            impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY,
            &features.to_config_json(),
        );
        *self
            .block_settings_handle
            .write()
            .expect("BlockSettings RwLock poisoned") = features;
        if let Some(secret) = vars.get(impresspress_core::blocks::auth::JWT_SECRET_KEY) {
            // Rotate BOTH holders of the secret to the seeded value: the crypto
            // service that SIGNS tokens and the router lock the pipeline VERIFIES
            // against. Rotating only the crypto service (the old bug) left the
            // router verifying with the empty build-time secret, so every
            // authenticated request 403'd after a successful login.
            self.crypto.set_jwt_secret(secret.clone());
            *self
                .jwt_secret_handle
                .write()
                .expect("jwt_secret handle RwLock poisoned") = secret.clone();
        }
        Ok(())
    }
}

#[wasm_bindgen]
pub async fn handle_request(request: web_sys::Request) -> Result<web_sys::Response, JsValue> {
    impresspress_browser::dispatch_request(request).await
}
