//! `KvCachedD1DatabaseService` — wraps a `DatabaseService` with a
//! Cloudflare KV cache for the per-block config-var hot path.
//!
//! See `docs/superpowers/specs/2026-05-22-kv-cached-d1-config-source-design.md`.
//!
//! Host-testable logic lives in `impresspress-core` so it can be unit-tested
//! (this crate is wasm32-only and excluded from `cargo test --workspace`):
//! pure cache-key derivation in `impresspress_core::cache_key`, and the
//! [`KvBackend`] trait + [`put_version_stamp_with_retry`] in
//! `impresspress_core::kv`.

use impresspress_core::kv::{put_version_stamp_with_retry, KvBackend};

/// Production [`KvBackend`] backed by `worker::kv::KvStore`.
pub struct WorkerKvBackend(pub worker::kv::KvStore);

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl KvBackend for WorkerKvBackend {
    async fn get(&self, key: &str) -> Result<Option<String>, String> {
        self.0.get(key).text().await.map_err(|e| e.to_string())
    }

    async fn put_with_ttl(&self, key: &str, value: &str, ttl_secs: u64) -> Result<(), String> {
        self.0
            .put(key, value)
            .map_err(|e| e.to_string())?
            .expiration_ttl(ttl_secs)
            .execute()
            .await
            .map_err(|e| e.to_string())
    }

    async fn put(&self, key: &str, value: &str) -> Result<(), String> {
        self.0
            .put(key, value)
            .map_err(|e| e.to_string())?
            .execute()
            .await
            .map_err(|e| e.to_string())
    }

    async fn delete(&self, key: &str) -> Result<(), String> {
        self.0.delete(key).await.map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// KvCachedD1DatabaseService
// ---------------------------------------------------------------------------

use std::{collections::HashMap, sync::Arc};

use impresspress_core::cache_key;
use wafer_block::db::{Filter, ListOptions};
use wafer_core::interfaces::database::service::{
    AggregateSpec, Column, DatabaseError, DatabaseService, Record, RecordList, Table, UpsertSpec,
};

/// KV TTL applied to every cache PUT (24 h). Also the TTL the per-isolate
/// runtime cache stamps a freshly-minted config-version with (see
/// `runtime_cache::current_version`).
pub(crate) const CACHE_TTL_SECS: u64 = 86_400;

/// Fresh opaque config-version stamp (16 random bytes, lowercase hex).
pub fn new_version_stamp() -> String {
    let mut buf = [0u8; 16];
    // getrandom's js feature routes to crypto.getRandomValues on wasm.
    getrandom::getrandom(&mut buf).expect("getrandom");
    wafer_run::hex_encode(&buf)
}

/// Behavior switches for [`KvCachedD1DatabaseService`], chosen per
/// runtime-build context.
#[derive(Debug, Clone, Copy)]
pub struct CacheMode {
    /// Skip the KV row-cache read in `list()` (still repopulating it from
    /// the fresh D1 result). Used by version-mismatch rebuilds and the
    /// deploy-init funnel, where a stale row cache must not be baked into
    /// a runtime tagged with the new version stamp.
    pub read_through: bool,
    /// Bump the config-version stamp on writes to classified tables.
    /// The deploy-init funnel disables this (~19 sequential same-key puts
    /// under KV per-key throttling) and issues one explicit bump after.
    pub bump_on_write: bool,
}

impl Default for CacheMode {
    fn default() -> Self {
        Self {
            read_through: false,
            bump_on_write: true,
        }
    }
}

/// Wraps a [`DatabaseService`] with a write-through-invalidated KV cache
/// for the `variables` and `block_settings` per-block read paths.
pub struct KvCachedD1DatabaseService {
    inner: Arc<dyn DatabaseService>,
    kv: Arc<dyn KvBackend>,
    mode: CacheMode,
}

impl KvCachedD1DatabaseService {
    /// Wrap `inner` with a KV-backed cache using `kv` and default
    /// [`CacheMode`].
    pub fn new(inner: Arc<dyn DatabaseService>, kv: Arc<dyn KvBackend>) -> Self {
        Self::with_mode(inner, kv, CacheMode::default())
    }

    /// Wrap `inner` with explicit [`CacheMode`] switches.
    pub fn with_mode(
        inner: Arc<dyn DatabaseService>,
        kv: Arc<dyn KvBackend>,
        mode: CacheMode,
    ) -> Self {
        Self { inner, kv, mode }
    }

    /// Delete every cache key in `keys`, logging (but never failing on) KV
    /// errors. The underlying mutation has already committed by the time this
    /// runs; a failed invalidation just leaves the stale entry to expire on
    /// its 24 h TTL. `op` names the originating write for log correlation.
    async fn invalidate_all(&self, collection: &str, keys: &[String], op: &str) {
        for key in keys {
            if let Err(e) = self.kv.delete(key).await {
                tracing::warn!(
                    table = %collection,
                    key = %key,
                    error = %e,
                    op,
                    "kv invalidate failed; relying on TTL"
                );
            } else {
                tracing::debug!(table = %collection, key = %key, op, "cache_invalidate");
            }
        }
    }

    /// Rewrite the config-version stamp after a write to a table whose
    /// contents a cached runtime bakes in (variables / block_settings /
    /// wrap_grants).
    ///
    /// Coalesces to at most one KV PUT attempt per second per isolate (KV
    /// throttles a key to one write/sec) and never retries immediately: an
    /// immediate retry after a throttled write lands in the exact same
    /// one-second window and fails the same way. A failed attempt instead
    /// queues a delayed retry — see [`take_pending_version_retry`], drained
    /// by `lib.rs::run()` through `ctx.wait_until` well outside the
    /// throttle window.
    async fn bump_config_version(&self, collection: &str, op: &str) {
        if !self.mode.bump_on_write || !cache_key::bumps_config_version(collection) {
            return;
        }
        // This isolate's local D1 state just changed — the next request
        // here must not keep serving a runtime built before this write for
        // up to the runtime cache's jittered probe window, regardless of
        // whether the KV stamp below lands or is still eventually
        // consistent even for the writer. See runtime_cache's DIRTY flag.
        // Unconditional (not gated on the coalescing window below): every
        // caller of this function represents a real local write, and the
        // isolate must self-heal from it regardless of whether THIS call
        // also gets to touch KV.
        crate::runtime_cache::mark_dirty();

        let now = impresspress_core::util::now_millis();
        let last = LAST_BUMP_ATTEMPT_MS.with(std::cell::Cell::get);
        if now.saturating_sub(last) < BUMP_COALESCE_WINDOW_MS {
            // A bump was attempted within the last second — KV would
            // throttle another PUT to the same key anyway. This isolate is
            // already marked dirty above; whichever attempt lands (this
            // one, or a queued delayed retry) carries a fresh-enough stamp
            // for every OTHER isolate. Skip the redundant PUT.
            tracing::debug!(
                table = %collection,
                op,
                "config-version bump coalesced (< 1s since last attempt)"
            );
            return;
        }
        LAST_BUMP_ATTEMPT_MS.with(|c| c.set(now));

        let stamp = new_version_stamp();
        if let Err(e) = self.kv.put(cache_key::CONFIG_VERSION_KEY, &stamp).await {
            tracing::warn!(
                table = %collection,
                error = %e,
                op,
                "config-version bump failed; queuing delayed retry"
            );
            note_pending_version_retry(stamp);
        } else {
            tracing::debug!(table = %collection, version = %stamp, op, "config_version_bump");
        }
    }
}

thread_local! {
    /// Wall-clock ms (`now_millis()`) of the last config-version KV PUT
    /// *attempt* in this isolate. Coalescing gate for
    /// [`KvCachedD1DatabaseService::bump_config_version`] — see
    /// [`BUMP_COALESCE_WINDOW_MS`].
    static LAST_BUMP_ATTEMPT_MS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// A version stamp whose KV PUT failed (most likely the 1-write/sec/key
    /// throttle) and is waiting for a delayed retry. Drained by
    /// [`take_pending_version_retry`] — `lib.rs::run()` calls it after
    /// dispatch and retries through `ctx.wait_until`, off the response path
    /// and comfortably past the throttle window.
    static PENDING_VERSION_RETRY: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Minimum spacing between config-version KV PUT *attempts* in one isolate.
/// Matches Cloudflare KV's documented one-write-per-second-per-key limit —
/// see "Fix KV version-write throttling and invalidation".
const BUMP_COALESCE_WINDOW_MS: u64 = 1_000;

fn note_pending_version_retry(stamp: String) {
    PENDING_VERSION_RETRY.with(|p| *p.borrow_mut() = Some(stamp));
}

/// Take (and clear) any version stamp whose PUT failed and needs a delayed
/// retry. See the `PENDING_VERSION_RETRY` thread_local's doc.
pub(crate) fn take_pending_version_retry() -> Option<String> {
    PENDING_VERSION_RETRY.with(|p| p.borrow_mut().take())
}

/// Mint and persist a fresh config-version stamp unconditionally.
/// Deploy-init calls this once after the funnel (per-write bumps are
/// suppressed during it — see [`CacheMode::bump_on_write`]).
pub(crate) async fn force_bump_config_version(kv: &dyn KvBackend) -> Result<(), String> {
    // The isolate handling `/_deploy/init` may also hold a pre-deploy
    // cached runtime from earlier ordinary requests; mark it dirty so this
    // isolate rebuilds on its next ordinary request instead of serving
    // stale (pre-migration/pre-reseed) state for up to the probe window.
    crate::runtime_cache::mark_dirty();
    put_version_stamp_with_retry(kv, &new_version_stamp()).await
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl DatabaseService for KvCachedD1DatabaseService {
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        self.inner.get(collection, id).await
    }

    async fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError> {
        self.inner.count(collection, filters).await
    }

    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        self.inner.sum(collection, field, filters).await
    }

    async fn aggregate(
        &self,
        collection: &str,
        spec: AggregateSpec,
    ) -> Result<Vec<Record>, DatabaseError> {
        self.inner.aggregate(collection, spec).await
    }

    async fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        self.inner.query_raw(query, args).await
    }

    async fn exec_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        self.inner.exec_raw(query, args).await
    }

    async fn increment_field_where(
        &self,
        collection: &str,
        col: &str,
        delta: i64,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        // MUST override — trait default returns Err(Internal).
        self.inner
            .increment_field_where(collection, col, delta, filters)
            .await
    }

    async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
        self.inner.ensure_schema_table(table).await
    }

    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        self.inner.schema_table_exists(name).await
    }

    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
        self.inner.schema_drop_table(name).await
    }

    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError> {
        self.inner.schema_add_column(table, column).await
    }

    // Bulk-write ops on cached tables hard-error to avoid silent stale-cache footguns.
    async fn delete_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<(), DatabaseError> {
        if cache_key::classify_table(collection).is_some() {
            return Err(DatabaseError::Internal(format!(
                "bulk delete_where not supported on cached table `{collection}` \
                 (would require KV mass-invalidation)"
            )));
        }
        self.inner.delete_where(collection, filters).await
    }

    async fn update_where(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        if cache_key::classify_table(collection).is_some() {
            return Err(DatabaseError::Internal(format!(
                "bulk update_where not supported on cached table `{collection}` \
                 (would require KV mass-invalidation)"
            )));
        }
        self.inner.update_where(collection, filters, data).await
    }

    async fn update_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<i64, DatabaseError> {
        if cache_key::classify_table(collection).is_some() {
            return Err(DatabaseError::Internal(format!(
                "bulk update_where_count not supported on cached table `{collection}` \
                 (would require KV mass-invalidation)"
            )));
        }
        self.inner
            .update_where_count(collection, filters, data)
            .await
    }

    async fn upsert(&self, collection: &str, spec: UpsertSpec) -> Result<i64, DatabaseError> {
        if cache_key::classify_table(collection).is_some() {
            return Err(DatabaseError::Internal(format!(
                "upsert not supported on cached table `{collection}` \
                 (would require KV invalidation)"
            )));
        }
        self.inner.upsert(collection, spec).await
    }

    async fn list(
        &self,
        collection: &str,
        opts: &ListOptions,
    ) -> Result<RecordList, DatabaseError> {
        let table = cache_key::classify_table(collection);
        let cache_key_opt = table.and_then(|t| cache_key::read_key(t, opts));

        let (Some(table), Some(key)) = (table, cache_key_opt) else {
            return self.inner.list(collection, opts).await;
        };

        // Try cache hit (skipped in read-through mode; the fall-through
        // below still repopulates KV with the fresh D1 rows).
        if !self.mode.read_through {
            match self.kv.get(&key).await {
                Ok(Some(body)) => match serde_json::from_str::<Vec<Record>>(&body) {
                    Ok(records) => {
                        // Defense in depth: a cached payload written before
                        // this check existed (or written by a future bug)
                        // must never be *served*, sensitive-config-in-KV or
                        // not. Treat it as stale, best-effort scrub it, and
                        // fall through to D1 rather than trust it.
                        if records
                            .iter()
                            .any(|r| cache_key::row_is_sensitive(table, &r.data))
                        {
                            tracing::warn!(
                                table = %collection,
                                key = %key,
                                "cached payload contains a sensitive row; treating as stale"
                            );
                            if let Err(e) = self.kv.delete(&key).await {
                                tracing::warn!(
                                    table = %collection,
                                    key = %key,
                                    error = %e,
                                    "stale sensitive cache-entry cleanup failed; relying on TTL"
                                );
                            }
                        } else {
                            let page_size = opts.limit;
                            let total_count = records.len() as i64;
                            tracing::debug!(table = %collection, key = %key, "cache_hit");
                            return Ok(RecordList {
                                records,
                                total_count,
                                page: 1,
                                page_size,
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            table = %collection,
                            key = %key,
                            error = %e,
                            "cache value deserialize failed; falling through"
                        );
                    }
                },
                Ok(None) => {
                    tracing::debug!(table = %collection, key = %key, "cache_miss");
                }
                Err(e) => {
                    tracing::warn!(
                        table = %collection,
                        key = %key,
                        error = %e,
                        "kv get failed; falling through"
                    );
                }
            }
        }

        // Fall through to D1 and populate cache.
        let result = self.inner.list(collection, opts).await?;

        // SEC: never let a sensitive row (OAuth/Stripe/email secrets, or any
        // `_SECRET`/`_KEY`-suffixed or explicitly-flagged config var) land in
        // KV — a globally replicated, eventually consistent store — even for
        // the row-cache's 24h TTL. Skip the cache write entirely; this block
        // is simply never served from cache and always reads through to D1.
        if result
            .records
            .iter()
            .any(|r| cache_key::row_is_sensitive(table, &r.data))
        {
            tracing::debug!(
                table = %collection,
                key = %key,
                "sensitive row present; skipping KV cache write"
            );
            return Ok(result);
        }

        let payload = match serde_json::to_string(&result.records) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "cache payload serialize failed; skipping put");
                return Ok(result);
            }
        };
        if let Err(e) = self.kv.put_with_ttl(&key, &payload, CACHE_TTL_SECS).await {
            tracing::warn!(
                table = %collection,
                key = %key,
                error = %e,
                "kv put failed; cache stays cold"
            );
        }
        Ok(result)
    }

    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        let cached = cache_key::classify_table(collection);
        let invalidate = cached
            .map(|t| cache_key::invalidate_keys(t, &data))
            .unwrap_or_default();

        let record = self.inner.create(collection, data).await?;

        if invalidate.is_empty() && cached.is_some() {
            tracing::warn!(
                table = %collection,
                "cached table write with no extractable cache-key column; relying on TTL"
            );
        }
        self.invalidate_all(collection, &invalidate, "create").await;
        self.bump_config_version(collection, "create").await;

        Ok(record)
    }

    async fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        let cached = cache_key::classify_table(collection);
        // Pre-read the OLD row so we can invalidate its cache key even if
        // the update changes the identity column the key is derived from
        // (`block` for Variables, `block_name` for BlockSettings) — see
        // `cache_key::invalidate_keys_for_update`.
        let old_row = if cached.is_some() {
            match self.inner.get(collection, id).await {
                Ok(row) => Some(row.data),
                Err(e) => {
                    tracing::warn!(
                        table = %collection,
                        id = %id,
                        error = %e,
                        "pre-read for cache-key failed; only post-update keys will be invalidated"
                    );
                    None
                }
            }
        } else {
            None
        };

        let record = self.inner.update(collection, id, data).await?;

        if let Some(t) = cached {
            // Invalidate the UNION of the old-row-derived keys and the
            // new (post-update) row-derived keys, not just one side — an
            // identity-field change otherwise leaves the other key stale
            // for up to the 24h cache TTL.
            let invalidate = match &old_row {
                Some(old) => cache_key::invalidate_keys_for_update(t, old, &record.data),
                None => cache_key::invalidate_keys(t, &record.data),
            };
            self.invalidate_all(collection, &invalidate, "update").await;
        }
        self.bump_config_version(collection, "update").await;

        Ok(record)
    }

    async fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError> {
        let cached = cache_key::classify_table(collection);
        let invalidate = if let Some(t) = cached {
            match self.inner.get(collection, id).await {
                Ok(row) => cache_key::invalidate_keys(t, &row.data),
                Err(e) => {
                    tracing::warn!(
                        table = %collection,
                        id = %id,
                        error = %e,
                        "pre-read for cache-key failed; skipping invalidation"
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        self.inner.delete(collection, id).await?;

        self.invalidate_all(collection, &invalidate, "delete").await;
        self.bump_config_version(collection, "delete").await;

        Ok(())
    }
}
