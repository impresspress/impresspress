//! HTTP response construction for the streaming block protocol.
//!
//! The response sugar — `ResponseBuilder`, `ok_json`/`ok_empty`, and the
//! `err_*` constructors — is the canonical implementation in
//! [`wafer_block::response`] (the producer half of the cross-repo
//! response-sugar finding). This module re-exports it so impresspress keeps a
//! single import path (`crate::http::*`) without carrying a behaviourally
//! identical local copy (a local copy of an upstream surface is a shim).
//!
//! The only impresspress-specific addition is [`redirect`], a thin convenience over
//! [`ResponseBuilder`] for the redirect response shape (status + `Location` +
//! empty `text/plain` body) used by page handlers.

pub use wafer_block::{
    err_bad_request, err_conflict, err_forbidden, err_internal, err_internal_no_cause,
    err_not_found, err_unauthorized, ok_empty, ok_json, ResponseBuilder,
};
use wafer_run::OutputStream;

/// Build a redirect `OutputStream` with the given status (302, 303, …) and
/// `Location` header. Single source of truth for the redirect response shape
/// (status + `Location` + empty `text/plain` body) used by page handlers.
pub fn redirect(status: u16, location: &str) -> OutputStream {
    ResponseBuilder::new()
        .status(status)
        .set_header("Location", location)
        .body(Vec::new(), "text/plain")
}

#[cfg(test)]
mod tests {
    use wafer_run::{MetaGet, META_RESP_STATUS};

    use super::*;

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
