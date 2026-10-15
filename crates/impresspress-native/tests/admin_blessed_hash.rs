//! Admin's migration 009 empties `blessed_hash` on every `block_settings`
//! row, on both SQL dialects, through the tracked runner a native boot applies
//! admin's list with (`migration_helper::apply_pending_via_service`).
//!
//! Two databases matter, and they have different schemas:
//!
//! - **Upgraded**: migrated by a build from before #145, whose `001` created
//!   the column, then by a later build that recorded newer lists without
//!   touching it. Rows still hold the hashes the old build blessed; 009 must
//!   empty every one of them.
//! - **Fresh**: created since #145 removed the column from `001`, so it never
//!   had one. 009 must apply here too, and replay over what it built.
//!
//! The SQLite tests run everywhere. The PostgreSQL one needs a server, so it
//! is `#[ignore]`d; CI's `test-postgres` job runs it with
//! `IMPRESSPRESS_TEST_POSTGRES_URL` naming a database it may create others
//! from.

use std::{collections::HashMap, sync::Arc};

use impresspress_core::{
    blocks::admin::{
        migrations::{migration_files, BLOCK_SETTINGS_CLEAR_BLESSED_HASH},
        ADMIN_BLOCK_ID,
    },
    migration_helper::{
        apply_ddl_via_service, apply_pending_via_service, migration_set_hash, MigrationFile,
    },
    platform_state::block_settings::TABLE,
};
use wafer_core::interfaces::database::service::DatabaseService;

/// The hash an old build blessed for admin and also recorded as applied.
const OLD_ADMIN: &str = "admin-list-blessed-by-an-old-build";

/// The blocks an upgraded database holds rows for: `(block, current_hash,
/// blessed_hash)`. Products is the live incident's shape — a newer build
/// recorded its list, the old build's blessing stayed. Auth's blessing equals
/// its record, which is the one whose re-application signs everyone out.
/// Files was never blessed.
const ROWS: &[(&str, &str, &str)] = &[
    (ADMIN_BLOCK_ID, OLD_ADMIN, OLD_ADMIN),
    (
        "impresspress/products",
        "products-list-main",
        "products-list-old",
    ),
    ("wafer-run/auth", "auth-list", "auth-list"),
    ("impresspress/files", "files-list", ""),
];

/// A list of admin's migration files, in apply order.
type List = &'static [MigrationFile<'static>];

/// Admin's list in `dialect`, split at 009: what an upgraded database had
/// applied, and the whole list.
fn admin_lists(dialect: &str) -> (List, List) {
    let all = migration_files(dialect);
    let at = all
        .iter()
        .position(|(name, _)| *name == BLOCK_SETTINGS_CLEAR_BLESSED_HASH)
        .unwrap_or_else(|| panic!("009 is wired into admin's {dialect} list"));
    (&all[..at], all)
}

fn sql_of(files: List) -> Vec<&'static str> {
    files.iter().map(|(_, sql)| *sql).collect()
}

/// Every `block_settings` row as `block_name -> (current_hash, blessed_hash)`.
async fn rows(db: &Arc<dyn DatabaseService>) -> HashMap<String, (String, String)> {
    let opts = wafer_block::db::ListOptions {
        limit: Some(10_000),
        skip_count: true,
        ..Default::default()
    };
    db.list(TABLE, &opts)
        .await
        .expect("list block_settings")
        .records
        .into_iter()
        .map(|record| {
            let text = |column: &str| {
                record
                    .data
                    .get(column)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{column} is text in {:?}", record.data))
                    .to_string()
            };
            (
                text("block_name"),
                (text("current_hash"), text("blessed_hash")),
            )
        })
        .collect()
}

/// An upgraded database: 001-008 applied, the column an old build's `001`
/// created, and rows the old build blessed. 009 empties every row's
/// `blessed_hash`, leaves every other block's record alone, and records
/// admin's list.
async fn clears_every_row(db: Arc<dyn DatabaseService>, dialect: &str) {
    let (before_009, all) = admin_lists(dialect);
    apply_ddl_via_service(&db, &sql_of(before_009))
        .await
        .expect("admin's 001-008 apply");
    // Test-fixture setup: the column a pre-#145 `001` created. Same in both
    // dialects, and absent before this, so a plain `ADD COLUMN`.
    db.exec_raw(
        &format!("ALTER TABLE {TABLE} ADD COLUMN blessed_hash TEXT NOT NULL DEFAULT ''"),
        &[],
    )
    .await
    .expect("add the pre-#145 column");
    for (i, (block, current, blessed)) in ROWS.iter().enumerate() {
        let mut data = HashMap::new();
        data.insert("id".to_string(), serde_json::json!(format!("bs_{i}")));
        data.insert("block_name".to_string(), serde_json::json!(block));
        data.insert("enabled".to_string(), serde_json::json!(1));
        data.insert("current_hash".to_string(), serde_json::json!(current));
        data.insert("blessed_hash".to_string(), serde_json::json!(blessed));
        data.insert(
            "created_at".to_string(),
            serde_json::json!("2026-09-27T00:00:00Z"),
        );
        data.insert(
            "updated_at".to_string(),
            serde_json::json!("2026-09-27T00:00:00Z"),
        );
        db.create(TABLE, data).await.expect("seed a blessed row");
    }

    apply_pending_via_service(&db, ADMIN_BLOCK_ID, all)
        .await
        .expect("admin's list, 009 included, applies to an upgraded database");

    let after = rows(&db).await;
    assert_eq!(after.len(), ROWS.len(), "no row added or lost: {after:?}");
    for (block, current, _) in ROWS {
        let (recorded, blessed) = &after[*block];
        assert_eq!(blessed, "", "{block}'s blessed_hash is emptied: {after:?}");
        let expected = if *block == ADMIN_BLOCK_ID {
            migration_set_hash(all)
        } else {
            (*current).to_string()
        };
        assert_eq!(
            *recorded, expected,
            "{block}'s current_hash is the one its own list recorded: {after:?}"
        );
    }

    // The next change to admin's list re-runs it whole, 009 included.
    apply_ddl_via_service(&db, &sql_of(all))
        .await
        .expect("admin's list replays over an upgraded database");
}

/// A database created since #145: no `blessed_hash` column until 009 adds it.
/// The whole list applies, the column is there and empty, and the list
/// replays over what it built.
async fn applies_fresh(db: Arc<dyn DatabaseService>, dialect: &str) {
    let (_, all) = admin_lists(dialect);
    apply_pending_via_service(&db, ADMIN_BLOCK_ID, all)
        .await
        .expect("admin's list, 009 included, applies to a fresh database");
    let after = rows(&db).await;
    assert_eq!(
        after.get(ADMIN_BLOCK_ID),
        Some(&(migration_set_hash(all), String::new())),
        "admin's row records its list and has an empty blessed_hash: {after:?}"
    );
    apply_ddl_via_service(&db, &sql_of(all))
        .await
        .expect("admin's list replays over a fresh database");
}

async fn sqlite(dir: &tempfile::TempDir, name: &str) -> Arc<dyn DatabaseService> {
    let path = dir.path().join(name);
    impresspress_native::make_database_service("sqlite", path.to_str().expect("utf-8 path"), None)
        .await
        .expect("open SQLite")
}

#[tokio::test]
async fn migration_009_empties_every_row_on_sqlite() {
    let dir = tempfile::tempdir().expect("tempdir");
    clears_every_row(sqlite(&dir, "upgraded.sqlite3").await, "sqlite").await;
}

#[tokio::test]
async fn migration_009_applies_to_a_fresh_sqlite_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    applies_fresh(sqlite(&dir, "fresh.sqlite3").await, "sqlite").await;
}

/// Both scenarios against PostgreSQL, each on a database of its own.
#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn the_same_holds_on_postgres() {
    let url = std::env::var("IMPRESSPRESS_TEST_POSTGRES_URL")
        .expect("IMPRESSPRESS_TEST_POSTGRES_URL must name a PostgreSQL database");
    let server = impresspress_native::make_postgres_database_service(&url)
        .await
        .expect("connect to PostgreSQL");
    for scenario in ["upgraded", "fresh"] {
        let name = format!("impresspress_admin_009_{scenario}");
        // Test-fixture setup: a database per scenario, created fresh.
        server
            .exec_raw(&format!("DROP DATABASE IF EXISTS {name}"), &[])
            .await
            .expect("drop the scenario's database");
        server
            .exec_raw(&format!("CREATE DATABASE {name}"), &[])
            .await
            .expect("create the scenario's database");
        let (base, _) = url.rsplit_once('/').expect("a database URL");
        let db = impresspress_native::make_postgres_database_service(&format!("{base}/{name}"))
            .await
            .expect("connect to the scenario's database");
        match scenario {
            "upgraded" => clears_every_row(db, "postgres").await,
            _ => applies_fresh(db, "postgres").await,
        }
    }
}
