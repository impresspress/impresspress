//! The boot funnel as a consumer drives it, on real sql.js under
//! `wasm-pack test --node` (the in-memory OPFS of
//! `database::test_support`): what [`super::BrowserServices::builder`]
//! configures, that an [`AppSeed`] reaches the blocks still to initialize,
//! and that what a consumer changes between `build` and `boot` is the
//! runtime `boot` seals.

use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use wafer_block::{
    core_types::{LifecycleEvent, WaferError},
    http_codec,
};
use wafer_core::interfaces::database::service::DatabaseService;
use wafer_run::{Block, BlockInfo, ConfigSource, Context, InputStream, Message, OutputStream};
use wasm_bindgen_test::wasm_bindgen_test;

use super::{boot, infrastructure_config, AppSeed, BrowserServices};

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

    let builder = services.builder().await.expect("a builder");
    let (mut wafer, hooks) = services.build(builder).expect("the runtime builds");
    let hooks = hooks.with_app_seed(MarkerSeed {
        calls: Rc::clone(&calls),
    });
    boot(&mut wafer, &hooks).await.expect("the runtime boots");

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
/// and still gets the shared seeding — the auto-generated JWT secret, on both
/// holders of it.
#[wasm_bindgen_test]
async fn a_runtime_without_an_app_seed_boots_with_the_shared_seeds() {
    crate::database::test_support::fresh_db().await;
    let services = BrowserServices::new(Vec::new()).expect("the browser services construct");

    let builder = services.builder().await.expect("a builder");
    let (mut wafer, hooks) = services.build(builder).expect("the runtime builds");
    let jwt_secret = Arc::clone(&hooks.jwt_secret_handle);
    boot(&mut wafer, &hooks).await.expect("the runtime boots");

    assert_eq!(resolved_for_a_declaring_block(&services).await, None);
    assert!(
        !jwt_secret.read().expect("jwt lock").is_empty(),
        "the shared seeding generated the JWT secret and rotated the router's copy",
    );
}

/// The route the consumer adds between `build` and `boot`.
const WRAPPED_PATH: &str = "/wrapped-by-the-consumer";
const WRAPPED_BODY: &[u8] = b"answered-by-the-consumer-block";

/// A consumer block, reached only through the route the consumer added to
/// the outer router in the gap.
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

/// **The build→boot gap is the consumer's.** A consumer that replaces the
/// outer `wafer-run/router`'s route table on the built runtime — as
/// gizza-site does to serve its own pages outside `/b/**` — gets that table
/// in the runtime `boot` seals: the request reaches the consumer's block,
/// not `wafer-run/web`, which `site_main::default_routes` sends every
/// unclaimed path to.
#[wasm_bindgen_test]
async fn a_router_wrapped_between_build_and_boot_takes_effect() {
    crate::database::test_support::fresh_db().await;
    let services = BrowserServices::new(Vec::new()).expect("the browser services construct");
    let reached = Arc::new(AtomicUsize::new(0));

    let builder = services.builder().await.expect("a builder").extra_block(
        "test/consumer",
        Arc::new(ConsumerBlock {
            reached: Arc::clone(&reached),
        }),
    );
    let (mut wafer, hooks) = services.build(builder).expect("the runtime builds");

    // The gap: the consumer's route ahead of the shipped table.
    let mut routes = vec![serde_json::json!({ "path": WRAPPED_PATH, "block": "test/consumer" })];
    routes.extend(
        impresspress_core::flows::site_main::default_routes()
            .as_array()
            .expect("the shipped table is a list")
            .iter()
            .cloned(),
    );
    wafer.add_block_config("wafer-run/router", serde_json::json!({ "routes": routes }));

    boot(&mut wafer, &hooks).await.expect("the runtime boots");

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
