//! Row-level access over `impresspress__files__cloud_quotas`.
//!
//! Per-user quota override table. Stores explicit byte/file caps that
//! override the block defaults for individual users (one row per user,
//! keyed by `user_id`). The interpretation of a row — field-by-field
//! fallback to `QuotaConfig` defaults — lives in `files::quota`; usage
//! accounting (sums/counts over object rows) lives in
//! [`super::objects`].

use std::collections::HashMap;

use wafer_block::db::{ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record, RecordList};
use wafer_run::{context::Context, WaferError};

/// Per-user quota override table.
pub const TABLE: &str = "impresspress__files__cloud_quotas";

/// Look up `user_id`'s quota-override row. Errors (including NotFound —
/// most users have no override) are surfaced for the caller to map to the
/// block defaults.
pub async fn find_for_user(ctx: &dyn Context, user_id: &str) -> Result<Record, WaferError> {
    db::get_by_field(
        ctx,
        TABLE,
        "user_id",
        serde_json::Value::String(user_id.to_string()),
    )
    .await
}

/// Up to `limit` override rows, unsorted (admin JSON listing).
pub async fn list(ctx: &dyn Context, limit: i64) -> Result<RecordList, WaferError> {
    let opts = ListOptions {
        limit,
        ..Default::default()
    };
    db::list(ctx, TABLE, &opts).await
}

/// Newest override rows first (admin SSR listing).
pub async fn list_recent(ctx: &dyn Context, limit: i64) -> Result<RecordList, WaferError> {
    let opts = ListOptions {
        sort: vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }],
        limit,
        ..Default::default()
    };
    db::list(ctx, TABLE, &opts).await
}

/// Total number of override rows (admin stats).
pub async fn count_all(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &[]).await
}

/// Create-or-replace `user_id`'s override row with the given (already
/// whitelisted — see SEC-059 in `cloud::handle_update_quota`) quota
/// `fields`. `user_id` and an `updated_at` stamp are written here so every
/// upsert path stays consistent.
pub async fn upsert_for_user(
    ctx: &dyn Context,
    user_id: &str,
    mut fields: HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    fields.insert(
        "user_id".to_string(),
        serde_json::Value::String(user_id.to_string()),
    );
    fields.insert(
        "updated_at".to_string(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    db::upsert_by_field(
        ctx,
        TABLE,
        "user_id",
        serde_json::Value::String(user_id.to_string()),
        fields,
    )
    .await
}

/// Test-fixture seeding: insert a raw row map exactly as given (no stamped
/// columns), so tests control the precise row shape.
#[cfg(test)]
pub async fn seed(
    ctx: &dyn Context,
    data: HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    db::create(ctx, TABLE, data).await
}
