//! Block enablement — which blocks are active.
//!
//! Uses a generic HashMap approach backed by the `block_settings` table.
//! Each block's enabled/disabled state is keyed by full block name.

use std::collections::{BTreeMap, HashMap};

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
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

    /// Return a deterministic snapshot of every explicitly stored block state.
    ///
    /// The deployment-plan format uses a [`BTreeMap`] so identical settings
    /// produce byte-identical JSON independent of `HashMap` iteration order.
    /// Missing blocks are intentionally omitted: they retain the existing
    /// "enabled with empty migration state" default when the snapshot is
    /// imported again.
    pub fn to_sorted_map(&self) -> BTreeMap<String, BlockState> {
        self.blocks
            .iter()
            .map(|(name, state)| (name.clone(), state.clone()))
            .collect()
    }

    /// Flip one block's `enabled` flag in place, leaving every other column of
    /// its [`BlockState`] alone and creating the entry when absent.
    ///
    /// The only mutator on this type, and it exists for exactly one caller:
    /// the admin block toggle, which writes the `block_settings` row and then
    /// updates the live snapshot the router reads through its
    /// `Arc<dyn FeatureConfig>`. Without it the toggle only reached the table,
    /// and on native — where nothing re-reads that table after `build()` —
    /// the router kept gating on the boot-time snapshot until the process
    /// restarted.
    ///
    /// Migration state is deliberately preserved: `migration_helper` owns
    /// those columns, and an operator disabling a block must not look like a
    /// block that has never run its migrations.
    pub fn set_block_enabled(&mut self, full_name: &str, enabled: bool) {
        self.blocks
            .entry(full_name.to_string())
            .or_default()
            .enabled = enabled;
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
    /// Block name, copied from the caller's defaults slice.
    pub block_name: String,
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
/// rows in sync with `defaults`.
///
/// Pure function — no DB access and no knowledge of the block registry. The
/// caller supplies both sides: the already-loaded `existing` map (keyed by
/// block_name → `ExistingRow`) and the `(block_name, default_enabled)` pairs
/// to seed towards, which production derives from the blocks' own `BlockInfo`
/// via [`crate::blocks::block_enabled_defaults`]. It then applies the returned
/// decisions in whatever shape its persistence layer prefers (one upsert per
/// decision, batched, etc.).
///
/// A row whose block is absent from `defaults` is left alone — no decision is
/// emitted for it. That is how a block that is not `can_disable`, or is not
/// compiled into this build, keeps whatever row it has (and, having none,
/// falls back to enabled).
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
pub fn plan_seed_decisions(
    existing: &HashMap<String, ExistingRow>,
    defaults: &[(String, bool)],
) -> Vec<SeedDecision> {
    let mut out = Vec::new();
    for (name, default) in defaults {
        let want_hash = seed_hash_for(*default);
        match existing.get(name) {
            None => out.push(SeedDecision {
                block_name: name.clone(),
                enabled: *default,
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
                    block_name: name.clone(),
                    enabled: *default,
                    hash: want_hash,
                    op: SeedOp::Update,
                });
            }
        }
    }
    out
}

/// All features enabled (for testing).
pub struct AllEnabled;

impl FeatureConfig for AllEnabled {
    fn is_block_enabled(&self, _: &str) -> bool {
        true
    }
}

#[cfg(test)]
mod block_settings_tests {
    use super::*;

    /// Creating the entry when absent is the common case: most blocks hold no
    /// `block_settings` row until someone toggles them, and the toggle has to
    /// work the first time.
    #[test]
    fn set_block_enabled_creates_an_absent_entry() {
        let mut settings = BlockSettings::default();
        assert!(
            settings.is_block_enabled("impresspress/files"),
            "a block with no entry defaults to enabled",
        );

        settings.set_block_enabled("impresspress/files", false);
        assert!(!settings.is_block_enabled("impresspress/files"));

        settings.set_block_enabled("impresspress/files", true);
        assert!(settings.is_block_enabled("impresspress/files"));
    }

    /// Migration state belongs to `migration_helper`, not to the toggle: an
    /// operator disabling a block must not leave it looking like one that has
    /// never run its migrations.
    #[test]
    fn set_block_enabled_preserves_every_other_column() {
        let mut blocks = HashMap::new();
        blocks.insert(
            "impresspress/files".to_string(),
            BlockState {
                enabled: true,
                migration: MigrationState {
                    current_hash: "cur".to_string(),
                    blessed_hash: "bless".to_string(),
                },
                seed_defaults_hash: "seed:abc".to_string(),
            },
        );
        let mut settings = BlockSettings::from_blocks(blocks);

        settings.set_block_enabled("impresspress/files", false);

        let state = settings.state("impresspress/files");
        assert!(!state.enabled);
        assert_eq!(state.migration.current_hash, "cur");
        assert_eq!(state.migration.blessed_hash, "bless");
        assert_eq!(state.seed_defaults_hash, "seed:abc");
    }

    #[test]
    fn set_block_enabled_touches_only_its_own_entry() {
        let mut settings = BlockSettings::default();
        settings.set_block_enabled("impresspress/files", false);
        assert!(
            settings.is_block_enabled("impresspress/tickets"),
            "an unrelated block keeps its default",
        );
    }
}

#[cfg(test)]
mod seed_plan_tests {
    use std::collections::HashMap;

    use super::*;

    /// The planner's inputs are a fixture, never the production block set.
    /// Driving these lanes from the real registry is what let the
    /// `legalpages`/`userportal` divergence sit unnoticed for as long as it
    /// did: the tests asserted the planner agreed with whatever the registry
    /// said, which is a tautology. Six entries, deliberately mixed
    /// `true`/`false`, so the mixed-state test below has its five lanes with
    /// one left over.
    fn fixture() -> Vec<(String, bool)> {
        [
            ("org/alpha", true),
            ("org/bravo", false),
            ("org/charlie", true),
            ("org/delta", false),
            ("org/echo", true),
            ("org/foxtrot", true),
        ]
        .into_iter()
        .map(|(name, enabled)| (name.to_string(), enabled))
        .collect()
    }

    /// Stage every fixture entry in `existing`, deriving the stored `enabled`
    /// and `seed_defaults_hash` from that entry's default.
    fn staged(
        defaults: &[(String, bool)],
        value: impl Fn(bool) -> bool,
        hash: impl Fn(bool) -> String,
    ) -> HashMap<String, ExistingRow> {
        defaults
            .iter()
            .map(|(name, default)| {
                (
                    name.clone(),
                    ExistingRow {
                        enabled: value(*default),
                        hash: hash(*default),
                    },
                )
            })
            .collect()
    }

    fn expected_default(defaults: &[(String, bool)], block_name: &str) -> bool {
        defaults
            .iter()
            .find(|(name, _)| name == block_name)
            .map(|(_, v)| *v)
            .expect("decision block name must be in the defaults slice")
    }

    #[test]
    fn plan_seed_decisions_inserts_when_row_absent() {
        let defaults = fixture();
        let existing: HashMap<String, ExistingRow> = HashMap::new();
        let decisions = plan_seed_decisions(&existing, &defaults);
        assert_eq!(decisions.len(), defaults.len());
        for d in &decisions {
            assert_eq!(d.op, SeedOp::Insert);
            let expected = expected_default(&defaults, &d.block_name);
            assert_eq!(d.enabled, expected);
            assert_eq!(d.hash, seed_hash_for(expected));
        }
    }

    #[test]
    fn plan_seed_decisions_skips_when_hash_matches_current() {
        let defaults = fixture();
        let existing = staged(&defaults, |d| d, seed_hash_for);
        let decisions = plan_seed_decisions(&existing, &defaults);
        assert!(
            decisions.is_empty(),
            "no decisions expected at steady state, got: {decisions:?}",
        );
    }

    #[test]
    fn plan_seed_decisions_updates_when_hash_stale() {
        let defaults = fixture();
        let existing = staged(&defaults, |d| !d, |d| seed_hash_for(!d));
        let decisions = plan_seed_decisions(&existing, &defaults);
        assert_eq!(decisions.len(), defaults.len());
        for d in &decisions {
            assert_eq!(d.op, SeedOp::Update);
            let expected = expected_default(&defaults, &d.block_name);
            assert_eq!(d.enabled, expected);
            assert_eq!(d.hash, seed_hash_for(expected));
        }
    }

    #[test]
    fn plan_seed_decisions_skips_user_edited() {
        let defaults = fixture();
        let existing = staged(&defaults, |d| !d, |_| USER_EDITED_SENTINEL.to_string());
        let decisions = plan_seed_decisions(&existing, &defaults);
        assert!(
            decisions.is_empty(),
            "user-edited rows must be preserved even when value drifts: {decisions:?}",
        );
    }

    #[test]
    fn plan_seed_decisions_skips_empty_hash_legacy() {
        let defaults = fixture();
        let existing = staged(&defaults, |d| !d, |_| String::new());
        let decisions = plan_seed_decisions(&existing, &defaults);
        assert!(
            decisions.is_empty(),
            "legacy empty-hash rows must be preserved: {decisions:?}",
        );
    }

    /// A row for a block the caller did not list is not the planner's
    /// business: no decision is emitted and the row is left alone. This is the
    /// lane `impresspress/system` and `wafer-run/auth` fall into once the
    /// defaults come from `can_disable` (spec 2.1.4) — an existing row of
    /// theirs is preserved as-is, and having none they fall back to enabled.
    #[test]
    fn plan_seed_decisions_ignores_rows_outside_the_defaults() {
        let defaults = fixture();
        let mut existing = staged(&defaults, |d| d, seed_hash_for);
        existing.insert(
            "org/not-listed".to_string(),
            ExistingRow {
                enabled: false,
                hash: seed_hash_for(true),
            },
        );
        let decisions = plan_seed_decisions(&existing, &defaults);
        assert!(
            decisions.is_empty(),
            "a row with no matching default must not be touched: {decisions:?}",
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
        let defaults = fixture();
        let mut existing = HashMap::new();

        // Lane A: at-current → skip.
        let (lane_a_name, lane_a_default) = defaults[0].clone();
        existing.insert(
            lane_a_name.clone(),
            ExistingRow {
                enabled: lane_a_default,
                hash: seed_hash_for(lane_a_default),
            },
        );

        // Lane B: stale seed hash → Update.
        let (lane_b_name, lane_b_default) = defaults[1].clone();
        let lane_b_old = !lane_b_default;
        existing.insert(
            lane_b_name.clone(),
            ExistingRow {
                enabled: lane_b_old,
                hash: seed_hash_for(lane_b_old),
            },
        );

        // Lane C: user-edited → skip even if value drifts.
        let (lane_c_name, lane_c_default) = defaults[2].clone();
        existing.insert(
            lane_c_name.clone(),
            ExistingRow {
                enabled: !lane_c_default,
                hash: USER_EDITED_SENTINEL.to_string(),
            },
        );

        // Lane D: legacy empty hash → skip (preserve).
        let (lane_d_name, lane_d_default) = defaults[3].clone();
        existing.insert(
            lane_d_name.clone(),
            ExistingRow {
                enabled: !lane_d_default,
                hash: String::new(),
            },
        );

        // Lanes E and beyond: absent → Insert.

        let decisions = plan_seed_decisions(&existing, &defaults);

        // Expected: 1 Update (lane B) + (defaults.len() - 4) Inserts (lanes E
        // onward). Lanes A, C, D produce no decisions.
        let expected_inserts = defaults.len() - 4;
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
        for skipped in [lane_a_name, lane_c_name, lane_d_name] {
            assert!(
                decisions.iter().all(|d| d.block_name != skipped),
                "{skipped} should not be in decisions: {decisions:?}",
            );
        }
    }
}
