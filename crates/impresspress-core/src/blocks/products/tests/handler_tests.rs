use std::collections::HashMap;

use base64ct::{Base64, Encoding};
use wafer_run::ErrorCode;

use super::harness::*;
use crate::blocks::products::contracts::{ApprovalStatus, DisputeStatus, ProductStatus};

// ============================================================
// Admin Product CRUD
// ============================================================

#[tokio::test]
async fn admin_create_product() {
    let ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({
            "name": "Cloud Hosting",
            "description": "Managed hosting",
            "currency": "USD"
        }),
    );

    let out = dispatch(&ctx, msg, input).await;
    let body = output_to_json(out).await;
    assert!(body["id"].as_str().is_some());
    assert_eq!(body["name"], "Cloud Hosting");
    assert_eq!(body["status"], "draft");
    assert_eq!(body["created_by"], "admin_1");
}

#[tokio::test]
async fn admin_list_products() {
    let ctx = ctx().await;

    // Create two products
    let (msg1, input1) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({
            "name": "Product A"
        }),
    );
    dispatch(&ctx, msg1, input1).await;
    let (msg2, input2) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({
            "name": "Product B"
        }),
    );
    dispatch(&ctx, msg2, input2).await;

    let (list_msg, list_input) = admin_get_msg("/b/products/api/admin/products");
    let out = dispatch(&ctx, list_msg, list_input).await;
    let body = output_to_json(out).await;
    assert!(body["records"].as_array().unwrap().len() >= 2);
}

#[tokio::test]
async fn admin_get_product() {
    let ctx = ctx().await;

    let (create_msg_data, create_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({
            "name": "Widget"
        }),
    );
    let create_out = dispatch(&ctx, create_msg_data, create_input).await;
    let id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (get_msg_data, get_input) = admin_get_msg(&format!("/b/products/api/admin/products/{id}"));
    let out = dispatch(&ctx, get_msg_data, get_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["name"], "Widget");
}

#[tokio::test]
async fn admin_update_product() {
    let ctx = ctx().await;

    let (create, create_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({
            "name": "Old Name"
        }),
    );
    let create_out = dispatch(&ctx, create, create_input).await;
    let id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (mut update, update_input) = request_msg(
        "update",
        &format!("/b/products/api/admin/products/{id}"),
        "admin_1",
        serde_json::json!({
            "name": "New Name"
        }),
    );
    update.set_meta("auth.user_roles", "admin");
    let out = dispatch(&ctx, update, update_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["name"], "New Name");
}

#[tokio::test]
async fn admin_delete_product() {
    let ctx = ctx().await;

    let (create, create_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({
            "name": "To Delete"
        }),
    );
    let create_out = dispatch(&ctx, create, create_input).await;
    let id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (mut del, del_input) =
        delete_msg(&format!("/b/products/api/admin/products/{id}"), "admin_1");
    del.set_meta("auth.user_roles", "admin");
    let out = dispatch(&ctx, del, del_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["deleted"], true);

    // Verify it's gone
    let (get, get_input) = admin_get_msg(&format!("/b/products/api/admin/products/{id}"));
    let out = dispatch(&ctx, get, get_input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

/// A soft-deleted product must 404 from the admin detail endpoint the same
/// as one that never existed — `handle_get_product` used to call
/// `db::get` with the table's old hardcoded constant directly, bypassing
/// the soft-delete filter entirely.
#[tokio::test]
async fn admin_product_detail_404s_for_a_soft_deleted_product() {
    let ctx = ctx().await;

    let mut gone = HashMap::new();
    gone.insert("name".to_string(), serde_json::json!("Gone"));
    gone.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "gone", gone).await;
    soft_delete_product(&ctx, "gone").await;

    let (msg, input) = admin_get_msg("/b/products/api/admin/products/gone");
    let out = dispatch(&ctx, msg, input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

/// `handle_update_product` (the generic admin PATCH) must not be a second
/// door onto `deleted_at`: an admin sending `{"deleted_at": null}` for a
/// soft-deleted product must not silently resurrect it — restore is the
/// only door back in. The refusal now comes from the body check, before the
/// write's own liveness filter is reached, so it reads as `InvalidArgument`
/// rather than `NotFound`; either way the row stays deleted.
#[tokio::test]
async fn admin_update_product_does_not_resurrect_via_deleted_at_null() {
    let ctx = ctx().await;

    let (create, create_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({ "name": "Oops" }),
    );
    let create_out = dispatch(&ctx, create, create_input).await;
    let id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    soft_delete_product(&ctx, &id).await;

    let (mut update, update_input) = request_msg(
        "update",
        &format!("/b/products/api/admin/products/{id}"),
        "admin_1",
        serde_json::json!({ "deleted_at": null }),
    );
    update.set_meta("auth.user_roles", "admin");
    let out = dispatch(&ctx, update, update_input).await;
    assert!(
        output_is_error(out, ErrorCode::InvalidArgument).await,
        "an admin PATCH must not resurrect a soft-deleted product"
    );

    let err = super::super::repo::products::get(&ctx, &id)
        .await
        .expect_err("the product must still read as deleted");
    assert_eq!(err.code, ErrorCode::NotFound);
}

/// A soft-deleted product must be restored before it is editable again — the
/// generic admin PATCH must refuse it outright rather than silently applying
/// unrelated field changes to a dead row.
#[tokio::test]
async fn admin_update_product_refuses_a_soft_deleted_product() {
    let ctx = ctx().await;

    let (create, create_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({ "name": "Oops" }),
    );
    let create_out = dispatch(&ctx, create, create_input).await;
    let id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    soft_delete_product(&ctx, &id).await;

    let (mut update, update_input) = request_msg(
        "update",
        &format!("/b/products/api/admin/products/{id}"),
        "admin_1",
        serde_json::json!({ "name": "New Name" }),
    );
    update.set_meta("auth.user_roles", "admin");
    let out = dispatch(&ctx, update, update_input).await;
    assert!(
        output_is_error(out, ErrorCode::NotFound).await,
        "a soft-deleted product must not be editable through the normal admin PATCH"
    );
}

/// `deleted_at` is `soft_delete`'s door, not the generic PATCH's: even for a
/// still-live product (so the liveness guard above doesn't reject the
/// request outright), an admin PATCH carrying `deleted_at` must not be able
/// to soft-delete it as a side effect of an otherwise ordinary field update.
///
/// The whole request is refused, rather than the field dropped and the rest
/// applied: a 200 whose body plainly shows `deleted_at` unchanged tells the
/// caller their write succeeded when part of it was discarded.
#[tokio::test]
async fn admin_update_product_refuses_deleted_at_in_the_request_body() {
    let ctx = ctx().await;

    let (create, create_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({ "name": "Still Live" }),
    );
    let create_out = dispatch(&ctx, create, create_input).await;
    let id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (mut update, update_input) = request_msg(
        "update",
        &format!("/b/products/api/admin/products/{id}"),
        "admin_1",
        serde_json::json!({ "name": "New Name", "deleted_at": "2026-09-01T00:00:00Z" }),
    );
    update.set_meta("auth.user_roles", "admin");
    let out = dispatch(&ctx, update, update_input).await;
    assert!(
        output_is_error(out, ErrorCode::InvalidArgument).await,
        "a PATCH naming deleted_at must be refused, not partly applied"
    );

    let record = super::super::repo::products::get(&ctx, &id)
        .await
        .expect("the generic PATCH must not have soft-deleted the product");
    assert!(crate::util::RecordExt::str_field(&record, "deleted_at").is_empty());
    assert_eq!(
        crate::util::RecordExt::str_field(&record, "name"),
        "Still Live",
        "a refused request must not apply its other fields either"
    );
}

/// The bug this whole plan exists for: `line_items.product_id` is `TEXT NOT
/// NULL`, so a hard delete of a product that was ever ordered orphaned that
/// order's line item. Soft delete must leave both rows resolvable.
#[tokio::test]
async fn admin_delete_keeps_the_row_and_its_order_history_resolvable() {
    let ctx = ctx().await;

    let mut sold = HashMap::new();
    sold.insert("name".to_string(), serde_json::json!("Sold"));
    sold.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "sold", sold).await;

    let mut order = HashMap::new();
    order.insert("user_id".to_string(), serde_json::json!("user_1"));
    order.insert("status".to_string(), serde_json::json!("completed"));
    seed(&ctx, "impresspress__products__purchases", "order_1", order).await;
    seed(
        &ctx,
        "impresspress__products__line_items",
        "line_1",
        HashMap::from([
            ("purchase_id".to_string(), serde_json::json!("order_1")),
            ("product_id".to_string(), serde_json::json!("sold")),
            ("product_name".to_string(), serde_json::json!("Sold")),
        ]),
    )
    .await;

    let (mut del, del_input) = delete_msg("/b/products/api/admin/products/sold", "admin_1");
    del.set_meta("auth.user_roles", "admin");
    let body = output_to_json(dispatch(&ctx, del, del_input).await).await;
    assert_eq!(body["deleted"], true);

    let row = wafer_core::clients::database::get(&ctx, "impresspress__products__products", "sold")
        .await
        .expect("the row must still exist");
    assert!(
        !crate::util::RecordExt::str_field(&row, "deleted_at").is_empty(),
        "deleted_at must be stamped"
    );

    let line_item =
        wafer_core::clients::database::get(&ctx, "impresspress__products__line_items", "line_1")
            .await
            .expect("the line item must still resolve");
    assert_eq!(
        crate::util::RecordExt::str_field(&line_item, "product_id"),
        "sold"
    );
}

/// A deleted product must disappear from the public catalog end-to-end
/// through the real delete handler, not just when `deleted_at` is stamped
/// by hand.
#[tokio::test]
async fn admin_delete_removes_the_product_from_the_catalog() {
    let ctx = ctx().await;

    let mut sold = HashMap::new();
    sold.insert("name".to_string(), serde_json::json!("Sold"));
    sold.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "sold", sold).await;

    let (mut del, del_input) = delete_msg("/b/products/api/admin/products/sold", "admin_1");
    del.set_meta("auth.user_roles", "admin");
    dispatch(&ctx, del, del_input).await;

    let (msg, input) = get_msg("/b/products/catalog", "");
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert!(body["records"].as_array().unwrap().is_empty());
}

/// A soft-deleted product frees its slug, because the unique index added in
/// migration 005 is partial on `deleted_at IS NULL`.
#[tokio::test]
async fn admin_delete_frees_the_products_slug() {
    let ctx = ctx().await;

    let mut first = HashMap::new();
    first.insert("name".to_string(), serde_json::json!("First"));
    first.insert("slug".to_string(), serde_json::json!("jacket"));
    seed(&ctx, "impresspress__products__products", "first", first).await;

    let (mut del, del_input) = delete_msg("/b/products/api/admin/products/first", "admin_1");
    del.set_meta("auth.user_roles", "admin");
    dispatch(&ctx, del, del_input).await;

    let (create, create_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({
            "name": "Second",
            "slug": "jacket"
        }),
    );
    let body = output_to_json(dispatch(&ctx, create, create_input).await).await;
    assert_eq!(body["slug"], "jacket", "the reused slug must not conflict");
}

// ============================================================
// Admin Group CRUD
// ============================================================

#[tokio::test]
async fn admin_create_and_list_groups() {
    let ctx = ctx().await;

    let (create, create_input) = admin_create_msg(
        "/b/products/api/admin/groups",
        serde_json::json!({
            "name": "Electronics"
        }),
    );
    let out = dispatch(&ctx, create, create_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["name"], "Electronics");
    assert_eq!(body["user_id"], "admin_1");

    let (list, list_input) = admin_get_msg("/b/products/api/admin/groups");
    let list_out = dispatch(&ctx, list, list_input).await;
    let list_body = output_to_json(list_out).await;
    assert_eq!(list_body["records"].as_array().unwrap().len(), 1);
}

// ============================================================
// Admin Types CRUD
// ============================================================

#[tokio::test]
async fn admin_create_and_list_types() {
    let ctx = ctx().await;

    let (create, create_input) = admin_create_msg(
        "/b/products/api/admin/types",
        serde_json::json!({
            "name": "subscription", "display_name": "Subscription"
        }),
    );
    dispatch(&ctx, create, create_input).await;

    let (list, list_input) = admin_get_msg("/b/products/api/admin/types");
    let out = dispatch(&ctx, list, list_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["records"].as_array().unwrap().len(), 1);
}

// ============================================================
// Admin Stats
// ============================================================

#[tokio::test]
async fn admin_stats() {
    let ctx = ctx().await;

    // Seed some products
    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!("Active Product"));
    data.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "p1", data).await;

    let mut data2 = HashMap::new();
    data2.insert("name".to_string(), serde_json::json!("Draft Product"));
    data2.insert("status".to_string(), serde_json::json!("draft"));
    seed(&ctx, "impresspress__products__products", "p2", data2).await;

    // Seed a completed purchase (user_id is NOT NULL in the real schema)
    let mut purchase_data = HashMap::new();
    purchase_data.insert("user_id".to_string(), serde_json::json!("user_1"));
    purchase_data.insert("status".to_string(), serde_json::json!("completed"));
    purchase_data.insert("total_cents".to_string(), serde_json::json!(2999));
    seed(
        &ctx,
        "impresspress__products__purchases",
        "pur1",
        purchase_data,
    )
    .await;

    let (msg, input) = admin_get_msg("/b/products/api/admin/stats");
    let out = dispatch(&ctx, msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["total_products"].as_i64().unwrap(), 2);
    assert_eq!(body["active_products"].as_i64().unwrap(), 1);
    assert_eq!(body["total_purchases"].as_i64().unwrap(), 1);
    assert_eq!(body["currency_analytics"][0]["gross_volume_minor"], 2999);
}

#[tokio::test]
async fn admin_stats_never_combine_currencies() {
    let ctx = ctx().await;
    for (id, currency, total) in [("order_nzd", "NZD", 2500), ("order_usd", "USD", 1900)] {
        seed(
            &ctx,
            "impresspress__products__purchases",
            id,
            HashMap::from([
                ("user_id".to_string(), serde_json::json!("buyer_stats")),
                ("status".to_string(), serde_json::json!("completed")),
                ("currency".to_string(), serde_json::json!(currency)),
                ("total_cents".to_string(), serde_json::json!(total)),
            ]),
        )
        .await;
    }
    for (purchase_id, dispute_id, status, currency, amount) in [
        (
            "order_nzd",
            "dp_admin_nzd",
            DisputeStatus::NeedsResponse,
            "NZD",
            700,
        ),
        ("order_usd", "dp_admin_usd", DisputeStatus::Lost, "USD", 900),
    ] {
        crate::blocks::products::repo::disputes::reconcile(
            &ctx,
            &crate::blocks::products::repo::disputes::DisputeSnapshot {
                purchase_id: purchase_id.to_string(),
                seller_account_id: String::new(),
                stripe_account_id: String::new(),
                provider_dispute_id: dispute_id.to_string(),
                provider_charge_id: format!("ch_{dispute_id}"),
                payment_intent_id: format!("pi_{dispute_id}"),
                status,
                amount_minor: amount,
                currency: currency.to_string(),
                reason: "fraudulent".to_string(),
                evidence_due_by: None,
                livemode: false,
                event_created: 1_750_000_000,
            },
        )
        .await
        .unwrap();
    }

    let (msg, input) = admin_get_msg("/b/products/api/admin/stats");
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
    let analytics = body["currency_analytics"].as_array().unwrap();
    assert_eq!(analytics.len(), 2);
    assert_eq!(analytics[0]["currency"], "NZD");
    assert_eq!(analytics[0]["gross_volume_minor"], 2500);
    assert_eq!(analytics[0]["open_dispute_count"], 1);
    assert_eq!(analytics[0]["open_disputed_volume_minor"], 700);
    assert_eq!(analytics[0]["lost_dispute_count"], 0);
    assert_eq!(analytics[1]["currency"], "USD");
    assert_eq!(analytics[1]["gross_volume_minor"], 1900);
    assert_eq!(analytics[1]["open_dispute_count"], 0);
    assert_eq!(analytics[1]["lost_dispute_count"], 1);
    assert_eq!(analytics[1]["lost_disputed_volume_minor"], 900);
}

/// One order refunded past its own total fails the stats read, even when the
/// currency's summed refunds stay under its summed gross.
///
/// A check on the sums alone cannot see this: the healthy order's 5000 hides
/// the bad order's 500 overshoot. The first dispatch is the control — the
/// same rows with a refund inside the total read fine — so the failure is
/// the per-row check and nothing else about the seed.
#[tokio::test]
async fn admin_stats_refuse_an_order_refunded_past_its_total() {
    let ctx = ctx().await;
    for (id, total, refunded) in [("order_bad", 1000, 1000), ("order_big", 5000, 0)] {
        seed(
            &ctx,
            "impresspress__products__purchases",
            id,
            HashMap::from([
                ("user_id".to_string(), serde_json::json!("buyer_stats")),
                ("status".to_string(), serde_json::json!("completed")),
                ("currency".to_string(), serde_json::json!("USD")),
                ("total_cents".to_string(), serde_json::json!(total)),
                (
                    "refunded_total_cents".to_string(),
                    serde_json::json!(refunded),
                ),
            ]),
        )
        .await;
    }
    let (msg, input) = admin_get_msg("/b/products/api/admin/stats");
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(body["currency_analytics"][0]["refunded_volume_minor"], 1000);

    wafer_core::clients::database::update(
        &ctx,
        "impresspress__products__purchases",
        "order_bad",
        HashMap::from([("refunded_total_cents".to_string(), serde_json::json!(1500))]),
    )
    .await
    .expect("overshoot the refund");

    let (msg, input) = admin_get_msg("/b/products/api/admin/stats");
    assert!(
        output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::Internal).await,
        "a refund larger than its order's total must fail the read"
    );
    let error = super::super::repo::purchases::commerce_analytics(&ctx, None)
        .await
        .expect_err("the analytics the handler reads fail the same way");
    assert!(
        error
            .message
            .contains("USD Completed orders hold a refund larger than their total"),
        "the message has to name the cause: {}",
        error.message
    );
}

#[tokio::test]
async fn seller_stats_orders_and_refunds_are_tenant_isolated() {
    let ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    for (id, user_id) in [
        ("seller_stats_1", "seller_user_1"),
        ("seller_stats_2", "seller_user_2"),
    ] {
        seed(
            &ctx,
            crate::blocks::products::repo::seller_accounts::TABLE,
            id,
            HashMap::from([
                ("user_id".to_string(), serde_json::json!(user_id)),
                ("status".to_string(), serde_json::json!("active")),
            ]),
        )
        .await;
    }
    for (id, seller_account_id, buyer, total) in [
        ("seller_order_1", "seller_stats_1", "buyer_1", 4200),
        ("seller_order_2", "seller_stats_2", "buyer_2", 9900),
    ] {
        seed(
            &ctx,
            "impresspress__products__purchases",
            id,
            HashMap::from([
                ("user_id".to_string(), serde_json::json!(buyer)),
                ("buyer_user_id".to_string(), serde_json::json!(buyer)),
                (
                    "seller_account_id".to_string(),
                    serde_json::json!(seller_account_id),
                ),
                ("status".to_string(), serde_json::json!("completed")),
                ("currency".to_string(), serde_json::json!("NZD")),
                ("total_cents".to_string(), serde_json::json!(total)),
                ("provider".to_string(), serde_json::json!("manual")),
            ]),
        )
        .await;
    }
    for (purchase_id, seller_account_id, dispute_id, status, amount) in [
        (
            "seller_order_1",
            "seller_stats_1",
            "dp_seller_stats_1",
            DisputeStatus::UnderReview,
            700,
        ),
        (
            "seller_order_2",
            "seller_stats_2",
            "dp_seller_stats_2",
            DisputeStatus::Lost,
            900,
        ),
    ] {
        crate::blocks::products::repo::disputes::reconcile(
            &ctx,
            &crate::blocks::products::repo::disputes::DisputeSnapshot {
                purchase_id: purchase_id.to_string(),
                seller_account_id: seller_account_id.to_string(),
                stripe_account_id: format!("acct_{seller_account_id}"),
                provider_dispute_id: dispute_id.to_string(),
                provider_charge_id: format!("ch_{dispute_id}"),
                payment_intent_id: format!("pi_{dispute_id}"),
                status,
                amount_minor: amount,
                currency: "NZD".to_string(),
                reason: "fraudulent".to_string(),
                evidence_due_by: None,
                livemode: false,
                event_created: 1_750_000_000,
            },
        )
        .await
        .unwrap();
    }
    seed(
        &ctx,
        "impresspress__products__line_items",
        "seller_line_1",
        HashMap::from([
            (
                "purchase_id".to_string(),
                serde_json::json!("seller_order_1"),
            ),
            (
                "product_id".to_string(),
                serde_json::json!("seller_product_1"),
            ),
            (
                "product_name".to_string(),
                serde_json::json!("Seller One Product"),
            ),
            ("quantity".to_string(), serde_json::json!(2)),
            ("total_minor".to_string(), serde_json::json!(4200)),
        ]),
    )
    .await;
    for (id, seller_account_id, error) in [
        (
            "seller_failure_own",
            "seller_stats_1",
            "Own card payment failed",
        ),
        (
            "seller_failure_other",
            "seller_stats_2",
            "Other seller failure",
        ),
    ] {
        seed(
            &ctx,
            "impresspress__products__purchases",
            id,
            HashMap::from([
                ("user_id".to_string(), serde_json::json!("buyer")),
                (
                    "seller_account_id".to_string(),
                    serde_json::json!(seller_account_id),
                ),
                ("status".to_string(), serde_json::json!("failed")),
                ("currency".to_string(), serde_json::json!("NZD")),
                ("total_cents".to_string(), serde_json::json!(1800)),
                ("reconciliation_error".to_string(), serde_json::json!(error)),
            ]),
        )
        .await;
    }
    for (id, seller_account_id, error) in [
        (
            "seller_pi_failure_own",
            "seller_stats_1",
            "Own PaymentIntent needs another payment method",
        ),
        (
            "seller_pi_failure_other",
            "seller_stats_2",
            "Other seller PaymentIntent failed",
        ),
    ] {
        seed(
            &ctx,
            "impresspress__products__purchases",
            id,
            HashMap::from([
                ("user_id".to_string(), serde_json::json!("buyer")),
                (
                    "seller_account_id".to_string(),
                    serde_json::json!(seller_account_id),
                ),
                ("status".to_string(), serde_json::json!("checkout_started")),
                ("currency".to_string(), serde_json::json!("NZD")),
                ("total_cents".to_string(), serde_json::json!(1600)),
                (
                    "provider_payment_status".to_string(),
                    serde_json::json!("payment_failed"),
                ),
                (
                    "provider_payment_error_message".to_string(),
                    serde_json::json!(error),
                ),
            ]),
        )
        .await;
    }

    let (msg, input) = get_msg("/b/products/api/seller/stats", "seller_user_1");
    let stats = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(stats["seller_account_id"], "seller_stats_1");
    assert_eq!(stats["currency_analytics"][0]["gross_volume_minor"], 4200);
    assert_eq!(stats["currency_analytics"][0]["open_dispute_count"], 1);
    assert_eq!(
        stats["currency_analytics"][0]["open_disputed_volume_minor"],
        700
    );
    assert_eq!(stats["currency_analytics"][0]["lost_dispute_count"], 0);
    assert_eq!(
        stats["currency_analytics"][0]["lost_disputed_volume_minor"],
        0
    );
    assert_eq!(
        stats["currency_analytics"][0]["top_products"][0]["product_id"],
        "seller_product_1"
    );
    let recent_failures = stats["recent_failures"].as_array().unwrap();
    assert_eq!(recent_failures.len(), 2);
    let terminal_failure = recent_failures
        .iter()
        .find(|failure| failure["order_id"] == "seller_failure_own")
        .unwrap();
    assert_eq!(terminal_failure["error"], "Own card payment failed");
    let payment_failure = recent_failures
        .iter()
        .find(|failure| failure["order_id"] == "seller_pi_failure_own")
        .unwrap();
    assert_eq!(
        payment_failure["error"],
        "Own PaymentIntent needs another payment method"
    );
    assert!(recent_failures
        .iter()
        .all(|failure| failure.get("buyer_email").is_none()));
    assert!(!recent_failures
        .iter()
        .any(|failure| failure["order_id"] == "seller_pi_failure_other"));

    let (msg, input) = get_msg("/b/products/api/seller/orders", "seller_user_1");
    let orders = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(orders["total_count"], 3);
    let order_ids = orders["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|order| order["id"].as_str())
        .collect::<Vec<_>>();
    assert!(order_ids.contains(&"seller_order_1"));
    assert!(order_ids.contains(&"seller_failure_own"));
    assert!(order_ids.contains(&"seller_pi_failure_own"));
    assert!(!order_ids.contains(&"seller_order_2"));
    assert!(!order_ids.contains(&"seller_failure_other"));
    assert!(!order_ids.contains(&"seller_pi_failure_other"));

    let (msg, input) = get_msg(
        "/b/products/api/seller/orders/seller_order_2",
        "seller_user_1",
    );
    assert!(
        output_is_error(
            dispatch(&ctx, msg, input).await,
            ErrorCode::PermissionDenied
        )
        .await
    );

    let (msg, input) = create_msg(
        "/b/products/api/seller/orders/seller_order_2/refund",
        "seller_user_1",
        serde_json::json!({"amount_minor": 1000}),
    );
    assert!(
        output_is_error(
            dispatch(&ctx, msg, input).await,
            ErrorCode::PermissionDenied
        )
        .await
    );

    let (msg, input) = create_msg(
        "/b/products/api/seller/orders/seller_order_1/refund",
        "seller_user_1",
        serde_json::json!({"amount_minor": 1200, "note": "Customer request"}),
    );
    let refund = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(refund["amount_minor"], 1200);
    assert_eq!(refund["refunded_total_minor"], 1200);
}

/// CODE_REVIEW_2026-07-16 "Error semantics fabricate successful defaults":
/// a genuine repository failure on any of the 5 independent stat
/// counts/sums must surface as an error, not be reported as "0 products /
/// $0 revenue" — an admin reading zeroed stats during a real outage would
/// mistake a broken dashboard for real (empty) business data.
/// `unwrap_or(0)` / `unwrap_or(0.0)` used to do exactly that.
#[tokio::test]
async fn admin_stats_repository_failure_surfaces_as_internal_error() {
    let ctx = ctx().await.break_reads();

    let (msg, input) = admin_get_msg("/b/products/api/admin/stats");
    let out = dispatch(&ctx, msg, input).await;
    assert!(
        output_is_error(out, ErrorCode::Internal).await,
        "a genuine repository failure must surface as Internal, not a fabricated all-zero stats body"
    );
}

/// Two counters read the products table with no filter at all (`total_products`
/// and `active_products`), so a soft-deleted product would still be counted —
/// both the admin dashboard and this stats endpoint would overstate the
/// catalog.
#[tokio::test]
async fn stats_do_not_count_soft_deleted_products() {
    let ctx = ctx().await;

    let mut live = HashMap::new();
    live.insert("name".to_string(), serde_json::json!("Live"));
    live.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "live", live).await;

    let mut gone = HashMap::new();
    gone.insert("name".to_string(), serde_json::json!("Gone"));
    gone.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "gone", gone).await;
    soft_delete_product(&ctx, "gone").await;

    let (msg, input) = admin_get_msg("/b/products/api/admin/stats");
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(body["total_products"].as_i64().unwrap(), 1);
    assert_eq!(body["active_products"].as_i64().unwrap(), 1);
}

// ============================================================
// User Product CRUD — ownership isolation
// ============================================================

async fn user_products_ctx() -> crate::test_support::TestContext {
    ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await
}

#[tokio::test]
async fn user_create_product_in_own_group() {
    let ctx = user_products_ctx().await;

    // Create a group for user_1
    let (create_group, cg_input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({
            "name": "My Store"
        }),
    );
    let group_out = dispatch(&ctx, create_group, cg_input).await;
    let group_id = output_to_json(group_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Create a product in that group
    let (create_prod, cp_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({
            "name": "Widget",
            "group_id": group_id
        }),
    );
    let out = dispatch(&ctx, create_prod, cp_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["name"], "Widget");
    assert_eq!(body["created_by"], "user_1");
}

#[tokio::test]
async fn user_cannot_create_product_in_other_users_group() {
    let ctx = user_products_ctx().await;

    // Create a group for user_1
    let (create_group, cg_input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({
            "name": "User1 Store"
        }),
    );
    let group_out = dispatch(&ctx, create_group, cg_input).await;
    let group_id = output_to_json(group_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // user_2 tries to create a product in user_1's group
    let (create_prod, cp_input) = create_msg(
        "/b/products/api/products",
        "user_2",
        serde_json::json!({
            "name": "Sneaky Product",
            "group_id": group_id
        }),
    );
    let out = dispatch(&ctx, create_prod, cp_input).await;
    assert!(output_is_error(out, ErrorCode::InvalidArgument).await);
}

#[tokio::test]
async fn user_cannot_see_other_users_products() {
    let ctx = user_products_ctx().await;

    // user_1 creates a product
    let (create, create_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({
            "name": "Private Product"
        }),
    );
    let create_out = dispatch(&ctx, create, create_input).await;
    let prod_id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // user_2 tries to get it
    let (get, get_input) = get_msg(&format!("/b/products/api/products/{prod_id}"), "user_2");
    let out = dispatch(&ctx, get, get_input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn user_cannot_update_other_users_products() {
    let ctx = user_products_ctx().await;

    let (create, create_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({
            "name": "My Product"
        }),
    );
    let create_out = dispatch(&ctx, create, create_input).await;
    let prod_id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (update, update_input) = update_msg(
        &format!("/b/products/api/products/{prod_id}"),
        "user_2",
        serde_json::json!({
            "name": "Hijacked!"
        }),
    );
    let out = dispatch(&ctx, update, update_input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn user_cannot_delete_other_users_products() {
    let ctx = user_products_ctx().await;

    let (create, create_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({
            "name": "My Product"
        }),
    );
    let create_out = dispatch(&ctx, create, create_input).await;
    let prod_id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (del, del_input) = delete_msg(&format!("/b/products/api/products/{prod_id}"), "user_2");
    let out = dispatch(&ctx, del, del_input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

/// The seller's own-product delete is the path a non-admin actually uses:
/// leaving it hard-deleting would orphan `line_items.product_id` (`TEXT NOT
/// NULL`) on exactly the path this task exists to fix.
#[tokio::test]
async fn user_delete_own_product_soft_deletes_instead_of_hard_deleting() {
    let ctx = user_products_ctx().await;

    let (create, create_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({
            "name": "My Product"
        }),
    );
    let create_out = dispatch(&ctx, create, create_input).await;
    let prod_id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (del, del_input) = delete_msg(&format!("/b/products/api/products/{prod_id}"), "user_1");
    let body = output_to_json(dispatch(&ctx, del, del_input).await).await;
    assert_eq!(body["deleted"], true);

    let row =
        wafer_core::clients::database::get(&ctx, "impresspress__products__products", &prod_id)
            .await
            .expect("the row must still exist");
    assert!(
        !crate::util::RecordExt::str_field(&row, "deleted_at").is_empty(),
        "deleted_at must be stamped"
    );
}

#[tokio::test]
async fn user_list_only_own_products() {
    let ctx = user_products_ctx().await;

    // user_1 creates a product
    let (c1, c1_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({"name": "U1 Product"}),
    );
    dispatch(&ctx, c1, c1_input).await;

    // user_2 creates a product
    let (c2, c2_input) = create_msg(
        "/b/products/api/products",
        "user_2",
        serde_json::json!({"name": "U2 Product"}),
    );
    dispatch(&ctx, c2, c2_input).await;

    // user_1 lists — should only see their own
    let (list, list_input) = get_msg("/b/products/api/products", "user_1");
    let out = dispatch(&ctx, list, list_input).await;
    let body = output_to_json(out).await;
    let records = body["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "U1 Product");
}

#[tokio::test]
async fn user_update_prevents_ownership_change() {
    let ctx = user_products_ctx().await;

    let (create, create_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({"name": "Mine"}),
    );
    let create_out = dispatch(&ctx, create, create_input).await;
    let prod_id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Try to change created_by — the whole request must be refused, not
    // silently reduced to the fields the caller does own.
    let (update, update_input) = update_msg(
        &format!("/b/products/api/products/{prod_id}"),
        "user_1",
        serde_json::json!({
            "name": "Updated",
            "created_by": "attacker"
        }),
    );
    let out = dispatch(&ctx, update, update_input).await;
    assert!(output_is_error(out, ErrorCode::InvalidArgument).await);

    let record = super::super::repo::products::get(&ctx, &prod_id)
        .await
        .expect("the product is still there");
    assert_eq!(
        crate::util::RecordExt::str_field(&record, "created_by"),
        "user_1"
    );
    assert_eq!(crate::util::RecordExt::str_field(&record, "name"), "Mine");
}

// ============================================================
// User Group CRUD — ownership isolation
// ============================================================

#[tokio::test]
async fn user_list_only_own_groups() {
    let ctx = user_products_ctx().await;

    let (g1, g1_input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "U1 Group"}),
    );
    dispatch(&ctx, g1, g1_input).await;

    let (g2, g2_input) = create_msg(
        "/b/products/groups",
        "user_2",
        serde_json::json!({"name": "U2 Group"}),
    );
    dispatch(&ctx, g2, g2_input).await;

    let (list, list_input) = get_msg("/b/products/groups", "user_1");
    let out = dispatch(&ctx, list, list_input).await;
    let body = output_to_json(out).await;
    let records = body["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "U1 Group");
}

#[tokio::test]
async fn user_cannot_update_other_users_group() {
    let ctx = user_products_ctx().await;

    let (create, create_input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "My Group"}),
    );
    let create_out = dispatch(&ctx, create, create_input).await;
    let group_id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (update, update_input) = update_msg(
        &format!("/b/products/groups/{group_id}"),
        "user_2",
        serde_json::json!({
            "name": "Stolen"
        }),
    );
    let out = dispatch(&ctx, update, update_input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn user_group_update_prevents_ownership_change() {
    let ctx = user_products_ctx().await;

    let (create, create_input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "My Group"}),
    );
    let create_out = dispatch(&ctx, create, create_input).await;
    let group_id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (update, update_input) = update_msg(
        &format!("/b/products/groups/{group_id}"),
        "user_1",
        serde_json::json!({
            "name": "Renamed",
            "user_id": "attacker"
        }),
    );
    let out = dispatch(&ctx, update, update_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["user_id"], "user_1");
}

// ============================================================
// Public Catalog
// ============================================================

#[tokio::test]
async fn catalog_only_shows_active_products() {
    let ctx = ctx().await;

    let mut d1 = HashMap::new();
    d1.insert("name".to_string(), serde_json::json!("Active"));
    d1.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "p_active", d1).await;

    let mut d2 = HashMap::new();
    d2.insert("name".to_string(), serde_json::json!("Draft"));
    d2.insert("status".to_string(), serde_json::json!("draft"));
    seed(&ctx, "impresspress__products__products", "p_draft", d2).await;

    let (msg, input) = get_msg("/b/products/catalog", "");
    let out = dispatch(&ctx, msg, input).await;
    let body = output_to_json(out).await;
    let records = body["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "Active");
}

#[tokio::test]
async fn catalog_get_hides_non_active() {
    let ctx = ctx().await;

    let mut d = HashMap::new();
    d.insert("name".to_string(), serde_json::json!("Hidden"));
    d.insert("status".to_string(), serde_json::json!("draft"));
    seed(&ctx, "impresspress__products__products", "p_hidden", d).await;

    let (msg, input) = get_msg("/b/products/catalog/p_hidden", "");
    let out = dispatch(&ctx, msg, input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

// ============================================================
// Soft-deleted products stay off every customer-facing surface
// ============================================================
//
// The catalog historically filtered on `status` alone, so a soft-deleted
// product that was still `active` stayed listed and purchasable. This is
// the hole soft delete would otherwise open; these tests pin that a
// soft-deleted row is invisible on every customer-facing read.

/// Mark a product soft-deleted the way the (future) soft-delete path will:
/// writing `deleted_at` directly, bypassing any handler.
async fn soft_delete_product(ctx: &crate::test_support::TestContext, id: &str) {
    wafer_core::clients::database::update(
        ctx,
        super::super::repo::products::TABLE,
        id,
        HashMap::from([(
            "deleted_at".to_string(),
            serde_json::json!("2026-09-01T00:00:00Z"),
        )]),
    )
    .await
    .expect("soft delete");
}

/// Whether `id`'s row still carries a `deleted_at` stamp, read straight from
/// the table.
///
/// Not `repo::products::get`, which cannot see a soft-deleted row at all and
/// so cannot tell "still deleted" from "never existed" — the distinction
/// every restore-authorization assertion below turns on.
async fn is_soft_deleted(ctx: &crate::test_support::TestContext, id: &str) -> bool {
    use crate::util::RecordExt;

    let record = wafer_core::clients::database::get(ctx, super::super::repo::products::TABLE, id)
        .await
        .expect("the product row must still exist");
    !record.str_field("deleted_at").is_empty()
}

/// Build an active, approved product with one published offer through the
/// same repo functions `stripe::handle_checkout` calls, so checkout has a
/// real purchasable offer to refuse once the product is soft-deleted.
async fn seed_published_offer(ctx: &crate::test_support::TestContext, product_id: &str) -> String {
    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!("Checkout product"));
    data.insert("status".to_string(), serde_json::json!("active"));
    seed(ctx, "impresspress__products__products", product_id, data).await;

    let definition: super::super::contracts::OfferDefinitionRequest =
        serde_json::from_value(serde_json::json!({
            "name": "Plan",
            "mode": "payment",
            "currency": "usd",
            "pricing_model": "fixed",
            "usage_type": "licensed",
            "billing_scheme": "per_unit",
            "tax_behavior": "exclusive",
            "components": [{
                "key": "price",
                "label": "Plan",
                "required": true,
                "amount": {"type": "fixed", "unit_amount_minor": 1000}
            }]
        }))
        .expect("offer definition");
    let offer = super::super::repo::offers::create(ctx, product_id, "admin_1", &definition)
        .await
        .expect("create offer");
    super::super::repo::offers::publish(ctx, product_id, &offer.offer.id)
        .await
        .expect("publish offer");
    offer.offer.id
}

#[tokio::test]
async fn catalog_list_omits_a_soft_deleted_active_product() {
    let ctx = ctx().await;

    let mut keep = HashMap::new();
    keep.insert("name".to_string(), serde_json::json!("Keep"));
    keep.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "keep", keep).await;

    let mut gone = HashMap::new();
    gone.insert("name".to_string(), serde_json::json!("Gone"));
    gone.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "gone", gone).await;
    soft_delete_product(&ctx, "gone").await;

    let (msg, input) = get_msg("/b/products/catalog", "");
    let out = dispatch(&ctx, msg, input).await;
    let body = output_to_json(out).await;
    let ids: Vec<&str> = body["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["keep"]);
}

#[tokio::test]
async fn catalog_detail_404s_for_a_soft_deleted_active_product() {
    let ctx = ctx().await;

    let mut gone = HashMap::new();
    gone.insert("name".to_string(), serde_json::json!("Gone"));
    gone.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "gone", gone).await;
    soft_delete_product(&ctx, "gone").await;

    let (msg, input) = get_msg("/b/products/catalog/gone", "");
    let out = dispatch(&ctx, msg, input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

/// Characterisation test, not a regression check: `repo::offers::get_public`
/// already refuses a soft-deleted product's offer today (before this task's
/// migration), so this must PASS before and after. It pins the behaviour the
/// migration must not lose, since checkout stops going through the table's
/// old hardcoded constant directly once this task lands.
#[tokio::test]
async fn checkout_refuses_a_soft_deleted_product() {
    let ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let offer_id = seed_published_offer(&ctx, "gone").await;
    soft_delete_product(&ctx, "gone").await;

    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({ "offer_id": offer_id }),
    );
    let out = super::super::stripe::handle_checkout(&ctx, &msg, input).await;
    assert!(
        output_is_error(out, ErrorCode::NotFound).await,
        "checkout must refuse a soft-deleted product's offer"
    );
}

// ============================================================
// Restoring a soft-deleted product — the door back out
// ============================================================
//
// Soft delete without a way back is worse than the hard delete it replaced:
// a deleted row would be permanently unreachable by any UI. These tests pin
// the restore endpoint's two obligations: it must clear `deleted_at` on the
// right row, and the restored row must be visible again everywhere a live
// product is visible (the public catalog, here — `repo::products::get`'s
// own restore test already covers the repo layer directly).

#[tokio::test]
async fn restore_endpoint_returns_the_product_to_the_catalog() {
    let ctx = ctx().await;

    let mut oops = HashMap::new();
    oops.insert("name".to_string(), serde_json::json!("oops"));
    oops.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "oops", oops).await;
    soft_delete_product(&ctx, "oops").await;

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products/oops/restore",
        serde_json::json!({}),
    );
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(body["id"], "oops");
    assert!(
        body["deleted_at"].is_null(),
        "restore must clear deleted_at: {body}"
    );

    let (catalog_msg, catalog_input) = get_msg("/b/products/catalog", "");
    let catalog_body = output_to_json(dispatch(&ctx, catalog_msg, catalog_input).await).await;
    assert_eq!(catalog_body["records"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn restore_endpoint_404s_for_an_unknown_product_id() {
    let ctx = ctx().await;

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products/missing/restore",
        serde_json::json!({}),
    );
    let out = dispatch(&ctx, msg, input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

/// The live-product case the old `restore_endpoint_404s_for_a_product_that_
/// was_never_deleted` claimed to cover and did not: it created a product,
/// ignored its id, and posted restore for the literal id `"missing"`, so it
/// only ever exercised the unknown-id path above.
///
/// Restoring a product that was never deleted is a no-op that answers 200
/// with the record — clearing an already-null `deleted_at` changes nothing.
/// Pinned rather than "fixed" because nothing reaches this endpoint except
/// the Deleted view's Restore button, which only renders for rows that ARE
/// deleted; see the report accompanying this branch for why a 409 here would
/// be defensible but is not this wave's change.
#[tokio::test]
async fn restore_endpoint_is_a_no_op_for_a_product_that_was_never_deleted() {
    let ctx = ctx().await;

    let (create, create_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({ "name": "Live" }),
    );
    let created = output_to_json(dispatch(&ctx, create, create_input).await).await;
    let id = created["id"]
        .as_str()
        .expect("created product id")
        .to_string();

    let (msg, input) = admin_create_msg(
        &format!("/b/products/api/admin/products/{id}/restore"),
        serde_json::json!({}),
    );
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(
        body["id"], id,
        "restoring a live product answers 200 with it: {body}"
    );
    assert!(
        !is_soft_deleted(&ctx, &id).await,
        "the product must stay live"
    );
}

/// A PLATFORM-owned soft-deleted product must be unreachable for a non-admin
/// through EVERY wire path that reaches a restore handler — not merely
/// through the one path a declaration happens to match.
///
/// Two routes reach a restore handler: the admin one, declared `Admin`, and
/// the seller one, declared `Authenticated`. The block serves each at
/// exactly one wire spelling now (`routes::ROUTES`; the former second
/// spelling of every seller route is pinned 404 by
/// `the_former_alias_spelling_of_a_seller_route_is_not_found`), which is
/// what closed the escalation this test was written for: `handle` used to
/// enter the user table from the raw `/b/products/...` path as well, so the
/// seller restore answered at an undeclared spelling that resolved to
/// `declared_access`'s fallback tier. Product ids are not secret;
/// `/b/products/catalog` hands them out.
///
/// The seller restore admits every logged-in caller, so what stops `user_1`
/// there is OWNERSHIP, not the tier. Seller products are enabled below
/// precisely so the feature flag's 403 cannot stand in for that check: the
/// product seeded here is platform-owned (blank `owner_id`/`created_by`),
/// which `handlers::is_owned_by` refuses for every caller.
///
/// Driven through `dispatch_routed` (the real `route_to_block`) because that
/// is where the tier is enforced — a `dispatch` test cannot see this
/// boundary at all, which is exactly how the escalation shipped.
#[tokio::test]
async fn restore_is_unreachable_for_a_non_admin_on_every_path_that_reaches_it() {
    let ctx = user_products_ctx().await;

    let mut gone = HashMap::new();
    gone.insert("name".to_string(), serde_json::json!("gone"));
    gone.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "gone", gone).await;
    soft_delete_product(&ctx, "gone").await;

    for path in [
        "/b/products/api/products/gone/restore",
        "/b/products/api/admin/products/gone/restore",
    ] {
        let (msg, input) = create_msg(path, "user_1", serde_json::json!({}));
        let out = dispatch_routed(&ctx, msg, input).await;
        assert!(
            out.collect_buffered().await.is_err(),
            "a non-admin POST to {path} must not succeed"
        );
        assert!(
            is_soft_deleted(&ctx, "gone").await,
            "a non-admin POST to {path} restored a soft-deleted product"
        );
    }

    // Positive control: the declared admin path DOES restore through the
    // same router, so the assertions above cannot be passing merely because
    // nothing routes anywhere.
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products/gone/restore",
        serde_json::json!({}),
    );
    let body = output_to_json(dispatch_routed(&ctx, msg, input).await).await;
    assert_eq!(
        body["id"], "gone",
        "an admin must be able to restore: {body}"
    );
    assert!(!is_soft_deleted(&ctx, "gone").await);
}

/// Soft delete frees the product's slug (migration 005's unique index is
/// partial on `deleted_at IS NULL`), and nothing stops a product created
/// afterwards from claiming it. Restoring the original then violates
/// `impresspress__products__products_owner_slug_uniq`.
///
/// That must read as a conflict naming the slug, not an opaque 500: the
/// Deleted view's Restore button only reloads on success, so a 500 renders
/// as nothing happening at all on the only door out of soft delete.
///
/// The conflict is read off the failed write, not from a pre-check ahead of
/// it. A pre-check answers about the moment before the write, so a slug
/// claimed in between produced the very 500 it existed to prevent; the
/// write's own failure cannot be raced, because the row is still deleted and
/// still probeable exactly when the write did not land.
#[tokio::test]
async fn restore_reports_a_slug_conflict_instead_of_an_opaque_error() {
    let ctx = ctx().await;

    let mut original = HashMap::new();
    original.insert("name".to_string(), serde_json::json!("Original"));
    original.insert("slug".to_string(), serde_json::json!("widget"));
    seed(
        &ctx,
        "impresspress__products__products",
        "original",
        original,
    )
    .await;
    soft_delete_product(&ctx, "original").await;

    // Legal only because the delete freed the slug.
    let mut claimant = HashMap::new();
    claimant.insert("name".to_string(), serde_json::json!("Claimant"));
    claimant.insert("slug".to_string(), serde_json::json!("widget"));
    seed(
        &ctx,
        "impresspress__products__products",
        "claimant",
        claimant,
    )
    .await;

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products/original/restore",
        serde_json::json!({}),
    );
    let out = dispatch(&ctx, msg, input).await;
    let error = match out.collect_buffered().await {
        Err(wafer_run::streams::output::TerminalNotResponse::Error(e)) => e,
        other => panic!("restore over a claimed slug must fail: {other:?}"),
    };
    assert_eq!(
        error.code,
        ErrorCode::AlreadyExists,
        "a slug collision is a conflict, not an internal error: {error:?}"
    );
    assert!(
        error.message.contains("widget"),
        "the conflict must name the colliding slug so an admin can act on it: {}",
        error.message
    );
    assert!(
        is_soft_deleted(&ctx, "original").await,
        "a refused restore must leave the product deleted"
    );
}

/// The admin PATCH refuses a soft-deleted product — a deleted row has to go
/// back through `restore` before it is editable again. It enforced that with
/// a separate `get` followed by a separate `update`, which is a guard with a
/// window in it: a delete landing between the two lets the PATCH write to an
/// already-deleted row and answer 200.
///
/// The window is reproduced exactly rather than raced for. The handler awaits
/// the request body between its liveness check and its write, so a body that
/// arrives only after the delete has committed puts the delete precisely
/// where a concurrent one would land.
#[tokio::test]
async fn admin_patch_refuses_a_product_soft_deleted_inside_the_request() {
    use std::sync::Arc;

    let ctx = Arc::new(ctx().await);

    let mut racer = HashMap::new();
    racer.insert("name".to_string(), serde_json::json!("before"));
    racer.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "racer", racer).await;

    let deleting = ctx.clone();
    let input = wafer_run::InputStream::from_stream(futures::stream::once(async move {
        super::super::repo::products::soft_delete(deleting.as_ref(), "racer")
            .await
            .expect("the concurrent delete lands");
        Ok(serde_json::to_vec(&serde_json::json!({"name": "after"})).unwrap())
    }));
    let (msg, _) = update_msg(
        "/b/products/api/admin/products/racer",
        "admin_1",
        serde_json::json!({}),
    );
    let mut msg = msg;
    msg.set_meta("auth.user_roles", "admin");

    let out = dispatch(ctx.as_ref(), msg, input).await;
    assert!(
        output_is_error(out, ErrorCode::NotFound).await,
        "a PATCH whose row was deleted before the write must 404, not report success"
    );

    let stored = wafer_core::clients::database::get(
        ctx.as_ref(),
        super::super::repo::products::TABLE,
        "racer",
    )
    .await
    .expect("the row still exists");
    assert_eq!(
        stored.data.get("name"),
        Some(&serde_json::json!("before")),
        "the PATCH must not have written to a deleted row"
    );
}

/// Move a competing product in or out of the slug at the exact moment one of
/// the restore's `UPDATE`s goes out, so a test can stand on any side of the
/// windows the handler's ordering leaves open.
#[derive(Clone, Copy)]
enum RaceMove {
    /// A live product claims the freed slug just BEFORE the first restore
    /// write reaches the database — after any pre-check would have looked.
    ClaimantAppearsFirst,
    /// The claimant is deleted just AFTER the first restore write refused it,
    /// so the collision probe that runs next finds nothing to blame.
    ClaimantVanishesFirst,
    /// Both moves in sequence, which is the one window the probe cannot
    /// close: the claimant is deleted after the first write, so the probe
    /// comes back clear and the handler retries — and a FRESH claimant takes
    /// the slug before that retry reaches the database.
    ReclaimedBeforeTheRetry,
}

/// The id of the second claimant, which takes the slug in the gap between a
/// clear probe and the retry that probe earned. A different row from
/// `claimant_id`: the first one is soft-deleted by then, and a deleted row
/// cannot violate a partial index keyed on `deleted_at IS NULL`.
const LATE_CLAIMANT: &str = "late_claimant";

/// Run a [`RaceMove`] against the restore's writes, then let each proceed.
///
/// The writes themselves are real: they go to the real database and succeed
/// or violate `..._owner_slug_uniq` exactly as production would. Only the
/// timing of the competing row is arranged.
#[derive(Clone)]
struct RaceTheRestoreWrite {
    inner: std::sync::Arc<crate::test_support::TestContext>,
    claimant_id: String,
    slug: String,
    when: RaceMove,
    /// How many of the HANDLER's own filtered writes to the products table
    /// have gone out, so a move can be hung on the first (the restore) or the
    /// second (the retry it earns).
    writes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Set while a move is running. The moves write to the products table
    /// themselves, and a move's own write is not one of the handler's — left
    /// uncounted it would shift every later move onto the wrong write.
    moving: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl RaceTheRestoreWrite {
    /// Seed a LIVE product holding the contested slug. Legal only while no
    /// other live row holds it, which is exactly the state each caller below
    /// has arranged.
    async fn claim_the_slug(&self, claimant_id: &str) {
        self.moving.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut claimant = HashMap::new();
        claimant.insert("name".to_string(), serde_json::json!("Claimant"));
        claimant.insert("slug".to_string(), serde_json::json!(self.slug.clone()));
        seed(
            self.inner.as_ref(),
            "impresspress__products__products",
            claimant_id,
            claimant,
        )
        .await;
        self.moving
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Soft-delete the claimant, freeing the slug again — the same write the
    /// delete endpoint issues, so the row goes on existing and the index
    /// stops seeing it.
    async fn release_the_slug(&self) {
        self.moving.store(true, std::sync::atomic::Ordering::SeqCst);
        super::super::repo::products::soft_delete(self.inner.as_ref(), &self.claimant_id)
            .await
            .expect("the concurrent delete of the claimant lands");
        self.moving
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl wafer_run::context::Context for RaceTheRestoreWrite {
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), wafer_run::WaferError> {
        self.inner
            .check_resource_access(resource, resource_type, access)
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.inner
            .resource_access_admitted(resource, resource_type, access)
    }

    async fn call_block(
        &self,
        name: &str,
        msg: wafer_run::Message,
        input: wafer_run::InputStream,
    ) -> wafer_run::OutputStream {
        #[derive(serde::Deserialize)]
        struct CollectionPeek {
            collection: String,
        }

        if name != "wafer-run/database" || msg.action() != "database.update_where_count" {
            return self.inner.call_block(name, msg, input).await;
        }
        let bytes = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return wafer_run::OutputStream::error(e),
        };
        let collection = wafer_block::codec::decode::<CollectionPeek>(&bytes)
            .map(|peek| peek.collection)
            .unwrap_or_default();
        let ours = collection == super::super::repo::products::TABLE
            && !self.moving.load(std::sync::atomic::Ordering::SeqCst);
        let nth = ours.then(|| {
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        });

        match (self.when, nth) {
            (RaceMove::ClaimantAppearsFirst, Some(0)) => {
                self.claim_the_slug(&self.claimant_id).await
            }
            (RaceMove::ReclaimedBeforeTheRetry, Some(1)) => {
                self.claim_the_slug(LATE_CLAIMANT).await
            }
            _ => {}
        }
        let out = self
            .inner
            .call_block(name, msg, wafer_run::InputStream::from_bytes(bytes))
            .await;
        match (self.when, nth) {
            (RaceMove::ClaimantVanishesFirst, Some(0))
            | (RaceMove::ReclaimedBeforeTheRetry, Some(0)) => self.release_the_slug().await,
            _ => {}
        }
        out
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
        self.inner.registered_blocks()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.inner.config_get(key)
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn wafer_run::context::Context> {
        std::sync::Arc::new(self.clone())
    }
}

/// Seed `original`, soft-deleted, holding slug `widget`.
async fn seed_a_deleted_product_holding_widget(ctx: &crate::test_support::TestContext) {
    let mut original = HashMap::new();
    original.insert("name".to_string(), serde_json::json!("Original"));
    original.insert("slug".to_string(), serde_json::json!("widget"));
    seed(
        ctx,
        "impresspress__products__products",
        "original",
        original,
    )
    .await;
    soft_delete_product(ctx, "original").await;
}

/// The ordering itself, not just its happy outcome: the restore WRITES first
/// and only reads if the write refused.
///
/// `restore_reports_a_slug_conflict_instead_of_an_opaque_error` seeds its
/// claimant before the request, so it answers 409 under a pre-check
/// implementation exactly as it does under this one — it pins the response,
/// not the ordering the response's correctness depends on. This pins the
/// ordering, by putting the claimant *inside* the window a pre-check leaves
/// open: it appears after any look-before-you-write would have looked, and
/// before the write it makes fail.
///
/// A pre-check therefore sees a clear slug, writes, and can only report the
/// index violation as an opaque 500 — the exact failure it was added to
/// prevent. Reading the collision off the write's own refusal cannot be
/// raced this way: the write either landed, in which case there was no
/// collision, or it did not, in which case the row is still deleted and the
/// claimant is still there to be named.
#[tokio::test]
async fn restore_names_a_slug_claimed_after_a_pre_check_would_have_looked() {
    let ctx = std::sync::Arc::new(ctx().await);
    seed_a_deleted_product_holding_widget(ctx.as_ref()).await;

    let racing = RaceTheRestoreWrite {
        inner: ctx.clone(),
        claimant_id: "claimant".to_string(),
        slug: "widget".to_string(),
        when: RaceMove::ClaimantAppearsFirst,
        writes: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        moving: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products/original/restore",
        serde_json::json!({}),
    );
    let out = dispatch(&racing, msg, input).await;
    let error = match out.collect_buffered().await {
        Err(wafer_run::streams::output::TerminalNotResponse::Error(e)) => e,
        other => panic!("restore over a slug claimed mid-flight must fail: {other:?}"),
    };
    assert_eq!(
        error.code,
        ErrorCode::AlreadyExists,
        "a slug claimed inside the window must still read as a conflict, not a 500: {error:?}"
    );
    assert!(
        error.message.contains("widget"),
        "the conflict must name the colliding slug: {}",
        error.message
    );
    assert!(
        is_soft_deleted(ctx.as_ref(), "original").await,
        "a refused restore must leave the product deleted"
    );
}

/// Writing first removed the pre-check's window, but the *classification*
/// still asks the database a second question, and the rows it asks about are
/// mutable. If the claimant is renamed or deleted between the failed write
/// and the probe, the probe finds nothing to blame and the admin gets back
/// the opaque 500 the whole branch exists to avoid — on a restore that would
/// now succeed if simply attempted again.
///
/// So a clear probe is not an answer, it is a reason to try again: a slug
/// nothing holds is a slug this product can re-claim. One retry, which either
/// lands (the collision really had gone) or fails again and is reported for
/// what it is.
#[tokio::test]
async fn restore_lands_when_the_claimant_vanishes_before_the_probe() {
    let ctx = std::sync::Arc::new(ctx().await);
    seed_a_deleted_product_holding_widget(ctx.as_ref()).await;

    // Legal only because the delete freed the slug.
    let mut claimant = HashMap::new();
    claimant.insert("name".to_string(), serde_json::json!("Claimant"));
    claimant.insert("slug".to_string(), serde_json::json!("widget"));
    seed(
        ctx.as_ref(),
        "impresspress__products__products",
        "claimant",
        claimant,
    )
    .await;

    let racing = RaceTheRestoreWrite {
        inner: ctx.clone(),
        claimant_id: "claimant".to_string(),
        slug: "widget".to_string(),
        when: RaceMove::ClaimantVanishesFirst,
        writes: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        moving: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products/original/restore",
        serde_json::json!({}),
    );
    let body = output_to_json(dispatch(&racing, msg, input).await).await;
    assert_eq!(
        body["id"], "original",
        "a collision that has gone by the time it is probed must not become a 500: {body}"
    );
    assert!(
        !is_soft_deleted(ctx.as_ref(), "original").await,
        "the retried restore must have landed"
    );
}

/// The window the retry above opens and does not close. The retry is itself
/// a write, and the slug it re-claims is free only until someone else takes
/// it: probe reads clear, another request claims the slug, the retry violates
/// the same index the first write did.
///
/// That second failure used to go straight to `write_error`, which answers
/// the opaque 500 this entire branch exists to prevent — on a request that
/// IS a slug conflict and has the slug in hand to name. The Deleted view's
/// Restore button only reloads on success, so what an admin sees is a button
/// that does nothing at all, with nothing said about why.
///
/// A second failure after a CLEAR probe is therefore reported as the conflict
/// it is. It is not retried again: a retry loop would chase a slug another
/// request is free to go on re-claiming, and the honest answer — someone else
/// holds this slug, free it and try again — is already available. One retry
/// stays safe for the reason it always was: `restore` only clears
/// `deleted_at` on the same already-deleted row, so a repeat creates no
/// duplicate record and no ancillary state.
#[tokio::test]
async fn restore_reports_a_conflict_when_the_slug_is_reclaimed_before_the_retry() {
    let ctx = std::sync::Arc::new(ctx().await);
    seed_a_deleted_product_holding_widget(ctx.as_ref()).await;

    // Legal only because the delete freed the slug, and what makes the FIRST
    // restore write violate `..._owner_slug_uniq`.
    let mut claimant = HashMap::new();
    claimant.insert("name".to_string(), serde_json::json!("Claimant"));
    claimant.insert("slug".to_string(), serde_json::json!("widget"));
    seed(
        ctx.as_ref(),
        "impresspress__products__products",
        "claimant",
        claimant,
    )
    .await;

    let racing = RaceTheRestoreWrite {
        inner: ctx.clone(),
        claimant_id: "claimant".to_string(),
        slug: "widget".to_string(),
        when: RaceMove::ReclaimedBeforeTheRetry,
        writes: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        moving: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products/original/restore",
        serde_json::json!({}),
    );
    let out = dispatch(&racing, msg, input).await;
    let error = match out.collect_buffered().await {
        Err(wafer_run::streams::output::TerminalNotResponse::Error(e)) => e,
        other => panic!("a retry over a re-claimed slug must fail: {other:?}"),
    };
    assert_eq!(
        error.code,
        ErrorCode::AlreadyExists,
        "a slug re-claimed between the probe and the retry must still read as \
         a conflict, not as the 500 the probe exists to prevent: {error:?}"
    );
    assert!(
        error.message.contains("widget"),
        "the conflict must name the colliding slug: {}",
        error.message
    );
    assert!(
        is_soft_deleted(ctx.as_ref(), "original").await,
        "a refused restore must leave the product deleted"
    );
    // The late claimant is the one holding the slug now, and it is live: the
    // conflict named a real, current claim rather than a stale one.
    assert!(
        !is_soft_deleted(ctx.as_ref(), LATE_CLAIMANT).await,
        "the slug's new holder must still be live"
    );
}

/// A restore write that fails for a reason other than the slug index is not
/// a slug question at all. Only `AlreadyExists` sends a failed restore to the
/// collision probe; any other failure is the write's own error, against a
/// correlation id — never a 409 blaming a slug, which for a product with no
/// slug would be `A product with the slug "" already exists`.
#[tokio::test]
async fn restore_of_a_slugless_product_reports_the_write_failure_not_a_slug_conflict() {
    let ctx = ctx().await;

    // No `slug` key at all, which is what every product created without one
    // carries.
    let mut slugless = HashMap::new();
    slugless.insert("name".to_string(), serde_json::json!("Slugless"));
    seed(
        &ctx,
        "impresspress__products__products",
        "slugless",
        slugless,
    )
    .await;
    soft_delete_product(&ctx, "slugless").await;

    // Mutations now fail while reads still resolve: the restore write fails
    // for a reason that has nothing to do with any index, and the probe that
    // runs next can still read the row it is asked about.
    let ctx = ctx.break_writes();

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products/slugless/restore",
        serde_json::json!({}),
    );
    let out = dispatch(&ctx, msg, input).await;
    let error = match out.collect_buffered().await {
        Err(wafer_run::streams::output::TerminalNotResponse::Error(e)) => e,
        other => panic!("a restore whose write failed must fail: {other:?}"),
    };
    assert_eq!(
        error.code,
        ErrorCode::Internal,
        "a product with no slug cannot collide on the slug index, so its \
         failed restore must carry the write's own error rather than a \
         conflict blaming an empty slug: {error:?}"
    );
    assert!(
        is_soft_deleted(&ctx, "slugless").await,
        "a refused restore must leave the product deleted"
    );
}

/// "Could not tell" is not "clear": when the restore write is refused on the
/// slug index and the collision probe that would name the claimant cannot
/// itself run, nothing is retried, and the write's own answer stands. That
/// answer is `AlreadyExists` — the backend's classification of the unique
/// violation — so the response is the 409 `crud` gives a duplicate key,
/// without a slug to name, rather than a guess in either direction.
///
/// The restore write is failed by a REAL slug collision (the claimant seeded
/// below), not by breaking the write itself: `TestContext::break_list_reads`
/// fails only listings, and every real backend runs a restore as one
/// `UPDATE` with no listing in it, so the index is the one thing production
/// can make this write fail on.
#[tokio::test]
async fn restore_whose_collision_probe_cannot_run_keeps_the_writes_own_conflict() {
    let ctx = ctx().await;

    let mut original = HashMap::new();
    original.insert("name".to_string(), serde_json::json!("Original"));
    original.insert("slug".to_string(), serde_json::json!("widget"));
    seed(
        &ctx,
        "impresspress__products__products",
        "original",
        original,
    )
    .await;
    soft_delete_product(&ctx, "original").await;

    // Legal only because the delete freed the slug — and it is what makes
    // the restore's `UPDATE` violate `..._owner_slug_uniq`, exactly as in
    // `restore_reports_a_slug_conflict_instead_of_an_opaque_error`.
    let mut claimant = HashMap::new();
    claimant.insert("name".to_string(), serde_json::json!("Claimant"));
    claimant.insert("slug".to_string(), serde_json::json!("widget"));
    seed(
        &ctx,
        "impresspress__products__products",
        "claimant",
        claimant,
    )
    .await;

    // Listings now fail while by-id reads still resolve: the restore write
    // fails on the index, `get_deleted` still finds the row, and the
    // collision probe's `list_all` is the one read that cannot run — the
    // exact interleaving a transient database wobble produces on top of a
    // genuine collision, and the only one that reaches the "probe could not
    // tell" branch.
    let ctx = ctx.break_list_reads();

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products/original/restore",
        serde_json::json!({}),
    );
    let out = dispatch(&ctx, msg, input).await;
    let error = match out.collect_buffered().await {
        Err(wafer_run::streams::output::TerminalNotResponse::Error(e)) => e,
        other => panic!("a refused restore must fail: {other:?}"),
    };
    assert_eq!(
        (error.code, error.message.as_str()),
        (ErrorCode::AlreadyExists, crate::blocks::crud::DUPLICATE_KEY),
        "a probe that could not run must neither retry nor name a slug it has \
         not confirmed: {error:?}"
    );
    assert!(
        is_soft_deleted(&ctx, "original").await,
        "a restore that could not check its slug must leave the product deleted"
    );
}

// ============================================================
// Group products endpoint
// ============================================================

#[tokio::test]
async fn user_group_products_list() {
    let ctx = user_products_ctx().await;

    // Create group
    let (cg, cg_input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "Store"}),
    );
    let gr = dispatch(&ctx, cg, cg_input).await;
    let gid = output_to_json(gr).await["id"].as_str().unwrap().to_string();

    // Create product in group
    let (cp, cp_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({
            "name": "In Group",
            "group_id": gid
        }),
    );
    dispatch(&ctx, cp, cp_input).await;

    // List products in group
    let (list, list_input) = get_msg(&format!("/b/products/groups/{gid}/products"), "user_1");
    let out = dispatch(&ctx, list, list_input).await;
    let body = output_to_json(out).await;
    assert!(!body["records"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn user_cannot_list_other_users_group_products() {
    let ctx = user_products_ctx().await;

    let (cg, cg_input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "Private"}),
    );
    let gr = dispatch(&ctx, cg, cg_input).await;
    let gid = output_to_json(gr).await["id"].as_str().unwrap().to_string();

    // user_2 tries to list user_1's group products
    let (list, list_input) = get_msg(&format!("/b/products/groups/{gid}/products"), "user_2");
    let out = dispatch(&ctx, list, list_input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

// ============================================================
// User products disabled by default
// ============================================================

#[tokio::test]
async fn user_products_rejected_when_disabled() {
    let ctx = ctx().await; // no ALLOW_USER_PRODUCTS config → defaults to false

    let (create, create_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({"name": "Test"}),
    );
    let out = dispatch(&ctx, create, create_input).await;
    assert!(output_is_error(out, ErrorCode::PermissionDenied).await);

    let (list, list_input) = get_msg("/b/products/api/products", "user_1");
    let out = dispatch(&ctx, list, list_input).await;
    assert!(output_is_error(out, ErrorCode::PermissionDenied).await);

    let (group, group_input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "Group"}),
    );
    let out = dispatch(&ctx, group, group_input).await;
    assert!(output_is_error(out, ErrorCode::PermissionDenied).await);
}

#[tokio::test]
async fn catalog_still_works_when_user_products_disabled() {
    let ctx = ctx().await; // user products disabled

    let mut d = std::collections::HashMap::new();
    d.insert("name".to_string(), serde_json::json!("Plan"));
    d.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "p1", d).await;

    let (msg, input) = get_msg("/b/products/catalog", "");
    let out = dispatch(&ctx, msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["records"].as_array().unwrap().len(), 1);
}

// ============================================================
// Not-found routes
// ============================================================

#[tokio::test]
async fn unknown_admin_route() {
    let ctx = ctx().await;
    let (msg, input) = admin_get_msg("/b/products/api/admin/nonexistent");
    let out = dispatch(&ctx, msg, input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn unknown_user_route() {
    let ctx = ctx().await;
    let (msg, input) = get_msg("/b/products/nonexistent", "user_1");
    let out = dispatch(&ctx, msg, input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

// ============================================================
// Page shell (ui::shell_page) + data_table adoption
// ============================================================

/// Commerce administration belongs in the admin shell. Its registered sidebar
/// item and the page's canonical request path must agree so Products is active.
#[tokio::test]
async fn overview_highlights_products_nav_via_request_path() {
    let ctx = ctx().await;
    let (msg, _input) = admin_get_msg("/b/products/admin/");
    let html = output_to_html(super::super::pages::overview(&ctx, &msg).await).await;

    // Full shell chrome present (shell_page wrapped a non-htmx request in the
    // sidebar+topbar document, not a bare fragment). The `.shell` wrapper only
    // exists on the full page, so it's the distinguishing marker.
    assert!(html.contains(r#"class="shell""#), "expected shell chrome");
    assert!(
        html.contains(r#"class="sidebar"#),
        "expected sidebar in full doc"
    );
    // Products admin nav item is active because current_path == its href.
    assert!(
        html.contains(r#"href="/b/products/admin/""#),
        "Products admin nav item should be present"
    );
    assert!(
        html.contains("is-active"),
        "the active sidebar item (Products) should carry is-active"
    );
}

/// When `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` is off and the catalog is
/// empty, the Overview page used to render a bare, actionless stat grid — a
/// live 403 on the gated user route (`/b/products/api/products`) was the
/// only signal that self-serve selling was disabled. It must now name the
/// config var and point at Settings, and must NOT show the "Add product"
/// CTA that belongs to the enabled+empty state (that CTA is safe either
/// way — it targets the *admin* create route, which isn't gated by this
/// flag — but the two states render distinct copy, so assert only the
/// disabled-state text appears).
#[tokio::test]
async fn overview_shows_disabled_notice_when_user_products_off() {
    let ctx = ctx().await; // no ALLOW_USER_PRODUCTS config → defaults to false
    let (msg, _input) = admin_get_msg("/b/products/admin/");
    let html = output_to_html(super::super::pages::overview(&ctx, &msg).await).await;

    assert!(
        html.contains("Customer accounts cannot create their own listings yet"),
        "disabled notice should clearly explain the customer-facing effect: {html}"
    );
    assert!(
        html.contains("Settings"),
        "disabled notice should point at how to enable it: {html}"
    );
    assert!(
        html.contains(r#"href="/b/products/admin/settings""#),
        "disabled notice's action should link to the Settings page: {html}"
    );
    assert!(
        !html.contains("Add your first product"),
        "disabled overview should not show the enabled+empty CTA copy: {html}"
    );
}

/// Enabled + empty catalog: the Overview page must show a working
/// "Add your first product" CTA to the real admin create path (Manage
/// Products, which owns the "+ New Product" modal), and must not show the
/// disabled-state notice.
#[tokio::test]
async fn overview_shows_add_product_cta_when_enabled_and_empty() {
    let ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    let (msg, _input) = admin_get_msg("/b/products/admin/");
    let html = output_to_html(super::super::pages::overview(&ctx, &msg).await).await;

    assert!(
        html.contains("Add your first product"),
        "enabled+empty overview should show the add-product CTA: {html}"
    );
    assert!(
        html.contains(r#"href="/b/products/admin/manage""#),
        "CTA should link to the real create path (Manage Products): {html}"
    );
    assert!(
        !html.contains("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS"),
        "enabled overview should not show the disabled-state notice: {html}"
    );
}

/// Once the catalog has products, the empty-state block (CTA or notice)
/// disappears entirely regardless of the enabled flag.
#[tokio::test]
async fn overview_hides_empty_state_once_products_exist() {
    let ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    let (c, c_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({ "name": "Cloud Hosting" }),
    );
    dispatch(&ctx, c, c_input).await;

    let (msg, _input) = admin_get_msg("/b/products/admin/");
    let html = output_to_html(super::super::pages::overview(&ctx, &msg).await).await;

    assert!(
        !html.contains("Add your first product"),
        "CTA should be gone once the catalog has products: {html}"
    );
}

/// CODE_REVIEW_2026-07-16 "Error semantics fabricate successful defaults": a
/// genuine repository failure on the Overview page's stat counts must
/// surface as an error, not silently render the page with fabricated "0"
/// stats — which would ALSO wrongly trigger the "Add your first product"
/// empty-state CTA during a real outage on a catalog that isn't actually
/// empty.
#[tokio::test]
async fn overview_repository_failure_surfaces_as_internal_error() {
    let ctx = ctx().await.break_reads();
    let (msg, _input) = admin_get_msg("/b/products/admin/");
    let out = super::super::pages::overview(&ctx, &msg).await;
    assert!(
        output_is_error(out, ErrorCode::Internal).await,
        "a genuine repository failure must surface as Internal, not a fabricated empty overview page"
    );
}

/// The catalog's primary action opens the dedicated product wizard rather
/// than the removed name/price-only modal.
#[tokio::test]
async fn manage_products_page_links_to_product_wizard() {
    let ctx = ctx().await;
    let (msg, _input) = admin_get_msg("/b/products/admin/manage");
    let html = output_to_html(super::super::pages::manage_products(&ctx, &msg).await).await;

    assert!(
        html.contains("+ New Product"),
        "manage page should render the create-product trigger: {html}"
    );
    assert!(
        html.contains(r#"href="/b/products/admin/new""#),
        "manage page should link to the full product wizard: {html}"
    );
}

#[tokio::test]
async fn admin_product_wizard_exposes_simple_and_advanced_templates() {
    let ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__DEFAULT_CURRENCY", "NZD"),
        ("IMPRESSPRESS__PRODUCTS__AUTOMATIC_TAX", "true"),
        ("IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY", "nz"),
    ])
    .await;
    let (msg, _input) = admin_get_msg("/b/products/admin/new");
    let html = output_to_html(super::super::pages::product_wizard(&ctx, &msg, true).await).await;

    for template in [
        "simple_product",
        "simple_subscription",
        "configurable_product",
        "configurable_subscription",
    ] {
        assert!(html.contains(&format!(r#"value="{template}""#)));
    }
    assert!(html.contains("Customer fields"));
    assert!(html.contains("Itemized price rows"));
    assert!(html.contains("Checkout options"));
    assert!(html.contains("Create and publish"));
    assert!(html.contains(r#"value="NZD""#));
    assert!(html.contains(r#"id="wizard-automatic-tax" type="checkbox" checked"#));
    assert!(html.contains("/b/products/api/admin/products"));
    assert!(html.contains(r#"id="wizard-shipping-countries""#));
    assert!(html.contains(r#"value="NZ""#));
    assert!(html.contains("Inline rates work in hosted and embedded Checkout"));
    assert!(html.contains("Create a Stripe Customer for one-time payments"));

    // The advanced rows, the price-model options and the money maths are the
    // bundle's, not the page's. `loaded_bundle` asserts the page loads it.
    #[cfg(feature = "embed-assets")]
    {
        let js = loaded_bundle(&html, "products-wizard.js");
        assert!(js.contains("Condition"));
        assert!(js.contains("BigInt"), "money conversion must be exact");
        assert!(js.contains("wizardCurrencyExponent"));
        assert!(js.contains("unit_amount_minor"));
        assert!(js.contains(r#"value="graduated""#));
        assert!(js.contains(r#"value="volume""#));
        assert!(js.contains(r#"value="package""#));
        assert!(js.contains("wizardParseTiers"));
        assert!(js.contains("wizardParseLookup"));
        assert!(js.contains("wizardParseShippingCountries"));
        assert!(js.contains("wizardParseShippingOptions"));
        assert!(js.contains("upper bound | unit amount | flat amount"));
    }
}

#[tokio::test]
async fn seller_product_wizard_reuses_builder_with_seller_routes_and_moderation_copy() {
    let ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    let (msg, _input) = get_msg("/b/products/my-products/new", "seller_1");
    let html = output_to_html(super::super::pages::product_wizard(&ctx, &msg, false).await).await;

    assert!(html.contains("Submit for publication"));
    assert!(html.contains("administrator review"));
    assert!(html.contains("/b/products/api/products"));
    assert!(!html.contains("/b/products/api/admin/products"));
    assert!(html.contains(r#"href="/b/products/my-products""#));
}

#[tokio::test]
async fn admin_product_manager_renders_product_offer_lifecycle_and_payment_link_controls() {
    let test_ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({
            "name": "Managed plan",
            "slug": "managed-plan",
            "description": "Lifecycle test",
            "currency": "NZD",
            "fulfillment_kind": "entitlement"
        }),
    );
    let product = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let product_id = product["id"].as_str().unwrap();
    let offer_collection = format!("/b/products/api/admin/products/{product_id}/offers");
    let definition = |name: &str| {
        serde_json::json!({
            "name": name,
            "mode": "payment",
            "currency": "NZD",
            "pricing_model": "fixed",
            "interval_count": 1,
            "usage_type": "licensed",
            "billing_scheme": "per_unit",
            "tax_behavior": "exclusive",
            "variables": [],
            "components": [{
                "key": "price",
                "label": name,
                "sort_order": 0,
                "required": true,
                "amount": {"type": "fixed", "unit_amount_minor": 2599},
                "quantity": {"type": "fixed", "value": 1},
                "condition": {"op": "always"}
            }],
            "checkout": {"automatic_tax": true}
        })
    };
    let (msg, input) = admin_create_msg(&offer_collection, definition("Published price"));
    let active = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let active_id = active["offer"]["id"].as_str().unwrap();
    let (msg, input) = admin_create_msg(
        &format!("{offer_collection}/{active_id}/publish"),
        serde_json::json!({}),
    );
    output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let (msg, input) = admin_create_msg(&offer_collection, definition("Editable price"));
    let draft = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let draft_id = draft["offer"]["id"].as_str().unwrap();

    let (msg, _input) = admin_get_msg(&format!("/b/products/admin/products/{product_id}"));
    let html = output_to_html(
        super::super::pages::product_manager(&test_ctx, &msg, product_id, true).await,
    )
    .await;

    assert!(html.contains("Managed plan"));
    assert!(html.contains("Save product details"));
    assert!(html.contains("Duplicate product"));
    assert!(html.contains("Published offers are immutable"));
    assert!(html.contains("Advanced draft definition"));
    assert!(html.contains("Duplicate to draft"));
    assert!(html.contains("Sync to Stripe"));
    assert!(html.contains("Shareable Stripe Payment Links"));
    assert!(html.contains("Create or reuse Payment Link"));
    assert!(html.contains("unit_amount_minor"));
    assert!(html.contains(&format!(
        "/b/products/api/admin/products/{product_id}/offers/{draft_id}"
    )));
    assert!(html.contains(&format!(
        "/b/products/api/admin/products/{product_id}/offers/{active_id}/presets"
    )));
    assert!(html.contains(&format!(
        "/b/products/api/admin/products/{product_id}/offers/{active_id}"
    )));
    assert!(html.contains(&format!(
        "/b/products/api/admin/products/{product_id}/offers/{active_id}/payment-links"
    )));
    // Copy-to-clipboard is the bundle's; the page's half is loading it.
    #[cfg(feature = "embed-assets")]
    assert!(loaded_bundle(&html, "products-manager.js").contains("navigator.clipboard"));

    wafer_core::clients::database::update(
        &test_ctx,
        super::super::repo::offers::TABLE,
        active_id,
        HashMap::from([
            ("sync_status".to_string(), serde_json::json!("failed")),
            (
                "sync_error".to_string(),
                serde_json::json!("Stripe Price response did not match the immutable offer row"),
            ),
        ]),
    )
    .await
    .unwrap();
    let (msg, _input) = admin_get_msg(&format!("/b/products/admin/products/{product_id}"));
    let retry_html = output_to_html(
        super::super::pages::product_manager(&test_ctx, &msg, product_id, true).await,
    )
    .await;
    assert!(retry_html.contains("Retry Stripe sync"));
    assert!(retry_html.contains(
        "Stripe sync error: Stripe Price response did not match the immutable offer row"
    ));

    wafer_core::clients::database::update(
        &test_ctx,
        super::super::repo::offers::TABLE,
        active_id,
        HashMap::from([
            ("sync_status".to_string(), serde_json::json!("synced")),
            ("sync_error".to_string(), serde_json::json!("")),
        ]),
    )
    .await
    .unwrap();
    let (msg, _input) = admin_get_msg(&format!("/b/products/admin/products/{product_id}"));
    let reconcile_html = output_to_html(
        super::super::pages::product_manager(&test_ctx, &msg, product_id, true).await,
    )
    .await;
    assert!(reconcile_html.contains("Reconcile Stripe"));
}

#[tokio::test]
async fn seller_product_manager_is_owner_isolated_and_uses_seller_endpoints() {
    let test_ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    let (msg, input) = create_msg(
        "/b/products/api/products",
        "seller_owner",
        serde_json::json!({
            "name": "Seller product",
            "slug": "seller-product",
            "currency": "USD"
        }),
    );
    let product = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let product_id = product["id"].as_str().unwrap();

    let (owner_msg, _input) = get_msg(
        &format!("/b/products/my-products/{product_id}"),
        "seller_owner",
    );
    let html = output_to_html(
        super::super::pages::product_manager(&test_ctx, &owner_msg, product_id, false).await,
    )
    .await;
    assert!(html.contains("Seller product"));
    assert!(html.contains(&format!("/b/products/api/products/{product_id}")));
    assert!(!html.contains("/b/products/api/admin/products"));

    let (other_msg, _input) = get_msg(
        &format!("/b/products/my-products/{product_id}"),
        "different_seller",
    );
    let out = super::super::pages::product_manager(&test_ctx, &other_msg, product_id, false).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn admin_product_duplicate_copies_safe_metadata_and_non_archived_offers_as_drafts() {
    let test_ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({
            "name": "Original product",
            "slug": "original-product",
            "description": "Keep this description",
            "currency": "NZD",
            "fulfillment_kind": "download"
        }),
    );
    let source = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let source_id = source["id"].as_str().unwrap();
    let collection = format!("/b/products/api/admin/products/{source_id}/offers");
    let offer_definition = |name: &str, amount: i64| {
        serde_json::json!({
            "name": name,
            "mode": "payment",
            "currency": "NZD",
            "pricing_model": "fixed",
            "interval_count": 1,
            "usage_type": "licensed",
            "billing_scheme": "per_unit",
            "tax_behavior": "exclusive",
            "variables": [],
            "components": [{
                "key": "price",
                "label": name,
                "required": true,
                "amount": {"type": "fixed", "unit_amount_minor": amount},
                "quantity": {"type": "fixed", "value": 1},
                "condition": {"op": "always"}
            }],
            "checkout": {}
        })
    };
    let (msg, input) = admin_create_msg(&collection, offer_definition("Current price", 2599));
    let current = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let current_id = current["offer"]["id"].as_str().unwrap();
    let (msg, input) = admin_create_msg(
        &format!("{collection}/{current_id}/publish"),
        serde_json::json!({}),
    );
    output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let (msg, input) = admin_create_msg(&collection, offer_definition("Old price", 1999));
    let old = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let old_id = old["offer"]["id"].as_str().unwrap();
    let (mut msg, input) = delete_msg(&format!("{collection}/{old_id}"), "admin_1");
    msg.set_meta("auth.user_roles", "admin");
    output_to_json(dispatch(&test_ctx, msg, input).await).await;

    let (msg, input) = admin_create_msg(
        &format!("/b/products/api/admin/products/{source_id}/duplicate"),
        serde_json::json!({}),
    );
    let duplicated = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let copy = &duplicated["product"];
    assert_ne!(copy["id"], source["id"]);
    assert_eq!(copy["name"], "Original product copy");
    assert!(copy["slug"]
        .as_str()
        .unwrap()
        .starts_with("original-product-copy-"));
    assert_eq!(copy["description"], "Keep this description");
    assert_eq!(copy["status"], "draft");
    assert_eq!(copy["owner_kind"], "platform");
    assert_eq!(copy["approval_status"], "approved");
    let offers = duplicated["offers"].as_array().unwrap();
    assert_eq!(offers.len(), 1, "archived offers must not be copied");
    assert_eq!(offers[0]["status"], "draft");
    assert_eq!(offers[0]["offer"]["name"], "Current price");
    assert_eq!(
        offers[0]["offer"]["components"][0]["amount"]["unit_amount_minor"],
        2599
    );
    assert_ne!(offers[0]["offer"]["id"], current["offer"]["id"]);
    assert_eq!(offers[0]["offer"]["stripe_product_id"], "");
    assert_eq!(offers[0]["offer"]["stripe_price_id"], "");
}

#[tokio::test]
async fn seller_product_duplicate_preserves_owner_moderation_and_rejects_other_sellers() {
    let test_ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        ("IMPRESSPRESS__PRODUCTS__SELLER_MODERATION_REQUIRED", "true"),
    ])
    .await;
    let (msg, input) = create_msg(
        "/b/products/api/products",
        "seller_a",
        serde_json::json!({"name": "Owned product", "slug": "owned-product"}),
    );
    let source = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let source_id = source["id"].as_str().unwrap();
    let path = format!("/b/products/api/products/{source_id}/duplicate");

    let (msg, input) = create_msg(&path, "seller_b", serde_json::json!({}));
    assert!(output_is_error(dispatch(&test_ctx, msg, input).await, ErrorCode::NotFound).await);

    let (msg, input) = create_msg(&path, "seller_a", serde_json::json!({}));
    let duplicated = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    assert_eq!(duplicated["product"]["owner_kind"], "user");
    assert_eq!(duplicated["product"]["owner_id"], "seller_a");
    assert_eq!(duplicated["product"]["created_by"], "seller_a");
    assert_eq!(duplicated["product"]["status"], "draft");
    assert_eq!(duplicated["product"]["approval_status"], "draft");
}

#[tokio::test]
async fn admin_wizard_sequence_creates_and_publishes_subscription_offer() {
    let test_ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({
            "name": "Team plan",
            "slug": "team-plan",
            "description": "Monthly access",
            "currency": "NZD",
            "fulfillment_kind": "entitlement",
            "product_template_id": "simple_subscription"
        }),
    );
    let product = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let product_id = product["id"].as_str().unwrap();
    assert_eq!(product["status"], "draft");

    let offer_collection = format!("/b/products/api/admin/products/{product_id}/offers");
    let (msg, input) = admin_create_msg(
        &offer_collection,
        serde_json::json!({
            "name": "Team plan",
            "mode": "subscription",
            "currency": "NZD",
            "pricing_model": "fixed",
            "recurring_interval": "month",
            "interval_count": 1,
            "usage_type": "licensed",
            "billing_scheme": "per_unit",
            "tax_behavior": "exclusive",
            "variables": [],
            "components": [{
                "key": "price",
                "label": "Team plan",
                "sort_order": 0,
                "required": true,
                "amount": {"type": "fixed", "unit_amount_minor": 1999},
                "quantity": {"type": "fixed", "value": 1},
                "condition": {"op": "always"},
                "recurrence": {"interval": "month", "interval_count": 1}
            }],
            "checkout": {
                "allow_promotion_codes": true,
                "automatic_tax": true,
                "collect_billing_address": true,
                "trial_days": 14
            }
        }),
    );
    let managed = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let offer_id = managed["offer"]["id"].as_str().unwrap();
    assert_eq!(
        managed["offer"]["components"][0]["amount"]["unit_amount_minor"],
        1999
    );
    assert_eq!(managed["offer"]["checkout"]["trial_days"], 14);

    let (msg, input) = admin_create_msg(
        &format!("{offer_collection}/{offer_id}/publish"),
        serde_json::json!({}),
    );
    let published = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    assert_eq!(published["status"], "active");
    let (msg, input) = update_msg(
        &format!("/b/products/api/admin/products/{product_id}"),
        "admin_1",
        serde_json::json!({"status": "active"}),
    );
    let active_product = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    assert_eq!(active_product["status"], "active");

    let (msg, input) = get_msg(&format!("/b/products/storefront/{product_id}"), "");
    let storefront = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    assert_eq!(storefront["offers"][0]["mode"], "subscription");
    assert_eq!(storefront["offers"][0]["recurring_interval"], "month");
}

#[tokio::test]
async fn seller_wizard_sequence_creates_configurable_offer_then_enters_moderation() {
    let test_ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        ("IMPRESSPRESS__PRODUCTS__SELLER_MODERATION_REQUIRED", "true"),
    ])
    .await;
    let (msg, input) = create_msg(
        "/b/products/api/products",
        "seller_wizard",
        serde_json::json!({
            "name": "Custom engraving",
            "slug": "custom-engraving",
            "currency": "USD",
            "fulfillment_kind": "manual",
            "product_template_id": "configurable_product"
        }),
    );
    let product = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let product_id = product["id"].as_str().unwrap();
    assert_eq!(product["owner_id"], "seller_wizard");
    assert_eq!(product["approval_status"], "draft");

    let offer_collection = format!("/b/products/api/products/{product_id}/offers");
    let (msg, input) = create_msg(
        &offer_collection,
        "seller_wizard",
        serde_json::json!({
            "name": "Custom engraving",
            "mode": "payment",
            "currency": "USD",
            "pricing_model": "components",
            "interval_count": 1,
            "usage_type": "licensed",
            "billing_scheme": "per_unit",
            "tax_behavior": "unspecified",
            "variables": [{
                "key": "characters",
                "kind": "integer",
                "label": "Characters",
                "required": true,
                "minimum": "1",
                "maximum": "100",
                "step": "1",
                "visibility": "public",
                "sort_order": 0
            }],
            "components": [
                {
                    "key": "base",
                    "label": "Engraving setup",
                    "sort_order": 0,
                    "required": true,
                    "amount": {"type": "fixed", "unit_amount_minor": 500},
                    "quantity": {"type": "fixed", "value": 1},
                    "condition": {"op": "always"}
                },
                {
                    "key": "characters",
                    "label": "Characters",
                    "sort_order": 1,
                    "required": true,
                    "amount": {"type": "per_unit", "input": "characters", "unit_amount_minor": 25},
                    "quantity": {"type": "fixed", "value": 1},
                    "condition": {"op": "always"}
                }
            ],
            "checkout": {"collect_billing_address": true}
        }),
    );
    let managed = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    let offer_id = managed["offer"]["id"].as_str().unwrap();
    assert_eq!(managed["offer"]["pricing_model"], "components");

    let (msg, input) = create_msg(
        &format!("{offer_collection}/{offer_id}/publish"),
        "seller_wizard",
        serde_json::json!({}),
    );
    let offer = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    assert_eq!(offer["status"], "active");
    let (msg, input) = update_msg(
        &format!("/b/products/api/products/{product_id}"),
        "seller_wizard",
        serde_json::json!({"status": "active"}),
    );
    let pending = output_to_json(dispatch(&test_ctx, msg, input).await).await;
    assert_eq!(pending["status"], "pending_review");
    assert_eq!(pending["approval_status"], "pending");

    let (msg, input) = get_msg(&format!("/b/products/storefront/{product_id}"), "");
    assert!(
        output_is_error(dispatch(&test_ctx, msg, input).await, ErrorCode::NotFound).await,
        "pending seller products must stay out of the public storefront"
    );
}

/// The products list pages adopt `ui::components::data_table`, which carries
/// the PR #75 mobile card-collapse fix via `td[data-label]`. Assert the manage
/// page renders the `.data-table` structure with per-cell data labels (so the
/// mobile baseline collapse works) instead of the old `.table-container`.
#[tokio::test]
async fn manage_products_uses_data_table_with_mobile_labels() {
    let ctx = ctx().await;
    let (c, c_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({ "name": "Widget" }),
    );
    dispatch(&ctx, c, c_input).await;

    let (msg, _input) = admin_get_msg("/b/products/admin/manage");
    let html = output_to_html(super::super::pages::manage_products(&ctx, &msg).await).await;

    assert!(
        html.contains(r#"class="data-table""#),
        "manage page should render the shared data_table component"
    );
    assert!(
        html.contains(r#"data-label="Name""#),
        "data_table cells should carry data-label for the mobile card collapse"
    );
    assert!(html.contains("Widget"), "the seeded product should render");
}

/// `?view=deleted` is the only way to reach a soft-deleted row from the
/// admin UI — without it, soft delete is a one-way door. Pins that the
/// deleted view shows exactly the deleted rows (not the live ones), and
/// that the default (no `view`) list keeps excluding them.
#[tokio::test]
async fn manage_products_deleted_view_lists_only_deleted_products() {
    let ctx = ctx().await;

    let mut live = HashMap::new();
    live.insert("name".to_string(), serde_json::json!("live"));
    live.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "live", live).await;

    let mut gone = HashMap::new();
    gone.insert("name".to_string(), serde_json::json!("gone"));
    gone.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "gone", gone).await;
    soft_delete_product(&ctx, "gone").await;

    let (default_msg, _input) = admin_get_msg("/b/products/admin/manage");
    let default_html =
        output_to_html(super::super::pages::manage_products(&ctx, &default_msg).await).await;
    assert!(default_html.contains(">live<"), "{default_html}");
    assert!(!default_html.contains(">gone<"), "{default_html}");

    let (mut deleted_msg, _input) = admin_get_msg("/b/products/admin/manage");
    deleted_msg.set_meta("req.query.view", "deleted");
    let deleted_html =
        output_to_html(super::super::pages::manage_products(&ctx, &deleted_msg).await).await;
    assert!(deleted_html.contains(">gone<"), "{deleted_html}");
    assert!(!deleted_html.contains(">live<"), "{deleted_html}");
}

/// The deleted view's table has a `Deleted` column, so it has to be ordered
/// by it. Sorting the deleted list `created_at desc` (the live list's order)
/// puts the product an admin just deleted wherever its creation date happens
/// to fall — which for an old product is the bottom of the list, on the one
/// page whose entire purpose is undoing a delete that was probably a moment
/// ago.
#[tokio::test]
async fn manage_products_deleted_view_sorts_by_when_the_product_was_deleted() {
    let ctx = ctx().await;

    // Created most recently, deleted longest ago: first by `created_at
    // desc`, last by `deleted_at desc`.
    let mut stale = HashMap::new();
    stale.insert("name".to_string(), serde_json::json!("Deleted long ago"));
    stale.insert("status".to_string(), serde_json::json!("active"));
    stale.insert(
        "created_at".to_string(),
        serde_json::json!("2026-02-01T00:00:00Z"),
    );
    stale.insert(
        "deleted_at".to_string(),
        serde_json::json!("2026-03-01T00:00:00Z"),
    );
    seed(&ctx, "impresspress__products__products", "stale", stale).await;

    let mut fresh = HashMap::new();
    fresh.insert("name".to_string(), serde_json::json!("Deleted just now"));
    fresh.insert("status".to_string(), serde_json::json!("active"));
    fresh.insert(
        "created_at".to_string(),
        serde_json::json!("2026-01-01T00:00:00Z"),
    );
    fresh.insert(
        "deleted_at".to_string(),
        serde_json::json!("2026-03-02T00:00:00Z"),
    );
    seed(&ctx, "impresspress__products__products", "fresh", fresh).await;

    let (mut msg, _input) = admin_get_msg("/b/products/admin/manage");
    msg.set_meta("req.query.view", "deleted");
    let html = output_to_html(super::super::pages::manage_products(&ctx, &msg).await).await;

    let fresh_at = html
        .find("Deleted just now")
        .expect("the recently deleted product must render");
    let stale_at = html
        .find("Deleted long ago")
        .expect("the older deletion must render");
    assert!(
        fresh_at < stale_at,
        "the deleted view must be ordered by deleted_at desc, so the product just deleted \
         is at the top; got 'Deleted long ago' at {stale_at} before 'Deleted just now' at {fresh_at}"
    );
}

/// The deleted view is a dead end unless the way back is obvious: pin that
/// each deleted row's Restore action posts to the actual restore endpoint,
/// not the edit page (which 404s for a soft-deleted product until it is
/// restored).
#[tokio::test]
async fn manage_products_deleted_view_offers_restore_not_an_edit_link() {
    let ctx = ctx().await;

    let mut gone = HashMap::new();
    gone.insert("name".to_string(), serde_json::json!("gone"));
    gone.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "gone", gone).await;
    soft_delete_product(&ctx, "gone").await;

    let (mut msg, _input) = admin_get_msg("/b/products/admin/manage");
    msg.set_meta("req.query.view", "deleted");
    let html = output_to_html(super::super::pages::manage_products(&ctx, &msg).await).await;

    assert!(
        html.contains("/b/products/api/admin/products/gone/restore"),
        "deleted row should offer a Restore action wired to the restore endpoint: {html}"
    );
    assert!(
        !html.contains(r#"href="/b/products/admin/products/gone""#),
        "a deleted row must not link into the edit page, which refuses a soft-deleted product: {html}"
    );
}

// The Restore button's URL is built server-side by interpolating the row's id
// into a path segment, so the id has to be percent-encoded — every
// browser-side URL in this file already runs its ids through
// `encodeURIComponent`, and this one had nothing. maud escapes HTML, not URLs.
//
// Ids containing `/`, `?` or `#` are reachable, not hypothetical: `db::create`
// synthesizes a UUID only when the body omits `id`, and the admin create
// endpoint forwarded the body verbatim until this branch began refusing `id`,
// so any deployment that ever ran a seeding client against it can hold such a
// row.
//
// Encoding alone is only half a round trip, and the half that is easy to
// assert. The test below therefore asserts the WHOLE trip — render the URL,
// send it through the real router, and check the product came back — because
// an "the HTML contains %2F" assertion passes just as happily when the
// endpoint on the other end 404s.

/// The URL the Deleted view renders for `id` is the URL that restores `id`.
///
/// Drives the real path: `manage_products` renders the row, the `hx-post`
/// target is read back out of that HTML verbatim (no hand-built URL, so a
/// change to either half of the encoding is caught), and it goes through
/// `route_to_block` → `ProductsBlock::handle` → `endpoint_match::dispatch`
/// → `handle_restore_product` → the repository.
///
/// The id carries `/`, `?` and `#`, which end a path segment for three
/// different reasons: path separator, query start, fragment start. Every HTTP
/// adapter hands impresspress the path still percent-encoded — axum's
/// `Uri::path`, `url::Url::path` on Cloudflare, `Url.pathname` in the Service
/// Worker — so `%2F` reaches the matcher intact and the route does not split;
/// the decode owed is on the matched `{id}`, and that is what
/// `dispatch` does.
#[tokio::test]
async fn the_restore_url_the_deleted_view_renders_restores_that_product() {
    let ctx = ctx().await;

    let awkward_id = "prod/1?x#y";

    let mut gone = HashMap::new();
    gone.insert("name".to_string(), serde_json::json!("gone"));
    gone.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", awkward_id, gone).await;
    soft_delete_product(&ctx, awkward_id).await;

    let (mut page_msg, _input) = admin_get_msg("/b/products/admin/manage");
    page_msg.set_meta("req.query.view", "deleted");
    let html = output_to_html(super::super::pages::manage_products(&ctx, &page_msg).await).await;

    let restore_url = rendered_hx_post(&html)
        .unwrap_or_else(|| panic!("the deleted row must render an hx-post Restore action: {html}"));
    assert_eq!(
        restore_url, "/b/products/api/admin/products/prod%2F1%3Fx%23y/restore",
        "the Restore action must percent-encode the product id into its path segment"
    );

    // The wire path, unmodified, through the real router.
    let (msg, input) = admin_create_msg(&restore_url, serde_json::json!({}));
    let out = dispatch_routed(&ctx, msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(
        body["id"], awkward_id,
        "posting the rendered Restore URL must reach the product it names"
    );

    // And the product is actually live again, not merely answered about.
    let restored = super::super::repo::products::get(&ctx, awkward_id)
        .await
        .expect("the restored product must be readable through the live-only door");
    assert!(
        crate::util::RecordExt::str_field(&restored, "deleted_at").is_empty(),
        "restore must clear deleted_at: {restored:?}"
    );
}

/// The value of the first `hx-post="..."` attribute in `html`.
fn rendered_hx_post(html: &str) -> Option<String> {
    let start = html.find("hx-post=\"")? + "hx-post=\"".len();
    let rest = &html[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// The Restore button reloads the page on success, and does NOT carry its own
/// failure branch.
///
/// A refused restore — a slug collision is reachable, see
/// `restore_reports_a_slug_conflict_instead_of_an_opaque_error` — must not
/// render as nothing happening, on the only door out of soft delete.
/// `ui/assets/chrome.js` carries a global `htmx:responseError` listener that
/// toasts every refused htmx request on every shelled page
/// (`ui/assets/test/chrome_error_toast.test.mjs` is its test), so a copy here
/// would toast the same refusal twice. What is pinned here is the half that is
/// this page's: `data-reload-on-success`, which the same file's section 5
/// applies only to a successful request, so a refusal does not reload the
/// page and look like it worked. It is not an `hx-on` handler: those need
/// `'unsafe-eval'`, which the served policy never grants.
#[tokio::test]
async fn manage_products_deleted_view_reports_a_failed_restore() {
    let ctx = ctx().await;

    let mut gone = HashMap::new();
    gone.insert("name".to_string(), serde_json::json!("gone"));
    gone.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", "gone", gone).await;
    soft_delete_product(&ctx, "gone").await;

    let (mut msg, _input) = admin_get_msg("/b/products/admin/manage");
    msg.set_meta("req.query.view", "deleted");
    let html = output_to_html(super::super::pages::manage_products(&ctx, &msg).await).await;

    assert!(
        html.contains("data-reload-on-success"),
        "the restore button must reload only on success: {html}"
    );
    // Assembled, because `ui::tests::pages_carry_no_htmx_eval_attributes`
    // scans this file and a literal would count as one.
    let htmx_handler = format!("hx-{}", "on");
    assert!(
        !html.contains(&htmx_handler),
        "an htmx handler attribute never runs under the served CSP: {html}"
    );
    assert!(
        !html.contains("new CustomEvent('showToast'"),
        "the per-button toast is the global htmx error listeners' job now; a \
         second copy would toast one refusal twice: {html}"
    );
    assert!(
        html.contains(r#"data-error-label="Could not restore this product""#),
        "but the button must still name ITSELF, or a refusal with no message \
         of its own says only `Request failed (502)`: {html}"
    );
}

// ============================================================
// The close-only manager for a deleted product
// ============================================================

/// Seed a soft-deleted product with one published offer and one Payment Link
/// that never reached Stripe, and return the offer and link ids.
///
/// The link is deliberately pending: deactivating it is then a purely local
/// write, so these tests need no Stripe stand-in for the thing they are
/// actually about — whether the UI can reach the close operations at all.
async fn seed_a_deleted_product_with_a_money_surface(
    ctx: &crate::test_support::TestContext,
    product_id: &str,
) -> (String, String) {
    use super::super::{
        contracts::PricingPreviewRequest,
        offer_pricing,
        repo::{offers as offer_repo, products},
    };

    let offer_id = seed_published_offer(ctx, product_id).await;
    let managed = offer_repo::get_managed(ctx, &offer_id)
        .await
        .expect("the offer is readable");
    let preview = offer_pricing::evaluate_offer(
        &managed.offer,
        &PricingPreviewRequest {
            offer_id: offer_id.clone(),
            quantity: 1,
            inputs: Default::default(),
        },
        offer_pricing::InputScope::Management,
    )
    .expect("price the offer");
    let link_id = seed_pending_payment_link(ctx, &offer_id, "close-me", &preview)
        .await
        .managed
        .id;

    products::soft_delete(ctx, product_id)
        .await
        .expect("soft delete");
    (offer_id, link_id)
}

/// The `ProductState::LiveOrDeleted` widening exists so a deleted product's
/// Stripe surface can be closed without restoring the listing, but the
/// Deleted view offered only **Restore** — which puts an active, approved
/// product straight back into the public catalog. The one affordance on
/// offer was the dangerous one, and the safe one was reachable only by an
/// operator who already knew every id.
#[tokio::test]
async fn deleted_view_links_to_the_close_only_manager() {
    let ctx = ctx().await;

    // An id that is not URL-safe, so the link is pinned to carry the same
    // percent-encoding the Restore action does.
    let awkward_id = "prod/1?x#y";
    let mut gone = HashMap::new();
    gone.insert("name".to_string(), serde_json::json!("gone"));
    gone.insert("status".to_string(), serde_json::json!("active"));
    seed(&ctx, "impresspress__products__products", awkward_id, gone).await;
    soft_delete_product(&ctx, awkward_id).await;

    let (mut msg, _input) = admin_get_msg("/b/products/admin/manage");
    msg.set_meta("req.query.view", "deleted");
    let html = output_to_html(super::super::pages::manage_products(&ctx, &msg).await).await;

    let close_url = "/b/products/admin/products/prod%2F1%3Fx%23y/close";
    assert!(
        html.contains(close_url),
        "a deleted row must offer a way to close its Stripe surface, not only Restore: {html}"
    );

    // And the link resolves: the SSR page dispatch decodes its id segment the
    // same way `endpoint_match` decodes an API one, so the encoding the row
    // applies has an inverse on the other end.
    let (msg, input) = admin_get_msg(close_url);
    let page = output_to_html(dispatch_routed(&ctx, msg, input).await).await;
    assert!(
        page.contains("Nothing left to close"),
        "the rendered close link must resolve to that product's close manager: {page}"
    );
}

/// The whole point of the page, end to end: the action it renders for a
/// deleted product's offer actually archives that offer — the first
/// `ProductState::LiveOrDeleted` operation any UI has ever reached.
///
/// The URL is read back out of the rendered HTML rather than hand-built, and
/// the page itself is fetched through the real router, so both halves — the
/// SSR route and the API the button targets — are the shipped ones.
#[tokio::test]
async fn the_close_manager_archives_a_deleted_products_offer_from_its_own_rendered_action() {
    let ctx = ctx().await;
    let (offer_id, _link_id) = seed_a_deleted_product_with_a_money_surface(&ctx, "gone").await;

    let (msg, input) = admin_get_msg("/b/products/admin/products/gone/close");
    let html = output_to_html(dispatch_routed(&ctx, msg, input).await).await;

    let archive_url = rendered_attr(&html, "hx-delete=\"", &offer_id).unwrap_or_else(|| {
        panic!("the close manager must render an archive action for offer {offer_id}: {html}")
    });
    let (mut msg, input) = delete_msg(&archive_url, "admin_1");
    msg.set_meta("auth.user_roles", "admin");
    let archived = output_to_json(dispatch_routed(&ctx, msg, input).await).await;
    assert_eq!(
        archived["status"], "archived",
        "the rendered archive action must archive the deleted product's offer: {archived}"
    );

    // And it did not resurrect the product.
    assert!(
        is_soft_deleted(&ctx, "gone").await,
        "closing a money surface must not restore the listing"
    );
}

/// The same for a Payment Link, which is the half that is actually still
/// taking money.
#[tokio::test]
async fn the_close_manager_deactivates_a_deleted_products_payment_link() {
    let ctx = ctx().await;
    let (_offer_id, link_id) = seed_a_deleted_product_with_a_money_surface(&ctx, "gone").await;

    let (msg, input) = admin_get_msg("/b/products/admin/products/gone/close");
    let html = output_to_html(dispatch_routed(&ctx, msg, input).await).await;

    let deactivate_url = rendered_attr(&html, "hx-delete=\"", &link_id).unwrap_or_else(|| {
        panic!("the close manager must render a deactivate action for link {link_id}: {html}")
    });
    let (mut msg, input) = delete_msg(&deactivate_url, "admin_1");
    msg.set_meta("auth.user_roles", "admin");
    let deactivated = output_to_json(dispatch_routed(&ctx, msg, input).await).await;
    assert_eq!(
        deactivated["active"],
        serde_json::json!(false),
        "the rendered deactivate action must take the link down: {deactivated}"
    );
}

/// Close-only means close-only. A deleted product's manager must not carry
/// anything that keeps it selling — no create, publish, sync, duplicate or
/// storefront link — because every one of those is an operation the
/// `ProductState::Live` gate below it would refuse anyway, and offering it
/// on a page about shutting a product down is an invitation to do the
/// opposite.
#[tokio::test]
async fn the_close_manager_offers_nothing_that_keeps_selling() {
    let ctx = ctx().await;
    seed_a_deleted_product_with_a_money_surface(&ctx, "gone").await;

    let (msg, input) = admin_get_msg("/b/products/admin/products/gone/close");
    let html = output_to_html(dispatch_routed(&ctx, msg, input).await).await;

    // Positive control first, so the refusals below cannot pass on an empty
    // page.
    assert!(
        html.contains("Checkout product") && html.contains("hx-delete="),
        "the page must render the product and its closing actions: {html}"
    );

    // Each name is markup the FULL manager renders. They used to be the
    // JavaScript handler names, which were only ever in this page's HTML
    // because the manager bundle was inlined into it — two of the five had
    // already ceased to exist anywhere in the tree, and the bundle is an
    // external file now, so the whole list asserted nothing. `data-action`
    // verbs and the bundle's own URL stem are what actually distinguish the
    // two pages.
    for forbidden in [
        "data-offer-op=\"publish\"",
        "data-offer-op=\"sync\"",
        "data-action=\"pm-duplicate\"",
        "data-action=\"pm-create-link\"",
        "data-action=\"pm-set-status\"",
        "products-manager",
        "product-manager-form",
        "/b/products/catalog/gone",
    ] {
        assert!(
            !html.contains(forbidden),
            "the close-only manager must not offer `{forbidden}`: {html}"
        );
    }
    // POST and PATCH open or change a money surface; only DELETE closes one.
    assert!(
        !html.contains("hx-post=") && !html.contains("hx-patch="),
        "the close-only manager must only ever issue closing requests: {html}"
    );
}

/// A live product belongs in the ordinary manager, which can do everything.
/// This page reads through `get_deleted`, so it simply does not exist for one.
#[tokio::test]
async fn the_close_manager_404s_for_a_live_product() {
    let ctx = ctx().await;
    seed_published_offer(&ctx, "alive").await;

    let (msg, input) = admin_get_msg("/b/products/admin/products/alive/close");
    assert!(
        output_is_error(dispatch_routed(&ctx, msg, input).await, ErrorCode::NotFound).await,
        "the close-only manager is for deleted products"
    );

    // Positive control: the same path for a DELETED product renders, so the
    // 404 above is about the product's state and not about the route.
    seed_a_deleted_product_with_a_money_surface(&ctx, "gone").await;
    let (msg, input) = admin_get_msg("/b/products/admin/products/gone/close");
    let html = output_to_html(dispatch_routed(&ctx, msg, input).await).await;
    assert!(
        html.contains("hx-delete="),
        "the route itself must exist: {html}"
    );
}

/// `/b/products` is a PUBLIC route prefix — every admin page under it is
/// admin-only solely because its `BlockEndpoint` declares
/// `AuthLevel::Admin`. A new page path with no declaration is therefore
/// reachable by anyone, so this drives the real router the way
/// `restore_is_unreachable_for_a_non_admin_on_every_path_that_reaches_it`
/// does.
#[tokio::test]
async fn the_close_manager_is_admin_only() {
    let ctx = ctx().await;
    seed_a_deleted_product_with_a_money_surface(&ctx, "gone").await;

    let (msg, input) = get_msg("/b/products/admin/products/gone/close", "user_1");
    assert!(
        dispatch_routed(&ctx, msg, input)
            .await
            .collect_buffered()
            .await
            .is_err(),
        "a non-admin must not reach the close-only manager"
    );

    // Positive control: the admin does.
    let (msg, input) = admin_get_msg("/b/products/admin/products/gone/close");
    let html = output_to_html(dispatch_routed(&ctx, msg, input).await).await;
    assert!(
        html.contains("hx-delete="),
        "an admin must reach the page the assertion above denies: {html}"
    );
}

/// The value of the first `attr` whose element also mentions `needle`.
///
/// Attribute values are unique enough here that a plain scan for the id and a
/// backwards walk to the attribute would be fragile; this instead takes every
/// value of `attr` and returns the one containing `needle`.
fn rendered_attr(html: &str, attr: &str, needle: &str) -> Option<String> {
    let mut rest = html;
    while let Some(start) = rest.find(attr) {
        rest = &rest[start + attr.len()..];
        let end = rest.find('"')?;
        let value = &rest[..end];
        if value.contains(needle) {
            return Some(value.to_string());
        }
        rest = &rest[end..];
    }
    None
}

#[tokio::test]
async fn stripe_setup_guides_configuration_without_rendering_credentials() {
    let ctx = ctx_with(&[
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY",
            "pk_test_must_never_render",
        ),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
            "whsec_must_never_render",
        ),
    ])
    .await;
    let (msg, _input) = admin_get_msg("/b/products/admin/stripe");
    let html = output_to_html(super::super::pages::stripe_setup(&ctx, &msg).await).await;

    assert!(html.contains("Stripe setup"));
    assert!(html.contains("Not configured"));
    assert!(html.contains("Go-live checklist"));
    assert!(html.contains("/b/products/webhooks"));
    assert!(html.contains("checkout.session.completed"));
    assert!(html.contains("checkout.session.async_payment_succeeded"));
    assert!(html.contains("checkout.session.async_payment_failed"));
    assert!(html.contains("payment_intent.succeeded"));
    assert!(html.contains("payment_intent.payment_failed"));
    assert!(html.contains("payment_intent.processing"));
    assert!(html.contains("payment_intent.requires_action"));
    assert!(html.contains("payment_intent.canceled"));
    assert!(html.contains("Test connection"));
    assert!(html.contains("Webhook delivery health"));
    assert!(html.contains("Needs manual review"));
    assert!(html.contains("Provider reconciliation"));
    assert!(html.contains("Reconcile due operations"));
    // The page renders empty containers and a Reconcile button; the two
    // administration endpoints they are filled from, and the replay and
    // reconcile calls, are the bundle's.
    #[cfg(feature = "embed-assets")]
    {
        let js = loaded_bundle(&html, "products-stripe-setup.js");
        assert!(js.contains("/b/products/api/admin/webhook-events"));
        assert!(js.contains("replayStripeWebhookEvent"));
        assert!(js.contains("/b/products/api/admin/provider-operations"));
        assert!(js.contains("reconcileStripeProviderOperations"));
    }
    assert!(!html.contains("pk_test_must_never_render"));
    assert!(!html.contains("whsec_must_never_render"));
    assert!(html.contains(r#"href="/b/products/admin/stripe""#));
    assert!(html.contains("class=\"tab active\""));
}

#[tokio::test]
async fn commerce_home_keeps_buyer_actions_and_hides_seller_ui_when_disabled() {
    let ctx = ctx().await;
    let (msg, _input) = get_msg("/b/products/", "buyer_1");
    let html = output_to_html(super::super::pages::portal_home(&ctx, &msg).await).await;

    assert!(html.contains("Purchases and subscriptions"));
    assert!(html.contains("View purchases"));
    assert!(html.contains("Manage billing"));
    assert!(html.contains(r#"href="/b/products/my-purchases""#));
    assert!(!html.contains(r#"href="/b/products/my-products""#));
    assert!(!html.contains("Stripe seller account"));
    assert!(!html.contains("Connect Stripe to sell"));
    assert!(!html.contains("Products for sale"));
}

#[tokio::test]
async fn commerce_home_renders_seller_requirements_and_actions_when_enabled() {
    let ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        ("IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS", "250"),
    ])
    .await;
    seed(
        &ctx,
        super::super::repo::seller_accounts::TABLE,
        "seller_account_1",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("seller_1")),
            ("status".to_string(), serde_json::json!("onboarding")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_test_seller_1"),
            ),
            ("details_submitted".to_string(), serde_json::json!(false)),
            ("charges_enabled".to_string(), serde_json::json!(false)),
            ("payouts_enabled".to_string(), serde_json::json!(false)),
            (
                "requirements_json".to_string(),
                serde_json::json!(r#"{"currently_due":["individual.verification.document"]}"#),
            ),
            ("fee_basis_points".to_string(), serde_json::json!(250)),
            ("dashboard_type".to_string(), serde_json::json!("express")),
        ]),
    )
    .await;

    let (msg, _input) = get_msg("/b/products/", "seller_1");
    let html = output_to_html(super::super::pages::portal_home(&ctx, &msg).await).await;
    assert!(html.contains(r#"href="/b/products/my-products""#));
    assert!(html.contains("Products for sale"));
    assert!(html.contains("Stripe seller account"));
    assert!(html.contains("Information Stripe still needs"));
    assert!(html.contains("individual › verification › document"));
    assert!(html.contains("Continue Stripe setup"));
    assert!(html.contains("Open Stripe dashboard"));
    assert!(html.contains("2.50%"));
}

#[tokio::test]
async fn seller_page_is_forbidden_when_user_selling_is_disabled() {
    use wafer_run::Block;

    let ctx = ctx().await;
    for path in [
        "/b/products/my-products",
        "/b/products/my-products/new",
        "/b/products/my-products/product_1",
        "/b/products/selling",
        "/b/products/selling/orders",
        "/b/products/selling/orders/order_1",
    ] {
        let (msg, input) = get_msg(path, "seller_1");
        let out = super::super::ProductsBlock::new()
            .handle(&ctx, msg, input)
            .await;
        assert!(
            output_is_error(out, ErrorCode::PermissionDenied).await,
            "{path} must be rejected while user selling is disabled"
        );
    }
}

#[test]
fn commerce_ssr_routes_declare_their_auth_tiers() {
    use wafer_run::{AuthLevel, Block};

    let info = super::super::ProductsBlock::new().info();
    for path in [
        "/b/products",
        "/b/products/",
        "/b/products/my-products",
        "/b/products/my-products/new",
        "/b/products/my-products/product_1",
        "/b/products/my-purchases",
        "/b/products/my-purchases/order_1",
        "/b/products/selling",
        "/b/products/selling/orders",
        "/b/products/selling/orders/order_1",
    ] {
        assert_eq!(
            crate::endpoint_match::endpoint_auth(&info.endpoints, "retrieve", path),
            Some(AuthLevel::Authenticated),
            "{path} must require an authenticated user"
        );
    }
    assert_eq!(
        crate::endpoint_match::endpoint_auth(
            &info.endpoints,
            "retrieve",
            "/b/products/admin/stripe"
        ),
        Some(AuthLevel::Admin)
    );
    assert_eq!(
        crate::endpoint_match::endpoint_auth(&info.endpoints, "retrieve", "/b/products/admin/new"),
        Some(AuthLevel::Admin)
    );
    assert_eq!(
        crate::endpoint_match::endpoint_auth(
            &info.endpoints,
            "retrieve",
            "/b/products/admin/products/product_1"
        ),
        Some(AuthLevel::Admin)
    );
    for (action, path) in [
        ("retrieve", "/b/products/api/products"),
        ("create", "/b/products/api/products"),
        ("update", "/b/products/api/products/product_1"),
        ("delete", "/b/products/api/products/product_1"),
        ("create", "/b/products/api/products/product_1/duplicate"),
    ] {
        assert_eq!(
            crate::endpoint_match::endpoint_auth(&info.endpoints, action, path),
            Some(AuthLevel::Authenticated),
            "{action} {path} must require seller authentication"
        );
    }
    assert_eq!(
        crate::endpoint_match::endpoint_auth(
            &info.endpoints,
            "create",
            "/b/products/api/admin/products/product_1/duplicate"
        ),
        Some(AuthLevel::Admin)
    );
    for (action, path) in [
        ("retrieve", "/b/products/api/admin/webhook-events"),
        (
            "create",
            "/b/products/api/admin/webhook-events/evt_1/replay",
        ),
    ] {
        assert_eq!(
            crate::endpoint_match::endpoint_auth(&info.endpoints, action, path),
            Some(AuthLevel::Admin),
            "{action} {path} must require an administrator"
        );
    }
}

#[tokio::test]
async fn admin_can_inspect_and_replay_dead_letter_webhooks_without_payload_disclosure() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        "whsec_route_replay",
    )])
    .await;
    let payload = r#"{"id":"evt_route_replay","type":"charge.refunded","livemode":false,"data":{"object":{"payment_intent":"pi_route_private","livemode":false}}}"#;
    seed(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_route_replay",
        HashMap::from([
            (
                "event_type".to_string(),
                serde_json::json!("charge.refunded"),
            ),
            ("status".to_string(), serde_json::json!("dead_letter")),
            ("attempts".to_string(), serde_json::json!(8)),
            (
                "processing_owner".to_string(),
                serde_json::json!("private-owner-token"),
            ),
            (
                "payload_base64".to_string(),
                serde_json::json!(Base64::encode_string(payload.as_bytes())),
            ),
            (
                "payload_sha256".to_string(),
                serde_json::json!(crate::util::sha256_hex(payload.as_bytes())),
            ),
            (
                "last_error".to_string(),
                serde_json::json!("temporary reconciliation failure"),
            ),
        ]),
    )
    .await;

    let (list, list_input) = admin_get_msg("/b/products/api/admin/webhook-events");
    let body = output_to_json(dispatch(&ctx, list, list_input).await).await;
    assert_eq!(body["total_count"], 1);
    assert_eq!(body["records"][0]["id"], "evt_route_replay");
    let encoded = serde_json::to_string(&body).unwrap();
    assert!(!encoded.contains("pi_route_private"));
    assert!(!encoded.contains("private-owner-token"));
    assert!(!encoded.contains("payload_base64"));
    assert!(!encoded.contains("payload_sha256"));

    let (replay, replay_input) = admin_create_msg(
        "/b/products/api/admin/webhook-events/evt_route_replay/replay",
        serde_json::json!({}),
    );
    let replayed = output_to_json(dispatch(&ctx, replay, replay_input).await).await;
    assert_eq!(replayed["received"], true);

    let event = wafer_core::clients::database::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_route_replay",
    )
    .await
    .expect("replayed event");
    assert_eq!(
        crate::util::RecordExt::str_field(&event, "status"),
        "processed"
    );
}

#[tokio::test]
async fn order_pages_use_exact_currency_and_enforce_buyer_seller_actions() {
    let ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    seed(
        &ctx,
        super::super::repo::seller_accounts::TABLE,
        "seller_page_account",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("seller_page_user")),
            ("status".to_string(), serde_json::json!("active")),
        ]),
    )
    .await;
    seed(
        &ctx,
        super::super::repo::seller_accounts::TABLE,
        "seller_other_account",
        HashMap::from([
            (
                "user_id".to_string(),
                serde_json::json!("seller_other_user"),
            ),
            ("status".to_string(), serde_json::json!("active")),
        ]),
    )
    .await;
    seed(
        &ctx,
        "impresspress__products__purchases",
        "order_page_jpy",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_page_user")),
            (
                "buyer_user_id".to_string(),
                serde_json::json!("buyer_page_user"),
            ),
            (
                "buyer_email".to_string(),
                serde_json::json!("buyer@example.com"),
            ),
            (
                "seller_account_id".to_string(),
                serde_json::json!("seller_page_account"),
            ),
            ("status".to_string(), serde_json::json!("completed")),
            ("currency".to_string(), serde_json::json!("JPY")),
            ("subtotal_cents".to_string(), serde_json::json!(1234)),
            ("total_cents".to_string(), serde_json::json!(1234)),
            ("provider".to_string(), serde_json::json!("stripe")),
            (
                "stripe_customer_id".to_string(),
                serde_json::json!("cus_page_buyer"),
            ),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_page_buyer"),
            ),
            (
                "subscription_status".to_string(),
                serde_json::json!("active"),
            ),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("reconciled"),
            ),
            (
                "provider_payment_status".to_string(),
                serde_json::json!("succeeded"),
            ),
            (
                "provider_payment_intent_id".to_string(),
                serde_json::json!("pi_page_order"),
            ),
            (
                "stripe_payment_intent_id".to_string(),
                serde_json::json!("pi_page_order"),
            ),
        ]),
    )
    .await;
    seed(
        &ctx,
        "impresspress__products__line_items",
        "order_page_line",
        HashMap::from([
            (
                "purchase_id".to_string(),
                serde_json::json!("order_page_jpy"),
            ),
            ("product_id".to_string(), serde_json::json!("tokyo_pass")),
            ("product_name".to_string(), serde_json::json!("Tokyo pass")),
            ("quantity".to_string(), serde_json::json!(1)),
            ("unit_amount_minor".to_string(), serde_json::json!(1234)),
            ("total_minor".to_string(), serde_json::json!(1234)),
        ]),
    )
    .await;
    seed(
        &ctx,
        super::super::repo::disputes::TABLE,
        "order_page_dispute",
        HashMap::from([
            (
                "purchase_id".to_string(),
                serde_json::json!("order_page_jpy"),
            ),
            (
                "seller_account_id".to_string(),
                serde_json::json!("seller_page_account"),
            ),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_page_seller"),
            ),
            (
                "provider_dispute_id".to_string(),
                serde_json::json!("dp_page_order"),
            ),
            (
                "payment_intent_id".to_string(),
                serde_json::json!("pi_page_order"),
            ),
            ("status".to_string(), serde_json::json!("needs_response")),
            ("amount_minor".to_string(), serde_json::json!(500)),
            ("currency".to_string(), serde_json::json!("JPY")),
            ("reason".to_string(), serde_json::json!("fraudulent")),
            (
                "evidence_due_by".to_string(),
                serde_json::json!("2033-05-18T03:33:20+00:00"),
            ),
        ]),
    )
    .await;

    let (buyer_msg, _) = get_msg("/b/products/my-purchases/order_page_jpy", "buyer_page_user");
    let buyer_html = output_to_html(
        super::super::pages::my_purchase_detail(&ctx, &buyer_msg, "order_page_jpy").await,
    )
    .await;
    assert!(buyer_html.contains("1234 JPY"));
    assert!(!buyer_html.contains("12.34 JPY"));
    assert!(buyer_html.contains("Tokyo pass"));
    assert!(buyer_html.contains("Manage subscription and billing"));
    assert!(buyer_html.contains("Payment state:"));
    assert!(buyer_html.contains("pi_page_order"));
    assert!(buyer_html.contains("Payment disputes"));
    assert!(buyer_html.contains("500 JPY"));
    assert!(!buyer_html.contains(r#"id="order-refund-amount""#));

    let (admin_msg, _) = admin_get_msg("/b/products/admin/purchases/order_page_jpy");
    let admin_html = output_to_html(
        super::super::pages::admin_purchase_detail(&ctx, &admin_msg, "order_page_jpy").await,
    )
    .await;
    assert!(admin_html.contains("Create refund"));
    assert!(admin_html.contains("dp_page_order"));
    assert!(
        admin_html.contains("Evidence, balance impact, and payout actions are managed in Stripe")
    );
    assert!(admin_html.contains(r#"id="order-refund-amount""#));
    assert!(admin_html.contains("/b/products/api/admin/purchases/order_page_jpy/refund"));

    let (seller_msg, _) = get_msg(
        "/b/products/selling/orders/order_page_jpy",
        "seller_page_user",
    );
    let seller_html = output_to_html(
        super::super::pages::seller_order_detail(&ctx, &seller_msg, "order_page_jpy").await,
    )
    .await;
    assert!(seller_html.contains("Create refund"));
    assert!(seller_html.contains("dp_page_order"));
    assert!(seller_html.contains("/b/products/api/seller/orders/order_page_jpy/refund"));

    let (other_seller, _) = get_msg(
        "/b/products/selling/orders/order_page_jpy",
        "seller_other_user",
    );
    assert!(
        output_is_error(
            super::super::pages::seller_order_detail(&ctx, &other_seller, "order_page_jpy").await,
            ErrorCode::PermissionDenied,
        )
        .await
    );
}

/// A line item's `input_snapshot` is a JSON-object column: written by
/// `serde_json::to_string(&preview.inputs)`, declared `TEXT NOT NULL DEFAULT
/// '{}'`. Two of the three database adapters (native SQLite and the browser)
/// re-parse a JSON-shaped TEXT column on read, so the column arrives as a
/// `Value::Object`, and `str_field` — which has no structured arm — answers
/// `""` for it. The order-detail Configuration column then took its
/// "nothing selected" branch and rendered a dash for every configured item,
/// on the buyer, seller and admin views alike.
#[tokio::test]
async fn order_detail_renders_a_line_item_configuration_snapshot() {
    let ctx = ctx().await;
    seed(
        &ctx,
        "impresspress__products__purchases",
        "order_snapshot",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_snapshot")),
            (
                "buyer_user_id".to_string(),
                serde_json::json!("buyer_snapshot"),
            ),
            ("status".to_string(), serde_json::json!("completed")),
            ("currency".to_string(), serde_json::json!("NZD")),
            ("total_cents".to_string(), serde_json::json!(2500)),
            ("subtotal_cents".to_string(), serde_json::json!(2500)),
        ]),
    )
    .await;
    seed(
        &ctx,
        "impresspress__products__line_items",
        "order_snapshot_line",
        HashMap::from([
            (
                "purchase_id".to_string(),
                serde_json::json!("order_snapshot"),
            ),
            ("product_id".to_string(), serde_json::json!("engraved_pen")),
            (
                "product_name".to_string(),
                serde_json::json!("Engraved pen"),
            ),
            ("quantity".to_string(), serde_json::json!(1)),
            ("unit_amount_minor".to_string(), serde_json::json!(2500)),
            ("total_minor".to_string(), serde_json::json!(2500)),
            // Exactly what `stripe.rs` writes: the encoded object, as text.
            (
                "input_snapshot".to_string(),
                serde_json::json!(r#"{"engraving":"For Ada"}"#),
            ),
        ]),
    )
    .await;

    let (buyer_msg, _) = get_msg("/b/products/my-purchases/order_snapshot", "buyer_snapshot");
    let buyer_html = output_to_html(
        super::super::pages::my_purchase_detail(&ctx, &buyer_msg, "order_snapshot").await,
    )
    .await;
    assert!(
        buyer_html.contains("For Ada"),
        "the buyer's Configuration cell must show the selected options: {buyer_html}"
    );

    let (admin_msg, _) = admin_get_msg("/b/products/admin/purchases/order_snapshot");
    let admin_html = output_to_html(
        super::super::pages::admin_purchase_detail(&ctx, &admin_msg, "order_snapshot").await,
    )
    .await;
    assert!(
        admin_html.contains("For Ada"),
        "the seller-side Configuration cell must show the selected options: {admin_html}"
    );
}

#[tokio::test]
async fn seller_dashboard_renders_only_own_currency_stats() {
    let ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    seed(
        &ctx,
        super::super::repo::seller_accounts::TABLE,
        "seller_dashboard_account",
        HashMap::from([
            (
                "user_id".to_string(),
                serde_json::json!("seller_dashboard_user"),
            ),
            ("status".to_string(), serde_json::json!("active")),
        ]),
    )
    .await;
    for (id, seller, total) in [
        ("seller_dashboard_own", "seller_dashboard_account", 4200),
        ("seller_dashboard_other", "another_seller_account", 9900),
    ] {
        seed(
            &ctx,
            "impresspress__products__purchases",
            id,
            HashMap::from([
                ("user_id".to_string(), serde_json::json!("buyer")),
                ("seller_account_id".to_string(), serde_json::json!(seller)),
                ("status".to_string(), serde_json::json!("completed")),
                ("currency".to_string(), serde_json::json!("NZD")),
                ("total_cents".to_string(), serde_json::json!(total)),
            ]),
        )
        .await;
    }
    seed(
        &ctx,
        "impresspress__products__purchases",
        "seller_dashboard_failed_own",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer")),
            (
                "seller_account_id".to_string(),
                serde_json::json!("seller_dashboard_account"),
            ),
            ("status".to_string(), serde_json::json!("failed")),
            ("currency".to_string(), serde_json::json!("NZD")),
            ("total_cents".to_string(), serde_json::json!(1700)),
            (
                "reconciliation_error".to_string(),
                serde_json::json!("Own checkout needs attention"),
            ),
        ]),
    )
    .await;
    seed(
        &ctx,
        "impresspress__products__purchases",
        "seller_dashboard_failed_other",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer")),
            (
                "seller_account_id".to_string(),
                serde_json::json!("another_seller_account"),
            ),
            ("status".to_string(), serde_json::json!("failed")),
            ("currency".to_string(), serde_json::json!("NZD")),
            ("total_cents".to_string(), serde_json::json!(9900)),
            (
                "reconciliation_error".to_string(),
                serde_json::json!("Other seller secret failure"),
            ),
        ]),
    )
    .await;
    let (msg, _) = get_msg("/b/products/selling", "seller_dashboard_user");
    let html = output_to_html(super::super::pages::seller_dashboard(&ctx, &msg).await).await;
    assert!(html.contains("42.00 NZD"));
    assert!(!html.contains("99.00 NZD"));
    assert!(html.contains("Before Stripe fees"));
    assert!(html.contains("before Stripe fees, disputes, reserves, and payout adjustments"));
    assert!(html.contains("Recent payment failures"));
    assert!(html.contains("Own checkout needs attention"));
    assert!(!html.contains("Other seller secret failure"));
    assert!(html.contains(r#"href="/b/products/selling/orders""#));
    assert!(html.contains(r#"href="/b/products/my-products""#));
}

#[test]
fn owned_group_and_taxonomy_routes_are_explicitly_authenticated() {
    use wafer_run::{AuthLevel, Block};

    let info = super::super::ProductsBlock::new().info();
    for (action, path) in [
        ("retrieve", "/b/products/groups"),
        ("create", "/b/products/groups"),
        ("retrieve", "/b/products/groups/group_1"),
        ("update", "/b/products/groups/group_1"),
        ("delete", "/b/products/groups/group_1"),
        ("retrieve", "/b/products/groups/group_1/products"),
        ("retrieve", "/b/products/types"),
        ("retrieve", "/b/products/group-templates"),
    ] {
        assert_eq!(
            crate::endpoint_match::endpoint_auth(&info.endpoints, action, path),
            Some(AuthLevel::Authenticated),
            "{action} {path} must be explicitly declared as authenticated"
        );
    }
}

#[test]
fn every_products_json_endpoint_has_discovery_schema() {
    use wafer_run::Block;

    let info = super::super::ProductsBlock::new().info();
    let missing = info
        .endpoints
        .iter()
        .filter(|endpoint| {
            endpoint.path.starts_with("/b/products/api/")
                || matches!(
                    endpoint.path.as_str(),
                    "/b/products/groups"
                        | "/b/products/groups/{id}"
                        | "/b/products/groups/{id}/products"
                        | "/b/products/types"
                        | "/b/products/group-templates"
                        | "/b/products/catalog"
                        | "/b/products/catalog/{id}"
                        | "/b/products/storefront/config"
                        | "/b/products/storefront/{product_id}"
                        | "/b/products/webhooks"
                        | "/b/products/pricing/preview"
                        | "/b/products/checkout"
                        | "/b/products/orders/{id}/status"
                        | "/b/products/purchases"
                        | "/b/products/purchases/{id}"
                        | "/b/products/subscription"
                        | "/b/products/billing-portal"
                )
        })
        .filter(|endpoint| !endpoint.has_schema())
        .map(|endpoint| format!("{:?} {}", endpoint.method, endpoint.path))
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "products JSON endpoints omitted from discovery:\n{}",
        missing.join("\n")
    );
}

// ============================================================
// Typed row projection
// ============================================================

/// Every product endpoint publishes `contracts::ProductView`, flat, with the
/// exact field set the type declares — not the database layer's
/// `{id, data: {column → value}}` record, and not whatever else the row holds.
#[tokio::test]
async fn product_endpoints_publish_the_flat_product_view() {
    use crate::blocks::products::contracts::ProductView;

    let ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({
            "name": "Flat",
            "tags": ["a", "b"],
            "metadata": {"k": 1},
            "stock": 3,
            "fulfillment_kind": "download"
        }),
    );
    let created = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert!(
        created.get("data").is_none(),
        "rows are flat views, not {{id, data}} records: {created}"
    );
    let view: ProductView =
        serde_json::from_value(created.clone()).expect("create response is a ProductView");
    assert_eq!(view.name, "Flat");
    assert_eq!(view.tags, vec!["a", "b"]);
    assert_eq!(view.metadata.get("k"), Some(&serde_json::json!(1)));
    assert_eq!(view.stock, 3);
    assert_eq!(view.status, ProductStatus::Draft);
    assert_eq!(view.owner_kind, "platform");
    assert_eq!(view.approval_status, ApprovalStatus::Approved);
    assert_eq!(view.created_by, "admin_1");
    assert_eq!(view.fulfillment_kind, "download");
    assert_eq!(view.current_version, 1);
    assert_eq!(view.published_at, None);
    // The response carries exactly the view's keys: nothing the row holds
    // reaches the wire unless the type names it.
    assert_eq!(serde_json::to_value(&view).unwrap(), created);

    let id = view.id.clone();
    let (msg, input) = admin_get_msg(&format!("/b/products/api/admin/products/{id}"));
    let fetched = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(fetched, created);

    let (msg, input) = admin_get_msg("/b/products/api/admin/products");
    let list = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(list["total_count"], 1);
    assert_eq!(list["page"], 1);
    assert_eq!(list["page_size"], 20);
    assert_eq!(list["records"][0], created);

    let (mut msg, input) = request_msg(
        "update",
        &format!("/b/products/api/admin/products/{id}"),
        "admin_1",
        serde_json::json!({"description": "Renamed", "tags": ["c"]}),
    );
    msg.set_meta("auth.user_roles", "admin");
    let updated = output_to_json(dispatch(&ctx, msg, input).await).await;
    let updated_view: ProductView =
        serde_json::from_value(updated.clone()).expect("update response is a ProductView");
    assert_eq!(updated_view.description, "Renamed");
    assert_eq!(updated_view.tags, vec!["c"]);
    assert_eq!(
        updated_view.name, "Flat",
        "untouched columns survive a PATCH"
    );
    assert_eq!(serde_json::to_value(&updated_view).unwrap(), updated);
}

/// The seller create path used to hand every key of the body to the
/// database. The update path stripped the ownership, moderation and provider
/// columns; the create path did not, so a seller could create a product
/// already published, already synchronized to a Stripe product they did not
/// own, or attached to another seller's account. The typed request has no
/// such fields, so nothing a client sends can reach them on either path.
#[tokio::test]
async fn seller_writes_cannot_set_ownership_moderation_or_provider_columns() {
    use super::super::repo::products::TABLE;
    use crate::util::RecordExt;

    let ctx = user_products_ctx().await;
    let smuggled = serde_json::json!({
        "seller_account_id": "acct_other",
        "stripe_product_id": "prod_hijacked",
        "current_version": 9,
        "published_at": "2026-01-01T00:00:00Z",
        "submitted_at": "2026-01-01T00:00:00Z",
        "deleted_at": "2026-01-01T00:00:00Z",
        "owner_kind": "platform",
        "owner_id": "attacker",
        "created_by": "attacker",
        "approval_status": "approved"
    });
    let mut body = smuggled.clone();
    body["name"] = serde_json::json!("Sneaky");
    let (msg, input) = create_msg("/b/products/api/products", "seller_a", body);
    assert!(
        output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await,
        "a create naming these columns is refused outright, not quietly stripped"
    );

    // The same seller creating an honest product gets every one of those
    // columns from the server, which is the other half of the property: the
    // values are not merely unreachable from a request body, they are set.
    let (msg, input) = create_msg(
        "/b/products/api/products",
        "seller_a",
        serde_json::json!({"name": "Honest"}),
    );
    let created = output_to_json(dispatch(&ctx, msg, input).await).await;
    let id = created["id"].as_str().expect("created id").to_string();

    let assert_untouched = |row: &wafer_core::clients::database::Record| {
        assert_eq!(row.str_field("seller_account_id"), "");
        assert_eq!(row.str_field("stripe_product_id"), "");
        assert_eq!(row.i64_field("current_version"), 1);
        assert_eq!(row.str_field("published_at"), "");
        assert_eq!(row.str_field("submitted_at"), "");
        assert_eq!(row.str_field("deleted_at"), "");
        assert_eq!(row.str_field("owner_kind"), "user");
        assert_eq!(row.str_field("owner_id"), "seller_a");
        assert_eq!(row.str_field("created_by"), "seller_a");
        assert_eq!(row.str_field("approval_status"), "draft");
    };
    let row = wafer_core::clients::database::get(&ctx, TABLE, &id)
        .await
        .expect("created row");
    assert_untouched(&row);

    let (msg, input) = update_msg(
        &format!("/b/products/api/products/{id}"),
        "seller_a",
        smuggled,
    );
    assert!(
        output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await,
        "and so is an update naming them"
    );
    let row = wafer_core::clients::database::get(&ctx, TABLE, &id)
        .await
        .expect("updated row");
    assert_untouched(&row);
}

/// A `PATCH` field sent as an explicit `null` is treated as absent: the
/// column keeps its value rather than becoming a `NULL` write against a
/// `NOT NULL` column, and the other fields in the same body still apply.
#[tokio::test]
async fn seller_patch_treats_explicit_null_fields_as_absent() {
    use super::super::repo::products::TABLE;
    use crate::util::RecordExt;

    let ctx = user_products_ctx().await;
    let (msg, input) = create_msg(
        "/b/products/api/products",
        "seller_a",
        serde_json::json!({
            "name": "Kept",
            "description": "Original",
            "category": "tools",
            "stock": 7
        }),
    );
    let created = output_to_json(dispatch(&ctx, msg, input).await).await;
    let id = created["id"].as_str().expect("created id").to_string();
    assert_eq!(created["stock"], 7, "{created}");

    let (msg, input) = update_msg(
        &format!("/b/products/api/products/{id}"),
        "seller_a",
        serde_json::json!({
            "name": "Renamed",
            "description": null,
            "category": null,
            "stock": null
        }),
    );
    let updated = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(updated["name"], "Renamed", "{updated}");
    assert_eq!(updated["description"], "Original", "{updated}");
    assert_eq!(updated["category"], "tools", "{updated}");
    assert_eq!(updated["stock"], 7, "{updated}");
    let row = wafer_core::clients::database::get(&ctx, TABLE, &id)
        .await
        .expect("updated row");
    assert_eq!(row.str_field("description"), "Original");
    assert_eq!(row.str_field("category"), "tools");
    assert_eq!(row.i64_field("stock"), 7);
}

/// A typed request refuses a value outside its contract instead of writing
/// it to the column as whatever JSON arrived.
#[tokio::test]
async fn typed_product_writes_reject_values_outside_the_contract() {
    let ctx = ctx().await;
    for body in [
        serde_json::json!({"status": "draft"}),
        serde_json::json!({"name": "x", "status": "published"}),
        serde_json::json!({"name": "x", "fulfillment_kind": "teleport"}),
        serde_json::json!({"name": "x", "stock": "many"}),
        serde_json::json!({"name": "x", "tags": "not-a-list"}),
    ] {
        let (msg, input) = admin_create_msg("/b/products/api/admin/products", body.clone());
        assert!(
            output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await,
            "{body} must be refused"
        );
    }
}

/// Every group endpoint publishes `contracts::GroupView`, flat, with exactly
/// the type's field set, on both the admin and the owner tier.
#[tokio::test]
async fn group_endpoints_publish_the_flat_group_view() {
    use crate::blocks::products::contracts::GroupView;

    let ctx = user_products_ctx().await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/groups",
        serde_json::json!({"name": "Admin group", "description": "d", "user_id": "someone"}),
    );
    let created = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert!(created.get("data").is_none(), "{created}");
    let view: GroupView = serde_json::from_value(created.clone()).expect("GroupView");
    assert_eq!(view.name, "Admin group");
    assert_eq!(view.user_id, "someone", "an admin may assign the owner");
    assert_eq!(view.status, "active", "the table default is reported");
    assert_eq!(serde_json::to_value(&view).unwrap(), created);

    let (mut msg, input) = request_msg(
        "update",
        &format!("/b/products/api/admin/groups/{}", view.id),
        "admin_1",
        serde_json::json!({"description": "changed"}),
    );
    msg.set_meta("auth.user_roles", "admin");
    let updated = output_to_json(dispatch(&ctx, msg, input).await).await;
    let updated_view: GroupView = serde_json::from_value(updated.clone()).expect("GroupView");
    assert_eq!(updated_view.description, "changed");
    assert_eq!(updated_view.name, "Admin group");

    let (msg, input) = admin_get_msg("/b/products/api/admin/groups");
    let list = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(list["total_count"], 1);
    assert_eq!(list["page_size"], 20);
    assert_eq!(list["records"][0], updated);

    // Owner tier: `user_id` is not a field of the request, so the row keeps
    // the caller as owner however the body tries to name someone else.
    let (msg, input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "Mine", "user_id": "attacker"}),
    );
    let own = output_to_json(dispatch(&ctx, msg, input).await).await;
    let own_view: GroupView = serde_json::from_value(own.clone()).expect("GroupView");
    assert_eq!(own_view.user_id, "user_1");
    assert!(
        !own_view.group_template_id.is_empty(),
        "the seeded default template is applied"
    );
    assert_eq!(serde_json::to_value(&own_view).unwrap(), own);

    let (msg, input) = get_msg(&format!("/b/products/groups/{}", own_view.id), "user_1");
    let fetched = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(fetched, own);

    let (msg, input) = get_msg("/b/products/groups", "user_1");
    let list = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(list["records"][0], own);
    assert_eq!(list["total_count"], 1);

    let (msg, input) = delete_msg(&format!("/b/products/groups/{}", own_view.id), "user_1");
    let deleted = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(deleted, serde_json::json!({"deleted": true}));
}

/// A group records who created it the way a product does: the caller, on
/// both tiers. On the admin tier that is the administrator even when the
/// body assigns the group to someone else as `user_id`; on the owner tier it
/// is the owner. `created_by` is not a request field, so a body naming
/// someone else is ignored rather than honoured.
#[tokio::test]
async fn group_creates_record_the_caller_as_created_by() {
    let ctx = user_products_ctx().await;

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/groups",
        serde_json::json!({"name": "Assigned", "user_id": "someone", "created_by": "attacker"}),
    );
    let created = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(created["user_id"], "someone");
    assert_eq!(
        created["created_by"], "admin_1",
        "the admin tier records the administrator: {created}"
    );

    let (msg, input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "Mine", "created_by": "attacker"}),
    );
    let own = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(own["user_id"], "user_1");
    assert_eq!(
        own["created_by"], "user_1",
        "the owner tier records the owner: {own}"
    );
}

/// The type endpoints publish `contracts::ProductTypeView`, flat, with
/// `is_system` normalized to a boolean whichever way the backend stores it.
#[tokio::test]
async fn type_endpoints_publish_the_flat_type_view() {
    use crate::blocks::products::contracts::{CreateProductTypeRequest, ProductTypeView};

    let ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/types",
        serde_json::json!({"name": "subscription", "description": "Recurring", "is_system": true}),
    );
    let created = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert!(created.get("data").is_none(), "{created}");
    let view: ProductTypeView = serde_json::from_value(created.clone()).expect("ProductTypeView");
    assert_eq!(view.name, "subscription");
    assert_eq!(view.description, "Recurring");
    assert!(view.is_system);
    assert_eq!(serde_json::to_value(&view).unwrap(), created);
    // The INTEGER column is written as `0` / `1`, which Postgres requires.
    // Reading the SQLite row back cannot show that (a bound `true` reads back
    // as `1` there too), so the assertion is on the column map the request
    // encodes, which is what reaches either backend.
    for (is_system, stored) in [(true, 1), (false, 0)] {
        let columns = CreateProductTypeRequest {
            name: "subscription".to_string(),
            description: None,
            is_system: Some(is_system),
        }
        .into_columns();
        assert_eq!(columns["is_system"], serde_json::json!(stored));
    }

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/types",
        serde_json::json!({"name": "plain"}),
    );
    let plain = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(plain["is_system"], false, "the table default is reported");

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/types",
        serde_json::json!({"name": "bad", "is_system": 1}),
    );
    assert!(
        output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await,
        "is_system is a boolean on the wire"
    );

    for (msg, input) in [
        admin_get_msg("/b/products/api/admin/types"),
        get_msg("/b/products/types", "user_1"),
    ] {
        let list = output_to_json(dispatch(&ctx, msg, input).await).await;
        assert_eq!(list["total_count"], 2, "{list}");
        assert_eq!(list["page_size"], 20);
        let names: Vec<&str> = list["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"subscription") && names.contains(&"plain"));
    }

    let (mut msg, input) = delete_msg(
        &format!("/b/products/api/admin/types/{}", view.id),
        "admin_1",
    );
    msg.set_meta("auth.user_roles", "admin");
    let deleted = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(deleted, serde_json::json!({"deleted": true}));
}

/// `GET /b/products/group-templates` was documented as the
/// `{records, total_count, page, page_size}` envelope every other list uses
/// but answered with a bare JSON array of raw records. It now publishes the
/// documented envelope over `contracts::GroupTemplateView` rows.
#[tokio::test]
async fn group_templates_publish_the_documented_list_envelope() {
    use crate::blocks::products::contracts::GroupTemplateView;

    let ctx = ctx().await;
    let (msg, input) = get_msg("/b/products/group-templates", "user_1");
    let list = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert!(list.is_object(), "an envelope, not a bare array: {list}");
    let records = list["records"].as_array().expect("records");
    assert_eq!(list["total_count"], records.len());
    // The descriptions commit to these: `page` is "Always `1`" and
    // `page_size` is "the fixed ceiling on rows returned", the handler's
    // `limit: 1000`.
    assert_eq!(list["page"], 1, "{list}");
    assert_eq!(list["page_size"], 1000, "{list}");
    assert!(
        records.iter().any(|r| r["name"] == "default"),
        "the seeded default template is listed: {list}"
    );
    for record in records {
        let view: GroupTemplateView =
            serde_json::from_value(record.clone()).expect("GroupTemplateView");
        assert_eq!(&serde_json::to_value(&view).unwrap(), record);
    }
}

/// `GET /b/products/subscription` publishes `contracts::SubscriptionView`
/// under `subscription`, or `null` when the user has none. The row's
/// `user_id` and `stripe_customer_id` are never part of it.
#[tokio::test]
async fn subscription_status_publishes_the_typed_projection() {
    use crate::blocks::products::contracts::SubscriptionStatusResponse;

    let ctx = ctx().await;
    seed(
        &ctx,
        super::super::repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_row",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("user_1")),
            (
                "stripe_customer_id".to_string(),
                serde_json::json!("cus_private"),
            ),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_stripe_1"),
            ),
            ("plan".to_string(), serde_json::json!("pro")),
            ("status".to_string(), serde_json::json!("active")),
            ("addon_projects".to_string(), serde_json::json!(2)),
        ]),
    )
    .await;

    let (msg, input) = get_msg("/b/products/subscription", "user_1");
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
    let typed: SubscriptionStatusResponse =
        serde_json::from_value(body.clone()).expect("SubscriptionStatusResponse");
    assert_eq!(serde_json::to_value(&typed).unwrap(), body);
    let subscription = typed.subscription.expect("a subscription");
    assert_eq!(subscription.id, "sub_row");
    assert_eq!(subscription.plan, "pro");
    assert_eq!(
        subscription.status,
        crate::blocks::products::contracts::SubscriptionStatus::Active
    );
    assert_eq!(subscription.stripe_subscription_id, "sub_stripe_1");
    assert_eq!(subscription.addon_projects, 2);
    assert_eq!(subscription.addon_requests, 0, "table default, not absent");
    assert_eq!(subscription.grace_period_end, None);
    let encoded = body.to_string();
    assert!(!encoded.contains("cus_private"), "{body}");
    assert!(!encoded.contains("user_id"), "{body}");

    let (msg, input) = get_msg("/b/products/subscription", "user_2");
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(body, serde_json::json!({"subscription": null}));
    // A row still holding the British `cancelled` that migration 022 rewrites
    // is outside the column's set: the endpoint answers an internal error
    // rather than publishing a second spelling of `canceled`.
    seed(
        &ctx,
        super::super::repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_unmigrated",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("user_3")),
            ("status".to_string(), serde_json::json!("cancelled")),
        ]),
    )
    .await;
    let (msg, input) = get_msg("/b/products/subscription", "user_3");
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::Internal).await);
}

/// The public catalog publishes `contracts::CatalogProductView`: a closed
/// projection that carries none of the ownership, moderation or provider
/// columns the row holds. Checked on the wire, with every withheld column
/// populated on the seeded row so an echo would be visible.
#[tokio::test]
async fn public_catalog_withholds_ownership_moderation_and_provider_columns() {
    use crate::blocks::products::contracts::{CatalogProductListResponse, CatalogProductView};

    let ctx = ctx().await;
    seed(
        &ctx,
        "impresspress__products__products",
        "p_public",
        HashMap::from([
            ("name".to_string(), serde_json::json!("Public")),
            ("status".to_string(), serde_json::json!("active")),
            ("tags".to_string(), serde_json::json!(["a"])),
            ("metadata".to_string(), serde_json::json!({"k": "v"})),
            (
                "created_by".to_string(),
                serde_json::json!("creator_secret"),
            ),
            ("owner_kind".to_string(), serde_json::json!("user")),
            ("owner_id".to_string(), serde_json::json!("owner_secret")),
            (
                "seller_account_id".to_string(),
                serde_json::json!("acct_secret"),
            ),
            ("approval_status".to_string(), serde_json::json!("approved")),
            (
                "stripe_product_id".to_string(),
                serde_json::json!("prod_secret"),
            ),
            ("current_version".to_string(), serde_json::json!(7)),
            (
                "submitted_at".to_string(),
                serde_json::json!("2026-07-01T00:00:00Z"),
            ),
            (
                "published_at".to_string(),
                serde_json::json!("2026-07-02T00:00:00Z"),
            ),
        ]),
    )
    .await;

    let (msg, input) = get_msg("/b/products/catalog/p_public", "");
    let detail = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert!(detail.get("data").is_none(), "{detail}");
    let view: CatalogProductView =
        serde_json::from_value(detail.clone()).expect("CatalogProductView");
    assert_eq!(serde_json::to_value(&view).unwrap(), detail);
    assert_eq!(view.name, "Public");
    assert_eq!(view.tags, vec!["a"]);
    assert_eq!(view.published_at.as_deref(), Some("2026-07-02T00:00:00Z"));

    let (msg, input) = get_msg("/b/products/catalog", "");
    let list = output_to_json(dispatch(&ctx, msg, input).await).await;
    let typed: CatalogProductListResponse =
        serde_json::from_value(list.clone()).expect("CatalogProductListResponse");
    assert_eq!(serde_json::to_value(&typed).unwrap(), list);
    assert_eq!(list["records"][0], detail);
    assert_eq!(list["total_count"], 1);

    for body in [&detail, &list] {
        let encoded = body.to_string();
        for withheld in [
            "created_by",
            "creator_secret",
            "owner_kind",
            "owner_id",
            "owner_secret",
            "seller_account_id",
            "acct_secret",
            "approval_status",
            "stripe_product_id",
            "prod_secret",
            "current_version",
            "submitted_at",
            "deleted_at",
        ] {
            assert!(
                !encoded.contains(withheld),
                "public catalog leaked {withheld}: {body}"
            );
        }
    }
}

/// Moderation writes `published_at = ""` when it returns a product to draft.
/// The view declares that column as a nullable `date-time`, so the empty
/// string must read as `null` rather than as a string that is not a date.
#[tokio::test]
async fn empty_timestamp_columns_read_as_null() {
    let ctx = ctx().await;
    seed(
        &ctx,
        "impresspress__products__products",
        "p_blank_ts",
        HashMap::from([
            ("name".to_string(), serde_json::json!("Blank")),
            ("status".to_string(), serde_json::json!("draft")),
            ("published_at".to_string(), serde_json::json!("")),
            (
                "submitted_at".to_string(),
                serde_json::json!("2026-07-01T00:00:00Z"),
            ),
        ]),
    )
    .await;
    let (msg, input) = admin_get_msg("/b/products/api/admin/products/p_blank_ts");
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert!(body["published_at"].is_null(), "{body}");
    assert_eq!(body["submitted_at"], "2026-07-01T00:00:00Z");
    assert!(body["deleted_at"].is_null());
}

// ============================================================
// The request body cannot rewrite a product's identity
// ============================================================

/// A PATCH body carrying `id` used to reach `update_live` verbatim, so the
/// write became `SET id = 'new' WHERE id = 'old' AND deleted_at IS NULL`:
/// one row updated, the guard satisfied, and every `product_id` reference in
/// `line_items`, `offers`, `product_versions` and `entitlements` orphaned —
/// the exact failure soft delete exists to prevent. The re-read then looked
/// up the ORIGINAL id, found nothing, and answered "Product not found", so
/// the caller was told the write had failed while it had in fact rewritten
/// the primary key.
#[tokio::test]
async fn admin_patch_cannot_rewrite_a_products_id() {
    let ctx = ctx().await;

    let (create, create_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({ "name": "Original" }),
    );
    let id = output_to_json(dispatch(&ctx, create, create_input).await).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (mut update, update_input) = request_msg(
        "update",
        &format!("/b/products/api/admin/products/{id}"),
        "admin_1",
        serde_json::json!({ "id": "p_hijacked", "name": "Renamed" }),
    );
    update.set_meta("auth.user_roles", "admin");
    let out = dispatch(&ctx, update, update_input).await;
    assert!(
        output_is_error(out, ErrorCode::InvalidArgument).await,
        "a body that names an unsettable field must be refused outright"
    );

    // The row still answers to its own id, and no row answers to the one the
    // body tried to claim.
    let kept = super::super::repo::products::get(&ctx, &id)
        .await
        .expect("the product must still answer to its original id");
    assert_eq!(kept.id, id);
    assert_eq!(
        crate::util::RecordExt::str_field(&kept, "name"),
        "Original",
        "a refused write must not apply its other fields either"
    );
    assert!(
        wafer_core::clients::database::get(&ctx, "impresspress__products__products", "p_hijacked")
            .await
            .is_err(),
        "no row may answer to the id the body tried to claim"
    );
}

/// The seller-owned PATCH reaches the same `update_live` with the same
/// caller-supplied body, so it carries the identical primary-key rewrite.
#[tokio::test]
async fn seller_patch_cannot_rewrite_a_products_id() {
    let ctx = user_products_ctx().await;

    let (create, create_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({ "name": "Original" }),
    );
    let id = output_to_json(dispatch(&ctx, create, create_input).await).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (update, update_input) = update_msg(
        &format!("/b/products/api/products/{id}"),
        "user_1",
        serde_json::json!({ "id": "p_hijacked", "name": "Renamed" }),
    );
    let out = dispatch(&ctx, update, update_input).await;
    assert!(
        output_is_error(out, ErrorCode::InvalidArgument).await,
        "a body that names an unsettable field must be refused outright"
    );

    let kept = super::super::repo::products::get(&ctx, &id)
        .await
        .expect("the product must still answer to its original id");
    assert_eq!(kept.id, id);
    assert_eq!(
        crate::util::RecordExt::str_field(&kept, "name"),
        "Original",
        "a refused write must not apply its other fields either"
    );
    assert!(
        wafer_core::clients::database::get(&ctx, "impresspress__products__products", "p_hijacked")
            .await
            .is_err(),
        "no row may answer to the id the body tried to claim"
    );
}

/// The handler-level refusal is a 400 for a clear message; the invariant
/// itself belongs to the repo, which is the layer every future caller goes
/// through. `update_live` must refuse an `id` in `data` on its own, so a new
/// call site cannot reintroduce the rewrite by forwarding a map the handler
/// never saw.
#[tokio::test]
async fn update_live_refuses_to_rewrite_the_primary_key() {
    let ctx = ctx().await;
    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!("Original"));
    seed(&ctx, "impresspress__products__products", "p1", data).await;

    let error = super::super::repo::products::update_live(
        &ctx,
        "p1",
        HashMap::from([("id".to_string(), serde_json::json!("p_hijacked"))]),
    )
    .await
    .expect_err("a product's id is immutable");
    assert_eq!(error.code, ErrorCode::InvalidArgument);

    assert!(
        super::super::repo::products::get(&ctx, "p1").await.is_ok(),
        "the row must still answer to its original id"
    );
}

/// The repo's `id`-rewrite refusal is a backstop for a caller that did not go
/// through `reject_unsettable_fields`, and it answers `InvalidArgument` with
/// a message naming what to change ("a product's id is immutable"). Every
/// product write handler matched only `NotFound` and funnelled the rest into
/// `err_internal`, which throws that message away and answers 500 — an
/// opaque server error for what is squarely a caller mistake, with nothing
/// for the caller to act on and a correlation id pointing at a log line that
/// says the same.
///
/// Reproduced by answering the write with `InvalidArgument` from below,
/// which is the shape any repository-level guard has when it reaches these
/// handlers. All three of them are driven, because each carried its own copy
/// of the match.
#[tokio::test]
async fn a_repo_invalid_argument_reaches_the_caller_as_a_400_carrying_its_message() {
    use crate::test_support::FailingDbOpContext;

    const REFUSAL: &str = "a product's id is immutable";

    async fn refusing(ctx: &crate::test_support::TestContext) -> FailingDbOpContext {
        FailingDbOpContext::failing_with(
            ctx.clone(),
            vec![(
                "database.update_where_count",
                super::super::repo::products::TABLE,
            )],
            wafer_run::WaferError::new(ErrorCode::InvalidArgument, REFUSAL),
        )
    }

    async fn refusal_message(out: wafer_run::OutputStream) -> String {
        match out.collect_buffered().await {
            Err(wafer_run::streams::output::TerminalNotResponse::Error(error)) => {
                assert_eq!(
                    error.code,
                    ErrorCode::InvalidArgument,
                    "a caller error from the repository must not become a 500: {error:?}"
                );
                error.message
            }
            other => panic!("the write must be refused: {other:?}"),
        }
    }

    // 1. the admin PATCH
    let ctx = ctx().await;
    let mut row = HashMap::new();
    row.insert("name".to_string(), serde_json::json!("Original"));
    seed(&ctx, "impresspress__products__products", "p_admin", row).await;
    let refusing_ctx = refusing(&ctx).await;
    let (mut msg, input) = update_msg(
        "/b/products/api/admin/products/p_admin",
        "admin_1",
        serde_json::json!({ "name": "Renamed" }),
    );
    msg.set_meta("auth.user_roles", "admin");
    let message = refusal_message(dispatch(&refusing_ctx, msg, input).await).await;
    assert!(
        message.contains(REFUSAL),
        "the admin PATCH must pass the refusal's own message through: {message}"
    );

    // 2. the seller PATCH
    let ctx = user_products_ctx().await;
    let (create, create_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({ "name": "Original" }),
    );
    let id = output_to_json(dispatch(&ctx, create, create_input).await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let refusing_ctx = refusing(&ctx).await;
    let (msg, input) = update_msg(
        &format!("/b/products/api/products/{id}"),
        "user_1",
        serde_json::json!({ "name": "Renamed" }),
    );
    let message = refusal_message(dispatch(&refusing_ctx, msg, input).await).await;
    assert!(
        message.contains(REFUSAL),
        "the seller PATCH must pass the refusal's own message through: {message}"
    );

    // 3. admin moderation, which writes the product through the same door
    let ctx = user_products_ctx().await;
    let mut pending = HashMap::new();
    pending.insert("name".to_string(), serde_json::json!("Pending"));
    pending.insert("owner_kind".to_string(), serde_json::json!("user"));
    pending.insert("owner_id".to_string(), serde_json::json!("seller_1"));
    pending.insert("approval_status".to_string(), serde_json::json!("pending"));
    pending.insert("status".to_string(), serde_json::json!("pending_review"));
    seed(
        &ctx,
        "impresspress__products__products",
        "p_pending",
        pending,
    )
    .await;
    seed(
        &ctx,
        super::super::repo::seller_accounts::TABLE,
        "acct_1",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("seller_1")),
            ("status".to_string(), serde_json::json!("active")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_stripe_1"),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(true)),
            ("payouts_enabled".to_string(), serde_json::json!(true)),
            ("requirements_json".to_string(), serde_json::json!("{}")),
            ("fee_basis_points".to_string(), serde_json::json!(250)),
        ]),
    )
    .await;
    let refusing_ctx = refusing(&ctx).await;
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/products/p_pending/approve",
        serde_json::json!({}),
    );
    let message = refusal_message(dispatch(&refusing_ctx, msg, input).await).await;
    assert!(
        message.contains(REFUSAL),
        "product moderation must pass the refusal's own message through: {message}"
    );
}

/// A `Context` that soft-deletes `product_id` the moment the FIRST by-id read
/// of the products table has been answered, and forwards everything else
/// untouched.
///
/// That is a concurrent delete landing between two reads inside one request,
/// reproduced deterministically instead of raced for. `stripe::handle_checkout`
/// reads the product twice — once inside `repo::offers::get_public`, once for
/// the checkout itself — and only the gap between them can produce a
/// `NotFound` from the second read.
#[derive(Clone)]
struct DeleteBetweenProductReads {
    inner: std::sync::Arc<crate::test_support::TestContext>,
    product_id: String,
    fired: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait::async_trait]
impl wafer_run::context::Context for DeleteBetweenProductReads {
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), wafer_run::WaferError> {
        self.inner
            .check_resource_access(resource, resource_type, access)
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.inner
            .resource_access_admitted(resource, resource_type, access)
    }

    async fn call_block(
        &self,
        name: &str,
        msg: wafer_run::Message,
        input: wafer_run::InputStream,
    ) -> wafer_run::OutputStream {
        #[derive(serde::Deserialize)]
        struct CollectionPeek {
            collection: String,
        }

        if name != "wafer-run/database" || msg.action() != "database.get" {
            return self.inner.call_block(name, msg, input).await;
        }
        let bytes = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return wafer_run::OutputStream::error(e),
        };
        let collection = wafer_block::codec::decode::<CollectionPeek>(&bytes)
            .map(|peek| peek.collection)
            .unwrap_or_default();
        let out = self
            .inner
            .call_block(name, msg, wafer_run::InputStream::from_bytes(bytes))
            .await;
        if collection == super::super::repo::products::TABLE
            && !self.fired.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            super::super::repo::products::soft_delete(self.inner.as_ref(), &self.product_id)
                .await
                .expect("the concurrent delete lands");
        }
        out
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
        self.inner.registered_blocks()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.inner.config_get(key)
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn wafer_run::context::Context> {
        std::sync::Arc::new(self.clone())
    }
}

/// A product soft-deleted between checkout's two product reads must answer
/// the storefront the same 404 its neighbouring refusals do — "Offer not
/// found" — not a 500.
///
/// `NotFound` from that second read is a state this branch created: the read
/// used to go straight to the table and could only fail for a row that was
/// physically gone, so mapping every error to `err_internal` was right. Now
/// it carries the soft-delete filter, so the ordinary outcome of a delete
/// landing mid-request renders as a server error on a public storefront —
/// while the identical condition observed a few microseconds earlier, inside
/// `repo::offers::get_public`, answers a clean 404.
#[tokio::test]
async fn checkout_404s_for_a_product_deleted_between_its_two_reads() {
    let ctx = std::sync::Arc::new(
        ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await,
    );
    let offer_id = seed_published_offer(ctx.as_ref(), "racing").await;

    let racing = DeleteBetweenProductReads {
        inner: ctx.clone(),
        product_id: "racing".to_string(),
        fired: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({ "offer_id": offer_id }),
    );
    let out = super::super::stripe::handle_checkout(&racing, &msg, input).await;
    assert!(
        output_is_error(out, ErrorCode::NotFound).await,
        "a product deleted mid-request must read as a missing offer, not a server error"
    );
}

/// One product, one caller, three answers. `owner_id = user_1` with
/// `created_by = admin_1` is the shape an administrator creating a listing
/// on a seller's behalf leaves behind, and it split the seller's own product
/// apart: `pages::product_manager` rendered it, `offers::verify_product`
/// accepted every offer and Payment Link route on it (both testing
/// `owner_id` OR `created_by`), while `verify_product_owner` — the single
/// door for GET/PATCH/DELETE/duplicate on the same product — compared
/// `created_by` alone and answered 404.
///
/// The half that was open is the more dangerous one: creating an offer and
/// opening a Payment Link is how money starts moving. So the doors agree on
/// the wider rule, from ONE predicate rather than three copies of it: a
/// caller may act on a product they own or created.
#[tokio::test]
async fn a_product_owned_but_not_created_by_the_seller_answers_the_same_everywhere() {
    let ctx = user_products_ctx().await;

    let mut assigned = HashMap::new();
    assigned.insert("name".to_string(), serde_json::json!("Assigned"));
    assigned.insert("status".to_string(), serde_json::json!("draft"));
    assigned.insert("owner_kind".to_string(), serde_json::json!("user"));
    assigned.insert("owner_id".to_string(), serde_json::json!("user_1"));
    assigned.insert("created_by".to_string(), serde_json::json!("admin_1"));
    seed(
        &ctx,
        "impresspress__products__products",
        "assigned",
        assigned,
    )
    .await;

    // The two doors that already accepted this caller — the positive
    // controls the CRUD door has to match, not the other way round.
    let html = output_to_html(
        super::super::pages::product_manager(
            &ctx,
            &crate::test_support::auth_msg("read", "/b/products/api/products/assigned", "user_1"),
            "assigned",
            false,
        )
        .await,
    )
    .await;
    assert!(
        html.contains("Assigned"),
        "the owner's product page already renders for them: {html}"
    );

    let (msg, input) = get_msg("/b/products/api/products/assigned/offers", "user_1");
    let listed = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert!(
        listed["offers"].is_array(),
        "the owner's offer routes already accept them: {listed}"
    );

    // GET
    let (msg, input) = get_msg("/b/products/api/products/assigned", "user_1");
    let fetched = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(
        fetched["id"], "assigned",
        "the owner must be able to read the product their own page renders: {fetched}"
    );

    // PATCH
    let (msg, input) = update_msg(
        "/b/products/api/products/assigned",
        "user_1",
        serde_json::json!({ "name": "Renamed" }),
    );
    let patched = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(
        patched["name"], "Renamed",
        "the owner must be able to edit it: {patched}"
    );

    // DELETE
    let (msg, input) = delete_msg("/b/products/api/products/assigned", "user_1");
    let deleted = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(
        deleted["deleted"], true,
        "the owner must be able to delete it: {deleted}"
    );

    // And the rule is still a rule: a third party is neither owner nor
    // creator and gets the same 404 from every one of those doors.
    let (msg, input) = get_msg("/b/products/api/products/assigned", "user_2");
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::NotFound).await);
    let (msg, input) = get_msg("/b/products/api/products/assigned/offers", "user_2");
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::NotFound).await);
    let stranger = super::super::pages::product_manager(
        &ctx,
        &crate::test_support::auth_msg("read", "/b/products/api/products/assigned", "user_2"),
        "assigned",
        false,
    )
    .await;
    assert!(output_is_error(stranger, ErrorCode::NotFound).await);
}

// ============================================================
// The seller's own deleted products
// ============================================================
//
// Soft delete gave the admin a Deleted view, a restore endpoint and a
// close-only manager. A seller got the delete and none of the three: their
// own product list reads live-only, so a product they deleted vanished from
// every page they can open, while its Prices and Payment Links stayed live in
// the connected account and went on taking money. Before soft delete a hard
// delete at least left nothing behind.
//
// Authorization is the whole risk here. `/b/products` is a PUBLIC route
// prefix, so what a caller faces is the tier the endpoint DECLARES, and only
// the router enforces it. Every test below therefore drives `dispatch_routed`
// (the real `route_to_block` → `check_access` → `ProductsBlock::handle`),
// never `harness::dispatch` or a handler directly, which skip `check_access`.

/// Seed a live product owned by `owner`, the way seller-created products
/// carry ownership: `owner_kind = "user"` with `owner_id` AND `created_by`
/// both set to the seller.
async fn seed_seller_product(
    ctx: &crate::test_support::TestContext,
    id: &str,
    owner: &str,
    name: &str,
) {
    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!(name));
    data.insert("status".to_string(), serde_json::json!("active"));
    data.insert("owner_kind".to_string(), serde_json::json!("user"));
    data.insert("owner_id".to_string(), serde_json::json!(owner));
    data.insert("created_by".to_string(), serde_json::json!(owner));
    seed(ctx, "impresspress__products__products", id, data).await;
}

/// [`seed_seller_product`] plus the soft delete, since every test in this
/// section is about a product the seller has already deleted.
async fn seed_deleted_seller_product(
    ctx: &crate::test_support::TestContext,
    id: &str,
    owner: &str,
    name: &str,
) {
    seed_seller_product(ctx, id, owner, name).await;
    soft_delete_product(ctx, id).await;
}

/// The seller's mirror of `manage_products_deleted_view_lists_only_deleted_products`,
/// with the extra obligation that only makes sense on a seller page: the
/// deleted set is scoped to the caller. `list_deleted` narrows the DELETED
/// set the same way `list_page` narrows the live one, so the owner filter the
/// live list already carries has to be handed to it too — without that, the
/// Deleted tab is every seller's deleted products, on a page reachable by any
/// authenticated user.
#[tokio::test]
async fn my_products_deleted_view_lists_only_the_sellers_own_deleted_products() {
    let ctx = user_products_ctx().await;

    seed_seller_product(&ctx, "mine_live", "seller_a", "Mine and live").await;
    seed_deleted_seller_product(&ctx, "mine_gone", "seller_a", "Mine and deleted").await;
    seed_deleted_seller_product(&ctx, "theirs_gone", "seller_b", "Theirs and deleted").await;

    let (live_msg, _input) = get_msg("/b/products/my-products", "seller_a");
    let live_html = output_to_html(super::super::pages::my_products(&ctx, &live_msg).await).await;
    assert!(live_html.contains("Mine and live"), "{live_html}");
    assert!(!live_html.contains("Mine and deleted"), "{live_html}");

    let (mut deleted_msg, _input) = get_msg("/b/products/my-products", "seller_a");
    deleted_msg.set_meta("req.query.view", "deleted");
    let deleted_html =
        output_to_html(super::super::pages::my_products(&ctx, &deleted_msg).await).await;
    assert!(deleted_html.contains("Mine and deleted"), "{deleted_html}");
    assert!(!deleted_html.contains("Mine and live"), "{deleted_html}");
    assert!(
        !deleted_html.contains("Theirs and deleted"),
        "a seller's deleted view must not show another seller's products: {deleted_html}"
    );
}

/// Both doors out, not just the dangerous one. Restore returns an active,
/// approved product to the public catalog immediately; the close-only
/// manager is how a seller shuts the Stripe surface down *without* doing
/// that. The admin Deleted view carries both, and a seller whose reason for
/// deleting was "stop selling this" needs the second one more than the first.
#[tokio::test]
async fn my_products_deleted_view_offers_both_restore_and_close() {
    let ctx = user_products_ctx().await;
    // An id that is not URL-safe, so both links are pinned to carry the same
    // percent-encoding the admin view's do.
    let awkward_id = "prod/1?x#y";
    seed_deleted_seller_product(&ctx, awkward_id, "seller_a", "Deleted product").await;

    let (mut msg, _input) = get_msg("/b/products/my-products", "seller_a");
    msg.set_meta("req.query.view", "deleted");
    let html = output_to_html(super::super::pages::my_products(&ctx, &msg).await).await;

    assert!(
        html.contains("/b/products/api/products/prod%2F1%3Fx%23y/restore"),
        "a seller's deleted row must offer Restore wired to the seller restore endpoint: {html}"
    );
    assert!(
        html.contains("/b/products/my-products/prod%2F1%3Fx%23y/close"),
        "a seller's deleted row must offer a way to close its Stripe surface: {html}"
    );
    assert!(
        !html.contains(r#"href="/b/products/my-products/prod%2F1%3Fx%23y""#),
        "a deleted row must not link into the manager, which refuses a soft-deleted product: {html}"
    );
    assert!(
        !html.contains("/b/products/api/admin/"),
        "a seller page must not target the admin API: {html}"
    );
}

/// The whole trip, the way `the_restore_url_the_deleted_view_renders_restores_that_product`
/// does it for the admin: the URL is read back out of the rendered HTML (so a
/// change to either half of the encoding is caught) and sent through the real
/// router.
#[tokio::test]
async fn the_restore_url_a_sellers_deleted_view_renders_restores_that_product() {
    let ctx = user_products_ctx().await;
    let awkward_id = "prod/1?x#y";
    seed_deleted_seller_product(&ctx, awkward_id, "seller_a", "Deleted product").await;

    let (mut page_msg, _input) = get_msg("/b/products/my-products", "seller_a");
    page_msg.set_meta("req.query.view", "deleted");
    let html = output_to_html(super::super::pages::my_products(&ctx, &page_msg).await).await;
    let restore_url = rendered_hx_post(&html)
        .unwrap_or_else(|| panic!("the deleted row must render an hx-post Restore action: {html}"));

    let (msg, input) = create_msg(&restore_url, "seller_a", serde_json::json!({}));
    let body = output_to_json(dispatch_routed(&ctx, msg, input).await).await;
    assert_eq!(
        body["id"], awkward_id,
        "posting the rendered Restore URL must reach the product it names: {body}"
    );
    assert!(
        !is_soft_deleted(&ctx, awkward_id).await,
        "the seller's restore must actually clear deleted_at"
    );
}

/// The negative half, and the reason every assertion in this section goes
/// through the router: a seller must not restore a product that is not
/// theirs. The route is `Authenticated`, which admits every logged-in
/// caller, so OWNERSHIP is the only thing between seller B and seller A's
/// catalog.
#[tokio::test]
async fn a_seller_cannot_restore_another_sellers_product() {
    let ctx = user_products_ctx().await;
    seed_deleted_seller_product(&ctx, "a_product", "seller_a", "Seller A's product").await;

    let path = "/b/products/api/products/a_product/restore";
    let (msg, input) = create_msg(path, "seller_b", serde_json::json!({}));
    assert!(
        dispatch_routed(&ctx, msg, input)
            .await
            .collect_buffered()
            .await
            .is_err(),
        "seller B restored seller A's product through {path}"
    );
    assert!(
        is_soft_deleted(&ctx, "a_product").await,
        "seller B's refused restore must leave seller A's product deleted"
    );

    // Positive control, so the refusal above cannot be passing merely
    // because nothing routes anywhere.
    let (msg, input) = create_msg(path, "seller_a", serde_json::json!({}));
    let body = output_to_json(dispatch_routed(&ctx, msg, input).await).await;
    assert_eq!(
        body["id"], "a_product",
        "the owner must be able to restore through {path}: {body}"
    );
    assert!(!is_soft_deleted(&ctx, "a_product").await);
}

/// Every seller route used to answer at a second, undeclared spelling:
/// `ProductsBlock::handle` entered the user dispatch table from the raw
/// `/b/products/...` path as well as the `/b/products/api`-stripped one, so
/// `POST /b/products/products/{id}/restore` reached the seller restore
/// handler at `declared_access`'s fallback tier while only the `/api/`
/// spelling was declared — the gap the restore escalation shipped through.
/// The route table now has one spelling per handler, the declared one; the
/// former alias is not a route and answers 404 from the block itself, before
/// any gate or handler runs. Pinned here, with the owner on the alias, so
/// the second spelling cannot silently come back.
#[tokio::test]
async fn the_former_alias_spelling_of_a_seller_route_is_not_found() {
    let ctx = user_products_ctx().await;
    seed_deleted_seller_product(&ctx, "a_product", "seller_a", "Seller A's product").await;

    let alias = "/b/products/products/a_product/restore";
    let (msg, input) = create_msg(alias, "seller_a", serde_json::json!({}));
    assert!(
        output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::NotFound).await,
        "the owner's POST to the former alias {alias} must be NotFound from the block"
    );
    assert!(
        is_soft_deleted(&ctx, "a_product").await,
        "a 404 on the alias must not have reached the restore handler"
    );

    // Positive control on the declared spelling, through the router.
    let declared = "/b/products/api/products/a_product/restore";
    let (msg, input) = create_msg(declared, "seller_a", serde_json::json!({}));
    let body = output_to_json(dispatch_routed(&ctx, msg, input).await).await;
    assert_eq!(
        body["id"], "a_product",
        "the owner must be able to restore through {declared}: {body}"
    );
    assert!(!is_soft_deleted(&ctx, "a_product").await);
}

/// An unauthenticated caller is refused by the router itself: the route is
/// declared `Authenticated`. Pinned because the seller restore endpoint is
/// the first *write* to live on a public prefix under an owner check alone —
/// if the tier ever slipped to Public, `is_owned_by`'s empty-`user_id` guard
/// would be the only thing left.
#[tokio::test]
async fn an_anonymous_caller_cannot_restore_a_deleted_product() {
    let ctx = user_products_ctx().await;
    seed_deleted_seller_product(&ctx, "a_product", "seller_a", "Seller A's product").await;

    let path = "/b/products/api/products/a_product/restore";
    let (msg, input) = create_msg(path, "", serde_json::json!({}));
    assert!(
        dispatch_routed(&ctx, msg, input)
            .await
            .collect_buffered()
            .await
            .is_err(),
        "an anonymous POST to {path} must not restore a product"
    );
    assert!(is_soft_deleted(&ctx, "a_product").await);
}

/// The seller half of `the_close_manager_archives_a_deleted_products_offer_
/// from_its_own_rendered_action`: the page a seller reaches, and the API its
/// buttons target, are both owner-scoped and both actually work.
///
/// The seller-tier offer/payment-link API already permitted this (the
/// `ProductState::LiveOrDeleted` widening covers `OfferAccess::Owner` too) —
/// only the page that reaches it was missing, which made the capability
/// operator-only in practice.
#[tokio::test]
async fn a_seller_closes_their_own_deleted_products_money_surface() {
    let ctx = user_products_ctx().await;
    let (offer_id, link_id) = seed_a_deleted_product_with_a_money_surface(&ctx, "gone").await;
    // `seed_published_offer` writes no ownership, so give the row an owner
    // before the page is asked whose it is.
    wafer_core::clients::database::update(
        &ctx,
        super::super::repo::products::TABLE,
        "gone",
        HashMap::from([
            ("owner_kind".to_string(), serde_json::json!("user")),
            ("owner_id".to_string(), serde_json::json!("seller_a")),
            ("created_by".to_string(), serde_json::json!("seller_a")),
        ]),
    )
    .await
    .expect("assign the product to a seller");

    let (msg, input) = get_msg("/b/products/my-products/gone/close", "seller_a");
    let html = output_to_html(dispatch_routed(&ctx, msg, input).await).await;
    assert!(
        !html.contains("/b/products/api/admin/"),
        "the seller's close manager must target the seller API, not the admin one: {html}"
    );

    let archive_url = rendered_attr(&html, "hx-delete=\"", &offer_id).unwrap_or_else(|| {
        panic!(
            "the seller close manager must render an archive action for offer {offer_id}: {html}"
        )
    });
    let (msg, input) = delete_msg(&archive_url, "seller_a");
    let archived = output_to_json(dispatch_routed(&ctx, msg, input).await).await;
    assert_eq!(
        archived["status"], "archived",
        "the rendered archive action must archive the seller's deleted offer: {archived}"
    );

    let deactivate_url = rendered_attr(&html, "hx-delete=\"", &link_id).unwrap_or_else(|| {
        panic!(
            "the seller close manager must render a deactivate action for link {link_id}: {html}"
        )
    });
    let (msg, input) = delete_msg(&deactivate_url, "seller_a");
    let deactivated = output_to_json(dispatch_routed(&ctx, msg, input).await).await;
    assert_eq!(
        deactivated["active"],
        serde_json::json!(false),
        "the rendered deactivate action must take the seller's link down: {deactivated}"
    );

    // Closing a money surface must not put the listing back in the catalog.
    assert!(is_soft_deleted(&ctx, "gone").await);
}

/// The close manager reads a soft-deleted row through `get_deleted`, which is
/// exactly the read the live-only door refuses — so the ownership check
/// cannot come from `repo::products::get`, and forgetting it entirely would
/// have been invisible. Seller B must get the same 404 every other seller
/// door gives for a product that is not theirs.
#[tokio::test]
async fn a_seller_cannot_open_another_sellers_close_manager() {
    let ctx = user_products_ctx().await;
    seed_a_deleted_product_with_a_money_surface(&ctx, "gone").await;
    wafer_core::clients::database::update(
        &ctx,
        super::super::repo::products::TABLE,
        "gone",
        HashMap::from([
            ("owner_kind".to_string(), serde_json::json!("user")),
            ("owner_id".to_string(), serde_json::json!("seller_a")),
            ("created_by".to_string(), serde_json::json!("seller_a")),
        ]),
    )
    .await
    .expect("assign the product to a seller");

    for user in ["seller_b", ""] {
        let (msg, input) = get_msg("/b/products/my-products/gone/close", user);
        assert!(
            dispatch_routed(&ctx, msg, input)
                .await
                .collect_buffered()
                .await
                .is_err(),
            "`{user}` must not reach seller A's close manager"
        );
    }

    // Positive control: the owner does.
    let (msg, input) = get_msg("/b/products/my-products/gone/close", "seller_a");
    let html = output_to_html(dispatch_routed(&ctx, msg, input).await).await;
    assert!(
        html.contains("hx-delete="),
        "the owner must reach the page the assertions above deny: {html}"
    );
}

/// A template lookup that could not run must not write a template-less row.
///
/// Both `default_id` twins returned `Option<String>` and folded a failed read
/// into `None`, and both creates read that as "there is no default template"
/// and wrote the row anyway. The row outlives the outage looking like a
/// deliberate choice, which is precisely what the seeded default exists to
/// prevent — and neither create logged a thing.
#[tokio::test]
async fn a_create_whose_template_lookup_fails_writes_no_row() {
    use crate::{blocks::products::repo, test_support::FailingDbOpContext};

    let ctx = user_products_ctx().await;

    // The positive control first: both creates land against a healthy
    // database, so the refusals below cannot pass for an unrelated reason.
    let (msg, input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "Healthy"}),
    );
    let group = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert!(group["id"].as_str().is_some_and(|id| !id.is_empty()));

    let (msg, input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({"name": "Healthy", "group_id": group["id"]}),
    );
    let product = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert!(product["id"].as_str().is_some_and(|id| !id.is_empty()));

    let groups_before = repo::groups::count(&ctx, &[]).await.expect("count groups");
    let products_before = repo::products::count(&ctx, &[])
        .await
        .expect("count products");

    let failing = FailingDbOpContext::new(
        ctx.clone(),
        vec![
            ("database.list", repo::group_templates::TABLE),
            ("database.list", repo::product_templates::TABLE),
        ],
    );

    let (msg, input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "During the outage"}),
    );
    assert!(
        output_is_error(dispatch(&failing, msg, input).await, ErrorCode::Internal).await,
        "a group create whose template lookup failed must refuse"
    );

    let (msg, input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({"name": "During the outage", "group_id": group["id"]}),
    );
    assert!(
        output_is_error(dispatch(&failing, msg, input).await, ErrorCode::Internal).await,
        "a product create whose template lookup failed must refuse"
    );

    assert_eq!(
        repo::groups::count(&ctx, &[]).await.expect("count groups"),
        groups_before,
        "the refused group create must leave no row behind"
    );
    assert_eq!(
        repo::products::count(&ctx, &[])
            .await
            .expect("count products"),
        products_before,
        "the refused product create must leave no row behind"
    );
}

/// A product create naming a group answers 400 "Group not found" only when
/// the group is absent; a group read that failed is a 500.
///
/// Every group read error was folded into "Group not found", so an outage
/// told the seller their own group did not exist.
#[tokio::test]
async fn a_product_create_whose_group_read_fails_is_a_fault_not_a_missing_group() {
    use crate::{
        blocks::products::repo,
        test_support::{output_http_status, FailingDbOpContext},
    };

    let ctx = user_products_ctx().await;
    let (msg, input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "Mine"}),
    );
    let group = output_to_json(dispatch(&ctx, msg, input).await).await;

    // Guard: a group that does not exist is still the caller's 400 (passes
    // before and after the fix).
    let (msg, input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({"name": "Orphan", "group_id": "grp_absent"}),
    );
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        400
    );

    let failing = FailingDbOpContext::new(ctx.clone(), vec![("database.get", repo::groups::TABLE)]);
    let (msg, input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({"name": "During the outage", "group_id": group["id"]}),
    );
    assert_eq!(
        output_http_status(dispatch(&failing, msg, input).await).await,
        500,
        "a group read that failed is a server fault, not a missing group"
    );
}

// ============================================================
// Named slug conflicts
// ============================================================

/// What a product write whose slug a live product of the same owner holds is
/// told: the field and the value, nothing of the index or the table.
const WIDGET_SLUG_TAKEN: &str =
    "A product with the slug \"widget\" already exists. Choose a different slug.";

/// Asserts `out` is the named 409 for the slug `widget`, with no schema text.
async fn expect_widget_slug_taken(out: wafer_run::OutputStream) {
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap_or_default();
    assert_eq!(parts.status, 409, "{body}");
    assert_eq!(
        body["message"],
        serde_json::json!(WIDGET_SLUG_TAKEN),
        "{body}"
    );
    let text = body.to_string();
    assert!(
        !text.contains("impresspress__products") && !text.contains("UNIQUE"),
        "schema leaked: {text}"
    );
}

/// A product id from a create that must succeed.
async fn created_id(
    ctx: &crate::test_support::TestContext,
    msg: (wafer_run::Message, wafer_run::InputStream),
) -> String {
    let body = output_to_json(dispatch(ctx, msg.0, msg.1).await).await;
    body["id"].as_str().expect("created product id").to_string()
}

/// An admin creating a second platform product under a slug a live one holds
/// is refused by migration 005's owner/slug index, and told which slug.
#[tokio::test]
async fn admin_create_with_a_taken_slug_is_a_409_naming_the_slug() {
    let ctx = ctx().await;
    let body = serde_json::json!({"name": "Widget", "slug": "widget"});
    created_id(
        &ctx,
        admin_create_msg("/b/products/api/admin/products", body.clone()),
    )
    .await;

    let (msg, input) = admin_create_msg("/b/products/api/admin/products", body);
    expect_widget_slug_taken(dispatch(&ctx, msg, input).await).await;
}

/// Changing a product's slug onto one another live product holds is the same
/// named 409, and the product keeps its own slug.
#[tokio::test]
async fn admin_update_onto_a_taken_slug_is_a_409_naming_the_slug() {
    let ctx = ctx().await;
    created_id(
        &ctx,
        admin_create_msg(
            "/b/products/api/admin/products",
            serde_json::json!({"name": "Widget", "slug": "widget"}),
        ),
    )
    .await;
    let other = created_id(
        &ctx,
        admin_create_msg(
            "/b/products/api/admin/products",
            serde_json::json!({"name": "Gadget", "slug": "gadget"}),
        ),
    )
    .await;

    let (mut msg, input) = request_msg(
        "update",
        &format!("/b/products/api/admin/products/{other}"),
        "admin_1",
        serde_json::json!({"slug": "widget"}),
    );
    msg.set_meta("auth.user_roles", "admin");
    expect_widget_slug_taken(dispatch(&ctx, msg, input).await).await;

    let (msg, input) = admin_get_msg(&format!("/b/products/api/admin/products/{other}"));
    let stored = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(stored["slug"], "gadget");
}

/// A seller's own create and update meet the same index, scoped to their own
/// products, and get the same named 409.
#[tokio::test]
async fn seller_writes_onto_a_taken_slug_are_a_409_naming_the_slug() {
    let ctx = user_products_ctx().await;
    created_id(
        &ctx,
        create_msg(
            "/b/products/api/products",
            "user_1",
            serde_json::json!({"name": "Widget", "slug": "widget"}),
        ),
    )
    .await;

    let (msg, input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({"name": "Widget again", "slug": "widget"}),
    );
    expect_widget_slug_taken(dispatch(&ctx, msg, input).await).await;

    let other = created_id(
        &ctx,
        create_msg(
            "/b/products/api/products",
            "user_1",
            serde_json::json!({"name": "Gadget", "slug": "gadget"}),
        ),
    )
    .await;
    let (msg, input) = update_msg(
        &format!("/b/products/api/products/{other}"),
        "user_1",
        serde_json::json!({"slug": "widget"}),
    );
    expect_widget_slug_taken(dispatch(&ctx, msg, input).await).await;
}

/// The message a write was refused with, or `None` if it was not refused
/// as `InvalidArgument`.
async fn invalid_argument_message(out: wafer_run::OutputStream) -> Option<String> {
    use wafer_run::streams::output::TerminalNotResponse;
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) if e.code == ErrorCode::InvalidArgument => {
            Some(e.message)
        }
        _ => None,
    }
}

/// Every product create and update — admin and seller — holds a slug to
/// the grammar the edit forms' `pattern` and `maxlength` state. Those are
/// browser hints; the API is reachable without them, so the server has to
/// refuse the same slugs, with a 400 that says why, and write nothing.
#[tokio::test]
async fn every_product_write_refuses_a_slug_outside_the_grammar() {
    let ctx = user_products_ctx().await;
    let bad = [
        ("Has Spaces", "lowercase letters"),
        ("UPPER", "lowercase letters"),
        ("double--hyphen", "single hyphens"),
        ("-leading", "single hyphens"),
        ("trailing-", "single hyphens"),
        ("caf\u{e9}", "lowercase letters"),
        (&*"a".repeat(161), "at most 160"),
    ];

    let (create, create_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({ "name": "Admin", "slug": "admin-original" }),
    );
    let admin_id = output_to_json(dispatch(&ctx, create, create_input).await).await["id"]
        .as_str()
        .expect("admin create with a valid slug")
        .to_string();
    let (create, create_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({ "name": "Seller", "slug": "seller-original" }),
    );
    let seller_id = output_to_json(dispatch(&ctx, create, create_input).await).await["id"]
        .as_str()
        .expect("seller create with a valid slug")
        .to_string();

    for (slug, says) in bad {
        let body = serde_json::json!({ "name": "Refused", "slug": slug });
        let writes = [
            (
                "admin create",
                admin_create_msg("/b/products/api/admin/products", body.clone()),
            ),
            (
                "seller create",
                create_msg("/b/products/api/products", "user_1", body.clone()),
            ),
            ("admin update", {
                let (mut msg, input) = request_msg(
                    "update",
                    &format!("/b/products/api/admin/products/{admin_id}"),
                    "admin_1",
                    body.clone(),
                );
                msg.set_meta("auth.user_roles", "admin");
                (msg, input)
            }),
            (
                "seller update",
                update_msg(
                    &format!("/b/products/api/products/{seller_id}"),
                    "user_1",
                    body.clone(),
                ),
            ),
        ];
        for (path, (msg, input)) in writes {
            let message = invalid_argument_message(dispatch(&ctx, msg, input).await)
                .await
                .unwrap_or_else(|| panic!("{path} must refuse the slug {slug:?} with a 400"));
            assert!(
                message.contains("slug") && message.contains(says),
                "{path}, slug {slug:?}: {message}"
            );
        }
    }

    for (id, slug) in [
        (&admin_id, "admin-original"),
        (&seller_id, "seller-original"),
    ] {
        let kept = super::super::repo::products::get(&ctx, id).await.unwrap();
        assert_eq!(crate::util::RecordExt::str_field(&kept, "slug"), slug);
        assert_ne!(
            crate::util::RecordExt::str_field(&kept, "name"),
            "Refused",
            "a refused update must apply none of its fields"
        );
    }
    let rows = super::super::repo::products::count(&ctx, &[])
        .await
        .unwrap();
    assert_eq!(rows, 2, "a refused create must write no row");

    // The bounds are inclusive, and empty means no web address.
    let longest = "a".repeat(160);
    for slug in [longest.as_str(), "a1-b2-c3", ""] {
        let (msg, input) = update_msg(
            &format!("/b/products/api/products/{seller_id}"),
            "user_1",
            serde_json::json!({ "slug": slug }),
        );
        let body = output_to_json(dispatch(&ctx, msg, input).await).await;
        assert_eq!(body["slug"], slug, "{slug:?} must be accepted: {body}");
    }
}

/// Store `slug` on product `id` beneath the API's check, as a row written
/// before the grammar was enforced (or restored from a data snapshot) has it.
async fn store_legacy_slug(ctx: &crate::test_support::TestContext, id: &str, slug: &str) {
    super::super::repo::products::update_live(
        ctx,
        id,
        HashMap::from([("slug".to_string(), serde_json::json!(slug))]),
    )
    .await
    .expect("store the legacy slug");
}

/// A stored slug outside the grammar does not lock its product: the edit
/// form sends the slug back with every save, and an update that leaves it
/// unchanged is accepted on both tiers. Changing it is held to the grammar,
/// including changing it to a different invalid value.
#[tokio::test]
async fn an_unchanged_legacy_slug_does_not_block_an_edit() {
    let ctx = user_products_ctx().await;
    let (create, create_input) = admin_create_msg(
        "/b/products/api/admin/products",
        serde_json::json!({ "name": "Admin shirt" }),
    );
    let admin_id = output_to_json(dispatch(&ctx, create, create_input).await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (create, create_input) = create_msg(
        "/b/products/api/products",
        "user_1",
        serde_json::json!({ "name": "Seller shirt" }),
    );
    let seller_id = output_to_json(dispatch(&ctx, create, create_input).await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    store_legacy_slug(&ctx, &admin_id, "My Shirt").await;
    store_legacy_slug(&ctx, &seller_id, "My Shirt").await;

    let admin_patch = |body: serde_json::Value| {
        let (mut msg, input) = request_msg(
            "update",
            &format!("/b/products/api/admin/products/{admin_id}"),
            "admin_1",
            body,
        );
        msg.set_meta("auth.user_roles", "admin");
        (msg, input)
    };
    let seller_patch = |body: serde_json::Value| {
        update_msg(
            &format!("/b/products/api/products/{seller_id}"),
            "user_1",
            body,
        )
    };

    // What the manager form sends: every field, the slug unchanged.
    for (tier, (msg, input)) in [
        (
            "admin",
            admin_patch(serde_json::json!({ "name": "Renamed", "slug": "My Shirt" })),
        ),
        (
            "seller",
            seller_patch(serde_json::json!({ "name": "Renamed", "slug": "My Shirt" })),
        ),
    ] {
        let body = output_to_json(dispatch(&ctx, msg, input).await).await;
        assert_eq!(body["name"], "Renamed", "{tier}: {body}");
        assert_eq!(body["slug"], "My Shirt", "{tier}: {body}");
    }

    for (tier, (msg, input)) in [
        (
            "admin",
            admin_patch(serde_json::json!({ "slug": "Other Shirt" })),
        ),
        (
            "seller",
            seller_patch(serde_json::json!({ "slug": "Other Shirt" })),
        ),
    ] {
        let message = invalid_argument_message(dispatch(&ctx, msg, input).await)
            .await
            .unwrap_or_else(|| panic!("{tier}: a changed slug is held to the grammar"));
        assert!(message.contains("slug"), "{tier}: {message}");
    }

    for (tier, (msg, input)) in [
        (
            "admin",
            admin_patch(serde_json::json!({ "slug": "my-shirt" })),
        ),
        (
            "seller",
            seller_patch(serde_json::json!({ "slug": "my-shirt" })),
        ),
    ] {
        let body = output_to_json(dispatch(&ctx, msg, input).await).await;
        assert_eq!(body["slug"], "my-shirt", "{tier}: {body}");
    }
}

/// A duplicate writes its slug without the API's check, so the slug has to
/// be valid by construction — from a legacy source slug, and from one whose
/// cut for the `-copy-` suffix lands just after a hyphen.
#[tokio::test]
async fn a_duplicates_slug_is_always_in_the_grammar() {
    use super::super::contracts::{check_product_slug, PRODUCT_SLUG_MAX_LEN};

    let ctx = ctx().await;
    let hyphen_at_cut = format!("{}-{}", "a".repeat(145), "b".repeat(14));
    let hyphen_at_140 = format!("{}-{}", "a".repeat(139), "b".repeat(20));
    assert_eq!(check_product_slug(&hyphen_at_cut), Ok(()));
    assert_eq!(check_product_slug(&hyphen_at_140), Ok(()));
    for (stored, expected_base) in [
        ("My Shirt", "my-shirt".to_string()),
        ("--", "product".to_string()),
        (hyphen_at_cut.as_str(), "a".repeat(145)),
        (
            hyphen_at_140.as_str(),
            format!("{}-{}", "a".repeat(139), "b".repeat(6)),
        ),
    ] {
        let (create, create_input) = admin_create_msg(
            "/b/products/api/admin/products",
            serde_json::json!({ "name": "Source" }),
        );
        let id = output_to_json(dispatch(&ctx, create, create_input).await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        store_legacy_slug(&ctx, &id, stored).await;

        let (msg, input) = admin_create_msg(
            &format!("/b/products/api/admin/products/{id}/duplicate"),
            serde_json::json!({}),
        );
        let duplicated = output_to_json(dispatch(&ctx, msg, input).await).await;
        let slug = duplicated["product"]["slug"]
            .as_str()
            .unwrap_or_else(|| panic!("duplicate of {stored:?}: {duplicated}"));
        assert_eq!(check_product_slug(slug), Ok(()), "{stored:?} -> {slug:?}");
        assert!(slug.len() <= PRODUCT_SLUG_MAX_LEN, "{slug:?}");
        assert!(
            slug.starts_with(&format!("{expected_base}-copy-")),
            "{stored:?} -> {slug:?}"
        );
    }
}
