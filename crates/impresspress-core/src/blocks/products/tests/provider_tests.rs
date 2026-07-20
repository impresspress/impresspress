use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use wafer_core::interfaces::network::service::{NetworkError, NetworkService, Request, Response};
use wafer_run::{Block, ErrorCode};

use super::harness::*;
use crate::blocks::products::repo;

#[derive(Clone)]
struct SequencedStripeNetwork {
    requests: Arc<Mutex<Vec<Request>>>,
    responses: Arc<Mutex<VecDeque<(u16, serde_json::Value)>>>,
}

#[async_trait]
impl NetworkService for SequencedStripeNetwork {
    async fn do_request(&self, request: &Request) -> Result<Response, NetworkError> {
        self.requests.lock().unwrap().push(request.clone());
        let (status_code, response) = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| (200, serde_json::json!({})));
        Ok(Response {
            status_code,
            headers: Default::default(),
            body: serde_json::to_vec(&response).unwrap(),
        })
    }
}

fn register_sequence(
    ctx: &mut crate::test_support::TestContext,
    responses: Vec<serde_json::Value>,
) -> Arc<Mutex<Vec<Request>>> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let block: Arc<dyn Block> = Arc::new(wafer_core::service_blocks::network::NetworkBlock::new(
        Arc::new(SequencedStripeNetwork {
            requests: requests.clone(),
            responses: Arc::new(Mutex::new(
                responses
                    .into_iter()
                    .map(|response| (200, response))
                    .collect(),
            )),
        }),
    ));
    ctx.register_block("wafer-run/network", block);
    requests
}

fn register_sequence_with_status(
    ctx: &mut crate::test_support::TestContext,
    responses: Vec<(u16, serde_json::Value)>,
) -> Arc<Mutex<Vec<Request>>> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let block: Arc<dyn Block> = Arc::new(wafer_core::service_blocks::network::NetworkBlock::new(
        Arc::new(SequencedStripeNetwork {
            requests: requests.clone(),
            responses: Arc::new(Mutex::new(responses.into())),
        }),
    ));
    ctx.register_block("wafer-run/network", block);
    requests
}

#[derive(Clone, Copy)]
enum BrokenStripeResponse {
    Malformed,
    Timeout,
}

#[derive(Clone)]
struct BrokenStripeNetwork {
    requests: Arc<Mutex<Vec<Request>>>,
    response: BrokenStripeResponse,
}

#[async_trait]
impl NetworkService for BrokenStripeNetwork {
    async fn do_request(&self, request: &Request) -> Result<Response, NetworkError> {
        self.requests.lock().unwrap().push(request.clone());
        match self.response {
            BrokenStripeResponse::Malformed => Ok(Response {
                status_code: 200,
                headers: Default::default(),
                body: b"{private-invalid-json".to_vec(),
            }),
            BrokenStripeResponse::Timeout => Err(NetworkError::RequestError(
                "timed out waiting for Stripe".to_string(),
            )),
        }
    }
}

fn register_broken_response(
    ctx: &mut crate::test_support::TestContext,
    response: BrokenStripeResponse,
) -> Arc<Mutex<Vec<Request>>> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let block: Arc<dyn Block> = Arc::new(wafer_core::service_blocks::network::NetworkBlock::new(
        Arc::new(BrokenStripeNetwork {
            requests: requests.clone(),
            response,
        }),
    ));
    ctx.register_block("wafer-run/network", block);
    requests
}

fn express_account(
    id: &str,
    details_submitted: bool,
    charges_enabled: bool,
    payouts_enabled: bool,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "object": "account",
        "country": "NZ",
        "default_currency": "nzd",
        "details_submitted": details_submitted,
        "charges_enabled": charges_enabled,
        "payouts_enabled": payouts_enabled,
        "business_profile": {"name": "Example Studio"},
        "capabilities": {
            "card_payments": if charges_enabled { "active" } else { "pending" },
            "transfers": if payouts_enabled { "active" } else { "pending" }
        },
        "controller": {"stripe_dashboard": {"type": "express"}},
        "requirements": {
            "currently_due": if details_submitted { serde_json::json!([]) } else { serde_json::json!(["individual.verification.document"]) },
            "disabled_reason": if charges_enabled { serde_json::Value::Null } else { serde_json::json!("requirements.pending_verification") }
        }
    })
}

async fn seed_portal_order(
    ctx: &crate::test_support::TestContext,
    id: &str,
    buyer_user_id: &str,
    stripe_customer_id: &str,
    stripe_account_id: &str,
    livemode: bool,
) {
    seed(
        ctx,
        repo::purchases::PURCHASES_TABLE,
        id,
        std::collections::HashMap::from([
            ("user_id".to_string(), serde_json::json!(buyer_user_id)),
            (
                "buyer_user_id".to_string(),
                serde_json::json!(buyer_user_id),
            ),
            ("status".to_string(), serde_json::json!("completed")),
            ("provider".to_string(), serde_json::json!("stripe")),
            (
                "stripe_customer_id".to_string(),
                serde_json::json!(stripe_customer_id),
            ),
            (
                "stripe_account_id".to_string(),
                serde_json::json!(stripe_account_id),
            ),
            ("livemode".to_string(), serde_json::json!(livemode)),
        ]),
    )
    .await;
}

async fn seed_stripe_refund_order(
    ctx: &crate::test_support::TestContext,
    id: &str,
    status: &str,
    total_minor: i64,
    refunded_total_minor: i64,
    stripe_account_id: &str,
    livemode: bool,
) {
    seed(
        ctx,
        repo::purchases::PURCHASES_TABLE,
        id,
        std::collections::HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_refund")),
            (
                "buyer_user_id".to_string(),
                serde_json::json!("buyer_refund"),
            ),
            ("status".to_string(), serde_json::json!(status)),
            ("provider".to_string(), serde_json::json!("stripe")),
            ("total_cents".to_string(), serde_json::json!(total_minor)),
            ("subtotal_cents".to_string(), serde_json::json!(total_minor)),
            (
                "refunded_total_cents".to_string(),
                serde_json::json!(refunded_total_minor),
            ),
            ("currency".to_string(), serde_json::json!("NZD")),
            (
                "stripe_payment_intent_id".to_string(),
                serde_json::json!(format!("pi_{id}")),
            ),
            (
                "provider_payment_intent_id".to_string(),
                serde_json::json!(format!("pi_{id}")),
            ),
            (
                "stripe_account_id".to_string(),
                serde_json::json!(stripe_account_id),
            ),
            ("platform_fee_cents".to_string(), serde_json::json!(500)),
            ("livemode".to_string(), serde_json::json!(livemode)),
        ]),
    )
    .await;
}

fn admin_refund_msg(
    purchase_id: &str,
    body: serde_json::Value,
) -> (wafer_run::Message, wafer_run::InputStream) {
    let (mut msg, input) = create_msg(
        &format!("/admin/b/products/purchases/{purchase_id}/refund"),
        "admin_1",
        body,
    );
    msg.set_meta("auth.user_roles", "admin");
    (msg, input)
}

#[tokio::test]
async fn admin_stripe_status_distinguishes_configuration_modes_without_secrets() {
    let ctx = ctx().await;
    let (msg, input) = admin_get_msg("/admin/b/products/stripe/status");
    let unconfigured = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    assert_eq!(unconfigured["state"], "not_configured");
    assert_eq!(unconfigured["configured"], false);
    assert!(unconfigured.get("secret_key").is_none());

    let mut ctx = ctx_with(&[
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_health",
        ),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY",
            "pk_test_health",
        ),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
            "whsec_health",
        ),
    ])
    .await;
    let requests = register_sequence(
        &mut ctx,
        vec![express_account("acct_platform", true, true, true)],
    );
    let (msg, input) = admin_get_msg("/admin/b/products/stripe/status");
    let connected = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    assert_eq!(connected["state"], "connected_test");
    assert_eq!(connected["account_id"], "acct_platform");
    assert_eq!(connected["livemode"], false);
    assert_eq!(connected["country"], "NZ");
    assert_eq!(connected["default_currency"], "NZD");
    assert_eq!(connected["capabilities"]["card_payments"], "active");
    assert_eq!(connected["publishable_key_configured"], true);
    assert_eq!(connected["webhook_secret_configured"], true);
    assert!(connected.get("secret_key").is_none());
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://api.stripe.com/v1/account");
    assert_eq!(
        requests[0].headers["Authorization"],
        "Bearer sk_test_health"
    );
    assert_eq!(requests[0].headers["Stripe-Version"], "2026-02-25.clover");
}

#[tokio::test]
async fn admin_stripe_status_rejects_test_live_key_mismatch_before_network() {
    let mut ctx = ctx_with(&[
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_live_health",
        ),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY",
            "pk_test_health",
        ),
    ])
    .await;
    let requests = register_sequence(
        &mut ctx,
        vec![express_account("acct_unused", true, true, true)],
    );
    let (msg, input) = admin_get_msg("/admin/b/products/stripe/status");
    let body = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    assert_eq!(body["state"], "misconfigured");
    assert_eq!(body["livemode"], true);
    assert!(body["error"].as_str().unwrap().contains("different modes"));
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn admin_stripe_status_safely_reports_malformed_response_and_timeout() {
    for (response, expected) in [
        (BrokenStripeResponse::Malformed, "unreadable response"),
        (BrokenStripeResponse::Timeout, "could not be completed"),
    ] {
        let mut ctx = ctx_with(&[
            (
                "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
                "sk_test_health",
            ),
            (
                "IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY",
                "pk_test_health",
            ),
        ])
        .await;
        let requests = register_broken_response(&mut ctx, response);
        let (msg, input) = admin_get_msg("/admin/b/products/stripe/status");
        let body = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
        assert_eq!(body["state"], "misconfigured");
        assert!(body["error"].as_str().unwrap().contains(expected));
        assert!(!body.to_string().contains("private-invalid-json"));
        assert_eq!(requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn seller_onboarding_creates_one_owned_express_account_and_single_use_link() {
    let mut ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_connect",
        ),
        ("IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY", "NZ"),
        ("IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS", "250"),
    ])
    .await;
    let requests = register_sequence(
        &mut ctx,
        vec![
            express_account("acct_seller_new", false, false, false),
            serde_json::json!({
                "object": "account_link",
                "url": "https://connect.stripe.com/setup/test-link",
                "expires_at": 1_900_000_000_i64
            }),
        ],
    );
    let (msg, input) = create_msg(
        "/b/products/seller/onboarding",
        "seller_new",
        serde_json::json!({
            "return_url": "https://shop.example/seller/stripe/return",
            "refresh_url": "https://shop.example/seller/stripe/refresh"
        }),
    );
    let body = output_to_json(dispatch_user(&ctx, msg, input).await).await;
    assert_eq!(body["url"], "https://connect.stripe.com/setup/test-link");
    assert_eq!(body["expires_at"], 1_900_000_000_i64);
    assert_eq!(body["account"]["user_id"], "seller_new");
    assert_eq!(body["account"]["stripe_account_id"], "acct_seller_new");
    assert_eq!(body["account"]["status"], "onboarding");
    assert_eq!(body["account"]["fee_basis_points"], 250);
    assert_eq!(
        body["account"]["capabilities"]["requirements_due"],
        serde_json::json!(["individual.verification.document"])
    );
    assert!(body.get("secret_key").is_none());

    let local = repo::seller_accounts::get_for_user(&ctx, "seller_new")
        .await
        .unwrap()
        .expect("seller row");
    assert_eq!(local.data["stripe_account_id"], "acct_seller_new");
    assert_eq!(local.data["country"], "NZ");
    assert_eq!(local.data["default_currency"], "NZD");

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].url, "https://api.stripe.com/v1/accounts");
    assert!(requests[0].headers["Idempotency-Key"].starts_with("impresspress_connect_account_"));
    let account_form = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert!(account_form.contains("type=express"));
    assert!(account_form.contains("capabilities[card_payments][requested]=true"));
    assert!(account_form.contains("capabilities[transfers][requested]=true"));
    assert!(account_form.contains("metadata[impresspress_user_id]=seller_new"));
    assert!(account_form.contains("country=NZ"));
    assert_eq!(requests[1].url, "https://api.stripe.com/v1/account_links");
    assert!(requests[1].headers["Idempotency-Key"].starts_with("impresspress_account_link_"));
    let link_form = String::from_utf8(requests[1].body.clone().unwrap()).unwrap();
    assert!(link_form.contains("account=acct_seller_new"));
    assert!(link_form.contains("type=account_onboarding"));
    assert!(link_form.contains("collection_options[fields]=eventually_due"));
}

#[tokio::test]
async fn seller_onboarding_validates_origin_and_feature_gate_before_provider_calls() {
    let mut ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_connect",
        ),
    ])
    .await;
    let requests = register_sequence(
        &mut ctx,
        vec![express_account("acct_unused", false, false, false)],
    );
    let (msg, input) = create_msg(
        "/b/products/seller/onboarding",
        "seller_bad_origin",
        serde_json::json!({
            "return_url": "https://attacker.example/complete",
            "refresh_url": "https://shop.example/refresh"
        }),
    );
    assert!(
        output_is_error(
            dispatch_user(&ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
    assert!(requests.lock().unwrap().is_empty());
    assert!(
        repo::seller_accounts::get_for_user(&ctx, "seller_bad_origin")
            .await
            .unwrap()
            .is_none()
    );

    let ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "false")]).await;
    let (msg, input) = get_msg("/b/products/seller/account", "seller_disabled");
    assert!(
        output_is_error(
            dispatch_user(&ctx, msg, input).await,
            ErrorCode::PermissionDenied
        )
        .await
    );
}

#[tokio::test]
async fn seller_dashboard_refreshes_only_the_callers_account_and_returns_express_link() {
    let mut ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_connect",
        ),
    ])
    .await;
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_account_dashboard",
        std::collections::HashMap::from([
            ("user_id".to_string(), serde_json::json!("seller_dashboard")),
            ("status".to_string(), serde_json::json!("active")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_dashboard"),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(true)),
            ("payouts_enabled".to_string(), serde_json::json!(true)),
        ]),
    )
    .await;
    let requests = register_sequence(
        &mut ctx,
        vec![
            express_account("acct_dashboard", true, true, true),
            serde_json::json!({
                "object": "login_link",
                "url": "https://connect.stripe.com/express/dashboard-link"
            }),
        ],
    );
    let (msg, input) = create_msg(
        "/b/products/seller/dashboard",
        "seller_dashboard",
        serde_json::json!({}),
    );
    let body = output_to_json(dispatch_user(&ctx, msg, input).await).await;
    assert_eq!(
        body["url"],
        "https://connect.stripe.com/express/dashboard-link"
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].url,
        "https://api.stripe.com/v1/accounts/acct_dashboard"
    );
    assert_eq!(
        requests[1].url,
        "https://api.stripe.com/v1/accounts/acct_dashboard/login_links"
    );
    assert!(requests[1].headers["Idempotency-Key"].starts_with("impresspress_login_link_"));
}

#[tokio::test]
async fn buyer_billing_portal_uses_owned_order_customer_and_connected_account() {
    let mut ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_portal",
        ),
    ])
    .await;
    seed_portal_order(
        &ctx,
        "purchase_portal",
        "buyer_portal",
        "cus_buyer",
        "acct_seller",
        false,
    )
    .await;
    let requests = register_sequence(
        &mut ctx,
        vec![serde_json::json!({
            "id": "bps_test",
            "object": "billing_portal.session",
            "url": "https://billing.stripe.com/p/session/test_portal"
        })],
    );
    let (msg, input) = create_msg(
        "/b/products/billing-portal",
        "buyer_portal",
        serde_json::json!({
            "return_url": "https://shop.example/account",
            "order_id": "purchase_portal"
        }),
    );
    let body = output_to_json(dispatch_user(&ctx, msg, input).await).await;
    assert_eq!(
        body["url"],
        "https://billing.stripe.com/p/session/test_portal"
    );

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url,
        "https://api.stripe.com/v1/billing_portal/sessions"
    );
    assert_eq!(requests[0].headers["Stripe-Account"], "acct_seller");
    assert!(requests[0].headers["Idempotency-Key"].starts_with("impresspress_billing_portal_"));
    assert_eq!(requests[0].headers["Stripe-Version"], "2026-02-25.clover");
    let form = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert!(form.contains("customer=cus_buyer"));
    assert!(form.contains("return_url=https%3A%2F%2Fshop.example%2Faccount"));
}

#[tokio::test]
async fn buyer_billing_portal_rejects_cross_user_order_before_provider_call() {
    let mut ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_portal",
        ),
    ])
    .await;
    seed_portal_order(
        &ctx,
        "purchase_private",
        "buyer_owner",
        "cus_owner",
        "acct_owner",
        false,
    )
    .await;
    let requests = register_sequence(
        &mut ctx,
        vec![serde_json::json!({
            "url": "https://billing.stripe.com/p/session/unused"
        })],
    );
    let (msg, input) = create_msg(
        "/b/products/billing-portal",
        "buyer_attacker",
        serde_json::json!({
            "return_url": "https://shop.example/account",
            "order_id": "purchase_private"
        }),
    );
    assert!(
        output_is_error(
            dispatch_user(&ctx, msg, input).await,
            ErrorCode::PermissionDenied
        )
        .await
    );
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn buyer_billing_portal_requires_order_when_customer_contexts_differ() {
    let mut ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_portal",
        ),
    ])
    .await;
    seed_portal_order(
        &ctx,
        "purchase_context_a",
        "buyer_multi",
        "cus_multi_a",
        "acct_seller_a",
        false,
    )
    .await;
    seed_portal_order(
        &ctx,
        "purchase_context_b",
        "buyer_multi",
        "cus_multi_b",
        "acct_seller_b",
        false,
    )
    .await;
    let requests = register_sequence(
        &mut ctx,
        vec![serde_json::json!({
            "url": "https://billing.stripe.com/p/session/unused"
        })],
    );
    let (msg, input) = create_msg(
        "/b/products/billing-portal",
        "buyer_multi",
        serde_json::json!({"return_url": "https://shop.example/account"}),
    );
    assert!(
        output_is_error(
            dispatch_user(&ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn buyer_billing_portal_rejects_mode_mismatch_and_untrusted_return_origin() {
    let mut ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_portal",
        ),
    ])
    .await;
    seed_portal_order(&ctx, "purchase_live", "buyer_live", "cus_live", "", true).await;
    let requests = register_sequence(
        &mut ctx,
        vec![serde_json::json!({
            "url": "https://billing.stripe.com/p/session/unused"
        })],
    );
    let (msg, input) = create_msg(
        "/b/products/billing-portal",
        "buyer_live",
        serde_json::json!({
            "return_url": "https://shop.example/account",
            "order_id": "purchase_live"
        }),
    );
    assert!(
        output_is_error(
            dispatch_user(&ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );

    let (msg, input) = create_msg(
        "/b/products/billing-portal",
        "buyer_live",
        serde_json::json!({
            "return_url": "https://attacker.example/account",
            "order_id": "purchase_live"
        }),
    );
    assert!(
        output_is_error(
            dispatch_user(&ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
    assert!(requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn connected_account_partial_refund_is_provider_first_exact_and_idempotent() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_refunds",
    )])
    .await;
    seed_stripe_refund_order(
        &ctx,
        "purchase_partial",
        "completed",
        10_000,
        0,
        "acct_refund_seller",
        false,
    )
    .await;
    let requests = register_sequence(
        &mut ctx,
        vec![serde_json::json!({
            "id": "re_partial",
            "object": "refund",
            "status": "succeeded",
            "amount": 2500,
            "payment_intent": "pi_purchase_partial",
            "livemode": false
        })],
    );
    let request_body = serde_json::json!({
        "amount_minor": 2500,
        "provider_reason": "requested_by_customer",
        "note": "Customer changed scope",
        "idempotency_key": "partial_refund_1"
    });
    let (msg, input) = admin_refund_msg("purchase_partial", request_body.clone());
    let body = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    assert_eq!(body["status"], "succeeded");
    assert_eq!(body["provider_refund_id"], "re_partial");
    assert_eq!(body["amount_minor"], 2500);
    assert_eq!(body["refunded_total_minor"], 2500);
    assert_eq!(body["order_total_minor"], 10_000);

    let purchase = repo::purchases::get(&ctx, "purchase_partial")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "partially_refunded");
    assert_eq!(purchase.data["refunded_total_cents"], 2500);
    assert_eq!(purchase.data["refund_reason"], "Customer changed scope");
    let ledger = repo::refunds::list_for_purchase(&ctx, "purchase_partial")
        .await
        .unwrap();
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].data["status"], "succeeded");
    assert_eq!(ledger[0].data["provider_refund_id"], "re_partial");

    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url, "https://api.stripe.com/v1/refunds");
        assert_eq!(requests[0].headers["Stripe-Account"], "acct_refund_seller");
        assert_eq!(
            requests[0].headers["Idempotency-Key"],
            "impresspress_refund_purchase_partial_partial_refund_1"
        );
        assert_eq!(requests[0].headers["Stripe-Version"], "2026-02-25.clover");
        let form = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
        assert!(form.contains("payment_intent=pi_purchase_partial"));
        assert!(form.contains("amount=2500"));
        assert!(form.contains("reason=requested_by_customer"));
        assert!(form.contains("refund_application_fee=true"));
        assert!(form.contains("metadata[impresspress_purchase_id]=purchase_partial"));
        assert!(
            !form.contains("Customer"),
            "private operator note leaked to Stripe"
        );
    }

    let (msg, input) = admin_refund_msg("purchase_partial", request_body);
    let replay = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    assert_eq!(replay["provider_refund_id"], "re_partial");
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "retry must not call Stripe"
    );
}

#[tokio::test]
async fn full_refund_after_partial_only_refunds_the_exact_remaining_amount() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_refunds",
    )])
    .await;
    seed_stripe_refund_order(
        &ctx,
        "purchase_remaining",
        "partially_refunded",
        10_000,
        2500,
        "",
        false,
    )
    .await;
    let requests = register_sequence(
        &mut ctx,
        vec![serde_json::json!({
            "id": "re_remaining",
            "status": "succeeded",
            "amount": 7500,
            "payment_intent": "pi_purchase_remaining",
            "livemode": false
        })],
    );
    let (msg, input) = admin_refund_msg("purchase_remaining", serde_json::json!({}));
    let body = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    assert_eq!(body["status"], "succeeded");
    assert_eq!(body["amount_minor"], 7500);
    assert_eq!(body["refunded_total_minor"], 10_000);
    let purchase = repo::purchases::get(&ctx, "purchase_remaining")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "refunded");
    let requests = requests.lock().unwrap();
    let form = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert!(form.contains("amount=7500"));
    assert!(!requests[0].headers.contains_key("Stripe-Account"));
}

#[tokio::test]
async fn pending_refund_preserves_purchase_and_blocks_a_different_operation() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_refunds",
    )])
    .await;
    seed_stripe_refund_order(
        &ctx,
        "purchase_pending_refund",
        "completed",
        5000,
        0,
        "",
        false,
    )
    .await;
    let requests = register_sequence(
        &mut ctx,
        vec![serde_json::json!({
            "id": "re_pending",
            "status": "pending",
            "amount": 1000,
            "payment_intent": "pi_purchase_pending_refund",
            "livemode": false
        })],
    );
    let (msg, input) = admin_refund_msg(
        "purchase_pending_refund",
        serde_json::json!({"amount_minor": 1000, "idempotency_key": "operation_a"}),
    );
    let body = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    assert_eq!(body["status"], "pending");
    assert_eq!(body["refunded_total_minor"], 0);
    let purchase = repo::purchases::get(&ctx, "purchase_pending_refund")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "completed");
    assert_eq!(purchase.data["refunded_total_cents"], 0);

    let (msg, input) = admin_refund_msg(
        "purchase_pending_refund",
        serde_json::json!({"amount_minor": 500, "idempotency_key": "operation_b"}),
    );
    assert!(
        output_is_error(
            dispatch_admin(&ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn provider_reconciliation_recovers_pending_refund_with_one_atomic_lease() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_refunds",
    )])
    .await;
    seed_stripe_refund_order(
        &ctx,
        "purchase_reconcile_refund",
        "completed",
        5000,
        0,
        "",
        false,
    )
    .await;
    let requests = register_sequence(
        &mut ctx,
        vec![
            serde_json::json!({
                "id": "re_reconcile_pending",
                "status": "pending",
                "amount": 1250,
                "payment_intent": "pi_purchase_reconcile_refund",
                "livemode": false
            }),
            serde_json::json!({
                "id": "re_reconcile_pending",
                "status": "succeeded",
                "amount": 1250,
                "payment_intent": "pi_purchase_reconcile_refund",
                "livemode": false
            }),
        ],
    );
    let (msg, input) = admin_refund_msg(
        "purchase_reconcile_refund",
        serde_json::json!({"amount_minor": 1250, "idempotency_key": "recovery"}),
    );
    let pending = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    assert_eq!(pending["status"], "pending");

    let (list, input) = admin_get_msg("/admin/b/products/provider-operations");
    let listed = output_to_json(dispatch_admin(&ctx, list, input).await).await;
    assert_eq!(listed["total_count"], 1);
    assert_eq!(listed["records"][0]["operation_type"], "refund.reconcile");
    assert_eq!(listed["records"][0]["status"], "pending");
    let safe = serde_json::to_string(&listed).unwrap();
    assert!(!safe.contains("idempotency_key"));
    assert!(!safe.contains("request_json"));
    assert!(!safe.contains("processing_owner"));

    let operation = wafer_core::clients::database::get_by_field(
        &ctx,
        repo::provider_operations::TABLE,
        "aggregate_type",
        serde_json::json!("refund"),
    )
    .await
    .unwrap();
    let first_claim = repo::provider_operations::claim_due(&ctx, 1).await.unwrap();
    assert_eq!(first_claim.len(), 1);
    assert!(repo::provider_operations::claim_due(&ctx, 1)
        .await
        .unwrap()
        .is_empty());
    wafer_core::clients::database::update(
        &ctx,
        repo::provider_operations::TABLE,
        &operation.id,
        std::collections::HashMap::from([
            ("status".to_string(), serde_json::json!("pending")),
            ("processing_owner".to_string(), serde_json::json!("")),
            ("processing_started_at".to_string(), serde_json::Value::Null),
            ("attempts".to_string(), serde_json::json!(0)),
        ]),
    )
    .await
    .unwrap();

    let (mut reconcile, input) = admin_create_msg(
        "/admin/b/products/provider-operations/reconcile",
        serde_json::json!({}),
    );
    reconcile.set_meta("req.query.limit", "1");
    let result = output_to_json(dispatch_admin(&ctx, reconcile, input).await).await;
    assert_eq!(result["claimed"], 1);
    assert_eq!(result["succeeded"], 1);
    assert_eq!(result["retry_scheduled"], 0);

    let purchase = repo::purchases::get(&ctx, "purchase_reconcile_refund")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "partially_refunded");
    assert_eq!(purchase.data["refunded_total_cents"], 1250);
    let operation =
        wafer_core::clients::database::get(&ctx, repo::provider_operations::TABLE, &operation.id)
            .await
            .unwrap();
    assert_eq!(operation.data["status"], "succeeded");
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "GET");
    assert!(requests[1]
        .url
        .ends_with("/v1/refunds/re_reconcile_pending"));
}

#[tokio::test]
async fn stripe_rejection_and_mode_mismatch_never_mark_purchase_refunded() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_refunds",
    )])
    .await;
    seed_stripe_refund_order(
        &ctx,
        "purchase_rejected_refund",
        "completed",
        5000,
        0,
        "",
        false,
    )
    .await;
    let requests = register_sequence_with_status(
        &mut ctx,
        vec![(
            400,
            serde_json::json!({"error": {"code": "charge_already_refunded"}}),
        )],
    );
    let (msg, input) = admin_refund_msg(
        "purchase_rejected_refund",
        serde_json::json!({"idempotency_key": "rejected"}),
    );
    assert!(
        output_is_error(
            dispatch_admin(&ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
    let purchase = repo::purchases::get(&ctx, "purchase_rejected_refund")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "completed");
    assert_eq!(purchase.data["refunded_total_cents"], 0);
    let ledger = repo::refunds::list_for_purchase(&ctx, "purchase_rejected_refund")
        .await
        .unwrap();
    assert_eq!(ledger[0].data["status"], "failed");
    assert!(ledger[0].data["last_error"]
        .as_str()
        .unwrap()
        .contains("charge_already_refunded"));
    assert_eq!(requests.lock().unwrap().len(), 1);

    seed_stripe_refund_order(&ctx, "purchase_live_refund", "completed", 5000, 0, "", true).await;
    let (msg, input) = admin_refund_msg(
        "purchase_live_refund",
        serde_json::json!({"idempotency_key": "wrong_mode"}),
    );
    assert!(
        output_is_error(
            dispatch_admin(&ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
    assert_eq!(
        requests.lock().unwrap().len(),
        1,
        "mode mismatch is preflighted"
    );
    let purchase = repo::purchases::get(&ctx, "purchase_live_refund")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "completed");
}

#[tokio::test]
async fn refund_validation_rejects_over_refund_and_unknown_fields_before_stripe() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_refunds",
    )])
    .await;
    seed_stripe_refund_order(
        &ctx,
        "purchase_validate_refund",
        "partially_refunded",
        5000,
        4500,
        "",
        false,
    )
    .await;
    let requests = register_sequence(&mut ctx, vec![]);
    let (msg, input) = admin_refund_msg(
        "purchase_validate_refund",
        serde_json::json!({"amount_minor": 501, "idempotency_key": "too_much"}),
    );
    assert!(
        output_is_error(
            dispatch_admin(&ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
    let (msg, input) = admin_refund_msg(
        "purchase_validate_refund",
        serde_json::json!({"amount_minor": 100, "unexpected": true}),
    );
    assert!(
        output_is_error(
            dispatch_admin(&ctx, msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
    assert!(requests.lock().unwrap().is_empty());
    assert!(
        repo::refunds::list_for_purchase(&ctx, "purchase_validate_refund")
            .await
            .unwrap()
            .is_empty()
    );
}
