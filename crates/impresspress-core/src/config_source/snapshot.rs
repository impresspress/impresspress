//! [`VariablesSnapshot`]: the runtime's one memoized read of the variables
//! table, shared by every reader of stored config.
//!
//! Two readers consume the table. The runtime's [`ConfigSource`] resolves each
//! block's declared keys at its first `Init`
//! ([`DatabaseConfigSource`](super::DatabaseConfigSource)), and the config block
//! answers `CONFIG_GET` and `CONFIG_GET_MANY` for the rest of the runtime's life
//! ([`VariablesConfigBlock`](crate::blocks::config::VariablesConfigBlock)). Each
//! used to keep its own memo, so a cold Cloudflare build read the whole table
//! twice — and, because the source was constructed per request there, a warm
//! request that resolved a block's config read it again. Both now read through
//! the one snapshot their runtime owns.
//!
//! ## Why the runtime owns it
//!
//! Not the config block: the source is consulted at boot, before the config
//! block is guaranteed initialized. And not the source: on Cloudflare the source
//! is built per request around that request's database handle and overlay, so a
//! memo inside it lives for one request. The runtime builds the snapshot first
//! and hands it to both ([`crate::builder::ImpresspressBuilder::variables_snapshot`]
//! for the config block, the source's constructor for the source); on
//! Cloudflare the cached runtime keeps it and passes it to every warm request's
//! source. A rebuilt runtime starts with a new, empty snapshot.
//!
//! It holds data and its memo only — no database handle. The caller passes the
//! handle it holds to [`VariablesSnapshot::read`], so a cached runtime never
//! retains one request's handle.
//!
//! ## Fail closed
//!
//! A failed read is remembered ([`State::Failed`]) and replayed to every reader
//! until it is retried, with no query. It is never turned into defaults, an
//! empty map or "not found": `load_for_block` answers `ConfigError::Transient`,
//! and `CONFIG_GET`/`CONFIG_GET_MANY` answer `ErrorCode::Unavailable`. A route
//! that reads stored config therefore answers an error while the table cannot
//! be read, rather than rendering with config nobody stored.
//!
//! ## When it re-reads
//!
//! - **A config write.** A snapshot (loaded or failed) carries the config-write
//!   generation ([`crate::config_generation`]) captured BEFORE its read, and
//!   any other generation invalidates it.
//! - **A failed read's retry time,** under [`RetryMode::Backoff`]: 1 s after the
//!   first consecutive failure, doubling to 60 s.
//! - **The pre-migration-002 shape, or no table yet,** every time: see
//!   [`VariablesSnapshot::read`].
//!
//! ## State
//!
//! On wasm32 an [`IsolateCell`](crate::isolate_cell::IsolateCell): Cloudflare
//! can hard-stop a request without running destructors, and the cell has no
//! borrow flag or guard to strand. Natively a [`std::sync::RwLock`], taken only
//! to clone an `Arc`-cheap [`State`] out or store one in, never across an
//! `.await`. A poisoned lock is read through: the value behind it is one
//! [`State`] replaced whole under the write guard, so a panic elsewhere cannot
//! have left it half-written, and treating poison as fatal would make one
//! panic a permanent state. Either way the worst an interruption can do is
//! lose the memo, which costs one read.

use std::{collections::HashMap, sync::Arc};

use wafer_core::interfaces::database::service::DatabaseService;

use crate::{
    config_generation::config_write_generation,
    platform_state::variables::{self, UnfilteredReadError, VariableRow},
};

/// The first retry of a failed read waits this long.
const FIRST_RETRY_MS: u64 = 1_000;
/// A failed read's retry delay doubles up to this.
const MAX_RETRY_MS: u64 = 60_000;

/// Every variables row, grouped by the `block` column. Rows whose `block` is
/// absent or empty are left out: shared (`WAFER_RUN_SHARED__*`) and ad hoc keys
/// belong to no block's config, and the per-block `WHERE block = ?` this
/// replaced never matched them either.
pub type BlockVariables = HashMap<String, HashMap<String, String>>;

/// One successful read of the variables table, with the two projections its
/// readers use, computed once per read.
#[derive(Debug)]
pub struct VariablesRows {
    rows: Vec<VariableRow>,
    by_block: BlockVariables,
    values: HashMap<String, String>,
}

impl VariablesRows {
    fn new(rows: Vec<VariableRow>) -> Self {
        let mut by_block: BlockVariables = HashMap::new();
        let mut values = HashMap::with_capacity(rows.len());
        for row in &rows {
            if let Some(block) = &row.block {
                by_block
                    .entry(block.clone())
                    .or_default()
                    .insert(row.key.clone(), row.value.clone());
            }
            // An empty value is "unset", not "explicitly blank", everywhere in
            // this repo (see `blocks::config`'s module docs), so it is left
            // out here and a reader falls through to what else holds the key.
            if !row.value.is_empty() {
                values.insert(row.key.clone(), row.value.clone());
            }
        }
        Self {
            rows,
            by_block,
            values,
        }
    }

    /// Every row that decoded, as stored.
    pub fn rows(&self) -> &[VariableRow] {
        &self.rows
    }

    /// The config source's projection: each block's rows, keyed by its
    /// `{ORG}__{BLOCK}` prefix. Empty values are kept — the resolution ladder
    /// treats one as no value and falls through to the declared default.
    pub fn by_block(&self) -> &BlockVariables {
        &self.by_block
    }

    /// The config block's projection: every key with a non-empty value.
    pub fn values(&self) -> &HashMap<String, String> {
        &self.values
    }
}

/// How a snapshot whose read failed is retried.
///
/// A constructor argument rather than a setting: the runtime that owns the
/// snapshot knows whether anything else retries for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryMode {
    /// The snapshot retries on its own: the first reader after the failure's
    /// retry time reads again. 1 s after the first consecutive failure,
    /// doubling to 60 s. For a runtime nothing rebuilds — native, the browser —
    /// and for Cloudflare until a retry window that rebuilds the runtime
    /// exists (D1-OUTAGE-DEGRADE-PLAN I5): without one, a failed read would
    /// otherwise wait for the next KV-probe rebuild, 5–10 minutes or never.
    Backoff,
    /// The snapshot never retries on its own; only a config write (a new
    /// generation) or a new snapshot — the runtime's rebuild — reads again.
    /// For a runtime whose owner retries by rebuilding it, so a failure costs
    /// one read per rebuild rather than one per rebuild plus one per backoff
    /// step. Cloudflare's, once the outage plan's retry window rebuilds the
    /// runtime (its step IP-B sets it).
    Rebuild,
}

/// A remembered failed read.
#[derive(Debug)]
struct Failure {
    /// The config-write generation captured before the failed read: a write
    /// since invalidates the failure exactly as it invalidates a loaded
    /// snapshot.
    generation: u64,
    /// What the read failed with, replayed to every reader until the retry.
    error: UnfilteredReadError,
    /// When [`RetryMode::Backoff`] next reads (`util::now_millis` scale).
    retry_at_ms: u64,
    /// Consecutive failed reads, this one included. A successful read ends
    /// the streak, and so does a read that finds no table (`Absent`, stored as
    /// `Empty`); a config write does not (it only makes the next read
    /// immediate), so a database that keeps failing keeps backing off.
    failures: u32,
}

/// What the snapshot holds. Cloning one is O(1): it is moved in and out of the
/// cell whole.
#[derive(Debug, Clone, Default)]
enum State {
    /// Nothing read yet, or nothing worth remembering: a fresh runtime, or the
    /// last read was the pre-migration-002 shape (see
    /// [`VariablesSnapshot::read`]), which is deliberately not memoized.
    #[default]
    Empty,
    /// A successful read, and the generation captured before it.
    Loaded {
        generation: u64,
        rows: Arc<VariablesRows>,
    },
    /// A failed read.
    Failed(Arc<Failure>),
}

#[cfg(target_arch = "wasm32")]
type StateCell = crate::isolate_cell::IsolateCell<State>;
#[cfg(not(target_arch = "wasm32"))]
type StateCell = std::sync::RwLock<State>;

/// The clock retry times are measured on.
type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The runtime's memoized read of the variables table. See the module docs.
pub struct VariablesSnapshot {
    state: StateCell,
    retry: RetryMode,
    clock: Clock,
}

impl std::fmt::Debug for VariablesSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VariablesSnapshot")
            .field("retry", &self.retry)
            .finish_non_exhaustive()
    }
}

impl Default for VariablesSnapshot {
    /// An empty snapshot that retries a failed read on its own
    /// ([`RetryMode::Backoff`]).
    fn default() -> Self {
        Self::new(RetryMode::Backoff)
    }
}

impl VariablesSnapshot {
    /// An empty snapshot, retrying a failed read as `retry` says, on the wall
    /// clock.
    pub fn new(retry: RetryMode) -> Self {
        Self {
            state: StateCell::default(),
            retry,
            clock: Arc::new(crate::util::now_millis),
        }
    }

    /// [`Self::new`] on a clock a test drives, in `util::now_millis` units.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_clock(retry: RetryMode, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        Self {
            state: StateCell::default(),
            retry,
            clock: Arc::new(clock),
        }
    }

    /// How a failed read is retried.
    pub fn retry_mode(&self) -> RetryMode {
        self.retry
    }

    #[cfg(target_arch = "wasm32")]
    fn load(&self) -> State {
        self.state.get().unwrap_or_default()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn load(&self) -> State {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[cfg(target_arch = "wasm32")]
    fn store(&self, state: State) {
        self.state.set(state);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn store(&self, state: State) {
        *self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = state;
    }

    /// The table's rows, read through `db` at most once per config-write
    /// generation; or the failed read's error, remembered until its retry.
    ///
    /// [`UnfilteredReadError::Absent`] — the table does not exist yet — is
    /// returned and not remembered, as a fresh database's answer rather than a
    /// failure.
    ///
    /// The pre-migration-002 exception: rows exist and NO record carries a
    /// `block` key at all ([`variables::UnfilteredRead::any_record_has_block_key`]).
    /// That is a table from before migration 002 added the column, and the
    /// migration that adds and backfills it runs as raw SQL through
    /// `migration_helper`, which records no config write to invalidate a memo
    /// with. Memoizing would leave every block on its defaults for the rest of
    /// the runtime, so that read is served and not remembered. Keyed on the
    /// record's shape, not on the grouping: a table holding only shared rows
    /// (`block` NULL on each) has the column, groups to nothing, and is
    /// memoized like any other — otherwise it would be re-read once per
    /// config-declaring block on every build, and on every `CONFIG_GET`.
    ///
    /// Every target applies admin 002 at provisioning before any request-path
    /// build, so the exception arises only on a version serving before its
    /// provisioning ran; it can go once every consumer has repinned past that.
    pub async fn read(
        &self,
        db: &Arc<dyn DatabaseService>,
    ) -> Result<Arc<VariablesRows>, UnfilteredReadError> {
        let generation = config_write_generation();
        let failures_before = match self.load() {
            State::Loaded {
                generation: loaded_at,
                rows,
            } if loaded_at == generation => return Ok(rows),
            State::Failed(failure) => {
                if failure.generation == generation && !self.retry_is_due(&failure) {
                    return Err(failure.error.clone());
                }
                failure.failures
            }
            State::Loaded { .. } | State::Empty => 0,
        };

        // `generation` was captured BEFORE this read: a write that lands while
        // the query is in flight must invalidate the result it raced, not be
        // swallowed by it. Two readers racing here each store the generation
        // their own read began at, so the loser leaves a state tagged older
        // than the store is — which costs the next reader a re-read and never
        // serves it rows from before a write it should see.
        match variables::read_unfiltered(db).await {
            Ok(read) => {
                let rows = Arc::new(VariablesRows::new(read.rows));
                if read.returned > 0 && !read.any_record_has_block_key {
                    tracing::warn!(
                        rows = read.returned,
                        "variables rows carry no `block` column (pre-migration-002); \
                         serving this read without remembering it"
                    );
                    self.store(State::Empty);
                } else {
                    self.store(State::Loaded {
                        generation,
                        rows: Arc::clone(&rows),
                    });
                }
                Ok(rows)
            }
            // Not a failure, and not remembered: a fresh database before
            // admin's migrations, whose table-creating migration records no
            // config write. See `UnfilteredReadError::Absent`.
            Err(error @ UnfilteredReadError::Absent(_)) => {
                self.store(State::Empty);
                Err(error)
            }
            Err(error) => {
                let failures = failures_before.saturating_add(1);
                let retry_at_ms = (self.clock)().saturating_add(retry_delay_ms(failures));
                tracing::warn!(
                    error = %error,
                    failures,
                    retry = ?self.retry,
                    "the variables table could not be read; every stored-config reader \
                     answers unavailable until it is read again"
                );
                self.store(State::Failed(Arc::new(Failure {
                    generation,
                    error: error.clone(),
                    retry_at_ms,
                    failures,
                })));
                Err(error)
            }
        }
    }

    /// Whether a remembered failure may be read again now.
    fn retry_is_due(&self, failure: &Failure) -> bool {
        match self.retry {
            RetryMode::Backoff => (self.clock)() >= failure.retry_at_ms,
            RetryMode::Rebuild => false,
        }
    }
}

/// The wait before retrying after `failures` consecutive failed reads: 1 s,
/// doubling, at most 60 s.
fn retry_delay_ms(failures: u32) -> u64 {
    let doublings = failures.saturating_sub(1).min(16);
    FIRST_RETRY_MS
        .saturating_mul(1u64 << doublings)
        .min(MAX_RETRY_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_retry_delay_starts_at_one_second_and_doubles_to_a_minute() {
        let delays: Vec<u64> = (1..=9).map(retry_delay_ms).collect();
        assert_eq!(
            delays,
            [1_000, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000, 60_000]
        );
        assert_eq!(retry_delay_ms(u32::MAX), MAX_RETRY_MS);
    }

    // The projections read only a row's `key`, `value` and `block` fields, so
    // these are labels, not config keys.
    const BLOCK_KEY: &str = "block-row";
    const BLOCK_KEY_EMPTY: &str = "block-row-empty";
    const SHARED_KEY: &str = "shared-row";
    const SHARED_KEY_EMPTY: &str = "shared-row-empty";
    const BLOCK: &str = "a-block";

    #[test]
    fn the_two_projections_split_rows_as_their_readers_need() {
        let row = |key: &str, value: &str, block: Option<&str>| VariableRow {
            id: key.to_string(),
            key: key.to_string(),
            value: value.to_string(),
            name: String::new(),
            description: String::new(),
            warning: String::new(),
            sensitive: false,
            block: block.map(str::to_string),
            updated_by: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let rows = VariablesRows::new(vec![
            row(BLOCK_KEY, "a", Some(BLOCK)),
            row(BLOCK_KEY_EMPTY, "", Some(BLOCK)),
            row(SHARED_KEY, "shared", None),
            row(SHARED_KEY_EMPTY, "", None),
        ]);

        assert_eq!(rows.rows().len(), 4);
        assert_eq!(
            rows.by_block().keys().collect::<Vec<_>>(),
            [BLOCK],
            "a shared row belongs to no block"
        );
        let auth = &rows.by_block()[BLOCK];
        assert_eq!(auth.get(BLOCK_KEY).map(String::as_str), Some("a"));
        assert_eq!(
            auth.get(BLOCK_KEY_EMPTY).map(String::as_str),
            Some(""),
            "the source's ladder decides what an empty value means"
        );
        assert_eq!(
            rows.values().get(SHARED_KEY).map(String::as_str),
            Some("shared")
        );
        assert!(!rows.values().contains_key(BLOCK_KEY_EMPTY));
        assert!(!rows.values().contains_key(SHARED_KEY_EMPTY));
    }
}
