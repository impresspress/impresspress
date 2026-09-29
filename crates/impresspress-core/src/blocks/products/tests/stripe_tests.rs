use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use wafer_block_crypto::primitives;
use wafer_core::{
    clients::database as db,
    interfaces::network::service::{NetworkError, NetworkService, Request, Response},
};
use wafer_run::{Block, ErrorCode, InputStream, Message};

use super::harness::*;
use crate::{
    blocks::products::{
        contracts::{
            OfferDefinitionRequest, OfferSyncStatus, PaymentLinkCreateRequest,
            PricingPreviewRequest,
        },
        offer_pricing, repo, stripe,
    },
    util::{hex_encode, sha256_hex, RecordExt},
};

// ============================================================
// Helpers
// ============================================================

const WEBHOOK_SECRET: &str = "whsec_test_secret_key";

#[derive(Clone)]
struct MockStripeNetwork {
    requests: Arc<Mutex<Vec<Request>>>,
    response: serde_json::Value,
}

#[async_trait]
impl NetworkService for MockStripeNetwork {
    async fn do_request(&self, request: &Request) -> Result<Response, NetworkError> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(Response {
            status_code: 200,
            headers: HashMap::new(),
            body: serde_json::to_vec(&self.response).unwrap(),
        })
    }
}

fn register_stripe_network(
    ctx: &mut crate::test_support::TestContext,
    response: serde_json::Value,
) -> Arc<Mutex<Vec<Request>>> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let block: Arc<dyn Block> = Arc::new(wafer_core::service_blocks::network::NetworkBlock::new(
        Arc::new(MockStripeNetwork {
            requests: requests.clone(),
            response,
        }),
    ));
    ctx.register_block("wafer-run/network", block);
    requests
}

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
            .expect("unexpected Stripe request without a queued response");
        Ok(Response {
            status_code,
            headers: HashMap::new(),
            body: serde_json::to_vec(&response).unwrap(),
        })
    }
}

fn register_stripe_sequence(
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

async fn seed_active_offer(
    ctx: &crate::test_support::TestContext,
    product_id: &str,
    owner_id: &str,
) -> String {
    seed(
        ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([
            ("name".to_string(), serde_json::json!("Configurable print")),
            ("slug".to_string(), serde_json::json!(product_id)),
            ("status".to_string(), serde_json::json!("active")),
            ("approval_status".to_string(), serde_json::json!("approved")),
            (
                "owner_kind".to_string(),
                serde_json::json!(if owner_id.is_empty() {
                    "platform"
                } else {
                    "user"
                }),
            ),
            ("owner_id".to_string(), serde_json::json!(owner_id)),
            ("created_by".to_string(), serde_json::json!(owner_id)),
        ]),
    )
    .await;
    let definition: OfferDefinitionRequest = serde_json::from_value(serde_json::json!({
        "name": "Print configuration",
        "mode": "payment",
        "currency": "nzd",
        "pricing_model": "components",
        "usage_type": "licensed",
        "billing_scheme": "per_unit",
        "tax_behavior": "exclusive",
        "variables": [{
            "key": "pages",
            "kind": "integer",
            "label": "Pages",
            "required": true,
            "minimum": "1",
            "maximum": "20",
            "step": "1"
        }],
        "components": [
            {
                "key": "setup",
                "label": "Setup",
                "required": true,
                "amount": {"type": "fixed", "unit_amount_minor": 1000}
            },
            {
                "key": "pages",
                "label": "Printed pages",
                "required": true,
                "amount": {
                    "type": "per_unit",
                    "input": "pages",
                    "unit_amount_minor": 25
                }
            }
        ],
        "checkout": {
            "automatic_tax": true,
            "collect_billing_address": true
        }
    }))
    .unwrap();
    let offer = repo::offers::create(ctx, product_id, "admin_1", &definition)
        .await
        .expect("create offer");
    repo::offers::publish(ctx, product_id, &offer.offer.id)
        .await
        .expect("publish offer");
    offer.offer.id
}

/// Seed a platform-owned product whose single active offer collects a
/// shipping address, with `allowed` as the offer's allowed country list
/// (empty = "the offer names none").
async fn seed_shipping_offer(
    ctx: &crate::test_support::TestContext,
    product_id: &str,
    allowed: &[&str],
) -> String {
    seed(
        ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([
            ("name".to_string(), serde_json::json!("Shipped print")),
            ("slug".to_string(), serde_json::json!(product_id)),
            ("status".to_string(), serde_json::json!("active")),
            ("approval_status".to_string(), serde_json::json!("approved")),
            ("owner_kind".to_string(), serde_json::json!("platform")),
            ("owner_id".to_string(), serde_json::json!("")),
            ("created_by".to_string(), serde_json::json!("")),
        ]),
    )
    .await;
    let definition: OfferDefinitionRequest = serde_json::from_value(serde_json::json!({
        "name": "Shipped print",
        "mode": "payment",
        "currency": "nzd",
        "pricing_model": "fixed",
        "interval_count": 1,
        "usage_type": "licensed",
        "billing_scheme": "per_unit",
        "tax_behavior": "exclusive",
        "variables": [],
        "components": [{
            "key": "base",
            "label": "Print",
            "required": true,
            "amount": {"type": "fixed", "unit_amount_minor": 4000}
        }],
        "checkout": {
            "collect_shipping_address": true,
            "allowed_shipping_countries": allowed,
        }
    }))
    .unwrap();
    let offer = repo::offers::create(ctx, product_id, "admin_1", &definition)
        .await
        .expect("create offer");
    repo::offers::publish(ctx, product_id, &offer.offer.id)
        .await
        .expect("publish offer");
    offer.offer.id
}

/// Build a valid Stripe webhook message with correct HMAC signature.
fn webhook_msg(payload: &serde_json::Value, secret: &str) -> (Message, InputStream) {
    let payload_bytes = serde_json::to_vec(payload).unwrap();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let signed = format!("{}.{}", timestamp, String::from_utf8_lossy(&payload_bytes));
    let sig_bytes = primitives::hmac_sha256(secret.as_bytes(), signed.as_bytes());
    let sig_hex = hex_encode(&sig_bytes);

    let sig_header = format!("t={timestamp},v1={sig_hex}");

    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "create");
    msg.set_meta("req.resource", "/b/products/webhooks");
    msg.set_meta("http.header.stripe-signature", &sig_header);
    (msg, InputStream::from_bytes(payload_bytes))
}

fn checkout_completed_event(purchase_id: &str, payment_intent: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "checkout.session.completed",
        "data": {
            "object": {
                "metadata": { "purchase_id": purchase_id },
                "payment_intent": payment_intent
            }
        }
    })
}

async fn seed_typed_checkout_order(
    ctx: &crate::test_support::TestContext,
    order_id: &str,
    session_id: &str,
) {
    seed(
        ctx,
        "impresspress__products__purchases",
        order_id,
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("")),
            ("status".to_string(), serde_json::json!("checkout_started")),
            ("subtotal_cents".to_string(), serde_json::json!(1000)),
            ("discount_cents".to_string(), serde_json::json!(0)),
            ("tax_cents".to_string(), serde_json::json!(0)),
            ("total_cents".to_string(), serde_json::json!(1000)),
            ("amount_cents".to_string(), serde_json::json!(1000)),
            ("currency".to_string(), serde_json::json!("NZD")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_expected"),
            ),
            ("livemode".to_string(), serde_json::json!(true)),
            (
                "provider_session_id".to_string(),
                serde_json::json!(session_id),
            ),
            (
                "metadata".to_string(),
                serde_json::json!(serde_json::json!({
                    "schema_version": 1,
                    "offer_id": "offer_expected",
                    "offer_version": 4,
                    "offer_mode": "payment",
                    "allowed_shipping_amounts_minor": [0, 500]
                })
                .to_string()),
            ),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("awaiting_payment"),
            ),
        ]),
    )
    .await;
}

fn typed_checkout_completed_event(order_id: &str, session_id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": format!("evt_{order_id}"),
        "type": "checkout.session.completed",
        "account": "acct_expected",
        "livemode": true,
        "data": {
            "object": {
                "id": session_id,
                "client_reference_id": order_id,
                "metadata": {
                    "purchase_id": order_id,
                    "offer_id": "offer_expected",
                    "offer_version": "4"
                },
                "mode": "payment",
                "payment_status": "paid",
                "currency": "nzd",
                "amount_subtotal": 1000,
                "amount_total": 1550,
                "total_details": {
                    "amount_discount": 100,
                    "amount_tax": 150,
                    "amount_shipping": 500
                },
                "payment_intent": {"id": "pi_reconciled"},
                "customer": {"id": "cus_reconciled"},
                "subscription": null,
                "livemode": true
            }
        }
    })
}

fn charge_refunded_event(payment_intent: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "charge.refunded",
        "data": {
            "object": {
                "payment_intent": payment_intent
            }
        }
    })
}

// ============================================================
// Webhook — checkout.session.completed
// ============================================================

#[tokio::test]
async fn webhook_checkout_completed_empty_purchase_id() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    // Event with empty purchase_id — should still return 200 (no-op)
    let event = serde_json::json!({
        "type": "checkout.session.completed",
        "data": {
            "object": {
                "metadata": { "purchase_id": "" },
                "payment_intent": "pi_xxx"
            }
        }
    });
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let out = stripe::handle_webhook(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["received"], true);
}

#[tokio::test]
async fn typed_checkout_webhook_reconciles_exact_provider_and_amount_state() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed_typed_checkout_order(&ctx, "order_exact", "cs_exact").await;
    let event = typed_checkout_completed_event("order_exact", "cs_exact");
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);

    let order = db::get(&ctx, "impresspress__products__purchases", "order_exact")
        .await
        .unwrap();
    assert_eq!(order.data["status"], "completed");
    assert_eq!(order.data["subtotal_cents"], 1000);
    assert_eq!(order.data["discount_cents"], 100);
    assert_eq!(order.data["tax_cents"], 150);
    assert_eq!(order.data["shipping_cents"], 500);
    assert_eq!(order.data["total_cents"], 1550);
    assert_eq!(order.data["provider_payment_intent_id"], "pi_reconciled");
    assert_eq!(order.data["stripe_customer_id"], "cus_reconciled");
    assert_eq!(order.data["reconciliation_status"], "reconciled");
}

/// If the local `provider_session_id` write failed after the Stripe session
/// was created, the order has an EMPTY session id and the completion event
/// would previously dead-letter with no recovery for a paid buyer. The signed
/// event's `client_reference_id` (set to the local purchase id at creation)
/// plus every other cross-check lets the order adopt the session id; a
/// DIFFERENT non-empty stored session id must still hard-fail.
#[tokio::test]
async fn typed_checkout_completion_adopts_session_after_lost_session_id_write() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    // The session-id write after session creation never landed locally.
    seed_typed_checkout_order(&ctx, "order_adopt", "").await;
    let event = typed_checkout_completed_event("order_adopt", "cs_adopted");
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    let order = db::get(&ctx, "impresspress__products__purchases", "order_adopt")
        .await
        .unwrap();
    assert_eq!(order.data["status"], "completed");
    assert_eq!(order.data["provider_session_id"], "cs_adopted");
    assert_eq!(order.data["reconciliation_status"], "reconciled");
    assert_eq!(order.data["provider_payment_intent_id"], "pi_reconciled");

    // Adoption is only for the EMPTY case: a different stored session id is
    // a conflict and must fail closed without touching the order.
    seed_typed_checkout_order(&ctx, "order_adopt_conflict", "cs_original").await;
    let conflict = typed_checkout_completed_event("order_adopt_conflict", "cs_hijack");
    let (msg, input) = webhook_msg(&conflict, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::Internal,
        )
        .await
    );
    let order = db::get(
        &ctx,
        "impresspress__products__purchases",
        "order_adopt_conflict",
    )
    .await
    .unwrap();
    assert_eq!(order.data["status"], "checkout_started");
    assert_eq!(order.data["provider_session_id"], "cs_original");
}

#[tokio::test]
async fn typed_checkout_webhook_defers_and_reconciles_delayed_payment_results() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    seed_typed_checkout_order(&ctx, "order_delayed_success", "cs_delayed_success").await;
    let mut pending = typed_checkout_completed_event("order_delayed_success", "cs_delayed_success");
    pending["id"] = serde_json::json!("evt_delayed_pending");
    pending["data"]["object"]["payment_status"] = serde_json::json!("unpaid");
    pending["data"]["object"]["payment_intent"] = serde_json::Value::Null;
    let (msg, input) = webhook_msg(&pending, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    let order = db::get(
        &ctx,
        "impresspress__products__purchases",
        "order_delayed_success",
    )
    .await
    .unwrap();
    assert_eq!(order.data["status"], "checkout_started");
    assert_eq!(order.data["reconciliation_status"], "awaiting_payment");

    let mut succeeded =
        typed_checkout_completed_event("order_delayed_success", "cs_delayed_success");
    succeeded["id"] = serde_json::json!("evt_delayed_succeeded");
    succeeded["type"] = serde_json::json!("checkout.session.async_payment_succeeded");
    let (msg, input) = webhook_msg(&succeeded, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    let order = db::get(
        &ctx,
        "impresspress__products__purchases",
        "order_delayed_success",
    )
    .await
    .unwrap();
    assert_eq!(order.data["status"], "completed");
    assert_eq!(order.data["reconciliation_status"], "reconciled");

    seed_typed_checkout_order(&ctx, "order_delayed_failure", "cs_delayed_failure").await;
    let mut failed = typed_checkout_completed_event("order_delayed_failure", "cs_delayed_failure");
    failed["id"] = serde_json::json!("evt_delayed_failed");
    failed["type"] = serde_json::json!("checkout.session.async_payment_failed");
    failed["data"]["object"]["payment_status"] = serde_json::json!("unpaid");
    failed["data"]["object"]["payment_intent"] = serde_json::Value::Null;
    let (msg, input) = webhook_msg(&failed, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    let order = db::get(
        &ctx,
        "impresspress__products__purchases",
        "order_delayed_failure",
    )
    .await
    .unwrap();
    assert_eq!(order.data["status"], "failed");
    assert_eq!(order.data["reconciliation_status"], "provider_error");
    assert_eq!(
        order.data["reconciliation_error"],
        "Stripe delayed payment failed"
    );

    seed_typed_checkout_order(&ctx, "order_delayed_tamper", "cs_delayed_tamper").await;
    let mut tampered = typed_checkout_completed_event("order_delayed_tamper", "cs_delayed_tamper");
    tampered["id"] = serde_json::json!("evt_delayed_tamper");
    tampered["type"] = serde_json::json!("checkout.session.async_payment_failed");
    tampered["data"]["object"]["id"] = serde_json::json!("cs_unrelated");
    tampered["data"]["object"]["payment_status"] = serde_json::json!("unpaid");
    tampered["data"]["object"]["payment_intent"] = serde_json::Value::Null;
    let (msg, input) = webhook_msg(&tampered, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::Internal,
        )
        .await
    );
    let order = db::get(
        &ctx,
        "impresspress__products__purchases",
        "order_delayed_tamper",
    )
    .await
    .unwrap();
    assert_eq!(order.data["status"], "checkout_started");
}

#[tokio::test]
async fn payment_intent_events_are_ordered_diagnostic_and_never_fulfill_alone() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed_typed_checkout_order(&ctx, "order_payment_intent", "cs_payment_intent").await;
    let intent_event = |id: &str, kind: &str, status: &str, created: i64| {
        serde_json::json!({
            "id": id,
            "type": kind,
            "created": created,
            "account": "acct_expected",
            "livemode": true,
            "data": {"object": {
                "id": "pi_payment_intent",
                "status": status,
                "amount": 1550,
                "currency": "nzd",
                "livemode": true,
                "metadata": {
                    "purchase_id": "order_payment_intent",
                    "offer_id": "offer_expected",
                    "offer_version": "4"
                }
            }}
        })
    };

    let processing = intent_event(
        "evt_payment_intent_processing",
        "payment_intent.processing",
        "processing",
        200,
    );
    let (msg, input) = webhook_msg(&processing, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let order = repo::purchases::get(&ctx, "order_payment_intent")
        .await
        .unwrap();
    assert_eq!(order.data["status"], "checkout_started");
    assert_eq!(
        order.data["provider_payment_intent_id"],
        "pi_payment_intent"
    );
    assert_eq!(order.data["provider_payment_status"], "processing");
    assert_eq!(order.data["reconciliation_status"], "payment_processing");
    assert_eq!(order.data["payment_intent_event_created"], 200);

    let mut failed = intent_event(
        "evt_payment_intent_failed",
        "payment_intent.payment_failed",
        "requires_payment_method",
        300,
    );
    failed["data"]["object"]["last_payment_error"] = serde_json::json!({
        "code": "card_declined",
        "message": "Card declined.\nTry another\u{0007} method"
    });
    let (msg, input) = webhook_msg(&failed, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let order = repo::purchases::get(&ctx, "order_payment_intent")
        .await
        .unwrap();
    assert_eq!(order.data["status"], "checkout_started");
    assert_eq!(order.data["provider_payment_status"], "payment_failed");
    assert_eq!(order.data["provider_payment_error_code"], "card_declined");
    assert_eq!(
        order.data["provider_payment_error_message"],
        "Card declined. Try another method"
    );
    assert_eq!(order.data["payment_intent_event_created"], 300);

    let stale = intent_event(
        "evt_payment_intent_stale",
        "payment_intent.processing",
        "processing",
        250,
    );
    let (msg, input) = webhook_msg(&stale, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let order = repo::purchases::get(&ctx, "order_payment_intent")
        .await
        .unwrap();
    assert_eq!(order.data["provider_payment_status"], "payment_failed");
    assert_eq!(order.data["payment_intent_event_created"], 300);

    let succeeded = intent_event(
        "evt_payment_intent_succeeded",
        "payment_intent.succeeded",
        "succeeded",
        400,
    );
    let (msg, input) = webhook_msg(&succeeded, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let order = repo::purchases::get(&ctx, "order_payment_intent")
        .await
        .unwrap();
    assert_eq!(
        order.data["status"], "checkout_started",
        "PaymentIntent success alone must never fulfill an order"
    );
    assert_eq!(order.data["provider_payment_status"], "succeeded");
    assert_eq!(
        order.data["reconciliation_status"],
        "payment_succeeded_awaiting_checkout"
    );
    assert_eq!(order.data["payment_intent_event_created"], 400);

    let mut checkout = typed_checkout_completed_event("order_payment_intent", "cs_payment_intent");
    checkout["id"] = serde_json::json!("evt_payment_intent_checkout_authority");
    checkout["data"]["object"]["payment_intent"] = serde_json::json!({"id": "pi_payment_intent"});
    let (msg, input) = webhook_msg(&checkout, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let order = repo::purchases::get(&ctx, "order_payment_intent")
        .await
        .unwrap();
    assert_eq!(order.data["status"], "completed");
    assert_eq!(order.data["provider_payment_status"], "succeeded");

    // Checkout's paid state is authoritative over a late non-success PI
    // delivery, including upgraded records without a comparable PI timestamp.
    let late_failed = intent_event(
        "evt_payment_intent_late_failed",
        "payment_intent.payment_failed",
        "requires_payment_method",
        500,
    );
    let (msg, input) = webhook_msg(&late_failed, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let order = repo::purchases::get(&ctx, "order_payment_intent")
        .await
        .unwrap();
    assert_eq!(order.data["status"], "completed");
    assert_eq!(order.data["provider_payment_status"], "succeeded");
    assert_eq!(order.data["payment_intent_event_created"], 400);

    let mut wrong_amount = intent_event(
        "evt_payment_intent_wrong_completed_amount",
        "payment_intent.succeeded",
        "succeeded",
        600,
    );
    wrong_amount["data"]["object"]["amount"] = serde_json::json!(1549);
    let (msg, input) = webhook_msg(&wrong_amount, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::Internal,
        )
        .await
    );
}

#[tokio::test]
async fn payment_intent_events_reject_identity_mode_schema_and_status_tampering() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    for case in [
        "account",
        "livemode",
        "event_object_mode",
        "currency",
        "offer",
        "offer_version",
        "amount",
        "object_status",
    ] {
        let order_id = format!("order_payment_intent_tamper_{case}");
        seed_typed_checkout_order(&ctx, &order_id, &format!("cs_pi_tamper_{case}")).await;
        let mut event = serde_json::json!({
            "id": format!("evt_payment_intent_tamper_{case}"),
            "type": "payment_intent.succeeded",
            "created": 100,
            "account": "acct_expected",
            "livemode": true,
            "data": {"object": {
                "id": format!("pi_tamper_{case}"),
                "status": "succeeded",
                "amount": 1550,
                "currency": "nzd",
                "livemode": true,
                "metadata": {
                    "purchase_id": order_id,
                    "offer_id": "offer_expected",
                    "offer_version": "4"
                }
            }}
        });
        match case {
            "account" => event["account"] = serde_json::json!("acct_wrong"),
            "livemode" => {
                event["livemode"] = serde_json::json!(false);
                event["data"]["object"]["livemode"] = serde_json::json!(false);
            }
            "event_object_mode" => event["data"]["object"]["livemode"] = serde_json::json!(false),
            "currency" => event["data"]["object"]["currency"] = serde_json::json!("usd"),
            "offer" => {
                event["data"]["object"]["metadata"]["offer_id"] = serde_json::json!("offer_wrong")
            }
            "offer_version" => {
                event["data"]["object"]["metadata"]["offer_version"] = serde_json::json!("5")
            }
            "amount" => event["data"]["object"]["amount"] = serde_json::json!(-1),
            "object_status" => event["data"]["object"]["status"] = serde_json::json!("processing"),
            _ => unreachable!(),
        }
        let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
        assert!(
            output_is_error(
                stripe::handle_webhook(&ctx, &msg, input).await,
                ErrorCode::Internal,
            )
            .await,
            "{case} tampering must fail closed"
        );
        let order = repo::purchases::get(&ctx, &order_id).await.unwrap();
        assert_eq!(order.data["status"], "checkout_started", "case {case}");
        assert_eq!(order.data["provider_payment_intent_id"], "", "case {case}");
    }
}

#[tokio::test]
async fn typed_checkout_webhook_rejects_identity_mode_and_amount_tampering() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    for case in [
        "session",
        "reference",
        "account",
        "livemode",
        "currency",
        "subtotal",
        "shipping",
        "total",
        "mode",
        "payment_status",
        "offer",
        "offer_version",
        "payment_intent",
    ] {
        let order_id = format!("order_tamper_{case}");
        let session_id = format!("cs_tamper_{case}");
        seed_typed_checkout_order(&ctx, &order_id, &session_id).await;
        let mut event = typed_checkout_completed_event(&order_id, &session_id);
        match case {
            "session" => event["data"]["object"]["id"] = serde_json::json!("cs_wrong"),
            "reference" => {
                event["data"]["object"]["client_reference_id"] = serde_json::json!("order_wrong")
            }
            "account" => event["account"] = serde_json::json!("acct_wrong"),
            "livemode" => {
                event["livemode"] = serde_json::json!(false);
                event["data"]["object"]["livemode"] = serde_json::json!(false);
            }
            "currency" => event["data"]["object"]["currency"] = serde_json::json!("usd"),
            "subtotal" => {
                event["data"]["object"]["amount_subtotal"] = serde_json::json!(999);
                event["data"]["object"]["amount_total"] = serde_json::json!(1549);
            }
            "shipping" => {
                event["data"]["object"]["total_details"]["amount_shipping"] =
                    serde_json::json!(400);
                event["data"]["object"]["amount_total"] = serde_json::json!(1450);
            }
            "total" => event["data"]["object"]["amount_total"] = serde_json::json!(1549),
            "mode" => event["data"]["object"]["mode"] = serde_json::json!("subscription"),
            "payment_status" => {
                event["type"] = serde_json::json!("checkout.session.async_payment_succeeded");
                event["data"]["object"]["payment_status"] = serde_json::json!("unpaid");
            }
            "offer" => {
                event["data"]["object"]["metadata"]["offer_id"] = serde_json::json!("offer_wrong")
            }
            "offer_version" => {
                event["data"]["object"]["metadata"]["offer_version"] = serde_json::json!("5")
            }
            "payment_intent" => event["data"]["object"]["payment_intent"] = serde_json::Value::Null,
            _ => unreachable!(),
        }
        event["id"] = serde_json::json!(format!("evt_tamper_{case}"));
        let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
        assert!(
            output_is_error(
                stripe::handle_webhook(&ctx, &msg, input).await,
                ErrorCode::Internal,
            )
            .await,
            "{case} mismatch must fail closed"
        );
        let order = db::get(&ctx, "impresspress__products__purchases", &order_id)
            .await
            .unwrap();
        assert_eq!(order.data["status"], "checkout_started", "case {case}");
        let event_row = db::get(
            &ctx,
            "impresspress__products__stripe_events",
            &format!("evt_tamper_{case}"),
        )
        .await
        .unwrap();
        assert_eq!(event_row.data["status"], "failed", "case {case}");
        assert!(!event_row.str_field("next_retry_at").is_empty());
    }
}

#[tokio::test]
async fn webhook_subscription_checkout_records_provider_identity_and_items() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        "impresspress__products__purchases",
        "purchase_subscription_webhook",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_1")),
            ("buyer_user_id".to_string(), serde_json::json!("buyer_1")),
            ("status".to_string(), serde_json::json!("checkout_started")),
            ("total_cents".to_string(), serde_json::json!(2500)),
            ("subtotal_cents".to_string(), serde_json::json!(2500)),
            ("currency".to_string(), serde_json::json!("NZD")),
            ("stripe_account_id".to_string(), serde_json::json!("")),
            ("livemode".to_string(), serde_json::json!(true)),
            (
                "provider_session_id".to_string(),
                serde_json::json!("cs_subscription_webhook"),
            ),
            (
                "metadata".to_string(),
                serde_json::json!(serde_json::json!({
                    "schema_version": 1,
                    "offer_id": "offer_pro_monthly",
                    "offer_version": 3,
                    "offer_mode": "subscription",
                    "allowed_shipping_amounts_minor": [0]
                })
                .to_string()),
            ),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("awaiting_payment"),
            ),
        ]),
    )
    .await;
    seed(
        &ctx,
        "impresspress__products__line_items",
        "line_subscription_webhook",
        HashMap::from([
            (
                "purchase_id".to_string(),
                serde_json::json!("purchase_subscription_webhook"),
            ),
            ("product_id".to_string(), serde_json::json!("product_pro")),
            ("product_name".to_string(), serde_json::json!("Pro plan")),
            (
                "offer_id".to_string(),
                serde_json::json!("offer_pro_monthly"),
            ),
            (
                "component_id".to_string(),
                serde_json::json!("component_base"),
            ),
            ("quantity".to_string(), serde_json::json!(1)),
            ("unit_amount_minor".to_string(), serde_json::json!(2500)),
            ("total_minor".to_string(), serde_json::json!(2500)),
            ("offer_version".to_string(), serde_json::json!(3)),
        ]),
    )
    .await;
    let event = serde_json::json!({
        "id": "evt_subscription_webhook",
        "type": "checkout.session.completed",
        "livemode": true,
        "data": {
            "object": {
                "id": "cs_subscription_webhook",
                "client_reference_id": "purchase_subscription_webhook",
                "metadata": {
                    "purchase_id": "purchase_subscription_webhook",
                    "offer_id": "offer_pro_monthly",
                    "offer_version": "3"
                },
                "mode": "subscription",
                "payment_status": "paid",
                "currency": "nzd",
                "amount_subtotal": 2500,
                "amount_total": 2500,
                "total_details": {
                    "amount_discount": 0,
                    "amount_tax": 0,
                    "amount_shipping": 0
                },
                "payment_intent": null,
                "customer": "cus_buyer_1",
                "subscription": "sub_product_1",
                "livemode": true
            }
        }
    });
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);

    let order = db::get(
        &ctx,
        "impresspress__products__purchases",
        "purchase_subscription_webhook",
    )
    .await
    .unwrap();
    assert_eq!(order.data["status"], "completed");
    assert_eq!(order.data["stripe_customer_id"], "cus_buyer_1");
    assert_eq!(order.data["stripe_subscription_id"], "sub_product_1");
    assert!(
        order.data["livemode"].as_bool() == Some(true)
            || order.data["livemode"].as_i64() == Some(1)
    );
    assert_eq!(order.data["reconciliation_status"], "reconciled");

    let items = db::list_all(
        &ctx,
        repo::subscription_items::TABLE,
        vec![wafer_block::db::Filter {
            field: "subscription_id".to_string(),
            operator: wafer_block::db::FilterOp::Equal,
            value: serde_json::json!("sub_product_1"),
        }],
    )
    .await
    .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].data["purchase_id"],
        "purchase_subscription_webhook"
    );
    assert_eq!(items[0].data["offer_id"], "offer_pro_monthly");
    assert_eq!(items[0].data["component_id"], "component_base");
}

/// A crash between the completion write and the subscription-item snapshot
/// used to be unrecoverable: the redelivery saw "already completed" and
/// skipped the snapshot forever. The snapshot is an idempotent upsert, so a
/// redelivery of the completion event must backfill it.
#[tokio::test]
async fn checkout_redelivery_backfills_missing_subscription_item_snapshot() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_snapshot_backfill",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_1")),
            ("buyer_user_id".to_string(), serde_json::json!("buyer_1")),
            ("status".to_string(), serde_json::json!("checkout_started")),
            ("total_cents".to_string(), serde_json::json!(2500)),
            ("subtotal_cents".to_string(), serde_json::json!(2500)),
            ("currency".to_string(), serde_json::json!("NZD")),
            ("stripe_account_id".to_string(), serde_json::json!("")),
            ("livemode".to_string(), serde_json::json!(true)),
            (
                "provider_session_id".to_string(),
                serde_json::json!("cs_snapshot_backfill"),
            ),
            (
                "metadata".to_string(),
                serde_json::json!(serde_json::json!({
                    "schema_version": 1,
                    "offer_id": "offer_pro_monthly",
                    "offer_version": 3,
                    "offer_mode": "subscription",
                    "allowed_shipping_amounts_minor": [0]
                })
                .to_string()),
            ),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("awaiting_payment"),
            ),
        ]),
    )
    .await;
    seed(
        &ctx,
        "impresspress__products__line_items",
        "line_snapshot_backfill",
        HashMap::from([
            (
                "purchase_id".to_string(),
                serde_json::json!("purchase_snapshot_backfill"),
            ),
            ("product_id".to_string(), serde_json::json!("product_pro")),
            ("product_name".to_string(), serde_json::json!("Pro plan")),
            (
                "offer_id".to_string(),
                serde_json::json!("offer_pro_monthly"),
            ),
            (
                "component_id".to_string(),
                serde_json::json!("component_base"),
            ),
            ("quantity".to_string(), serde_json::json!(1)),
            ("unit_amount_minor".to_string(), serde_json::json!(2500)),
            ("total_minor".to_string(), serde_json::json!(2500)),
            ("offer_version".to_string(), serde_json::json!(3)),
        ]),
    )
    .await;
    let event = serde_json::json!({
        "id": "evt_snapshot_backfill",
        "type": "checkout.session.completed",
        "livemode": true,
        "data": {
            "object": {
                "id": "cs_snapshot_backfill",
                "client_reference_id": "purchase_snapshot_backfill",
                "metadata": {
                    "purchase_id": "purchase_snapshot_backfill",
                    "offer_id": "offer_pro_monthly",
                    "offer_version": "3"
                },
                "mode": "subscription",
                "payment_status": "paid",
                "currency": "nzd",
                "amount_subtotal": 2500,
                "amount_total": 2500,
                "total_details": {
                    "amount_discount": 0,
                    "amount_tax": 0,
                    "amount_shipping": 0
                },
                "payment_intent": null,
                "customer": "cus_buyer_1",
                "subscription": "sub_backfill",
                "livemode": true
            }
        }
    });

    // First delivery: the completion write lands, then the subscription-item
    // snapshot hits a transient outage. The delivery must fail retryably.
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx.clone(),
        vec![("database.upsert", repo::subscription_items::TABLE)],
    );
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&failing, &msg, input).await,
            ErrorCode::Internal,
        )
        .await
    );
    let order = repo::purchases::get(&ctx, "purchase_snapshot_backfill")
        .await
        .unwrap();
    assert_eq!(order.data["status"], "completed");
    assert_eq!(
        db::list_all(&ctx, repo::subscription_items::TABLE, vec![])
            .await
            .unwrap()
            .len(),
        0
    );
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_snapshot_backfill",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "failed");
    assert!(!event_row.str_field("next_retry_at").is_empty());

    // Stripe redelivers after the backoff window; the order is already
    // completed (rows == 0) but the missing snapshot must be backfilled.
    db::update(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_snapshot_backfill",
        HashMap::from([(
            "next_retry_at".to_string(),
            serde_json::json!("2000-01-01T00:00:00Z"),
        )]),
    )
    .await
    .unwrap();
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    let items = db::list_all(
        &ctx,
        repo::subscription_items::TABLE,
        vec![wafer_block::db::Filter {
            field: "subscription_id".to_string(),
            operator: wafer_block::db::FilterOp::Equal,
            value: serde_json::json!("sub_backfill"),
        }],
    )
    .await
    .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].data["purchase_id"], "purchase_snapshot_backfill");
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_snapshot_backfill",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "processed");
}

/// A subscription delivery carrying a lifecycle state this build does not
/// define is refused, not silently dropped.
///
/// `sync_commerce_subscription` takes a typed status now, so the wire value
/// has to be parsed at the webhook boundary. Mapping an unparseable one to
/// "no subscription" would have skipped the whole commerce branch and
/// answered `200 received`, which tells Stripe the event was handled and
/// leaves the projection silently stale. The 500 makes Stripe redeliver and
/// puts the event row in the operator's failed queue.
#[tokio::test]
async fn a_subscription_status_outside_the_contract_fails_the_webhook() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_unknown_sub_status",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_unknown")),
            (
                "buyer_user_id".to_string(),
                serde_json::json!("buyer_unknown"),
            ),
            ("status".to_string(), serde_json::json!("completed")),
            ("total_cents".to_string(), serde_json::json!(2500)),
            ("currency".to_string(), serde_json::json!("NZD")),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_unknown_status"),
            ),
            (
                "subscription_status".to_string(),
                serde_json::json!("active"),
            ),
        ]),
    )
    .await;

    let event = serde_json::json!({
        "id": "evt_unknown_sub_status",
        "type": "customer.subscription.updated",
        "livemode": false,
        "data": {"object": {
            "id": "sub_unknown_status",
            "status": "hibernating",
            "cancel_at_period_end": false,
            "canceled_at": null
        }}
    });
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    assert_eq!(
        crate::test_support::output_http_status(stripe::handle_webhook(&ctx, &msg, input).await)
            .await,
        500,
        "an unknown lifecycle state must make Stripe redeliver, not report success",
    );

    // The stored projection is untouched: nothing was applied, and nothing
    // was quietly cleared either.
    let purchase = repo::purchases::get(&ctx, "purchase_unknown_sub_status")
        .await
        .unwrap();
    assert_eq!(purchase.data["subscription_status"], "active");

    // And the event row is in the failed queue naming THIS reason. The
    // assertion is on the reason, not just on the 500: without the refusal
    // the delivery still fails, but three steps later and for the wrong
    // reason — the commerce branch is skipped as "no subscription", the
    // platform-billing lookup then finds no row of its own, and the operator
    // is told the subscription is unowned rather than that its state is
    // unrecognised.
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_unknown_sub_status",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "failed");
    assert_eq!(
        event_row.data["last_error"], "subscription status was unsupported",
        "the failure has to name the unrecognised state, not a downstream symptom",
    );
}

#[tokio::test]
async fn commerce_subscription_webhooks_keep_authoritative_lifecycle_state() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_subscription_lifecycle",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_lifecycle")),
            (
                "buyer_user_id".to_string(),
                serde_json::json!("buyer_lifecycle"),
            ),
            ("status".to_string(), serde_json::json!("completed")),
            ("total_cents".to_string(), serde_json::json!(2500)),
            ("currency".to_string(), serde_json::json!("NZD")),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_commerce_lifecycle"),
            ),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_commerce_seller"),
            ),
            ("livemode".to_string(), serde_json::json!(true)),
            (
                "subscription_status".to_string(),
                serde_json::json!("active"),
            ),
        ]),
    )
    .await;
    let period_end = 2_000_000_000_i64;
    let updated = serde_json::json!({
        "id": "evt_commerce_subscription_updated",
        "type": "customer.subscription.updated",
        "account": "acct_commerce_seller",
        "livemode": true,
        "data": {"object": {
            "id": "sub_commerce_lifecycle",
            "status": "trialing",
            "current_period_end": period_end,
            "cancel_at_period_end": true,
            "canceled_at": null
        }}
    });
    let (msg, input) = webhook_msg(&updated, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);

    let purchase = repo::purchases::get(&ctx, "purchase_subscription_lifecycle")
        .await
        .unwrap();
    assert_eq!(purchase.data["subscription_status"], "trialing");
    assert_eq!(
        purchase.data["subscription_current_period_end"],
        chrono::DateTime::<chrono::Utc>::from_timestamp(period_end, 0)
            .unwrap()
            .to_rfc3339()
    );
    assert!(
        purchase.data["subscription_cancel_at_period_end"].as_bool() == Some(true)
            || purchase.data["subscription_cancel_at_period_end"].as_i64() == Some(1)
    );
    assert!(purchase.data["subscription_last_synced_at"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));

    let payment_failed = serde_json::json!({
        "id": "evt_commerce_invoice_failed",
        "type": "invoice.payment_failed",
        "account": "acct_commerce_seller",
        "livemode": true,
        "data": {"object": {
            "subscription": "sub_commerce_lifecycle"
        }}
    });
    let (msg, input) = webhook_msg(&payment_failed, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);

    let purchase = repo::purchases::get(&ctx, "purchase_subscription_lifecycle")
        .await
        .unwrap();
    assert_eq!(purchase.data["subscription_status"], "past_due");
    assert!(
        purchase.data["subscription_cancel_at_period_end"].as_bool() == Some(true)
            || purchase.data["subscription_cancel_at_period_end"].as_i64() == Some(1)
    );

    let canceled_at = 2_000_001_234_i64;
    let deleted = serde_json::json!({
        "id": "evt_commerce_subscription_deleted",
        "type": "customer.subscription.deleted",
        "account": "acct_commerce_seller",
        "livemode": true,
        "data": {"object": {
            "id": "sub_commerce_lifecycle",
            "canceled_at": canceled_at
        }}
    });
    let (msg, input) = webhook_msg(&deleted, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);

    let purchase = repo::purchases::get(&ctx, "purchase_subscription_lifecycle")
        .await
        .unwrap();
    assert_eq!(purchase.data["subscription_status"], "canceled");
    assert_eq!(
        purchase.data["subscription_canceled_at"],
        chrono::DateTime::<chrono::Utc>::from_timestamp(canceled_at, 0)
            .unwrap()
            .to_rfc3339()
    );
    assert!(
        purchase.data["subscription_cancel_at_period_end"].as_bool() == Some(false)
            || purchase.data["subscription_cancel_at_period_end"].as_i64() == Some(0)
    );
}

/// API versions from 2025-03 (incl. the pinned Clover default) drop
/// `current_period_end` from the subscription top level (it moves onto the
/// items) and express a Billing-Portal "cancel at period end" as a concrete
/// `cancel_at` timestamp while the legacy boolean stays false. The sync must
/// read both newer shapes or every Clover deployment reports "no period end"
/// and never flags scheduled cancellations.
#[tokio::test]
async fn commerce_subscription_sync_reads_clover_item_periods_and_cancel_at() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_subscription_clover",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_clover")),
            (
                "buyer_user_id".to_string(),
                serde_json::json!("buyer_clover"),
            ),
            ("status".to_string(), serde_json::json!("completed")),
            ("total_cents".to_string(), serde_json::json!(900)),
            ("currency".to_string(), serde_json::json!("NZD")),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_commerce_clover"),
            ),
            ("stripe_account_id".to_string(), serde_json::json!("")),
            ("livemode".to_string(), serde_json::json!(false)),
            (
                "subscription_status".to_string(),
                serde_json::json!("active"),
            ),
        ]),
    )
    .await;
    let item_period_end = 2_100_000_000_i64;
    let updated = serde_json::json!({
        "id": "evt_commerce_subscription_clover",
        "type": "customer.subscription.updated",
        "livemode": false,
        "data": {"object": {
            "id": "sub_commerce_clover",
            "status": "active",
            "cancel_at_period_end": false,
            "cancel_at": item_period_end,
            "canceled_at": 2_000_000_500_i64,
            "items": {"data": [
                {"current_period_end": item_period_end - 600},
                {"current_period_end": item_period_end}
            ]}
        }}
    });
    let (msg, input) = webhook_msg(&updated, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);

    let purchase = repo::purchases::get(&ctx, "purchase_subscription_clover")
        .await
        .unwrap();
    assert_eq!(
        purchase.data["subscription_current_period_end"],
        chrono::DateTime::<chrono::Utc>::from_timestamp(item_period_end, 0)
            .unwrap()
            .to_rfc3339()
    );
    assert!(
        purchase.data["subscription_cancel_at_period_end"].as_bool() == Some(true)
            || purchase.data["subscription_cancel_at_period_end"].as_i64() == Some(1)
    );
    assert_eq!(purchase.data["subscription_status"], "active");
}

#[tokio::test]
async fn commerce_invoice_events_recover_past_due_without_resurrecting_or_reordering_state() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_invoice_ordering",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_invoice")),
            ("status".to_string(), serde_json::json!("completed")),
            ("total_cents".to_string(), serde_json::json!(2500)),
            ("currency".to_string(), serde_json::json!("NZD")),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_invoice_ordering"),
            ),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_invoice_seller"),
            ),
            ("livemode".to_string(), serde_json::json!(true)),
            (
                "subscription_status".to_string(),
                serde_json::json!("active"),
            ),
        ]),
    )
    .await;
    seed(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_platform_invoice_ordering",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("platform_buyer")),
            (
                "stripe_customer_id".to_string(),
                serde_json::json!("cus_platform_invoice"),
            ),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_invoice_ordering"),
            ),
            ("plan".to_string(), serde_json::json!("pro")),
            ("status".to_string(), serde_json::json!("active")),
            ("stripe_event_created".to_string(), serde_json::json!(100)),
        ]),
    )
    .await;

    let event = |id: &str, kind: &str, created: i64| {
        serde_json::json!({
            "id": id,
            "type": kind,
            "created": created,
            "account": "acct_invoice_seller",
            "livemode": true,
            "data": {"object": {
                "parent": {"subscription_details": {
                    "subscription": "sub_invoice_ordering"
                }}
            }}
        })
    };

    let failed = event("evt_invoice_failed_new", "invoice.payment_failed", 200);
    let (msg, input) = webhook_msg(&failed, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let purchase = repo::purchases::get(&ctx, "purchase_invoice_ordering")
        .await
        .unwrap();
    assert_eq!(purchase.data["subscription_status"], "past_due");
    assert_eq!(purchase.data["subscription_event_created"], 200);
    let platform = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_platform_invoice_ordering",
    )
    .await
    .unwrap();
    assert_eq!(platform.data["status"], "past_due");
    assert_eq!(platform.data["stripe_event_created"], 200);

    let paid = event("evt_invoice_paid_new", "invoice.paid", 300);
    let (msg, input) = webhook_msg(&paid, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let purchase = repo::purchases::get(&ctx, "purchase_invoice_ordering")
        .await
        .unwrap();
    assert_eq!(purchase.data["subscription_status"], "active");
    assert_eq!(purchase.data["subscription_event_created"], 300);
    let platform = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_platform_invoice_ordering",
    )
    .await
    .unwrap();
    assert_eq!(platform.data["status"], "active");
    assert_eq!(platform.data["stripe_event_created"], 300);

    // A late older failure is acknowledged but cannot overwrite the newer
    // successful invoice projection.
    let stale = event("evt_invoice_failed_stale", "invoice.payment_failed", 250);
    let (msg, input) = webhook_msg(&stale, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let purchase = repo::purchases::get(&ctx, "purchase_invoice_ordering")
        .await
        .unwrap();
    assert_eq!(purchase.data["subscription_status"], "active");
    assert_eq!(purchase.data["subscription_event_created"], 300);
    let platform = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_platform_invoice_ordering",
    )
    .await
    .unwrap();
    assert_eq!(platform.data["status"], "active");
    assert_eq!(platform.data["stripe_event_created"], 300);

    let deleted = serde_json::json!({
        "id": "evt_subscription_deleted_new",
        "type": "customer.subscription.deleted",
        "created": 400,
        "account": "acct_invoice_seller",
        "livemode": true,
        "data": {"object": {
            "id": "sub_invoice_ordering",
            "canceled_at": 400
        }}
    });
    let (msg, input) = webhook_msg(&deleted, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );

    // Paying a final invoice after cancellation is not evidence that the
    // subscription itself became active again.
    let final_paid = event("evt_final_invoice_paid", "invoice.payment_succeeded", 500);
    let (msg, input) = webhook_msg(&final_paid, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let purchase = repo::purchases::get(&ctx, "purchase_invoice_ordering")
        .await
        .unwrap();
    assert_eq!(purchase.data["subscription_status"], "canceled");
    assert_eq!(purchase.data["subscription_event_created"], 400);
    let platform = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_platform_invoice_ordering",
    )
    .await
    .unwrap();
    assert_eq!(platform.data["status"], "canceled");
    assert_eq!(platform.data["stripe_event_created"], 400);
}

/// Immediate cancellation makes Stripe emit `customer.subscription.updated`
/// (still "active") and `customer.subscription.deleted` with the same
/// `created` second. Whichever order they are delivered in, the deletion is
/// authoritative: an equal-second update may never move either projection
/// away from the terminal status.
#[tokio::test]
async fn same_second_subscription_update_cannot_resurrect_deleted_subscription() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_same_second",
        HashMap::from([
            (
                "user_id".to_string(),
                serde_json::json!("buyer_same_second"),
            ),
            ("status".to_string(), serde_json::json!("completed")),
            ("total_cents".to_string(), serde_json::json!(2500)),
            ("currency".to_string(), serde_json::json!("NZD")),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_same_second"),
            ),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_same_second"),
            ),
            ("livemode".to_string(), serde_json::json!(true)),
            (
                "subscription_status".to_string(),
                serde_json::json!("active"),
            ),
            (
                "subscription_event_created".to_string(),
                serde_json::json!(100),
            ),
        ]),
    )
    .await;
    seed(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_platform_same_second",
        HashMap::from([
            (
                "user_id".to_string(),
                serde_json::json!("platform_same_second"),
            ),
            (
                "stripe_customer_id".to_string(),
                serde_json::json!("cus_same_second"),
            ),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_same_second"),
            ),
            ("plan".to_string(), serde_json::json!("pro")),
            ("status".to_string(), serde_json::json!("active")),
            ("stripe_event_created".to_string(), serde_json::json!(100)),
        ]),
    )
    .await;

    let deleted = serde_json::json!({
        "id": "evt_same_second_deleted",
        "type": "customer.subscription.deleted",
        "created": 200,
        "account": "acct_same_second",
        "livemode": true,
        "data": {"object": {
            "id": "sub_same_second",
            "canceled_at": 200
        }}
    });
    let (msg, input) = webhook_msg(&deleted, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );

    // The lingering same-second snapshot still says "active".
    let updated = serde_json::json!({
        "id": "evt_same_second_updated",
        "type": "customer.subscription.updated",
        "created": 200,
        "account": "acct_same_second",
        "livemode": true,
        "data": {"object": {
            "id": "sub_same_second",
            "status": "active",
            "cancel_at_period_end": false
        }}
    });
    let (msg, input) = webhook_msg(&updated, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );

    let purchase = repo::purchases::get(&ctx, "purchase_same_second")
        .await
        .unwrap();
    assert_eq!(purchase.data["subscription_status"], "canceled");
    assert_eq!(purchase.data["subscription_event_created"], 200);
    let platform = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_platform_same_second",
    )
    .await
    .unwrap();
    assert_eq!(platform.data["status"], "canceled");
    assert_eq!(platform.data["stripe_event_created"], 200);
}

/// A commerce order only gains its `stripe_subscription_id` when
/// `checkout.session.completed` reconciles, so a subscription event delivered
/// ahead of the (retried) completion matches nothing yet. It must fail as
/// retryable — not be sealed as processed — and apply once the redelivery
/// finds the linked order.
#[tokio::test]
async fn prelink_subscription_event_retries_until_checkout_completion_links_it() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_prelink",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_prelink")),
            (
                "buyer_user_id".to_string(),
                serde_json::json!("buyer_prelink"),
            ),
            ("status".to_string(), serde_json::json!("checkout_started")),
            ("total_cents".to_string(), serde_json::json!(2500)),
            ("subtotal_cents".to_string(), serde_json::json!(2500)),
            ("currency".to_string(), serde_json::json!("NZD")),
            ("stripe_account_id".to_string(), serde_json::json!("")),
            ("livemode".to_string(), serde_json::json!(true)),
            (
                "provider_session_id".to_string(),
                serde_json::json!("cs_prelink"),
            ),
            (
                "metadata".to_string(),
                serde_json::json!(serde_json::json!({
                    "schema_version": 1,
                    "offer_id": "offer_pro_monthly",
                    "offer_version": 3,
                    "offer_mode": "subscription",
                    "allowed_shipping_amounts_minor": [0]
                })
                .to_string()),
            ),
            (
                "reconciliation_status".to_string(),
                serde_json::json!("awaiting_payment"),
            ),
        ]),
    )
    .await;
    seed(
        &ctx,
        "impresspress__products__line_items",
        "line_prelink",
        HashMap::from([
            (
                "purchase_id".to_string(),
                serde_json::json!("purchase_prelink"),
            ),
            ("product_id".to_string(), serde_json::json!("product_pro")),
            ("product_name".to_string(), serde_json::json!("Pro plan")),
            (
                "offer_id".to_string(),
                serde_json::json!("offer_pro_monthly"),
            ),
            (
                "component_id".to_string(),
                serde_json::json!("component_base"),
            ),
            ("quantity".to_string(), serde_json::json!(1)),
            ("unit_amount_minor".to_string(), serde_json::json!(2500)),
            ("total_minor".to_string(), serde_json::json!(2500)),
            ("offer_version".to_string(), serde_json::json!(3)),
        ]),
    )
    .await;

    // The subscription event races ahead of its checkout completion: nothing
    // references sub_prelink yet, so sealing it would lose the state change.
    let updated = serde_json::json!({
        "id": "evt_prelink_updated",
        "type": "customer.subscription.updated",
        "created": 200,
        "livemode": true,
        "data": {"object": {
            "id": "sub_prelink",
            "status": "past_due",
            "cancel_at_period_end": false
        }}
    });
    let (msg, input) = webhook_msg(&updated, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::Internal,
        )
        .await,
        "a subscription event matching no local subscription must be retried, not sealed"
    );
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_prelink_updated",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "failed");
    assert!(!event_row.str_field("next_retry_at").is_empty());
    assert!(event_row.str_field("last_error").contains("sub_prelink"));
    assert!(event_row.str_field("last_error").contains("out-of-order"));
    let purchase = repo::purchases::get(&ctx, "purchase_prelink")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "checkout_started");

    // The retried checkout completion finally links the subscription.
    let completed = serde_json::json!({
        "id": "evt_prelink_completed",
        "type": "checkout.session.completed",
        "created": 100,
        "livemode": true,
        "data": {"object": {
            "id": "cs_prelink",
            "client_reference_id": "purchase_prelink",
            "metadata": {
                "purchase_id": "purchase_prelink",
                "offer_id": "offer_pro_monthly",
                "offer_version": "3"
            },
            "mode": "subscription",
            "payment_status": "paid",
            "currency": "nzd",
            "amount_subtotal": 2500,
            "amount_total": 2500,
            "total_details": {
                "amount_discount": 0,
                "amount_tax": 0,
                "amount_shipping": 0
            },
            "payment_intent": null,
            "customer": "cus_prelink",
            "subscription": "sub_prelink",
            "livemode": true
        }}
    });
    let (msg, input) = webhook_msg(&completed, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let purchase = repo::purchases::get(&ctx, "purchase_prelink")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "completed");
    assert_eq!(purchase.data["stripe_subscription_id"], "sub_prelink");
    assert_eq!(purchase.data["subscription_status"], "active");

    // Stripe redelivers the failed event after its backoff window; rewind the
    // retry gate the way the scheduler would observe it after the delay.
    db::update(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_prelink_updated",
        HashMap::from([(
            "next_retry_at".to_string(),
            serde_json::json!("2000-01-01T00:00:00Z"),
        )]),
    )
    .await
    .unwrap();
    let (msg, input) = webhook_msg(&updated, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let purchase = repo::purchases::get(&ctx, "purchase_prelink")
        .await
        .unwrap();
    assert_eq!(purchase.data["subscription_status"], "past_due");
    assert_eq!(purchase.data["subscription_event_created"], 200);
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_prelink_updated",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "processed");
}

/// A failed payment on a leftover open invoice after
/// `customer.subscription.deleted` must not move either projection back to
/// `past_due` — that would resurrect access with a fresh grace window.
#[tokio::test]
async fn invoice_payment_failed_after_deletion_does_not_regress_terminal_state() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_failed_after_cancel",
        HashMap::from([
            (
                "user_id".to_string(),
                serde_json::json!("buyer_failed_after_cancel"),
            ),
            ("status".to_string(), serde_json::json!("completed")),
            ("total_cents".to_string(), serde_json::json!(2500)),
            ("currency".to_string(), serde_json::json!("NZD")),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_failed_after_cancel"),
            ),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_failed_seller"),
            ),
            ("livemode".to_string(), serde_json::json!(true)),
            (
                "subscription_status".to_string(),
                serde_json::json!("active"),
            ),
            (
                "subscription_event_created".to_string(),
                serde_json::json!(100),
            ),
        ]),
    )
    .await;
    seed(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_platform_failed_after_cancel",
        HashMap::from([
            (
                "user_id".to_string(),
                serde_json::json!("platform_failed_after_cancel"),
            ),
            (
                "stripe_customer_id".to_string(),
                serde_json::json!("cus_failed_after_cancel"),
            ),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_failed_after_cancel"),
            ),
            ("plan".to_string(), serde_json::json!("pro")),
            ("status".to_string(), serde_json::json!("active")),
            ("stripe_event_created".to_string(), serde_json::json!(100)),
        ]),
    )
    .await;

    let deleted = serde_json::json!({
        "id": "evt_failed_after_cancel_deleted",
        "type": "customer.subscription.deleted",
        "created": 300,
        "account": "acct_failed_seller",
        "livemode": true,
        "data": {"object": {
            "id": "sub_failed_after_cancel",
            "canceled_at": 300
        }}
    });
    let (msg, input) = webhook_msg(&deleted, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );

    // The final open invoice fails with a strictly newer timestamp.
    let payment_failed = serde_json::json!({
        "id": "evt_failed_after_cancel_invoice",
        "type": "invoice.payment_failed",
        "created": 400,
        "account": "acct_failed_seller",
        "livemode": true,
        "data": {"object": {
            "parent": {"subscription_details": {
                "subscription": "sub_failed_after_cancel"
            }}
        }}
    });
    let (msg, input) = webhook_msg(&payment_failed, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );

    let purchase = repo::purchases::get(&ctx, "purchase_failed_after_cancel")
        .await
        .unwrap();
    assert_eq!(purchase.data["subscription_status"], "canceled");
    assert_eq!(purchase.data["subscription_event_created"], 300);
    let platform = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_platform_failed_after_cancel",
    )
    .await
    .unwrap();
    assert_eq!(platform.data["status"], "canceled");
    assert_eq!(platform.data["stripe_event_created"], 300);
    assert!(
        platform.data["grace_period_end"]
            .as_str()
            .unwrap_or("")
            .is_empty(),
        "a refused past-due write must not grant a fresh grace window"
    );
}

#[tokio::test]
async fn platform_subscription_checkout_is_ordered_and_allows_newer_resubscription() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    let checkout = |id: &str, created: i64, plan: &str, customer: &str, subscription: &str| {
        serde_json::json!({
            "id": id,
            "type": "checkout.session.completed",
            "created": created,
            "livemode": false,
            "data": {"object": {
                "id": format!("cs_{id}"),
                "payment_status": "paid",
                "livemode": false,
                "metadata": {
                    "user_id": "platform_ordering",
                    "plan": plan
                },
                "customer": customer,
                "subscription": subscription
            }}
        })
    };

    let initial = checkout(
        "evt_platform_checkout_initial",
        100,
        "starter",
        "cus_platform_initial",
        "sub_platform_initial",
    );
    let (msg, input) = webhook_msg(&initial, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );

    let updated = serde_json::json!({
        "id": "evt_platform_subscription_updated",
        "type": "customer.subscription.updated",
        "created": 300,
        "livemode": false,
        "data": {"object": {
            "id": "sub_platform_initial",
            "status": "past_due",
            "items": {"data": [{"price": {"lookup_key": "pro"}}]}
        }}
    });
    let (msg, input) = webhook_msg(&updated, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );

    let stale = checkout(
        "evt_platform_checkout_stale",
        200,
        "stale-plan",
        "cus_platform_stale",
        "sub_platform_stale",
    );
    let (msg, input) = webhook_msg(&stale, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let subscription = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_platform_ordering",
    )
    .await
    .unwrap();
    assert_eq!(
        subscription.data["stripe_customer_id"],
        "cus_platform_initial"
    );
    assert_eq!(
        subscription.data["stripe_subscription_id"],
        "sub_platform_initial"
    );
    assert_eq!(subscription.data["plan"], "pro");
    assert_eq!(subscription.data["status"], "past_due");
    assert_eq!(subscription.data["stripe_event_created"], 300);

    let deleted = serde_json::json!({
        "id": "evt_platform_subscription_deleted",
        "type": "customer.subscription.deleted",
        "created": 400,
        "livemode": false,
        "data": {"object": {
            "id": "sub_platform_initial",
            "status": "canceled",
            "canceled_at": 400
        }}
    });
    let (msg, input) = webhook_msg(&deleted, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );

    let stale_after_cancel = checkout(
        "evt_platform_checkout_stale_after_cancel",
        350,
        "stale-after-cancel",
        "cus_platform_stale_after_cancel",
        "sub_platform_stale_after_cancel",
    );
    let (msg, input) = webhook_msg(&stale_after_cancel, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let subscription = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_platform_ordering",
    )
    .await
    .unwrap();
    assert_eq!(subscription.data["status"], "canceled");
    assert_eq!(subscription.data["stripe_event_created"], 400);

    let resubscribe = checkout(
        "evt_platform_checkout_resubscribe",
        500,
        "team",
        "cus_platform_new",
        "sub_platform_new",
    );
    let (msg, input) = webhook_msg(&resubscribe, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let subscription = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_platform_ordering",
    )
    .await
    .unwrap();
    assert_eq!(subscription.data["stripe_customer_id"], "cus_platform_new");
    assert_eq!(
        subscription.data["stripe_subscription_id"],
        "sub_platform_new"
    );
    assert_eq!(subscription.data["plan"], "team");
    assert_eq!(subscription.data["status"], "active");
    assert_eq!(subscription.data["stripe_event_created"], 500);
}

#[tokio::test]
async fn platform_subscription_write_failure_is_not_acknowledged() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await
    .break_writes();
    // Omitting the event id avoids the durable lease write so the injected
    // fault reaches the platform-subscription mutation under test.
    let checkout = serde_json::json!({
        "type": "checkout.session.completed",
        "created": 100,
        "livemode": false,
        "data": {"object": {
            "id": "cs_platform_write_failure",
            "payment_status": "paid",
            "livemode": false,
            "metadata": {
                "user_id": "platform_write_failure",
                "plan": "pro"
            },
            "customer": "cus_platform_write_failure",
            "subscription": "sub_platform_write_failure"
        }}
    });
    let (msg, input) = webhook_msg(&checkout, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::Internal,
        )
        .await,
        "Stripe must retry when the platform subscription projection cannot be persisted"
    );
}

#[tokio::test]
async fn commerce_subscription_webhooks_reject_account_and_mode_mismatch() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_subscription_identity",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_identity")),
            ("status".to_string(), serde_json::json!("completed")),
            ("total_cents".to_string(), serde_json::json!(5000)),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_commerce_identity"),
            ),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_expected"),
            ),
            ("livemode".to_string(), serde_json::json!(true)),
            (
                "subscription_status".to_string(),
                serde_json::json!("active"),
            ),
        ]),
    )
    .await;

    for (event_id, account, livemode) in [
        ("evt_subscription_wrong_account", "acct_attacker", true),
        ("evt_subscription_wrong_mode", "acct_expected", false),
    ] {
        let event = serde_json::json!({
            "id": event_id,
            "type": "customer.subscription.updated",
            "account": account,
            "livemode": livemode,
            "data": {"object": {
                "id": "sub_commerce_identity",
                "status": "canceled",
                "cancel_at_period_end": false
            }}
        });
        let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
        assert!(
            output_is_error(
                stripe::handle_webhook(&ctx, &msg, input).await,
                ErrorCode::Internal
            )
            .await
        );
    }

    let purchase = repo::purchases::get(&ctx, "purchase_subscription_identity")
        .await
        .unwrap();
    assert_eq!(purchase.data["subscription_status"], "active");
    assert!(purchase.data["subscription_last_synced_at"]
        .as_str()
        .unwrap_or("")
        .is_empty());
}

#[tokio::test]
async fn webhook_account_updated_refreshes_and_revokes_seller_charge_capability() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_account_webhook",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("seller_webhook")),
            ("status".to_string(), serde_json::json!("onboarding")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_seller_webhook"),
            ),
            ("fee_basis_points".to_string(), serde_json::json!(175)),
        ]),
    )
    .await;

    let active = serde_json::json!({
        "id": "evt_account_active",
        "type": "account.updated",
        "created": 100,
        "account": "acct_seller_webhook",
        "livemode": true,
        "data": {"object": {
            "id": "acct_seller_webhook",
            "country": "nz",
            "default_currency": "nzd",
            "details_submitted": true,
            "charges_enabled": true,
            "payouts_enabled": true,
            "controller": {"stripe_dashboard": {"type": "express"}},
            "requirements": {"currently_due": [], "disabled_reason": null}
        }}
    });
    let (msg, input) = webhook_msg(&active, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    let local = repo::seller_accounts::get_for_user(&ctx, "seller_webhook")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(local.data["status"], "active");
    assert_eq!(local.data["country"], "NZ");
    assert_eq!(local.data["default_currency"], "NZD");
    assert_eq!(local.data["dashboard_type"], "express");
    assert!(
        local.data["livemode"].as_bool() == Some(true)
            || local.data["livemode"].as_i64() == Some(1)
    );
    assert!(
        repo::seller_accounts::ready_for_user(&ctx, "seller_webhook")
            .await
            .is_ok()
    );

    let restricted = serde_json::json!({
        "id": "evt_account_restricted",
        "type": "account.updated",
        "created": 200,
        "account": "acct_seller_webhook",
        "livemode": true,
        "data": {"object": {
            "id": "acct_seller_webhook",
            "country": "NZ",
            "default_currency": "nzd",
            "details_submitted": true,
            "charges_enabled": false,
            "payouts_enabled": false,
            "controller": {"stripe_dashboard": {"type": "express"}},
            "requirements": {
                "currently_due": ["individual.verification.document"],
                "disabled_reason": "requirements.past_due"
            }
        }}
    });
    let (msg, input) = webhook_msg(&restricted, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    let local = repo::seller_accounts::get_for_user(&ctx, "seller_webhook")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(local.data["status"], "restricted");
    assert_eq!(
        local.data["requirements_disabled_reason"],
        "requirements.past_due"
    );
    assert!(
        repo::seller_accounts::ready_for_user(&ctx, "seller_webhook")
            .await
            .is_err()
    );

    for (event_id, created) in [
        ("evt_account_stale_active", 150),
        ("evt_account_tied_active", 200),
    ] {
        let stale_or_tied = serde_json::json!({
            "id": event_id,
            "type": "account.updated",
            "created": created,
            "account": "acct_seller_webhook",
            "livemode": true,
            "data": {"object": {
                "id": "acct_seller_webhook",
                "country": "NZ",
                "default_currency": "nzd",
                "details_submitted": true,
                "charges_enabled": true,
                "payouts_enabled": true,
                "controller": {"stripe_dashboard": {"type": "express"}},
                "requirements": {"currently_due": [], "disabled_reason": null}
            }}
        });
        let (msg, input) = webhook_msg(&stale_or_tied, WEBHOOK_SECRET);
        let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
        assert_eq!(body["received"], true);
    }
    let local = repo::seller_accounts::get_for_user(&ctx, "seller_webhook")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(local.data["status"], "restricted");
    assert_eq!(local.data["stripe_event_created"], 200);

    let newer_active = serde_json::json!({
        "id": "evt_account_newer_active",
        "type": "account.updated",
        "created": 300,
        "account": "acct_seller_webhook",
        "livemode": true,
        "data": {"object": {
            "id": "acct_seller_webhook",
            "country": "NZ",
            "default_currency": "nzd",
            "details_submitted": true,
            "charges_enabled": true,
            "payouts_enabled": true,
            "controller": {"stripe_dashboard": {"type": "express"}},
            "requirements": {"currently_due": [], "disabled_reason": null}
        }}
    });
    let (msg, input) = webhook_msg(&newer_active, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    let local = repo::seller_accounts::get_for_user(&ctx, "seller_webhook")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(local.data["status"], "active");
    assert_eq!(local.data["stripe_event_created"], 300);
}

// ============================================================
// Webhook — charge.refunded
// ============================================================

#[tokio::test]
async fn webhook_charge_refunded_marks_purchase() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    // Seed a completed purchase with a payment intent
    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("completed"));
    pd.insert("total_cents".to_string(), serde_json::json!(5000));
    pd.insert(
        "provider_payment_intent_id".to_string(),
        serde_json::json!("pi_refund_test"),
    );
    seed(&ctx, "impresspress__products__purchases", "pur_ref1", pd).await;

    let event = charge_refunded_event("pi_refund_test");
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);

    let out = stripe::handle_webhook(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["received"], true);
    let purchase = repo::purchases::get(&ctx, "pur_ref1").await.unwrap();
    assert_eq!(purchase.data["status"], "refunded");
    assert_eq!(purchase.data["refunded_total_cents"], 5000);
}

#[tokio::test]
async fn webhook_charge_refunded_unknown_intent_is_noop() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    // No matching purchase — should still return 200
    let event = charge_refunded_event("pi_unknown");
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);

    let out = stripe::handle_webhook(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["received"], true);
}

/// Only a definitive NotFound may treat `charge.refunded` as a foreign
/// charge. A transient purchase-lookup outage must fail the delivery
/// retryably — sealing the event would permanently drop a dashboard-initiated
/// refund locally.
#[tokio::test]
async fn webhook_charge_refunded_lookup_outage_is_retried_not_sealed() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_refund_outage",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_outage")),
            ("status".to_string(), serde_json::json!("completed")),
            ("provider".to_string(), serde_json::json!("stripe")),
            ("total_cents".to_string(), serde_json::json!(5000)),
            (
                "provider_payment_intent_id".to_string(),
                serde_json::json!("pi_refund_outage"),
            ),
        ]),
    )
    .await;
    let event = serde_json::json!({
        "id": "evt_refund_outage",
        "type": "charge.refunded",
        "livemode": false,
        "data": {"object": {
            "payment_intent": "pi_refund_outage",
            "amount": 5000,
            "amount_refunded": 5000,
            "refunded": true,
            "livemode": false
        }}
    });

    // The purchase lookup hits a simulated database outage: the event must
    // fail with a scheduled retry, not be sealed as processed.
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx.clone(),
        vec![("database.list", repo::purchases::PURCHASES_TABLE)],
    );
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&failing, &msg, input).await,
            ErrorCode::Internal,
        )
        .await
    );
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_refund_outage",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "failed");
    assert!(!event_row.str_field("next_retry_at").is_empty());
    let purchase = repo::purchases::get(&ctx, "purchase_refund_outage")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "completed");

    // Stripe redelivers after the backoff window and the refund lands.
    db::update(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_refund_outage",
        HashMap::from([(
            "next_retry_at".to_string(),
            serde_json::json!("2000-01-01T00:00:00Z"),
        )]),
    )
    .await
    .unwrap();
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    let purchase = repo::purchases::get(&ctx, "purchase_refund_outage")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "refunded");
    assert_eq!(purchase.data["refunded_total_cents"], 5000);
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_refund_outage",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "processed");
}

#[tokio::test]
async fn webhook_charge_partial_refund_reconciles_cumulative_amount_without_full_mark() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_webhook_partial",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_partial")),
            ("status".to_string(), serde_json::json!("completed")),
            ("provider".to_string(), serde_json::json!("stripe")),
            ("total_cents".to_string(), serde_json::json!(5000)),
            (
                "provider_payment_intent_id".to_string(),
                serde_json::json!("pi_webhook_partial"),
            ),
        ]),
    )
    .await;
    for (event_id, cumulative) in [("evt_partial_1", 1200), ("evt_partial_2", 3000)] {
        let event = serde_json::json!({
            "id": event_id,
            "type": "charge.refunded",
            "livemode": false,
            "data": {"object": {
                "payment_intent": "pi_webhook_partial",
                "amount": 5000,
                "amount_refunded": cumulative,
                "refunded": false,
                "livemode": false
            }}
        });
        let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
        let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
        assert_eq!(body["received"], true);
    }
    let purchase = repo::purchases::get(&ctx, "purchase_webhook_partial")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "partially_refunded");
    assert_eq!(purchase.data["refunded_total_cents"], 3000);
}

#[tokio::test]
async fn webhook_refund_updated_completes_pending_ledger_and_purchase() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_refund_updated",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_refund")),
            ("status".to_string(), serde_json::json!("completed")),
            ("provider".to_string(), serde_json::json!("stripe")),
            ("total_cents".to_string(), serde_json::json!(5000)),
            (
                "provider_payment_intent_id".to_string(),
                serde_json::json!("pi_refund_updated"),
            ),
        ]),
    )
    .await;
    let ledger = repo::refunds::claim(
        &ctx,
        &repo::refunds::RefundClaim {
            purchase_id: "purchase_refund_updated".to_string(),
            payment_intent_id: "pi_refund_updated".to_string(),
            stripe_account_id: String::new(),
            idempotency_key: "impresspress_refund_purchase_refund_updated_webhook".to_string(),
            amount_minor: 1000,
            target_refunded_total_minor: 1000,
            currency: "NZD".to_string(),
            provider_reason: "requested_by_customer".to_string(),
            note: "Webhook pending refund".to_string(),
            refunded_by: "admin_1".to_string(),
            livemode: false,
        },
    )
    .await
    .unwrap();
    repo::refunds::record_provider_response(
        &ctx,
        &ledger.id,
        "re_webhook_pending",
        "pending",
        false,
        "{}",
    )
    .await
    .unwrap();
    let event = serde_json::json!({
        "id": "evt_refund_updated_success",
        "type": "refund.updated",
        "created": 200,
        "livemode": false,
        "data": {"object": {
            "id": "re_webhook_pending",
            "payment_intent": "pi_refund_updated",
            "amount": 1000,
            "currency": "nzd",
            "status": "succeeded",
            "livemode": false
        }}
    });
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    let purchase = repo::purchases::get(&ctx, "purchase_refund_updated")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "partially_refunded");
    assert_eq!(purchase.data["refunded_total_cents"], 1000);
    assert_eq!(purchase.data["refund_reason"], "Webhook pending refund");
    let ledger = repo::refunds::get_by_provider_refund_id(&ctx, "re_webhook_pending")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ledger.data["status"], "succeeded");
    assert_eq!(ledger.data["provider_status"], "succeeded");
    assert_eq!(ledger.data["stripe_event_created"], 200);

    for (event_id, created) in [
        ("evt_refund_stale_pending", 100),
        ("evt_refund_tied_pending", 200),
    ] {
        let pending = serde_json::json!({
            "id": event_id,
            "type": "refund.updated",
            "created": created,
            "livemode": false,
            "data": {"object": {
                "id": "re_webhook_pending",
                "payment_intent": "pi_refund_updated",
                "amount": 1000,
                "currency": "NZD",
                "status": "pending",
                "livemode": false
            }}
        });
        let (msg, input) = webhook_msg(&pending, WEBHOOK_SECRET);
        let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
        assert_eq!(body["received"], true);
    }
    let ledger = repo::refunds::get_by_provider_refund_id(&ctx, "re_webhook_pending")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ledger.data["status"], "succeeded");
    assert_eq!(ledger.data["provider_status"], "succeeded");
    assert_eq!(ledger.data["stripe_event_created"], 200);
}

#[tokio::test]
async fn refund_webhooks_reject_account_and_amount_tampering_without_state_change() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_refund_tamper",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_refund")),
            ("status".to_string(), serde_json::json!("completed")),
            ("provider".to_string(), serde_json::json!("stripe")),
            ("total_cents".to_string(), serde_json::json!(5000)),
            (
                "provider_payment_intent_id".to_string(),
                serde_json::json!("pi_refund_tamper"),
            ),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_expected"),
            ),
        ]),
    )
    .await;
    let wrong_account = serde_json::json!({
        "id": "evt_refund_wrong_account",
        "type": "charge.refunded",
        "account": "acct_attacker",
        "data": {"object": {
            "payment_intent": "pi_refund_tamper",
            "amount": 5000,
            "amount_refunded": 1000,
            "refunded": false
        }}
    });
    let (msg, input) = webhook_msg(&wrong_account, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::Internal
        )
        .await
    );

    let wrong_total = serde_json::json!({
        "id": "evt_refund_wrong_total",
        "type": "charge.refunded",
        "account": "acct_expected",
        "data": {"object": {
            "payment_intent": "pi_refund_tamper",
            "amount": 9000,
            "amount_refunded": 1000,
            "refunded": false
        }}
    });
    let (msg, input) = webhook_msg(&wrong_total, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::Internal
        )
        .await
    );
    let purchase = repo::purchases::get(&ctx, "purchase_refund_tamper")
        .await
        .unwrap();
    assert_eq!(purchase.data["status"], "completed");
    assert_eq!(purchase.data["refunded_total_cents"], 0);
}

#[tokio::test]
async fn dispute_webhooks_are_ordered_tenant_safe_and_immutable() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "purchase_dispute",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_dispute")),
            ("status".to_string(), serde_json::json!("completed")),
            ("provider".to_string(), serde_json::json!("stripe")),
            ("total_cents".to_string(), serde_json::json!(5000)),
            ("currency".to_string(), serde_json::json!("NZD")),
            (
                "provider_payment_intent_id".to_string(),
                serde_json::json!("pi_dispute"),
            ),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_dispute_seller"),
            ),
            (
                "seller_account_id".to_string(),
                serde_json::json!("seller_dispute"),
            ),
            ("livemode".to_string(), serde_json::json!(true)),
        ]),
    )
    .await;

    let dispute = |id: &str, kind: &str, created: i64, status: &str, amount: i64| {
        serde_json::json!({
            "id": id,
            "type": kind,
            "created": created,
            "account": "acct_dispute_seller",
            "livemode": true,
            "data": {"object": {
                "id": "dp_dispute",
                "payment_intent": {"id": "pi_dispute"},
                "charge": "ch_dispute",
                "status": status,
                "amount": amount,
                "currency": "nzd",
                "reason": "fraudulent",
                "livemode": true,
                "evidence_details": {"due_by": 2_000_000_000_i64}
            }}
        })
    };

    let created = dispute(
        "evt_dispute_created",
        "charge.dispute.created",
        100,
        "needs_response",
        2500,
    );
    let (msg, input) = webhook_msg(&created, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let records = repo::disputes::list_for_purchase(&ctx, "purchase_dispute")
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].data["status"], "needs_response");
    assert_eq!(records[0].data["amount_minor"], 2500);
    assert_eq!(records[0].data["currency"], "NZD");
    assert_eq!(records[0].data["seller_account_id"], "seller_dispute");
    assert_eq!(records[0].data["event_created"], 100);
    assert!(records[0].data["evidence_due_by"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));

    let reviewed = dispute(
        "evt_dispute_reviewed",
        "charge.dispute.updated",
        300,
        "under_review",
        2500,
    );
    let (msg, input) = webhook_msg(&reviewed, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let stale = dispute(
        "evt_dispute_stale",
        "charge.dispute.updated",
        200,
        "needs_response",
        2500,
    );
    let (msg, input) = webhook_msg(&stale, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let records = repo::disputes::list_for_purchase(&ctx, "purchase_dispute")
        .await
        .unwrap();
    assert_eq!(records[0].data["status"], "under_review");
    assert_eq!(records[0].data["event_created"], 300);

    // Provider identity and immutable amount changes fail the webhook lease so
    // Stripe can retry after operator investigation.
    let mut wrong_account = dispute(
        "evt_dispute_wrong_account",
        "charge.dispute.updated",
        350,
        "under_review",
        2500,
    );
    wrong_account["account"] = serde_json::json!("acct_attacker");
    let (msg, input) = webhook_msg(&wrong_account, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::Internal
        )
        .await
    );
    let changed_amount = dispute(
        "evt_dispute_changed_amount",
        "charge.dispute.updated",
        360,
        "under_review",
        2000,
    );
    let (msg, input) = webhook_msg(&changed_amount, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::Internal
        )
        .await
    );

    let closed = dispute(
        "evt_dispute_closed",
        "charge.dispute.closed",
        400,
        "won",
        2500,
    );
    let (msg, input) = webhook_msg(&closed, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let records = repo::disputes::list_for_purchase(&ctx, "purchase_dispute")
        .await
        .unwrap();
    assert_eq!(records[0].data["status"], "won");
    assert!(records[0].data["closed_at"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
}

// ============================================================
// Webhook — unhandled event types
// ============================================================

/// A type this block does not handle is ordinary traffic, and this is what
/// "ordinary" means: acknowledged as received, recorded under the type
/// Stripe actually sent, and sealed `processed` so a redelivery of the same
/// id is deduped rather than re-run.
///
/// A Stripe destination can be subscribed to more event types than any one
/// integration handles — the dashboard's "select all events" is one click —
/// so this is the common case, not an edge one. Answering anything but 2xx
/// would make Stripe retry the delivery on its backoff schedule and
/// eventually mark the destination unhealthy, for an event no handler wants.
/// The dispatcher's ignore arm was `_ =>` before this PR and is `None =>`
/// after it; this test is the same assertion across both.
#[tokio::test]
async fn webhook_unhandled_event_is_acknowledged_and_sealed() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    let event = serde_json::json!({
        "id": "evt_unhandled_type",
        "type": "payment_intent.created",
        "data": { "object": {} }
    });
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);

    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    // `duplicate` and `dead_letter` are skipped when false, so the whole
    // body is the assertion: neither flag is set.
    assert_eq!(body, serde_json::json!({ "received": true }));

    // Recorded before the dispatch, so the row holds the raw type — the
    // event_type column is Stripe's whole vocabulary, not the handled
    // subset, which is why `WebhookEventSummary.event_type` stays a string.
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_unhandled_type",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["event_type"], "payment_intent.created");
    assert_eq!(
        event_row.data["status"], "processed",
        "an unhandled type is done, not queued for retry",
    );

    // And a redelivery of the same id is deduped rather than re-processed.
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(
        body,
        serde_json::json!({ "received": true, "duplicate": true })
    );
}

// ============================================================
// Webhook — security: signature verification
// ============================================================

#[tokio::test]
async fn webhook_rejects_missing_secret_config() {
    // No STRIPE_WEBHOOK_SECRET configured
    let ctx = ctx().await;

    let event = checkout_completed_event("pur_1", "pi_1");
    let (msg, input) = webhook_msg(&event, "anything");

    let out = stripe::handle_webhook(&ctx, &msg, input).await;
    // Unavailable, not Internal: the secret being unset means webhook
    // processing is switched off, not that this deployment is broken. (It
    // does not change redelivery — Stripe retries on any non-2xx.) See
    // `err_unavailable` in `crate::http`.
    assert!(output_is_error(out, ErrorCode::Unavailable).await);
}

#[tokio::test]
async fn webhook_rejects_missing_signature_header() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    let event = checkout_completed_event("pur_1", "pi_1");
    let payload_bytes = serde_json::to_vec(&event).unwrap();
    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "create");
    msg.set_meta("req.resource", "/b/products/webhooks");
    // No stripe-signature header
    let input = InputStream::from_bytes(payload_bytes);

    let out = stripe::handle_webhook(&ctx, &msg, input).await;
    assert!(output_is_error(out, ErrorCode::Unauthenticated).await);
}

#[tokio::test]
async fn webhook_rejects_invalid_signature() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    let event = checkout_completed_event("pur_1", "pi_1");
    // Sign with wrong secret
    let (msg, input) = webhook_msg(&event, "wrong_secret");

    let out = stripe::handle_webhook(&ctx, &msg, input).await;
    assert!(output_is_error(out, ErrorCode::Unauthenticated).await);
}

#[tokio::test]
async fn webhook_rejects_tampered_payload() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    // Create a valid signature for one payload
    let original_event = checkout_completed_event("pur_1", "pi_1");
    let original_bytes = serde_json::to_vec(&original_event).unwrap();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let signed = format!("{}.{}", timestamp, String::from_utf8_lossy(&original_bytes));
    let sig_bytes = primitives::hmac_sha256(WEBHOOK_SECRET.as_bytes(), signed.as_bytes());
    let sig_hex = hex_encode(&sig_bytes);
    let sig_header = format!("t={timestamp},v1={sig_hex}");

    // But send a different payload
    let tampered_event = checkout_completed_event("pur_HACKED", "pi_evil");
    let tampered_bytes = serde_json::to_vec(&tampered_event).unwrap();

    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "create");
    msg.set_meta("req.resource", "/b/products/webhooks");
    msg.set_meta("http.header.stripe-signature", &sig_header);
    let input = InputStream::from_bytes(tampered_bytes);

    let out = stripe::handle_webhook(&ctx, &msg, input).await;
    assert!(output_is_error(out, ErrorCode::Unauthenticated).await);
}

// ============================================================
// Checkout — error cases (no network mock, just config errors)
// ============================================================

#[tokio::test]
async fn checkout_rejects_when_stripe_not_configured() {
    let ctx = ctx().await;
    // No STRIPE_SECRET_KEY configured

    let (msg, input) = create_msg(
        "/b/products/checkout",
        "user_1",
        serde_json::json!({
            "offer_id": "offer_1"
        }),
    );

    let out = stripe::handle_checkout(&ctx, &msg, input).await;
    // Unavailable, not Internal: checkout is a PUBLIC endpoint on a default
    // install with no Stripe keys, and "not configured" is not a fault.
    assert!(output_is_error(out, ErrorCode::Unavailable).await);
}

#[tokio::test]
async fn guest_offer_checkout_snapshots_components_and_sends_distinct_stripe_lines() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        ("IMPRESSPRESS__PRODUCTS__STRIPE_ACCOUNT_COUNTRY", "NZ"),
    ])
    .await;
    let requests = register_stripe_network(
        &mut ctx,
        serde_json::json!({
            "id": "cs_test_offer",
            "url": "https://checkout.stripe.com/c/pay/cs_test_offer"
        }),
    );
    let offer_id = seed_active_offer(&ctx, "product_offer_checkout", "").await;

    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({
            "offer_id": offer_id,
            "quantity": 2,
            "inputs": {"pages": 3},
            "presentation": "hosted",
            "buyer_email": "guest@example.com"
        }),
    );
    let body = output_to_json(stripe::handle_checkout(&ctx, &msg, input).await).await;
    assert_eq!(
        body["checkout_url"],
        "https://checkout.stripe.com/c/pay/cs_test_offer"
    );
    assert_eq!(body["presentation"], "hosted");
    assert_eq!(body["amounts"]["currency"], "NZD");
    assert_eq!(body["amounts"]["total_minor"], 2150);
    let order_id = body["order_id"].as_str().expect("order id");
    let receipt_token = body["receipt_token"]
        .as_str()
        .expect("one-time guest receipt token");
    assert_eq!(receipt_token.len(), 64);
    assert!(body["receipt_token_expires_at"].as_str().is_some());

    let order = db::get(&ctx, "impresspress__products__purchases", order_id)
        .await
        .expect("order snapshot");
    assert_eq!(order.data["buyer_user_id"], serde_json::json!(""));
    assert_eq!(
        order.data["buyer_email"],
        serde_json::json!("guest@example.com")
    );
    assert_eq!(order.data["subtotal_cents"], serde_json::json!(2150));
    assert_eq!(order.data["status"], serde_json::json!("checkout_started"));
    assert_eq!(
        order.data["receipt_token_hash"],
        serde_json::json!(sha256_hex(receipt_token.as_bytes()))
    );
    assert_ne!(
        order.data["receipt_token_hash"],
        serde_json::json!(receipt_token),
        "the raw capability must never be persisted"
    );
    assert_eq!(
        order.data["reconciliation_status"],
        serde_json::json!("awaiting_payment")
    );

    let items = repo::purchases::list_line_items(&ctx, order_id)
        .await
        .expect("line snapshots");
    assert_eq!(items.len(), 2);
    let exact: Vec<_> = items
        .iter()
        .map(|item| {
            (
                item.data["unit_amount_minor"].as_i64().unwrap(),
                item.data["quantity"].as_i64().unwrap(),
                item.data["total_minor"].as_i64().unwrap(),
                item.data["offer_version"].as_i64().unwrap(),
            )
        })
        .collect();
    assert!(exact.contains(&(1000, 2, 2000, 1)));
    assert!(exact.contains(&(75, 2, 150, 1)));
    assert!(items.iter().all(|item| match &item.data["input_snapshot"] {
        serde_json::Value::String(snapshot) => snapshot.contains("\"pages\":3"),
        serde_json::Value::Object(snapshot) => snapshot.get("pages") == Some(&serde_json::json!(3)),
        _ => false,
    }));

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.url, "https://api.stripe.com/v1/checkout/sessions");
    assert_eq!(request.headers["Stripe-Version"], "2026-02-25.clover");
    assert!(!request.headers.contains_key("Stripe-Account"));
    let form = String::from_utf8(request.body.clone().unwrap()).unwrap();
    assert!(form.contains("line_items[0][price_data][unit_amount]="));
    assert!(form.contains("line_items[0][quantity]=2"));
    assert!(form.contains("line_items[1][price_data][unit_amount]="));
    assert!(form.contains("line_items[1][quantity]=2"));
    assert!(form.contains("[unit_amount]=1000"));
    assert!(form.contains("[unit_amount]=75"));
    assert!(form.contains("automatic_tax[enabled]=true"));
    assert!(form.contains("billing_address_collection=required"));
    assert!(form.contains(&format!("metadata[purchase_id]={order_id}")));
    assert!(form.contains(&format!(
        "payment_intent_data[metadata][purchase_id]={order_id}"
    )));
    assert!(form.contains("payment_intent_data[metadata][offer_id]="));
    assert!(form.contains("payment_intent_data[metadata][offer_version]=1"));
    assert!(!form.contains("subscription_data[metadata]"));
}

#[tokio::test]
async fn catalog_sync_persists_fixed_prices_and_reuses_them_in_checkout_and_payment_links() {
    let mut ctx = ctx_with(&[
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_catalog",
        ),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
    ])
    .await;
    let requests = register_stripe_sequence(
        &mut ctx,
        vec![
            (
                200,
                serde_json::json!({
                    "id": "prod_catalog",
                    "livemode": false,
                    "active": true
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "price_catalog_setup",
                    "livemode": false,
                    "product": "prod_catalog",
                    "currency": "nzd",
                    "unit_amount": 1000,
                    "active": true
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "cs_test_catalog",
                    "url": "https://checkout.stripe.com/c/pay/cs_test_catalog"
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "plink_catalog",
                    "url": "https://buy.stripe.com/catalog"
                }),
            ),
        ],
    );
    let product_id = "product_catalog_sync";
    let offer_id = seed_active_offer(&ctx, product_id, "").await;

    let synced = stripe::sync_offer_catalog(&ctx, product_id, &offer_id)
        .await
        .expect("synchronize immutable fixed rows");
    assert_eq!(synced.sync_status, OfferSyncStatus::Synced);
    assert!(synced.sync_error.is_empty());
    assert_eq!(synced.offer.stripe_product_id, "prod_catalog");
    assert!(
        synced.offer.stripe_price_id.is_empty(),
        "a multi-row offer must not claim one canonical Price"
    );
    let fixed = synced
        .offer
        .components
        .iter()
        .find(|component| component.key == "setup")
        .unwrap();
    let dynamic = synced
        .offer
        .components
        .iter()
        .find(|component| component.key == "pages")
        .unwrap();
    assert_eq!(fixed.stripe_price_id, "price_catalog_setup");
    assert!(dynamic.stripe_price_id.is_empty());
    let product = db::get(&ctx, repo::products::TABLE, product_id)
        .await
        .expect("synced product row");
    assert_eq!(product.str_field("stripe_product_id"), "prod_catalog");

    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({
            "offer_id": offer_id,
            "quantity": 2,
            "inputs": {"pages": 3}
        }),
    );
    let checkout = output_to_json(stripe::handle_checkout(&ctx, &msg, input).await).await;
    assert_eq!(checkout["amounts"]["total_minor"], 2150);

    let preset = repo::checkout_presets::create(
        &ctx,
        &offer_id,
        "admin_1",
        &serde_json::from_value(serde_json::json!({
            "name": "Four pages",
            "slug": "four-pages",
            "inputs": {"pages": 4}
        }))
        .unwrap(),
    )
    .await
    .expect("create immutable Payment Link preset");
    let link = stripe::create_payment_link(
        &ctx,
        &product,
        &offer_id,
        &PaymentLinkCreateRequest {
            preset_id: Some(preset.id),
            after_completion_url: None,
        },
    )
    .await
    .expect("create Payment Link using the synchronized Price");
    assert_eq!(link.url, "https://buy.stripe.com/catalog");

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].url, "https://api.stripe.com/v1/products");
    assert_eq!(requests[1].url, "https://api.stripe.com/v1/prices");
    assert_eq!(
        requests[0].headers["Idempotency-Key"],
        "impresspress_product_product_catalog_sync"
    );
    assert_eq!(requests[0].headers["Stripe-Version"], "2026-02-25.clover");
    let product_form = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert!(product_form.contains("name=Configurable%20print"));
    assert!(product_form.contains("metadata[impresspress_product_id]=product_catalog_sync"));
    let price_form = String::from_utf8(requests[1].body.clone().unwrap()).unwrap();
    assert!(price_form.contains("product=prod_catalog"));
    assert!(price_form.contains("currency=nzd"));
    assert!(price_form.contains("unit_amount=1000"));
    assert!(price_form.contains("metadata[impresspress_component_key]=setup"));

    let checkout_form = String::from_utf8(requests[2].body.clone().unwrap()).unwrap();
    assert!(checkout_form.contains("line_items[1][price]=price_catalog_setup"));
    assert!(!checkout_form.contains("line_items[1][price_data]"));
    assert!(checkout_form.contains("line_items[1][quantity]=2"));
    assert!(checkout_form.contains("line_items[0][price_data][unit_amount]=75"));
    let link_form = String::from_utf8(requests[3].body.clone().unwrap()).unwrap();
    assert!(link_form.contains("line_items[1][price]=price_catalog_setup"));
    assert!(!link_form.contains("line_items[1][price_data]"));
    assert!(link_form.contains("line_items[0][price_data][unit_amount]=100"));
}

/// [B21] A Stripe outage during catalog sync is retryable, not a rejection.
///
/// `stripe_catalog_post` classified every status of 400 or more as
/// `FailedPrecondition` — terminal — while `StripeClient::request_json`, one
/// file over, has always treated 429 and 5xx as `Internal`. So a Stripe 503
/// reached the operator as `409 Stripe rejected catalog synchronization` and
/// was written into the offer's `sync_error` in those words: a transient
/// outage recorded as a permanent refusal, on the one row an operator would
/// read to decide whether to retry.
#[tokio::test]
async fn catalog_sync_classifies_a_provider_outage_as_retryable() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_outage",
    )])
    .await;
    let requests = register_stripe_sequence(
        &mut ctx,
        vec![(503, serde_json::json!({"error": {"type": "api_error"}}))],
    );
    let product_id = "product_catalog_outage";
    let offer_id = seed_active_offer(&ctx, product_id, "").await;

    let error = stripe::sync_offer_catalog(&ctx, product_id, &offer_id)
        .await
        .expect_err("a 503 must fail the sync");
    assert_eq!(
        error.code,
        ErrorCode::Internal,
        "a 503 is retryable, not a terminal rejection: {}",
        error.message
    );
    assert!(error.message.contains("503"), "{}", error.message);
    let failed = repo::offers::get_managed(&ctx, &offer_id).await.unwrap();
    assert!(
        !failed.sync_error.to_ascii_lowercase().contains("reject"),
        "the persisted sync error must not call an outage a rejection: {}",
        failed.sync_error
    );
    assert!(failed.sync_error.contains("503"), "{}", failed.sync_error);
    assert_eq!(requests.lock().unwrap().len(), 1);
}

/// The other half of the same classification: a deterministic 400 stays
/// terminal, because retrying it changes nothing.
#[tokio::test]
async fn catalog_sync_classifies_a_provider_rejection_as_terminal() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_rejected",
    )])
    .await;
    register_stripe_sequence(
        &mut ctx,
        vec![(
            400,
            serde_json::json!({"error": {"code": "parameter_invalid_empty"}}),
        )],
    );
    let product_id = "product_catalog_rejected";
    let offer_id = seed_active_offer(&ctx, product_id, "").await;

    let error = stripe::sync_offer_catalog(&ctx, product_id, &offer_id)
        .await
        .expect_err("a 400 must fail the sync");
    assert_eq!(
        error.code,
        ErrorCode::FailedPrecondition,
        "{}",
        error.message
    );
    assert!(
        error.message.contains("parameter_invalid_empty"),
        "the provider's own error code must survive: {}",
        error.message
    );
}

/// [B21] The Payment-Link deactivate classified in the opposite direction:
/// **every** failure was `Internal`, so a deterministic 400 was reported as
/// retryable and the caller was invited to try it again forever.
#[tokio::test]
async fn payment_link_deactivation_classifies_a_provider_rejection_as_terminal() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
    ])
    .await;
    let product_id = "product_link_deactivate";
    let offer_id = seed_active_offer(&ctx, product_id, "").await;
    let offer = repo::offers::get_managed(&ctx, &offer_id).await.unwrap();
    let preview = offer_pricing::evaluate_offer(
        &offer.offer,
        &PricingPreviewRequest {
            offer_id: offer_id.clone(),
            quantity: 1,
            inputs: serde_json::from_value(serde_json::json!({"pages": 2})).unwrap(),
        },
        offer_pricing::InputScope::Management,
    )
    .unwrap();
    let pending = seed_pending_payment_link(&ctx, &offer_id, "deactivate-config", &preview).await;
    let link_id = pending.managed.id;
    repo::payment_links::mark_synced(
        &ctx,
        &link_id,
        "plink_deactivate",
        "https://buy.stripe.com/deactivate",
    )
    .await
    .unwrap();
    register_stripe_sequence(
        &mut ctx,
        vec![(
            400,
            serde_json::json!({"error": {"code": "resource_missing"}}),
        )],
    );

    let error = stripe::deactivate_payment_link(&ctx, &offer_id, &link_id)
        .await
        .expect_err("Stripe rejected the deactivation");
    assert_eq!(
        error.code,
        ErrorCode::FailedPrecondition,
        "a 400 is a rejection, not an outage: {}",
        error.message
    );
    assert!(
        repo::payment_links::list_for_offer(&ctx, &offer_id)
            .await
            .unwrap()[0]
            .active,
        "a rejected deactivation must not deactivate the local row"
    );
}

#[tokio::test]
async fn catalog_sync_failure_is_visible_and_retry_reuses_the_persisted_product() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_catalog_retry",
    )])
    .await;
    let first_requests = register_stripe_sequence(
        &mut ctx,
        vec![
            (
                200,
                serde_json::json!({
                    "id": "prod_catalog_retry",
                    "livemode": false,
                    "active": true
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "price_wrong_amount",
                    "livemode": false,
                    "product": "prod_catalog_retry",
                    "currency": "nzd",
                    "unit_amount": 999,
                    "active": true
                }),
            ),
        ],
    );
    let product_id = "product_catalog_retry";
    let offer_id = seed_active_offer(&ctx, product_id, "").await;

    let error = stripe::sync_offer_catalog(&ctx, product_id, &offer_id)
        .await
        .expect_err("a mismatched Stripe Price must fail closed");
    assert_eq!(error.code, ErrorCode::Internal);
    assert!(error.message.contains("immutable offer row"));
    let failed = repo::offers::get_managed(&ctx, &offer_id).await.unwrap();
    assert_eq!(failed.sync_status, OfferSyncStatus::Failed);
    assert!(failed.sync_error.contains("immutable offer row"));
    assert!(failed
        .offer
        .components
        .iter()
        .all(|component| component.stripe_price_id.is_empty()));
    let persisted_product = db::get(&ctx, repo::products::TABLE, product_id)
        .await
        .unwrap();
    assert_eq!(
        persisted_product.str_field("stripe_product_id"),
        "prod_catalog_retry"
    );
    assert_eq!(first_requests.lock().unwrap().len(), 2);

    let retry_requests = register_stripe_sequence(
        &mut ctx,
        vec![
            (
                200,
                serde_json::json!({
                    "id": "prod_catalog_retry",
                    "livemode": false,
                    "active": true
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "prod_catalog_retry",
                    "livemode": false,
                    "active": true
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "price_catalog_retry",
                    "livemode": false,
                    "product": "prod_catalog_retry",
                    "currency": "nzd",
                    "unit_amount": 1000,
                    "active": true
                }),
            ),
        ],
    );
    let retried = stripe::sync_offer_catalog(&ctx, product_id, &offer_id)
        .await
        .expect("retry catalog sync");
    assert_eq!(retried.sync_status, OfferSyncStatus::Synced);
    assert!(retried.sync_error.is_empty());
    assert_eq!(
        retried
            .offer
            .components
            .iter()
            .find(|component| component.key == "setup")
            .unwrap()
            .stripe_price_id,
        "price_catalog_retry"
    );
    let retry_requests = retry_requests.lock().unwrap();
    assert_eq!(
        retry_requests.len(),
        3,
        "the Product must be reconciled, not recreated"
    );
    assert_eq!(
        retry_requests[0].url,
        "https://api.stripe.com/v1/products/prod_catalog_retry"
    );
    assert!(retry_requests[0].body.is_none());
    assert_eq!(
        retry_requests[1].url,
        "https://api.stripe.com/v1/products/prod_catalog_retry"
    );
    assert_eq!(retry_requests[2].url, "https://api.stripe.com/v1/prices");
}

#[tokio::test]
async fn catalog_reconciliation_refreshes_product_metadata_and_reactivates_fixed_prices() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_catalog_reconcile",
    )])
    .await;
    let product_id = "product_catalog_reconcile";
    let offer_id = seed_active_offer(&ctx, product_id, "").await;
    let offer = repo::offers::get_managed(&ctx, &offer_id).await.unwrap();
    let fixed = offer
        .offer
        .components
        .iter()
        .find(|component| component.key == "setup")
        .unwrap();
    db::update(
        &ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([(
            "stripe_product_id".to_string(),
            serde_json::json!("prod_reconcile"),
        )]),
    )
    .await
    .unwrap();
    repo::offer_components::set_stripe_price_id(&ctx, &fixed.id, "price_reconcile")
        .await
        .unwrap();
    repo::offers::mark_synced(&ctx, &offer_id, "prod_reconcile", "")
        .await
        .unwrap();

    let requests = register_stripe_sequence(
        &mut ctx,
        vec![
            (
                200,
                serde_json::json!({
                    "id": "prod_reconcile",
                    "livemode": false,
                    "active": false
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "prod_reconcile",
                    "livemode": false,
                    "active": true
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "price_reconcile",
                    "livemode": false,
                    "active": false,
                    "product": "prod_reconcile",
                    "currency": "nzd",
                    "unit_amount": 1000
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "price_reconcile",
                    "livemode": false,
                    "active": true,
                    "product": "prod_reconcile",
                    "currency": "nzd",
                    "unit_amount": 1000
                }),
            ),
        ],
    );
    let reconciled = stripe::sync_offer_catalog(&ctx, product_id, &offer_id)
        .await
        .expect("repair inactive catalog objects");
    assert_eq!(reconciled.sync_status, OfferSyncStatus::Synced);

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[0].url,
        "https://api.stripe.com/v1/products/prod_reconcile"
    );
    assert!(requests[0].body.is_none());
    let product_update = String::from_utf8(requests[1].body.clone().unwrap()).unwrap();
    assert!(product_update.contains("active=true"));
    assert!(product_update.contains("name=Configurable%20print"));
    assert_eq!(
        requests[2].url,
        "https://api.stripe.com/v1/prices/price_reconcile"
    );
    assert!(requests[2].body.is_none());
    assert_eq!(requests[3].body.as_deref(), Some(b"active=true".as_slice()));
}

#[tokio::test]
async fn catalog_reconciliation_replaces_a_missing_product_and_its_dependent_price() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_catalog_repair",
    )])
    .await;
    let product_id = "product_catalog_repair";
    let offer_id = seed_active_offer(&ctx, product_id, "").await;
    let offer = repo::offers::get_managed(&ctx, &offer_id).await.unwrap();
    let fixed = offer
        .offer
        .components
        .iter()
        .find(|component| component.key == "setup")
        .unwrap();
    db::update(
        &ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([(
            "stripe_product_id".to_string(),
            serde_json::json!("prod_missing"),
        )]),
    )
    .await
    .unwrap();
    repo::offer_components::set_stripe_price_id(&ctx, &fixed.id, "price_orphaned")
        .await
        .unwrap();
    repo::offers::mark_synced(&ctx, &offer_id, "prod_missing", "")
        .await
        .unwrap();

    let requests = register_stripe_sequence(
        &mut ctx,
        vec![
            (
                404,
                serde_json::json!({"error": {"code": "resource_missing"}}),
            ),
            (
                200,
                serde_json::json!({
                    "id": "prod_repaired",
                    "livemode": false,
                    "active": true
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "price_repaired",
                    "livemode": false,
                    "active": true,
                    "product": "prod_repaired",
                    "currency": "nzd",
                    "unit_amount": 1000
                }),
            ),
        ],
    );
    let repaired = stripe::sync_offer_catalog(&ctx, product_id, &offer_id)
        .await
        .expect("repair missing Product and dependent Price");
    assert_eq!(repaired.offer.stripe_product_id, "prod_repaired");
    assert_eq!(
        repaired
            .offer
            .components
            .iter()
            .find(|component| component.key == "setup")
            .unwrap()
            .stripe_price_id,
        "price_repaired"
    );

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[0].url,
        "https://api.stripe.com/v1/products/prod_missing"
    );
    assert_eq!(requests[1].url, "https://api.stripe.com/v1/products");
    assert_eq!(requests[2].url, "https://api.stripe.com/v1/prices");
    assert!(requests
        .iter()
        .all(|request| !request.url.contains("price_orphaned")));
    assert!(requests[1].headers["Idempotency-Key"].contains("repair"));
    assert!(requests[2].headers["Idempotency-Key"].contains("repair"));
}

#[tokio::test]
async fn seller_catalog_sync_creates_resources_in_the_owned_connected_account() {
    let mut ctx = ctx_with(&[
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_seller_catalog",
        ),
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
    ])
    .await;
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_catalog_account",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("seller_catalog")),
            ("status".to_string(), serde_json::json!("active")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_seller_catalog"),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(true)),
            ("payouts_enabled".to_string(), serde_json::json!(true)),
        ]),
    )
    .await;
    let requests = register_stripe_sequence(
        &mut ctx,
        vec![
            (
                200,
                serde_json::json!({
                    "id": "prod_seller_catalog",
                    "livemode": false,
                    "active": true
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "price_seller_catalog",
                    "livemode": false,
                    "active": true,
                    "product": "prod_seller_catalog",
                    "currency": "nzd",
                    "unit_amount": 1000
                }),
            ),
        ],
    );
    let product_id = "seller_catalog_product";
    let offer_id = seed_active_offer(&ctx, product_id, "seller_catalog").await;

    let synced = stripe::sync_offer_catalog(&ctx, product_id, &offer_id)
        .await
        .expect("sync seller catalog");
    assert_eq!(synced.sync_status, OfferSyncStatus::Synced);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|request| request.headers["Stripe-Account"] == "acct_seller_catalog"));
}

#[tokio::test]
async fn subscription_catalog_sync_creates_and_persists_a_strict_recurring_price() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_subscription_catalog",
    )])
    .await;
    let product_id = "subscription_catalog_product";
    seed(
        &ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([
            ("name".to_string(), serde_json::json!("Quarterly care plan")),
            ("slug".to_string(), serde_json::json!(product_id)),
            ("status".to_string(), serde_json::json!("active")),
            ("approval_status".to_string(), serde_json::json!("approved")),
            ("owner_kind".to_string(), serde_json::json!("platform")),
        ]),
    )
    .await;
    let definition: OfferDefinitionRequest = serde_json::from_value(serde_json::json!({
        "name": "Quarterly subscription",
        "mode": "subscription",
        "currency": "nzd",
        "pricing_model": "fixed",
        "recurring_interval": "month",
        "interval_count": 3,
        "usage_type": "licensed",
        "billing_scheme": "per_unit",
        "tax_behavior": "exclusive",
        "components": [{
            "key": "plan",
            "label": "Care plan",
            "required": true,
            "amount": {"type": "fixed", "unit_amount_minor": 4900}
        }]
    }))
    .unwrap();
    let offer = repo::offers::create(&ctx, product_id, "admin_1", &definition)
        .await
        .unwrap();
    let offer_id = offer.offer.id;
    repo::offers::publish(&ctx, product_id, &offer_id)
        .await
        .unwrap();
    let requests = register_stripe_sequence(
        &mut ctx,
        vec![
            (
                200,
                serde_json::json!({
                    "id": "prod_subscription_catalog",
                    "livemode": false,
                    "active": true
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "price_subscription_catalog",
                    "livemode": false,
                    "active": true,
                    "product": "prod_subscription_catalog",
                    "currency": "nzd",
                    "unit_amount": 4900,
                    "recurring": {
                        "interval": "month",
                        "interval_count": 3,
                        "usage_type": "licensed"
                    }
                }),
            ),
        ],
    );

    let synced = stripe::sync_offer_catalog(&ctx, product_id, &offer_id)
        .await
        .expect("sync recurring Price");
    assert_eq!(synced.offer.stripe_price_id, "price_subscription_catalog");
    assert_eq!(
        synced.offer.components[0].stripe_price_id,
        "price_subscription_catalog"
    );
    let requests = requests.lock().unwrap();
    let price_form = String::from_utf8(requests[1].body.clone().unwrap()).unwrap();
    assert!(price_form.contains("recurring[interval]=month"));
    assert!(price_form.contains("recurring[interval_count]=3"));
    assert!(price_form.contains("recurring[usage_type]=licensed"));
}

#[tokio::test]
async fn synced_offer_archive_is_provider_first_retryable_and_idempotent() {
    let mut ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
        "sk_test_catalog_archive",
    )])
    .await;
    let product_id = "product_catalog_archive";
    let offer_id = seed_active_offer(&ctx, product_id, "").await;
    let offer = repo::offers::get_managed(&ctx, &offer_id).await.unwrap();
    let fixed = offer
        .offer
        .components
        .iter()
        .find(|component| component.key == "setup")
        .unwrap();
    db::update(
        &ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([(
            "stripe_product_id".to_string(),
            serde_json::json!("prod_archive"),
        )]),
    )
    .await
    .unwrap();
    repo::offer_components::set_stripe_price_id(&ctx, &fixed.id, "price_archive")
        .await
        .unwrap();
    repo::offers::mark_synced(&ctx, &offer_id, "prod_archive", "")
        .await
        .unwrap();
    let preview = offer_pricing::evaluate_offer(
        &offer.offer,
        &PricingPreviewRequest {
            offer_id: offer_id.clone(),
            quantity: 1,
            inputs: serde_json::from_value(serde_json::json!({"pages": 2})).unwrap(),
        },
        offer_pricing::InputScope::Management,
    )
    .unwrap();
    let pending_link =
        seed_pending_payment_link(&ctx, &offer_id, "archive-link-config", &preview).await;
    let link_id = pending_link.managed.id;
    repo::payment_links::mark_synced(
        &ctx,
        &link_id,
        "plink_archive",
        "https://buy.stripe.com/archive",
    )
    .await
    .unwrap();
    let active_price = serde_json::json!({
        "id": "price_archive",
        "livemode": false,
        "active": true,
        "product": "prod_archive",
        "currency": "nzd",
        "unit_amount": 1000
    });
    let failed_requests = register_stripe_sequence(
        &mut ctx,
        vec![
            (
                200,
                serde_json::json!({"id": "plink_archive", "active": false}),
            ),
            (200, active_price.clone()),
            (
                400,
                serde_json::json!({"error": {"code": "catalog_archive_failed"}}),
            ),
        ],
    );
    let path = format!("/b/products/api/admin/products/{product_id}/offers/{offer_id}");
    let (msg, input) = delete_msg(&path, "admin_1");
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::AlreadyExists).await);
    assert_eq!(
        repo::offers::get_managed(&ctx, &offer_id)
            .await
            .unwrap()
            .status,
        crate::blocks::products::contracts::OfferStatus::Active,
        "local visibility must remain active when Stripe rejects archival"
    );
    {
        let failed_requests = failed_requests.lock().unwrap();
        assert_eq!(failed_requests.len(), 3);
        assert_eq!(
            failed_requests[0].url,
            "https://api.stripe.com/v1/payment_links/plink_archive"
        );
        assert_eq!(
            failed_requests[0].body.as_deref(),
            Some(b"active=false".as_slice())
        );
        assert_eq!(
            failed_requests[2].body.as_deref(),
            Some(b"active=false".as_slice())
        );
        assert!(failed_requests[2].headers["Idempotency-Key"].contains("archive"));
    }
    assert!(
        !repo::payment_links::list_for_offer(&ctx, &offer_id)
            .await
            .unwrap()[0]
            .active
    );

    let retry_requests = register_stripe_sequence(
        &mut ctx,
        vec![
            (200, active_price),
            (
                200,
                serde_json::json!({
                    "id": "price_archive",
                    "livemode": false,
                    "active": false,
                    "product": "prod_archive",
                    "currency": "nzd",
                    "unit_amount": 1000
                }),
            ),
        ],
    );
    let (msg, input) = delete_msg(&path, "admin_1");
    let archived = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(archived["status"], "archived");
    assert_eq!(retry_requests.lock().unwrap().len(), 2);

    let idempotent_requests = register_stripe_sequence(&mut ctx, vec![]);
    let (msg, input) = delete_msg(&path, "admin_1");
    let archived_again = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(archived_again["status"], "archived");
    assert!(idempotent_requests.lock().unwrap().is_empty());
}

/// Suspending a seller is a lifecycle/fraud operation, so it has to reach
/// every row the seller owns. A soft-deleted product is still one of theirs,
/// and soft delete touches nothing in Stripe: its Prices and Payment Links
/// stay live in the connected account until something archives them.
///
/// `seller_products` reads through the live-only door, which silently exempted
/// exactly those rows from the guarantee
/// `seller_suspension_fails_closed_until_connected_catalog_archival_succeeds`
/// documents — a suspended fraudster's deleted listings kept taking money.
#[tokio::test]
async fn seller_suspension_archives_the_catalog_of_soft_deleted_products_too() {
    let mut ctx = ctx_with(&[
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_suspend_deleted",
        ),
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
    ])
    .await;
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "deleted_suspend_account",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("deleted_suspend")),
            ("status".to_string(), serde_json::json!("active")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_deleted_suspend"),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(true)),
            ("payouts_enabled".to_string(), serde_json::json!(true)),
            ("fee_basis_points".to_string(), serde_json::json!(200)),
        ]),
    )
    .await;
    let product_id = "deleted_suspend_product";
    let offer_id = seed_active_offer(&ctx, product_id, "deleted_suspend").await;
    let fixed = repo::offers::get_managed(&ctx, &offer_id)
        .await
        .unwrap()
        .offer
        .components
        .into_iter()
        .find(|component| component.key == "setup")
        .unwrap();
    db::update(
        &ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([(
            "stripe_product_id".to_string(),
            serde_json::json!("prod_deleted_suspend"),
        )]),
    )
    .await
    .unwrap();
    repo::offer_components::set_stripe_price_id(&ctx, &fixed.id, "price_deleted_suspend")
        .await
        .unwrap();
    repo::offers::mark_synced(&ctx, &offer_id, "prod_deleted_suspend", "")
        .await
        .unwrap();

    // The seller deletes the listing. Nothing in Stripe changes.
    repo::products::soft_delete(&ctx, product_id)
        .await
        .expect("soft delete");

    let requests = register_stripe_sequence(
        &mut ctx,
        vec![
            (
                200,
                serde_json::json!({
                    "id": "price_deleted_suspend",
                    "livemode": false,
                    "active": true,
                    "product": "prod_deleted_suspend",
                    "currency": "nzd",
                    "unit_amount": 1000
                }),
            ),
            (
                200,
                serde_json::json!({
                    "id": "price_deleted_suspend",
                    "livemode": false,
                    "active": false,
                    "product": "prod_deleted_suspend",
                    "currency": "nzd",
                    "unit_amount": 1000
                }),
            ),
        ],
    );
    let path = "/b/products/api/admin/sellers/deleted_suspend_account/suspend";
    let (msg, input) = admin_create_msg(path, serde_json::json!({}));
    let suspended = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(suspended["status"], "suspended");

    {
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            2,
            "the soft-deleted product's Stripe Price must still be fetched and archived"
        );
        assert!(requests
            .iter()
            .all(|request| request.headers["Stripe-Account"] == "acct_deleted_suspend"));
        assert_eq!(
            requests[1].body.as_deref(),
            Some(b"active=false".as_slice())
        );
    }
    assert_eq!(
        repo::offers::get_managed(&ctx, &offer_id)
            .await
            .unwrap()
            .status,
        crate::blocks::products::contracts::OfferStatus::Archived,
        "a suspended seller's soft-deleted offer must end up archived"
    );
    // The row stays soft-deleted: suspension archives the catalog, it does
    // not resurrect a deleted listing.
    assert!(repo::products::get(&ctx, product_id).await.is_err());
}

#[tokio::test]
async fn seller_suspension_fails_closed_until_connected_catalog_archival_succeeds() {
    let mut ctx = ctx_with(&[
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_seller_suspend",
        ),
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
    ])
    .await;
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_suspend_account",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("seller_suspend")),
            ("status".to_string(), serde_json::json!("active")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_seller_suspend"),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(true)),
            ("payouts_enabled".to_string(), serde_json::json!(true)),
            ("fee_basis_points".to_string(), serde_json::json!(200)),
        ]),
    )
    .await;
    let product_id = "seller_suspend_product";
    let offer_id = seed_active_offer(&ctx, product_id, "seller_suspend").await;
    let fixed = repo::offers::get_managed(&ctx, &offer_id)
        .await
        .unwrap()
        .offer
        .components
        .into_iter()
        .find(|component| component.key == "setup")
        .unwrap();
    db::update(
        &ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([(
            "stripe_product_id".to_string(),
            serde_json::json!("prod_seller_suspend"),
        )]),
    )
    .await
    .unwrap();
    repo::offer_components::set_stripe_price_id(&ctx, &fixed.id, "price_seller_suspend")
        .await
        .unwrap();
    repo::offers::mark_synced(&ctx, &offer_id, "prod_seller_suspend", "")
        .await
        .unwrap();
    let active_price = serde_json::json!({
        "id": "price_seller_suspend",
        "livemode": false,
        "active": true,
        "product": "prod_seller_suspend",
        "currency": "nzd",
        "unit_amount": 1000
    });
    let failed_requests = register_stripe_sequence(
        &mut ctx,
        vec![
            (200, active_price.clone()),
            (
                400,
                serde_json::json!({"error": {"code": "catalog_archive_failed"}}),
            ),
        ],
    );
    let path = "/b/products/api/admin/sellers/seller_suspend_account/suspend";
    let (msg, input) = admin_create_msg(path, serde_json::json!({}));
    assert!(output_is_error(dispatch(&ctx, msg, input).await, ErrorCode::AlreadyExists,).await);
    assert_eq!(
        db::get(&ctx, repo::seller_accounts::TABLE, "seller_suspend_account")
            .await
            .unwrap()
            .str_field("status"),
        "active"
    );
    assert_eq!(
        db::get(&ctx, repo::products::TABLE, product_id)
            .await
            .unwrap()
            .str_field("status"),
        "active"
    );
    assert_eq!(
        repo::offers::get_managed(&ctx, &offer_id)
            .await
            .unwrap()
            .status,
        crate::blocks::products::contracts::OfferStatus::Active
    );
    {
        let failed_requests = failed_requests.lock().unwrap();
        assert_eq!(failed_requests.len(), 2);
        assert!(failed_requests
            .iter()
            .all(|request| request.headers["Stripe-Account"] == "acct_seller_suspend"));
    }

    let retry_requests = register_stripe_sequence(
        &mut ctx,
        vec![
            (200, active_price),
            (
                200,
                serde_json::json!({
                    "id": "price_seller_suspend",
                    "livemode": false,
                    "active": false,
                    "product": "prod_seller_suspend",
                    "currency": "nzd",
                    "unit_amount": 1000
                }),
            ),
        ],
    );
    let (msg, input) = admin_create_msg(path, serde_json::json!({}));
    let suspended = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(suspended["status"], "suspended");
    assert_eq!(
        db::get(&ctx, repo::products::TABLE, product_id)
            .await
            .unwrap()
            .str_field("status"),
        "archived"
    );
    assert_eq!(retry_requests.lock().unwrap().len(), 2);
}

/// The two duplicated columns on the orders table are written together, by
/// one writer, from one value each — and nothing in the tree compares them.
///
/// `amount_cents` mirrors `total_cents` and `user_id` mirrors
/// `buyer_user_id`. Neither duplicate is published any more (`PurchaseView`
/// carries `total_cents` and `buyer_user_id` only, and this is the PR that
/// stopped it publishing both), and neither column can be dropped without a
/// migration this phase deliberately defers. What is left is the risk that a
/// future writer sets one of a pair and forgets the other, which nothing
/// would notice: the internal read sets differ, so an order would list under
/// one identity and access-check under another. This pins the invariant on
/// the writer that creates the row.
#[tokio::test]
async fn a_created_order_writes_the_same_value_into_both_duplicated_columns() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
    ])
    .await;
    register_stripe_network(
        &mut ctx,
        serde_json::json!({
            "id": "cs_test_mirror",
            "url": "https://checkout.stripe.com/c/pay/cs_test_mirror"
        }),
    );
    let offer_id = seed_active_offer(&ctx, "product_mirror_checkout", "").await;

    // A SIGNED-IN buyer, so both identity columns are non-empty and the
    // assertion has something to compare. A guest order writes `""` into
    // both, which would pass whatever the writer did.
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "user_mirror",
        serde_json::json!({
            "offer_id": offer_id,
            "inputs": {"pages": 2},
            "presentation": "hosted"
        }),
    );
    let body = output_to_json(stripe::handle_checkout(&ctx, &msg, input).await).await;
    let order_id = body["order_id"].as_str().expect("order id");

    let order = db::get(&ctx, "impresspress__products__purchases", order_id)
        .await
        .expect("order row");
    assert_eq!(
        order.data["buyer_user_id"],
        serde_json::json!("user_mirror"),
        "the fixture must produce a signed-in order, or the mirror assertions \
         below compare two empty strings"
    );
    assert_eq!(
        order.data["user_id"], order.data["buyer_user_id"],
        "`user_id` mirrors `buyer_user_id`; a writer that sets one and not the \
         other makes an order list under one identity and access-check under \
         another"
    );
    assert!(
        order.data["total_cents"].as_i64().is_some_and(|v| v > 0),
        "the fixture must charge something, or the amount assertion below \
         compares two absent fields: {:?}",
        order.data["total_cents"]
    );
    assert_eq!(
        order.data["amount_cents"], order.data["total_cents"],
        "`amount_cents` mirrors `total_cents`; a writer that sets one and not \
         the other stores two answers for what the buyer was charged"
    );
}

#[tokio::test]
async fn embedded_offer_checkout_returns_client_secret_and_uses_return_url() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
    ])
    .await;
    let requests = register_stripe_network(
        &mut ctx,
        serde_json::json!({
            "id": "cs_test_embedded",
            "client_secret": "cs_test_embedded_secret_123"
        }),
    );
    let offer_id = seed_active_offer(&ctx, "product_embedded_checkout", "").await;
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({
            "offer_id": offer_id,
            "inputs": {"pages": 2},
            "presentation": "embedded",
            "success_url": "https://shop.example/embedded/return?session_id={CHECKOUT_SESSION_ID}"
        }),
    );
    let body = output_to_json(stripe::handle_checkout(&ctx, &msg, input).await).await;
    assert_eq!(body["presentation"], "embedded");
    assert_eq!(body["client_secret"], "cs_test_embedded_secret_123");
    assert!(body["checkout_url"].is_null());
    let requests = requests.lock().unwrap();
    let form = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert!(form.contains("ui_mode=embedded"));
    assert!(form.contains("return_url=https%3A%2F%2Fshop.example%2Fembedded%2Freturn"));
    assert!(!form.contains("success_url="));
    assert!(!form.contains("cancel_url="));
}

#[tokio::test]
async fn seller_offer_checkout_uses_direct_charge_header_and_application_fee() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        ("IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS", "250"),
    ])
    .await;
    let requests = register_stripe_network(
        &mut ctx,
        serde_json::json!({
            "id": "cs_test_seller",
            "url": "https://checkout.stripe.com/c/pay/cs_test_seller"
        }),
    );
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_account_1",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("seller_1")),
            ("status".to_string(), serde_json::json!("active")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_connected_1"),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(true)),
            ("payouts_enabled".to_string(), serde_json::json!(true)),
        ]),
    )
    .await;
    let offer_id = seed_active_offer(&ctx, "seller_product_checkout", "seller_1").await;

    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({
            "offer_id": offer_id,
            "inputs": {"pages": 4}
        }),
    );
    let body = output_to_json(stripe::handle_checkout(&ctx, &msg, input).await).await;
    assert_eq!(body["amounts"]["total_minor"], 1100);
    assert_eq!(body["amounts"]["platform_fee_minor"], 27);
    let order_id = body["order_id"].as_str().unwrap();
    let order = db::get(&ctx, "impresspress__products__purchases", order_id)
        .await
        .unwrap();
    assert_eq!(order.data["seller_account_id"], "seller_account_1");
    assert_eq!(order.data["stripe_account_id"], "acct_connected_1");
    assert_eq!(order.data["platform_fee_cents"], 27);

    let requests = requests.lock().unwrap();
    assert_eq!(requests[0].headers["Stripe-Account"], "acct_connected_1");
    let form = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert!(form.contains("payment_intent_data[application_fee_amount]=27"));
}

/// [B22] A garbage application fee refuses the checkout instead of quietly
/// taking no platform fee.
///
/// `SELLER_APPLICATION_FEE_BPS` was parsed with `.ok().filter(..)
/// .unwrap_or(0)` on both money paths, so any value the `u16` parse rejected
/// — a stray `%`, a percentage rather than basis points, a blanked field —
/// charged the buyer in full and paid the platform nothing, with no error
/// anywhere. Seller onboarding refused the identical value.
#[tokio::test]
async fn seller_offer_checkout_refuses_a_misconfigured_application_fee() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        ("IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS", "2.5%"),
    ])
    .await;
    let requests = register_stripe_network(
        &mut ctx,
        serde_json::json!({"id": "must_not_be_used", "url": "https://example.invalid"}),
    );
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_account_bad_fee",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("seller_bad_fee")),
            ("status".to_string(), serde_json::json!("active")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_bad_fee"),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(true)),
            ("payouts_enabled".to_string(), serde_json::json!(true)),
        ]),
    )
    .await;
    let offer_id = seed_active_offer(&ctx, "seller_product_bad_fee", "seller_bad_fee").await;
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({"offer_id": offer_id, "inputs": {"pages": 4}}),
    );
    assert!(
        output_is_error(
            stripe::handle_checkout(&ctx, &msg, input).await,
            ErrorCode::Internal
        )
        .await,
        "a fee the platform cannot parse must refuse the sale, not take zero"
    );
    assert!(
        requests.lock().unwrap().is_empty(),
        "no Checkout Session may be created with a fee nobody could read"
    );
}

/// [B23] With no platform country and no allowed shipping countries, a
/// checkout that collects a shipping address is refused rather than shipped
/// to the United States.
///
/// `stripe.rs` defaulted `PLATFORM_COUNTRY` to `"US"` and also fell back to
/// `"US"` on an unreadable value, while the `ConfigVar` and seller
/// onboarding default it to empty — so an NZ merchant who left it unset got
/// a US-only Checkout and no error. Omitting the key instead is not an
/// option: `allowed_countries` is a required member of Stripe's
/// `shipping_address_collection`, so omitting it collects no address at all.
#[tokio::test]
async fn shipping_checkout_refuses_when_no_country_is_configured() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
    ])
    .await;
    let requests = register_stripe_network(
        &mut ctx,
        serde_json::json!({"id": "must_not_be_used", "url": "https://example.invalid"}),
    );
    let offer_id = seed_shipping_offer(&ctx, "product_shipping_no_country", &[]).await;
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({"offer_id": offer_id, "inputs": {}}),
    );
    let out = stripe::handle_checkout(&ctx, &msg, input).await;
    assert!(
        output_is_error(out, ErrorCode::InvalidArgument).await,
        "an offer collecting a shipping address with no country list and no platform country must refuse"
    );
    assert!(
        requests.lock().unwrap().is_empty(),
        "no Checkout Session may be created with a fabricated country list"
    );
}

/// An offer that names its own shipping countries never needed the platform
/// country and still does not: the refusal above is only for the offer that
/// names none.
#[tokio::test]
async fn shipping_checkout_prefers_the_offers_own_country_list() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
    ])
    .await;
    let requests = register_stripe_network(
        &mut ctx,
        serde_json::json!({
            "id": "cs_test_offer_countries",
            "url": "https://checkout.stripe.com/c/pay/cs_test_offer_countries"
        }),
    );
    let offer_id = seed_shipping_offer(&ctx, "product_shipping_offer_list", &["au", "NZ"]).await;
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({"offer_id": offer_id, "inputs": {}}),
    );
    let body = output_to_json(stripe::handle_checkout(&ctx, &msg, input).await).await;
    assert!(body["checkout_url"].is_string());
    let requests = requests.lock().unwrap();
    let form = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert!(
        form.contains("shipping_address_collection[allowed_countries][0]=AU")
            && form.contains("shipping_address_collection[allowed_countries][1]=NZ"),
        "{form}"
    );
}

/// The same offer ships once the platform country is set — and it ships to
/// that country, not to the deleted `"US"` default.
#[tokio::test]
async fn shipping_checkout_uses_the_configured_platform_country() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        ("IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY", "nz"),
    ])
    .await;
    let requests = register_stripe_network(
        &mut ctx,
        serde_json::json!({
            "id": "cs_test_shipping",
            "url": "https://checkout.stripe.com/c/pay/cs_test_shipping"
        }),
    );
    let offer_id = seed_shipping_offer(&ctx, "product_shipping_nz", &[]).await;
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({"offer_id": offer_id, "inputs": {}}),
    );
    let body = output_to_json(stripe::handle_checkout(&ctx, &msg, input).await).await;
    assert!(body["checkout_url"].is_string());
    let requests = requests.lock().unwrap();
    let form = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert!(
        form.contains("shipping_address_collection[allowed_countries][0]=NZ"),
        "{form}"
    );
}

#[tokio::test]
async fn seller_offer_checkout_fails_closed_when_connect_charges_are_disabled() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
    ])
    .await;
    let requests = register_stripe_network(
        &mut ctx,
        serde_json::json!({"id": "must_not_be_used", "url": "https://example.invalid"}),
    );
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_account_disabled",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("seller_disabled")),
            ("status".to_string(), serde_json::json!("active")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_disabled"),
            ),
            ("charges_enabled".to_string(), serde_json::json!(false)),
        ]),
    )
    .await;
    let offer_id = seed_active_offer(&ctx, "seller_product_disabled", "seller_disabled").await;
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({"offer_id": offer_id, "inputs": {"pages": 2}}),
    );
    assert!(
        output_is_error(
            stripe::handle_checkout(&ctx, &msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
    assert!(requests.lock().unwrap().is_empty());
    assert_eq!(
        db::count(&ctx, "impresspress__products__purchases", &[])
            .await
            .unwrap(),
        0
    );
}

/// A checkout preset's slug is unique per offer (migration 005's
/// `checkout_presets_slug_uniq`): creating a second preset under a slug the
/// offer already has, or renaming one onto it, is a 409 that says which slug —
/// not the generic "same key" that leaves the admin to guess, nor anything of
/// the index or the table.
#[tokio::test]
async fn a_taken_preset_slug_is_a_409_naming_the_slug() {
    let ctx = ctx().await;
    let offer_id = seed_active_offer(&ctx, "product_presets", "").await;
    let presets =
        format!("/b/products/api/admin/products/product_presets/offers/{offer_id}/presets");
    let preset = |name: &str, slug: &str| serde_json::json!({"name": name, "slug": slug, "inputs": {"pages": 4}});
    let expect_taken = |out: wafer_run::OutputStream| async move {
        let parts = wafer_block::http_codec::collect_http_response(out).await;
        let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap_or_default();
        assert_eq!(parts.status, 409, "{body}");
        assert_eq!(
            body["message"],
            serde_json::json!(
                "A checkout preset with the slug \"four-pages\" already exists. \
                 Choose a different slug."
            ),
            "{body}"
        );
        let text = body.to_string();
        assert!(
            !text.contains("impresspress__products") && !text.contains("UNIQUE"),
            "schema leaked: {text}"
        );
    };

    let (msg, input) = admin_create_msg(&presets, preset("Four pages", "four-pages"));
    output_to_json(dispatch(&ctx, msg, input).await).await;

    let (msg, input) = admin_create_msg(&presets, preset("Four again", "four-pages"));
    expect_taken(dispatch(&ctx, msg, input).await).await;

    let (msg, input) = admin_create_msg(&presets, preset("Eight pages", "eight-pages"));
    let other = output_to_json(dispatch(&ctx, msg, input).await).await;
    let other_id = other["id"].as_str().expect("preset id");
    let (mut msg, input) = request_msg(
        "update",
        &format!("{presets}/{other_id}"),
        "admin_1",
        preset("Eight pages", "four-pages"),
    );
    msg.set_meta("auth.user_roles", "admin");
    expect_taken(dispatch(&ctx, msg, input).await).await;
}

#[tokio::test]
async fn admin_preset_payment_link_lifecycle_reuses_and_exposes_only_safe_url() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
    ])
    .await;
    let requests = register_stripe_network(
        &mut ctx,
        serde_json::json!({
            "id": "plink_test_print",
            "url": "https://buy.stripe.com/test_print"
        }),
    );
    let offer_id = seed_active_offer(&ctx, "product_payment_link", "").await;
    let base = format!("/b/products/api/admin/products/product_payment_link/offers/{offer_id}");

    let (msg, input) = admin_create_msg(
        &format!("{base}/presets"),
        serde_json::json!({
            "name": "Four page flyer",
            "slug": "four-page-flyer",
            "inputs": {"pages": 4}
        }),
    );
    let preset = output_to_json(dispatch(&ctx, msg, input).await).await;
    let preset_id = preset["id"].as_str().expect("preset id").to_string();
    assert_eq!(preset["inputs"]["pages"], 4);
    assert_eq!(preset["active"], true);
    assert_eq!(preset["configuration_hash"].as_str().unwrap().len(), 64);

    let create_body = serde_json::json!({
        "preset_id": preset_id,
        "after_completion_url": "https://shop.example/payment-link/thanks?session_id={CHECKOUT_SESSION_ID}"
    });
    let (msg, input) = admin_create_msg(&format!("{base}/payment-links"), create_body.clone());
    let link = output_to_json(dispatch(&ctx, msg, input).await).await;
    let link_id = link["id"]
        .as_str()
        .expect("local Payment Link id")
        .to_string();
    assert_eq!(link["url"], "https://buy.stripe.com/test_print");
    assert_eq!(link["sync_status"], "synced");
    assert!(link.get("stripe_payment_link_id").is_none());

    // Same immutable configuration reuses the existing provider resource.
    let (msg, input) = admin_create_msg(&format!("{base}/payment-links"), create_body);
    let reused = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(reused["id"], link_id);
    assert_eq!(requests.lock().unwrap().len(), 1);

    {
        let requests_guard = requests.lock().unwrap();
        let create = &requests_guard[0];
        assert_eq!(create.url, "https://api.stripe.com/v1/payment_links");
        assert_eq!(create.headers["Stripe-Version"], "2026-02-25.clover");
        let form = String::from_utf8(create.body.clone().unwrap()).unwrap();
        assert!(form.contains("line_items[0][price_data][currency]=nzd"));
        assert!(form.contains("[unit_amount]=1000"));
        assert!(form.contains("[unit_amount]=100"));
        assert!(form.contains("automatic_tax[enabled]=true"));
        assert!(form.contains("after_completion[type]=redirect"));
        assert!(form.contains("metadata[impresspress_payment_link_id]="));
    }

    let (msg, input) = get_msg("/b/products/storefront/product_payment_link", "");
    let storefront = output_to_json(dispatch(&ctx, msg, input).await).await;
    let public_link = &storefront["offers"][0]["payment_links"][0];
    assert_eq!(public_link["id"], link_id);
    assert_eq!(public_link["preset_id"], preset_id);
    assert_eq!(public_link["url"], "https://buy.stripe.com/test_print");
    assert!(public_link.get("configuration_hash").is_none());
    assert!(public_link.get("sync_status").is_none());

    let (msg, input) = delete_msg(&format!("{base}/payment-links/{link_id}"), "admin_1");
    let deactivated = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(deactivated["active"], false);
    {
        let requests_guard = requests.lock().unwrap();
        assert_eq!(requests_guard.len(), 2);
        assert_eq!(
            requests_guard[1].url,
            "https://api.stripe.com/v1/payment_links/plink_test_print"
        );
        assert_eq!(
            requests_guard[1].body.as_deref(),
            Some(b"active=false".as_slice())
        );
    }

    let (msg, input) = get_msg("/b/products/storefront/product_payment_link", "");
    let storefront = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(
        storefront["offers"][0]["payment_links"],
        serde_json::json!([])
    );
}

/// What the Stripe stand-in does with a create it has not seen under the key.
#[derive(Clone, Copy)]
enum FreshOutcome {
    /// Create a Payment Link and save the 200 under the key.
    Create,
    /// Execute and refuse with a 400, saved under the key like Stripe saves
    /// every result of a request whose execution began.
    Reject,
}

/// Rendezvous points for pinning two concurrent creates under one key. When
/// `armed`, the first fresh execution signals `started` once its key is in
/// flight and parks until `release`. A request that meets a key in flight
/// signals `conflict_met` and answers its 409 only after `answer_conflict`.
#[derive(Default)]
struct HeldExecution {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    conflict_met: tokio::sync::Notify,
    answer_conflict: tokio::sync::Notify,
    armed: std::sync::atomic::AtomicBool,
}

/// Stripe's idempotency scope: the `Stripe-Account` a key was sent for, and
/// the key.
type IdempotencyScope = (String, String);

/// What Stripe saved under a key: the request body, and the status and body
/// it answered.
type SavedResult = (Vec<u8>, u16, serde_json::Value);

/// A Stripe stand-in that applies Stripe's idempotency rules to
/// `POST /v1/payment_links`:
///
/// - the first `rate_limited` requests get a 429, which Stripe's rate limiter
///   answers before the idempotency layer, so nothing is saved;
/// - a key whose original request is still executing gets a 409;
/// - a saved key replays its saved status and body, but only for the same
///   parameters; different parameters get a 400 `idempotency_error`;
/// - an unseen key executes (see [`FreshOutcome`]) and saves the result.
///
/// Keys are scoped to the `Stripe-Account` they were sent for. A request to
/// `/v1/payment_links/{id}` (deactivation) answers the link as inactive.
#[derive(Clone, Default)]
struct IdempotentPaymentLinkStripe {
    requests: Arc<Mutex<Vec<Request>>>,
    saved: Arc<Mutex<HashMap<IdempotencyScope, SavedResult>>>,
    in_flight: Arc<Mutex<std::collections::HashSet<IdempotencyScope>>>,
    links_created: Arc<Mutex<Vec<String>>>,
    rate_limited: Arc<Mutex<usize>>,
    /// Answer this many `POST /v1/payment_links/{id}` deactivations with a
    /// 500 before letting one through.
    deactivations_failed: Arc<Mutex<usize>>,
    fresh_outcomes: Arc<Mutex<VecDeque<FreshOutcome>>>,
    held: Arc<HeldExecution>,
}

fn stripe_response(status_code: u16, body: &serde_json::Value) -> Response {
    Response {
        status_code,
        headers: HashMap::new(),
        body: serde_json::to_vec(body).unwrap(),
    }
}

#[async_trait]
impl NetworkService for IdempotentPaymentLinkStripe {
    async fn do_request(&self, request: &Request) -> Result<Response, NetworkError> {
        self.requests.lock().unwrap().push(request.clone());
        if !request.url.ends_with("/v1/payment_links") {
            let id = request.url.rsplit('/').next().unwrap_or("").to_string();
            {
                let mut remaining = self.deactivations_failed.lock().unwrap();
                if *remaining > 0 {
                    *remaining -= 1;
                    return Ok(stripe_response(
                        500,
                        &serde_json::json!({"error": {"type": "api_error"}}),
                    ));
                }
            }
            return Ok(stripe_response(
                200,
                &serde_json::json!({"id": id, "active": false}),
            ));
        }
        {
            let mut remaining = self.rate_limited.lock().unwrap();
            if *remaining > 0 {
                *remaining -= 1;
                return Ok(stripe_response(
                    429,
                    &serde_json::json!({"error": {"type": "rate_limit_error"}}),
                ));
            }
        }
        let scope = (
            request
                .headers
                .get("Stripe-Account")
                .cloned()
                .unwrap_or_default(),
            request.headers["Idempotency-Key"].clone(),
        );
        let body = request.body.clone().unwrap_or_default();
        if self.in_flight.lock().unwrap().contains(&scope) {
            self.held.conflict_met.notify_one();
            self.held.answer_conflict.notified().await;
            return Ok(stripe_response(
                409,
                &serde_json::json!({"error": {"type": "idempotency_error"}}),
            ));
        }
        let replay = self.saved.lock().unwrap().get(&scope).cloned();
        if let Some((saved_body, status_code, response)) = replay {
            if saved_body != body {
                return Ok(stripe_response(
                    400,
                    &serde_json::json!({"error": {"type": "idempotency_error"}}),
                ));
            }
            return Ok(stripe_response(status_code, &response));
        }
        self.in_flight.lock().unwrap().insert(scope.clone());
        if self
            .held
            .armed
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.held.started.notify_one();
            self.held.release.notified().await;
        }
        let outcome = self
            .fresh_outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(FreshOutcome::Create);
        let (status_code, response) = match outcome {
            FreshOutcome::Create => {
                let mut links = self.links_created.lock().unwrap();
                let id = format!("plink_minted_{}", links.len() + 1);
                links.push(id.clone());
                (
                    200,
                    serde_json::json!({
                        "id": id,
                        "url": format!("https://buy.stripe.com/{id}"),
                    }),
                )
            }
            FreshOutcome::Reject => (
                400,
                serde_json::json!({"error": {
                    "type": "invalid_request_error",
                    "code": "account_invalid"
                }}),
            ),
        };
        self.saved
            .lock()
            .unwrap()
            .insert(scope.clone(), (body, status_code, response.clone()));
        self.in_flight.lock().unwrap().remove(&scope);
        Ok(stripe_response(status_code, &response))
    }
}

fn register_idempotent_payment_link_stripe(
    ctx: &mut crate::test_support::TestContext,
    rate_limited: usize,
) -> IdempotentPaymentLinkStripe {
    let stripe = IdempotentPaymentLinkStripe {
        rate_limited: Arc::new(Mutex::new(rate_limited)),
        ..Default::default()
    };
    let block: Arc<dyn Block> = Arc::new(wafer_core::service_blocks::network::NetworkBlock::new(
        Arc::new(stripe.clone()),
    ));
    ctx.register_block("wafer-run/network", block);
    stripe
}

/// The idempotency keys of every Payment Link create, in request order.
fn idempotency_keys(stripe: &IdempotentPaymentLinkStripe) -> Vec<String> {
    stripe
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.url.ends_with("/v1/payment_links"))
        .map(|request| request.headers["Idempotency-Key"].clone())
        .collect()
}

/// Seed a platform offer plus a named preset, and return the product record,
/// the offer id and the create request a Payment Link call sends.
async fn seed_payment_link_configuration(
    ctx: &crate::test_support::TestContext,
    product_id: &str,
) -> (db::Record, String, PaymentLinkCreateRequest) {
    let offer_id = seed_active_offer(ctx, product_id, "").await;
    let preset = repo::checkout_presets::create(
        ctx,
        &offer_id,
        "admin_1",
        &serde_json::from_value(serde_json::json!({
            "name": "Three pages",
            "slug": "three-pages",
            "inputs": {"pages": 3}
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let product = db::get(ctx, repo::products::TABLE, product_id)
        .await
        .unwrap();
    (
        product,
        offer_id,
        PaymentLinkCreateRequest {
            preset_id: Some(preset.id),
            after_completion_url: None,
        },
    )
}

/// A retry of a configuration whose first attempt failed at Stripe must reach
/// Stripe under the SAME idempotency key and re-drive the same local row, not
/// insert a second row with a fresh key.
#[tokio::test]
async fn payment_link_retry_reuses_one_idempotency_key_and_one_row() {
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 1);
    let (product, offer_id, request) =
        seed_payment_link_configuration(&ctx, "product_link_retry").await;

    let error = stripe::create_payment_link(&ctx, &product, &offer_id, &request)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal, "{error:?}");
    let failed = repo::payment_links::list_for_offer(&ctx, &offer_id)
        .await
        .unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].sync_status, "error");

    let link = stripe::create_payment_link(&ctx, &product, &offer_id, &request)
        .await
        .expect("the retry must succeed");
    assert_eq!(link.sync_status, "synced");
    let keys = idempotency_keys(&stripe);
    assert_eq!(keys.len(), 2, "two attempts, two requests: {keys:?}");
    assert_eq!(keys[0], keys[1], "both attempts must share one key");
    assert!(keys[0].starts_with("impresspress_payment_link_"));
    assert_eq!(
        link.id, failed[0].id,
        "the retry must re-drive the failed row"
    );
    let rows = repo::payment_links::list_for_offer(&ctx, &offer_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "no orphan row may be left behind: {rows:?}");
}

/// Stripe created the link but recording it locally failed. The retry must
/// adopt that same Stripe link into the same row — a second live Payment Link
/// for one configuration is a second way to take the buyer's money.
#[tokio::test]
async fn payment_link_retry_adopts_the_link_stripe_created_when_recording_it_failed() {
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    let (product, offer_id, request) =
        seed_payment_link_configuration(&ctx, "product_link_adopt").await;

    // A first attempt takes two filtered updates on the table: the pending
    // row's attempt start, then `mark_synced`. Letting the first through
    // fails only the write after Stripe has answered.
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx.clone(),
        vec![("database.update_where_count", repo::payment_links::TABLE)],
    )
    .after_passing(1);
    stripe::create_payment_link(&failing, &product, &offer_id, &request)
        .await
        .expect_err("the local write after Stripe succeeded fails");
    assert_eq!(
        stripe.links_created.lock().unwrap().len(),
        1,
        "Stripe holds one live link after the first attempt"
    );
    let stuck = repo::payment_links::list_for_offer(&ctx, &offer_id)
        .await
        .unwrap();
    assert_eq!(stuck.len(), 1);
    assert_eq!(stuck[0].sync_status, "syncing");

    let link = stripe::create_payment_link(&ctx, &product, &offer_id, &request)
        .await
        .expect("the retry must succeed");
    assert_eq!(
        stripe.links_created.lock().unwrap().len(),
        1,
        "the retry must adopt the existing Stripe link, not mint a second"
    );
    assert_eq!(link.url, "https://buy.stripe.com/plink_minted_1");
    assert_eq!(
        link.id, stuck[0].id,
        "the retry must re-drive the stuck row"
    );
    let stored = repo::payment_links::get_for_offer(&ctx, &offer_id, &link.id)
        .await
        .unwrap();
    assert_eq!(stored.stripe_payment_link_id, "plink_minted_1");
    // The adopted link's metadata names the row that now records it, which is
    // what a Payment Link checkout webhook resolves.
    for request in stripe.requests.lock().unwrap().iter() {
        let form = String::from_utf8(request.body.clone().unwrap()).unwrap();
        assert!(form.contains(&format!(
            "metadata[impresspress_payment_link_id]={}",
            link.id
        )));
    }
    assert_eq!(
        repo::payment_links::list_for_offer(&ctx, &offer_id)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// Fail the write that records the link Stripe just minted, leaving a row in
/// `syncing` over a live Payment Link. Returns the product, the offer and the
/// stuck row's id.
async fn a_live_link_no_row_records(
    ctx: &crate::test_support::TestContext,
    product_id: &str,
    stripe: &IdempotentPaymentLinkStripe,
) -> (db::Record, String, String) {
    let (product, offer_id, request) = seed_payment_link_configuration(ctx, product_id).await;
    // A first attempt takes two filtered updates on the table: the pending
    // row's attempt start, then `mark_synced`. Letting the first through
    // fails only the write after Stripe has answered.
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx.clone(),
        vec![("database.update_where_count", repo::payment_links::TABLE)],
    )
    .after_passing(1);
    stripe::create_payment_link(&failing, &product, &offer_id, &request)
        .await
        .expect_err("the local write after Stripe succeeded fails");
    assert_eq!(
        stripe.links_created.lock().unwrap().clone(),
        vec!["plink_minted_1".to_string()],
        "Stripe holds one live link"
    );
    let stuck = repo::payment_links::get_for_offer(
        ctx,
        &offer_id,
        &repo::payment_links::list_for_offer(ctx, &offer_id)
            .await
            .unwrap()[0]
            .id,
    )
    .await
    .unwrap();
    assert_eq!(stuck.managed.sync_status, "syncing");
    assert!(
        stuck.stripe_payment_link_id.is_empty(),
        "the row records no link id"
    );
    assert!(
        !stuck.stripe_request.is_empty() && !stuck.stripe_request_at.is_empty(),
        "the attempt's request is recorded before it is sent: {stuck:?}"
    );
    (product, offer_id, stuck.managed.id)
}

/// The requests the Stripe stand-in received, as `(url, body)`.
fn stripe_calls(stripe: &IdempotentPaymentLinkStripe) -> Vec<(String, String)> {
    stripe
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| {
            (
                request.url.clone(),
                String::from_utf8_lossy(request.body.as_deref().unwrap_or_default()).into_owned(),
            )
        })
        .collect()
}

/// How many times Stripe was told to take `stripe_payment_link_id` down.
/// Sending the request is not the same as Stripe accepting it, so a test
/// about retries counts these rather than asking whether one was ever sent.
fn deactivations_of(stripe: &IdempotentPaymentLinkStripe, stripe_payment_link_id: &str) -> usize {
    stripe_calls(stripe)
        .iter()
        .filter(|(url, body)| {
            url.ends_with(&format!("/v1/payment_links/{stripe_payment_link_id}"))
                && body.contains("active=false")
        })
        .count()
}

/// `true` when Stripe was told to take `stripe_payment_link_id` down.
fn was_deactivated_at_stripe(calls: &[(String, String)], stripe_payment_link_id: &str) -> bool {
    calls.iter().any(|(url, body)| {
        url.ends_with(&format!("/v1/payment_links/{stripe_payment_link_id}"))
            && body.contains("active=false")
    })
}

/// Deactivate a Payment Link the way an admin does: the real
/// `DELETE /b/products/api/admin/.../payment-links/{link_id}` route, through
/// the central router.
async fn deactivate_link_over_the_wire(
    ctx: &crate::test_support::TestContext,
    product_id: &str,
    offer_id: &str,
    link_id: &str,
) -> wafer_run::OutputStream {
    let (mut msg, input) = delete_msg(
        &format!(
            "/b/products/api/admin/products/{product_id}/offers/{offer_id}/payment-links/{link_id}"
        ),
        "admin_1",
    );
    msg.set_meta("auth.user_roles", "admin");
    dispatch_routed(ctx, msg, input).await
}

/// Backdate a row's recorded Stripe request by `hours`, putting it outside
/// Stripe's idempotency-key retention.
async fn age_payment_link_request(
    ctx: &crate::test_support::TestContext,
    link_id: &str,
    hours: i64,
) {
    db::update(
        ctx,
        repo::payment_links::TABLE,
        link_id,
        HashMap::from([(
            "stripe_request_at".to_string(),
            serde_json::json!((chrono::Utc::now() - chrono::Duration::hours(hours)).to_rfc3339()),
        )]),
    )
    .await
    .unwrap();
}

/// Seed an active row whose Stripe request went out too long ago to be
/// re-sent: the shape a crashed synchronization leaves behind.
async fn seed_unresolvable_payment_link(
    ctx: &crate::test_support::TestContext,
    offer_id: &str,
    stripe_account_id: &str,
) -> String {
    let link_id = format!("link_stuck_{offer_id}");
    seed(
        ctx,
        repo::payment_links::TABLE,
        &link_id,
        HashMap::from([
            ("offer_id".to_string(), serde_json::json!(offer_id)),
            (
                "stripe_account_id".to_string(),
                serde_json::json!(stripe_account_id),
            ),
            ("active".to_string(), serde_json::json!(true)),
            ("sync_status".to_string(), serde_json::json!("syncing")),
            (
                "stripe_request".to_string(),
                serde_json::json!("[[\"metadata[impresspress_payment_link_id]\",\"x\"]]"),
            ),
        ]),
    )
    .await;
    age_payment_link_request(ctx, &link_id, 25).await;
    link_id
}

/// The queued takedown of one Payment Link row. Panics when none was
/// enqueued, which is the failure this whole area is about.
async fn takedown_operation(ctx: &crate::test_support::TestContext, link_id: &str) -> db::Record {
    repo::provider_operations::list(ctx, None, 1, 50)
        .await
        .expect("list provider operations")
        .records
        .into_iter()
        .find(|operation| {
            operation.str_field("operation_type")
                == repo::provider_operations::PAYMENT_LINK_DEACTIVATE
                && operation.str_field("aggregate_id") == link_id
        })
        .unwrap_or_else(|| panic!("no takedown operation was enqueued for {link_id}"))
}

/// A row whose attempt never recorded a link id can still have a live,
/// buyable link at Stripe. Deactivating it must take that link down: an
/// inactive local row over a live Payment Link is a checkout page that goes
/// on charging buyers with nothing left to reconcile them against.
#[tokio::test]
async fn deactivating_a_row_with_no_recorded_link_takes_the_link_down_at_stripe() {
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    let (product, offer_id, link_id) =
        a_live_link_no_row_records(&ctx, "product_link_unrecorded", &stripe).await;

    let deactivated =
        output_to_json(deactivate_link_over_the_wire(&ctx, &product.id, &offer_id, &link_id).await)
            .await;
    assert_eq!(
        deactivated["active"],
        serde_json::json!(false),
        "the row must come back deactivated: {deactivated}"
    );

    let calls = stripe_calls(&stripe);
    assert!(
        was_deactivated_at_stripe(&calls, "plink_minted_1"),
        "the live link must be deactivated at Stripe: {calls:?}"
    );
    assert_eq!(
        stripe.links_created.lock().unwrap().len(),
        1,
        "learning the link id must replay the saved result, not mint a second link"
    );
}

/// Past Stripe's idempotency-key retention the saved result is gone, so
/// re-sending the request would create a second live link rather than name
/// the first. The row still retires — blocking on it would let one stuck link
/// block an offer archival or a seller suspension — and the takedown
/// dead-letters with what an operator has to do instead.
#[tokio::test]
async fn deactivating_a_row_whose_request_stripe_has_forgotten_dead_letters_the_takedown() {
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    let (product, offer_id, link_id) =
        a_live_link_no_row_records(&ctx, "product_link_forgotten", &stripe).await;
    age_payment_link_request(&ctx, &link_id, 25).await;
    let calls_before = stripe_calls(&stripe).len();

    let deactivated =
        output_to_json(deactivate_link_over_the_wire(&ctx, &product.id, &offer_id, &link_id).await)
            .await;
    assert_eq!(
        deactivated["active"],
        serde_json::json!(false),
        "the row must retire even though its link cannot be named: {deactivated}"
    );
    assert_eq!(
        stripe_calls(&stripe).len(),
        calls_before,
        "re-sending a forgotten request would mint a second live link"
    );

    let operation = takedown_operation(&ctx, &link_id).await;
    assert_eq!(
        operation.str_field("status"),
        "dead_letter",
        "no retry can do better, so the operation must not stay due: {:?}",
        operation.data
    );
    let last_error = operation.str_field("last_error");
    assert!(
        last_error.contains(&format!("metadata[impresspress_payment_link_id]={link_id}")),
        "the operator needs the handle the link actually carries: {last_error}"
    );
}

/// A takedown that fails at Stripe must stay retryable, and the link it could
/// not take down must be named by the row — otherwise the compensating call
/// is one shot and its failure leaves exactly the live, unnamed link this all
/// exists to prevent.
#[tokio::test]
async fn a_takedown_that_fails_at_stripe_is_retried_from_the_recorded_link_id() {
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    // The one deactivation this create compensates with fails.
    *stripe.deactivations_failed.lock().unwrap() = 1;
    let (product, offer_id, request) =
        seed_payment_link_configuration(&ctx, "product_link_takedown_retry").await;

    stripe
        .held
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let create = stripe::create_payment_link(&ctx, &product, &offer_id, &request);
    let retire = async {
        stripe.held.started.notified().await;
        let rows = repo::payment_links::list_for_offer(&ctx, &offer_id)
            .await
            .unwrap();
        repo::payment_links::deactivate_local(&ctx, &offer_id, &rows[0].id)
            .await
            .expect("retire the row mid-flight");
        stripe.held.release.notify_one();
        rows[0].id.clone()
    };
    let (created, link_id) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(create, retire)
    })
    .await
    .expect("the create must reach Stripe");
    created.expect_err("a create whose row was retired cannot report success");

    // The compensating deactivation failed, so the link is still live at
    // Stripe — but the row names it and the operation is due.
    let stored = repo::payment_links::get_for_offer(&ctx, &offer_id, &link_id)
        .await
        .unwrap();
    assert_eq!(
        stored.stripe_payment_link_id, "plink_minted_1",
        "the retired row must record the link minted for it"
    );
    assert!(!stored.managed.active);
    let operation = takedown_operation(&ctx, &link_id).await;
    assert_eq!(
        operation.str_field("status"),
        "pending",
        "a failed takedown must stay due: {:?}",
        operation.data
    );
    assert_eq!(
        deactivations_of(&stripe, "plink_minted_1"),
        1,
        "one attempt was made, and Stripe refused it"
    );

    // The worker that owns the queue finishes it.
    let result = super::super::stripe_provider::reconcile_provider_operations(&ctx, 25)
        .await
        .expect("reconcile the queue");
    assert_eq!(result.succeeded, 1, "{result:?}");
    assert_eq!(
        deactivations_of(&stripe, "plink_minted_1"),
        2,
        "the retry must send the takedown again: {:?}",
        stripe_calls(&stripe)
    );
    assert_eq!(
        takedown_operation(&ctx, &link_id).await.str_field("status"),
        "succeeded"
    );
}

/// The other way a link can refuse to go down: the row names it, but Stripe
/// will not answer. A provider outage is exactly when a fraud control has to
/// complete, so suspension retires the row, queues the takedown and carries
/// on rather than stopping on the first failing link.
#[tokio::test]
async fn seller_suspension_is_not_blocked_when_stripe_refuses_the_takedown() {
    let mut ctx = ctx_with(&[
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_stripe_down",
        ),
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
    ])
    .await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    // Every deactivation this test makes fails.
    *stripe.deactivations_failed.lock().unwrap() = 10;
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_stripe_down_account",
        HashMap::from([
            (
                "user_id".to_string(),
                serde_json::json!("seller_stripe_down"),
            ),
            ("status".to_string(), serde_json::json!("active")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_stripe_down"),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(true)),
            ("payouts_enabled".to_string(), serde_json::json!(true)),
            ("fee_basis_points".to_string(), serde_json::json!(200)),
        ]),
    )
    .await;
    let product_id = "seller_stripe_down_product";
    let offer_id = seed_active_offer(&ctx, product_id, "seller_stripe_down").await;
    // A fully synchronized link: the row names it, so this is not the
    // unresolvable case — only Stripe is refusing.
    let link_id = "link_stripe_down";
    seed(
        &ctx,
        repo::payment_links::TABLE,
        link_id,
        HashMap::from([
            ("offer_id".to_string(), serde_json::json!(&offer_id)),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_stripe_down"),
            ),
            (
                "stripe_payment_link_id".to_string(),
                serde_json::json!("plink_stripe_down"),
            ),
            (
                "url".to_string(),
                serde_json::json!("https://buy.stripe.com/plink_stripe_down"),
            ),
            ("active".to_string(), serde_json::json!(true)),
            ("sync_status".to_string(), serde_json::json!("synced")),
        ]),
    )
    .await;

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/sellers/seller_stripe_down_account/suspend",
        serde_json::json!({}),
    );
    let suspended = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(
        suspended["status"],
        serde_json::json!("suspended"),
        "a Stripe outage must not stop the fraud control: {suspended}"
    );
    assert_eq!(
        db::get(&ctx, repo::products::TABLE, product_id)
            .await
            .unwrap()
            .str_field("status"),
        "archived"
    );
    assert!(
        !repo::payment_links::get(&ctx, link_id)
            .await
            .unwrap()
            .managed
            .active,
        "the row must retire even though its link is still live"
    );
    assert_eq!(
        deactivations_of(&stripe, "plink_stripe_down"),
        1,
        "the takedown was attempted once, and Stripe refused it"
    );
    assert_eq!(
        takedown_operation(&ctx, link_id).await.str_field("status"),
        "pending",
        "the link is still live, so the takedown must be due"
    );

    // And the queue an administrator reconciles finishes the job.
    *stripe.deactivations_failed.lock().unwrap() = 0;
    let result = super::super::stripe_provider::reconcile_provider_operations(&ctx, 25)
        .await
        .expect("reconcile the queue");
    assert_eq!(result.succeeded, 1, "{result:?}");
    assert_eq!(deactivations_of(&stripe, "plink_stripe_down"), 2);
}

/// Suspending a seller is a fraud control: it must not be stoppable by one
/// Payment Link row whose Stripe link nothing can name any more.
#[tokio::test]
async fn seller_suspension_is_not_blocked_by_a_payment_link_that_cannot_be_resolved() {
    let mut ctx = ctx_with(&[
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY",
            "sk_test_stuck_link",
        ),
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
    ])
    .await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_stuck_link_account",
        HashMap::from([
            (
                "user_id".to_string(),
                serde_json::json!("seller_stuck_link"),
            ),
            ("status".to_string(), serde_json::json!("active")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_stuck_link"),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(true)),
            ("payouts_enabled".to_string(), serde_json::json!(true)),
            ("fee_basis_points".to_string(), serde_json::json!(200)),
        ]),
    )
    .await;
    let product_id = "seller_stuck_link_product";
    let offer_id = seed_active_offer(&ctx, product_id, "seller_stuck_link").await;
    // A row whose attempt went out but never came back, long enough ago that
    // Stripe has forgotten its idempotency key.
    let link_id = seed_unresolvable_payment_link(&ctx, &offer_id, "acct_stuck_link").await;

    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/sellers/seller_stuck_link_account/suspend",
        serde_json::json!({}),
    );
    let suspended = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(
        suspended["status"],
        serde_json::json!("suspended"),
        "the fraud control must complete: {suspended}"
    );
    assert_eq!(
        db::get(&ctx, repo::products::TABLE, product_id)
            .await
            .unwrap()
            .str_field("status"),
        "archived",
        "suspension must reach the product rows"
    );
    assert!(
        !repo::payment_links::get(&ctx, &link_id)
            .await
            .unwrap()
            .managed
            .active,
        "the stuck link's row must still retire"
    );
    assert_eq!(
        takedown_operation(&ctx, &link_id).await.str_field("status"),
        "dead_letter",
        "and what an operator must finish by hand must be queued for them"
    );
    assert!(
        stripe.requests.lock().unwrap().is_empty(),
        "a forgotten request must not be re-sent"
    );
}

/// A row no request has ever been sent for has nothing at Stripe, and
/// deactivates locally. (A guard: it passes before the re-send path exists
/// too. The row here stores an empty JSON list; the shapes a row written
/// before the column existed has are covered by
/// `a_row_from_before_the_request_column_deactivates_locally`.)
#[tokio::test]
async fn deactivating_a_row_with_no_stripe_request_stays_local() {
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    let product_id = "product_link_never_sent";
    let offer_id = seed_active_offer(&ctx, product_id, "").await;
    let offer = repo::offers::get_managed(&ctx, &offer_id).await.unwrap();
    let preview = offer_pricing::evaluate_offer(
        &offer.offer,
        &PricingPreviewRequest {
            offer_id: offer_id.clone(),
            quantity: 1,
            inputs: serde_json::from_value(serde_json::json!({"pages": 2})).unwrap(),
        },
        offer_pricing::InputScope::Management,
    )
    .unwrap();
    let link_id = seed_pending_payment_link(&ctx, &offer_id, "never-sent", &preview)
        .await
        .managed
        .id;

    let deactivated =
        output_to_json(deactivate_link_over_the_wire(&ctx, product_id, &offer_id, &link_id).await)
            .await;
    assert_eq!(
        deactivated["active"],
        serde_json::json!(false),
        "{deactivated}"
    );
    assert!(
        stripe.requests.lock().unwrap().is_empty(),
        "a row with no request in flight must not reach Stripe"
    );
}

/// The real shape of a row written before the `stripe_request` column: the
/// migration's `DEFAULT ''` leaves it empty rather than an empty JSON list,
/// and a row the database layer never gave the column at all reads the same.
/// Both must hydrate as "no request in flight" — reading either as JSON would
/// fail the row closed and make it undeactivatable.
#[tokio::test]
async fn a_row_from_before_the_request_column_deactivates_locally() {
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    let product_id = "product_link_pre_migration";
    let offer_id = seed_active_offer(&ctx, product_id, "").await;

    // Seeded without the column: the database layer supplies the same
    // default the migration gives an existing row.
    let absent = "link_pre_migration_absent";
    seed(
        &ctx,
        repo::payment_links::TABLE,
        absent,
        HashMap::from([
            ("offer_id".to_string(), serde_json::json!(&offer_id)),
            ("active".to_string(), serde_json::json!(true)),
            ("sync_status".to_string(), serde_json::json!("syncing")),
        ]),
    )
    .await;
    // And the explicit empty string the `ALTER TABLE ... DEFAULT ''` writes.
    let empty = "link_pre_migration_empty";
    seed(
        &ctx,
        repo::payment_links::TABLE,
        empty,
        HashMap::from([
            ("offer_id".to_string(), serde_json::json!(&offer_id)),
            ("active".to_string(), serde_json::json!(true)),
            ("sync_status".to_string(), serde_json::json!("syncing")),
            ("stripe_request".to_string(), serde_json::json!("")),
            ("stripe_request_at".to_string(), serde_json::json!("")),
        ]),
    )
    .await;

    for link_id in [absent, empty] {
        let stored = repo::payment_links::get(&ctx, link_id).await.unwrap();
        assert!(
            stored.stripe_request.is_empty() && stored.stripe_request_at.is_empty(),
            "{link_id} must read as no request in flight: {stored:?}"
        );
        let deactivated = output_to_json(
            deactivate_link_over_the_wire(&ctx, product_id, &offer_id, link_id).await,
        )
        .await;
        assert_eq!(
            deactivated["active"],
            serde_json::json!(false),
            "{link_id}: {deactivated}"
        );
    }
    assert!(
        stripe.requests.lock().unwrap().is_empty(),
        "neither shape has anything at Stripe to take down"
    );
}

/// The other order of the same race: the row is retired while Stripe is
/// minting its link. Nothing local will ever point at that link, so the
/// create must take it down at Stripe instead of reporting success over a
/// row that says the configuration is not for sale.
#[tokio::test]
async fn a_link_minted_for_a_row_retired_mid_flight_is_deactivated_at_stripe() {
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    let (product, offer_id, request) =
        seed_payment_link_configuration(&ctx, "product_link_retired").await;

    // Park the create inside Stripe, retire its row there, then let it go:
    // it comes back from Stripe holding a link its row no longer wants.
    stripe
        .held
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let create = stripe::create_payment_link(&ctx, &product, &offer_id, &request);
    let retire = async {
        stripe.held.started.notified().await;
        let rows = repo::payment_links::list_for_offer(&ctx, &offer_id)
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "the create writes its row before it calls Stripe"
        );
        repo::payment_links::deactivate_local(&ctx, &offer_id, &rows[0].id)
            .await
            .expect("retire the row mid-flight");
        stripe.held.release.notify_one();
        rows[0].id.clone()
    };
    let (created, link_id) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(create, retire)
    })
    .await
    .expect("the create must reach Stripe");

    let error = created.expect_err("a create whose row was retired cannot report success");
    assert_eq!(error.code, ErrorCode::Aborted, "{error:?}");
    let calls = stripe_calls(&stripe);
    assert!(
        was_deactivated_at_stripe(&calls, "plink_minted_1"),
        "the link minted for the retired row must be deactivated at Stripe: {calls:?}"
    );
    let stored = repo::payment_links::get_for_offer(&ctx, &offer_id, &link_id)
        .await
        .unwrap();
    assert!(!stored.managed.active);
    assert_eq!(
        stored.stripe_payment_link_id, "plink_minted_1",
        "the retired row must name the link so the takedown is retryable"
    );
    assert!(
        stored.managed.url.is_empty() && stored.managed.sync_status != "synced",
        "naming the link must not make a retired row sellable again: {stored:?}"
    );
    assert_eq!(
        takedown_operation(&ctx, &link_id).await.str_field("status"),
        "succeeded",
        "the takedown settled inline, so nothing is left due"
    );
}

/// A configuration that can never produce a valid Stripe request is refused
/// before anything is written: retrying it must not pile up `syncing` rows.
#[tokio::test]
async fn an_invalid_payment_link_request_leaves_no_row_behind() {
    // No offer country list and no platform country: the shipping section of
    // the form cannot be built.
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    let offer_id = seed_shipping_offer(&ctx, "product_link_no_country", &[]).await;
    let product = db::get(&ctx, repo::products::TABLE, "product_link_no_country")
        .await
        .unwrap();
    let request = PaymentLinkCreateRequest {
        preset_id: None,
        after_completion_url: None,
    };
    for _ in 0..2 {
        let error = stripe::create_payment_link(&ctx, &product, &offer_id, &request)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{error:?}");
    }
    assert!(
        repo::payment_links::list_for_offer(&ctx, &offer_id)
            .await
            .unwrap()
            .is_empty(),
        "a request that fails to build must leave no row"
    );

    // A malformed platform country fails every attempt the same way.
    ctx.set_config("IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY", "not-a-country");
    let (product, offer_id, request) =
        seed_payment_link_configuration(&ctx, "product_link_bad_country").await;
    for _ in 0..2 {
        let error = stripe::create_payment_link(&ctx, &product, &offer_id, &request)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::FailedPrecondition, "{error:?}");
    }
    assert!(
        repo::payment_links::list_for_offer(&ctx, &offer_id)
            .await
            .unwrap()
            .is_empty(),
        "a malformed platform country must leave no row"
    );
    assert!(stripe.requests.lock().unwrap().is_empty());
}

/// Deactivating a link and asking for the same configuration again is an
/// ordinary admin flow. It must mint a fresh Stripe link, not collide with
/// the deactivated link's idempotency key (a parameter mismatch) or replay
/// the deactivated link.
#[tokio::test]
async fn a_deactivated_configuration_can_be_recreated_as_a_new_stripe_link() {
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    let (product, offer_id, request) =
        seed_payment_link_configuration(&ctx, "product_link_recreate").await;

    let first = stripe::create_payment_link(&ctx, &product, &offer_id, &request)
        .await
        .expect("the first link");
    stripe::deactivate_payment_link(&ctx, &offer_id, &first.id)
        .await
        .expect("deactivate");
    let second = stripe::create_payment_link(&ctx, &product, &offer_id, &request)
        .await
        .expect("recreating a deactivated configuration must succeed");

    assert_eq!(
        stripe.links_created.lock().unwrap().clone(),
        vec!["plink_minted_1".to_string(), "plink_minted_2".to_string()],
        "the recreate must be a new Stripe link"
    );
    assert_ne!(second.id, first.id);
    assert_eq!(second.url, "https://buy.stripe.com/plink_minted_2");
    assert_eq!(second.sync_status, "synced");
    let keys = idempotency_keys(&stripe);
    assert_ne!(keys[0], keys[1], "a recreate must not reuse the old key");
}

/// Two first attempts at one configuration that both find nothing must end
/// as ONE row and ONE Stripe link, with neither request failing — not as two
/// rows whose requests share a key with different parameters.
#[tokio::test]
async fn concurrent_first_attempts_share_one_row_and_one_stripe_link() {
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    let (product, offer_id, request) =
        seed_payment_link_configuration(&ctx, "product_link_race").await;

    // Both racers finish the configuration lookup, and so both see no row,
    // before either writes one.
    let racing = crate::test_support::RendezvousDbOpContext::new(
        ctx.clone(),
        "database.list",
        repo::payment_links::TABLE,
        2,
    );
    let (left, right) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            stripe::create_payment_link(&racing, &product, &offer_id, &request),
            stripe::create_payment_link(&racing, &product, &offer_id, &request),
        )
    })
    .await
    .expect("the racers must not deadlock");
    let left = left.expect("the first racer must succeed");
    let right = right.expect("the second racer must succeed");

    assert_eq!(left.id, right.id, "both racers must land on one row");
    assert_eq!(
        stripe.links_created.lock().unwrap().len(),
        1,
        "one configuration, one Stripe link"
    );
    let rows = repo::payment_links::list_for_offer(&ctx, &offer_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "no race-loser row may be left: {rows:?}");
    assert_eq!(rows[0].sync_status, "synced");
}

/// Stripe saves and replays the result of a request it began executing,
/// including a 400 caused by Stripe-side state the seller can fix. A retry
/// after such a refusal must reach Stripe under a fresh key, or it replays
/// the stale refusal for as long as Stripe retains the key.
#[tokio::test]
async fn a_retry_after_a_definite_stripe_refusal_uses_a_fresh_key() {
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 0);
    stripe
        .fresh_outcomes
        .lock()
        .unwrap()
        .push_back(FreshOutcome::Reject);
    let (product, offer_id, request) =
        seed_payment_link_configuration(&ctx, "product_link_refused").await;

    let error = stripe::create_payment_link(&ctx, &product, &offer_id, &request)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::FailedPrecondition, "{error:?}");

    let link = stripe::create_payment_link(&ctx, &product, &offer_id, &request)
        .await
        .expect("once the Stripe-side cause is fixed, the retry must succeed");
    let keys = idempotency_keys(&stripe);
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1], "a definite refusal must retire its key");
    assert_eq!(link.url, "https://buy.stripe.com/plink_minted_1");
    let rows = repo::payment_links::list_for_offer(&ctx, &offer_id)
        .await
        .unwrap();
    let refused = rows
        .iter()
        .find(|row| row.id != link.id)
        .expect("the refused attempt stays on record");
    assert!(!refused.active, "a refused attempt is not a live link");
    assert_eq!(refused.sync_status, "error");
}

/// Two concurrent retries of one unfinished row share its key. The one
/// Stripe answers with a 409 (the other is still executing) must not mark
/// the row failed after the other has recorded the live link.
#[tokio::test]
async fn a_conflicting_retry_cannot_unsync_the_link_its_twin_recorded() {
    // The first attempt is rate limited, which leaves an unfinished row.
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 1);
    let (product, offer_id, request) =
        seed_payment_link_configuration(&ctx, "product_link_twins").await;
    stripe::create_payment_link(&ctx, &product, &offer_id, &request)
        .await
        .expect_err("rate limited");

    // The first retry parks inside Stripe with the key in flight; the second
    // meets it there and gets a 409, which it only sees once the first has
    // been let go and has recorded the link.
    stripe
        .held
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let held = stripe.held.clone();
    let first = async {
        let result = stripe::create_payment_link(&ctx, &product, &offer_id, &request).await;
        held.answer_conflict.notify_one();
        result
    };
    let second = async {
        stripe.held.started.notified().await;
        stripe::create_payment_link(&ctx, &product, &offer_id, &request).await
    };
    let let_first_go = async {
        stripe.held.conflict_met.notified().await;
        stripe.held.release.notify_one();
    };
    // A retry that never meets its twin in flight leaves the held execution
    // parked; the timeout turns that into a failure instead of a hang.
    let (first, second, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(first, second, let_first_go)
    })
    .await
    .expect("the second retry must meet the first in flight at Stripe");
    let first = first.expect("the executing retry records the link");
    second.expect_err("the conflicting retry reports the conflict");

    let stored = repo::payment_links::get_for_offer(&ctx, &offer_id, &first.id)
        .await
        .unwrap();
    assert_eq!(
        stored.managed.sync_status, "synced",
        "a late conflict must not flip a recorded link to error"
    );
    assert!(stored.managed.active);
    assert_eq!(stored.stripe_payment_link_id, "plink_minted_1");
}

/// The other order of the race above: the 409 reaches the conflicting retry
/// while its twin is still executing. A 409 says nothing about the outcome,
/// so it must not retire the row the twin is about to record the link on —
/// a live link on a retired row would make the next request mint a second.
#[tokio::test]
async fn a_conflict_answered_mid_flight_does_not_retire_the_row() {
    let mut ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    let stripe = register_idempotent_payment_link_stripe(&mut ctx, 1);
    let (product, offer_id, request) =
        seed_payment_link_configuration(&ctx, "product_link_mid_flight").await;
    stripe::create_payment_link(&ctx, &product, &offer_id, &request)
        .await
        .expect_err("rate limited");

    stripe
        .held
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    // The conflict is answered as soon as it is met, and the first retry is
    // let go only once the second has finished recording its failure.
    stripe.held.answer_conflict.notify_one();
    let first = stripe::create_payment_link(&ctx, &product, &offer_id, &request);
    let second = async {
        stripe.held.started.notified().await;
        let result = stripe::create_payment_link(&ctx, &product, &offer_id, &request).await;
        stripe.held.release.notify_one();
        result
    };
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(first, second)
    })
    .await
    .expect("the second retry must meet the first in flight at Stripe");
    let first = first.expect("the executing retry records the link");
    second.expect_err("the conflicting retry reports the conflict");

    let stored = repo::payment_links::get_for_offer(&ctx, &offer_id, &first.id)
        .await
        .unwrap();
    assert!(
        stored.managed.active,
        "a 409 must not retire the row its twin records the link on"
    );
    assert_eq!(stored.managed.sync_status, "synced");
    let again = stripe::create_payment_link(&ctx, &product, &offer_id, &request)
        .await
        .expect("the recorded link is reused");
    assert_eq!(again.id, first.id);
    assert_eq!(stripe.links_created.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn typed_checkout_can_use_validated_named_preset_without_runtime_inputs() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
    ])
    .await;
    register_stripe_network(
        &mut ctx,
        serde_json::json!({
            "id": "cs_test_preset",
            "url": "https://checkout.stripe.com/c/pay/cs_test_preset"
        }),
    );
    let offer_id = seed_active_offer(&ctx, "product_preset_checkout", "").await;
    let preset = repo::checkout_presets::create(
        &ctx,
        &offer_id,
        "admin_1",
        &serde_json::from_value(serde_json::json!({
            "name": "Six pages",
            "slug": "six-pages",
            "inputs": {"pages": 6}
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({
            "offer_id": offer_id,
            "preset_id": preset.id
        }),
    );
    let checkout = output_to_json(stripe::handle_checkout(&ctx, &msg, input).await).await;
    assert_eq!(checkout["amounts"]["total_minor"], 1150);

    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({
            "offer_id": offer_id,
            "preset_id": preset.id,
            "inputs": {"pages": 2}
        }),
    );
    assert!(
        output_is_error(
            stripe::handle_checkout(&ctx, &msg, input).await,
            ErrorCode::InvalidArgument
        )
        .await
    );
}

/// Seed an active platform offer whose price hinges on a hidden `comp`
/// toggle and an admin-only `discount_tier`, alongside the public `pages`
/// input.
async fn seed_active_offer_with_restricted_variables(
    ctx: &crate::test_support::TestContext,
    product_id: &str,
) -> String {
    seed(
        ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([
            ("name".to_string(), serde_json::json!("Configurable print")),
            ("slug".to_string(), serde_json::json!(product_id)),
            ("status".to_string(), serde_json::json!("active")),
            ("approval_status".to_string(), serde_json::json!("approved")),
            ("owner_kind".to_string(), serde_json::json!("platform")),
            ("owner_id".to_string(), serde_json::json!("")),
            ("created_by".to_string(), serde_json::json!("")),
        ]),
    )
    .await;
    let definition: OfferDefinitionRequest = serde_json::from_value(serde_json::json!({
        "name": "Print configuration",
        "mode": "payment",
        "currency": "nzd",
        "pricing_model": "components",
        "usage_type": "licensed",
        "billing_scheme": "per_unit",
        "tax_behavior": "exclusive",
        "variables": [
            {
                "key": "pages",
                "kind": "integer",
                "label": "Pages",
                "required": true,
                "minimum": "1",
                "maximum": "20",
                "step": "1"
            },
            {
                "key": "comp",
                "kind": "boolean",
                "label": "Comp this order",
                "default_value": false,
                "visibility": "hidden"
            },
            {
                "key": "discount_tier",
                "kind": "select",
                "label": "Discount tier",
                "default_value": "none",
                "allowed_values": ["none", "half"],
                "visibility": "admin_only"
            }
        ],
        "components": [
            {
                "key": "setup",
                "label": "Setup",
                "required": true,
                "amount": {"type": "fixed", "unit_amount_minor": 1000},
                "condition": {"op": "equals", "input": "comp", "value": false}
            },
            {
                "key": "comped_setup",
                "label": "Comped setup",
                "required": true,
                "amount": {"type": "fixed", "unit_amount_minor": 100},
                "condition": {"op": "equals", "input": "comp", "value": true}
            },
            {
                "key": "pages",
                "label": "Printed pages",
                "required": true,
                "amount": {
                    "type": "per_unit",
                    "input": "pages",
                    "unit_amount_minor": 25
                }
            }
        ]
    }))
    .unwrap();
    let offer = repo::offers::create(ctx, product_id, "admin_1", &definition)
        .await
        .expect("create offer");
    repo::offers::publish(ctx, product_id, &offer.offer.id)
        .await
        .expect("publish offer");
    offer.offer.id
}

#[tokio::test]
async fn public_checkout_rejects_restricted_inputs_while_presets_may_pin_them() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
    ])
    .await;
    register_stripe_network(
        &mut ctx,
        serde_json::json!({
            "id": "cs_test_restricted",
            "url": "https://checkout.stripe.com/c/pay/cs_test_restricted"
        }),
    );
    let offer_id =
        seed_active_offer_with_restricted_variables(&ctx, "product_restricted_inputs").await;

    // An anonymous buyer must not be able to set the hidden comp toggle or
    // the admin-only discount tier on the direct checkout path.
    for restricted in [
        serde_json::json!({"pages": 2, "comp": true}),
        serde_json::json!({"pages": 2, "discount_tier": "half"}),
    ] {
        let (msg, input) = create_msg(
            "/b/products/checkout",
            "",
            serde_json::json!({"offer_id": offer_id, "inputs": restricted}),
        );
        assert!(
            output_is_error(
                stripe::handle_checkout(&ctx, &msg, input).await,
                ErrorCode::InvalidArgument
            )
            .await
        );
    }

    // Customer-visible inputs still check out at the undiscounted total.
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({"offer_id": offer_id, "inputs": {"pages": 2}}),
    );
    let checkout = output_to_json(stripe::handle_checkout(&ctx, &msg, input).await).await;
    assert_eq!(checkout["amounts"]["total_minor"], 1050);

    // A management-authored preset may deliberately pin the hidden toggle,
    // and checking out through that preset keeps working.
    let preset = repo::checkout_presets::create(
        &ctx,
        &offer_id,
        "admin_1",
        &serde_json::from_value(serde_json::json!({
            "name": "Comped two pages",
            "slug": "comped-two-pages",
            "inputs": {"pages": 2, "comp": true}
        }))
        .unwrap(),
    )
    .await
    .expect("management preset may pin hidden variables");
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({"offer_id": offer_id, "preset_id": preset.id}),
    );
    let comped = output_to_json(stripe::handle_checkout(&ctx, &msg, input).await).await;
    assert_eq!(comped["amounts"]["total_minor"], 150);
}

#[tokio::test]
async fn checkout_and_payment_links_enforce_offer_total_policy_before_stripe() {
    let ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
    ])
    .await;
    let offer_id = seed_active_offer(&ctx, "product_total_policy", "").await;
    let preset = repo::checkout_presets::create(
        &ctx,
        &offer_id,
        "admin_1",
        &serde_json::from_value(serde_json::json!({
            "name": "Two pages",
            "slug": "two-pages",
            "inputs": {"pages": 2}
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    db::update(
        &ctx,
        repo::offers::TABLE,
        &offer_id,
        HashMap::from([(
            "config_json".to_string(),
            serde_json::json!(r#"{"minimum_total_minor":1051}"#),
        )]),
    )
    .await
    .unwrap();

    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({
            "offer_id": offer_id,
            "inputs": {"pages": 2}
        }),
    );
    assert!(
        output_is_error(
            stripe::handle_checkout(&ctx, &msg, input).await,
            ErrorCode::InvalidArgument,
        )
        .await
    );

    let product = db::get(&ctx, repo::products::TABLE, "product_total_policy")
        .await
        .unwrap();
    let error = stripe::create_payment_link(
        &ctx,
        &product,
        &offer_id,
        &PaymentLinkCreateRequest {
            preset_id: Some(preset.id),
            after_completion_url: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(error.message.contains("below the offer minimum"));
}

/// A Payment Link delivery creates the local order, attaches the session id,
/// and then completes it. A crash between attach and completion used to make
/// the redelivery a false duplicate (any order for the session short-circuited
/// as Ok), sealing the event with a paid order stranded in `pending`. The
/// redelivery must resume the completion path, and a crash between completion
/// and the subscription-item snapshot must backfill the snapshot.
#[tokio::test]
async fn payment_link_redelivery_resumes_partial_order_and_backfills_snapshot() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    let product_id = "product_payment_link_resume";
    seed(
        &ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([
            ("name".to_string(), serde_json::json!("Care plan")),
            ("slug".to_string(), serde_json::json!(product_id)),
            ("status".to_string(), serde_json::json!("active")),
            ("approval_status".to_string(), serde_json::json!("approved")),
            ("owner_kind".to_string(), serde_json::json!("platform")),
        ]),
    )
    .await;
    let definition: OfferDefinitionRequest = serde_json::from_value(serde_json::json!({
        "name": "Monthly subscription",
        "mode": "subscription",
        "currency": "nzd",
        "pricing_model": "fixed",
        "recurring_interval": "month",
        "interval_count": 1,
        "usage_type": "licensed",
        "billing_scheme": "per_unit",
        "tax_behavior": "exclusive",
        "components": [{
            "key": "plan",
            "label": "Care plan",
            "required": true,
            "amount": {"type": "fixed", "unit_amount_minor": 4900}
        }]
    }))
    .unwrap();
    let offer = repo::offers::create(&ctx, product_id, "admin_1", &definition)
        .await
        .unwrap();
    let offer_id = offer.offer.id;
    repo::offers::publish(&ctx, product_id, &offer_id)
        .await
        .unwrap();
    let managed = repo::offers::get_managed(&ctx, &offer_id).await.unwrap();
    let preview = offer_pricing::evaluate_offer(
        &managed.offer,
        &PricingPreviewRequest {
            offer_id: offer_id.clone(),
            quantity: 1,
            inputs: Default::default(),
        },
        offer_pricing::InputScope::Management,
    )
    .unwrap();
    let pending_link =
        seed_pending_payment_link(&ctx, &offer_id, "resume-link-config", &preview).await;
    let link_id = pending_link.managed.id;
    repo::payment_links::mark_synced(
        &ctx,
        &link_id,
        "plink_resume",
        "https://buy.stripe.com/resume",
    )
    .await
    .unwrap();

    let event = serde_json::json!({
        "id": "evt_payment_link_resume",
        "type": "checkout.session.completed",
        "livemode": false,
        "data": {
            "object": {
                "id": "cs_payment_link_resume",
                "payment_link": "plink_resume",
                "mode": "subscription",
                "payment_status": "paid",
                "metadata": {
                    "impresspress_payment_link_id": link_id,
                    "offer_id": offer_id,
                    "offer_version": "1"
                },
                "currency": "nzd",
                "amount_subtotal": 4900,
                "amount_total": 4900,
                "total_details": {
                    "amount_discount": 0,
                    "amount_tax": 0,
                    "amount_shipping": 0
                },
                "customer_details": {"email": "guest@example.com"},
                "customer": "cus_resume",
                "payment_intent": null,
                "subscription": "sub_resume",
                "livemode": false
            }
        }
    });

    // First delivery: the order and its line items are created and the
    // session id is attached, then the completion write hits a simulated
    // outage. The delivery must fail retryably with the order left pending.
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx.clone(),
        vec![(
            "database.update_where_count",
            repo::purchases::PURCHASES_TABLE,
        )],
    );
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&failing, &msg, input).await,
            ErrorCode::Internal,
        )
        .await
    );
    let order = repo::purchases::find_by_session(&ctx, "cs_payment_link_resume")
        .await
        .unwrap()
        .expect("order created by the failed delivery");
    assert_eq!(order.data["status"], "pending");
    assert_eq!(
        db::list_all(&ctx, repo::subscription_items::TABLE, vec![])
            .await
            .unwrap()
            .len(),
        0
    );
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_payment_link_resume",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "failed");
    assert!(!event_row.str_field("next_retry_at").is_empty());

    // Second delivery: the redelivery must resume the completion path for
    // the existing pending order — this time the subscription-item snapshot
    // hits an outage after the completion write lands.
    db::update(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_payment_link_resume",
        HashMap::from([(
            "next_retry_at".to_string(),
            serde_json::json!("2000-01-01T00:00:00Z"),
        )]),
    )
    .await
    .unwrap();
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx.clone(),
        vec![("database.upsert", repo::subscription_items::TABLE)],
    );
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&failing, &msg, input).await,
            ErrorCode::Internal,
        )
        .await
    );
    let order = repo::purchases::find_by_session(&ctx, "cs_payment_link_resume")
        .await
        .unwrap()
        .expect("resumed order");
    assert_eq!(order.data["status"], "completed");
    assert_eq!(order.data["stripe_subscription_id"], "sub_resume");
    assert_eq!(
        db::list_all(&ctx, repo::subscription_items::TABLE, vec![])
            .await
            .unwrap()
            .len(),
        0
    );

    // Third delivery: the order is terminal, so the redelivery is a
    // duplicate — but it must still backfill the missing idempotent
    // subscription-item snapshot before sealing the event.
    db::update(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_payment_link_resume",
        HashMap::from([(
            "next_retry_at".to_string(),
            serde_json::json!("2000-01-01T00:00:00Z"),
        )]),
    )
    .await
    .unwrap();
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    let order = repo::purchases::find_by_session(&ctx, "cs_payment_link_resume")
        .await
        .unwrap()
        .expect("completed order");
    assert_eq!(order.data["status"], "completed");
    assert_eq!(order.data["reconciliation_status"], "reconciled");
    assert_eq!(order.data["buyer_email"], "guest@example.com");
    assert_eq!(order.data["subtotal_cents"], 4900);
    assert_eq!(order.data["total_cents"], 4900);
    // Exactly one order and one immutable line snapshot exist across all
    // three deliveries.
    assert_eq!(
        db::count(&ctx, repo::purchases::PURCHASES_TABLE, &[])
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        repo::purchases::list_line_items(&ctx, &order.id)
            .await
            .unwrap()
            .len(),
        1
    );
    let items = db::list_all(
        &ctx,
        repo::subscription_items::TABLE,
        vec![wafer_block::db::Filter {
            field: "subscription_id".to_string(),
            operator: wafer_block::db::FilterOp::Equal,
            value: serde_json::json!("sub_resume"),
        }],
    )
    .await
    .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].data["purchase_id"], order.id);
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_payment_link_resume",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "processed");
}

#[tokio::test]
async fn payment_link_webhook_reconciles_exact_order_and_rejects_tampering() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        (
            "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
            WEBHOOK_SECRET,
        ),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
    ])
    .await;
    register_stripe_network(
        &mut ctx,
        serde_json::json!({
            "id": "plink_reconcile_print",
            "url": "https://buy.stripe.com/reconcile_print"
        }),
    );
    let offer_id = seed_active_offer(&ctx, "product_payment_link_webhook", "").await;
    db::update(
        &ctx,
        repo::offers::TABLE,
        &offer_id,
        HashMap::from([(
            "config_json".to_string(),
            serde_json::json!(serde_json::json!({
                "collect_shipping_address": true,
                "allowed_shipping_countries": ["NZ"],
                "shipping_options": [{
                    "display_name": "Standard",
                    "amount_minor": 500,
                    "tax_behavior": "exclusive",
                    "stripe_shipping_rate_id": "shr_standard_123"
                }]
            })
            .to_string()),
        )]),
    )
    .await
    .expect("configure immutable Payment Link shipping policy");
    let preset = repo::checkout_presets::create(
        &ctx,
        &offer_id,
        "admin_1",
        &serde_json::from_value(serde_json::json!({
            "name": "Four pages",
            "slug": "four-pages",
            "inputs": {"pages": 4}
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let product = db::get(&ctx, repo::products::TABLE, "product_payment_link_webhook")
        .await
        .unwrap();
    let link = stripe::create_payment_link(
        &ctx,
        &product,
        &offer_id,
        &PaymentLinkCreateRequest {
            preset_id: Some(preset.id),
            after_completion_url: Some(
                "https://shop.example/thanks?session_id={CHECKOUT_SESSION_ID}".to_string(),
            ),
        },
    )
    .await
    .unwrap();

    let event = serde_json::json!({
        "id": "evt_payment_link_paid",
        "type": "checkout.session.async_payment_succeeded",
        "data": {
            "object": {
                "id": "cs_payment_link_paid",
                "payment_link": "plink_reconcile_print",
                "mode": "payment",
                "payment_status": "paid",
                "metadata": {
                    "impresspress_payment_link_id": link.id,
                    "offer_id": offer_id,
                    "offer_version": "1"
                },
                "currency": "nzd",
                "amount_subtotal": 1100,
                "amount_total": 1765,
                "total_details": {
                    "amount_discount": 0,
                    "amount_tax": 165,
                    "amount_shipping": 500
                },
                "customer_details": {"email": "guest@example.com"},
                "customer": "cus_payment_link_guest",
                "payment_intent": "pi_payment_link_paid",
                "subscription": null,
                "livemode": false
            }
        }
    });
    let mut pending = event.clone();
    pending["id"] = serde_json::json!("evt_payment_link_pending");
    pending["type"] = serde_json::json!("checkout.session.completed");
    pending["data"]["object"]["payment_status"] = serde_json::json!("unpaid");
    pending["data"]["object"]["payment_intent"] = serde_json::Value::Null;
    let (msg, input) = webhook_msg(&pending, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    assert!(
        repo::purchases::find_by_session(&ctx, "cs_payment_link_paid")
            .await
            .unwrap()
            .is_none()
    );

    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);

    let order = repo::purchases::find_by_session(&ctx, "cs_payment_link_paid")
        .await
        .unwrap()
        .expect("Payment Link order");
    assert_eq!(order.data["status"], "completed");
    assert_eq!(order.data["checkout_mode"], "payment_link");
    assert_eq!(order.data["buyer_email"], "guest@example.com");
    assert_eq!(order.data["currency"], "NZD");
    assert_eq!(order.data["subtotal_cents"], 1100);
    assert_eq!(order.data["discount_cents"], 0);
    assert_eq!(order.data["tax_cents"], 165);
    assert_eq!(order.data["shipping_cents"], 500);
    assert_eq!(order.data["total_cents"], 1765);
    assert_eq!(
        order.data["provider_payment_intent_id"],
        "pi_payment_link_paid"
    );
    assert_eq!(order.data["stripe_customer_id"], "cus_payment_link_guest");
    assert_eq!(order.data["reconciliation_status"], "reconciled");
    let items = repo::purchases::list_line_items(&ctx, &order.id)
        .await
        .unwrap();
    assert_eq!(items.len(), 2);
    let mut exact: Vec<_> = items
        .iter()
        .map(|item| {
            (
                item.data["unit_amount_minor"].as_i64().unwrap(),
                item.data["total_minor"].as_i64().unwrap(),
            )
        })
        .collect();
    exact.sort_unstable();
    assert_eq!(exact, vec![(100, 100), (1000, 1000)]);

    // Stripe retries the same event id without creating another local order.
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let replay = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(replay["duplicate"], true);
    assert_eq!(
        db::count(&ctx, "impresspress__products__purchases", &[])
            .await
            .unwrap(),
        1
    );

    // A valid signature is not enough: the provider subtotal must still match
    // the immutable local quote associated with this reusable URL.
    let mut tampered = event.clone();
    tampered["id"] = serde_json::json!("evt_payment_link_tampered");
    tampered["data"]["object"]["id"] = serde_json::json!("cs_payment_link_tampered");
    tampered["data"]["object"]["amount_subtotal"] = serde_json::json!(1099);
    tampered["data"]["object"]["amount_total"] = serde_json::json!(1764);
    let (msg, input) = webhook_msg(&tampered, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::Internal
        )
        .await
    );
    assert!(
        repo::purchases::find_by_session(&ctx, "cs_payment_link_tampered")
            .await
            .unwrap()
            .is_none()
    );
    let event_record = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_payment_link_tampered",
    )
    .await
    .unwrap();
    assert_eq!(event_record.data["status"], "failed");
    assert!(!event_record.str_field("next_retry_at").is_empty());

    for case in [
        "livemode",
        "mode",
        "payment_status",
        "offer_version",
        "shipping",
    ] {
        let mut tampered = event.clone();
        tampered["id"] = serde_json::json!(format!("evt_payment_link_{case}"));
        tampered["data"]["object"]["id"] = serde_json::json!(format!("cs_payment_link_{case}"));
        match case {
            "livemode" => {
                tampered["livemode"] = serde_json::json!(true);
                tampered["data"]["object"]["livemode"] = serde_json::json!(true);
            }
            "mode" => tampered["data"]["object"]["mode"] = serde_json::json!("subscription"),
            "payment_status" => {
                tampered["data"]["object"]["payment_status"] = serde_json::json!("unpaid")
            }
            "offer_version" => {
                tampered["data"]["object"]["metadata"]["offer_version"] = serde_json::json!("2")
            }
            "shipping" => {
                tampered["data"]["object"]["total_details"]["amount_shipping"] =
                    serde_json::json!(400);
                tampered["data"]["object"]["amount_total"] = serde_json::json!(1665);
            }
            _ => unreachable!(),
        }
        let (msg, input) = webhook_msg(&tampered, WEBHOOK_SECRET);
        assert!(
            output_is_error(
                stripe::handle_webhook(&ctx, &msg, input).await,
                ErrorCode::Internal,
            )
            .await,
            "Payment Link {case} mismatch must fail closed"
        );
        assert!(
            repo::purchases::find_by_session(&ctx, &format!("cs_payment_link_{case}"))
                .await
                .unwrap()
                .is_none()
        );
    }
}

/// Soft delete touches nothing in Stripe, so a deleted product's Payment
/// Links stay live in the connected account and stay payable. Reconciliation
/// used to read the product through the live-only `repo::products::get`, so
/// the delivery for a session paid through such a link answered `NotFound`,
/// `fail_webhook!` turned that into an error, and Stripe retried it forever:
/// money captured, no purchase row, no line items, and a buyer's order-status
/// page that never resolves. The reconciliation reads past the filter for the
/// same reason `archive_offer_catalog` does — its subject is a payment that
/// has already happened, and the row is read for the buyer's own receipt.
#[tokio::test]
async fn a_paid_link_for_a_soft_deleted_product_still_reconciles_into_an_order() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    let product_id = "product_deleted_but_payable";
    seed(
        &ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([
            ("name".to_string(), serde_json::json!("Field guide")),
            ("slug".to_string(), serde_json::json!(product_id)),
            ("status".to_string(), serde_json::json!("active")),
            ("approval_status".to_string(), serde_json::json!("approved")),
            ("owner_kind".to_string(), serde_json::json!("platform")),
        ]),
    )
    .await;
    let definition: OfferDefinitionRequest = serde_json::from_value(serde_json::json!({
        "name": "One-off purchase",
        "mode": "payment",
        "currency": "nzd",
        "pricing_model": "fixed",
        "usage_type": "licensed",
        "billing_scheme": "per_unit",
        "tax_behavior": "exclusive",
        "components": [{
            "key": "guide",
            "label": "Field guide",
            "required": true,
            "amount": {"type": "fixed", "unit_amount_minor": 4900}
        }]
    }))
    .unwrap();
    let offer = repo::offers::create(&ctx, product_id, "admin_1", &definition)
        .await
        .unwrap();
    let offer_id = offer.offer.id;
    repo::offers::publish(&ctx, product_id, &offer_id)
        .await
        .unwrap();
    let managed = repo::offers::get_managed(&ctx, &offer_id).await.unwrap();
    let preview = offer_pricing::evaluate_offer(
        &managed.offer,
        &PricingPreviewRequest {
            offer_id: offer_id.clone(),
            quantity: 1,
            inputs: Default::default(),
        },
        offer_pricing::InputScope::Management,
    )
    .unwrap();
    let pending_link =
        seed_pending_payment_link(&ctx, &offer_id, "deleted-product-link-config", &preview).await;
    let link_id = pending_link.managed.id;
    repo::payment_links::mark_synced(
        &ctx,
        &link_id,
        "plink_deleted",
        "https://buy.stripe.com/deleted",
    )
    .await
    .unwrap();

    // The product is deleted locally. Its Payment Link is untouched in
    // Stripe, so a customer can still pay through it.
    repo::products::soft_delete(&ctx, product_id)
        .await
        .expect("soft delete");

    let event = serde_json::json!({
        "id": "evt_deleted_product_link",
        "type": "checkout.session.completed",
        "livemode": false,
        "data": {
            "object": {
                "id": "cs_deleted_product_link",
                "payment_link": "plink_deleted",
                "mode": "payment",
                "payment_status": "paid",
                "metadata": {
                    "impresspress_payment_link_id": link_id,
                    "offer_id": offer_id,
                    "offer_version": "1"
                },
                "currency": "nzd",
                "amount_subtotal": 4900,
                "amount_total": 4900,
                "total_details": {
                    "amount_discount": 0,
                    "amount_tax": 0,
                    "amount_shipping": 0
                },
                "customer_details": {"email": "buyer@example.com"},
                "customer": "cus_deleted",
                "payment_intent": "pi_deleted",
                "livemode": false
            }
        }
    });

    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await;
    assert_eq!(body["received"], true, "the delivery must not fail: {body}");

    let order = repo::purchases::find_by_session(&ctx, "cs_deleted_product_link")
        .await
        .unwrap()
        .expect("the captured payment must have produced an order");
    assert_eq!(order.data["status"], "completed");
    assert_eq!(order.data["total_cents"], 4900);
    assert_eq!(order.data["buyer_email"], "buyer@example.com");

    let items = repo::purchases::list_line_items(&ctx, &order.id)
        .await
        .unwrap();
    assert_eq!(items.len(), 1, "the order must carry its line item");
    assert_eq!(
        RecordExt::str_field(&items[0], "product_id"),
        product_id,
        "the line item still points at the soft-deleted product row, which is \
         why the row has to stay"
    );

    // And the product is still deleted — reconciling a payment must not
    // resurrect a listing into the public catalog.
    assert!(repo::products::get(&ctx, product_id).await.is_err());
}

/// A buyer who cannot be checked is not a buyer who does not own it.
///
/// `user_owns_product` collapsed all three of its reads into `false` — the
/// middle one literally as `Err(_) => false` — so a database outage answered
/// "You must sign in and own the required product before purchasing this
/// item." to a signed-in buyer who already owned it. That refusal names the
/// buyer as the problem, is a 400 no client retries, and leaves no trace of
/// the outage anywhere near the storefront.
#[tokio::test]
async fn a_gated_checkout_reports_an_outage_instead_of_denying_ownership() {
    let ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;
    seed(
        &ctx,
        repo::products::TABLE,
        "prereq",
        HashMap::from([
            ("name".to_string(), serde_json::json!("Prerequisite")),
            ("status".to_string(), serde_json::json!("active")),
        ]),
    )
    .await;
    let offer_id = seed_gated_offer(&ctx, "gated", "prereq").await;

    // The positive control: with a healthy database the buyer genuinely does
    // not own `prereq`, and that answer is unchanged.
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "buyer_1",
        serde_json::json!({ "offer_id": offer_id }),
    );
    assert!(
        output_is_error(
            stripe::handle_checkout(&ctx, &msg, input).await,
            ErrorCode::InvalidArgument,
        )
        .await,
        "a buyer who really does not own the prerequisite still gets the 400"
    );

    // Now the subscription read — the first of the three — cannot answer.
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx.clone(),
        vec![("database.list", repo::subscriptions::SUBSCRIPTIONS_TABLE)],
    );
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "buyer_1",
        serde_json::json!({ "offer_id": offer_id }),
    );
    assert!(
        output_is_error(
            stripe::handle_checkout(&failing, &msg, input).await,
            ErrorCode::Internal,
        )
        .await,
        "an ownership check that could not run must report the outage, not deny the buyer"
    );

    // And the same for the line-item half, which the subscription read falls
    // through to. It is only reached once the buyer has a completed order to
    // look inside, so seed one — without it the check short-circuits on an
    // empty id list and the read under test never runs.
    seed(
        &ctx,
        repo::purchases::PURCHASES_TABLE,
        "order_probe",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("buyer_1")),
            ("buyer_user_id".to_string(), serde_json::json!("buyer_1")),
            ("status".to_string(), serde_json::json!("completed")),
            ("total_cents".to_string(), serde_json::json!(1000)),
            ("currency".to_string(), serde_json::json!("USD")),
        ]),
    )
    .await;
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx.clone(),
        vec![("database.list", "impresspress__products__line_items")],
    );
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "buyer_1",
        serde_json::json!({ "offer_id": offer_id }),
    );
    assert!(
        output_is_error(
            stripe::handle_checkout(&failing, &msg, input).await,
            ErrorCode::Internal,
        )
        .await,
        "the line-item half of the ownership check propagates too"
    );
}

/// Seed a published offer on `product_id`, which requires `requires`.
async fn seed_gated_offer(
    ctx: &crate::test_support::TestContext,
    product_id: &str,
    requires: &str,
) -> String {
    seed(
        ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([
            ("name".to_string(), serde_json::json!("Gated product")),
            ("status".to_string(), serde_json::json!("active")),
            ("requires".to_string(), serde_json::json!(requires)),
        ]),
    )
    .await;
    let definition: OfferDefinitionRequest = serde_json::from_value(serde_json::json!({
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
    let offer = repo::offers::create(ctx, product_id, "admin_1", &definition)
        .await
        .expect("create offer");
    repo::offers::publish(ctx, product_id, &offer.offer.id)
        .await
        .expect("publish offer");
    offer.offer.id
}

/// A subscription whose owner could not be looked up is not an unowned one.
///
/// `find_user_by_stripe_sub` collapsed a failed read into `None`, and the
/// `customer.subscription.updated` arm reads that as "nobody owns this":
/// the addon-total sync and the outbound `products.subscription.updated`
/// were both skipped and the delivery still answered Stripe with a success,
/// so nothing retried and the platform's view of a paying account drifted
/// silently.
#[tokio::test]
async fn a_subscription_update_whose_owner_lookup_fails_does_not_report_success() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_owner_probe",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("owner_1")),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!("sub_owner_probe"),
            ),
            ("status".to_string(), serde_json::json!("active")),
            ("plan".to_string(), serde_json::json!("pro")),
        ]),
    )
    .await;

    let event = serde_json::json!({
        "id": "evt_owner_probe",
        "type": "customer.subscription.updated",
        "livemode": false,
        "data": {"object": {
            "id": "sub_owner_probe",
            "status": "active",
            "cancel_at_period_end": false,
            "canceled_at": null
        }}
    });

    // Positive control: the same delivery succeeds against a healthy
    // database, so the assertion below cannot pass because the event was
    // malformed.
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    assert_eq!(
        crate::test_support::output_http_status(stripe::handle_webhook(&ctx, &msg, input).await)
            .await,
        200,
    );

    // A second, identical delivery under an outage on the owner lookup.
    // `stripe_events` de-duplicates by id, so this one carries its own.
    let mut retry = event.clone();
    retry["id"] = serde_json::json!("evt_owner_probe_2");
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx.clone(),
        vec![("database.list", repo::subscriptions::SUBSCRIPTIONS_TABLE)],
    )
    .after_passing(2);
    let (msg, input) = webhook_msg(&retry, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&failing, &msg, input).await,
            ErrorCode::Internal,
        )
        .await,
        "an owner lookup that could not run must make Stripe redeliver, not report success"
    );
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_owner_probe_2",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "failed");
    assert_eq!(
        event_row.data["last_error"], "subscription owner lookup failed",
        "the failure has to name the lookup, not a downstream symptom"
    );
}

// ============================================================
// Webhook signature rotation and add-on totals
// ============================================================

/// A `Stripe-Signature` header signed by several secrets at once, which is
/// what Stripe sends for the whole window a rolled secret stays live.
fn webhook_msg_signed_by(payload: &serde_json::Value, secrets: &[&str]) -> (Message, InputStream) {
    let payload_bytes = serde_json::to_vec(payload).unwrap();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut signed = format!("{timestamp}.");
    signed.push_str(&String::from_utf8_lossy(&payload_bytes));

    let mut sig_header = format!("t={timestamp}");
    for secret in secrets {
        let sig = primitives::hmac_sha256(secret.as_bytes(), signed.as_bytes());
        sig_header.push_str(&format!(",v1={}", hex_encode(&sig)));
    }

    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "create");
    msg.set_meta("req.resource", "/b/products/webhooks");
    msg.set_meta("http.header.stripe-signature", &sig_header);
    (msg, InputStream::from_bytes(payload_bytes))
}

/// Rolling the endpoint's signing secret leaves the retired one live for up
/// to 24 hours, and Stripe signs every delivery in that window with both. The
/// order of the `v1` values is Stripe's, not the endpoint's, so reading one
/// of them rejected every delivery of the roll window whenever the configured
/// secret's signature was not the last.
#[tokio::test]
async fn a_delivery_signed_during_a_secret_roll_is_accepted_whichever_v1_comes_last() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    let retired = "whsec_the_secret_being_rolled_out";

    let event = |id: &str| {
        serde_json::json!({
            "id": id,
            "type": "charge.refunded",
            "livemode": false,
            "data": {"object": {"payment_intent": "pi_does_not_exist"}}
        })
    };

    // The configured secret signs second…
    let body = event("evt_roll_configured_last");
    let (msg, input) = webhook_msg_signed_by(&body, &[retired, WEBHOOK_SECRET]);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );

    // …and first.
    let body = event("evt_roll_configured_first");
    let (msg, input) = webhook_msg_signed_by(&body, &[WEBHOOK_SECRET, retired]);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );

    // A delivery carrying no signature from the configured secret is still
    // refused, so the two above cannot be passing because verification stopped
    // happening.
    let body = event("evt_roll_neither");
    let (msg, input) = webhook_msg_signed_by(&body, &[retired, "whsec_a_third_secret"]);
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::Unauthenticated,
        )
        .await,
        "a header with no signature from the configured secret must be rejected"
    );
}

/// Seed a platform-billing subscription row that a
/// `customer.subscription.updated` delivery can be matched against.
async fn seed_platform_subscription(
    ctx: &crate::test_support::TestContext,
    stripe_subscription_id: &str,
    user_id: &str,
    status: &str,
) {
    seed(
        ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        stripe_subscription_id,
        HashMap::from([
            ("user_id".to_string(), serde_json::json!(user_id)),
            (
                "stripe_subscription_id".to_string(),
                serde_json::json!(stripe_subscription_id),
            ),
            ("status".to_string(), serde_json::json!(status)),
            ("plan".to_string(), serde_json::json!("pro")),
            ("stripe_event_created".to_string(), serde_json::json!(100)),
            ("addon_r2_bytes".to_string(), serde_json::json!(5)),
        ]),
    )
    .await;
}

/// Which Stripe object the platform stamped the add-on metadata on.
///
/// Stripe always serialises a subscription item's own `metadata`, as `{}` when
/// it is unset, so a price-stamped add-on arrives with an empty object at item
/// level beside the populated one on the price. A fixture that omits item
/// `metadata` altogether is not a shape Stripe sends, and it hides a reader
/// that tests the item object for presence rather than for the marker.
#[derive(Clone, Copy)]
enum AddonStamp {
    Item,
    Price,
}

/// A `customer.subscription.updated` event whose single add-on item reports
/// `extra_r2_bytes` per unit at `quantity`, stamped on the object `stamp`
/// names and with the other object carrying the empty metadata Stripe sends.
fn subscription_updated_with_addon(
    event_id: &str,
    stripe_subscription_id: &str,
    status: &str,
    extra_r2_bytes: &str,
    quantity: i64,
    stamp: AddonStamp,
) -> serde_json::Value {
    let addon_metadata = serde_json::json!({
        "addon_id": "storage_pack",
        "extra_r2_bytes": extra_r2_bytes
    });
    let empty = serde_json::json!({});
    let (item_metadata, price_metadata) = match stamp {
        AddonStamp::Item => (&addon_metadata, &empty),
        AddonStamp::Price => (&empty, &addon_metadata),
    };
    serde_json::json!({
        "id": event_id,
        "type": "customer.subscription.updated",
        "created": 200,
        "livemode": false,
        "data": {"object": {
            "id": stripe_subscription_id,
            "status": status,
            "items": {"data": [{
                "quantity": quantity,
                "metadata": item_metadata,
                "price": {"id": "price_storage_pack", "metadata": price_metadata}
            }]}
        }}
    })
}

/// Both stamping conventions are read.
///
/// The platform may carry the add-on metadata on the subscription item or on
/// the price the item points at, and this block cannot see which it chose —
/// nothing here creates those items any more (see `ADDON_ITEM_MARKER`). The
/// reader looked at `item.metadata` and fell back to the price only when that
/// key was absent, which on a real payload it never is: a price-stamped add-on
/// read as `{}`, counted as nothing, and wrote zero quota to a paying
/// subscriber. Testing the marker rather than the object's presence is what
/// makes the fallback reachable.
#[tokio::test]
async fn addon_totals_are_read_from_whichever_object_carries_the_marker() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    for (index, stamp) in [AddonStamp::Item, AddonStamp::Price]
        .into_iter()
        .enumerate()
    {
        let subscription_id = format!("sub_addon_stamp_{index}");
        seed_platform_subscription(
            &ctx,
            &subscription_id,
            &format!("owner_stamp_{index}"),
            "active",
        )
        .await;

        let event = subscription_updated_with_addon(
            &format!("evt_addon_stamp_{index}"),
            &subscription_id,
            "active",
            "1024",
            3,
            stamp,
        );
        let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
        assert_eq!(
            output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
            true
        );

        let subscription = db::get(
            &ctx,
            repo::subscriptions::SUBSCRIPTIONS_TABLE,
            &subscription_id,
        )
        .await
        .unwrap();
        assert_eq!(
            subscription.data["addon_r2_bytes"],
            3072,
            "the add-on is stamped on the {} and must still be counted",
            match stamp {
                AddonStamp::Item => "item",
                AddonStamp::Price => "price",
            }
        );
    }
}

/// An item marked on neither object is the base plan and contributes nothing —
/// the marker test must not turn "no add-on here" into "read the price
/// anyway".
#[tokio::test]
async fn a_base_plan_item_contributes_no_addon_total() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed_platform_subscription(&ctx, "sub_addon_base", "owner_base", "active").await;

    let event = serde_json::json!({
        "id": "evt_addon_base",
        "type": "customer.subscription.updated",
        "created": 200,
        "livemode": false,
        "data": {"object": {
            "id": "sub_addon_base",
            "status": "active",
            "items": {"data": [{
                "quantity": 1,
                "metadata": {},
                "price": {"id": "price_pro", "lookup_key": "pro", "metadata": {
                    // No marker: a plan price may carry metadata of its own.
                    "extra_r2_bytes": "999999"
                }}
            }]}
        }}
    });
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );

    let subscription = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_addon_base",
    )
    .await
    .unwrap();
    assert_eq!(
        subscription.data["addon_r2_bytes"], 0,
        "an unmarked item must contribute nothing, so the totals write zeroes"
    );
}

/// A `customer.subscription.updated` delivery with no `status` reports
/// nothing about the lifecycle, so the platform row keeps the status it has
/// while the plan the payload does carry is applied. Written as-is, the empty
/// status ranks with the live statuses, so a newer statusless event would
/// blank an active subscription's status to `""`.
#[tokio::test]
async fn a_subscription_update_without_a_status_keeps_the_stored_status() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed_platform_subscription(&ctx, "sub_statusless", "owner_statusless", "active").await;
    seed_platform_subscription(
        &ctx,
        "sub_statusless_canceled",
        "owner_statusless_canceled",
        "canceled",
    )
    .await;

    let statusless = |event_id: &str, subscription_id: &str, created: i64, plan: &str| {
        serde_json::json!({
            "id": event_id,
            "type": "customer.subscription.updated",
            "created": created,
            "livemode": false,
            "data": {"object": {
                "id": subscription_id,
                "items": {"data": [{
                    "quantity": 1,
                    "metadata": {},
                    "price": {"id": "price_plan", "lookup_key": plan, "metadata": {}}
                }]}
            }}
        })
    };
    let deliver = |event: serde_json::Value| {
        let ctx = &ctx;
        async move {
            let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
            let answer = output_to_json(stripe::handle_webhook(ctx, &msg, input).await).await;
            assert_eq!(
                answer["received"], true,
                "{} was answered {answer}",
                event["id"]
            );
        }
    };
    let row = |subscription_id: &'static str| {
        let ctx = &ctx;
        async move {
            db::get(
                ctx,
                repo::subscriptions::SUBSCRIPTIONS_TABLE,
                subscription_id,
            )
            .await
            .unwrap()
        }
    };

    deliver(statusless(
        "evt_statusless_newer",
        "sub_statusless",
        200,
        "business",
    ))
    .await;
    let subscription = row("sub_statusless").await;
    assert_eq!(
        subscription.data["status"], "active",
        "an event without a status must not overwrite the stored one"
    );
    assert_eq!(subscription.data["plan"], "business");
    assert_eq!(subscription.data["stripe_event_created"], 200);

    // Guard (passes without the fix too): the ordering rule still refuses a
    // strictly older statusless event, so it cannot put an old plan back.
    deliver(statusless(
        "evt_statusless_older",
        "sub_statusless",
        150,
        "starter",
    ))
    .await;
    let subscription = row("sub_statusless").await;
    assert_eq!(subscription.data["plan"], "business");
    assert_eq!(subscription.data["stripe_event_created"], 200);

    // A statusless event on a terminal row restates the terminal status, so
    // it applies and reaches the compare-and-swap on a `canceled` row, whose
    // filter has to match the stored text for the plan to land.
    deliver(statusless(
        "evt_statusless_canceled",
        "sub_statusless_canceled",
        200,
        "business",
    ))
    .await;
    let subscription = row("sub_statusless_canceled").await;
    assert_eq!(subscription.data["status"], "canceled");
    assert_eq!(subscription.data["plan"], "business");
    assert_eq!(subscription.data["stripe_event_created"], 200);
}

/// `customer.subscription.deleted` stores the platform row as `canceled`. A
/// later `customer.subscription.updated` that restates `canceled` is allowed
/// by the transition rules, and its compare-and-swap filters on the parsed
/// status re-serialised. That only matches because the deletion wrote the
/// same spelling: had it written anything else, every attempt would read as
/// a concurrent change and the delivery would fail until it dead-lettered.
#[tokio::test]
async fn a_canceled_update_after_the_deletion_is_applied_not_retried() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed_platform_subscription(&ctx, "sub_deleted_then_updated", "owner_deleted", "active").await;

    let deleted = serde_json::json!({
        "id": "evt_deleted_first",
        "type": "customer.subscription.deleted",
        "created": 200,
        "livemode": false,
        "data": {"object": {
            "id": "sub_deleted_then_updated",
            "status": "canceled",
            "canceled_at": 200
        }}
    });
    let (msg, input) = webhook_msg(&deleted, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );

    // Immediate cancellation stamps both events with the same second.
    let updated = serde_json::json!({
        "id": "evt_updated_second",
        "type": "customer.subscription.updated",
        "created": 200,
        "livemode": false,
        "data": {"object": {
            "id": "sub_deleted_then_updated",
            "status": "canceled",
            "items": {"data": [{
                "quantity": 1,
                "metadata": {},
                "price": {"id": "price_plan", "lookup_key": "pro", "metadata": {}}
            }]}
        }}
    });
    let (msg, input) = webhook_msg(&updated, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true,
        "a canceled restatement of a canceled row must not fail the delivery"
    );
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_updated_second",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "processed");

    let subscription = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_deleted_then_updated",
    )
    .await
    .unwrap();
    assert_eq!(subscription.data["status"], "canceled");
    assert_eq!(subscription.data["addon_r2_bytes"], 0);

    // And the subscriber reads Stripe's spelling back from the endpoint that
    // publishes the row.
    let (msg, input) = get_msg("/b/products/subscription", "owner_deleted");
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(body["subscription"]["status"], "canceled", "{body}");
}

/// A failed invoice on a canceled row is refused by the transition rules
/// (terminal -> `past_due`), so the delivery is answered and sealed, not
/// retried into the dead-letter queue, and no grace window appears.
#[tokio::test]
async fn a_failed_invoice_on_a_canceled_row_is_refused_not_dead_lettered() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed_platform_subscription(
        &ctx,
        "sub_failed_invoice",
        "owner_failed_invoice",
        "canceled",
    )
    .await;

    let payment_failed = serde_json::json!({
        "id": "evt_failed_invoice_canceled",
        "type": "invoice.payment_failed",
        "created": 400,
        "livemode": false,
        "data": {"object": {
            "parent": {"subscription_details": {"subscription": "sub_failed_invoice"}}
        }}
    });
    let (msg, input) = webhook_msg(&payment_failed, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true
    );
    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_failed_invoice_canceled",
    )
    .await
    .unwrap();
    assert_eq!(
        event_row.data["status"], "processed",
        "a refused past-due write is not a failure to retry"
    );

    let subscription = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_failed_invoice",
    )
    .await
    .unwrap();
    assert_eq!(subscription.data["status"], "canceled");
    assert_eq!(subscription.data["stripe_event_created"], 100);
    assert!(
        subscription.data["grace_period_end"]
            .as_str()
            .unwrap_or("")
            .is_empty(),
        "a refused past-due write must not grant a fresh grace window"
    );
}

/// The add-on totals are summed from payload numbers, so an amount or a
/// quantity big enough to wrap would write a negative quota — a subscriber
/// billed for storage handed less than none. The delivery fails instead.
#[tokio::test]
async fn an_addon_total_that_would_wrap_fails_the_delivery_instead_of_being_written() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed_platform_subscription(&ctx, "sub_addon_overflow", "owner_overflow", "active").await;

    let event = subscription_updated_with_addon(
        "evt_addon_overflow",
        "sub_addon_overflow",
        "active",
        &i64::MAX.to_string(),
        2,
        AddonStamp::Item,
    );
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&ctx, &msg, input).await,
            ErrorCode::Internal,
        )
        .await,
        "an add-on total that cannot be represented must not be answered with a success"
    );

    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_addon_overflow",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "failed");
    assert_eq!(
        event_row.data["last_error"], "add-on total synchronization failed",
        "the failure has to name the sync, not a downstream symptom"
    );

    let subscription = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_addon_overflow",
    )
    .await
    .unwrap();
    assert_eq!(
        subscription.data["addon_r2_bytes"], 5,
        "the stored total must be left alone, not replaced by a wrapped one"
    );
}

/// The write that records the totals is the point of the arm. Failing it used
/// to be logged and nothing else, so the arm ran on to `mark_event_processed`
/// and Stripe was told the delivery had succeeded — the subscriber kept paying
/// for add-ons no row recorded, and nothing retried.
#[tokio::test]
async fn an_addon_total_write_that_fails_does_not_report_success() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    seed_platform_subscription(&ctx, "sub_addon_write", "owner_write", "active").await;

    let event = subscription_updated_with_addon(
        "evt_addon_write",
        "sub_addon_write",
        "active",
        "1024",
        3,
        AddonStamp::Item,
    );

    // The status/plan write lands first on the same table; only the add-on
    // total write after it is failed.
    let failing = crate::test_support::FailingDbOpContext::new(
        ctx.clone(),
        vec![(
            "database.update_where_count",
            repo::subscriptions::SUBSCRIPTIONS_TABLE,
        )],
    )
    .after_passing(1);
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    assert!(
        output_is_error(
            stripe::handle_webhook(&failing, &msg, input).await,
            ErrorCode::Internal,
        )
        .await,
        "an add-on total write that could not run must make Stripe redeliver"
    );

    let event_row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_addon_write",
    )
    .await
    .unwrap();
    assert_eq!(event_row.data["status"], "failed");
    assert_eq!(
        event_row.data["last_error"],
        "add-on total synchronization failed"
    );
}

/// Which subscription states have their add-on totals recorded.
///
/// The columns are a projection of what Stripe reports, and Stripe reports
/// add-on items on a trialing or past-due subscription exactly as it does on
/// an active one. Writing only `active` rows lost an add-on bought during a
/// trial until the next `updated` delivery, and one bought while past due
/// until the item set next changed; which lifecycle states earn the quota is
/// the reading platform's decision, made from the `status` it is served
/// beside them.
///
/// The states that stay excluded are the terminal ones — a row that can never
/// go live again, because Stripe issues a new subscription id for a
/// resubscription. `customer.subscription.updated` can be delivered after
/// `customer.subscription.deleted`, and writing quota onto a canceled row
/// would undo the zeroing `cancel_and_reset_addons` just did.
#[tokio::test]
async fn addon_totals_reach_every_live_subscription_state_and_no_terminal_one() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    // (stored status, the status the delivery reports, the expected total).
    // A terminal row is probed with an `active` delivery, which is the
    // redelivery that would resurrect it; 5 is what `seed_platform_subscription`
    // leaves in the column, so "unchanged" is distinguishable from "zeroed".
    let cases: [(&str, &str, i64); 8] = [
        ("incomplete", "incomplete", 3072),
        ("trialing", "trialing", 3072),
        ("active", "active", 3072),
        ("past_due", "past_due", 3072),
        ("unpaid", "unpaid", 3072),
        ("paused", "paused", 3072),
        ("canceled", "active", 5),
        ("incomplete_expired", "active", 5),
    ];

    for (index, (stored, reported, expected)) in cases.into_iter().enumerate() {
        let subscription_id = format!("sub_addon_state_{index}");
        seed_platform_subscription(
            &ctx,
            &subscription_id,
            &format!("owner_state_{index}"),
            stored,
        )
        .await;

        let event = subscription_updated_with_addon(
            &format!("evt_addon_state_{index}"),
            &subscription_id,
            reported,
            "1024",
            3,
            AddonStamp::Item,
        );
        let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
        assert_eq!(
            output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
            true,
            "the {stored} delivery has to be acknowledged"
        );

        let subscription = db::get(
            &ctx,
            repo::subscriptions::SUBSCRIPTIONS_TABLE,
            &subscription_id,
        )
        .await
        .unwrap();
        assert_eq!(
            subscription.data["addon_r2_bytes"], expected,
            "a {stored} subscription's add-on total"
        );
    }
}

/// The totals write carries the same ordering predicate every other write to
/// this table carries. A delivery that failed and is retried after a newer one
/// has landed must not put the older payload's totals back: `update_status_plan`
/// refuses the stale status and answers `Ok(0)`, and the arm runs on to the
/// add-on sync regardless, so without the predicate the stale totals were
/// written over the current ones.
#[tokio::test]
async fn a_stale_redelivery_does_not_overwrite_newer_addon_totals() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    // `seed_platform_subscription` stamps `stripe_event_created` at 100 and
    // `subscription_updated_with_addon` builds events created at 200, so this
    // row is a subscription whose newest applied event is later than both.
    seed_platform_subscription(&ctx, "sub_addon_stale", "owner_stale", "active").await;
    db::update(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_addon_stale",
        HashMap::from([
            ("stripe_event_created".to_string(), serde_json::json!(500)),
            ("addon_r2_bytes".to_string(), serde_json::json!(9000)),
        ]),
    )
    .await
    .expect("advance the row past the stale event");

    let event = subscription_updated_with_addon(
        "evt_addon_stale",
        "sub_addon_stale",
        "active",
        "1024",
        3,
        AddonStamp::Item,
    );
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    assert_eq!(
        output_to_json(stripe::handle_webhook(&ctx, &msg, input).await).await["received"],
        true,
        "a stale delivery is still acknowledged — it is applied to nothing, not failed"
    );

    let subscription = db::get(
        &ctx,
        repo::subscriptions::SUBSCRIPTIONS_TABLE,
        "sub_addon_stale",
    )
    .await
    .unwrap();
    assert_eq!(
        subscription.data["addon_r2_bytes"], 9000,
        "an event older than the row must not write its totals"
    );
    assert_eq!(
        subscription.data["stripe_event_created"], 500,
        "the totals write must not move the column `update_status_plan` compare-and-swaps on"
    );
}

/// The totals are quotas, so a negative one is not a smaller number — it is
/// less capacity than none. Neither a negative per-unit amount nor a negative
/// quantity may reach the column.
#[tokio::test]
async fn a_negative_addon_amount_or_quantity_fails_the_delivery() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    for (index, (amount, quantity)) in [("-1024", 3), ("1024", -3)].into_iter().enumerate() {
        let subscription_id = format!("sub_addon_negative_{index}");
        seed_platform_subscription(
            &ctx,
            &subscription_id,
            &format!("owner_negative_{index}"),
            "active",
        )
        .await;

        let event_id = format!("evt_addon_negative_{index}");
        let event = subscription_updated_with_addon(
            &event_id,
            &subscription_id,
            "active",
            amount,
            quantity,
            AddonStamp::Item,
        );
        let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
        assert!(
            output_is_error(
                stripe::handle_webhook(&ctx, &msg, input).await,
                ErrorCode::Internal,
            )
            .await,
            "a negative add-on total must not be answered with a success \
             (amount {amount}, quantity {quantity})"
        );

        let event_row = db::get(&ctx, "impresspress__products__stripe_events", &event_id)
            .await
            .unwrap();
        assert_eq!(event_row.data["status"], "failed");
        assert_eq!(
            event_row.data["last_error"],
            "add-on total synchronization failed"
        );

        let subscription = db::get(
            &ctx,
            repo::subscriptions::SUBSCRIPTIONS_TABLE,
            &subscription_id,
        )
        .await
        .unwrap();
        assert_eq!(
            subscription.data["addon_r2_bytes"], 5,
            "the stored total must be left alone"
        );
    }
}

// ============================================================
// The platform application fee: one fee, charged and shown alike
// ============================================================

/// A Stripe stand-in for a seller's whole selling life: Connect onboarding,
/// the account refresh the seller API makes, a Checkout Session and a
/// Payment Link. Each answer is picked by method and path, so the order the
/// handlers call in is theirs to choose.
#[derive(Clone, Default)]
struct SellerLifecycleStripe {
    requests: Arc<Mutex<Vec<Request>>>,
    links_created: Arc<Mutex<usize>>,
}

const LIFECYCLE_ACCOUNT: &str = "acct_fee_lifecycle";

fn lifecycle_account(active: bool) -> serde_json::Value {
    serde_json::json!({
        "id": LIFECYCLE_ACCOUNT,
        "object": "account",
        "country": "NZ",
        "default_currency": "nzd",
        "details_submitted": active,
        "charges_enabled": active,
        "payouts_enabled": active,
        "controller": {"stripe_dashboard": {"type": "express"}},
        "requirements": {"currently_due": []}
    })
}

#[async_trait]
impl NetworkService for SellerLifecycleStripe {
    async fn do_request(&self, request: &Request) -> Result<Response, NetworkError> {
        self.requests.lock().unwrap().push(request.clone());
        let path = request
            .url
            .strip_prefix("https://api.stripe.com")
            .unwrap_or(&request.url);
        let body = match (request.method.as_str(), path) {
            ("POST", "/v1/accounts") => lifecycle_account(false),
            ("GET", p) if p == format!("/v1/accounts/{LIFECYCLE_ACCOUNT}") => {
                lifecycle_account(true)
            }
            ("POST", "/v1/account_links") => serde_json::json!({
                "object": "account_link",
                "url": "https://connect.stripe.com/setup/fee-lifecycle",
                "expires_at": 1_900_000_000_i64
            }),
            ("POST", "/v1/checkout/sessions") => serde_json::json!({
                "id": "cs_fee_lifecycle",
                "url": "https://checkout.stripe.com/c/pay/cs_fee_lifecycle"
            }),
            ("POST", "/v1/payment_links") => {
                let mut created = self.links_created.lock().unwrap();
                *created += 1;
                serde_json::json!({
                    "id": format!("plink_fee_lifecycle_{created}"),
                    "url": format!("https://buy.stripe.com/fee_lifecycle_{created}")
                })
            }
            (method, path) => panic!("unexpected Stripe request {method} {path}"),
        };
        Ok(Response {
            status_code: 200,
            headers: HashMap::new(),
            body: serde_json::to_vec(&body).unwrap(),
        })
    }
}

/// The form bodies this lifecycle sent to `path`, in order.
fn lifecycle_forms(stripe: &SellerLifecycleStripe, path: &str) -> Vec<String> {
    stripe
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.url == format!("https://api.stripe.com{path}"))
        .map(|request| String::from_utf8(request.body.clone().unwrap()).unwrap())
        .collect()
}

/// Whether a Stripe form carries `application_fee_amount={fee_minor}`, or
/// no fee at all when `fee_minor` is 0 (a zero fee is omitted, not sent).
fn carries_fee(form: &str, fee_minor: i64) -> bool {
    if fee_minor == 0 {
        !form.contains("application_fee_amount")
    } else {
        form.contains(&format!("application_fee_amount={fee_minor}"))
    }
}

/// Create a seller preset for `pages` and a Payment Link for it, through the
/// seller routes. Returns the link and the preset id.
async fn create_preset_link(
    ctx: &crate::test_support::TestContext,
    base: &str,
    slug: &str,
    pages: u32,
) -> (serde_json::Value, serde_json::Value) {
    let (msg, input) = create_msg(
        &format!("{base}/presets"),
        "seller_fee",
        serde_json::json!({"name": slug, "slug": slug, "inputs": {"pages": pages}}),
    );
    let preset = output_to_json(dispatch(ctx, msg, input).await).await;
    let (msg, input) = create_msg(
        &format!("{base}/payment-links"),
        "seller_fee",
        serde_json::json!({"preset_id": preset["id"]}),
    );
    (
        output_to_json(dispatch(ctx, msg, input).await).await,
        preset["id"].clone(),
    )
}

/// What a seller is charged and shown follows the platform fee, whatever it
/// was when they onboarded — for everything created after the change.
///
/// Onboarding stamped the platform fee of that moment into the seller row and
/// nothing ever changed it. Checkout and Payment Links read a stored 0 as
/// "use the platform fee" and any other value as the seller's own, while
/// every seller page printed the stored value. So a seller onboarded at 0 bps
/// was SHOWN 0.00% and CHARGED the raised fee, and a seller onboarded at a
/// higher fee was charged that fee forever. Both directions are driven here,
/// through the real onboarding, checkout, Payment Link, seller API and page
/// routes: a new Checkout Session, a newly created Payment Link, the API and
/// every page must agree on the fee the platform sets now.
///
/// A Payment Link created BEFORE the change is reused as it is, with the fee
/// it was created with: the fee is not part of its configuration hash. That
/// is the boundary `config::seller_fee_bps` documents.
#[tokio::test]
async fn a_changed_platform_fee_is_what_every_seller_is_charged_and_shown() {
    // (fee at onboarding, fee after the change, the old link's fee on 1100
    // minor units, the new checkout's fee on 1100, the new link's fee on
    // 1200, the percentage every page prints)
    for (onboarded_at, now, old_link_fee, checkout_fee, new_link_fee, shown) in [
        ("0", "500", 0, 55, 60, "5.00%"),
        ("500", "200", 55, 22, 24, "2.00%"),
    ] {
        let mut ctx = ctx_with(&[
            ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
            ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
            ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
            ("IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY", "NZ"),
            (
                "IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS",
                onboarded_at,
            ),
        ])
        .await;
        let stripe = SellerLifecycleStripe::default();
        let block: Arc<dyn Block> = Arc::new(
            wafer_core::service_blocks::network::NetworkBlock::new(Arc::new(stripe.clone())),
        );
        ctx.register_block("wafer-run/network", block);

        let (msg, input) = create_msg(
            "/b/products/api/seller/onboarding",
            "seller_fee",
            serde_json::json!({
                "return_url": "https://shop.example/seller/stripe/return",
                "refresh_url": "https://shop.example/seller/stripe/refresh"
            }),
        );
        let onboarded = output_to_json(dispatch(&ctx, msg, input).await).await;
        assert_eq!(onboarded["account"]["stripe_account_id"], LIFECYCLE_ACCOUNT);

        // The seller API refreshes the account from Stripe, which is what
        // turns it active.
        let (msg, input) = get_msg("/b/products/api/seller/account", "seller_fee");
        let account = output_to_json(dispatch(&ctx, msg, input).await).await;
        assert_eq!(account["status"], "active", "onboarded at {onboarded_at}");

        // A Payment Link created at the onboarding fee.
        let offer_id = seed_active_offer(&ctx, "product_fee_lifecycle", "seller_fee").await;
        let base = format!("/b/products/api/products/product_fee_lifecycle/offers/{offer_id}");
        let (old_link, old_preset) = create_preset_link(&ctx, &base, "four-pages", 4).await;
        assert_eq!(old_link["url"], "https://buy.stripe.com/fee_lifecycle_1");

        // The platform changes its fee.
        ctx.set_config("IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS", now);
        let now_bps: u64 = now.parse().unwrap();

        let (msg, input) = get_msg("/b/products/api/seller/account", "seller_fee");
        let account = output_to_json(dispatch(&ctx, msg, input).await).await;
        assert_eq!(
            account["fee_basis_points"], now_bps,
            "the seller API publishes today's fee (onboarded at {onboarded_at})"
        );

        let (msg, input) = create_msg(
            "/b/products/checkout",
            "",
            serde_json::json!({"offer_id": offer_id, "inputs": {"pages": 4}}),
        );
        let checkout = output_to_json(dispatch(&ctx, msg, input).await).await;
        assert_eq!(checkout["amounts"]["total_minor"], 1100);
        assert_eq!(
            checkout["amounts"]["platform_fee_minor"], checkout_fee,
            "a new checkout charges today's fee (onboarded at {onboarded_at})"
        );
        let sessions = lifecycle_forms(&stripe, "/v1/checkout/sessions");
        assert_eq!(sessions.len(), 1);
        assert!(
            sessions[0].contains(&format!(
                "payment_intent_data[application_fee_amount]={checkout_fee}"
            )),
            "the Checkout Session carries today's fee (onboarded at {onboarded_at})"
        );

        // The link made before the change is reused, untouched.
        let (msg, input) = create_msg(
            &format!("{base}/payment-links"),
            "seller_fee",
            serde_json::json!({"preset_id": old_preset}),
        );
        let reused = output_to_json(dispatch(&ctx, msg, input).await).await;
        assert_eq!(
            reused["url"], old_link["url"],
            "the existing link is reused"
        );

        // A link created after the change carries the new fee.
        let (new_link, _) = create_preset_link(&ctx, &base, "eight-pages", 8).await;
        assert_eq!(new_link["url"], "https://buy.stripe.com/fee_lifecycle_2");

        let links = lifecycle_forms(&stripe, "/v1/payment_links");
        assert_eq!(links.len(), 2, "the reuse sends nothing to Stripe");
        assert!(
            carries_fee(&links[0], old_link_fee),
            "the pre-change link keeps its fee {old_link_fee} (onboarded at {onboarded_at})"
        );
        assert!(
            carries_fee(&links[1], new_link_fee),
            "a new Payment Link carries today's fee (onboarded at {onboarded_at})"
        );

        let local = repo::seller_accounts::get_for_user(&ctx, "seller_fee")
            .await
            .unwrap()
            .expect("seller row");
        for (path, admin) in [
            ("/b/products/".to_string(), false),
            ("/b/products/selling".to_string(), false),
            (format!("/b/products/admin/sellers/{}", local.id), true),
        ] {
            let (msg, input) = if admin {
                admin_get_msg(&path)
            } else {
                get_msg(&path, "seller_fee")
            };
            let html = output_to_html(dispatch(&ctx, msg, input).await).await;
            assert!(
                html.contains(shown),
                "{path} shows today's fee {shown} (onboarded at {onboarded_at})"
            );
        }
    }
}

/// A typo in the fee setting leaves the admin seller pages — and the suspend
/// control on the detail page — working, and shows the fee as misconfigured
/// rather than as a number nobody set.
///
/// The fee is a platform setting, so reading it could fail both pages with a
/// 500, and the detail page is where an operator suspends a seller: a config
/// typo would have switched the fraud control off in the UI.
#[tokio::test]
async fn an_unreadable_fee_setting_leaves_the_admin_seller_pages_and_suspension_working() {
    let ctx = ctx_with(&[
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
        ("IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS", "2.5%"),
    ])
    .await;
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_typo",
        HashMap::from([
            ("user_id".to_string(), serde_json::json!("user_typo")),
            ("status".to_string(), serde_json::json!("active")),
            (
                "stripe_account_id".to_string(),
                serde_json::json!("acct_typo"),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(true)),
            ("payouts_enabled".to_string(), serde_json::json!(true)),
        ]),
    )
    .await;

    let (msg, input) = admin_get_msg("/b/products/admin/sellers");
    let list = output_to_html(dispatch(&ctx, msg, input).await).await;
    assert!(list.contains("user_typo"), "the seller list renders");

    let (msg, input) = admin_get_msg("/b/products/admin/sellers/seller_typo");
    let detail = output_to_html(dispatch(&ctx, msg, input).await).await;
    assert!(
        detail.contains("data-seller-action=\"suspend\""),
        "the suspend control renders"
    );
    assert!(
        detail.contains("Misconfigured"),
        "the fee is shown as misconfigured"
    );

    // The suspension itself lands; the answer that cannot carry a fee is a
    // server fault (its logged label says the change is saved), not a 409.
    let (msg, input) = admin_create_msg(
        "/b/products/api/admin/sellers/seller_typo/suspend",
        serde_json::json!({}),
    );
    assert_eq!(
        crate::test_support::output_http_status(dispatch(&ctx, msg, input).await).await,
        500,
        "an unreadable fee setting is a server fault, not a 409 inviting a retry"
    );
    let row = db::get(&ctx, repo::seller_accounts::TABLE, "seller_typo")
        .await
        .unwrap();
    assert_eq!(row.data["status"], "suspended", "the suspension is saved");
}

/// Checkout's seller lookup answers 400 only when the seller genuinely is not
/// ready; a fault reading the seller is a 500.
///
/// Every `ready_for_user` error became 400 "this seller's Stripe account is
/// not ready", so a database outage — or a seller row the block cannot
/// decode — told the buyer the seller had not finished onboarding, and
/// nothing reached the logs.
#[tokio::test]
async fn checkout_separates_a_seller_that_is_not_ready_from_a_failed_read() {
    use crate::test_support::{output_http_status, FailingDbOpContext};

    let ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        ("WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "true"),
    ])
    .await;
    let seller = |status: &str| {
        HashMap::from([
            (
                "user_id".to_string(),
                serde_json::json!(format!("seller_{status}")),
            ),
            ("status".to_string(), serde_json::json!(status)),
            (
                "stripe_account_id".to_string(),
                serde_json::json!(format!("acct_{status}")),
            ),
            ("details_submitted".to_string(), serde_json::json!(true)),
            ("charges_enabled".to_string(), serde_json::json!(false)),
        ])
    };
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_restricted",
        seller("restricted"),
    )
    .await;
    // Not a `SellerStatus` spelling: the row exists and cannot be decoded.
    seed(
        &ctx,
        repo::seller_accounts::TABLE,
        "seller_dormant",
        seller("dormant"),
    )
    .await;
    let restricted = seed_active_offer(&ctx, "product_restricted", "seller_restricted").await;
    let dormant = seed_active_offer(&ctx, "product_dormant", "seller_dormant").await;
    let checkout = |offer_id: &str| {
        create_msg(
            "/b/products/checkout",
            "",
            serde_json::json!({"offer_id": offer_id, "inputs": {"pages": 4}}),
        )
    };

    // Guard: a seller that cannot take charges yet is still the buyer's 400
    // (passes before and after the fix).
    let (msg, input) = checkout(&restricted);
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        400
    );

    let (msg, input) = checkout(&dormant);
    assert_eq!(
        output_http_status(dispatch(&ctx, msg, input).await).await,
        500,
        "an undecodable seller row is a server fault, not an unready seller"
    );

    let outage = FailingDbOpContext::new(
        ctx.clone(),
        vec![("database.list", repo::seller_accounts::TABLE)],
    );
    let (msg, input) = checkout(&restricted);
    assert_eq!(
        output_http_status(dispatch(&outage, msg, input).await).await,
        500,
        "a seller read that failed is a server fault, not an unready seller"
    );
}

// ============================================================
// Error mapping — the webhook dispatcher and the offer checkout
// ============================================================

/// A `refund.updated` delivery whose first read inside the dispatcher — the
/// refund-ledger lookup — answers `code`, and the status the delivery gets.
///
/// The lookup is a `database.list` on the refunds table, which nothing before
/// it in `handle_webhook` touches: the lease claim is on the events table. So
/// the injected refusal lands on a dispatcher site, and the lease is already
/// held when it does.
async fn refund_webhook_status_when_the_ledger_read_answers(
    code: ErrorCode,
    event_id: &str,
) -> (crate::test_support::TestContext, serde_json::Value, u16) {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    let event = serde_json::json!({
        "id": event_id,
        "type": "refund.updated",
        "livemode": false,
        "data": {"object": {
            "id": format!("re_{event_id}"),
            "status": "succeeded",
            "livemode": false
        }}
    });
    let failing = crate::test_support::FailingDbOpContext::failing_with(
        ctx.clone(),
        vec![("database.list", repo::refunds::TABLE)],
        wafer_run::WaferError::new(code, "refused by the database client"),
    );
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let status = crate::test_support::output_http_status(
        stripe::handle_webhook(&failing, &msg, input).await,
    )
    .await;
    (ctx, event, status)
}

/// The lease the refused delivery held was released as a scheduled retry, and
/// the next delivery after the backoff processes the event. A 4xx does not
/// stop Stripe redelivering — every non-2xx is a failed delivery — and this
/// is the local half: the refusal did not seal or strand the event.
async fn assert_refused_delivery_is_retried(
    ctx: &crate::test_support::TestContext,
    event: &serde_json::Value,
    event_id: &str,
) {
    let row = db::get(ctx, "impresspress__products__stripe_events", event_id)
        .await
        .unwrap();
    assert_eq!(row.data["status"], "failed");
    assert!(!row.str_field("next_retry_at").is_empty());

    db::update(
        ctx,
        "impresspress__products__stripe_events",
        event_id,
        HashMap::from([(
            "next_retry_at".to_string(),
            serde_json::json!("2000-01-01T00:00:00Z"),
        )]),
    )
    .await
    .unwrap();
    let (msg, input) = webhook_msg(event, WEBHOOK_SECRET);
    let body = output_to_json(stripe::handle_webhook(ctx, &msg, input).await).await;
    assert_eq!(body["received"], true);
    let row = db::get(ctx, "impresspress__products__stripe_events", event_id)
        .await
        .unwrap();
    assert_eq!(row.data["status"], "processed");
}

#[tokio::test]
async fn webhook_database_denial_is_403_and_the_delivery_is_retried() {
    let (ctx, event, status) = refund_webhook_status_when_the_ledger_read_answers(
        ErrorCode::PermissionDenied,
        "evt_ledger_denied",
    )
    .await;
    assert_eq!(status, 403, "a WRAP denial inside the dispatcher is a 403");
    assert_refused_delivery_is_retried(&ctx, &event, "evt_ledger_denied").await;
}

#[tokio::test]
async fn webhook_database_quota_is_429_and_the_delivery_is_retried() {
    let (ctx, event, status) = refund_webhook_status_when_the_ledger_read_answers(
        ErrorCode::ResourceExhausted,
        "evt_ledger_quota",
    )
    .await;
    assert_eq!(
        status, 429,
        "a database quota inside the dispatcher is a 429"
    );
    assert_refused_delivery_is_retried(&ctx, &event, "evt_ledger_quota").await;
}

/// Guard (passes before and after the database tails were classified): a
/// Stripe rate limit is Stripe's, not the database's, so the checkout it
/// interrupts is the sanitized 500 — never the 429 a database quota earns.
#[tokio::test]
async fn checkout_stripe_rate_limit_stays_500() {
    let mut ctx = ctx_with(&[
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
        ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
        ("IMPRESSPRESS__PRODUCTS__STRIPE_ACCOUNT_COUNTRY", "NZ"),
    ])
    .await;
    let requests = register_stripe_sequence(
        &mut ctx,
        vec![(429, serde_json::json!({"error": {"code": "rate_limit"}}))],
    );
    let offer_id = seed_active_offer(&ctx, "product_checkout_rate_limit", "").await;
    let (msg, input) = create_msg(
        "/b/products/checkout",
        "",
        serde_json::json!({
            "offer_id": offer_id,
            "quantity": 1,
            "inputs": {"pages": 3},
            "presentation": "hosted"
        }),
    );
    assert_eq!(
        crate::test_support::output_http_status(stripe::handle_checkout(&ctx, &msg, input).await)
            .await,
        500
    );
    let requests = requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        1,
        "the Stripe call is the failure under test"
    );
    assert!(requests[0].url.ends_with("/v1/checkout/sessions"));
}

/// A checkout whose order insert is refused by the database is that refusal's
/// status, not a 500. The insert is the first write to the purchases table, so
/// nothing has reached Stripe yet.
#[tokio::test]
async fn checkout_order_insert_denial_is_403_and_quota_is_429() {
    for (code, status) in [
        (ErrorCode::PermissionDenied, 403),
        (ErrorCode::ResourceExhausted, 429),
    ] {
        let mut ctx = ctx_with(&[
            ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x"),
            ("WAFER_RUN_SHARED__FRONTEND_URL", "https://shop.example"),
            ("IMPRESSPRESS__PRODUCTS__STRIPE_ACCOUNT_COUNTRY", "NZ"),
        ])
        .await;
        let requests = register_stripe_sequence(&mut ctx, Vec::new());
        let offer_id = seed_active_offer(&ctx, "product_checkout_denied", "").await;
        let failing = crate::test_support::FailingDbOpContext::failing_with(
            ctx.clone(),
            vec![("database.create", repo::purchases::PURCHASES_TABLE)],
            wafer_run::WaferError::new(code, "refused by the database client"),
        );
        let (msg, input) = create_msg(
            "/b/products/checkout",
            "",
            serde_json::json!({
                "offer_id": offer_id,
                "quantity": 1,
                "inputs": {"pages": 3},
                "presentation": "hosted"
            }),
        );
        assert_eq!(
            crate::test_support::output_http_status(
                stripe::handle_checkout(&failing, &msg, input).await
            )
            .await,
            status,
            "{code:?}"
        );
        assert!(requests.lock().unwrap().is_empty());
    }
}

/// A Payment Link completion from a connected account other than the link's
/// is an integrity mismatch, answered with the same 500 every other webhook
/// identity mismatch gets — not the 403 the database door gives a WRAP
/// denial, which is what it would be reported as if the mismatch carried
/// `PermissionDenied`.
#[tokio::test]
async fn payment_link_account_mismatch_is_500_not_a_wrap_denial() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    let product_id = "product_payment_link_foreign";
    seed(
        &ctx,
        repo::products::TABLE,
        product_id,
        HashMap::from([
            ("name".to_string(), serde_json::json!("Care plan")),
            ("slug".to_string(), serde_json::json!(product_id)),
            ("status".to_string(), serde_json::json!("active")),
            ("approval_status".to_string(), serde_json::json!("approved")),
            ("owner_kind".to_string(), serde_json::json!("platform")),
        ]),
    )
    .await;
    let definition: OfferDefinitionRequest = serde_json::from_value(serde_json::json!({
        "name": "Monthly subscription",
        "mode": "subscription",
        "currency": "nzd",
        "pricing_model": "fixed",
        "recurring_interval": "month",
        "interval_count": 1,
        "usage_type": "licensed",
        "billing_scheme": "per_unit",
        "tax_behavior": "exclusive",
        "components": [{
            "key": "plan",
            "label": "Care plan",
            "required": true,
            "amount": {"type": "fixed", "unit_amount_minor": 4900}
        }]
    }))
    .unwrap();
    let offer_id = repo::offers::create(&ctx, product_id, "admin_1", &definition)
        .await
        .unwrap()
        .offer
        .id;
    repo::offers::publish(&ctx, product_id, &offer_id)
        .await
        .unwrap();
    let managed = repo::offers::get_managed(&ctx, &offer_id).await.unwrap();
    let preview = offer_pricing::evaluate_offer(
        &managed.offer,
        &PricingPreviewRequest {
            offer_id: offer_id.clone(),
            quantity: 1,
            inputs: Default::default(),
        },
        offer_pricing::InputScope::Management,
    )
    .unwrap();
    let link_id = seed_pending_payment_link(&ctx, &offer_id, "foreign-link-config", &preview)
        .await
        .managed
        .id;

    let event = serde_json::json!({
        "id": "evt_payment_link_foreign",
        "type": "checkout.session.completed",
        "account": "acct_attacker",
        "livemode": false,
        "data": {"object": {
            "id": "cs_payment_link_foreign",
            "mode": "subscription",
            "payment_status": "paid",
            "metadata": {
                "impresspress_payment_link_id": link_id,
                "offer_id": offer_id,
                "offer_version": "1"
            },
            "currency": "nzd",
            "amount_total": 4900,
            "livemode": false
        }}
    });
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    assert_eq!(
        crate::test_support::output_http_status(stripe::handle_webhook(&ctx, &msg, input).await)
            .await,
        500
    );
    assert!(
        repo::purchases::find_by_session(&ctx, "cs_payment_link_foreign")
            .await
            .unwrap()
            .is_none(),
        "a mismatched delivery must not create an order"
    );
}

/// Seed a `charge.refunded` event (for an unknown PaymentIntent, so
/// processing it is a no-op) whose last attempt died holding the processing
/// lease: `processing`, the whole budget spent, the lease long lapsed.
/// Returns the event `webhook_msg` redelivers — the stored hash is of the
/// exact bytes it sends, so the redelivery and a replay both match.
async fn seed_event_out_of_attempts(
    ctx: &crate::test_support::TestContext,
    id: &str,
) -> serde_json::Value {
    use base64ct::{Base64, Encoding};

    let event = serde_json::json!({
        "id": id,
        "type": "charge.refunded",
        "livemode": false,
        "data": { "object": { "payment_intent": "pi_lapsed_unknown", "livemode": false } }
    });
    let payload = serde_json::to_vec(&event).unwrap();
    seed(
        ctx,
        "impresspress__products__stripe_events",
        id,
        HashMap::from([
            (
                "event_type".to_string(),
                serde_json::json!("charge.refunded"),
            ),
            ("status".to_string(), serde_json::json!("processing")),
            (
                "attempts".to_string(),
                serde_json::json!(repo::MAX_ATTEMPTS),
            ),
            (
                "processing_owner".to_string(),
                serde_json::json!("crashed-worker"),
            ),
            (
                "processing_started_at".to_string(),
                serde_json::json!(
                    (chrono::Utc::now() - chrono::Duration::seconds(3600)).to_rfc3339()
                ),
            ),
            (
                "payload_sha256".to_string(),
                serde_json::json!(sha256_hex(&payload)),
            ),
            (
                "payload_base64".to_string(),
                serde_json::json!(Base64::encode_string(&payload)),
            ),
            (
                "last_error".to_string(),
                serde_json::json!("refund ledger unavailable"),
            ),
        ]),
    )
    .await;
    event
}

/// An event whose last attempt died holding the processing lease has no
/// outcome recorded. When a redelivery finds the budget spent, the webhook
/// acknowledges it — Stripe then stops redelivering — so the row must be
/// `dead_letter` with its reason by then: a row left `processing` can never
/// be replayed from the admin queue.
#[tokio::test]
async fn an_event_out_of_attempts_on_a_lapsed_lease_is_dead_lettered_and_replayable() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    let event = seed_event_out_of_attempts(&ctx, "evt_lapsed_last_attempt").await;

    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);
    let body = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(
        body,
        serde_json::json!({ "received": true, "dead_letter": true })
    );

    let row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_lapsed_last_attempt",
    )
    .await
    .unwrap();
    assert_eq!(
        row.data["status"], "dead_letter",
        "an acknowledged event must not be left processing"
    );
    let reason = row.str_field("last_error");
    assert!(
        reason.contains("retry budget") && reason.contains("expired"),
        "the reason must say the budget ran out on a lapsed lease: {reason:?}"
    );
    assert!(
        reason.contains("refund ledger unavailable"),
        "the earlier attempts' error must survive: {reason:?}"
    );
    assert!(!row.str_field("terminal_at").is_empty());
    assert_eq!(row.str_field("processing_owner"), "");

    let (replay, input) = admin_create_msg(
        "/b/products/api/admin/webhook-events/evt_lapsed_last_attempt/replay",
        serde_json::json!({}),
    );
    let replayed = output_to_json(dispatch(&ctx, replay, input).await).await;
    assert_eq!(replayed["received"], true, "replay refused: {replayed}");
    let row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_lapsed_last_attempt",
    )
    .await
    .unwrap();
    assert_eq!(row.data["status"], "processed");
}

/// Two redeliveries of an out-of-budget event both read the row before
/// either writes. Only the one whose dead-letter write still matches the row
/// it read acknowledges; the other finds the row moved under it and must ask
/// Stripe to retry (500) rather than acknowledge an outcome it did not
/// record — and must leave the winner's row as the winner wrote it.
#[tokio::test]
async fn a_redelivery_that_loses_the_dead_letter_race_is_retried_not_acknowledged() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;
    let event = seed_event_out_of_attempts(&ctx, "evt_dead_letter_race").await;
    // Each delivery's one `get` of the row is held until both have made it,
    // so both read owner `crashed-worker` before either writes.
    let racing = crate::test_support::RendezvousDbOpContext::new(
        ctx.clone(),
        "database.get",
        "impresspress__products__stripe_events",
        2,
    );
    let (first, first_input) = webhook_msg(&event, WEBHOOK_SECRET);
    let (second, second_input) = webhook_msg(&event, WEBHOOK_SECRET);
    let (left, right) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(
            dispatch(&racing, first, first_input),
            dispatch(&racing, second, second_input),
        )
    })
    .await
    .expect("both deliveries must pass the rendezvous");
    let mut statuses = vec![
        crate::test_support::output_http_status(left).await,
        crate::test_support::output_http_status(right).await,
    ];
    statuses.sort_unstable();
    assert_eq!(
        statuses,
        vec![200, 500],
        "exactly one delivery acknowledges; the loser is retried"
    );

    let row = db::get(
        &ctx,
        "impresspress__products__stripe_events",
        "evt_dead_letter_race",
    )
    .await
    .unwrap();
    assert_eq!(row.data["status"], "dead_letter");
    assert_eq!(row.str_field("processing_owner"), "");
    assert!(row.str_field("last_error").contains("retry budget"));
}
