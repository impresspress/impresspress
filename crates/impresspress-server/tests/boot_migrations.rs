//! A native boot applies the database's pending block migrations before it
//! serves, applies nothing that is already applied, refuses to start when a
//! migration fails, and lets only one of two processes booting together
//! apply a list.
//!
//! Each boot here is `start_native` — what `impresspress serve` runs — over a
//! fresh `DatabaseService` on the same database, as a restarted or a second
//! process would open it, with a consumer block whose migration list the test
//! chooses. Its `002` inserts a row, so the number of rows is the number of
//! times the list really ran.
//!
//! The SQLite tests run everywhere. The PostgreSQL one needs a server, so it
//! is `#[ignore]`d; CI's `test-postgres` job runs it with
//! `IMPRESSPRESS_TEST_POSTGRES_URL` naming a database it may create others
//! from.

use std::{collections::HashMap, sync::Arc, time::Duration};

use impresspress_core::migration_helper::{self, MigrationFile};
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
        Migrating {
            files,
            then_fail: None,
        },
    )
    .await
}

/// Start a server over `database` with `block` as its consumer block.
async fn boot_block(database: &Database, storage_root: &str, block: Migrating) -> Boot {
    let port = free_port();
    let infra = database.infra(port, storage_root);
    let hooks = AppHooks {
        register_blocks: Box::new(move |builder| Ok(builder.extra_block(BLOCK, Arc::new(block)))),
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
    database
        .open()
        .await
        .count(RUNS, &[])
        .await
        .expect("count the runs")
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

    let unchanged = boot_and_serve(database, storage_root, V2).await;
    unchanged.shutdown().await;
    assert_eq!(runs(database).await, 1, "an applied list is not run again");
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
    assert!(
        error.contains("refusing to start: migrations failed"),
        "{error}"
    );
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
        migration_helper::APPLYING,
        "a failed list is recorded as still being applied, not as applied"
    );

    let fixed = boot_and_serve(database, storage_root, V2).await;
    fixed.shutdown().await;
    assert_eq!(
        applied(database, BLOCK).await,
        migration_helper::migration_set_hash(V2)
    );
}

/// Any other `Init` failure is tolerated, as it always was on native: the
/// block's migrations applied, so the server starts and serves the rest.
#[tokio::test(flavor = "multi_thread")]
async fn an_init_failure_after_the_migrations_is_tolerated() {
    let db = sqlite();
    let Boot { port, result } = boot_block(
        &db.database,
        &db.storage_root,
        Migrating {
            files: V2,
            then_fail: Some("a setting this block cannot use"),
        },
    )
    .await;
    let wafer = result.expect("a non-migration Init failure does not refuse the boot");
    let mut listening = false;
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            listening = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(listening, "the server is listening");
    wafer.shutdown().await;
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

/// Admin's list is applied before the runtime is built, through the same
/// tracking: a boot whose record is up to date leaves admin's schema alone,
/// and a boot whose record is stale applies the list and records its hash.
/// What a run leaves behind is an index admin's `002` creates, dropped here
/// in between: only a boot that runs the list puts it back.
#[tokio::test(flavor = "multi_thread")]
async fn the_admin_list_is_applied_only_when_pending() {
    const INDEX: &str = "impresspress__admin__variables_block_idx";
    let db = sqlite();
    let admin = impresspress_core::blocks::admin::ADMIN_BLOCK_ID;
    let admin_hash = migration_helper::migration_set_hash(
        impresspress_core::blocks::admin::migrations::migration_files("sqlite"),
    );
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

    boot_and_serve(&db.database, &db.storage_root, V1)
        .await
        .shutdown()
        .await;
    assert_eq!(applied(&db.database, admin).await, admin_hash);
    assert!(index_exists().await, "the first boot applied admin's list");

    drop_index().await;
    boot_and_serve(&db.database, &db.storage_root, V1)
        .await
        .shutdown()
        .await;
    assert!(
        !index_exists().await,
        "a boot whose admin record is up to date does not run admin's list"
    );

    let mut stale = HashMap::new();
    stale.insert(
        "current_hash".to_string(),
        serde_json::json!("an-older-list"),
    );
    db.database
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
        .expect("record an older admin list");
    boot_and_serve(&db.database, &db.storage_root, V1)
        .await
        .shutdown()
        .await;
    assert!(
        index_exists().await,
        "a stale admin record makes the boot run admin's list"
    );
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
    for scenario in ["upgrade", "failure", "concurrent"] {
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
            "upgrade" => upgrade_applies_once(&database, &storage_root).await,
            "failure" => {
                boot_and_serve(&database, &storage_root, V1)
                    .await
                    .shutdown()
                    .await;
                failure_refuses_the_boot(&database, &storage_root).await;
            }
            _ => concurrent_boots_apply_once(&database, &storage_root).await,
        }
    }
}
