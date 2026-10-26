//! Every block's PostgreSQL migration set, applied to a fresh database and
//! then replayed over the result, through the statement loop a frozen
//! migration runs through.
//!
//! The files every block shipped before the per-file ledger (its frozen
//! files, `migration_helper::FROZEN_THROUGH`) were written to be run again
//! over the schema they already built: the runner before the ledger re-ran a
//! block's whole set whenever its hash moved, the legacy replay runs the
//! frozen set once more over a deployment whose record from that era proves
//! nothing, and a frozen file that failed part-way runs again from its start.
//! So every frozen `.postgres.sql` file has to succeed a second time over the
//! schema it already built. Each migration's own tests cover what it does to
//! rows; this covers that each set survives its replay at all. A file added
//! after the ledger runs once, in a transaction, and is not replayed here.
//!
//! The sets are read off disk, one per `src/blocks/*/migrations` directory,
//! in file-name order, keeping the files at or before the block's freeze mark
//! — a glob, so a new block is covered without anyone listing it. Each set is joined with `\n` and handed to
//! `apply_ddl_via_service`, which shares its statement loop (and its
//! duplicate-`ADD COLUMN` tolerance) with the frozen-file runner.
//!
//! Needs a server, so it is `#[ignore]`d. CI's `test-postgres` job runs it
//! against an empty database named by `IMPRESSPRESS_TEST_POSTGRES_URL`.

#![cfg(feature = "postgres")]

use std::path::{Path, PathBuf};

const URL_VAR: &str = "IMPRESSPRESS_TEST_POSTGRES_URL";

/// `(block, [frozen *.postgres.sql in file-name order])` for every block that
/// ships any, in block-name order.
fn block_migration_sets() -> Vec<(String, Vec<PathBuf>)> {
    let blocks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../impresspress-core/src/blocks");
    let mut sets = Vec::new();
    for entry in std::fs::read_dir(&blocks).expect("read the blocks directory") {
        let dir = entry
            .expect("read a blocks entry")
            .path()
            .join("migrations");
        if !dir.is_dir() {
            continue;
        }
        let block = dir
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .expect("block directory name")
            .to_string();
        let block_name = impresspress_core::migration_helper::FROZEN_THROUGH
            .iter()
            .map(|(name, _)| *name)
            .find(|name| name.rsplit_once('/').map(|(_, dir)| dir) == Some(block.as_str()))
            .unwrap_or_else(|| panic!("{block} has no freeze mark"));
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("read a migrations directory")
            .map(|file| file.expect("read a migration entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".postgres.sql"))
                    .is_some_and(|basename| {
                        impresspress_core::migration_helper::is_frozen(block_name, basename)
                    })
            })
            .collect();
        if files.is_empty() {
            continue;
        }
        files.sort();
        sets.push((block, files));
    }
    sets.sort();
    sets
}

#[tokio::test]
#[ignore = "needs an empty PostgreSQL database named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn every_block_migration_set_applies_fresh_and_replays() {
    let url = std::env::var(URL_VAR)
        .unwrap_or_else(|_| panic!("{URL_VAR} must name an empty PostgreSQL database"));
    let db = impresspress_native::make_postgres_database_service(&url)
        .await
        .expect("connect to PostgreSQL");

    // "Fresh" is part of the claim: a schema some earlier step built would
    // turn the first pass into a replay and leave the fresh apply untested.
    let tables = db
        .query_raw(
            "SELECT count(*) AS n FROM information_schema.tables WHERE table_schema = 'public'",
            &[],
        )
        .await
        .expect("count existing tables");
    assert_eq!(
        tables[0].data.get("n"),
        Some(&serde_json::json!(0)),
        "{URL_VAR} must name an EMPTY database"
    );

    let sets = block_migration_sets();
    assert!(
        sets.len() >= 10,
        "found only {} blocks with PostgreSQL migrations: {:?}",
        sets.len(),
        sets.iter().map(|(block, _)| block).collect::<Vec<_>>()
    );

    for pass in ["fresh", "replay"] {
        for (block, files) in &sets {
            let joined = files
                .iter()
                .map(|file| {
                    std::fs::read_to_string(file)
                        .unwrap_or_else(|e| panic!("read {}: {e}", file.display()))
                })
                .collect::<Vec<_>>()
                .join("\n");
            println!("{pass}: {block} ({} files)", files.len());
            impresspress_core::migration_helper::apply_ddl_via_service(&db, &[joined.as_str()])
                .await
                .unwrap_or_else(|e| panic!("{pass} apply of the {block} migrations failed: {e}"));
        }
    }
}
