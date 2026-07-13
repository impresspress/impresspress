use std::collections::HashMap;

use wafer_block_crypto::primitives;
use wafer_core::clients::{config, database as db, network};
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{repo, PRODUCTS_TABLE};
use crate::{
    http::{
        err_bad_request, err_forbidden, err_internal, err_internal_no_cause, err_not_found,
        err_unauthorized, ok_json,
    },
    util::hex_encode,
};

pub async fn handle_checkout(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let Ok(stripe_key) = config::get(ctx, "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY").await else {
        return err_internal_no_cause("Stripe is not configured");
    };

    #[derive(serde::Deserialize)]
    struct CheckoutReq {
        purchase_id: String,
        success_url: Option<String>,
        cancel_url: Option<String>,
    }
    let raw = input.collect_to_bytes().await;
    let body: CheckoutReq = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Get purchase and verify ownership
    let Ok(purchase) = repo::purchases::get(ctx, &body.purchase_id).await else {
        return err_not_found("Purchase not found");
    };
    let purchase_user = purchase
        .data
        .get("user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if purchase_user != msg.user_id() {
        return err_forbidden("Cannot checkout another user's purchase");
    }

    let total_cents = purchase
        .data
        .get("total_cents")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if total_cents <= 0 {
        return err_bad_request("Purchase total must be positive");
    }

    // Check product dependency (requires field)
    let line_items = repo::purchases::line_item_product_ids(ctx, &body.purchase_id).await;

    if let Ok(items) = &line_items {
        for item in items {
            let product_id = item
                .data
                .get("product_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if product_id.is_empty() {
                continue;
            }
            if let Ok(product) = db::get(ctx, PRODUCTS_TABLE, product_id).await {
                let requires = product
                    .data
                    .get("requires")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !requires.is_empty() {
                    let has_required = user_owns_product(ctx, purchase_user, requires).await;
                    if !has_required {
                        return err_bad_request(
                            "You must own the required product before purchasing this item.",
                        );
                    }
                }
            }
        }
    }

    // Atomic status transition: pending -> checkout_started (prevents double-checkout race)
    let rows = match repo::purchases::claim_for_checkout(ctx, &body.purchase_id).await {
        Ok(n) => n,
        Err(e) => return err_internal("Failed to claim purchase for checkout", e),
    };
    if rows == 0 {
        return err_bad_request("Purchase is not in pending state or is already being processed");
    }

    let currency = purchase
        .data
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("usd")
        .to_lowercase();

    let base_url = config::get_default(
        ctx,
        "WAFER_RUN_SHARED__FRONTEND_URL",
        "http://localhost:5173",
    )
    .await;
    let success_url = body.success_url.unwrap_or_else(|| {
        format!("{base_url}/checkout/success?session_id={{CHECKOUT_SESSION_ID}}")
    });
    let cancel_url = body
        .cancel_url
        .unwrap_or_else(|| format!("{base_url}/checkout/cancel"));

    // Reject caller-supplied URLs not on the configured frontend origin.
    // Stops attackers from luring users into a Stripe-branded session that
    // redirects to an attacker-controlled site post-payment.
    if !is_same_origin(&success_url, &base_url) || !is_same_origin(&cancel_url, &base_url) {
        return err_bad_request(
            "success_url and cancel_url must be on the configured frontend origin",
        );
    }

    // Create Stripe checkout session. The form values are interpolated into
    // an application/x-www-form-urlencoded body; URL-encode everything we
    // forward from caller-controlled data (purchase_id, currency, the
    // pre-built success/cancel URLs) so a malicious id can't inject extra
    // form keys.
    let stripe_body = format!(
        "payment_method_types[]=card&line_items[0][price_data][currency]={}&line_items[0][price_data][unit_amount]={}&line_items[0][price_data][product_data][name]=Order {}&line_items[0][quantity]=1&mode=payment&success_url={}&cancel_url={}&metadata[purchase_id]={}",
        crate::util::url_path_encode(&currency),
        total_cents,
        crate::util::url_path_encode(&body.purchase_id),
        crate::util::url_path_encode(&success_url),
        crate::util::url_path_encode(&cancel_url),
        crate::util::url_path_encode(&body.purchase_id),
    );

    let mut headers = HashMap::new();
    headers.insert("Authorization".to_string(), format!("Bearer {stripe_key}"));
    headers.insert(
        "Content-Type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    );

    let stripe_api_url = config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__STRIPE_API_URL",
        "https://api.stripe.com",
    )
    .await;
    let checkout_url_endpoint = format!("{stripe_api_url}/v1/checkout/sessions");

    let resp = match network::do_request(
        ctx,
        "POST",
        &checkout_url_endpoint,
        &headers,
        Some(&stripe_body.into_bytes()),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return err_internal("Stripe API error", e),
    };

    if resp.status_code >= 400 {
        // Revert status back to pending so user can retry
        let _ = repo::purchases::revert_checkout_claim(ctx, &body.purchase_id).await;
        // SEC-054: log full Stripe response server-side for diagnostics,
        // but never forward Stripe's response body to the client — it can
        // leak account configuration, API keys in error messages, internal
        // resource IDs, etc.
        let err_body = String::from_utf8_lossy(&resp.body);
        tracing::error!(
            status = resp.status_code,
            body = %err_body,
            purchase_id = %body.purchase_id,
            "Stripe checkout session creation failed"
        );
        return err_internal(
            "Stripe API error",
            format!("status={} body={}", resp.status_code, err_body),
        );
    }

    let session: serde_json::Value = match serde_json::from_slice(&resp.body) {
        Ok(d) => d,
        Err(_) => {
            // Revert so the purchase doesn't stay stuck in checkout_started
            // (claim_for_checkout only re-claims `pending`).
            let _ = repo::purchases::revert_checkout_claim(ctx, &body.purchase_id).await;
            return err_internal_no_cause("Failed to parse Stripe response");
        }
    };

    let session_id = session.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let checkout_url = session.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if session_id.is_empty() || checkout_url.is_empty() {
        // No usable checkout session — revert the claim, same as the parse/4xx
        // paths, rather than returning an empty/broken checkout URL.
        let _ = repo::purchases::revert_checkout_claim(ctx, &body.purchase_id).await;
        return err_internal_no_cause("Stripe response missing session id or checkout url");
    }

    // Update purchase with Stripe session ID
    let mut upd = HashMap::new();
    upd.insert(
        "provider".to_string(),
        serde_json::Value::String("stripe".to_string()),
    );
    upd.insert(
        "provider_session_id".to_string(),
        serde_json::Value::String(session_id.to_string()),
    );
    upd.insert(
        "updated_at".to_string(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    if let Err(e) = repo::purchases::update(ctx, &body.purchase_id, upd).await {
        tracing::warn!("Failed to update purchase with Stripe session ID: {e}");
    }

    ok_json(&serde_json::json!({
        "session_id": session_id,
        "checkout_url": checkout_url
    }))
}

pub async fn handle_webhook(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    // Verify Stripe webhook signature - REQUIRED
    let webhook_secret =
        config::get_default(ctx, "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET", "").await;
    if webhook_secret.is_empty() {
        return err_internal_no_cause(
            "STRIPE_WEBHOOK_SECRET not configured — webhook processing disabled for security",
        );
    }
    let sig_header = msg.header("stripe-signature").to_string();
    if sig_header.is_empty() {
        return err_unauthorized("Missing Stripe-Signature header");
    }
    let raw_body = input.collect_to_bytes().await;
    if !verify_stripe_signature(&raw_body, &sig_header, &webhook_secret) {
        return err_unauthorized("Invalid webhook signature");
    }

    // Parse webhook event
    let event: serde_json::Value = match serde_json::from_slice(&raw_body) {
        Ok(e) => e,
        Err(e) => return err_bad_request(&format!("Invalid webhook body: {e}")),
    };

    let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

    let data_object = event
        .get("data")
        .and_then(|d| d.get("object"))
        .cloned()
        .unwrap_or_default();

    match event_type {
        "checkout.session.completed" => {
            // Handle product purchase completion
            let purchase_id = data_object
                .pointer("/metadata/purchase_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if !purchase_id.is_empty() {
                let payment_intent = data_object
                    .get("payment_intent")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Atomic: only complete if still in checkout_started (or pending for backwards compat)
                let rows = match repo::purchases::complete_atomic(ctx, purchase_id, &payment_intent)
                    .await
                {
                    Ok(n) => n,
                    // Returning a 500 here makes Stripe retry the webhook —
                    // a transient DB blip mustn't quietly drop the
                    // "purchase complete" transition.
                    Err(e) => return err_internal("Failed to complete purchase", e),
                };
                if rows == 0 {
                    tracing::warn!(
                        "Purchase {} not updated — already completed or refunded",
                        purchase_id
                    );
                }
            }

            // Handle subscription creation (platform billing)
            let user_id = data_object
                .pointer("/metadata/user_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let plan = data_object
                .pointer("/metadata/plan")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let stripe_customer_id = data_object
                .get("customer")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let stripe_sub_id = data_object
                .get("subscription")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if !user_id.is_empty() && !plan.is_empty() {
                if let Err(e) = repo::subscriptions::upsert_platform(
                    ctx,
                    user_id,
                    stripe_customer_id,
                    stripe_sub_id,
                    plan,
                )
                .await
                {
                    tracing::error!(error = %e, user_id = %user_id, "subscription upsert failed");
                }

                fire_products_webhook(
                    ctx,
                    "products.checkout.completed",
                    &serde_json::json!({
                        "user_id": user_id, "plan": plan
                    }),
                )
                .await;
            }
        }

        "customer.subscription.updated" => {
            let stripe_sub_id = data_object.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let status = data_object
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let plan = data_object
                .pointer("/items/data/0/price/lookup_key")
                .or_else(|| data_object.pointer("/items/data/0/price/metadata/plan"))
                .and_then(|v| v.as_str());
            if let Err(e) =
                repo::subscriptions::update_status_plan(ctx, stripe_sub_id, status, plan).await
            {
                tracing::error!(
                    error = %e,
                    stripe_sub_id = %stripe_sub_id,
                    "subscription status/plan sync failed"
                );
            }

            // Sync addon totals from Stripe subscription items metadata.
            // Each addon subscription item has metadata fields: extra_projects,
            // extra_requests, extra_r2_bytes, extra_d1_bytes (set when creating
            // the subscription item via Stripe API).
            let user_id = repo::subscriptions::find_user_by_stripe_sub(ctx, stripe_sub_id).await;
            if let Some(ref uid) = user_id {
                if let Some(items) = data_object.get("items") {
                    sync_addon_totals_from_items(ctx, uid, items).await;
                }
            }

            // Notify control plane
            if let Some(uid) = user_id {
                fire_products_webhook(
                    ctx,
                    "products.subscription.updated",
                    &serde_json::json!({
                        "user_id": uid, "plan": plan.unwrap_or("free")
                    }),
                )
                .await;
            }
        }

        "invoice.payment_failed" => {
            let stripe_sub_id = data_object
                .get("subscription")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !stripe_sub_id.is_empty() {
                // Billing-critical: surface DB failures so Stripe retries.
                if let Err(e) = repo::subscriptions::mark_past_due(ctx, stripe_sub_id).await {
                    tracing::error!(
                        error = %e,
                        stripe_sub_id = %stripe_sub_id,
                        "marking subscription past_due failed"
                    );
                    return err_internal("Failed to mark subscription past_due", e);
                }
            }
        }

        "customer.subscription.deleted" => {
            let stripe_sub_id = data_object.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let user_id = repo::subscriptions::find_user_by_stripe_sub(ctx, stripe_sub_id).await;

            // Cancellation is billing-critical — make Stripe retry on DB failure
            // so we don't leave a "cancelled in Stripe but still active here"
            // gap that grants free access to a paid user.
            if let Err(e) = repo::subscriptions::cancel_and_reset_addons(ctx, stripe_sub_id).await {
                tracing::error!(
                    error = %e,
                    stripe_sub_id = %stripe_sub_id,
                    "subscription cancellation failed"
                );
                return err_internal("Failed to cancel subscription", e);
            }

            if let Some(uid) = user_id {
                fire_products_webhook(
                    ctx,
                    "products.subscription.deleted",
                    &serde_json::json!({
                        "user_id": uid
                    }),
                )
                .await;
            }
        }

        "charge.refunded" => {
            let payment_intent = data_object
                .get("payment_intent")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if !payment_intent.is_empty() {
                if let Ok(purchase) =
                    repo::purchases::find_by_payment_intent(ctx, &payment_intent).await
                {
                    if let Err(e) = repo::purchases::mark_refunded(ctx, &purchase.id).await {
                        tracing::error!("Failed to mark purchase as refunded: {e}");
                        return err_internal("Failed to update purchase", e);
                    }
                }
            }
        }

        _ => {
            // Ignore unhandled event types
        }
    }

    ok_json(&serde_json::json!({"received": true}))
}

/// Fire a webhook for product/billing events.
/// Best-effort — if PRODUCTS_WEBHOOK_URL is not configured, this is a no-op.
/// The webhook is signed with HMAC-SHA256 using PRODUCTS_WEBHOOK_SECRET.
async fn fire_products_webhook(ctx: &dyn Context, event: &str, data: &serde_json::Value) {
    let url = config::get_default(ctx, "IMPRESSPRESS__PRODUCTS__WEBHOOK_URL", "").await;
    let secret = config::get_default(ctx, "IMPRESSPRESS__PRODUCTS__WEBHOOK_SECRET", "").await;
    if url.is_empty() {
        return;
    }

    let body = serde_json::json!({
        "event": event,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "data": data
    });
    // Silent `unwrap_or_default` would sign and send an empty body on
    // serialization failure (which would still be a 400-ish event on the
    // receiver). Drop the delivery instead — this is a best-effort webhook.
    let payload = match serde_json::to_vec(&body) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(event = event, error = %e, "failed to serialize products webhook payload; skipping delivery");
            return;
        }
    };

    // Sign with HMAC-SHA256 (same pattern as Stripe webhook verification).
    let signature = if !secret.is_empty() {
        let sig = primitives::hmac_sha256(secret.as_bytes(), &payload);
        format!("sha256={}", hex_encode(&sig))
    } else {
        String::new()
    };

    let mut headers = HashMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    if !signature.is_empty() {
        headers.insert("X-Webhook-Signature".to_string(), signature);
    }

    match network::do_request(ctx, "POST", &url, &headers, Some(&payload)).await {
        Ok(resp) if resp.status_code < 400 => {
            tracing::info!(event = event, "products webhook delivered");
        }
        Ok(resp) => {
            tracing::warn!(
                event = event,
                status = resp.status_code,
                "products webhook failed"
            );
        }
        Err(e) => {
            tracing::warn!(event = event, error = %e, "products webhook delivery error");
        }
    }
}

/// Verify Stripe webhook signature using HMAC-SHA256.
/// Stripe sends `t=timestamp,v1=signature` in the Stripe-Signature header.
fn verify_stripe_signature(payload: &[u8], sig_header: &str, secret: &str) -> bool {
    let mut timestamp = "";
    let mut expected_sig = "";

    for part in sig_header.split(',') {
        let part = part.trim();
        if let Some(t) = part.strip_prefix("t=") {
            timestamp = t;
        } else if let Some(v) = part.strip_prefix("v1=") {
            expected_sig = v;
        }
    }

    if timestamp.is_empty() || expected_sig.is_empty() {
        return false;
    }

    // Reject events with timestamps older than 5 minutes (replay protection)
    if let Ok(ts) = timestamp.parse::<u64>() {
        let now = chrono::Utc::now().timestamp() as u64;
        if now.abs_diff(ts) > 300 {
            return false;
        }
    } else {
        return false;
    }

    // Compute expected signature: HMAC-SHA256(secret, "timestamp.payload").
    // The payload is the raw HTTP body and may contain non-UTF8 bytes; running
    // it through `String::from_utf8_lossy` substitutes U+FFFD for invalid
    // sequences and would corrupt the signed buffer. Concat the parts at the
    // byte level so the HMAC matches Stripe's signer byte-for-byte.
    let mut signed_payload: Vec<u8> = Vec::with_capacity(timestamp.len() + 1 + payload.len());
    signed_payload.extend_from_slice(timestamp.as_bytes());
    signed_payload.push(b'.');
    signed_payload.extend_from_slice(payload);

    let computed = primitives::hmac_sha256(secret.as_bytes(), &signed_payload);
    let computed_hex = hex_encode(&computed);

    // Constant-time comparison
    primitives::constant_time_eq(computed_hex.as_bytes(), expected_sig.as_bytes())
}

/// Strict origin match: scheme + host + port must agree between `url` and
/// `expected_origin`. Used to validate caller-supplied success/cancel URLs.
fn is_same_origin(url: &str, expected_origin: &str) -> bool {
    fn parts(s: &str) -> Option<(&str, &str)> {
        // Split scheme://authority/...
        let after_scheme = s.find("://")?;
        let scheme = &s[..after_scheme];
        let rest = &s[after_scheme + 3..];
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        Some((scheme, authority))
    }
    match (parts(url), parts(expected_origin)) {
        (Some((s1, a1)), Some((s2, a2))) => {
            s1.eq_ignore_ascii_case(s2) && a1.eq_ignore_ascii_case(a2)
        }
        _ => false,
    }
}

/// Check if a user owns a product — either via an active subscription that
/// references it, or a completed purchase containing it as a line item.
async fn user_owns_product(ctx: &dyn Context, user_id: &str, product_id: &str) -> bool {
    // Active subscription whose plan references the product.
    if repo::subscriptions::active_plan_exists(ctx, user_id, product_id).await {
        return true;
    }
    // Completed purchase containing this product as a line item.
    let purchase_ids: Vec<serde_json::Value> =
        match repo::purchases::completed_purchase_ids(ctx, user_id).await {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|r| r.data.get("id").and_then(|v| v.as_str()).map(String::from))
                .map(serde_json::Value::String)
                .collect(),
            Err(_) => return false,
        };
    repo::purchases::line_item_exists_for_product(ctx, purchase_ids, product_id).await
}

/// Sync addon column totals from Stripe subscription items.
///
/// Reads addon values from item metadata (set by the platform when creating
/// subscription items). This keeps the products block plan-agnostic — it
/// doesn't need to know what addon packs exist, just what Stripe reports.
async fn sync_addon_totals_from_items(ctx: &dyn Context, user_id: &str, items: &serde_json::Value) {
    let mut total_projects: i64 = 0;
    let mut total_requests: i64 = 0;
    let mut total_r2: i64 = 0;
    let mut total_d1: i64 = 0;

    if let Some(data) = items.get("data").and_then(|v| v.as_array()) {
        for item in data {
            let meta = item
                .get("metadata")
                .or_else(|| item.pointer("/price/metadata"));
            let Some(meta) = meta else {
                continue;
            };

            // Skip non-addon items (the base plan item won't have addon_id)
            if meta.get("addon_id").is_none() {
                continue;
            }

            let qty = item.get("quantity").and_then(|v| v.as_i64()).unwrap_or(1);
            let parse = |key: &str| -> i64 {
                meta.get(key)
                    .and_then(|v| {
                        v.as_str()
                            .and_then(|s| s.parse().ok())
                            .or_else(|| v.as_i64())
                    })
                    .unwrap_or(0)
            };
            total_projects += parse("extra_projects") * qty;
            total_requests += parse("extra_requests") * qty;
            total_r2 += parse("extra_r2_bytes") * qty;
            total_d1 += parse("extra_d1_bytes") * qty;
        }
    }

    if let Err(e) = repo::subscriptions::set_addon_totals(
        ctx,
        user_id,
        total_projects,
        total_requests,
        total_r2,
        total_d1,
    )
    .await
    {
        tracing::error!(error = %e, user_id = %user_id, "syncing addon totals failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // constant_time_eq / hmac_sha256 behavior is tested in
    // `wafer_block_crypto::primitives` — only the Stripe-specific signature
    // protocol is exercised here.

    fn build_signed_payload(timestamp: u64, payload: &[u8]) -> Vec<u8> {
        let ts = timestamp.to_string();
        let mut buf = Vec::with_capacity(ts.len() + 1 + payload.len());
        buf.extend_from_slice(ts.as_bytes());
        buf.push(b'.');
        buf.extend_from_slice(payload);
        buf
    }

    #[test]
    fn test_verify_stripe_signature_valid() {
        let secret = "whsec_test_secret";
        let payload = b"{\"type\":\"checkout.session.completed\"}";
        let timestamp = chrono::Utc::now().timestamp() as u64;

        let signed_payload = build_signed_payload(timestamp, payload);
        let computed = primitives::hmac_sha256(secret.as_bytes(), &signed_payload);
        let computed_hex = hex_encode(&computed);

        let sig_header = format!("t={timestamp},v1={computed_hex}");

        assert!(verify_stripe_signature(payload, &sig_header, secret));
    }

    #[test]
    fn test_verify_stripe_signature_non_utf8_payload() {
        // Stripe webhook bodies are arbitrary bytes; the signer must not
        // lossy-convert them through UTF-8.
        let secret = "whsec_test";
        let payload: &[u8] = &[0xff, 0xfe, b'{', b'}'];
        let timestamp = chrono::Utc::now().timestamp() as u64;

        let signed_payload = build_signed_payload(timestamp, payload);
        let computed = primitives::hmac_sha256(secret.as_bytes(), &signed_payload);
        let computed_hex = hex_encode(&computed);

        let sig_header = format!("t={timestamp},v1={computed_hex}");
        assert!(verify_stripe_signature(payload, &sig_header, secret));
    }

    #[test]
    fn test_verify_stripe_signature_invalid_sig() {
        let timestamp = chrono::Utc::now().timestamp() as u64;

        let sig_header = format!(
            "t={timestamp},v1=0000000000000000000000000000000000000000000000000000000000000000"
        );

        assert!(!verify_stripe_signature(b"payload", &sig_header, "secret"));
    }

    #[test]
    fn test_verify_stripe_signature_expired() {
        let secret = "whsec_test";
        let payload = b"data";
        let old_timestamp = 1000000u64; // way in the past

        let signed_payload = build_signed_payload(old_timestamp, payload);
        let computed = primitives::hmac_sha256(secret.as_bytes(), &signed_payload);
        let computed_hex = hex_encode(&computed);

        let sig_header = format!("t={old_timestamp},v1={computed_hex}");

        assert!(!verify_stripe_signature(payload, &sig_header, secret));
    }

    #[test]
    fn test_verify_stripe_signature_missing_parts() {
        assert!(!verify_stripe_signature(b"data", "", "secret"));
        assert!(!verify_stripe_signature(b"data", "t=123", "secret"));
        assert!(!verify_stripe_signature(b"data", "v1=abc", "secret"));
    }

    #[test]
    fn test_is_same_origin() {
        // Match: scheme+host+port equal, path differs
        assert!(is_same_origin(
            "https://example.com/checkout/success",
            "https://example.com"
        ));
        assert!(is_same_origin(
            "https://example.com:8443/x",
            "https://example.com:8443"
        ));
        // Trailing slash on origin is fine
        assert!(is_same_origin(
            "https://example.com/x",
            "https://example.com/"
        ));

        // Mismatch: different host
        assert!(!is_same_origin(
            "https://attacker.com/x",
            "https://example.com"
        ));
        // Mismatch: different scheme
        assert!(!is_same_origin(
            "http://example.com/x",
            "https://example.com"
        ));
        // Mismatch: different port
        assert!(!is_same_origin(
            "https://example.com:8080/x",
            "https://example.com"
        ));
        // Garbage doesn't pass
        assert!(!is_same_origin("not a url", "https://example.com"));
    }

    #[test]
    fn test_urlencoding() {
        use crate::util::url_path_encode;
        assert_eq!(url_path_encode("hello"), "hello");
        assert_eq!(url_path_encode("hello world"), "hello%20world");
        assert_eq!(url_path_encode("a+b=c&d"), "a%2Bb%3Dc%26d");
        assert_eq!(
            url_path_encode("https://example.com"),
            "https%3A%2F%2Fexample.com"
        );
    }
}
