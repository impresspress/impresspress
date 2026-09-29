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
/// index names are `[a-z0-9_]` (a legacy index keeps its uppercase letters
/// until `rename_index` moves it) and none of those suffixes match this
/// literal name.
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
        params: vec![
            serde_json::json!(name),
            serde_json::json!(dimensions),
            serde_json::json!(metric_to_storage_str(metric)),
            serde_json::json!(keyword_search as i64),
        ],
    }
}

/// `(sql, params)` to look up one index's persisted config row by name.
/// `params` is the plain bind-value list — encode via
/// `db_codec::params_to_js` at the bridge boundary, no JSON-string step.
pub fn build_registry_select_sql(name: &str) -> (String, Vec<serde_json::Value>) {
    (
        format!(
            r#"SELECT dimensions, metric, keyword_search FROM "{REGISTRY_TABLE}" WHERE name = ?"#
        ),
        vec![serde_json::json!(name)],
    )
}

/// `(sql, params)` listing every registered index whose name starts with
/// `prefix`, in lexical order. The prefix is compared literally (`substr`,
/// not `LIKE`, so `_` in it is not a wildcard).
pub fn build_registry_list_sql(prefix: &str) -> (String, Vec<serde_json::Value>) {
    (
        format!(
            r#"SELECT name FROM "{REGISTRY_TABLE}" WHERE substr(name, 1, ?) = ? ORDER BY name"#
        ),
        vec![
            serde_json::json!(prefix.chars().count()),
            serde_json::json!(prefix),
        ],
    )
}

/// `(sql, params)` to remove an index's persisted config row.
pub fn build_registry_delete_sql(name: &str) -> (String, Vec<serde_json::Value>) {
    (
        format!(r#"DELETE FROM "{REGISTRY_TABLE}" WHERE name = ?"#),
        vec![serde_json::json!(name)],
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

/// Parses one registry row — a JSON object decoded (via
/// `db_codec::rows_from_js`) from the row `bridge::db_query_raw` resolves
/// for the query built by [`build_registry_select_sql`] — into
/// `(dimensions, metric, keyword_search)`.
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

/// Every table one index owns, in the order [`build_create_index_sql`]
/// creates them.
///
/// The names are spelled here, beside the DDL that creates and drops them,
/// so a caller that has to act on the whole set — invalidating the database
/// service's cached schema for it, say — cannot drift from the builders.
pub fn index_tables(prefixed_name: &str, keyword_search: bool) -> Vec<String> {
    let mut out = vec![format!("{prefixed_name}_vectors")];
    if keyword_search {
        out.push(format!("{prefixed_name}_fts"));
    }
    out.push(format!("{prefixed_name}_meta"));
    out
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

/// Reconstruct one `_vectors` table row — `(id, vector, metadata)` — from
/// the raw pieces already pulled off the JS bridge boundary.
///
/// `service.rs::load_all_vectors` decodes the `vector` BLOB column as a
/// typed `Vec<u8>` field via `serde_wasm_bindgen::from_value` directly
/// (mirroring `storage.rs`'s `GetResponse`/`network.rs`'s `FetchResponse`),
/// NOT via the generic `db_codec::rows_from_js`/`serde_json::Value` row
/// decode — sql.js resolves BLOB columns as a real `Uint8Array`, and
/// `serde_json::Value`'s `Deserialize` impl has no `visit_bytes`/
/// `visit_byte_buf`, so decoding a row with a BLOB column through that
/// generic path always errors.
///
/// This function is the pure remainder once those raw bytes are in hand:
/// unpack them into `Vec<f32>` via [`parse_vector_blob`] and parse the
/// `metadata` TEXT column as JSON (silently `None` on absence or on
/// malformed JSON — matches `load_metadata_for_ids`'s existing behavior for
/// the same column elsewhere). Kept in this module (no `wasm_bindgen`
/// involved) so it's host-testable even though its only real caller is
/// wasm32-only.
pub fn decode_vector_row(
    id: String,
    vector_bytes: &[u8],
    metadata_json: Option<&str>,
    dims: u32,
) -> Result<(String, Vec<f32>, Option<serde_json::Value>), String> {
    let vector = parse_vector_blob(vector_bytes, dims)?;
    let metadata = metadata_json.and_then(|s| serde_json::from_str(s).ok());
    Ok((id, vector, metadata))
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
    /// Positional bind values, in `?`-placeholder order — encode via
    /// `db_codec::params_to_js` at the bridge boundary (`bridge::db_exec_raw`
    /// takes a structured JS array, not a JSON string).
    pub params: Vec<serde_json::Value>,
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
                vec![
                    serde_json::json!(e.id),
                    serde_json::json!(e.vector_blob_b64),
                    serde_json::json!(e.metadata_json),
                    serde_json::json!(e.text.clone().unwrap_or_default()),
                ],
            )
        } else {
            (
                format!(
                    r#"INSERT OR REPLACE INTO "{prefixed_name}_vectors" (id, vector, metadata) VALUES (?, base64_decode(?), ?)"#
                ),
                vec![
                    serde_json::json!(e.id),
                    serde_json::json!(e.vector_blob_b64),
                    serde_json::json!(e.metadata_json),
                ],
            )
        };
        out.push(PreparedStmt {
            sql: sql_v,
            params: params_v,
        });

        if keyword_search {
            out.push(PreparedStmt {
                sql: format!(
                    r#"INSERT OR REPLACE INTO "{prefixed_name}_fts" (id, text) VALUES (?, ?)"#
                ),
                params: vec![
                    serde_json::json!(e.id),
                    serde_json::json!(e.text.clone().unwrap_or_default()),
                ],
            });
        }

        let (sql_m, params_m) = if keyword_search {
            (
                format!(
                    r#"INSERT OR REPLACE INTO "{prefixed_name}_meta" (id, rowid, metadata, text) VALUES (?, NULL, ?, ?)"#
                ),
                vec![
                    serde_json::json!(e.id),
                    serde_json::json!(e.metadata_json),
                    serde_json::json!(e.text.clone().unwrap_or_default()),
                ],
            )
        } else {
            (
                format!(
                    r#"INSERT OR REPLACE INTO "{prefixed_name}_meta" (id, rowid, metadata) VALUES (?, NULL, ?)"#
                ),
                vec![serde_json::json!(e.id), serde_json::json!(e.metadata_json)],
            )
        };
        out.push(PreparedStmt {
            sql: sql_m,
            params: params_m,
        });
    }
    out
}

/// The statements that move index `from` to `to`, run by
/// `BrowserVectorService::rename_index` inside one transaction once it has
/// checked the names (`check_rename`), that `from` is registered and that
/// nothing occupies `to`.
///
/// Each of the index's tables moves `from` → staging → `to`. SQLite compares
/// table names case-insensitively, so a direct `Docs_meta` → `docs_meta`
/// rename is refused as "already another table". The staging stem
/// `{to}-rename` can never be an index's own name, because index names never
/// contain `-`. FTS5 renames its shadow tables along with the virtual table.
/// The registry row moves last, in the same transaction.
pub fn build_rename_index_sql(from: &str, to: &str, keyword_search: bool) -> Vec<PreparedStmt> {
    let staging = format!("{to}-rename");
    let mut out = Vec::new();
    for ((old, stage), new) in index_tables(from, keyword_search)
        .into_iter()
        .zip(index_tables(&staging, keyword_search))
        .zip(index_tables(to, keyword_search))
    {
        out.push(PreparedStmt {
            sql: format!(r#"ALTER TABLE "{old}" RENAME TO "{stage}""#),
            params: Vec::new(),
        });
        out.push(PreparedStmt {
            sql: format!(r#"ALTER TABLE "{stage}" RENAME TO "{new}""#),
            params: Vec::new(),
        });
    }
    out.push(PreparedStmt {
        sql: format!(r#"UPDATE "{REGISTRY_TABLE}" SET name = ? WHERE name = ?"#),
        params: vec![serde_json::json!(to), serde_json::json!(from)],
    });
    out
}

/// `(sql, params)` listing every table that would stand in `to`'s way: one
/// named like any of `to`'s tables ignoring case (SQLite's own rule for table
/// names) that is not one of `from`'s own tables, which become `to`'s. Any
/// row means two indexes differ only by case, and the move is refused rather
/// than merging them. The FTS name is checked even when `from` has no keyword
/// search: the moved index would otherwise pick that table up as its own.
pub fn build_rename_conflicts_sql(from: &str, to: &str) -> (String, Vec<serde_json::Value>) {
    let targets = index_tables(to, true);
    let own = index_tables(from, true);
    let placeholders = |n: usize| vec!["lower(?)"; n].join(", ");
    let own_placeholders = vec!["?"; own.len()].join(", ");
    let mut params: Vec<serde_json::Value> = targets.iter().map(|t| serde_json::json!(t)).collect();
    params.extend(own.iter().map(|t| serde_json::json!(t)));
    (
        format!(
            "SELECT name FROM sqlite_master WHERE lower(name) IN ({}) AND name NOT IN ({own_placeholders})",
            placeholders(targets.len()),
        ),
        params,
    )
}

#[cfg(test)]
mod tests {

    use super::*;

    /// [`index_tables`] claims to name every table the create/delete builders
    /// touch. Enforced rather than asserted in prose: each statement has to
    /// name the table at its position, in both keyword-search shapes.
    #[test]
    fn index_tables_names_the_table_each_built_statement_touches() {
        for keyword_search in [false, true] {
            let tables = index_tables("idx", keyword_search);
            for (builder, statements) in [
                ("create", build_create_index_sql("idx", keyword_search)),
                ("delete", build_delete_index_sql("idx", keyword_search)),
            ] {
                assert_eq!(
                    statements.len(),
                    tables.len(),
                    "{builder} builds one statement per table (keyword_search = {keyword_search})"
                );
                for (statement, table) in statements.iter().zip(&tables) {
                    assert!(
                        statement.contains(table.as_str()),
                        "{builder} statement {statement:?} does not name {table}"
                    );
                }
            }
        }
    }

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
        let (sqls, params) = build_delete_ids_sql(
            "impresspress__vector__docs",
            &["a".into(), "b".into()],
            true,
        );
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

    // ─── decode_vector_row (BLOB-column row reconstruction) ────────────────

    #[test]
    fn decode_vector_row_reconstructs_vector_and_metadata() {
        let v = vec![0.25f32, -1.5, 3.0];
        let bytes = pack_vector_blob(&v);
        let (id, vector, metadata) =
            decode_vector_row("doc1".into(), &bytes, Some(r#"{"k":"v"}"#), v.len() as u32)
                .expect("decode succeeds");
        assert_eq!(id, "doc1");
        assert_eq!(vector, v);
        assert_eq!(metadata, Some(serde_json::json!({"k":"v"})));
    }

    #[test]
    fn decode_vector_row_missing_metadata_is_none() {
        let v = vec![1.0f32, 2.0];
        let bytes = pack_vector_blob(&v);
        let (_, _, metadata) =
            decode_vector_row("doc1".into(), &bytes, None, v.len() as u32).unwrap();
        assert_eq!(metadata, None);
    }

    #[test]
    fn decode_vector_row_malformed_metadata_json_is_none_not_error() {
        let v = vec![1.0f32];
        let bytes = pack_vector_blob(&v);
        let (_, _, metadata) =
            decode_vector_row("doc1".into(), &bytes, Some("not json"), v.len() as u32).unwrap();
        assert_eq!(metadata, None);
    }

    #[test]
    fn decode_vector_row_propagates_blob_length_mismatch() {
        let bytes = vec![0u8; 3]; // not divisible by 4, and wrong for dims=1
        assert!(decode_vector_row("doc1".into(), &bytes, None, 1).is_err());
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
        assert_eq!(
            stmt.params,
            vec![
                serde_json::json!("impresspress__vector__docs"),
                serde_json::json!(384),
                serde_json::json!("cosine"),
                serde_json::json!(1),
            ]
        );
    }

    #[test]
    fn registry_upsert_encodes_keyword_search_false_as_zero() {
        let stmt = build_registry_upsert_sql("idx", 3, DistanceMetric::Euclidean, false);
        assert_eq!(
            stmt.params,
            vec![
                serde_json::json!("idx"),
                serde_json::json!(3),
                serde_json::json!("euclidean"),
                serde_json::json!(0),
            ]
        );
    }

    #[test]
    fn registry_select_targets_name_and_registry_table() {
        let (sql, params) = build_registry_select_sql("idx");
        assert!(sql.contains(REGISTRY_TABLE));
        assert!(sql.contains("WHERE name = ?"));
        assert_eq!(params, vec![serde_json::json!("idx")]);
    }

    #[test]
    fn registry_delete_targets_name_and_registry_table() {
        let (sql, params) = build_registry_delete_sql("idx");
        assert!(sql.starts_with("DELETE FROM"));
        assert!(sql.contains(REGISTRY_TABLE));
        assert_eq!(params, vec![serde_json::json!("idx")]);
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

    /// The rename statements move a legacy index — tables, entries, keyword
    /// search and registry row — to its lowercase name on a real SQLite, the
    /// engine sql.js compiles to. The legacy index is built by this module's
    /// own create statements, which are what the browser wrote under a
    /// mixed-case name before names had to be lowercase.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_rename_statements_move_a_mixed_case_index_with_its_rows() {
        let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
        conn.execute_batch(&build_registry_ddl()).expect("registry");
        let from = "impresspress__vector__Docs";
        let to = "impresspress__vector__docs";
        for stmt in build_create_index_sql(from, true) {
            conn.execute_batch(&stmt).expect("legacy create");
        }
        let reg = build_registry_upsert_sql(from, 3, DistanceMetric::Cosine, true);
        conn.execute(&reg.sql, rusqlite::params![from, 3, "cosine", 1])
            .expect("legacy registry row");
        conn.execute_batch(&format!(
            r#"INSERT INTO "{from}_vectors" (id, vector, metadata, text) VALUES ('a', x'00', '{{}}', 'hello');
               INSERT INTO "{from}_meta" (id, rowid, metadata, text) VALUES ('a', 1, '{{}}', 'hello');
               INSERT INTO "{from}_fts" (id, text) VALUES ('a', 'hello');"#
        ))
        .expect("legacy rows");

        let (conflicts, params) = build_rename_conflicts_sql(from, to);
        let taken: Vec<String> = conn
            .prepare(&conflicts)
            .unwrap()
            .query_map(
                rusqlite::params_from_iter(params.iter().map(|p| p.as_str().unwrap())),
                |r| r.get(0),
            )
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            taken.is_empty(),
            "the index's own tables are not in the way: {taken:?}"
        );

        conn.execute_batch("BEGIN").unwrap();
        for stmt in build_rename_index_sql(from, to, true) {
            let params: Vec<String> = stmt
                .params
                .iter()
                .map(|p| p.as_str().unwrap().to_string())
                .collect();
            conn.execute(&stmt.sql, rusqlite::params_from_iter(params))
                .unwrap_or_else(|e| panic!("{}: {e}", stmt.sql));
        }
        conn.execute_batch("COMMIT").unwrap();

        let names: std::collections::BTreeSet<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for table in index_tables(to, true) {
            assert!(names.contains(&table), "{table} missing from {names:?}");
        }
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with(from) || n.contains("-rename")),
            "no legacy or staging table is left: {names:?}"
        );
        let text: String = conn
            .query_row(
                &format!(r#"SELECT text FROM "{to}_fts" WHERE "{to}_fts" MATCH 'hello'"#),
                [],
                |r| r.get(0),
            )
            .expect("keyword search moved with the index");
        assert_eq!(text, "hello");
        let meta: i64 = conn
            .query_row(&format!(r#"SELECT COUNT(*) FROM "{to}_meta""#), [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(meta, 1);
        let (sel, sel_params) = build_registry_select_sql(to);
        let dims: i64 = conn
            .query_row(
                &sel,
                rusqlite::params![sel_params[0].as_str().unwrap()],
                |r| r.get(0),
            )
            .expect("the registry row moved");
        assert_eq!(dims, 3);
    }

    /// A table named like one of `to`'s, in any case, that is not one of
    /// `from`'s own is a conflict: two indexes differ only by case.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_case_twin_is_reported_as_a_conflict() {
        let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
        let from = "impresspress__vector__Docs";
        let to = "impresspress__vector__docs";
        for stmt in build_create_index_sql(from, false) {
            conn.execute_batch(&stmt).expect("legacy create");
        }
        // An FTS table under the lowercase name, left by another index.
        conn.execute_batch(&format!(
            r#"CREATE VIRTUAL TABLE "{to}_fts" USING fts5(id UNINDEXED, text)"#
        ))
        .unwrap();
        let (conflicts, params) = build_rename_conflicts_sql(from, to);
        let taken: Vec<String> = conn
            .prepare(&conflicts)
            .unwrap()
            .query_map(
                rusqlite::params_from_iter(params.iter().map(|p| p.as_str().unwrap())),
                |r| r.get(0),
            )
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(taken, vec![format!("{to}_fts")]);
    }

    /// The catalog lists registered names under a literal prefix, in order:
    /// `_` in the prefix is not a wildcard, and a legacy mixed-case name is
    /// listed as stored.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_registry_lists_the_names_under_a_literal_prefix() {
        let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
        conn.execute_batch(&build_registry_ddl()).expect("registry");
        for name in [
            "impresspress__vector__docs",
            "impresspress__vector__Notes",
            "impresspressXvectorXother",
            "other__vector__x",
        ] {
            let reg = build_registry_upsert_sql(name, 3, DistanceMetric::Cosine, false);
            conn.execute(&reg.sql, rusqlite::params![name, 3, "cosine", 0])
                .expect("row");
        }
        let (sql, params) = build_registry_list_sql("impresspress__vector__");
        let names: Vec<String> = conn
            .prepare(&sql)
            .unwrap()
            .query_map(
                rusqlite::params![params[0].as_i64().unwrap(), params[1].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            names,
            vec![
                "impresspress__vector__Notes".to_string(),
                "impresspress__vector__docs".to_string(),
            ]
        );
    }
}
