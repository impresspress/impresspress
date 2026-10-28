//! A native boot applies the database's pending block migrations before it
//! serves, applies nothing that is already applied, refuses to start when a
//! migration fails or when the database's migration ledger and the build
//! disagree, adopts what a database from before the ledger recorded when the
//! history table names that record, refuses one it does not support or does
//! not know, and lets only one of two processes booting together apply a
//! list.
//!
//! The real blocks' pre-ledger databases are built by the runner from before
//! the ledger (`impresspress_core::test_support::migration_history`) at
//! states main had, and booted: auth and admin then run only the files their
//! records lack, and admin's 009 leaves no `blessed_hash` column whichever
//! `001` built the table.
//!
//! Each boot here is `start_native` — what `impresspress serve` runs — over a
//! fresh `DatabaseService` on the same database, as a restarted or a second
//! process would open it, with a consumer block whose migration list the test
//! chooses. Its `002` and `003` insert a row naming themselves, so the rows
//! are the number of times each really ran.
//!
//! The SQLite tests run everywhere. The PostgreSQL one needs a server, so it
//! is `#[ignore]`d; CI's `test-postgres` job runs it with
//! `IMPRESSPRESS_TEST_POSTGRES_URL` naming a database it may create others
//! from.

use std::{collections::HashMap, sync::Arc, time::Duration};

use impresspress_core::{
    blocks::{admin, auth},
    migration_helper::{self, BlockMigrations, HistoricalState, MigrationFile, Verdict},
    platform_state::{
        applied_migrations::{self, Origin},
        block_settings,
    },
    test_support::migration_history as history,
};
use impresspress_native::{InfraConfig, ListenerEnv};
use impresspress_server::{start_native, AppHooks};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_run::{context::Context, InputStream, LifecycleEvent, Message, OutputStream, WaferError};
use wafer_sql_utils::Backend;

const BLOCK: &str = "test/migrating";
const FLOW: &str = "test-main";
const ANSWER: &str = "served";
const RUNS: &str = "test__migrating__runs";

/// The list an older build shipped.
const V1: &[MigrationFile<'static>] = &[(
    "001_runs",
    "CREATE TABLE IF NOT EXISTS test__migrating__runs (file TEXT NOT NULL);",
)];

/// The list a newer build ships: `002` is pending on a database `V1` built.
const V2: &[MigrationFile<'static>] = &[
    V1[0],
    (
        "002_count_runs",
        "INSERT INTO test__migrating__runs (file) VALUES ('002');",
    ),
];

/// A build after `V2`: `003` is pending on a database `V2` built.
const V3: &[MigrationFile<'static>] = &[
    V1[0],
    V2[1],
    (
        "003_count_runs",
        "INSERT INTO test__migrating__runs (file) VALUES ('003');",
    ),
];

/// `V2` with its applied `002` edited: a statement changed, not a comment.
const V2_EDITED: &[MigrationFile<'static>] = &[
    V1[0],
    (
        "002_count_runs",
        "INSERT INTO test__migrating__runs (file) VALUES ('002-edited');",
    ),
];

/// A newer build whose `002` the database refuses.
const BROKEN: &[MigrationFile<'static>] = &[
    V1[0],
    (
        "002_broken",
        "INSERT INTO test__migrating__no_such_table (file) VALUES ('002');",
    ),
];

/// The consumer block's migrations: `files` in both dialects, and `history`
/// as its pre-ledger history table.
fn listed(
    files: &'static [MigrationFile<'static>],
    history: &'static [HistoricalState],
) -> &'static BlockMigrations {
    Box::leak(Box::new(BlockMigrations {
        block: BLOCK,
        sqlite: files,
        postgres: files,
        sqlite_history: history,
        postgres_history: history,
    }))
}

/// A history entry for the consumer block: the list `files` with `verdict`.
fn entry(files: &[MigrationFile<'static>], verdict: Verdict) -> &'static [HistoricalState] {
    Box::leak(Box::new([HistoricalState {
        list_hash: Box::leak(migration_helper::migration_set_hash(files).into_boxed_str()),
        first_seen: "2026-09-01 0123abcd",
        composition: &[],
        verdict,
    }]))
}

/// A consumer block that applies its migrations at `Init`, the way every
/// impresspress block applies its own, then fails its `Init` with
/// `then_fail` when it is set, and answers every request.
struct Migrating {
    migrations: &'static BlockMigrations,
    then_fail: Option<&'static str>,
}

#[wafer_block::wafer_async_trait]
impl wafer_run::Block for Migrating {
    fn info(&self) -> wafer_run::BlockInfo {
        wafer_run::BlockInfo::new(BLOCK, "0.0.1", "http-handler@v1", "applies a test list")
    }

    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        migration_helper::lifecycle_init(ctx, &event, self.migrations).await?;
        match self.then_fail {
            Some(reason) => Err(WaferError::new(
                wafer_run::ErrorCode::FailedPrecondition,
                reason,
            )),
            None => Ok(()),
        }
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(ANSWER.as_bytes().to_vec())
    }
}

/// Where a boot's database lives.
#[derive(Clone)]
enum Database {
    Sqlite(String),
    Postgres(String),
}

impl Database {
    fn infra(&self, port: u16, storage_root: &str) -> InfraConfig {
        let (db_type, db_path, db_url) = match self {
            Self::Sqlite(path) => ("sqlite", path.clone(), None),
            Self::Postgres(url) => ("postgres", String::new(), Some(url.clone())),
        };
        InfraConfig {
            listen: format!("127.0.0.1:{port}"),
            db_type: db_type.to_string(),
            db_path,
            db_url,
            storage_type: "local".to_string(),
            storage_root: storage_root.to_string(),
            model_cache_dir: "data/models".to_string(),
            listener: ListenerEnv::default(),
        }
    }

    async fn open(&self) -> Arc<dyn DatabaseService> {
        let infra = self.infra(0, "");
        impresspress_native::make_database_service(
            &infra.db_type,
            &infra.db_path,
            infra.db_url.as_deref(),
        )
        .await
        .expect("open the database")
    }
}

/// A port nothing is listening on, for the listener to bind.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// What one boot did: the running server, or why it refused to start.
struct Boot {
    port: u16,
    result: anyhow::Result<Arc<wafer_run::Wafer>>,
}

/// Start a server over `database` whose consumer block ships `files`, with
/// no history.
async fn boot(
    database: &Database,
    storage_root: &str,
    files: &'static [MigrationFile<'static>],
) -> Boot {
    boot_listed(database, storage_root, listed(files, &[])).await
}

/// Start a server over `database` whose consumer block ships `migrations`.
async fn boot_listed(
    database: &Database,
    storage_root: &str,
    migrations: &'static BlockMigrations,
) -> Boot {
    boot_block(
        database,
        storage_root,
        Arc::new(Migrating {
            migrations,
            then_fail: None,
        }),
    )
    .await
}

/// Start a server over `database` with `block` as its consumer block.
async fn boot_block(
    database: &Database,
    storage_root: &str,
    block: Arc<dyn wafer_run::Block>,
) -> Boot {
    let port = free_port();
    let infra = database.infra(port, storage_root);
    let hooks = AppHooks {
        register_blocks: Box::new(move |builder| Ok(builder.extra_block(BLOCK, block))),
        register_post_build: Box::new(|wafer, _storage| {
            wafer.add_flow_json(&format!(
                r#"{{ "id": "{FLOW}", "name": "Test", "version": "0.1.0",
                     "steps": [ {{ "id": "migrating", "block": "{BLOCK}" }} ] }}"#
            ))?;
            Ok(())
        }),
    };
    let result = start_native(
        &infra,
        database.open().await,
        &HashMap::new(),
        Default::default(),
        FLOW,
        hooks,
    )
    .await;
    Boot { port, result }
}

/// Boot, require that it started, and require that it answers a request.
async fn boot_and_serve(
    database: &Database,
    storage_root: &str,
    files: &'static [MigrationFile<'static>],
) -> Arc<wafer_run::Wafer> {
    let Boot { port, result } = boot(database, storage_root, files).await;
    let wafer = result.expect("the server starts");
    assert_serves(port).await;
    wafer
}

async fn assert_serves(port: u16) {
    let url = format!("http://127.0.0.1:{port}/");
    for _ in 0..50 {
        if let Ok(resp) = reqwest::get(&url).await {
            assert_eq!(resp.status().as_u16(), 200);
            assert_eq!(resp.text().await.unwrap_or_default(), ANSWER);
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("nothing answered on port {port}");
}

/// How many times `002` has run against the database.
async fn runs(database: &Database) -> i64 {
    runs_of(database, "002").await
}

/// How many times the file that inserts `file` has run against the database.
async fn runs_of(database: &Database, file: &str) -> i64 {
    database
        .open()
        .await
        .count(
            RUNS,
            &[wafer_block::db::Filter {
                field: "file".into(),
                operator: wafer_block::db::FilterOp::Equal,
                value: serde_json::json!(file),
            }],
        )
        .await
        .expect("count the runs")
}

/// What the ledger records for `block`, as `(file, origin)` in file order.
async fn ledger(database: &Database, block: &str) -> Vec<(String, Origin)> {
    let mut rows: Vec<(String, Origin)> =
        applied_migrations::list_for_block_via_service(&database.open().await, block)
            .await
            .expect("read the ledger")
            .into_iter()
            .map(|row| (row.file_name, row.origin))
            .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

fn recorded(rows: &[(&str, Origin)]) -> Vec<(String, Origin)> {
    rows.iter()
        .map(|(file, origin)| ((*file).to_string(), *origin))
        .collect()
}

/// Whether the ledger table exists. Test-fixture inspection of the schema.
async fn ledger_exists(database: &Database) -> bool {
    database
        .open()
        .await
        .schema_table_exists(applied_migrations::TABLE)
        .await
        .expect("probe the ledger table")
}

/// Turn the database into one a build from before the per-file ledger left:
/// the same schema and `block_settings` records, and no ledger. Test-fixture
/// setup.
async fn forget_the_ledger(database: &Database) {
    database
        .open()
        .await
        .exec_raw(&format!("DROP TABLE {}", applied_migrations::TABLE), &[])
        .await
        .expect("drop the ledger");
}

/// Boot `files`, require that the boot is refused, and return the reason.
async fn refused(
    database: &Database,
    storage_root: &str,
    files: &'static [MigrationFile<'static>],
) -> String {
    refused_listed(database, storage_root, listed(files, &[])).await
}

/// Boot `migrations`, require that the boot is refused, and return the
/// reason.
async fn refused_listed(
    database: &Database,
    storage_root: &str,
    migrations: &'static BlockMigrations,
) -> String {
    let Boot { port, result } = boot_listed(database, storage_root, migrations).await;
    let error = format!("{:#}", result.err().expect("the boot is refused"));
    assert!(error.contains("refusing to start"), "{error}");
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "a refused boot binds nothing"
    );
    error
}

/// The hash the database records as applied for `block`.
async fn applied(database: &Database, block: &str) -> String {
    impresspress_core::platform_state::block_settings::load(&database.open().await)
        .await
        .expect("load block settings")
        .state(block)
        .migration
        .current_hash
}

/// An older install gains a migration: the next boot applies it before it
/// serves, and the boot after that applies nothing.
async fn upgrade_applies_once(database: &Database, storage_root: &str) {
    let first = boot_and_serve(database, storage_root, V1).await;
    first.shutdown().await;
    assert_eq!(runs(database).await, 0, "V1 has no 002");
    assert_eq!(
        applied(database, BLOCK).await,
        migration_helper::migration_set_hash(V1)
    );

    let upgraded = boot_and_serve(database, storage_root, V2).await;
    upgraded.shutdown().await;
    assert_eq!(runs(database).await, 1, "the upgrade's boot applied 002");
    assert_eq!(
        applied(database, BLOCK).await,
        migration_helper::migration_set_hash(V2)
    );
    assert_eq!(
        ledger(database, BLOCK).await,
        recorded(&[("001_runs", Origin::Ran), ("002_count_runs", Origin::Ran)])
    );

    let unchanged = boot_and_serve(database, storage_root, V2).await;
    unchanged.shutdown().await;
    assert_eq!(runs(database).await, 1, "an applied list is not run again");
}

/// A boot whose every block is settled reads no ledger: each block's record
/// answers from the snapshot. With the ledger table gone, such a boot starts
/// and nothing creates it again.
async fn a_settled_boot_reads_no_ledger(database: &Database, storage_root: &str) {
    boot_and_serve(database, storage_root, V2)
        .await
        .shutdown()
        .await;
    forget_the_ledger(database).await;
    boot_and_serve(database, storage_root, V2)
        .await
        .shutdown()
        .await;
    assert!(
        !ledger_exists(database).await,
        "no block of a settled boot looked at the ledger"
    );
}

/// An applied migration whose statements this build ships edited refuses
/// the boot, naming the block, the file and both hashes; nothing runs.
async fn an_edited_migration_refuses_the_boot(database: &Database, storage_root: &str) {
    boot_and_serve(database, storage_root, V2)
        .await
        .shutdown()
        .await;
    let error = refused(database, storage_root, V2_EDITED).await;
    assert!(
        error.contains(&format!(
            "{BLOCK}: migration `002_count_runs` was applied with content hash"
        )),
        "{error}"
    );
    assert!(
        error.contains(&migration_helper::content_hash(V2[1].1))
            && error.contains(&migration_helper::content_hash(V2_EDITED[1].1)),
        "{error}"
    );
    assert_eq!(runs_of(database, "002-edited").await, 0);
    assert_eq!(runs(database).await, 1);
}

/// A build older than the database — one file fewer — refuses the boot as
/// "database ahead", naming the file it does not ship.
async fn an_older_build_refuses_the_boot(database: &Database, storage_root: &str) {
    boot_and_serve(database, storage_root, V2)
        .await
        .shutdown()
        .await;
    let error = refused(database, storage_root, V1).await;
    assert!(error.contains("migrated by a newer build"), "{error}");
    assert!(error.contains("002_count_runs"), "{error}");
    assert_eq!(
        applied(database, BLOCK).await,
        migration_helper::migration_set_hash(V2),
        "the refused boot recorded nothing"
    );
}

/// A database from before the ledger, whose block recorded `V2`, boots a
/// build shipping `V3` whose history says `V2` applied 001 and 002: those
/// are adopted without running, and only `003` runs.
async fn a_pre_ledger_database_adopts(database: &Database, storage_root: &str) {
    boot_and_serve(database, storage_root, V2)
        .await
        .shutdown()
        .await;
    forget_the_ledger(database).await;
    let history = entry(V2, Verdict::Applied(&["001_runs", "002_count_runs"]));
    boot_listed(database, storage_root, listed(V3, history))
        .await
        .result
        .expect("the server starts")
        .shutdown()
        .await;
    assert_eq!(runs(database).await, 1, "002 was adopted, not run again");
    assert_eq!(runs_of(database, "003").await, 1, "003 ran once");
    assert_eq!(
        ledger(database, BLOCK).await,
        recorded(&[
            ("001_runs", Origin::Adopted),
            ("002_count_runs", Origin::Adopted),
            ("003_count_runs", Origin::Ran),
        ])
    );
    assert_eq!(
        applied(database, BLOCK).await,
        migration_helper::migration_set_hash(V3)
    );
}

/// A database from before the ledger whose record the history table marks
/// unsupported refuses the boot with the entry's reason; one whose record it
/// does not name at all (a branch build's) refuses as unknown. Neither runs
/// or records anything.
async fn a_pre_ledger_record_the_table_refuses(database: &Database, storage_root: &str) {
    boot_and_serve(database, storage_root, V2)
        .await
        .shutdown()
        .await;
    forget_the_ledger(database).await;
    let unsupported = entry(
        V2,
        Verdict::Unsupported("its 002 was edited after it shipped"),
    );
    let error = refused_listed(database, storage_root, listed(V3, unsupported)).await;
    assert!(
        error.contains("cannot be upgraded: its 002 was edited after it shipped"),
        "{error}"
    );
    let error = refused_listed(database, storage_root, listed(V3, &[])).await;
    assert!(error.contains("is no state main ever had"), "{error}");
    assert_eq!(runs_of(database, "003").await, 0, "nothing ran");
    assert!(ledger(database, BLOCK).await.is_empty(), "nothing recorded");
    assert_eq!(
        applied(database, BLOCK).await,
        migration_helper::migration_set_hash(V2),
        "the record stayed"
    );
}

/// A migration the database refuses stops the boot before anything binds,
/// names the block and the file, and leaves the list pending, so the next
/// boot with a fixed build applies it.
async fn failure_refuses_the_boot(database: &Database, storage_root: &str) {
    let Boot { port, result } = boot(database, storage_root, BROKEN).await;
    let error = format!(
        "{:#}",
        result.err().expect("a failed migration refuses the boot")
    );
    assert!(error.contains("refusing to start"), "{error}");
    assert!(error.contains(&format!("`{BLOCK}`")), "{error}");
    assert!(
        error.contains("migration `002_broken` failed on"),
        "{error}"
    );
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "a refused boot binds nothing"
    );
    assert_eq!(
        applied(database, BLOCK).await,
        migration_helper::migration_set_hash(V1),
        "a failed list is not recorded as applied"
    );

    let fixed = boot_and_serve(database, storage_root, V2).await;
    fixed.shutdown().await;
    assert_eq!(
        applied(database, BLOCK).await,
        migration_helper::migration_set_hash(V2)
    );
}

/// Any other `Init` failure refuses the boot too, after the block's
/// migrations applied: a block whose `Init` failed stays failed until the
/// process restarts.
#[tokio::test(flavor = "multi_thread")]
async fn any_init_failure_refuses_the_boot() {
    let db = sqlite();
    let Boot { port, result } = boot_block(
        &db.database,
        &db.storage_root,
        Arc::new(Migrating {
            migrations: listed(V2, &[]),
            then_fail: Some("a setting this block cannot use"),
        }),
    )
    .await;
    let error = format!(
        "{:#}",
        result.err().expect("a failed Init refuses the boot")
    );
    assert!(error.contains("refusing to start"), "{error}");
    assert!(error.contains("a setting this block cannot use"), "{error}");
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "a refused boot binds nothing"
    );
    assert_eq!(
        applied(&db.database, BLOCK).await,
        migration_helper::migration_set_hash(V2)
    );
}

/// Two processes boot against a fresh database at once: both start, and the
/// list runs once.
async fn concurrent_boots_apply_once(database: &Database, storage_root: &str) {
    // Each boot runs on a thread of its own with a runtime of its own — the
    // stand-in for a process — so the two really run at once: a boot over
    // SQLite rarely yields, and two boots polled by one task would all but
    // run one after the other.
    let process = || {
        let database = database.clone();
        let storage_root = storage_root.to_string();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("a runtime for the process");
            runtime.block_on(async {
                let wafer = boot(&database, &storage_root, V2).await.result?;
                wafer.shutdown().await;
                anyhow::Ok(())
            })
        })
    };
    let (a, b) = (process(), process());
    for (name, process) in [("first", a), ("second", b)] {
        tokio::task::spawn_blocking(move || process.join())
            .await
            .expect("join the process thread")
            .expect("the process thread did not panic")
            .unwrap_or_else(|e| panic!("the {name} process starts: {e:#}"));
    }
    assert_eq!(runs(database).await, 1, "only one process applied the list");
}

struct Sqlite {
    _dir: tempfile::TempDir,
    database: Database,
    storage_root: String,
}

fn sqlite() -> Sqlite {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage_root = dir.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let path = dir.path().join("boot.sqlite3");
    Sqlite {
        database: Database::Sqlite(path.to_str().expect("utf-8 path").to_string()),
        storage_root: storage_root.to_str().expect("utf-8 path").to_string(),
        _dir: dir,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_boot_applies_a_pending_migration_once_and_serves() {
    let db = sqlite();
    upgrade_applies_once(&db.database, &db.storage_root).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_migration_refuses_the_boot() {
    let db = sqlite();
    boot_and_serve(&db.database, &db.storage_root, V1)
        .await
        .shutdown()
        .await;
    failure_refuses_the_boot(&db.database, &db.storage_root).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_processes_booting_together_apply_a_list_once() {
    let db = sqlite();
    concurrent_boots_apply_once(&db.database, &db.storage_root).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_settled_boot_never_reads_the_ledger() {
    let db = sqlite();
    a_settled_boot_reads_no_ledger(&db.database, &db.storage_root).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edited_applied_migration_refuses_the_boot() {
    let db = sqlite();
    an_edited_migration_refuses_the_boot(&db.database, &db.storage_root).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_build_older_than_the_database_refuses_the_boot() {
    let db = sqlite();
    an_older_build_refuses_the_boot(&db.database, &db.storage_root).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_database_from_before_the_ledger_adopts_what_it_ran() {
    let db = sqlite();
    a_pre_ledger_database_adopts(&db.database, &db.storage_root).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pre_ledger_record_the_history_does_not_support_refuses_the_boot() {
    let db = sqlite();
    a_pre_ledger_record_the_table_refuses(&db.database, &db.storage_root).await;
}

/// Admin's list is applied before the runtime is built, through the same
/// ledger and the same record:
///
/// - the first boot records each of admin's files;
/// - a boot whose admin record is up to date runs nothing of it;
/// - a stale record over a ledger that records every file runs nothing
///   either, and is stamped again.
///
/// What a run of 002 leaves behind is an index, dropped here in between:
/// only a boot that runs 002 puts it back. Databases from before the ledger
/// are the scenarios after this one.
#[tokio::test(flavor = "multi_thread")]
async fn admins_pre_build_apply_plans_against_the_ledger() {
    const INDEX: &str = "impresspress__admin__variables_block_idx";
    let db = sqlite();
    let admin = admin::ADMIN_BLOCK_ID;
    let admin_files = admin::migrations::MIGRATIONS.sqlite;
    let admin_hash = migration_helper::migration_set_hash(admin_files);
    let index_exists = || async {
        // Test-fixture inspection of the schema.
        !db.database
            .open()
            .await
            .query_raw(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?",
                &[serde_json::json!(INDEX)],
            )
            .await
            .expect("read the schema")
            .is_empty()
    };
    let drop_index = || async {
        db.database
            .open()
            .await
            .exec_raw(&format!("DROP INDEX {INDEX}"), &[])
            .await
            .expect("drop the index");
    };
    let record_admin = |hash: String| {
        let database = db.database.clone();
        async move {
            let mut stale = HashMap::new();
            stale.insert("current_hash".to_string(), serde_json::json!(hash));
            database
                .open()
                .await
                .update_where_count(
                    impresspress_core::platform_state::block_settings::TABLE,
                    &[wafer_block::db::Filter {
                        field: "block_name".into(),
                        operator: wafer_block::db::FilterOp::Equal,
                        value: serde_json::json!(admin),
                    }],
                    stale,
                )
                .await
                .expect("record an admin list");
        }
    };

    boot_and_serve(&db.database, &db.storage_root, V1)
        .await
        .shutdown()
        .await;
    assert_eq!(applied(&db.database, admin).await, admin_hash);
    assert!(index_exists().await, "the first boot applied admin's list");
    let every_file_ran: Vec<(String, Origin)> = admin_files
        .iter()
        .map(|(name, _)| ((*name).to_string(), Origin::Ran))
        .collect();
    assert_eq!(ledger(&db.database, admin).await, every_file_ran);

    drop_index().await;
    boot_and_serve(&db.database, &db.storage_root, V1)
        .await
        .shutdown()
        .await;
    assert!(
        !index_exists().await,
        "a boot whose admin record is up to date does not run admin's list"
    );

    record_admin("an-older-list".to_string()).await;
    boot_and_serve(&db.database, &db.storage_root, V1)
        .await
        .shutdown()
        .await;
    assert!(
        !index_exists().await,
        "a stale record over a full ledger runs nothing of admin's list"
    );
    assert_eq!(
        applied(&db.database, admin).await,
        admin_hash,
        "and is stamped again"
    );
}

// ---------------------------------------------------------------------------
// The real blocks' databases from before the ledger
// ---------------------------------------------------------------------------

/// Auth's newest state before 016 and the state 016 first shipped in, in
/// `backend`'s dialect (their hashes differ by dialect; the commits that
/// made them are 7e566600 / 6499cfa8 and 6b989378).
fn auth_states(backend: Backend) -> (&'static str, &'static str) {
    match backend {
        Backend::Sqlite => ("cc40a45f", "e73b4cc2"),
        Backend::Postgres => ("4390791b", "43907ff1"),
    }
}

/// Admin's state of a main commit, in `backend`'s dialect: `dd4fd96c` made
/// the 2026-10-07 list (SQLite `84c038a6`), `56ab1250` the one with the first
/// 009 (`0f4bad57`), `aa1dc9cd` the last list whose 001 still created
/// `blessed_hash` (`61675928`), and `1f1763b4` the unsupported list with
/// 007's first text (`a2512c2f`).
fn admin_at(backend: Backend, commit: &str) -> &'static str {
    history::hash_at(&admin::migrations::MIGRATIONS, backend, commit).expect("an admin state")
}

/// Build `migrations` at `short` with the runner from before the ledger.
async fn build(database: &Database, backend: Backend, migrations: &BlockMigrations, short: &str) {
    history::build_state(&database.open().await, backend, migrations, short)
        .await
        .unwrap_or_else(|e| panic!("build {} at {short}: {e}", migrations.block));
}

/// A block's ledger as `(origin, adopted_from)` per file, in file order.
async fn ledger_from(database: &Database, block: &str) -> Vec<(String, Origin, String)> {
    history::ledger(&database.open().await, block)
        .await
        .expect("read the ledger")
        .into_iter()
        .map(|row| (row.file_name, row.origin, row.adopted_from))
        .collect()
}

/// What `migrations`' ledger must hold after adopting `adopted` files from
/// `from` and running the rest.
fn adopted_then_ran(
    migrations: &BlockMigrations,
    backend: Backend,
    adopted: usize,
    from: &str,
) -> Vec<(String, Origin, String)> {
    migrations
        .for_backend(backend)
        .0
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            if i < adopted {
                ((*name).to_string(), Origin::Adopted, from.to_string())
            } else {
                ((*name).to_string(), Origin::Ran, String::new())
            }
        })
        .collect()
}

/// How many files the entry named by its first eight characters had
/// applied: the ones a boot adopts. The files after them run.
fn applied_by_entry(migrations: &BlockMigrations, backend: Backend, short: &str) -> usize {
    match history::state(migrations, backend, short)
        .expect("a history entry")
        .verdict
    {
        impresspress_core::migration_helper::Verdict::Applied(files) => files.len(),
        impresspress_core::migration_helper::Verdict::Unsupported(reason) => {
            panic!("{short} is unsupported: {reason}")
        }
    }
}

/// The full hash of an entry named by its first eight characters.
fn full(migrations: &BlockMigrations, backend: Backend, short: &str) -> &'static str {
    history::state(migrations, backend, short)
        .expect("a history entry")
        .list_hash
}

/// Whether `block_settings` has a `blessed_hash` column.
async fn has_blessed_hash(database: &Database) -> bool {
    database
        .open()
        .await
        .schema_columns(block_settings::TABLE)
        .await
        .expect("introspect block_settings")
        .iter()
        .any(|c| c == "blessed_hash")
}

/// Admin on 2026-10-07's list and auth on its newest list before 016, both
/// recorded by the old runner: the boot runs admin's 009 and auth's 016 (and
/// the auth files added after 016), and every other block installs fresh.
async fn a_database_from_main_runs_only_what_it_lacks(
    database: &Database,
    storage_root: &str,
    backend: Backend,
) {
    let (admin_m, auth_m) = (
        &admin::migrations::MIGRATIONS,
        &auth::migrations::MIGRATIONS,
    );
    let admin_hash = admin_at(backend, "dd4fd96c");
    build(database, backend, admin_m, &admin_hash[..8]).await;
    let auth_short = auth_states(backend).0;
    build(database, backend, auth_m, auth_short).await;

    boot_and_serve(database, storage_root, V1)
        .await
        .shutdown()
        .await;

    let admin_files = admin_m.for_backend(backend).0.len();
    assert_eq!(
        ledger_from(database, admin_m.block).await,
        adopted_then_ran(admin_m, backend, admin_files - 1, admin_hash),
        "admin adopted 001-008 and ran 009"
    );
    assert_eq!(
        ledger_from(database, auth_m.block).await,
        adopted_then_ran(
            auth_m,
            backend,
            applied_by_entry(auth_m, backend, auth_short),
            full(auth_m, backend, auth_short)
        ),
        "auth adopted 001-015 and ran 016 and every later file"
    );
    assert!(!has_blessed_hash(database).await);
}

/// Admin on the list with 009's first text, which added and emptied
/// `blessed_hash`, and auth on the list with 016's first text: auth adopts
/// every file through 016 and runs the later ones, and admin adopts 001-008 and runs today's 009, which drops the
/// column — the history marks the first 009 as not applied.
async fn a_database_that_ran_the_first_016_and_009_runs_009_and_the_newer_files(
    database: &Database,
    storage_root: &str,
    backend: Backend,
) {
    let (admin_m, auth_m) = (
        &admin::migrations::MIGRATIONS,
        &auth::migrations::MIGRATIONS,
    );
    let admin_hash = admin_at(backend, "56ab1250");
    build(database, backend, admin_m, &admin_hash[..8]).await;
    let auth_short = auth_states(backend).1;
    build(database, backend, auth_m, auth_short).await;
    assert!(
        has_blessed_hash(database).await,
        "the first 009 added the column"
    );

    boot_and_serve(database, storage_root, V1)
        .await
        .shutdown()
        .await;

    let admin_files = admin_m.for_backend(backend).0.len();
    assert_eq!(
        ledger_from(database, admin_m.block).await,
        adopted_then_ran(admin_m, backend, admin_files - 1, admin_hash)
    );
    assert_eq!(
        ledger_from(database, auth_m.block).await,
        adopted_then_ran(
            auth_m,
            backend,
            applied_by_entry(auth_m, backend, auth_short),
            full(auth_m, backend, auth_short)
        ),
        "auth adopted every file through 016 and ran only the later ones"
    );
    assert!(
        !has_blessed_hash(database).await,
        "today's 009 dropped the column"
    );
}

/// The two populations that share admin's 2026-10-07 record: one created
/// before that day, whose `001` created `blessed_hash` (built on the
/// 2026-10-06 list, then migrated by the 2026-10-07 build, which kept the
/// table), and one created on it, whose `001` did not. Both hold a row an
/// old build blessed for a block this build does not register, disabled. The
/// boot ends both with no `blessed_hash` column and every row's other values
/// as they were, `enabled = 0` included.
async fn both_populations_of_admins_record_lose_blessed_hash(
    database: &Database,
    storage_root: &str,
    backend: Backend,
    created_before_the_column_left: bool,
) {
    let admin_m = &admin::migrations::MIGRATIONS;
    if created_before_the_column_left {
        let older = admin_at(backend, "aa1dc9cd");
        build(database, backend, admin_m, &older[..8]).await;
        assert!(
            has_blessed_hash(database).await,
            "that 001 created the column"
        );
    }
    let admin_hash = admin_at(backend, "dd4fd96c");
    build(database, backend, admin_m, &admin_hash[..8]).await;
    let mut row = HashMap::new();
    row.insert("id".to_string(), serde_json::json!("bs_unregistered"));
    row.insert(
        "block_name".to_string(),
        serde_json::json!("test/unregistered"),
    );
    // Disabled, so a rebuild that copied `enabled` wrong would show.
    row.insert("enabled".to_string(), serde_json::json!(0));
    row.insert("current_hash".to_string(), serde_json::json!("its-list"));
    row.insert(
        "seed_defaults_hash".to_string(),
        serde_json::json!("its-seed"),
    );
    row.insert(
        "created_at".to_string(),
        serde_json::json!("2026-09-27T00:00:00Z"),
    );
    row.insert(
        "updated_at".to_string(),
        serde_json::json!("2026-09-27T00:00:00Z"),
    );
    if created_before_the_column_left {
        row.insert("blessed_hash".to_string(), serde_json::json!("blessed"));
    }
    // Test-fixture setup: a row as an older build left it.
    database
        .open()
        .await
        .create(block_settings::TABLE, row)
        .await
        .expect("seed a row");

    boot_and_serve(database, storage_root, V1)
        .await
        .shutdown()
        .await;

    assert!(!has_blessed_hash(database).await);
    let kept = database
        .open()
        .await
        .get(block_settings::TABLE, "bs_unregistered")
        .await
        .expect("the row survives");
    for (column, value) in [
        ("block_name", "test/unregistered"),
        ("current_hash", "its-list"),
        ("seed_defaults_hash", "its-seed"),
        ("created_at", "2026-09-27T00:00:00Z"),
    ] {
        assert_eq!(kept.data[column], serde_json::json!(value), "{column}");
    }
    let enabled = &kept.data["enabled"];
    assert!(
        enabled == &serde_json::json!(0) || enabled == &serde_json::json!(false),
        "the row stays disabled: {enabled}"
    );
    let admin_files = admin_m.for_backend(backend).0.len();
    assert_eq!(
        ledger_from(database, admin_m.block).await,
        adopted_then_ran(admin_m, backend, admin_files - 1, admin_hash)
    );
}

/// Admin on a list the history marks unsupported refuses the boot with the
/// entry's reason, before anything is built; nothing is recorded.
async fn an_unsupported_admin_state_refuses_to_start(
    database: &Database,
    storage_root: &str,
    backend: Backend,
) {
    let admin_m = &admin::migrations::MIGRATIONS;
    let hash = admin_at(backend, "1f1763b4");
    build(database, backend, admin_m, &hash[..8]).await;
    let Boot { port, result } = boot(database, storage_root, V1).await;
    let error = format!("{:#}", result.err().expect("the boot is refused"));
    assert!(error.contains(&hash[..8]), "{error}");
    assert!(error.contains("cannot be upgraded"), "{error}");
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "a refused boot binds nothing"
    );
    assert!(ledger_from(database, admin_m.block).await.is_empty());
    assert_eq!(applied(database, admin_m.block).await, hash);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_database_from_main_runs_only_the_files_its_record_lacks() {
    let db = sqlite();
    a_database_from_main_runs_only_what_it_lacks(&db.database, &db.storage_root, Backend::Sqlite)
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_database_that_ran_the_first_016_and_009_runs_today_s_009_and_the_newer_files() {
    let db = sqlite();
    a_database_that_ran_the_first_016_and_009_runs_009_and_the_newer_files(
        &db.database,
        &db.storage_root,
        Backend::Sqlite,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn admins_2026_10_07_record_loses_blessed_hash_in_both_populations() {
    for before in [true, false] {
        let db = sqlite();
        both_populations_of_admins_record_lose_blessed_hash(
            &db.database,
            &db.storage_root,
            Backend::Sqlite,
            before,
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unsupported_admin_state_refuses_the_boot() {
    let db = sqlite();
    an_unsupported_admin_state_refuses_to_start(&db.database, &db.storage_root, Backend::Sqlite)
        .await;
}

/// Every scenario above against PostgreSQL, each on a database of its own.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a PostgreSQL server named by IMPRESSPRESS_TEST_POSTGRES_URL"]
async fn the_same_holds_on_postgres() {
    let url = std::env::var("IMPRESSPRESS_TEST_POSTGRES_URL")
        .expect("IMPRESSPRESS_TEST_POSTGRES_URL must name a PostgreSQL database");
    let admin = impresspress_native::make_database_service("postgres", "", Some(&url))
        .await
        .expect("connect to PostgreSQL");
    let storage = tempfile::tempdir().expect("tempdir");
    let storage_root = storage.path().to_str().expect("utf-8 path").to_string();
    for scenario in [
        "upgrade",
        "failure",
        "concurrent",
        "settled",
        "edited",
        "older",
        "adopt",
        "refuse",
        "from_main",
        "first_016_009",
        "before_10_07",
        "after_10_07",
        "unsupported",
    ] {
        let name = format!("impresspress_boot_migrations_{scenario}");
        // Test-fixture setup: a database per scenario, created fresh.
        admin
            .exec_raw(&format!("DROP DATABASE IF EXISTS {name}"), &[])
            .await
            .expect("drop the scenario's database");
        admin
            .exec_raw(&format!("CREATE DATABASE {name}"), &[])
            .await
            .expect("create the scenario's database");
        let (base, _) = url.rsplit_once('/').expect("a database URL");
        let database = Database::Postgres(format!("{base}/{name}"));
        match scenario {
            "upgrade" => {
                upgrade_applies_once(&database, &storage_root).await;
                // Every block migrates in the PostgreSQL dialect, admin's
                // `Init` included: it agrees with the list the server applied
                // before the build, so it recorded that list's hash.
                assert_eq!(
                    applied(&database, admin::ADMIN_BLOCK_ID).await,
                    migration_helper::migration_set_hash(admin::migrations::MIGRATIONS.postgres),
                );
            }
            "failure" => {
                boot_and_serve(&database, &storage_root, V1)
                    .await
                    .shutdown()
                    .await;
                failure_refuses_the_boot(&database, &storage_root).await;
            }
            "concurrent" => concurrent_boots_apply_once(&database, &storage_root).await,
            "settled" => a_settled_boot_reads_no_ledger(&database, &storage_root).await,
            "edited" => an_edited_migration_refuses_the_boot(&database, &storage_root).await,
            "older" => an_older_build_refuses_the_boot(&database, &storage_root).await,
            "adopt" => a_pre_ledger_database_adopts(&database, &storage_root).await,
            "refuse" => a_pre_ledger_record_the_table_refuses(&database, &storage_root).await,
            "from_main" => {
                a_database_from_main_runs_only_what_it_lacks(
                    &database,
                    &storage_root,
                    Backend::Postgres,
                )
                .await
            }
            "first_016_009" => {
                a_database_that_ran_the_first_016_and_009_runs_009_and_the_newer_files(
                    &database,
                    &storage_root,
                    Backend::Postgres,
                )
                .await
            }
            "before_10_07" | "after_10_07" => {
                both_populations_of_admins_record_lose_blessed_hash(
                    &database,
                    &storage_root,
                    Backend::Postgres,
                    scenario == "before_10_07",
                )
                .await
            }
            _ => {
                an_unsupported_admin_state_refuses_to_start(
                    &database,
                    &storage_root,
                    Backend::Postgres,
                )
                .await
            }
        }
    }
}
