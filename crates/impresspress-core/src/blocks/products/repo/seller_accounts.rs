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
    blocks::products::contracts::{
        SellerAccount, SellerApproval, SellerCapabilities, SellerStatus,
    },
    db_read::{self, CappedList},
    util::{enum_column, wire_str, RecordExt},
};

pub(crate) const TABLE: &str = "impresspress__products__seller_accounts";

/// The seller-status ladder, computed once.
///
/// `set_admin_suspended`, `sync_account` and `sync_account_event` each held
/// a verbatim copy of this `if` chain, reading the same three facts from
/// three different places (an argument, the stored row, a merged snapshot).
/// Three copies of a five-value ladder is three chances to add a state to
/// one of them; the callers now differ only in where the three booleans
/// come from.
///
/// `suspended` outranks everything: an administrator's suspension survives
/// any capability change Stripe reports. Below it, charges enabled means
/// the seller can be paid; details submitted without charges means Stripe
/// is still deciding; neither means onboarding has not finished.
/// [`SellerStatus::NotStarted`] is deliberately unreachable from here — it
/// is the value `ensure_for_user` inserts before any Connect account
/// exists, and no snapshot can put a row back into it.
pub(crate) const fn ladder(
    suspended: bool,
    charges_enabled: bool,
    details_submitted: bool,
) -> SellerStatus {
    if suspended {
        SellerStatus::Suspended
    } else if charges_enabled {
        SellerStatus::Active
    } else if details_submitted {
        SellerStatus::Restricted
    } else {
        SellerStatus::Onboarding
    }
}

/// The published approval state for a stored [`SellerStatus`].
///
/// The whole of `SellerAccount.approval_status`: a suspended account is
/// suspended, every other state is approved. It was written inline as a
/// ternary over a string comparison and typed as the five-variant
/// `ApprovalStatus`, which describes the *product* moderation column;
/// three of those five could never be produced here.
pub(crate) const fn approval_from_status(status: SellerStatus) -> SellerApproval {
    match status {
        SellerStatus::Suspended => SellerApproval::Suspended,
        SellerStatus::NotStarted
        | SellerStatus::Onboarding
        | SellerStatus::Restricted
        | SellerStatus::Active => SellerApproval::Approved,
    }
}

/// The `status` column of a seller row, as the enum that defines it.
fn status_of(record: &db::Record) -> Result<SellerStatus, WaferError> {
    enum_column(record, "status")
}

/// Whether the stored row is suspended.
///
/// The ladder's first argument, and the one question three call sites
/// outside this module ask of a seller row they already hold.
pub(crate) fn is_suspended_record(record: &db::Record) -> Result<bool, WaferError> {
    Ok(status_of(record)? == SellerStatus::Suspended)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadySellerAccount {
    pub id: String,
    pub stripe_account_id: String,
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

/// Everything a stored seller row says about the account, decoded.
///
/// [`SellerAccount`] minus its `fee_basis_points`: the fee is a platform
/// setting ([`crate::blocks::products::config::seller_fee_bps`]), not a fact
/// about the row, so a surface that must render even when that setting cannot
/// be read — the admin seller pages, where the suspend control lives — works
/// from this and never has to invent a number. [`SellerRow::into_contract`]
/// adds the fee for everything that publishes the contract.
///
/// The table's `fee_basis_points` column is neither read nor written: no path
/// ever changed it after a row's first insert, so what it holds is not what
/// anyone is charged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SellerRow {
    pub id: String,
    pub user_id: String,
    pub status: SellerStatus,
    pub approval_status: SellerApproval,
    pub stripe_account_id: String,
    pub capabilities: SellerCapabilities,
    pub livemode: bool,
    pub country: String,
    pub default_currency: String,
    pub dashboard_type: String,
    pub disabled_reason: String,
    pub sync_error: String,
    pub last_synced_at: String,
}

impl SellerRow {
    /// Decode a stored row. Fails only on a `status` outside the contract.
    pub(crate) fn from_record(record: &db::Record) -> Result<Self, WaferError> {
        let requirements = requirements_value(record);
        let status = status_of(record)?;
        Ok(Self {
            id: record.id.clone(),
            user_id: record.str_field("user_id").to_string(),
            status,
            approval_status: approval_from_status(status),
            stripe_account_id: record.str_field("stripe_account_id").to_string(),
            capabilities: SellerCapabilities {
                details_submitted: record.bool_field("details_submitted"),
                charges_enabled: record.bool_field("charges_enabled"),
                payouts_enabled: record.bool_field("payouts_enabled"),
                requirements_due: due_requirements(&requirements),
            },
            livemode: record.bool_field("livemode"),
            country: record.str_field("country").to_string(),
            default_currency: record.str_field("default_currency").to_string(),
            dashboard_type: record.str_field("dashboard_type").to_string(),
            disabled_reason: record.str_field("requirements_disabled_reason").to_string(),
            sync_error: record.str_field("sync_error").to_string(),
            last_synced_at: record.str_field("last_synced_at").to_string(),
        })
    }

    /// The published [`SellerAccount`], carrying `fee_basis_points` as the
    /// caller read it from the platform setting.
    pub(crate) fn into_contract(self, fee_basis_points: u16) -> SellerAccount {
        SellerAccount {
            id: self.id,
            user_id: self.user_id,
            status: self.status,
            approval_status: self.approval_status,
            stripe_account_id: self.stripe_account_id,
            capabilities: self.capabilities,
            fee_basis_points: fee_basis_points.into(),
            livemode: self.livemode,
            country: self.country,
            default_currency: self.default_currency,
            dashboard_type: self.dashboard_type,
            disabled_reason: self.disabled_reason,
            sync_error: self.sync_error,
            last_synced_at: self.last_synced_at,
        }
    }
}

/// The stored row as the published [`SellerAccount`]: [`SellerRow`] plus
/// the platform fee the caller read.
pub(crate) fn to_contract(
    record: &db::Record,
    fee_basis_points: u16,
) -> Result<SellerAccount, WaferError> {
    Ok(SellerRow::from_record(record)?.into_contract(fee_basis_points))
}

/// Seller accounts as decoded [`SellerRow`]s, and whether there are more
/// than the read returned.
///
/// The admin seller list (`GET /b/products/api/admin/sellers`) and the admin
/// sellers page rendered this from two verbatim copies of the same read plus
/// the projection; a decode failure on any row is an error for the whole read,
/// because a seller list missing the row that could not be decoded is a
/// governance surface that silently hides an account.
///
/// The table holds one row per selling user, so it grows with the platform.
/// That is why the read is capped rather than unpaged-and-hopeful: the same
/// argument that makes an undecodable row fatal makes a silently dropped tail
/// unacceptable, so the caller is handed the fact that it has a prefix.
pub(crate) async fn list_rows(ctx: &dyn Context) -> Result<CappedList<SellerRow>, WaferError> {
    db_read::list_capped(ctx, TABLE, vec![])
        .await?
        .try_map(|record| SellerRow::from_record(&record))
}

/// How many seller accounts exist, for the listing that shows a prefix.
pub(crate) async fn count_all(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &[]).await
}

/// One seller account by its local id, as the stored row. `Ok(None)` when
/// there is no such row.
///
/// Kept alongside [`get_row`] for the one caller — suspension — that
/// must be able to act on a row whose contract projection would fail: an
/// account whose stored `status` no longer decodes is exactly the one an
/// operator most needs to be able to suspend.
pub(crate) async fn get(ctx: &dyn Context, id: &str) -> Result<Option<db::Record>, WaferError> {
    match db::get(ctx, TABLE, id).await {
        Ok(record) => Ok(Some(record)),
        Err(error) if error.code == ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// One seller account by its local id, decoded. `Ok(None)` when there is no
/// such row, so a caller can answer 404 without matching on an error code.
pub(crate) async fn get_row(ctx: &dyn Context, id: &str) -> Result<Option<SellerRow>, WaferError> {
    match db::get(ctx, TABLE, id).await {
        Ok(record) => SellerRow::from_record(&record).map(Some),
        Err(error) if error.code == ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error),
    }
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
    match get_for_user(ctx, user_id).await? {
        Some(record) => is_suspended_record(&record),
        None => Ok(false),
    }
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
    let status = ladder(
        suspended,
        current.bool_field("charges_enabled"),
        current.bool_field("details_submitted"),
    );
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
            (
                "status".to_string(),
                serde_json::json!(SellerStatus::NotStarted),
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
    let status = ladder(
        is_suspended_record(&current)?,
        snapshot.charges_enabled,
        snapshot.details_submitted,
    );
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
        let status = ladder(
            is_suspended_record(&current)?,
            charges_enabled,
            details_submitted,
        );
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
                    value: serde_json::json!(wire_str(&status_of(&current)?)),
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
///
/// `FailedPrecondition` is the one answer about the seller — no account, or
/// one that cannot take charges yet. Any other error is a fault reading it.
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
    if status_of(&record)? != SellerStatus::Active
        || !record.bool_field("charges_enabled")
        || stripe_account_id.is_empty()
    {
        return Err(WaferError::new(
            ErrorCode::FailedPrecondition,
            "seller Stripe account is not ready to accept charges",
        ));
    }
    Ok(ReadySellerAccount {
        id: record.id,
        stripe_account_id,
    })
}
