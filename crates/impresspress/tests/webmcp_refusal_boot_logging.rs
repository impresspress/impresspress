//! Proves the second half of the WebMCP refusal-logging fix: refusals are
//! still reported somewhere an operator will see them, even though the
//! per-request `GET /b/webmcp/manifest.json` path (covered by
//! `impresspress_core::pipeline::discovery_tests::webmcp_manifest_request_does_not_log_refusals`)
//! no longer logs them.
//!
//! The chosen hook is `ImpresspressBuilder::build()`
//! (`impresspress-core/src/builder/registration.rs`): right after
//! `wafer.block_infos()` collects every registered block's `BlockInfo` (the
//! same snapshot the router — and therefore the manifest endpoint — is built
//! from), `build()` now runs `wafer_core::discovery::generate_webmcp_report`
//! once and logs each refusal at `warn!`. `build()` runs once per `Wafer`
//! construction; mirroring `impresspress_server::build_native_runtime`'s native
//! boot path here exercises the exact same call.

use std::{path::Path, sync::Arc};

use impresspress_core::builder::ImpresspressBuilder;
use wafer_run::{
    context::Context, AuthLevel, Block, BlockEndpoint, BlockInfo, InputStream, LifecycleEvent,
    Message, OutputStream, WaferError,
};

/// Two endpoints opted into the SAME WebMCP tool name — a structural defect
/// (`WebMcpRefusal::DuplicateToolName`) `BlockInfo::validate` does not catch
/// at registration (it only checks name *syntax*, not cross-endpoint
/// uniqueness — that dedup pass is `generate_webmcp_report`'s job). Never
/// dispatched to in this test; only `.info()` is exercised.
struct WebmcpRefusalFixtureBlock;

#[wafer_block::wafer_async_trait]
impl Block for WebmcpRefusalFixtureBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/webmcp-refusal-fixture",
            "0.0.1",
            "http-handler@v1",
            "two endpoints sharing one tool name, on purpose",
        )
        .endpoints(vec![
            BlockEndpoint::get("/x/webmcp-refusal-fixture/one")
                .summary("first")
                .auth(AuthLevel::Public)
                .agent_tool("webmcp_refusal_fixture_dup", "first"),
            BlockEndpoint::get("/x/webmcp-refusal-fixture/two")
                .summary("second")
                .auth(AuthLevel::Public)
                .agent_tool("webmcp_refusal_fixture_dup", "second"),
        ])
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(())
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

/// Minimal `tracing::Subscriber` that records every field of every event —
/// not just the rendered `message` — so this test can assert on exactly
/// what `build()` logged, including the `scope` field this fix adds,
/// without pulling in `tracing-subscriber`. Mirrors
/// `impresspress_core::pipeline::discovery_tests::MessageCapture` — kept
/// local (integration test binaries can't share code across `tests/*.rs`
/// files without a `tests/common/` module this crate doesn't otherwise
/// have). That sibling copy only needs the `message` field (it counts
/// occurrences, it does not inspect structured fields), so it is left as-is.
#[derive(Clone, Default)]
struct MessageCapture(Arc<std::sync::Mutex<Vec<String>>>);

struct MessageVisitor<'a> {
    out: &'a mut String,
}

impl tracing::field::Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        // Every field lands in the same string, space-separated, in
        // whatever order `tracing` visits them — good enough for substring
        // assertions like `scope=outputSchema` below, without committing to
        // an exact rendering of the whole event.
        let _ = write!(self.out, "{}={value:?} ", field.name());
    }
}

impl tracing::Subscriber for MessageCapture {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut message = String::new();
        event.record(&mut MessageVisitor { out: &mut message });
        self.0
            .lock()
            .expect("MessageCapture mutex poisoned")
            .push(message);
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

impl MessageCapture {
    fn messages_containing(&self, needle: &str) -> Vec<String> {
        self.0
            .lock()
            .expect("MessageCapture mutex poisoned")
            .iter()
            .filter(|m| m.contains(needle))
            .cloned()
            .collect()
    }
}

const REFUSAL_WARNING: &str = "webmcp: endpoint opted in to agent-tool exposure but was refused";

/// Build one `ImpresspressBuilder` runtime over a scratch sqlite file +
/// local storage root — the same construction native boot uses
/// (`impresspress_server::build_native_runtime`, minus the admin-table
/// pre-seeding and HTTP-listener steps `build()` itself doesn't need: it is
/// a synchronous, no-I/O block-registration method). Every block in `extras`
/// is registered before `block_infos` is captured, so all of them
/// participate in the same snapshot the router — and now the boot-time
/// refusal log — is built from. Takes a list (rather than one block) so a
/// test can combine fixtures — e.g. a `DuplicateToolName` block alongside an
/// `OutputSchemaNotAnObject` one — and see both kinds of refusal, with their
/// different `scope`, in one boot pass.
async fn build_runtime_with_extra_blocks(
    db_path: &Path,
    storage_root: &Path,
    extras: Vec<(&str, Arc<dyn Block>)>,
) -> wafer_run::Wafer {
    let db_path_str = db_path.to_str().expect("db path is valid utf-8");
    let database = impresspress_native::make_database_service("sqlite", db_path_str, None)
        .await
        .expect("construct sqlite database service");

    let storage_root_str = storage_root.to_str().expect("storage root is valid utf-8");
    let storage = impresspress_native::make_storage_service("local", storage_root_str)
        .await
        .expect("construct local storage service");

    // `RuntimeConfig::install` is the only way to hand a `ConfigService` to the
    // builder, so a caller cannot fill the async surface and forget the
    // synchronous snapshot. This harness needs neither key: it is testing what
    // `build()` logs, not what blocks read.
    let (builder, ()) = impresspress_core::builder::RuntimeConfig::new().install(
        ImpresspressBuilder::new()
            .database(database)
            .storage(storage),
        |map| {
            (
                impresspress_core::builder::fill_config_service(
                    Arc::new(wafer_core::service_blocks::config::EnvConfigService::new()),
                    map,
                ),
                (),
            )
        },
    );
    let mut builder = builder
        .crypto(
            impresspress_native::make_jwt_crypto_service(
                "webmcp-refusal-boot-logging-test-jwt-secret".to_string(),
                Default::default(),
            )
            .expect("jwt crypto service"),
        )
        .network(impresspress_native::make_fetch_network_service())
        .logger(impresspress_native::make_tracing_logger());
    for (name, block) in extras {
        builder = builder.extra_block(name, block);
    }

    builder.build().expect("build impresspress runtime")
}

/// The hook this fix relies on: `ImpresspressBuilder::build()` must compute
/// and log WebMCP refusals exactly once per construction, from the full
/// registered-block set, identifying the offending block/method/path/tool.
#[tokio::test]
async fn webmcp_refusals_are_logged_once_at_runtime_construction() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp.path().join("webmcp_refusal_boot_logging_test.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    let capture = MessageCapture::default();
    let guard = tracing::subscriber::set_default(capture.clone());
    let wafer = build_runtime_with_extra_blocks(
        &db_path,
        &storage_root,
        vec![(
            "test/webmcp-refusal-fixture",
            Arc::new(WebmcpRefusalFixtureBlock) as Arc<dyn Block>,
        )],
    )
    .await;
    drop(guard);
    drop(wafer);

    let refusal_logs = capture.messages_containing(REFUSAL_WARNING);
    assert_eq!(
        refusal_logs.len(),
        2,
        "build() must log exactly one warning per refused endpoint, once, at construction \
         (both fixture endpoints refused as DuplicateToolName): {refusal_logs:?}"
    );
}

/// An endpoint that opts in to agent-tool exposure and declares a response
/// schema that does not describe a JSON object —
/// `WebMcpRefusal::OutputSchemaNotAnObject`. Unlike `DuplicateToolName`
/// above, this refusal is scoped to `WebMcpRefusalScope::OutputSchema`, not
/// `Scope::Tool`: the tool itself is still published, only its
/// `outputSchema` field is dropped. Exercises the `scope` field this fix
/// adds to the boot-time log — see
/// `wafer_core::discovery::WebMcpRefusalScope`.
struct WebmcpOutputSchemaRefusalFixtureBlock;

#[wafer_block::wafer_async_trait]
impl Block for WebmcpOutputSchemaRefusalFixtureBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "test/webmcp-output-schema-refusal-fixture",
            "0.0.1",
            "http-handler@v1",
            "one endpoint whose declared output schema is not an object, on purpose",
        )
        .endpoints(vec![BlockEndpoint::get(
            "/x/webmcp-output-schema-refusal-fixture/one",
        )
        .summary("array-shaped response")
        .auth(AuthLevel::Public)
        .output_schema(serde_json::json!({ "type": "array" }))
        .agent_tool("webmcp_output_schema_refusal_fixture", "one")])
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        Ok(())
    }

    async fn handle(&self, _ctx: &dyn Context, _msg: Message, _input: InputStream) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

/// Proves the `scope` field the boot-time log now carries distinguishes a
/// whole-tool refusal (`DuplicateToolName`, scope `tool`) from a
/// field-only refusal (`OutputSchemaNotAnObject`, scope `outputSchema`) —
/// the distinction `registration.rs`'s corrected comment and log message
/// describe. Combines both fixtures in one boot pass so both scopes are
/// asserted from the same runtime construction.
#[tokio::test]
async fn webmcp_refusal_boot_log_includes_scope() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let db_path = tmp
        .path()
        .join("webmcp_refusal_boot_logging_scope_test.sqlite3");
    let storage_root = tmp.path().join("storage");
    std::fs::create_dir_all(&storage_root).expect("create storage root");

    let capture = MessageCapture::default();
    let guard = tracing::subscriber::set_default(capture.clone());
    let wafer = build_runtime_with_extra_blocks(
        &db_path,
        &storage_root,
        vec![
            (
                "test/webmcp-refusal-fixture",
                Arc::new(WebmcpRefusalFixtureBlock) as Arc<dyn Block>,
            ),
            (
                "test/webmcp-output-schema-refusal-fixture",
                Arc::new(WebmcpOutputSchemaRefusalFixtureBlock) as Arc<dyn Block>,
            ),
        ],
    )
    .await;
    drop(guard);
    drop(wafer);

    let refusal_logs = capture.messages_containing(REFUSAL_WARNING);
    assert_eq!(
        refusal_logs.len(),
        3,
        "build() must log one warning per refusal: 2 DuplicateToolName (whole tool) + 1 \
         OutputSchemaNotAnObject (field only): {refusal_logs:?}"
    );

    let tool_scoped = refusal_logs
        .iter()
        .filter(|m| m.contains("scope=tool "))
        .count();
    assert_eq!(
        tool_scoped, 2,
        "both DuplicateToolName refusals must be logged with scope=tool (the whole tool was \
         refused): {refusal_logs:?}"
    );

    let output_schema_scoped = refusal_logs
        .iter()
        .filter(|m| m.contains("scope=outputSchema "))
        .count();
    assert_eq!(
        output_schema_scoped, 1,
        "the OutputSchemaNotAnObject refusal must be logged with scope=outputSchema (only the \
         field was dropped, the tool was still published): {refusal_logs:?}"
    );
}
