//! Shared migration helper.
//!
//! A block's migrations are an ordered list of `.sql` files, each a
//! [`MigrationFile`] `(basename, sql)` pair. Every block applies its list from
//! `lifecycle(Init)` through [`lifecycle_init`] (auth's service `init` calls
//! [`apply_migrations`] directly), which picks the list for the active SQL
//! dialect and hands it to [`apply_pending`]:
//!
//! 1. Reads the block's `MigrationState` from the cached `BlockSettings`.
//! 2. Hashes the list ([`migration_set_hash`]: SHA-256 of the files' SQL
//!    joined with `\n`).
//! 3. If `current_hash` matches → already applied, return.
//! 4. If this runtime build applies migrations (`IMPRESSPRESS_RUN_MIGRATIONS`
//!    is `"1"`) or the block has never applied any → run every file's
//!    statements in order through `db::ddl`, then record the list's hash as
//!    the block's `current_hash` in `impresspress__admin__block_settings`.
//! 5. Otherwise → log `schema drift` and return.
//!
//! # Which builds apply migrations
//!
//! Every build that is a deploy or a boot sets `IMPRESSPRESS_RUN_MIGRATIONS`:
//!
//! - **native**: every boot (`impresspress_server::build_native_runtime`).
//!   The admin block's list runs before the runtime is built, through
//!   [`apply_pending_via_service`], because the server seeds its variables
//!   from admin's tables pre-build; every other block's list runs at its
//!   `Init`. The server refuses to start if any of it fails, and a
//!   database-level lock keeps two processes booting against one database
//!   from applying the same list twice (`impresspress_native::migration_lock`).
//! - **Cloudflare**: the `/_deploy/prepare` / `/_deploy/init` funnel, before
//!   the new version is promoted. A failure fails the deploy.
//! - **browser**: every runtime build, because loading a bundle is its
//!   deploy (`impresspress-web`'s `RuntimeFactory::build`).
//!
//! Cloudflare's request-path builds are the only ones that do not, so step 5
//! is reached only there, by a version serving before its deploy funnel ran.
//!
//! # A failure names the block and the file
//!
//! A statement the database refuses stops the list. The error names the
//! block, the file's basename and the statement, and the list is not
//! recorded as applied, so the next migrating build runs it again. The
//! statement splitter handles `;` outside `--` comments. Block comments
//! `/* ... */` and `;` inside string literals are not supported — the
//! canonical .sql files don't use either.
//!
//! # Adding a file re-runs the whole list
//!
//! Step 2 hashes the whole list, so a new file makes every earlier file of
//! that block run again too. Every file therefore has to be re-runnable over
//! the schema it already built (`CREATE … IF NOT EXISTS`, a duplicate
//! `ADD COLUMN` is tolerated, repairs that only touch rows still needing
//! them). Auth's list is the costly case: its `004` drops the refresh-token
//! table and `012` the session table, so ANY change to the auth list signs
//! every user out on the deploy or boot that applies it.
//!
//! # A shipped .sql file is immutable, comments included
//!
//! Step 2 hashes the file's **whole text**, so editing a `--` comment in a
//! migration that has already shipped changes its hash exactly as much as
//! editing a statement does: the next deploy or boot of every deployment
//! that already applied it re-runs that block's whole list from 001 — for
//! auth, signing everyone out — and a Cloudflare version serving before its
//! deploy funnel logs `schema drift` until it runs.
//!
//! So do not tidy prose in a shipped migration, even to fix a comment that
//! names a since-renamed Rust item. Put the explanation in the block's
//! `migrations/mod.rs` beside the test that covers the migration, where it is
//! not hash-addressed and a reader is more likely to find it. Products'
//! `slug_collision_cannot_fail_020` is the worked example.

use std::sync::Arc;

use wafer_core::{
    clients::{config, database as db},
    interfaces::database::service::DatabaseService,
};
use wafer_run::{context::Context, ErrorCode, LifecycleEvent, LifecycleType, WaferError};
use wafer_sql_utils::Backend;

use crate::{
    features::{BlockSettings, MigrationState, BLOCK_SETTINGS_CONFIG_KEY},
    platform_state::block_settings::{self, BlockSettingsPatch},
};
// NOTE: `BlockSettings::state_for` parses only the requested block's entry
// out of the JSON map, avoiding the full-map materialization that
// `from_config_json` would do on every `apply_pending` call.

/// Config key a runtime build carries, as `"1"`, when it applies pending
/// migrations: every native boot, Cloudflare's deploy funnel and every
/// browser build (see the module docs). An infrastructure key: it is set on
/// the config snapshot by the target's boot code, never read from the
/// process environment or the variables table.
pub const RUN_MIGRATIONS_KEY: &str = "IMPRESSPRESS_RUN_MIGRATIONS";

/// One migration file: its basename (`"003_block_settings_seed_hash"`, the
/// file name without the dialect suffix) and its SQL. A block's list of them,
/// in apply order, is its `migrations::SQLITE_MIGRATIONS` /
/// `migrations::POSTGRES_MIGRATIONS`.
pub type MigrationFile<'a> = (&'a str, &'a str);

/// Shared config var that selects the SQL dialect. `"postgres"`
/// (case-insensitive) picks the PostgreSQL dialect; anything else (including
/// the unset default) picks SQLite. Read by both the migration dispatcher
/// ([`apply_migrations`]) and the runtime query-builder helper
/// ([`db_backend`]) so a deployment's migrations and its live queries always
/// render against the same dialect.
pub const DATABASE_BACKEND_KEY: &str = "WAFER_RUN_SHARED__DATABASE__BACKEND";

/// Resolve the active SQL [`Backend`] from the config snapshot.
///
/// Reads [`DATABASE_BACKEND_KEY`] (the same var [`apply_migrations`] uses to
/// pick which `.sql` dialect files to run) and maps `"postgres"`
/// (case-insensitive) to [`Backend::Postgres`], everything else to
/// [`Backend::Sqlite`].
///
/// This is the single runtime source of truth for the query-builder dialect:
/// every `wafer_sql_utils::{query,aggregate,introspect,upsert}` call site in
/// a block passes the result of this helper rather than a hardcoded
/// `Backend::Sqlite`, so a postgres deployment renders postgres-dialect SQL
/// at runtime, matching the migrations it applied at boot.
///
/// Cheap to call per request: the value comes from the in-memory config
/// snapshot via `config::get_default` (no DB hop), so call sites read it
/// inline rather than threading a cached `Backend` through every signature.
///
/// A failed read is returned: rendering the other dialect's SQL against the
/// database would fail anyway, further from the cause.
pub async fn db_backend(ctx: &dyn Context) -> Result<Backend, WaferError> {
    let backend = config::get_default(ctx, DATABASE_BACKEND_KEY, "sqlite")
        .await?
        .to_ascii_lowercase();
    Ok(if backend == "postgres" {
        Backend::Postgres
    } else {
        Backend::Sqlite
    })
}

/// Pick the block's migration list for the active SQL dialect
/// (`WAFER_RUN_SHARED__DATABASE__BACKEND`, read through [`db_backend`]) and
/// forward it to [`apply_pending`].
///
/// ```ignore
/// pub async fn apply(ctx: &dyn Context) -> Result<(), String> {
///     migration_helper::apply_migrations(
///         ctx,
///         "impresspress/messages",
///         migrations::SQLITE_MIGRATIONS,
///         migrations::POSTGRES_MIGRATIONS,
///     )
///     .await
/// }
/// ```
///
/// Backends other than `"postgres"` (case-insensitive) take `sqlite_files`,
/// matching the `config::get_default(..., "sqlite")` default every consumer
/// reads.
pub async fn apply_migrations(
    ctx: &dyn Context,
    block_name: &str,
    sqlite_files: &[MigrationFile<'_>],
    postgres_files: &[MigrationFile<'_>],
) -> Result<(), String> {
    let backend = db_backend(ctx)
        .await
        .map_err(|e| format!("{block_name}: read the database backend: {e}"))?;
    let files = match backend {
        Backend::Postgres => postgres_files,
        Backend::Sqlite => sqlite_files,
    };
    apply_pending(ctx, block_name, files).await
}

/// `lifecycle(Init)` body shared by every impresspress feature block: on the
/// [`Init`](LifecycleType::Init) event, apply the block's migrations and turn
/// a failure into a [`WaferError`]. Non-`Init` events are a no-op.
///
/// `sqlite_migrations` / `postgres_migrations` are the block's
/// `migrations::SQLITE_MIGRATIONS` / `migrations::POSTGRES_MIGRATIONS`; the
/// active dialect is selected inside [`apply_migrations`]. The error already
/// names the block (and, for a refused statement, the file), so it is passed
/// through as the message.
///
/// Blocks with extra `Init` work (admin's seed steps, llm's legacy-provider
/// copy + reload) call this first, then run their extra steps; blocks with no
/// migrations at all (the embedding wrappers) simply omit `lifecycle`.
pub async fn lifecycle_init(
    ctx: &dyn Context,
    event: &LifecycleEvent,
    block_name: &str,
    sqlite_migrations: &[MigrationFile<'_>],
    postgres_migrations: &[MigrationFile<'_>],
) -> Result<(), WaferError> {
    if !matches!(event.event_type, LifecycleType::Init) {
        return Ok(());
    }
    apply_migrations(ctx, block_name, sqlite_migrations, postgres_migrations)
        .await
        .map_err(|e| WaferError::new(ErrorCode::Internal, e))
}

/// Apply `files` against `db::ddl` when they are pending and this build may
/// apply them (module docs, steps 1-5). Idempotent across calls: returns
/// early once `current_hash` in the cached `BlockSettings` matches the list's
/// hash.
///
/// `block_name` is the full block name (e.g. `"impresspress/files"`); every
/// error names it.
pub async fn apply_pending(
    ctx: &dyn Context,
    block_name: &str,
    files: &[MigrationFile<'_>],
) -> Result<(), String> {
    let code_hash = migration_set_hash(files);
    let state = read_state(ctx, block_name);
    if state.current_hash == code_hash {
        return Ok(());
    }

    // Read directly from the config snapshot: `IMPRESSPRESS_RUN_MIGRATIONS`
    // is an infrastructure key each target's boot code sets on the snapshot,
    // never a variables-table row. Tests set it via `ctx.set_config(...)`.
    let run_requested = ctx.config_get(RUN_MIGRATIONS_KEY) == Some("1");
    // A block that never applied anything has no prior schema to protect, so
    // even a build that does not migrate (Cloudflare's request path) creates
    // it.
    let is_fresh = state.current_hash.is_empty();
    if !(is_fresh || run_requested) {
        tracing::warn!(
            block = %block_name,
            current = %state.current_hash,
            code = %code_hash,
            "schema drift; this build does not apply migrations, the next deploy does"
        );
        return Ok(());
    }

    run_files(block_name, files, |stmt| db::ddl(ctx, stmt))
        .await
        .map_err(|failed| {
            tracing::warn!(error = %failed, "migration failed");
            failed.to_string()
        })?;
    record_applied(ctx, block_name, &code_hash).await
}

/// Apply `block_name`'s pending migrations straight through a
/// [`DatabaseService`], before the runtime exists.
///
/// The native server's counterpart of [`apply_pending`], for the one list
/// it must apply pre-build: admin's, whose tables hold the variables and
/// block settings the server reads to build its runtime. It keeps the same
/// tracking — reads the block's `block_settings` row, returns when its
/// `current_hash` matches [`migration_set_hash`], otherwise runs every file
/// and stamps the row — so the block's own `Init` then finds the list applied
/// and skips it. A native boot always applies migrations, so there is no
/// `IMPRESSPRESS_RUN_MIGRATIONS` gate to consult.
///
/// The statements go through `db.exec_raw` — the migration-file-runner
/// exception to the no-raw-SQL rule (CLAUDE.md).
pub async fn apply_pending_via_service(
    db: &Arc<dyn DatabaseService>,
    block_name: &str,
    files: &[MigrationFile<'_>],
) -> Result<(), String> {
    let code_hash = migration_set_hash(files);
    let applied = block_settings::load(db)
        .await
        .map_err(|e| format!("{block_name}: read the applied migrations: {e}"))?
        .state(block_name)
        .migration
        .current_hash;
    if applied == code_hash {
        return Ok(());
    }
    run_files(block_name, files, |stmt| db.exec_raw(stmt, &[]))
        .await
        .map_err(|failed| failed.to_string())?;
    let patch = BlockSettingsPatch {
        current_hash: Some(code_hash),
        ..Default::default()
    };
    block_settings::upsert_fields_via_service(db, block_name, patch)
        .await
        .map_err(|e| format!("{block_name}: record the applied migrations: {e}"))
}

/// Run migration SQL straight through a [`DatabaseService`], with no
/// tracking at all: every statement of every file, every time.
///
/// For test fixtures and replay tests that build a schema up to a chosen
/// point, or replay a list over the schema it already built — the runner
/// shares its statement loop (and its duplicate-`ADD COLUMN` tolerance) with
/// the tracked ones, so what a replay test proves here holds for them. A
/// deployment never calls it: it applies migrations through
/// [`apply_pending`] / [`apply_pending_via_service`].
pub async fn apply_ddl_via_service(
    db: &Arc<dyn DatabaseService>,
    sql_files: &[&str],
) -> Result<(), String> {
    for sql in sql_files {
        run_statements(sql, |stmt| db.exec_raw(stmt, &[]))
            .await
            .map_err(|failed| format!("ddl failed on `{}`: {}", failed.statement, failed.error))?;
    }
    Ok(())
}

/// The hash that identifies a block's migration list: SHA-256 hex of the
/// files' SQL joined with `\n`, in order. The basenames take no part in it.
pub fn migration_set_hash(files: &[MigrationFile<'_>]) -> String {
    let joined = files
        .iter()
        .map(|(_, sql)| *sql)
        .collect::<Vec<_>>()
        .join("\n");
    sha256_hex(&joined)
}

/// A migration statement the database refused, and why.
struct FailedStatement<'a> {
    statement: &'a str,
    error: String,
}

/// A block's migration that failed: which block, which file, which
/// statement, and the database's error. Its `Display` is the error every
/// runner returns, so a boot that refuses to start says exactly this.
struct MigrationFailure<'a> {
    block: &'a str,
    file: &'a str,
    statement: &'a str,
    error: String,
}

impl std::fmt::Display for MigrationFailure<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: migration `{}` failed on `{}`: {}",
            self.block, self.file, self.statement, self.error
        )
    }
}

/// Run every file of `files` in order through `exec`, stopping at the first
/// refused statement and naming its file.
async fn run_files<'a, F, Fut, T, E>(
    block: &'a str,
    files: &[MigrationFile<'a>],
    mut exec: F,
) -> Result<(), MigrationFailure<'a>>
where
    F: FnMut(&'a str) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    for &(file, sql) in files {
        run_statements(sql, &mut exec)
            .await
            .map_err(|failed| MigrationFailure {
                block,
                file,
                statement: failed.statement,
                error: failed.error,
            })?;
    }
    Ok(())
}

/// Run each statement of a migration batch through `exec`, in order, stopping
/// at the first failure.
///
/// The one statement loop behind every runner — [`apply_pending`],
/// [`apply_pending_via_service`] and the untracked [`apply_ddl_via_service`]
/// — so what a replay test proves through the untracked one holds for the
/// tracked ones. `ALTER TABLE ... ADD COLUMN` is non-idempotent on SQLite/D1,
/// which have no `IF NOT EXISTS` for columns: when an earlier run already
/// added the column the re-run raises "duplicate column", and that — only for
/// an `ADD COLUMN` statement — is a benign no-op so the rest of the batch
/// still runs. Every other failure is returned.
async fn run_statements<'a, F, Fut, T, E>(
    sql: &'a str,
    mut exec: F,
) -> Result<(), FailedStatement<'a>>
where
    F: FnMut(&'a str) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    for stmt in split_statements(sql) {
        if !has_executable_content(stmt) {
            continue;
        }
        let statement = stmt.trim();
        if let Err(e) = exec(statement).await {
            let error = e.to_string();
            if is_alter_add_column(statement) && is_duplicate_column_error(&error) {
                tracing::debug!(
                    stmt = %statement,
                    err = %error,
                    "ddl: duplicate column, treating as idempotent no-op",
                );
                continue;
            }
            return Err(FailedStatement { statement, error });
        }
    }
    Ok(())
}

/// Read the cached `BlockSettings` from the wafer config and look up the
/// migration state for `block_name`. Returns an empty `MigrationState` when
/// no row exists yet.
///
/// Reads directly from the config snapshot — `BLOCK_SETTINGS_CONFIG_KEY` is
/// a synthetic key set at boot by the loader (not a DB-backed config var),
/// so it lives in `ctx.config_get`, not behind the config block service.
fn read_state(ctx: &dyn Context, block_name: &str) -> MigrationState {
    let json = ctx.config_get(BLOCK_SETTINGS_CONFIG_KEY).unwrap_or("{}");
    BlockSettings::state_for(json, block_name).migration
}

/// Record `code_hash` as the list applied for `block_name`: its row's
/// `current_hash` in `impresspress__admin__block_settings`, creating the row
/// when it has none and preserving its `enabled` flag otherwise.
async fn record_applied(
    ctx: &dyn Context,
    block_name: &str,
    code_hash: &str,
) -> Result<(), String> {
    let patch = BlockSettingsPatch {
        current_hash: Some(code_hash.to_string()),
        ..Default::default()
    };
    block_settings::upsert_fields(ctx, block_name, patch)
        .await
        .map_err(|e| format!("{block_name}: record the applied migrations: {e}"))
}

/// Compute a SHA-256 hex digest. Re-exported for callers (e.g.
/// `admin::settings::seed_defaults`) that hash-gate against a payload other
/// than SQL bytes but want to share the same digest algorithm with
/// [`migration_set_hash`].
///
/// Delegates to the single canonical [`crate::util::sha256_hex`]
/// (`wafer_block::hash`) — there is one SHA-256-hex implementation in
/// impresspress, not a private copy here.
pub(crate) fn sha256_hex_bytes(payload: &[u8]) -> String {
    crate::util::sha256_hex(payload)
}

fn sha256_hex(sql: &str) -> String {
    sha256_hex_bytes(sql.as_bytes())
}

/// Split `sql` on `;` outside `--` line comments. Returns byte-range slices
/// into the original `sql` — no per-statement allocation.
fn split_statements(sql: &str) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut in_line_comment = false;
    let mut prev_was_dash = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            prev_was_dash = false;
            continue;
        }
        if b == b'-' && prev_was_dash {
            in_line_comment = true;
            prev_was_dash = false;
            continue;
        }
        if b == b';' {
            out.push(&sql[start..i]);
            start = i + 1;
            prev_was_dash = false;
            continue;
        }
        prev_was_dash = b == b'-';
    }
    if start < bytes.len() {
        out.push(&sql[start..]);
    }
    out
}

fn has_executable_content(stmt: &str) -> bool {
    stmt.lines().any(|line| {
        let l = line.trim();
        !l.is_empty() && !l.starts_with("--")
    })
}

/// `stmt`'s executable text, upper-cased with its whitespace collapsed:
/// comment lines dropped, so a leading `--` header does not hide its shape.
fn executable_shape(stmt: &str) -> String {
    stmt.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("--"))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `true` when `stmt`'s first executable token sequence is
/// `ALTER TABLE … ADD COLUMN`. Case-insensitive, comment-tolerant.
///
/// Used to gate the duplicate-column-error tolerance in
/// `apply_pending` — we only swallow the benign duplicate on this
/// specific statement shape, not on every DDL.
fn is_alter_add_column(stmt: &str) -> bool {
    let shape = executable_shape(stmt);
    shape.starts_with("ALTER TABLE ") && shape.contains(" ADD COLUMN ")
}

/// `true` when an error message looks like a "column already exists"
/// failure from any backend we target.
///
/// - SQLite / D1: `duplicate column name`
/// - PostgreSQL: `column "<name>" of relation "<table>" already exists`
///
/// Substring match is intentional — backends wrap these strings in their
/// own error envelopes and we only care that the canonical phrase appears.
fn is_duplicate_column_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("duplicate column name") || lower.contains("already exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_deterministic() {
        let a = sha256_hex("CREATE TABLE foo (id TEXT);");
        let b = sha256_hex("CREATE TABLE foo (id TEXT);");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn sha256_hex_differs_for_different_input() {
        let a = sha256_hex("CREATE TABLE foo (id TEXT);");
        let b = sha256_hex("CREATE TABLE bar (id TEXT);");
        assert_ne!(a, b);
    }

    #[test]
    fn detects_alter_table_add_column_in_all_shapes() {
        assert!(is_alter_add_column("ALTER TABLE foo ADD COLUMN bar TEXT"));
        assert!(is_alter_add_column("alter table foo add column bar text"));
        // Tolerates leading line comments + extra whitespace.
        assert!(is_alter_add_column(
            "-- header\n\nALTER TABLE foo\n  ADD COLUMN bar TEXT"
        ));
        // Excludes other ALTER variants and unrelated DDL.
        assert!(!is_alter_add_column("ALTER TABLE foo DROP COLUMN bar"));
        assert!(!is_alter_add_column("CREATE TABLE foo (id TEXT)"));
        assert!(!is_alter_add_column("CREATE INDEX bar ON foo (id)"));
    }

    #[test]
    fn detects_duplicate_column_errors_from_each_backend() {
        // SQLite / D1 wording.
        assert!(is_duplicate_column_error(
            "Internal: internal database error: duplicate column name: block"
        ));
        // Mixed case is fine.
        assert!(is_duplicate_column_error("Duplicate Column Name: foo"));
        // PostgreSQL wording.
        assert!(is_duplicate_column_error(
            r#"column "block" of relation "impresspress__admin__variables" already exists"#
        ));
        // Non-matching errors stay non-matching.
        assert!(!is_duplicate_column_error("no such table: foo"));
        assert!(!is_duplicate_column_error("syntax error near 'ALTER'"));
    }

    #[test]
    fn empty_chunk_has_no_executable_content() {
        assert!(!has_executable_content(""));
        assert!(!has_executable_content("   \n  "));
    }

    #[test]
    fn comment_only_chunk_is_skipped() {
        assert!(!has_executable_content("-- one\n-- two\n"));
    }

    #[test]
    fn ddl_with_leading_comment_is_executed() {
        assert!(has_executable_content(
            "-- header\nCREATE TABLE foo (id TEXT)"
        ));
    }

    #[test]
    fn split_ignores_semicolons_inside_line_comments() {
        let sql = "-- Placeholder; text\nSELECT 1;";
        let parts = split_statements(sql);
        assert_eq!(parts.len(), 1);
        assert!(parts[0].contains("SELECT 1"));
    }

    #[test]
    fn split_handles_multiple_statements() {
        let sql = "DROP TABLE foo;\nCREATE TABLE bar (id TEXT);\n";
        let count = split_statements(sql)
            .into_iter()
            .filter(|s| has_executable_content(s))
            .count();
        assert_eq!(count, 2);
    }

    #[test]
    fn legalpages_sql_splits_into_expected_chunks() {
        let sql_sqlite =
            include_str!("blocks/legalpages/migrations/001_legalpages_schema.sqlite.sql");
        let sqlite_count = split_statements(sql_sqlite)
            .into_iter()
            .filter(|s| has_executable_content(s))
            .count();
        assert_eq!(
            sqlite_count, 2,
            "legalpages sqlite migration: expected 2 statements, got {sqlite_count}"
        );

        let sql_postgres =
            include_str!("blocks/legalpages/migrations/001_legalpages_schema.postgres.sql");
        let postgres_count = split_statements(sql_postgres)
            .into_iter()
            .filter(|s| has_executable_content(s))
            .count();
        assert_eq!(
            postgres_count, 2,
            "legalpages postgres migration: expected 2 statements, got {postgres_count}"
        );
    }

    #[test]
    fn files_sql_splits_into_expected_chunks() {
        // Counts the executable statements in the files block SQL files.
        // Fails if the SQL file is edited and the helper's splitter is broken
        // or a new statement is added without updating this count.
        let sql_sqlite = include_str!("blocks/files/migrations/001_initial_schema.sqlite.sql");
        let sqlite_count = split_statements(sql_sqlite)
            .into_iter()
            .filter(|s| has_executable_content(s))
            .count();
        assert_eq!(
            sqlite_count, 14,
            "files sqlite migration: expected 14 statements, got {sqlite_count}"
        );

        let sql_postgres = include_str!("blocks/files/migrations/001_initial_schema.postgres.sql");
        let postgres_count = split_statements(sql_postgres)
            .into_iter()
            .filter(|s| has_executable_content(s))
            .count();
        assert_eq!(
            postgres_count, 14,
            "files postgres migration: expected 14 statements, got {postgres_count}"
        );

        // 002: the duplicate-name repair and the unique index it makes
        // creatable — two statements, and BOTH have to reach `db::ddl` or the
        // index is never built on a database that already holds duplicates.
        for (dialect, sql) in [
            (
                "sqlite",
                include_str!("blocks/files/migrations/002_bucket_name_unique.sqlite.sql"),
            ),
            (
                "postgres",
                include_str!("blocks/files/migrations/002_bucket_name_unique.postgres.sql"),
            ),
        ] {
            let count = split_statements(sql)
                .into_iter()
                .filter(|s| has_executable_content(s))
                .count();
            assert_eq!(
                count, 2,
                "files {dialect} migration 002: expected 2 statements, got {count}"
            );
        }
    }

    #[test]
    fn admin_004_splits_into_its_repair_and_its_index() {
        // Both statements have to reach `db::ddl`: without the `DELETE` the
        // index cannot be created on a database that already repeats a grant.
        for (dialect, sql) in [
            (
                "sqlite",
                include_str!("blocks/admin/migrations/004_user_roles_unique.sqlite.sql"),
            ),
            (
                "postgres",
                include_str!("blocks/admin/migrations/004_user_roles_unique.postgres.sql"),
            ),
        ] {
            let count = split_statements(sql)
                .into_iter()
                .filter(|s| has_executable_content(s))
                .count();
            assert_eq!(
                count, 2,
                "admin {dialect} migration 004: expected 2 statements, got {count}"
            );
        }
    }

    #[test]
    fn admin_005_splits_into_its_column_and_its_repair() {
        // The `ADD COLUMN` has to reach the runner on its own, so a re-run's
        // duplicate-column error is recognised and the `UPDATE` still runs.
        for (dialect, sql) in [
            (
                "sqlite",
                include_str!("blocks/admin/migrations/005_wrap_grants_append_column.sqlite.sql"),
            ),
            (
                "postgres",
                include_str!("blocks/admin/migrations/005_wrap_grants_append_column.postgres.sql"),
            ),
        ] {
            let stmts: Vec<&str> = split_statements(sql)
                .into_iter()
                .filter(|s| has_executable_content(s))
                .collect();
            assert_eq!(
                stmts.len(),
                2,
                "admin {dialect} migration 005: expected 2 statements, got {stmts:?}"
            );
            assert!(is_alter_add_column(stmts[0]), "{dialect}: {}", stmts[0]);
        }
    }

    #[test]
    fn products_sql_splits_into_expected_chunks() {
        // Counts the executable statements in the products block SQL files.
        // 9 CREATE TABLE + 9 CREATE INDEX = 18 statements per backend (the
        // 2026-07 pre-production reset removed `pricing_templates`).
        let sql_sqlite = include_str!("blocks/products/migrations/001_products_schema.sqlite.sql");
        let sqlite_count = split_statements(sql_sqlite)
            .into_iter()
            .filter(|s| has_executable_content(s))
            .count();
        assert_eq!(
            sqlite_count, 18,
            "products sqlite migration: expected 18 statements, got {sqlite_count}"
        );

        let sql_postgres =
            include_str!("blocks/products/migrations/001_products_schema.postgres.sql");
        let postgres_count = split_statements(sql_postgres)
            .into_iter()
            .filter(|s| has_executable_content(s))
            .count();
        assert_eq!(
            postgres_count, 18,
            "products postgres migration: expected 18 statements, got {postgres_count}"
        );

        // 002 seeds the two default templates (one INSERT each) per backend.
        for (label, sql) in [
            (
                "sqlite",
                include_str!("blocks/products/migrations/002_default_templates.sqlite.sql"),
            ),
            (
                "postgres",
                include_str!("blocks/products/migrations/002_default_templates.postgres.sql"),
            ),
        ] {
            let count = split_statements(sql)
                .into_iter()
                .filter(|s| has_executable_content(s))
                .count();
            assert_eq!(
                count, 2,
                "products {label} migration 002: expected 2 statements, got {count}"
            );
        }
    }

    /// Reproduces the prod failure mode this commit fixes: a block's
    /// migration SQL runs once, the column gets added. A later cold start
    /// loses the `block_settings` row (e.g. fresh D1, or schema drift that
    /// dropped the row), the snapshot has no entry, so `apply_pending`
    /// can't early-return — it re-runs every statement, and `ALTER TABLE …
    /// ADD COLUMN` blows up with "duplicate column name".
    ///
    /// Before this fix, the entire migration batch aborted and
    /// `record` never stamped the row, leaving the block stuck in
    /// the same broken state on every subsequent cold start.
    #[tokio::test]
    async fn apply_pending_tolerates_duplicate_add_column_re_run() {
        let ctx = crate::test_support::TestContext::with_admin().await;

        // Pre-create the target table and the column the migration "wants"
        // to add — mimicking the prod schema after a previous successful
        // apply, with the tracking row since gone.
        wafer_core::clients::database::ddl(
            &ctx.fixture(),
            "CREATE TABLE IF NOT EXISTS dup_col_test (id TEXT PRIMARY KEY)",
        )
        .await
        .expect("setup: create table");
        wafer_core::clients::database::ddl(
            &ctx.fixture(),
            "ALTER TABLE dup_col_test ADD COLUMN name TEXT",
        )
        .await
        .expect("setup: add column");

        // Migration SQL re-asserts the same column. Without the fix this
        // statement returns "duplicate column name" and the batch aborts.
        let migration_sql = "\
            CREATE TABLE IF NOT EXISTS dup_col_test (id TEXT PRIMARY KEY);\n\
            ALTER TABLE dup_col_test ADD COLUMN name TEXT;\n\
        ";

        apply_pending(
            &ctx.clone().running_as("test/dup-add-column"),
            "test/dup-add-column",
            &[("001_dup_column", migration_sql)],
        )
        .await
        .expect("benign duplicate ALTER must not abort the batch");
    }

    /// Regression guard: only ALTER TABLE ADD COLUMN gets the duplicate
    /// tolerance. A real DDL failure (e.g. syntax error) still propagates,
    /// naming the block, the file and the statement, and leaves the list
    /// unrecorded so the next migrating build runs it again.
    #[tokio::test]
    async fn apply_pending_still_fails_on_non_duplicate_ddl_error() {
        let ctx = crate::test_support::TestContext::with_admin().await;

        // Garbled DDL — sqlite reports "syntax error". Must NOT be swallowed.
        let files = [
            (
                "001_good",
                "CREATE TABLE IF NOT EXISTS bad_ddl_test (id TEXT);",
            ),
            ("002_bad", "CREATE NONSENSE foo (id TEXT);"),
        ];
        let ctx = ctx.running_as("test/bad-ddl");
        let err = apply_pending(&ctx, "test/bad-ddl", &files)
            .await
            .expect_err("syntax error must propagate");
        assert!(
            err.starts_with("test/bad-ddl: migration `002_bad` failed on `CREATE NONSENSE foo"),
            "the error names the block, the file and the statement: {err}"
        );
        let rows = block_settings::list_all(&ctx)
            .await
            .expect("list block settings");
        assert!(
            rows.iter().all(|row| row.block_name != "test/bad-ddl"),
            "a failed list is not recorded as applied: {rows:?}"
        );
    }

    /// The pre-build runner keeps the same tracking as the in-runtime one: it
    /// applies a pending list once, stamps the hash the block's `Init` will
    /// compute, and does nothing on the next boot.
    #[tokio::test]
    async fn apply_pending_via_service_applies_once_and_stamps_the_lists_hash() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db: Arc<dyn DatabaseService> = Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open(
                tmp.path()
                    .join("pending.sqlite3")
                    .to_str()
                    .expect("utf-8 path"),
            )
            .expect("open sqlite"),
        );
        let admin = crate::blocks::admin::migrations::SQLITE_MIGRATIONS;
        apply_pending_via_service(&db, crate::blocks::admin::ADMIN_BLOCK_ID, admin)
            .await
            .expect("admin's list applies to an empty database");
        // A row only the first apply can have written: a re-run would insert
        // a second one.
        let files = [(
            "001_counted",
            "CREATE TABLE IF NOT EXISTS pending_runs (id TEXT);\n\
             INSERT INTO pending_runs (id) VALUES ('run');",
        )];
        for _ in 0..2 {
            apply_pending_via_service(&db, "test/pending", &files)
                .await
                .expect("apply the counted list");
        }
        let runs = db.count("pending_runs", &[]).await.expect("count runs");
        assert_eq!(runs, 1, "an applied list is not run again");
        let state = block_settings::load(&db)
            .await
            .expect("load block settings")
            .state("test/pending")
            .migration;
        assert_eq!(state.current_hash, migration_set_hash(&files));
    }

    /// The list's hash covers its SQL alone: the SHA-256 of the files' SQL
    /// joined with `\n`, whatever the files are called.
    #[test]
    fn the_list_hash_is_the_joined_sql_hash() {
        let files = [
            ("001_a", "CREATE TABLE a (id TEXT);"),
            ("002_b", "SELECT 1;"),
        ];
        assert_eq!(
            migration_set_hash(&files),
            sha256_hex("CREATE TABLE a (id TEXT);\nSELECT 1;")
        );
    }

    /// `/_deploy/prepare` applies every block's pending migrations in ONE
    /// Worker invocation, so on a fresh database the statement count of every
    /// SQLite migration file the crate ships has to fit well inside D1's
    /// per-invocation query limit ([`D1_QUERIES_PER_INVOCATION_DEFAULT`]),
    /// with the funnel's seeds, migration stamps and config reads in what is
    /// left. Half the limit is that line.
    ///
    /// Read from the files rather than the blocks' `SQLITE_MIGRATIONS`
    /// constants so the count covers every block whatever features this test
    /// is built with. If this fails, the funnel has to apply migrations over
    /// more than one invocation before the migration that crossed the line
    /// ships; raising the threshold would only move where a first deploy
    /// breaks.
    ///
    /// A guard: it passes on the shipped migrations by design, and fails only
    /// when they grow past the line.
    ///
    /// [`D1_QUERIES_PER_INVOCATION_DEFAULT`]: crate::config_vars::D1_QUERIES_PER_INVOCATION_DEFAULT
    #[test]
    fn a_fresh_databases_migrations_fit_half_of_one_d1_invocation() {
        let blocks = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/blocks");
        let mut total = 0usize;
        let mut per_block = Vec::new();
        for block in std::fs::read_dir(&blocks).expect("read src/blocks") {
            let dir = block.expect("block entry").path().join("migrations");
            let Ok(files) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut count = 0usize;
            for file in files {
                let path = file.expect("migration entry").path();
                let is_sqlite = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".sqlite.sql"));
                if !is_sqlite {
                    continue;
                }
                let sql = std::fs::read_to_string(&path).expect("read migration");
                count += split_statements(&sql)
                    .into_iter()
                    .filter(|stmt| has_executable_content(stmt))
                    .count();
            }
            total += count;
            per_block.push((dir, count));
        }
        let limit = crate::config_vars::D1_QUERIES_PER_INVOCATION_DEFAULT as usize;
        assert!(
            total > 0,
            "no SQLite migration found under {}",
            blocks.display()
        );
        assert!(
            total <= limit / 2,
            "a fresh database's migrations are {total} statements, more than half of the \
             {limit} D1 queries `/_deploy/prepare` may run in its one invocation: \
             {per_block:?}"
        );
    }
}
