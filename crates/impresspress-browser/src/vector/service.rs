//! `BrowserVectorService` — sql.js-backed `VectorService`.
//!
//! Vectors are stored as `BLOB` columns in the shared OPFS sql.js database
//! (no separate file). Scoring is in-process Rust using SIMD on wasm32. FTS5
//! powers keyword search when the index has `keyword_search: true`.

use std::{collections::HashMap, sync::Mutex};

use wafer_core::interfaces::vector::service::{
    DistanceMetric, MetadataFilter, Result as VResult, SearchMode, VectorEntry, VectorError,
    VectorIndexConfig, VectorMatch, VectorService,
};

use crate::{bridge, vector::sql};

fn js_err(e: wasm_bindgen::JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}

fn matches_filter(metadata: Option<&serde_json::Value>, filter: &MetadataFilter) -> bool {
    if filter.equals.is_empty() {
        return true;
    }
    let Some(meta) = metadata else { return false };
    for (path, expected) in &filter.equals {
        let mut cursor = meta;
        for segment in path.split('.') {
            cursor = match cursor.get(segment) {
                Some(v) => v,
                None => return false,
            };
        }
        if cursor != expected {
            return false;
        }
    }
    true
}

/// Per-index config: cached in memory for the lifetime of this
/// `BrowserVectorService`, and persisted (`dimensions`/`metric`/
/// `keyword_search`) in the `sql::REGISTRY_TABLE` table inside the same
/// sql.js OPFS database that holds the index's own
/// `_vectors`/`_fts`/`_meta` tables.
///
/// Browsers kill idle Service Workers within minutes, and
/// `BrowserVectorService::new()` always starts with an empty `indexes`
/// map — so on every SW restart the in-memory cache is cold while the
/// on-disk tables (and this registry row) survive untouched. `lookup`
/// treats a cache miss as "maybe just cold, not gone": it hydrates from
/// the registry row before concluding `IndexNotFound`. `create_index`
/// writes the row idempotently ONLY when there is no existing row or the
/// existing row's config matches exactly (the SW-restart recovery case,
/// mirroring the `IF NOT EXISTS` index-table DDL) — a re-create with a
/// DIFFERENT config is rejected with `VectorError::IndexAlreadyExists`
/// rather than silently overwritten, since the underlying
/// `_vectors`/`_meta`/`_fts` tables and their stored rows would otherwise
/// be left on the old config. `delete_index` removes the row so a deleted
/// index can't hydrate back from a stale one.
#[derive(Clone)]
struct IndexState {
    dimensions: u32,
    metric: DistanceMetric,
    keyword_search: bool,
}

pub struct BrowserVectorService {
    indexes: Mutex<HashMap<String, IndexState>>,
}

// SAFETY: wasm32-unknown-unknown has no threads, so the `Mutex` here is
// never contended and the `Send`/`Sync` bounds required by
// `Arc<dyn VectorService>` are satisfied trivially — no cross-thread
// aliasing or data races are possible.
unsafe impl Send for BrowserVectorService {}
unsafe impl Sync for BrowserVectorService {}

impl Default for BrowserVectorService {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserVectorService {
    pub fn new() -> Self {
        Self {
            indexes: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the config for `name`, hydrating from the persisted registry
    /// row on a cache miss before concluding the index is genuinely absent.
    /// A miss can mean either "no such index" or "cold cache after a
    /// Service Worker restart" — see the `IndexState` doc comment.
    fn lookup(&self, name: &str) -> VResult<Option<IndexState>> {
        if let Some(state) = self
            .indexes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(name)
            .cloned()
        {
            return Ok(Some(state));
        }
        self.hydrate(name)
    }

    /// Reads `name`'s registry row (if any) and rebuilds it into the
    /// in-memory cache. Returns `Ok(None)` when there is no such row —
    /// either the index was never created, or it predates this table
    /// (unrecoverable; falls back to `IndexNotFound` like a genuinely
    /// missing index).
    fn hydrate(&self, name: &str) -> VResult<Option<IndexState>> {
        // Idempotent — guarantees the table exists so the SELECT below
        // can't fail with "no such table" on a DB that has never had any
        // index created in it yet.
        bridge::db_exec_raw(&sql::build_registry_ddl(), "[]")
            .map_err(|e| VectorError::Internal(js_err(e)))?;

        let Some(state) = self.read_registry_row(name)? else {
            return Ok(None);
        };
        self.indexes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(name.to_string(), state.clone());
        Ok(Some(state))
    }

    /// Reads and parses `name`'s registry row, without touching the
    /// in-memory cache. Assumes the registry table already exists (callers
    /// run `sql::build_registry_ddl()` first). Shared by `hydrate` (cache
    /// rebuild) and `create_index` (re-create guard).
    fn read_registry_row(&self, name: &str) -> VResult<Option<IndexState>> {
        let (query, params) = sql::build_registry_select_sql(name);
        let json =
            bridge::db_query_raw(&query, &params).map_err(|e| VectorError::Internal(js_err(e)))?;
        let rows: Vec<serde_json::Value> = serde_json::from_str(&json)
            .map_err(|e| VectorError::Internal(format!("parse registry row: {e}")))?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        let (dimensions, metric, keyword_search) = sql::parse_registry_row(row)
            .map_err(|e| VectorError::Internal(format!("registry row for {name:?}: {e}")))?;
        Ok(Some(IndexState {
            dimensions,
            metric,
            keyword_search,
        }))
    }
}

#[async_trait::async_trait(?Send)]
impl VectorService for BrowserVectorService {
    async fn create_index(&self, config: VectorIndexConfig) -> VResult<()> {
        // Idempotent — ensures the registry table exists before the select
        // and upsert below, on the very first index ever created in this DB.
        bridge::db_exec_raw(&sql::build_registry_ddl(), "[]")
            .map_err(|e| VectorError::Internal(js_err(e)))?;

        // Guard against a silent config-mismatched overwrite: the
        // `_vectors`/`_meta`/`_fts` DDL below is `IF NOT EXISTS` (idempotent,
        // to support the SW-restart recovery path — see `IndexState`'s doc
        // comment), so without this check, re-calling `create_index` for an
        // EXISTING name with different dimensions/metric/keyword_search
        // would overwrite the registry row and in-memory cache while
        // leaving the already-created tables (and any stored rows) on the
        // old config — bricking the index for subsequent `query`/`upsert`.
        // Only a genuine name collision (mismatched config) is rejected;
        // an identical re-create is the legitimate recovery case and must
        // stay a no-op (matches native's `IndexAlreadyExists` contract for
        // the collision case, see `wafer-block-sqlite`'s
        // `create_index_duplicate_fails`).
        if let Some(existing) = self.read_registry_row(&config.name)? {
            let existing_tuple = (
                existing.dimensions,
                existing.metric,
                existing.keyword_search,
            );
            let incoming_tuple = (config.dimensions, config.metric, config.keyword_search);
            if sql::classify_registry_conflict(Some(existing_tuple), incoming_tuple)
                == sql::RegistryConflict::Mismatch
            {
                return Err(VectorError::IndexAlreadyExists(config.name));
            }
        }

        let stmts = sql::build_create_index_sql(&config.name, config.keyword_search);
        for s in stmts {
            bridge::db_exec_raw(&s, "[]").map_err(|e| VectorError::Internal(js_err(e)))?;
        }

        // Persist the config so a future cold cache (post-SW-restart) can
        // hydrate this index instead of returning `IndexNotFound`.
        let reg = sql::build_registry_upsert_sql(
            &config.name,
            config.dimensions,
            config.metric,
            config.keyword_search,
        );
        bridge::db_exec_raw(&reg.sql, &reg.params_json)
            .map_err(|e| VectorError::Internal(js_err(e)))?;

        bridge::dbFlush()
            .await
            .map_err(|e| VectorError::Internal(js_err(e)))?;
        self.indexes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                config.name.clone(),
                IndexState {
                    dimensions: config.dimensions,
                    metric: config.metric,
                    keyword_search: config.keyword_search,
                },
            );
        Ok(())
    }

    async fn delete_index(&self, name: &str) -> VResult<()> {
        let state = self
            .lookup(name)?
            .ok_or_else(|| VectorError::IndexNotFound(name.into()))?;
        let stmts = sql::build_delete_index_sql(name, state.keyword_search);
        for s in stmts {
            bridge::db_exec_raw(&s, "[]").map_err(|e| VectorError::Internal(js_err(e)))?;
        }

        // Clear the registry row too — otherwise a later `lookup` miss
        // would hydrate a phantom `IndexState` for tables that no longer
        // exist, turning what should be `IndexNotFound` into an
        // `Internal` "no such table" error on the next call.
        let (del_sql, del_params) = sql::build_registry_delete_sql(name);
        bridge::db_exec_raw(&del_sql, &del_params).map_err(|e| VectorError::Internal(js_err(e)))?;

        bridge::dbFlush()
            .await
            .map_err(|e| VectorError::Internal(js_err(e)))?;
        self.indexes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(name);
        Ok(())
    }

    async fn upsert(&self, index: &str, entries: Vec<VectorEntry>) -> VResult<()> {
        let state = self
            .lookup(index)?
            .ok_or_else(|| VectorError::IndexNotFound(index.into()))?;

        use base64ct::{Base64, Encoding};
        let prepared: Result<Vec<sql::SqlUpsertEntry>, VectorError> = entries
            .iter()
            .map(|e| {
                if e.vector.len() as u32 != state.dimensions {
                    return Err(VectorError::DimensionMismatch {
                        expected: state.dimensions,
                        got: e.vector.len() as u32,
                    });
                }
                if state.keyword_search && e.text.is_none() {
                    return Err(VectorError::TextRequired);
                }
                let blob = sql::pack_vector_blob(&e.vector);
                Ok(sql::SqlUpsertEntry {
                    id: e.id.clone(),
                    vector_blob_b64: Base64::encode_string(&blob),
                    metadata_json: e
                        .metadata
                        .as_ref()
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "{}".into()),
                    text: e.text.clone(),
                })
            })
            .collect();
        let prepared = prepared?;

        for stmt in sql::build_upsert_sql_stmts(index, state.keyword_search, &prepared) {
            bridge::db_exec_raw(&stmt.sql, &stmt.params_json)
                .map_err(|e| VectorError::Internal(js_err(e)))?;
        }
        bridge::dbFlush()
            .await
            .map_err(|e| VectorError::Internal(js_err(e)))?;
        Ok(())
    }

    async fn query(
        &self,
        index: &str,
        vector: Vec<f32>,
        top_k: usize,
        filter: Option<MetadataFilter>,
        mode: SearchMode,
        keyword_query: Option<String>,
    ) -> VResult<Vec<VectorMatch>> {
        let state = self
            .lookup(index)?
            .ok_or_else(|| VectorError::IndexNotFound(index.into()))?;

        let needs_keyword = matches!(mode, SearchMode::Keyword | SearchMode::Hybrid);
        if needs_keyword && !state.keyword_search {
            return Err(VectorError::KeywordSearchNotEnabled);
        }
        if needs_keyword && keyword_query.as_deref().unwrap_or("").is_empty() {
            return Err(VectorError::KeywordQueryRequired(mode));
        }
        if mode != SearchMode::Keyword && vector.len() as u32 != state.dimensions {
            return Err(VectorError::DimensionMismatch {
                expected: state.dimensions,
                got: vector.len() as u32,
            });
        }

        let f = filter.unwrap_or_default();
        let fetch_n = if matches!(mode, SearchMode::Hybrid) {
            50.max(top_k)
        } else {
            top_k
        };

        use crate::vector::score;

        match mode {
            SearchMode::Vector => {
                let candidates = load_all_vectors(index, state.dimensions, &f)?;
                let scored = score::top_k_borrowed(
                    &vector,
                    candidates
                        .iter()
                        .map(|(id, v, _m)| (id.as_str(), v.as_slice())),
                    fetch_n,
                    state.metric,
                );
                Ok(attach_metadata(&candidates, scored))
            }
            SearchMode::Keyword => {
                let kq =
                    keyword_query.ok_or(VectorError::KeywordQueryRequired(SearchMode::Keyword))?;
                let ids = fts_search(index, &kq, fetch_n)?;
                let metadata = load_metadata_for_ids(index, &ids)?;
                Ok(ids
                    .into_iter()
                    .enumerate()
                    .filter_map(|(rank, id)| {
                        let m = metadata.get(&id).cloned().flatten();
                        if !matches_filter(m.as_ref(), &f) {
                            return None;
                        }
                        Some(VectorMatch {
                            id,
                            score: 1.0 / (1.0 + rank as f32),
                            metadata: m,
                        })
                    })
                    .collect())
            }
            SearchMode::Hybrid => {
                let kq =
                    keyword_query.ok_or(VectorError::KeywordQueryRequired(SearchMode::Hybrid))?;
                let candidates = load_all_vectors(index, state.dimensions, &f)?;
                let vec_top = score::top_k_borrowed(
                    &vector,
                    candidates
                        .iter()
                        .map(|(id, v, _m)| (id.as_str(), v.as_slice())),
                    fetch_n,
                    state.metric,
                );
                let kw_top = fts_search(index, &kq, fetch_n)?;

                // Inline RRF fusion. We need per-id scores in the response,
                // so we don't use wafer_core::rrf::fuse (which discards scores).
                const RRF_K: f32 = 60.0;
                let mut rrf: std::collections::HashMap<String, f32> =
                    std::collections::HashMap::new();
                for (rank, (id, _)) in vec_top.iter().enumerate() {
                    *rrf.entry(id.clone()).or_insert(0.0) += 1.0 / (RRF_K + (rank + 1) as f32);
                }
                for (rank, id) in kw_top.iter().enumerate() {
                    *rrf.entry(id.clone()).or_insert(0.0) += 1.0 / (RRF_K + (rank + 1) as f32);
                }
                let mut fused: Vec<(String, f32)> = rrf.into_iter().collect();
                fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

                // Hydrate metadata from both sources: vector candidates carry it
                // already, FTS-only ids need a separate meta lookup.
                let mut by_id: std::collections::HashMap<String, Option<serde_json::Value>> =
                    candidates.into_iter().map(|(id, _v, m)| (id, m)).collect();
                let kw_only_ids: Vec<String> = kw_top
                    .into_iter()
                    .filter(|id| !by_id.contains_key(id))
                    .collect();
                let kw_meta = load_metadata_for_ids(index, &kw_only_ids)?;
                for (id, m) in kw_meta {
                    by_id.insert(id, m);
                }

                Ok(fused
                    .into_iter()
                    .take(top_k)
                    .map(|(id, score)| VectorMatch {
                        metadata: by_id.get(&id).cloned().flatten(),
                        id,
                        score,
                    })
                    .collect())
            }
        }
    }

    async fn delete(&self, index: &str, ids: Vec<String>) -> VResult<()> {
        let state = self
            .lookup(index)?
            .ok_or_else(|| VectorError::IndexNotFound(index.into()))?;
        if ids.is_empty() {
            return Ok(());
        }
        let (stmts, params) = sql::build_delete_ids_sql(index, &ids, state.keyword_search);
        let params_json = serde_json::to_string(&params).unwrap_or_else(|_| "[]".into());
        for s in stmts {
            bridge::db_exec_raw(&s, &params_json).map_err(|e| VectorError::Internal(js_err(e)))?;
        }
        bridge::dbFlush()
            .await
            .map_err(|e| VectorError::Internal(js_err(e)))?;
        Ok(())
    }

    async fn count(&self, index: &str) -> VResult<u64> {
        if self.lookup(index)?.is_none() {
            return Err(VectorError::IndexNotFound(index.into()));
        }
        let row_json = bridge::db_query_raw(&sql::build_count_sql(index), "[]")
            .map_err(|e| VectorError::Internal(js_err(e)))?;
        // sql.js returns rows as `[{ "n": <number> }]`.
        let rows: Vec<serde_json::Value> = serde_json::from_str(&row_json)
            .map_err(|e| VectorError::Internal(format!("parse count: {e}")))?;
        let n = rows
            .first()
            .and_then(|r| r.get("n"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Ok(n)
    }
}

/// A loaded vector row: `(id, vector, metadata)`.
type VectorRow = (String, Vec<f32>, Option<serde_json::Value>);

fn load_all_vectors(index: &str, dims: u32, f: &MetadataFilter) -> VResult<Vec<VectorRow>> {
    let s = format!(r#"SELECT id, vector, metadata FROM "{index}_vectors""#);
    let json = bridge::db_query_raw(&s, "[]").map_err(|e| VectorError::Internal(js_err(e)))?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json)
        .map_err(|e| VectorError::Internal(format!("parse vectors: {e}")))?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let id = r
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // sql.js returns BLOBs as `Uint8Array`, which serializes as JSON
        // arrays of integers when shipped through JSON.stringify. We accept
        // both: array of numbers OR base64 string (forward compat).
        let bytes: Vec<u8> = match r.get("vector") {
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_u64().map(|x| x as u8))
                .collect(),
            Some(serde_json::Value::String(b64)) => {
                use base64ct::{Base64, Encoding};
                Base64::decode_vec(b64)
                    .map_err(|e| VectorError::Internal(format!("blob b64: {e}")))?
            }
            _ => continue,
        };
        let vector = sql::parse_vector_blob(&bytes, dims).map_err(VectorError::Internal)?;
        let metadata: Option<serde_json::Value> = r
            .get("metadata")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str(s).ok());
        if !matches_filter(metadata.as_ref(), f) {
            continue;
        }
        out.push((id, vector, metadata));
    }
    Ok(out)
}

fn fts_search(index: &str, query: &str, limit: usize) -> VResult<Vec<String>> {
    let s = format!(
        r#"SELECT id FROM "{index}_fts" WHERE "{index}_fts" MATCH ? ORDER BY rank LIMIT ?"#
    );
    let params = serde_json::json!([query, limit]).to_string();
    let json = bridge::db_query_raw(&s, &params).map_err(|e| VectorError::Internal(js_err(e)))?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json)
        .map_err(|e| VectorError::Internal(format!("parse fts: {e}")))?;
    Ok(rows
        .into_iter()
        .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(String::from))
        .collect())
}

fn load_metadata_for_ids(
    index: &str,
    ids: &[String],
) -> VResult<std::collections::HashMap<String, Option<serde_json::Value>>> {
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let s = format!(r#"SELECT id, metadata FROM "{index}_meta" WHERE id IN ({placeholders})"#);
    let params = serde_json::to_string(ids).unwrap_or_else(|_| "[]".into());
    let json = bridge::db_query_raw(&s, &params).map_err(|e| VectorError::Internal(js_err(e)))?;
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json)
        .map_err(|e| VectorError::Internal(format!("parse meta: {e}")))?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let id = r.get("id")?.as_str()?.to_string();
            let md: Option<serde_json::Value> = r
                .get("metadata")
                .and_then(|v| v.as_str())
                .and_then(|s| serde_json::from_str(s).ok());
            Some((id, md))
        })
        .collect())
}

fn attach_metadata(
    cands: &[(String, Vec<f32>, Option<serde_json::Value>)],
    scored: Vec<(String, f32)>,
) -> Vec<VectorMatch> {
    let by_id: std::collections::HashMap<&str, &Option<serde_json::Value>> =
        cands.iter().map(|(id, _, m)| (id.as_str(), m)).collect();
    scored
        .into_iter()
        .map(|(id, score)| VectorMatch {
            metadata: by_id.get(id.as_str()).copied().cloned().unwrap_or(None),
            id,
            score,
        })
        .collect()
}
