//! Durable refund-operation ledger and per-purchase serialization claim.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::util::RecordExt;

pub(crate) const TABLE: &str = "impresspress__products__refunds";

pub(crate) struct RefundClaim {
    pub purchase_id: String,
    pub payment_intent_id: String,
    pub stripe_account_id: String,
    pub idempotency_key: String,
    pub amount_minor: i64,
    pub target_refunded_total_minor: i64,
    pub currency: String,
    pub provider_reason: String,
    pub note: String,
    pub refunded_by: String,
    pub livemode: bool,
}

pub(crate) struct RefundWebhookResult {
    pub record: Record,
    pub applied: bool,
}

async fn get_by_field_optional(
    ctx: &dyn Context,
    field: &str,
    value: &str,
) -> Result<Option<Record>, WaferError> {
    match db::get_by_field(ctx, TABLE, field, serde_json::json!(value)).await {
        Ok(record) => Ok(Some(record)),
        Err(error) if error.code == ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) async fn get_by_idempotency_key(
    ctx: &dyn Context,
    idempotency_key: &str,
) -> Result<Option<Record>, WaferError> {
    get_by_field_optional(ctx, "idempotency_key", idempotency_key).await
}

pub(crate) async fn get_by_provider_refund_id(
    ctx: &dyn Context,
    provider_refund_id: &str,
) -> Result<Option<Record>, WaferError> {
    get_by_field_optional(ctx, "provider_refund_id", provider_refund_id).await
}

async fn active_for_purchase(
    ctx: &dyn Context,
    purchase_id: &str,
) -> Result<Option<Record>, WaferError> {
    let mut records = db::list(
        ctx,
        TABLE,
        &ListOptions {
            filters: vec![
                Filter {
                    field: "purchase_id".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(purchase_id),
                },
                Filter {
                    field: "status".to_string(),
                    operator: FilterOp::In,
                    value: serde_json::json!(["pending", "provider_succeeded"]),
                },
            ],
            sort: vec![SortField {
                field: "created_at".to_string(),
                desc: true,
            }],
            limit: 1,
            skip_count: true,
            ..Default::default()
        },
    )
    .await?
    .records;
    Ok(records.pop())
}

fn validate_existing(record: &Record, claim: &RefundClaim) -> Result<(), WaferError> {
    if record.str_field("purchase_id") != claim.purchase_id
        || record.str_field("payment_intent_id") != claim.payment_intent_id
        || record.i64_field("amount_minor") != claim.amount_minor
        || record.i64_field("target_refunded_total_minor") != claim.target_refunded_total_minor
    {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            "refund idempotency key was already used for a different request",
        ));
    }
    Ok(())
}

/// Claim the only active refund slot for a purchase. Reusing the same
/// operation key returns its existing row; a different concurrent operation
/// is rejected by both this check and the partial unique database index.
pub(crate) async fn claim(ctx: &dyn Context, claim: &RefundClaim) -> Result<Record, WaferError> {
    if let Some(existing) = get_by_idempotency_key(ctx, &claim.idempotency_key).await? {
        validate_existing(&existing, claim)?;
        if matches!(existing.str_field("status"), "failed" | "canceled") {
            return db::update(
                ctx,
                TABLE,
                &existing.id,
                HashMap::from([
                    ("status".to_string(), serde_json::json!("pending")),
                    ("last_error".to_string(), serde_json::json!("")),
                    (
                        "updated_at".to_string(),
                        serde_json::json!(chrono::Utc::now().to_rfc3339()),
                    ),
                ]),
            )
            .await;
        }
        return Ok(existing);
    }
    if let Some(active) = active_for_purchase(ctx, &claim.purchase_id).await? {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            format!("refund {} is still being reconciled", active.id),
        ));
    }

    let now = chrono::Utc::now().to_rfc3339();
    let data = HashMap::from([
        (
            "purchase_id".to_string(),
            serde_json::json!(&claim.purchase_id),
        ),
        (
            "payment_intent_id".to_string(),
            serde_json::json!(&claim.payment_intent_id),
        ),
        (
            "stripe_account_id".to_string(),
            serde_json::json!(&claim.stripe_account_id),
        ),
        (
            "idempotency_key".to_string(),
            serde_json::json!(&claim.idempotency_key),
        ),
        (
            "amount_minor".to_string(),
            serde_json::json!(claim.amount_minor),
        ),
        (
            "target_refunded_total_minor".to_string(),
            serde_json::json!(claim.target_refunded_total_minor),
        ),
        ("currency".to_string(), serde_json::json!(&claim.currency)),
        ("status".to_string(), serde_json::json!("pending")),
        (
            "provider_reason".to_string(),
            serde_json::json!(&claim.provider_reason),
        ),
        ("note".to_string(), serde_json::json!(&claim.note)),
        (
            "refunded_by".to_string(),
            serde_json::json!(&claim.refunded_by),
        ),
        ("livemode".to_string(), serde_json::json!(claim.livemode)),
        ("created_at".to_string(), serde_json::json!(&now)),
        ("updated_at".to_string(), serde_json::json!(&now)),
    ]);
    match db::create(ctx, TABLE, data).await {
        Ok(record) => Ok(record),
        Err(create_error) => {
            if let Some(existing) = get_by_idempotency_key(ctx, &claim.idempotency_key).await? {
                validate_existing(&existing, claim)?;
                return Ok(existing);
            }
            if active_for_purchase(ctx, &claim.purchase_id)
                .await?
                .is_some()
            {
                return Err(WaferError::new(
                    ErrorCode::FailedPrecondition,
                    "another refund is still being reconciled",
                ));
            }
            Err(create_error)
        }
    }
}

pub(crate) async fn record_provider_response(
    ctx: &dyn Context,
    id: &str,
    provider_refund_id: &str,
    provider_status: &str,
    livemode: bool,
    response_json: &str,
) -> Result<Record, WaferError> {
    let current = db::get(ctx, TABLE, id).await?;
    if current.i64_field("stripe_event_created") > 0 {
        if !current.str_field("provider_refund_id").is_empty()
            && current.str_field("provider_refund_id") != provider_refund_id
        {
            return Err(WaferError::new(
                ErrorCode::FailedPrecondition,
                "refund provider identity changed during reconciliation",
            ));
        }
        return Ok(current);
    }
    let local_status = match provider_status {
        "succeeded" => "provider_succeeded",
        "failed" | "canceled" => "failed",
        _ => "pending",
    };
    let rows = db::update_by_filters_count(
        ctx,
        TABLE,
        vec![
            Filter {
                field: "id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(id),
            },
            Filter {
                field: "stripe_event_created".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(0),
            },
        ],
        HashMap::from([
            (
                "provider_refund_id".to_string(),
                serde_json::json!(provider_refund_id),
            ),
            (
                "provider_status".to_string(),
                serde_json::json!(provider_status),
            ),
            ("livemode".to_string(), serde_json::json!(livemode)),
            ("status".to_string(), serde_json::json!(local_status)),
            (
                "response_json".to_string(),
                serde_json::json!(response_json),
            ),
            (
                "updated_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            ),
        ]),
    )
    .await?;
    let updated = db::get(ctx, TABLE, id).await?;
    if rows == 1
        || (!updated.str_field("provider_refund_id").is_empty()
            && updated.str_field("provider_refund_id") == provider_refund_id)
    {
        Ok(updated)
    } else {
        Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "refund provider response changed concurrently",
        ))
    }
}

fn is_terminal_provider_status(status: &str) -> bool {
    matches!(status, "succeeded" | "failed" | "canceled")
}

/// Apply a Stripe refund event only when it is newer than the stored provider
/// projection. Equal-second collisions fail closed: terminal states outrank
/// non-terminal states and `requires_action` outranks `pending`. A genuinely
/// newer terminal contradiction is rejected for operator reconciliation
/// rather than silently rewriting purchase accounting.
pub(crate) async fn record_webhook_response(
    ctx: &dyn Context,
    id: &str,
    provider_refund_id: &str,
    provider_status: &str,
    livemode: bool,
    response_json: &str,
    event_created: i64,
) -> Result<RefundWebhookResult, WaferError> {
    if event_created < 0 {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            "refund event timestamp must not be negative",
        ));
    }
    for _ in 0..3 {
        let current = db::get(ctx, TABLE, id).await?;
        if current.str_field("provider_refund_id") != provider_refund_id {
            return Err(WaferError::new(
                ErrorCode::FailedPrecondition,
                "refund webhook provider identity does not match its ledger",
            ));
        }
        let current_created = current.i64_field("stripe_event_created");
        let current_provider_status = current.str_field("provider_status");
        if current_created > event_created {
            return Ok(RefundWebhookResult {
                record: current,
                applied: false,
            });
        }
        if current_created == event_created && current_provider_status != provider_status {
            let keep_current = is_terminal_provider_status(current_provider_status)
                || (current_provider_status == "requires_action" && provider_status == "pending");
            if keep_current {
                return Ok(RefundWebhookResult {
                    record: current,
                    applied: false,
                });
            }
        }
        if current_created < event_created
            && is_terminal_provider_status(current_provider_status)
            && current_provider_status != provider_status
        {
            return Err(WaferError::new(
                ErrorCode::FailedPrecondition,
                format!(
                    "refund terminal status changed from {current_provider_status} to {provider_status}; provider reconciliation is required"
                ),
            ));
        }
        let local_status = match provider_status {
            "succeeded" if current.str_field("status") == "succeeded" => "succeeded",
            "succeeded" => "provider_succeeded",
            "failed" | "canceled" => "failed",
            _ => "pending",
        };
        let rows = db::update_by_filters_count(
            ctx,
            TABLE,
            vec![
                Filter {
                    field: "id".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(id),
                },
                Filter {
                    field: "stripe_event_created".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(current_created),
                },
                Filter {
                    field: "provider_status".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(current_provider_status),
                },
            ],
            HashMap::from([
                (
                    "provider_status".to_string(),
                    serde_json::json!(provider_status),
                ),
                ("livemode".to_string(), serde_json::json!(livemode)),
                ("status".to_string(), serde_json::json!(local_status)),
                (
                    "response_json".to_string(),
                    serde_json::json!(response_json),
                ),
                (
                    "stripe_event_created".to_string(),
                    serde_json::json!(event_created),
                ),
                (
                    "updated_at".to_string(),
                    serde_json::json!(chrono::Utc::now().to_rfc3339()),
                ),
            ]),
        )
        .await?;
        if rows == 1 {
            return Ok(RefundWebhookResult {
                record: db::get(ctx, TABLE, id).await?,
                applied: true,
            });
        }
    }
    Err(WaferError::new(
        ErrorCode::FailedPrecondition,
        "refund webhook state changed concurrently; retry the event",
    ))
}

pub(crate) async fn mark_succeeded(ctx: &dyn Context, id: &str) -> Result<Record, WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    db::update(
        ctx,
        TABLE,
        id,
        HashMap::from([
            ("status".to_string(), serde_json::json!("succeeded")),
            (
                "provider_status".to_string(),
                serde_json::json!("succeeded"),
            ),
            ("completed_at".to_string(), serde_json::json!(&now)),
            ("updated_at".to_string(), serde_json::json!(&now)),
        ]),
    )
    .await
}

pub(crate) async fn mark_failed(
    ctx: &dyn Context,
    id: &str,
    message: &str,
) -> Result<Record, WaferError> {
    db::update(
        ctx,
        TABLE,
        id,
        HashMap::from([
            ("status".to_string(), serde_json::json!("failed")),
            ("last_error".to_string(), serde_json::json!(message)),
            (
                "updated_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            ),
        ]),
    )
    .await
}

/// Preserve the active claim after an ambiguous network/runtime failure. A
/// retry with the same key is safe; a different refund remains blocked.
pub(crate) async fn mark_retryable_error(
    ctx: &dyn Context,
    id: &str,
    message: &str,
) -> Result<Record, WaferError> {
    db::update(
        ctx,
        TABLE,
        id,
        HashMap::from([
            ("status".to_string(), serde_json::json!("pending")),
            ("last_error".to_string(), serde_json::json!(message)),
            (
                "updated_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            ),
        ]),
    )
    .await
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
