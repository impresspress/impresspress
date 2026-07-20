use std::collections::HashMap;

use wafer_core::clients::database as db;

use super::harness::*;
use crate::blocks::products::repo;

/// `cancel_and_reset_addons` flips status to cancelled and zeroes every addon
/// column for the matched subscription.
#[tokio::test]
async fn cancel_and_reset_addons_zeroes_addons_and_cancels() {
    let ctx = ctx().await;
    let mut sd = HashMap::new();
    sd.insert("user_id".to_string(), serde_json::json!("user_1"));
    sd.insert(
        "stripe_subscription_id".to_string(),
        serde_json::json!("sub_stripe_1"),
    );
    sd.insert("status".to_string(), serde_json::json!("active"));
    sd.insert("addon_projects".to_string(), serde_json::json!(5));
    sd.insert("addon_requests".to_string(), serde_json::json!(1000));
    sd.insert("addon_r2_bytes".to_string(), serde_json::json!(42));
    sd.insert("addon_d1_bytes".to_string(), serde_json::json!(7));
    seed(
        &ctx,
        "impresspress__products__subscriptions",
        "sub_db_1",
        sd,
    )
    .await;

    let rows = repo::subscriptions::cancel_and_reset_addons(&ctx, "sub_stripe_1", 1)
        .await
        .expect("cancel ok");
    assert_eq!(rows, 1, "exactly one subscription row updated");

    let rec = db::get(&ctx, "impresspress__products__subscriptions", "sub_db_1")
        .await
        .expect("row exists");
    assert_eq!(
        rec.data.get("status").and_then(|v| v.as_str()),
        Some("cancelled")
    );
    assert_eq!(
        rec.data.get("addon_projects").and_then(|v| v.as_i64()),
        Some(0)
    );
    assert_eq!(
        rec.data.get("addon_requests").and_then(|v| v.as_i64()),
        Some(0)
    );
    assert_eq!(
        rec.data.get("addon_r2_bytes").and_then(|v| v.as_i64()),
        Some(0)
    );
    assert_eq!(
        rec.data.get("addon_d1_bytes").and_then(|v| v.as_i64()),
        Some(0)
    );
}

/// `complete_atomic` transitions a pending purchase to completed and records
/// the payment intent; a second call is a 0-row no-op (idempotent).
#[tokio::test]
async fn complete_atomic_only_from_pending_or_checkout_started() {
    let ctx = ctx().await;
    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("pending"));
    seed(&ctx, "impresspress__products__purchases", "pur_1", pd).await;

    let rows = repo::purchases::complete_atomic(&ctx, "pur_1", "pi_abc")
        .await
        .expect("complete ok");
    assert_eq!(rows, 1);
    let rec = db::get(&ctx, "impresspress__products__purchases", "pur_1")
        .await
        .unwrap();
    assert_eq!(
        rec.data.get("status").and_then(|v| v.as_str()),
        Some("completed")
    );
    assert_eq!(
        rec.data
            .get("provider_payment_intent_id")
            .and_then(|v| v.as_str()),
        Some("pi_abc")
    );

    // Second call: already completed -> 0 rows, no change.
    let rows2 = repo::purchases::complete_atomic(&ctx, "pur_1", "pi_zzz")
        .await
        .expect("idempotent ok");
    assert_eq!(rows2, 0, "completed purchase is not re-completed");
    let rec2 = db::get(&ctx, "impresspress__products__purchases", "pur_1")
        .await
        .unwrap();
    assert_eq!(
        rec2.data
            .get("provider_payment_intent_id")
            .and_then(|v| v.as_str()),
        Some("pi_abc"),
        "payment intent not overwritten by the no-op call"
    );
}

/// `refund_atomic` only transitions a completed purchase; a pending one is a
/// 0-row no-op (prevents double-refund / refunding incomplete orders).
#[tokio::test]
async fn refund_atomic_only_from_completed() {
    let ctx = ctx().await;
    let mut completed = HashMap::new();
    completed.insert("user_id".to_string(), serde_json::json!("user_1"));
    completed.insert("status".to_string(), serde_json::json!("completed"));
    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur_done",
        completed,
    )
    .await;
    let mut pending = HashMap::new();
    pending.insert("user_id".to_string(), serde_json::json!("user_1"));
    pending.insert("status".to_string(), serde_json::json!("pending"));
    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur_pending",
        pending,
    )
    .await;

    let ok = repo::purchases::refund_atomic(&ctx, "pur_done", "admin_1", "duplicate")
        .await
        .expect("refund ok");
    assert_eq!(ok, 1);
    let rec = db::get(&ctx, "impresspress__products__purchases", "pur_done")
        .await
        .unwrap();
    assert_eq!(
        rec.data.get("status").and_then(|v| v.as_str()),
        Some("refunded")
    );
    assert_eq!(
        rec.data.get("refunded_by").and_then(|v| v.as_str()),
        Some("admin_1")
    );

    let noop = repo::purchases::refund_atomic(&ctx, "pur_pending", "admin_1", "x")
        .await
        .expect("noop ok");
    assert_eq!(noop, 0, "pending purchase cannot be refunded");
}

/// `subscription_for_user` (refactored to `db::get_by_field` + a curated
/// Rust-side projection) must not leak `user_id`/`stripe_customer_id` into
/// the response, and must coalesce the 4 addon columns to 0 when
/// NULL/absent. Regression test for the SP-B2b consumer migration.
#[tokio::test]
async fn subscription_for_user_projects_curated_columns_without_leaking_ids() {
    let ctx = ctx().await;
    let mut sd = HashMap::new();
    sd.insert("user_id".to_string(), serde_json::json!("user_1"));
    sd.insert(
        "stripe_customer_id".to_string(),
        serde_json::json!("cus_stripe_1"),
    );
    sd.insert(
        "stripe_subscription_id".to_string(),
        serde_json::json!("sub_stripe_1"),
    );
    sd.insert("plan".to_string(), serde_json::json!("pro"));
    sd.insert("status".to_string(), serde_json::json!("active"));
    // addon_* columns intentionally omitted (absent) so the schema's
    // NOT NULL DEFAULT 0 / the fn's own coalesce is what fills them in —
    // exercising the same NULL/absent-addon path `subscription_for_user`
    // guards against.
    seed(
        &ctx,
        "impresspress__products__subscriptions",
        "sub_user_1",
        sd,
    )
    .await;

    let out = repo::subscriptions::subscription_for_user(&ctx, "user_1")
        .await
        .expect("no repository error")
        .expect("subscription exists");
    let map = out
        .as_object()
        .expect("subscription_for_user returns a JSON object");

    for col in [
        "id",
        "plan",
        "status",
        "stripe_subscription_id",
        "grace_period_end",
        "created_at",
        "updated_at",
    ] {
        assert!(
            map.contains_key(col),
            "curated column {col} missing from response"
        );
    }

    for col in [
        "addon_projects",
        "addon_requests",
        "addon_r2_bytes",
        "addon_d1_bytes",
    ] {
        assert_eq!(
            map.get(col).and_then(|v| v.as_i64()),
            Some(0),
            "{col} not coalesced to 0"
        );
    }

    assert!(
        !map.contains_key("user_id"),
        "user_id leaked into subscription_for_user response"
    );
    assert!(
        !map.contains_key("stripe_customer_id"),
        "stripe_customer_id leaked into subscription_for_user response"
    );
}

/// The legitimate "no subscription row" case must still map to `Ok(None)` —
/// only genuine repository errors should surface as `Err`.
#[tokio::test]
async fn subscription_for_user_returns_ok_none_when_no_row() {
    let ctx = ctx().await;
    let result = repo::subscriptions::subscription_for_user(&ctx, "no_such_user").await;
    assert!(
        matches!(result, Ok(None)),
        "no subscription row must be Ok(None), got {result:?}"
    );
}

/// CODE_REVIEW_2026-07-16 "Error semantics fabricate successful defaults":
/// a genuine repository failure must surface as `Err`, not be folded into
/// the same `None` used for "user has no subscription" — the two were
/// previously indistinguishable to the caller (`handle_subscription`
/// reported `{"subscription": null}` for both).
#[tokio::test]
async fn subscription_for_user_repository_failure_surfaces_as_error() {
    let ctx = ctx().await.break_reads();
    let result = repo::subscriptions::subscription_for_user(&ctx, "user_1").await;
    assert!(
        result.is_err(),
        "a genuine repository failure must surface as Err, not a fabricated None"
    );
}

/// Offer state transitions are compare-and-swap writes: a write conditioned
/// on a status the row no longer holds must not land. This is the guard that
/// keeps a stale draft edit from wiping `stripe_price_id` on an offer that a
/// concurrent request published between the read and the write.
#[tokio::test]
async fn stale_offer_write_cannot_land_after_status_transition() {
    let ctx = ctx().await;

    let mut od = HashMap::new();
    od.insert("product_id".to_string(), serde_json::json!("prod_cas"));
    od.insert("name".to_string(), serde_json::json!("Live offer"));
    od.insert("status".to_string(), serde_json::json!("active"));
    od.insert(
        "stripe_price_id".to_string(),
        serde_json::json!("price_live"),
    );
    od.insert(
        "created_at".to_string(),
        serde_json::json!("2026-01-01T00:00:00Z"),
    );
    od.insert(
        "updated_at".to_string(),
        serde_json::json!("2026-01-01T00:00:00Z"),
    );
    seed(&ctx, "impresspress__products__offers", "offer_cas", od).await;

    let landed = repo::offers::update_if_status(
        &ctx,
        "offer_cas",
        "draft",
        HashMap::from([("stripe_price_id".to_string(), serde_json::json!(""))]),
    )
    .await
    .unwrap();
    assert!(!landed, "stale write must be rejected");
    let record = db::get(&ctx, "impresspress__products__offers", "offer_cas")
        .await
        .unwrap();
    assert_eq!(record.data["stripe_price_id"], "price_live");
    assert_eq!(record.data["status"], "active");

    // The same write lands when the row still holds the expected status.
    let mut dd = HashMap::new();
    dd.insert("product_id".to_string(), serde_json::json!("prod_cas"));
    dd.insert("name".to_string(), serde_json::json!("Draft offer"));
    dd.insert("status".to_string(), serde_json::json!("draft"));
    dd.insert(
        "stripe_price_id".to_string(),
        serde_json::json!("price_stale"),
    );
    dd.insert(
        "created_at".to_string(),
        serde_json::json!("2026-01-01T00:00:00Z"),
    );
    dd.insert(
        "updated_at".to_string(),
        serde_json::json!("2026-01-01T00:00:00Z"),
    );
    seed(
        &ctx,
        "impresspress__products__offers",
        "offer_cas_draft",
        dd,
    )
    .await;
    let landed = repo::offers::update_if_status(
        &ctx,
        "offer_cas_draft",
        "draft",
        HashMap::from([("stripe_price_id".to_string(), serde_json::json!(""))]),
    )
    .await
    .unwrap();
    assert!(landed);
    let record = db::get(&ctx, "impresspress__products__offers", "offer_cas_draft")
        .await
        .unwrap();
    assert_eq!(record.data["stripe_price_id"], "");
}
