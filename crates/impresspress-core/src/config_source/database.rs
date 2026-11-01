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
//! ONE unfiltered read of the whole table serves every block — and the config
//! block too. The read is the runtime's [`VariablesSnapshot`], which this
//! source is handed rather than owns: on Cloudflare a source is built per
//! request (around that request's database handle and overlay), while the
//! snapshot lives as long as the runtime. A block's load is a map lookup in
//! the snapshot's [`by_block`](super::VariablesRows::by_block) projection, not
//! a query.
//!
//! The `block` column (added by migration 002, which also indexed it) is NOT
//! obsolete — it is still exactly what the grouping keys on, and a row
//! without it belongs to no block's config. Only the QUERY STRATEGY changed:
//! this source used to issue one filtered `WHERE block = ?` lookup per
//! registered block (the column existed so that lookup could be an indexed
//! equality rather than a `LIKE prefix%` match on `key`), and that was one
//! KV-cached read per block on every cold hydration — 22 on the production
//! deployment, every cold hydration, and (measured 2026-09-01) all 22
//! returning zero rows. Neither filtered shape is issued here now;
//! [`variables::read_unfiltered`](crate::platform_state::variables::read_unfiltered)
//! records why the unfiltered shape is deliberately not cacheable.
//!
//! Optionally layers an in-memory overlay (on Cloudflare, `worker::Env`
//! secrets such as `WAFER_RUN__AUTH__JWT_SECRET`, and a consumer's request
//! config) on top of the rows. Overlay values win over the table: the overlay
//! represents bindings that must override whatever an admin happens to have
//! stored in the variables table.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use wafer_block::ConfigVar;
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_run::{init_failure_is_transient, ConfigError, ConfigSource, EnvBlockConfig};

use super::VariablesSnapshot;
use crate::platform_state::variables::UnfilteredReadError;

/// Reads block-declared config keys from the variables table through a
/// [`DatabaseService`], falling back to each [`ConfigVar`]'s `default` when
/// the row is missing or its `value` is empty.
///
/// Returns [`ConfigError::MissingRequired`] for keys with `optional ==
/// false` where neither the overlay, the row nor a non-empty default supplies
/// a value. A failed read of the table — fresh, or remembered by the snapshot
/// until its retry — surfaces as [`ConfigError::Transient`] when it may clear
/// on its own and [`ConfigError::SourceFault`] when it cannot (see
/// [`config_error`]), never as defaults. wafer-run does not cache a transient
/// error in the block slot: it retries the block's `Init` after its transient
/// backoff, and that retry reads through the snapshot again. A source fault is
/// cached as the block's permanent failure.
pub struct DatabaseConfigSource {
    db: Arc<dyn DatabaseService>,
    /// Static overlay applied on top of the rows in `load_for_block`. Keys
    /// present here override the table — on Cloudflare, `worker::Env` secrets
    /// (e.g. `WAFER_RUN__AUTH__JWT_SECRET`) that must not live in D1, and a
    /// consumer's request config. Empty when constructed via [`Self::new`].
    overlay: HashMap<String, String>,
    /// The runtime's read of the variables table, shared with its config block.
    snapshot: Arc<VariablesSnapshot>,
}

impl DatabaseConfigSource {
    /// A source over `db`, reading through its runtime's `snapshot`.
    pub fn new(db: Arc<dyn DatabaseService>, snapshot: Arc<VariablesSnapshot>) -> Self {
        Self::with_overlay(db, HashMap::new(), snapshot)
    }

    /// Construct with a static overlay of values that win over the table's
    /// rows. On Cloudflare: the `worker::Env` secrets admins manage via
    /// `wrangler secret put` rather than the admin dashboard, and a
    /// consumer's request config.
    pub fn with_overlay(
        db: Arc<dyn DatabaseService>,
        overlay: HashMap<String, String>,
        snapshot: Arc<VariablesSnapshot>,
    ) -> Self {
        Self {
            db,
            overlay,
            snapshot,
        }
    }
}

/// The [`ConfigError`] a block's load reports when the variables table could
/// not be read: the one place a snapshot failure becomes a config error, and
/// the one place it is classified. The snapshot remembers the failure and
/// replays it until its retry, so a remembered failure reaches this exactly as
/// a fresh one did and is classified the same way.
///
/// - [`UnfilteredReadError::Read`]: the database's own error, classified by
///   wafer-run's [`init_failure_is_transient`] over its code and detail code —
///   the rule a block `Init` that fails the same way is classified by, so the
///   config path and the lifecycle path cannot disagree about one refusal.
///   `Unavailable` (a backend down, overloaded, over a quota that resets: the
///   D1 texts `sqlite_text_error` lists) and a spent per-invocation call
///   budget (`CallBudgetExhausted`, the adapter's own refusal or the
///   platform's "Too many API requests by single Worker invocation") are
///   [`ConfigError::Transient`]: wafer-run does not cache them in the block
///   slot, and retries the block's `Init` after its transient backoff. Every
///   other variant — a missing table on a migrated database, a malformed
///   query, a D1 text nobody has classified as one that clears — is
///   [`ConfigError::SourceFault`]: an answer retrying cannot change, cached as
///   the block's permanent failure.
/// - [`UnfilteredReadError::Absent`]: `Transient`. The table does not exist
///   yet — a fresh database before admin's migrations — so a block
///   initialized before them is retried once they have run and the deploy
///   hook has seeded, rather than resolved on defaults: a required key the
///   seed provides (products' auto-generated webhook secret) would otherwise
///   be a permanent `MissingRequired`.
/// - [`UnfilteredReadError::MayBeTruncated`]: `SourceFault`. A page that came
///   back exactly full is a table too large for one bounded read; reading it
///   again returns the same page, and only a change to the read (a raised
///   limit, pagination) cures it.
///
/// The typed error stays behind either variant (`source`), so a reader of the
/// [`ConfigError`] can still downcast to the [`UnfilteredReadError`].
fn config_error(block: &str, error: UnfilteredReadError) -> ConfigError {
    let transient = match &error {
        UnfilteredReadError::Read(database) => {
            init_failure_is_transient(database.code(), database.detail_code())
        }
        UnfilteredReadError::Absent(_) => true,
        UnfilteredReadError::MayBeTruncated { .. } => false,
    };
    let block = block.to_string();
    let source = Box::new(error);
    if transient {
        ConfigError::Transient { block, source }
    } else {
        ConfigError::SourceFault { block, source }
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
        let snapshot = self
            .snapshot
            .read(&self.db)
            .await
            .map_err(|e| config_error(block, e))?;
        let rows = snapshot
            .by_block()
            .get(&crate::config_vars::screaming_block(block));
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
