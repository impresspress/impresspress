//! Reusable Stripe Payment Link synchronization records.

use std::collections::HashMap;

use serde_json::Value;
use wafer_block::{
    db::{Filter, FilterOp, SortField},
    wire::database::OnConflict,
};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    blocks::products::contracts::{ManagedPaymentLink, PricingPreview, StorefrontPaymentLink},
    db_read::{self, Bound},
    util::RecordExt,
};

pub(crate) const TABLE: &str = "impresspress__products__payment_links";

#[derive(Debug, Clone)]
pub(crate) struct StoredPaymentLink {
    pub managed: ManagedPaymentLink,
    pub seller_account_id: String,
    pub stripe_payment_link_id: String,
    pub stripe_account_id: String,
    pub livemode: bool,
    pub pricing_snapshot: Option<PricingPreview>,
    pub fee_basis_points: u16,
    /// The form parameters of the attempt this row has in flight, in the
    /// order they were sent. Empty when no attempt has been sent, which is
    /// also the state of a row written before the column existed.
    pub stripe_request: Vec<(String, String)>,
    /// When [`Self::stripe_request`] was recorded, RFC 3339. Empty alongside
    /// an empty request.
    pub stripe_request_at: String,
}

/// The values one synchronization attempt sends to Stripe on behalf of a row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Attempt<'a> {
    pub seller_account_id: &'a str,
    pub stripe_account_id: &'a str,
    pub pricing_snapshot: &'a PricingPreview,
    pub fee_basis_points: u16,
    /// The exact form this attempt sends, which with the account decides its
    /// idempotency key. Recorded before the request goes out so a
    /// deactivation can re-send it and learn what Stripe did with it.
    pub request: &'a [(String, String)],
}

fn hydrate(record: Record) -> Result<StoredPaymentLink, WaferError> {
    let pricing_snapshot = match record.data.get("pricing_snapshot") {
        None | Some(Value::Null) => None,
        Some(Value::String(raw)) if raw.is_empty() || raw == "{}" => None,
        Some(Value::String(raw)) => Some(serde_json::from_str(raw).map_err(|error| {
            WaferError::new(
                ErrorCode::Internal,
                format!("invalid persisted Payment Link pricing snapshot: {error}"),
            )
        })?),
        Some(Value::Object(value)) if value.is_empty() => None,
        Some(value) => Some(serde_json::from_value(value.clone()).map_err(|error| {
            WaferError::new(
                ErrorCode::Internal,
                format!("invalid persisted Payment Link pricing snapshot: {error}"),
            )
        })?),
    };
    let fee_basis_points = u16::try_from(record.i64_field("fee_basis_points"))
        .ok()
        .filter(|fee| *fee <= 10_000)
        .ok_or_else(|| {
            WaferError::new(
                ErrorCode::Internal,
                "invalid persisted Payment Link application fee",
            )
        })?;
    // Stored as JSON text, which the database layer may hand back already
    // decoded — as `pricing_snapshot` above is.
    let invalid_request = |error: serde_json::Error| {
        WaferError::new(
            ErrorCode::Internal,
            format!("invalid persisted Payment Link request: {error}"),
        )
    };
    let stripe_request: Vec<(String, String)> = match record.data.get("stripe_request") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(raw)) if raw.is_empty() => Vec::new(),
        Some(Value::String(raw)) => serde_json::from_str(raw).map_err(invalid_request)?,
        Some(value) => serde_json::from_value(value.clone()).map_err(invalid_request)?,
    };
    Ok(StoredPaymentLink {
        managed: ManagedPaymentLink {
            id: record.id.clone(),
            offer_id: record.str_field("offer_id").to_string(),
            preset_id: record.str_field("preset_id").to_string(),
            url: record.str_field("url").to_string(),
            active: record.bool_field("active"),
            configuration_hash: record.str_field("configuration_hash").to_string(),
            sync_status: record.str_field("sync_status").to_string(),
            sync_error: record.str_field("sync_error").to_string(),
        },
        seller_account_id: record.str_field("seller_account_id").to_string(),
        stripe_payment_link_id: record.str_field("stripe_payment_link_id").to_string(),
        stripe_account_id: record.str_field("stripe_account_id").to_string(),
        livemode: record.bool_field("livemode"),
        pricing_snapshot,
        fee_basis_points,
        stripe_request,
        stripe_request_at: record.str_field("stripe_request_at").to_string(),
    })
}

fn offer_filter(offer_id: &str) -> Filter {
    Filter {
        field: "offer_id".to_string(),
        operator: FilterOp::Equal,
        value: Value::String(offer_id.to_string()),
    }
}

/// The columns one synchronization attempt writes on its row. A retry of the
/// same configuration rewrites them, so the row always describes the attempt
/// that is in flight.
fn attempt_fields(attempt: &Attempt<'_>) -> Result<HashMap<String, Value>, WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    Ok(HashMap::from([
        (
            "seller_account_id".to_string(),
            Value::String(attempt.seller_account_id.to_string()),
        ),
        (
            "stripe_account_id".to_string(),
            Value::String(attempt.stripe_account_id.to_string()),
        ),
        (
            "pricing_snapshot".to_string(),
            Value::String(
                serde_json::to_string(attempt.pricing_snapshot).map_err(|error| {
                    WaferError::new(
                        ErrorCode::Internal,
                        format!("could not encode Payment Link pricing snapshot: {error}"),
                    )
                })?,
            ),
        ),
        (
            "fee_basis_points".to_string(),
            Value::from(attempt.fee_basis_points),
        ),
        (
            "stripe_request".to_string(),
            Value::String(serde_json::to_string(attempt.request).map_err(|error| {
                WaferError::new(
                    ErrorCode::Internal,
                    format!("could not encode Payment Link request: {error}"),
                )
            })?),
        ),
        ("stripe_request_at".to_string(), Value::String(now.clone())),
        (
            "sync_status".to_string(),
            Value::String("syncing".to_string()),
        ),
        ("sync_error".to_string(), Value::String(String::new())),
        ("updated_at".to_string(), Value::String(now)),
    ]))
}

fn configuration_filters(
    offer_id: &str,
    preset_id: &str,
    configuration_hash: &str,
    active: bool,
) -> Vec<Filter> {
    vec![
        offer_filter(offer_id),
        Filter {
            field: "preset_id".to_string(),
            operator: FilterOp::Equal,
            value: Value::String(preset_id.to_string()),
        },
        Filter {
            field: "configuration_hash".to_string(),
            operator: FilterOp::Equal,
            value: Value::String(configuration_hash.to_string()),
        },
        Filter {
            field: "active".to_string(),
            operator: FilterOp::Equal,
            value: Value::Bool(active),
        },
    ]
}

fn not_synced_filter() -> Filter {
    Filter {
        field: "sync_status".to_string(),
        operator: FilterOp::NotEqual,
        value: Value::String("synced".to_string()),
    }
}

fn id_filter(id: &str) -> Filter {
    Filter {
        field: "id".to_string(),
        operator: FilterOp::Equal,
        value: Value::String(id.to_string()),
    }
}

/// The row id for generation `generation` of one configuration.
///
/// Every attempt at a configuration derives the same id, so two concurrent
/// first attempts meet on the primary key instead of inserting two rows, and
/// the id (which the Stripe request carries as metadata) is the same on every
/// retry. A generation ends when its row is retired — deactivated by an
/// owner, or refused outright by Stripe — so the next request for the same
/// configuration gets a new id.
fn configuration_link_id(
    offer_id: &str,
    preset_id: &str,
    configuration_hash: &str,
    generation: i64,
) -> String {
    let digest = wafer_block::hash::sha256_hex(
        format!("{offer_id}\n{preset_id}\n{configuration_hash}\n{generation}").as_bytes(),
    );
    let mut bytes = [0u8; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digest[index * 2..index * 2 + 2], 16).unwrap_or_default();
    }
    uuid::Builder::from_custom_bytes(bytes)
        .into_uuid()
        .to_string()
}

/// The row id the next attempt at a configuration uses when no active row
/// exists for it: the id of the configuration's current generation.
///
/// The generation is the number of retired rows for the configuration. Rows
/// are never deleted and a retired row never comes back, so the count only
/// grows and every retired row's generation is below it. The caller needs
/// the id before the row is written, because the id travels in the Stripe
/// request's metadata and therefore decides the request's idempotency key.
pub(crate) async fn pending_id(
    ctx: &dyn Context,
    offer_id: &str,
    preset_id: &str,
    configuration_hash: &str,
) -> Result<String, WaferError> {
    let generation = db::count(
        ctx,
        TABLE,
        &configuration_filters(offer_id, preset_id, configuration_hash, false),
    )
    .await?;
    Ok(configuration_link_id(
        offer_id,
        preset_id,
        configuration_hash,
        generation,
    ))
}

/// Insert the row [`pending_id`] named (unless a concurrent attempt already
/// did) and start an attempt on it; see [`restart_pending`] for what comes
/// back.
pub(crate) async fn create_pending(
    ctx: &dyn Context,
    id: &str,
    offer_id: &str,
    preset_id: &str,
    livemode: bool,
    configuration_hash: &str,
    attempt: &Attempt<'_>,
) -> Result<StoredPaymentLink, WaferError> {
    let mut data = attempt_fields(attempt)?;
    let created_at = data["updated_at"].clone();
    data.extend([
        ("id".to_string(), Value::String(id.to_string())),
        ("offer_id".to_string(), Value::String(offer_id.to_string())),
        (
            "preset_id".to_string(),
            Value::String(preset_id.to_string()),
        ),
        ("livemode".to_string(), Value::Bool(livemode)),
        (
            "stripe_payment_link_id".to_string(),
            Value::String(String::new()),
        ),
        (
            "stripe_buy_button_id".to_string(),
            Value::String(String::new()),
        ),
        ("url".to_string(), Value::String(String::new())),
        ("active".to_string(), Value::Bool(true)),
        (
            "configuration_hash".to_string(),
            Value::String(configuration_hash.to_string()),
        ),
        ("created_at".to_string(), created_at),
    ]);
    // Insert-or-ignore on the primary key: a concurrent first attempt that
    // got there first keeps its row, and this attempt joins it below.
    db::upsert(
        ctx,
        TABLE,
        data.into_iter().collect(),
        vec!["id".to_string()],
        OnConflict::SetColumns(Vec::new()),
    )
    .await?;
    restart_pending(ctx, id, attempt).await
}

/// Start an attempt on an active, not-yet-synced row: put it in `syncing`
/// and record what this attempt sends.
///
/// Returns the row as it then stands. It is `syncing` when this attempt
/// started, or `synced` when a concurrent attempt recorded the link first,
/// and the caller reuses that link. A row retired in the meantime is an
/// `Aborted` error: the request for it is no longer current.
pub(crate) async fn restart_pending(
    ctx: &dyn Context,
    id: &str,
    attempt: &Attempt<'_>,
) -> Result<StoredPaymentLink, WaferError> {
    let started = db::update_by_filters_count(
        ctx,
        TABLE,
        vec![
            id_filter(id),
            Filter {
                field: "active".to_string(),
                operator: FilterOp::Equal,
                value: Value::Bool(true),
            },
            not_synced_filter(),
        ],
        attempt_fields(attempt)?,
    )
    .await?;
    let stored = hydrate(db::get(ctx, TABLE, id).await?)?;
    if started == 0 && !(stored.managed.active && stored.managed.sync_status == "synced") {
        return Err(WaferError::new(
            ErrorCode::Aborted,
            "the Payment Link was retired while this attempt started; retry",
        ));
    }
    Ok(stored)
}

/// Record the link Stripe created against its row, but only while the row is
/// still active. `None` means the row was retired while Stripe was minting
/// the link: nothing local will ever point at that link, so the caller has to
/// take it down at Stripe rather than leave a live link no row records.
///
/// A row a concurrent twin already marked `synced` is updated again — both
/// attempts hold the same link id under the same idempotency key, so the
/// write is the same write.
pub(crate) async fn mark_synced(
    ctx: &dyn Context,
    id: &str,
    stripe_payment_link_id: &str,
    url: &str,
) -> Result<Option<StoredPaymentLink>, WaferError> {
    let recorded = db::update_by_filters_count(
        ctx,
        TABLE,
        vec![
            id_filter(id),
            Filter {
                field: "active".to_string(),
                operator: FilterOp::Equal,
                value: Value::Bool(true),
            },
        ],
        HashMap::from([
            (
                "stripe_payment_link_id".to_string(),
                Value::String(stripe_payment_link_id.to_string()),
            ),
            ("url".to_string(), Value::String(url.to_string())),
            (
                "sync_status".to_string(),
                Value::String("synced".to_string()),
            ),
            ("sync_error".to_string(), Value::String(String::new())),
            (
                "updated_at".to_string(),
                Value::String(chrono::Utc::now().to_rfc3339()),
            ),
        ]),
    )
    .await?;
    if recorded == 0 {
        return Ok(None);
    }
    hydrate(db::get(ctx, TABLE, id).await?).map(Some)
}

/// Record an attempt whose outcome at Stripe is unknown (a transport
/// failure, a 429 or 5xx, a 409 while a twin attempt runs). The row stays
/// active, and a retry re-sends the same request under the same key. A row
/// that a concurrent attempt already recorded as synced is left alone.
pub(crate) async fn mark_error(
    ctx: &dyn Context,
    id: &str,
    message: &str,
) -> Result<(), WaferError> {
    db::update_by_filters(
        ctx,
        TABLE,
        vec![id_filter(id), not_synced_filter()],
        HashMap::from([
            (
                "sync_status".to_string(),
                Value::String("error".to_string()),
            ),
            ("sync_error".to_string(), Value::String(message.to_string())),
            (
                "updated_at".to_string(),
                Value::String(chrono::Utc::now().to_rfc3339()),
            ),
        ]),
    )
    .await
}

/// Record a request Stripe definitely refused: retire the row (inactive,
/// `error`), so its generation, row id and idempotency key end with it. A
/// retry then sends a new request under a new key instead of replaying the
/// refusal Stripe saved under the old one.
pub(crate) async fn mark_rejected(
    ctx: &dyn Context,
    id: &str,
    message: &str,
) -> Result<(), WaferError> {
    db::update_by_filters(
        ctx,
        TABLE,
        vec![id_filter(id), not_synced_filter()],
        HashMap::from([
            ("active".to_string(), Value::Bool(false)),
            (
                "sync_status".to_string(),
                Value::String("error".to_string()),
            ),
            ("sync_error".to_string(), Value::String(message.to_string())),
            (
                "updated_at".to_string(),
                Value::String(chrono::Utc::now().to_rfc3339()),
            ),
        ]),
    )
    .await
}

/// One row by id, without the offer scoping [`get_for_offer`] applies. For
/// the callers that already hold the row id as their subject — the durable
/// takedown operation, which is keyed on it.
pub(crate) async fn get(ctx: &dyn Context, link_id: &str) -> Result<StoredPaymentLink, WaferError> {
    hydrate(db::get(ctx, TABLE, link_id).await?)
}

/// Record the Stripe link a row turned out to own, whatever state the row is
/// in.
///
/// Unlike [`mark_synced`] this does not make the row `synced` and does not
/// require it to be active: its subject is a link that exists at Stripe and
/// that nothing local names, including one minted for a row that was retired
/// while Stripe was creating it. Writing the id is what makes taking that
/// link down a retryable operation instead of a one-shot attempt.
pub(crate) async fn record_stripe_link(
    ctx: &dyn Context,
    link_id: &str,
    stripe_payment_link_id: &str,
) -> Result<(), WaferError> {
    db::update(
        ctx,
        TABLE,
        link_id,
        HashMap::from([
            (
                "stripe_payment_link_id".to_string(),
                Value::String(stripe_payment_link_id.to_string()),
            ),
            (
                "updated_at".to_string(),
                Value::String(chrono::Utc::now().to_rfc3339()),
            ),
        ]),
    )
    .await?;
    Ok(())
}

pub(crate) async fn get_for_offer(
    ctx: &dyn Context,
    offer_id: &str,
    link_id: &str,
) -> Result<StoredPaymentLink, WaferError> {
    let record = db::get(ctx, TABLE, link_id).await?;
    if record.str_field("offer_id") != offer_id {
        return Err(WaferError::new(
            ErrorCode::NotFound,
            "payment link not found",
        ));
    }
    hydrate(record)
}

pub(crate) async fn list_for_offer(
    ctx: &dyn Context,
    offer_id: &str,
) -> Result<Vec<ManagedPaymentLink>, WaferError> {
    let mut records = db_read::list_bounded(
        ctx,
        TABLE,
        vec![offer_filter(offer_id)],
        Bound::OnePer("payment link on one offer"),
    )
    .await?;
    records.sort_by(|left, right| {
        left.data["created_at"]
            .to_string()
            .cmp(&right.data["created_at"].to_string())
    });
    records
        .into_iter()
        .map(hydrate)
        .map(|stored| stored.map(|stored| stored.managed))
        .collect()
}

/// The active local row for one Payment Link configuration.
pub(crate) enum ConfiguredLink {
    /// Stripe holds the link and the row records it: reuse it as it is.
    Synced(ManagedPaymentLink),
    /// A row whose last attempt did not finish (`syncing` or `error`). Stripe
    /// may or may not hold a link for it; a retry re-drives this row rather
    /// than inserting another. Carries the row id.
    Unfinished(String),
}

/// Find the active row for `(offer, preset, configuration)`, preferring a
/// synced one. Of several unfinished rows the oldest is returned (by
/// `created_at`, then id), so every retry re-drives the same one.
pub(crate) async fn find_for_configuration(
    ctx: &dyn Context,
    offer_id: &str,
    preset_id: &str,
    configuration_hash: &str,
) -> Result<Option<ConfiguredLink>, WaferError> {
    let rows = db_read::list_bounded_sorted(
        ctx,
        TABLE,
        configuration_filters(offer_id, preset_id, configuration_hash, true),
        // The database breaks a `created_at` tie on the primary key, `id`,
        // ascending: a sorted `list` ends its `ORDER BY` with the table's key,
        // and this table has one (`id TEXT PRIMARY KEY`,
        // `005_commerce_v2.sqlite.sql`).
        vec![SortField {
            field: "created_at".to_string(),
            desc: false,
        }],
        Bound::OnePer("payment link on one offer preset"),
    )
    .await?;
    let mut unfinished = None;
    for record in rows {
        let stored = hydrate(record)?;
        if stored.managed.sync_status == "synced" {
            return Ok(Some(ConfiguredLink::Synced(stored.managed)));
        }
        unfinished.get_or_insert(stored.managed.id);
    }
    Ok(unfinished.map(ConfiguredLink::Unfinished))
}

pub(crate) async fn deactivate_local(
    ctx: &dyn Context,
    offer_id: &str,
    link_id: &str,
) -> Result<ManagedPaymentLink, WaferError> {
    get_for_offer(ctx, offer_id, link_id).await?;
    Ok(hydrate(
        db::update(
            ctx,
            TABLE,
            link_id,
            HashMap::from([
                ("active".to_string(), Value::Bool(false)),
                (
                    "updated_at".to_string(),
                    Value::String(chrono::Utc::now().to_rfc3339()),
                ),
            ]),
        )
        .await?,
    )?
    .managed)
}

pub(crate) async fn list_public_for_offer(
    ctx: &dyn Context,
    offer_id: &str,
) -> Result<Vec<StorefrontPaymentLink>, WaferError> {
    let records = db_read::list_bounded(
        ctx,
        TABLE,
        vec![
            offer_filter(offer_id),
            Filter {
                field: "active".to_string(),
                operator: FilterOp::Equal,
                value: Value::Bool(true),
            },
            Filter {
                field: "sync_status".to_string(),
                operator: FilterOp::Equal,
                value: Value::String("synced".to_string()),
            },
        ],
        Bound::OnePer("payment link on one offer"),
    )
    .await?;
    let mut public = Vec::new();
    for record in records {
        let stored = hydrate(record)?;
        // Links created before immutable snapshots were introduced cannot be
        // represented truthfully on a static page. Keep them out of the
        // public projection until an owner re-synchronizes them.
        let Some(pricing) = stored.pricing_snapshot else {
            continue;
        };
        public.push(StorefrontPaymentLink {
            id: stored.managed.id,
            preset_id: stored.managed.preset_id,
            url: stored.managed.url,
            pricing,
        });
    }
    Ok(public)
}
