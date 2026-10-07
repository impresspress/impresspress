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
//! How the lock is held depends on the database:
//!
//! - **PostgreSQL**: a session-level `pg_advisory_lock` on a connection of
//!   the lock's own. The server releases it when that session ends, however
//!   it ends, so a crashed holder needs no take-over. The holder pings the
//!   connection every [`Lease::heartbeat`]; a failed ping means the session —
//!   and with it the lock — may be gone.
//! - **SQLite**: there is no session to tie a lock to, so it is a lease: one
//!   row in [`LEASE_TABLE`], claimed by inserting it under a fixed id (a
//!   second insert of the key is refused) and released by deleting it. The
//!   holder bumps the row's `heartbeat` every [`Lease::heartbeat`]; a waiter
//!   that sees the same value for [`Lease::stale_after`] by its own clock
//!   takes the row over. Watching the value, rather than comparing a stored
//!   timestamp with the waiter's clock, keeps clocks out of it.
//!
//! Either way a holder that can no longer show it holds the lock reports it
//! through [`MigrationLock::lost`], and the boot that holds it stops: past
//! that point another process may be applying the same lists.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::sync::watch;
use wafer_block::db::{Filter, FilterOp};
use wafer_core::interfaces::database::{
    mint_record_id,
    service::{col_int64, col_string, pk, timestamps, DatabaseError, DatabaseService, Table},
};

use crate::InfraConfig;

/// The SQLite lease's table. Owned by the native server, not by a block.
pub const LEASE_TABLE: &str = "impresspress__native__migration_lock";

/// The one lease row every booting process competes for.
const LEASE_ROW: &str = "boot";

/// The timing of a held lock: how often its holder shows it still holds it,
/// and — for the SQLite lease — how long a waiter watches a holder that has
/// stopped before taking the lock over, and how often it re-tries the claim.
#[derive(Debug, Clone, Copy)]
pub struct Lease {
    /// How often the holder bumps the lease row / pings its PostgreSQL
    /// session.
    pub heartbeat: Duration,
    /// How long an unchanged heartbeat is watched before the lease is taken
    /// over, and how long a holder goes without a successful heartbeat
    /// before it counts the lock as lost.
    pub stale_after: Duration,
    /// How often a waiter re-tries the SQLite claim.
    pub poll: Duration,
}

impl Lease {
    /// What a boot uses. Six missed heartbeats before a take-over: long
    /// enough that a holder busy in a slow statement still beats in time
    /// (the heartbeat runs on its own task), short enough that a crashed
    /// boot delays the next one by half a minute.
    pub const DEFAULT: Lease = Lease {
        heartbeat: Duration::from_secs(5),
        stale_after: Duration::from_secs(30),
        poll: Duration::from_millis(250),
    };
}

/// A held migration lock. Release it with [`MigrationLock::release`] once the
/// boot's migrations have run, and stop the boot if [`MigrationLock::lost`]
/// resolves first.
pub struct MigrationLock {
    held: Held,
    heartbeat: tokio::task::JoinHandle<()>,
    lost: watch::Receiver<bool>,
}

enum Held {
    Lease {
        db: Arc<dyn DatabaseService>,
        owner: String,
    },
    #[cfg(feature = "postgres")]
    Postgres(Arc<tokio::sync::Mutex<postgres::Session>>),
}

impl MigrationLock {
    /// Wait until this process holds the lock on the migrations of the
    /// database `infra` names, which `db` is the platform service for.
    pub async fn acquire(infra: &InfraConfig, db: &Arc<dyn DatabaseService>) -> Result<Self> {
        match infra.db_type.as_str() {
            "postgres" => acquire_postgres(infra.db_url.as_deref(), Lease::DEFAULT).await,
            _ => Self::acquire_lease(db, Lease::DEFAULT).await,
        }
    }

    /// Wait until this process holds the SQLite lease on `db`, with `lease`'s
    /// timing.
    pub async fn acquire_lease(db: &Arc<dyn DatabaseService>, lease: Lease) -> Result<Self> {
        create_lease_table(db).await?;
        let owner = mint_record_id();
        // The holder and heartbeat last seen, and when they were first seen.
        let mut watched: Option<(String, i64, Instant)> = None;
        let mut announced = false;
        loop {
            match db.create(LEASE_TABLE, claim(&owner)).await {
                Ok(_) => break,
                Err(DatabaseError::AlreadyExists(_)) => {}
                Err(e) => return Err(anyhow!(e)).context("claim the migration lock"),
            }
            let (holder, beat) = match db.get(LEASE_TABLE, LEASE_ROW).await {
                Ok(record) => (
                    record
                        .data
                        .get("owner")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    record
                        .data
                        .get("heartbeat")
                        .and_then(Value::as_i64)
                        .unwrap_or_default(),
                ),
                // Released between the claim and the read: claim again.
                Err(DatabaseError::NotFound) => continue,
                Err(e) => return Err(anyhow!(e)).context("read the migration lock"),
            };
            if !announced {
                tracing::info!(
                    holder = %holder,
                    "another process is applying migrations to this database; waiting for it"
                );
                announced = true;
            }
            match &watched {
                Some((seen_holder, seen_beat, since))
                    if *seen_holder == holder && *seen_beat == beat =>
                {
                    if since.elapsed() >= lease.stale_after {
                        tracing::warn!(
                            holder = %holder,
                            "the process holding the migration lock stopped its heartbeat; \
                             taking the lock over"
                        );
                        // Only the row as it was watched: a holder that beat
                        // in the meantime keeps its lock.
                        db.delete_where_count(LEASE_TABLE, &held_by(&holder, Some(beat)))
                            .await
                            .context("take over a stale migration lock")?;
                        watched = None;
                        continue;
                    }
                }
                _ => watched = Some((holder, beat, Instant::now())),
            }
            tokio::time::sleep(lease.poll).await;
        }
        tracing::debug!(owner = %owner, "migration lock acquired");
        let (lost_tx, lost) = watch::channel(false);
        let heartbeat = tokio::spawn(beat_lease(db.clone(), owner.clone(), lease, lost_tx));
        Ok(Self {
            held: Held::Lease {
                db: db.clone(),
                owner,
            },
            heartbeat,
            lost,
        })
    }

    /// Resolves once this process can no longer show it holds the lock: its
    /// lease row is gone or has another holder, its heartbeat has failed for
    /// [`Lease::stale_after`], or its PostgreSQL session stopped answering.
    /// Never resolves while the lock is held.
    pub async fn lost(&self) {
        let mut lost = self.lost.clone();
        if lost.wait_for(|lost| *lost).await.is_err() {
            // The heartbeat task ended without reporting a loss: only
            // `release` ends it that way, and it consumes the lock.
            std::future::pending::<()>().await;
        }
    }

    /// Give the lock up.
    pub async fn release(self) -> Result<()> {
        self.heartbeat.abort();
        match self.held {
            Held::Lease { db, owner } => {
                db.delete_where_count(LEASE_TABLE, &held_by(&owner, None))
                    .await
                    .context("release the migration lock")?;
            }
            #[cfg(feature = "postgres")]
            Held::Postgres(session) => session.lock().await.release().await?,
        }
        Ok(())
    }
}

/// Bump the held row's `heartbeat` until the task is aborted, and report the
/// lock lost when the row is no longer this process's, or when no bump has
/// succeeded for `lease.stale_after` (a waiter may have taken it over).
async fn beat_lease(
    db: Arc<dyn DatabaseService>,
    owner: String,
    lease: Lease,
    lost: watch::Sender<bool>,
) {
    let mut beat: i64 = 0;
    let mut last_success = Instant::now();
    loop {
        tokio::time::sleep(lease.heartbeat).await;
        beat += 1;
        let mut data = HashMap::new();
        data.insert("heartbeat".to_string(), json!(beat));
        match db
            .update_where_count(LEASE_TABLE, &held_by(&owner, None), data)
            .await
        {
            Ok(0) => {
                tracing::error!("the migration lock was taken over while this process held it");
                lost.send_replace(true);
                return;
            }
            Ok(_) => last_success = Instant::now(),
            Err(e) => {
                tracing::warn!(error = %e, "migration lock heartbeat failed");
                if last_success.elapsed() >= lease.stale_after {
                    tracing::error!(
                        "the migration lock's heartbeat has failed for longer than a waiter \
                         waits before taking it over"
                    );
                    lost.send_replace(true);
                    return;
                }
            }
        }
    }
}

/// Create the lease table unless it exists.
///
/// Created here, through `ensure_schema_table`, rather than by a migration:
/// the lock has to exist before the first migration runs.
async fn create_lease_table(db: &Arc<dyn DatabaseService>) -> Result<()> {
    db.ensure_schema_table(&lease_table())
        .await
        .map_err(|e| anyhow!(e))
        .with_context(|| format!("create the migration lock table `{LEASE_TABLE}`"))
}

/// The lease table: the row's fixed id, its holder and its heartbeat.
fn lease_table() -> Table {
    let mut table = Table::new(LEASE_TABLE);
    table.columns = vec![
        pk("id"),
        col_string("owner").not_null(),
        col_int64("heartbeat").not_null(),
    ];
    table.columns.extend(timestamps());
    table
}

/// The row `owner` inserts to claim the lease.
fn claim(owner: &str) -> HashMap<String, Value> {
    let mut data = HashMap::new();
    data.insert("id".to_string(), json!(LEASE_ROW));
    data.insert("owner".to_string(), json!(owner));
    data.insert("heartbeat".to_string(), json!(0));
    data
}

/// The lease row while `owner` holds it (and, given one, at that heartbeat).
fn held_by(owner: &str, heartbeat: Option<i64>) -> Vec<Filter> {
    let mut filters = vec![
        Filter {
            field: "id".into(),
            operator: FilterOp::Equal,
            value: json!(LEASE_ROW),
        },
        Filter {
            field: "owner".into(),
            operator: FilterOp::Equal,
            value: json!(owner),
        },
    ];
    if let Some(beat) = heartbeat {
        filters.push(Filter {
            field: "heartbeat".into(),
            operator: FilterOp::Equal,
            value: json!(beat),
        });
    }
    filters
}

#[cfg(feature = "postgres")]
async fn acquire_postgres(url: Option<&str>, lease: Lease) -> Result<MigrationLock> {
    let url = url.ok_or_else(|| {
        anyhow!("IMPRESSPRESS_DB_TYPE=postgres requires IMPRESSPRESS_DB_URL to be set")
    })?;
    let session = Arc::new(tokio::sync::Mutex::new(postgres::Session::lock(url).await?));
    let (lost_tx, lost) = watch::channel(false);
    let heartbeat = tokio::spawn(postgres::beat(session.clone(), lease, lost_tx));
    Ok(MigrationLock {
        held: Held::Postgres(session),
        heartbeat,
        lost,
    })
}

#[cfg(not(feature = "postgres"))]
async fn acquire_postgres(_url: Option<&str>, _lease: Lease) -> Result<MigrationLock> {
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

    use super::Lease;

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
    pub(super) async fn beat(
        session: Arc<Mutex<Session>>,
        lease: Lease,
        lost: watch::Sender<bool>,
    ) {
        loop {
            tokio::time::sleep(lease.heartbeat).await;
            if let Err(e) = session.lock().await.conn.ping().await {
                tracing::error!(error = %e, "the migration lock's PostgreSQL session failed");
                lost.send_replace(true);
                return;
            }
        }
    }
}
