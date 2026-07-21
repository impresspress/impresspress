//! Durable idempotent Stripe mutation and reconciliation operations.

use std::collections::HashMap;

use wafer_block::{
    db::{Filter, FilterOp, ListOptions, SortField},
    wire::database::OnConflict,
};
use wafer_core::clients::database::{self as db, Record, RecordList};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::util::RecordExt;

pub(crate) const TABLE: &str = "impresspress__products__provider_operations";
pub(crate) const REFUND_RECONCILE: &str = "refund.reconcile";
const LEASE_SECONDS: i64 = 300;
const MAX_ATTEMPTS: u64 = 8;

pub(crate) struct OperationClaim {
    pub record: Record,
    pub owner: String,
    pub attempts: u64,
}

fn timestamp(record: &Record, field: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(record.str_field(field))
        .ok()
        .map(|value| value.with_timezone(&chrono::Utc))
}

fn retry_delay_seconds(attempts: u64) -> i64 {
    let exponent = attempts.saturating_sub(1).min(7) as u32;
    (30_i64.saturating_mul(2_i64.pow(exponent))).min(3600)
}

pub(crate) async fn ensure(
    ctx: &dyn Context,
    operation_type: &str,
    aggregate_type: &str,
    aggregate_id: &str,
    stripe_account_id: &str,
    idempotency_key: &str,
    request_json: &str,
) -> Result<Record, WaferError> {
    if operation_type.is_empty()
        || aggregate_type.is_empty()
        || aggregate_id.is_empty()
        || idempotency_key.is_empty()
    {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            "provider operation identity is incomplete",
        ));
    }
    let now = chrono::Utc::now().to_rfc3339();
    let id = format!(
        "op_{}",
        &wafer_block::hash::sha256_hex(idempotency_key.as_bytes())[..32]
    );
    db::upsert(
        ctx,
        TABLE,
        vec![
            ("id".to_string(), serde_json::json!(&id)),
            (
                "operation_type".to_string(),
                serde_json::json!(operation_type),
            ),
            (
                "aggregate_type".to_string(),
                serde_json::json!(aggregate_type),
            ),
            ("aggregate_id".to_string(), serde_json::json!(aggregate_id)),
            (
                "stripe_account_id".to_string(),
                serde_json::json!(stripe_account_id),
            ),
            (
                "idempotency_key".to_string(),
                serde_json::json!(idempotency_key),
            ),
            ("status".to_string(), serde_json::json!("pending")),
            ("request_json".to_string(), serde_json::json!(request_json)),
            ("created_at".to_string(), serde_json::json!(&now)),
            ("updated_at".to_string(), serde_json::json!(&now)),
        ],
        vec!["idempotency_key".to_string()],
        OnConflict::SetColumns(vec![]),
    )
    .await?;
    let record = db::get_by_field(
        ctx,
        TABLE,
        "idempotency_key",
        serde_json::json!(idempotency_key),
    )
    .await?;
    if record.str_field("operation_type") != operation_type
        || record.str_field("aggregate_type") != aggregate_type
        || record.str_field("aggregate_id") != aggregate_id
        || record.str_field("stripe_account_id") != stripe_account_id
    {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "provider-operation idempotency key was reused for another request",
        ));
    }
    Ok(record)
}

pub(crate) async fn list(
    ctx: &dyn Context,
    status: Option<&str>,
    page: i64,
    page_size: i64,
) -> Result<RecordList, WaferError> {
    let filters = status
        .map(|status| {
            vec![Filter {
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(status),
            }]
        })
        .unwrap_or_default();
    db::paginated_list(
        ctx,
        TABLE,
        page,
        page_size,
        filters,
        vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }],
    )
    .await
}

async fn dead_letter_unclaimed(ctx: &dyn Context, record: &Record) -> Result<(), WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    db::update_by_filters_count(
        ctx,
        TABLE,
        vec![
            Filter {
                field: "id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(&record.id),
            },
            Filter {
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(record.str_field("status")),
            },
            Filter {
                field: "processing_owner".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(record.str_field("processing_owner")),
            },
        ],
        HashMap::from([
            ("status".to_string(), serde_json::json!("dead_letter")),
            ("processing_owner".to_string(), serde_json::json!("")),
            ("processing_started_at".to_string(), serde_json::Value::Null),
            ("next_attempt_at".to_string(), serde_json::Value::Null),
            ("terminal_at".to_string(), serde_json::json!(&now)),
            ("updated_at".to_string(), serde_json::json!(&now)),
        ]),
    )
    .await?;
    Ok(())
}

pub(crate) async fn claim_due(
    ctx: &dyn Context,
    limit: usize,
) -> Result<Vec<OperationClaim>, WaferError> {
    let now_value = chrono::Utc::now();
    let candidates = db::list(
        ctx,
        TABLE,
        &ListOptions {
            filters: vec![Filter {
                field: "status".to_string(),
                operator: FilterOp::In,
                value: serde_json::json!(["pending", "failed", "processing"]),
            }],
            sort: vec![SortField {
                field: "created_at".to_string(),
                desc: false,
            }],
            limit: (limit.clamp(1, 100) * 4) as i64,
            skip_count: true,
            ..Default::default()
        },
    )
    .await?
    .records;
    let mut claimed = Vec::new();
    for record in candidates {
        if claimed.len() >= limit {
            break;
        }
        let eligible = match record.str_field("status") {
            "pending" => timestamp(&record, "next_attempt_at").is_none_or(|next| next <= now_value),
            "failed" => timestamp(&record, "next_attempt_at").is_none_or(|next| next <= now_value),
            "processing" => timestamp(&record, "processing_started_at").is_none_or(|started| {
                now_value.signed_duration_since(started).num_seconds() >= LEASE_SECONDS
            }),
            _ => false,
        };
        if !eligible {
            continue;
        }
        let attempts = record.u64_field("attempts").saturating_add(1);
        if attempts > MAX_ATTEMPTS {
            dead_letter_unclaimed(ctx, &record).await?;
            continue;
        }
        let owner = uuid::Uuid::now_v7().to_string();
        let now = now_value.to_rfc3339();
        let rows = db::update_by_filters_count(
            ctx,
            TABLE,
            vec![
                Filter {
                    field: "id".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(&record.id),
                },
                Filter {
                    field: "status".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(record.str_field("status")),
                },
                Filter {
                    field: "processing_owner".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(record.str_field("processing_owner")),
                },
            ],
            HashMap::from([
                ("status".to_string(), serde_json::json!("processing")),
                ("attempts".to_string(), serde_json::json!(attempts)),
                ("processing_owner".to_string(), serde_json::json!(&owner)),
                ("processing_started_at".to_string(), serde_json::json!(&now)),
                ("next_attempt_at".to_string(), serde_json::Value::Null),
                ("updated_at".to_string(), serde_json::json!(&now)),
            ]),
        )
        .await?;
        if rows == 1 {
            claimed.push(OperationClaim {
                record: db::get(ctx, TABLE, &record.id).await?,
                owner,
                attempts,
            });
        }
    }
    Ok(claimed)
}

pub(crate) async fn mark_completed(
    ctx: &dyn Context,
    id: &str,
    owner: &str,
    response_json: &str,
) -> Result<(), WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
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
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!("processing"),
            },
            Filter {
                field: "processing_owner".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(owner),
            },
        ],
        HashMap::from([
            ("status".to_string(), serde_json::json!("succeeded")),
            (
                "response_json".to_string(),
                serde_json::json!(response_json),
            ),
            ("processing_owner".to_string(), serde_json::json!("")),
            ("processing_started_at".to_string(), serde_json::Value::Null),
            ("next_attempt_at".to_string(), serde_json::Value::Null),
            ("last_error".to_string(), serde_json::json!("")),
            ("completed_at".to_string(), serde_json::json!(&now)),
            ("terminal_at".to_string(), serde_json::json!(&now)),
            ("updated_at".to_string(), serde_json::json!(&now)),
        ]),
    )
    .await?;
    if rows == 1 || db::get(ctx, TABLE, id).await?.str_field("status") == "succeeded" {
        Ok(())
    } else {
        Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "provider-operation lease was lost before completion",
        ))
    }
}

pub(crate) async fn mark_retry(
    ctx: &dyn Context,
    id: &str,
    owner: &str,
    attempts: u64,
    message: &str,
) -> Result<(), WaferError> {
    let now = chrono::Utc::now();
    let dead_letter = attempts >= MAX_ATTEMPTS;
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
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!("processing"),
            },
            Filter {
                field: "processing_owner".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(owner),
            },
        ],
        HashMap::from([
            (
                "status".to_string(),
                serde_json::json!(if dead_letter { "dead_letter" } else { "failed" }),
            ),
            ("processing_owner".to_string(), serde_json::json!("")),
            ("processing_started_at".to_string(), serde_json::Value::Null),
            (
                "next_attempt_at".to_string(),
                if dead_letter {
                    serde_json::Value::Null
                } else {
                    serde_json::json!((now
                        + chrono::Duration::seconds(retry_delay_seconds(attempts)))
                    .to_rfc3339())
                },
            ),
            (
                "last_error".to_string(),
                serde_json::json!(message.chars().take(1000).collect::<String>()),
            ),
            (
                "terminal_at".to_string(),
                if dead_letter {
                    serde_json::json!(now.to_rfc3339())
                } else {
                    serde_json::Value::Null
                },
            ),
            (
                "updated_at".to_string(),
                serde_json::json!(now.to_rfc3339()),
            ),
        ]),
    )
    .await?;
    if rows == 1 {
        Ok(())
    } else {
        Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "provider-operation lease was lost before retry was recorded",
        ))
    }
}

pub(crate) async fn resolve_unleased(
    ctx: &dyn Context,
    id: &str,
    succeeded: bool,
    response_json: &str,
    message: &str,
) -> Result<(), WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    db::update(
        ctx,
        TABLE,
        id,
        HashMap::from([
            (
                "status".to_string(),
                serde_json::json!(if succeeded {
                    "succeeded"
                } else {
                    "dead_letter"
                }),
            ),
            (
                "response_json".to_string(),
                serde_json::json!(response_json),
            ),
            (
                "last_error".to_string(),
                serde_json::json!(message.chars().take(1000).collect::<String>()),
            ),
            ("processing_owner".to_string(), serde_json::json!("")),
            ("processing_started_at".to_string(), serde_json::Value::Null),
            ("next_attempt_at".to_string(), serde_json::Value::Null),
            (
                "completed_at".to_string(),
                if succeeded {
                    serde_json::json!(&now)
                } else {
                    serde_json::Value::Null
                },
            ),
            ("terminal_at".to_string(), serde_json::json!(&now)),
            ("updated_at".to_string(), serde_json::json!(&now)),
        ]),
    )
    .await?;
    Ok(())
}

pub(crate) async fn resolve_for_aggregate(
    ctx: &dyn Context,
    operation_type: &str,
    aggregate_id: &str,
    succeeded: bool,
    response_json: &str,
    message: &str,
) -> Result<(), WaferError> {
    let operations = db::list(
        ctx,
        TABLE,
        &ListOptions {
            filters: vec![
                Filter {
                    field: "operation_type".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(operation_type),
                },
                Filter {
                    field: "aggregate_id".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(aggregate_id),
                },
            ],
            limit: 10,
            skip_count: true,
            ..Default::default()
        },
    )
    .await?
    .records;
    for operation in operations {
        let resolved = if succeeded {
            "succeeded"
        } else {
            "dead_letter"
        };
        if operation.str_field("status") != resolved {
            resolve_unleased(ctx, &operation.id, succeeded, response_json, message).await?;
        }
    }
    Ok(())
}

pub(crate) async fn complete_for_aggregate(
    ctx: &dyn Context,
    operation_type: &str,
    aggregate_id: &str,
    response_json: &str,
) -> Result<(), WaferError> {
    resolve_for_aggregate(ctx, operation_type, aggregate_id, true, response_json, "").await
}
