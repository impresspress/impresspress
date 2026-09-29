use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use wafer_core::interfaces::network::service::{NetworkError, NetworkService, Request, Response};
use wafer_run::{Block, ErrorCode};

use super::harness::*;
use crate::{blocks::products::repo, util::RecordExt};

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
        &format!("/b/products/api/admin/purchases/{purchase_id}/refund"),
        "admin_1",
        body,
    );
    msg.set_meta("auth.user_roles", "admin");
    (msg, input)
}

#[tokio::test]
async fn admin_stripe_status_distinguishes_configuration_modes_without_secrets() {
    let ctx = ctx().await;
    let (msg, input) = admin_get_msg("/b/products/api/admin/stripe/status");
    let unconfigured = output_to_json(dispatch(&ctx, msg, input).await).await;
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
    let (msg, input) = admin_get_msg("/b/products/api/admin/stripe/status");
    let connected = output_to_json(dispatch(&ctx, msg, input).await).await;
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
    let (msg, input) = admin_get_msg("/b/products/api/admin/stripe/status");
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
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
        let (msg, input) = admin_get_msg("/b/products/api/admin/stripe/status");
        let body = output_to_json(dispatch(&ctx, msg, input).await).await;
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
        "/b/products/api/seller/onboarding",
        "seller_new",
        serde_json::json!({
            "return_url": "https://shop.example/seller/stripe/return",
            "refresh_url": "https://shop.example/seller/stripe/refresh"
        }),
    );
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
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
        "/b/products/api/seller/onboarding",
        "seller_bad_origin",
        serde_json::json!({
            "return_url": "https://attacker.example/complete",
            "refresh_url": "https://shop.example/refresh"
        }),
    );
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await);
    assert!(requests.lock().unwrap().is_empty());
    assert!(
        repo::seller_accounts::get_for_user(&ctx, "seller_bad_origin")
            .await
            .unwrap()
            .is_none()
    );

    let ctx = ctx_with(&[("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "false")]).await;
    let (msg, input) = get_msg("/b/products/api/seller/account", "seller_disabled");
    assert!(
        output_is_error(
            dispatch(&ctx, msg, input).await,
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
        "/b/products/api/seller/dashboard",
        "seller_dashboard",
        serde_json::json!({}),
    );
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
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
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
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
            dispatch(&ctx, msg, input).await,
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
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await);
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
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await);

    let (msg, input) = create_msg(
        "/b/products/billing-portal",
        "buyer_live",
        serde_json::json!({
            "return_url": "https://attacker.example/account",
            "order_id": "purchase_live"
        }),
    );
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await);
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
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
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
    let replay = output_to_json(dispatch(&ctx, msg, input).await).await;
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
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
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
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
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
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await);
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
    let pending = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(pending["status"], "pending");

    let (list, input) = admin_get_msg("/b/products/api/admin/provider-operations");
    let listed = output_to_json(dispatch(&ctx, list, input).await).await;
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
    assert_eq!(first_claim.claims.len(), 1);
    assert!(repo::provider_operations::claim_due(&ctx, 1)
        .await
        .unwrap()
        .claims
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
        "/b/products/api/admin/provider-operations/reconcile",
        serde_json::json!({}),
    );
    reconcile.set_meta("req.query.limit", "1");
    let result = output_to_json(dispatch(&ctx, reconcile, input).await).await;
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
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await);
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
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await);
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

/// HTTP 429 and 5xx from POST /v1/refunds are ambiguous: Stripe may have
/// created the refund before failing, so the ledger claim must stay pending
/// and the provider operation queued for a retry with the SAME idempotency
/// key. Marking the ledger failed here would desynchronize it from real money
/// movement — the later refund webhook would find no active row.
#[tokio::test]
async fn ambiguous_stripe_refund_failure_stays_retryable_with_the_same_key() {
    for status in [500_u16, 429] {
        let mut ctx = ctx_with(&[(
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_refunds",
        )])
        .await;
        seed_stripe_refund_order(
            &ctx,
            "purchase_ambiguous_refund",
            "completed",
            5000,
            0,
            "",
            false,
        )
        .await;
        let requests = register_sequence_with_status(
            &mut ctx,
            vec![
                (
                    status,
                    serde_json::json!({"error": {"code": "lock_timeout"}}),
                ),
                (
                    200,
                    serde_json::json!({
                        "id": "re_ambiguous",
                        "status": "succeeded",
                        "amount": 1000,
                        "payment_intent": "pi_purchase_ambiguous_refund",
                        "livemode": false
                    }),
                ),
            ],
        );
        let (msg, input) = admin_refund_msg(
            "purchase_ambiguous_refund",
            serde_json::json!({"amount_minor": 1000, "idempotency_key": "ambiguous"}),
        );
        assert!(
            output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::Internal).await,
            "HTTP {status} must surface as a retryable internal error"
        );

        let ledger = repo::refunds::list_for_purchase(&ctx, "purchase_ambiguous_refund")
            .await
            .unwrap();
        assert_eq!(ledger.len(), 1);
        assert_eq!(
            ledger[0].data["status"], "pending",
            "HTTP {status} must keep the ledger claim pending, not failed"
        );
        let purchase = repo::purchases::get(&ctx, "purchase_ambiguous_refund")
            .await
            .unwrap();
        assert_eq!(purchase.data["status"], "completed");
        assert_eq!(purchase.data["refunded_total_cents"], 0);
        let operation = wafer_core::clients::database::get_by_field(
            &ctx,
            repo::provider_operations::TABLE,
            "aggregate_type",
            serde_json::json!("refund"),
        )
        .await
        .unwrap();
        assert_eq!(
            operation.data["status"], "pending",
            "HTTP {status} must keep the provider operation queued, not dead-lettered"
        );

        let (mut reconcile, input) = admin_create_msg(
            "/b/products/api/admin/provider-operations/reconcile",
            serde_json::json!({}),
        );
        reconcile.set_meta("req.query.limit", "1");
        let result = output_to_json(dispatch(&ctx, reconcile, input).await).await;
        assert_eq!(result["claimed"], 1);
        assert_eq!(result["succeeded"], 1);

        // Cloned out of the guard rather than asserted through it: a
        // `MutexGuard` alive at the end of an async test body is held across
        // the await points the harness adds around it.
        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].method, "POST");
        assert_eq!(requests[1].url, "https://api.stripe.com/v1/refunds");
        assert_eq!(
            requests[0].headers["Idempotency-Key"],
            "impresspress_refund_purchase_ambiguous_refund_ambiguous"
        );
        assert_eq!(
            requests[1].headers["Idempotency-Key"], requests[0].headers["Idempotency-Key"],
            "the retry must reuse the original idempotency key"
        );
        let purchase = repo::purchases::get(&ctx, "purchase_ambiguous_refund")
            .await
            .unwrap();
        assert_eq!(purchase.data["status"], "partially_refunded");
        assert_eq!(purchase.data["refunded_total_cents"], 1000);
    }
}

/// 402 (like every non-429 4xx) means Stripe deterministically rejected the
/// request: no refund exists, so the ledger row fails terminally and the
/// provider operation dead-letters instead of retrying a refused request.
#[tokio::test]
async fn card_level_stripe_rejection_fails_the_refund_deterministically() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_refunds",
    )])
    .await;
    seed_stripe_refund_order(
        &ctx,
        "purchase_declined_refund",
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
            402,
            serde_json::json!({"error": {"code": "expired_or_canceled_card"}}),
        )],
    );
    let (msg, input) = admin_refund_msg(
        "purchase_declined_refund",
        serde_json::json!({"idempotency_key": "declined"}),
    );
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await);
    let ledger = repo::refunds::list_for_purchase(&ctx, "purchase_declined_refund")
        .await
        .unwrap();
    assert_eq!(ledger[0].data["status"], "failed");
    assert!(ledger[0].data["last_error"]
        .as_str()
        .unwrap()
        .contains("expired_or_canceled_card"));
    let operation = wafer_core::clients::database::get_by_field(
        &ctx,
        repo::provider_operations::TABLE,
        "aggregate_type",
        serde_json::json!("refund"),
    )
    .await
    .unwrap();
    assert_eq!(operation.data["status"], "dead_letter");
    let purchase = repo::purchases::get(&ctx, "purchase_declined_refund")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "completed");
    assert_eq!(purchase.data["refunded_total_cents"], 0);
    assert_eq!(requests.lock().unwrap().len(), 1);
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
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await);
    let (msg, input) = admin_refund_msg(
        "purchase_validate_refund",
        serde_json::json!({"amount_minor": 100, "unexpected": true}),
    );
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::InvalidArgument).await);
    assert!(requests.lock().unwrap().is_empty());
    assert!(
        repo::refunds::list_for_purchase(&ctx, "purchase_validate_refund")
            .await
            .unwrap()
            .is_empty()
    );
}

/// Re-read an operation row and hand back its `response_json` column as the
/// value it encodes, whichever way the adapter decoded it.
async fn operation_response_json(
    ctx: &crate::test_support::TestContext,
    id: &str,
) -> serde_json::Value {
    let operation = wafer_core::clients::database::get(ctx, repo::provider_operations::TABLE, id)
        .await
        .unwrap();
    match operation.data.get("response_json") {
        Some(serde_json::Value::String(raw)) => {
            serde_json::from_str(raw).unwrap_or(serde_json::Value::Null)
        }
        Some(value) => value.clone(),
        None => serde_json::Value::Null,
    }
}

/// Put a settled operation back on the queue, the way a lost lease leaves it.
async fn reset_operation_to_pending(ctx: &crate::test_support::TestContext, id: &str) {
    wafer_core::clients::database::update(
        ctx,
        repo::provider_operations::TABLE,
        id,
        std::collections::HashMap::from([
            ("status".to_string(), serde_json::json!("pending")),
            ("processing_owner".to_string(), serde_json::json!("")),
            ("processing_started_at".to_string(), serde_json::Value::Null),
            ("next_attempt_at".to_string(), serde_json::Value::Null),
            ("attempts".to_string(), serde_json::json!(0)),
        ]),
    )
    .await
    .unwrap();
}

/// `response_json` is this repo's own summary of a provider response — never
/// a Stripe body; see the note on `provider_operations` in
/// `impresspress_core::secret_tables` for why that distinction is load-
/// bearing. It is a JSON-object column written as
/// `serde_json::json!({..}).to_string()` and declared `TEXT NOT NULL DEFAULT
/// '{}'`. Native SQLite and the browser re-parse a JSON-shaped TEXT column on
/// read, so it arrives as a `Value::Object` — for which `str_field`, having no
/// structured arm, answers `""`. Every reader that carries the summary from
/// the refund ledger onto the provider-operation row therefore persisted an
/// empty string, silently dropping the refund's provider outcome from a
/// payments audit trail. Nothing reads it back, so nothing failed loudly.
///
/// Four call sites carry that summary from the refund ledger onto the
/// provider-operation row. Three are exercised here, in the order a real
/// refund meets them: the reconcile worker settling a pending refund
/// (`stripe_provider::reconcile_refund_operation` → `mark_completed`), a
/// second reconcile of an already-settled ledger row (its early return), and
/// a retried delivery of the original refund request
/// (`purchase::refund_purchase`'s `Succeeded` arm → `resolve_unleased`).
///
/// The fourth is `refund_purchase`'s `ProviderSucceeded` arm, which reads the
/// same column through the same `resolve_unleased` and so shares the fix, but
/// is reached only from a ledger state this test does not stage.
#[tokio::test]
async fn refund_reconciliation_keeps_the_provider_response_summary() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_refunds",
    )])
    .await;
    seed_stripe_refund_order(
        &ctx,
        "purchase_audit_trail",
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
                "id": "re_audit_trail",
                "status": "pending",
                "amount": 1250,
                "payment_intent": "pi_purchase_audit_trail",
                "livemode": false
            }),
            serde_json::json!({
                "id": "re_audit_trail",
                "status": "succeeded",
                "amount": 1250,
                "payment_intent": "pi_purchase_audit_trail",
                "livemode": false
            }),
        ],
    );

    let (msg, input) = admin_refund_msg(
        "purchase_audit_trail",
        serde_json::json!({"amount_minor": 1250, "idempotency_key": "audit_trail"}),
    );
    let pending = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(pending["status"], "pending");

    let operation = wafer_core::clients::database::get_by_field(
        &ctx,
        repo::provider_operations::TABLE,
        "aggregate_type",
        serde_json::json!("refund"),
    )
    .await
    .unwrap();

    // 1. The reconcile worker settles the refund against Stripe and completes
    //    the operation with the response it just recorded on the ledger.
    reset_operation_to_pending(&ctx, &operation.id).await;
    let (mut reconcile, input) = admin_create_msg(
        "/b/products/api/admin/provider-operations/reconcile",
        serde_json::json!({}),
    );
    reconcile.set_meta("req.query.limit", "1");
    let reconciled = output_to_json(dispatch(&ctx, reconcile, input).await).await;
    assert_eq!(reconciled["succeeded"], 1);
    assert_eq!(
        operation_response_json(&ctx, &operation.id).await,
        serde_json::json!({
            "id": "re_audit_trail",
            "status": "succeeded",
            "amount_minor": 1250,
            "livemode": false,
            "source": "provider_reconciliation"
        }),
        "the settled operation must keep the raw provider response"
    );

    // 2. A second reconcile of an already-settled ledger row takes the early
    //    return and must republish the same payload, not blank it.
    reset_operation_to_pending(&ctx, &operation.id).await;
    let (mut reconcile, input) = admin_create_msg(
        "/b/products/api/admin/provider-operations/reconcile",
        serde_json::json!({}),
    );
    reconcile.set_meta("req.query.limit", "1");
    let again = output_to_json(dispatch(&ctx, reconcile, input).await).await;
    assert_eq!(again["succeeded"], 1);
    assert_eq!(
        operation_response_json(&ctx, &operation.id).await["id"],
        "re_audit_trail",
        "an already-settled refund must not lose its provider response"
    );

    // 3. A retried delivery of the original request resolves the operation
    //    from the ledger row, and must carry the payload across too.
    wafer_core::clients::database::update(
        &ctx,
        repo::provider_operations::TABLE,
        &operation.id,
        std::collections::HashMap::from([("response_json".to_string(), serde_json::json!("{}"))]),
    )
    .await
    .unwrap();
    let (msg, input) = admin_refund_msg(
        "purchase_audit_trail",
        serde_json::json!({"amount_minor": 1250, "idempotency_key": "audit_trail"}),
    );
    let retried = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(retried["status"], "succeeded");
    assert_eq!(
        operation_response_json(&ctx, &operation.id).await["id"],
        "re_audit_trail",
        "a retried refund delivery must not blank the provider response"
    );

    // 4. The same retry against a ledger row Stripe has settled but this side
    //    has not (`provider_succeeded`, what an interrupted reconcile leaves)
    //    takes the other arm of `refund_purchase`, which carries the payload
    //    across too.
    let ledger = repo::refunds::get_by_idempotency_key(
        &ctx,
        "impresspress_refund_purchase_audit_trail_audit_trail",
    )
    .await
    .unwrap()
    .expect("the refund ledger row this test just drove to succeeded");
    // No product path parks a row here for longer than one request —
    // `refund_purchase` writes `provider_succeeded` and settles it a few lines
    // later — so the state an interrupted reconcile leaves behind is staged
    // directly. `record_provider_response` cannot do it once the reconcile has
    // stamped `stripe_event_created`, which it has by now.
    wafer_core::clients::database::update(
        &ctx,
        repo::refunds::TABLE,
        &ledger.id,
        std::collections::HashMap::from([(
            "status".to_string(),
            serde_json::json!("provider_succeeded"),
        )]),
    )
    .await
    .unwrap();
    wafer_core::clients::database::update(
        &ctx,
        repo::provider_operations::TABLE,
        &operation.id,
        std::collections::HashMap::from([("response_json".to_string(), serde_json::json!("{}"))]),
    )
    .await
    .unwrap();
    let (msg, input) = admin_refund_msg(
        "purchase_audit_trail",
        serde_json::json!({"amount_minor": 1250, "idempotency_key": "audit_trail"}),
    );
    let settled = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(settled["status"], "succeeded");
    assert_eq!(
        operation_response_json(&ctx, &operation.id).await["id"],
        "re_audit_trail",
        "settling a provider-succeeded refund must not blank the provider response"
    );

    // Stripe was asked exactly twice: the create and the one reconcile GET.
    assert_eq!(requests.lock().unwrap().len(), 2);
}

// ============================================================
// Error mapping — the refund orchestration in `purchase.rs`
// ============================================================

/// An admin refund of a Stripe order whose refund-ledger lookup answers
/// `code`, and the status the request gets.
///
/// The lookup (`get_by_idempotency_key`, a `database.list` on the refunds
/// table) runs after the purchase read, which is a `database.get` on the
/// purchases table, so the refusal lands on the orchestration's own site.
async fn refund_status_when_the_ledger_read_answers(code: ErrorCode) -> u16 {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_refunds",
    )])
    .await;
    seed_stripe_refund_order(
        &ctx,
        "purchase_ledger_refused",
        "completed",
        5000,
        0,
        "",
        false,
    )
    .await;
    let requests = register_sequence_with_status(&mut ctx, Vec::new());
    let failing = crate::test_support::FailingDbOpContext::failing_with(
        ctx.clone(),
        vec![("database.list", repo::refunds::TABLE)],
        wafer_run::WaferError::new(code, "refused by the database client"),
    );
    let (msg, input) = admin_refund_msg(
        "purchase_ledger_refused",
        serde_json::json!({"amount_minor": 1000, "idempotency_key": "refused"}),
    );
    let status =
        crate::test_support::output_http_status(dispatch(&failing, msg, input).await).await;
    assert!(
        requests.lock().unwrap().is_empty(),
        "a refused ledger read must stop the refund before Stripe is asked"
    );
    status
}

#[tokio::test]
async fn refund_ledger_denial_is_403() {
    assert_eq!(
        refund_status_when_the_ledger_read_answers(ErrorCode::PermissionDenied).await,
        403
    );
}

#[tokio::test]
async fn refund_ledger_quota_is_429() {
    assert_eq!(
        refund_status_when_the_ledger_read_answers(ErrorCode::ResourceExhausted).await,
        429
    );
}

/// Guard (passes before and after the database tails were classified): a
/// Stripe rate limit on the refund call is Stripe's, so it stays the
/// sanitized 500 rather than borrowing the 429 a database quota earns.
#[tokio::test]
async fn refund_stripe_rate_limit_stays_500() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_refunds",
    )])
    .await;
    seed_stripe_refund_order(
        &ctx,
        "purchase_stripe_rate_limited",
        "completed",
        5000,
        0,
        "",
        false,
    )
    .await;
    let requests = register_sequence_with_status(
        &mut ctx,
        vec![(429, serde_json::json!({"error": {"code": "rate_limit"}}))],
    );
    let (msg, input) = admin_refund_msg(
        "purchase_stripe_rate_limited",
        serde_json::json!({"amount_minor": 1000, "idempotency_key": "rate_limited"}),
    );
    assert_eq!(
        crate::test_support::output_http_status(dispatch(&ctx, msg, input).await).await,
        500
    );
    assert_eq!(requests.lock().unwrap().len(), 1);
}

/// Seed a provider operation that is due now. `created_at` is explicit so
/// the claim pass meets the rows in a known order.
async fn seed_due_operation(
    ctx: &crate::test_support::TestContext,
    id: &str,
    operation_type: &str,
    created_at: &str,
    extra: &[(&str, serde_json::Value)],
) {
    let mut data = std::collections::HashMap::from([
        (
            "operation_type".to_string(),
            serde_json::json!(operation_type),
        ),
        ("aggregate_type".to_string(), serde_json::json!("refund")),
        (
            "aggregate_id".to_string(),
            serde_json::json!(format!("refund_{id}")),
        ),
        (
            "idempotency_key".to_string(),
            serde_json::json!(format!("key_{id}")),
        ),
        ("status".to_string(), serde_json::json!("pending")),
        ("created_at".to_string(), serde_json::json!(created_at)),
        ("updated_at".to_string(), serde_json::json!(created_at)),
    ]);
    for (field, value) in extra {
        data.insert(field.to_string(), value.clone());
    }
    seed(ctx, repo::provider_operations::TABLE, id, data).await;
}

/// A `processing` row whose lease lapsed long ago, as a crashed worker leaves
/// it.
fn expired_lease(attempts: u64, last_error: &str) -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("status", serde_json::json!("processing")),
        ("attempts", serde_json::json!(attempts)),
        ("processing_owner", serde_json::json!("crashed-worker")),
        (
            "processing_started_at",
            serde_json::json!((chrono::Utc::now() - chrono::Duration::seconds(3600)).to_rfc3339()),
        ),
        ("last_error", serde_json::json!(last_error)),
    ]
}

async fn operation_row(
    ctx: &dyn wafer_run::context::Context,
    id: &str,
) -> wafer_core::clients::database::Record {
    wafer_core::clients::database::get(ctx, repo::provider_operations::TABLE, id)
        .await
        .unwrap()
}

async fn reconcile_due(ctx: &dyn wafer_run::context::Context) -> wafer_run::OutputStream {
    let (reconcile, input) = admin_create_msg(
        "/b/products/api/admin/provider-operations/reconcile",
        serde_json::json!({}),
    );
    dispatch(ctx, reconcile, input).await
}

/// A failed outcome write for one operation is that operation's problem: the
/// rest of the claimed batch still runs and records its outcome, and the
/// response counts the one that could not be recorded instead of failing
/// the whole request.
///
/// The first row's outcome is a terminal one (an unsupported operation type),
/// written with `database.update`, which is the op made to fail. The second
/// row's outcome is a retry (its refund row is missing), written with
/// `database.update_where_count`, which keeps working.
#[tokio::test]
async fn one_operations_failed_outcome_write_does_not_abort_the_batch() {
    let ctx = ctx().await;
    seed_due_operation(
        &ctx,
        "op_first",
        "unsupported.kind",
        "2026-01-01T00:00:00Z",
        &[],
    )
    .await;
    seed_due_operation(
        &ctx,
        "op_second",
        repo::provider_operations::REFUND_RECONCILE,
        "2026-01-01T00:00:01Z",
        &[],
    )
    .await;
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx,
        vec![("database.update", repo::provider_operations::TABLE)],
    );

    let result = output_to_json(reconcile_due(&failing).await).await;
    assert_eq!(
        result,
        serde_json::json!({
            "claimed": 2,
            "succeeded": 0,
            "retry_scheduled": 1,
            "dead_letter": 0,
            "unrecorded": 1,
        })
    );

    let second = operation_row(&failing, "op_second").await;
    assert_eq!(
        second.data["status"], "failed",
        "the row after the failed write must still get its outcome"
    );
    assert!(!second.str_field("last_error").is_empty());
    assert!(!second.str_field("next_attempt_at").is_empty());
    // The row whose write failed keeps its lease until the lease lapses.
    assert_eq!(
        operation_row(&failing, "op_first").await.data["status"],
        "processing"
    );
}

/// The claim pass has the same rule: a row whose dead-letter write fails is
/// reported, and the rows already claimed in the same pass are still run
/// rather than left leased with an attempt spent.
#[tokio::test]
async fn one_rows_failed_claim_write_does_not_strand_the_rows_already_claimed() {
    let ctx = ctx().await;
    seed_due_operation(
        &ctx,
        "op_claimed",
        "unsupported.kind",
        "2026-01-01T00:00:00Z",
        &[],
    )
    .await;
    seed_due_operation(
        &ctx,
        "op_exhausted",
        repo::provider_operations::REFUND_RECONCILE,
        "2026-01-01T00:00:01Z",
        &expired_lease(repo::MAX_ATTEMPTS, ""),
    )
    .await;
    // The first `update_where_count` is `op_claimed`'s claim; the second is
    // `op_exhausted`'s dead-letter write, which fails.
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx,
        vec![(
            "database.update_where_count",
            repo::provider_operations::TABLE,
        )],
    )
    .after_passing(1);

    let result = output_to_json(reconcile_due(&failing).await).await;
    assert_eq!(
        result,
        serde_json::json!({
            "claimed": 1,
            "succeeded": 0,
            "retry_scheduled": 0,
            "dead_letter": 1,
            "unrecorded": 1,
        })
    );
    assert_eq!(
        operation_row(&failing, "op_claimed").await.data["status"],
        "dead_letter",
        "the claimed row must be run to its outcome"
    );
    assert_eq!(
        operation_row(&failing, "op_exhausted").await.data["status"],
        "processing"
    );
}

/// An operation out of attempts is dead-lettered with the reason an operator
/// needs, whichever path spends the last attempt: the claim pass finding a
/// lapsed lease on the last attempt, or the last attempt failing.
#[tokio::test]
async fn an_operation_out_of_attempts_dead_letters_with_its_reason_and_is_counted() {
    let ctx = ctx().await;
    seed_due_operation(
        &ctx,
        "op_lapsed",
        repo::provider_operations::REFUND_RECONCILE,
        "2026-01-01T00:00:00Z",
        &expired_lease(repo::MAX_ATTEMPTS, "Stripe timed out"),
    )
    .await;
    seed_due_operation(
        &ctx,
        "op_last_try",
        repo::provider_operations::REFUND_RECONCILE,
        "2026-01-01T00:00:01Z",
        &[("attempts", serde_json::json!(repo::MAX_ATTEMPTS - 1))],
    )
    .await;

    let result = output_to_json(reconcile_due(&ctx).await).await;
    assert_eq!(
        result,
        serde_json::json!({
            "claimed": 1,
            "succeeded": 0,
            "retry_scheduled": 0,
            "dead_letter": 2,
            "unrecorded": 0,
        })
    );

    let lapsed = operation_row(&ctx, "op_lapsed").await;
    assert_eq!(lapsed.data["status"], "dead_letter");
    let reason = lapsed.str_field("last_error");
    assert!(
        reason.contains("retry budget") && reason.contains("expired"),
        "the reason must say the budget ran out on a lapsed lease: {reason:?}"
    );
    assert!(
        reason.contains("Stripe timed out"),
        "the earlier attempts' error must survive: {reason:?}"
    );
    assert!(!lapsed.str_field("terminal_at").is_empty());

    let last_try = operation_row(&ctx, "op_last_try").await;
    assert_eq!(last_try.data["status"], "dead_letter");
    assert_eq!(last_try.data["attempts"], repo::MAX_ATTEMPTS);
    assert!(!last_try.str_field("last_error").is_empty());
}

/// Taking a lease is the last database step of a claim: the claimed row is
/// handed to the worker without being read back, so no failure can land
/// between the lease and the work and strand the row leased with an attempt
/// spent. Every read of the table is made to fail after the claim pass's
/// candidate list.
#[tokio::test]
async fn a_claimed_operation_is_run_without_reading_it_back() {
    let ctx = ctx().await;
    seed_due_operation(
        &ctx,
        "op_no_readback",
        "unsupported.kind",
        "2026-01-01T00:00:00Z",
        &[],
    )
    .await;
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx.clone(),
        vec![("database.get", repo::provider_operations::TABLE)],
    );

    let result = output_to_json(reconcile_due(&failing).await).await;
    assert_eq!(
        result,
        serde_json::json!({
            "claimed": 1,
            "succeeded": 0,
            "retry_scheduled": 0,
            "dead_letter": 1,
            "unrecorded": 0,
        })
    );
    let row = operation_row(&ctx, "op_no_readback").await;
    assert_eq!(row.data["status"], "dead_letter");
    assert_eq!(row.data["attempts"], 1);
}
