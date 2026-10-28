//! The files that declare the stamped columns (auth 017, dev 005, tickets
//! 002, vector 002), on SQLite, through the real runner: right on a fresh
//! install and on a database whose writes created those columns lazily,
//! and, for auth 017, a by-name refusal where a password reset did
//! (`impresspress_core::test_support::stamped_columns`; the PostgreSQL lane
//! is `impresspress-native`'s `tests/postgres_migration_history.rs`).

use std::sync::{Arc, Mutex};

use impresspress_core::test_support::stamped_columns;
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_sql_utils::Backend;

#[tokio::test(flavor = "multi_thread")]
async fn the_stamped_column_files_are_right_for_both_kinds_of_database() {
    let dirs = Mutex::new(Vec::new());
    let new_db = || {
        let dir = tempfile::tempdir().expect("tempdir");
        // File-backed, as native opens one, and without strict schema, as
        // native and the browser ran before it: the lazy writes need it.
        let db: Arc<dyn DatabaseService> = Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open(
                dir.path().join("db.sqlite3").to_str().expect("utf-8 path"),
            )
            .expect("open sqlite"),
        );
        dirs.lock().expect("dirs").push(dir);
        Box::pin(async move { db }) as std::pin::Pin<Box<dyn std::future::Future<Output = _>>>
    };
    let problems = stamped_columns::check(Backend::Sqlite, &new_db).await;
    assert!(problems.is_empty(), "{problems:#?}");
}
