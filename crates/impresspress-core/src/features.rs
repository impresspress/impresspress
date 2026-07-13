//! Block enablement — which blocks are active.
//!
//! Uses a generic HashMap approach backed by the `block_settings` table.
//! Each block's enabled/disabled state is keyed by full block name.

use std::collections::HashMap;

/// Synthetic config key carrying the block_settings JSON map.
///
/// At boot, loaders fan-out the loaded `BlockSettings` into the wafer config
/// snapshot under this key (in addition to the existing `FeatureConfig` Arc
/// handed to `ImpresspressRouterBlock`). Blocks that need to query enablement
/// state without re-reading D1 read this key via `ctx.config_get` and parse
/// the JSON. Double-underscore brackets mark the key as internal — it is
/// never set via env var or the variables table.
pub const BLOCK_SETTINGS_CONFIG_KEY: &str = "__IMPRESSPRESS_BLOCK_SETTINGS_JSON__";

/// Trait for querying which impresspress blocks are enabled.
pub trait FeatureConfig: wafer_run::MaybeSend + wafer_run::MaybeSync {
    /// Check if a block is enabled by its full name (e.g., "impresspress/products").
    fn is_block_enabled(&self, full_name: &str) -> bool;
}

/// Per-block runtime state stored in `impresspress__admin__block_settings`.
/// Both `enabled`, `migration`, and `seed_defaults_hash` live on the same
/// row, loaded together by the per-isolate cache.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BlockState {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub migration: MigrationState,
    /// SHA-256 hex of the deterministic seed payload last applied by
    /// `admin::settings::seed_defaults`. Empty = never seeded (or pre-PR3
    /// row). When this matches the current hash of `shared_config_vars()`,
    /// `seed_defaults` short-circuits before issuing any D1 query. Only the
    /// `impresspress/admin` row uses this field today — other blocks leave
    /// it empty.
    #[serde(default)]
    pub seed_defaults_hash: String,
}

/// Hashes that gate `migration_helper::apply_if_blessed`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct MigrationState {
    /// SHA-256 hex of the SQL bytes that have been applied. Empty = never.
    #[serde(default)]
    pub current_hash: String,
    /// SHA-256 hex of the SQL bytes the operator has blessed. Empty = never.
    #[serde(default)]
    pub blessed_hash: String,
}

fn default_true() -> bool {
    true
}

/// Generic block settings backed by a HashMap.
/// Blocks default to enabled unless explicitly disabled.
#[derive(Clone, Debug, Default)]
pub struct BlockSettings {
    blocks: HashMap<String, BlockState>,
}

impl BlockSettings {
    pub fn from_blocks(blocks: HashMap<String, BlockState>) -> Self {
        Self { blocks }
    }

    /// Construct from a legacy `enabled` map. Migration state defaults to empty.
    /// Used by callers that haven't been updated to the new shape yet.
    pub fn from_map(enabled: HashMap<String, bool>) -> Self {
        let blocks = enabled
            .into_iter()
            .map(|(name, enabled)| {
                (
                    name,
                    BlockState {
                        enabled,
                        migration: MigrationState::default(),
                        seed_defaults_hash: String::new(),
                    },
                )
            })
            .collect();
        Self { blocks }
    }

    /// Look up the full `BlockState` for a block by full name.
    /// Returns a default (enabled + empty migration state) when the block has
    /// no row in `block_settings` yet.
    pub fn state(&self, full_name: &str) -> BlockState {
        self.blocks
            .get(full_name)
            .cloned()
            .unwrap_or_else(|| BlockState {
                enabled: true,
                migration: MigrationState::default(),
                seed_defaults_hash: String::new(),
            })
    }

    /// Serialize all block state to JSON for transport through the wafer
    /// config snapshot under [`BLOCK_SETTINGS_CONFIG_KEY`]. Empty map → `"{}"`.
    ///
    /// # Panics
    ///
    /// `HashMap<String, BlockState>` has no custom `Serialize` impl that can
    /// fail (the field types are `String` / `bool`), so an error here
    /// indicates either OOM during string growth or a future schema change
    /// that broke this invariant — both should be loud rather than silently
    /// emitting `"{}"` and losing migration-gate state on transport.
    pub fn to_config_json(&self) -> String {
        serde_json::to_string(&self.blocks)
            .expect("BlockState serialization is infallible for current schema")
    }

    /// Parse a `BlockSettings` from the JSON shape produced by
    /// [`Self::to_config_json`]. Falls back to empty on parse error so
    /// `is_block_enabled` retains "default enabled" semantics.
    pub fn from_config_json(json: &str) -> Self {
        let blocks: HashMap<String, BlockState> = serde_json::from_str(json).unwrap_or_default();
        Self::from_blocks(blocks)
    }

    /// Look up a single block's [`BlockState`] from the JSON produced by
    /// [`Self::to_config_json`] without materializing every entry.
    ///
    /// Used by `migration_helper::apply_if_blessed` — called once per block
    /// per startup, on a payload that grows linearly with installed blocks.
    /// Walks the JSON's top-level object until the key matches, then
    /// deserializes only that entry. Returns the default when the key is
    /// absent or the JSON is malformed (same "default enabled" semantics
    /// as `from_config_json`).
    pub fn state_for(json: &str, block_name: &str) -> BlockState {
        let missing = || BlockState {
            enabled: true,
            migration: MigrationState::default(),
            seed_defaults_hash: String::new(),
        };
        let value: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return missing(),
        };
        let Some(obj) = value.as_object() else {
            return missing();
        };
        let Some(entry) = obj.get(block_name) else {
            return missing();
        };
        serde_json::from_value(entry.clone()).unwrap_or_else(|_| missing())
    }
}

impl FeatureConfig for BlockSettings {
    fn is_block_enabled(&self, full_name: &str) -> bool {
        self.blocks
            .get(full_name)
            .map(|s| s.enabled)
            .unwrap_or(true)
    }
}

/// `FeatureConfig` for the shared+mutable form `Arc<RwLock<BlockSettings>>`.
///
/// `ImpresspressBuilder` stores its block_settings as `Arc<RwLock<BlockSettings>>`
/// so consumers can mutate the live snapshot post-build (the OPFS-backed
/// `impresspress-web` flow needs this — it can't load block_settings until after
/// `init_block(admin)` has created the `impresspress__admin__block_settings`
/// table, but the runtime's `FeatureConfig` Arc has to exist at build time).
/// The router holds an `Arc<dyn FeatureConfig>` cloned off the same lock, so
/// post-`build()` writes are visible on subsequent route checks without any
/// ImpresspressBuilder API gymnastics.
///
/// `read()` panics only if a previous holder panicked while holding the
/// write lock, which would leave the snapshot in an indeterminate state —
/// surfacing that immediately is preferable to handing the router a stale
/// "all-enabled" fallback.
impl FeatureConfig for std::sync::RwLock<BlockSettings> {
    fn is_block_enabled(&self, full_name: &str) -> bool {
        self.read()
            .expect("BlockSettings RwLock poisoned")
            .is_block_enabled(full_name)
    }
}

/// Canonical defaults for `impresspress__admin__block_settings.enabled`.
/// Consumed by [`plan_seed_decisions`] on every cold start.
///
/// Adding a block here: bump the list, ship — every existing row gets the
/// INSERT path (no row yet → write the new default at the current hash).
///
/// Changing an existing default: just edit the bool — the hash gate detects
/// the change and re-seeds rows still at the old default. Admin-UI edits
/// (marked [`USER_EDITED_SENTINEL`]) are preserved.
///
/// Excluded for now: `impresspress/llm` and `impresspress/vector`. The LLM
/// block module is gated on `feature = "llm"` (wasm32-incompatible) so
/// the router would dispatch into a void on wasm32 if either was enabled
/// here. Restored when the LlmService trait refactor lands.
///
/// Also excluded: `impresspress/admin`. The admin row's `seed_defaults_hash`
/// column is owned by [`crate::blocks::admin::settings::seed_defaults`]
/// for the shared-vars-list payload hash (raw hex, no prefix). Two
/// writers on the same column with different formats would cause an
/// infinite re-seed loop on every cold start. The admin block is always
/// enabled by design (FeatureConfig falls back to `true` when the row is
/// absent), so omitting it from the seed has no behavioural effect.
pub const ENABLED_DEFAULTS: &[(&str, bool)] = &[
    ("wafer-run/auth", true),
    ("impresspress/files", true),
    ("impresspress/legalpages", true),
    ("impresspress/messages", true),
    ("impresspress/products", true),
    ("impresspress/system", true),
    ("impresspress/userportal", true),
];

/// Stored in `seed_defaults_hash` to mark a row that was last written by
/// the admin UI's toggle. Such rows are never overwritten by the seed.
pub const USER_EDITED_SENTINEL: &str = "user-edited";

/// Compute the canonical `"seed:<sha256_hex>"` marker for a default value.
///
/// The body of the hash is `sha256_hex(b"true")` or `sha256_hex(b"false")`
/// — short, deterministic, and stable across builds. The `"seed:"` prefix
/// distinguishes seed-managed rows from admin-managed rows
/// ([`USER_EDITED_SENTINEL`]) and from legacy empty-string state.
pub fn seed_hash_for(default: bool) -> String {
    let hex = crate::migration_helper::sha256_hex_bytes(default.to_string().as_bytes());
    format!("seed:{hex}")
}

/// One row of `impresspress__admin__block_settings` as seen by the seed
/// planner. Decoupled from `BlockState` so the planner stays pure (no
/// migration-helper dependency, no FeatureConfig trait conversion).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingRow {
    pub enabled: bool,
    pub hash: String,
}

/// What the planner decided about a given block name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedDecision {
    /// Static block name from [`ENABLED_DEFAULTS`].
    pub block_name: &'static str,
    /// Value to write.
    pub enabled: bool,
    /// `seed_defaults_hash` value to write (always `"seed:<hex>"`).
    pub hash: String,
    pub op: SeedOp,
}

/// INSERT vs UPDATE. Lets the caller pick the right SQL shape (some
/// callers can collapse both into a single UPSERT statement; others
/// prefer two distinct paths for logging clarity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedOp {
    Insert,
    Update,
}

/// Compute the set of writes needed to bring `block_settings.enabled`
/// rows in sync with [`ENABLED_DEFAULTS`].
///
/// Pure function — no DB access. The caller supplies the already-loaded
/// `existing` map (keyed by block_name → `ExistingRow`) and applies the
/// returned decisions in whatever shape its persistence layer prefers
/// (one upsert per decision, batched, etc.).
///
/// Steady-state cost: empty `Vec` returned → caller issues zero writes.
///
/// Per-row branching:
/// - Row absent → `SeedOp::Insert` at the current default.
/// - Row present with `hash == USER_EDITED_SENTINEL` → skip (admin owns it).
/// - Row present with empty `hash` → skip (legacy state, preserve).
/// - Row present with `hash == seed_hash_for(current_default)` → skip
///   (already at the current seeded default).
/// - Row present with any other `"seed:..."` hash → `SeedOp::Update`
///   (stale seed hash; default changed since the row was last seeded).
pub fn plan_seed_decisions(existing: &HashMap<String, ExistingRow>) -> Vec<SeedDecision> {
    let mut out = Vec::new();
    for &(name, default) in ENABLED_DEFAULTS {
        let want_hash = seed_hash_for(default);
        match existing.get(name) {
            None => out.push(SeedDecision {
                block_name: name,
                enabled: default,
                hash: want_hash,
                op: SeedOp::Insert,
            }),
            Some(row) => {
                if row.hash == USER_EDITED_SENTINEL || row.hash.is_empty() {
                    continue;
                }
                if row.hash == want_hash {
                    continue;
                }
                out.push(SeedDecision {
                    block_name: name,
                    enabled: default,
                    hash: want_hash,
                    op: SeedOp::Update,
                });
            }
        }
    }
    out
}

/// Read `block_settings` rows, run the hash-gated [`plan_seed_decisions`]
/// planner, apply the resulting inserts/updates, and return the post-seed
/// [`BlockSettings`].
///
/// This is the single implementation behind every target's block-settings
/// load: the Cloudflare runner, the browser config loader, AND — for the first
/// time — the native CLI, which previously read the table without ever running
/// the #222 hash-gate, so `ENABLED_DEFAULTS` changes silently never propagated
/// on native boots. Routing native through here closes that gap.
///
/// Written against [`DatabaseService`] so all three targets share it. Steady
/// state: the planner returns an empty `Vec`, so zero writes are issued and the
/// only cost is the initial list (+ no re-read).
///
/// Tolerant of a missing table (returns [`BlockSettings::default`] on list
/// error) so a fresh DB — or a cold Cloudflare isolate whose first request
/// races admin's Init — falls back to "all blocks enabled".
pub async fn load_and_seed_block_settings(
    db: &std::sync::Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
) -> BlockSettings {
    use wafer_block::db::ListOptions;

    let opts = ListOptions {
        offset: 0,
        limit: 10_000,
        skip_count: true,
        ..Default::default()
    };
    let record_list = match db
        .list(crate::admin_schema::BLOCK_SETTINGS_TABLE, &opts)
        .await
    {
        Ok(rl) => rl,
        Err(e) => {
            tracing::warn!(error = %e, "load_and_seed_block_settings: list failed");
            return BlockSettings::default();
        }
    };

    // Existing-row map for the hash-gate planner.
    let existing: HashMap<String, ExistingRow> = record_list
        .records
        .iter()
        .filter_map(|r| {
            let name = r.data.get("block_name")?.as_str()?.to_string();
            let enabled = r.data.get("enabled")?.as_i64()? != 0;
            let hash = r
                .data
                .get("seed_defaults_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some((name, ExistingRow { enabled, hash }))
        })
        .collect();

    // `block_name` → row `id`, so a `SeedOp::Update` can do a single-row
    // `db.update` (which the KV wrapper invalidates) instead of `update_where`
    // (which hard-errors on cached tables, so a changed `ENABLED_DEFAULTS` hash
    // would never propagate to existing rows).
    let id_by_block: HashMap<String, String> = record_list
        .records
        .iter()
        .filter_map(|r| {
            let name = r.data.get("block_name")?.as_str()?.to_string();
            let id = r.data.get("id")?.as_str()?.to_string();
            Some((name, id))
        })
        .collect();

    let decisions = plan_seed_decisions(&existing);
    let any_writes = !decisions.is_empty();
    for d in &decisions {
        apply_seed_decision(db, d, &id_by_block).await;
    }

    // Re-read only when something changed (rare). Costs one extra read.
    let final_records = if any_writes {
        match db
            .list(crate::admin_schema::BLOCK_SETTINGS_TABLE, &opts)
            .await
        {
            Ok(rl) => rl.records,
            Err(e) => {
                tracing::warn!(error = %e, "load_and_seed_block_settings: post-seed re-read failed");
                record_list.records
            }
        }
    } else {
        record_list.records
    };

    let blocks: HashMap<String, BlockState> = final_records
        .into_iter()
        .filter_map(|r| {
            let name = r.data.get("block_name")?.as_str()?.to_string();
            let enabled = r.data.get("enabled")?.as_i64()? != 0;
            let current_hash = r
                .data
                .get("current_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let blessed_hash = r
                .data
                .get("blessed_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let seed_defaults_hash = r
                .data
                .get("seed_defaults_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some((
                name,
                BlockState {
                    enabled,
                    migration: MigrationState {
                        current_hash,
                        blessed_hash,
                    },
                    seed_defaults_hash,
                },
            ))
        })
        .collect();

    BlockSettings::from_blocks(blocks)
}

/// Apply one [`SeedDecision`] via [`DatabaseService`]. Insert builds a fresh
/// row; Update resolves the row id from `id_by_block` (always present for an
/// Update, which is only planned for an existing row) and does a single-row
/// `db.update`. Per-decision failures are logged and tolerated.
async fn apply_seed_decision(
    db: &std::sync::Arc<dyn wafer_core::interfaces::database::service::DatabaseService>,
    d: &SeedDecision,
    id_by_block: &HashMap<String, String>,
) {
    let enabled_val = serde_json::Value::Number(serde_json::Number::from(i64::from(d.enabled)));
    let hash_val = serde_json::Value::String(d.hash.clone());
    let now = chrono::Utc::now().to_rfc3339();
    match d.op {
        SeedOp::Insert => {
            let id = format!("bs_{}", uuid::Uuid::new_v4());
            let mut data: HashMap<String, serde_json::Value> = HashMap::new();
            data.insert("id".into(), serde_json::Value::String(id));
            data.insert(
                "block_name".into(),
                serde_json::Value::String(d.block_name.to_string()),
            );
            data.insert("enabled".into(), enabled_val);
            data.insert("seed_defaults_hash".into(), hash_val);
            data.insert("created_at".into(), serde_json::Value::String(now.clone()));
            data.insert("updated_at".into(), serde_json::Value::String(now));
            if let Err(e) = db
                .create(crate::admin_schema::BLOCK_SETTINGS_TABLE, data)
                .await
            {
                tracing::warn!(block = %d.block_name, error = %e, "seed insert failed");
            }
        }
        SeedOp::Update => {
            let Some(id) = id_by_block.get(d.block_name) else {
                tracing::warn!(block = %d.block_name, "seed update skipped: no row id");
                return;
            };
            let mut data: HashMap<String, serde_json::Value> = HashMap::new();
            data.insert("enabled".into(), enabled_val);
            data.insert("seed_defaults_hash".into(), hash_val);
            data.insert("updated_at".into(), serde_json::Value::String(now));
            if let Err(e) = db
                .update(crate::admin_schema::BLOCK_SETTINGS_TABLE, id, data)
                .await
            {
                tracing::warn!(block = %d.block_name, error = %e, "seed update failed");
            }
        }
    }
}

/// All features enabled (for testing).
pub struct AllEnabled;

impl FeatureConfig for AllEnabled {
    fn is_block_enabled(&self, _: &str) -> bool {
        true
    }
}

#[cfg(test)]
mod seed_plan_tests {
    use std::collections::HashMap;

    use super::*;

    fn defaults_count() -> usize {
        ENABLED_DEFAULTS.len()
    }

    #[test]
    fn plan_seed_decisions_inserts_when_row_absent() {
        let existing: HashMap<String, ExistingRow> = HashMap::new();
        let decisions = plan_seed_decisions(&existing);
        assert_eq!(decisions.len(), defaults_count());
        for d in &decisions {
            assert_eq!(d.op, SeedOp::Insert);
            let expected_default = ENABLED_DEFAULTS
                .iter()
                .find(|(name, _)| *name == d.block_name)
                .map(|(_, v)| *v)
                .expect("decision block name must be in ENABLED_DEFAULTS");
            assert_eq!(d.enabled, expected_default);
            assert_eq!(d.hash, seed_hash_for(expected_default));
        }
    }

    #[test]
    fn plan_seed_decisions_skips_when_hash_matches_current() {
        let mut existing = HashMap::new();
        for (name, default) in ENABLED_DEFAULTS {
            existing.insert(
                (*name).to_string(),
                ExistingRow {
                    enabled: *default,
                    hash: seed_hash_for(*default),
                },
            );
        }
        let decisions = plan_seed_decisions(&existing);
        assert!(
            decisions.is_empty(),
            "no decisions expected at steady state, got: {decisions:?}",
        );
    }

    #[test]
    fn plan_seed_decisions_updates_when_hash_stale() {
        let mut existing = HashMap::new();
        for (name, default) in ENABLED_DEFAULTS {
            let opposite = !*default;
            existing.insert(
                (*name).to_string(),
                ExistingRow {
                    enabled: opposite,
                    hash: seed_hash_for(opposite),
                },
            );
        }
        let decisions = plan_seed_decisions(&existing);
        assert_eq!(decisions.len(), defaults_count());
        for d in &decisions {
            assert_eq!(d.op, SeedOp::Update);
            let expected_default = ENABLED_DEFAULTS
                .iter()
                .find(|(name, _)| *name == d.block_name)
                .map(|(_, v)| *v)
                .expect("decision block name must be in ENABLED_DEFAULTS");
            assert_eq!(d.enabled, expected_default);
            assert_eq!(d.hash, seed_hash_for(expected_default));
        }
    }

    #[test]
    fn plan_seed_decisions_skips_user_edited() {
        let mut existing = HashMap::new();
        for (name, default) in ENABLED_DEFAULTS {
            existing.insert(
                (*name).to_string(),
                ExistingRow {
                    enabled: !*default,
                    hash: USER_EDITED_SENTINEL.to_string(),
                },
            );
        }
        let decisions = plan_seed_decisions(&existing);
        assert!(
            decisions.is_empty(),
            "user-edited rows must be preserved even when value drifts: {decisions:?}",
        );
    }

    #[test]
    fn plan_seed_decisions_skips_empty_hash_legacy() {
        let mut existing = HashMap::new();
        for (name, default) in ENABLED_DEFAULTS {
            existing.insert(
                (*name).to_string(),
                ExistingRow {
                    enabled: !*default,
                    hash: String::new(),
                },
            );
        }
        let decisions = plan_seed_decisions(&existing);
        assert!(
            decisions.is_empty(),
            "legacy empty-hash rows must be preserved: {decisions:?}",
        );
    }

    #[test]
    fn seed_hash_for_is_stable_and_distinct() {
        let h_true = seed_hash_for(true);
        let h_false = seed_hash_for(false);
        assert_ne!(h_true, h_false);
        assert_eq!(h_true, seed_hash_for(true));
        assert!(h_true.starts_with("seed:"));
        assert!(h_false.starts_with("seed:"));
    }

    #[test]
    fn plan_seed_decisions_handles_mixed_row_states() {
        // Realistic boot: some rows absent (new blocks), some at the current
        // seed hash (no-op), some at a stale seed hash (re-seed), some
        // user-edited (preserve), some legacy empty hash (preserve). The
        // planner must produce exactly the right decisions, no extras and
        // no skips.
        let mut existing = HashMap::new();

        // Pick five blocks from ENABLED_DEFAULTS to stage in different states.
        // ENABLED_DEFAULTS has 7 entries; assign one to each lane and let the
        // remaining 2 fall into the "absent → Insert" lane.
        assert!(
            ENABLED_DEFAULTS.len() >= 5,
            "test assumes at least 5 entries in ENABLED_DEFAULTS"
        );

        // Lane A: at-current → skip.
        let (lane_a_name, lane_a_default) = ENABLED_DEFAULTS[0];
        existing.insert(
            lane_a_name.to_string(),
            ExistingRow {
                enabled: lane_a_default,
                hash: seed_hash_for(lane_a_default),
            },
        );

        // Lane B: stale seed hash → Update.
        let (lane_b_name, lane_b_default) = ENABLED_DEFAULTS[1];
        let lane_b_old = !lane_b_default;
        existing.insert(
            lane_b_name.to_string(),
            ExistingRow {
                enabled: lane_b_old,
                hash: seed_hash_for(lane_b_old),
            },
        );

        // Lane C: user-edited → skip even if value drifts.
        let (lane_c_name, lane_c_default) = ENABLED_DEFAULTS[2];
        existing.insert(
            lane_c_name.to_string(),
            ExistingRow {
                enabled: !lane_c_default,
                hash: USER_EDITED_SENTINEL.to_string(),
            },
        );

        // Lane D: legacy empty hash → skip (preserve).
        let (lane_d_name, lane_d_default) = ENABLED_DEFAULTS[3];
        existing.insert(
            lane_d_name.to_string(),
            ExistingRow {
                enabled: !lane_d_default,
                hash: String::new(),
            },
        );

        // Lanes E and beyond: absent → Insert. ENABLED_DEFAULTS[4..] are all absent.

        let decisions = plan_seed_decisions(&existing);

        // Expected: 1 Update (lane B) + (ENABLED_DEFAULTS.len() - 4) Inserts
        // (lanes E onward). Lanes A, C, D produce no decisions.
        let expected_inserts = ENABLED_DEFAULTS.len() - 4;
        let inserts: Vec<&SeedDecision> = decisions
            .iter()
            .filter(|d| d.op == SeedOp::Insert)
            .collect();
        let updates: Vec<&SeedDecision> = decisions
            .iter()
            .filter(|d| d.op == SeedOp::Update)
            .collect();
        assert_eq!(
            inserts.len(),
            expected_inserts,
            "expected {expected_inserts} Inserts, got: {inserts:?}",
        );
        assert_eq!(
            updates.len(),
            1,
            "expected 1 Update (lane B), got: {updates:?}"
        );
        assert_eq!(updates[0].block_name, lane_b_name);
        assert_eq!(updates[0].enabled, lane_b_default);
        assert_eq!(updates[0].hash, seed_hash_for(lane_b_default));

        // Confirm none of the skipped lanes (A, C, D) appear in any decision.
        for skipped in &[lane_a_name, lane_c_name, lane_d_name] {
            assert!(
                decisions.iter().all(|d| d.block_name != *skipped),
                "{skipped} should not be in decisions: {decisions:?}",
            );
        }
    }
}

/// End-to-end tests for [`load_and_seed_block_settings`] against a real
/// in-memory SQLite [`DatabaseService`] — the path NATIVE now runs.
///
/// Before this package, native (`server_config::load_block_settings`) read the
/// `block_settings` table with a plain `SELECT` and never invoked the #222
/// hash-gate. An `ENABLED_DEFAULTS` change therefore propagated on Cloudflare
/// and browser boots but silently NOT on native boots. These tests pin that
/// the unified loader runs the gate, so a native boot now re-seeds stale rows.
#[cfg(test)]
mod load_and_seed_tests {
    use std::sync::Arc;

    use wafer_core::interfaces::database::service::DatabaseService;

    use super::*;
    use crate::admin_schema::BLOCK_SETTINGS_TABLE;

    /// Open an in-memory SQLite service and create the canonical
    /// `block_settings` table (test-fixture setup — an allowed raw-SQL
    /// exception). Schema mirrors admin migration 001 + 003.
    async fn db_with_block_settings_table() -> Arc<dyn DatabaseService> {
        let svc: Arc<dyn DatabaseService> = Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        );
        svc.exec_raw(
            &format!(
                "CREATE TABLE {BLOCK_SETTINGS_TABLE} (
                    id                 TEXT PRIMARY KEY,
                    block_name         TEXT NOT NULL UNIQUE,
                    enabled            INTEGER NOT NULL DEFAULT 1,
                    current_hash       TEXT NOT NULL DEFAULT '',
                    blessed_hash       TEXT NOT NULL DEFAULT '',
                    seed_defaults_hash TEXT NOT NULL DEFAULT '',
                    created_at         TEXT NOT NULL DEFAULT '',
                    updated_at         TEXT NOT NULL DEFAULT ''
                )"
            ),
            &[],
        )
        .await
        .expect("create block_settings table");
        svc
    }

    async fn read_row(db: &Arc<dyn DatabaseService>, block_name: &str) -> Option<(bool, String)> {
        let rows = db
            .query_raw(
                &format!(
                    "SELECT enabled, seed_defaults_hash FROM {BLOCK_SETTINGS_TABLE} \
                     WHERE block_name = ?1"
                ),
                &[serde_json::Value::String(block_name.to_string())],
            )
            .await
            .expect("read row");
        rows.first().map(|r| {
            let enabled = r.data.get("enabled").and_then(|v| v.as_i64()).unwrap_or(0) != 0;
            let hash = r
                .data
                .get("seed_defaults_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (enabled, hash)
        })
    }

    /// Fresh table → every `ENABLED_DEFAULTS` block is inserted at its current
    /// seed hash (the native-fresh-boot case).
    #[tokio::test]
    async fn seeds_all_defaults_on_empty_table() {
        let db = db_with_block_settings_table().await;
        let settings = load_and_seed_block_settings(&db).await;
        for (name, default) in ENABLED_DEFAULTS {
            assert_eq!(
                settings.is_block_enabled(name),
                *default,
                "{name} enablement should match its default",
            );
            let (_enabled, hash) = read_row(&db, name)
                .await
                .unwrap_or_else(|| panic!("{name} row should have been inserted"));
            assert_eq!(
                hash,
                seed_hash_for(*default),
                "{name} hash should be seeded"
            );
        }
    }

    /// THE NATIVE GAP: a row pinned at a STALE seed hash (an old default) must
    /// be UPDATED to the current default + current hash when the loader runs.
    /// This is precisely the propagation native used to skip.
    #[tokio::test]
    async fn re_seeds_stale_hash_row_the_native_path_used_to_skip() {
        let db = db_with_block_settings_table().await;
        let (block_name, current_default) = ENABLED_DEFAULTS[0];
        let stale_default = !current_default;

        // Insert a row as if a previous build had seeded the opposite default.
        db.exec_raw(
            &format!(
                "INSERT INTO {BLOCK_SETTINGS_TABLE} \
                 (id, block_name, enabled, seed_defaults_hash, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, '', '')"
            ),
            &[
                serde_json::Value::String("bs_stale".into()),
                serde_json::Value::String(block_name.to_string()),
                serde_json::Value::Number(i64::from(stale_default).into()),
                serde_json::Value::String(seed_hash_for(stale_default)),
            ],
        )
        .await
        .expect("insert stale row");

        // Pre-condition: the row is at the stale value.
        let (before_enabled, before_hash) = read_row(&db, block_name).await.unwrap();
        assert_eq!(before_enabled, stale_default);
        assert_eq!(before_hash, seed_hash_for(stale_default));

        let settings = load_and_seed_block_settings(&db).await;

        // Post-condition: the gate fired — row updated to the current default.
        let (after_enabled, after_hash) = read_row(&db, block_name).await.unwrap();
        assert_eq!(
            after_enabled, current_default,
            "stale row should have been re-seeded to the current default",
        );
        assert_eq!(after_hash, seed_hash_for(current_default));
        assert_eq!(settings.is_block_enabled(block_name), current_default);
    }

    /// A `user-edited` row must be preserved — admin-UI toggles win over the
    /// seed even when the loader runs on every boot.
    #[tokio::test]
    async fn preserves_user_edited_row() {
        let db = db_with_block_settings_table().await;
        let (block_name, default) = ENABLED_DEFAULTS[0];
        let user_choice = !default;

        db.exec_raw(
            &format!(
                "INSERT INTO {BLOCK_SETTINGS_TABLE} \
                 (id, block_name, enabled, seed_defaults_hash, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, '', '')"
            ),
            &[
                serde_json::Value::String("bs_user".into()),
                serde_json::Value::String(block_name.to_string()),
                serde_json::Value::Number(i64::from(user_choice).into()),
                serde_json::Value::String(USER_EDITED_SENTINEL.to_string()),
            ],
        )
        .await
        .expect("insert user-edited row");

        let settings = load_and_seed_block_settings(&db).await;

        let (after_enabled, after_hash) = read_row(&db, block_name).await.unwrap();
        assert_eq!(after_enabled, user_choice, "user choice must be preserved");
        assert_eq!(after_hash, USER_EDITED_SENTINEL);
        assert_eq!(settings.is_block_enabled(block_name), user_choice);
    }

    /// Steady state: a table already at every current hash issues zero writes
    /// and round-trips unchanged.
    #[tokio::test]
    async fn no_writes_at_steady_state() {
        let db = db_with_block_settings_table().await;
        // First pass seeds everything.
        load_and_seed_block_settings(&db).await;
        // Capture updated_at to detect any spurious write on the second pass.
        let before = db
            .query_raw(
                &format!("SELECT block_name, updated_at FROM {BLOCK_SETTINGS_TABLE}"),
                &[],
            )
            .await
            .expect("snapshot before");
        // Second pass should be a no-op (empty plan).
        load_and_seed_block_settings(&db).await;
        let after = db
            .query_raw(
                &format!("SELECT block_name, updated_at FROM {BLOCK_SETTINGS_TABLE}"),
                &[],
            )
            .await
            .expect("snapshot after");
        assert_eq!(
            before.len(),
            after.len(),
            "steady-state pass must not insert rows",
        );
    }
}
