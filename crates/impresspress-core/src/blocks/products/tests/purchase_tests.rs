use std::collections::HashMap;

use wafer_run::ErrorCode;

use super::harness::*;
use crate::blocks::products::purchase;

// ============================================================
// Purchase creation — happy path
// ============================================================

#[tokio::test]
async fn create_purchase_single_item() {
    let ctx = ctx().await;

    // Seed a product with base_price
    let mut product = HashMap::new();
    product.insert("name".to_string(), serde_json::json!("Widget"));
    product.insert("base_price".to_string(), serde_json::json!(19.99));
    product.insert("currency".to_string(), serde_json::json!("USD"));
    product.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "prod_1", product).await;

    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": [{"product_id": "prod_1", "quantity": 2}]
        }),
    );

    let out = purchase::handle_create(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    assert!(body["id"].as_str().is_some());
    assert_eq!(body["status"], "pending");
    // 19.99 * 2 = 39.98 → 3998 cents
    assert_eq!(body["total_cents"].as_i64().unwrap(), 3998);
    assert_eq!(body["item_count"].as_i64().unwrap(), 1);
}

#[tokio::test]
async fn create_purchase_multiple_items() {
    let ctx = ctx().await;

    let mut p1 = HashMap::new();
    p1.insert("name".to_string(), serde_json::json!("Item A"));
    p1.insert("base_price".to_string(), serde_json::json!(10.0));
    p1.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "pa", p1).await;

    let mut p2 = HashMap::new();
    p2.insert("name".to_string(), serde_json::json!("Item B"));
    p2.insert("base_price".to_string(), serde_json::json!(25.50));
    p2.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "pb", p2).await;

    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": [
                {"product_id": "pa", "quantity": 1},
                {"product_id": "pb", "quantity": 3}
            ]
        }),
    );

    let out = purchase::handle_create(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    // 10.0*1 + 25.50*3 = 10 + 76.50 = 86.50 → 8650 cents
    assert_eq!(body["total_cents"].as_i64().unwrap(), 8650);
    assert_eq!(body["item_count"].as_i64().unwrap(), 2);
}

#[tokio::test]
async fn create_purchase_with_pricing_template() {
    let ctx = ctx().await;

    // Pricing template with formula
    let mut tmpl = HashMap::new();
    tmpl.insert("name".to_string(), serde_json::json!("per-unit"));
    tmpl.insert(
        "price_formula".to_string(),
        serde_json::json!("base * rate"),
    );
    seed(
        &ctx,
        "impresspress__products__pricing_templates",
        "tmpl_1",
        tmpl,
    )
    .await;

    // Product referencing the template
    let mut product = HashMap::new();
    product.insert("name".to_string(), serde_json::json!("Service"));
    product.insert(
        "pricing_template_id".to_string(),
        serde_json::json!("tmpl_1"),
    );
    product.insert("status".to_string(), serde_json::json!("active"));
    seed(
        &ctx,
        "impresspress__products__products",
        "prod_svc",
        product,
    )
    .await;

    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": [{
                "product_id": "prod_svc",
                "quantity": 2,
                "variables": {"base": 100.0, "rate": 0.15}
            }]
        }),
    );

    let out = purchase::handle_create(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    // formula: base * rate = 100 * 0.15 = 15.0 per unit, qty=2, total = 30.0 → 3000 cents
    assert_eq!(body["total_cents"].as_i64().unwrap(), 3000);
}

#[tokio::test]
async fn create_purchase_defaults_to_usd() {
    let ctx = ctx().await;

    let mut product = HashMap::new();
    product.insert("name".to_string(), serde_json::json!("Widget"));
    product.insert("base_price".to_string(), serde_json::json!(5.0));
    product.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "p1", product).await;

    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": [{"product_id": "p1", "quantity": 1}]
        }),
    );

    let out = purchase::handle_create(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    // Currency defaults to USD — verify purchase was created
    assert_eq!(body["status"], "pending");
}

// ============================================================
// Purchase creation — error cases
// ============================================================

#[tokio::test]
async fn create_purchase_empty_items() {
    let ctx = ctx().await;

    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": []
        }),
    );

    let out = purchase::handle_create(&ctx, &msg, input).await;
    assert!(output_is_error(out, ErrorCode::InvalidArgument).await);
}

#[tokio::test]
async fn create_purchase_zero_quantity() {
    let ctx = ctx().await;

    let mut product = HashMap::new();
    product.insert("name".to_string(), serde_json::json!("Widget"));
    product.insert("base_price".to_string(), serde_json::json!(10.0));
    seed(&ctx, "impresspress__products__products", "p1", product).await;

    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": [{"product_id": "p1", "quantity": 0}]
        }),
    );

    let out = purchase::handle_create(&ctx, &msg, input).await;
    assert!(output_is_error(out, ErrorCode::InvalidArgument).await);
}

#[tokio::test]
async fn create_purchase_negative_quantity() {
    let ctx = ctx().await;

    let mut product = HashMap::new();
    product.insert("name".to_string(), serde_json::json!("Widget"));
    product.insert("base_price".to_string(), serde_json::json!(10.0));
    seed(&ctx, "impresspress__products__products", "p1", product).await;

    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": [{"product_id": "p1", "quantity": -1}]
        }),
    );

    let out = purchase::handle_create(&ctx, &msg, input).await;
    assert!(output_is_error(out, ErrorCode::InvalidArgument).await);
}

#[tokio::test]
async fn create_purchase_product_not_found() {
    let ctx = ctx().await;

    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": [{"product_id": "nonexistent", "quantity": 1}]
        }),
    );

    let out = purchase::handle_create(&ctx, &msg, input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn create_purchase_fallback_when_template_missing() {
    let ctx = ctx().await;

    // Product references a template that doesn't exist — should fall back to base_price
    let mut product = HashMap::new();
    product.insert("name".to_string(), serde_json::json!("Fallback"));
    product.insert("base_price".to_string(), serde_json::json!(42.0));
    product.insert(
        "pricing_template_id".to_string(),
        serde_json::json!("nonexistent_tmpl"),
    );
    product.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "p_fb", product).await;

    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": [{"product_id": "p_fb", "quantity": 1}]
        }),
    );

    let out = purchase::handle_create(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    // Should fall back to base_price = 42.0 → 4200 cents
    assert_eq!(body["total_cents"].as_i64().unwrap(), 4200);
}

#[tokio::test]
async fn create_purchase_no_base_price_rejected() {
    let ctx = ctx().await;

    // Product with no base_price and no template — zero-price purchases are rejected
    let mut product = HashMap::new();
    product.insert("name".to_string(), serde_json::json!("Free"));
    product.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "p_free", product).await;

    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": [{"product_id": "p_free", "quantity": 5}]
        }),
    );

    let out = purchase::handle_create(&ctx, &msg, input).await;
    assert!(
        output_is_error(out, ErrorCode::InvalidArgument).await,
        "zero-price purchases should be rejected"
    );
}

// ============================================================
// Purchase listing
// ============================================================

#[tokio::test]
async fn list_user_purchases_only_own() {
    let ctx = ctx().await;

    // Seed purchases for two different users
    let mut p1 = HashMap::new();
    p1.insert("user_id".to_string(), serde_json::json!("user_1"));
    p1.insert("status".to_string(), serde_json::json!("pending"));
    p1.insert("total_cents".to_string(), serde_json::json!(1000));
    seed(&ctx, "impresspress__products__purchases", "pur_1", p1).await;

    let mut p2 = HashMap::new();
    p2.insert("user_id".to_string(), serde_json::json!("user_2"));
    p2.insert("status".to_string(), serde_json::json!("completed"));
    p2.insert("total_cents".to_string(), serde_json::json!(2000));
    seed(&ctx, "impresspress__products__purchases", "pur_2", p2).await;

    let (msg, _input) = get_msg("/b/products/purchases", "user_1");
    let out = purchase::handle_list_user(&ctx, &msg).await;
    let body = output_to_json(out).await;
    let records = body["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["id"], "pur_1");
}

// ============================================================
// Purchase detail retrieval
// ============================================================

#[tokio::test]
async fn get_purchase_own() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("pending"));
    pd.insert("total_cents".to_string(), serde_json::json!(5000));
    seed(&ctx, "impresspress__products__purchases", "pur_own", pd).await;

    let (msg, _input) = get_msg("/b/products/purchases/pur_own", "user_1");
    let out = purchase::handle_get(&ctx, &msg).await;
    let body = output_to_json(out).await;
    assert_eq!(body["purchase"]["id"], "pur_own");
}

#[tokio::test]
async fn get_purchase_denied_for_other_user() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("pending"));
    seed(&ctx, "impresspress__products__purchases", "pur_priv", pd).await;

    // user_2 tries to access user_1's purchase
    let (msg, _input) = get_msg("/b/products/purchases/pur_priv", "user_2");
    let out = purchase::handle_get(&ctx, &msg).await;
    assert!(output_is_error(out, ErrorCode::PermissionDenied).await);
}

#[tokio::test]
async fn get_purchase_not_found() {
    let ctx = ctx().await;

    let (msg, _input) = get_msg("/b/products/purchases/nonexistent", "user_1");
    let out = purchase::handle_get(&ctx, &msg).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn get_purchase_admin_can_view_any() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("completed"));
    seed(&ctx, "impresspress__products__purchases", "pur_any", pd).await;

    let (mut msg, _input) = get_msg("/b/products/purchases/pur_any", "admin_1");
    msg.set_meta("auth.user_roles", "admin");
    let out = purchase::handle_get(&ctx, &msg).await;
    let body = output_to_json(out).await;
    assert!(body["purchase"]["id"].as_str().is_some());
}

// ============================================================
// Refund
// ============================================================

#[tokio::test]
async fn refund_completed_purchase() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("completed"));
    pd.insert("total_cents".to_string(), serde_json::json!(5000));
    seed(&ctx, "impresspress__products__purchases", "pur_refund", pd).await;

    let (mut msg, input) = create_msg(
        "/admin/b/products/purchases/pur_refund/refund",
        "admin_1",
        serde_json::json!({"reason": "Customer requested"}),
    );
    msg.set_meta("auth.user_roles", "admin");

    let out = purchase::handle_refund(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["data"]["status"], "refunded");
    assert_eq!(body["data"]["refund_reason"], "Customer requested");
    assert!(body["data"]["refunded_by"].as_str().is_some());
}

#[tokio::test]
async fn refund_non_completed_fails() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("pending"));
    seed(&ctx, "impresspress__products__purchases", "pur_pending", pd).await;

    let (mut msg, input) = create_msg(
        "/admin/b/products/purchases/pur_pending/refund",
        "admin_1",
        serde_json::json!({}),
    );
    msg.set_meta("auth.user_roles", "admin");

    let out = purchase::handle_refund(&ctx, &msg, input).await;
    assert!(output_is_error(out, ErrorCode::InvalidArgument).await);
}

#[tokio::test]
async fn refund_already_refunded_fails() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("refunded"));
    seed(&ctx, "impresspress__products__purchases", "pur_already", pd).await;

    let (mut msg, input) = create_msg(
        "/admin/b/products/purchases/pur_already/refund",
        "admin_1",
        serde_json::json!({}),
    );
    msg.set_meta("auth.user_roles", "admin");

    let out = purchase::handle_refund(&ctx, &msg, input).await;
    assert!(output_is_error(out, ErrorCode::InvalidArgument).await);
}

#[tokio::test]
async fn refund_purchase_not_found() {
    let ctx = ctx().await;

    let (mut msg, input) = create_msg(
        "/admin/b/products/purchases/nonexistent/refund",
        "admin_1",
        serde_json::json!({}),
    );
    msg.set_meta("auth.user_roles", "admin");

    let out = purchase::handle_refund(&ctx, &msg, input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn refund_without_reason() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("completed"));
    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur_noreason",
        pd,
    )
    .await;

    let (mut msg, input) = create_msg(
        "/admin/b/products/purchases/pur_noreason/refund",
        "admin_1",
        serde_json::json!({}),
    );
    msg.set_meta("auth.user_roles", "admin");

    let out = purchase::handle_refund(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["data"]["status"], "refunded");
}

/// CODE_REVIEW_2026-07-16 "Error semantics fabricate successful defaults":
/// malformed refund JSON must be rejected, not silently treated as "no
/// reason given" (`unwrap_or_default()` used to swallow the parse error).
/// The purchase must be left untouched — no fabricated refund out of a
/// broken request body.
#[tokio::test]
async fn refund_rejects_malformed_json_body() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("completed"));
    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur_malformed",
        pd,
    )
    .await;

    let mut msg = wafer_run::Message::new("http.request");
    msg.set_meta("req.action", "create");
    msg.set_meta(
        "req.resource",
        "/admin/b/products/purchases/pur_malformed/refund",
    );
    msg.set_meta("auth.user_id", "admin_1");
    msg.set_meta("auth.user_roles", "admin");
    let input = wafer_run::InputStream::from_bytes(b"{not valid json".to_vec());

    let out = purchase::handle_refund(&ctx, &msg, input).await;
    assert!(
        output_is_error(out, ErrorCode::InvalidArgument).await,
        "malformed refund body must be rejected as a bad request"
    );

    let record = super::super::repo::purchases::get(&ctx, "pur_malformed")
        .await
        .expect("purchase still exists");
    assert_eq!(
        record.data.get("status").and_then(|v| v.as_str()),
        Some("completed"),
        "a malformed body must not fabricate a refund"
    );
}

/// A genuine repository failure while applying the refund must surface as an
/// internal-server error, not be folded into the same `rows == 0` branch as
/// the legitimate "purchase isn't in `completed` status" business outcome —
/// `unwrap_or(0)` used to conflate the two, reporting a real outage as the
/// same 400 "can only refund completed purchases" message.
#[tokio::test]
async fn refund_repository_failure_surfaces_as_internal_error() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("completed"));
    seed(&ctx, "impresspress__products__purchases", "pur_outage", pd).await;

    let ctx = ctx.break_writes();

    let (mut msg, input) = create_msg(
        "/admin/b/products/purchases/pur_outage/refund",
        "admin_1",
        serde_json::json!({"reason": "Customer requested"}),
    );
    msg.set_meta("auth.user_roles", "admin");

    let out = purchase::handle_refund(&ctx, &msg, input).await;
    assert!(
        output_is_error(out, ErrorCode::Internal).await,
        "a genuine repository failure must surface as Internal, not the \
         business-rule 400 used for an already-settled purchase"
    );
}

// ============================================================
// Purchase via user handler routing
// ============================================================

#[tokio::test]
async fn purchase_create_via_user_handler() {
    let ctx = ctx().await;

    let mut product = HashMap::new();
    product.insert("name".to_string(), serde_json::json!("Routed Product"));
    product.insert("base_price".to_string(), serde_json::json!(10.0));
    product.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "p_route", product).await;

    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": [{"product_id": "p_route", "quantity": 1}]
        }),
    );

    let out = dispatch_user(&ctx, msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["total_cents"].as_i64().unwrap(), 1000);
}

#[tokio::test]
async fn purchase_list_via_user_handler() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("pending"));
    seed(&ctx, "impresspress__products__purchases", "pur_route", pd).await;

    let (msg, input) = get_msg("/b/products/purchases", "user_1");
    let out = dispatch_user(&ctx, msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["records"].as_array().unwrap().len(), 1);
}

// ============================================================
// Rounding edge case
// ============================================================

#[tokio::test]
async fn purchase_rounding_precision() {
    let ctx = ctx().await;

    // 19.99 * 3 = 59.97 → should be 5997 cents, not 5996
    let mut product = HashMap::new();
    product.insert("name".to_string(), serde_json::json!("Precise"));
    product.insert("base_price".to_string(), serde_json::json!(19.99));
    product.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "p_round", product).await;

    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": [{"product_id": "p_round", "quantity": 3}]
        }),
    );

    let out = purchase::handle_create(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["total_cents"].as_i64().unwrap(), 5997);
}
