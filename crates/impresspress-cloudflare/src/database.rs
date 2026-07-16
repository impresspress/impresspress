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
//! ## Lazy column-add
//!
//! Tables themselves must exist before any `create()` — every block ships
//! explicit `migrations/*.sql` applied from the `Init` lifecycle. The shared
//! `DbExec::ensure_data_columns`/`ensure_query_columns` add only *columns* on
//! demand (always `TEXT` on SQLite), matching the native sqlite/postgres
//! backends. Reads against a missing table return empty/NotFound via the
//! `dbx_table_exists` guard the defaults run first.

use wafer_block::db::{Filter, ListOptions};
use wafer_core::interfaces::database::{
    exec::DbExec,
    service::{
        AggregateSpec, Column, DatabaseError, DatabaseService, Record, RecordList, Table,
        UpsertSpec,
    },
};
use wafer_sql_utils::{introspect, Backend};
use wasm_bindgen::JsValue;
use worker::*;

/// Async database service wrapping Cloudflare D1.
pub struct D1DatabaseService {
    db: D1Database,
}

impl D1DatabaseService {
    pub fn new(db: D1Database) -> Self {
        Self { db }
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
// trivially — no cross-thread aliasing or data races can occur.
unsafe impl Send for D1DatabaseService {}
unsafe impl Sync for D1DatabaseService {}

// ---------------------------------------------------------------------------
// DbExec primitives — the only backend-specific execution code.
// ---------------------------------------------------------------------------

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl DbExec for D1DatabaseService {
    const BACKEND: Backend = Backend::Sqlite;

    async fn run_fetch(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        let stmt = self.prepare_bind(sql, params)?;
        let results = stmt.all().await.map_err(db_err)?;
        let rows: Vec<serde_json::Value> = results.results().map_err(db_err)?;
        Ok(rows.into_iter().map(json_to_record).collect())
    }

    async fn run_fetch_one(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Record, DatabaseError> {
        let stmt = self.prepare_bind(sql, params)?;
        let row = match stmt.first::<serde_json::Value>(None).await {
            Ok(row) => row,
            // A `get`-by-id against a not-yet-created table is "not found",
            // matching the native backends' `QueryReturnedNoRows` mapping.
            Err(e) if is_no_such_table(&e.to_string()) => return Err(DatabaseError::NotFound),
            Err(e) => return Err(db_err(e)),
        };
        row.map(json_to_record).ok_or(DatabaseError::NotFound)
    }

    async fn run_execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let result = self
            .prepare_bind(sql, params)?
            .run()
            .await
            .map_err(db_err)?;
        // worker-rs 0.7 exposes D1Result::meta().changes (Option<usize>) for
        // mutations — surface a real rows_affected so the shared defaults can
        // map 0-rows to NotFound on update/delete-by-id.
        let changes = result
            .meta()
            .map_err(db_err)?
            .and_then(|m| m.changes)
            .unwrap_or(0);
        Ok(changes as i64)
    }

    async fn run_scalar_i64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let stmt = self.prepare_bind(sql, params)?;
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
        let row = stmt
            .first::<serde_json::Value>(None)
            .await
            .map_err(db_err)?;
        Ok(scalar_f64(row))
    }

    async fn dbx_table_exists(&self, table: &str) -> Result<bool, DatabaseError> {
        let (sql, params) = introspect::build_table_exists(table, Backend::Sqlite);
        Ok(self.run_scalar_i64(&sql, &params).await? > 0)
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

    async fn upsert(&self, collection: &str, spec: UpsertSpec) -> Result<i64, DatabaseError> {
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

    // --- Schema management: D1 schema is owned by Wrangler migrations ---

    async fn ensure_schema_table(&self, _table: &Table) -> Result<(), DatabaseError> {
        Ok(())
    }

    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        DbExec::schema_table_exists(self, name).await
    }

    async fn schema_drop_table(&self, _name: &str) -> Result<(), DatabaseError> {
        Ok(())
    }

    async fn schema_add_column(&self, _table: &str, _column: &Column) -> Result<(), DatabaseError> {
        Ok(())
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

/// Convert any Display error into a DatabaseError::Internal.
fn db_err(e: impl std::fmt::Display) -> DatabaseError {
    DatabaseError::Internal(e.to_string())
}

/// Whether a D1 error message indicates the target table doesn't exist.
/// D1 surfaces SQLite's `no such table: X` verbatim through the JsValue
/// error; we string-match because the `worker::Error` type doesn't expose
/// SQLite's structured error code.
pub(crate) fn is_no_such_table(msg: &str) -> bool {
    msg.contains("no such table")
}

/// Extract the single scalar column of a `COUNT`/aggregate row as i64.
/// The shared builders alias the scalar column (`build_count` → its own
/// alias), so we take the first numeric value present rather than a fixed key.
fn scalar_i64(row: Option<serde_json::Value>) -> i64 {
    row.and_then(first_scalar)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .unwrap_or(0)
}

/// Extract the single scalar column of a `SUM`/aggregate row as f64.
fn scalar_f64(row: Option<serde_json::Value>) -> f64 {
    row.and_then(first_scalar)
        .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|i| i as f64)))
        .unwrap_or(0.0)
}

/// The first value of a single-column result object, regardless of its alias.
fn first_scalar(row: serde_json::Value) -> Option<serde_json::Value> {
    match row {
        serde_json::Value::Object(map) => map.into_iter().next().map(|(_, v)| v),
        other => Some(other),
    }
}

/// Convert a D1 result row (as JSON) into a Record.
fn json_to_record(val: serde_json::Value) -> Record {
    if let serde_json::Value::Object(mut map) = val {
        let id = map
            .remove("id")
            .and_then(|v| match v {
                serde_json::Value::String(s) => Some(s),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .unwrap_or_default();

        Record {
            id,
            data: map.into_iter().collect(),
        }
    } else {
        Record {
            id: String::new(),
            data: std::collections::HashMap::new(),
        }
    }
}

// Note: unit tests for the pure SQL-planning layer live in `wafer-sql-utils`
// and `wafer-core::interfaces::database::exec` (shared across all SQL
// backends). `impresspress-cloudflare` only compiles on `wasm32-unknown-unknown`
// (the R2/D1 services hold `!Send` JsFutures), so `cargo test
// -p impresspress-cloudflare` errors before reaching any test module. End-to-end
// validation comes from a real CF deploy.
