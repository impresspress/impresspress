//! The database-level lock a native boot holds while it applies migrations.
//!
//! A native boot applies every pending block migration before it serves (see
//! `impresspress_core::migration_helper`). Two processes booting against one
//! database at once — two replicas started together, or a restart that
//! overlaps the process it replaces — would each read the same pending lists
//! from their own block-settings snapshot and both apply them: every
//! statement twice, racing each other, and a list such as auth's, which drops
//! and re-creates tables, re-run under a process that may already be serving.
//! [`MigrationLock::acquire`] makes them take turns. The second process waits
//! until the first has finished its whole boot and only then reads the block
//! settings, so it finds the lists applied and applies nothing.
//!
//! The lock is one row in [`TABLE`], claimed by inserting it under a fixed
//! id — the database refuses a second insert of the same key — and released
//! by deleting it. While a process holds the row it bumps the row's
//! `heartbeat` every [`HEARTBEAT_EVERY`]. A waiter that sees the same
//! `heartbeat` value for [`STALE_AFTER`] by its own clock concludes the holder
//! died mid-boot and takes the row over. Watching the value, rather than
//! comparing a stored timestamp with the waiter's clock, keeps the clocks of
//! two hosts sharing one PostgreSQL database out of it.
//!
//! The table is created here, through `ensure_schema_table`, rather than by a
//! migration: the lock has to exist before the first migration runs.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context};
use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp};
use wafer_core::interfaces::database::{
    mint_record_id,
    service::{col_int64, col_string, pk, timestamps, DatabaseError, DatabaseService, Table},
};

/// The lock's table. Owned by the native server, not by a block.
pub const TABLE: &str = "impresspress__server__migration_lock";

/// The one row every booting process competes for.
const LOCK_ROW: &str = "boot";

/// How often the holder bumps its `heartbeat`.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(5);

/// How long a waiter watches an unchanged `heartbeat` before it takes the
/// lock over. Six missed heartbeats: long enough that a holder busy in a slow
/// statement still beats in time (the heartbeat runs on its own task), short
/// enough that a crashed boot delays the next one by half a minute.
const STALE_AFTER: Duration = Duration::from_secs(30);

/// How often a waiter re-tries the claim.
const POLL_EVERY: Duration = Duration::from_millis(250);

/// A held migration lock. Release it with [`MigrationLock::release`] once the
/// boot's migrations have run; a process that dies holding it is taken over
/// after [`STALE_AFTER`].
pub struct MigrationLock {
    db: Arc<dyn DatabaseService>,
    owner: String,
    heartbeat: tokio::task::JoinHandle<()>,
}

impl MigrationLock {
    /// Wait until this process holds the lock on `db`'s migrations.
    pub async fn acquire(db: &Arc<dyn DatabaseService>) -> anyhow::Result<Self> {
        create_table(db).await?;
        let owner = mint_record_id();
        // The holder and heartbeat last seen, and when they were first seen.
        let mut watched: Option<(String, i64, Instant)> = None;
        let mut announced = false;
        loop {
            match db.create(TABLE, claim(&owner)).await {
                Ok(_) => break,
                Err(DatabaseError::AlreadyExists(_)) => {}
                Err(e) => return Err(anyhow!(e)).context("claim the migration lock"),
            }
            let (holder, beat) = match db.get(TABLE, LOCK_ROW).await {
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
                    if since.elapsed() >= STALE_AFTER {
                        tracing::warn!(
                            holder = %holder,
                            "the process holding the migration lock stopped its heartbeat; \
                             taking the lock over"
                        );
                        // Only the row as it was watched: a holder that beat
                        // in the meantime keeps its lock.
                        db.delete_where_count(TABLE, &held_by(&holder, Some(beat)))
                            .await
                            .context("take over a stale migration lock")?;
                        watched = None;
                        continue;
                    }
                }
                _ => watched = Some((holder, beat, Instant::now())),
            }
            tokio::time::sleep(POLL_EVERY).await;
        }
        tracing::debug!(owner = %owner, "migration lock acquired");
        let heartbeat = tokio::spawn(beat(db.clone(), owner.clone()));
        Ok(Self {
            db: db.clone(),
            owner,
            heartbeat,
        })
    }

    /// Give the lock up.
    pub async fn release(self) -> anyhow::Result<()> {
        self.heartbeat.abort();
        self.db
            .delete_where_count(TABLE, &held_by(&self.owner, None))
            .await
            .context("release the migration lock")?;
        Ok(())
    }
}

/// Create the lock table unless it exists.
///
/// Two processes starting together race here too, and on PostgreSQL the
/// loser of two concurrent `CREATE TABLE IF NOT EXISTS` is refused (a
/// duplicate `pg_type` row) rather than told the table exists. A refusal
/// after which the table is there is that race lost, not a failure.
async fn create_table(db: &Arc<dyn DatabaseService>) -> anyhow::Result<()> {
    let Err(error) = db.ensure_schema_table(&table()).await else {
        return Ok(());
    };
    if let Ok(true) = db.schema_table_exists(TABLE).await {
        return Ok(());
    }
    Err(anyhow!(error)).with_context(|| format!("create the migration lock table `{TABLE}`"))
}

/// Bump the held row's `heartbeat` until the task is aborted, or until the
/// row is no longer this process's.
async fn beat(db: Arc<dyn DatabaseService>, owner: String) {
    let mut beat: i64 = 0;
    loop {
        tokio::time::sleep(HEARTBEAT_EVERY).await;
        beat += 1;
        let mut data = HashMap::new();
        data.insert("heartbeat".to_string(), json!(beat));
        match db
            .update_where_count(TABLE, &held_by(&owner, None), data)
            .await
        {
            Ok(0) => {
                tracing::error!(
                    "the migration lock was taken over while this process held it; another \
                     process may be applying the same migrations"
                );
                return;
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "migration lock heartbeat failed"),
        }
    }
}

/// The lock table: the row's fixed id, its holder and its heartbeat.
fn table() -> Table {
    let mut table = Table::new(TABLE);
    table.columns = vec![
        pk("id"),
        col_string("owner").not_null(),
        col_int64("heartbeat").not_null(),
    ];
    table.columns.extend(timestamps());
    table
}

/// The row `owner` inserts to claim the lock.
fn claim(owner: &str) -> HashMap<String, Value> {
    let mut data = HashMap::new();
    data.insert("id".to_string(), json!(LOCK_ROW));
    data.insert("owner".to_string(), json!(owner));
    data.insert("heartbeat".to_string(), json!(0));
    data
}

/// The lock row while `owner` holds it (and, given one, at that heartbeat).
fn held_by(owner: &str, heartbeat: Option<i64>) -> Vec<Filter> {
    let mut filters = vec![
        Filter {
            field: "id".into(),
            operator: FilterOp::Equal,
            value: json!(LOCK_ROW),
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
