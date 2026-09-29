//! The password pepper on the runtime the binary builds.
//!
//! `run()` reads the three `IMPRESSPRESS_PASSWORD_PEPPER_*` variables with
//! `password_peppers_from_env` and hands the result to `build_native_runtime`,
//! which gives it to the crypto service and to nothing else. These tests drive
//! that path — `build_native_runtime`, the shared `boot`, the auth block's
//! bootstrap (which hashes through the crypto block), the `site-main` login
//! route and the `wafer-run/config` block a block's config client reaches —
//! and check each half of the contract: the pepper is used, and nothing can
//! read it back.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

use impresspress_core::{
    blocks::auth::config::{BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY},
    builder::{boot, GrantSource, InitPolicy},
};
use impresspress_native::InfraConfig;
use impresspress_password::pepper::{
    self as password_pepper, PasswordPeppers, PASSWORD_PEPPER_KEY_VAR,
    PASSWORD_PEPPER_PREVIOUS_KEYS_VAR, PASSWORD_PEPPER_REQUIRED_VAR,
};
use impresspress_server::{
    build_native_runtime, password_peppers_from_env, AppHooks, NativeBootHooks,
};
use wafer_block::{http_codec, InputStream};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_run::{Message, Wafer};

/// A pepper key as `openssl rand -base64 32` prints one: 32 bytes of `0x2a`.
const KEY: &str = "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=";
/// A second key, for the previous-keys list: 32 bytes of `0x2b`.
const PREVIOUS_KEY: &str = "KysrKysrKysrKysrKysrKysrKysrKysrKysrKysrKys=";

const ADMIN_EMAIL: &str = "admin@example.com";
const ADMIN_PASSWORD: &str = "correct-horse-battery-staple";

/// A runtime-owned key native boot puts on the boot map, read as the
/// probe's control: the config block serves it as it would a pepper
/// variable put there.
const CONTROL_KEY: &str = impresspress_core::platform_state::variables::HAS_PROCESS_ENV_CONFIG_KEY;

const PEPPER_VARS: [&str; 3] = [
    PASSWORD_PEPPER_KEY_VAR,
    PASSWORD_PEPPER_PREVIOUS_KEYS_VAR,
    PASSWORD_PEPPER_REQUIRED_VAR,
];

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

/// What `run()` passes `password_peppers_from_env`: a lookup into the process
/// environment, here a fixed map, so no test mutates the real one.
fn process_env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let vars: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |key| vars.get(key).cloned()
}

/// The app config `run()` collects: a bootstrap admin — and the pepper
/// variables too, as if an operator had exported them alongside it, so the
/// seeding and config-source paths are shown to refuse them as well.
fn app_env() -> HashMap<String, String> {
    HashMap::from([
        (
            BOOTSTRAP_ADMIN_EMAIL_KEY.to_string(),
            ADMIN_EMAIL.to_string(),
        ),
        (
            BOOTSTRAP_ADMIN_PASSWORD_KEY.to_string(),
            ADMIN_PASSWORD.to_string(),
        ),
        (PASSWORD_PEPPER_KEY_VAR.to_string(), KEY.to_string()),
        (
            PASSWORD_PEPPER_PREVIOUS_KEYS_VAR.to_string(),
            PREVIOUS_KEY.to_string(),
        ),
        (PASSWORD_PEPPER_REQUIRED_VAR.to_string(), "true".to_string()),
    ])
}

/// Build, boot and start the runtime over `db_path`, as `run()` does.
async fn start(
    db_path: &Path,
    storage_root: &Path,
    peppers: PasswordPeppers,
) -> (Arc<Wafer>, Arc<dyn DatabaseService>) {
    let infra = infra_for(db_path, storage_root);
    let database = impresspress_native::make_database_service(&infra.db_type, &infra.db_path, None)
        .await
        .expect("construct sqlite database service");
    seed_probe_grants(
        &database,
        &[
            PASSWORD_PEPPER_KEY_VAR,
            PASSWORD_PEPPER_PREVIOUS_KEYS_VAR,
            PASSWORD_PEPPER_REQUIRED_VAR,
            CONTROL_KEY,
        ],
    )
    .await;
    let mut wafer = build_native_runtime(
        &infra,
        database.clone(),
        &app_env(),
        peppers,
        false,
        AppHooks::none(),
    )
    .await
    .expect("build impresspress runtime");
    wafer
        .register_block(PROBE, Arc::new(ConfigProbe))
        .expect("register the config probe");
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
    (wafer.bind_all(), database)
}

/// The status a password login answers with.
async fn login(wafer: &Wafer, password: &str) -> u16 {
    let body = serde_json::json!({ "email": ADMIN_EMAIL, "password": password }).to_string();
    let msg = http_codec::build_http_message(
        "POST",
        "/b/auth/api/login",
        "",
        "127.0.0.1",
        [
            ("host", "localhost"),
            ("origin", "http://localhost"),
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/json"),
            ("accept", "application/json"),
        ],
    );
    let parts = http_codec::collect_http_response(
        wafer
            .run("site-main", msg, InputStream::from_bytes(body.into_bytes()))
            .await,
    )
    .await;
    parts.status
}

/// The admin's stored password hash.
async fn stored_hash(database: &Arc<dyn DatabaseService>) -> String {
    let opts = wafer_block::db::ListOptions {
        limit: Some(10),
        skip_count: true,
        ..Default::default()
    };
    let rows = database
        .list(
            impresspress_core::blocks::auth::repo::local_credentials::TABLE,
            &opts,
        )
        .await
        .expect("list local credentials")
        .records;
    assert_eq!(rows.len(), 1, "one bootstrapped admin: {rows:?}");
    rows[0].data["password_hash"]
        .as_str()
        .expect("password_hash is a string")
        .to_string()
}

/// The block that reads config the way any block does, through
/// `wafer_core::clients::config` — a `call_block` into `wafer-run/config`,
/// attributed to this block and checked by WRAP.
const PROBE: &str = "test/config-probe";

/// Meta naming the key the probe reads.
const PROBE_KEY_META: &str = "probe.key";

/// Answers `value:<v>` for a key the config client returns, `absent` for one
/// it does not find, and `error:<code>` for a refusal.
struct ConfigProbe;

#[wafer_block::wafer_async_trait]
impl wafer_run::Block for ConfigProbe {
    fn info(&self) -> wafer_run::BlockInfo {
        wafer_run::BlockInfo::new(
            PROBE,
            "0.0.1",
            "http-handler@v1",
            "reads one config key through the config client",
        )
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn wafer_run::context::Context,
        _event: wafer_run::LifecycleEvent,
    ) -> Result<(), wafer_run::WaferError> {
        Ok(())
    }

    async fn handle(
        &self,
        ctx: &dyn wafer_run::context::Context,
        msg: Message,
        _input: InputStream,
    ) -> wafer_run::OutputStream {
        let key = msg.get_meta(PROBE_KEY_META).to_string();
        let answer = match wafer_core::clients::config::get_optional(ctx, &key).await {
            Ok(Some(value)) => format!("value:{value}"),
            Ok(None) => "absent".to_string(),
            Err(e) => format!("error:{:?}", e.code),
        };
        wafer_run::OutputStream::respond(answer.into_bytes())
    }
}

/// Every key the probe reads is one an admin granted it read access to: the
/// strongest reader short of the admin block itself, which reaches the same
/// `wafer-run/config` block. Without the grants WRAP refuses the read before
/// the config block looks, and a refusal would prove nothing.
async fn seed_probe_grants(database: &Arc<dyn DatabaseService>, keys: &[&str]) {
    impresspress_core::migration_helper::apply_ddl_via_service(
        database,
        impresspress_core::blocks::admin::migrations::ddl_files("sqlite"),
    )
    .await
    .expect("apply admin tables");
    for key in keys {
        let row = impresspress_core::platform_state::wrap_grants::NewWrapGrant {
            grantee: PROBE.to_string(),
            resource: (*key).to_string(),
            write: wafer_block::GrantWrite::None,
            resource_type: "config".to_string(),
            description: String::new(),
        }
        .into_row();
        database
            .create(
                impresspress_core::platform_state::wrap_grants::TABLE,
                row.to_data(),
            )
            .await
            .expect("seed probe grant");
    }
}

/// What the probe's config client reads for `key`.
async fn config_get(wafer: &Wafer, key: &str) -> String {
    let mut msg = Message::new("probe.read");
    msg.set_meta(PROBE_KEY_META, key);
    let parts =
        http_codec::collect_http_response(wafer.run_block(PROBE, msg, InputStream::empty()).await)
            .await;
    String::from_utf8(parts.body).expect("utf-8 answer")
}

/// Events logged on this thread while a [`capture_logs`] guard is held,
/// each rendered with all its fields.
///
/// One process-wide subscriber, installed once, rather than a
/// `set_default` per test: `tracing` caches whether a call site is enabled
/// across threads, so a scoped subscriber in one test can miss events while
/// another test runs without one. The tests run on current-thread runtimes,
/// so the thread the test holds the guard on is the thread its events are
/// logged on.
#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<String>>>);

thread_local! {
    static CAPTURE: std::cell::RefCell<Option<LogCapture>> = const { std::cell::RefCell::new(None) };
}

/// Start capturing this thread's events; capture stops when the guard drops.
fn capture_logs() -> (LogCapture, CaptureGuard) {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        tracing::subscriber::set_global_default(ThreadCapture)
            .expect("no other global subscriber in this test binary");
    });
    let logs = LogCapture::default();
    CAPTURE.with(|c| *c.borrow_mut() = Some(logs.clone()));
    (logs, CaptureGuard)
}

struct CaptureGuard;

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CAPTURE.with(|c| *c.borrow_mut() = None);
    }
}

struct FieldVisitor<'a>(&'a mut String);

impl tracing::field::Visit for FieldVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = write!(self.0, "{}={value:?} ", field.name());
    }
}

struct ThreadCapture;

impl tracing::Subscriber for ThreadCapture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        CAPTURE.with(|c| {
            if let Some(logs) = c.borrow().as_ref() {
                let mut line = String::new();
                event.record(&mut FieldVisitor(&mut line));
                logs.0.lock().expect("log capture poisoned").push(line);
            }
        });
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

/// With a required pepper the real crypto service hashes with it: the
/// bootstrapped admin's stored hash is peppered with the current key, the
/// admin signs in with it, and the wrong password is still refused. No
/// config read can return a pepper variable — not the config block a block's
/// client reaches, not the variables table — even though the variables were
/// also exported as app config; and the boot log names the key by its id
/// only.
#[tokio::test]
async fn a_required_pepper_hashes_passwords_and_is_readable_nowhere() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("password_pepper.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    let peppers = password_peppers_from_env(process_env(&[
        (PASSWORD_PEPPER_KEY_VAR, KEY),
        (PASSWORD_PEPPER_PREVIOUS_KEYS_VAR, PREVIOUS_KEY),
        (PASSWORD_PEPPER_REQUIRED_VAR, "true"),
    ]))
    .expect("a well-formed pepper");
    let key_id = peppers.current().expect("a current key").id().to_string();

    let (logs, guard) = capture_logs();
    let (wafer, database) = start(&db_path, &storage_root, peppers).await;
    drop(guard);

    let hash = stored_hash(&database).await;
    assert!(
        hash.starts_with("$argon2id-hmac-sha256$") && hash.contains(&format!(",pepper={key_id}$")),
        "the bootstrapped admin's hash must be peppered with the current key: {hash}"
    );
    assert_eq!(login(&wafer, ADMIN_PASSWORD).await, 200);
    assert_eq!(login(&wafer, "not-the-password").await, 401);

    // The config block answers a key on the boot map (the control), and
    // none of the pepper variables.
    assert_eq!(
        config_get(&wafer, CONTROL_KEY).await,
        "value:1",
        "the control key must be readable, or the probe proves nothing"
    );
    for var in PEPPER_VARS {
        assert_eq!(
            config_get(&wafer, var).await,
            "absent",
            "{var} must not be readable through the config client"
        );
        assert!(
            impresspress_core::platform_state::variables::find_by_key(&database, var)
                .await
                .expect("read the variables table")
                .is_none(),
            "{var} must never be written to the variables table"
        );
    }

    let logs = logs.0.lock().expect("log capture poisoned").clone();
    assert!(
        logs.iter().any(|line| line.contains(&key_id)),
        "boot must log which pepper key is in use, by id: {logs:#?}"
    );
    for line in &logs {
        assert!(
            !line.contains(KEY) && !line.contains(PREVIOUS_KEY),
            "a pepper key reached the log: {line}"
        );
    }
}

/// A pepper switched on over existing accounts locks nobody out while it is
/// optional: a hash written before it (unpeppered) still verifies. Once it
/// is required, that same hash is refused.
#[tokio::test]
async fn an_unpeppered_hash_verifies_until_the_pepper_is_required() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("password_pepper_legacy.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    // First boot without a pepper: the admin's hash is plain argon2id.
    let (wafer, database) = start(&db_path, &storage_root, PasswordPeppers::default()).await;
    let legacy = stored_hash(&database).await;
    assert!(legacy.starts_with("$argon2id$"), "{legacy}");
    drop(wafer);

    let optional = password_peppers_from_env(process_env(&[(PASSWORD_PEPPER_KEY_VAR, KEY)]))
        .expect("a well-formed pepper");
    let (wafer, _) = start(&db_path, &storage_root, optional).await;
    assert_eq!(
        login(&wafer, ADMIN_PASSWORD).await,
        200,
        "an unpeppered hash must keep verifying while the pepper is optional"
    );
    drop(wafer);

    let required = password_peppers_from_env(process_env(&[
        (PASSWORD_PEPPER_KEY_VAR, KEY),
        (PASSWORD_PEPPER_REQUIRED_VAR, "true"),
    ]))
    .expect("a well-formed pepper");
    let (wafer, _) = start(&db_path, &storage_root, required).await;
    assert_eq!(
        login(&wafer, ADMIN_PASSWORD).await,
        503,
        "a required pepper refuses an unpeppered hash with the same 503 as any \
         check that could not decide, never as a wrong password"
    );
}

/// A peppered hash whose key the deployment no longer holds cannot be
/// checked: the login is a 503 (not a wrong password, which would send the
/// user to reset a password that is right), and the operator log names the
/// account and says it is a configuration fault.
#[tokio::test]
async fn a_peppered_hash_whose_key_is_gone_is_a_503_logged_as_a_config_fault() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("password_pepper_key_gone.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    let with_key = password_peppers_from_env(process_env(&[(PASSWORD_PEPPER_KEY_VAR, KEY)]))
        .expect("a well-formed pepper");
    let (wafer, database) = start(&db_path, &storage_root, with_key).await;
    assert!(stored_hash(&database)
        .await
        .starts_with("$argon2id-hmac-sha256$"));
    drop(wafer);

    // The key was replaced without being kept as a previous key.
    let other_key =
        password_peppers_from_env(process_env(&[(PASSWORD_PEPPER_KEY_VAR, PREVIOUS_KEY)]))
            .expect("a well-formed pepper");
    let (wafer, _) = start(&db_path, &storage_root, other_key).await;
    let (logs, guard) = capture_logs();
    let status = login(&wafer, ADMIN_PASSWORD).await;
    drop(guard);
    assert_eq!(status, 503);

    let logs = logs.0.lock().expect("log capture poisoned").clone();
    assert!(
        logs.iter()
            .any(|line| line.contains("user_id=") && line.contains("configuration fault")),
        "the pepper fault must be logged with the account and as a config fault: {logs:#?}"
    );
}

/// A value that does not parse fails the boot, naming the variable, and a
/// malformed key is never echoed.
#[test]
fn a_malformed_pepper_fails_the_boot_naming_the_variable() {
    for required in ["1", "yes", "TRUE", ""] {
        let err = password_peppers_from_env(process_env(&[
            (PASSWORD_PEPPER_KEY_VAR, KEY),
            (PASSWORD_PEPPER_REQUIRED_VAR, required),
        ]))
        .expect_err(required);
        assert!(
            err.to_string().contains(PASSWORD_PEPPER_REQUIRED_VAR),
            "{required:?}: {err}"
        );
    }

    let short = "c2hvcnQtcGVwcGVy"; // "short-pepper"
    let err = password_peppers_from_env(process_env(&[(PASSWORD_PEPPER_KEY_VAR, short)]))
        .expect_err("a short key");
    let shown = format!("{err:#}");
    assert!(shown.contains(PASSWORD_PEPPER_KEY_VAR), "{shown}");
    assert!(!shown.contains(short), "the error echoed the key: {shown}");

    let err = password_peppers_from_env(process_env(&[(PASSWORD_PEPPER_REQUIRED_VAR, "true")]))
        .expect_err("required without a key");
    assert!(
        err.to_string().contains(PASSWORD_PEPPER_REQUIRED_VAR),
        "{err}"
    );
}

/// `Debug` of the parsed pepper — what a `{:?}` of boot state would print —
/// shows key ids, never keys.
#[test]
fn the_parsed_pepper_debug_shows_no_key() {
    let peppers = password_peppers_from_env(process_env(&[
        (PASSWORD_PEPPER_KEY_VAR, KEY),
        (PASSWORD_PEPPER_PREVIOUS_KEYS_VAR, PREVIOUS_KEY),
    ]))
    .expect("a well-formed pepper");
    for shown in [format!("{peppers:?}"), password_pepper::describe(&peppers)] {
        assert!(
            !shown.contains(KEY) && !shown.contains(PREVIOUS_KEY),
            "{shown}"
        );
    }
}
