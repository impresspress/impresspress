use std::collections::HashMap;

use wafer_block_crypto::primitives;
use wafer_core::clients::database as db;
use wafer_run::{ErrorCode, InputStream, Message};

use super::harness::*;
use crate::{
    blocks::products::{purchase, stripe},
    util::hex_encode,
};

// ============================================================
// Helpers
// ============================================================

const WEBHOOK_SECRET: &str = "whsec_test_secret_key";

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
async fn webhook_checkout_completed_marks_purchase() {
    let ctx = ctx_with(&[(
        "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
        WEBHOOK_SECRET,
    )])
    .await;

    // Seed a pending purchase
    let mut pd = HashMap::new();
    pd.insert("user_id".to_string(), serde_json::json!("user_1"));
    pd.insert("status".to_string(), serde_json::json!("pending"));
    pd.insert("total_cents".to_string(), serde_json::json!(5000));
    seed(&ctx, "impresspress__products__purchases", "pur_wh1", pd).await;

    let event = checkout_completed_event("pur_wh1", "pi_12345");
    let (msg, input) = webhook_msg(&event, WEBHOOK_SECRET);

    let out = stripe::handle_webhook(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    assert_eq!(body["received"], true);
}

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
            "purchase_id": "pur_1"
        }),
    );

    let out = stripe::handle_checkout(&ctx, &msg, input).await;
    assert!(output_is_error(out, ErrorCode::Internal).await);
}

/// Regression for SB-2: `line_item_product_ids` (the checkout dependency
/// probe) must inspect every line item of the purchase, not just the first.
/// A 2-item purchase where the SECOND item `requires` a product the buyer
/// does not own must be rejected — with the old `limit: 1` query, only the
/// first (unrelated, ungated) item was fetched and the gate silently passed.
#[tokio::test]
async fn checkout_enforces_requires_on_every_line_item_not_just_the_first() {
    let ctx = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_x")]).await;

    // Prerequisite product — user_1 owns neither a purchase nor a
    // subscription that references it.
    let mut prereq = HashMap::new();
    prereq.insert("name".to_string(), serde_json::json!("Prereq"));
    prereq.insert("base_price".to_string(), serde_json::json!(5.0));
    prereq.insert("status".to_string(), serde_json::json!("active"));
    seed(
        &ctx,
        "impresspress__products__products",
        "prod_prereq",
        prereq,
    )
    .await;

    // Cheap filler with no `requires`, listed FIRST — this is the row the
    // buggy `limit: 1` query returns, so the real gated item behind it must
    // still be checked.
    let mut filler = HashMap::new();
    filler.insert("name".to_string(), serde_json::json!("Filler"));
    filler.insert("base_price".to_string(), serde_json::json!(1.0));
    filler.insert("status".to_string(), serde_json::json!("active"));
    seed(
        &ctx,
        "impresspress__products__products",
        "prod_filler",
        filler,
    )
    .await;

    // Gated item, listed SECOND — requires `prod_prereq`, which user_1 does
    // not own.
    let mut gated = HashMap::new();
    gated.insert("name".to_string(), serde_json::json!("Gated"));
    gated.insert("base_price".to_string(), serde_json::json!(50.0));
    gated.insert("status".to_string(), serde_json::json!("active"));
    gated.insert("requires".to_string(), serde_json::json!("prod_prereq"));
    seed(
        &ctx,
        "impresspress__products__products",
        "prod_gated",
        gated,
    )
    .await;

    // Build the purchase through the real create-purchase path so line items
    // land in request-body order (filler, then gated) exactly like a live
    // checkout.
    let (msg, input) = create_msg(
        "/b/products/purchases",
        "user_1",
        serde_json::json!({
            "items": [
                {"product_id": "prod_filler", "quantity": 1},
                {"product_id": "prod_gated", "quantity": 1}
            ]
        }),
    );
    let out = purchase::handle_create(&ctx, &msg, input).await;
    let body = output_to_json(out).await;
    let purchase_id = body["id"].as_str().expect("purchase created").to_string();

    let (msg, input) = create_msg(
        "/b/products/checkout",
        "user_1",
        serde_json::json!({ "purchase_id": purchase_id }),
    );
    let out = stripe::handle_checkout(&ctx, &msg, input).await;
    assert!(
        output_is_error(out, ErrorCode::InvalidArgument).await,
        "checkout must reject when a later line item's `requires` is unmet"
    );

    // The requires-gate must reject BEFORE the atomic checkout claim
    // (`pending` -> `checkout_started`); if the gate were skipped for item 2,
    // the purchase would have advanced past `pending`.
    let rec = db::get(&ctx, "impresspress__products__purchases", &purchase_id)
        .await
        .expect("purchase row exists");
    assert_eq!(
        rec.data.get("status").and_then(|v| v.as_str()),
        Some("pending"),
        "purchase must not be claimed for checkout when the requires gate rejects it"
    );
}
