//! Row-level access over `impresspress__files__views`.
//!
//! Object-view audit table — one row per tracked object download
//! ([`insert`], written best-effort by `storage::handle_get_object`), read
//! back newest-first by the `/b/storage/api/recent` endpoint
//! ([`list_recent_for_user`]).

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, WaferError};

use super::Page;
use crate::util::RecordExt;

/// Object-view audit table.
pub const TABLE: &str = "impresspress__files__views";

/// One object-view audit row, decoded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct ViewRow {
    pub id: String,
    /// Bucket holding the viewed object.
    pub bucket: String,
    /// Object key within the bucket.
    pub key: String,
    /// The viewer.
    pub user_id: String,
    /// RFC 3339 instant of the view.
    pub viewed_at: String,
    pub created_at: String,
    pub updated_at: String,
}

impl ViewRow {
    /// The one decode of a view row.
    pub fn from_record(rec: &Record) -> Self {
        Self {
            id: rec.id.clone(),
            bucket: rec.str_field("bucket").to_string(),
            key: rec.str_field("key").to_string(),
            user_id: rec.str_field("user_id").to_string(),
            viewed_at: rec.str_field("viewed_at").to_string(),
            created_at: rec.str_field("created_at").to_string(),
            updated_at: rec.str_field("updated_at").to_string(),
        }
    }
}

/// Record that `user_id` viewed `(bucket, key)` (`viewed_at` stamped with
/// [`crate::util::now_rfc3339`]).
pub async fn insert(
    ctx: &dyn Context,
    bucket: &str,
    key: &str,
    user_id: &str,
) -> Result<ViewRow, WaferError> {
    let data = crate::util::json_map(serde_json::json!({
        "bucket": bucket,
        "key": key,
        "user_id": user_id,
        "viewed_at": crate::util::now_rfc3339(),
    }));
    db::create(ctx, TABLE, data)
        .await
        .map(|r| ViewRow::from_record(&r))
}

/// `user_id`'s most recent views, newest first.
pub async fn list_recent_for_user(
    ctx: &dyn Context,
    user_id: &str,
    limit: u32,
) -> Result<Page<ViewRow>, WaferError> {
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
        limit: Some(limit),
        ..Default::default()
    };
    Ok(Page::decode(
        db::list(ctx, TABLE, &opts).await?,
        ViewRow::from_record,
    ))
}

/// Test helper: every row of `TABLE`, undecoded — each column exactly as
/// stored, for asserting that something left the table alone.
#[cfg(test)]
pub async fn raw_rows(
    ctx: &dyn Context,
) -> Result<Vec<wafer_core::clients::database::Record>, WaferError> {
    crate::db_read::list_every(ctx, TABLE, vec![]).await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn from_record_decodes_the_whole_row() {
        let row = ViewRow::from_record(&Record {
            id: "v1".to_string(),
            data: [
                ("bucket".to_string(), json!("photos")),
                ("key".to_string(), json!("a.png")),
                ("user_id".to_string(), json!("alice")),
                ("viewed_at".to_string(), json!("2026-05-06T10:00:00Z")),
                ("created_at".to_string(), json!("2026-05-06T10:00:00Z")),
                ("updated_at".to_string(), json!("2026-05-06T10:00:01Z")),
            ]
            .into_iter()
            .collect(),
        });
        assert_eq!(
            row,
            ViewRow {
                id: "v1".to_string(),
                bucket: "photos".to_string(),
                key: "a.png".to_string(),
                user_id: "alice".to_string(),
                viewed_at: "2026-05-06T10:00:00Z".to_string(),
                created_at: "2026-05-06T10:00:00Z".to_string(),
                updated_at: "2026-05-06T10:00:01Z".to_string(),
            }
        );
    }
}
