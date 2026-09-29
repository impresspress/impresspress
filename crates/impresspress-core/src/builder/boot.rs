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
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_run::{RuntimeError, Wafer};

/// Request-current config flag set only while an authenticated deployment
/// candidate is exporting a prepared runtime plan.
///
/// Consumer blocks whose `Init` lifecycle also publishes mutable derived
/// output (for example a pre-rendered homepage) should still run migrations
/// and seeds, but defer that publication while this value is `"1"`. This
/// keeps the currently promoted Worker and its mutable objects unchanged
/// until the prepared candidate has passed verification and promotion.
pub const PREPARE_RUNTIME_PLAN_KEY: &str = "IMPRESSPRESS_PREPARE_RUNTIME_PLAN";

/// What a block `Init` failure (or a seed-hook failure) *does*. The ordering
/// [`boot`] runs is identical under all three; only the consequence differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitPolicy {
    /// Log and continue. The runtime is published even with a broken block, so
    /// one misconfigured block cannot wedge a whole server. Used by the
    /// long-lived targets (native, browser), which can be inspected and fixed
    /// in place.
    Tolerant,
    /// Fail closed on the first failure. Used by the Cloudflare request path:
    /// publishing a runtime with a failed lazy-init slot would let concurrent
    /// requests wait on one another's init future, which is not a valid
    /// execution model for a request-isolated platform.
    Strict,
    /// Capture every outcome and keep going, so the caller can render the full
    /// picture. Used by `/_deploy/init`, whose whole product is the report.
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
    /// before seal. Missing table / read errors degrade to no dynamic grants —
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
pub const DEPLOY_RESPONSE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepOutcome {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockInitOutcome {
    pub block: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
/// cannot change the meaning of the four flags the deploy is gated on, so
/// refusing it would force a lockstep CLI upgrade for a purely additive
/// change. The envelope itself stays strict.
///
/// `/_deploy/init` serializes the report as the whole body, with no envelope
/// and no version — but nothing parses that body: `impresspress serve
/// --target cloudflare` reads it as text and prints it for the operator. If a
/// consumer of that shape ever appears it needs the envelope, not a relaxed
/// argument.
///
/// [`InitPolicy::Tolerant`] and [`InitPolicy::Strict`] callers may ignore it:
/// under `Strict` a failure is an `Err` instead, and under `Tolerant` the same
/// failures are logged.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootReport {
    pub sealed: bool,
    pub seed: StepOutcome,
    pub blocks: Vec<BlockInitOutcome>,
    /// True iff every step and every block init succeeded.
    pub ok: bool,
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
    /// Under [`InitPolicy::Tolerant`] and [`InitPolicy::Strict`] an `Err`
    /// aborts the boot; under [`InitPolicy::Reported`] it is captured in
    /// [`BootReport::seed`]. Return `Err` for a genuinely fatal condition (for
    /// example, structural settings could not be persisted consistently, or a
    /// required secret cannot be read). Optional best-effort per-key seeds
    /// should still log and continue inside the implementation.
    ///
    /// Receives `&mut Wafer` so a target that could not know its settings
    /// before admin migration (browser/Cloudflare first deploy) can publish the
    /// seeded values into the config snapshot before any other block initializes.
    async fn seed_after_admin_init(&self, wafer: &mut Wafer) -> Result<(), String>;
}

/// The one post-`build()` lifecycle, for every target:
///
/// 1. `grants` — register deployment-owned WRAP grants. MUST precede `seal`.
/// 2. `seal()` — finalize composite/uses/capability/snapshot wiring.
/// 3. `init_block(admin)` FIRST — admin's migrations create the variables /
///    block_settings tables before any other block's `Init` writes to them,
///    and before the seed step reads them.
/// 4. `hooks.seed_after_admin_init` — seed + publish (see [`BootHooks`]).
/// 5. every remaining block, in `Wafer::block_names()` order (sorted, so the
///    sequence is identical across processes and platforms).
///
/// `policy` changes none of that ordering; it decides only what a failure at
/// step 3, 4 or 5 does. See [`InitPolicy`].
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
) -> Result<BootReport, RuntimeError> {
    // 1. Deployment-owned WRAP grants, before seal — `add_wrap_grants` after
    //    seal is silently ignored.
    match grants {
        GrantSource::Database(db) => {
            let loaded = crate::platform_state::wrap_grants::load(db).await;
            if !loaded.is_empty() {
                tracing::info!(count = loaded.len(), "registering database WRAP grants");
                wafer.add_wrap_grants(loaded)?;
            }
        }
        GrantSource::PreInstalled(_because) => {}
    }

    // 2. Seal.
    wafer.seal().await?;

    let admin_id = crate::blocks::admin::ADMIN_BLOCK_ID;
    let names = wafer.block_names();
    let mut blocks = Vec::new();

    // 3. Admin first — its Init creates the variables / block_settings tables
    //    the seed step reads, and migration 002's `block` column the auto-gen
    //    seeder writes.
    if names.iter().any(|name| name == admin_id) {
        blocks.push(init_one(wafer, admin_id, policy).await?);
    }

    // 4. Seed + publish.
    let seed = match hooks.seed_after_admin_init(wafer).await {
        Ok(()) => StepOutcome {
            ok: true,
            error: None,
        },
        Err(e) => match policy {
            InitPolicy::Reported => StepOutcome {
                ok: false,
                error: Some(e),
            },
            InitPolicy::Tolerant | InitPolicy::Strict => return Err(RuntimeError::Config(e)),
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
        blocks.push(init_one(wafer, name, policy).await?);
    }

    let ok = seed.ok && blocks.iter().all(|b| b.ok);
    Ok(BootReport {
        sealed: true,
        seed,
        blocks,
        ok,
    })
}

/// Initialize one block and turn the outcome into what `policy` asks for.
/// `Strict` is the only arm that can return `Err`.
async fn init_one(
    wafer: &Wafer,
    name: &str,
    policy: InitPolicy,
) -> Result<BlockInitOutcome, RuntimeError> {
    match wafer.init_block(name).await {
        Ok(_) => Ok(BlockInitOutcome {
            block: name.to_string(),
            ok: true,
            error: None,
        }),
        Err(e) => match policy {
            InitPolicy::Strict => Err(RuntimeError::Config(format!(
                "block `{name}` Init failed: {e}"
            ))),
            InitPolicy::Tolerant => {
                tracing::error!(
                    block = %name,
                    error = %e,
                    "block init lifecycle failed during boot",
                );
                Ok(BlockInitOutcome {
                    block: name.to_string(),
                    ok: false,
                    error: Some(e.to_string()),
                })
            }
            InitPolicy::Reported => Ok(BlockInitOutcome {
                block: name.to_string(),
                ok: false,
                error: Some(e.to_string()),
            }),
        },
    }
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
        fail: bool,
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
                if self.fail {
                    return Err(WaferError::new(
                        ErrorCode::Unknown,
                        "deliberate Init failure",
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
        async fn seed_after_admin_init(&self, _wafer: &mut Wafer) -> Result<(), String> {
            push(&self.order, "seed");
            if self.fail {
                return Err("deliberate seed failure".to_string());
            }
            Ok(())
        }
    }

    fn register_probe(
        wafer: &mut Wafer,
        name: &'static str,
        order: &Arc<Mutex<Vec<String>>>,
        fail: bool,
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
        Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("build a bare runtime")
    }

    /// Three probes plus the admin id, one of which optionally fails.
    fn wafer_with_probes(order: &Arc<Mutex<Vec<String>>>, failing: Option<&'static str>) -> Wafer {
        let mut wafer = empty_wafer();
        register_probe(&mut wafer, "test/zeta", order, failing == Some("test/zeta"));
        register_probe(
            &mut wafer,
            crate::blocks::admin::ADMIN_BLOCK_ID,
            order,
            failing == Some(crate::blocks::admin::ADMIN_BLOCK_ID),
        );
        register_probe(
            &mut wafer,
            "test/alpha",
            order,
            failing == Some("test/alpha"),
        );
        wafer
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

    /// The hook runs under EVERY policy, including the `Strict` one the
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
            InitPolicy::Strict,
            InitPolicy::Reported,
        ] {
            let order = Arc::new(Mutex::new(Vec::new()));
            let mut wafer = wafer_with_probes(&order, None);

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

    #[tokio::test]
    async fn strict_fails_closed_without_initializing_later_blocks() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut wafer = wafer_with_probes(&order, Some("test/alpha"));

        let error = boot(
            &mut wafer,
            &ProbeHooks {
                order: order.clone(),
                fail: false,
            },
            GrantSource::PreInstalled("test fixture declares none"),
            InitPolicy::Strict,
        )
        .await
        .expect_err("strict boot must fail closed");

        assert!(error.to_string().contains("test/alpha"), "{error}");
        assert!(
            !taken(&order).contains(&"test/zeta".to_string()),
            "no block after the failing one may initialize",
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
        for policy in [InitPolicy::Tolerant, InitPolicy::Strict] {
            let order = Arc::new(Mutex::new(Vec::new()));
            let mut wafer = wafer_with_probes(&order, None);

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
        crate::migration_helper::apply_ddl_via_service(
            &db,
            crate::blocks::admin::migrations::ddl_files("sqlite"),
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

        let expected = crate::platform_state::wrap_grants::load(&db).await;
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
        crate::migration_helper::apply_ddl_via_service(
            &db,
            crate::blocks::admin::migrations::ddl_files("sqlite"),
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
            InitPolicy::Strict,
        )
        .await
        .expect("boot");

        assert!(
            !format!("{:?}", wafer.wrap_grants())
                .contains(crate::platform_state::wrap_grants::FIXTURE_RESOURCE),
            "PreInstalled must not load the database's grants behind the caller's back",
        );
    }

    /// The `/_deploy/init` response body is a wire contract with
    /// `impresspress deploy`, which deserializes this very type. Field names
    /// are therefore checked by the compiler; what still needs asserting is
    /// the *shape* — the four keys and the `skip_serializing_if` that keeps a
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
        assert_eq!(keys, vec!["blocks", "ok", "sealed", "seed"]);
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
