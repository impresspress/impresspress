//! Async database service backed by Cloudflare D1 (SQLite at the edge).
//!
//! D1 implements only the [`DbExec`] execution *primitives* (prepare/bind/run
//! via [`json_value_to_js`]); all `get/list/count/sum/create/update/delete`
//! orchestration — filter/IN expansion, sorted-key INSERT/UPDATE construction,
//! lazy column-add, table-exists guards — is inherited from the shared
//! `wafer-core` [`DbExec`] defaults, identical to `wafer-block-sqlite` and
//! `wafer-block-postgres`. The `DatabaseService` impl forwards each method into
//! the matching `DbExec` default.
//!
//! ## Atomic multi-statement writes
//!
//! `create_many`, `batch` and the guarded writes all reach D1 through one
//! primitive, [`DbExec::run_transaction`], which this adapter implements with
//! D1's native `db.batch()`: the statements run in order inside one implicit
//! transaction and a failing statement rolls every earlier one back.
//!
//! ## The per-invocation statement budget
//!
//! D1 runs at most a fixed number of queries per Worker invocation, 1,000 on
//! Workers Free and Paid alike (see "Which limit" below). The
//! `IMPRESSPRESS_D1_QUERIES_PER_INVOCATION` Worker var states it (default
//! 1000; lower only for a Worker whose `limits.subrequests` is lower), read
//! once per invocation by
//! [`CfEnvironment::capture`](crate::environment::CfEnvironment::capture).
//!
//! Every D1 service built in one invocation shares one [`D1QueryCount`],
//! created at the Worker entry (`run_with_config`, `run_scheduled_with_config`)
//! and counted ONLY in the statement-sending primitives below: one per
//! `run_fetch`/`run_fetch_one`/`run_execute`/`run_scalar_*`, and one per
//! statement of a `run_batch`/`run_transaction` (D1's limits page does not say
//! whether a `db.batch()` counts as one query or N; N is the conservative
//! reading). A statement is counted when it is sent, not when it succeeds,
//! because D1 counts it either way. [`DbExec::statement_budget`] reports that
//! count against the limit, and `wafer-core` admits every `create_many` and
//! `batch` against it — in the database handler before the service runs, and
//! again before the transaction's first statement — so a write the invocation
//! cannot finish is refused whole instead of failing part-way inside D1. Every
//! primitive also refuses, before sending, a statement past the reported
//! limit.
//!
//! The counter is the invocation's, not the isolate's: one isolate interleaves
//! concurrent requests, and each has its own D1 limit. So it is never a
//! thread-local.
//!
//! A request's own services report D1's limit less the statements its
//! [`D1QueryCount`] reserves for the request's audit row
//! (`impresspress_core::after_response::AUDIT_ROW_STATEMENTS`, none when the
//! request-log policy is `off`); the row is written afterwards through a
//! [`D1QueryCount::for_reserved_work`] handle that may use them, so it fits
//! however the request spent its budget. Then the reservation is released,
//! and the request's deferred tasks run on everything the row left.
//!
//! ### Which limit
//!
//! A D1 query is a subrequest to a Cloudflare internal service. The Workers
//! limits page gives two subrequest rows: "Subrequests per invocation" (50 on
//! Free, 10,000 by default on Paid), which is `fetch()` to the outside, and
//! "Subrequests to internal services" (1,000 on Free, the configured limit on
//! Paid), which is D1, KV and R2. D1's own limits page still says 50 queries
//! per invocation on Free and 1,000 on Paid, but a Free-plan Worker runs a
//! fresh database's `/_deploy/prepare` — over 250 D1 queries — in one
//! invocation, so the internal-services row is the one D1 is held to. The
//! default of 1,000 is within it on both plans, and within D1's page for Paid.
//!
//! The budget counts D1 queries and nothing else. The Workers page puts KV
//! and R2 operations in that same internal-services row, and neither page
//! says whether they share one count with D1's queries; none of them is
//! counted here.
//!
//! Sources: <https://developers.cloudflare.com/d1/platform/limits/>,
//! <https://developers.cloudflare.com/workers/platform/limits/#subrequests>.
//!
//! A refusal because the invocation has run out is
//! [`DatabaseError::ResourceExhausted`], which a client sees as HTTP 429. It
//! does NOT mean "retry later": the same request retried does the same work
//! and is refused again, so a client that auto-retries 429s would loop. It
//! means this request asked for more statements than one invocation may run;
//! the fix is a smaller request (or, where the deploy stated a lowered
//! `IMPRESSPRESS_D1_QUERIES_PER_INVOCATION`, a higher one). A write larger than the whole
//! limit is `InvalidArgument`.
//!
//! ## A taken key
//!
//! D1 reports a failed statement only as text, so every error is classified by
//! [`impresspress_core::sqlite_text_error::statement_error`]: a primary- or
//! unique-key violation is [`DatabaseError::AlreadyExists`], as the native
//! backends report it from the driver's code, and anything else `Internal`.
//! That is part of the `DatabaseService` contract, not a courtesy:
//! `impresspress_core::blocks::crud` answers `AlreadyExists` as a 409 and
//! never re-reads a key to find out what a refused write meant. The tests
//! `a_create_refused_for_a_taken_key_is_already_exists` and
//! `a_batch_refused_for_a_taken_key_is_already_exists` pin it for the
//! single-row and the batch path.
//!
//! ## Lazy column-add
//!
//! Tables themselves must exist before any `create()` — every block ships
//! explicit `migrations/*.sql` applied from the `Init` lifecycle. The shared
//! `DbExec::ensure_data_columns` adds only a missing *column* a write's data
//! names (always `TEXT` on SQLite), and `DbExec::require_columns` refuses a
//! read, filter or guard naming an unknown column instead of adding it,
//! matching the native sqlite/postgres backends. Under STRICT_SCHEMA (below)
//! neither introspects. Outside STRICT_SCHEMA, reads against a missing table
//! return empty/NotFound via the `dbx_table_exists` guard the defaults run
//! first; under it the statement reaches D1 and its "no such table" failure
//! is `Internal`, as on the native backends.
//!
//! ## Schema cache + STRICT_SCHEMA (wafer-run #313)
//!
//! Each logical op the shared executor runs is fronted by schema introspection
//! — a table-exists check plus, on the lazy-column paths, a column-list query.
//! On D1 each is a *network* round-trip that dwarfs the data query. This
//! adapter therefore opts into the two `DbExec` accessors #313 added:
//!
//! - [`schema_cache`](DbExec::schema_cache) returns the isolate's
//!   [`SchemaCache`], so a warm isolate memoizes those introspection facts and
//!   issues zero introspection round-trips in steady state. The shared defaults
//!   own invalidation (lazy `ALTER TABLE`, `exec_raw`/DDL) — D1's own
//!   schema-*mutation* methods never run on the live path (schema is
//!   migration-owned; a runtime mutation attempt is an explicit error, see the
//!   `DatabaseService` impl) and so touch no cache.
//! - [`strict_schema`](DbExec::strict_schema) reads a flag seeded at
//!   construction from the deploy's `WAFER_RUN__DATABASE__STRICT_SCHEMA` var
//!   (see [`D1DatabaseService::new`]) and re-applied at lifecycle `Init` via
//!   [`set_strict_schema`](DatabaseService::set_strict_schema) for the one
//!   service a Wafer runtime is built around (the shared `wafer-run/database`
//!   block reads the same var from its `lifecycle(Init)` config, which the
//!   `ConfigSource` overlay resolves from the Worker env). When set the executor
//!   trusts the migrated schema: no table-exists probe, no lazy column-add.
//!   Production CF deploys enable it (wrangler `[vars]`); a write/query
//!   referencing an unmigrated column then fails loudly, as intended.
//!
//!   One introspection survives strict mode: a sorted or paged `list` ends its
//!   `ORDER BY` with the table's primary key, which the executor reads with a
//!   `pragma_table_info` the first time it lists that table
//!   ([`DbExec::get_primary_key`]) and memoizes in the schema cache (plus one
//!   table-exists probe for a table with no key).
//!
//!   Seeding at construction is what covers the D1 services that never reach
//!   an `Init`: the audit-row write's handle, built per request inside
//!   `run_with_config` and used from `ctx.wait_until`, and the handle
//!   `build_runtime` reads `block_settings` through before a runtime exists.
//!   Neither is ever handed to `set_strict_schema`, so without the seeding the
//!   audit-row write would run the lazy column-add path on every insert,
//!   whatever the deploy configured.
//!
//! The cache is the **isolate's**, not the service's ([`isolate_schema_cache`]),
//! and that is what makes the memoization worth anything: every D1 service is
//! built per request (`warm_request_services`, the audit-row write handle,
//! `build_runtime`'s pre-`Init` `block_settings` read), and every request runs
//! `D1ConfigSource::snapshot`, a paged `list` over the variables table. A
//! per-service cache would be cold for each of them, so each request would pay
//! the key round trip. What may invalidate the cache is enumerated on
//! [`D1DatabaseService::schema_cache`].

use std::{
    cell::Cell,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

use impresspress_core::IdentityCache;
use wafer_block::db::{Filter, ListOptions};
use wafer_core::interfaces::database::{
    codec::{record_from_json_row, scalar_f64, scalar_i64, JsonColumns},
    exec::{BatchOp, BatchResult, DbExec, TxOp, TxResult},
    schema_cache::SchemaCache,
    service::{
        AggregateSpec, CapGuard, Column, DatabaseError, DatabaseService, GuardedInsert,
        GuardedUpdate, Record, RecordList, StatementBudget, Table, UpsertSpec, WriteOp,
        WriteOutcome,
    },
};
use wafer_sql_utils::{introspect, Backend};
use wasm_bindgen::JsValue;
use worker::*;

thread_local! {
    /// The isolate's [`SchemaCache`], keyed by D1 binding name and shared by
    /// every [`D1DatabaseService`] over that binding (see
    /// [`isolate_schema_cache`]).
    ///
    /// An [`IdentityCache`], built on `IsolateCell` rather than a `RefCell`:
    /// this is isolate-lifetime state on the request path, and a request
    /// hard-stopped inside a borrow would strand the flag for the life of the
    /// isolate. The worst a hard stop can do here is drop the entry, which is
    /// a cold isolate — a state every caller already handles.
    static ISOLATE_SCHEMA_CACHE: IdentityCache<SchemaCache> = const { IdentityCache::new() };
}

/// The isolate's schema cache for the D1 binding named `binding`, created on
/// first use.
///
/// Scoped to the isolate rather than to one service because the services are
/// per *request*: `warm_request_services` builds a fresh `D1DatabaseService`
/// (and so a fresh `KvCachedD1DatabaseService` and `D1ConfigSource`) for every
/// request it serves. A per-service cache is therefore always cold, and
/// `D1ConfigSource::snapshot` — which every request runs — issues a paged
/// `list` over the variables table, so each request would pay a
/// `pragma_table_info` round trip for the primary key before its select. One
/// cache per isolate is what makes the introspection amortize, and it is the
/// same argument the browser backend's cache is a static for: the facts
/// describe the database, not the handle.
///
/// Keyed by binding name because the two constructors that reach this
/// (`services::make_d1_database_service`,
/// `services::make_kv_cached_database_service`) are public and take one:
/// two bindings are two databases, and one table name can mean a different
/// schema in each. `IdentityCache` holds one entry, so a second binding
/// *replaces* the first's rather than being answered from it — a worker that
/// alternates bindings re-introspects instead of being told the wrong schema.
/// Impresspress ships one D1 binding (`runner::D1_BINDING`).
///
/// What may invalidate it is enumerated on [`D1DatabaseService::schema_cache`].
pub(crate) fn isolate_schema_cache(binding: &str) -> Rc<SchemaCache> {
    ISOLATE_SCHEMA_CACHE.with(|cache| {
        if let Some(entry) = cache.get(binding) {
            return entry;
        }
        let entry = Rc::new(SchemaCache::new());
        cache.store(binding.to_string(), Rc::clone(&entry));
        entry
    })
}

/// Drop the isolate's schema cache.
///
/// Called when this isolate installs a (re)built runtime — the event that
/// follows a deploy, a migration run or any config-version bump, and so the
/// one point at which a schema change made by *another* isolate can be
/// assumed to have reached this one. Within an isolate the shared `DbExec`
/// paths invalidate per table as they mutate; see
/// [`D1DatabaseService::schema_cache`].
pub(crate) fn forget_isolate_schema() {
    ISOLATE_SCHEMA_CACHE.with(IdentityCache::clear);
}

/// The D1 statements one Worker invocation has sent, shared by every
/// [`D1DatabaseService`] built in that invocation (see the module docs), and
/// the statements of the invocation's limit reserved for its audit row.
///
/// Create one at the Worker entry, once per `fetch` or `scheduled`
/// invocation, and hand a clone to each service built while serving it: the
/// request's own services, the runtime build's pre-`Init` handle, the write
/// of the request's audit row run from `ctx.wait_until`. They all count
/// against the one D1 limit. Never keep one past its invocation or share it
/// between two: an isolate interleaves concurrent requests, and each has its
/// own limit.
///
/// [`reserve`](Self::reserve) holds statements back from every service built
/// from this count, so the request cannot spend them however it spends its
/// budget; [`for_reserved_work`](Self::for_reserved_work) is the handle the
/// audit row is written with, which may use them; and
/// [`release_reservation`](Self::release_reservation) hands whatever the row
/// did not use back to everything else once it is written or skipped.
#[derive(Clone, Default)]
pub struct D1QueryCount {
    sent: Rc<Cell<u64>>,
    reserved: Rc<Cell<u64>>,
    spends_reservation: bool,
}

impl D1QueryCount {
    /// A fresh count for a new invocation, reserving nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold `statements` of the invocation's limit back from every service
    /// built from this count, other than [`for_reserved_work`] handles: they
    /// report a limit that much lower and refuse any statement past it.
    ///
    /// [`for_reserved_work`]: Self::for_reserved_work
    pub fn reserve(&self, statements: u64) {
        self.reserved.set(statements);
    }

    /// Give the reservation back: every service built from this count may
    /// use the invocation's whole limit again, less what has been sent.
    pub fn release_reservation(&self) {
        self.reserved.set(0);
    }

    /// This invocation's count, for the work the reservation is for: services
    /// built from it see the invocation's whole limit.
    pub fn for_reserved_work(&self) -> Self {
        Self {
            spends_reservation: true,
            ..self.clone()
        }
    }

    /// Statements sent in this invocation so far, through every handle.
    pub fn sent(&self) -> u64 {
        self.sent.get()
    }

    /// The statements currently reserved, for a test that checks when the
    /// reservation is released.
    #[cfg(test)]
    pub(crate) fn reserved_for_test(&self) -> u64 {
        self.reserved.get()
    }

    /// The statements a service built from this handle must leave unused.
    fn held_back(&self) -> u64 {
        if self.spends_reservation {
            0
        } else {
            self.reserved.get()
        }
    }

    fn add(&self, statements: usize) {
        let statements = u64::try_from(statements).unwrap_or(u64::MAX);
        self.sent.set(self.sent.get().saturating_add(statements));
    }
}

/// Async database service wrapping Cloudflare D1.
pub struct D1DatabaseService {
    db: D1Database,
    /// The isolate's memoized table-exists / column-list / primary-key facts
    /// (see [`isolate_schema_cache`]). An `Rc`, not an owned cache: every
    /// service in the isolate addresses the same D1 database, and each of
    /// them is built per request.
    schema_cache: Rc<SchemaCache>,
    /// STRICT_SCHEMA flag. Seeded at construction from the deploy's
    /// `WAFER_RUN__DATABASE__STRICT_SCHEMA` var, and re-applied at lifecycle
    /// `Init` via [`DatabaseService::set_strict_schema`] for the one service a
    /// Wafer runtime is built around. When set, the shared executor skips the
    /// table-exists probe and the lazy column-add; a sorted or paged `list`
    /// still looks up the table's primary key. `AtomicBool` (not `Cell`) so
    /// the struct keeps the `Sync` bound `Arc<dyn DatabaseService>` needs; on
    /// wasm32's single thread the ordering is immaterial.
    strict_schema: AtomicBool,
    /// This invocation's statement count (see [`D1QueryCount`]).
    queries: D1QueryCount,
    /// Statements one invocation may send: this deploy's
    /// `IMPRESSPRESS_D1_QUERIES_PER_INVOCATION`.
    query_limit: u64,
}

impl D1DatabaseService {
    /// Wrap a D1 binding, with this deploy's STRICT_SCHEMA verdict already
    /// applied.
    ///
    /// `strict_schema` is a parameter rather than a `false` default a caller
    /// may later overwrite because not every D1 service reaches a lifecycle
    /// `Init`: the audit-row write handle and `build_runtime`'s pre-`Init`
    /// `block_settings` read are both constructed outside any runtime, and a
    /// default would silently put them on the always-introspect path. Callers
    /// get the verdict from
    /// [`CfEnvironment::strict_schema_enabled`](crate::environment::CfEnvironment::strict_schema_enabled).
    ///
    /// `Init` still calls [`DatabaseService::set_strict_schema`] on the
    /// runtime's own service; it reads the same var from its `lifecycle(Init)`
    /// config, which the `ConfigSource` overlay resolves from the Worker env,
    /// so it re-affirms this value rather than contradicting it.
    ///
    /// `binding` is the D1 binding `db` came from. It is the key of the
    /// isolate's schema cache ([`isolate_schema_cache`]): two bindings are two
    /// databases, and one table name can name a different schema in each.
    ///
    /// `queries` is the invocation's statement count, shared with every other
    /// D1 service built in it, and `query_limit` the statements one invocation
    /// may send (see the module docs on the per-invocation budget).
    pub fn new(
        db: D1Database,
        strict_schema: bool,
        binding: &str,
        queries: D1QueryCount,
        query_limit: u64,
    ) -> Self {
        Self {
            db,
            schema_cache: isolate_schema_cache(binding),
            strict_schema: AtomicBool::new(strict_schema),
            queries,
            query_limit,
        }
    }

    /// Count `statements` about to be sent, or refuse them — before they
    /// reach D1 — when they would take the invocation past the limit this
    /// service reports: D1's own limit less what its [`D1QueryCount`] holds
    /// back. Refusing single statements too, not only the multi-statement
    /// writes `wafer-core` admits, is what keeps held-back statements free
    /// for the work they are reserved for.
    fn send(&self, statements: usize) -> Result<(), DatabaseError> {
        DbExec::statement_budget(self)?.admit(statements, "this statement")?;
        self.queries.add(statements);
        Ok(())
    }

    /// Bind `params` (the JSON form produced by `sea_values_to_json`) to a
    /// prepared statement, mapping each value to a `JsValue` at the edge.
    fn prepare_bind(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<D1PreparedStatement, DatabaseError> {
        let js_params: Vec<JsValue> = params.iter().map(json_value_to_js).collect();
        self.db.prepare(sql).bind(&js_params).map_err(db_err)
    }
}

// SAFETY: `D1DatabaseService` holds a `D1Database` handle scoped to a single
// Worker isolate. wasm32-unknown-unknown has no threads, so the
// `Send`/`Sync` bounds required by `Arc<dyn DatabaseService>` are satisfied
// trivially — no cross-thread aliasing or data races can occur. `strict_schema`
// (`AtomicBool`) is `Send + Sync` on its own; `schema_cache` is an
// `Rc<SchemaCache>`, which is not, and is sound here for the same reason the
// `D1Database` handle is: the `Rc` and every clone of it live in the one
// isolate that owns this thread-local cache. The `unsafe impl` is required by
// both.
unsafe impl Send for D1DatabaseService {}
unsafe impl Sync for D1DatabaseService {}

// ---------------------------------------------------------------------------
// DbExec primitives — the only backend-specific execution code.
// ---------------------------------------------------------------------------

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl DbExec for D1DatabaseService {
    const BACKEND: Backend = Backend::Sqlite;

    /// The isolate's cache (see [`isolate_schema_cache`]).
    ///
    /// Every D1 schema change invalidates what it touches, which is what
    /// makes an isolate-lifetime cache sound here:
    ///
    /// - **Migrations** reach `DatabaseService::exec_raw` — both the
    ///   `database.ddl` host op and `migration_helper::apply_ddl_via_service`
    ///   end there, and the KV decorator forwards it — and the shared
    ///   [`DbExec::exec_raw`] clears the whole cache after a statement it
    ///   cannot attribute to a table. (The admin SQL explorer is not a writer:
    ///   `validate_readonly_query` refuses anything but a read.)
    /// - **The lazy column-add** (`DbExec::add_column_checked`, non-strict
    ///   mode only) invalidates the table it alters.
    /// - **`ensure_schema_table`, `schema_add_column`, `schema_drop_table`**
    ///   are refused on D1 (see their impls below): the schema is
    ///   migration-owned, so a block cannot mutate it around the path above.
    ///   That covers the dev sandbox too — its tables come from
    ///   `dev/migrations/*.sql` through the migration runner, and it issues no
    ///   runtime DDL outside it.
    /// - **Another isolate's migration** is outside this cache's reach, and
    ///   not promptly. The deploy that runs it bumps the KV config-version
    ///   stamp (`force_bump_config_version`), which marks the *writing*
    ///   isolate dirty; every other isolate notices at its next version probe,
    ///   which `runtime_cache`'s jittered floor puts up to
    ///   `PROBE_INTERVAL_FLOOR_MS + PROBE_INTERVAL_JITTER_MS` away (5-10
    ///   minutes). The rebuild that follows calls [`forget_isolate_schema`].
    ///
    ///   What can be stale in that window is bounded by what is cached:
    ///   a negative table-exists is never stored (the shared
    ///   `DbExec::table_present_for_op` memoizes only a present table), so a table the
    ///   migration creates is not read as missing; a column list that predates
    ///   an `ADD COLUMN` only makes the lazy add re-run, which
    ///   `add_column_checked` treats as benign; and a primary key is stale
    ///   only if a migration rebuilds the table under a different key, which
    ///   would make a sorted `list` in an unrebuilt isolate sort by a column
    ///   that no longer exists until the probe lands.
    fn schema_cache(&self) -> Option<&SchemaCache> {
        Some(&self.schema_cache)
    }

    fn strict_schema(&self) -> bool {
        self.strict_schema.load(Ordering::Relaxed)
    }

    /// D1's per-invocation query limit and the statements this invocation has
    /// sent through every D1 service sharing its [`D1QueryCount`].
    fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
        Ok(StatementBudget::Limited {
            limit: self.query_limit.saturating_sub(self.queries.held_back()),
            used: self.queries.sent(),
        })
    }

    async fn run_fetch(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError> {
        let stmt = self.prepare_bind(sql, params)?;
        self.send(1)?;
        let results = stmt.all().await.map_err(db_err)?;
        let rows: Vec<serde_json::Value> = results.results().map_err(db_err)?;
        Ok(rows
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
        let stmt = self.prepare_bind(sql, params)?;
        self.send(1)?;
        // A missing table is a fault, not an absent row: it goes through
        // `db_err` like every other failure and comes back `Internal`, as it
        // does on native SQLite and PostgreSQL. `NotFound` is only a query
        // that ran and matched no row.
        let row = stmt
            .first::<serde_json::Value>(None)
            .await
            .map_err(db_err)?;
        row.map(|row| record_from_json_row(row, json))
            .ok_or(DatabaseError::NotFound)
    }

    async fn run_execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let stmt = self.prepare_bind(sql, params)?;
        self.send(1)?;
        let result = stmt.run().await.map_err(db_err)?;
        changes(&result)
    }

    /// Delegates to [`run_fetch`](Self::run_fetch), which counts the
    /// statement: a D1 binding is one
    /// handle, and every statement this adapter issues goes through
    /// `db.prepare()` on it — there is no reader/writer split for a
    /// `DELETE … RETURNING` to land on the wrong side of, and
    /// `D1PreparedStatement::all()` applies a statement's side effects just
    /// as `run()` does while also handing back the `RETURNING` rows. (This
    /// adapter uses no D1 Sessions API, so no read-replica routing exists
    /// here either.) Delegating rather than repeating `prepare_bind` +
    /// `all()` + `record_from_json_row` keeps the two decode paths identical
    /// by construction.
    ///
    /// It stays a distinct trait method rather than riding on `run_fetch`
    /// at the call site because the *contract* differs — a statement with
    /// side effects that also returns rows — and the shared
    /// [`DbExec::create`], [`DbExec::take_where`] and [`DbExec::upsert`]
    /// route through it: the day this adapter grows read-replica routing,
    /// only this delegation changes.
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
        let stmt = self.prepare_bind(sql, params)?;
        self.send(1)?;
        let row = stmt
            .first::<serde_json::Value>(None)
            .await
            .map_err(db_err)?;
        Ok(scalar_i64(row))
    }

    async fn run_scalar_f64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<f64, DatabaseError> {
        let stmt = self.prepare_bind(sql, params)?;
        self.send(1)?;
        let row = stmt
            .first::<serde_json::Value>(None)
            .await
            .map_err(db_err)?;
        Ok(scalar_f64(row))
    }

    /// Counted by the `run_scalar_i64` it sends through, not here.
    async fn dbx_table_exists(&self, table: &str) -> Result<bool, DatabaseError> {
        let (sql, params) = introspect::build_table_exists(table, Backend::Sqlite);
        Ok(self.run_scalar_i64(&sql, &params).await? > 0)
    }

    /// Collapse `ops` into ONE native D1 `db.batch()` round-trip.
    ///
    /// The shared `DbExec` default issues each op through its own single-
    /// statement primitive — on D1 that is N separate network round-trips.
    /// This override prepares every op's `(sql, params)` uniformly (the same
    /// `prepare_bind` the primitives use, so binding is byte-identical) and
    /// submits them as a single `db.batch()`, exactly like
    /// [`run_transaction`](DbExec::run_transaction).
    /// D1 returns one [`D1Result`] per statement **in submission order**
    /// (worker-rs `D1Database::batch` documents this), so each result is
    /// decoded positionally into the [`BatchResult`] variant its op names,
    /// reusing the very helpers the single-statement primitives use:
    ///
    /// - [`BatchOp::Rows`] → `results()` → [`record_from_json_row`] per row (as
    ///   [`run_fetch`](DbExec::run_fetch)).
    /// - [`BatchOp::FetchOne`] → first of `results()` → `record_from_json_row`,
    ///   empty ⇒ [`DatabaseError::NotFound`] (as
    ///   [`run_fetch_one`](DbExec::run_fetch_one)).
    /// - [`BatchOp::Execute`] → `meta().changes` (as
    ///   [`run_execute`](DbExec::run_execute)).
    /// - [`BatchOp::ScalarI64`] → first of `results()` → [`scalar_i64`] (as
    ///   [`run_scalar_i64`](DbExec::run_scalar_i64)).
    /// - [`BatchOp::ScalarF64`] → first of `results()` → [`scalar_f64`] (as
    ///   [`run_scalar_f64`](DbExec::run_scalar_f64)).
    ///
    /// Note `results()` yields the same first row object `first()` returns, so
    /// the scalar/fetch-one decode matches the primitives byte-for-byte.
    ///
    /// **Transactional vs. sequential (all-or-nothing).** D1 `batch()` runs
    /// the statements sequentially inside one implicit transaction: it stops
    /// at the first failing statement and rolls the whole batch back, and the
    /// `batch()` promise rejects — so a failure surfaces here as the outer
    /// `Err` (the whole call fails; results below are only reached when every
    /// statement succeeded). This preserves the sequential default's
    /// first-error identity (the first failing statement's error, later ops
    /// not observed) but is **stricter**: the sequential default leaves an
    /// *earlier* successful statement's side effects committed, whereas the
    /// batch rolls them back. For the read-only `list` count+select batch that
    /// is purely a consistency win (both statements see one snapshot); for
    /// `update`'s UPDATE+re-fetch it means a failing re-fetch would also
    /// undo the UPDATE — a failure that cannot occur on the happy path (a
    /// well-formed by-id SELECT against the just-updated table), and rolling
    /// back rather than half-applying is the safe direction regardless.
    async fn run_batch(&self, ops: &[BatchOp<'_>]) -> Result<Vec<BatchResult>, DatabaseError> {
        // An empty batch is a no-op; `db.batch(vec![])` has nothing to submit.
        // Matches the sequential default (which pushes nothing).
        if ops.is_empty() {
            return Ok(Vec::new());
        }

        // Prepare + bind every statement, then submit as ONE round-trip.
        let mut statements = Vec::with_capacity(ops.len());
        for op in ops {
            let (sql, params) = op.sql_params();
            statements.push(self.prepare_bind(sql, params)?);
        }
        self.send(statements.len())?;
        let results = self.db.batch(statements).await.map_err(db_err)?;

        // D1 returns exactly one result per submitted statement, in order. A
        // length mismatch would break positional decoding — surface it rather
        // than silently mis-aligning results with ops.
        if results.len() != ops.len() {
            return Err(DatabaseError::Internal(format!(
                "D1 batch returned {} results for {} statements",
                results.len(),
                ops.len()
            )));
        }

        let mut out = Vec::with_capacity(ops.len());
        for (op, result) in ops.iter().zip(results.iter()) {
            // A transactional batch rejects (the `Err` above) on any statement
            // failure, so a non-success result here is unexpected; guard it
            // and surface D1's own error text rather than decode a failed
            // statement.
            check_statement_succeeded(result)?;
            let decoded = match op {
                BatchOp::Rows { json, .. } => {
                    let rows: Vec<serde_json::Value> = result.results().map_err(db_err)?;
                    BatchResult::Rows(
                        rows.into_iter()
                            .map(|row| record_from_json_row(row, json))
                            .collect(),
                    )
                }
                BatchOp::FetchOne { json, .. } => {
                    let rows: Vec<serde_json::Value> = result.results().map_err(db_err)?;
                    let row = rows.into_iter().next().ok_or(DatabaseError::NotFound)?;
                    BatchResult::FetchOne(record_from_json_row(row, json))
                }
                // Same source as `run_execute`: D1Result meta's `changes`.
                BatchOp::Execute { .. } => BatchResult::Execute(changes(result)?),
                BatchOp::ScalarI64 { .. } => {
                    let rows: Vec<serde_json::Value> = result.results().map_err(db_err)?;
                    BatchResult::ScalarI64(scalar_i64(rows.into_iter().next()))
                }
                BatchOp::ScalarF64 { .. } => {
                    let rows: Vec<serde_json::Value> = result.results().map_err(db_err)?;
                    BatchResult::ScalarF64(scalar_f64(rows.into_iter().next()))
                }
            };
            out.push(decoded);
        }
        Ok(out)
    }

    /// Run `ops` as ONE native D1 `db.batch()`, which is what makes it a
    /// transaction: D1 runs a batch's statements in order inside one implicit
    /// transaction, and a failing statement rolls back every statement before
    /// it and rejects the whole call (the outer `Err`, classified by
    /// [`db_err`] so a taken key is `AlreadyExists`).
    ///
    /// Decoded positionally, as [`run_batch`](DbExec::run_batch) is: a
    /// [`TxOp::Execute`] yields `meta().changes`, as
    /// [`run_execute`](DbExec::run_execute) does, and a [`TxOp::Returning`]
    /// its `RETURNING` rows through [`record_from_json_row`], as
    /// [`run_execute_returning`](DbExec::run_execute_returning) does.
    async fn run_transaction(&self, ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, DatabaseError> {
        if ops.is_empty() {
            return Ok(Vec::new());
        }
        let mut statements = Vec::with_capacity(ops.len());
        for op in ops {
            let (sql, params) = op.sql_params();
            statements.push(self.prepare_bind(sql, params)?);
        }
        self.send(statements.len())?;
        let results = self.db.batch(statements).await.map_err(db_err)?;
        if results.len() != ops.len() {
            return Err(DatabaseError::Internal(format!(
                "D1 batch returned {} results for {} statements",
                results.len(),
                ops.len()
            )));
        }
        ops.iter()
            .zip(results.iter())
            .map(|(op, result)| {
                check_statement_succeeded(result)?;
                Ok(match op {
                    TxOp::Execute { .. } => TxResult::Execute(changes(result)?),
                    TxOp::Returning { json, .. } => {
                        let rows: Vec<serde_json::Value> = result.results().map_err(db_err)?;
                        TxResult::Returning(
                            rows.into_iter()
                                .map(|row| record_from_json_row(row, json))
                                .collect(),
                        )
                    }
                })
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// DatabaseService — forwards into the shared DbExec defaults.
// ---------------------------------------------------------------------------

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl DatabaseService for D1DatabaseService {
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        DbExec::get(self, collection, id).await
    }

    async fn list(
        &self,
        collection: &str,
        opts: &ListOptions,
    ) -> Result<RecordList, DatabaseError> {
        DbExec::list(self, collection, opts).await
    }

    async fn create(
        &self,
        collection: &str,
        data: std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        DbExec::create(self, collection, data).await
    }

    async fn update(
        &self,
        collection: &str,
        id: &str,
        data: std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        DbExec::update(self, collection, id, data).await
    }

    async fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError> {
        DbExec::delete(self, collection, id).await
    }

    async fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError> {
        DbExec::count(self, collection, filters).await
    }

    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        DbExec::sum(self, collection, field, filters).await
    }

    async fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        DbExec::query_raw(self, query, args).await
    }

    async fn exec_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        DbExec::exec_raw(self, query, args).await
    }

    async fn delete_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<(), DatabaseError> {
        DbExec::delete_where(self, collection, filters).await
    }

    async fn delete_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        DbExec::delete_where_count(self, collection, filters).await
    }

    async fn take_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError> {
        DbExec::take_where(self, collection, filters).await
    }

    async fn update_where(
        &self,
        collection: &str,
        filters: &[Filter],
        data: std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        DbExec::update_where(self, collection, filters, data).await
    }

    async fn increment_field_where(
        &self,
        collection: &str,
        col: &str,
        delta: i64,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        DbExec::increment_field_where(self, collection, col, delta, filters).await
    }

    async fn upsert(
        &self,
        collection: &str,
        spec: UpsertSpec,
    ) -> Result<Option<Record>, DatabaseError> {
        DbExec::upsert(self, collection, spec).await
    }

    async fn aggregate(
        &self,
        collection: &str,
        spec: AggregateSpec,
    ) -> Result<Vec<Record>, DatabaseError> {
        DbExec::aggregate(self, collection, spec).await
    }

    async fn update_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
        data: std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<i64, DatabaseError> {
        DbExec::update_where_count(self, collection, filters, data).await
    }

    async fn create_many(
        &self,
        collection: &str,
        rows: Vec<std::collections::HashMap<String, serde_json::Value>>,
    ) -> Result<i64, DatabaseError> {
        DbExec::create_many(self, collection, rows).await
    }

    async fn batch(&self, ops: Vec<WriteOp>) -> Result<Vec<WriteOutcome>, DatabaseError> {
        DbExec::batch(self, ops).await
    }

    async fn insert_guarded(
        &self,
        collection: &str,
        data: std::collections::HashMap<String, serde_json::Value>,
        guards: &[CapGuard],
    ) -> Result<GuardedInsert, DatabaseError> {
        DbExec::insert_guarded(self, collection, data, guards).await
    }

    async fn update_guarded(
        &self,
        collection: &str,
        filters: &[Filter],
        data: std::collections::HashMap<String, serde_json::Value>,
        guards: &[CapGuard],
    ) -> Result<GuardedUpdate, DatabaseError> {
        DbExec::update_guarded(self, collection, filters, data, guards).await
    }

    // --- Schema management: D1 schema is migration-owned ---
    //
    // D1's schema is established *exclusively* by each block's `migrations/*.sql`,
    // applied at lifecycle `Init` through `db::ddl` (the host DDL op → `run_execute`)
    // and gated by the migration-bless / `IMPRESSPRESS_RUN_MIGRATIONS` workflow.
    // The runtime `DatabaseService` schema-*mutation* methods below are therefore
    // never part of the live D1 path:
    //
    // - `ensure_schema_table` runs only from `handler::handle_lifecycle`, and only
    //   when the `wafer-run/database` block is registered *with* a non-empty table
    //   set. Impresspress always registers it via `service_blocks::database::register_with`
    //   (empty `tables`), so `handle_lifecycle` takes its `tables.is_empty()` branch
    //   and never calls this.
    // - `schema_add_column` / `schema_drop_table` have no production caller at all.
    //   The shared `DbExec` lazy column-add issues its `ALTER TABLE ADD COLUMN` via
    //   `run_execute` (see `DbExec::add_column_checked`), not this method, and it is
    //   disabled outright under STRICT_SCHEMA (which live CF deploys set).
    //
    // Returning `Ok(())` here would *claim* a mutation happened when nothing did — a
    // silent success that only surfaces later as a confusing "no such table" / "no
    // such column" from the next query. Instead each returns an explicit error so a
    // mistaken runtime schema mutation on D1 fails loudly and names the fix (edit the
    // block's migration files). `schema_table_exists` and `schema_columns` are reads and stay live.

    async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
        Err(schema_mutation_unsupported(
            "ensure_schema_table",
            &table.name,
        ))
    }

    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        DbExec::schema_table_exists(self, name).await
    }

    async fn schema_columns(&self, table: &str) -> Result<Vec<String>, DatabaseError> {
        DbExec::schema_columns(self, table).await
    }

    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
        Err(schema_mutation_unsupported("schema_drop_table", name))
    }

    async fn schema_add_column(&self, table: &str, _column: &Column) -> Result<(), DatabaseError> {
        Err(schema_mutation_unsupported("schema_add_column", table))
    }

    fn set_strict_schema(&self, enabled: bool) {
        self.strict_schema.store(enabled, Ordering::Relaxed);
    }

    fn statement_budget(&self) -> Result<StatementBudget, DatabaseError> {
        DbExec::statement_budget(self)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a serde_json::Value param to a JsValue for D1 binding. Arrays and
/// objects bind as JSON text (D1 stores JSON columns as TEXT), matching the
/// `coerce_param` policy on the browser backend.
fn json_value_to_js(val: &serde_json::Value) -> JsValue {
    match val {
        serde_json::Value::Null => JsValue::NULL,
        serde_json::Value::Bool(b) => JsValue::from(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                JsValue::from(i as f64)
            } else if let Some(f) = n.as_f64() {
                JsValue::from(f)
            } else {
                JsValue::from(n.to_string())
            }
        }
        serde_json::Value::String(s) => JsValue::from(s.as_str()),
        _ => JsValue::from(val.to_string()),
    }
}

/// A D1 failure as a [`DatabaseError`]. D1 hands back text only, so a taken
/// key is recognised by it — see
/// [`impresspress_core::sqlite_text_error::statement_error`] — and that text
/// is [`d1_error_text`].
fn db_err(e: worker::Error) -> DatabaseError {
    impresspress_core::sqlite_text_error::statement_error(d1_error_text(&e))
}

/// The whole text of a failed D1 call.
///
/// worker-rs wraps a rejection whose message starts with `D1` as
/// [`worker::Error::D1`], and that variant's `Display` prints only the JS
/// error's `cause` — which is `undefined` when D1 attached none, dropping the
/// message (`D1_ERROR: UNIQUE constraint failed: …`) the classifier reads.
/// So a D1 error is spelled here as its message followed by its cause; every
/// other variant's `Display` already carries its text.
fn d1_error_text(e: &worker::Error) -> String {
    match e {
        worker::Error::D1(d1) => {
            let error: &js_sys::Error = d1.as_ref();
            format!("{}: {}", String::from(error.message()), d1.cause())
        }
        other => other.to_string(),
    }
}

/// The affected-row count of one statement's result. worker-rs exposes
/// `D1Result::meta().changes` (`Option<usize>`) for mutations; it is a real
/// count, so the shared defaults can map 0 rows to `NotFound` on an
/// update/delete by id.
fn changes(result: &D1Result) -> Result<i64, DatabaseError> {
    let changes = result
        .meta()
        .map_err(db_err)?
        .and_then(|m| m.changes)
        .unwrap_or(0);
    Ok(changes as i64)
}

/// `Err` carrying D1's own text when one statement of a batch reports
/// failure. A batch rejects outright when a statement fails, so this is the
/// guard against a result that says otherwise, not the usual failure path.
fn check_statement_succeeded(result: &D1Result) -> Result<(), DatabaseError> {
    if result.success() {
        return Ok(());
    }
    Err(impresspress_core::sqlite_text_error::statement_error(
        format!(
            "D1 batch statement failed: {}",
            result
                .error()
                .unwrap_or_else(|| "unknown error".to_string())
        ),
    ))
}

/// The explicit "runtime schema mutation unsupported on D1" error shared by the
/// three schema-*mutation* methods (`ensure_schema_table`, `schema_drop_table`,
/// `schema_add_column`). D1's schema is migration-owned (see those methods'
/// doc comment): a runtime mutation attempt is a genuine misuse, so it fails
/// loudly rather than returning a silent `Ok(())` that hides the no-op. `method`
/// is the trait method name and `target` the table it was asked to mutate — both
/// surfaced so the misuse is diagnosable from the error line alone.
fn schema_mutation_unsupported(method: &str, target: &str) -> DatabaseError {
    DatabaseError::Internal(format!(
        "runtime schema mutation unsupported on Cloudflare D1: `{method}` on \
         `{target}` — D1 schema is migration-owned. Declare the table/column in \
         the block's `migrations/*.sql` (applied at Init via `db::ddl`) instead \
         of mutating the schema at runtime."
    ))
}

// Note: unit tests for the pure SQL-planning layer live in `wafer-sql-utils`
// and `wafer-core::interfaces::database::exec` (shared across all SQL
// backends). `impresspress-cloudflare` only compiles on `wasm32-unknown-unknown`
// (the R2/D1 services hold `!Send` JsFutures), so `cargo test
// -p impresspress-cloudflare` errors before reaching any test module. The
// `wasm_bindgen_test`s below run under Node in CI's `cloudflare-wasm-test` job;
// end-to-end validation of the live D1 path comes from a real CF deploy.

#[cfg(test)]
mod tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    /// SQLite — and therefore D1 — has no array/object storage class, so a
    /// JSON column's value is stored as JSON text. This is the row decoder
    /// every D1 read goes through: `run_fetch`, `run_fetch_one` and the
    /// `BatchOp::Rows`/`FetchOne` arms of `run_batch` all map their rows with
    /// it, handing it the statement's set of JSON columns. A column in that
    /// set decodes back to the structure that was written; any other column's
    /// text stays the string that was written, however much it looks like
    /// JSON — a title of `[1]` is a title, not an array.
    ///
    /// The end-to-end pin is
    /// `wafer_core::interfaces::database::conformance::run_conformance`,
    /// which this crate can only *typecheck* (see `conformance.rs`: a live run
    /// needs a workerd D1 binding CI does not have).
    #[wasm_bindgen_test]
    fn a_json_column_decodes_back_and_a_text_column_stays_text() {
        let record = record_from_json_row(
            serde_json::json!({
                "id": "r1",
                "meta": "{\"k\":[1,2],\"nested\":{\"b\":true}}",
                "tags": "[\"a\",\"b\"]",
                "note": "hello world",
                "braced_prose": "{not json at all",
                "count": 3,
            }),
            &JsonColumns::new(["meta", "braced_prose"]),
        );

        assert_eq!(record.id, "r1");
        assert_eq!(
            record.data.get("meta"),
            Some(&serde_json::json!({"k": [1, 2], "nested": {"b": true}})),
            "a JSON column decodes back to the object that was written",
        );
        assert_eq!(
            record.data.get("tags"),
            Some(&serde_json::json!("[\"a\",\"b\"]")),
            "JSON-looking text in a column not declared JSON stays text",
        );
        assert_eq!(
            record.data.get("note"),
            Some(&serde_json::json!("hello world")),
        );
        assert_eq!(
            record.data.get("braced_prose"),
            Some(&serde_json::json!("{not json at all")),
            "text that does not parse is returned verbatim, even in a JSON column",
        );
        assert_eq!(record.data.get("count"), Some(&serde_json::json!(3)));
    }

    /// The scalar decoders the aggregate paths use: `run_scalar_i64` /
    /// `run_scalar_f64` read a one-column row whose column the shared builders
    /// alias themselves, so the value has to be taken positionally.
    #[wasm_bindgen_test]
    fn an_aggregate_row_yields_its_single_column_whatever_it_is_aliased_as() {
        assert_eq!(scalar_i64(Some(serde_json::json!({"cnt": 7}))), 7);
        assert_eq!(scalar_i64(None), 0);
        assert_eq!(scalar_f64(Some(serde_json::json!({"total": 2.5}))), 2.5);
        assert_eq!(scalar_f64(None), 0.0);
    }

    /// A D1 service over `db` with a fresh invocation count and the Workers
    /// Paid query limit, for the tests that are not about the budget.
    fn service(db: D1Database, strict_schema: bool, binding: &str) -> D1DatabaseService {
        D1DatabaseService::new(db, strict_schema, binding, D1QueryCount::new(), 1000)
    }

    /// A `D1Database` that is never queried.
    ///
    /// `unchecked_into` only re-types the `JsValue`; it calls nothing on it.
    /// The tests below read `DbExec::strict_schema`, which is plain Rust state
    /// on the adapter (`AtomicBool`), so the `undefined` handle is never
    /// dereferenced. Constructing a *usable* one needs a workerd D1 binding,
    /// which neither this runner nor CI has — see the module note above and
    /// `conformance.rs`.
    fn never_queried_handle() -> D1Database {
        wasm_bindgen::JsCast::unchecked_into::<D1Database>(JsValue::undefined())
    }

    /// A D1 handle whose every statement's `first()` rejects with the error
    /// D1 raises for a table that does not exist — its message as D1 spells
    /// it, with `cause` attached when `cause` is `Some`, since D1 sets one on
    /// some rejections and not on others — and whose `all()` (the
    /// introspection reads) answers no rows, as SQLite's `PRAGMA table_info`
    /// does for a missing table. The object is structural: worker-rs calls
    /// `prepare`, `bind`, `first` and `all` on it by name, as on a real
    /// binding.
    fn missing_table_handle(cause: Option<&str>) -> D1Database {
        let handle = js_sys::Function::new_with_args(
            "cause",
            "const stmt = {
                bind() { return stmt; },
                first() {
                    const message =
                        'D1_ERROR: no such table: impresspress__d1test__never_created: SQLITE_ERROR';
                    return Promise.reject(
                        cause === undefined ? new Error(message) : new Error(message, { cause }),
                    );
                },
                all() { return Promise.resolve({ results: [], success: true, meta: {} }); },
            };
            return { prepare() { return stmt; } };",
        )
        .call1(&JsValue::NULL, &cause.map_or(JsValue::UNDEFINED, JsValue::from_str))
        .expect("the fake binding builds");
        wasm_bindgen::JsCast::unchecked_into::<D1Database>(handle)
    }

    /// Under STRICT_SCHEMA (every generated `wrangler.toml`) a `get` against a
    /// table that does not exist reaches D1, which fails the statement. That
    /// failure is a fault — `Internal`, as native SQLite and PostgreSQL report
    /// it — and never `NotFound`, which `crud::get_record` would answer as the
    /// caller's 404 ("no such row") when nothing could be read at all.
    #[wasm_bindgen_test]
    async fn a_get_against_a_missing_table_is_a_fault_not_an_absent_row() {
        for cause in [
            Some("no such table: impresspress__d1test__never_created: SQLITE_ERROR"),
            None,
        ] {
            forget_isolate_schema();
            let svc = service(missing_table_handle(cause), true, "DB");
            let err = DatabaseService::get(&svc, "impresspress__d1test__never_created", "any-id")
                .await
                .expect_err("the statement fails");
            assert!(
                matches!(err, DatabaseError::Internal(ref text) if text.contains("no such table")),
                "cause {cause:?}: a missing table must be Internal, got {err:?}"
            );
        }
    }

    /// **Fails with a per-service cache**: every D1 service in an isolate has
    /// to answer with the same cache, because each of them is built per
    /// request (`warm_request_services`) over the same database, and
    /// `D1ConfigSource::snapshot`'s paged `list` would otherwise pay a
    /// primary-key round trip on every request.
    #[wasm_bindgen_test]
    fn every_d1_service_in_an_isolate_shares_one_schema_cache() {
        forget_isolate_schema();
        let request_one = service(never_queried_handle(), true, "DB");
        let request_two = service(never_queried_handle(), true, "DB");
        let cache = DbExec::schema_cache(&request_one).expect("the D1 backend keeps a cache");
        let other = DbExec::schema_cache(&request_two).expect("so does every other handle");
        assert!(std::ptr::eq(cache, other), "one isolate, one cache");

        cache.set_primary_key_if_gen("shared_t", vec!["id".into()], cache.generation());
        assert_eq!(
            other.primary_key("shared_t"),
            Some(vec!["id".to_string()]),
            "a key one request learned is served to the next"
        );
    }

    /// A `D1Database` double for the table-exists probe: `prepare(sql)` →
    /// `bind(args)` → `first()` answers one row, `{"present": 0|1}`, which is
    /// what [`introspect::build_table_exists`] asks for and `scalar_i64` reads
    /// positionally. `present` is read when `first()` is called, so a test can
    /// flip it to model a migration landing between two probes; `probes`
    /// counts the round trips, which is what tells a cached answer from a
    /// re-read.
    ///
    /// The other tests here use an `undefined` handle, which is enough while
    /// nothing calls it. This one has to call it: the behaviour under test is
    /// what the guard does with the probe's answer.
    fn scripted_d1(
        present: Rc<std::cell::Cell<bool>>,
        probes: Rc<std::cell::Cell<u32>>,
    ) -> D1Database {
        use wasm_bindgen::{closure::Closure, JsCast};

        let statement = js_sys::Object::new();
        let first = Closure::<dyn Fn(JsValue) -> js_sys::Promise>::new(move |_col: JsValue| {
            probes.set(probes.get() + 1);
            let row = js_sys::Object::new();
            js_sys::Reflect::set(
                &row,
                &JsValue::from_str("present"),
                &JsValue::from(u32::from(present.get())),
            )
            .expect("set present");
            js_sys::Promise::resolve(&JsValue::from(row))
        });
        js_sys::Reflect::set(
            &statement,
            &JsValue::from_str("first"),
            first.as_ref().unchecked_ref(),
        )
        .expect("set first");
        first.forget();

        let bound = statement.clone();
        let bind = Closure::<dyn Fn(JsValue) -> JsValue>::new(move |_args: JsValue| {
            JsValue::from(bound.clone())
        });
        js_sys::Reflect::set(
            &statement,
            &JsValue::from_str("bind"),
            bind.as_ref().unchecked_ref(),
        )
        .expect("set bind");
        bind.forget();

        let db = js_sys::Object::new();
        let prepared = statement;
        let prepare = Closure::<dyn Fn(JsValue) -> JsValue>::new(move |_sql: JsValue| {
            JsValue::from(prepared.clone())
        });
        js_sys::Reflect::set(
            &db,
            &JsValue::from_str("prepare"),
            prepare.as_ref().unchecked_ref(),
        )
        .expect("set prepare");
        prepare.forget();

        JsValue::from(db).unchecked_into::<D1Database>()
    }

    /// A table that does not exist is **not** memoized as missing, so the
    /// migration that creates it is seen by the next operation rather than by
    /// the next runtime rebuild.
    ///
    /// The shared `DbExec::table_present_for_op` is what this adapter runs,
    /// so this is a guard on that default as D1 uses it: the cache here is
    /// isolate-scoped, and a memoized "missing" would make a `list` against
    /// the table answer an empty `RecordList` — a silent wrong answer, for as
    /// long as the isolate lives. A positive is still memoized: nothing drops
    /// a table at runtime on D1.
    #[wasm_bindgen_test]
    async fn a_missing_table_is_re_probed_until_it_appears() {
        forget_isolate_schema();
        let present = Rc::new(std::cell::Cell::new(false));
        let probes = Rc::new(std::cell::Cell::new(0u32));
        let svc = service(
            scripted_d1(Rc::clone(&present), Rc::clone(&probes)),
            false,
            "DB",
        );

        assert_eq!(
            DbExec::table_present_for_op(&svc, "later_t").await.ok(),
            Some(false)
        );
        assert!(
            !DbExec::schema_cache(&svc)
                .expect("a cache")
                .table_known_present("later_t"),
            "a missing table must leave no memoized fact behind"
        );

        // The migration lands out of band.
        present.set(true);
        assert_eq!(
            DbExec::table_present_for_op(&svc, "later_t").await.ok(),
            Some(true),
            "the next operation must see the table, not the isolate's rebuild"
        );
        assert_eq!(probes.get(), 2, "both calls probed");

        // …and the positive IS memoized, so the probes stop there.
        assert_eq!(
            DbExec::table_present_for_op(&svc, "later_t").await.ok(),
            Some(true)
        );
        assert_eq!(
            probes.get(),
            2,
            "a memoized `present` is served without a probe"
        );
    }

    /// A second D1 binding is a second database: it must not be answered from
    /// the first's memoized schema, where one table name can mean a different
    /// shape.
    #[wasm_bindgen_test]
    fn a_second_binding_is_not_answered_from_the_firsts_schema() {
        forget_isolate_schema();
        let primary = service(never_queried_handle(), true, "DB");
        let cache = DbExec::schema_cache(&primary).expect("a cache");
        cache.set_primary_key_if_gen("shared_name", vec!["id".into()], cache.generation());

        let secondary = service(never_queried_handle(), true, "ARCHIVE_DB");
        assert_eq!(
            DbExec::schema_cache(&secondary)
                .expect("a cache")
                .primary_key("shared_name"),
            None,
            "one binding's schema must never answer for another's"
        );
    }

    /// The cache outlives a request but not a runtime rebuild: that rebuild is
    /// what follows a deploy or a migration run in another isolate, and
    /// `runtime_cache::store` calls this.
    #[wasm_bindgen_test]
    fn a_rebuild_forgets_what_the_isolate_had_memoized() {
        forget_isolate_schema();
        let before = service(never_queried_handle(), true, "DB");
        let cache = DbExec::schema_cache(&before).expect("a cache");
        cache.set_primary_key_if_gen("rebuilt_t", vec!["id".into()], cache.generation());
        let next_request = service(never_queried_handle(), true, "DB");
        assert_eq!(
            DbExec::schema_cache(&next_request)
                .expect("a cache")
                .primary_key("rebuilt_t"),
            Some(vec!["id".to_string()]),
            "the memoized key outlives the request that learned it"
        );

        forget_isolate_schema();

        let after = service(never_queried_handle(), true, "DB");
        assert_eq!(
            DbExec::schema_cache(&after)
                .expect("a cache")
                .primary_key("rebuilt_t"),
            None,
            "a rebuilt isolate re-introspects rather than trusting the old schema"
        );
    }

    /// The verdict a D1 service is *born* with is the one the executor reads.
    ///
    /// This is what covers the two services that never reach a lifecycle
    /// `Init` — the audit-row write handle in `run_with_config` and
    /// `build_runtime`'s pre-`Init` `block_settings` read. Both are built and
    /// dropped inside one request, so `set_strict_schema` is never called on
    /// them and their `SchemaCache` never warms: with strict off,
    /// `create_many`'s `DbExec::ensure_data_columns` introspects on every
    /// single drain.
    #[wasm_bindgen_test]
    fn a_d1_service_is_born_with_the_deploys_strict_schema_verdict() {
        let strict = service(never_queried_handle(), true, "DB");
        assert!(
            DbExec::strict_schema(&strict),
            "a service constructed with STRICT_SCHEMA on must already skip the \
             table-exists and column introspection — nothing calls \
             `set_strict_schema` on the drain or pre-Init handles",
        );

        let lax = service(never_queried_handle(), false, "DB");
        assert!(
            !DbExec::strict_schema(&lax),
            "and a service constructed with it off must still introspect",
        );
    }

    /// `Init` must still be able to speak. `handle_lifecycle` calls
    /// `set_strict_schema` on the runtime's own service after construction; on
    /// Cloudflare it reads the same var, so it normally re-affirms the seeded
    /// value — but the setter has to remain the authority, not be shadowed by
    /// the constructor.
    #[wasm_bindgen_test]
    fn lifecycle_init_still_overrides_the_constructed_verdict() {
        let svc = service(never_queried_handle(), false, "DB");
        DatabaseService::set_strict_schema(&svc, true);
        assert!(DbExec::strict_schema(&svc));

        DatabaseService::set_strict_schema(&svc, false);
        assert!(!DbExec::strict_schema(&svc));
    }

    /// What a [`batching_d1`] double's `batch()` does with what it is handed,
    /// and what a lone statement's `run()` does.
    #[derive(Clone, Copy)]
    enum BatchAnswer {
        /// Resolve with one successful result per statement: `changes: 1`, and
        /// a `RETURNING` statement's row echoed back as `{"id": "row-<n>"}`.
        Succeed,
        /// Reject the way D1 rejects a batch whose statement failed: an
        /// `Error` whose message starts with `D1_` and whose `cause` carries
        /// SQLite's text — or, with `None`, no cause at all.
        Reject {
            message: &'static str,
            cause: Option<&'static str>,
        },
    }

    impl BatchAnswer {
        /// The promise a lone `run()` returns: one successful result with
        /// `changes: 1`, or the rejection. A rejected `batch()` is the same
        /// rejection.
        fn settle_run(self) -> js_sys::Promise {
            match self {
                BatchAnswer::Succeed => {
                    let result = js_sys::Object::new();
                    js_sys::Reflect::set(&result, &JsValue::from_str("success"), &JsValue::TRUE)
                        .expect("set success");
                    let meta = js_sys::Object::new();
                    js_sys::Reflect::set(&meta, &JsValue::from_str("changes"), &JsValue::from(1))
                        .expect("set changes");
                    js_sys::Reflect::set(&result, &JsValue::from_str("meta"), &meta)
                        .expect("set meta");
                    js_sys::Promise::resolve(&JsValue::from(result))
                }
                BatchAnswer::Reject { message, cause } => {
                    let error = js_sys::Error::new(message);
                    if let Some(cause) = cause {
                        error.set_cause(&js_sys::Error::new(cause));
                    }
                    js_sys::Promise::reject(&JsValue::from(error))
                }
            }
        }
    }

    /// A `D1Database` double for the batch path: `prepare(sql)` → a statement
    /// that remembers its SQL, `bind(...)` → the same statement, and
    /// `batch(statements)` → [`BatchAnswer`], as is a lone statement's
    /// `run()` (the single-row `create`). `batches` records the SQL of
    /// every statement of every `batch()` call, so a test can tell one
    /// round trip of N statements from N round trips.
    fn batching_d1(
        answer: BatchAnswer,
        batches: Rc<std::cell::RefCell<Vec<Vec<String>>>>,
    ) -> D1Database {
        batching_d1_with_columns(answer, batches, &[])
    }

    /// [`batching_d1`] whose lone-statement `all()` — the executor's column
    /// list, `SELECT name, type AS decl_type FROM pragma_table_info(?1)` —
    /// answers `columns`, each declared `TEXT`, as a migrated table would.
    fn batching_d1_with_columns(
        answer: BatchAnswer,
        batches: Rc<std::cell::RefCell<Vec<Vec<String>>>>,
        columns: &'static [&'static str],
    ) -> D1Database {
        use wasm_bindgen::{closure::Closure, JsCast};

        let db = js_sys::Object::new();
        let prepare = Closure::<dyn Fn(JsValue) -> JsValue>::new(move |sql: JsValue| {
            let statement = js_sys::Object::new();
            js_sys::Reflect::set(&statement, &JsValue::from_str("sql"), &sql).expect("set sql");
            let this = statement.clone();
            let bind = Closure::<dyn Fn() -> JsValue>::new(move || JsValue::from(this.clone()));
            js_sys::Reflect::set(
                &statement,
                &JsValue::from_str("bind"),
                bind.as_ref().unchecked_ref(),
            )
            .expect("set bind");
            bind.forget();
            // A lone statement's `all()`: the executor's one read of a table's
            // columns and their declared types (which hold JSON). It answers
            // `columns`, all `TEXT`, so every value is written and read as it
            // is.
            let all = Closure::<dyn Fn() -> js_sys::Promise>::new(move || {
                let result = js_sys::Object::new();
                js_sys::Reflect::set(&result, &JsValue::from_str("success"), &JsValue::TRUE)
                    .expect("set success");
                let rows = js_sys::Array::new();
                for column in columns {
                    let row = js_sys::Object::new();
                    js_sys::Reflect::set(
                        &row,
                        &JsValue::from_str("name"),
                        &JsValue::from_str(column),
                    )
                    .expect("set name");
                    js_sys::Reflect::set(
                        &row,
                        &JsValue::from_str("decl_type"),
                        &JsValue::from_str("TEXT"),
                    )
                    .expect("set decl_type");
                    rows.push(&row);
                }
                js_sys::Reflect::set(&result, &JsValue::from_str("results"), &rows)
                    .expect("set results");
                js_sys::Promise::resolve(&JsValue::from(result))
            });
            js_sys::Reflect::set(
                &statement,
                &JsValue::from_str("all"),
                all.as_ref().unchecked_ref(),
            )
            .expect("set all");
            all.forget();
            // A lone statement's `first()`: the executor's one probe of where
            // a created row's id comes from (`introspect::build_id_policy`),
            // issued in strict mode too. It answers `0` — the executor mints
            // the id — as for every `TEXT` primary key impresspress declares.
            let first = Closure::<dyn Fn(JsValue) -> js_sys::Promise>::new(move |_col: JsValue| {
                let row = js_sys::Object::new();
                js_sys::Reflect::set(&row, &JsValue::from_str("id_policy"), &JsValue::from(0))
                    .expect("set id_policy");
                js_sys::Promise::resolve(&JsValue::from(row))
            });
            js_sys::Reflect::set(
                &statement,
                &JsValue::from_str("first"),
                first.as_ref().unchecked_ref(),
            )
            .expect("set first");
            first.forget();
            // A lone statement's `run()`: a single-row write.
            let run = Closure::<dyn Fn() -> js_sys::Promise>::new(move || answer.settle_run());
            js_sys::Reflect::set(
                &statement,
                &JsValue::from_str("run"),
                run.as_ref().unchecked_ref(),
            )
            .expect("set run");
            run.forget();
            JsValue::from(statement)
        });
        js_sys::Reflect::set(
            &db,
            &JsValue::from_str("prepare"),
            prepare.as_ref().unchecked_ref(),
        )
        .expect("set prepare");
        prepare.forget();

        let batch = Closure::<dyn Fn(js_sys::Array) -> js_sys::Promise>::new(
            move |statements: js_sys::Array| {
                let sqls: Vec<String> = statements
                    .iter()
                    .map(|statement| {
                        js_sys::Reflect::get(&statement, &JsValue::from_str("sql"))
                            .expect("sql")
                            .as_string()
                            .expect("sql is text")
                    })
                    .collect();
                batches.borrow_mut().push(sqls.clone());
                match answer {
                    BatchAnswer::Succeed => {
                        let results = js_sys::Array::new();
                        for (n, sql) in sqls.iter().enumerate() {
                            let result = js_sys::Object::new();
                            let set = |key: &str, value: &JsValue| {
                                js_sys::Reflect::set(&result, &JsValue::from_str(key), value)
                                    .expect("set result field");
                            };
                            set("success", &JsValue::TRUE);
                            let meta = js_sys::Object::new();
                            js_sys::Reflect::set(
                                &meta,
                                &JsValue::from_str("changes"),
                                &JsValue::from(1),
                            )
                            .expect("set changes");
                            set("meta", &meta);
                            let rows = js_sys::Array::new();
                            if sql.contains("RETURNING") {
                                let row = js_sys::Object::new();
                                js_sys::Reflect::set(
                                    &row,
                                    &JsValue::from_str("id"),
                                    &JsValue::from_str(&format!("row-{n}")),
                                )
                                .expect("set id");
                                rows.push(&row);
                            }
                            set("results", &rows);
                            results.push(&result);
                        }
                        js_sys::Promise::resolve(&JsValue::from(results))
                    }
                    BatchAnswer::Reject { .. } => answer.settle_run(),
                }
            },
        );
        js_sys::Reflect::set(
            &db,
            &JsValue::from_str("batch"),
            batch.as_ref().unchecked_ref(),
        )
        .expect("set batch");
        batch.forget();

        JsValue::from(db).unchecked_into::<D1Database>()
    }

    fn rows(n: usize) -> Vec<std::collections::HashMap<String, serde_json::Value>> {
        (0..n)
            .map(|i| {
                std::collections::HashMap::from([(
                    "path".to_string(),
                    serde_json::json!(format!("/r/{i}")),
                )])
            })
            .collect()
    }

    /// **`create_many` is one D1 round trip.** The rows reach D1 as ONE
    /// `batch()` of one INSERT each — the call D1 runs as a single implicit
    /// transaction — rather than a `run()` per row. Strict schema is on, as in
    /// production, so no introspection statement joins the batch (the id
    /// policy probe runs on its own, ahead of it).
    #[wasm_bindgen_test]
    async fn create_many_is_one_batch_of_one_insert_per_row() {
        forget_isolate_schema();
        let batches = Rc::new(std::cell::RefCell::new(Vec::new()));
        let svc = service(
            batching_d1(BatchAnswer::Succeed, Rc::clone(&batches)),
            true,
            "DB",
        );

        let inserted = DatabaseService::create_many(&svc, "request_logs", rows(40))
            .await
            .expect("inserted");

        assert_eq!(inserted, 40);
        let batches = batches.borrow();
        assert_eq!(batches.len(), 1, "one round trip, not one per row");
        assert_eq!(batches[0].len(), 40);
        assert!(batches[0].iter().all(|sql| sql.starts_with("INSERT INTO")));
    }

    /// `run_transaction` decodes each statement's result in its own position:
    /// an `Execute` is its `changes`, a `Returning` its rows.
    #[wasm_bindgen_test]
    async fn run_transaction_decodes_each_result_as_its_op_asked() {
        forget_isolate_schema();
        let batches = Rc::new(std::cell::RefCell::new(Vec::new()));
        let svc = service(
            batching_d1(BatchAnswer::Succeed, Rc::clone(&batches)),
            true,
            "DB",
        );
        let results = DbExec::run_transaction(
            &svc,
            &[
                TxOp::Execute {
                    sql: "UPDATE t SET a = 1",
                    params: &[],
                },
                TxOp::Returning {
                    sql: "INSERT INTO t (a) VALUES (2) RETURNING *",
                    params: &[],
                    json: JsonColumns::NONE,
                },
            ],
        )
        .await
        .expect("committed");

        assert!(matches!(results[0], TxResult::Execute(1)), "{results:?}");
        match &results[1] {
            TxResult::Returning(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].id, "row-1");
            }
            other => panic!("expected the RETURNING rows, got {other:?}"),
        }
        assert_eq!(batches.borrow().len(), 1);
    }

    /// **A taken key is `AlreadyExists`, not a fault.** D1 has no error code
    /// to read, only the text of the rejection; the database handler turns
    /// `AlreadyExists` into a 409, `Internal` into a 500. Covered both with
    /// and without the `cause` D1 normally attaches, because worker-rs's
    /// `Display` for a D1 error prints only the cause.
    #[wasm_bindgen_test]
    async fn a_batch_refused_for_a_taken_key_is_already_exists() {
        for cause in [
            Some("UNIQUE constraint failed: t.id: SQLITE_CONSTRAINT"),
            None,
        ] {
            forget_isolate_schema();
            let svc = service(
                batching_d1(
                    BatchAnswer::Reject {
                        message: "D1_ERROR: UNIQUE constraint failed: t.id: SQLITE_CONSTRAINT",
                        cause,
                    },
                    Rc::new(std::cell::RefCell::new(Vec::new())),
                ),
                true,
                "DB",
            );
            let err = DatabaseService::create_many(&svc, "t", rows(2))
                .await
                .expect_err("refused");
            assert!(
                matches!(err, DatabaseError::AlreadyExists(_)),
                "cause {cause:?}: {err:?}"
            );
        }
    }

    /// **The `DatabaseService` contract on D1's single-row write.** A `create`
    /// whose `INSERT` D1 refuses for a taken key is `AlreadyExists` — which
    /// `crud` answers as a 409 without re-reading the key — and a refusal for
    /// anything else stays `Internal`. The rejection comes from the
    /// statement's own `run()`, so this is the path every single-row create
    /// takes, not the batch one above.
    #[wasm_bindgen_test]
    async fn a_create_refused_for_a_taken_key_is_already_exists() {
        let create = |answer: BatchAnswer| async move {
            forget_isolate_schema();
            let svc = service(
                batching_d1(answer, Rc::new(std::cell::RefCell::new(Vec::new()))),
                true,
                "DB",
            );
            let row = std::collections::HashMap::from([
                ("id".to_string(), serde_json::json!("taken")),
                ("path".to_string(), serde_json::json!("/r/0")),
            ]);
            DatabaseService::create(&svc, "t", row).await
        };

        assert!(
            create(BatchAnswer::Succeed).await.is_ok(),
            "the double writes"
        );
        for cause in [
            Some("UNIQUE constraint failed: t.id: SQLITE_CONSTRAINT"),
            None,
        ] {
            let err = create(BatchAnswer::Reject {
                message: "D1_ERROR: UNIQUE constraint failed: t.id: SQLITE_CONSTRAINT",
                cause,
            })
            .await
            .expect_err("refused");
            assert!(
                matches!(err, DatabaseError::AlreadyExists(_)),
                "cause {cause:?}: {err:?}"
            );
        }
        let err = create(BatchAnswer::Reject {
            message: "D1_ERROR: NOT NULL constraint failed: t.path: SQLITE_CONSTRAINT",
            cause: Some("NOT NULL constraint failed: t.path: SQLITE_CONSTRAINT"),
        })
        .await
        .expect_err("refused");
        assert!(matches!(err, DatabaseError::Internal(_)), "{err:?}");
    }

    /// Any other refusal stays a fault.
    #[wasm_bindgen_test]
    async fn a_batch_refused_for_anything_else_is_internal() {
        forget_isolate_schema();
        let svc = service(
            batching_d1(
                BatchAnswer::Reject {
                    message: "D1_ERROR: NOT NULL constraint failed: t.path: SQLITE_CONSTRAINT",
                    cause: Some("NOT NULL constraint failed: t.path: SQLITE_CONSTRAINT"),
                },
                Rc::new(std::cell::RefCell::new(Vec::new())),
            ),
            true,
            "DB",
        );
        let err = DatabaseService::create_many(&svc, "t", rows(2))
            .await
            .expect_err("refused");
        assert!(matches!(err, DatabaseError::Internal(_)), "{err:?}");
    }

    // ── The per-invocation statement budget ─────────────────────────────────

    fn used(svc: &D1DatabaseService) -> u64 {
        match DbExec::statement_budget(svc) {
            Ok(StatementBudget::Limited { used, .. }) => used,
            other => panic!("a D1 service reports a limited budget: {other:?}"),
        }
    }

    /// **Every statement-sending primitive counts what it sends, once.** A
    /// lone statement is one; a `db.batch()` is one per statement; the
    /// table-exists probe and `run_execute_returning` are counted by the
    /// primitive they send through, not a second time on top.
    #[wasm_bindgen_test]
    async fn each_primitive_counts_the_statements_it_sends() {
        forget_isolate_schema();
        let svc = service(
            batching_d1(
                BatchAnswer::Succeed,
                Rc::new(std::cell::RefCell::new(Vec::new())),
            ),
            true,
            "DB",
        );
        let json = JsonColumns::NONE;
        let mut expected = 0;
        let mut step = |what: &str, delta: u64, svc: &D1DatabaseService| {
            expected += delta;
            assert_eq!(used(svc), expected, "after {what}");
        };

        DbExec::run_fetch(&svc, "SELECT 1", &[], json)
            .await
            .expect("fetch");
        step("run_fetch", 1, &svc);
        DbExec::run_fetch_one(&svc, "SELECT 1", &[], json)
            .await
            .expect("fetch one");
        step("run_fetch_one", 1, &svc);
        DbExec::run_execute(&svc, "UPDATE t SET a = 1", &[])
            .await
            .expect("execute");
        step("run_execute", 1, &svc);
        DbExec::run_execute_returning(&svc, "DELETE FROM t RETURNING *", &[], json)
            .await
            .expect("execute returning");
        step("run_execute_returning", 1, &svc);
        DbExec::run_scalar_i64(&svc, "SELECT 1", &[])
            .await
            .expect("scalar i64");
        step("run_scalar_i64", 1, &svc);
        DbExec::run_scalar_f64(&svc, "SELECT 1", &[])
            .await
            .expect("scalar f64");
        step("run_scalar_f64", 1, &svc);
        DbExec::dbx_table_exists(&svc, "t").await.expect("probe");
        step("dbx_table_exists", 1, &svc);
        DbExec::run_batch(
            &svc,
            &[
                BatchOp::Execute {
                    sql: "UPDATE t SET a = 1",
                    params: &[],
                },
                BatchOp::Execute {
                    sql: "UPDATE t SET a = 2",
                    params: &[],
                },
            ],
        )
        .await
        .expect("batch");
        step("run_batch of 2", 2, &svc);
        DbExec::run_transaction(
            &svc,
            &[
                TxOp::Execute {
                    sql: "UPDATE t SET a = 1",
                    params: &[],
                },
                TxOp::Execute {
                    sql: "UPDATE t SET a = 2",
                    params: &[],
                },
                TxOp::Execute {
                    sql: "UPDATE t SET a = 3",
                    params: &[],
                },
            ],
        )
        .await
        .expect("transaction");
        step("run_transaction of 3", 3, &svc);
    }

    /// A statement D1 refuses was still sent, and D1 counts it: the budget
    /// counts it too.
    #[wasm_bindgen_test]
    async fn a_refused_statement_still_counts() {
        forget_isolate_schema();
        let svc = service(
            batching_d1(
                BatchAnswer::Reject {
                    message: "D1_ERROR: NOT NULL constraint failed: t.path: SQLITE_CONSTRAINT",
                    cause: None,
                },
                Rc::new(std::cell::RefCell::new(Vec::new())),
            ),
            true,
            "DB",
        );
        DbExec::run_execute(&svc, "UPDATE t SET a = 1", &[])
            .await
            .expect_err("refused");
        assert_eq!(used(&svc), 1);
    }

    /// **A write the invocation cannot finish never reaches D1.** After the
    /// request has already run queries, a `create_many` that fits D1's limit
    /// but not what this invocation has left is `ResourceExhausted` and sends
    /// no `batch()`: D1 would otherwise run out part-way through the request.
    /// The same write in an invocation that has run nothing goes through as
    /// one batch.
    #[wasm_bindgen_test]
    async fn a_create_many_past_what_the_invocation_has_left_sends_no_batch() {
        const LIMIT: u64 = 10;
        let batches = Rc::new(std::cell::RefCell::new(Vec::new()));
        let queries = D1QueryCount::new();
        forget_isolate_schema();
        let svc = D1DatabaseService::new(
            batching_d1(BatchAnswer::Succeed, Rc::clone(&batches)),
            true,
            "DB",
            queries.clone(),
            LIMIT,
        );
        for _ in 0..4 {
            DatabaseService::query_raw(&svc, "SELECT 1", &[])
                .await
                .expect("an earlier query in this request");
        }
        assert_eq!(queries.sent(), 4);

        let err = DatabaseService::create_many(&svc, "request_logs", rows(6))
            .await
            .expect_err("6 inserts do not fit what is left of 10 after 4");
        assert!(
            matches!(err, DatabaseError::ResourceExhausted(_)),
            "{err:?}"
        );
        assert!(batches.borrow().is_empty(), "D1 was never sent the batch");

        // A new invocation, the same write: it fits, and runs as one batch.
        forget_isolate_schema();
        let fresh = D1DatabaseService::new(
            batching_d1(BatchAnswer::Succeed, Rc::clone(&batches)),
            true,
            "DB",
            D1QueryCount::new(),
            LIMIT,
        );
        DatabaseService::create_many(&fresh, "request_logs", rows(6))
            .await
            .expect("a fresh invocation has room");
        assert_eq!(batches.borrow().len(), 1);
    }

    /// **The audit row always fits.** A request's own services leave the
    /// statements reserved for its audit row unused — every statement past
    /// D1's limit less the reservation is refused before it reaches D1, not
    /// only multi-statement writes — so however the request spends its
    /// budget, the reserved-work handle still writes the row, from a cold
    /// schema cache, within [`AUDIT_ROW_STATEMENTS`] statements. Both with
    /// STRICT_SCHEMA (every generated config) and without it, against a fake
    /// D1 that lists the table's real columns, so a probe added to either
    /// path that the reservation does not cover fails here.
    ///
    /// [`AUDIT_ROW_STATEMENTS`]: impresspress_core::after_response::AUDIT_ROW_STATEMENTS
    #[wasm_bindgen_test]
    async fn a_request_that_spends_its_budget_still_has_room_for_its_audit_row() {
        use impresspress_core::after_response::AUDIT_ROW_STATEMENTS;
        const LIMIT: u64 = 10;
        for strict_schema in [true, false] {
            let batches = Rc::new(std::cell::RefCell::new(Vec::new()));
            let d1 = || {
                batching_d1_with_columns(
                    BatchAnswer::Succeed,
                    Rc::clone(&batches),
                    // `impresspress__admin__request_logs` as the admin block's
                    // migration 001 creates it.
                    &[
                        "id",
                        "flow_id",
                        "method",
                        "path",
                        "status",
                        "status_code",
                        "duration_ms",
                        "error_message",
                        "client_ip",
                        "user_id",
                        "created_at",
                        "updated_at",
                    ],
                )
            };
            let queries = D1QueryCount::new();
            queries.reserve(AUDIT_ROW_STATEMENTS);
            forget_isolate_schema();
            let request = D1DatabaseService::new(d1(), strict_schema, "DB", queries.clone(), LIMIT);
            let ran = spend_everything(&request).await;
            assert_eq!(ran, LIMIT - AUDIT_ROW_STATEMENTS, "strict={strict_schema}");
            assert_eq!(queries.sent(), ran, "the refused statement was not sent");

            let audit = D1DatabaseService::new(
                d1(),
                strict_schema,
                "DB",
                queries.for_reserved_work(),
                LIMIT,
            );
            // The row exactly as the pipeline queues it.
            let row = impresspress_core::platform_state::request_logs::NewRequestLog {
                method: "GET",
                path: "/",
                status_code: 200,
                error_message: "",
                duration_ms: 1,
                client_ip: "203.0.113.1",
                user_id: "",
            }
            .to_data();
            DatabaseService::create_many(
                &audit,
                impresspress_core::platform_state::request_logs::TABLE,
                vec![row],
            )
            .await
            .unwrap_or_else(|e| panic!("strict={strict_schema}: the row fits: {e:?}"));
            assert!(
                queries.sent() - ran <= AUDIT_ROW_STATEMENTS,
                "strict={strict_schema}: the row took {} statements",
                queries.sent() - ran
            );
            assert_eq!(batches.borrow().len(), 1, "the row reached D1");
        }
    }

    /// Run `SELECT 1` through `svc` until it is refused; the statements that
    /// ran. The refusal must be the budget's.
    async fn spend_everything(svc: &D1DatabaseService) -> u64 {
        let mut ran = 0;
        loop {
            match DatabaseService::query_raw(svc, "SELECT 1", &[]).await {
                Ok(_) => ran += 1,
                Err(err) => {
                    assert!(
                        matches!(err, DatabaseError::ResourceExhausted(_)),
                        "{err:?}"
                    );
                    return ran;
                }
            }
            assert!(ran <= 1000, "never refused");
        }
    }

    /// **What the row leaves goes to the deferred tasks.** Once the audit row
    /// is written and the reservation released, the request's own services —
    /// which its deferred tasks run under — may use the invocation's whole
    /// limit less everything sent, the row's statements included, and not a
    /// limit still lowered by the reservation.
    #[wasm_bindgen_test]
    async fn a_released_reservation_goes_back_to_the_request() {
        use impresspress_core::after_response::AUDIT_ROW_STATEMENTS;
        const LIMIT: u64 = 10;
        let batches = Rc::new(std::cell::RefCell::new(Vec::new()));
        let queries = D1QueryCount::new();
        queries.reserve(AUDIT_ROW_STATEMENTS);
        forget_isolate_schema();
        let request = D1DatabaseService::new(
            batching_d1(BatchAnswer::Succeed, Rc::clone(&batches)),
            true,
            "DB",
            queries.clone(),
            LIMIT,
        );
        let spent = spend_everything(&request).await;
        let audit = D1DatabaseService::new(
            batching_d1(BatchAnswer::Succeed, Rc::clone(&batches)),
            true,
            "DB",
            queries.for_reserved_work(),
            LIMIT,
        );
        DatabaseService::create_many(&audit, "request_logs", rows(1))
            .await
            .expect("the row fits");
        let row = queries.sent() - spent;
        assert!(row < AUDIT_ROW_STATEMENTS, "a room left over for this test");

        queries.release_reservation();
        let task = spend_everything(&request).await;
        assert_eq!(
            task,
            LIMIT - spent - row,
            "the task may use everything the request and its row left"
        );
    }

    /// **Two requests interleaved in one isolate count separately.** Each
    /// invocation's D1 services share that invocation's [`D1QueryCount`] and
    /// no other: a request is not refused for statements a concurrent request
    /// in the same isolate sent. Their awaits interleave, as an isolate
    /// serving two requests runs them.
    #[wasm_bindgen_test]
    async fn two_interleaved_invocations_do_not_share_a_count() {
        forget_isolate_schema();
        let d1 = || {
            batching_d1(
                BatchAnswer::Succeed,
                Rc::new(std::cell::RefCell::new(Vec::new())),
            )
        };
        let request_a = service(d1(), true, "DB");
        let request_b = service(d1(), true, "DB");

        async fn run(
            svc: &D1DatabaseService,
            name: &'static str,
            statements: usize,
            order: &std::cell::RefCell<Vec<&'static str>>,
        ) {
            for _ in 0..statements {
                DatabaseService::query_raw(svc, "SELECT 1", &[])
                    .await
                    .expect("query");
                order.borrow_mut().push(name);
            }
        }
        let order = std::cell::RefCell::new(Vec::new());
        futures::future::join(
            run(&request_a, "a", 3, &order),
            run(&request_b, "b", 5, &order),
        )
        .await;

        assert!(
            order.borrow().windows(2).any(|pair| pair[0] != pair[1]),
            "the two requests' statements interleaved: {:?}",
            order.borrow()
        );
        assert_eq!(used(&request_a), 3, "request A counts only its own");
        assert_eq!(used(&request_b), 5, "request B counts only its own");
    }
}
