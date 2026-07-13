//! Apply migrations through 002; verify the four reserved orgs are seeded and
//! that re-applying doesn't error (idempotency).

use impresspress_core::blocks::auth::migrations;
use wafer_core::clients::database as db;

use crate::common::MigrationTestCtx;

const EXPECTED_RESERVED: &[&str] = &["impresspress", "wafer", "wafer-run"];

#[tokio::test]
async fn migration_002_seeds_three_reserved_orgs_idempotently() {
    let ctx = MigrationTestCtx::new().await;
    migrations::apply(&ctx).await.expect("first apply");
    migrations::apply(&ctx)
        .await
        .expect("second apply must succeed (idempotent)");

    let rows = db::query_raw(
        &ctx,
        "SELECT name FROM wafer_run__auth__orgs WHERE is_reserved = 1 ORDER BY name",
        &[],
    )
    .await
    .expect("query reserved orgs");

    let names: Vec<String> = rows
        .iter()
        .filter_map(|r| {
            r.data
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    assert_eq!(
        names,
        EXPECTED_RESERVED
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        "expected exactly the three reserved orgs, got {names:?}"
    );
}
