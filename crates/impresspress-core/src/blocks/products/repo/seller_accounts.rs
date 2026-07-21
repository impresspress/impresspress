//! User-owned Stripe Connect account and capability state.

use std::collections::HashMap;

use serde_json::Value;
use wafer_block::{
    db::{Filter, FilterOp},
    wire::database::OnConflict,
};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    blocks::products::contracts::{ApprovalStatus, SellerAccount, SellerCapabilities},
    util::RecordExt,
};

pub(crate) const TABLE: &str = "impresspress__products__seller_accounts";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadySellerAccount {
    pub id: String,
    pub stripe_account_id: String,
    pub fee_basis_points: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StripeSellerSnapshot {
    pub stripe_account_id: String,
    pub livemode: bool,
    pub details_submitted: bool,
    pub charges_enabled: bool,
    pub payouts_enabled: bool,
    pub requirements: Value,
    pub country: String,
    pub default_currency: String,
    pub dashboard_type: String,
    pub disabled_reason: String,
}

fn requirements_value(record: &db::Record) -> Value {
    match record.data.get("requirements_json") {
        Some(Value::String(raw)) => {
            serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({}))
        }
        Some(value) => value.clone(),
        None => serde_json::json!({}),
    }
}

fn due_requirements(value: &Value) -> Vec<String> {
    value
        .get("currently_due")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

pub(crate) fn to_contract(record: &db::Record) -> Result<SellerAccount, WaferError> {
    let fee_basis_points = u32::try_from(record.i64_field("fee_basis_points"))
        .ok()
        .filter(|value| *value <= 10_000)
        .ok_or_else(|| {
            WaferError::new(
                ErrorCode::Internal,
                "seller account has an invalid application fee",
            )
        })?;
    let requirements = requirements_value(record);
    let suspended = record.str_field("status") == "suspended";
    Ok(SellerAccount {
        id: record.id.clone(),
        user_id: record.str_field("user_id").to_string(),
        status: record.str_field("status").to_string(),
        approval_status: if suspended {
            ApprovalStatus::Suspended
        } else {
            ApprovalStatus::Approved
        },
        stripe_account_id: record.str_field("stripe_account_id").to_string(),
        capabilities: SellerCapabilities {
            details_submitted: record.bool_field("details_submitted"),
            charges_enabled: record.bool_field("charges_enabled"),
            payouts_enabled: record.bool_field("payouts_enabled"),
            requirements_due: due_requirements(&requirements),
        },
        fee_basis_points,
        livemode: record.bool_field("livemode"),
        country: record.str_field("country").to_string(),
        default_currency: record.str_field("default_currency").to_string(),
        dashboard_type: record.str_field("dashboard_type").to_string(),
        disabled_reason: record.str_field("requirements_disabled_reason").to_string(),
        sync_error: record.str_field("sync_error").to_string(),
        last_synced_at: record.str_field("last_synced_at").to_string(),
    })
}

pub(crate) async fn get_for_user(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<Option<db::Record>, WaferError> {
    match db::get_by_field(ctx, TABLE, "user_id", Value::String(user_id.to_string())).await {
        Ok(record) => Ok(Some(record)),
        Err(error) if error.code == ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) async fn is_suspended(ctx: &dyn Context, user_id: &str) -> Result<bool, WaferError> {
    Ok(get_for_user(ctx, user_id)
        .await?
        .is_some_and(|record| record.str_field("status") == "suspended"))
}

pub(crate) async fn get_by_stripe_account(
    ctx: &dyn Context,
    stripe_account_id: &str,
) -> Result<Option<db::Record>, WaferError> {
    match db::get_by_field(
        ctx,
        TABLE,
        "stripe_account_id",
        Value::String(stripe_account_id.to_string()),
    )
    .await
    {
        Ok(record) => Ok(Some(record)),
        Err(error) if error.code == ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) async fn set_admin_suspended(
    ctx: &dyn Context,
    local_id: &str,
    suspended: bool,
) -> Result<db::Record, WaferError> {
    let current = db::get(ctx, TABLE, local_id).await?;
    let now = chrono::Utc::now().to_rfc3339();
    let status = if suspended {
        "suspended"
    } else if current.bool_field("charges_enabled") {
        "active"
    } else if current.bool_field("details_submitted") {
        "restricted"
    } else {
        "onboarding"
    };
    db::update(
        ctx,
        TABLE,
        local_id,
        HashMap::from([
            ("status".to_string(), serde_json::json!(status)),
            (
                "suspended_at".to_string(),
                if suspended {
                    serde_json::json!(&now)
                } else {
                    serde_json::Value::Null
                },
            ),
            ("updated_at".to_string(), serde_json::json!(now)),
        ]),
    )
    .await
}

pub(crate) async fn ensure_for_user(
    ctx: &dyn Context,
    user_id: &str,
    fee_basis_points: u16,
) -> Result<db::Record, WaferError> {
    let digest = wafer_block::hash::sha256_hex(user_id.as_bytes());
    let id = format!("seller_{}", &digest[..32]);
    let now = chrono::Utc::now().to_rfc3339();
    db::upsert(
        ctx,
        TABLE,
        vec![
            ("id".to_string(), serde_json::json!(&id)),
            ("user_id".to_string(), serde_json::json!(user_id)),
            ("status".to_string(), serde_json::json!("not_started")),
            (
                "fee_basis_points".to_string(),
                serde_json::json!(fee_basis_points),
            ),
            ("created_at".to_string(), serde_json::json!(&now)),
            ("updated_at".to_string(), serde_json::json!(&now)),
        ],
        vec!["id".to_string()],
        OnConflict::SetColumns(vec![]),
    )
    .await?;
    db::get(ctx, TABLE, &id).await
}

pub(crate) async fn sync_account(
    ctx: &dyn Context,
    local_id: &str,
    snapshot: &StripeSellerSnapshot,
) -> Result<db::Record, WaferError> {
    let current = db::get(ctx, TABLE, local_id).await?;
    let status = if current.str_field("status") == "suspended" {
        "suspended"
    } else if snapshot.charges_enabled {
        "active"
    } else if snapshot.details_submitted {
        "restricted"
    } else {
        "onboarding"
    };
    let now = chrono::Utc::now().to_rfc3339();
    db::update(
        ctx,
        TABLE,
        local_id,
        HashMap::from([
            ("status".to_string(), serde_json::json!(status)),
            (
                "stripe_account_id".to_string(),
                serde_json::json!(&snapshot.stripe_account_id),
            ),
            ("livemode".to_string(), serde_json::json!(snapshot.livemode)),
            (
                "details_submitted".to_string(),
                serde_json::json!(snapshot.details_submitted),
            ),
            (
                "charges_enabled".to_string(),
                serde_json::json!(snapshot.charges_enabled),
            ),
            (
                "payouts_enabled".to_string(),
                serde_json::json!(snapshot.payouts_enabled),
            ),
            (
                "requirements_json".to_string(),
                serde_json::json!(serde_json::to_string(&snapshot.requirements).map_err(
                    |error| WaferError::new(
                        ErrorCode::Internal,
                        format!("could not encode seller requirements: {error}")
                    )
                )?),
            ),
            ("country".to_string(), serde_json::json!(&snapshot.country)),
            (
                "default_currency".to_string(),
                serde_json::json!(&snapshot.default_currency),
            ),
            (
                "dashboard_type".to_string(),
                serde_json::json!(&snapshot.dashboard_type),
            ),
            (
                "requirements_disabled_reason".to_string(),
                serde_json::json!(&snapshot.disabled_reason),
            ),
            ("sync_error".to_string(), serde_json::json!("")),
            ("last_synced_at".to_string(), serde_json::json!(&now)),
            ("updated_at".to_string(), serde_json::json!(&now)),
        ]),
    )
    .await
}

/// Apply a connected-account webhook snapshot in Stripe event-time order.
/// For events created in the same second, capability booleans merge toward
/// the more restrictive value. That can temporarily require a provider
/// refresh to re-enable sales, but can never temporarily authorize charges
/// from an ambiguously ordered delivery.
pub(crate) async fn sync_account_event(
    ctx: &dyn Context,
    local_id: &str,
    snapshot: &StripeSellerSnapshot,
    event_created: i64,
) -> Result<db::Record, WaferError> {
    if event_created < 0 {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            "connected-account event timestamp must not be negative",
        ));
    }
    for _ in 0..3 {
        let current = db::get(ctx, TABLE, local_id).await?;
        if !current.str_field("stripe_account_id").is_empty()
            && current.str_field("stripe_account_id") != snapshot.stripe_account_id
        {
            return Err(WaferError::new(
                ErrorCode::FailedPrecondition,
                "connected-account identity changed during synchronization",
            ));
        }
        if current.i64_field("stripe_event_created") > event_created {
            return Ok(current);
        }
        let has_authoritative_snapshot = current.i64_field("stripe_event_created") > 0
            || !current.str_field("last_synced_at").is_empty();
        if has_authoritative_snapshot && current.bool_field("livemode") != snapshot.livemode {
            return Err(WaferError::new(
                ErrorCode::FailedPrecondition,
                "connected-account mode does not match its local account",
            ));
        }

        let same_second =
            event_created > 0 && current.i64_field("stripe_event_created") == event_created;
        let details_submitted = if same_second {
            current.bool_field("details_submitted") && snapshot.details_submitted
        } else {
            snapshot.details_submitted
        };
        let charges_enabled = if same_second {
            current.bool_field("charges_enabled") && snapshot.charges_enabled
        } else {
            snapshot.charges_enabled
        };
        let payouts_enabled = if same_second {
            current.bool_field("payouts_enabled") && snapshot.payouts_enabled
        } else {
            snapshot.payouts_enabled
        };
        let incoming_restricts = (current.bool_field("details_submitted") && !details_submitted)
            || (current.bool_field("charges_enabled") && !charges_enabled)
            || (current.bool_field("payouts_enabled") && !payouts_enabled)
            || (current.str_field("requirements_disabled_reason").is_empty()
                && !snapshot.disabled_reason.is_empty());
        let requirements_json = if same_second && !incoming_restricts {
            match current.data.get("requirements_json") {
                Some(Value::String(raw)) => raw.clone(),
                Some(value) => serde_json::to_string(value).map_err(|error| {
                    WaferError::new(
                        ErrorCode::Internal,
                        format!("could not encode seller requirements: {error}"),
                    )
                })?,
                None => "{}".to_string(),
            }
        } else {
            serde_json::to_string(&snapshot.requirements).map_err(|error| {
                WaferError::new(
                    ErrorCode::Internal,
                    format!("could not encode seller requirements: {error}"),
                )
            })?
        };
        let disabled_reason = if same_second && !incoming_restricts {
            current.str_field("requirements_disabled_reason")
        } else {
            &snapshot.disabled_reason
        };
        let status = if current.str_field("status") == "suspended" {
            "suspended"
        } else if charges_enabled {
            "active"
        } else if details_submitted {
            "restricted"
        } else {
            "onboarding"
        };
        let now = chrono::Utc::now().to_rfc3339();
        let rows = db::update_by_filters_count(
            ctx,
            TABLE,
            vec![
                Filter {
                    field: "id".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(local_id),
                },
                Filter {
                    field: "stripe_event_created".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(current.i64_field("stripe_event_created")),
                },
                Filter {
                    field: "status".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(current.str_field("status")),
                },
                Filter {
                    field: "charges_enabled".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(current.bool_field("charges_enabled")),
                },
                Filter {
                    field: "payouts_enabled".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(current.bool_field("payouts_enabled")),
                },
                Filter {
                    field: "details_submitted".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::json!(current.bool_field("details_submitted")),
                },
            ],
            HashMap::from([
                ("status".to_string(), serde_json::json!(status)),
                (
                    "stripe_account_id".to_string(),
                    serde_json::json!(&snapshot.stripe_account_id),
                ),
                ("livemode".to_string(), serde_json::json!(snapshot.livemode)),
                (
                    "details_submitted".to_string(),
                    serde_json::json!(details_submitted),
                ),
                (
                    "charges_enabled".to_string(),
                    serde_json::json!(charges_enabled),
                ),
                (
                    "payouts_enabled".to_string(),
                    serde_json::json!(payouts_enabled),
                ),
                (
                    "requirements_json".to_string(),
                    serde_json::json!(requirements_json),
                ),
                ("country".to_string(), serde_json::json!(&snapshot.country)),
                (
                    "default_currency".to_string(),
                    serde_json::json!(&snapshot.default_currency),
                ),
                (
                    "dashboard_type".to_string(),
                    serde_json::json!(&snapshot.dashboard_type),
                ),
                (
                    "requirements_disabled_reason".to_string(),
                    serde_json::json!(disabled_reason),
                ),
                ("sync_error".to_string(), serde_json::json!("")),
                ("last_synced_at".to_string(), serde_json::json!(&now)),
                (
                    "stripe_event_created".to_string(),
                    serde_json::json!(event_created),
                ),
                ("updated_at".to_string(), serde_json::json!(&now)),
            ]),
        )
        .await?;
        if rows == 1 {
            return db::get(ctx, TABLE, local_id).await;
        }
    }
    Err(WaferError::new(
        ErrorCode::FailedPrecondition,
        "connected-account state changed concurrently; retry the event",
    ))
}

pub(crate) async fn mark_sync_error(
    ctx: &dyn Context,
    local_id: &str,
    message: &str,
) -> Result<db::Record, WaferError> {
    db::update(
        ctx,
        TABLE,
        local_id,
        HashMap::from([
            ("sync_error".to_string(), serde_json::json!(message)),
            (
                "updated_at".to_string(),
                serde_json::json!(chrono::Utc::now().to_rfc3339()),
            ),
        ]),
    )
    .await
}

/// Resolve the connected account used for direct charges. Capability state
/// is checked at checkout time, so disabling charges in Stripe fails closed.
pub(crate) async fn ready_for_user(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<ReadySellerAccount, WaferError> {
    let record = get_for_user(ctx, user_id).await?.ok_or_else(|| {
        WaferError::new(
            ErrorCode::FailedPrecondition,
            "seller Stripe account is not ready to accept charges",
        )
    })?;
    let stripe_account_id = record.str_field("stripe_account_id").to_string();
    if record.str_field("status") != "active"
        || !record.bool_field("charges_enabled")
        || stripe_account_id.is_empty()
    {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "seller Stripe account is not ready to accept charges",
        ));
    }
    let fee_basis_points = u16::try_from(record.i64_field("fee_basis_points"))
        .ok()
        .filter(|value| *value <= 10_000)
        .ok_or_else(|| {
            WaferError::new(
                ErrorCode::Internal,
                "seller account has an invalid application fee",
            )
        })?;
    Ok(ReadySellerAccount {
        id: record.id,
        stripe_account_id,
        fee_basis_points,
    })
}
