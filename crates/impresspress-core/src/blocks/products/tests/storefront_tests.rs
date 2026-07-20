use std::collections::HashMap;

use wafer_core::clients::database as db;
use wafer_run::{AuthLevel, Block, ErrorCode, MetaGet, META_RESP_CONTENT_TYPE};

use super::harness::*;
use crate::{
    blocks::products::{pages, repo::purchases::PURCHASES_TABLE, stripe, ProductsBlock},
    util::sha256_hex,
};

#[test]
fn storefront_browser_routes_are_explicitly_public() {
    let info = ProductsBlock::new().info();
    for path in [
        "/b/products/storefront.js",
        "/b/products/storefront/config",
        "/b/products/storefront/product_1",
        "/b/products/orders/order_1/status",
    ] {
        assert_eq!(
            crate::endpoint_match::endpoint_auth(&info.endpoints, "retrieve", path),
            Some(AuthLevel::Public),
            "{path} must remain usable from a guest static page"
        );
    }
    assert_eq!(
        crate::endpoint_match::endpoint_auth(&info.endpoints, "create", "/b/products/webhooks"),
        Some(AuthLevel::Public),
        "Stripe webhooks must remain reachable without a browser session"
    );
}

#[tokio::test]
async fn storefront_config_exposes_only_a_valid_matching_publishable_key() {
    let ctx = ctx_with(&[
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_server_only",
        ),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY",
            "pk_test_browser_safe",
        ),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
            "whsec_never_expose",
        ),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_API_URL",
            "https://internal-stripe.invalid",
        ),
    ])
    .await;
    let (msg, input) = get_msg("/b/products/storefront/config", "");
    let body = output_to_json(dispatch_user(&ctx, msg, input).await).await;
    assert_eq!(body["embedded_checkout_available"], true);
    assert_eq!(body["stripe_publishable_key"], "pk_test_browser_safe");
    assert_eq!(body["stripe_mode"], "test");
    let encoded = body.to_string();
    assert!(!encoded.contains("sk_test_server_only"));
    assert!(!encoded.contains("whsec_never_expose"));
    assert!(!encoded.contains("internal-stripe"));

    let mismatch = ctx_with(&[
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_live_server_only",
        ),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY",
            "pk_test_browser_safe",
        ),
    ])
    .await;
    let (msg, input) = get_msg("/b/products/storefront/config", "");
    let body = output_to_json(dispatch_user(&mismatch, msg, input).await).await;
    assert_eq!(body["embedded_checkout_available"], false);

    let invalid = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY",
        "not-a-publishable-key",
    )])
    .await;
    let (msg, input) = get_msg("/b/products/storefront/config", "");
    let body = output_to_json(dispatch_user(&invalid, msg, input).await).await;
    assert_eq!(body["embedded_checkout_available"], false);
    assert!(body.get("stripe_publishable_key").is_none());
    assert!(body.get("stripe_mode").is_none());
}

#[tokio::test]
async fn browser_runtime_hides_secret_settings_and_rejects_stripe_secret_operations() {
    let ctx = ctx_with(&[
        (crate::blocks::products::RUNTIME_KIND_CONFIG_KEY, "browser"),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_must_not_run",
        ),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY",
            "pk_test_browser_safe",
        ),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
            "whsec_must_not_run",
        ),
    ])
    .await;

    let (msg, input) = get_msg("/b/products/storefront/config", "");
    let config = output_to_json(dispatch_user(&ctx, msg, input).await).await;
    assert_eq!(config["embedded_checkout_available"], false);

    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({"offer_id": "does_not_matter"}),
    );
    assert!(
        output_is_error(
            stripe::handle_checkout(&ctx, &msg, input).await,
            ErrorCode::PermissionDenied,
        )
        .await,
        "browser checkout must fail before offer lookup or provider access"
    );

    let (msg, input) = create_msg("/b/products/webhooks", "", serde_json::json!({}));
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::PermissionDenied,
        )
        .await,
        "the browser runtime must not hold or process Stripe webhook secrets"
    );

    let (msg, _) = admin_get_msg("/b/products/admin/settings");
    let html = output_to_html(pages::settings(&ctx, &msg).await).await;
    assert!(html.contains("Browser runtime safety"));
    assert!(!html.contains("name=\"IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY\""));
    assert!(!html.contains("name=\"IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET\""));
    assert!(!html.contains("name=\"IMPRESSPRESS__PRODUCTS__WEBHOOK_SECRET\""));
}

#[tokio::test]
async fn storefront_widget_is_javascript_and_uses_all_three_stripe_presentations() {
    let ctx = ctx().await;
    let (msg, input) = get_msg("/b/products/storefront.js", "");
    let response = dispatch_user(&ctx, msg, input)
        .await
        .collect_buffered()
        .await
        .expect("widget response");
    assert_eq!(
        MetaGet::get(&response.meta, META_RESP_CONTENT_TYPE),
        Some("application/javascript; charset=utf-8")
    );
    assert_eq!(
        MetaGet::get(&response.meta, "resp.header.X-Content-Type-Options"),
        Some("nosniff")
    );
    let script = String::from_utf8(response.body).expect("utf-8 widget");
    for fragment in [
        "customElements.define(\"impresspress-product\"",
        "/b/products/pricing/preview",
        "/b/products/checkout",
        "window.location.assign(link.url)",
        "initEmbeddedCheckout",
        "receipt_token",
        "textContent",
        "https://js.stripe.com/clover/stripe.js",
    ] {
        assert!(script.contains(fragment), "widget is missing {fragment}");
    }
}

#[tokio::test]
async fn guest_order_status_requires_an_unexpired_receipt_and_returns_a_minimal_projection() {
    let ctx = ctx().await;
    let token = "guest-receipt-token";
    seed(
        &ctx,
        PURCHASES_TABLE,
        "order_guest_receipt",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("")),
            ("buyer_user_id".to_string(), serde_json::json!("")),
            (
                "buyer_email".to_string(),
                serde_json::json!("private@example.com"),
            ),
            ("status".to_string(), serde_json::json!("completed")),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("reconciled"),
            ),
            ("subtotal_cents".to_string(), serde_json::json!(1234)),
            ("discount_cents".to_string(), serde_json::json!(100)),
            ("tax_cents".to_string(), serde_json::json!(66)),
            ("platform_fee_cents".to_string(), serde_json::json!(25)),
            ("total_cents".to_string(), serde_json::json!(1200)),
            ("currency".to_string(), serde_json::json!("JPY")),
            (
                "provider_session_id".to_string(),
                serde_json::json!("cs_private"),
            ),
            (
                "receipt_token_hash".to_string(),
                serde_json::json!(sha256_hex(token.as_bytes())),
            ),
            (
                "receipt_token_expires_at".to_string(),
                serde_json::json!((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
            ),
            (
                "payment_at".to_string(),
                serde_json::json!("2026-07-19T01:02:03Z"),
            ),
        ]),
    )
    .await;

    let (mut msg, input) = get_msg("/b/products/orders/order_guest_receipt/status", "");
    msg.set_meta("req.query.receipt_token", token);
    let body = output_to_json(dispatch_user(&ctx, msg, input).await).await;
    assert_eq!(body["order_id"], "order_guest_receipt");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["amounts"]["currency"], "JPY");
    assert_eq!(body["amounts"]["total_minor"], 1200);
    assert_eq!(body["paid_at"], "2026-07-19T01:02:03Z");
    let encoded = body.to_string();
    for secret in [
        "private@example.com",
        "cs_private",
        "guest-receipt-token",
        "receipt_token_hash",
        "provider_session_id",
    ] {
        assert!(!encoded.contains(secret), "guest response leaked {secret}");
    }

    let (mut msg, input) = get_msg("/b/products/orders/order_guest_receipt/status", "");
    msg.set_meta("req.query.receipt_token", "wrong-token");
    assert!(
        output_is_error(dispatch_user(&ctx, msg, input).await, ErrorCode::NotFound).await,
        "a wrong capability must not reveal whether the order exists"
    );

    db::update(
        &ctx,
        PURCHASES_TABLE,
        "order_guest_receipt",
        HashMap::from([(
            "receipt_token_expires_at".to_string(),
            serde_json::json!((chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339()),
        )]),
    )
    .await
    .expect("expire receipt");
    let (mut msg, input) = get_msg("/b/products/orders/order_guest_receipt/status", "");
    msg.set_meta("req.query.receipt_token", token);
    assert!(output_is_error(dispatch_user(&ctx, msg, input).await, ErrorCode::NotFound).await);
}

#[tokio::test]
async fn anonymous_commerce_routes_have_independent_ip_rate_limits() {
    let ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__RATE_LIMIT_PRODUCTS_PREVIEW", "1/60"),
        ("WAFER_RUN_SHARED__RATE_LIMIT_PRODUCTS_CHECKOUT", "1/60"),
        ("WAFER_RUN_SHARED__RATE_LIMIT_PRODUCTS_RECEIPT", "1/60"),
    ])
    .await;
    let block = ProductsBlock::new();

    async fn assert_second_request_is_limited(
        block: &ProductsBlock,
        ctx: &crate::test_support::TestContext,
        build: impl Fn() -> (wafer_run::Message, wafer_run::InputStream),
    ) {
        let (mut first, input) = build();
        first.set_meta("req.client.ip", "203.0.113.42");
        assert!(
            !output_is_error(
                block.handle(ctx, first, input).await,
                ErrorCode::ResourceExhausted,
            )
            .await,
            "the first request in a public commerce bucket must be allowed"
        );

        let (mut second, input) = build();
        second.set_meta("req.client.ip", "203.0.113.42");
        assert!(
            output_is_error(
                block.handle(ctx, second, input).await,
                ErrorCode::ResourceExhausted,
            )
            .await,
            "the second request must be rejected by the configured 1/minute limit"
        );
    }

    assert_second_request_is_limited(&block, &ctx, || {
        create_msg(
            "/b/products/pricing/preview",
            "",
            serde_json::json!({"offer_id": "missing"}),
        )
    })
    .await;
    assert_second_request_is_limited(&block, &ctx, || {
        create_msg(
            "/b/products/checkout",
            "",
            serde_json::json!({"offer_id": "missing"}),
        )
    })
    .await;
    assert_second_request_is_limited(&block, &ctx, || {
        get_msg("/b/products/orders/missing/status", "")
    })
    .await;
}
