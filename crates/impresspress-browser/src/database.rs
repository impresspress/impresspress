//! Browser-side `DatabaseService` backed by sql.js via the JS bridge.
//!
//! The browser backend implements only the [`DbExec`] execution *primitives*
//! (synchronous `bridge::db_query_raw` / `bridge::db_exec_raw`, marshaling
//! params/rows across the bridge as structured `JsValue`s via
//! `db_codec`/`serde_wasm_bindgen` — no JSON-string round trip) and then
//! decoding each row with the shared
//! [`wafer_core::interfaces::database::codec`], the same policy the native
//! SQLite and Cloudflare D1 backends decode with. All
//! `get/list/count/sum/create/update/delete` orchestration — filter/IN
//! expansion, sorted-key INSERT/UPDATE construction, lazy column-add,
//! table-exists guards — is inherited from the shared `wafer-core` [`DbExec`]
//! defaults, identical to `wafer-block-sqlite`, `wafer-block-postgres`, and the
//! Cloudflare D1 backend.
//!
//! Tables must already exist via the owning block's migration files (applied
//! at `lifecycle(Init)`). The shared `ensure_data_columns` adds only a missing
//! *column* a write's data names (always `TEXT` on SQLite), and
//! `require_columns` refuses a read, filter or guard that names an unknown
//! column instead of adding it — unless STRICT_SCHEMA is on, which this
//! backend honours (see [`STRICT_SCHEMA`]): then neither introspects, and the
//! statement itself fails on an unmigrated column.
//!
//! ## Schema cache
//!
//! The shared executor fronts its operations with introspection: a
//! table-exists probe and a column list (off in STRICT_SCHEMA mode), and a
//! primary-key lookup before every sorted or paged `list`, whose `ORDER BY`
//! ends with the key. Each of those is a `bridge::db_query_raw` round trip
//! into sql.js, so this backend memoizes them in [`SCHEMA_CACHE`], one cache
//! for the one database. The shared defaults invalidate it on every schema
//! change they make (`exec_raw`, `ensure_schema_table`, lazy column-add); the
//! schema changes made anywhere else invalidate it themselves: this file's own
//! `schema_drop_table` / `schema_add_column`, `vector::service`'s DDL (through
//! [`forget_table_schema`]), and `db_init` reopening the database (through
//! [`forget_schema`]).
//!
//! ## The `DatabaseService` impl is a ledger, not a list of forwards
//!
//! It is written with [`wafer_core::forward_database_service!`], whose
//! `ops { … }` block must name every operation on the trait or it does not
//! expand. Eight of the trait's operations carry defaults that are NOT
//! pass-throughs, so an implementation that leaves one out does not inherit
//! "the same behaviour" — it inherits a different one, silently. Writing the
//! word `inherit` is how a default gets taken here.
//!
//! ## OPFS flush durability contract
//!
//! `run_execute` (the `DbExec` primitive) does NOT flush to OPFS — it only
//! mutates sql.js's in-memory database. Flushing is done exactly once per
//! *logical* [`DatabaseService`] mutation, by [`BrowserDatabaseService::with_flush`],
//! which wraps every mutating `DatabaseService` method. A logical mutation
//! (e.g. `create`) may issue several SQL statements internally (a lazy
//! column-add ALTER, then the INSERT) — those all share the ONE flush at the
//! end of the call, instead of the previous behavior of flushing after every
//! single statement.
//!
//! The contract itself lives in [`with_flush_mapped`] and its precedence rules
//! in [`resolve_flush_outcome`], because this is not the only writer to that
//! database: `vector::service` writes the same sql.js file through the same
//! bridge and goes through the same helper. There is one durability contract
//! for the crate, not one per service.
//!
//! The multi-write operations — `create_many`, `batch` and the guarded writes —
//! are one logical mutation each, so a `create_many` of a thousand rows is one
//! flush, not a thousand. They reach sql.js through
//! [`DbExec::run_transaction`], which this backend runs between `BEGIN` and
//! `COMMIT` (see [`in_transaction`]).
//!
//! ## A taken key
//!
//! sql.js reports a failed statement as a JS exception carrying SQLite's
//! message and no code, so the primitives classify that text with
//! [`impresspress_core::sqlite_text_error::statement_error`]: a primary- or
//! unique-key violation is [`DatabaseError::AlreadyExists`], as on the native
//! backends, and anything else `Internal`.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        LazyLock,
    },
};

// The `forward_database_service!` ledger below spells every generated
// signature with a fully-qualified path, so only the types the `custom` bodies
// name in their own signatures are imported here.
use wafer_block::db::Filter;
use wafer_core::interfaces::database::{
    codec::{record_from_json_row, scalar_f64, scalar_i64, JsonColumns},
    exec::{DbExec, TxOp, TxResult},
    schema_cache::SchemaCache,
    service::{
        CapGuard, Column, DatabaseError, DatabaseService, GuardedInsert, GuardedUpdate, Record,
        Table, UpsertSpec, WriteOp, WriteOutcome,
    },
};
use wafer_sql_utils::{introspect, Backend};

use crate::{bridge, db_codec};

/// The resolved `WAFER_RUN__DATABASE__STRICT_SCHEMA` verdict, written once by
/// `DatabaseService::set_strict_schema` at the shared `DatabaseBlock`'s `Init`
/// and read by [`DbExec::strict_schema`] on every operation.
///
/// A static rather than a field because [`BrowserDatabaseService`] is a unit
/// struct: it carries no handle, and every instance addresses the one global
/// sql.js/OPFS database — `impresspress-web` constructs a second one for its
/// boot hook precisely because they are interchangeable. STRICT_SCHEMA is a
/// property of that database, so a per-instance field would let two handles
/// disagree about the same schema.
static STRICT_SCHEMA: AtomicBool = AtomicBool::new(false);

/// Memoized table-exists / column-list / primary-key facts for the one
/// sql.js database, returned by [`DbExec::schema_cache`].
///
/// A static for the reason [`STRICT_SCHEMA`] is one: every
/// [`BrowserDatabaseService`] handle addresses the same database, so they
/// share what is known about its schema. A per-handle cache would let the
/// handle `impresspress-web`'s boot hook builds keep facts that a migration
/// run through the runtime's handle had already invalidated.
static SCHEMA_CACHE: LazyLock<SchemaCache> = LazyLock::new(SchemaCache::new);

/// Drop every memoized schema fact. Called when the whole database changes
/// under the cache — `db_init` reopening it — rather than one table's shape.
pub(crate) fn forget_schema() {
    SCHEMA_CACHE.clear();
}

/// Drop the memoized schema facts for one table, for a schema change the
/// shared executor did not make and so did not invalidate for: the DDL
/// `vector::service` runs straight through the bridge.
pub(crate) fn forget_table_schema(table: &str) {
    SCHEMA_CACHE.invalidate(table);
}

/// Browser-side DatabaseService backed by sql.js via the JS bridge.
pub struct BrowserDatabaseService;

/// The crate's ONE durability contract: run a mutating `op`, then flush the
/// sql.js database to OPFS exactly once, whatever `op` returned.
///
/// [`BrowserDatabaseService::with_flush`] is this with `E = DatabaseError`;
/// `vector::service` is the other caller, with `E = VectorError`. `map_flush`
/// turns the JS rejection into the caller's error type, which is the only
/// thing that ever differed between them — the precedence rules below are the
/// contract and must not be restated per caller. Before this was shared, the
/// vector service ran `bridge::dbFlush()` itself with `?`, which skipped the
/// flush entirely whenever the operation that mutated the database failed.
///
/// See [`resolve_flush_outcome`] for the precedence and why each arm is what
/// it is.
///
/// Every logical mutation also starts and ends on a connection that is not
/// inside a transaction — see [`end_open_transaction`] for why, and for what
/// each end does when it finds one.
pub(crate) async fn with_flush_mapped<T, E>(
    op: impl std::future::Future<Output = Result<T, E>>,
    map_flush: impl FnOnce(String) -> E,
) -> Result<T, E> {
    match end_open_transaction() {
        Ok(false) => {}
        Ok(true) => tracing::error!(
            "a transaction was open on the sql.js connection before a write; rolled it back, \
             discarding every statement run inside it since it began"
        ),
        // The write runs anyway: when the connection cannot be settled (no
        // database loaded, a wedged connection) its own statements fail too,
        // and they say more about why than this probe can.
        Err(e) => {
            tracing::error!(error = %e, "could not check the sql.js connection before a write")
        }
    }
    with_flush_through(op, flush_through_bridge, map_flush).await
}

/// The one flush this crate performs: end whatever transaction the operation
/// left open, then hand the sql.js database to `bridge.js` to write out to
/// OPFS.
///
/// The flush is attempted whatever the transaction check found, because the
/// committed statements before it are owed their durability. A transaction
/// that was still open is an error even when the flush succeeds: its
/// statements were rolled back, so the operation must not be reported done.
async fn flush_through_bridge() -> Result<(), String> {
    let left_open = end_open_transaction();
    let flushed = bridge::dbFlush()
        .await
        .map(|_| ())
        .map_err(|e| format!("flush to OPFS: {}", bridge::describe(&e)));
    match left_open {
        Ok(false) => flushed,
        Ok(true) => Err(
            "the write left a transaction open on the sql.js connection; it was rolled back, \
             with every statement run inside it"
                .to_string(),
        ),
        Err(e) => Err(format!("end the transaction the write left open: {e}")),
    }
}

/// Roll back the transaction open on the sql.js connection, if there is one:
/// `Ok(true)` when there was, `Ok(false)` when the connection was clean.
///
/// sql.js offers no way to ask whether a transaction is open, so the
/// `ROLLBACK` is the question: SQLite refuses it with
/// [`NO_ACTIVE_TRANSACTION`] exactly when there is none.
///
/// Why every logical mutation asks, at both ends: a transaction left open —
/// a `BEGIN` sent through the unflushed `query_raw`, or one a failed
/// `ROLLBACK` could not end — swallows everything after it. Every later
/// statement runs inside it, and the next flush's `db.export()`, which closes
/// and reopens the connection, rolls all of them back, although each
/// reported success. Asking before the write keeps the write's own
/// statements out of a transaction that is not its own; asking before the
/// flush turns that silent rollback into an error the caller sees.
///
/// Rolling back changes the schema back to what it was when the transaction
/// began, so the memoized schema is dropped with it.
fn end_open_transaction() -> Result<bool, String> {
    match bridge_control("ROLLBACK") {
        Ok(()) => {
            forget_schema();
            Ok(true)
        }
        Err(refused) if refused.contains(NO_ACTIVE_TRANSACTION) => Ok(false),
        Err(refused) => Err(refused),
    }
}

/// [`with_flush_mapped`] with the flush supplied by the caller.
///
/// `flush` is a closure, not a future, so it cannot be started before `op`
/// finishes — and so the ONE property the whole contract rests on, that the
/// flush runs whatever `op` returned, is assertable without a bridge or an
/// OPFS. That property is the regression this shape exists to prevent: the
/// vector service used to run `bridge::dbFlush()` with `?`, which skipped the
/// flush entirely whenever the mutating operation failed.
async fn with_flush_through<T, E, Fut>(
    op: impl std::future::Future<Output = Result<T, E>>,
    flush: impl FnOnce() -> Fut,
    map_flush: impl FnOnce(String) -> E,
) -> Result<T, E>
where
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let result = op.await;
    let flush = flush().await.map_err(map_flush);
    resolve_flush_outcome(result, flush)
}

/// Which of an operation's outcome and its flush's outcome the caller is told
/// about. Pure, so it is testable without a bridge (`flush_precedence`).
///
/// - `op` succeeds, flush succeeds → `Ok`. The common case: durable.
/// - `op` succeeds, flush fails → the flush error. The mutation is sitting in
///   memory only (quota exceeded, OPFS permission revoked); reporting success
///   would tell the caller data is durable when a Service Worker eviction
///   could still lose it.
/// - `op` fails → the operation's own error, whatever the flush did. It is the
///   more specific and more actionable of the two. The flush is still
///   *attempted* — a failed logical operation may have applied some of its
///   statements already (a lazy column-add ALTER before a rejected INSERT),
///   and skipping the flush would leave those in memory until some unrelated
///   later mutation happened to write them out.
pub(crate) fn resolve_flush_outcome<T, E>(op: Result<T, E>, flush: Result<(), E>) -> Result<T, E> {
    match (op, flush) {
        (Ok(v), Ok(())) => Ok(v),
        (Ok(_), Err(flush_err)) => Err(flush_err),
        (Err(op_err), _) => Err(op_err),
    }
}

// SAFETY: `BrowserDatabaseService` is a unit struct; the state its handles
// share (`STRICT_SCHEMA`, `SCHEMA_CACHE`) is in statics that are `Sync`.
// wasm32-unknown-unknown has no threads, so the `Send`/`Sync` bounds
// required by `Arc<dyn DatabaseService>` are satisfied trivially — no
// cross-thread aliasing or data races are possible.
unsafe impl Send for BrowserDatabaseService {}
unsafe impl Sync for BrowserDatabaseService {}

impl BrowserDatabaseService {
    /// Run a mutating `op`, then flush the sql.js DB to OPFS exactly once —
    /// this is the coalescing point described in the module doc comment.
    /// [`with_flush_mapped`] owns the contract; this is it at
    /// `E = DatabaseError`.
    async fn with_flush<T>(
        &self,
        op: impl std::future::Future<Output = Result<T, DatabaseError>>,
    ) -> Result<T, DatabaseError> {
        with_flush_mapped(op, DatabaseError::Internal).await
    }

    /// Run `sql` and hand back the raw per-column JSON row objects sql.js
    /// resolved, undecoded.
    ///
    /// The only browser-specific step in a read is crossing the bridge; what
    /// a row *means* is [`wafer_core::interfaces::database::codec`]'s job, and
    /// every caller below feeds these rows straight into it. `bridge::
    /// db_query_raw` is synchronous, so this is not `async`.
    fn query_json_rows(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<serde_json::Value>, DatabaseError> {
        let params_js = db_codec::params_to_js(params).map_err(DatabaseError::Internal)?;
        let value = bridge::db_query_raw(sql, params_js).map_err(|e| statement_failed(&e))?;
        db_codec::rows_from_js(value).map_err(DatabaseError::Internal)
    }

    /// The first row of a single-row query (the scalar-aggregate shape), or
    /// `None` for an empty result — the argument shape
    /// [`scalar_i64`]/[`scalar_f64`] take.
    fn query_first_json_row(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Option<serde_json::Value>, DatabaseError> {
        Ok(self.query_json_rows(sql, params)?.into_iter().next())
    }
}

// ─── DbExec primitives — the only backend-specific execution code ─────────────

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl DbExec for BrowserDatabaseService {
    const BACKEND: Backend = Backend::Sqlite;

    /// The database-wide [`SCHEMA_CACHE`]. Without it every sorted or paged
    /// `list` would read the table's primary key through the bridge first.
    fn schema_cache(&self) -> Option<&SchemaCache> {
        Some(&SCHEMA_CACHE)
    }

    /// The flag `DatabaseService::set_strict_schema` recorded. When it is on,
    /// the shared orchestration skips the per-operation table-exists probe and
    /// the lazy ADD COLUMN path, trusting the migrated schema.
    fn strict_schema(&self) -> bool {
        STRICT_SCHEMA.load(Ordering::Relaxed)
    }

    /// sql.js runs in the page's own worker and has no per-invocation
    /// statement limit, so a write of any size is admitted.
    fn statement_budget(
        &self,
    ) -> Result<wafer_core::interfaces::database::service::StatementBudget, DatabaseError> {
        Ok(wafer_core::interfaces::database::service::StatementBudget::Unbounded)
    }

    /// Decoding is [`record_from_json_row`], the one policy every SQL-family
    /// backend now shares — the private `db_codec::build_records` this
    /// replaced was the last of the three copies.
    ///
    /// **Behaviour difference, taken deliberately:** `build_records` returned
    /// `Err("expected row object")` for a row that was not a JSON object,
    /// where `record_from_json_row` returns an empty `Record`. The shared
    /// answer is the right one. The bridge cannot produce a non-object row —
    /// `bridge.js`'s `dbQueryRaw` builds every row from sql.js's column-name
    /// array, so the error arm was unreachable in production and only ever
    /// diverged the browser from the other two adapters. Where it *could*
    /// fire it is also the worse answer: it fails the whole query (every row,
    /// including the well-formed ones) with a message that names no table, no
    /// column and no row, and it makes one platform report a hard error for a
    /// shape the other two report as an empty record. A decode policy that
    /// three backends share is only worth anything if all three answer the
    /// same; keeping a fourth answer here is what unification is for.
    async fn run_fetch(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError> {
        Ok(self
            .query_json_rows(sql, params)?
            .into_iter()
            .map(|row| record_from_json_row(row, json))
            .collect())
    }

    async fn run_fetch_one(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Record, DatabaseError> {
        let records = self.run_fetch(sql, params, json).await?;
        records.into_iter().next().ok_or(DatabaseError::NotFound)
    }

    async fn run_execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let params_js = db_codec::params_to_js(params).map_err(DatabaseError::Internal)?;
        let rows_modified =
            bridge::db_exec_raw(sql, params_js).map_err(|e| statement_failed(&e))?;
        // NOTE: deliberately no `bridge::dbFlush()` here — flushing is
        // coalesced at the `DatabaseService` method boundary via
        // `with_flush`. See the module doc comment.
        Ok(rows_modified as i64)
    }

    /// Delegates to [`run_fetch`](Self::run_fetch): sql.js is a single
    /// in-process database behind one bridge handle, so there is no
    /// reader/writer split for a `DELETE … RETURNING` to land on the wrong
    /// side of, and `bridge.js`'s `dbQueryRaw` runs the statement through
    /// `_db.exec()`, which applies side effects and returns the `RETURNING`
    /// rows in the same call.
    ///
    /// Like [`run_execute`](Self::run_execute) and every other `DbExec`
    /// primitive here, it deliberately does NOT flush to OPFS — flushing is
    /// coalesced once per logical `DatabaseService` call by
    /// [`BrowserDatabaseService::with_flush`], and the shared defaults that
    /// reach this primitive ([`DbExec::create`], [`DbExec::take_where`],
    /// [`DbExec::upsert`]) are `custom` entries in the ledger below precisely
    /// so they get that flush.
    /// See the module doc comment's durability contract.
    async fn run_execute_returning(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError> {
        self.run_fetch(sql, params, json).await
    }

    async fn run_scalar_i64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        Ok(scalar_i64(self.query_first_json_row(sql, params)?))
    }

    async fn run_scalar_f64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<f64, DatabaseError> {
        Ok(scalar_f64(self.query_first_json_row(sql, params)?))
    }

    async fn dbx_table_exists(&self, table: &str) -> Result<bool, DatabaseError> {
        let (sql, params) = introspect::build_table_exists(table, Backend::Sqlite);
        Ok(self.run_scalar_i64(&sql, &params).await? > 0)
    }

    /// `ops` between `BEGIN` and `COMMIT` on the one sql.js database, rolled
    /// back on the first failure — see [`in_transaction`]. Like every other
    /// primitive here it does not flush; the `DatabaseService` method that
    /// called it does, once.
    async fn run_transaction(&self, ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, DatabaseError> {
        in_transaction(bridge_control, || {
            ops.iter().map(|op| self.run_tx_op(op)).collect()
        })
    }
}

impl BrowserDatabaseService {
    /// One statement of a [`DbExec::run_transaction`], run through the bridge.
    fn run_tx_op(&self, op: &TxOp<'_>) -> Result<TxResult, DatabaseError> {
        match *op {
            TxOp::Execute { sql, params } => {
                let params_js = db_codec::params_to_js(params).map_err(DatabaseError::Internal)?;
                let rows = bridge::db_exec_raw(sql, params_js).map_err(|e| statement_failed(&e))?;
                Ok(TxResult::Execute(rows as i64))
            }
            TxOp::Returning { sql, params, json } => Ok(TxResult::Returning(
                self.query_json_rows(sql, params)?
                    .into_iter()
                    .map(|row| record_from_json_row(row, json))
                    .collect(),
            )),
        }
    }
}

/// A statement sql.js refused, as a [`DatabaseError`]: its text classified by
/// [`impresspress_core::sqlite_text_error::statement_error`], so a taken key is
/// `AlreadyExists`.
fn statement_failed(e: &wasm_bindgen::JsValue) -> DatabaseError {
    impresspress_core::sqlite_text_error::statement_error(format!("sql exec: {e:?}"))
}

/// The error type of a caller of [`in_transaction`]: how it spells the two
/// failures the framing itself can report.
pub(crate) trait TxError: std::fmt::Display {
    /// SQLite refused a `BEGIN` or `COMMIT`; `message` is its text.
    fn refused(message: String) -> Self;
    /// A failed transaction could not be rolled back, so the connection is
    /// still inside it. Never a caller's mistake, whatever the failure that
    /// started it was.
    fn stuck(message: String) -> Self;
}

impl TxError for DatabaseError {
    /// Classified like any other refused statement, so a busy database stays
    /// `Unavailable`.
    fn refused(message: String) -> Self {
        impresspress_core::sqlite_text_error::statement_error(format!("sql exec: {message}"))
    }

    fn stuck(message: String) -> Self {
        DatabaseError::Internal(message)
    }
}

/// Run one transaction-control statement (`BEGIN`, `COMMIT`, `ROLLBACK`) on
/// the one sql.js connection, answering with SQLite's text when it is refused
/// — the text [`in_transaction`] reads the connection's state from.
pub(crate) fn bridge_control(sql: &str) -> Result<(), String> {
    bridge::db_exec_raw(sql, db_codec::empty_params())
        .map(|_| ())
        .map_err(|e| bridge::describe(&e))
}

/// SQLite's refusal of a `BEGIN` on a connection that is already inside a
/// transaction.
const NESTED_BEGIN: &str = "cannot start a transaction within a transaction";

/// SQLite's refusal of a `ROLLBACK` on a connection that is not inside a
/// transaction — which, after a failed statement, means SQLite has already
/// rolled the transaction back itself (it does for `SQLITE_FULL`,
/// `SQLITE_NOMEM`, `SQLITE_IOERR` and an interrupt).
const NO_ACTIVE_TRANSACTION: &str = "no transaction is active";

/// Run `body` as one transaction on the one sql.js connection: `BEGIN`,
/// `body`, `COMMIT` — and on any failure `ROLLBACK` and that failure, so
/// either everything `body` ran is applied or none of it is. Every
/// transaction in this crate goes through here: this module's
/// [`DbExec::run_transaction`] and `vector::service`'s index rename.
///
/// `control` runs `BEGIN`/`COMMIT`/`ROLLBACK` and answers with SQLite's text
/// when one is refused ([`bridge_control`] in production); [`TxError`] turns
/// that text into the caller's error type. `control` is a parameter so the
/// framing can be driven by a recording runner (`transaction_framing`) and a
/// `ROLLBACK` failure can be injected in front of real sql.js
/// (`sql_js_transactions`), which has no other way to refuse one.
///
/// Everything is synchronous, and that is what makes this a transaction on a
/// database other code shares: every bridge call is synchronous, so nothing
/// between `BEGIN` and `COMMIT` yields to the executor, and no other task's
/// statement can land inside it.
///
/// A connection left inside a transaction is the failure this guards against
/// from both ends, because sql.js does not surface it any other way: every
/// later statement would run inside that transaction, and the next flush —
/// `db.export()` closes and reopens the connection — would silently roll back
/// what those statements wrote, although each of them reported success.
///
/// - **At `BEGIN`**, the `BEGIN` itself is the probe: SQLite refuses it with
///   [`NESTED_BEGIN`] when a transaction is already open. Every logical write
///   starts on a clean connection ([`with_flush_mapped`] sees to it), so one
///   open here was opened during this write, and its earlier statements — a
///   lazy `ADD COLUMN` before a `create_many`'s inserts, say — ran inside it.
///   It cannot be committed (nobody framed those statements) or kept (the
///   flush would roll it back), so it is rolled back and the write fails,
///   saying why.
/// - **After a failed `ROLLBACK`**: [`NO_ACTIVE_TRANSACTION`] means SQLite
///   already ended the transaction, and the connection is clean. Any other
///   refusal is retried once; if that fails too the connection is still
///   inside the transaction, and the caller gets an error saying so, carrying
///   the original failure, rather than the original failure alone. The check
///   before the flush that follows ([`flush_through_bridge`]) then ends it.
pub(crate) fn in_transaction<T, E: TxError>(
    mut control: impl FnMut(&str) -> Result<(), String>,
    body: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    begin(&mut control).map_err(E::refused)?;
    let outcome = body().and_then(|value| control("COMMIT").map(|()| value).map_err(E::refused));
    let failure = match outcome {
        Ok(value) => return Ok(value),
        Err(failure) => failure,
    };
    match rollback(&mut control) {
        Ok(()) => Err(failure),
        Err(stuck) => {
            tracing::error!(
                error = %failure,
                rollback = %stuck,
                "a failed transaction could not be rolled back; the sql.js connection is \
                 still inside it"
            );
            Err(E::stuck(format!(
                "{failure}; ROLLBACK failed twice ({stuck}), so the connection is still inside \
                 the failed transaction"
            )))
        }
    }
}

/// `BEGIN` — refused, after rolling back, when a transaction is already
/// open. See [`in_transaction`].
fn begin(control: &mut impl FnMut(&str) -> Result<(), String>) -> Result<(), String> {
    match control("BEGIN") {
        Err(refused) if refused.contains(NESTED_BEGIN) => {
            let rolled_back = match control("ROLLBACK") {
                Ok(()) => "rolled it back".to_string(),
                Err(e) => format!("could not roll it back either: {e}"),
            };
            forget_schema();
            tracing::error!(
                %rolled_back,
                "a transaction was already open on the sql.js connection at BEGIN"
            );
            Err(format!(
                "{refused}: a transaction was already open on the sql.js connection, so this \
                 write's statements before BEGIN ran inside it; {rolled_back}"
            ))
        }
        other => other,
    }
}

/// `ROLLBACK`, retried once — `Err` only when the connection is still inside
/// the transaction after both attempts. See [`in_transaction`].
fn rollback(control: &mut impl FnMut(&str) -> Result<(), String>) -> Result<(), String> {
    let first = match control("ROLLBACK") {
        Ok(()) => return Ok(()),
        Err(refused) if refused.contains(NO_ACTIVE_TRANSACTION) => {
            tracing::debug!(error = %refused, "SQLite already rolled the failed transaction back");
            return Ok(());
        }
        Err(refused) => refused,
    };
    tracing::warn!(error = %first, "ROLLBACK after a failed transaction failed; retrying it");
    match control("ROLLBACK") {
        Ok(()) => Ok(()),
        Err(refused) if refused.contains(NO_ACTIVE_TRANSACTION) => Ok(()),
        Err(refused) => Err(format!("{first}; then {refused}")),
    }
}

// ─── DatabaseService — an explicit ledger over the shared DbExec defaults ─────
//
// Written with `forward_database_service!` rather than by hand. The macro's
// `ops { … }` block names EVERY operation on the trait and refuses to expand
// if one is missing, so the twenty-seven lines below are a ledger of what this
// backend does with each: `forward` = the shared `DbExec` default, `custom` =
// written here, `inherit` = deliberately the `DatabaseService` trait default.
// Eight of those trait defaults are not pass-throughs (`take_where` is a
// list-then-delete loop instead of `DELETE … RETURNING`, `set_strict_schema`
// is a silent no-op, …), and a backend that omits one does not get "the same
// behaviour" — it gets a different, worse one, invisibly. That is the bug the
// ledger makes unrepresentable.
//
// The `custom` entries here are all the same thing: every method that can
// mutate the sql.js database wraps its `DbExec` default in `with_flush`, so
// exactly one OPFS flush happens per logical call however many `run_execute`
// statements the shared default issued internally. `take_where` is a mutator
// (`DELETE … RETURNING`) despite its read-shaped return, so it is flushed too.
// `set_strict_schema` is custom because `DbExec` has no such operation to
// forward to — it is the setter behind `DbExec::strict_schema`.
wafer_core::forward_database_service! {
    impl DatabaseService for BrowserDatabaseService {
        forward_to DbExec;

        ops {
            get: forward,
            list: forward,
            create: custom,
            create_many: custom,
            update: custom,
            delete: custom,
            count: forward,
            sum: forward,
            query_raw: forward,
            exec_raw: custom,
            delete_where: custom,
            delete_where_count: custom,
            take_where: custom,
            update_where: custom,
            update_where_count: custom,
            increment_field_where: custom,
            upsert: custom,
            aggregate: forward,
            batch: custom,
            insert_guarded: custom,
            update_guarded: custom,
            ensure_schema_table: custom,
            ensure_schema_tables: inherit,
            schema_table_exists: forward,
            schema_columns: forward,
            schema_drop_table: custom,
            schema_add_column: custom,
            set_strict_schema: custom,
            statement_budget: forward,
        }

        async fn create(
            &self,
            collection: &str,
            data: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            self.with_flush(DbExec::create(self, collection, data))
                .await
        }

        async fn update(
            &self,
            collection: &str,
            id: &str,
            data: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            self.with_flush(DbExec::update(self, collection, id, data))
                .await
        }

        async fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError> {
            self.with_flush(DbExec::delete(self, collection, id)).await
        }

        async fn exec_raw(
            &self,
            query: &str,
            args: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            self.with_flush(DbExec::exec_raw(self, query, args)).await
        }

        async fn delete_where(
            &self,
            collection: &str,
            filters: &[Filter],
        ) -> Result<(), DatabaseError> {
            self.with_flush(DbExec::delete_where(self, collection, filters))
                .await
        }

        async fn delete_where_count(
            &self,
            collection: &str,
            filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            self.with_flush(DbExec::delete_where_count(self, collection, filters))
                .await
        }

        async fn take_where(
            &self,
            collection: &str,
            filters: &[Filter],
        ) -> Result<Vec<Record>, DatabaseError> {
            self.with_flush(DbExec::take_where(self, collection, filters))
                .await
        }

        async fn update_where(
            &self,
            collection: &str,
            filters: &[Filter],
            data: HashMap<String, serde_json::Value>,
        ) -> Result<(), DatabaseError> {
            self.with_flush(DbExec::update_where(self, collection, filters, data))
                .await
        }

        async fn update_where_count(
            &self,
            collection: &str,
            filters: &[Filter],
            data: HashMap<String, serde_json::Value>,
        ) -> Result<i64, DatabaseError> {
            self.with_flush(DbExec::update_where_count(self, collection, filters, data))
                .await
        }

        async fn increment_field_where(
            &self,
            collection: &str,
            col: &str,
            delta: i64,
            filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            self.with_flush(DbExec::increment_field_where(
                self, collection, col, delta, filters,
            ))
            .await
        }

        async fn upsert(&self, collection: &str, spec: UpsertSpec) -> Result<Option<Record>, DatabaseError> {
            self.with_flush(DbExec::upsert(self, collection, spec))
                .await
        }

        /// One transaction, one flush, however many rows.
        async fn create_many(
            &self,
            collection: &str,
            rows: Vec<HashMap<String, serde_json::Value>>,
        ) -> Result<i64, DatabaseError> {
            self.with_flush(DbExec::create_many(self, collection, rows))
                .await
        }

        /// One transaction, one flush, however many ops.
        async fn batch(&self, ops: Vec<WriteOp>) -> Result<Vec<WriteOutcome>, DatabaseError> {
            self.with_flush(DbExec::batch(self, ops)).await
        }

        async fn insert_guarded(
            &self,
            collection: &str,
            data: HashMap<String, serde_json::Value>,
            guards: &[CapGuard],
        ) -> Result<GuardedInsert, DatabaseError> {
            self.with_flush(DbExec::insert_guarded(self, collection, data, guards))
                .await
        }

        async fn update_guarded(
            &self,
            collection: &str,
            filters: &[Filter],
            data: HashMap<String, serde_json::Value>,
            guards: &[CapGuard],
        ) -> Result<GuardedUpdate, DatabaseError> {
            self.with_flush(DbExec::update_guarded(
                self, collection, filters, data, guards,
            ))
            .await
        }

        /// The DDL sequence itself is [`DbExec::ensure_schema_table`] — the
        /// shared default this file used to carry a copy of. The copy had
        /// drifted in one way that mattered and one that did not: it built the
        /// same CREATE / add-missing-columns / indexes / FK-indexes sequence,
        /// but it hard-coded `Backend::Sqlite` instead of reading
        /// `Self::BACKEND`, and it did not invalidate the schema cache on the
        /// error path. The shared default invalidates [`SCHEMA_CACHE`] on
        /// both paths.
        ///
        /// What stays browser-specific is the one flush: the whole sequence is
        /// several `run_execute` calls and they share a single write to OPFS.
        async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
            self.with_flush(DbExec::ensure_schema_table(self, table))
                .await
        }

        /// DDL through `run_execute` rather than a shared default, so the
        /// cached facts for `name` are dropped here, whatever the statement
        /// returned.
        async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
            self.with_flush(async {
                let stmt = wafer_sql_utils::ddl::build_drop_table(name, Self::BACKEND)?;
                let dropped = self.run_execute(&stmt.sql, &[]).await;
                SCHEMA_CACHE.invalidate(name);
                dropped.map(|_| ())
            })
            .await
        }

        async fn schema_add_column(
            &self,
            table: &str,
            column: &Column,
        ) -> Result<(), DatabaseError> {
            self.with_flush(async {
                let stmt = wafer_sql_utils::ddl::build_add_column(table, column, Self::BACKEND)?;
                let added = self.run_execute(&stmt.sql, &[]).await;
                SCHEMA_CACHE.invalidate(table);
                added.map(|_| ())
            })
            .await
        }

        /// Record the resolved STRICT_SCHEMA verdict so [`DbExec::strict_schema`]
        /// can read it. The trait default is a silent no-op, which is the
        /// wrong answer for a backend that DOES run through `DbExec`: the
        /// shared `DatabaseBlock` advertises
        /// `WAFER_RUN__DATABASE__STRICT_SCHEMA` as a config key and applies it
        /// at `Init` on every backend, so inheriting the no-op meant this
        /// target offered an operator a switch that did nothing.
        fn set_strict_schema(&self, enabled: bool) {
            STRICT_SCHEMA.store(enabled, Ordering::Relaxed);
        }
    }
}

/// Factory: returns an `Arc<dyn DatabaseService>` backed by the
/// browser's sql.js + OPFS integration. Call after `crate::db_init()`
/// has completed.
pub fn make_database_service() -> std::sync::Arc<dyn DatabaseService> {
    std::sync::Arc::new(BrowserDatabaseService)
}

// ─── Shared DatabaseService conformance wiring ────────────────────────────────
//
// wafer-run #319 ships a backend-agnostic conformance suite
// (`wafer_core::interfaces::database::conformance::run_conformance`) that drives
// every `DatabaseService` op against a live service and asserts the concrete
// observable behavior (CRUD round-trips, the full `FilterOp` surface, sorted /
// paginated / projected / OR-grouped `list`, atomic `increment_field_where`,
// insert-then-update and windowed-counter `upsert`, grouped aggregates, raw
// SQL, schema management). It is the anti-drift mechanism: a `DatabaseService`
// impl that silently no-ops or fails-open on any single op fails an assertion
// instead of passing silently. Native SQLite and (gated) PostgreSQL already run
// it inside wafer-run; this module wires the browser adapter in.
//
// ── Coverage achieved here: COMPILE-TIME conformance (no live run) ──
//
// `_browser_adapter_is_conformable` typechecks — for the real, shipping
// `wasm32-unknown-unknown` target — that `BrowserDatabaseService` satisfies the
// exact `DatabaseService` surface `run_conformance` drives, and that the suite
// entry point exists under the enabled `conformance` feature. It is compiled by
// an unconditional wasm32 CI step —
// `cargo check --tests -p impresspress-browser --target wasm32-unknown-unknown`,
// in `.github/workflows/ci-shared.yml`, the one body both gates run. The
// `--tests` flag is required: this
// `#[cfg(all(test, target_arch = "wasm32"))]` module needs the wasm32 dev-dep
// `conformance` feature, which a plain `cargo check` (no `--tests`) does not
// activate — so the plain check does NOT compile it. The dedicated
// `wasm-pack test --node crates/impresspress-browser` job also builds it, but on
// a pull request it is gated on a diff touching this crate, `impresspress-core`,
// `Cargo.toml` or `Cargo.lock` (a merge runs it unconditionally), so on a pull
// request that changes none of those the unconditional step above is what
// catches trait-surface drift. A wafer-run pin bump does reach the gated job,
// via `Cargo.lock`. If the trait surface drifts (a new required op, or a changed
// signature) or the suite entry is removed/renamed/re-gated, this stops
// compiling — surfacing the drift at the consumer rather than only inside
// wafer-run. The assertion is never
// executed and constructs no trait object, so it makes no sql.js/OPFS bridge
// call.
//
// ── What runs live, and what only typechecks ──
//
// `sql_js_conformance` below drives this adapter against REAL sql.js under
// `wasm-pack test --node`: `js/test/node-hooks.mjs` resolves bridge.js's static
// `/vendor/sql-wasm-esm.js` import to the vendored build in
// `crates/impresspress-bundle/assets/vendor/`, and the test installs an
// in-memory stand-in for the OPFS directory `dbInit` reads and `dbFlush`
// writes. It pins the unique-key classification (`AlreadyExists`) that
// `impresspress_core::blocks::crud`'s 409 rests on. The full
// `run_conformance(&BrowserDatabaseService).await` is not run live; this module
// only typechecks it, and the same two pieces are what a live run would use.
#[cfg(all(test, target_arch = "wasm32"))]
mod conformance {
    use wafer_core::interfaces::database::{
        conformance::run_conformance, service::DatabaseService,
    };

    use super::BrowserDatabaseService;

    /// Compile-time proof (never executed) that the browser adapter is a valid
    /// argument to the shared conformance suite. Typechecking the call — with
    /// the `&BrowserDatabaseService` → `&dyn DatabaseService` coercion
    /// `run_conformance` requires — is what enforces the trait-surface
    /// conformance; awaiting it here would run the whole suite live, which the
    /// note above says this crate does not do.
    async fn _browser_adapter_is_conformable(svc: &BrowserDatabaseService) {
        run_conformance(svc as &dyn DatabaseService).await;
    }
}

/// **The `DatabaseService` contract on real sql.js.** A write that duplicates
/// a primary or unique key is `AlreadyExists` — the 409
/// `impresspress_core::blocks::crud` answers, without re-reading the key —
/// and any other refused write is `Internal`. sql.js reports every refusal as
/// a JS exception carrying only SQLite's text, so this is the one place the
/// classification can be seen working: the exception comes from the vendored
/// sql.js build the site serves, through `bridge.js`, into the adapter's own
/// `create`.
///
/// Under `wasm-pack test --node`, `js/test/node-hooks.mjs` points bridge.js's
/// `/vendor/sql-wasm-esm.js` import at that vendored build, and
/// [`install_memory_opfs`] stands in for the OPFS directory `dbInit` reads
/// and `dbFlush` writes.
#[cfg(all(test, target_arch = "wasm32"))]
mod sql_js_conformance {
    use std::collections::HashMap;

    use wafer_core::interfaces::database::service::{DatabaseError, DatabaseService};
    use wasm_bindgen::prelude::wasm_bindgen;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::BrowserDatabaseService;

    #[wasm_bindgen(inline_js = r#"
let writes = 0;
export function opfsWrites() { return writes; }
export function installMemoryOpfs() {
    const files = new Map();
    writes = 0;
    const handle = (name) => ({
        async getFile() {
            const data = files.get(name);
            return { async arrayBuffer() { return data.slice().buffer; } };
        },
        async createWritable() {
            let data = new Uint8Array(0);
            return {
                async write(chunk) { data = chunk; },
                async close() { files.set(name, data); writes += 1; },
            };
        },
    });
    const root = {
        async getFileHandle(name, options) {
            if (!files.has(name)) {
                if (!(options && options.create)) {
                    throw new DOMException('no such file', 'NotFoundError');
                }
                files.set(name, new Uint8Array(0));
            }
            return handle(name);
        },
    };
    Object.defineProperty(globalThis.navigator, 'storage', {
        configurable: true,
        value: { async getDirectory() { return root; } },
    });
}
"#)]
    extern "C" {
        /// An in-memory OPFS: `navigator.storage.getDirectory()` answering
        /// the file-handle calls bridge.js makes, starting empty.
        #[wasm_bindgen(js_name = installMemoryOpfs)]
        pub(super) fn install_memory_opfs();

        /// How many times a file was written to the OPFS installed last —
        /// one per `dbFlush`.
        #[wasm_bindgen(js_name = opfsWrites)]
        pub(super) fn opfs_writes() -> u32;
    }

    fn row(id: &str, name: Option<&str>) -> HashMap<String, serde_json::Value> {
        let mut row = HashMap::from([("id".to_string(), serde_json::json!(id))]);
        if let Some(name) = name {
            row.insert("name".to_string(), serde_json::json!(name));
        }
        row
    }

    #[wasm_bindgen_test]
    async fn a_duplicate_insert_is_already_exists() {
        install_memory_opfs();
        crate::bridge::dbInit().await.expect("sql.js loads");
        let svc = BrowserDatabaseService;
        svc.exec_raw(
            "CREATE TABLE sql_js_dup_t (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE)",
            &[],
        )
        .await
        .expect("create table");

        svc.create("sql_js_dup_t", row("a", Some("first")))
            .await
            .expect("the first row lands");
        for (what, taken) in [
            ("primary key", row("a", Some("second"))),
            ("unique column", row("b", Some("first"))),
        ] {
            let err = svc.create("sql_js_dup_t", taken).await.expect_err(what);
            assert!(
                matches!(err, DatabaseError::AlreadyExists(_)),
                "{what}: {err:?}"
            );
        }
        let err = svc
            .create("sql_js_dup_t", row("c", None))
            .await
            .expect_err("NOT NULL");
        assert!(matches!(err, DatabaseError::Internal(_)), "{err:?}");
    }
}

/// The row-decode policy this adapter now shares with native SQLite and
/// Cloudflare D1. `database.rs` is wasm32-only, so these run under
/// `wasm-pack test --node`; the codec itself is pure and needs no bridge.
#[cfg(all(test, target_arch = "wasm32"))]
mod codec_policy {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{record_from_json_row, scalar_f64, scalar_i64, JsonColumns};

    /// sql.js stores JSON columns as TEXT; the shared codec restores the
    /// structure the writer put in for a column declared JSON, so a block
    /// reading it sees a `Value::Object` on all three adapters.
    #[wasm_bindgen_test]
    fn json_declared_columns_are_reparsed() {
        let rec = record_from_json_row(
            serde_json::json!({"id": "1", "meta": "{\"k\":\"v\"}", "tags": "[1,2]"}),
            &JsonColumns::new(["meta", "tags"]),
        );
        assert_eq!(rec.id, "1");
        assert_eq!(rec.data.get("meta").unwrap(), &serde_json::json!({"k":"v"}));
        assert_eq!(rec.data.get("tags").unwrap(), &serde_json::json!([1, 2]));
    }

    /// Text in a column not declared JSON stays the string that was written,
    /// however much it looks like JSON.
    #[wasm_bindgen_test]
    fn json_looking_text_in_a_text_column_stays_a_string() {
        let rec = record_from_json_row(
            serde_json::json!({"id": "1", "title": "{\"k\":\"v\"}", "tags": "[1,2]"}),
            JsonColumns::NONE,
        );
        assert_eq!(
            rec.data.get("title").unwrap(),
            &serde_json::json!("{\"k\":\"v\"}")
        );
        assert_eq!(rec.data.get("tags").unwrap(), &serde_json::json!("[1,2]"));
    }

    /// A plain string that does not look like JSON stays a string, and one
    /// that does not parse stays a string even in a JSON column.
    #[wasm_bindgen_test]
    fn non_json_text_is_left_alone() {
        let rec = record_from_json_row(
            serde_json::json!({"id": "1", "note": "hello world", "broken": "{not json"}),
            &JsonColumns::new(["broken"]),
        );
        assert_eq!(
            rec.data.get("note").unwrap(),
            &serde_json::json!("hello world")
        );
        assert_eq!(
            rec.data.get("broken").unwrap(),
            &serde_json::json!("{not json")
        );
    }

    /// An integer primary key is stringified into `Record::id` and kept in
    /// `data` — the row decoders in block repositories read the whole column
    /// map.
    #[wasm_bindgen_test]
    fn numeric_id_is_stringified_and_retained() {
        let rec = record_from_json_row(serde_json::json!({"id": 7, "v": "x"}), JsonColumns::NONE);
        assert_eq!(rec.id, "7");
        assert_eq!(rec.data.get("id").unwrap(), &serde_json::json!(7));
    }

    /// The one behaviour the private copy did differently: a non-object row.
    /// `db_codec::build_records` returned `Err("expected row object")`, which
    /// failed the whole query on one platform for a shape the other two
    /// report as an empty record. See `run_fetch`'s doc for why the shared
    /// answer wins. This fails against the pre-unification tree.
    #[wasm_bindgen_test]
    fn a_non_object_row_is_an_empty_record_not_an_error() {
        let rec = record_from_json_row(serde_json::json!(42), JsonColumns::NONE);
        assert_eq!(rec.id, "");
        assert!(rec.data.is_empty());
    }

    /// `SELECT COUNT(*) AS "cnt"` — the shared builders alias their scalar
    /// column themselves, so the scalar accessors must not look it up by name.
    #[wasm_bindgen_test]
    fn scalars_read_the_aliased_aggregate_column() {
        assert_eq!(scalar_i64(Some(serde_json::json!({"cnt": 5}))), 5);
        assert!((scalar_f64(Some(serde_json::json!({"total": 12.5}))) - 12.5).abs() < f64::EPSILON);
    }

    /// An absent row counts as zero — the same answer the SQL aggregate gives
    /// for an empty table, so "no row" can never be mistaken for a count.
    #[wasm_bindgen_test]
    fn an_absent_scalar_row_is_zero() {
        assert_eq!(scalar_i64(None), 0);
        assert!(scalar_f64(None).abs() < f64::EPSILON);
    }
}

/// STRICT_SCHEMA is applied, not silently dropped. `database.rs` is
/// wasm32-only, so these run under `wasm-pack test --node`; they read and
/// write the flag and touch no bridge.
#[cfg(all(test, target_arch = "wasm32"))]
mod strict_schema_policy {
    use wafer_core::interfaces::database::{exec::DbExec, service::DatabaseService};
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::BrowserDatabaseService;

    /// **Fails on the pre-change tree**, where `set_strict_schema` was the
    /// trait's silent no-op default and `DbExec::strict_schema` therefore
    /// always answered `false`. `DatabaseBlock`'s `Init` reads
    /// `WAFER_RUN__DATABASE__STRICT_SCHEMA` and calls the setter on every
    /// backend, so the browser advertised the config key (through the shared
    /// block's `config_keys`) and then ignored whatever an operator set.
    #[wasm_bindgen_test]
    fn setting_strict_schema_is_observed_by_the_shared_executor() {
        let svc = BrowserDatabaseService;
        // The default the shared executor starts from.
        assert!(!DbExec::strict_schema(&svc));

        DatabaseService::set_strict_schema(&svc, true);
        assert!(
            DbExec::strict_schema(&svc),
            "the shared orchestration must see the flag, or the table-exists \
             probe and the lazy ADD COLUMN path stay on the hot path"
        );

        // A second handle sees it too: the service is a unit struct over one
        // global sql.js database, so the flag is a property of that database
        // and not of a handle. `impresspress-web` holds a second handle for
        // its boot hook.
        assert!(DbExec::strict_schema(&BrowserDatabaseService));

        DatabaseService::set_strict_schema(&svc, false);
        assert!(!DbExec::strict_schema(&svc));
    }
}

/// The schema cache the shared executor memoizes introspection in. No bridge:
/// the facts are written straight into the cache the executor reads.
#[cfg(all(test, target_arch = "wasm32"))]
mod schema_cache_policy {
    use wafer_core::interfaces::database::exec::DbExec;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{forget_schema, BrowserDatabaseService};

    /// **Fails without the cache**, where `DbExec::schema_cache` was the
    /// trait's `None` default: every sorted or paged `list` then read the
    /// table's primary key through the bridge before its select. Two handles
    /// share one cache, because they address one database, and
    /// `forget_schema` empties it for the schema changes the shared executor
    /// does not see.
    #[wasm_bindgen_test]
    fn every_handle_shares_one_cache_and_forget_schema_empties_it() {
        let runtime = BrowserDatabaseService;
        let boot_hook = BrowserDatabaseService;
        let cache = DbExec::schema_cache(&runtime).expect("the browser backend keeps a cache");
        let other = DbExec::schema_cache(&boot_hook).expect("so does every other handle");
        assert!(std::ptr::eq(cache, other), "one database, one cache");

        cache.set_primary_key_if_gen("cache_policy_t", vec!["id".into()], cache.generation());
        assert_eq!(
            other.primary_key("cache_policy_t"),
            Some(vec!["id".to_string()]),
            "a key one handle learned is served to the other"
        );

        forget_schema();
        assert_eq!(other.primary_key("cache_policy_t"), None);
    }
}

/// The invalidation half of the cache: every schema change the shared executor
/// does not make has to drop what the cache knows about the table it changed.
///
/// Each of these calls a bridge function, which under `wasm-pack test --node`
/// rejects until `sql_js_conformance` has loaded sql.js and may run after —
/// and either is the point: the invalidation has to happen whatever the
/// statement returned, because a failed DDL may still have applied. The test
/// seeds a fact, calls the real method, and asserts the fact is gone however
/// the call ended.
#[cfg(all(test, target_arch = "wasm32"))]
mod schema_invalidation {
    use wafer_core::interfaces::database::{
        exec::DbExec,
        service::{Column, DatabaseService},
    };
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::BrowserDatabaseService;

    /// Seed a primary key for `table` and hand back the cache it went into.
    fn seed(table: &str) -> &'static wafer_core::interfaces::database::schema_cache::SchemaCache {
        let cache = DbExec::schema_cache(&BrowserDatabaseService).expect("a cache");
        cache.set_primary_key_if_gen(table, vec!["id".into()], cache.generation());
        assert_eq!(cache.primary_key(table), Some(vec!["id".to_string()]));
        cache
    }

    #[wasm_bindgen_test]
    async fn dropping_a_table_forgets_it() {
        let cache = seed("invalidate_drop_t");
        let _ =
            DatabaseService::schema_drop_table(&BrowserDatabaseService, "invalidate_drop_t").await;
        assert_eq!(cache.primary_key("invalidate_drop_t"), None);
    }

    #[wasm_bindgen_test]
    async fn adding_a_column_forgets_its_table() {
        let cache = seed("invalidate_add_t");
        let _ = DatabaseService::schema_add_column(
            &BrowserDatabaseService,
            "invalidate_add_t",
            &Column::new(
                "extra",
                wafer_core::interfaces::database::service::DataType::Text,
            ),
        )
        .await;
        assert_eq!(cache.primary_key("invalidate_add_t"), None);
    }

    /// Reopening the database replaces everything the cache described, so it
    /// drops the lot rather than one table.
    #[wasm_bindgen_test]
    async fn reopening_the_database_forgets_everything() {
        let cache = seed("invalidate_init_t");
        let _ = crate::db_init().await;
        assert_eq!(cache.primary_key("invalidate_init_t"), None);
    }
}

/// The flush precedence every mutating path in this crate shares, and the
/// unconditional flush underneath it. No bridge, no OPFS.
#[cfg(all(test, target_arch = "wasm32"))]
mod flush_precedence {
    use std::{cell::Cell, rc::Rc};

    use wafer_core::interfaces::database::service::DatabaseError;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{resolve_flush_outcome, with_flush_through};

    /// A FAILED operation must still flush. The headline of the durability
    /// change was exactly this: four vector-service sites ran the flush with
    /// `?`, so a failed mutation skipped it and left whatever statements had
    /// already applied (a lazy column-add ALTER before a rejected INSERT) in
    /// memory only, until some unrelated later mutation happened to write them
    /// out. `resolve_flush_outcome` cannot see this — it is handed both
    /// outcomes — so the assertion has to be on the wrapper.
    #[wasm_bindgen_test]
    async fn a_failed_operation_still_flushes() {
        let flushes = Rc::new(Cell::new(0u32));
        let counter = flushes.clone();

        let out: Result<u8, DatabaseError> = with_flush_through(
            async { Err(DatabaseError::NotFound) },
            move || {
                counter.set(counter.get() + 1);
                async { Ok(()) }
            },
            DatabaseError::Internal,
        )
        .await;

        assert_eq!(
            flushes.get(),
            1,
            "the flush was skipped because the operation failed"
        );
        assert!(matches!(out, Err(DatabaseError::NotFound)));
    }

    /// …and a successful one flushes exactly once, not once per statement the
    /// operation ran. That coalescing is the other half of the contract.
    #[wasm_bindgen_test]
    async fn a_successful_operation_flushes_exactly_once() {
        let flushes = Rc::new(Cell::new(0u32));
        let counter = flushes.clone();

        let out: Result<u8, DatabaseError> = with_flush_through(
            async { Ok(7) },
            move || {
                counter.set(counter.get() + 1);
                async { Ok(()) }
            },
            DatabaseError::Internal,
        )
        .await;

        assert_eq!(flushes.get(), 1);
        assert_eq!(out.expect("ok"), 7);
    }

    /// The flush's own failure reaches the caller through `map_flush`, in the
    /// caller's error type — the only thing that ever differed between this
    /// helper's two callers.
    #[wasm_bindgen_test]
    async fn a_flush_failure_is_mapped_into_the_callers_error_type() {
        let out: Result<u8, DatabaseError> = with_flush_through(
            async { Ok(7) },
            || async { Err("quota exceeded".to_string()) },
            DatabaseError::Internal,
        )
        .await;

        match out {
            Err(DatabaseError::Internal(msg)) => assert_eq!(msg, "quota exceeded"),
            other => panic!("expected the mapped flush error, got {other:?}"),
        }
    }

    #[wasm_bindgen_test]
    fn a_durable_success_is_a_success() {
        let out: Result<u8, DatabaseError> = resolve_flush_outcome(Ok(7), Ok(()));
        assert_eq!(out.expect("ok"), 7);
    }

    /// A mutation that only reached memory must not be reported as done: a
    /// Service Worker eviction would lose it.
    #[wasm_bindgen_test]
    fn a_failed_flush_beats_a_successful_operation() {
        let out: Result<u8, DatabaseError> =
            resolve_flush_outcome(Ok(7), Err(DatabaseError::Internal("quota".into())));
        match out {
            Err(DatabaseError::Internal(msg)) => assert_eq!(msg, "quota"),
            other => panic!("expected the flush error, got {other:?}"),
        }
    }

    /// …but the operation's own error is the more actionable of the two, so it
    /// wins even when the flush also failed.
    #[wasm_bindgen_test]
    fn the_operations_error_beats_the_flushs() {
        let out: Result<u8, DatabaseError> = resolve_flush_outcome(
            Err(DatabaseError::NotFound),
            Err(DatabaseError::Internal("quota".into())),
        );
        assert!(matches!(out, Err(DatabaseError::NotFound)));
    }

    /// The same three answers for the vector service's error type — the point
    /// of sharing the helper is that the two callers cannot drift.
    #[wasm_bindgen_test]
    fn the_vector_services_error_type_gets_the_same_precedence() {
        use wafer_core::interfaces::vector::service::VectorError;

        let ok: Result<(), VectorError> = resolve_flush_outcome(Ok(()), Ok(()));
        assert!(ok.is_ok());

        let flush_failed: Result<(), VectorError> =
            resolve_flush_outcome(Ok(()), Err(VectorError::Internal("quota".into())));
        assert!(matches!(flush_failed, Err(VectorError::Internal(_))));

        let both_failed: Result<(), VectorError> = resolve_flush_outcome(
            Err(VectorError::IndexNotFound("idx".into())),
            Err(VectorError::Internal("quota".into())),
        );
        assert!(matches!(both_failed, Err(VectorError::IndexNotFound(_))));
    }
}

/// The `BEGIN`/`COMMIT`/`ROLLBACK` framing of [`in_transaction`], driven
/// through a recording stand-in for the connection that keeps SQLite's one
/// piece of state — whether a transaction is open — and refuses the control
/// statements with SQLite's own texts. `sql_js_transactions` below runs the
/// cases real sql.js can produce against real sql.js.
#[cfg(all(test, target_arch = "wasm32"))]
mod transaction_framing {
    use std::cell::RefCell;

    use wafer_core::interfaces::database::service::DatabaseError;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{in_transaction, NESTED_BEGIN, NO_ACTIVE_TRANSACTION};

    /// A connection: whether a transaction is open, every statement it was
    /// handed, and the refusals scripted for it.
    #[derive(Default)]
    struct Connection {
        open: bool,
        seen: Vec<String>,
        /// `ROLLBACK`s to refuse, with a text that is not "no transaction".
        refuse_rollbacks: usize,
        /// Refuse the `COMMIT`.
        refuse_commit: bool,
    }

    impl Connection {
        fn control(&mut self, sql: &str) -> Result<(), String> {
            self.seen.push(sql.to_string());
            match sql {
                "BEGIN" if self.open => Err(NESTED_BEGIN.to_string()),
                "BEGIN" => {
                    self.open = true;
                    Ok(())
                }
                "COMMIT" if self.refuse_commit => Err("database is full".into()),
                "COMMIT" => {
                    self.open = false;
                    Ok(())
                }
                "ROLLBACK" if !self.open => {
                    Err(format!("cannot rollback - {NO_ACTIVE_TRANSACTION}"))
                }
                "ROLLBACK" if self.refuse_rollbacks > 0 => {
                    self.refuse_rollbacks -= 1;
                    Err("cannot rollback transaction - SQL statements in progress".into())
                }
                "ROLLBACK" => {
                    self.open = false;
                    Ok(())
                }
                other => panic!("not a control statement: {other}"),
            }
        }
    }

    /// Run a three-insert transaction whose second insert fails the way
    /// sql.js reports a taken key when `fail` is set. `ended_by_sqlite` closes
    /// the transaction as the failure happens, as SQLite does itself for
    /// `SQLITE_FULL` and friends.
    fn run(
        conn: &RefCell<Connection>,
        fail: bool,
        ended_by_sqlite: bool,
    ) -> Result<Vec<&'static str>, DatabaseError> {
        in_transaction(
            |sql| conn.borrow_mut().control(sql),
            || {
                let mut done = Vec::new();
                for insert in ["insert a", "insert b", "insert c"] {
                    conn.borrow_mut().seen.push(insert.to_string());
                    if fail && insert == "insert b" {
                        if ended_by_sqlite {
                            conn.borrow_mut().open = false;
                        }
                        return Err(impresspress_core::sqlite_text_error::statement_error(
                            "sql exec: JsValue(Error: UNIQUE constraint failed: t.id)".into(),
                        ));
                    }
                    done.push(insert);
                }
                Ok(done)
            },
        )
    }

    #[wasm_bindgen_test]
    fn every_statement_runs_between_begin_and_commit() {
        let conn = RefCell::new(Connection::default());
        assert_eq!(run(&conn, false, false).expect("committed").len(), 3);
        let conn = conn.into_inner();
        assert_eq!(
            conn.seen,
            ["BEGIN", "insert a", "insert b", "insert c", "COMMIT"]
        );
        assert!(!conn.open);
    }

    /// **The all-or-nothing half.** A failing statement rolls back the ones
    /// before it and is never followed by a `COMMIT` — without the rollback,
    /// the first insert would stay applied and the next flush would persist
    /// it. The failure reaches the caller as the taken key it was.
    #[wasm_bindgen_test]
    fn a_failing_statement_rolls_back_and_nothing_after_it_runs() {
        let conn = RefCell::new(Connection::default());
        let out = run(&conn, true, false);
        assert!(
            matches!(out, Err(DatabaseError::AlreadyExists(_))),
            "{out:?}"
        );
        let conn = conn.into_inner();
        assert_eq!(conn.seen, ["BEGIN", "insert a", "insert b", "ROLLBACK"]);
        assert!(!conn.open);
    }

    /// A `COMMIT` that fails is a failed transaction too.
    #[wasm_bindgen_test]
    fn a_failed_commit_rolls_back() {
        let conn = RefCell::new(Connection {
            refuse_commit: true,
            ..Connection::default()
        });
        assert!(run(&conn, false, false).is_err());
        let conn = conn.into_inner();
        assert_eq!(conn.seen.last().map(String::as_str), Some("ROLLBACK"));
        assert!(!conn.open);
    }

    /// SQLite ends a transaction itself on some failures, and then refuses
    /// the `ROLLBACK` as having nothing to roll back. The connection is
    /// clean, so that refusal is not retried and the caller gets the
    /// statement's own failure.
    #[wasm_bindgen_test]
    fn a_rollback_sqlite_already_did_is_not_a_failure() {
        let conn = RefCell::new(Connection::default());
        let out = run(&conn, true, true);
        assert!(
            matches!(out, Err(DatabaseError::AlreadyExists(_))),
            "{out:?}"
        );
        let conn = conn.into_inner();
        assert_eq!(conn.seen, ["BEGIN", "insert a", "insert b", "ROLLBACK"]);
    }

    /// **Fails on the pre-change tree**, which logged a refused `ROLLBACK`
    /// and returned with the connection still inside the transaction. The
    /// `ROLLBACK` is retried, and the connection ends up outside it.
    #[wasm_bindgen_test]
    fn a_refused_rollback_is_retried_until_the_transaction_is_gone() {
        let conn = RefCell::new(Connection {
            refuse_rollbacks: 1,
            ..Connection::default()
        });
        let out = run(&conn, true, false);
        assert!(
            matches!(out, Err(DatabaseError::AlreadyExists(_))),
            "{out:?}"
        );
        let conn = conn.into_inner();
        assert!(!conn.open, "the connection was left inside the transaction");
        assert_eq!(
            conn.seen,
            ["BEGIN", "insert a", "insert b", "ROLLBACK", "ROLLBACK"]
        );
    }

    /// **Fails on the pre-change tree**, which answered with the statement's
    /// own failure — a taken key, a 409 — while the connection stayed inside
    /// the transaction. A connection that cannot be rolled back is a fault,
    /// and the error says what state it left.
    #[wasm_bindgen_test]
    fn a_rollback_that_keeps_failing_is_an_internal_error_naming_the_state() {
        let conn = RefCell::new(Connection {
            refuse_rollbacks: 2,
            ..Connection::default()
        });
        match run(&conn, true, false) {
            Err(DatabaseError::Internal(msg)) => {
                assert!(msg.contains("still inside"), "{msg}");
                assert!(msg.contains("UNIQUE constraint failed"), "{msg}");
            }
            other => panic!("expected the stuck-connection error, got {other:?}"),
        }
    }

    /// **Fails on the pre-change tree**, which returned the refused `BEGIN`
    /// and left the transaction open, so whatever ran next ran inside it. A
    /// transaction already open at `BEGIN` is rolled back, and the write
    /// fails saying so rather than running its statements.
    #[wasm_bindgen_test]
    fn a_transaction_open_at_begin_is_rolled_back_and_refused() {
        let conn = RefCell::new(Connection {
            open: true,
            ..Connection::default()
        });
        match run(&conn, false, false) {
            Err(DatabaseError::Internal(msg)) => {
                assert!(msg.contains("already open"), "{msg}");
            }
            other => panic!("expected the open-transaction error, got {other:?}"),
        }
        let conn = conn.into_inner();
        assert_eq!(conn.seen, ["BEGIN", "ROLLBACK"]);
        assert!(!conn.open);
    }
}

/// Transactions and flushes against REAL sql.js, through the service a block
/// calls — the harness `sql_js_conformance` describes.
///
/// "Durable" is checked the only way that means anything here: by reopening
/// the database from the OPFS image (`crate::db_init`) and reading it back.
/// sql.js's `db.export()`, which every flush runs, closes and reopens the
/// connection, so a write that reported success from inside a transaction
/// nobody committed is rolled back by that flush and is simply not there.
#[cfg(all(test, target_arch = "wasm32"))]
mod sql_js_transactions {
    use std::collections::HashMap;

    use wafer_core::interfaces::database::service::{DatabaseError, DatabaseService, WriteOp};
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{
        bridge_control, in_transaction,
        sql_js_conformance::{install_memory_opfs, opfs_writes},
        BrowserDatabaseService,
    };

    const TABLE: &str = "sql_js_tx_t";

    /// A fresh in-memory OPFS, a fresh sql.js database on it, and one table.
    async fn fresh() -> BrowserDatabaseService {
        install_memory_opfs();
        crate::db_init().await.expect("sql.js loads");
        let svc = BrowserDatabaseService;
        svc.exec_raw(
            &format!("CREATE TABLE {TABLE} (id TEXT PRIMARY KEY, name TEXT)"),
            &[],
        )
        .await
        .expect("create table");
        svc
    }

    fn row(id: &str) -> HashMap<String, serde_json::Value> {
        HashMap::from([
            ("id".to_string(), serde_json::json!(id)),
            ("name".to_string(), serde_json::json!(format!("row {id}"))),
        ])
    }

    /// The ids in the table as the flushed OPFS image holds them, sorted.
    async fn durable_ids(svc: &BrowserDatabaseService) -> Vec<String> {
        crate::db_init().await.expect("reopen from OPFS");
        let mut ids: Vec<String> = svc
            .query_raw(&format!("SELECT id FROM {TABLE}"), &[])
            .await
            .expect("read back")
            .into_iter()
            .map(|r| r.id)
            .collect();
        ids.sort();
        ids
    }

    /// **The coalescing half of the durability contract, on the real
    /// service.** `create_many` is one logical mutation however many rows it
    /// carries and however many statements it takes — here twenty inserts,
    /// plus the lazy ADD COLUMNs for the `created_at`/`updated_at` stamps
    /// before them — so it writes the database to OPFS exactly once, and that
    /// one write holds every row. Fails when `create_many` is `forward` (no
    /// flush: nothing durable) or flushes per statement.
    #[wasm_bindgen_test]
    async fn create_many_flushes_to_opfs_exactly_once() {
        let svc = fresh().await;
        let ids: Vec<String> = (0..20).map(|i| format!("r{i:02}")).collect();

        let before = opfs_writes();
        let inserted = svc
            .create_many(TABLE, ids.iter().map(|id| row(id)).collect())
            .await
            .expect("create_many");
        assert_eq!(inserted, 20);
        assert_eq!(opfs_writes() - before, 1, "one logical write, one flush");

        assert_eq!(durable_ids(&svc).await, ids);
    }

    /// The same for `batch`: a create, an update and a delete in one call are
    /// one transaction and one OPFS write.
    #[wasm_bindgen_test]
    async fn batch_flushes_to_opfs_exactly_once() {
        let svc = fresh().await;
        svc.create_many(TABLE, vec![row("keep"), row("drop")])
            .await
            .expect("seed");

        let before = opfs_writes();
        let outcomes = svc
            .batch(vec![
                WriteOp::Create {
                    collection: TABLE.into(),
                    data: row("new"),
                },
                WriteOp::Update {
                    collection: TABLE.into(),
                    id: "keep".into(),
                    data: HashMap::from([("name".to_string(), serde_json::json!("kept"))]),
                },
                WriteOp::Delete {
                    collection: TABLE.into(),
                    id: "drop".into(),
                },
            ])
            .await
            .expect("batch");
        assert_eq!(outcomes.len(), 3);
        assert_eq!(opfs_writes() - before, 1, "one logical write, one flush");

        assert_eq!(durable_ids(&svc).await, ["keep", "new"]);
    }

    /// **Fails on the pre-change tree.** A transaction left open on the
    /// connection — here by a `BEGIN` sent through `query_raw`, which does
    /// not flush and so never reaches the reopen that would end it — used to
    /// swallow the next `create_many`: its lazy `ADD COLUMN`s ran inside that
    /// transaction and its own `BEGIN` was refused ("cannot start a
    /// transaction within a transaction"). The write now rolls the stranger's
    /// transaction back before it starts, and its rows land and are durable.
    #[wasm_bindgen_test]
    async fn a_transaction_left_open_does_not_swallow_the_next_create_many() {
        let svc = fresh().await;
        svc.query_raw("BEGIN", &[])
            .await
            .expect("open a transaction");

        let inserted = svc
            .create_many(TABLE, vec![row("a"), row("b")])
            .await
            .expect("create_many recovers the connection");
        assert_eq!(inserted, 2);
        assert_eq!(durable_ids(&svc).await, ["a", "b"]);
    }

    /// **Fails on the pre-change tree**, where the `BEGIN` was reported as a
    /// successful write and the flush's reopen then rolled it back unseen. A
    /// write that leaves a transaction open is an error: whatever ran inside
    /// that transaction is gone.
    #[wasm_bindgen_test]
    async fn a_write_that_leaves_a_transaction_open_is_an_error() {
        let svc = fresh().await;
        let err = svc
            .exec_raw("BEGIN", &[])
            .await
            .expect_err("the flush rolled the transaction back");
        assert!(
            matches!(&err, DatabaseError::Internal(msg) if msg.contains("left a transaction open")),
            "{err:?}"
        );

        svc.create(TABLE, row("after"))
            .await
            .expect("a later write");
        assert_eq!(durable_ids(&svc).await, ["after"]);
    }

    /// **Fails on the pre-change tree.** sql.js cannot be made to refuse a
    /// `ROLLBACK` from outside, so the first one is refused here, in front of
    /// the real connection, which it therefore leaves inside the failed
    /// transaction. The pre-change framing logged that and returned, and the
    /// next write — a plain `create`, reported as a success — ran inside the
    /// dead transaction and was rolled back by its own flush, silently. Now
    /// the `ROLLBACK` is retried (`transaction_framing` pins that on its
    /// own) and the next write checks the connection before it starts, so the
    /// failed transaction's first insert is gone and the later write is
    /// durable; either one alone keeps this passing.
    #[wasm_bindgen_test]
    async fn a_refused_rollback_does_not_leave_the_connection_inside_the_transaction() {
        let svc = fresh().await;
        let mut refused = false;
        let out: Result<(), DatabaseError> = in_transaction(
            |sql| {
                if sql == "ROLLBACK" && !refused {
                    refused = true;
                    return Err("cannot rollback transaction - SQL statements in progress".into());
                }
                bridge_control(sql)
            },
            || {
                for id in ["a", "a"] {
                    let params = crate::db_codec::params_to_js(&[serde_json::json!(id)])
                        .map_err(DatabaseError::Internal)?;
                    crate::bridge::db_exec_raw(
                        &format!("INSERT INTO {TABLE} (id) VALUES (?)"),
                        params,
                    )
                    .map_err(|e| super::statement_failed(&e))?;
                }
                Ok(())
            },
        );
        assert!(refused, "the injected refusal was never reached");
        assert!(
            matches!(out, Err(DatabaseError::AlreadyExists(_))),
            "{out:?}"
        );

        svc.create(TABLE, row("b")).await.expect("a later write");
        assert_eq!(durable_ids(&svc).await, ["b"]);
    }

    /// **Fails on the pre-change tree.** sql.js's `db.export()`, which every
    /// flush runs, closes the connection and opens a new one — and a new
    /// connection has SQLite's defaults: foreign keys off, and no
    /// `base64_decode` (the function the vector service's upsert stores its
    /// blobs through). `dbInit` set both up once, so after the first flush
    /// neither held. A foreign key is enforced, and the function is there,
    /// after as many flushes as it takes.
    #[wasm_bindgen_test]
    async fn the_connection_setup_survives_a_flush() {
        let svc = fresh().await;
        svc.exec_raw("CREATE TABLE sql_js_fk_parent (id TEXT PRIMARY KEY)", &[])
            .await
            .expect("parent table");
        svc.exec_raw(
            "CREATE TABLE sql_js_fk_child (id TEXT PRIMARY KEY, \
             parent TEXT REFERENCES sql_js_fk_parent(id))",
            &[],
        )
        .await
        .expect("child table");

        let orphan = HashMap::from([
            ("id".to_string(), serde_json::json!("c1")),
            ("parent".to_string(), serde_json::json!("no such parent")),
        ]);
        let err = svc
            .create("sql_js_fk_child", orphan)
            .await
            .expect_err("a dangling foreign key is refused");
        assert!(
            matches!(&err, DatabaseError::Internal(msg) if msg.contains("FOREIGN KEY")),
            "{err:?}"
        );

        let decoded = svc
            .query_raw("SELECT length(base64_decode('AAAA')) AS n", &[])
            .await
            .expect("base64_decode is registered");
        assert_eq!(decoded[0].data.get("n"), Some(&serde_json::json!(3)));
    }
}
