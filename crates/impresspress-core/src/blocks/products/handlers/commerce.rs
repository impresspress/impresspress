//! Public commerce-v2 storefront handlers.

use serde_json::Value;
use wafer_block_crypto::primitives;
use wafer_core::clients::{
    config,
    database::{self as db, Record},
};
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream, WaferError};

use crate::{
    blocks::products::{
        contracts::{
            FulfillmentKind, GuestOrderStatus, MoneyBreakdown, PricingPreviewRequest,
            StorefrontConfig, StorefrontOffer, StorefrontProduct, VariableVisibility,
            COMMERCE_SCHEMA_VERSION,
        },
        offer_pricing,
        repo::{offers, payment_links, purchases},
        stripe_secret_operations_allowed, PRODUCTS_TABLE,
    },
    http::{err_bad_request, err_internal, err_not_found, ok_json, ResponseBuilder},
    util::{sha256_hex, RecordExt},
};

const STOREFRONT_WIDGET_JS: &str = include_str!("../assets/storefront.js");

fn validated_publishable_key(value: &str) -> Option<(String, String)> {
    let value = value.trim();
    let mode = if value.starts_with("pk_test_") {
        "test"
    } else if value.starts_with("pk_live_") {
        "live"
    } else {
        return None;
    };
    if value.len() > 256 || value.len() < 12 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return None;
    }
    Some((value.to_string(), mode.to_string()))
}

pub(crate) async fn handle_storefront_config(ctx: &dyn Context) -> OutputStream {
    let key = config::get_default(ctx, "IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY", "").await;
    let secret = config::get_default(ctx, "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "").await;
    let validated = validated_publishable_key(&key);
    let matching_secret = validated.as_ref().is_some_and(|(key, _)| {
        super::super::stripe_client::publishable_livemode(key)
            .zip(super::super::stripe_client::secret_livemode(secret.trim()))
            .is_some_and(|(publishable, secret)| publishable == secret)
    }) && stripe_secret_operations_allowed(ctx).await;
    let response = StorefrontConfig {
        schema_version: COMMERCE_SCHEMA_VERSION,
        embedded_checkout_available: matching_secret,
        stripe_publishable_key: validated.as_ref().map(|(key, _)| key.clone()),
        stripe_mode: validated.map(|(_, mode)| mode),
    };
    let body = match serde_json::to_vec(&response) {
        Ok(body) => body,
        Err(error) => return err_internal("Could not encode storefront config", error),
    };
    ResponseBuilder::new()
        .set_header("Cache-Control", "public, max-age=60")
        .body(body, "application/json")
}

pub(crate) fn handle_storefront_widget() -> OutputStream {
    ResponseBuilder::new()
        .set_header("Cache-Control", "public, max-age=300")
        .set_header("X-Content-Type-Options", "nosniff")
        .body(
            STOREFRONT_WIDGET_JS.as_bytes().to_vec(),
            "application/javascript; charset=utf-8",
        )
}

fn optional_nonempty(record: &Record, field: &str) -> Option<String> {
    record
        .data
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(crate) async fn handle_guest_order_status(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let order_id = msg.var("id");
    let token = msg.get_meta("req.query.receipt_token");
    if order_id.is_empty() || token.is_empty() || token.len() > 256 {
        return err_not_found("Order status not found");
    }
    let order = match purchases::get(ctx, order_id).await {
        Ok(order) => order,
        Err(error) if error.code == ErrorCode::NotFound => {
            return err_not_found("Order status not found");
        }
        Err(error) => return err_internal("Could not load order status", error),
    };
    let expected_hash = order.str_field("receipt_token_hash");
    let expires_at = order.str_field("receipt_token_expires_at");
    let valid_expiry = chrono::DateTime::parse_from_rfc3339(expires_at)
        .map(|expires| expires.with_timezone(&chrono::Utc) > chrono::Utc::now())
        .unwrap_or(false);
    let supplied_hash = sha256_hex(token.as_bytes());
    if expected_hash.is_empty()
        || !valid_expiry
        || !primitives::constant_time_eq(supplied_hash.as_bytes(), expected_hash.as_bytes())
    {
        return err_not_found("Order status not found");
    }

    let currency =
        match crate::blocks::products::money::normalize_currency(order.str_field("currency")) {
            Ok(currency) => currency,
            Err(error) => return err_internal("Order has invalid currency", error),
        };
    let subscription_status = optional_nonempty(&order, "subscription_status");
    let response = GuestOrderStatus {
        schema_version: COMMERCE_SCHEMA_VERSION,
        order_id: order.id.clone(),
        status: order.str_field("status").to_string(),
        reconciliation_status: order.str_field("reconciliation_status").to_string(),
        amounts: MoneyBreakdown {
            currency,
            subtotal_minor: order.i64_field("subtotal_cents"),
            discount_minor: order.i64_field("discount_cents"),
            tax_minor: order.i64_field("tax_cents"),
            shipping_minor: order.i64_field("shipping_cents"),
            platform_fee_minor: order.i64_field("platform_fee_cents"),
            total_minor: order.i64_field("total_cents"),
        },
        subscription_status,
        subscription_current_period_end: optional_nonempty(
            &order,
            "subscription_current_period_end",
        ),
        subscription_cancel_at_period_end: order.bool_field("subscription_cancel_at_period_end"),
        paid_at: optional_nonempty(&order, "payment_at"),
        refunded_at: optional_nonempty(&order, "refunded_at"),
    };
    let body = match serde_json::to_vec(&response) {
        Ok(body) => body,
        Err(error) => return err_internal("Could not encode order status", error),
    };
    ResponseBuilder::new()
        .set_header("Cache-Control", "no-store")
        .body(body, "application/json")
}

fn decode_tags(record: &Record) -> Result<Vec<String>, WaferError> {
    match record.data.get("tags") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(raw)) if raw.is_empty() => Ok(Vec::new()),
        Some(Value::String(raw)) => serde_json::from_str(raw).map_err(|error| {
            WaferError::new(
                ErrorCode::Internal,
                format!("invalid persisted product tags: {error}"),
            )
        }),
        Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
            WaferError::new(
                ErrorCode::Internal,
                format!("invalid persisted product tags: {error}"),
            )
        }),
    }
}

fn fulfillment(record: &Record) -> Result<FulfillmentKind, WaferError> {
    let value = record.str_field("fulfillment_kind");
    let value = if value.is_empty() { "none" } else { value };
    serde_json::from_value(Value::String(value.to_string())).map_err(|error| {
        WaferError::new(
            ErrorCode::Internal,
            format!("invalid persisted fulfillment kind: {error}"),
        )
    })
}

pub(crate) async fn handle_storefront_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let product_id = msg.var("product_id");
    if product_id.is_empty() {
        return err_bad_request("Missing product ID");
    }
    let product = match db::get(ctx, PRODUCTS_TABLE, product_id).await {
        Ok(product) => product,
        Err(error) if error.code == ErrorCode::NotFound => {
            return err_not_found("Product not found");
        }
        Err(error) => return err_internal("Could not load product", error),
    };
    let deleted = product
        .data
        .get("deleted_at")
        .is_some_and(|value| !value.is_null() && value.as_str() != Some(""));
    if product.str_field("status") != "active"
        || product.str_field("approval_status") != "approved"
        || deleted
    {
        return err_not_found("Product not found");
    }

    let offer_rows = match offers::list_public_for_product(ctx, product_id).await {
        Ok(offers) => offers,
        Err(error) => return err_internal("Could not load product offers", error),
    };
    let mut public_offers = Vec::with_capacity(offer_rows.len());
    for offer in offer_rows {
        let links = match payment_links::list_public_for_offer(ctx, &offer.id).await {
            Ok(links) => links,
            Err(error) => return err_internal("Could not load offer Payment Links", error),
        };
        public_offers.push(StorefrontOffer {
            id: offer.id,
            version: offer.version,
            name: offer.name,
            mode: offer.mode,
            currency: offer.currency,
            pricing_model: offer.pricing_model,
            recurring_interval: offer.recurring_interval,
            interval_count: offer.interval_count,
            variables: offer
                .variables
                .into_iter()
                .filter(|variable| variable.visibility == VariableVisibility::Public)
                .collect(),
            checkout: offer.checkout,
            payment_links: links,
        });
    }
    let tags = match decode_tags(&product) {
        Ok(tags) => tags,
        Err(error) => return err_internal("Could not decode product", error),
    };
    let fulfillment_kind = match fulfillment(&product) {
        Ok(kind) => kind,
        Err(error) => return err_internal("Could not decode product", error),
    };
    ok_json(&StorefrontProduct {
        schema_version: COMMERCE_SCHEMA_VERSION,
        id: product.id.clone(),
        name: product.str_field("name").to_string(),
        slug: product.str_field("slug").to_string(),
        description: product.str_field("description").to_string(),
        image_url: product.str_field("image_url").to_string(),
        tags,
        fulfillment_kind,
        offers: public_offers,
    })
}

/// Evaluate a persisted active offer. The client supplies inputs, never an
/// offer definition or a trusted total; every amount comes from server-owned
/// versioned rows.
pub(crate) async fn handle_preview(ctx: &dyn Context, input: InputStream) -> OutputStream {
    let raw = input.collect_to_bytes().await;
    let request: PricingPreviewRequest = match serde_json::from_slice(&raw) {
        Ok(request) => request,
        Err(error) => return err_bad_request(&format!("Invalid body: {error}")),
    };
    let offer = match offers::get_public(ctx, &request.offer_id).await {
        Ok(offer) => offer,
        Err(error) if error.code == ErrorCode::NotFound => return err_not_found("Offer not found"),
        Err(error) => return err_internal("Could not load offer", error),
    };
    match offer_pricing::evaluate_offer(&offer, &request) {
        Ok(preview) => ok_json(&preview),
        Err(error) => err_bad_request(&format!("{}: {}", error.code, error)),
    }
}
