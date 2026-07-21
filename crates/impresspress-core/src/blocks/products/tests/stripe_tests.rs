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
        contracts::{OfferDefinitionRequest, PaymentLinkCreateRequest, PricingPreviewRequest},
        offer_pricing, repo, stripe, PRODUCTS_TABLE,
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
        PRODUCTS_TABLE,
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
    assert_eq!(platform.data["status"], "cancelled");
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
    assert_eq!(platform.data["status"], "cancelled");
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
    assert_eq!(platform.data["status"], "cancelled");
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
    assert_eq!(subscription.data["status"], "cancelled");
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

#[tokio::test]
async fn webhook_unhandled_event_returns_ok() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    let event = serde_json::json!({
        "type": "payment_intent.created",
        "data": { "object": {} }
    });
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);

    let out = stripe::handle_webhook(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["received"], true);
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
    assert!(output_is_error(out, ErrorCode::Internal).await);
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
    assert!(output_is_error(out, ErrorCode::Internal).await);
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
    assert_eq!(synced.sync_status, "synced");
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
    let product = db::get(&ctx, PRODUCTS_TABLE, product_id)
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
    assert_eq!(failed.sync_status, "failed");
    assert!(failed.sync_error.contains("immutable offer row"));
    assert!(failed
        .offer
        .components
        .iter()
        .all(|component| component.stripe_price_id.is_empty()));
    let persisted_product = db::get(&ctx, PRODUCTS_TABLE, product_id).await.unwrap();
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
    assert_eq!(retried.sync_status, "synced");
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
        PRODUCTS_TABLE,
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
    assert_eq!(reconciled.sync_status, "synced");

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
        PRODUCTS_TABLE,
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
    assert_eq!(synced.sync_status, "synced");
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
        PRODUCTS_TABLE,
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
        PRODUCTS_TABLE,
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
    let pending_link = repo::payment_links::create_pending(
        &ctx,
        &offer_id,
        "",
        "",
        "",
        false,
        "archive-link-config",
        &preview,
        0,
    )
    .await
    .unwrap();
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
    let path = format!("/admin/b/products/products/{product_id}/offers/{offer_id}");
    let (msg, input) = delete_msg(&path, "admin_1");
    assert!(
        output_is_error(
            dispatch_admin(&ctx, msg, input).await,
            ErrorCode::AlreadyExists
        )
        .await
    );
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
    let archived = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    assert_eq!(archived["status"], "archived");
    assert_eq!(retry_requests.lock().unwrap().len(), 2);

    let idempotent_requests = register_stripe_sequence(&mut ctx, vec![]);
    let (msg, input) = delete_msg(&path, "admin_1");
    let archived_again = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    assert_eq!(archived_again["status"], "archived");
    assert!(idempotent_requests.lock().unwrap().is_empty());
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
        PRODUCTS_TABLE,
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
    let path = "/admin/b/products/sellers/seller_suspend_account/suspend";
    let (msg, input) = admin_create_msg(path, serde_json::json!({}));
    assert!(
        output_is_error(
            dispatch_admin(&ctx, msg, input).await,
            ErrorCode::AlreadyExists,
        )
        .await
    );
    assert_eq!(
        db::get(&ctx, repo::seller_accounts::TABLE, "seller_suspend_account")
            .await
            .unwrap()
            .str_field("status"),
        "active"
    );
    assert_eq!(
        db::get(&ctx, PRODUCTS_TABLE, product_id)
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
    let suspended = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    assert_eq!(suspended["status"], "suspended");
    assert_eq!(
        db::get(&ctx, PRODUCTS_TABLE, product_id)
            .await
            .unwrap()
            .str_field("status"),
        "archived"
    );
    assert_eq!(retry_requests.lock().unwrap().len(), 2);
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
    let base = format!("/admin/b/products/products/product_payment_link/offers/{offer_id}");

    let (msg, input) = admin_create_msg(
        &format!("{base}/presets"),
        serde_json::json!({
            "name": "Four page flyer",
            "slug": "four-page-flyer",
            "inputs": {"pages": 4}
        }),
    );
    let preset = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    let preset_id = preset["id"].as_str().expect("preset id").to_string();
    assert_eq!(preset["inputs"]["pages"], 4);
    assert_eq!(preset["active"], true);
    assert_eq!(preset["configuration_hash"].as_str().unwrap().len(), 64);

    let create_body = serde_json::json!({
        "preset_id": preset_id,
        "after_completion_url": "https://shop.example/payment-link/thanks?session_id={CHECKOUT_SESSION_ID}"
    });
    let (msg, input) = admin_create_msg(&format!("{base}/payment-links"), create_body.clone());
    let link = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
    let link_id = link["id"]
        .as_str()
        .expect("local Payment Link id")
        .to_string();
    assert_eq!(link["url"], "https://buy.stripe.com/test_print");
    assert_eq!(link["sync_status"], "synced");
    assert!(link.get("stripe_payment_link_id").is_none());

    // Same immutable configuration reuses the existing provider resource.
    let (msg, input) = admin_create_msg(&format!("{base}/payment-links"), create_body);
    let reused = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
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
    let storefront = output_to_json(dispatch_user(&ctx, msg, input).await).await;
    let public_link = &storefront["offers"][0]["payment_links"][0];
    assert_eq!(public_link["id"], link_id);
    assert_eq!(public_link["preset_id"], preset_id);
    assert_eq!(public_link["url"], "https://buy.stripe.com/test_print");
    assert!(public_link.get("configuration_hash").is_none());
    assert!(public_link.get("sync_status").is_none());

    let (msg, input) = delete_msg(&format!("{base}/payment-links/{link_id}"), "admin_1");
    let deactivated = output_to_json(dispatch_admin(&ctx, msg, input).await).await;
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
    let storefront = output_to_json(dispatch_user(&ctx, msg, input).await).await;
    assert_eq!(
        storefront["offers"][0]["payment_links"],
        serde_json::json!([])
    );
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
        PRODUCTS_TABLE,
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

    let product = db::get(&ctx, PRODUCTS_TABLE, "product_total_policy")
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
        PRODUCTS_TABLE,
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
    let pending_link = repo::payment_links::create_pending(
        &ctx,
        &offer_id,
        "",
        "",
        "",
        false,
        "resume-link-config",
        &preview,
        0,
    )
    .await
    .unwrap();
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
    let product = db::get(&ctx, PRODUCTS_TABLE, "product_payment_link_webhook")
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
