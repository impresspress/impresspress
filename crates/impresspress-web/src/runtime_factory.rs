//! Builds a booted [`wafer_run::Wafer`] from the browser platform services.
//!
//! Cold start builds once with no dynamic blocks; an activation that changes
//! the block set builds again with the new set and swaps the runtime (see
//! `dev_runtime`, Task 9). Everything `initialize()` used to do between
//! `db_init()` and `store_wafer()` lives here, parameterised by the dynamic
//! block set — so the second build is the *same* build, not a hand-kept copy
//! of it.
//!
//! The browser services are constructed once, in [`RuntimeFactory::new`], and
//! shared by every runtime the factory builds. That is what keeps a rebuild
//! cheap and, more importantly, what keeps the seeded JWT secret and the
//! loaded config alive across a swap: the crypto service and `ConfigService`
//! the new runtime gets are the same allocations the old one held.

use std::sync::Arc;

#[cfg(feature = "browser-devtools")]
use impresspress_core::blocks::dev::{DevShared, DynamicBlockSpec};
use impresspress_core::builder::{self, ImpresspressBuilder};
use wafer_core::interfaces::{
    config::service::ConfigService,
    crypto::service::CryptoService,
    image::service::ImageService,
    llm::service::LlmService,
    vector::service::{EmbeddingService, VectorService},
};
use wasm_bindgen::prelude::*;

/// One dynamically-registered guest: its manifest entry plus the already-loaded
/// block. Loading (wasmi instantiation over the stored artifact) is the
/// caller's job — the factory only registers what it is handed, so a guest that
/// fails to load never reaches a `Wafer` at all.
#[cfg(feature = "browser-devtools")]
pub type DynamicBlock = (DynamicBlockSpec, Arc<dyn wafer_run::Block>);

/// Placeholder element type for builds without `browser-devtools`.
///
/// Uninhabited on purpose: without the feature the sandbox cannot exist, so the
/// only slice that can be constructed is the empty one. `build(&[])` therefore
/// compiles unchanged in both configurations without the call site growing a
/// `#[cfg]`, and no "the feature is off but we still handled a dynamic block"
/// path can be written by accident.
#[cfg(not(feature = "browser-devtools"))]
pub enum DynamicBlock {}

/// Build-time policy that is fixed for the life of the service worker.
pub struct RuntimeOptions {
    /// `initialize({ dev: … })` — what the *bundle asked for*. This is a
    /// request, not a verdict: [`RuntimeFactory::new`] runs it through
    /// [`SandboxMode::resolve`] and keeps only the result, because a bundle
    /// can ask for a workspace that was never compiled in — and because
    /// `dev: false` on a build that HAS the feature is an exported site,
    /// which still needs the sandbox's runtime half.
    pub dev_enabled: bool,
}

/// What the development sandbox contributes to this runtime.
///
/// **Three states, not two, and the middle one is the whole point.** The
/// sandbox is two separable things that used to be one boolean:
///
/// * a **runtime** — seed-on-boot, the generation ledger, journal convergence,
///   the dynamic-block rebuild, and the fact that `/` serves a site rather
///   than bouncing to the login page. Every bundle that has this code
///   compiled in needs it, because it is what makes an ImpressPress folder
///   *serve the site it ships*;
/// * a **workspace** — `/b/dev`, its route, the in-browser compiler, the
///   widened CSP and the cross-origin isolation that compiler needs.
///
/// Keying both on one flag is what made an exported bundle unbootable: the
/// export renders `const DEV_ENABLED = false;` into `sw.js` (it must — the
/// exported folder is a plain site with no workspace), and the runtime half
/// went with it, so the `seed/` the archive ships beside itself was never
/// imported and the exported site came up empty. Design §10.2 says the seed
/// is read "on a cold boot with no active generation" — a property of the
/// deployment, not of whether anyone can edit it.
///
/// So: **the feature decides the runtime; the flag decides the workspace.**
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxMode {
    /// `browser-devtools` is not compiled in. There is no sandbox code in the
    /// binary at all, and the runtime this produces is indistinguishable from
    /// one that was never asked for a sandbox — no seeded variables, no
    /// widened CSP, no seed import, nothing but the one console warning
    /// `initialize()` emits when a bundle asks anyway. "Feature off =
    /// nothing" is the security model (design §13), not an optimisation: a
    /// relaxed `worker-src`/`frame-src` on a build with no sandbox to use
    /// them is pure attack surface.
    Absent,
    /// The feature is compiled in and the bundle booted with `dev: false` —
    /// **an exported site**. The runtime half is fully live (it imports its
    /// `seed/`, converges its journal, rebuilds with its dynamic blocks and
    /// serves `/`), and the workspace half does not exist: no `/b/dev` route,
    /// no widened CSP, and no cross-origin isolation — which an exported site
    /// positively wants back, since isolation is what stops a page it serves
    /// from embedding a third-party iframe (design §20, amendment 14).
    Exported,
    /// The feature is compiled in and the bundle booted with `dev: true` —
    /// the development sandbox itself. Everything [`Self::Exported`] has,
    /// plus `/b/dev` and the tooling around it.
    Workspace,
}

impl SandboxMode {
    /// Resolve the mode from the build and the bundle's request.
    ///
    /// `feature_compiled` is `cfg!(feature = "browser-devtools")`;
    /// `requested` is the `dev` flag from `initialize({ dev: … })`. The flag
    /// is a *request*: a bundle can ask for a workspace that was never
    /// compiled in, and the answer is [`Self::Absent`], not a workspace.
    ///
    /// Taken as parameters rather than read from `cfg!` inside, so the rule is
    /// one pure expression stated once instead of a `cfg!` repeated at each
    /// site that consumes it — every consumer reads
    /// [`RuntimeFactory::mode`], and the raw request is deliberately not
    /// stored on the factory at all.
    pub const fn resolve(feature_compiled: bool, requested: bool) -> Self {
        match (feature_compiled, requested) {
            (false, _) => Self::Absent,
            (true, false) => Self::Exported,
            (true, true) => Self::Workspace,
        }
    }

    /// Whether the sandbox's **runtime** half is live: the ledger, the seed
    /// import, journal convergence, the dynamic-block rebuild, and the
    /// `HAS_LANDING_PAGE` fact. True for both compiled-in modes.
    pub const fn runtime_present(self) -> bool {
        !matches!(self, Self::Absent)
    }

    /// Whether the **workspace** half is exposed: the `/b/dev` route, the
    /// widened CSP, the cross-origin isolation and the framing relaxation.
    /// True only for [`Self::Workspace`].
    pub const fn workspace(self) -> bool {
        matches!(self, Self::Workspace)
    }
}

// The rule, checked by rustc in the configuration actually being built rather
// than by a unit test passing hand-written booleans. `impresspress-web` does
// not compile for the host at all (every module reaches into
// `impresspress_browser`'s `#[cfg(target_arch = "wasm32")]` items), so a
// `#[test]` here would never execute; these `const` assertions do, on every
// `cargo check --target wasm32-unknown-unknown` in both configurations and in
// CI's wasm build. That is also the stronger check: it exercises the real
// `cfg!(feature = …)` wiring below, which a parameterised unit test cannot.

// --- feature off: nothing, whatever the bundle asked for -------------------
#[cfg(not(feature = "browser-devtools"))]
const _: () = {
    assert!(matches!(
        SandboxMode::resolve(cfg!(feature = "browser-devtools"), false),
        SandboxMode::Absent
    ));
    assert!(matches!(
        SandboxMode::resolve(cfg!(feature = "browser-devtools"), true),
        SandboxMode::Absent
    ));
};

// --- feature on: the flag chooses which half ------------------------------
#[cfg(feature = "browser-devtools")]
const _: () = {
    // `dev: false` — an exported site. The runtime half IS live; this is the
    // assertion whose absence made an exported bundle boot empty.
    assert!(matches!(
        SandboxMode::resolve(cfg!(feature = "browser-devtools"), false),
        SandboxMode::Exported
    ));
    assert!(SandboxMode::resolve(cfg!(feature = "browser-devtools"), false).runtime_present());
    assert!(!SandboxMode::resolve(cfg!(feature = "browser-devtools"), false).workspace());

    // `dev: true` — the sandbox itself: both halves.
    assert!(matches!(
        SandboxMode::resolve(cfg!(feature = "browser-devtools"), true),
        SandboxMode::Workspace
    ));
    assert!(SandboxMode::resolve(cfg!(feature = "browser-devtools"), true).runtime_present());
    assert!(SandboxMode::resolve(cfg!(feature = "browser-devtools"), true).workspace());
};

// --- and the predicates agree with the states, in every build -------------
const _: () = {
    assert!(!SandboxMode::Absent.runtime_present());
    assert!(!SandboxMode::Absent.workspace());
    assert!(SandboxMode::Exported.runtime_present());
    assert!(!SandboxMode::Exported.workspace());
    assert!(SandboxMode::Workspace.runtime_present());
    assert!(SandboxMode::Workspace.workspace());
};

/// The browser platform services plus the policy every runtime is built under.
pub struct RuntimeFactory {
    /// The resolved verdict from [`SandboxMode::resolve`], computed once in
    /// [`RuntimeFactory::new`]. The raw `initialize({ dev })` request is
    /// intentionally *not* retained: keeping only the resolved value is what
    /// makes it impossible for a later consumer to key on "the bundle asked
    /// for it" on a build where the sandbox does not exist.
    pub(crate) mode: SandboxMode,
    pub(crate) config_svc: Arc<dyn ConfigService>,
    /// The runtime's per-block [`wafer_run::ConfigSource`]. Built empty and
    /// filled by [`crate::BrowserBootHooks`] once the variables table exists
    /// — see `SharedConfigSource` for why the browser is the one target that
    /// cannot supply this at build time. Held on the factory (like
    /// `config_svc` and `crypto`) so a rebuild reuses the same allocation and
    /// the blocks of the new runtime see the config the old one resolved.
    pub(crate) config_source: Arc<impresspress_core::config_source::SharedConfigSource>,
    /// Held as the concrete type (not `Arc<dyn CryptoService>`) so
    /// [`crate::BrowserBootHooks`] can rotate the JWT secret through
    /// `set_jwt_secret` after admin's migration seeds it.
    pub(crate) crypto: Arc<impresspress_browser::crypto::BrowserCryptoService>,
    pub(crate) llm: Arc<dyn LlmService>,
    pub(crate) image: Arc<dyn ImageService>,
    pub(crate) vector: Arc<dyn VectorService>,
    pub(crate) embedding: Arc<dyn EmbeddingService>,
    /// The sandbox control plane, once it has been installed. `None` on cold
    /// start — the first runtime is built before there is anything to control.
    #[cfg(feature = "browser-devtools")]
    pub(crate) dev: Option<Arc<DevShared>>,
}

impl RuntimeFactory {
    /// Construct the browser services once.
    /// `BrowserEmbeddingService::new` is the only fallible one.
    pub fn new(options: RuntimeOptions) -> Result<Self, String> {
        let mode = SandboxMode::resolve(cfg!(feature = "browser-devtools"), options.dev_enabled);

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
        let crypto = Arc::new(impresspress_browser::crypto::BrowserCryptoService::new(
            String::new(),
        ));

        let llm: Arc<dyn LlmService> =
            Arc::new(impresspress_browser::llm::BrowserLlmService::new());
        let image: Arc<dyn ImageService> =
            Arc::new(impresspress_browser::image::BrowserImageService::new());
        let vector: Arc<dyn VectorService> =
            Arc::new(impresspress_browser::vector::BrowserVectorService::new());
        // Logged as well as returned: this is the one fallible service, the
        // error surfaces to JS as an opaque `initialize()` rejection, and the
        // console line is what tells an operator *which* service failed.
        let embedding: Arc<dyn EmbeddingService> =
            match impresspress_browser::vector::BrowserEmbeddingService::new() {
                Ok(svc) => Arc::new(svc),
                Err(e) => {
                    web_sys::console::error_1(&format!("BrowserEmbeddingService init: {e}").into());
                    return Err(e);
                }
            };

        Ok(Self {
            mode,
            config_svc,
            config_source,
            crypto,
            llm,
            image,
            vector,
            embedding,
            #[cfg(feature = "browser-devtools")]
            dev: None,
        })
    }

    /// Install the sandbox control plane. Every runtime built after this point
    /// carries the `impresspress/dev` block, its `/b/dev` Admin route and the
    /// WRAP grant it needs on the published site.
    #[cfg(feature = "browser-devtools")]
    pub fn with_dev(mut self, dev: Arc<DevShared>) -> Self {
        self.dev = Some(dev);
        self
    }

    /// Build + boot one runtime. `dynamic` is empty on cold start.
    ///
    /// Returns the booted `Wafer`; the caller decides whether this runtime
    /// becomes the live one (`store_wafer`) or replaces one (`replace_wafer`).
    pub async fn build(&self, dynamic: &[DynamicBlock]) -> Result<wafer_run::Wafer, JsValue> {
        #[cfg(not(feature = "browser-devtools"))]
        let _ = dynamic;

        // ── Phase 1 ─────────────────────────────────────────────────────────
        // Build with the block_settings already in OPFS, and an EMPTY
        // ConfigSource. Nothing may be SEEDED yet: the
        // `impresspress__admin__variables` / `impresspress__admin__block_settings`
        // tables are created by admin's lazy `lifecycle(Init)`, and admin can't
        // run until the wafer is built and sealed.
        //
        // The schema-drift class of bug (#210/#211) came from this crate trying
        // to short-cut that chicken-and-egg with `CREATE TABLE IF NOT EXISTS`
        // pre-creates that duplicated the admin migration schema by hand. Any
        // drift between the two schemas was silent until the first per-block
        // `migration_helper::write_state` upserted into the stale table and
        // failed on a missing column, taking the whole runtime with it.
        //
        // The proper fix is what the native CLI and Cloudflare runner already
        // do: defer seeding until *after* `init_block(admin)`. Admin's
        // migration is the single source of schema truth; this crate just reads
        // back what it created.
        //
        // READING what an earlier boot created is another matter, and it has
        // to happen here, as Cloudflare's builds do: every block's Init —
        // admin's first — decides whether to run its migrations from the
        // migration state in block_settings, which it reads off the config
        // snapshot (`migration_helper::apply_if_blessed`). Built with an empty
        // snapshot, admin found no state on every boot and re-ran all of its
        // migrations over a database it had already migrated. `load` never
        // writes, and on a fresh profile — no table yet — it answers the empty
        // settings this build used to start from. The boot hook still seeds,
        // and republishes whatever that changes, after admin's Init.
        //
        // And the browser consents to its own migrations, as the native CLI
        // does with `--run-migrations`: loading a new bundle IS deploying it.
        // There is no operator step, no `/_deploy/init`, and no other place a
        // browser install could ever bless a changed migration, so without the
        // consent a database created by an older bundle would log "schema
        // drift" and keep its old schema for good. With it, the gate still
        // skips a block whose recorded hash matches the code, so a migration
        // runs once per change, not once per boot.
        let db = impresspress_browser::make_database_service();
        let initial_block_settings = impresspress_core::platform_state::block_settings::load(&db)
            .await
            .map_err(|e| JsValue::from_str(&format!("load block settings: {e}")))?;
        let mut initial_config = builder::RuntimeConfig::new();
        initial_config
            .both(
                impresspress_core::features::BLOCK_SETTINGS_CONFIG_KEY,
                initial_block_settings.to_config_json(),
            )
            .both(impresspress_core::migration_helper::RUN_MIGRATIONS_KEY, "1");
        // The factory's own `SharedConfigSource`, EMPTY at this point and
        // filled by the boot hook below once admin's migration has created the
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

        // `add_block_config` is a map insert, not a merge — the last
        // declaration for a block name wins outright. So the sandbox's
        // `frame_ancestors` relaxation is folded into this one value and
        // declared once, below; a second `block_config` for the same block
        // would silently drop the CSP.
        //
        // The `mut` on this and on `builder` only bites under
        // `browser-devtools`; without the feature there is no sandbox branch
        // to widen either of them.
        #[cfg_attr(
            not(feature = "browser-devtools"),
            expect(
                unused_mut,
                reason = "only the sandbox branch widens it, and that branch is \
                          `feature = \"browser-devtools\"`-gated"
            )
        )]
        let mut security_headers = serde_json::json!({ "csp": self.csp() });

        // Both config surfaces, holding only the block settings read above and
        // the migration consent.
        // The browser cannot know a single VARIABLE at build time: the
        // `variables` table does not exist until admin's migration runs, so
        // every value arrives through `BrowserBootHooks::seed_after_admin_init`,
        // which publishes it onto both surfaces at once via
        // `RuntimeConfig::republish`.
        // Going through `install` even with nothing to install is what keeps
        // the async `ConfigService` and the synchronous snapshot in one
        // owner's hands — the builder has no other way to receive a
        // `ConfigService`.
        //
        // `fill_config_service` rather than a closure that ignores its
        // argument: this handle is filled by `set`, so the map must be written
        // through it. Discarding the map was correct only for as long as the
        // `RuntimeConfig` above stayed empty — the first `both()` added here
        // would otherwise reach the snapshot and be dropped from the async
        // surface, silently.
        let config_svc = self.config_svc.clone();
        let (with_config, ()) = initial_config.install(
            ImpresspressBuilder::new()
                .database(impresspress_browser::make_database_service())
                .storage(impresspress_browser::make_storage_service()),
            |map| (builder::fill_config_service(config_svc, map), ()),
        );

        #[cfg_attr(
            not(feature = "browser-devtools"),
            expect(
                unused_mut,
                reason = "only the sandbox branch extends it, and that branch is \
                          `feature = \"browser-devtools\"`-gated"
            )
        )]
        let mut builder = with_config
            .crypto(crypto_svc)
            .network(impresspress_browser::make_network_service())
            .logger(impresspress_browser::make_console_logger())
            .llm_service("browser", self.llm.clone())
            .image_service("browser", self.image.clone())
            .vector_service(self.vector.clone())
            .embedding_service(self.embedding.clone())
            .block_settings(initial_block_settings)
            .config_source(cfg_source);

        #[cfg(feature = "browser-devtools")]
        if let Some(dev) = &self.dev {
            use impresspress_core::blocks::dev;

            // ── The RUNTIME half — both compiled-in modes ─────────────────
            //
            // Everything below is what makes an ImpressPress folder serve the
            // site it ships, and an exported bundle needs all of it. See
            // `SandboxMode` for why this is not keyed on the `dev` flag.
            //
            // The block itself is REGISTERED in both modes, and that is what
            // runs its `lifecycle(Init)` — which is what creates the
            // `impresspress__dev__*` ledger tables the seed import, the
            // activation journal and the generation history all write to. It
            // is registered under the runtime's own block authority, which is
            // the only authority that may write admin's `block_settings`
            // migration-tracking row; running those migrations from the boot
            // path instead would mean granting the dev block write access to
            // another block's table for no reason but bookkeeping.
            //
            // What keeps `/b/dev` unreachable in `Exported` is the absence of
            // the ROUTE below — which is the same gate design §13 names as
            // *the* gate ("the router — not any check inside any handler").
            // An unrouted block has no HTTP surface at all.
            //
            // Which constructor is the mode itself: both register the block,
            // but only the workspace one routes `/b/dev` below, and only it
            // may therefore declare that surface in its `BlockInfo` — see
            // `DevBlock::runtime_only`.
            let dev_block: Arc<dyn wafer_run::Block> = Arc::new(if self.mode.workspace() {
                dev::DevBlock::with_workspace(dev.clone())
            } else {
                dev::DevBlock::runtime_only(dev.clone())
            });
            builder = builder
                .extra_block(dev::BLOCK_NAME, dev_block)
                // The published site is owned by `wafer-run/web`, so the dev
                // block cannot declare this grant itself — a block may only
                // grant what it owns. Whoever registers it hands it over.
                // Needed in BOTH modes: the seed import writes the site and
                // the data-snapshot tables through these grants, under the
                // boot context, before anything is serving.
                .wrap_grants(dev::wrap_grants())
                // A sandbox iteration republishes the site under the same
                // URLs; a cached page would show the previous generation. An
                // exported bundle keeps it too — design §10.1: "the exported
                // bundle serves the site with `cache_mode = "no-cache"` too:
                // it is a local preview and the user will re-export".
                .block_config(
                    "wafer-run/web",
                    serde_json::json!({ "cache_mode": "no-cache" }),
                );

            // The generation's own blocks. An exported bundle activates its
            // seeded generation exactly as the workspace activates a compiled
            // one, so a site with a backend block serves that block here too.
            for (spec, block) in dynamic {
                builder = builder.extra_block(spec.name.clone(), Arc::clone(block));
                for route in &spec.routes {
                    // `add_refined_route`, NOT `add_route`: a guest's accepted
                    // spec carries one `Public` prefix as a FLOOR, on the
                    // understanding that every path it actually serves is
                    // governed by the endpoint the guest declared for it. With
                    // `add_route` that floor would be the whole answer for any
                    // path the guest did not declare — including EVERY path,
                    // for a guest that declares no endpoints at all — and the
                    // sandbox would serve host-compiled third-party code to
                    // anonymous callers. See `routing::ExtraRoute`.
                    builder = builder.add_refined_route(
                        route.prefix.clone(),
                        spec.name.clone(),
                        route.access.to_route_access(),
                    );
                }
            }

            // ── The WORKSPACE half — `dev: true` only ────────────────────
            if self.mode.workspace() {
                // `/b/dev` is registered at `RouteAccess::Admin` here, at the
                // router — not by a check inside any handler. That is the
                // single gate keeping the sandbox admin-only, and its absence
                // is what makes `/b/dev` a 404 on an exported site.
                builder = builder.add_route(
                    dev::ROUTE_PREFIX,
                    dev::BLOCK_NAME,
                    impresspress_core::routing::RouteAccess::Admin,
                );
                // The `/b/dev` page previews the live site in a same-origin
                // iframe. An exported site frames nothing.
                security_headers["frame_ancestors"] = serde_json::json!("self");
                // The compiler worker spawns blob-URL subordinate workers. The
                // operator `csp` cannot grant `blob:` in `worker-src` — the
                // block refuses it there — so the embedder asks the block
                // itself, which adds exactly that source and nothing else.
                security_headers["allow_blob_workers"] = serde_json::json!(true);
                // `/b/dev` is cross-origin isolated (COOP + COEP) for the
                // compiler's `SharedArrayBuffer`, and a COEP document can only
                // embed nested documents that carry a compatible COEP themselves
                // — the HTML spec's check is origin-independent — so the SITE has
                // to send it too or the preview iframe stays blank. Deployment-
                // wide, through the block that owns response headers, and
                // `credentialless` rather than `require-corp` so an agent-built
                // page can still show a cross-origin image whose host never set
                // CORP (design §20, amendment 14). `blocks::dev::page` sets the
                // same pair on its own document and must agree with this value.
                //
                // Deliberately NOT set in `Exported`: there is no compiler to
                // need `SharedArrayBuffer` and no preview frame to keep
                // loadable, and isolation is exactly what stops a page the
                // agent wrote from embedding a YouTube video, a map or Stripe
                // Embedded Checkout (amendment 14's stated cost). An exported
                // site gets those back.
                security_headers["cross_origin_isolation"] = serde_json::json!("credentialless");
            }
        }

        let builder = builder.block_config("wafer-run/security-headers", security_headers);

        let block_settings_handle = builder.block_settings_handle();
        // The router verifies JWTs against this secret. It's empty at build
        // time (the variables table doesn't exist yet), so grab the handle and
        // rotate it to the seeded value in `seed_after_admin_init`, alongside
        // the crypto service that signs with it.
        let jwt_secret_handle = builder.jwt_secret_handle();

        let mut wafer = builder
            .build()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        wafer.set_asset_loader(&impresspress_browser::make_sw_asset_loader());

        // ── Phase 2 ─────────────────────────────────────────────────────────
        // Run the shared boot funnel: grants → seal → init_block(admin) →
        // seed_after_admin_init → the remaining blocks.
        //
        // admin's `lifecycle(Init)` runs FIRST so its migrations create the
        // canonical `impresspress__admin__variables` + `block_settings` tables
        // before the seed hook reads them — admin's migration is the single
        // source of schema truth (the #210/#211 schema-drift lesson). The hook
        // then seeds + publishes into the services the wafer already holds (see
        // `BrowserBootHooks`), all over `BrowserDatabaseService` rather than
        // the old bridge raw-SQL strings.
        let hooks = crate::BrowserBootHooks {
            db: db.clone(),
            config_svc: self.config_svc.clone(),
            config_source: self.config_source.clone(),
            block_settings_handle,
            jwt_secret_handle,
            crypto: self.crypto.clone(),
            mode: self.mode,
        };
        builder::boot(
            &mut wafer,
            &hooks,
            // The admin permissions page is served in the browser too, so the
            // rows it writes are this runtime's deployment-owned grants. The
            // browser used to be the one target that never read them — not by
            // decision, but because the grant call was a separate statement
            // every caller had to remember. On a fresh profile the table does
            // not exist yet and `load` degrades to no grants.
            builder::GrantSource::Database(&db),
            // A browser runtime is long-lived and inspectable (and rebuilt on
            // demand), so one broken block must not wedge the whole page.
            builder::InitPolicy::Tolerant,
        )
        .await
        .map_err(|e| JsValue::from_str(&format!("boot: {e}")))?;

        Ok(wafer)
    }

    /// The `Content-Security-Policy` every response is served under.
    ///
    /// Keyed off [`SandboxMode::workspace`] rather than `dev.is_some()`: the
    /// policy is resolved once per runtime build, and a sandbox activation
    /// must not have to widen headers on a runtime that is already answering
    /// requests. A bundle that was never booted with `{ dev: true }` — an
    /// exported site, or any build without `browser-devtools` — therefore
    /// carries the unrelaxed policy, which is what the feature-off smoke
    /// asserts. Both relaxations exist for the workspace alone: the compiler
    /// worker and the `/b/dev` preview iframe.
    fn csp(&self) -> String {
        let mut csp = crate::IMPRESSPRESS_CSP.to_string();
        if self.mode.workspace() {
            // The compiler worker (a same-origin module worker; its blob-URL
            // subordinate workers come from `allow_blob_workers`) and the
            // live-site preview iframe on `/b/dev`.
            csp.push_str("; worker-src 'self'; frame-src 'self'");
        }
        csp
    }
}
