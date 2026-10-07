//! The migration lock a native boot holds while it applies migrations: a
//! second process waits for it, and a holder that dies frees it — the kernel
//! closes a SQLite holder's lock file, the server ends a PostgreSQL holder's
//! session — with no take-over and no time-out.
//!
//! The SQLite tests run everywhere. The one for a killed holder kills a real
//! process: it runs this test binary again as the holder
//! ([`hold_the_lock_until_killed`], which does nothing unless the parent
//! names a lock file for it). The PostgreSQL test needs a server, so it is
//! `#[ignore]`d; CI's `test-postgres` job runs it with
//! `IMPRESSPRESS_TEST_POSTGRES_URL` naming a database.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use impresspress_native::migration_lock::{sqlite_lock_path, MigrationLock};

/// The env var naming the lock file the child holder takes.
const HOLD_VAR: &str = "IMPRESSPRESS_TEST_HOLD_MIGRATION_LOCK";

fn lock_path(dir: &tempfile::TempDir) -> PathBuf {
    sqlite_lock_path(
        dir.path()
            .join("lock.sqlite3")
            .to_str()
            .expect("utf-8 path"),
    )
}

/// Whether some process holds the lock on `path` right now.
fn is_held(path: &Path) -> bool {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .expect("open the lock file");
    match file.try_lock() {
        Ok(()) => false,
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(e)) => panic!("probe the lock: {e}"),
    }
}

/// The holder process, killed when dropped — so a failing assertion never
/// leaves it running and holding the lock.
struct Holder(std::process::Child);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A second process waits while the first holds the lock, and gets it once
/// the first releases it.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_process_waits_until_the_first_releases() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = lock_path(&dir);
    let first = MigrationLock::acquire_file(path.clone())
        .await
        .expect("the first process takes the lock");
    let second_path = path.clone();
    let second = tokio::spawn(async move { MigrationLock::acquire_file(second_path).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!second.is_finished(), "the second process waits");
    first.release().await.expect("release");
    let second = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .expect("the second process gets the lock once it is released")
        .expect("join")
        .expect("acquire");
    second.release().await.expect("release");
}

/// A holder that is killed mid-boot frees the lock: the kernel closes its
/// file, and the waiter gets it at once.
#[tokio::test(flavor = "multi_thread")]
async fn a_killed_holder_frees_the_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = lock_path(&dir);
    let mut holder = Holder(
        std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "hold_the_lock_until_killed",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env(HOLD_VAR, &path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start the holder process"),
    );
    let mut waited = Duration::ZERO;
    while !is_held(&path) {
        assert!(
            waited < Duration::from_secs(10),
            "the holder never took the lock"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        waited += Duration::from_millis(50);
    }

    let waiter_path = path.clone();
    let waiter = tokio::spawn(async move { MigrationLock::acquire_file(waiter_path).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !waiter.is_finished(),
        "the waiter waits while the holder lives"
    );

    holder.0.kill().expect("kill the holder");
    holder.0.wait().expect("reap the holder");
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("a killed holder frees the lock")
        .expect("join")
        .expect("acquire")
        .release()
        .await
        .expect("release");
}

/// The holder [`a_killed_holder_frees_the_lock`] runs as a process of its
/// own: takes the lock on the file it is given and holds it until killed.
/// Does nothing when run any other way.
#[tokio::test]
#[ignore = "a helper process, run by a_killed_holder_frees_the_lock"]
async fn hold_the_lock_until_killed() {
    let Some(path) = std::env::var_os(HOLD_VAR) else {
        return;
    };
    let _held = MigrationLock::acquire_file(PathBuf::from(path))
        .await
        .expect("take the lock");
    std::future::pending::<()>().await;
}

/// On PostgreSQL the lock is a session-level advisory lock: a second process
/// waits for it, a holder whose session ends learns the lock is lost, and the
/// server hands the lock to the waiter without any take-over.
#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn the_postgres_lock_is_held_by_its_session() {
    use impresspress_native::{
        migration_lock::{postgres::APPLICATION_NAME, HEARTBEAT},
        InfraConfig,
    };

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

    let first = MigrationLock::acquire(&infra())
        .await
        .expect("the first process takes the lock");
    let second_infra = infra();
    let second = tokio::spawn(async move { MigrationLock::acquire(&second_infra).await });
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
        &[serde_json::json!(APPLICATION_NAME)],
    )
    .await
    .expect("end the holder's session");

    tokio::time::timeout(HEARTBEAT * 2, first.lost())
        .await
        .expect("the holder learns its session, and the lock, are gone");
    let second = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .expect("the server hands the lock to the waiter")
        .expect("join")
        .expect("acquire");
    second.release().await.expect("release");
}
