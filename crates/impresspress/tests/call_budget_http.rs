//! A request whose invocation has no database call left is answered 429,
//! through a real route, and writes nothing.
//!
//! A Cloudflare D1 service counts the calls its invocation has sent against
//! D1's per-invocation limit and refuses a call past it before sending it,
//! with `DatabaseError::CallBudgetExhausted`
//! (`impresspress-cloudflare`'s `database` module); D1's own refusal of the
//! limit maps to the same error. This drives that refusal end to end on the
//! runtime the binary builds: `build_native_runtime`, the shared `boot`, the
//! `site-main` flow, the router and the auth-ui signup handler, whose account
//! and credential rows are one `batch`, one call. The database is the real
//! SQLite service behind a decorator that refuses its multi-row writes once
//! armed (boot's own writes run unrefused, as a deploy's `/_deploy/init` is
//! its own invocation).
//!
//! The 429 means "this request did too much", not "try again later": the
//! same signup retried would make the same calls. The body carries the
//! refusal's text and its detail code.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

use impresspress_core::builder::{boot, GrantSource, InitPolicy};
use impresspress_native::InfraConfig;
use impresspress_server::{build_native_runtime, AppHooks, NativeBootHooks};
use wafer_block::{http_codec, InputStream};
use wafer_core::interfaces::database::service::{
    DatabaseError, DatabaseService, WriteOp, WriteOutcome,
};

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

/// The real SQLite service, whose `create_many` and `batch` refuse with
/// `CallBudgetExhausted` — what a D1 service answers when the invocation has
/// no call left — once a test arms it, and run until then. Every other
/// operation is the inner service's.
struct BudgetedDb {
    inner: Arc<dyn DatabaseService>,
    spent: Arc<Mutex<bool>>,
}

const REFUSAL: &str = "this invocation has sent 1000 D1 calls and may send 1000";

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
            create_many: custom,
            update: forward,
            delete: forward,
            count: forward,
            sum: forward,
            query_raw: forward,
            exec_raw: forward,
            exec_raw_transaction: forward,
            delete_where: forward,
            delete_where_count: forward,
            take_where: forward,
            update_where: forward,
            update_where_count: forward,
            increment_field_where: forward,
            upsert: forward,
            aggregate: forward,
            batch: custom,
            insert_guarded: forward,
            update_guarded: forward,
            ensure_schema_table: forward,
            ensure_schema_tables: forward,
            schema_table_exists: forward,
            schema_columns: forward,
            schema_drop_table: forward,
            schema_add_column: forward,
            catalog_tables: forward,
            catalog_columns: forward,
            catalog_row_count: forward,
        }

        async fn create_many(
            &self,
            collection: &str,
            rows: Vec<HashMap<String, serde_json::Value>>,
        ) -> Result<i64, DatabaseError> {
            if *self.spent.lock().expect("spent lock") {
                return Err(DatabaseError::CallBudgetExhausted(REFUSAL.into()));
            }
            self.inner.create_many(collection, rows).await
        }

        async fn batch(&self, ops: Vec<WriteOp>) -> Result<Vec<WriteOutcome>, DatabaseError> {
            if *self.spent.lock().expect("spent lock") {
                return Err(DatabaseError::CallBudgetExhausted(REFUSAL.into()));
            }
            self.inner.batch(ops).await
        }
    }
}

#[tokio::test]
async fn a_signup_with_no_call_left_is_a_429_that_writes_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("call_budget.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    let infra = infra_for(&db_path, &storage_root);
    let sqlite = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");
    let spent = Arc::new(Mutex::new(false));
    let database: Arc<dyn DatabaseService> = Arc::new(BudgetedDb {
        inner: sqlite.clone(),
        spent: Arc::clone(&spent),
    });
    let mut wafer = build_native_runtime(
        &infra,
        database,
        &HashMap::new(),
        Default::default(),
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

    // The invocation has no call left for the signup's one batch, its
    // account and credential rows.
    *spent.lock().unwrap() = true;

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
        text.contains(REFUSAL),
        "the refusal says what the invocation spent: {text}"
    );
    let body: serde_json::Value = serde_json::from_slice(&parts.body).expect("a JSON error body");
    assert_eq!(
        body["code"],
        wafer_block::wire::database::CALL_BUDGET_EXHAUSTED,
        "the 429 names the budget, so a client can tell it from a rate limit and not retry: \
         {text}"
    );

    assert_eq!(
        rows().await,
        before,
        "a refused signup writes neither the account nor its password"
    );
}
