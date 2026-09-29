//! Shared migration helper.
//!
//! Each block's `migrations::apply()` calls [`apply_if_blessed`], which:
//!
//! 1. Reads the block's `MigrationState` from the cached `BlockSettings`.
//! 2. Computes the SQL's SHA-256.
//! 3. If `current_hash` matches the code's hash → already applied, return.
//! 4. If `blessed_hash` matches OR the `IMPRESSPRESS_RUN_MIGRATIONS` env var is
//!    set to `"1"` → apply all statements via `db::ddl`, then upsert the
//!    block's row in `impresspress__admin__block_settings` with the new hash.
//! 5. Otherwise → log warning and return (operator must redeploy with
//!    `--run-migrations` to bless this schema).
//!
//! The statement splitter handles `;` outside `--` comments. Block comments
//! `/* ... */` and `;` inside string literals are not supported — the
//! canonical .sql files don't use either.
//!
//! # A shipped .sql file is immutable, comments included
//!
//! Step 2 hashes the file's **whole text**, so editing a `--` comment in a
//! migration that has already shipped changes its hash exactly as much as
//! editing a statement does. On every deployment that already applied it,
//! `current_hash` and `blessed_hash` then both differ from the code hash,
//! step 5 logs `schema drift` on every boot, and clearing that requires a
//! redeploy with `--run-migrations` — which re-runs that block's migrations
//! from 001.
//!
//! So do not tidy prose in a shipped migration, even to fix a comment that
//! names a since-renamed Rust item. Put the explanation in the block's
//! `migrations/mod.rs` beside the test that covers the migration, where it is
//! not hash-addressed and a reader is more likely to find it. Products'
//! `slug_collision_cannot_fail_020` is the worked example.

use wafer_core::clients::{config, database as db};
use wafer_run::{context::Context, ErrorCode, LifecycleEvent, LifecycleType, WaferError};
use wafer_sql_utils::Backend;

use crate::{
    features::{BlockSettings, MigrationState, BLOCK_SETTINGS_CONFIG_KEY},
    platform_state::block_settings::{self, BlockSettingsPatch},
};
// NOTE: `BlockSettings::state_for` parses only the requested block's entry
// out of the JSON map, avoiding the full-map materialization that
// `from_config_json` would do on every `apply_if_blessed` call.

/// Env-var name set by `impresspress --run-migrations` (native) or
/// `deploy-cloudflare.sh deploy --run-migrations` (CF).
pub const RUN_MIGRATIONS_KEY: &str = "IMPRESSPRESS_RUN_MIGRATIONS";

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

/// Read `WAFER_RUN_SHARED__DATABASE__BACKEND` from the config snapshot,
/// concatenate the matching per-backend SQL files, and forward to
/// [`apply_if_blessed`].
///
/// Consolidates the backend dispatch + concatenation boilerplate that
/// every block's `migrations/mod.rs::apply` previously open-coded:
///
/// ```ignore
/// pub async fn apply(ctx: &dyn Context) -> Result<(), String> {
///     migration_helper::apply_migrations(
///         ctx,
///         "impresspress/messages",
///         &[SQL_001_SQLITE],
///         &[SQL_001_POSTGRES],
///     )
///     .await
/// }
/// ```
///
/// `sqlite_files` and `postgres_files` are joined with `\n` separators —
/// the same shape `apply_if_blessed`'s statement splitter expects.
/// Backends other than `"postgres"` (case-insensitive) fall back to
/// `sqlite_files`, matching the `config::get_default(..., "sqlite")`
/// behavior every consumer used to spell out.
pub async fn apply_migrations(
    ctx: &dyn Context,
    block_name: &str,
    sqlite_files: &[&str],
    postgres_files: &[&str],
) -> Result<(), String> {
    let backend = db_backend(ctx)
        .await
        .map_err(|e| format!("{block_name}: read the database backend: {e}"))?;
    let files = match backend {
        Backend::Postgres => postgres_files,
        Backend::Sqlite => sqlite_files,
    };
    let sql = files.join("\n");
    apply_if_blessed(ctx, block_name, &sql).await
}

/// `lifecycle(Init)` body shared by every impresspress feature block: on the
/// [`Init`](LifecycleType::Init) event, apply the block's migrations and wrap
/// any failure in a [`WaferError`] tagged with the block name. Non-`Init`
/// events are a no-op.
///
/// Folds the three things every block's `lifecycle` open-coded around its
/// migrations:
///
/// 1. `if matches!(event.event_type, LifecycleType::Init) { … }`,
/// 2. deriving the SQLite `&[&str]` from the block's `SQLITE_MIGRATIONS`
///    `(basename, content)` pairs — the S3-S single schema source that also
///    feeds the Cloudflare D1 registry,
/// 3. mapping the `apply_migrations` `Err(String)` onto
///    `WaferError::new(ErrorCode::Internal, "<block> migrations: …")`.
///
/// `sqlite_migrations` is the block's `migrations::SQLITE_MIGRATIONS`
/// (`(basename, content)`); only the content half is executed (the basename is
/// for the D1 filename). `postgres_files` is the matching ordered list of
/// PostgreSQL-dialect scripts. The active dialect is selected inside
/// [`apply_migrations`] from `WAFER_RUN_SHARED__DATABASE__BACKEND`.
///
/// Blocks with extra `Init` work (admin's seed steps, llm's legacy-provider
/// copy + reload) call this first, then run their extra steps; blocks with no
/// migrations at all (the embedding wrappers) simply omit `lifecycle`.
pub async fn lifecycle_init(
    ctx: &dyn Context,
    event: &LifecycleEvent,
    block_name: &str,
    sqlite_migrations: &[(&str, &str)],
    postgres_files: &[&str],
) -> Result<(), WaferError> {
    if !matches!(event.event_type, LifecycleType::Init) {
        return Ok(());
    }
    let sqlite: Vec<&str> = sqlite_migrations.iter().map(|(_, sql)| *sql).collect();
    apply_migrations(ctx, block_name, &sqlite, postgres_files)
        .await
        .map_err(|e| WaferError::new(ErrorCode::Internal, format!("{block_name} migrations: {e}")))
}

/// Apply `sql` against `db::ddl` iff the operator has blessed it or
/// `IMPRESSPRESS_RUN_MIGRATIONS=1`. Idempotent across calls: returns early
/// once `current_hash` in the cached `BlockSettings` matches the SQL's hash.
///
/// `block_name` is the full block name (e.g. `"impresspress/files"`).
/// `sql` is the embedded migration SQL (usually `include_str!(...)`).
pub async fn apply_if_blessed(
    ctx: &dyn Context,
    block_name: &str,
    sql: &str,
) -> Result<(), String> {
    let code_hash = sha256_hex(sql);
    let state = read_state(ctx, block_name);
    // Read directly from the config snapshot (env-var sourced key). The
    // config block service is not involved — `IMPRESSPRESS_RUN_MIGRATIONS` is
    // an infra env var that populates the wafer config snapshot at boot,
    // never written to the DB. Tests populate it via `ctx.set_config(...)`.
    let run_requested = ctx.config_get(RUN_MIGRATIONS_KEY) == Some("1");

    if state.current_hash == code_hash {
        return Ok(());
    }

    // Fresh install (no previous apply) bootstraps without operator consent —
    // there's no prior schema to protect, and dev/test modes can't pass
    // `--run-migrations`. Operator gating still applies to SCHEMA CHANGES
    // (current_hash non-empty + different code_hash below); the browser gives
    // that consent on every boot, because loading its bundle is its deploy
    // (`impresspress-web`'s `RuntimeFactory::build`).
    let is_fresh = state.current_hash.is_empty();
    let should_apply = is_fresh || run_requested || state.blessed_hash == code_hash;
    if !should_apply {
        tracing::warn!(
            block = %block_name,
            current = %state.current_hash,
            blessed = %state.blessed_hash,
            code = %code_hash,
            "schema drift; redeploy with --run-migrations to apply"
        );
        return Ok(());
    }

    run_statements(sql, |stmt| db::ddl(ctx, stmt))
        .await
        .map_err(|failed| {
            tracing::warn!(
                block = %block_name,
                stmt = %failed.statement,
                err = %failed.error,
                "ddl failed",
            );
            format!("ddl failed on `{}`: {}", failed.statement, failed.error)
        })?;

    let new_state = MigrationState {
        current_hash: code_hash.clone(),
        blessed_hash: code_hash,
    };
    write_state(ctx, block_name, &new_state).await?;

    Ok(())
}

/// Apply migration DDL directly against a [`DatabaseService`], outside the
/// runtime/`Context` gate.
///
/// This is the **pre-wafer** counterpart of [`apply_if_blessed`]: the native
/// CLI must create the admin variables / block_settings tables *before* the
/// wafer exists (so it can seed the JWT secret and construct its immutable
/// crypto service), and it has no `Context` yet. It runs the migration-file SQL
/// through `db.exec_raw` — the migration-file-runner exception to the no-raw-SQL
/// rule (CLAUDE.md) — reusing the exact embedded `.sql` constants admin's
/// gated `Init` runs later, so there's a single schema source.
///
/// Idempotent like the gated path: `CREATE TABLE IF NOT EXISTS` no-ops on
/// re-run, and a duplicate `ALTER TABLE ADD COLUMN` (no `IF NOT EXISTS` for
/// columns on SQLite) is swallowed as a benign re-run. Every other DDL error
/// propagates. Admin's later gated `Init` re-asserts the same SQL (a no-op once
/// the rows are stamped), so the migration-state bookkeeping is unaffected.
pub async fn apply_ddl_via_service(
    db: &std::sync::Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
    sql_files: &[&str],
) -> Result<(), String> {
    for sql in sql_files {
        run_statements(sql, |stmt| db.exec_raw(stmt, &[]))
            .await
            .map_err(|failed| {
                format!(
                    "pre-wafer ddl failed on `{}`: {}",
                    failed.statement, failed.error
                )
            })?;
    }
    Ok(())
}

/// A migration statement the database refused, and why.
struct FailedStatement<'a> {
    statement: &'a str,
    error: String,
}

/// Run each statement of a migration batch through `exec`, in order, stopping
/// at the first failure.
///
/// The one statement loop behind both [`apply_if_blessed`] and
/// [`apply_ddl_via_service`], so what a replay test proves through the
/// pre-wafer runner holds for the gated one. `ALTER TABLE ... ADD COLUMN` is
/// non-idempotent on SQLite/D1, which have no `IF NOT EXISTS` for columns:
/// when an earlier run already added the column the re-run raises "duplicate
/// column", and that — only for an `ADD COLUMN` statement — is a benign no-op
/// so the rest of the batch still runs. Every other failure is returned.
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

/// Upsert the block's row in `impresspress__admin__block_settings` with the
/// new migration state. Preserves the `enabled` flag if the row already exists.
async fn write_state(
    ctx: &dyn Context,
    block_name: &str,
    state: &MigrationState,
) -> Result<(), String> {
    block_settings::upsert_fields(
        ctx,
        block_name,
        BlockSettingsPatch {
            current_hash: Some(state.current_hash.clone()),
            blessed_hash: Some(state.blessed_hash.clone()),
            ..Default::default()
        },
    )
    .await
    .map_err(|e| format!("block_settings upsert: {e}"))
}

/// Compute a SHA-256 hex digest. Re-exported for callers (e.g.
/// `admin::settings::seed_defaults`) that hash-gate against a payload other
/// than SQL bytes but want to share the same digest algorithm with
/// `apply_if_blessed`.
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

/// `true` when `stmt`'s first executable token sequence is
/// `ALTER TABLE … ADD COLUMN`. Case-insensitive, comment-tolerant.
///
/// Used to gate the duplicate-column-error tolerance in
/// `apply_if_blessed` — we only swallow the benign duplicate on this
/// specific statement shape, not on every DDL.
fn is_alter_add_column(stmt: &str) -> bool {
    let upper: String = stmt
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("--"))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase();
    let collapsed = upper.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.starts_with("ALTER TABLE ") && collapsed.contains(" ADD COLUMN ")
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
    /// dropped the row), the snapshot has no entry, so `apply_if_blessed`
    /// can't early-return — it re-runs every statement, and `ALTER TABLE …
    /// ADD COLUMN` blows up with "duplicate column name".
    ///
    /// Before this fix, the entire migration batch aborted and
    /// `write_state` never stamped the row, leaving the block stuck in
    /// the same broken state on every subsequent cold start.
    #[tokio::test]
    async fn apply_if_blessed_tolerates_duplicate_add_column_re_run() {
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

        apply_if_blessed(
            &ctx.clone().running_as("test/dup-add-column"),
            "test/dup-add-column",
            migration_sql,
        )
        .await
        .expect("benign duplicate ALTER must not abort the batch");
    }

    /// Regression guard: only ALTER TABLE ADD COLUMN gets the duplicate
    /// tolerance. A real DDL failure (e.g. syntax error) still propagates.
    #[tokio::test]
    async fn apply_if_blessed_still_fails_on_non_duplicate_ddl_error() {
        let ctx = crate::test_support::TestContext::with_admin().await;

        // Garbled DDL — sqlite reports "syntax error". Must NOT be swallowed.
        let bad_sql = "CREATE NONSENSE foo (id TEXT);";
        let err = apply_if_blessed(&ctx, "test/bad-ddl", bad_sql)
            .await
            .expect_err("syntax error must propagate");
        assert!(
            err.contains("ddl failed"),
            "expected `ddl failed` in error string, got: {err}"
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
