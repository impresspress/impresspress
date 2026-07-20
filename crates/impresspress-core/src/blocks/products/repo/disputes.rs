//! Durable Stripe dispute ledger, keyed by connected-account/provider identity.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::util::RecordExt;

pub(crate) const TABLE: &str = "impresspress__products__disputes";

pub(crate) struct DisputeSnapshot {
    pub purchase_id: String,
    pub seller_account_id: String,
    pub stripe_account_id: String,
    pub provider_dispute_id: String,
    pub provider_charge_id: String,
    pub payment_intent_id: String,
    pub status: String,
    pub amount_minor: i64,
    pub currency: String,
    pub reason: String,
    pub evidence_due_by: Option<String>,
    pub livemode: bool,
    pub event_created: i64,
}

fn supported_status(status: &str) -> bool {
    matches!(
        status,
        "warning_needs_response"
            | "warning_under_review"
            | "warning_closed"
            | "needs_response"
            | "under_review"
            | "won"
            | "lost"
            | "prevented"
    )
}

fn is_closed(status: &str) -> bool {
    matches!(status, "warning_closed" | "won" | "lost" | "prevented")
}

fn validate_snapshot(snapshot: &DisputeSnapshot) -> Result<(), WaferError> {
    if snapshot.purchase_id.is_empty()
        || snapshot.provider_dispute_id.is_empty()
        || snapshot.payment_intent_id.is_empty()
        || snapshot.amount_minor <= 0
        || snapshot.currency.is_empty()
        || snapshot.event_created < 0
        || !supported_status(&snapshot.status)
    {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            "dispute identity, supported status, positive amount, currency, and event timestamp are required",
        ));
    }
    Ok(())
}

fn validate_existing(record: &Record, snapshot: &DisputeSnapshot) -> Result<(), WaferError> {
    let existing_charge = record.str_field("provider_charge_id");
    if record.str_field("purchase_id") != snapshot.purchase_id
        || record.str_field("stripe_account_id") != snapshot.stripe_account_id
        || record.str_field("payment_intent_id") != snapshot.payment_intent_id
        || record.i64_field("amount_minor") != snapshot.amount_minor
        || !record
            .str_field("currency")
            .eq_ignore_ascii_case(&snapshot.currency)
        || record.bool_field("livemode") != snapshot.livemode
        || (!existing_charge.is_empty()
            && !snapshot.provider_charge_id.is_empty()
            && existing_charge != snapshot.provider_charge_id)
    {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "dispute webhook does not match its immutable order snapshot",
        ));
    }
    Ok(())
}

async fn find_existing(
    ctx: &dyn Context,
    provider_dispute_id: &str,
) -> Result<Option<Record>, WaferError> {
    match db::get_by_field(
        ctx,
        TABLE,
        "provider_dispute_id",
        serde_json::json!(provider_dispute_id),
    )
    .await
    {
        Ok(record) => Ok(Some(record)),
        Err(error) if error.code == ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

async fn update_existing(
    ctx: &dyn Context,
    existing: &Record,
    snapshot: &DisputeSnapshot,
) -> Result<Record, WaferError> {
    validate_existing(existing, snapshot)?;
    let now = chrono::Utc::now().to_rfc3339();
    let provider_charge_id = if snapshot.provider_charge_id.is_empty() {
        existing.str_field("provider_charge_id")
    } else {
        &snapshot.provider_charge_id
    };
    let closed_at = if is_closed(&snapshot.status) {
        let current = existing.str_field("closed_at");
        serde_json::json!(if current.is_empty() { &now } else { current })
    } else {
        serde_json::Value::Null
    };
    db::update_by_filters_count(
        ctx,
        TABLE,
        vec![
            Filter {
                field: "id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(&existing.id),
            },
            Filter {
                field: "event_created".to_string(),
                operator: FilterOp::LessEqual,
                value: serde_json::json!(snapshot.event_created),
            },
        ],
        HashMap::from([
            ("status".to_string(), serde_json::json!(&snapshot.status)),
            (
                "provider_charge_id".to_string(),
                serde_json::json!(provider_charge_id),
            ),
            ("reason".to_string(), serde_json::json!(&snapshot.reason)),
            (
                "evidence_due_by".to_string(),
                snapshot
                    .evidence_due_by
                    .as_ref()
                    .map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
            ),
            (
                "event_created".to_string(),
                serde_json::json!(snapshot.event_created),
            ),
            ("closed_at".to_string(), closed_at),
            ("updated_at".to_string(), serde_json::json!(now)),
        ]),
    )
    .await?;
    db::get(ctx, TABLE, &existing.id).await
}

pub(crate) async fn reconcile(
    ctx: &dyn Context,
    snapshot: &DisputeSnapshot,
) -> Result<Record, WaferError> {
    validate_snapshot(snapshot)?;
    if let Some(existing) = find_existing(ctx, &snapshot.provider_dispute_id).await? {
        return update_existing(ctx, &existing, snapshot).await;
    }

    let now = chrono::Utc::now().to_rfc3339();
    let data = HashMap::from([
        (
            "purchase_id".to_string(),
            serde_json::json!(&snapshot.purchase_id),
        ),
        (
            "seller_account_id".to_string(),
            serde_json::json!(&snapshot.seller_account_id),
        ),
        (
            "stripe_account_id".to_string(),
            serde_json::json!(&snapshot.stripe_account_id),
        ),
        (
            "provider_dispute_id".to_string(),
            serde_json::json!(&snapshot.provider_dispute_id),
        ),
        (
            "provider_charge_id".to_string(),
            serde_json::json!(&snapshot.provider_charge_id),
        ),
        (
            "payment_intent_id".to_string(),
            serde_json::json!(&snapshot.payment_intent_id),
        ),
        ("status".to_string(), serde_json::json!(&snapshot.status)),
        (
            "amount_minor".to_string(),
            serde_json::json!(snapshot.amount_minor),
        ),
        (
            "currency".to_string(),
            serde_json::json!(&snapshot.currency),
        ),
        ("reason".to_string(), serde_json::json!(&snapshot.reason)),
        (
            "evidence_due_by".to_string(),
            snapshot
                .evidence_due_by
                .as_ref()
                .map_or(serde_json::Value::Null, |value| serde_json::json!(value)),
        ),
        ("livemode".to_string(), serde_json::json!(snapshot.livemode)),
        (
            "event_created".to_string(),
            serde_json::json!(snapshot.event_created),
        ),
        (
            "closed_at".to_string(),
            if is_closed(&snapshot.status) {
                serde_json::json!(&now)
            } else {
                serde_json::Value::Null
            },
        ),
        ("created_at".to_string(), serde_json::json!(&now)),
        ("updated_at".to_string(), serde_json::json!(&now)),
    ]);
    match db::create(ctx, TABLE, data).await {
        Ok(record) => Ok(record),
        Err(create_error) => {
            if let Some(existing) = find_existing(ctx, &snapshot.provider_dispute_id).await? {
                return update_existing(ctx, &existing, snapshot).await;
            }
            Err(create_error)
        }
    }
}

pub(crate) async fn list_for_purchase(
    ctx: &dyn Context,
    purchase_id: &str,
) -> Result<Vec<Record>, WaferError> {
    Ok(db::list(
        ctx,
        TABLE,
        &ListOptions {
            filters: vec![Filter {
                field: "purchase_id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(purchase_id),
            }],
            sort: vec![SortField {
                field: "created_at".to_string(),
                desc: true,
            }],
            skip_count: true,
            ..Default::default()
        },
    )
    .await?
    .records)
}

pub(crate) async fn list_for_analytics(
    ctx: &dyn Context,
    seller_account_id: Option<&str>,
) -> Result<Vec<Record>, WaferError> {
    let filters = seller_account_id
        .filter(|value| !value.is_empty())
        .map(|value| {
            vec![Filter {
                field: "seller_account_id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(value),
            }]
        })
        .unwrap_or_default();
    db::list_all(ctx, TABLE, filters).await
}
