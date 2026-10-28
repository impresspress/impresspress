//! The migration history and the atomic fresh install, on SQLite
//! (`impresspress_core::test_support::migration_history`; the PostgreSQL lane
//! is `impresspress-native`'s `tests/postgres_migration_history.rs`).
//!
//! - **Every block applies fresh, atomically:** each block's whole list from
//!   an empty database, admin first, each file one transaction, through the
//!   real runner, with no failure. This is what the two file edits of the
//!   ledger release (auth 016, admin 009) make true.
//! - **The fresh-install budget:** what that install sends, counted through
//!   a decorator, fits half of one D1 invocation's default budget.
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
///
/// And the budget: `/_deploy/prepare` applies a fresh install in ONE Worker
/// invocation, and until a D1 batch is admitted as one call the adapter
/// admits each statement, so the statements sent — every file's statements
/// and its ledger row, plus each block's own calls (the ledger table, the
/// ledger read, the record's read and write, the fast-path read) — must fit
/// half of the default budget ([`D1_QUERIES_PER_INVOCATION_DEFAULT`]), with
/// the funnel's seeds and config reads in what is left. Counted here through
/// a decorator, not computed, so the runner's next change is measured.
///
/// [`D1_QUERIES_PER_INVOCATION_DEFAULT`]: impresspress_core::config_vars::D1_QUERIES_PER_INVOCATION_DEFAULT
#[tokio::test]
async fn every_block_applies_fresh_atomically() {
    let (_dir, inner) = sqlite();
    let counting = Arc::new(Counting {
        inner,
        transactions: AtomicUsize::new(0),
        statements_in_transactions: AtomicUsize::new(0),
        other_calls: AtomicUsize::new(0),
    });
    let db: Arc<dyn DatabaseService> = counting.clone();
    let results = history::apply_fresh(&db, Backend::Sqlite).await;
    let failures: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert!(failures.is_empty(), "a fresh install failed: {failures:?}");

    let applied: Vec<(String, usize, usize)> = results.into_iter().map(Result::unwrap).collect();
    let files: usize = applied.iter().map(|(_, f, _)| f).sum();
    let statements: usize = applied.iter().map(|(_, _, s)| s).sum();
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

    let sent = statements + files + counting.other_calls.load(Ordering::SeqCst);
    println!(
        "a fresh install sends {sent}: {statements} file statements, {files} ledger rows, {} \
         other calls",
        sent - statements - files
    );
    let limit = impresspress_core::config_vars::D1_QUERIES_PER_INVOCATION_DEFAULT as usize;
    assert!(
        sent <= limit / 2,
        "a fresh install sends {sent} statements ({statements} in files, {files} ledger rows, \
         the rest the runner's own calls), more than half of the {limit} D1 queries \
         `/_deploy/prepare` may run in its one invocation"
    );
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
