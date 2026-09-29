//! The database round trips a password sign-in waits on, counted the way D1
//! charges them.
//!
//! On Cloudflare every D1 statement is a network round trip from the Worker to
//! the database's region, and a sign-in answers only after the last one it
//! awaits. Measured on a Workers Free deployment (client and Worker in
//! Auckland, D1 in region `OC`), each of those round trips cost about 20 ms,
//! and they, not CPU or the password hash, were most of a warm sign-in's time.
//! So the number of SEQUENTIAL round trips before the response is the latency
//! budget, and the number of statements the D1 query budget.
//!
//! These tests drive the real `site-main` flow of the runtime the native binary
//! builds, the way the Cloudflare entry drives it: inside an
//! [`after_response::scope`], with [`DeferMode::Queued`], so the audit row and
//! the deferred tasks are queued rather than run inline, and what the
//! response waits on is exactly what runs inside the scope.
//!
//! The database is the platform's SQLite service behind [`D1Shaped`], a
//! `DbExec` decorator that makes it behave like the D1 adapter in the three
//! ways that decide what a request costs there: STRICT_SCHEMA on (as the
//! generated wrangler config sets it), one schema cache that outlives the
//! request (D1's is the isolate's), and one round trip per primitive, with a
//! `run_batch`/`run_transaction` one round trip for all of its statements (D1's
//! `db.batch()`). Every `get`/`list`/`create`/... is the shared `DbExec`
//! default D1 runs too, so the statements are the ones D1 would be sent. A
//! fake D1 binding in the wasm test harness could count them as well, but it
//! cannot execute a sign-in: it has no SQL engine behind it.

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use impresspress_core::{
    after_response::{self, AfterResponse},
    blocks::auth::{
        config::{BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY},
        repo::users,
    },
    builder::{boot, GrantSource, InitPolicy},
    deferred::{self, DeferMode},
};
use impresspress_native::InfraConfig;
use impresspress_password::pepper::PasswordPeppers;
use impresspress_server::{build_native_runtime, AppHooks, NativeBootHooks};
use wafer_block::{http_codec, InputStream};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::interfaces::database::{
    codec::JsonColumns,
    exec::{BatchOp, BatchResult, DbExec, TxOp, TxResult},
    schema_cache::SchemaCache,
    service::{Column, DatabaseError, DatabaseService, Record, StatementBudget, Table},
};
use wafer_run::Wafer;
use wafer_sql_utils::Backend;

const ADMIN_EMAIL: &str = "admin@example.com";
const ADMIN_PASSWORD: &str = "correct-horse-battery-staple";

/// One round trip: the wave it started in, how many statements it carried,
/// and what they were.
#[derive(Debug, Clone)]
struct RoundTrip {
    wave: usize,
    statements: usize,
    sql: String,
}

/// What [`D1Shaped`] has seen since the last [`Ledger::take`].
#[derive(Default)]
struct Ledger {
    trips: Mutex<Vec<RoundTrip>>,
    in_flight: AtomicUsize,
    waves: AtomicUsize,
}

impl Ledger {
    fn take(&self) -> Vec<RoundTrip> {
        self.waves.store(0, Ordering::SeqCst);
        std::mem::take(&mut *self.trips.lock().expect("ledger poisoned"))
    }
}

/// Ends a round trip when the primitive returns, however it returns.
struct InFlight<'a>(&'a Ledger);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The platform SQLite service, shaped like the D1 adapter (see the module
/// docs), recording every round trip it is asked for.
struct D1Shaped {
    inner: SQLiteDatabaseService,
    cache: SchemaCache,
    strict: AtomicBool,
    ledger: Arc<Ledger>,
}

impl D1Shaped {
    /// Start a round trip of `statements` statements. A round trip that starts
    /// while none is in flight opens a new wave: the request had to wait for
    /// everything before it. One that starts while another is in flight joins
    /// that wave, as two D1 calls sent together do.
    ///
    /// The `yield_now` hands control back once before the statement runs, so
    /// a caller that sends two primitives together (`futures::join!`) has
    /// started both before either finishes, as it would on Workers, whatever
    /// the local SQLite worker's timing.
    async fn begin(&self, statements: usize, sql: String) -> InFlight<'_> {
        let ledger = &*self.ledger;
        let wave = if ledger.in_flight.fetch_add(1, Ordering::SeqCst) == 0 {
            ledger.waves.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            ledger.waves.load(Ordering::SeqCst)
        };
        ledger
            .trips
            .lock()
            .expect("ledger poisoned")
            .push(RoundTrip {
                wave,
                statements,
                sql,
            });
        let guard = InFlight(ledger);
        tokio::task::yield_now().await;
        guard
    }
}

#[wafer_block::wafer_async_trait]
impl DbExec for D1Shaped {
    const BACKEND: Backend = Backend::Sqlite;

    fn schema_cache(&self) -> Option<&SchemaCache> {
        Some(&self.cache)
    }

    fn strict_schema(&self) -> bool {
        self.strict.load(Ordering::SeqCst)
    }

    fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
        Ok(StatementBudget::Unbounded)
    }

    async fn run_fetch(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError> {
        let _trip = self.begin(1, sql.to_string()).await;
        self.inner.run_fetch(sql, params, json).await
    }

    async fn run_fetch_one(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Record, DatabaseError> {
        let _trip = self.begin(1, sql.to_string()).await;
        self.inner.run_fetch_one(sql, params, json).await
    }

    async fn run_execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let _trip = self.begin(1, sql.to_string()).await;
        self.inner.run_execute(sql, params).await
    }

    async fn run_execute_returning(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError> {
        let _trip = self.begin(1, sql.to_string()).await;
        self.inner.run_execute_returning(sql, params, json).await
    }

    async fn run_scalar_i64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let _trip = self.begin(1, sql.to_string()).await;
        self.inner.run_scalar_i64(sql, params).await
    }

    async fn run_scalar_f64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<f64, DatabaseError> {
        let _trip = self.begin(1, sql.to_string()).await;
        self.inner.run_scalar_f64(sql, params).await
    }

    async fn dbx_table_exists(&self, table: &str) -> Result<bool, DatabaseError> {
        let _trip = self.begin(1, format!("<table exists: {table}>")).await;
        self.inner.dbx_table_exists(table).await
    }

    async fn run_transaction(&self, ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, DatabaseError> {
        let sql = ops
            .iter()
            .map(|op| match op {
                TxOp::Execute { sql, .. } | TxOp::Returning { sql, .. } => *sql,
            })
            .collect::<Vec<_>>()
            .join("; ");
        let _trip = self.begin(ops.len(), sql).await;
        self.inner.run_transaction(ops).await
    }

    async fn run_batch(&self, ops: &[BatchOp<'_>]) -> Result<Vec<BatchResult>, DatabaseError> {
        let sql = ops
            .iter()
            .map(|op| match op {
                BatchOp::Rows { sql, .. }
                | BatchOp::FetchOne { sql, .. }
                | BatchOp::Execute { sql, .. }
                | BatchOp::ScalarI64 { sql, .. }
                | BatchOp::ScalarF64 { sql, .. } => *sql,
            })
            .collect::<Vec<_>>()
            .join("; ");
        let _trip = self.begin(ops.len(), sql).await;
        self.inner.run_batch(ops).await
    }
}

wafer_core::forward_database_service! {
    impl DatabaseService for D1Shaped {
        forward_to DbExec;

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
            // Schema mutation is boot's business, before anything is counted.
            // Each drops this decorator's cached facts, as the SQLite
            // service's own implementations drop its.
            ensure_schema_table: custom,
            ensure_schema_tables: inherit,
            schema_table_exists: forward,
            schema_columns: forward,
            schema_drop_table: custom,
            schema_add_column: custom,
            set_strict_schema: custom,
            statement_budget: forward,
        }

        async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
            self.cache.clear();
            DatabaseService::ensure_schema_table(&self.inner, table).await
        }

        async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
            self.cache.clear();
            DatabaseService::schema_drop_table(&self.inner, name).await
        }

        async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError> {
            self.cache.clear();
            DatabaseService::schema_add_column(&self.inner, table, column).await
        }

        fn set_strict_schema(&self, enabled: bool) {
            self.strict.store(enabled, Ordering::SeqCst);
        }
    }
}

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

/// A booted runtime over a [`D1Shaped`] database, with a bootstrapped admin.
struct Site {
    wafer: Arc<Wafer>,
    database: Arc<dyn DatabaseService>,
    ledger: Arc<Ledger>,
    _tmp: tempfile::TempDir,
}

async fn start() -> Site {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("login.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let infra = infra_for(&db_path, &storage_root);

    let ledger = Arc::new(Ledger::default());
    let database: Arc<dyn DatabaseService> = Arc::new(D1Shaped {
        inner: SQLiteDatabaseService::open(&infra.db_path).expect("open sqlite"),
        cache: SchemaCache::new(),
        strict: AtomicBool::new(false),
        ledger: Arc::clone(&ledger),
    });
    let app_env = HashMap::from([
        (
            BOOTSTRAP_ADMIN_EMAIL_KEY.to_string(),
            ADMIN_EMAIL.to_string(),
        ),
        (
            BOOTSTRAP_ADMIN_PASSWORD_KEY.to_string(),
            ADMIN_PASSWORD.to_string(),
        ),
    ]);
    let mut wafer = build_native_runtime(
        &infra,
        database.clone(),
        &app_env,
        PasswordPeppers::default(),
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
    // Production D1 runs with STRICT_SCHEMA on: the generated wrangler config
    // sets it for every Cloudflare deploy.
    database.set_strict_schema(true);
    Site {
        wafer: wafer.bind_all(),
        database,
        ledger,
        _tmp: tmp,
    }
}

/// The admin's `last_login_at`, read straight from the platform database.
async fn last_login_at(database: &Arc<dyn DatabaseService>) -> Option<String> {
    let opts = wafer_block::db::ListOptions {
        filters: vec![wafer_block::db::Filter {
            field: "email".to_string(),
            operator: wafer_block::db::FilterOp::Equal,
            value: serde_json::json!(ADMIN_EMAIL),
        }],
        limit: Some(1),
        skip_count: true,
        ..Default::default()
    };
    let rows = database
        .list(users::TABLE, &opts)
        .await
        .expect("read the admin's row")
        .records;
    rows.first()
        .and_then(|row| row.data.get("last_login_at"))
        .and_then(serde_json::Value::as_str)
        .filter(|at| !at.is_empty())
        .map(str::to_string)
}

/// What one request cost: the round trips its response waited on, and the
/// ones it left to run after it.
struct Cost {
    status: u16,
    response: Vec<RoundTrip>,
    after: Vec<RoundTrip>,
}

impl Cost {
    fn waves(trips: &[RoundTrip]) -> usize {
        trips.iter().map(|t| t.wave).max().unwrap_or(0)
    }

    fn statements(trips: &[RoundTrip]) -> usize {
        trips.iter().map(|t| t.statements).sum()
    }

    fn describe(trips: &[RoundTrip]) -> String {
        trips
            .iter()
            .map(|t| format!("  wave {} [{}] {}", t.wave, t.statements, t.sql))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// POST a password sign-in as the Cloudflare entry runs a request: dispatch
/// inside a fresh [`after_response::scope`], then the audit row, then the
/// deferred tasks.
async fn sign_in(site: &Site, email: &str, password: &str) -> Cost {
    deferred::set_mode(DeferMode::Queued);
    site.ledger.take();

    let body = serde_json::json!({ "email": email, "password": password }).to_string();
    let msg = http_codec::build_http_message(
        "POST",
        "/b/auth/api/login",
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
    let after = AfterResponse::new();
    let wafer = Arc::clone(&site.wafer);
    let status = after_response::scope(std::rc::Rc::clone(&after), async move {
        http_codec::collect_http_response(
            wafer
                .run("site-main", msg, InputStream::from_bytes(body.into_bytes()))
                .await,
        )
        .await
        .status
    })
    .await;
    let response = site.ledger.take();

    if let Some(row) = after.take_audit_row() {
        after_response::persist_audit_row(site.database.as_ref(), row)
            .await
            .expect("audit row written");
    }
    for task in after.take_tasks() {
        task.await;
    }
    let after = site.ledger.take();
    Cost {
        status,
        response,
        after,
    }
}

/// A warm sign-in's response waits on five sequential D1 round trips, five
/// statements: the account by email, its credential, its role grants, the
/// refresh-token row and the device-list row. Each lookup is the one
/// `SELECT … LIMIT 1` `db::get_by_field` sends, with no `COUNT(*)` beside it.
/// The tokens' `auth_version` and inline role are the ones on the account row
/// read first — the version must be read no later than the roles
/// (`helpers::TokenGrant`), so the row is not read again for them.
/// Everything else it writes — the last-login stamp, the retention throttle
/// check, the audit row — runs after the response.
///
/// On Cloudflare the sign-in route's rate limit adds one more before any of
/// these (`auth::repo::rate_limits::windowed_increment`, one upsert that
/// returns the counter). That limiter is D1-backed only on wasm32; the native
/// one this runtime uses keeps its counters in memory, so it is not counted
/// here.
///
/// "Warm" is the second sign-in in the process: the first fills the caches a
/// Worker isolate holds (schema facts, including the id source of every table
/// it creates into, the config snapshot, the timing-equalization hash) and
/// runs the hourly retention pass.
///
/// The counts fail on earlier code: the roles' two reads ran one after the
/// other, a brand-new login family was first "touched" by an UPDATE that could
/// match nothing, the retention throttle read and the last-login UPDATE ran
/// before the response, the users row was read twice more, for the roles
/// and for the version, each lookup carried an unread `COUNT(*)`, and the
/// second create into an insert-only table probed its id source again.
#[tokio::test]
async fn a_warm_sign_in_waits_on_five_round_trips() {
    let site = start().await;
    let cold = sign_in(&site, ADMIN_EMAIL, ADMIN_PASSWORD).await;
    assert_eq!(cold.status, 200);

    let warm = sign_in(&site, ADMIN_EMAIL, ADMIN_PASSWORD).await;
    assert_eq!(warm.status, 200);
    let trace = format!(
        "response path:\n{}\nafter the response:\n{}",
        Cost::describe(&warm.response),
        Cost::describe(&warm.after)
    );

    assert_eq!(
        Cost::waves(&warm.response),
        5,
        "sequential D1 round trips before the response\n{trace}"
    );
    assert_eq!(
        Cost::statements(&warm.response),
        5,
        "D1 statements before the response\n{trace}"
    );
    // A lookup is its `SELECT … LIMIT 1` alone: no total nothing reads.
    assert!(
        !warm.response.iter().any(|t| t.sql.contains("COUNT(*)")),
        "a sign-in reads no totals\n{trace}"
    );
    // The first sign-in's creates cached every id source this one needs, the
    // audit row's insert-only table included.
    assert!(
        !warm
            .response
            .iter()
            .chain(&warm.after)
            .any(|t| t.sql.contains("id_policy")),
        "a warm sign-in probes no table's id source\n{trace}"
    );

    // The security reads stay on the response path, in order: the account
    // row (which carries the `auth_version` the tokens embed) is read before
    // the role grants, so a role change landing between them leaves the
    // token's version behind the change's bump.
    let wave_of = |needle: &str| {
        warm.response
            .iter()
            .find(|t| t.sql.contains(needle))
            .map(|t| t.wave)
            .unwrap_or_else(|| panic!("no {needle} read on the response path\n{trace}"))
    };
    assert!(
        wave_of("FROM \"wafer_run__auth__users\" WHERE \"email\"")
            < wave_of("FROM \"impresspress__admin__user_roles\""),
        "the account row must be read before the role grants\n{trace}"
    );
    // The refresh-token row is written before the response: without it the
    // refresh token handed out could never be used.
    assert!(
        warm.response
            .iter()
            .any(|t| t.sql.contains("INSERT INTO \"wafer_run__auth__tokens\"")),
        "{trace}"
    );
}

/// The work moved off the response path still happens: the last-login stamp,
/// the retention throttle check and the audit row all run after it.
#[tokio::test]
async fn the_bookkeeping_a_sign_in_defers_still_runs() {
    let site = start().await;
    let cold = sign_in(&site, ADMIN_EMAIL, ADMIN_PASSWORD).await;
    assert_eq!(cold.status, 200);

    let first_stamp = last_login_at(&site.database)
        .await
        .expect("the first sign-in stamped last_login_at");

    let warm = sign_in(&site, ADMIN_EMAIL, ADMIN_PASSWORD).await;
    assert_eq!(warm.status, 200);
    let trace = Cost::describe(&warm.after);

    for (what, needle) in [
        ("the last-login stamp", "UPDATE \"wafer_run__auth__users\""),
        (
            "the retention throttle check",
            "FROM \"wafer_run__auth__maintenance\"",
        ),
        (
            "the audit row",
            "INSERT INTO \"impresspress__admin__request_logs\"",
        ),
    ] {
        assert!(
            warm.after.iter().any(|t| t.sql.contains(needle)),
            "{what} must run after the response:\n{trace}"
        );
        assert!(
            !warm.response.iter().any(|t| t.sql.contains(needle)),
            "{what} must not delay the response:\n{}",
            Cost::describe(&warm.response)
        );
    }
    assert!(
        last_login_at(&site.database)
            .await
            .is_some_and(|at| at >= first_stamp),
        "the deferred stamp must land"
    );
}
