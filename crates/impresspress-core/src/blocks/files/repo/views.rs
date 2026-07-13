//! Row-level access over `impresspress__files__views`.
//!
//! Object-view audit table — one row per tracked object download
//! ([`insert`], written best-effort by `storage::handle_get_object`), read
//! back newest-first by the `/b/storage/api/recent` endpoint
//! ([`list_recent_for_user`]).

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record, RecordList};
use wafer_run::{context::Context, WaferError};

/// Object-view audit table.
pub const TABLE: &str = "impresspress__files__views";

/// Record that `user_id` viewed `(bucket, key)` (`viewed_at` stamped with
/// [`crate::util::now_rfc3339`]).
pub async fn insert(
    ctx: &dyn Context,
    bucket: &str,
    key: &str,
    user_id: &str,
) -> Result<Record, WaferError> {
    let data = crate::util::json_map(serde_json::json!({
        "bucket": bucket,
        "key": key,
        "user_id": user_id,
        "viewed_at": crate::util::now_rfc3339(),
    }));
    db::create(ctx, TABLE, data).await
}

/// `user_id`'s most recent views, newest first.
pub async fn list_recent_for_user(
    ctx: &dyn Context,
    user_id: &str,
    limit: i64,
) -> Result<RecordList, WaferError> {
    let opts = ListOptions {
        filters: vec![Filter {
            field: "user_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id.to_string()),
        }],
        sort: vec![SortField {
            field: "viewed_at".to_string(),
            desc: true,
        }],
        limit,
        ..Default::default()
    };
    db::list(ctx, TABLE, &opts).await
}
