//! Bridge-boundary parameter/row codec for the browser sql.js bridge edge.
//!
//! Everything here is *bridge-local*: turning a `serde_json::Value` params
//! slice into the `JsValue` array `bridge::db_exec_raw`/`bridge::db_query_raw`
//! bind positionally, and turning the `{ columns, values }` result those
//! resolve back into rows that keep the statement's column order.
//!
//! What is deliberately NOT here any more is the row → [`Record`] decode
//! policy. That used to be a private `build_records`/`first_scalar` pair —
//! the last of the three private copies of one policy (native SQLite, D1 and
//! this one). It now lives once, upstream, in
//! `wafer_core::interfaces::database::codec`, and `database.rs` calls it
//! directly. Keeping a per-adapter copy is how the D1 backend ended up
//! answering `Value::String` where the other two answered `Value::Object` for
//! the same JSON-in-TEXT column.
//!
//! `params_to_js`/`ordered_rows_from_js`/`rows_from_js`/`empty_params` sit right at the wasm-bindgen
//! boundary (they build/consume `JsValue`s via `serde_wasm_bindgen`) and are
//! `#[cfg(target_arch = "wasm32")]`-gated: their only callers (`database.rs`,
//! `vector/service.rs`) are themselves wasm32-only modules, and a `JsValue`
//! only behaves like a real JS value under `wasm32-unknown-unknown` anyway, so
//! there is nothing for a host test to exercise. `coerce_param` stays pure
//! `serde_json`, with no `JsValue` involved, so it compiles on the host (this
//! module is pulled in there under `cfg(test)` — see `lib.rs`) and keeps its
//! ordinary host-run `#[test]`s below.

use wafer_core::interfaces::database::service::RecordData;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;

/// Map a JSON value to a scalar suitable for embedding in a params array.
/// Arrays and objects are serialized as JSON strings — sql.js (like SQLite)
/// has no native array/object bind type, so these still bind as TEXT,
/// matching the D1 `json_value_to_js` policy.
pub(crate) fn coerce_param(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            serde_json::Value::String(v.to_string())
        }
        other => other.clone(),
    }
}

/// Encode `params` as the structured JS array `bridge::db_exec_raw` /
/// `bridge::db_query_raw` bind positionally — no JSON-string round trip.
/// Each value is `coerce_param`'d first so arrays/objects still bind as JSON
/// text.
///
/// Uses an explicit `Serializer` with `serialize_missing_as_null(true)`
/// rather than the bare `serde_wasm_bindgen::to_value` free function: the
/// default serializer maps `serde_json::Value::Null` (any nullable column,
/// e.g. bootstrap's `deleted_at`) to JS `undefined` (`Value::Null`'s
/// `Serialize` impl calls `serializer.serialize_unit()`, and
/// `Serializer::new()`'s `serialize_missing_as_null` defaults to `false`,
/// so `serialize_unit` returns `JsValue::UNDEFINED`). sql.js's parameter
/// binder switches on `typeof value` and only recognizes
/// `"string"|"number"|"bigint"|"boolean"`, plus an explicit `null === value`
/// check under `"object"` — `"undefined"` matches none of those and throws
/// `Wrong API use : tried to bind a value of an unknown type (undefined).`,
/// which was silently killing the browser admin-bootstrap insert (and any
/// other write with a null column) at the DB bridge. Setting
/// `serialize_missing_as_null(true)` makes `serialize_unit` return
/// `JsValue::NULL` instead, matching the old JSON.stringify/JSON.parse round
/// trip's behavior (JSON has no `undefined`, so `null` always decoded back
/// to a real JS `null`) and what sql.js accepts.
#[cfg(target_arch = "wasm32")]
pub(crate) fn params_to_js(params: &[serde_json::Value]) -> Result<JsValue, String> {
    let coerced: Vec<serde_json::Value> = params.iter().map(coerce_param).collect();
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_missing_as_null(true);
    serde::Serialize::serialize(&coerced, &serializer).map_err(|e| format!("encode params: {e}"))
}

/// The empty bind-params array for a `db_exec_raw`/`db_query_raw` call that
/// binds no `?` placeholders. Built directly via `js_sys::Array` rather than
/// `params_to_js(&[])` — an empty array can't fail to encode, so this avoids
/// a fallible call at every no-params call site.
#[cfg(target_arch = "wasm32")]
pub(crate) fn empty_params() -> JsValue {
    js_sys::Array::new().into()
}

/// What `bridge::db_query_raw` resolves: sql.js's own result shape for one
/// statement — the result's column names in `SELECT` order, and each row as
/// the values in that order. Positional, rather than one JS object per row,
/// so the column order is exact: a JS object enumerates integer-like keys
/// (`SELECT 1`) before the others and keeps one of two same-named columns.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct QueryResult {
    columns: Vec<String>,
    values: Vec<Vec<serde_json::Value>>,
}

impl QueryResult {
    /// Each row as its columns, name → value, in result-column order — the
    /// shape `codec::record_from_columns` takes. A row with a different
    /// number of values than there are columns is not what sql.js produces
    /// and is refused rather than truncated.
    pub(crate) fn into_rows(self) -> Result<Vec<RecordData>, String> {
        let Self { columns, values } = self;
        values
            .into_iter()
            .map(|row| {
                if row.len() != columns.len() {
                    return Err(format!(
                        "decode rows: a row has {} values for {} columns",
                        row.len(),
                        columns.len()
                    ));
                }
                Ok(columns.iter().cloned().zip(row).collect())
            })
            .collect()
    }
}

/// Decode what `bridge::db_query_raw` resolves (a JS `{ columns, values }`,
/// NOT a JSON string) into its rows, each in result-column order. This is
/// the whole of the bridge's decode job: what a row *means* (`Record` id/data
/// split, a JSON column's text parsed, single-column scalar extraction) is
/// the shared codec's, not ours. `database.rs` hands each row to
/// `codec::record_from_columns`.
#[cfg(target_arch = "wasm32")]
pub(crate) fn ordered_rows_from_js(value: JsValue) -> Result<Vec<RecordData>, String> {
    let result: QueryResult =
        serde_wasm_bindgen::from_value(value).map_err(|e| format!("decode rows: {e}"))?;
    result.into_rows()
}

/// [`ordered_rows_from_js`], each row as a JSON object — for callers that
/// only look columns up by name (`vector/service.rs`'s raw-row readers, the
/// scalar accessors). A `serde_json` object does not keep the column order;
/// anything that shows or forwards a row's columns uses
/// [`ordered_rows_from_js`].
#[cfg(target_arch = "wasm32")]
pub(crate) fn rows_from_js(value: JsValue) -> Result<Vec<serde_json::Value>, String> {
    Ok(ordered_rows_from_js(value)?
        .into_iter()
        .map(|row| serde_json::Value::Object(row.into_iter().collect()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── QueryResult ───────────────────────────────────────────────────────────

    fn result(columns: &[&str], values: Vec<Vec<serde_json::Value>>) -> QueryResult {
        QueryResult {
            columns: columns.iter().map(|c| (*c).to_string()).collect(),
            values,
        }
    }

    #[test]
    fn rows_keep_the_select_column_order() {
        // `SELECT b, 1, a`: an integer-like name stays where the SELECT put it.
        let rows = result(
            &["b", "1", "a"],
            vec![vec![
                serde_json::json!("x"),
                serde_json::json!(1),
                serde_json::json!(null),
            ]],
        )
        .into_rows()
        .unwrap();
        let names: Vec<&str> = rows[0].keys().map(String::as_str).collect();
        assert_eq!(names, ["b", "1", "a"]);
        assert_eq!(rows[0]["b"], serde_json::json!("x"));
    }

    #[test]
    fn an_empty_result_has_no_rows() {
        assert!(QueryResult::default().into_rows().unwrap().is_empty());
    }

    #[test]
    fn a_row_that_does_not_match_the_columns_is_refused() {
        let err = result(&["a", "b"], vec![vec![serde_json::json!(1)]])
            .into_rows()
            .unwrap_err();
        assert!(err.contains("1 values for 2 columns"), "{err}");
    }

    // ── coerce_param ──────────────────────────────────────────────────────────

    #[test]
    fn coerce_param_passes_scalars_through() {
        for v in [
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!(42),
            serde_json::json!(2.5),
            serde_json::json!("hello"),
        ] {
            assert_eq!(coerce_param(&v), v);
        }
    }

    #[test]
    fn coerce_param_serializes_arrays_and_objects_as_text() {
        assert_eq!(
            coerce_param(&serde_json::json!([1, 2, 3])),
            serde_json::Value::String("[1,2,3]".to_string())
        );
        assert_eq!(
            coerce_param(&serde_json::json!({"a": 1})),
            serde_json::Value::String("{\"a\":1}".to_string())
        );
    }
}

/// Pin the unified statements both wasm backends now emit through the shared
/// `wafer-sql-utils` builders behind `DbExec` (the two hand-rolled SQLite
/// planners they replaced had already diverged — see the PR drift table). These
/// run on the host; the per-backend `database.rs` only marshals params/rows
/// across its bridge and never builds SQL itself.
#[cfg(test)]
mod planning {
    use wafer_block::db::{Filter, FilterOp};
    use wafer_sql_utils::{aggregate, ddl, query, Backend};

    const SQLITE: Backend = Backend::Sqlite;

    /// `FilterOp::In` over an N-element array expands to N positional
    /// placeholders and binds each element — not the old browser `1=0`
    /// empty-array literal nor the D1 single-`?` fallback.
    #[test]
    fn filter_in_expands_to_one_placeholder_per_element() {
        let filters = vec![Filter {
            field: "status".into(),
            operator: FilterOp::In,
            value: serde_json::json!(["a", "b", "c"]),
        }];
        let stmt = aggregate::build_count("items", &filters, SQLITE);
        assert_eq!(
            stmt.sql,
            r#"SELECT COUNT(*) AS "cnt" FROM "items" WHERE "status" IN (?, ?, ?)"#
        );
        assert_eq!(stmt.values.len(), 3);
    }

    /// INSERT columns/values are emitted in sorted-key order so the prepared
    /// statement is stable across `HashMap` permutations (one cached plan per
    /// table+column-set on the backend).
    #[test]
    fn insert_columns_are_sorted_by_key() {
        let mut pairs = vec![
            ("b_col".to_string(), serde_json::json!(2)),
            ("a_col".to_string(), serde_json::json!(1)),
        ];
        pairs.sort_by(|x, y| x.0.cmp(&y.0));
        let stmt = query::build_insert("items", &pairs, SQLITE);
        assert_eq!(
            stmt.sql,
            r#"INSERT INTO "items" ("a_col", "b_col") VALUES (?, ?)"#
        );
    }

    /// UPDATE … SET pairs are likewise emitted in sorted-key order, WHERE id.
    #[test]
    fn update_by_id_set_clause_is_sorted_by_key() {
        let mut pairs = vec![
            ("b_col".to_string(), serde_json::json!(2)),
            ("a_col".to_string(), serde_json::json!(1)),
        ];
        pairs.sort_by(|x, y| x.0.cmp(&y.0));
        let stmt = query::build_update_by_id("items", "xyz", &pairs, SQLITE);
        assert_eq!(
            stmt.sql,
            r#"UPDATE "items" SET "a_col" = ?, "b_col" = ? WHERE "id" = ?"#
        );
    }

    /// Lazily added columns are always `TEXT` on SQLite (D1 + sql.js),
    /// whatever the value a write carries.
    #[test]
    fn lazy_column_add_is_text_on_sqlite() {
        let stmt =
            ddl::build_add_column_for_value("items", "newcol", &serde_json::json!(42), SQLITE)
                .expect("a plain column name");
        assert_eq!(stmt.sql, r#"ALTER TABLE "items" ADD COLUMN "newcol" TEXT"#);
    }

    /// `get`-by-id and the table-exists probe both bind their argument rather
    /// than interpolating it (the old hand-rolled `format!` planners).
    #[test]
    fn select_by_id_binds_the_id() {
        let stmt = query::build_select_by_id("items", "xyz", SQLITE);
        assert_eq!(stmt.sql, r#"SELECT * FROM "items" WHERE "id" = ?"#);
        assert_eq!(stmt.values.len(), 1);
    }
}
