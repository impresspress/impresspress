//! D1ConfigSource — Cloudflare target's [`ConfigSource`] impl.
//!
//! Reads block-declared env-var config keys from the admin block's
//! `impresspress__admin__variables` D1 table.
//!
//! ONE unfiltered read of the whole table serves every block. The `snapshot`
//! method lists the table with no filter, groups the rows by the `block`
//! column IN MEMORY, and memoizes that grouping against the config-write
//! generation it was read at; `fetch_block_variables` is then a map lookup,
//! not a query. One grouping is deliberately NOT memoized — rows read but
//! every one of them dropped, the pre-migration-002 shape — for the reason
//! the `snapshot` method records.
//!
//! The `block` column (added by migration 002, which also indexed it) is NOT
//! obsolete — it is still exactly what the grouping keys on, and a row
//! without it belongs to no block's config. Only the QUERY STRATEGY changed:
//! this source used to issue one filtered `WHERE block = ?` lookup per
//! registered block (the column existed so that lookup could be an indexed
//! equality rather than a `LIKE prefix%` match on `key`), and that was one
//! KV-cached read per block on every cold hydration. Neither filtered shape
//! is issued here now. The `snapshot` FIELD records the measurement
//! behind that trade; the `snapshot` METHOD records why the unfiltered shape
//! is deliberately not cacheable.
//!
//! Optionally layers an in-memory overlay (e.g. `worker::Env` secrets such
//! as `WAFER_RUN__AUTH__JWT_SECRET`) on top of the D1 rows. Overlay values
//! win over D1 — overlay represents CF env bindings that must override
//! whatever an admin happens to have stored in the variables table.

use std::{cell::Cell, collections::HashMap, rc::Rc, sync::Arc};

use async_trait::async_trait;
use impresspress_core::{cache_key, platform_state::variables};
use wafer_block::ConfigVar;
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_run::{ConfigError, ConfigSource, EnvBlockConfig};

/// True when a snapshot read came back exactly full, so rows may have been
/// dropped.
///
/// One unfiltered read shares a single row budget across the whole variables
/// table, where the per-block query it replaced gave every block its own. A
/// full page is therefore the one shape that cannot be distinguished from a
/// truncated one — and `skip_count` means there is no `total_count` to check
/// it against. Silent truncation would show up as some blocks mysteriously
/// falling back to their defaults, so it is worth a log line even though the
/// production table holds a few dozen rows against a 10,000 budget.
fn snapshot_may_be_truncated(returned: usize, limit: Option<u32>) -> bool {
    limit.is_some_and(|limit| returned >= limit as usize)
}

/// Every variables row, grouped by the `block` column. Rows whose `block`
/// is absent or empty are dropped: the per-block `WHERE block = ?` this
/// replaced never matched them either, and inventing a home for them here
/// would silently change which config is live.
type BlockVariables = HashMap<String, HashMap<String, String>>;

/// Reads block-declared config keys from a D1-backed
/// [`DatabaseService`], falling back to each [`ConfigVar`]'s `default`
/// when the row is missing or its `value` is empty.
///
/// Returns [`ConfigError::MissingRequired`] for keys with `optional ==
/// false` where neither the D1 row nor a non-empty default supplies a
/// value. D1 query failures surface as [`ConfigError::Transient`] —
/// callers may retry on the next request because the runtime does
/// not cache transient errors in the block slot.
pub struct D1ConfigSource {
    db: Arc<dyn DatabaseService>,
    /// Static overlay applied on top of D1 rows in `load_for_block`.
    /// Keys present here override D1 — used for `worker::Env` secrets
    /// (e.g. `WAFER_RUN__AUTH__JWT_SECRET`) that must not live in D1.
    /// Empty when constructed via [`Self::new`].
    overlay: HashMap<String, String>,
    /// The whole variables table, fetched at most once per source.
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
    /// row seeded after it was filled — see
    /// [`impresspress_core::config_generation`]. A write bumps the
    /// generation and the next read re-fetches; a build that seeds nothing
    /// (the common case on an established database) never re-fetches.
    ///
    /// `Cell` + `Rc` rather than `RefCell`: this crate treats a borrow flag
    /// stranded by a Cloudflare hard-stop as a wedged-isolate hazard (see
    /// `runtime_cache`'s thread_local comment). `take`/`set` has no flag to
    /// strand, and no borrow is ever held across the fetch `await`.
    snapshot: Cell<Option<(u64, Rc<BlockVariables>)>>,
}

impl D1ConfigSource {
    pub fn new(db: Arc<dyn DatabaseService>) -> Self {
        Self {
            db,
            overlay: HashMap::new(),
            snapshot: Cell::new(None),
        }
    }

    /// Construct with a static overlay of values that win over D1 rows.
    /// Intended for `worker::Env` secrets such as
    /// `WAFER_RUN__AUTH__JWT_SECRET` that admins manage via
    /// `wrangler secret put` rather than the admin dashboard.
    pub fn with_overlay(db: Arc<dyn DatabaseService>, overlay: HashMap<String, String>) -> Self {
        Self {
            db,
            overlay,
            snapshot: Cell::new(None),
        }
    }

    /// Map a kebab-case block name like `"wafer-run/auth"` to the
    /// SCREAMING_SNAKE prefix stored in the `block` column.
    ///
    /// Thin re-export of [`impresspress_core::config_vars::screaming_block`], the
    /// single source of truth for the `variables.block` column shape. Kept as
    /// an inherent method so existing call sites read naturally; the body just
    /// delegates so the CF copy can't drift from the seeder / migration-002
    /// backfill.
    pub(crate) fn screaming_block(name: &str) -> String {
        impresspress_core::config_vars::screaming_block(name)
    }

    /// The memoized snapshot, if this source has already fetched it.
    ///
    /// `take` + `set` rather than a borrow, so there is no flag a
    /// hard-stopped request can strand.
    fn cached_snapshot(&self) -> Option<Rc<BlockVariables>> {
        let current = self.snapshot.take();
        self.snapshot.set(current.clone());
        current.and_then(|(generation, snapshot)| {
            (generation == impresspress_core::config_generation::config_write_generation())
                .then_some(snapshot)
        })
    }

    /// Every variables row, grouped by block — fetched at most once.
    ///
    /// ONE unfiltered query rather than one filtered query per block. The
    /// unfiltered shape is deliberately not a cacheable one
    /// (`cache_key::read_key` returns `None` for it, pinned by
    /// `the_config_source_snapshot_shape_is_deliberately_uncacheable`), which
    /// is what makes this safe: the KV row cache has no invalidation story
    /// for a whole-table key, and an unfiltered read returns sensitive rows
    /// that must never reach KV. So this trades N KV reads for one D1 query
    /// and writes nothing to KV.
    ///
    /// Pre-migration-002 D1s no longer need a special case. The old
    /// per-block query named the `block` column and so failed outright with
    /// `no such column: block`; an unfiltered read does not name it, and
    /// rows lacking the column are simply skipped by the grouping below —
    /// leaving every block on its `ConfigVar` defaults, which is exactly what
    /// that special case returned.
    async fn snapshot(
        &self,
    ) -> Result<Rc<BlockVariables>, Box<dyn std::error::Error + Send + Sync>> {
        if let Some(snapshot) = self.cached_snapshot() {
            return Ok(snapshot);
        }
        // Captured BEFORE the read: a write that lands while this query is in
        // flight must invalidate the result it raced, not be swallowed by it.
        let generation = impresspress_core::config_generation::config_write_generation();
        let rows = self
            .db
            .list(variables::TABLE, &cache_key::full_table_list_opts())
            .await
            .map_err(Box::new)?;

        // Refuse to build config from a page that may be truncated. One
        // unfiltered read shares a single row budget across the whole table
        // where the per-block query it replaced gave each block its own, and
        // `skip_count` leaves no `total_count` to check against — so an
        // exactly-full page cannot be told apart from a truncated one.
        // Continuing would resolve some blocks against config that exists but
        // was not returned: silently on defaults, or hard-failing on a
        // required key that is actually present. `build_runtime` makes the
        // same call when block settings cannot be read ("Propagate rather than
        // fabricate ... no requests are served with a fabricated all-enabled
        // snapshot"), and this is the same situation.
        let limit = cache_key::full_table_list_opts().limit;
        if snapshot_may_be_truncated(rows.records.len(), limit) {
            return Err(format!(
                "variables snapshot returned {} rows, at or above the {}-row query limit: \
                 the table may be truncated, and resolving block config from a partial \
                 snapshot would silently leave blocks on their defaults. Raise the limit or \
                 paginate this read.",
                rows.records.len(),
                limit.unwrap_or_default(),
            )
            .into());
        }

        let returned_rows = rows.records.len();
        let mut grouped: BlockVariables = HashMap::new();
        for record in rows.records {
            // Decoded through the row type that owns these column names, so
            // this adapter cannot drift from `platform_state::variables`.
            // A row the codec rejects (no `key`) is dropped rather than
            // failing the snapshot, as it was before: one malformed row must
            // not leave every block on its defaults.
            let Ok(row) = variables::VariableRow::from_record(&record.id, &record.data) else {
                continue;
            };
            // Shared (`WAFER_RUN_SHARED__*`) and ad hoc keys have no block
            // and belong to no block's snapshot.
            let Some(block) = row.block else {
                continue;
            };
            grouped.entry(block).or_default().insert(row.key, row.value);
        }

        let snapshot = Rc::new(grouped);
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
        if snapshot.is_empty() && returned_rows > 0 {
            tracing::warn!(
                rows = returned_rows,
                "variables rows carry no usable `block` column (pre-migration-002?);                  re-reading rather than caching an empty snapshot"
            );
            return Ok(snapshot);
        }
        self.snapshot.set(Some((generation, snapshot.clone())));
        Ok(snapshot)
    }

    /// This block's rows from the snapshot, empty when it has none.
    pub(crate) async fn fetch_block_variables(
        &self,
        screaming_block: &str,
    ) -> Result<HashMap<String, String>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self
            .snapshot()
            .await?
            .get(screaming_block)
            .cloned()
            .unwrap_or_default())
    }

    /// Core resolution logic — applied per [`ConfigVar`] against rows
    /// already fetched from D1, with an optional overlay layered on top.
    /// Overlay entries win over D1 rows; both must contain non-empty
    /// values to be considered (an empty string falls through to the
    /// `ConfigVar::default` fallback).
    pub(crate) fn resolve(
        block: &str,
        rows: &HashMap<String, String>,
        overlay: &HashMap<String, String>,
        declared_keys: &[ConfigVar],
    ) -> Result<EnvBlockConfig, ConfigError> {
        let mut out = HashMap::with_capacity(declared_keys.len());
        for var in declared_keys {
            // Overlay wins (CF env secrets), then D1, then default.
            let from_overlay = overlay.get(&var.key).filter(|s| !s.is_empty()).cloned();
            let from_db = rows.get(&var.key).filter(|s| !s.is_empty()).cloned();
            let resolved = from_overlay.or(from_db).or_else(|| {
                if var.default.is_empty() {
                    None
                } else {
                    Some(var.default.clone())
                }
            });

            match resolved {
                Some(v) => {
                    out.insert(var.key.clone(), v);
                }
                None if !var.optional => {
                    return Err(ConfigError::MissingRequired {
                        block: block.to_string(),
                        key: var.key.clone(),
                    });
                }
                None => {
                    // optional + no value + no default: skip; the
                    // EnvBlockConfig::get() call returns None at the block.
                }
            }
        }
        Ok(EnvBlockConfig::new(out))
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl ConfigSource for D1ConfigSource {
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
        let screaming = Self::screaming_block(block);
        let rows =
            self.fetch_block_variables(&screaming)
                .await
                .map_err(|e| ConfigError::Transient {
                    block: block.to_string(),
                    source: e,
                })?;
        Self::resolve(block, &rows, &self.overlay, declared_keys)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use wafer_block::db::{Filter, ListOptions};
    use wafer_core::interfaces::database::service::{
        AggregateSpec, Column, DatabaseError, Record, RecordList, Table, UpsertSpec,
    };
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    /// Counts `list` calls and records the filters each one carried, so a test
    /// can assert BOTH how many queries a hydration costs and that they are
    /// the unfiltered snapshot shape rather than per-block reads.
    struct CountingDb {
        lists: Cell<usize>,
        filtered_lists: Cell<usize>,
        rows: Vec<(&'static str, &'static str, &'static str)>,
        /// How many of `rows` a `list` currently returns, so a test can make
        /// seeding actually happen partway through a boot.
        rows_visible: Cell<usize>,
        /// Report this many rows regardless of `rows`, so a test can simulate
        /// a full (and therefore possibly truncated) page without building
        /// ten thousand fixtures.
        force_returned_len: Cell<Option<usize>>,
    }

    impl CountingDb {
        fn new(rows: Vec<(&'static str, &'static str, &'static str)>) -> Arc<Self> {
            let visible = rows.len();
            Arc::new(Self {
                lists: Cell::new(0),
                filtered_lists: Cell::new(0),
                rows,
                rows_visible: Cell::new(visible),
                force_returned_len: Cell::new(None),
            })
        }
    }

    #[wafer_block::wafer_async_trait]
    impl DatabaseService for CountingDb {
        async fn list(
            &self,
            _collection: &str,
            opts: &ListOptions,
        ) -> Result<RecordList, DatabaseError> {
            self.lists.set(self.lists.get() + 1);
            if !opts.filters.is_empty() {
                self.filtered_lists.set(self.filtered_lists.get() + 1);
            }
            let records: Vec<Record> = self
                .rows
                .iter()
                .take(self.rows_visible.get())
                .enumerate()
                .map(|(i, (block, key, value))| {
                    let mut data = HashMap::new();
                    if !block.is_empty() {
                        data.insert("block".to_string(), serde_json::json!(block));
                    }
                    data.insert("key".to_string(), serde_json::json!(key));
                    data.insert("value".to_string(), serde_json::json!(value));
                    Record {
                        id: i.to_string(),
                        data,
                    }
                })
                .collect();
            let records = match self.force_returned_len.get() {
                Some(n) => {
                    let mut padded = records;
                    while padded.len() < n {
                        padded.push(Record {
                            id: format!("pad-{}", padded.len()),
                            data: HashMap::new(),
                        });
                    }
                    padded
                }
                None => records,
            };
            let total = records.len() as i64;
            Ok(RecordList {
                records,
                total_count: total,
                page: 1,
                page_size: 10_000,
            })
        }

        async fn get(&self, _c: &str, _id: &str) -> Result<Record, DatabaseError> {
            unreachable!()
        }
        async fn create(
            &self,
            _c: &str,
            _d: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            unreachable!()
        }
        async fn update(
            &self,
            _c: &str,
            _id: &str,
            _d: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            unreachable!()
        }
        async fn delete(&self, _c: &str, _id: &str) -> Result<(), DatabaseError> {
            unreachable!()
        }
        async fn count(&self, _c: &str, _f: &[Filter]) -> Result<i64, DatabaseError> {
            unreachable!()
        }
        async fn sum(&self, _c: &str, _f: &str, _x: &[Filter]) -> Result<f64, DatabaseError> {
            unreachable!()
        }
        async fn query_raw(
            &self,
            _q: &str,
            _a: &[serde_json::Value],
        ) -> Result<Vec<Record>, DatabaseError> {
            unreachable!()
        }
        async fn exec_raw(&self, _q: &str, _a: &[serde_json::Value]) -> Result<i64, DatabaseError> {
            unreachable!()
        }
        async fn create_many(
            &self,
            _collection: &str,
            _rows: Vec<std::collections::HashMap<String, serde_json::Value>>,
        ) -> Result<i64, wafer_core::interfaces::database::service::DatabaseError> {
            unreachable!()
        }

        async fn take_where(
            &self,
            _collection: &str,
            _filters: &[Filter],
        ) -> Result<Vec<Record>, DatabaseError> {
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

        async fn upsert(&self, _c: &str, _s: UpsertSpec) -> Result<Option<Record>, DatabaseError> {
            unreachable!()
        }
        async fn aggregate(
            &self,
            _c: &str,
            _s: AggregateSpec,
        ) -> Result<Vec<Record>, DatabaseError> {
            unreachable!()
        }
        async fn ensure_schema_table(&self, _t: &Table) -> Result<(), DatabaseError> {
            unreachable!()
        }
        async fn schema_table_exists(&self, _n: &str) -> Result<bool, DatabaseError> {
            unreachable!()
        }
        async fn schema_columns(&self, _table: &str) -> Result<Vec<String>, DatabaseError> {
            unreachable!()
        }
        async fn schema_drop_table(&self, _n: &str) -> Result<(), DatabaseError> {
            unreachable!()
        }
        async fn schema_add_column(&self, _t: &str, _c: &Column) -> Result<(), DatabaseError> {
            unreachable!()
        }

        fn statement_budget(
            &self,
        ) -> Result<wafer_core::interfaces::database::service::StatementBudget, DatabaseError>
        {
            Ok(wafer_core::interfaces::database::service::StatementBudget::Unbounded)
        }
    }

    fn var(key: &str) -> ConfigVar {
        ConfigVar::new(key, "doc", "").optional()
    }

    /// THE read-amplification fix: initializing every block must cost ONE
    /// query, not one per block. Production ran 22 configured blocks, each
    /// paying its own KV-cached per-block lookup on every cold hydration.
    #[wasm_bindgen_test]
    async fn every_block_is_served_by_one_unfiltered_query() {
        let db = CountingDb::new(vec![
            ("WAFER_RUN__AUTH", "WAFER_RUN__AUTH__A", "auth-value"),
            (
                "IMPRESSPRESS__EMAIL",
                "IMPRESSPRESS__EMAIL__B",
                "email-value",
            ),
        ]);
        let src = D1ConfigSource::new(db.clone() as Arc<dyn DatabaseService>);

        let auth = src
            .load_for_block("wafer-run/auth", &[var("WAFER_RUN__AUTH__A")])
            .await
            .unwrap();
        assert_eq!(auth.get("WAFER_RUN__AUTH__A"), Some("auth-value"));

        let email = src
            .load_for_block("impresspress/email", &[var("IMPRESSPRESS__EMAIL__B")])
            .await
            .unwrap();
        assert_eq!(email.get("IMPRESSPRESS__EMAIL__B"), Some("email-value"));

        assert_eq!(
            db.lists.get(),
            1,
            "the snapshot must be fetched once and reused for every block"
        );
        assert_eq!(
            db.filtered_lists.get(),
            0,
            "a per-block filter would be a cacheable shape and reintroduce one KV read per block"
        );
    }

    /// Rows must stay scoped to their own block. The snapshot holds every
    /// row at once, so a grouping bug would silently let one block read
    /// another's value — which the old per-block WHERE clause made impossible.
    #[wasm_bindgen_test]
    async fn a_block_never_sees_another_blocks_row() {
        let db = CountingDb::new(vec![
            ("WAFER_RUN__AUTH", "SHARED_NAME", "auth-value"),
            ("IMPRESSPRESS__EMAIL", "SHARED_NAME", "email-value"),
            // A NULL-block row (migration-002 backfill gap in production).
            // The per-block WHERE never matched these; preserve that exactly
            // rather than quietly changing which config is live.
            ("", "SHARED_NAME", "unscoped-value"),
        ]);
        let src = D1ConfigSource::new(db.clone() as Arc<dyn DatabaseService>);

        let auth = src
            .load_for_block("wafer-run/auth", &[var("SHARED_NAME")])
            .await
            .unwrap();
        assert_eq!(auth.get("SHARED_NAME"), Some("auth-value"));

        let email = src
            .load_for_block("impresspress/email", &[var("SHARED_NAME")])
            .await
            .unwrap();
        assert_eq!(email.get("SHARED_NAME"), Some("email-value"));

        let other = src
            .load_for_block("wafer-run/cors", &[var("SHARED_NAME")])
            .await
            .unwrap();
        assert_eq!(
            other.get("SHARED_NAME"),
            None,
            "a block with no rows of its own must not inherit a NULL-block row"
        );
    }

    /// THE boot-ordering hazard, reproduced.
    ///
    /// A runtime build reads config and writes it in the same pass: admin
    /// initializes first, its `Init` runs `settings::seed_defaults`, and the
    /// Cloudflare hook then runs `seed_auto_generated` — all after the
    /// database service block (which declares a config key of its own, so the
    /// no-keys short-circuit does not skip it) has already been lazily
    /// initialized by admin's own seeding query, filling the snapshot.
    ///
    /// Caching for the source's lifetime would make every row seeded after
    /// that point invisible to every block initialized later. For a required
    /// key with no default that is a permanent `InitError` cached for the
    /// block slot's lifetime, not merely a stale read.
    #[wasm_bindgen_test]
    async fn config_seeded_mid_boot_is_visible_to_blocks_initialized_after_it() {
        let db = CountingDb::new(vec![("WAFER_RUN__AUTH", "SEEDED", "seeded-value")]);
        let src = D1ConfigSource::new(db.clone() as Arc<dyn DatabaseService>);

        // An early block resolves its own config, filling the snapshot from a
        // table that does not yet contain the seed.
        db.rows_visible.set(0);
        let early = src
            .load_for_block("wafer-run/database", &[var("SEEDED")])
            .await
            .unwrap();
        assert_eq!(early.get("SEEDED"), None, "nothing seeded yet");

        // Seeding happens: rows land, and the writer records it.
        db.rows_visible.set(1);
        impresspress_core::config_generation::note_config_write();

        let late = src
            .load_for_block("wafer-run/auth", &[var("SEEDED")])
            .await
            .unwrap();
        assert_eq!(
            late.get("SEEDED"),
            Some("seeded-value"),
            "a block initialized after seeding must see the seeded row"
        );
        assert_eq!(
            db.lists.get(),
            2,
            "exactly one re-read: the write invalidated the snapshot once"
        );
    }

    /// Truncation must FAIL, not just log.
    ///
    /// The old per-block query gave each block its own 10k budget; one
    /// unfiltered read shares a single budget across the whole table. If it
    /// ever fills, rows are dropped arbitrarily — and because `skip_count` is
    /// set there is no `total_count` to notice it by. Continuing would build a
    /// runtime from config known to be incomplete: some blocks silently on
    /// defaults, others hard-failing on a required key that does exist.
    ///
    /// `build_runtime` already sets the precedent for exactly this situation
    /// when block settings cannot be read — "Propagate rather than fabricate
    /// 'every block enabled': the runtime build fails, so no requests are
    /// served with a fabricated all-enabled snapshot." Same reasoning, same
    /// answer.
    #[wasm_bindgen_test]
    fn a_full_page_is_detected_as_possible_truncation() {
        let limit = impresspress_core::cache_key::full_table_list_opts().limit;
        let rows = limit.expect("the full-table read is bounded") as usize;
        assert!(snapshot_may_be_truncated(rows, limit));
        assert!(!snapshot_may_be_truncated(rows - 1, limit));
        assert!(!snapshot_may_be_truncated(0, limit));
        assert!(
            !snapshot_may_be_truncated(rows, None),
            "an unbounded read is complete"
        );
    }

    #[wasm_bindgen_test]
    async fn a_truncated_snapshot_fails_the_load_instead_of_serving_partial_config() {
        let limit = impresspress_core::cache_key::full_table_list_opts()
            .limit
            .expect("the full-table read is bounded") as usize;
        let db = CountingDb::new(vec![("WAFER_RUN__AUTH", "K", "v")]);
        // Claim a full page came back, which is indistinguishable from a
        // truncated one.
        db.force_returned_len.set(Some(limit));
        let src = D1ConfigSource::new(db.clone() as Arc<dyn DatabaseService>);

        let err = src
            .load_for_block("wafer-run/auth", &[var("K")])
            .await
            .expect_err("incomplete config must not resolve");
        assert!(
            matches!(err, ConfigError::Transient { .. }),
            "must be Transient so the block slot does not cache it \
             permanently: {err:?}"
        );
    }

    /// A pre-migration-002 table must not memoize its own emptiness.
    ///
    /// Before that migration the variables table has no `block` column, so
    /// every row is dropped by the grouping and the snapshot resolves to
    /// nothing. The migration then ALTERs and backfills through
    /// `migration_helper` -> `exec_raw`, which is raw SQL: it never reaches
    /// the wrapper's write paths, so it records no config write and cannot
    /// invalidate anything. Seeding does not save it either — `seed_defaults`
    /// is hash-gated and `insert_if_absent` finds the rows already present,
    /// so neither issues a `create`.
    ///
    /// The old per-block query tolerated this by returning empty PER CALL, so
    /// blocks initialized after the migration re-queried and saw the
    /// backfilled rows. Memoizing makes that emptiness permanent for the
    /// whole boot, which is a silent fallback to defaults for every block —
    /// or `MissingRequired` cached in the slot for a key that does exist.
    #[wasm_bindgen_test]
    async fn rows_without_a_block_column_are_not_memoized_as_empty() {
        let db = CountingDb::new(vec![
            // Pre-migration shape: a row with no `block` column at all.
            ("", "PRE_MIGRATION", "value"),
            // What the backfill adds later.
            ("WAFER_RUN__AUTH", "WAFER_RUN__AUTH__A", "backfilled"),
        ]);
        db.rows_visible.set(1);
        let src = D1ConfigSource::new(db.clone() as Arc<dyn DatabaseService>);

        let first = src
            .load_for_block("wafer-run/auth", &[var("WAFER_RUN__AUTH__A")])
            .await
            .unwrap();
        assert_eq!(first.get("WAFER_RUN__AUTH__A"), None);
        assert_eq!(db.lists.get(), 1);

        // The migration lands out of band, via raw SQL that records nothing.
        db.rows_visible.set(2);

        let second = src
            .load_for_block("wafer-run/auth", &[var("WAFER_RUN__AUTH__A")])
            .await
            .unwrap();
        assert_eq!(
            second.get("WAFER_RUN__AUTH__A"),
            Some("backfilled"),
            "a snapshot that grouped to nothing must be re-read, not trusted"
        );
        assert_eq!(db.lists.get(), 2);
    }

    /// The re-read is driven by writes, not by every call. An established
    /// database seeds nothing, so the whole pass must still cost one query.
    #[wasm_bindgen_test]
    async fn no_config_write_means_no_refetch() {
        let db = CountingDb::new(vec![("WAFER_RUN__AUTH", "K", "v")]);
        let src = D1ConfigSource::new(db.clone() as Arc<dyn DatabaseService>);
        for _ in 0..5 {
            src.load_for_block("wafer-run/auth", &[var("K")])
                .await
                .unwrap();
        }
        assert_eq!(db.lists.get(), 1);
    }

    /// A block declaring no config keys must not touch the database at all.
    /// `wafer-run` calls `load_for_block` for EVERY block regardless of
    /// whether it declares any keys, so without this the first config-less
    /// block still triggered the fetch.
    #[wasm_bindgen_test]
    async fn a_block_with_no_declared_keys_issues_no_query() {
        let db = CountingDb::new(vec![("WAFER_RUN__AUTH", "K", "v")]);
        let src = D1ConfigSource::new(db.clone() as Arc<dyn DatabaseService>);

        let cfg = src.load_for_block("wafer-run/cors", &[]).await.unwrap();
        assert_eq!(cfg.get("K"), None);
        assert_eq!(
            db.lists.get(),
            0,
            "a block declaring no keys has nothing to resolve, so the snapshot must not be fetched on its behalf"
        );
    }

    #[wasm_bindgen_test]
    fn screaming_block_handles_two_segments() {
        assert_eq!(
            D1ConfigSource::screaming_block("wafer-run/auth"),
            "WAFER_RUN__AUTH"
        );
        assert_eq!(
            D1ConfigSource::screaming_block("wafer-run/sqlite"),
            "WAFER_RUN__SQLITE"
        );
    }

    #[wasm_bindgen_test]
    fn screaming_block_handles_org_only() {
        assert_eq!(
            D1ConfigSource::screaming_block("impresspress"),
            "IMPRESSPRESS"
        );
    }

    #[wasm_bindgen_test]
    fn resolve_returns_db_value_when_present() {
        let mut rows = HashMap::new();
        rows.insert("KEY".to_string(), "from-db".to_string());
        let overlay = HashMap::new();
        let declared = vec![ConfigVar::new("KEY", "doc", "default")];
        let cfg = D1ConfigSource::resolve("test/block", &rows, &overlay, &declared).unwrap();
        assert_eq!(cfg.get("KEY"), Some("from-db"));
    }

    #[wasm_bindgen_test]
    fn resolve_falls_back_to_default_when_db_missing() {
        let rows = HashMap::new();
        let overlay = HashMap::new();
        let declared = vec![ConfigVar::new("KEY", "doc", "fallback")];
        let cfg = D1ConfigSource::resolve("test/block", &rows, &overlay, &declared).unwrap();
        assert_eq!(cfg.get("KEY"), Some("fallback"));
    }

    #[wasm_bindgen_test]
    fn resolve_falls_back_to_default_when_db_value_empty() {
        let mut rows = HashMap::new();
        rows.insert("KEY".to_string(), "".to_string());
        let overlay = HashMap::new();
        let declared = vec![ConfigVar::new("KEY", "doc", "fallback")];
        let cfg = D1ConfigSource::resolve("test/block", &rows, &overlay, &declared).unwrap();
        assert_eq!(cfg.get("KEY"), Some("fallback"));
    }

    #[wasm_bindgen_test]
    fn resolve_required_missing_returns_error() {
        let rows = HashMap::new();
        let overlay = HashMap::new();
        let declared = vec![ConfigVar::new("KEY", "doc", "")];
        let result = D1ConfigSource::resolve("test/block", &rows, &overlay, &declared);
        assert!(matches!(result, Err(ConfigError::MissingRequired { .. })));
    }

    #[wasm_bindgen_test]
    fn resolve_optional_missing_is_skipped() {
        let rows = HashMap::new();
        let overlay = HashMap::new();
        let declared = vec![ConfigVar::new("KEY", "doc", "").optional()];
        let cfg = D1ConfigSource::resolve("test/block", &rows, &overlay, &declared).unwrap();
        assert_eq!(cfg.get("KEY"), None);
    }

    #[wasm_bindgen_test]
    fn resolve_overlay_wins_over_db() {
        let mut rows = HashMap::new();
        rows.insert("KEY".to_string(), "from-db".to_string());
        let mut overlay = HashMap::new();
        overlay.insert("KEY".to_string(), "from-overlay".to_string());
        let declared = vec![ConfigVar::new("KEY", "doc", "default")];
        let cfg = D1ConfigSource::resolve("test/block", &rows, &overlay, &declared).unwrap();
        assert_eq!(cfg.get("KEY"), Some("from-overlay"));
    }

    #[wasm_bindgen_test]
    fn resolve_overlay_supplies_required_value_when_db_empty() {
        let rows = HashMap::new();
        let mut overlay = HashMap::new();
        overlay.insert("KEY".to_string(), "secret".to_string());
        let declared = vec![ConfigVar::new("KEY", "doc", "")];
        let cfg = D1ConfigSource::resolve("test/block", &rows, &overlay, &declared).unwrap();
        assert_eq!(cfg.get("KEY"), Some("secret"));
    }
}
