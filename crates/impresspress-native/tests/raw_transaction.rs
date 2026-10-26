//! `DatabaseService::exec_raw_transaction` on the databases a native
//! deployment opens (`make_database_service`), each a real server or file:
//! the statements commit together or not at all, DDL included, and a failure
//! carries its classified code, the failing statement's position and the
//! backend's message.
//!
//! This is the primitive the per-file migration runner will apply a file
//! with, its ledger row first, so "nothing of a refused file remains" is the
//! claim every test here checks by reading the schema back.
//!
//! The SQLite tests run everywhere, on a file (the native topology: one
//! writer, read-only readers). The PostgreSQL one needs a server, so it is
//! `#[ignore]`d; CI's `test-postgres` job runs it with
//! `IMPRESSPRESS_TEST_POSTGRES_URL` naming a database it may create others
//! from.

use std::sync::Arc;

use wafer_core::interfaces::database::service::{
    DatabaseError, DatabaseService, RawStatement, TransactionError,
};

/// A statement with no bind arguments: every statement here is the same text
/// in both dialects.
fn raw(query: &str) -> RawStatement {
    RawStatement {
        query: query.to_string(),
        args: vec![],
    }
}

const A: &str = "ip1_tx_a";
const B: &str = "ip1_tx_b";

async fn assert_absent(db: &dyn DatabaseService, tables: &[&str], after: &str) {
    for table in tables {
        assert!(
            !db.schema_table_exists(table).await.expect("table lookup"),
            "{table} survived {after}"
        );
    }
}

async fn refused(db: &dyn DatabaseService, statements: &[RawStatement]) -> TransactionError {
    db.exec_raw_transaction(statements)
        .await
        .expect_err("the transaction is refused")
}

/// Every claim, against one database that starts without the test's tables.
async fn all_or_nothing(db: Arc<dyn DatabaseService>) {
    let db = db.as_ref();

    // Two tables created, then a statement the database refuses: neither
    // table remains, and the error names the third statement.
    let err = refused(
        db,
        &[
            raw(&format!("CREATE TABLE {A} (id TEXT PRIMARY KEY)")),
            raw(&format!("CREATE TABLE {B} (x TEXT)")),
            raw("INSERT INTO ip1_tx_missing (id) VALUES ('x')"),
        ],
    )
    .await;
    assert_eq!(err.statement, Some(2), "{err}");
    match &err.error {
        DatabaseError::Internal(message) => {
            assert!(message.contains("ip1_tx_missing"), "{message}");
        }
        other => panic!("a missing table is a fault, not {other:?}"),
    }
    assert_absent(db, &[A, B], "a refused third statement").await;

    // DDL, DML and an `ALTER`, then a taken key: `AlreadyExists` at its
    // position, and the table, its column and its row are gone with it.
    let err = refused(
        db,
        &[
            raw(&format!("CREATE TABLE {A} (id TEXT PRIMARY KEY)")),
            raw(&format!("INSERT INTO {A} (id) VALUES ('k')")),
            raw(&format!("ALTER TABLE {A} ADD COLUMN note TEXT")),
            raw(&format!("INSERT INTO {A} (id) VALUES ('k')")),
        ],
    )
    .await;
    assert_eq!(err.statement, Some(3), "{err}");
    assert!(
        matches!(err.error, DatabaseError::AlreadyExists(_)),
        "a taken key is AlreadyExists: {err}"
    );
    assert_absent(db, &[A], "a taken key at the fourth statement").await;

    // The same file without the duplicate commits whole, and a typed read
    // sees the column the transaction added.
    let counts = db
        .exec_raw_transaction(&[
            raw(&format!("CREATE TABLE {A} (id TEXT PRIMARY KEY)")),
            raw(&format!("ALTER TABLE {A} ADD COLUMN note TEXT")),
            raw(&format!("INSERT INTO {A} (id, note) VALUES ('k', 'kept')")),
        ])
        .await
        .expect("the transaction commits");
    assert_eq!(counts.len(), 3);
    assert_eq!(counts[2], 1, "the insert's count");
    let row = db.get(A, "k").await.expect("the committed row");
    assert_eq!(row.data.get("note"), Some(&serde_json::json!("kept")));

    // Leave the database as it was found.
    db.exec_raw_transaction(&[raw(&format!("DROP TABLE {A}"))])
        .await
        .expect("drop the test's table");
    assert_absent(db, &[A], "the cleanup").await;
}

#[tokio::test]
async fn sqlite_runs_a_raw_transaction_all_or_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("raw_transaction.sqlite3");
    let db = impresspress_native::make_database_service(
        "sqlite",
        path.to_str().expect("utf-8 path"),
        None,
    )
    .await
    .expect("open SQLite");
    all_or_nothing(db).await;
}

/// The same claims against PostgreSQL, on a database of their own.
#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn postgres_runs_a_raw_transaction_all_or_nothing() {
    let url = std::env::var("IMPRESSPRESS_TEST_POSTGRES_URL")
        .expect("IMPRESSPRESS_TEST_POSTGRES_URL must name a PostgreSQL database");
    let server = impresspress_native::make_postgres_database_service(&url)
        .await
        .expect("connect to PostgreSQL");
    let name = "impresspress_raw_transaction";
    // Test-fixture setup: a database of its own, created fresh.
    server
        .exec_raw(&format!("DROP DATABASE IF EXISTS {name}"), &[])
        .await
        .expect("drop the test's database");
    server
        .exec_raw(&format!("CREATE DATABASE {name}"), &[])
        .await
        .expect("create the test's database");
    let (base, _) = url.rsplit_once('/').expect("a database URL");
    let db = impresspress_native::make_postgres_database_service(&format!("{base}/{name}"))
        .await
        .expect("connect to the test's database");
    all_or_nothing(db).await;
}
