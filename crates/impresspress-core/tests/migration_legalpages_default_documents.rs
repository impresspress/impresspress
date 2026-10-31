//! Legalpages' `003_default_documents` on SQLite, through the real runner: a
//! fresh install holds the two default documents, a database with any
//! document gets none, and once 003 is recorded an emptied table stays empty
//! (`impresspress_core::test_support::legalpages_default_documents`; the
//! PostgreSQL lane is `impresspress-native`'s
//! `tests/postgres_migration_history.rs`).

#![cfg(feature = "block-legalpages")]

use std::sync::{Arc, Mutex};

use impresspress_core::test_support::legalpages_default_documents;
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_sql_utils::Backend;

#[tokio::test(flavor = "multi_thread")]
async fn legalpages_003_seeds_the_defaults_into_an_empty_table_once() {
    let dirs = Mutex::new(Vec::new());
    let new_db = || {
        let dir = tempfile::tempdir().expect("tempdir");
        // File-backed, as native opens one.
        let db: Arc<dyn DatabaseService> = Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open(
                dir.path().join("db.sqlite3").to_str().expect("utf-8 path"),
            )
            .expect("open sqlite"),
        );
        dirs.lock().expect("dirs").push(dir);
        Box::pin(async move { db }) as std::pin::Pin<Box<dyn std::future::Future<Output = _>>>
    };
    let problems = legalpages_default_documents::check(Backend::Sqlite, &new_db).await;
    assert!(problems.is_empty(), "{problems:#?}");
}
