//! The proof behind the files that declare the stamped columns (auth 017,
//! dev 005, tickets 002, vector 002): each is right for both kinds of
//! database that run it.
//!
//! Driven by `tests/migration_stamped_columns.rs` (SQLite) and by
//! `impresspress-native`'s `tests/postgres_migration_history.rs` (the
//! `postgres` lane), over databases the caller provides, empty.
//!
//! - **fresh:** every block's list from empty; each new file runs and is
//!   recorded `ran`.
//! - **lazy:** every block's list up to its new file (the files before it:
//!   a real database stood exactly there), rows seeded with raw SQL that
//!   names only the columns that list creates, then the stamped
//!   columns as the executor's lazy column-add created them before strict
//!   schema became the only behaviour (wafer-run `f0ca9c79`): a raw `ALTER
//!   TABLE … ADD COLUMN "c" TEXT` per column, nullable with no default, the
//!   definition `column_type_for_value` gave a string on both dialects, and
//!   the value its write stamped. That definition is the thing the
//!   PostgreSQL files' `IF NOT EXISTS` rests on. Then today's lists through
//!   the real runner, which runs each new file and every later one, as a
//!   deployment does: no refusal, each new file `ran`, the schema equal to
//!   the fresh install's — every column's declared type, nullability and
//!   default included — and every seeded value where the files say it lands.
//! - **lazy reset token:** the same, plus a password reset written before
//!   strict schema. `users` cannot be rebuilt (six cascading foreign keys),
//!   so auth 017 adds its columns plainly and that database is refused by
//!   name as drifted schema, 017 and every auth file after it unrecorded,
//!   while every other block upgrades.

use std::sync::Arc;

use serde_json::{json, Value};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_sql_utils::Backend;

use super::migration_history::{self as history, NewDatabase, Schema};
use crate::{
    migration_helper::{self, BlockMigrations, Refusal},
    platform_state::applied_migrations::Origin,
};

/// The file each block gained to declare the columns the executor stamps.
pub const STAMPED_COLUMN_FILES: &[(&str, &str)] = &[
    (
        crate::blocks::auth::AUTH_BLOCK_ID,
        crate::blocks::auth::migrations::RESET_TOKEN_COLUMNS,
    ),
    ("impresspress/dev", "005_stamped_columns"),
    ("impresspress/tickets", "002_stamped_columns"),
    ("impresspress/vector", "002_registry_updated_at"),
];

/// When every seeded row was created.
const CREATED: &str = "2026-10-01T00:00:00Z";
/// When the seeded maintenance pass ran.
const PRUNED: &str = "2026-10-02T00:00:00Z";
/// What the lazy writes stamped; the files replace it.
const LAZY_STAMP: &str = "2026-10-03T00:00:00Z";

const USERS: &str = crate::blocks::auth::repo::users::TABLE;
const CREDENTIALS: &str = "wafer_run__auth__local_credentials";
const GENERATIONS: &str = "impresspress__dev__generations";
const BUILDS: &str = "impresspress__dev__builds";
const SEED_INFO: &str = "impresspress__dev__seed_info";
const EVENTS: &str = "impresspress__tickets__events";
const ANALYSES: &str = "impresspress__tickets__analyses";
const MAINTENANCE: &str = "impresspress__tickets__maintenance";
const REGISTRY: &str = "impresspress__vector__registry";

fn compiled(block: &str) -> bool {
    history::blocks().iter().any(|m| m.block == block)
}

/// The new file of `migrations`, if it is one of [`STAMPED_COLUMN_FILES`].
fn new_file(migrations: &BlockMigrations) -> Option<&'static str> {
    STAMPED_COLUMN_FILES
        .iter()
        .find(|(block, _)| *block == migrations.block)
        .map(|(_, file)| *file)
}

/// Every block's list as it was before its new file: the files before it.
/// The new file and every later one are left to the upgrade.
async fn apply_before_new_files(
    db: &Arc<dyn DatabaseService>,
    backend: Backend,
) -> Result<(), String> {
    for migrations in history::blocks() {
        let (files, history_rows) = migrations.for_backend(backend);
        let upto = match new_file(migrations) {
            Some(file) => files
                .iter()
                .position(|(name, _)| *name == file)
                .ok_or_else(|| format!("{} does not ship {file}", migrations.block))?,
            None => files.len(),
        };
        migration_helper::apply_pending_via_service(
            db,
            backend,
            migrations.block,
            &files[..upto],
            history_rows,
        )
        .await
        .map_err(|e| format!("{} before its new file: {e}", migrations.block))?;
    }
    Ok(())
}

async fn exec(db: &Arc<dyn DatabaseService>, sql: &str, args: &[Value]) -> Result<(), String> {
    db.exec_raw(sql, args)
        .await
        .map(|_| ())
        .map_err(|e| format!("`{sql}`: {e}"))
}

/// One row in each table a new file rebuilds or alters, by raw SQL naming
/// only the columns the list before it creates (test-fixture setup).
async fn seed(db: &Arc<dyn DatabaseService>) -> Result<(), String> {
    exec(
        db,
        &format!(
            "INSERT INTO {USERS} (id, email, display_name, created_at, updated_at) \
             VALUES ('u1', 'u1@example.com', 'u1', '{CREATED}', '{CREATED}')"
        ),
        &[],
    )
    .await?;
    exec(
        db,
        &format!(
            "INSERT INTO {CREDENTIALS} (id, user_id, password_hash, created_at) \
             VALUES ('lc1', 'u1', 'hash', '{CREATED}')"
        ),
        &[],
    )
    .await?;
    if compiled("impresspress/dev") {
        exec(
            db,
            &format!(
                "INSERT INTO {GENERATIONS} (id, status, cause, site_manifest_json, \
                 block_manifest_json, manifest_sha256, created_at) \
                 VALUES ('g1', 'active', 'seed', '{{}}', '{{}}', 'sha', '{CREATED}')"
            ),
            &[],
        )
        .await?;
        exec(
            db,
            &format!(
                "INSERT INTO {BUILDS} (id, block_name, source_manifest_sha256, artifact_sha256, \
                 block_info_json, diagnostics_json, compiler_version, status, created_at, \
                 artifact_bytes) \
                 VALUES ('b1', 'site/x', 'src', 'art', '{{}}', '[]', 'rustc', 'valid', \
                 '{CREATED}', 7)"
            ),
            &[],
        )
        .await?;
        exec(
            db,
            &format!(
                "UPDATE {SEED_INFO} SET template = 'blank', imported_at = '{CREATED}' \
                 WHERE singleton_id = 1"
            ),
            &[],
        )
        .await?;
    }
    if compiled("impresspress/tickets") {
        exec(
            db,
            &format!(
                "INSERT INTO {EVENTS} (id, ticket_id, event_type, actor_type, created_at) \
                 VALUES ('e1', 't1', 'created', 'system', '{CREATED}')"
            ),
            &[],
        )
        .await?;
        exec(
            db,
            &format!(
                "INSERT INTO {ANALYSES} (id, ticket_id, source, summary, confidence, created_at) \
                 VALUES ('a1', 't1', 'ai', 'summary', 0.5, '{CREATED}')"
            ),
            &[],
        )
        .await?;
        exec(
            db,
            &format!(
                "UPDATE {MAINTENANCE} SET last_pruned_day = '2026-10-02', \
                 last_pruned_at = '{PRUNED}' WHERE id = 'singleton'"
            ),
            &[],
        )
        .await?;
    }
    if compiled("impresspress/vector") {
        exec(
            db,
            &format!(
                "INSERT INTO {REGISTRY} (prefixed_name, model, dimensions, keyword_search) \
                 VALUES ('impresspress__vector__docs', 'm', 3, 1)"
            ),
            &[],
        )
        .await?;
    }
    Ok(())
}

/// The columns production's typed writes created lazily before strict
/// schema, and what those writes left in them (test-fixture setup, by raw
/// SQL: no write adds a column now). The executor stamped `updated_at` on
/// every one; the maintenance `created_at` is `mark_audit_degraded`'s old
/// fallback create, which added the column before its insert failed on the
/// singleton's key, so it stayed NULL.
async fn write_lazily(db: &Arc<dyn DatabaseService>, reset_token: bool) -> Result<(), String> {
    for (table, column) in lazy_columns(reset_token) {
        // `column_type_for_value` at f0ca9c79: a string is `TEXT` on both
        // dialects, and the column was added nullable with no default.
        exec(
            db,
            &format!("ALTER TABLE {table} ADD COLUMN \"{column}\" TEXT"),
            &[],
        )
        .await?;
    }
    let mut stamped: Vec<(&str, &str, &str, &str)> = Vec::new();
    if reset_token {
        stamped.extend([
            (USERS, "reset_token", "id = 'u1'", "digest"),
            (
                USERS,
                "reset_token_expires",
                "id = 'u1'",
                "2026-10-04T00:00:00Z",
            ),
        ]);
    }
    if compiled("impresspress/dev") {
        stamped.extend([
            (GENERATIONS, "updated_at", "id = 'g1'", LAZY_STAMP),
            (BUILDS, "updated_at", "id = 'b1'", LAZY_STAMP),
            (SEED_INFO, "updated_at", "singleton_id = 1", LAZY_STAMP),
        ]);
    }
    if compiled("impresspress/tickets") {
        stamped.extend([
            (EVENTS, "updated_at", "ticket_id = 't1'", LAZY_STAMP),
            (ANALYSES, "updated_at", "ticket_id = 't1'", LAZY_STAMP),
            (MAINTENANCE, "updated_at", "id = 'singleton'", LAZY_STAMP),
        ]);
    }
    if compiled("impresspress/vector") {
        stamped.push((
            REGISTRY,
            "updated_at",
            "prefixed_name = 'impresspress__vector__docs'",
            LAZY_STAMP,
        ));
    }
    for (table, column, row, value) in stamped {
        exec(
            db,
            &format!("UPDATE {table} SET \"{column}\" = '{value}' WHERE {row}"),
            &[],
        )
        .await?;
    }
    Ok(())
}

/// The columns [`write_lazily`] creates, as `(table, column)`.
fn lazy_columns(reset_token: bool) -> Vec<(&'static str, &'static str)> {
    let mut columns = Vec::new();
    if reset_token {
        columns.extend([(USERS, "reset_token"), (USERS, "reset_token_expires")]);
    }
    if compiled("impresspress/dev") {
        columns.extend([
            (GENERATIONS, "updated_at"),
            (BUILDS, "updated_at"),
            (SEED_INFO, "updated_at"),
        ]);
    }
    if compiled("impresspress/tickets") {
        columns.extend([
            (EVENTS, "updated_at"),
            (ANALYSES, "updated_at"),
            (MAINTENANCE, "created_at"),
            (MAINTENANCE, "updated_at"),
        ]);
    }
    if compiled("impresspress/vector") {
        columns.push((REGISTRY, "updated_at"));
    }
    columns
}

/// Today's lists through the real runner, block by block: each block's
/// result.
async fn upgrade_all(
    db: &Arc<dyn DatabaseService>,
    backend: Backend,
) -> Vec<(&'static str, Result<(), migration_helper::MigrationError>)> {
    let mut out = Vec::new();
    for migrations in history::blocks() {
        out.push((
            migrations.block,
            history::upgrade(db, backend, migrations).await,
        ));
    }
    out
}

/// Each new file's ledger row is `ran`; `skip` names a block not to check.
async fn check_ran(
    db: &Arc<dyn DatabaseService>,
    skip: Option<&str>,
    case: &str,
    problems: &mut Vec<String>,
) {
    for (block, file) in STAMPED_COLUMN_FILES {
        if !compiled(block) || Some(*block) == skip {
            continue;
        }
        match history::ledger(db, block).await {
            Ok(rows) => match rows.iter().find(|r| r.file_name == *file) {
                Some(row) if row.origin == Origin::Ran => {}
                Some(row) => problems.push(format!("{case}: {block} {file} is {:?}", row.origin)),
                None => problems.push(format!("{case}: {block} {file} is not recorded")),
            },
            Err(e) => problems.push(format!("{case}: {block} ledger: {e}")),
        }
    }
}

async fn value(
    db: &Arc<dyn DatabaseService>,
    sql: &str,
    column: &str,
) -> Result<Option<Value>, String> {
    let rows = db
        .query_raw(sql, &[])
        .await
        .map_err(|e| format!("`{sql}`: {e}"))?;
    let row = rows.first().ok_or_else(|| format!("`{sql}`: no row"))?;
    Ok(row.data.get(column).cloned())
}

/// Every seeded value where the files say it lands: the rows kept, the new
/// columns filled from the row's own data (the lazily stamped value
/// replaced), `artifact_bytes` and the credential untouched.
async fn check_values(db: &Arc<dyn DatabaseService>, case: &str, problems: &mut Vec<String>) {
    let mut want: Vec<(String, &str, Option<Value>)> = vec![
        (
            format!("SELECT email FROM {USERS} WHERE id = 'u1'"),
            "email",
            Some(json!("u1@example.com")),
        ),
        (
            format!("SELECT user_id FROM {CREDENTIALS} WHERE id = 'lc1'"),
            "user_id",
            Some(json!("u1")),
        ),
    ];
    if compiled("impresspress/dev") {
        want.extend([
            (
                format!("SELECT updated_at FROM {GENERATIONS} WHERE id = 'g1'"),
                "updated_at",
                Some(json!(CREATED)),
            ),
            (
                format!("SELECT updated_at FROM {BUILDS} WHERE id = 'b1'"),
                "updated_at",
                Some(json!(CREATED)),
            ),
            (
                format!("SELECT artifact_bytes FROM {BUILDS} WHERE id = 'b1'"),
                "artifact_bytes",
                Some(json!(7)),
            ),
            (
                format!("SELECT updated_at FROM {SEED_INFO} WHERE singleton_id = 1"),
                "updated_at",
                Some(json!(CREATED)),
            ),
            (
                format!("SELECT template FROM {SEED_INFO} WHERE singleton_id = 1"),
                "template",
                Some(json!("blank")),
            ),
        ]);
    }
    if compiled("impresspress/tickets") {
        want.extend([
            (
                format!("SELECT updated_at FROM {EVENTS} WHERE id = 'e1'"),
                "updated_at",
                Some(json!(CREATED)),
            ),
            (
                format!("SELECT updated_at FROM {ANALYSES} WHERE id = 'a1'"),
                "updated_at",
                Some(json!(CREATED)),
            ),
            (
                format!("SELECT updated_at FROM {MAINTENANCE} WHERE id = 'singleton'"),
                "updated_at",
                Some(json!(PRUNED)),
            ),
            (
                format!("SELECT created_at FROM {MAINTENANCE} WHERE id = 'singleton'"),
                "created_at",
                Some(Value::Null),
            ),
        ]);
    }
    if compiled("impresspress/vector") {
        want.extend([
            (
                format!(
                    "SELECT updated_at FROM {REGISTRY} \
                     WHERE prefixed_name = 'impresspress__vector__docs'"
                ),
                "updated_at",
                Some(Value::Null),
            ),
            (
                format!(
                    "SELECT dimensions FROM {REGISTRY} \
                     WHERE prefixed_name = 'impresspress__vector__docs'"
                ),
                "dimensions",
                Some(json!(3)),
            ),
        ]);
    }
    for (sql, column, expected) in want {
        match value(db, &sql, column).await {
            // A NULL may come back as an absent key or as `null`.
            Ok(got) if got == expected || (expected == Some(Value::Null) && got.is_none()) => {}
            Ok(got) => problems.push(format!("{case}: `{sql}` is {got:?}, not {expected:?}")),
            Err(e) => problems.push(format!("{case}: {e}")),
        }
    }
}

/// The three cases of the module docs, each on a database `new_db` hands
/// back empty: what is wrong, if anything.
pub async fn check(backend: Backend, new_db: NewDatabase<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    if let Err(e) = check_cases(backend, new_db, &mut problems).await {
        problems.push(e);
    }
    problems
}

async fn check_cases(
    backend: Backend,
    new_db: NewDatabase<'_>,
    problems: &mut Vec<String>,
) -> Result<(), String> {
    // Fresh.
    let fresh_db = new_db().await;
    for (block, result) in upgrade_all(&fresh_db, backend).await {
        if let Err(e) = result {
            problems.push(format!("fresh: {block}: {e}"));
        }
    }
    check_ran(&fresh_db, None, "fresh", problems).await;
    let fresh = Schema::read(&fresh_db, backend).await?;

    // Lazy stamped columns, no reset token.
    let lazy = new_db().await;
    apply_before_new_files(&lazy, backend).await?;
    seed(&lazy).await?;
    write_lazily(&lazy, false).await?;
    let before = Schema::read(&lazy, backend).await?;
    for (table, column) in lazy_columns(false) {
        let present = before
            .tables
            .get(table)
            .is_some_and(|t| t.columns.iter().any(|c| c.name == column));
        if !present {
            problems.push(format!(
                "lazy: the fixture did not create {table}.{column} lazily"
            ));
        }
    }
    for (block, result) in upgrade_all(&lazy, backend).await {
        if let Err(e) = result {
            problems.push(format!("lazy: {block}: {e}"));
        }
    }
    check_ran(&lazy, None, "lazy", problems).await;
    let after = Schema::read(&lazy, backend).await?;
    // Each lazily created column, as declared by the fresh install: type,
    // nullability and default, not only presence.
    for (table, column) in lazy_columns(false) {
        let definition = |schema: &Schema| {
            schema
                .tables
                .get(table)
                .and_then(|t| t.columns.iter().find(|c| c.name == column).cloned())
        };
        let (got, want) = (definition(&after), definition(&fresh));
        if got != want {
            problems.push(format!(
                "lazy: {table}.{column} is {got:?} after the upgrade, {want:?} on a fresh install"
            ));
        }
    }
    let comparison = after.compare(&fresh);
    if !comparison.extra.is_empty() || !comparison.missing.is_empty() {
        problems.push(format!(
            "lazy: not a fresh install's schema: extra {:?}, missing {:?}",
            comparison.extra, comparison.missing
        ));
    }
    check_values(&lazy, "lazy", problems).await;

    // Lazy reset token: auth refuses by name, the rest upgrade.
    let reset = new_db().await;
    apply_before_new_files(&reset, backend).await?;
    seed(&reset).await?;
    write_lazily(&reset, true).await?;
    let auth = crate::blocks::auth::AUTH_BLOCK_ID;
    let reset_file = crate::blocks::auth::migrations::RESET_TOKEN_COLUMNS;
    for (block, result) in upgrade_all(&reset, backend).await {
        match (block == auth, result) {
            (false, Ok(())) => {}
            (false, Err(e)) => problems.push(format!("lazy reset token: {block}: {e}")),
            (true, Ok(())) => problems.push(
                "lazy reset token: auth upgraded over the lazily created reset columns".to_string(),
            ),
            (true, Err(e)) => match e.refusal() {
                Some(Refusal::DriftedSchema { file, .. }) if file == reset_file => {}
                _ => problems.push(format!(
                    "lazy reset token: auth failed, but not as drifted schema at {reset_file}: {e}"
                )),
            },
        }
    }
    check_ran(&reset, Some(auth), "lazy reset token", problems).await;
    // The refusal stops the block: neither 017 nor any auth file after it
    // runs.
    let (auth_files, _) = crate::blocks::auth::migrations::MIGRATIONS.for_backend(backend);
    let from = auth_files
        .iter()
        .position(|(name, _)| *name == reset_file)
        .ok_or_else(|| format!("auth does not ship {reset_file}"))?;
    match history::ledger(&reset, auth).await {
        Ok(rows) => {
            for (file, _) in &auth_files[from..] {
                if rows.iter().any(|r| r.file_name == *file) {
                    problems.push(format!(
                        "lazy reset token: {file} is recorded although auth was refused at \
                         {reset_file}"
                    ));
                }
            }
        }
        Err(e) => problems.push(format!("lazy reset token: auth ledger: {e}")),
    }
    check_values(&reset, "lazy reset token", problems).await;
    Ok(())
}
