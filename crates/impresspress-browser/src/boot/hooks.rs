//! The browser's [`BootHooks`]: the seeding and publishing a runtime needs
//! once admin's migration has created the platform tables, plus the
//! consumer's own [`AppSeed`].

use std::{collections::HashMap, sync::Arc};

use impresspress_core::builder::{self, BootHooks};
use wafer_core::interfaces::{config::service::ConfigService, database::service::DatabaseService};

/// An app's own seeds: the values this app writes into the variables table on
/// every boot, which no block's declared default supplies.
///
/// It runs after `init_block(admin)` — so the `impresspress__admin__variables`
/// table exists — and before the variables are auto-generated and loaded
/// back, so whatever it writes is in the map [`BrowserBootHooks`] publishes
/// to the config source and to the blocks still to initialize.
///
/// Optional ([`BrowserBootHooks::with_app_seed`]): a runtime with nothing of
/// its own to seed boots without one. Use
/// `impresspress_core::platform_state::variables::{seed_if_absent, set}` —
/// `seed_if_absent` leaves a value a previous boot or an admin edit wrote,
/// `set` overrides it. An `Err` aborts the boot.
#[wafer_block::wafer_async_trait]
pub trait AppSeed {
    async fn seed(&self, db: &Arc<dyn DatabaseService>) -> Result<(), String>;
}

/// [`BootHooks`] for a browser runtime, made by
/// [`super::BrowserServices::build`] and run by [`super::boot`].
///
/// After `init_block(admin)` has created the variables / block_settings
/// tables, this runs the consumer's [`AppSeed`] (if any), auto-generates the
/// declared secrets (the JWT secret among them), loads the variables back and
/// hash-gate-seeds the block settings (#222), then publishes the loaded state
/// into the services the wafer already holds:
///  - `config_source` — the `SharedConfigSource` the runtime resolves every
///    block's declared `ConfigVar`s from at its first `lifecycle(Init)`.
///  - `config_svc` and the synchronous snapshot — only what is read
///    synchronously or runtime-owned (see `seed_after_admin_init`).
///  - `block_settings_handle` — the same `Arc<RwLock<BlockSettings>>` the
///    router's `FeatureConfig` reads, so the write is visible to the
///    remaining blocks' `Init` and every later request.
///  - `crypto` and `jwt_secret_handle` — both holders of the JWT secret,
///    rotated to the seeded value.
///
/// `db` is a fresh `BrowserDatabaseService` handle; the service is a stateless
/// unit struct over global OPFS, so it points at the same database the wafer
/// uses.
pub struct BrowserBootHooks {
    pub(super) db: Arc<dyn DatabaseService>,
    pub(super) config_svc: Arc<dyn ConfigService>,
    pub(super) config_source: Arc<impresspress_core::config_source::SharedConfigSource>,
    pub(super) block_settings_handle:
        Arc<std::sync::RwLock<impresspress_core::features::BlockSettings>>,
    pub(super) jwt_secret_handle: Arc<std::sync::RwLock<String>>,
    pub(super) crypto: Arc<crate::crypto::BrowserCryptoService>,
    /// Comma-joined, published as
    /// `impresspress_core::ui::PAGE_ENGINE_SCRIPTS_CONFIG_KEY`.
    pub(super) page_engine_scripts: String,
    pub(super) app_seed: Option<Box<dyn AppSeed>>,
}

impl BrowserBootHooks {
    /// Run `seed` as this app's [`AppSeed`].
    pub fn with_app_seed(mut self, seed: impl AppSeed + 'static) -> Self {
        self.app_seed = Some(Box::new(seed));
        self
    }

    /// The app's seeds, then the shared ones: auto-generate the declared
    /// secrets (incl. the auth JWT secret) and load the full set back — the
    /// core path every target runs, over `BrowserDatabaseService`. There are
    /// no process env vars in the browser, hence `&[]`.
    async fn seed_and_load_variables(&self) -> Result<HashMap<String, String>, String> {
        if let Some(seed) = &self.app_seed {
            seed.seed(&self.db).await?;
        }
        impresspress_core::platform_state::variables::seed_and_load(&self.db, &[]).await
    }

    /// Load + hash-gate-seed block settings, the exact #222 gate Cloudflare
    /// and native run.
    ///
    /// A read error is always a genuine operational failure (OPFS/sql.js
    /// corruption, quota exhaustion) — never the missing-table cold-start
    /// case, which `DatabaseService::list` already tolerates internally.
    /// Propagates rather than fabricating "every block enabled".
    async fn load_block_settings(
        &self,
    ) -> Result<impresspress_core::features::BlockSettings, String> {
        impresspress_core::platform_state::block_settings::load_and_seed(
            &self.db,
            &impresspress_core::blocks::block_enabled_defaults(),
        )
        .await
        .map_err(|e| format!("load block settings: {e}"))
    }
}

#[wafer_block::wafer_async_trait]
impl BootHooks for BrowserBootHooks {
    async fn seed_after_admin_init(&self, wafer: &mut wafer_run::Wafer) -> Result<(), String> {
        let vars = self.seed_and_load_variables().await?;
        web_sys::console::log_1(
            &format!(
                "impresspress: {} variables loaded from database",
                vars.len()
            )
            .into(),
        );
        let features = self.load_block_settings().await?;

        // The runtime resolves every block's DECLARED `ConfigVar`s through the
        // config source before it calls that block's `lifecycle(Init)` — a
        // required key it cannot resolve is `InitError::Permanent`, and the
        // block is then dead for the life of the runtime however well its
        // handlers would have coped with the value being absent. So the
        // seeded map goes here as well as onto the two config surfaces: this
        // is the half the remaining blocks' Init actually consults.
        self.config_source.publish(vars.clone());

        // Both config surfaces, published together — but only the values
        // something has to read SYNCHRONOUSLY or that the runtime owns, not a
        // copy of the variables table. The config block serves every stored
        // variable from the table itself (`impresspress_core::blocks::config`),
        // and now learns of this seeding through the config-write generation
        // `variables::insert_if_absent` bumps. A table copy here would only put
        // admin-editable keys onto the boot-frozen `ctx.config_get` snapshot,
        // where the first synchronous reader of one silently gets a stale
        // value — the defect class the config-store work removed.
        let mut published = builder::RuntimeConfig::new();
        published
            // This adapter executes inside an end user's browser. The config
            // block treats `__…__` keys as runtime-owned and never serves them
            // from the table, so a database/admin value cannot enable Stripe
            // secret-key operations locally. Static pages may still use a
            // remote trusted commerce API or pre-created Payment Links.
            .both(
                impresspress_core::runtime_kind::RUNTIME_KIND_CONFIG_KEY,
                "browser",
            )
            .both(
                impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY,
                features.to_config_json(),
            )
            // The pages the runtime renders load the engines its services
            // post to: the ones the bundle shipped (`initialize({ pageEngines })`).
            .both(
                impresspress_core::ui::PAGE_ENGINE_SCRIPTS_CONFIG_KEY,
                self.page_engine_scripts.clone(),
            );
        // `csrf` and `auth::service` read the secret per request off the
        // synchronous snapshot; seeding just generated it if it was absent.
        if let Some(secret) = vars.get(impresspress_core::blocks::auth::JWT_SECRET_KEY) {
            published.both(
                impresspress_core::blocks::auth::JWT_SECRET_KEY,
                secret.clone(),
            );
        }
        published.republish(wafer, &self.config_svc);

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
