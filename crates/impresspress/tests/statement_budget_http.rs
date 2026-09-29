//! A request that asks for more database statements than its invocation has
//! left is answered 429, through a real route, and writes nothing.
//!
//! A Cloudflare D1 service reports D1's per-invocation query limit and the
//! statements the request has already sent
//! (`impresspress-cloudflare`'s `database` module), and `wafer-core`'s
//! database handler admits every `create_many` and `batch` against it before
//! the service runs. This drives that refusal end to end on the runtime the
//! binary builds: `build_native_runtime`, the shared `boot`, the `site-main`
//! flow, the router and the auth-ui signup handler, whose account and
//! credential rows are one `batch`. The database is the real SQLite service
//! behind a decorator that reports a D1-shaped budget once armed (boot's own
//! writes run unbudgeted, as a deploy's `/_deploy/init` is its own
//! invocation).
//!
//! The 429 means "this request did too much", not "try again later": the
//! same signup retried would ask for the same statements. The body carries
//! the numbers so a client can see which.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

use impresspress_core::builder::{boot, GrantSource, InitPolicy};
use impresspress_native::InfraConfig;
use impresspress_server::{build_native_runtime, AppHooks, NativeBootHooks};
use wafer_block::{http_codec, InputStream};
use wafer_core::interfaces::database::service::{DatabaseError, DatabaseService, StatementBudget};

fn infra_for(db_path: &Path, storage_root: &Path) -> InfraConfig {
    InfraConfig {
        listen: "127.0.0.1:0".to_string(),
        db_type: "sqlite".to_string(),
        db_path: db_path
            .to_str()
            .expect("db path is valid utf-8")
            .to_string(),
        db_url: None,
        storage_type: "local".to_string(),
        storage_root: storage_root
            .to_str()
            .expect("storage root is valid utf-8")
            .to_string(),
        model_cache_dir: "data/models".to_string(),
        listener: Default::default(),
    }
}

/// The real SQLite service, reporting `budget` once a test sets it and
/// `Unbounded` (SQLite's own answer) until then. Every operation is the inner
/// service's.
struct BudgetedDb {
    inner: Arc<dyn DatabaseService>,
    budget: Arc<Mutex<StatementBudget>>,
}

impl BudgetedDb {
    fn inner_service(&self) -> &dyn DatabaseService {
        self.inner.as_ref()
    }
}

wafer_core::forward_database_service! {
    impl DatabaseService for BudgetedDb {
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
            statement_budget: custom,
        }

        fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
            Ok(*self.budget.lock().expect("budget lock"))
        }
    }
}

#[tokio::test]
async fn a_signup_past_what_the_invocation_has_left_is_a_429_that_writes_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("statement_budget.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    let infra = infra_for(&db_path, &storage_root);
    let sqlite = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");
    let budget = Arc::new(Mutex::new(StatementBudget::Unbounded));
    let database: Arc<dyn DatabaseService> = Arc::new(BudgetedDb {
        inner: sqlite.clone(),
        budget: Arc::clone(&budget),
    });
    let mut wafer = build_native_runtime(
        &infra,
        database,
        &HashMap::new(),
        Default::default(),
        false,
        AppHooks::none(),
    )
    .await
    .expect("build impresspress runtime");
    let report = boot(
        &mut wafer,
        &NativeBootHooks,
        GrantSource::PreInstalled(
            "build_native_runtime loads them from the platform database into \
             ImpresspressBuilder::wrap_grants before build()",
        ),
        InitPolicy::Reported,
    )
    .await
    .expect("seal");
    assert!(report.ok, "boot must succeed: {report:?}");
    wafer.run_start_lifecycle().await;
    let wafer = wafer.bind_all();

    let rows = || async {
        let accounts = sqlite
            .count("wafer_run__auth__users", &[])
            .await
            .expect("count accounts");
        let credentials = sqlite
            .count("wafer_run__auth__local_credentials", &[])
            .await
            .expect("count credentials");
        (accounts, credentials)
    };
    let before = rows().await;

    // The request has already sent 999 of its 1000 statements; the signup's
    // account and credential rows are one batch of two.
    *budget.lock().unwrap() = StatementBudget::Limited {
        limit: 1000,
        used: 999,
    };

    let body = br#"{"email":"new@example.com","password":"correct-horse-battery"}"#;
    let msg = http_codec::build_http_message(
        "POST",
        "/b/auth/api/signup",
        "",
        "127.0.0.1",
        [
            ("host", "localhost"),
            ("origin", "http://localhost"),
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/json"),
            ("accept", "application/json"),
        ],
    );
    let parts = http_codec::collect_http_response(
        wafer
            .run("site-main", msg, InputStream::from_bytes(body.to_vec()))
            .await,
    )
    .await;

    let text = String::from_utf8_lossy(&parts.body);
    assert_eq!(parts.status, 429, "body: {text}");
    assert!(
        text.contains("this invocation has 1 of its 1000 left"),
        "the refusal says what the request asked for and what was left: {text}"
    );
    let body: serde_json::Value = serde_json::from_slice(&parts.body).expect("a JSON error body");
    assert_eq!(
        body["code"],
        wafer_block::wire::database::STATEMENT_BUDGET_EXHAUSTED,
        "the 429 names the budget, so a client can tell it from a rate limit and not retry: \
         {text}"
    );

    assert_eq!(
        rows().await,
        before,
        "a refused signup writes neither the account nor its password"
    );
}
