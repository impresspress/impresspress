//! What a failed read answers on the legal pages.
//!
//! A WRAP `PermissionDenied` — a deployment that never granted the block its
//! own table or settings — is a 403 at both of its reads, not a 500.
//! Each test drives the real route through [`LegalPagesBlock`]'s own dispatch
//! as a caller holding no grants, so the refusal is the one
//! `wrap::check_access` gives, and every read before the site under test
//! (the config reads that fall back to their defaults) runs for real.

use wafer_run::{
    streams::output::TerminalNotResponse, Block, ErrorCode, InputStream, OutputStream,
};

use super::{test_ctx, LegalPagesBlock};
use crate::test_support::{admin_msg, anon_msg, TestContext};

/// A legalpages deployment whose caller holds no WRAP grants.
async fn ungranted() -> TestContext {
    test_ctx().await.running_as("test/ungranted")
}

async fn dispatch(ctx: &TestContext, msg: wafer_run::Message) -> OutputStream {
    LegalPagesBlock::new()
        .handle(ctx, msg, InputStream::empty())
        .await
}

/// The public terms page's published-document read, refused, is the door's
/// 403 — not the sanitized 500 `err_internal` gives.
#[tokio::test]
async fn a_refused_published_document_read_is_403() {
    let ctx = ungranted().await;
    match dispatch(&ctx, anon_msg("retrieve", "/b/legalpages/terms"))
        .await
        .collect_buffered()
        .await
    {
        Err(TerminalNotResponse::Error(error)) => assert_eq!(
            (error.code, error.message.as_str()),
            (ErrorCode::PermissionDenied, "Access denied")
        ),
        Ok(_) => panic!("expected the door's WRAP denial, got a response"),
        Err(_) => panic!("expected the door's WRAP denial, got another terminal"),
    }
}

/// The settings page renders every value through the config service, which
/// WRAP guards like the database. A refusal is the 403 page, not the 500
/// page, and carries none of the denial's own text.
#[tokio::test]
async fn a_refused_settings_read_is_the_403_page() {
    let ctx = ungranted().await;
    let mut msg = admin_msg("retrieve", "/b/legalpages/admin/settings");
    msg.set_meta("http.header.accept", "text/html");
    let parts = wafer_block::http_codec::collect_http_response(dispatch(&ctx, msg).await).await;
    let html = String::from_utf8_lossy(&parts.body);
    assert_eq!(parts.status, 403, "{html}");
    assert!(html.contains("Go home"), "{html}");
    assert!(!html.contains("WRAP"), "{html}");
}
