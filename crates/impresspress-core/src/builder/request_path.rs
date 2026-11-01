//! The hosted target's boot funnels: the deploy funnel that provisions a
//! database, the two request-path builds that serve from one, and the closing
//! sequence that seals a deploy into a prepared plan.
//!
//! These are Cloudflare's paths, and they live here rather than in
//! `impresspress-cloudflare` because nothing in them is Cloudflare's: every
//! step is a core call over a `DatabaseService`, a `ConfigService` and the
//! router's `BlockSettings` handle. What the Worker adds — its D1/KV/R2
//! bindings and the request-scoped service proxies a cached runtime forwards
//! through — wraps these calls rather than living inside them. Having them
//! here is what lets a native test drive the very funnels a Worker runs, over
//! a database that counts what it is sent (`tests/d1_read_cost.rs`), instead
//! of a copy that drifts.
//!
//! Picking a funnel is picking a function, never composing a
//! `(grants, hooks, policy)` triple at a call site:
//!
//! - [`boot_deploy_runtime`]: grants from the database, [`DeployBootHooks`],
//!   [`InitPolicy::Reported`]. The one path that seeds.
//! - [`boot_request_runtime`] with [`RequestBuild::Database`]: grants from the
//!   database, [`RequestBootHooks`] (read, republish, write nothing).
//! - [`boot_request_runtime`] with [`RequestBuild::Prepared`]: grants already
//!   installed from a verified plan, [`PreparedPlanBootHooks`] (a written
//!   no-op).
//!
//! The two request builds share one policy, [`REQUEST_PATH_POLICY`], which is
//! a constant rather than an argument: there is exactly one request-path
//! policy, so a parameter would only let two call sites disagree about it.
//!
//! [`seal_prepared_plan`] is `/_deploy/prepare`'s closing sequence — the last
//! reads a deploy makes before its plan is exported.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use wafer_core::interfaces::{config::service::ConfigService, database::service::DatabaseService};
use wafer_run::{RuntimeError, Wafer};

use super::{
    boot, BootHooks, BootReport, GrantSource, InitPolicy, PreparedPlanExporter, RuntimeConfig,
};
use crate::features::BlockSettings;

/// The policy both request-path builds boot under.
///
/// `Strict`: a failure fails the build rather than publishing a runtime with a
/// failed lazy-init slot. Concurrent requests would then wait on one another's
/// init future, which is not a valid execution model for a request-isolated
/// platform.
///
/// A hook failure is fatal too, and for [`RequestBootHooks`] that is the
/// right coupling rather than an accident of the policy: its only failure is
/// an operational failure of the same `block_settings` read the build already
/// made and already treats as fatal (see
/// [`crate::platform_state::block_settings::load`] for what it tolerates).
/// Serving requests off a half-known enablement map is exactly what that read
/// fails closed to prevent.
const REQUEST_PATH_POLICY: InitPolicy = InitPolicy::Strict;

/// Which request-path build a runtime is: the only thing a request-path
/// caller chooses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestBuild {
    /// A runtime built from the database: grants and `block_settings` are
    /// read from it, and [`RequestBootHooks`] re-reads the settings after
    /// admin's `Init`.
    Database,
    /// A runtime hydrated from a verified prepared plan:
    /// `ImpresspressBuilder::apply_prepared_plan` installed the plan's grants
    /// and settings before `build()`, so nothing structural is read here.
    Prepared,
}

/// Run the post-build lifecycle over a **request-path** runtime.
///
/// - [`RequestBuild::Database`]: grants come from `db`, and the hook is
///   [`RequestBootHooks`], which re-reads `block_settings` and republishes
///   it, and **writes nothing**. A request is not a deploy; see that type for
///   the three failure modes a seeding hook has here that it does not have in
///   the deploy funnel.
/// - [`RequestBuild::Prepared`]: grants are `PreInstalled` — the builder
///   copied `structure.wrap_grants` and `structure.deployment_wrap_grants`
///   out of the verified plan before `build()`, so reading `db` would answer
///   the same question a second time, over the network — and the hook is
///   [`PreparedPlanBootHooks`]. `db`, `block_settings` and `config` are then
///   unused, which is the point: this path performs **no** structural read.
///
/// Both boot under [`REQUEST_PATH_POLICY`].
pub async fn boot_request_runtime(
    wafer: &mut Wafer,
    db: &Arc<dyn DatabaseService>,
    block_settings: &Arc<RwLock<BlockSettings>>,
    config: Arc<dyn ConfigService>,
    build: RequestBuild,
) -> Result<BootReport, RuntimeError> {
    match build {
        RequestBuild::Database => {
            let hooks = RequestBootHooks {
                db: db.clone(),
                block_settings_handle: block_settings.clone(),
                config,
            };
            boot(
                wafer,
                &hooks,
                GrantSource::Database(db),
                REQUEST_PATH_POLICY,
            )
            .await
        }
        RequestBuild::Prepared => {
            boot(
                wafer,
                &PreparedPlanBootHooks,
                GrantSource::PreInstalled(
                    "ImpresspressBuilder::apply_prepared_plan installs the verified \
                     plan's wrap_grants and deployment_wrap_grants before build()",
                ),
                REQUEST_PATH_POLICY,
            )
            .await
        }
    }
}

/// Run the post-build lifecycle over the **deploy funnel's** runtime
/// (`/_deploy/init` and `/_deploy/prepare`, reached by a production deploy and
/// by local `impresspress serve --target cloudflare`).
///
/// - **Grants come from `db`**, as on every database-built runtime.
/// - **The seeding hook runs here and only here.** Seeding — structural
///   `block_settings` rows and auto-generated secrets — is a deploy-time
///   mutation, and this is the deploy. See [`DeployBootHooks`].
/// - **`InitPolicy::Reported`**, because a deploy funnel's whole product is
///   the per-step report; it captures every outcome instead of aborting on
///   the first failure.
///
/// The caller's runtime must carry `RUN_MIGRATIONS_KEY = "1"` on its config
/// snapshot: that, not this function, is what makes each block's `Init`
/// apply its pending list.
pub async fn boot_deploy_runtime(
    wafer: &mut Wafer,
    db: &Arc<dyn DatabaseService>,
    block_settings: &Arc<RwLock<BlockSettings>>,
    config: Arc<dyn ConfigService>,
) -> Result<BootReport, RuntimeError> {
    let hooks = DeployBootHooks::new(db.clone(), block_settings.clone(), config);
    boot(
        wafer,
        &hooks,
        GrantSource::Database(db),
        InitPolicy::Reported,
    )
    .await
}

/// `/_deploy/prepare`'s closing sequence, run after a successful
/// [`boot_deploy_runtime`]: re-read `block_settings` and the WRAP grants once
/// every block has initialized — blocks stamp their migration state during
/// `Init`, after the seed hook published its snapshot — and publish both into
/// `exporter`, which the caller then turns into the plan.
///
/// These are the last reads a deploy makes, and the plan they seal is what
/// every prepared cold build hydrates from without reading either table. So
/// either read failing fails the prepare: a plan sealed over a failed grants
/// read would carry no admin-created grants into every runtime built from it.
pub async fn seal_prepared_plan(
    db: &Arc<dyn DatabaseService>,
    exporter: &PreparedPlanExporter,
) -> Result<(), Box<dyn std::error::Error>> {
    let final_settings = crate::platform_state::block_settings::load(db).await?;
    let final_grants = crate::platform_state::wrap_grants::load(db).await?;
    exporter.publish_block_settings(final_settings)?;
    exporter.publish_wrap_grants(&final_grants)?;
    Ok(())
}

/// [`BootHooks`] for the deploy funnel's runtime — the one hosted path that
/// **seeds**.
///
/// Every build loads `block_settings` read-only before `build()` (the router
/// needs its enablement map up front); this hook additionally seeds structural
/// defaults after admin's migration has created the table, then publishes the
/// resulting settings onto both config surfaces before any other block
/// initializes. The shared auto-generated-secret pass also belongs after
/// admin migration 002 has added the `variables.block` column.
///
/// Only [`boot_deploy_runtime`] constructs one; it is public so a target's
/// own tests can drive [`Self::seed_and_load`] over that target's database
/// wrapper. See [`RequestBootHooks`] for what the request paths get instead
/// and why the difference is load-bearing.
pub struct DeployBootHooks {
    db: Arc<dyn DatabaseService>,
    block_settings_handle: Arc<RwLock<BlockSettings>>,
    /// The forwarder the seeded settings are published through, onto the
    /// async surface as well as the snapshot. Cloudflare's concrete
    /// `ConfigService` is map-backed and its `set` is a documented no-op, so
    /// there only the snapshot actually moves — that is a property of the
    /// service, not a reason to write one surface and skip the other.
    config: Arc<dyn ConfigService>,
    /// The `(block_name, default_enabled)` set this funnel seeds towards,
    /// held as data rather than called for inline. [`RequestBootHooks`] has
    /// no such field and that is the point: the seeding path is the one that
    /// has something to seed towards, so re-introducing a request-path seed
    /// means visibly giving the request hook a defaults set, not quietly
    /// swapping one function call for another.
    seed_defaults: Vec<(String, bool)>,
}

impl DeployBootHooks {
    /// The deploy hooks over `db`, seeding towards
    /// [`crate::blocks::block_enabled_defaults`].
    pub fn new(
        db: Arc<dyn DatabaseService>,
        block_settings_handle: Arc<RwLock<BlockSettings>>,
        config: Arc<dyn ConfigService>,
    ) -> Self {
        Self::seeding_towards(
            db,
            block_settings_handle,
            config,
            crate::blocks::block_enabled_defaults(),
        )
    }

    /// The deploy hooks seeding towards an explicit defaults set — for a test
    /// whose build's real set would be empty (a build with no `can_disable`
    /// block seeds nothing, for a reason unrelated to what it asserts).
    pub fn seeding_towards(
        db: Arc<dyn DatabaseService>,
        block_settings_handle: Arc<RwLock<BlockSettings>>,
        config: Arc<dyn ConfigService>,
        seed_defaults: Vec<(String, bool)>,
    ) -> Self {
        Self {
            db,
            block_settings_handle,
            config,
            seed_defaults,
        }
    }

    /// The database half of [`BootHooks::seed_after_admin_init`]: the
    /// auto-generated secret pass and the hash-gated structural seed, both of
    /// which write. Public because `seed_after_admin_init` needs a
    /// `&mut Wafer` that a target's wrapper tests cannot produce, and what
    /// those tests are about is the writes.
    pub async fn seed_and_load(&self) -> Result<BlockSettings, String> {
        crate::platform_state::variables::seed_auto_generated(&self.db).await;

        // The hosted target's only call site for the sensitive-flag repair.
        // Cloudflare never runs `variables::seed_and_load` — it has no process
        // environment to seed from, so this hook is the whole of its variables
        // seeding — and the repair therefore has to be named here rather than
        // inherited. Without it a row an older build stored unflagged (a
        // bootstrap credential is the likely one) keeps being served in the
        // clear by `GET /b/admin/api/settings/{key}` and stays eligible for the
        // KV cache via `cache_key::row_is_sensitive`.
        //
        // Here and not in `RequestBootHooks`: that hook is write-free by
        // invariant (see its doc — a write there self-invalidates the fleet's
        // config version and races concurrent isolates). The rows this repairs
        // are legacy, so deploy time is both sufficient and the only safe slot.
        crate::platform_state::variables::repair_sensitive_flags(&self.db).await;

        crate::platform_state::block_settings::load_and_seed(&self.db, &self.seed_defaults)
            .await
            .map_err(|e| format!("seed block_settings after admin init: {e}"))
    }
}

#[wafer_block::wafer_async_trait]
impl BootHooks for DeployBootHooks {
    async fn seed_after_admin_init(&self, wafer: &mut Wafer) -> Result<(), String> {
        let block_settings = self.seed_and_load().await?;
        publish_block_settings(
            wafer,
            &self.config,
            &self.block_settings_handle,
            block_settings,
        );
        Ok(())
    }
}

/// [`BootHooks`] for a **database-built request-path** runtime: read
/// `block_settings`, republish it, write nothing.
///
/// # Why it re-reads at all
///
/// The build reads `block_settings` before `build()`, but that read happens
/// before `init_block(admin)`, and on a database that has never seen the
/// deploy funnel admin's `Init` is a *fresh install* — `apply_pending`
/// bootstraps a fresh install without operator consent, so the table can come
/// into existence, and gain rows, between the pre-build read and this hook.
/// Re-reading here and republishing onto both surfaces is what makes the
/// enablement map every later block's `Init` consults the post-admin-init one.
/// On a settled deployment it returns exactly what the build installed, at the
/// cost of one list (KV-cached on Cloudflare).
///
/// # Why it must not seed
///
/// It is physically write-free, which is an invariant of this path and not a
/// missing feature (`platform_state::block_settings::load` states the same
/// thing at its own definition). Seeding here — as ruling 5.5 originally asked
/// — has three failure modes the deploy funnel does not have, all of them
/// under [`REQUEST_PATH_POLICY`], where a hook error is a 500 on every
/// request:
///
/// 1. **Missing tables 500 every request.** `read_rows` tolerates a missing
///    table, so the seed planner plans an `Insert` for every block; `db.create`
///    does not tolerate one. Production is shielded by preview 404s and by
///    promotion following `/_deploy/init`, but `impresspress serve --target
///    cloudflare` deliberately keeps serving when the local funnel is
///    unreachable (`flows/embed_cloudflare.rs`: "local /_deploy/init not
///    reachable …; serving anyway"), and that stops working.
/// 2. **A successful seed self-invalidates the fleet.** `block_settings` is in
///    `cache_key::bumps_config_version` and both request paths run with
///    `bump_on_write: true`, so the write rewrites `cfg:v1:config_version` in
///    KV and every *other* isolate takes the multi-second full dynamic rebuild
///    Cloudflare error 1102 is made of.
/// 3. **Concurrent isolates race the insert.** `block_name` is `TEXT NOT NULL
///    UNIQUE`; `load_and_seed` is read-then-write with no conflict tolerance,
///    and a generation bump rebuilds runtimes across the fleet, so the
///    losing isolate answers a unique-constraint 500.
///
/// Auto-generated secrets are seeding too, and are likewise the funnel's job:
/// this hook does not call `variables::seed_auto_generated`.
pub struct RequestBootHooks {
    db: Arc<dyn DatabaseService>,
    block_settings_handle: Arc<RwLock<BlockSettings>>,
    /// See [`DeployBootHooks`]'s field of the same name.
    config: Arc<dyn ConfigService>,
}

impl RequestBootHooks {
    /// The request hooks over `db`. Only [`boot_request_runtime`] constructs
    /// one for a boot; public for the same reason as [`DeployBootHooks`].
    pub fn new(
        db: Arc<dyn DatabaseService>,
        block_settings_handle: Arc<RwLock<BlockSettings>>,
        config: Arc<dyn ConfigService>,
    ) -> Self {
        Self {
            db,
            block_settings_handle,
            config,
        }
    }

    /// The database half of [`BootHooks::seed_after_admin_init`]: one read,
    /// and there is no second half. Public for the same reason as
    /// [`DeployBootHooks::seed_and_load`].
    pub async fn load(&self) -> Result<BlockSettings, String> {
        crate::platform_state::block_settings::load(&self.db)
            .await
            .map_err(|e| format!("read block_settings after admin init: {e}"))
    }
}

#[wafer_block::wafer_async_trait]
impl BootHooks for RequestBootHooks {
    async fn seed_after_admin_init(&self, wafer: &mut Wafer) -> Result<(), String> {
        let block_settings = self.load().await?;
        publish_block_settings(
            wafer,
            &self.config,
            &self.block_settings_handle,
            block_settings,
        );
        Ok(())
    }
}

/// [`BootHooks`] for a runtime hydrated from a verified prepared plan:
/// deliberately nothing to do.
///
/// The plan carries the block settings and WRAP grants the deploy funnel
/// sealed into it ([`seal_prepared_plan`]), and the builder installs them
/// without touching the database. Re-running [`DeployBootHooks`]'s seed — or
/// even [`RequestBootHooks`]'s read — here would answer questions the plan has
/// already answered, over the network, on the one path whose entire purpose is
/// to have no structural reads: D1 round trips and subrequests (a dynamic build
/// measured up to 8.4 s of wall time) on every cold isolate, to re-derive
/// state the deploy already sealed. The plan saves I/O, not CPU: a cold
/// prepared build still costs 170–245 ms of CPU locally, and a cold request
/// that builds one measured about 400–670 ms of CPU on Cloudflare, its own
/// work included (CF-RESOURCE-LIMITS, 2026-10-15).
///
/// A no-op impl rather than a `None` argument: `BootHooks` is not an `Option`
/// precisely so a target that seeds nothing has to say why, in a place a
/// reader can find.
pub struct PreparedPlanBootHooks;

#[wafer_block::wafer_async_trait]
impl BootHooks for PreparedPlanBootHooks {
    async fn seed_after_admin_init(&self, _wafer: &mut Wafer) -> Result<(), String> {
        Ok(())
    }
}

/// Publish a freshly loaded `BlockSettings` everywhere the rest of the boot
/// reads it from: the router's shared handle, and both config surfaces (via
/// `RuntimeConfig`, so neither can be filled without the other).
fn publish_block_settings(
    wafer: &mut Wafer,
    config: &Arc<dyn ConfigService>,
    handle: &Arc<RwLock<BlockSettings>>,
    block_settings: BlockSettings,
) {
    let published_json = block_settings.to_config_json();
    *handle
        .write()
        .expect("BlockSettings RwLock poisoned during boot") = block_settings;

    let mut published = RuntimeConfig::new();
    published.both(crate::features::BLOCK_SETTINGS_CONFIG_KEY, published_json);
    published.republish(wafer, config);
}

/// The hosted target's bound secrets: Worker bindings (`wrangler secret put`)
/// that override `variables` rows. Most config belongs in the table so admins
/// can manage it through the dashboard — this list stays short.
pub const PROTECTED_ENV_KEYS: &[&str] = &[crate::blocks::auth::JWT_SECRET_KEY];

/// The hosted target's builder-time vars, plus the one operational knob a
/// deploy sets in `wrangler.toml`.
///
/// The middleware entries are here because they cannot be deferred to the
/// table-backed `ConfigSource`: the flow already exists by the time per-block
/// config is resolvable.
///
/// `IMPRESSPRESS_REQUEST_LOG`, the knob, is read per request, long after the
/// flow exists, so that reason does **not** apply to it. It is here because it
/// is an infrastructure key ([`crate::config_vars::is_infrastructure_key`]),
/// so `blocks::config` answers it from the boot map whatever the `variables`
/// table holds and `CONFIG_SET` refuses to write it. A Worker var is the only
/// channel it has.
pub const BUILDER_WORKER_VAR_KEYS: &[&str] = &[
    crate::config_vars::CORS_ALLOWED_ORIGINS_KEY,
    crate::config_vars::CSP_DIRECTIVES_KEY,
    crate::config_vars::REQUEST_LOG_CONFIG_KEY,
];

/// Whether a config key comes from the hosted target's environment
/// ([`PROTECTED_ENV_KEYS`] or [`BUILDER_WORKER_VAR_KEYS`]) rather than from
/// the `variables` table.
pub fn environment_owns_config_key(key: &str) -> bool {
    PROTECTED_ENV_KEYS.contains(&key) || BUILDER_WORKER_VAR_KEYS.contains(&key)
}

/// What the hosted target's structural config pass takes from its
/// environment: everything a Worker's bindings decide, already read.
pub struct StructuralConfigInputs {
    /// The bound [`PROTECTED_ENV_KEYS`] (the JWT secret today). They reach both config surfaces and the
    /// `ConfigSource` overlay.
    pub secrets: Vec<(&'static str, String)>,
    /// The bound [`BUILDER_WORKER_VAR_KEYS`]. Both config surfaces, never the overlay.
    pub worker_vars: Vec<(&'static str, String)>,
    /// `BlockSettings::to_config_json` for this build.
    pub block_settings_json: String,
    /// Set only by the deploy funnel.
    pub run_migrations: bool,
}

/// Build the structural half of both config surfaces, and the
/// `ConfigSource` overlay. The map carries only:
///
/// - the bound secrets. These never live in the `variables` table.
/// - the builder-time worker vars. CSP/CORS additions are builder-time input:
///   the middleware flow is constructed before lazy block config exists, so
///   they must be present in `ConfigService` before `builder.build()`.
/// - the synthetic `BLOCK_SETTINGS_CONFIG_KEY` → JSON entry so consumer blocks
///   (userportal, migration_helper) can read block enablement and migration
///   state via `ctx.config_get` without a separate query per request.
/// - `IMPRESSPRESS_RUN_MIGRATIONS`, on the deploy funnel only. Request-path
///   builds never migrate: a deploy is what migrates a hosted database, as a
///   boot is what migrates a native server.
///
/// Every key is written with `both`, so it lands on each surface; the one
/// deliberate divergence (`service_only`) belongs to consumer request config,
/// which the target adds afterwards with its reason.
///
/// The overlay layers the secrets over the `variables` rows, so a secret never
/// has to be mirrored into that table. Worker vars are not in it: they are
/// builder-time middleware input, not per-block config, and a block that
/// declared one as its `Init` config would then read the admin's saved row,
/// not a var that silently overrides it.
pub fn structural_runtime_config(
    inputs: StructuralConfigInputs,
) -> (RuntimeConfig, HashMap<String, String>) {
    let mut config = RuntimeConfig::new();
    let mut overlay: HashMap<String, String> = HashMap::new();
    for (key, value) in inputs.secrets {
        config.both(key, value.clone());
        overlay.insert(key.to_string(), value);
    }
    for (key, value) in inputs.worker_vars {
        config.both(key, value);
    }
    config.both(
        crate::features::BLOCK_SETTINGS_CONFIG_KEY,
        inputs.block_settings_json,
    );
    if inputs.run_migrations {
        config.both(crate::migration_helper::RUN_MIGRATIONS_KEY, "1");
    }
    (config, overlay)
}

/// Add consumer-declared request config to the async service surfaces only.
/// Both fills call this: the cold one through [`add_request_config`], the warm
/// one through [`warm_config_surfaces`].
fn extend_with_request_config(
    config: &mut HashMap<String, String>,
    overlay: &mut HashMap<String, String>,
    request_config: &HashMap<String, String>,
) {
    for (key, value) in request_config {
        // Framework-owned protected/builder keys come from the environment and
        // must not be shadowed by a consumer-provided duplicate.
        if environment_owns_config_key(key) {
            continue;
        }
        config.insert(key.clone(), value.clone());
        overlay.insert(key.clone(), value.clone());
    }
}

/// Add consumer-declared request config (a forwarding consumer's
/// `request_config`) to a build's async service surface and to the
/// `ConfigSource` overlay, and to neither snapshot: one request's secret must
/// never be baked into the isolate-cached synchronous surface.
///
/// `RuntimeConfig::service_only` takes that reason as an argument, so the
/// divergence is stated where it is created rather than inferred from a
/// missing line.
///
/// A key the structural pass has already installed is skipped outright, and
/// that rule is derived rather than listed: `service_only` REMOVES its key
/// from the snapshot, so a collision does not shadow the structural value, it
/// deletes it. The two name lists `extend_with_request_config` checks
/// ([`PROTECTED_ENV_KEYS`], [`BUILDER_WORKER_VAR_KEYS`]) cover the
/// environment-owned families but not the keys the structural pass installs
/// itself — `IMPRESSPRESS_BLOCK_SETTINGS` and `IMPRESSPRESS_RUN_MIGRATIONS`.
/// Asking the `RuntimeConfig` what it already holds covers all of them, and
/// covers the next structural key without anyone remembering to add it to a
/// list.
pub fn add_request_config(
    config: &mut RuntimeConfig,
    overlay: &mut HashMap<String, String>,
    request_config: &HashMap<String, String>,
) {
    let consumer_owned: HashMap<String, String> = request_config
        .iter()
        .filter(|(key, _)| !config.snapshot_contains(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut service_map = HashMap::new();
    extend_with_request_config(&mut service_map, overlay, &consumer_owned);
    for (key, value) in service_map {
        config.service_only(
            key,
            value,
            "consumer request config is request-current: the isolate-cached \
             snapshot must not carry one request's values into the next",
        );
    }
}

/// The config surfaces a **warm** request's services read through: the map
/// behind that request's `ConfigService`, and the `ConfigSource` overlay
/// layered over the `variables` rows (which [`request_config_source`] then
/// turns into the request's own config source).
///
/// `bound` is every environment-owned key ([`PROTECTED_ENV_KEYS`],
/// [`BUILDER_WORKER_VAR_KEYS`]) bound at THIS request, at the value it sees.
/// Those come from `bound` and from nowhere else; the cached runtime's
/// structural snapshot contributes only the keys the environment does *not*
/// own, which today is the table-derived `IMPRESSPRESS_BLOCK_SETTINGS`. A
/// binding removed since the runtime was built is therefore absent by
/// construction.
///
/// There is no snapshot here: one request's values must never be baked into
/// the cached runtime's, so this is a pair of maps rather than a
/// [`RuntimeConfig`].
pub fn warm_config_surfaces(
    bound: &HashMap<String, String>,
    structural_snapshot: &HashMap<String, String>,
    request_config: &HashMap<String, String>,
) -> (HashMap<String, String>, HashMap<String, String>) {
    let mut config_map: HashMap<String, String> = structural_snapshot
        .iter()
        .filter(|(key, _)| !environment_owns_config_key(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    config_map.extend(
        bound
            .iter()
            .filter(|(key, _)| environment_owns_config_key(key))
            .map(|(key, value)| (key.clone(), value.clone())),
    );

    let mut overlay = HashMap::new();
    extend_with_request_config(&mut config_map, &mut overlay, request_config);
    // Secrets are layered over the `variables` rows for the same reason the
    // cold fill does it: a secret then never has to be mirrored into that table.
    for key in PROTECTED_ENV_KEYS {
        if let Some(value) = bound.get(*key) {
            overlay.insert((*key).to_string(), value.clone());
        }
    }
    (config_map, overlay)
}

/// The config source every hosted build and warm request uses:
/// [`DatabaseConfigSource`](crate::config_source::DatabaseConfigSource) over
/// `db`, resolving each block against `snapshot` — the runtime's one lazily
/// fetched unfiltered read of the variables table, which its config block
/// shares — with `overlay` (the bound secrets and the consumer's request
/// config) layered over the rows.
///
/// The source is per request, the snapshot per runtime: a warm request passes
/// the cached runtime's snapshot, so resolving a block's config on it reads
/// nothing.
pub fn request_config_source(
    db: Arc<dyn DatabaseService>,
    overlay: HashMap<String, String>,
    snapshot: Arc<crate::config_source::VariablesSnapshot>,
) -> Arc<dyn wafer_run::ConfigSource> {
    Arc::new(crate::config_source::DatabaseConfigSource::with_overlay(
        db, overlay, snapshot,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/_deploy/prepare`'s closing grants read failing fails the prepare:
    /// sealing a plan over it would ship no admin-created grants to every
    /// prepared runtime built from that plan, until the next deploy.
    #[tokio::test]
    async fn sealing_a_plan_fails_when_the_grants_read_fails() {
        let db = crate::platform_state::wrap_grants::fixture_with_failing_reads().await;
        let exporter = super::super::ImpresspressBuilder::new()
            .prepared_plan_exporter()
            .expect("an exporter");
        let error = seal_prepared_plan(&db, &exporter)
            .await
            .expect_err("a failed grants read must not seal a plan");
        assert!(error.to_string().contains("injected outage"), "{error}");
    }

    /// The structural half of the hosted fill: every key reaches BOTH
    /// surfaces with the same value, and only the secrets reach the overlay.
    #[test]
    fn structural_keys_reach_both_config_surfaces() {
        let (config, overlay) = structural_runtime_config(StructuralConfigInputs {
            secrets: vec![(crate::blocks::auth::JWT_SECRET_KEY, "jwt".to_string())],
            worker_vars: vec![
                (
                    crate::config_vars::CORS_ALLOWED_ORIGINS_KEY,
                    "*".to_string(),
                ),
                (
                    crate::config_vars::CSP_DIRECTIVES_KEY,
                    "default-src 'self'".to_string(),
                ),
            ],
            block_settings_json: "{}".to_string(),
            run_migrations: true,
        });

        for (key, value) in [
            (crate::blocks::auth::JWT_SECRET_KEY, "jwt"),
            (crate::config_vars::CORS_ALLOWED_ORIGINS_KEY, "*"),
            (crate::config_vars::CSP_DIRECTIVES_KEY, "default-src 'self'"),
            (crate::features::BLOCK_SETTINGS_CONFIG_KEY, "{}"),
            (crate::migration_helper::RUN_MIGRATIONS_KEY, "1"),
        ] {
            assert_eq!(
                config.service_get(key),
                Some(value),
                "{key} on the service map"
            );
            assert!(config.snapshot_contains(key), "{key} on the snapshot");
        }
        assert!(
            config.service_only_keys().is_empty(),
            "no structural key diverges",
        );
        assert_eq!(
            overlay
                .get(crate::blocks::auth::JWT_SECRET_KEY)
                .map(String::as_str),
            Some("jwt"),
        );
        for key in [
            crate::config_vars::CORS_ALLOWED_ORIGINS_KEY,
            crate::config_vars::CSP_DIRECTIVES_KEY,
        ] {
            assert!(!overlay.contains_key(key), "{key} is not in the overlay");
        }
    }

    /// The `run_migrations` flag is the deploy funnel's alone: a request-path
    /// build passes `false` and the key must be absent, not present-and-empty
    /// — `migration_helper` gates on `== Some("1")`, but an absent key is also
    /// what keeps the two builds' config identity distinct.
    #[test]
    fn a_request_path_build_carries_no_run_migrations_key() {
        let (config, _overlay) = structural_runtime_config(StructuralConfigInputs {
            secrets: Vec::new(),
            worker_vars: Vec::new(),
            block_settings_json: "{}".to_string(),
            run_migrations: false,
        });

        assert_eq!(
            config.service_get(crate::migration_helper::RUN_MIGRATIONS_KEY),
            None,
        );
        assert!(!config.snapshot_contains(crate::migration_helper::RUN_MIGRATIONS_KEY));
    }
}
