use std::collections::HashMap;

use wafer_run::ErrorCode;

use super::harness::*;
use crate::blocks::products::{contracts::RefundStatus, purchase};

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
    let out = purchase::handle_get(&ctx, &routed(msg)).await;
    let body = output_to_json(out).await;
    assert_eq!(body["purchase"]["id"], "pur_own");
    assert_eq!(
        body["disputes"][0]["provider_dispute_id"],
        "dp_provider_own"
    );
}

/// Each tier sees only what belongs to it.
///
/// One `PurchaseView` used to serve buyer, seller and admin alike, so the
/// buyer's own order list — and `list_my_purchases`, the WebMCP tool built on
/// it — published the platform's economics (`platform_fee_cents`), the
/// seller's Stripe account, the provider's internal handles and the
/// reconciliation diagnostics. The same row handed the seller the buyer's
/// platform user id and Stripe customer id. Products got
/// `ProductView`/`CatalogProductView` for exactly this reason; orders did not.
#[tokio::test]
async fn each_order_tier_publishes_only_its_own_fields() {
    let ctx = ctx().await;
    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur_tiers",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("user_1")),
            ("buyer_user_id".to_string(), serde_json::json!("user_1")),
            (
                "buyer_email".to_string(),
                serde_json::json!("buyer@example.com"),
            ),
            (
                "seller_account_id".to_string(),
                serde_json::json!("acct_seller_1"),
            ),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_stripe_1"),
            ),
            (
                "stripe_customer_id".to_string(),
                serde_json::json!("cus_buyer_1"),
            ),
            (
                "provider_session_id".to_string(),
                serde_json::json!("cs_test_1"),
            ),
            ("platform_fee_cents".to_string(), serde_json::json!(250)),
            (
                "reconciliation_error".to_string(),
                serde_json::json!("internal diagnostic"),
            ),
            ("status".to_string(), serde_json::json!("completed")),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("reconciled"),
            ),
            ("total_cents".to_string(), serde_json::json!(5000)),
        ]),
    )
    .await;

    // Fields the platform and the provider own. A buyer has no use for any of
    // them, and `list_my_purchases` feeds whatever is here to a page agent.
    const PLATFORM_ONLY: [&str; 8] = [
        "platform_fee_cents",
        "seller_account_id",
        "stripe_account_id",
        "stripe_customer_id",
        "provider_session_id",
        "stripe_payment_intent_id",
        "provider_payment_intent_id",
        "reconciliation_error",
    ];

    let (msg, _input) = get_msg("/b/products/purchases", "user_1");
    let list = output_to_json(purchase::handle_list_user(&ctx, &msg).await).await;
    let row = list["records"]
        .as_array()
        .expect("records")
        .iter()
        .find(|r| r["id"] == "pur_tiers")
        .expect("the seeded order is the caller's own")
        .clone();
    for field in PLATFORM_ONLY {
        assert!(
            row.get(field).is_none(),
            "buyer order list must not publish {field}: {:?}",
            row.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
    }
    // What the buyer legitimately needs is still there.
    for field in ["id", "status", "currency", "total_cents", "created_at"] {
        assert!(
            row.get(field).is_some(),
            "buyer order list must still publish {field}"
        );
    }

    let (msg, _input) = get_msg("/b/products/purchases/pur_tiers", "user_1");
    let detail = output_to_json(purchase::handle_get(&ctx, &routed(msg)).await).await;
    // Pin the parent before asserting what is missing from it. `Value::Null`
    // answers `None` to every `get`, so an absence loop over a `purchase` key
    // that had stopped existing — because the response was renamed, flattened
    // or replaced by an error envelope — would pass while checking nothing.
    // The list assertions above are self-pinning (they assert presence too);
    // this one is not, so it says so explicitly.
    assert!(
        detail["purchase"].is_object(),
        "buyer order detail must carry a `purchase` object, or the loop below \
         asserts nothing: {detail}"
    );
    for field in PLATFORM_ONLY {
        assert!(
            detail["purchase"].get(field).is_none(),
            "buyer order detail must not publish {field}"
        );
    }

    // The admin tier is the one that legitimately sees the whole row.
    let (msg, _input) = get_msg("/b/products/api/admin/purchases/pur_tiers", "admin_1");
    let admin = output_to_json(purchase::handle_get_admin(&ctx, &routed(msg)).await).await;
    for field in PLATFORM_ONLY {
        assert!(
            admin["purchase"].get(field).is_some(),
            "admin order detail must still publish {field}"
        );
    }
}

/// The order list and detail endpoints publish `contracts::PurchaseView`
/// rows (and `LineItemView` / `RefundView` / `DisputeView` under the detail),
/// flat, with exactly the types' field sets. Two columns the raw echo used
/// to hand out are withheld everywhere: `receipt_token_hash`, the sha256 of
/// the guest receipt capability, together with its expiry, and on refund
/// rows `idempotency_key` and `response_json`, which the block's own
/// provider-operation projection already keeps private.
#[tokio::test]
async fn order_endpoints_publish_typed_views_and_withhold_the_receipt_digest() {
    use crate::blocks::products::contracts::{BuyerOrderDetailResponse, BuyerOrderListResponse};

    let ctx = ctx().await;
    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur_typed",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("user_1")),
            ("buyer_user_id".to_string(), serde_json::json!("user_1")),
            (
                "buyer_email".to_string(),
                serde_json::json!("buyer@example.com"),
            ),
            ("status".to_string(), serde_json::json!("completed")),
            ("total_cents".to_string(), serde_json::json!(5000)),
            ("livemode".to_string(), serde_json::json!(1)),
            (
                "subscription_cancel_at_period_end".to_string(),
                serde_json::json!(0),
            ),
            (
                "metadata".to_string(),
                serde_json::json!({"offer_id": "offer_1", "offer_version": 2}),
            ),
            (
                "receipt_token_hash".to_string(),
                serde_json::json!("deadbeef-digest"),
            ),
            (
                "receipt_token_expires_at".to_string(),
                serde_json::json!("2026-08-01T00:00:00Z"),
            ),
            (
                "payment_at".to_string(),
                serde_json::json!("2026-07-19T01:02:03Z"),
            ),
        ]),
    )
    .await;
    seed(
        &ctx,
        "impresspress__products__line_items",
        "li_typed",
        HashMap::from([
            ("purchase_id".to_string(), serde_json::json!("pur_typed")),
            ("product_id".to_string(), serde_json::json!("prod_1")),
            ("product_name".to_string(), serde_json::json!("Widget")),
            ("quantity".to_string(), serde_json::json!(2)),
            ("total_minor".to_string(), serde_json::json!(5000)),
            (
                "input_snapshot".to_string(),
                serde_json::json!({"size": "large"}),
            ),
        ]),
    )
    .await;
    seed(
        &ctx,
        super::super::repo::refunds::TABLE,
        "rf_typed",
        HashMap::from([
            ("purchase_id".to_string(), serde_json::json!("pur_typed")),
            (
                "payment_intent_id".to_string(),
                serde_json::json!("pi_typed"),
            ),
            (
                "idempotency_key".to_string(),
                serde_json::json!("impresspress_refund_pur_typed_full"),
            ),
            ("amount_minor".to_string(), serde_json::json!(1000)),
            (
                "target_refunded_total_minor".to_string(),
                serde_json::json!(1000),
            ),
            ("currency".to_string(), serde_json::json!("USD")),
            ("status".to_string(), serde_json::json!("succeeded")),
            ("note".to_string(), serde_json::json!("goodwill")),
            (
                "response_json".to_string(),
                serde_json::json!("{\"id\":\"re_secret\"}"),
            ),
        ]),
    )
    .await;
    seed(
        &ctx,
        super::super::repo::disputes::TABLE,
        "dp_typed",
        HashMap::from([
            ("purchase_id".to_string(), serde_json::json!("pur_typed")),
            (
                "provider_dispute_id".to_string(),
                serde_json::json!("dp_provider_typed"),
            ),
            (
                "payment_intent_id".to_string(),
                serde_json::json!("pi_typed"),
            ),
            ("status".to_string(), serde_json::json!("needs_response")),
            ("amount_minor".to_string(), serde_json::json!(1000)),
            ("currency".to_string(), serde_json::json!("USD")),
            ("livemode".to_string(), serde_json::json!(1)),
        ]),
    )
    .await;

    let (msg, _input) = get_msg("/b/products/purchases/pur_typed", "user_1");
    let body = output_to_json(purchase::handle_get(&ctx, &routed(msg)).await).await;
    let detail: BuyerOrderDetailResponse =
        serde_json::from_value(body.clone()).expect("BuyerOrderDetailResponse");
    assert_eq!(serde_json::to_value(&detail).unwrap(), body);
    assert_eq!(detail.purchase.id, "pur_typed");
    assert_eq!(detail.purchase.total_cents, 5000);
    // `livemode` is withheld from the buyer (see `BuyerOrderView`), so the
    // INTEGER-reads-as-a-boolean property is pinned on the boolean the buyer
    // does keep. The seller and admin views still carry `livemode`.
    assert!(!detail.purchase.subscription_cancel_at_period_end);
    assert_eq!(
        detail.purchase.metadata.get("offer_id"),
        Some(&serde_json::json!("offer_1"))
    );
    assert_eq!(
        detail.purchase.payment_at.as_deref(),
        Some("2026-07-19T01:02:03Z")
    );
    assert_eq!(detail.line_items[0].product_name, "Widget");
    assert_eq!(detail.line_items[0].quantity, 2);
    assert_eq!(
        serde_json::Value::Object(detail.line_items[0].input_snapshot.clone()),
        serde_json::json!({"size": "large"})
    );
    assert_eq!(detail.refunds[0].amount_minor, 1000);
    assert_eq!(
        detail.refunds[0].status,
        RefundStatus::Succeeded,
        "the buyer sees whether their refund landed"
    );
    assert_eq!(detail.disputes[0].provider_dispute_id, "dp_provider_typed");
    assert!(detail.disputes[0].livemode);

    let encoded = body.to_string();
    for withheld in [
        "receipt_token_hash",
        "receipt_token_expires_at",
        "deadbeef-digest",
        "idempotency_key",
        "impresspress_refund_pur_typed_full",
        "response_json",
        "re_secret",
    ] {
        assert!(
            !encoded.contains(withheld),
            "detail leaked {withheld}: {body}"
        );
    }

    let (msg, _input) = get_msg("/b/products/purchases", "user_1");
    let list = output_to_json(purchase::handle_list_user(&ctx, &msg).await).await;
    let typed: BuyerOrderListResponse =
        serde_json::from_value(list.clone()).expect("BuyerOrderListResponse");
    assert_eq!(serde_json::to_value(&typed).unwrap(), list);
    assert_eq!(typed.records[0].id, "pur_typed");
    assert_eq!(typed.page_size, 20);
    let encoded = list.to_string();
    assert!(
        !encoded.contains("receipt_token"),
        "list leaked the digest: {list}"
    );

    // The admin list is deliberately NOT the same row any more: it is the
    // whole record, where the buyer's is a projection of it. What must hold
    // is that they describe the same order and that everything the buyer sees
    // the admin also sees.
    let (mut msg, _input) = get_msg("/b/products/api/admin/purchases", "admin_1");
    msg.set_meta("auth.user_roles", "admin");
    let admin_list = output_to_json(purchase::handle_list_admin(&ctx, &msg).await).await;
    let admin_row = &admin_list["records"][0];
    let buyer_row = &list["records"][0];
    assert_eq!(admin_row["id"], buyer_row["id"]);
    for (key, value) in buyer_row.as_object().expect("buyer row is an object") {
        assert_eq!(
            admin_row.get(key),
            Some(value),
            "the admin row must agree with the buyer's on {key}"
        );
    }
    assert!(
        admin_row.get("platform_fee_cents").is_some()
            && buyer_row.get("platform_fee_cents").is_none(),
        "and must carry what the buyer's withholds"
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
    let out = purchase::handle_get(&ctx, &routed(msg)).await;
    assert!(output_is_error(out, ErrorCode::PermissionDenied).await);
}

#[tokio::test]
async fn get_purchase_not_found() {
    let ctx = ctx().await;

    let (msg, _input) = get_msg("/b/products/purchases/nonexistent", "user_1");
    let out = purchase::handle_get(&ctx, &routed(msg)).await;
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
    let out = purchase::handle_get(&ctx, &routed(msg)).await;
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
        "/b/products/api/admin/purchases/pur_refund/refund",
        "admin_1",
        serde_json::json!({"reason": "Customer requested"}),
    );
    msg.set_meta("auth.user_roles", "admin");

    let out = purchase::handle_refund(&ctx, &routed(msg), input).await;
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
async fn manual_partial_refund_retry_is_idempotent() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("completed"));
    pd.insert("total_cents".to_string(), serde_json::json!(5000));
    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur_manual_retry",
        pd,
    )
    .await;

    let request = serde_json::json!({
        "amount_minor": 2000,
        "idempotency_key": "manual_retry_1",
    });
    let (mut msg, input) = create_msg(
        "/b/products/api/admin/purchases/pur_manual_retry/refund",
        "admin_1",
        request.clone(),
    );
    msg.set_meta("auth.user_roles", "admin");
    let body = output_to_json(purchase::handle_refund(&ctx, &routed(msg), input).await).await;
    assert_eq!(body["status"], "succeeded");
    assert_eq!(body["amount_minor"], 2000);
    assert_eq!(body["refunded_total_minor"], 2000);
    // `manual` is the ephemeral result's word for "no provider was asked";
    // the ledger row itself records the refund as `succeeded` on both state
    // columns, which is what `RefundView.provider_status` documents.
    assert_eq!(body["provider_status"], "manual");
    let (msg, _input) = admin_get_msg("/b/products/api/admin/purchases/pur_manual_retry");
    let detail = output_to_json(purchase::handle_get(&ctx, &routed(msg)).await).await;
    assert_eq!(detail["refunds"].as_array().map(Vec::len), Some(1));
    assert_eq!(detail["refunds"][0]["status"], "succeeded");
    assert_eq!(
        detail["refunds"][0]["provider_status"], "succeeded",
        "a refund recorded without a provider is `succeeded`, never `manual`: {detail}"
    );

    // A retried delivery of the same request (same idempotency key, e.g.
    // after a timeout) must return the recorded outcome, not deduct again.
    let (mut msg, input) = create_msg(
        "/b/products/api/admin/purchases/pur_manual_retry/refund",
        "admin_1",
        request,
    );
    msg.set_meta("auth.user_roles", "admin");
    let body = output_to_json(purchase::handle_refund(&ctx, &routed(msg), input).await).await;
    assert_eq!(body["status"], "succeeded");
    assert_eq!(body["amount_minor"], 2000);
    assert_eq!(body["refunded_total_minor"], 2000);

    let purchase = super::super::repo::purchases::get(&ctx, "pur_manual_retry")
        .await
        .unwrap();
    assert_eq!(
        purchase.data["refunded_total_cents"],
        serde_json::json!(2000)
    );
    assert_eq!(
        purchase.data["status"],
        serde_json::json!("partially_refunded")
    );
}

#[tokio::test]
async fn manual_refund_key_reuse_with_different_amount_fails() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("completed"));
    pd.insert("total_cents".to_string(), serde_json::json!(5000));
    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur_manual_reuse",
        pd,
    )
    .await;

    let (mut msg, input) = create_msg(
        "/b/products/api/admin/purchases/pur_manual_reuse/refund",
        "admin_1",
        serde_json::json!({"amount_minor": 2000, "idempotency_key": "manual_reuse_1"}),
    );
    msg.set_meta("auth.user_roles", "admin");
    let body = output_to_json(purchase::handle_refund(&ctx, &routed(msg), input).await).await;
    assert_eq!(body["status"], "succeeded");

    // The same key with a different amount is a client bug, not a retry.
    let (mut msg, input) = create_msg(
        "/b/products/api/admin/purchases/pur_manual_reuse/refund",
        "admin_1",
        serde_json::json!({"amount_minor": 1000, "idempotency_key": "manual_reuse_1"}),
    );
    msg.set_meta("auth.user_roles", "admin");
    let out = purchase::handle_refund(&ctx, &routed(msg), input).await;
    assert!(output_is_error(out, ErrorCode::InvalidArgument).await);
}

#[tokio::test]
async fn refund_non_completed_fails() {
    let ctx = ctx().await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("pending"));
    seed(&ctx, "impresspress__products__purchases", "pur_pending", pd).await;

    let (mut msg, input) = create_msg(
        "/b/products/api/admin/purchases/pur_pending/refund",
        "admin_1",
        serde_json::json!({}),
    );
    msg.set_meta("auth.user_roles", "admin");

    let out = purchase::handle_refund(&ctx, &routed(msg), input).await;
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
        "/b/products/api/admin/purchases/pur_already/refund",
        "admin_1",
        serde_json::json!({}),
    );
    msg.set_meta("auth.user_roles", "admin");

    let out = purchase::handle_refund(&ctx, &routed(msg), input).await;
    assert!(output_is_error(out, ErrorCode::InvalidArgument).await);
}

#[tokio::test]
async fn refund_purchase_not_found() {
    let ctx = ctx().await;

    let (mut msg, input) = create_msg(
        "/b/products/api/admin/purchases/nonexistent/refund",
        "admin_1",
        serde_json::json!({}),
    );
    msg.set_meta("auth.user_roles", "admin");

    let out = purchase::handle_refund(&ctx, &routed(msg), input).await;
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
        "/b/products/api/admin/purchases/pur_noreason/refund",
        "admin_1",
        serde_json::json!({}),
    );
    msg.set_meta("auth.user_roles", "admin");

    let out = purchase::handle_refund(&ctx, &routed(msg), input).await;
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
        "/b/products/api/admin/purchases/pur_malformed/refund",
    );
    msg.set_meta("auth.user_id", "admin_1");
    msg.set_meta("auth.user_roles", "admin");
    let input = wafer_run::InputStream::from_bytes(b"{not valid json".to_vec());

    let out = purchase::handle_refund(&ctx, &routed(msg), input).await;
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
        "/b/products/api/admin/purchases/pur_outage/refund",
        "admin_1",
        serde_json::json!({"reason": "Customer requested"}),
    );
    msg.set_meta("auth.user_roles", "admin");

    let out = purchase::handle_refund(&ctx, &routed(msg), input).await;
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
    let out = dispatch(&ctx, msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["records"].as_array().unwrap().len(), 1);
}

/// A stored order state outside the contract is a data-integrity error, and
/// it is never published as a `200` carrying a value the schema does not
/// define, nor silently defaulted.
///
/// The two surfaces answer differently, on purpose. On a single-order GET the
/// row *is* the response, so it fails loudly. On a list the row is one of
/// many, so it is omitted and logged: failing the page meant one legacy,
/// imported or hand-edited order denied the buyer every order they had.
#[tokio::test]
async fn order_rows_outside_the_state_contract_never_reach_the_wire() {
    let ctx = ctx().await;
    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur_bad_reconciliation",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("user_1")),
            ("status".to_string(), serde_json::json!("completed")),
            ("total_cents".to_string(), serde_json::json!(1000)),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("half_done"),
            ),
        ]),
    )
    .await;

    let (msg, _input) = get_msg("/b/products/purchases/pur_bad_reconciliation", "user_1");
    assert!(
        output_is_error(
            purchase::handle_get(&ctx, &routed(msg)).await,
            ErrorCode::Internal
        )
        .await,
        "a 200 would publish `half_done`, which the contract does not define"
    );
    // A conforming order for the same buyer: without one, "the list is not
    // empty" would pass vacuously and prove nothing about the skip.
    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur_good",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("user_1")),
            ("status".to_string(), serde_json::json!("completed")),
            ("total_cents".to_string(), serde_json::json!(2500)),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("reconciled"),
            ),
        ]),
    )
    .await;

    // The list omits the offending row and still serves the caller's other
    // orders.
    let (msg, _input) = get_msg("/b/products/purchases", "user_1");
    let body = output_to_json(purchase::handle_list_user(&ctx, &msg).await).await;
    let ids: Vec<&str> = body["records"]
        .as_array()
        .expect("records array")
        .iter()
        .map(|r| r["id"].as_str().expect("row id"))
        .collect();
    assert!(
        !ids.contains(&"pur_bad_reconciliation"),
        "the list must not publish a row the contract cannot describe: {ids:?}"
    );
    assert!(
        ids.contains(&"pur_good"),
        "one unprojectable row must not cost the caller their other orders: {ids:?}"
    );

    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur_bad_status",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("user_2")),
            ("status".to_string(), serde_json::json!("shipped")),
            ("total_cents".to_string(), serde_json::json!(1000)),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("reconciled"),
            ),
        ]),
    )
    .await;
    let (msg, _input) = get_msg("/b/products/purchases/pur_bad_status", "user_2");
    assert!(
        output_is_error(
            purchase::handle_get(&ctx, &routed(msg)).await,
            ErrorCode::Internal
        )
        .await,
        "a 200 would publish `shipped`, which is not an order state"
    );
}

/// One order, one identity, one amount.
///
/// `PurchaseView` — the admin projection, the only one of the three that
/// published either — carried `user_id` beside `buyer_user_id` and
/// `amount_cents` beside `total_cents`. Each pair is two columns holding one
/// value, written together by one writer and never compared, so a row where
/// they drift ships two answers to one question. The duplicate half of each
/// pair is also the half nothing else reads: `amount_cents` had exactly one
/// reader in the tree — this projection — on a field whose own published
/// description said to prefer `total_cents`, and `user_id`'s description
/// claimed it is empty for guest orders while the writer mirrors `""` into
/// both. Ownership is decided on `buyer_user_id`; that is what is published.
///
/// The columns are untouched: this is the publication half, and it needs no
/// migration because every order view is hand-projected.
#[tokio::test]
async fn an_order_publishes_one_identity_and_one_amount() {
    let ctx = ctx().await;
    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur_drift",
        HashMap::from([
            // The two pairs, deliberately disagreeing: no writer produces
            // this, which is the point — nothing in the tree would notice.
            ("user_id".to_string(), serde_json::json!("user_legacy")),
            ("buyer_user_id".to_string(), serde_json::json!("user_1")),
            ("amount_cents".to_string(), serde_json::json!(111)),
            ("total_cents".to_string(), serde_json::json!(5000)),
            ("status".to_string(), serde_json::json!("completed")),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("reconciled"),
            ),
        ]),
    )
    .await;

    let (msg, _input) = get_msg("/b/products/api/admin/purchases/pur_drift", "admin_1");
    let admin = output_to_json(purchase::handle_get_admin(&ctx, &routed(msg)).await).await;
    let purchase = &admin["purchase"];
    assert!(
        purchase.is_object(),
        "the admin detail must carry a `purchase` object, or the absence \
         assertions below assert nothing: {admin}"
    );

    assert_eq!(purchase["buyer_user_id"], "user_1");
    assert_eq!(purchase["total_cents"], 5000);
    assert!(
        purchase.get("user_id").is_none(),
        "an order publishes one buyer identity, and it is `buyer_user_id`: {purchase}"
    );
    assert!(
        purchase.get("amount_cents").is_none(),
        "an order publishes one amount, and it is `total_cents`: {purchase}"
    );
}
