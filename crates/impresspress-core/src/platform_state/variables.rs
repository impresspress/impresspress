//! `impresspress__admin__variables`: the admin-managed configuration store —
//! one row per config key, the value every block resolves its declared
//! [`wafer_block::ConfigVar`]s against.
//!
//! Two callers, one codec (spec 2.1.2). The boot flavour runs before WRAP
//! over [`DatabaseService`]: native seeds pre-wafer (its immutable crypto
//! service and config snapshot need the variables before `build()`),
//! Cloudflare and the browser seed after admin's `Init` has created the
//! table. The runtime flavour runs under WRAP over [`Context`]: admin's
//! settings surface and the dev sandbox's seed diagnostics. Both decode rows
//! through [`VariableRow::from_record`] and write through
//! [`VariableRow::to_data`], so a column name is spelled in this file only.
//!
//! Before this module existed each platform carried its own copy of the
//! seeders with documented drift between them (the audit's Top-10 #9); the
//! boot functions here are that single copy, moved from the former
//! `boot.rs`.

use std::{collections::HashMap, sync::Arc};

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::{clients::database as db, interfaces::database::service::DatabaseService};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    db_read::{self, Bound},
    util::RecordExt,
};

pub const TABLE: &str = "impresspress__admin__variables";

/// One row of the variables table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableRow {
    pub id: String,
    pub key: String,
    pub value: String,
    pub name: String,
    pub description: String,
    pub warning: String,
    /// Masked in every listing and never clearable through the admin UI.
    /// Stored as the integer column `sensitive` (migration 001).
    pub sensitive: bool,
    /// The `{ORG}__{BLOCK}` prefix of the key (migration 002): what
    /// `D1ConfigSource` groups rows by, so a block sees only its own. `None`
    /// for shared (`WAFER_RUN_SHARED__*`) and ad hoc keys, whose column stays
    /// NULL.
    pub block: Option<String>,
    /// The admin user who last wrote the row through the admin surface;
    /// empty for rows the seeders wrote.
    pub updated_by: String,
    pub created_at: String,
    pub updated_at: String,
}

impl VariableRow {
    /// Decode one row. `id` comes from the `Record` envelope (both the WRAP
    /// client's and the service's carry it); `data` is its column map. The
    /// only column that cannot be defaulted is `key`: a keyless row is
    /// corruption, not a variable.
    pub fn from_record(id: &str, data: &HashMap<String, Value>) -> Result<Self, String> {
        let key = data.str_field("key");
        if key.is_empty() {
            return Err(format!("{TABLE} row `{id}` has no key"));
        }
        Ok(Self {
            id: id.to_string(),
            key: key.to_string(),
            value: data.str_field("value").to_string(),
            name: data.str_field("name").to_string(),
            description: data.str_field("description").to_string(),
            warning: data.str_field("warning").to_string(),
            sensitive: data.bool_field("sensitive"),
            block: data.opt_str_field("block").filter(|b| !b.is_empty()),
            updated_by: data.str_field("updated_by").to_string(),
            created_at: data.str_field("created_at").to_string(),
            updated_at: data.str_field("updated_at").to_string(),
        })
    }

    /// The column map this row inserts as. `block` is omitted when `None` so
    /// the column stays NULL rather than becoming an empty string.
    pub fn to_data(&self) -> HashMap<String, Value> {
        let mut data = HashMap::new();
        data.insert("id".to_string(), json!(self.id));
        data.insert("key".to_string(), json!(self.key));
        data.insert("value".to_string(), json!(self.value));
        data.insert("name".to_string(), json!(self.name));
        data.insert("description".to_string(), json!(self.description));
        data.insert("warning".to_string(), json!(self.warning));
        data.insert("sensitive".to_string(), json!(i64::from(self.sensitive)));
        if let Some(block) = &self.block {
            data.insert("block".to_string(), json!(block));
        }
        data.insert("updated_by".to_string(), json!(self.updated_by));
        data.insert("created_at".to_string(), json!(self.created_at));
        data.insert("updated_at".to_string(), json!(self.updated_at));
        data
    }
}

/// The `block` column for a key by migration 002's rule
/// ([`crate::config_vars::key_block_prefix`]): `None` when the key carries no
/// `{ORG}__{BLOCK}__` prefix.
pub fn block_for_key(key: &str) -> Option<String> {
    let block = crate::config_vars::key_block_prefix(key);
    (!block.is_empty()).then_some(block)
}

/// A row to insert. `block` is explicit: env-seeded and admin-created rows
/// derive it from the key ([`block_for_key`]); an auto-generated secret is
/// tagged with the block that declared it
/// ([`crate::config_vars::screaming_block`]), because that is the block
/// `D1ConfigSource` must hand the row to.
#[derive(Debug, Clone)]
pub struct NewVariable {
    pub key: String,
    pub value: String,
    pub name: String,
    pub description: String,
    pub warning: String,
    pub sensitive: bool,
    pub updated_by: String,
    pub block: Option<String>,
}

impl NewVariable {
    /// The row this becomes: a synthesised `var_<uuid>` id and both
    /// timestamps set to now.
    ///
    /// The funnel for creating a variables row through this module —
    /// `insert_if_absent`, [`set`]'s create branch, [`insert`] and therefore
    /// [`upsert_by_key`]'s create branch all pass through here — which is why
    /// the `sensitive` flag is settled here rather than trusted from each
    /// caller.
    ///
    /// One writer to this table does NOT come through here:
    /// `blocks::dev::data_snapshot::import` upserts a bundle's own columns
    /// straight through `db::upsert`. It applies the same rule itself
    /// (`raise_imported_sensitive_flag`) rather than being routed here, because
    /// it is writing rows that already exist elsewhere rather than minting new
    /// ones — it must keep the bundle's `id` and timestamps, which this
    /// function synthesises.
    ///
    /// `sensitive` is RAISED to whatever
    /// [`crate::config_vars::is_sensitive_for_storage`] says the key requires,
    /// and never lowered: a caller may mark an ad hoc row sensitive on its own
    /// authority, but it may not mark a `Password`-typed declared var or a
    /// `*_SECRET`/`*_KEY` key as safe to publish.
    ///
    /// Two call sites used to derive this from the key's spelling alone —
    /// `seed_and_load`'s env loop and `blocks::config`'s `CONFIG_SET` — and
    /// both therefore stored
    /// `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` unflagged, since it
    /// is declared `InputType::Password` and ends in neither suffix. After that
    /// `util::is_sensitive_key` had nothing left to go on and the settings API
    /// served the password verbatim. Settling it here is what makes the next
    /// call site safe without it having to know the rule.
    ///
    /// It does NOT cover a key the build has never heard of: it raises from the
    /// declaration or the suffix, and an ad hoc key is neither. That case is
    /// [`VariablePatch::into_new`]'s default, which is how both write surfaces
    /// that create a row for a key they were merely handed reach this
    /// function: the admin PUT ([`upsert_by_key`]) and [`set`]'s create branch,
    /// which is what `blocks::config`'s `CONFIG_SET` writes through. A caller
    /// that builds a [`NewVariable`] itself — [`seed_if_absent`], seeding a key
    /// it chose — is stating the flag rather than omitting it, and keeps what
    /// it stated.
    pub fn into_row(self) -> VariableRow {
        let now = crate::util::now_rfc3339();
        let sensitive = self.sensitive || crate::config_vars::is_sensitive_for_storage(&self.key);
        VariableRow {
            id: format!("var_{}", uuid::Uuid::new_v4()),
            key: self.key,
            value: self.value,
            name: self.name,
            description: self.description,
            warning: self.warning,
            sensitive,
            block: self.block,
            updated_by: self.updated_by,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

/// The columns an update may change; `None` leaves the stored value alone.
/// `key`, `id`, `block` and `created_at` are never patched.
#[derive(Debug, Clone, Default)]
pub struct VariablePatch {
    pub value: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub warning: Option<String>,
    pub sensitive: Option<bool>,
    pub updated_by: Option<String>,
}

impl VariablePatch {
    /// The column map for an `update`: the set fields plus `updated_at`.
    fn to_update_data(&self) -> HashMap<String, Value> {
        let mut data = HashMap::new();
        if let Some(value) = &self.value {
            data.insert("value".to_string(), json!(value));
        }
        if let Some(name) = &self.name {
            data.insert("name".to_string(), json!(name));
        }
        if let Some(description) = &self.description {
            data.insert("description".to_string(), json!(description));
        }
        if let Some(warning) = &self.warning {
            data.insert("warning".to_string(), json!(warning));
        }
        if let Some(sensitive) = self.sensitive {
            data.insert("sensitive".to_string(), json!(i64::from(sensitive)));
        }
        if let Some(updated_by) = &self.updated_by {
            data.insert("updated_by".to_string(), json!(updated_by));
        }
        data.insert("updated_at".to_string(), json!(crate::util::now_rfc3339()));
        data
    }

    /// The row to create when the key has none yet: unset fields take the
    /// column defaults, `block` is derived from the key.
    ///
    /// `sensitive` is the exception to "unset means the column default". An
    /// unset flag takes [`crate::config_vars::is_sensitive_by_default_when_created`],
    /// which protects an undeclared ad hoc key, because `false` here was a real
    /// hole: `admin::ops::update_variable` builds a patch that never sets
    /// `sensitive`, so a `PATCH /b/admin/api/settings/MY_SERVICE_TOKEN` on a key
    /// with no row stored it unflagged and the next GET published it — while the
    /// same key through POST was protected by `handle_create`'s "absent means
    /// sensitive" rule. `NewVariable::into_row` could not save it either: that
    /// funnel raises for the declared set and the `_SECRET`/`_KEY` suffix, and
    /// an ad hoc key is neither.
    ///
    /// A caller that means it still wins by saying so — `dev::seed::record_failure`
    /// passes an explicit `Some(false)` for its diagnostic row.
    ///
    /// [`set_with_row`]'s create branch builds one of these too, so
    /// `blocks::config`'s `CONFIG_SET` and the admin PUT create a row by the
    /// same rule. Its `sensitive` argument is a floor rather than an assertion
    /// — `false` there means the caller has nothing to say and takes this
    /// default — which is why it maps to `None` and not to `Some(false)`.
    fn into_new(self, key: &str) -> NewVariable {
        NewVariable {
            key: key.to_string(),
            value: self.value.unwrap_or_default(),
            name: self.name.unwrap_or_default(),
            description: self.description.unwrap_or_default(),
            warning: self.warning.unwrap_or_default(),
            sensitive: self
                .sensitive
                .unwrap_or_else(|| crate::config_vars::is_sensitive_by_default_when_created(key)),
            updated_by: self.updated_by.unwrap_or_default(),
            block: block_for_key(key),
        }
    }
}

fn key_filter(key: &str) -> Filter {
    Filter {
        field: "key".to_string(),
        operator: FilterOp::Equal,
        value: Value::String(key.to_string()),
    }
}

fn decode_error(e: String) -> WaferError {
    WaferError::new(ErrorCode::Internal, e)
}

// ---------------------------------------------------------------------------
// Boot flavour: over `DatabaseService`, before WRAP.
// ---------------------------------------------------------------------------

/// The row for `key`, if any, read the way every boot function reads it.
///
/// `DatabaseService::list` tolerates a missing table (it returns empty), so
/// on a fresh database this is a clean "absent" rather than an error.
pub async fn find_by_key(
    db: &Arc<dyn DatabaseService>,
    key: &str,
) -> Result<Option<VariableRow>, String> {
    let opts = ListOptions {
        filters: vec![key_filter(key)],
        limit: Some(1),
        offset: 0,
        skip_count: true,
        ..Default::default()
    };
    let listed = db
        .list(TABLE, &opts)
        .await
        .map_err(|e| format!("list {TABLE} for key `{key}`: {e}"))?;
    listed
        .records
        .first()
        .map(|r| VariableRow::from_record(&r.id, &r.data))
        .transpose()
}

/// `INSERT OR IGNORE` semantics: insert `row` only when no row with its key
/// exists. A pre-existing row (a prior boot's seed, an admin-UI edit) always
/// wins — seeding never clobbers a stored value.
///
/// This is the right shape for a *default* and the wrong one for an
/// instruction. [`seed_and_load`] used it for the process environment and that
/// is precisely the bug it now documents: an env var is not a default, so it
/// goes through [`set`] instead.
///
/// Returns `Ok(true)` when a row was inserted, `Ok(false)` when one already
/// existed. Errors bubble up so the caller can decide whether a failed seed
/// is fatal (a missing JWT secret) or merely logged (best-effort secrets).
async fn insert_if_absent(db: &Arc<dyn DatabaseService>, row: VariableRow) -> Result<bool, String> {
    if find_by_key(db, &row.key).await?.is_some() {
        return Ok(false);
    }
    db.create(TABLE, row.to_data())
        .await
        .map_err(|e| format!("insert variable `{}`: {e}", row.key))?;
    // Every boot seeder funnels through here. A config reader that memoized
    // the table before this row existed must see that it moved — the browser
    // seeds after admin's Init through the raw service, with no KV-cached
    // wrapper to record the write on its behalf.
    crate::config_generation::note_config_write();
    Ok(true)
}

/// Seed one variable when absent.
///
/// Public so platform code can seed its own non-declared defaults (the
/// browser's bootstrap-admin credentials and WebLLM script var) through the
/// same `DatabaseService` path. The `block` column is derived from the key
/// ([`block_for_key`]), matching migration 002.
///
/// Returns `Ok(true)` when a row was inserted, `Ok(false)` when one already
/// existed.
pub async fn seed_if_absent(
    db: &Arc<dyn DatabaseService>,
    key: &str,
    value: &str,
    name: &str,
    description: &str,
    sensitive: bool,
) -> Result<bool, String> {
    let row = NewVariable {
        key: key.to_string(),
        value: value.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        warning: String::new(),
        sensitive,
        updated_by: String::new(),
        block: block_for_key(key),
    }
    .into_row();
    insert_if_absent(db, row).await
}

/// Write `value` for `key`, overwriting whatever is stored.
///
/// The counterpart to [`seed_if_absent`], and the one to reach for when a
/// value is a **fact about this deployment** rather than a default an
/// operator may override. `seed_if_absent` cannot express that: by the time
/// a platform's post-admin seed hook runs, the admin block's
/// `lifecycle(Init)` has already written every declared
/// [`crate::config_vars`] default, so "insert if absent" on a declared key is
/// a guaranteed no-op. That is exactly how the browser sandbox's
/// `WAFER_RUN_SHARED__HAS_LANDING_PAGE = "true"` silently lost to the
/// declared `"false"`.
///
/// `sensitive` is an OPTION, and the two arms are different statements. `Some`
/// is the caller vouching for the key — it knows what the value is — and
/// `None` is "I have nothing to say", which is what a seeder handed a key by
/// its environment has. They diverge only when the row has to be CREATED, where
/// `None` takes [`crate::config_vars::is_sensitive_by_default_when_created`] —
/// the rule that protects a key no `ConfigVar` declares — and `Some(false)`
/// stores the plain row the caller asked for.
/// [`NewVariable::into_row`] raises from the declaration and the
/// `_SECRET`/`_KEY` suffix on top of either, so `Some(false)` cannot publish a
/// key the build knows to be a secret.
///
/// On an existing row this writes `value` and, when the caller says the value
/// is sensitive and the stored flag is clear, raises `sensitive` — never
/// lowers it. `Some(false)` and `None` therefore do the same thing there:
/// nothing reaching THIS function can lower a stored flag, so "plain" and "no
/// opinion" are the same instruction on an update. That is a property of this
/// path, not of the system — `PATCH /b/admin/api/settings/{key}` carrying
/// `{"sensitive": false}` DOES clear the flag, through
/// `admin::settings::handle_set` → `ops::update_variable` →
/// [`VariablePatch::sensitive`], and
/// [`crate::config_vars::is_sensitive_by_default_when_created`] relies on that
/// being possible, since unflagging in the admin UI is how an operator makes an
/// ad hoc row exportable. `name` and `description` describe the variable rather than the
/// deployment, so an operator's wording survives; `sensitive` is different in
/// kind, because it is the only thing that carries an AD HOC row's
/// sensitivity — one the build declares no `ConfigVar` for, so
/// `util::is_sensitive_key`'s key half has nothing to say about it — and
/// because a row stored unflagged for a key that should be flagged is a
/// disagreement between the column and the declaration, which the boot repair
/// pass then has to reconcile. The metadata arguments are otherwise used only
/// when the row has to be created — the same shape [`seed_if_absent`] takes, so
/// the two read alike at a call site.
///
/// Says which of the four things it did ([`Wrote`]). A boot that re-asserts a
/// value it already holds performs no write at all, which is what keeps this
/// callable unconditionally on every boot.
pub async fn set(
    db: &Arc<dyn DatabaseService>,
    key: &str,
    value: &str,
    name: &str,
    description: &str,
    sensitive: Option<bool>,
) -> Result<Wrote, String> {
    set_with_owner(db, key, value, name, description, sensitive, None).await
}

/// [`set`] from an ADMIN SURFACE: the value is written and the row is stamped
/// admin-owned, so [`seed_and_load`] will not let the process environment
/// overwrite it again.
///
/// `admin_id` is the acting admin's user id where the surface knows it. The
/// settings forms do not: `wafer_core::clients::config::set` builds a fresh
/// message rather than forwarding the caller's (`svc!`, not `svc_msg!`), so
/// `CONFIG_SET` never sees the admin's identity. That path passes
/// [`crate::features::USER_EDITED_SENTINEL`] — the same marker
/// `plan_seed_decisions` already writes into `block_settings.seed_defaults_hash`
/// for an admin-UI toggle, reused rather than re-invented so the two surfaces
/// spell "a human owns this row" the same way.
pub async fn set_by_admin(
    db: &Arc<dyn DatabaseService>,
    key: &str,
    value: &str,
    sensitive: Option<bool>,
    admin_id: &str,
) -> Result<Wrote, String> {
    set_with_owner(db, key, value, "", "", sensitive, Some(admin_id)).await
}

/// The `updated_by` marker [`seed_and_load`]'s one-time upgrade transition
/// writes onto a row it keeps.
///
/// Deliberately NOT [`crate::features::USER_EDITED_SENTINEL`]. That sentinel
/// asserts a human made a decision, and the transition cannot know that — the
/// row predates edit tracking, so "an admin edited this" and "an earlier boot's
/// environment wrote this" are indistinguishable. A separate marker is what
/// lets the steady-state WARN and the Variables page say the true thing
/// ("pinned at upgrade") instead of a plausible-sounding false one.
pub const PRE_UPGRADE_SENTINEL: &str = "pre-upgrade";

/// The `updated_by` marker [`reset_to_environment`] writes.
///
/// NOT an empty string, which is what it used to write, and the difference is
/// load-bearing. Empty means "no surface has ever spoken for this row", which is
/// precisely the condition the upgrade transition acts on — so a reset that
/// wrote empty could be undone by a later transition, on a deployment whose
/// first boots carried no exports at all and therefore never recorded the gate.
/// The operator's reset is a decision; it says "the environment owns this", not
/// "nobody has considered this".
///
/// It reads as UNPINNED ([`pin_of`] answers `None`), so the environment sets the
/// key on every boot — which is the whole point of the control.
pub const RELEASED_TO_ENV_SENTINEL: &str = "released-to-environment";

/// The row whose presence records that [`seed_and_load`]'s one-time
/// env-precedence transition has already run.
///
/// A block-scoped key, so it is ordinary storable config rather than a
/// runtime-owned one — the same shape `dev::seed`'s
/// `IMPRESSPRESS__DEV__SEED_ERROR` diagnostic row already uses.
pub const ENV_PRECEDENCE_TRANSITION_KEY: &str = "IMPRESSPRESS__ADMIN__ENV_PRECEDENCE_TRANSITION";

/// Internal, adapter-injected key: `"1"` when this deployment boots from a
/// process environment, absent otherwise.
///
/// Published by the NATIVE runtime only (`impresspress_server::build_native_runtime`),
/// which is the only target that hands [`seed_and_load`] a non-empty
/// `env_vars`: Cloudflare never calls it at all, and the browser calls it with
/// `&[]`. Every pin this module records exists to resist a process environment,
/// so a surface that offers to hand a key BACK to one has to know whether there
/// is one — see [`deployment_seeds_from_process_env`].
///
/// Bracketed in double underscores, so it is an internal key by
/// [`crate::config_vars::is_internal_key`]: served only from the boot map, never
/// stored in this table, and refused by every write surface. Same shape as
/// `products::RUNTIME_KIND_CONFIG_KEY`.
///
/// FAIL-CLOSED, and that is the whole reason it is a new key rather than a third
/// value for `RUNTIME_KIND_CONFIG_KEY`: that one defaults to `"server"`, so a
/// Cloudflare boot path that forgot to publish `"cloudflare"` would claim a
/// process environment it does not have. Absent here means "no", so the failure
/// mode of forgetting to publish it is a control that does not render — which
/// leaves the documented API route working — rather than one that lies.
///
/// "Native only" is structural, not merely a convention nobody has broken yet.
/// A Cloudflare deploy cannot set it from `wrangler.toml`: that target fills its
/// boot map from `runtime_build::structural_config_inputs`, which reads only
/// keys on `environment::PROTECTED_ENV_KEYS` and
/// `environment::BUILDER_WORKER_VAR_KEYS` — two `&'static [&'static str]`
/// allowlists this key is not on — so an arbitrary worker var of this name is
/// never copied through. Worth stating because the same question is worth
/// asking of any `__…__` key, and the answer is not the same for all of them.
pub const HAS_PROCESS_ENV_CONFIG_KEY: &str = "__IMPRESSPRESS_HAS_PROCESS_ENV__";

/// Whether this deployment boots from a process environment, and so whether
/// handing a key back to one means anything here.
///
/// See [`HAS_PROCESS_ENV_CONFIG_KEY`] for why absent means "no". A failed
/// read is returned.
pub async fn deployment_seeds_from_process_env(
    ctx: &dyn Context,
) -> Result<bool, wafer_run::WaferError> {
    Ok(wafer_core::clients::config::get_default(ctx, HAS_PROCESS_ENV_CONFIG_KEY, "").await? == "1")
}

/// Why a stored row outranks the process environment, when it does.
///
/// The signal is `updated_by`, which already carried the admin half of this
/// meaning: `admin::ops::{create_variable, update_variable}` stamp the acting
/// admin's id, and every seeder — the env loop, [`seed_if_absent`],
/// [`seed_one_secret`], [`seed_jwt_secret`], `seed_defaults`' create branch —
/// leaves it empty. [`set_by_admin`] closes the one admin surface that did not
/// stamp it.
///
/// Empty therefore means "nothing has claimed this row", NOT "unknown". That is
/// the opposite reading to `plan_seed_decisions`, where an empty hash means
/// legacy-and-preserve — and deliberately so: there, an empty hash is rare
/// legacy state, while here it is what every seeded row carries, so treating it
/// as claimed would stop the environment seeding anything at all and undo the
/// bug this PR exists to fix. The rows that really are legacy are handled once,
/// by the upgrade transition in [`seed_and_load`], rather than by reading every
/// empty marker as a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    /// An admin of THIS instance wrote the row through an admin surface.
    AdminEdit,
    /// The one-time upgrade transition kept the row: it predates edit tracking
    /// and its value differed from the export present at the time, so this
    /// build could not tell whether an admin had set it.
    PreUpgrade,
}

/// Why `row` outranks the process environment, or `None` when nothing has
/// claimed it and [`seed_and_load`] may seed it.
pub fn pin_of(row: &VariableRow) -> Option<Pin> {
    match row.updated_by.as_str() {
        "" | RELEASED_TO_ENV_SENTINEL => None,
        PRE_UPGRADE_SENTINEL => Some(Pin::PreUpgrade),
        // A user id, or `features::USER_EDITED_SENTINEL` from the settings
        // forms, which cannot learn the admin's id.
        _ => Some(Pin::AdminEdit),
    }
}

/// Whether NOTHING has ever spoken for this row — no admin surface, no upgrade
/// transition, no operator reset.
///
/// The condition the one-time upgrade transition acts on, and deliberately
/// narrower than `pin_of(row).is_none()`: a row an operator RELEASED reads as
/// unpinned, because the environment is meant to set it, but it is not
/// unconsidered and the transition must leave it alone.
fn is_unclaimed(row: &VariableRow) -> bool {
    row.updated_by.is_empty()
}

/// Whether anything has claimed `row` from the process environment — either
/// kind of [`Pin`].
pub fn is_pinned(row: &VariableRow) -> bool {
    pin_of(row).is_some()
}

/// Hand a key back to the process environment: clear its pin so
/// [`seed_and_load`] seeds it again on the next boot.
///
/// The counterpart to [`set_by_admin`], and the reason a pin is not a one-way
/// door. Without it a pinned key had no supported exit:
/// `admin::ops::delete_variable` refuses any declared `WAFER_RUN_SHARED__*`
/// row, and `update_variable` stamps `updated_by` on every write, so clearing
/// the value re-pinned the row it was meant to release.
///
/// Only the marker moves — the stored value stays until the next boot actually
/// re-seeds it. That keeps the action reversible in the window before a
/// restart, and means a key with no export simply keeps the value it has rather
/// than silently reverting to a declared default.
///
/// It writes [`RELEASED_TO_ENV_SENTINEL`] rather than emptying the column.
/// Empty is what an untouched row carries, and the upgrade transition acts on
/// exactly that — so on a deployment whose early boots carried no exports, and
/// which therefore has not recorded the transition gate yet, an emptied row
/// would be pinned straight back by the first boot that did carry one. See
/// [`is_unclaimed`].
pub async fn reset_to_environment(ctx: &dyn Context, key: &str) -> Result<(), WaferError> {
    let patch = VariablePatch {
        updated_by: Some(RELEASED_TO_ENV_SENTINEL.to_string()),
        ..Default::default()
    };
    upsert_by_key(ctx, key, patch).await.map(|_| ())
}

/// Whether the process environment can ever set `key`, and so whether handing
/// it back to the environment does anything.
///
/// A pin is real on any row an admin has edited, but [`reset_to_environment`]
/// only means something when a later boot will actually re-seed the key — and
/// every surface that offers the action promises exactly that ("replaced on the
/// next restart"). A surface must not render a control whose action is inert.
///
/// Derived from the production gate rather than restated:
/// `impresspress_server::filter_to_declared_keys` — the filter in front of
/// [`seed_and_load`] on native — keeps exactly
/// [`crate::config_vars::is_declared_key`], so a key outside it never reaches
/// the seeder whatever the environment says. `WAFER_RUN__AUTH__JWT_SECRET` is
/// the case that matters and needs no special mention here: it is declared by
/// no `ConfigVar`, which is precisely why the filter strips it, even though
/// `admin::ops::reject_runtime_owned_key` deliberately lets an admin edit it.
///
/// The runtime-owned half is [`seed_and_load`]'s own guard, restated here
/// because the two refusals are independent and a surface must mirror both.
///
/// Answers a different question from [`deployment_seeds_from_process_env`],
/// which is per DEPLOYMENT rather than per key: that one says whether there is
/// a process environment at all on this target, this one says whether a
/// particular key could ever come from it. Both have to hold before handing a
/// key back does anything.
pub fn key_can_be_seeded_from_env(key: &str) -> bool {
    crate::config_vars::is_declared_key(key) && !crate::config_vars::is_runtime_owned_key(key)
}

/// A key [`seed_and_load`]'s one-time upgrade transition pinned, which the
/// process environment can still set.
///
/// The input type of the bulk release (`admin::ops::release_keys_pinned_at_upgrade`),
/// and what stops that action SELECTING a [`Pin::AdminEdit`] row. Releasing an
/// admin edit would silently undo the decision rule 2 of the precedence
/// contract exists to make permanent, so "which keys" is not a question a UI
/// surface gets to answer.
///
/// Three things together are what make it an answer the surface cannot forge,
/// and all three are load-bearing:
///
/// * the inner `String` is private to this module, so nobody outside can build
///   one from a key;
/// * [`Self::of`] is the only constructor and is private too;
/// * [`keys_pinned_at_upgrade`] is the only PUBLIC way to obtain one, and it
///   reads the real table.
///
/// The third is easy to lose. [`count_pinned_at_upgrade`] takes caller-supplied
/// rows, and [`VariableRow`] is a public struct with public fields — so had that
/// function returned `PinnedAtUpgrade` values instead of a count, any caller
/// could have forged a row (`updated_by: PRE_UPGRADE_SENTINEL`) for a key
/// nothing pinned and minted one from it. It returns `usize` for exactly that
/// reason, not because a count happened to be all the page wanted.
///
/// What this type does NOT prove is the state of the row at the moment of the
/// WRITE: it is evidence about a read that has already happened. See
/// `admin::ops::ReleaseGuard::StillPinnedAtUpgrade` for the half that speaks
/// for the row itself.
#[derive(Debug)]
pub struct PinnedAtUpgrade(String);

impl PinnedAtUpgrade {
    /// `Some` when `row` is one the bulk release may act on.
    ///
    /// The match on [`Pin`] is EXHAUSTIVE on purpose rather than an equality
    /// test against [`Pin::PreUpgrade`]: a third pin kind added later is then a
    /// compile error here, where somebody has to decide whether a bulk release
    /// may clear it, instead of silently falling into either answer.
    fn of(row: &VariableRow) -> Option<Self> {
        match pin_of(row)? {
            // The transition kept this row because it could not prove an admin
            // set it. Handing the whole set back is the operator supplying the
            // proof it lacked.
            Pin::PreUpgrade => {}
            // A person made this decision and this build recorded it. Nothing
            // here may undo it in bulk.
            Pin::AdminEdit => return None,
        }
        // A pin on a key no environment can set is real, but releasing it
        // changes nothing — the same rule that keeps the per-key control off
        // such a row.
        key_can_be_seeded_from_env(&row.key).then(|| Self(row.key.clone()))
    }

    /// The config key, for the release itself and the audit row that records
    /// it. Read-only: there is no way back from a `&str` to one of these.
    pub fn key(&self) -> &str {
        &self.0
    }
}

/// Every key in `rows` the one-time upgrade transition pinned and this
/// deployment could still seed from its process environment.
///
/// PRIVATE, and the doc on [`PinnedAtUpgrade`] says why: this is the one place
/// that mints them from rows, and rows are forgeable by any caller. The two
/// public faces of it are [`count_pinned_at_upgrade`] (a number, for rendering)
/// and [`keys_pinned_at_upgrade`] (the values, over rows this module read
/// itself).
fn pinned_at_upgrade(rows: &[VariableRow]) -> Vec<PinnedAtUpgrade> {
    rows.iter().filter_map(PinnedAtUpgrade::of).collect()
}

/// How many of `rows` the one-time upgrade transition pinned and this
/// deployment could still seed from its process environment.
///
/// Over rows the caller already holds, so the admin Variables page can count
/// them from the listing it renders rather than reading the table a second
/// time, and through the same [`pinned_at_upgrade`] the action's selection uses
/// — which is what makes "the number on the button equals the number of rows
/// below it offering their own reset control" true by construction rather than
/// by two filters agreeing.
///
/// A COUNT rather than the values, so that handing rows in cannot mint a
/// [`PinnedAtUpgrade`]; see that type's doc.
///
/// Says nothing about whether there IS a process environment — that is
/// [`deployment_seeds_from_process_env`], which a surface has to check as well.
pub fn count_pinned_at_upgrade(rows: &[VariableRow]) -> usize {
    pinned_at_upgrade(rows).len()
}

/// [`pinned_at_upgrade`] over the whole table, for a caller that is acting
/// rather than rendering. The only public source of [`PinnedAtUpgrade`] values.
///
/// The bulk release reads for itself rather than trusting a set a request
/// carried: what the page rendered is a snapshot, and the authoritative
/// question is what the table says now. (Even this is only "now" as of the
/// read — see `admin::ops::ReleaseGuard::StillPinnedAtUpgrade` for the window
/// between it and each write.)
pub async fn keys_pinned_at_upgrade(ctx: &dyn Context) -> Result<Vec<PinnedAtUpgrade>, WaferError> {
    Ok(pinned_at_upgrade(&list_all(ctx).await?))
}

async fn set_with_owner(
    db: &Arc<dyn DatabaseService>,
    key: &str,
    value: &str,
    name: &str,
    description: &str,
    sensitive: Option<bool>,
    updated_by: Option<&str>,
) -> Result<Wrote, String> {
    let existing = find_by_key(db, key).await?;
    set_with_row(
        db,
        key,
        value,
        name,
        description,
        sensitive,
        updated_by,
        existing,
    )
    .await
}

/// [`set_with_owner`] for a caller that has ALREADY read the row.
///
/// `seed_and_load`'s env loop reads each row to check ownership before
/// deciding to write; without this it would then pay a second `list` per key
/// inside the write — 40-160 extra round trips on a native cold start, for
/// rows it is holding already.
#[expect(
    clippy::too_many_arguments,
    reason = "the already-read row is a parameter so the env seed loop does not \
              pay a second `list` per key"
)]
async fn set_with_row(
    db: &Arc<dyn DatabaseService>,
    key: &str,
    value: &str,
    name: &str,
    description: &str,
    sensitive: Option<bool>,
    updated_by: Option<&str>,
    existing: Option<VariableRow>,
) -> Result<Wrote, String> {
    let Some(existing) = existing else {
        // Through [`VariablePatch::into_new`], the shared create default, so
        // this path protects an undeclared ad hoc key exactly as the admin PUT
        // does. `sensitive` is carried through as the `Option` it arrived as:
        // `None` means the caller has nothing to say and takes
        // `is_sensitive_by_default_when_created`, `Some` is the caller
        // vouching for the key. `NewVariable::into_row` then raises from the
        // declaration and the `_SECRET`/`_KEY` suffix on top of either, as it
        // does for every other creator.
        let row = VariablePatch {
            value: Some(value.to_string()),
            name: Some(name.to_string()),
            description: Some(description.to_string()),
            sensitive,
            updated_by: Some(updated_by.unwrap_or_default().to_string()),
            ..Default::default()
        }
        .into_new(key)
        .into_row();
        db.create(TABLE, row.to_data())
            .await
            .map_err(|e| format!("insert variable `{key}`: {e}"))?;
        crate::config_generation::note_config_write();
        return Ok(Wrote::Created);
    };
    // The `sensitive` flag is the one piece of metadata a forced write DOES
    // touch, and only ever upward. `name`/`description` describe the variable
    // and an operator's wording survives; `sensitive` decides whether the
    // value reaches an API response at all, and for an AD HOC key — one no
    // `ConfigVar` declares — it is the only thing that can say so, because
    // `util::is_sensitive_key`'s key half asks the declaration and there is
    // none. A row stored with the flag clear when the caller knows better is
    // a column that contradicts the declaration, so it is repaired in place.
    // Never lowered: the read path's union means every disagreement must
    // resolve towards more masking.
    // The declaration has the final say on the update path too, not just at
    // the create funnel: `blocks::config`'s `CONFIG_SET` passes the row's OWN
    // stored flag for an existing row, so a row already stored unflagged would
    // otherwise re-assert its own mistake forever.
    //
    // `None` and `Some(false)` are the same instruction here, and that is not
    // a conflation: the flag is never lowered, so the only thing a caller can
    // ask for is a RAISE, and both of those decline to ask for one. They part
    // company on the create branch above, where storing a row means answering
    // the question one way or the other.
    let sensitive = sensitive.unwrap_or(false) || crate::config_vars::is_sensitive_for_storage(key);
    let raise_sensitive = sensitive && !existing.sensitive;
    let value_changed = existing.value != value;
    // Ownership is stamped ONLY when the value actually moved.
    //
    // It used to stamp on any admin-surface write, on the reasoning that
    // saving a form is a human decision. That is false per key:
    // `ui::settings_form::save_settings` calls `config::set` for every PLAIN
    // field the form posts, and the form posts every named input — so changing
    // one colour on an admin settings page pinned `APP_NAME`, every logo URL
    // and the favicon too, silently making their exports inert forever.
    // Requiring a real change keeps the claim honest: the admin edited THIS
    // key.
    //
    // "Plain" because `save_settings` skips a blank SENSITIVE field —
    // `render_field` renders a secret empty, so blank means "I did not touch
    // this". That skip is not a substitute for this rule: every key named
    // above is a plain one, posted and written on every save, so the fields
    // that motivated the rule are exactly the ones it does not cover.
    //
    // The narrow cost is that an admin cannot pin a key by re-saving the value
    // the environment already supplies — but there is nothing to pin then, the
    // two agree, and any later divergence is a real edit that stamps.
    let stamp_owner = value_changed && updated_by.is_some_and(|who| who != existing.updated_by);
    if !value_changed && !raise_sensitive && !stamp_owner {
        return Ok(Wrote::Unchanged);
    }
    let patch = VariablePatch {
        value: value_changed.then(|| value.to_string()),
        sensitive: raise_sensitive.then_some(true),
        updated_by: stamp_owner.then(|| updated_by.unwrap_or_default().to_string()),
        ..Default::default()
    };
    db.update(TABLE, &existing.id, patch.to_update_data())
        .await
        .map_err(|e| format!("update variable `{key}`: {e}"))?;
    crate::config_generation::note_config_write();
    Ok(if value_changed {
        Wrote::Replaced
    } else if raise_sensitive {
        Wrote::FlagRaised
    } else {
        // An ownership stamp on its own. Nothing a config reader can observe
        // moved, which is what this variant reports.
        Wrote::Unchanged
    })
}

/// What a [`set`] call did to the row for its key.
///
/// Four cases rather than "did it write", because the callers that log have to
/// tell them apart: creating the row for a key nobody had set yet is
/// unremarkable, replacing a value someone else stored is the thing an operator
/// needs to be told about, and raising a `sensitive` flag is a repair that must
/// not be reported as either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrote {
    /// No row existed for the key; one was created.
    Created,
    /// A row existed holding a different value, and that value was replaced.
    /// Its `sensitive` flag may have been raised in the same write.
    Replaced,
    /// The row already held this value, but its `sensitive` flag was clear
    /// and the caller knows the value is sensitive, so only the flag was
    /// raised. A repair, not a config change.
    FlagRaised,
    /// Nothing a config reader can observe changed: the row already held this
    /// value and its flag was already right. (An admin-ownership stamp may
    /// still have been recorded — that is bookkeeping, not configuration.)
    Unchanged,
}

/// Auto-generate random 32-byte hex secrets for every [`wafer_block::ConfigVar`]
/// declared with `.auto_generate()` that lacks a row. Shared by all three
/// targets.
///
/// Idempotent: a key that already has a row is left untouched. Per-key
/// failures are logged and tolerated — operators retain the manual seed
/// fallback.
///
/// Ordering contract: this MUST run after the admin block's `lifecycle(Init)`
/// (so migration 002's `block` column exists) and BEFORE the remaining blocks
/// initialize, on the targets that seed post-admin (Cloudflare, browser) —
/// which is exactly the slot [`crate::builder::BootHooks::seed_after_admin_init`]
/// occupies in [`crate::builder::boot`]. Native seeds pre-wafer, so it ensures
/// the tables itself first via [`crate::migration_helper::apply_ddl_via_service`].
pub async fn seed_auto_generated(db: &Arc<dyn DatabaseService>) {
    let block_infos = crate::blocks::all_block_infos();
    for info in &block_infos {
        let block_col = crate::config_vars::screaming_block(&info.name);
        for var in &info.config_keys {
            if !var.auto_generate {
                continue;
            }
            match seed_one_secret(db, &block_col, var).await {
                Ok(true) => tracing::warn!(
                    key = %var.key,
                    block = %info.name,
                    "auto-generated secret seeded (no row existed)"
                ),
                Ok(false) => {}
                Err(e) => tracing::warn!(
                    key = %var.key,
                    block = %info.name,
                    error = %e,
                    "seed_auto_generated failed"
                ),
            }
        }
    }
}

fn random_hex_secret() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("getrandom: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Generate one 32-byte hex secret and insert it for `var` when absent,
/// tagged with the declaring block's `block` column.
async fn seed_one_secret(
    db: &Arc<dyn DatabaseService>,
    block_col: &str,
    var: &wafer_block::ConfigVar,
) -> Result<bool, String> {
    let row = NewVariable {
        key: var.key.clone(),
        value: random_hex_secret()?,
        name: var.name.clone(),
        description: var.description.clone(),
        warning: var.warning.clone(),
        sensitive: true,
        updated_by: String::new(),
        block: (!block_col.is_empty()).then(|| block_col.to_string()),
    }
    .into_row();
    insert_if_absent(db, row).await
}

/// Write `env_vars` into the table, auto-generate any `auto_generate` secrets,
/// reconcile every row's `sensitive` flag, and return the full key→value map
/// currently stored.
///
/// `env_vars` is empty for the browser and Cloudflare targets (their config
/// lives in the platform store, not process env) and carries the
/// declared-key-filtered process environment on native.
///
/// ## Precedence: the environment seeds, and an admin edit wins for good
///
/// The contract, decided by the project owner:
///
/// 1. A key **no admin has edited** takes the environment's value on every
///    boot. That is the original bug fixed — a fresh deployment honours its
///    `.env`, and so does an existing one whose admin has left the key alone.
/// 2. Once an admin has written the row through an admin surface
///    ([`Pin::AdminEdit`]), the ROW wins permanently and the export is inert
///    for that key.
///
/// ## The upgrade transition: pin on conflict, once
///
/// Rows written before this release carry no marker, so on the first boot after
/// upgrading, an admin's settings-form edit and an earlier boot's env seed look
/// identical. Rule 1 applied to them would revert the edit.
///
/// That is not a hypothetical. `variables::set`'s create branch writes
/// `updated_by: String::new()`, and `ui::settings_form::save_settings` →
/// `config::set` → `CONFIG_SET` was the write path for the auth-ui settings
/// form, which carries `WAFER_RUN_SHARED__ALLOW_SIGNUP`,
/// `WAFER_RUN_SHARED__ENABLE_OAUTH` and both `BOOTSTRAP_ADMIN_*` keys. An admin
/// who closed signup during an abuse incident left no provenance at all, so
/// "let the environment win once, loudly" would reopen signup on the upgrade
/// boot — exactly the harm rule 2 exists to prevent — and there is nothing to
/// restore the old value from afterwards: [`reset_to_environment`] clears a pin,
/// it does not remember a value, and the audit log stores no previous value.
///
/// So the transition is **pin on conflict**, run once:
///
/// - An UNCLAIMED row ([`is_unclaimed`] — nothing has ever spoken for it) whose
///   value DIFFERS from a present export is kept, stamped
///   [`PRE_UPGRADE_SENTINEL`], and named in a WARN.
/// - An unclaimed row that already EQUALS its export is left alone, silently:
///   there is no conflict to resolve and nothing to tell anybody.
/// - Once [`ENV_PRECEDENCE_TRANSITION_KEY`] exists, rules 1 and 2 apply as
///   written and the transition never runs again.
///
/// Two things keep an operator's [`reset_to_environment`] from being undone,
/// and both are needed. The gate stops the transition re-running at all, and
/// [`RELEASED_TO_ENV_SENTINEL`] stops a released row looking unclaimed — which
/// matters because the gate is only recorded by a boot that HAD an environment,
/// so a deployment whose early boots carried no exports reaches the admin UI
/// with the transition still armed.
///
/// ### Exactly what the transition promises
///
/// Stated positively, because the negative version of it has been written
/// wrong twice — each time more generously than the branch condition above.
/// The condition IS the promise, so here it is in words:
///
/// > An UNSTAMPED pre-upgrade edit — the settings-form path described above —
/// > survives if and only if, on the boot that records the gate (the first one
/// > whose declared batch is non-empty AND whose [`record_transition`]
/// > succeeds), the environment exported THAT key, with a non-empty value,
/// > that value differed from the stored one, and the row was readable.
///
/// The "unstamped" is load-bearing: an edit made through Admin → Variables was
/// already stamped with the editing admin's id before this PR existed, so it is
/// [`Pin::AdminEdit`], [`is_unclaimed`] is false, the transition passes over it
/// and the export is inert forever — none of the clauses below apply to it.
///
/// Every clause is load-bearing, and dropping any of them leaves the row
/// unclaimed and the gate recorded, so a LATER export wins over the pre-upgrade
/// edit — silently, because a key the transition passes over is a key it says
/// nothing about. `a_pre_upgrade_edit_is_unprotected_unless_the_export_differed`
/// drives the three reachable ways to drop one:
///
/// - the key is absent from that batch (`WAFER_RUN_SHARED__ENABLE_OAUTH` is not
///   in the compose file on the upgrade boot; the operator adds it months later
///   and OAuth comes back on);
/// - it is exported EMPTY, which this module treats as unset;
/// - it is exported with the value already stored — the admin had already
///   aligned the two, and there is no conflict to see.
///
/// (Two more exist, neither reachable from a test with the fixtures here,
/// because `break_reads` / `break_writes` are all-or-nothing and each also
/// disables the gate write.
///
/// - A per-key READ that fails is skipped, and the gate is still recorded at
///   the end of the loop. A read failure that takes out the whole table cannot
///   cause it — [`transition_has_run`] fails closed, so the transition does not
///   run and the gate is not recorded either.
/// - A per-key PIN WRITE that fails: [`pin_at_upgrade`] returns `false`, the
///   loop continues, and the gate is recorded regardless — pin outcomes are not
///   consulted at the recording site. Unlike the three reachable cases this one
///   is LOUD: the `Err` arm warns that the process environment may overwrite
///   the row on the next boot.)
///
/// A boot whose batch is EMPTY records no gate at all, so a deployment that
/// exports nothing yet keeps the transition armed for the first boot that does
/// — see `a_boot_with_no_environment_records_no_transition_and_writes_nothing`.
///
/// None of this is closed by stamping the rows the transition passes over. A row
/// that AGREES with its export is most rows on most deployments, and pinning
/// those re-breaks the headline bug for every key an operator has been waiting
/// to set. The remedy is the operator's and it is one action: re-apply the
/// setting in the admin UI once after upgrading, which stamps it for good. An
/// admin edit made AFTER the upgrade is always safe; that is the line, and
/// `RELEASE.md` tells operators the same thing in their own terms.
///
/// It lives INSIDE the env loop rather than in a pass of its own, which is what
/// makes it cover every declared key — block-scoped secrets like
/// `IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY` included, where reverting to a
/// rotated-out credential is the costly failure — rather than only the keys
/// `config_vars::shared_config_vars` names. It also means the comparison is
/// against the export actually present, not against a declared default: a
/// default is no evidence of anything, and two of them
/// (`WAFER_RUN_SHARED__FAVICON_URL`, `WAFER_RUN_SHARED__LOGO_ICON_URL`) are
/// content-hashed asset URLs that move whenever those bytes change, so any row
/// would look edited after an unrelated release.
///
/// The seeding half used to be missing entirely: env values were written with
/// `insert_if_absent`, so they only ever landed on the very first boot of a
/// fresh database. From the second boot on a row existed and every
/// `WAFER_RUN_SHARED__*` export was silently discarded — an operator who set
/// `WAFER_RUN_SHARED__APP_NAME=Foo` on a running deployment saw nothing change
/// and got no log line saying why.
///
/// The bootstrap-admin pair looked like it still worked, which is what hid the
/// defect: their declared default is `""`, so their row is empty, and
/// [`crate::blocks::config`]'s read order falls an empty row through to the
/// boot map — whose `EnvConfigService` reads `std::env` directly. Only keys
/// with a non-empty default were affected, and only after the first boot.
///
/// ## Why admin-wins has to be loud
///
/// Rule 2 reintroduces "this env var has no effect" for edited keys, which is
/// the *shape* of the defect this module was fixed for. The difference has to
/// be that it is announced: every boot logs a WARN naming each key whose export
/// is inert, saying the stored admin edit is what boots and how to hand the key
/// back. Silence here would trade one invisible failure for another and fix
/// nothing.
///
/// Ownership is marked the way `block_settings` already marks it — see
/// [`crate::features::plan_seed_decisions`], which skips any row stamped
/// [`crate::features::USER_EDITED_SENTINEL`]. This mirrors that convention
/// rather than inventing a second one: the marker lives in `updated_by`
/// (see [`pin_of`]), and the settings-form surface writes that same sentinel.
/// The upgrade transition is the one thing that writes a DIFFERENT marker,
/// because it is making a weaker claim and the log lines have to be able to say
/// so.
///
/// An **empty** env value is treated as unset and skipped, so `FOO=` in a
/// shell or `.env` cannot blank out a meaningful stored value. That is the
/// convention already used by
/// [`crate::blocks::admin::settings::seed_defaults`] and
/// [`crate::blocks::auth::config::AuthConfig::from_map`].
///
/// Keys the runtime owns ([`crate::config_vars::is_runtime_owned_key`] —
/// infrastructure `IMPRESSPRESS_*` and internal `__…__` keys) are refused here:
/// they are never variables-table config, and `blocks::config` would not serve
/// such a row anyway. On native they are already gone before this runs —
/// `collect_app_env_vars` drops every key without `__`, and
/// `impresspress_server::filter_to_declared_keys` drops every undeclared key —
/// so this guard is defence in depth for a caller that assembles its own
/// batch, not the thing standing between the process environment and the
/// table. (The same check does stand between the environment and native's
/// blocks: [`usable_env_exports`] applies it to the unfiltered app
/// environment.)
///
/// This path does **not** carry the JWT signing secret, even though
/// [`crate::config_vars::is_instance_owned_key`] names it as the one
/// stored-config key beyond the runtime-owned set.
/// `WAFER_RUN__AUTH__JWT_SECRET` is declared by no `ConfigVar` (the gap
/// [`seed_jwt_secret`] exists to paper over), so `filter_to_declared_keys`
/// removes it from the native batch before `seed_and_load` ever sees it:
/// exporting it changes nothing, and [`seed_jwt_secret`] auto-generates one
/// instead. The supported way to pin the secret is the admin surface, which
/// `admin::ops::reject_runtime_owned_key` deliberately exempts from its
/// refusal for exactly that reason. Making the env var work would mean
/// declaring the key — which pulls it into the admin config tables,
/// `seed_defaults` and the auto-generate loop — and is a design decision of
/// its own rather than part of this path.
///
/// PRECONDITION: the table must already exist — either because the admin
/// block's `lifecycle(Init)` has run (browser, Cloudflare), or because the
/// caller ensured it pre-wafer (native). `db.create` does not lazily create
/// tables.
pub async fn seed_and_load(
    db: &Arc<dyn DatabaseService>,
    env_vars: &[(String, String)],
) -> Result<HashMap<String, String>, String> {
    // 0. Has the one-time upgrade transition already run? One indexed lookup,
    //    outside the loop, rather than per key.
    //
    //    It is read even when `env_vars` is empty so the answer is available to
    //    the loop without a second branch, but the GATE ROW is only written when
    //    the loop had something to decide (below). A target with no process
    //    environment — Cloudflare's deploy hook, the browser's
    //    `config::seed_and_load_variables` — must not record a transition it
    //    never performed, and must not bump the config-write generation for it.
    let transition_done = transition_has_run(db).await;

    // 1. Apply env-provided values to keys nothing has pinned (see above).
    //
    //    Counted rather than advised per key: the recovery advice is identical
    //    for every one of them and goes out once, below, so the per-key lines
    //    carry only what differs.
    let mut inert_exports = 0usize;
    // How many of `inert_exports` are held by an upgrade pin rather than an
    // admin edit: the ones the page's BULK release would act on, and so the only
    // ones the summary line may point at it for.
    let mut upgrade_pins = 0usize;
    for (key, value) in env_vars {
        match check_env_export(key, value) {
            Ok(()) => {}
            Err(EnvRefusal::RuntimeOwned) => {
                tracing::warn!(
                    key = %key,
                    "refusing to store a runtime-owned key from the environment; \
                     infrastructure and adapter-injected keys are never variables-table config"
                );
                continue;
            }
            Err(EnvRefusal::Empty) => continue,
            Err(EnvRefusal::ValueRule(e)) => {
                log_refused_env_value(key, value, &e);
                continue;
            }
        }
        // Deliberately NOT `util::validate_url_value`, the guard
        // `blocks::config`'s `CONFIG_SET` and `admin::ops::update_variable`
        // apply to a `*_URL` key. Those two accept values from a browser
        // request; that guard exists to stop untrusted web input reaching
        // internal hosts (SSRF). The process environment is a different trust
        // boundary entirely — it is operator-supplied deployment config, as
        // trusted as the binary reading it — so applying a remote-input guard
        // to it is a category error, and an expensive one:
        // `IMPRESSPRESS__PRODUCTS__STRIPE_API_URL=http://127.0.0.1:12111` (the
        // Stripe mock) and `WAFER_RUN_SHARED__FRONTEND_URL=http://web:5173` (a
        // docker-compose service name) are exactly what local and containerised
        // development sets, and rejecting them would boot the declared default
        // instead. An operator who can set an env var can already point this
        // deployment anywhere.
        //
        // A PIN WINS. A row an admin has written through an admin surface, or
        // one the upgrade transition kept, owns its key from then on, and this
        // export is inert for it.
        //
        // Read once, and hand the row to the write below rather than letting
        // it list the table a second time for the same key.
        let existing = match find_by_key(db, key).await {
            Ok(row) => row,
            // A read failure is not a licence to overwrite: it means we cannot
            // tell whether the row is pinned, and the contract says the pinned
            // value survives. Skip and say so.
            Err(e) => {
                tracing::warn!(
                    key = %key,
                    error = %e,
                    "could not read the stored row for this config key; leaving it alone \
                     rather than risk overwriting an admin edit"
                );
                continue;
            }
        };
        if let Some(row) = &existing {
            // Announced at WARN, never silently: the defect this whole path
            // exists to fix was an env var being ignored without a word, and a
            // pin-wins rule that said nothing would have traded one silent
            // failure for another.
            //
            // Gated on the values actually DIFFERING. A pinned row that already
            // holds what the export says is not a conflict — nothing is being
            // ignored and the operator has nothing to do — and a WARN on every
            // boot for the common case is how an operator learns to skip the
            // one that matters.
            if let Some(pin) = pin_of(row) {
                if row.value != *value {
                    warn_export_is_inert(key, pin);
                    inert_exports += 1;
                    if pin == Pin::PreUpgrade {
                        upgrade_pins += 1;
                    }
                }
                continue;
            }
            // The one-time upgrade transition: a row NOTHING has spoken for,
            // disagreeing with the export, predates edit tracking — so keep it
            // and say so. A row an operator released reads as unpinned but is
            // not unclaimed, and must not be re-pinned here: that reset is the
            // operator saying the environment owns the key.
            if !transition_done && is_unclaimed(row) && row.value != *value {
                if pin_at_upgrade(db, row).await {
                    inert_exports += 1;
                    upgrade_pins += 1;
                }
                continue;
            }
        }
        // `None`, because a boot handed a key by its environment knows nothing
        // about what the value is: `set_with_row` settles the flag from the
        // key's declaration and the `_SECRET`/`_KEY` suffix
        // (`VariablePatch::into_new` on a create, `NewVariable::into_row` on
        // top of it), which is strictly more than this loop could assert.
        // Native filters this batch to declared keys before it gets here
        // (`impresspress_server::filter_to_declared_keys`), so the
        // undeclared-key default `into_new` applies is not what seeds a row on
        // this path.
        match set_with_row(db, key, value, "", "", None, None, existing).await {
            // Reached only for a row nothing has pinned, so the previous value
            // was a seeder's: a declared default, or an earlier boot's
            // environment.
            Ok(Wrote::Replaced) => tracing::warn!(
                key = %key,
                "the process environment replaced a previously seeded value for \
                 this config key; the new value is now what is stored, and \
                 removing the environment variable does NOT restore the old one \
                 — it only stops the override being re-applied on each boot"
            ),
            Ok(Wrote::Created | Wrote::FlagRaised | Wrote::Unchanged) => {}
            Err(e) => tracing::warn!(key = %key, error = %e, "failed to seed env variable"),
        }
    }

    if inert_exports > 0 {
        warn_how_to_undo_a_pin(inert_exports, upgrade_pins);
    }

    // 1b. Record that the transition has run — LAST, so a boot that dies
    //     partway retries rather than recording a pass that only half happened,
    //     and only when there was an environment to transition against.
    if !transition_done && !env_vars.is_empty() {
        record_transition(db).await;
    }

    // 2. Auto-generate declared secrets (incl. the auth JWT secret).
    seed_auto_generated(db).await;
    seed_jwt_secret(db).await;

    // 3. Load the full set back, reconciling any row whose stored `sensitive`
    //    flag disagrees with what its key requires while passing over it.
    let rows = load_rows(db).await?;
    repair_sensitive_flags_in(db, &rows).await;
    Ok(rows
        .into_iter()
        .map(|loaded| (loaded.row.key, loaded.row.value))
        .collect())
}

/// Why an environment export does not configure its key.
enum EnvRefusal {
    /// [`crate::config_vars::is_runtime_owned_key`]: never stored config.
    RuntimeOwned,
    /// Blank means unset, so the export must not blank out anything.
    Empty,
    /// The key's declared value rule refused the value, for this reason.
    ValueRule(String),
}

/// The checks every environment export must pass before it configures its
/// key, whichever surface it would configure: [`seed_and_load`] applies them
/// before storing a row, and [`usable_env_exports`] before handing the export
/// to the blocks' `ConfigSource`.
///
/// The key's declared value rule DOES apply, unlike the URL guard
/// [`seed_and_load`] deliberately leaves out: it is about what the value means to its reader, not
/// about who supplied it. Handing a reader a value it refuses would fail that
/// reader on every call (an out-of-range session lifetime fails every login),
/// so the export is refused; the stored value, else the declared default,
/// stays in effect.
fn check_env_export(key: &str, value: &str) -> Result<(), EnvRefusal> {
    if crate::config_vars::is_runtime_owned_key(key) {
        return Err(EnvRefusal::RuntimeOwned);
    }
    if value.is_empty() {
        return Err(EnvRefusal::Empty);
    }
    crate::config_vars::check_config_value(key, value).map_err(EnvRefusal::ValueRule)
}

/// The exports in `app_env` that may configure their key: every one
/// [`check_env_export`] accepts, the same verdict [`seed_and_load`] reaches
/// before storing a row.
///
/// Native's blocks resolve their declared keys from the variables table, then
/// from these, so an export the seeder refused must not reach a block through
/// this second route. Nothing is logged here: every key with a value rule is
/// a declared key (`config_vars`' `every_value_rule_names_a_declared_key`
/// holds that), and [`seed_and_load`] has already named a refused export of
/// one at ERROR.
pub fn usable_env_exports(app_env: &HashMap<String, String>) -> HashMap<String, String> {
    app_env
        .iter()
        .filter(|(key, value)| check_env_export(key, value).is_ok())
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Name an environment export a boot seeder refused for failing its key's
/// declared value rule ([`crate::config_vars::check_config_value`]). At ERROR:
/// the operator set something that is not taking effect. The value is shown
/// unless the key is sensitive, so the log line says exactly what to fix.
pub(crate) fn log_refused_env_value(key: &str, value: &str, reason: &str) {
    let shown = if crate::config_vars::is_sensitive_for_storage(key) {
        crate::util::MASKED_VALUE
    } else {
        value
    };
    tracing::error!(
        key = %key,
        value = %shown,
        reason = %reason,
        "refusing to seed this config key from the environment: its reader cannot use \
         the value; the stored value, else the declared default, stays in effect"
    );
}

/// JWT_SECRET is not declared as an `auto_generate: true` `ConfigVar` by the
/// auth block (a wafer-run config-keys gap noted in the auth block module), so
/// the auto-gen loop above never seeds it. Seed it here so the strict
/// empty-secret boot check (native `impresspress_server::build_native_runtime`) can't trip on a fresh DB and
/// the browser/CF crypto can pick up a real key. Idempotent.
async fn seed_jwt_secret(db: &Arc<dyn DatabaseService>) {
    let key = crate::blocks::auth::JWT_SECRET_KEY;
    let secret = match random_hex_secret() {
        Ok(secret) => secret,
        Err(e) => {
            tracing::warn!(error = %e, "getrandom failed for JWT secret");
            return;
        }
    };
    let row = NewVariable {
        key: key.to_string(),
        value: secret,
        name: "JWT signing secret".to_string(),
        description: "256-bit secret used to sign access + refresh JWTs.".to_string(),
        warning: "Rotating this secret invalidates every issued session.".to_string(),
        sensitive: true,
        updated_by: String::new(),
        block: block_for_key(key),
    }
    .into_row();
    match insert_if_absent(db, row).await {
        Ok(true) => {
            tracing::warn!(key = %key, "auto-generated JWT secret (not found in variables table)")
        }
        Ok(false) => {}
        Err(e) => tracing::warn!(key = %key, error = %e, "failed to seed JWT secret"),
    }
}

/// Seed one row with an explicit `sensitive` column, bypassing
/// [`NewVariable::into_row`].
///
/// TEST FIXTURE, and it lives here because this module owns the table — a
/// block file naming `TABLE` to build the same row trips `tests/repo_door.rs`,
/// correctly.
///
/// It exists because no supported API can produce this row any more: the
/// creation funnel raises the flag on the way in, which is the property it
/// exists for. A row an OLDER build left behind is the thing under test for
/// the repair pass and for the admin edit form's masking.
#[cfg(test)]
pub(crate) async fn seed_row_with_flag(
    ctx: &crate::test_support::TestContext,
    key: &str,
    value: &str,
    sensitive: i64,
) {
    let ctx = &ctx.fixture();
    let now = crate::util::now_rfc3339();
    let mut data = VariableRow {
        id: format!("var_{}", uuid::Uuid::new_v4()),
        key: key.to_string(),
        value: value.to_string(),
        name: String::new(),
        description: String::new(),
        warning: String::new(),
        sensitive: false,
        block: block_for_key(key),
        updated_by: String::new(),
        created_at: now.clone(),
        updated_at: now,
    }
    .to_data();
    data.insert("sensitive".to_string(), serde_json::json!(sensitive));
    db::create(ctx, TABLE, data)
        .await
        .expect("seed a variables row");
}

/// Seed one row carrying an explicit `updated_by` marker.
///
/// TEST FIXTURE, here for the same reason as [`seed_row_with_flag`]: this
/// module owns the table, and a block file naming `TABLE` to build the row
/// itself trips `tests/repo_door.rs`.
///
/// It exists because the two markers come from two places no test can reach
/// together — an admin surface stamps an id or
/// [`crate::features::USER_EDITED_SENTINEL`], and only the boot path's upgrade
/// transition writes [`PRE_UPGRADE_SENTINEL`] — so a surface that has to
/// distinguish them needs a fixture that can stage either.
#[cfg(test)]
pub(crate) async fn seed_row_with_owner(
    ctx: &crate::test_support::TestContext,
    key: &str,
    value: &str,
    updated_by: &str,
) {
    insert(
        &ctx.fixture(),
        NewVariable {
            key: key.to_string(),
            value: value.to_string(),
            name: String::new(),
            description: String::new(),
            warning: String::new(),
            sensitive: false,
            updated_by: updated_by.to_string(),
            block: block_for_key(key),
        },
    )
    .await
    .expect("seed a variables row");
}

/// Read every row into a key→value map. A row that does not decode (an empty
/// `key`) is skipped and warned about as corruption rather than silently
/// dropped.
pub async fn load_all(db: &Arc<dyn DatabaseService>) -> Result<HashMap<String, String>, String> {
    Ok(load_rows(db)
        .await?
        .into_iter()
        .map(|loaded| (loaded.row.key, loaded.row.value))
        .collect())
}

/// Every decodable row. The shared body of [`load_all`] and the boot-time
/// sensitive-flag repair, which needs the whole row rather than the value.
async fn load_rows(db: &Arc<dyn DatabaseService>) -> Result<Vec<LoadedRow>, String> {
    let opts = ListOptions {
        offset: 0,
        limit: Some(100_000),
        skip_count: true,
        ..Default::default()
    };
    let listed = db
        .list(TABLE, &opts)
        .await
        .map_err(|e| format!("load variables from {TABLE}: {e}"))?;
    let mut rows = Vec::with_capacity(listed.records.len());
    for record in listed.records {
        match VariableRow::from_record(&record.id, &record.data) {
            Ok(row) => {
                // Compared against the canonical value directly. A shape test
                // on `Value::Number` was false for every backend that returns
                // this INTEGER column as a string or a float — the shapes this
                // change set exists because they occur — so the repair rewrote
                // every required-sensitive row on every boot, each one bumping
                // the config-write generation and invalidating warm snapshots.
                let flag_is_canonical_one =
                    record.data.get("sensitive") == Some(&serde_json::json!(1));
                rows.push(LoadedRow {
                    row,
                    flag_is_canonical_one,
                });
            }
            Err(e) => tracing::warn!(error = %e, "variables table contains an undecodable row"),
        }
    }
    Ok(rows)
}

/// A decoded row plus how its `sensitive` column was actually spelled on disk.
struct LoadedRow {
    row: VariableRow,
    /// `true` only when the column came back as exactly the integer `1`.
    /// Any other spelling a backend or bundle can produce — a bool, a string,
    /// a float, `2` — is readable (see [`crate::util::flag_is_set`]) but not
    /// what the schema declares, so the repair pass rewrites it.
    flag_is_canonical_one: bool,
}

/// Reconcile every stored row's `sensitive` flag with what its key requires,
/// reading the table to find the ones that disagree.
///
/// Raises a flag the declaration or the `_SECRET`/`_KEY` suffix calls for, and
/// never clears one. See the RAISE ONLY note in the body: lowering was the
/// wrong trade — a mis-flagged row is a cosmetic annoyance, an unflagged
/// credential is a leak — and an admin flagging an ad hoc row by hand is a
/// decision this code has no standing to reverse either.
///
/// The write paths settle this at creation now ([`NewVariable::into_row`]), but
/// a row an EARLIER build wrote is already in the database with the flag clear,
/// and nothing else will ever revisit it: [`seed_and_load`] only touches keys
/// the environment still exports, and `admin::settings::seed_defaults`'
/// existing-row branch short-circuits on the stamped declared-vars hash for the
/// life of a release. The likeliest holder of such a row is a bootstrap
/// credential — set once from `.env` and then removed from it — so "the next
/// write fixes it" would mean "never" for exactly the rows that matter most.
///
/// ## Every target has to call this, and they do not share one entry point
///
/// Native and the browser get it inside [`seed_and_load`], which reuses rows it
/// has already fetched. **Cloudflare never calls `seed_and_load` at all** — its
/// `CfDeployBootHooks::seed_and_load` runs [`seed_auto_generated`] and the
/// `block_settings` seed and nothing else — so the hosted target calls this
/// directly from that same deploy hook, at the cost of the one list this does
/// for itself.
///
/// It belongs in the DEPLOY hook specifically, not the request-path one:
/// `CfRequestBootHooks` is documented as physically write-free, and a write
/// from it self-invalidates the fleet's config version and races concurrent
/// isolates on insert. A repair at `/_deploy/init` time is both sufficient (the
/// rows are legacy, not newly created) and the only safe slot.
///
/// Idempotent, and writes only when a row is actually wrong, so a healthy
/// deployment pays one list and no writes.
pub async fn repair_sensitive_flags(db: &Arc<dyn DatabaseService>) {
    match load_rows(db).await {
        Ok(rows) => repair_sensitive_flags_in(db, &rows).await,
        Err(e) => tracing::warn!(
            error = %e,
            "could not read the variables table to repair `sensitive` flags"
        ),
    }
}

/// Whether [`seed_and_load`]'s one-time env-precedence transition has already
/// run on this database.
///
/// A single indexed lookup on [`ENV_PRECEDENCE_TRANSITION_KEY`], done once per
/// boot rather than per key.
///
/// A read failure answers "yes". Not knowing is not a licence to run a one-time
/// pass a second time — that would re-pin every key an operator had reset,
/// which is the one thing the gate exists to prevent. A database this call
/// cannot read is one the per-key reads below cannot read either, so nothing is
/// overwritten in the meantime.
async fn transition_has_run(db: &Arc<dyn DatabaseService>) -> bool {
    match find_by_key(db, ENV_PRECEDENCE_TRANSITION_KEY).await {
        Ok(row) => row.is_some(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "could not check whether the env-precedence upgrade transition has run; \
                 treating it as done rather than risk pinning keys an operator has reset"
            );
            true
        }
    }
}

/// Record that the one-time env-precedence transition has run.
///
/// [`seed_if_absent`], so a concurrent second boot cannot duplicate the row, and
/// a failure is logged rather than fatal: the cost of not recording it is that
/// the next boot repeats a pass which is, by construction, a no-op for every key
/// it already pinned.
async fn record_transition(db: &Arc<dyn DatabaseService>) {
    if let Err(e) = seed_if_absent(
        db,
        ENV_PRECEDENCE_TRANSITION_KEY,
        "done",
        "Env precedence transition",
        "Records that the one-time env-precedence upgrade transition has run. Deleting this \
         row makes the next boot re-run it.",
        false,
    )
    .await
    {
        tracing::warn!(
            error = %e,
            "could not record that the env-precedence transition ran; the next boot will \
             repeat it"
        );
    }
}

/// Keep `row` against a disagreeing export, because it predates edit tracking.
///
/// Stamped [`PRE_UPGRADE_SENTINEL`] rather than
/// [`crate::features::USER_EDITED_SENTINEL`]: this is not the claim that an
/// admin edited the key, it is the claim that nobody can tell which, and the log
/// line and the Variables page both have to be able to say so.
///
/// Reached only for a key the environment exports on this boot, with a
/// non-empty value that DIFFERS from the stored one — see "Exactly what the
/// transition promises" on [`seed_and_load`]. A pre-upgrade UI change that
/// misses any of those (not in the batch, exported empty, or exported with the
/// value already stored) is not protected by this, and a later export for it
/// will take effect.
async fn pin_at_upgrade(db: &Arc<dyn DatabaseService>, row: &VariableRow) -> bool {
    let patch = VariablePatch {
        updated_by: Some(PRE_UPGRADE_SENTINEL.to_string()),
        ..Default::default()
    };
    match db.update(TABLE, &row.id, patch.to_update_data()).await {
        Ok(_) => {
            crate::config_generation::note_config_write();
            tracing::warn!(
                key = %row.key,
                "this environment variable has NO EFFECT from now on — the stored value \
                 differs from it and predates edit tracking, so this build cannot tell \
                 whether an admin set it, and keeps it{}.",
                secrecy_note(&row.key),
            );
            true
        }
        Err(e) => {
            tracing::warn!(
                key = %row.key,
                error = %e,
                "failed to pin a config key during the one-time env-precedence transition; \
                 the process environment may overwrite it on the next boot"
            );
            false
        }
    }
}

/// One summary line for a boot that found inert exports, carrying the recovery
/// advice ONCE.
///
/// The advice used to be appended to every per-key line. At ten pinned keys
/// that is ten copies of ~170 identical characters per boot, and the key names —
/// the only part that differs, and the only part an operator has to act on —
/// are what gets buried. The per-key lines are short and carry the structured
/// `key` field; this says what to do about all of them.
///
/// The page control it names is gated on two independent things, and this line
/// is safe to print because of the first one only:
///
/// - PER DEPLOYMENT, [`deployment_seeds_from_process_env`] — and that is the
///   same condition under which this line can be emitted at all, since it is
///   reached only from [`seed_and_load`]'s env loop, whose body does not run
///   when `env_vars` is empty, and it is empty on exactly the targets that hide
///   the control.
/// - PER KEY, [`key_can_be_seeded_from_env`]. This line does not check it, and
///   on the native path it does not have to: every key the loop saw came
///   through `impresspress_server::filter_to_declared_keys`, which is the
///   predicate that mirrors. A caller assembling its own batch (the case this
///   module's runtime-owned guard is documented for, and what its unit tests
///   do) can get a count here for a key the page would offer no control for — a
///   summary that over-counts by one per such key on a path production does
///   not take, which is not worth a second per-key pass to avoid.
///
/// `upgrade_pins` is how many of those `count` keys are held by a
/// [`Pin::PreUpgrade`] rather than a [`Pin::AdminEdit`], and it is passed
/// separately so the extra sentence about the BULK control is printed only when
/// that control will be on the page. That control needs one more thing than
/// this line does — at least one upgrade pin — and the [`Pin::AdminEdit`] half
/// of `count` does not supply it, so a deployment whose only inert exports are
/// admin edits would be told to press a button that is not there.
///
/// "Only when" carries the same per-key caveat as `count` above, and for the
/// same reason: this number does not consult [`key_can_be_seeded_from_env`],
/// while [`PinnedAtUpgrade::of`] does. A caller assembling its own batch with an
/// undeclared key can therefore reach `upgrade_pins >= 1` with no button
/// rendering. Not reachable in production — a runtime-owned key `continue`s
/// before it is counted, and on native `filter_to_declared_keys` has already
/// removed every other undeclared key — and not worth a second pass over the
/// table to tighten.
///
/// The sentence names NO NUMBER, and that is not squeamishness: `upgrade_pins`
/// and the page's count are taken over different populations and legitimately
/// disagree. This one counts upgrade pins whose export is still present AND
/// still differing, because it is derived from the env loop that has just
/// walked that batch; the button counts every [`Pin::PreUpgrade`] row in the
/// table. Remove one export from a deployment with two pinned keys and this
/// would say "1" while the page says "2" and the action releases 2 — and an
/// operator reading both has no way to tell which is lying. Deriving the honest
/// number here would mean a second pass over the whole table on every boot that
/// finds an inert export, to put a figure in a log line that is pointing at a
/// page which shows the real one.
fn warn_how_to_undo_a_pin(count: usize, upgrade_pins: usize) {
    // Named separately from the message so the sentence reads as one thing an
    // operator can act on rather than a conditional clause.
    let bulk = if upgrade_pins > 0 {
        ". At least one of them was pinned by this deployment's one-time upgrade boot; \
         \"Reset all keys pinned at upgrade\" on that page releases every such key at once, \
         and never touches a key an admin edited"
    } else {
        ""
    };
    tracing::warn!(
        // `inert_exports` and nothing else. `upgrade_pins` decides WHETHER the
        // sentence below is printed but is never published, for the same reason
        // the sentence names no figure: it counts this batch, the page counts
        // the table, and a field is not a lesser claim than message text — an
        // operator grepping `upgrade_pins=1` beside a button reading "(2)" is
        // in exactly the position the wording change exists to avoid.
        inert_exports = count,
        // Deliberately NOT the per-key lines' "NO EFFECT" wording: that phrase
        // is how an operator greps for the keys to act on, and how this
        // module's tests count them, so the summary must not inflate it.
        "{count} environment variable(s) named above are set but are not in effect, \
         because a stored value takes precedence for those keys. To hand one back to the \
         environment, use \"Reset to environment\" on the admin Variables page (or POST \
         /b/admin/api/settings/{{key}}/reset-to-environment) and restart{bulk}"
    );
}

/// The steady-state WARN for an export that a pinned row outranks.
///
/// Only ever called when the two values actually DIFFER — a pinned row already
/// holding what the export says is not a conflict, and warning about it on every
/// boot is how an operator learns to ignore the line that matters.
fn warn_export_is_inert(key: &str, pin: Pin) {
    let reason = match pin {
        Pin::AdminEdit => "an admin edited this setting through the admin UI",
        Pin::PreUpgrade => {
            "this key was pinned at upgrade: its stored value predates edit tracking and \
             differed from the environment, so no admin edit can be proved either way"
        }
    };
    tracing::warn!(
        key = %key,
        "this environment variable is set but has NO EFFECT — {reason}; the stored value \
         is what boots{}.",
        secrecy_note(key),
    );
}

/// The clause that stands in for "the two values are X and Y" on a key whose
/// value is a credential.
///
/// No line here prints either value for any key, secret or not: a boot log is
/// harder to redact than a table, and the operator can read the stored value on
/// the Variables page. For a credential the page masks it too, so this says
/// where to compare instead — the place that issued it.
fn secrecy_note(key: &str) -> &'static str {
    if crate::config_vars::is_sensitive_for_storage(key) {
        ". Neither value is shown, because this key holds a credential — compare the \
         stored value against the one issued where you manage that credential"
    } else {
        ""
    }
}

/// [`repair_sensitive_flags`] over rows the caller already has, so
/// [`seed_and_load`] does not list the table twice.
async fn repair_sensitive_flags_in(db: &Arc<dyn DatabaseService>, rows: &[LoadedRow]) {
    for loaded in rows {
        let row = &loaded.row;
        let required = crate::config_vars::is_sensitive_for_storage(&row.key);

        // RAISE ONLY. The pass used to also clear the flag on a declared key
        // whose declaration did not call for one, so a mis-flag was
        // recoverable. That was the wrong trade and is gone: the Add Variable
        // modal ticks Sensitive by default, and a declared var with an empty
        // default has no row until an admin makes one — so
        // `WAFER_RUN_SHARED__EMBEDDED_SCRIPTS`, which carries operator-supplied
        // script text that routinely embeds an analytics or API key, could be
        // created flagged and then silently unflagged by the next boot,
        // rendered in clear on the Variables page and made exportable into
        // another deployment's seed bundle. Permanent masking is an annoyance;
        // auto-unmasking a secret-bearing value and then exporting it is a
        // leak, and the two do not weigh the same. A mis-flag is now fixed by
        // the admin who made it, through the edit form's Sensitive control.
        //
        // Rewrite when the flag is not the canonical `1`: a row whose column
        // holds `2`, `true` or `"true"` reads as flagged, so a `row.sensitive`
        // skip would leave it in a shape the schema does not declare — and,
        // before the readers were reconciled, one the settings API and the KV
        // cache both read as UNflagged.
        if !required || loaded.flag_is_canonical_one {
            continue;
        }

        // Whether the clear flag ever exposed anything, which decides how
        // loudly this is reported. Two kinds of row reach here without having
        // been readable, and calling either a breach is a false alarm with its
        // own cost:
        //
        //   * a `*_SECRET`/`*_KEY` row. The key half of `util::is_sensitive_key`
        //     has always covered the suffix, so the read path and the edge
        //     cache (`cache_key::row_is_sensitive`) masked and excluded it all
        //     along whatever its flag said — telling an operator their Stripe
        //     key "was being served unmasked" is simply untrue.
        //   * a row whose column holds `2`, `true`, `"true"` or `1.0`. That is
        //     the OTHER reason this branch is reached (`!flag_is_canonical_one`
        //     rather than a clear flag), and `VariableRow::from_record` decodes
        //     it through `flag_is_set`, so `row.sensitive` is true and every
        //     reader masked it on the flag alone. What is wrong with such a row
        //     is its SHAPE, which is what the rewrite below fixes — nothing was
        //     published, so nothing needs rotating.
        //
        // What is left is a DECLARED `Password`/`auto_generate` key whose
        // column really is clear. Today's build masks it anyway — the key half
        // of `is_sensitive_key` asks the declaration now — but the build that
        // wrote this row did not, and served it in the clear for the row's
        // whole life up to this upgrade. That is a credential to rotate, and
        // this is the one moment an operator gets told.
        let was_exposed = !row.sensitive && !crate::config_vars::has_sensitive_suffix(&row.key);
        let patch = VariablePatch {
            sensitive: Some(true),
            ..Default::default()
        };
        match db.update(TABLE, &row.id, patch.to_update_data()).await {
            Ok(_) => {
                crate::config_generation::note_config_write();
                if was_exposed {
                    tracing::warn!(
                        key = %row.key,
                        "repaired the stored `sensitive` flag for this config key; it was \
                         written before the flag was derived from the variable's declaration, \
                         so an EARLIER BUILD of this deployment served its value unmasked — \
                         treat the credential as exposed and rotate it"
                    );
                } else {
                    tracing::info!(
                        key = %row.key,
                        "tidied the stored `sensitive` flag for this config key; it was \
                         already reading as sensitive (its `_SECRET`/`_KEY` name, or a \
                         non-canonical but truthy column), so nothing was exposed"
                    );
                }
            }
            Err(e) => tracing::warn!(
                key = %row.key,
                error = %e,
                "failed to repair a config key's `sensitive` flag; the value stays masked \
                 (`util::is_sensitive_key` reads the declaration, not just this column), \
                 but the row keeps a shape the schema does not declare"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime flavour: over `Context`, under WRAP.
// ---------------------------------------------------------------------------

/// Every row, by key. A row that does not decode is skipped and warned
/// about, the same policy [`load_all`] applies at boot.
///
/// The order is the query's, not the table's: without one, SQL returns rows
/// in whatever order the backend's plan produces, and a Postgres update
/// writes a new row version that a scan can return somewhere else — the All
/// Variables tab could reshuffle after an edit.
pub async fn list_all(ctx: &dyn Context) -> Result<Vec<VariableRow>, WaferError> {
    let records = db_read::list_bounded_sorted(
        ctx,
        TABLE,
        vec![],
        vec![SortField {
            field: "key".to_string(),
            desc: false,
        }],
        Bound::OnePer("declared config key"),
    )
    .await?;
    Ok(records
        .iter()
        .filter_map(|r| match VariableRow::from_record(&r.id, &r.data) {
            Ok(row) => Some(row),
            Err(e) => {
                tracing::warn!(error = %e, "variables table contains an undecodable row");
                None
            }
        })
        .collect())
}

/// The row for `key`, if any.
pub async fn get_by_key(ctx: &dyn Context, key: &str) -> Result<Option<VariableRow>, WaferError> {
    match db::get_by_field(ctx, TABLE, "key", Value::String(key.to_string())).await {
        Ok(rec) => VariableRow::from_record(&rec.id, &rec.data)
            .map(Some)
            .map_err(decode_error),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The row as it now stands, for a write that has **already committed**.
///
/// Past a successful `db::create` / `db::update`, nothing may report a failure.
/// The `?` on the write is the write failing; everything after it is this
/// module reading back what it just wrote, and the two must not reach a caller
/// as the same error — each write path had its own way of going wrong when they
/// did:
///
/// * `admin::ops::create_variable` classifies a failed insert by RE-READING the
///   key, so a decode failure came back as "that key is already taken" — the
///   row this very request had created. The admin was told the key existed, the
///   `variable.create` audit row was skipped, and the untracked row made every
///   retry conflict forever.
/// * `admin::ops::update_variable` returns on the error BEFORE its `audit_log`
///   call, so an edit that committed went unrecorded — and a missing audit row
///   cannot be told apart from the change never having been made.
///
/// So the echo is MERGED rather than trusted or distrusted wholesale: start
/// from the row as it stood before the write (`as_read`), lay the columns this
/// call WROTE over it, then lay whatever the backend ECHOED over that. Each
/// layer is more authoritative than the last, and a backend that echoes
/// nothing, a bare `key`, or every column all reach the right answer through
/// the same expression.
///
/// Merging unconditionally — rather than only when the echo fails to decode —
/// is what covers the middle case. [`VariableRow::from_record`] insists on
/// `key` and nothing else, so an echo carrying `key` and little else decodes
/// *successfully* into a row with an empty `name`, no `created_at` and
/// `sensitive: false`. On the update path that is user-visible: it reports a
/// variable that is still sensitive as no longer sensitive.
///
/// `id` is the caller's to decide, because the two writes learn it differently:
/// an update addressed a row the caller already identified, so the echo has no
/// say in which row it is, while a create may legitimately be told an id the
/// backend minted.
fn stored_row(
    id: &str,
    as_read: &VariableRow,
    written: HashMap<String, Value>,
    echoed: HashMap<String, Value>,
) -> VariableRow {
    let mut merged = as_read.to_data();
    merged.extend(written);
    merged.extend(echoed);
    VariableRow::from_record(id, &merged).unwrap_or_else(|e| {
        // Unreachable: `merged` starts from a row that already carries the
        // `key` `from_record` insists on. `as_read` under the caller's id is
        // the honest answer if it somehow is not.
        tracing::warn!(
            error = %e,
            id = %id,
            "variables write landed but the merged row did not decode; \
             reporting the row as written",
        );
        VariableRow {
            id: id.to_string(),
            ..as_read.clone()
        }
    })
}

/// Insert a new row and return it as stored.
pub async fn insert(ctx: &dyn Context, new: NewVariable) -> Result<VariableRow, WaferError> {
    let row = new.into_row();
    let written = row.to_data();
    let rec = db::create(ctx, TABLE, written.clone()).await?;
    crate::config_generation::note_config_write();
    // The write has committed; see [`stored_row`] for why nothing past here
    // reports a failure. A create may be told an id the backend minted, but an
    // echo carrying NO id is not that: taking `rec.id` regardless would publish
    // an empty id for a row stored under the `var_<uuid>`
    // [`NewVariable::into_row`] synthesised and `to_data` wrote.
    let id = if rec.id.is_empty() {
        row.id.clone()
    } else {
        rec.id
    };
    Ok(stored_row(&id, &row, written, rec.data))
}

/// Update the row for `key`, or create it when absent, and return the row as
/// stored.
///
/// The get-then-write shape rather than the atomic `db::upsert`: the two
/// writes it issues (`update` | `create`) are the ones the Cloudflare KV row
/// cache invalidates, and that cache *refuses* the atomic upsert on this
/// table (`KvCachedD1DatabaseService::upsert`). The create branch derives
/// `block` from the key and synthesises the id and timestamps through the
/// same codec [`insert`] uses.
pub async fn upsert_by_key(
    ctx: &dyn Context,
    key: &str,
    patch: VariablePatch,
) -> Result<VariableRow, WaferError> {
    match get_by_key(ctx, key).await? {
        Some(existing) => {
            let written = patch.to_update_data();
            let rec = db::update(ctx, TABLE, &existing.id, written.clone()).await?;
            crate::config_generation::note_config_write();
            // The change has committed; see [`stored_row`] for why nothing past
            // here reports a failure. Deriving the merge from `written` rather
            // than restating the patch's field list keeps one description of
            // what an update changes, and `updated_at` comes along because
            // `to_update_data` mints it there. The id is `existing.id`: this
            // update named the row itself.
            Ok(stored_row(&existing.id, &existing, written, rec.data))
        }
        // `insert` notes the write itself.
        None => insert(ctx, patch.into_new(key)).await,
    }
}

/// Delete the row with `id`. `NotFound` when there is none.
pub async fn delete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, TABLE, id).await?;
    crate::config_generation::note_config_write();
    Ok(())
}

/// Delete the row for `key`, if any. Deleting an absent key affects nothing
/// and is not an error, which is what makes this callable unconditionally.
pub async fn delete_by_key(ctx: &dyn Context, key: &str) -> Result<(), WaferError> {
    db::delete_by_filters(ctx, TABLE, vec![key_filter(key)]).await?;
    crate::config_generation::note_config_write();
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config_vars::APP_NAME_KEY, test_support::TestContext};

    /// **A taken key is the write's own answer, on the database these tests
    /// run on.** A second variable with a taken key is refused by the UNIQUE
    /// index, and the refusal reaches the caller as `AlreadyExists` — the
    /// `DatabaseService` contract the 409 rests on, with nothing re-read.
    #[tokio::test]
    async fn a_taken_key_is_already_exists_and_a_conflict() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let new = || NewVariable {
            key: "SITE__TAKEN".into(),
            value: "one".into(),
            name: "SITE__TAKEN".into(),
            description: String::new(),
            warning: String::new(),
            sensitive: false,
            updated_by: String::new(),
            block: None,
        };
        insert(&ctx, new()).await.expect("the first one");
        let refused = insert(&ctx, new()).await.expect_err("the key is taken");
        assert_eq!(
            refused.code,
            wafer_run::ErrorCode::AlreadyExists,
            "{refused:?}"
        );

        let out = crate::blocks::crud::taken_key_or_db_error(
            refused,
            crate::blocks::crud::TakenKey::new("variable", "key", "SITE__TAKEN"),
            "Database error",
        );
        assert_eq!(crate::test_support::output_http_status(out).await, 409);
    }

    /// A block-scoped key no block declares, which the fixture rows are
    /// stored under.
    const FROM_KEY: &str = "IMPRESSPRESS__EMAIL__FROM";

    fn new_var(key: &str) -> NewVariable {
        NewVariable {
            key: key.to_string(),
            value: "noreply@example.com".to_string(),
            name: "From address".to_string(),
            description: "Sender of every outbound email".to_string(),
            warning: "Changing this breaks DKIM".to_string(),
            sensitive: true,
            updated_by: "admin_1".to_string(),
            block: block_for_key(key),
        }
    }

    /// A backend that acknowledges the create without naming the row leaves the
    /// id [`NewVariable::into_row`] minted standing.
    ///
    /// Taking `rec.id` regardless published an EMPTY id — and
    /// `admin::settings::handle_create` echoes that id straight to the client,
    /// while the row is stored under the `var_<uuid>` that was actually
    /// written. A create may legitimately be told an id the backend minted;
    /// being told nothing is not that.
    #[tokio::test]
    async fn a_create_whose_echo_carries_no_id_keeps_the_id_that_was_written() {
        let ctx = crate::test_support::EcholessWriteContext::new(
            TestContext::with_admin()
                .await
                .running_as(crate::blocks::admin::ADMIN_BLOCK_ID),
        )
        .without_the_id();

        let row = insert(&ctx, new_var(FROM_KEY))
            .await
            .expect("the write lands; only the echo is thin");
        assert!(
            row.id.starts_with("var_"),
            "the caller must get the id the row is stored under, not `{}`",
            row.id,
        );
        assert_eq!(
            get_by_key(&ctx, FROM_KEY)
                .await
                .expect("read back")
                .expect("stored")
                .id,
            row.id,
            "and it must be the id the table really holds",
        );
    }

    /// A PARTIAL echo — enough to decode, not enough to be the row — does not
    /// silently default the columns it left out.
    ///
    /// `from_record` insists on `key` and nothing else, so this record decodes
    /// successfully. A fallback that only ran when decoding FAILED never saw
    /// this case, and the caller got `sensitive: false`, an empty `name` and no
    /// `created_at` for a row that has all three.
    #[tokio::test]
    async fn a_partial_echo_does_not_default_the_columns_it_left_out() {
        let ctx = crate::test_support::EcholessWriteContext::new(
            TestContext::with_admin()
                .await
                .running_as(crate::blocks::admin::ADMIN_BLOCK_ID),
        )
        .keeping_columns(&["key", "value"]);

        let row = insert(&ctx, new_var(FROM_KEY))
            .await
            .expect("the write lands; only the echo is partial");
        assert_eq!(row.key, FROM_KEY);
        assert_eq!(row.value, "noreply@example.com", "the echoed column");
        assert!(row.sensitive, "and every column the echo omitted");
        assert_eq!(row.name, "From address");
        assert_eq!(row.description, "Sender of every outbound email");
        assert_eq!(row.warning, "Changing this breaks DKIM");
        assert_eq!(row.updated_by, "admin_1");
        assert!(!row.created_at.is_empty());
        assert_eq!(row.block, Some("IMPRESSPRESS__EMAIL".to_string()));
    }

    /// A write the database REFUSES still reaches the caller as the refusal it
    /// was, through the echo-thinning wrapper the two tests above it use.
    ///
    /// This pins the test wrapper rather than this module, and it is worth
    /// pinning here, where the table constant lives: the wrapper used to
    /// substitute an `Internal` for whatever the inner context answered, so a
    /// test written against a denied write would have asserted a 403 and
    /// quietly received a 500 — proving nothing while looking like it proved
    /// something.
    #[tokio::test]
    async fn a_refused_write_keeps_its_code_through_the_echo_thinning_wrapper() {
        let denied = crate::test_support::FailingDbOpContext::failing_with(
            TestContext::with_admin()
                .await
                .running_as(crate::blocks::admin::ADMIN_BLOCK_ID),
            vec![("database.create", TABLE)],
            WaferError::new(ErrorCode::PermissionDenied, "denied"),
        );
        let ctx = crate::test_support::EcholessWriteContext::new(denied);

        let error = insert(&ctx, new_var(FROM_KEY))
            .await
            .expect_err("the write is refused, so the insert must fail");
        assert_eq!(error.code, ErrorCode::PermissionDenied);
    }

    /// The codec is the whole point: every column written by `to_data` comes
    /// back through `from_record` unchanged, on the real admin schema, with
    /// `sensitive` a bool and `block` derived from the key the way migration
    /// 002 backfills it.
    #[tokio::test]
    async fn insert_and_get_by_key_round_trip_every_column() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let inserted = insert(&ctx, new_var(FROM_KEY)).await.expect("insert");
        assert!(inserted.id.starts_with("var_"), "{}", inserted.id);
        assert_eq!(inserted.key, FROM_KEY);
        assert_eq!(inserted.value, "noreply@example.com");
        assert_eq!(inserted.name, "From address");
        assert_eq!(inserted.description, "Sender of every outbound email");
        assert_eq!(inserted.warning, "Changing this breaks DKIM");
        assert!(inserted.sensitive);
        assert_eq!(inserted.block.as_deref(), Some("IMPRESSPRESS__EMAIL"));
        assert_eq!(inserted.updated_by, "admin_1");
        assert!(!inserted.created_at.is_empty());
        assert_eq!(inserted.created_at, inserted.updated_at);

        let read = get_by_key(&ctx, FROM_KEY)
            .await
            .expect("get")
            .expect("the row exists");
        assert_eq!(read, inserted);

        let again = VariableRow::from_record(&read.id, &read.to_data()).expect("decode");
        assert_eq!(again, read);
    }

    /// A shared or ad hoc key has no block prefix: the column stays NULL,
    /// which is what `D1ConfigSource` relies on to keep it out of every
    /// block's config.
    #[tokio::test]
    async fn a_key_without_a_block_prefix_keeps_the_column_null() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let row = insert(&ctx, new_var(APP_NAME_KEY)).await.expect("insert");
        assert_eq!(row.block, None);
        assert!(
            !row.to_data().contains_key("block"),
            "an absent block must not be written as an empty string"
        );
    }

    #[tokio::test]
    async fn get_by_key_on_an_absent_key_is_none() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        assert_eq!(get_by_key(&ctx, "NOPE").await.expect("get"), None);
    }

    #[tokio::test]
    async fn upsert_by_key_creates_then_updates_one_row() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let created = upsert_by_key(
            &ctx,
            "SITE_TAGLINE",
            VariablePatch {
                value: Some("Hello".to_string()),
                description: Some("a fresh key".to_string()),
                updated_by: Some("admin_1".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("create branch");
        assert_eq!(created.value, "Hello");
        assert_eq!(created.description, "a fresh key");
        assert_eq!(
            created.name, "",
            "unset patch fields take the column default"
        );
        // `sensitive` is the one field that does NOT take the column default:
        // `SITE_TAGLINE` is an ad hoc key no `ConfigVar` declares, so
        // `into_new` protects it rather than publishing it. Matches what a
        // POST of the same key through `handle_create` (absent means
        // sensitive) has always produced.
        assert!(
            created.sensitive,
            "an undeclared key created without an explicit flag is protected"
        );

        let updated = upsert_by_key(
            &ctx,
            "SITE_TAGLINE",
            VariablePatch {
                value: Some("Goodbye".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("update branch");
        assert_eq!(
            updated.id, created.id,
            "the update must land on the same row"
        );
        assert_eq!(updated.value, "Goodbye");
        assert_eq!(
            updated.description, "a fresh key",
            "a field the patch leaves unset is preserved"
        );
        assert_eq!(list_all(&ctx).await.expect("list").len(), 1);
    }

    /// The All Variables tab and the settings API show the rows in the order
    /// this returns, so it is the key's, not the order they were written in.
    #[tokio::test]
    async fn list_all_is_in_key_order_whatever_order_the_rows_were_written_in() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        for key in ["SITE_ZEBRA", "SITE_APPLE", "SITE_MANGO"] {
            insert(&ctx, new_var(key)).await.expect("insert");
        }
        let keys: Vec<String> = list_all(&ctx)
            .await
            .expect("list")
            .into_iter()
            .map(|row| row.key)
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
        assert!(keys.contains(&"SITE_ZEBRA".to_string()), "{keys:?}");
    }

    #[tokio::test]
    async fn delete_removes_the_row_and_delete_by_key_tolerates_absence() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let row = insert(&ctx, new_var("SITE_MOTTO")).await.expect("insert");
        delete(&ctx, &row.id).await.expect("delete");
        assert_eq!(get_by_key(&ctx, "SITE_MOTTO").await.expect("get"), None);
        delete_by_key(&ctx, "SITE_MOTTO")
            .await
            .expect("deleting an absent key is not an error");
    }

    #[test]
    fn a_record_without_a_key_does_not_decode() {
        let mut data = HashMap::new();
        data.insert("value".to_string(), serde_json::json!("x"));
        let err = VariableRow::from_record("var_1", &data).expect_err("no key");
        assert!(err.contains(TABLE) && err.contains("var_1"), "{err}");
    }

    /// `sensitive` arrives as an integer from SQLite, a bool from Postgres
    /// and a string from a hand-built fixture; the codec reads all three.
    #[test]
    fn sensitive_decodes_from_every_backend_shape() {
        for (shape, want) in [
            (serde_json::json!(1), true),
            (serde_json::json!(0), false),
            (serde_json::json!(true), true),
            (serde_json::json!("1"), true),
            (serde_json::json!("false"), false),
        ] {
            let mut data = HashMap::new();
            data.insert("key".to_string(), serde_json::json!("K"));
            data.insert("sensitive".to_string(), shape.clone());
            let row = VariableRow::from_record("var_1", &data).expect("decode");
            assert_eq!(row.sensitive, want, "{shape}");
        }
    }
}

/// The boot flavour, over [`DatabaseService`]: the tests `boot.rs` carried
/// for `set_variable`, against the moved names.
#[cfg(test)]
mod boot_tests {
    use super::*;
    use crate::{
        blocks::email::MAILGUN_API_KEY,
        config_vars::{
            ALLOW_SIGNUP_KEY, APP_NAME_KEY, AUTH_HEADLINE_KEY, EMBEDDED_SCRIPTS_KEY,
            ENABLE_OAUTH_KEY, FAVICON_URL_KEY, HAS_LANDING_PAGE_KEY, PRIMARY_COLOR_KEY,
        },
    };

    /// A `DatabaseService` with the admin schema applied through the
    /// pre-wafer DDL runner (`migration_helper::apply_ddl_via_service` +
    /// `blocks::admin::migrations::ddl_files`), the migration-file-runner
    /// exception to the no-raw-SQL rule, so the row shape under test is the
    /// one production writes.
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

    async fn value_of(db: &Arc<dyn DatabaseService>, key: &str) -> Option<String> {
        load_all(db).await.expect("load").remove(key)
    }

    /// The bug `set` exists for, stated as a test: a declared default is
    /// already in the table when the platform hook runs, and the platform's
    /// value has to win.
    #[tokio::test]
    async fn a_forced_write_overrides_a_value_seed_if_absent_would_have_kept() {
        let db = migrated_db().await;
        let key = HAS_LANDING_PAGE_KEY;

        // What the admin block's `Init` does with the declared default.
        assert!(
            seed_if_absent(&db, key, "false", "Has Landing Page", "declared", false)
                .await
                .expect("seed the declared default"),
            "the declared default is the first writer"
        );

        // What `seed_if_absent` would do from the platform hook — nothing.
        // This is the assertion that makes the fix load-bearing.
        assert!(
            !seed_if_absent(&db, key, "true", "Has Landing Page", "declared", false)
                .await
                .expect("second seed"),
            "seeding cannot beat a row that is already there"
        );
        assert_eq!(value_of(&db, key).await.as_deref(), Some("false"));

        assert_eq!(
            set(
                &db,
                key,
                "true",
                "Has Landing Page",
                "declared",
                Some(false)
            )
            .await
            .expect("force-set"),
            Wrote::Replaced,
            "the value changed, so the row was written"
        );
        assert_eq!(value_of(&db, key).await.as_deref(), Some("true"));
    }

    /// Callable unconditionally on every boot: the second call writes nothing.
    #[tokio::test]
    async fn re_asserting_the_same_value_is_not_a_write() {
        let db = migrated_db().await;
        let key = HAS_LANDING_PAGE_KEY;
        assert_eq!(
            set(&db, key, "true", "n", "d", Some(false))
                .await
                .expect("create"),
            Wrote::Created
        );
        assert_eq!(
            set(&db, key, "true", "n", "d", Some(false))
                .await
                .expect("re-assert"),
            Wrote::Unchanged,
            "an unchanged value must not be written again"
        );
        assert_eq!(value_of(&db, key).await.as_deref(), Some("true"));
    }

    /// On a key with no row at all it creates one, `block` column included —
    /// the same row shape `seed_if_absent` produces.
    #[tokio::test]
    async fn an_absent_key_is_created_with_its_block_column() {
        let db = migrated_db().await;
        const KEY: &str = "WAFER_RUN__AUTH__PROBE";
        assert_eq!(
            set(&db, KEY, "true", "Probe", "d", Some(false))
                .await
                .expect("create"),
            Wrote::Created
        );

        let row = find_by_key(&db, KEY)
            .await
            .expect("list")
            .expect("exactly one row per key");
        assert_eq!(row.value, "true");
        assert_eq!(row.name, "Probe");
        assert_eq!(row.block.as_deref(), Some("WAFER_RUN__AUTH"));
    }

    /// Seeding a row is a config write, and has to say so.
    ///
    /// `insert_if_absent` is the one create path every boot seeder shares —
    /// `seed_if_absent`, `seed_one_secret`, `seed_jwt_secret`, and the env
    /// seeding in `seed_and_load`. The config block memoizes the variables
    /// table against the config-write generation, so a row seeded after that
    /// snapshot was filled stays invisible unless this bumps it. The browser
    /// seeds after the admin block initializes, through the raw service
    /// (no KV-cached wrapper to bump on its behalf), which is exactly that
    /// case.
    ///
    /// Counted with `config_generation::writes_noted_on_this_thread`, not the
    /// process-wide generation: that counter is shared by the whole process, so
    /// under `cargo test`'s parallel threads it moves for reasons this test
    /// does not control. The per-thread tally counts what THIS test's
    /// `#[tokio::test]` body did, which is the claim being made.
    #[tokio::test]
    async fn seeding_a_row_records_a_config_write_and_a_no_op_does_not() {
        let db = migrated_db().await;
        const KEY: &str = "WAFER_RUN__AUTH__SEED_PROBE";

        let before = crate::config_generation::writes_noted_on_this_thread();
        assert!(seed_if_absent(&db, KEY, "v", "Probe", "d", false)
            .await
            .expect("seed"));
        let after_insert = crate::config_generation::writes_noted_on_this_thread();
        assert_eq!(
            after_insert,
            before + 1,
            "a seeded row must record a config write, so every memoized reader re-reads"
        );

        assert!(!seed_if_absent(&db, KEY, "other", "Probe", "d", false)
            .await
            .expect("re-seed"));
        assert_eq!(
            after_insert,
            crate::config_generation::writes_noted_on_this_thread(),
            "a seed that found an existing row wrote nothing and must not bump"
        );
    }

    /// An operator's edit to the *description* survives a forced value write:
    /// only `value` and `updated_at` are touched.
    #[tokio::test]
    async fn a_forced_write_keeps_the_metadata_the_row_already_had() {
        let db = migrated_db().await;
        let key = HAS_LANDING_PAGE_KEY;
        seed_if_absent(
            &db,
            key,
            "false",
            "Has Landing Page",
            "operator wording",
            false,
        )
        .await
        .expect("seed");

        set(
            &db,
            key,
            "true",
            "A Different Name",
            "different wording",
            Some(false),
        )
        .await
        .expect("force-set");

        let row = find_by_key(&db, key).await.expect("list").expect("row");
        assert_eq!(row.value, "true", "the value is the platform's");
        assert_eq!(
            row.description, "operator wording",
            "metadata describes the variable, not the deployment"
        );
        assert_eq!(row.name, "Has Landing Page");
    }

    /// The audit finding, stated as a test: an operator exports
    /// `WAFER_RUN_SHARED__APP_NAME=Foo` on a deployment whose database already
    /// carries a row for that key (every boot after the first does), and the
    /// value they set has to be the one that boots.
    ///
    /// The upgrade transition is the ONE boot on which that is not true, and
    /// this drives both halves so the cost is written down rather than
    /// discovered. On the upgrade boot the disagreement is kept and named —
    /// this build cannot tell a stale seeded default from an admin's edit, and
    /// reverting the second is worse than deferring the first. From the next
    /// boot on, and for the whole life of the deployment after, the export is
    /// what boots.
    ///
    /// Driven over a `TestContext` so the recovery step goes through the REAL
    /// [`reset_to_environment`], which needs a `Context`. Writing the column by
    /// hand here would have written an empty `updated_by` — exactly what the
    /// control was changed NOT to do — and the test would still have passed,
    /// because the gate row is already recorded by this point.
    #[tokio::test]
    async fn an_env_var_beats_the_row_already_in_the_table() {
        let ctx = crate::test_support::TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let key = APP_NAME_KEY;
        // The declared default a previous boot stored, carrying no provenance.
        seed_row_with_owner(&ctx, key, "Impresspress", "").await;

        // The upgrade boot defers to the row and says so.
        let capture = crate::test_support::MessageCapture::default();
        {
            let _guard = tracing::subscriber::set_default(capture.clone());
            ctx.seed_env_vars(&[(key, "Foo")]).await;
        }
        assert_eq!(
            capture.count_containing("NO EFFECT"),
            1,
            "the one boot that ignores the export has to name the key"
        );

        // The operator clicks "Reset to environment" on the Variables page.
        reset_to_environment(&ctx, key).await.expect("reset");

        ctx.seed_env_vars(&[(key, "Foo")]).await;
        assert_eq!(
            get_by_key(&ctx, key)
                .await
                .expect("get")
                .expect("row")
                .value,
            "Foo",
            "the process environment is the operator's instruction for this boot, and it \
             must be stored so the admin UI shows the value in effect"
        );

        // And it keeps applying, with no further intervention.
        ctx.seed_env_vars(&[(key, "Bar")]).await;
        assert_eq!(
            get_by_key(&ctx, key)
                .await
                .expect("get")
                .expect("row")
                .value,
            "Bar"
        );
    }

    /// An export that fails its key's declared value rule is not seeded: it
    /// is named at ERROR and the stored value, else the declared default,
    /// stays in effect.
    ///
    /// `SESSION_LIFETIME_DAYS=0` (or `abc`, or ` 7`) used to be stored and
    /// read back as the default. The reader now refuses such a value, so
    /// storing it would fail every login from that boot on.
    #[tokio::test]
    async fn an_export_its_reader_refuses_is_not_seeded() {
        let db = migrated_db().await;
        let key = crate::blocks::auth::config::SESSION_LIFETIME_DAYS_KEY;
        let env = |v: &str| [(key.to_string(), v.to_string())];

        for refused in ["0", "abc", " 7", "100000000"] {
            let capture = crate::test_support::MessageCapture::default();
            let vars = {
                let _guard = tracing::subscriber::set_default(capture.clone());
                seed_and_load(&db, &env(refused)).await.expect("boot")
            };
            assert_eq!(vars.get(key), None, "{refused:?} must not be seeded");
            assert_eq!(
                capture.count_containing("refusing to seed this config key"),
                1,
                "the refused export {refused:?} has to be named"
            );
        }

        let vars = seed_and_load(&db, &env("14")).await.expect("boot");
        assert_eq!(vars.get(key).map(String::as_str), Some("14"));

        // A later bad export leaves the good stored value alone.
        let vars = seed_and_load(&db, &env("0")).await.expect("boot");
        assert_eq!(vars.get(key).map(String::as_str), Some("14"));
    }

    /// `usable_env_exports` — the environment native's blocks resolve their
    /// declared keys from, beneath the table — reaches the verdict
    /// `seed_and_load` reaches: an export the seeder refuses is left out, one
    /// it stores is kept.
    #[tokio::test]
    async fn the_block_config_fallback_refuses_what_the_seeder_refuses() {
        let session = crate::blocks::auth::config::SESSION_LIFETIME_DAYS_KEY;
        let internal = "__IMPRESSPRESS_RUNTIME_KIND__";
        let strict = wafer_core::interfaces::database::handler::STRICT_SCHEMA_CONFIG_KEY;
        assert!(crate::config_vars::is_runtime_owned_key(internal));
        let app_env = HashMap::from([
            (session.to_string(), "0".to_string()),
            (internal.to_string(), "server".to_string()),
            (APP_NAME_KEY.to_string(), String::new()),
            (strict.to_string(), "true".to_string()),
        ]);

        let usable = usable_env_exports(&app_env);
        assert_eq!(
            usable,
            HashMap::from([(strict.to_string(), "true".to_string())]),
            "only the export no check refuses may reach a block"
        );

        // The seeder stores none of the refused three, off the same batch.
        let db = migrated_db().await;
        let batch: Vec<(String, String)> = app_env.into_iter().collect();
        let vars = seed_and_load(&db, &batch).await.expect("boot");
        for key in [session, internal, APP_NAME_KEY] {
            assert_eq!(vars.get(key), None, "{key} must not be seeded");
        }

        // A value the rule accepts is kept by both.
        let accepted = HashMap::from([(session.to_string(), "14".to_string())]);
        assert_eq!(usable_env_exports(&accepted), accepted);
    }

    /// THE CONTRACT, both halves. The environment seeds a key no admin has
    /// edited; once an admin has edited it, the row wins permanently.
    #[tokio::test]
    async fn the_environment_seeds_until_an_admin_edits_and_never_after() {
        let db = migrated_db().await;
        let key = APP_NAME_KEY;
        let env = |v: &str| [(key.to_string(), v.to_string())];

        // Boot 1: fresh database, the environment lands. (The original bug.)
        let vars = seed_and_load(&db, &env("First")).await.expect("boot 1");
        assert_eq!(vars.get(key).map(String::as_str), Some("First"));

        // Boot 2: the operator changes the export, no admin has touched it, so
        // the environment still wins. (The second half of the original bug.)
        let vars = seed_and_load(&db, &env("Second")).await.expect("boot 2");
        assert_eq!(vars.get(key).map(String::as_str), Some("Second"));

        // An admin edits the row through an admin surface.
        set_by_admin(&db, key, "AdminChoice", Some(false), "admin_1")
            .await
            .expect("admin edit");
        assert!(is_pinned(
            &find_by_key(&db, key).await.expect("list").expect("row")
        ));

        // Boot 3: the export is now inert, whatever it says.
        let vars = seed_and_load(&db, &env("Third")).await.expect("boot 3");
        assert_eq!(
            vars.get(key).map(String::as_str),
            Some("AdminChoice"),
            "an admin edit outranks the environment from then on"
        );
        assert_eq!(
            find_by_key(&db, key)
                .await
                .expect("list")
                .expect("row")
                .value,
            "AdminChoice",
            "and the stored row is untouched, not just the returned map"
        );
    }

    /// Review finding 2, as a test: a compose file exporting
    /// `ALLOW_SIGNUP=true` must not re-open signup after an admin closed it
    /// during an incident.
    #[tokio::test]
    async fn an_export_cannot_reopen_signup_an_admin_closed() {
        let db = migrated_db().await;
        let key = ALLOW_SIGNUP_KEY;
        let env = [(key.to_string(), "true".to_string())];

        seed_and_load(&db, &env).await.expect("boot with signup on");

        // The incident: an admin turns signup off. This is the settings-form
        // path — `ui::settings_form` → `CONFIG_SET` → `set_by_admin` — which
        // is the surface that owns this toggle.
        set_by_admin(
            &db,
            key,
            "false",
            Some(false),
            crate::features::USER_EDITED_SENTINEL,
        )
        .await
        .expect("admin closes signup");

        // Every later boot: the compose file still says `true`, and signup
        // stays closed.
        for _ in 0..3 {
            let vars = seed_and_load(&db, &env).await.expect("later boot");
            assert_eq!(
                vars.get(key).map(String::as_str),
                Some("false"),
                "a stale export must never re-open signup an admin closed"
            );
        }
    }

    /// Re-saving the value the environment already supplies does NOT pin the
    /// key.
    ///
    /// The rule [`set_with_row`] implements, and the rule the previous version
    /// of this test asserted the OPPOSITE of ("saving an unchanged value is
    /// still an admin claiming the key"). It passed anyway, because its fixture
    /// left the table empty: `set_by_admin` took the CREATE branch, which
    /// stamps unconditionally, and never reached the unchanged-save branch the
    /// test was named for. Staging the row first is the whole difference.
    ///
    /// Why the rule is what it is: `ui::settings_form::save_settings` calls
    /// `config::set` for every PLAIN field the form posts, and the form posts
    /// every named input — so stamping on any admin-surface write pinned a
    /// whole settings page at once. (A blank SENSITIVE field is skipped
    /// instead; that covers secrets, not the branding and toggle fields this
    /// rule is about.)
    #[tokio::test]
    async fn re_saving_the_value_the_environment_supplies_does_not_pin_the_key() {
        let db = migrated_db().await;
        let key = APP_NAME_KEY;
        seed_if_absent(&db, key, "Same", "App Name", "declared", false)
            .await
            .expect("the row a previous boot stored");

        // The admin saves the settings form without changing anything: the
        // value already equals what the environment would write.
        set_by_admin(
            &db,
            key,
            "Same",
            Some(false),
            crate::features::USER_EDITED_SENTINEL,
        )
        .await
        .expect("admin saves");

        assert!(
            !is_pinned(&find_by_key(&db, key).await.expect("list").expect("row")),
            "an unchanged save claims nothing: there is no divergence to pin, and any \
             later one is a real edit that stamps"
        );
    }

    /// A row whose pin state cannot be READ is left alone.
    ///
    /// The contract says a pinned value survives, and a failed read means we
    /// cannot tell whether this row is pinned — so it is not a licence to
    /// overwrite. The branch is `seed_and_load`'s `Err(e) => continue`, and it
    /// had no coverage at all: the test that claimed it never induced a read
    /// failure, and passed on an unrelated path.
    ///
    /// Driven through `break_list_reads`, so `find_by_key`'s `list` fails while
    /// writes still land — which is what lets the fixture stage the row first.
    /// `seed_and_load` itself returns `Err` (its final table read fails too),
    /// so the assertions are on the log: reading the row back is exactly what
    /// this fixture has made impossible.
    ///
    /// What the branch buys is that no write is ATTEMPTED, and that is what the
    /// third assertion pins. Without it this test is weaker than it looks:
    /// mutate the branch to `Err(_) => None` and `set_with_row` sees no existing
    /// row, so it can only take its CREATE path — which the `key` column's
    /// UNIQUE index rejects. Nothing would be overwritten and no config write
    /// would be recorded, so both of the other assertions would still pass, on
    /// a guarantee the schema is providing rather than this branch. The seeder
    /// would simply log a failed write per key, which is what the third
    /// assertion refuses.
    #[tokio::test]
    async fn a_row_whose_pin_state_cannot_be_read_is_not_overwritten() {
        let key = APP_NAME_KEY;
        let ctx = crate::test_support::TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        seed_row_with_owner(&ctx, key, "AdminChoice", "admin_1").await;

        let ctx = ctx.break_list_reads();
        let before = crate::config_generation::writes_noted_on_this_thread();
        let capture = crate::test_support::MessageCapture::default();
        {
            let _guard = tracing::subscriber::set_default(capture.clone());
            assert!(
                ctx.try_seed_env_vars(&[(key, "FromEnv")]).await.is_err(),
                "the premise: this boot cannot read the table"
            );
        }

        assert_eq!(
            capture.count_containing("leaving it alone"),
            1,
            "the operator has to be told the key was skipped and why"
        );
        assert_eq!(
            before,
            crate::config_generation::writes_noted_on_this_thread(),
            "an unreadable row must not be written"
        );
        assert_eq!(
            capture.count_containing("failed to seed env variable"),
            0,
            "and no write must be ATTEMPTED — that, not the outcome, is what this \
             branch buys; the unique index would refuse the write anyway"
        );
    }

    /// One "Save settings" click must pin only the field that changed.
    ///
    /// `ui::settings_form::save_settings` calls `config::set` for every PLAIN
    /// key the form posts, and the form posts every named input — so stamping
    /// on any admin-surface write pinned a whole page of variables at once,
    /// silently making their exports inert forever. (A blank SENSITIVE field is
    /// skipped instead, because `render_field` renders a secret empty; that
    /// covers secrets, not the branding fields below.) This drives the same
    /// shape: one changed key among several re-asserted ones.
    #[tokio::test]
    async fn saving_a_settings_form_pins_only_the_field_that_changed() {
        let db = migrated_db().await;
        let changed = PRIMARY_COLOR_KEY;
        let untouched = [
            APP_NAME_KEY,
            crate::config_vars::LOGO_URL_KEY,
            FAVICON_URL_KEY,
        ];

        // What the environment seeded on an earlier boot.
        let env: Vec<(String, String)> = std::iter::once(changed)
            .chain(untouched)
            .map(|k| (k.to_string(), format!("env-{k}")))
            .collect();
        seed_and_load(&db, &env).await.expect("seeded boot");

        // The admin opens the page, changes one colour, and saves. Every
        // field on the form is posted, including the ones they did not touch
        // (and, for a blank field, an empty value the form skips).
        let admin = crate::features::USER_EDITED_SENTINEL;
        set_by_admin(&db, changed, "#ff0000", Some(false), admin)
            .await
            .expect("the changed field");
        for key in untouched {
            set_by_admin(&db, key, &format!("env-{key}"), Some(false), admin)
                .await
                .expect("a re-asserted field");
        }

        assert!(
            is_pinned(&find_by_key(&db, changed).await.expect("l").expect("r")),
            "the field the admin actually changed is theirs"
        );
        for key in untouched {
            let row = find_by_key(&db, key).await.expect("l").expect("r");
            assert!(
                !is_pinned(&row),
                "{key} was only re-posted unchanged and must stay seeder-owned"
            );
        }

        // And the environment still governs the untouched keys on the next boot.
        let next: Vec<(String, String)> = std::iter::once(changed)
            .chain(untouched)
            .map(|k| (k.to_string(), format!("next-{k}")))
            .collect();
        let vars = seed_and_load(&db, &next).await.expect("next boot");
        assert_eq!(
            vars.get(changed).map(String::as_str),
            Some("#ff0000"),
            "the pinned key keeps the admin's value"
        );
        for key in untouched {
            assert_eq!(
                vars.get(key).map(String::as_str),
                Some(format!("next-{key}").as_str()),
                "{key} must still follow the environment"
            );
        }
    }

    /// The way out of a pinned key. Without it, admin-wins is a one-way door:
    /// `delete_variable` refuses declared shared vars and `update_variable`
    /// re-stamps ownership on every write.
    #[tokio::test]
    async fn reset_to_environment_hands_a_pinned_key_back() {
        let ctx = crate::test_support::TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let key = APP_NAME_KEY;

        insert(
            &ctx,
            NewVariable {
                key: key.to_string(),
                value: "AdminChoice".to_string(),
                name: String::new(),
                description: String::new(),
                warning: String::new(),
                sensitive: false,
                updated_by: "admin_1".to_string(),
                block: block_for_key(key),
            },
        )
        .await
        .expect("a pinned row");

        reset_to_environment(&ctx, key).await.expect("reset");

        let row = get_by_key(&ctx, key).await.expect("get").expect("row");
        assert!(!is_pinned(&row), "the marker is cleared");
        assert_eq!(
            row.value, "AdminChoice",
            "only the marker is cleared; the value stays until a boot re-seeds it"
        );
    }

    /// THE UPGRADE BOOT, and the case it exists for: a row a settings form
    /// edited BEFORE this release carries no marker, and a still-present export
    /// would revert it.
    ///
    /// `WAFER_RUN_SHARED__ALLOW_SIGNUP` is the concrete harm.
    /// `ui::settings_form::save_settings` -> `config::set` -> `CONFIG_SET` ->
    /// `variables::set` was the write path for the auth-ui settings form that
    /// carries it, and `set`'s create branch writes `updated_by: String::new()`
    /// — so an admin who closed signup during an abuse incident left no
    /// provenance at all, and there is nothing to restore the old value from
    /// afterwards.
    #[tokio::test]
    async fn the_upgrade_boot_keeps_a_pre_existing_settings_form_edit() {
        let db = migrated_db().await;
        let key = ALLOW_SIGNUP_KEY;

        // The pre-upgrade world: signup closed through a settings form, which
        // left no marker, and the compose file still says `true`.
        raw_insert_unowned(&db, key, "false").await;

        let vars = seed_and_load(&db, &[(key.to_string(), "true".to_string())])
            .await
            .expect("the boot that ships the fix");
        assert_eq!(
            vars.get(key).map(String::as_str),
            Some("false"),
            "the upgrade boot must not reopen signup an admin closed"
        );
        assert_eq!(
            pin_of(&find_by_key(&db, key).await.expect("l").expect("r")),
            Some(Pin::PreUpgrade),
            "and it is pinned as what it is — a row nobody can attribute — not as an admin edit"
        );

        // Every later boot leaves it alone too.
        let vars = seed_and_load(&db, &[(key.to_string(), "true".to_string())])
            .await
            .expect("later boot");
        assert_eq!(vars.get(key).map(String::as_str), Some("false"));
    }

    /// THE BOUNDARY OF THE TRANSITION, as three executable cases.
    ///
    /// A pre-upgrade edit is protected only when, on the upgrade boot, ALL of
    /// this held: the environment exported the key, with a non-empty value, and
    /// that value differed from the stored one. Each case below breaks exactly
    /// one of those and shows the same outcome — the row is left unclaimed, the
    /// gate is recorded anyway, and a LATER export therefore wins.
    ///
    /// Stated positively and pinned here because the prose version of it has now
    /// been wrong twice, each time by being more generous than
    /// `seed_and_load`'s actual branch condition. A test cannot be more generous
    /// than the code.
    ///
    /// None of these is a defect to fix by stamping: the rule compares against
    /// the export, and stamping a row that AGREES with its export would pin
    /// most keys on most deployments and re-break the headline bug. They are
    /// the cost of that rule, and the operator's remedy is the same in all
    /// three — re-apply the setting in the UI once after upgrading, which
    /// stamps it for good.
    #[tokio::test]
    async fn a_pre_upgrade_edit_is_unprotected_unless_the_export_differed() {
        let key = ALLOW_SIGNUP_KEY;
        // Every case exports SOMETHING, so the gate is recorded on the upgrade
        // boot. That is load-bearing: a boot carrying no exports at all records
        // no gate and leaves the transition armed for the next one, so the key
        // would still be protected — see
        // `a_boot_with_no_environment_records_no_transition_and_writes_nothing`.
        let filler = (APP_NAME_KEY.to_string(), "Shop".to_string());
        // (what the upgrade boot exports for `key`, why the transition passes it over)
        let cases = [
            (None, "the key is not in the exported batch"),
            (
                Some(String::new()),
                "it is exported empty, which is 'unset' by this repo's convention",
            ),
            (
                Some("false".to_string()),
                "it is exported, but with the value already stored",
            ),
        ];

        for (exported, why) in cases {
            let db = migrated_db().await;
            let mut upgrade_env = vec![filler.clone()];
            if let Some(value) = exported {
                upgrade_env.push((key.to_string(), value));
            }
            // The pre-upgrade world: an admin closed signup through a settings
            // form, which left no marker.
            raw_insert_unowned(&db, key, "false").await;

            let capture = crate::test_support::MessageCapture::default();
            {
                let _guard = tracing::subscriber::set_default(capture.clone());
                seed_and_load(&db, &upgrade_env)
                    .await
                    .expect("upgrade boot");
            }
            assert_eq!(
                capture.count_containing("NO EFFECT"),
                0,
                "nothing is reported when {why} — which is what makes this silent"
            );
            // `is_unclaimed`, not `pin_of(..) == None`: this module spent a
            // commit establishing that the two differ (a released row is
            // unpinned but claimed), and it is the stronger one that makes the
            // later export win below.
            assert!(
                is_unclaimed(&find_by_key(&db, key).await.expect("l").expect("r")),
                "the row is left unclaimed when {why}"
            );

            // The operator adds (or changes) the export afterwards.
            let vars = seed_and_load(&db, &[(key.to_string(), "true".to_string())])
                .await
                .expect("later boot");
            assert_eq!(
                vars.get(key).map(String::as_str),
                Some("true"),
                "a later export wins because {why}"
            );
        }
    }

    /// A block-scoped credential rotated through the admin UI, with the old one
    /// still in the compose file.
    ///
    /// The transition lives inside the env loop precisely so this is covered:
    /// a pass over `config_vars::shared_config_vars()` would not see a
    /// block-scoped key like `IMPRESSPRESS__EMAIL__MAILGUN_API_KEY` at all, and
    /// reverting to a revoked key is the expensive version of this mistake.
    #[tokio::test]
    async fn the_upgrade_boot_keeps_a_rotated_block_scoped_secret() {
        let db = migrated_db().await;
        let key = MAILGUN_API_KEY;
        assert!(
            crate::config_vars::is_sensitive_for_storage(key),
            "the point of this case is a credential"
        );
        raw_insert_unowned(&db, key, "sk_live_rotated").await;

        let vars = seed_and_load(&db, &[(key.to_string(), "sk_live_revoked".to_string())])
            .await
            .expect("upgrade boot");
        assert_eq!(
            vars.get(key).map(String::as_str),
            Some("sk_live_rotated"),
            "a rotated credential must not be replaced by the revoked one still in the \
             environment"
        );
    }

    /// An unmarked row that already AGREES with its export is not a conflict:
    /// nothing is pinned and nothing is said.
    ///
    /// This is the common case on every upgrading deployment — the environment
    /// seeded the row and has not changed since — so pinning it would freeze a
    /// key the operator still controls, and warning about it would bury the
    /// lines that matter.
    #[tokio::test]
    async fn the_upgrade_boot_leaves_a_row_that_matches_its_export_alone() {
        let db = migrated_db().await;
        let key = APP_NAME_KEY;
        raw_insert_unowned(&db, key, "Foo").await;

        let capture = crate::test_support::MessageCapture::default();
        {
            let _guard = tracing::subscriber::set_default(capture.clone());
            seed_and_load(&db, &[(key.to_string(), "Foo".to_string())])
                .await
                .expect("upgrade boot");
        }

        let row = find_by_key(&db, key).await.expect("l").expect("r");
        assert_eq!(pin_of(&row), None, "agreement is not a claim");
        assert_eq!(
            capture.count_containing("NO EFFECT"),
            0,
            "a row that already holds what the export says has nothing to report"
        );

        // And the environment still governs it on the next boot, gate or no gate.
        let vars = seed_and_load(&db, &[(key.to_string(), "Bar".to_string())])
            .await
            .expect("next boot");
        assert_eq!(vars.get(key).map(String::as_str), Some("Bar"));
    }

    /// A declared default that MOVES between releases must not read as an edit.
    ///
    /// `WAFER_RUN_SHARED__FAVICON_URL` and `WAFER_RUN_SHARED__LOGO_ICON_URL`
    /// default to content-hashed asset URLs (`ui::assets`), so a release that
    /// changes those bytes changes the default. Comparing a stored row against
    /// its declared default — which is what the withdrawn backfill did — would
    /// have claimed every such row on the next unrelated release. The
    /// comparison is against the EXPORT, so a key with no export is not
    /// considered at all.
    #[tokio::test]
    async fn a_moved_declared_default_is_not_mistaken_for_an_edit() {
        let db = migrated_db().await;
        let key = FAVICON_URL_KEY;
        // What an earlier release's default was: a hash this build no longer
        // produces, so it differs from today's declared default.
        raw_insert_unowned(&db, key, "/b/static/favicon-deadbeef.ico").await;
        assert_ne!(
            crate::ui::assets::favicon_url(),
            "/b/static/favicon-deadbeef.ico",
            "the fixture has to be a default this build has moved away from"
        );

        // A boot with no export for that key at all — the ordinary case.
        seed_and_load(&db, &[(APP_NAME_KEY.to_string(), "Foo".to_string())])
            .await
            .expect("upgrade boot");

        assert_eq!(
            pin_of(&find_by_key(&db, key).await.expect("l").expect("r")),
            None,
            "a row the environment says nothing about must not be pinned by an unrelated \
             release moving its default"
        );
    }

    /// An admin who REVERTED a key to its declared default during an incident
    /// is protected too.
    ///
    /// `WAFER_RUN_SHARED__ENABLE_OAUTH` declares `false`; a compose file says
    /// `true`; the admin turns OAuth off. Stored now EQUALS the declared
    /// default, so the withdrawn value-differs-from-default backfill would have
    /// passed straight over it and the environment would have switched OAuth
    /// back on. Comparing against the export catches it.
    #[tokio::test]
    async fn the_upgrade_boot_keeps_an_edit_that_reverted_to_the_declared_default() {
        let db = migrated_db().await;
        let key = ENABLE_OAUTH_KEY;
        let declared = crate::config_vars::shared_config_vars()
            .into_iter()
            .find(|var| var.key == key)
            .expect("declared")
            .default;
        assert_eq!(declared, "false", "the fixture depends on this declaration");

        raw_insert_unowned(&db, key, "false").await;
        let vars = seed_and_load(&db, &[(key.to_string(), "true".to_string())])
            .await
            .expect("upgrade boot");
        assert_eq!(
            vars.get(key).map(String::as_str),
            Some("false"),
            "an admin's revert-to-default is still an admin decision"
        );
    }

    /// The transition is strictly one-time: a key an operator RESETS after it
    /// must stay reset, or the reset route would not work at all.
    #[tokio::test]
    async fn the_upgrade_transition_does_not_reclaim_a_key_after_a_reset() {
        let ctx = crate::test_support::TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let key = APP_NAME_KEY;
        seed_row_with_owner(&ctx, key, "EditedLongAgo", "").await;

        ctx.seed_env_vars(&[(key, "FromEnv")]).await;
        assert_eq!(
            pin_of(&get_by_key(&ctx, key).await.expect("g").expect("r")),
            Some(Pin::PreUpgrade)
        );
        assert!(
            get_by_key(&ctx, ENV_PRECEDENCE_TRANSITION_KEY)
                .await
                .expect("g")
                .is_some(),
            "the gate has to be recorded, or 'one-time' is not one-time"
        );

        // The operator says "that was the environment, not me", through the
        // real control rather than a hand-written column write.
        reset_to_environment(&ctx, key).await.expect("reset");

        ctx.seed_env_vars(&[(key, "FromEnv")]).await;
        assert_eq!(
            get_by_key(&ctx, key).await.expect("g").expect("r").value,
            "FromEnv",
            "a reset key must follow the environment again, not be re-claimed"
        );
    }

    /// A reset survives a deployment whose transition has NOT yet run.
    ///
    /// The gate is only recorded by a boot that had an environment to
    /// transition against, so a deployment that starts with no exports at all
    /// reaches the admin UI with the transition still armed. If
    /// [`reset_to_environment`] wrote an empty marker, the first boot that DID
    /// carry an export would read the released row as never-considered and pin
    /// it straight back — the reset control silently not working, on the one
    /// path the boot WARN sends operators down.
    #[tokio::test]
    async fn a_reset_is_not_undone_by_a_transition_that_has_not_run_yet() {
        let ctx = crate::test_support::TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let key = APP_NAME_KEY;

        // No exports at all, so no gate is recorded.
        ctx.seed_env_vars(&[]).await;
        assert!(
            get_by_key(&ctx, ENV_PRECEDENCE_TRANSITION_KEY)
                .await
                .expect("g")
                .is_none(),
            "the premise: the transition is still armed"
        );

        // An admin sets it — the shape `set_by_admin` writes — then hands it
        // back.
        seed_row_with_owner(&ctx, key, "AdminChoice", "admin_1").await;
        reset_to_environment(&ctx, key).await.expect("reset");
        assert!(!is_pinned(
            &get_by_key(&ctx, key).await.expect("g").expect("r")
        ));

        // The operator adds the export they wanted all along.
        ctx.seed_env_vars(&[(key, "FromEnv")]).await;
        assert_eq!(
            get_by_key(&ctx, key).await.expect("g").expect("r").value,
            "FromEnv",
            "the reset has to hold against the transition, not just against later boots"
        );
    }

    /// Once the gate exists, an unmarked row is ordinary: the environment wins.
    ///
    /// The half of the contract the whole branch exists for. A row written
    /// AFTER the transition — by a later boot's environment, or by a
    /// `seed_if_absent` default — carries no marker and must not inherit the
    /// transition's protection.
    ///
    /// It is also the boundary of what the transition promises, and the reason
    /// that boundary is written down rather than discovered: the same rule
    /// means any key the transition PASSED OVER on the gate-recording boot —
    /// absent from the batch, exported empty, or exported with the value
    /// already stored — is ordinary from then on, so a later export for it
    /// takes effect even over a change an admin made in the UI before
    /// upgrading. `a_pre_upgrade_edit_is_unprotected_unless_the_export_differed`
    /// drives all three. Closing it would mean stamping rows that agree with
    /// their export, which is most rows, and re-breaks the headline bug for
    /// every key an operator has been waiting to set.
    #[tokio::test]
    async fn after_the_transition_an_unmarked_row_follows_the_environment() {
        let db = migrated_db().await;
        let key = APP_NAME_KEY;

        // Boot 1 runs the transition over an empty table and records the gate.
        seed_and_load(&db, &[(key.to_string(), "First".to_string())])
            .await
            .expect("boot 1");

        // A row that looks exactly like the pre-upgrade case, but arrived after
        // the transition.
        let other = PRIMARY_COLOR_KEY;
        raw_insert_unowned(&db, other, "#000000").await;

        let vars = seed_and_load(&db, &[(other.to_string(), "#ff0000".to_string())])
            .await
            .expect("boot 2");
        assert_eq!(
            vars.get(other).map(String::as_str),
            Some("#ff0000"),
            "past the gate, an unmarked row is the environment's to set"
        );
    }

    /// A target with NO process environment must record nothing.
    ///
    /// Cloudflare never calls this at all and the browser calls it with `&[]`
    /// (`impresspress-web`'s `config::seed_and_load_variables`), so the gate row
    /// must not be written there: it would claim a transition that never ran,
    /// and it would bump the config-write generation — invalidating every
    /// memoized config reader — for nothing.
    #[tokio::test]
    async fn a_boot_with_no_environment_records_no_transition_and_writes_nothing() {
        let db = migrated_db().await;
        raw_insert_unowned(&db, APP_NAME_KEY, "Stored").await;

        seed_and_load(&db, &[]).await.expect("first boot");
        let before = crate::config_generation::writes_noted_on_this_thread();
        seed_and_load(&db, &[]).await.expect("second boot");

        assert!(
            find_by_key(&db, ENV_PRECEDENCE_TRANSITION_KEY)
                .await
                .expect("l")
                .is_none(),
            "a boot with no environment has nothing to transition"
        );
        assert_eq!(
            before,
            crate::config_generation::writes_noted_on_this_thread(),
            "and must not write at all"
        );
    }

    /// The steady-state WARN fires only on a real disagreement.
    ///
    /// A pinned row that already holds what the export says is not a conflict;
    /// a line about it on every boot is how an operator learns to skip the line
    /// that matters. The disagreeing case is the positive control in the same
    /// test, so a `warn_export_is_inert` that simply never fired could not pass.
    #[tokio::test]
    async fn the_inert_export_warning_fires_only_when_the_values_differ() {
        let key = APP_NAME_KEY;

        for (stored, exported, expected) in [("Same", "Same", 0), ("Stored", "Exported", 1)] {
            let db = migrated_db().await;
            // `None`: there is no row on a fresh database, and `CONFIG_SET`
            // passes the row it read, so an admin edit that CREATES has
            // nothing to say about the flag. Every other admin-surface
            // fixture here stages the row first and so passes `Some(false)`.
            set_by_admin(&db, key, stored, None, "admin_1")
                .await
                .expect("admin edit");

            let capture = crate::test_support::MessageCapture::default();
            {
                let _guard = tracing::subscriber::set_default(capture.clone());
                seed_and_load(&db, &[(key.to_string(), exported.to_string())])
                    .await
                    .expect("boot");
            }
            assert_eq!(
                capture.count_containing("NO EFFECT"),
                expected,
                "stored={stored} exported={exported}"
            );
        }
    }

    /// Every inert key is named; the advice about them is given ONCE.
    ///
    /// The advice used to be appended to every per-key line — ~170 identical
    /// characters each, so at ten pinned keys the boot log is 5 KB of the same
    /// sentence and the key names, the only part an operator has to act on, are
    /// what gets buried. The upgrade boot pins precisely the keys the operator
    /// has changed, so several at once is the motivating case, not the rare one.
    #[tokio::test]
    async fn the_recovery_advice_is_given_once_however_many_keys_are_pinned() {
        let db = migrated_db().await;
        let keys = [
            APP_NAME_KEY,
            PRIMARY_COLOR_KEY,
            crate::config_vars::LOGO_URL_KEY,
        ];
        let env: Vec<(String, String)> = keys
            .iter()
            .map(|k| ((*k).to_string(), format!("env-{k}")))
            .collect();
        for key in keys {
            raw_insert_unowned(&db, key, "stored").await;
        }

        let capture = crate::test_support::MessageCapture::default();
        {
            let _guard = tracing::subscriber::set_default(capture.clone());
            seed_and_load(&db, &env).await.expect("upgrade boot");
        }

        // One line per key. The key itself rides on the `key` field rather than
        // in the message text — the convention everywhere in this module — and
        // `MessageCapture` records only `message`, so the count is what can be
        // asserted here.
        assert_eq!(
            capture.count_containing("NO EFFECT"),
            keys.len(),
            "every inert export has to be named on its own line"
        );
        assert_eq!(
            capture.count_containing("Reset to environment"),
            1,
            "and the advice exactly once, however many keys there are"
        );
        // Every one of these was pinned by this very boot's transition, so the
        // bulk control will be on the page and the advice says so.
        assert_eq!(
            capture.count_containing("Reset all keys pinned at upgrade"),
            1,
            "with several upgrade pins, the one-press route has to be named"
        );
    }

    /// The summary names the BULK control only when that control will render.
    ///
    /// It is gated on at least one upgrade pin, and an admin-edited key is not
    /// one: `PinnedAtUpgrade::of` refuses it, so nothing on the Variables page
    /// would offer to release it in bulk. Naming a button that is not there is
    /// the shape of advice this module's WARNs exist to stop giving.
    #[tokio::test]
    async fn the_bulk_advice_is_withheld_when_only_an_admin_edit_is_inert() {
        let ctx = crate::test_support::TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let key = APP_NAME_KEY;
        seed_row_with_owner(&ctx, key, "AdminChoice", "admin_1").await;

        let capture = crate::test_support::MessageCapture::default();
        {
            let _guard = tracing::subscriber::set_default(capture.clone());
            ctx.seed_env_vars(&[(key, "FromEnv")]).await;
        }

        assert_eq!(
            capture.count_containing("Reset to environment"),
            1,
            "the per-key advice still applies — an admin can release their own edit"
        );
        assert_eq!(
            capture.count_containing("Reset all keys pinned at upgrade"),
            0,
            "but the bulk control does not render for an admin edit, so it is not named"
        );
    }

    /// The summary must not put a NUMBER on what the bulk control will release.
    ///
    /// This boot's count is over the batch it just walked; the page's count is
    /// over the table. Drop one export from a deployment with two pinned keys
    /// and the two disagree — the log would say one, the button would say two,
    /// and the action would release two. An operator reading both has no way to
    /// tell which is lying, so the line names the control and leaves the count
    /// to the page that can take it honestly.
    #[tokio::test]
    async fn the_bulk_advice_claims_no_count_the_page_would_contradict() {
        let ctx = crate::test_support::TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let kept = APP_NAME_KEY;
        let dropped = AUTH_HEADLINE_KEY;
        for key in [kept, dropped] {
            seed_row_with_owner(&ctx, key, "KeptAtUpgrade", PRE_UPGRADE_SENTINEL).await;
        }

        // The operator has since removed one of the two exports, so this boot
        // sees only one of the pinned keys — while both are still pinned.
        let capture = crate::test_support::MessageCapture::default();
        {
            let _guard = tracing::subscriber::set_default(capture.clone());
            ctx.seed_env_vars(&[(kept, "FromEnv")]).await;
        }

        assert_eq!(
            keys_pinned_at_upgrade(&ctx).await.expect("select").len(),
            2,
            "the premise: the button would offer to release both"
        );
        assert_eq!(
            capture.count_containing("Reset all keys pinned at upgrade"),
            1,
            "the bulk route is still worth naming"
        );
        assert_eq!(
            capture.count_containing("releases every such key at once"),
            1,
            "but it must describe the scope rather than count it"
        );
        for wrong in ["1 of them", "releases all 1"] {
            assert_eq!(
                capture.count_containing(wrong),
                0,
                "the line must not claim a figure the page contradicts: {wrong:?}"
            );
        }
        // And not as a structured field either. A field is not a lesser claim
        // than message text — this is exactly where the figure hid after it was
        // taken out of the sentence.
        assert_eq!(
            capture.count_fields_containing("upgrade_pins"),
            0,
            "the count must not be published as a field once it is out of the message"
        );
        assert_eq!(
            capture.count_fields_containing("inert_exports=1"),
            1,
            "the field that IS published has to be the one the message agrees with, \
             or this test would pass by publishing nothing at all"
        );
    }

    /// No boot line ever prints the value of a credential, on either side of a
    /// pin.
    ///
    /// A log file is harder to redact than a table, and an operator sent to
    /// compare a Stripe key can read the stored one where they manage it. This
    /// drives both reporting paths — the upgrade pin and the steady-state
    /// inert-export line — because they are two different call sites and only
    /// one of them existed first.
    #[tokio::test]
    async fn no_boot_line_prints_a_credential() {
        let key = MAILGUN_API_KEY;
        const STORED: &str = "sk_live_stored_secret";
        const EXPORTED: &str = "sk_live_exported_secret";

        // The upgrade pin.
        let db = migrated_db().await;
        raw_insert_unowned(&db, key, STORED).await;
        let capture = crate::test_support::MessageCapture::default();
        {
            let _guard = tracing::subscriber::set_default(capture.clone());
            seed_and_load(&db, &[(key.to_string(), EXPORTED.to_string())])
                .await
                .expect("upgrade boot");
        }
        assert_eq!(capture.count_containing(STORED), 0, "the stored credential");
        assert_eq!(
            capture.count_containing(EXPORTED),
            0,
            "the exported credential"
        );
        assert_eq!(
            capture.count_containing("compare the stored value"),
            1,
            "and the operator is told where to compare instead"
        );

        // The steady-state line, on the next boot over the now-pinned row.
        let capture = crate::test_support::MessageCapture::default();
        {
            let _guard = tracing::subscriber::set_default(capture.clone());
            seed_and_load(&db, &[(key.to_string(), EXPORTED.to_string())])
                .await
                .expect("later boot");
        }
        assert_eq!(capture.count_containing(STORED), 0);
        assert_eq!(capture.count_containing(EXPORTED), 0);
        assert_eq!(capture.count_containing("compare the stored value"), 1);
    }

    /// `FOO=` in a shell or a `.env` file is "unset", not "set to blank": an
    /// empty env value must not wipe a meaningful stored one. Same convention
    /// as `admin::settings::seed_defaults` and `AuthConfig::from_map`.
    #[tokio::test]
    async fn an_empty_env_value_does_not_blank_out_a_stored_value() {
        let db = migrated_db().await;
        let key = APP_NAME_KEY;
        seed_if_absent(&db, key, "Impresspress", "App Name", "declared", false)
            .await
            .expect("seed");

        let vars = seed_and_load(&db, &[(key.to_string(), String::new())])
            .await
            .expect("seed and load");
        assert_eq!(vars.get(key).map(String::as_str), Some("Impresspress"));
    }

    /// An env-written row for a `Password`-typed declared var must carry
    /// `sensitive = 1`, even though its key ends in neither `_SECRET` nor
    /// `_KEY`. The read path's union (`util::is_sensitive_key`) asks the
    /// declaration too now, so a wrong column no longer publishes the value —
    /// but it still decides what the admin UI's Sensitive control reads back,
    /// and leaving it wrong means every boot re-runs the repair pass and every
    /// repair re-reports a breach. The masking itself is asserted in
    /// `blocks::admin::settings`; this pins the column beside it.
    #[tokio::test]
    async fn an_env_written_password_var_is_stored_sensitive() {
        let db = migrated_db().await;
        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        assert!(
            !crate::config_vars::has_sensitive_suffix(key),
            "the point of this test is a key the suffix rule cannot catch"
        );

        seed_and_load(&db, &[(key.to_string(), "hunter2".to_string())])
            .await
            .expect("seed and load");

        let row = find_by_key(&db, key).await.expect("list").expect("row");
        assert!(row.sensitive, "a declared Password var must be flagged");
    }

    /// Write a row the way an OLDER BUILD did — straight to the table,
    /// bypassing [`NewVariable::into_row`], which now settles the flag — so
    /// the repair paths have something to repair. No supported API can produce
    /// this row any more, which is the point of the funnel.
    ///
    /// `flag` is written into the `sensitive` column verbatim, so a test can
    /// stage the shapes the schema does not declare (`2`, `true`, `"true"`,
    /// `1.0`) as well as a plain clear flag.
    async fn raw_insert_with_flag(
        db: &Arc<dyn DatabaseService>,
        key: &str,
        value: &str,
        flag: Value,
    ) {
        let now = crate::util::now_rfc3339();
        let mut data = VariableRow {
            id: format!("var_{}", uuid::Uuid::new_v4()),
            key: key.to_string(),
            value: value.to_string(),
            name: String::new(),
            description: String::new(),
            warning: String::new(),
            sensitive: false,
            block: block_for_key(key),
            updated_by: String::new(),
            created_at: now.clone(),
            updated_at: now,
        }
        .to_data();
        data.insert("sensitive".to_string(), flag);
        db.create(TABLE, data).await.expect("raw create");
    }

    /// [`raw_insert_with_flag`] with the clear flag an older build wrote.
    async fn raw_insert_unflagged(db: &Arc<dyn DatabaseService>, key: &str, value: &str) {
        raw_insert_with_flag(db, key, value, json!(0)).await;
    }

    /// A row as it looked BEFORE this release recorded ownership: a real value
    /// and an empty `updated_by`. Physically the same shape as
    /// [`raw_insert_unflagged`] — a pre-upgrade row is both unowned and
    /// unflagged — named for the property the precedence tests are about, so a
    /// test that is not about masking does not read as though it were.
    async fn raw_insert_unowned(db: &Arc<dyn DatabaseService>, key: &str, value: &str) {
        raw_insert_with_flag(db, key, value, json!(0)).await;
    }

    /// Every create funnels through `NewVariable::into_row`, so a caller that
    /// passes `sensitive: false` for a `Password`-typed declared var gets a
    /// flagged row anyway. This is the class fix: `seed_if_absent`, `set`'s
    /// create branch, `insert` and `upsert_by_key`'s create branch all pass
    /// through it, so no call site can reintroduce the leak.
    #[tokio::test]
    async fn every_create_path_flags_a_password_var_whatever_the_caller_passes() {
        let password = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        let token = crate::blocks::auth::config::BOOTSTRAP_ADMIN_TOKEN_KEY;

        let db = migrated_db().await;
        seed_if_absent(&db, password, "hunter2", "", "", false)
            .await
            .expect("seed");
        assert!(
            find_by_key(&db, password)
                .await
                .expect("list")
                .expect("row")
                .sensitive,
            "seed_if_absent must not be able to store a Password var unflagged"
        );

        let db = migrated_db().await;
        set(&db, token, "tok", "", "", Some(false))
            .await
            .expect("set-create");
        assert!(
            find_by_key(&db, token)
                .await
                .expect("list")
                .expect("row")
                .sensitive,
            "set's create branch must not be able to store a Password var unflagged"
        );
    }

    /// A row an older build left unflagged is repaired in place by a write,
    /// with its value untouched, and the repair is not reported as a config
    /// change.
    #[tokio::test]
    async fn a_write_repairs_an_unflagged_row_without_touching_its_value() {
        let db = migrated_db().await;
        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        raw_insert_unflagged(&db, key, "hunter2").await;

        // `Some(false)` is what `blocks::config`'s CONFIG_SET passes for an
        // existing row: the row's own stored flag, i.e. its own mistake.
        assert_eq!(
            set(&db, key, "hunter2", "", "", Some(false))
                .await
                .expect("repair"),
            Wrote::FlagRaised,
            "an unchanged value whose flag needs raising is a repair, not a replacement"
        );
        let row = find_by_key(&db, key).await.expect("list").expect("row");
        assert!(row.sensitive);
        assert_eq!(row.value, "hunter2", "a repair must not move the value");

        assert_eq!(
            set(&db, key, "hunter2", "", "", Some(false))
                .await
                .expect("re-assert"),
            Wrote::Unchanged,
            "the repair is idempotent"
        );
        assert!(
            find_by_key(&db, key)
                .await
                .expect("list")
                .expect("row")
                .sensitive,
            "the flag must never be lowered — the read path's union only ever \
             resolves towards more masking"
        );
    }

    /// The one-shot boot repair, which is what reaches the row that matters
    /// most: a bootstrap credential an operator set once from `.env` and then
    /// removed. Nothing writes that key any more — `seed_and_load` only
    /// touches keys the environment still exports, and `seed_defaults`'
    /// existing-row branch is hash-gated for the life of a release — so
    /// without this pass it stays unmasked indefinitely.
    #[tokio::test]
    async fn boot_repairs_an_unflagged_row_for_a_key_the_environment_no_longer_sets() {
        let db = migrated_db().await;
        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        raw_insert_unflagged(&db, key, "hunter2").await;

        // An empty environment: the operator removed the export.
        let vars = seed_and_load(&db, &[]).await.expect("boot");

        let row = find_by_key(&db, key).await.expect("list").expect("row");
        assert!(
            row.sensitive,
            "the boot pass must repair a row nothing else will ever revisit"
        );
        assert_eq!(row.value, "hunter2", "a repair must not move the value");
        assert_eq!(
            vars.get(key).map(String::as_str),
            Some("hunter2"),
            "and the loaded map still carries the value"
        );

        // Idempotent: a healthy table costs no writes.
        let before = crate::config_generation::writes_noted_on_this_thread();
        seed_and_load(&db, &[]).await.expect("second boot");
        assert_eq!(
            before,
            crate::config_generation::writes_noted_on_this_thread(),
            "a boot with nothing to repair must not write"
        );
    }

    /// The standalone entry point, which is what the HOSTED target calls:
    /// Cloudflare never runs `seed_and_load` (it has no process environment to
    /// seed from), so `CfDeployBootHooks::seed_and_load` calls this directly.
    /// That crate is wasm-only and CI does not execute its tests, so this is
    /// the executed coverage for the function behind that call.
    #[tokio::test]
    async fn the_standalone_repair_entry_point_fixes_an_unflagged_row() {
        let db = migrated_db().await;
        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        raw_insert_unflagged(&db, key, "hunter2").await;

        repair_sensitive_flags(&db).await;

        let row = find_by_key(&db, key).await.expect("list").expect("row");
        assert!(row.sensitive);
        assert_eq!(row.value, "hunter2", "a repair must not move the value");
    }

    /// A flag stored in a shape the schema does not declare is normalised,
    /// not skipped.
    ///
    /// `2`, `true` and `"true"` all read as flagged by `RecordExt::bool_field`
    /// — so the repair pass used to `continue` past them as "already fine" —
    /// while `util::is_sensitive_key` wanted exactly `1` and
    /// `cache_key::row_is_sensitive` converted with `json_as_i64`, which
    /// answers `None` for a bool and for `"true"`. The row was therefore
    /// served in the clear and judged KV-cacheable at the same time as being
    /// "already flagged". Both halves are asserted: every shape reads as
    /// sensitive on the two read paths, and the repair rewrites it to the
    /// canonical integer.
    #[tokio::test]
    async fn a_non_canonical_sensitive_flag_is_read_as_set_and_normalised() {
        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        for shape in [
            serde_json::json!(2),
            serde_json::json!(true),
            serde_json::json!("true"),
            // A float is a shape `variable_is_exportable` already anticipates.
            // Read as unset it would have been served in the clear AND
            // rewritten on every boot without ever converging, since
            // `flag_is_canonical_one` can never become true for it.
            serde_json::json!(1.0),
        ] {
            // The read paths must already agree that this is sensitive, with
            // no boot required.
            assert!(
                crate::util::flag_is_set(&shape),
                "{shape} must read as a set flag"
            );
            let mut row_map = HashMap::new();
            row_map.insert("key".to_string(), serde_json::json!(key));
            row_map.insert("sensitive".to_string(), shape.clone());
            assert!(
                crate::cache_key::row_is_sensitive(
                    crate::cache_key::CachedTable::Variables,
                    &row_map
                ),
                "{shape} must keep the row out of the KV cache"
            );

            // And the repair normalises it to the declared integer.
            let db = migrated_db().await;
            raw_insert_with_flag(&db, key, "hunter2", shape.clone()).await;

            repair_sensitive_flags(&db).await;

            let stored = db
                .list(
                    TABLE,
                    &ListOptions {
                        limit: Some(10),
                        skip_count: true,
                        ..Default::default()
                    },
                )
                .await
                .expect("list")
                .records;
            assert_eq!(
                stored[0].data.get("sensitive"),
                Some(&serde_json::json!(1)),
                "{shape} must be normalised to the canonical integer"
            );
        }
    }

    /// A non-canonical but TRUTHY flag is a shape defect, not a breach.
    ///
    /// `2`, `true`, `"true"` and `1.0` all decode through `flag_is_set`, so
    /// `VariableRow::sensitive` is true and every read path masked the value on
    /// the flag alone. The repair still rewrites the column — that is what the
    /// test above pins — but reporting "an EARLIER BUILD … served its value
    /// unmasked" for such a row sends an operator to rotate a credential that
    /// was never published. The pass already refuses that false alarm for the
    /// `_SECRET`/`_KEY` case; the argument is the same here.
    ///
    /// The clear-flag row is the positive control in the same test, so a
    /// `was_exposed` that simply answered `false` everywhere could not pass it.
    #[tokio::test]
    async fn only_a_row_that_was_really_readable_is_reported_as_exposed() {
        /// A fragment unique to the breach WARN.
        const BREACH: &str = "rotate it";
        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        assert!(
            !crate::config_vars::has_sensitive_suffix(key),
            "a suffix key is already exempt for a different reason; this needs the other kind"
        );

        for shape in [json!(2), json!(true), json!("true"), json!(1.0)] {
            let db = migrated_db().await;
            raw_insert_with_flag(&db, key, "hunter2", shape.clone()).await;

            let capture = crate::test_support::MessageCapture::default();
            {
                let _guard = tracing::subscriber::set_default(capture.clone());
                repair_sensitive_flags(&db).await;
            }
            assert_eq!(
                capture.count_containing(BREACH),
                0,
                "{shape} read as a set flag, so the value was masked all along — \
                 reporting it as exposed is a false breach report"
            );
        }

        // Positive control: a genuinely clear flag on the same key WAS served
        // unmasked by the build that wrote it, and must still be reported.
        let db = migrated_db().await;
        raw_insert_unflagged(&db, key, "hunter2").await;
        let capture = crate::test_support::MessageCapture::default();
        {
            let _guard = tracing::subscriber::set_default(capture.clone());
            repair_sensitive_flags(&db).await;
        }
        assert_eq!(
            capture.count_containing(BREACH),
            1,
            "a declaration-only-sensitive row stored with a clear flag really was \
             published by the build that wrote it, and the operator has to be told"
        );
    }

    /// The repair pass NEVER clears a flag, only raises it.
    ///
    /// It used to clear one on any declared key the declaration did not call
    /// for, so a mis-flag was recoverable. The Add Variable modal ticks
    /// Sensitive by default and a declared var with an empty default has no
    /// row until an admin makes one — so
    /// `WAFER_RUN_SHARED__EMBEDDED_SCRIPTS`, which carries operator-supplied
    /// script text that routinely embeds an analytics or API key, could be
    /// created flagged and silently unflagged by the next boot, then rendered
    /// in clear and allowed into a seed bundle. Recovery from a mis-flag is
    /// the edit form's job now.
    #[tokio::test]
    async fn the_repair_pass_never_clears_a_flag_an_admin_set() {
        let db = migrated_db().await;
        let key = EMBEDDED_SCRIPTS_KEY;
        assert!(
            !crate::config_vars::is_sensitive_for_storage(key),
            "the declaration does not call for a flag here — that is the case under test"
        );

        // What the Add Variable modal produces with its default tick.
        seed_if_absent(&db, key, "/analytics.js?token=abc123", "", "", true)
            .await
            .expect("admin creates it flagged");

        repair_sensitive_flags(&db).await;

        assert!(
            find_by_key(&db, key)
                .await
                .expect("list")
                .expect("row")
                .sensitive,
            "a boot must never unmask a value an admin chose to mask"
        );
    }

    /// A row that is legitimately not sensitive is left alone by the repair
    /// pass — it raises flags, it does not set them everywhere.
    #[tokio::test]
    async fn the_boot_repair_leaves_an_ordinary_row_alone() {
        let db = migrated_db().await;
        seed_if_absent(&db, APP_NAME_KEY, "Foo", "", "", false)
            .await
            .expect("seed");
        seed_and_load(&db, &[]).await.expect("boot");
        assert!(
            !find_by_key(&db, APP_NAME_KEY)
                .await
                .expect("list")
                .expect("row")
                .sensitive
        );
    }

    /// An env var must never write a key the RUNTIME owns. Infrastructure
    /// (`IMPRESSPRESS_*` with no `__`) and internal adapter-injected (`__…__`)
    /// keys are not variables-table config by the repo's naming rules, and
    /// `blocks::config` answers them from the boot map whatever the table
    /// holds — so a row for one is at best dead weight and at worst a forgery
    /// (`__IMPRESSPRESS_RUNTIME_KIND__` is what keeps Stripe secret-key
    /// operations off in a visitor's browser).
    ///
    /// This is the ONLY coverage of that guard, and deliberately calls
    /// `seed_and_load` directly: on native both classes are already filtered
    /// out upstream (`collect_app_env_vars` + `filter_to_declared_keys`), so no
    /// test driving the production path can reach the branch. The guard is
    /// defence in depth for a future caller that assembles its own batch.
    #[tokio::test]
    async fn a_runtime_owned_key_is_never_written_from_the_environment() {
        let db = migrated_db().await;
        let env = [
            (
                crate::config_vars::DEPLOY_TOKEN_KEY.to_string(),
                "tok".to_string(),
            ),
            (
                "__IMPRESSPRESS_RUNTIME_KIND__".to_string(),
                "server".to_string(),
            ),
            (APP_NAME_KEY.to_string(), "Foo".to_string()),
        ];
        let vars = seed_and_load(&db, &env).await.expect("seed and load");

        assert_eq!(vars.get(crate::config_vars::DEPLOY_TOKEN_KEY), None);
        assert_eq!(vars.get("__IMPRESSPRESS_RUNTIME_KIND__"), None);
        assert_eq!(
            vars.get(APP_NAME_KEY).map(String::as_str),
            Some("Foo"),
            "a legitimate shared key in the same batch still lands"
        );
    }

    /// A boot that re-asserts the same environment writes nothing at all —
    /// the property that keeps env-wins from issuing an UPDATE per declared
    /// key on every cold start.
    #[tokio::test]
    async fn re_applying_the_same_environment_writes_nothing() {
        let db = migrated_db().await;
        let env = [(APP_NAME_KEY.to_string(), "Foo".to_string())];
        seed_and_load(&db, &env).await.expect("first boot");

        let before = crate::config_generation::writes_noted_on_this_thread();
        seed_and_load(&db, &env).await.expect("second boot");
        assert_eq!(
            before,
            crate::config_generation::writes_noted_on_this_thread(),
            "an unchanged environment must not write a row"
        );
    }

    /// `load_all` skips a row whose key is empty (corruption) rather than
    /// inserting an empty key into the map.
    #[tokio::test]
    async fn load_all_skips_a_keyless_row() {
        let db = migrated_db().await;
        seed_if_absent(&db, "A", "1", "", "", false)
            .await
            .expect("seed");
        let mut keyless = NewVariable {
            key: String::new(),
            value: "orphan".to_string(),
            name: String::new(),
            description: String::new(),
            warning: String::new(),
            sensitive: false,
            updated_by: String::new(),
            block: None,
        }
        .into_row();
        keyless.key = String::new();
        db.create(TABLE, keyless.to_data())
            .await
            .expect("raw create");
        let all = load_all(&db).await.expect("load");
        assert_eq!(all.get("A").map(String::as_str), Some("1"));
        assert_eq!(all.len(), 1, "{all:?}");
    }
}
