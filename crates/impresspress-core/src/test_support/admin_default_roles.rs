//! The proof behind admin's `010_default_roles`, on both dialects.
//!
//! Driven by `tests/migration_admin_default_roles.rs` (SQLite) and by
//! `impresspress-native`'s `tests/postgres_migration_history.rs` (the
//! `postgres` lane), over databases the caller provides, empty. Every
//! database goes through the real runner (`apply_pending_via_service`, the
//! entry point the native server and the browser use pre-build).
//!
//! - **a fresh install** holds the two system roles `Init` used to seed:
//!   `admin` and `user`, with the same descriptions, `permissions` `[]` and
//!   `is_system` 1; both timestamps are one stamp in the executor's `+00:00`
//!   form, which sorts against the executor's own stamps as the instants do;
//!   010 is recorded `ran` with its content hash, and re-running the upgrade
//!   changes nothing.
//! - **a database that already has the roles** (as the `Init` seed wrote
//!   them, executor ids and stamps, and an admin's role beside them) keeps
//!   exactly the rows it had: nothing is inserted, so nothing is duplicated.
//! - **the guard is the whole table**, as the seed's count was: a table
//!   holding only `admin` (a seed that wrote one role of two), or only a
//!   role an admin defined, gets nothing.
//! - **an empty table** is seeded by the upgrade that runs 010, as the next
//!   boot's `Init` seeded it before. Once 010 is recorded, emptying the table
//!   is final: re-running the upgrade, and re-planning against the ledger
//!   after admin's `current_hash` is lost, inserts nothing.

use std::sync::Arc;

use serde_json::{json, Value};
use wafer_block::db::{ListOptions, SortField};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_sql_utils::Backend;

use super::migration_history::{self as history, NewDatabase};
use crate::{
    blocks::admin::{
        migrations::{DEFAULT_ROLES, MIGRATIONS},
        ROLES_TABLE,
    },
    migration_helper::{self, content_hash},
    platform_state::applied_migrations::Origin,
};

/// The rows 010 writes, as `(id, name, description)`, by id; every one has
/// `permissions` `[]` and `is_system` 1. The descriptions are the ones `Init`
/// seeded.
const DEFAULTS: [(&str, &str, &str); 2] = [
    ("seed-admin", "admin", "Full access to all resources"),
    ("seed-user", "user", "Standard user access"),
];

/// The columns a role row has, in the order rows are compared.
const COLUMNS: [&str; 7] = [
    "id",
    "name",
    "description",
    "permissions",
    "is_system",
    "created_at",
    "updated_at",
];

/// Every role row, ordered by id, each as its column values.
async fn rows(db: &Arc<dyn DatabaseService>) -> Result<Vec<Vec<Value>>, String> {
    let list = db
        .list(
            ROLES_TABLE,
            &ListOptions {
                sort: vec![SortField {
                    field: "id".to_string(),
                    desc: false,
                }],
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("list roles: {e}"))?;
    Ok(list
        .records
        .iter()
        .map(|record| {
            COLUMNS
                .iter()
                .map(|column| record.data.get(*column).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect())
}

/// Admin's list up to (not including) 010, through the real runner: the
/// database a release before this one left.
async fn before_010(db: &Arc<dyn DatabaseService>, backend: Backend) -> Result<(), String> {
    let (files, history_rows) = MIGRATIONS.for_backend(backend);
    let at = files
        .iter()
        .position(|(name, _)| *name == DEFAULT_ROLES)
        .ok_or_else(|| format!("admin does not ship {DEFAULT_ROLES}"))?;
    migration_helper::apply_pending_via_service(
        db,
        backend,
        MIGRATIONS.block,
        &files[..at],
        history_rows,
    )
    .await
    .map_err(|e| format!("admin before {DEFAULT_ROLES}: {e}"))
}

async fn upgrade(db: &Arc<dyn DatabaseService>, backend: Backend) -> Result<(), String> {
    history::upgrade(db, backend, &MIGRATIONS)
        .await
        .map_err(|e| format!("the upgrade failed: {e}"))
}

/// A role written through the executor, as the `Init` seed wrote one: the
/// executor mints the id and stamps `updated_at`; `created_at` is the
/// caller's `to_rfc3339`.
async fn write_role(
    db: &Arc<dyn DatabaseService>,
    name: &str,
    description: &str,
    is_system: bool,
) -> Result<Vec<Value>, String> {
    let data = crate::util::json_map(json!({
        "name": name,
        "description": description,
        "is_system": is_system,
        "created_at": crate::util::now_rfc3339(),
        "permissions": [],
    }));
    let record = db
        .create(ROLES_TABLE, data)
        .await
        .map_err(|e| format!("write the {name} role: {e}"))?;
    Ok(COLUMNS
        .iter()
        .map(|column| record.data.get(*column).cloned().unwrap_or(Value::Null))
        .collect())
}

/// What is wrong with the roles 010 wrote, if anything: exactly the two
/// system roles, each with one `+00:00` stamp in both timestamp columns.
fn check_defaults(rows: &[Vec<Value>], what: &str, problems: &mut Vec<String>) {
    let expected: Vec<Vec<Value>> = DEFAULTS
        .iter()
        .map(|(id, name, description)| {
            vec![
                json!(id),
                json!(name),
                json!(description),
                json!("[]"),
                json!(1),
            ]
        })
        .collect();
    let written: Vec<Vec<Value>> = rows.iter().map(|row| row[..5].to_vec()).collect();
    if written != expected {
        problems.push(format!(
            "{what}: the roles are {written:?}, not the two system roles {expected:?}"
        ));
        return;
    }
    for row in rows {
        let stamps = &row[5..];
        let first = stamps[0].as_str().unwrap_or_default();
        if stamps.iter().any(|stamp| stamp.as_str() != Some(first)) {
            problems.push(format!(
                "{what}: {} has two different timestamps {stamps:?}",
                row[0]
            ));
        }
        if !is_whole_second_utc(first) {
            problems.push(format!(
                "{what}: {} is stamped {first:?}, not `YYYY-MM-DDTHH:MM:SS+00:00`",
                row[0]
            ));
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SS+00:00`: the executor's offset, whole seconds.
fn is_whole_second_utc(stamp: &str) -> bool {
    let b = stamp.as_bytes();
    let digits = |range: std::ops::Range<usize>| range.into_iter().all(|i| b[i].is_ascii_digit());
    b.len() == 25
        && digits(0..4)
        && b[4] == b'-'
        && digits(5..7)
        && b[7] == b'-'
        && digits(8..10)
        && b[10] == b'T'
        && digits(11..13)
        && b[13] == b':'
        && digits(14..16)
        && b[16] == b':'
        && digits(17..19)
        && &stamp[19..] == "+00:00"
        && chrono::DateTime::parse_from_rfc3339(stamp).is_ok()
}

/// The cases of the module docs, each on a database `new_db` hands back
/// empty: what is wrong, if anything.
pub async fn check(backend: Backend, new_db: NewDatabase<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    if let Err(e) = a_fresh_install_has_the_system_roles(backend, new_db, &mut problems).await {
        problems.push(format!("fresh install: {e}"));
    }
    if let Err(e) = existing_roles_are_kept_as_they_are(backend, new_db, &mut problems).await {
        problems.push(format!("existing roles: {e}"));
    }
    for (case, roles) in [
        (
            "only admin",
            &[("admin", "Full access to all resources", true)][..],
        ),
        (
            "only an admin's role",
            &[("editor", "Edits posts", false)][..],
        ),
    ] {
        if let Err(e) = any_role_skips_the_seed(backend, new_db, roles, case, &mut problems).await {
            problems.push(format!("{case}: {e}"));
        }
    }
    if let Err(e) = an_empty_table_is_seeded_once(backend, new_db, &mut problems).await {
        problems.push(format!("empty table: {e}"));
    }
    problems
}

async fn a_fresh_install_has_the_system_roles(
    backend: Backend,
    new_db: NewDatabase<'_>,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    let db = new_db().await;
    history::fresh_admin(&db, backend).await?;
    let seeded = rows(&db).await?;
    check_defaults(&seeded, "fresh install", problems);

    let (files, _) = MIGRATIONS.for_backend(backend);
    let (_, sql) = files
        .iter()
        .find(|(name, _)| *name == DEFAULT_ROLES)
        .ok_or("010 is not wired")?;
    let ledger = history::ledger(&db, MIGRATIONS.block).await?;
    match ledger.iter().find(|r| r.file_name == DEFAULT_ROLES) {
        Some(row) if row.origin == Origin::Ran && row.content_sha256 == content_hash(sql) => {}
        Some(row) => problems.push(format!(
            "fresh install: 010 is recorded {:?} with {}, not ran with {}",
            row.origin,
            row.content_sha256,
            content_hash(sql)
        )),
        None => problems.push("fresh install: 010 is not recorded".to_string()),
    }

    // The stamp sorts as text against the executor's stamps as the instants
    // do. Its own formatter, one millisecond either side of the seed …
    let Some(stamp) = seeded.first().and_then(|row| row[6].as_str()) else {
        return Ok(());
    };
    let instant = chrono::DateTime::parse_from_rfc3339(stamp)
        .map_err(|e| format!("parse {stamp:?}: {e}"))?
        .with_timezone(&chrono::Utc);
    let millisecond = chrono::Duration::milliseconds(1);
    let before = (instant - millisecond).to_rfc3339();
    let after = (instant + millisecond).to_rfc3339();
    if !(before.as_str() < stamp && stamp < after.as_str()) {
        problems.push(format!(
            "fresh install: {stamp:?} does not sort between the executor's {before:?} and \
             {after:?}"
        ));
    }
    // … and a role the executor writes after the seed, most likely within the
    // same second, which is where a `…Z` stamp would sort after it.
    let editor = write_role(&db, "editor", "Edits posts", false).await?;
    let written = editor[6].as_str().unwrap_or_default().to_string();
    if written.as_str() <= stamp {
        problems.push(format!(
            "fresh install: a role the executor wrote after the seed, stamped {written:?}, \
             sorts before the seed's {stamp:?}"
        ));
    }

    // Re-running the upgrade is a no-op.
    let before_rerun = rows(&db).await?;
    upgrade(&db, backend).await?;
    if rows(&db).await? != before_rerun {
        problems.push("fresh install: re-running the upgrade changed the roles".to_string());
    }
    Ok(())
}

async fn existing_roles_are_kept_as_they_are(
    backend: Backend,
    new_db: NewDatabase<'_>,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    let db = new_db().await;
    before_010(&db, backend).await?;
    // The system roles as the `Init` seed wrote them (executor ids and
    // stamps), and an admin's role beside them.
    for (_, name, description) in DEFAULTS {
        write_role(&db, name, description, true).await?;
    }
    write_role(&db, "editor", "Edits posts", false).await?;
    let before = rows(&db).await?;

    upgrade(&db, backend).await?;

    let after = rows(&db).await?;
    if after != before {
        problems.push(format!(
            "existing roles: the upgrade changed them from {before:?} to {after:?}"
        ));
    }
    if !history::ledger(&db, MIGRATIONS.block)
        .await?
        .iter()
        .any(|r| r.file_name == DEFAULT_ROLES && r.origin == Origin::Ran)
    {
        problems.push("existing roles: 010 is not recorded as ran".to_string());
    }
    Ok(())
}

/// A table holding exactly `roles` before 010 holds exactly them after it.
async fn any_role_skips_the_seed(
    backend: Backend,
    new_db: NewDatabase<'_>,
    roles: &[(&str, &str, bool)],
    case: &str,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    let db = new_db().await;
    before_010(&db, backend).await?;
    for (name, description, is_system) in roles {
        write_role(&db, name, description, *is_system).await?;
    }
    let before = rows(&db).await?;

    upgrade(&db, backend).await?;

    let after = rows(&db).await?;
    if after != before {
        problems.push(format!(
            "{case}: the upgrade changed the roles from {before:?} to {after:?}"
        ));
    }
    Ok(())
}

async fn an_empty_table_is_seeded_once(
    backend: Backend,
    new_db: NewDatabase<'_>,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    let db = new_db().await;
    before_010(&db, backend).await?;

    // An empty table before this release: the upgrade seeds it, as the next
    // boot's `Init` did.
    upgrade(&db, backend).await?;
    check_defaults(&rows(&db).await?, "empty table", problems);

    // Every role deleted after 010 ran (only the SQL explorer can delete a
    // system role): nothing seeds them again.
    for (id, ..) in DEFAULTS {
        db.delete(ROLES_TABLE, id)
            .await
            .map_err(|e| format!("delete {id}: {e}"))?;
    }
    upgrade(&db, backend).await?;
    let after_rerun = rows(&db).await?;
    if !after_rerun.is_empty() {
        problems.push(format!(
            "empty table: re-running the upgrade re-seeded {after_rerun:?}"
        ));
    }
    // Admin's record lost: the runner plans against the ledger, which
    // records 010, so nothing is pending and only the record is restored.
    history::stamp(&db, MIGRATIONS.block, "").await?;
    upgrade(&db, backend).await?;
    let after_replan = rows(&db).await?;
    if !after_replan.is_empty() {
        problems.push(format!(
            "empty table: re-planning against the ledger re-seeded {after_replan:?}"
        ));
    }
    if history::current_hash(&db, MIGRATIONS.block).await?
        != history::todays_hash(&MIGRATIONS, backend)
    {
        problems.push("empty table: admin's record is not today's list".to_string());
    }
    Ok(())
}
