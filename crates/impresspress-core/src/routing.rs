//! Shared routing table — maps URL path prefixes to impresspress blocks.
//!
//! Both Cloudflare and native adapters use this same routing logic.
//! All impresspress blocks are registered in the Wafer registry at boot; routing
//! dispatches via `ctx.call_block` without any factory indirection.

use wafer_run::{
    context::Context, AuthLevel, BlockEndpoint, BlockInfo, InputStream, Message, OutputStream,
};

use crate::{config_vars::HAS_LANDING_PAGE_KEY, endpoint_match, features::FeatureConfig};

/// URL prefix for embedded static assets, served by `impresspress/system`.
///
/// Single source of truth shared by the routing table below, the
/// `ui::assets` URL builders, and the pipeline's request-log noise filter —
/// so the prefix can't drift between them (a stale `/static/` literal in the
/// filter once made every asset request write a `request_logs` row).
pub const STATIC_PREFIX: &str = "/b/static/";

/// A single route entry: the coarse access floor for one block's prefix.
///
/// `block` is the impresspress block name (`{org}/{block}`) used for feature-gating
/// and the inspector's [`routes_config`] view. `dispatch_to` is the Wafer block
/// name passed to `ctx.call_block`; it equals `block` for every route except the
/// inspector, which is feature-gated/displayed as `impresspress/inspector` but
/// dispatches to the `wafer-run/inspector` runtime block.
///
/// `access` is a floor, never the whole decision: [`route_to_block`] enforces
/// `access.max(declared_access(..))`, so the block's declared per-endpoint
/// [`AuthLevel`] can only strengthen it, and an *undeclared* path under the
/// route falls back to [`declared_access`]'s fail-closed default
/// (`Authenticated`) rather than to this route's own (possibly looser)
/// `access`. A path that must be reachable with no session (a signed
/// webhook, an OAuth provider callback, a password-reset link, a
/// content-hashed asset) is public because its block declares it
/// `AuthLevel::Public`; the router carries no per-path override.
pub struct Route {
    pub prefix: &'static str,
    pub access: RouteAccess,
    pub block: &'static str,
    pub dispatch_to: &'static str,
}

impl Route {
    /// A route whose dispatch target equals its block name (the common case).
    const fn new(prefix: &'static str, access: RouteAccess, block: &'static str) -> Route {
        Route {
            prefix,
            access,
            block,
            dispatch_to: block,
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
    /// `msg.user_id()` must be non-empty. A request with no identity is sent
    /// to login if it is a browser page and refused `401` otherwise.
    Authenticated,
    /// User must have the `admin` role (per [`crate::util::is_admin`]): no
    /// identity is answered as for `Authenticated`, an identity without the
    /// role is `403`.
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
///
/// # Access
///
/// A *declared* path — one `BlockEndpoint` matches — is always
/// `access.max(declared)`, exactly as a built-in [`Route`]'s is: neither the
/// prefix tier nor the block's own declaration can weaken the other.
///
/// What the two constructors decide is the tier for an **undeclared** path,
/// and it is a property of the *route*, not of how many endpoints its block
/// happens to declare:
///
/// * [`ExtraRoute::new`] — `access` is the complete answer. This is what
///   `ImpresspressBuilder::add_route` registers, because a downstream
///   project's catch-all block serves paths it never declared and a
///   fail-closed default would lock every one of them.
/// * [`ExtraRoute::refined`] — an undeclared path falls back to
///   [`RouteAccess::Authenticated`], the same fail-closed default
///   [`declared_access`] applies to built-in routes. This is what the dev
///   sandbox registers its dynamically-added guest blocks with.
///
/// # Why not "does the block declare any endpoint?"
///
/// That was the original switch, and it is wrong in both directions.
///
/// Fail-open: a guest block that declares NO endpoint is indistinguishable
/// from a catch-all, so every path under its `Public` prefix was served to
/// anonymous callers — the exact hole the per-endpoint refinement exists to
/// close. (`validate_static` now also refuses such a guest, but the routing
/// layer must not depend on that: the seed importer admits specs without a
/// `BlockInfo` in hand.)
///
/// Fail-closed-by-surprise: adding a FIRST `BlockEndpoint` to an existing
/// catch-all block flipped every *other* path that block serves from `access`
/// to `Authenticated`, silently, as a side effect of declaring one endpoint.
///
/// Making it an explicit per-route choice is what lets both of those be right
/// at once.
#[derive(Debug, Clone)]
pub struct ExtraRoute {
    pub prefix: String,
    pub access: RouteAccess,
    pub block_name: String,
    /// Whether an undeclared path under `prefix` falls back to
    /// [`RouteAccess::Authenticated`] rather than standing at `access`.
    ///
    /// Private so the choice is made through [`Self::new`] or
    /// [`Self::refined`], whose names say which semantics the registrar
    /// wanted.
    refine_undeclared: bool,
}

impl ExtraRoute {
    /// A route whose `access` is the complete answer for any path its block
    /// has not declared an endpoint for.
    ///
    /// The catch-all case, and what `ImpresspressBuilder::add_route` uses.
    pub fn new(
        prefix: impl Into<String>,
        block_name: impl Into<String>,
        access: RouteAccess,
    ) -> ExtraRoute {
        ExtraRoute {
            prefix: prefix.into(),
            access,
            block_name: block_name.into(),
            refine_undeclared: false,
        }
    }

    /// A route whose `access` is a FLOOR: an undeclared path under it
    /// requires a logged-in caller.
    ///
    /// For a block whose endpoint declarations are the authorization model —
    /// the dev sandbox's guest blocks, which are compiled from source the
    /// host did not write and are admitted on a `Public` prefix precisely
    /// because every genuinely public path has to be declared as one.
    pub fn refined(
        prefix: impl Into<String>,
        block_name: impl Into<String>,
        access: RouteAccess,
    ) -> ExtraRoute {
        ExtraRoute {
            prefix: prefix.into(),
            access,
            block_name: block_name.into(),
            refine_undeclared: true,
        }
    }

    /// Whether an undeclared path under this route falls back to
    /// [`RouteAccess::Authenticated`].
    ///
    /// Exposed within the crate so deployment-plan export can round-trip the
    /// exact route policy.
    pub(crate) const fn refines_undeclared(&self) -> bool {
        self.refine_undeclared
    }
}

/// The shared routing table: one coarse prefix per block, plus the inspector
/// proxy.
///
/// Hand-written on purpose — the inspector proxy, `/health` outside `/b/`,
/// files' two prefixes and the slash-suffixed spellings are decisions a
/// derivation would have to encode anyway — and kept honest by two tests:
/// every declared endpoint of every block in `blocks::all_block_infos()` sits
/// under an entry naming its block and every entry's block declares under
/// it; and no entry's prefix is served by another's, so order does not
/// matter.
///
/// Each entry's `access` is the floor for its prefix; the per-path level is
/// the block's own `BlockEndpoint` declaration (see [`declared_access`]).
/// There is no per-path entry: a path a block wants public, admin-only or
/// session-less is declared so in the block's table.
///
/// All block routes live under `/b/{block_name}/...`. SSR pages and JSON API
/// share the same prefix — blocks distinguish by HTTP method and path.
/// `/health` is the only route outside `/b/`.
pub const ROUTES: &[Route] = &[
    // System: the health probe, and the content-hashed, immutable,
    // session-less static assets (CSS/JS/fonts/logo for the logged-out
    // login/signup pages). `SystemBlock::info()` declares
    // `GET /b/static/{filename}` public; that row is what admits an
    // anonymous asset request.
    Route::new("/health", RouteAccess::Public, "impresspress/system"),
    Route::new(STATIC_PREFIX, RouteAccess::Public, "impresspress/system"),
    // Inspector — runtime debugging UI (admin only). Feature-gated as
    // `impresspress/inspector` but dispatches to the `wafer-run/inspector` block.
    Route::proxy(
        "/b/inspector",
        RouteAccess::Admin,
        "impresspress/inspector",
        "wafer-run/inspector",
    ),
    // Auth — SSR pages + API under /b/auth/. The session-less paths (OAuth
    // callback, password reset, verification, provider list) are declared
    // public by the auth-ui block; each handler gates itself by a token or a
    // signature.
    Route::new("/b/auth/", RouteAccess::Public, "impresspress/auth-ui"),
    // Admin — SSR pages + API under /b/admin/, every row declared `admin` on
    // top of this tier. The bare `/b/admin` form is covered by
    // `route_prefix_matches` (like every other slash-suffixed prefix).
    Route::new("/b/admin/", RouteAccess::Admin, "impresspress/admin"),
    // Feature blocks — SSR + API under /b/{block}/
    Route::new("/b/storage/", RouteAccess::Public, "impresspress/files"),
    Route::new(
        "/b/cloudstorage/",
        RouteAccess::Public,
        "impresspress/files",
    ),
    // Products — storefront, seller and admin surfaces, and the Stripe
    // webhook, which the block declares public (verified by HMAC signature
    // in `stripe.rs::handle_webhook`, not by `msg.user_id()`).
    Route::new("/b/products", RouteAccess::Public, "impresspress/products"),
    // Tickets — three declared public intake routes plus declared admin UI/API.
    Route::new("/b/tickets", RouteAccess::Public, "impresspress/tickets"),
    // Legalpages — public reads (`/terms`, `/privacy`); every row under
    // `/admin` and `/api` is declared `admin`. The handlers do not re-check
    // `is_admin`, so those declarations are the gate.
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
    // Route is open; the tier comes from `LlmBlock::info().endpoints`:
    // admin for the UI pages, provider CRUD, model load/unload and the two
    // per-thread config WRITES; authenticated for chat (buffered and SSE),
    // the config read, the aggregated model list and the per-model status
    // probe.
    Route::new("/b/llm", RouteAccess::Public, "impresspress/llm"),
    // Vector — similarity search, hybrid retrieval, RAG ingestion.
    //
    // ONE prefix route. The previous nine decorative entries all shared the
    // same access tier (`Public`) and dispatch target, differing only in
    // path — pure duplication, since the block does its own per-method
    // path-param matching in `pages::route`. The per-endpoint access tier
    // now comes from `VectorBlock::info().endpoints` and is enforced
    // centrally via `declared_access` (every row → Admin: an index has no
    // owner column to scope a request to, so the JSON API is admin-only for
    // the same reason the pages are — see the `ROUTES` doc in that block),
    // so the coarse prefix tier is `Public` and the declared level refines
    // it. A guard test (`vector::access_tests`) keeps it that way. The
    // inspector sources endpoint granularity from the same
    // `info().endpoints` (see [`routes_config`]).
    Route::new("/b/vector/", RouteAccess::Public, "impresspress/vector"),
    // Signal — WebRTC room-code handshake for blockfarming's online play.
    // ONE public prefix, same reasoning as vector's: every declared endpoint
    // is `AuthLevel::Public` (the game has no account), enforced from
    // `SignalBlock::info().endpoints` via `declared_access`, and a guard
    // test (`signal::tests::every_signal_endpoint_is_public`) keeps it that
    // way. Abuse is bounded by per-IP rate limiting inside the block itself,
    // not by this route's tier.
    Route::new("/b/signal/", RouteAccess::Public, "impresspress/signal"),
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

/// The access tier an [`ExtraRoute`] actually enforces for `msg`.
///
/// Extra routes used to enforce `route.access` alone, which made
/// `BlockEndpoint::auth` documentation-only for every downstream-registered
/// block: a block reached through a `Public` extra route served its
/// `Admin`-declared endpoints to anonymous callers. The dev sandbox is
/// registered exactly that way, and its dynamically-added guest blocks
/// declare their own per-endpoint auth, so the gap was load-bearing.
///
/// A DECLARED path is `access.max(declared)` for every extra route, without
/// exception. Only the UNDECLARED case differs, and it is decided by which
/// [`ExtraRoute`] constructor the registrar used rather than by whether the
/// block happens to declare any endpoint at all — see [`ExtraRoute`]'s doc
/// comment for why that distinction is load-bearing in both directions.
fn extra_route_access(block_infos: &[BlockInfo], route: &ExtraRoute, msg: &Message) -> RouteAccess {
    match declared_endpoint_access(block_infos, &route.block_name, msg) {
        Some(declared) => route.access.max(declared),
        None if route.refines_undeclared() => route.access.max(RouteAccess::Authenticated),
        None => route.access,
    }
}

/// The tier `block_name` DECLARED for `(msg.action, msg.path)`, or `None`
/// when no endpoint matches (including when the block has no [`BlockInfo`]).
///
/// The undecorated answer: what the caller does with a `None` is the
/// caller's policy, and the two callers differ. [`declared_access`] folds it
/// into the fail-closed `Authenticated` default that governs built-in routes;
/// [`extra_route_access`] consults the route's own choice.
fn declared_endpoint_access(
    block_infos: &[BlockInfo],
    block_name: &str,
    msg: &Message,
) -> Option<RouteAccess> {
    let info = block_infos.iter().find(|i| i.name == block_name)?;
    endpoint_match::endpoint_auth(&info.endpoints, msg.action(), msg.path())
        .map(RouteAccess::from_auth_level)
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
/// `Public`): a block must explicitly declare a `BlockEndpoint` with
/// `AuthLevel::Public` for any path that is genuinely meant to have no
/// session; the router has no per-path override. `Authenticated`, not a
/// hard deny, so a forgotten declaration degrades to "please log in" rather
/// than 404ing a route that already works for logged-in callers.
fn declared_access(block_infos: &[BlockInfo], block_name: &str, msg: &Message) -> RouteAccess {
    declared_endpoint_access(block_infos, block_name, msg).unwrap_or(RouteAccess::Authenticated)
}

/// Resolve the [`AuthLevel`] a caller must actually have to invoke `ep`,
/// mirroring exactly what [`route_to_block`] enforces for it:
/// `route.access.max(declared)` for a built-in route, and the same max for
/// an extra route (see [`extra_route_access`]; its undeclared branch never
/// applies to a block's own endpoint).
///
/// Lives here (not in `pipeline.rs`, where the WebMCP manifest calls it) so
/// it sits beside [`ROUTES`], [`declared_access`] and [`check_access`], the
/// three things it has to agree with: a resolver that drifted from the
/// router would advertise tools the router rejects or hide tools it admits.
///
/// Unlike [`declared_access`] — called by `route_to_block` with a live
/// `Message` whose concrete path can match more than one declared endpoint
/// in the same block, taking the strictest of all matches — this is asked
/// about a single, already-known endpoint, so the "declared" half of the
/// max is `ep.auth` directly rather than a fresh path lookup.
///
/// # Route resolution mirrors the router, not the block name
///
/// The "prefix" half is resolved exactly the way [`route_to_block`] does:
/// the FIRST entry — [`ROUTES`] in table order, then `extra_routes` — whose
/// prefix matches `ep.path`. It is deliberately NOT "the first entry that
/// happens to name this block", because the router never asks that
/// question: it picks a route by path and then dispatches to
/// `route.dispatch_to`, which can differ from `route.block`. Filtering the
/// table by block name first made the inspector — `BlockInfo` name
/// `wafer-run/inspector`, route `block` name `impresspress/inspector` —
/// match no route at all and resolve to the fallback, while the router
/// enforced `Admin`: the exact raising-direction leak this resolver exists
/// to close.
///
/// A matched route is only accepted when it actually serves THIS block —
/// `route.block == block.name` (feature-gate/display name) or
/// `route.dispatch_to == block.name` (the `ctx.call_block` target). If the
/// route that owns the path dispatches somewhere else, this endpoint is
/// unreachable through it.
///
/// # Fails closed
///
/// Anything this function cannot positively resolve to a route serving
/// `block` yields the STRICTEST level ([`AuthLevel::Admin`]) — no route
/// matches the path, or the matching route belongs to another block. Such
/// an endpoint is unreachable (the router 404s it), so publishing its tool
/// name to anyone is pure recon surface; `Authenticated` would have
/// published it to every logged-in visitor.
///
/// Used by the WebMCP manifest (`pipeline.rs`) via
/// `wafer_core::discovery::generate_webmcp_report`, whose declared-auth-only
/// convenience form `generate_webmcp_declared_auth` filters on `ep.auth`
/// alone — which would advertise a tool the router still rejects whenever a
/// block declares an endpoint looser than the prefix tier it is actually
/// served under (recon surface: a tool name published to a caller who can
/// never invoke it).
pub fn effective_access(
    block: &BlockInfo,
    ep: &BlockEndpoint,
    extra_routes: &[ExtraRoute],
) -> AuthLevel {
    // The same matcher `route_to_block` uses, bare form of a slash-suffixed
    // prefix included, so the resolver can never disagree with the router
    // about which entry serves a declared path.
    let prefix_matches = |prefix: &str| route_prefix_matches(prefix, &ep.path);

    // Built-in `ROUTES` win on prefix collision and are searched first —
    // same order, same matching, as `route_to_block`.
    let access = match ROUTES.iter().find(|r| prefix_matches(r.prefix)) {
        Some(r) if r.block != block.name && r.dispatch_to != block.name => {
            // The router serves this path from a different block, so this
            // endpoint is dead. Fail closed.
            RouteAccess::Admin
        }
        Some(r) => r.access.max(RouteAccess::from_auth_level(ep.auth)),
        // No built-in route claims this path — fall through to the
        // downstream-registered ones, exactly as `route_to_block` does.
        None => match extra_routes.iter().find(|r| prefix_matches(&r.prefix)) {
            // Mirrors `extra_route_access`'s DECLARED branch, which is the
            // only one reachable from here: `ep` is one of this block's own
            // endpoints, so a request for `ep.path` necessarily matches a
            // declaration and `refines_undeclared` never comes into it.
            // Dropping the refinement would advertise a tool the router now
            // rejects; adding one the router does not apply would hide a tool
            // it admits.
            Some(r) if r.block_name == block.name => {
                r.access.max(RouteAccess::from_auth_level(ep.auth))
            }
            _ => RouteAccess::Admin,
        },
    };

    match access {
        RouteAccess::Public => AuthLevel::Public,
        RouteAccess::Authenticated => AuthLevel::Authenticated,
        RouteAccess::Admin => AuthLevel::Admin,
    }
}

/// The block name [`route_to_block`]'s feature gate actually consults for
/// a block whose [`BlockInfo`] is named `block_name`.
///
/// For every block but one these are the same string. The inspector is the
/// exception: its `BlockInfo` is named `wafer-run/inspector` (it is the
/// runtime's own block) while the router gates and displays it as
/// `impresspress/inspector` (`Route::proxy`). Asking
/// `FeatureConfig::is_block_enabled("wafer-run/inspector")` would therefore
/// always answer "enabled" — unknown names default to enabled — even with
/// the admin toggle off.
///
/// Used by the WebMCP manifest (`pipeline.rs`), which must not advertise
/// tools from a block the router 404s.
/// Meta key by which [`route_to_block`] hands the block it dispatches the
/// gate decision it just made.
///
/// Same shape as `auth.user_roles`: an upstream layer publishes what it
/// decided, so a downstream handler never re-derives it from a source that can
/// disagree. It exists because the gate's source — the router's
/// `Arc<dyn FeatureConfig>` — is reachable only by constructor injection, and
/// `ui::shell_page` alone has 39 call sites across ten blocks. Those blocks
/// cannot read the gate; they can read the request.
///
/// The value is a JSON array of the block names this request's gate considers
/// OFF. `"[]"` when none — which is what separates "routed, nothing disabled"
/// from "never went through the router" (an absent key), since `get_meta`
/// answers both with an empty string otherwise.
pub const META_DISABLED_BLOCKS: &str = "routing.disabled_blocks";

/// The blocks `features` reports OFF, as the JSON [`META_DISABLED_BLOCKS`]
/// carries.
///
/// Derived by testing each registered block rather than read off a
/// `BlockSettings`, because the router holds `Arc<dyn FeatureConfig>` and the
/// trait's only method is the predicate. That is one in-memory map lookup per
/// registered block — and on any ordinary deployment the answer is `[]`.
fn disabled_blocks_json(features: &dyn FeatureConfig, block_infos: &[BlockInfo]) -> String {
    let disabled: Vec<&str> = block_infos
        .iter()
        .map(|b| feature_gate_name(&b.name))
        .filter(|gate| !features.is_block_enabled(gate))
        .collect();
    serde_json::to_string(&disabled).unwrap_or_else(|_| "[]".to_string())
}

/// The gate decision [`route_to_block`] published for this request.
///
/// For any handler that renders enablement-dependent UI — the sidebar, the
/// portal's feature list — so it shows exactly what the router will serve
/// rather than what the boot snapshot said. The snapshot is frozen at
/// `build()` (`RuntimeConfig::republish` needs `&mut Wafer`, which no handler
/// has), so once the admin toggle moves the router's live gate, a
/// snapshot-backed reader renders links the router has already begun 404ing.
///
/// Falls back to that snapshot when the key is absent — a message that did not
/// come through the router (a direct block call in a test, a lifecycle event).
/// Never falls back to "all enabled": that would fabricate an answer out of a
/// missing one, the failure `block_settings::load_and_seed` records a review
/// finding for.
pub fn gate_from_request(ctx: &dyn Context, msg: &Message) -> crate::features::BlockSettings {
    let raw = msg.get_meta(META_DISABLED_BLOCKS);
    if raw.is_empty() {
        return crate::features::BlockSettings::from_config_json(
            ctx.config_get(crate::features::BLOCK_SETTINGS_CONFIG_KEY)
                .unwrap_or("{}"),
        );
    }
    let disabled: Vec<String> = serde_json::from_str(raw).unwrap_or_default();
    crate::features::BlockSettings::from_map(disabled.into_iter().map(|n| (n, false)).collect())
}

pub fn feature_gate_name(block_name: &str) -> &str {
    ROUTES
        .iter()
        .find(|r| r.block == block_name || r.dispatch_to == block_name)
        .map_or(block_name, |r| r.block)
}

/// Enforce a route's [`RouteAccess`] tier against the request. Returns
/// `Some(refusal)` when the caller fails the tier, or `None` to proceed.
/// Shared by the built-in and extra-route dispatch loops.
fn check_access(access: RouteAccess, msg: &Message) -> Option<OutputStream> {
    match access {
        RouteAccess::Public => None,
        // Missing identity (anonymous OR stale session — crypto.rs leaves
        // `user_id` empty on any invalid token) → send browsers to login with a
        // return path; answer API callers 401 with a `WWW-Authenticate`
        // challenge. Both protected tiers share this: an `Admin` route hit
        // with no identity is a login problem, not a role problem.
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

/// Whether `path` belongs to the route registered under `prefix`.
///
/// Exact match, or prefix match, or -- the third arm -- the bare form of a
/// prefix that was registered WITH a trailing slash. Without that arm,
/// `/b/vector` 404'd while `/b/vector/` served the page, because
/// `"/b/vector".starts_with("/b/vector/")` is false. The same held for
/// `/b/storage` and `/b/auth`; `/b/admin` escaped it only because the table
/// carried a hand-written second entry for the bare form, which this arm now
/// makes unnecessary (that duplicate is removed).
///
/// The arm is deliberately narrow: it fires only when the path is exactly the
/// prefix minus its trailing slash, so it can never pull in a sibling like
/// `/b/vectors`.
fn route_prefix_matches(prefix: &str, path: &str) -> bool {
    path == prefix || path.starts_with(prefix) || prefix.strip_suffix('/') == Some(path)
}

/// Route a message to the appropriate impresspress block based on request path.
///
/// Checks the feature gate and the access tier, then dispatches via
/// `ctx.call_block` — every impresspress block is registered in the Wafer
/// registry at boot (`blocks::register_feature_blocks`, `register_llm`,
/// `register_auth`).
pub async fn route_to_block(
    ctx: &dyn Context,
    mut msg: Message,
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
        // Read through the config client, not the context snapshot. That
        // snapshot is frozen at boot, so an admin who set this key kept
        // getting the old routing until the process restarted — and on
        // Cloudflare the D1 row never reached that surface at all. Being the
        // only reader made the two surfaces impossible to catch disagreeing,
        // not impossible for them to disagree.
        //
        // The cost on this hot path is bounded: `blocks::config` serves reads
        // from a snapshot of the variables table memoized against the
        // config-write generation, so this is one query per write, not one
        // per request to `/`.
        let has_landing_page =
            match crate::config_vars::get_bool(ctx, HAS_LANDING_PAGE_KEY, false).await {
                Ok(has_landing_page) => has_landing_page,
                Err(e) => {
                    return crate::blocks::crud::db_error_internal(
                        e,
                        "routing: landing page switch read failed",
                    )
                }
            };
        if has_landing_page {
            return ctx.call_block("wafer-run/web", msg, input).await;
        }
        return root_redirect(msg.user_id().is_empty());
    }

    for route in ROUTES {
        if !route_prefix_matches(route.prefix, &path) {
            continue;
        }

        // Feature gate
        if !features.is_block_enabled(route.block) {
            return crate::http::err_not_found("endpoint not found");
        }

        // Access gate. The coarse prefix tier is a floor; if the target
        // block declares an endpoint matching this exact (action, path) we
        // also enforce that endpoint's declared `AuthLevel` — taking the
        // stricter of the two. This is what makes `BlockEndpoint::auth`
        // load-bearing instead of documentation-only, and lets blocks drop
        // their per-handler `is_admin`/`user_id` preambles. An UNDECLARED
        // path falls back to `Authenticated` (fail-closed).
        let access = route
            .access
            .max(declared_access(block_infos, route.block, &msg));
        if let Some(denied) = check_access(access, &msg) {
            return denied;
        }

        // Hand the block the gate decision just made, so any enablement-
        // dependent UI it renders agrees with what this router will serve.
        // See `META_DISABLED_BLOCKS`.
        msg.set_meta(
            META_DISABLED_BLOCKS,
            disabled_blocks_json(features, block_infos),
        );

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

        // Access gate, refined by the target block's own declarations exactly
        // as the built-in loop above does — see `extra_route_access`.
        if let Some(denied) = check_access(extra_route_access(block_infos, route, &msg), &msg) {
            return denied;
        }

        // Same stamp as the built-in loop above — a downstream-registered
        // route's pages render the same chrome.
        msg.set_meta(
            META_DISABLED_BLOCKS,
            disabled_blocks_json(features, block_infos),
        );

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
            ("/b/admin/settings/email", "impresspress/admin"),
            ("/b/storage/buckets", "impresspress/files"),
            ("/b/cloudstorage/shares", "impresspress/files"),
            ("/b/products", "impresspress/products"),
            ("/b/products/webhooks", "impresspress/products"),
            ("/b/tickets/submit", "impresspress/tickets"),
            ("/b/legalpages", "impresspress/legalpages"),
            ("/b/legalpages/admin/terms", "impresspress/legalpages"),
            ("/b/userportal", "impresspress/userportal"),
        ];

        for (path, expected_block) in cases {
            // Calls the router's own matcher rather than restating it: both
            // of these tests used to inline `path == prefix ||
            // starts_with(prefix)`, so they asserted against a private copy
            // of the logic and would keep passing while the real matcher
            // diverged from them.
            let matched = ROUTES.iter().find(|r| route_prefix_matches(r.prefix, path));
            assert!(matched.is_some(), "path {path} should match a route");
            assert_eq!(
                matched.unwrap().block,
                expected_block,
                "path {path} should route to {expected_block}"
            );
        }
    }

    #[test]
    fn bare_form_of_a_slash_suffixed_prefix_still_routes() {
        // `/b/vector` 404'd while `/b/vector/` worked, because the table
        // registers the prefix WITH a slash and `"/b/vector"
        // .starts_with("/b/vector/")` is false. `/b/storage` and `/b/auth`
        // had the same hole; `/b/admin` escaped it only via a duplicate entry.
        for (path, expected_block) in [
            ("/b/vector", "impresspress/vector"),
            ("/b/storage", "impresspress/files"),
            ("/b/admin", "impresspress/admin"),
        ] {
            let matched = ROUTES.iter().find(|r| route_prefix_matches(r.prefix, path));
            assert!(matched.is_some(), "bare path {path} should match a route");
            assert_eq!(matched.unwrap().block, expected_block, "for {path}");
        }
    }

    #[test]
    fn slash_tolerance_does_not_match_a_sibling_prefix() {
        // The bare-form arm must fire only on an exact prefix-minus-slash,
        // never on a longer name that merely shares the stem.
        assert!(route_prefix_matches("/b/vector/", "/b/vector"));
        assert!(!route_prefix_matches("/b/vector/", "/b/vectors"));
        assert!(!route_prefix_matches("/b/vector/", "/b/vect"));
        assert!(!route_prefix_matches("/b/vector/", "/b/vectorial/x"));
        // And it must not turn a non-prefix into a match.
        assert!(!route_prefix_matches("/b/admin/", "/b/other"));
        // A path that merely EXTENDS a built-in's bare form is not under it.
        // `/b/admin/` matches the bare `/b/admin` by equality, not by prefix,
        // so `/b/admins/` belongs to whoever claims it. `blocks::dev`'s
        // staging validation relies on this: it used to refuse the block name
        // `admins` on the router's behalf, back when `/b/admin` was a second
        // slash-less route that swallowed the name by `starts_with`.
        assert!(!route_prefix_matches("/b/admin/", "/b/admins/"));
        assert!(!route_prefix_matches("/b/admin/", "/b/admins"));
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
            let matched = ROUTES.iter().find(|r| route_prefix_matches(r.prefix, path));
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
        // The admin-only paths under these prefixes (legalpages' `/admin`
        // and `/api` rows, the tickets admin UI, ...) are gated by the
        // blocks' own `admin` declarations, not by the prefix tier.
        let non_admin_prefixes = [
            "/health",
            STATIC_PREFIX,
            "/b/auth/",
            "/b/storage/",
            "/b/products",
            "/b/tickets",
            "/b/legalpages",
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
        "impresspress/tickets",
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
            let extra = vec![ExtraRoute::new(
                "/x/extra",
                "test/extra",
                RouteAccess::Public,
            )];
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

    // -----------------------------------------------------------------------
    // `effective_access` — the WebMCP manifest's resolver (pipeline.rs) must
    // agree with what `route_to_block` actually admits, for each shape the
    // table and a declaration can combine into. Each test asserts the
    // resolver's verdict AND drives a real request through `route_to_block`
    // to confirm the router itself behaves the same way — if the two
    // disagree, the manifest is wrong by definition.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn effective_access_agrees_with_the_router_for_public_under_public() {
        use crate::test_support::{anon_msg, TestContext};

        let ep_path = "/b/products/catalog";
        let info = BlockInfo::new("impresspress/products", "0.0.1", "http-handler@v1", "t")
            .endpoints(vec![BlockEndpoint::get(ep_path).auth(AuthLevel::Public)]);
        let ep = info.endpoints[0].clone();

        assert_eq!(
            effective_access(&info, &ep, &[]),
            AuthLevel::Public,
            "a Public endpoint under the Public `/b/products` prefix stays Public"
        );

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/products",
            DispatchProbeBlock::named("impresspress/products"),
        );
        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", ep_path),
            InputStream::empty(),
            &AllEnabled,
            std::slice::from_ref(&info),
            &[],
        )
        .await;
        let buf = out.collect_buffered().await.expect(
            "router must actually admit an anonymous caller here, matching effective_access's Public verdict",
        );
        assert_eq!(buf.body, b"DISPATCHED");
    }

    #[tokio::test]
    async fn effective_access_agrees_with_the_router_for_public_endpoint_under_admin_prefix() {
        use crate::test_support::{anon_msg, TestContext};

        // The dangerous case this resolver exists for: a block endpoint
        // declares itself `Public` even though it lives under the
        // Admin-tier `/b/admin/` prefix. The router still enforces Admin
        // via `RouteAccess::max` (routing.rs:435-440) — `effective_access`
        // must agree, or the manifest would advertise this tool name to
        // anonymous callers the router then silently refuses.
        let ep_path = "/b/admin/misdeclared-report";
        let info = BlockInfo::new("impresspress/admin", "0.0.1", "http-handler@v1", "t")
            .endpoints(vec![BlockEndpoint::get(ep_path).auth(AuthLevel::Public)]);
        let ep = info.endpoints[0].clone();

        assert_eq!(
            effective_access(&info, &ep, &[]),
            AuthLevel::Admin,
            "the Admin prefix tier must win over a looser declared endpoint level"
        );

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/admin",
            DispatchProbeBlock::named("impresspress/admin"),
        );
        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", ep_path),
            InputStream::empty(),
            &AllEnabled,
            std::slice::from_ref(&info),
            &[],
        )
        .await;
        assert!(
            crate::test_support::output_is_error(out, "Unauthenticated").await,
            "the router must reject the anonymous caller despite the endpoint's Public declaration"
        );
    }

    /// Both directions on the static prefix, which used to be the one place
    /// a router entry overrode the declaration: the system block's real
    /// `GET /b/static/{filename}` public row under the Public prefix resolves
    /// `Public` and the router admits an anonymous caller; a stricter row
    /// declared under the same prefix resolves to that level and the router
    /// enforces it, because the max wins.
    #[tokio::test]
    async fn effective_access_agrees_with_the_router_for_a_declared_public_row_under_a_public_prefix(
    ) {
        use wafer_run::Block as _;

        use crate::{
            blocks::system::SystemBlock,
            test_support::{anon_msg, output_is_error, TestContext},
        };

        let info = SystemBlock::new().info();
        let asset_row = info
            .endpoints
            .iter()
            .find(|ep| ep.path == "/b/static/{filename}")
            .expect("the system block declares the asset row");
        assert_eq!(
            effective_access(&info, asset_row, &[]),
            AuthLevel::Public,
            "a Public row under the Public static prefix is Public"
        );

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/system",
            DispatchProbeBlock::named("impresspress/system"),
        );
        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", "/b/static/app-abc123.css"),
            InputStream::empty(),
            &AllEnabled,
            std::slice::from_ref(&info),
            &[],
        )
        .await;
        let buf = out.collect_buffered().await.expect(
            "the router must admit an anonymous asset request, matching the resolver's Public verdict",
        );
        assert_eq!(buf.body, b"DISPATCHED");

        let ep_path = "/b/static/secret-admin-only-thing";
        let strict = BlockInfo::new("impresspress/system", "0.0.1", "http-handler@v1", "t")
            .endpoints(vec![BlockEndpoint::get(ep_path).auth(AuthLevel::Admin)]);
        let ep = strict.endpoints[0].clone();
        assert_eq!(
            effective_access(&strict, &ep, &[]),
            AuthLevel::Admin,
            "a stricter declaration under a Public prefix is enforced, not overridden"
        );
        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", ep_path),
            InputStream::empty(),
            &AllEnabled,
            std::slice::from_ref(&strict),
            &[],
        )
        .await;
        assert!(
            output_is_error(out, "Unauthenticated").await,
            "the router must reject the anonymous caller, matching the resolver's Admin verdict"
        );
    }

    #[tokio::test]
    async fn effective_access_resolves_a_proxy_route_by_its_dispatch_target() {
        use crate::test_support::{auth_msg, TestContext};

        // The inspector is the one `Route::proxy` in the table: gated and
        // displayed as `impresspress/inspector`, but DISPATCHED to — and
        // named in its own `BlockInfo` as — `wafer-run/inspector`. Matching
        // the routing table on `route.block == block.name` therefore found
        // NOTHING for this block, and the old `None` fallback answered
        // `Authenticated` while the router enforces `Admin`: every
        // logged-in non-admin would have been advertised the inspector's
        // tools and 403d on every call.
        let ep_path = "/b/inspector/blocks";
        let info = BlockInfo::new("wafer-run/inspector", "0.0.1", "http-handler@v1", "t")
            .endpoints(vec![BlockEndpoint::get(ep_path).auth(AuthLevel::Public)]);
        let ep = info.endpoints[0].clone();

        assert_eq!(
            effective_access(&info, &ep, &[]),
            AuthLevel::Admin,
            "the proxy route's Admin tier must be found via `dispatch_to`, not just `block`"
        );

        // And the router really does enforce Admin here — for a caller who
        // IS logged in, which is the case the old `Authenticated` verdict
        // wrongly advertised to.
        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "wafer-run/inspector",
            DispatchProbeBlock::named("wafer-run/inspector"),
        );
        let out = route_to_block(
            &ctx,
            auth_msg("retrieve", ep_path, "user-1"),
            InputStream::empty(),
            &AllEnabled,
            std::slice::from_ref(&info),
            &[],
        )
        .await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "the router must reject a logged-in non-admin on the inspector route"
        );
    }

    #[tokio::test]
    async fn effective_access_fails_closed_when_no_route_serves_the_path() {
        use crate::test_support::{admin_msg, TestContext};

        // No `ROUTES` prefix and no extra route covers `/x/...`, so
        // `route_to_block` 404s it for EVERY caller. A tool name published
        // for an unreachable endpoint is pure recon surface, so the
        // strictest level is the only honest answer.
        let ep_path = "/x/orphaned";
        let info = BlockInfo::new("test/orphan", "0.0.1", "http-handler@v1", "t")
            .endpoints(vec![BlockEndpoint::get(ep_path).auth(AuthLevel::Public)]);
        let ep = info.endpoints[0].clone();

        assert_eq!(
            effective_access(&info, &ep, &[]),
            AuthLevel::Admin,
            "an endpoint no route serves must fail closed, not fall back to Authenticated"
        );

        // Unreachable in fact, not just in the resolver's opinion — even an
        // admin gets a 404.
        let mut ctx = TestContext::new().await;
        ctx.register_block("test/orphan", DispatchProbeBlock::named("test/orphan"));
        let out = route_to_block(
            &ctx,
            admin_msg("retrieve", ep_path),
            InputStream::empty(),
            &AllEnabled,
            std::slice::from_ref(&info),
            &[],
        )
        .await;
        assert!(
            crate::test_support::output_is_error(out, "NotFound").await,
            "no route serves this path — the router must 404 it, for an admin too"
        );
    }

    #[tokio::test]
    async fn effective_access_honors_a_stricter_extra_route() {
        use crate::test_support::{auth_msg, TestContext};

        // A downstream project's `ImpresspressBuilder::add_route` is
        // enforced by `route_to_block`'s second loop exactly like a
        // built-in. Before `extra_routes` was threaded through, this
        // endpoint matched nothing and resolved to the fallback — again
        // advertising an Admin-gated tool to every logged-in visitor.
        let ep_path = "/x/reports/summary";
        let info = BlockInfo::new("test/reports", "0.0.1", "http-handler@v1", "t")
            .endpoints(vec![BlockEndpoint::get(ep_path).auth(AuthLevel::Public)]);
        let ep = info.endpoints[0].clone();
        let extra = vec![ExtraRoute::new(
            "/x/reports",
            "test/reports",
            RouteAccess::Admin,
        )];

        assert_eq!(
            effective_access(&info, &ep, &extra),
            AuthLevel::Admin,
            "a downstream add_route(Admin) must win over the endpoint's Public declaration"
        );

        let mut ctx = TestContext::new().await;
        ctx.register_block("test/reports", DispatchProbeBlock::named("test/reports"));
        let out = route_to_block(
            &ctx,
            auth_msg("retrieve", ep_path, "user-1"),
            InputStream::empty(),
            &AllEnabled,
            std::slice::from_ref(&info),
            &extra,
        )
        .await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "the router must reject a logged-in non-admin on an Admin extra route"
        );
    }

    #[tokio::test]
    async fn an_extra_route_is_refined_by_the_target_blocks_own_declarations() {
        use crate::test_support::{anon_msg, TestContext};

        // The other direction, and the one that used to be a hole:
        // `route_to_block`'s extra-route loop enforced `route.access` alone,
        // so a block reached through a `Public` extra route served its
        // `Admin`-declared endpoints to anonymous callers. The dev sandbox is
        // registered exactly this way and its guest blocks declare their own
        // per-endpoint auth, so the gap was load-bearing.
        let ep_path = "/x/admin-thing";
        let info = BlockInfo::new("test/pub", "0.0.1", "http-handler@v1", "t")
            .endpoints(vec![BlockEndpoint::get(ep_path).auth(AuthLevel::Admin)]);
        let ep = info.endpoints[0].clone();
        let extra = vec![ExtraRoute::new("/x/", "test/pub", RouteAccess::Public)];

        assert_eq!(
            effective_access(&info, &ep, &extra),
            AuthLevel::Admin,
            "the resolver must report what the router now enforces"
        );

        // And the router agrees — the whole point of the resolver existing.
        let mut ctx = TestContext::new().await;
        ctx.register_block("test/pub", DispatchProbeBlock::named("test/pub"));
        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", ep_path),
            InputStream::empty(),
            &AllEnabled,
            std::slice::from_ref(&info),
            &extra,
        )
        .await;
        assert!(
            crate::test_support::output_is_error(out, "Unauthenticated").await,
            "an anonymous caller must not reach an Admin-declared endpoint through a Public \
             extra route"
        );
    }

    #[tokio::test]
    async fn an_extra_route_to_a_block_that_declares_nothing_keeps_its_own_tier() {
        use crate::test_support::{anon_msg, TestContext};

        // Every existing `ImpresspressBuilder::add_route` consumer registers a
        // catch-all block that declares no endpoints. `declared_access`'s
        // fail-closed `Authenticated` default would lock all of them, so the
        // refinement applies only to a block that has opted in by declaring
        // something.
        let info = BlockInfo::new("test/catchall", "0.0.1", "http-handler@v1", "t");
        let extra = vec![ExtraRoute::new("/x/", "test/catchall", RouteAccess::Public)];

        let mut ctx = TestContext::new().await;
        ctx.register_block("test/catchall", DispatchProbeBlock::named("test/catchall"));
        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", "/x/anything"),
            InputStream::empty(),
            &AllEnabled,
            std::slice::from_ref(&info),
            &extra,
        )
        .await;
        assert_eq!(
            crate::test_support::output_html(out).await,
            "DISPATCHED",
            "a block that declares no endpoints keeps its route's Public tier"
        );
    }

    #[test]
    fn feature_gate_name_maps_the_proxy_block_to_its_gated_name() {
        // The inspector is gated under its impresspress name even though its
        // `BlockInfo` carries the runtime name — an unmapped lookup would
        // hit `BlockSettings`' default-enabled branch and ignore the admin
        // toggle entirely.
        assert_eq!(
            feature_gate_name("wafer-run/inspector"),
            "impresspress/inspector"
        );
        assert_eq!(
            feature_gate_name("impresspress/inspector"),
            "impresspress/inspector"
        );
        assert_eq!(
            feature_gate_name("impresspress/products"),
            "impresspress/products"
        );
        // A block with no route at all is gated under its own name.
        assert_eq!(feature_gate_name("test/unrouted"), "test/unrouted");
    }

    /// The router hands the block it dispatches the gate it just applied.
    ///
    /// This is what lets `ui::shell_document` filter the sidebar on the same
    /// answer the gate gave — without the `Arc<dyn FeatureConfig>` it cannot
    /// reach (`ui::shell_page` alone has 39 call sites across ten blocks) and
    /// without a database read in the shared chrome path, which
    /// `llm::pages`' boundary test forbids outright.
    ///
    /// The regression it pins: once the admin toggle began moving the router's
    /// live gate, a sidebar reading the boot snapshot rendered links the
    /// router had already started answering with "endpoint not found".
    #[tokio::test]
    async fn the_router_hands_the_block_the_gate_it_just_applied() {
        use crate::test_support::{admin_msg, TestContext};

        struct TicketsOff;
        impl FeatureConfig for TicketsOff {
            fn is_block_enabled(&self, full_name: &str) -> bool {
                full_name != "impresspress/tickets"
            }
        }

        struct GateEchoBlock;
        #[async_trait::async_trait]
        impl wafer_run::Block for GateEchoBlock {
            fn info(&self) -> wafer_run::BlockInfo {
                // Stands in for the files block, so it reports that name.
                wafer_run::BlockInfo::new("impresspress/files", "0.0.1", "echo@v1", "gate echo")
                    .category(wafer_run::BlockCategory::Service)
            }
            async fn handle(
                &self,
                _ctx: &dyn Context,
                msg: Message,
                _input: InputStream,
            ) -> OutputStream {
                crate::http::ResponseBuilder::new().status(200).body(
                    msg.get_meta(META_DISABLED_BLOCKS).as_bytes().to_vec(),
                    "text/plain",
                )
            }
            async fn lifecycle(
                &self,
                _ctx: &dyn Context,
                _e: wafer_run::LifecycleEvent,
            ) -> Result<(), wafer_run::WaferError> {
                Ok(())
            }
        }

        let infos = vec![
            wafer_run::BlockInfo::new("impresspress/files", "0.0.1", "http-handler@v1", "files"),
            wafer_run::BlockInfo::new(
                "impresspress/tickets",
                "0.0.1",
                "http-handler@v1",
                "tickets",
            ),
        ];
        let mut ctx = TestContext::new().await;
        ctx.register_block("impresspress/files", std::sync::Arc::new(GateEchoBlock));

        // `/b/storage/` is served by `impresspress/files`, which is ENABLED
        // here — so the request dispatches, and what is asserted is what it
        // was told about the block that is not.
        //
        // Authenticated, not anonymous: these `BlockInfo`s declare no
        // endpoints, and `declared_access` answers an UNDECLARED path with
        // `Authenticated` (fail-closed), which outranks the route's own
        // Public tier. An anonymous caller never reaches dispatch.
        let out = route_to_block(
            &ctx,
            admin_msg("retrieve", "/b/storage/"),
            InputStream::empty(),
            &TicketsOff,
            &infos,
            &[],
        )
        .await;
        let buf = out.collect_buffered().await.expect("must dispatch");
        let stamped = String::from_utf8(buf.body).expect("utf-8 body");

        assert_eq!(
            stamped, r#"["impresspress/tickets"]"#,
            "the dispatched block must receive the gate the router applied",
        );
    }

    /// The property the sidebar rests on: what a reader recovers from the
    /// stamp is exactly what the router refused.
    #[tokio::test]
    async fn gate_from_request_reproduces_the_routers_answer() {
        use crate::test_support::{anon_msg, TestContext};

        let ctx = TestContext::new().await;
        let mut msg = anon_msg("retrieve", "/b/admin/");
        msg.set_meta(META_DISABLED_BLOCKS, r#"["impresspress/tickets"]"#);

        let gate = gate_from_request(&ctx, &msg);
        assert!(!gate.is_block_enabled("impresspress/tickets"));
        assert!(
            gate.is_block_enabled("impresspress/files"),
            "a block the router did not refuse stays enabled",
        );
    }

    /// A message that never went through the router — a direct block call in a
    /// test, a lifecycle event — falls back to the boot snapshot, and never to
    /// "everything enabled". Fabricating that out of a missing answer is the
    /// failure `block_settings::load_and_seed` records a review finding for.
    #[tokio::test]
    async fn gate_from_request_falls_back_to_the_boot_snapshot_when_unrouted() {
        use crate::test_support::{anon_msg, TestContext};

        let mut ctx = TestContext::new().await;
        ctx.set_config(
            crate::features::BLOCK_SETTINGS_CONFIG_KEY,
            &serde_json::json!({ "impresspress/tickets": { "enabled": false } }).to_string(),
        );

        let gate = gate_from_request(&ctx, &anon_msg("retrieve", "/b/admin/"));
        assert!(
            !gate.is_block_enabled("impresspress/tickets"),
            "an unrouted message must fall back to the snapshot, not to all-enabled",
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
            crate::test_support::output_is_error(out, "Unauthenticated").await,
            "an anonymous caller must be denied on an undeclared path, not dispatched"
        );
    }

    #[tokio::test]
    async fn undeclared_path_under_public_prefix_is_reachable_once_authenticated() {
        use crate::test_support::{auth_msg, TestContext};

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/vector",
            DispatchProbeBlock::named("impresspress/vector"),
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

    /// A block's `dispatch` serves `GET /b/llm` from the `/b/llm/` row (the
    /// matcher retries a bare index path with a trailing slash), and that row
    /// is declared `Admin`. The router must gate the bare form at the same
    /// level, not at the fail-closed `Authenticated` default it falls back to
    /// when no declaration matches.
    /// `effective_access` must resolve a route the way `route_to_block` does,
    /// including the bare form of a slash-suffixed prefix (`/b/vector` for
    /// the `/b/vector/` entry): a row declared at that bare path is served by
    /// the router, so the resolver must not fail closed to `Admin` and hide
    /// the tool the router admits.
    #[test]
    fn effective_access_matches_the_bare_form_of_a_slash_suffixed_prefix() {
        let info =
            wafer_run::BlockInfo::new("impresspress/vector", "0.0.1", "http-handler@v1", "t")
                .endpoints(vec![
                    wafer_run::BlockEndpoint::get("/b/vector").auth(AuthLevel::Public)
                ]);
        let ep = &info.endpoints[0];
        assert_eq!(effective_access(&info, ep, &[]), AuthLevel::Public);
    }

    #[tokio::test]
    async fn bare_index_path_is_gated_at_the_declared_level() {
        use crate::test_support::{auth_msg, TestContext};

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/llm",
            DispatchProbeBlock::named("impresspress/llm"),
        );
        let block_infos =
            vec![
                wafer_run::BlockInfo::new("impresspress/llm", "0.0.1", "http-handler@v1", "t")
                    .endpoints(vec![
                        wafer_run::BlockEndpoint::get("/b/llm/").auth(AuthLevel::Admin)
                    ]),
            ];

        let out = route_to_block(
            &ctx,
            auth_msg("retrieve", "/b/llm", "user_1"),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        assert!(
            crate::test_support::output_is_error(out, "PermissionDenied").await,
            "a logged-in non-admin must not reach the admin chat page through the bare index path"
        );
    }

    /// An anonymous asset request reaches dispatch from the system block's
    /// own declaration: `SystemBlock::info()` declares
    /// `GET /b/static/{filename}` public, and `declared_access` resolves the
    /// bound filename to that row. Driven with the real `info()`, so a
    /// router entry that merely restates it is redundant.
    #[tokio::test]
    async fn anonymous_static_asset_request_is_not_denied() {
        use wafer_run::Block as _;

        use crate::{
            blocks::system::SystemBlock,
            test_support::{anon_msg, TestContext},
        };

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/system",
            DispatchProbeBlock::named("impresspress/system"),
        );
        let block_infos = vec![SystemBlock::new().info()];

        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", "/b/static/app-abc123.css"),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        let buf = out.collect_buffered().await.expect(
            "an anonymous caller must reach dispatch for a static asset — \
             the logged-out login/signup pages depend on this for CSS/JS/fonts/logo",
        );
        assert_eq!(buf.body, b"DISPATCHED");
    }

    #[tokio::test]
    async fn webmcp_script_asset_is_publicly_reachable() {
        use wafer_run::Block as _;

        use crate::{
            blocks::system::SystemBlock,
            test_support::{anon_msg, TestContext},
        };

        // The WebMCP registration script (`crates/impresspress-core/src/ui/
        // assets/webmcp.js`, served under `/b/static/`) must load for
        // anonymous visitors, or tools silently never register on the
        // public storefront. Drive a real anonymous request through
        // `route_to_block` against the actual URL the page embeds
        // (`assets::webmcp_js_url()`) and the system block's real
        // declaration, the same way
        // `anonymous_static_asset_request_is_not_denied` proves this for the
        // static prefix generally.
        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/system",
            DispatchProbeBlock::named("impresspress/system"),
        );
        let block_infos = vec![SystemBlock::new().info()];

        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", &crate::ui::assets::webmcp_js_url()),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        let buf = out.collect_buffered().await.expect(
            "an anonymous caller must reach dispatch for the WebMCP script — \
             an undeclared or misrouted static path fails closed to \
             Authenticated and would silently disable tools on the public \
             storefront",
        );
        assert_eq!(buf.body, b"DISPATCHED");
    }

    /// The Stripe webhook is verified by HMAC signature inside its handler,
    /// not by a session. The products block declares
    /// `POST /b/products/webhooks` public and the router reads that
    /// declaration; driven with the block's real `info()`, an anonymous
    /// POST reaches dispatch.
    // Driven with `ProductsBlock::new().info()` — the real declaration is
    // the point, so the test exists only where that block is compiled.
    #[cfg(feature = "block-products")]
    #[tokio::test]
    async fn stripe_webhook_stays_reachable_with_no_session() {
        use wafer_run::Block as _;

        use crate::{
            blocks::products::ProductsBlock,
            test_support::{anon_msg, TestContext},
        };

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/products",
            DispatchProbeBlock::named("impresspress/products"),
        );
        let block_infos = vec![ProductsBlock::new().info()];

        let out = route_to_block(
            &ctx,
            anon_msg("create", "/b/products/webhooks"),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        let buf = out
            .collect_buffered()
            .await
            .expect("the Stripe webhook path must stay reachable with no session");
        assert_eq!(buf.body, b"DISPATCHED");
    }

    /// The resolution behind `stripe_webhook_stays_reachable_with_no_session`:
    /// `endpoint_auth` and `declared_access` resolve
    /// `POST /b/products/webhooks` to `Public` from
    /// `ProductsBlock::new().info()` alone, with no router entry involved.
    #[cfg(feature = "block-products")]
    #[test]
    fn stripe_webhook_is_public_from_the_products_declaration_alone() {
        use wafer_run::{AuthLevel, Block as _};

        use crate::{blocks::products::ProductsBlock, test_support::anon_msg};

        let block_infos = vec![ProductsBlock::new().info()];
        assert_eq!(
            endpoint_match::endpoint_auth(
                &block_infos[0].endpoints,
                "create",
                "/b/products/webhooks"
            ),
            Some(AuthLevel::Public),
            "the products block declares the webhook public"
        );
        assert_eq!(
            declared_access(
                &block_infos,
                "impresspress/products",
                &anon_msg("create", "/b/products/webhooks"),
            ),
            RouteAccess::Public,
            "the router resolves the webhook public from the declaration"
        );
    }

    #[tokio::test]
    async fn undeclared_products_path_requires_auth() {
        use crate::test_support::{anon_msg, TestContext};

        let ctx = TestContext::new().await;
        let block_infos = vec![wafer_run::BlockInfo::new(
            "impresspress/products",
            "0.0.1",
            "http-handler@v1",
            "t",
        )];

        // Same Public-tier `/b/products` prefix as the declared public
        // webhook, but not a declared path — must still require auth. A
        // public declaration is narrow, not a reopening of the whole prefix.
        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", "/b/products/some-made-up-undeclared-path"),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        assert!(crate::test_support::output_is_error(out, "Unauthenticated").await);
    }

    /// Task 6 fix-round-1 finding: the products block's own `harness::dispatch`
    /// tests enter `ProductsBlock::handle` directly and never go through
    /// `route_to_block`/`check_access`, so they prove the
    /// restore handler's behaviour but not its authorization boundary —
    /// nothing previously exercised the fact that
    /// `POST /b/products/api/admin/products/{id}/restore` is
    /// `AuthLevel::Admin` at the layer that actually enforces it. This test
    /// drives that real path through the real router with the real
    /// `ProductsBlock` `BlockInfo` (not a synthetic fixture — a typo'd path,
    /// method, or `AuthLevel` in `blocks/products/mod.rs`'s declaration must
    /// fail this test), the same way `pipeline.rs`'s
    /// `discovery_tests::real_block_infos()` favors the real declaration
    /// over a hand-rolled one for exactly this reason.
    ///
    /// The products block now serves each handler at exactly one wire
    /// spelling, the declared one (`blocks::products::routes::ROUTES`; its
    /// `table_tests` pin that the former second spellings 404), so the tier
    /// proven here is the whole boundary. The companion
    /// `blocks::products::tests::handler_tests
    /// ::restore_is_unreachable_for_a_non_admin_on_every_path_that_reaches_it`
    /// drives the same router against the REAL block for both restore
    /// routes; it lives there because it needs the products database
    /// harness.
    #[cfg(feature = "block-products")]
    #[tokio::test]
    async fn restore_product_endpoint_is_admin_only_end_to_end() {
        use wafer_run::Block;

        use crate::{
            blocks::products::ProductsBlock,
            test_support::{admin_msg, auth_msg, TestContext},
        };

        // The real wire path, exactly what `route_to_block` sees and what
        // `ProductsBlock::handle` dispatches on.
        let restore_path = "/b/products/api/admin/products/prod_1/restore";
        let block_infos = vec![ProductsBlock::new().info()];

        // 1. The endpoint resolves to Admin via the same resolver
        //    `route_to_block` calls (routing.rs:519-524), against the real
        //    declared `BlockInfo` — not a hand-rolled stand-in.
        assert_eq!(
            declared_access(
                &block_infos,
                "impresspress/products",
                &admin_msg("create", restore_path),
            ),
            RouteAccess::Admin,
            "the restore endpoint must resolve to Admin through declared_access, matching \
             its `.auth(AuthLevel::Admin)` declaration in blocks/products/mod.rs"
        );

        let mut ctx = TestContext::new().await;
        // A dispatch probe, not the real `ProductsBlock` handler: this test
        // is only about the authorization boundary `route_to_block` enforces
        // BEFORE `ctx.call_block` ever reaches the block, not about the
        // restore handler's own behaviour (already covered by
        // `restore_endpoint_returns_the_product_to_the_catalog` et al.).
        ctx.register_block(
            "impresspress/products",
            DispatchProbeBlock::named("impresspress/products"),
        );

        // 2. A non-admin authenticated caller is rejected — the exact
        //    boundary `harness::dispatch`-based tests cannot see, since they
        //    enter `ProductsBlock::handle` directly and skip
        //    `route_to_block`/`check_access` entirely.
        let denied = route_to_block(
            &ctx,
            auth_msg("create", restore_path, "user_1"),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        assert!(
            crate::test_support::output_is_error(denied, "PermissionDenied").await,
            "a non-admin authenticated caller must be denied the restore endpoint"
        );

        // 3. An admin is admitted through to dispatch.
        let admitted = route_to_block(
            &ctx,
            admin_msg("create", restore_path),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        let buf = admitted
            .collect_buffered()
            .await
            .expect("an admin caller must be admitted to the restore endpoint");
        assert_eq!(buf.body, b"DISPATCHED");
    }

    /// The eight session-less `/b/auth/...` paths resolve to `Public` from the
    /// auth-ui block's own declaration, and the two api-key rows resolve to
    /// `Authenticated`. These ten (eleven before B14 deleted `sync-user`) were
    /// the block's undeclared surface before PR #14; the router carve-outs
    /// that kept them reachable are gone because these declarations carry the
    /// level.
    #[test]
    fn auth_ui_declares_its_eight_session_less_and_two_api_key_paths() {
        use wafer_run::Block as _;

        let info = crate::blocks::auth_ui::AuthUiBlock::new().info();

        for (action, path) in AUTH_UI_SESSION_LESS_PATHS {
            assert_eq!(
                endpoint_match::endpoint_auth(&info.endpoints, action, path),
                Some(AuthLevel::Public),
                "{action} {path} must be declared public by the auth-ui block itself"
            );
        }
        for action in ["update", "delete"] {
            assert_eq!(
                endpoint_match::endpoint_auth(&info.endpoints, action, "/b/auth/api/api-keys/k-1"),
                Some(AuthLevel::Authenticated),
                "{action} /b/auth/api/api-keys/{{id}} must be declared authenticated"
            );
        }
    }

    /// The eight auth-ui `(action, path)` pairs that legitimately have no
    /// session: each handler gates itself by a token or a signature, and the
    /// block declares each `public`. (It was nine until B14 deleted
    /// `POST /b/auth/api/oauth/sync-user`, whose gate was a config var no
    /// `ConfigVar` declared and `auth_grants()` did not grant.)
    const AUTH_UI_SESSION_LESS_PATHS: &[(&str, &str)] = &[
        ("retrieve", "/b/auth/reset-password"),
        ("retrieve", "/b/auth/oauth/callback"),
        ("retrieve", "/b/auth/api/verify"),
        ("create", "/b/auth/api/verify"),
        ("create", "/b/auth/api/resend-verification"),
        ("create", "/b/auth/api/forgot-password"),
        ("create", "/b/auth/api/reset-password"),
        ("retrieve", "/b/auth/api/oauth/providers"),
    ];

    /// Each session-less auth-ui path reaches dispatch anonymously from the
    /// auth-ui declaration alone, driven through `route_to_block` with the
    /// block's real `info()`. This is the router-level proof behind
    /// `auth_ui_declares_its_eight_session_less_and_two_api_key_paths`: a
    /// router entry that merely restates one of these rows is redundant.
    #[tokio::test]
    async fn auth_ui_session_less_paths_dispatch_anonymously_from_the_declaration() {
        use wafer_run::Block as _;

        use crate::{
            blocks::auth_ui::AuthUiBlock,
            test_support::{anon_msg, TestContext},
        };

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/auth-ui",
            DispatchProbeBlock::named("impresspress/auth-ui"),
        );
        let block_infos = vec![AuthUiBlock::new().info()];

        for (action, path) in AUTH_UI_SESSION_LESS_PATHS {
            let out = route_to_block(
                &ctx,
                anon_msg(action, path),
                InputStream::empty(),
                &AllEnabled,
                &block_infos,
                &[],
            )
            .await;
            let buf = out.collect_buffered().await.unwrap_or_else(|terminal| {
                panic!("anonymous {action} {path} must dispatch from the declaration, got {terminal:?}")
            });
            assert_eq!(buf.body, b"DISPATCHED", "{action} {path}");
        }
    }

    /// The two api-key rows were never carved out: they were reachable only
    /// because `declared_access`'s fail-closed default happens to be the
    /// level the handler wants. Now the block declares them `authenticated`
    /// and the router enforces exactly that from the declaration.
    #[tokio::test]
    async fn auth_ui_api_key_rows_need_a_session_from_the_declaration() {
        use wafer_run::Block as _;

        use crate::{
            blocks::auth_ui::AuthUiBlock,
            test_support::{anon_msg, auth_msg, output_is_error, TestContext},
        };

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/auth-ui",
            DispatchProbeBlock::named("impresspress/auth-ui"),
        );
        let block_infos = vec![AuthUiBlock::new().info()];
        let path = "/b/auth/api/api-keys/k-1";

        for action in ["update", "delete"] {
            let denied = route_to_block(
                &ctx,
                anon_msg(action, path),
                InputStream::empty(),
                &AllEnabled,
                &block_infos,
                &[],
            )
            .await;
            assert!(
                output_is_error(denied, "Unauthenticated").await,
                "anonymous {action} {path} must be denied"
            );

            let admitted = route_to_block(
                &ctx,
                auth_msg(action, path, "user_1"),
                InputStream::empty(),
                &AllEnabled,
                &block_infos,
                &[],
            )
            .await;
            let buf = admitted
                .collect_buffered()
                .await
                .unwrap_or_else(|terminal| {
                    panic!("{action} {path} must dispatch, got {terminal:?}")
                });
            assert_eq!(buf.body, b"DISPATCHED", "{action} {path}");
        }
    }

    /// `/b/admin/settings` carried its own `Admin` prefix entry. Every path
    /// under it is gated `Admin` regardless: by the `/b/admin/` entry, and by
    /// the admin block's own rows, which are all `admin`. Driven with the
    /// real `all_block_infos()`, for a declared tab, the redirecting index
    /// and an undeclared path, so the dedicated entry is redundant.
    #[tokio::test]
    async fn admin_settings_paths_are_denied_without_the_admin_role() {
        use crate::test_support::{admin_msg, anon_msg, auth_msg, output_is_error, TestContext};

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/admin",
            DispatchProbeBlock::named("impresspress/admin"),
        );
        let block_infos = crate::blocks::all_block_infos();

        let declared = ["/b/admin/settings/", "/b/admin/settings/email"];
        let undeclared = "/b/admin/settings/not-a-tab";
        for path in declared.iter().copied().chain(std::iter::once(undeclared)) {
            for (label, msg, refusal) in [
                ("anonymous", anon_msg("retrieve", path), "Unauthenticated"),
                (
                    "non-admin",
                    auth_msg("retrieve", path, "user_1"),
                    "PermissionDenied",
                ),
            ] {
                let out = route_to_block(
                    &ctx,
                    msg,
                    InputStream::empty(),
                    &AllEnabled,
                    &block_infos,
                    &[],
                )
                .await;
                assert!(
                    output_is_error(out, refusal).await,
                    "{label} GET {path} must be denied"
                );
            }
        }
        for path in declared {
            let out = route_to_block(
                &ctx,
                admin_msg("retrieve", path),
                InputStream::empty(),
                &AllEnabled,
                &block_infos,
                &[],
            )
            .await;
            let buf = out.collect_buffered().await.unwrap_or_else(|terminal| {
                panic!("admin GET {path} must dispatch, got {terminal:?}")
            });
            assert_eq!(buf.body, b"DISPATCHED", "admin GET {path}");
        }
    }

    /// `/b/legalpages/admin` and `/b/legalpages/api` carried `Admin` prefix
    /// entries because the block's handlers do not re-check `is_admin`.
    /// Every row the block declares under those prefixes is `admin`, so the
    /// declaration gates each path at the same level. Driven with the real
    /// `all_block_infos()` for a representative row of every shape (pages,
    /// page writes, the JSON collection, an `{id}` row), plus the public
    /// terms page to show the public rows are not over-gated.
    #[tokio::test]
    async fn legalpages_admin_and_api_paths_are_denied_without_the_admin_role() {
        use crate::test_support::{admin_msg, anon_msg, auth_msg, output_is_error, TestContext};

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/legalpages",
            DispatchProbeBlock::named("impresspress/legalpages"),
        );
        let block_infos = crate::blocks::all_block_infos();

        const ADMIN_ROWS: &[(&str, &str)] = &[
            ("retrieve", "/b/legalpages/admin"),
            ("retrieve", "/b/legalpages/admin/terms"),
            ("create", "/b/legalpages/admin/save"),
            ("retrieve", "/b/legalpages/api/documents"),
            ("create", "/b/legalpages/api/documents"),
            ("update", "/b/legalpages/api/documents/d-1"),
            ("delete", "/b/legalpages/api/documents/d-1"),
        ];
        for (action, path) in ADMIN_ROWS {
            for (label, msg, refusal) in [
                ("anonymous", anon_msg(action, path), "Unauthenticated"),
                (
                    "non-admin",
                    auth_msg(action, path, "user_1"),
                    "PermissionDenied",
                ),
            ] {
                let out = route_to_block(
                    &ctx,
                    msg,
                    InputStream::empty(),
                    &AllEnabled,
                    &block_infos,
                    &[],
                )
                .await;
                assert!(
                    output_is_error(out, refusal).await,
                    "{label} {action} {path} must be denied"
                );
            }
            let out = route_to_block(
                &ctx,
                admin_msg(action, path),
                InputStream::empty(),
                &AllEnabled,
                &block_infos,
                &[],
            )
            .await;
            let buf = out.collect_buffered().await.unwrap_or_else(|terminal| {
                panic!("admin {action} {path} must dispatch, got {terminal:?}")
            });
            assert_eq!(buf.body, b"DISPATCHED", "admin {action} {path}");
        }

        let out = route_to_block(
            &ctx,
            anon_msg("retrieve", "/b/legalpages/terms"),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        let buf = out
            .collect_buffered()
            .await
            .expect("the public terms page must dispatch anonymously");
        assert_eq!(buf.body, b"DISPATCHED");
    }

    /// A router carve-out was a prefix entry: it admitted every method and
    /// every path under it, and the block answered the shapes it did not
    /// serve with a 404. A declaration admits exactly its `(method,
    /// template)` row, so those shapes are undeclared, fall to the
    /// `Authenticated` default, and are denied before dispatch. Driven with
    /// the real declarations of the three blocks that used to be carved out.
    #[cfg(feature = "block-products")]
    #[tokio::test]
    async fn a_declaration_admits_its_template_where_a_carve_out_admitted_a_prefix() {
        use wafer_run::Block as _;

        use crate::{
            blocks::{auth_ui::AuthUiBlock, products::ProductsBlock, system::SystemBlock},
            test_support::{anon_msg, output_is_error, TestContext},
        };

        let mut ctx = TestContext::new().await;
        for name in [
            "impresspress/system",
            "impresspress/auth-ui",
            "impresspress/products",
        ] {
            ctx.register_block(name, DispatchProbeBlock::named(name));
        }
        let block_infos = vec![
            SystemBlock::new().info(),
            AuthUiBlock::new().info(),
            ProductsBlock::new().info(),
        ];

        for (action, path) in [
            ("retrieve", "/b/products/webhooks"),
            ("create", "/b/products/webhooks/extra"),
            ("retrieve", "/b/auth/api/verify/extra"),
            ("retrieve", "/b/static/a/b"),
        ] {
            let out = route_to_block(
                &ctx,
                anon_msg(action, path),
                InputStream::empty(),
                &AllEnabled,
                &block_infos,
                &[],
            )
            .await;
            assert!(
                output_is_error(out, "Unauthenticated").await,
                "anonymous {action} {path} is undeclared and must be denied before dispatch"
            );
        }
    }

    /// An undeclared path under a prefix that used to carry its own `Admin`
    /// entry falls to the fail-closed `Authenticated` default: an anonymous
    /// caller is denied, and a logged-in caller reaches the block, whose
    /// table dispatch answers 404 for a path it does not declare
    /// (`blocks::legalpages` reads nothing from the path before
    /// `endpoint_match::dispatch`). Every path the block does serve under
    /// `/b/legalpages/admin` and `/b/legalpages/api` is declared `admin` and
    /// gated by that declaration, see
    /// `legalpages_admin_and_api_paths_are_denied_without_the_admin_role`.
    #[tokio::test]
    async fn an_undeclared_path_under_a_former_admin_prefix_entry_falls_to_authenticated() {
        use crate::test_support::{anon_msg, auth_msg, output_is_error, TestContext};

        let mut ctx = TestContext::new().await;
        ctx.register_block(
            "impresspress/legalpages",
            DispatchProbeBlock::named("impresspress/legalpages"),
        );
        let block_infos = crate::blocks::all_block_infos();
        let path = "/b/legalpages/admin/does-not-exist";

        let denied = route_to_block(
            &ctx,
            anon_msg("retrieve", path),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        assert!(
            output_is_error(denied, "Unauthenticated").await,
            "an anonymous caller must be denied an undeclared path"
        );

        let admitted = route_to_block(
            &ctx,
            auth_msg("retrieve", path, "user_1"),
            InputStream::empty(),
            &AllEnabled,
            &block_infos,
            &[],
        )
        .await;
        let buf = admitted.collect_buffered().await.expect(
            "a logged-in caller reaches the block, whose table dispatch answers 404 for an \
             undeclared path",
        );
        assert_eq!(buf.body, b"DISPATCHED");
    }

    /// No entry's prefix is served by another entry's, so the table is
    /// order-independent: the first match is the only match. A new entry
    /// nested under an existing one (the shape the deleted carve-outs and
    /// the `/b/admin/settings` entry had) needs this test changed and the
    /// reason for the nesting written down.
    #[test]
    fn prefix_entries_are_pairwise_disjoint() {
        for (i, a) in ROUTES.iter().enumerate() {
            for (j, b) in ROUTES.iter().enumerate() {
                if i == j {
                    continue;
                }
                assert!(
                    !route_prefix_matches(a.prefix, b.prefix),
                    "the {} entry serves the {} entry's prefix",
                    a.prefix,
                    b.prefix
                );
            }
        }
    }

    /// The table is hand-written; this is what keeps it honest against the
    /// blocks' own declarations. Every declared endpoint of every registered
    /// block is served by an entry naming that block (as `block` or
    /// `dispatch_to`), and every entry's block declares at least one endpoint
    /// under it. The one exemption is the inspector proxy: its `BlockInfo`
    /// (`wafer_block_inspector::InspectorBlock`) is the runtime's own,
    /// declares no `BlockEndpoint`s and is not in `all_block_infos()`; its
    /// per-request gating is its own `AccessPolicy` on top of the `Admin`
    /// prefix tier.
    #[test]
    fn every_declared_endpoint_sits_under_its_blocks_prefix_and_every_prefix_is_declared_against() {
        let infos = crate::blocks::all_block_infos();

        for info in &infos {
            for ep in &info.endpoints {
                let entry = ROUTES
                    .iter()
                    .find(|r| route_prefix_matches(r.prefix, &ep.path))
                    .unwrap_or_else(|| {
                        panic!(
                            "{} declares {} {} but no ROUTES entry serves it",
                            info.name, ep.method, ep.path
                        )
                    });
                assert!(
                    entry.block == info.name || entry.dispatch_to == info.name,
                    "{} declares {} {} but the {} entry serves it from {}",
                    info.name,
                    ep.method,
                    ep.path,
                    entry.prefix,
                    entry.block
                );
            }
        }

        let mut without_info = Vec::new();
        for route in ROUTES {
            match infos
                .iter()
                .find(|i| i.name == route.block || i.name == route.dispatch_to)
            {
                Some(info) => assert!(
                    info.endpoints
                        .iter()
                        .any(|ep| route_prefix_matches(route.prefix, &ep.path)),
                    "the {} entry names {} but the block declares nothing under it",
                    route.prefix,
                    route.block
                ),
                None => without_info.push(route),
            }
        }
        let exempt: Vec<&str> = without_info.iter().map(|r| r.prefix).collect();
        assert_eq!(
            exempt,
            vec!["/b/inspector"],
            "only the inspector proxy may name a block absent from all_block_infos()"
        );
        assert_ne!(
            without_info[0].block, without_info[0].dispatch_to,
            "the exemption is the one proxy, which dispatches to the runtime's own block"
        );
    }

    /// Shared dummy block for the tests above: always dispatches successfully
    /// with a recognizable body, so a test can prove "reached dispatch"
    /// rather than merely "wasn't denied". It stands in for whichever block
    /// it is registered as, and reports that name, as the runtime requires.
    struct DispatchProbeBlock(String);

    impl DispatchProbeBlock {
        fn named(name: &str) -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self(name.to_string()))
        }
    }

    #[async_trait::async_trait]
    impl wafer_run::Block for DispatchProbeBlock {
        fn info(&self) -> wafer_run::BlockInfo {
            wafer_run::BlockInfo::new(&self.0, "0.0.1", "echo@v1", "dispatch probe")
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
