//! Row-level access over `impresspress__files__objects`.
//!
//! Object metadata rows — one row per uploaded file (sibling of the raw
//! storage blob in `wafer-run/storage`). Tracks size, content type,
//! status, uploader and timestamps. Rows are inserted `pending` *before*
//! the storage upload (to close the quota TOCTOU window) and flipped to
//! `complete` afterward; quota accounting sums/counts by `uploaded_by`
//! (including in-flight `pending` reservations), while user-facing search
//! and admin stats only see `complete` rows.

use std::collections::HashMap;

use wafer_block::{
    db::{Filter, FilterOp, ListOptions, SortField},
    wire::database as wire,
};
use wafer_core::clients::database::{self as db, Record, RecordList};
use wafer_run::{context::Context, WaferError};

use crate::util::RecordExt;

/// Object metadata table — one row per uploaded file (sibling of the raw
/// storage blob in `wafer-run/storage`). Tracks size, content type, status,
/// uploader and timestamps.
pub const TABLE: &str = "impresspress__files__objects";

/// Filter matching only fully uploaded rows (`status = 'complete'`),
/// excluding in-flight `pending` reservations.
fn complete_filter() -> [Filter; 1] {
    [Filter {
        field: "status".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String("complete".to_string()),
    }]
}

/// Filter matching all objects uploaded by `user_id` (the rows that count
/// toward that user's quota, including in-flight `pending` reservations).
fn owned_objects_filter(user_id: &str) -> Vec<Filter> {
    vec![Filter {
        field: "uploaded_by".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id.to_string()),
    }]
}

/// Escape SQL LIKE wildcards (`%`, `_`) and the escape char itself (`\`) in
/// user-supplied search terms so a user searching for `100% off` doesn't
/// also match arbitrary characters.
///
/// SQLite's `LIKE` has *no* default escape character — a bare backslash is
/// just a literal byte, so escaping here would be silently inert on its own.
/// What makes it effective is the `wafer-sql-utils` `FilterOp::Like` builder
/// (used by [`search_completed`]'s query below), which renders an explicit
/// `ESCAPE '\'` clause on every backend (SQLite/D1 and Postgres) — see
/// `wafer-sql-utils::query::leaf_expr`. Without that clause, a query
/// containing `_` or `%` would match as a wildcard instead of a literal
/// character.
fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            other => out.push(other),
        }
    }
    out
}

/// Insert the `pending` reservation row written BEFORE the storage upload,
/// so concurrent quota checks see the in-flight size (closes the
/// check-quota → upload TOCTOU race). `uploaded_at` is stamped with
/// [`crate::util::now_rfc3339`].
pub async fn insert_pending(
    ctx: &dyn Context,
    bucket: &str,
    key: &str,
    size: usize,
    content_type: &str,
    uploaded_by: &str,
) -> Result<Record, WaferError> {
    let data = crate::util::json_map(serde_json::json!({
        "bucket": bucket,
        "key": key,
        "size": size,
        "content_type": content_type,
        "status": "pending",
        "uploaded_by": uploaded_by,
        "uploaded_at": crate::util::now_rfc3339(),
    }));
    db::create(ctx, TABLE, data).await
}

/// Flip a `pending` row to `status = 'complete'` after its storage upload
/// succeeded.
pub async fn mark_complete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    let data = crate::util::json_map(serde_json::json!({ "status": "complete" }));
    db::update(ctx, TABLE, id, data).await.map(|_| ())
}

/// Hard-delete one object row by id (the compensating delete when a
/// storage upload fails after its `pending` row was inserted).
pub async fn delete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, TABLE, id).await
}

/// Delete every object row in `bucket` (bucket-deletion metadata cleanup).
pub async fn delete_for_bucket(ctx: &dyn Context, bucket: &str) -> Result<(), WaferError> {
    db::delete_by_field(
        ctx,
        TABLE,
        "bucket",
        serde_json::Value::String(bucket.to_string()),
    )
    .await
}

/// Delete the object row for `(bucket, key)` (object-deletion metadata
/// cleanup).
pub async fn delete_by_bucket_key(
    ctx: &dyn Context,
    bucket: &str,
    key: &str,
) -> Result<(), WaferError> {
    db::delete_by_filters(
        ctx,
        TABLE,
        vec![
            Filter {
                field: "bucket".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(bucket.to_string()),
            },
            Filter {
                field: "key".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(key.to_string()),
            },
        ],
    )
    .await
}

/// Delete `user_id`'s `pending`-status rows with `uploaded_at` strictly
/// before `cutoff` (an RFC 3339 timestamp, string-compared the same way the
/// column is written). See `quota::sweep_stale_pending` for the policy and
/// why this is safe to run best-effort on every upload.
pub async fn delete_stale_pending(
    ctx: &dyn Context,
    user_id: &str,
    cutoff: &str,
) -> Result<(), WaferError> {
    let filters = vec![
        Filter {
            field: "uploaded_by".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id.to_string()),
        },
        Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String("pending".to_string()),
        },
        Filter {
            field: "uploaded_at".to_string(),
            operator: FilterOp::LessThan,
            value: serde_json::Value::String(cutoff.to_string()),
        },
    ];
    db::delete_by_filters(ctx, TABLE, filters).await
}

/// Search `user_id`'s `complete` objects whose key contains `query`
/// (case rules per backend `LIKE`), newest upload first. `query` is
/// LIKE-escaped here ([`escape_like`]) so `%`/`_` match literally.
pub async fn search_completed(
    ctx: &dyn Context,
    user_id: &str,
    query: &str,
    limit: i64,
    offset: i64,
) -> Result<RecordList, WaferError> {
    let opts = ListOptions {
        filters: vec![
            Filter {
                field: "key".to_string(),
                operator: FilterOp::Like,
                value: serde_json::Value::String(format!("%{}%", escape_like(query))),
            },
            // Only show the current user's files
            Filter {
                field: "uploaded_by".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(user_id.to_string()),
            },
            // Exclude pending uploads
            Filter {
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String("complete".to_string()),
            },
        ],
        sort: vec![SortField {
            field: "uploaded_at".to_string(),
            desc: true,
        }],
        limit,
        offset,
        skip_count: false,
        ..Default::default()
    };
    db::list(ctx, TABLE, &opts).await
}

/// List up to `limit` object rows in `bucket`, sorted by `key` ascending
/// (the SSR object-browser order).
pub async fn list_for_bucket(
    ctx: &dyn Context,
    bucket: &str,
    limit: i64,
) -> Result<RecordList, WaferError> {
    let opts = ListOptions {
        filters: vec![Filter {
            field: "bucket".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(bucket.to_string()),
        }],
        sort: vec![SortField {
            field: "key".to_string(),
            desc: false,
        }],
        limit,
        ..Default::default()
    };
    db::list(ctx, TABLE, &opts).await
}

/// Object counts per bucket for the given bucket names, via a single
/// GROUP BY aggregate (one row per bucket) — avoids an N+1 `db::count` per
/// bucket. Counts ALL rows in each bucket regardless of `uploaded_by` or
/// status, matching the previous per-bucket `db::count` semantics. Buckets
/// with zero objects are simply absent from the returned map.
pub async fn count_by_bucket(
    ctx: &dyn Context,
    bucket_names: &[String],
) -> Result<HashMap<String, i64>, WaferError> {
    let names: Vec<serde_json::Value> = bucket_names
        .iter()
        .map(|s| serde_json::Value::String(s.clone()))
        .collect();
    let req = wire::AggregateRequest {
        collection: TABLE.to_string(),
        select_columns: vec!["bucket".into()],
        aggregates: vec![wire::AggregateColumnDef::Count {
            alias: "cnt".into(),
        }],
        filters: vec![wire::FilterNode::Leaf(wire::FilterDef {
            field: "bucket".into(),
            operator: "in".into(),
            value: serde_json::Value::Array(names),
        })],
        group_by: vec![wire::GroupByDef::Column("bucket".into())],
        sort: vec![],
        limit: 0,
    };
    let rows = db::aggregate(ctx, req).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let bucket = r.data.get("bucket").and_then(|v| v.as_str())?.to_string();
            let cnt = r.i64_field("cnt");
            Some((bucket, cnt))
        })
        .collect())
}

/// Number of `complete` object rows (admin stats).
pub async fn count_completed(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &complete_filter()).await
}

/// `SUM(size)` over `complete` object rows (admin stats).
pub async fn sum_size_completed(ctx: &dyn Context) -> Result<f64, WaferError> {
    db::sum(ctx, TABLE, "size", &complete_filter()).await
}

/// Number of object rows uploaded by `user_id` (quota accounting —
/// includes `pending` reservations).
pub async fn count_for_uploader(ctx: &dyn Context, user_id: &str) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &owned_objects_filter(user_id)).await
}

/// `SUM(size)` over the rows uploaded by `user_id` (quota accounting —
/// includes `pending` reservations; no row materialization).
pub async fn sum_size_for_uploader(ctx: &dyn Context, user_id: &str) -> Result<f64, WaferError> {
    db::sum(ctx, TABLE, "size", &owned_objects_filter(user_id)).await
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

/// Test helper: every object row, unfiltered.
#[cfg(test)]
pub async fn list_all(ctx: &dyn Context) -> Result<Vec<Record>, WaferError> {
    db::list_all(ctx, TABLE, vec![]).await
}
