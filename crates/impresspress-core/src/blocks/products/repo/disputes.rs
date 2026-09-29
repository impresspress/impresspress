//! Durable Stripe dispute ledger, keyed by connected-account/provider identity.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    blocks::products::contracts::DisputeStatus,
    util::{enum_column, RecordExt},
};

pub(crate) const TABLE: &str = "impresspress__products__disputes";

pub(crate) struct DisputeSnapshot {
    pub purchase_id: String,
    pub seller_account_id: String,
    pub stripe_account_id: String,
    pub provider_dispute_id: String,
    pub provider_charge_id: String,
    pub payment_intent_id: String,
    /// The network state the delivery reported. Parsed at the webhook
    /// boundary, so an event carrying a status Stripe has added since this
    /// type was written is refused there rather than stored.
    pub status: DisputeStatus,
    pub amount_minor: i64,
    pub currency: String,
    pub reason: String,
    pub evidence_due_by: Option<String>,
    pub livemode: bool,
    pub event_created: i64,
}

/// The `status` column of a dispute row, as the enum that defines it.
///
/// The decode door for the two readers outside this module — the analytics
/// aggregate and the admin projection — so the eight network states are
/// spelled once, here, and not re-listed wherever a dispute is counted.
pub(crate) fn status_of(record: &Record) -> Result<DisputeStatus, WaferError> {
    enum_column(record, "status")
}

fn validate_snapshot(snapshot: &DisputeSnapshot) -> Result<(), WaferError> {
    if snapshot.purchase_id.is_empty()
        || snapshot.provider_dispute_id.is_empty()
        || snapshot.payment_intent_id.is_empty()
        || snapshot.amount_minor <= 0
        || snapshot.currency.is_empty()
        || snapshot.event_created < 0
    {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            "dispute identity, positive amount, currency, and event timestamp are required",
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
    let closed_at = if snapshot.status.is_closed() {
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
            ("status".to_string(), serde_json::json!(snapshot.status)),
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
        ("status".to_string(), serde_json::json!(snapshot.status)),
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
            if snapshot.status.is_closed() {
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

/// Dispute totals for the commerce analytics, one row per
/// `(currency, status)` pair.
///
/// A `GROUP BY` in the database, not a row scan in Rust: the dispute ledger
/// grows with every dispute the platform ever sees, so reading it row by row
/// to add the amounts up would stop being right as soon as it outgrew a
/// single unpaged read. The grouped statement stays exact at any table size
/// and transfers one row per currency-and-state pair.
///
/// Each returned record carries `currency`, `status`, `disputes`
/// (`COUNT(*)`), `amount` (`SUM(amount_minor)`) and `invalid_amounts` — the
/// number of rows in the group whose `amount_minor` is not positive, which
/// is the per-row integrity check the caller used to make while scanning.
pub(crate) async fn analytics_totals(
    ctx: &dyn Context,
    seller_account_id: Option<&str>,
) -> Result<Vec<Record>, WaferError> {
    use wafer_block::wire::database as wire;

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
    let req = wire::AggregateRequest {
        collection: TABLE.to_string(),
        select_columns: vec!["currency".into(), "status".into()],
        aggregates: vec![
            wire::AggregateColumnDef::Count {
                alias: "disputes".into(),
            },
            wire::AggregateColumnDef::Sum {
                field: "amount_minor".into(),
                alias: "amount".into(),
                cast_as: crate::util::bigint_cast(),
            },
            wire::AggregateColumnDef::CaseWhenSum {
                when: crate::util::to_wire_filters(&[Filter {
                    field: "amount_minor".to_string(),
                    operator: FilterOp::LessEqual,
                    value: serde_json::json!(0),
                }]),
                alias: "invalid_amounts".into(),
            },
        ],
        filters: crate::util::to_wire_filters(&filters),
        group_by: vec![
            wire::GroupByDef::Column("currency".into()),
            wire::GroupByDef::Column("status".into()),
        ],
        sort: vec![],
        limit: 0,
    };
    db::aggregate(ctx, req).await
}
