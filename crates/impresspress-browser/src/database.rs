//! Browser-side `DatabaseService` backed by sql.js via the JS bridge.
//!
//! The browser backend implements only the [`DbExec`] execution *primitives*
//! (synchronous `bridge::db_query_raw` / `bridge::db_exec_raw`, marshaling
//! params/rows across the bridge as structured `JsValue`s via
//! `db_codec`/`serde_wasm_bindgen` — no JSON-string round trip). All
//! `get/list/count/sum/create/update/delete` orchestration — filter/IN
//! expansion, sorted-key INSERT/UPDATE construction, lazy column-add,
//! table-exists guards — is inherited from the shared `wafer-core` [`DbExec`]
//! defaults, identical to `wafer-block-sqlite`, `wafer-block-postgres`, and the
//! Cloudflare D1 backend.
//!
//! Tables must already exist via the owning block's migration files (applied
//! at `lifecycle(Init)`); the shared `ensure_data_columns`/`ensure_query_columns`
//! add only missing *columns* (always `TEXT` on SQLite) on demand.
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
//! single statement. See `with_flush`'s doc comment for the full contract,
//! including why the flush still happens when the wrapped operation itself
//! returns an error.

use std::collections::HashMap;

use wafer_block::db::{Filter, ListOptions};
use wafer_core::interfaces::database::{
    exec::DbExec,
    service::{
        AggregateSpec, Column, DatabaseError, DatabaseService, Record, RecordList, Table,
        UpsertSpec,
    },
};
use wafer_sql_utils::{introspect, Backend};

use crate::{bridge, db_codec};

/// Browser-side DatabaseService backed by sql.js via the JS bridge.
pub struct BrowserDatabaseService;

// SAFETY: `BrowserDatabaseService` is a unit struct with no shared state.
// wasm32-unknown-unknown has no threads, so the `Send`/`Sync` bounds
// required by `Arc<dyn DatabaseService>` are satisfied trivially — no
// cross-thread aliasing or data races are possible.
unsafe impl Send for BrowserDatabaseService {}
unsafe impl Sync for BrowserDatabaseService {}

impl BrowserDatabaseService {
    /// Run a mutating `op`, then flush the sql.js DB to OPFS exactly once —
    /// this is the coalescing point described in the module doc comment.
    ///
    /// Flushes even when `op` itself resolves to `Err`: the shared
    /// `DbExec` defaults can issue more than one statement per logical
    /// operation (e.g. `create`'s lazy column-add ALTER before its INSERT),
    /// so an operation that ultimately fails may still have mutated the
    /// in-memory sql.js DB. Skipping the flush in that case would silently
    /// discard an already-applied statement until some *later* mutation
    /// happens to flush it — an unnecessary, avoidable durability gap.
    ///
    /// Outcome precedence:
    /// - `op` succeeds, flush succeeds → `Ok` (the common case: durable).
    /// - `op` succeeds, flush fails → `Err` (the flush error). The mutation
    ///   is only sitting in memory at this point (quota exceeded, OPFS
    ///   permission revoked, etc.) — reporting success here would tell the
    ///   caller data is durable when a Service Worker eviction could lose
    ///   it, so this must surface as a failure.
    /// - `op` fails (regardless of flush outcome) → `Err` (the operation's
    ///   own error) — more specific/actionable than whatever the flush
    ///   attempt did; we still attempt the flush as a best-effort capture
    ///   of any partial writes the failed operation may have already made.
    async fn with_flush<T>(
        &self,
        op: impl std::future::Future<Output = Result<T, DatabaseError>>,
    ) -> Result<T, DatabaseError> {
        let result = op.await;
        let flush = bridge::dbFlush().await.map(|_| ()).map_err(|e| {
            DatabaseError::Internal(format!("flush to OPFS: {}", bridge::describe(&e)))
        });
        match (result, flush) {
            (Ok(v), Ok(())) => Ok(v),
            (Ok(_), Err(flush_err)) => Err(flush_err),
            (Err(op_err), _) => Err(op_err),
        }
    }
}

// ─── DbExec primitives — the only backend-specific execution code ─────────────

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl DbExec for BrowserDatabaseService {
    const BACKEND: Backend = Backend::Sqlite;

    async fn run_fetch(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        let params_js = db_codec::params_to_js(params).map_err(DatabaseError::Internal)?;
        let value = bridge::db_query_raw(sql, params_js)
            .map_err(|e| DatabaseError::Internal(format!("sql exec: {e:?}")))?;
        db_codec::parse_rows(value).map_err(DatabaseError::Internal)
    }

    async fn run_fetch_one(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Record, DatabaseError> {
        let records = self.run_fetch(sql, params).await?;
        records.into_iter().next().ok_or(DatabaseError::NotFound)
    }

    async fn run_execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let params_js = db_codec::params_to_js(params).map_err(DatabaseError::Internal)?;
        let rows_modified = bridge::db_exec_raw(sql, params_js)
            .map_err(|e| DatabaseError::Internal(format!("sql exec: {e:?}")))?;
        // NOTE: deliberately no `bridge::dbFlush()` here — flushing is
        // coalesced at the `DatabaseService` method boundary via
        // `with_flush`. See the module doc comment.
        Ok(rows_modified as i64)
    }

    async fn run_scalar_i64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let records = self.run_fetch(sql, params).await?;
        Ok(db_codec::first_scalar(records)
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
            .unwrap_or(0))
    }

    async fn run_scalar_f64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<f64, DatabaseError> {
        let records = self.run_fetch(sql, params).await?;
        Ok(db_codec::first_scalar(records)
            .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|i| i as f64)))
            .unwrap_or(0.0))
    }

    async fn dbx_table_exists(&self, table: &str) -> Result<bool, DatabaseError> {
        let (sql, params) = introspect::build_table_exists(table, Backend::Sqlite);
        Ok(self.run_scalar_i64(&sql, &params).await? > 0)
    }
}

// ─── DatabaseService — forwards into the shared DbExec defaults ───────────────
//
// Every method that can mutate the sql.js DB wraps its `DbExec` default call
// in `with_flush` so exactly one OPFS flush happens per logical call,
// regardless of how many `run_execute` statements the shared default issued
// internally. Read-only methods (`get`/`list`/`count`/`sum`/`query_raw`/
// `take_where`/`aggregate`) forward directly — nothing to flush.

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl DatabaseService for BrowserDatabaseService {
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
        DbExec::take_where(self, collection, filters).await
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

    async fn upsert(&self, collection: &str, spec: UpsertSpec) -> Result<i64, DatabaseError> {
        self.with_flush(DbExec::upsert(self, collection, spec))
            .await
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
        data: HashMap<String, serde_json::Value>,
    ) -> Result<i64, DatabaseError> {
        self.with_flush(DbExec::update_where_count(self, collection, filters, data))
            .await
    }

    // --- Schema management ---

    async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
        self.with_flush(async {
            // Blocks own their schema via migration files; runtime callers
            // may still ask for a one-off table. Build the DDL via the
            // shared ddl builders and run it through the execution
            // primitive.
            let create = wafer_sql_utils::ddl::build_create_table(table, Backend::Sqlite)
                .map_err(|e| DatabaseError::Internal(format!("build create table: {e}")))?;
            self.run_execute(&create.sql, &[]).await?;

            let existing = DbExec::get_columns(self, &table.name).await?;
            for col in &table.columns {
                if !existing.contains(&col.name.to_lowercase()) {
                    let alter =
                        wafer_sql_utils::ddl::build_add_column(&table.name, col, Backend::Sqlite);
                    // Best-effort: a duplicate column on re-run is benign.
                    let _ = self.run_execute(&alter.sql, &[]).await;
                }
            }

            for idx in &table.indexes {
                let stmt =
                    wafer_sql_utils::ddl::build_create_index(&table.name, idx, Backend::Sqlite)
                        .map_err(|e| DatabaseError::Internal(format!("build create index: {e}")))?;
                self.run_execute(&stmt.sql, &[]).await?;
            }
            for stmt in wafer_sql_utils::ddl::build_fk_indexes(table, Backend::Sqlite)
                .map_err(|e| DatabaseError::Internal(format!("build FK indexes: {e}")))?
            {
                self.run_execute(&stmt.sql, &[]).await?;
            }
            Ok(())
        })
        .await
    }

    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        DbExec::schema_table_exists(self, name).await
    }

    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
        self.with_flush(async {
            let stmt = wafer_sql_utils::ddl::build_drop_table(name, Backend::Sqlite);
            self.run_execute(&stmt.sql, &[]).await?;
            Ok(())
        })
        .await
    }

    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError> {
        self.with_flush(async {
            let stmt = wafer_sql_utils::ddl::build_add_column(table, column, Backend::Sqlite);
            self.run_execute(&stmt.sql, &[]).await?;
            Ok(())
        })
        .await
    }
}

/// Factory: returns an `Arc<dyn DatabaseService>` backed by the
/// browser's sql.js + OPFS integration. Call after `crate::db_init()`
/// has completed.
pub fn make_database_service() -> std::sync::Arc<dyn DatabaseService> {
    std::sync::Arc::new(BrowserDatabaseService)
}
