//! The one post-`build()` lifecycle for every target: [`boot`], its
//! [`BootHooks`] seam, the [`InitPolicy`] that decides what a failure *does*,
//! the [`GrantSource`] that decides where the runtime's WRAP grants come from,
//! and the native-embedding `register_vector_block` helper.
//!
//! Before this module owned the whole sequence there were four hand-maintained
//! orderings — the tolerant native/browser funnel, the reported deploy funnel,
//! and two Cloudflare request-path copies that differed from each other over
//! whether WRAP grants were applied and from both funnels over whether the
//! seed hook ran at all. Every step below is therefore a *parameter* of one
//! function rather than a line a caller has to remember to copy.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use wafer_core::interfaces::database::service::{DatabaseError, DatabaseService};
use wafer_run::{InitError, InitWait, RuntimeError, Wafer};

/// Request-current config flag set only while an authenticated deployment
/// candidate is exporting a prepared runtime plan.
///
/// Consumer blocks whose `Init` lifecycle also publishes mutable derived
/// output (for example a pre-rendered homepage) should still run migrations
/// and seeds, but defer that publication while this value is `"1"`. This
/// keeps the currently promoted Worker and its mutable objects unchanged
/// until the prepared candidate has passed verification and promotion.
pub const PREPARE_RUNTIME_PLAN_KEY: &str = "IMPRESSPRESS_PREPARE_RUNTIME_PLAN";

/// What a block `Init` failure, a seed-hook failure or a failed
/// [`GrantSource::Database`] read *does*. The ordering [`boot`] runs is
/// identical under all three; only the consequence differs.
///
/// [`InitPolicy::Tolerant`] and [`InitPolicy::Serving`] return a
/// [`BootError`] where they abort; [`InitPolicy::Reported`] never does for a
/// step's failure, only for the runtime's own (`seal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitPolicy {
    /// Log and continue. The runtime is published even with a broken block, so
    /// one misconfigured block cannot wedge a whole page. Used by the browser,
    /// whose runtime is long-lived, inspectable and rebuilt on demand.
    ///
    /// Three failures are treated differently here, deliberately. A failed
    /// seed hook still aborts: it leaves the config and enablement snapshots
    /// the runtime would serve from unknown. So does a failed admin `Init`:
    /// admin's `Init` is the first half of the same seeding — its migrations
    /// create the tables the seed hook reads, and on a provisioning build
    /// (every browser build is one) it seeds the shared variables, so a
    /// failed one leaves the same snapshots unknown. Booting on would also
    /// leave the admin block failed for the runtime's lifetime (a permanent
    /// `Init` error is cached in its slot), the very surface that would repair
    /// the row. A failed grants read boots on: the
    /// admin-created grants only ever *add* access (`Wafer::add_wrap_grants`
    /// is additive), so a runtime without them denies what they would have
    /// allowed and never allows more — the fail-safe direction, and the page
    /// stays usable to repair the database. The failure is logged and is in
    /// [`BootReport::grants`].
    Tolerant,
    /// Serve what can be served. Used by the Cloudflare request path, whose
    /// runtime is rebuilt rather than repaired in place.
    ///
    /// - A block whose `Init` fails **transiently** (its backend unreachable,
    ///   a condition that clears) is recorded in [`BootReport::degraded`] and
    ///   the boot goes on: the runtime is published without it, every other
    ///   block serves, and a dispatch to it answers `Unavailable`. The caller
    ///   retries by building a fresh runtime, never by re-running `Init` in a
    ///   request.
    /// - A **permanent** block `Init` failure (a real config bug), a failed
    ///   admin `Init` of either kind, a failed grants read and a failed seed
    ///   hook abort the boot with a [`BootError`] that says which.
    ///
    /// Admin is a precondition for the reason [`Self::Tolerant`] gives: its
    /// `Init` creates the tables the seed hook reads and, on a provisioning
    /// build, seeds them. On a settled request-path build it reads nothing.
    ///
    /// Requires a runtime built with [`InitWait::Refuse`], and [`boot`]
    /// refuses the pair otherwise, before anything runs: under
    /// [`InitWait::Await`] a published slot that is not initialized would run
    /// its `Init` inside a later request, which may be stopped part-way while
    /// it holds the slot's attempt lock — on Workers, for the runtime's life.
    Serving,
    /// Capture every outcome and keep going, so the caller can render the full
    /// picture. Used by `/_deploy/init`, whose whole product is the report, and
    /// by the native boot (`impresspress_server::boot_native`), which names
    /// every failed block and then refuses to start.
    Reported,
}

/// Where this runtime's deployment-owned WRAP grants come from.
///
/// Grants registered after [`Wafer::seal`] are ignored, so this is a step that
/// has to happen inside the funnel and cannot be a follow-up call. Making it a
/// required argument is the point: the Cloudflare prepared-hydration path used
/// to differ from its two sibling paths purely by *omitting* the grant call,
/// and nothing but a comment recorded that the omission was deliberate.
pub enum GrantSource<'a> {
    /// Load admin-created grants from the platform database and register them
    /// before seal. A table that does not exist yet is no grants; a read that
    /// failed is a failed step, which [`boot`] handles by its [`InitPolicy`] —
    /// see [`crate::platform_state::wrap_grants::load`].
    Database(&'a Arc<dyn DatabaseService>),
    /// Every grant this runtime gets was already installed by the builder,
    /// via [`super::ImpresspressBuilder::wrap_grants`] or a verified prepared
    /// plan. The `&'static str` is the reason, recorded at the call site
    /// rather than in a comment next to a call that is not there.
    PreInstalled(&'static str),
}

/// Schema version of the `/_deploy/*` control-plane response envelopes.
///
/// One number for `/_deploy/prepare` and `/_deploy/verify` together, because
/// they are two halves of one deployment handshake: `impresspress deploy`
/// cannot use a prepare response without the verify response that follows it,
/// so a CLI that understands one version of either must understand that
/// version of both. Splitting them would let a Worker advertise a pair the CLI
/// can only half-read.
///
/// It lives here, beside [`BootReport`], because the report is the payload the
/// version describes. It was previously three literals — two `serde_json::
/// json!` `1`s in the Worker's deploy endpoints and one bare `1` in the CLI's
/// verify parser — plus a fourth constant in the CLI, with nothing tying any
/// of them together.
///
/// A change to either response that the other side cannot read bumps it, so
/// a mismatched CLI and Worker meet a schema refusal rather than a decode
/// error. At 2, `/_deploy/verify` carries the password-hasher probe's
/// required `password_hasher` field. At 3, the prepare response's
/// [`BootReport`] carries the required `grants` step.
pub const DEPLOY_RESPONSE_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepOutcome {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether the failure may clear on a fresh attempt — an unreachable
    /// store, or the invocation's spent database call budget — by the rule
    /// every `Init` failure is classified with
    /// ([`wafer_run::init_failure_is_transient`]). Kept from the step's typed
    /// error, never read back from `error`'s text. `false` on success.
    ///
    /// Additive on the wire: written only when `true`, and an absent field
    /// reads `false`, the class every failure had before it existed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub transient: bool,
}

impl StepOutcome {
    /// A step that succeeded.
    pub fn succeeded() -> Self {
        Self {
            ok: true,
            error: None,
            transient: false,
        }
    }

    /// A step that failed with `error`, transiently or not.
    pub fn failed(error: String, transient: bool) -> Self {
        Self {
            ok: false,
            error: Some(error),
            transient,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockInitOutcome {
    pub block: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether the block's `Init` failed transiently ([`InitError::Transient`]).
    /// See [`StepOutcome::transient`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub transient: bool,
}

impl BlockInitOutcome {
    /// `block`'s `Init` succeeded.
    pub fn succeeded(block: &str) -> Self {
        Self {
            block: block.to_string(),
            ok: true,
            error: None,
            transient: false,
        }
    }

    /// `block`'s `Init` failed with `error`.
    pub fn failed(block: &str, error: &InitError) -> Self {
        Self {
            block: block.to_string(),
            ok: false,
            error: Some(error.to_string()),
            transient: matches!(error, InitError::Transient(_)),
        }
    }
}

/// `skip_serializing_if` for the `transient` flags.
fn is_false(flag: &bool) -> bool {
    !flag
}

/// Per-step outcome of one [`boot`] call, and the `/_deploy/init` and
/// `/_deploy/prepare` response body verbatim.
///
/// This type crosses the CLI/Worker boundary: `impresspress deploy`
/// (`cli::helpers::cloudflare::prepared`) deserializes the very same struct
/// the runtime serialized, rather than a hand-written twin of it. The twins
/// were the defect — being separate types, a renamed field was not a compile
/// error anywhere, and both twins carried `deny_unknown_fields`, so the
/// producer could not add a step without breaking every CLI already built.
///
/// **No `deny_unknown_fields`, deliberately.** The only place this type is
/// *deserialized* is the `init_report` field of the `/_deploy/prepare`
/// envelope, and that envelope carries [`DEPLOY_RESPONSE_SCHEMA_VERSION`],
/// which is what gates a skew the consumer cannot survive. An added step
/// cannot change the meaning of the flags the deploy is gated on, so
/// refusing it would force a lockstep CLI upgrade for a purely additive
/// change. The envelope itself stays strict.
///
/// `/_deploy/init` serializes the report as the whole body, with no envelope
/// and no version — but nothing parses that body: `impresspress serve
/// --target cloudflare` reads it as text and prints it for the operator. If a
/// consumer of that shape ever appears it needs the envelope, not a relaxed
/// argument.
///
/// [`InitPolicy::Tolerant`] and [`InitPolicy::Serving`] callers read it for
/// what they published without ([`Self::degraded`], `Serving` only); the
/// failures they abort on are a [`BootError`] instead.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootReport {
    pub sealed: bool,
    /// Loading the deployment-owned WRAP grants ([`GrantSource`]). Always
    /// `ok` for [`GrantSource::PreInstalled`], which reads nothing.
    pub grants: StepOutcome,
    pub seed: StepOutcome,
    pub blocks: Vec<BlockInitOutcome>,
    /// The blocks an [`InitPolicy::Serving`] boot published without, because
    /// their `Init` failed transiently. Also in [`Self::blocks`] as failed.
    /// Never filled by [`InitPolicy::Reported`], so the deploy wire shape
    /// (and its schema version) does not change: an empty list is not
    /// serialized, and an absent one reads as empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<DegradedBlock>,
    /// True iff every step and every block init succeeded.
    pub ok: bool,
}

impl BootReport {
    /// Whether this report failed only in ways a fresh attempt may clear:
    /// it is not `ok`, and every failed step and block `Init` is
    /// `transient`. What `/_deploy/init|prepare` answers 503 (retry) rather
    /// than 500 (fix) on — a database outage, or a migration the
    /// invocation's call budget refused, whose ledger lets the next attempt
    /// resume where this one stopped.
    pub fn failed_transiently(&self) -> bool {
        let steps = [&self.grants, &self.seed]
            .into_iter()
            .filter(|step| !step.ok)
            .map(|step| step.transient);
        let blocks = self
            .blocks
            .iter()
            .filter(|block| !block.ok)
            .map(|block| block.transient);
        !self.ok && steps.chain(blocks).all(|transient| transient)
    }
}

/// A block an [`InitPolicy::Serving`] boot published without.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DegradedBlock {
    pub block: String,
    /// The transient `Init` failure, rendered.
    pub reason: String,
}

/// Why a seed hook ([`BootHooks::seed_after_admin_init`]) failed, classified
/// so the caller can tell an outage (retry later) from a fault (fix it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedError {
    /// The store the hook reads or writes could not be reached; a retry may
    /// succeed.
    Unavailable(String),
    /// Anything a retry cannot fix.
    Fault(String),
}

impl SeedError {
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Unavailable(_))
    }
}

impl std::fmt::Display for SeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(message) | Self::Fault(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SeedError {}

impl From<DatabaseError> for SeedError {
    /// Classified by the one transient rule every `Init` and config source
    /// failure is ([`wafer_run::init_failure_is_transient`]).
    fn from(e: DatabaseError) -> Self {
        if wafer_run::init_failure_is_transient(e.code(), e.detail_code()) {
            Self::Unavailable(e.to_string())
        } else {
            Self::Fault(e.to_string())
        }
    }
}

/// Why [`boot`] aborted, typed so a caller can answer an outage (503, retry)
/// differently from a fault (500): [`Self::is_transient`].
#[derive(Debug)]
pub enum BootError {
    /// A block's `Init` failed in a way the policy does not degrade: a
    /// permanent failure under [`InitPolicy::Serving`], or admin's failure of
    /// either kind under `Serving` and [`InitPolicy::Tolerant`].
    BlockInit { block: String, error: InitError },
    /// The deployment-owned WRAP grants could not be read
    /// ([`GrantSource::Database`]).
    Grants(DatabaseError),
    /// The seed hook failed.
    Seed(SeedError),
    /// The runtime itself failed (`seal`, a malformed grant), or the boot was
    /// asked for a policy its runtime cannot run. Always permanent.
    Runtime(RuntimeError),
}

impl BootError {
    /// Whether the database itself could not answer the boot: a block's
    /// `Init` that propagated the database's outage, or the grants read
    /// (`runtime_health::is_database_outage`).
    pub fn is_database_outage(&self) -> bool {
        match self {
            Self::BlockInit {
                error: InitError::Transient(failure),
                ..
            } => crate::runtime_health::is_database_outage(failure.detail_code.as_deref()),
            Self::Grants(e) => crate::runtime_health::is_database_outage(e.detail_code()),
            Self::BlockInit { .. } | Self::Seed(_) | Self::Runtime(_) => false,
        }
    }

    /// Whether a fresh attempt may succeed: a transient `Init` failure, an
    /// unreachable grants read or seed store — by the same rule every `Init`
    /// failure is classified with ([`wafer_run::init_failure_is_transient`]).
    pub fn is_transient(&self) -> bool {
        match self {
            Self::BlockInit { error, .. } => matches!(error, InitError::Transient(_)),
            Self::Grants(e) => wafer_run::init_failure_is_transient(e.code(), e.detail_code()),
            Self::Seed(e) => e.is_transient(),
            Self::Runtime(_) => false,
        }
    }
}

impl std::fmt::Display for BootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BlockInit { block, error } => write!(f, "block `{block}` Init failed: {error}"),
            Self::Grants(e) => write!(f, "load the admin-created WRAP grants: {e}"),
            Self::Seed(e) => write!(f, "seed after admin init: {e}"),
            Self::Runtime(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BootError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BlockInit { error, .. } => Some(error),
            Self::Grants(e) => Some(e),
            Self::Seed(e) => Some(e),
            Self::Runtime(e) => Some(e),
        }
    }
}

impl From<RuntimeError> for BootError {
    fn from(e: RuntimeError) -> Self {
        Self::Runtime(e)
    }
}

/// Per-target boot I/O for [`boot`]. Implemented by each platform to supply
/// the one step that genuinely differs between them: what to seed and which
/// shared snapshots to publish into once the admin block's `Init` has created
/// the variables / block_settings tables.
///
/// Everything else around it — the invariant `grants → seal → admin-first init
/// → seed → init the rest` ordering — is owned by [`boot`].
///
/// Deliberately **not** an `Option` argument on [`boot`]: a target with
/// nothing to seed writes a no-op impl and says why (native's
/// `NativeBootHooks`), which is a decision a reader can find. A `None` is a
/// decision nobody has to write down, and skipping the seed by accident on one
/// of several paths is exactly the defect this funnel exists to close.
#[wafer_block::wafer_async_trait]
pub trait BootHooks {
    /// Runs AFTER `init_block(admin)` (so the admin migration has created the
    /// `impresspress__admin__variables` + `block_settings` tables) and BEFORE
    /// the remaining blocks initialize (so a block depending on a seeded
    /// `auto_generate` key can't lose the `HashMap::keys()` race and
    /// permanent-fail on a missing secret — the impresspress #209 regression
    /// class).
    ///
    /// Implementations call [`crate::platform_state::variables::seed_auto_generated`] /
    /// [`crate::platform_state::variables::seed_and_load`] /
    /// [`crate::platform_state::block_settings::load_and_seed`] as appropriate and
    /// publish the results into the shared `ConfigService` / `BlockSettings`
    /// handle / crypto secret the runtime already holds — through
    /// [`super::RuntimeConfig::republish`], so both config surfaces move
    /// together.
    ///
    /// Under [`InitPolicy::Tolerant`] and [`InitPolicy::Serving`] an `Err`
    /// aborts the boot ([`BootError::Seed`]); under [`InitPolicy::Reported`]
    /// it is captured in [`BootReport::seed`]. Classify it: a store that could
    /// not be reached is [`SeedError::Unavailable`] (`From<DatabaseError>`
    /// does that by the shared rule), anything else [`SeedError::Fault`]. Return `Err` for a genuinely fatal condition (for
    /// example, structural settings could not be persisted consistently, or a
    /// required secret cannot be read). Optional best-effort per-key seeds
    /// should still log and continue inside the implementation.
    ///
    /// Receives `&mut Wafer` so a target that could not know its settings
    /// before admin migration (browser/Cloudflare first deploy) can publish the
    /// seeded values into the config snapshot before any other block initializes.
    async fn seed_after_admin_init(&self, wafer: &mut Wafer) -> Result<(), SeedError>;
}

/// The one post-`build()` lifecycle, for every target:
///
/// 1. `grants` — register deployment-owned WRAP grants. MUST precede `seal`.
///    A failed [`GrantSource::Database`] read aborts the boot under
///    [`InitPolicy::Serving`] ([`BootError::Grants`]), is captured in
///    [`BootReport::grants`] under [`InitPolicy::Reported`], and is logged
///    under [`InitPolicy::Tolerant`]; the last two then seal without the
///    admin-created grants.
/// 2. `seal()` — finalize composite/uses/capability/snapshot wiring.
/// 3. `init_block(admin)` FIRST — admin's migrations create the variables /
///    block_settings tables before any other block's `Init` writes to them,
///    and before the seed step reads them; on a provisioning build it also
///    seeds the shared variables. A failure aborts under
///    [`InitPolicy::Serving`] and [`InitPolicy::Tolerant`].
/// 4. `hooks.seed_after_admin_init` — seed + publish (see [`BootHooks`]).
/// 5. every remaining block, in `Wafer::block_names()` order (sorted, so the
///    sequence is identical across processes and platforms).
///
/// `policy` changes none of that ordering; it decides only what a failure at
/// step 1, 3, 4 or 5 does. See [`InitPolicy`]. Before step 1, an
/// [`InitPolicy::Serving`] boot of a runtime that is not
/// [`InitWait::Refuse`] is refused ([`BootError::Runtime`]), with nothing run.
///
/// The caller must have already wired the pre-seal bits its platform needs
/// (`set_asset_loader`, any post-build block registration) onto `wafer` before
/// calling this; the config surfaces are the builder's (see
/// [`super::RuntimeConfig`]). After it returns, the caller dispatches requests
/// / stores the runtime handle as appropriate.
///
/// Native uses this funnel too, then runs the native-only
/// [`Wafer::run_start_lifecycle`] + [`Wafer::bind_all`] steps afterwards: its
/// HTTP-listener block binds the TCP socket in the `Start`-lifecycle `bind()`
/// pass, which the stateless targets omit (they dispatch per-request via
/// `wafer.run`).
pub async fn boot(
    wafer: &mut Wafer,
    hooks: &dyn BootHooks,
    grants: GrantSource<'_>,
    policy: InitPolicy,
) -> Result<BootReport, BootError> {
    // 0. `Serving` publishes unsettled slots; only a runtime that never runs
    //    an `Init` in a dispatch may hold them (see `InitPolicy::Serving`).
    if policy == InitPolicy::Serving && wafer.init_wait() != InitWait::Refuse {
        return Err(BootError::Runtime(RuntimeError::Config(format!(
            "InitPolicy::Serving publishes blocks whose Init has not succeeded, so it needs a \
             runtime built with InitWait::Refuse; this one is {:?}",
            wafer.init_wait()
        ))));
    }

    // 1. Deployment-owned WRAP grants, before seal — `add_wrap_grants` after
    //    seal is silently ignored.
    let grants = match grants {
        GrantSource::Database(db) => match crate::platform_state::wrap_grants::load(db).await {
            Ok(loaded) => {
                if !loaded.is_empty() {
                    tracing::info!(count = loaded.len(), "registering database WRAP grants");
                    wafer.add_wrap_grants(loaded)?;
                }
                StepOutcome::succeeded()
            }
            Err(e) => {
                if policy == InitPolicy::Serving {
                    // Typed, so an unreachable table (503) and anything else
                    // (500) stay apart — never classified from a string.
                    return Err(BootError::Grants(e));
                }
                let transient = wafer_run::init_failure_is_transient(e.code(), e.detail_code());
                let error = format!("load the admin-created WRAP grants: {e}");
                if policy == InitPolicy::Tolerant {
                    tracing::error!(
                        error = %error,
                        "booting without the admin-created WRAP grants",
                    );
                }
                StepOutcome::failed(error, transient)
            }
        },
        GrantSource::PreInstalled(_because) => StepOutcome::succeeded(),
    };

    // 2. Seal.
    wafer.seal().await?;

    let admin_id = crate::blocks::admin::ADMIN_BLOCK_ID;
    let names = wafer.block_names();
    let mut blocks = Vec::new();
    let mut degraded = Vec::new();

    // 3. Admin first — its Init creates the variables / block_settings tables
    //    the seed step reads, and migration 002's `block` column the auto-gen
    //    seeder writes. A precondition under `Tolerant` and `Serving` (see
    //    `InitPolicy::Tolerant`); `Reported` keeps going so its report is
    //    complete.
    if names.iter().any(|name| name == admin_id) {
        match wafer.init_block(admin_id).await {
            Ok(_) => blocks.push(BlockInitOutcome::succeeded(admin_id)),
            Err(error) => {
                if policy != InitPolicy::Reported {
                    return Err(BootError::BlockInit {
                        block: admin_id.to_string(),
                        error,
                    });
                }
                blocks.push(BlockInitOutcome::failed(admin_id, &error));
            }
        }
    }

    // 4. Seed + publish.
    let seed = match hooks.seed_after_admin_init(wafer).await {
        Ok(()) => StepOutcome::succeeded(),
        Err(e) => match policy {
            InitPolicy::Reported => StepOutcome::failed(e.to_string(), e.is_transient()),
            InitPolicy::Tolerant | InitPolicy::Serving => return Err(BootError::Seed(e)),
        },
    };

    // 5. Every remaining block. Admin is a slot-cached no-op on a second pass,
    //    so it is skipped rather than reported twice. Iterates the registration
    //    keys (`block_names`) — the exact set `init_block` resolves — rather
    //    than each block's self-reported `info().name`.
    for name in &names {
        if name == admin_id {
            continue;
        }
        match wafer.init_block(name).await {
            Ok(_) => blocks.push(BlockInitOutcome::succeeded(name)),
            Err(error) => {
                match policy {
                    InitPolicy::Serving => {
                        if !matches!(error, InitError::Transient(_)) {
                            return Err(BootError::BlockInit {
                                block: name.clone(),
                                error,
                            });
                        }
                        tracing::warn!(
                            block = %name,
                            error = %error,
                            "publishing without a block whose Init failed transiently",
                        );
                        degraded.push(DegradedBlock {
                            block: name.clone(),
                            reason: error.to_string(),
                        });
                    }
                    InitPolicy::Tolerant => {
                        tracing::error!(
                            block = %name,
                            error = %error,
                            "block init lifecycle failed during boot",
                        );
                    }
                    InitPolicy::Reported => {}
                }
                blocks.push(BlockInitOutcome::failed(name, &error));
            }
        }
    }

    let ok = grants.ok && seed.ok && blocks.iter().all(|b| b.ok);
    Ok(BootReport {
        sealed: true,
        grants,
        seed,
        blocks,
        degraded,
        ok,
    })
}

/// Register the `wafer-run/vector` runtime block backed by native
/// `SqliteVecService`.
///
/// - Opens a dedicated `rusqlite::Connection` at `db_path`. SQLite supports
///   multi-connection access with WAL, so sharing the DB file with the
///   platform's `DatabaseService` connection is safe.
/// - Loads no embedding model. `wafer-run/vector` serves only `vector@v1`;
///   embedding is `impresspress/fastembed`'s (registered by `block-fastembed`,
///   which this feature implies), and its model loads lazily on the first
///   embed call. So the vector store is registered whether or not the model
///   can be downloaded.
///
/// This function is only compiled when the `native-embedding` feature is on;
/// the `impresspress/vector` feature block registration in `impresspress-core` is
/// gated by the same feature so the two stay in sync.
#[cfg(feature = "native-embedding")]
pub(super) fn register_vector_block(
    wafer: &mut Wafer,
    db_path: Option<&str>,
) -> Result<(), RuntimeError> {
    use wafer_block_sqlite::vector::SqliteVecService;
    use wafer_core::interfaces::vector::service::VectorService;

    let Some(db_path) = db_path else {
        return Err(RuntimeError::Config(
            "native-embedding feature is enabled but no sqlite_db_path was \
             provided to ImpresspressBuilder — call .sqlite_db_path(...) before \
             .build()"
                .to_string(),
        ));
    };

    // Dedicated connection for the vector service — see module docs on
    // `sqlite_db_path` for why a second connection is fine.
    let vec_conn = rusqlite::Connection::open(db_path).map_err(|e| {
        RuntimeError::Config(format!(
            "failed to open SQLite connection at '{db_path}' for vector service: {e}"
        ))
    })?;
    let vec_svc: Arc<dyn VectorService> =
        Arc::new(SqliteVecService::new(vec_conn).map_err(|e| {
            RuntimeError::Config(format!(
                "failed to start the vector service on '{db_path}': {e}"
            ))
        })?);

    wafer_core::service_blocks::vector::register_with(wafer, vec_svc)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use wafer_block::{
        core_types::{ErrorCode, LifecycleEvent, LifecycleType, Message, WaferError},
        streams::{input::InputStream, output::OutputStream},
        Block, BlockInfo,
    };
    use wafer_core::interfaces::database::service::DatabaseService;
    use wafer_run::Wafer;

    use super::*;

    struct InitProbeBlock {
        name: &'static str,
        order: Arc<Mutex<Vec<String>>>,
        /// The code its `Init` fails with, if it fails: `Unknown` is
        /// permanent, `Unavailable` transient
        /// ([`wafer_run::init_failure_is_transient`]).
        fail: Option<ErrorCode>,
    }

    #[wafer_block::wafer_async_trait]
    impl Block for InitProbeBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new(self.name, "0.1.0", "test/init@v1", "test")
        }

        async fn lifecycle(
            &self,
            _ctx: &dyn wafer_block::context::Context,
            event: LifecycleEvent,
        ) -> Result<(), WaferError> {
            if event.event_type == LifecycleType::Init {
                push(&self.order, self.name);
                if let Some(code) = self.fail {
                    return Err(WaferError::new(code, "deliberate Init failure"));
                }
            }
            Ok(())
        }

        async fn handle(
            &self,
            _ctx: &dyn wafer_block::context::Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::respond(Vec::new())
        }
    }

    fn push(order: &Arc<Mutex<Vec<String>>>, entry: &str) {
        order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(entry.to_string());
    }

    fn taken(order: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Records the moment `seed_after_admin_init` ran into the same ordering
    /// log the probe blocks write to, so a test can assert where in the
    /// sequence it landed. `fail` makes the hook return `Err`.
    struct ProbeHooks {
        order: Arc<Mutex<Vec<String>>>,
        fail: bool,
    }

    #[wafer_block::wafer_async_trait]
    impl BootHooks for ProbeHooks {
        async fn seed_after_admin_init(&self, _wafer: &mut Wafer) -> Result<(), SeedError> {
            push(&self.order, "seed");
            if self.fail {
                return Err(SeedError::Fault("deliberate seed failure".to_string()));
            }
            Ok(())
        }
    }

    fn register_probe(
        wafer: &mut Wafer,
        name: &'static str,
        order: &Arc<Mutex<Vec<String>>>,
        fail: Option<ErrorCode>,
    ) {
        wafer
            .register_block(
                name,
                Arc::new(InitProbeBlock {
                    name,
                    order: order.clone(),
                    fail,
                }),
            )
            .unwrap();
    }

    /// **The vector store does not wait on an embedding model.**
    /// `wafer-run/vector` serves only `vector@v1`, so the native store is
    /// registered from the database file alone: nothing is downloaded, and no
    /// model directory is consulted.
    #[cfg(feature = "native-embedding")]
    #[test]
    fn the_native_vector_store_registers_without_an_embedding_model() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("vectors.sqlite3");
        let mut wafer = empty_wafer();

        register_vector_block(&mut wafer, Some(db.to_str().expect("utf-8 path")))
            .expect("register the vector store");

        assert!(
            wafer.block_names().iter().any(|n| n == "wafer-run/vector"),
            "{:?}",
            wafer.block_names()
        );
    }

    /// A runtime holding only what a test registers. The statically linked
    /// blocks stay out: `wafer-run/web` requires a storage block these probes
    /// never register, and seal refuses a block whose `requires` is unmet.
    fn empty_wafer() -> Wafer {
        empty_wafer_waiting(InitWait::Await)
    }

    fn empty_wafer_waiting(init_wait: InitWait) -> Wafer {
        Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .init_wait(init_wait)
            .build()
            .expect("build a bare runtime")
    }

    /// Three probes plus the admin id, one of which optionally fails
    /// permanently.
    fn wafer_with_probes(order: &Arc<Mutex<Vec<String>>>, failing: Option<&'static str>) -> Wafer {
        probes_in(
            empty_wafer(),
            order,
            failing.map(|name| (name, ErrorCode::Unknown)),
        )
    }

    /// [`wafer_with_probes`] on a runtime that never initializes in a
    /// dispatch — the one [`InitPolicy::Serving`] boots — with the failing
    /// probe's code chosen.
    fn serving_wafer(
        order: &Arc<Mutex<Vec<String>>>,
        failing: Option<(&'static str, ErrorCode)>,
    ) -> Wafer {
        probes_in(empty_wafer_waiting(InitWait::Refuse), order, failing)
    }

    fn probes_in(
        mut wafer: Wafer,
        order: &Arc<Mutex<Vec<String>>>,
        failing: Option<(&'static str, ErrorCode)>,
    ) -> Wafer {
        let fails = |name: &str| failing.filter(|(f, _)| *f == name).map(|(_, code)| code);
        register_probe(&mut wafer, "test/zeta", order, fails("test/zeta"));
        register_probe(
            &mut wafer,
            crate::blocks::admin::ADMIN_BLOCK_ID,
            order,
            fails(crate::blocks::admin::ADMIN_BLOCK_ID),
        );
        register_probe(&mut wafer, "test/alpha", order, fails("test/alpha"));
        wafer
    }

    fn hooks(order: &Arc<Mutex<Vec<String>>>, fail: bool) -> ProbeHooks {
        ProbeHooks {
            order: order.clone(),
            fail,
        }
    }

    async fn bare_db() -> Arc<dyn DatabaseService> {
        Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        )
    }

    #[tokio::test]
    async fn ordering_is_admin_first_then_seed_then_the_rest_sorted() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = wafer_with_probes(&order, None);

        let report = boot(
            &mut wafer,
            &ProbeHooks {
                order: order.clone(),
                fail: false,
            },
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Tolerant,
        )
        .await
        .expect("boot");

        assert_eq!(
            taken(&order),
            vec![
                crate::blocks::admin::ADMIN_BLOCK_ID.to_string(),
                "seed".to_string(),
                "test/alpha".to_string(),
                "test/zeta".to_string(),
            ],
        );
        assert!(report.ok);
        assert!(report.sealed);
        assert_eq!(
            report
                .blocks
                .iter()
                .filter(|b| b.block == crate::blocks::admin::ADMIN_BLOCK_ID)
                .count(),
            1,
            "admin initializes first and is reported exactly once, not again in the tail",
        );
    }

    /// The hook runs under EVERY policy, including the `Serving` one the
    /// Cloudflare request path uses. Before this funnel existed, that path
    /// called `strict_init_all_blocks` directly and no `BootHooks` value
    /// reached it at all, so `seed_after_admin_init` never ran on a Cloudflare
    /// request build — not because anyone decided it should not (ruling 5.5).
    ///
    /// What each target's hook *does* is that target's decision and not this
    /// funnel's: Cloudflare's request-path hook is deliberately a read and a
    /// republish, with no write in it, while its deploy funnel seeds. The
    /// guarantee here is only that the hook a caller supplies is reached, in
    /// this position, under every policy.
    #[tokio::test]
    async fn every_policy_runs_the_seed_hook_after_admin() {
        for policy in [
            InitPolicy::Tolerant,
            InitPolicy::Serving,
            InitPolicy::Reported,
        ] {
            let order = Arc::new(Mutex::new(Vec::new()));
            let mut wafer = serving_wafer(&order, None);

            boot(
                &mut wafer,
                &ProbeHooks {
                    order: order.clone(),
                    fail: false,
                },
                GrantSource::PreInstalled("test fixture declares none"),
                policy,
            )
            .await
            .unwrap_or_else(|e| panic!("boot under {policy:?}: {e}"));

            let log = taken(&order);
            assert_eq!(
                log.first().map(String::as_str),
                Some(crate::blocks::admin::ADMIN_BLOCK_ID),
                "{policy:?}",
            );
            assert_eq!(log.get(1).map(String::as_str), Some("seed"), "{policy:?}");
        }
    }

    #[tokio::test]
    async fn tolerant_logs_a_failure_and_still_initializes_later_blocks() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = wafer_with_probes(&order, Some("test/alpha"));

        let report = boot(
            &mut wafer,
            &ProbeHooks {
                order: order.clone(),
                fail: false,
            },
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Tolerant,
        )
        .await
        .expect("tolerant boot returns Ok even with a broken block");

        assert!(!report.ok);
        assert!(
            taken(&order).contains(&"test/zeta".to_string()),
            "the block after the failing one must still initialize",
        );
    }

    /// A failed admin `Init` aborts a `Tolerant` boot before the seed hook
    /// and every later block, as a failed seed hook does: admin's `Init`
    /// seeds the shared variables on a provisioning build, so the snapshots
    /// the runtime would serve from are unknown. `Reported` keeps going.
    #[tokio::test]
    async fn a_failed_admin_init_aborts_a_tolerant_boot() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = wafer_with_probes(&order, Some(crate::blocks::admin::ADMIN_BLOCK_ID));
        let error = boot(
            &mut wafer,
            &ProbeHooks {
                order: order.clone(),
                fail: false,
            },
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Tolerant,
        )
        .await
        .expect_err("a failed admin Init aborts a Tolerant boot");
        assert!(
            error
                .to_string()
                .contains(crate::blocks::admin::ADMIN_BLOCK_ID),
            "{error}"
        );
        assert_eq!(
            taken(&order),
            vec![crate::blocks::admin::ADMIN_BLOCK_ID.to_string()],
            "neither the seed hook nor any later block runs"
        );

        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = wafer_with_probes(&order, Some(crate::blocks::admin::ADMIN_BLOCK_ID));
        let report = boot(
            &mut wafer,
            &ProbeHooks {
                order: order.clone(),
                fail: false,
            },
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Reported,
        )
        .await
        .expect("a Reported boot returns its report");
        assert!(!report.ok);
        assert!(taken(&order).contains(&"test/zeta".to_string()));
    }

    /// `Serving` aborts on a block's **permanent** `Init` failure — a config
    /// bug no rebuild cures — with the typed error, and initializes nothing
    /// after it.
    #[tokio::test]
    async fn serving_aborts_on_a_permanent_failure_without_initializing_later_blocks() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = serving_wafer(&order, Some(("test/alpha", ErrorCode::Unknown)));

        let error = boot(
            &mut wafer,
            &hooks(&order, false),
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Serving,
        )
        .await
        .expect_err("a permanent Init failure aborts a Serving boot");

        assert!(
            matches!(
                &error,
                BootError::BlockInit { block, error: InitError::Permanent(_) } if block == "test/alpha"
            ),
            "{error:?}"
        );
        assert!(!error.is_transient());
        assert!(
            !taken(&order).contains(&"test/zeta".to_string()),
            "no block after the failing one may initialize",
        );
    }

    /// The `transient` flags are additive on the wire: written only when
    /// set, and a report from a Worker that predates them reads as every
    /// failure permanent — the class it was answered with.
    #[test]
    fn the_transient_flag_is_written_only_when_set() {
        let transient = BlockInitOutcome::failed(
            "a/b",
            &InitError::Transient(wafer_run::TransientFailure::new("down")),
        );
        assert_eq!(
            serde_json::to_value(&transient).unwrap(),
            serde_json::json!({
                "block": "a/b",
                "ok": false,
                "error": "transient init failure: down",
                "transient": true,
            })
        );
        let permanent = StepOutcome::failed("bad".into(), false);
        assert_eq!(
            serde_json::to_value(&permanent).unwrap(),
            serde_json::json!({"ok": false, "error": "bad"})
        );
        assert_eq!(
            serde_json::to_value(StepOutcome::succeeded()).unwrap(),
            serde_json::json!({"ok": true})
        );
        let older: BlockInitOutcome =
            serde_json::from_value(serde_json::json!({"block": "a/b", "ok": false, "error": "x"}))
                .unwrap();
        assert!(!older.transient);
    }

    /// `Serving` publishes past a block whose `Init` failed **transiently**:
    /// it is listed in `degraded`, every other block initializes, and a
    /// dispatch to it answers `Unavailable` without running its `Init` again
    /// — the runtime is retried by being rebuilt, never in a request.
    #[tokio::test]
    async fn serving_publishes_without_a_block_whose_init_failed_transiently() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = serving_wafer(&order, Some(("test/alpha", ErrorCode::Unavailable)));

        let report = boot(
            &mut wafer,
            &hooks(&order, false),
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Serving,
        )
        .await
        .expect("a transient Init failure publishes degraded");

        assert!(!report.ok);
        assert_eq!(
            report
                .degraded
                .iter()
                .map(|d| d.block.as_str())
                .collect::<Vec<_>>(),
            vec!["test/alpha"]
        );
        assert!(
            taken(&order).contains(&"test/zeta".to_string()),
            "the block after the degraded one initializes",
        );
        let inits_before = taken(&order).len();

        let healthy = wafer
            .run_block("test/zeta", Message::new("probe"), InputStream::empty())
            .await
            .collect_buffered()
            .await;
        assert!(healthy.is_ok(), "a healthy block serves: {healthy:?}");

        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let refused = wafer
            .run_block("test/alpha", Message::new("probe"), InputStream::empty())
            .await
            .collect_buffered()
            .await
            .expect_err("a degraded block answers an error");
        assert!(
            matches!(&refused, wafer_block::TerminalNotResponse::Error(e) if e.code == ErrorCode::Unavailable),
            "{refused:?}"
        );
        assert_eq!(
            taken(&order).len(),
            inits_before,
            "the dispatch ran no Init: {:?}",
            taken(&order)
        );
    }

    /// `Serving` refuses a runtime that would run an `Init` in a dispatch,
    /// before anything runs: the pair cannot be split by a builder flag.
    #[tokio::test]
    async fn serving_refuses_an_await_runtime_before_any_init() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = wafer_with_probes(&order, None);

        let error = boot(
            &mut wafer,
            &hooks(&order, false),
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Serving,
        )
        .await
        .expect_err("Serving needs InitWait::Refuse");

        assert!(matches!(error, BootError::Runtime(_)), "{error:?}");
        assert!(!error.is_transient());
        assert!(
            taken(&order).is_empty(),
            "no Init and no hook ran: {:?}",
            taken(&order)
        );
    }

    /// Admin is a precondition under `Serving`: even a transient admin
    /// failure aborts (transiently), before the seed hook and every later
    /// block — the hook would read tables admin did not create.
    #[tokio::test]
    async fn a_failed_admin_init_aborts_a_serving_boot() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let admin = crate::blocks::admin::ADMIN_BLOCK_ID;
        let mut wafer = serving_wafer(&order, Some((admin, ErrorCode::Unavailable)));

        let error = boot(
            &mut wafer,
            &hooks(&order, false),
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Serving,
        )
        .await
        .expect_err("a failed admin Init aborts a Serving boot");

        assert!(
            matches!(
                &error,
                BootError::BlockInit { block, error: InitError::Transient(_) } if block == admin
            ),
            "{error:?}"
        );
        assert!(error.is_transient());
        assert_eq!(
            taken(&order),
            vec![admin.to_string()],
            "neither the seed hook nor any later block runs"
        );
    }

    #[tokio::test]
    async fn reported_captures_every_block_and_keeps_going() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = wafer_with_probes(&order, Some("test/alpha"));

        let report = boot(
            &mut wafer,
            &ProbeHooks {
                order: order.clone(),
                fail: false,
            },
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Reported,
        )
        .await
        .expect("reported boot returns Ok");

        assert!(!report.ok);
        let failed: Vec<&str> = report
            .blocks
            .iter()
            .filter(|b| !b.ok)
            .map(|b| b.block.as_str())
            .collect();
        assert_eq!(failed, vec!["test/alpha"]);
        assert!(
            report.blocks.iter().any(|b| b.block == "test/zeta" && b.ok),
            "the block after the failing one is still attempted and reported",
        );
    }

    #[tokio::test]
    async fn seed_failure_is_fatal_except_under_reported() {
        for policy in [InitPolicy::Tolerant, InitPolicy::Serving] {
            let order = Arc::new(Mutex::new(Vec::new()));
            let mut wafer = serving_wafer(&order, None);

            let error = boot(
                &mut wafer,
                &ProbeHooks {
                    order: order.clone(),
                    fail: true,
                },
                GrantSource::PreInstalled("test fixture declares none"),
                policy,
            )
            .await
            .expect_err(&format!("seed failure must abort under {policy:?}"));
            assert!(
                matches!(&error, BootError::Seed(SeedError::Fault(_))),
                "{policy:?}: {error:?}"
            );
            assert!(error.to_string().contains("deliberate seed failure"));
        }

        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = wafer_with_probes(&order, None);
        let report = boot(
            &mut wafer,
            &ProbeHooks {
                order: order.clone(),
                fail: true,
            },
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Reported,
        )
        .await
        .expect("reported boot captures a seed failure");
        assert!(!report.seed.ok);
        assert!(!report.ok);
        assert_eq!(
            report.seed.error.as_deref(),
            Some("deliberate seed failure")
        );
    }

    /// `GrantSource::Database` and `GrantSource::PreInstalled` (over the same
    /// rows, installed through the builder) must leave the runtime holding the
    /// same grants — otherwise the choice would be a behavioural fork rather
    /// than a statement of provenance.
    #[tokio::test]
    async fn database_and_pre_installed_grants_agree() {
        let db = bare_db().await;
        crate::test_support::migrate_sqlite(
            &db,
            crate::blocks::admin::ADMIN_BLOCK_ID,
            crate::blocks::admin::migrations::SQLITE_MIGRATIONS,
        )
        .await
        .expect("apply admin migrations");
        crate::platform_state::wrap_grants::seed_fixture_grant(&db).await;

        let order = Arc::new(Mutex::new(Vec::new()));
        let mut from_db = wafer_with_probes(&order, None);
        boot(
            &mut from_db,
            &ProbeHooks {
                order: order.clone(),
                fail: false,
            },
            GrantSource::Database(&db),
            InitPolicy::Tolerant,
        )
        .await
        .expect("boot from database grants");

        let expected = crate::platform_state::wrap_grants::load(&db)
            .await
            .expect("load the fixture grant");
        assert_eq!(expected.len(), 1, "fixture seeds exactly one grant");

        let order2 = Arc::new(Mutex::new(Vec::new()));
        let mut pre_installed = wafer_with_probes(&order2, None);
        pre_installed
            .add_wrap_grants(expected.clone())
            .expect("the loaded grants are well-formed");
        boot(
            &mut pre_installed,
            &ProbeHooks {
                order: order2,
                fail: false,
            },
            GrantSource::PreInstalled("installed above, standing in for the builder"),
            InitPolicy::Tolerant,
        )
        .await
        .expect("boot with pre-installed grants");

        assert_eq!(
            format!("{:?}", from_db.wrap_grants()),
            format!("{:?}", pre_installed.wrap_grants()),
        );
        assert!(
            format!("{:?}", from_db.wrap_grants())
                .contains(crate::platform_state::wrap_grants::FIXTURE_RESOURCE),
            "the seeded grant must reach the sealed runtime",
        );
    }

    /// `GrantSource::PreInstalled` must not go looking in a database — that is
    /// the whole reason the prepared Cloudflare hydration can stay off D1.
    #[tokio::test]
    async fn pre_installed_does_not_read_the_database() {
        let db = bare_db().await;
        crate::test_support::migrate_sqlite(
            &db,
            crate::blocks::admin::ADMIN_BLOCK_ID,
            crate::blocks::admin::migrations::SQLITE_MIGRATIONS,
        )
        .await
        .expect("apply admin migrations");
        crate::platform_state::wrap_grants::seed_fixture_grant(&db).await;

        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = wafer_with_probes(&order, None);
        boot(
            &mut wafer,
            &ProbeHooks { order, fail: false },
            GrantSource::PreInstalled("the plan is authoritative"),
            InitPolicy::Tolerant,
        )
        .await
        .expect("boot");

        assert!(
            !format!("{:?}", wafer.wrap_grants())
                .contains(crate::platform_state::wrap_grants::FIXTURE_RESOURCE),
            "PreInstalled must not load the database's grants behind the caller's back",
        );
    }

    /// A grants read that failed is a failed step, never "no grants": the
    /// request path's `Serving` boot refuses to build a runtime that would
    /// deny every access the admin-created grants allow, and keeps the
    /// database's error typed, so an outage (here) is transient — a 503 — and
    /// anything else a 500.
    #[tokio::test]
    async fn a_failed_grants_read_fails_a_serving_boot() {
        let db = crate::platform_state::wrap_grants::fixture_with_failing_reads().await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = serving_wafer(&order, None);
        let error = boot(
            &mut wafer,
            &ProbeHooks {
                order: order.clone(),
                fail: false,
            },
            GrantSource::Database(&db),
            InitPolicy::Serving,
        )
        .await
        .expect_err("a Serving boot must not build without its grants");
        assert!(
            matches!(&error, BootError::Grants(DatabaseError::Unavailable(_))),
            "{error:?}"
        );
        assert!(error.is_transient());
        assert!(
            error
                .to_string()
                .contains("load the admin-created WRAP grants")
                && error.to_string().contains("injected outage"),
            "{error}"
        );
        assert!(
            order.lock().unwrap().is_empty(),
            "the boot stops before any block initializes: {:?}",
            order.lock().unwrap()
        );
    }

    /// Under `Reported` the failure is captured in its own step and fails
    /// the report, so `/_deploy/init` and `/_deploy/prepare` answer 500 and
    /// the CLI refuses the deploy; under `Tolerant` (the browser) it is
    /// recorded too and the runtime serves on without the grants.
    #[tokio::test]
    async fn a_failed_grants_read_is_reported_or_tolerated_by_policy() {
        for policy in [InitPolicy::Reported, InitPolicy::Tolerant] {
            let db = crate::platform_state::wrap_grants::fixture_with_failing_reads().await;
            let order = Arc::new(Mutex::new(Vec::new()));
            let mut wafer = wafer_with_probes(&order, None);
            let report = boot(
                &mut wafer,
                &ProbeHooks {
                    order: order.clone(),
                    fail: false,
                },
                GrantSource::Database(&db),
                policy,
            )
            .await
            .unwrap_or_else(|e| panic!("{policy:?} captures the failure: {e}"));
            assert!(!report.grants.ok, "{policy:?}: {report:?}");
            assert!(
                report
                    .grants
                    .error
                    .as_deref()
                    .is_some_and(|e| e.contains("injected outage")),
                "{policy:?}: {report:?}"
            );
            assert!(
                !report.ok,
                "{policy:?}: a failed grants step fails the report"
            );
            assert!(
                report.sealed && report.blocks.iter().all(|b| b.ok),
                "{policy:?}"
            );
            assert!(
                !format!("{:?}", wafer.wrap_grants())
                    .contains(crate::platform_state::wrap_grants::FIXTURE_RESOURCE),
                "{policy:?}: no grant was read, so none is installed",
            );
        }
    }

    /// The `/_deploy/init` response body is a wire contract with
    /// `impresspress deploy`, which deserializes this very type. Field names
    /// are therefore checked by the compiler; what still needs asserting is
    /// the *shape* — the five keys and the `skip_serializing_if` that keeps a
    /// successful step's `error` off the wire entirely.
    #[tokio::test]
    async fn report_serializes_the_deploy_wire_shape() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = wafer_with_probes(&order, Some("test/alpha"));
        let report = boot(
            &mut wafer,
            &ProbeHooks { order, fail: false },
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Reported,
        )
        .await
        .expect("boot");

        let json = serde_json::to_value(&report).expect("serialize report");
        let object = json.as_object().expect("report is a JSON object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["blocks", "grants", "ok", "sealed", "seed"]);
        assert_eq!(
            json["seed"]
                .as_object()
                .expect("seed is an object")
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["ok"],
            "a successful step omits `error` entirely",
        );
        let failed = json["blocks"]
            .as_array()
            .expect("blocks is an array")
            .iter()
            .find(|b| b["block"] == "test/alpha")
            .expect("the failing block is reported");
        let mut block_keys: Vec<&str> = failed
            .as_object()
            .expect("block outcome is an object")
            .keys()
            .map(String::as_str)
            .collect();
        block_keys.sort_unstable();
        assert_eq!(block_keys, vec!["block", "error", "ok"]);
    }

    /// A block whose `Init` failed once, at boot, with a transient error.
    struct FlakyInitBlock {
        attempts: Arc<Mutex<u32>>,
    }

    #[wafer_block::wafer_async_trait]
    impl Block for FlakyInitBlock {
        fn info(&self) -> BlockInfo {
            BlockInfo::new("test/flaky", "0.1.0", "test/init@v1", "test")
        }

        async fn lifecycle(
            &self,
            _ctx: &dyn wafer_block::context::Context,
            event: LifecycleEvent,
        ) -> Result<(), WaferError> {
            if event.event_type == LifecycleType::Init {
                let mut attempts = self.attempts.lock().unwrap_or_else(|p| p.into_inner());
                *attempts += 1;
                if *attempts == 1 {
                    return Err(WaferError::new(
                        ErrorCode::Unavailable,
                        "database is locked",
                    ));
                }
            }
            Ok(())
        }

        async fn handle(
            &self,
            _ctx: &dyn wafer_block::context::Context,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            OutputStream::respond(b"served".to_vec())
        }
    }

    /// A block whose `Init` hits a transient fault during a tolerant boot is
    /// not wedged: the boot reports the failure and serves on, and the
    /// block's first request after the runtime's backoff runs `Init` again
    /// and is served. A permanent failure would leave it refusing every
    /// request until the process restarted.
    #[tokio::test]
    async fn a_transient_init_failure_at_boot_is_retried_by_the_first_request() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = wafer_with_probes(&order, None);
        let attempts = Arc::new(Mutex::new(0));
        wafer
            .register_block(
                "test/flaky",
                Arc::new(FlakyInitBlock {
                    attempts: attempts.clone(),
                }),
            )
            .unwrap();

        let report = boot(
            &mut wafer,
            &ProbeHooks { order, fail: false },
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Tolerant,
        )
        .await
        .expect("a tolerant boot serves on");
        assert!(
            report
                .blocks
                .iter()
                .any(|b| b.block == "test/flaky" && !b.ok),
            "the boot reports the failed Init: {report:?}"
        );

        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let served = wafer
            .run_block("test/flaky", Message::new("probe"), InputStream::empty())
            .await
            .collect_buffered()
            .await
            .expect("the block is served once its Init succeeds");
        assert_eq!(served.body, b"served");
        assert_eq!(
            *attempts.lock().unwrap(),
            2,
            "Init ran again on the request"
        );
    }
}
