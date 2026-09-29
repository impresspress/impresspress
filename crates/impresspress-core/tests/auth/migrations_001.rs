//! Apply migration 001 against in-memory SQLite; verify all §3 tables exist.
//!
//! Uses a minimal `Context` test harness that dispatches
//! `call_block("wafer-run/database", ...)` to a real `DatabaseBlock` wrapping
//! an in-memory `SQLiteDatabaseService`. This exercises the same message
//! contract (`exec_raw` / `query_raw`) the block uses at runtime.
//!
//! `config::get_default(ctx, "WAFER_RUN_SHARED__DATABASE__BACKEND", "sqlite")`
//! falls back to `"sqlite"` because the fixture's config block holds no key —
//! the unset default keeps the test self-contained.

use impresspress_core::blocks::auth::migrations;
use wafer_core::clients::database as db;

use crate::common::auth_fixture;

const EXPECTED_TABLES: &[&str] = &[
    "wafer_run__auth__users",
    "wafer_run__auth__local_credentials",
    "wafer_run__auth__provider_links",
    "wafer_run__auth__orgs",
    "wafer_run__auth__sessions",
    "wafer_run__auth__personal_access_tokens",
    "wafer_run__auth__bootstrap_tokens",
];

#[tokio::test]
async fn migration_001_creates_all_tables() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("migration 001 apply");

    let rows = db::query_raw(
        &ctx.fixture(),
        "SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'wafer_run__auth__%'",
        &[],
    )
    .await
    .expect("query sqlite_master");

    let names: Vec<String> = rows
        .iter()
        .filter_map(|r| {
            r.data
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    for t in EXPECTED_TABLES {
        assert!(
            names.contains(&t.to_string()),
            "table missing: {t} (got {names:?})"
        );
    }
}

#[tokio::test]
async fn migration_001_is_idempotent() {
    let ctx = auth_fixture(impresspress_core::blocks::auth::AUTH_BLOCK_ID).await;
    migrations::apply(&ctx).await.expect("first apply");
    migrations::apply(&ctx)
        .await
        .expect("second apply must succeed (idempotent)");
}
