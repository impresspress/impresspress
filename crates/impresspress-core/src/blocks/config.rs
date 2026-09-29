//! Impresspress config block — the variables table is the config store.
//!
//! Wraps wafer-core's `ConfigBlock` so `info()` and the interface declaration
//! stay single-sourced, and takes over the two operations that decide whether
//! an admin's saved setting is real: `CONFIG_GET` and `CONFIG_SET`.
//!
//! ## Why this block exists
//!
//! wafer-core's block serves both operations from a `ConfigService`, which is
//! a **synchronous** trait (`fn get(&self, key) -> Option<String>`). A
//! synchronous reader cannot consult a database, so every target answered
//! reads from an in-memory map instead:
//!
//! - native seeded an `EnvConfigService` once at boot from the `variables`
//!   table, so an admin's `PATCH /b/admin/api/settings/{key}` — which writes
//!   only that table — was invisible until the process restarted;
//! - Cloudflare's `HashMapConfigService::set` is a no-op, so the five admin
//!   forms behind `ui::settings_form::save_settings` answered
//!   `200 Settings saved` and changed nothing at all.
//!
//! A `Block`'s `handle` is async, which a `ConfigService` is not. That is the
//! whole reason the fix is a block rather than another service implementation:
//! it can await the database without a sync bridge.
//!
//! ## Read order
//!
//! 1. Runtime-owned keys always come from the boot map — see
//!    `served_only_from_boot_map`. Infrastructure keys (`IMPRESSPRESS_*`
//!    without `__`) and internal adapter-injected keys (`__…__`) are never
//!    variables-table config by the repo's naming rules, so a row carrying one
//!    must not be served: the browser's `__IMPRESSPRESS_RUNTIME_KIND__` marker
//!    is what keeps Stripe secret-key operations off in a visitor's browser.
//!    The JWT secret is the named exception — a table row on native, but the
//!    boot map already holds that value, and a row must not rotate the signing
//!    key out from under a running process. `CONFIG_SET` refuses all of them.
//! 2. Otherwise the `variables` row wins when it holds a non-empty value.
//!    This is the actual fix: an admin write lands in the table and the very
//!    next read sees it.
//! 3. Otherwise the boot map answers. It carries what the table cannot —
//!    worker/env bindings, builder-time vars (CORS, CSP, and on Cloudflare
//!    the `STRICT_SCHEMA` worker var) and the synthetic block-settings JSON.
//! 4. Otherwise `NotFound`, exactly as wafer-core's block reports it: the
//!    one answer `config::get_default` / `get_optional` take as "unset".
//!
//! A `CONFIG_GET` that cannot read the table answers from the boot map, so a
//! database blip does not blank every page's branding. That is wrong for a
//! form: a settings page pre-filled from the boot map or the declared
//! defaults, then saved, writes those over the stored values. Forms read
//! through [`get_many`] ([`CONFIG_GET_MANY`]) instead, which follows the same
//! read order but answers an unreadable table with an error.
//!
//! An empty row value deliberately falls through to the boot map rather than
//! masking it. Blank means "unset", not "explicitly blank", everywhere in this
//! repo — the boot seeder skips an empty env value and
//! `admin::settings::seed_defaults` skips an empty declared default for
//! exactly that reason — so a row that ended up blank must not shadow what the
//! boot map holds for the key.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use wafer_block::{codec, wire::config as wire, ResourceAccess, ServiceOp};
use wafer_core::interfaces::{config::service::ConfigService, database::service::DatabaseService};
use wafer_run::{
    context::Context, Block, BlockInfo, ErrorCode, InputStream, Message, OutputStream,
    ResourceType, WaferError,
};

use crate::{
    config_generation::config_write_generation,
    // audit-allow: this block never reaches the table under WRAP — it reads and writes through `variables`' boot-flavour API (`load_all`, `find_by_key`, `set`) over the raw `DatabaseService` that `builder::registration` hands it, the same way `D1ConfigSource` reads this table, so no grant applies (a `ctx`-routed read IS denied: see `the_config_block_reads_the_variables_table_under_wrap`); the audit also derives the caller `impresspress/config` from the file path, while the block registers as `wafer-run/config`
    platform_state::variables,
    util::{is_sensitive_key, validate_config_value},
};

/// Keys the boot map always answers, whatever the variables table holds — and
/// that `CONFIG_SET` refuses, since a stored row for one would never be served.
///
/// Derived from the repo's key-naming conventions rather than listed:
/// infrastructure keys ([`crate::config_vars::is_infrastructure_key`]) and
/// internal adapter-injected keys ([`crate::config_vars::is_internal_key`]) are
/// by definition never variables-table config, so a row carrying one is a
/// mistake or a forgery. That matters: the browser adapter's
/// `__IMPRESSPRESS_RUNTIME_KIND__ = "browser"` is what keeps Stripe secret-key
/// operations off inside a visitor's browser, and a table-first read let a
/// `server` row switch them back on.
///
/// The JWT secret is the one named exception. It IS a table row on native, but
/// the boot map already holds that same value, and a row must not rotate the
/// signing key out from under a running process.
fn served_only_from_boot_map(key: &str) -> bool {
    crate::config_vars::is_instance_owned_key(key)
}

/// The block every config read and write is addressed to.
const CONFIG_BLOCK: &str = "wafer-run/config";

/// Read several keys in the `CONFIG_GET` read order, failing when the
/// `variables` table cannot be read rather than answering from the boot map.
/// An impresspress operation on the config block this module registers;
/// callers go through [`get_many`].
pub const CONFIG_GET_MANY: &str = "impresspress.config.get_many";

/// The interface this block declares: `config@v1`'s two actions plus
/// [`CONFIG_GET_MANY`].
///
/// It has to be its own interface rather than `config@v1`, and that is a
/// runtime rule rather than bookkeeping: `RuntimeContext::dispatch_call`
/// refuses a `call_block` whose action is not in the TARGET's declared
/// interface (`wafer_run::runtime::validation::check_action_interface`), so
/// under `config@v1` — whose action map is exactly `config.get` and
/// `config.set` — [`CONFIG_GET_MANY`] is refused before this block sees it,
/// and every settings page answers 500. Overwriting the platform's
/// `config@v1` spec instead would claim every `config@v1` block serves an
/// action only this one does.
pub const CONFIG_INTERFACE: &str = "impresspress-config@v1";

/// [`CONFIG_INTERFACE`]'s spec: wafer-run's `config@v1` actions under the new
/// name, plus [`CONFIG_GET_MANY`]. Built from `interfaces::config_v1()` so the
/// two `config.*` actions cannot drift from the platform's.
pub fn interface_spec() -> wafer_block::InterfaceSpec {
    let mut spec = wafer_block::interfaces::config_v1();
    spec.name = CONFIG_INTERFACE.to_string();
    spec.description = format!(
        "{} Adds {CONFIG_GET_MANY}, which fails rather than falling back when the variables \
         table cannot be read.",
        spec.description
    );
    spec.actions.insert(
        CONFIG_GET_MANY.to_string(),
        wafer_block::ActionSpec {
            description: "Read several config values by key, failing when the stored values \
                          cannot be read."
                .to_string(),
            message_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "keys": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["keys"]
            })),
            response_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "values": {
                        "type": "object",
                        "additionalProperties": { "type": "string" },
                        "description": "One entry per requested key that has a value; a key \
                                        with none is absent"
                    }
                }
            })),
        },
    );
    spec
}

/// Request for [`CONFIG_GET_MANY`].
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct GetManyRequest {
    keys: Vec<String>,
}

/// Response for [`CONFIG_GET_MANY`]: the keys that have a value. A key that is
/// absent is unset everywhere, and the caller applies its own default.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct GetManyResponse {
    values: HashMap<String, String>,
}

/// The current value of each of `keys` that has one, or an error when the
/// `variables` table could not be read (or the caller may not read a key).
///
/// For a form. A `config.get` that cannot read the table answers from the
/// boot map, and [`wafer_core::clients::config::get_default`] turns a key
/// missing there into the caller's default — right for page chrome and wrong
/// for a form whose Save posts every field back.
pub async fn get_many(
    ctx: &dyn Context,
    keys: &[&str],
) -> Result<HashMap<String, String>, WaferError> {
    let req = GetManyRequest {
        keys: keys.iter().map(|key| (*key).to_string()).collect(),
    };
    let body = codec::encode(&req)?;
    let out = ctx
        .call_block(
            CONFIG_BLOCK,
            Message::new(CONFIG_GET_MANY),
            InputStream::from_bytes(body),
        )
        .await;
    let buf = out.collect_buffered().await.map_err(WaferError::from)?;
    let resp: GetManyResponse = codec::decode(&buf.body).map_err(codec::DecodeError::internal)?;
    Ok(resp.values)
}

/// The `variables` table as a key/value map, shared by every reader holding
/// the snapshot it was built for.
type ConfigRows = Arc<HashMap<String, String>>;

/// A memoized [`ConfigRows`] tagged with the config-write generation it was
/// read at, so a later write makes it visibly stale. See
/// [`VariablesConfigBlock::snapshot`].
type CachedRows = RwLock<Option<(u64, ConfigRows)>>;

/// The config block impresspress registers in place of wafer-core's.
pub struct VariablesConfigBlock {
    /// wafer-core's block, kept for `info()` and any operation this one does
    /// not claim, so a new config op added upstream keeps working here.
    inner: Arc<dyn Block>,
    /// The target's boot map: what the table cannot hold, or must not be
    /// trusted for. Env and worker bindings, builder-time vars (CORS, CSP,
    /// and on Cloudflare the `STRICT_SCHEMA` worker var), the block-settings
    /// JSON, runtime markers, and the JWT secret. Native and the browser do
    /// not copy the variables table into it — this block serves stored
    /// variables from the table itself.
    boot: Arc<dyn ConfigService>,
    /// The platform database, held as the raw service rather than reached
    /// through `ctx`.
    ///
    /// `impresspress__admin__variables` belongs to the ADMIN block, and
    /// `db::list_all` sends the collection as a WRAP resource, so reaching it
    /// through `ctx` is a cross-block read that WRAP denies by default. This
    /// block would then fall back to the boot map and serve compiled-in
    /// defaults on every page — which is exactly what a live Cloudflare
    /// deploy did before this field existed.
    ///
    /// Reading the platform's own config store through the raw service is the
    /// established shape, not a workaround: `variables`' boot-flavour API is
    /// documented as "over `DatabaseService`, before WRAP", and
    /// `D1ConfigSource` reads the same table the same way for the same
    /// reason.
    db: Arc<dyn DatabaseService>,
    /// The `variables` table, memoized alongside the config-write generation
    /// it was read at. See [`Self::snapshot`].
    snapshot: CachedRows,
}

impl VariablesConfigBlock {
    /// Wrap wafer-core's config block over the same boot-seeded service the
    /// builder already constructed.
    pub fn new(boot: Arc<dyn ConfigService>, db: Arc<dyn DatabaseService>) -> Self {
        let inner: Arc<dyn Block> = Arc::new(wafer_core::service_blocks::config::ConfigBlock::new(
            boot.clone(),
        ));
        Self {
            inner,
            boot,
            db,
            snapshot: RwLock::new(None),
        }
    }

    /// The key a `CONFIG_GET` is asking for: the codec-encoded `GetRequest`
    /// body, as wafer-core's config handler reads it. A body that does not
    /// decode is `InvalidArgument` whatever meta the message carries.
    fn get_key(body: &[u8]) -> Result<String, OutputStream> {
        codec::decode::<wire::GetRequest>(body)
            .map(|req| req.key)
            .map_err(|e| {
                OutputStream::error(WaferError::new(
                    ErrorCode::InvalidArgument,
                    format!("config.get: {e}"),
                ))
            })
    }

    /// The whole `variables` table as a key/value map, fetched at most once
    /// per config-write generation.
    ///
    /// One query, not one per key. A single page render reads roughly eight
    /// branding keys through `config::get_default`; a row fetch per key would
    /// turn that into eight D1 round-trips per request on Workers, where CPU
    /// and latency are the scarce resources. `D1ConfigSource::cached_snapshot`
    /// solves the same problem the same way and against the same counter, so
    /// the two invalidate together.
    ///
    /// The generation is captured BEFORE the read: a write landing while the
    /// query is in flight must not be masked by the snapshot it raced. That is
    /// also what makes two workers racing this on native safe. Each stores the
    /// generation its own rows were read at, so the loser leaves behind a
    /// snapshot tagged older than the store is — which costs the next reader a
    /// re-query and can never hand it rows from before a write it should see.
    ///
    /// No lock is held across the `await` — the guard is dropped before the
    /// fetch and re-taken after — so a hard-stopped request cannot strand one.
    ///
    /// A failed read is not cached: the next reader queries again.
    async fn snapshot(&self) -> Result<ConfigRows, WaferError> {
        let generation = config_write_generation();
        {
            let cached = self
                .snapshot
                .read()
                .expect("config snapshot lock poisoned")
                .clone();
            if let Some((cached_generation, rows)) = cached {
                if cached_generation == generation {
                    return Ok(rows);
                }
            }
        }

        let rows = variables::load_all(&self.db)
            .await
            .map_err(|e| WaferError::new(ErrorCode::Unavailable, e))?;
        // `load_all` already returns a key/value map; an empty value falls
        // through to the boot map rather than masking it (see the module docs).
        let map: HashMap<String, String> = rows
            .into_iter()
            .filter(|(_, value)| !value.is_empty())
            .collect();
        let map = Arc::new(map);

        *self
            .snapshot
            .write()
            .expect("config snapshot lock poisoned") = Some((generation, map.clone()));
        Ok(map)
    }

    /// The value a reader of `key` is served, in the module's read order: the
    /// boot map for a runtime-owned key, else a non-empty row, else the boot
    /// map. `Ok(None)` when none holds one; `Err` when the table could not be
    /// read for a key it might hold.
    async fn resolve(&self, key: &str) -> Result<Option<String>, WaferError> {
        if served_only_from_boot_map(key) {
            return Ok(self.boot.get(key));
        }
        let rows = self.snapshot().await?;
        Ok(rows.get(key).cloned().or_else(|| self.boot.get(key)))
    }

    /// [`CONFIG_GET_MANY`]: every requested key the caller may read, resolved
    /// like `CONFIG_GET`, except that an unreadable table is an error.
    async fn get_many_op(&self, ctx: &dyn Context, input: InputStream) -> OutputStream {
        let body = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return OutputStream::error(e),
        };
        let req = match codec::decode::<GetManyRequest>(&body) {
            Ok(req) => req,
            Err(e) => {
                return OutputStream::error(WaferError::new(
                    ErrorCode::InvalidArgument,
                    format!("{CONFIG_GET_MANY}: {e}"),
                ))
            }
        };
        let mut values = HashMap::with_capacity(req.keys.len());
        for key in req.keys {
            if let Err(e) =
                ctx.check_resource_access(&key, ResourceType::Config, ResourceAccess::Read)
            {
                return OutputStream::error(e);
            }
            match self.resolve(&key).await {
                Ok(Some(value)) => {
                    values.insert(key, value);
                }
                Ok(None) => {}
                Err(e) => {
                    return OutputStream::error(WaferError::new(
                        e.code,
                        format!("{CONFIG_GET_MANY} could not read the variables table: {e}"),
                    ))
                }
            }
        }
        match codec::encode(&GetManyResponse { values }) {
            Ok(bytes) => OutputStream::respond(bytes),
            Err(e) => OutputStream::error(e),
        }
    }

    /// Persist `key` and let cached readers know the store moved.
    ///
    /// Writes through the raw service for the same reason reads do: a
    /// `ctx`-routed write to the admin block's table is a cross-block write
    /// WRAP denies.
    async fn write(&self, key: &str, value: &str) -> Result<(), OutputStream> {
        // The same masked-submission, sensitive-empty and `_URL` guards
        // `blocks::admin::ops::update_variable` applies, spelled from the same
        // helpers, so the two write surfaces agree on the RULES.
        //
        // They do not agree on the whole of the sensitive-empty EXEMPTION, and
        // deliberately: this surface exempts only the static
        // `config_vars::is_provisioning_only_key` (the bootstrap password),
        // while the admin PUT calls `ops::is_clearable_provisioning_credential`,
        // which additionally exempts a bootstrap TOKEN that has already been
        // redeemed. So `CONFIG_SET` refuses a clear the admin PUT accepts. The
        // narrower rule is the correct one here and `ops.rs` says why: "is the
        // token redeemed" is answered by counting admin users, and this
        // operation runs over the raw `DatabaseService` with no `Context` to
        // count through. Widening it here would exempt an UNREDEEMED token on
        // the one surface that cannot check, and clearing a live token is a
        // lockout with no way back on Cloudflare.
        //
        // The divergence is not reachable today, and the reason is simpler than
        // it looks. It concerns exactly one key — `BOOTSTRAP_ADMIN_TOKEN`,
        // which the admin path exempts once redeemed and this one never does —
        // and that key is in no `save_settings` allowlist at all. The only
        // settings form carrying bootstrap keys is `auth_ui::pages::settings`,
        // whose "Admin" section is the bootstrap EMAIL and PASSWORD; nothing
        // renders the token. So this caller cannot submit a value for it,
        // empty or otherwise.
        //
        // Note the narrowness: that is an argument about ONE key, not about
        // empty submissions in general. `save_settings` decides sensitivity
        // from the declared var and this function decides it from the stored
        // row, so a declared-plain key whose row an operator flagged sensitive
        // DOES reach the guard below with an empty value and is refused here —
        // which is correct, and which `save_settings` now forwards as the 400
        // it is rather than the 500 it used to flatten it into. A
        // `MASKED_VALUE` submission is handled before it can get here: that
        // caller's pre-pass refuses a mask that would replace a value, for
        // every allowlisted var and not just the ones it can see are sensitive,
        // deliberately covering more than this guard does because it cannot
        // read the flag this one reads.
        //
        // The parity covers the create path too: `variables::set`'s create
        // branch builds its row through `VariablePatch::into_new`, so a
        // `config.set` that creates an undeclared, suffix-less ad hoc key
        // stores it flagged exactly as the admin PUT would — this operation is
        // reachable by any block, and a key nothing declares is one nothing
        // here can vouch for. Note that "declared" has to mean what
        // `config_vars::collect_all_config_vars` says it means —
        // `auth_ui::pages::settings` renders
        // `auth::config::auth_identity_config_vars`, which belongs to no
        // `BlockInfo`, and an earlier version of that collector missed them and
        // so called two ordinary admin toggles ad hoc.
        //
        // The runtime-owned refusal below is deliberately NOT symmetric: this
        // surface refuses the JWT secret (no caller legitimately writes it
        // here — `ui::settings_form` writes declared block and shared vars
        // only), while the admin variables API accepts it, because on native
        // that row IS the next boot's secret. See
        // `blocks::admin::ops::reject_runtime_owned_key`. The
        // sensitive-empty guard reads the stored flag exactly as that path
        // does; a missing row contributes no flag, so for it the guard rests
        // on `is_sensitive_key`'s key half alone — the key's declaration or
        // its `_SECRET`/`_KEY` spelling.
        let existing = match variables::find_by_key(&self.db, key).await {
            Ok(row) => row,
            Err(e) => {
                return Err(OutputStream::error(WaferError::new(
                    ErrorCode::Internal,
                    format!("config.set could not read {key}: {e}"),
                )))
            }
        };
        // The row's own flag, shared by the two guards below: neither of them
        // may decide off the key's spelling alone, or an ad hoc row an admin
        // marked sensitive in the UI would be judged as if it were plain.
        let stored_flag = existing.as_ref().map_or(0, |row| i64::from(row.sensitive));
        // The mask is never a value, on this surface as on the admin ones.
        //
        // This operation is reachable by ANY block through
        // `wafer_core::clients::config::set`, so without the guard here the
        // rule held only because the three surfaces that exist today each
        // enforce it themselves — an invariant that breaks silently the day a
        // block adds a call. Nothing stops it being enforced here: the stored
        // row is already in hand for the empty guard below, which is the only
        // thing the mask check needs. (Contrast the provisioning EXEMPTION
        // discussed above, which genuinely cannot be mirrored here because it
        // asks a question only a `Context` can answer.)
        //
        // Narrowed to a mask that would REPLACE something, for the reason
        // `ui::settings_form`'s pre-pass is: a write that changes nothing
        // destroys nothing, and refusing it only strands whoever is holding a
        // value that already is those eight characters.
        //
        // "Already" has to mean what a READER would answer, which is this
        // block's own read order — a non-empty row, else the boot map — and not
        // the row alone. The pre-pass asks `config::get_default`, which is that
        // order; comparing against `existing.value` here made the two disagree
        // in exactly the case the row cannot speak for: absent or empty, where
        // `CONFIG_GET` drops it and the boot map answers. For a key whose boot
        // value is the mask the pre-pass then allowed and this guard refused,
        // mid-loop, with the rest of the page already written — reachable from
        // an env-seeded credential that is literally `********`, cleared on the
        // Variables page and typed again on a settings form. One question,
        // asked the same way on both sides, is what makes that impossible
        // rather than merely unlikely.
        if crate::util::is_masked_submission(key, stored_flag, value) {
            let boot_value = self.boot.get(key);
            let current = existing
                .as_ref()
                .map(|row| row.value.as_str())
                .filter(|stored| !stored.is_empty())
                .or(boot_value.as_deref())
                .unwrap_or("");
            if current != value {
                return Err(OutputStream::error(WaferError::new(
                    ErrorCode::InvalidArgument,
                    format!(
                        "{} is the mask {key} reads back as, not its value: storing it would \
                         destroy the secret",
                        crate::util::MASKED_VALUE
                    ),
                )));
            }
        }
        // The static provisioning-only exemption — the narrower of the two, per
        // the note above. It has to be here at all for the reason it exists on
        // the admin path: a spent bootstrap password must stay clearable
        // because `delete_variable` and `key_is_deletable` both refuse to
        // delete a declared `WAFER_RUN_SHARED__*` row, so without it the
        // deployment keeps a plaintext admin password by every route.
        if value.is_empty()
            && !crate::config_vars::is_provisioning_only_key(key)
            && is_sensitive_key(key, stored_flag)
        {
            return Err(OutputStream::error(WaferError::new(
                ErrorCode::InvalidArgument,
                format!("Cannot set {key} to an empty value"),
            )));
        }
        if let Err(e) = validate_config_value(key, value) {
            return Err(OutputStream::error(WaferError::new(
                ErrorCode::InvalidArgument,
                format!("Invalid value for {key}: {e}"),
            )));
        }

        // What this asserts ON TOP of the key's own declaration, which
        // `variables::set` and `NewVariable::into_row` settle themselves: an
        // existing row's stored flag, so an ad hoc row an admin marked
        // sensitive in the UI stays that way. Deriving it here from the key's
        // spelling alone is what let a `Password`-typed declared var with no
        // row yet —
        // `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD`, spelled neither
        // `_SECRET` nor `_KEY` — land unflagged, after which the settings API
        // served it verbatim and `cache_key::row_is_sensitive` judged it
        // eligible for the edge cache.
        //
        // An `Option`, and a missing row is `None` rather than `Some(false)`
        // by construction: this surface can speak for a row it just read, and
        // about a key with no row it knows nothing, which is exactly what the
        // create default is for.
        let sensitive = existing.as_ref().map(|row| row.sensitive);
        // Through `set_by_admin`, so the row is stamped admin-owned and
        // `seed_and_load` stops letting the process environment overwrite it.
        // This operation has exactly one caller in the tree —
        // `ui::settings_form::save_settings`, the admin settings forms — so
        // "reaching CONFIG_SET" IS "an admin edited it"; there is no other
        // writer whose intent this could misattribute.
        //
        // The marker rather than a user id because the identity does not reach
        // here: `wafer_core::clients::config::set` builds a fresh message
        // (`svc!`) instead of forwarding the caller's (`svc_msg!`), so
        // `msg.user_id()` in this block is empty on this path. Threading it
        // would be an upstream change; the row only has to record THAT a human
        // owns it, which is exactly what `block_settings` records with the same
        // sentinel.
        if let Err(e) = variables::set_by_admin(
            &self.db,
            key,
            value,
            sensitive,
            crate::features::USER_EDITED_SENTINEL,
        )
        .await
        {
            return Err(OutputStream::error(WaferError::new(
                ErrorCode::Internal,
                format!("config.set could not write {key}: {e}"),
            )));
        }
        // The generation bump lives in the `variables` repo, not here: in
        // `set_with_row` for the write just issued, and in `upsert_by_key` for
        // the one `PATCH /b/admin/api/settings/{key}` issues through
        // `ops::update_variable` without ever entering this block. A bump
        // placed here would cover only this surface and leave a warm snapshot
        // stale for the life of the process on exactly the path the admin uses
        // — see `an_admin_write_invalidates_an_already_warm_snapshot`.
        Ok(())
    }
}

#[wafer_block::wafer_async_trait]
impl Block for VariablesConfigBlock {
    fn info(&self) -> BlockInfo {
        // wafer-core's info, under this block's own interface: it serves one
        // action more than `config@v1` declares. See [`CONFIG_INTERFACE`].
        let mut info = self.inner.info();
        info.interface = CONFIG_INTERFACE.to_string();
        info
    }

    async fn handle(&self, ctx: &dyn Context, msg: Message, input: InputStream) -> OutputStream {
        match msg.kind.as_str() {
            ServiceOp::CONFIG_GET => {
                let body = match input.collect_to_bytes().await {
                    Ok(bytes) => bytes,
                    Err(e) => return OutputStream::error(e),
                };
                let key = match Self::get_key(&body) {
                    Ok(key) => key,
                    Err(out) => return out,
                };
                if let Err(e) =
                    ctx.check_resource_access(&key, ResourceType::Config, ResourceAccess::Read)
                {
                    return OutputStream::error(e);
                }

                let value = match self.resolve(&key).await {
                    Ok(value) => value,
                    Err(e) => {
                        // Reported rather than propagated: a config read that
                        // cannot reach the table falls back to the boot map,
                        // which is what the process was already serving.
                        // Failing instead would turn a transient database blip
                        // into a blank site. A form must not be filled from
                        // this; it reads through `CONFIG_GET_MANY`.
                        tracing::warn!(
                            error = %e,
                            "config read could not reach the variables table; falling back to the boot map"
                        );
                        self.boot.get(&key)
                    }
                };

                value.map_or_else(
                    || {
                        OutputStream::error(WaferError::new(
                            ErrorCode::NotFound,
                            format!("config key not found: {key}"),
                        ))
                    },
                    |value| match codec::encode(&wire::GetResponse { value }) {
                        Ok(bytes) => OutputStream::respond(bytes),
                        Err(e) => OutputStream::error(e),
                    },
                )
            }
            CONFIG_GET_MANY => self.get_many_op(ctx, input).await,
            ServiceOp::CONFIG_SET => {
                let body = match input.collect_to_bytes().await {
                    Ok(bytes) => bytes,
                    Err(e) => return OutputStream::error(e),
                };
                let req = match codec::decode::<wire::SetRequest>(&body) {
                    Ok(req) => req,
                    Err(e) => {
                        return OutputStream::error(WaferError::new(
                            ErrorCode::InvalidArgument,
                            format!("config.set: {e}"),
                        ))
                    }
                };
                if let Err(e) =
                    ctx.check_resource_access(&req.key, ResourceType::Config, ResourceAccess::Write)
                {
                    return OutputStream::error(e);
                }
                // A runtime-owned key is never served from the table, so
                // storing one would report success for a value no reader can
                // ever see. Refuse it instead of writing an unservable row.
                if served_only_from_boot_map(&req.key) {
                    return OutputStream::error(WaferError::new(
                        ErrorCode::InvalidArgument,
                        format!(
                            "{} is set by the runtime, not stored config; it cannot be written",
                            req.key
                        ),
                    ));
                }
                match self.write(&req.key, &req.value).await {
                    Ok(()) => OutputStream::respond(vec![]),
                    Err(out) => out,
                }
            }
            _ => self.inner.handle(ctx, msg, input).await,
        }
    }
}

/// Register impresspress's config block under the name every caller uses.
///
/// Replaces `wafer_core::service_blocks::config::register_with`, which binds
/// the same name to a block serving both operations from the in-memory map.
pub fn register_with(
    wafer: &mut wafer_run::Wafer,
    boot: Arc<dyn ConfigService>,
    db: Arc<dyn DatabaseService>,
) -> Result<(), wafer_run::RuntimeError> {
    let block: Arc<dyn Block> = Arc::new(VariablesConfigBlock::new(boot, db));
    // The spec for the interface `info()` declares. `dispatch_call` validates
    // a call's action against it, and an interface name with no spec only
    // warns and lets every action through. Both registrations are snapshotted
    // at `seal()`, so their order here does not matter.
    wafer.register_interface(interface_spec());
    wafer.register_block(CONFIG_BLOCK, block)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config_vars::APP_NAME_KEY,
        platform_state::variables::VariablePatch,
        test_support::{unique_config_value, TestContext},
    };

    /// `CONFIG_GET_MANY` reads what `CONFIG_GET` reads — a stored row, else
    /// the boot map — and leaves out a key nothing holds, so the caller's
    /// default applies.
    #[tokio::test]
    async fn get_many_reads_rows_over_the_boot_map_and_omits_unset_keys() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config("X__BOOT_ONLY", "from-boot");
        let stored = unique_config_value();
        wafer_core::clients::config::set(&ctx.fixture(), APP_NAME_KEY, &stored)
            .await
            .expect("store a row");

        let values = get_many(&ctx, &[APP_NAME_KEY, "X__BOOT_ONLY", "X__UNSET"])
            .await
            .expect("a healthy read");

        assert_eq!(values.get(APP_NAME_KEY), Some(&stored));
        assert_eq!(
            values.get("X__BOOT_ONLY").map(String::as_str),
            Some("from-boot")
        );
        assert!(!values.contains_key("X__UNSET"), "{values:?}");
    }

    /// `config.get` takes its key from the body only, as wafer-core's config
    /// handler does. A `key` meta beside a body that does not decode used to
    /// be served — a second request shape the typed client never sends, and
    /// one wafer-core dropped — so it is refused here too.
    #[tokio::test]
    async fn a_get_whose_body_does_not_decode_is_refused_whatever_its_meta() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config(APP_NAME_KEY, "from-boot");
        let mut msg = Message::new(ServiceOp::CONFIG_GET);
        msg.set_meta("key", APP_NAME_KEY);
        let out = ctx
            .call_block(CONFIG_BLOCK, msg, InputStream::empty())
            .await;
        assert!(
            crate::test_support::output_is_error(out, "InvalidArgument").await,
            "an undecodable config.get must be InvalidArgument"
        );
    }

    /// Where `CONFIG_GET` falls back to the boot map on an unreadable table,
    /// `CONFIG_GET_MANY` fails: a form must not be filled from the fallback.
    /// The `CONFIG_GET` half is a guard on the fallback this keeps.
    #[tokio::test]
    async fn get_many_fails_where_config_get_falls_back() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config(APP_NAME_KEY, "from-boot");
        wafer_core::clients::config::set(&ctx.fixture(), APP_NAME_KEY, "stored")
            .await
            .expect("store a row");
        let ctx = ctx.break_reads();

        assert_eq!(
            wafer_core::clients::config::get_default(&ctx, APP_NAME_KEY, "")
                .await
                .expect("config read"),
            "from-boot"
        );
        assert!(get_many(&ctx, &[APP_NAME_KEY]).await.is_err());
    }

    /// An admin write must be visible to a reader whose snapshot is ALREADY
    /// warm.
    ///
    /// The reproduction tests in `blocks::admin::settings` and
    /// `ui::settings_form` both read for the first time after writing, so the
    /// snapshot is built fresh and they pass however invalidation behaves.
    /// A real server reads config while serving its first request and keeps
    /// the snapshot; if an admin's `PATCH` does not invalidate it, that
    /// write is invisible for the life of the process — the original defect,
    /// reintroduced by the cache that was supposed to make the fix affordable.
    ///
    /// The automatic bump lives in `impresspress-cloudflare`'s `kv_cached_db`
    /// and so covers only that target. This asserts the behaviour on native,
    /// where the `variables` repo has to supply it.
    #[tokio::test]
    async fn an_admin_write_invalidates_an_already_warm_snapshot() {
        const KEY: &str = crate::config_vars::PRIMARY_COLOR_KEY;

        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx.fixture())
            .await
            .expect("apply admin migrations");
        ctx.boot_config_service().await;

        // Warm the snapshot, the way serving one request does.
        let first = unique_config_value();
        variables::upsert_by_key(
            &ctx.fixture(),
            KEY,
            VariablePatch {
                value: Some(first.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("seed the first value");
        assert_eq!(
            wafer_core::clients::config::get_default(&ctx, KEY, "unset")
                .await
                .expect("config read"),
            first,
            "precondition: the first read populates the snapshot"
        );

        // Now a write lands that did NOT go through this block — which is
        // what `PATCH /b/admin/api/settings/{key}` does: `handle_set` calls
        // `ops::update_variable`, which calls exactly this. The repo write is
        // the shared choke point, so asserting on it covers the admin
        // endpoint and every other direct writer at once.
        let second = unique_config_value();
        variables::upsert_by_key(
            &ctx,
            KEY,
            VariablePatch {
                value: Some(second.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("the admin write lands in the table");

        assert_eq!(
            wafer_core::clients::config::get_default(&ctx, KEY, "unset")
                .await
                .expect("config read"),
            second,
            "an admin write must invalidate a warm config snapshot, or the \
             change stays invisible for the life of the process"
        );
    }

    /// The same requirement when the write and the read happen on DIFFERENT
    /// threads, which is the only shape native ever serves.
    ///
    /// `impresspress`'s `#[tokio::main]` runtime is multi-threaded (tokio
    /// "full") and the wafer-run http listener serves through `axum::serve`,
    /// which drives a task per connection, so the worker that handles an
    /// admin's `PATCH /b/admin/api/settings/{key}` is routinely not the worker
    /// that renders the next page. This block is registered once and
    /// its snapshot is shared by every one of them, so the generation the
    /// snapshot is tagged with has to be shared too. While that counter was
    /// `thread_local`, the write bumped only the writing thread: a reader on a
    /// worker whose own counter still equalled the tag served the pre-write
    /// value for the life of the process, and — since the tag is whichever
    /// worker refilled the snapshot last — two workers that disagreed about
    /// the count discarded and refetched each other's snapshot for as long as
    /// reads kept alternating between them.
    ///
    /// Every other test here runs under `#[tokio::test]`, which is
    /// current-thread — one thread both writes and reads — which is why a suite
    /// this size never saw it.
    ///
    /// The reader runs in a spawned task, so it runs on a tokio WORKER thread;
    /// the write below runs in the test body, which `block_on` polls on the
    /// runtime's own thread. No worker can therefore have observed the write
    /// thread-locally, whichever worker the reader resumes on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_admin_write_on_another_worker_reaches_a_warm_snapshot() {
        const KEY: &str = crate::config_vars::PRIMARY_COLOR_KEY;

        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx.fixture())
            .await
            .expect("apply admin migrations");
        ctx.boot_config_service().await;

        let first = unique_config_value();
        variables::upsert_by_key(
            &ctx.fixture(),
            KEY,
            VariablePatch {
                value: Some(first.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("seed the first value");

        let (warmed_tx, warmed_rx) = tokio::sync::oneshot::channel();
        let (written_tx, written_rx) = tokio::sync::oneshot::channel();
        let reader_ctx = ctx.clone();
        let first_expected = first.clone();
        let reader = tokio::spawn(async move {
            let warm = wafer_core::clients::config::get_default(&reader_ctx, KEY, "unset")
                .await
                .expect("config read");
            assert_eq!(
                warm, first_expected,
                "precondition: this read fills the shared snapshot on a worker thread"
            );
            warmed_tx.send(()).expect("the test is waiting for this");
            written_rx.await.expect("the admin write happens");
            wafer_core::clients::config::get_default(&reader_ctx, KEY, "unset")
                .await
                .expect("config read")
        });
        warmed_rx.await.expect("the reader warmed the snapshot");

        let second = unique_config_value();
        variables::upsert_by_key(
            &ctx,
            KEY,
            VariablePatch {
                value: Some(second.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("the admin write lands in the table");
        written_tx.send(()).expect("the reader is waiting for this");

        assert_eq!(
            reader.await.expect("the reader task finished"),
            second,
            "a config write on one thread must invalidate the snapshot every \
             other thread reads, or native serves the pre-write value until it \
             restarts"
        );
    }

    /// The login page shows branding an admin saved, without a restart.
    ///
    /// This is the requirement read surface 2 stood for. It could not be
    /// asserted while `blocks::auth_ui::pages::site_config` read
    /// `ctx.config_get`: that snapshot is filled at boot and never refilled,
    /// so the assertion would have failed however correct the config store
    /// was. Step 3 of the decision moved those reads onto the async client,
    /// which is what makes this expressible at all.
    ///
    /// Asserted through `SiteConfig::load_for_auth` — what the login, signup,
    /// bootstrap, change-password, reset-password and verify pages all build
    /// their chrome from — rather than by rendering one page, so it covers
    /// every one of them.
    #[tokio::test]
    async fn the_auth_pages_show_branding_saved_after_boot() {
        const KEY: &str = crate::config_vars::PRIMARY_COLOR_KEY;

        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx.fixture())
            .await
            .expect("apply admin migrations");
        ctx.boot_config_service().await;

        // Boot is over; the snapshot the auth pages used to read is now fixed.
        let saved = unique_config_value();
        variables::upsert_by_key(
            &ctx.fixture(),
            KEY,
            VariablePatch {
                value: Some(saved.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("the admin write lands in the table");

        // Read as auth-ui, the block whose pages build their chrome from it.
        let site = crate::ui::SiteConfig::load_for_auth(
            &ctx.clone()
                .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID),
        )
        .await
        .expect("site config");
        assert_eq!(
            site.primary_color, saved,
            "the auth pages must render the brand colour an admin saved, not \
             the one that happened to be in the table when the process booted"
        );
    }

    /// The config block must be able to read the variables table under WRAP.
    ///
    /// Found by deploying to a live Cloudflare Worker, not by any test here.
    /// `impresspress__admin__variables` belongs to the ADMIN block, and
    /// `db::list_all` sends the collection as a WRAP resource
    /// (`svc!(.., Some(collection), .., Some("db"))`). A cross-block read is
    /// denied by default, so on a runtime that enforces WRAP this block's
    /// table read failed, it fell back to the boot map, and every page served
    /// the compiled-in default — the exact defect the fix was meant to remove,
    /// still live, with a green test suite behind it.
    ///
    /// The suite was green because `TestContext` did not enforce WRAP on the
    /// frame a test ran in — the same blind spot that let the files block
    /// ship a `wafer-run/crypto` call it had not declared. This test runs as
    /// the config block, which is refused what production refuses it.
    #[tokio::test]
    async fn the_config_block_reads_the_variables_table_under_wrap() {
        const KEY: &str = crate::config_vars::PRIMARY_COLOR_KEY;

        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx.fixture())
            .await
            .expect("apply admin migrations");
        ctx.boot_config_service().await;

        let saved = unique_config_value();
        variables::upsert_by_key(
            &ctx.fixture(),
            KEY,
            VariablePatch {
                value: Some(saved.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("seed the value");

        // Act as the config block itself, on the same gates production
        // applies: the admin block owns the table, and grants the config
        // block nothing on it.
        let ctx = ctx.running_as("wafer-run/config");

        assert_eq!(
            wafer_core::clients::config::get_default(&ctx, KEY, "unset")
                .await
                .expect("config read"),
            saved,
            "the config block must read the variables table under WRAP; if it \
             cannot it falls back to the boot map and every page serves the \
             compiled-in default while the admin's saved value sits in the table"
        );
    }
}

/// Keys the runtime sets must not be overridable from the variables table.
#[cfg(test)]
mod boot_owned_key_tests {
    use super::*;
    use crate::{platform_state::variables::NewVariable, test_support::TestContext};

    /// A key whose `_SECRET` suffix makes it sensitive, and which no block
    /// declares.
    const SENSITIVE_UNDECLARED_KEY: &str = "WAFER_RUN_SHARED__AUTH__OAUTH_GOOGLE_CLIENT_SECRET";

    async fn booted_with(adapter_values: &[(&str, &str)]) -> TestContext {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx.fixture())
            .await
            .expect("apply admin migrations");
        ctx.boot_config_service_with(adapter_values).await;
        ctx
    }

    async fn store_row(ctx: &TestContext, key: &str, value: &str) {
        variables::insert(
            &ctx.fixture(),
            NewVariable {
                key: key.to_string(),
                value: value.to_string(),
                name: String::new(),
                description: String::new(),
                warning: String::new(),
                sensitive: false,
                updated_by: String::new(),
                block: variables::block_for_key(key),
            },
        )
        .await
        .expect("store the row");
    }

    /// The browser adapter's runtime marker must beat a variables row.
    ///
    /// `products::RUNTIME_KIND_CONFIG_KEY` is documented as set by the browser
    /// adapter "after loading persisted variables, so an admin database value
    /// cannot accidentally turn a public browser runtime into a trusted
    /// secret holder". `products::stripe_secret_operations_allowed` reads it
    /// off the `config_get` snapshot, but this block serves it too, so a
    /// config-client read must not answer a table row either: answered
    /// table-first, a row holding `server` under that key would tell any such
    /// reader it runs on a server inside a visitor's browser — a row that the
    /// admin variables API accepts for any key, and that a dev-sandbox data
    /// import can carry.
    #[cfg(feature = "block-products")]
    #[tokio::test]
    async fn a_table_row_cannot_override_an_internal_adapter_key() {
        let key = crate::blocks::products::RUNTIME_KIND_CONFIG_KEY;
        let ctx = booted_with(&[(key, "browser")]).await;
        store_row(&ctx, key, "server").await;

        assert_eq!(
            wafer_core::clients::config::get_default(&ctx, key, "server").await.expect("config read"),
            "browser",
            "an adapter-injected internal key must come from the boot map, never the variables table"
        );
    }

    /// Infrastructure keys follow the same rule: `IMPRESSPRESS_*` without a
    /// `__` separator is "infrastructure, never in DB" by the repo's naming
    /// convention, so a row carrying one must not be what a reader sees.
    #[tokio::test]
    async fn a_table_row_cannot_override_an_infrastructure_key() {
        let key = crate::migration_helper::RUN_MIGRATIONS_KEY;
        let ctx = booted_with(&[(key, "1")]).await;
        store_row(&ctx, key, "0").await;

        assert_eq!(
            wafer_core::clients::config::get_default(&ctx, key, "unset")
                .await
                .expect("config read"),
            "1",
            "an infrastructure key must come from the boot map, never the variables table"
        );
    }

    /// A write to a runtime-owned key is refused, not silently stored.
    ///
    /// The read side never serves such a row, so accepting the write would
    /// report success for a value no reader can ever see — the silent no-op
    /// this block exists to remove.
    #[tokio::test]
    async fn config_set_refuses_a_runtime_owned_key() {
        let key = crate::features::BLOCK_SETTINGS_CONFIG_KEY;
        let ctx = booted_with(&[(key, "{}")]).await;

        let result = wafer_core::clients::config::set(&ctx, key, r#"{"forged":true}"#).await;
        assert!(
            result.is_err(),
            "CONFIG_SET of a runtime-owned key must fail rather than store an unservable row"
        );
        assert!(
            variables::get_by_key(&ctx.fixture(), key)
                .await
                .expect("read back")
                .is_none(),
            "a refused write must not leave a row behind"
        );
    }

    /// `CONFIG_SET` refuses a session lifetime past its bound, like the admin
    /// write surfaces: the value would fail every login
    /// (`auth::helpers::session_lifetime_days`).
    #[tokio::test]
    async fn config_set_refuses_an_out_of_range_session_lifetime() {
        let key = crate::blocks::auth::config::SESSION_LIFETIME_DAYS_KEY;
        let ctx = booted_with(&[]).await;

        let result = wafer_core::clients::config::set(&ctx, key, "100000000").await;
        assert!(
            result.is_err(),
            "CONFIG_SET of an out-of-range session lifetime must fail"
        );
        assert!(
            variables::get_by_key(&ctx.fixture(), key)
                .await
                .expect("read back")
                .is_none(),
            "a refused write must not leave a row behind"
        );

        wafer_core::clients::config::set(&ctx, key, "30")
            .await
            .expect("an in-range lifetime is stored");
    }

    /// `CONFIG_SET` refuses the mask, like every other write surface.
    ///
    /// This is the FOURTH writer into the `variables` table — any block can
    /// reach it through `wafer_core::clients::config::set` — and it is the one
    /// that makes the claim in `util::is_masked_submission` true by
    /// construction rather than by accident of who happens to call what today.
    /// Without it the guard held only because the two admin surfaces and
    /// `ui::settings_form` all enforce it themselves, which is the kind of
    /// invariant that breaks silently the day a block adds a call.
    #[tokio::test]
    async fn config_set_refuses_the_mask_for_a_sensitive_key() {
        const KEY: &str = SENSITIVE_UNDECLARED_KEY;
        let ctx = booted_with(&[]).await;
        store_row(&ctx, KEY, "real-client-secret").await;

        let result = wafer_core::clients::config::set(&ctx, KEY, crate::util::MASKED_VALUE).await;
        assert!(
            result.is_err(),
            "storing the mask over a secret must fail, not report success"
        );
        assert_eq!(
            variables::get_by_key(&ctx, KEY)
                .await
                .expect("read back")
                .expect("the row is still there")
                .value,
            "real-client-secret",
            "and the stored secret must survive the refusal"
        );
    }

    /// The mask refusal is for a mask that REPLACES something. A row already
    /// holding those eight characters is a no-op write, and refusing it would
    /// strand whoever holds such a row — and, since
    /// `ui::settings_form`'s pre-pass allows a submission equal to the current
    /// value, would do it mid-loop with part of the page already saved.
    #[tokio::test]
    async fn config_set_allows_the_mask_when_it_replaces_nothing() {
        const KEY: &str = SENSITIVE_UNDECLARED_KEY;
        let ctx = booted_with(&[]).await;
        store_row(&ctx, KEY, crate::util::MASKED_VALUE).await;

        wafer_core::clients::config::set(&ctx, KEY, crate::util::MASKED_VALUE)
            .await
            .expect("a write that changes nothing must not be refused");
        assert_eq!(
            variables::get_by_key(&ctx, KEY)
                .await
                .expect("read back")
                .expect("the row is still there")
                .value,
            crate::util::MASKED_VALUE,
        );
    }

    /// A row this operation CREATES for a key nothing declares is stored
    /// sensitive, exactly as `PATCH /b/admin/api/settings/{key}` would store
    /// it.
    ///
    /// `CONFIG_SET` is reachable by any block through
    /// `wafer_core::clients::config::set`, and about an undeclared,
    /// suffix-less key the build knows nothing — so the create default has to
    /// be the protective one on this surface too. It used to build its own
    /// `NewVariable`, which raises the flag only from the declaration or the
    /// `_SECRET`/`_KEY` spelling, so the same ad hoc key was masked when
    /// created through the admin API and published when created here.
    #[tokio::test]
    async fn config_set_creating_an_undeclared_key_stores_it_sensitive() {
        const KEY: &str = "WAFER_RUN_SHARED__MY_SERVICE_TOKEN";
        let ctx = booted_with(&[]).await;

        wafer_core::clients::config::set(&ctx, KEY, "ad-hoc-value")
            .await
            .expect("an undeclared key is storable config");

        assert!(
            variables::get_by_key(&ctx, KEY)
                .await
                .expect("read back")
                .expect("the row was created")
                .sensitive,
            "a key no `ConfigVar` declares must be created flagged: nothing here \
             knows what it holds, and the admin PUT already protects it"
        );
    }

    /// …and the default does not spill onto a key the build DOES know is
    /// plain: a declared, non-`Password` var is still created unflagged, so it
    /// stays readable in the settings API and exportable in a seed bundle.
    #[tokio::test]
    async fn config_set_creating_a_declared_plain_key_stores_it_unflagged() {
        const KEY: &str = crate::config_vars::APP_NAME_KEY;
        let ctx = booted_with(&[]).await;

        wafer_core::clients::config::set(&ctx, KEY, "Acme")
            .await
            .expect("a declared shared var is storable config");

        assert!(
            !variables::get_by_key(&ctx, KEY)
                .await
                .expect("read back")
                .expect("the row was created")
                .sensitive,
            "a declared plain var must not be masked by the ad hoc default"
        );
    }

    /// Ordinary shared keys are unaffected: the table still wins.
    #[tokio::test]
    async fn a_shared_key_is_still_served_from_the_table() {
        const KEY: &str = crate::config_vars::APP_NAME_KEY;
        let ctx = booted_with(&[(KEY, "boot-value")]).await;
        let saved = crate::test_support::unique_config_value();
        store_row(&ctx, KEY, &saved).await;

        assert_eq!(
            wafer_core::clients::config::get_default(&ctx, KEY, "unset")
                .await
                .expect("config read"),
            saved,
            "an admin-editable shared key must still come from the variables table"
        );
    }
}
