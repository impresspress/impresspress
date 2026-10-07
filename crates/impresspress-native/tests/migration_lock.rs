//! The migration lock a native boot holds while it applies migrations: a
//! second process waits for it, a holder that died is taken over (SQLite
//! lease) or released by the server (PostgreSQL session), and a holder that
//! can no longer show it holds the lock is told so.
//!
//! The SQLite tests run everywhere, with a short [`Lease`] so a take-over
//! takes a fraction of a second. The PostgreSQL one needs a server, so it is
//! `#[ignore]`d; CI's `test-postgres` job runs it with
//! `IMPRESSPRESS_TEST_POSTGRES_URL` naming a database.

use std::{collections::HashMap, sync::Arc, time::Duration};

use impresspress_native::migration_lock::{Lease, MigrationLock, LEASE_TABLE};
use serde_json::json;
use wafer_core::interfaces::database::service::DatabaseService;

const SHORT: Lease = Lease {
    heartbeat: Duration::from_millis(100),
    stale_after: Duration::from_millis(600),
    poll: Duration::from_millis(50),
};

fn sqlite(dir: &tempfile::TempDir) -> Arc<dyn DatabaseService> {
    impresspress_native::make_sqlite_database_service(
        dir.path()
            .join("lock.sqlite3")
            .to_str()
            .expect("utf-8 path"),
    )
    .expect("open sqlite")
}

/// A second process waits while the first holds the lease, and gets it once
/// the first releases it.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_process_waits_for_the_lease() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = MigrationLock::acquire_lease(&sqlite(&dir), SHORT)
        .await
        .expect("the first process takes the lease");
    let second_db = sqlite(&dir);
    let second = tokio::spawn(async move { MigrationLock::acquire_lease(&second_db, SHORT).await });
    tokio::time::sleep(SHORT.stale_after * 2).await;
    assert!(
        !second.is_finished(),
        "a holder that keeps beating keeps the lease"
    );
    first.release().await.expect("release");
    let second = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .expect("the second process gets the lease once it is released")
        .expect("join")
        .expect("acquire");
    second.release().await.expect("release");
}

/// A holder that died mid-boot — its row is there, its heartbeat never moves
/// — is taken over once a waiter has watched it stand still for
/// `stale_after`.
#[tokio::test(flavor = "multi_thread")]
async fn a_dead_holders_lease_is_taken_over() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = sqlite(&dir);
    // A first process claimed the lease and died: the row is there, and its
    // heartbeat never moves. (One acquire and release creates the table.)
    MigrationLock::acquire_lease(&db, SHORT)
        .await
        .expect("create the lease table")
        .release()
        .await
        .expect("release");
    let mut row = HashMap::new();
    row.insert("id".to_string(), json!("boot"));
    row.insert("owner".to_string(), json!("a-process-that-died"));
    row.insert("heartbeat".to_string(), json!(7));
    db.create(LEASE_TABLE, row)
        .await
        .expect("plant the dead holder's row");
    let dead_owner = owner(&db).await;

    let started = std::time::Instant::now();
    let lock = tokio::time::timeout(
        Duration::from_secs(5),
        MigrationLock::acquire_lease(&sqlite(&dir), SHORT),
    )
    .await
    .expect("the lease is taken over")
    .expect("acquire");
    assert!(
        started.elapsed() >= SHORT.stale_after,
        "not before the holder has stood still for stale_after"
    );
    assert_ne!(owner(&db).await, dead_owner, "the row is the new holder's");
    lock.release().await.expect("release");
}

/// A holder whose row is taken away learns it at its next heartbeat.
#[tokio::test(flavor = "multi_thread")]
async fn a_holder_whose_lease_is_taken_learns_it_is_lost() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = sqlite(&dir);
    let lock = MigrationLock::acquire_lease(&db, SHORT)
        .await
        .expect("take the lease");
    assert!(
        tokio::time::timeout(SHORT.heartbeat * 3, lock.lost())
            .await
            .is_err(),
        "a lease that is held is not lost"
    );

    // Another process's take-over: the row now names someone else.
    let mut data = HashMap::new();
    data.insert("owner".to_string(), json!("someone-else"));
    db.update_where_count(LEASE_TABLE, &[], data)
        .await
        .expect("take the lease over");
    tokio::time::timeout(SHORT.heartbeat * 3, lock.lost())
        .await
        .expect("the holder learns the lease is lost");
}

async fn owner(db: &Arc<dyn DatabaseService>) -> String {
    db.get(LEASE_TABLE, "boot")
        .await
        .expect("the lease row")
        .data
        .get("owner")
        .and_then(serde_json::Value::as_str)
        .expect("an owner")
        .to_string()
}

/// On PostgreSQL the lock is a session-level advisory lock: a second process
/// waits for it, a holder whose session ends learns the lock is lost, and the
/// server hands the lock to the waiter without any take-over.
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn the_postgres_lock_is_held_by_its_session() {
    use impresspress_native::{migration_lock::postgres::APPLICATION_NAME, InfraConfig};

    let url = std::env::var("IMPRESSPRESS_TEST_POSTGRES_URL")
        .expect("IMPRESSPRESS_TEST_POSTGRES_URL must name a PostgreSQL database");
    let infra = || InfraConfig {
        listen: String::new(),
        db_type: "postgres".to_string(),
        db_path: String::new(),
        db_url: Some(url.clone()),
        storage_type: "local".to_string(),
        storage_root: String::new(),
        model_cache_dir: String::new(),
        listener: Default::default(),
    };
    let db = impresspress_native::make_database_service("postgres", "", Some(&url))
        .await
        .expect("connect to PostgreSQL");

    let first = MigrationLock::acquire(&infra(), &db)
        .await
        .expect("the first process takes the lock");
    let (second_infra, second_db) = (infra(), db.clone());
    let second =
        tokio::spawn(async move { MigrationLock::acquire(&second_infra, &second_db).await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!second.is_finished(), "the second process waits");

    // The first process's session ends — a crash, a dropped connection.
    // Test-fixture setup: end the held session from outside. The waiting
    // session carries the same application name, but has not got the lock:
    // only the holder is ended.
    db.query_raw(
        "SELECT pg_terminate_backend(a.pid) FROM pg_stat_activity a \
         JOIN pg_locks l ON l.pid = a.pid \
         WHERE a.application_name = $1 AND l.locktype = 'advisory' AND l.granted",
        &[json!(APPLICATION_NAME)],
    )
    .await
    .expect("end the holder's session");

    tokio::time::timeout(Lease::DEFAULT.heartbeat * 2, first.lost())
        .await
        .expect("the holder learns its session, and the lock, are gone");
    let second = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .expect("the server hands the lock to the waiter")
        .expect("join")
        .expect("acquire");
    second.release().await.expect("release");
}
