//! Row-level access over `impresspress__files__cloud_shares` and its child
//! audit table `impresspress__files__cloud_access_logs`.
//!
//! A share row is one generated public link (token, source object,
//! optional expiry / access cap, running `access_count`). Every recorded
//! access appends an access-log row ([`log_access`]). Both tables are
//! owned here because the log rows are meaningless without their share.

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record, RecordList};
use wafer_run::{context::Context, WaferError};

/// Public share-link table — one row per generated token.
pub const TABLE: &str = "impresspress__files__cloud_shares";

/// Access log table — one row per recorded share access (audit trail).
pub const ACCESS_LOGS_TABLE: &str = "impresspress__files__cloud_access_logs";

/// Insert payload for [`insert`]. Borrowed fields — the caller keeps
/// ownership. `created_at` is caller-supplied (not stamped here) because
/// the share's `expires_at` is derived from the same instant.
#[derive(Debug, Clone, Copy)]
pub struct NewShare<'a> {
    pub token: &'a str,
    pub bucket: &'a str,
    pub key: &'a str,
    pub created_by: &'a str,
    /// RFC 3339 creation instant (also the base of `expires_at`).
    pub created_at: &'a str,
    /// Optional absolute expiry (RFC 3339).
    pub expires_at: Option<&'a str>,
    /// Optional access cap; `None` (or a non-positive stored value) means
    /// unlimited.
    pub max_access_count: Option<i64>,
}

/// Insert a share row (`access_count` starts at 0) and return it.
pub async fn insert(ctx: &dyn Context, new: NewShare<'_>) -> Result<Record, WaferError> {
    let mut data = crate::util::json_map(serde_json::json!({
        "token": new.token,
        "bucket": new.bucket,
        "key": new.key,
        "created_by": new.created_by,
        "created_at": new.created_at,
        "access_count": 0,
    }));
    if let Some(exp) = new.expires_at {
        data.insert(
            "expires_at".to_string(),
            serde_json::Value::String(exp.to_string()),
        );
    }
    if let Some(max) = new.max_access_count {
        data.insert("max_access_count".to_string(), serde_json::json!(max));
    }
    db::create(ctx, TABLE, data).await
}

/// Look up a share by its raw token (the value embedded in the public
/// `/b/storage/direct/{token}` URL).
pub async fn find_by_token(ctx: &dyn Context, token: &str) -> Result<Record, WaferError> {
    db::get_by_field(
        ctx,
        TABLE,
        "token",
        serde_json::Value::String(token.to_string()),
    )
    .await
}

/// Look up a share by its primary `id`.
pub async fn find_by_id(ctx: &dyn Context, id: &str) -> Result<Record, WaferError> {
    db::get(ctx, TABLE, id).await
}

/// Hard-delete a share row by id.
pub async fn delete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, TABLE, id).await
}

/// Up to `limit` of `user_id`'s shares, newest first (JSON API listing).
pub async fn list_for_user(
    ctx: &dyn Context,
    user_id: &str,
    limit: i64,
) -> Result<RecordList, WaferError> {
    let opts = ListOptions {
        filters: vec![Filter {
            field: "created_by".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id.to_string()),
        }],
        sort: vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }],
        limit,
        ..Default::default()
    };
    db::list(ctx, TABLE, &opts).await
}

/// ALL of `user_id`'s shares, newest first, unpaginated (the SSR shares
/// page).
pub async fn list_all_for_user(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<Vec<Record>, WaferError> {
    db::list_sorted(
        ctx,
        TABLE,
        vec![Filter {
            field: "created_by".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id.to_string()),
        }],
        vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }],
    )
    .await
}

/// Newest shares across ALL users (admin listing).
pub async fn list_recent(
    ctx: &dyn Context,
    limit: i64,
    offset: i64,
) -> Result<RecordList, WaferError> {
    let opts = ListOptions {
        sort: vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }],
        limit,
        offset,
        ..Default::default()
    };
    db::list(ctx, TABLE, &opts).await
}

/// Total number of share rows (admin stats).
pub async fn count_all(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &[]).await
}

/// CAS-style increment of `access_count` for a share row. Returns `Ok(true)`
/// if a row was updated (and the cap, if any, still allowed the access),
/// `Ok(false)` if the row was already at its cap, or `Err` on DB failure.
///
/// `max <= 0` means unlimited — we only filter on id. Otherwise we add
/// `access_count < max` to the WHERE so two concurrent accesses can't both
/// pass a 1-access cap:
///   UPDATE shares SET access_count = access_count + 1
///   WHERE id = ? AND access_count < max
/// With the cap inside the WHERE clause, at most one updater wins per row
/// and rowcount 0 ⇒ cap reached.
pub async fn increment_access_count_capped(
    ctx: &dyn Context,
    share_id: &str,
    max: i64,
) -> Result<bool, WaferError> {
    let mut filters: Vec<Filter> = vec![Filter {
        field: "id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(share_id.to_string()),
    }];
    if max > 0 {
        filters.push(Filter {
            field: "access_count".to_string(),
            operator: FilterOp::LessThan,
            value: serde_json::json!(max),
        });
    }
    let rows = db::increment_field_where(ctx, TABLE, "access_count", 1, &filters).await?;
    Ok(rows > 0)
}

/// Append an access-log row for `share_id` (`accessed_at` stamped with
/// [`crate::util::now_rfc3339`]).
pub async fn log_access(
    ctx: &dyn Context,
    share_id: &str,
    ip_address: &str,
    user_agent: &str,
) -> Result<Record, WaferError> {
    let data = crate::util::json_map(serde_json::json!({
        "share_id": share_id,
        "accessed_at": crate::util::now_rfc3339(),
        "ip_address": ip_address,
        "user_agent": user_agent,
    }));
    db::create(ctx, ACCESS_LOGS_TABLE, data).await
}

/// Access-log rows, newest first, optionally restricted to one share
/// (admin audit listing).
pub async fn list_access_logs(
    ctx: &dyn Context,
    share_id: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<RecordList, WaferError> {
    let mut filters = Vec::new();
    if let Some(share_id) = share_id {
        filters.push(Filter {
            field: "share_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(share_id.to_string()),
        });
    }
    let opts = ListOptions {
        filters,
        sort: vec![SortField {
            field: "accessed_at".to_string(),
            desc: true,
        }],
        limit,
        offset,
        skip_count: false,
        ..Default::default()
    };
    db::list(ctx, ACCESS_LOGS_TABLE, &opts).await
}

/// Test-fixture seeding: insert a raw share row map exactly as given (no
/// stamped columns), so tests control the precise row shape.
#[cfg(test)]
pub async fn seed(
    ctx: &dyn Context,
    data: std::collections::HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    db::create(ctx, TABLE, data).await
}
