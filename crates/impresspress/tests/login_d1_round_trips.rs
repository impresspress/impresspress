//! The database round trips a password sign-in waits on, counted the way D1
//! charges them.
//!
//! On Cloudflare every D1 statement is a network round trip from the Worker to
//! the database's region, and a sign-in answers only after the last one it
//! awaits. Measured on a Workers Free deployment (client and Worker in
//! Auckland, D1 in region `OC`), each of those round trips cost about 20 ms,
//! and they, not CPU or the password hash, were most of a warm sign-in's time.
//! So the number of SEQUENTIAL round trips before the response is the latency
//! budget, and the number of statements the D1 query budget.
//!
//! These tests drive the real `site-main` flow of the runtime the native binary
//! builds, the way the Cloudflare entry drives it: inside an
//! [`after_response::scope`], with [`DeferMode::Queued`], so the audit row and
//! the deferred tasks are queued rather than run inline, and what the
//! response waits on is exactly what runs inside the scope.
//!
//! The database is the platform's SQLite service behind [`D1Shaped`]
//! (`impresspress_core::test_support::d1_shaped`), which makes it behave like
//! the D1 adapter in the two ways that decide what a request costs there and
//! records every round trip; see its module docs.

use std::{collections::HashMap, path::Path, sync::Arc};

use impresspress_core::{
    after_response::{self, AfterResponse},
    blocks::auth::{
        config::{BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY},
        repo::users,
    },
    builder::{boot, GrantSource, InitPolicy},
    deferred::{self, DeferMode},
    test_support::d1_shaped::{D1Shaped, Ledger, RoundTrip},
};
use impresspress_native::InfraConfig;
use impresspress_password::pepper::PasswordPeppers;
use impresspress_server::{build_native_runtime, AppHooks, NativeBootHooks};
use wafer_block::{http_codec, InputStream};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_run::Wafer;

const ADMIN_EMAIL: &str = "admin@example.com";
const ADMIN_PASSWORD: &str = "correct-horse-battery-staple";

fn infra_for(db_path: &Path, storage_root: &Path) -> InfraConfig {
    InfraConfig {
        listen: "127.0.0.1:0".to_string(),
        db_type: "sqlite".to_string(),
        db_path: db_path
            .to_str()
            .expect("db path is valid utf-8")
            .to_string(),
        db_url: None,
        storage_type: "local".to_string(),
        storage_root: storage_root
            .to_str()
            .expect("storage root is valid utf-8")
            .to_string(),
        model_cache_dir: "data/models".to_string(),
        listener: Default::default(),
    }
}

/// A booted runtime over a [`D1Shaped`] database, with a bootstrapped admin.
struct Site {
    wafer: Arc<Wafer>,
    database: Arc<dyn DatabaseService>,
    ledger: Arc<Ledger>,
    _tmp: tempfile::TempDir,
}

async fn start() -> Site {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("login.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let infra = infra_for(&db_path, &storage_root);

    let ledger = Arc::new(Ledger::default());
    let database: Arc<dyn DatabaseService> =
        Arc::new(D1Shaped::open(&db_path, Arc::clone(&ledger)));
    let app_env = HashMap::from([
        (
            BOOTSTRAP_ADMIN_EMAIL_KEY.to_string(),
            ADMIN_EMAIL.to_string(),
        ),
        (
            BOOTSTRAP_ADMIN_PASSWORD_KEY.to_string(),
            ADMIN_PASSWORD.to_string(),
        ),
    ]);
    let mut wafer = build_native_runtime(
        &infra,
        database.clone(),
        &app_env,
        PasswordPeppers::default(),
        AppHooks::none(),
    )
    .await
    .expect("build impresspress runtime");
    let report = boot(
        &mut wafer,
        &NativeBootHooks,
        GrantSource::PreInstalled(
            "build_native_runtime loads them from the platform database into \
             ImpresspressBuilder::wrap_grants before build()",
        ),
        InitPolicy::Reported,
    )
    .await
    .expect("seal");
    assert!(report.ok, "boot must succeed: {report:?}");
    wafer.run_start_lifecycle().await;
    Site {
        wafer: wafer.bind_all(),
        database,
        ledger,
        _tmp: tmp,
    }
}

/// The admin's `last_login_at`, read straight from the platform database.
async fn last_login_at(database: &Arc<dyn DatabaseService>) -> Option<String> {
    let opts = wafer_block::db::ListOptions {
        filters: vec![wafer_block::db::Filter {
            field: "email".to_string(),
            operator: wafer_block::db::FilterOp::Equal,
            value: serde_json::json!(ADMIN_EMAIL),
        }],
        limit: Some(1),
        skip_count: true,
        ..Default::default()
    };
    let rows = database
        .list(users::TABLE, &opts)
        .await
        .expect("read the admin's row")
        .records;
    rows.first()
        .and_then(|row| row.data.get("last_login_at"))
        .and_then(serde_json::Value::as_str)
        .filter(|at| !at.is_empty())
        .map(str::to_string)
}

/// What one request cost: the round trips its response waited on, and the
/// ones it left to run after it.
struct Cost {
    status: u16,
    body: serde_json::Value,
    response: Vec<RoundTrip>,
    after: Vec<RoundTrip>,
}

impl Cost {
    fn waves(trips: &[RoundTrip]) -> usize {
        trips.iter().map(|t| t.wave).max().unwrap_or(0)
    }

    fn statements(trips: &[RoundTrip]) -> usize {
        trips.iter().map(|t| t.statements()).sum()
    }

    fn describe(trips: &[RoundTrip]) -> String {
        trips
            .iter()
            .map(|t| format!("  wave {} [{}] {}", t.wave, t.statements(), t.sql()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Send `method path` with `headers` and `body` as the Cloudflare entry runs
/// a request: dispatch inside a fresh [`after_response::scope`], then the
/// audit row, then the deferred tasks.
async fn run_request(
    site: &Site,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Cost {
    deferred::set_mode(DeferMode::Queued);
    site.ledger.take();

    let msg =
        http_codec::build_http_message(method, path, "", "127.0.0.1", headers.iter().copied());
    let after = AfterResponse::new();
    let wafer = Arc::clone(&site.wafer);
    let (status, body) = after_response::scope(std::rc::Rc::clone(&after), async move {
        let parts = http_codec::collect_http_response(
            wafer
                .run("site-main", msg, InputStream::from_bytes(body))
                .await,
        )
        .await;
        (parts.status, parts.body)
    })
    .await;
    let response = site.ledger.take();

    if let Some(row) = after.take_audit_row() {
        after_response::persist_audit_row(site.database.as_ref(), row)
            .await
            .expect("audit row written");
    }
    for task in after.take_tasks() {
        task.await;
    }
    let after = site.ledger.take();
    Cost {
        status,
        body: serde_json::from_slice(&body).unwrap_or_default(),
        response,
        after,
    }
}

/// POST a password sign-in.
async fn sign_in(site: &Site, email: &str, password: &str) -> Cost {
    let body = serde_json::json!({ "email": email, "password": password }).to_string();
    run_request(
        site,
        "POST",
        "/b/auth/api/login",
        &[
            ("host", "localhost"),
            ("origin", "http://localhost"),
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/json"),
            ("accept", "application/json"),
        ],
        body.into_bytes(),
    )
    .await
}

/// A warm sign-in's response waits on four sequential D1 round trips, five
/// statements: the account by email, its credential, its role grants, and one
/// batch that writes the login family's refresh-token row and session row
/// together (`helpers::establish_family`). Each lookup is the one
/// `SELECT … LIMIT 1` `db::get_by_field` sends, with no `COUNT(*)` beside it.
/// The tokens' `auth_version` is the one on the account row read first — the
/// version must be read no later than the roles
/// (`helpers::TokenGrant`), so the row is not read again for them.
/// Everything else it writes — the last-login stamp, the retention throttle
/// check, the audit row — runs after the response.
///
/// On Cloudflare the sign-in route's rate limit adds one more before any of
/// these (`auth::repo::rate_limits::windowed_increment`, one upsert that
/// returns the counter). That limiter is D1-backed only on wasm32; the native
/// one this runtime uses keeps its counters in memory, so it is not counted
/// here.
///
/// "Warm" is the second sign-in in the process: the first fills the caches a
/// Worker isolate holds (schema facts, including the id source of every table
/// it creates into, the config snapshot, the timing-equalization hash) and
/// runs the hourly retention pass.
///
/// The counts fail on earlier code: the family's two rows were two writes,
/// one after the other, the roles' two reads ran one after the
/// other, a brand-new login family was first "touched" by an UPDATE that could
/// match nothing, the retention throttle read and the last-login UPDATE ran
/// before the response, the users row was read twice more, for the roles
/// and for the version, each lookup carried an unread `COUNT(*)`, and the
/// second create into an insert-only table probed its id source again.
#[tokio::test]
async fn a_warm_sign_in_waits_on_four_round_trips() {
    let site = start().await;
    let cold = sign_in(&site, ADMIN_EMAIL, ADMIN_PASSWORD).await;
    assert_eq!(cold.status, 200);

    let warm = sign_in(&site, ADMIN_EMAIL, ADMIN_PASSWORD).await;
    assert_eq!(warm.status, 200);
    let trace = format!(
        "response path:\n{}\nafter the response:\n{}",
        Cost::describe(&warm.response),
        Cost::describe(&warm.after)
    );

    assert_eq!(
        Cost::waves(&warm.response),
        4,
        "sequential D1 round trips before the response\n{trace}"
    );
    assert_eq!(
        Cost::statements(&warm.response),
        5,
        "D1 statements before the response\n{trace}"
    );
    // A lookup is its `SELECT … LIMIT 1` alone: no total nothing reads.
    assert!(
        !warm.response.iter().any(|t| t.sql().contains("COUNT(*)")),
        "a sign-in reads no totals\n{trace}"
    );
    // The first sign-in's creates cached every id source this one needs, the
    // audit row's insert-only table included.
    assert!(
        !warm
            .response
            .iter()
            .chain(&warm.after)
            .any(|t| t.sql().contains("id_policy")),
        "a warm sign-in probes no table's id source\n{trace}"
    );

    // The security reads stay on the response path, in order: the account
    // row (which carries the `auth_version` the tokens embed) is read before
    // the role grants, so a role change landing between them leaves the
    // token's version behind the change's bump.
    let wave_of = |needle: &str| {
        warm.response
            .iter()
            .find(|t| t.sql().contains(needle))
            .map(|t| t.wave)
            .unwrap_or_else(|| panic!("no {needle} read on the response path\n{trace}"))
    };
    assert!(
        wave_of("FROM \"wafer_run__auth__users\" WHERE \"email\"")
            < wave_of("FROM \"impresspress__admin__user_roles\""),
        "the account row must be read before the role grants\n{trace}"
    );
    // The family's two rows are written before the response, in ONE round
    // trip: without the refresh row the refresh token handed out could never
    // be used, and without the session row its first refresh is refused.
    let wave_of_insert = |table: &str| {
        warm.response
            .iter()
            .find(|t| t.sql().contains(&format!("INSERT INTO \"{table}\"")))
            .map(|t| t.wave)
            .unwrap_or_else(|| panic!("no insert into {table} on the response path\n{trace}"))
    };
    assert_eq!(
        wave_of_insert("wafer_run__auth__tokens"),
        wave_of_insert("wafer_run__auth__sessions"),
        "the refresh row and the session row are one batch\n{trace}"
    );
}

/// The work moved off the response path still happens: the last-login stamp,
/// the retention throttle check and the audit row all run after it.
#[tokio::test]
async fn the_bookkeeping_a_sign_in_defers_still_runs() {
    let site = start().await;
    let cold = sign_in(&site, ADMIN_EMAIL, ADMIN_PASSWORD).await;
    assert_eq!(cold.status, 200);

    let first_stamp = last_login_at(&site.database)
        .await
        .expect("the first sign-in stamped last_login_at");

    let warm = sign_in(&site, ADMIN_EMAIL, ADMIN_PASSWORD).await;
    assert_eq!(warm.status, 200);
    let trace = Cost::describe(&warm.after);

    for (what, needle) in [
        ("the last-login stamp", "UPDATE \"wafer_run__auth__users\""),
        (
            "the retention throttle check",
            "FROM \"wafer_run__auth__maintenance\"",
        ),
        (
            "the audit row",
            "INSERT INTO \"impresspress__admin__request_logs\"",
        ),
    ] {
        assert!(
            warm.after.iter().any(|t| t.sql().contains(needle)),
            "{what} must run after the response:\n{trace}"
        );
        assert!(
            !warm.response.iter().any(|t| t.sql().contains(needle)),
            "{what} must not delay the response:\n{}",
            Cost::describe(&warm.response)
        );
    }
    assert!(
        last_login_at(&site.database)
            .await
            .is_some_and(|at| at >= first_stamp),
        "the deferred stamp must land"
    );
}

/// An authenticated request checks its access token's database facts in ONE
/// round trip, and that round trip carries only them:
/// `crypto::verify_access_token` reads the login family's session row and
/// (on a cache miss) the account's `auth_version` with `futures::join!`, and
/// nothing else. The family read is what SEC-02 added to every
/// JWT-authenticated request; revocation must bite at once, so it is not
/// cached, and it is read on every request, the warm one included.
///
/// Both halves of the join are measured. On the first request with the token
/// the `auth_version` cache is cold (the sign-in does not fill it), so the
/// family read and the version read share the first wave and nothing joins
/// them. On the second, the version is a cache hit and the family read rides
/// alone.
///
/// The whole request is pinned as well, as the sign-in's is: the check, then
/// `/me`'s own reads of the account (`users`) and its roles (`user_roles`).
/// Cold, that is 3 round trips and 4 statements; warm, 3 and 3. The per-wave
/// assertions say which statements share the check's round trip; the totals
/// are what catch a statement added to the check after the join, which costs
/// a round trip of its own and would otherwise hide among the handler's reads.
///
/// The first request is measured on a warm schema cache. The family read
/// lists the sessions table, which needs its primary key, and on a cold cache
/// that probe runs before the read and lands in the first wave or opens the
/// next one depending on how fast the version read returns. On D1 the cache
/// is the isolate's, warmed by whichever earlier request first listed the
/// table; here one list of the table, outside the measured request, stands
/// in for it.
#[tokio::test]
async fn an_access_token_is_checked_in_one_round_trip() {
    let site = start().await;
    let signed_in = sign_in(&site, ADMIN_EMAIL, ADMIN_PASSWORD).await;
    assert_eq!(signed_in.status, 200);
    let bearer = format!(
        "Bearer {}",
        signed_in.body["access_token"]
            .as_str()
            .expect("the sign-in hands out an access token")
    );
    let headers = [
        ("host", "localhost"),
        ("accept", "application/json"),
        ("authorization", bearer.as_str()),
    ];
    let me = || run_request(&site, "GET", "/b/auth/api/me", &headers, Vec::new());
    let reads_of = |trips: &[RoundTrip], table: &str| {
        trips
            .iter()
            .filter(|t| t.sql().contains(&format!("FROM \"{table}\"")))
            .map(|t| t.wave)
            .collect::<Vec<_>>()
    };
    let wave = |trips: &[RoundTrip], wave: usize| {
        trips
            .iter()
            .filter(|t| t.wave == wave)
            .map(|t| t.sql())
            .collect::<Vec<_>>()
    };

    site.database
        .list(
            "wafer_run__auth__sessions",
            &wafer_block::db::ListOptions {
                limit: Some(1),
                skip_count: true,
                ..Default::default()
            },
        )
        .await
        .expect("warm the sessions table's schema");

    let cold = me().await;
    let trace = Cost::describe(&cold.response);
    assert_eq!(cold.status, 200, "the token authenticates\n{trace}");
    let family = reads_of(&cold.response, "wafer_run__auth__sessions");
    assert_eq!(family, vec![1], "the family is read once, first\n{trace}");
    let check = wave(&cold.response, 1);
    assert_eq!(
        check.len(),
        2,
        "the check's round trip is the family read and the auth_version read, \
         nothing else\n{trace}"
    );
    assert!(
        check
            .iter()
            .any(|sql| sql.contains("FROM \"wafer_run__auth__users\"")),
        "the auth_version read shares the family read's round trip\n{trace}"
    );
    assert_eq!(
        Cost::waves(&cold.response),
        3,
        "sequential D1 round trips before the response\n{trace}"
    );
    assert_eq!(
        Cost::statements(&cold.response),
        4,
        "D1 statements before the response\n{trace}"
    );

    let warm = me().await;
    let trace = Cost::describe(&warm.response);
    assert_eq!(warm.status, 200, "the token authenticates\n{trace}");
    let family = reads_of(&warm.response, "wafer_run__auth__sessions");
    assert_eq!(
        family,
        vec![1],
        "the login family is read once per request, uncached\n{trace}"
    );
    assert_eq!(
        wave(&warm.response, 1).len(),
        1,
        "with auth_version cached, the family read rides alone\n{trace}"
    );
    assert_eq!(
        Cost::waves(&warm.response),
        3,
        "sequential D1 round trips before the response\n{trace}"
    );
    assert_eq!(
        Cost::statements(&warm.response),
        3,
        "D1 statements before the response\n{trace}"
    );
}
