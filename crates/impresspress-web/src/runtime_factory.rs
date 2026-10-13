//! Builds a booted [`wafer_run::Wafer`] through `impresspress-browser`'s boot
//! funnel ([`impresspress_browser::boot`]), adding what is this app's: its
//! security headers, its seeds ([`crate::config::WebSeeds`]) and the
//! sandbox's block, routes and generation blocks.
//!
//! Cold start builds once with no dynamic blocks; an activation that changes
//! the block set builds again with the new set and swaps the runtime (see
//! `site_runtime`). Every build goes through [`RuntimeFactory::build`],
//! parameterised by the dynamic block set — so the second build is the *same*
//! build, not a hand-kept copy of it.
//!
//! The browser services ([`BrowserServices`]) are constructed once and shared
//! by every runtime the factory builds. That is what keeps a rebuild cheap
//! and, more importantly, what keeps the seeded JWT secret and the loaded
//! config alive across a swap: the crypto service and `ConfigService` the new
//! runtime gets are the same allocations the old one held.

#[cfg(feature = "browser-site-runtime")]
use std::sync::Arc;

use impresspress_browser::boot::BrowserServices;
#[cfg(feature = "browser-site-runtime")]
use impresspress_core::blocks::dev::{DevShared, DynamicBlockSpec};
#[cfg(feature = "browser-site-runtime")]
use impresspress_core::routing::RouteAccess;
use impresspress_core::sandbox_mode::SandboxMode;
use wasm_bindgen::prelude::*;

/// One dynamically-registered guest: its manifest entry plus the already-loaded
/// block. Loading (wasmi instantiation over the stored artifact) is the
/// caller's job — the factory only registers what it is handed, so a guest that
/// fails to load never reaches a `Wafer` at all.
#[cfg(feature = "browser-site-runtime")]
pub type DynamicBlock = (DynamicBlockSpec, Arc<dyn wafer_run::Block>);

/// Placeholder element type for builds without `browser-site-runtime`.
///
/// Uninhabited on purpose: without the feature the sandbox's runtime cannot
/// exist, so the only slice that can be constructed is the empty one. `build(&[])` therefore
/// compiles unchanged in both configurations without the call site growing a
/// `#[cfg]`, and no "the feature is off but we still handled a dynamic block"
/// path can be written by accident.
#[cfg(not(feature = "browser-site-runtime"))]
pub enum DynamicBlock {}

/// The routes [`RuntimeFactory::build`] registers for the `impresspress/dev`
/// block, in the order the router must be given them:
/// `blocks::dev::WORKSPACE_ROUTES` when the workspace is compiled in
/// ([`SandboxMode::COMPILED`]), and none otherwise.
///
/// `build` iterates exactly this, so the const assertions below state what
/// the runtime actually routes, not a parallel belief about it. In a site
/// build the empty answer holds twice over: `impresspress-core` without
/// `block-dev` has no `WORKSPACE_ROUTES` to name, so no branch of this
/// function can return one.
#[cfg(feature = "browser-site-runtime")]
const fn workspace_routes() -> &'static [(&'static str, RouteAccess)] {
    #[cfg(feature = "browser-devtools")]
    if SandboxMode::COMPILED.workspace() {
        return impresspress_core::blocks::dev::WORKSPACE_ROUTES;
    }
    &[]
}

// This crate's features against the mode `impresspress-core` compiled.
// `SandboxMode::COMPILED` reads the core's `site-runtime`/`block-dev`, which
// `browser-site-runtime`/`browser-devtools` forward to; these blocks check,
// in each of this crate's three configurations, that the forwarding holds —
// that the mode the runtime declares (`impresspress-browser`'s
// `compiled_sandbox_mode()` and its custom section) is the one this crate's
// own `#[cfg(feature = …)]` code was compiled for — and what the factory
// routes for it. Checked by rustc in the configuration actually being built
// rather than by a unit test passing hand-written values: `impresspress-web`
// does not compile for the host at all (every module reaches into
// `impresspress_browser`'s `#[cfg(target_arch = "wasm32")]` items), so a
// `#[test]` here would never execute; these `const` assertions do, on every
// `cargo check --target wasm32-unknown-unknown` of each configuration — CI's
// three wasm32 clippy steps (no features, `browser-site-runtime`,
// `browser-devtools`) and its wasm builds. The predicates themselves are
// asserted beside the enum, in `impresspress_core::sandbox_mode`.
// One block per configuration; exactly one of them compiles in any build.

// --- no sandbox: nothing -----------------------------------------------------
#[cfg(not(feature = "browser-site-runtime"))]
const _: () = {
    assert!(matches!(SandboxMode::COMPILED, SandboxMode::Absent));
    assert!(!SandboxMode::COMPILED.runtime_present());
    assert!(!SandboxMode::COMPILED.workspace());
};

// --- site build: the runtime half, and no workspace route ------------------
#[cfg(all(feature = "browser-site-runtime", not(feature = "browser-devtools")))]
const _: () = {
    assert!(matches!(SandboxMode::COMPILED, SandboxMode::Site));
    // The runtime half is live (`site_runtime::attach` relies on this), the
    // workspace is not.
    assert!(SandboxMode::COMPILED.runtime_present());
    assert!(!SandboxMode::COMPILED.workspace());
    // And so the factory routes nothing of the workspace: no `/b/dev`, no
    // `/b/dev/enter`. The router legs of `impresspress-core`'s
    // `tests/site_runtime.rs::no_workspace_path_answers` test only that
    // test's fixture; this is the function production's registration loop
    // (`RuntimeFactory::build`) iterates.
    assert!(workspace_routes().is_empty());
};

// --- devtools build: both halves, and every workspace route ----------------
#[cfg(feature = "browser-devtools")]
const _: () = {
    assert!(matches!(SandboxMode::COMPILED, SandboxMode::Workspace));
    assert!(SandboxMode::COMPILED.runtime_present());
    assert!(SandboxMode::COMPILED.workspace());
    assert!(workspace_routes().len() == impresspress_core::blocks::dev::WORKSPACE_ROUTES.len());
    assert!(!impresspress_core::blocks::dev::WORKSPACE_ROUTES.is_empty());
};

/// The browser platform services plus this app's additions to every runtime
/// built over them.
pub struct RuntimeFactory {
    services: BrowserServices,
    /// The sandbox control plane, once it has been installed. `None` until
    /// `site_runtime::attach` installs it, which it does before the first
    /// build (see that function).
    #[cfg(feature = "browser-site-runtime")]
    pub(crate) dev: Option<Arc<DevShared>>,
}

impl RuntimeFactory {
    /// A factory over `services`, constructed once per service worker.
    pub fn new(services: BrowserServices) -> Self {
        Self {
            services,
            #[cfg(feature = "browser-site-runtime")]
            dev: None,
        }
    }

    /// Install the sandbox control plane. Every runtime built after this point
    /// carries the `impresspress/dev` block and the WRAP grant it needs on the
    /// published site — and, in a `browser-devtools` build
    /// ([`SandboxMode::Workspace`]), its `/b/dev` routes (`workspace_routes`).
    #[cfg(feature = "browser-site-runtime")]
    pub fn with_dev(mut self, dev: Arc<DevShared>) -> Self {
        self.dev = Some(dev);
        self
    }

    /// Build + boot one runtime. `dynamic` is empty on cold start.
    ///
    /// Returns the booted `Wafer`; the caller decides whether this runtime
    /// becomes the live one (`store_wafer`) or replaces one (`replace_wafer`).
    pub async fn build(&self, dynamic: &[DynamicBlock]) -> Result<wafer_run::Wafer, JsValue> {
        #[cfg(not(feature = "browser-site-runtime"))]
        let _ = dynamic;

        // The funnel's pre-configured builder: the browser services, the
        // block settings an earlier boot left, the infrastructure config and
        // the shared config source. See `BrowserServices::builder`.
        //
        // The `mut` here bites under `browser-site-runtime`: the sandbox's
        // runtime half registers the dev block and the generation's own blocks.
        #[cfg_attr(
            not(feature = "browser-site-runtime"),
            expect(
                unused_mut,
                reason = "only the sandbox branch extends it, and that branch is \
                          `feature = \"browser-site-runtime\"`-gated"
            )
        )]
        let mut builder = self.services.builder().await?;

        // `add_block_config` is a map insert, not a merge — the last
        // declaration for a block name wins outright. So the sandbox's
        // `frame_ancestors` relaxation is folded into this one value and
        // declared once, below; a second `block_config` for the same block
        // would silently drop the CSP.
        //
        // Widened below for the workspace alone (`SandboxMode::COMPILED`).
        let mut security_headers = serde_json::json!({ "csp": csp() });

        #[cfg(feature = "browser-site-runtime")]
        if let Some(dev) = &self.dev {
            use impresspress_core::blocks::dev;

            // ── The RUNTIME half — both sandbox builds ──
            //
            // Everything below is what makes an ImpressPress folder serve the
            // site it ships, and an exported site needs all of it — a site
            // build (`browser-site-runtime`) compiles this and nothing of the
            // workspace. See `SandboxMode`.
            //
            // The block itself is REGISTERED in both builds, and that is what
            // runs its `lifecycle(Init)` — which is what creates the
            // `impresspress__dev__*` ledger tables the seed import, the
            // activation journal and the generation history all write to. It
            // is registered under the runtime's own block authority, which is
            // the only authority that may write admin's `block_settings`
            // migration-tracking row; running those migrations from the boot
            // path instead would mean granting the dev block write access to
            // another block's table for no reason but bookkeeping.
            //
            // What keeps `/b/dev` unreachable on a site build is the absence
            // of its ROUTES ([`workspace_routes`], below) — which is the same
            // gate design §13 names as *the* gate ("the router — not any check
            // inside any handler") — and, behind it, the absence of the
            // workspace's code: a site build's block has no dispatch table.
            //
            // Each build has one constructor, taking what that build's block
            // reads. The workspace's serves `/b/dev` from the shared state;
            // the site build's holds nothing — the host keeps `DevShared` for
            // the activation queue and the boot run (`site_runtime::Sandbox`),
            // and this block only needs to be registered.
            #[cfg(feature = "browser-devtools")]
            let dev_block = dev::DevBlock::new(dev.clone());
            #[cfg(not(feature = "browser-devtools"))]
            let dev_block = {
                let _ = dev;
                dev::DevBlock::new()
            };
            let dev_block: Arc<dyn wafer_run::Block> = Arc::new(dev_block);
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

            // ── The WORKSPACE half — a devtools build only ──
            //
            // `/b/dev` is registered at `RouteAccess::Admin` here, at the
            // router — not by a check inside any handler. That is the single
            // gate keeping the sandbox admin-only, and its absence is what
            // makes `/b/dev` a 404 on an exported site. The one `Public` row,
            // the one-click entry page, is registered ahead of it and is
            // absent from an exported site for the same reason;
            // `dev::WORKSPACE_ROUTES` owns the list and its order, and
            // `workspace_routes` is the one place that decides whether this
            // runtime gets it — the function the const assertions above check
            // in this configuration.
            for (prefix, access) in workspace_routes() {
                builder = builder.add_route(*prefix, dev::BLOCK_NAME, *access);
            }
        }

        // The workspace's header widening, keyed on the build like
        // everything else about the mode: a constant, so a build without the
        // workspace (a site build, or no sandbox at all) folds the branch
        // away. Outside the `dev` branch above because nothing in it reads
        // the control plane — the headers are the build's, not the
        // activation's.
        if SandboxMode::COMPILED.workspace() {
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
            // Deliberately NOT set on a site build: there is no compiler to
            // need `SharedArrayBuffer` and no preview frame to keep
            // loadable, and isolation is exactly what stops a page the
            // agent wrote from embedding a YouTube video, a map or Stripe
            // Embedded Checkout (amendment 14's stated cost). An exported
            // site gets those back.
            security_headers["cross_origin_isolation"] = serde_json::json!("credentialless");
        }

        let builder = builder.block_config("wafer-run/security-headers", security_headers);

        // Build and boot through the funnel, with this app's own seeds.
        let wafer = self
            .services
            .boot(builder, Some(&crate::config::WebSeeds))
            .await?;

        Ok(wafer)
    }
}

/// The `Content-Security-Policy` every response is served under.
///
/// Keyed on [`SandboxMode::COMPILED`], like the rest of the workspace's
/// header widening in [`RuntimeFactory::build`]: both relaxations exist for
/// the workspace alone — the compiler worker and the `/b/dev` preview iframe
/// — and the workspace exists exactly in a `browser-devtools` build. A site
/// build, or any build without the sandbox, therefore carries the unrelaxed
/// policy, which is what the feature-off smoke asserts; the condition is a
/// constant, so such a build has no branch that could widen it.
fn csp() -> String {
    if SandboxMode::COMPILED.workspace() {
        // The compiler worker (a same-origin module worker; its blob-URL
        // subordinate workers come from `allow_blob_workers`) and the
        // live-site preview iframe on `/b/dev`.
        return format!(
            "{}; worker-src 'self'; frame-src 'self'",
            crate::IMPRESSPRESS_CSP
        );
    }
    crate::IMPRESSPRESS_CSP.to_string()
}
