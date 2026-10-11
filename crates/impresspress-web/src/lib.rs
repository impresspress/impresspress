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
// The SMART POINTER is forced at every site the lint reaches — that is the
// claim this allow rests on, and it is narrower than "the code is all
// API-shaped". There are two in a site build and four in a
// `browser-devtools` one:
//
// * `BrowserRuntimeControl::new` (`site_runtime`), whose `Arc` is what
//   `DevShared::new` takes as `Arc<dyn RuntimeControl>` (and, in a devtools
//   build, `Arc<dyn WorkspaceControl>`);
// * `Context::clone_arc` for `BootContext` (`site_runtime`) and, in a
//   devtools build, for `DenyAllContext` (`dev_runtime`), which returns
//   `Arc<dyn Context>`;
// * in a devtools build, the `DevBlock` `runtime_factory` hands
//   `ImpresspressBuilder::extra_block` as `Arc<dyn Block>` — the workspace
//   block holds the `DevShared`, and through it the control. (A site
//   build's `DevBlock` holds nothing and is genuinely `Send + Sync`.)
//
// Each API takes or returns `Arc<dyn _>` by value, and `Rc` does not coerce
// into an `Arc<dyn Trait>` (E0605). On this single-threaded target none of
// them is making a cross-thread claim to be wrong about.
//
// Scoped to wasm32 even though this crate only ships to a browser, because
// that is the actual precondition: on a native target the same bounds resolve
// to real `Send + Sync`, the lint is accurate again, and this allow must not
// silence it. Scoped to `browser-site-runtime` because every site is in the
// sandbox's code: a build without it has none, and an `expect` with nothing
// to catch is itself an error.
#![cfg_attr(
    all(target_arch = "wasm32", feature = "browser-site-runtime"),
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
// Both sandbox modules document themselves (`//!` in each file). Deliberately
// no `///` here: rustdoc merges an outer doc comment on the `mod` item with
// the module's own inner docs and then resolves the whole block in *this*
// scope, so every intra-doc link the module makes to its own items goes
// unresolved.
#[cfg(feature = "browser-devtools")]
pub mod dev_runtime;
pub mod runtime_factory;
#[cfg(feature = "browser-site-runtime")]
pub mod site_runtime;

pub use runtime_factory::{CompiledSandbox, RuntimeFactory, RuntimeOptions, SandboxMode};

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
    "connect-src 'self' https://cdn.jsdelivr.net https://huggingface.co ",
        "https://raw.githubusercontent.com https://*.huggingface.co https://*.hf.co https://*.xethub.hf.co; ",
    "base-uri 'self'; ",
    "form-action 'self'",
);

/// Boot the runtime inside the Service Worker.
///
/// `options` is the object `sw.js` passes:
/// `{ sandbox: 'none' | 'site' | 'workspace', bypass: <rules>, pageEngines: <paths> }`,
/// rendered from the bundle's `__SANDBOX__`, `__BYPASS_RULES__` and
/// `__PAGE_ENGINES__` placeholders (`impresspress-bundle`'s `SandboxShell`
/// spells the mode). Only `'workspace'` asks for the workspace; any other
/// value — including a missing one, or one that is none of the three, which
/// gets a console warning — does not: the workspace is never enabled by an
/// unparseable value. `bypass` is the paths this worker leaves to the static
/// host (see `bypass_rules_option`).
///
/// The mode is a *request*, and what it can select is the WORKSPACE half of
/// the sandbox only, and only on a build that compiled one (see
/// [`SandboxMode`]). A build with `browser-site-runtime` compiled in — a
/// site build, or a `browser-devtools` build, which implies it — always runs
/// the sandbox's RUNTIME half: the seed import, the generation ledger,
/// journal convergence, the dynamic-block rebuild. That is what makes an
/// ImpressPress folder serve the site it ships, and an EXPORTED bundle boots
/// with `{ sandbox: 'site' }` precisely so it has no `/b/dev`. Where the
/// build has no workspace — a site build ([`SandboxMode::Exported`]) or no
/// sandbox at all ([`SandboxMode::Absent`]) — `{ sandbox: 'workspace' }` is
/// a no-op apart from the single console warning below: same seeded
/// variables, same CSP, same routes as any other request. So is
/// `{ sandbox: 'site' }` on a build with no sandbox at all, with a warning of
/// its own.
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

    // Work a handler defers joins its own request's after-response queue,
    // which `handle_request` hands the service worker to keep alive with
    // `event.waitUntil`. The default, a bare `spawn_local`, is tied to no
    // event, so nothing kept the worker alive for it. See
    // `impresspress_core::deferred`.
    impresspress_core::deferred::set_mode(impresspress_core::deferred::DeferMode::Queued);

    let sandbox = js_sys::Reflect::get(&options, &JsValue::from_str("sandbox"))
        .ok()
        .and_then(|v| v.as_string());
    let workspace_requested = sandbox.as_deref() == Some("workspace");

    // A value that is none of the three words is a `sw.js` this runtime does
    // not understand. It boots without the workspace — the rule above — but
    // says so, rather than leaving an operator to wonder why `/b/dev` 404s.
    if !matches!(sandbox.as_deref(), Some("none" | "site" | "workspace")) {
        web_sys::console::warn_1(
            &format!(
                "impresspress: initialize() was handed sandbox {sandbox:?}, which is none of \
                 'none', 'site' and 'workspace'; booting without the workspace"
            )
            .into(),
        );
    }

    // A bundle can ask for the workspace on a build that never compiled it.
    // That is accepted rather than fatal — the workspace cannot exist, so
    // there is nothing to disable — but it is the difference between "the
    // workspace is off" and "the workspace is missing", so say so once rather
    // than leaving an operator to wonder why `/b/dev` 404s.
    if workspace_requested {
        let missing = match CompiledSandbox::COMPILED {
            CompiledSandbox::None => Some(
                "a build without the sandbox (neither `browser-site-runtime` nor \
                 `browser-devtools`) — the sandbox is not compiled in",
            ),
            CompiledSandbox::Site => Some(
                "a site build (`browser-site-runtime` without `browser-devtools`) — \
                 the site runtime runs, but the workspace is not compiled in",
            ),
            CompiledSandbox::Devtools => None,
        };
        if let Some(missing) = missing {
            web_sys::console::warn_1(
                &format!(
                    "impresspress: initialize({{ sandbox: 'workspace' }}) on {missing}, and \
                     /b/dev will not exist"
                )
                .into(),
            );
        }
    }
    // The same for the runtime half: a site asked for on a build with no
    // sandbox at all has nothing to import its `seed/` with.
    if sandbox.as_deref() == Some("site") && CompiledSandbox::COMPILED == CompiledSandbox::None {
        web_sys::console::warn_1(
            &"impresspress: initialize({ sandbox: 'site' }) on a build without the sandbox \
              (neither `browser-site-runtime` nor `browser-devtools`) — the site runtime is \
              not compiled in, and the bundle's seed/ will not be imported"
                .into(),
        );
    }

    impresspress_browser::db_init().await?;

    let factory = RuntimeFactory::new(RuntimeOptions {
        workspace_requested,
        page_engine_scripts: page_engines_option(&options),
    })
    .map_err(|e| JsValue::from_str(&e))?;

    // The sandbox control plane is attached BEFORE the first build, so the
    // cold-start runtime already carries the dev block — whose `Init` creates
    // the ledger the seed import writes to — and, in a workspace, `/b/dev`.
    // Doing it afterwards would mean neither existed until a rebuild — and an
    // instance with no guest blocks has no reason to rebuild, so on the common
    // path they would never exist at all. Without `browser-site-runtime`
    // there is no `attach` at all and `factory` is used as constructed.
    #[cfg(feature = "browser-site-runtime")]
    let (factory, sandbox) = site_runtime::attach(factory, bypass_rules_option(&options));

    let wafer = factory.build(&[]).await?;

    web_sys::console::log_1(&"impresspress: WAFER runtime started".into());

    impresspress_browser::store_wafer(wafer).map_err(|e| JsValue::from_str(&e.to_string()))?;

    // Seed on a fresh instance, converge on whatever the activation journal
    // was in the middle of, and rebuild with the active block set — all before
    // returning, because requests only start once `initialize()` resolves.
    //
    // This runs for an EXPORTED bundle too — `{ sandbox: 'site' }` on a
    // `browser-devtools` build, or any request on a site build. It is the
    // whole reason the export works: the archive ships a `seed/` beside the
    // shell, and this is what reads it. See `SandboxMode`.
    #[cfg(feature = "browser-site-runtime")]
    site_runtime::install(&sandbox).await;

    Ok(())
}

/// The `bypass` option of `initialize()`: the rules the running worker's fetch
/// handler applies, rendered by `impresspress-bundle` from the same value as
/// its condition (`BYPASS_RULES` in `sw.js.tmpl`). The sandbox's runtime
/// keeps them and refuses a site file at a path they list — the seed import
/// on every sandbox build, and the workspace's site-file writes and export on a
/// `browser-devtools` one — exactly this worker's rules, with no fetch and
/// nothing that can fail later.
///
/// Missing or unparseable — a `sw.js` rendered before the option existed —
/// is NO rules, with a console warning: that worker still bypasses what it
/// always did, the sandbox just cannot see which paths those are, so it
/// refuses none, as before. Refusing to boot instead would turn a missing
/// check into a dead deployment.
#[cfg(feature = "browser-site-runtime")]
fn bypass_rules_option(options: &JsValue) -> impresspress_core::blocks::dev::BypassRules {
    let value =
        js_sys::Reflect::get(options, &JsValue::from_str("bypass")).unwrap_or(JsValue::UNDEFINED);
    let parsed = if value.is_undefined() || value.is_null() {
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
                "impresspress: no service-worker bypass rules ({reason}); the sandbox \
                 will not refuse site files this worker serves from the static host — \
                 rebuild the bundle with a current impresspress-bundle"
            )
            .into(),
        );
        impresspress_core::blocks::dev::BypassRules::default()
    })
}

/// The `pageEngines` option of `initialize()`: the page-side model engines the
/// bundle ships, as URL paths, rendered by `impresspress-bundle` from its one
/// list (`PAGE_ENGINES` in `sw.js.tmpl`). The runtime's pages load exactly
/// these, so the runtime publishes what the bundle actually shipped rather
/// than a list of its own.
///
/// Missing or malformed — a `sw.js` rendered before the option existed — is
/// NO engines, with a console warning: the pages then carry no engine
/// scripts, and `bridge.js` refuses each model request at once ("no open page
/// runs the … engine") rather than the runtime failing to boot.
fn page_engines_option(options: &JsValue) -> Vec<String> {
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
    /// The bundle's page engines, comma-joined
    /// ([`RuntimeOptions::page_engine_scripts`]), published as
    /// `impresspress_core::ui::PAGE_ENGINE_SCRIPTS_CONFIG_KEY`.
    page_engine_scripts: String,
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

/// Answer one fetch event: resolves to `{ response, after }`, where
/// `response` is the `Response` to hand to `respondWith` and `after` is a
/// promise for the work the request left to run after it (its request-log
/// row and the tasks its handlers deferred). The service worker must pass
/// `after` to `event.waitUntil`, or the worker can be stopped before that
/// work has run. See `impresspress_browser::dispatch_request`.
#[wasm_bindgen]
pub async fn handle_request(request: web_sys::Request) -> Result<JsValue, JsValue> {
    impresspress_browser::dispatch_fetch(request).await
}
