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

pub use runtime_factory::RuntimeFactory;

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
/// `{ exportShell: <root> | null, bypass: <rules>, pageEngines: <paths> }`,
/// rendered from the bundle's `__EXPORT_SHELL__`, `__BYPASS_RULES__` and
/// `__PAGE_ENGINES__` placeholders. `exportShell` roots the export shell a
/// workspace's export reads (see `export_shell_option`). `bypass` is the
/// paths this worker leaves to the static host (see `bypass_rules_option`).
/// `pageEngines` is the model engines the runtime's pages load (see
/// [`page_engines_option`]).
///
/// The boot itself is `impresspress-browser`'s funnel
/// ([`impresspress_browser::boot`]); this crate adds its own blocks, routes,
/// headers and seeds at the seams it leaves ([`RuntimeFactory::build`]).
///
/// There is no mode option: what the sandbox contributes is
/// [`SandboxMode::COMPILED`], decided by the cargo features (this crate's
/// forward to `impresspress-core`'s, which the constant reads), and `sw.js`
/// has already checked that it is the mode the bundle was built for
/// ([`compiled_sandbox_mode`], which this cdylib exports by linking
/// `impresspress-browser`). A build with `browser-site-runtime` compiled
/// in — a site build, or a `browser-devtools` build, which implies it —
/// always runs the sandbox's RUNTIME half: the seed import, the generation
/// ledger, journal convergence, the dynamic-block rebuild. That is what makes
/// an ImpressPress folder serve the site it ships. An EXPORTED bundle is the
/// deployment's export shell: a site build, with no `/b/dev` compiled in at
/// all. A `browser-devtools` build is always the workspace.
///
/// [`SandboxMode::COMPILED`]: impresspress_core::sandbox_mode::SandboxMode::COMPILED
/// [`compiled_sandbox_mode`]: impresspress_browser::sandbox_mode::compiled_sandbox_mode
/// [`page_engines_option`]: impresspress_browser::boot::page_engines_option
#[wasm_bindgen]
pub async fn initialize(options: JsValue) -> Result<(), JsValue> {
    // Before anything that can log, and before the re-entry check: the
    // console tracing subscriber and the queued deferral mode. See
    // `impresspress_browser::boot::prepare_worker`.
    impresspress_browser::boot::prepare_worker();

    if impresspress_browser::is_initialized() {
        return Ok(());
    }

    // A workspace whose deployment carries no export shell still boots — the
    // shell is what its export ships, not what the workspace runs on — but
    // says so once, the way the other missing options do. Its export then
    // refuses, naming the missing shell; it never ships the sandbox's own.
    #[cfg(feature = "browser-devtools")]
    let export_shell = export_shell_option(&options);
    #[cfg(feature = "browser-devtools")]
    if let Err(reason) = &export_shell {
        web_sys::console::warn_1(
            &format!(
                "impresspress: this workspace's deployment carries no export shell \
                 ({reason}), so its export will refuse — rebuild the deployment with \
                 examples/dev-sandbox/build.sh, which builds one ([sandbox] export_shell)"
            )
            .into(),
        );
    }

    // The error passes through as it is: `sw.js` reads its `name` to tell a
    // database that could not be read (`DatabaseUnreadable`, its stage
    // `open`) from one that was read and failed (`initialize`). The same
    // holds for `factory.build` below: a migration the runtime refused is an
    // `Error` named `MigrationRefused` (its stage `migration`).
    impresspress_browser::db_init().await?;

    let factory = RuntimeFactory::new(
        impresspress_browser::boot::BrowserServices::new(
            impresspress_browser::boot::page_engines_option(&options),
        )
        .map_err(|e| JsValue::from_str(&e))?,
    );

    // The sandbox control plane is attached BEFORE the first build, so the
    // cold-start runtime already carries the dev block — whose `Init` creates
    // the ledger the seed import writes to — and, in a workspace, `/b/dev`.
    // Doing it afterwards would mean neither existed until a rebuild — and an
    // instance with no guest blocks has no reason to rebuild, so on the common
    // path they would never exist at all. Without `browser-site-runtime`
    // there is no `attach` at all and `factory` is used as constructed.
    #[cfg(feature = "browser-site-runtime")]
    let (factory, sandbox) = site_runtime::attach(
        factory,
        site_runtime::SandboxOptions {
            bypass: bypass_rules_option(&options),
            #[cfg(feature = "browser-devtools")]
            export_shell: export_shell.ok(),
        },
    );

    let wafer = factory.build(&[]).await?;

    web_sys::console::log_1(&"impresspress: WAFER runtime started".into());

    impresspress_browser::store_wafer(wafer).map_err(|e| JsValue::from_str(&e.to_string()))?;

    // Seed on a fresh instance, converge on whatever the activation journal
    // was in the middle of, and rebuild with the active block set — all before
    // returning, because requests only start once `initialize()` resolves.
    //
    // This runs for an EXPORTED bundle too — the export shell, a site build.
    // It is the whole reason the export works: the archive ships a `seed/`
    // beside the shell, and this is what reads it. See
    // `impresspress_core::sandbox_mode::SandboxMode`.
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

/// The `exportShell` option of `initialize()`: the root path at which this
/// deployment carries its export shell — the bundle built as a site, which
/// the workspace's export ships — rendered by `impresspress-bundle` from its
/// one `EXPORT_SHELL_ROOT` (`EXPORT_SHELL` in `sw.js.tmpl`), or `null` for a
/// bundle built without one (`[sandbox] export_shell`).
///
/// A root is a path that starts and ends with `/`; it roots the
/// `BrowserShellSource` the export reads. Anything else — missing, `null`,
/// or a value that is not such a path — is the reason there is none, for the
/// caller's warning, and the export refuses.
#[cfg(feature = "browser-devtools")]
fn export_shell_option(options: &JsValue) -> Result<String, String> {
    let value = js_sys::Reflect::get(options, &JsValue::from_str("exportShell"))
        .unwrap_or(JsValue::UNDEFINED);
    if value.is_undefined() {
        return Err("initialize() was not handed the option".to_string());
    }
    if value.is_null() {
        return Err("the bundle was built without [sandbox] export_shell".to_string());
    }
    match value.as_string() {
        Some(root) if root.starts_with('/') && root.ends_with('/') => Ok(root),
        _ => Err(format!(
            "the option is {}, not a root path",
            describe_option(&value)
        )),
    }
}

/// `value` as a console message can show it.
#[cfg(feature = "browser-devtools")]
fn describe_option(value: &JsValue) -> String {
    js_sys::JSON::stringify(value)
        .ok()
        .and_then(|json| json.as_string())
        .unwrap_or_else(|| format!("{value:?}"))
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

/// `initialize()` against an in-memory OPFS whose database file holds data
/// and cannot be read this time.
#[cfg(all(test, target_arch = "wasm32"))]
mod initialize_tests {
    use impresspress_browser::database::test_support::{fail_next_opfs_read, fresh_db};
    use wafer_core::interfaces::database::service::DatabaseService;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    /// **The worker sees the open failure by name.** `sw.js` reports an
    /// `initialize()` rejection whose `name` is `DatabaseUnreadable` as its
    /// non-erasing stage `open`, so the rejection must reach it as the error
    /// `dbInit` made, not re-wrapped or turned into text on the way.
    #[wasm_bindgen_test]
    async fn an_unreadable_database_rejects_initialize_by_name() {
        let db = fresh_db().await;
        db.exec_raw("CREATE TABLE initialize_unreadable_t (id TEXT)", &[])
            .await
            .expect("a file with data in it");

        fail_next_opfs_read("NotReadableError");
        let err = initialize(JsValue::UNDEFINED)
            .await
            .expect_err("an unreadable database fails initialize()");

        let name = js_sys::Reflect::get(&err, &"name".into()).expect("an Error");
        assert_eq!(name.as_string().as_deref(), Some("DatabaseUnreadable"));
        assert!(!impresspress_browser::is_initialized());
    }

    /// **The worker sees a refused migration by name.** A database whose
    /// migration ledger records an admin file this bundle does not ship — one
    /// a newer bundle migrated — is refused, and `sw.js` reports an
    /// `initialize()` rejection named `MigrationRefused` as its non-erasing
    /// stage `migration`; so the rejection must reach it as that error, not
    /// re-wrapped or turned into text, and the database must be left as it
    /// was.
    #[wasm_bindgen_test]
    async fn a_refused_migration_rejects_initialize_by_name() {
        use impresspress_core::{
            blocks::admin::ADMIN_BLOCK_ID,
            migration_helper::backend_named,
            platform_state::applied_migrations::{self, AppliedMigration, Origin},
        };

        let db: std::sync::Arc<dyn DatabaseService> = std::sync::Arc::new(fresh_db().await);
        let sqlite = backend_named("sqlite");
        db.exec_raw(
            &applied_migrations::create_table_sql(sqlite).expect("the ledger's schema"),
            &[],
        )
        .await
        .expect("create the ledger");
        let newer = AppliedMigration::new(
            ADMIN_BLOCK_ID,
            "999_from_a_newer_bundle",
            "a-newer-bundles-hash",
            Origin::Ran,
        );
        db.exec_raw_transaction(&[applied_migrations::insert_statement(&newer, sqlite)])
            .await
            .expect("record the newer bundle's file");

        let err = initialize(JsValue::UNDEFINED)
            .await
            .expect_err("a database ahead of the bundle fails initialize()");

        let name = js_sys::Reflect::get(&err, &"name".into()).expect("an Error");
        assert_eq!(
            name.as_string().as_deref(),
            Some(impresspress_browser::boot::MIGRATION_REFUSED)
        );
        assert!(!impresspress_browser::is_initialized());
        let rows = applied_migrations::list_for_block_via_service(&db, ADMIN_BLOCK_ID)
            .await
            .expect("read the ledger");
        assert_eq!(
            rows,
            vec![newer],
            "the refusal ran nothing and recorded nothing"
        );
    }
}
