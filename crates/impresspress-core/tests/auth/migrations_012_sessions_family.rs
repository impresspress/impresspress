//! `wafer_run__auth__sessions` is keyed on the refresh rotation `family`
//! (B12), and migration 012 creates the sweeper's singleton bookkeeping table.
//!
//! 012 used to re-key the table itself, by dropping 001's
//! `sha256(access_token)`-keyed layout and recreating it. That drop re-ran
//! with every later auth migration and, once a session row became a family's
//! liveness, signed every device out each time. The family-keyed layout now
//! lives in 001 and nothing drops it. Two paths: a fresh database, and one
//! that already holds the family-keyed table with rows in it, which is the
//! shape every live deployment is in when this change lands.
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

/// The path a live deployment takes: the family-keyed table already exists,
/// created by the old 012, with a signed-in device in it. Running the set
/// again (any auth migration change does) must leave the row, since the row
/// is what keeps the device signed in.
#[tokio::test]
async fn apply_over_an_existing_family_table_keeps_its_rows() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;

    // 1. The post-B12 world, spelled out rather than derived from the
    //    migration list: the on-disk shape the old 012 left, pinned verbatim
    //    so a later edit to 001 cannot quietly change what this test claims
    //    to upgrade from.
    for stmt in [
        "CREATE TABLE wafer_run__auth__users ( \
             id TEXT PRIMARY KEY, email TEXT NOT NULL UNIQUE, \
             display_name TEXT NOT NULL, avatar_url TEXT, \
             role TEXT NOT NULL DEFAULT 'user', \
             email_verified INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL, updated_at TEXT NOT NULL )",
        "CREATE TABLE wafer_run__auth__sessions ( \
             family TEXT PRIMARY KEY, \
             user_id TEXT NOT NULL REFERENCES wafer_run__auth__users(id) ON DELETE CASCADE, \
             auth_method TEXT NOT NULL DEFAULT '', \
             created_at TEXT NOT NULL, last_used_at TEXT NOT NULL, \
             expires_at TEXT NOT NULL, id TEXT, updated_at TEXT )",
        "CREATE INDEX wafer_run__auth__sessions_user_id_idx \
             ON wafer_run__auth__sessions (user_id)",
        "CREATE INDEX wafer_run__auth__sessions_expires_at_idx \
             ON wafer_run__auth__sessions (expires_at)",
    ] {
        db::ddl(&ctx, stmt)
            .await
            .expect("build the post-B12 schema");
    }

    // 2. A user and a signed-in device, the way a live database holds them.
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
         (family, user_id, auth_method, created_at, last_used_at, expires_at) \
         VALUES ('fam-1', 'u1', 'password', '2026-01-01T00:00:00Z', \
         '2026-01-01T00:00:00Z', '2099-01-01T00:00:00Z')",
        &[],
    )
    .await
    .expect("seed a signed-in device");

    // 3. The full apply.
    migrations::apply(&ctx).await.expect("apply migrations");

    assert_eq!(
        row_count(&ctx, "wafer_run__auth__sessions").await,
        1,
        "the signed-in device survives the set"
    );
    assert!(columns(&ctx, "wafer_run__auth__sessions")
        .await
        .contains(&"family".to_string()));
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
