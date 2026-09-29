//! Migration 012 — `wafer_run__auth__sessions` is re-keyed on the refresh
//! rotation `family` (B12), and the sweeper's singleton bookkeeping table is
//! created alongside it.
//!
//! Two paths, because they are genuinely different: a fresh database (where
//! 001 creates the old table moments before 012 replaces it) and a database
//! that already holds the 001 table with rows in it — the shape every live
//! deployment is in when this migration lands.
//!
//! Same harness as `migrations_001`: `call_block("wafer-run/database", ..)`
//! goes to a real `DatabaseBlock` over in-memory SQLite, so the migration
//! runs through the same `exec_raw`/`query_raw` contract it uses in
//! production. Raw SQL in this file is test-fixture setup and assertion, the
//! explicit exception CLAUDE.md carves out.

use impresspress_core::{blocks::auth::migrations, test_support::TestContext};
use serde_json::json;
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
async fn fresh_apply_keys_sessions_on_family_and_drops_the_token_hash_column() {
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
            "012 must declare `{expected}` on the sessions table (got {cols:?})"
        );
    }
    assert!(
        !cols.contains(&"token_hash".to_string()),
        "the access-token key must be gone — a session row is a login family now (got {cols:?})"
    );

    // `db::create` synthesizes an `id` and stamps `updated_at`; both are
    // declared by 012 itself rather than by a later ALTER, because the table
    // is created fresh.
    for bookkeeping in ["id", "updated_at"] {
        assert!(
            cols.contains(&bookkeeping.to_string()),
            "012 must declare `{bookkeeping}` for `db::create` (got {cols:?})"
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

/// The path a live deployment takes: the 001 sessions table already exists,
/// with rows keyed by hashes of access tokens. Those rows cannot be converted
/// — no family is recoverable from a token hash — and they are not
/// credentials, so 012 drops them with the table. What must not happen is the
/// migration silently leaving the old shape in place, which is exactly what
/// `CREATE TABLE IF NOT EXISTS` alone would do.
#[tokio::test]
async fn apply_over_an_existing_001_table_with_rows_replaces_the_shape() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;

    // 1. The pre-012 world, spelled out rather than derived from the
    //    migration list: this is the on-disk shape a deployment running 011
    //    has, and pinning it verbatim is the point — a later edit to 001 must
    //    not quietly change what this test claims to upgrade from.
    for stmt in [
        "CREATE TABLE wafer_run__auth__users ( \
             id TEXT PRIMARY KEY, email TEXT NOT NULL UNIQUE, \
             display_name TEXT NOT NULL, avatar_url TEXT, \
             role TEXT NOT NULL DEFAULT 'user', \
             email_verified INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL )",
        "CREATE TABLE wafer_run__auth__sessions ( \
             token_hash BLOB PRIMARY KEY, \
             user_id TEXT NOT NULL REFERENCES wafer_run__auth__users(id) ON DELETE CASCADE, \
             created_at TEXT NOT NULL, last_used_at TEXT NOT NULL, \
             expires_at TEXT NOT NULL, id TEXT, updated_at TEXT )",
        "CREATE INDEX wafer_run__auth__sessions_user_id_idx \
             ON wafer_run__auth__sessions (user_id)",
        "CREATE INDEX wafer_run__auth__sessions_expires_at_idx \
             ON wafer_run__auth__sessions (expires_at)",
    ] {
        db::ddl(&ctx, stmt).await.expect("build the pre-012 schema");
    }
    assert!(
        columns(&ctx, "wafer_run__auth__sessions")
            .await
            .contains(&"token_hash".to_string()),
        "precondition: the pre-012 sessions table is keyed by token_hash"
    );

    // 2. A user and a session row of the old shape, the way a live database
    //    holds them.
    db::exec_raw(
        &ctx.fixture(),
        "INSERT INTO wafer_run__auth__users \
         (id, email, display_name, role, created_at, updated_at) \
         VALUES ('u1', 'u1@example.com', 'U1', 'user', \
         '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        &[],
    )
    .await
    .expect("seed pre-existing user");
    db::exec_raw(
        &ctx.fixture(),
        "INSERT INTO wafer_run__auth__sessions \
         (token_hash, user_id, created_at, last_used_at, expires_at) \
         VALUES ('deadbeef', 'u1', '2026-01-01T00:00:00Z', \
         '2026-01-01T00:00:00Z', '2099-01-01T00:00:00Z')",
        &[],
    )
    .await
    .expect("seed pre-existing session row");
    assert_eq!(row_count(&ctx, "wafer_run__auth__sessions").await, 1);

    // 3. The full apply, 012 included.
    migrations::apply(&ctx).await.expect("apply migrations");

    let cols = columns(&ctx, "wafer_run__auth__sessions").await;
    assert!(
        cols.contains(&"family".to_string()) && !cols.contains(&"token_hash".to_string()),
        "012 must replace the old shape on an existing table, not skip it (got {cols:?})"
    );
    assert_eq!(
        row_count(&ctx, "wafer_run__auth__sessions").await,
        0,
        "the old rows are hashes of long-expired tokens; they go with the table"
    );
    assert_eq!(
        row_count(&ctx, "wafer_run__auth__users").await,
        1,
        "only the sessions table is replaced — the user row is untouched"
    );

    // The new shape accepts a family row.
    let mut data = std::collections::HashMap::new();
    data.insert("family".to_string(), json!("fam-1"));
    data.insert("user_id".to_string(), json!("u1"));
    data.insert("auth_method".to_string(), json!("password"));
    data.insert("created_at".to_string(), json!("2026-02-01T00:00:00Z"));
    data.insert("last_used_at".to_string(), json!("2026-02-01T00:00:00Z"));
    data.insert("expires_at".to_string(), json!("2099-01-01T00:00:00Z"));
    db::create(&ctx, "wafer_run__auth__sessions", data)
        .await
        .expect("a family-keyed row writes through db::create after 012");
}

/// A second full apply is a no-op on everything except the sessions table it
/// deliberately drops, and leaves both tables in the post-012 shape.
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
