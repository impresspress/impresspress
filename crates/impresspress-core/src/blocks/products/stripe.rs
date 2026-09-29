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
    config::{
        platform_country, seller_fee_bps, CountryCode, AUTOMATIC_TAX, CHECKOUT_ALLOWED_ORIGINS,
        STRIPE_API_VERSION, STRIPE_SECRET_KEY, STRIPE_WEBHOOK_SECRET, WEBHOOK_SECRET, WEBHOOK_URL,
    },
    contracts::{
        self, AmountRule, CheckoutPresentation, CheckoutRequest, CheckoutResponse, EventStatus,
        ManagedOffer, ManagedPaymentLink, Offer, OfferMode, OfferStatus, OrderStatus,
        PaymentLinkCreateRequest, PricingPreviewRequest, ProviderPaymentStatus,
        ReconciliationStatus, StripeEventType, SubscriptionStatus, WebhookAck, WebhookEventList,
        WebhookEventSummary,
    },
    money, offer_pricing, repo,
    stripe_client::{self, StripeClient},
    stripe_provider, stripe_secret_operations_allowed,
};
use crate::{
    blocks::crud,
    config_vars::FRONTEND_URL_KEY,
    http::{
        err_bad_request, err_forbidden, err_internal, err_internal_no_cause, err_not_found,
        err_unauthorized, err_unavailable, ok_json,
    },
    util::{hex_encode, sha256_hex, RecordExt},
};

/// Recorded Stripe webhook event ids (code review 2026-07-16: "Stripe
/// webhooks lack event idempotency"; I1 follow-up 2026-07-17: "recording
/// event before side effects drops the event on transient failure"). See
/// `003_stripe_events.sqlite.sql` for the schema and full rationale.
///
/// Sourced from `repo::stripe_events::TABLE` (the repo-convention owner of
/// the name) rather than restated here — this file keeps its own short
/// alias since every call site below already spells `STRIPE_EVENTS_TABLE`.
const STRIPE_EVENTS_TABLE: &str = repo::stripe_events::TABLE;

const EVENT_LEASE_SECONDS: i64 = 300;

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
    /// The row is `dead_letter` (with its `last_error`) when this is returned.
    DeadLetter,
}

fn event_timestamp(record: &Record, field: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(record.str_field(field))
        .ok()
        .map(|value| value.with_timezone(&chrono::Utc))
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
                serde_json::json!(EventStatus::Processing),
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
    let status = event_status(&existing)?;
    match status {
        EventStatus::Processed => return Ok(EventRecordState::AlreadyProcessed),
        EventStatus::DeadLetter => return Ok(EventRecordState::DeadLetter),
        EventStatus::Processing => {
            let lease_is_live =
                event_timestamp(&existing, "processing_started_at").is_some_and(|started| {
                    now_value.signed_duration_since(started).num_seconds() < EVENT_LEASE_SECONDS
                });
            if lease_is_live {
                return Ok(EventRecordState::InFlight);
            }
        }
        EventStatus::Failed => {
            if event_timestamp(&existing, "next_retry_at").is_some_and(|next| next > now_value) {
                return Ok(EventRecordState::RetryScheduled);
            }
        }
        EventStatus::Pending => {}
    }

    // The CAS both writes below take: the row must still be in the state
    // this delivery just read, under the owner it just read.
    let unchanged = vec![
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
    ];
    let attempts = existing.u64_field("attempts").saturating_add(1);
    if attempts > repo::MAX_ATTEMPTS {
        // Out of budget without a recorded outcome — the last attempt's
        // lease expired. The row is moved to `dead_letter` here, before the
        // caller acknowledges: Stripe stops redelivering an acknowledged
        // event, and only a `failed`/`dead_letter` row can be replayed, so a
        // row left `processing` would be unrecoverable.
        let reason = exhausted_event_reason(status, &existing);
        let dead_lettered = db::update_by_filters_count(
            ctx,
            STRIPE_EVENTS_TABLE,
            unchanged,
            HashMap::from([
                (
                    "status".to_string(),
                    serde_json::json!(EventStatus::DeadLetter),
                ),
                ("processing_owner".to_string(), serde_json::json!("")),
                ("processing_started_at".to_string(), serde_json::Value::Null),
                ("next_retry_at".to_string(), serde_json::Value::Null),
                ("last_error".to_string(), serde_json::json!(reason)),
                ("terminal_at".to_string(), serde_json::json!(&now)),
            ]),
        )
        .await?;
        return Ok(if dead_lettered == 1 {
            EventRecordState::DeadLetter
        } else {
            // Another delivery moved the row first; its state stands and
            // this delivery is retried against it.
            EventRecordState::InFlight
        });
    }
    let mut data = HashMap::new();
    data.insert(
        "status".to_string(),
        serde_json::json!(EventStatus::Processing),
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
    let claimed = db::update_by_filters_count(ctx, STRIPE_EVENTS_TABLE, unchanged, data).await?;
    if claimed == 1 {
        Ok(EventRecordState::Claimed { owner, attempts })
    } else {
        Ok(EventRecordState::InFlight)
    }
}

/// `last_error` for an event that ran out of attempts without its last
/// attempt recording an outcome. The previous recorded error, if any, is kept
/// after it so the operator still sees why the earlier attempts failed.
fn exhausted_event_reason(status: EventStatus, existing: &Record) -> String {
    let attempts = existing.u64_field("attempts");
    let mut reason = match status {
        EventStatus::Processing => format!(
            "retry budget of {} attempts exhausted: the processing lease of attempt {attempts} \
             expired without recording an outcome",
            repo::MAX_ATTEMPTS
        ),
        _ => format!(
            "retry budget of {} attempts exhausted after {attempts} attempts",
            repo::MAX_ATTEMPTS
        ),
    };
    let previous = existing.str_field("last_error");
    if !previous.is_empty() {
        reason.push_str("; last recorded error: ");
        reason.push_str(previous);
    }
    reason.chars().take(1000).collect()
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
        serde_json::json!(EventStatus::Processed),
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
                value: serde_json::json!(EventStatus::Processing),
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
    let dead_letter = attempts >= repo::MAX_ATTEMPTS;
    let mut data = HashMap::new();
    data.insert(
        "status".to_string(),
        serde_json::json!(if dead_letter {
            EventStatus::DeadLetter
        } else {
            EventStatus::Failed
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
                + chrono::Duration::seconds(repo::retry_delay_seconds(attempts)))
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
                value: serde_json::json!(EventStatus::Processing),
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

/// The `status` column of a stripe-events row, as the enum that defines
/// it. The five `const &str` this replaces were used both as written
/// values and as match patterns against the raw column, so a stored value
/// outside the set fell through every arm silently.
fn event_status(record: &Record) -> Result<EventStatus, WaferError> {
    crate::util::enum_column(record, "status")
}

fn webhook_event_summary(record: Record) -> Result<WebhookEventSummary, WaferError> {
    Ok(WebhookEventSummary {
        id: record.id.clone(),
        event_type: record.str_field("event_type").to_string(),
        status: event_status(&record)?,
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
    })
}

pub(crate) async fn list_webhook_events(
    ctx: &dyn Context,
    status: Option<EventStatus>,
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
    // Loudly, not row-by-row: this is the operator's webhook queue, and a row
    // whose `status` is outside the set is exactly the row an operator opened
    // the page to find. Omitting it would hide the fault from the one view
    // that exists to show it.
    Ok(WebhookEventList {
        records: result
            .records
            .into_iter()
            .map(webhook_event_summary)
            .collect::<Result<Vec<_>, _>>()?,
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
    if !stripe_secret_operations_allowed(ctx) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe webhook replay is disabled in the browser runtime",
        ));
    }
    let secret = config::get_default(ctx, STRIPE_WEBHOOK_SECRET, "").await?;
    if secret.is_empty() {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe webhook secret is not configured",
        ));
    }
    let event = db::get(ctx, STRIPE_EVENTS_TABLE, event_id).await?;
    let previous_status = event_status(&event)?;
    if !matches!(
        previous_status,
        EventStatus::Failed | EventStatus::DeadLetter
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
                value: serde_json::json!(previous_status),
            },
        ],
        HashMap::from([
            (
                "status".to_string(),
                serde_json::json!(EventStatus::Pending),
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
    let settings = async {
        Ok::<_, WaferError>((
            stripe_secret_operations_allowed(ctx),
            config::get_optional(ctx, STRIPE_SECRET_KEY).await?,
            config::get_default(ctx, STRIPE_API_VERSION, DEFAULT_STRIPE_API_VERSION).await?,
        ))
    };
    let (secret_operations_allowed, stripe_key, stripe_api_version) = match settings.await {
        Ok(settings) => settings,
        Err(e) => return crud::db_error_internal(e, "Could not read the Stripe settings"),
    };
    if !secret_operations_allowed {
        return err_forbidden(
            "Stripe secret-key checkout is disabled in the browser runtime; use a trusted remote commerce API or a pre-created Payment Link",
        );
    }
    if stripe_key.is_none_or(|key| key.trim().is_empty()) {
        return err_unavailable("Stripe is not configured");
    }
    if !is_stable_stripe_api_version(&stripe_api_version) {
        return err_internal_no_cause(
            "Stripe API version must be a stable YYYY-MM-DD.release value",
        );
    }

    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let request: CheckoutRequest = match serde_json::from_slice(&raw) {
        Ok(request) => request,
        Err(error) => return err_bad_request(&format!("Invalid body: {error}")),
    };
    // Every guard above has already run, so the only way loading the client
    // can fail here is a key that is neither `sk_test_` nor `sk_live_`.
    let Ok(client) = StripeClient::load(ctx).await else {
        return err_internal_no_cause(
            "Stripe secret key is malformed; expected an sk_test_ or sk_live_ key",
        );
    };
    handle_offer_checkout(ctx, msg, request, &client).await
}

/// Whether Stripe automatic tax is on by default for new offers.
///
/// One reader for `IMPRESSPRESS__PRODUCTS__AUTOMATIC_TAX`: the checkout and
/// Payment-Link money paths read it here, and so does the product wizard
/// (`pages::product_wizard`), which used to compare the raw value against
/// `"true"` — so `=1` turned tax on at checkout while the wizard drew the
/// toggle off.
pub(in crate::blocks::products) async fn automatic_tax_enabled(
    ctx: &dyn Context,
) -> Result<bool, WaferError> {
    crate::config_vars::get_bool(ctx, AUTOMATIC_TAX, false).await
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

/// The country list Stripe collects a shipping address for: the offer's own
/// list, or the platform's country when the offer names none.
///
/// [B23] There is no third answer. `allowed_countries` is a required member
/// of Stripe's `shipping_address_collection`, so a caller that cannot name a
/// country cannot silently omit the key — that would leave `collect_shipping
/// _address` on an offer that then collects no address at all. It used to
/// substitute `"US"`, which shipped a New Zealand storefront to the United
/// States and said nothing.
fn shipping_countries(
    offer: &Offer,
    platform_country: Option<&CountryCode>,
) -> Result<Vec<String>, String> {
    if !offer.checkout.allowed_shipping_countries.is_empty() {
        return Ok(offer
            .checkout
            .allowed_shipping_countries
            .iter()
            .map(|country| country.trim().to_ascii_uppercase())
            .collect());
    }
    match platform_country {
        Some(country) => Ok(vec![country.as_str().to_string()]),
        None => Err(
            "this offer collects a shipping address but names no allowed countries; list them on the offer or set IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY"
                .to_string(),
        ),
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
    platform_country: Option<&CountryCode>,
) -> Result<(), String> {
    if !offer.checkout.collect_shipping_address {
        return Ok(());
    }
    for (index, country) in shipping_countries(offer, platform_country)?
        .into_iter()
        .enumerate()
    {
        push_form(
            pairs,
            format!("shipping_address_collection[allowed_countries][{index}]"),
            country,
        );
    }
    Ok(())
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

#[expect(
    clippy::too_many_arguments,
    reason = "the Stripe form is assembled from independently-sourced values — \
              offer, pricing preview, URLs, tax and fee settings"
)]
fn build_offer_checkout_form(
    offer: &Offer,
    preview: &crate::blocks::products::contracts::PricingPreview,
    product_name: &str,
    order_id: &str,
    request: &CheckoutRequest,
    success_url: &str,
    cancel_url: &str,
    automatic_tax: bool,
    platform_country: Option<&CountryCode>,
    fee_minor: i64,
    fee_basis_points: u16,
) -> Result<Vec<(String, String)>, String> {
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
    push_shipping_address_collection(&mut pairs, offer, platform_country)?;
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
    Ok(pairs)
}

/// Mark a claimed checkout order failed after the provider call could not be
/// completed, so it can never be claimed for another checkout.
///
/// The caller still returns the *original* failure — that is what a
/// compensation is, and reporting the rollback's error instead would hide the
/// reason the checkout stopped. What it must not do is drop it: an order this
/// could not mark stays `checkout_started` and claimed forever, and without
/// its id in the log there is nothing to search for. All four checkout
/// failure paths route through here so the reason and the id are recorded the
/// same way at each of them.
async fn record_checkout_failure(ctx: &dyn Context, order_id: &str, reason: &str) {
    if let Err(error) = repo::purchases::mark_checkout_failed(ctx, order_id, reason).await {
        tracing::error!(
            order_id = %order_id,
            reason = %reason,
            error = %error,
            "could not mark a failed checkout order; it stays claimed"
        );
    }
}

/// The Payment Link half of [`record_checkout_failure`]: the same
/// compensation, on the managed-link row, with the same reason for logging a
/// rollback that could not be written.
///
/// A definite refusal (`FailedPrecondition`, see `stripe_client::classify`)
/// retires the row so a retry is a new request under a new key; any other
/// failure leaves the outcome at Stripe unknown, so the row stays active and
/// a retry re-sends the same request under the same key.
async fn record_payment_link_failure(ctx: &dyn Context, link_id: &str, failure: &WaferError) {
    let recorded = if failure.code == wafer_run::ErrorCode::FailedPrecondition {
        repo::payment_links::mark_rejected(ctx, link_id, &failure.message).await
    } else {
        repo::payment_links::mark_error(ctx, link_id, &failure.message).await
    };
    if let Err(error) = recorded {
        tracing::error!(
            link_id = %link_id,
            reason = %failure.message,
            error = %error,
            "could not mark a failed payment link; it stays pending"
        );
    }
}

async fn handle_offer_checkout(
    ctx: &dyn Context,
    msg: &Message,
    request: CheckoutRequest,
    client: &StripeClient,
) -> OutputStream {
    if let Some(email) = request.buyer_email.as_deref() {
        if email.len() > 254 || email.chars().any(char::is_control) {
            return err_bad_request("buyer_email is invalid");
        }
    }
    let offer = match repo::offers::get_public(ctx, &request.offer_id).await {
        Ok(offer) => offer,
        Err(error) => return crud::db_error(error, "Offer not found", "Could not load offer"),
    };
    let product = match repo::products::get(ctx, &offer.product_id).await {
        Ok(product) => product,
        // The same 404 the offer read above gives, and for the same reason:
        // this read carries the soft-delete filter now, so a delete landing
        // between the two reads answers `NotFound` — an ordinary outcome, not
        // a server fault. Mapping it to `err_internal` showed a storefront
        // buyer a 500 for the very state the neighbouring refusal calls
        // "Offer not found".
        Err(error) => {
            return crud::db_error(error, "Offer not found", "Could not load offer product")
        }
    };

    let owner_is_user = product.str_field("owner_kind") == "user";
    let (seller_account_id, stripe_account_id, fee_basis_points) = if owner_is_user {
        match super::handlers::user_products_enabled(ctx).await {
            Ok(true) => {}
            Ok(false) => return err_not_found("Offer not found"),
            Err(e) => return crud::db_error_internal(e, "Could not read the seller switch"),
        }
        let owner_id = product.str_field("owner_id");
        let seller = match repo::seller_accounts::ready_for_user(ctx, owner_id).await {
            Ok(seller) => seller,
            Err(error) if error.code == wafer_run::ErrorCode::FailedPrecondition => {
                return err_bad_request(
                    "This seller's Stripe account is not ready to accept charges",
                )
            }
            Err(error) => return crud::db_error_internal(error, "Could not load seller account"),
        };
        let fee = match seller_fee_bps(ctx).await {
            Ok(fee) => fee,
            Err(error) => return err_internal("Platform application fee is misconfigured", error),
        };
        (seller.id, seller.stripe_account_id, fee)
    } else {
        (String::new(), String::new(), 0)
    };

    let (checkout_inputs, input_scope) = match request.preset_id.as_deref() {
        Some(preset_id) => {
            if !request.inputs.is_empty() {
                return err_bad_request("preset checkout cannot also provide runtime inputs");
            }
            match repo::checkout_presets::get_active(ctx, &offer.id, preset_id).await {
                // Preset values were validated under the management scope when
                // the preset was saved, so they may pin hidden or admin-only
                // variables the buyer could never supply directly.
                Ok(preset) => (preset.inputs, offer_pricing::InputScope::Management),
                Err(error) => {
                    return crud::db_error(
                        error,
                        "Checkout preset not found",
                        "Could not load checkout preset",
                    )
                }
            }
        }
        None => (request.inputs.clone(), offer_pricing::InputScope::Public),
    };
    let pricing_request = PricingPreviewRequest {
        offer_id: request.offer_id.clone(),
        quantity: request.quantity,
        inputs: checkout_inputs,
    };
    let mut preview = match offer_pricing::evaluate_offer(&offer, &pricing_request, input_scope) {
        Ok(preview) => preview,
        Err(error) => return err_bad_request(&format!("{}: {}", error.code, error)),
    };
    let fee_minor = match application_fee(preview.amounts.total_minor, fee_basis_points) {
        Ok(fee) => fee,
        Err(error) => return err_bad_request(&error),
    };
    preview.amounts.platform_fee_minor = fee_minor;

    let requires = product.str_field("requires");
    if !requires.is_empty() {
        let owns = if msg.user_id().is_empty() {
            // An anonymous caller cannot own anything, and asking the
            // database would only be a chance to fail for the wrong reason.
            false
        } else {
            match user_owns_product(ctx, msg.user_id(), requires).await {
                Ok(owns) => owns,
                // "We could not check" is not "you do not own it": that
                // refusal names the buyer as the problem and no retry clears
                // it, while a 500 says what actually happened and makes the
                // storefront retryable.
                Err(error) => {
                    return crud::db_error_internal(error, "Could not verify product ownership")
                }
            }
        };
        if !owns {
            return err_bad_request(
                "You must sign in and own the required product before purchasing this item.",
            );
        }
    }

    let origins = async {
        Ok::<_, WaferError>((
            config::get_default(ctx, FRONTEND_URL_KEY, "http://localhost:5173").await?,
            config::get_default(ctx, CHECKOUT_ALLOWED_ORIGINS, "").await?,
        ))
    };
    let (base_url, allowed_origins) = match origins.await {
        Ok(origins) => origins,
        Err(e) => return crud::db_error_internal(e, "Could not read the checkout origins"),
    };
    let success_url = request.success_url.clone().unwrap_or_else(|| {
        format!("{base_url}/checkout/success?session_id={{CHECKOUT_SESSION_ID}}")
    });
    let cancel_url = request
        .cancel_url
        .clone()
        .unwrap_or_else(|| format!("{base_url}/checkout/cancel"));
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
    let expected_livemode = client.livemode;
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
        Err(error) => return crud::db_error_internal(error, "Could not create checkout order"),
    };

    let rows = match repo::purchases::claim_for_checkout(ctx, &order.id).await {
        Ok(rows) => rows,
        Err(error) => return crud::db_error_internal(error, "Could not claim checkout order"),
    };
    if rows != 1 {
        return err_internal_no_cause("Checkout order could not be claimed");
    }

    let automatic_tax = offer.checkout.automatic_tax
        || match automatic_tax_enabled(ctx).await {
            Ok(enabled) => enabled,
            Err(error) => {
                record_checkout_failure(ctx, &order.id, &error.message).await;
                return crud::db_error_internal(error, "Could not read the automatic tax setting");
            }
        };
    let country = match platform_country(ctx).await {
        Ok(country) => country,
        Err(error) => {
            record_checkout_failure(ctx, &order.id, &error.message).await;
            return err_internal("Platform country is misconfigured", error);
        }
    };
    let stripe_body = match build_offer_checkout_form(
        &offer,
        &preview,
        product.str_field("name"),
        &order.id,
        &request,
        &success_url,
        &cancel_url,
        automatic_tax,
        country.as_ref(),
        fee_minor,
        fee_basis_points,
    ) {
        Ok(body) => body,
        Err(error) => {
            record_checkout_failure(ctx, &order.id, &error).await;
            return err_bad_request(&error);
        }
    };

    let session = match client
        .request_json(
            ctx,
            "POST",
            "/v1/checkout/sessions",
            Some(&stripe_account_id),
            Some(&format!("impresspress_offer_checkout_{}", order.id)),
            Some(stripe_body),
        )
        .await
    {
        Ok(session) => session,
        Err(error) => {
            record_checkout_failure(ctx, &order.id, &error.message).await;
            return err_internal("Stripe API error", error);
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
        record_checkout_failure(
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
            serde_json::json!(ReconciliationStatus::AwaitingPayment),
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
        return crud::db_error_internal(error, "Could not save Stripe checkout session");
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
    if !super::handlers::user_products_enabled(ctx).await? {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "user product selling is disabled",
        ));
    }
    let seller = repo::seller_accounts::ready_for_user(ctx, product.str_field("owner_id")).await?;
    let fee = seller_fee_bps(ctx).await?;
    Ok((seller.id, seller.stripe_account_id, fee))
}

/// Read a catalog object, treating "not there" as a fact.
///
/// [B21] The classification is [`StripeClient`]'s — this wrapper only adds
/// the second way Stripe says an object is gone: a `deleted: true` body on a
/// 200. It used to own a copy of the whole decision and got it wrong for
/// every retryable status.
async fn stripe_catalog_get(
    ctx: &dyn Context,
    client: &StripeClient,
    path: &str,
    stripe_account_id: &str,
) -> Result<Option<serde_json::Value>, WaferError> {
    let Some(decoded) = client
        .request_json_optional(ctx, "GET", path, Some(stripe_account_id))
        .await?
    else {
        return Ok(None);
    };
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
    let client = StripeClient::load(ctx).await?;
    let livemode = client.livemode;
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
        let path = format!("/v1/products/{stripe_product_id}");
        match stripe_catalog_get(ctx, &client, &path, &stripe_account_id).await? {
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
                let updated = client
                    .request_json(
                        ctx,
                        "POST",
                        &path,
                        Some(&stripe_account_id),
                        Some(&idempotency_key),
                        Some(form),
                    )
                    .await?;
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
        let response = client
            .request_json(
                ctx,
                "POST",
                "/v1/products",
                Some(&stripe_account_id),
                Some(&idempotency_key),
                Some(stripe_product_form(product)),
            )
            .await?;
        stripe_product_id = validate_stripe_product(&response, None, livemode)?;
        // The unfiltered write on purpose: the Stripe Product above already
        // exists. Refusing to record its id because the local product was
        // soft-deleted while the sync was in flight would leave that Stripe
        // object orphaned in the connected account with nothing pointing at
        // it — and it is exactly `stripe_product_id` that
        // `archive_offer_catalog` later needs in order to take it down.
        repo::products::update_including_deleted(
            ctx,
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
            let path = format!("/v1/prices/{price_id}");
            match stripe_catalog_get(ctx, &client, &path, &stripe_account_id).await? {
                Some(response) => {
                    let active = response.get("active").and_then(serde_json::Value::as_bool);
                    if active == Some(false) {
                        let idempotency_key = format!(
                            "impresspress_price_reactivate_{}_{}",
                            component.id,
                            &sha256_hex(price_id.as_bytes())[..16]
                        );
                        let reactivated = client
                            .request_json(
                                ctx,
                                "POST",
                                &path,
                                Some(&stripe_account_id),
                                Some(&idempotency_key),
                                Some(vec![("active".to_string(), "true".to_string())]),
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
            let response = client
                .request_json(
                    ctx,
                    "POST",
                    "/v1/prices",
                    Some(&stripe_account_id),
                    Some(&idempotency_key),
                    Some(form),
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
    if !stripe_secret_operations_allowed(ctx) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe catalog synchronization is disabled in the browser runtime",
        ));
    }
    let product = repo::products::get(ctx, product_id).await?;
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
        retire_payment_link_for_archival(ctx, offer_id, &link.id).await?;
    }
    if synced_components.is_empty() {
        return repo::offers::archive(ctx, product_id, offer_id).await;
    }
    if !stripe_secret_operations_allowed(ctx) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe catalog archival is disabled in the browser runtime",
        ));
    }
    // Reads past the soft-delete filter on purpose. Archival takes a
    // product's Prices out of the live Stripe catalog, and a soft-deleted
    // product is exactly when that most needs doing: deleting a product
    // touches nothing in Stripe, so seller suspension has to be able to
    // archive the catalog of a listing that is already gone locally. The row
    // is read only for `owner_kind`/`owner_id` (which connected account to
    // address) and `stripe_product_id`; nothing about a deleted product
    // reaches a caller, since this path only ever deactivates.
    let product = repo::products::get_including_deleted(ctx, product_id).await?;
    let client = StripeClient::load(ctx).await?;
    let livemode = client.livemode;
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
        let path = format!("/v1/prices/{}", component.stripe_price_id);
        let Some(remote) = stripe_catalog_get(ctx, &client, &path, &stripe_account_id).await?
        else {
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
        let archived = client
            .request_json(
                ctx,
                "POST",
                &path,
                Some(&stripe_account_id),
                Some(&idempotency_key),
                Some(vec![("active".to_string(), "false".to_string())]),
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

#[expect(
    clippy::too_many_arguments,
    reason = "the Stripe form is assembled from independently-sourced values — \
              offer, pricing preview, completion URL, tax and fee settings"
)]
fn payment_link_form(
    offer: &Offer,
    preview: &crate::blocks::products::contracts::PricingPreview,
    product_name: &str,
    preset_id: &str,
    after_completion_url: Option<&str>,
    automatic_tax: bool,
    platform_country: Option<&CountryCode>,
    fee_minor: i64,
    fee_basis_points: u16,
) -> Result<Vec<(String, String)>, String> {
    let included: Vec<_> = preview
        .components
        .iter()
        .filter(|component| component.included)
        .collect();
    if included.is_empty() || included.len() > 20 {
        return Err("Payment Links require between 1 and 20 included line items".to_string());
    }
    let mut pairs = Vec::new();
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
    push_shipping_address_collection(&mut pairs, offer, platform_country)?;
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
    Ok(pairs)
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
    if !stripe_secret_operations_allowed(ctx) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe Payment Link creation is disabled in the browser runtime",
        ));
    }
    let client = StripeClient::load(ctx).await?;
    let livemode = client.livemode;
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
        // Payment Links are created on owner/admin routes from preset values
        // that were validated under the management scope at save time.
        offer_pricing::InputScope::Management,
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
        let base_url = config::get_default(ctx, FRONTEND_URL_KEY, "http://localhost:5173").await?;
        let allowed = config::get_default(ctx, CHECKOUT_ALLOWED_ORIGINS, "").await?;
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
    let configured =
        repo::payment_links::find_for_configuration(ctx, offer_id, &preset_id, &configuration_hash)
            .await?;
    let unfinished = match configured {
        Some(repo::payment_links::ConfiguredLink::Synced(existing)) => return Ok(existing),
        Some(repo::payment_links::ConfiguredLink::Unfinished(id)) => Some(id),
        None => None,
    };

    let (seller_account_id, stripe_account_id, fee_basis_points) =
        payment_link_seller_context(ctx, product).await?;
    let fee_minor = application_fee(preview.amounts.total_minor, fee_basis_points)
        .map_err(|error| WaferError::new(wafer_run::ErrorCode::InvalidArgument, error))?;
    // Everything the Stripe request carries except the row id is settled
    // before any row is written, so a configuration that can never produce a
    // valid request (a malformed platform country, an out-of-range line-item
    // count) leaves nothing behind to retry.
    let country = platform_country(ctx).await?;
    let mut body = payment_link_form(
        &offer,
        &preview,
        product.str_field("name"),
        &preset_id,
        after_completion_url,
        offer.checkout.automatic_tax || automatic_tax_enabled(ctx).await?,
        country.as_ref(),
        fee_minor,
        fee_basis_points,
    )
    .map_err(|error| WaferError::new(wafer_run::ErrorCode::InvalidArgument, error))?;
    let link_id = match &unfinished {
        Some(id) => id.clone(),
        None => {
            repo::payment_links::pending_id(ctx, offer_id, &preset_id, &configuration_hash).await?
        }
    };
    // The row id travels in the request's metadata, so the request is
    // complete before the attempt is recorded — and the row can record the
    // exact bytes its idempotency key covers.
    push_form(
        &mut body,
        "metadata[impresspress_payment_link_id]",
        &link_id,
    );
    let idempotency_key = payment_link_idempotency_key(&stripe_account_id, &body);
    let attempt = repo::payment_links::Attempt {
        seller_account_id: &seller_account_id,
        stripe_account_id: &stripe_account_id,
        pricing_snapshot: &preview,
        fee_basis_points,
        request: &body,
    };
    let pending = match unfinished {
        Some(_) => repo::payment_links::restart_pending(ctx, &link_id, &attempt).await?,
        None => {
            repo::payment_links::create_pending(
                ctx,
                &link_id,
                offer_id,
                &preset_id,
                livemode,
                &configuration_hash,
                &attempt,
            )
            .await?
        }
    };
    if pending.managed.sync_status == "synced" {
        return Ok(pending.managed);
    }
    let response = match client
        .request_json(
            ctx,
            "POST",
            "/v1/payment_links",
            Some(&stripe_account_id),
            Some(&idempotency_key),
            Some(body),
        )
        .await
    {
        Ok(response) => response,
        Err(error) => {
            record_payment_link_failure(ctx, &pending.managed.id, &error).await;
            return Err(error);
        }
    };
    let stripe_id = response
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let url = response
        .get("url")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if stripe_id.is_empty() || url.is_empty() {
        let error = WaferError::new(
            wafer_run::ErrorCode::Internal,
            "Stripe Payment Link response was incomplete",
        );
        record_payment_link_failure(ctx, &pending.managed.id, &error).await;
        return Err(error);
    }
    // Stripe now holds a live link. If this write fails the row stays
    // `syncing`; a retry re-drives the same row with the same request, so
    // while Stripe retains the key it answers with this same link for the
    // retry to record — and a deactivation of the row reaches it the same
    // way. A retry whose request changed in between (see
    // `payment_link_idempotency_key`) cannot reach it; the log line names the
    // link so it can be reconciled.
    match repo::payment_links::mark_synced(ctx, &pending.managed.id, stripe_id, url).await {
        Ok(Some(stored)) => Ok(stored.managed),
        Ok(None) => {
            // The row was retired between this request and its result — an
            // owner deactivating it, or a refusal recorded by a twin. No row
            // points at this link, so it must not stay buyable. Record the id
            // on the retired row first: that is what makes the takedown a
            // retryable operation rather than this one call, which can fail
            // and leave a live link nothing names.
            if let Err(error) =
                repo::payment_links::record_stripe_link(ctx, &pending.managed.id, stripe_id).await
            {
                tracing::error!(
                    link_id = %pending.managed.id,
                    stripe_payment_link_id = %stripe_id,
                    error = %error,
                    "Stripe created a Payment Link for a retired row that could not be \
                     recorded; it must be deactivated at Stripe"
                );
            }
            enqueue_payment_link_takedown(ctx, &pending.managed.id, &stripe_account_id).await;
            Err(WaferError::new(
                wafer_run::ErrorCode::Aborted,
                "the Payment Link was retired while Stripe created it; retry",
            ))
        }
        Err(error) => {
            tracing::error!(
                link_id = %pending.managed.id,
                stripe_payment_link_id = %stripe_id,
                error = %error,
                "Stripe created a Payment Link that could not be recorded locally"
            );
            Err(error)
        }
    }
}

/// Take a Payment Link down at Stripe. The key is the durable local row id,
/// so a repeat of the same deactivation is the same request.
async fn deactivate_stripe_payment_link(
    ctx: &dyn Context,
    client: &StripeClient,
    stripe_account_id: &str,
    stripe_payment_link_id: &str,
    link_id: &str,
) -> Result<(), WaferError> {
    client
        .request_json(
            ctx,
            "POST",
            &format!(
                "/v1/payment_links/{}",
                crate::util::url_path_encode(stripe_payment_link_id)
            ),
            Some(stripe_account_id),
            Some(&payment_link_deactivate_key(link_id)),
            Some(vec![("active".to_string(), "false".to_string())]),
        )
        .await?;
    Ok(())
}

/// The Stripe idempotency key for one Payment Link request: a digest of the
/// Stripe account and the complete request body.
///
/// Stripe refuses a reused key whose parameters differ, so the key covers
/// every parameter sent — including the local row id in the metadata, which
/// is itself derived from the configuration and its generation (see
/// `repo::payment_links::pending_id`). One key therefore always carries one
/// request. A retry of an unfinished row with nothing changed sends the same
/// bytes under the same key, and while Stripe retains the key (at least 24
/// hours) it reaches the object the first attempt created, or replays that
/// attempt's saved result — which is also how
/// `resolve_unrecorded_payment_link` learns a link id the row never
/// recorded. Anything that changes the request — the fee, the
/// automatic-tax setting, the product name, the platform country, a
/// component's synced Price, the account, a new generation after a
/// deactivation or a refusal — is a new key and a new Stripe object.
fn payment_link_idempotency_key(stripe_account_id: &str, form: &[(String, String)]) -> String {
    let request = format!("{stripe_account_id}\n{}", encode_form(form.to_vec()));
    format!(
        "impresspress_payment_link_{}",
        sha256_hex(request.as_bytes())
    )
}

/// Take a Payment Link down for an offer being archived, and retire its row
/// even when Stripe will not answer.
///
/// Archival is the off switch, and seller suspension runs it over every offer
/// a seller owns as a fraud control. A Stripe outage is exactly when that has
/// to complete, so a provider failure hands the link to the durable takedown
/// queue and the sweep carries on; the row retires either way, so nothing
/// local goes on selling it. The direct deactivation route propagates
/// instead — there a caller is watching and can repeat the action.
async fn retire_payment_link_for_archival(
    ctx: &dyn Context,
    offer_id: &str,
    link_id: &str,
) -> Result<(), WaferError> {
    let Err(error) = deactivate_payment_link(ctx, offer_id, link_id).await else {
        return Ok(());
    };
    if !stripe_secret_operations_allowed(ctx) {
        // Queuing buys nothing in a runtime that cannot reach Stripe at all,
        // and the refusal is the honest answer to "archive this".
        return Err(error);
    }
    let stored = repo::payment_links::get(ctx, link_id).await?;
    tracing::warn!(
        link_id = %link_id,
        stripe_payment_link_id = %stored.stripe_payment_link_id,
        error = %error,
        "archiving an offer could not take its Payment Link down at Stripe; queued for retry"
    );
    queue_payment_link_takedown(ctx, link_id, &stored.stripe_account_id).await;
    repo::payment_links::deactivate_local(ctx, offer_id, link_id).await?;
    Ok(())
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
    if stored.stripe_payment_link_id.is_empty() && stored.stripe_request.is_empty() {
        // The request is written before it is sent, so a row that records
        // none has nothing at Stripe to take down. A row older than the
        // `stripe_request` column reads the same way and deactivates
        // locally.
        return repo::payment_links::deactivate_local(ctx, offer_id, link_id).await;
    }
    if !stripe_secret_operations_allowed(ctx) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe Payment Link deactivation is disabled in the browser runtime",
        ));
    }
    if !stored.stripe_payment_link_id.is_empty() {
        // The row names the link, so this is one request whose failure the
        // caller can act on by repeating it: the row stays active until
        // Stripe has answered.
        let client = StripeClient::load(ctx).await?;
        deactivate_stripe_payment_link(
            ctx,
            &client,
            &stored.stripe_account_id,
            &stored.stripe_payment_link_id,
            link_id,
        )
        .await?;
        return repo::payment_links::deactivate_local(ctx, offer_id, link_id).await;
    }
    // A link may exist at Stripe that no row names. Resolving it can take
    // more than this request has — Stripe may be down, and past the key
    // retention only an operator can find it — so the takedown becomes a
    // durable operation and the row retires either way. Blocking the retire
    // on it would make one stuck link enough to block archiving an offer or
    // suspending a seller, which is a fraud control.
    enqueue_payment_link_takedown(ctx, &stored.managed.id, &stored.stripe_account_id).await;
    repo::payment_links::deactivate_local(ctx, offer_id, link_id).await
}

/// Record the durable takedown of `link_id`'s Stripe link without attempting
/// it. `false` when nothing durable holds it, which is only ever a database
/// failure.
///
/// Queuing is not doing: nothing drains the provider-operation queue on its
/// own, so a takedown recorded here runs when an administrator reconciles
/// provider operations (or a scheduler calls that endpoint), and the link can
/// go on taking money until then.
async fn queue_payment_link_takedown(
    ctx: &dyn Context,
    link_id: &str,
    stripe_account_id: &str,
) -> bool {
    if let Err(error) = repo::provider_operations::ensure(
        ctx,
        repo::provider_operations::PAYMENT_LINK_DEACTIVATE,
        "payment_link",
        link_id,
        stripe_account_id,
        &payment_link_deactivate_key(link_id),
        "{\"version\":1}",
    )
    .await
    {
        // Nothing durable holds the takedown now, so say so at the level an
        // operator reads: the row id is the link's handle in Stripe metadata.
        tracing::error!(
            link_id = %link_id,
            error = %error,
            "could not enqueue the takedown of a Payment Link whose id no row records"
        );
        return false;
    }
    true
}

/// Queue the durable takedown of `link_id`'s Stripe link and try to settle it
/// now. A failure here is never the caller's to handle: the queued operation
/// is the retry, and the administrator's provider-operation queue is where an
/// unsettled one surfaces.
async fn enqueue_payment_link_takedown(ctx: &dyn Context, link_id: &str, stripe_account_id: &str) {
    if !queue_payment_link_takedown(ctx, link_id, stripe_account_id).await {
        return;
    }
    match take_down_payment_link(ctx, link_id).await {
        Ok(PaymentLinkTakedown::Settled) => {
            if let Err(error) = repo::provider_operations::complete_for_aggregate(
                ctx,
                repo::provider_operations::PAYMENT_LINK_DEACTIVATE,
                link_id,
                "{}",
            )
            .await
            {
                // The link is down; only the bookkeeping failed, so the
                // operation stays pending and the worker settles it again.
                tracing::warn!(
                    link_id = %link_id,
                    error = %error,
                    "could not complete a Payment Link takedown operation that succeeded"
                );
            }
        }
        Ok(PaymentLinkTakedown::Unresolvable(reason)) => {
            if let Err(error) = repo::provider_operations::resolve_for_aggregate(
                ctx,
                repo::provider_operations::PAYMENT_LINK_DEACTIVATE,
                link_id,
                false,
                "{}",
                &reason,
            )
            .await
            {
                tracing::error!(
                    link_id = %link_id,
                    error = %error,
                    "could not dead-letter an unresolvable Payment Link takedown"
                );
            }
        }
        Err(error) => {
            // Transient as far as anything here can tell, and the operation
            // is due. Nothing drains the queue on its own, though: it runs at
            // the next administrator reconcile, and this link keeps taking
            // money until it does.
            tracing::warn!(
                link_id = %link_id,
                error = %error,
                "a Payment Link takedown is due in the provider-operation queue; it runs at \
                 the next administrator reconcile"
            );
        }
    }
}

/// The idempotency key every attempt at taking one row's Payment Link down
/// shares — inline, queued or replayed. Stripe therefore sees one request
/// however many times this runs.
fn payment_link_deactivate_key(link_id: &str) -> String {
    format!("impresspress_deactivate_payment_link_{link_id}")
}

/// What one attempt at taking a Payment Link down achieved.
pub(crate) enum PaymentLinkTakedown {
    /// The link is inactive at Stripe, or the row provably never had one.
    Settled,
    /// Nothing local can name the link any more, so no retry will do better.
    /// Carries what an operator has to do instead.
    Unresolvable(String),
}

/// Take down the Stripe Payment Link of row `link_id`, whatever the row
/// records. Drives both the deactivation route and the reconciliation
/// worker, so a link takes the same path down however the takedown was
/// reached.
///
/// An `Err` is worth retrying; [`PaymentLinkTakedown::Unresolvable`] is not.
pub(crate) async fn take_down_payment_link(
    ctx: &dyn Context,
    link_id: &str,
) -> Result<PaymentLinkTakedown, WaferError> {
    if !stripe_secret_operations_allowed(ctx) {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
            "Stripe Payment Link deactivation is disabled in the browser runtime",
        ));
    }
    let stored = repo::payment_links::get(ctx, link_id).await?;
    if stored.stripe_payment_link_id.is_empty() && stored.stripe_request.is_empty() {
        return Ok(PaymentLinkTakedown::Settled);
    }
    let client = StripeClient::load(ctx).await?;
    let stripe_payment_link_id = if stored.stripe_payment_link_id.is_empty() {
        match resolve_unrecorded_payment_link(ctx, &client, &stored).await? {
            Resolved::Link(stripe_id) => {
                // Persist before the takedown, so a failure from here on
                // leaves a link the row names and any later attempt — this
                // operation's retry, or an owner repeating the action —
                // takes the short path above instead of resolving again.
                repo::payment_links::record_stripe_link(ctx, link_id, &stripe_id).await?;
                stripe_id
            }
            Resolved::Unresolvable(reason) => return Ok(PaymentLinkTakedown::Unresolvable(reason)),
        }
    } else {
        stored.stripe_payment_link_id.clone()
    };
    deactivate_stripe_payment_link(
        ctx,
        &client,
        &stored.stripe_account_id,
        &stripe_payment_link_id,
        link_id,
    )
    .await?;
    Ok(PaymentLinkTakedown::Settled)
}

/// How long Stripe keeps an idempotency key's saved result. Stripe documents
/// "at least 24 hours"; the shorter end of that promise is the only one a
/// re-send may rely on.
const STRIPE_IDEMPOTENCY_KEY_RETENTION_HOURS: i64 = 24;

/// What is known about the Stripe link of a row that never recorded one.
enum Resolved {
    Link(String),
    /// No request can name it any more; only an operator can.
    Unresolvable(String),
}

/// Learn the Stripe link id of a row whose attempt never recorded one, by
/// re-sending that attempt's own request under its own idempotency key.
///
/// Two outcomes, and the caller cannot tell them apart: if Stripe still holds
/// the key it replays the saved result, which names the link that attempt
/// created; if the attempt never reached the idempotency layer — a 429 from
/// the rate limiter, a connection that died before Stripe saw it — the key is
/// unseen and the re-send EXECUTES, minting a link. Either way the answer
/// names a live link for this row that the caller then takes down, and one
/// row never ends up with two live links.
///
/// Past the retention window the saved result is gone while the original link
/// (if there ever was one) is not, so a re-send would mint a second live link
/// and still not name the first. That is [`Resolved::Unresolvable`]: the row
/// id is the link's `metadata[impresspress_payment_link_id]` at Stripe, which
/// is the handle an operator searches on.
async fn resolve_unrecorded_payment_link(
    ctx: &dyn Context,
    client: &StripeClient,
    stored: &repo::payment_links::StoredPaymentLink,
) -> Result<Resolved, WaferError> {
    let sent_at = chrono::DateTime::parse_from_rfc3339(&stored.stripe_request_at)
        .map(|value| value.with_timezone(&chrono::Utc))
        .map_err(|error| {
            WaferError::new(
                wafer_run::ErrorCode::Internal,
                format!("Payment Link request timestamp is unreadable: {error}"),
            )
        })?;
    if chrono::Utc::now() - sent_at
        > chrono::Duration::hours(STRIPE_IDEMPOTENCY_KEY_RETENTION_HOURS)
    {
        return Ok(Resolved::Unresolvable(format!(
            "The Stripe request of Payment Link {} is older than Stripe's \
             {STRIPE_IDEMPOTENCY_KEY_RETENTION_HOURS} hour idempotency-key retention, so \
             re-sending it would create a second live link instead of naming the first. In \
             the Stripe Dashboard, search Payment Links for \
             metadata[impresspress_payment_link_id]={} and deactivate what you find; this \
             row is already retired locally.",
            stored.managed.id, stored.managed.id
        )));
    }
    let response = client
        .request_json(
            ctx,
            "POST",
            "/v1/payment_links",
            Some(&stored.stripe_account_id),
            Some(&payment_link_idempotency_key(
                &stored.stripe_account_id,
                &stored.stripe_request,
            )),
            Some(stored.stripe_request.clone()),
        )
        .await?;
    let stripe_id = response
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if stripe_id.is_empty() {
        return Err(WaferError::new(
            wafer_run::ErrorCode::Internal,
            "Stripe Payment Link response was incomplete",
        ));
    }
    Ok(Resolved::Link(stripe_id.to_string()))
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
    // A redelivery may find an order a prior delivery already created for
    // this session. Only a terminal order makes the redelivery a duplicate:
    // an order still in its pre-completion state means the prior delivery
    // crashed between order creation and completion, and returning Ok here
    // would seal the event with a paid order stranded in `pending` forever.
    // Such an order resumes the completion path below instead (every
    // identity/amount cross-check still runs; only creation is skipped).
    let mut resumed_order = None;
    if let Some(existing) = repo::purchases::find_by_session(ctx, session_id).await? {
        let status = OrderStatus::from_record(&existing)?;
        if status.awaits_completion() {
            resumed_order = Some(existing);
        } else {
            // The order already completed. Backfill the idempotent
            // subscription-item snapshot in case the prior delivery crashed
            // between the completion write and the snapshot.
            let subscription = session
                .get("subscription")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if !subscription.is_empty() && status == OrderStatus::Completed {
                repo::subscription_items::snapshot_from_purchase(ctx, &existing.id, subscription)
                    .await?;
            }
            return Ok(());
        }
    }
    let offer_id = session
        .pointer("/metadata/offer_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let stored = repo::payment_links::get_for_offer(ctx, offer_id, local_link_id).await?;
    // `FailedPrecondition`, not `PermissionDenied`: the event and the stored
    // link disagree about their own identity, which is not a WRAP refusal and
    // must not be answered as one by `crud::db_error_internal`.
    if stored.stripe_account_id != event_account {
        return Err(WaferError::new(
            wafer_run::ErrorCode::FailedPrecondition,
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
    // Reads past the soft-delete filter on purpose. Soft delete touches
    // nothing in Stripe, so a deleted product's Payment Links stay live in
    // the connected account and stay payable; the money in this session has
    // already been captured. A live-only read answered `NotFound` here, the
    // caller's `fail_webhook!` turned that into an error, and Stripe retried
    // the delivery forever — the customer charged, no purchase row, no line
    // items, and an order-status page that never resolved. The row is read
    // for the product name on the buyer's own receipt.
    let product = repo::products::get_including_deleted(ctx, &offer.product_id).await?;
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
    let order = match resumed_order {
        // The order and its line items already exist from the failed prior
        // delivery; only the completion transition and snapshot remain.
        Some(order) => order,
        None => {
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
            order
        }
    };
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

/// Billing-period end for a subscription event. Stripe API versions from
/// 2025-03 onward (including the pinned Clover version) removed
/// `current_period_end` from the subscription top level and report it per
/// item instead; prefer the top-level field when an older-shaped payload
/// carries it, otherwise take the latest item period end.
fn subscription_period_end(data_object: &serde_json::Value) -> Option<String> {
    if let Some(value) = stripe_timestamp(data_object.get("current_period_end")) {
        return Some(value);
    }
    data_object
        .pointer("/items/data")?
        .as_array()?
        .iter()
        .filter_map(|item| {
            item.get("current_period_end")
                .and_then(serde_json::Value::as_i64)
        })
        .max()
        .and_then(|seconds| {
            chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0)
                .map(|value| value.to_rfc3339())
        })
}

/// Whether the subscription has a scheduled (end-of-period) cancellation.
/// Newer API versions express the Billing Portal's "cancel at period end" as
/// a concrete `cancel_at` timestamp while leaving the legacy
/// `cancel_at_period_end` boolean false; either representation counts.
fn subscription_cancels_at_period_end(data_object: &serde_json::Value) -> bool {
    data_object
        .get("cancel_at_period_end")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        || data_object
            .get("cancel_at")
            .is_some_and(|value| !value.is_null())
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

/// A subscription/invoice event that matches neither a commerce purchase nor
/// the platform subscription table is indistinguishable from a delivery that
/// raced ahead of its own `checkout.session.completed` (a commerce order only
/// gains its `stripe_subscription_id` at completion). Sealing it as processed
/// would silently drop the state change, so the webhook fails it as retryable
/// instead: the lease/backoff machinery retries until the completion links
/// the subscription, and a genuinely foreign subscription dead-letters after
/// the bounded attempts (replayable from the admin queue).
fn unmatched_subscription_message(stripe_subscription_id: &str) -> String {
    format!(
        "Stripe subscription {stripe_subscription_id} matches no commerce order or platform \
         subscription; it may be an out-of-order delivery ahead of its checkout completion — \
         retrying until the completion links it"
    )
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
    let settings = async {
        Ok::<_, WaferError>((
            stripe_secret_operations_allowed(ctx),
            config::get_default(ctx, STRIPE_WEBHOOK_SECRET, "").await?,
        ))
    };
    let (secret_operations_allowed, webhook_secret) = match settings.await {
        Ok(settings) => settings,
        Err(e) => return crud::db_error_internal(e, "Could not read the Stripe webhook settings"),
    };
    if !secret_operations_allowed {
        return err_forbidden("Stripe webhooks are disabled in the browser runtime");
    }
    // Verify Stripe webhook signature - REQUIRED
    if webhook_secret.is_empty() {
        return err_unavailable(
            "STRIPE_WEBHOOK_SECRET not configured — webhook processing disabled for security",
        );
    }
    let sig_header = msg.header("stripe-signature").to_string();
    if sig_header.is_empty() {
        return err_unauthorized("Missing Stripe-Signature header");
    }
    let raw_body = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
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
                return ok_json(&WebhookAck::duplicate());
            }
            Ok(EventRecordState::DeadLetter) => {
                tracing::error!(
                    event_id = %event_id,
                    event_type = %event_type,
                    "Stripe webhook event exhausted its retry budget"
                );
                return ok_json(&WebhookAck::dead_letter());
            }
            Err(e) => return crud::db_error_internal(e, "Failed to record webhook event"),
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

    // Dispatch on the type, not on its spelling. Every arm below is a
    // variant, so a type added to `StripeEventType` — which is also what the
    // Stripe setup page advertises — does not compile until it is routed
    // here, and the two lists cannot describe different sets of events.
    match StripeEventType::from_wire(event_type) {
        Some(StripeEventType::AccountUpdated) => {
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
                    crud::db_error_internal(error, "Failed to synchronize connected account"),
                    "connected-account synchronization failed"
                );
            }
        }
        Some(StripeEventType::CheckoutSessionCompleted)
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
        Some(
            StripeEventType::CheckoutSessionCompleted
            | StripeEventType::CheckoutSessionAsyncPaymentSucceeded,
        ) => {
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
                        crud::db_error_internal(error, "Failed to reconcile checkout purchase"),
                        "checkout session did not match its immutable order"
                    ),
                };
                if rows == 0 {
                    tracing::warn!(
                        "Purchase {} not updated — already completed or refunded",
                        purchase_id
                    );
                }
                if !stripe_subscription_id.is_empty() {
                    // The snapshot is an idempotent upsert and must not be
                    // tied to the completion transition: a crash between the
                    // completion write and the snapshot means the redelivery
                    // sees rows == 0, and skipping it then would strand the
                    // subscription without its item snapshot forever.
                    let snapshot_due = rows == 1
                        || match repo::purchases::get(ctx, purchase_id).await {
                            Ok(purchase) => match OrderStatus::from_record(&purchase) {
                                Ok(status) => status == OrderStatus::Completed,
                                Err(error) => fail_webhook!(
                                    err_internal("Purchase row is outside the contract", error),
                                    "subscription snapshot purchase state is outside the contract"
                                ),
                            },
                            Err(error) => fail_webhook!(
                                crud::db_error_internal(
                                    error,
                                    "Failed to load purchase for subscription snapshot"
                                ),
                                "subscription snapshot purchase lookup failed"
                            ),
                        };
                    if snapshot_due {
                        if let Err(error) = repo::subscription_items::snapshot_from_purchase(
                            ctx,
                            purchase_id,
                            &stripe_subscription_id,
                        )
                        .await
                        {
                            fail_webhook!(
                                crud::db_error_internal(
                                    error,
                                    "Failed to snapshot subscription items"
                                ),
                                "subscription item snapshot failed"
                            );
                        }
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
                        crud::db_error_internal(error, "Failed to reconcile Payment Link order"),
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
                        crud::db_error_internal(error, "Failed to create platform subscription"),
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

        Some(StripeEventType::CheckoutSessionAsyncPaymentFailed) => {
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
                        crud::db_error_internal(
                            error,
                            "Failed to reconcile checkout payment failure"
                        ),
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

        Some(
            kind @ (StripeEventType::PaymentIntentSucceeded
            | StripeEventType::PaymentIntentPaymentFailed
            | StripeEventType::PaymentIntentProcessing
            | StripeEventType::PaymentIntentRequiresAction
            | StripeEventType::PaymentIntentCanceled),
        ) => {
            let payment_intent_id = stripe_resource_id(data_object.get("id"));
            let object_status = data_object
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let status = match kind {
                StripeEventType::PaymentIntentSucceeded if object_status == "succeeded" => {
                    ProviderPaymentStatus::Succeeded
                }
                StripeEventType::PaymentIntentPaymentFailed => ProviderPaymentStatus::PaymentFailed,
                StripeEventType::PaymentIntentProcessing if object_status == "processing" => {
                    ProviderPaymentStatus::Processing
                }
                StripeEventType::PaymentIntentRequiresAction
                    if object_status == "requires_action" =>
                {
                    ProviderPaymentStatus::RequiresAction
                }
                StripeEventType::PaymentIntentCanceled if object_status == "canceled" => {
                    ProviderPaymentStatus::Canceled
                }
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
                status,
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
                    crud::db_error_internal(error, "Failed to reconcile PaymentIntent"),
                    "PaymentIntent reconciliation failed"
                ),
            }
        }

        Some(StripeEventType::CustomerSubscriptionUpdated) => {
            let stripe_sub_id = data_object.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let status = data_object
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let plan = data_object
                .pointer("/items/data/0/price/lookup_key")
                .or_else(|| data_object.pointer("/items/data/0/price/metadata/plan"))
                .and_then(|v| v.as_str());
            let mut commerce_matched = false;
            // Refused here rather than mapped to a default, for the same
            // reason the dispute branch below refuses an unknown network
            // state: a delivery whose `status` this build does not know is a
            // fact about the subscription that would otherwise be dropped on
            // the floor. A 500 makes Stripe redeliver, so nothing is lost and
            // the gap is visible. An absent or empty `status` decodes to
            // `Unset`, which is not a value the subscription is in: the
            // commerce sync below is skipped entirely, and the platform
            // projection keeps its stored status while the plan and the
            // event timestamp the payload does carry still apply
            // (`repo::subscriptions::update_status_plan`).
            let Ok(status) = serde_json::from_value::<SubscriptionStatus>(
                serde_json::Value::String(status.to_string()),
            ) else {
                fail_webhook!(
                    err_internal_no_cause("Subscription status is outside the supported set"),
                    "subscription status was unsupported"
                );
            };
            if !stripe_sub_id.is_empty() && status != SubscriptionStatus::Unset {
                let current_period_end = subscription_period_end(&data_object);
                let canceled_at = stripe_timestamp(data_object.get("canceled_at"));
                match repo::purchases::sync_commerce_subscription(
                    ctx,
                    stripe_sub_id,
                    event_account,
                    event_livemode,
                    status,
                    current_period_end.as_deref(),
                    Some(subscription_cancels_at_period_end(&data_object)),
                    canceled_at.as_deref(),
                    None,
                    event_created,
                )
                .await
                {
                    Ok(Some(_)) => commerce_matched = true,
                    Ok(None) => {}
                    Err(error) => fail_webhook!(
                        crud::db_error_internal(
                            error,
                            "Failed to synchronize commerce subscription"
                        ),
                        "commerce subscription synchronization failed"
                    ),
                }
            }
            if !commerce_matched && !stripe_sub_id.is_empty() {
                match repo::subscriptions::platform_subscription_exists(ctx, stripe_sub_id).await {
                    Ok(true) => {}
                    Ok(false) => {
                        let message = unmatched_subscription_message(stripe_sub_id);
                        fail_webhook!(err_internal_no_cause(&message), &message);
                    }
                    Err(error) => fail_webhook!(
                        crud::db_error_internal(
                            error,
                            "Failed to resolve Stripe subscription ownership"
                        ),
                        "subscription ownership lookup failed"
                    ),
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
                    crud::db_error_internal(error, "Failed to synchronize platform subscription"),
                    "platform subscription status/plan synchronization failed"
                );
            }

            // Sync add-on totals from the metadata of the subscription's
            // items. `repo::subscriptions::ADDON_TOTALS` names the metadata
            // keys the platform stamps on its add-on objects.
            let user_id = match repo::subscriptions::find_user_by_stripe_sub(ctx, stripe_sub_id)
                .await
            {
                Ok(user_id) => user_id,
                // The two things this answer gates — the addon-total sync and
                // the outbound `products.subscription.updated` — were both
                // skipped silently when the read failed, and the delivery
                // still told Stripe it had succeeded, so nothing retried them.
                Err(error) => fail_webhook!(
                    crud::db_error_internal(error, "Failed to resolve Stripe subscription owner"),
                    "subscription owner lookup failed"
                ),
            };
            if let Some(ref uid) = user_id {
                if let Some(items) = data_object.get("items") {
                    // A failed sync used to be logged and nothing else, so the
                    // delivery still sealed the event as processed and Stripe
                    // had nothing to retry: the subscriber kept paying for
                    // add-ons their row never recorded.
                    if let Err(error) =
                        sync_addon_totals_from_items(ctx, uid, items, event_created).await
                    {
                        fail_webhook!(
                            crud::db_error_internal(error, "Failed to synchronize add-on totals"),
                            "add-on total synchronization failed"
                        );
                    }
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

        Some(StripeEventType::InvoicePaid | StripeEventType::InvoicePaymentSucceeded) => {
            let stripe_sub_id = invoice_subscription_id(&data_object);
            if !stripe_sub_id.is_empty() {
                let commerce_matched = match repo::purchases::sync_commerce_subscription(
                    ctx,
                    &stripe_sub_id,
                    event_account,
                    event_livemode,
                    SubscriptionStatus::Active,
                    None,
                    None,
                    None,
                    Some(SubscriptionStatus::PastDue),
                    event_created,
                )
                .await
                {
                    Ok(purchase) => purchase.is_some(),
                    Err(error) => fail_webhook!(
                        crud::db_error_internal(error, "Failed to recover commerce subscription"),
                        "commerce subscription recovery write failed"
                    ),
                };
                if !commerce_matched {
                    match repo::subscriptions::platform_subscription_exists(ctx, &stripe_sub_id)
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            let message = unmatched_subscription_message(&stripe_sub_id);
                            fail_webhook!(err_internal_no_cause(&message), &message);
                        }
                        Err(error) => fail_webhook!(
                            crud::db_error_internal(
                                error,
                                "Failed to resolve Stripe subscription ownership"
                            ),
                            "subscription ownership lookup failed"
                        ),
                    }
                }
                if let Err(error) = repo::subscriptions::recover_from_paid_invoice(
                    ctx,
                    &stripe_sub_id,
                    event_created,
                )
                .await
                {
                    fail_webhook!(
                        crud::db_error_internal(error, "Failed to recover subscription"),
                        "platform subscription recovery write failed"
                    );
                }
            }
        }

        Some(StripeEventType::InvoicePaymentFailed) => {
            let stripe_sub_id = invoice_subscription_id(&data_object);
            if !stripe_sub_id.is_empty() {
                // The past-due write is derived from the invoice, not an
                // authoritative subscription snapshot; the repo layer refuses
                // to move a terminal (canceled) projection back to past_due.
                let commerce_matched = match repo::purchases::sync_commerce_subscription(
                    ctx,
                    &stripe_sub_id,
                    event_account,
                    event_livemode,
                    SubscriptionStatus::PastDue,
                    None,
                    None,
                    None,
                    None,
                    event_created,
                )
                .await
                {
                    Ok(purchase) => purchase.is_some(),
                    Err(error) => fail_webhook!(
                        crud::db_error_internal(
                            error,
                            "Failed to mark commerce subscription past due"
                        ),
                        "commerce subscription past-due write failed"
                    ),
                };
                if !commerce_matched {
                    match repo::subscriptions::platform_subscription_exists(ctx, &stripe_sub_id)
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            let message = unmatched_subscription_message(&stripe_sub_id);
                            fail_webhook!(err_internal_no_cause(&message), &message);
                        }
                        Err(error) => fail_webhook!(
                            crud::db_error_internal(
                                error,
                                "Failed to resolve Stripe subscription ownership"
                            ),
                            "subscription ownership lookup failed"
                        ),
                    }
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
                        crud::db_error_internal(e, "Failed to mark subscription past_due"),
                        "platform subscription past-due write failed"
                    );
                }
            }
        }

        Some(StripeEventType::CustomerSubscriptionDeleted) => {
            let stripe_sub_id = data_object.get("id").and_then(|v| v.as_str()).unwrap_or("");
            // Read before the cancellation writes, as it always was — the row
            // this names is about to have its addons zeroed. A failed read is
            // the whole delivery's failure: the outbound
            // `products.subscription.deleted` is the only thing that tells
            // the platform a paid user has lapsed.
            let user_id = match repo::subscriptions::find_user_by_stripe_sub(ctx, stripe_sub_id)
                .await
            {
                Ok(user_id) => user_id,
                Err(error) => fail_webhook!(
                    crud::db_error_internal(error, "Failed to resolve Stripe subscription owner"),
                    "subscription owner lookup failed"
                ),
            };

            if !stripe_sub_id.is_empty() {
                let canceled_at = stripe_timestamp(data_object.get("canceled_at"));
                let commerce_matched = match repo::purchases::sync_commerce_subscription(
                    ctx,
                    stripe_sub_id,
                    event_account,
                    event_livemode,
                    SubscriptionStatus::Canceled,
                    None,
                    Some(false),
                    canceled_at.as_deref(),
                    None,
                    event_created,
                )
                .await
                {
                    Ok(purchase) => purchase.is_some(),
                    Err(error) => fail_webhook!(
                        crud::db_error_internal(error, "Failed to cancel commerce subscription"),
                        "commerce subscription cancellation failed"
                    ),
                };
                if !commerce_matched {
                    match repo::subscriptions::platform_subscription_exists(ctx, stripe_sub_id)
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            let message = unmatched_subscription_message(stripe_sub_id);
                            fail_webhook!(err_internal_no_cause(&message), &message);
                        }
                        Err(error) => fail_webhook!(
                            crud::db_error_internal(
                                error,
                                "Failed to resolve Stripe subscription ownership"
                            ),
                            "subscription ownership lookup failed"
                        ),
                    }
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
                    crud::db_error_internal(e, "Failed to cancel subscription"),
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

        Some(
            StripeEventType::ChargeDisputeCreated
            | StripeEventType::ChargeDisputeUpdated
            | StripeEventType::ChargeDisputeClosed,
        ) => {
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
                                return crud::db_error_internal(
                                    error,
                                    "Failed to complete webhook processing lease",
                                );
                            }
                        }
                        return ok_json(&WebhookAck::received());
                    }
                    Err(error) => fail_webhook!(
                        crud::db_error_internal(error, "Failed to load disputed purchase"),
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
            // The network state, as the enum that defines the set. The
            // repo used to hold a `supported_status` list and refuse an
            // unrecognised value from inside `reconcile`; the refusal
            // happens here now, one step earlier and with the same
            // outcome — a 500 that makes Stripe redeliver, so a dispute
            // state this build does not know is never stored.
            let Ok(status) = serde_json::from_value::<contracts::DisputeStatus>(
                data_object.get("status").cloned().unwrap_or_default(),
            ) else {
                fail_webhook!(
                    err_internal_no_cause("Dispute status is outside the supported set"),
                    "dispute status was unsupported"
                );
            };
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
                    crud::db_error_internal(error, "Failed to reconcile Stripe dispute"),
                    "dispute ledger reconciliation failed"
                );
            }
        }

        Some(
            kind @ (StripeEventType::RefundCreated
            | StripeEventType::RefundUpdated
            | StripeEventType::RefundFailed),
        ) => {
            let provider_refund_id = data_object
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if !provider_refund_id.is_empty() {
                let ledger =
                    match repo::refunds::get_by_provider_refund_id(ctx, provider_refund_id).await {
                        Ok(ledger) => ledger,
                        Err(error) => fail_webhook!(
                            crud::db_error_internal(error, "Failed to load refund ledger"),
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
                        .unwrap_or(if kind == StripeEventType::RefundFailed {
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
                            crud::db_error_internal(error, "Failed to update refund ledger"),
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
                                crud::db_error_internal(
                                    error,
                                    "Failed to reconcile refund purchase"
                                ),
                                "refund purchase reconciliation failed"
                            );
                        }
                        if let Err(error) = repo::refunds::mark_succeeded(ctx, &ledger.id).await {
                            fail_webhook!(
                                crud::db_error_internal(error, "Failed to complete refund ledger"),
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
                                crud::db_error_internal(
                                    error,
                                    "Failed to complete provider operation"
                                ),
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
                                crud::db_error_internal(
                                    error,
                                    "Failed to resolve provider operation"
                                ),
                                "refund provider operation failure write failed"
                            );
                        }
                    }
                }
            }
        }

        Some(StripeEventType::ChargeRefunded) => {
            let payment_intent = data_object
                .get("payment_intent")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if !payment_intent.is_empty() {
                // A refunded charge for a foreign PaymentIntent is a benign
                // no-op, but only a definitive NotFound proves that. Any
                // other lookup error must fail the delivery as retryable —
                // sealing the event on a transient outage would permanently
                // drop a dashboard-initiated refund locally.
                let purchase =
                    match repo::purchases::find_by_payment_intent(ctx, &payment_intent).await {
                        Ok(purchase) => Some(purchase),
                        Err(error) if error.code == wafer_run::ErrorCode::NotFound => {
                            tracing::info!(
                                payment_intent_id = %payment_intent,
                                "Stripe refunded charge does not belong to a local commerce order"
                            );
                            None
                        }
                        Err(error) => fail_webhook!(
                            crud::db_error_internal(error, "Failed to load refunded purchase"),
                            "refunded purchase lookup failed"
                        ),
                    };
                if let Some(purchase) = purchase {
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
                            crud::db_error_internal(
                                error,
                                "Failed to update purchase refund total"
                            ),
                            "refunded charge purchase update failed"
                        );
                    }
                }
            }
        }

        None => {
            // A destination can be subscribed to more event types than this
            // block handles, so an unrecognised type is ordinary traffic:
            // ignore it, seal the lease below and acknowledge the delivery.
            // Answering anything else would make Stripe retry a type no
            // handler will ever want.
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
            return crud::db_error_internal(error, "Failed to complete webhook processing lease");
        }
    }

    ok_json(&WebhookAck::received())
}

/// Fire a webhook for product/billing events.
/// Best-effort — if PRODUCTS_WEBHOOK_URL is not configured, this is a no-op.
/// The webhook is signed with HMAC-SHA256 using PRODUCTS_WEBHOOK_SECRET.
/// A settings read that fails is logged and nothing is sent, like every other
/// failure here: the event it reports has already been processed.
async fn fire_products_webhook(ctx: &dyn Context, event: &str, data: &serde_json::Value) {
    let settings = async {
        Ok::<_, WaferError>((
            config::get_default(ctx, WEBHOOK_URL, "").await?,
            config::get_default(ctx, WEBHOOK_SECRET, "").await?,
        ))
    };
    let (url, secret) = match settings.await {
        Ok(settings) => settings,
        Err(e) => {
            tracing::warn!(event = %event, error = %e, "products webhook not sent: settings read failed");
            return;
        }
    };
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

/// Verify a Stripe webhook signature: HMAC-SHA256 over `timestamp.payload`.
///
/// `Stripe-Signature` carries a `t=` timestamp and one `v1=` signature *per
/// signing secret currently active on the endpoint*. Rolling a secret leaves
/// the retired one live for up to 24 hours, and every delivery in that window
/// is signed with both, so the header holds two `v1` values of which only one
/// matches the secret this deployment holds. The delivery is accepted when any
/// `v1` matches; reading a single value rejected every delivery for the whole
/// roll window whenever the retired secret's signature came second.
fn verify_stripe_signature(payload: &[u8], sig_header: &str, secret: &str) -> bool {
    let candidates = || {
        sig_header
            .split(',')
            .filter_map(|part| part.trim().strip_prefix("v1="))
    };
    let mut timestamp = "";
    for part in sig_header.split(',') {
        if let Some(t) = part.trim().strip_prefix("t=") {
            timestamp = t;
        }
    }

    if timestamp.is_empty() || candidates().all(str::is_empty) {
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

    // Constant-time comparison against each offered signature. Which of them
    // matches is not a secret — the header is attacker-supplied — so stopping
    // at the first match leaks nothing about `secret`.
    candidates().any(|candidate| {
        primitives::constant_time_eq(computed_hex.as_bytes(), candidate.as_bytes())
    })
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

/// Check if a user owns a product — either via an active subscription that
/// references it, or a completed purchase containing it as a line item.
///
/// All three reads propagate. This answer gates a purchase, and every one of
/// them used to collapse a failure into `false` (the middle one literally as
/// `Err(_) => false`), so a database outage told a buyer "you must sign in
/// and own the required product" for a product they had already bought — a
/// refusal that looks like their fault and that no retry can clear.
async fn user_owns_product(
    ctx: &dyn Context,
    user_id: &str,
    product_id: &str,
) -> Result<bool, WaferError> {
    // Active subscription whose plan references the product.
    if repo::subscriptions::active_plan_exists(ctx, user_id, product_id).await? {
        return Ok(true);
    }
    // Completed purchase containing this product as a line item.
    let purchase_ids: Vec<serde_json::Value> =
        repo::purchases::completed_purchase_ids(ctx, user_id)
            .await?
            .into_iter()
            .filter_map(|r| r.data.get("id").and_then(|v| v.as_str()).map(String::from))
            .map(serde_json::Value::String)
            .collect();
    repo::purchases::line_item_exists_for_product(ctx, purchase_ids, product_id).await
}

/// Sum the add-on totals a Stripe subscription's items report and write them
/// to the subscriber's row.
///
/// The per-unit amounts are read from the metadata the platform stamps on its
/// add-on objects. [`repo::subscriptions::ADDON_ITEM_MARKER`] is what makes an
/// item an add-on at all, and [`repo::subscriptions::ADDON_TOTALS`] owns the
/// metadata key for each total and the column it feeds. The block never needs a
/// list of the add-on packs that exist — only the totals Stripe reports.
///
/// The marker decides which object to read, not just whether to read one:
/// Stripe always serialises a subscription item's own `metadata`, as `{}` when
/// it is unset, so testing the item object for presence rather than for the
/// marker meant the price was never consulted and a price-stamped add-on
/// counted as zero. Whichever object carries the marker supplies the amounts
/// too — the objects are not merged.
///
/// The totals are quotas, so a quantity or an amount that is negative, or a
/// product or sum too large to represent, is refused rather than written: each
/// would otherwise hand the subscriber less capacity than none. That and a
/// failed write are both errors, and the caller answers Stripe with one so the
/// delivery is retried.
async fn sync_addon_totals_from_items(
    ctx: &dyn Context,
    user_id: &str,
    items: &serde_json::Value,
    event_created: i64,
) -> Result<(), WaferError> {
    let mut totals = [0i64; repo::subscriptions::ADDON_TOTALS.len()];
    let refuse = |detail: String| {
        WaferError::new(
            wafer_run::ErrorCode::InvalidArgument,
            format!("Stripe subscription item metadata {detail}"),
        )
    };

    if let Some(data) = items.get("data").and_then(|v| v.as_array()) {
        for item in data {
            let marked = |meta: &serde_json::Value| {
                meta.get(repo::subscriptions::ADDON_ITEM_MARKER).is_some()
            };
            let meta = item
                .get("metadata")
                .filter(|meta| marked(meta))
                .or_else(|| item.pointer("/price/metadata").filter(|meta| marked(meta)));
            // The base plan item carries the marker on neither object and
            // contributes nothing to the totals.
            let Some(meta) = meta else {
                continue;
            };

            let qty = item.get("quantity").and_then(|v| v.as_i64()).unwrap_or(1);
            if qty < 0 {
                return Err(refuse(format!("reports a negative quantity ({qty})")));
            }
            let parse = |key: &str| -> i64 {
                meta.get(key)
                    .and_then(|v| {
                        v.as_str()
                            .and_then(|s| s.parse().ok())
                            .or_else(|| v.as_i64())
                    })
                    .unwrap_or(0)
            };
            for (total, (metadata_key, _)) in
                totals.iter_mut().zip(repo::subscriptions::ADDON_TOTALS)
            {
                let per_unit = parse(metadata_key);
                if per_unit < 0 {
                    return Err(refuse(format!(
                        "reports a negative {metadata_key} ({per_unit})"
                    )));
                }
                *total = per_unit
                    .checked_mul(qty)
                    .and_then(|line| total.checked_add(line))
                    .ok_or_else(|| refuse(format!("overflows the {metadata_key} add-on total")))?;
            }
        }
    }

    repo::subscriptions::set_addon_totals(ctx, user_id, totals, event_created).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::products::config::STRIPE_WEBHOOK_SECRET;

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

    /// Rolling a webhook secret leaves the retired one live for up to 24
    /// hours, and Stripe signs every delivery in that window with both: the
    /// header carries one `v1` per active secret, in an order the endpoint
    /// does not choose. Reading a single value accepted the delivery only
    /// when the held secret's signature happened to come last.
    #[test]
    fn test_verify_stripe_signature_accepts_either_secret_during_a_roll() {
        let held = "whsec_current";
        let retired = "whsec_retired";
        let payload = b"{\"type\":\"customer.subscription.updated\"}";
        let timestamp = chrono::Utc::now().timestamp() as u64;
        let sign = |secret: &str| {
            hex_encode(&primitives::hmac_sha256(
                secret.as_bytes(),
                &build_signed_payload(timestamp, payload),
            ))
        };
        let held_sig = sign(held);
        let retired_sig = sign(retired);

        assert!(
            verify_stripe_signature(
                payload,
                &format!("t={timestamp},v1={retired_sig},v1={held_sig}"),
                held
            ),
            "the held secret's signature last must verify"
        );
        assert!(
            verify_stripe_signature(
                payload,
                &format!("t={timestamp},v1={held_sig},v1={retired_sig}"),
                held
            ),
            "the held secret's signature first must verify just the same"
        );
        assert!(
            !verify_stripe_signature(
                payload,
                &format!("t={timestamp},v1={retired_sig},v1={retired_sig}"),
                held
            ),
            "a header carrying no signature from the held secret must not verify"
        );
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
            offer_pricing::InputScope::Public,
        )
        .unwrap();
        let request: CheckoutRequest = serde_json::from_value(serde_json::json!({
            "offer_id": offer.id,
            "presentation": "hosted"
        }))
        .unwrap();
        let form = encode_form(
            build_offer_checkout_form(
                &offer,
                &preview,
                "Monthly service",
                "order_subscription",
                &request,
                "https://shop.example/success",
                "https://shop.example/cancel",
                false,
                CountryCode::parse("NZ").as_ref(),
                110,
                275,
            )
            .unwrap(),
        );
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
            offer_pricing::InputScope::Public,
        )
        .unwrap();
        let request: CheckoutRequest = serde_json::from_value(serde_json::json!({
            "offer_id": offer.id,
            "presentation": "hosted"
        }))
        .unwrap();
        let checkout = encode_form(
            build_offer_checkout_form(
                &offer,
                &preview,
                "Shipped product",
                "order_shipping",
                &request,
                "https://shop.example/success",
                "https://shop.example/cancel",
                false,
                CountryCode::parse("US").as_ref(),
                0,
                0,
            )
            .unwrap(),
        );
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
            "",
            None,
            false,
            CountryCode::parse("US").as_ref(),
            0,
            0,
        )
        .unwrap_err();
        assert!(error.contains("Stripe shipping rate ID"));

        offer.checkout.shipping_options[0].stripe_shipping_rate_id = "shr_standard_123".into();
        let payment_link = encode_form(
            payment_link_form(
                &offer,
                &preview,
                "Shipped product",
                "",
                None,
                false,
                CountryCode::parse("US").as_ref(),
                0,
                0,
            )
            .unwrap(),
        );
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
        ctx.set_config(STRIPE_WEBHOOK_SECRET, secret);

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
        ctx.set_config(STRIPE_WEBHOOK_SECRET, secret);

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
        ctx.set_config(STRIPE_WEBHOOK_SECRET, secret);

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
        status: EventStatus,
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
        ctx.set_config(STRIPE_WEBHOOK_SECRET, secret);

        seed_stripe_event_row(&ctx, "evt_pending_retry", EventStatus::Pending).await;

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
            Some(crate::util::wire_str(&EventStatus::Processed).as_str()),
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
        ctx.set_config(STRIPE_WEBHOOK_SECRET, secret);

        seed_stripe_event_row(&ctx, "evt_already_processed", EventStatus::Processed).await;

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
            serde_json::json!(EventStatus::Processing),
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
        assert_eq!(
            row.str_field("status"),
            crate::util::wire_str(&EventStatus::Failed)
        );
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
            repo::MAX_ATTEMPTS,
            "permanent failure",
        )
        .await
        .expect("dead-letter event");
        let row = db::get(&ctx, STRIPE_EVENTS_TABLE, "evt_failure")
            .await
            .expect("dead-letter row");
        assert_eq!(
            row.str_field("status"),
            crate::util::wire_str(&EventStatus::DeadLetter)
        );
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
                    serde_json::json!(EventStatus::DeadLetter),
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

        let list = list_webhook_events(&ctx, Some(EventStatus::DeadLetter), 1, 20)
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
        ctx.set_config(STRIPE_WEBHOOK_SECRET, "whsec_manual_replay");
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
                    serde_json::json!(EventStatus::DeadLetter),
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
        assert_eq!(
            replayed.str_field("status"),
            crate::util::wire_str(&EventStatus::Processed)
        );
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
                ("status".to_string(), serde_json::json!(EventStatus::Failed)),
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
