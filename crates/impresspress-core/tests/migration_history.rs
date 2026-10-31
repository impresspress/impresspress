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
//!   manifest), counted statement by statement through the D1-shaped
//!   decorator, fits one D1 invocation's default budget.
//! - **The history table:** every state's composition resolves to its own
//!   hash; the table's invariants hold; every supported state, rebuilt with
//!   the runner from before the ledger and upgraded through today's,
//!   converges on a fresh install's schema with every seeded row intact and
//!   the ledger it should have; every unsupported state is refused by name
//!   and changes nothing; and each supported state reached from its block's
//!   first state is recorded in `migration-history-crossing.txt`.
//! - **Unknown states** are refused by name.

use std::{collections::BTreeSet, sync::Arc};

use impresspress_core::{
    migration_helper::{self, Refusal},
    test_support::{
        d1_shaped::{Carrier, D1Shaped, Ledger, RoundTrip},
        migration_history as history,
    },
};
use wafer_core::interfaces::database::service::DatabaseService;
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

/// An empty SQLite database behind the D1-shaped decorator, recording every
/// statement it is sent: what a Cloudflare fresh install sends D1.
fn d1_shaped() -> (tempfile::TempDir, Arc<D1Shaped>, Arc<Ledger>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let ledger = Arc::new(Ledger::default());
    let db = Arc::new(D1Shaped::open(
        &dir.path().join("db.sqlite3"),
        Arc::clone(&ledger),
    ));
    (dir, db, ledger)
}

/// The round trips that were transactions (each migration file is one), and
/// the statements they carried.
fn transactions(trips: &[RoundTrip]) -> (usize, usize) {
    let transactions: Vec<&RoundTrip> = trips
        .iter()
        .filter(|trip| trip.carrier == Carrier::Transaction)
        .collect();
    (
        transactions.len(),
        transactions.iter().map(|trip| trip.statements()).sum(),
    )
}

/// Each block's whole list applies to an empty database, admin first, each
/// file as one transaction, with nothing failing — the rule that lets the
/// runner tolerate nothing. One transaction per file, its ledger row
/// included, is counted.
#[tokio::test]
async fn every_block_applies_fresh_atomically() {
    let (_dir, shaped, ledger) = d1_shaped();
    let db: Arc<dyn DatabaseService> = shaped;
    let results = history::apply_fresh(&db, Backend::Sqlite).await;
    let (files, statements) = assert_applied(results);
    let (transactions, in_transactions) = transactions(&ledger.take());
    assert_eq!(transactions, files, "one transaction per file");
    assert_eq!(
        in_transactions,
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
/// and its ledger row, plus the runner's own (the ledger table, the ledger
/// read, the record's read and write, the fast-path read, and the
/// executor's introspection of each table it touches) — must fit the
/// default per-invocation limit ([`D1_QUERIES_PER_INVOCATION_DEFAULT`]).
/// Counted through the D1-shaped decorator, statement by statement as D1
/// receives them, not computed, so the runner's next change is measured.
///
/// This is the migrations' share alone. The whole invocation — these plus
/// the funnel's seeds, config reads and closing reads — is
/// `tests/d1_read_cost.rs`'s provision phase, which asserts the same limit
/// over all of it.
///
/// **The line is interim.** It used to be half the limit, counted per call,
/// and the ledger plan's decision U-IP5-2 was that crossing it schedules
/// batch accounting rather than a higher line. Counted per statement, the
/// install had already crossed it (534 > 500), so batch accounting is the
/// next producer-first change (PR267-review M1): wafer-run counts a
/// `db.batch()` as the one call D1 counts, then the adapter's guard — and
/// this test, `config_vars.rs`'s budget doc and the harness's — re-derive in
/// calls (172 for the migrations and 214 for the whole prepare, as measured today).
/// Until then the line is the adapter's own refusal point, the limit itself.
///
/// It counts what a Cloudflare build migrates ([`cloudflare_blocks`]): a
/// block the Worker cannot compile never runs in its prepare. Every one of
/// those blocks must be compiled into this test, or the count is short.
///
/// [`D1_QUERIES_PER_INVOCATION_DEFAULT`]: impresspress_core::config_vars::D1_QUERIES_PER_INVOCATION_DEFAULT
#[tokio::test]
async fn a_cloudflare_fresh_install_fits_the_d1_budget() {
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

    let (_dir, shaped, ledger) = d1_shaped();
    let db: Arc<dyn DatabaseService> = shaped;
    let results = history::apply_fresh_blocks(&db, Backend::Sqlite, &selected).await;
    let (files, statements) = assert_applied(results);

    let trips = ledger.take();
    let (_, in_transactions) = transactions(&trips);
    assert_eq!(
        in_transactions,
        statements + files,
        "each file's statements and its ledger row, in its transaction"
    );
    let sent: usize = trips.iter().map(RoundTrip::statements).sum();
    println!(
        "a Cloudflare fresh install sends {sent}: {statements} file statements, {files} \
         ledger rows, {} of the runner's own; {} D1 calls",
        sent - statements - files,
        trips.len()
    );
    let limit = impresspress_core::config_vars::D1_QUERIES_PER_INVOCATION_DEFAULT as usize;
    assert!(
        sent <= limit,
        "a Cloudflare fresh install sends {sent} statements ({statements} in files, {files} \
         ledger rows, the rest the runner's own), more than the {limit} D1 queries \
         `/_deploy/prepare` may run in its one invocation"
    );
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
/// without it could take no typed write.
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
