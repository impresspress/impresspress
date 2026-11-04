//! What a cold Cloudflare isolate sends D1 on boot and on its first requests,
//! pinned as a golden statement list (`tests/d1_read_cost.golden`).
//!
//! The harness drives the hosted target's own funnels
//! (`impresspress_core::builder::{boot_deploy_runtime, seal_prepared_plan,
//! boot_request_runtime}`), the config source every hosted build uses
//! (`request_config_source`) over the overlay the structural pass builds
//! (`structural_runtime_config`, with the bound JWT secret), and the
//! `site-main` dispatch a Worker runs — over the platform SQLite service
//! behind [`D1Shaped`], which records every statement D1 would be sent. Each
//! phase that stands for a cold isolate gets a fresh [`D1Shaped`], so its
//! schema cache is cold, as an isolate's is.
//!
//! Phases, per seed variant (`mixed`, `shared`):
//!
//! 1. **provision** — `boot_deploy_runtime` (`RUN_MIGRATIONS=1`, `Reported`,
//!    the deploy hooks) over an empty database, then `seal_prepared_plan`;
//!    the plan is exported. Recorded once (the `mixed` run's).
//!    - **1b** — a prepared build before the first account, with a bootstrap
//!      token forwarded as request config, as a consumer that binds the
//!      token after the first deploy forwards it: the token is installed.
//! 2. **seed** — fixture rows, uncounted: legal documents, users, variables,
//!    grants, an admin session. `shared` deletes every block-scoped variables
//!    row — products' auto-generated webhook secret and the three auth
//!    defaults admin's settings seed stores under their blocks — and forwards
//!    them as request config (see the golden's header).
//! 3. **database build** — `boot_request_runtime(RequestBuild::Database)`,
//!    with the build's own pre-`build()` `block_settings` read.
//!    - **3b** — the same with admin's `seed_defaults_hash` stale, as a
//!      release that changes a declared shared variable leaves it before its
//!      deploy: the request path still seeds nothing.
//! 4. **prepared build** — `boot_request_runtime(RequestBuild::Prepared)`,
//!    hydrated from phase 1's plan.
//!    - **4b** — the same, with a bootstrap email and password forwarded
//!      while accounts exist: what a still-bound credential costs.
//!    - **4c** — the same, with only the bootstrap email forwarded: the
//!      normal leftover, which costs nothing.
//! 5. **warm** — five requests on runtime 4 (its warm schema cache): `GET /`,
//!    the terms page, `/health`, an authenticated admin page, and a 404. Each
//!    counts the statements its response waited on and the ones it left to
//!    run after it (its audit row and deferred tasks).
//! 6. **window retry, gate fails** — the Worker's retry window checking a
//!    database whose variables table is down before it builds anything
//!    (`VariablesSnapshot::check_backend`, which `runtime_build::read_variables` calls): the first check after a publish (a cold schema cache), and
//!    **6b** a later one, which sends the same (the outage fails the
//!    introspection, so nothing is memoized). Only the variables table is
//!    read.
//!
//! Every request-path build runs that gate first (phases 1b, 3, 3b, 4–4c):
//! it is the read the boot's config-declaring blocks would make, moved in
//! front of assembly and handed to the build, so those phases send what they
//! sent before the gate.
//!
//! The golden holds, per phase, `(table, class, statements, rows≈)`; verbatim
//! SQL appears only in failure messages. `BLESS=1` rewrites it. Independent of
//! the golden, the rules a later change must not break are asserted directly
//! (see [`check_rules`]).
//!
//! It runs with exactly the blocks a Cloudflare build can compile — every
//! `block-*` feature `impresspress-cloudflare` passes through, as
//! `migration_history.rs`'s budget counts them — and without what a Worker
//! never has: the native `llm` provider backend (whose `Init` reloads the
//! providers table), the dev sandbox and the native embedding blocks. The
//! golden names the block set it was blessed with. Any other feature set
//! boots a different runtime, so this file is compiled out there, and CI runs
//! it on its own:
//!
//! ```text
//! cargo test -p impresspress-core --no-default-features --features \
//!   test-support,block-files,block-messages,block-vector,block-llm,block-legalpages,block-tickets,block-userportal,block-products,block-signal \
//!   --test d1_read_cost
//! ```
//!
//! (`just d1-cost`; `BLESS=1 just d1-cost` rewrites the golden.)

#![cfg(all(
    feature = "block-legalpages",
    not(any(
        feature = "llm",
        feature = "block-dev",
        feature = "site-runtime",
        feature = "native-embedding",
        feature = "block-fastembed",
    ))
))]

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, RwLock},
    time::Duration,
};

use impresspress_core::{
    after_response::{self, AfterResponse},
    blocks::auth::{
        config::{
            BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY, BOOTSTRAP_ADMIN_TOKEN_KEY,
        },
        repo as auth_repo, JWT_SECRET_KEY,
    },
    builder::{
        add_request_config, boot_deploy_runtime, boot_request_runtime, request_config_source,
        seal_prepared_plan, structural_runtime_config, warm_config_surfaces, ImpresspressBuilder,
        PreparedPlanExporter, RequestBuild, RuntimeConfig, StructuralConfigInputs,
        PREPARE_RUNTIME_PLAN_KEY,
    },
    config_source::{RetryMode, VariablesSnapshot},
    deferred::{self, DeferMode},
    features::BlockSettings,
    platform_state::{block_settings, user_roles, variables, wrap_grants},
    test_support::{
        d1_shaped::{D1Shaped, Fault, Ledger, RoundTrip, Sent},
        InMemoryStorageService,
    },
    PreparedReleaseAssets, PreparedRuntimePlan, WaferLockIdentity,
};
use serde_json::{json, Value};
use wafer_block::{http_codec, InputStream};
use wafer_block_sqlite::service::SQLiteDatabaseService;
use wafer_core::interfaces::{
    config::service::ConfigService,
    database::service::{DatabaseError, DatabaseService},
    network::service::{NetworkError, NetworkService, Request, Response},
};
use wafer_run::Wafer;
use wafer_sql_utils::Backend;

/// The bound Worker secret every build carries (`PROTECTED_ENV_KEYS`): on
/// both config surfaces and in the config source's overlay.
const JWT_SECRET: &str = "d1-read-cost-harness-jwt-secret-0123456789abcdef";
const APP_ID: &str = "d1-read-cost-harness";
const ADMIN_EMAIL: &str = "admin@example.com";
const ADMIN_FAMILY: &str = "fam-d1-read-cost";
/// legalpages' table; its repo module is private to the block.
const LEGAL_DOCUMENTS_TABLE: &str = "impresspress__legalpages__documents";

/// The seed sizes (gdsf's measured shape, D1-READ-COST-PLAN §0).
const LEGAL_DOCUMENTS: usize = 300;
const USERS: usize = 500;
const SHARED_VARIABLES: usize = 18;
const GRANTS: usize = 3;
/// gdsf's 4 `GDSF__SITE` rows, as rows real blocks declare.
const BLOCK_VARIABLES: [(&str, &str); 4] = [
    ("IMPRESSPRESS__SIGNAL__ROOM_TTL_SECONDS", "600"),
    ("IMPRESSPRESS__SIGNAL__MAX_SDP_BYTES", "16384"),
    (
        "IMPRESSPRESS__SIGNAL__STUN_URLS",
        "stun:stun.example.com:3478",
    ),
    (
        "IMPRESSPRESS__TICKETS__SUPPORT_EMAIL",
        "support@example.com",
    ),
];

/// Held for the whole body of every test here: the variables snapshot's memo
/// is keyed on the process-wide config-write generation, and
/// [`an_outage_leaves_a_page_reading_no_stored_config_serving`] moves it,
/// which would make the golden run's snapshots read again mid-phase.
static GENERATION: std::sync::LazyLock<futures::lock::Mutex<()>> =
    std::sync::LazyLock::new(|| futures::lock::Mutex::new(()));

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/d1_read_cost.golden")
}

// ---------------------------------------------------------------------------
// The runtime, assembled as `impresspress-cloudflare`'s `build_runtime` does.
// ---------------------------------------------------------------------------

/// A network service that refuses: nothing a boot or these requests do
/// should leave the Worker.
struct NoNetwork;

#[wafer_block::wafer_async_trait]
impl NetworkService for NoNetwork {
    async fn do_request(&self, req: &Request) -> Result<Response, NetworkError> {
        Err(NetworkError::Other(format!(
            "the D1 cost harness has no network: {}",
            req.url
        )))
    }
}

/// What a built runtime hands the funnels.
struct Built {
    wafer: Wafer,
    exporter: PreparedPlanExporter,
    block_settings: Arc<RwLock<BlockSettings>>,
    config: Arc<dyn ConfigService>,
    source: Arc<RequestScopedSource>,
    variables: Arc<VariablesSnapshot>,
}

/// The config source a built runtime holds, standing in for the Worker's
/// `config_source_proxy()`: it forwards to whichever source the current
/// request installed. On Cloudflare that is a fresh `DatabaseConfigSource` per
/// request (`warm_request_services`), over the runtime's variables snapshot;
/// the harness installs one the same way before each warm request
/// ([`warm_source`]), so a warm request that resolved a block's config would
/// show here whether it read the table again.
struct RequestScopedSource(RwLock<Arc<dyn wafer_run::ConfigSource>>);

impl RequestScopedSource {
    fn install(&self, source: Arc<dyn wafer_run::ConfigSource>) {
        *self.0.write().unwrap_or_else(|p| p.into_inner()) = source;
    }
}

#[wafer_block::wafer_async_trait]
impl wafer_run::ConfigSource for RequestScopedSource {
    async fn load_for_block(
        &self,
        block: &str,
        declared_keys: &[wafer_run::ConfigVar],
    ) -> Result<wafer_run::EnvBlockConfig, wafer_run::ConfigError> {
        let current = self.0.read().unwrap_or_else(|p| p.into_inner()).clone();
        current.load_for_block(block, declared_keys).await
    }
}

/// The Worker environment's bound keys, as `CfEnvironment::config_map` gives
/// them: the JWT secret, and no builder-time var.
fn bound() -> HashMap<String, String> {
    HashMap::from([(JWT_SECRET_KEY.to_string(), JWT_SECRET.to_string())])
}

/// A warm request's config source, built as `warm_request_services` builds it:
/// the warm surfaces over the cached runtime's config snapshot, then a fresh
/// `request_config_source` over this request's overlay and the runtime's
/// variables snapshot.
fn warm_source(
    db: &Arc<dyn DatabaseService>,
    wafer: &Wafer,
    variables: &Arc<VariablesSnapshot>,
    request_config: &HashMap<String, String>,
) -> Arc<dyn wafer_run::ConfigSource> {
    let (_config_map, overlay) =
        warm_config_surfaces(&bound(), wafer.config_snapshot(), request_config);
    request_config_source(db.clone(), overlay, variables.clone())
}

/// The plan identity a Worker version carries, fixed for the harness.
fn identity() -> (String, WaferLockIdentity, PreparedReleaseAssets) {
    let sha = format!("sha256:{}", "b".repeat(64));
    let lock = WaferLockIdentity::from_digest(2, format!("sha256:{}", "a".repeat(64)))
        .expect("a lock identity");
    (sha, lock, PreparedReleaseAssets::absent())
}

/// Build (not boot) a runtime over `db` the way `build_runtime` does: the
/// `block_settings` read (or the plan's), the structural config pass with the
/// bound JWT secret, the consumer's request config, the runtime's variables
/// snapshot, the config source over the resulting overlay and that snapshot,
/// the builder (handed the same snapshot). The config passes and the source are the
/// production functions; the sequence around them is `build_runtime`'s,
/// copied, because that function needs a `worker::Env` (the golden's header
/// names what the copy cannot see).
///
/// What differs from the Worker is otherwise only what the Worker binds:
/// services are handed to the builder directly instead of through the
/// request-scoped proxies (the config source excepted, see
/// [`RequestScopedSource`]), the storage is in memory, and the network
/// refuses.
///
/// `gated` runs the request path's gate first, before the settings, as the
/// Worker's slot owner does (`VariablesSnapshot::check_backend`, which
/// `runtime_build::read_variables` calls); the deploy funnel is never gated.
async fn build(
    db: &Arc<dyn DatabaseService>,
    prepared: Option<&PreparedRuntimePlan>,
    provisioning: bool,
    request_config: &HashMap<String, String>,
    variables: Arc<VariablesSnapshot>,
    gated: bool,
) -> Built {
    try_build(db, prepared, provisioning, request_config, variables, gated)
        .await
        .expect("the build's block_settings read")
}

/// [`build`], answering a failed `block_settings` read — the dynamic build's
/// one read of its own, as `runtime_build::read_block_settings` — rather
/// than panicking on it.
async fn try_build(
    db: &Arc<dyn DatabaseService>,
    prepared: Option<&PreparedRuntimePlan>,
    provisioning: bool,
    request_config: &HashMap<String, String>,
    variables: Arc<VariablesSnapshot>,
    gated: bool,
) -> Result<Built, DatabaseError> {
    if gated {
        variables
            .check_backend(db)
            .await
            .expect("the gate reads the variables table");
    }
    let settings = match prepared {
        Some(plan) => BlockSettings::from_blocks(
            plan.structure
                .block_settings
                .iter()
                .map(|(name, state)| (name.clone(), state.clone()))
                .collect(),
        ),
        None => block_settings::load(db).await?,
    };
    let (mut runtime_config, mut overlay) = structural_runtime_config(StructuralConfigInputs {
        secrets: vec![(JWT_SECRET_KEY, JWT_SECRET.to_string())],
        worker_vars: Vec::new(),
        block_settings_json: settings.to_config_json(),
        run_migrations: provisioning,
    });
    let mut request_config = request_config.clone();
    if provisioning {
        // `/_deploy/prepare` adds its flag to the request config, and the
        // Worker then retains it on both surfaces
        // (`retain_prepare_runtime_plan_flag`).
        request_config.insert(PREPARE_RUNTIME_PLAN_KEY.to_string(), "1".to_string());
    }
    add_request_config(&mut runtime_config, &mut overlay, &request_config);
    if provisioning {
        runtime_config.both(PREPARE_RUNTIME_PLAN_KEY, "1");
    }
    let source = Arc::new(RequestScopedSource(RwLock::new(request_config_source(
        db.clone(),
        overlay,
        variables.clone(),
    ))));
    let (builder, config) = install(
        runtime_config,
        ImpresspressBuilder::new().database(db.clone(), Backend::Sqlite),
    );
    let builder = builder
        // As `build_runtime` does: the request path's `Serving` boot requires it.
        .init_wait(wafer_run::InitWait::Refuse)
        .storage(Arc::new(InMemoryStorageService::new()))
        .crypto(Arc::new(
            wafer_block_crypto::service::Argon2JwtCryptoService::new(JWT_SECRET.to_string())
                .expect("a JWT crypto service"),
        ))
        .network(Arc::new(NoNetwork))
        .logger(Arc::new(wafer_core::service_blocks::logger::TracingLogger))
        .block_settings(settings)
        .config_source(source.clone())
        .variables_snapshot(variables.clone());
    let block_settings = builder.block_settings_handle();
    let (wafer, exporter) = match prepared {
        Some(plan) => {
            let (sha, lock, assets) = identity();
            builder
                .apply_prepared_plan(plan, APP_ID, &sha, &lock, &assets)
                .expect("the plan applies")
                .build_with_plan_exporter()
        }
        None => builder.build_with_plan_exporter(),
    }
    .expect("the runtime builds");
    Ok(Built {
        wafer,
        exporter,
        block_settings,
        config,
        source,
        variables,
    })
}

/// The variables snapshot every Worker build starts from
/// (`runtime_build::fresh_variables_snapshot`): a failed read is retried
/// only by the runtime cache's retry window, never on its own.
fn workers_snapshot() -> Arc<VariablesSnapshot> {
    Arc::new(VariablesSnapshot::new(RetryMode::Rebuild))
}

/// `build_runtime`'s order: the database (D1 speaks SQLite) goes into the
/// builder before `install`, which publishes its dialect on both surfaces.
fn install(
    runtime_config: RuntimeConfig,
    builder: ImpresspressBuilder,
) -> (ImpresspressBuilder, Arc<dyn ConfigService>) {
    runtime_config
        .install(builder, |map| {
            let service = impresspress_core::builder::fill_config_service(
                Arc::new(wafer_core::service_blocks::config::EnvConfigService::new()),
                map,
            );
            (service.clone(), service)
        })
        .expect("install the config surfaces")
}

/// Phase 1: the deploy funnel over an empty database, then the prepare
/// closing sequence; returns the plan.
async fn provision(db: &Arc<dyn DatabaseService>) -> PreparedRuntimePlan {
    let mut built = build(db, None, true, &HashMap::new(), workers_snapshot(), false).await;
    let report = boot_deploy_runtime(
        &mut built.wafer,
        db,
        &built.block_settings,
        built.config.clone(),
    )
    .await
    .expect("the deploy funnel runs");
    assert!(report.ok, "provisioning must succeed: {report:#?}");
    seal_prepared_plan(db, &built.exporter)
        .await
        .expect("the plan seals");
    let (sha, lock, assets) = identity();
    built
        .exporter
        .prepare_runtime_plan_with_config_generation(APP_ID, sha, "0".repeat(32), lock, assets)
        .expect("the plan exports")
}

/// Phases 3 and 4: a request-path build, booted; with the config source a
/// warm request installs into, and the runtime's variables snapshot.
async fn boot_request(
    db: &Arc<dyn DatabaseService>,
    build_kind: RequestBuild,
    plan: &PreparedRuntimePlan,
    request_config: &HashMap<String, String>,
) -> (Wafer, Arc<RequestScopedSource>, Arc<VariablesSnapshot>) {
    let prepared = (build_kind == RequestBuild::Prepared).then_some(plan);
    let mut built = build(
        db,
        prepared,
        false,
        request_config,
        workers_snapshot(),
        true,
    )
    .await;
    boot_request_runtime(
        &mut built.wafer,
        db,
        &built.block_settings,
        built.config.clone(),
        build_kind,
    )
    .await
    .unwrap_or_else(|e| panic!("the {build_kind:?} request build boots: {e}"));
    (built.wafer, built.source, built.variables)
}

// ---------------------------------------------------------------------------
// Seeds (fixture setup, through a plain SQLite service: uncounted).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    /// gdsf's shape: shared rows plus block-scoped ones.
    Mixed,
    /// No block-scoped `variables` row at all (R4).
    Shared,
}

impl Variant {
    fn name(self) -> &'static str {
        match self {
            Self::Mixed => "mixed",
            Self::Shared => "shared",
        }
    }
}

fn row(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

async fn count(db: &Arc<dyn DatabaseService>, table: &str) -> usize {
    usize::try_from(db.count(table, &[]).await.expect("count")).expect("a count")
}

/// Seed `variant`'s sizes over a provisioned database; returns the admin's
/// access token, and the request config the request-path builds forward.
async fn seed(path: &Path, variant: Variant) -> (String, HashMap<String, String>) {
    let db: Arc<dyn DatabaseService> = Arc::new(
        SQLiteDatabaseService::open(path.to_str().expect("utf-8")).expect("open the fixture db"),
    );
    // Now, not a fixed date: the admin dashboard counts by recent windows, and
    // a fixed date would drift out of them.
    let now = impresspress_core::util::now_rfc3339();
    let now = now.as_str();

    // Legal documents: the two 003 seeded, then drafts up to the size.
    let have = count(&db, LEGAL_DOCUMENTS_TABLE).await;
    let docs: Vec<HashMap<String, Value>> = (have..LEGAL_DOCUMENTS)
        .map(|i| {
            let doc_type = if i % 2 == 0 { "terms" } else { "privacy" };
            row(&[
                ("id", json!(format!("doc-{i}"))),
                ("doc_type", json!(doc_type)),
                ("title", json!(format!("Version {i}"))),
                ("content", json!("Archived text.")),
                ("status", json!("archived")),
                ("version", json!(i + 10)),
                ("created_by", json!("harness")),
                ("created_at", json!(now)),
                ("updated_at", json!(now)),
            ])
        })
        .collect();
    db.create_many(LEGAL_DOCUMENTS_TABLE, docs)
        .await
        .expect("seed legal documents");

    // Users, the first an admin, each holding its role as a grant row.
    let mut users = Vec::new();
    let mut grants = Vec::new();
    let mut admin_id = String::new();
    for i in 0..USERS {
        // Ids differ per variant: `auth`'s version cache is process-wide, and
        // a cold isolate starts with it empty.
        let id = format!("{}-user-{i:04}", variant.name());
        let (email, role) = if i == 0 {
            admin_id = id.clone();
            (ADMIN_EMAIL.to_string(), "admin")
        } else {
            (format!("user{i}@example.com"), "user")
        };
        users.push(row(&[
            ("id", json!(id)),
            ("email", json!(email)),
            ("display_name", json!(format!("User {i}"))),
            ("name", json!(format!("User {i}"))),
            ("email_verified", json!(true)),
            ("created_at", json!(now)),
            ("updated_at", json!(now)),
        ]));
        if let wafer_block::wire::database::BatchWrite::Create { data, .. } =
            user_roles::create_op(&id, role)
        {
            grants.push(data);
        }
    }
    db.create_many(auth_repo::users::TABLE, users)
        .await
        .expect("seed users");
    db.create_many(user_roles::TABLE, grants)
        .await
        .expect("seed role grants");

    // The admin's login family: the session row a sign-in writes.
    db.create(
        auth_repo::sessions::TABLE,
        row(&[
            ("family", json!(ADMIN_FAMILY)),
            ("user_id", json!(admin_id)),
            ("auth_method", json!("password")),
            ("created_at", json!(now)),
            ("last_used_at", json!(now)),
            ("expires_at", json!("2099-01-01T00:00:00Z")),
        ]),
    )
    .await
    .expect("seed the admin session");

    // Variables: shared rows up to the size; block-scoped rows per variant.
    let existing = variables::read_unfiltered(&db)
        .await
        .expect("read variables")
        .rows;
    let shared = existing.iter().filter(|r| r.block.is_none()).count();
    for i in shared..SHARED_VARIABLES {
        variables::seed_if_absent(
            &db,
            &format!("WAFER_RUN_SHARED__HARNESS_{i}"),
            "value",
            "",
            "",
            false,
        )
        .await
        .expect("seed a shared variable");
    }
    let mut request_config = HashMap::new();
    match variant {
        Variant::Mixed => {
            for (key, value) in BLOCK_VARIABLES {
                variables::seed_if_absent(&db, key, value, "", "", false)
                    .await
                    .expect("seed a block-scoped variable");
            }
        }
        Variant::Shared => {
            // Every block-scoped row goes: products' auto-generated webhook
            // secret and the three auth defaults admin's settings seed stores
            // under their blocks. The site forwards all four as request
            // config instead (products requires its secret, or its Init
            // fails).
            for scoped in existing.iter().filter(|r| r.block.is_some()) {
                db.delete(variables::TABLE, &scoped.id)
                    .await
                    .expect("drop a block-scoped variable");
                request_config.insert(scoped.key.clone(), scoped.value.clone());
            }
        }
    }

    // Grants an admin added.
    for i in 0..GRANTS {
        let grant = wrap_grants::NewWrapGrant {
            grantee: "impresspress/files".to_string(),
            resource: format!("harness-{i}/*"),
            write: wafer_block::GrantWrite::None,
            resource_type: "storage".to_string(),
            description: String::new(),
        }
        .into_row();
        db.create(wrap_grants::TABLE, grant.to_data())
            .await
            .expect("seed a grant");
    }

    (access_token(&admin_id), request_config)
}

/// An access JWT for `sub` in [`ADMIN_FAMILY`], signed the way auth-ui signs
/// one: with the key derived from the bound secret for `impresspress/auth-ui`.
fn access_token(sub: &str) -> String {
    use wafer_block_crypto::primitives;

    let key = primitives::derive_block_key(
        JWT_SECRET.as_bytes(),
        impresspress_core::blocks::auth_ui::AUTH_UI_BLOCK_ID,
    );
    let claims = BTreeMap::from([
        ("sub".to_string(), json!(sub)),
        ("type".to_string(), json!("access")),
        ("iss".to_string(), json!("http://localhost:5173")),
        ("roles".to_string(), json!(["admin"])),
        ("family".to_string(), json!(ADMIN_FAMILY)),
    ]);
    primitives::jwt_sign(claims, Duration::from_secs(3600), key.as_bytes()).expect("sign")
}

// ---------------------------------------------------------------------------
// Warm requests, dispatched as the Worker dispatches them.
// ---------------------------------------------------------------------------

/// One warm request: `site-main` inside a fresh root after-response scope,
/// then its audit row, then its deferred tasks (the Worker hands both to
/// `wait_until`, on the same request's D1).
async fn request(
    wafer: &Wafer,
    db: &Arc<dyn DatabaseService>,
    path: &str,
    cookie: Option<&str>,
) -> u16 {
    request_flow(wafer, db, "site-main", path, cookie).await
}

/// [`request`] through `flow` rather than the impresspress `site-main`.
async fn request_flow(
    wafer: &Wafer,
    db: &Arc<dyn DatabaseService>,
    flow: &str,
    path: &str,
    cookie: Option<&str>,
) -> u16 {
    respond_flow(wafer, db, flow, path, cookie).await.status
}

/// [`request_flow`]'s whole response: status, headers and body.
async fn respond_flow(
    wafer: &Wafer,
    db: &Arc<dyn DatabaseService>,
    flow: &str,
    path: &str,
    cookie: Option<&str>,
) -> http_codec::HttpResponseParts {
    let mut headers = vec![
        ("host", "localhost".to_string()),
        ("accept", "text/html".to_string()),
        ("sec-fetch-mode", "navigate".to_string()),
        ("sec-fetch-site", "none".to_string()),
    ];
    if let Some(token) = cookie {
        headers.push(("cookie", format!("auth_token={token}")));
    }
    respond(wafer, db, flow, path, &headers).await
}

/// A `GET path` with `headers` through `flow`, as the Worker serves it: the
/// whole response the client receives, then the audit row and the deferred
/// work the request handed off.
async fn respond(
    wafer: &Wafer,
    db: &Arc<dyn DatabaseService>,
    flow: &str,
    path: &str,
    headers: &[(&str, String)],
) -> http_codec::HttpResponseParts {
    deferred::set_mode(DeferMode::Queued);
    let msg = http_codec::build_http_message(
        "GET",
        path,
        "",
        "127.0.0.1",
        headers.iter().map(|(k, v)| (*k, v.as_str())),
    );
    let after = AfterResponse::new();
    let response = after_response::scope_root(
        Rc::clone(&after),
        async {
            http_codec::collect_http_response(wafer.run(flow, msg, InputStream::empty()).await)
                .await
        },
        || panic!("a stale after-response scope"),
    )
    .await;
    if let Some(row) = after.take_audit_row() {
        after_response::persist_audit_row(db.as_ref(), row)
            .await
            .expect("the audit row is written");
    }
    for task in after.take_tasks() {
        task.await;
    }
    response
}

/// The warm requests, with the status each must answer: a request that took
/// another path (a refused session, a redirect) would pin another path's
/// statements.
const WARM: [(&str, &str, bool, u16); 5] = [
    ("GET /", "/", false, 302),
    ("GET /b/legalpages/terms", "/b/legalpages/terms", false, 200),
    ("GET /health", "/health", false, 200),
    ("GET /b/admin/ (signed in)", "/b/admin/", true, 200),
    ("GET 404", "/no-such-page", false, 404),
];

// ---------------------------------------------------------------------------
// Classifying what was sent.
// ---------------------------------------------------------------------------

/// A statement's class: what it costs D1 in kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    /// `SELECT COUNT(…)`: reads every row it counts.
    Count,
    /// A read whose plan scans a whole table (or a whole index of it).
    Scan,
    /// A read whose plan only searches an index.
    Search,
    /// Schema introspection: `pragma_table_info`, or a `sqlite_master` read
    /// that probes no single table.
    Introspect,
    /// A table-existence probe (`SELECT EXISTS(… sqlite_master …)`), filed
    /// under the table it looks for.
    Exists,
    /// The executor's id-kind probe before a table's first create
    /// (`CASE WHEN NOT EXISTS(… sqlite_master …) … pragma_table_info …`),
    /// filed under the table it probes.
    IdProbe,
    /// Any write, DDL included.
    Write,
    /// A read whose plan could not be taken (its table is gone by the end of
    /// the phase). Only provisioning, which drops tables, has any.
    Read,
}

impl Class {
    fn label(self) -> &'static str {
        match self {
            Self::Count => "COUNT",
            Self::Scan => "SCAN",
            Self::Search => "SEARCH",
            Self::Introspect => "INTROSPECT",
            Self::Exists => "EXISTS",
            Self::IdProbe => "ID-PROBE",
            Self::Write => "WRITE",
            Self::Read => "READ",
        }
    }
}

/// One statement, classified, with the rows the model says D1 reads for it.
#[derive(Debug, Clone)]
struct Classified {
    table: String,
    class: Class,
    rows: u64,
    sql: String,
}

/// A query plan: its `(SCAN|SEARCH, table)` rows, and every line as SQLite
/// gives it.
type Plan = (Vec<(String, String)>, Vec<String>);

/// Classifies statements against the database file through a second,
/// read-only connection, as it stands at the end of the phase.
struct Classifier {
    conn: rusqlite::Connection,
}

fn to_sql(value: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match value {
        Value::Null => V::Null,
        Value::Bool(b) => V::Integer(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .map(V::Integer)
            .unwrap_or_else(|| V::Real(n.as_f64().unwrap_or_default())),
        Value::String(s) => V::Text(s.clone()),
        other => V::Text(other.to_string()),
    }
}

fn unquote(name: &str) -> String {
    name.trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']')
        .trim_end_matches([';', ','])
        .to_string()
}

/// The identifier after the first of `keywords` in `sql`.
fn table_after(sql: &str, keywords: &[&str]) -> Option<String> {
    let tokens: Vec<&str> = sql.split_whitespace().collect();
    for (i, token) in tokens.iter().enumerate() {
        if keywords.iter().any(|k| token.eq_ignore_ascii_case(k)) {
            let mut next = i + 1;
            while tokens.get(next).is_some_and(|t| {
                ["IF", "NOT", "EXISTS", "ONLY"]
                    .iter()
                    .any(|k| t.eq_ignore_ascii_case(k))
            }) {
                next += 1;
            }
            let name = tokens.get(next)?;
            return Some(unquote(name.split('(').next().unwrap_or(name)));
        }
    }
    None
}

impl Classifier {
    fn open(path: &Path) -> Self {
        Self {
            conn: rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .expect("a read-only connection"),
        }
    }

    fn scalar(&self, sql: &str, params: &[Value]) -> Option<u64> {
        let mut stmt = self.conn.prepare(sql).ok()?;
        let values: Vec<rusqlite::types::Value> = params.iter().map(to_sql).collect();
        stmt.query_row(rusqlite::params_from_iter(values), |r| r.get::<_, i64>(0))
            .ok()
            .and_then(|n| u64::try_from(n).ok())
    }

    /// The sum of the first column of `sent`'s result: what a `COUNT`
    /// answers, over every group.
    fn counted(&self, sent: &Sent) -> u64 {
        let Ok(mut stmt) = self.conn.prepare(&sent.sql) else {
            return 0;
        };
        let values: Vec<rusqlite::types::Value> = sent.params.iter().map(to_sql).collect();
        let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(values), |r| r.get::<_, i64>(0))
        else {
            return 0;
        };
        rows.filter_map(Result::ok)
            .filter_map(|n| u64::try_from(n).ok())
            .sum()
    }

    fn table_rows(&self, table: &str) -> u64 {
        self.scalar(&format!("SELECT COUNT(*) FROM \"{table}\""), &[])
            .unwrap_or(0)
    }

    /// The `SCAN`/`SEARCH` rows of `sql`'s query plan, as `(verb, table)`,
    /// and every line of the plan as SQLite gives it.
    fn plan(&self, sent: &Sent) -> Option<Plan> {
        let mut stmt = self
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN {}", sent.sql))
            .ok()?;
        let values: Vec<rusqlite::types::Value> = sent.params.iter().map(to_sql).collect();
        let details: Vec<String> = stmt
            .query_map(rusqlite::params_from_iter(values), |r| {
                r.get::<_, String>(3)
            })
            .ok()?
            .collect::<Result<_, _>>()
            .ok()?;
        Some((
            details
                .iter()
                .filter_map(|d| {
                    let mut words = d.split_whitespace();
                    let verb = words.next()?;
                    let table = words.next()?;
                    (matches!(verb, "SCAN" | "SEARCH") && table != "CONSTANT")
                        .then(|| (verb.to_string(), unquote(table)))
                })
                .collect(),
            details,
        ))
    }

    fn classify(&self, sent: &Sent) -> Classified {
        let sql = sent.sql.trim();
        let upper = sql.to_ascii_uppercase();
        let verbatim = |table: String, class: Class, rows: u64| Classified {
            table,
            class,
            rows,
            sql: format!("{sql}  {:?}", sent.params),
        };
        if upper.contains("SQLITE_MASTER") {
            let all = self
                .scalar("SELECT COUNT(*) FROM sqlite_master", &[])
                .unwrap_or(0);
            let Some(probed) = sent.params.first().and_then(Value::as_str) else {
                return verbatim("sqlite_master".to_string(), Class::Introspect, all);
            };
            // A probe for one table scans the schema in rowid order until it
            // finds it, or to the end when it is absent.
            let to_match = self
                .scalar(
                    "SELECT COUNT(*) FROM sqlite_master WHERE rowid <= \
                     (SELECT rowid FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                    &sent.params[..1],
                )
                .filter(|n| *n > 0)
                .unwrap_or(all);
            if upper.contains("PRAGMA_TABLE_INFO") {
                // The id-kind probe also reads the table's column list.
                let columns = self
                    .scalar(
                        "SELECT COUNT(*) FROM pragma_table_info(?1)",
                        &sent.params[..1],
                    )
                    .unwrap_or(0);
                return verbatim(probed.to_string(), Class::IdProbe, to_match + columns);
            }
            return verbatim(probed.to_string(), Class::Exists, to_match);
        }
        if upper.contains("PRAGMA_TABLE_INFO") {
            let table = sent
                .params
                .first()
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| "?".to_string());
            let rows = self
                .scalar(&format!("SELECT COUNT(*) FROM ({sql})"), &sent.params)
                .unwrap_or(0);
            return verbatim(table, Class::Introspect, rows);
        }
        let is_read = upper.starts_with("SELECT") || upper.starts_with("WITH");
        if !is_read {
            let table = table_after(sql, &["INTO", "UPDATE", "FROM", "TABLE", "ON"])
                .unwrap_or_else(|| "?".to_string());
            return verbatim(table, Class::Write, 0);
        }
        let Some((plan, details)) = self.plan(sent) else {
            let table = table_after(sql, &["FROM"]).unwrap_or_else(|| "?".to_string());
            return verbatim(table, Class::Read, 0);
        };
        let scanned = plan.iter().find(|(verb, _)| verb == "SCAN");
        let (verb, table) = scanned
            .or_else(|| plan.first())
            .cloned()
            .unwrap_or_else(|| ("SEARCH".to_string(), "?".to_string()));
        if upper.starts_with("SELECT COUNT(") {
            // A whole-table count reads every row; one an index narrows reads
            // the rows it counts, which is what it answers.
            let rows = if verb == "SCAN" {
                self.table_rows(&table)
            } else {
                self.counted(sent)
            };
            return verbatim(table, Class::Count, rows);
        }
        if verb == "SCAN" {
            let all = self.table_rows(&table);
            let rows = if limit_stops_scan(&upper, &details) {
                // It steps over the rows it skips, then the rows it returns.
                let returned = self
                    .scalar(&format!("SELECT COUNT(*) FROM ({sql})"), &sent.params)
                    .unwrap_or(0);
                all.min(returned + offset(sql, &sent.params))
            } else {
                all
            };
            verbatim(table, Class::Scan, rows)
        } else {
            let rows = self
                .scalar(&format!("SELECT COUNT(*) FROM ({sql})"), &sent.params)
                .unwrap_or(0);
            verbatim(table, Class::Search, rows)
        }
    }
}

/// Whether a `LIMIT` stops a scan early: only when the plan is that one scan
/// and nothing else (no temporary B-tree to sort through, no second table)
/// and the statement has nothing to do before it can return a row — no
/// `WHERE`, `JOIN`, `GROUP BY`, `DISTINCT` or aggregate. Otherwise the scan
/// reads the whole table whatever the `LIMIT` says.
fn limit_stops_scan(upper: &str, plan: &[String]) -> bool {
    const AGGREGATES: [&str; 7] = [
        "COUNT(",
        "SUM(",
        "TOTAL(",
        "AVG(",
        "MIN(",
        "MAX(",
        "GROUP_CONCAT(",
    ];
    upper.contains(" LIMIT ")
        && plan.len() == 1
        && plan[0].starts_with("SCAN ")
        && ![" WHERE ", " JOIN ", " GROUP BY ", "DISTINCT"]
            .iter()
            .any(|clause| upper.contains(clause))
        && !AGGREGATES.iter().any(|aggregate| upper.contains(aggregate))
}

/// The rows `sql`'s `OFFSET` skips: a literal, or the parameter it binds
/// (`?N`, or the plain `?` at its position).
fn offset(sql: &str, params: &[Value]) -> u64 {
    let upper = sql.to_ascii_uppercase();
    let Some(at) = upper.find(" OFFSET ") else {
        return 0;
    };
    let token = sql[at + " OFFSET ".len()..]
        .split_whitespace()
        .next()
        .unwrap_or_default();
    let value = if let Some(n) = token.strip_prefix('?') {
        let index = if n.is_empty() {
            sql[..at].matches('?').count()
        } else {
            n.parse::<usize>()
                .map_or(usize::MAX, |n| n.saturating_sub(1))
        };
        params.get(index).and_then(Value::as_u64)
    } else {
        token.parse::<u64>().ok()
    };
    value.unwrap_or(0)
}

/// One phase's recorded statements, classified.
struct Phase {
    name: String,
    statements: Vec<Classified>,
    /// D1 calls: round trips, a batch or transaction counting once.
    calls: usize,
    /// Extra facts for the section header (a warm request's status).
    note: String,
}

impl Phase {
    fn new(name: String, trips: &[RoundTrip], classifier: &Classifier, note: String) -> Self {
        let statements = trips
            .iter()
            .flat_map(|trip| trip.sent.iter())
            .map(|sent| classifier.classify(sent))
            .collect();
        Self {
            name,
            statements,
            calls: trips.len(),
            note,
        }
    }

    fn rows(&self) -> u64 {
        self.statements.iter().map(|s| s.rows).sum()
    }

    fn render(&self) -> String {
        let mut grouped: BTreeMap<(String, Class), (usize, u64)> = BTreeMap::new();
        for s in &self.statements {
            let entry = grouped.entry((s.table.clone(), s.class)).or_default();
            entry.0 += 1;
            entry.1 += s.rows;
        }
        let mut out = format!(
            "## {} — {} statements, {} D1 calls, rows≈{}{}\n",
            self.name,
            self.statements.len(),
            self.calls,
            self.rows(),
            self.note
        );
        for ((table, class), (n, rows)) in grouped {
            out.push_str(&format!(
                "{table:<48} {:<10} {n:>4} {rows:>7}\n",
                class.label()
            ));
        }
        out
    }

    fn verbatim(&self) -> String {
        self.statements
            .iter()
            .map(|s| format!("  [{} {}] {}", s.class.label(), s.table, s.sql))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ---------------------------------------------------------------------------
// The rules, asserted whatever the golden says.
// ---------------------------------------------------------------------------

/// Statements the rules would refuse on a cold build, allowed with the reason
/// they are still there, the PR that removes or changes them, and exactly how
/// many each variant sends.
///
/// Strict both ways: a count that differs fails, and an entry that no longer
/// occurs at all fails too, so a change that removes a statement or sends it
/// fewer times edits its entry here — the rule, not only the golden, records
/// it. Each `because` names the plan step that is expected to. A write is
/// allowed the same way, with the reason it is safe on the request path.
struct Allowed {
    phase: &'static str,
    table: &'static str,
    class: Class,
    /// Statements the `mixed` variant sends.
    mixed: usize,
    /// Statements the `shared` variant sends.
    shared: usize,
    because: &'static str,
}

impl Allowed {
    fn count(&self, variant: Variant) -> usize {
        match variant {
            Variant::Mixed => self.mixed,
            Variant::Shared => self.shared,
        }
    }
}

const PHASE_TOKEN: &str = "1b prepared build, bootstrap token forwarded, no account yet";
const PHASE_DATABASE: &str = "3 database build";
const PHASE_PREPARED: &str = "4 prepared build";
const PHASE_STALE_SEED: &str = "3b database build, seed hash stale";
const PHASE_PASSWORD: &str = "4b prepared build, bootstrap email and password forwarded";
const PHASE_EMAIL: &str = "4c prepared build, only the bootstrap email forwarded";
const PHASE_GATE_FIRST: &str = "6 window retry, gate fails, first after a publish";
const PHASE_GATE_AGAIN: &str = "6b window retry, gate fails again";
/// Every phase that stands for a cold isolate's build.
const COLD_BUILDS: [&str; 6] = [
    PHASE_TOKEN,
    PHASE_DATABASE,
    PHASE_STALE_SEED,
    PHASE_PREPARED,
    PHASE_PASSWORD,
    PHASE_EMAIL,
];

/// The bootstrap credentials a consumer forwards after the first deploy.
const BOOTSTRAP_TOKEN: &str = "d1-read-cost-harness-bootstrap-token";
const BOOTSTRAP_PASSWORD: &str = "d1-read-cost-harness-bootstrap-password";

const ALLOWED: &[Allowed] = &[
    Allowed {
        phase: PHASE_DATABASE,
        table: "impresspress__admin__variables",
        class: Class::Scan,
        mixed: 1,
        shared: 1,
        because: "the runtime's variables snapshot: one unfiltered read serves the config \
                  source for every config-declaring block and the config block, with or \
                  without a block-scoped row (D1-READ-COST PR 6)",
    },
    Allowed {
        phase: PHASE_PREPARED,
        table: "impresspress__admin__variables",
        class: Class::Scan,
        mixed: 1,
        shared: 1,
        because: "the runtime's variables snapshot, as on the database build",
    },
    Allowed {
        phase: PHASE_STALE_SEED,
        table: "impresspress__admin__variables",
        class: Class::Scan,
        mixed: 1,
        shared: 1,
        because: "the runtime's variables snapshot, as on the database build; a stale \
                  seed hash adds nothing, because the shared-variable seed runs only on \
                  provisioning builds (D1-READ-COST PR 4b)",
    },
    Allowed {
        phase: PHASE_STALE_SEED,
        table: "impresspress__admin__block_settings",
        class: Class::Scan,
        mixed: 2,
        shared: 2,
        because: "as on the database build: before `build()` and after admin's Init, \
                  KV-cached on Cloudflare",
    },
    Allowed {
        phase: PHASE_STALE_SEED,
        table: "impresspress__admin__wrap_grants",
        class: Class::Scan,
        mixed: 1,
        shared: 1,
        because: "as on the database build: the admin-created grants",
    },
    Allowed {
        phase: PHASE_EMAIL,
        table: "impresspress__admin__variables",
        class: Class::Scan,
        mixed: 1,
        shared: 1,
        because: "the runtime's variables snapshot, as on every prepared build; an email \
                  alone cannot create an admin, so it costs no users read \
                  (D1-READ-COST PR 4c)",
    },
    Allowed {
        phase: PHASE_TOKEN,
        table: "impresspress__admin__variables",
        class: Class::Scan,
        mixed: 1,
        shared: 1,
        because: "the runtime's variables snapshot, as on every prepared build",
    },
    Allowed {
        phase: PHASE_PASSWORD,
        table: "impresspress__admin__variables",
        class: Class::Scan,
        mixed: 1,
        shared: 1,
        because: "the runtime's variables snapshot, as on every prepared build",
    },
    Allowed {
        phase: PHASE_TOKEN,
        table: "wafer_run__auth__users",
        class: Class::Scan,
        mixed: 1,
        shared: 1,
        because: "a bootstrap key is configured, so auth's Init asks whether an account \
                  exists: `users::any_exists`, one row at most (D1-READ-COST PR 4c), for \
                  as long as the key stays bound",
    },
    Allowed {
        phase: PHASE_PASSWORD,
        table: "wafer_run__auth__users",
        class: Class::Scan,
        mixed: 1,
        shared: 1,
        because: "a bootstrap key is still bound after the first account: \
                  `users::any_exists` reads one row, then the bootstrap is a no-op \
                  (D1-READ-COST PR 4c)",
    },
    Allowed {
        phase: PHASE_TOKEN,
        table: "wafer_run__auth__bootstrap_tokens",
        class: Class::Write,
        mixed: 1,
        shared: 1,
        because: "the token path with no account yet: `bootstrap_tokens::insert_if_absent`, \
                  one INSERT … ON CONFLICT DO NOTHING per cold build until the token is \
                  redeemed. The table does not bump the config version \
                  (`cache_key::bumps_config_version`), so no fleet rebuild \
                  (D1-READ-COST PR 4c/4e)",
    },
    Allowed {
        phase: PHASE_DATABASE,
        table: "impresspress__admin__block_settings",
        class: Class::Scan,
        mixed: 2,
        shared: 2,
        because: "a database build reads its enablement map before `build()` and again \
                  after admin's Init (`RequestBootHooks`); on Cloudflare both are served \
                  by the KV row cache and reach D1 only on a KV miss",
    },
    Allowed {
        phase: PHASE_DATABASE,
        table: "impresspress__admin__wrap_grants",
        class: Class::Scan,
        mixed: 1,
        shared: 1,
        because: "a database build loads the admin-created grants (`GrantSource::Database`); \
                  a prepared build takes them from the plan",
    },
];

/// The tables a warm request must not read or write: the per-build state.
const BUILD_STATE: [&str; 3] = [
    "impresspress__admin__variables",
    "impresspress__admin__block_settings",
    "impresspress__admin__wrap_grants",
];

/// Whether a cold build may send `s` only under an allowance: any write, any
/// `COUNT`, any `sqlite_master` probe, and any scan of a table.
fn needs_allowance(s: &Classified) -> bool {
    matches!(
        s.class,
        Class::Write | Class::Count | Class::Exists | Class::IdProbe
    ) || s.table == "sqlite_master"
        || s.class == Class::Scan
}

/// Every rule a phase breaks, as messages.
fn check_rules(variant: Variant, phases: &[Phase]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut sent = vec![0usize; ALLOWED.len()];
    for phase in phases {
        let short = phase.name.split(": ").nth(1).unwrap_or(&phase.name);
        let cold_build = COLD_BUILDS.contains(&short);
        let warm = short.starts_with("5 warm");
        for s in &phase.statements {
            if cold_build && needs_allowance(s) {
                match ALLOWED
                    .iter()
                    .position(|a| a.phase == short && a.table == s.table && a.class == s.class)
                {
                    Some(i) => sent[i] += 1,
                    None => problems.push(format!(
                        "{}: a cold build sends no write, no COUNT, no sqlite_master probe \
                         and no table SCAN unless allowed with a reason: [{} {}] {}",
                        phase.name,
                        s.class.label(),
                        s.table,
                        s.sql
                    )),
                }
            }
            if warm && BUILD_STATE.contains(&s.table.as_str()) {
                problems.push(format!(
                    "{}: a warm request touches no per-build state ({}): {}",
                    phase.name, s.table, s.sql
                ));
            }
        }
    }
    for (allowed, sent) in ALLOWED.iter().zip(sent) {
        let expected = allowed.count(variant);
        if sent != expected {
            let fix = if sent == 0 {
                "remove the allowance"
            } else {
                "change its count, with the reason"
            };
            problems.push(format!(
                "{}: `{}` sends {sent} {} on {}, the allowance says {expected} — {fix} ({})",
                variant.name(),
                allowed.phase,
                allowed.class.label(),
                allowed.table,
                allowed.because
            ));
        }
    }
    problems
}

// ---------------------------------------------------------------------------
// The run.
// ---------------------------------------------------------------------------

/// Everything one variant produced.
struct Run {
    provision: Phase,
    phases: Vec<Phase>,
    blocks: Vec<String>,
    config_blocks: Vec<String>,
    sizes: String,
}

fn cold(path: &Path, ledger: &Arc<Ledger>) -> Arc<dyn DatabaseService> {
    Arc::new(D1Shaped::open(path, Arc::clone(ledger)))
}

async fn run(variant: Variant) -> Run {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("d1.sqlite3");
    let ledger = Arc::new(Ledger::default());
    let label = |phase: &str| format!("{}: {phase}", variant.name());

    // 1. Provision.
    let db = cold(&path, &ledger);
    let plan = provision(&db).await;
    drop(db);
    let provision_trips = ledger.take();

    let classifier = Classifier::open(&path);
    let provision = Phase::new(
        "provision".to_string(),
        &provision_trips,
        &classifier,
        String::new(),
    );

    // 1b. A bootstrap token forwarded after the first deploy, before any
    //     account exists: the next cold build installs it.
    let mut phases = Vec::new();
    let db = cold(&path, &ledger);
    let forwarded = HashMap::from([(
        BOOTSTRAP_ADMIN_TOKEN_KEY.to_string(),
        BOOTSTRAP_TOKEN.to_string(),
    )]);
    let (wafer, _, _) = boot_request(&db, RequestBuild::Prepared, &plan, &forwarded).await;
    drop(wafer);
    drop(db);
    phases.push(Phase::new(
        label(PHASE_TOKEN),
        &ledger.take(),
        &classifier,
        String::new(),
    ));
    assert_eq!(
        classifier.table_rows(auth_repo::bootstrap_tokens::TABLE),
        1,
        "a bootstrap token forwarded after provisioning is installed by the next cold build"
    );

    // 2. Seed.
    let (token, request_config) = seed(&path, variant).await;
    let sizes = format!(
        "variables={} (block-scoped {}), legal documents={}, users={}, roles={}, grants={}",
        classifier.table_rows(variables::TABLE),
        classifier
            .scalar(
                &format!(
                    "SELECT COUNT(*) FROM \"{}\" WHERE block IS NOT NULL AND block <> ''",
                    variables::TABLE
                ),
                &[],
            )
            .unwrap_or(0),
        classifier.table_rows(LEGAL_DOCUMENTS_TABLE),
        classifier.table_rows(auth_repo::users::TABLE),
        classifier.table_rows("impresspress__admin__roles"),
        classifier.table_rows(wrap_grants::TABLE),
    );
    let mut forwarded: Vec<&str> = request_config.keys().map(String::as_str).collect();
    forwarded.sort_unstable();
    let sizes = if forwarded.is_empty() {
        sizes
    } else {
        format!(
            "{sizes}; block-scoped rows deleted and forwarded as request config: {}",
            forwarded.join(", ")
        )
    };

    // 3. A database build, on a cold isolate.
    let db = cold(&path, &ledger);
    let (wafer, _, _) = boot_request(&db, RequestBuild::Database, &plan, &request_config).await;
    drop(wafer);
    drop(db);
    phases.push(Phase::new(
        label(PHASE_DATABASE),
        &ledger.take(),
        &classifier,
        String::new(),
    ));

    // 3b. A database build whose admin seed hash is stale, as a release that
    //     changes a declared shared variable leaves it until its deploy runs:
    //     the request path still seeds nothing. The hash is restored after.
    let stale_seed_hash = |hash: Option<String>| {
        let path = path.clone();
        async move {
            let plain: Arc<dyn DatabaseService> = Arc::new(
                SQLiteDatabaseService::open(path.to_str().expect("utf-8")).expect("open the db"),
            );
            let admin = "impresspress/admin";
            let before = block_settings::load(&plain)
                .await
                .expect("read block_settings")
                .state(admin)
                .seed_defaults_hash;
            block_settings::upsert_fields_via_service(
                &plain,
                admin,
                block_settings::BlockSettingsPatch {
                    seed_defaults_hash: Some(hash.unwrap_or_else(|| "stale".to_string())),
                    ..Default::default()
                },
            )
            .await
            .expect("set the seed hash");
            before
        }
    };
    let stamped = stale_seed_hash(None).await;
    let db = cold(&path, &ledger);
    let (wafer, _, _) = boot_request(&db, RequestBuild::Database, &plan, &request_config).await;
    drop(wafer);
    drop(db);
    phases.push(Phase::new(
        label(PHASE_STALE_SEED),
        &ledger.take(),
        &classifier,
        String::new(),
    ));
    stale_seed_hash(Some(stamped)).await;

    // 4. A prepared build, on a cold isolate.
    let db = cold(&path, &ledger);
    let (wafer, source, variables) =
        boot_request(&db, RequestBuild::Prepared, &plan, &request_config).await;
    phases.push(Phase::new(
        label(PHASE_PREPARED),
        &ledger.take(),
        &classifier,
        String::new(),
    ));
    // 4b. A bootstrap credential still bound once accounts exist, on another
    //     cold isolate.
    {
        let db = cold(&path, &ledger);
        let mut bound = request_config.clone();
        bound.insert(
            BOOTSTRAP_ADMIN_EMAIL_KEY.to_string(),
            ADMIN_EMAIL.to_string(),
        );
        bound.insert(
            BOOTSTRAP_ADMIN_PASSWORD_KEY.to_string(),
            BOOTSTRAP_PASSWORD.to_string(),
        );
        let users = classifier.table_rows(auth_repo::users::TABLE);
        let (wafer, _, _) = boot_request(&db, RequestBuild::Prepared, &plan, &bound).await;
        drop(wafer);
        phases.push(Phase::new(
            label(PHASE_PASSWORD),
            &ledger.take(),
            &classifier,
            String::new(),
        ));
        assert_eq!(
            classifier.table_rows(auth_repo::users::TABLE),
            users,
            "a bootstrap credential creates no account once one exists"
        );
    }

    // 4c. Only the bootstrap email still bound once accounts exist: the
    //     normal leftover, which cannot create an admin.
    {
        let db = cold(&path, &ledger);
        let mut bound = request_config.clone();
        bound.insert(
            BOOTSTRAP_ADMIN_EMAIL_KEY.to_string(),
            ADMIN_EMAIL.to_string(),
        );
        let (wafer, _, _) = boot_request(&db, RequestBuild::Prepared, &plan, &bound).await;
        drop(wafer);
        phases.push(Phase::new(
            label(PHASE_EMAIL),
            &ledger.take(),
            &classifier,
            String::new(),
        ));
    }

    let blocks = wafer.block_names();
    let mut config_blocks: Vec<String> = wafer
        .block_infos()
        .into_iter()
        .filter(|info| !info.config_keys.is_empty())
        .map(|info| info.name.clone())
        .collect();
    config_blocks.sort();

    // 5. Warm requests on runtime 4.
    for (name, path, signed_in, expected) in WARM {
        source.install(warm_source(&db, &wafer, &variables, &request_config));
        let status = request(&wafer, &db, path, signed_in.then_some(token.as_str())).await;
        let phase = Phase::new(
            label(&format!("5 warm {name}")),
            &ledger.take(),
            &classifier,
            format!(", status {status}"),
        );
        assert_eq!(
            status,
            expected,
            "{} answered {status}, not {expected}; it would pin another path's \
             statements:\n{}",
            phase.name,
            phase.verbatim()
        );
        phases.push(phase);
    }

    // 6. The retry window's gate on an isolate whose database is down: the
    //    one read a retry makes before it builds anything. The first after a
    //    publish meets a cold schema cache (`runtime_cache::store` forgets
    //    the isolate's). So does every later one: the outage fails the
    //    introspection itself, which is then not memoized, so each check
    //    sends the same — the table's primary-key introspection, refused,
    //    and the existence probe the failed read asks — and stores nothing
    //    that changes the next.
    let shaped = Arc::new(D1Shaped::open(&path, Arc::clone(&ledger)));
    let down: Arc<dyn DatabaseService> = shaped.clone();
    shaped.fail_table(variables::TABLE, Fault::Unavailable);
    let mut first_gate = None;
    for name in [PHASE_GATE_FIRST, PHASE_GATE_AGAIN] {
        let error = workers_snapshot()
            .check_backend(&down)
            .await
            .expect_err("the outage fails the gate");
        assert!(error.is_transient(), "{name}: {error}");
        let phase = Phase::new(label(name), &ledger.take(), &classifier, String::new());
        let strays: Vec<String> = phase
            .statements
            .iter()
            .filter(|s| s.table != variables::TABLE)
            .map(|s| format!("[{} {}] {}", s.class.label(), s.table, s.sql))
            .collect();
        assert!(
            strays.is_empty(),
            "{}: the gate reads the variables table and nothing else: {strays:?}",
            phase.name
        );
        let sent: Vec<(Class, String)> = phase
            .statements
            .iter()
            .map(|s| (s.class, s.sql.clone()))
            .collect();
        match &first_gate {
            None => first_gate = Some(sent),
            Some(first) => assert_eq!(
                &sent, first,
                "{}: a failed gate stores nothing that changes the next one's cost",
                phase.name
            ),
        }
        phases.push(phase);
    }
    shaped.clear_fault();

    Run {
        provision,
        phases,
        blocks,
        config_blocks,
        sizes,
    }
}

const HEADER: &str = "\
# D1 read cost: what a cold Cloudflare isolate sends D1 on boot and first requests.
#
# Written by crates/impresspress-core/tests/d1_read_cost.rs (`just d1-cost`, which
# runs it with the Cloudflare block set). Regenerate with `BLESS=1 just d1-cost`
# and review the diff: a PR that saves statements or rows shows it here.
#
# Section headers count statements (each statement of a batch; rows are per
# statement) and D1 calls (the unit D1 limits per invocation and the Worker's
# budget admits; a batch counts once).
#
# Columns: table, class, statements, rows≈.
#   COUNT       SELECT COUNT(...): reads every row it counts.
#   SCAN        a read whose plan scans a table (or a whole index of it).
#   SEARCH      a read whose plan only searches an index.
#   INTROSPECT  pragma_table_info: up to two per table per cold isolate (its
#               columns and its primary key); or a sqlite_master read that
#               probes no single table (provisioning's).
#   EXISTS      a table-existence probe (sqlite_master), under the table it
#               looks for.
#   ID-PROBE    the executor's id-kind probe before a table's first create
#               (sqlite_master and pragmas), under the table it probes.
#   WRITE       any write, DDL included.
#   READ        a read whose plan could not be taken (its table is gone by the end
#               of the phase); provisioning only.
# rows≈ is a model, not D1's own rows_read: a SCAN reads every row of its table,
# unless a LIMIT stops it — only when the plan is that one scan alone and the
# statement has no WHERE, JOIN, GROUP BY, DISTINCT or aggregate — and then the
# rows it skips (OFFSET) and returns, at most the table. A COUNT reads every row
# it counts (the whole table unless an index narrows it); a SEARCH the rows it
# matches; pragma_table_info the rows it returns; EXISTS the schema rows up to
# the table it looks for (all of them when it is absent); ID-PROBE those plus the
# table's columns. A WRITE reads none.
# Counted against the database as it stands at the end of each phase
# (provisioning: before the seed).
#
# What this cannot see:
#   - the KV config-version probe a warm isolate makes every 5-10 minutes;
#   - the KV row cache: the two block_settings reads of a database build are KV
#     reads on Cloudflare and reach D1 only on a KV miss, which is what is shown;
#   - D1's own rows_read, which the `metric=d1_cost` line reports live (PR 7);
#   - a D1 read `build_runtime` adds around the funnels, or a consumer's
#     `register_blocks` makes: the harness runs the production config passes,
#     config source and funnels, but copies `build_runtime`'s sequence around
#     them (it needs a `worker::Env`).
#
# The `shared` variant has no block-scoped variables row at all. A database the
# main funnel provisioned always has some: products' auto-generated webhook
# secret (re-seeded on every deploy) and the auth defaults admin's settings seed
# stores under their blocks. So the all-shared shape exists only before a
# repinned site's first prepare, or after a manual delete. The variant deletes
# them and forwards their values as request config (`add_request_config`), as a
# consumer binding its secrets does; products requires its secret. Its rows
# still carry the `block` column (NULL), so the runtime's variables snapshot
# memoizes them like any other: one read per build, as in `mixed` (PR 6, R4).
";

fn render(runs: &[Run]) -> String {
    let first = &runs[0];
    let mut out = HEADER.to_string();
    out.push_str(&format!(
        "#\n# Blocks ({}): {}\n# Config-declaring blocks ({}): {}\n",
        first.blocks.len(),
        first.blocks.join(", "),
        first.config_blocks.len(),
        first.config_blocks.join(", ")
    ));
    for run in runs {
        out.push_str(&format!(
            "# Seeded ({}): {}\n",
            run.phases[0].name.split(':').next().unwrap_or("?"),
            run.sizes
        ));
    }
    out.push('\n');
    out.push_str(&first.provision.render());
    for run in runs {
        for phase in &run.phases {
            out.push('\n');
            out.push_str(&phase.render());
        }
    }
    out
}

/// **The golden list.** Both seed variants, every phase, against
/// `tests/d1_read_cost.golden`; and the rules, whatever the golden says.
#[tokio::test]
async fn a_cold_isolate_sends_d1_the_golden_statements() {
    let _generation = GENERATION.lock().await;
    let runs = vec![run(Variant::Mixed).await, run(Variant::Shared).await];

    let mut problems = Vec::new();
    for (run, variant) in runs.iter().zip([Variant::Mixed, Variant::Shared]) {
        problems.extend(check_rules(variant, &run.phases));
    }
    // `/_deploy/prepare` is one Worker invocation: the whole provisioning —
    // migrations, seeds, config reads and the closing reads — must fit the
    // default per-invocation limit, in D1 calls. A growth ceiling: the
    // migrations' own line (`tests/migration_history.rs`, half the limit) is
    // the one that pins the unit; this one fails only if prepare grows past
    // the whole limit, and a statement count would pass it today too.
    let limit = impresspress_core::config_vars::D1_CALLS_PER_INVOCATION_DEFAULT as usize;
    let provisioning = runs[0].provision.calls;
    if provisioning > limit {
        problems.push(format!(
            "provisioning makes {provisioning} D1 calls, more than the {limit} \
             `/_deploy/prepare` may make in its one invocation"
        ));
    }

    if std::env::var_os("D1_COST_VERBOSE").is_some() {
        let all_phases =
            std::iter::once(&runs[0].provision).chain(runs.iter().flat_map(|r| r.phases.iter()));
        for phase in all_phases {
            println!("## {}\n{}\n", phase.name, phase.verbatim());
        }
    }

    let actual = render(&runs);
    let path = golden_path();
    if std::env::var_os("BLESS").is_some() {
        std::fs::write(&path, &actual).expect("write the golden");
    } else {
        let expected = std::fs::read_to_string(&path).unwrap_or_default();
        if expected != actual {
            let sections = |text: &str| -> BTreeMap<String, String> {
                text.split("\n## ")
                    .skip(1)
                    .map(|s| {
                        let title = s.lines().next().unwrap_or_default();
                        let key = title.split(" — ").next().unwrap_or(title).to_string();
                        (key, s.to_string())
                    })
                    .collect()
            };
            let (want, got) = (sections(&expected), sections(&actual));
            let all_phases = std::iter::once(&runs[0].provision)
                .chain(runs.iter().flat_map(|r| r.phases.iter()));
            let mut diff = String::new();
            for phase in all_phases {
                let key = &phase.name;
                if want.get(key) != got.get(key) {
                    diff.push_str(&format!(
                        "\n--- golden\n## {}\n+++ actual\n## {}\nsent:\n{}\n",
                        want.get(key).map_or("(none)", String::as_str),
                        got.get(key).map_or("(none)", String::as_str),
                        phase.verbatim()
                    ));
                }
            }
            if diff.is_empty() {
                diff = "the header differs (the block set or the seeded sizes)".to_string();
            }
            problems.push(format!(
                "{} differs from what a cold isolate sends now. If the change is \
                 intended, re-run with BLESS=1 and commit the diff.\n{diff}",
                path.display()
            ));
        }
    }

    assert!(problems.is_empty(), "{}", problems.join("\n\n"));
}

/// Every `impresspress-core` feature, and whether this build has it.
const COMPILED_FEATURES: &[(&str, bool)] = &[
    ("block-dev", cfg!(feature = "block-dev")),
    ("block-fastembed", cfg!(feature = "block-fastembed")),
    ("block-files", cfg!(feature = "block-files")),
    ("block-legalpages", cfg!(feature = "block-legalpages")),
    ("block-llm", cfg!(feature = "block-llm")),
    ("block-messages", cfg!(feature = "block-messages")),
    ("block-products", cfg!(feature = "block-products")),
    ("block-signal", cfg!(feature = "block-signal")),
    ("block-tickets", cfg!(feature = "block-tickets")),
    ("block-userportal", cfg!(feature = "block-userportal")),
    ("block-vector", cfg!(feature = "block-vector")),
    ("embed-assets", cfg!(feature = "embed-assets")),
    ("llm", cfg!(feature = "llm")),
    ("migration-fixtures", cfg!(feature = "migration-fixtures")),
    ("native-embedding", cfg!(feature = "native-embedding")),
    ("postgres", cfg!(feature = "postgres")),
    ("site-runtime", cfg!(feature = "site-runtime")),
    ("sqlite", cfg!(feature = "sqlite")),
    ("storage-local", cfg!(feature = "storage-local")),
    ("storage-s3", cfg!(feature = "storage-s3")),
    ("test-hooks", cfg!(feature = "test-hooks")),
    ("test-support", cfg!(feature = "test-support")),
    ("wasm", cfg!(feature = "wasm")),
];

// ---------------------------------------------------------------------------
// An outage: what still serves while stored config cannot be read.
// ---------------------------------------------------------------------------

/// The flow a wafer-site-shaped deployment dispatches to: impresspress's
/// `site-main` middleware chain step for step, under the site's own id, as
/// the wafer.run site defines `wafer-site-main` (its `flows::site`).
const SITE_FLOW: &str = "wafer-site-main";

/// The site's static content block, as the wafer.run site's
/// `wafer-site/content` is: every path read straight from a storage service,
/// `/` as `index.html`, no config read.
struct SiteContent(Arc<InMemoryStorageService>);

impl SiteContent {
    const NAME: &'static str = "wafer-site/content";
    const INDEX: &'static [u8] = b"<h1>wafer-site</h1>";
}

#[wafer_block::wafer_async_trait]
impl wafer_run::Block for SiteContent {
    fn info(&self) -> wafer_run::BlockInfo {
        wafer_run::BlockInfo::new(Self::NAME, "0.0.1", "http-handler@v1", "site content")
    }

    async fn handle(
        &self,
        _ctx: &dyn wafer_run::Context,
        msg: wafer_run::Message,
        _input: InputStream,
    ) -> wafer_run::OutputStream {
        let path = msg.path().trim_start_matches('/');
        let key = if path.is_empty() { "index.html" } else { path };
        match wafer_core::interfaces::storage::service::StorageService::get(
            self.0.as_ref(),
            "dist",
            key,
        )
        .await
        {
            Ok((data, _)) => wafer_run::OutputStream::respond_with_meta(
                data,
                vec![wafer_run::MetaEntry {
                    key: wafer_run::META_RESP_CONTENT_TYPE.to_string(),
                    value: "text/html; charset=utf-8".to_string(),
                }],
            ),
            Err(_) => wafer_run::OutputStream::error(wafer_run::WaferError::new(
                wafer_run::ErrorCode::NotFound,
                "Not found",
            )),
        }
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn wafer_run::Context,
        _event: wafer_run::LifecycleEvent,
    ) -> Result<(), wafer_run::WaferError> {
        Ok(())
    }
}

/// Make `wafer` a wafer-site-shaped deployment, after `build()` and before
/// the boot, as the wafer.run site's `register_post_build_for_site` does: the
/// content block, the router's route table replaced by the site's (`/b/**`
/// and `/health` to impresspress, everything else to the content block), and
/// the site's flow over impresspress's middleware chain.
async fn make_site_shaped(wafer: &mut Wafer) {
    let storage = Arc::new(InMemoryStorageService::new());
    wafer_core::interfaces::storage::service::StorageService::put(
        storage.as_ref(),
        "dist",
        "index.html",
        SiteContent::INDEX,
        "text/html",
    )
    .await
    .expect("stage the site's index.html");
    wafer
        .register_block(SiteContent::NAME, Arc::new(SiteContent(storage)))
        .expect("register the site's content block");
    wafer.add_block_config(
        "wafer-run/router",
        json!({ "routes": [
            { "path": "/b/**",   "block": "impresspress/router" },
            { "path": "/health", "block": "impresspress/router" },
            { "path": "/**",     "block": SiteContent::NAME },
        ] }),
    );
    let mut flow: Value =
        serde_json::from_str(impresspress_core::flows::site_main::JSON).expect("site-main is JSON");
    flow["id"] = json!(SITE_FLOW);
    flow.as_object_mut()
        .expect("a flow object")
        .remove("config_map");
    wafer
        .add_flow_json(&flow.to_string())
        .expect("register the site's flow");
}

/// A prepared build over `db` and `variables`, booted on the request path;
/// `site_shaped` makes it the wafer-site-shaped deployment first.
async fn boot_prepared(
    db: &Arc<dyn DatabaseService>,
    plan: &PreparedRuntimePlan,
    site_shaped: bool,
    variables: Arc<VariablesSnapshot>,
) -> (Wafer, Arc<VariablesSnapshot>) {
    let mut built = build(db, Some(plan), false, &HashMap::new(), variables, true).await;
    if site_shaped {
        make_site_shaped(&mut built.wafer).await;
    }
    boot_request_runtime(
        &mut built.wafer,
        db,
        &built.block_settings,
        built.config.clone(),
        RequestBuild::Prepared,
    )
    .await
    .unwrap_or_else(|e| panic!("the prepared request build boots: {e}"));
    (built.wafer, built.variables)
}

/// The clock an outage test's snapshot measures its retry on, moved by hand:
/// a remembered failure is replayed with no query until the test moves it
/// past the retry time, however slowly the test runs.
#[derive(Clone, Default)]
struct HandClock(Arc<std::sync::atomic::AtomicU64>);

impl HandClock {
    /// Long past any retry a failure stored at time 0 schedules (60 s at most).
    const RETRY_DUE: u64 = 3_600_000;

    fn set(&self, ms: u64) {
        self.0.store(ms, std::sync::atomic::Ordering::SeqCst);
    }

    /// A snapshot that retries on its own (`Backoff`), on this clock — not
    /// `build_runtime`'s `Rebuild`, deliberately: moving the clock past the
    /// retry is how a test makes a remembered failure read again, so any
    /// stored-config read a request makes reaches D1 and is seen.
    fn snapshot(&self) -> Arc<VariablesSnapshot> {
        let now = Arc::clone(&self.0);
        Arc::new(VariablesSnapshot::with_clock(
            RetryMode::Backoff,
            move || now.load(std::sync::atomic::Ordering::SeqCst),
        ))
    }
}

/// Put `variables` into its failed state as an outage leaves a published
/// runtime's: the variables table fails, a config write invalidates the read
/// the boot made, and the next read fails and is remembered.
async fn fail_stored_config(
    shaped: &D1Shaped,
    db: &Arc<dyn DatabaseService>,
    variables: &VariablesSnapshot,
) {
    shaped.fail_table(variables::TABLE, Fault::Unavailable);
    impresspress_core::config_generation::note_config_write();
    variables
        .read(db)
        .await
        .expect_err("the outage fails the variables read");
}

/// Whether any statement of `trips` names the variables table.
fn reads_variables(trips: &[RoundTrip]) -> bool {
    trips
        .iter()
        .flat_map(|trip| trip.sent.iter())
        .any(|sent| sent.sql.contains(variables::TABLE))
}

/// **An outage leaves a page that reads no stored config serving** (the
/// outage plan's I3 test; fail closed, binding). With the variables snapshot
/// failed — every stored-config read `Unavailable` — a wafer-site-shaped
/// deployment still answers `/` 200 from its content block, sending D1 no
/// variables read on that request: the middleware chain and the router read
/// no table config. Its request runs with the failure's retry due, so a
/// stored-config read on the path — through the snapshot or straight at the
/// table — would reach D1 and be seen, not be answered from the memo. An
/// impresspress site's `/` reads its landing switch
/// (`HAS_LANDING_PAGE`, a table key), which fails closed: an error, never the
/// redirect it answers healthy and never a page decided by the switch's
/// declared default; its request runs before the retry, so the remembered
/// failure answers with no query. The snapshots' clock is the test's
/// ([`HandClock`]), so neither half depends on how fast the test runs.
/// It answers 503 (the outage plan's I4b: `Unavailable` is a 503 at the
/// classifier), never the 500 a fault is.
#[tokio::test]
async fn an_outage_leaves_a_page_reading_no_stored_config_serving() {
    let _generation = GENERATION.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("d1.sqlite3");
    let ledger = Arc::new(Ledger::default());
    let plan = provision(&cold(&path, &ledger)).await;

    // The wafer-site-shaped deployment.
    let shaped = Arc::new(D1Shaped::open(&path, Arc::clone(&ledger)));
    let db: Arc<dyn DatabaseService> = shaped.clone();
    let clock = HandClock::default();
    let (wafer, variables) = boot_prepared(&db, &plan, true, clock.snapshot()).await;
    assert_eq!(request_flow(&wafer, &db, SITE_FLOW, "/", None).await, 200);
    fail_stored_config(&shaped, &db, &variables).await;
    clock.set(HandClock::RETRY_DUE);
    ledger.take();
    assert_eq!(
        request_flow(&wafer, &db, SITE_FLOW, "/", None).await,
        200,
        "the content path reads no stored config, so an outage leaves it serving"
    );
    let trips = ledger.take();
    assert!(
        !reads_variables(&trips),
        "the content path read stored config: {trips:#?}"
    );
    shaped.clear_fault();
    drop(wafer);

    // An impresspress site.
    let shaped = Arc::new(D1Shaped::open(&path, Arc::clone(&ledger)));
    let db: Arc<dyn DatabaseService> = shaped.clone();
    let clock = HandClock::default();
    let (wafer, variables) = boot_prepared(&db, &plan, false, clock.snapshot()).await;
    assert_eq!(
        request(&wafer, &db, "/", None).await,
        302,
        "healthy, `/` redirects an anonymous visitor to sign in"
    );
    fail_stored_config(&shaped, &db, &variables).await;
    ledger.take();
    assert_eq!(
        request(&wafer, &db, "/", None).await,
        503,
        "the landing switch fails closed: a 503 (I4b), not the redirect its default decides"
    );
    assert!(
        !reads_variables(&ledger.take()),
        "before its retry, the remembered failure answers with no query"
    );
}

/// **Admin's `Init` reads no stored config, and the first block that does
/// is one that declares config keys** (the outage plan's IP-B addendum,
/// IP-B0). Attribution by ordering, on the prepared build `boot` runs: seal,
/// then admin alone, then every other block in `block_names()` order (the
/// prepared hook is a no-op, so this is `boot`'s sequence), draining the
/// ledger after each.
///
/// Admin used to read the database dialect from the variables table first
/// (`WAFER_RUN_SHARED__DATABASE__BACKEND`), so a failed variables read failed
/// admin's `Init` transiently whatever failed, and the config source's
/// `SourceFault` was never reached. Now the dialect is a boot-map fact, and
/// the first variables read is a config-declaring block's source load, which
/// classifies it. The positive half — some block does send the read — is
/// what proves the ledger sees variables statements at all.
#[tokio::test]
async fn admin_init_reads_no_stored_config_and_a_config_declaring_block_reads_first() {
    let _generation = GENERATION.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("d1.sqlite3");
    let ledger = Arc::new(Ledger::default());
    let plan = provision(&cold(&path, &ledger)).await;

    let db = cold(&path, &ledger);
    // Ungated: this attributes the boot's own reads to the `Init` that sends
    // them, which a gate made before assembly would answer from its memo.
    let mut built = build(
        &db,
        Some(&plan),
        false,
        &HashMap::new(),
        workers_snapshot(),
        false,
    )
    .await;
    built.wafer.seal().await.expect("seal");
    ledger.take();

    let admin = impresspress_core::blocks::admin::ADMIN_BLOCK_ID;
    built
        .wafer
        .init_block(admin)
        .await
        .expect("admin initializes");
    let trips = ledger.take();
    assert!(
        !reads_variables(&trips),
        "admin's Init read stored config: {trips:#?}"
    );

    let declaring: Vec<String> = built
        .wafer
        .block_infos()
        .into_iter()
        .filter(|info| !info.config_keys.is_empty())
        .map(|info| info.name.clone())
        .collect();
    let mut first_reader = None;
    for name in built.wafer.block_names() {
        if name == admin {
            continue;
        }
        built
            .wafer
            .init_block(&name)
            .await
            .unwrap_or_else(|e| panic!("{name} initializes: {e}"));
        if reads_variables(&ledger.take()) {
            first_reader = Some(name);
            break;
        }
    }
    let first_reader = first_reader.expect("some block's Init reads the variables table");
    assert!(
        declaring.contains(&first_reader),
        "the first stored-config read came from `{first_reader}`, which declares no config \
         keys, so its Init read through the config block (a fault there is `Unavailable`, \
         transient) before any source load could classify it; declaring: {declaring:?}"
    );
}

/// A prepared request-path build booted while its database refuses the
/// variables table with `fault` from the first read: the incident shape, an
/// outage during a cold boot. The snapshot is `RetryMode::Rebuild`, as
/// Cloudflare's is, so a failure is remembered for the whole build.
async fn boot_prepared_while_variables_fail(
    path: &Path,
    ledger: &Arc<Ledger>,
    plan: &PreparedRuntimePlan,
    fault: Fault,
) -> (
    Result<impresspress_core::builder::BootReport, impresspress_core::builder::BootError>,
    Wafer,
    Arc<dyn DatabaseService>,
) {
    let shaped = Arc::new(D1Shaped::open(path, Arc::clone(ledger)));
    let db: Arc<dyn DatabaseService> = shaped.clone();
    // Ungated, so the failure is met by the boot's own first read: what a
    // cold isolate's degraded publish boots over, the gate having failed the
    // same way just before.
    let mut built = build(
        &db,
        Some(plan),
        false,
        &HashMap::new(),
        Arc::new(VariablesSnapshot::new(RetryMode::Rebuild)),
        false,
    )
    .await;
    shaped.fail_table(variables::TABLE, fault);
    let booted = boot_request_runtime(
        &mut built.wafer,
        &db,
        &built.block_settings,
        built.config.clone(),
        RequestBuild::Prepared,
    )
    .await;
    (booted, built.wafer, db)
}

/// **A cold prepared build handed the gate's failed read sends the variables
/// table nothing** (CPU-GATE review, acceptance 8): a cold isolate whose
/// gate failed builds anyway (Decision 1) over the snapshot the gate filled,
/// which replays the failure to every config reader. So the boot publishes
/// degraded without a second variables statement — even with the database
/// back by then, which proves it is the memo answering, not the outage.
#[tokio::test]
async fn a_prepared_build_over_a_failed_gate_sends_the_variables_table_nothing() {
    let _generation = GENERATION.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("d1.sqlite3");
    let ledger = Arc::new(Ledger::default());
    let plan = provision(&cold(&path, &ledger)).await;

    let shaped = Arc::new(D1Shaped::open(&path, Arc::clone(&ledger)));
    let db: Arc<dyn DatabaseService> = shaped.clone();
    let variables = workers_snapshot();
    shaped.fail_table(variables::TABLE, Fault::Unavailable);
    variables
        .check_backend(&db)
        .await
        .expect_err("the outage fails the gate");
    shaped.clear_fault();
    assert!(reads_variables(&ledger.take()), "the gate read the table");

    let mut built = build(&db, Some(&plan), false, &HashMap::new(), variables, false).await;
    let report = boot_request_runtime(
        &mut built.wafer,
        &db,
        &built.block_settings,
        built.config.clone(),
        RequestBuild::Prepared,
    )
    .await
    .expect("a cold isolate publishes degraded");
    let trips = ledger.take();
    assert!(
        !report.degraded.is_empty(),
        "the config readers were refused by the remembered failure"
    );
    assert!(
        !reads_variables(&trips),
        "the build sent the variables table a statement of its own: {trips:#?}"
    );
}

/// **A failed gate costs a sliver of the build it stops** (CPU-GATE review
/// m4 (i)): the retry window's check of a database that is down — the one
/// read a retry makes before it builds anything, here the worst case, the
/// first after a publish, with a cold schema cache — takes under 2 % of a
/// cold prepared build in the same process. A ratio, so the runner's speed
/// cancels: Workers cannot measure their own CPU (`performance.now()` is
/// frozen), and the live tail's `cpuTime` stays the number that matters. A
/// build is the best of three, a gate the worst of five, so noise can only
/// make the test fail later than the truth would.
#[tokio::test]
async fn a_failed_gate_costs_a_sliver_of_a_cold_build() {
    let _generation = GENERATION.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("d1.sqlite3");
    let ledger = Arc::new(Ledger::default());
    let plan = provision(&cold(&path, &ledger)).await;

    let mut build = Duration::MAX;
    for _ in 0..3 {
        let db = cold(&path, &ledger);
        let started = std::time::Instant::now();
        let (wafer, _, _) = boot_request(&db, RequestBuild::Prepared, &plan, &HashMap::new()).await;
        build = build.min(started.elapsed());
        drop(wafer);
    }

    let mut gated = Duration::ZERO;
    for _ in 0..5 {
        let shaped = Arc::new(D1Shaped::open(&path, Arc::clone(&ledger)));
        shaped.fail_table(variables::TABLE, Fault::Unavailable);
        let down: Arc<dyn DatabaseService> = shaped.clone();
        let started = std::time::Instant::now();
        workers_snapshot()
            .check_backend(&down)
            .await
            .expect_err("the outage fails the gate");
        gated = gated.max(started.elapsed());
    }
    ledger.take();

    println!("cold prepared build {build:?}, failed gate {gated:?}");
    assert!(
        gated * 50 < build,
        "a failed gate took {gated:?}, not under 2 % of a cold prepared build's {build:?}"
    );
}

/// **A request-path build constructs no block set.** A cold isolate's build
/// and boot — the prepared one and the database one, from the variables gate
/// through every block's `Init` — read each block's declaration once, at its
/// registration (wafer-run#478), and every later reader takes the registered
/// copies. `blocks::block_set_sweeps` counts every use of the block set
/// `blocks::all_block_infos()` constructs: a call, directly or through a
/// wrapper (`block_enabled_defaults`), and every read of `config_vars`'
/// declared-key memo — which `provision` below has already built in this
/// process, so a read here would be the sweep on a cold isolate and is
/// counted as one. Before this pin a prepared build swept twice: the plan
/// import and the plan exporter.
#[tokio::test]
async fn a_request_path_build_constructs_no_block_set() {
    let _generation = GENERATION.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("d1.sqlite3");
    let ledger = Arc::new(Ledger::default());
    let plan = provision(&cold(&path, &ledger)).await;

    for build_kind in [RequestBuild::Prepared, RequestBuild::Database] {
        let db = cold(&path, &ledger);
        let before = impresspress_core::blocks::block_set_sweeps();
        let (wafer, _, _) = boot_request(&db, build_kind, &plan, &HashMap::new()).await;
        assert_eq!(
            impresspress_core::blocks::block_set_sweeps(),
            before,
            "a {build_kind:?} request build constructed the feature blocks again"
        );
        drop(wafer);
    }
    ledger.take();
}

/// The blocks of `wafer` that declare config keys, by registered name.
fn config_declaring(wafer: &Wafer) -> Vec<String> {
    let declaring: Vec<String> = wafer
        .block_infos()
        .into_iter()
        .filter(|info| !info.config_keys.is_empty())
        .map(|info| info.name.clone())
        .collect();
    wafer
        .block_names()
        .into_iter()
        .filter(|name| declaring.contains(name))
        .collect()
}

/// **A variables fault a retry cannot fix fails a `Serving` boot
/// permanently** (#272 review M1; addendum §2.5 test 5). Admin reads no
/// stored config (IP-B0), so the boot reaches the first config-declaring
/// block, whose source load classifies the remembered failure as a
/// `SourceFault`: a permanent `Init` failure, which `Serving` does not
/// degrade. Before IP-B0 admin's dialect read turned every variables
/// failure into its own transient failure.
#[tokio::test]
async fn a_serving_boot_over_a_variables_fault_fails_permanently() {
    let _generation = GENERATION.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("d1.sqlite3");
    let ledger = Arc::new(Ledger::default());
    let plan = provision(&cold(&path, &ledger)).await;

    let (booted, wafer, _db) = boot_prepared_while_variables_fail(
        &path,
        &ledger,
        &plan,
        Fault::Internal("D1_ERROR: some unclassified fault: SQLITE_ERROR".to_string()),
    )
    .await;
    let error = booted.expect_err("a fault a retry cannot fix fails the boot");
    let first = config_declaring(&wafer)
        .into_iter()
        .find(|name| name != impresspress_core::blocks::admin::ADMIN_BLOCK_ID)
        .expect("a config-declaring block");
    match &error {
        impresspress_core::builder::BootError::BlockInit {
            block,
            error: wafer_run::InitError::Permanent(_),
        } => assert_eq!(block, &first, "{error}"),
        other => panic!("expected the first config-declaring block's permanent failure: {other}"),
    }
    assert!(!error.is_transient());
}

/// The blocks that read **shared** stored config at `Init` without declaring
/// it — shared keys are never declared by a block (`config_vars`) — each with
/// why. Together with the config-declaring blocks, they are exactly what a
/// variables outage degrades; a block that starts reading stored config at
/// `Init` belongs here, with its reason, or must stop.
const SHARED_CONFIG_INIT_READERS: &[(&str, &str)] = &[(
    impresspress_core::blocks::auth::AUTH_BLOCK_ID,
    "`AuthConfig::from_ctx` reads the WAFER_RUN_SHARED__AUTH__* settings and the \
     bootstrap credentials to decide whether to bootstrap an admin",
)];

/// **A variables outage publishes a degraded runtime** (addendum §2.5 test
/// 6): exactly the blocks that read stored config at `Init` are degraded —
/// the config-declaring ones and [`SHARED_CONFIG_INIT_READERS`] — while admin
/// and every `site-main` step block are `Ready`. A degraded block answers
/// `Unavailable` and is not initialized by the request.
#[tokio::test]
async fn a_serving_boot_over_a_variables_outage_publishes_degraded() {
    let _generation = GENERATION.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("d1.sqlite3");
    let ledger = Arc::new(Ledger::default());
    let plan = provision(&cold(&path, &ledger)).await;

    let (booted, wafer, db) =
        boot_prepared_while_variables_fail(&path, &ledger, &plan, Fault::Unavailable).await;
    let report = booted.expect("an outage publishes degraded");

    let mut degraded: Vec<String> = report.degraded.iter().map(|d| d.block.clone()).collect();
    degraded.sort();
    let declaring = config_declaring(&wafer);
    let mut readers: Vec<String> = declaring
        .iter()
        .cloned()
        .chain(
            SHARED_CONFIG_INIT_READERS
                .iter()
                .map(|(block, _why)| (*block).to_string()),
        )
        .collect();
    readers.sort();
    assert_eq!(
        degraded, readers,
        "exactly the blocks that read stored config at Init are degraded"
    );

    let states = wafer.init_states();
    let ready = |name: &str| matches!(states.get(name), Some(wafer_run::BlockInitState::Ready));
    assert!(ready(impresspress_core::blocks::admin::ADMIN_BLOCK_ID));
    let flow: Value = serde_json::from_str(impresspress_core::flows::site_main::JSON)
        .expect("the site-main flow parses");
    for step in flow["steps"].as_array().expect("steps") {
        let block = step["block"].as_str().expect("a step block");
        assert!(
            ready(block),
            "site-main step `{block}` is {:?}",
            states.get(block)
        );
    }

    // `/health` reports exactly those blocks, by their class, from the
    // runtime's own slots: a 503 with `Retry-After`. It is served — by a
    // block with no D1 dependency, over the database that is still refusing
    // the variables table — and sends that database nothing.
    ledger.take();
    let health = respond_flow(&wafer, &db, "site-main", "/health", None).await;
    assert_eq!(health.status, 503, "a degraded runtime is not healthy");
    assert!(
        health
            .headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("retry-after")
                && value.parse::<u32>().is_ok_and(|secs| secs >= 1)),
        "every 503 carries Retry-After: {:?}",
        health.headers
    );
    let body: Value = serde_json::from_slice(&health.body).expect("a JSON body");
    assert_eq!(body["status"], "degraded", "{body}");
    let mut listed: Vec<String> = body["unavailable"]
        .as_array()
        .expect("the unavailable blocks")
        .iter()
        .map(|entry| {
            assert_eq!(entry["class"], "retrying", "{entry}");
            entry["block"].as_str().expect("a block name").to_string()
        })
        .collect();
    listed.sort();
    assert_eq!(
        listed, readers,
        "`/health` lists exactly the degraded blocks"
    );
    assert!(
        !body.to_string().contains("injected outage"),
        "no backend text in the public body: {body}"
    );
    assert!(
        ledger.take().is_empty(),
        "`/health` reads the slots, never the database"
    );

    let degraded_block = declaring.first().expect("a degraded block").clone();
    ledger.take();
    let refused = wafer
        .run_block(
            &degraded_block,
            wafer_run::Message::new("probe"),
            InputStream::empty(),
        )
        .await
        .collect_buffered()
        .await
        .expect_err("a degraded block answers an error");
    assert!(
        matches!(&refused, wafer_block::TerminalNotResponse::Error(e) if e.code == wafer_run::ErrorCode::Unavailable),
        "{refused:?}"
    );
    assert!(
        matches!(
            wafer.init_states().get(&degraded_block),
            Some(wafer_run::BlockInitState::Retrying { .. })
        ),
        "`{degraded_block}` is still unsettled"
    );
    let sent = ledger.take();
    assert!(
        sent.is_empty(),
        "the dispatch ran no Init on `{degraded_block}` — an Init would read its config: {sent:#?}"
    );
}

/// **A degraded block's 503 tells the client nothing of the cause**, on
/// every way a client reaches it — a page, an htmx swap and an API call —
/// and on both of wafer-run's refusal branches. The refusal is the runtime's
/// (the block's handler never runs), and before wafer-run sanitized it the
/// body quoted the block's `Init` error — the D1 text, a table name, the
/// call budget's numbers.
///
/// **And the cause is on the console line the body's reference names.** The
/// framework's `tracing` events are captured through the console subscriber
/// the Cloudflare Worker installs, at the Worker's own level
/// (`console_tracing::WORKER_MAX_LEVEL`), so the line asserted is the one
/// `wrangler tail` shows: an operator holding the client's `(ref: X)` finds
/// `correlation_id=X` and the D1 cause on one line.
///
/// Each request takes a known branch rather than whichever the clock gives
/// it (the slot's transient backoff is 100 ms after a failed `Init`, doubling):
/// - **past the backoff** (`not_initialized_error`, the line's
///   `last_failure=`): the request waits until the slot reports no backoff
///   left — every request a long-lived degraded runtime refuses;
/// - **inside the backoff** (`init_error_to_wafer_error`, the line's
///   `error=`): `init_block` re-fails the `Init` over the still-failing
///   variables table right before the request, re-arming the backoff — the
///   branch the first request after a rebuild takes, and the one that leaked
///   D1's text in the Miniflare harness.
#[tokio::test]
async fn a_degraded_block_answers_503_without_its_cause() {
    let _generation = GENERATION.lock().await;
    let console: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let sink = Arc::clone(&console);
    let _console =
        tracing::subscriber::set_default(impresspress_core::console_tracing::ConsoleTracing::new(
            impresspress_core::console_tracing::WORKER_MAX_LEVEL,
            move |_level, line| sink.lock().expect("console lines").push(line),
        ));
    let page = vec![
        ("host", "localhost".to_string()),
        ("accept", "text/html".to_string()),
        ("sec-fetch-mode", "navigate".to_string()),
    ];
    // A swap is a fetch from the page, never a navigation.
    let htmx = vec![
        ("host", "localhost".to_string()),
        ("accept", "*/*".to_string()),
        ("sec-fetch-mode", "cors".to_string()),
        ("hx-request", "true".to_string()),
    ];
    let api = vec![
        ("host", "localhost".to_string()),
        ("accept", "application/json".to_string()),
    ];
    let auth_ui = "impresspress/auth-ui";
    for (fault, cause) in [
        (Fault::Unavailable, "injected outage"),
        (Fault::CallBudgetExhausted, "Too many API requests"),
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("d1.sqlite3");
        let ledger = Arc::new(Ledger::default());
        let plan = provision(&cold(&path, &ledger)).await;

        let (booted, wafer, db) =
            boot_prepared_while_variables_fail(&path, &ledger, &plan, fault.clone()).await;
        let report = booted.expect("an outage publishes degraded");
        assert!(
            report.degraded.iter().any(|d| d.block == auth_ui),
            "{fault:?}: `{auth_ui}` is degraded: {report:?}"
        );

        for (branch, line_marks) in [
            (
                "past the backoff",
                [
                    "the block is not initialized in this runtime",
                    "last_failure=",
                ],
            ),
            (
                "inside the backoff",
                ["the block's Init failed transiently", "error="],
            ),
        ] {
            for (shape, headers) in [("page", &page), ("htmx", &htmx), ("api", &api)] {
                let what = format!("{fault:?} {branch} {shape}");
                if branch == "past the backoff" {
                    wait_out_the_backoff(&wafer, auth_ui).await;
                } else {
                    wafer
                        .init_block(auth_ui)
                        .await
                        .expect_err("the Init fails again over the failing table");
                }
                let response = respond(&wafer, &db, "site-main", "/b/auth/login", headers).await;
                let body = String::from_utf8_lossy(&response.body);
                assert_eq!(response.status, 503, "{what}: {body}");
                for quoted in [
                    "D1_ERROR",
                    "injected",
                    "Too many API requests",
                    variables::TABLE,
                    "init transient failure",
                    "not initialized in this runtime",
                ] {
                    assert!(
                        !body.contains(quoted),
                        "{what}: the body quotes the cause ({quoted:?}): {body}"
                    );
                }
                let message = serde_json::from_slice::<Value>(&response.body)
                    .unwrap_or_else(|e| panic!("{what}: a JSON body ({e}): {body}"))["message"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{what}: a message: {body}"))
                    .to_string();
                let reference = message
                    .strip_prefix(&format!(
                        "block `{auth_ui}` is temporarily unavailable (ref: "
                    ))
                    .and_then(|rest| rest.strip_suffix(')'))
                    .unwrap_or_else(|| panic!("{what}: not the sanitized refusal: {message}"));
                let lines = console.lock().expect("console lines");
                assert!(
                    lines.iter().any(|line| {
                        line.contains(&format!("correlation_id={reference}"))
                            && line.contains(auth_ui)
                            && line_marks.iter().all(|mark| line.contains(mark))
                            && line.contains(cause)
                    }),
                    "{what}: no console line of this branch ({line_marks:?}) names \
                     `{reference}` and the cause ({cause:?}): {lines:#?}"
                );
            }
        }
    }
}

/// Wait until `block`'s slot has no transient backoff left, so the next
/// dispatch to it takes `InitWait::Refuse`'s "not initialized" branch.
async fn wait_out_the_backoff(wafer: &Wafer, block: &str) {
    for _ in 0..200 {
        match wafer.init_states().get(block) {
            Some(wafer_run::BlockInitState::Retrying { retry_in, .. }) if retry_in.is_zero() => {
                return
            }
            Some(wafer_run::BlockInitState::Retrying { retry_in, .. }) => {
                tokio::time::sleep(*retry_in + Duration::from_millis(1)).await;
            }
            other => panic!("`{block}` is not retrying: {other:?}"),
        }
    }
    panic!("`{block}`'s backoff never ran out");
}

/// **A deploy funnel under a spent D1 call budget reports the budget, as a
/// transient failure** — what `/_deploy/prepare` answers 503 (retry) on.
///
/// - At the Miniflare rehearsal's 4 calls, a fresh database fails the
///   build's own `block_settings` read (`build_runtime`'s eager read, copied
///   here as `build` copies it): the cold `list` of the missing table spends
///   the budget and its existence probe is refused. The read used to return
///   the list's `no such table`, and the deploy answered a permanent 500
///   naming a table.
/// - With enough calls for the build, admin's migrations run out part-way.
///   Every failure in the `Reported` boot's report is then transient — admin's
///   refused migration, the seed, and each later block the budget refused —
///   so the report is [`BootReport::failed_transiently`], and admin's own
///   error names the budget.
#[tokio::test]
async fn a_funnel_under_a_spent_call_budget_reports_it_transiently() {
    let _generation = GENERATION.lock().await;

    let tmp = tempfile::tempdir().expect("tempdir");
    let ledger = Arc::new(Ledger::default());
    let shaped = Arc::new(D1Shaped::open(
        &tmp.path().join("d1.sqlite3"),
        Arc::clone(&ledger),
    ));
    shaped.limit_calls(4);
    let db: Arc<dyn DatabaseService> = shaped.clone();
    let error = block_settings::load(&db)
        .await
        .expect_err("4 calls are not enough for the build's read");
    assert!(
        matches!(error, DatabaseError::CallBudgetExhausted(_)),
        "the build's read names the budget, not the missing table: {error:?}"
    );
    assert!(wafer_run::init_failure_is_transient(
        error.code(),
        error.detail_code()
    ));

    let tmp = tempfile::tempdir().expect("tempdir");
    let shaped = Arc::new(D1Shaped::open(
        &tmp.path().join("d1.sqlite3"),
        Arc::clone(&ledger),
    ));
    shaped.limit_calls(FUNNEL_CALLS_PAST_THE_BUILD);
    let db: Arc<dyn DatabaseService> = shaped.clone();
    let mut built = build(&db, None, true, &HashMap::new(), workers_snapshot(), false).await;
    let report = boot_deploy_runtime(
        &mut built.wafer,
        &db,
        &built.block_settings,
        built.config.clone(),
    )
    .await
    .expect("a Reported boot runs to its report");
    assert!(!report.ok, "{report:#?}");
    let admin = report
        .blocks
        .iter()
        .find(|b| b.block == impresspress_core::blocks::admin::ADMIN_BLOCK_ID)
        .expect("admin's outcome");
    assert!(!admin.ok && admin.transient, "{admin:#?}");
    let admin_error = admin.error.as_deref().unwrap_or_default();
    assert!(
        admin_error.contains(&format!(
            "has sent {FUNNEL_CALLS_PAST_THE_BUILD} D1 calls and may send \
             {FUNNEL_CALLS_PAST_THE_BUILD}, so this call was not sent"
        )),
        "admin's error names the spent budget: {admin_error}"
    );
    assert!(
        report.failed_transiently(),
        "every failure under a spent budget is transient: {report:#?}"
    );
}

/// **No call budget makes `/_deploy/prepare` fail permanently.** For every
/// budget from 1 to [`SWEPT_CALL_BUDGETS`], on a fresh database, the funnel —
/// the build's `block_settings` read, then the `Reported` boot — either
/// succeeds or fails only transiently: a build error the budget refused, or
/// a report whose every failure is transient (what the endpoint answers 503
/// for). The cut lands in a different step at each budget: the build's cold
/// read (1–4), admin's migrations, admin's variables seed — its read of the
/// table, its writes, its seed-hash stamp, which each re-wrapped the
/// budget's refusal without its detail code and so failed permanently
/// (28–49) — and the later blocks' `Init`s. Two sample budgets missed that
/// band; the sweep is what keeps the next re-wrap from bringing it back.
#[tokio::test]
async fn no_call_budget_fails_the_funnel_permanently() {
    let _generation = GENERATION.lock().await;
    let mut permanent = Vec::new();
    for budget in 1..=SWEPT_CALL_BUDGETS {
        let tmp = tempfile::tempdir().expect("tempdir");
        let shaped = Arc::new(D1Shaped::open(
            &tmp.path().join("d1.sqlite3"),
            Arc::new(Ledger::default()),
        ));
        shaped.limit_calls(budget);
        let db: Arc<dyn DatabaseService> = shaped.clone();
        let mut built =
            match try_build(&db, None, true, &HashMap::new(), workers_snapshot(), false).await {
                Ok(built) => built,
                Err(error) => {
                    if !wafer_run::init_failure_is_transient(error.code(), error.detail_code()) {
                        permanent.push(format!("{budget}: build: {error}"));
                    }
                    continue;
                }
            };
        let report = boot_deploy_runtime(
            &mut built.wafer,
            &db,
            &built.block_settings,
            built.config.clone(),
        )
        .await
        .expect("a Reported boot runs to its report");
        if !report.ok && !report.failed_transiently() {
            let failures: Vec<String> = [("grants", &report.grants), ("seed", &report.seed)]
                .into_iter()
                .filter(|(_, step)| !step.ok && !step.transient)
                .map(|(name, step)| format!("{name}: {:?}", step.error))
                .chain(
                    report
                        .blocks
                        .iter()
                        .filter(|block| !block.ok && !block.transient)
                        .map(|block| format!("{}: {:?}", block.block, block.error)),
                )
                .collect();
            permanent.push(format!("{budget}: {}", failures.join("; ")));
        }
    }
    assert!(
        permanent.is_empty(),
        "budgets that fail the funnel permanently:\n{}",
        permanent.join("\n")
    );
}

/// The budgets [`no_call_budget_fails_the_funnel_permanently`] sweeps: every
/// one up to past the last band the reviewer's 1–260 sweep found (admin's
/// variables seed, 28–49), with margin. A provision takes 210 calls; the
/// budgets above this reach only later blocks' `Init`s and the seal, which
/// the sweep up to 260 found transient.
const SWEPT_CALL_BUDGETS: usize = 64;

/// Enough calls for [`a_funnel_under_a_spent_call_budget_reports_it_transiently`]'s
/// build (its `block_settings` read and probe) and too few for admin's
/// migrations, which provisioning spends most of its 210 on.
const FUNNEL_CALLS_PAST_THE_BUILD: usize = 20;

/// The `[features]` table of the manifest at `crate_dir`.
fn manifest_features(crate_dir: &str) -> toml::Table {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(crate_dir)
        .join("Cargo.toml");
    let manifest: toml::Table = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .parse()
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .cloned()
        .expect("a [features] table")
}

/// **The golden is the Worker's runtime, feature for feature.** Every
/// `impresspress-core` feature is named here and is on exactly when a Worker
/// with every block it can compile has it:
///
/// - each `block-*` feature `impresspress-cloudflare` passes through
///   (`X = ["impresspress-core/X"]`) — on;
/// - `test-support` (and `migration-fixtures`, which it implies) — on: the
///   harness's own fixtures, no block and no boot step;
/// - everything else — off. That includes `embed-assets`, which a Worker can
///   pass through but serves from its assets binding instead, and `llm`,
///   `block-dev`, `site-runtime` and the native embedding features, which no
///   Worker has.
///
/// A feature added to either manifest fails here until it is classified, and
/// `just d1-cost` and the CI step name the result.
#[test]
fn the_harness_compiles_exactly_what_a_worker_can() {
    let core = manifest_features("impresspress-core");
    let declared: std::collections::BTreeSet<&str> = core
        .keys()
        .map(String::as_str)
        .filter(|name| *name != "default")
        .collect();
    let named: std::collections::BTreeSet<&str> =
        COMPILED_FEATURES.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        named, declared,
        "classify every impresspress-core feature in COMPILED_FEATURES"
    );

    let cloudflare = manifest_features("impresspress-cloudflare");
    let mut expected: std::collections::BTreeSet<&str> = cloudflare
        .iter()
        .filter(|(name, implied)| {
            name.starts_with("block-")
                && implied.as_array().is_some_and(|a| {
                    a.len() == 1 && a[0].as_str() == Some(&format!("impresspress-core/{name}"))
                })
        })
        .map(|(name, _)| name.as_str())
        .collect();
    expected.extend(["test-support", "migration-fixtures"]);
    let compiled: std::collections::BTreeSet<&str> = COMPILED_FEATURES
        .iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(
        compiled, expected,
        "this run must compile exactly what a Worker with every block it can compile has"
    );
}

/// A classifier over a fresh file with `x` (500 rows, a text primary key, as
/// every platform table has) and `y` (500 rows pointing at `x`).
fn model_fixture() -> (tempfile::TempDir, Classifier) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("model.sqlite3");
    let conn = rusqlite::Connection::open(&path).expect("open");
    conn.execute_batch(
        "CREATE TABLE x (id TEXT PRIMARY KEY, a INTEGER);
         CREATE TABLE y (id TEXT PRIMARY KEY, xid TEXT);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 500)
         INSERT INTO x SELECT printf('x%04d', i), i FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 500)
         INSERT INTO y SELECT printf('y%04d', i), printf('x%04d', i) FROM n;",
    )
    .expect("fixture");
    drop(conn);
    let classifier = Classifier::open(&path);
    (dir, classifier)
}

fn modelled(classifier: &Classifier, sql: &str, params: Vec<Value>) -> (Class, String, u64) {
    let c = classifier.classify(&Sent {
        sql: sql.to_string(),
        params,
    });
    (c.class, c.table, c.rows)
}

/// **The rows model counts a `LIMIT` only where it stops the scan**: a page
/// at an offset steps over the rows it skips, and an aggregate, a join or a
/// filter reads the whole table whatever the `LIMIT` says.
#[test]
fn a_limit_shortens_a_scan_only_where_it_stops_it() {
    let (_dir, c) = model_fixture();
    // The executor's paged list: ordered by the primary key, one scan.
    assert_eq!(
        modelled(
            &c,
            "SELECT * FROM \"x\" ORDER BY \"id\" ASC LIMIT ?",
            vec![json!(10)]
        ),
        (Class::Scan, "x".to_string(), 10),
        "a LIMIT with nothing before it stops the scan"
    );
    assert_eq!(
        modelled(
            &c,
            "SELECT * FROM \"x\" ORDER BY \"id\" ASC LIMIT ? OFFSET ?",
            vec![json!(10), json!(400)]
        ),
        (Class::Scan, "x".to_string(), 410),
        "a page at OFFSET 400 steps over the 400 rows it skips"
    );
    assert_eq!(
        modelled(&c, "SELECT SUM(\"a\") FROM \"x\" LIMIT 1", vec![]),
        (Class::Scan, "x".to_string(), 500),
        "an aggregate reads every row before its one result"
    );
    assert_eq!(
        modelled(
            &c,
            "SELECT * FROM \"y\" JOIN \"x\" ON \"x\".\"id\" = \"y\".\"xid\" LIMIT 1",
            vec![]
        ),
        (Class::Scan, "y".to_string(), 500),
        "a join filtered only by its ON reads the scanned table whole"
    );
    assert_eq!(
        modelled(
            &c,
            "SELECT * FROM \"x\" WHERE \"a\" > ? LIMIT 1",
            vec![json!(499)]
        ),
        (Class::Scan, "x".to_string(), 500),
        "a filter may read the whole table to find its row"
    );
}
