//! The PostgreSQL lane of `impresspress-core`'s `tests/migration_history.rs`:
//! the same engine (`impresspress_core::test_support::migration_history`)
//! over PostgreSQL databases.
//!
//! - **Every block applies fresh, atomically:** each block's whole
//!   PostgreSQL list from an empty database, admin first, each file one
//!   transaction, through the real runner, with no failure.
//! - **Every table declares `updated_at`** on a fresh install (the ledger
//!   aside), as on SQLite.
//! - **The stamped-column files** (auth 017, dev 005, tickets 002, vector
//!   002) are right on a fresh database and on one whose writes created
//!   their columns lazily before strict schema was the only behaviour, the
//!   lazily created column's type, nullability and default included, and
//!   auth 017 is refused by name where a password reset was stored lazily
//!   (`impresspress_core::test_support::stamped_columns`).
//! - **Auth 018** drops the JWT blocklist and nothing else, and on a
//!   database without the table fails whole with PostgreSQL's own error,
//!   recording nothing (`impresspress_core::test_support::jwt_blocklist_drop`).
//! - **Legalpages 003** seeds the two default documents into an empty table
//!   and nothing into a table holding any document, and once recorded leaves
//!   an emptied table empty
//!   (`impresspress_core::test_support::legalpages_default_documents`).
//! - **Every state upgrades as its entry says:** every supported entry of
//!   every block's PostgreSQL history, rebuilt with the runner from before
//!   the ledger and upgraded through today's, converges on a fresh install's
//!   schema with every seeded row intact and the ledger it should have; every
//!   unsupported entry is refused by name and changes nothing; and each
//!   supported state reached from an older one is the line
//!   `migration-history-crossing.txt` records for it.
//!
//! Needs a server, so each is `#[ignore]`d. CI's `test-postgres` job runs
//! them with `IMPRESSPRESS_TEST_POSTGRES_URL` naming a database on a server
//! they may create others on: each case gets an empty database of its own,
//! dropped when the test ends.

#![cfg(feature = "postgres")]

use std::{
    collections::BTreeSet,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use impresspress_core::test_support::{
    jwt_blocklist_drop, legalpages_default_documents, migration_history as history, stamped_columns,
};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_sql_utils::Backend;

const URL_VAR: &str = "IMPRESSPRESS_TEST_POSTGRES_URL";

/// Creates an empty database per case on the server `IMPRESSPRESS_TEST_POSTGRES_URL`
/// names, and drops them all when dropped.
struct Server {
    url: String,
    admin: Arc<dyn DatabaseService>,
    prefix: String,
    next: AtomicUsize,
    created: Mutex<Vec<String>>,
}

impl Server {
    async fn connect(prefix: &str) -> Self {
        let url = std::env::var(URL_VAR)
            .unwrap_or_else(|_| panic!("{URL_VAR} must name a PostgreSQL database"));
        let admin = impresspress_native::make_postgres_database_service(&url)
            .await
            .expect("connect to PostgreSQL");
        Self {
            url,
            admin,
            prefix: prefix.to_string(),
            next: AtomicUsize::new(0),
            created: Mutex::new(Vec::new()),
        }
    }

    /// A new, empty database. Test-fixture setup.
    async fn database(&self) -> Arc<dyn DatabaseService> {
        let name = format!(
            "{}_{}",
            self.prefix,
            self.next.fetch_add(1, Ordering::SeqCst)
        );
        self.admin
            .exec_raw(&format!("DROP DATABASE IF EXISTS {name}"), &[])
            .await
            .expect("drop a leftover database");
        self.admin
            .exec_raw(&format!("CREATE DATABASE {name}"), &[])
            .await
            .expect("create the case's database");
        self.created.lock().expect("names").push(name.clone());
        let (base, _) = self.url.rsplit_once('/').expect("a database URL");
        impresspress_native::make_postgres_database_service(&format!("{base}/{name}"))
            .await
            .expect("connect to the case's database")
    }

    /// Drop every database this server created. The connections to them are
    /// the caller's, so this runs after the cases are done.
    async fn drop_all(&self) {
        let names = std::mem::take(&mut *self.created.lock().expect("names"));
        for name in names {
            // Test-fixture teardown; `WITH (FORCE)` ends the pool's sessions.
            let _ = self
                .admin
                .exec_raw(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"), &[])
                .await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn every_block_applies_fresh_atomically_on_postgres() {
    let server = Server::connect("impresspress_fresh").await;
    let db = server.database().await;
    let results = history::apply_fresh(&db, Backend::Postgres).await;
    let applied: Vec<&(String, usize, usize)> =
        results.iter().filter_map(|r| r.as_ref().ok()).collect();
    let failures: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    println!("applied {applied:?}");
    drop(db);
    server.drop_all().await;
    assert!(failures.is_empty(), "a fresh install failed: {failures:?}");
    assert!(
        applied.len() >= 10,
        "only {} blocks applied: {applied:?}",
        applied.len()
    );
}

/// The crossing record's lines for `dialect`, for the blocks this build
/// compiles in.
fn recorded_crossing(dialect: &str) -> BTreeSet<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../impresspress-core/migration-history-crossing.txt");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
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
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn every_state_upgrades_as_its_entry_says_on_postgres() {
    let server = Server::connect("impresspress_history").await;
    let new_db = || {
        Box::pin(server.database()) as Pin<Box<dyn Future<Output = Arc<dyn DatabaseService>> + '_>>
    };
    let report = history::check_states(Backend::Postgres, &new_db).await;
    server.drop_all().await;
    assert!(report.problems.is_empty(), "{:#?}", report.problems);

    let actual: BTreeSet<String> = report.crossing.into_iter().collect();
    let recorded = recorded_crossing("postgres");
    assert!(
        actual == recorded,
        "migration-history-crossing.txt does not match what the postgres states do.\n\
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn every_table_declares_updated_at_on_postgres() {
    let server = Server::connect("impresspress_updated_at").await;
    let db = server.database().await;
    let results = history::apply_fresh(&db, Backend::Postgres).await;
    let failures: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    let schema = history::Schema::read(&db, Backend::Postgres).await;
    drop(db);
    server.drop_all().await;
    assert!(failures.is_empty(), "a fresh install failed: {failures:?}");
    let lacking = history::tables_without_updated_at(&schema.expect("read the schema"));
    assert!(
        lacking.is_empty(),
        "tables a fresh install creates without `updated_at`: {lacking:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn the_stamped_column_files_are_right_for_both_kinds_of_database_on_postgres() {
    let server = Server::connect("impresspress_stamped").await;
    let new_db = || {
        Box::pin(server.database()) as Pin<Box<dyn Future<Output = Arc<dyn DatabaseService>> + '_>>
    };
    let problems = stamped_columns::check(Backend::Postgres, &new_db).await;
    server.drop_all().await;
    assert!(problems.is_empty(), "{problems:#?}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn auth_018_drops_the_blocklist_and_nothing_else_on_postgres() {
    let server = Server::connect("impresspress_auth_018").await;
    let new_db = || {
        Box::pin(server.database()) as Pin<Box<dyn Future<Output = Arc<dyn DatabaseService>> + '_>>
    };
    let problems = jwt_blocklist_drop::check(Backend::Postgres, &new_db).await;
    server.drop_all().await;
    assert!(problems.is_empty(), "{problems:#?}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn legalpages_003_seeds_the_defaults_into_an_empty_table_once_on_postgres() {
    let server = Server::connect("impresspress_legalpages_003").await;
    let new_db = || {
        Box::pin(server.database()) as Pin<Box<dyn Future<Output = Arc<dyn DatabaseService>> + '_>>
    };
    let problems = legalpages_default_documents::check(Backend::Postgres, &new_db).await;
    server.drop_all().await;
    assert!(problems.is_empty(), "{problems:#?}");
}
