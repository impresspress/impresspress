//! The migration history and the atomic fresh install, on SQLite
//! (`impresspress_core::test_support::migration_history`; the PostgreSQL lane
//! is `impresspress-native`'s `tests/postgres_migration_history.rs`).
//!
//! - **Every block applies fresh, atomically:** each block's whole list from
//!   an empty database, admin first, each file one transaction, through the
//!   real runner, with no failure. This is what the two file edits of the
//!   ledger release (auth 016, admin 009) make true.
//! - **The fresh-install budget:** what a Cloudflare build's install sends
//!   (the blocks `impresspress-cloudflare` can compile, read from its
//!   manifest), counted through a decorator, fits half of one D1
//!   invocation's default budget.
//! - **The history table:** every state's composition resolves to its own
//!   hash; the table's invariants hold; every supported state, rebuilt with
//!   the runner from before the ledger and upgraded through today's,
//!   converges on a fresh install's schema with every seeded row intact and
//!   the ledger it should have; every unsupported state is refused by name
//!   and changes nothing; and each supported state reached from its block's
//!   first state is recorded in `migration-history-crossing.txt`.
//! - **Unknown states** are refused by name.

use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use impresspress_core::{
    migration_helper::{self, Refusal},
    test_support::migration_history as history,
};
use wafer_core::interfaces::database::service::{DatabaseService, RawStatement, TransactionError};
use wafer_sql_utils::Backend;

/// An empty SQLite database in its own directory, file-backed as native
/// opens one.
fn sqlite() -> (tempfile::TempDir, Arc<dyn DatabaseService>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db: Arc<dyn DatabaseService> = Arc::new(
        wafer_block_sqlite::service::SQLiteDatabaseService::open(
            dir.path().join("db.sqlite3").to_str().expect("utf-8 path"),
        )
        .expect("open sqlite"),
    );
    (dir, db)
}

/// Counts what reaches the database: transactions and the statements in
/// them, and every other call that sends SQL.
struct Counting {
    inner: Arc<dyn DatabaseService>,
    transactions: AtomicUsize,
    statements_in_transactions: AtomicUsize,
    other_calls: AtomicUsize,
}

impl Counting {
    fn inner_service(&self) -> &dyn DatabaseService {
        self.other_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.as_ref()
    }
}

wafer_core::forward_database_service! {
    impl DatabaseService for Counting {
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
            self.transactions.fetch_add(1, Ordering::SeqCst);
            self.statements_in_transactions
                .fetch_add(statements.len(), Ordering::SeqCst);
            self.inner.exec_raw_transaction(statements).await
        }
    }
}

/// Each block's whole list applies to an empty database, admin first, each
/// file as one transaction, with nothing failing — the rule that lets the
/// runner tolerate nothing. One transaction per file, its ledger row
/// included, is counted.
#[tokio::test]
async fn every_block_applies_fresh_atomically() {
    let (_dir, inner) = sqlite();
    let counting = counting(inner);
    let db: Arc<dyn DatabaseService> = counting.clone();
    let results = history::apply_fresh(&db, Backend::Sqlite).await;
    let (files, statements) = assert_applied(results);
    assert_eq!(
        counting.transactions.load(Ordering::SeqCst),
        files,
        "one transaction per file"
    );
    assert_eq!(
        counting.statements_in_transactions.load(Ordering::SeqCst),
        statements + files,
        "each file's statements and its ledger row, in its transaction"
    );
}

/// Every block in the migration registry, with the `impresspress-core`
/// feature that compiles it in (`None`: every build has it).
///
/// The fresh-install budget below is a Cloudflare limit, so it counts the
/// blocks a Cloudflare build can compile: those with no feature, and those
/// whose feature `impresspress-cloudflare` passes through (read from its
/// `Cargo.toml`, as `manifest_block_parity.rs` reads it). A block added to
/// the registry must be added here, or
/// `every_registered_block_names_its_feature` fails.
const BLOCK_FEATURES: &[(&str, Option<&str>)] = &[
    ("impresspress/admin", None),
    ("wafer-run/auth", None),
    ("impresspress/dev", Some("site-runtime")),
    ("impresspress/files", Some("block-files")),
    ("impresspress/legalpages", Some("block-legalpages")),
    ("impresspress/llm", Some("block-llm")),
    ("impresspress/messages", Some("block-messages")),
    ("impresspress/products", Some("block-products")),
    ("impresspress/signal", Some("block-signal")),
    ("impresspress/tickets", Some("block-tickets")),
    ("impresspress/userportal", Some("block-userportal")),
    ("impresspress/vector", Some("block-vector")),
];

/// The `[features]` table of the crate in `crate_dir`, as
/// `name -> [implied…]`.
fn manifest_features(crate_dir: &str) -> std::collections::BTreeMap<String, Vec<String>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(crate_dir)
        .join("Cargo.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let manifest: toml::Table = text
        .parse()
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .map(|table| {
            table
                .iter()
                .map(|(name, implied)| {
                    let implied = implied
                        .as_array()
                        .unwrap_or_else(|| panic!("feature `{name}` is not an array"))
                        .iter()
                        .map(|v| v.as_str().expect("a feature name").to_string())
                        .collect();
                    (name.clone(), implied)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `impresspress-core` features `impresspress-cloudflare` passes
/// through: each `X = ["impresspress-core/X"]` of its `[features]`.
fn cloudflare_passthroughs() -> BTreeSet<String> {
    manifest_features("impresspress-cloudflare")
        .into_iter()
        .filter(|(name, implied)| implied == &[format!("impresspress-core/{name}")])
        .map(|(name, _)| name)
        .collect()
}

/// The blocks a Cloudflare build can compile, by [`BLOCK_FEATURES`].
fn cloudflare_blocks() -> BTreeSet<&'static str> {
    let passthroughs = cloudflare_passthroughs();
    BLOCK_FEATURES
        .iter()
        .filter(|(_, feature)| feature.is_none_or(|f| passthroughs.contains(f)))
        .map(|(block, _)| *block)
        .collect()
}

/// [`BLOCK_FEATURES`] names every block this build registers, each once,
/// and every feature it names is one `impresspress-core` declares.
#[test]
fn every_registered_block_names_its_feature() {
    let core = manifest_features("impresspress-core");
    let named: BTreeSet<&str> = BLOCK_FEATURES.iter().map(|(block, _)| *block).collect();
    assert_eq!(named.len(), BLOCK_FEATURES.len(), "a block is listed twice");
    for migrations in history::blocks() {
        assert!(
            named.contains(migrations.block),
            "{} migrates but `BLOCK_FEATURES` does not name it: add it with the \
             impresspress-core feature that compiles it in",
            migrations.block
        );
    }
    for (block, feature) in BLOCK_FEATURES {
        if let Some(feature) = feature {
            assert!(
                core.contains_key(*feature),
                "{block}: impresspress-core declares no feature `{feature}`"
            );
        }
    }
}

/// The budget: `/_deploy/prepare` applies a fresh install in ONE Worker
/// invocation, and until a D1 batch is admitted as one call the adapter
/// admits each statement, so the statements sent — every file's statements
/// and its ledger row, plus each block's own calls (the ledger table, the
/// ledger read, the record's read and write, the fast-path read) — must fit
/// half of the default budget ([`D1_QUERIES_PER_INVOCATION_DEFAULT`]), with
/// the funnel's seeds and config reads in what is left. Counted here through
/// a decorator, not computed, so the runner's next change is measured.
///
/// It counts what a Cloudflare build migrates ([`cloudflare_blocks`]): a
/// block the Worker cannot compile never runs in its prepare. Every one of
/// those blocks must be compiled into this test, or the count is short.
///
/// [`D1_QUERIES_PER_INVOCATION_DEFAULT`]: impresspress_core::config_vars::D1_QUERIES_PER_INVOCATION_DEFAULT
#[tokio::test]
async fn a_cloudflare_fresh_install_fits_half_the_d1_budget() {
    let on_cloudflare = cloudflare_blocks();
    let selected: Vec<_> = history::blocks()
        .into_iter()
        .filter(|m| on_cloudflare.contains(m.block))
        .collect();
    let compiled: BTreeSet<&str> = selected.iter().map(|m| m.block).collect();
    assert_eq!(
        compiled, on_cloudflare,
        "this build does not compile every block a Cloudflare build can, so the \
         count would be short; run it with impresspress-core's default features"
    );

    let (_dir, inner) = sqlite();
    let counting = counting(inner);
    let db: Arc<dyn DatabaseService> = counting.clone();
    let results = history::apply_fresh_blocks(&db, Backend::Sqlite, &selected).await;
    let (files, statements) = assert_applied(results);

    let sent = statements + files + counting.other_calls.load(Ordering::SeqCst);
    println!(
        "a Cloudflare fresh install sends {sent}: {statements} file statements, {files} \
         ledger rows, {} other calls",
        sent - statements - files
    );
    let limit = impresspress_core::config_vars::D1_QUERIES_PER_INVOCATION_DEFAULT as usize;
    assert!(
        sent <= limit / 2,
        "a Cloudflare fresh install sends {sent} statements ({statements} in files, {files} \
         ledger rows, the rest the runner's own calls), more than half of the {limit} D1 \
         queries `/_deploy/prepare` may run in its one invocation"
    );
}

/// A [`Counting`] decorator over `inner`, every counter at zero.
fn counting(inner: Arc<dyn DatabaseService>) -> Arc<Counting> {
    Arc::new(Counting {
        inner,
        transactions: AtomicUsize::new(0),
        statements_in_transactions: AtomicUsize::new(0),
        other_calls: AtomicUsize::new(0),
    })
}

/// Assert a fresh install had no failure; the files and statements it sent.
fn assert_applied(results: Vec<Result<(String, usize, usize), String>>) -> (usize, usize) {
    let failures: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert!(failures.is_empty(), "a fresh install failed: {failures:?}");
    let applied: Vec<(String, usize, usize)> = results.into_iter().map(Result::unwrap).collect();
    (
        applied.iter().map(|(_, f, _)| f).sum(),
        applied.iter().map(|(_, _, s)| s).sum(),
    )
}

#[test]
fn every_states_composition_is_exact() {
    for backend in [Backend::Sqlite, Backend::Postgres] {
        let problems = history::check_compositions(backend);
        assert!(problems.is_empty(), "{problems:#?}");
    }
}

#[test]
fn the_history_tables_invariants_hold() {
    for backend in [Backend::Sqlite, Backend::Postgres] {
        let problems = history::check_invariants(backend);
        assert!(problems.is_empty(), "{problems:#?}");
    }
}

/// The crossing record's lines for `dialect`, for the blocks this build
/// compiles in.
pub fn recorded_crossing(dialect: &str) -> BTreeSet<String> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migration-history-crossing.txt");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter(|l| l.split(' ').nth(1) == Some(dialect))
        // Only the blocks this build compiles in: a block behind a feature
        // that is off has no states to cross here.
        .filter(|l| {
            let block = l.split(' ').next().unwrap_or_default();
            history::blocks().iter().any(|m| m.block == block)
        })
        .map(str::to_string)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_state_upgrades_as_its_entry_says() {
    let dirs = std::sync::Mutex::new(Vec::new());
    let new_db = || {
        let (dir, db) = sqlite();
        dirs.lock().expect("dirs").push(dir);
        Box::pin(async move { db }) as std::pin::Pin<Box<dyn std::future::Future<Output = _>>>
    };
    let report = history::check_states(Backend::Sqlite, &new_db).await;
    assert!(report.problems.is_empty(), "{:#?}", report.problems);

    let actual: BTreeSet<String> = report.crossing.into_iter().collect();
    let recorded = recorded_crossing("sqlite");
    assert!(
        actual == recorded,
        "migration-history-crossing.txt does not match what the sqlite states do.\n\
         Only in the run:\n  {}\nOnly in the file:\n  {}",
        actual
            .difference(&recorded)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  "),
        recorded
            .difference(&actual)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  "),
    );
}

/// A record main never had — the `applying` a run that crashed before #145
/// left, or a hash from a branch build — is refused by name: nothing runs,
/// nothing is recorded, the record stays.
#[tokio::test]
async fn an_unknown_state_is_refused_by_name() {
    for record in [
        "applying",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    ] {
        let (_dir, db) = sqlite();
        history::fresh_admin(&db, Backend::Sqlite)
            .await
            .expect("admin's schema");
        let auth = &impresspress_core::blocks::auth::migrations::MIGRATIONS;
        history::stamp(&db, auth.block, record)
            .await
            .expect("stamp the record");
        let err = history::upgrade(&db, Backend::Sqlite, auth)
            .await
            .expect_err("refused");
        assert_eq!(
            err.refusal(),
            Some(&Refusal::UnknownState {
                list_hash: record.to_string()
            })
        );
        assert!(err.to_string().contains("no state main ever had"), "{err}");
        assert!(history::ledger(&db, auth.block)
            .await
            .expect("ledger")
            .is_empty());
        assert_eq!(
            history::current_hash(&db, auth.block)
                .await
                .expect("record"),
            record
        );
        assert!(
            !history::Schema::read(&db, Backend::Sqlite)
                .await
                .expect("schema")
                .tables
                .contains_key("wafer_run__auth__users"),
            "no file ran"
        );
    }
}

/// A list that has not changed since the table was derived is in it — and
/// is taken by the fast path before the table is read — so its entry must
/// adopt every file.
#[test]
fn todays_lists_in_the_table_adopt_every_file() {
    for migrations in history::blocks() {
        for backend in [Backend::Sqlite, Backend::Postgres] {
            let (files, history_rows) = migrations.for_backend(backend);
            if files.is_empty() {
                continue;
            }
            let today = migration_helper::migration_set_hash(files);
            if let Some(state) = history_rows.iter().find(|s| s.list_hash == today) {
                assert!(
                    matches!(
                        state.verdict,
                        migration_helper::Verdict::Applied(applied) if applied.len() == files.len()
                    ),
                    "{} {}: {:?}",
                    migrations.block,
                    &today[..8],
                    state.verdict
                );
            }
        }
    }
}

/// Every table a fresh install's migrations create declares `updated_at`:
/// the executor stamps it on every typed create and update, so a table
/// without it could take no typed write under strict schema.
///
/// The one table it does not hold for is the migration ledger, which no
/// migration creates: the runner creates it and writes its rows itself, as
/// raw statements inside each file's transaction, and nothing writes it
/// typed.
#[tokio::test]
async fn every_table_declares_updated_at() {
    let (_dir, db) = sqlite();
    let (files, _) = assert_applied(history::apply_fresh(&db, Backend::Sqlite).await);
    assert!(files > 0);
    let schema = history::Schema::read(&db, Backend::Sqlite)
        .await
        .expect("read the schema");
    let lacking = history::tables_without_updated_at(&schema);
    assert!(
        lacking.is_empty(),
        "tables a fresh install creates without `updated_at`: {lacking:?}"
    );
}
