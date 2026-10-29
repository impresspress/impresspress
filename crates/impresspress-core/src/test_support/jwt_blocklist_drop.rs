//! The proof behind auth's `018_drop_jwt_blocklist` (the 018 note on
//! `blocks::auth::migrations::JWT_BLOCKLIST_DROPPED`), on both dialects.
//!
//! Driven by `tests/migration_jwt_blocklist_drop.rs` (SQLite) and by
//! `impresspress-native`'s `tests/postgres_migration_history.rs` (the
//! `postgres` lane), over databases the caller provides, empty.
//!
//! - **drops the table and nothing else:** admin's list, then auth's list as
//!   it stood before 018, each through the real runner; one row seeded into
//!   every table but `block_settings` (raw SQL naming only the columns each
//!   table has, [`history::seed`]); then today's auth list. 018 is recorded
//!   `ran` with its content hash, the table and its index are gone, every
//!   other seeded row is where it was with every value it was written with,
//!   the schema is a fresh install's, and auth's record is today's list.
//! - **fails whole without the table:** the same database, the table dropped
//!   by hand before the upgrade. The upgrade fails, not as a refusal, naming
//!   018 and the backend's own "no such table" error; 018 is not recorded and
//!   auth's record still names the list before it.

use std::sync::Arc;

use wafer_core::interfaces::database::service::DatabaseService;
use wafer_sql_utils::Backend;

use super::migration_history::{self as history, NewDatabase, Schema};
use crate::{
    blocks::auth::migrations::{JWT_BLOCKLIST_DROPPED, MIGRATIONS},
    migration_helper::{self, content_hash, migration_set_hash},
    platform_state::{applied_migrations::Origin, block_settings},
};

const BLOCKLIST: &str = "wafer_run__auth__jwt_blocklist";
const BLOCKLIST_INDEX: &str = "wafer_run__auth__jwt_blocklist_expires_at_idx";

/// What the backend says when a statement names a table that does not exist.
fn missing_table_error(backend: Backend) -> String {
    match backend {
        Backend::Sqlite => format!("no such table: {BLOCKLIST}"),
        Backend::Postgres => format!("relation \"{BLOCKLIST}\" does not exist"),
    }
}

/// The schema objects named after the blocklist: the table and its index.
async fn blocklist_objects(
    db: &Arc<dyn DatabaseService>,
    backend: Backend,
) -> Result<Vec<String>, String> {
    // Test-fixture read of the catalogue.
    let (sql, column) = match backend {
        Backend::Sqlite => (
            format!("SELECT name FROM sqlite_master WHERE name LIKE '{BLOCKLIST}%' ORDER BY name"),
            "name",
        ),
        Backend::Postgres => (
            format!(
                "SELECT relname FROM pg_class WHERE relname LIKE '{BLOCKLIST}%' ORDER BY relname"
            ),
            "relname",
        ),
    };
    Ok(db
        .query_raw(&sql, &[])
        .await
        .map_err(|e| format!("`{sql}`: {e}"))?
        .iter()
        .filter_map(|r| {
            r.data
                .get(column)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect())
}

/// Admin's list, then auth's up to (not including) 018, through the real
/// runner: the database 018 meets. Returns that list's hash.
async fn before_018(db: &Arc<dyn DatabaseService>, backend: Backend) -> Result<String, String> {
    history::fresh_admin(db, backend).await?;
    let (files, history_rows) = MIGRATIONS.for_backend(backend);
    let at = files
        .iter()
        .position(|(name, _)| *name == JWT_BLOCKLIST_DROPPED)
        .ok_or_else(|| format!("auth does not ship {JWT_BLOCKLIST_DROPPED}"))?;
    migration_helper::apply_pending_via_service(
        db,
        backend,
        MIGRATIONS.block,
        &files[..at],
        history_rows,
    )
    .await
    .map_err(|e| format!("auth before {JWT_BLOCKLIST_DROPPED}: {e}"))?;
    Ok(migration_set_hash(&files[..at]))
}

/// The two cases of the module docs, each on a database `new_db` hands back
/// empty: what is wrong, if anything.
pub async fn check(backend: Backend, new_db: NewDatabase<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    if let Err(e) = drops_the_table_and_nothing_else(backend, new_db, &mut problems).await {
        problems.push(format!("drop: {e}"));
    }
    if let Err(e) = fails_whole_without_the_table(backend, new_db, &mut problems).await {
        problems.push(format!("missing table: {e}"));
    }
    problems
}

async fn drops_the_table_and_nothing_else(
    backend: Backend,
    new_db: NewDatabase<'_>,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    let fresh = history::fresh_install(&new_db().await, backend, &MIGRATIONS).await?;

    let db = new_db().await;
    before_018(&db, backend).await?;
    let objects = blocklist_objects(&db, backend).await?;
    if objects != [BLOCKLIST, BLOCKLIST_INDEX] {
        return Err(format!(
            "before 018 the database must hold the table and its index, not {objects:?}"
        ));
    }
    let before = Schema::read(&db, backend).await?;
    // `block_settings` is the record the upgrade restamps.
    let seeded = history::seed(&db, backend, &before, &[block_settings::TABLE]).await?;
    if !seeded.iter().any(|row| row.table == BLOCKLIST) {
        return Err("the seed wrote no blocklist row".to_string());
    }

    if let Err(e) = history::upgrade(&db, backend, &MIGRATIONS).await {
        return Err(format!("the upgrade failed: {e}"));
    }

    let objects = blocklist_objects(&db, backend).await?;
    if !objects.is_empty() {
        problems.push(format!("drop: {objects:?} survived the upgrade"));
    }
    let lost = history::lost(&db, &seeded).await?;
    if lost != [BLOCKLIST] {
        problems.push(format!(
            "drop: the seeded rows lost or changed are {lost:?}, not the blocklist's alone"
        ));
    }
    let after = Schema::read(&db, backend).await?;
    let comparison = after.compare(&fresh);
    if !comparison.extra.is_empty() || !comparison.missing.is_empty() {
        problems.push(format!(
            "drop: not a fresh install's schema: extra {:?}, missing {:?}",
            comparison.extra, comparison.missing
        ));
    }

    let (files, _) = MIGRATIONS.for_backend(backend);
    let (_, sql) = files
        .iter()
        .find(|(name, _)| *name == JWT_BLOCKLIST_DROPPED)
        .ok_or("018 is not wired")?;
    let ledger = history::ledger(&db, MIGRATIONS.block).await?;
    match ledger.iter().find(|r| r.file_name == JWT_BLOCKLIST_DROPPED) {
        Some(row) if row.origin == Origin::Ran && row.content_sha256 == content_hash(sql) => {}
        Some(row) => problems.push(format!(
            "drop: 018 is recorded {:?} with {}, not ran with {}",
            row.origin,
            row.content_sha256,
            content_hash(sql)
        )),
        None => problems.push("drop: 018 is not recorded".to_string()),
    }
    if history::current_hash(&db, MIGRATIONS.block).await?
        != history::todays_hash(&MIGRATIONS, backend)
    {
        problems.push("drop: auth's record is not today's list".to_string());
    }
    Ok(())
}

async fn fails_whole_without_the_table(
    backend: Backend,
    new_db: NewDatabase<'_>,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    let db = new_db().await;
    let recorded = before_018(&db, backend).await?;
    // Test-fixture setup: the hand edit 018 must refuse to paper over.
    db.exec_raw(&format!("DROP TABLE {BLOCKLIST}"), &[])
        .await
        .map_err(|e| format!("drop the table by hand: {e}"))?;
    let ledger_before = history::ledger(&db, MIGRATIONS.block).await?;

    match history::upgrade(&db, backend, &MIGRATIONS).await {
        Ok(()) => problems.push(
            "missing table: 018 recorded itself over a database without the blocklist".to_string(),
        ),
        Err(e) => {
            let message = e.to_string();
            if let Some(refusal) = e.refusal() {
                problems.push(format!(
                    "missing table: a failed statement is not a refusal, got {refusal:?}"
                ));
            }
            if !message.contains(&format!("migration `{JWT_BLOCKLIST_DROPPED}`")) {
                problems.push(format!(
                    "missing table: the error does not name 018: {message}"
                ));
            }
            if !message.contains(&missing_table_error(backend)) {
                problems.push(format!(
                    "missing table: the error does not carry {:?}: {message}",
                    missing_table_error(backend)
                ));
            }
        }
    }
    let ledger_after = history::ledger(&db, MIGRATIONS.block).await?;
    if ledger_after
        .iter()
        .any(|r| r.file_name == JWT_BLOCKLIST_DROPPED)
    {
        problems.push("missing table: 018 is recorded".to_string());
    }
    if ledger_after.len() != ledger_before.len() {
        problems.push(format!(
            "missing table: the ledger went from {} rows to {}",
            ledger_before.len(),
            ledger_after.len()
        ));
    }
    if history::current_hash(&db, MIGRATIONS.block).await? != recorded {
        problems.push("missing table: auth's record moved".to_string());
    }
    Ok(())
}
