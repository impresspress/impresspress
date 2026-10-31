//! [`DatabaseConfigSource`]: block config resolved from the variables table
//! through any [`DatabaseService`].
//!
//! Cloudflare's config source. It used to live in `impresspress-cloudflare`
//! as `D1ConfigSource`, which only compiled for wasm32 and held its memo in
//! `Rc`/`Cell`, so nothing native could construct it — not a test, not the
//! D1 cost harness that has to boot through the source production uses. The
//! database half lives here now and runs over SQLite natively exactly as it
//! runs over D1; the Cloudflare crate keeps only what is Cloudflare's: the
//! overlay of `worker::Env` secrets and request config it builds, and the
//! `ScopedConfigSource` proxy that reaches this per request.
//!
//! ONE unfiltered read of the whole table serves every block. The `snapshot`
//! method lists the table with no filter
//! ([`variables::read_unfiltered`]), groups the rows by the `block` column IN
//! MEMORY, and memoizes that grouping against the config-write generation it
//! was read at; a block's load is then a map lookup, not a query. One grouping
//! is deliberately NOT memoized — rows read but every one of them dropped, the
//! pre-migration-002 shape — for the reason the `snapshot` method records.
//!
//! The `block` column (added by migration 002, which also indexed it) is NOT
//! obsolete — it is still exactly what the grouping keys on, and a row
//! without it belongs to no block's config. Only the QUERY STRATEGY changed:
//! this source used to issue one filtered `WHERE block = ?` lookup per
//! registered block (the column existed so that lookup could be an indexed
//! equality rather than a `LIKE prefix%` match on `key`), and that was one
//! KV-cached read per block on every cold hydration. Neither filtered shape
//! is issued here now. The `snapshot` FIELD records the measurement behind
//! that trade; [`variables::read_unfiltered`] records why the unfiltered shape
//! is deliberately not cacheable.
//!
//! Optionally layers an in-memory overlay (on Cloudflare, `worker::Env`
//! secrets such as `WAFER_RUN__AUTH__JWT_SECRET`, and a consumer's request
//! config) on top of the rows. Overlay values win over the table: the overlay
//! represents bindings that must override whatever an admin happens to have
//! stored in the variables table.

use std::{
    collections::HashMap,
    sync::{Arc, PoisonError, RwLock},
};

use async_trait::async_trait;
use wafer_block::ConfigVar;
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_run::{ConfigError, ConfigSource, EnvBlockConfig};

use crate::{
    config_generation::config_write_generation,
    platform_state::variables::{self, UnfilteredReadError},
};

/// Every variables row, grouped by the `block` column. Rows whose `block`
/// is absent or empty are dropped: the per-block `WHERE block = ?` this
/// replaced never matched them either, and inventing a home for them here
/// would silently change which config is live.
type BlockVariables = HashMap<String, HashMap<String, String>>;

/// Reads block-declared config keys from the variables table through a
/// [`DatabaseService`], falling back to each [`ConfigVar`]'s `default` when
/// the row is missing or its `value` is empty.
///
/// Returns [`ConfigError::MissingRequired`] for keys with `optional ==
/// false` where neither the overlay, the row nor a non-empty default supplies
/// a value. A failed read of the table surfaces as [`ConfigError::Transient`]
/// (see [`config_error`]) — never as defaults: the runtime does not cache a
/// transient error in the block slot, so the next request retries.
pub struct DatabaseConfigSource {
    db: Arc<dyn DatabaseService>,
    /// Static overlay applied on top of the rows in `load_for_block`. Keys
    /// present here override the table — on Cloudflare, `worker::Env` secrets
    /// (e.g. `WAFER_RUN__AUTH__JWT_SECRET`) that must not live in D1, and a
    /// consumer's request config. Empty when constructed via [`Self::new`].
    overlay: HashMap<String, String>,
    /// The whole variables table, fetched at most once per config-write
    /// generation.
    ///
    /// `wafer-run` calls `load_for_block` once per registered block while
    /// `builder::boot` initializes them, and the previous per-block query made that
    /// one KV-cached read EACH — 22 on the production deployment, every cold
    /// hydration, and (measured 2026-09-01) all 22 returning zero rows,
    /// because only one block has block-scoped rows at all and its own
    /// sensitive values keep it out of the cache. One unfiltered query
    /// answers every block instead.
    ///
    /// Held alongside the config-write generation it was read at: a build
    /// SEEDS config partway through the same pass that reads it (admin's
    /// `Init` seeds, then the Cloudflare hook auto-generates secrets), so a
    /// snapshot cached for this source's whole lifetime would hide every
    /// row seeded after it was filled — see [`crate::config_generation`]. A
    /// write bumps the generation and the next read re-fetches; a build that
    /// seeds nothing (the common case on an established database) never
    /// re-fetches.
    ///
    /// A `std::sync::RwLock` never held across an `.await`, as
    /// `blocks::config::VariablesConfigBlock` holds its own: the guard is
    /// taken to clone the `Arc` out and dropped before the read, and re-taken
    /// only to store the result. A request Cloudflare hard-stops mid-read
    /// therefore holds no guard to strand. A poisoned lock is read through
    /// rather than treated as fatal: the value behind it is one `Option`
    /// replaced whole under the write guard, so a panic elsewhere cannot have
    /// left it half-written.
    snapshot: RwLock<Option<(u64, Arc<BlockVariables>)>>,
}

impl DatabaseConfigSource {
    pub fn new(db: Arc<dyn DatabaseService>) -> Self {
        Self::with_overlay(db, HashMap::new())
    }

    /// Construct with a static overlay of values that win over the table's
    /// rows. On Cloudflare: the `worker::Env` secrets admins manage via
    /// `wrangler secret put` rather than the admin dashboard, and a
    /// consumer's request config.
    pub fn with_overlay(db: Arc<dyn DatabaseService>, overlay: HashMap<String, String>) -> Self {
        Self {
            db,
            overlay,
            snapshot: RwLock::new(None),
        }
    }

    /// The memoized snapshot, if it was read at the current config-write
    /// generation.
    fn cached_snapshot(&self) -> Option<Arc<BlockVariables>> {
        let cached = self
            .snapshot
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        cached.and_then(|(generation, snapshot)| {
            (generation == config_write_generation()).then_some(snapshot)
        })
    }

    /// Every variables row, grouped by block — fetched at most once per
    /// config-write generation.
    ///
    /// Pre-migration-002 databases need no special query. The old per-block
    /// query named the `block` column and so failed outright with `no such
    /// column: block`; an unfiltered read does not name it, and rows lacking
    /// the column are simply skipped by the grouping below — leaving every
    /// block on its `ConfigVar` defaults, which is exactly what that special
    /// case returned.
    async fn snapshot(&self) -> Result<Arc<BlockVariables>, UnfilteredReadError> {
        if let Some(snapshot) = self.cached_snapshot() {
            return Ok(snapshot);
        }
        // Captured BEFORE the read: a write that lands while this query is in
        // flight must invalidate the result it raced, not be swallowed by it.
        let generation = config_write_generation();
        let read = variables::read_unfiltered(&self.db).await?;

        let mut grouped: BlockVariables = HashMap::new();
        for row in read.rows {
            // Shared (`WAFER_RUN_SHARED__*`) and ad hoc keys have no block
            // and belong to no block's snapshot.
            let Some(block) = row.block else {
                continue;
            };
            grouped.entry(block).or_default().insert(row.key, row.value);
        }

        let snapshot = Arc::new(grouped);
        // Do not memoize a snapshot that learned nothing from rows that
        // exist. That is the pre-migration-002 shape — rows present, no
        // `block` column, so every one is dropped above — and the migration
        // that fixes it runs as raw SQL through `migration_helper`, which
        // never reaches a write path and so records no config write to
        // invalidate this with. The per-block query this replaced tolerated
        // that by re-reading on every call; caching the emptiness instead
        // would leave every block on defaults for the rest of the boot.
        //
        // A genuinely empty TABLE is not this case and is safely cached:
        // seeding it goes through `create`, which does record a write.
        if snapshot.is_empty() && read.returned > 0 {
            tracing::warn!(
                rows = read.returned,
                "variables rows carry no usable `block` column (pre-migration-002?); \
                 re-reading rather than caching an empty snapshot"
            );
            return Ok(snapshot);
        }
        *self
            .snapshot
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some((generation, Arc::clone(&snapshot)));
        Ok(snapshot)
    }
}

/// The [`ConfigError`] a block's load reports when the variables table could
/// not be read.
///
/// The one place a snapshot failure becomes a config error, so the day the
/// classification grows (a database fault told apart from a transient one) it
/// changes here and nowhere else. Every failure is `Transient` today: the
/// runtime does not cache it in the block slot, so the block is retried rather
/// than initialized on defaults it was never configured with.
fn config_error(block: &str, error: UnfilteredReadError) -> ConfigError {
    ConfigError::Transient {
        block: block.to_string(),
        source: Box::new(error),
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl ConfigSource for DatabaseConfigSource {
    async fn load_for_block(
        &self,
        block: &str,
        declared_keys: &[ConfigVar],
    ) -> Result<EnvBlockConfig, ConfigError> {
        // `wafer-run` calls this for EVERY registered block, including the
        // many that declare no config at all. Resolving nothing against
        // anything is still nothing, so return before touching the database:
        // otherwise the first config-less block to initialize pays for the
        // snapshot on behalf of blocks that never needed it.
        if declared_keys.is_empty() {
            return Ok(EnvBlockConfig::new(HashMap::new()));
        }
        let snapshot = self.snapshot().await.map_err(|e| config_error(block, e))?;
        let rows = snapshot.get(&crate::config_vars::screaming_block(block));
        // Overlay first, then the block's own row; an empty value in either
        // is no value, so it falls through to the declared default. The rest
        // of the ladder — default, then `MissingRequired` for a required key,
        // then omission — is wafer-run's, shared with every other source.
        wafer_run::resolve_declared(block, declared_keys, |key| {
            self.overlay
                .get(key)
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    rows.and_then(|rows| rows.get(key))
                        .filter(|value| !value.is_empty())
                })
                .cloned()
        })
    }
}
