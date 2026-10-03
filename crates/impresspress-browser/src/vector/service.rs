//! `BrowserVectorService` — sql.js-backed `VectorService`.
//!
//! Vectors are stored as `BLOB` columns in the shared OPFS sql.js database
//! (no separate file). Scoring is in-process Rust using SIMD on wasm32. FTS5
//! powers keyword search when the index has `keyword_search: true`.

use std::{collections::HashMap, sync::Mutex};

use wafer_core::interfaces::vector::{
    self as vector_rrf,
    service::{
        ColumnInfo, DescribeIndexResponse, DistanceMetric, MetadataFilter, Result as VResult,
        SearchMode, VectorEntry, VectorError, VectorIndexConfig, VectorMatch, VectorService,
    },
};

use crate::{
    bridge, database, db_codec,
    vector::sql::{self, IndexName},
};

fn js_err(e: wasm_bindgen::JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
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
    fn lookup(&self, name: &IndexName) -> VResult<Option<IndexState>> {
        if let Some(state) = self
            .indexes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(name.as_str())
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
    fn hydrate(&self, name: &IndexName) -> VResult<Option<IndexState>> {
        // Idempotent — guarantees the table exists so the SELECT below
        // can't fail with "no such table" on a DB that has never had any
        // index created in it yet.
        exec_ddl(
            &[sql::build_registry_ddl()],
            &[sql::REGISTRY_TABLE.to_string()],
        )?;

        let Some(state) = self.read_registry_row(name)? else {
            return Ok(None);
        };
        self.indexes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(name.as_str().to_string(), state.clone());
        Ok(Some(state))
    }

    /// Reads and parses `name`'s registry row, without touching the
    /// in-memory cache. Assumes the registry table already exists (callers
    /// run `sql::build_registry_ddl()` first). Shared by `hydrate` (cache
    /// rebuild) and `create_index` (re-create guard).
    fn read_registry_row(&self, name: &IndexName) -> VResult<Option<IndexState>> {
        let (query, params) = sql::build_registry_select_sql(name);
        let params_js = db_codec::params_to_js(&params).map_err(VectorError::Internal)?;
        let value = bridge::db_query_raw(&query, params_js)
            .map_err(|e| VectorError::Internal(js_err(e)))?;
        let rows = db_codec::rows_from_js(value).map_err(VectorError::Internal)?;
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
        let name = IndexName::parse(&config.name)?;
        // Everything from the first DDL statement to the last is one logical
        // mutation with one OPFS flush at the end — including the
        // config-mismatch refusal below, which is reached only AFTER the
        // registry DDL has already run. The hand-written `dbFlush()?` this
        // replaced returned early on that path and left the registry table
        // in memory only.
        database::with_flush_mapped(
            self.create_index_statements(&name, &config),
            VectorError::Internal,
        )
        .await?;

        self.indexes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                name.as_str().to_string(),
                IndexState {
                    dimensions: config.dimensions,
                    metric: config.metric,
                    keyword_search: config.keyword_search,
                },
            );
        Ok(())
    }

    async fn delete_index(&self, name: &str) -> VResult<()> {
        let name = IndexName::parse(name)?;
        // Read-only, so it stays outside the flush: a miss must not cost a
        // whole-database write to OPFS.
        let state = self
            .lookup(&name)?
            .ok_or_else(|| VectorError::IndexNotFound(name.as_str().into()))?;

        database::with_flush_mapped(
            self.delete_index_statements(&name, state.keyword_search),
            VectorError::Internal,
        )
        .await?;

        self.indexes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(name.as_str());
        Ok(())
    }

    /// Move a legacy mixed-case index to its lowercase name: tables, rows,
    /// keyword search and registry row, in one transaction.
    ///
    /// `from` must be registered under exactly that spelling
    /// (`IndexNotFound` otherwise, which a startup migration reads as
    /// already moved). A registry row for `to`, or any table `to` would
    /// occupy under any spelling, is `IndexAlreadyExists`: two indexes that
    /// differ only by case are never merged. sql.js shares SQLite's
    /// case-insensitive table names, so `Docs` and `docs` registry rows can
    /// point at the same tables; refusing leaves the operator to delete one.
    async fn rename_index(&self, from: &str, to: &str) -> VResult<()> {
        let (from, to) = IndexName::rename(from, to)?;
        exec_ddl(
            &[sql::build_registry_ddl()],
            &[sql::REGISTRY_TABLE.to_string()],
        )?;
        let state = self
            .read_registry_row(&from)?
            .ok_or_else(|| VectorError::IndexNotFound(from.as_str().into()))?;
        if self.read_registry_row(&to)?.is_some() {
            return Err(VectorError::IndexAlreadyExists(to.as_str().into()));
        }
        let (conflicts, conflict_params) = sql::build_rename_conflicts_sql(&from, &to);
        let params_js = db_codec::params_to_js(&conflict_params).map_err(VectorError::Internal)?;
        let taken = bridge::db_query_raw(&conflicts, params_js)
            .map_err(|e| VectorError::Internal(js_err(e)))?;
        if !db_codec::rows_from_js(taken)
            .map_err(VectorError::Internal)?
            .is_empty()
        {
            return Err(VectorError::IndexAlreadyExists(to.as_str().into()));
        }

        let mut touched = sql::index_tables(&from, true);
        touched.extend(sql::index_tables(&to, true));
        database::with_flush_mapped(
            async {
                let moved = in_transaction(&sql::build_rename_index_sql(
                    &from,
                    &to,
                    state.keyword_search,
                ));
                for table in &touched {
                    database::forget_table_schema(table);
                }
                moved
            },
            VectorError::Internal,
        )
        .await?;

        let mut indexes = self.indexes.lock().unwrap_or_else(|p| p.into_inner());
        indexes.remove(from.as_str());
        indexes.insert(to.as_str().to_string(), state);
        Ok(())
    }

    /// Every index registered under `prefix`, in lexical order. The
    /// registry is this backend's catalog: `create_index` writes a row for
    /// every index it creates, and an index without one cannot be opened
    /// here at all.
    async fn list_indexes(&self, prefix: &str) -> VResult<Vec<String>> {
        exec_ddl(
            &[sql::build_registry_ddl()],
            &[sql::REGISTRY_TABLE.to_string()],
        )?;
        let (query, params) = sql::build_registry_list_sql(prefix);
        let params_js = db_codec::params_to_js(&params).map_err(VectorError::Internal)?;
        let value = bridge::db_query_raw(&query, params_js)
            .map_err(|e| VectorError::Internal(js_err(e)))?;
        Ok(db_codec::rows_from_js(value)
            .map_err(VectorError::Internal)?
            .iter()
            .filter_map(|row| row.get("name").and_then(|v| v.as_str()).map(str::to_string))
            .collect())
    }

    async fn upsert(&self, index: &str, entries: Vec<VectorEntry>) -> VResult<()> {
        let index = IndexName::parse(index)?;
        let state = self
            .lookup(&index)?
            .ok_or_else(|| VectorError::IndexNotFound(index.as_str().into()))?;

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
        // Validation is pure and rejects the whole batch, so it too stays
        // outside the flush — nothing has been written yet.
        let prepared = prepared?;

        database::with_flush_mapped(
            async {
                for stmt in sql::build_upsert_sql_stmts(&index, state.keyword_search, &prepared) {
                    let params_js =
                        db_codec::params_to_js(&stmt.params).map_err(VectorError::Internal)?;
                    bridge::db_exec_raw(&stmt.sql, params_js)
                        .map_err(|e| VectorError::Internal(js_err(e)))?;
                }
                Ok(())
            },
            VectorError::Internal,
        )
        .await
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
        let index = IndexName::parse(index)?;
        let state = self
            .lookup(&index)?
            .ok_or_else(|| VectorError::IndexNotFound(index.as_str().into()))?;

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
                let candidates = load_all_vectors(&index, state.dimensions, &f)?;
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
                let ids = fts_search(&index, &kq, fetch_n)?;
                let metadata = load_metadata_for_ids(&index, &ids)?;
                Ok(ids
                    .into_iter()
                    .enumerate()
                    .filter_map(|(rank, id)| {
                        let m = metadata.get(&id).cloned().flatten();
                        if !f.matches(m.as_ref()) {
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
                let candidates = load_all_vectors(&index, state.dimensions, &f)?;
                let vec_top = score::top_k_borrowed(
                    &vector,
                    candidates
                        .iter()
                        .map(|(id, v, _m)| (id.as_str(), v.as_slice())),
                    fetch_n,
                    state.metric,
                );
                let kw_top = fts_search(&index, &kq, fetch_n)?;

                // Reciprocal Rank Fusion, from the shared implementation the
                // native sqlite-vec backend also fuses with. `fuse_scored`
                // keeps the real RRF value (the inline copy this replaced
                // existed because of a comment claiming only the
                // score-discarding `fuse` was available), truncates to
                // `top_k` itself, and breaks score ties by id — so two ids
                // that fuse to the same score come back in a stable order
                // instead of whatever the old `HashMap` iteration produced.
                let vec_ids: Vec<String> = vec_top.iter().map(|(id, _)| id.clone()).collect();
                let fused = vector_rrf::fuse_scored(
                    &[vec_ids, kw_top.clone()],
                    top_k,
                    vector_rrf::DEFAULT_RRF_K,
                );

                // Hydrate metadata from both sources: vector candidates carry it
                // already, FTS-only ids need a separate meta lookup.
                let mut by_id: std::collections::HashMap<String, Option<serde_json::Value>> =
                    candidates.into_iter().map(|(id, _v, m)| (id, m)).collect();
                let kw_only_ids: Vec<String> = kw_top
                    .into_iter()
                    .filter(|id| !by_id.contains_key(id))
                    .collect();
                let kw_meta = load_metadata_for_ids(&index, &kw_only_ids)?;
                for (id, m) in kw_meta {
                    by_id.insert(id, m);
                }

                Ok(fused
                    .into_iter()
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
        let index = IndexName::parse(index)?;
        let state = self
            .lookup(&index)?
            .ok_or_else(|| VectorError::IndexNotFound(index.as_str().into()))?;
        // Nothing to write, so nothing to flush.
        if ids.is_empty() {
            return Ok(());
        }
        database::with_flush_mapped(
            async {
                let (stmts, id_params) =
                    sql::build_delete_ids_sql(&index, &ids, state.keyword_search);
                let params: Vec<serde_json::Value> = id_params
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect();
                let params_js = db_codec::params_to_js(&params).map_err(VectorError::Internal)?;
                for s in stmts {
                    bridge::db_exec_raw(&s, params_js.clone())
                        .map_err(|e| VectorError::Internal(js_err(e)))?;
                }
                Ok(())
            },
            VectorError::Internal,
        )
        .await
    }

    /// The meta table's real state: whether it exists, its columns in
    /// declaration order, and whether the index has an FTS table. Absence is
    /// `exists: false`, not an error. The same probe the native backend runs,
    /// keyed on the tables rather than on the registry row, because what this
    /// op promises is what is on disk.
    async fn describe_index(&self, index: &str) -> VResult<DescribeIndexResponse> {
        let index = IndexName::parse(index)?;
        let meta = sql::meta_table(&index);
        if !table_exists(&meta)? {
            return Ok(DescribeIndexResponse {
                exists: false,
                columns: Vec::new(),
                keyword_search: false,
            });
        }
        let (query, params) = sql::build_table_columns_sql(&meta);
        let columns = query_rows(&query, &params)?
            .iter()
            .map(|row| {
                let text = |key: &str| {
                    row.get(key)
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .ok_or_else(|| {
                            VectorError::Internal(format!("{meta} column row has no {key}: {row}"))
                        })
                };
                Ok(ColumnInfo {
                    name: text("name")?,
                    sql_type: text("type")?,
                })
            })
            .collect::<VResult<Vec<_>>>()?;
        let keyword_search = table_exists(&sql::fts_table(&index))?;
        Ok(DescribeIndexResponse {
            exists: true,
            columns,
            keyword_search,
        })
    }

    /// Ids of the entries whose metadata satisfies every `filter.equals`
    /// condition. The filter must be non-empty and its values JSON strings or
    /// numbers, as the trait requires of every backend.
    ///
    /// Two differences from the native backend, both this backend's own
    /// rules rather than the trait's:
    /// - An index is missing when it has no registry row — the test every
    ///   other op here applies (see `IndexState`) — where native looks for
    ///   the `_meta` table.
    /// - A condition holds as [`MetadataFilter::matches`] defines it, the
    ///   predicate `query` filters with here: JSON value equality, so `1`
    ///   matches neither `1.0` nor `true`. Native compares `json_extract`
    ///   output with SQLite's `=`, under which `1` and `1.0` are equal.
    async fn list_ids(&self, index: &str, filter: MetadataFilter) -> VResult<Vec<String>> {
        check_list_ids_filter(&filter)?;
        let index = IndexName::parse(index)?;
        if self.lookup(&index)?.is_none() {
            return Err(VectorError::IndexNotFound(index.as_str().into()));
        }
        Ok(query_rows(&sql::build_select_meta_sql(&index), &[])?
            .into_iter()
            .filter_map(|row| {
                let id = row.get("id")?.as_str()?.to_string();
                let metadata: Option<serde_json::Value> = row
                    .get("metadata")
                    .and_then(|v| v.as_str())
                    .and_then(|s| serde_json::from_str(s).ok());
                filter.matches(metadata.as_ref()).then_some(id)
            })
            .collect())
    }

    async fn count(&self, index: &str) -> VResult<u64> {
        let index = IndexName::parse(index)?;
        if self.lookup(&index)?.is_none() {
            return Err(VectorError::IndexNotFound(index.as_str().into()));
        }
        let value = bridge::db_query_raw(&sql::build_count_sql(&index), db_codec::empty_params())
            .map_err(|e| VectorError::Internal(js_err(e)))?;
        // sql.js returns rows as `[{ "n": <number> }]`.
        let rows = db_codec::rows_from_js(value).map_err(VectorError::Internal)?;
        let n = rows
            .first()
            .and_then(|r| r.get("n"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Ok(n)
    }
}

impl BrowserVectorService {
    /// Every statement `create_index` writes, as one future so the caller can
    /// wrap it in a single flush. Returns without touching the in-memory
    /// index cache — that update belongs after the write is durable.
    async fn create_index_statements(
        &self,
        name: &IndexName,
        config: &VectorIndexConfig,
    ) -> VResult<()> {
        // Idempotent — ensures the registry table exists before the select
        // and upsert below, on the very first index ever created in this DB.
        exec_ddl(
            &[sql::build_registry_ddl()],
            &[sql::REGISTRY_TABLE.to_string()],
        )?;

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
        if let Some(existing) = self.read_registry_row(name)? {
            let existing_tuple = (
                existing.dimensions,
                existing.metric,
                existing.keyword_search,
            );
            let incoming_tuple = (config.dimensions, config.metric, config.keyword_search);
            if sql::classify_registry_conflict(Some(existing_tuple), incoming_tuple)
                == sql::RegistryConflict::Mismatch
            {
                return Err(VectorError::IndexAlreadyExists(config.name.clone()));
            }
        }

        exec_ddl(
            &sql::build_create_index_sql(name, config.keyword_search),
            &sql::index_tables(name, config.keyword_search),
        )?;

        // Persist the config so a future cold cache (post-SW-restart) can
        // hydrate this index instead of returning `IndexNotFound`.
        let reg = sql::build_registry_upsert_sql(
            name,
            config.dimensions,
            config.metric,
            config.keyword_search,
        );
        let reg_params = db_codec::params_to_js(&reg.params).map_err(VectorError::Internal)?;
        bridge::db_exec_raw(&reg.sql, reg_params).map_err(|e| VectorError::Internal(js_err(e)))?;
        Ok(())
    }

    /// Every statement `delete_index` writes, as one future. The in-memory
    /// cache eviction happens in the caller, after the write is durable.
    async fn delete_index_statements(&self, name: &IndexName, keyword_search: bool) -> VResult<()> {
        exec_ddl(
            &sql::build_delete_index_sql(name, keyword_search),
            &sql::index_tables(name, keyword_search),
        )?;

        // Clear the registry row too — otherwise a later `lookup` miss
        // would hydrate a phantom `IndexState` for tables that no longer
        // exist, turning what should be `IndexNotFound` into an
        // `Internal` "no such table" error on the next call.
        let (del_sql, del_params) = sql::build_registry_delete_sql(name);
        let del_params_js = db_codec::params_to_js(&del_params).map_err(VectorError::Internal)?;
        bridge::db_exec_raw(&del_sql, del_params_js)
            .map_err(|e| VectorError::Internal(js_err(e)))?;
        Ok(())
    }
}

/// Run `statements` — this module's DDL — through the bridge, then drop the
/// database service's cached schema for `tables`.
///
/// The invalidation happens as soon as the statements have run, whatever they
/// returned, and before this function yields: the DDL does not go through
/// `DbExec`, so nothing else invalidates for it, and a `DatabaseService` read
/// polled in between would otherwise memoize a schema fact this DDL has
/// already invalidated (a table it just created as "missing", one it just
/// dropped as "present"). A failed statement may still have applied some of
/// its predecessors, so the failure path invalidates too.
///
/// Only the tables the statements touch are forgotten, so an index's DDL does
/// not cost the rest of the database its memoized schema.
fn exec_ddl(statements: &[String], tables: &[String]) -> VResult<()> {
    let mut ran = Ok(());
    for statement in statements {
        ran = bridge::db_exec_raw(statement, db_codec::empty_params())
            .map(|_| ())
            .map_err(|e| VectorError::Internal(js_err(e)));
        if ran.is_err() {
            break;
        }
    }
    for table in tables {
        database::forget_table_schema(table);
    }
    ran
}

/// The `list_ids` filter rule every backend applies: at least one condition,
/// and every value a JSON string or number. An unconditioned id dump is not a
/// supported query shape.
fn check_list_ids_filter(filter: &MetadataFilter) -> VResult<()> {
    if filter.equals.is_empty() {
        return Err(VectorError::InvalidMetadataFilter(
            "filter.equals must contain at least one condition".into(),
        ));
    }
    for (path, value) in &filter.equals {
        if !(value.is_string() || value.is_number()) {
            return Err(VectorError::InvalidMetadataFilter(format!(
                "value for path {path:?} must be a JSON string or number, got {value}"
            )));
        }
    }
    Ok(())
}

/// Run a read through the bridge and decode its rows as plain JSON objects.
fn query_rows(query: &str, params: &[serde_json::Value]) -> VResult<Vec<serde_json::Value>> {
    let params_js = db_codec::params_to_js(params).map_err(VectorError::Internal)?;
    let value =
        bridge::db_query_raw(query, params_js).map_err(|e| VectorError::Internal(js_err(e)))?;
    db_codec::rows_from_js(value).map_err(VectorError::Internal)
}

/// Whether a table is named exactly `table`.
fn table_exists(table: &str) -> VResult<bool> {
    let (query, params) = sql::build_table_exists_sql(table);
    Ok(!query_rows(&query, &params)?.is_empty())
}

/// Run `statements` as one transaction — the crate's one framing,
/// [`database::in_transaction`] — so a half-moved index never persists.
fn in_transaction(statements: &[sql::PreparedStmt]) -> VResult<()> {
    database::in_transaction(database::bridge_control, || {
        statements.iter().try_for_each(|stmt| {
            let params_js = db_codec::params_to_js(&stmt.params).map_err(VectorError::Internal)?;
            bridge::db_exec_raw(&stmt.sql, params_js)
                .map(|_| ())
                .map_err(|e| VectorError::Internal(js_err(e)))
        })
    })
}

impl database::TxError for VectorError {
    fn refused(message: String) -> Self {
        VectorError::Internal(message)
    }

    fn stuck(message: String) -> Self {
        VectorError::Internal(message)
    }
}

/// A loaded vector row: `(id, vector, metadata)`.
type VectorRow = (String, Vec<f32>, Option<serde_json::Value>);

/// Raw shape of one `_vectors` table row, decoded straight off the
/// `serde_wasm_bindgen` boundary — NOT via the generic
/// `db_codec::rows_from_js`/`serde_json::Value` row decode.
///
/// sql.js resolves the `vector` BLOB column as a real `Uint8Array`.
/// `serde_json::Value`'s `Deserialize` impl has no `visit_bytes`/
/// `visit_byte_buf`, so decoding a row containing a BLOB column generically
/// as `serde_json::Value` always fails (`invalid type: byte array`) — this
/// broke `query()` on every non-empty index. Declaring `vector: Vec<u8>` on
/// a concrete struct and decoding via `serde_wasm_bindgen::from_value`
/// directly sidesteps that: `serde_wasm_bindgen` deserializes a `Uint8Array`
/// straight into `Vec<u8>` in one step, exactly like `storage.rs`'s
/// `GetResponse.data` and `network.rs`'s `FetchResponse.body`.
#[derive(serde::Deserialize)]
struct VectorBlobRow {
    id: String,
    vector: Vec<u8>,
    metadata: Option<String>,
}

fn load_all_vectors(index: &IndexName, dims: u32, f: &MetadataFilter) -> VResult<Vec<VectorRow>> {
    let s = format!(r#"SELECT id, vector, metadata FROM "{index}_vectors""#);
    let value = bridge::db_query_raw(&s, db_codec::empty_params())
        .map_err(|e| VectorError::Internal(js_err(e)))?;
    let rows: Vec<VectorBlobRow> = serde_wasm_bindgen::from_value(value)
        .map_err(|e| VectorError::Internal(format!("decode vector rows: {e}")))?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let (id, vector, metadata) =
            sql::decode_vector_row(r.id, &r.vector, r.metadata.as_deref(), dims)
                .map_err(VectorError::Internal)?;
        if !f.matches(metadata.as_ref()) {
            continue;
        }
        out.push((id, vector, metadata));
    }
    Ok(out)
}

fn fts_search(index: &IndexName, query: &str, limit: usize) -> VResult<Vec<String>> {
    let s = format!(
        r#"SELECT id FROM "{index}_fts" WHERE "{index}_fts" MATCH ? ORDER BY rank LIMIT ?"#
    );
    let params = vec![serde_json::json!(query), serde_json::json!(limit)];
    let params_js = db_codec::params_to_js(&params).map_err(VectorError::Internal)?;
    let value =
        bridge::db_query_raw(&s, params_js).map_err(|e| VectorError::Internal(js_err(e)))?;
    let rows = db_codec::rows_from_js(value).map_err(VectorError::Internal)?;
    Ok(rows
        .into_iter()
        .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(String::from))
        .collect())
}

fn load_metadata_for_ids(
    index: &IndexName,
    ids: &[String],
) -> VResult<std::collections::HashMap<String, Option<serde_json::Value>>> {
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let s = format!(r#"SELECT id, metadata FROM "{index}_meta" WHERE id IN ({placeholders})"#);
    let params: Vec<serde_json::Value> =
        ids.iter().cloned().map(serde_json::Value::String).collect();
    let params_js = db_codec::params_to_js(&params).map_err(VectorError::Internal)?;
    let value =
        bridge::db_query_raw(&s, params_js).map_err(|e| VectorError::Internal(js_err(e)))?;
    let rows = db_codec::rows_from_js(value).map_err(VectorError::Internal)?;
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

/// This module's DDL does not go through `DbExec`, so it invalidates the
/// database service's cached schema itself ([`exec_ddl`]). A statement that
/// fails is exactly the case the invalidation has to cover: it may still have
/// applied, and a later `DatabaseService` read must not be served a fact the
/// DDL invalidated.
#[cfg(all(test, target_arch = "wasm32"))]
mod schema_invalidation {
    use wafer_core::interfaces::database::exec::DbExec;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{
        exec_ddl, sql, BrowserVectorService, DistanceMetric, VectorIndexConfig, VectorService,
    };
    use crate::database::BrowserDatabaseService;

    fn seed(table: &str) -> &'static wafer_core::interfaces::database::schema_cache::SchemaCache {
        let cache = DbExec::schema_cache(&BrowserDatabaseService).expect("a cache");
        cache.set_primary_key_if_gen(table, vec!["id".into()], cache.generation());
        assert_eq!(cache.primary_key(table), Some(vec!["id".to_string()]));
        cache
    }

    /// The helper forgets every table it was given even when the statements
    /// failed, and forgets nothing else.
    #[wasm_bindgen_test]
    fn failed_ddl_still_forgets_its_tables_and_only_its_tables() {
        let cache = seed("vec_ddl_t");
        seed("vec_untouched_t");

        let ran = exec_ddl(
            // Unterminated, so it fails whether or not sql.js is loaded.
            &[r#"CREATE TABLE "vec_ddl_t" (id TEXT PRIMARY KEY"#.to_string()],
            &["vec_ddl_t".to_string()],
        );

        assert!(ran.is_err(), "the statement is malformed");
        assert_eq!(cache.primary_key("vec_ddl_t"), None);
        assert_eq!(
            cache.primary_key("vec_untouched_t"),
            Some(vec!["id".to_string()]),
            "another table's memoized schema is not collateral"
        );
    }

    /// `create_index` runs the registry DDL first, so the registry table's
    /// facts go whatever the rest of the call does.
    #[wasm_bindgen_test]
    async fn create_index_forgets_the_registry_table() {
        let cache = seed(sql::REGISTRY_TABLE);
        let _ = BrowserVectorService::new()
            .create_index(VectorIndexConfig {
                name: "vec_create_idx".to_string(),
                model: "test-model".to_string(),
                dimensions: 3,
                metric: DistanceMetric::Cosine,
                keyword_search: false,
            })
            .await;
        assert_eq!(cache.primary_key(sql::REGISTRY_TABLE), None);
    }

    /// `delete_index` hydrates first, and the hydrate runs the same registry
    /// DDL.
    #[wasm_bindgen_test]
    async fn delete_index_forgets_the_registry_table_through_hydrate() {
        let cache = seed(sql::REGISTRY_TABLE);
        let _ = BrowserVectorService::new()
            .delete_index("vec_delete_idx")
            .await;
        assert_eq!(cache.primary_key(sql::REGISTRY_TABLE), None);
    }
}

/// `describe_index` and `list_ids` on the real service over real sql.js
/// (`database::test_support`'s in-memory OPFS and fresh database). The trait's
/// defaults for these two answer `Internal("not implemented")`, which is what
/// the admin index detail page and ingest got here before this backend
/// implemented them — so these drive `BrowserVectorService` itself, not a
/// stand-in.
#[cfg(all(test, target_arch = "wasm32"))]
mod introspection {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{
        BrowserVectorService, DistanceMetric, MetadataFilter, VectorEntry, VectorError,
        VectorIndexConfig, VectorService,
    };
    use crate::database::test_support::fresh_db;

    fn config(name: &str, keyword_search: bool) -> VectorIndexConfig {
        VectorIndexConfig {
            name: name.to_string(),
            model: "test-model".to_string(),
            dimensions: 3,
            metric: DistanceMetric::Cosine,
            keyword_search,
        }
    }

    fn entry(id: &str, metadata: serde_json::Value, keyword_search: bool) -> VectorEntry {
        VectorEntry {
            id: id.to_string(),
            vector: vec![1.0, 0.0, 0.0],
            metadata: Some(metadata),
            text: keyword_search.then(|| format!("text of {id}")),
        }
    }

    fn filter(conditions: &[(&str, serde_json::Value)]) -> MetadataFilter {
        let mut f = MetadataFilter::default();
        for (path, value) in conditions {
            f.equals.insert((*path).to_string(), value.clone());
        }
        f
    }

    fn column_names(desc: &super::DescribeIndexResponse) -> Vec<&str> {
        desc.columns.iter().map(|c| c.name.as_str()).collect()
    }

    #[wasm_bindgen_test]
    async fn describe_reports_a_created_index_and_absence_is_data() {
        fresh_db().await;
        let svc = BrowserVectorService::new();
        svc.create_index(config("describe_kw", true))
            .await
            .expect("create");
        svc.create_index(config("describe_plain", false))
            .await
            .expect("create");

        let kw = svc.describe_index("describe_kw").await.expect("describe");
        assert!(kw.exists);
        assert!(kw.keyword_search);
        assert_eq!(column_names(&kw), ["id", "rowid", "metadata", "text"]);
        assert_eq!(kw.columns[0].sql_type, "TEXT");
        assert_eq!(kw.columns[1].sql_type, "INTEGER");

        let plain = svc
            .describe_index("describe_plain")
            .await
            .expect("describe");
        assert!(plain.exists);
        assert!(!plain.keyword_search);
        assert_eq!(column_names(&plain), ["id", "rowid", "metadata"]);

        let missing = svc.describe_index("describe_nope").await.expect("describe");
        assert!(!missing.exists);
        assert!(missing.columns.is_empty());
        assert!(!missing.keyword_search);

        svc.delete_index("describe_plain").await.expect("delete");
        let deleted = svc
            .describe_index("describe_plain")
            .await
            .expect("describe");
        assert!(!deleted.exists, "a deleted index describes as absent");
    }

    /// The describe probe reads the database, not this instance's cache: a
    /// fresh service — a restarted Service Worker — describes an index it
    /// never created.
    #[wasm_bindgen_test]
    async fn describe_reads_the_database_not_the_cache() {
        fresh_db().await;
        BrowserVectorService::new()
            .create_index(config("describe_cold", true))
            .await
            .expect("create");
        let desc = BrowserVectorService::new()
            .describe_index("describe_cold")
            .await
            .expect("describe");
        assert!(desc.exists);
        assert!(desc.keyword_search);
    }

    #[wasm_bindgen_test]
    async fn describe_refuses_a_name_that_is_not_a_plain_identifier() {
        fresh_db().await;
        let err = BrowserVectorService::new()
            .describe_index(r#"x" OR 1"#)
            .await
            .expect_err("refused");
        assert!(matches!(err, VectorError::InvalidIndexName(_)), "{err:?}");
    }

    #[wasm_bindgen_test]
    async fn list_ids_filters_by_metadata_equality() {
        fresh_db().await;
        let svc = BrowserVectorService::new();
        for (name, kw) in [("list_ids_kw", true), ("list_ids_plain", false)] {
            svc.create_index(config(name, kw)).await.expect("create");
            svc.upsert(
                name,
                vec![
                    entry(
                        "a",
                        serde_json::json!({ "document_id": "d1", "page": 1 }),
                        kw,
                    ),
                    entry(
                        "b",
                        serde_json::json!({ "document_id": "d1", "page": 2 }),
                        kw,
                    ),
                    entry(
                        "c",
                        serde_json::json!({ "document_id": "d2", "page": 1 }),
                        kw,
                    ),
                ],
            )
            .await
            .expect("upsert");

            let mut ids = svc
                .list_ids(name, filter(&[("document_id", serde_json::json!("d1"))]))
                .await
                .expect("list_ids");
            ids.sort();
            assert_eq!(ids, ["a", "b"], "{name}");

            // Numbers compare as numbers, and conditions AND.
            let ids = svc
                .list_ids(
                    name,
                    filter(&[
                        ("document_id", serde_json::json!("d1")),
                        ("page", serde_json::json!(2)),
                    ]),
                )
                .await
                .expect("list_ids");
            assert_eq!(ids, ["b"], "{name}");

            // Typed: the string "1" is not the number 1.
            let ids = svc
                .list_ids(name, filter(&[("page", serde_json::json!("1"))]))
                .await
                .expect("list_ids");
            assert!(ids.is_empty(), "{name}: {ids:?}");

            let ids = svc
                .list_ids(name, filter(&[("document_id", serde_json::json!("d9"))]))
                .await
                .expect("list_ids");
            assert!(ids.is_empty(), "{name}: {ids:?}");
        }
    }

    /// The re-ingest path deletes what `list_ids` found: the two together
    /// leave only the other document's entries.
    #[wasm_bindgen_test]
    async fn list_ids_then_delete_clears_one_document() {
        fresh_db().await;
        let svc = BrowserVectorService::new();
        svc.create_index(config("list_ids_reingest", false))
            .await
            .expect("create");
        svc.upsert(
            "list_ids_reingest",
            vec![
                entry("a", serde_json::json!({ "document_id": "d1" }), false),
                entry("b", serde_json::json!({ "document_id": "d2" }), false),
            ],
        )
        .await
        .expect("upsert");
        let prior = svc
            .list_ids(
                "list_ids_reingest",
                filter(&[("document_id", serde_json::json!("d1"))]),
            )
            .await
            .expect("list_ids");
        svc.delete("list_ids_reingest", prior)
            .await
            .expect("delete");
        assert_eq!(svc.count("list_ids_reingest").await.expect("count"), 1);
    }

    #[wasm_bindgen_test]
    async fn list_ids_on_a_missing_index_is_not_found() {
        fresh_db().await;
        let err = BrowserVectorService::new()
            .list_ids("list_ids_nope", filter(&[("k", serde_json::json!("v"))]))
            .await
            .expect_err("missing");
        assert!(matches!(err, VectorError::IndexNotFound(_)), "{err:?}");
    }

    #[wasm_bindgen_test]
    async fn list_ids_refuses_an_empty_or_non_scalar_filter_and_a_bad_name() {
        fresh_db().await;
        let svc = BrowserVectorService::new();
        svc.create_index(config("list_ids_refusals", false))
            .await
            .expect("create");

        for bad in [
            MetadataFilter::default(),
            filter(&[("flag", serde_json::json!(true))]),
            filter(&[("nested", serde_json::json!({ "a": 1 }))]),
            filter(&[("none", serde_json::Value::Null)]),
        ] {
            let err = svc
                .list_ids("list_ids_refusals", bad.clone())
                .await
                .expect_err("refused");
            assert!(
                matches!(err, VectorError::InvalidMetadataFilter(_)),
                "{bad:?}: {err:?}"
            );
        }

        let err = svc
            .list_ids(
                "List_Ids_Refusals",
                filter(&[("k", serde_json::json!("v"))]),
            )
            .await
            .expect_err("refused");
        assert!(matches!(err, VectorError::InvalidIndexName(_)), "{err:?}");
    }
}

/// An index name is spliced into quoted identifiers, and sql.js runs every
/// statement in the string it is handed — so a name carrying `"` or `;` would
/// reach other tables or append statements. The host only checks a caller's
/// `{org}__{block}__` prefix, so every op here must refuse such a name before
/// any SQL runs. Each op is driven with such names over real sql.js, and the
/// database is shown untouched: the same catalog (so the registry DDL did not
/// run either) and no changed rows.
#[cfg(all(test, target_arch = "wasm32"))]
mod hostile_names {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{
        BrowserVectorService, DistanceMetric, MetadataFilter, SearchMode, VectorEntry, VectorError,
        VectorIndexConfig, VectorService,
    };
    use crate::{bridge, database::test_support::fresh_db, db_codec};

    const HOSTILE: [&str; 3] = [
        r#"evil__block__a" ; DROP TABLE "sentinel"; --"#,
        r#"evil__block__a"_meta" WHERE 1=1 UNION SELECT name, sql FROM sqlite_master --"#,
        "evil__block__a;b",
    ];

    /// Everything a statement could have changed: the catalog, and the
    /// connection's count of changed rows.
    fn snapshot() -> serde_json::Value {
        let rows = |sql: &str| {
            db_codec::rows_from_js(
                bridge::db_query_raw(sql, db_codec::empty_params()).expect("snapshot query"),
            )
            .expect("decode")
        };
        serde_json::json!({
            "catalog": rows("SELECT type, name, sql FROM sqlite_master ORDER BY name"),
            "changes": rows("SELECT total_changes() AS n"),
        })
    }

    async fn sentinel_db() -> serde_json::Value {
        fresh_db().await;
        bridge::db_exec_raw(
            r#"CREATE TABLE "sentinel" (id TEXT PRIMARY KEY)"#,
            db_codec::empty_params(),
        )
        .expect("sentinel");
        snapshot()
    }

    fn filter() -> MetadataFilter {
        let mut f = MetadataFilter::default();
        f.equals.insert("k".into(), serde_json::json!("v"));
        f
    }

    fn assert_invalid_name<T: std::fmt::Debug>(op: &str, name: &str, got: Result<T, VectorError>) {
        assert!(
            matches!(got, Err(VectorError::InvalidIndexName(_))),
            "{op}({name:?}) must be refused as an invalid name: {got:?}"
        );
    }

    #[wasm_bindgen_test]
    async fn every_op_refuses_a_hostile_name_before_any_sql_runs() {
        for name in HOSTILE {
            let before = sentinel_db().await;
            let svc = BrowserVectorService::new();

            assert_invalid_name(
                "create_index",
                name,
                svc.create_index(VectorIndexConfig {
                    name: name.to_string(),
                    model: "m".into(),
                    dimensions: 3,
                    metric: DistanceMetric::Cosine,
                    keyword_search: true,
                })
                .await,
            );
            assert_invalid_name("delete_index", name, svc.delete_index(name).await);
            assert_invalid_name(
                "upsert",
                name,
                svc.upsert(
                    name,
                    vec![VectorEntry {
                        id: "a".into(),
                        vector: vec![1.0, 0.0, 0.0],
                        metadata: None,
                        text: Some("t".into()),
                    }],
                )
                .await,
            );
            for mode in [SearchMode::Vector, SearchMode::Keyword, SearchMode::Hybrid] {
                assert_invalid_name(
                    "query",
                    name,
                    svc.query(name, vec![1.0, 0.0, 0.0], 3, None, mode, Some("t".into()))
                        .await,
                );
            }
            assert_invalid_name("delete", name, svc.delete(name, vec!["a".into()]).await);
            assert_invalid_name("count", name, svc.count(name).await);
            assert_invalid_name("describe_index", name, svc.describe_index(name).await);
            assert_invalid_name("list_ids", name, svc.list_ids(name, filter()).await);

            let renamed = svc.rename_index(name, &name.to_ascii_lowercase()).await;
            assert!(
                matches!(renamed, Err(VectorError::InvalidRename { .. })),
                "rename_index({name:?}) must be refused: {renamed:?}"
            );

            assert_eq!(snapshot(), before, "{name:?}: a statement ran");
        }
    }

    /// The prefix `list_indexes` takes is a bound value, never spliced, so a
    /// hostile one is only a prefix nothing matches.
    #[wasm_bindgen_test]
    async fn list_indexes_binds_its_prefix() {
        sentinel_db().await;
        for prefix in HOSTILE {
            assert_eq!(
                BrowserVectorService::new()
                    .list_indexes(prefix)
                    .await
                    .expect("list"),
                Vec::<String>::new()
            );
        }
        let sentinel = db_codec::rows_from_js(
            bridge::db_query_raw(
                "SELECT name FROM sqlite_master WHERE name = 'sentinel'",
                db_codec::empty_params(),
            )
            .expect("query"),
        )
        .expect("decode");
        assert_eq!(sentinel.len(), 1);
    }
}

/// The hybrid-search fusion pin. `service.rs` is wasm32-only (it drives the
/// sql.js bridge), so these run under `wasm-pack test --node`; the fusion
/// itself is pure and needs no bridge.
#[cfg(all(test, target_arch = "wasm32"))]
mod rrf_tests {
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::vector_rrf;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    /// The inline RRF this file used to carry computed
    /// `sum over lists of 1 / (60 + rank_1based)` and sorted descending.
    /// `fuse_scored` must reproduce those scores exactly for a fixture, or the
    /// hybrid `VectorMatch::score` values every consumer reads would move.
    #[wasm_bindgen_test]
    fn fuse_scored_reproduces_the_inline_rrf_scores() {
        let vector_ranking = ids(&["a", "b", "c"]);
        let keyword_ranking = ids(&["c", "a", "d"]);

        let fused = vector_rrf::fuse_scored(
            &[vector_ranking, keyword_ranking],
            10,
            vector_rrf::DEFAULT_RRF_K,
        );

        // Hand-computed with k = 60, ranks 1-based, exactly as the inline
        // `1.0 / (RRF_K + (rank + 1) as f32)` accumulation did.
        let k = 60.0_f32;
        let expect_a = 1.0 / (k + 1.0) + 1.0 / (k + 2.0);
        let expect_c = 1.0 / (k + 3.0) + 1.0 / (k + 1.0);
        let expect_b = 1.0 / (k + 2.0);
        let expect_d = 1.0 / (k + 3.0);

        let got: Vec<(&str, f32)> = fused.iter().map(|(id, s)| (id.as_str(), *s)).collect();
        assert_eq!(got.len(), 4, "every id in either list survives: {got:?}");
        // `a` (ranks 1 and 2) narrowly outscores `c` (ranks 3 and 1); both are
        // ahead of the ids that appear in only one list.
        assert_eq!(got[0].0, "a", "{got:?}");
        assert_eq!(got[1].0, "c", "{got:?}");

        for (id, expected) in [
            ("a", expect_a),
            ("b", expect_b),
            ("c", expect_c),
            ("d", expect_d),
        ] {
            let actual = got
                .iter()
                .find(|(got_id, _)| *got_id == id)
                .map(|(_, s)| *s)
                .unwrap_or_else(|| panic!("{id} missing from {got:?}"));
            assert!(
                (actual - expected).abs() < 1e-7,
                "{id}: fuse_scored gave {actual}, inline RRF gave {expected}"
            );
        }
    }

    /// `fuse_scored` truncates to `top_k` itself — the `.take(top_k)` the
    /// inline copy needed after sorting is gone, so this is what keeps the
    /// hybrid result length honest.
    #[wasm_bindgen_test]
    fn fuse_scored_truncates_to_top_k() {
        let fused =
            vector_rrf::fuse_scored(&[ids(&["a", "b", "c", "d"])], 2, vector_rrf::DEFAULT_RRF_K);
        assert_eq!(
            fused.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
    }

    /// Two ids that fuse to the same score come back in a stable (id) order.
    /// The `HashMap` + tie-break-free sort this replaced could return either
    /// order from run to run.
    #[wasm_bindgen_test]
    fn equal_scores_break_ties_by_id() {
        let fused =
            vector_rrf::fuse_scored(&[ids(&["z"]), ids(&["a"])], 10, vector_rrf::DEFAULT_RRF_K);
        assert_eq!(
            fused.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            vec!["a", "z"]
        );
    }
}

/// The vector service does not own a second durability contract.
///
/// It writes the same sql.js database `database.rs` does, through the same
/// bridge, so the flush policy has to be one policy. Before this, four sites
/// here ran `bridge::dbFlush()` with `?`, which meant a failed mutation
/// skipped the flush entirely and left already-applied statements in memory
/// only — the opposite of what `with_flush` promises three lines away.
///
/// What the shared helper actually DOES on a failed operation is asserted in
/// `database::flush_precedence` (`a_failed_operation_still_flushes`); this
/// module only has to say that every mutating site goes through it. That is a
/// source-text property — "no flush of its own, and the shared one at every
/// mutating site" — and there is nothing else to assert it against without a
/// live OPFS.
#[cfg(all(test, target_arch = "wasm32"))]
mod one_durability_contract {
    use wasm_bindgen_test::wasm_bindgen_test;

    /// The five `VectorService` methods that mutate the database:
    /// `create_index`, `delete_index`, `rename_index`, `upsert` and `delete`.
    /// A sixth would have to come here and say which contract it uses.
    const MUTATING_SITES: usize = 5;

    /// Code lines only: a comment may name what the code may not.
    fn code_lines(src: &str) -> impl Iterator<Item = (usize, &str)> {
        src.lines()
            .enumerate()
            .map(|(n, line)| (n + 1, line))
            .filter(|(_, line)| !line.trim_start().starts_with("//"))
    }

    #[wasm_bindgen_test]
    fn this_module_calls_no_flush_of_its_own() {
        // Assembled rather than written out, so this test is not itself an
        // occurrence of what it is looking for. Matched WITHOUT a module
        // prefix, so `use crate::bridge::dbFlush;` followed by a bare call is
        // caught too — the spelling the previous needle missed.
        let needle = ["db", "Flush"].concat();
        for (n, line) in code_lines(include_str!("service.rs")) {
            assert!(
                !line.contains(&needle),
                "line {n}: flush through `database::with_flush_mapped`, which \
                 owns the crate's one durability contract, not through the \
                 bridge directly"
            );
        }
    }

    /// The other half, and the one the needle above cannot express: a file
    /// that flushes NOWHERE passes an assertion about what it must not call.
    /// Every mutating method here has to route through the shared helper.
    #[wasm_bindgen_test]
    fn every_mutating_site_routes_through_the_shared_contract() {
        // Assembled for the same reason the needle above is: this line is
        // itself a code line in the file being scanned.
        let call = ["with_flush", "_mapped("].concat();
        let calls = code_lines(include_str!("service.rs"))
            .filter(|(_, line)| line.contains(&call))
            .count();

        assert_eq!(
            calls, MUTATING_SITES,
            "expected {MUTATING_SITES} `with_flush_mapped` call sites, found {calls}: a \
             mutating method was added or removed without deciding what flushes it"
        );
    }
}
