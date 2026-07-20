//! Reusable Stripe Payment Link synchronization records.

use std::collections::HashMap;

use serde_json::Value;
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    blocks::products::contracts::{ManagedPaymentLink, PricingPreview, StorefrontPaymentLink},
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
    })
}

fn offer_filter(offer_id: &str) -> Filter {
    Filter {
        field: "offer_id".to_string(),
        operator: FilterOp::Equal,
        value: Value::String(offer_id.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_pending(
    ctx: &dyn Context,
    offer_id: &str,
    preset_id: &str,
    seller_account_id: &str,
    stripe_account_id: &str,
    livemode: bool,
    configuration_hash: &str,
    pricing_snapshot: &PricingPreview,
    fee_basis_points: u16,
) -> Result<StoredPaymentLink, WaferError> {
    let id = uuid::Uuid::now_v7().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let data = HashMap::from([
        ("id".to_string(), Value::String(id)),
        ("offer_id".to_string(), Value::String(offer_id.to_string())),
        (
            "preset_id".to_string(),
            Value::String(preset_id.to_string()),
        ),
        (
            "seller_account_id".to_string(),
            Value::String(seller_account_id.to_string()),
        ),
        (
            "stripe_account_id".to_string(),
            Value::String(stripe_account_id.to_string()),
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
        (
            "pricing_snapshot".to_string(),
            Value::String(serde_json::to_string(pricing_snapshot).map_err(|error| {
                WaferError::new(
                    ErrorCode::Internal,
                    format!("could not encode Payment Link pricing snapshot: {error}"),
                )
            })?),
        ),
        (
            "fee_basis_points".to_string(),
            Value::from(fee_basis_points),
        ),
        (
            "sync_status".to_string(),
            Value::String("syncing".to_string()),
        ),
        ("sync_error".to_string(), Value::String(String::new())),
        ("created_at".to_string(), Value::String(now.clone())),
        ("updated_at".to_string(), Value::String(now)),
    ]);
    hydrate(db::create(ctx, TABLE, data).await?)
}

pub(crate) async fn mark_synced(
    ctx: &dyn Context,
    id: &str,
    stripe_payment_link_id: &str,
    url: &str,
) -> Result<StoredPaymentLink, WaferError> {
    hydrate(
        db::update(
            ctx,
            TABLE,
            id,
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
        .await?,
    )
}

pub(crate) async fn mark_error(
    ctx: &dyn Context,
    id: &str,
    message: &str,
) -> Result<StoredPaymentLink, WaferError> {
    hydrate(
        db::update(
            ctx,
            TABLE,
            id,
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
        .await?,
    )
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
    let mut records = db::list_all(ctx, TABLE, vec![offer_filter(offer_id)]).await?;
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

pub(crate) async fn find_reusable(
    ctx: &dyn Context,
    offer_id: &str,
    preset_id: &str,
    configuration_hash: &str,
) -> Result<Option<ManagedPaymentLink>, WaferError> {
    let rows = db::list_all(
        ctx,
        TABLE,
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
                value: Value::Bool(true),
            },
            Filter {
                field: "sync_status".to_string(),
                operator: FilterOp::Equal,
                value: Value::String("synced".to_string()),
            },
        ],
    )
    .await?;
    rows.into_iter()
        .next()
        .map(hydrate)
        .transpose()
        .map(|stored| stored.map(|stored| stored.managed))
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
    let records = db::list_all(
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
