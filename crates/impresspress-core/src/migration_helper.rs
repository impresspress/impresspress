//! Shared migration runner.
//!
//! A block's migrations are an ordered list of `.sql` files, each a
//! [`MigrationFile`] `(basename, sql)` pair, declared together with the
//! block's [history](HistoricalState) in one [`BlockMigrations`]. Every block
//! applies its list from `lifecycle(Init)` through [`lifecycle_init`] (auth's
//! service `init` calls [`apply_migrations`] directly), which picks the list
//! for the active SQL dialect and hands it to [`apply_pending`].
//!
//! # The ledger: one row per applied file
//!
//! What a database has applied is recorded per file, in
//! `impresspress__admin__applied_migrations`
//! ([`crate::platform_state::applied_migrations`]): the block, the file's
//! basename, its [`content_hash`] as applied, how the row came to be (`ran`
//! or `adopted`) and, for an adopted row, the pre-ledger list hash it was
//! adopted from. **Every file is applied as ONE transaction**
//! (`db::ddl_transaction`): its ledger row first, then its statements. The
//! whole file commits with its row, or nothing of it does, so a row exists
//! exactly when its file's statements took effect, on every backend. No
//! statement is ever tolerated: a refused statement fails its whole file.
//!
//! Beside it, the block's `block_settings.current_hash` keeps the meaning it
//! always had: the [`migration_set_hash`] of the list this database last had
//! fully applied. It is the fast path, and it is what a database from before
//! the ledger is recognised by (below).
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
//! 6. Run each pending file, in order, as its own transaction.
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
//! # A failed file is decided by the ledger, never by its error
//!
//! When a file's transaction fails, whatever the error, the runner reads the
//! block's ledger once more:
//!
//! - **the file is recorded with the same hash:** another migrator applied
//!   it (the ledger insert is the claim, and the second claim on a file is
//!   refused with its transaction), or this run's own commit landed although
//!   the backend reported a failure after it. Continue with the next file.
//! - **recorded with another hash:** the content-changed refusal.
//! - **not recorded:** nothing of the file remains. The error names the
//!   block, the file, the backend's classification, the failing statement
//!   when the backend says which (D1's `batch()` never does) and its message.
//!   When the failure is a column the file adds that already exists, the
//!   error is a named refusal instead (below).
//!
//! Neither the error's text nor the statement index decides whether a file
//! applied: a claim conflict on Postgres names a constraint and on SQLite a
//! table, a D1 error names no statement at all, and only the ledger says
//! whether the file is recorded. If the re-read itself fails, the error
//! carries both failures.
//!
//! These errors carry the database's own text. They are `Init` errors, for
//! the deploy report, the boot's refusal and the logs; a block must not copy
//! one into an HTTP response body.
//!
//! # A column that already exists
//!
//! A file that is not recorded and failed because a column it adds already
//! exists is refused by name, whichever block it is, rather than reported as
//! a plain failure. Nothing of the file remains either way; only the words
//! and the classification differ, and the classification is what keeps a
//! browser from erasing its data over it ([`MigrationError::refusal`]):
//!
//! - **tracking lost** ([`Refusal::TrackingLost`]): the block had no record
//!   at all (fresh), yet its schema exists — both its `block_settings` row
//!   and its ledger rows were lost. The refusal names the state on the first
//!   attempt; the `CREATE … IF NOT EXISTS` files before the failing one ran
//!   as no-ops and are recorded, so a later attempt reports the same file as
//!   [`Refusal::DriftedSchema`]. The remedy is the same.
//! - **drifted schema** ([`Refusal::DriftedSchema`]): the block was adopted
//!   or has run files, and a typed write created the column before the
//!   migration that adds it ran. Until the pinned wafer-run made strict
//!   schema the only behaviour (2026-10), native and browser databases added
//!   a missing column on the first write naming it; a browser profile created
//!   before 2026-09-26 never migrated after its creation, so newer code wrote
//!   such columns into it. Remedy: recreate the database, or restore a backup
//!   (a browser's stopped screen offers Reset).
//!
//! ## Adding a column a write may already have created
//!
//! No write adds a column any more, but until 2026-10 a native or browser
//! database added one on the first typed write that named it, so such a
//! database may hold a column no migration declared. A new file that adds such
//! a column is refused as drifted schema on exactly those databases. So before
//! a file adds a column to an existing table, look for any code that ever wrote
//! it there, current code and code since removed alike (`git log
//! -S'"<name>"'`), and choose:
//!
//! - a **rebuild** of the table (SQLite: a `_v2` table, the rows copied by
//!   name, the old table dropped, its indexes recreated; PostgreSQL:
//!   `ADD COLUMN IF NOT EXISTS` with the exact definition the lazy add gave
//!   the column), which gives both kinds of database the same end state —
//!   dev 005, tickets 002 and vector 002 do this, and their headers name the
//!   two kinds; it is possible only for a table no foreign key references;
//! - or a plain `ADD COLUMN`, accepting that such a database is refused by
//!   name and recreated — auth 017 does this for `users`, which six
//!   cascading foreign keys reference, so it cannot be rebuilt.
//!
//! A write path a test exercises fails that test if it names a column no
//! migration declares, as it fails in production. A write path no test
//! reaches is not covered by that.
//!
//! # Databases from before the ledger
//!
//! A block with no ledger rows is classified by its `current_hash` against
//! the block's history ([`HistoricalState`]): a generated, checked-in table
//! of every list state the block's migrations were ever in on main, derived
//! from git (`examples/migration-history.rs`, `migration-history.toml`) and
//! proven by `migration_history_tests`, which rebuilds each state with the
//! runner from before the ledger, upgrades it through this one, and compares
//! the schema with a fresh install's.
//!
//! - **fresh** (`current_hash` empty): every file runs.
//! - **known, supported** ([`Verdict::Applied`]): the files that state
//!   already has applied are recorded `adopted` (with `adopted_from`, the
//!   stored hash) in one transaction, without running, and the rest run.
//! - **known, unsupported** ([`Verdict::Unsupported`]): refused by name
//!   ([`Refusal::UnsupportedState`]), with the state's date and the reason.
//! - **unknown** (any other value: a branch build, the literal `applying` a
//!   crashed run from before #145 left, a corrupt row): refused by name
//!   ([`Refusal::UnknownState`]).
//!
//! Nothing at runtime computes a prefix or probes the schema: the decision
//! is one lookup of the stored hash in a reviewed table.
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
//!   transaction is one `db.batch()`.
//! - **browser**: every runtime build, because loading a bundle is its
//!   deploy. Every block's list runs before the runtime is built, through
//!   [`apply_pending_via_service`] over [`crate::blocks::all_block_migrations`]
//!   (`impresspress_browser::boot`), so that any block's refusal stops the
//!   boot by name, without erasing anything.
//!
//! Cloudflare's request-path builds are the only ones that do not, so step 2
//! is reached only there, by a version serving before its deploy funnel ran.
//!
//! # A shipped file's statements are immutable
//!
//! A file's ledger row holds the [`content_hash`] of the statements that ran,
//! and step 4 refuses a database whose recorded hash differs from the code's:
//! the native server refuses to start, a deploy fails, a browser build stops
//! with an error its boot shell reports without erasing anything. So a
//! change to a shipped migration goes in a NEW file
//! (`migration-content-hashes.txt` records every shipped file's hash, and a
//! test fails at PR time when one moves). [`content_hash`] covers the
//! statements only, so editing a whole-line `--` comment does not move it;
//! it does move the file's [`migration_set_hash`], and the history table's
//! pinning test then names the fixture the edit has to add.
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
        applied_migrations::{self, AppliedMigration},
        block_settings::{self, BlockSettingsPatch},
    },
};
// NOTE: `BlockSettings::state_for` parses only the requested block's entry
// out of the JSON map, avoiding the full-map materialization that
// `from_config_json` would do on every `apply_pending` call.

/// The migration runner from before the ledger, reproduced for test fixtures
/// that rebuild a historical database. Never compiled into a deployment: it
/// is behind `migration-fixtures`, which only test builds enable.
#[cfg(any(test, feature = "migration-fixtures"))]
pub mod legacy_runner;

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

/// One list state a block's migrations were in on main before the per-file
/// ledger: what a database last migrated by that list stores as its
/// `block_settings.current_hash`, and what this build does with it. Generated
/// by `examples/migration-history.rs` into `crate::migration_history`; never
/// written by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoricalState {
    /// [`migration_set_hash`] of the list.
    pub list_hash: &'static str,
    /// The first main commit with this list, as `"<date> <sha8>"`.
    pub first_seen: &'static str,
    /// The list's files, in order, as `basename@<sha8 of the whole text>`.
    pub composition: &'static [&'static str],
    pub verdict: Verdict,
}

/// What a database in a [`HistoricalState`] can do under this build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Today's files the database already has applied, by basename: a
    /// non-empty prefix of the current list. They are adopted; the rest run.
    Applied(&'static [&'static str]),
    /// Not upgradable; the reason, as shown to the operator.
    Unsupported(&'static str),
}

/// A block's migrations, as its `lifecycle(Init)` applies them: the block's
/// name, each dialect's file list and each dialect's [history](HistoricalState).
///
/// One constant per block (`migrations::MIGRATIONS`), handed to
/// [`lifecycle_init`] and listed in [`crate::blocks::all_block_migrations`],
/// so the block's own `Init` and the browser's pre-build apply the same
/// thing. An external consumer's block declares its own, with an empty
/// history if it has no databases from before the ledger.
#[derive(Debug, Clone, Copy)]
pub struct BlockMigrations {
    pub block: &'static str,
    pub sqlite: &'static [MigrationFile<'static>],
    pub postgres: &'static [MigrationFile<'static>],
    pub sqlite_history: &'static [HistoricalState],
    pub postgres_history: &'static [HistoricalState],
}

impl BlockMigrations {
    /// The files and the history for `backend`'s dialect.
    pub fn for_backend(
        &self,
        backend: Backend,
    ) -> (
        &'static [MigrationFile<'static>],
        &'static [HistoricalState],
    ) {
        match backend {
            Backend::Postgres => (self.postgres, self.postgres_history),
            Backend::Sqlite => (self.sqlite, self.sqlite_history),
        }
    }
}

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

/// Pick the block's migration list and history for the active SQL dialect
/// (`WAFER_RUN_SHARED__DATABASE__BACKEND`, read through [`db_backend`]) and
/// forward them to [`apply_pending`].
pub async fn apply_migrations(
    ctx: &dyn Context,
    migrations: &BlockMigrations,
) -> Result<(), MigrationError> {
    let backend = db_backend(ctx).await.map_err(|e| {
        MigrationError::step(
            migrations.block,
            "read the database backend",
            StepError::from(e),
        )
    })?;
    let (files, history) = migrations.for_backend(backend);
    apply_pending(ctx, migrations.block, files, history).await
}

/// `lifecycle(Init)` body shared by every impresspress feature block: on the
/// [`Init`](LifecycleType::Init) event, apply the block's migrations and turn
/// a failure into a [`WaferError`] ([`MigrationError::to_wafer_error`]).
/// Non-`Init` events are a no-op.
///
/// Blocks with extra `Init` work (admin's seed steps, llm's legacy-provider
/// copy + reload) call this first, then run their extra steps; blocks with no
/// migrations at all (the embedding wrappers) simply omit `lifecycle`.
pub async fn lifecycle_init(
    ctx: &dyn Context,
    event: &LifecycleEvent,
    migrations: &BlockMigrations,
) -> Result<(), WaferError> {
    if !matches!(event.event_type, LifecycleType::Init) {
        return Ok(());
    }
    apply_migrations(ctx, migrations)
        .await
        .map_err(|e| e.to_wafer_error())
}

/// Apply `block_name`'s pending `files` through the block's context, when
/// this build may apply them (module docs, "A run, step by step"). Returns
/// at once when `current_hash` in the snapshot's `BlockSettings` matches the
/// list's hash. `history` is the block's [`HistoricalState`] table for the
/// dialect of `files`, consulted only for a block with no ledger rows.
///
/// `block_name` is the full block name (e.g. `"impresspress/files"`); every
/// error names it.
pub async fn apply_pending(
    ctx: &dyn Context,
    block_name: &str,
    files: &[MigrationFile<'_>],
    history: &[HistoricalState],
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
    .apply(files, history, &state.current_hash, &code_hash)
    .await;
    if let Err(e) = &outcome {
        tracing::warn!(error = %e, "migration failed");
    }
    outcome
}

/// Apply `block_name`'s pending migrations straight through a
/// [`DatabaseService`], before the runtime exists.
///
/// The counterpart of [`apply_pending`] for a list applied pre-build: the
/// native server's admin list, whose tables hold the variables and block
/// settings the server reads to build its runtime, and the browser's every
/// list. Same ledger, same plan, same history, same stamp — it reads the
/// block's `block_settings` row, returns when its `current_hash` matches
/// [`migration_set_hash`], and otherwise runs the pending files — so the
/// block's own `Init` then finds the list applied and skips it. A boot that
/// applies a list pre-build always migrates, so there is no
/// `IMPRESSPRESS_RUN_MIGRATIONS` gate to consult. `backend` is the dialect
/// `files` are written in, which renders the ledger's own statements.
///
/// The statements go through `db.exec_raw` / `db.exec_raw_transaction` — the
/// migration-file-runner exception to the no-raw-SQL rule (CLAUDE.md).
pub async fn apply_pending_via_service(
    db: &Arc<dyn DatabaseService>,
    backend: Backend,
    block_name: &str,
    files: &[MigrationFile<'_>],
    history: &[HistoricalState],
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
    .apply(files, history, &current_hash, &code_hash)
    .await
}

/// The hash that identifies a block's migration list: SHA-256 hex of the
/// files' SQL joined with `\n`, in order. The basenames take no part in it.
///
/// What `block_settings.current_hash` holds once a list is fully applied,
/// and what a database from before the ledger is recognised by.
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
/// These are exactly the statements the runner sends, in one transaction,
/// and exactly what [`content_hash`] covers.
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
/// browser's stopped screen say exactly this. It always names the block.
#[derive(Debug)]
pub struct MigrationError {
    block: String,
    /// Boxed: the error travels through every `Init`'s `Result`, and its
    /// largest variant carries a file's statements.
    kind: Box<MigrationErrorKind>,
}

#[derive(Debug)]
enum MigrationErrorKind {
    /// The ledger, the history and the code disagree with the database;
    /// nothing of the refused file ran.
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
    /// Reading or writing the migration record itself failed.
    Step { step: String, error: StepError },
    /// A file's transaction failed, and reading the ledger to decide it
    /// failed too: whether the file applied is not known.
    Undecided {
        file: String,
        failure: TxFailure,
        read: StepError,
    },
}

/// Why the runner refused a block. Each names what the operator can do, and
/// none ran anything of the refused file.
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
    /// The block has a migration record, and `file` adds a column the
    /// database already has: a typed write created it before the migration
    /// ran.
    DriftedSchema { file: String, message: String },
    /// The block's `current_hash` is a list state from before the ledger that
    /// this build does not upgrade.
    UnsupportedState {
        list_hash: String,
        first_seen: String,
        reason: String,
    },
    /// The block's `current_hash` is no list state main ever had.
    UnknownState { list_hash: String },
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

    /// The block the error names.
    pub fn block(&self) -> &str {
        &self.block
    }

    /// The refusal, when the runner refused the block because the database's
    /// record or schema and the code disagree: an outcome of the code and
    /// the stored record, never a statement that failed on the data. A
    /// browser stops on a refusal without erasing anything.
    pub fn refusal(&self) -> Option<&Refusal> {
        match self.kind.as_ref() {
            MigrationErrorKind::Refused(refusal) => Some(refusal),
            MigrationErrorKind::FileFailed { .. }
            | MigrationErrorKind::Step { .. }
            | MigrationErrorKind::Undecided { .. } => None,
        }
    }

    /// The code a failed `Init` answers with: `FailedPrecondition` for a
    /// [`Refusal`]; the database's own classification for a file or a step
    /// that failed (`Unavailable` stays `Unavailable`, so the runtime retries
    /// the `Init`; a statement-budget refusal keeps its code and detail
    /// code); for an undecided file, the ledger read's classification.
    pub fn code(&self) -> ErrorCode {
        match self.kind.as_ref() {
            MigrationErrorKind::Refused(_) => ErrorCode::FailedPrecondition,
            MigrationErrorKind::FileFailed { failure, .. } => failure.code,
            MigrationErrorKind::Step { error, .. } => error.code,
            MigrationErrorKind::Undecided { read, .. } => read.code,
        }
    }

    fn detail_code(&self) -> Option<&str> {
        match self.kind.as_ref() {
            MigrationErrorKind::FileFailed { failure, .. } => failure.detail_code.as_deref(),
            MigrationErrorKind::Step { error, .. } => error.detail_code.as_deref(),
            MigrationErrorKind::Refused(_) | MigrationErrorKind::Undecided { .. } => None,
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
            MigrationErrorKind::Undecided {
                file,
                failure,
                read,
            } => write!(
                f,
                "{block}: migration `{file}` reported a failure ({:?}: {}), and reading the \
                 migration ledger to see whether it applied failed too: {}",
                failure.code, failure.message, read.message
            ),
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

/// The first eight hex digits of a hash, as the history table and the
/// operator see it.
fn short(hash: &str) -> &str {
    hash.get(..8).unwrap_or(hash)
}

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
                "its tracking was lost: no migration record, but its schema exists. Migration \
                 `{file}` met that schema and was rolled back ({message}). Restore {} and {} \
                 from a backup taken before the loss, or recreate the database",
                block_settings::TABLE,
                applied_migrations::TABLE
            ),
            Self::DriftedSchema { file, message } => write!(
                f,
                "its schema drifted: migration `{file}` adds a column the database already has, \
                 which a write created before the migration ran ({message}). Nothing of `{file}` \
                 ran. Recreate the database or restore a backup"
            ),
            Self::UnsupportedState {
                list_hash,
                first_seen,
                reason,
            } => write!(
                f,
                "its migration state {} (main, {first_seen}) cannot be upgraded: {reason}. \
                 Nothing was run. Recreate the database or restore a backup",
                short(list_hash)
            ),
            Self::UnknownState { list_hash } => write!(
                f,
                "its migration state {} is no state main ever had (a branch build, an \
                 interrupted run, or a corrupt record). Nothing was run. Recreate the database \
                 or restore a backup",
                short(list_hash)
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
/// every block's `Init`) or the platform service (a pre-build apply).
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

/// How a block with no ledger rows is applied (module docs, "Databases from
/// before the ledger").
#[derive(Debug, PartialEq, Eq)]
enum Class<'h> {
    Fresh,
    /// Files `..k` are applied in the historical state.
    Adopt(usize),
    Unsupported(&'h HistoricalState, &'static str),
    Unknown,
}

/// Look `current_hash` up in `history`. An `Applied` verdict must name a
/// prefix of `files`; anything else means the table and the list disagree,
/// which `migration_history_tests` rules out for every shipped block.
fn classify<'h>(
    current_hash: &str,
    files: &[MigrationFile<'_>],
    history: &'h [HistoricalState],
) -> Result<Class<'h>, String> {
    if current_hash.is_empty() {
        return Ok(Class::Fresh);
    }
    let Some(state) = history.iter().find(|state| state.list_hash == current_hash) else {
        return Ok(Class::Unknown);
    };
    match state.verdict {
        Verdict::Unsupported(reason) => Ok(Class::Unsupported(state, reason)),
        Verdict::Applied(applied) => {
            let is_prefix = !applied.is_empty()
                && applied.len() <= files.len()
                && applied
                    .iter()
                    .zip(files)
                    .all(|(adopted, (name, _))| adopted == name);
            if is_prefix {
                Ok(Class::Adopt(applied.len()))
            } else {
                Err(format!(
                    "the history entry for {} adopts {applied:?}, which is not a prefix of this \
                     build's list",
                    short(current_hash)
                ))
            }
        }
    }
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
        history: &[HistoricalState],
        current_hash: &str,
        code_hash: &str,
    ) -> Result<(), MigrationError> {
        let internal = |message: String| StepError {
            code: ErrorCode::Internal,
            detail_code: None,
            message,
        };
        let create = applied_migrations::create_table_sql(self.backend)
            .map_err(|message| self.step_error("create the migration ledger", internal(message)))?;
        self.db
            .exec(&create)
            .await
            .map_err(|e| self.step_error("create the migration ledger", e))?;

        let rows = self.read_ledger().await?;
        let (first_pending, fresh) = if rows.is_empty() {
            let class = classify(current_hash, files, history).map_err(|message| {
                self.step_error("read its migration history", internal(message))
            })?;
            match class {
                Class::Fresh => (0, true),
                Class::Adopt(k) => (self.adopt(&files[..k], files, current_hash).await?, false),
                Class::Unsupported(state, reason) => {
                    return Err(MigrationError::refused(
                        self.block,
                        Refusal::UnsupportedState {
                            list_hash: state.list_hash.to_string(),
                            first_seen: state.first_seen.to_string(),
                            reason: reason.to_string(),
                        },
                    ))
                }
                Class::Unknown => {
                    return Err(MigrationError::refused(
                        self.block,
                        Refusal::UnknownState {
                            list_hash: current_hash.to_string(),
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

    /// Read the ledger again to decide a failed transaction of `file`; if
    /// the read fails too, the error carries both failures.
    async fn reread(
        &self,
        file: &str,
        failure: &TxFailure,
    ) -> Result<Vec<AppliedMigration>, MigrationError> {
        self.db
            .ledger(self.block)
            .await
            .map_err(|read| MigrationError {
                block: self.block.to_string(),
                kind: Box::new(MigrationErrorKind::Undecided {
                    file: file.to_string(),
                    failure: failure.clone(),
                    read,
                }),
            })
    }

    fn plan(
        &self,
        files: &[MigrationFile<'_>],
        rows: &[AppliedMigration],
    ) -> Result<usize, MigrationError> {
        plan(files, rows).map_err(|refusal| MigrationError::refused(self.block, refusal))
    }

    /// Record `adopted` as applied without running them, in one transaction,
    /// each row naming `adopted_from`, and return where the list resumes. A
    /// failed adoption is decided by the ledger, like a failed file: when
    /// another migrator's rows now cover the adopted files, its record is
    /// planned against instead.
    async fn adopt(
        &self,
        adopted: &[MigrationFile<'_>],
        files: &[MigrationFile<'_>],
        adopted_from: &str,
    ) -> Result<usize, MigrationError> {
        let statements: Vec<RawStatement> = adopted
            .iter()
            .map(|&(name, sql)| {
                applied_migrations::insert_statement(
                    &AppliedMigration::adopted(self.block, name, &content_hash(sql), adopted_from),
                    self.backend,
                )
            })
            .collect();
        let Err(failure) = self.db.transaction(&statements).await else {
            return Ok(adopted.len());
        };
        let file = format!(
            "{}..{} (adopted)",
            adopted[0].0,
            adopted[adopted.len() - 1].0
        );
        let rows = self.reread(&file, &failure).await?;
        match self.plan(files, &rows)? {
            first_pending if first_pending >= adopted.len() => Ok(first_pending),
            _ => Err(MigrationError {
                block: self.block.to_string(),
                kind: Box::new(MigrationErrorKind::FileFailed {
                    file,
                    statements: statements.into_iter().map(|s| s.query).collect(),
                    failure,
                }),
            }),
        }
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
        let mut statements = vec![applied_migrations::insert_statement(
            &AppliedMigration::ran(self.block, name, &content_hash(sql)),
            self.backend,
        )];
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

        let rows = self.reread(name, &failure).await?;
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
            // The file is refused either way and nothing of it remains; the
            // message only picks which named refusal says so (module docs,
            // "A column that already exists").
            None if is_duplicate_column_error(&failure.message) => {
                let refusal = if fresh {
                    Refusal::TrackingLost {
                        file: name.to_string(),
                        message: failure.message,
                    }
                } else {
                    Refusal::DriftedSchema {
                        file: name.to_string(),
                        message: failure.message,
                    }
                };
                Err(MigrationError::refused(self.block, refusal))
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

/// `true` when an error message looks like a "column already exists"
/// failure from any backend we target.
///
/// - SQLite / D1: `duplicate column name`
/// - PostgreSQL: `column "<name>" of relation "<table>" already exists`
///
/// Substring match is intentional — backends wrap these strings in their
/// own error envelopes and we only care that the canonical phrase appears.
/// It picks which named refusal a file that is not recorded is reported as
/// ([`Refusal::TrackingLost`], [`Refusal::DriftedSchema`]); it never decides
/// whether a file applied. Only the column case: a table or index that
/// already exists is an ordinary failure.
fn is_duplicate_column_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("duplicate column name")
        || (lower.contains("column \"")
            && lower.contains("\" of relation \"")
            && lower.contains("already exists"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform_state::applied_migrations::Origin;

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
        // The `ADD COLUMN` and the `UPDATE` are two statements of the
        // file's one transaction.
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
            assert!(
                stmts[0].to_ascii_uppercase().starts_with("ALTER TABLE"),
                "{dialect}: {}",
                stmts[0]
            );
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
            &[],
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
            apply_pending_via_service(&db, Backend::Sqlite, "test/pending", &files, &[])
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
            apply_pending(&ctx, BLOCK, &files, &[])
                .await
                .expect("apply");
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
        apply_pending(&ctx, BLOCK, &[F1, F2], &[])
            .await
            .expect("apply v1");
        apply_pending(&ctx, BLOCK, &[F1, F2, F3], &[])
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
        apply_pending(&ctx, BLOCK, &[F1, F2], &[])
            .await
            .expect("apply v1");
        let edited: MigrationFile<'static> = (
            F2.0,
            "INSERT INTO ledger_runs (file) VALUES ('002-edited');",
        );
        let err = apply_pending(&ctx, BLOCK, &[F1, edited, F3], &[])
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
        apply_pending(&ctx, BLOCK, &[F1, F2], &[])
            .await
            .expect("apply v1");
        let commented: MigrationFile<'static> = (
            F2.0,
            "-- Counts a run.\nINSERT INTO ledger_runs (file) VALUES ('002');\n-- The end.\n",
        );
        apply_pending(&ctx, BLOCK, &[F1, commented], &[])
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
        apply_pending(&ctx, BLOCK, &[F1, F2], &[])
            .await
            .expect("apply v2");
        let err = apply_pending(&ctx, BLOCK, &[F1], &[])
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
        apply_pending(&ctx, BLOCK, &[F1, F2], &[])
            .await
            .expect("apply");
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
        let err = apply_pending(&ctx, BLOCK, &[F1, F2, F3], &[])
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
        let err = apply_pending(&ctx, BLOCK, &[F1, broken], &[])
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
        apply_pending(&ctx, BLOCK, &[F1, fixed], &[])
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
        let err = apply_pending(&ctx, BLOCK, &[F1, duplicate, F3], &[])
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
        apply_pending(&ctx, BLOCK, &[F1, F2], &[])
            .await
            .expect("apply");
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
        apply_pending(&ctx, BLOCK, &[F1, F2], &[])
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
        for statement in [
            "CREATE TABLE ledger_runs (file TEXT NOT NULL)",
            "ALTER TABLE ledger_runs ADD COLUMN extra TEXT",
        ] {
            db.exec_raw(statement, &[]).await.expect("build the schema");
        }
        let file: MigrationFile<'static> = (
            "001_schema",
            "CREATE TABLE IF NOT EXISTS ledger_runs (file TEXT NOT NULL);\n\
             CREATE TABLE ledger_side (id TEXT);\n\
             ALTER TABLE ledger_runs ADD COLUMN extra TEXT;",
        );
        let err = apply_pending(&ctx, BLOCK, &[file], &[])
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

    /// A table that already exists is not a column that already exists: on
    /// an adopted block it is an ordinary failure, not a drifted schema.
    #[test]
    fn only_a_duplicate_column_reads_as_one() {
        assert!(is_duplicate_column_error("duplicate column name: extra"));
        assert!(is_duplicate_column_error(
            r#"error returned from database: column "extra" of relation "t" already exists"#
        ));
        assert!(!is_duplicate_column_error("table t_v2 already exists"));
        assert!(!is_duplicate_column_error(r#"relation "t" already exists"#));
    }

    /// A history entry for the test block: `files` as the list a database
    /// from before the ledger last applied, with `verdict`.
    fn state(files: &[MigrationFile<'_>], verdict: Verdict) -> HistoricalState {
        HistoricalState {
            list_hash: Box::leak(migration_set_hash(files).into_boxed_str()),
            first_seen: "2026-10-01 0123abcd",
            composition: &[],
            verdict,
        }
    }

    /// The old runner's database for `files`: each statement run as it was,
    /// then the list's hash stamped by the caller's snapshot.
    async fn pre_ledger(ctx: &TestContext, files: &[MigrationFile<'_>]) {
        legacy_runner::apply(&ctx.database_service(), files)
            .await
            .expect("the pre-ledger run");
    }

    /// A database from before the ledger whose record its history entry
    /// says has the first `k` files applied: those are recorded `adopted`,
    /// from that record, without running, and the rest run.
    #[tokio::test]
    async fn a_known_pre_ledger_state_adopts_the_files_its_entry_names() {
        for (k, applied) in [(1, &["001_runs"][..]), (2, &["001_runs", "002_more"][..])] {
            let files = [F1, F2, F3];
            let history = [state(&files[..k], Verdict::Applied(applied))];
            let ctx = ledger_ctx_with_hash(history[0].list_hash).await;
            pre_ledger(&ctx, &files[..k]).await;

            apply_pending(&ctx, BLOCK, &files, &history)
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
            let adopted_from: Vec<String> =
                applied_migrations::list_for_block(&ctx.fixture(), BLOCK)
                    .await
                    .expect("ledger")
                    .into_iter()
                    .filter(|row| row.origin == Origin::Adopted)
                    .map(|row| row.adopted_from)
                    .collect();
            assert_eq!(adopted_from, vec![history[0].list_hash.to_string(); k]);
            assert_eq!(stamped(&ctx).await, Some(migration_set_hash(&files)));
        }
    }

    /// A pre-ledger record whose entry is unsupported is refused by name,
    /// with the entry's date and reason; nothing runs or is recorded.
    #[tokio::test]
    async fn an_unsupported_pre_ledger_state_is_refused_by_name() {
        let files = [F1, F2];
        let history = [state(
            &files[..1],
            Verdict::Unsupported("its 001 was edited after it shipped"),
        )];
        let ctx = ledger_ctx_with_hash(history[0].list_hash).await;
        let err = apply_pending(&ctx, BLOCK, &files, &history)
            .await
            .expect_err("refused");
        assert_eq!(
            err.refusal(),
            Some(&Refusal::UnsupportedState {
                list_hash: history[0].list_hash.to_string(),
                first_seen: "2026-10-01 0123abcd".to_string(),
                reason: "its 001 was edited after it shipped".to_string(),
            })
        );
        let text = err.to_string();
        assert!(
            text.contains("cannot be upgraded: its 001 was edited after it shipped"),
            "{text}"
        );
        assert!(text.contains(&history[0].list_hash[..8]), "{text}");
        assert_eq!(err.code(), ErrorCode::FailedPrecondition);
        assert!(!table_exists(&ctx, "ledger_runs").await, "nothing ran");
        assert!(ledger_rows(&ctx).await.is_empty(), "nothing recorded");
    }

    /// A record no entry names — the `applying` a run that crashed before
    /// #145 left, a branch build's hash — is refused by name: no prefix is
    /// guessed, nothing runs.
    #[tokio::test]
    async fn an_unknown_pre_ledger_state_is_refused_by_name() {
        for current in ["applying", "an-older-list"] {
            let history = [state(&[F1], Verdict::Applied(&["001_runs"]))];
            let ctx = ledger_ctx_with_hash(current).await;
            let err = apply_pending(&ctx, BLOCK, &[F1, F2], &history)
                .await
                .expect_err("refused");
            assert_eq!(
                err.refusal(),
                Some(&Refusal::UnknownState {
                    list_hash: current.to_string()
                })
            );
            assert!(!table_exists(&ctx, "ledger_runs").await, "nothing ran");
            assert!(ledger_rows(&ctx).await.is_empty(), "nothing recorded");
        }
    }

    /// A block that adopted (or ran) files and then meets a column a typed
    /// write already created — a browser profile that never migrated while
    /// newer code wrote into it — is refused by name as drifted; the file
    /// rolls back whole and is not recorded.
    #[tokio::test]
    async fn a_column_a_write_created_before_its_migration_is_refused_as_drifted() {
        let files = [F1, F2];
        let history = [state(&files[..1], Verdict::Applied(&["001_runs"]))];
        let ctx = ledger_ctx_with_hash(history[0].list_hash).await;
        pre_ledger(&ctx, &files[..1]).await;
        // Test-fixture setup: the column an earlier build's typed write added.
        db::ddl(
            &ctx.fixture(),
            "ALTER TABLE ledger_runs ADD COLUMN drifted TEXT",
        )
        .await
        .expect("the lazy column");
        let adds: MigrationFile<'static> = (
            "002_more",
            "CREATE TABLE ledger_side (id TEXT);\n\
             ALTER TABLE ledger_runs ADD COLUMN drifted TEXT;",
        );
        let err = apply_pending(&ctx, BLOCK, &[F1, adds], &history)
            .await
            .expect_err("refused");
        match err.refusal() {
            Some(Refusal::DriftedSchema { file, .. }) => assert_eq!(file, "002_more"),
            other => panic!("expected DriftedSchema, got {other:?}: {err}"),
        }
        assert!(err.to_string().contains("its schema drifted"), "{err}");
        assert!(
            !table_exists(&ctx, "ledger_side").await,
            "nothing of 002 remains"
        );
        assert_eq!(
            ledger_rows(&ctx).await,
            owned(&[("001_runs", Origin::Adopted)])
        );
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
            &[],
        )
        .await
        .expect("admin's schema");
        apply_pending_via_service(&sqlite, Backend::Sqlite, BLOCK, &[F1, F2], &[])
            .await
            .expect("apply v1");
        let racing = RacingDb::new(Arc::clone(&sqlite), None);
        let db: Arc<dyn DatabaseService> = racing.clone();
        apply_pending_via_service(&db, Backend::Sqlite, BLOCK, &[F1, F2, F3], &[])
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
            &[],
        )
        .await
        .expect("admin's schema");
        apply_pending_via_service(&sqlite, Backend::Sqlite, BLOCK, &[F1], &[])
            .await
            .expect("apply v1");
        let claim = applied_migrations::insert_statement(
            &AppliedMigration::ran(BLOCK, F2.0, &content_hash(F2.1)),
            Backend::Sqlite,
        );
        let racing = RacingDb::new(Arc::clone(&sqlite), Some(claim));
        let db: Arc<dyn DatabaseService> = racing.clone();
        apply_pending_via_service(&db, Backend::Sqlite, BLOCK, &[F1, F2, F3], &[])
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

    /// The registry the browser applies before it builds a runtime
    /// (`crate::blocks::all_block_migrations`) and each block's own `Init`
    /// apply the same constant: every block directory that declares
    /// migrations is in the registry's source, every block's non-test
    /// `lifecycle_init` / `apply_migrations` call names its own `MIGRATIONS`,
    /// and every entry this build compiles in is listed once, under a name
    /// whose last segment is the directory it lives in, admin first.
    #[test]
    fn the_registry_and_each_blocks_init_apply_the_same_migrations() {
        let blocks = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/blocks");
        let registry_source = std::fs::read_to_string(blocks.join("mod.rs")).expect("read");
        let mut dirs = Vec::new();
        for entry in std::fs::read_dir(&blocks).expect("read blocks") {
            let dir = entry.expect("entry").path();
            if dir.join("migrations/mod.rs").is_file() {
                dirs.push(dir.file_name().expect("name").to_string_lossy().to_string());
            }
        }
        assert!(dirs.len() >= 12, "{dirs:?}");
        for dir in &dirs {
            assert!(
                registry_source.contains(&format!("&{dir}::migrations::MIGRATIONS,")),
                "{dir} declares migrations but is not in all_block_migrations"
            );
            let mut stack = vec![blocks.join(dir)];
            while let Some(path) = stack.pop() {
                if path.is_dir() {
                    for entry in std::fs::read_dir(&path).expect("read") {
                        stack.push(entry.expect("entry").path());
                    }
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read");
                let deployed = text
                    .find("#[cfg(test)]")
                    .map_or(text.as_str(), |at| &text[..at]);
                for call in ["lifecycle_init(", "apply_migrations("] {
                    for (at, _) in deployed.match_indices(call) {
                        if deployed[..at].ends_with("fn ") {
                            continue;
                        }
                        let args = &deployed[at..];
                        let args = &args[..args.find(';').unwrap_or(args.len())];
                        assert!(
                            args.contains("MIGRATIONS"),
                            "{}: `{call}` applies something other than the block's MIGRATIONS: {args}",
                            path.display()
                        );
                    }
                }
            }
        }
        let registry = crate::blocks::all_block_migrations();
        assert_eq!(registry[0].block, crate::blocks::admin::ADMIN_BLOCK_ID);
        let names: std::collections::BTreeSet<&str> = registry.iter().map(|m| m.block).collect();
        assert_eq!(names.len(), registry.len(), "a block is listed twice");
        for migrations in &registry {
            let dir = crate::test_support::migration_history::block_dir(migrations);
            assert!(
                dirs.iter().any(|d| d == dir),
                "{} lives in no {dir}/",
                migrations.block
            );
        }
    }

    /// The runner from before the ledger is a test fixture: no code compiled
    /// into a deployment names it. Its tolerance of a duplicate `ADD COLUMN`
    /// must never reach a deployed runner again. A module may name it only
    /// below its first `#[cfg(test)]` (where every module keeps its tests);
    /// the three files listed are its declaration, itself, and the history
    /// tests' engine, all behind test-only features (`migration-fixtures`,
    /// `test-support`).
    #[test]
    fn no_deployed_module_names_the_legacy_runner() {
        let allowed = [
            // its declaration, behind `cfg(any(test, feature = "test-support"))`
            "migration_helper.rs",
            "migration_helper/legacy_runner.rs",
            // the history tests' engine, itself test-support only
            "test_support/migration_history.rs",
        ];
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut naming = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let relative = path
                    .strip_prefix(&src)
                    .expect("under src")
                    .to_string_lossy()
                    .replace('\\', "/");
                if allowed.contains(&relative.as_str()) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read");
                let deployed = text
                    .find("#[cfg(test)]")
                    .map_or(text.as_str(), |at| &text[..at]);
                if deployed.contains("legacy_runner") {
                    naming.push(relative);
                }
            }
        }
        assert!(
            naming.is_empty(),
            "modules naming the legacy runner: {naming:?}"
        );
    }
}
