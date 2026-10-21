//! A request that hands out a session must come from this site (SEC-03).
//!
//! `POST /b/auth/api/login`, `/signup` and `/refresh` are public and set the
//! `auth_token` cookie. The cookie-authenticated CSRF layer
//! (`csrf::enforce_origin_policy`) exempts them, since they carry no verified
//! session, so before this check a cross-site `<form enctype="text/plain">`
//! whose body happened to be valid JSON signed the victim's browser into the
//! attacker's account (login CSRF). Each of those handlers now obtains a
//! `csrf::SessionIssuance` first, which refuses a cross-origin browser request
//! (even one from an origin `WAFER_RUN_SHARED__CORS_ALLOWED_ORIGINS` allows),
//! and decodes its body only when it is declared `application/json`.
//!
//! These drive the runtime the native binary builds: `build_native_runtime`,
//! the shared `boot`, and the `site-main` flow, so the CORS step, the router,
//! auth-meta extraction, both CSRF layers and the real handlers all run.

use std::{collections::HashMap, path::Path, sync::Arc};

use impresspress_core::{
    blocks::auth::{
        config::{BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY},
        repo::{sessions, users},
    },
    builder::{boot, GrantSource, InitPolicy},
    config_vars::CORS_ALLOWED_ORIGINS_KEY,
    csrf::ORIGIN_BLOCKED_REASON,
};
use impresspress_native::InfraConfig;
use impresspress_password::pepper::PasswordPeppers;
use impresspress_server::{build_native_runtime, AppHooks, NativeBootHooks};
use wafer_block::{http_codec, InputStream};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_run::Wafer;

const ADMIN_EMAIL: &str = "admin@example.com";
const ADMIN_PASSWORD: &str = "correct-horse-battery-staple";
/// An origin the deployment lists in `WAFER_RUN_SHARED__CORS_ALLOWED_ORIGINS`.
const PARTNER_ORIGIN: &str = "https://partner.example";

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

/// A booted runtime with a bootstrapped admin, signups open and
/// [`PARTNER_ORIGIN`] allowed by CORS.
struct Site {
    wafer: Arc<Wafer>,
    database: Arc<dyn DatabaseService>,
    _tmp: tempfile::TempDir,
}

async fn start() -> Site {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("session_issuance.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");
    let infra = infra_for(&db_path, &storage_root);

    let database = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");
    let app_env = HashMap::from([
        (
            BOOTSTRAP_ADMIN_EMAIL_KEY.to_string(),
            ADMIN_EMAIL.to_string(),
        ),
        (
            BOOTSTRAP_ADMIN_PASSWORD_KEY.to_string(),
            ADMIN_PASSWORD.to_string(),
        ),
        (
            CORS_ALLOWED_ORIGINS_KEY.to_string(),
            PARTNER_ORIGIN.to_string(),
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
        _tmp: tmp,
    }
}

/// What came back: status, headers (lower-cased names) and the body.
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Answer {
    fn sets_session_cookie(&self) -> bool {
        self.headers
            .iter()
            .any(|(name, value)| name == "set-cookie" && value.starts_with("auth_token="))
    }

    fn header(&self, wanted: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, value)| value.as_str())
    }

    /// The origin refusal: a 403 giving [`ORIGIN_BLOCKED_REASON`], with no
    /// session cookie.
    fn assert_origin_refusal(&self) {
        assert_eq!(self.status, 403, "body: {}", self.body);
        assert_eq!(
            self.json()["message"],
            ORIGIN_BLOCKED_REASON,
            "refused for where it came from, nothing else: {}",
            self.body
        );
        assert!(
            !self.sets_session_cookie(),
            "a refused request sets no session cookie: {:?}",
            self.headers
        );
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("a JSON body ({e}): {}", self.body))
    }
}

/// The headers a browser on this site sends with a `fetch` POST of JSON.
const SAME_ORIGIN_JSON: &[(&str, &str)] = &[
    ("origin", "http://localhost"),
    ("sec-fetch-site", "same-origin"),
    ("content-type", "application/json"),
];

/// The headers a cross-site `<form enctype="text/plain">` POST carries.
const CROSS_SITE_FORM: &[(&str, &str)] = &[
    ("origin", "https://evil.example"),
    ("sec-fetch-site", "cross-site"),
    ("content-type", "text/plain"),
];

/// POST `body` to `path` through `site-main`, from `127.0.0.1` to `localhost`,
/// with `headers` on top of `Host` and `Accept: application/json`.
async fn post(site: &Site, path: &str, headers: &[(&str, &str)], body: &str) -> Answer {
    let mut all: Vec<(&str, &str)> = vec![("host", "localhost"), ("accept", "application/json")];
    all.extend_from_slice(headers);
    let msg = http_codec::build_http_message("POST", path, "", "127.0.0.1", all);
    let parts = http_codec::collect_http_response(
        site.wafer
            .run(
                "site-main",
                msg,
                InputStream::from_bytes(body.as_bytes().to_vec()),
            )
            .await,
    )
    .await;
    Answer {
        status: parts.status,
        headers: parts
            .headers
            .into_iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value))
            .collect(),
        body: String::from_utf8_lossy(&parts.body).into_owned(),
    }
}

fn admin_credentials() -> String {
    serde_json::json!({ "email": ADMIN_EMAIL, "password": ADMIN_PASSWORD }).to_string()
}

async fn rows(site: &Site, table: &str) -> i64 {
    site.database
        .count(table, &[])
        .await
        .unwrap_or_else(|e| panic!("count {table}: {e}"))
}

/// The review's reproduction: a cross-site `text/plain` form whose body is
/// valid JSON (`x` soaks up the `=` a form inserts) posts the attacker's
/// credentials. It is refused before the password is checked: no cookie, no
/// session.
#[tokio::test]
async fn a_cross_site_text_plain_login_is_refused_and_signs_no_one_in() {
    let site = start().await;
    let before = rows(&site, sessions::TABLE).await;

    let body = format!(r#"{{"email":"{ADMIN_EMAIL}","password":"{ADMIN_PASSWORD}","x":"="}}"#);
    let answer = post(&site, "/b/auth/api/login", CROSS_SITE_FORM, &body).await;

    answer.assert_origin_refusal();
    assert_eq!(
        rows(&site, sessions::TABLE).await,
        before,
        "a refused login starts no session"
    );
}

/// Without Fetch Metadata, an `Origin` naming another site is cross-origin.
#[tokio::test]
async fn a_login_from_a_foreign_origin_without_fetch_metadata_is_refused() {
    let site = start().await;
    let answer = post(
        &site,
        "/b/auth/api/login",
        &[
            ("origin", "https://evil.example"),
            ("content-type", "application/json"),
        ],
        &admin_credentials(),
    )
    .await;
    answer.assert_origin_refusal();
}

/// This site's own page signs in as it always has.
#[tokio::test]
async fn a_same_origin_json_login_signs_in() {
    let site = start().await;
    let answer = post(
        &site,
        "/b/auth/api/login",
        SAME_ORIGIN_JSON,
        &admin_credentials(),
    )
    .await;
    assert_eq!(answer.status, 200, "body: {}", answer.body);
    assert!(answer.sets_session_cookie(), "{:?}", answer.headers);
}

/// An API client, the CLI or `curl` sends no browser headers at all. There
/// is no victim's browser to forge such a request from, so it is admitted.
#[tokio::test]
async fn a_json_login_with_no_browser_headers_signs_in() {
    let site = start().await;
    let answer = post(
        &site,
        "/b/auth/api/login",
        &[("content-type", "application/json")],
        &admin_credentials(),
    )
    .await;
    assert_eq!(answer.status, 200, "body: {}", answer.body);
    assert!(answer.json()["access_token"].is_string(), "{}", answer.body);
}

/// CORS lets an allowlisted origin read responses; it does not let that
/// origin obtain this site's session (user decision 5).
#[tokio::test]
async fn a_login_from_a_cors_allowlisted_origin_is_refused() {
    let site = start().await;
    let answer = post(
        &site,
        "/b/auth/api/login",
        &[
            ("origin", PARTNER_ORIGIN),
            ("sec-fetch-site", "cross-site"),
            ("content-type", "application/json"),
        ],
        &admin_credentials(),
    )
    .await;
    assert_eq!(
        answer.header("access-control-allow-origin"),
        Some(PARTNER_ORIGIN),
        "precondition: the deployment's CORS step allows this origin: {:?}",
        answer.headers
    );
    answer.assert_origin_refusal();
}

/// Defence in depth: even from this site, a body not declared as JSON is not
/// decoded as JSON. A `text/plain` body is what a cross-site form can send
/// without a preflight.
#[tokio::test]
async fn a_same_origin_text_plain_login_is_a_415_and_signs_no_one_in() {
    let site = start().await;
    let before = rows(&site, sessions::TABLE).await;
    let answer = post(
        &site,
        "/b/auth/api/login",
        &[
            ("origin", "http://localhost"),
            ("sec-fetch-site", "same-origin"),
            ("content-type", "text/plain"),
        ],
        &admin_credentials(),
    )
    .await;
    assert_eq!(answer.status, 415, "body: {}", answer.body);
    assert!(!answer.sets_session_cookie(), "{:?}", answer.headers);
    assert_eq!(rows(&site, sessions::TABLE).await, before);
}

/// A cross-site signup is refused before the account is written, so it
/// creates nothing, not merely no session.
#[tokio::test]
async fn a_cross_site_signup_creates_no_account() {
    let site = start().await;
    let before = rows(&site, users::TABLE).await;
    let answer = post(
        &site,
        "/b/auth/api/signup",
        &[
            ("origin", "https://evil.example"),
            ("sec-fetch-site", "cross-site"),
            ("content-type", "application/json"),
        ],
        r#"{"email":"planted@example.com","password":"correct-horse-battery"}"#,
    )
    .await;
    answer.assert_origin_refusal();
    assert_eq!(
        rows(&site, users::TABLE).await,
        before,
        "a refused signup writes no account"
    );
}

/// A cross-site refresh is refused before the presented token is looked at:
/// it is not rotated away or treated as a replay, so the client that holds it
/// can still refresh with it.
#[tokio::test]
async fn a_cross_site_refresh_leaves_the_presented_token_live() {
    let site = start().await;
    let signed_in = post(
        &site,
        "/b/auth/api/login",
        SAME_ORIGIN_JSON,
        &admin_credentials(),
    )
    .await;
    assert_eq!(signed_in.status, 200, "body: {}", signed_in.body);
    let refresh_token = signed_in.json()["refresh_token"]
        .as_str()
        .expect("login hands out a refresh token")
        .to_string();
    let body = serde_json::json!({ "refresh_token": refresh_token }).to_string();

    let refused = post(
        &site,
        "/b/auth/api/refresh",
        &[
            ("origin", "https://evil.example"),
            ("sec-fetch-site", "cross-site"),
            ("content-type", "application/json"),
        ],
        &body,
    )
    .await;
    refused.assert_origin_refusal();

    let rotated = post(&site, "/b/auth/api/refresh", SAME_ORIGIN_JSON, &body).await;
    assert_eq!(
        rotated.status, 200,
        "the token the refused refresh presented is still live: {}",
        rotated.body
    );
    assert!(rotated.sets_session_cookie(), "{:?}", rotated.headers);
}
