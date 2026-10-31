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
//! 2. **seed** — fixture rows, uncounted: legal documents, users, variables,
//!    grants, an admin session. `shared` deletes every block-scoped variables
//!    row, the auto-generated secrets included (see the golden's header).
//! 3. **database build** — `boot_request_runtime(RequestBuild::Database)`,
//!    with the build's own pre-`build()` `block_settings` read.
//! 4. **prepared build** — `boot_request_runtime(RequestBuild::Prepared)`,
//!    hydrated from phase 1's plan.
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
    blocks::auth::{repo as auth_repo, JWT_SECRET_KEY},
    builder::{
        add_request_config, boot_deploy_runtime, boot_request_runtime, request_config_source,
        seal_prepared_plan, structural_runtime_config, warm_config_surfaces, ImpresspressBuilder,
        PreparedPlanExporter, RequestBuild, RuntimeConfig, StructuralConfigInputs,
        PREPARE_RUNTIME_PLAN_KEY,
    },
    deferred::{self, DeferMode},
    features::BlockSettings,
    platform_state::{block_settings, user_roles, variables, wrap_grants},
    test_support::{
        d1_shaped::{D1Shaped, Ledger, RoundTrip, Sent},
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
}

/// The config source a built runtime holds, standing in for the Worker's
/// `config_source_proxy()`: it forwards to whichever source the current
/// request installed. On Cloudflare that is a fresh `DatabaseConfigSource` per
/// request (`warm_request_services`), so a warm request that resolved a
/// block's config would read the table again; the harness installs one the
/// same way before each warm request ([`warm_source`]).
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
/// the warm surfaces over the cached runtime's snapshot, then a fresh
/// `request_config_source` over this request's overlay.
fn warm_source(
    db: &Arc<dyn DatabaseService>,
    wafer: &Wafer,
    request_config: &HashMap<String, String>,
) -> Arc<dyn wafer_run::ConfigSource> {
    let (_config_map, overlay) =
        warm_config_surfaces(&bound(), wafer.config_snapshot(), request_config);
    request_config_source(db.clone(), overlay)
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
/// bound JWT secret, the consumer's request config, the config source over the
/// resulting overlay, the builder. The config passes and the source are the
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
        .config_source(source.clone());
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
    }
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
    let mut built = build(db, None, true, &HashMap::new()).await;
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
/// warm request installs into.
async fn boot_request(
    db: &Arc<dyn DatabaseService>,
    build_kind: RequestBuild,
    plan: &PreparedRuntimePlan,
    request_config: &HashMap<String, String>,
) -> (Wafer, Arc<RequestScopedSource>) {
    let prepared = (build_kind == RequestBuild::Prepared).then_some(plan);
    let mut built = build(db, prepared, false, request_config).await;
    boot_request_runtime(
        &mut built.wafer,
        db,
        &built.block_settings,
        built.config.clone(),
        build_kind,
    )
    .await
    .unwrap_or_else(|e| panic!("the {build_kind:?} request build boots: {e}"));
    (built.wafer, built.source)
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
            // Every block-scoped row goes, the auto-generated secrets
            // included; the site forwards those instead (a block's required
            // secret must come from somewhere, or its Init fails).
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
            http_codec::collect_http_response(
                wafer.run("site-main", msg, InputStream::empty()).await,
            )
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
    /// and whether the plan sorts through a temporary B-tree (so a `LIMIT`
    /// cannot stop the scan early).
    fn plan(&self, sent: &Sent) -> Option<(Vec<(String, String)>, bool)> {
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
        let sorts = details.iter().any(|d| d.contains("TEMP B-TREE"));
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
            sorts,
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
        let Some((plan, sorts)) = self.plan(sent) else {
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
            // A scan reads the whole table, unless a `LIMIT` with nothing to
            // filter or sort ahead of it stops it after the rows it returns.
            let stops_early = upper.contains(" LIMIT ") && !upper.contains(" WHERE ") && !sorts;
            let rows = if stops_early {
                self.scalar(&format!("SELECT COUNT(*) FROM ({sql})"), &sent.params)
                    .unwrap_or(0)
            } else {
                self.table_rows(&table)
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

const PHASE_DATABASE: &str = "3 database build";
const PHASE_PREPARED: &str = "4 prepared build";

const ALLOWED: &[Allowed] = &[
    Allowed {
        phase: PHASE_DATABASE,
        table: "wafer_run__auth__users",
        class: Class::Count,
        mixed: 1,
        shared: 1,
        because: "auth Init's bootstrap check on every build; D1-READ-COST PR 4 \
                  (provisioning-only seeds, `users::any_exists`) removes it",
    },
    Allowed {
        phase: PHASE_PREPARED,
        table: "wafer_run__auth__users",
        class: Class::Count,
        mixed: 1,
        shared: 1,
        because: "auth Init's bootstrap check on every build; D1-READ-COST PR 4 removes it",
    },
    Allowed {
        phase: PHASE_DATABASE,
        table: "impresspress__admin__wrap_grants",
        class: Class::Exists,
        mixed: 1,
        shared: 1,
        because: "`wrap_grants::load` probes before it lists; D1-READ-COST PR 5 (list \
                  first, probe only after a failure) removes it",
    },
    Allowed {
        phase: PHASE_DATABASE,
        table: "impresspress__admin__variables",
        class: Class::Scan,
        mixed: 2,
        shared: 10,
        because: "the config source's unfiltered snapshot and the config block's; with \
                  no block-scoped row the source re-reads it for each of the 9 \
                  config-declaring blocks. D1-READ-COST PR 6 makes it 1 per build",
    },
    Allowed {
        phase: PHASE_PREPARED,
        table: "impresspress__admin__variables",
        class: Class::Scan,
        mixed: 2,
        shared: 10,
        because: "as on the database build; D1-READ-COST PR 6 makes it 1 per build",
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
        let cold_build = short == PHASE_DATABASE || short == PHASE_PREPARED;
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

    let mut phases = Vec::new();

    // 3. A database build, on a cold isolate.
    let db = cold(&path, &ledger);
    let (wafer, _) = boot_request(&db, RequestBuild::Database, &plan, &request_config).await;
    drop(wafer);
    drop(db);
    phases.push(Phase::new(
        label(PHASE_DATABASE),
        &ledger.take(),
        &classifier,
        String::new(),
    ));

    // 4. A prepared build, on a cold isolate.
    let db = cold(&path, &ledger);
    let (wafer, source) = boot_request(&db, RequestBuild::Prepared, &plan, &request_config).await;
    phases.push(Phase::new(
        label(PHASE_PREPARED),
        &ledger.take(),
        &classifier,
        String::new(),
    ));
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
        source.install(warm_source(&db, &wafer, &request_config));
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
# Section headers count statements (the unit the Worker's D1 budget admits today,
# a batch counting each of its statements) and D1 calls (the unit D1 itself
# limits per invocation, a batch counting once).
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
# or only the rows it returns when a LIMIT stops it with nothing to filter or
# sort first; a COUNT every row it counts (the whole table unless an index
# narrows it); a SEARCH the rows it matches; pragma_table_info the rows it
# returns; EXISTS the schema rows up to the table it looks for (all of them when
# it is absent); ID-PROBE those plus the table's columns. A WRITE reads none.
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
# consumer binding its secrets does; products requires its secret. With no row
# carrying a block, the config source's pre-002 rule leaves its grouped snapshot
# unmemoized, so each config-declaring block re-reads the table (PR 6, R4).
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
    let runs = vec![run(Variant::Mixed).await, run(Variant::Shared).await];

    let mut problems = Vec::new();
    for (run, variant) in runs.iter().zip([Variant::Mixed, Variant::Shared]) {
        problems.extend(check_rules(variant, &run.phases));
    }
    // `/_deploy/prepare` is one Worker invocation: the whole provisioning —
    // migrations, seeds, config reads and the closing reads — must fit the
    // default per-invocation limit.
    let limit = impresspress_core::config_vars::D1_QUERIES_PER_INVOCATION_DEFAULT as usize;
    let provisioning = runs[0].provision.statements.len();
    if provisioning > limit {
        problems.push(format!(
            "provisioning sends {provisioning} statements, more than the {limit} D1 queries \
             `/_deploy/prepare` may run in its one invocation"
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
