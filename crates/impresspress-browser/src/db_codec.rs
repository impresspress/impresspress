//! Pure parameter/row codec for the browser sql.js bridge edge.
//!
//! Split out of `database.rs` (which is wasm32-only because it links the JS
//! bridge externs) so these pure `serde_json` <-> bridge conversions compile
//! and unit-test on the host. The `DbExec` primitives in `database.rs` call
//! these to marshal the JSON params/rows the shared `wafer-sql-utils` builders
//! produce across the bridge boundary.

use std::collections::HashMap;

use wafer_core::interfaces::database::service::{DatabaseError, Record};

/// Map a JSON value to a scalar suitable for embedding in a params array.
/// Arrays and objects are serialized as JSON strings — sql.js binds them as
/// TEXT, matching the D1 `json_value_to_js` policy.
pub(crate) fn coerce_param(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            serde_json::Value::String(v.to_string())
        }
        other => other.clone(),
    }
}

/// Serialize params into the JSON array string the bridge functions expect.
/// Each value is `coerce_param`'d first so arrays/objects bind as JSON text.
pub(crate) fn params_to_json(params: &[serde_json::Value]) -> Result<String, DatabaseError> {
    let coerced: Vec<serde_json::Value> = params.iter().map(coerce_param).collect();
    serde_json::to_string(&coerced)
        .map_err(|e| DatabaseError::Internal(format!("encode params: {e}")))
}

/// Parse the JSON array of row objects returned by `db_query_raw` into
/// `Vec<Record>`. JSON-looking TEXT columns (sql.js stores JSON as TEXT) are
/// re-parsed back into structured values.
pub(crate) fn parse_rows(json: &str) -> Result<Vec<Record>, DatabaseError> {
    let rows: Vec<serde_json::Value> = serde_json::from_str(json)
        .map_err(|e| DatabaseError::Internal(format!("parse rows: {e}")))?;

    let mut records = Vec::with_capacity(rows.len());
    for row in rows {
        let serde_json::Value::Object(obj) = row else {
            return Err(DatabaseError::Internal("expected row object".to_string()));
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
/// into `Record.id` by `parse_rows`; a pure scalar query never names a column
/// `id`, so the remaining-data map carries the value.
pub(crate) fn first_scalar(records: Vec<Record>) -> Option<serde_json::Value> {
    records
        .into_iter()
        .next()
        .and_then(|r| r.data.into_iter().next().map(|(_, v)| v))
}

/// Parse the rows-modified count string `db_exec_raw` returns.
pub(crate) fn parse_rows_modified(s: &str) -> i64 {
    s.trim().parse::<i64>().unwrap_or(0)
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

    #[test]
    fn params_to_json_coerces_each_element() {
        // Table-driven: (input params, expected JSON array string).
        let cases: &[(Vec<serde_json::Value>, &str)] = &[
            (vec![], "[]"),
            (
                vec![serde_json::json!("x"), serde_json::json!(1)],
                "[\"x\",1]",
            ),
            // Nested array/object collapse to JSON-text scalars.
            (
                vec![serde_json::json!([1, 2]), serde_json::json!({"k": "v"})],
                "[\"[1,2]\",\"{\\\"k\\\":\\\"v\\\"}\"]",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(&params_to_json(input).unwrap(), expected);
        }
    }

    // ── parse_rows ────────────────────────────────────────────────────────────

    #[test]
    fn parse_rows_extracts_id_and_data() {
        let json = r#"[{"id":"abc","name":"Bob","age":3}]"#;
        let recs = parse_rows(json).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].id, "abc");
        assert_eq!(recs[0].data.get("name").unwrap(), &serde_json::json!("Bob"));
        assert_eq!(recs[0].data.get("age").unwrap(), &serde_json::json!(3));
    }

    #[test]
    fn parse_rows_reparses_json_text_columns() {
        // sql.js stores JSON columns as TEXT; parse_rows restores structure.
        let json = r#"[{"id":"1","meta":"{\"k\":\"v\"}","tags":"[1,2]"}]"#;
        let recs = parse_rows(json).unwrap();
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
    fn parse_rows_numeric_id_stringified() {
        let recs = parse_rows(r#"[{"id":7,"v":"x"}]"#).unwrap();
        assert_eq!(recs[0].id, "7");
    }

    #[test]
    fn parse_rows_non_json_text_left_alone() {
        // A plain string that doesn't look like JSON must stay a string.
        let recs = parse_rows(r#"[{"id":"1","note":"hello world"}]"#).unwrap();
        assert_eq!(
            recs[0].data.get("note").unwrap(),
            &serde_json::json!("hello world")
        );
    }

    #[test]
    fn parse_rows_empty_is_empty() {
        assert!(parse_rows("[]").unwrap().is_empty());
    }

    #[test]
    fn parse_rows_rejects_non_object_row() {
        assert!(parse_rows("[1,2]").is_err());
    }

    // ── first_scalar ──────────────────────────────────────────────────────────

    #[test]
    fn first_scalar_takes_aliased_count_column() {
        // `SELECT COUNT(*) AS cnt` → one row, one column named `cnt`.
        let recs = parse_rows(r#"[{"cnt":5}]"#).unwrap();
        assert_eq!(first_scalar(recs), Some(serde_json::json!(5)));
    }

    #[test]
    fn first_scalar_takes_aliased_sum_column() {
        let recs = parse_rows(r#"[{"total":12.5}]"#).unwrap();
        assert_eq!(first_scalar(recs), Some(serde_json::json!(12.5)));
    }

    #[test]
    fn first_scalar_empty_is_none() {
        assert_eq!(first_scalar(parse_rows("[]").unwrap()), None);
    }

    // ── parse_rows_modified ───────────────────────────────────────────────────

    #[test]
    fn parse_rows_modified_table() {
        assert_eq!(parse_rows_modified("0"), 0);
        assert_eq!(parse_rows_modified(" 3 "), 3);
        assert_eq!(parse_rows_modified("not-a-number"), 0);
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
