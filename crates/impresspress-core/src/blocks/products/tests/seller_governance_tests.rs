use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_core::clients::database as db;
use wafer_run::{AuthLevel, Block, ErrorCode, OutputStream};

use super::super::{contracts::OfferStatus, repo, ProductsBlock, PRODUCTS_TABLE};
use super::harness::{
    admin_create_msg, admin_get_msg, create_msg, ctx_with, dispatch_admin, dispatch_user, get_msg,
    output_is_error, output_to_html, output_to_json, seed, update_msg,
};

fn fixed_offer() -> Value {
    json!({
        "name": "Seller listing price",
        "mode": "payment",
        "currency": "nzd",
        "pricing_model": "fixed",
        "usage_type": "licensed",
        "billing_scheme": "per_unit",
        "tax_behavior": "exclusive",
        "components": [{
            "key": "listing",
            "label": "Seller listing",
            "required": true,
            "amount": {"type": "fixed", "unit_amount_minor": 2500}
        }]
    })
}

async fn seed_seller(
    test_ctx: &crate::test_support::TestContext,
    id: &str,
    user_id: &str,
    ready: bool,
) {
    seed(
        test_ctx,
        repo::seller_accounts::TABLE,
        id,
        HashMap::from([
            ("user_id".to_string(), json!(user_id)),
            (
                "status".to_string(),
                json!(if ready { "active" } else { "onboarding" }),
            ),
            ("stripe_account_id".to_string(), json!(format!("acct_{id}"))),
            ("details_submitted".to_string(), json!(ready)),
            ("charges_enabled".to_string(), json!(ready)),
            ("payouts_enabled".to_string(), json!(ready)),
            (
                "requirements_json".to_string(),
                json!(if ready {
                    "{}"
                } else {
                    r#"{"currently_due":["individual.verification.document"]}"#
                }),
            ),
            ("fee_basis_points".to_string(), json!(250)),
        ]),
    )
    .await;
}

async fn create_seller_product(
    test_ctx: &crate::test_support::TestContext,
    user_id: &str,
    name: &str,
) -> String {
    let (msg, input) = create_msg(
        "/b/products/products",
        user_id,
        json!({"name": name, "slug": name.to_lowercase().replace(' ', "-")}),
    );
    output_to_json(dispatch_user(test_ctx, msg, input).await).await["id"]
        .as_str()
        .expect("seller product id")
        .to_string()
}

async fn submit_product(
    test_ctx: &crate::test_support::TestContext,
    user_id: &str,
    product_id: &str,
) -> Value {
    let (msg, input) = update_msg(
        &format!("/b/products/products/{product_id}"),
        user_id,
        json!({"status": "active"}),
    );
    output_to_json(dispatch_user(test_ctx, msg, input).await).await
}

async fn terminal_error(out: OutputStream) -> wafer_run::WaferError {
    use wafer_run::streams::output::TerminalNotResponse;

    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(error)) => error,
        other => panic!("expected terminal error, got {other:?}"),
    }
}

#[tokio::test]
async fn admin_moderation_approves_rejects_and_resubmits_only_ready_sellers() {
    let test_ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    seed_seller(&test_ctx, "seller_ready", "maker_ready", true).await;

    let approved_id = create_seller_product(&test_ctx, "maker_ready", "Approved print").await;
    let pending = submit_product(&test_ctx, "maker_ready", &approved_id).await;
    assert_eq!(pending["data"]["status"], "pending_review");
    assert_eq!(pending["data"]["approval_status"], "pending");

    let path = format!("/admin/b/products/products/{approved_id}/approve");
    let (msg, input) = admin_create_msg(&path, json!({}));
    let approved = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(approved["data"]["status"], "active");
    assert_eq!(approved["data"]["approval_status"], "approved");
    assert!(approved["data"]["published_at"].as_str().is_some());

    let (msg, input) = admin_create_msg(&path, json!({}));
    let repeated = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(repeated["data"]["status"], "active");

    let rejected_id = create_seller_product(&test_ctx, "maker_ready", "Needs changes").await;
    submit_product(&test_ctx, "maker_ready", &rejected_id).await;
    let (msg, input) = admin_create_msg(
        &format!("/admin/b/products/products/{rejected_id}/reject"),
        json!({}),
    );
    let rejected = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(rejected["data"]["status"], "draft");
    assert_eq!(rejected["data"]["approval_status"], "rejected");
    let resubmitted = submit_product(&test_ctx, "maker_ready", &rejected_id).await;
    assert_eq!(resubmitted["data"]["status"], "pending_review");
    assert_eq!(resubmitted["data"]["approval_status"], "pending");

    seed_seller(&test_ctx, "seller_not_ready", "maker_not_ready", false).await;
    let blocked_id = create_seller_product(&test_ctx, "maker_not_ready", "Blocked print").await;
    submit_product(&test_ctx, "maker_not_ready", &blocked_id).await;
    let (msg, input) = admin_create_msg(
        &format!("/admin/b/products/products/{blocked_id}/approve"),
        json!({}),
    );
    assert!(
        output_is_error(
            dispatch_admin(&test_ctx, msg, input).await,
            ErrorCode::AlreadyExists,
        )
        .await
    );
    let unchanged = db::get(&test_ctx, PRODUCTS_TABLE, &blocked_id)
        .await
        .expect("blocked product");
    assert_eq!(unchanged.data["status"], "pending_review");
}

#[tokio::test]
async fn suspension_archives_catalog_blocks_mutations_and_reactivation_stays_draft() {
    let test_ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        (
            "IMPRESSPRESS__PRODUCTS__SELLER_MODERATION_REQUIRED",
            "false",
        ),
    ])
    .await;
    seed_seller(&test_ctx, "seller_governed", "maker_governed", true).await;
    let product_id = create_seller_product(&test_ctx, "maker_governed", "Governed print").await;
    let published = submit_product(&test_ctx, "maker_governed", &product_id).await;
    assert_eq!(published["data"]["status"], "active");

    let offers_path = format!("/b/products/products/{product_id}/offers");
    let (msg, input) = create_msg(&offers_path, "maker_governed", fixed_offer());
    let offer = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    let offer_id = offer["offer"]["id"].as_str().expect("offer id");
    let (msg, input) = create_msg(
        &format!("{offers_path}/{offer_id}/publish"),
        "maker_governed",
        json!({}),
    );
    let offer = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    assert_eq!(offer["status"], "active");

    let (msg, input) = admin_create_msg(
        "/admin/b/products/sellers/seller_governed/suspend",
        json!({}),
    );
    let suspended = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(suspended["status"], "suspended");
    assert_eq!(suspended["approval_status"], "suspended");
    assert!(
        db::get(&test_ctx, repo::seller_accounts::TABLE, "seller_governed")
            .await
            .expect("suspended seller")
            .data["suspended_at"]
            .as_str()
            .is_some()
    );

    let product = db::get(&test_ctx, PRODUCTS_TABLE, &product_id)
        .await
        .expect("governed product");
    assert_eq!(product.data["status"], "archived");
    assert_eq!(product.data["approval_status"], "suspended");
    let offers = repo::offers::list_for_product(&test_ctx, &product_id)
        .await
        .expect("offers");
    assert_eq!(offers[0].status, OfferStatus::Archived);

    let (msg, input) = update_msg(
        &format!("/b/products/products/{product_id}"),
        "maker_governed",
        json!({"name": "Bypass attempt"}),
    );
    assert!(
        output_is_error(
            dispatch_user(&test_ctx, msg, input).await,
            ErrorCode::PermissionDenied,
        )
        .await
    );
    let (msg, input) = create_msg(
        "/b/products/products",
        "maker_governed",
        json!({"name": "Second bypass"}),
    );
    assert!(
        output_is_error(
            dispatch_user(&test_ctx, msg, input).await,
            ErrorCode::PermissionDenied,
        )
        .await
    );
    let (msg, input) = get_msg("/b/products/products", "maker_governed");
    let visible = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    assert_eq!(
        visible["records"].as_array().expect("owned products").len(),
        1
    );

    let (msg, input) = admin_create_msg(
        "/admin/b/products/sellers/seller_governed/reactivate",
        json!({}),
    );
    let reactivated = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(reactivated["status"], "active");
    assert!(
        db::get(&test_ctx, repo::seller_accounts::TABLE, "seller_governed")
            .await
            .expect("reactivated seller")
            .data["suspended_at"]
            .is_null()
    );
    let product = db::get(&test_ctx, PRODUCTS_TABLE, &product_id)
        .await
        .expect("reactivated product");
    assert_eq!(product.data["status"], "draft");
    assert_eq!(product.data["approval_status"], "draft");
    assert_eq!(
        repo::offers::list_for_product(&test_ctx, &product_id)
            .await
            .expect("offers")[0]
            .status,
        OfferStatus::Archived
    );
    let new_id = create_seller_product(&test_ctx, "maker_governed", "Allowed again").await;
    assert!(!new_id.is_empty());
}

#[tokio::test]
async fn suspended_seller_cannot_issue_refunds() {
    let test_ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    seed_seller(&test_ctx, "seller_refund_gate", "maker_refund_gate", true).await;

    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), json!("buyer_1"));
    pd.insert("seller_account_id".to_string(), json!("seller_refund_gate"));
    pd.insert("status".to_string(), json!("completed"));
    pd.insert("total_cents".to_string(), json!(4000));
    seed(
        &test_ctx,
        "impresspress__products__purchases",
        "pur_seller_gate",
        pd,
    )
    .await;

    let (msg, input) = admin_create_msg(
        "/admin/b/products/sellers/seller_refund_gate/suspend",
        json!({}),
    );
    let suspended = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(suspended["status"], "suspended");

    // A platform suspension stops money-moving mutations: issuing a refund
    // is one, and must be rejected like every other gated seller mutation
    // (admins can still refund the buyer via the admin refund route).
    let (msg, input) = create_msg(
        "/b/products/seller/orders/pur_seller_gate/refund",
        "maker_refund_gate",
        json!({}),
    );
    assert!(
        output_is_error(
            dispatch_user(&test_ctx, msg, input).await,
            ErrorCode::PermissionDenied,
        )
        .await
    );
}

#[tokio::test]
async fn admin_seller_api_and_pages_expose_owned_products_and_safe_capability_state() {
    let test_ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true")]).await;
    seed_seller(&test_ctx, "seller_visible", "maker_visible", true).await;
    seed_seller(&test_ctx, "seller_other", "maker_other", true).await;
    let product_id = create_seller_product(&test_ctx, "maker_visible", "Visible print").await;
    submit_product(&test_ctx, "maker_visible", &product_id).await;
    create_seller_product(&test_ctx, "maker_other", "Other print").await;

    let (msg, input) = admin_get_msg("/admin/b/products/sellers");
    let list = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(list["sellers"].as_array().expect("sellers").len(), 2);
    let (msg, input) = admin_get_msg("/admin/b/products/sellers/seller_visible");
    let detail = output_to_json(dispatch_admin(&test_ctx, msg, input).await).await;
    assert_eq!(detail["seller"]["user_id"], "maker_visible");
    assert_eq!(detail["products"].as_array().expect("products").len(), 1);
    assert_eq!(detail["products"][0]["id"], product_id);

    let (msg, _) = admin_get_msg("/b/products/admin/sellers");
    let html = output_to_html(super::super::pages::admin_sellers(&test_ctx, &msg).await).await;
    assert!(html.contains("Moderation queue"));
    assert!(html.contains("Visible print"));
    assert!(html.contains("maker_visible"));
    assert!(html.contains("/b/products/admin/sellers/seller_visible"));

    let (msg, _) = admin_get_msg("/b/products/admin/sellers/seller_visible");
    let html = output_to_html(
        super::super::pages::admin_seller_detail(&test_ctx, &msg, "seller_visible").await,
    )
    .await;
    assert!(html.contains("Seller account"));
    assert!(html.contains("acct_seller_visible"));
    assert!(html.contains("Suspend seller"));
    assert!(html.contains("data-seller-action=\"suspend\""));
    assert!(html.contains("Visible print"));

    let (msg, _) = admin_get_msg(&format!("/b/products/admin/products/{product_id}"));
    let html = output_to_html(
        super::super::pages::product_manager(&test_ctx, &msg, &product_id, true).await,
    )
    .await;
    assert!(html.contains("Approve listing"));
    assert!(html.contains("Return to seller"));
    assert!(html.contains("productManagerModerate"));
}

#[test]
fn seller_governance_routes_are_all_admin_only() {
    let info = ProductsBlock::new().info();
    for (action, path) in [
        ("retrieve", "/b/products/admin/sellers"),
        ("retrieve", "/b/products/admin/sellers/seller_1"),
        ("retrieve", "/b/products/api/admin/sellers"),
        ("retrieve", "/b/products/api/admin/sellers/seller_1"),
        ("create", "/b/products/api/admin/sellers/seller_1/suspend"),
        (
            "create",
            "/b/products/api/admin/sellers/seller_1/reactivate",
        ),
        ("create", "/b/products/api/admin/products/product_1/approve"),
        ("create", "/b/products/api/admin/products/product_1/reject"),
    ] {
        assert_eq!(
            crate::endpoint_match::endpoint_auth(&info.endpoints, action, path),
            Some(AuthLevel::Admin),
            "{action} {path} must require an administrator"
        );
    }
}

#[tokio::test]
async fn activation_validates_merged_values_not_stale_record() {
    // A product created before the platform tightened its currency policy
    // holds a now-disallowed currency. Fixing the field and activating in the
    // same PATCH must validate the merged view, not the stale record.
    let test_ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        ("IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CURRENCIES", "nzd"),
    ])
    .await;

    let mut pd = HashMap::new();
    pd.insert("created_by".to_string(), json!("legacy_seller"));
    pd.insert("name".to_string(), json!("Legacy priced product"));
    pd.insert("product_template_id".to_string(), json!("simple_product"));
    pd.insert("currency".to_string(), json!("USD"));
    pd.insert("status".to_string(), json!("draft"));
    seed(&test_ctx, PRODUCTS_TABLE, "prod_legacy_ccy", pd).await;

    let (msg, input) = update_msg(
        "/b/products/products/prod_legacy_ccy",
        "legacy_seller",
        json!({"currency": "NZD", "status": "active"}),
    );
    let body = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    assert_eq!(body["data"]["currency"], "NZD");
    assert_eq!(body["data"]["status"], "active");

    // Activating while leaving the stale value untouched must still fail.
    let mut pd = HashMap::new();
    pd.insert("created_by".to_string(), json!("legacy_seller"));
    pd.insert("name".to_string(), json!("Still stale"));
    pd.insert("product_template_id".to_string(), json!("simple_product"));
    pd.insert("currency".to_string(), json!("USD"));
    pd.insert("status".to_string(), json!("draft"));
    seed(&test_ctx, PRODUCTS_TABLE, "prod_stale_ccy", pd).await;

    let (msg, input) = update_msg(
        "/b/products/products/prod_stale_ccy",
        "legacy_seller",
        json!({"status": "active"}),
    );
    let error = terminal_error(dispatch_user(&test_ctx, msg, input).await).await;
    assert!(
        error.message.contains("currency is not allowed"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn admin_seller_policy_is_enforced_by_apis_and_reflected_in_the_wizard() {
    let test_ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        (
            "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_TEMPLATES",
            "simple_product",
        ),
        ("IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CURRENCIES", "nzd"),
        (
            "IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CATEGORIES",
            "art,prints",
        ),
        ("IMPRESSPRESS__PRODUCTS__SELLER_MAX_PRODUCTS", "1"),
    ])
    .await;
    let (msg, input) = create_msg(
        "/b/products/products",
        "policy_seller",
        json!({
            "name": "Allowed print",
            "product_template_id": "simple_product",
            "currency": "NZD",
            "category": "Art"
        }),
    );
    let allowed = output_to_json(dispatch_user(&test_ctx, msg, input).await).await;
    let product_id = allowed["id"].as_str().expect("allowed product").to_string();

    let (msg, input) = update_msg(
        &format!("/b/products/products/{product_id}"),
        "policy_seller",
        json!({"currency": "USD"}),
    );
    let error = terminal_error(dispatch_user(&test_ctx, msg, input).await).await;
    assert!(error.message.contains("currency is not allowed"));
    let (msg, input) = create_msg(
        &format!("/b/products/products/{product_id}/duplicate"),
        "policy_seller",
        json!({}),
    );
    let error = terminal_error(dispatch_user(&test_ctx, msg, input).await).await;
    assert!(error.message.contains("product limit reached (1)"));

    let (msg, input) = create_msg(
        "/b/products/products",
        "policy_seller",
        json!({
            "name": "Over limit",
            "product_template_id": "simple_product",
            "currency": "NZD"
        }),
    );
    let error = terminal_error(dispatch_user(&test_ctx, msg, input).await).await;
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(error.message.contains("product limit reached (1)"));

    for (user_id, body, expected) in [
        (
            "bad_template",
            json!({"name":"Bad template","product_template_id":"configurable_product","currency":"NZD","category":"art"}),
            "template is not allowed",
        ),
        (
            "bad_currency",
            json!({"name":"Bad currency","product_template_id":"simple_product","currency":"USD","category":"art"}),
            "currency is not allowed",
        ),
        (
            "bad_category",
            json!({"name":"Bad category","product_template_id":"simple_product","currency":"NZD","category":"services"}),
            "category is not allowed",
        ),
    ] {
        let (msg, input) = create_msg("/b/products/products", user_id, body);
        let error = terminal_error(dispatch_user(&test_ctx, msg, input).await).await;
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(error.message.contains(expected), "{}", error.message);
    }

    let mut usd_offer = fixed_offer();
    usd_offer["currency"] = json!("usd");
    let (msg, input) = create_msg(
        &format!("/b/products/products/{product_id}/offers"),
        "policy_seller",
        usd_offer,
    );
    let error = terminal_error(dispatch_user(&test_ctx, msg, input).await).await;
    assert!(error.message.contains("currency is not allowed"));

    let (msg, input) = admin_create_msg(
        "/admin/b/products/products",
        json!({
            "name": "Unrestricted platform product",
            "product_template_id": "configurable_product",
            "currency": "USD",
            "category": "services"
        }),
    );
    assert!(
        output_to_json(dispatch_admin(&test_ctx, msg, input).await).await["id"]
            .as_str()
            .is_some()
    );

    let (msg, _) = get_msg("/b/products/my-products/new", "policy_seller");
    let html =
        output_to_html(super::super::pages::product_wizard(&test_ctx, &msg, false).await).await;
    assert!(html.contains("value=\"simple_product\""));
    assert!(!html.contains("value=\"simple_subscription\""));
    assert!(!html.contains("value=\"configurable_product\""));
    assert!(html.contains("Allowed seller currencies: NZD"));
    assert!(html.contains("id=\"wizard-currency-options\""));

    let (msg, _) = admin_get_msg("/b/products/admin/settings");
    let html = output_to_html(super::super::pages::settings(&test_ctx, &msg).await).await;
    assert!(html.contains("Seller Allowed Templates"));
    assert!(html.contains("Seller Allowed Currencies"));
    assert!(html.contains("Seller Allowed Categories"));
    assert!(html.contains("Seller Product Limit"));
}
