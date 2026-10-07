//! The database-level lock a native boot holds while it applies migrations.
//!
//! A native boot applies every pending block migration before it serves.
//! Two processes booting against one database at once — two replicas started
//! together, or a restart that overlaps the process it replaces — would each
//! read the same pending lists and both apply them: every statement twice,
//! racing each other, and a list such as auth's, which drops and re-creates
//! tables, re-run under a process that may already be serving.
//! [`MigrationLock::acquire`] makes them take turns: the second process waits
//! until the first has finished its boot, and only then reads which lists are
//! applied.
//!
//! Both locks are held by something the operating system or the database
//! server releases when the holder dies, so there is no lease to time out,
//! no take-over, and no window in which two processes both believe they hold
//! it:
//!
//! - **SQLite**: an exclusive `flock` on [`sqlite_lock_path`], a file next to
//!   the database. The kernel releases it when the file is closed — at the
//!   latest when the process exits, however it exits. Nothing can take it
//!   from a live holder.
//! - **PostgreSQL**: a session-level `pg_advisory_lock` on a connection of
//!   the lock's own. The server releases it when that session ends. The
//!   session can end under a live holder (a dropped connection, an operator
//!   terminating it), so the holder pings it every [`HEARTBEAT`] and reports
//!   a failed ping through [`MigrationLock::lost`]; the boot that holds the
//!   lock then stops, because another process may already hold it.

use std::{fs::File, path::PathBuf, time::Duration};

use anyhow::{anyhow, Context, Result};
use tokio::sync::watch;

use crate::InfraConfig;

/// How often the PostgreSQL holder pings its session.
pub const HEARTBEAT: Duration = Duration::from_secs(5);

/// The SQLite lock file for the database at `db_path`: `<db_path>.migrate.lock`.
pub fn sqlite_lock_path(db_path: &str) -> PathBuf {
    PathBuf::from(format!("{db_path}.migrate.lock"))
}

/// A held migration lock. Release it with [`MigrationLock::release`] once the
/// boot's migrations have run, and stop the boot if [`MigrationLock::lost`]
/// resolves first.
pub struct MigrationLock {
    held: Held,
    lost: watch::Receiver<bool>,
}

enum Held {
    /// The locked file; closing it releases the lock.
    File(File),
    #[cfg(feature = "postgres")]
    Postgres {
        session: std::sync::Arc<tokio::sync::Mutex<postgres::Session>>,
        heartbeat: tokio::task::JoinHandle<()>,
    },
}

impl MigrationLock {
    /// Wait until this process holds the lock on the migrations of the
    /// database `infra` names.
    pub async fn acquire(infra: &InfraConfig) -> Result<Self> {
        match infra.db_type.as_str() {
            "postgres" => acquire_postgres(infra.db_url.as_deref()).await,
            _ => Self::acquire_file(sqlite_lock_path(&infra.db_path)).await,
        }
    }

    /// Wait until this process holds an exclusive lock on the file at `path`,
    /// creating it if it does not exist.
    pub async fn acquire_file(path: PathBuf) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("open the migration lock file {}", path.display()))?;
        let file = match file.try_lock() {
            Ok(()) => file,
            Err(std::fs::TryLockError::WouldBlock) => {
                tracing::info!(
                    lock = %path.display(),
                    "another process is applying migrations to this database; waiting for it"
                );
                // `File::lock` blocks the thread until the holder lets go, so
                // it waits on the blocking pool, not on a runtime worker.
                tokio::task::spawn_blocking(move || file.lock().map(|()| file))
                    .await
                    .context("wait for the migration lock")?
                    .with_context(|| format!("lock {}", path.display()))?
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("lock {}", path.display()));
            }
        };
        tracing::debug!(lock = %path.display(), "migration lock acquired");
        // No sender: a file lock is never lost, so `lost` stays pending.
        let (_, lost) = watch::channel(false);
        Ok(Self {
            held: Held::File(file),
            lost,
        })
    }

    /// Resolves once this process can no longer show it holds the lock — its
    /// PostgreSQL session stopped answering. Never resolves for a file lock,
    /// or while the lock is held.
    pub async fn lost(&self) {
        let mut lost = self.lost.clone();
        if lost.wait_for(|lost| *lost).await.is_err() {
            // No sender left without a loss reported: a file lock (which has
            // none), or a heartbeat task `release` ended.
            std::future::pending::<()>().await;
        }
    }

    /// Give the lock up.
    pub async fn release(self) -> Result<()> {
        match self.held {
            Held::File(file) => {
                file.unlock().context("release the migration lock")?;
            }
            #[cfg(feature = "postgres")]
            Held::Postgres { session, heartbeat } => {
                heartbeat.abort();
                session.lock().await.release().await?;
            }
        }
        Ok(())
    }
}

#[cfg(feature = "postgres")]
async fn acquire_postgres(url: Option<&str>) -> Result<MigrationLock> {
    let url = url.ok_or_else(|| {
        anyhow!("IMPRESSPRESS_DB_TYPE=postgres requires IMPRESSPRESS_DB_URL to be set")
    })?;
    let session = std::sync::Arc::new(tokio::sync::Mutex::new(postgres::Session::lock(url).await?));
    let (lost_tx, lost) = watch::channel(false);
    let heartbeat = tokio::spawn(postgres::beat(session.clone(), lost_tx));
    Ok(MigrationLock {
        held: Held::Postgres { session, heartbeat },
        lost,
    })
}

#[cfg(not(feature = "postgres"))]
async fn acquire_postgres(_url: Option<&str>) -> Result<MigrationLock> {
    Err(anyhow!(
        "IMPRESSPRESS_DB_TYPE=postgres but this binary was built without the `postgres` feature"
    ))
}

/// The PostgreSQL lock: a session-level advisory lock on a connection of its
/// own.
#[cfg(feature = "postgres")]
pub mod postgres {
    use std::{str::FromStr, sync::Arc};

    use anyhow::{Context, Result};
    use sqlx::{
        postgres::{PgConnectOptions, PgConnection},
        ConnectOptions, Connection,
    };
    use tokio::sync::{watch, Mutex};

    use super::HEARTBEAT;

    /// The advisory-lock key every impresspress boot locks: the bytes of
    /// `"impressm"` (impresspress migrations) as a big-endian `i64`. Any key
    /// works as long as nothing else on the database locks the same one.
    pub const ADVISORY_KEY: i64 = i64::from_be_bytes(*b"impressm");

    /// The `application_name` of the lock's session, so an operator can find
    /// it in `pg_stat_activity` (and a test can end it).
    pub const APPLICATION_NAME: &str = "impresspress-migration-lock";

    /// The lock's own session, holding [`ADVISORY_KEY`].
    pub struct Session {
        conn: PgConnection,
    }

    impl Session {
        /// Connect and wait until the session holds the lock.
        pub(super) async fn lock(url: &str) -> Result<Self> {
            let mut conn = PgConnectOptions::from_str(url)
                .context("parse IMPRESSPRESS_DB_URL for the migration lock")?
                .application_name(APPLICATION_NAME)
                .connect()
                .await
                .context("open the migration lock's PostgreSQL session")?;
            let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                .bind(ADVISORY_KEY)
                .fetch_one(&mut conn)
                .await
                .context("try the migration lock")?;
            if !held {
                tracing::info!(
                    "another process is applying migrations to this database; waiting for it"
                );
                sqlx::query("SELECT pg_advisory_lock($1)")
                    .bind(ADVISORY_KEY)
                    .execute(&mut conn)
                    .await
                    .context("wait for the migration lock")?;
            }
            tracing::debug!("migration lock acquired");
            Ok(Self { conn })
        }

        /// Unlock and close the session.
        pub(super) async fn release(&mut self) -> Result<()> {
            sqlx::query("SELECT pg_advisory_unlock($1)")
                .bind(ADVISORY_KEY)
                .execute(&mut self.conn)
                .await
                .context("release the migration lock")?;
            Ok(())
        }
    }

    /// Ping the session until the task is aborted; report the lock lost on
    /// the first failed ping — the session, and the lock it holds, may be
    /// gone, and another process may already hold it.
    pub(super) async fn beat(session: Arc<Mutex<Session>>, lost: watch::Sender<bool>) {
        loop {
            tokio::time::sleep(HEARTBEAT).await;
            if let Err(e) = session.lock().await.conn.ping().await {
                tracing::error!(error = %e, "the migration lock's PostgreSQL session failed");
                lost.send_replace(true);
                return;
            }
        }
    }
}
