//! Shared migration runner.
//!
//! A block's migrations are an ordered list of `.sql` files, each a
//! [`MigrationFile`] `(basename, sql)` pair. Every block applies its list from
//! `lifecycle(Init)` through [`lifecycle_init`] (auth's service `init` calls
//! [`apply_migrations`] directly), which picks the list for the active SQL
//! dialect and hands it to [`apply_pending`].
//!
//! # The ledger: one row per applied file
//!
//! What a database has applied is recorded per file, in
//! `impresspress__admin__applied_migrations`
//! ([`crate::platform_state::applied_migrations`]): the block, the file's
//! basename, its [`content_hash`] as applied, and how the row came to be
//! (`ran`, `adopted`, `replayed`). A file added after the ledger shipped is
//! applied as ONE transaction (`db::ddl_transaction`): its ledger row first,
//! then its statements. The whole file commits with its row, or nothing of it
//! does, so a row exists exactly when its file's statements took effect, on
//! every backend. Files from before the ledger are applied as they were
//! written to be ("Frozen files", below).
//!
//! Beside it, the block's `block_settings.current_hash` keeps the meaning it
//! always had: the [`migration_set_hash`] of the list this database last had
//! fully applied. It is the fast path, and it is what a database from before
//! the ledger is classified by (below).
//!
//! # A run, step by step
//!
//! 1. **Fast path.** `current_hash` (read off the config snapshot, so a
//!    settled block reads nothing) equals [`migration_set_hash`] of the list:
//!    return.
//! 2. A build that does not apply migrations (Cloudflare's request path, see
//!    below) logs `schema drift` and returns, unless the block has no
//!    `current_hash` at all: a block that never applied anything has no
//!    schema to protect, so even such a build creates it.
//! 3. Ensure the ledger table (`CREATE TABLE IF NOT EXISTS`), then read the
//!    block's rows.
//! 4. **Plan.** Refuse, before running anything for the block, when the
//!    ledger and the code disagree ([`Refusal`]):
//!    - a recorded file's [`content_hash`] differs from the shipped file of
//!      that name (**content changed**);
//!    - the ledger names a file the code does not ship (**database ahead**);
//!    - a recorded file follows one that is not recorded (**gap**).
//! 5. A block with no ledger rows is classified by its `current_hash` (see
//!    "Databases from before the ledger").
//! 6. Run each pending file, in order: a file from after the ledger as its
//!    own transaction, a frozen file as the next section says.
//! 7. Stamp `current_hash` with the list's hash, through the typed
//!    `block_settings` write. On Cloudflare that write also drops the KV row
//!    cache's all-rows key of `block_settings` (`cache_key::invalidate_keys`),
//!    which no raw statement does: a file that rewrites that table in raw SQL,
//!    like admin's 009, reaches an older version's cached reads through this
//!    stamp, or through the deploy funnel's read-through reads of the table
//!    that follow admin's `Init`. See the 009 note in
//!    `blocks/admin/migrations/mod.rs`. If the stamp fails, the next run finds
//!    every file recorded and only stamps.
//!
//! A `block_settings` row that was lost on its own is therefore recovered by
//! step 7 alone: the ledger still records every file, nothing is pending, and
//! the row is stamped again.
//!
//! # Frozen files
//!
//! A block's files up to its [`FROZEN_THROUGH`] mark — every file it shipped
//! before the ledger — were written for the runner before it, which ran a
//! block's whole list again whenever it changed, one statement at a time, and
//! took a duplicate `ALTER TABLE … ADD COLUMN` as already applied. Some of
//! them depend on that on their FIRST run, not only on a re-run:
//!
//! - auth's 016 opens with `ADD COLUMN role` so that its `DROP COLUMN role`
//!   can be re-run on SQLite, and 001 creates that column, so on SQLite the
//!   `ADD` is a duplicate on every database 016 has not run on;
//! - admin's 009 adds `blessed_hash`, which a database created before #145
//!   already has (its old 001 created it);
//! - a native or browser database whose typed writes added a column before
//!   the migration that adds it shipped (auth's 010 is that list) meets that
//!   migration's `ADD COLUMN` as a duplicate.
//!
//! In one transaction a refused statement fails the whole file, so the
//! runner applies a frozen file the way it was written to be applied: its
//! statements one at a time, a duplicate `ADD COLUMN` taken as already
//! applied, and its ledger row written once they all succeeded. A frozen file
//! is therefore not atomic: one that fails leaves its earlier statements
//! applied and is not recorded, and the next run runs it again from its
//! start, which every frozen file is written to survive (the "re-runnable"
//! notes in each block's `migrations/mod.rs`). Every file added after the
//! ledger runs once, in its transaction, and none of that applies to it.
//!
//! # A failed file is decided by the ledger, never by its error
//!
//! When a file fails — its transaction, or a frozen file's statement or
//! ledger row — whatever the error, the runner reads the block's ledger once
//! more:
//!
//! - **the file is recorded with the same hash:** another migrator applied
//!   it (the ledger insert is the claim, and the second claim on a file is
//!   refused with its transaction), or this run's own commit landed although
//!   the backend reported a failure after it. Continue with the next file.
//! - **recorded with another hash:** the content-changed refusal.
//! - **not recorded:** the file was refused. Nothing of a file from after
//!   the ledger remains; a frozen file's earlier statements do (above). The
//!   error names the block, the file, the backend's classification, the
//!   failing statement when the backend says which (D1's `batch()` never
//!   does) and its message.
//!
//! Neither the error's text nor the statement index decides anything: a
//! claim conflict on Postgres names a constraint and on SQLite a table, a D1
//! error names no statement at all, and only the ledger says whether the file
//! is applied.
//!
//! These errors carry the database's own text. They are `Init` errors, for
//! the deploy report, the boot's refusal and the logs; a block must not copy
//! one into an HTTP response body.
//!
//! # Databases from before the ledger
//!
//! A block with no ledger rows is in exactly one of three classes, decided by
//! its `current_hash`:
//!
//! - **fresh** (`current_hash` empty): every file runs.
//! - **adopt** (`current_hash` equals [`migration_set_hash`] of the first `k`
//!   files, `k >= 1`): files `1..=k` are recorded as `adopted` without
//!   running, in one transaction, and the rest run. The runner from before
//!   the ledger stamped a hash only after the whole list it hashed had run,
//!   so a hash equal to a prefix of today's list proves exactly those files
//!   ran, with today's text.
//! - **replay** (any other value, including the literal `applying` a crashed
//!   run from before #145 left): the hash proves nothing about which text
//!   ran. That class needs the legacy replay, which ships separately; until
//!   it does, such a block is refused ([`Refusal::NeedsReplay`]).
//!
//! A fresh block whose schema already exists — both its `block_settings` row
//! and its ledger rows were lost — runs its frozen files again over that
//! schema, which they are written to survive, and records them. Its first
//! file from after the ledger that meets the existing schema (typically a
//! duplicate `ADD COLUMN`) rolls back, and the runner names that state
//! ([`Refusal::TrackingLost`]) and its remedy instead of the bare duplicate.
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
//!   The ledger's claim is defence in depth behind that lock.
//! - **Cloudflare**: the `/_deploy/prepare` / `/_deploy/init` funnel, before
//!   the new version is promoted. A failure fails the deploy. On D1 a file's
//!   transaction is one `db.batch()`; a frozen file's statements are one
//!   query each, and one the statement budget refuses part-way leaves the
//!   file unrecorded for the next deploy to run again.
//! - **browser**: every runtime build, because loading a bundle is its
//!   deploy (`impresspress_browser::boot`'s `BrowserServices::builder`).
//!
//! Cloudflare's request-path builds are the only ones that do not, so step 2
//! is reached only there, by a version serving before its deploy funnel ran.
//!
//! # A shipped file's statements are immutable
//!
//! A file's ledger row holds the [`content_hash`] of the statements that ran,
//! and step 4 refuses a database whose recorded hash differs from the code's:
//! the native server refuses to start, a deploy fails, and a browser build
//! refusing admin's list stops with an error the boot shell reports without
//! erasing anything (`impresspress_browser::boot::MIGRATION_REFUSED`), while
//! any other block's refusal fails that block alone. So a change to a
//! shipped migration goes in a NEW file. [`content_hash`] covers the statements only, so editing a
//! whole-line `--` comment does not move it.
//!
//! Comments are still not free: [`migration_set_hash`] covers the whole text,
//! and the adopt class matches a database from before the ledger by it, so a
//! comment edit to a shipped file turns every such database of that block
//! from adopt into replay. Until the legacy replay is removed, put the
//! explanation in the block's `migrations/mod.rs` instead, beside the test
//! that covers the migration. Products' `slug_collision_cannot_fail_020` is
//! the worked example.
//!
//! The statement splitter handles `;` outside `--` comments. Block comments
//! `/* ... */` and `;` inside string literals are not supported — the
//! canonical .sql files don't use either.

use std::sync::Arc;

use wafer_core::{
    clients::{config, database as db},
    interfaces::database::service::{
        DatabaseError, DatabaseService, RawStatement, TransactionError,
    },
};
use wafer_run::{context::Context, ErrorCode, LifecycleEvent, LifecycleType, WaferError};
use wafer_sql_utils::Backend;

use crate::{
    features::{BlockSettings, MigrationState, BLOCK_SETTINGS_CONFIG_KEY},
    platform_state::{
        applied_migrations::{self, AppliedMigration, Origin},
        block_settings::{self, BlockSettingsPatch},
    },
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
/// `migrations::POSTGRES_MIGRATIONS`. The basename is the file's key in the
/// ledger, so it never changes once the file has shipped.
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
    let backend = config::get_default(ctx, DATABASE_BACKEND_KEY, "sqlite").await?;
    Ok(backend_named(&backend))
}

/// The [`Backend`] a dialect name selects: `"postgres"` (case-insensitive)
/// is [`Backend::Postgres`], anything else [`Backend::Sqlite`] — the rule
/// [`db_backend`] applies to [`DATABASE_BACKEND_KEY`], for a caller that holds
/// the name itself (the native server's `IMPRESSPRESS_DB_TYPE`).
pub fn backend_named(name: &str) -> Backend {
    if name.eq_ignore_ascii_case("postgres") {
        Backend::Postgres
    } else {
        Backend::Sqlite
    }
}

/// Pick the block's migration list for the active SQL dialect
/// (`WAFER_RUN_SHARED__DATABASE__BACKEND`, read through [`db_backend`]) and
/// forward it to [`apply_pending`].
///
/// ```ignore
/// pub async fn apply(ctx: &dyn Context) -> Result<(), MigrationError> {
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
) -> Result<(), MigrationError> {
    let backend = db_backend(ctx).await.map_err(|e| {
        MigrationError::step(block_name, "read the database backend", StepError::from(e))
    })?;
    let files = match backend {
        Backend::Postgres => postgres_files,
        Backend::Sqlite => sqlite_files,
    };
    apply_pending(ctx, block_name, files).await
}

/// `lifecycle(Init)` body shared by every impresspress feature block: on the
/// [`Init`](LifecycleType::Init) event, apply the block's migrations and turn
/// a failure into a [`WaferError`] ([`MigrationError::to_wafer_error`]).
/// Non-`Init` events are a no-op.
///
/// `sqlite_migrations` / `postgres_migrations` are the block's
/// `migrations::SQLITE_MIGRATIONS` / `migrations::POSTGRES_MIGRATIONS`; the
/// active dialect is selected inside [`apply_migrations`].
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
        .map_err(|e| e.to_wafer_error())
}

/// Apply `block_name`'s pending `files` through the block's context, when
/// this build may apply them (module docs, "A run, step by step"). Returns
/// at once when `current_hash` in the snapshot's `BlockSettings` matches the
/// list's hash.
///
/// `block_name` is the full block name (e.g. `"impresspress/files"`); every
/// error names it.
pub async fn apply_pending(
    ctx: &dyn Context,
    block_name: &str,
    files: &[MigrationFile<'_>],
) -> Result<(), MigrationError> {
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

    let backend = db_backend(ctx).await.map_err(|e| {
        MigrationError::step(block_name, "read the database backend", StepError::from(e))
    })?;
    let outcome = Ledger {
        db: Db::Ctx(ctx),
        backend,
        block: block_name,
    }
    .apply(files, &state.current_hash, &code_hash)
    .await;
    if let Err(e) = &outcome {
        tracing::warn!(error = %e, "migration failed");
    }
    outcome
}

/// Apply `block_name`'s pending migrations straight through a
/// [`DatabaseService`], before the runtime exists.
///
/// The native server's counterpart of [`apply_pending`], for the one list
/// it must apply pre-build: admin's, whose tables hold the variables and
/// block settings the server reads to build its runtime. Same ledger, same
/// plan, same stamp — it reads the block's `block_settings` row, returns
/// when its `current_hash` matches [`migration_set_hash`], and otherwise
/// runs the pending files — so the block's own `Init` then finds the list
/// applied and skips it. A native boot always applies migrations, so there
/// is no `IMPRESSPRESS_RUN_MIGRATIONS` gate to consult. `backend` is the
/// dialect `files` are written in, which renders the ledger's own statements.
///
/// The statements go through `db.exec_raw` / `db.exec_raw_transaction` — the
/// migration-file-runner exception to the no-raw-SQL rule (CLAUDE.md).
pub async fn apply_pending_via_service(
    db: &Arc<dyn DatabaseService>,
    backend: Backend,
    block_name: &str,
    files: &[MigrationFile<'_>],
) -> Result<(), MigrationError> {
    let code_hash = migration_set_hash(files);
    let current_hash = block_settings::load(db)
        .await
        .map_err(|e| {
            MigrationError::step(
                block_name,
                "read its block_settings row",
                StepError::from(e),
            )
        })?
        .state(block_name)
        .migration
        .current_hash;
    if current_hash == code_hash {
        return Ok(());
    }
    Ledger {
        db: Db::Service(db),
        backend,
        block: block_name,
    }
    .apply(files, &current_hash, &code_hash)
    .await
}

/// Run migration SQL straight through a [`DatabaseService`], with no
/// tracking at all: every statement of every file, every time, one at a
/// time, with a duplicate `ADD COLUMN` treated as already applied.
///
/// For test fixtures that build a schema up to a chosen point, and for
/// tests of the files a legacy replay re-runs over the schema they already
/// built. A deployment never calls it: it applies migrations through
/// [`apply_pending`] / [`apply_pending_via_service`], which run each file
/// once, atomically, and record it.
pub async fn apply_ddl_via_service(
    db: &Arc<dyn DatabaseService>,
    sql_files: &[&str],
) -> Result<(), String> {
    for sql in sql_files {
        let statements = executable_statements(sql);
        run_statements(&statements, |stmt| db.exec_raw(stmt, &[]))
            .await
            .map_err(|failed| format!("ddl failed on `{}`: {}", failed.statement, failed.error))?;
    }
    Ok(())
}

/// The hash that identifies a block's migration list: SHA-256 hex of the
/// files' SQL joined with `\n`, in order. The basenames take no part in it.
///
/// What `block_settings.current_hash` holds once a list is fully applied,
/// and what a database from before the ledger is adopted by.
pub fn migration_set_hash(files: &[MigrationFile<'_>]) -> String {
    let joined = files
        .iter()
        .map(|(_, sql)| *sql)
        .collect::<Vec<_>>()
        .join("\n");
    sha256_hex(&joined)
}

/// The statements a migration file runs, in order: the file split on `;`
/// outside `--` comments, each piece's whole-line `--` comments dropped and
/// the rest trimmed, and pieces with nothing left skipped. Nothing inside a
/// statement is normalised: its inner whitespace, literals and trailing
/// inline comments are kept as written.
///
/// These are exactly the statements the runner sends — in one transaction
/// for a file added after the ledger, one at a time for a frozen file — and
/// exactly what [`content_hash`] covers.
pub fn executable_statements(sql: &str) -> Vec<String> {
    split_statements(sql)
        .into_iter()
        .map(|piece| {
            piece
                .lines()
                .filter(|line| !line.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        })
        .filter(|statement| !statement.is_empty())
        .collect()
}

/// A migration file's content hash, the one its ledger row records: SHA-256
/// hex of its [`executable_statements`] joined with `;\n`. An edit to a
/// whole-line comment leaves it alone; any edit to a statement moves it.
pub fn content_hash(sql: &str) -> String {
    sha256_hex(&executable_statements(sql).join(";\n"))
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A block's migrations that did not apply. Its `Display` is the error every
/// runner returns, so a boot that refuses to start, a deploy report and a
/// browser's console say exactly this. It always names the block.
#[derive(Debug)]
pub struct MigrationError {
    block: String,
    /// Boxed: the error travels through every `Init`'s `Result`, and its
    /// largest variant carries a file's statements.
    kind: Box<MigrationErrorKind>,
}

#[derive(Debug)]
enum MigrationErrorKind {
    /// The ledger and the code disagree; nothing ran for the block.
    Refused(Refusal),
    /// A file's transaction failed and the ledger does not record the file:
    /// nothing of it remains.
    FileFailed {
        file: String,
        /// The statement text at each index of the transaction the file was
        /// sent as: index 0 is its ledger row.
        statements: Vec<String>,
        failure: TxFailure,
    },
    /// A frozen file failed and the ledger does not record it: the
    /// statements before the failing one stay applied, and the next run runs
    /// the file again from its start.
    FrozenFileFailed {
        file: String,
        failure: FrozenFailure,
    },
    /// Reading or writing the migration record itself failed.
    Step { step: String, error: StepError },
}

/// Why the runner refused a block before running anything for it. Each says
/// what the operator can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The ledger records `file` with `recorded`, and the code ships it with
    /// `shipped`: a shipped migration's statements were edited.
    ContentChanged {
        file: String,
        recorded: String,
        shipped: String,
    },
    /// The ledger records files this code does not ship: the database was
    /// migrated by a newer build.
    DatabaseAhead { files: Vec<String> },
    /// `applied` is recorded although `pending`, which comes before it, is
    /// not: the ledger is not a prefix of the code's list.
    Gap { applied: String, pending: String },
    /// The block has no migration record at all, yet `file` met a schema that
    /// already exists.
    TrackingLost { file: String, message: String },
    /// The block's `current_hash` is from before the ledger and matches no
    /// prefix of today's list.
    NeedsReplay { current_hash: String },
}

impl MigrationError {
    fn refused(block: &str, refusal: Refusal) -> Self {
        Self {
            block: block.to_string(),
            kind: Box::new(MigrationErrorKind::Refused(refusal)),
        }
    }

    fn step(block: &str, step: &str, error: StepError) -> Self {
        Self {
            block: block.to_string(),
            kind: Box::new(MigrationErrorKind::Step {
                step: step.to_string(),
                error,
            }),
        }
    }

    /// The refusal, when the runner refused the block because its ledger and
    /// the code disagree: an outcome of the code and the stored record, never
    /// of the data the files would have touched.
    pub fn refusal(&self) -> Option<&Refusal> {
        match self.kind.as_ref() {
            MigrationErrorKind::Refused(refusal) => Some(refusal),
            MigrationErrorKind::FileFailed { .. }
            | MigrationErrorKind::FrozenFileFailed { .. }
            | MigrationErrorKind::Step { .. } => None,
        }
    }

    /// The code a failed `Init` answers with: `FailedPrecondition` for a
    /// [`Refusal`]; the database's own classification for a file or a step
    /// that failed (`Unavailable` stays `Unavailable`, so the runtime retries
    /// the `Init`; a statement-budget refusal keeps its code and detail
    /// code); `Internal` otherwise.
    pub fn code(&self) -> ErrorCode {
        match self.kind.as_ref() {
            MigrationErrorKind::Refused(_) => ErrorCode::FailedPrecondition,
            MigrationErrorKind::FileFailed { failure, .. } => failure.code,
            MigrationErrorKind::FrozenFileFailed { failure, .. } => match failure {
                FrozenFailure::Statement { error, .. } => error.code,
                FrozenFailure::Record { failure, .. } => failure.code,
            },
            MigrationErrorKind::Step { error, .. } => error.code,
        }
    }

    fn detail_code(&self) -> Option<&str> {
        match self.kind.as_ref() {
            MigrationErrorKind::FileFailed { failure, .. } => failure.detail_code.as_deref(),
            MigrationErrorKind::FrozenFileFailed { failure, .. } => match failure {
                FrozenFailure::Statement { error, .. } => error.detail_code.as_deref(),
                FrozenFailure::Record { failure, .. } => failure.detail_code.as_deref(),
            },
            MigrationErrorKind::Step { error, .. } => error.detail_code.as_deref(),
            MigrationErrorKind::Refused(_) => None,
        }
    }

    /// This error as the [`WaferError`] a failed `Init` returns: [`Self::code`]
    /// and this error's text, with the statement-budget detail code when the
    /// failure was one.
    pub fn to_wafer_error(&self) -> WaferError {
        let error = WaferError::new(self.code(), self.to_string());
        match self.detail_code() {
            Some(detail) => error.with_detail_code(detail),
            None => error,
        }
    }
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let block = &self.block;
        match self.kind.as_ref() {
            MigrationErrorKind::Refused(refusal) => write!(f, "{block}: {refusal}"),
            MigrationErrorKind::Step { step, error } => {
                write!(f, "{block}: {step}: {}", error.message)
            }
            MigrationErrorKind::FrozenFileFailed { file, failure } => {
                let (statement, code, message) = match failure {
                    FrozenFailure::Statement { statement, error } => {
                        (statement, error.code, &error.message)
                    }
                    FrozenFailure::Record { statement, failure } => {
                        (statement, failure.code, &failure.message)
                    }
                };
                write!(
                    f,
                    "{block}: migration `{file}` failed on `{statement}` ({code:?}): {message}. \
                     It is from before the per-file ledger and runs one statement at a time: \
                     the statements before this one stay applied, and the next run runs the \
                     file again from its start"
                )
            }
            MigrationErrorKind::FileFailed {
                file,
                statements,
                failure,
            } => {
                let sent = statements.len();
                match failure.detail_code.as_deref() {
                    Some(wafer_block::wire::database::STATEMENT_BUDGET_EXHAUSTED) => write!(
                        f,
                        "{block}: migration `{file}` was not sent: its {sent} statements \
                         (its ledger row included, admitted whole) do not fit what this \
                         invocation has left of its statement budget; earlier files stay \
                         recorded, and a fresh invocation resumes at this one: {}",
                        failure.message
                    ),
                    Some(wafer_block::wire::database::STATEMENT_BUDGET_EXCEEDS_LIMIT) => write!(
                        f,
                        "{block}: migration `{file}` was not sent: its {sent} statements (its \
                         ledger row included) are more than one invocation may run; deploy it \
                         once with a statement budget of at least {sent}: {}",
                        failure.message
                    ),
                    _ => match failure.statement.and_then(|index| statements.get(index)) {
                        Some(statement) => write!(
                            f,
                            "{block}: migration `{file}` failed on `{statement}` and was rolled \
                             back ({:?}): {}",
                            failure.code, failure.message
                        ),
                        None => write!(
                            f,
                            "{block}: migration `{file}` failed and was rolled back ({:?}; this \
                             backend does not say which statement): {}",
                            failure.code, failure.message
                        ),
                    },
                }
            }
        }
    }
}

impl std::error::Error for MigrationError {}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ContentChanged {
                file,
                recorded,
                shipped,
            } => write!(
                f,
                "migration `{file}` was applied with content hash {recorded}, but this build \
                 ships it with {shipped}: a shipped migration's statements were edited. Nothing \
                 was run. Restore the file as it shipped and put the change in a new migration"
            ),
            Self::DatabaseAhead { files } => write!(
                f,
                "this database has applied migrations this build does not ship ({}): it was \
                 migrated by a newer build. Nothing was run. Downgrading across migrations is \
                 not supported: run the newer build again, or restore a backup taken before the \
                 upgrade. Deleting their ledger rows is not a remedy",
                files.join(", ")
            ),
            Self::Gap { applied, pending } => write!(
                f,
                "the migration ledger records `{applied}` but not `{pending}`, which comes \
                 before it, so it is not a record this runner wrote. Nothing was run. Restore \
                 {} from a backup",
                applied_migrations::TABLE
            ),
            Self::TrackingLost { file, message } => write!(
                f,
                "has no migration record (no block_settings hash, no ledger rows) but its schema \
                 already exists: its tracking was lost. Migration `{file}` met that schema and \
                 was rolled back ({message}). Restore {} and {} from a backup taken before the \
                 loss, or recreate the database",
                block_settings::TABLE,
                applied_migrations::TABLE
            ),
            Self::NeedsReplay { current_hash } => write!(
                f,
                "its migration record ({current_hash}) is from before the per-file ledger and \
                 matches no prefix of this build's list, so it needs the legacy replay, which \
                 this build does not carry. Nothing was run. Deploy a build that carries the \
                 replay"
            ),
        }
    }
}

/// A database operation of the runner's own that failed: the code it was
/// classified as, the statement-budget detail code if it was one, and the
/// message.
#[derive(Debug, Clone)]
struct StepError {
    code: ErrorCode,
    detail_code: Option<String>,
    message: String,
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<WaferError> for StepError {
    fn from(e: WaferError) -> Self {
        Self {
            code: e.code,
            detail_code: e.detail_code().map(str::to_string),
            message: e.message,
        }
    }
}

impl From<DatabaseError> for StepError {
    fn from(e: DatabaseError) -> Self {
        Self {
            code: e.code(),
            detail_code: e.detail_code().map(str::to_string),
            message: e.to_string(),
        }
    }
}

/// A file's transaction that did not commit, as either path reports it: the
/// classification, the statement-budget detail code if it was a budget
/// refusal, the failing statement's index when the backend says which, and
/// the message. Diagnostic only: what happens next is decided by the ledger.
#[derive(Debug, Clone)]
struct TxFailure {
    code: ErrorCode,
    detail_code: Option<String>,
    statement: Option<usize>,
    message: String,
}

impl From<WaferError> for TxFailure {
    fn from(e: WaferError) -> Self {
        Self {
            statement: wafer_block::wire::database::failed_statement_index(&e),
            code: e.code,
            detail_code: e.detail_code().map(str::to_string),
            message: e.message,
        }
    }
}

impl From<TransactionError> for TxFailure {
    fn from(e: TransactionError) -> Self {
        Self {
            code: e.error.code(),
            detail_code: e.error.detail_code().map(str::to_string),
            statement: e.statement,
            message: e.error.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// The ledger runner
// ---------------------------------------------------------------------------

/// The database the runner reaches: the block's own context (under WRAP,
/// every block's `Init`) or the platform service (native's pre-build apply of
/// admin's list).
enum Db<'a> {
    Ctx(&'a dyn Context),
    Service(&'a Arc<dyn DatabaseService>),
}

impl Db<'_> {
    async fn exec(&self, sql: &str) -> Result<(), StepError> {
        match self {
            Self::Ctx(ctx) => db::ddl(*ctx, sql).await.map(drop).map_err(StepError::from),
            Self::Service(db) => db
                .exec_raw(sql, &[])
                .await
                .map(drop)
                .map_err(StepError::from),
        }
    }

    async fn ledger(&self, block: &str) -> Result<Vec<AppliedMigration>, StepError> {
        match self {
            Self::Ctx(ctx) => applied_migrations::list_for_block(*ctx, block)
                .await
                .map_err(StepError::from),
            Self::Service(db) => applied_migrations::list_for_block_via_service(db, block)
                .await
                .map_err(StepError::from),
        }
    }

    async fn transaction(&self, statements: &[RawStatement]) -> Result<(), TxFailure> {
        match self {
            Self::Ctx(ctx) => db::ddl_transaction(*ctx, statements)
                .await
                .map(drop)
                .map_err(TxFailure::from),
            Self::Service(db) => db
                .exec_raw_transaction(statements)
                .await
                .map(drop)
                .map_err(TxFailure::from),
        }
    }

    async fn stamp(&self, block: &str, code_hash: &str) -> Result<(), StepError> {
        let patch = BlockSettingsPatch {
            current_hash: Some(code_hash.to_string()),
            ..Default::default()
        };
        match self {
            Self::Ctx(ctx) => block_settings::upsert_fields(*ctx, block, patch)
                .await
                .map_err(StepError::from),
            Self::Service(db) => block_settings::upsert_fields_via_service(db, block, patch)
                .await
                .map_err(StepError::from),
        }
    }
}

/// The last file of each block's list when the per-file ledger shipped: the
/// block's **frozen** files are that one and every file before it (module
/// docs, "Frozen files").
///
/// Spelled once, here, and never moved: a file added after the ledger is
/// never frozen. A block that is not listed has no frozen file.
pub const FROZEN_THROUGH: &[(&str, &str)] = &[
    (
        "impresspress/admin",
        "009_block_settings_clear_blessed_hash",
    ),
    ("wafer-run/auth", "016_roles_are_grants"),
    ("impresspress/dev", "004_seed_llms_text"),
    ("impresspress/files", "005_object_blob_key"),
    ("impresspress/legalpages", "002_legalpages_version_numbers"),
    ("impresspress/llm", "002_provider_max_tokens_field"),
    ("impresspress/messages", "002_owner_id"),
    ("impresspress/products", "023_canonical_currency"),
    ("impresspress/signal", "001_signal_rooms"),
    ("impresspress/tickets", "001_tickets_schema"),
    ("impresspress/userportal", "001_userportal_schema"),
    ("impresspress/vector", "001_vector_schema"),
];

/// The numeric `NNN` a basename starts with. Every shipped basename has one,
/// strictly increasing within its block (the guard
/// `every_migration_ships_in_both_dialects_with_increasing_prefixes`).
fn sequence_number(basename: &str) -> Option<u32> {
    basename.split_once('_')?.0.parse().ok()
}

/// Whether `file` of `block` is frozen: at or before the block's
/// [`FROZEN_THROUGH`] entry. Compared by sequence number, so a list that
/// stops before the freeze mark — a test fixture's prefix — is frozen whole,
/// and a file added after it never is.
pub fn is_frozen(block: &str, file: &str) -> bool {
    FROZEN_THROUGH
        .iter()
        .find(|(name, _)| *name == block)
        .and_then(|(_, last)| Some((sequence_number(file)?, sequence_number(last)?)))
        .is_some_and(|(file, last)| file <= last)
}

/// How a block with no ledger rows is applied (module docs, "Databases from
/// before the ledger").
#[derive(Debug, PartialEq, Eq)]
enum Class {
    Fresh,
    /// Files `..k` ran with today's text.
    Adopt(usize),
    Replay,
}

fn classify(current_hash: &str, files: &[MigrationFile<'_>]) -> Class {
    if current_hash.is_empty() {
        return Class::Fresh;
    }
    (1..=files.len())
        .find(|&k| migration_set_hash(&files[..k]) == current_hash)
        .map_or(Class::Replay, Class::Adopt)
}

/// Where the code's list resumes against the block's ledger rows: the index
/// of the first file not recorded, or the [`Refusal`] that stops the block.
fn plan(files: &[MigrationFile<'_>], rows: &[AppliedMigration]) -> Result<usize, Refusal> {
    let ahead: Vec<String> = rows
        .iter()
        .filter(|row| !files.iter().any(|(name, _)| *name == row.file_name))
        .map(|row| row.file_name.clone())
        .collect();
    if !ahead.is_empty() {
        let mut files = ahead;
        files.sort();
        return Err(Refusal::DatabaseAhead { files });
    }
    let recorded = |name: &str| rows.iter().find(|row| row.file_name == name);
    for &(name, sql) in files {
        if let Some(row) = recorded(name) {
            let shipped = content_hash(sql);
            if row.content_sha256 != shipped {
                return Err(Refusal::ContentChanged {
                    file: name.to_string(),
                    recorded: row.content_sha256.clone(),
                    shipped,
                });
            }
        }
    }
    let first_pending = files
        .iter()
        .position(|(name, _)| recorded(name).is_none())
        .unwrap_or(files.len());
    if let Some((applied, _)) = files[first_pending..]
        .iter()
        .find(|(name, _)| recorded(name).is_some())
    {
        return Err(Refusal::Gap {
            applied: (*applied).to_string(),
            pending: files[first_pending].0.to_string(),
        });
    }
    Ok(first_pending)
}

/// One block's run over its ledger: steps 3-7 of the module docs.
struct Ledger<'a> {
    db: Db<'a>,
    backend: Backend,
    block: &'a str,
}

impl Ledger<'_> {
    async fn apply(
        &self,
        files: &[MigrationFile<'_>],
        current_hash: &str,
        code_hash: &str,
    ) -> Result<(), MigrationError> {
        let create = applied_migrations::create_table_sql(self.backend).map_err(|message| {
            self.step_error(
                "create the migration ledger",
                StepError {
                    code: ErrorCode::Internal,
                    detail_code: None,
                    message,
                },
            )
        })?;
        self.db
            .exec(&create)
            .await
            .map_err(|e| self.step_error("create the migration ledger", e))?;

        let rows = self.read_ledger().await?;
        let (first_pending, fresh) = if rows.is_empty() {
            match classify(current_hash, files) {
                Class::Fresh => (0, true),
                Class::Adopt(k) => (self.adopt(&files[..k], files).await?, false),
                Class::Replay => {
                    return Err(MigrationError::refused(
                        self.block,
                        Refusal::NeedsReplay {
                            current_hash: current_hash.to_string(),
                        },
                    ))
                }
            }
        } else {
            (self.plan(files, &rows)?, false)
        };

        for &(name, sql) in &files[first_pending..] {
            self.run_file(files, name, sql, fresh).await?;
        }

        self.db
            .stamp(self.block, code_hash)
            .await
            .map_err(|e| self.step_error("record the applied migrations", e))
    }

    fn step_error(&self, step: &str, error: StepError) -> MigrationError {
        MigrationError::step(self.block, step, error)
    }

    async fn read_ledger(&self) -> Result<Vec<AppliedMigration>, MigrationError> {
        self.db
            .ledger(self.block)
            .await
            .map_err(|e| self.step_error("read the migration ledger", e))
    }

    fn plan(
        &self,
        files: &[MigrationFile<'_>],
        rows: &[AppliedMigration],
    ) -> Result<usize, MigrationError> {
        plan(files, rows).map_err(|refusal| MigrationError::refused(self.block, refusal))
    }

    /// Record `adopted` as applied without running them, in one transaction,
    /// and return where the list resumes. A failed adoption is decided by the
    /// ledger, like a failed file: when another migrator's rows now cover the
    /// adopted files, its record is planned against instead.
    async fn adopt(
        &self,
        adopted: &[MigrationFile<'_>],
        files: &[MigrationFile<'_>],
    ) -> Result<usize, MigrationError> {
        let statements: Vec<RawStatement> = adopted
            .iter()
            .map(|&(name, sql)| self.ledger_row(name, sql, Origin::Adopted))
            .collect();
        let Err(failure) = self.db.transaction(&statements).await else {
            return Ok(adopted.len());
        };
        let rows = self.read_ledger().await?;
        match self.plan(files, &rows)? {
            first_pending if first_pending >= adopted.len() => Ok(first_pending),
            _ => Err(MigrationError {
                block: self.block.to_string(),
                kind: Box::new(MigrationErrorKind::FileFailed {
                    file: format!(
                        "{}..{} (adopted)",
                        adopted[0].0,
                        adopted[adopted.len() - 1].0
                    ),
                    statements: statements.into_iter().map(|s| s.query).collect(),
                    failure,
                }),
            }),
        }
    }

    fn ledger_row(&self, name: &str, sql: &str, origin: Origin) -> RawStatement {
        applied_migrations::insert_statement(
            &AppliedMigration::new(self.block, name, &content_hash(sql), origin),
            self.backend,
        )
    }

    /// Apply one file as one transaction — its ledger row, then its
    /// statements — and decide a failure by reading the ledger again (module
    /// docs, "A failed file is decided by the ledger").
    async fn run_file(
        &self,
        files: &[MigrationFile<'_>],
        name: &str,
        sql: &str,
        fresh: bool,
    ) -> Result<(), MigrationError> {
        if is_frozen(self.block, name) {
            return self.run_frozen_file(files, name, sql).await;
        }
        let mut statements = vec![self.ledger_row(name, sql, Origin::Ran)];
        statements.extend(
            executable_statements(sql)
                .into_iter()
                .map(|query| RawStatement {
                    query,
                    args: Vec::new(),
                }),
        );
        let Err(failure) = self.db.transaction(&statements).await else {
            return Ok(());
        };

        let rows = self.read_ledger().await?;
        match rows.iter().find(|row| row.file_name == name) {
            Some(row) if row.content_sha256 == content_hash(sql) => {
                tracing::info!(
                    block = %self.block,
                    file = %name,
                    error = %failure.message,
                    "migration's transaction reported a failure, but the ledger records the \
                     file: another migrator applied it, or the commit landed; continuing"
                );
                // What the other migrator recorded is held to the same
                // refusals as a record read at the start.
                self.plan(files, &rows).map(drop)
            }
            Some(row) => Err(MigrationError::refused(
                self.block,
                Refusal::ContentChanged {
                    file: name.to_string(),
                    recorded: row.content_sha256.clone(),
                    shipped: content_hash(sql),
                },
            )),
            // Only the wording depends on the message: the file is refused
            // either way, and nothing of it remains.
            None if fresh && is_duplicate_column_error(&failure.message) => {
                Err(MigrationError::refused(
                    self.block,
                    Refusal::TrackingLost {
                        file: name.to_string(),
                        message: failure.message,
                    },
                ))
            }
            None => Err(MigrationError {
                block: self.block.to_string(),
                kind: Box::new(MigrationErrorKind::FileFailed {
                    file: name.to_string(),
                    statements: statements.into_iter().map(|s| s.query).collect(),
                    failure,
                }),
            }),
        }
    }

    /// Apply one frozen file the way it was written to be applied (module
    /// docs, "Frozen files"): its statements one at a time through the
    /// tolerant loop, then its ledger row. A failure of either is decided by
    /// reading the ledger again, as a failed transaction is.
    async fn run_frozen_file(
        &self,
        files: &[MigrationFile<'_>],
        name: &str,
        sql: &str,
    ) -> Result<(), MigrationError> {
        let statements = executable_statements(sql);
        let ran = run_statements(&statements, |statement| self.db.exec(statement)).await;
        let failure = match ran {
            Err(failed) => FrozenFailure::Statement {
                statement: failed.statement,
                error: failed.error,
            },
            Ok(()) => {
                let row = self.ledger_row(name, sql, Origin::Ran);
                let Err(failure) = self.db.transaction(std::slice::from_ref(&row)).await else {
                    return Ok(());
                };
                FrozenFailure::Record {
                    statement: row.query,
                    failure,
                }
            }
        };

        let rows = self.read_ledger().await?;
        match rows.iter().find(|row| row.file_name == name) {
            Some(row) if row.content_sha256 == content_hash(sql) => {
                tracing::info!(
                    block = %self.block,
                    file = %name,
                    "migration failed here, but the ledger records the file: another \
                     migrator applied it; continuing"
                );
                self.plan(files, &rows).map(drop)
            }
            Some(row) => Err(MigrationError::refused(
                self.block,
                Refusal::ContentChanged {
                    file: name.to_string(),
                    recorded: row.content_sha256.clone(),
                    shipped: content_hash(sql),
                },
            )),
            None => Err(MigrationError {
                block: self.block.to_string(),
                kind: Box::new(MigrationErrorKind::FrozenFileFailed {
                    file: name.to_string(),
                    failure,
                }),
            }),
        }
    }
}

/// How a frozen file failed: one of its statements, or the ledger row
/// written after them.
#[derive(Debug)]
enum FrozenFailure {
    Statement {
        statement: String,
        error: StepError,
    },
    Record {
        statement: String,
        failure: TxFailure,
    },
}

/// A migration statement the database refused, and why.
struct FailedStatement<E> {
    statement: String,
    error: E,
}

/// Run a migration file's [`executable_statements`] through `exec`, one at
/// a time, in order, stopping at the first failure: the loop a frozen file
/// is applied with, and the untracked [`apply_ddl_via_service`]. `ALTER TABLE ... ADD COLUMN` is non-idempotent on
/// SQLite/D1, which have no `IF NOT EXISTS` for columns: when an earlier run
/// already added the column the re-run raises "duplicate column", and that —
/// only for an `ADD COLUMN` statement — is a benign no-op so the rest of the
/// file still runs. Every other failure is returned.
///
/// A file added after the ledger never runs through it: it runs once, as one
/// transaction, where a refused statement fails the whole file and nothing
/// is swallowed.
async fn run_statements<'a, F, Fut, T, E>(
    statements: &'a [String],
    mut exec: F,
) -> Result<(), FailedStatement<E>>
where
    F: FnMut(&'a str) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    for statement in statements {
        if let Err(error) = exec(statement).await {
            if is_alter_add_column(statement) && is_duplicate_column_error(&error.to_string()) {
                tracing::debug!(
                    stmt = %statement,
                    err = %error,
                    "ddl: duplicate column, treating as idempotent no-op",
                );
                continue;
            }
            return Err(FailedStatement {
                statement: statement.clone(),
                error,
            });
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
/// Used to gate the duplicate-column-error tolerance in [`run_statements`]
/// — the benign duplicate is swallowed on this specific statement shape
/// only, not on every DDL.
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
/// It decides what [`run_statements`] swallows, and which words the ledger
/// runner refuses a fresh block's failed file with
/// ([`Refusal::TrackingLost`]); it never decides whether a file applied.
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
        assert!(executable_statements("").is_empty());
        assert!(executable_statements("   \n  ").is_empty());
    }

    #[test]
    fn comment_only_chunk_is_skipped() {
        assert!(executable_statements("-- one\n-- two\n").is_empty());
    }

    #[test]
    fn ddl_with_leading_comment_is_executed() {
        assert_eq!(
            executable_statements("-- header\nCREATE TABLE foo (id TEXT)"),
            vec!["CREATE TABLE foo (id TEXT)".to_string()]
        );
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
        let count = executable_statements(sql).len();
        assert_eq!(count, 2);
    }

    #[test]
    fn legalpages_sql_splits_into_expected_chunks() {
        let sql_sqlite =
            include_str!("blocks/legalpages/migrations/001_legalpages_schema.sqlite.sql");
        let sqlite_count = executable_statements(sql_sqlite).len();
        assert_eq!(
            sqlite_count, 2,
            "legalpages sqlite migration: expected 2 statements, got {sqlite_count}"
        );

        let sql_postgres =
            include_str!("blocks/legalpages/migrations/001_legalpages_schema.postgres.sql");
        let postgres_count = executable_statements(sql_postgres).len();
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
        let sqlite_count = executable_statements(sql_sqlite).len();
        assert_eq!(
            sqlite_count, 14,
            "files sqlite migration: expected 14 statements, got {sqlite_count}"
        );

        let sql_postgres = include_str!("blocks/files/migrations/001_initial_schema.postgres.sql");
        let postgres_count = executable_statements(sql_postgres).len();
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
            let count = executable_statements(sql).len();
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
            let count = executable_statements(sql).len();
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
            let stmts: Vec<String> = executable_statements(sql);
            assert_eq!(
                stmts.len(),
                2,
                "admin {dialect} migration 005: expected 2 statements, got {stmts:?}"
            );
            assert!(is_alter_add_column(&stmts[0]), "{dialect}: {}", stmts[0]);
        }
    }

    #[test]
    fn products_sql_splits_into_expected_chunks() {
        // Counts the executable statements in the products block SQL files.
        // 9 CREATE TABLE + 9 CREATE INDEX = 18 statements per backend (the
        // 2026-07 pre-production reset removed `pricing_templates`).
        let sql_sqlite = include_str!("blocks/products/migrations/001_products_schema.sqlite.sql");
        let sqlite_count = executable_statements(sql_sqlite).len();
        assert_eq!(
            sqlite_count, 18,
            "products sqlite migration: expected 18 statements, got {sqlite_count}"
        );

        let sql_postgres =
            include_str!("blocks/products/migrations/001_products_schema.postgres.sql");
        let postgres_count = executable_statements(sql_postgres).len();
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
            let count = executable_statements(sql).len();
            assert_eq!(
                count, 2,
                "products {label} migration 002: expected 2 statements, got {count}"
            );
        }
    }

    /// The tolerance the legacy replay relies on: a frozen file replayed over
    /// the schema it built meets its own `ADD COLUMN` again, and the
    /// untracked loop swallows that duplicate. Only that loop does — the
    /// ledger runner runs a file once, in one transaction, where nothing can
    /// be swallowed (see `a_fresh_block_whose_schema_exists_is_refused_as_tracking_lost`).
    #[tokio::test]
    async fn the_untracked_loop_tolerates_a_duplicate_add_column() {
        let ctx = crate::test_support::TestContext::with_admin().await;
        let migration_sql = "\
            CREATE TABLE IF NOT EXISTS dup_col_test (id TEXT PRIMARY KEY);\n\
            ALTER TABLE dup_col_test ADD COLUMN name TEXT;\n\
        ";
        let db = ctx.database_service();
        apply_ddl_via_service(&db, &[migration_sql])
            .await
            .expect("the first run adds the column");
        apply_ddl_via_service(&db, &[migration_sql])
            .await
            .expect("a re-run meets its own ADD COLUMN, and the duplicate is swallowed");
        // Any other failure still stops it.
        let err = apply_ddl_via_service(&db, &["CREATE NONSENSE foo (id TEXT);"])
            .await
            .expect_err("a syntax error is not swallowed");
        assert!(err.contains("CREATE NONSENSE foo"), "{err}");
    }

    /// The pre-build runner keeps the same tracking as the in-runtime one: it
    /// applies a pending list once, records each file, stamps the hash the
    /// block's `Init` will compute, and does nothing on the next boot.
    #[tokio::test]
    async fn apply_pending_via_service_applies_once_and_stamps_the_lists_hash() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = open_sqlite(&tmp);
        let admin = crate::blocks::admin::migrations::SQLITE_MIGRATIONS;
        apply_pending_via_service(
            &db,
            Backend::Sqlite,
            crate::blocks::admin::ADMIN_BLOCK_ID,
            admin,
        )
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
            apply_pending_via_service(&db, Backend::Sqlite, "test/pending", &files)
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
        let rows = applied_migrations::list_for_block_via_service(&db, "test/pending")
            .await
            .expect("read the ledger");
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].file_name, "001_counted");
        assert_eq!(rows[0].origin, Origin::Ran);
        assert_eq!(rows[0].content_sha256, content_hash(files[0].1));
    }

    /// The list hash covers its SQL alone: the SHA-256 of the files' SQL
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

    /// A file the content-hash algorithm is pinned on: comment lines at the
    /// top, between statements and inside one, blank lines, indentation, a
    /// trailing inline comment and a `;` inside a comment. A change to how
    /// [`split_statements`] splits or how [`content_hash`] joins moves the
    /// pinned value, and would otherwise refuse every deployment at once.
    const CONTENT_HASH_FIXTURE: &str = "\
-- A header; with a semicolon in it.

CREATE TABLE IF NOT EXISTS fixture_t (
    -- a column comment
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT 'a  b' -- trailing; inline
);

  -- between statements
INSERT INTO fixture_t (id, name) VALUES ('1', 'x');
";

    #[test]
    fn the_content_hash_algorithm_is_pinned() {
        assert_eq!(
            executable_statements(CONTENT_HASH_FIXTURE),
            vec![
                "CREATE TABLE IF NOT EXISTS fixture_t (\n    id TEXT PRIMARY KEY,\n    name TEXT \
                 NOT NULL DEFAULT 'a  b' -- trailing; inline\n)"
                    .to_string(),
                "INSERT INTO fixture_t (id, name) VALUES ('1', 'x')".to_string(),
            ]
        );
        assert_eq!(
            content_hash(CONTENT_HASH_FIXTURE),
            "ee9565f9535941c3d254ac6e6e29cad3f18228ffce28541ad417e5c74ea2e203"
        );
    }

    // -----------------------------------------------------------------------
    // The ledger runner, through a block's context (`apply_pending`)
    // -----------------------------------------------------------------------

    use crate::{features::BLOCK_SETTINGS_CONFIG_KEY, test_support::TestContext};

    const BLOCK: &str = "test/ledger";
    /// Each file inserts a row naming itself, so the rows are the number of
    /// times each file really ran.
    const F1: MigrationFile<'static> = (
        "001_runs",
        "CREATE TABLE IF NOT EXISTS ledger_runs (file TEXT NOT NULL);\n\
         INSERT INTO ledger_runs (file) VALUES ('001');",
    );
    const F2: MigrationFile<'static> =
        ("002_more", "INSERT INTO ledger_runs (file) VALUES ('002');");
    const F3: MigrationFile<'static> =
        ("003_more", "INSERT INTO ledger_runs (file) VALUES ('003');");

    /// A fixture with admin's schema, running as the test block. Its snapshot
    /// carries no `block_settings` state, so the fast path never answers and
    /// every call plans against the ledger.
    async fn ledger_ctx() -> TestContext {
        TestContext::with_admin().await.running_as(BLOCK)
    }

    /// The same fixture as a build that applies migrations, whose snapshot
    /// says the block last applied the list hashed `current_hash` — what a
    /// database from before the ledger carries.
    async fn ledger_ctx_with_hash(current_hash: &str) -> TestContext {
        let mut ctx = ledger_ctx().await;
        ctx.set_config(RUN_MIGRATIONS_KEY, "1");
        ctx.set_config(
            BLOCK_SETTINGS_CONFIG_KEY,
            &serde_json::json!({ BLOCK: { "migration": { "current_hash": current_hash } } })
                .to_string(),
        );
        ctx
    }

    async fn runs(ctx: &TestContext, file: &str) -> i64 {
        db::count(
            &ctx.fixture(),
            "ledger_runs",
            &[wafer_block::db::Filter {
                field: "file".into(),
                operator: wafer_block::db::FilterOp::Equal,
                value: serde_json::json!(file),
            }],
        )
        .await
        .expect("count runs")
    }

    async fn ledger_rows(ctx: &TestContext) -> Vec<(String, Origin)> {
        let mut rows: Vec<(String, Origin)> =
            applied_migrations::list_for_block(&ctx.fixture(), BLOCK)
                .await
                .expect("read the ledger")
                .into_iter()
                .map(|row| (row.file_name, row.origin))
                .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }

    async fn stamped(ctx: &TestContext) -> Option<String> {
        block_settings::list_all(&ctx.fixture())
            .await
            .expect("list block settings")
            .into_iter()
            .find(|row| row.block_name == BLOCK)
            .map(|row| row.current_hash)
    }

    async fn table_exists(ctx: &TestContext, table: &str) -> bool {
        db::table_exists(&ctx.fixture(), table)
            .await
            .expect("probe the table")
    }

    fn owned(rows: &[(&str, Origin)]) -> Vec<(String, Origin)> {
        rows.iter()
            .map(|(name, origin)| ((*name).to_string(), *origin))
            .collect()
    }

    /// A fresh block: each file runs once and is recorded `ran` with its
    /// content hash, the list is stamped, and a second call runs nothing.
    #[tokio::test]
    async fn a_fresh_block_runs_each_file_once_and_stamps_the_list() {
        let ctx = ledger_ctx().await;
        let files = [F1, F2];
        for _ in 0..2 {
            apply_pending(&ctx, BLOCK, &files).await.expect("apply");
        }
        assert_eq!((runs(&ctx, "001").await, runs(&ctx, "002").await), (1, 1));
        assert_eq!(
            ledger_rows(&ctx).await,
            owned(&[("001_runs", Origin::Ran), ("002_more", Origin::Ran)])
        );
        assert_eq!(stamped(&ctx).await, Some(migration_set_hash(&files)));
    }

    /// A list that grows runs only the new file.
    #[tokio::test]
    async fn a_grown_list_runs_only_the_new_file() {
        let ctx = ledger_ctx().await;
        apply_pending(&ctx, BLOCK, &[F1, F2])
            .await
            .expect("apply v1");
        apply_pending(&ctx, BLOCK, &[F1, F2, F3])
            .await
            .expect("apply v2");
        assert_eq!(
            (
                runs(&ctx, "001").await,
                runs(&ctx, "002").await,
                runs(&ctx, "003").await
            ),
            (1, 1, 1)
        );
        assert_eq!(stamped(&ctx).await, Some(migration_set_hash(&[F1, F2, F3])));
    }

    /// An applied file whose statements were edited refuses the block before
    /// anything runs, naming the block, the file and both hashes.
    #[tokio::test]
    async fn an_edited_applied_file_refuses_and_runs_nothing() {
        let ctx = ledger_ctx().await;
        apply_pending(&ctx, BLOCK, &[F1, F2])
            .await
            .expect("apply v1");
        let edited: MigrationFile<'static> = (
            F2.0,
            "INSERT INTO ledger_runs (file) VALUES ('002-edited');",
        );
        let err = apply_pending(&ctx, BLOCK, &[F1, edited, F3])
            .await
            .expect_err("an edited applied file refuses");
        assert_eq!(
            err.refusal(),
            Some(&Refusal::ContentChanged {
                file: F2.0.to_string(),
                recorded: content_hash(F2.1),
                shipped: content_hash(edited.1),
            })
        );
        let text = err.to_string();
        assert!(
            text.starts_with(&format!("{BLOCK}: migration `{}`", F2.0)),
            "{text}"
        );
        assert!(
            text.contains(&content_hash(F2.1)) && text.contains(&content_hash(edited.1)),
            "{text}"
        );
        assert_eq!(err.code(), ErrorCode::FailedPrecondition);
        assert_eq!(runs(&ctx, "003").await, 0, "no later file runs");
        assert_eq!(runs(&ctx, "002-edited").await, 0);
    }

    /// Editing only an applied file's comments runs nothing: the content hash
    /// is unchanged, and the new list is stamped.
    #[tokio::test]
    async fn an_applied_files_comment_edit_runs_nothing_and_restamps() {
        let ctx = ledger_ctx().await;
        apply_pending(&ctx, BLOCK, &[F1, F2])
            .await
            .expect("apply v1");
        let commented: MigrationFile<'static> = (
            F2.0,
            "-- Counts a run.\nINSERT INTO ledger_runs (file) VALUES ('002');\n-- The end.\n",
        );
        apply_pending(&ctx, BLOCK, &[F1, commented])
            .await
            .expect("a comment edit is not a content change");
        assert_eq!(runs(&ctx, "002").await, 1);
        assert_eq!(
            stamped(&ctx).await,
            Some(migration_set_hash(&[F1, commented]))
        );
    }

    /// The ledger names a file this build does not ship: "database ahead",
    /// with its remedy, and nothing runs.
    #[tokio::test]
    async fn a_database_ahead_of_the_code_refuses() {
        let ctx = ledger_ctx().await;
        apply_pending(&ctx, BLOCK, &[F1, F2])
            .await
            .expect("apply v2");
        let err = apply_pending(&ctx, BLOCK, &[F1])
            .await
            .expect_err("an older list refuses");
        assert_eq!(
            err.refusal(),
            Some(&Refusal::DatabaseAhead {
                files: vec![F2.0.to_string()]
            })
        );
        let text = err.to_string();
        assert!(text.contains("migrated by a newer build"), "{text}");
        assert!(text.contains("restore a backup"), "{text}");
        assert_eq!(stamped(&ctx).await, Some(migration_set_hash(&[F1, F2])));
    }

    /// A recorded file after one that is not: a gap, refused.
    #[tokio::test]
    async fn a_gap_in_the_ledger_refuses() {
        let ctx = ledger_ctx().await;
        apply_pending(&ctx, BLOCK, &[F1, F2]).await.expect("apply");
        let first = applied_migrations::list_for_block(&ctx.fixture(), BLOCK)
            .await
            .expect("read the ledger")
            .into_iter()
            .find(|row| row.file_name == F1.0)
            .expect("001 is recorded");
        // Test-fixture setup: a ledger edited by hand.
        db::delete(&ctx.fixture(), applied_migrations::TABLE, &first.id)
            .await
            .expect("delete 001's row");
        let err = apply_pending(&ctx, BLOCK, &[F1, F2, F3])
            .await
            .expect_err("a gap refuses");
        assert_eq!(
            err.refusal(),
            Some(&Refusal::Gap {
                applied: F2.0.to_string(),
                pending: F1.0.to_string(),
            })
        );
        assert_eq!(runs(&ctx, "001").await, 1, "001 did not run again");
        assert_eq!(runs(&ctx, "003").await, 0);
    }

    /// A file whose third statement fails leaves nothing of itself — its
    /// first two statements and its ledger row roll back with it — while the
    /// file before it stays recorded; the error names the block, the file and
    /// the statement, the list is not stamped, and the next run resumes at
    /// the failed file.
    #[tokio::test]
    async fn a_failing_file_rolls_back_whole_and_the_next_run_resumes_at_it() {
        let ctx = ledger_ctx().await;
        let broken: MigrationFile<'static> = (
            "002_broken",
            "CREATE TABLE ledger_side (id TEXT);\n\
             INSERT INTO ledger_runs (file) VALUES ('002');\n\
             INSERT INTO ledger_missing (x) VALUES (1);",
        );
        let err = apply_pending(&ctx, BLOCK, &[F1, broken])
            .await
            .expect_err("002 fails");
        let text = err.to_string();
        assert!(
            text.starts_with(&format!(
                "{BLOCK}: migration `002_broken` failed on `INSERT INTO ledger_missing (x) \
                 VALUES (1)` and was rolled back"
            )),
            "the error names the block, the file and the statement: {text}"
        );
        assert!(err.refusal().is_none());
        assert!(
            !table_exists(&ctx, "ledger_side").await,
            "002's first statement rolled back"
        );
        assert_eq!(
            runs(&ctx, "002").await,
            0,
            "002's second statement rolled back"
        );
        assert_eq!(ledger_rows(&ctx).await, owned(&[("001_runs", Origin::Ran)]));
        assert_eq!(stamped(&ctx).await, None, "a failed list is not stamped");

        let fixed: MigrationFile<'static> = (
            "002_broken",
            "CREATE TABLE ledger_side (id TEXT);\n\
             INSERT INTO ledger_runs (file) VALUES ('002');",
        );
        apply_pending(&ctx, BLOCK, &[F1, fixed])
            .await
            .expect("the fixed file applies");
        assert_eq!((runs(&ctx, "001").await, runs(&ctx, "002").await), (1, 1));
        assert_eq!(stamped(&ctx).await, Some(migration_set_hash(&[F1, fixed])));
    }

    /// `AlreadyExists` past the ledger row — a statement of the file itself
    /// that duplicates a key — is a failure of the file, not someone else's
    /// claim: the ledger does not record the file, so the block stops.
    #[tokio::test]
    async fn a_duplicate_key_inside_a_file_is_a_failure_not_a_claim() {
        let ctx = ledger_ctx().await;
        let duplicate: MigrationFile<'static> = (
            "002_duplicate",
            "CREATE TABLE ledger_unique (id TEXT PRIMARY KEY);\n\
             INSERT INTO ledger_unique (id) VALUES ('a');\n\
             INSERT INTO ledger_unique (id) VALUES ('a');",
        );
        let err = apply_pending(&ctx, BLOCK, &[F1, duplicate, F3])
            .await
            .expect_err("a duplicate inside the file fails it");
        assert_eq!(err.code(), ErrorCode::AlreadyExists, "{err}");
        assert!(err.refusal().is_none());
        assert!(
            err.to_string()
                .contains("failed on `INSERT INTO ledger_unique (id) VALUES ('a')`"),
            "{err}"
        );
        assert!(!table_exists(&ctx, "ledger_unique").await);
        assert_eq!(runs(&ctx, "003").await, 0, "nothing after it runs");
        assert_eq!(stamped(&ctx).await, None);
    }

    /// A lost `block_settings` row alone: the ledger still records every file,
    /// so nothing runs, and the row is stamped again.
    #[tokio::test]
    async fn a_lost_block_settings_row_alone_is_restamped_without_running() {
        let ctx = ledger_ctx().await;
        apply_pending(&ctx, BLOCK, &[F1, F2]).await.expect("apply");
        let row = block_settings::list_all(&ctx.fixture())
            .await
            .expect("list")
            .into_iter()
            .find(|row| row.block_name == BLOCK)
            .expect("stamped");
        // Test-fixture setup: the row is lost.
        db::delete(&ctx.fixture(), block_settings::TABLE, &row.id)
            .await
            .expect("delete the row");
        apply_pending(&ctx, BLOCK, &[F1, F2])
            .await
            .expect("re-apply");
        assert_eq!((runs(&ctx, "001").await, runs(&ctx, "002").await), (1, 1));
        assert_eq!(stamped(&ctx).await, Some(migration_set_hash(&[F1, F2])));
    }

    /// Both the `block_settings` row and the ledger rows are gone but the
    /// schema is there: the block classifies as fresh, its file meets the
    /// existing schema, rolls back whole, and the refusal names the state
    /// and its remedy rather than the bare duplicate.
    #[tokio::test]
    async fn a_fresh_block_whose_schema_exists_is_refused_as_tracking_lost() {
        let ctx = ledger_ctx().await;
        let db = ctx.database_service();
        // Test-fixture setup: the schema an earlier, since-forgotten run built.
        apply_ddl_via_service(
            &db,
            &["CREATE TABLE ledger_runs (file TEXT NOT NULL);\n\
               ALTER TABLE ledger_runs ADD COLUMN extra TEXT;"],
        )
        .await
        .expect("build the schema");
        let file: MigrationFile<'static> = (
            "001_schema",
            "CREATE TABLE IF NOT EXISTS ledger_runs (file TEXT NOT NULL);\n\
             CREATE TABLE ledger_side (id TEXT);\n\
             ALTER TABLE ledger_runs ADD COLUMN extra TEXT;",
        );
        let err = apply_pending(&ctx, BLOCK, &[file])
            .await
            .expect_err("tracking lost refuses");
        match err.refusal() {
            Some(Refusal::TrackingLost { file: name, .. }) => assert_eq!(name, "001_schema"),
            other => panic!("expected TrackingLost, got {other:?}: {err}"),
        }
        let text = err.to_string();
        assert!(text.contains("its tracking was lost"), "{text}");
        assert!(
            text.contains("from a backup taken before the loss"),
            "{text}"
        );
        assert!(
            !table_exists(&ctx, "ledger_side").await,
            "nothing is half-applied"
        );
        assert!(ledger_rows(&ctx).await.is_empty());
    }

    /// A database from before the ledger whose hash is today's first file:
    /// that file is recorded `adopted` without running, and the rest run.
    #[tokio::test]
    async fn a_pre_ledger_block_adopts_the_prefix_its_hash_proves() {
        for k in [1, 2] {
            let files = [F1, F2, F3];
            let ctx = ledger_ctx_with_hash(&migration_set_hash(&files[..k])).await;
            // What the runner from before the ledger left: the first `k` files
            // ran, and their list's hash was stamped.
            let sql: Vec<&str> = files[..k].iter().map(|(_, sql)| *sql).collect();
            apply_ddl_via_service(&ctx.database_service(), &sql)
                .await
                .expect("the pre-ledger run");

            apply_pending(&ctx, BLOCK, &files)
                .await
                .expect("adopt and run");

            for file in ["001", "002", "003"] {
                assert_eq!(runs(&ctx, file).await, 1, "k = {k}: {file} ran once");
            }
            let expected: Vec<(&str, Origin)> = files
                .iter()
                .enumerate()
                .map(|(i, (name, _))| (*name, if i < k { Origin::Adopted } else { Origin::Ran }))
                .collect();
            assert_eq!(ledger_rows(&ctx).await, owned(&expected), "k = {k}");
            assert_eq!(stamped(&ctx).await, Some(migration_set_hash(&files)));
        }
    }

    /// A hash from before the ledger that matches no prefix — an edited file
    /// was applied, a corrupt value, or the `applying` sentinel a crashed run
    /// from before #145 left — needs the legacy replay, which this build does
    /// not carry: refused, with nothing run or recorded.
    #[tokio::test]
    async fn an_unmatched_pre_ledger_hash_is_refused_until_the_replay_ships() {
        for current in ["applying", "an-older-list"] {
            let ctx = ledger_ctx_with_hash(current).await;
            let err = apply_pending(&ctx, BLOCK, &[F1, F2])
                .await
                .expect_err("refused");
            assert_eq!(
                err.refusal(),
                Some(&Refusal::NeedsReplay {
                    current_hash: current.to_string()
                })
            );
            assert!(err.to_string().contains("needs the legacy replay"), "{err}");
            assert!(!table_exists(&ctx, "ledger_runs").await, "nothing ran");
            assert!(ledger_rows(&ctx).await.is_empty(), "nothing recorded");
        }
    }

    /// Frozen files run as they were written to: auth's whole list applies to
    /// a fresh database although its 016 opens with an `ADD COLUMN` that
    /// 001's column makes a duplicate there, and every file is recorded with
    /// its content hash.
    #[tokio::test]
    async fn a_fresh_blocks_frozen_files_run_as_written_and_are_recorded() {
        let ctx = TestContext::with_auth().await;
        let rows = applied_migrations::list_for_block_via_service(
            &ctx.database_service(),
            crate::blocks::auth::AUTH_BLOCK_ID,
        )
        .await
        .expect("read the ledger");
        let auth = crate::blocks::auth::migrations::SQLITE_MIGRATIONS;
        assert_eq!(rows.len(), auth.len(), "{rows:?}");
        for (name, sql) in auth {
            let row = rows
                .iter()
                .find(|row| row.file_name == *name)
                .unwrap_or_else(|| panic!("{name} is recorded"));
            assert_eq!(row.origin, Origin::Ran);
            assert_eq!(row.content_sha256, content_hash(sql));
        }
        // A frozen file is recorded only once every statement of it ran, so
        // 016's row says its copy and its `DROP COLUMN` ran too.
    }

    /// A frozen file that fails part-way is not atomic: the statements before
    /// the failing one stay applied, the file is not recorded, the error
    /// says so, and the next run runs it again from its start.
    #[tokio::test]
    async fn a_failed_frozen_file_stays_unrecorded_and_runs_again_from_its_start() {
        // A block with a freeze mark, and a file at it: frozen.
        const FROZEN_BLOCK: &str = "impresspress/signal";
        assert!(is_frozen(FROZEN_BLOCK, "001_frozen"));
        let ctx = TestContext::with_admin().await.running_as(FROZEN_BLOCK);
        let broken: MigrationFile<'static> = (
            "001_frozen",
            "CREATE TABLE IF NOT EXISTS frozen_side (id TEXT);\n\
             INSERT INTO frozen_missing (x) VALUES (1);",
        );
        let err = apply_pending(&ctx, FROZEN_BLOCK, &[broken])
            .await
            .expect_err("the second statement fails");
        let text = err.to_string();
        assert!(
            text.starts_with(&format!(
                "{FROZEN_BLOCK}: migration `001_frozen` failed on `INSERT INTO frozen_missing (x) \
                 VALUES (1)`"
            )) && text.contains("the statements before this one stay applied"),
            "{text}"
        );
        assert!(
            table_exists(&ctx, "frozen_side").await,
            "not atomic: the first stays"
        );
        let recorded = applied_migrations::list_for_block(&ctx.fixture(), FROZEN_BLOCK)
            .await
            .expect("read the ledger");
        assert!(recorded.is_empty(), "{recorded:?}");

        let fixed: MigrationFile<'static> = (
            "001_frozen",
            "CREATE TABLE IF NOT EXISTS frozen_side (id TEXT);\n\
             CREATE TABLE IF NOT EXISTS frozen_missing (x INTEGER);\n\
             INSERT INTO frozen_missing (x) VALUES (1);",
        );
        apply_pending(&ctx, FROZEN_BLOCK, &[fixed])
            .await
            .expect("the whole file runs again");
        let recorded = applied_migrations::list_for_block(&ctx.fixture(), FROZEN_BLOCK)
            .await
            .expect("read the ledger");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].content_sha256, content_hash(fixed.1));
    }

    // -----------------------------------------------------------------------
    // The same runner over the platform service (`apply_pending_via_service`)
    // -----------------------------------------------------------------------

    fn open_sqlite(dir: &tempfile::TempDir) -> Arc<dyn DatabaseService> {
        Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open(
                dir.path()
                    .join("ledger.sqlite3")
                    .to_str()
                    .expect("utf-8 path"),
            )
            .expect("open sqlite"),
        )
    }

    /// A decorator that counts the transactions sent through it and, once,
    /// writes `claim` — another migrator's ledger row — just before
    /// forwarding a transaction, so that migrator's claim lands between the
    /// runner's plan and its own claim.
    struct RacingDb {
        inner: Arc<dyn DatabaseService>,
        transactions: std::sync::atomic::AtomicUsize,
        claim: std::sync::Mutex<Option<RawStatement>>,
    }

    impl RacingDb {
        fn new(inner: Arc<dyn DatabaseService>, claim: Option<RawStatement>) -> Arc<Self> {
            Arc::new(Self {
                inner,
                transactions: std::sync::atomic::AtomicUsize::new(0),
                claim: std::sync::Mutex::new(claim),
            })
        }

        fn inner_service(&self) -> &dyn DatabaseService {
            self.inner.as_ref()
        }

        fn transactions(&self) -> usize {
            self.transactions.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    wafer_core::forward_database_service! {
        impl DatabaseService for RacingDb {
            forward_to inner_service();

            ops {
                get: forward,
                list: forward,
                create: forward,
                create_many: forward,
                update: forward,
                delete: forward,
                count: forward,
                sum: forward,
                query_raw: forward,
                exec_raw: forward,
                exec_raw_transaction: custom,
                delete_where: forward,
                delete_where_count: forward,
                take_where: forward,
                update_where: forward,
                update_where_count: forward,
                increment_field_where: forward,
                upsert: forward,
                aggregate: forward,
                batch: forward,
                insert_guarded: forward,
                update_guarded: forward,
                ensure_schema_table: forward,
                ensure_schema_tables: forward,
                schema_table_exists: forward,
                schema_columns: forward,
                schema_drop_table: forward,
                schema_add_column: forward,
                set_strict_schema: forward,
                statement_budget: forward,
            }

            async fn exec_raw_transaction(
                &self,
                statements: &[RawStatement],
            ) -> Result<Vec<i64>, TransactionError> {
                self.transactions
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let claim = self.claim.lock().expect("claim").take();
                if let Some(claim) = claim {
                    self.inner
                        .exec_raw(&claim.query, &claim.args)
                        .await
                        .expect("the other migrator's claim");
                }
                self.inner.exec_raw_transaction(statements).await
            }
        }
    }

    /// A list that grows sends one transaction, for the new file: the plan
    /// reads the ledger and skips what it records.
    #[tokio::test]
    async fn a_grown_list_sends_only_the_new_files_transaction() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sqlite = open_sqlite(&tmp);
        apply_pending_via_service(
            &sqlite,
            Backend::Sqlite,
            ADMIN,
            crate::blocks::admin::migrations::SQLITE_MIGRATIONS,
        )
        .await
        .expect("admin's schema");
        apply_pending_via_service(&sqlite, Backend::Sqlite, BLOCK, &[F1, F2])
            .await
            .expect("apply v1");
        let racing = RacingDb::new(Arc::clone(&sqlite), None);
        let db: Arc<dyn DatabaseService> = racing.clone();
        apply_pending_via_service(&db, Backend::Sqlite, BLOCK, &[F1, F2, F3])
            .await
            .expect("apply v2");
        assert_eq!(racing.transactions(), 1, "only 003 was sent");
        assert_eq!(sqlite.count("ledger_runs", &[]).await.expect("count"), 3);
    }

    /// Another migrator claims file 2 between this runner's plan and its own
    /// claim: this runner's transaction for file 2 is refused at its ledger
    /// insert and rolls back, the runner reads the ledger, finds file 2
    /// recorded with the same hash, and goes on to file 3.
    #[tokio::test]
    async fn a_claim_conflict_is_decided_by_the_ledger_and_continues() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sqlite = open_sqlite(&tmp);
        apply_pending_via_service(
            &sqlite,
            Backend::Sqlite,
            ADMIN,
            crate::blocks::admin::migrations::SQLITE_MIGRATIONS,
        )
        .await
        .expect("admin's schema");
        apply_pending_via_service(&sqlite, Backend::Sqlite, BLOCK, &[F1])
            .await
            .expect("apply v1");
        let claim = applied_migrations::insert_statement(
            &AppliedMigration::new(BLOCK, F2.0, &content_hash(F2.1), Origin::Ran),
            Backend::Sqlite,
        );
        let racing = RacingDb::new(Arc::clone(&sqlite), Some(claim));
        let db: Arc<dyn DatabaseService> = racing.clone();
        apply_pending_via_service(&db, Backend::Sqlite, BLOCK, &[F1, F2, F3])
            .await
            .expect("the conflict is read from the ledger and the run continues");
        let count = |file: &'static str| {
            let sqlite = Arc::clone(&sqlite);
            async move {
                sqlite
                    .count(
                        "ledger_runs",
                        &[wafer_block::db::Filter {
                            field: "file".into(),
                            operator: wafer_block::db::FilterOp::Equal,
                            value: serde_json::json!(file),
                        }],
                    )
                    .await
                    .expect("count")
            }
        };
        assert_eq!(
            count("002").await,
            0,
            "this runner's 002 rolled back with its claim"
        );
        assert_eq!(count("003").await, 1, "the run went on to 003");
        let state = block_settings::load(&sqlite)
            .await
            .expect("load")
            .state(BLOCK)
            .migration;
        assert_eq!(state.current_hash, migration_set_hash(&[F1, F2, F3]));
    }

    // -----------------------------------------------------------------------
    // Guards over the shipped migration files
    // -----------------------------------------------------------------------

    use crate::blocks::admin::ADMIN_BLOCK_ID as ADMIN;

    /// One shipped migration file, read off disk.
    struct Shipped {
        /// `<block directory>/<basename>`, e.g. `auth/016_roles_are_grants`.
        key: String,
        /// `sqlite` or `postgres`.
        dialect: String,
        sql: String,
    }

    /// Every `src/blocks/*/migrations/*.{sqlite,postgres}.sql`, sorted by key
    /// then dialect. Read off disk rather than from the blocks' constants, so
    /// the guards cover every block whatever features the test is built
    /// with.
    fn shipped_migrations() -> Vec<Shipped> {
        let blocks = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/blocks");
        let mut shipped = Vec::new();
        for block in std::fs::read_dir(&blocks).expect("read src/blocks") {
            let block = block.expect("block entry").path();
            let Ok(files) = std::fs::read_dir(block.join("migrations")) else {
                continue;
            };
            let block_name = block
                .file_name()
                .and_then(|name| name.to_str())
                .expect("block directory name")
                .to_string();
            for file in files {
                let path = file.expect("migration entry").path();
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("file name")
                    .to_string();
                for dialect in ["sqlite", "postgres"] {
                    if let Some(basename) = name.strip_suffix(&format!(".{dialect}.sql")) {
                        shipped.push(Shipped {
                            key: format!("{block_name}/{basename}"),
                            dialect: dialect.to_string(),
                            sql: std::fs::read_to_string(&path).expect("read migration"),
                        });
                    }
                }
            }
        }
        shipped.sort_by(|a, b| (&a.key, &a.dialect).cmp(&(&b.key, &b.dialect)));
        assert!(
            !shipped.is_empty(),
            "no migration found under {}",
            blocks.display()
        );
        shipped
    }

    /// Every file ships in both dialects under one basename, and a block's
    /// basenames carry strictly increasing numeric prefixes: the ledger keys
    /// a file by its basename, whichever dialect ran it.
    #[test]
    fn every_migration_ships_in_both_dialects_with_increasing_prefixes() {
        let shipped = shipped_migrations();
        let mut by_key: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
        for file in &shipped {
            by_key.entry(&file.key).or_default().push(&file.dialect);
        }
        let lopsided: Vec<_> = by_key
            .iter()
            .filter(|(_, dialects)| dialects.as_slice() != ["postgres", "sqlite"])
            .collect();
        assert!(lopsided.is_empty(), "files missing a dialect: {lopsided:?}");

        let mut last: std::collections::BTreeMap<&str, u32> = Default::default();
        for key in by_key.keys() {
            let (block, basename) = key.split_once('/').expect("block/basename");
            let prefix: u32 = basename
                .split_once('_')
                .and_then(|(number, _)| number.parse().ok())
                .unwrap_or_else(|| panic!("{key} has no NNN_ prefix"));
            if let Some(previous) = last.insert(block, prefix) {
                assert!(
                    prefix > previous,
                    "{key} does not follow {block}'s {previous:03}"
                );
            }
        }
    }

    /// The most executable statements a file added after the ledger may
    /// have. Such a file is admitted against the per-invocation statement
    /// budget whole — its statements plus its ledger row — so a file that
    /// needs more than one invocation can give can never be deployed. Half
    /// of fullapp-test's budget of 50, the line the fresh-install guard below
    /// uses. Frozen files stay as they shipped (products' 005 has 95).
    const NEW_FILE_STATEMENT_CAP: usize = 25;

    #[test]
    fn no_file_after_the_freeze_exceeds_the_statement_cap() {
        let over: Vec<String> = shipped_migrations()
            .into_iter()
            .filter(|file| file.dialect == "sqlite")
            .filter(|file| {
                let (directory, basename) = file.key.split_once('/').expect("block/basename");
                !is_frozen(block_of(directory), basename)
            })
            .filter_map(|file| {
                let count = executable_statements(&file.sql).len();
                (count > NEW_FILE_STATEMENT_CAP).then(|| format!("{} ({count})", file.key))
            })
            .collect();
        assert!(
            over.is_empty(),
            "migration files over {NEW_FILE_STATEMENT_CAP} statements: {over:?} — split the \
             change into more files"
        );
    }

    /// The content hash of every shipped migration, per dialect, as
    /// `migration-content-hashes.txt` (next to this crate's `Cargo.toml`)
    /// records it. A shipped file is immutable: a database that applied it
    /// refuses a build whose copy hashes differently. So a moved hash fails
    /// here, at PR time, with the lines that moved; a new file fails until
    /// its line is added.
    #[test]
    fn shipped_migration_content_hashes_match_the_manifest() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migration-content-hashes.txt");
        let recorded = std::fs::read_to_string(&path).unwrap_or_default();
        let recorded: std::collections::BTreeSet<&str> = recorded
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        let actual: Vec<String> = shipped_migrations()
            .iter()
            .map(|file| format!("{} {} {}", file.key, file.dialect, content_hash(&file.sql)))
            .collect();
        let actual_set: std::collections::BTreeSet<&str> =
            actual.iter().map(String::as_str).collect();
        let moved_or_new: Vec<&&str> = actual_set.difference(&recorded).collect();
        let gone_or_old: Vec<&&str> = recorded.difference(&actual_set).collect();
        assert!(
            moved_or_new.is_empty() && gone_or_old.is_empty(),
            "{} does not match the shipped migrations.\n\
             In the files, not the manifest (a new file, or an EDITED shipped file — which no \
             database that applied it will accept; put the change in a new file):\n  {}\n\
             In the manifest, not the files (removed or edited):\n  {}",
            path.display(),
            moved_or_new
                .iter()
                .map(|line| **line)
                .collect::<Vec<_>>()
                .join("\n  "),
            gone_or_old
                .iter()
                .map(|line| **line)
                .collect::<Vec<_>>()
                .join("\n  "),
        );
    }

    /// The block name a migrations directory belongs to, from
    /// [`FROZEN_THROUGH`]: the entry whose name ends in `/<directory>`.
    fn block_of(directory: &str) -> &'static str {
        let matching: Vec<&str> = FROZEN_THROUGH
            .iter()
            .map(|(block, _)| *block)
            .filter(|block| block.rsplit_once('/').map(|(_, dir)| dir) == Some(directory))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "src/blocks/{directory}/migrations needs exactly one FROZEN_THROUGH entry: {matching:?}"
        );
        matching[0]
    }

    /// Every block that ships migrations has its freeze mark, and the mark
    /// names a file it ships.
    #[test]
    fn every_block_has_a_freeze_mark_naming_a_shipped_file() {
        let shipped = shipped_migrations();
        for (block, last) in FROZEN_THROUGH {
            let directory = block.rsplit_once('/').expect("org/block").1;
            assert!(
                shipped
                    .iter()
                    .any(|file| file.key == format!("{directory}/{last}")),
                "{block}'s freeze mark {last} is not a shipped file"
            );
        }
        for file in &shipped {
            block_of(file.key.split_once('/').expect("block/basename").0);
        }
    }

    /// A block's freeze mark covers its files up to and including the mark,
    /// a prefix of its list stopping short of it, and nothing after it; an
    /// unlisted block has no frozen file.
    #[test]
    fn frozen_is_at_or_before_the_freeze_mark() {
        assert!(is_frozen("wafer-run/auth", "001_auth_schema"));
        assert!(is_frozen("wafer-run/auth", "016_roles_are_grants"));
        assert!(!is_frozen("wafer-run/auth", "017_anything_later"));
        assert!(!is_frozen("test/ledger", "001_runs"));
    }

    /// No migration file names the ledger: its rows are the runner's record
    /// of what ran, written only with the file they record, and a file that
    /// wrote one could claim another file without running it.
    #[test]
    fn no_migration_file_names_the_ledger_table() {
        let naming: Vec<String> = shipped_migrations()
            .into_iter()
            .filter(|file| file.sql.contains(applied_migrations::TABLE))
            .map(|file| format!("{}.{}", file.key, file.dialect))
            .collect();
        assert!(
            naming.is_empty(),
            "migrations naming the ledger: {naming:?}"
        );
    }

    /// `/_deploy/prepare` applies every block's pending migrations in ONE
    /// Worker invocation, so on a fresh database the statements of every
    /// SQLite migration file the crate ships, each file's ledger row
    /// included, have to fit well inside D1's per-invocation query limit
    /// ([`D1_QUERIES_PER_INVOCATION_DEFAULT`]), with the funnel's seeds,
    /// migration stamps and config reads in what is left. Half the limit is
    /// that line.
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
        let mut per_block: std::collections::BTreeMap<String, usize> = Default::default();
        for file in shipped_migrations()
            .iter()
            .filter(|file| file.dialect == "sqlite")
        {
            let block = file
                .key
                .split_once('/')
                .expect("block/basename")
                .0
                .to_string();
            // The file's statements, and its ledger row.
            *per_block.entry(block).or_default() += executable_statements(&file.sql).len() + 1;
        }
        let total: usize = per_block.values().sum();
        let limit = crate::config_vars::D1_QUERIES_PER_INVOCATION_DEFAULT as usize;
        assert!(
            total <= limit / 2,
            "a fresh database's migrations are {total} statements, more than half of the \
             {limit} D1 queries `/_deploy/prepare` may run in its one invocation: \
             {per_block:?}"
        );
    }
}
