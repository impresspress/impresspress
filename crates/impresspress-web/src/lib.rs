//! Impresspress app compiled to WASM for running in the browser via Service Worker.
//!
//! Thin wasm-bindgen wrapper around the `impresspress-browser` framework. Uses
//! `ImpresspressBuilder` (from `impresspress-core`) to wire up the full Impresspress
//! block suite + the app-specific `BrowserLlmService`.

// `clippy::arc_with_non_send_sync` is stated once here, crate-wide and
// target-scoped, rather than repeated at every `Arc::new`.
//
// wafer-run's service and block traits are bounded on
// `wafer_block::compat::{MaybeSend, MaybeSync}`. Those are `Send`/`Sync` on
// native, and on wasm32 they are *unbounded* blanket markers
// (`impl<T: ?Sized> MaybeSend for T`) — which is precisely what lets this
// crate hand `Cell`/`RefCell`-backed values (`BrowserRuntimeControl` and the
// dev `Context` impls) across those trait boundaries with no
// `unsafe impl Send`/`Sync`.
//
// The SMART POINTER is forced at the four sites the lint reaches — that is the
// claim this allow rests on, and it is narrower than "the code is all
// API-shaped". `ImpresspressBuilder::extra_block`, `DevShared::new` and
// `Context::clone_arc` take or return `Arc<dyn _>` by value, and `Rc` does not
// coerce into an `Arc<dyn Trait>` (E0605). On this single-threaded target none
// of the four is making a cross-thread claim to be wrong about.
//
// Scoped to wasm32 even though this crate only ships to a browser, because
// that is the actual precondition: on a native target the same bounds resolve
// to real `Send + Sync`, the lint is accurate again, and this allow must not
// silence it.
#![cfg_attr(
    target_arch = "wasm32",
    expect(
        clippy::arc_with_non_send_sync,
        reason = "on this single-threaded target the `Arc` is forced by the \
                  trait-object bounds, not chosen over an `Rc`"
    )
)]

use std::sync::Arc;

use impresspress_core::builder;
use wafer_core::interfaces::config::service::ConfigService;
use wasm_bindgen::prelude::*;

pub mod config;
// The module documents itself (`//!` in `dev_runtime.rs`). Deliberately no
// `///` here: rustdoc merges an outer doc comment on the `mod` item with the
// module's own inner docs and then resolves the whole block in *this* scope,
// so every intra-doc link the module makes to its own items goes unresolved.
#[cfg(feature = "browser-devtools")]
pub mod dev_runtime;
pub mod runtime_factory;

pub use runtime_factory::{RuntimeFactory, RuntimeOptions, SandboxMode};

/// The operator-level `csp` the browser runtime hands `wafer-run/security-headers`,
/// which merges it directive by directive over its own baseline and refuses
/// anything that would weaken it (`'unsafe-eval'`, a `frame-ancestors`
/// directive — that one is the block's `frame_ancestors` key — or a repeated
/// directive), so nothing here may be one of those.
const IMPRESSPRESS_CSP: &str = concat!(
    "default-src 'self'; ",
    "script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval' https://cdn.jsdelivr.net; ",
    "style-src 'self' 'unsafe-inline'; ",
    "img-src 'self' data: blob: https:; ",
    "font-src 'self' https:; ",
    "connect-src 'self' https://cdn.jsdelivr.net https://esm.run https://huggingface.co ",
        "https://raw.githubusercontent.com https://*.huggingface.co https://*.hf.co https://*.xethub.hf.co; ",
    "base-uri 'self'; ",
    "form-action 'self'",
);

/// Boot the runtime inside the Service Worker.
///
/// `options` is the object `sw.js` passes: `{ dev: <bool> }`, rendered from
/// the bundle's `__DEV_ENABLED__` placeholder. A missing or non-boolean `dev`
/// reads as `false` — the sandbox is never enabled by an unparseable value.
///
/// The flag is a *request*, and it selects the WORKSPACE half of the sandbox
/// only (see [`SandboxMode`]). A build with `browser-devtools` compiled in
/// always runs the sandbox's RUNTIME half — the seed import, the generation
/// ledger, journal convergence, the dynamic-block rebuild — because that is
/// what makes an ImpressPress folder serve the site it ships, and an EXPORTED
/// bundle boots with `{ dev: false }` precisely so it has no `/b/dev`. On a
/// build without the feature the flag resolves to [`SandboxMode::Absent`] and
/// `{ dev: true }` is a no-op apart from the single console warning below:
/// same seeded variables, same CSP, same routes as `{ dev: false }`.
#[wasm_bindgen]
pub async fn initialize(options: JsValue) -> Result<(), JsValue> {
    // Before anything that can log. `tracing`'s default dispatcher discards
    // every event, so until this is installed each `warn!`/`error!` in
    // `impresspress-core`, `wafer-run` and the browser adapter goes nowhere —
    // and a framework failure the code deliberately survives (a malformed LLM
    // chunk, a refused subrequest) leaves a console with nothing in it.
    // Idempotent, and a `false` here only means someone installed a subscriber
    // first, which is their prerogative.
    let _ = impresspress_browser::init_console_tracing();

    if impresspress_browser::is_initialized() {
        return Ok(());
    }

    let dev_requested = js_sys::Reflect::get(&options, &JsValue::from_str("dev"))
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // A bundle can ask for the sandbox on a build that never compiled it. That
    // is accepted rather than fatal — the block cannot exist, so there is
    // nothing to disable — but it is the difference between "the sandbox is
    // off" and "the sandbox is missing", so say so once rather than leaving an
    // operator to wonder why `/b/dev` 404s.
    #[cfg(not(feature = "browser-devtools"))]
    if dev_requested {
        web_sys::console::warn_1(
            &"impresspress: initialize({ dev: true }) on a build without the \
              `browser-devtools` feature — the sandbox is not compiled in and \
              /b/dev will not exist"
                .into(),
        );
    }

    impresspress_browser::db_init().await?;

    let factory = RuntimeFactory::new(RuntimeOptions {
        dev_enabled: dev_requested,
    })
    .map_err(|e| JsValue::from_str(&e))?;

    // The sandbox control plane is attached BEFORE the first build, so the
    // cold-start runtime already carries the dev block (and, in a workspace
    // build, `/b/dev`). Doing it afterwards would mean the page did not exist
    // until a rebuild — and an instance with no guest blocks has no reason to
    // rebuild, so on the common path it would never exist at all. `attach`
    // answers `None` only when the feature is not compiled in, and the factory
    // is then untouched. Without the feature there is no `attach` at all and
    // `factory` is used as constructed.
    #[cfg(feature = "browser-devtools")]
    let (factory, sandbox) = dev_runtime::attach(factory);

    let wafer = factory.build(&[]).await?;

    web_sys::console::log_1(&"impresspress: WAFER runtime started".into());

    impresspress_browser::store_wafer(wafer).map_err(|e| JsValue::from_str(&e.to_string()))?;

    // Seed on a fresh instance, converge on whatever the activation journal
    // was in the middle of, and rebuild with the active block set — all before
    // returning, because requests only start once `initialize()` resolves.
    //
    // This runs for an EXPORTED bundle too (`{ dev: false }` with the feature
    // compiled in). It is the whole reason the export works: the archive ships
    // a `seed/` beside the shell, and this is what reads it. See
    // `SandboxMode`.
    #[cfg(feature = "browser-devtools")]
    if let Some(sandbox) = &sandbox {
        dev_runtime::install(sandbox).await;
    }

    Ok(())
}

/// [`BootHooks`](impresspress_core::builder::BootHooks) impl for the browser
/// target. After `init_block(admin)` has created the variables /
/// block_settings tables, this seeds them (auto-gen + JWT + browser-only
/// defaults) and the #222 block-settings hash-gate, then publishes the loaded
/// state into the services the wafer already holds:
///  - `config_svc` — the same `Arc<dyn ConfigService>` (mutated via `.set()`).
///  - `config_source` — the `SharedConfigSource` the runtime resolves every
///    block's declared `ConfigVar`s from at its first `lifecycle(Init)`.
///  - `block_settings_handle` — the same `Arc<RwLock<BlockSettings>>` the
///    router's `FeatureConfig` reads, so the write is visible to the
///    remaining blocks' `Init` and every later request.
///  - `crypto` — the concrete `BrowserCryptoService`, rotated to the real JWT
///    secret so any not-yet-initialised block signs/verifies with it.
///
/// `db` is a fresh `BrowserDatabaseService` handle; the service is a stateless
/// unit struct over global OPFS, so it points at the same database the wafer
/// uses.
struct BrowserBootHooks {
    db: Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
    config_svc: Arc<dyn ConfigService>,
    /// The runtime's per-block config source, empty until this hook fills it.
    config_source: Arc<impresspress_core::config_source::SharedConfigSource>,
    block_settings_handle: Arc<std::sync::RwLock<impresspress_core::features::BlockSettings>>,
    jwt_secret_handle: Arc<std::sync::RwLock<String>>,
    crypto: Arc<impresspress_browser::crypto::BrowserCryptoService>,
    /// What the sandbox contributes to this runtime. Decides which of the
    /// sandbox's own variables are seeded — see
    /// `config::seed_and_load_variables`.
    mode: SandboxMode,
}

#[wafer_block::wafer_async_trait]
impl builder::BootHooks for BrowserBootHooks {
    async fn seed_after_admin_init(&self, wafer: &mut wafer_run::Wafer) -> Result<(), String> {
        let vars = config::seed_and_load_variables(&self.db, self.mode).await?;
        web_sys::console::log_1(
            &format!(
                "impresspress: {} variables loaded from database",
                vars.len()
            )
            .into(),
        );
        let features = config::load_block_settings(&self.db).await?;

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
                impresspress_core::blocks::products::RUNTIME_KIND_CONFIG_KEY,
                "browser",
            )
            .both(
                impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY,
                features.to_config_json(),
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

#[wasm_bindgen]
pub async fn handle_request(request: web_sys::Request) -> Result<web_sys::Response, JsValue> {
    impresspress_browser::dispatch_request(request).await
}
