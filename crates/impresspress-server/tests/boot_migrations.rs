//! A native boot applies the database's pending block migrations before it
//! serves, applies nothing that is already applied, refuses to start when a
//! migration fails or when the database's migration ledger and the build
//! disagree, adopts what a database from before the ledger proves it ran, and
//! lets only one of two processes booting together apply a list.
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
    migration_helper::{self, MigrationFile},
    platform_state::applied_migrations::{self, Origin},
};
use impresspress_native::{InfraConfig, ListenerEnv};
use impresspress_server::{start_native, AppHooks};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_run::{context::Context, InputStream, LifecycleEvent, Message, OutputStream, WaferError};

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

/// A consumer block that applies `files` at `Init`, the way every
/// impresspress block applies its own, then fails its `Init` with
/// `then_fail` when it is set, and answers every request.
struct Migrating {
    files: &'static [MigrationFile<'static>],
    then_fail: Option<&'static str>,
}

#[wafer_block::wafer_async_trait]
impl wafer_run::Block for Migrating {
    fn info(&self) -> wafer_run::BlockInfo {
        wafer_run::BlockInfo::new(BLOCK, "0.0.1", "http-handler@v1", "applies a test list")
    }

    async fn lifecycle(&self, ctx: &dyn Context, event: LifecycleEvent) -> Result<(), WaferError> {
        migration_helper::lifecycle_init(ctx, &event, BLOCK, self.files, self.files).await?;
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

/// Start a server over `database` whose consumer block ships `files`.
async fn boot(
    database: &Database,
    storage_root: &str,
    files: &'static [MigrationFile<'static>],
) -> Boot {
    boot_block(
        database,
        storage_root,
        Arc::new(Migrating {
            files,
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
    let Boot { port, result } = boot(database, storage_root, files).await;
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
/// build shipping `V3`: `V2`'s files are adopted without running, and only
/// `003` runs.
async fn a_pre_ledger_database_adopts(database: &Database, storage_root: &str) {
    boot_and_serve(database, storage_root, V2)
        .await
        .shutdown()
        .await;
    forget_the_ledger(database).await;
    boot_and_serve(database, storage_root, V3)
        .await
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
            files: V2,
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

/// Admin's list is applied before the runtime is built, through the same
/// ledger and the same record:
///
/// - the first boot records each of admin's files;
/// - a boot whose admin record is up to date runs nothing of it;
/// - a stale record over a ledger that records every file runs nothing
///   either, and is stamped again;
/// - a database from before the ledger whose admin record is its 001-008
///   adopts those and runs 009 alone.
///
/// What a run of 002 leaves behind is an index, dropped here in between:
/// only a boot that runs 002 puts it back. What a run of 009 does is empty
/// every `blessed_hash`.
#[tokio::test(flavor = "multi_thread")]
async fn admins_pre_build_apply_plans_against_the_ledger() {
    const INDEX: &str = "impresspress__admin__variables_block_idx";
    let db = sqlite();
    let admin = impresspress_core::blocks::admin::ADMIN_BLOCK_ID;
    let admin_files = impresspress_core::blocks::admin::migrations::migration_files("sqlite");
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

    // A database from before the ledger, whose admin record is 001-008, with
    // a row an old build blessed.
    forget_the_ledger(&db.database).await;
    let before_009 = &admin_files[..admin_files.len() - 1];
    record_admin(migration_helper::migration_set_hash(before_009)).await;
    let mut blessed = HashMap::new();
    blessed.insert("blessed_hash".to_string(), serde_json::json!("blessed"));
    db.database
        .open()
        .await
        .update_where_count(
            impresspress_core::platform_state::block_settings::TABLE,
            &[wafer_block::db::Filter {
                field: "block_name".into(),
                operator: wafer_block::db::FilterOp::Equal,
                value: serde_json::json!(BLOCK),
            }],
            blessed,
        )
        .await
        .expect("bless a row");
    boot_and_serve(&db.database, &db.storage_root, V1)
        .await
        .shutdown()
        .await;
    assert!(!index_exists().await, "001-008 were adopted, not run");
    let mut adopted: Vec<(String, Origin)> = before_009
        .iter()
        .map(|(name, _)| ((*name).to_string(), Origin::Adopted))
        .collect();
    adopted.push((
        admin_files[admin_files.len() - 1].0.to_string(),
        Origin::Ran,
    ));
    assert_eq!(ledger(&db.database, admin).await, adopted);
    let blessed_left = db
        .database
        .open()
        .await
        .count(
            impresspress_core::platform_state::block_settings::TABLE,
            &[wafer_block::db::Filter {
                field: "blessed_hash".into(),
                operator: wafer_block::db::FilterOp::NotEqual,
                value: serde_json::json!(""),
            }],
        )
        .await
        .expect("count blessed rows");
    assert_eq!(blessed_left, 0, "009 ran and emptied every blessed_hash");
    assert_eq!(applied(&db.database, admin).await, admin_hash);
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
                    applied(&database, impresspress_core::blocks::admin::ADMIN_BLOCK_ID).await,
                    migration_helper::migration_set_hash(
                        impresspress_core::blocks::admin::migrations::migration_files("postgres")
                    ),
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
            _ => a_pre_ledger_database_adopts(&database, &storage_root).await,
        }
    }
}
