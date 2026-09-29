//! `KvCachedD1DatabaseService` — wraps a `DatabaseService` with a
//! Cloudflare KV cache for the config-state read shapes
//! `impresspress_core::cache_key::read_key` recognizes on the `variables`
//! and `block_settings` tables.
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
    AggregateSpec, CapGuard, Column, DatabaseError, DatabaseService, GuardedInsert, GuardedUpdate,
    Record, RecordList, Table, UpsertSpec, WriteOp, WriteOutcome,
};

/// KV TTL applied to every row-cache PUT (24 h).
///
/// Row-cache entries only. The config-version stamp is written WITHOUT a TTL
/// (`put`, not `put_with_ttl` — see
/// [`impresspress_core::kv::put_version_stamp_with_retry`]): a quiet day must
/// not expire the stamp and trigger a fleet-wide restamp.
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
/// for the `variables` and `block_settings` read shapes
/// [`cache_key::read_key`] recognizes.
///
/// A write's error is the inner service's, returned unchanged and before any
/// invalidation or version bump: a write that duplicates a key stays the
/// `AlreadyExists` the `DatabaseService` contract names, which
/// `impresspress_core::blocks::crud` answers as a 409.
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
    /// throttle window. A coalesce-SKIP (this call landed inside another
    /// call's throttle window, so it never even attempts a PUT) *also*
    /// mints and stashes a fresh stamp for that same delayed-retry queue —
    /// otherwise only the first write of a sub-second burst gets a stamp,
    /// and a runtime that rebuilds mid-burst could stay pinned to a stale
    /// version until an unrelated future write happens to land outside a
    /// throttle window.
    async fn bump_config_version(&self, collection: &str, op: &str) {
        if !cache_key::bumps_config_version(collection) {
            return;
        }
        // Record the local write BEFORE the `bump_on_write` gate. That gate is
        // about the KV stamp, which the deploy-init funnel suppresses on
        // purpose — and deploy-init is precisely the pass that seeds config
        // mid-boot, which is what an in-process config reader has to notice.
        //
        // Narrower than the stamp bump: only the VARIABLES table feeds the
        // config snapshot. `block_settings` is written by every block during
        // its own Init, so treating those as snapshot-invalidating would
        // discard a good snapshot once per block on a first boot. See
        // `impresspress_core::config_generation`.
        if impresspress_core::config_generation::writes_invalidate_config_snapshot(collection) {
            impresspress_core::config_generation::note_config_write();
        }
        if !self.mode.bump_on_write {
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
            //
            // But mint-and-stash a fresh stamp for the delayed-retry drain
            // regardless: in a burst of writes inside one coalesce window,
            // only the first write's PUT actually lands — writes 2..N all
            // take this branch and none of them get their own KV stamp. If
            // we didn't stash anything here, a rebuild that happens to land
            // mid-burst (after write 1's stamp, before write N's) would see
            // no further version bump until some unrelated future write
            // finally attempts (and lands) a PUT outside the window. Reuse
            // the same single-slot pending-retry queue the failed-PUT path
            // below feeds — `run()` unconditionally drains it via
            // `take_pending_version_retry` and PUTs it ~1.1s later, past the
            // throttle window, so the post-burst state still gets one fresh
            // stamp even though no coalesced write got to PUT its own.
            note_pending_version_retry(new_version_stamp());
            tracing::debug!(
                table = %collection,
                op,
                "config-version bump coalesced (< 1s since last attempt); \
                 stashed fresh stamp for delayed retry"
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
    /// A version stamp waiting for a delayed retry PUT — either because its
    /// own KV PUT failed (most likely the 1-write/sec/key throttle), or
    /// because it was minted fresh by a coalesce-SKIP that never attempted a
    /// PUT at all (see `bump_config_version`'s coalescing gate). Single
    /// slot, last-wins: only the most recent pending stamp needs to land.
    /// Drained by [`take_pending_version_retry`] — `lib.rs::run()` calls it
    /// after dispatch and retries through `ctx.wait_until`, off the response
    /// path and comfortably past the throttle window.
    ///
    /// `IsolateCell` rather than `RefCell` — a borrow flag stranded by a
    /// Cloudflare hard-stop would trap every later request in the isolate.
    /// See `impresspress_core::isolate_cell`.
    static PENDING_VERSION_RETRY: impresspress_core::IsolateCell<String> =
        const { impresspress_core::IsolateCell::new() };
}

/// Minimum spacing between config-version KV PUT *attempts* in one isolate.
/// Matches Cloudflare KV's documented one-write-per-second-per-key limit —
/// see "Fix KV version-write throttling and invalidation".
const BUMP_COALESCE_WINDOW_MS: u64 = 1_000;

fn note_pending_version_retry(stamp: String) {
    PENDING_VERSION_RETRY.with(|p| p.set(stamp));
}

/// Take (and clear) any version stamp still needing a delayed retry PUT. See
/// the `PENDING_VERSION_RETRY` thread_local's doc.
pub(crate) fn take_pending_version_retry() -> Option<String> {
    PENDING_VERSION_RETRY.with(impresspress_core::IsolateCell::take)
}

/// Mint and persist a fresh config-version stamp unconditionally.
/// Deploy-init calls this once after the funnel (per-write bumps are
/// suppressed during it — see [`CacheMode::bump_on_write`]).
pub(crate) async fn force_bump_config_version(kv: &dyn KvBackend) -> Result<String, String> {
    // The isolate handling `/_deploy/init` may also hold a pre-deploy
    // cached runtime from earlier ordinary requests; mark it dirty so this
    // isolate rebuilds on its next ordinary request instead of serving
    // stale (pre-migration/pre-reseed) state for up to the probe window.
    crate::runtime_cache::mark_dirty();
    let stamp = new_version_stamp();
    put_version_stamp_with_retry(kv, &stamp).await?;
    Ok(stamp)
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

    async fn schema_columns(&self, table: &str) -> Result<Vec<String>, DatabaseError> {
        self.inner.schema_columns(table).await
    }

    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
        self.inner.schema_drop_table(name).await
    }

    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError> {
        self.inner.schema_add_column(table, column).await
    }

    fn set_strict_schema(&self, enabled: bool) {
        // MUST forward — the trait default is a no-op, which would swallow the
        // STRICT_SCHEMA flag here and leave the wrapped D1 adapter on the
        // always-introspect path (its `schema_cache`/`strict_schema` overrides
        // never seeing the flag). The `wafer-run/database` block applies this
        // to whatever `DatabaseService` it holds — on CF that is this wrapper,
        // so the flag reaches D1 only through this delegation.
        self.inner.set_strict_schema(enabled);
    }

    /// The wrapped D1 service's budget: the cache answers some reads without
    /// a statement, but every statement it does send is D1's, counted there.
    fn statement_budget(
        &self,
    ) -> Result<wafer_core::interfaces::database::service::StatementBudget, DatabaseError> {
        self.inner.statement_budget()
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

    // `delete_where_count` and `take_where` MUST be overridden as well. Their
    // trait defaults compose the primitives above (`count` + `delete_where`;
    // `list` + `delete` per row), which on a cached table would take the row
    // set from KV — possibly stale — and delete behind the cache's back, and
    // on every table would skip the inner adapter's single atomic
    // `DELETE … RETURNING`. Same policy as the other bulk writes: refuse on a
    // cached table, forward otherwise.
    async fn delete_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        if cache_key::classify_table(collection).is_some() {
            return Err(DatabaseError::Internal(format!(
                "bulk delete_where_count not supported on cached table `{collection}` \
                 (would require KV mass-invalidation)"
            )));
        }
        self.inner.delete_where_count(collection, filters).await
    }

    async fn take_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError> {
        if cache_key::classify_table(collection).is_some() {
            return Err(DatabaseError::Internal(format!(
                "take_where not supported on cached table `{collection}` \
                 (would require KV mass-invalidation)"
            )));
        }
        self.inner.take_where(collection, filters).await
    }

    async fn upsert(
        &self,
        collection: &str,
        spec: UpsertSpec,
    ) -> Result<Option<Record>, DatabaseError> {
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

        // ONE KV read serves both modes. In serve mode its value can answer
        // the query outright. In read-through mode it is never *served* — it
        // is read solely so the repopulating PUT below can be skipped when
        // the rows did not actually move (`kv::needs_cache_write`).
        //
        // That read is not a new cost: read-through previously always PUT,
        // so the common case (config unchanged across a rebuild) trades one
        // write for one read — the same op count against the subrequest
        // budget, against the far more generous side of KV's free-tier
        // quotas. Only a genuine config change now costs both.
        let (mut cached_body, kv_read_failed) = match self.kv.get(&key).await {
            Ok(body) => (body, false),
            Err(e) => {
                tracing::warn!(
                    table = %collection,
                    key = %key,
                    error = %e,
                    "kv get failed; falling through"
                );
                (None, true)
            }
        };

        // Set when the serve path scrubbed a sensitive entry out of KV, so
        // the write decision below sees KV's true (now empty) state.
        let mut cache_scrubbed = false;

        if !self.mode.read_through {
            match cached_body.as_deref() {
                Some(body) => match serde_json::from_str::<Vec<Record>>(body) {
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
                            } else {
                                // KV no longer holds it, so the compare below
                                // must not treat it as still cached.
                                cache_scrubbed = true;
                            }
                        } else {
                            let total_count = records.len() as i64;
                            // What the executor reports for an unpaged list:
                            // the limit asked for, or the rows returned.
                            let page_size = opts.limit.map_or(total_count, i64::from);
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
                None => {
                    tracing::debug!(table = %collection, key = %key, "cache_miss");
                }
            }
        }
        if cache_scrubbed {
            cached_body = None;
        }

        // Fall through to D1 and populate cache.
        let result = self.inner.list(collection, opts).await?;

        // A failed GET is an availability/quota problem, not a miss: writing
        // back into the service that just failed converts read exhaustion
        // into a write storm against the far scarcer write allowance (the
        // 2026-08-30/31 multi-thousand write-request bursts). D1 already
        // answered this request; the next healthy read repopulates.
        if kv_read_failed {
            tracing::warn!(
                table = %collection,
                key = %key,
                "kv read failed; suppressing cache repopulation put"
            );
            return Ok(result);
        }

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
        // Only write when KV does not already say this. A read-through
        // rebuild touches every cached key, and config almost never moves
        // across one, so an unconditional PUT here is what made a single
        // deploy cost (isolates x keys) writes against a 1k/day budget.
        //
        // Skipping the PUT also stops refreshing `CACHE_TTL_SECS`, so an
        // unchanging key now genuinely expires 24h after it last changed
        // rather than being kept alive by deploy churn. That is the TTL
        // working as intended: the next build misses, reads D1, and
        // repopulates through this same path.
        if !impresspress_core::kv::needs_cache_write(cached_body.as_deref(), &payload) {
            tracing::debug!(table = %collection, key = %key, "cache_current; skipping put");
            return Ok(result);
        }

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

    /// `create` for every row: each row's cache keys are derived from the
    /// data being written, exactly as a single `create` derives them, so a
    /// cached table is served here rather than refused.
    async fn create_many(
        &self,
        collection: &str,
        rows: Vec<HashMap<String, serde_json::Value>>,
    ) -> Result<i64, DatabaseError> {
        let cached = cache_key::classify_table(collection);
        let mut invalidate: Vec<String> = cached
            .map(|t| {
                rows.iter()
                    .flat_map(|row| cache_key::invalidate_keys(t, row))
                    .collect()
            })
            .unwrap_or_default();
        invalidate.sort();
        invalidate.dedup();

        let inserted = self.inner.create_many(collection, rows).await?;

        self.invalidate_all(collection, &invalidate, "create_many")
            .await;
        self.bump_config_version(collection, "create_many").await;
        Ok(inserted)
    }

    /// Refused when any op targets a cached table, like the other bulk
    /// writes: an `Update`/`Delete` by id would need the old row's keys, and
    /// an `UpdateWhere`/`DeleteWhere`/`Upsert` the whole matched set's.
    /// Otherwise forwarded,
    /// then the config version is bumped once per written table that feeds a
    /// cached runtime.
    async fn batch(&self, ops: Vec<WriteOp>) -> Result<Vec<WriteOutcome>, DatabaseError> {
        if let Some(op) = ops
            .iter()
            .find(|op| cache_key::classify_table(op.collection()).is_some())
        {
            return Err(DatabaseError::Internal(format!(
                "batch not supported on cached table `{}` (would require KV invalidation)",
                op.collection()
            )));
        }
        let mut written: Vec<String> = ops.iter().map(|op| op.collection().to_string()).collect();
        written.sort();
        written.dedup();

        let outcomes = self.inner.batch(ops).await?;

        for collection in &written {
            self.bump_config_version(collection, "batch").await;
        }
        Ok(outcomes)
    }

    /// `create` behind a guard: an inserted row invalidates its keys and
    /// bumps the version as `create` does; a refused one wrote nothing.
    async fn insert_guarded(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
        guards: &[CapGuard],
    ) -> Result<GuardedInsert, DatabaseError> {
        let invalidate = cache_key::classify_table(collection)
            .map(|t| cache_key::invalidate_keys(t, &data))
            .unwrap_or_default();

        let outcome = self.inner.insert_guarded(collection, data, guards).await?;

        if matches!(outcome, GuardedInsert::Inserted(_)) {
            self.invalidate_all(collection, &invalidate, "insert_guarded")
                .await;
            self.bump_config_version(collection, "insert_guarded").await;
        }
        Ok(outcome)
    }

    /// A filtered write, so refused on a cached table like `update_where`.
    async fn update_guarded(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
        guards: &[CapGuard],
    ) -> Result<GuardedUpdate, DatabaseError> {
        if cache_key::classify_table(collection).is_some() {
            return Err(DatabaseError::Internal(format!(
                "update_guarded not supported on cached table `{collection}` \
                 (would require KV mass-invalidation)"
            )));
        }
        let outcome = self
            .inner
            .update_guarded(collection, filters, data, guards)
            .await?;
        if matches!(outcome, GuardedUpdate::Updated { .. }) {
            self.bump_config_version(collection, "update_guarded").await;
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use impresspress_core::platform_state::variables;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    /// What the mock KV's `get` returns, to separate a genuine miss
    /// (`Ok(None)`) from an availability failure (`Err`).
    enum KvGet {
        Missing,
        Fail,
    }

    /// In-memory [`impresspress_core::kv::KvBackend`] that scripts `get` and
    /// counts writes. Only the row-cache paths are exercised.
    struct MockKv {
        get: KvGet,
        writes: Cell<u32>,
    }

    #[async_trait::async_trait(?Send)]
    impl impresspress_core::kv::KvBackend for MockKv {
        async fn get(&self, _key: &str) -> Result<Option<String>, String> {
            match self.get {
                KvGet::Missing => Ok(None),
                KvGet::Fail => Err("simulated kv read failure".to_string()),
            }
        }

        async fn put_with_ttl(
            &self,
            _key: &str,
            _value: &str,
            _ttl_secs: u64,
        ) -> Result<(), String> {
            self.writes.set(self.writes.get() + 1);
            Ok(())
        }

        async fn put(&self, _key: &str, _value: &str) -> Result<(), String> {
            self.writes.set(self.writes.get() + 1);
            Ok(())
        }

        async fn delete(&self, _key: &str) -> Result<(), String> {
            unreachable!("the cached list path never deletes")
        }
    }

    /// [`DatabaseService`] stub whose `list` returns one fixed non-sensitive
    /// variables row and whose `create` is refused as a taken key; every other
    /// method is unreachable on the paths under test.
    struct MockDb;

    fn variables_rows() -> RecordList {
        let mut data = HashMap::new();
        data.insert("block".to_string(), serde_json::json!("GDSF__SITE"));
        data.insert("key".to_string(), serde_json::json!("GDSF__SITE__PUBLIC"));
        data.insert("value".to_string(), serde_json::json!("on"));
        RecordList {
            records: vec![Record {
                id: "1".to_string(),
                data,
            }],
            total_count: 1,
            page: 1,
            page_size: 500,
        }
    }

    #[wafer_block::wafer_async_trait]
    impl DatabaseService for MockDb {
        async fn get(&self, _collection: &str, _id: &str) -> Result<Record, DatabaseError> {
            unreachable!()
        }

        async fn list(
            &self,
            _collection: &str,
            _opts: &ListOptions,
        ) -> Result<RecordList, DatabaseError> {
            Ok(variables_rows())
        }

        async fn create(
            &self,
            collection: &str,
            _data: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            Err(DatabaseError::AlreadyExists(format!(
                "UNIQUE constraint failed: {collection}.key"
            )))
        }

        async fn update(
            &self,
            _collection: &str,
            _id: &str,
            _data: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            unreachable!()
        }

        async fn delete(&self, _collection: &str, _id: &str) -> Result<(), DatabaseError> {
            unreachable!()
        }

        /// Marker implementation: a caller that reaches the inner adapter's
        /// own `take_where` gets a row tagged with the collection.
        async fn take_where(
            &self,
            collection: &str,
            _filters: &[Filter],
        ) -> Result<Vec<Record>, DatabaseError> {
            Ok(vec![Record {
                id: format!("took:{collection}"),
                data: HashMap::new(),
            }])
        }

        /// Marker implementation, see `take_where`; the trait default would
        /// go through `count` + `delete_where`.
        async fn delete_where_count(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            Ok(7)
        }

        async fn count(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<i64, DatabaseError> {
            unreachable!()
        }

        async fn sum(
            &self,
            _collection: &str,
            _field: &str,
            _filters: &[Filter],
        ) -> Result<f64, DatabaseError> {
            unreachable!()
        }

        async fn query_raw(
            &self,
            _query: &str,
            _args: &[serde_json::Value],
        ) -> Result<Vec<Record>, DatabaseError> {
            unreachable!()
        }

        async fn exec_raw(
            &self,
            _query: &str,
            _args: &[serde_json::Value],
        ) -> Result<i64, DatabaseError> {
            unreachable!()
        }

        async fn create_many(
            &self,
            _collection: &str,
            _rows: Vec<std::collections::HashMap<String, serde_json::Value>>,
        ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
            unreachable!()
        }

        async fn update_where(
            &self,
            _collection: &str,
            _filters: &[Filter],
            _data: HashMap<String, serde_json::Value>,
        ) -> Result<(), DatabaseError> {
            unreachable!()
        }

        async fn batch(
            &self,
            _ops: Vec<wafer_core::interfaces::database::service::WriteOp>,
        ) -> Result<
            Vec<wafer_core::interfaces::database::service::WriteOutcome>,
            wafer_core::interfaces::database::service::DatabaseError,
        > {
            unreachable!()
        }

        async fn insert_guarded(
            &self,
            _collection: &str,
            _data: std::collections::HashMap<String, serde_json::Value>,
            _guards: &[wafer_core::interfaces::database::service::CapGuard],
        ) -> Result<
            wafer_core::interfaces::database::service::GuardedInsert,
            wafer_core::interfaces::database::service::DatabaseError,
        > {
            unreachable!()
        }

        async fn update_guarded(
            &self,
            _collection: &str,
            _filters: &[wafer_block::db::Filter],
            _data: std::collections::HashMap<String, serde_json::Value>,
            _guards: &[wafer_core::interfaces::database::service::CapGuard],
        ) -> Result<
            wafer_core::interfaces::database::service::GuardedUpdate,
            wafer_core::interfaces::database::service::DatabaseError,
        > {
            unreachable!()
        }

        async fn upsert(
            &self,
            _collection: &str,
            _spec: UpsertSpec,
        ) -> Result<Option<Record>, DatabaseError> {
            unreachable!()
        }

        async fn aggregate(
            &self,
            _collection: &str,
            _spec: AggregateSpec,
        ) -> Result<Vec<Record>, DatabaseError> {
            unreachable!()
        }

        async fn ensure_schema_table(&self, _table: &Table) -> Result<(), DatabaseError> {
            unreachable!()
        }

        async fn schema_table_exists(&self, _name: &str) -> Result<bool, DatabaseError> {
            unreachable!()
        }

        async fn schema_columns(&self, _table: &str) -> Result<Vec<String>, DatabaseError> {
            unreachable!()
        }

        async fn schema_drop_table(&self, _name: &str) -> Result<(), DatabaseError> {
            unreachable!()
        }

        async fn schema_add_column(
            &self,
            _table: &str,
            _column: &Column,
        ) -> Result<(), DatabaseError> {
            unreachable!()
        }

        fn statement_budget(
            &self,
        ) -> Result<wafer_core::interfaces::database::service::StatementBudget, DatabaseError>
        {
            Ok(wafer_core::interfaces::database::service::StatementBudget::Unbounded)
        }
    }

    fn cached_service(get: KvGet) -> (KvCachedD1DatabaseService, Arc<MockKv>) {
        let kv = Arc::new(MockKv {
            get,
            writes: Cell::new(0),
        });
        let svc = KvCachedD1DatabaseService::new(
            Arc::new(MockDb),
            kv.clone() as Arc<dyn impresspress_core::kv::KvBackend>,
        );
        (svc, kv)
    }

    fn variables_list_opts() -> ListOptions {
        cache_key::block_list_opts(cache_key::CachedTable::Variables, "GDSF__SITE")
    }

    /// **The `DatabaseService` contract through the cache.** A create on a
    /// cached table that the database refuses as a taken key comes back as
    /// the same `AlreadyExists` — `crud`'s 409 — and, since nothing was
    /// written, touches no cache entry and bumps no version stamp.
    #[wasm_bindgen_test]
    async fn a_refused_duplicate_create_stays_already_exists() {
        let (svc, kv) = cached_service(KvGet::Missing);
        let err = svc
            .create(variables::TABLE, HashMap::new())
            .await
            .expect_err("refused");
        assert!(matches!(err, DatabaseError::AlreadyExists(_)), "{err:?}");
        assert_eq!(kv.writes.get(), 0, "a refused write bumps nothing");
    }

    /// A `batch` that writes a cached table is refused before it reaches the
    /// database (the mock's `batch` is `unreachable!`), however many of its
    /// other ops are on uncached tables: an `Update`/`Delete` by id there
    /// would leave the old row's KV entry serving for up to its 24 h TTL.
    #[wasm_bindgen_test]
    async fn a_batch_touching_a_cached_table_is_refused_before_it_writes() {
        let (svc, _kv) = cached_service(KvGet::Missing);
        let err = svc
            .batch(vec![
                WriteOp::Create {
                    collection: "impresspress__files__objects".into(),
                    data: HashMap::new(),
                },
                WriteOp::Delete {
                    collection: variables::TABLE.into(),
                    id: "v1".into(),
                },
            ])
            .await
            .expect_err("refused");
        assert!(
            matches!(&err, DatabaseError::Internal(msg) if msg.contains(variables::TABLE)),
            "{err:?}"
        );
    }

    /// THE August 30/31 write-storm mechanism, row-cache side: a failed KV
    /// GET must fall through to D1 for availability but must NOT attempt to
    /// repopulate the same KV service that just failed — under read-quota
    /// exhaustion that converts every read into a doomed write request
    /// against the far scarcer write allowance.
    #[wasm_bindgen_test]
    async fn row_cache_get_error_falls_through_without_put() {
        let (svc, kv) = cached_service(KvGet::Fail);
        let result = svc
            .list(variables::TABLE, &variables_list_opts())
            .await
            .expect("a KV failure must not fail the query; D1 answers it");
        assert_eq!(result.records.len(), 1, "D1 rows must still be served");
        assert_eq!(
            kv.writes.get(),
            0,
            "a KV read error must cause zero KV write attempts"
        );
    }

    /// A genuine `Ok(None)` miss keeps today's behavior: fall through to D1
    /// and repopulate the cache so the next isolate can serve from KV.
    #[wasm_bindgen_test]
    async fn row_cache_missing_key_repopulates() {
        let (svc, kv) = cached_service(KvGet::Missing);
        let result = svc
            .list(variables::TABLE, &variables_list_opts())
            .await
            .expect("a cache miss must fall through to D1");
        assert_eq!(result.records.len(), 1);
        assert_eq!(
            kv.writes.get(),
            1,
            "a genuine miss must repopulate the cache with one PUT"
        );
    }

    /// Bulk ops on a cached table are refused like the other bulk writes,
    /// never served by the trait defaults: those would `list` (from KV, so
    /// possibly stale) and then `delete` row by row behind the cache's back.
    #[wasm_bindgen_test]
    async fn take_where_on_cached_table_is_refused() {
        let (svc, kv) = cached_service(KvGet::Missing);
        assert!(svc.take_where(variables::TABLE, &[]).await.is_err());
        assert_eq!(
            kv.writes.get(),
            0,
            "a refused bulk op must not touch the cache"
        );
    }

    #[wasm_bindgen_test]
    async fn delete_where_count_on_cached_table_is_refused() {
        let (svc, kv) = cached_service(KvGet::Missing);
        assert!(svc.delete_where_count(variables::TABLE, &[]).await.is_err());
        assert_eq!(kv.writes.get(), 0);
    }

    /// On an uncached table both forward to the inner adapter's own (atomic)
    /// implementation rather than the list-then-delete default.
    #[wasm_bindgen_test]
    async fn take_where_forwards_to_inner_for_uncached_table() {
        let (svc, _kv) = cached_service(KvGet::Missing);
        let rows = svc
            .take_where("conf_x", &[])
            .await
            .expect("forwarded to inner");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "took:conf_x");
    }

    #[wasm_bindgen_test]
    async fn delete_where_count_forwards_to_inner_for_uncached_table() {
        let (svc, _kv) = cached_service(KvGet::Missing);
        assert_eq!(
            svc.delete_where_count("conf_x", &[])
                .await
                .expect("forwarded to inner"),
            7
        );
    }
}
