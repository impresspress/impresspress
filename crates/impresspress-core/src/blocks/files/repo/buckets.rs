//! Row-level access over `impresspress__files__buckets`.
//!
//! Buckets are user-created storage containers (one row per bucket). The
//! table is the single source of truth for bucket existence / ownership /
//! visibility — both the admin and user listing paths read it, and
//! [`find_owned`] is *the* ownership lookup every access-control caller
//! derives from (`storage::bucket_owned_by` → `is_bucket_access_denied`,
//! the SSR portal's owner check, and the share-creation path).

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record, RecordList};
use wafer_run::{context::Context, WaferError};

/// Buckets table — user-created storage containers (one row per bucket).
pub const TABLE: &str = "impresspress__files__buckets";

fn created_by_filter(user_id: &str) -> Filter {
    Filter {
        field: "created_by".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id.to_string()),
    }
}

/// Look up the bucket named `name` owned by `user_id`. Returns `Ok(None)`
/// when no such row exists (unknown bucket OR a bucket owned by someone
/// else — callers cannot distinguish the two, by design).
///
/// This is the single bucket-ownership predicate for the files block;
/// `storage::bucket_owned_by` layers the fail-closed bool + logging on top,
/// and the admin-bypass policy split lives in
/// `storage::is_bucket_access_denied` (see its docs).
pub async fn find_owned(
    ctx: &dyn Context,
    name: &str,
    user_id: &str,
) -> Result<Option<Record>, WaferError> {
    let filters = vec![
        Filter {
            field: "name".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(name.to_string()),
        },
        created_by_filter(user_id),
    ];
    let records = db::list_all(ctx, TABLE, filters).await?;
    Ok(records.into_iter().next())
}

/// List bucket rows visible to `owner`: `Some(user_id)` restricts to that
/// user's buckets (`created_by` filter), `None` returns every bucket (the
/// admin view). Unsorted, unpaginated — mirrors the JSON API listing.
pub async fn list_visible(
    ctx: &dyn Context,
    owner: Option<&str>,
) -> Result<Vec<Record>, WaferError> {
    let filters = match owner {
        Some(user_id) => vec![created_by_filter(user_id)],
        None => Vec::new(),
    };
    db::list_all(ctx, TABLE, filters).await
}

/// List `user_id`'s buckets sorted by `name` ascending (the SSR bucket-list
/// page order).
pub async fn list_owned_sorted(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<Vec<Record>, WaferError> {
    db::list_sorted(
        ctx,
        TABLE,
        vec![created_by_filter(user_id)],
        vec![SortField {
            field: "name".to_string(),
            desc: false,
        }],
    )
    .await
}

/// Most recently created buckets, newest first (admin listing).
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

/// Insert a bucket row (`created_at` stamped with
/// [`crate::util::now_rfc3339`]) and return it.
pub async fn insert(
    ctx: &dyn Context,
    name: &str,
    public: bool,
    created_by: &str,
) -> Result<Record, WaferError> {
    let data = crate::util::json_map(serde_json::json!({
        "name": name,
        "public": public,
        "created_by": created_by,
        "created_at": crate::util::now_rfc3339(),
    }));
    db::create(ctx, TABLE, data).await
}

/// Delete the bucket row named `name` (bucket names are unique).
pub async fn delete_by_name(ctx: &dyn Context, name: &str) -> Result<(), WaferError> {
    db::delete_by_field(
        ctx,
        TABLE,
        "name",
        serde_json::Value::String(name.to_string()),
    )
    .await
}

/// Total number of bucket rows (admin stats).
pub async fn count_all(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &[]).await
}

/// Test-fixture seeding: insert a raw row map exactly as given (no stamped
/// columns), so tests control the precise row shape.
#[cfg(test)]
pub async fn seed(
    ctx: &dyn Context,
    data: std::collections::HashMap<String, serde_json::Value>,
) -> Result<Record, WaferError> {
    db::create(ctx, TABLE, data).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestContext;

    /// The ownership predicate matches on BOTH `name` and `created_by`:
    /// a hit requires the exact (bucket, owner) pair, cross-user lookups
    /// and unknown buckets both come back `None`.
    #[tokio::test]
    async fn find_owned_matches_only_the_name_owner_pair() {
        let ctx = TestContext::with_files().await;
        insert(&ctx, "photos", false, "alice").await.expect("seed");
        insert(&ctx, "docs", true, "bob").await.expect("seed");

        let hit = find_owned(&ctx, "photos", "alice")
            .await
            .expect("find_owned")
            .expect("alice owns photos");
        assert_eq!(
            hit.data.get("name").and_then(|v| v.as_str()),
            Some("photos")
        );
        assert_eq!(
            hit.data.get("created_by").and_then(|v| v.as_str()),
            Some("alice")
        );

        // Someone else's bucket → None (cross-user isolation).
        assert!(find_owned(&ctx, "photos", "bob")
            .await
            .expect("find_owned")
            .is_none());
        // Unknown bucket → None.
        assert!(find_owned(&ctx, "missing", "alice")
            .await
            .expect("find_owned")
            .is_none());
    }
}
