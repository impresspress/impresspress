//! `impresspress__admin__block_settings`: one row per block — its `enabled`
//! flag, the migration hashes that gate `migration_helper::apply_if_blessed`,
//! and the `seed_defaults_hash` that gates both the boot-time enablement
//! seed ([`crate::features::plan_seed_decisions`]) and admin's shared-variable
//! seed.
//!
//! Two callers, one codec (spec 2.1.2). The boot flavour ([`load`],
//! [`load_and_seed`]) runs over [`DatabaseService`] before WRAP and feeds the
//! router's [`BlockSettings`] snapshot; the runtime flavour runs under WRAP
//! over [`Context`]: every block's `Init` stamps its migration state through
//! [`upsert_fields`], admin's pages toggle and list through [`set_enabled`],
//! [`is_enabled`] and [`list_all`]. The settings types themselves
//! (`BlockState`, `BlockSettings`, the planner) stay in
//! [`crate::features`]; only the table access lives here.

use std::{collections::HashMap, sync::Arc};

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::{
    clients::database as db,
    interfaces::database::service::{DatabaseError, DatabaseService},
};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    db_read::{self, Bound},
    features::{
        plan_seed_decisions, BlockSettings, BlockState, ExistingRow, MigrationState, SeedDecision,
        SeedOp, USER_EDITED_SENTINEL,
    },
    util::RecordExt,
};

pub const TABLE: &str = "impresspress__admin__block_settings";

/// One row of the block_settings table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSettingsRow {
    pub id: String,
    pub block_name: String,
    /// Stored as the integer column `enabled` (migration 001, default 1).
    pub enabled: bool,
    /// SHA-256 hex of the migration SQL that has been applied; empty = never.
    pub current_hash: String,
    /// SHA-256 hex of the migration SQL the operator has blessed; empty = never.
    pub blessed_hash: String,
    /// `"seed:<hex>"` for a seed-managed row, [`USER_EDITED_SENTINEL`] for an
    /// admin-UI toggle, the shared-vars payload hash on admin's own row, empty
    /// for a legacy row (migration 003).
    pub seed_defaults_hash: String,
    pub created_at: String,
    pub updated_at: String,
}

impl BlockSettingsRow {
    /// Decode one row. `block_name` and `enabled` are required (both `NOT
    /// NULL`); a row without them is not a block setting.
    pub fn from_record(id: &str, data: &HashMap<String, Value>) -> Result<Self, String> {
        let block_name = data.str_field("block_name");
        if block_name.is_empty() {
            return Err(format!("{TABLE} row `{id}` has no block_name"));
        }
        if data.get("enabled").is_none() {
            return Err(format!("{TABLE} row `{id}` has no enabled column"));
        }
        Ok(Self {
            id: id.to_string(),
            block_name: block_name.to_string(),
            enabled: data.bool_field("enabled"),
            current_hash: data.str_field("current_hash").to_string(),
            blessed_hash: data.str_field("blessed_hash").to_string(),
            seed_defaults_hash: data.str_field("seed_defaults_hash").to_string(),
            created_at: data.str_field("created_at").to_string(),
            updated_at: data.str_field("updated_at").to_string(),
        })
    }

    /// The column map this row inserts as.
    pub fn to_data(&self) -> HashMap<String, Value> {
        let mut data = HashMap::new();
        data.insert("id".to_string(), json!(self.id));
        data.insert("block_name".to_string(), json!(self.block_name));
        data.insert("enabled".to_string(), json!(i64::from(self.enabled)));
        data.insert("current_hash".to_string(), json!(self.current_hash));
        data.insert("blessed_hash".to_string(), json!(self.blessed_hash));
        data.insert(
            "seed_defaults_hash".to_string(),
            json!(self.seed_defaults_hash),
        );
        data.insert("created_at".to_string(), json!(self.created_at));
        data.insert("updated_at".to_string(), json!(self.updated_at));
        data
    }

    /// The row as the per-block state the router and the migration gate read.
    pub fn state(&self) -> BlockState {
        BlockState {
            enabled: self.enabled,
            migration: MigrationState {
                current_hash: self.current_hash.clone(),
                blessed_hash: self.blessed_hash.clone(),
            },
            seed_defaults_hash: self.seed_defaults_hash.clone(),
        }
    }
}

/// The columns an upsert may set; `None` leaves the stored value alone (or
/// takes the column default when the row is created).
#[derive(Debug, Clone, Default)]
pub struct BlockSettingsPatch {
    pub enabled: Option<bool>,
    pub current_hash: Option<String>,
    pub blessed_hash: Option<String>,
    pub seed_defaults_hash: Option<String>,
}

impl BlockSettingsPatch {
    /// The column map for an `update`: the set fields plus `updated_at`.
    fn to_update_data(&self) -> HashMap<String, Value> {
        let mut data = HashMap::new();
        if let Some(enabled) = self.enabled {
            data.insert("enabled".to_string(), json!(i64::from(enabled)));
        }
        if let Some(current_hash) = &self.current_hash {
            data.insert("current_hash".to_string(), json!(current_hash));
        }
        if let Some(blessed_hash) = &self.blessed_hash {
            data.insert("blessed_hash".to_string(), json!(blessed_hash));
        }
        if let Some(seed_defaults_hash) = &self.seed_defaults_hash {
            data.insert("seed_defaults_hash".to_string(), json!(seed_defaults_hash));
        }
        data.insert("updated_at".to_string(), json!(crate::util::now_rfc3339()));
        data
    }

    /// The row to create when `block_name` has none yet: `enabled` defaults
    /// to `true` (the fallback every reader applies to an absent row), the
    /// hashes to empty, with a synthesised `bs_<uuid>` id and both timestamps
    /// set to now.
    fn into_row(self, block_name: &str) -> BlockSettingsRow {
        let now = crate::util::now_rfc3339();
        BlockSettingsRow {
            id: format!("bs_{}", uuid::Uuid::new_v4()),
            block_name: block_name.to_string(),
            enabled: self.enabled.unwrap_or(true),
            current_hash: self.current_hash.unwrap_or_default(),
            blessed_hash: self.blessed_hash.unwrap_or_default(),
            seed_defaults_hash: self.seed_defaults_hash.unwrap_or_default(),
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

fn decode_error(e: String) -> WaferError {
    WaferError::new(ErrorCode::Internal, e)
}

/// Decode every record, warning about and skipping the ones that do not
/// decode (the policy the loaders have always applied to a malformed row).
fn decode_rows<'a>(
    records: impl IntoIterator<Item = (&'a str, &'a HashMap<String, Value>)>,
) -> Vec<BlockSettingsRow> {
    records
        .into_iter()
        .filter_map(|(id, data)| match BlockSettingsRow::from_record(id, data) {
            Ok(row) => Some(row),
            Err(e) => {
                tracing::warn!(error = %e, "block_settings table contains an undecodable row");
                None
            }
        })
        .collect()
}

fn settings_from_rows(rows: &[BlockSettingsRow]) -> BlockSettings {
    BlockSettings::from_blocks(
        rows.iter()
            .map(|row| (row.block_name.clone(), row.state()))
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Boot flavour: over `DatabaseService`, before WRAP.
// ---------------------------------------------------------------------------

/// Every row, read with the full-table shape the KV row cache recognises.
///
/// Built by `cache_key` rather than open-coded: that shape is what
/// `read_key` recognizes as the cacheable full-table read, so a local
/// literal here could drift out of cache coverage silently.
///
/// # The missing table is this function's own problem
///
/// A first-ever deploy reads this table BEFORE anything has created it:
/// `/_deploy/prepare` builds a runtime, and only the next line runs the
/// migrations. So "table absent" is an ordinary cold start here and must
/// yield no rows, while every other failure must propagate — fabricating
/// [`BlockSettings::default`] out of an outage reports every block enabled.
///
/// That tolerance used to be inherited from `DbExec::list`, which returns
/// `Ok(RecordList::default())` for a table its `table_present_for_op` guard
/// says is absent. **STRICT_SCHEMA disables that guard**: it makes
/// `table_present_for_op` return `Ok(true)` unconditionally (trusting the
/// migrated schema is the whole point), so the SELECT runs and the backend
/// answers "no such table". Cloudflare deploys set the var, so relying on
/// that guard made the cold start depend on a flag that exists to remove it.
///
/// The probe is [`DatabaseService::schema_table_exists`], which `DbExec`
/// deliberately routes past `table_present_for_op` straight to
/// `dbx_table_exists` and which strict mode therefore does not disable.
/// [`crate::platform_state::wrap_grants::load`] — the other read on this
/// same pre-migration path — probes the same way.
///
/// It runs only AFTER a failed read, where `wrap_grants` probes first,
/// because this read is the one the Cloudflare KV row cache is built to
/// serve (`cache_key::read_key`): probing first would spend a database
/// round-trip on every cold isolate start even when the cache could answer
/// the list without one. On a healthy deployment the table exists, the list
/// succeeds, and the probe never runs at all.
async fn read_rows(db: &Arc<dyn DatabaseService>) -> Result<Vec<BlockSettingsRow>, DatabaseError> {
    let opts = crate::cache_key::full_table_list_opts();
    let listed = match db.list(TABLE, &opts).await {
        Ok(listed) => listed,
        Err(e) => {
            // A probe that cannot answer is not evidence the table is there,
            // so anything but a definite `false` keeps the original error.
            if let Ok(false) = db.schema_table_exists(TABLE).await {
                tracing::debug!(
                    "block_settings table absent (fresh boot before migrations); no rows"
                );
                return Ok(Vec::new());
            }
            tracing::error!(
                error = %e,
                "block_settings list failed (operational error, not a missing table); \
                 refusing to fabricate all-enabled defaults"
            );
            return Err(e);
        }
    };
    Ok(decode_rows(
        listed.records.iter().map(|r| (r.id.as_str(), &r.data)),
    ))
}

/// Read and parse `block_settings` without inserting or updating anything.
///
/// This is the ordinary Cloudflare request-path loader. Structural seeding is
/// a deploy/boot mutation and must use [`load_and_seed`] only after admin
/// migration has created the canonical table. Keeping this path physically
/// write-free prevents a cold runtime from invalidating itself via the KV
/// config-generation bump.
pub async fn load(db: &Arc<dyn DatabaseService>) -> Result<BlockSettings, DatabaseError> {
    let rows = read_rows(db).await?;
    Ok(settings_from_rows(&rows))
}

/// Read `block_settings` rows, run the hash-gated [`plan_seed_decisions`]
/// planner, apply the resulting inserts/updates, and return the post-seed
/// [`BlockSettings`].
///
/// `defaults` is the `(block_name, default_enabled)` set to seed towards,
/// which every production caller builds with
/// [`crate::blocks::block_enabled_defaults`]; a block absent from it keeps
/// whatever row it has and is never given one.
///
/// This is the single implementation behind every target's block-settings
/// load: the Cloudflare runner, the browser config loader, AND the native
/// CLI, which previously read the table without ever running the #222
/// hash-gate, so a changed default silently never propagated on native
/// boots. Routing native through here closes that gap.
///
/// Steady state: the planner returns an empty `Vec`, so zero writes are
/// issued and the only cost is the initial list (+ no re-read).
///
/// # Error semantics
///
/// A missing `block_settings` table (fresh DB, or a cold Cloudflare isolate
/// whose first request races admin's `Init`) is **not** an error condition
/// here at all: [`DatabaseService::list`]'s shared `DbExec` implementation
/// already guards on table existence and returns `Ok(RecordList::default())`
/// for a table that doesn't exist yet (see `DbExec::list` in
/// `wafer-core/src/interfaces/database/exec.rs`). That is the one and only
/// place the "tolerant" cold-start case is handled.
///
/// Consequently, an `Err` reaching this function is **always** a genuine
/// operational failure (backend outage, corruption, permissions) — never the
/// missing-table case. Silently substituting [`BlockSettings::default`] here
/// used to fabricate "every block enabled" out of a real error (CODE_REVIEW
/// finding: "Feature settings fail open to all-blocks-enabled"). Instead this
/// propagates the error so the caller can decide the right failure policy —
/// every current caller treats it as fatal to the boot/build (fail closed:
/// no runtime gets built/served with a fabricated all-enabled snapshot).
pub async fn load_and_seed(
    db: &Arc<dyn DatabaseService>,
    defaults: &[(String, bool)],
) -> Result<BlockSettings, DatabaseError> {
    let rows = read_rows(db).await?;

    // Existing-row map for the hash-gate planner.
    let existing: HashMap<String, ExistingRow> = rows
        .iter()
        .map(|row| {
            (
                row.block_name.clone(),
                ExistingRow {
                    enabled: row.enabled,
                    hash: row.seed_defaults_hash.clone(),
                },
            )
        })
        .collect();

    // `block_name` → row `id`, so a `SeedOp::Update` can do a single-row
    // `db.update` (which the KV wrapper invalidates) instead of `update_where`
    // (which hard-errors on cached tables, so a changed seed hash would never
    // propagate to existing rows).
    let id_by_block: HashMap<String, String> = rows
        .iter()
        .map(|row| (row.block_name.clone(), row.id.clone()))
        .collect();

    let decisions = plan_seed_decisions(&existing, defaults);
    let any_writes = !decisions.is_empty();
    for d in &decisions {
        apply_seed_decision(db, d, &id_by_block).await?;
    }

    // Re-read only when something changed (rare). Costs one extra read.
    let final_rows = if any_writes {
        read_rows(db).await?
    } else {
        rows
    };

    Ok(settings_from_rows(&final_rows))
}

/// Apply one [`SeedDecision`] via [`DatabaseService`]. Insert builds a fresh
/// row; Update resolves the row id from `id_by_block` (always present for an
/// Update, which is only planned for an existing row) and does a single-row
/// `db.update`. Failures propagate so a deploy/boot cannot claim structural
/// seeding succeeded while leaving missing or stale rows behind.
async fn apply_seed_decision(
    db: &Arc<dyn DatabaseService>,
    d: &SeedDecision,
    id_by_block: &HashMap<String, String>,
) -> Result<(), DatabaseError> {
    let patch = BlockSettingsPatch {
        enabled: Some(d.enabled),
        seed_defaults_hash: Some(d.hash.clone()),
        ..Default::default()
    };
    match d.op {
        SeedOp::Insert => {
            db.create(TABLE, patch.into_row(&d.block_name).to_data())
                .await?;
        }
        SeedOp::Update => {
            let Some(id) = id_by_block.get(&d.block_name) else {
                return Err(DatabaseError::Internal(format!(
                    "block_settings seed update for {} has no row id",
                    d.block_name
                )));
            };
            db.update(TABLE, id, patch.to_update_data()).await?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Runtime flavour: over `Context`, under WRAP.
// ---------------------------------------------------------------------------

/// Every row. A row that does not decode is skipped and warned about, the
/// policy the boot loader applies.
pub async fn list_all(ctx: &dyn Context) -> Result<Vec<BlockSettingsRow>, WaferError> {
    let records =
        db_read::list_bounded(ctx, TABLE, vec![], Bound::OnePer("registered block")).await?;
    Ok(decode_rows(
        records.iter().map(|r| (r.id.as_str(), &r.data)),
    ))
}

/// Whether `block_name` is enabled.
///
/// Defaults to `true` when no row exists (all blocks are enabled by
/// default). A read failure is returned, never mapped to "enabled": the
/// toggle handler derives the state it writes from this answer, so guessing
/// here would flip a block on the strength of an outage.
pub async fn is_enabled(ctx: &dyn Context, block_name: &str) -> Result<bool, WaferError> {
    match db::get_by_field(ctx, TABLE, "block_name", json!(block_name)).await {
        Ok(rec) => BlockSettingsRow::from_record(&rec.id, &rec.data)
            .map(|row| row.enabled)
            .map_err(decode_error),
        Err(e) if e.code == ErrorCode::NotFound => Ok(true),
        Err(e) => Err(e),
    }
}

/// Persist the `enabled` flag for `block_name`, marking the row as
/// user-owned ([`USER_EDITED_SENTINEL`]) so the boot-time seed never
/// overwrites an admin-UI toggle.
pub async fn set_enabled(
    ctx: &dyn Context,
    block_name: &str,
    enabled: bool,
) -> Result<(), WaferError> {
    upsert_fields(
        ctx,
        block_name,
        BlockSettingsPatch {
            enabled: Some(enabled),
            seed_defaults_hash: Some(USER_EDITED_SENTINEL.to_string()),
            ..Default::default()
        },
    )
    .await
}

/// Set a subset of columns on the row for `block_name`, creating the row
/// (`enabled = true`) when absent and preserving every column the patch
/// leaves unset otherwise.
///
/// Shared by `migration_helper::write_state` (the migration hash columns),
/// `admin::settings::seed_defaults` (`seed_defaults_hash`) and
/// [`set_enabled`], so every writer goes through the same
/// single-row-per-block primitive.
///
/// Get-then-write rather than a raw SQL upsert: the structured path hits
/// `DatabaseService::{create,update}`, which the Cloudflare
/// `KvCachedD1DatabaseService` invalidates — so toggling a block clears the
/// cached `block_settings` read (both the per-block key and the full-table
/// all-rows key). An atomic upsert would leave the eager [`load`] cache stale
/// until its TTL.
pub async fn upsert_fields(
    ctx: &dyn Context,
    block_name: &str,
    patch: BlockSettingsPatch,
) -> Result<(), WaferError> {
    let opts = ListOptions {
        filters: vec![Filter {
            field: "block_name".into(),
            operator: FilterOp::Equal,
            value: Value::String(block_name.to_string()),
        }],
        sort: vec![SortField {
            field: "created_at".into(),
            desc: false,
        }],
        limit: Some(1),
        offset: 0,
        skip_count: true,
        ..Default::default()
    };
    let existing = db::list(ctx, TABLE, &opts).await?;
    match existing.records.first() {
        Some(record) => {
            db::update(ctx, TABLE, &record.id, patch.to_update_data()).await?;
        }
        None => {
            db::create(ctx, TABLE, patch.into_row(block_name).to_data()).await?;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        features::USER_EDITED_SENTINEL,
        test_support::{FailingDbOpContext, TestContext},
    };

    /// The codec: every column `upsert_fields` writes comes back through
    /// `list_all` unchanged, `enabled` as a bool from the integer column.
    #[tokio::test]
    async fn upsert_fields_and_list_all_round_trip_every_column() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        upsert_fields(
            &ctx,
            "impresspress/probe",
            BlockSettingsPatch {
                enabled: Some(false),
                current_hash: Some("cur".to_string()),
                blessed_hash: Some("bless".to_string()),
                seed_defaults_hash: Some("seed:abc".to_string()),
            },
        )
        .await
        .expect("upsert");

        // `with_admin` stamps admin's own migration row, so select the probe's.
        let rows: Vec<_> = list_all(&ctx)
            .await
            .expect("list")
            .into_iter()
            .filter(|r| r.block_name == "impresspress/probe")
            .collect();
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert!(row.id.starts_with("bs_"), "{}", row.id);
        assert_eq!(row.block_name, "impresspress/probe");
        assert!(!row.enabled);
        assert_eq!(row.current_hash, "cur");
        assert_eq!(row.blessed_hash, "bless");
        assert_eq!(row.seed_defaults_hash, "seed:abc");
        assert!(!row.created_at.is_empty());
        assert_eq!(row.created_at, row.updated_at);

        let again = BlockSettingsRow::from_record(&row.id, &row.to_data()).expect("decode");
        assert_eq!(&again, row);

        let state = row.state();
        assert!(!state.enabled);
        assert_eq!(state.migration.current_hash, "cur");
        assert_eq!(state.migration.blessed_hash, "bless");
        assert_eq!(state.seed_defaults_hash, "seed:abc");
    }

    /// An absent block gets a row with `enabled = true` (the fallback every
    /// reader applies to a missing row), and a later patch that says nothing
    /// about `enabled` preserves whatever it holds.
    #[tokio::test]
    async fn upsert_fields_creates_enabled_and_preserves_the_flag_on_update() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        upsert_fields(
            &ctx,
            "impresspress/probe",
            BlockSettingsPatch {
                current_hash: Some("one".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("create");
        assert!(is_enabled(&ctx, "impresspress/probe").await.expect("read"));

        set_enabled(&ctx, "impresspress/probe", false)
            .await
            .expect("disable");
        upsert_fields(
            &ctx,
            "impresspress/probe",
            BlockSettingsPatch {
                current_hash: Some("two".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("update");

        let rows: Vec<_> = list_all(&ctx)
            .await
            .expect("list")
            .into_iter()
            .filter(|r| r.block_name == "impresspress/probe")
            .collect();
        assert_eq!(rows.len(), 1, "one row per block: {rows:?}");
        assert!(
            !rows[0].enabled,
            "a hash patch must not re-enable the block"
        );
        assert_eq!(rows[0].current_hash, "two");
    }

    /// `is_enabled` defaults to `true` when no row exists.
    #[tokio::test]
    async fn is_enabled_defaults_to_true_when_no_row() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        assert!(
            is_enabled(&ctx, "impresspress/nonexistent")
                .await
                .expect("no row is not an error"),
            "is_enabled should return true when no block_settings row exists"
        );
    }

    /// A read failure is not "enabled": the toggle handler writes the
    /// opposite of whatever this returns, so an outage must be an error.
    #[tokio::test]
    async fn is_enabled_surfaces_read_errors() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let failing = FailingDbOpContext::new(ctx, vec![("database.list", TABLE)]);
        assert!(
            is_enabled(&failing, "impresspress/files").await.is_err(),
            "an unreadable block_settings table must not read as enabled"
        );
    }

    /// `set_enabled` stamps `seed_defaults_hash` with the
    /// [`USER_EDITED_SENTINEL`] so the boot-time seed will never clobber an
    /// admin-UI toggle. See `plan_seed_decisions` in `features.rs`.
    #[tokio::test]
    async fn set_enabled_marks_row_user_edited() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let name = "impresspress/some-block";
        set_enabled(&ctx, name, false)
            .await
            .expect("set_enabled false");

        let rows = list_all(&ctx).await.expect("list block_settings");
        let rows: Vec<_> = rows.iter().filter(|r| r.block_name == name).collect();
        assert_eq!(rows.len(), 1, "exactly one block_settings row for {name}");
        assert_eq!(
            rows[0].seed_defaults_hash, USER_EDITED_SENTINEL,
            "set_enabled must stamp seed_defaults_hash with the user-edited sentinel",
        );
    }

    /// `set_enabled` / `is_enabled` round-trip: write false, read back false;
    /// write true, read back true.
    #[tokio::test]
    async fn set_enabled_round_trip() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let name = "impresspress/some-block";

        set_enabled(&ctx, name, false)
            .await
            .expect("set_enabled false");
        assert!(
            !is_enabled(&ctx, name).await.expect("read block setting"),
            "is_enabled should return false after set_enabled(false)"
        );

        set_enabled(&ctx, name, true)
            .await
            .expect("set_enabled true");
        assert!(
            is_enabled(&ctx, name).await.expect("read block setting"),
            "is_enabled should return true after set_enabled(true)"
        );
    }

    #[test]
    fn a_record_without_a_block_name_or_enabled_does_not_decode() {
        let mut data = HashMap::new();
        data.insert("enabled".to_string(), serde_json::json!(1));
        let err = BlockSettingsRow::from_record("bs_1", &data).expect_err("no block_name");
        assert!(err.contains(TABLE) && err.contains("bs_1"), "{err}");

        let mut data = HashMap::new();
        data.insert("block_name".to_string(), serde_json::json!("x/y"));
        let err = BlockSettingsRow::from_record("bs_1", &data).expect_err("no enabled");
        assert!(err.contains("enabled"), "{err}");
    }
}

/// End-to-end tests for [`load_and_seed`] against a real in-memory SQLite
/// [`DatabaseService`] — the path NATIVE now runs.
///
/// Before the loader was unified, native's own block-settings reader read
/// the `block_settings` table with a plain `SELECT` and never invoked the
/// #222 hash-gate. A changed default therefore propagated on Cloudflare
/// and browser boots but silently NOT on native boots. These
/// tests pin that the unified loader runs the gate, so a native boot now
/// re-seeds stale rows.
#[cfg(test)]
mod load_and_seed_tests {
    use super::*;
    use crate::features::{seed_hash_for, FeatureConfig, USER_EDITED_SENTINEL};

    /// The defaults these tests seed towards. A fixture, not the production
    /// block set: the loader's contract is "apply the planner's decisions over
    /// whatever defaults you were handed", and pinning it to the real registry
    /// would make every one of these tests re-assert the registry instead of
    /// the loader. The `block_settings` table has no foreign key to the block
    /// registry, so invented names round-trip exactly as real ones do.
    fn fixture() -> Vec<(String, bool)> {
        [
            ("org/alpha", true),
            ("org/bravo", false),
            ("org/charlie", true),
        ]
        .into_iter()
        .map(|(name, enabled)| (name.to_string(), enabled))
        .collect()
    }

    /// A first-ever deploy's database: opened, nothing created.
    fn unmigrated_db() -> Arc<dyn DatabaseService> {
        Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        )
    }

    #[tokio::test]
    async fn repro_cold_start_under_strict_schema() {
        let db = unmigrated_db();
        db.set_strict_schema(true);
        let result = load(&db).await;
        assert!(
            result.is_ok(),
            "a first deploy builds its runtime BEFORE migrations run, so this \
             read must tolerate the missing table even under STRICT_SCHEMA: {:?}",
            result.err(),
        );
    }

    /// A `DatabaseService` with the admin schema applied through the
    /// pre-wafer DDL runner (the migration-file-runner exception to the
    /// no-raw-SQL rule), so the table under test is the one production
    /// creates rather than a hand-rolled mirror of it.
    async fn migrated_db() -> Arc<dyn DatabaseService> {
        let db: Arc<dyn DatabaseService> = Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        );
        crate::migration_helper::apply_ddl_via_service(
            &db,
            crate::blocks::admin::migrations::ddl_files("sqlite"),
        )
        .await
        .expect("apply admin migrations");
        db
    }

    async fn read_row(db: &Arc<dyn DatabaseService>, block_name: &str) -> Option<(bool, String)> {
        read_rows(db)
            .await
            .expect("read rows")
            .into_iter()
            .find(|r| r.block_name == block_name)
            .map(|r| (r.enabled, r.seed_defaults_hash))
    }

    /// Insert a row as if a previous build had seeded it at `hash`.
    async fn seed_row(db: &Arc<dyn DatabaseService>, block_name: &str, enabled: bool, hash: &str) {
        let row = BlockSettingsPatch {
            enabled: Some(enabled),
            seed_defaults_hash: Some(hash.to_string()),
            ..Default::default()
        }
        .into_row(block_name);
        db.create(TABLE, row.to_data()).await.expect("insert row");
    }

    /// Fresh table → every block in the defaults is inserted at its current
    /// seed hash (the native-fresh-boot case).
    #[tokio::test]
    async fn seeds_all_defaults_on_empty_table() {
        let db = migrated_db().await;
        let defaults = fixture();
        let settings = load_and_seed(&db, &defaults).await.expect("load_and_seed");
        for (name, default) in &defaults {
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
        let db = migrated_db().await;
        let defaults = fixture();
        let (block_name, current_default) = defaults[0].clone();
        let stale_default = !current_default;
        seed_row(
            &db,
            &block_name,
            stale_default,
            &seed_hash_for(stale_default),
        )
        .await;

        // Pre-condition: the row is at the stale value.
        let (before_enabled, before_hash) = read_row(&db, &block_name).await.unwrap();
        assert_eq!(before_enabled, stale_default);
        assert_eq!(before_hash, seed_hash_for(stale_default));

        let settings = load_and_seed(&db, &defaults).await.expect("load_and_seed");

        // Post-condition: the gate fired — row updated to the current default.
        let (after_enabled, after_hash) = read_row(&db, &block_name).await.unwrap();
        assert_eq!(
            after_enabled, current_default,
            "stale row should have been re-seeded to the current default",
        );
        assert_eq!(after_hash, seed_hash_for(current_default));
        assert_eq!(settings.is_block_enabled(&block_name), current_default);
    }

    /// The Cloudflare request path is read-only: it returns the stored value
    /// exactly as found and neither updates a stale seed hash nor inserts the
    /// other default rows. Deploy init owns those mutations.
    #[tokio::test]
    async fn read_only_loader_never_applies_seed_plan() {
        let db = migrated_db().await;
        let (block_name, current_default) = fixture()[0].clone();
        let stale_default = !current_default;
        let stale_hash = seed_hash_for(stale_default);
        seed_row(&db, &block_name, stale_default, &stale_hash).await;

        let settings = load(&db).await.expect("read-only load");

        assert_eq!(settings.is_block_enabled(&block_name), stale_default);
        assert_eq!(
            read_row(&db, &block_name).await,
            Some((stale_default, stale_hash))
        );
        assert_eq!(
            read_rows(&db).await.expect("read rows").len(),
            1,
            "read-only load must not insert the rest of the defaults"
        );
    }

    /// A `user-edited` row must be preserved — admin-UI toggles win over the
    /// seed even when the loader runs on every boot.
    #[tokio::test]
    async fn preserves_user_edited_row() {
        let db = migrated_db().await;
        let defaults = fixture();
        let (block_name, default) = defaults[0].clone();
        let user_choice = !default;
        seed_row(&db, &block_name, user_choice, USER_EDITED_SENTINEL).await;

        let settings = load_and_seed(&db, &defaults).await.expect("load_and_seed");

        let (after_enabled, after_hash) = read_row(&db, &block_name).await.unwrap();
        assert_eq!(after_enabled, user_choice, "user choice must be preserved");
        assert_eq!(after_hash, USER_EDITED_SENTINEL);
        assert_eq!(settings.is_block_enabled(&block_name), user_choice);
    }

    /// Steady state: a table already at every current hash issues zero writes
    /// and round-trips unchanged.
    #[tokio::test]
    async fn no_writes_at_steady_state() {
        let db = migrated_db().await;
        let defaults = fixture();
        // First pass seeds everything.
        load_and_seed(&db, &defaults).await.expect("load_and_seed");
        // Capture every row to detect any spurious write on the second pass.
        let before = read_rows(&db).await.expect("snapshot before");
        // Second pass should be a no-op (empty plan).
        load_and_seed(&db, &defaults).await.expect("load_and_seed");
        let after = read_rows(&db).await.expect("snapshot after");
        assert_eq!(before, after, "steady-state pass must not write");
    }
}

/// Regression coverage for the fail-open finding: a genuine operational read
/// error must propagate as `Err`, never fabricate `BlockSettings::default()`
/// (which reads as "every block enabled").
#[cfg(test)]
mod operational_error_tests {
    use wafer_block::db::Filter;
    use wafer_core::interfaces::database::service::{
        AggregateSpec, Column, DatabaseError, Record, RecordList, Table, UpsertSpec,
    };

    use super::*;
    use crate::features::{BlockSettings, FeatureConfig};

    /// A [`DatabaseService`] whose `list` always fails with a simulated
    /// operational error (backend outage / corruption — NOT a missing
    /// table, which `DbExec::list` already handles by returning
    /// `Ok(RecordList::default())` before any error can surface). Every
    /// other method is `unreachable!`: `load_and_seed` must short-circuit at
    /// the first `list()` call and never reach them.
    struct AlwaysErrorsOnList;

    #[async_trait::async_trait]
    impl DatabaseService for AlwaysErrorsOnList {
        fn statement_budget(
            &self,
        ) -> Result<wafer_core::interfaces::database::service::StatementBudget, DatabaseError>
        {
            Ok(wafer_core::interfaces::database::service::StatementBudget::Unbounded)
        }
        async fn get(&self, _collection: &str, _id: &str) -> Result<Record, DatabaseError> {
            unreachable!("must not read a record after a list failure")
        }

        async fn list(
            &self,
            _collection: &str,
            _opts: &ListOptions,
        ) -> Result<RecordList, DatabaseError> {
            Err(DatabaseError::Internal(
                "simulated block_settings outage (not a missing-table condition)".into(),
            ))
        }

        async fn create(
            &self,
            _collection: &str,
            _data: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            unreachable!("must not write after a list failure")
        }

        async fn update(
            &self,
            _collection: &str,
            _id: &str,
            _data: HashMap<String, serde_json::Value>,
        ) -> Result<Record, DatabaseError> {
            unreachable!("must not write after a list failure")
        }

        async fn delete(&self, _collection: &str, _id: &str) -> Result<(), DatabaseError> {
            unreachable!()
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

        /// The table IS there; the failure this mock injects is operational.
        /// `read_rows` probes here after a failed list to tell the two apart,
        /// so answering `true` is what makes
        /// `genuine_read_error_propagates_instead_of_fabricating_all_enabled`
        /// test the case it is named for rather than the cold-start case.
        async fn schema_table_exists(&self, _name: &str) -> Result<bool, DatabaseError> {
            Ok(true)
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
    }

    /// The core regression: a genuine read error must NOT be treated as "all
    /// enabled". Before this fix, the loader caught the error and returned
    /// `BlockSettings::default()`, whose `is_block_enabled` reports `true`
    /// for every block name — silently enabling every feature (including
    /// ones normally gated off) on a real outage. It must now propagate
    /// `Err` instead.
    #[tokio::test]
    async fn genuine_read_error_propagates_instead_of_fabricating_all_enabled() {
        let db: Arc<dyn DatabaseService> = Arc::new(AlwaysErrorsOnList);
        let defaults = vec![("org/alpha".to_string(), true)];
        let result = load_and_seed(&db, &defaults).await;

        assert!(
            result.is_err(),
            "an operational read error must surface as Err, not a fabricated BlockSettings"
        );

        // Spell out the danger the old behavior risked: `BlockSettings::default()`
        // (an empty map) reports every block enabled, including ones that would
        // never legitimately default to on.
        let fabricated_default = BlockSettings::default();
        assert!(
            fabricated_default.is_block_enabled("some/never-configured-block"),
            "sanity check: BlockSettings::default() is the all-enabled trap this fix avoids"
        );

        assert!(
            load(&db).await.is_err(),
            "the read-only request loader must also fail closed on an operational read error"
        );
    }
}
