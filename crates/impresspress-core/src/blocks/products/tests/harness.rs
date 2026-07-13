//! Test harness for the products block.
//!
//! Backs every products test on the crate-wide [`TestContext`], which wires
//! the production `DatabaseBlock` onto an in-memory SQLite database with the
//! products migrations applied. This replaces the former 657-line
//! `mock_context.rs` wire-codec fake — tests now exercise the real
//! `wafer-sql-utils` statements the repo layer builds (and the real schema
//! constraints, which the fake silently ignored).

use std::collections::HashMap;

use wafer_run::{ErrorCode, InputStream, Message, OutputStream};

use crate::test_support::TestContext;

/// Build a `TestContext` with the products (and admin) migrations applied.
pub async fn ctx() -> TestContext {
    TestContext::with_products().await
}

/// Build a products `TestContext` with `config` entries pre-populated.
pub async fn ctx_with(config: &[(&str, &str)]) -> TestContext {
    let mut ctx = TestContext::with_products().await;
    for (k, v) in config {
        ctx.set_config(k, v);
    }
    ctx
}

/// Insert a record directly for test setup, honoring the supplied `id`.
///
/// Writes through the production database client, so the row must satisfy the
/// table's schema (NOT NULL columns without a default must be supplied). The
/// db layer stamps `created_at`/`updated_at` and synthesizes missing optional
/// columns, matching production create behavior.
pub async fn seed(
    ctx: &TestContext,
    collection: &str,
    id: &str,
    data: HashMap<String, serde_json::Value>,
) {
    use wafer_core::clients::database as db;
    let mut data = data;
    data.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    db::create(ctx, collection, data).await.unwrap_or_else(|e| {
        panic!(
            "seed into {collection} failed: {} ({:?})",
            e.message, e.code
        )
    });
}

// --- Test message builders ---

/// Build a request message with JSON body, action, path, and user_id.
/// Returns `(Message, InputStream)` — the body is delivered via the input
/// stream in the streaming protocol, not via the message.
pub fn request_msg(
    action: &str,
    path: &str,
    user_id: &str,
    body: serde_json::Value,
) -> (Message, InputStream) {
    let data = serde_json::to_vec(&body).unwrap();
    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", action);
    msg.set_meta("req.resource", path);
    if !user_id.is_empty() {
        msg.set_meta("auth.user_id", user_id);
    }
    (msg, InputStream::from_bytes(data))
}

/// Build a GET (`retrieve`) request.
pub fn get_msg(path: &str, user_id: &str) -> (Message, InputStream) {
    request_msg("retrieve", path, user_id, serde_json::json!({}))
}

/// Build a POST/create request.
pub fn create_msg(path: &str, user_id: &str, body: serde_json::Value) -> (Message, InputStream) {
    request_msg("create", path, user_id, body)
}

/// Build a PATCH/update request.
pub fn update_msg(path: &str, user_id: &str, body: serde_json::Value) -> (Message, InputStream) {
    request_msg("update", path, user_id, body)
}

/// Build a DELETE request.
pub fn delete_msg(path: &str, user_id: &str) -> (Message, InputStream) {
    request_msg("delete", path, user_id, serde_json::json!({}))
}

/// Build an admin GET request (user `admin_1`, role `admin`).
pub fn admin_get_msg(path: &str) -> (Message, InputStream) {
    let (mut msg, input) = get_msg(path, "admin_1");
    msg.set_meta("auth.user_roles", "admin");
    (msg, input)
}

/// Build an admin create request (user `admin_1`, role `admin`).
pub fn admin_create_msg(path: &str, body: serde_json::Value) -> (Message, InputStream) {
    let (mut msg, input) = create_msg(path, "admin_1", body);
    msg.set_meta("auth.user_roles", "admin");
    (msg, input)
}

/// Dispatch an admin request the way `ProductsBlock::handle` does, but for
/// tests that build the message with the normalized `/admin/b/products/...`
/// path already in `req.resource`: the normalized sub-path is now an explicit
/// argument (no `req.resource` rewrite), so here it equals `msg.path()`.
pub async fn dispatch_admin(
    ctx: &TestContext,
    mut msg: Message,
    input: InputStream,
) -> OutputStream {
    let norm = msg.path().to_string();
    super::super::handlers::handle_admin(ctx, &mut msg, &norm, input).await
}

/// Dispatch a user request the same way (normalized `/b/products/...` path is
/// already in `req.resource` for these tests).
pub async fn dispatch_user(
    ctx: &TestContext,
    mut msg: Message,
    input: InputStream,
) -> OutputStream {
    let norm = msg.path().to_string();
    super::super::handlers::handle_user(ctx, &mut msg, &norm, input).await
}

/// Collect an `OutputStream`'s body and decode it as JSON.
/// Returns `Value::Null` if the stream did not terminate with `Complete`.
pub async fn output_to_json(out: OutputStream) -> serde_json::Value {
    match out.collect_buffered().await {
        Ok(buf) => serde_json::from_slice(&buf.body).unwrap_or(serde_json::Value::Null),
        Err(_) => serde_json::Value::Null,
    }
}

/// Collect an `OutputStream`'s body and decode it as a UTF-8 string (for
/// asserting on rendered SSR HTML). Empty string if the stream errored.
pub async fn output_to_html(out: OutputStream) -> String {
    match out.collect_buffered().await {
        Ok(buf) => String::from_utf8(buf.body).unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/// Check if an `OutputStream` terminated with an error of the given code.
pub async fn output_is_error(out: OutputStream, expected: ErrorCode) -> bool {
    use wafer_run::streams::output::TerminalNotResponse;
    matches!(
        out.collect_buffered().await,
        Err(TerminalNotResponse::Error(e)) if e.code == expected
    )
}
