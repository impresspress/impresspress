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
    database::service::DatabaseService,
    network::service::{NetworkError, NetworkService, Request, Response},
};
use wafer_run::Wafer;

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
async fn build(
    db: &Arc<dyn DatabaseService>,
    prepared: Option<&PreparedRuntimePlan>,
    provisioning: bool,
    request_config: &HashMap<String, String>,
    variables: Arc<VariablesSnapshot>,
) -> Built {
    let settings = match prepared {
        Some(plan) => BlockSettings::from_blocks(
            plan.structure
                .block_settings
                .iter()
                .map(|(name, state)| (name.clone(), state.clone()))
                .collect(),
        ),
        None => block_settings::load(db)
            .await
            .expect("the build's block_settings read"),
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
    let (builder, config) = install(runtime_config);
    let builder = builder
        .database(db.clone())
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
    let builder = match prepared {
        Some(plan) => {
            let (sha, lock, assets) = identity();
            builder
                .apply_prepared_plan(plan, APP_ID, &sha, &lock, &assets)
                .expect("the plan applies")
        }
        None => builder,
    };
    let exporter = builder.prepared_plan_exporter().expect("an exporter");
    let block_settings = builder.block_settings_handle();
    let wafer = builder.build().expect("the runtime builds");
    Built {
        wafer,
        exporter,
        block_settings,
        config,
        source,
        variables,
    }
}

/// The variables snapshot `build_runtime` creates: retrying a failed read on
/// its own, on the wall clock.
fn workers_snapshot() -> Arc<VariablesSnapshot> {
    Arc::new(VariablesSnapshot::new(RetryMode::Backoff))
}

fn install(runtime_config: RuntimeConfig) -> (ImpresspressBuilder, Arc<dyn ConfigService>) {
    runtime_config.install(ImpresspressBuilder::new(), |map| {
        let service = impresspress_core::builder::fill_config_service(
            Arc::new(wafer_core::service_blocks::config::EnvConfigService::new()),
            map,
        );
        (service.clone(), service)
    })
}

/// Phase 1: the deploy funnel over an empty database, then the prepare
/// closing sequence; returns the plan.
async fn provision(db: &Arc<dyn DatabaseService>) -> PreparedRuntimePlan {
    let mut built = build(db, None, true, &HashMap::new(), workers_snapshot()).await;
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
    let mut built = build(db, prepared, false, request_config, workers_snapshot()).await;
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
    deferred::set_mode(DeferMode::Queued);
    let mut headers = vec![
        ("host", "localhost".to_string()),
        ("accept", "text/html".to_string()),
        ("sec-fetch-mode", "navigate".to_string()),
        ("sec-fetch-site", "none".to_string()),
    ];
    if let Some(token) = cookie {
        headers.push(("cookie", format!("auth_token={token}")));
    }
    let msg = http_codec::build_http_message(
        "GET",
        path,
        "",
        "127.0.0.1",
        headers.iter().map(|(k, v)| (*k, v.as_str())),
    );
    let after = AfterResponse::new();
    let status = after_response::scope_root(
        Rc::clone(&after),
        async {
            http_codec::collect_http_response(wafer.run(flow, msg, InputStream::empty()).await)
                .await
                .status
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
    status
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
        .map(|info| info.name)
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
    let mut built = build(db, Some(plan), false, &HashMap::new(), variables).await;
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

    /// `build_runtime`'s snapshot (`Backoff`), on this clock.
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
/// (Today's 500 becomes the outage plan's 503 in its I4b.)
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
        500,
        "the landing switch fails closed: an error, not the redirect its default decides"
    );
    assert!(
        !reads_variables(&ledger.take()),
        "before its retry, the remembered failure answers with no query"
    );
}

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
