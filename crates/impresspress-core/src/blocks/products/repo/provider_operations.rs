//! Durable idempotent Stripe mutation and reconciliation operations.

use std::collections::HashMap;

use wafer_block::{
    db::{Filter, FilterOp, ListOptions, SortField},
    wire::database::OnConflict,
};
use wafer_core::clients::database::{self as db, Record, RecordList};
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{retry_delay_seconds, MAX_ATTEMPTS};
use crate::{
    blocks::products::contracts::OperationStatus,
    util::{enum_column, wire_str, RecordExt},
};

pub(crate) const TABLE: &str = "impresspress__products__provider_operations";
pub(crate) const REFUND_RECONCILE: &str = "refund.reconcile";
/// Take a Stripe Payment Link down for a local row that no longer sells it.
/// Its aggregate is the Payment Link row id, which is also the handle the
/// link carries in its Stripe metadata.
pub(crate) const PAYMENT_LINK_DEACTIVATE: &str = "payment_link.deactivate";
const LEASE_SECONDS: i64 = 300;

/// The `status` column of an operation row, as the enum that defines it.
fn status_of(record: &Record) -> Result<OperationStatus, WaferError> {
    enum_column(record, "status")
}

pub(crate) struct OperationClaim {
    pub record: Record,
    pub owner: String,
    pub attempts: u64,
}

/// What one [`claim_due`] pass did with the due rows it looked at.
pub(crate) struct ClaimBatch {
    /// Rows this pass now holds the lease on.
    pub claims: Vec<OperationClaim>,
    /// Rows found out of attempts and moved to `dead_letter` instead.
    pub dead_lettered: u64,
    /// Rows whose claim or dead-letter write failed, by id. They keep the
    /// state they had and are looked at again by the next pass.
    pub failures: Vec<(String, WaferError)>,
}

/// Outcome of [`claim_one`] for a single candidate row.
enum Candidate {
    Claimed(OperationClaim),
    DeadLettered,
    /// Not due, or another worker changed the row first.
    Skipped,
}

/// Whether [`mark_retry`] scheduled another attempt or spent the last one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetryRecorded {
    Scheduled,
    DeadLettered,
}

fn timestamp(record: &Record, field: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(record.str_field(field))
        .ok()
        .map(|value| value.with_timezone(&chrono::Utc))
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
            (
                "status".to_string(),
                serde_json::json!(OperationStatus::Pending),
            ),
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
    status: Option<OperationStatus>,
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

/// Move a row that is out of attempts to `dead_letter`, recording why. `true`
/// when this call moved it; `false` when another worker changed it first.
async fn dead_letter_unclaimed(
    ctx: &dyn Context,
    record: &Record,
    status: OperationStatus,
) -> Result<bool, WaferError> {
    let now = chrono::Utc::now().to_rfc3339();
    let attempts = record.u64_field("attempts");
    let mut reason = match status {
        OperationStatus::Processing => format!(
            "retry budget of {MAX_ATTEMPTS} attempts exhausted: the lease of attempt {attempts} \
             expired without recording an outcome"
        ),
        _ => format!("retry budget of {MAX_ATTEMPTS} attempts exhausted after {attempts} attempts"),
    };
    let previous = record.str_field("last_error");
    if !previous.is_empty() {
        reason.push_str("; last recorded error: ");
        reason.push_str(previous);
    }
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
                value: serde_json::json!(wire_str(&status)),
            },
            Filter {
                field: "processing_owner".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(record.str_field("processing_owner")),
            },
        ],
        HashMap::from([
            (
                "status".to_string(),
                serde_json::json!(OperationStatus::DeadLetter),
            ),
            ("processing_owner".to_string(), serde_json::json!("")),
            ("processing_started_at".to_string(), serde_json::Value::Null),
            ("next_attempt_at".to_string(), serde_json::Value::Null),
            (
                "last_error".to_string(),
                serde_json::json!(reason.chars().take(1000).collect::<String>()),
            ),
            ("terminal_at".to_string(), serde_json::json!(&now)),
            ("updated_at".to_string(), serde_json::json!(&now)),
        ]),
    )
    .await?;
    Ok(rows == 1)
}

/// Lease up to `limit` due rows. One row's failed write does not stop the
/// pass: it is reported in [`ClaimBatch::failures`] and the rest are still
/// claimed, so a single bad row cannot starve the queue behind it.
pub(crate) async fn claim_due(ctx: &dyn Context, limit: usize) -> Result<ClaimBatch, WaferError> {
    let now_value = chrono::Utc::now();
    let candidates = db::list(
        ctx,
        TABLE,
        &ListOptions {
            filters: vec![Filter {
                field: "status".to_string(),
                operator: FilterOp::In,
                value: serde_json::json!([
                    OperationStatus::Pending,
                    OperationStatus::Failed,
                    OperationStatus::Processing,
                ]),
            }],
            sort: vec![SortField {
                field: "created_at".to_string(),
                desc: false,
            }],
            limit: Some((limit.clamp(1, 100) * 4) as u32),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?
    .records;
    let mut batch = ClaimBatch {
        claims: Vec::new(),
        dead_lettered: 0,
        failures: Vec::new(),
    };
    for record in candidates {
        if batch.claims.len() >= limit {
            break;
        }
        let id = record.id.clone();
        match claim_one(ctx, record, now_value).await {
            Ok(Candidate::Claimed(claim)) => batch.claims.push(claim),
            Ok(Candidate::DeadLettered) => batch.dead_lettered += 1,
            Ok(Candidate::Skipped) => {}
            Err(error) => batch.failures.push((id, error)),
        }
    }
    Ok(batch)
}

async fn claim_one(
    ctx: &dyn Context,
    record: Record,
    now_value: chrono::DateTime<chrono::Utc>,
) -> Result<Candidate, WaferError> {
    let status = status_of(&record)?;
    let eligible = match status {
        OperationStatus::Pending | OperationStatus::Failed => {
            timestamp(&record, "next_attempt_at").is_none_or(|next| next <= now_value)
        }
        OperationStatus::Processing => {
            timestamp(&record, "processing_started_at").is_none_or(|started| {
                now_value.signed_duration_since(started).num_seconds() >= LEASE_SECONDS
            })
        }
        // Terminal, and the query above does not select them anyway.
        OperationStatus::Succeeded | OperationStatus::DeadLetter => false,
    };
    if !eligible {
        return Ok(Candidate::Skipped);
    }
    let attempts = record.u64_field("attempts").saturating_add(1);
    if attempts > MAX_ATTEMPTS {
        return Ok(if dead_letter_unclaimed(ctx, &record, status).await? {
            Candidate::DeadLettered
        } else {
            Candidate::Skipped
        });
    }
    let owner = uuid::Uuid::now_v7().to_string();
    let now = now_value.to_rfc3339();
    let claimed_fields = HashMap::from([
        (
            "status".to_string(),
            serde_json::json!(OperationStatus::Processing),
        ),
        ("attempts".to_string(), serde_json::json!(attempts)),
        ("processing_owner".to_string(), serde_json::json!(&owner)),
        ("processing_started_at".to_string(), serde_json::json!(&now)),
        ("next_attempt_at".to_string(), serde_json::Value::Null),
        ("updated_at".to_string(), serde_json::json!(&now)),
    ]);
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
                value: serde_json::json!(wire_str(&status)),
            },
            Filter {
                field: "processing_owner".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(record.str_field("processing_owner")),
            },
        ],
        claimed_fields.clone(),
    )
    .await?;
    if rows != 1 {
        return Ok(Candidate::Skipped);
    }
    // The CAS matched the row exactly as read, so the row now IS the read
    // plus the fields just written. Building it here rather than reading it
    // back means nothing can fail between taking the lease and handing it to
    // the caller, which would strand the row leased with an attempt spent.
    let mut record = record;
    record.data.extend(claimed_fields);
    Ok(Candidate::Claimed(OperationClaim {
        record,
        owner,
        attempts,
    }))
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
                value: serde_json::json!(OperationStatus::Processing),
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
                serde_json::json!(OperationStatus::Succeeded),
            ),
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
    if rows == 1 || status_of(&db::get(ctx, TABLE, id).await?)? == OperationStatus::Succeeded {
        Ok(())
    } else {
        Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "provider-operation lease was lost before completion",
        ))
    }
}

/// Record a failed attempt under the caller's lease: another attempt is
/// scheduled with backoff, or the row dead-letters when `attempts` was the
/// last of [`MAX_ATTEMPTS`].
pub(crate) async fn mark_retry(
    ctx: &dyn Context,
    id: &str,
    owner: &str,
    attempts: u64,
    message: &str,
) -> Result<RetryRecorded, WaferError> {
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
                value: serde_json::json!(OperationStatus::Processing),
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
                serde_json::json!(if dead_letter {
                    OperationStatus::DeadLetter
                } else {
                    OperationStatus::Failed
                }),
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
    if rows != 1 {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "provider-operation lease was lost before retry was recorded",
        ));
    }
    Ok(if dead_letter {
        RetryRecorded::DeadLettered
    } else {
        RetryRecorded::Scheduled
    })
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
                    OperationStatus::Succeeded
                } else {
                    OperationStatus::DeadLetter
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
            limit: Some(10),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?
    .records;
    for operation in operations {
        let resolved = if succeeded {
            OperationStatus::Succeeded
        } else {
            OperationStatus::DeadLetter
        };
        if status_of(&operation)? != resolved {
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
