use std::collections::HashMap;

use wafer_core::clients::database as db;

use super::harness::*;
use crate::blocks::products::{
    contracts::{OfferStatus, SubscriptionStatus},
    repo,
};

/// `cancel_and_reset_addons` flips status to `canceled` and zeroes every addon
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
        Some("canceled")
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

/// Same-second deliveries may only move toward a more-terminal status, and
/// nothing — not even a strictly newer event — moves a terminal row back to a
/// live or past-due status. Immediate cancellation emits `updated` (active)
/// and `deleted` with the same `created` second, and a leftover open invoice
/// can still fail after deletion; neither may resurrect the subscription.
#[tokio::test]
async fn update_status_plan_and_mark_past_due_respect_terminal_status_ranking() {
    let ctx = ctx().await;
    let mut sd = HashMap::new();
    sd.insert("user_id".to_string(), serde_json::json!("user_rank"));
    sd.insert(
        "stripe_subscription_id".to_string(),
        serde_json::json!("sub_stripe_rank"),
    );
    sd.insert("plan".to_string(), serde_json::json!("pro"));
    sd.insert("status".to_string(), serde_json::json!("active"));
    sd.insert("stripe_event_created".to_string(), serde_json::json!(100));
    seed(
        &ctx,
        "impresspress__products__subscriptions",
        "sub_db_rank",
        sd,
    )
    .await;

    // The cancellation lands first...
    let rows = repo::subscriptions::update_status_plan(
        &ctx,
        "sub_stripe_rank",
        SubscriptionStatus::Canceled,
        None,
        200,
    )
    .await
    .expect("cancel ok");
    assert_eq!(rows, 1);

    // ...and the same-second "active" snapshot must not resurrect it.
    let rows = repo::subscriptions::update_status_plan(
        &ctx,
        "sub_stripe_rank",
        SubscriptionStatus::Active,
        None,
        200,
    )
    .await
    .expect("refused write ok");
    assert_eq!(rows, 0);

    // A strictly newer non-terminal update cannot leave the terminal state
    // either: a canceled Stripe subscription id never becomes live again.
    let rows = repo::subscriptions::update_status_plan(
        &ctx,
        "sub_stripe_rank",
        SubscriptionStatus::Active,
        None,
        300,
    )
    .await
    .expect("refused write ok");
    assert_eq!(rows, 0);

    // Neither can a failed payment on a leftover open invoice.
    let rows = repo::subscriptions::mark_past_due(&ctx, "sub_stripe_rank", 400)
        .await
        .expect("refused write ok");
    assert_eq!(rows, 0);

    let rec = db::get(&ctx, "impresspress__products__subscriptions", "sub_db_rank")
        .await
        .expect("row exists");
    assert_eq!(
        rec.data.get("status").and_then(|v| v.as_str()),
        Some("canceled")
    );
    assert_eq!(
        rec.data
            .get("stripe_event_created")
            .and_then(|v| v.as_i64()),
        Some(200)
    );
    assert!(rec
        .data
        .get("grace_period_end")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .is_empty());

    // A live subscription still becomes past-due with a grace window.
    let mut sd = HashMap::new();
    sd.insert("user_id".to_string(), serde_json::json!("user_live"));
    sd.insert(
        "stripe_subscription_id".to_string(),
        serde_json::json!("sub_stripe_live"),
    );
    sd.insert("plan".to_string(), serde_json::json!("pro"));
    sd.insert("status".to_string(), serde_json::json!("active"));
    sd.insert("stripe_event_created".to_string(), serde_json::json!(100));
    seed(
        &ctx,
        "impresspress__products__subscriptions",
        "sub_db_live",
        sd,
    )
    .await;
    let rows = repo::subscriptions::mark_past_due(&ctx, "sub_stripe_live", 150)
        .await
        .expect("past-due ok");
    assert_eq!(rows, 1);
    let rec = db::get(&ctx, "impresspress__products__subscriptions", "sub_db_live")
        .await
        .expect("row exists");
    assert_eq!(
        rec.data.get("status").and_then(|v| v.as_str()),
        Some("past_due")
    );
    assert!(!rec
        .data
        .get("grace_period_end")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .is_empty());
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

/// A refund is reconciled only onto a paid order. A pending one is refused
/// as not refundable, before any write, and is left exactly as it was; a
/// completed one takes the provider's total and becomes `refunded`.
///
/// The refusal's message is asserted, not just its code: the write below the
/// check also filters on status, so without the check the pending row would
/// still be left alone — but refused as a concurrent change, which tells the
/// operator to retry an order no retry can ever refund.
#[tokio::test]
async fn reconcile_refund_total_refuses_an_unpaid_order_and_leaves_it_untouched() {
    use crate::util::RecordExt as _;

    let ctx = ctx().await;
    for (id, status) in [("pur_done", "completed"), ("pur_pending", "pending")] {
        seed(
            &ctx,
            "impresspress__products__purchases",
            id,
            HashMap::from([
                ("user_id".to_string(), serde_json::json!("user_1")),
                ("status".to_string(), serde_json::json!(status)),
                ("total_cents".to_string(), serde_json::json!(1000)),
                ("refunded_total_cents".to_string(), serde_json::json!(0)),
            ]),
        )
        .await;
    }
    let pending_before = db::get(&ctx, "impresspress__products__purchases", "pur_pending")
        .await
        .unwrap()
        .data;

    let refused =
        repo::purchases::reconcile_refund_total(&ctx, "pur_pending", 1000, "admin_1", "x")
            .await
            .expect_err("a pending order cannot be refunded");
    assert_eq!(refused.code, wafer_run::ErrorCode::FailedPrecondition);
    assert!(
        refused.message.contains("not in a refundable state"),
        "refused for the wrong reason: {}",
        refused.message
    );
    assert_eq!(
        db::get(&ctx, "impresspress__products__purchases", "pur_pending")
            .await
            .unwrap()
            .data,
        pending_before,
        "the refused order is unchanged"
    );

    let refunded =
        repo::purchases::reconcile_refund_total(&ctx, "pur_done", 1000, "admin_1", "duplicate")
            .await
            .expect("a completed order is refunded");
    assert_eq!(refunded.str_field("status"), "refunded");
    assert_eq!(refunded.i64_field("refunded_total_cents"), 1000);
    assert_eq!(refunded.str_field("refunded_by"), "admin_1");
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
    // The repo returns the typed `SubscriptionView`; the assertions below are
    // about what it puts on the wire, so check its serialized form.
    let value = serde_json::to_value(&out).expect("SubscriptionView serializes");
    let map = value
        .as_object()
        .expect("subscription_for_user serializes as a JSON object");

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
        OfferStatus::Draft,
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
        OfferStatus::Draft,
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

// ---------------------------------------------------------------------------
// The doors that replaced `pages.rs`'s hand-rolled reads
// ---------------------------------------------------------------------------
//
// `handlers/sellers.rs` and `pages.rs` each ran their own `db::list_all` +
// `to_contract` and their own `db::get` + `to_contract` against the seller
// accounts table, and their own `owner_id` filter against the products table.
// These tests pin that the shared functions answer what the duplicated reads
// answered, so a future divergence between the JSON API and the SSR page is a
// test failure rather than a support ticket.

async fn seed_seller_account(
    ctx: &crate::test_support::TestContext,
    id: &str,
    user_id: &str,
    status: &str,
) {
    seed(
        ctx,
        repo::seller_accounts::TABLE,
        id,
        HashMap::from([
            ("user_id".to_string(), serde_json::json!(user_id)),
            ("status".to_string(), serde_json::json!(status)),
            (
                "stripe_account_id".to_string(),
                serde_json::json!(format!("acct_{id}")),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(true)),
            ("payouts_enabled".to_string(), serde_json::json!(true)),
            ("requirements_json".to_string(), serde_json::json!("{}")),
        ]),
    )
    .await;
}

/// `list_rows` is exactly the read both call sites hand-rolled: every row of
/// the table, each decoded.
#[tokio::test]
async fn list_rows_equals_every_row_decoded() {
    let ctx = ctx().await;
    seed_seller_account(&ctx, "seller_a", "user_a", "active").await;
    seed_seller_account(&ctx, "seller_b", "user_b", "restricted").await;

    let expected: Vec<_> = db::list_all(&ctx, repo::seller_accounts::TABLE, vec![])
        .await
        .expect("rows")
        .iter()
        .map(repo::seller_accounts::SellerRow::from_record)
        .collect::<Result<Vec<_>, _>>()
        .expect("projections");

    let actual = repo::seller_accounts::list_rows(&ctx)
        .await
        .expect("list_rows");
    assert_eq!(actual.rows, expected);
    assert_eq!(actual.rows.len(), 2, "both seeded rows are listed");
    assert!(!actual.truncated, "two rows is not a prefix");
}

/// A row that cannot be projected fails the whole read. A seller list quietly
/// missing the one account that would not decode is a governance surface that
/// hides an account, which is worse than an error.
#[tokio::test]
async fn list_rows_fails_when_a_row_cannot_be_decoded() {
    let ctx = ctx().await;
    seed_seller_account(&ctx, "seller_ok", "user_ok", "active").await;
    // Not a `SellerStatus` spelling, so the row cannot be decoded.
    seed_seller_account(&ctx, "seller_bad", "user_bad", "dormant").await;

    let error = repo::seller_accounts::list_rows(&ctx)
        .await
        .expect_err("an unprojectable row fails the read");
    assert_eq!(error.code, wafer_run::ErrorCode::Internal);
}

/// `get_row` decodes the row and reports a missing id as `Ok(None)`, so a
/// caller answers 404 without matching on an error code. The contract it
/// becomes carries the fee it is handed, not the row's unread column.
#[tokio::test]
async fn get_row_decodes_the_row_and_answers_none_for_a_missing_id() {
    let ctx = ctx().await;
    seed_seller_account(&ctx, "seller_a", "user_a", "active").await;

    let record = db::get(&ctx, repo::seller_accounts::TABLE, "seller_a")
        .await
        .expect("row");
    let expected = repo::seller_accounts::SellerRow::from_record(&record).expect("projection");

    let row = repo::seller_accounts::get_row(&ctx, "seller_a")
        .await
        .expect("get_row");
    assert_eq!(row, Some(expected));
    assert_eq!(row.unwrap().into_contract(250).fee_basis_points, 250);
    assert_eq!(
        repo::seller_accounts::get_row(&ctx, "seller_missing")
            .await
            .expect("get_row"),
        None
    );
}

async fn seed_owned_product(
    ctx: &crate::test_support::TestContext,
    id: &str,
    owner_id: &str,
    deleted: bool,
) {
    let mut data = HashMap::from([
        ("name".to_string(), serde_json::json!("Listing")),
        ("slug".to_string(), serde_json::json!(id)),
        ("status".to_string(), serde_json::json!("active")),
        ("approval_status".to_string(), serde_json::json!("approved")),
        ("owner_kind".to_string(), serde_json::json!("user")),
        ("owner_id".to_string(), serde_json::json!(owner_id)),
        ("created_by".to_string(), serde_json::json!(owner_id)),
    ]);
    if deleted {
        data.insert(
            "deleted_at".to_string(),
            serde_json::json!("2026-09-06T00:00:00Z"),
        );
    }
    seed(ctx, repo::products::TABLE, id, data).await;
}

/// A seller's catalog is their LIVE products only, and only theirs; the
/// suspension read is the same owner filter over both sets. The two are one
/// word apart, which is why they are pinned together.
#[tokio::test]
async fn list_owned_by_is_the_owners_live_products_only() {
    let ctx = ctx().await;
    seed_owned_product(&ctx, "p_live", "user_a", false).await;
    seed_owned_product(&ctx, "p_deleted", "user_a", true).await;
    seed_owned_product(&ctx, "p_other", "user_b", false).await;

    let live: Vec<String> = repo::products::list_owned_by(&ctx, "user_a")
        .await
        .expect("live")
        .rows
        .into_iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(live, vec!["p_live".to_string()]);

    let mut every: Vec<String> = repo::products::list_owned_by_including_deleted(&ctx, "user_a")
        .await
        .expect("every")
        .into_iter()
        .map(|record| record.id)
        .collect();
    every.sort();
    assert_eq!(every, vec!["p_deleted".to_string(), "p_live".to_string()]);
}

async fn seed_group(ctx: &crate::test_support::TestContext, id: &str, name: &str, user_id: &str) {
    seed(
        ctx,
        repo::groups::TABLE,
        id,
        HashMap::from([
            ("name".to_string(), serde_json::json!(name)),
            ("user_id".to_string(), serde_json::json!(user_id)),
        ]),
    )
    .await;
}

/// `groups::count` counts and `groups::list_by_name` sorts by name and honors
/// the caller's filters — the four reads (admin overview, admin stats, admin
/// groups page, a user's own groups) that used to be four separate queries
/// spread over three files.
#[tokio::test]
async fn groups_count_and_list_by_name_replace_the_hand_rolled_reads() {
    use crate::util::RecordExt;

    let ctx = ctx().await;
    seed_group(&ctx, "grp_z", "Zebra", "user_a").await;
    seed_group(&ctx, "grp_a", "Alpaca", "user_a").await;
    seed_group(&ctx, "grp_m", "Manatee", "user_b").await;

    assert_eq!(repo::groups::count(&ctx, &[]).await.expect("count"), 3);

    let all: Vec<String> = repo::groups::list_by_name(&ctx, vec![], 100)
        .await
        .expect("list")
        .records
        .into_iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(
        all,
        vec![
            "grp_a".to_string(),
            "grp_m".to_string(),
            "grp_z".to_string()
        ],
        "name-ascending"
    );

    let owned = vec![wafer_block::db::Filter {
        field: "user_id".to_string(),
        operator: wafer_block::db::FilterOp::Equal,
        value: serde_json::json!("user_a"),
    }];
    let mine: Vec<String> = repo::groups::list_by_name(&ctx, owned, 1000)
        .await
        .expect("list")
        .records
        .into_iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(mine, vec!["grp_a".to_string(), "grp_z".to_string()]);

    assert_eq!(
        repo::groups::get(&ctx, "grp_a")
            .await
            .expect("group")
            .str_field("name"),
        "Alpaca"
    );
}

/// A failing groups count is an error, not a zero. Both surfaces that count
/// groups render a headline number, and a fabricated `0` reads as real
/// business data during an outage — the admin overview page additionally
/// trips its "Add your first product" empty state on it. The count moved
/// behind `repo::groups::count`, which returns `Result`; these two are the
/// call sites that have to keep propagating it.
#[tokio::test]
async fn a_failing_groups_count_fails_the_stats_endpoint_and_the_overview_page() {
    use wafer_run::ErrorCode;

    use crate::test_support::FailingDbOpContext;

    let base = ctx().await;
    seed_group(&base, "grp_a", "Alpaca", "").await;

    // Both requests answer on a healthy context, so the assertions below
    // cannot pass because the path stopped routing.
    let (msg, input) = admin_get_msg("/b/products/api/admin/stats");
    assert_eq!(
        output_to_json(dispatch(&base, msg, input).await).await["total_groups"],
        serde_json::json!(1)
    );
    let (msg, input) = admin_get_msg("/b/products/admin");
    assert!(output_to_html(dispatch(&base, msg, input).await)
        .await
        .contains("Groups"));

    let failing = FailingDbOpContext::new(base, vec![("database.count", repo::groups::TABLE)]);

    assert_eq!(
        repo::groups::count(&failing, &[])
            .await
            .expect_err("the outage surfaces")
            .code,
        wafer_run::ErrorCode::Internal
    );

    let (msg, input) = admin_get_msg("/b/products/api/admin/stats");
    assert!(
        output_is_error(dispatch(&failing, msg, input).await, ErrorCode::Internal).await,
        "the stats endpoint must report the outage, not answer total_groups = 0"
    );

    let (msg, input) = admin_get_msg("/b/products/admin");
    assert!(
        output_is_error(dispatch(&failing, msg, input).await, ErrorCode::Internal).await,
        "the admin overview page must report the outage, not render 0 groups"
    );
}

/// The five lookups whose caller could not tell "no" from "the database is
/// down".
///
/// Each answered a plain `bool` or `Option` and collapsed a failed read into
/// the negative answer, and a caller then acted on it: a blip in
/// `find_user_by_stripe_sub` skipped the addon-total sync and the outbound
/// `products.subscription.updated` while the webhook still reported success,
/// a blip in either `default_id` wrote a group or a product with no template
/// instead of the seeded default, and a blip in the two ownership reads told
/// a buyer they did not own the product a purchase required.
///
/// The positive controls share this test on purpose: the point is not that a
/// broken read errors, it is that a legitimately absent row still answers no.
#[tokio::test]
async fn the_ownership_and_template_lookups_separate_no_from_a_failed_read() {
    let healthy = ctx().await;

    assert!(
        !repo::subscriptions::active_plan_exists(&healthy, "user_1", "plan_x")
            .await
            .expect("a healthy read answers"),
        "no subscription row is a legitimate no"
    );
    assert!(
        !repo::purchases::line_item_exists_for_product(
            &healthy,
            vec![serde_json::json!("purchase_1")],
            "prod_x"
        )
        .await
        .expect("a healthy read answers"),
        "no line item is a legitimate no"
    );
    assert_eq!(
        repo::subscriptions::find_user_by_stripe_sub(&healthy, "sub_absent")
            .await
            .expect("a healthy read answers"),
        None,
        "no subscription for that Stripe id is a legitimate absence"
    );
    // Seeding the two "default" templates is the Init lifecycle's job, not
    // the migrations', so a migrated-but-uninitialised database legitimately
    // has none — which is the `Ok(None)` a create may still proceed on.
    repo::group_templates::default_id(&healthy)
        .await
        .expect("a healthy read answers");
    repo::product_templates::default_id(&healthy)
        .await
        .expect("a healthy read answers");

    let broken = ctx().await.break_reads();

    assert!(
        repo::subscriptions::active_plan_exists(&broken, "user_1", "plan_x")
            .await
            .is_err(),
        "a failed subscription read must not read as `you do not own this`"
    );
    assert!(
        repo::purchases::line_item_exists_for_product(
            &broken,
            vec![serde_json::json!("purchase_1")],
            "prod_x"
        )
        .await
        .is_err(),
        "a failed line-item read must not read as `you do not own this`"
    );
    assert!(
        repo::subscriptions::find_user_by_stripe_sub(&broken, "sub_absent")
            .await
            .is_err(),
        "a failed owner lookup must not read as `this subscription is unowned`"
    );
    assert!(
        repo::group_templates::default_id(&broken).await.is_err(),
        "a failed template read must not read as `there is no default template`"
    );
    assert!(
        repo::product_templates::default_id(&broken).await.is_err(),
        "a failed template read must not read as `there is no default template`"
    );
}
