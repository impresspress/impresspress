//! Bridge-boundary parameter/row codec for the browser sql.js bridge edge.
//!
//! `params_to_js`/`rows_from_js`/`parse_rows`/`empty_params` sit right at the
//! wasm-bindgen boundary (they build/consume `JsValue`s via
//! `serde_wasm_bindgen`) and are `#[cfg(target_arch = "wasm32")]`-gated: their
//! only callers (`database.rs`, `vector/service.rs`) are themselves wasm32-only
//! modules, and a `JsValue` only behaves like a real JS value under
//! `wasm32-unknown-unknown` anyway, so there is nothing for a host test to
//! exercise. `coerce_param`/`build_records`/`first_scalar` stay pure
//! `serde_json`, with no `JsValue` involved, so those compile on the host
//! (this module is pulled in there under `cfg(test)` — see `lib.rs`) and keep
//! ordinary host-run `#[test]`s below.
//!
//! Shared by both `database.rs` (the `DbExec` primitives backing the generic
//! `DatabaseService`) and `vector/service.rs` (which drives `bridge::db_*`
//! directly for its own hand-rolled vector-index SQL) — both cross the exact
//! same sql.js bridge boundary and both used to hand-roll their own
//! JSON-string encode/decode before this was centralized here.

use std::collections::HashMap;

use wafer_core::interfaces::database::service::Record;
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

/// Decode the JS array of plain row objects `bridge::db_query_raw` resolves
/// (NOT a JSON string) into `Vec<serde_json::Value>` — one JSON object per
/// row, keyed by column name. Shared by `parse_rows` below (`database.rs`'s
/// `Record`-shaped path) and `vector/service.rs`'s raw-row callers, which
/// need the plain per-column value shape without `Record`'s id/data split.
#[cfg(target_arch = "wasm32")]
pub(crate) fn rows_from_js(value: JsValue) -> Result<Vec<serde_json::Value>, String> {
    serde_wasm_bindgen::from_value(value).map_err(|e| format!("decode rows: {e}"))
}

/// Decode `bridge::db_query_raw`'s resolved value straight into
/// `Vec<Record>`.
#[cfg(target_arch = "wasm32")]
pub(crate) fn parse_rows(value: JsValue) -> Result<Vec<Record>, String> {
    build_records(rows_from_js(value)?)
}

/// Pure row-object → `Record` conversion. JSON-looking TEXT columns (sql.js
/// stores JSON as TEXT) are re-parsed back into structured values. Split out
/// from `parse_rows` so this logic — the only part of the decode path that
/// isn't just "ask serde_wasm_bindgen to do it" — stays host-testable
/// without a real `JsValue`.
pub(crate) fn build_records(rows: Vec<serde_json::Value>) -> Result<Vec<Record>, String> {
    let mut records = Vec::with_capacity(rows.len());
    for row in rows {
        let serde_json::Value::Object(obj) = row else {
            return Err("expected row object".to_string());
        };

        let mut data: HashMap<String, serde_json::Value> = HashMap::new();
        let mut id = String::new();

        for (k, v) in obj {
            let parsed = match &v {
                serde_json::Value::String(s)
                    if (s.starts_with('{') && s.ends_with('}'))
                        || (s.starts_with('[') && s.ends_with(']')) =>
                {
                    serde_json::from_str(s).unwrap_or(v.clone())
                }
                other => other.clone(),
            };

            if k == "id" {
                id = match &parsed {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    _ => String::new(),
                };
            }
            data.insert(k, parsed);
        }

        records.push(Record { id, data });
    }

    Ok(records)
}

/// The first scalar value of a single-column aggregate row, regardless of its
/// alias (the shared builders alias `COUNT`/`SUM` columns). `id` is stripped
/// into `Record.id` by `build_records`; a pure scalar query never names a
/// column `id`, so the remaining-data map carries the value.
pub(crate) fn first_scalar(records: Vec<Record>) -> Option<serde_json::Value> {
    records
        .into_iter()
        .next()
        .and_then(|r| r.data.into_iter().next().map(|(_, v)| v))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ── build_records ─────────────────────────────────────────────────────────

    fn rows_from_json(json: &str) -> Vec<serde_json::Value> {
        serde_json::from_str(json).expect("valid JSON array fixture")
    }

    #[test]
    fn build_records_extracts_id_and_data() {
        let rows = rows_from_json(r#"[{"id":"abc","name":"Bob","age":3}]"#);
        let recs = build_records(rows).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].id, "abc");
        assert_eq!(recs[0].data.get("name").unwrap(), &serde_json::json!("Bob"));
        assert_eq!(recs[0].data.get("age").unwrap(), &serde_json::json!(3));
    }

    #[test]
    fn build_records_reparses_json_text_columns() {
        // sql.js stores JSON columns as TEXT; build_records restores structure.
        let rows = rows_from_json(r#"[{"id":"1","meta":"{\"k\":\"v\"}","tags":"[1,2]"}]"#);
        let recs = build_records(rows).unwrap();
        assert_eq!(
            recs[0].data.get("meta").unwrap(),
            &serde_json::json!({"k":"v"})
        );
        assert_eq!(
            recs[0].data.get("tags").unwrap(),
            &serde_json::json!([1, 2])
        );
    }

    #[test]
    fn build_records_numeric_id_stringified() {
        let recs = build_records(rows_from_json(r#"[{"id":7,"v":"x"}]"#)).unwrap();
        assert_eq!(recs[0].id, "7");
    }

    #[test]
    fn build_records_non_json_text_left_alone() {
        // A plain string that doesn't look like JSON must stay a string.
        let recs = build_records(rows_from_json(r#"[{"id":"1","note":"hello world"}]"#)).unwrap();
        assert_eq!(
            recs[0].data.get("note").unwrap(),
            &serde_json::json!("hello world")
        );
    }

    #[test]
    fn build_records_empty_is_empty() {
        assert!(build_records(rows_from_json("[]")).unwrap().is_empty());
    }

    #[test]
    fn build_records_rejects_non_object_row() {
        assert!(build_records(rows_from_json("[1,2]")).is_err());
    }

    // ── first_scalar ──────────────────────────────────────────────────────────

    #[test]
    fn first_scalar_takes_aliased_count_column() {
        // `SELECT COUNT(*) AS cnt` → one row, one column named `cnt`.
        let recs = build_records(rows_from_json(r#"[{"cnt":5}]"#)).unwrap();
        assert_eq!(first_scalar(recs), Some(serde_json::json!(5)));
    }

    #[test]
    fn first_scalar_takes_aliased_sum_column() {
        let recs = build_records(rows_from_json(r#"[{"total":12.5}]"#)).unwrap();
        assert_eq!(first_scalar(recs), Some(serde_json::json!(12.5)));
    }

    #[test]
    fn first_scalar_empty_is_none() {
        assert_eq!(
            first_scalar(build_records(rows_from_json("[]")).unwrap()),
            None
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

    /// Lazily added columns are always `TEXT` on SQLite (D1 + sql.js), matching
    /// the historical lazy column-add type both backends hand-rolled.
    #[test]
    fn lazy_column_add_is_text_on_sqlite() {
        let stmt = ddl::build_add_text_column("items", "newcol", SQLITE);
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
