//! CSRF defenses for cookie-authenticated mutations.
//!
//! Cookie authentication (the `auth_token` cookie — see
//! `blocks::router::ImpresspressRouterBlock::handle`) relies on
//! `SameSite=Lax` alone unless this module is wired in. `SameSite=Lax` still
//! attaches the cookie to a cross-site *top-level* navigation using a "safe"
//! method, and to any request made via an unsafe method that a permissive
//! browser or a same-site (but attacker-influenced) subdomain can trigger, so
//! it is not a complete CSRF defense on its own.
//!
//! [`enforce_origin_policy`] — Fetch-Metadata (`Sec-Fetch-Site`) / `Origin` /
//! `Referer` validation for cookie-authenticated unsafe-method requests. This
//! is the BROAD net: called exactly once, from `pipeline::handle_request`,
//! before any block dispatch, so it covers every mutation uniformly. A
//! request authenticated by a real `Authorization: Bearer` header (not the
//! cookie) is exempt — a cross-site page has no ambient credential to attach
//! a bearer token with, so it cannot forge that request in the first place.
//!
//! A stateless per-form synchronizer token (defense-in-depth for the handful
//! of plain SSR `<form>` POSTs in this app) is layered on top of this module
//! in a follow-up change.

use wafer_run::{Message, OutputStream};

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
    // including same-origin ones. Only these three values indicate the
    // request originated from our own site (or from no web page at all —
    // "none" covers a typed URL / bookmark / browser extension); anything
    // else, including "cross-site" and any value this policy doesn't
    // recognize, is rejected.
    let sec_fetch_site = msg.header("sec-fetch-site");
    if !sec_fetch_site.is_empty() {
        let is_safe = matches!(
            sec_fetch_site.to_ascii_lowercase().as_str(),
            "same-origin" | "same-site" | "none"
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{anon_msg, auth_msg};

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
    fn same_site_and_none_are_allowed() {
        for value in ["same-site", "none", "SAME-ORIGIN"] {
            let mut msg = cookie_msg("update", "/b/admin/users/1", "user-1");
            msg.set_meta("http.header.sec-fetch-site", value);
            assert!(
                enforce_origin_policy(&msg, true).is_none(),
                "{value} should be allowed"
            );
        }
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
}
