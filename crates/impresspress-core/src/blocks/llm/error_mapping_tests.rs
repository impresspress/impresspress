//! What a failed database or service call answers on the llm routes.
//!
//! A WRAP `PermissionDenied` — a [`wafer_run::ResourceGrant`] the deployment
//! is missing — is a 403, and a quota is a 429, the same as on every other
//! block (`crud::db_error_internal`). Each test drives a real route through
//! [`LlmBlock`]'s own dispatch over a context that refuses the one call the
//! site under test makes, so every earlier step of the route runs for real.
//!
//! A JSON route must end in the door's own "Access denied": the status alone
//! would also match a route gate's refusal, which carries its own message. A
//! full page must be the styled 403 `ui::refused_response` draws ("Go home"),
//! with none of the denial's own text in it.

use std::sync::Arc;

use wafer_block::ServiceOp;
use wafer_run::{
    context::Context, streams::output::TerminalNotResponse, Block, ErrorCode, InputStream, Message,
    OutputStream, WaferError,
};

use super::{
    repo,
    routes::test_support::{RecordingProviderAdmin, StubLlmServiceBlock},
    schema::TABLE as PROVIDERS_TABLE,
    LlmBlock, DEFAULT_MODEL_VAR, DEFAULT_PROVIDER_VAR,
};
use crate::test_support::{admin_msg, auth_msg, output_json, FailingDbOpContext, TestContext};

/// The refusal WRAP answers a call its caller holds no grant for. Its text
/// names the grant, which is deployment topology: logged, never shown.
fn wrap_denial() -> WaferError {
    WaferError::new(
        ErrorCode::PermissionDenied,
        "WRAP: impresspress/llm holds no grant on this table",
    )
}

/// `ctx` with `ops` refused the way WRAP refuses them.
fn denied(ctx: &TestContext, ops: Vec<(&'static str, &'static str)>) -> FailingDbOpContext {
    FailingDbOpContext::failing_with(ctx.clone(), ops, wrap_denial())
}

/// Every database op on `table`.
fn every_op_on(table: &'static str) -> Vec<(&'static str, &'static str)> {
    ServiceOp::DATABASE_OPS
        .iter()
        .map(|op| (*op, table))
        .collect()
}

fn block() -> LlmBlock {
    LlmBlock::new(Arc::new(RecordingProviderAdmin::default()))
}

/// A JSON request through the block's own `handle`.
async fn api(ctx: &dyn Context, msg: Message, body: &str) -> OutputStream {
    let mut msg = msg;
    msg.set_meta("http.header.accept", "application/json");
    block()
        .handle(ctx, msg, InputStream::from_bytes(body.as_bytes().to_vec()))
        .await
}

/// A browser `GET` of `path` through the block's own `handle`.
async fn page(ctx: &dyn Context, path: &str) -> wafer_block::http_codec::HttpResponseParts {
    let mut msg = admin_msg("retrieve", path);
    msg.set_meta("http.header.accept", "text/html");
    let out = block().handle(ctx, msg, InputStream::empty()).await;
    wafer_block::http_codec::collect_http_response(out).await
}

/// Records a miss unless the request ended in the door's WRAP denial:
/// `PermissionDenied` with its own "Access denied".
async fn expect_wrap_denial(misses: &mut Vec<String>, out: OutputStream, site: &str) {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(error))
            if (error.code, error.message.as_str())
                == (ErrorCode::PermissionDenied, "Access denied") => {}
        Err(TerminalNotResponse::Error(error)) => {
            misses.push(format!("{site}: {:?} {:?}", error.code, error.message))
        }
        Ok(_) => misses.push(format!("{site}: a response, not a WRAP denial")),
        Err(_) => misses.push(format!("{site}: another terminal, not a WRAP denial")),
    }
}

/// Records a miss unless the page is the styled 403 a refused read gets,
/// with none of the denial's text.
async fn expect_refused_page(misses: &mut Vec<String>, ctx: &dyn Context, path: &str) {
    let parts = page(ctx, path).await;
    let html = String::from_utf8_lossy(&parts.body);
    if parts.status != 403 || !html.contains("Go home") || html.contains("holds no grant") {
        misses.push(format!("{path}: {} {html}", parts.status));
    }
}

fn report(misses: Vec<String>) {
    assert!(
        misses.is_empty(),
        "expected the door's WRAP denial at every site:\n{}",
        misses.join("\n")
    );
}

/// An llm fixture holding one provider row, and that row's id.
async fn with_a_provider() -> (TestContext, String) {
    let ctx = TestContext::with_llm().await;
    let created = output_json(
        api(
            &ctx,
            admin_msg("create", "/b/llm/api/providers"),
            r#"{"name":"main","protocol":"open_ai","endpoint":"https://api.openai.com/v1"}"#,
        )
        .await,
    )
    .await;
    let id = created["id"].as_str().expect("provider id").to_string();
    (ctx, id)
}

/// Listing the providers classifies its read. A guard: the list already went
/// through the door, so this passes on the code before the reload sites
/// were converted too.
#[tokio::test]
async fn provider_list_denial_is_403() {
    let (ctx, _) = with_a_provider().await;
    let mut misses = Vec::new();
    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, vec![(ServiceOp::DATABASE_LIST, PROVIDERS_TABLE)]),
            admin_msg("retrieve", "/b/llm/api/providers"),
            "",
        )
        .await,
        "GET /b/llm/api/providers",
    )
    .await;
    report(misses);
}

/// Every provider write reloads the router from the providers table. That
/// reload's refused read is a 403 at all five of its call sites — not a 500
/// from an error flattened to a `String`, after the write had landed.
#[tokio::test]
async fn a_refused_reload_read_is_403_on_every_provider_write() {
    let mut misses = Vec::new();
    let list = || vec![(ServiceOp::DATABASE_LIST, PROVIDERS_TABLE)];

    let ctx = TestContext::with_llm().await;
    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, list()),
            admin_msg("create", "/b/llm/api/providers"),
            r#"{"name":"main","protocol":"open_ai","endpoint":"https://api.openai.com/v1"}"#,
        )
        .await,
        "POST /b/llm/api/providers (reload)",
    )
    .await;

    let (ctx, id) = with_a_provider().await;
    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, list()),
            admin_msg("update", &format!("/b/llm/api/providers/{id}")),
            r#"{"enabled":false}"#,
        )
        .await,
        "PATCH /b/llm/api/providers/{id} (reload)",
    )
    .await;

    let (ctx, id) = with_a_provider().await;
    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, list()),
            admin_msg(
                "create",
                &format!("/b/llm/api/providers/{id}/discover-models"),
            ),
            "",
        )
        .await,
        "discover-models (the reload before discovery)",
    )
    .await;

    let (ctx, id) = with_a_provider().await;
    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, list()).after_passing(1),
            admin_msg(
                "create",
                &format!("/b/llm/api/providers/{id}/discover-models"),
            ),
            "",
        )
        .await,
        "discover-models (the reload after the write-back)",
    )
    .await;

    let (ctx, id) = with_a_provider().await;
    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, list()),
            admin_msg("delete", &format!("/b/llm/api/providers/{id}")),
            "",
        )
        .await,
        "DELETE /b/llm/api/providers/{id} (reload)",
    )
    .await;

    report(misses);
}

/// Discovery writes the discovered model list back to the row; a refused
/// write is a 403.
#[tokio::test]
async fn a_refused_discovery_write_back_is_403() {
    let (ctx, id) = with_a_provider().await;
    let mut misses = Vec::new();
    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, vec![(ServiceOp::DATABASE_UPDATE, PROVIDERS_TABLE)]),
            admin_msg(
                "create",
                &format!("/b/llm/api/providers/{id}/discover-models"),
            ),
            "",
        )
        .await,
        "discover-models (write-back)",
    )
    .await;
    report(misses);
}

/// A per-thread override is read, then written; either refused is a 403.
#[tokio::test]
async fn a_refused_thread_override_is_403() {
    let ctx = TestContext::with_llm().await;
    let body = r#"{"thread_id":"t1","provider_block":"main","model":"gpt-4o"}"#;
    let mut misses = Vec::new();
    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, every_op_on(repo::settings::TABLE)),
            admin_msg("create", "/b/llm/api/config"),
            body,
        )
        .await,
        "POST /b/llm/api/config (the override read)",
    )
    .await;
    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, every_op_on(repo::settings::TABLE)).after_passing(1),
            admin_msg("create", "/b/llm/api/config"),
            body,
        )
        .await,
        "POST /b/llm/api/config (the override write)",
    )
    .await;
    report(misses);
}

/// A chat fixture: the messages block, one thread owned by `user-a`, a
/// default provider and model, and `service` as `wafer-run/llm`. Returns the
/// context and the thread id.
async fn chat_fixture(service: StubLlmServiceBlock) -> (TestContext, String) {
    let mut ctx = TestContext::with_llm().await;
    let sqlite: Vec<&str> = crate::blocks::messages::migrations::SQLITE_MIGRATIONS
        .iter()
        .map(|(_, sql)| *sql)
        .collect();
    crate::migration_helper::apply_migrations(
        &ctx,
        "impresspress/messages",
        &sqlite,
        crate::blocks::messages::migrations::POSTGRES_MIGRATIONS,
    )
    .await
    .expect("apply messages migrations");
    ctx.register_block(
        "impresspress/messages",
        Arc::new(crate::blocks::messages::MessagesBlock::new()),
    );
    ctx.set_config(DEFAULT_PROVIDER_VAR, "stub-backend");
    ctx.set_config(DEFAULT_MODEL_VAR, "stub-model");
    ctx.register_block("wafer-run/llm", Arc::new(service));
    let thread = crate::blocks::messages::service::create_context(
        &ctx.fixture(),
        "user-a",
        "conversation",
        "T",
        "",
        "",
        None,
        None,
    )
    .await
    .expect("seed a thread");
    (ctx, thread.id)
}

/// A service that refuses every op the way WRAP refuses a call.
fn refusing_service() -> StubLlmServiceBlock {
    let denial = wrap_denial();
    StubLlmServiceBlock {
        error: Some((denial.code, denial.message)),
        ..Default::default()
    }
}

fn chat_msg() -> Message {
    auth_msg("create", "/b/llm/api/chat", "user-a")
}

fn chat_body(thread_id: &str) -> String {
    serde_json::json!({ "thread_id": thread_id, "message": "hi" }).to_string()
}

/// The chat prelude reads the thread's override, then dispatches to the
/// llm service. Each refused is a 403: the service's refusal keeps its code
/// rather than being reduced to its message.
#[tokio::test]
async fn a_refused_chat_prelude_is_403() {
    let mut misses = Vec::new();

    let (ctx, thread) = chat_fixture(StubLlmServiceBlock::default()).await;
    expect_wrap_denial(
        &mut misses,
        api(
            &denied(&ctx, every_op_on(repo::settings::TABLE)),
            chat_msg(),
            &chat_body(&thread),
        )
        .await,
        "POST /b/llm/api/chat (the override read)",
    )
    .await;

    let (ctx, thread) = chat_fixture(refusing_service()).await;
    expect_wrap_denial(
        &mut misses,
        api(&ctx, chat_msg(), &chat_body(&thread)).await,
        "POST /b/llm/api/chat (the service dispatch)",
    )
    .await;

    report(misses);
}

/// The aggregated model list keeps the service's code.
#[tokio::test]
async fn a_refused_model_list_is_403() {
    let mut ctx = TestContext::with_llm().await;
    ctx.register_block("wafer-run/llm", Arc::new(refusing_service()));
    let mut misses = Vec::new();
    expect_wrap_denial(
        &mut misses,
        api(
            &ctx,
            auth_msg("retrieve", "/b/llm/api/models", "user-a"),
            "",
        )
        .await,
        "GET /b/llm/api/models",
    )
    .await;
    report(misses);
}

/// Answers every call to `impresspress/messages` with a WRAP denial — what
/// the messages block itself answers when its own read is refused.
#[derive(Clone)]
struct MessagesRefused(TestContext);

#[async_trait::async_trait]
impl Context for MessagesRefused {
    async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
        if name == "impresspress/messages" {
            return OutputStream::error(wrap_denial());
        }
        self.0.call_block(name, msg, input).await
    }
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), WaferError> {
        self.0
            .check_resource_access(resource, resource_type, access)
    }
    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.0
            .resource_access_admitted(resource, resource_type, access)
    }
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
    fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
        self.0.registered_blocks()
    }
    fn config_get(&self, key: &str) -> Option<&str> {
        self.0.config_get(key)
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

/// Every llm page whose read failed is the styled 403 page, not the 500
/// page.
#[tokio::test]
async fn refused_page_reads_are_the_403_page() {
    let mut misses = Vec::new();

    let ctx = TestContext::with_llm().await;
    expect_refused_page(
        &mut misses,
        &denied(&ctx, every_op_on(PROVIDERS_TABLE)),
        "/b/llm/providers",
    )
    .await;
    expect_refused_page(
        &mut misses,
        &denied(&ctx, every_op_on(repo::settings::TABLE)),
        "/b/llm/settings",
    )
    .await;
    expect_refused_page(&mut misses, &MessagesRefused(ctx.clone()), "/b/llm/").await;

    let (ctx, thread) = chat_fixture(StubLlmServiceBlock::default()).await;
    // The thread list is read first; only the entry list is refused here.
    struct EntriesRefused(TestContext);
    #[async_trait::async_trait]
    impl Context for EntriesRefused {
        async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
            if name == "impresspress/messages" && msg.path().contains("/entries") {
                return OutputStream::error(wrap_denial());
            }
            self.0.call_block(name, msg, input).await
        }
        /// Admits nothing, as the fail-closed `check_resource_access` default
        /// this context keeps does.
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_run::ResourceType,
            _access: wafer_block::ResourceAccess,
        ) -> bool {
            false
        }
        fn is_cancelled(&self) -> bool {
            self.0.is_cancelled()
        }
        fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
            self.0.registered_blocks()
        }
        fn config_get(&self, key: &str) -> Option<&str> {
            self.0.config_get(key)
        }
        fn clone_arc(&self) -> Arc<dyn Context> {
            Arc::new(EntriesRefused(self.0.clone()))
        }
    }
    expect_refused_page(
        &mut misses,
        &EntriesRefused(ctx),
        &format!("/b/llm/threads/{thread}"),
    )
    .await;

    let mut ctx = TestContext::with_llm().await;
    ctx.register_block("wafer-run/llm", Arc::new(refusing_service()));
    expect_refused_page(&mut misses, &ctx, "/b/llm/models").await;

    report(misses);
}

/// A deployment whose default provider is the legacy router with no enabled
/// provider is configured that way, not broken: the chat answers 503 naming
/// what an admin has to do, not the sanitized 500.
#[tokio::test]
async fn a_chat_with_no_enabled_provider_is_a_503_that_says_why() {
    let (mut ctx, thread) = chat_fixture(StubLlmServiceBlock::default()).await;
    ctx.set_config(DEFAULT_PROVIDER_VAR, super::DEFAULT_PROVIDER);
    match api(&ctx, chat_msg(), &chat_body(&thread))
        .await
        .collect_buffered()
        .await
    {
        Err(TerminalNotResponse::Error(error)) => {
            assert_eq!(error.code, ErrorCode::Unavailable, "{}", error.message);
            assert!(
                error.message.contains("/b/llm/providers"),
                "the refusal names where an admin fixes it: {}",
                error.message
            );
        }
        Ok(_) => panic!("expected a 503, got a response"),
        Err(_) => panic!("expected a 503, got another terminal"),
    }
}

/// Answers the messages block's thread list with a body that has no
/// `records` array.
#[derive(Clone)]
struct ThreadListUnreadable(TestContext);

#[async_trait::async_trait]
impl Context for ThreadListUnreadable {
    async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
        if name == "impresspress/messages" {
            return OutputStream::respond(br#"{"total_count":0}"#.to_vec());
        }
        self.0.call_block(name, msg, input).await
    }
    /// Admits nothing, as the fail-closed `check_resource_access` default
    /// this context keeps does.
    fn resource_access_admitted(
        &self,
        _resource: &str,
        _resource_type: wafer_run::ResourceType,
        _access: wafer_block::ResourceAccess,
    ) -> bool {
        false
    }
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
    fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
        self.0.registered_blocks()
    }
    fn config_get(&self, key: &str) -> Option<&str> {
        self.0.config_get(key)
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

/// A thread list the chat page cannot read is a failed page, not an empty
/// sidebar claiming the caller has no threads.
#[tokio::test]
async fn an_unreadable_thread_list_fails_the_chat_page() {
    let ctx = TestContext::with_llm().await;
    let parts = page(&ThreadListUnreadable(ctx), "/b/llm/").await;
    assert_eq!(
        parts.status,
        500,
        "{}",
        String::from_utf8_lossy(&parts.body)
    );
}

/// What a provider write whose name another provider holds is told: the
/// name, the field, and nothing of the driver's text.
const MAIN_TAKEN: &str =
    "A provider with the name \"main\" already exists. Choose a different name.";

/// Asserts `out` is the named 409 for the name `main`, with no schema in it.
async fn expect_main_taken(out: OutputStream) {
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap_or_default();
    assert_eq!(parts.status, 409, "{body}");
    assert_eq!(body["message"], serde_json::json!(MAIN_TAKEN), "{body}");
    let text = body.to_string();
    assert!(
        !text.contains(PROVIDERS_TABLE) && !text.contains("UNIQUE"),
        "schema leaked: {text}"
    );
}

/// A second provider under a name the first holds is refused by the
/// `providers.name` UNIQUE index, and every backend reports that refusal as
/// `AlreadyExists`. The route answers the 409 naming the taken name — not
/// "Internal server error" for an admin who re-typed a name, nor a generic
/// "same key" that leaves them to guess which field — and the driver's text,
/// which names the table and the column, stays in the log.
#[tokio::test]
async fn a_duplicate_provider_name_is_a_409_naming_the_name() {
    let (ctx, _) = with_a_provider().await;

    expect_main_taken(
        api(
            &ctx,
            admin_msg("create", "/b/llm/api/providers"),
            r#"{"name":"main","protocol":"open_ai","endpoint":"https://api.openai.com/v1"}"#,
        )
        .await,
    )
    .await;

    let rows = wafer_core::clients::database::count(&ctx, PROVIDERS_TABLE, &[])
        .await
        .expect("count providers");
    assert_eq!(rows, 1, "the refused create must not have written a row");
}

/// The admin page's "Add provider" form posts form-encoded fields to the same
/// route; its 409 is the JSON envelope `chrome.js`'s error toast reads the
/// message out of, so the operator sees which name is taken.
#[tokio::test]
async fn the_add_provider_form_is_told_which_name_is_taken() {
    let (ctx, _) = with_a_provider().await;

    let mut msg = admin_msg("create", "/b/llm/api/providers");
    msg.set_meta("http.header.accept", "text/html");
    msg.set_meta("http.header.hx-request", "true");
    let out = block()
        .handle(
            &ctx,
            msg,
            InputStream::from_bytes(
                b"name=main&protocol=open_ai&endpoint=https%3A%2F%2Fapi.openai.com%2Fv1".to_vec(),
            ),
        )
        .await;
    expect_main_taken(out).await;
}

/// Renaming a provider onto a name another provider holds is the same named
/// 409, and the renamed row keeps its name.
#[tokio::test]
async fn renaming_a_provider_onto_a_taken_name_is_a_409_naming_the_name() {
    let (ctx, _) = with_a_provider().await;
    let other = output_json(
        api(
            &ctx,
            admin_msg("create", "/b/llm/api/providers"),
            r#"{"name":"backup","protocol":"open_ai","endpoint":"https://api.openai.com/v1"}"#,
        )
        .await,
    )
    .await;
    let other_id = other["id"].as_str().expect("provider id");

    expect_main_taken(
        api(
            &ctx,
            admin_msg("update", &format!("/b/llm/api/providers/{other_id}")),
            r#"{"name":"main"}"#,
        )
        .await,
    )
    .await;

    let stored = wafer_core::clients::database::get(&ctx, PROVIDERS_TABLE, other_id)
        .await
        .expect("the renamed provider");
    assert_eq!(crate::util::RecordExt::str_field(&stored, "name"), "backup");
}
