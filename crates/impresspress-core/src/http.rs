//! HTTP response construction for the streaming block protocol.
//!
//! The response sugar — `ResponseBuilder`, `ok_json`/`ok_empty`, and the
//! `err_*` constructors — is the canonical implementation in
//! [`wafer_block::response`] (the producer half of the cross-repo
//! response-sugar finding). This module re-exports it so impresspress keeps a
//! single import path (`crate::http::*`) without carrying a behaviourally
//! identical local copy (a local copy of an upstream surface is a shim).
//!
//! The impresspress-specific additions are [`redirect`], a thin convenience over
//! [`ResponseBuilder`] for the redirect response shape (status + `Location` +
//! empty `text/plain` body) used by page handlers, [`err_unavailable`],
//! the `503` constructor `wafer_block::response` does not (yet) carry, and
//! [`err_unauthenticated`], the `401` for a request with no identity, which
//! carries the [`WWW_AUTHENTICATE`] challenge.

pub use wafer_block::{
    err_bad_request, err_conflict, err_forbidden, err_internal, err_internal_no_cause,
    err_not_found, err_unauthorized, ok_empty, ok_json, ResponseBuilder,
};
use wafer_run::{ErrorCode, MetaEntry, OutputStream, WaferError};

/// The `WWW-Authenticate` challenge [`err_unauthenticated`] carries
/// (RFC 9110 §11.6.1): the two schemes the request pipeline accepts in
/// `Authorization` — an access token (`Bearer`, RFC 6750) and an API key
/// (`ApiKey`).
pub const WWW_AUTHENTICATE: &str = r#"Bearer realm="impresspress", ApiKey realm="impresspress""#;

/// `401` — the request has no identity: it carried no credential, or one
/// that did not verify (malformed, expired, signed out, issued before a
/// password change or a disable, an API key that matches no live row).
/// `pipeline::handle_request` treats all of those as anonymous, so by the
/// time a route checks `msg.user_id()` they are the same answer.
///
/// Every JSON refusal for an empty `msg.user_id()` answers with this: the
/// router's gate for an API caller and each handler's own check behind it.
/// (The user portal's two htmx buttons, sessions revoke and provider unlink,
/// answer `text/plain` for the swap and set [`WWW_AUTHENTICATE`] themselves.)
/// It carries the [`WWW_AUTHENTICATE`] challenge RFC 9110 §15.5.2 requires
/// on a `401`, and the detail code `not_authenticated`
/// ([`crate::blocks::errors::ErrorCode::NotAuthenticated`]).
///
/// It is for a missing identity only. A `401` about a credential that is not
/// an `Authorization` scheme — a wrong password at login, a bootstrap token,
/// a refresh token, a webhook signature — is answered by its own handler
/// without the challenge. A caller that IS identified but may not do this is
/// a `403` ([`err_forbidden`]); a credential the server could not check (its
/// database read failed) is neither, and keeps the status
/// `blocks::auth::credential_check_failed` gives it.
pub fn err_unauthenticated(message: &str) -> OutputStream {
    let mut error = WaferError::new(ErrorCode::Unauthenticated, message)
        .with_detail_code(crate::blocks::errors::ErrorCode::NotAuthenticated.as_str());
    error.meta.push(MetaEntry {
        key: format!(
            "{}WWW-Authenticate",
            wafer_block::meta::META_RESP_HEADER_PREFIX
        ),
        value: WWW_AUTHENTICATE.to_string(),
    });
    OutputStream::error(error)
}

/// The row, or the 404 its absence turns into.
///
/// The `Option` half of [`crate::blocks::crud::db_error`]'s sentence: a repo
/// function that returns `Result<Option<T>, WaferError>` splits "the call
/// failed" from "there is no such row", and this is the second half —
/// `db_error` on the `Result`, `require_row` on the `Option`:
///
/// ```ignore
/// let doc = require_row(
///     documents::get(ctx, id).await.map_err(|e| db_error(e, "Document not found", "Database error"))?,
///     "Document not found",
/// )?;
/// ```
///
/// `not_found` is the whole message, not a noun: the route knows what it was
/// looking for and this function does not. It lives here rather than in
/// `crud.rs` because it turns an `Option` into a response and has nothing to
/// do with the generic-table helpers.
pub fn require_row<T>(row: Option<T>, not_found: &str) -> Result<T, OutputStream> {
    row.ok_or_else(|| err_not_found(not_found))
}

/// `503` — a capability that is not configured is unavailable, not broken.
///
/// The sibling of the `err_*` constructors re-exported above, which
/// `wafer_block::response` has no 503 for. Reach for it when the request
/// arrived at a handler that works, on a deployment that is fine, but the
/// capability behind it is switched off — no keys set, no backend registered.
/// `500` claims this deployment failed; `503` says it did not and this one
/// feature is not turned on, which is the honest answer for the operator
/// reading logs and for the human caller. When the deployment really is
/// broken, `err_internal`/`err_internal_no_cause` remain the right answers.
///
/// The two PUBLIC Stripe endpoints — `POST /b/products/checkout` and
/// `POST /b/products/webhooks` — are the case that motivated it: both are
/// reachable on a default install where no Stripe keys are set, which is how
/// the 2026-09-10 live run recorded them as 500s.
///
/// It does NOT stop Stripe redelivering: Stripe retries on ANY non-2xx, 503
/// included, and no `Retry-After` is set here. The gain is an accurate
/// status, not fewer retries — an earlier version of this comment claimed
/// otherwise and was wrong.
pub fn err_unavailable(message: &str) -> OutputStream {
    OutputStream::error(WaferError::new(ErrorCode::Unavailable, message.to_string()))
}

/// Build a redirect `OutputStream` with the given status (302, 303, …) and
/// `Location` header. Single source of truth for the redirect response shape
/// (status + `Location` + empty `text/plain` body) used by page handlers.
pub fn redirect(status: u16, location: &str) -> OutputStream {
    ResponseBuilder::new()
        .status(status)
        .set_header("Location", location)
        .body(Vec::new(), "text/plain")
}

/// Conditional GET (`If-None-Match` → `304 Not Modified`).
///
/// Two routes serve fixed content at a STABLE url — no query string or path
/// segment carries a version, unlike the content-hashed `/b/static/*`
/// bundle — so revalidation is the only way a repeat visitor avoids
/// re-downloading bytes that have not changed: `/b/webmcp/webmcp.js`
/// (`pipeline.rs`) and `/b/dev/static/{dev.js,dev.css}`
/// (`blocks::dev::page`). Both already sent a quoted `ETag`, but nothing
/// compared it against a repeat visitor's `If-None-Match`, so the `304`
/// their `Cache-Control: no-cache` implied never actually fired — this
/// module is that comparison.
pub mod conditional {
    use wafer_run::{Message, OutputStream};

    use super::ResponseBuilder;

    /// If the request's `If-None-Match` header (RFC 9110 §13.1.2) names
    /// `etag` — already the quoted form, e.g. `"\"a1b2c3d4\""` — or is `*`,
    /// return the `304` this GET should answer with instead: no body, the
    /// same `ETag` and `Cache-Control` the `200` would have carried. `None`
    /// means the caller has no cached copy (or a stale one) and should send
    /// the full response.
    ///
    /// Per spec, `If-None-Match` uses the WEAK comparison function: a weak
    /// validator (`W/"..."`) and a strong one with the same opaque tag
    /// match — this is a cache-validation header, not an edit-conflict
    /// check (contrast `expected_sha256` in `blocks::dev::files`, which
    /// needs the STRONG comparison a byte-identity guarantee requires). A
    /// missing header, or one that names something else, is a normal `200`.
    pub fn not_modified(msg: &Message, etag: &str, cache_control: &str) -> Option<OutputStream> {
        let header = msg.get_meta("http.header.if-none-match");
        if header.is_empty() || !matches(header, etag) {
            return None;
        }
        Some(
            ResponseBuilder::new()
                .status(304)
                .set_header("Cache-Control", cache_control)
                .set_header("ETag", etag)
                .empty(),
        )
    }

    /// `*` matches unconditionally — RFC 9110 §13.1.2 makes it false only
    /// when the origin server has no current representation, and every
    /// caller of [`not_modified`] is serving one. Otherwise a
    /// comma-separated list of entity-tags, each optionally weak
    /// (`W/"..."`), any of which may match `etag` (the `W/` prefix ignored
    /// on both sides — the weak comparison function).
    fn matches(header: &str, etag: &str) -> bool {
        let header = header.trim();
        if header == "*" {
            return true;
        }
        let target = strip_weak(etag.trim());
        header
            .split(',')
            .map(str::trim)
            .any(|candidate| strip_weak(candidate) == target)
    }

    fn strip_weak(tag: &str) -> &str {
        tag.strip_prefix("W/").unwrap_or(tag)
    }

    #[cfg(test)]
    mod tests {
        use wafer_run::MetaGet;

        use super::*;

        fn msg_with_if_none_match(value: &str) -> Message {
            let mut m = Message::new("http.request");
            m.set_meta("http.header.if-none-match", value);
            m
        }

        #[test]
        fn no_header_means_no_304() {
            let m = Message::new("http.request");
            assert!(not_modified(&m, "\"abcd1234\"", "no-cache").is_none());
        }

        #[test]
        fn exact_strong_match_is_304() {
            let m = msg_with_if_none_match("\"abcd1234\"");
            assert!(not_modified(&m, "\"abcd1234\"", "no-cache").is_some());
        }

        #[test]
        fn mismatch_is_not_304() {
            let m = msg_with_if_none_match("\"deadbeef\"");
            assert!(not_modified(&m, "\"abcd1234\"", "no-cache").is_none());
        }

        #[test]
        fn weak_validator_on_the_request_still_matches_a_strong_etag() {
            let m = msg_with_if_none_match("W/\"abcd1234\"");
            assert!(not_modified(&m, "\"abcd1234\"", "no-cache").is_some());
        }

        #[test]
        fn comma_separated_list_matches_any_entry() {
            let m = msg_with_if_none_match("\"11111111\", \"abcd1234\", W/\"22222222\"");
            assert!(not_modified(&m, "\"abcd1234\"", "no-cache").is_some());
        }

        #[test]
        fn star_always_matches() {
            let m = msg_with_if_none_match("*");
            assert!(not_modified(&m, "\"anything\"", "no-cache").is_some());
        }

        #[tokio::test]
        async fn a_304_carries_no_body_and_the_same_etag_and_cache_control() {
            let m = msg_with_if_none_match("\"abcd1234\"");
            let out = not_modified(&m, "\"abcd1234\"", "no-cache").expect("should match");
            let buf = out.collect_buffered().await.expect("respond");
            assert!(buf.body.is_empty());
            assert_eq!(
                MetaGet::get(&buf.meta, wafer_run::META_RESP_STATUS),
                Some("304")
            );
            assert_eq!(
                MetaGet::get(&buf.meta, "resp.header.ETag"),
                Some("\"abcd1234\"")
            );
            assert_eq!(
                MetaGet::get(&buf.meta, "resp.header.Cache-Control"),
                Some("no-cache")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use wafer_run::{MetaGet, META_RESP_STATUS};

    use super::*;

    /// The whole point of the helper: `503`, not `500`, carrying the caller's
    /// message verbatim.
    #[tokio::test]
    async fn err_unavailable_is_a_503_carrying_the_message() {
        let out = err_unavailable("Stripe is not configured");
        match out.collect_buffered().await {
            Err(wafer_run::TerminalNotResponse::Error(e)) => {
                assert_eq!(wafer_block::http_codec::resolve_error_status(&e), 503);
                assert_eq!(e.message, "Stripe is not configured");
            }
            other => panic!("expected an error terminal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn redirect_sets_status_and_location() {
        let buf = redirect(303, "/login")
            .collect_buffered()
            .await
            .expect("respond");
        assert!(buf.body.is_empty());
        assert_eq!(MetaGet::get(&buf.meta, META_RESP_STATUS), Some("303"));
        assert_eq!(
            MetaGet::get(&buf.meta, "resp.header.Location"),
            Some("/login")
        );
    }
}

#[cfg(test)]
mod require_row_tests {
    use wafer_run::{MetaGet, META_RESP_STATUS};

    use super::*;

    #[tokio::test]
    async fn a_present_row_passes_through() {
        assert_eq!(require_row(Some(7_u8), "User not found").ok(), Some(7));
    }

    #[tokio::test]
    async fn an_absent_row_is_a_404_carrying_the_label() {
        let out = require_row(None::<u8>, "User not found").expect_err("None is a refusal");
        match out.collect_buffered().await {
            Err(wafer_run::TerminalNotResponse::Error(e)) => {
                assert_eq!(wafer_block::http_codec::resolve_error_status(&e), 404);
                assert_eq!(e.message, "User not found");
            }
            other => panic!("expected an error terminal, got {other:?}"),
        }
    }

    /// `?` on the refusal hands the handler an `OutputStream` it returns
    /// as-is — the shape every call site uses.
    #[tokio::test]
    async fn the_refusal_is_returnable_from_a_handler() {
        fn handler(row: Option<u8>) -> OutputStream {
            match require_row(row, "Document not found") {
                Ok(_) => ResponseBuilder::new().status(200).empty(),
                Err(refusal) => refusal,
            }
        }
        let buf = handler(Some(1)).collect_buffered().await.expect("respond");
        assert_eq!(MetaGet::get(&buf.meta, META_RESP_STATUS), Some("200"));
        assert!(handler(None).collect_buffered().await.is_err());
    }
}
