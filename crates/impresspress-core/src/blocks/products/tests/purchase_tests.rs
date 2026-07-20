use std::collections::HashMap;

use wafer_run::ErrorCode;

use super::harness::*;
use crate::blocks::products::purchase;

// ============================================================
// Order history and refunds
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
    seed(
        &ctx,
        super::super::repo::disputes::TABLE,
        "dp_own",
        HashMap::from([
            ("purchase_id".to_string(), serde_json::json!("pur_own")),
            (
                "provider_dispute_id".to_string(),
                serde_json::json!("dp_provider_own"),
            ),
            ("payment_intent_id".to_string(), serde_json::json!("pi_own")),
            ("status".to_string(), serde_json::json!("under_review")),
            ("amount_minor".to_string(), serde_json::json!(1000)),
            ("currency".to_string(), serde_json::json!("USD")),
        ]),
    )
    .await;

    let (msg, _input) = get_msg("/b/products/purchases/pur_own", "user_1");
    let out = purchase::handle_get(&ctx, &msg).await;
    let body = output_to_json(out).await;
    assert_eq!(body["purchase"]["id"], "pur_own");
    assert_eq!(
        body["disputes"][0]["data"]["provider_dispute_id"],
        "dp_provider_own"
    );
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
    assert_eq!(body["status"], "succeeded");
    assert_eq!(body["amount_minor"], 5000);
    assert_eq!(body["refunded_total_minor"], 5000);
    let purchase = super::super::repo::purchases::get(&ctx, "pur_refund")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "refunded");
    assert_eq!(purchase.data["refund_reason"], "Customer requested");
    assert_eq!(purchase.data["refunded_by"], "admin_1");
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
    pd.insert("total_cents".to_string(), serde_json::json!(1200));
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
    assert_eq!(body["status"], "succeeded");
    assert_eq!(body["refunded_total_minor"], 1200);
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
    pd.insert("total_cents".to_string(), serde_json::json!(1200));
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
    pd.insert("total_cents".to_string(), serde_json::json!(1200));
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
