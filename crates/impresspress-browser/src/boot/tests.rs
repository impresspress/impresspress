//! The boot funnel as a consumer drives it, on real sql.js under
//! `wasm-pack test --node` (the in-memory OPFS of
//! `database::test_support`): what [`super::BrowserServices::builder`]
//! configures, that an [`AppSeed`] reaches the blocks still to initialize,
//! that both holders of the JWT secret agree after a boot, and that a final
//! override the consumer declares on the funnel's builder is the runtime
//! [`super::BrowserServices::boot`] serves.

use std::{
    cell::Cell,
    collections::BTreeMap,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use wafer_block::{
    core_types::{LifecycleEvent, WaferError},
    http_codec,
};
use wafer_core::interfaces::{crypto::service::CryptoService, database::service::DatabaseService};
use wafer_run::{Block, BlockInfo, ConfigSource, Context, InputStream, Message, OutputStream};
use wasm_bindgen_test::wasm_bindgen_test;

use super::{infrastructure_config, AppSeed, BrowserServices};

/// Browser builds log server errors only, on both config surfaces — the
/// pipeline reads the policy off the synchronous snapshot.
#[wasm_bindgen_test]
fn a_browser_build_logs_server_errors_only() {
    let config = infrastructure_config(String::new());
    let key = impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY;
    assert_eq!(config.service_get(key), Some("errors"));
    assert!(config.snapshot_contains(key));
}

/// A key no block declares a default for, so only the app's seed can put a
/// value in it.
const SEEDED_KEY: &str = "TEST__APP__SEEDED_MARKER";
const SEEDED_VALUE: &str = "seeded-by-the-app";

/// The app seed under test: writes [`SEEDED_KEY`] and counts its calls.
struct MarkerSeed {
    calls: Rc<Cell<u32>>,
}

#[wafer_block::wafer_async_trait]
impl AppSeed for MarkerSeed {
    async fn seed(&self, db: &Arc<dyn DatabaseService>) -> Result<(), String> {
        self.calls.set(self.calls.get() + 1);
        impresspress_core::platform_state::variables::seed_if_absent(
            db,
            SEEDED_KEY,
            SEEDED_VALUE,
            "Seeded marker",
            "written by the app's seed hook",
            false,
        )
        .await
        .map(|_| ())
    }
}

/// What the runtime's config source resolves [`SEEDED_KEY`] to for a block
/// that declares it — the value that block's `Init` would be handed. `None`
/// is the source refusing: the key is required (no default), so a block
/// declaring it would fail its `Init` permanently.
async fn resolved_for_a_declaring_block(services: &BrowserServices) -> Option<String> {
    let declared = [wafer_block::ConfigVar::new(SEEDED_KEY, "a seeded key", "")];
    let config = services
        .config_source
        .load_for_block("test/declares-the-key", &declared)
        .await
        .ok()?;
    config.get(SEEDED_KEY).map(str::to_string)
}

/// **The app seed runs once per boot, after admin's migration and before the
/// variables are loaded back**, so what it writes is in the map published to
/// the blocks still to initialize. Before the seed hook was the funnel's, an
/// app that seeded did it in its own copy of the funnel.
#[wasm_bindgen_test]
async fn an_app_seed_reaches_the_blocks_still_to_initialize() {
    let db: Arc<dyn DatabaseService> = Arc::new(crate::database::test_support::fresh_db().await);
    let services = BrowserServices::new(Vec::new()).expect("the browser services construct");
    let calls = Rc::new(Cell::new(0));

    let seed = MarkerSeed {
        calls: Rc::clone(&calls),
    };
    let builder = services.builder().await.expect("a builder");
    services
        .boot(builder, Some(&seed))
        .await
        .expect("the runtime boots");

    assert_eq!(calls.get(), 1, "the funnel runs the app seed exactly once");
    // Through the config source rather than the table: a seed that ran
    // AFTER the load would leave the row and miss the published map.
    assert_eq!(
        resolved_for_a_declaring_block(&services).await.as_deref(),
        Some(SEEDED_VALUE),
        "the seeded value is what a declaring block resolves at its Init",
    );
    let stored = impresspress_core::platform_state::variables::load_all(&db)
        .await
        .expect("the variables load");
    assert_eq!(
        stored.get(SEEDED_KEY).map(String::as_str),
        Some(SEEDED_VALUE)
    );
}

/// **The seed is optional.** A runtime with nothing of its own to seed boots,
/// and still gets the shared seeding — the auto-generated JWT secret, rotated
/// onto BOTH of its holders: the crypto service that signs tokens and the
/// router's handle the pipeline verifies against. A token the first signs
/// must verify under the second's secret; when only one of them was rotated,
/// every authenticated request 403'd after a successful login.
#[wasm_bindgen_test]
async fn a_runtime_without_an_app_seed_boots_with_the_shared_seeds() {
    crate::database::test_support::fresh_db().await;
    let services = BrowserServices::new(Vec::new()).expect("the browser services construct");

    let builder = services.builder().await.expect("a builder");
    let router_secret = builder.jwt_secret_handle();
    services
        .boot(builder, None)
        .await
        .expect("the runtime boots");

    assert_eq!(resolved_for_a_declaring_block(&services).await, None);
    let router_secret = router_secret.read().expect("jwt lock").clone();
    assert!(
        !router_secret.is_empty(),
        "the shared seeding generated the JWT secret and rotated the router's copy",
    );
    let mut claims = BTreeMap::new();
    claims.insert("sub".to_string(), serde_json::json!("someone"));
    let token = services
        .crypto
        .sign_for("wafer-run/auth", claims, Duration::from_secs(60))
        .await
        .expect("the crypto service signs");
    let verified = crate::crypto::BrowserCryptoService::new(router_secret)
        .verify_for("wafer-run/auth", &token)
        .await
        .expect("a token the crypto service signed verifies under the router's secret");
    assert_eq!(verified.get("sub"), Some(&serde_json::json!("someone")));
}

/// The route the consumer declares ahead of the shipped table.
const WRAPPED_PATH: &str = "/wrapped-by-the-consumer";
const WRAPPED_BODY: &[u8] = b"answered-by-the-consumer-block";

/// A consumer block, reached only through the route the consumer declared on
/// the outer router.
struct ConsumerBlock {
    reached: Arc<AtomicUsize>,
}

#[wafer_block::wafer_async_trait]
impl Block for ConsumerBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/consumer",
            "0.1.0",
            "test/consumer@v1",
            "consumer page",
        )
    }
    async fn handle(&self, _c: &dyn Context, _m: Message, _i: InputStream) -> OutputStream {
        self.reached.fetch_add(1, Ordering::SeqCst);
        OutputStream::respond(WRAPPED_BODY.to_vec())
    }
    async fn lifecycle(&self, _c: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
}

/// **A final router override declared on the funnel's builder takes
/// effect.** A consumer that replaces the outer `wafer-run/router`'s route
/// table with `final_block_config` — as gizza-site does to serve its own
/// pages outside `/b/**` — gets that table in the runtime the funnel boots:
/// the request reaches the consumer's block, not `wafer-run/web`, which
/// `site_main::default_routes` sends every unclaimed path to. Declared, not
/// patched onto a built runtime, so prepared-plan export and parity checks
/// see it too.
#[wasm_bindgen_test]
async fn a_final_router_override_declared_on_the_builder_takes_effect() {
    crate::database::test_support::fresh_db().await;
    let services = BrowserServices::new(Vec::new()).expect("the browser services construct");
    let reached = Arc::new(AtomicUsize::new(0));

    // The consumer's route ahead of the shipped table.
    let mut routes = vec![serde_json::json!({ "path": WRAPPED_PATH, "block": "test/consumer" })];
    routes.extend(
        impresspress_core::flows::site_main::default_routes()
            .as_array()
            .expect("the shipped table is a list")
            .iter()
            .cloned(),
    );
    let builder = services
        .builder()
        .await
        .expect("a builder")
        .extra_block(
            "test/consumer",
            Arc::new(ConsumerBlock {
                reached: Arc::clone(&reached),
            }),
        )
        .final_block_config("wafer-run/router", serde_json::json!({ "routes": routes }));
    let wafer = services
        .boot(builder, None)
        .await
        .expect("the runtime boots");

    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "retrieve");
    msg.set_meta("req.resource", WRAPPED_PATH);
    let parts =
        http_codec::collect_http_response(wafer.run("site-main", msg, InputStream::empty()).await)
            .await;

    assert_eq!(
        reached.load(Ordering::SeqCst),
        1,
        "the consumer's route reached its block (status {}, body {:?})",
        parts.status,
        String::from_utf8_lossy(&parts.body),
    );
    assert_eq!(parts.body, WRAPPED_BODY);
}

/// A runtime the funnel booted, with its `wafer-run/auth` block, through
/// [`BrowserServices::boot`] on a fresh database.
async fn booted_runtime() -> wafer_run::Wafer {
    crate::database::test_support::fresh_db().await;
    let services = BrowserServices::new(Vec::new()).expect("the browser services construct");
    let builder = services.builder().await.expect("a builder");
    services
        .boot(builder, None)
        .await
        .expect("the runtime boots")
}

/// A `Weak` on `wafer`'s registered `wafer-run/auth` block. The runtime's
/// context owns every registered block, so anything that keeps a context of
/// this runtime alive keeps that block alive too.
fn auth_block_probe(wafer: &wafer_run::Wafer) -> std::sync::Weak<dyn Block> {
    Arc::downgrade(
        &wafer
            .lookup_block("wafer-run/auth")
            .expect("the auth block is registered")
            .1,
    )
}

/// **A booted runtime is freed when it is dropped.** Until 2026-10
/// `AuthServiceImpl::init` kept its `Init` context in the service the auth
/// block owns: a reference cycle, and no runtime was ever freed. This is the
/// wasm half of `impresspress-server`'s `runtime_drop` test, over the boot
/// funnel the browser shares with native, on the wasm32 build. It runs with
/// this crate's features, which leave core's feature blocks off, so it covers
/// auth and core on wasm32; the native test covers the full block set.
#[wasm_bindgen_test]
async fn a_booted_runtime_is_freed_when_dropped() {
    let wafer = booted_runtime().await;
    let probe = auth_block_probe(&wafer);
    drop(wafer);
    assert!(
        probe.upgrade().is_none(),
        "the auth block outlived its runtime: something keeps a context of it"
    );
}

/// **A runtime swapped out by an activation is freed once its handle goes.**
/// Every dev-sandbox activation `replace_wafer`s, and `impresspress-web`'s
/// `RetainedRuntime` keeps the displaced runtime for one rollback only. When
/// that last handle is dropped the displaced runtime, auth block included,
/// must go with it, or each activation leaks the runtime it replaced.
#[wasm_bindgen_test]
async fn a_runtime_replace_wafer_displaced_is_freed_with_its_last_handle() {
    let first = booted_runtime().await;
    let probe = auth_block_probe(&first);
    // Other tests in this binary may have installed a runtime already.
    if crate::is_initialized() {
        drop(crate::replace_wafer(first).expect("a runtime is installed"));
    } else {
        crate::store_wafer(first).expect("the first runtime installs");
    }

    let second = booted_runtime().await;
    let displaced = crate::replace_wafer(second).expect("a runtime is installed");
    assert!(
        probe.upgrade().is_some(),
        "the displaced runtime lives while its handle does"
    );
    drop(displaced);
    assert!(
        probe.upgrade().is_none(),
        "the displaced runtime's auth block outlived its last handle"
    );
}

/// What every block's pre-build migration pass ([`super::apply_block_migrations`])
/// does with a database file from before the per-file ledger, on real sql.js.
mod pre_ledger_migrations {
    use std::sync::Arc;

    use impresspress_core::{
        blocks::admin::{
            migrations::{BLOCK_SETTINGS_CLEAR_BLESSED_HASH, MIGRATIONS},
            ADMIN_BLOCK_ID,
        },
        migration_helper::{legacy_runner, migration_set_hash, MigrationFile},
        platform_state::{
            applied_migrations::{self, Origin},
            block_settings::{self, BlockSettingsPatch},
        },
    };
    use wafer_core::interfaces::database::service::DatabaseService;
    use wasm_bindgen::JsValue;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::super::{apply_block_migrations, MIGRATION_REFUSED};
    use crate::database::test_support::fresh_db;

    /// Admin 009 as it first shipped (2026-10-15, `56ab1250`): `ADD COLUMN
    /// blessed_hash` then an `UPDATE` emptying it. A byte-exact copy of the
    /// text main carried, pinned by the history table's fixture.
    const FIRST_009: &str = include_str!(
        "../../../impresspress-core/src/blocks/admin/migrations/history/009_block_settings_clear_blessed_hash.3863a42e.sqlite.sql"
    );

    /// The history entry first seen at `commit`, for admin's SQLite list.
    fn admin_state(commit: &str) -> &'static str {
        impresspress_core::migration_history::admin::SQLITE
            .iter()
            .find(|state| state.first_seen.ends_with(commit))
            .expect("an admin state")
            .list_hash
    }

    async fn record(db: &Arc<dyn DatabaseService>, block: &str, hash: &str) {
        block_settings::upsert_fields_via_service(
            db,
            block,
            BlockSettingsPatch {
                current_hash: Some(hash.to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("record the list as the old runner did");
    }

    fn name_of(error: &JsValue) -> Option<String> {
        js_sys::Reflect::get(error, &"name".into())
            .ok()
            .and_then(|name| name.as_string())
    }

    /// **A database file the old runner left on main's 2026-10-15 list
    /// (`0f4bad57`) is adopted, and keeps its rows.** Built with the runner
    /// from before the ledger, from the texts that list carried (today's
    /// 001-008 and the first 009), and recorded as that runner recorded it.
    /// The pass adopts 001-008 from that record, runs today's 009 — the
    /// history marks the first 009 as not applied — and drops the column,
    /// and a grant the database held survives.
    #[wasm_bindgen_test]
    async fn a_pre_ledger_database_file_is_adopted_and_keeps_its_rows() {
        let db: Arc<dyn DatabaseService> = Arc::new(fresh_db().await);
        let mut files: Vec<MigrationFile<'static>> = MIGRATIONS.sqlite.to_vec();
        let last = files.pop().expect("009");
        assert_eq!(last.0, BLOCK_SETTINGS_CLEAR_BLESSED_HASH);
        files.push((BLOCK_SETTINGS_CLEAR_BLESSED_HASH, FIRST_009));
        let state = admin_state("56ab1250");
        assert_eq!(
            migration_set_hash(&files),
            state,
            "the file is the 0f4bad57 list"
        );
        legacy_runner::apply(&db, &files)
            .await
            .expect("the old runner applies the list");
        record(&db, ADMIN_BLOCK_ID, state).await;
        let mut grant = std::collections::HashMap::new();
        for (column, value) in [
            ("id", "ur_kept"),
            ("user_id", "u1"),
            ("role", "admin"),
            ("assigned_at", "2026-10-16T00:00:00Z"),
            ("assigned_by", ""),
            ("created_at", "2026-10-16T00:00:00Z"),
            ("updated_at", "2026-10-16T00:00:00Z"),
        ] {
            grant.insert(column.to_string(), serde_json::json!(value));
        }
        db.create(impresspress_core::platform_state::user_roles::TABLE, grant)
            .await
            .expect("a grant the database held");

        apply_block_migrations(&db)
            .await
            .unwrap_or_else(|e| panic!("the pass adopts the file: {e:?}"));

        let ledger = applied_migrations::list_for_block_via_service(&db, ADMIN_BLOCK_ID)
            .await
            .expect("read the ledger");
        let mut rows: Vec<(String, Origin, String)> = ledger
            .into_iter()
            .map(|row| (row.file_name, row.origin, row.adopted_from))
            .collect();
        rows.sort();
        let expected: Vec<(String, Origin, String)> = MIGRATIONS
            .sqlite
            .iter()
            .map(|(name, _)| {
                if *name == BLOCK_SETTINGS_CLEAR_BLESSED_HASH {
                    (name.to_string(), Origin::Ran, String::new())
                } else {
                    (name.to_string(), Origin::Adopted, state.to_string())
                }
            })
            .collect();
        assert_eq!(rows, expected);
        let columns = db
            .schema_columns(block_settings::TABLE)
            .await
            .expect("introspect block_settings");
        assert!(!columns.iter().any(|c| c == "blessed_hash"), "{columns:?}");
        db.get(
            impresspress_core::platform_state::user_roles::TABLE,
            "ur_kept",
        )
        .await
        .expect("the grant survives");
    }

    /// **A database file from before 2026-07-13 is refused by name, and
    /// nothing is erased.** Its admin record is a list the history table
    /// marks unsupported (its tables were `solobase__*`), so the pass rejects
    /// with an `Error` named [`MIGRATION_REFUSED`] — the name `sw.js` keys its
    /// non-erasing `migration` stage on — and runs and records nothing.
    #[wasm_bindgen_test]
    async fn a_database_file_from_before_2026_07_13_is_refused_by_name() {
        let db: Arc<dyn DatabaseService> = Arc::new(fresh_db().await);
        // Admin's tables, so the record has a row to live in; the refusal
        // depends on the record alone.
        legacy_runner::apply(&db, &MIGRATIONS.sqlite[..1])
            .await
            .expect("admin's 001");
        let state = admin_state("f5f73c25");
        record(&db, ADMIN_BLOCK_ID, state).await;

        let error = apply_block_migrations(&db)
            .await
            .expect_err("the pass refuses the record");
        assert_eq!(
            name_of(&error).as_deref(),
            Some(MIGRATION_REFUSED),
            "{error:?}"
        );
        let message = js_sys::Reflect::get(&error, &"message".into())
            .ok()
            .and_then(|m| m.as_string())
            .unwrap_or_default();
        assert!(message.contains("cannot be upgraded"), "{message}");
        assert!(message.contains(&state[..8]), "{message}");
        assert!(
            applied_migrations::list_for_block_via_service(&db, ADMIN_BLOCK_ID)
                .await
                .unwrap_or_default()
                .is_empty(),
            "nothing recorded"
        );
        let settings = block_settings::load(&db).await.expect("read the record");
        assert_eq!(
            settings.state(ADMIN_BLOCK_ID).migration.current_hash,
            state,
            "the record stayed"
        );
    }

    /// **A file that fails on the data is not a refusal.** The pass rejects
    /// with a plain error — `sw.js`'s `initialize` stage, the one stage a
    /// build rendered to erase on recovery may erase for — and the failed
    /// file is not recorded. Here admin's 001 meets a `variables` table some
    /// other code created without the column its index names.
    #[wasm_bindgen_test]
    async fn a_file_that_fails_on_the_data_is_an_ordinary_error() {
        let db: Arc<dyn DatabaseService> = Arc::new(fresh_db().await);
        // Test-fixture setup: a table admin's 001 cannot index.
        db.exec_raw(
            "CREATE TABLE impresspress__admin__variables (other TEXT)",
            &[],
        )
        .await
        .expect("a conflicting table");

        let error = apply_block_migrations(&db)
            .await
            .expect_err("admin's 001 fails");
        assert_ne!(
            name_of(&error).as_deref(),
            Some(MIGRATION_REFUSED),
            "{error:?}"
        );
        let message = error.as_string().unwrap_or_default();
        assert!(message.contains("001_admin_schema"), "{message}");
        assert!(
            applied_migrations::list_for_block_via_service(&db, ADMIN_BLOCK_ID)
                .await
                .unwrap_or_default()
                .is_empty(),
            "the failed file is not recorded"
        );
    }
}
