//! Shared routing table — maps URL path prefixes to impresspress blocks.
//!
//! Both Cloudflare and native adapters use this same routing logic.
//! All impresspress blocks are registered in the Wafer registry at boot; routing
//! dispatches via `ctx.call_block` without any factory indirection.

use wafer_run::{context::Context, AuthLevel, BlockInfo, InputStream, Message, OutputStream};

use crate::{endpoint_match, features::FeatureConfig};

/// URL prefix for embedded static assets, served by `impresspress/system`.
///
/// Single source of truth shared by the routing table below, the
/// `ui::assets` URL builders, and the pipeline's request-log noise filter —
/// so the prefix can't drift between them (a stale `/static/` literal in the
/// filter once made every asset request write a `request_logs` row).
pub const STATIC_PREFIX: &str = "/b/static/";

/// A single route entry.
///
/// `block` is the impresspress block name (`{org}/{block}`) used for feature-gating
/// and the inspector's [`routes_config`] view. `dispatch_to` is the Wafer block
/// name passed to `ctx.call_block`; it equals `block` for every route except the
/// inspector, which is feature-gated/displayed as `impresspress/inspector` but
/// dispatches to the `wafer-run/inspector` runtime block.
///
/// `router_final` controls whether [`route_to_block`] may *refine* `access`
/// with the target block's declared per-endpoint [`AuthLevel`] (see
/// [`declared_access`]): `false` (the default, via [`Route::new`] /
/// [`Route::proxy`]) lets a declared endpoint strengthen `access` — the
/// normal case, and also how an *undeclared* path under the route falls back
/// to [`declared_access`]'s fail-closed default (`Authenticated`) rather than
/// this route's own (possibly looser) `access`. `true` (via
/// [`Route::router_declared_public`]) makes `access` final: the router's own
/// declaration IS the complete authorization decision for that exact path,
/// and the [`declared_access`] fallback is never consulted. This is the
/// escape hatch for a narrow, single-purpose path that legitimately has no
/// session (a signed webhook, an OAuth provider callback, a password-reset
/// link) but the owning block hasn't declared it as a `BlockEndpoint` — see
/// [`Route::router_declared_public`] for why that matters.
pub struct Route {
    pub prefix: &'static str,
    pub access: RouteAccess,
    pub block: &'static str,
    pub dispatch_to: &'static str,
    router_final: bool,
}

impl Route {
    /// A route whose dispatch target equals its block name (the common case).
    const fn new(prefix: &'static str, access: RouteAccess, block: &'static str) -> Route {
        Route {
            prefix,
            access,
            block,
            dispatch_to: block,
            router_final: false,
        }
    }

    /// A route whose `ctx.call_block` target differs from its block name. Used
    /// only by the inspector, which dispatches to the `wafer-run/inspector`
    /// runtime block while remaining feature-gated as `impresspress/inspector`.
    const fn proxy(
        prefix: &'static str,
        access: RouteAccess,
        block: &'static str,
        dispatch_to: &'static str,
    ) -> Route {
        Route {
            prefix,
            access,
            block,
            dispatch_to,
            router_final: false,
        }
    }

    /// A narrow, exact-path route the ROUTER declares `Public` outright,
    /// bypassing the [`declared_access`] refinement step entirely.
    ///
    /// [`declared_access`]'s fallback for an undeclared path is
    /// `Authenticated` (fail-closed — see its doc comment), which is
    /// *stricter* than `Public`. Combined via [`RouteAccess::max`], a
    /// stricter fallback can only ever win over a looser prefix `access` —
    /// that's the whole point of the fail-closed default. Which means a
    /// path that must stay genuinely public (no session at all: Stripe
    /// webhooks verified by HMAC, an OAuth provider's browser-redirect
    /// callback, a password-reset link) but is NOT declared as a
    /// `BlockEndpoint` cannot be kept public by adding a normal
    /// [`Route::new`] entry — the fallback would still win. This
    /// constructor is the router-level escape hatch for exactly that case:
    /// it must be listed BEFORE the block's general prefix route
    /// (most-specific-first, like every other carve-out in [`ROUTES`]), and
    /// its `access` is final — no declared endpoint (there isn't one) or
    /// fallback can strengthen or weaken it.
    ///
    /// The real fix for each such path is still to declare it as a
    /// `BlockEndpoint` (with the correct `AuthLevel`) in the owning block's
    /// `info()`; this constructor exists because routing.rs cannot add that
    /// declaration to another block's file on its own.
    const fn router_declared_public(prefix: &'static str, block: &'static str) -> Route {
        Route {
            prefix,
            access: RouteAccess::Public,
            block,
            dispatch_to: block,
            router_final: true,
        }
    }
}

/// Access tier for a route.
///
/// Checked by [`route_to_block`] (via `check_access`) before dispatching to the
/// target block, for both built-in [`Route`]s and runtime-added [`ExtraRoute`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum RouteAccess {
    /// No auth check. Anyone can hit this route.
    Public,
    /// `msg.user_id()` must be non-empty, or the request is rejected with 403.
    Authenticated,
    /// User must have the `admin` role (per [`crate::util::is_admin`]) or 403.
    Admin,
}

impl RouteAccess {
    /// Bridge a block's declared per-endpoint [`AuthLevel`] (from
    /// `BlockInfo::endpoints`) into the router's coarse [`RouteAccess`] tier.
    /// The two enums are the same three-tier ladder; this is the single place
    /// they are mapped so the declared level can be enforced by the same
    /// `check_access` path as the prefix tier.
    fn from_auth_level(level: AuthLevel) -> RouteAccess {
        match level {
            AuthLevel::Public => RouteAccess::Public,
            AuthLevel::Authenticated => RouteAccess::Authenticated,
            AuthLevel::Admin => RouteAccess::Admin,
        }
    }

    /// The stricter of two tiers (`Public < Authenticated < Admin`). Used to
    /// combine the coarse prefix tier with a matched endpoint's declared level
    /// so neither can weaken the other: the prefix is a backstop for paths a
    /// block has not (yet) declared an endpoint for, and the declared endpoint
    /// level refines it where present.
    fn max(self, other: RouteAccess) -> RouteAccess {
        std::cmp::max(self, other)
    }
}

/// A runtime-added route registered by a downstream project via
/// `ImpresspressBuilder::add_route`.
///
/// Carries an owned `block_name` `String` (rather than the built-in [`Route`]'s
/// `&'static str`) since projects supply these at build time.
///
/// # Priority
///
/// Built-in [`ROUTES`] always win. An extra route with the same prefix as a
/// built-in is ignored. To disable a built-in route, disable its feature
/// flag — do not try to override it.
#[derive(Debug, Clone)]
pub struct ExtraRoute {
    pub prefix: String,
    pub access: RouteAccess,
    pub block_name: String,
}

/// The shared routing table. Order matters — more specific prefixes before general ones.
///
/// All block routes live under `/b/{block_name}/...`. SSR pages and JSON API
/// share the same prefix — blocks distinguish by HTTP method and path.
/// System endpoints (`/health`, `/nav`, `/static/`, `/debug/`) are the only
/// routes outside `/b/`.
pub const ROUTES: &[Route] = &[
    // System & static assets
    Route::new("/health", RouteAccess::Public, "impresspress/system"),
    // Static assets are content-hashed, immutable, and session-less by
    // design (CSS/JS/fonts/logo for the logged-out login/signup pages).
    // `SystemBlock::info().endpoints` declares them with MID-SEGMENT hash
    // placeholders (e.g. `/b/static/app-{hash}.css`) — `{name}` only binds a
    // WHOLE path segment in `endpoint_match::match_template`, so a real
    // request like `/b/static/app-abc123.css` never matches any declared
    // endpoint. That makes `declared_access` fall back to its fail-closed
    // `Authenticated` default, which combined via `RouteAccess::max` would
    // 403 every anonymous asset request (code review 2026-07-16, C1: "the
    // logged-out login/signup pages load with no CSS/JS/fonts/logo").
    // `router_declared_public` is the fix: it makes this route's own
    // `Public` access final, so the fail-closed default is never consulted.
    // Covers the whole `/b/static/*` prefix (this is a prefix route).
    Route::router_declared_public(STATIC_PREFIX, "impresspress/system"),
    // Inspector — runtime debugging UI (admin only). Feature-gated as
    // `impresspress/inspector` but dispatches to the `wafer-run/inspector` block.
    Route::proxy(
        "/b/inspector",
        RouteAccess::Admin,
        "impresspress/inspector",
        "wafer-run/inspector",
    ),
    // Auth — genuinely-public, session-less endpoints that `impresspress/auth-ui`
    // has NOT (yet) declared as `BlockEndpoint`s (routing.rs cannot add that
    // declaration to the block's own file). Each is gated by its own
    // token/secret/signature inside the handler, not by `msg.user_id()` — see
    // `Route::router_declared_public`'s doc comment for why a plain
    // `Route::new(_, Public, _)` entry would NOT be enough once undeclared
    // paths default to `Authenticated`. Must precede the general `/b/auth/`
    // entry below (most-specific-first).
    Route::router_declared_public("/b/auth/oauth/callback", "impresspress/auth-ui"), // OAuth provider browser redirect — single-use PKCE state, no prior session by design.
    Route::router_declared_public("/b/auth/api/oauth/sync-user", "impresspress/auth-ui"), // Internal caller gated by INTERNAL_SECRET header, not a user session.
    Route::router_declared_public("/b/auth/api/oauth/providers", "impresspress/auth-ui"), // Non-sensitive (which providers are configured); needed pre-login.
    Route::router_declared_public("/b/auth/reset-password", "impresspress/auth-ui"), // Password-reset SSR page — logged-out by definition.
    Route::router_declared_public("/b/auth/api/reset-password", "impresspress/auth-ui"), // Consumes a single-use reset token.
    Route::router_declared_public("/b/auth/api/forgot-password", "impresspress/auth-ui"), // Requests a reset token by email — no session yet.
    Route::router_declared_public("/b/auth/api/verify", "impresspress/auth-ui"), // Consumes a single-use email-verification token.
    Route::router_declared_public("/b/auth/api/resend-verification", "impresspress/auth-ui"), // Re-sends the verification token — no session yet.
    // Auth — SSR pages + API under /b/auth/
    Route::new("/b/auth/", RouteAccess::Public, "impresspress/auth-ui"),
    // Admin settings — more specific prefix must come before the /b/admin/ catch-all
    Route::new(
        "/b/admin/settings",
        RouteAccess::Admin,
        "impresspress/admin",
    ),
    // Admin — SSR pages + API under /b/admin/
    Route::new("/b/admin/", RouteAccess::Admin, "impresspress/admin"),
    Route::new("/b/admin", RouteAccess::Admin, "impresspress/admin"),
    // Feature blocks — SSR + API under /b/{block}/
    Route::new("/b/storage/", RouteAccess::Public, "impresspress/files"),
    Route::new(
        "/b/cloudstorage/",
        RouteAccess::Public,
        "impresspress/files",
    ),
    // Stripe webhook — verified by HMAC signature (`stripe.rs::handle_webhook`),
    // not by `msg.user_id()`. It is also declared Public in the products
    // block for discovery, while this router-final carve-out keeps delivery
    // reachable during boot or tests where BlockInfo metadata is unavailable.
    // Must precede the general `/b/products` entry below.
    Route::router_declared_public("/b/products/webhooks", "impresspress/products"),
    Route::new("/b/products", RouteAccess::Public, "impresspress/products"),
    // Legalpages — public reads + admin writes/UI.
    // Admin and API prefixes must come BEFORE the bare `/b/legalpages` entry
    // because `route_to_block` matches on first-prefix-hit. Admin handlers
    // inside the block do not re-check `is_admin`, so this gate is the only
    // thing keeping random callers off `/admin/publish` and friends.
    Route::new(
        "/b/legalpages/admin",
        RouteAccess::Admin,
        "impresspress/legalpages",
    ),
    Route::new(
        "/b/legalpages/api",
        RouteAccess::Admin,
        "impresspress/legalpages",
    ),
    Route::new(
        "/b/legalpages",
        RouteAccess::Public,
        "impresspress/legalpages",
    ),
    Route::new(
        "/b/userportal",
        RouteAccess::Public,
        "impresspress/userportal",
    ),
    // Messages — generic thread/message system
    // Route is open; block enforces admin for UI pages, authenticated for API
    Route::new("/b/messages", RouteAccess::Public, "impresspress/messages"),
    // LLM — chat orchestrator
    // Route is open; block enforces admin for UI pages, authenticated for API
    Route::new("/b/llm", RouteAccess::Public, "impresspress/llm"),
    // Vector — similarity search, hybrid retrieval, RAG ingestion.
    //
    // ONE prefix route. The previous nine decorative entries all shared the
    // same access tier (`Public`) and dispatch target, differing only in
    // path — pure duplication, since the block does its own per-method
    // path-param matching in `pages::route`. The per-endpoint access tier
    // now comes from `VectorBlock::info().endpoints` and is enforced
    // centrally via `declared_access` (UI pages → Admin, JSON API →
    // Authenticated), so the coarse prefix tier is `Public` and the declared
    // level refines it. The inspector sources endpoint granularity from the
    // same `info().endpoints` (see [`routes_config`]).
    Route::new("/b/vector/", RouteAccess::Public, "impresspress/vector"),
];

/// Generate the routing table as JSON config (same format as wafer-run/router).
/// Used to expose routes to the inspector.
///
/// Each coarse prefix [`Route`] contributes one `{prefix}**` entry. Endpoint
/// granularity (the exact method+path templates a block exposes) is sourced
/// from each block's `BlockInfo::endpoints` rather than from hand-maintained
/// per-endpoint `Route` entries — this is what lets the vector block collapse
/// to a single prefix route while the inspector still shows its nine
/// endpoints. Endpoint entries are de-duplicated against the prefix entries.
pub fn routes_config(block_infos: &[BlockInfo]) -> serde_json::Value {
    let mut routes: Vec<serde_json::Value> = ROUTES
        .iter()
        .map(|r| {
            let path = format!("{}**", r.prefix);
            serde_json::json!({ "path": path, "block": r.block })
        })
        .collect();

    // Per-endpoint granularity from the blocks themselves. Only emit entries
    // for blocks that own a built-in prefix route (so we mirror the routing
    // table, not the whole registry), and skip any whose exact `{prefix}**`
    // form already covers them.
    for info in block_infos {
        if !ROUTES.iter().any(|r| r.block == info.name) {
            continue;
        }
        for ep in &info.endpoints {
            let entry = serde_json::json!({
                "path": ep.path,
                "method": ep.method.to_string(),
                "block": info.name,
                "auth": ep.auth.to_string(),
            });
            if !routes.contains(&entry) {
                routes.push(entry);
            }
        }
    }

    serde_json::json!({ "routes": routes })
}

/// Resolve the declared per-endpoint access tier for `(msg.action,
/// msg.path)` from the target block's `BlockInfo::endpoints`, mapped into the
/// router's [`RouteAccess`] ladder.
///
/// Returns [`RouteAccess::Authenticated`] when no declared endpoint matches
/// (including when the block has no `BlockInfo` at all) — the caller
/// combines this with the coarse prefix tier via [`RouteAccess::max`], so an
/// UNDECLARED path under even a `Public`-tier prefix requires a logged-in
/// caller by default, and a declared path is governed by the stricter of
/// prefix and endpoint. This is the fail-closed fix for "route declarations
/// fail open" (undeclared endpoint metadata used to silently resolve to
/// `Public`): a block must now explicitly declare a `BlockEndpoint` — with
/// `AuthLevel::Public` — for any path that is genuinely meant to have no
/// session, or use [`Route::router_declared_public`] at the router level when
/// it can't (yet) declare that endpoint itself. `Authenticated`, not a hard
/// deny, so a forgotten declaration degrades to "please log in" rather than
/// 404ing a route that already works for logged-in callers.
fn declared_access(block_infos: &[BlockInfo], block_name: &str, msg: &Message) -> RouteAccess {
    let Some(info) = block_infos.iter().find(|i| i.name == block_name) else {
        return RouteAccess::Authenticated;
    };
    endpoint_match::endpoint_auth(&info.endpoints, msg.action(), msg.path())
        .map(RouteAccess::from_auth_level)
        .unwrap_or(RouteAccess::Authenticated)
}

/// Enforce a route's [`RouteAccess`] tier against the request. Returns
/// `Some(forbidden_response)` when the caller fails the tier, or `None` to
/// proceed. Shared by the built-in and extra-route dispatch loops.
fn check_access(access: RouteAccess, msg: &Message) -> Option<OutputStream> {
    match access {
        RouteAccess::Public => None,
        // Missing identity (anonymous OR stale session — crypto.rs leaves
        // `user_id` empty on any invalid token) → send browsers to login with a
        // return path; keep the JSON 403 for API callers. Both protected tiers
        // share this: an `Admin` route hit with no identity is a login problem,
        // not a role problem.
        RouteAccess::Authenticated if msg.user_id().is_empty() => {
            Some(crate::ui::unauthenticated_response(msg))
        }
        RouteAccess::Authenticated => None,
        RouteAccess::Admin if msg.user_id().is_empty() => {
            Some(crate::ui::unauthenticated_response(msg))
        }
        // Authenticated but lacking the admin role is a genuine 403, not a
        // "log in" — keep the styled/JSON forbidden response (no redirect).
        RouteAccess::Admin if !crate::util::is_admin(msg) => {
            Some(crate::ui::forbidden_response(msg))
        }
        RouteAccess::Admin => None,
    }
}

/// Route a message to the appropriate impresspress block based on request path.
///
/// Checks feature flags and admin role. Dispatches via `ctx.call_block` — all
/// impresspress blocks are registered in the Wafer registry at boot (zero-arg
/// blocks via `register_static_block!`, LlmBlock via `register_llm()`).
pub async fn route_to_block(
    ctx: &dyn Context,
    msg: Message,
    input: InputStream,
    features: &dyn FeatureConfig,
    block_infos: &[BlockInfo],
    extra_routes: &[ExtraRoute],
) -> OutputStream {
    let path = msg.path().to_string();

    // Root: redirect logged-in users to portal dashboard, anonymous to login.
    // When the deployment ships a static landing page, serve it directly via
    // `wafer-run/web` instead. Gated by the `WAFER_RUN_SHARED__HAS_LANDING_PAGE`
    // config var so the decision is explicit and works identically on native
    // and Cloudflare (no filesystem probe, which is meaningless on Workers and
    // CWD-relative on native).
    if path == "/" {
        let has_landing_page = ctx
            .config_get("WAFER_RUN_SHARED__HAS_LANDING_PAGE")
            .unwrap_or("false")
            == "true";
        if has_landing_page {
            return ctx.call_block("wafer-run/web", msg, input).await;
        }
        return root_redirect(msg.user_id().is_empty());
    }

    for route in ROUTES {
        let matches = path == route.prefix || path.starts_with(route.prefix);
        if !matches {
            continue;
        }

        // Feature gate
        if !features.is_block_enabled(route.block) {
            return crate::http::err_not_found("endpoint not found");
        }

        // Access gate. The coarse prefix tier is a backstop; if the target
        // block declares an endpoint matching this exact (action, path) we
        // also enforce that endpoint's declared `AuthLevel` — taking the
        // stricter of the two. This is what makes `BlockEndpoint::auth`
        // load-bearing instead of documentation-only, and lets blocks drop
        // their per-handler `is_admin`/`user_id` preambles. An UNDECLARED
        // path falls back to `Authenticated` (fail-closed), UNLESS this
        // route is `router_final` (see `Route::router_declared_public`), in
        // which case the router's own `access` is the complete decision and
        // `declared_access`'s fallback is never consulted.
        let access = if route.router_final {
            route.access
        } else {
            route
                .access
                .max(declared_access(block_infos, route.block, &msg))
        };
        if let Some(denied) = check_access(access, &msg) {
            return denied;
        }

        // Dispatch via call_block so WRAP sees the correct caller identity.
        return ctx.call_block(route.dispatch_to, msg, input).await;
    }

    // Fall back to project-registered extra routes. Built-ins above win on
    // prefix collision — this loop only runs when no built-in matched.
    for route in extra_routes {
        let matches = path == route.prefix || path.starts_with(&route.prefix);
        if !matches {
            continue;
        }

        // Feature gate — downstream-registered routes honor the admin disable
        // toggle exactly like the built-in `ROUTES` loop above (which they
        // bypassed before). Keep this gate in sync with that one.
        if !features.is_block_enabled(&route.block_name) {
            return crate::http::err_not_found("endpoint not found");
        }

        if let Some(denied) = check_access(route.access, &msg) {
            return denied;
        }

        return ctx.call_block(&route.block_name, msg, input).await;
    }

    crate::ui::not_found_response(&msg)
}

/// Build a root redirect response. Extracted for unit testability.
fn root_redirect(user_id_empty: bool) -> OutputStream {
    let target = if user_id_empty {
        "/b/auth/login"
    } else {
        "/b/userportal/"
    };
    crate::http::ResponseBuilder::new()
        .status(302)
        .set_header("Location", target)
        .body(Vec::new(), "text/plain")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that the routing table covers expected prefixes and block assignments.
    #[test]
    fn route_table_maps_expected_paths() {
        let cases = vec![
            // System endpoints
            ("/health", "impresspress/system"),
            ("/b/static/app.css", "impresspress/system"),
            // Inspector
            ("/b/inspector", "impresspress/inspector"),
            ("/b/inspector/blocks", "impresspress/inspector"),
            // All block routes under /b/
            ("/b/auth/login", "impresspress/auth-ui"),
            ("/b/auth/signup", "impresspress/auth-ui"),
            ("/b/auth/api/me", "impresspress/auth-ui"),
            ("/b/admin/", "impresspress/admin"),
            ("/b/admin/users", "impresspress/admin"),
            ("/b/admin", "impresspress/admin"),
            ("/b/storage/buckets", "impresspress/files"),
            ("/b/cloudstorage/shares", "impresspress/files"),
            ("/b/products", "impresspress/products"),
            ("/b/legalpages", "impresspress/legalpages"),
            ("/b/userportal", "impresspress/userportal"),
        ];

        for (path, expected_block) in cases {
            let matched = ROUTES
                .iter()
                .find(|r| path == r.prefix || path.starts_with(r.prefix));
            assert!(matched.is_some(), "path {path} should match a route");
            assert_eq!(
                matched.unwrap().block,
                expected_block,
                "path {path} should route to {expected_block}"
            );
        }
    }

    #[test]
    fn unmatched_paths_have_no_route() {
        // Legacy paths no longer match — all block routes are under /b/
        let unmatched = vec![
            "/unknown",
            "/foo/bar",
            "/",
            "/auth/login",
            "/admin/settings",
            "/storage/buckets",
            "/settings",
            "/profile",
            "/nav",
            "/debug/time",
        ];
        for path in unmatched {
            let matched = ROUTES
                .iter()
                .find(|r| path == r.prefix || path.starts_with(r.prefix));
            assert!(matched.is_none(), "path {path} should NOT match any route");
        }
    }

    #[test]
    fn admin_routes_require_admin() {
        for route in ROUTES {
            if route.prefix.starts_with("/b/admin") {
                assert_eq!(
                    route.access,
                    RouteAccess::Admin,
                    "route {} should require admin",
                    route.prefix
                );
            }
        }
    }

    #[test]
    fn non_admin_routes_dont_require_admin() {
        // Note: `/b/legalpages` is intentionally omitted here because it has
        // sub-routes (`/b/legalpages/admin`, `/b/legalpages/api`) that DO
        // require admin. Those sub-routes are verified by
        // `legalpages_admin_routes_require_admin`.
        let non_admin_prefixes = [
            "/health",
            "/static/",
            "/b/auth/",
            "/b/storage/",
            "/b/products",
            "/b/userportal",
            "/b/cloudstorage/",
        ];
        for route in ROUTES {
            if non_admin_prefixes
                .iter()
                .any(|p| route.prefix == *p || route.prefix.starts_with(p))
            {
                assert_ne!(
                    route.access,
                    RouteAccess::Admin,
                    "route {} should NOT require admin",
                    route.prefix
                );
            }
        }
    }

    #[tokio::test]
    async fn root_redirects_anonymous_to_login() {
        let out = super::root_redirect(true);
        let buf = out.collect_buffered().await.unwrap();
        let status = buf
            .meta
            .iter()
            .find(|e| e.key == "resp.status")
            .map(|e| e.value.as_str())
            .unwrap_or("");
        let location = buf
            .meta
            .iter()
            .find(|e| e.key == "resp.header.Location")
            .map(|e| e.value.as_str())
            .unwrap_or("");
        assert_eq!(status, "302");
        assert_eq!(location, "/b/auth/login");
    }

    #[tokio::test]
    async fn root_redirects_authenticated_to_portal_home() {
        let out = super::root_redirect(false);
        let buf = out.collect_buffered().await.unwrap();
        let location = buf
            .meta
            .iter()
            .find(|e| e.key == "resp.header.Location")
            .map(|e| e.value.as_str())
            .unwrap_or("");
        assert_eq!(location, "/b/userportal/");
    }

    struct AllEnabled;
    impl FeatureConfig for AllEnabled {
        fn is_block_enabled(&self, _: &str) -> bool {
            true
        }
    }

    struct NoneEnabled;
    impl FeatureConfig for NoneEnabled {
        fn is_block_enabled(&self, _: &str) -> bool {
            false
        }
    }

    /// The block names every built-in route feature-gates against. The
    /// `route_to_block` feature gate calls `features.is_block_enabled(route.block)`.
    const GATED_BLOCKS: &[&str] = &[
        "impresspress/auth-ui",
        "impresspress/admin",
        "impresspress/files",
        "impresspress/products",
        "impresspress/legalpages",
        "impresspress/userportal",
    ];

    #[tokio::test]
    async fn extra_routes_honor_the_feature_gate() {
        use async_trait::async_trait;
        use wafer_run::{Block as RunBlock, BlockCategory, BlockInfo, LifecycleEvent, WaferError};

        use crate::test_support::{anon_msg, TestContext};

        struct EchoBlock;
        #[async_trait]
        impl RunBlock for EchoBlock {
            fn info(&self) -> BlockInfo {
                BlockInfo::new("test/extra", "0.0.1", "echo@v1", "extra route target")
                    .category(BlockCategory::Service)
            }
            async fn handle(
                &self,
                _ctx: &dyn Context,
                _msg: Message,
                _input: InputStream,
            ) -> OutputStream {
                crate::http::ResponseBuilder::new()
                    .status(200)
                    .body(b"DISPATCHED".to_vec(), "text/plain")
            }
            async fn lifecycle(
                &self,
                _ctx: &dyn Context,
                _e: LifecycleEvent,
            ) -> Result<(), WaferError> {
                Ok(())
            }
        }

        async fn dispatched(features: &dyn FeatureConfig) -> bool {
            let mut ctx = TestContext::new().await;
            ctx.register_block("test/extra", std::sync::Arc::new(EchoBlock));
            let extra = vec![ExtraRoute {
                prefix: "/x/extra".to_string(),
                access: RouteAccess::Public,
                block_name: "test/extra".to_string(),
            }];
            let out = route_to_block(
                &ctx,
                anon_msg("retrieve", "/x/extra/thing"),
                InputStream::empty(),
                features,
                &[],
                &extra,
            )
            .await;
            out.collect_buffered()
                .await
                .map(|b| b.body == b"DISPATCHED")
                .unwrap_or(false)
        }

        // Enabled → dispatched; disabled → feature-gated (NOT dispatched), the
        // gap this fix closes for downstream-registered routes.
        assert!(
            dispatched(&AllEnabled).await,
            "enabled extra route should dispatch"
        );
        assert!(
            !dispatched(&NoneEnabled).await,
            "disabled extra route must be feature-gated, not dispatched"
        );
    }

    #[test]
    fn feature_gating_all_enabled() {
        let all = AllEnabled;
        for block in GATED_BLOCKS {
            assert!(all.is_block_enabled(block), "{block} should be enabled");
        }
    }

    #[test]
    fn feature_gating_all_disabled() {
        let none = NoneEnabled;
        for block in GATED_BLOCKS {
            assert!(!none.is_block_enabled(block), "{block} should be disabled");
        }
    }

    #[test]
    fn legalpages_admin_routes_require_admin() {
        let admin_route = ROUTES
            .iter()
            .find(|r| r.prefix == "/b/legalpages/admin")
            .expect("legalpages admin route not declared");
        assert_eq!(
            admin_route.access,
            RouteAccess::Admin,
            "/b/legalpages/admin must require admin"
        );
        assert_eq!(admin_route.block, "impresspress/legalpages");

        let api_route = ROUTES
            .iter()
            .find(|r| r.prefix == "/b/legalpages/api")
            .expect("legalpages api route not declared");
        assert_eq!(
            api_route.access,
            RouteAccess::Admin,
            "/b/legalpages/api must require admin"
        );

        let public_route = ROUTES
            .iter()
            .find(|r| r.prefix == "/b/legalpages")
            .expect("public legalpages route not declared");
        assert_ne!(
            public_route.access,
            RouteAccess::Admin,
            "/b/legalpages must remain public"
        );

        // Most-specific-first ordering matters for the `starts_with` matcher.
        let positions: Vec<_> = ROUTES
            .iter()
            .enumerate()
            .filter(|(_, r)| r.block == "impresspress/legalpages")
            .map(|(i, r)| (i, r.prefix))
            .collect();
        assert_eq!(
            positions.iter().map(|(_, p)| *p).collect::<Vec<_>>(),
            vec!["/b/legalpages/admin", "/b/legalpages/api", "/b/legalpages"],
            "legalpages routes must be ordered most-specific-first",
        );
    }

    #[test]
    fn all_block_routes_are_under_b_prefix() {
        for route in ROUTES {
            let is_system = route.block == "impresspress/system";
            if !is_system {
                assert!(
                    route.prefix.starts_with("/b/"),
                    "block route {} should start with /b/",
                    route.prefix
                );
            }
        }
    }

    #[test]
    fn inspector_dispatch_diverges_from_block_name() {
        // The inspector is the one route whose dispatch target differs from its
        // feature/display name: gated as `impresspress/inspector`, dispatched to
        // the `wafer-run/inspector` runtime block.
        let inspector = ROUTES
            .iter()
            .find(|r| r.prefix == "/b/inspector")
            .expect("inspector route not declared");
        assert_eq!(inspector.block, "impresspress/inspector");
        assert_eq!(inspector.dispatch_to, "wafer-run/inspector");
    }

    #[test]
    fn only_inspector_has_a_dispatch_override() {
        // Every other route dispatches to its own block name (the `new`
        // constructor's invariant). Catches a stray `proxy` entry.
        for route in ROUTES {
            if route.prefix == "/b/inspector" {
                continue;
            }
            assert_eq!(
                route.dispatch_to, route.block,
                "route {} should dispatch to its own block",
                route.prefix
            );
        }
    }

    #[test]
    fn routes_config_uses_display_block_name_for_inspector() {
        // routes_config() must show the inspector as `impresspress/inspector`
        // (the display/feature name), not its `wafer-run/inspector` dispatch
        // target — the inspector UI keys its feature map on the former.
        let cfg = super::routes_config(&[]);
        let routes = cfg["routes"].as_array().expect("routes array");
        let inspector = routes
            .iter()
            .find(|r| r["path"] == "/b/inspector**")
            .expect("inspector route in config");
        assert_eq!(inspector["block"], "impresspress/inspector");
    }

    #[test]
    fn routes_config_sources_endpoint_granularity_from_block_infos() {
        use wafer_run::{AuthLevel, BlockEndpoint, BlockInfo};
        // A block that owns a built-in prefix route ("/b/vector/") contributes
        // its declared endpoints to the inspector view even though the route
        // table has a single collapsed prefix entry.
        let info = BlockInfo::new("impresspress/vector", "0.0.1", "http-handler@v1", "v")
            .endpoints(vec![
                BlockEndpoint::post("/b/vector/api/query").auth(AuthLevel::Authenticated),
                BlockEndpoint::get("/b/vector/").auth(AuthLevel::Admin),
            ]);
        let cfg = super::routes_config(std::slice::from_ref(&info));
        let routes = cfg["routes"].as_array().expect("routes array");
        // The collapsed prefix entry is present.
        assert!(routes.iter().any(|r| r["path"] == "/b/vector/**"));
        // And the per-endpoint granularity is sourced from info().endpoints.
        let query = routes
            .iter()
            .find(|r| r["path"] == "/b/vector/api/query")
            .expect("endpoint-sourced query route");
        assert_eq!(query["method"], "POST");
        assert_eq!(query["auth"], "authenticated");
        assert_eq!(query["block"], "impresspress/vector");
    }

    // -----------------------------------------------------------------------
    // Fail-open fix: undeclared paths under a Public-tier prefix must NOT
    // default to Public (code review 2026-07-16, "route declarations fail
    // open").
    // -----------------------------------------------------------------------

    #[test]
    fn declared_access_defaults_undeclared_path_to_authenticated_not_public() {
        use wafer_run::{AuthLevel, BlockEndpoint, BlockInfo};

        let info = BlockInfo::new("test/block", "0.0.1", "http-handler@v1", "t").endpoints(vec![
            BlockEndpoint::get("/b/test/declared").auth(AuthLevel::Public),
        ]);
        let msg = crate::test_support::anon_msg("retrieve", "/b/test/totally-undeclared");

        assert_eq!(
            declared_access(std::slice::from_ref(&info), "test/block", &msg),
            RouteAccess::Authenticated,
            "an undeclared path must fall back to Authenticated, not Public"
        );
        // A declared path is unaffected — still resolves to its own level.
        let declared_msg = crate::test_support::anon_msg("retrieve", "/b/test/declared");
        assert_eq!(
            declared_access(std::slice::from_ref(&info), "test/block", &declared_msg),
            RouteAccess::Public
        );
    }

    #[test]
    fn declared_access_defaults_to_authenticated_when_block_has_no_info_at_all() {
        let msg = crate::test_support::anon_msg("retrieve", "/b/unregistered/anything");
        assert_eq!(
            declared_access(&[], "test/block-not-registered", &msg),
            RouteAccess::Authenticated
        );
    }

    #[tokio::test]
    async fn undeclared_path_under_public_prefix_is_not_publicly_reachable() {
        use crate::test_support::{anon_msg, TestContext};

        let ctx = TestContext::new().await;
        // `impresspress/vector` owns a real Public-tier prefix route
        // (`/b/vector/`) but this BlockInfo declares no endpoints at all —
        // simulating a forgotten declaration for a brand-new handler.
        let block_infos = vec![wafer_run::BlockInfo::new(
            "impresspress/vector",
            "0.0.1",
            "http-handler@v1",
            "t",
        )];

        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", "/b/vector/some/undeclared/path"),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "an anonymous caller must be denied on an undeclared path, not dispatched"
        );
    }

    #[tokio::test]
    async fn undeclared_path_under_public_prefix_is_reachable_once_authenticated() {
        use crate::test_support::{auth_msg, TestContext};

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/vector",
            std::sync::Arc::new(DispatchProbeBlock),
        );
        let block_infos = vec![wafer_run::BlockInfo::new(
            "impresspress/vector",
            "0.0.1",
            "http-handler@v1",
            "t",
        )];

        let out = route_to_block(
            &ctx,
            auth_msg("retrieve", "/b/vector/some/undeclared/path", "user_1"),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        let buf = out
            .collect_buffered()
            .await
            .expect("a logged-in caller must reach dispatch on an undeclared path (Authenticated, not a hard deny)");
        assert_eq!(buf.body, b"DISPATCHED");
    }

    #[tokio::test]
    async fn anonymous_static_asset_request_is_not_denied() {
        use crate::test_support::{anon_msg, TestContext};

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/system",
            std::sync::Arc::new(DispatchProbeBlock),
        );

        // No `BlockInfo` passed at all — proves the fix doesn't depend on
        // `declared_access` matching. `SystemBlock::info().endpoints` uses
        // MID-SEGMENT hash placeholders (`/b/static/app-{hash}.css`) that
        // `match_template` can't bind to a real path anyway (see C1); the
        // `router_declared_public` route must keep this reachable regardless.
        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", "/b/static/app-abc123.css"),
            InputStream::empty(),
            &AllEnabled,
            &[],
            &[],
        )
        .await;
        let buf = out.collect_buffered().await.expect(
            "an anonymous caller must reach dispatch for a static asset — \
             the logged-out login/signup pages depend on this for CSS/JS/fonts/logo",
        );
        assert_eq!(buf.body, b"DISPATCHED");
    }

    #[test]
    fn static_prefix_route_is_router_declared_public() {
        // Direct check on the ROUTES entry itself (companion to the dispatch
        // test above): the static prefix must use the `router_final` escape
        // hatch, not a plain `Route::new(_, Public, _)`, or the fail-closed
        // `declared_access` default would still win via `RouteAccess::max`.
        let static_route = ROUTES
            .iter()
            .find(|r| r.prefix == STATIC_PREFIX)
            .expect("static prefix route not declared");
        assert_eq!(static_route.access, RouteAccess::Public);
        assert!(
            static_route.router_final,
            "/b/static/ must be router_final so the Authenticated default can't override it"
        );
    }

    #[tokio::test]
    async fn stripe_webhook_carveout_stays_reachable_with_no_session() {
        use crate::test_support::{anon_msg, TestContext};

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/products",
            std::sync::Arc::new(DispatchProbeBlock),
        );

        // No BlockInfo passed at all — `router_declared_public` routes never
        // consult `declared_access`, so this must dispatch regardless.
        let out = route_to_block(
            &ctx,
            anon_msg("create", "/b/products/webhooks"),
            InputStream::empty(),
            &AllEnabled,
            &[],
            &[],
        )
        .await;
        let buf = out
            .collect_buffered()
            .await
            .expect("the Stripe webhook path must stay reachable with no session");
        assert_eq!(buf.body, b"DISPATCHED");
    }

    #[tokio::test]
    async fn undeclared_products_path_other_than_the_webhook_carveout_requires_auth() {
        use crate::test_support::{anon_msg, TestContext};

        let ctx = TestContext::new().await;
        let block_infos = vec![wafer_run::BlockInfo::new(
            "impresspress/products",
            "0.0.1",
            "http-handler@v1",
            "t",
        )];

        // Same general `/b/products` prefix as the webhook carve-out, but NOT
        // one of the router-declared-public paths — must still require auth.
        // Proves the carve-out is narrow, not a reopening of the whole prefix.
        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", "/b/products/some-made-up-undeclared-path"),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        assert!(crate::test_support::output_is_error(out, "PermissionDenied").await);
    }

    #[test]
    fn router_declared_public_routes_precede_their_general_prefix() {
        // Most-specific-first ordering matters for the `starts_with` matcher
        // (same discipline as the legalpages admin/api-before-bare routes).
        let router_declared_public_prefixes = [
            "/b/auth/oauth/callback",
            "/b/auth/api/oauth/sync-user",
            "/b/auth/api/oauth/providers",
            "/b/auth/reset-password",
            "/b/auth/api/reset-password",
            "/b/auth/api/forgot-password",
            "/b/auth/api/verify",
            "/b/auth/api/resend-verification",
            "/b/products/webhooks",
        ];
        for prefix in router_declared_public_prefixes {
            let carveout_pos = ROUTES
                .iter()
                .position(|r| r.prefix == prefix)
                .unwrap_or_else(|| panic!("router_declared_public route {prefix} not found"));
            let general_prefix = if prefix.starts_with("/b/auth/") {
                "/b/auth/"
            } else {
                "/b/products"
            };
            let general_pos = ROUTES
                .iter()
                .position(|r| r.prefix == general_prefix)
                .unwrap_or_else(|| panic!("general route {general_prefix} not found"));
            assert!(
                carveout_pos < general_pos,
                "{prefix} (at {carveout_pos}) must precede {general_prefix} (at {general_pos})"
            );
            assert_eq!(
                ROUTES[carveout_pos].access,
                RouteAccess::Public,
                "{prefix} must be Public"
            );
            assert!(
                ROUTES[carveout_pos].router_final,
                "{prefix} must be router_final so the Authenticated default can't override it"
            );
        }
    }

    /// Shared dummy block for the tests above: always dispatches successfully
    /// with a recognizable body, so a test can prove "reached dispatch"
    /// rather than merely "wasn't denied".
    struct DispatchProbeBlock;
    #[async_trait::async_trait]
    impl wafer_run::Block for DispatchProbeBlock {
        fn info(&self) -> wafer_run::BlockInfo {
            wafer_run::BlockInfo::new("test/dispatch-probe", "0.0.1", "echo@v1", "dispatch probe")
                .category(wafer_run::BlockCategory::Service)
        }
        async fn handle(
            &self,
            _ctx: &dyn Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            crate::http::ResponseBuilder::new()
                .status(200)
                .body(b"DISPATCHED".to_vec(), "text/plain")
        }
        async fn lifecycle(
            &self,
            _ctx: &dyn Context,
            _e: wafer_run::LifecycleEvent,
        ) -> Result<(), wafer_run::WaferError> {
            Ok(())
        }
    }
}
