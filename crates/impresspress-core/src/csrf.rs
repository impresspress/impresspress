//! CSRF defenses for cookie-authenticated mutations.
//!
//! Cookie authentication (the `auth_token` cookie — see
//! `blocks::router::ImpresspressRouterBlock::handle`) relies on
//! `SameSite=Lax` alone unless this module is wired in. `SameSite=Lax` still
//! attaches the cookie to a cross-site *top-level* navigation using a "safe"
//! method, and — because the cookie is host-only rather than scoped to a
//! specific subdomain — to any same-site cross-subdomain request using an
//! unsafe method, so it is not a complete CSRF defense on its own.
//!
//! Two independent layers, both centralized here so no handler re-implements
//! either:
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
//! 2. [`token`] / [`hidden_field`] / [`verify`] — a stateless synchronizer
//!    token for the plain (non-JS, non-`htmx`) SSR `<form>` POSTs in this
//!    app (bootstrap-admin redemption, profile update). Defense-in-depth on
//!    top of layer 1: even an allowed-origin POST must also carry the
//!    correct per-identity token, which nothing outside this same origin
//!    could produce or read.
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

/// KDF context label for the CSRF signing key (see [`primitives::derive_block_key`]).
/// Keeps the CSRF key cryptographically independent from the JWT verify key
/// derived (elsewhere) from the same master secret, even though both trace
/// back to `WAFER_RUN__AUTH__JWT_SECRET`.
const CSRF_KEY_LABEL: &str = "impresspress/csrf";

/// Subject used to key the token when no session is established yet (e.g.
/// the bootstrap-admin form, rendered and submitted with no `auth_token`
/// cookie at all). A constant subject is fine here — the anonymous case does
/// not need per-visitor uniqueness to defeat CSRF, only unguessability, and
/// an attacker's cross-site page can neither compute this HMAC (no master
/// secret) nor read it off our page (Same-Origin Policy).
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

/// Enforce the CSRF origin policy for one already-routed request.
///
/// Called from `pipeline::handle_request`, after the auth-meta extraction
/// step (so `msg.user_id()` reflects whether the cookie's JWT actually
/// verified) and before `routing::route_to_block`. Returns `Some(response)`
/// to short-circuit with a rejection, `None` to let the request proceed.
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
///   endpoints) is not meant to require this check at all.
/// - the method is unsafe (`is_unsafe_method`).
pub fn enforce_origin_policy(msg: &Message, cookie_authenticated: bool) -> Option<OutputStream> {
    if !cookie_authenticated || msg.user_id().is_empty() || !is_unsafe_method(msg.action()) {
        return None;
    }

    // Primary signal: Fetch Metadata. Sent by every modern browser for
    // essentially all requests (fetch/XHR, `<form>` submission, `htmx`),
    // including same-origin ones. Only these two values indicate the request
    // originated from our own origin (or from no web page at all — "none"
    // covers a typed URL / bookmark / browser extension); everything else,
    // including "same-site" and "cross-site" and any value this policy
    // doesn't recognize, is rejected. `same-site` is deliberately NOT
    // allowed: the `auth_token` cookie is host-only (no explicit `Domain`
    // attribute), but `SameSite=Lax` still attaches it to a same-site
    // cross-subdomain request, so a `same-site` classification does not
    // imply the request came from this app — it could come from any sibling
    // subdomain under the registrable domain. impresspress serves everything
    // same-origin, so there is no legitimate same-site-but-cross-subdomain
    // flow to accommodate.
    let sec_fetch_site = msg.header("sec-fetch-site");
    if !sec_fetch_site.is_empty() {
        let is_safe = matches!(
            sec_fetch_site.to_ascii_lowercase().as_str(),
            "same-origin" | "none"
        );
        return if is_safe {
            None
        } else {
            Some(crate::ui::forbidden_response(msg))
        };
    }

    // Fallback for clients that don't send Fetch Metadata (older browsers
    // predating Chrome 76 / Firefox 90 / Safari 16.4, or non-browser
    // clients presenting a cookie): validate Origin, then Referer, against
    // this request's own Host. Whichever is present wins; if neither is
    // present at all on an unsafe cookie-authenticated request, fail closed
    // rather than assume same-origin.
    let self_authority = normalize_authority(msg.header("host"));
    for header in ["origin", "referer"] {
        let value = msg.header(header);
        if value.is_empty() {
            continue;
        }
        let authority = request_authority(value);
        let allowed = authority.is_some() && authority == self_authority;
        return if allowed {
            None
        } else {
            Some(crate::ui::forbidden_response(msg))
        };
    }

    Some(crate::ui::forbidden_response(msg))
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
        assert!(enforce_origin_policy(&msg, true).is_none());
    }

    #[test]
    fn bearer_authenticated_request_is_exempt_even_when_cross_site() {
        // cookie_authenticated=false: a real Authorization header resolved
        // this credential, not the cookie fallback — never CSRF-able.
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "cross-site");
        assert!(enforce_origin_policy(&msg, false).is_none());
    }

    #[test]
    fn unauthenticated_cookie_is_not_blocked_here() {
        // cookie_authenticated=true but no user_id — the JWT didn't verify.
        // No session to protect; downstream `check_access` handles a
        // protected route, and a genuinely public route is untouched.
        let mut msg = cookie_msg("create", "/b/admin/users", "");
        msg.set_meta("http.header.sec-fetch-site", "cross-site");
        assert!(enforce_origin_policy(&msg, true).is_none());
    }

    // -- enforce_origin_policy: Sec-Fetch-Site ------------------------------

    #[tokio::test]
    async fn cross_site_cookie_authenticated_post_is_rejected() {
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "cross-site");
        let out = enforce_origin_policy(&msg, true).expect("must reject");
        assert!(crate::test_support::output_is_error(out, "PermissionDenied").await);
    }

    #[test]
    fn same_origin_cookie_authenticated_post_is_allowed() {
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "same-origin");
        assert!(enforce_origin_policy(&msg, true).is_none());
    }

    #[test]
    fn none_and_same_origin_are_allowed() {
        for value in ["none", "SAME-ORIGIN"] {
            let mut msg = cookie_msg("update", "/b/admin/users/1", "user-1");
            msg.set_meta("http.header.sec-fetch-site", value);
            assert!(
                enforce_origin_policy(&msg, true).is_none(),
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
        let out = enforce_origin_policy(&msg, true).expect("must reject");
        assert!(crate::test_support::output_is_error(out, "PermissionDenied").await);
    }

    #[tokio::test]
    async fn unrecognized_sec_fetch_site_value_is_rejected() {
        let mut msg = cookie_msg("delete", "/b/admin/users/1", "user-1");
        msg.set_meta("http.header.sec-fetch-site", "garbage");
        let out = enforce_origin_policy(&msg, true).expect("must reject");
        assert!(crate::test_support::output_is_error(out, "PermissionDenied").await);
    }

    // -- enforce_origin_policy: Origin/Referer fallback ---------------------

    #[test]
    fn matching_origin_is_allowed_when_no_sec_fetch_site() {
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta("http.header.origin", "https://impresspress.example.com");
        assert!(enforce_origin_policy(&msg, true).is_none());
    }

    #[tokio::test]
    async fn mismatched_origin_is_rejected() {
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta("http.header.origin", "https://evil.example");
        let out = enforce_origin_policy(&msg, true).expect("must reject");
        assert!(crate::test_support::output_is_error(out, "PermissionDenied").await);
    }

    #[test]
    fn matching_referer_is_allowed_when_origin_absent() {
        let mut msg = cookie_msg("create", "/b/admin/users", "user-1");
        msg.set_meta(
            "http.header.referer",
            "https://impresspress.example.com/b/admin/users",
        );
        assert!(enforce_origin_policy(&msg, true).is_none());
    }

    #[tokio::test]
    async fn no_fetch_metadata_no_origin_no_referer_is_rejected() {
        let msg = cookie_msg("create", "/b/admin/users", "user-1");
        let out = enforce_origin_policy(&msg, true).expect("must reject fail-closed");
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
        assert!(enforce_origin_policy(&msg, true).is_none());
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
