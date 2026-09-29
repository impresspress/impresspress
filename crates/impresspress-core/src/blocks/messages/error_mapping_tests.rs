//! What a failed read answers on the messages admin pages.
//!
//! A WRAP `PermissionDenied` is the styled 403 page (`crud::db_error_page`),
//! not the 500 page, at all three of its reads. Each test drives the real
//! page through [`MessagesBlock`]'s own dispatch over a context that refuses
//! the one read the site under test makes.

use wafer_block::ServiceOp;
use wafer_run::{Block, ErrorCode, InputStream, WaferError};

use super::{
    service::{self, CONTEXTS_TABLE, ENTRIES_TABLE},
    test_support::ctx_with_messages,
    MessagesBlock,
};
use crate::test_support::{admin_msg, FailingDbOpContext, TestContext};

/// `ctx` with `(op, table)` refused the way WRAP refuses a call its caller
/// holds no grant for.
fn denied(ctx: &TestContext, ops: Vec<(&'static str, &'static str)>) -> FailingDbOpContext {
    FailingDbOpContext::failing_with(
        ctx.clone(),
        ops,
        WaferError::new(
            ErrorCode::PermissionDenied,
            "WRAP: impresspress/messages holds no grant on this table",
        ),
    )
}

/// Records a miss unless `path` answers the styled 403 page, with none of
/// the denial's own text.
async fn expect_refused_page(
    misses: &mut Vec<String>,
    ctx: &dyn wafer_run::context::Context,
    path: &str,
) {
    let mut msg = admin_msg("retrieve", path);
    msg.set_meta("http.header.accept", "text/html");
    let out = MessagesBlock::new()
        .handle(ctx, msg, InputStream::empty())
        .await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let html = String::from_utf8_lossy(&parts.body);
    if parts.status != 403 || !html.contains("Go home") || html.contains("holds no grant") {
        misses.push(format!("{path}: {} {html}", parts.status));
    }
}

#[tokio::test]
async fn refused_page_reads_are_the_403_page() {
    let ctx = ctx_with_messages().await;
    let conversation =
        service::create_context(&ctx, "user-a", "conversation", "T", "", "", None, None)
            .await
            .expect("seed a conversation");
    let detail = format!("/b/messages/contexts/{}", conversation.id);
    let mut misses = Vec::new();

    expect_refused_page(
        &mut misses,
        &denied(&ctx, vec![(ServiceOp::DATABASE_LIST, CONTEXTS_TABLE)]),
        "/b/messages/",
    )
    .await;
    expect_refused_page(
        &mut misses,
        &denied(&ctx, vec![(ServiceOp::DATABASE_LIST, ENTRIES_TABLE)]),
        &detail,
    )
    .await;
    // The context itself is a `get`; the sibling conversations are the list.
    expect_refused_page(
        &mut misses,
        &denied(&ctx, vec![(ServiceOp::DATABASE_LIST, CONTEXTS_TABLE)]),
        &detail,
    )
    .await;

    assert!(
        misses.is_empty(),
        "expected the 403 page at every site:\n{}",
        misses.join("\n")
    );
}
