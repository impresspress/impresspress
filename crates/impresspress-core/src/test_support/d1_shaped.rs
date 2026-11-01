//! The platform SQLite service, shaped like the Cloudflare D1 adapter and
//! recording every statement it is sent.
//!
//! On Cloudflare every D1 call — a lone statement, or a whole `db.batch()` —
//! is a network round trip and one call against the per-invocation budget,
//! so the round trips a path makes — and
//! which of them it waits on in sequence — are what it costs there. A fake D1
//! binding in the wasm test harness can count them too, but it cannot execute
//! a sign-in or a boot: it has no SQL engine behind it. [`D1Shaped`] has one.
//!
//! It makes the SQLite service behave like the D1 adapter in the two ways that
//! decide what a request costs there:
//!
//! - **One schema cache that outlives the request** (D1's is the isolate's).
//!   A fresh [`D1Shaped`] is a cold isolate; reusing one is a warm one.
//! - **One round trip per primitive**, with a `run_batch`/`run_transaction`
//!   one round trip for all of its statements (D1's `db.batch()`).
//!
//! Every `get`/`list`/`create`/... is the shared `DbExec` default D1 runs too,
//! so the statements recorded are the ones D1 would be sent — the table-exists
//! probe included, which the shared `exec::table_exists` sends through
//! `run_fetch` on both, so it records the same `sqlite_master` statement D1's
//! adapter sends.
//!
//! A round trip here is one D1 call: `trips().len()` is what the adapter's
//! `spend_call` counts. A new D1 primitive must keep the two agreeing.
//!
//! Users: `impresspress`'s `login_d1_round_trips.rs` (round trips a sign-in
//! waits on), `tests/migration_history.rs` (statements a fresh install sends)
//! and `tests/d1_read_cost.rs` (what a cold isolate sends on boot and first
//! requests).
//!
//! **Fault switch.** [`D1Shaped::fail_table`] makes every statement that
//! names a table fail as D1 would — [`Fault::Unavailable`] for a transient
//! fault, [`Fault::CallBudgetExhausted`] for the invocation's spent call
//! budget, [`Fault::Internal`] for anything else (`no such table`) — without
//! the statement reaching SQLite. The statement is still recorded: D1 counts
//! a call it was sent whether or not it answered. [`D1Shaped::answer_no_rows`]
//! makes one statement "succeed" with an empty `results` instead, as a
//! malformed D1 answer would, so a test can drive the strict decode of what
//! the statement asks.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
};

use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::interfaces::database::{
    codec::JsonColumns,
    exec::{BatchOp, BatchResult, DbExec, TxOp, TxResult},
    schema_cache::SchemaCache,
    service::{Column, DatabaseError, Record, Table, TransactionError},
};
use wafer_sql_utils::Backend;

/// One statement as D1 would receive it.
#[derive(Debug, Clone, PartialEq)]
pub struct Sent {
    pub sql: String,
    pub params: Vec<serde_json::Value>,
}

/// What carried a round trip's statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    /// One primitive, one statement.
    Single,
    /// A read/write batch (`run_batch`): D1's `db.batch()`.
    Batch,
    /// An all-or-nothing transaction (`run_transaction`, which a raw
    /// transaction and a migration file reach): also D1's `db.batch()`.
    Transaction,
}

/// One round trip: the wave it started in, and the statements it carried (one,
/// or every statement of a batch or transaction).
#[derive(Debug, Clone)]
pub struct RoundTrip {
    /// Waves count sequential waits: a round trip that starts while none is in
    /// flight opens a new one; one that starts while another is in flight
    /// joins it, as two D1 calls sent together do.
    pub wave: usize,
    pub carrier: Carrier,
    pub sent: Vec<Sent>,
}

impl RoundTrip {
    /// How many statements this round trip carried.
    pub fn statements(&self) -> usize {
        self.sent.len()
    }

    /// Its statements' SQL, `; `-joined.
    pub fn sql(&self) -> String {
        self.sent
            .iter()
            .map(|sent| sent.sql.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// What a [`D1Shaped`] has seen since the last [`Ledger::take`]. Shared, so
/// several cold isolates over one database file can report into one ledger.
#[derive(Default)]
pub struct Ledger {
    trips: Mutex<Vec<RoundTrip>>,
    in_flight: AtomicUsize,
    waves: AtomicUsize,
}

impl Ledger {
    /// Every round trip recorded since the last call, and a wave count reset
    /// to zero.
    pub fn take(&self) -> Vec<RoundTrip> {
        self.waves.store(0, Ordering::SeqCst);
        std::mem::take(&mut *self.trips.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// How a [`D1Shaped::fail_table`] statement fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// A transient fault: [`DatabaseError::Unavailable`], as the D1 adapter
    /// classifies an overloaded or unreachable database.
    Unavailable,
    /// The invocation's call budget is spent:
    /// [`DatabaseError::CallBudgetExhausted`], as the D1 adapter answers its
    /// own refusal and the platform's ("Too many API requests by single
    /// Worker invocation").
    CallBudgetExhausted,
    /// Any other failure, with D1's message: [`DatabaseError::Internal`] —
    /// `"no such table: …"` for a table that is not there.
    Internal(String),
}

/// Ends a round trip when the primitive returns, however it returns.
struct InFlight<'a>(&'a Ledger);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Yield to the executor exactly once.
struct YieldOnce(bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

/// The platform SQLite service, shaped like the D1 adapter (see the module
/// docs), recording every round trip into its [`Ledger`].
pub struct D1Shaped {
    inner: SQLiteDatabaseService,
    cache: SchemaCache,
    ledger: Arc<Ledger>,
    fault: Mutex<Option<(String, Fault)>>,
    no_rows: Mutex<Option<String>>,
}

impl D1Shaped {
    /// A cold isolate over `inner`: an empty schema cache, reporting into
    /// `ledger`.
    pub fn new(inner: SQLiteDatabaseService, ledger: Arc<Ledger>) -> Self {
        Self {
            inner,
            cache: SchemaCache::new(),
            ledger,
            fault: Mutex::new(None),
            no_rows: Mutex::new(None),
        }
    }

    /// A cold isolate over the SQLite file at `path`.
    ///
    /// # Panics
    ///
    /// When the file cannot be opened: this is fixture setup.
    pub fn open(path: &std::path::Path, ledger: Arc<Ledger>) -> Self {
        let path = path.to_str().expect("a UTF-8 database path");
        Self::new(
            SQLiteDatabaseService::open(path).expect("open the SQLite database"),
            ledger,
        )
    }

    /// The ledger this decorator records into.
    pub fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }

    /// From now on, every statement naming `table` fails with `fault`.
    pub fn fail_table(&self, table: &str, fault: Fault) {
        *self.fault.lock().unwrap_or_else(|p| p.into_inner()) = Some((table.to_string(), fault));
    }

    /// From now on, a `run_fetch` of exactly `sql` is recorded and answered
    /// with no rows, without reaching SQLite and exempt from the fault
    /// switch: D1 answering a statement with an empty `results`. A read that
    /// expects a row (the existence probe, a `COUNT`) must refuse it.
    pub fn answer_no_rows(&self, sql: &str) {
        *self.no_rows.lock().unwrap_or_else(|p| p.into_inner()) = Some(sql.to_string());
    }

    /// Statements reach SQLite again.
    pub fn clear_fault(&self) {
        *self.fault.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self.no_rows.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    fn answers_no_rows(&self, sql: &str) -> bool {
        self.no_rows
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_deref()
            == Some(sql)
    }

    /// The fault `sent` meets, if the switch names a table it touches: in its
    /// SQL (where generated statements quote it) or as a parameter (where the
    /// introspection statements pass it).
    fn fault_for(&self, sent: &[Sent]) -> Option<DatabaseError> {
        let guard = self.fault.lock().unwrap_or_else(|p| p.into_inner());
        let (table, fault) = guard.as_ref()?;
        let names = |s: &Sent| {
            !self.answers_no_rows(&s.sql)
                && (s.sql.contains(table.as_str())
                    || s.params.iter().any(|p| p.as_str() == Some(table.as_str())))
        };
        sent.iter().any(names).then(|| match fault {
            Fault::Unavailable => {
                DatabaseError::Unavailable(format!("D1_ERROR: injected outage on {table}"))
            }
            Fault::CallBudgetExhausted => DatabaseError::CallBudgetExhausted(format!(
                "D1 refused the call: Too many API requests by single Worker invocation \
                 (injected on {table})"
            )),
            Fault::Internal(message) => DatabaseError::Internal(message.clone()),
        })
    }

    /// Start a round trip carrying `sent`, or fail it if the fault switch
    /// names a table it touches. A round trip that starts while none is in
    /// flight opens a new wave: the caller had to wait for everything before
    /// it. One that starts while another is in flight joins that wave.
    ///
    /// The yield hands control back once before the statement runs, so a
    /// caller that sends two primitives together (`futures::join!`) has
    /// started both before either finishes, as it would on Workers, whatever
    /// the local SQLite worker's timing.
    async fn begin(
        &self,
        carrier: Carrier,
        sent: Vec<Sent>,
    ) -> Result<InFlight<'_>, DatabaseError> {
        let ledger = &*self.ledger;
        let wave = if ledger.in_flight.fetch_add(1, Ordering::SeqCst) == 0 {
            ledger.waves.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            ledger.waves.load(Ordering::SeqCst)
        };
        let fault = self.fault_for(&sent);
        ledger
            .trips
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(RoundTrip {
                wave,
                carrier,
                sent,
            });
        let guard = InFlight(ledger);
        YieldOnce(false).await;
        match fault {
            Some(error) => Err(error),
            None => Ok(guard),
        }
    }

    async fn begin_one(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<InFlight<'_>, DatabaseError> {
        self.begin(
            Carrier::Single,
            vec![Sent {
                sql: sql.to_string(),
                params: params.to_vec(),
            }],
        )
        .await
    }
}

#[wafer_block::wafer_async_trait]
impl DbExec for D1Shaped {
    const BACKEND: Backend = Backend::Sqlite;

    fn schema_cache(&self) -> Option<&SchemaCache> {
        Some(&self.cache)
    }

    async fn run_fetch(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError> {
        let _trip = self.begin_one(sql, params).await?;
        if self.answers_no_rows(sql) {
            return Ok(Vec::new());
        }
        self.inner.run_fetch(sql, params, json).await
    }

    async fn run_fetch_one(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Record, DatabaseError> {
        let _trip = self.begin_one(sql, params).await?;
        self.inner.run_fetch_one(sql, params, json).await
    }

    async fn run_execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let _trip = self.begin_one(sql, params).await?;
        self.inner.run_execute(sql, params).await
    }

    async fn run_execute_returning(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError> {
        let _trip = self.begin_one(sql, params).await?;
        self.inner.run_execute_returning(sql, params, json).await
    }

    async fn run_scalar_i64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let _trip = self.begin_one(sql, params).await?;
        self.inner.run_scalar_i64(sql, params).await
    }

    async fn run_scalar_f64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<f64, DatabaseError> {
        let _trip = self.begin_one(sql, params).await?;
        self.inner.run_scalar_f64(sql, params).await
    }

    /// One round trip for every statement, all-or-nothing as D1's
    /// `db.batch()` is (the SQLite service's own transaction), and failing
    /// as a batch fails: one error for the whole call, its classified code
    /// and message kept and no statement named, because D1's rejection names
    /// none. SQLite knows which statement failed; D1 does not, so neither
    /// does this.
    async fn run_transaction(&self, ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, TransactionError> {
        let sent = ops
            .iter()
            .map(|op| match op {
                TxOp::Execute { sql, params } | TxOp::Returning { sql, params, .. } => Sent {
                    sql: (*sql).to_string(),
                    params: params.to_vec(),
                },
            })
            .collect();
        let _trip = self
            .begin(Carrier::Transaction, sent)
            .await
            .map_err(TransactionError::whole)?;
        self.inner
            .run_transaction(ops)
            .await
            .map_err(|failed| TransactionError::whole(failed.error))
    }

    async fn run_batch(&self, ops: &[BatchOp<'_>]) -> Result<Vec<BatchResult>, DatabaseError> {
        let sent = ops
            .iter()
            .map(|op| match op {
                BatchOp::Rows { sql, params, .. }
                | BatchOp::FetchOne { sql, params, .. }
                | BatchOp::Execute { sql, params }
                | BatchOp::ScalarI64 { sql, params }
                | BatchOp::ScalarF64 { sql, params } => Sent {
                    sql: (*sql).to_string(),
                    params: params.to_vec(),
                },
            })
            .collect();
        let _trip = self.begin(Carrier::Batch, sent).await?;
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
            exec_raw_transaction: forward,
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
            // Refused, as the D1 adapter refuses them: D1's schema is
            // migration-owned, so a path that reached one of these would
            // fail on Cloudflare, and must fail here rather than succeed
            // uncounted.
            ensure_schema_table: custom,
            ensure_schema_tables: inherit,
            schema_table_exists: forward,
            schema_columns: forward,
            schema_drop_table: custom,
            schema_add_column: custom,
            catalog_tables: forward,
            catalog_columns: forward,
            catalog_row_count: forward,
        }

        async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
            Err(schema_mutation_refused("ensure_schema_table", &table.name))
        }

        async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
            Err(schema_mutation_refused("schema_drop_table", name))
        }

        async fn schema_add_column(&self, table: &str, _column: &Column) -> Result<(), DatabaseError> {
            Err(schema_mutation_refused("schema_add_column", table))
        }
    }
}

/// The refusal the D1 adapter answers a runtime schema mutation with.
fn schema_mutation_refused(method: &str, target: &str) -> DatabaseError {
    DatabaseError::Internal(format!(
        "runtime schema mutation unsupported on Cloudflare D1: `{method}` on `{target}` — \
         D1 schema is migration-owned"
    ))
}

#[cfg(test)]
mod tests {
    use wafer_core::interfaces::database::service::DatabaseService;

    use super::*;

    fn shaped() -> (Arc<D1Shaped>, Arc<Ledger>) {
        let ledger = Arc::new(Ledger::default());
        let db = Arc::new(D1Shaped::new(
            SQLiteDatabaseService::open_in_memory().expect("open sqlite"),
            ledger.clone(),
        ));
        (db, ledger)
    }

    /// The fault switch fails a statement naming the table, records it as
    /// sent, and leaves other tables alone; clearing it lets the table answer.
    #[tokio::test]
    async fn the_fault_switch_fails_only_the_named_table() {
        let (db, ledger) = shaped();
        DatabaseService::exec_raw(&*db, "CREATE TABLE faulty (id TEXT PRIMARY KEY)", &[])
            .await
            .expect("create faulty");
        DatabaseService::exec_raw(&*db, "CREATE TABLE healthy (id TEXT PRIMARY KEY)", &[])
            .await
            .expect("create healthy");
        ledger.take();

        db.fail_table("faulty", Fault::Unavailable);
        let refused = DatabaseService::list(&*db, "faulty", &Default::default())
            .await
            .expect_err("the switch fails the table");
        assert!(
            matches!(refused, DatabaseError::Unavailable(_)),
            "{refused:?}"
        );
        assert!(
            !ledger.take().is_empty(),
            "a refused statement is still one D1 was sent"
        );
        assert_eq!(
            DatabaseService::count(&*db, "healthy", &[])
                .await
                .expect("another table still answers"),
            0
        );

        db.fail_table("faulty", Fault::Internal("no such table: faulty".into()));
        let missing = DatabaseService::count(&*db, "faulty", &[])
            .await
            .expect_err("an internal fault fails the table");
        assert_eq!(missing.to_string(), "database error: no such table: faulty");

        db.clear_fault();
        assert_eq!(
            DatabaseService::count(&*db, "faulty", &[])
                .await
                .expect("cleared"),
            0
        );
    }

    /// **A raw transaction runs as D1 runs a `db.batch()`**, so a migration
    /// file applied through [`D1Shaped`] costs and fails as it would on D1:
    /// one round trip for every statement, all-or-nothing with the DDL in it,
    /// and a failure that keeps its classified code and message but names no
    /// statement, because D1's rejection names none (D1-BATCH-MEASUREMENT.md).
    #[tokio::test]
    async fn a_raw_transaction_is_one_round_trip_that_fails_whole() {
        use wafer_core::interfaces::database::service::RawStatement;

        let raw = |query: &str| RawStatement {
            query: query.to_string(),
            args: vec![],
        };
        let tmp = tempfile::tempdir().expect("tempdir");
        let ledger = Arc::new(Ledger::default());
        let db = D1Shaped::open(&tmp.path().join("batch.sqlite3"), Arc::clone(&ledger));

        let err = DatabaseService::exec_raw_transaction(
            &db,
            &[
                raw("CREATE TABLE d1_tx_a (id TEXT PRIMARY KEY)"),
                raw("CREATE TABLE d1_tx_b (x TEXT)"),
                raw("INSERT INTO d1_tx_a (id) VALUES ('k')"),
                raw("INSERT INTO d1_tx_a (id) VALUES ('k')"),
            ],
        )
        .await
        .expect_err("a taken key refuses the batch");
        let trips = ledger.take();
        assert_eq!(
            trips.iter().map(RoundTrip::statements).collect::<Vec<_>>(),
            [4],
            "one round trip carrying every statement: {trips:?}"
        );
        assert_eq!(err.statement, None, "a D1 batch names no statement: {err}");
        match &err.error {
            DatabaseError::AlreadyExists(message) => {
                assert!(message.contains("d1_tx_a"), "D1's own message: {message}");
            }
            other => panic!("a taken key is AlreadyExists, not {other:?}"),
        }
        for table in ["d1_tx_a", "d1_tx_b"] {
            assert!(
                !DatabaseService::schema_table_exists(&db, table)
                    .await
                    .expect("table lookup"),
                "{table} survived the refused batch"
            );
        }

        ledger.take();
        let counts = DatabaseService::exec_raw_transaction(
            &db,
            &[
                raw("CREATE TABLE d1_tx_a (id TEXT PRIMARY KEY)"),
                raw("INSERT INTO d1_tx_a (id) VALUES ('k')"),
            ],
        )
        .await
        .expect("the batch commits");
        assert_eq!(counts.len(), 2);
        assert_eq!(
            ledger
                .take()
                .iter()
                .map(RoundTrip::statements)
                .collect::<Vec<_>>(),
            [2],
            "one round trip"
        );
        assert!(DatabaseService::schema_table_exists(&db, "d1_tx_a")
            .await
            .expect("table lookup"));
    }

    /// A runtime schema mutation is refused, as D1 refuses it, and sends
    /// nothing.
    #[tokio::test]
    async fn a_schema_mutation_is_refused_as_on_d1() {
        let (db, ledger) = shaped();
        let err = DatabaseService::schema_drop_table(&*db, "anything")
            .await
            .expect_err("D1 refuses a runtime schema mutation");
        assert!(err.to_string().contains("migration-owned"), "{err}");
        assert!(ledger.take().is_empty());
    }

    /// Two primitives sent together share a wave; one after the other, they
    /// open two.
    #[tokio::test]
    async fn concurrent_round_trips_share_a_wave() {
        let (db, ledger) = shaped();
        DatabaseService::exec_raw(&*db, "CREATE TABLE t (id TEXT PRIMARY KEY)", &[])
            .await
            .expect("create");
        ledger.take();

        let (a, b) = futures::join!(
            DatabaseService::count(&*db, "t", &[]),
            DatabaseService::count(&*db, "t", &[])
        );
        a.expect("a");
        b.expect("b");
        let trips = ledger.take();
        assert!(trips.iter().all(|t| t.wave == 1), "{trips:?}");

        DatabaseService::count(&*db, "t", &[]).await.expect("c");
        DatabaseService::count(&*db, "t", &[]).await.expect("d");
        let waves: Vec<usize> = ledger.take().iter().map(|t| t.wave).collect();
        assert_eq!(waves, vec![1, 2]);
    }
}
