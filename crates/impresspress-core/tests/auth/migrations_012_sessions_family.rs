//! `wafer_run__auth__sessions` is keyed on the refresh rotation `family`
//! (B12), and migration 012 creates the sweeper's singleton bookkeeping table.
//!
//! 012 used to re-key the table itself, by dropping 001's
//! `sha256(access_token)`-keyed layout and recreating it. That drop re-ran
//! with every later auth migration and, once a session row became a family's
//! liveness, signed every device out each time. The family-keyed layout now
//! lives in 001 and nothing drops it. These are the fresh database's paths;
//! a database from before the per-file ledger, rows and all, is upgraded
//! from the states main had by `tests/migration_history.rs`, which checks
//! that every seeded row survives.
//!
//! Same harness as `migrations_001`: `call_block("wafer-run/database", ..)`
//! goes to a real `DatabaseBlock` over in-memory SQLite, so the migration
//! runs through the same `exec_raw`/`query_raw` contract it uses in
//! production. Raw SQL in this file is test-fixture setup and assertion, the
//! explicit exception CLAUDE.md carves out.

use impresspress_core::{blocks::auth::migrations, test_support::TestContext};
use wafer_core::clients::database as db;

use crate::common::auth_fixture;

/// The column names of `table`, as SQLite reports them.
async fn columns(ctx: &TestContext, table: &str) -> Vec<String> {
    db::query_raw(&ctx.fixture(), &format!("PRAGMA table_info({table})"), &[])
        .await
        .expect("PRAGMA table_info")
        .iter()
        .filter_map(|r| {
            r.data
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect()
}

async fn row_count(ctx: &TestContext, table: &str) -> i64 {
    db::query_raw(
        &ctx.fixture(),
        &format!("SELECT COUNT(*) AS n FROM {table}"),
        &[],
    )
    .await
    .expect("count rows")
    .first()
    .and_then(|r| r.data.get("n").and_then(|v| v.as_i64()))
    .expect("COUNT(*) returns a number")
}

#[tokio::test]
async fn fresh_apply_keys_sessions_on_family_with_no_token_hash_column() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("apply migrations");

    let cols = columns(&ctx, "wafer_run__auth__sessions").await;
    for expected in [
        "family",
        "user_id",
        "auth_method",
        "created_at",
        "last_used_at",
        "expires_at",
    ] {
        assert!(
            cols.contains(&expected.to_string()),
            "the auth set must declare `{expected}` on the sessions table (got {cols:?})"
        );
    }
    assert!(
        !cols.contains(&"token_hash".to_string()),
        "the access-token key must be gone — a session row is a login family now (got {cols:?})"
    );

    // `db::create` synthesizes an `id` and stamps `updated_at`; 010 adds
    // both.
    for bookkeeping in ["id", "updated_at"] {
        assert!(
            cols.contains(&bookkeeping.to_string()),
            "the auth set must declare `{bookkeeping}` for `db::create` (got {cols:?})"
        );
    }

    // `family` is the primary key, not just a column.
    let pk: Vec<String> = db::query_raw(
        &ctx.fixture(),
        "PRAGMA table_info(wafer_run__auth__sessions)",
        &[],
    )
    .await
    .expect("PRAGMA table_info")
    .iter()
    .filter(|r| r.data.get("pk").and_then(|v| v.as_i64()).unwrap_or(0) > 0)
    .filter_map(|r| {
        r.data
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    })
    .collect();
    assert_eq!(pk, vec!["family".to_string()], "family is the primary key");
}

#[tokio::test]
async fn fresh_apply_creates_the_maintenance_singleton() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("apply migrations");

    let cols = columns(&ctx, "wafer_run__auth__maintenance").await;
    assert!(
        cols.contains(&"last_swept_at".to_string()),
        "the sweeper's stamp column must exist (got {cols:?})"
    );
    assert_eq!(
        row_count(&ctx, "wafer_run__auth__maintenance").await,
        1,
        "the singleton row is seeded by the migration, so the first sweep has a row to update"
    );
}

/// A second full apply is a no-op, and leaves both tables in their shape.
#[tokio::test]
async fn migration_012_is_idempotent() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("first apply");
    migrations::apply(&ctx).await.expect("second apply");

    assert!(columns(&ctx, "wafer_run__auth__sessions")
        .await
        .contains(&"family".to_string()));
    assert_eq!(
        row_count(&ctx, "wafer_run__auth__maintenance").await,
        1,
        "the singleton seed is ON CONFLICT DO NOTHING, so a re-apply does not duplicate it"
    );
}
