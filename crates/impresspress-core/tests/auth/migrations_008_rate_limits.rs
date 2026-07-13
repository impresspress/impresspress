//! Apply migrations through 008; verify `wafer_run__auth__rate_limits`
//! exists and satisfies the `OnConflict::WindowedCounter` upsert contract
//! that `blocks/rate_limit.rs` relies on for the wasm32 (D1) counter path.
//!
//! Before 008 the table had no in-repo migration at all, so on Cloudflare —
//! the only platform that uses it — the windowed upsert failed against a
//! missing table, the caller's `let _ =` swallowed the error, and rate
//! limiting silently never triggered.

use impresspress_core::blocks::auth::migrations;
use serde_json::json;
use wafer_block::{
    db::{Filter, FilterOp},
    wire::database::OnConflict,
};
use wafer_core::clients::database as db;

use crate::common::MigrationTestCtx;

/// Mirrors `blocks/rate_limit.rs`: same table, data shape, conflict target,
/// and counter/window/timestamp field wiring.
const TABLE: &str = "wafer_run__auth__rate_limits";

async fn windowed_upsert(ctx: &MigrationTestCtx, key: &str, now: i64, window_cutoff: i64) {
    let id = format!("rl-test-{key}-{now}");
    db::upsert(
        ctx,
        TABLE,
        vec![
            ("id".to_string(), json!(id)),
            ("key".to_string(), json!(key)),
        ],
        vec!["key".to_string()],
        OnConflict::WindowedCounter {
            count_field: "count".to_string(),
            window_field: "window_start".to_string(),
            now,
            window_cutoff,
            created_fields: vec!["created_at".to_string()],
            updated_fields: vec!["updated_at".to_string()],
        },
    )
    .await
    .expect("windowed-counter upsert must succeed against the migrated table");
}

async fn read_counter(ctx: &MigrationTestCtx, key: &str) -> (i64, i64) {
    let rows = db::list_all(
        ctx,
        TABLE,
        vec![Filter {
            field: "key".into(),
            operator: FilterOp::Equal,
            value: json!(key),
        }],
    )
    .await
    .expect("read back rate-limit row");
    assert_eq!(
        rows.len(),
        1,
        "exactly one row per key (UNIQUE conflict target)"
    );
    let row = &rows[0];
    let count = row.data.get("count").and_then(|v| v.as_i64()).unwrap();
    let window = row
        .data
        .get("window_start")
        .and_then(|v| v.as_i64())
        .unwrap();
    (count, window)
}

#[tokio::test]
async fn migration_008_rate_limits_supports_windowed_counter_upsert() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("migration apply");
    migrations::apply(&ctx)
        .await
        .expect("second apply must succeed (idempotent)");

    // Two hits inside the same window increment the counter and keep the
    // original window_start.
    windowed_upsert(&ctx, "user-1", 1_000, 940).await;
    windowed_upsert(&ctx, "user-1", 1_010, 950).await;
    assert_eq!(read_counter(&ctx, "user-1").await, (2, 1_000));

    // A hit after the window expired (window_start < cutoff) resets the
    // counter and starts a new window.
    windowed_upsert(&ctx, "user-1", 2_000, 1_940).await;
    assert_eq!(read_counter(&ctx, "user-1").await, (1, 2_000));

    // Independent keys do not interfere.
    windowed_upsert(&ctx, "user-2", 2_000, 1_940).await;
    assert_eq!(read_counter(&ctx, "user-1").await, (1, 2_000));
    assert_eq!(read_counter(&ctx, "user-2").await, (1, 2_000));
}
