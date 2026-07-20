use std::collections::HashMap;

use base64ct::{Base64, Encoding};
use wafer_run::ErrorCode;

use super::harness::*;

// ============================================================
// Admin Product CRUD
// ============================================================

#[tokio::test]
async fn admin_create_product() {
    let ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/admin/b/products/products",
        serde_json::json!({
            "name": "Cloud Hosting",
            "description": "Managed hosting",
            "currency": "USD"
        }),
    );

    let out = dispatch_admin(&ctx, msg, input).await;
    let body = output_to_json(out).await;
    assert!(body["id"].as_str().is_some());
    assert_eq!(body["data"]["name"], "Cloud Hosting");
    assert_eq!(body["data"]["status"], "draft");
    assert_eq!(body["data"]["created_by"], "admin_1");
}

#[tokio::test]
async fn admin_list_products() {
    let ctx = ctx().await;

    // Create two products
    let (msg1, input1) = admin_create_msg(
        "/admin/b/products/products",
        serde_json::json!({
            "name": "Product A"
        }),
    );
    dispatch_admin(&ctx, msg1, input1).await;
    let (msg2, input2) = admin_create_msg(
        "/admin/b/products/products",
        serde_json::json!({
            "name": "Product B"
        }),
    );
    dispatch_admin(&ctx, msg2, input2).await;

    let (list_msg, list_input) = admin_get_msg("/admin/b/products/products");
    let out = dispatch_admin(&ctx, list_msg, list_input).await;
    let body = output_to_json(out).await;
    assert!(body["records"].as_array().unwrap().len() >= 2);
}

#[tokio::test]
async fn admin_get_product() {
    let ctx = ctx().await;

    let (create_msg_data, create_input) = admin_create_msg(
        "/admin/b/products/products",
        serde_json::json!({
            "name": "Widget"
        }),
    );
    let create_out = dispatch_admin(&ctx, create_msg_data, create_input).await;
    let id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (get_msg_data, get_input) = admin_get_msg(&format!("/admin/b/products/products/{id}"));
    let out = dispatch_admin(&ctx, get_msg_data, get_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["data"]["name"], "Widget");
}

#[tokio::test]
async fn admin_update_product() {
    let ctx = ctx().await;

    let (create, create_input) = admin_create_msg(
        "/admin/b/products/products",
        serde_json::json!({
            "name": "Old Name"
        }),
    );
    let create_out = dispatch_admin(&ctx, create, create_input).await;
    let id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (mut update, update_input) = request_msg(
        "update",
        &format!("/admin/b/products/products/{id}"),
        "admin_1",
        serde_json::json!({
            "name": "New Name"
        }),
    );
    update.set_meta("auth.user_roles", "admin");
    let out = dispatch_admin(&ctx, update, update_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["data"]["name"], "New Name");
}

#[tokio::test]
async fn admin_delete_product() {
    let ctx = ctx().await;

    let (create, create_input) = admin_create_msg(
        "/admin/b/products/products",
        serde_json::json!({
            "name": "To Delete"
        }),
    );
    let create_out = dispatch_admin(&ctx, create, create_input).await;
    let id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (mut del, del_input) = delete_msg(&format!("/admin/b/products/products/{id}"), "admin_1");
    del.set_meta("auth.user_roles", "admin");
    let out = dispatch_admin(&ctx, del, del_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["deleted"], true);

    // Verify it's gone
    let (get, get_input) = admin_get_msg(&format!("/admin/b/products/products/{id}"));
    let out = dispatch_admin(&ctx, get, get_input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

// ============================================================
// Admin Group CRUD
// ============================================================

#[tokio::test]
async fn admin_create_and_list_groups() {
    let ctx = ctx().await;

    let (create, create_input) = admin_create_msg(
        "/admin/b/products/groups",
        serde_json::json!({
            "name": "Electronics"
        }),
    );
    let out = dispatch_admin(&ctx, create, create_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["data"]["name"], "Electronics");
    assert_eq!(body["data"]["user_id"], "admin_1");

    let (list, list_input) = admin_get_msg("/admin/b/products/groups");
    let list_out = dispatch_admin(&ctx, list, list_input).await;
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
        "/admin/b/products/types",
        serde_json::json!({
            "name": "subscription", "display_name": "Subscription"
        }),
    );
    dispatch_admin(&ctx, create, create_input).await;

    let (list, list_input) = admin_get_msg("/admin/b/products/types");
    let out = dispatch_admin(&ctx, list, list_input).await;
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

    let (msg, input) = admin_get_msg("/admin/b/products/stats");
    let out = dispatch_admin(&ctx, msg, input).await;
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
        ("order_nzd", "dp_admin_nzd", "needs_response", "NZD", 700),
        ("order_usd", "dp_admin_usd", "lost", "USD", 900),
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
                status: status.to_string(),
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

    let (msg, input) = admin_get_msg("/admin/b/products/stats");
    let body = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
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
            "under_review",
            700,
        ),
        (
            "seller_order_2",
            "seller_stats_2",
            "dp_seller_stats_2",
            "lost",
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
                status: status.to_string(),
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

    let (msg, input) = get_msg("/b/products/seller/stats", "seller_user_1");
    let stats = output_to_json(dispatch_user(&ctx, msg, input).await).await;
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

    let (msg, input) = get_msg("/b/products/seller/orders", "seller_user_1");
    let orders = output_to_json(dispatch_user(&ctx, msg, input).await).await;
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

    let (msg, input) = get_msg("/b/products/seller/orders/seller_order_2", "seller_user_1");
    assert!(
        output_is_error(
            dispatch_user(&ctx, msg, input).await,
            ErrorCode::PermissionDenied
        )
        .await
    );

    let (msg, input) = create_msg(
        "/b/products/seller/orders/seller_order_2/refund",
        "seller_user_1",
        serde_json::json!({"amount_minor": 1000}),
    );
    assert!(
        output_is_error(
            dispatch_user(&ctx, msg, input).await,
            ErrorCode::PermissionDenied
        )
        .await
    );

    let (msg, input) = create_msg(
        "/b/products/seller/orders/seller_order_1/refund",
        "seller_user_1",
        serde_json::json!({"amount_minor": 1200, "note": "Customer request"}),
    );
    let refund = output_to_json(dispatch_user(&ctx, msg, input).await).await;
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

    let (msg, input) = admin_get_msg("/admin/b/products/stats");
    let out = dispatch_admin(&ctx, msg, input).await;
    assert!(
        output_is_error(out, ErrorCode::Internal).await,
        "a genuine repository failure must surface as Internal, not a fabricated all-zero stats body"
    );
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
    let group_out = dispatch_user(&ctx, create_group, cg_input).await;
    let group_id = output_to_json(group_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Create a product in that group
    let (create_prod, cp_input) = create_msg(
        "/b/products/products",
        "user_1",
        serde_json::json!({
            "name": "Widget",
            "group_id": group_id
        }),
    );
    let out = dispatch_user(&ctx, create_prod, cp_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["data"]["name"], "Widget");
    assert_eq!(body["data"]["created_by"], "user_1");
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
    let group_out = dispatch_user(&ctx, create_group, cg_input).await;
    let group_id = output_to_json(group_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // user_2 tries to create a product in user_1's group
    let (create_prod, cp_input) = create_msg(
        "/b/products/products",
        "user_2",
        serde_json::json!({
            "name": "Sneaky Product",
            "group_id": group_id
        }),
    );
    let out = dispatch_user(&ctx, create_prod, cp_input).await;
    assert!(output_is_error(out, ErrorCode::InvalidArgument).await);
}

#[tokio::test]
async fn user_cannot_see_other_users_products() {
    let ctx = user_products_ctx().await;

    // user_1 creates a product
    let (create, create_input) = create_msg(
        "/b/products/products",
        "user_1",
        serde_json::json!({
            "name": "Private Product"
        }),
    );
    let create_out = dispatch_user(&ctx, create, create_input).await;
    let prod_id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // user_2 tries to get it
    let (get, get_input) = get_msg(&format!("/b/products/products/{prod_id}"), "user_2");
    let out = dispatch_user(&ctx, get, get_input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn user_cannot_update_other_users_products() {
    let ctx = user_products_ctx().await;

    let (create, create_input) = create_msg(
        "/b/products/products",
        "user_1",
        serde_json::json!({
            "name": "My Product"
        }),
    );
    let create_out = dispatch_user(&ctx, create, create_input).await;
    let prod_id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (update, update_input) = update_msg(
        &format!("/b/products/products/{prod_id}"),
        "user_2",
        serde_json::json!({
            "name": "Hijacked!"
        }),
    );
    let out = dispatch_user(&ctx, update, update_input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn user_cannot_delete_other_users_products() {
    let ctx = user_products_ctx().await;

    let (create, create_input) = create_msg(
        "/b/products/products",
        "user_1",
        serde_json::json!({
            "name": "My Product"
        }),
    );
    let create_out = dispatch_user(&ctx, create, create_input).await;
    let prod_id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (del, del_input) = delete_msg(&format!("/b/products/products/{prod_id}"), "user_2");
    let out = dispatch_user(&ctx, del, del_input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn user_list_only_own_products() {
    let ctx = user_products_ctx().await;

    // user_1 creates a product
    let (c1, c1_input) = create_msg(
        "/b/products/products",
        "user_1",
        serde_json::json!({"name": "U1 Product"}),
    );
    dispatch_user(&ctx, c1, c1_input).await;

    // user_2 creates a product
    let (c2, c2_input) = create_msg(
        "/b/products/products",
        "user_2",
        serde_json::json!({"name": "U2 Product"}),
    );
    dispatch_user(&ctx, c2, c2_input).await;

    // user_1 lists — should only see their own
    let (list, list_input) = get_msg("/b/products/products", "user_1");
    let out = dispatch_user(&ctx, list, list_input).await;
    let body = output_to_json(out).await;
    let records = body["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["data"]["name"], "U1 Product");
}

#[tokio::test]
async fn user_update_prevents_ownership_change() {
    let ctx = user_products_ctx().await;

    let (create, create_input) = create_msg(
        "/b/products/products",
        "user_1",
        serde_json::json!({"name": "Mine"}),
    );
    let create_out = dispatch_user(&ctx, create, create_input).await;
    let prod_id = output_to_json(create_out).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Try to change created_by — should be stripped
    let (update, update_input) = update_msg(
        &format!("/b/products/products/{prod_id}"),
        "user_1",
        serde_json::json!({
            "name": "Updated",
            "created_by": "attacker"
        }),
    );
    let out = dispatch_user(&ctx, update, update_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["data"]["created_by"], "user_1");
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
    dispatch_user(&ctx, g1, g1_input).await;

    let (g2, g2_input) = create_msg(
        "/b/products/groups",
        "user_2",
        serde_json::json!({"name": "U2 Group"}),
    );
    dispatch_user(&ctx, g2, g2_input).await;

    let (list, list_input) = get_msg("/b/products/groups", "user_1");
    let out = dispatch_user(&ctx, list, list_input).await;
    let body = output_to_json(out).await;
    let records = body["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["data"]["name"], "U1 Group");
}

#[tokio::test]
async fn user_cannot_update_other_users_group() {
    let ctx = user_products_ctx().await;

    let (create, create_input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "My Group"}),
    );
    let create_out = dispatch_user(&ctx, create, create_input).await;
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
    let out = dispatch_user(&ctx, update, update_input).await;
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
    let create_out = dispatch_user(&ctx, create, create_input).await;
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
    let out = dispatch_user(&ctx, update, update_input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["data"]["user_id"], "user_1");
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
    let out = dispatch_user(&ctx, msg, input).await;
    let body = output_to_json(out).await;
    let records = body["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["data"]["name"], "Active");
}

#[tokio::test]
async fn catalog_get_hides_non_active() {
    let ctx = ctx().await;

    let mut d = HashMap::new();
    d.insert("name".to_string(), serde_json::json!("Hidden"));
    d.insert("status".to_string(), serde_json::json!("draft"));
    seed(&ctx, "impresspress__products__products", "p_hidden", d).await;

    let (msg, input) = get_msg("/b/products/catalog/p_hidden", "");
    let out = dispatch_user(&ctx, msg, input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
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
    let gr = dispatch_user(&ctx, cg, cg_input).await;
    let gid = output_to_json(gr).await["id"].as_str().unwrap().to_string();

    // Create product in group
    let (cp, cp_input) = create_msg(
        "/b/products/products",
        "user_1",
        serde_json::json!({
            "name": "In Group",
            "group_id": gid
        }),
    );
    dispatch_user(&ctx, cp, cp_input).await;

    // List products in group
    let (list, list_input) = get_msg(&format!("/b/products/groups/{gid}/products"), "user_1");
    let out = dispatch_user(&ctx, list, list_input).await;
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
    let gr = dispatch_user(&ctx, cg, cg_input).await;
    let gid = output_to_json(gr).await["id"].as_str().unwrap().to_string();

    // user_2 tries to list user_1's group products
    let (list, list_input) = get_msg(&format!("/b/products/groups/{gid}/products"), "user_2");
    let out = dispatch_user(&ctx, list, list_input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

// ============================================================
// User products disabled by default
// ============================================================

#[tokio::test]
async fn user_products_rejected_when_disabled() {
    let ctx = ctx().await; // no ALLOW_USER_PRODUCTS config → defaults to false

    let (create, create_input) = create_msg(
        "/b/products/products",
        "user_1",
        serde_json::json!({"name": "Test"}),
    );
    let out = dispatch_user(&ctx, create, create_input).await;
    assert!(output_is_error(out, ErrorCode::PermissionDenied).await);

    let (list, list_input) = get_msg("/b/products/products", "user_1");
    let out = dispatch_user(&ctx, list, list_input).await;
    assert!(output_is_error(out, ErrorCode::PermissionDenied).await);

    let (group, group_input) = create_msg(
        "/b/products/groups",
        "user_1",
        serde_json::json!({"name": "Group"}),
    );
    let out = dispatch_user(&ctx, group, group_input).await;
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
    let out = dispatch_user(&ctx, msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["records"].as_array().unwrap().len(), 1);
}

// ============================================================
// Not-found routes
// ============================================================

#[tokio::test]
async fn unknown_admin_route() {
    let ctx = ctx().await;
    let (msg, input) = admin_get_msg("/admin/b/products/nonexistent");
    let out = dispatch_admin(&ctx, msg, input).await;
    assert!(output_is_error(out, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn unknown_user_route() {
    let ctx = ctx().await;
    let (msg, input) = get_msg("/b/products/nonexistent", "user_1");
    let out = dispatch_user(&ctx, msg, input).await;
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
        "/admin/b/products/products",
        serde_json::json!({ "name": "Cloud Hosting" }),
    );
    dispatch_admin(&ctx, c, c_input).await;

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
    assert!(html.contains("Condition"));
    assert!(html.contains("Checkout options"));
    assert!(html.contains("Create and publish"));
    assert!(html.contains(r#"value="NZD""#));
    assert!(html.contains(r#"id="wizard-automatic-tax" type="checkbox" checked"#));
    assert!(html.contains("/b/products/api/admin/products"));
    assert!(html.contains("BigInt"), "money conversion must be exact");
    assert!(html.contains("wizardCurrencyExponent"));
    assert!(html.contains("unit_amount_minor"));
    assert!(html.contains(r#"value="graduated""#));
    assert!(html.contains(r#"value="volume""#));
    assert!(html.contains(r#"value="package""#));
    assert!(html.contains("wizardParseTiers"));
    assert!(html.contains("wizardParseLookup"));
    assert!(html.contains("wizardParseShippingCountries"));
    assert!(html.contains("wizardParseShippingOptions"));
    assert!(html.contains(r#"id="wizard-shipping-countries""#));
    assert!(html.contains(r#"value="NZ""#));
    assert!(html.contains("Inline rates work in hosted and embedded Checkout"));
    assert!(html.contains("Create a Stripe Customer for one-time payments"));
    assert!(html.contains("upper bound | unit amount | flat amount"));
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
        "/admin/b/products/products",
        serde_json::json!({
            "name": "Managed plan",
            "slug": "managed-plan",
            "description": "Lifecycle test",
            "currency": "NZD",
            "fulfillment_kind": "entitlement"
        }),
    );
    let product = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    let product_id = product["id"].as_str().unwrap();
    let offer_collection = format!("/admin/b/products/products/{product_id}/offers");
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
    let active = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    let active_id = active["offer"]["id"].as_str().unwrap();
    let (msg, input) = admin_create_msg(
        &format!("{offer_collection}/{active_id}/publish"),
        serde_json::json!({}),
    );
    output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    let (msg, input) = admin_create_msg(&offer_collection, definition("Editable price"));
    let draft = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
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
    assert!(html.contains("navigator.clipboard"));

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
        "/b/products/products",
        "seller_owner",
        serde_json::json!({
            "name": "Seller product",
            "slug": "seller-product",
            "currency": "USD"
        }),
    );
    let product = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
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
        "/admin/b/products/products",
        serde_json::json!({
            "name": "Original product",
            "slug": "original-product",
            "description": "Keep this description",
            "currency": "NZD",
            "fulfillment_kind": "download"
        }),
    );
    let source = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    let source_id = source["id"].as_str().unwrap();
    let collection = format!("/admin/b/products/products/{source_id}/offers");
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
    let current = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    let current_id = current["offer"]["id"].as_str().unwrap();
    let (msg, input) = admin_create_msg(
        &format!("{collection}/{current_id}/publish"),
        serde_json::json!({}),
    );
    output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    let (msg, input) = admin_create_msg(&collection, offer_definition("Old price", 1999));
    let old = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    let old_id = old["offer"]["id"].as_str().unwrap();
    let (mut msg, input) = delete_msg(&format!("{collection}/{old_id}"), "admin_1");
    msg.set_meta("auth.user_roles", "admin");
    output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;

    let (msg, input) = admin_create_msg(
        &format!("/admin/b/products/products/{source_id}/duplicate"),
        serde_json::json!({}),
    );
    let duplicated = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    let copy = &duplicated["product"];
    assert_ne!(copy["id"], source["id"]);
    assert_eq!(copy["data"]["name"], "Original product copy");
    assert!(copy["data"]["slug"]
        .as_str()
        .unwrap()
        .starts_with("original-product-copy-"));
    assert_eq!(copy["data"]["description"], "Keep this description");
    assert_eq!(copy["data"]["status"], "draft");
    assert_eq!(copy["data"]["owner_kind"], "platform");
    assert_eq!(copy["data"]["approval_status"], "approved");
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
        "/b/products/products",
        "seller_a",
        serde_json::json!({"name": "Owned product", "slug": "owned-product"}),
    );
    let source = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    let source_id = source["id"].as_str().unwrap();
    let path = format!("/b/products/products/{source_id}/duplicate");

    let (msg, input) = create_msg(&path, "seller_b", serde_json::json!({}));
    assert!(
        output_is_error(
            dispatch_user(&test_ctx, msg, input).await,
            ErrorCode::NotFound
        )
        .await
    );

    let (msg, input) = create_msg(&path, "seller_a", serde_json::json!({}));
    let duplicated = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    assert_eq!(duplicated["product"]["data"]["owner_kind"], "user");
    assert_eq!(duplicated["product"]["data"]["owner_id"], "seller_a");
    assert_eq!(duplicated["product"]["data"]["created_by"], "seller_a");
    assert_eq!(duplicated["product"]["data"]["status"], "draft");
    assert_eq!(duplicated["product"]["data"]["approval_status"], "draft");
}

#[tokio::test]
async fn admin_wizard_sequence_creates_and_publishes_subscription_offer() {
    let test_ctx = ctx().await;
    let (msg, input) = admin_create_msg(
        "/admin/b/products/products",
        serde_json::json!({
            "name": "Team plan",
            "slug": "team-plan",
            "description": "Monthly access",
            "currency": "NZD",
            "fulfillment_kind": "entitlement",
            "product_template_id": "simple_subscription"
        }),
    );
    let product = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    let product_id = product["id"].as_str().unwrap();
    assert_eq!(product["data"]["status"], "draft");

    let offer_collection = format!("/admin/b/products/products/{product_id}/offers");
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
    let managed = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
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
    let published = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(published["status"], "active");
    let (msg, input) = update_msg(
        &format!("/admin/b/products/products/{product_id}"),
        "admin_1",
        serde_json::json!({"status": "active"}),
    );
    let active_product = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(active_product["data"]["status"], "active");

    let (msg, input) = get_msg(&format!("/b/products/storefront/{product_id}"), "");
    let storefront = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
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
        "/b/products/products",
        "seller_wizard",
        serde_json::json!({
            "name": "Custom engraving",
            "slug": "custom-engraving",
            "currency": "USD",
            "fulfillment_kind": "manual",
            "product_template_id": "configurable_product"
        }),
    );
    let product = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    let product_id = product["id"].as_str().unwrap();
    assert_eq!(product["data"]["owner_id"], "seller_wizard");
    assert_eq!(product["data"]["approval_status"], "draft");

    let offer_collection = format!("/b/products/products/{product_id}/offers");
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
    let managed = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    let offer_id = managed["offer"]["id"].as_str().unwrap();
    assert_eq!(managed["offer"]["pricing_model"], "components");

    let (msg, input) = create_msg(
        &format!("{offer_collection}/{offer_id}/publish"),
        "seller_wizard",
        serde_json::json!({}),
    );
    let offer = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    assert_eq!(offer["status"], "active");
    let (msg, input) = update_msg(
        &format!("/b/products/products/{product_id}"),
        "seller_wizard",
        serde_json::json!({"status": "active"}),
    );
    let pending = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    assert_eq!(pending["data"]["status"], "pending_review");
    assert_eq!(pending["data"]["approval_status"], "pending");

    let (msg, input) = get_msg(&format!("/b/products/storefront/{product_id}"), "");
    assert!(
        output_is_error(
            dispatch_user(&test_ctx, msg, input).await,
            ErrorCode::NotFound
        )
        .await,
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
        "/admin/b/products/products",
        serde_json::json!({ "name": "Widget" }),
    );
    dispatch_admin(&ctx, c, c_input).await;

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
    assert!(html.contains("/b/products/api/admin/webhook-events"));
    assert!(html.contains("replayStripeWebhookEvent"));
    assert!(html.contains("Provider reconciliation"));
    assert!(html.contains("Reconcile due operations"));
    assert!(html.contains("/b/products/api/admin/provider-operations"));
    assert!(html.contains("reconcileStripeProviderOperations"));
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

    let (list, list_input) = admin_get_msg("/admin/b/products/webhook-events");
    let body = output_to_json(dispatch_admin(&ctx, list, list_input).await).await;
    assert_eq!(body["total_count"], 1);
    assert_eq!(body["records"][0]["id"], "evt_route_replay");
    let encoded = serde_json::to_string(&body).unwrap();
    assert!(!encoded.contains("pi_route_private"));
    assert!(!encoded.contains("private-owner-token"));
    assert!(!encoded.contains("payload_base64"));
    assert!(!encoded.contains("payload_sha256"));

    let (replay, replay_input) = admin_create_msg(
        "/admin/b/products/webhook-events/evt_route_replay/replay",
        serde_json::json!({}),
    );
    let replayed = output_to_json(dispatch_admin(&ctx, replay, replay_input).await).await;
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
