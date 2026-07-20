use std::collections::HashMap;

use base64ct::{Base64, Encoding};
use wafer_block::{
    db::{Filter, FilterOp, SortField},
    wire::database::OnConflict,
};
use wafer_block_crypto::primitives;
use wafer_core::clients::{
    config,
    database::{self as db, Record},
    network,
};
use wafer_run::{context::Context, InputStream, Message, OutputStream, WaferError};

use super::{
    contracts::{
        AmountRule, CheckoutPresentation, CheckoutRequest, CheckoutResponse, ManagedOffer,
        ManagedPaymentLink, Offer, OfferMode, OfferStatus, PaymentLinkCreateRequest,
        PricingPreviewRequest, WebhookEventList, WebhookEventSummary,
    },
    money, offer_pricing, repo, stripe_client, stripe_provider, stripe_secret_operations_allowed,
    PRODUCTS_TABLE,
};
use crate::{
    http::{
        err_bad_request, err_forbidden, err_internal, err_internal_no_cause, err_not_found,
        err_unauthorized, ok_json,
    },
    util::{hex_encode, sha256_hex, RecordExt},
};

/// Recorded Stripe webhook event ids (code review 2026-07-16: "Stripe
/// webhooks lack event idempotency"; I1 follow-up 2026-07-17: "recording
/// event before side effects drops the event on transient failure"). See
/// `003_stripe_events.sqlite.sql` for the schema and full rationale.
const STRIPE_EVENTS_TABLE: &str = "impresspress__products__stripe_events";

/// `status` column values on [`STRIPE_EVENTS_TABLE`].
const EVENT_STATUS_PENDING: &str = "pending";
const EVENT_STATUS_PROCESSING: &str = "processing";
const EVENT_STATUS_FAILED: &str = "failed";
const EVENT_STATUS_PROCESSED: &str = "processed";
const EVENT_STATUS_DEAD_LETTER: &str = "dead_letter";
const EVENT_LEASE_SECONDS: i64 = 300;
const EVENT_MAX_ATTEMPTS: u64 = 8;

/// Stable GA version used when an administrator has not selected another.
const DEFAULT_STRIPE_API_VERSION: &str = "2026-02-25.clover";
const RECEIPT_TOKEN_BYTES: usize = 32;
const RECEIPT_TOKEN_LIFETIME_DAYS: i64 = 7;

/// Outcome of recording a Stripe event id before running its side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EventRecordState {
    /// This delivery owns the exclusive processing lease.
    Claimed { owner: String, attempts: u64 },
    /// Another live delivery owns the lease. Its result remains authoritative.
    InFlight,
    /// A prior failure has a bounded backoff window which has not elapsed.
    RetryScheduled,
    /// A row already existed with `status = "processed"` — the side effects
    /// already completed. A true duplicate; the caller must skip.
    AlreadyProcessed,
    /// The bounded retry budget was exhausted and requires operator review.
    DeadLetter,
}

fn event_timestamp(record: &Record, field: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(record.str_field(field))
        .ok()
        .map(|value| value.with_timezone(&chrono::Utc))
}

fn event_retry_delay_seconds(attempts: u64) -> i64 {
    let exponent = attempts.saturating_sub(1).min(7) as u32;
    (30_i64.saturating_mul(2_i64.pow(exponent))).min(3600)
}

/// Insert and claim a fresh event atomically, or atomically acquire a failed,
/// pending, or expired processing lease. Payload hashes make a reused
/// Stripe event id with different contents fail closed. Only the matching
/// owner token can complete or release a lease.
async fn record_event(
    ctx: &dyn Context,
    event_id: &str,
    event_type: &str,
    payload: &[u8],
    stripe_account_id: &str,
    livemode: bool,
) -> Result<EventRecordState, WaferError> {
    let payload_sha256 = sha256_hex(payload);
    let payload_base64 = Base64::encode_string(payload);
    let now_value = chrono::Utc::now();
    let now = now_value.to_rfc3339();
    let owner = uuid::Uuid::now_v7().to_string();
    let rows = db::upsert(
        ctx,
        STRIPE_EVENTS_TABLE,
        vec![
            ("id".to_string(), serde_json::json!(event_id)),
            ("event_type".to_string(), serde_json::json!(event_type)),
            (
                "status".to_string(),
                serde_json::json!(EVENT_STATUS_PROCESSING),
            ),
            (
                "stripe_account_id".to_string(),
                serde_json::json!(stripe_account_id),
            ),
            ("livemode".to_string(), serde_json::json!(livemode)),
            ("attempts".to_string(), serde_json::json!(1)),
            ("processing_owner".to_string(), serde_json::json!(&owner)),
            ("processing_started_at".to_string(), serde_json::json!(&now)),
            (
                "payload_sha256".to_string(),
                serde_json::json!(&payload_sha256),
            ),
            (
                "payload_base64".to_string(),
                serde_json::json!(&payload_base64),
            ),
            ("created_at".to_string(), serde_json::json!(&now)),
        ],
        vec!["id".to_string()],
        OnConflict::SetColumns(vec![]),
    )
    .await?;
    if rows > 0 {
        return Ok(EventRecordState::Claimed { owner, attempts: 1 });
    }

    let existing = db::get(ctx, STRIPE_EVENTS_TABLE, event_id).await?;
    let stored_hash = existing.str_field("payload_sha256");
    if !stored_hash.is_empty() && stored_hash != payload_sha256 {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe event id was reused with a different signed payload",
        ));
    }
    let status = existing.str_field("status");
    match status {
        EVENT_STATUS_PROCESSED => return Ok(EventRecordState::AlreadyProcessed),
        EVENT_STATUS_DEAD_LETTER => return Ok(EventRecordState::DeadLetter),
        EVENT_STATUS_PROCESSING => {
            let lease_is_live =
                event_timestamp(&existing, "processing_started_at").is_some_and(|started| {
                    now_value.signed_duration_since(started).num_seconds() < EVENT_LEASE_SECONDS
                });
            if lease_is_live {
                return Ok(EventRecordState::InFlight);
            }
        }
        EVENT_STATUS_FAILED => {
            if event_timestamp(&existing, "next_retry_at").is_some_and(|next| next > now_value) {
                return Ok(EventRecordState::RetryScheduled);
            }
        }
        EVENT_STATUS_PENDING => {}
        _ => {}
    }

    let attempts = existing.u64_field("attempts").saturating_add(1);
    if attempts > EVENT_MAX_ATTEMPTS {
        return Ok(EventRecordState::DeadLetter);
    }
    let mut data = HashMap::new();
    data.insert(
        "status".to_string(),
        serde_json::json!(EVENT_STATUS_PROCESSING),
    );
    data.insert("attempts".to_string(), serde_json::json!(attempts));
    data.insert("processing_owner".to_string(), serde_json::json!(&owner));
    data.insert("processing_started_at".to_string(), serde_json::json!(&now));
    data.insert("next_retry_at".to_string(), serde_json::Value::Null);
    data.insert(
        "payload_sha256".to_string(),
        serde_json::json!(&payload_sha256),
    );
    data.insert(
        "payload_base64".to_string(),
        serde_json::json!(&payload_base64),
    );
    data.insert(
        "stripe_account_id".to_string(),
        serde_json::json!(stripe_account_id),
    );
    data.insert("livemode".to_string(), serde_json::json!(livemode));
    let claimed = db::update_by_filters_count(
        ctx,
        STRIPE_EVENTS_TABLE,
        vec![
            Filter {
                field: "id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(event_id),
            },
            Filter {
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(status),
            },
            Filter {
                field: "processing_owner".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(existing.str_field("processing_owner")),
            },
        ],
        data,
    )
    .await?;
    if claimed == 1 {
        Ok(EventRecordState::Claimed { owner, attempts })
    } else {
        Ok(EventRecordState::InFlight)
    }
}

async fn mark_event_processed(
    ctx: &dyn Context,
    event_id: &str,
    owner: &str,
) -> Result<(), WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut data: HashMap<String, serde_json::Value> = HashMap::new();
    data.insert(
        "status".to_string(),
        serde_json::json!(EVENT_STATUS_PROCESSED),
    );
    data.insert("processing_owner".to_string(), serde_json::json!(""));
    data.insert("processing_started_at".to_string(), serde_json::Value::Null);
    data.insert("next_retry_at".to_string(), serde_json::Value::Null);
    data.insert("last_error".to_string(), serde_json::json!(""));
    data.insert("processed_at".to_string(), serde_json::json!(&now));
    data.insert("terminal_at".to_string(), serde_json::json!(&now));
    let updated = db::update_by_filters_count(
        ctx,
        STRIPE_EVENTS_TABLE,
        vec![
            Filter {
                field: "id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(event_id),
            },
            Filter {
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(EVENT_STATUS_PROCESSING),
            },
            Filter {
                field: "processing_owner".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(owner),
            },
        ],
        data,
    )
    .await?;
    if updated == 1 {
        Ok(())
    } else {
        Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe webhook processing lease was lost before completion",
        ))
    }
}

async fn mark_event_failed(
    ctx: &dyn Context,
    event_id: &str,
    owner: &str,
    attempts: u64,
    error: &str,
) -> Result<(), WaferError> {
    let now = chrono::Utc::now();
    let dead_letter = attempts >= EVENT_MAX_ATTEMPTS;
    let mut data = HashMap::new();
    data.insert(
        "status".to_string(),
        serde_json::json!(if dead_letter {
            EVENT_STATUS_DEAD_LETTER
        } else {
            EVENT_STATUS_FAILED
        }),
    );
    data.insert("processing_owner".to_string(), serde_json::json!(""));
    data.insert("processing_started_at".to_string(), serde_json::Value::Null);
    data.insert(
        "last_error".to_string(),
        serde_json::json!(error.chars().take(1000).collect::<String>()),
    );
    if dead_letter {
        data.insert("next_retry_at".to_string(), serde_json::Value::Null);
        data.insert(
            "terminal_at".to_string(),
            serde_json::json!(now.to_rfc3339()),
        );
    } else {
        data.insert(
            "next_retry_at".to_string(),
            serde_json::json!((now
                + chrono::Duration::seconds(event_retry_delay_seconds(attempts)))
            .to_rfc3339()),
        );
    }
    let updated = db::update_by_filters_count(
        ctx,
        STRIPE_EVENTS_TABLE,
        vec![
            Filter {
                field: "id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(event_id),
            },
            Filter {
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(EVENT_STATUS_PROCESSING),
            },
            Filter {
                field: "processing_owner".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(owner),
            },
        ],
        data,
    )
    .await?;
    if updated == 1 {
        Ok(())
    } else {
        Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe webhook processing lease was lost before failure was recorded",
        ))
    }
}

fn optional_record_string(record: &Record, field: &str) -> Option<String> {
    record
        .data
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn webhook_event_summary(record: Record) -> WebhookEventSummary {
    WebhookEventSummary {
        id: record.id.clone(),
        event_type: record.str_field("event_type").to_string(),
        status: record.str_field("status").to_string(),
        stripe_account_id: record.str_field("stripe_account_id").to_string(),
        livemode: record.bool_field("livemode"),
        attempts: record.u64_field("attempts"),
        processing_started_at: optional_record_string(&record, "processing_started_at"),
        next_retry_at: optional_record_string(&record, "next_retry_at"),
        last_error: record.str_field("last_error").to_string(),
        processed_at: optional_record_string(&record, "processed_at"),
        terminal_at: optional_record_string(&record, "terminal_at"),
        created_at: record.str_field("created_at").to_string(),
        updated_at: record.str_field("updated_at").to_string(),
    }
}

pub(crate) async fn list_webhook_events(
    ctx: &dyn Context,
    status: Option<&str>,
    page: i64,
    page_size: i64,
) -> Result<WebhookEventList, WaferError> {
    let filters = status
        .map(|status| {
            vec![Filter {
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(status),
            }]
        })
        .unwrap_or_default();
    let result = db::paginated_list(
        ctx,
        STRIPE_EVENTS_TABLE,
        page,
        page_size,
        filters,
        vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }],
    )
    .await?;
    Ok(WebhookEventList {
        records: result
            .records
            .into_iter()
            .map(webhook_event_summary)
            .collect(),
        total_count: result.total_count,
        page: result.page,
        page_size: result.page_size,
    })
}

/// Replay a payload which was accepted by the Stripe signature verifier but
/// failed local processing. The stored payload hash is checked before its
/// lifecycle is reset, then it is passed back through the normal handler with
/// a fresh internal signature. This preserves every validation and lease rule.
pub(crate) async fn replay_webhook_event(
    ctx: &dyn Context,
    event_id: &str,
) -> Result<OutputStream, WaferError> {
    if !stripe_secret_operations_allowed(ctx).await {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe webhook replay is disabled in the browser runtime",
        ));
    }
    let secret =
        config::get_default(ctx, "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET", "").await;
    if secret.is_empty() {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe webhook secret is not configured",
        ));
    }
    let event = db::get(ctx, STRIPE_EVENTS_TABLE, event_id).await?;
    if !matches!(
        event.str_field("status"),
        EVENT_STATUS_FAILED | EVENT_STATUS_DEAD_LETTER
    ) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "only failed or dead-letter Stripe events can be replayed",
        ));
    }
    let payload = Base64::decode_vec(event.str_field("payload_base64")).map_err(|_| {
        WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "stored Stripe event payload failed its integrity check",
        )
    })?;
    if payload.is_empty()
        || event.str_field("payload_sha256").is_empty()
        || sha256_hex(&payload) != event.str_field("payload_sha256")
    {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "stored Stripe event payload failed its integrity check",
        ));
    }
    let parsed: serde_json::Value = serde_json::from_slice(&payload).map_err(|_| {
        WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "stored Stripe event payload is not valid JSON",
        )
    })?;
    if parsed.get("id").and_then(serde_json::Value::as_str) != Some(event_id) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "stored Stripe event id does not match its row",
        ));
    }

    let previous_status = event.str_field("status").to_string();
    let reset = db::update_by_filters_count(
        ctx,
        STRIPE_EVENTS_TABLE,
        vec![
            Filter {
                field: "id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(event_id),
            },
            Filter {
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(&previous_status),
            },
        ],
        HashMap::from([
            (
                "status".to_string(),
                serde_json::json!(EVENT_STATUS_PENDING),
            ),
            ("attempts".to_string(), serde_json::json!(0)),
            ("processing_owner".to_string(), serde_json::json!("")),
            ("processing_started_at".to_string(), serde_json::Value::Null),
            ("next_retry_at".to_string(), serde_json::Value::Null),
            ("processed_at".to_string(), serde_json::Value::Null),
            ("terminal_at".to_string(), serde_json::Value::Null),
            (
                "last_error".to_string(),
                serde_json::json!("manual replay requested"),
            ),
        ]),
    )
    .await?;
    if reset != 1 {
        return Err(WaferError::new(
            wafer_run::ErrorCode::Aborted,
            "Stripe event changed state before replay could start",
        ));
    }

    let timestamp = chrono::Utc::now().timestamp() as u64;
    let mut signed_payload = timestamp.to_string().into_bytes();
    signed_payload.push(b'.');
    signed_payload.extend_from_slice(&payload);
    let signature = primitives::hmac_sha256(secret.as_bytes(), &signed_payload);
    let mut message = Message::new("http.request");
    message.set_meta("req.action", "create");
    message.set_meta("req.resource", "/b/products/webhooks");
    message.set_meta(
        "http.header.stripe-signature",
        format!("t={timestamp},v1={}", hex_encode(&signature)),
    );
    Ok(handle_webhook(ctx, &message, InputStream::from_bytes(payload)).await)
}

pub async fn handle_checkout(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    if !stripe_secret_operations_allowed(ctx).await {
        return err_forbidden(
            "Stripe secret-key checkout is disabled in the browser runtime; use a trusted remote commerce API or a pre-created Payment Link",
        );
    }
    let Ok(stripe_key) = config::get(ctx, "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY").await else {
        return err_internal_no_cause("Stripe is not configured");
    };
    if stripe_key.trim().is_empty() {
        return err_internal_no_cause("Stripe is not configured");
    }
    let stripe_api_version = config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__STRIPE_API_VERSION",
        DEFAULT_STRIPE_API_VERSION,
    )
    .await;
    if !is_stable_stripe_api_version(&stripe_api_version) {
        return err_internal_no_cause(
            "Stripe API version must be a stable YYYY-MM-DD.release value",
        );
    }

    let raw = input.collect_to_bytes().await;
    let request: CheckoutRequest = match serde_json::from_slice(&raw) {
        Ok(request) => request,
        Err(error) => return err_bad_request(&format!("Invalid body: {error}")),
    };
    handle_offer_checkout(ctx, msg, request, &stripe_key, &stripe_api_version).await
}

fn configured_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

async fn issue_receipt_token(ctx: &dyn Context) -> Result<(String, String, String), WaferError> {
    let _ = ctx;
    let mut bytes = vec![0_u8; RECEIPT_TOKEN_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|error| {
        WaferError::new(
            wafer_run::ErrorCode::Internal,
            format!("could not generate checkout receipt token: {error}"),
        )
    })?;
    let token = hex_encode(&bytes);
    let token_hash = sha256_hex(token.as_bytes());
    let expires_at =
        (chrono::Utc::now() + chrono::Duration::days(RECEIPT_TOKEN_LIFETIME_DAYS)).to_rfc3339();
    Ok((token, token_hash, expires_at))
}

fn wire_enum<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| "commerce enum did not serialize as a string".to_string())
}

fn push_form(pairs: &mut Vec<(String, String)>, key: impl Into<String>, value: impl ToString) {
    pairs.push((key.into(), value.to_string()));
}

fn encode_form(pairs: Vec<(String, String)>) -> String {
    stripe_client::encode_form(pairs)
}

fn application_fee(total_minor: i64, basis_points: u16) -> Result<i64, String> {
    let fee = i128::from(total_minor)
        .checked_mul(i128::from(basis_points))
        .ok_or_else(|| "application fee is too large".to_string())?
        / 10_000;
    i64::try_from(fee).map_err(|_| "application fee is too large".to_string())
}

fn synced_component_price<'a>(
    offer: &'a Offer,
    resolved: &crate::blocks::products::contracts::ResolvedComponent,
) -> Option<&'a str> {
    offer.components.iter().find_map(|component| {
        let price_id = component.stripe_price_id.trim();
        match &component.amount {
            AmountRule::Fixed { unit_amount_minor }
                if component.id == resolved.component_id
                    && *unit_amount_minor == resolved.unit_amount_minor
                    && price_id.starts_with("price_") =>
            {
                Some(price_id)
            }
            _ => None,
        }
    })
}

fn shipping_countries(offer: &Offer, fallback_country: &str) -> Vec<String> {
    if offer.checkout.allowed_shipping_countries.is_empty() {
        vec![fallback_country.to_ascii_uppercase()]
    } else {
        offer
            .checkout
            .allowed_shipping_countries
            .iter()
            .map(|country| country.trim().to_ascii_uppercase())
            .collect()
    }
}

fn allowed_shipping_amounts(offer: &Offer) -> Vec<i64> {
    if offer.checkout.shipping_options.is_empty() {
        return vec![0];
    }
    let mut amounts = offer
        .checkout
        .shipping_options
        .iter()
        .map(|option| option.amount_minor)
        .collect::<Vec<_>>();
    amounts.sort_unstable();
    amounts.dedup();
    amounts
}

fn shipping_amount_is_allowed(offer: &Offer, amount_minor: i64) -> bool {
    allowed_shipping_amounts(offer).contains(&amount_minor)
}

fn push_shipping_address_collection(
    pairs: &mut Vec<(String, String)>,
    offer: &Offer,
    fallback_country: &str,
) {
    if !offer.checkout.collect_shipping_address {
        return;
    }
    for (index, country) in shipping_countries(offer, fallback_country)
        .into_iter()
        .enumerate()
    {
        push_form(
            pairs,
            format!("shipping_address_collection[allowed_countries][{index}]"),
            country,
        );
    }
}

fn push_checkout_shipping_options(
    pairs: &mut Vec<(String, String)>,
    offer: &Offer,
    currency: &str,
) -> Result<(), String> {
    for (index, option) in offer.checkout.shipping_options.iter().enumerate() {
        let prefix = format!("shipping_options[{index}]");
        let stripe_rate = option.stripe_shipping_rate_id.trim();
        if !stripe_rate.is_empty() {
            push_form(pairs, format!("{prefix}[shipping_rate]"), stripe_rate);
            continue;
        }
        let rate = format!("{prefix}[shipping_rate_data]");
        push_form(pairs, format!("{rate}[type]"), "fixed_amount");
        push_form(
            pairs,
            format!("{rate}[display_name]"),
            option.display_name.trim(),
        );
        push_form(
            pairs,
            format!("{rate}[fixed_amount][amount]"),
            option.amount_minor,
        );
        push_form(pairs, format!("{rate}[fixed_amount][currency]"), currency);
        push_form(
            pairs,
            format!("{rate}[tax_behavior]"),
            wire_enum(&option.tax_behavior)?,
        );
        if let Some(estimate) = &option.delivery_estimate {
            let unit = wire_enum(&estimate.unit)?;
            if let Some(minimum) = estimate.minimum {
                push_form(
                    pairs,
                    format!("{rate}[delivery_estimate][minimum][value]"),
                    minimum,
                );
                push_form(
                    pairs,
                    format!("{rate}[delivery_estimate][minimum][unit]"),
                    &unit,
                );
            }
            if let Some(maximum) = estimate.maximum {
                push_form(
                    pairs,
                    format!("{rate}[delivery_estimate][maximum][value]"),
                    maximum,
                );
                push_form(
                    pairs,
                    format!("{rate}[delivery_estimate][maximum][unit]"),
                    &unit,
                );
            }
        }
    }
    Ok(())
}

fn payment_link_shipping_supported(offer: &Offer) -> Result<(), String> {
    if offer
        .checkout
        .shipping_options
        .iter()
        .any(|option| option.stripe_shipping_rate_id.trim().is_empty())
    {
        return Err(
            "Payment Links require a Stripe shipping rate ID for every shipping option; use hosted or embedded Checkout for inline fixed rates"
                .to_string(),
        );
    }
    Ok(())
}

async fn platform_country(ctx: &dyn Context) -> String {
    let configured =
        config::get_default(ctx, "IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY", "US").await;
    let country = configured.trim();
    if country.len() == 2 && country.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        country.to_ascii_uppercase()
    } else {
        "US".to_string()
    }
}

#[allow(clippy::too_many_arguments)]
fn build_offer_checkout_form(
    offer: &Offer,
    preview: &crate::blocks::products::contracts::PricingPreview,
    product_name: &str,
    order_id: &str,
    request: &CheckoutRequest,
    success_url: &str,
    cancel_url: &str,
    automatic_tax: bool,
    country: &str,
    fee_minor: i64,
    fee_basis_points: u16,
) -> Result<String, String> {
    let mut pairs = Vec::new();
    push_form(&mut pairs, "payment_method_types[]", "card");
    push_form(&mut pairs, "mode", wire_enum(&offer.mode)?);
    push_form(&mut pairs, "client_reference_id", order_id);
    push_form(&mut pairs, "metadata[purchase_id]", order_id);
    push_form(&mut pairs, "metadata[offer_id]", &offer.id);
    push_form(&mut pairs, "metadata[offer_version]", offer.version);
    let provider_metadata_prefix = match offer.mode {
        OfferMode::Payment => "payment_intent_data[metadata]",
        OfferMode::Subscription => "subscription_data[metadata]",
    };
    push_form(
        &mut pairs,
        format!("{provider_metadata_prefix}[purchase_id]"),
        order_id,
    );
    push_form(
        &mut pairs,
        format!("{provider_metadata_prefix}[offer_id]"),
        &offer.id,
    );
    push_form(
        &mut pairs,
        format!("{provider_metadata_prefix}[offer_version]"),
        offer.version,
    );
    if let Some(email) = request
        .buyer_email
        .as_deref()
        .map(str::trim)
        .filter(|email| !email.is_empty())
    {
        push_form(&mut pairs, "customer_email", email);
    }

    match request.presentation {
        CheckoutPresentation::Hosted => {
            push_form(&mut pairs, "success_url", success_url);
            push_form(&mut pairs, "cancel_url", cancel_url);
        }
        CheckoutPresentation::Embedded => {
            push_form(&mut pairs, "ui_mode", "embedded");
            push_form(&mut pairs, "return_url", success_url);
        }
        CheckoutPresentation::PaymentLink => {
            return Err(
                "payment_link checkout requires a synchronized fixed offer or preset".to_string(),
            );
        }
    }

    if automatic_tax {
        push_form(&mut pairs, "automatic_tax[enabled]", "true");
    }
    if offer.checkout.allow_promotion_codes {
        push_form(&mut pairs, "allow_promotion_codes", "true");
    }
    if offer.checkout.collect_billing_address {
        push_form(&mut pairs, "billing_address_collection", "required");
    }
    push_shipping_address_collection(&mut pairs, offer, country);
    push_checkout_shipping_options(
        &mut pairs,
        offer,
        &preview.amounts.currency.to_ascii_lowercase(),
    )?;
    if offer.checkout.create_customer && matches!(offer.mode, OfferMode::Payment) {
        push_form(&mut pairs, "customer_creation", "always");
    }
    if offer.checkout.require_terms_consent {
        push_form(
            &mut pairs,
            "consent_collection[terms_of_service]",
            "required",
        );
    }
    if matches!(offer.mode, OfferMode::Subscription) && offer.checkout.trial_days > 0 {
        push_form(
            &mut pairs,
            "subscription_data[trial_period_days]",
            offer.checkout.trial_days,
        );
    }
    if fee_minor > 0 {
        match offer.mode {
            OfferMode::Payment => push_form(
                &mut pairs,
                "payment_intent_data[application_fee_amount]",
                fee_minor,
            ),
            OfferMode::Subscription => {
                // Stripe accepts up to two fractional percent digits. Basis
                // points map exactly to that representation.
                let mut percentage =
                    format!("{}.{:02}", fee_basis_points / 100, fee_basis_points % 100);
                while percentage.ends_with('0') {
                    percentage.pop();
                }
                if percentage.ends_with('.') {
                    percentage.pop();
                }
                push_form(
                    &mut pairs,
                    "subscription_data[application_fee_percent]",
                    percentage,
                );
            }
        }
    }

    let mode = wire_enum(&offer.mode)?;
    let currency = preview.amounts.currency.to_ascii_lowercase();
    let tax_behavior = wire_enum(&offer.tax_behavior)?;
    let recurring_interval = offer
        .recurring_interval
        .as_ref()
        .map(wire_enum)
        .transpose()?;
    let mut item_index = 0usize;
    for component in preview
        .components
        .iter()
        .filter(|component| component.included)
    {
        let prefix = format!("line_items[{item_index}]");
        if let Some(price_id) = synced_component_price(offer, component) {
            push_form(&mut pairs, format!("{prefix}[price]"), price_id);
        } else {
            push_form(
                &mut pairs,
                format!("{prefix}[price_data][currency]"),
                &currency,
            );
            push_form(
                &mut pairs,
                format!("{prefix}[price_data][unit_amount]"),
                component.unit_amount_minor,
            );
            push_form(
                &mut pairs,
                format!("{prefix}[price_data][product_data][name]"),
                format!("{product_name} — {}", component.label),
            );
            push_form(
                &mut pairs,
                format!("{prefix}[price_data][tax_behavior]"),
                &tax_behavior,
            );
            if mode == "subscription" {
                push_form(
                    &mut pairs,
                    format!("{prefix}[price_data][recurring][interval]"),
                    recurring_interval
                        .as_deref()
                        .ok_or_else(|| "subscription offer is missing recurrence".to_string())?,
                );
                push_form(
                    &mut pairs,
                    format!("{prefix}[price_data][recurring][interval_count]"),
                    offer.interval_count,
                );
            }
        }
        push_form(
            &mut pairs,
            format!("{prefix}[quantity]"),
            component.quantity,
        );
        item_index += 1;
    }
    if item_index == 0 {
        return Err("checkout has no included line items".to_string());
    }
    Ok(encode_form(pairs))
}

async fn handle_offer_checkout(
    ctx: &dyn Context,
    msg: &Message,
    request: CheckoutRequest,
    stripe_key: &str,
    stripe_api_version: &str,
) -> OutputStream {
    if let Some(email) = request.buyer_email.as_deref() {
        if email.len() > 254 || email.chars().any(char::is_control) {
            return err_bad_request("buyer_email is invalid");
        }
    }
    let offer = match repo::offers::get_public(ctx, &request.offer_id).await {
        Ok(offer) => offer,
        Err(error) if error.code == wafer_run::ErrorCode::NotFound => {
            return err_not_found("Offer not found");
        }
        Err(error) => return err_internal("Could not load offer", error),
    };
    let product = match db::get(ctx, PRODUCTS_TABLE, &offer.product_id).await {
        Ok(product) => product,
        Err(error) => return err_internal("Could not load offer product", error),
    };

    let owner_is_user = product.str_field("owner_kind") == "user";
    let (seller_account_id, stripe_account_id, fee_basis_points) = if owner_is_user {
        let user_selling =
            config::get_default(ctx, "WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "false").await;
        if !configured_bool(&user_selling) {
            return err_not_found("Offer not found");
        }
        let owner_id = product.str_field("owner_id");
        let Ok(seller) = repo::seller_accounts::ready_for_user(ctx, owner_id).await else {
            return err_bad_request("This seller's Stripe account is not ready to accept charges");
        };
        let configured_fee = config::get_default(
            ctx,
            "IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS",
            "0",
        )
        .await
        .parse::<u16>()
        .ok()
        .filter(|value| *value <= 10_000)
        .unwrap_or(0);
        let fee = if seller.fee_basis_points == 0 {
            configured_fee
        } else {
            seller.fee_basis_points
        };
        (seller.id, seller.stripe_account_id, fee)
    } else {
        (String::new(), String::new(), 0)
    };

    let checkout_inputs = match request.preset_id.as_deref() {
        Some(preset_id) => {
            if !request.inputs.is_empty() {
                return err_bad_request("preset checkout cannot also provide runtime inputs");
            }
            match repo::checkout_presets::get_active(ctx, &offer.id, preset_id).await {
                Ok(preset) => preset.inputs,
                Err(error) if error.code == wafer_run::ErrorCode::NotFound => {
                    return err_not_found("Checkout preset not found");
                }
                Err(error) => return err_internal("Could not load checkout preset", error),
            }
        }
        None => request.inputs.clone(),
    };
    let pricing_request = PricingPreviewRequest {
        offer_id: request.offer_id.clone(),
        quantity: request.quantity,
        inputs: checkout_inputs,
    };
    let mut preview = match offer_pricing::evaluate_offer(&offer, &pricing_request) {
        Ok(preview) => preview,
        Err(error) => return err_bad_request(&format!("{}: {}", error.code, error)),
    };
    let fee_minor = match application_fee(preview.amounts.total_minor, fee_basis_points) {
        Ok(fee) => fee,
        Err(error) => return err_bad_request(&error),
    };
    preview.amounts.platform_fee_minor = fee_minor;

    let requires = product.str_field("requires");
    if !requires.is_empty()
        && (msg.user_id().is_empty() || !user_owns_product(ctx, msg.user_id(), requires).await)
    {
        return err_bad_request(
            "You must sign in and own the required product before purchasing this item.",
        );
    }

    let base_url = config::get_default(
        ctx,
        "WAFER_RUN_SHARED__FRONTEND_URL",
        "http://localhost:5173",
    )
    .await;
    let success_url = request.success_url.clone().unwrap_or_else(|| {
        format!("{base_url}/checkout/success?session_id={{CHECKOUT_SESSION_ID}}")
    });
    let cancel_url = request
        .cancel_url
        .clone()
        .unwrap_or_else(|| format!("{base_url}/checkout/cancel"));
    let allowed_origins =
        config::get_default(ctx, "IMPRESSPRESS__PRODUCTS__CHECKOUT_ALLOWED_ORIGINS", "").await;
    if !is_allowed_checkout_url(&success_url, &base_url, &allowed_origins)
        || !is_allowed_checkout_url(&cancel_url, &base_url, &allowed_origins)
    {
        return err_bad_request(
            "success_url and cancel_url must be on a configured checkout origin",
        );
    }

    let input_snapshot = match serde_json::to_string(&preview.inputs) {
        Ok(snapshot) => snapshot,
        Err(error) => return err_internal("Could not snapshot checkout inputs", error),
    };
    let Some(expected_livemode) = stripe_client::secret_livemode(stripe_key) else {
        return err_internal_no_cause(
            "Stripe secret key is malformed; expected an sk_test_ or sk_live_ key",
        );
    };
    let (receipt_token, receipt_token_hash, receipt_token_expires_at) =
        match issue_receipt_token(ctx).await {
            Ok(receipt) => receipt,
            Err(error) => return err_internal("Could not create checkout receipt", error),
        };
    let mut items = Vec::new();
    for resolved in preview
        .components
        .iter()
        .filter(|component| component.included)
    {
        let Some(component) = offer
            .components
            .iter()
            .find(|component| component.id == resolved.component_id)
        else {
            return err_internal_no_cause("Resolved checkout component is missing");
        };
        let condition_snapshot = match serde_json::to_string(&component.condition) {
            Ok(snapshot) => snapshot,
            Err(error) => return err_internal("Could not snapshot checkout condition", error),
        };
        items.push(repo::purchases::CheckoutLineSnapshot {
            product_id: offer.product_id.clone(),
            product_name: format!("{} — {}", product.str_field("name"), resolved.label),
            offer_id: offer.id.clone(),
            offer_version: offer.version,
            component_id: resolved.component_id.clone(),
            quantity: resolved.quantity,
            unit_amount_minor: resolved.unit_amount_minor,
            total_amount_minor: resolved.total_amount_minor,
            input_snapshot: input_snapshot.clone(),
            condition_snapshot,
        });
    }
    let order = match repo::purchases::create_checkout_order(
        ctx,
        repo::purchases::CheckoutOrderSnapshot {
            buyer_user_id: msg.user_id().to_string(),
            buyer_email: request
                .buyer_email
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .to_string(),
            seller_account_id,
            stripe_account_id: stripe_account_id.clone(),
            presentation: request.presentation,
            mode: offer.mode,
            offer_id: offer.id.clone(),
            offer_version: offer.version,
            livemode: expected_livemode,
            receipt_token_hash,
            receipt_token_expires_at: Some(receipt_token_expires_at.clone()),
            allowed_shipping_amounts_minor: allowed_shipping_amounts(&offer),
            amounts: preview.amounts.clone(),
            items,
        },
    )
    .await
    {
        Ok(order) => order,
        Err(error) => return err_internal("Could not create checkout order", error),
    };

    let rows = match repo::purchases::claim_for_checkout(ctx, &order.id).await {
        Ok(rows) => rows,
        Err(error) => return err_internal("Could not claim checkout order", error),
    };
    if rows != 1 {
        return err_internal_no_cause("Checkout order could not be claimed");
    }

    let automatic_tax_config =
        config::get_default(ctx, "IMPRESSPRESS__PRODUCTS__AUTOMATIC_TAX", "false").await;
    let automatic_tax = offer.checkout.automatic_tax || configured_bool(&automatic_tax_config);
    let country = platform_country(ctx).await;
    let stripe_body = match build_offer_checkout_form(
        &offer,
        &preview,
        product.str_field("name"),
        &order.id,
        &request,
        &success_url,
        &cancel_url,
        automatic_tax,
        &country.to_ascii_uppercase(),
        fee_minor,
        fee_basis_points,
    ) {
        Ok(body) => body,
        Err(error) => {
            let _ = repo::purchases::mark_checkout_failed(ctx, &order.id, &error).await;
            return err_bad_request(&error);
        }
    };

    let mut headers = stripe_request_headers(
        stripe_key,
        stripe_api_version,
        Some(&format!("impresspress_offer_checkout_{}", order.id)),
    );
    if !stripe_account_id.is_empty() {
        headers.insert("Stripe-Account".to_string(), stripe_account_id);
    }
    let stripe_api_url = config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__STRIPE_API_URL",
        "https://api.stripe.com",
    )
    .await;
    let endpoint = format!("{stripe_api_url}/v1/checkout/sessions");
    let response = match stripe_client::send_raw(
        ctx,
        "POST",
        &endpoint,
        &headers,
        Some(stripe_body.as_bytes()),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            let _ = repo::purchases::mark_checkout_failed(
                ctx,
                &order.id,
                "Stripe Checkout Session request failed",
            )
            .await;
            return err_internal("Stripe API error", error);
        }
    };
    if response.status_code >= 400 {
        let body = String::from_utf8_lossy(&response.body);
        tracing::error!(
            status = response.status_code,
            body = %body,
            purchase_id = %order.id,
            "Stripe offer Checkout Session creation failed"
        );
        let _ = repo::purchases::mark_checkout_failed(
            ctx,
            &order.id,
            "Stripe rejected the Checkout Session",
        )
        .await;
        return err_internal_no_cause("Stripe API error");
    }
    let session: serde_json::Value = match serde_json::from_slice(&response.body) {
        Ok(session) => session,
        Err(_) => {
            let _ = repo::purchases::mark_checkout_failed(
                ctx,
                &order.id,
                "Stripe response could not be decoded",
            )
            .await;
            return err_internal_no_cause("Failed to parse Stripe response");
        }
    };
    let session_id = session
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let checkout_url = session
        .get("url")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let client_secret = session
        .get("client_secret")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let response_is_usable = !session_id.is_empty()
        && match request.presentation {
            CheckoutPresentation::Hosted => checkout_url.is_some(),
            CheckoutPresentation::Embedded => client_secret.is_some(),
            CheckoutPresentation::PaymentLink => false,
        };
    if !response_is_usable {
        let _ = repo::purchases::mark_checkout_failed(
            ctx,
            &order.id,
            "Stripe response was missing required Checkout Session fields",
        )
        .await;
        return err_internal_no_cause("Stripe response missing required Checkout Session fields");
    }
    let updated = HashMap::from([
        (
            "provider_session_id".to_string(),
            serde_json::json!(session_id),
        ),
        (
            "reconciliation_status".to_string(),
            serde_json::json!("awaiting_payment"),
        ),
        (
            "updated_at".to_string(),
            serde_json::json!(chrono::Utc::now().to_rfc3339()),
        ),
    ]);
    if let Err(error) = repo::purchases::update(ctx, &order.id, updated).await {
        // The provider session now exists and its metadata points to this
        // retained checkout_started order. Do not revert the claim or create a
        // second charge path; the webhook/reconciliation worker can finish it.
        return err_internal("Could not save Stripe checkout session", error);
    }
    ok_json(&CheckoutResponse {
        order_id: order.id,
        receipt_token,
        receipt_token_expires_at,
        presentation: request.presentation,
        checkout_url,
        client_secret,
        payment_link_url: None,
        amounts: preview.amounts,
    })
}

async fn payment_link_seller_context(
    ctx: &dyn Context,
    product: &Record,
) -> Result<(String, String, u16), WaferError> {
    if product.str_field("owner_kind") != "user" {
        return Ok((String::new(), String::new(), 0));
    }
    let user_selling =
        config::get_default(ctx, "WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS", "false").await;
    if !configured_bool(&user_selling) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "user product selling is disabled",
        ));
    }
    let seller = repo::seller_accounts::ready_for_user(ctx, product.str_field("owner_id")).await?;
    let configured_fee = config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS",
        "0",
    )
    .await
    .parse::<u16>()
    .ok()
    .filter(|value| *value <= 10_000)
    .unwrap_or(0);
    let fee = if seller.fee_basis_points == 0 {
        configured_fee
    } else {
        seller.fee_basis_points
    };
    Ok((seller.id, seller.stripe_account_id, fee))
}

async fn stripe_catalog_post(
    ctx: &dyn Context,
    endpoint: &str,
    headers: &HashMap<String, String>,
    form: Vec<(String, String)>,
) -> Result<serde_json::Value, WaferError> {
    let body = encode_form(form);
    let response = stripe_client::send_raw(ctx, "POST", endpoint, headers, Some(body.as_bytes()))
        .await
        .map_err(|error| {
            WaferError::new(
                wafer_run::ErrorCode::Internal,
                format!("Stripe catalog request could not be completed: {error}"),
            )
        })?;
    if response.status_code >= 400 {
        let decoded: serde_json::Value = serde_json::from_slice(&response.body).unwrap_or_default();
        let code = decoded
            .pointer("/error/code")
            .or_else(|| decoded.pointer("/error/type"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("provider_error");
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            format!(
                "Stripe rejected catalog synchronization (HTTP {}, code {code})",
                response.status_code
            ),
        ));
    }
    serde_json::from_slice(&response.body).map_err(|_| {
        WaferError::new(
            wafer_run::ErrorCode::Internal,
            "Stripe catalog response could not be decoded",
        )
    })
}

async fn stripe_catalog_get(
    ctx: &dyn Context,
    endpoint: &str,
    headers: &HashMap<String, String>,
) -> Result<Option<serde_json::Value>, WaferError> {
    let response = stripe_client::send_raw(ctx, "GET", endpoint, headers, None)
        .await
        .map_err(|error| {
            WaferError::new(
                wafer_run::ErrorCode::Internal,
                format!("Stripe catalog reconciliation could not be completed: {error}"),
            )
        })?;
    if response.status_code == 404 {
        return Ok(None);
    }
    if response.status_code >= 400 {
        let decoded: serde_json::Value = serde_json::from_slice(&response.body).unwrap_or_default();
        let code = decoded
            .pointer("/error/code")
            .or_else(|| decoded.pointer("/error/type"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("provider_error");
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            format!(
                "Stripe rejected catalog reconciliation (HTTP {}, code {code})",
                response.status_code
            ),
        ));
    }
    let decoded: serde_json::Value = serde_json::from_slice(&response.body).map_err(|_| {
        WaferError::new(
            wafer_run::ErrorCode::Internal,
            "Stripe catalog reconciliation response could not be decoded",
        )
    })?;
    if decoded
        .get("deleted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        Ok(None)
    } else {
        Ok(Some(decoded))
    }
}

fn stripe_catalog_headers(
    stripe_key: &str,
    api_version: &str,
    stripe_account_id: &str,
    idempotency_key: Option<&str>,
) -> HashMap<String, String> {
    let mut headers = stripe_request_headers(stripe_key, api_version, idempotency_key);
    if !stripe_account_id.is_empty() {
        headers.insert("Stripe-Account".to_string(), stripe_account_id.to_string());
    }
    headers
}

fn stripe_product_form(product: &Record) -> Vec<(String, String)> {
    vec![
        ("name".to_string(), product.str_field("name").to_string()),
        (
            "description".to_string(),
            product.str_field("description").to_string(),
        ),
        ("active".to_string(), "true".to_string()),
        (
            "metadata[impresspress_product_id]".to_string(),
            product.id.clone(),
        ),
    ]
}

fn validate_stripe_product(
    response: &serde_json::Value,
    expected_id: Option<&str>,
    livemode: bool,
) -> Result<String, WaferError> {
    let id = response
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let response_livemode = response
        .get("livemode")
        .and_then(serde_json::Value::as_bool);
    let active = response.get("active").and_then(serde_json::Value::as_bool);
    if !id.starts_with("prod_")
        || expected_id.is_some_and(|expected| expected != id)
        || response_livemode != Some(livemode)
        || active != Some(true)
    {
        return Err(WaferError::new(
            wafer_run::ErrorCode::Internal,
            "Stripe Product response did not match the active configured catalog",
        ));
    }
    Ok(id.to_string())
}

fn validate_stripe_price(
    response: &serde_json::Value,
    expected_id: Option<&str>,
    stripe_product_id: &str,
    offer: &Offer,
    unit_amount_minor: i64,
    livemode: bool,
    expected_active: bool,
) -> Result<String, WaferError> {
    let id = response
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let response_livemode = response
        .get("livemode")
        .and_then(serde_json::Value::as_bool);
    let response_product = response
        .get("product")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let response_currency = response
        .get("currency")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let response_amount = response
        .get("unit_amount")
        .and_then(serde_json::Value::as_i64);
    let active = response.get("active").and_then(serde_json::Value::as_bool);
    let recurring_matches = match offer.mode {
        OfferMode::Payment => response
            .get("recurring")
            .is_none_or(serde_json::Value::is_null),
        OfferMode::Subscription => {
            let expected_interval = offer
                .recurring_interval
                .as_ref()
                .and_then(|interval| serde_json::to_value(interval).ok())
                .and_then(|interval| interval.as_str().map(str::to_string));
            let expected_usage = serde_json::to_value(offer.usage_type)
                .ok()
                .and_then(|usage| usage.as_str().map(str::to_string));
            response
                .pointer("/recurring/interval")
                .and_then(serde_json::Value::as_str)
                == expected_interval.as_deref()
                && response
                    .pointer("/recurring/interval_count")
                    .and_then(serde_json::Value::as_u64)
                    == Some(u64::from(offer.interval_count))
                && response
                    .pointer("/recurring/usage_type")
                    .and_then(serde_json::Value::as_str)
                    == expected_usage.as_deref()
        }
    };
    if !id.starts_with("price_")
        || expected_id.is_some_and(|expected| expected != id)
        || response_livemode != Some(livemode)
        || response_product != stripe_product_id
        || !response_currency.eq_ignore_ascii_case(&offer.currency)
        || response_amount != Some(unit_amount_minor)
        || active != Some(expected_active)
        || !recurring_matches
    {
        return Err(WaferError::new(
            wafer_run::ErrorCode::Internal,
            "Stripe Price response did not match the active immutable offer row",
        ));
    }
    Ok(id.to_string())
}

async fn sync_offer_catalog_inner(
    ctx: &dyn Context,
    product: &Record,
    managed: ManagedOffer,
) -> Result<ManagedOffer, WaferError> {
    if managed.status != OfferStatus::Active {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "only active immutable offers can be synchronized to Stripe",
        ));
    }
    offer_pricing::validate_offer(&managed.offer).map_err(|error| {
        WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            format!("offer is not valid for Stripe synchronization: {error}"),
        )
    })?;
    let stripe_key = config::get(ctx, "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY").await?;
    let livemode = stripe_client::secret_livemode(&stripe_key).ok_or_else(|| {
        WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe secret key must be a test or live secret key",
        )
    })?;
    let api_version = config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__STRIPE_API_VERSION",
        DEFAULT_STRIPE_API_VERSION,
    )
    .await;
    if !is_stable_stripe_api_version(&api_version) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe API version must be a stable named release",
        ));
    }
    let api_url = config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__STRIPE_API_URL",
        "https://api.stripe.com",
    )
    .await;
    let (_, stripe_account_id, _) = payment_link_seller_context(ctx, product).await?;

    let mut stripe_product_id = product.str_field("stripe_product_id").to_string();
    if stripe_product_id.is_empty() {
        stripe_product_id = managed.offer.stripe_product_id.clone();
    }
    let stale_product_id = stripe_product_id.clone();
    let mut product_replaced = false;
    if !stripe_product_id.is_empty() {
        if !stripe_product_id.starts_with("prod_") {
            return Err(WaferError::new(
                wafer_run::ErrorCode::FailedPrecondition,
                "stored Stripe Product id is invalid",
            ));
        }
        let headers = stripe_catalog_headers(&stripe_key, &api_version, &stripe_account_id, None);
        let endpoint = format!(
            "{}/v1/products/{stripe_product_id}",
            api_url.trim_end_matches('/')
        );
        match stripe_catalog_get(ctx, &endpoint, &headers).await? {
            Some(response) => {
                let remote_id = response
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let remote_livemode = response
                    .get("livemode")
                    .and_then(serde_json::Value::as_bool);
                if remote_id != stripe_product_id || remote_livemode != Some(livemode) {
                    return Err(WaferError::new(
                        wafer_run::ErrorCode::Internal,
                        "stored Stripe Product did not match the configured account and mode",
                    ));
                }
                let form = stripe_product_form(product);
                let form_hash = sha256_hex(encode_form(form.clone()).as_bytes());
                let idempotency_key = format!(
                    "impresspress_product_sync_{}_{}",
                    product.id,
                    &form_hash[..16]
                );
                let headers = stripe_catalog_headers(
                    &stripe_key,
                    &api_version,
                    &stripe_account_id,
                    Some(&idempotency_key),
                );
                let updated = stripe_catalog_post(ctx, &endpoint, &headers, form).await?;
                stripe_product_id =
                    validate_stripe_product(&updated, Some(&stripe_product_id), livemode)?;
            }
            None => {
                stripe_product_id.clear();
                product_replaced = true;
            }
        }
    }
    if stripe_product_id.is_empty() {
        let idempotency_key = if stale_product_id.is_empty() {
            format!("impresspress_product_{}", product.id)
        } else {
            let stale_hash = sha256_hex(stale_product_id.as_bytes());
            format!(
                "impresspress_product_{}_repair_{}",
                product.id,
                &stale_hash[..16]
            )
        };
        let headers = stripe_catalog_headers(
            &stripe_key,
            &api_version,
            &stripe_account_id,
            Some(&idempotency_key),
        );
        let response = stripe_catalog_post(
            ctx,
            &format!("{}/v1/products", api_url.trim_end_matches('/')),
            &headers,
            stripe_product_form(product),
        )
        .await?;
        stripe_product_id = validate_stripe_product(&response, None, livemode)?;
        db::update(
            ctx,
            PRODUCTS_TABLE,
            &product.id,
            HashMap::from([(
                "stripe_product_id".to_string(),
                serde_json::json!(&stripe_product_id),
            )]),
        )
        .await?;
    }

    let mut fixed_price_ids = Vec::new();
    for component in &managed.offer.components {
        let AmountRule::Fixed { unit_amount_minor } = component.amount else {
            continue;
        };
        let stale_price_id = component.stripe_price_id.clone();
        let mut price_id = if product_replaced {
            String::new()
        } else {
            stale_price_id.clone()
        };
        if !price_id.is_empty() {
            if !price_id.starts_with("price_") {
                return Err(WaferError::new(
                    wafer_run::ErrorCode::FailedPrecondition,
                    "stored Stripe Price id is invalid",
                ));
            }
            let headers =
                stripe_catalog_headers(&stripe_key, &api_version, &stripe_account_id, None);
            let endpoint = format!("{}/v1/prices/{price_id}", api_url.trim_end_matches('/'));
            match stripe_catalog_get(ctx, &endpoint, &headers).await? {
                Some(response) => {
                    let active = response.get("active").and_then(serde_json::Value::as_bool);
                    if active == Some(false) {
                        let idempotency_key = format!(
                            "impresspress_price_reactivate_{}_{}",
                            component.id,
                            &sha256_hex(price_id.as_bytes())[..16]
                        );
                        let headers = stripe_catalog_headers(
                            &stripe_key,
                            &api_version,
                            &stripe_account_id,
                            Some(&idempotency_key),
                        );
                        let reactivated = stripe_catalog_post(
                            ctx,
                            &endpoint,
                            &headers,
                            vec![("active".to_string(), "true".to_string())],
                        )
                        .await?;
                        price_id = validate_stripe_price(
                            &reactivated,
                            Some(&price_id),
                            &stripe_product_id,
                            &managed.offer,
                            unit_amount_minor,
                            livemode,
                            true,
                        )?;
                    } else {
                        price_id = validate_stripe_price(
                            &response,
                            Some(&price_id),
                            &stripe_product_id,
                            &managed.offer,
                            unit_amount_minor,
                            livemode,
                            true,
                        )?;
                    }
                }
                None => price_id.clear(),
            }
        }
        if price_id.is_empty() {
            let idempotency_key = if stale_price_id.is_empty() {
                format!(
                    "impresspress_price_{}_v{}",
                    component.id, managed.offer.version
                )
            } else {
                format!(
                    "impresspress_price_{}_v{}_repair_{}",
                    component.id,
                    managed.offer.version,
                    &sha256_hex(stale_price_id.as_bytes())[..16]
                )
            };
            let headers = stripe_catalog_headers(
                &stripe_key,
                &api_version,
                &stripe_account_id,
                Some(&idempotency_key),
            );
            let mut form = vec![
                (
                    "currency".to_string(),
                    managed.offer.currency.to_ascii_lowercase(),
                ),
                ("unit_amount".to_string(), unit_amount_minor.to_string()),
                ("product".to_string(), stripe_product_id.clone()),
                ("nickname".to_string(), component.label.clone()),
                (
                    "tax_behavior".to_string(),
                    wire_enum(&managed.offer.tax_behavior).map_err(|message| {
                        WaferError::new(wafer_run::ErrorCode::Internal, message)
                    })?,
                ),
                (
                    "metadata[impresspress_offer_id]".to_string(),
                    managed.offer.id.clone(),
                ),
                (
                    "metadata[impresspress_offer_version]".to_string(),
                    managed.offer.version.to_string(),
                ),
                (
                    "metadata[impresspress_component_key]".to_string(),
                    component.key.clone(),
                ),
            ];
            if managed.offer.mode == OfferMode::Subscription {
                let interval = managed.offer.recurring_interval.as_ref().ok_or_else(|| {
                    WaferError::new(
                        wafer_run::ErrorCode::InvalidArgument,
                        "subscription offer is missing recurrence",
                    )
                })?;
                form.extend([
                    (
                        "recurring[interval]".to_string(),
                        wire_enum(interval).map_err(|message| {
                            WaferError::new(wafer_run::ErrorCode::Internal, message)
                        })?,
                    ),
                    (
                        "recurring[interval_count]".to_string(),
                        managed.offer.interval_count.to_string(),
                    ),
                    (
                        "recurring[usage_type]".to_string(),
                        wire_enum(&managed.offer.usage_type).map_err(|message| {
                            WaferError::new(wafer_run::ErrorCode::Internal, message)
                        })?,
                    ),
                ]);
            }
            let response = stripe_catalog_post(
                ctx,
                &format!("{}/v1/prices", api_url.trim_end_matches('/')),
                &headers,
                form,
            )
            .await?;
            price_id = validate_stripe_price(
                &response,
                None,
                &stripe_product_id,
                &managed.offer,
                unit_amount_minor,
                livemode,
                true,
            )?;
            repo::offer_components::set_stripe_price_id(ctx, &component.id, &price_id).await?;
        }
        fixed_price_ids.push(price_id);
    }
    let offer_price_id = if managed.offer.components.len() == 1 {
        fixed_price_ids.first().cloned().unwrap_or_default()
    } else {
        String::new()
    };
    repo::offers::mark_synced(ctx, &managed.offer.id, &stripe_product_id, &offer_price_id).await
}

pub(crate) async fn sync_offer_catalog(
    ctx: &dyn Context,
    product_id: &str,
    offer_id: &str,
) -> Result<ManagedOffer, WaferError> {
    if !stripe_secret_operations_allowed(ctx).await {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe catalog synchronization is disabled in the browser runtime",
        ));
    }
    let product = db::get(ctx, PRODUCTS_TABLE, product_id).await?;
    let managed = repo::offers::get_for_product(ctx, product_id, offer_id).await?;
    repo::offers::mark_syncing(ctx, offer_id).await?;
    match sync_offer_catalog_inner(ctx, &product, managed).await {
        Ok(synced) => Ok(synced),
        Err(error) => {
            if let Err(write_error) =
                repo::offers::mark_sync_error(ctx, offer_id, &error.message).await
            {
                tracing::error!(offer_id, error = %write_error, "could not persist Stripe sync failure");
            }
            Err(error)
        }
    }
}

async fn catalog_account_for_archive(
    ctx: &dyn Context,
    product: &Record,
) -> Result<String, WaferError> {
    if product.str_field("owner_kind") != "user" {
        return Ok(String::new());
    }
    let seller = repo::seller_accounts::get_for_user(ctx, product.str_field("owner_id"))
        .await?
        .ok_or_else(|| {
            WaferError::new(
                wafer_run::ErrorCode::FailedPrecondition,
                "seller Stripe account is not available for catalog archival",
            )
        })?;
    let account_id = seller.str_field("stripe_account_id");
    if !account_id.starts_with("acct_") {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "seller Stripe account is not available for catalog archival",
        ));
    }
    Ok(account_id.to_string())
}

pub(crate) async fn archive_offer_catalog(
    ctx: &dyn Context,
    product_id: &str,
    offer_id: &str,
) -> Result<ManagedOffer, WaferError> {
    let managed = repo::offers::get_for_product(ctx, product_id, offer_id).await?;
    if managed.status == OfferStatus::Archived {
        return Ok(managed);
    }
    let synced_components = managed
        .offer
        .components
        .iter()
        .filter_map(|component| match component.amount {
            AmountRule::Fixed { unit_amount_minor }
                if component.stripe_price_id.starts_with("price_") =>
            {
                Some((component, unit_amount_minor))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let active_links = repo::payment_links::list_for_offer(ctx, offer_id)
        .await?
        .into_iter()
        .filter(|link| link.active)
        .collect::<Vec<_>>();
    if synced_components.is_empty() && active_links.is_empty() {
        return repo::offers::archive(ctx, product_id, offer_id).await;
    }
    for link in active_links {
        deactivate_payment_link(ctx, offer_id, &link.id).await?;
    }
    if synced_components.is_empty() {
        return repo::offers::archive(ctx, product_id, offer_id).await;
    }
    if !stripe_secret_operations_allowed(ctx).await {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe catalog archival is disabled in the browser runtime",
        ));
    }
    let product = db::get(ctx, PRODUCTS_TABLE, product_id).await?;
    let stripe_key = config::get(ctx, "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY").await?;
    let livemode = stripe_client::secret_livemode(&stripe_key).ok_or_else(|| {
        WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe secret key must be configured before synced offers can be archived",
        )
    })?;
    let api_version = config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__STRIPE_API_VERSION",
        DEFAULT_STRIPE_API_VERSION,
    )
    .await;
    if !is_stable_stripe_api_version(&api_version) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe API version must be a stable named release",
        ));
    }
    let api_url = config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__STRIPE_API_URL",
        "https://api.stripe.com",
    )
    .await;
    let stripe_account_id = catalog_account_for_archive(ctx, &product).await?;
    let stripe_product_id = if product.str_field("stripe_product_id").is_empty() {
        managed.offer.stripe_product_id.as_str()
    } else {
        product.str_field("stripe_product_id")
    };
    if !stripe_product_id.starts_with("prod_") {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "synced offer is missing its Stripe Product id",
        ));
    }

    for (component, unit_amount_minor) in synced_components {
        let endpoint = format!(
            "{}/v1/prices/{}",
            api_url.trim_end_matches('/'),
            component.stripe_price_id
        );
        let headers = stripe_catalog_headers(&stripe_key, &api_version, &stripe_account_id, None);
        let Some(remote) = stripe_catalog_get(ctx, &endpoint, &headers).await? else {
            continue;
        };
        let active = remote
            .get("active")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| {
                WaferError::new(
                    wafer_run::ErrorCode::Internal,
                    "Stripe Price archival response did not include active state",
                )
            })?;
        validate_stripe_price(
            &remote,
            Some(&component.stripe_price_id),
            stripe_product_id,
            &managed.offer,
            unit_amount_minor,
            livemode,
            active,
        )?;
        if !active {
            continue;
        }
        let idempotency_key = format!(
            "impresspress_price_archive_{}_{}",
            component.id,
            &sha256_hex(component.stripe_price_id.as_bytes())[..16]
        );
        let headers = stripe_catalog_headers(
            &stripe_key,
            &api_version,
            &stripe_account_id,
            Some(&idempotency_key),
        );
        let archived = stripe_catalog_post(
            ctx,
            &endpoint,
            &headers,
            vec![("active".to_string(), "false".to_string())],
        )
        .await?;
        validate_stripe_price(
            &archived,
            Some(&component.stripe_price_id),
            stripe_product_id,
            &managed.offer,
            unit_amount_minor,
            livemode,
            false,
        )?;
    }
    repo::offers::archive(ctx, product_id, offer_id).await
}

#[allow(clippy::too_many_arguments)]
fn payment_link_form(
    offer: &Offer,
    preview: &crate::blocks::products::contracts::PricingPreview,
    product_name: &str,
    local_link_id: &str,
    preset_id: &str,
    after_completion_url: Option<&str>,
    automatic_tax: bool,
    country: &str,
    fee_minor: i64,
    fee_basis_points: u16,
) -> Result<String, String> {
    let included: Vec<_> = preview
        .components
        .iter()
        .filter(|component| component.included)
        .collect();
    if included.is_empty() || included.len() > 20 {
        return Err("Payment Links require between 1 and 20 included line items".to_string());
    }
    let mut pairs = Vec::new();
    push_form(
        &mut pairs,
        "metadata[impresspress_payment_link_id]",
        local_link_id,
    );
    push_form(&mut pairs, "metadata[offer_id]", &offer.id);
    push_form(&mut pairs, "metadata[offer_version]", offer.version);
    if !preset_id.is_empty() {
        push_form(&mut pairs, "metadata[preset_id]", preset_id);
    }
    match after_completion_url {
        Some(url) => {
            push_form(&mut pairs, "after_completion[type]", "redirect");
            push_form(&mut pairs, "after_completion[redirect][url]", url);
        }
        None => push_form(&mut pairs, "after_completion[type]", "hosted_confirmation"),
    }
    if automatic_tax {
        push_form(&mut pairs, "automatic_tax[enabled]", "true");
    }
    if offer.checkout.allow_promotion_codes {
        push_form(&mut pairs, "allow_promotion_codes", "true");
    }
    if offer.checkout.collect_billing_address {
        push_form(&mut pairs, "billing_address_collection", "required");
    }
    push_shipping_address_collection(&mut pairs, offer, country);
    payment_link_shipping_supported(offer)?;
    for (index, option) in offer.checkout.shipping_options.iter().enumerate() {
        push_form(
            &mut pairs,
            format!("shipping_options[{index}][shipping_rate]"),
            option.stripe_shipping_rate_id.trim(),
        );
    }
    if offer.checkout.create_customer && matches!(offer.mode, OfferMode::Payment) {
        push_form(&mut pairs, "customer_creation", "always");
    }
    if offer.checkout.require_terms_consent {
        push_form(
            &mut pairs,
            "consent_collection[terms_of_service]",
            "required",
        );
    }
    if matches!(offer.mode, OfferMode::Subscription) && offer.checkout.trial_days > 0 {
        push_form(
            &mut pairs,
            "subscription_data[trial_period_days]",
            offer.checkout.trial_days,
        );
    }
    if fee_minor > 0 {
        match offer.mode {
            OfferMode::Payment => push_form(&mut pairs, "application_fee_amount", fee_minor),
            OfferMode::Subscription => {
                let mut percentage =
                    format!("{}.{:02}", fee_basis_points / 100, fee_basis_points % 100);
                while percentage.ends_with('0') {
                    percentage.pop();
                }
                if percentage.ends_with('.') {
                    percentage.pop();
                }
                push_form(&mut pairs, "application_fee_percent", percentage);
            }
        }
    }

    let currency = preview.amounts.currency.to_ascii_lowercase();
    let tax_behavior = wire_enum(&offer.tax_behavior)?;
    let recurring_interval = offer
        .recurring_interval
        .as_ref()
        .map(wire_enum)
        .transpose()?;
    for (index, component) in included.into_iter().enumerate() {
        let prefix = format!("line_items[{index}]");
        if let Some(price_id) = synced_component_price(offer, component) {
            push_form(&mut pairs, format!("{prefix}[price]"), price_id);
        } else {
            push_form(
                &mut pairs,
                format!("{prefix}[price_data][currency]"),
                &currency,
            );
            push_form(
                &mut pairs,
                format!("{prefix}[price_data][unit_amount]"),
                component.unit_amount_minor,
            );
            push_form(
                &mut pairs,
                format!("{prefix}[price_data][product_data][name]"),
                format!("{product_name} — {}", component.label),
            );
            push_form(
                &mut pairs,
                format!("{prefix}[price_data][tax_behavior]"),
                &tax_behavior,
            );
            if matches!(offer.mode, OfferMode::Subscription) {
                push_form(
                    &mut pairs,
                    format!("{prefix}[price_data][recurring][interval]"),
                    recurring_interval
                        .as_deref()
                        .ok_or_else(|| "subscription offer is missing recurrence".to_string())?,
                );
                push_form(
                    &mut pairs,
                    format!("{prefix}[price_data][recurring][interval_count]"),
                    offer.interval_count,
                );
            }
        }
        push_form(
            &mut pairs,
            format!("{prefix}[quantity]"),
            component.quantity,
        );
    }
    Ok(encode_form(pairs))
}

/// Create or reuse a shareable Payment Link for an immutable active offer and
/// optional validated preset. Arbitrary runtime inputs are intentionally not
/// accepted because a reusable URL must always resolve to the same price.
pub(crate) async fn create_payment_link(
    ctx: &dyn Context,
    product: &Record,
    offer_id: &str,
    request: &PaymentLinkCreateRequest,
) -> Result<ManagedPaymentLink, WaferError> {
    if !stripe_secret_operations_allowed(ctx).await {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe Payment Link creation is disabled in the browser runtime",
        ));
    }
    let stripe_key = config::get(ctx, "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY").await?;
    if stripe_key.trim().is_empty() {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe is not configured",
        ));
    }
    let livemode = stripe_client::secret_livemode(&stripe_key).ok_or_else(|| {
        WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe secret key is malformed; expected an sk_test_ or sk_live_ key",
        )
    })?;
    let api_version = config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__STRIPE_API_VERSION",
        DEFAULT_STRIPE_API_VERSION,
    )
    .await;
    if !is_stable_stripe_api_version(&api_version) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe API version must be stable",
        ));
    }
    let managed = repo::offers::get_for_product(ctx, &product.id, offer_id).await?;
    if managed.status != OfferStatus::Active {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Payment Links require an active immutable offer",
        ));
    }
    let offer = managed.offer;
    let (preset_id, inputs, preset_hash) = match request.preset_id.as_deref() {
        Some(preset_id) => {
            let preset = repo::checkout_presets::get_active(ctx, offer_id, preset_id).await?;
            (preset.id, preset.inputs, preset.configuration_hash)
        }
        None => (String::new(), Default::default(), String::new()),
    };
    let preview = offer_pricing::evaluate_offer(
        &offer,
        &PricingPreviewRequest {
            offer_id: offer.id.clone(),
            quantity: 1,
            inputs,
        },
    )
    .map_err(|error| {
        WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            format!("offer requires a named preset before it can become a Payment Link: {error}"),
        )
    })?;
    payment_link_shipping_supported(&offer)
        .map_err(|error| WaferError::new(wafer_run::ErrorCode::InvalidArgument, error))?;
    let after_completion_url = request.after_completion_url.as_deref();
    if let Some(url) = after_completion_url {
        let base_url = config::get_default(
            ctx,
            "WAFER_RUN_SHARED__FRONTEND_URL",
            "http://localhost:5173",
        )
        .await;
        let allowed =
            config::get_default(ctx, "IMPRESSPRESS__PRODUCTS__CHECKOUT_ALLOWED_ORIGINS", "").await;
        if !is_allowed_checkout_url(url, &base_url, &allowed) {
            return Err(WaferError::new(
                wafer_run::ErrorCode::InvalidArgument,
                "after_completion_url must be on a configured checkout origin",
            ));
        }
    }
    let canonical = serde_json::to_string(&serde_json::json!({
        "offer_id": offer.id,
        "offer_version": offer.version,
        "preset_hash": preset_hash,
        "inputs": preview.inputs,
        "after_completion_url": after_completion_url,
        "livemode": livemode,
    }))
    .map_err(|error| {
        WaferError::new(
            wafer_run::ErrorCode::Internal,
            format!("could not encode Payment Link configuration: {error}"),
        )
    })?;
    let configuration_hash = wafer_block::hash::sha256_hex(canonical.as_bytes());
    if let Some(existing) =
        repo::payment_links::find_reusable(ctx, offer_id, &preset_id, &configuration_hash).await?
    {
        return Ok(existing);
    }

    let (seller_account_id, stripe_account_id, fee_basis_points) =
        payment_link_seller_context(ctx, product).await?;
    let fee_minor = application_fee(preview.amounts.total_minor, fee_basis_points)
        .map_err(|error| WaferError::new(wafer_run::ErrorCode::InvalidArgument, error))?;
    let pending = repo::payment_links::create_pending(
        ctx,
        offer_id,
        &preset_id,
        &seller_account_id,
        &stripe_account_id,
        livemode,
        &configuration_hash,
        &preview,
        fee_basis_points,
    )
    .await?;
    let automatic_tax_config =
        config::get_default(ctx, "IMPRESSPRESS__PRODUCTS__AUTOMATIC_TAX", "false").await;
    let country = platform_country(ctx).await;
    let body = payment_link_form(
        &offer,
        &preview,
        product.str_field("name"),
        &pending.managed.id,
        &preset_id,
        after_completion_url,
        offer.checkout.automatic_tax || configured_bool(&automatic_tax_config),
        &country.to_ascii_uppercase(),
        fee_minor,
        fee_basis_points,
    )
    .map_err(|error| WaferError::new(wafer_run::ErrorCode::InvalidArgument, error))?;
    let mut headers = stripe_request_headers(
        &stripe_key,
        &api_version,
        Some(&format!("impresspress_payment_link_{}", pending.managed.id)),
    );
    if !stripe_account_id.is_empty() {
        headers.insert("Stripe-Account".to_string(), stripe_account_id);
    }
    let api_url = config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__STRIPE_API_URL",
        "https://api.stripe.com",
    )
    .await;
    let endpoint = format!("{api_url}/v1/payment_links");
    let response = match stripe_client::send_raw(
        ctx,
        "POST",
        &endpoint,
        &headers,
        Some(body.as_bytes()),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            let _ = repo::payment_links::mark_error(
                ctx,
                &pending.managed.id,
                "Stripe Payment Link request failed",
            )
            .await;
            return Err(error);
        }
    };
    if response.status_code >= 400 {
        tracing::error!(
            status = response.status_code,
            body = %String::from_utf8_lossy(&response.body),
            payment_link_id = %pending.managed.id,
            "Stripe Payment Link creation failed"
        );
        let _ = repo::payment_links::mark_error(
            ctx,
            &pending.managed.id,
            "Stripe rejected the Payment Link",
        )
        .await;
        return Err(WaferError::new(
            wafer_run::ErrorCode::Internal,
            "Stripe rejected the Payment Link",
        ));
    }
    let response: serde_json::Value = serde_json::from_slice(&response.body).map_err(|_| {
        WaferError::new(
            wafer_run::ErrorCode::Internal,
            "Stripe Payment Link response could not be decoded",
        )
    })?;
    let stripe_id = response
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let url = response
        .get("url")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if stripe_id.is_empty() || url.is_empty() {
        let _ = repo::payment_links::mark_error(
            ctx,
            &pending.managed.id,
            "Stripe Payment Link response was incomplete",
        )
        .await;
        return Err(WaferError::new(
            wafer_run::ErrorCode::Internal,
            "Stripe Payment Link response was incomplete",
        ));
    }
    Ok(
        repo::payment_links::mark_synced(ctx, &pending.managed.id, stripe_id, url)
            .await?
            .managed,
    )
}

pub(crate) async fn deactivate_payment_link(
    ctx: &dyn Context,
    offer_id: &str,
    link_id: &str,
) -> Result<ManagedPaymentLink, WaferError> {
    let stored = repo::payment_links::get_for_offer(ctx, offer_id, link_id).await?;
    if !stored.managed.active {
        return Ok(stored.managed);
    }
    if stored.stripe_payment_link_id.is_empty() {
        return repo::payment_links::deactivate_local(ctx, offer_id, link_id).await;
    }
    if !stripe_secret_operations_allowed(ctx).await {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe Payment Link deactivation is disabled in the browser runtime",
        ));
    }
    let stripe_key = config::get(ctx, "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY").await?;
    let api_version = config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__STRIPE_API_VERSION",
        DEFAULT_STRIPE_API_VERSION,
    )
    .await;
    let mut headers = stripe_request_headers(
        &stripe_key,
        &api_version,
        Some(&format!("impresspress_deactivate_payment_link_{link_id}")),
    );
    if !stored.stripe_account_id.is_empty() {
        headers.insert(
            "Stripe-Account".to_string(),
            stored.stripe_account_id.clone(),
        );
    }
    let api_url = config::get_default(
        ctx,
        "IMPRESSPRESS__PRODUCTS__STRIPE_API_URL",
        "https://api.stripe.com",
    )
    .await;
    let endpoint = format!(
        "{api_url}/v1/payment_links/{}",
        crate::util::url_path_encode(&stored.stripe_payment_link_id)
    );
    let response =
        stripe_client::send_raw(ctx, "POST", &endpoint, &headers, Some(b"active=false")).await?;
    if response.status_code >= 400 {
        return Err(WaferError::new(
            wafer_run::ErrorCode::Internal,
            "Stripe rejected Payment Link deactivation",
        ));
    }
    repo::payment_links::deactivate_local(ctx, offer_id, link_id).await
}

async fn reconcile_payment_link_session(
    ctx: &dyn Context,
    local_link_id: &str,
    event_account: &str,
    event_livemode: bool,
    session: &serde_json::Value,
) -> Result<(), WaferError> {
    let session_id = session
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if session_id.is_empty() {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "Payment Link Checkout Session is missing its id",
        ));
    }
    if repo::purchases::find_by_session(ctx, session_id)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let offer_id = session
        .pointer("/metadata/offer_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let stored = repo::payment_links::get_for_offer(ctx, offer_id, local_link_id).await?;
    if stored.stripe_account_id != event_account {
        return Err(WaferError::new(
            wafer_run::ErrorCode::PermissionDenied,
            "Payment Link webhook account does not match the configured seller",
        ));
    }
    let session_livemode = session
        .get("livemode")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            WaferError::new(
                wafer_run::ErrorCode::InvalidArgument,
                "Payment Link Checkout Session is missing livemode",
            )
        })?;
    if stored.livemode != event_livemode || session_livemode != event_livemode {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Payment Link webhook mode does not match its stored provider context",
        ));
    }
    let provider_link_id = session
        .get("payment_link")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if provider_link_id != stored.stripe_payment_link_id {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "Checkout Session does not belong to the expected Payment Link",
        ));
    }
    let mut pricing = stored.pricing_snapshot.ok_or_else(|| {
        WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Payment Link is missing its immutable pricing snapshot",
        )
    })?;
    let managed_offer = repo::offers::get_managed(ctx, offer_id).await?;
    if managed_offer.offer.version != pricing.offer_version {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Payment Link offer version no longer matches its immutable quote",
        ));
    }
    let offer = managed_offer.offer;
    let currency = session
        .get("currency")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_ascii_uppercase();
    if currency != pricing.amounts.currency {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "Payment Link Checkout Session currency does not match the expected quote",
        ));
    }
    let subtotal = session
        .get("amount_subtotal")
        .and_then(|value| value.as_i64())
        .ok_or_else(|| {
            WaferError::new(
                wafer_run::ErrorCode::InvalidArgument,
                "Payment Link Checkout Session is missing amount_subtotal",
            )
        })?;
    if subtotal != pricing.amounts.subtotal_minor {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "Payment Link Checkout Session subtotal does not match the immutable quote",
        ));
    }
    let discount = session
        .pointer("/total_details/amount_discount")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let tax = session
        .pointer("/total_details/amount_tax")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let shipping = session
        .pointer("/total_details/amount_shipping")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    if shipping < 0 || !shipping_amount_is_allowed(&offer, shipping) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "Payment Link Checkout Session shipping amount is not allowed by its immutable offer",
        ));
    }
    let total = session
        .get("amount_total")
        .and_then(|value| value.as_i64())
        .ok_or_else(|| {
            WaferError::new(
                wafer_run::ErrorCode::InvalidArgument,
                "Payment Link Checkout Session is missing amount_total",
            )
        })?;
    let expected_total = subtotal
        .checked_sub(discount)
        .and_then(|value| value.checked_add(tax))
        .and_then(|value| value.checked_add(shipping))
        .ok_or_else(|| {
            WaferError::new(
                wafer_run::ErrorCode::InvalidArgument,
                "Payment Link Checkout Session amount breakdown overflowed",
            )
        })?;
    if total != expected_total {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "Payment Link Checkout Session amount breakdown is inconsistent",
        ));
    }
    let session_offer_version = session
        .pointer("/metadata/offer_version")
        .and_then(crate::util::json_as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or_default();
    if session_offer_version != offer.version {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "Payment Link Checkout Session offer version does not match its immutable quote",
        ));
    }
    let expected_mode = wire_enum(&offer.mode)
        .map_err(|error| WaferError::new(wafer_run::ErrorCode::Internal, error))?;
    let session_mode = session
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if session_mode != expected_mode {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "Payment Link Checkout Session mode does not match its offer",
        ));
    }
    let payment_status = session
        .get("payment_status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if !matches!(payment_status, "paid" | "no_payment_required") {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Payment Link Checkout Session payment is not complete",
        ));
    }
    let product = db::get(ctx, PRODUCTS_TABLE, &offer.product_id).await?;
    pricing.amounts.discount_minor = discount;
    pricing.amounts.tax_minor = tax;
    pricing.amounts.shipping_minor = shipping;
    pricing.amounts.total_minor = total;
    pricing.amounts.platform_fee_minor = match offer.mode {
        OfferMode::Payment => {
            application_fee(pricing.amounts.subtotal_minor, stored.fee_basis_points)
        }
        OfferMode::Subscription => application_fee(total, stored.fee_basis_points),
    }
    .map_err(|error| WaferError::new(wafer_run::ErrorCode::InvalidArgument, error))?;
    let input_snapshot = serde_json::to_string(&pricing.inputs).map_err(|error| {
        WaferError::new(
            wafer_run::ErrorCode::Internal,
            format!("could not encode Payment Link inputs: {error}"),
        )
    })?;
    let mut items = Vec::new();
    for resolved in pricing
        .components
        .iter()
        .filter(|component| component.included)
    {
        let component = offer
            .components
            .iter()
            .find(|component| component.id == resolved.component_id)
            .ok_or_else(|| {
                WaferError::new(
                    wafer_run::ErrorCode::FailedPrecondition,
                    "Payment Link component no longer matches its immutable offer",
                )
            })?;
        items.push(repo::purchases::CheckoutLineSnapshot {
            product_id: offer.product_id.clone(),
            product_name: format!("{} — {}", product.str_field("name"), resolved.label),
            offer_id: offer.id.clone(),
            offer_version: offer.version,
            component_id: resolved.component_id.clone(),
            quantity: resolved.quantity,
            unit_amount_minor: resolved.unit_amount_minor,
            total_amount_minor: resolved.total_amount_minor,
            input_snapshot: input_snapshot.clone(),
            condition_snapshot: serde_json::to_string(&component.condition).map_err(|error| {
                WaferError::new(
                    wafer_run::ErrorCode::Internal,
                    format!("could not encode Payment Link condition: {error}"),
                )
            })?,
        });
    }
    let buyer_email = session
        .pointer("/customer_details/email")
        .or_else(|| session.get("customer_email"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let order = repo::purchases::create_checkout_order(
        ctx,
        repo::purchases::CheckoutOrderSnapshot {
            buyer_user_id: String::new(),
            buyer_email: buyer_email.to_string(),
            seller_account_id: stored.seller_account_id,
            stripe_account_id: stored.stripe_account_id,
            presentation: CheckoutPresentation::PaymentLink,
            mode: offer.mode,
            offer_id: offer.id.clone(),
            offer_version: offer.version,
            livemode: stored.livemode,
            receipt_token_hash: String::new(),
            receipt_token_expires_at: None,
            allowed_shipping_amounts_minor: allowed_shipping_amounts(&offer),
            amounts: pricing.amounts,
            items,
        },
    )
    .await?;
    repo::purchases::update(
        ctx,
        &order.id,
        HashMap::from([(
            "provider_session_id".to_string(),
            serde_json::json!(session_id),
        )]),
    )
    .await?;
    let payment_intent = session
        .get("payment_intent")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let customer = session
        .get("customer")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let subscription = session
        .get("subscription")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if matches!(offer.mode, OfferMode::Payment)
        && payment_intent.is_empty()
        && payment_status != "no_payment_required"
    {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "Paid Payment Link Checkout is missing its PaymentIntent",
        ));
    }
    if matches!(offer.mode, OfferMode::Subscription) && subscription.is_empty() {
        return Err(WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            "Subscription Payment Link Checkout is missing its Subscription",
        ));
    }
    let rows = repo::purchases::complete_checkout_atomic(
        ctx,
        &order.id,
        payment_intent,
        customer,
        subscription,
        stored.livemode,
    )
    .await?;
    if rows != 1 {
        return Err(WaferError::new(
            wafer_run::ErrorCode::Aborted,
            "Payment Link order could not be completed atomically",
        ));
    }
    if !subscription.is_empty() {
        repo::subscription_items::snapshot_from_purchase(ctx, &order.id, subscription).await?;
    }
    Ok(())
}

fn stripe_timestamp(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        Some(serde_json::Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(value) => value.as_i64().and_then(|seconds| {
            chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0)
                .map(|value| value.to_rfc3339())
        }),
        None => None,
    }
}

fn stripe_resource_id(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(|value| {
            value
                .as_str()
                .or_else(|| value.get("id").and_then(serde_json::Value::as_str))
        })
        .unwrap_or("")
        .to_string()
}

fn invoice_subscription_id(invoice: &serde_json::Value) -> String {
    let direct = stripe_resource_id(invoice.get("subscription"));
    if direct.is_empty() {
        stripe_resource_id(invoice.pointer("/parent/subscription_details/subscription"))
    } else {
        direct
    }
}

fn checkout_session_completion(
    event_account: &str,
    session: &serde_json::Value,
) -> repo::purchases::CheckoutSessionCompletion {
    repo::purchases::CheckoutSessionCompletion {
        session_id: stripe_resource_id(session.get("id")),
        client_reference_id: session
            .get("client_reference_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        event_account: event_account.to_string(),
        livemode: session
            .get("livemode")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        mode: session
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        payment_status: session
            .get("payment_status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        currency: session
            .get("currency")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        subtotal_minor: session
            .get("amount_subtotal")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(-1),
        discount_minor: session
            .pointer("/total_details/amount_discount")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default(),
        tax_minor: session
            .pointer("/total_details/amount_tax")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default(),
        shipping_minor: session
            .pointer("/total_details/amount_shipping")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default(),
        total_minor: session
            .get("amount_total")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(-1),
        offer_id: session
            .pointer("/metadata/offer_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        offer_version: session
            .pointer("/metadata/offer_version")
            .and_then(crate::util::json_as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_default(),
        payment_intent_id: stripe_resource_id(session.get("payment_intent")),
        customer_id: stripe_resource_id(session.get("customer")),
        subscription_id: stripe_resource_id(session.get("subscription")),
    }
}

fn bounded_provider_diagnostic(value: Option<&serde_json::Value>, limit: usize) -> String {
    value
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .chars()
        .filter_map(|character| {
            if character.is_control() {
                character.is_whitespace().then_some(' ')
            } else {
                Some(character)
            }
        })
        .take(limit)
        .collect::<String>()
        .trim()
        .to_string()
}

pub async fn handle_webhook(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    if !stripe_secret_operations_allowed(ctx).await {
        return err_forbidden("Stripe webhooks are disabled in the browser runtime");
    }
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
    let event_account = event
        .get("account")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let event_livemode = event
        .get("livemode")
        .or_else(|| data_object.get("livemode"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let event_created = event
        .get("created")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default()
        .max(0);

    // Idempotency: persist the top-level Stripe event id under a UNIQUE
    // constraint BEFORE running any side effect, as `status = "pending"`.
    // Stripe retries undelivered/non-2xx webhooks, and the signature
    // timestamp window above itself accepts up to 5 minutes of replay —
    // both redeliver the same `id`. Real Stripe events always carry one; a
    // signed body without one (synthetic/malformed) can't be deduped, so
    // it's processed as-is rather than rejected outright — the signature
    // already establishes it came from a holder of the webhook secret.
    //
    let event_id = event.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let mut event_lease: Option<(String, u64)> = None;
    if !event_id.is_empty() {
        match record_event(
            ctx,
            event_id,
            event_type,
            &raw_body,
            event_account,
            event_livemode,
        )
        .await
        {
            Ok(EventRecordState::Claimed { owner, attempts }) => {
                if attempts > 1 {
                    tracing::info!(
                        event_id = %event_id,
                        event_type = %event_type,
                        attempts,
                        "re-processing a failed or expired Stripe webhook lease"
                    );
                }
                event_lease = Some((owner, attempts));
            }
            Ok(EventRecordState::InFlight) => {
                tracing::info!(
                    event_id = %event_id,
                    event_type = %event_type,
                    "concurrent Stripe webhook delivery — existing processing lease retained"
                );
                return err_internal_no_cause(
                    "Stripe webhook event is already being processed; retry this delivery",
                );
            }
            Ok(EventRecordState::RetryScheduled) => {
                return err_internal_no_cause(
                    "Stripe webhook retry is scheduled after a prior processing failure",
                );
            }
            Ok(EventRecordState::AlreadyProcessed) => {
                tracing::info!(
                    event_id = %event_id,
                    event_type = %event_type,
                    "duplicate Stripe webhook event — skipping side effects"
                );
                return ok_json(&serde_json::json!({"received": true, "duplicate": true}));
            }
            Ok(EventRecordState::DeadLetter) => {
                tracing::error!(
                    event_id = %event_id,
                    event_type = %event_type,
                    "Stripe webhook event exhausted its retry budget"
                );
                return ok_json(&serde_json::json!({
                    "received": true,
                    "dead_letter": true
                }));
            }
            Err(e) => return err_internal("Failed to record webhook event", e),
        }
    } else {
        tracing::warn!(
            event_type = %event_type,
            "Stripe webhook event missing top-level id — cannot dedupe replay/retry for this delivery"
        );
    }

    macro_rules! fail_webhook {
        ($response:expr, $message:expr) => {{
            if let Some((owner, attempts)) = event_lease.as_ref() {
                if let Err(error) =
                    mark_event_failed(ctx, event_id, owner, *attempts, $message).await
                {
                    tracing::error!(
                        event_id = %event_id,
                        error = %error,
                        "failed to release Stripe webhook processing lease"
                    );
                }
            }
            return $response;
        }};
    }

    match event_type {
        "account.updated" => {
            let account_id = data_object
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if !event_account.is_empty() && event_account != account_id {
                fail_webhook!(
                    err_internal_no_cause(
                        "Connected-account webhook identity does not match its account object",
                    ),
                    "connected-account identity mismatch"
                );
            }
            let livemode = event
                .get("livemode")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if let Err(error) =
                stripe_provider::sync_connected_account(ctx, &data_object, livemode, event_created)
                    .await
            {
                fail_webhook!(
                    err_internal("Failed to synchronize connected account", error),
                    "connected-account synchronization failed"
                );
            }
        }
        "checkout.session.completed"
            if data_object
                .get("payment_status")
                .and_then(serde_json::Value::as_str)
                == Some("unpaid") =>
        {
            // Delayed payment methods complete Checkout before the funds have
            // settled. Persist this delivery as processed, but do not grant
            // access or create an order until Stripe reports the final result.
            tracing::info!(
                event_id = %event_id,
                session_id = %stripe_resource_id(data_object.get("id")),
                "Checkout Session is awaiting asynchronous payment confirmation"
            );
        }
        "checkout.session.completed" | "checkout.session.async_payment_succeeded" => {
            // Handle product purchase completion
            let purchase_id = data_object
                .pointer("/metadata/purchase_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if !purchase_id.is_empty() {
                let completion = checkout_session_completion(event_account, &data_object);
                let stripe_subscription_id = completion.subscription_id.clone();
                let rows = match repo::purchases::reconcile_checkout_session(
                    ctx,
                    purchase_id,
                    &completion,
                )
                .await
                {
                    Ok(rows) => rows,
                    Err(error) => fail_webhook!(
                        err_internal("Failed to reconcile checkout purchase", error),
                        "checkout session did not match its immutable order"
                    ),
                };
                if rows == 0 {
                    tracing::warn!(
                        "Purchase {} not updated — already completed or refunded",
                        purchase_id
                    );
                } else if !stripe_subscription_id.is_empty() {
                    if let Err(error) = repo::subscription_items::snapshot_from_purchase(
                        ctx,
                        purchase_id,
                        &stripe_subscription_id,
                    )
                    .await
                    {
                        fail_webhook!(
                            err_internal("Failed to snapshot subscription items", error),
                            "subscription item snapshot failed"
                        );
                    }
                }
            } else if let Some(local_link_id) = data_object
                .pointer("/metadata/impresspress_payment_link_id")
                .and_then(|value| value.as_str())
            {
                if let Err(error) = reconcile_payment_link_session(
                    ctx,
                    local_link_id,
                    event_account,
                    event_livemode,
                    &data_object,
                )
                .await
                {
                    fail_webhook!(
                        err_internal("Failed to reconcile Payment Link order", error),
                        "Payment Link reconciliation failed"
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
                if let Err(error) = repo::subscriptions::upsert_platform(
                    ctx,
                    user_id,
                    stripe_customer_id,
                    stripe_sub_id,
                    plan,
                    event_created,
                )
                .await
                {
                    fail_webhook!(
                        err_internal("Failed to create platform subscription", error),
                        "platform subscription upsert failed"
                    );
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

        "checkout.session.async_payment_failed" => {
            let purchase_id = data_object
                .pointer("/metadata/purchase_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if !purchase_id.is_empty() {
                let completion = checkout_session_completion(event_account, &data_object);
                let rows = match repo::purchases::reconcile_checkout_failure(
                    ctx,
                    purchase_id,
                    &completion,
                    "Stripe delayed payment failed",
                )
                .await
                {
                    Ok(rows) => rows,
                    Err(error) => fail_webhook!(
                        err_internal("Failed to reconcile checkout payment failure", error),
                        "checkout failure did not match its immutable order"
                    ),
                };
                if rows == 0 {
                    tracing::warn!(
                        purchase_id = %purchase_id,
                        "Checkout payment failure did not update an already-terminal order"
                    );
                }
            } else {
                // Reusable Payment Links do not create local pending orders.
                // A failed attempt therefore has nothing local to transition.
                tracing::info!(
                    event_id = %event_id,
                    session_id = %stripe_resource_id(data_object.get("id")),
                    "Payment Link asynchronous payment failed before a local order was created"
                );
            }
        }

        "payment_intent.succeeded"
        | "payment_intent.payment_failed"
        | "payment_intent.processing"
        | "payment_intent.requires_action"
        | "payment_intent.canceled" => {
            let payment_intent_id = stripe_resource_id(data_object.get("id"));
            let object_status = data_object
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let status = match event_type {
                "payment_intent.succeeded" if object_status == "succeeded" => "succeeded",
                "payment_intent.payment_failed" => "payment_failed",
                "payment_intent.processing" if object_status == "processing" => "processing",
                "payment_intent.requires_action" if object_status == "requires_action" => {
                    "requires_action"
                }
                "payment_intent.canceled" if object_status == "canceled" => "canceled",
                _ => fail_webhook!(
                    err_internal_no_cause(
                        "PaymentIntent event type does not match its object status",
                    ),
                    "PaymentIntent event/object status mismatch"
                ),
            };
            if let Some(object_livemode) = data_object
                .get("livemode")
                .and_then(serde_json::Value::as_bool)
            {
                if object_livemode != event_livemode {
                    fail_webhook!(
                        err_internal_no_cause(
                            "PaymentIntent event and object test/live modes do not match",
                        ),
                        "PaymentIntent event/object mode mismatch"
                    );
                }
            }
            let failure = data_object.get("last_payment_error");
            let error_code = bounded_provider_diagnostic(
                failure
                    .and_then(|error| error.get("code"))
                    .or_else(|| failure.and_then(|error| error.get("decline_code"))),
                100,
            );
            let error_message =
                bounded_provider_diagnostic(failure.and_then(|error| error.get("message")), 500);
            let snapshot = repo::purchases::PaymentIntentSnapshot {
                purchase_id: data_object
                    .pointer("/metadata/purchase_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                offer_id: data_object
                    .pointer("/metadata/offer_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                offer_version: data_object
                    .pointer("/metadata/offer_version")
                    .and_then(crate::util::json_as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_default(),
                payment_intent_id,
                stripe_account_id: event_account.to_string(),
                livemode: event_livemode,
                status: status.to_string(),
                amount_minor: data_object
                    .get("amount")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(-1),
                currency: data_object
                    .get("currency")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                error_code,
                error_message,
                event_created,
            };
            match repo::purchases::sync_payment_intent(ctx, &snapshot).await {
                Ok(Some(_)) => {}
                Ok(None) => tracing::info!(
                    payment_intent_id = %snapshot.payment_intent_id,
                    "PaymentIntent event has no matching typed payment-mode order"
                ),
                Err(error) => fail_webhook!(
                    err_internal("Failed to reconcile PaymentIntent", error),
                    "PaymentIntent reconciliation failed"
                ),
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
            if !stripe_sub_id.is_empty() && !status.is_empty() {
                let current_period_end = stripe_timestamp(data_object.get("current_period_end"));
                let canceled_at = stripe_timestamp(data_object.get("canceled_at"));
                if let Err(error) = repo::purchases::sync_commerce_subscription(
                    ctx,
                    stripe_sub_id,
                    event_account,
                    event_livemode,
                    status,
                    current_period_end.as_deref(),
                    Some(
                        data_object
                            .get("cancel_at_period_end")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false),
                    ),
                    canceled_at.as_deref(),
                    None,
                    event_created,
                )
                .await
                {
                    fail_webhook!(
                        err_internal("Failed to synchronize commerce subscription", error),
                        "commerce subscription synchronization failed"
                    );
                }
            }
            if let Err(error) = repo::subscriptions::update_status_plan(
                ctx,
                stripe_sub_id,
                status,
                plan,
                event_created,
            )
            .await
            {
                fail_webhook!(
                    err_internal("Failed to synchronize platform subscription", error),
                    "platform subscription status/plan synchronization failed"
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

        "invoice.paid" | "invoice.payment_succeeded" => {
            let stripe_sub_id = invoice_subscription_id(&data_object);
            if !stripe_sub_id.is_empty() {
                if let Err(error) = repo::purchases::sync_commerce_subscription(
                    ctx,
                    &stripe_sub_id,
                    event_account,
                    event_livemode,
                    "active",
                    None,
                    None,
                    None,
                    Some("past_due"),
                    event_created,
                )
                .await
                {
                    fail_webhook!(
                        err_internal("Failed to recover commerce subscription", error),
                        "commerce subscription recovery write failed"
                    );
                }
                if let Err(error) = repo::subscriptions::recover_from_paid_invoice(
                    ctx,
                    &stripe_sub_id,
                    event_created,
                )
                .await
                {
                    fail_webhook!(
                        err_internal("Failed to recover subscription", error),
                        "platform subscription recovery write failed"
                    );
                }
            }
        }

        "invoice.payment_failed" => {
            let stripe_sub_id = invoice_subscription_id(&data_object);
            if !stripe_sub_id.is_empty() {
                if let Err(error) = repo::purchases::sync_commerce_subscription(
                    ctx,
                    &stripe_sub_id,
                    event_account,
                    event_livemode,
                    "past_due",
                    None,
                    None,
                    None,
                    None,
                    event_created,
                )
                .await
                {
                    fail_webhook!(
                        err_internal("Failed to mark commerce subscription past due", error),
                        "commerce subscription past-due write failed"
                    );
                }
                // Billing-critical: surface DB failures so Stripe retries.
                if let Err(e) =
                    repo::subscriptions::mark_past_due(ctx, &stripe_sub_id, event_created).await
                {
                    tracing::error!(
                        error = %e,
                        stripe_sub_id = %stripe_sub_id,
                        "marking subscription past_due failed"
                    );
                    fail_webhook!(
                        err_internal("Failed to mark subscription past_due", e),
                        "platform subscription past-due write failed"
                    );
                }
            }
        }

        "customer.subscription.deleted" => {
            let stripe_sub_id = data_object.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let user_id = repo::subscriptions::find_user_by_stripe_sub(ctx, stripe_sub_id).await;

            if !stripe_sub_id.is_empty() {
                let canceled_at = stripe_timestamp(data_object.get("canceled_at"));
                if let Err(error) = repo::purchases::sync_commerce_subscription(
                    ctx,
                    stripe_sub_id,
                    event_account,
                    event_livemode,
                    "canceled",
                    None,
                    Some(false),
                    canceled_at.as_deref(),
                    None,
                    event_created,
                )
                .await
                {
                    fail_webhook!(
                        err_internal("Failed to cancel commerce subscription", error),
                        "commerce subscription cancellation failed"
                    );
                }
            }

            // Cancellation is billing-critical — make Stripe retry on DB failure
            // so we don't leave a "cancelled in Stripe but still active here"
            // gap that grants free access to a paid user.
            if let Err(e) =
                repo::subscriptions::cancel_and_reset_addons(ctx, stripe_sub_id, event_created)
                    .await
            {
                tracing::error!(
                    error = %e,
                    stripe_sub_id = %stripe_sub_id,
                    "subscription cancellation failed"
                );
                fail_webhook!(
                    err_internal("Failed to cancel subscription", e),
                    "platform subscription cancellation failed"
                );
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

        "charge.dispute.created" | "charge.dispute.updated" | "charge.dispute.closed" => {
            let provider_dispute_id = stripe_resource_id(data_object.get("id"));
            let payment_intent_id = stripe_resource_id(data_object.get("payment_intent"));
            if provider_dispute_id.is_empty() || payment_intent_id.is_empty() {
                fail_webhook!(
                    err_internal_no_cause(
                        "Stripe dispute event is missing its dispute or PaymentIntent identity",
                    ),
                    "dispute identity was missing"
                );
            }
            let purchase =
                match repo::purchases::find_by_payment_intent(ctx, &payment_intent_id).await {
                    Ok(purchase) => purchase,
                    Err(error) if error.code == wafer_run::ErrorCode::NotFound => {
                        tracing::info!(
                            dispute_id = %provider_dispute_id,
                            payment_intent_id = %payment_intent_id,
                            "Stripe dispute does not belong to a local commerce order"
                        );
                        if let Some((owner, _)) = event_lease.as_ref() {
                            if let Err(error) = mark_event_processed(ctx, event_id, owner).await {
                                return err_internal(
                                    "Failed to complete webhook processing lease",
                                    error,
                                );
                            }
                        }
                        return ok_json(&serde_json::json!({"received": true}));
                    }
                    Err(error) => fail_webhook!(
                        err_internal("Failed to load disputed purchase", error),
                        "disputed purchase lookup failed"
                    ),
                };
            let purchase_account = purchase.str_field("stripe_account_id");
            if (!event_account.is_empty() && event_account != purchase_account)
                || (event_account.is_empty() && !purchase_account.is_empty())
            {
                fail_webhook!(
                    err_internal_no_cause("Dispute connected account does not match its purchase",),
                    "dispute connected-account mismatch"
                );
            }
            let dispute_livemode = data_object
                .get("livemode")
                .and_then(serde_json::Value::as_bool)
                .or_else(|| event.get("livemode").and_then(serde_json::Value::as_bool))
                .unwrap_or(false);
            if dispute_livemode != purchase.bool_field("livemode") {
                fail_webhook!(
                    err_internal_no_cause("Dispute mode does not match its purchase"),
                    "dispute livemode mismatch"
                );
            }
            let amount_minor = data_object
                .get("amount")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default();
            if amount_minor <= 0 || amount_minor > purchase.i64_field("total_cents") {
                fail_webhook!(
                    err_internal_no_cause("Dispute amount does not match its purchase bounds"),
                    "dispute amount mismatch"
                );
            }
            let Ok(currency) = money::normalize_currency(
                data_object
                    .get("currency")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
            ) else {
                fail_webhook!(
                    err_internal_no_cause("Dispute currency is invalid"),
                    "dispute currency was invalid"
                )
            };
            if !currency.eq_ignore_ascii_case(purchase.str_field("currency")) {
                fail_webhook!(
                    err_internal_no_cause("Dispute currency does not match its purchase"),
                    "dispute currency mismatch"
                );
            }
            let status = data_object
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let snapshot = repo::disputes::DisputeSnapshot {
                purchase_id: purchase.id.clone(),
                seller_account_id: purchase.str_field("seller_account_id").to_string(),
                stripe_account_id: purchase_account.to_string(),
                provider_dispute_id,
                provider_charge_id: stripe_resource_id(data_object.get("charge")),
                payment_intent_id,
                status,
                amount_minor,
                currency,
                reason: data_object
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                evidence_due_by: stripe_timestamp(data_object.pointer("/evidence_details/due_by")),
                livemode: dispute_livemode,
                event_created,
            };
            if let Err(error) = repo::disputes::reconcile(ctx, &snapshot).await {
                fail_webhook!(
                    err_internal("Failed to reconcile Stripe dispute", error),
                    "dispute ledger reconciliation failed"
                );
            }
        }

        "refund.created" | "refund.updated" | "refund.failed" => {
            let provider_refund_id = data_object
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if !provider_refund_id.is_empty() {
                let ledger =
                    match repo::refunds::get_by_provider_refund_id(ctx, provider_refund_id).await {
                        Ok(ledger) => ledger,
                        Err(error) => fail_webhook!(
                            err_internal("Failed to load refund ledger", error),
                            "refund ledger lookup failed"
                        ),
                    };
                if let Some(mut ledger) = ledger {
                    let ledger_account = ledger.str_field("stripe_account_id");
                    if (!event_account.is_empty() && event_account != ledger_account)
                        || (event_account.is_empty() && !ledger_account.is_empty())
                    {
                        fail_webhook!(
                            err_internal_no_cause(
                                "Refund webhook connected account does not match its ledger",
                            ),
                            "refund connected-account mismatch"
                        );
                    }
                    let event_intent = data_object
                        .get("payment_intent")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
                    let event_amount = data_object
                        .get("amount")
                        .and_then(|value| value.as_i64())
                        .unwrap_or_default();
                    if (!event_intent.is_empty()
                        && event_intent != ledger.str_field("payment_intent_id"))
                        || (event_amount > 0 && event_amount != ledger.i64_field("amount_minor"))
                    {
                        fail_webhook!(
                            err_internal_no_cause(
                                "Refund webhook does not match its immutable request snapshot",
                            ),
                            "refund immutable snapshot mismatch"
                        );
                    }
                    let event_currency = data_object
                        .get("currency")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    if !event_currency.is_empty()
                        && !event_currency.eq_ignore_ascii_case(ledger.str_field("currency"))
                    {
                        fail_webhook!(
                            err_internal_no_cause(
                                "Refund webhook currency does not match its ledger",
                            ),
                            "refund currency mismatch"
                        );
                    }
                    let provider_status = data_object
                        .get("status")
                        .and_then(|value| value.as_str())
                        .unwrap_or(if event_type == "refund.failed" {
                            "failed"
                        } else {
                            "pending"
                        });
                    if !matches!(
                        provider_status,
                        "pending" | "requires_action" | "succeeded" | "failed" | "canceled"
                    ) {
                        fail_webhook!(
                            err_internal_no_cause(
                                "Refund webhook has an unsupported provider status",
                            ),
                            "refund provider status was unsupported"
                        );
                    }
                    let livemode = data_object
                        .get("livemode")
                        .and_then(|value| value.as_bool())
                        .or_else(|| event.get("livemode").and_then(|value| value.as_bool()))
                        .unwrap_or_else(|| ledger.bool_field("livemode"));
                    if livemode != ledger.bool_field("livemode") {
                        fail_webhook!(
                            err_internal_no_cause("Refund webhook mode does not match its ledger",),
                            "refund livemode mismatch"
                        );
                    }
                    let response_json = serde_json::json!({
                        "id": provider_refund_id,
                        "status": provider_status,
                        "amount_minor": ledger.i64_field("amount_minor"),
                        "livemode": livemode,
                        "source": "webhook"
                    })
                    .to_string();
                    let ordered = match repo::refunds::record_webhook_response(
                        ctx,
                        &ledger.id,
                        provider_refund_id,
                        provider_status,
                        livemode,
                        &response_json,
                        event_created,
                    )
                    .await
                    {
                        Ok(ordered) => ordered,
                        Err(error) => fail_webhook!(
                            err_internal("Failed to update refund ledger", error),
                            "refund provider response write failed"
                        ),
                    };
                    ledger = ordered.record;
                    if ordered.applied && provider_status == "succeeded" {
                        if let Err(error) = repo::purchases::reconcile_refund_total(
                            ctx,
                            ledger.str_field("purchase_id"),
                            ledger.i64_field("target_refunded_total_minor"),
                            ledger.str_field("refunded_by"),
                            ledger.str_field("note"),
                        )
                        .await
                        {
                            fail_webhook!(
                                err_internal("Failed to reconcile refund purchase", error),
                                "refund purchase reconciliation failed"
                            );
                        }
                        if let Err(error) = repo::refunds::mark_succeeded(ctx, &ledger.id).await {
                            fail_webhook!(
                                err_internal("Failed to complete refund ledger", error),
                                "refund ledger completion failed"
                            );
                        }
                        if let Err(error) = repo::provider_operations::complete_for_aggregate(
                            ctx,
                            repo::provider_operations::REFUND_RECONCILE,
                            &ledger.id,
                            &response_json,
                        )
                        .await
                        {
                            fail_webhook!(
                                err_internal("Failed to complete provider operation", error),
                                "refund provider operation completion failed"
                            );
                        }
                    } else if ordered.applied && matches!(provider_status, "failed" | "canceled") {
                        if let Err(error) = repo::provider_operations::resolve_for_aggregate(
                            ctx,
                            repo::provider_operations::REFUND_RECONCILE,
                            &ledger.id,
                            false,
                            &response_json,
                            "Stripe refund failed or was canceled",
                        )
                        .await
                        {
                            fail_webhook!(
                                err_internal("Failed to resolve provider operation", error),
                                "refund provider operation failure write failed"
                            );
                        }
                    }
                }
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
                    let purchase_account = purchase.str_field("stripe_account_id");
                    if (!event_account.is_empty() && event_account != purchase_account)
                        || (event_account.is_empty() && !purchase_account.is_empty())
                    {
                        fail_webhook!(
                            err_internal_no_cause(
                                "Refunded charge connected account does not match its purchase",
                            ),
                            "refunded charge connected-account mismatch"
                        );
                    }
                    let event_livemode = data_object
                        .get("livemode")
                        .and_then(|value| value.as_bool())
                        .or_else(|| event.get("livemode").and_then(|value| value.as_bool()));
                    if event_livemode
                        .is_some_and(|livemode| livemode != purchase.bool_field("livemode"))
                    {
                        fail_webhook!(
                            err_internal_no_cause(
                                "Refunded charge mode does not match its purchase",
                            ),
                            "refunded charge livemode mismatch"
                        );
                    }
                    let purchase_total = purchase.i64_field("total_cents");
                    let charge_total = data_object
                        .get("amount")
                        .and_then(|value| value.as_i64())
                        .unwrap_or_default();
                    if charge_total > 0 && charge_total != purchase_total {
                        fail_webhook!(
                            err_internal_no_cause(
                                "Refunded charge total does not match its purchase",
                            ),
                            "refunded charge amount mismatch"
                        );
                    }
                    let amount_refunded = data_object
                        .get("amount_refunded")
                        .and_then(|value| value.as_i64())
                        .unwrap_or_default();
                    let explicitly_full = data_object
                        .get("refunded")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false);
                    let legacy_without_totals = data_object.get("amount_refunded").is_none()
                        && data_object.get("refunded").is_none();
                    let target = if amount_refunded > 0 {
                        amount_refunded
                    } else if explicitly_full || legacy_without_totals {
                        purchase_total
                    } else {
                        fail_webhook!(
                            err_internal_no_cause(
                                "Refunded charge event is missing its cumulative refunded amount",
                            ),
                            "refunded charge cumulative amount missing"
                        );
                    };
                    if let Err(error) =
                        repo::purchases::reconcile_refund_total(ctx, &purchase.id, target, "", "")
                            .await
                    {
                        tracing::error!("Failed to reconcile refunded charge: {error}");
                        fail_webhook!(
                            err_internal("Failed to update purchase refund total", error),
                            "refunded charge purchase update failed"
                        );
                    }
                }
            }
        }

        _ => {
            // Ignore unhandled event types
        }
    }

    // Seal only the lease this delivery owns. If this write fails or the
    // lease was taken over after expiry, return non-2xx so Stripe retries;
    // acknowledging without a durable terminal state could lose the event.
    if let Some((owner, _)) = event_lease.as_ref() {
        if let Err(error) = mark_event_processed(ctx, event_id, owner).await {
            tracing::error!(
                event_id = %event_id,
                error = %error,
                "failed to mark Stripe webhook event processed"
            );
            return err_internal("Failed to complete webhook processing lease", error);
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

/// Accept the primary frontend origin or one of the explicitly configured
/// static-storefront origins.
pub(crate) fn is_allowed_checkout_url(
    url: &str,
    frontend_url: &str,
    allowed_origins: &str,
) -> bool {
    is_same_origin(url, frontend_url)
        || allowed_origins
            .split(',')
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .any(|origin| is_same_origin(url, origin))
}

/// Stripe versions are date + named GA release (for example
/// 2026-02-25.clover). Preview channels are intentionally rejected.
pub(crate) fn is_stable_stripe_api_version(value: &str) -> bool {
    let Some((date, release)) = value.split_once('.') else {
        return false;
    };
    let date = date.as_bytes();
    date.len() == 10
        && date[0..4].iter().all(u8::is_ascii_digit)
        && date[4] == b'-'
        && date[5..7].iter().all(u8::is_ascii_digit)
        && date[7] == b'-'
        && date[8..10].iter().all(u8::is_ascii_digit)
        && !release.is_empty()
        && release
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && release != "preview"
}

fn stripe_request_headers(
    secret_key: &str,
    api_version: &str,
    idempotency_key: Option<&str>,
) -> HashMap<String, String> {
    stripe_client::request_headers(secret_key, api_version, None, idempotency_key)
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
    fn checkout_url_accepts_explicit_static_origins_only() {
        let extra = "https://shop.example, https://campaign.example:8443";
        assert!(is_allowed_checkout_url(
            "https://shop.example/thanks",
            "https://app.example",
            extra
        ));
        assert!(is_allowed_checkout_url(
            "https://campaign.example:8443/thanks",
            "https://app.example",
            extra
        ));
        assert!(!is_allowed_checkout_url(
            "https://campaign.example/thanks",
            "https://app.example",
            extra
        ));
        assert!(!is_allowed_checkout_url(
            "https://attacker.example/thanks",
            "https://app.example",
            extra
        ));
    }

    #[test]
    fn stripe_api_version_requires_a_stable_named_release() {
        assert!(is_stable_stripe_api_version("2026-02-25.clover"));
        for invalid in [
            "",
            "2026-02-25",
            "2026-2-25.clover",
            "2026-02-25.preview",
            "2026-02-25.Clover",
            "latest",
        ] {
            assert!(!is_stable_stripe_api_version(invalid), "{invalid}");
        }
    }

    #[test]
    fn stripe_headers_pin_version_and_idempotency() {
        let headers = stripe_request_headers("sk_test_x", "2026-02-25.clover", Some("checkout_1"));
        assert_eq!(headers["Authorization"], "Bearer sk_test_x");
        assert_eq!(headers["Stripe-Version"], "2026-02-25.clover");
        assert_eq!(headers["Idempotency-Key"], "checkout_1");
    }

    #[test]
    fn subscription_checkout_form_uses_inline_recurring_prices_and_exact_fee_percent() {
        let offer: Offer = serde_json::from_value(serde_json::json!({
            "id": "offer_subscription",
            "product_id": "product_subscription",
            "version": 4,
            "name": "Monthly service",
            "mode": "subscription",
            "currency": "NZD",
            "pricing_model": "components",
            "recurring_interval": "month",
            "interval_count": 1,
            "usage_type": "licensed",
            "billing_scheme": "per_unit",
            "tax_behavior": "exclusive",
            "variables": [],
            "components": [{
                "id": "component_subscription_base",
                "key": "base",
                "label": "Base plan",
                "required": true,
                "amount": {"type": "fixed", "unit_amount_minor": 4000}
            }],
            "checkout": {"trial_days": 14}
        }))
        .unwrap();
        let preview = offer_pricing::evaluate_offer(
            &offer,
            &PricingPreviewRequest {
                offer_id: offer.id.clone(),
                quantity: 1,
                inputs: Default::default(),
            },
        )
        .unwrap();
        let request: CheckoutRequest = serde_json::from_value(serde_json::json!({
            "offer_id": offer.id,
            "presentation": "hosted"
        }))
        .unwrap();
        let form = build_offer_checkout_form(
            &offer,
            &preview,
            "Monthly service",
            "order_subscription",
            &request,
            "https://shop.example/success",
            "https://shop.example/cancel",
            false,
            "NZ",
            110,
            275,
        )
        .unwrap();
        assert!(form.contains("mode=subscription"));
        assert!(form.contains("[recurring][interval]=month"));
        assert!(form.contains("[recurring][interval_count]=1"));
        assert!(form.contains("subscription_data[trial_period_days]=14"));
        assert!(form.contains("subscription_data[application_fee_percent]=2.75"));
        assert!(form.contains("subscription_data[metadata][purchase_id]=order_subscription"));
        assert!(form.contains("subscription_data[metadata][offer_id]=offer_subscription"));
        assert!(form.contains("subscription_data[metadata][offer_version]=4"));
        assert!(!form.contains("payment_intent_data[metadata]"));
        assert!(!form.contains("payment_intent_data[application_fee_amount]"));
    }

    #[test]
    fn checkout_and_payment_link_forms_apply_validated_shipping_policy() {
        let mut offer: Offer = serde_json::from_value(serde_json::json!({
            "id": "offer_shipping",
            "product_id": "product_shipping",
            "version": 2,
            "name": "Shipped product",
            "mode": "payment",
            "currency": "NZD",
            "pricing_model": "fixed",
            "interval_count": 1,
            "usage_type": "licensed",
            "billing_scheme": "per_unit",
            "tax_behavior": "exclusive",
            "variables": [],
            "components": [{
                "id": "component_shipping_base",
                "key": "base",
                "label": "Product",
                "required": true,
                "amount": {"type": "fixed", "unit_amount_minor": 4000}
            }],
            "checkout": {
                "collect_shipping_address": true,
                "allowed_shipping_countries": ["NZ", "AU"],
                "create_customer": true,
                "shipping_options": [{
                    "display_name": "Standard shipping",
                    "amount_minor": 500,
                    "tax_behavior": "exclusive",
                    "delivery_estimate": {
                        "minimum": 3,
                        "maximum": 5,
                        "unit": "business_day"
                    }
                }, {
                    "display_name": "Express",
                    "amount_minor": 1500,
                    "stripe_shipping_rate_id": "shr_express_123"
                }]
            }
        }))
        .unwrap();
        let preview = offer_pricing::evaluate_offer(
            &offer,
            &PricingPreviewRequest {
                offer_id: offer.id.clone(),
                quantity: 1,
                inputs: Default::default(),
            },
        )
        .unwrap();
        let request: CheckoutRequest = serde_json::from_value(serde_json::json!({
            "offer_id": offer.id,
            "presentation": "hosted"
        }))
        .unwrap();
        let checkout = build_offer_checkout_form(
            &offer,
            &preview,
            "Shipped product",
            "order_shipping",
            &request,
            "https://shop.example/success",
            "https://shop.example/cancel",
            false,
            "US",
            0,
            0,
        )
        .unwrap();
        assert!(checkout.contains("shipping_address_collection[allowed_countries][0]=NZ"));
        assert!(checkout.contains("shipping_address_collection[allowed_countries][1]=AU"));
        assert!(checkout
            .contains("shipping_options[0][shipping_rate_data][display_name]=Standard%20shipping"));
        assert!(
            checkout.contains("shipping_options[0][shipping_rate_data][fixed_amount][amount]=500")
        );
        assert!(checkout
            .contains("shipping_options[0][shipping_rate_data][fixed_amount][currency]=nzd"));
        assert!(checkout.contains(
            "shipping_options[0][shipping_rate_data][delivery_estimate][minimum][value]=3"
        ));
        assert!(checkout.contains(
            "shipping_options[0][shipping_rate_data][delivery_estimate][maximum][unit]=business_day"
        ));
        assert!(checkout.contains("shipping_options[1][shipping_rate]=shr_express_123"));
        assert!(checkout.contains("customer_creation=always"));

        let error = payment_link_form(
            &offer,
            &preview,
            "Shipped product",
            "link_shipping",
            "",
            None,
            false,
            "US",
            0,
            0,
        )
        .unwrap_err();
        assert!(error.contains("Stripe shipping rate ID"));

        offer.checkout.shipping_options[0].stripe_shipping_rate_id = "shr_standard_123".into();
        let payment_link = payment_link_form(
            &offer,
            &preview,
            "Shipped product",
            "link_shipping",
            "",
            None,
            false,
            "US",
            0,
            0,
        )
        .unwrap();
        assert!(payment_link.contains("shipping_options[0][shipping_rate]=shr_standard_123"));
        assert!(payment_link.contains("shipping_options[1][shipping_rate]=shr_express_123"));
        assert!(!payment_link.contains("shipping_rate_data"));
        assert!(payment_link.contains("customer_creation=always"));
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

    // --- Webhook event idempotency (code review 2026-07-16) ---

    use crate::test_support::{output_json, TestContext};

    /// Build a signed webhook request `(Message, InputStream)` for `body`,
    /// using `secret` to compute the `Stripe-Signature` header the same way
    /// `verify_stripe_signature` expects it.
    fn signed_webhook_request(body: &serde_json::Value, secret: &str) -> (Message, InputStream) {
        let payload = serde_json::to_vec(body).unwrap();
        let timestamp = chrono::Utc::now().timestamp() as u64;
        let signed_payload = build_signed_payload(timestamp, &payload);
        let computed = primitives::hmac_sha256(secret.as_bytes(), &signed_payload);
        let sig_header = format!("t={timestamp},v1={}", hex_encode(&computed));

        let mut msg = Message::new("http.request");
        msg.set_meta("req.action", "create");
        msg.set_meta("req.resource", "/b/products/webhooks");
        msg.set_meta("http.header.stripe-signature", sig_header);
        (msg, InputStream::from_bytes(payload))
    }

    #[tokio::test]
    async fn handle_webhook_is_idempotent_on_replayed_event_id() {
        let mut ctx = TestContext::with_products().await;
        let secret = "whsec_test_idempotency";
        ctx.set_config("IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET", secret);

        // `charge.refunded` with no matching purchase: the event-type match
        // arm runs (purchase lookup misses, so no further side effect) — this
        // isolates the assertion to the idempotency mechanism itself rather
        // than a specific business side effect.
        let body = serde_json::json!({
            "id": "evt_replay_test_1",
            "type": "charge.refunded",
            "data": { "object": { "payment_intent": "pi_does_not_exist" } }
        });

        // First delivery: processed normally, no `duplicate` marker.
        let (msg1, input1) = signed_webhook_request(&body, secret);
        let json1 = output_json(handle_webhook(&ctx, &msg1, input1).await).await;
        assert_eq!(json1["received"], true);
        assert!(
            json1.get("duplicate").is_none(),
            "first delivery must not be marked duplicate: {json1:?}"
        );

        // Replay: identical event id — must ack 200 and skip processing.
        let (msg2, input2) = signed_webhook_request(&body, secret);
        let json2 = output_json(handle_webhook(&ctx, &msg2, input2).await).await;
        assert_eq!(json2["received"], true);
        assert_eq!(
            json2["duplicate"], true,
            "replayed event id must be acked as a duplicate no-op: {json2:?}"
        );

        // Exactly one row recorded for this event id — proves the UNIQUE
        // constraint (not just app-level logic) is what's deduping.
        let count = db::count_by_field(
            &ctx,
            "impresspress__products__stripe_events",
            "id",
            serde_json::json!("evt_replay_test_1"),
        )
        .await
        .expect("count stripe_events rows");
        assert_eq!(
            count, 1,
            "exactly one row should exist for the replayed event id"
        );
    }

    #[tokio::test]
    async fn handle_webhook_processes_distinct_event_ids_independently() {
        let mut ctx = TestContext::with_products().await;
        let secret = "whsec_test_idempotency_2";
        ctx.set_config("IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET", secret);

        for id in ["evt_distinct_1", "evt_distinct_2"] {
            let body = serde_json::json!({
                "id": id,
                "type": "charge.refunded",
                "data": { "object": { "payment_intent": "pi_does_not_exist" } }
            });
            let (msg, input) = signed_webhook_request(&body, secret);
            let json = output_json(handle_webhook(&ctx, &msg, input).await).await;
            assert_eq!(json["received"], true);
            assert!(
                json.get("duplicate").is_none(),
                "a fresh, distinct event id must not be marked duplicate: {json:?}"
            );
        }
    }

    /// An event with no top-level `id` can't be deduped, but must still be
    /// processed (not rejected) — this preserves existing behavior for
    /// synthetic/malformed-but-signed payloads, since the HMAC signature
    /// already establishes the caller holds the webhook secret.
    #[tokio::test]
    async fn handle_webhook_processes_event_with_no_id_without_erroring() {
        let mut ctx = TestContext::with_products().await;
        let secret = "whsec_test_idempotency_3";
        ctx.set_config("IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET", secret);

        let body = serde_json::json!({ "type": "charge.refunded", "data": {} });
        let (msg, input) = signed_webhook_request(&body, secret);
        let json = output_json(handle_webhook(&ctx, &msg, input).await).await;
        assert_eq!(json["received"], true);
        assert!(json.get("duplicate").is_none());
    }

    // --- Pending/processed status (I1 follow-up 2026-07-17: "recording
    // event before side effects drops the event on transient failure") ---

    /// Seed a row directly in `impresspress__products__stripe_events`,
    /// simulating a delivery recorded by [`record_event`] at some earlier
    /// point (either a prior attempt that died mid-way, or one that already
    /// completed) — without going through `handle_webhook` itself.
    async fn seed_stripe_event_row(
        ctx: &crate::test_support::TestContext,
        event_id: &str,
        status: &str,
    ) {
        let mut row = HashMap::new();
        row.insert("id".to_string(), serde_json::json!(event_id));
        row.insert(
            "event_type".to_string(),
            serde_json::json!("charge.refunded"),
        );
        row.insert("status".to_string(), serde_json::json!(status));
        row.insert(
            "created_at".to_string(),
            serde_json::json!(chrono::Utc::now().to_rfc3339()),
        );
        db::create(ctx, STRIPE_EVENTS_TABLE, row)
            .await
            .expect("seed stripe_events row");
    }

    /// A `pending` row (a prior attempt that recorded the event but died
    /// before its side effects completed — process crash, transient DB
    /// error, …) must be RE-processed on the next delivery of the same
    /// event id, not silently skipped as a duplicate. Once that delivery
    /// completes, the row must flip to `processed` so a THIRD delivery is
    /// then correctly skipped.
    #[tokio::test]
    async fn handle_webhook_reprocesses_a_previously_pending_event() {
        let mut ctx = TestContext::with_products().await;
        let secret = "whsec_test_pending_retry";
        ctx.set_config("IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET", secret);

        seed_stripe_event_row(&ctx, "evt_pending_retry", EVENT_STATUS_PENDING).await;

        let body = serde_json::json!({
            "id": "evt_pending_retry",
            "type": "charge.refunded",
            "data": { "object": { "payment_intent": "pi_does_not_exist" } }
        });

        // Delivery must re-process (not skip) a still-pending event.
        let (msg, input) = signed_webhook_request(&body, secret);
        let json = output_json(handle_webhook(&ctx, &msg, input).await).await;
        assert_eq!(json["received"], true);
        assert!(
            json.get("duplicate").is_none(),
            "a previously-pending event must be RE-processed, not skipped as a duplicate: {json:?}"
        );

        // The row must now be sealed as processed.
        let row = db::get(&ctx, STRIPE_EVENTS_TABLE, "evt_pending_retry")
            .await
            .expect("row exists after processing");
        assert_eq!(
            row.data.get("status").and_then(|v| v.as_str()),
            Some(EVENT_STATUS_PROCESSED),
            "row must be sealed processed once side effects succeed"
        );

        // A THIRD delivery of the same event id is now a true duplicate.
        let (msg2, input2) = signed_webhook_request(&body, secret);
        let json2 = output_json(handle_webhook(&ctx, &msg2, input2).await).await;
        assert_eq!(json2["received"], true);
        assert_eq!(
            json2["duplicate"], true,
            "a processed event must be skipped on a later redelivery: {json2:?}"
        );
    }

    /// A `processed` row is a true duplicate and must be skipped outright —
    /// the counterpart to the `pending` re-process case above.
    #[tokio::test]
    async fn handle_webhook_skips_an_already_processed_event() {
        let mut ctx = TestContext::with_products().await;
        let secret = "whsec_test_already_processed";
        ctx.set_config("IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET", secret);

        seed_stripe_event_row(&ctx, "evt_already_processed", EVENT_STATUS_PROCESSED).await;

        let body = serde_json::json!({
            "id": "evt_already_processed",
            "type": "charge.refunded",
            "data": { "object": { "payment_intent": "pi_does_not_exist" } }
        });
        let (msg, input) = signed_webhook_request(&body, secret);
        let json = output_json(handle_webhook(&ctx, &msg, input).await).await;
        assert_eq!(json["received"], true);
        assert_eq!(
            json["duplicate"], true,
            "an already-processed event must be skipped, not re-run: {json:?}"
        );
    }

    #[tokio::test]
    async fn webhook_event_claim_has_one_live_owner_and_owner_checked_completion() {
        let ctx = TestContext::with_products().await;
        let payload = r#"{"id":"evt_lease","type":"charge.refunded"}"#;
        let first = record_event(
            &ctx,
            "evt_lease",
            "charge.refunded",
            payload.as_bytes(),
            "",
            false,
        )
        .await
        .expect("first claim");
        let (owner, attempts) = match first {
            EventRecordState::Claimed { owner, attempts } => (owner, attempts),
            state => panic!("fresh event was not claimed: {state:?}"),
        };
        assert_eq!(attempts, 1);

        assert_eq!(
            record_event(
                &ctx,
                "evt_lease",
                "charge.refunded",
                payload.as_bytes(),
                "",
                false,
            )
            .await
            .expect("concurrent claim result"),
            EventRecordState::InFlight,
        );
        assert!(mark_event_processed(&ctx, "evt_lease", "not-the-owner")
            .await
            .is_err());
        mark_event_processed(&ctx, "evt_lease", &owner)
            .await
            .expect("owner completes lease");
        assert_eq!(
            record_event(
                &ctx,
                "evt_lease",
                "charge.refunded",
                payload.as_bytes(),
                "",
                false,
            )
            .await
            .expect("processed duplicate"),
            EventRecordState::AlreadyProcessed,
        );
    }

    #[tokio::test]
    async fn webhook_event_expired_lease_can_be_taken_over_atomically() {
        let ctx = TestContext::with_products().await;
        let payload = r#"{"id":"evt_expired","type":"charge.refunded"}"#;
        let hash = sha256_hex(payload.as_bytes());
        let mut row = HashMap::new();
        row.insert("id".to_string(), serde_json::json!("evt_expired"));
        row.insert(
            "event_type".to_string(),
            serde_json::json!("charge.refunded"),
        );
        row.insert(
            "status".to_string(),
            serde_json::json!(EVENT_STATUS_PROCESSING),
        );
        row.insert("attempts".to_string(), serde_json::json!(2));
        row.insert(
            "processing_owner".to_string(),
            serde_json::json!("expired-owner"),
        );
        row.insert(
            "processing_started_at".to_string(),
            serde_json::json!((chrono::Utc::now()
                - chrono::Duration::seconds(EVENT_LEASE_SECONDS + 1))
            .to_rfc3339()),
        );
        row.insert("payload_sha256".to_string(), serde_json::json!(&hash));
        row.insert(
            "payload_base64".to_string(),
            serde_json::json!(Base64::encode_string(payload.as_bytes())),
        );
        row.insert(
            "created_at".to_string(),
            serde_json::json!(chrono::Utc::now().to_rfc3339()),
        );
        db::create(&ctx, STRIPE_EVENTS_TABLE, row)
            .await
            .expect("seed expired lease");

        let reclaimed = record_event(
            &ctx,
            "evt_expired",
            "charge.refunded",
            payload.as_bytes(),
            "",
            false,
        )
        .await
        .expect("reclaim expired lease");
        let owner = match reclaimed {
            EventRecordState::Claimed { owner, attempts: 3 } => owner,
            state => panic!("expired lease was not reclaimed: {state:?}"),
        };
        assert_ne!(owner, "expired-owner");
        assert!(
            mark_event_processed(&ctx, "evt_expired", "expired-owner")
                .await
                .is_err(),
            "an expired worker must not commit after takeover"
        );
        mark_event_processed(&ctx, "evt_expired", &owner)
            .await
            .expect("new owner completes event");
    }

    #[tokio::test]
    async fn webhook_event_failures_back_off_and_exhaust_into_dead_letter() {
        let ctx = TestContext::with_products().await;
        let payload = r#"{"id":"evt_failure","type":"charge.refunded"}"#;
        let claimed = record_event(
            &ctx,
            "evt_failure",
            "charge.refunded",
            payload.as_bytes(),
            "",
            false,
        )
        .await
        .expect("claim failure fixture");
        let owner = match claimed {
            EventRecordState::Claimed { owner, attempts: 1 } => owner,
            state => panic!("unexpected initial state: {state:?}"),
        };
        mark_event_failed(&ctx, "evt_failure", &owner, 1, "transient database error")
            .await
            .expect("release failed lease");
        let row = db::get(&ctx, STRIPE_EVENTS_TABLE, "evt_failure")
            .await
            .expect("failed event row");
        assert_eq!(row.str_field("status"), EVENT_STATUS_FAILED);
        assert_eq!(row.str_field("last_error"), "transient database error");
        assert!(!row.str_field("next_retry_at").is_empty());
        assert_eq!(
            record_event(
                &ctx,
                "evt_failure",
                "charge.refunded",
                payload.as_bytes(),
                "",
                false,
            )
            .await
            .expect("backoff state"),
            EventRecordState::RetryScheduled,
        );

        let mut retry_now = HashMap::new();
        retry_now.insert(
            "next_retry_at".to_string(),
            serde_json::json!((chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339()),
        );
        db::update(&ctx, STRIPE_EVENTS_TABLE, "evt_failure", retry_now)
            .await
            .expect("make retry due");
        let retry = record_event(
            &ctx,
            "evt_failure",
            "charge.refunded",
            payload.as_bytes(),
            "",
            false,
        )
        .await
        .expect("retry claim");
        let retry_owner = match retry {
            EventRecordState::Claimed { owner, attempts: 2 } => owner,
            state => panic!("due failure was not reclaimed: {state:?}"),
        };
        mark_event_failed(
            &ctx,
            "evt_failure",
            &retry_owner,
            EVENT_MAX_ATTEMPTS,
            "permanent failure",
        )
        .await
        .expect("dead-letter event");
        let row = db::get(&ctx, STRIPE_EVENTS_TABLE, "evt_failure")
            .await
            .expect("dead-letter row");
        assert_eq!(row.str_field("status"), EVENT_STATUS_DEAD_LETTER);
        assert!(!row.str_field("terminal_at").is_empty());
    }

    #[tokio::test]
    async fn webhook_event_id_cannot_be_reused_with_a_different_payload() {
        let ctx = TestContext::with_products().await;
        let first_payload = r#"{"id":"evt_tamper","type":"charge.refunded"}"#;
        let first = record_event(
            &ctx,
            "evt_tamper",
            "charge.refunded",
            first_payload.as_bytes(),
            "",
            false,
        )
        .await
        .expect("claim first payload");
        let owner = match first {
            EventRecordState::Claimed { owner, .. } => owner,
            state => panic!("first payload not claimed: {state:?}"),
        };
        mark_event_processed(&ctx, "evt_tamper", &owner)
            .await
            .expect("complete first payload");

        let changed_payload = r#"{"id":"evt_tamper","type":"account.updated"}"#;
        let error = record_event(
            &ctx,
            "evt_tamper",
            "account.updated",
            changed_payload.as_bytes(),
            "acct_changed",
            true,
        )
        .await
        .expect_err("event id reuse with changed payload must fail");
        assert_eq!(error.code, wafer_run::ErrorCode::FailedPrecondition);
    }

    #[tokio::test]
    async fn webhook_event_admin_projection_hides_payload_and_processing_owner() {
        let ctx = TestContext::with_products().await;
        let payload = r#"{"id":"evt_admin_safe","type":"charge.refunded","data":{"object":{"payment_intent":"pi_private_payload"}}}"#;
        db::create(
            &ctx,
            STRIPE_EVENTS_TABLE,
            HashMap::from([
                ("id".to_string(), serde_json::json!("evt_admin_safe")),
                (
                    "event_type".to_string(),
                    serde_json::json!("charge.refunded"),
                ),
                (
                    "status".to_string(),
                    serde_json::json!(EVENT_STATUS_DEAD_LETTER),
                ),
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
                    serde_json::json!(sha256_hex(payload.as_bytes())),
                ),
                (
                    "last_error".to_string(),
                    serde_json::json!("purchase write failed"),
                ),
                (
                    "created_at".to_string(),
                    serde_json::json!(chrono::Utc::now().to_rfc3339()),
                ),
            ]),
        )
        .await
        .expect("seed dead-letter event");

        let list = list_webhook_events(&ctx, Some(EVENT_STATUS_DEAD_LETTER), 1, 20)
            .await
            .expect("list dead-letter events");
        assert_eq!(list.total_count, 1);
        assert_eq!(list.records[0].id, "evt_admin_safe");
        assert_eq!(list.records[0].attempts, 8);
        let encoded = serde_json::to_string(&list).unwrap();
        assert!(!encoded.contains("pi_private_payload"));
        assert!(!encoded.contains("private-owner-token"));
        assert!(!encoded.contains("payload_base64"));
        assert!(!encoded.contains("payload_sha256"));
        assert!(!encoded.contains("processing_owner"));
    }

    #[tokio::test]
    async fn dead_letter_replay_checks_integrity_and_uses_normal_webhook_processing() {
        let mut ctx = TestContext::with_products().await;
        ctx.set_config(
            "IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET",
            "whsec_manual_replay",
        );
        let payload = r#"{"id":"evt_manual_replay","type":"charge.refunded","livemode":false,"data":{"object":{"payment_intent":"pi_missing","livemode":false}}}"#;
        db::create(
            &ctx,
            STRIPE_EVENTS_TABLE,
            HashMap::from([
                ("id".to_string(), serde_json::json!("evt_manual_replay")),
                (
                    "event_type".to_string(),
                    serde_json::json!("charge.refunded"),
                ),
                (
                    "status".to_string(),
                    serde_json::json!(EVENT_STATUS_DEAD_LETTER),
                ),
                ("attempts".to_string(), serde_json::json!(8)),
                (
                    "payload_base64".to_string(),
                    serde_json::json!(Base64::encode_string(payload.as_bytes())),
                ),
                (
                    "payload_sha256".to_string(),
                    serde_json::json!(sha256_hex(payload.as_bytes())),
                ),
                (
                    "created_at".to_string(),
                    serde_json::json!(chrono::Utc::now().to_rfc3339()),
                ),
            ]),
        )
        .await
        .expect("seed replay event");
        let stored = db::get(&ctx, STRIPE_EVENTS_TABLE, "evt_manual_replay")
            .await
            .expect("stored replay event");
        assert_eq!(
            Base64::decode_vec(stored.str_field("payload_base64")).unwrap(),
            payload.as_bytes()
        );
        assert_eq!(
            stored.str_field("payload_sha256"),
            sha256_hex(payload.as_bytes())
        );

        let response = replay_webhook_event(&ctx, "evt_manual_replay")
            .await
            .expect("start manual replay");
        let body = output_json(response).await;
        assert_eq!(body["received"], true);
        let replayed = db::get(&ctx, STRIPE_EVENTS_TABLE, "evt_manual_replay")
            .await
            .expect("replayed event row");
        assert_eq!(replayed.str_field("status"), EVENT_STATUS_PROCESSED);
        assert_eq!(replayed.u64_field("attempts"), 1);
        assert!(!replayed.str_field("processed_at").is_empty());

        let tampered_payload = r#"{"id":"evt_bad_replay","type":"charge.refunded"}"#;
        db::create(
            &ctx,
            STRIPE_EVENTS_TABLE,
            HashMap::from([
                ("id".to_string(), serde_json::json!("evt_bad_replay")),
                (
                    "event_type".to_string(),
                    serde_json::json!("charge.refunded"),
                ),
                ("status".to_string(), serde_json::json!(EVENT_STATUS_FAILED)),
                (
                    "payload_base64".to_string(),
                    serde_json::json!(Base64::encode_string(tampered_payload.as_bytes())),
                ),
                (
                    "payload_sha256".to_string(),
                    serde_json::json!("incorrect-hash"),
                ),
                (
                    "created_at".to_string(),
                    serde_json::json!(chrono::Utc::now().to_rfc3339()),
                ),
            ]),
        )
        .await
        .expect("seed corrupted event");
        let Err(error) = replay_webhook_event(&ctx, "evt_bad_replay").await else {
            panic!("corrupted payload must not replay");
        };
        assert_eq!(error.code, wafer_run::ErrorCode::FailedPrecondition);
    }
}
