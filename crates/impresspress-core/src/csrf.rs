//! CSRF defenses for cookie-authenticated mutations, and for the requests
//! that mint the cookie in the first place.
//!
//! Cookie authentication (the `auth_token` cookie — see
//! `blocks::router::ImpresspressRouterBlock::handle`) relies on
//! `SameSite=Lax` alone unless this module is wired in. `SameSite=Lax` still
//! attaches the cookie to a cross-site *top-level* navigation using a "safe"
//! method, and — because the cookie is host-only rather than scoped to a
//! specific subdomain — to any same-site cross-subdomain request using an
//! unsafe method, so it is not a complete CSRF defense on its own.
//!
//! Where a request came from is decided once, by [`request_origin`], and the
//! layers below read that one answer:
//!
//! 1. [`enforce_origin_policy`] — Fetch-Metadata (`Sec-Fetch-Site`) / `Origin`
//!    / `Referer` validation for cookie-authenticated unsafe-method requests.
//!    This is the BROAD net: called exactly once, from
//!    `pipeline::handle_request`, before any block dispatch, so it covers
//!    every mutation uniformly. A request authenticated by a real
//!    `Authorization: Bearer` header (not the cookie) is exempt — a
//!    cross-site page has no ambient credential to attach a bearer token
//!    with, so it cannot forge that request in the first place.
//!
//!    1b. [`SessionIssuance`] — session issuance. Layer 1 exempts a request
//!    with no verified session cookie, and the requests that hand one out
//!    (password login, signup, refresh, bootstrap redemption) carry none: a
//!    cross-site `<form>` posting the attacker's credentials to the login
//!    endpoint would sign the victim's browser into the attacker's account
//!    (login CSRF). So the issuance tail,
//!    `blocks::auth::helpers::issue_tokens_and_cookie`, takes a
//!    [`SessionIssuance`], and the only ways to get one are
//!    [`SessionIssuance::verify`] — the request is same-origin, or carries no
//!    browser signal at all — and [`SessionIssuance::oauth_callback`], whose
//!    cross-site navigation is defended by the single-use PKCE state it
//!    consumed. The password-path handlers obtain it before any side effect;
//!    the callback builds it from the state it has just consumed.
//!
//! 2. [`token`] / [`hidden_field`] / [`verify`] — a stateless synchronizer
//!    token for the plain (non-JS, non-`htmx`) SSR `<form>` POSTs in this
//!    app (bootstrap-admin redemption, profile update). Defense-in-depth on
//!    top of layer 1: even an allowed-origin POST must also carry the
//!    correct per-identity token. For a signed-in identity nothing outside
//!    this origin can produce or read it; signed out it is one shared,
//!    readable value (see [`ANONYMOUS_SUBJECT`]).
//!
//! `htmx`- and `fetch`-driven forms elsewhere in the app (admin variables,
//! users, database explorer, portal buttons, chat composer, settings forms,
//! …) are not retrofitted with the token: they are real browser-issued
//! requests like any other, so layer 1 already covers them, and they already
//! require the `Admin`/`Authenticated` route tier centrally enforced by
//! `routing::route_to_block`. Layer 2 targets the handful of forms that
//! submit as a plain, unmediated browser POST.

use maud::{html, Markup};
use wafer_block_crypto::primitives;
use wafer_run::{context::Context, Message, OutputStream};

use crate::blocks::auth::repo::oauth_pkce::ConsumedPkceState;

/// KDF context label for the CSRF signing key (see [`primitives::derive_block_key`]).
/// Keeps the CSRF key cryptographically independent from the JWT verify key
/// derived (elsewhere) from the same master secret, even though both trace
/// back to `WAFER_RUN__AUTH__JWT_SECRET`.
const CSRF_KEY_LABEL: &str = "impresspress/csrf";

/// Subject used to key the token when no session is established yet (e.g.
/// the bootstrap-admin form, rendered and submitted with no `auth_token`
/// cookie at all). The token is then the same for every signed-out visitor,
/// and anyone can read it by fetching the form page, so it is not a secret:
/// it only stops a cross-site page's script from reading it out of the
/// victim's browser (Same-Origin Policy), and a signed-out route that needs
/// a real cross-site defence has the origin check
/// ([`SessionIssuance::verify`]).
const ANONYMOUS_SUBJECT: &str = "anonymous";

/// `<form>` field name every CSRF-protected plain-HTML form submits the
/// token under.
pub const FIELD_NAME: &str = "csrf_token";

// ---------------------------------------------------------------------------
// Layer 1: Fetch-Metadata / Origin / Referer policy
// ---------------------------------------------------------------------------

/// HTTP methods a CSRF attack can drive (state-changing). Matches the
/// `wafer_block::http_codec::action_for_http_method` wire contract:
/// `POST` → `create`, `PUT`/`PATCH` → `update`, `DELETE` → `delete`.
fn is_unsafe_method(action: &str) -> bool {
    matches!(action, "create" | "update" | "delete")
}

/// The reason a request the origin policy refuses is given: the JSON error
/// message for an API caller, the body of the 403 page for a browser.
pub const ORIGIN_BLOCKED_REASON: &str =
    "This request couldn't be verified as coming from this site. Reload the page and try again.";

/// Enforce the CSRF origin policy for one already-routed request.
///
/// Called from `pipeline::handle_request`, after the auth-meta extraction
/// step (so `msg.user_id()` reflects whether the cookie's JWT actually
/// verified) and before `routing::route_to_block`. Returns `Some(response)`
/// to short-circuit with the 403 [`crate::ui::forbidden_response`] draws for
/// [`ORIGIN_BLOCKED_REASON`], `None` to let the request proceed. The decision
/// itself is [`origin_policy_refuses`].
pub async fn enforce_origin_policy(
    ctx: &dyn Context,
    msg: &Message,
    cookie_authenticated: bool,
) -> Option<OutputStream> {
    if !origin_policy_refuses(msg, cookie_authenticated) {
        return None;
    }
    Some(
        crate::ui::forbidden_response(
            ctx,
            msg,
            ORIGIN_BLOCKED_REASON,
            crate::ui::SignedOutAction::SignIn,
        )
        .await,
    )
}

/// Whether the CSRF origin policy refuses this request.
///
/// Only applies when ALL of:
/// - `cookie_authenticated` — the credential came from the `auth_token`
///   cookie fallback, not a real `Authorization: Bearer` header (see
///   `blocks::router::ImpresspressRouterBlock::handle`, the one place that
///   knows which source resolved the credential).
/// - `!msg.user_id().is_empty()` — the cookie's JWT actually verified. An
///   invalid/expired cookie establishes no session to protect; a protected
///   route still rejects it downstream (missing `user_id`), and a genuinely
///   public route (the Stripe webhook, OAuth callback, password-reset
///   endpoints) is not meant to require this check at all. The public routes
///   that MINT a session are gated separately, by [`SessionIssuance`].
/// - the method is unsafe (`is_unsafe_method`).
///
/// Such a request must be [`Origin::SameOrigin`]. A cookie with no browser
/// signal at all ([`Origin::NoBrowserSignal`]) fails closed: the cookie is
/// an ambient credential, so its presence alone proves nothing about who
/// sent the request.
fn origin_policy_refuses(msg: &Message, cookie_authenticated: bool) -> bool {
    if !cookie_authenticated || msg.user_id().is_empty() || !is_unsafe_method(msg.action()) {
        return false;
    }
    request_origin(msg) != Origin::SameOrigin
}

/// Where a request came from, as far as the browser says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `Sec-Fetch-Site` is `same-origin` or `none`, or — without Fetch
    /// Metadata — `Origin` (else `Referer`) names this request's own `Host`.
    SameOrigin,
    /// Any other `Sec-Fetch-Site` value, or an `Origin`/`Referer` that names
    /// another host or none (the opaque `Origin: null`).
    CrossOrigin,
    /// None of `Sec-Fetch-Site`, `Origin` or `Referer`: not a browser (an API
    /// client, the CLI, `curl`), or a browser too old to send any of them.
    NoBrowserSignal,
}

/// Classify where `msg` came from. The single source of that answer for
/// both layer 1 ([`enforce_origin_policy`]) and session issuance
/// ([`SessionIssuance::verify`]).
///
/// The primary signal is Fetch Metadata, sent by every modern browser for
/// essentially all requests (fetch/XHR, `<form>` submission, `htmx`),
/// including same-origin ones. Only `same-origin` and `none` (a typed URL /
/// bookmark / browser extension — no web page at all) say the request came
/// from this origin; everything else, including `same-site`, `cross-site`
/// and any value this does not recognize, is cross-origin. `same-site` is
/// deliberately NOT same-origin: the `auth_token` cookie is host-only (no
/// explicit `Domain` attribute), but `SameSite=Lax` still attaches it to a
/// same-site cross-subdomain request, so a `same-site` classification does
/// not imply the request came from this app — it could come from any sibling
/// subdomain under the registrable domain. impresspress serves everything
/// same-origin, so there is no legitimate same-site-but-cross-subdomain flow
/// to accommodate.
///
/// The fallback, for clients that send no Fetch Metadata (browsers predating
/// Chrome 76 / Firefox 90 / Safari 16.4, or non-browser clients), compares
/// `Origin`, then `Referer`, against this request's own `Host`; whichever is
/// present first decides. The browser runtime's service worker, which is
/// never sent these headers, synthesizes `sec-fetch-site` for the requests
/// it intercepts (`impresspress-browser`'s `convert.rs`), so this sees the
/// same signal on native, Cloudflare and the browser.
pub fn request_origin(msg: &Message) -> Origin {
    let sec_fetch_site = msg.header("sec-fetch-site");
    if !sec_fetch_site.is_empty() {
        return match sec_fetch_site.to_ascii_lowercase().as_str() {
            "same-origin" | "none" => Origin::SameOrigin,
            _ => Origin::CrossOrigin,
        };
    }

    let self_authority = normalize_authority(msg.header("host"));
    for header in ["origin", "referer"] {
        let value = msg.header(header);
        if value.is_empty() {
            continue;
        }
        let authority = request_authority(value);
        return if authority.is_some() && authority == self_authority {
            Origin::SameOrigin
        } else {
            Origin::CrossOrigin
        };
    }

    Origin::NoBrowserSignal
}

// ---------------------------------------------------------------------------
// Layer 1b: session issuance
// ---------------------------------------------------------------------------

/// Proof that this request may be handed a session: the one argument
/// `blocks::auth::helpers::issue_tokens_and_cookie` will not mint without.
///
/// It has a private field and no `Default` or `Clone`, so outside this module
/// it exists only as what [`verify`](Self::verify) or
/// [`oauth_callback`](Self::oauth_callback) returned, and issuance consumes
/// it, so one proof mints one session.
#[derive(Debug)]
pub struct SessionIssuance {
    _proof: (),
}

impl SessionIssuance {
    /// Admit a password login, signup, refresh or bootstrap redemption.
    ///
    /// `Ok` when the request is [`Origin::SameOrigin`], or carries
    /// [`Origin::NoBrowserSignal`]: with no browser there is no victim's
    /// browser to forge the request from, and those callers (API clients,
    /// the CLI, `curl`) hold the credentials they send. A
    /// [`Origin::CrossOrigin`] request is refused with the same 403 layer 1
    /// gives ([`ORIGIN_BLOCKED_REASON`]), and that includes an origin listed
    /// in `WAFER_RUN_SHARED__CORS_ALLOWED_ORIGINS`: CORS lets such a page READ
    /// responses, but no origin other than this one may obtain this site's
    /// session cookie.
    ///
    /// Callers check this before any side effect, so a refused request has
    /// created no account, spent no token and checked no password.
    pub async fn verify(ctx: &dyn Context, msg: &Message) -> Result<Self, OutputStream> {
        match request_origin(msg) {
            Origin::SameOrigin | Origin::NoBrowserSignal => Ok(Self { _proof: () }),
            Origin::CrossOrigin => Err(crate::ui::forbidden_response(
                ctx,
                msg,
                ORIGIN_BLOCKED_REASON,
                crate::ui::SignedOutAction::SignIn,
            )
            .await),
        }
    }

    /// Admit the OAuth callback. The provider redirects the browser here, so
    /// it is a cross-site top-level navigation by design and
    /// [`verify`](Self::verify) would refuse it. Its CSRF defence is the
    /// single-use PKCE state this flow consumed (plus the browser binding the
    /// callback checks before consuming it): [`ConsumedPkceState`] is
    /// produced only by `oauth_pkce::take`, so holding one proves this
    /// request redeemed a flow this deployment started.
    pub fn oauth_callback(_state: &ConsumedPkceState) -> Self {
        Self { _proof: () }
    }
}

/// Extract the `host[:port]` authority from an absolute URL string (an
/// `Origin` or `Referer` header value), lower-cased. `None` if the value
/// doesn't parse as an absolute URL (covers the opaque `Origin: null` case).
fn request_authority(url_str: &str) -> Option<String> {
    let url = url::Url::parse(url_str).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    Some(match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    })
}

/// Normalize a raw `Host` header value (no scheme) into the same
/// `host[:port]` shape [`request_authority`] produces, so the two compare
/// equal regardless of scheme.
fn normalize_authority(host_header: &str) -> Option<String> {
    if host_header.is_empty() {
        return None;
    }
    request_authority(&format!("http://{host_header}"))
}

// ---------------------------------------------------------------------------
// Layer 2: stateless synchronizer token for plain SSR `<form>` POSTs
// ---------------------------------------------------------------------------

/// Compute the expected CSRF token for the current request's identity.
///
/// Stateless: an HMAC over the caller's identity (the authenticated
/// `user_id`, or [`ANONYMOUS_SUBJECT`] pre-login) keyed by a
/// `WAFER_RUN__AUTH__JWT_SECRET`-derived key. No server-side storage, so it
/// survives restarts and multi-isolate deployments; rotating the master
/// secret invalidates every outstanding token exactly like it invalidates
/// every JWT.
pub fn token(ctx: &dyn Context, msg: &Message) -> String {
    let secret = ctx
        .config_get(crate::blocks::auth::JWT_SECRET_KEY)
        .unwrap_or("");
    let key = primitives::derive_block_key(secret.as_bytes(), CSRF_KEY_LABEL);
    let subject = subject_for(msg);
    let mac = primitives::hmac_sha256(key.as_bytes(), subject.as_bytes());
    crate::util::hex_encode(&mac)
}

fn subject_for(msg: &Message) -> &str {
    let uid = msg.user_id();
    if uid.is_empty() {
        ANONYMOUS_SUBJECT
    } else {
        uid
    }
}

/// Hidden `<input>` carrying the current [`token`] value. Embed inside any
/// plain SSR `<form>` this module protects; the corresponding POST handler
/// validates it with [`verify`].
pub fn hidden_field(ctx: &dyn Context, msg: &Message) -> Markup {
    let value = token(ctx, msg);
    html! { input type="hidden" name=(FIELD_NAME) value=(value); }
}

/// Validate a submitted token against the expected value for this request's
/// identity. Constant-time compare — a timing side-channel would let an
/// attacker recover the expected token byte-by-byte, defeating the whole
/// point of the check.
pub fn verify(ctx: &dyn Context, msg: &Message, submitted: &str) -> bool {
    if submitted.is_empty() {
        return false;
    }
    let expected = token(ctx, msg);
    primitives::constant_time_eq(expected.as_bytes(), submitted.as_bytes())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{anon_msg, auth_msg, TestContext};

    fn cookie_msg(action: &str, path: &str, user_id: &str) -> Message {
        let mut msg = if user_id.is_empty() {
            anon_msg(action, path)
        } else {
            auth_msg(action, path, user_id)
        };
        msg.set_meta("http.header.host", "impresspress.example.com");
        msg
    }

    // -- enforce_origin_policy: gating conditions --------------------------

    #[test]
    fn safe_method_is_never_blocked_regardless_of_headers() {
        let mut msg = cookie_msg("retrieve", "/b/admin/users", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "cross-site");
        assert!(!origin_policy_refuses(&msg, true));
    }

    #[test]
    fn bearer_authenticated_request_is_exempt_even_when_cross_site() {
        // cookie_authenticated=false: a real Authorization header resolved
        // this credential, not the cookie fallback — never CSRF-able.
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "cross-site");
        assert!(!origin_policy_refuses(&msg, false));
    }

    #[test]
    fn unauthenticated_cookie_is_not_blocked_here() {
        // cookie_authenticated=true but no user_id — the JWT didn't verify.
        // No session to protect; downstream `check_access` handles a
        // protected route, and a genuinely public route is untouched.
        let mut msg = cookie_msg("create", "/b/admin/users", "");
        msg.set_meta("http.header.sec-fetch-site", "cross-site");
        assert!(!origin_policy_refuses(&msg, true));
    }

    // -- enforce_origin_policy: Sec-Fetch-Site ------------------------------

    #[tokio::test]
    async fn cross_site_cookie_authenticated_post_is_rejected() {
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "cross-site");
        let ctx = TestContext::new().await;
        let out = enforce_origin_policy(&ctx, &msg, true)
            .await
            .expect("must reject");
        assert!(crate::test_support::output_is_error(out, "PermissionDenied").await);
    }

    #[test]
    fn same_origin_cookie_authenticated_post_is_allowed() {
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "same-origin");
        assert!(!origin_policy_refuses(&msg, true));
    }

    #[test]
    fn none_and_same_origin_are_allowed() {
        for value in ["none", "SAME-ORIGIN"] {
            let mut msg = cookie_msg("update", "/b/admin/users/1", "user-1");
            msg.set_meta("http.header.sec-fetch-site", value);
            assert!(
                !origin_policy_refuses(&msg, true),
                "{value} should be allowed"
            );
        }
    }

    #[tokio::test]
    async fn same_site_cookie_authenticated_post_is_rejected() {
        // A sibling subdomain under the same registrable domain classifies
        // as `same-site`, not `same-origin`. The host-only `auth_token`
        // cookie still attaches to it under `SameSite=Lax`, so an attacker
        // controlling any sibling subdomain could otherwise forge this
        // mutation. impresspress has no legitimate same-site-but-cross-
        // subdomain POST flow, so this must be rejected just like
        // `cross-site`.
        let mut msg = cookie_msg("update", "/b/admin/users/1", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "same-site");
        let ctx = TestContext::new().await;
        let out = enforce_origin_policy(&ctx, &msg, true)
            .await
            .expect("must reject");
        assert!(crate::test_support::output_is_error(out, "PermissionDenied").await);
    }

    #[tokio::test]
    async fn unrecognized_sec_fetch_site_value_is_rejected() {
        let mut msg = cookie_msg("delete", "/b/admin/users/1", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "garbage");
        let ctx = TestContext::new().await;
        let out = enforce_origin_policy(&ctx, &msg, true)
            .await
            .expect("must reject");
        assert!(crate::test_support::output_is_error(out, "PermissionDenied").await);
    }

    // -- enforce_origin_policy: Origin/Referer fallback ---------------------

    #[test]
    fn matching_origin_is_allowed_when_no_sec_fetch_site() {
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta("http.header.origin", "https://impresspress.example.com");
        assert!(!origin_policy_refuses(&msg, true));
    }

    #[tokio::test]
    async fn mismatched_origin_is_rejected() {
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta("http.header.origin", "https://evil.example");
        let ctx = TestContext::new().await;
        let out = enforce_origin_policy(&ctx, &msg, true)
            .await
            .expect("must reject");
        assert!(crate::test_support::output_is_error(out, "PermissionDenied").await);
    }

    #[test]
    fn matching_referer_is_allowed_when_origin_absent() {
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta(
            "http.header.referer",
            "https://impresspress.example.com/b/admin/users",
        );
        assert!(!origin_policy_refuses(&msg, true));
    }

    #[tokio::test]
    async fn no_fetch_metadata_no_origin_no_referer_is_rejected() {
        let msg = cookie_msg("create", "/b/admin/users", "user-1");
        let ctx = TestContext::new().await;
        let out = enforce_origin_policy(&ctx, &msg, true)
            .await
            .expect("must reject fail-closed");
        assert!(crate::test_support::output_is_error(out, "PermissionDenied").await);
    }

    #[test]
    fn origin_ignores_scheme_difference_from_host() {
        // Self-authority comparison is host[:port]-only — dev often serves
        // http:// locally while Origin/Referer construction elsewhere in the
        // stack assumes https. Scheme is not a security-relevant dimension
        // here: an attacker still needs control of the matching host.
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta("http.header.origin", "http://impresspress.example.com");
        assert!(!origin_policy_refuses(&msg, true));
    }

    // -- SessionIssuance::verify: the signed-out cross-origin shapes --------

    /// A request with no session to protect, as login, signup and refresh
    /// receive it, from `impresspress.example.com`'s point of view.
    fn signed_out_post() -> Message {
        cookie_msg("create", "/b/auth/api/login", "")
    }

    async fn issuance_is_refused(msg: &Message) -> bool {
        let ctx = TestContext::new().await;
        match SessionIssuance::verify(&ctx, msg).await {
            Ok(_) => false,
            Err(out) => crate::test_support::output_is_error(out, "PermissionDenied").await,
        }
    }

    /// `Origin: null` is what a sandboxed iframe, a page with
    /// `Referrer-Policy: no-referrer` and a cross-origin redirect of a POST
    /// send. It names no host, so it is not this one, and a Referer that does
    /// match cannot rescue it: `Origin` is present, so it decides.
    #[tokio::test]
    async fn an_opaque_origin_is_refused_issuance() {
        for referer in [None, Some("https://impresspress.example.com/b/auth/login")] {
            let mut msg = signed_out_post();
            msg.set_meta("http.header.origin", "null");
            if let Some(referer) = referer {
                msg.set_meta("http.header.referer", referer);
            }
            assert_eq!(request_origin(&msg), Origin::CrossOrigin, "{referer:?}");
            assert!(issuance_is_refused(&msg).await, "{referer:?}");
        }
    }

    /// With neither Fetch Metadata nor `Origin`, a `Referer` naming another
    /// site is cross-origin, not "no browser signal".
    #[tokio::test]
    async fn a_foreign_referer_alone_is_refused_issuance() {
        let mut msg = signed_out_post();
        msg.set_meta(
            "http.header.referer",
            "https://evil.example/login-csrf.html",
        );
        assert_eq!(request_origin(&msg), Origin::CrossOrigin);
        assert!(issuance_is_refused(&msg).await);
    }

    // -- token / hidden_field / verify ---------------------------------------

    #[tokio::test]
    async fn token_roundtrips_for_authenticated_user() {
        let mut ctx = TestContext::new().await;
        ctx.set_config(crate::blocks::auth::JWT_SECRET_KEY, "test-master-secret");
        let msg = auth_msg("create", "/b/userportal/update-profile", "user-1");

        let tok = token(&ctx, &msg);
        assert!(!tok.is_empty());
        assert!(verify(&ctx, &msg, &tok));
    }

    #[tokio::test]
    async fn token_differs_per_user() {
        let mut ctx = TestContext::new().await;
        ctx.set_config(crate::blocks::auth::JWT_SECRET_KEY, "test-master-secret");
        let msg_a = auth_msg("create", "/x", "user-a");
        let msg_b = auth_msg("create", "/x", "user-b");
        assert_ne!(token(&ctx, &msg_a), token(&ctx, &msg_b));
    }

    #[tokio::test]
    async fn anonymous_subject_produces_a_stable_token() {
        let mut ctx = TestContext::new().await;
        ctx.set_config(crate::blocks::auth::JWT_SECRET_KEY, "test-master-secret");
        let msg1 = anon_msg("create", "/b/auth/api/bootstrap");
        let msg2 = anon_msg("create", "/b/auth/api/bootstrap");
        assert_eq!(token(&ctx, &msg1), token(&ctx, &msg2));
    }

    #[tokio::test]
    async fn verify_rejects_empty_or_wrong_token() {
        let mut ctx = TestContext::new().await;
        ctx.set_config(crate::blocks::auth::JWT_SECRET_KEY, "test-master-secret");
        let msg = auth_msg("create", "/b/userportal/update-profile", "user-1");
        assert!(!verify(&ctx, &msg, ""));
        assert!(!verify(&ctx, &msg, "not-the-right-token"));
    }

    #[tokio::test]
    async fn hidden_field_embeds_the_current_token() {
        let mut ctx = TestContext::new().await;
        ctx.set_config(crate::blocks::auth::JWT_SECRET_KEY, "test-master-secret");
        let msg = auth_msg("retrieve", "/b/userportal/profile", "user-1");
        let rendered = hidden_field(&ctx, &msg).into_string();
        assert!(rendered.contains(&format!(r#"name="{FIELD_NAME}""#)));
        assert!(rendered.contains(&token(&ctx, &msg)));
    }
}
