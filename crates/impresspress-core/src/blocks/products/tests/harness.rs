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

/// Build a `TestContext` with the products (and admin) migrations applied,
/// running as `impresspress/products`.
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
/// Written from the fixture's own frame ([`TestContext::fixture`]): the
/// products frame [`ctx`] runs in holds no grant on the other blocks' tables
/// a scenario stages (auth users, admin roles), and staging is not the
/// block's own doing.
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
    db::create(&ctx.fixture(), collection, data)
        .await
        .unwrap_or_else(|e| {
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

/// Dispatch a request the way the runtime does once the router has admitted
/// it: through `ProductsBlock::handle`, so the route table, the route's
/// rate-limit bucket and the `ALLOW_USER_PRODUCTS` / seller-suspension gates
/// all run. `req.resource` is the wire path. A fresh `ProductsBlock` per
/// call means a fresh limiter, so no test spends another's quota.
pub async fn dispatch(
    ctx: &dyn wafer_run::context::Context,
    msg: Message,
    input: InputStream,
) -> OutputStream {
    use wafer_run::Block;

    super::super::ProductsBlock::new()
        .handle(ctx, msg, input)
        .await
}

/// Run `msg` through the block's route table so `{id}` (and the other path
/// variables) are bound the way they are on the wire, then hand the message
/// to a handler directly. Panics when no row matches: a test that sends an
/// unroutable path would otherwise exercise the handler's "missing id"
/// branch by accident.
pub fn routed(mut msg: Message) -> Message {
    let route = crate::endpoint_match::dispatch(&mut msg, super::super::routes::ROUTES);
    assert!(
        route.is_some(),
        "no products route matches {} {}",
        msg.action(),
        msg.path()
    );
    msg
}

/// Dispatch a request through the central router — `route_to_block` →
/// `check_access` → `ProductsBlock::handle` — rather than straight into the
/// block.
///
/// `dispatch` above enters the block *below* the layer that enforces a
/// declared endpoint's `AuthLevel`, so it can prove a handler's behaviour but
/// never its authorization tier. A test about which tier may invoke an
/// endpoint must use this.
///
/// The caller is the one the message's `auth.*` meta names, taken as
/// resolved ([`TestContext::dispatch_resolved_with_input`]): these tests are
/// about the tier and ownership rules given a caller, keyed to the fixture's
/// seeded owner ids. How a token or key becomes that caller is the request
/// preamble's, driven by `TestContext::request` in `tests/request_preamble.rs`.
pub async fn dispatch_routed(ctx: &TestContext, msg: Message, input: InputStream) -> OutputStream {
    ctx.dispatch_resolved_with_input(msg, input).await
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

/// Assert that `html` loads the products bundle `logical`, and hand back that
/// bundle's source.
///
/// The pages delegate most of their behaviour to JavaScript, and that
/// JavaScript is served from `/b/static/` rather than inlined into the page.
/// A page test that wants to assert on it therefore has two halves to check,
/// and this is both of them at once: the page's `<script src>` (carrying the
/// bundle's content hash, so a rename cannot pass) and the bundle's own
/// source. Asserting only the second would pass for a page that never loads
/// the file — which is exactly what these assertions would have degraded into
/// when the source moved out of the page.
#[cfg(feature = "embed-assets")]
pub fn loaded_bundle(html: &str, logical: &str) -> &'static str {
    let url = crate::ui::assets::url(logical);
    assert!(
        html.contains(&format!("<script src=\"{url}\"")),
        "the page must load {logical} from {url}"
    );
    let bytes = super::super::assets::bytes(logical)
        .unwrap_or_else(|| panic!("{logical} is not an embedded products asset"));
    std::str::from_utf8(bytes).expect("asset source is UTF-8")
}

/// Check if an `OutputStream` terminated with an error of the given code.
pub async fn output_is_error(out: OutputStream, expected: ErrorCode) -> bool {
    use wafer_run::streams::output::TerminalNotResponse;
    matches!(
        out.collect_buffered().await,
        Err(TerminalNotResponse::Error(e)) if e.code == expected
    )
}

/// Seed the `syncing` row a first Payment Link synchronization attempt
/// writes: no preset, the platform account, test mode, no application fee and
/// no Stripe request in flight. Returns the row.
pub async fn seed_pending_payment_link(
    ctx: &TestContext,
    offer_id: &str,
    configuration_hash: &str,
    pricing_snapshot: &crate::blocks::products::contracts::PricingPreview,
) -> crate::blocks::products::repo::payment_links::StoredPaymentLink {
    use crate::blocks::products::repo::payment_links;

    let id = payment_links::pending_id(ctx, offer_id, "", configuration_hash)
        .await
        .expect("the pending row id");
    payment_links::create_pending(
        ctx,
        &id,
        offer_id,
        "",
        false,
        configuration_hash,
        &payment_links::Attempt {
            seller_account_id: "",
            stripe_account_id: "",
            pricing_snapshot,
            fee_basis_points: 0,
            request: &[],
        },
    )
    .await
    .expect("a pending Payment Link")
}
