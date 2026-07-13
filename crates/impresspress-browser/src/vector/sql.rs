//! Pure SQL-string and BLOB-packing helpers for `BrowserVectorService`.
//!
//! Side-effect-free and native-testable: no `wasm_bindgen`/browser-bridge
//! calls here, so these unit-test on native even though the module imports
//! `wafer_core::interfaces::vector::service::DistanceMetric`.

use wafer_core::interfaces::vector::service::DistanceMetric;

/// Returns the DDL statements to create a vector index. Tables:
/// - `{name}_vectors` — id PK, vector BLOB, metadata TEXT, [text TEXT]
/// - `{name}_fts` — fts5(id UNINDEXED, text) — only when keyword_search=true
/// - `{name}_meta` — id PK, rowid INTEGER, metadata TEXT, [text TEXT]
///
/// Every statement carries `IF NOT EXISTS`: browsers kill idle Service
/// Workers within minutes, and the SW's in-memory index cache is rebuilt
/// from scratch on every restart (see `IndexState` in `service.rs`). A
/// caller recovering from a cold cache re-calls `create_index` for an index
/// that may already exist on disk — that must succeed idempotently, not
/// throw "table already exists".
pub fn build_create_index_sql(prefixed_name: &str, keyword_search: bool) -> Vec<String> {
    let v = format!("{prefixed_name}_vectors");
    let m = format!("{prefixed_name}_meta");

    let text_col = if keyword_search { ", text TEXT" } else { "" };

    let mut out = vec![format!(
        r#"CREATE TABLE IF NOT EXISTS "{v}" (id TEXT PRIMARY KEY, vector BLOB NOT NULL, metadata TEXT{text_col})"#
    )];
    if keyword_search {
        let f = format!("{prefixed_name}_fts");
        out.push(format!(
            r#"CREATE VIRTUAL TABLE IF NOT EXISTS "{f}" USING fts5(id UNINDEXED, text)"#
        ));
    }
    out.push(format!(
        r#"CREATE TABLE IF NOT EXISTS "{m}" (id TEXT PRIMARY KEY, rowid INTEGER, metadata TEXT{text_col})"#
    ));
    out
}

// ─── Index config registry ──────────────────────────────────────────────
//
// `dimensions`/`metric`/`keyword_search` aren't recoverable from the
// `_vectors`/`_meta`/`_fts` tables' own schema (the `vector` column is a
// plain BLOB with no length constraint, and nothing on disk records which
// distance metric an index was created with). This table is the only
// record of that config, so `BrowserVectorService::lookup` can hydrate its
// in-memory cache after a Service Worker restart instead of returning
// `IndexNotFound` for an index that is still physically on disk.

/// Table that persists per-index config across Service Worker restarts,
/// inside the same sql.js OPFS database that stores each index's own
/// `_vectors`/`_fts`/`_meta` tables. Named with a leading/trailing `__` so
/// it can never collide with a `{prefixed_name}_vectors|_fts|_meta` table —
/// index names are restricted to `[A-Za-z0-9_]` and none of those suffixes
/// match this literal name.
pub const REGISTRY_TABLE: &str = "__vector_index_registry__";

/// Idempotent DDL for the registry table. Safe to run before every write or
/// hydration read — a warm cache that already created it pays only a no-op
/// statement.
pub fn build_registry_ddl() -> String {
    format!(
        r#"CREATE TABLE IF NOT EXISTS "{REGISTRY_TABLE}" (name TEXT PRIMARY KEY, dimensions INTEGER NOT NULL, metric TEXT NOT NULL, keyword_search INTEGER NOT NULL)"#
    )
}

/// `INSERT OR REPLACE` the config row for `name` — idempotent, matching the
/// idempotent DDL above, so re-registering an existing index just refreshes
/// its row instead of erroring.
pub fn build_registry_upsert_sql(
    name: &str,
    dimensions: u32,
    metric: DistanceMetric,
    keyword_search: bool,
) -> PreparedStmt {
    PreparedStmt {
        sql: format!(
            r#"INSERT OR REPLACE INTO "{REGISTRY_TABLE}" (name, dimensions, metric, keyword_search) VALUES (?, ?, ?, ?)"#
        ),
        params_json: serde_json::json!([
            name,
            dimensions,
            metric_to_storage_str(metric),
            keyword_search as i64
        ])
        .to_string(),
    }
}

/// `(sql, params_json)` to look up one index's persisted config row by name.
pub fn build_registry_select_sql(name: &str) -> (String, String) {
    (
        format!(
            r#"SELECT dimensions, metric, keyword_search FROM "{REGISTRY_TABLE}" WHERE name = ?"#
        ),
        serde_json::json!([name]).to_string(),
    )
}

/// `(sql, params_json)` to remove an index's persisted config row.
pub fn build_registry_delete_sql(name: &str) -> (String, String) {
    (
        format!(r#"DELETE FROM "{REGISTRY_TABLE}" WHERE name = ?"#),
        serde_json::json!([name]).to_string(),
    )
}

/// Outcome of comparing an existing registry row's config (if any) against
/// an incoming `create_index` request's config for the same index name.
/// `BrowserVectorService::create_index` uses this to decide whether to
/// proceed (write the registry row + run the idempotent DDL), silently
/// no-op (the legitimate SW-restart recovery case: the same config was
/// already registered), or fail with `VectorError::IndexAlreadyExists` —
/// matching the native `wafer-block-sqlite` backend's contract for a
/// genuine name collision (`create_index_duplicate_fails`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryConflict {
    /// No existing row for this name — a genuine create.
    New,
    /// An existing row's config exactly matches the incoming request — the
    /// SW-restart recovery case. Safe to no-op (re-running the idempotent
    /// DDL and re-writing the identical row is harmless).
    IdenticalNoOp,
    /// An existing row's config differs in dimensions, metric, or
    /// keyword_search. A real name collision: silently overwriting the
    /// registry row here would leave the `_vectors`/`_meta` tables (and
    /// their already-stored rows) out of sync with the new config, so this
    /// must be rejected rather than applied.
    Mismatch,
}

/// Classifies a `create_index(name, incoming)` call against `name`'s
/// existing registry row, if any. Both config tuples are
/// `(dimensions, metric, keyword_search)`. `DistanceMetric` is a discrete
/// enum (`Cosine`/`Euclidean`/`DotProduct`), not a float, so this
/// comparison is exact equality — no epsilon/rounding ambiguity.
pub fn classify_registry_conflict(
    existing: Option<(u32, DistanceMetric, bool)>,
    incoming: (u32, DistanceMetric, bool),
) -> RegistryConflict {
    match existing {
        None => RegistryConflict::New,
        Some(e) if e == incoming => RegistryConflict::IdenticalNoOp,
        Some(_) => RegistryConflict::Mismatch,
    }
}

/// Encodes a [`DistanceMetric`] for the registry `metric` column. A storage
/// encoding of our own (not the wire JSON one) so it stays legible and
/// stable regardless of how `wafer_block::wire::vector::DistanceMetric`'s
/// serde attributes evolve — this string never leaves the browser's own
/// sql.js database.
fn metric_to_storage_str(metric: DistanceMetric) -> &'static str {
    match metric {
        DistanceMetric::Cosine => "cosine",
        DistanceMetric::Euclidean => "euclidean",
        DistanceMetric::DotProduct => "dot_product",
    }
}

/// Inverse of [`metric_to_storage_str`]. `None` on anything else — a
/// registry row with an unrecognized metric string is corrupt, not a
/// silently-defaulted `Cosine`.
fn metric_from_storage_str(s: &str) -> Option<DistanceMetric> {
    match s {
        "cosine" => Some(DistanceMetric::Cosine),
        "euclidean" => Some(DistanceMetric::Euclidean),
        "dot_product" => Some(DistanceMetric::DotProduct),
        _ => None,
    }
}

/// Parses one registry row — a JSON object as returned by `db_query_raw`
/// for the query built by [`build_registry_select_sql`] (see
/// `bridge::db_query_raw`'s doc comment for the row-object JSON shape) —
/// into `(dimensions, metric, keyword_search)`.
///
/// Pulled out of `service.rs` (which is wasm32-only, since it calls the
/// `bridge` extern functions) so the row-shape parsing — including its
/// error paths — is unit-testable on native without a real sql.js/OPFS
/// backing store.
pub fn parse_registry_row(row: &serde_json::Value) -> Result<(u32, DistanceMetric, bool), String> {
    let dimensions = row
        .get("dimensions")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "registry row missing/non-numeric dimensions".to_string())?
        as u32;
    let metric_str = row
        .get("metric")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "registry row missing/non-string metric".to_string())?;
    let metric = metric_from_storage_str(metric_str)
        .ok_or_else(|| format!("registry row has unknown metric {metric_str:?}"))?;
    let keyword_search = row
        .get("keyword_search")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| "registry row missing/non-numeric keyword_search".to_string())?
        != 0;
    Ok((dimensions, metric, keyword_search))
}

pub fn build_delete_index_sql(prefixed_name: &str, keyword_search: bool) -> Vec<String> {
    let mut out = vec![format!(r#"DROP TABLE IF EXISTS "{prefixed_name}_vectors""#)];
    if keyword_search {
        out.push(format!(r#"DROP TABLE IF EXISTS "{prefixed_name}_fts""#));
    }
    out.push(format!(r#"DROP TABLE IF EXISTS "{prefixed_name}_meta""#));
    out
}

pub fn build_count_sql(prefixed_name: &str) -> String {
    format!(r#"SELECT COUNT(*) AS n FROM "{prefixed_name}_meta""#)
}

/// Returns `(statements, params)`. Statements share the same parameter list.
/// Each statement targets one of the index's tables.
pub fn build_delete_ids_sql(
    prefixed_name: &str,
    ids: &[String],
    keyword_search: bool,
) -> (Vec<String>, Vec<String>) {
    if ids.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let mut out = vec![format!(
        r#"DELETE FROM "{prefixed_name}_vectors" WHERE id IN ({placeholders})"#
    )];
    if keyword_search {
        out.push(format!(
            r#"DELETE FROM "{prefixed_name}_fts" WHERE id IN ({placeholders})"#
        ));
    }
    out.push(format!(
        r#"DELETE FROM "{prefixed_name}_meta" WHERE id IN ({placeholders})"#
    ));
    (out, ids.to_vec())
}

/// Pack `&[f32]` as little-endian bytes for storage in a sql.js BLOB column.
pub fn pack_vector_blob(v: &[f32]) -> Vec<u8> {
    let bytes: &[u8] = bytemuck::cast_slice(v);
    bytes.to_vec()
}

/// Unpack a BLOB into `Vec<f32>`. Errors if the byte length does not equal
/// `4 * expected_dims`.
pub fn parse_vector_blob(bytes: &[u8], expected_dims: u32) -> Result<Vec<f32>, String> {
    let want = (expected_dims as usize) * 4;
    if bytes.len() != want {
        return Err(format!(
            "vector blob length {} != expected {} ({}d × 4 bytes)",
            bytes.len(),
            want,
            expected_dims
        ));
    }
    let floats: &[f32] =
        bytemuck::try_cast_slice(bytes).map_err(|e| format!("blob alignment error: {e}"))?;
    Ok(floats.to_vec())
}

#[derive(Clone, Debug)]
pub struct SqlUpsertEntry {
    pub id: String,
    /// Base64-encoded packed f32 BLOB.
    pub vector_blob_b64: String,
    pub metadata_json: String,
    pub text: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PreparedStmt {
    pub sql: String,
    /// JSON array string (the format `bridge::db_exec_raw` expects).
    pub params_json: String,
}

/// Builds `INSERT OR REPLACE` statements. One statement per table per row
/// keeps each blob param self-contained — sql.js's positional binding handles
/// strings and base64 blobs uniformly via the JSON-array convention used by
/// the rest of the bridge.
pub fn build_upsert_sql_stmts(
    prefixed_name: &str,
    keyword_search: bool,
    entries: &[SqlUpsertEntry],
) -> Vec<PreparedStmt> {
    let mut out = Vec::with_capacity(entries.len() * if keyword_search { 3 } else { 2 });
    for e in entries {
        let (sql_v, params_v) = if keyword_search {
            (
                format!(
                    r#"INSERT OR REPLACE INTO "{prefixed_name}_vectors" (id, vector, metadata, text) VALUES (?, base64_decode(?), ?, ?)"#
                ),
                serde_json::json!([
                    e.id,
                    e.vector_blob_b64,
                    e.metadata_json,
                    e.text.clone().unwrap_or_default()
                ]),
            )
        } else {
            (
                format!(
                    r#"INSERT OR REPLACE INTO "{prefixed_name}_vectors" (id, vector, metadata) VALUES (?, base64_decode(?), ?)"#
                ),
                serde_json::json!([e.id, e.vector_blob_b64, e.metadata_json]),
            )
        };
        out.push(PreparedStmt {
            sql: sql_v,
            params_json: params_v.to_string(),
        });

        if keyword_search {
            out.push(PreparedStmt {
                sql: format!(
                    r#"INSERT OR REPLACE INTO "{prefixed_name}_fts" (id, text) VALUES (?, ?)"#
                ),
                params_json: serde_json::json!([e.id, e.text.clone().unwrap_or_default()])
                    .to_string(),
            });
        }

        let (sql_m, params_m) = if keyword_search {
            (
                format!(
                    r#"INSERT OR REPLACE INTO "{prefixed_name}_meta" (id, rowid, metadata, text) VALUES (?, NULL, ?, ?)"#
                ),
                serde_json::json!([e.id, e.metadata_json, e.text.clone().unwrap_or_default()]),
            )
        } else {
            (
                format!(
                    r#"INSERT OR REPLACE INTO "{prefixed_name}_meta" (id, rowid, metadata) VALUES (?, NULL, ?)"#
                ),
                serde_json::json!([e.id, e.metadata_json]),
            )
        };
        out.push(PreparedStmt {
            sql: sql_m,
            params_json: params_m.to_string(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_index_with_keyword_emits_three_tables() {
        let sqls = build_create_index_sql("impresspress__vector__docs", true);
        assert_eq!(sqls.len(), 3);
        assert!(
            sqls[0].contains(r#"CREATE TABLE IF NOT EXISTS "impresspress__vector__docs_vectors""#)
        );
        assert!(sqls[0].contains("vector BLOB"));
        assert!(sqls[0].contains("text TEXT"));
        assert!(sqls[1]
            .contains(r#"CREATE VIRTUAL TABLE IF NOT EXISTS "impresspress__vector__docs_fts""#));
        assert!(sqls[1].contains("USING fts5(id UNINDEXED, text)"));
        assert!(sqls[2].contains(r#"CREATE TABLE IF NOT EXISTS "impresspress__vector__docs_meta""#));
        assert!(
            sqls[2].contains("text TEXT"),
            "expected _meta to include text column when keyword_search=true"
        );
    }

    #[test]
    fn create_index_sql_is_idempotent() {
        // Re-registering an existing index after a Service Worker restart
        // (the cache-cold recovery path) must not throw "table already
        // exists" — every DDL statement needs IF NOT EXISTS.
        for keyword_search in [true, false] {
            let sqls = build_create_index_sql("idx", keyword_search);
            assert!(
                sqls.iter().all(|s| s.contains("IF NOT EXISTS")),
                "every create-index statement must be idempotent (keyword_search={keyword_search}): {sqls:?}"
            );
        }
    }

    #[test]
    fn create_index_without_keyword_emits_two_tables() {
        let sqls = build_create_index_sql("impresspress__vector__docs", false);
        assert_eq!(sqls.len(), 2);
        assert!(sqls[0].contains("vectors"));
        assert!(!sqls[0].contains("text TEXT"));
        assert!(sqls[1].contains("meta"));
        assert!(
            !sqls[1].contains("text TEXT"),
            "expected _meta to omit text column when keyword_search=false"
        );
        assert!(!sqls.iter().any(|s| s.contains("USING fts5")));
    }

    #[test]
    fn delete_index_drops_all_three_tables() {
        let sqls = build_delete_index_sql("impresspress__vector__docs", true);
        assert_eq!(sqls.len(), 3);
        assert!(sqls
            .iter()
            .any(|s| s.contains("DROP TABLE IF EXISTS \"impresspress__vector__docs_vectors\"")));
        assert!(sqls
            .iter()
            .any(|s| s.contains("DROP TABLE IF EXISTS \"impresspress__vector__docs_fts\"")));
        assert!(sqls
            .iter()
            .any(|s| s.contains("DROP TABLE IF EXISTS \"impresspress__vector__docs_meta\"")));
    }

    #[test]
    fn delete_index_without_keyword_drops_two() {
        let sqls = build_delete_index_sql("impresspress__vector__docs", false);
        assert_eq!(sqls.len(), 2);
        assert!(!sqls.iter().any(|s| s.contains("_fts")));
    }

    #[test]
    fn count_sql_targets_meta_table() {
        assert_eq!(
            build_count_sql("impresspress__vector__docs"),
            r#"SELECT COUNT(*) AS n FROM "impresspress__vector__docs_meta""#
        );
    }

    #[test]
    fn delete_by_ids_uses_in_clause() {
        let (sqls, params) =
            build_delete_ids_sql("impresspress__vector__docs", &["a".into(), "b".into()], true);
        assert_eq!(sqls.len(), 3);
        assert!(sqls[0]
            .contains(r#"DELETE FROM "impresspress__vector__docs_vectors" WHERE id IN (?, ?)"#));
        assert!(
            sqls[1].contains(r#"DELETE FROM "impresspress__vector__docs_fts" WHERE id IN (?, ?)"#)
        );
        assert!(
            sqls[2].contains(r#"DELETE FROM "impresspress__vector__docs_meta" WHERE id IN (?, ?)"#)
        );
        assert_eq!(params, vec!["a", "b"]);
    }

    #[test]
    fn delete_by_ids_empty_returns_no_statements() {
        let (sqls, params) = build_delete_ids_sql("impresspress__vector__docs", &[], true);
        assert!(sqls.is_empty());
        assert!(params.is_empty());
    }

    #[test]
    fn pack_then_unpack_roundtrip() {
        let v = vec![0.1f32, -0.5, 1e-7, f32::INFINITY, 0.0];
        let packed = pack_vector_blob(&v);
        assert_eq!(packed.len(), v.len() * 4);
        let unpacked = parse_vector_blob(&packed, v.len() as u32).expect("parse");
        assert_eq!(unpacked, v);
    }

    #[test]
    fn parse_rejects_wrong_byte_length() {
        let blob = vec![0u8; 10]; // not divisible by 4
        assert!(parse_vector_blob(&blob, 1).is_err());
    }

    #[test]
    fn parse_rejects_dimension_mismatch() {
        let v = vec![0.1f32; 4];
        let packed = pack_vector_blob(&v);
        assert!(parse_vector_blob(&packed, 5).is_err());
    }

    #[test]
    fn upsert_emits_three_statements_with_keyword() {
        let entry = SqlUpsertEntry {
            id: "doc1".into(),
            vector_blob_b64: "AAAA".into(),
            metadata_json: "{}".into(),
            text: Some("hello".into()),
        };
        let stmts = build_upsert_sql_stmts("impresspress__vector__docs", true, &[entry]);
        assert_eq!(stmts.len(), 3, "expected vectors + fts + meta upserts");
        assert!(stmts[0]
            .sql
            .contains("INSERT OR REPLACE INTO \"impresspress__vector__docs_vectors\""));
        assert!(stmts[1]
            .sql
            .contains("INSERT OR REPLACE INTO \"impresspress__vector__docs_fts\""));
        assert!(stmts[2]
            .sql
            .contains("INSERT OR REPLACE INTO \"impresspress__vector__docs_meta\""));
    }

    #[test]
    fn upsert_without_keyword_skips_fts() {
        let entry = SqlUpsertEntry {
            id: "doc1".into(),
            vector_blob_b64: "AAAA".into(),
            metadata_json: "{}".into(),
            text: None,
        };
        let stmts = build_upsert_sql_stmts("impresspress__vector__docs", false, &[entry]);
        assert_eq!(stmts.len(), 2);
        assert!(!stmts.iter().any(|s| s.sql.contains("_fts")));
    }

    // ─── Registry (hydration persistence) ───────────────────────────────

    #[test]
    fn registry_ddl_is_idempotent_and_targets_registry_table() {
        let sql = build_registry_ddl();
        assert!(sql.contains("IF NOT EXISTS"));
        assert!(sql.contains(REGISTRY_TABLE));
        assert!(sql.contains("dimensions INTEGER NOT NULL"));
        assert!(sql.contains("metric TEXT NOT NULL"));
        assert!(sql.contains("keyword_search INTEGER NOT NULL"));
    }

    #[test]
    fn registry_upsert_uses_or_replace_and_binds_all_fields() {
        let stmt = build_registry_upsert_sql(
            "impresspress__vector__docs",
            384,
            DistanceMetric::Cosine,
            true,
        );
        assert!(stmt.sql.contains("INSERT OR REPLACE INTO"));
        assert!(stmt.sql.contains(REGISTRY_TABLE));
        let params: serde_json::Value = serde_json::from_str(&stmt.params_json).unwrap();
        assert_eq!(
            params,
            serde_json::json!(["impresspress__vector__docs", 384, "cosine", 1])
        );
    }

    #[test]
    fn registry_upsert_encodes_keyword_search_false_as_zero() {
        let stmt = build_registry_upsert_sql("idx", 3, DistanceMetric::Euclidean, false);
        let params: serde_json::Value = serde_json::from_str(&stmt.params_json).unwrap();
        assert_eq!(params, serde_json::json!(["idx", 3, "euclidean", 0]));
    }

    #[test]
    fn registry_select_targets_name_and_registry_table() {
        let (sql, params) = build_registry_select_sql("idx");
        assert!(sql.contains(REGISTRY_TABLE));
        assert!(sql.contains("WHERE name = ?"));
        assert_eq!(params, serde_json::json!(["idx"]).to_string());
    }

    #[test]
    fn registry_delete_targets_name_and_registry_table() {
        let (sql, params) = build_registry_delete_sql("idx");
        assert!(sql.starts_with("DELETE FROM"));
        assert!(sql.contains(REGISTRY_TABLE));
        assert_eq!(params, serde_json::json!(["idx"]).to_string());
    }

    // ─── create_index re-create guard (registry conflict classification) ──

    #[test]
    fn classify_registry_conflict_no_existing_row_is_new() {
        let incoming = (384, DistanceMetric::Cosine, false);
        assert_eq!(
            classify_registry_conflict(None, incoming),
            RegistryConflict::New
        );
    }

    #[test]
    fn classify_registry_conflict_identical_config_is_idempotent_noop() {
        // The SW-restart recovery case: re-registering the exact same
        // config after a cold cache must not error.
        let cfg = (384, DistanceMetric::Cosine, true);
        assert_eq!(
            classify_registry_conflict(Some(cfg), cfg),
            RegistryConflict::IdenticalNoOp
        );
    }

    #[test]
    fn classify_registry_conflict_different_dimensions_is_mismatch() {
        let existing = (384, DistanceMetric::Cosine, false);
        let incoming = (768, DistanceMetric::Cosine, false);
        assert_eq!(
            classify_registry_conflict(Some(existing), incoming),
            RegistryConflict::Mismatch
        );
    }

    #[test]
    fn classify_registry_conflict_different_metric_is_mismatch() {
        let existing = (384, DistanceMetric::Cosine, false);
        let incoming = (384, DistanceMetric::Euclidean, false);
        assert_eq!(
            classify_registry_conflict(Some(existing), incoming),
            RegistryConflict::Mismatch
        );
    }

    #[test]
    fn classify_registry_conflict_different_keyword_search_is_mismatch() {
        let existing = (384, DistanceMetric::Cosine, false);
        let incoming = (384, DistanceMetric::Cosine, true);
        assert_eq!(
            classify_registry_conflict(Some(existing), incoming),
            RegistryConflict::Mismatch
        );
    }

    #[test]
    fn metric_storage_encoding_roundtrips_for_every_variant() {
        for metric in [
            DistanceMetric::Cosine,
            DistanceMetric::Euclidean,
            DistanceMetric::DotProduct,
        ] {
            let s = metric_to_storage_str(metric);
            assert_eq!(
                metric_from_storage_str(s),
                Some(metric),
                "storage encoding for {metric:?} must round-trip"
            );
        }
    }

    #[test]
    fn metric_from_storage_str_rejects_unknown_values() {
        assert_eq!(metric_from_storage_str("manhattan"), None);
        assert_eq!(metric_from_storage_str(""), None);
        // Must not silently accept the wire JSON encoding of DotProduct
        // (serde's `rename_all = "lowercase"` would collapse it to
        // "dotproduct") — the registry format is deliberately its own.
        assert_eq!(metric_from_storage_str("dotproduct"), None);
    }

    #[test]
    fn parse_registry_row_happy_path() {
        let row = serde_json::json!({ "dimensions": 384, "metric": "cosine", "keyword_search": 1 });
        let (dims, metric, kw) = parse_registry_row(&row).expect("valid row parses");
        assert_eq!(dims, 384);
        assert_eq!(metric, DistanceMetric::Cosine);
        assert!(kw);
    }

    #[test]
    fn parse_registry_row_keyword_search_zero_is_false() {
        let row =
            serde_json::json!({ "dimensions": 3, "metric": "dot_product", "keyword_search": 0 });
        let (_, metric, kw) = parse_registry_row(&row).expect("valid row parses");
        assert_eq!(metric, DistanceMetric::DotProduct);
        assert!(!kw);
    }

    #[test]
    fn parse_registry_row_rejects_missing_dimensions() {
        let row = serde_json::json!({ "metric": "cosine", "keyword_search": 0 });
        assert!(parse_registry_row(&row).is_err());
    }

    #[test]
    fn parse_registry_row_rejects_missing_metric() {
        let row = serde_json::json!({ "dimensions": 3, "keyword_search": 0 });
        assert!(parse_registry_row(&row).is_err());
    }

    #[test]
    fn parse_registry_row_rejects_unknown_metric() {
        let row =
            serde_json::json!({ "dimensions": 3, "metric": "manhattan", "keyword_search": 0 });
        assert!(parse_registry_row(&row).is_err());
    }

    #[test]
    fn parse_registry_row_rejects_missing_keyword_search() {
        let row = serde_json::json!({ "dimensions": 3, "metric": "cosine" });
        assert!(parse_registry_row(&row).is_err());
    }
}
