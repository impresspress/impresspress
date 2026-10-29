//! Auth's `018_drop_jwt_blocklist` on SQLite, through the real runner: it
//! drops the blocklist and nothing else, and fails whole, recording nothing,
//! on a database that lacks the table
//! (`impresspress_core::test_support::jwt_blocklist_drop`; the PostgreSQL
//! lane is `impresspress-native`'s `tests/postgres_migration_history.rs`).

use std::sync::{Arc, Mutex};

use impresspress_core::test_support::jwt_blocklist_drop;
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_sql_utils::Backend;

#[tokio::test(flavor = "multi_thread")]
async fn auth_018_drops_the_blocklist_and_nothing_else() {
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
    let problems = jwt_blocklist_drop::check(Backend::Sqlite, &new_db).await;
    assert!(problems.is_empty(), "{problems:#?}");
}
