//! Shared test fixtures for the `routes` submodules: minimal `Context`
//! stubs and message builders used across the chat/streaming/providers/
//! models unit tests.

use std::sync::{Arc, Mutex};

use wafer_block::common::ServiceOp;
use wafer_core::{
    clients::llm::{ChatChunk, ModelInfo, ModelStatus},
    interfaces::llm::service::LlmError,
};
use wafer_run::{
    context::Context, Block, BlockCategory, BlockInfo, ErrorCode, InputStream, LifecycleEvent,
    Message, OutputStream, WaferError,
};

use crate::blocks::llm::{
    provider_admin::ProviderAdmin, providers::config::ProviderConfig, LlmBlock,
};

/// Minimal Context that panics on `call_block` — the bad-request tests must
/// reject before any block dispatch.
#[derive(Clone)]
pub(super) struct PanicCtx;

#[async_trait::async_trait]
impl Context for PanicCtx {
    async fn call_block(
        &self,
        _block_name: &str,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        panic!("call_block must not be invoked on a parse-error path");
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
        false
    }
    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }
    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        std::sync::Arc::new(self.clone())
    }
}

/// A block whose provider-admin handle is deliberately not what the test is
/// about — it manages providers, holds nothing, and records what it is told.
///
/// It used to be a
/// [`NoopProviderAdmin`](crate::blocks::llm::provider_admin::NoopProviderAdmin)
/// on the grounds that the parse-error
/// tests reject before reaching the provider-admin surface. That stopped
/// being true when the provider CRUD handlers started refusing outright on a
/// runtime that cannot manage providers: an inert handle here would answer
/// 501 to every one of them, and a test asserting 400 for a malformed body
/// would be asserting nothing about the body. The capability question has its
/// own fixture (`providers::inert_router_tests::inert_block`).
pub(super) fn stub_block() -> LlmBlock {
    LlmBlock::new(Arc::new(RecordingProviderAdmin::default()))
}

/// One recorded `call_block` invocation on a [`RecordingCtx`].
pub(in crate::blocks::llm) struct RecordedCall {
    pub(in crate::blocks::llm) block_name: String,
    pub(in crate::blocks::llm) msg: Message,
    pub(in crate::blocks::llm) body: Vec<u8>,
}

/// Context that records every `call_block` invocation (block name, message,
/// drained input body) but config reads, and answers with a canned OK JSON
/// body; a config read answers as for an unset key. `clone_arc`
/// hands out a handle sharing the same call log, so a test can inspect calls
/// made through the cloned Arc.
///
/// [`RecordingCtx::answering`] scripts a reply for one request path, which is
/// what lets a test drive a handler that reads another block through
/// `call_block` — the chat page, which lists threads and entries from
/// `impresspress/messages`.
/// A scripted reply: any request whose path contains the fragment is
/// answered with the body.
struct ScriptedAnswer {
    resource_fragment: String,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
pub(in crate::blocks::llm) struct RecordingCtx {
    calls: Arc<std::sync::Mutex<Vec<RecordedCall>>>,
    answers: Arc<std::sync::Mutex<Vec<ScriptedAnswer>>>,
}

impl RecordingCtx {
    pub(in crate::blocks::llm) fn calls(&self) -> std::sync::MutexGuard<'_, Vec<RecordedCall>> {
        self.calls.lock().expect("call log lock")
    }

    /// Answer any request whose path contains `resource_fragment` with
    /// `body`. First match wins; anything unscripted keeps the canned
    /// `{"id":"entry-1"}` reply.
    pub(in crate::blocks::llm) fn answering(
        self,
        resource_fragment: &str,
        body: serde_json::Value,
    ) -> Self {
        self.answers
            .lock()
            .expect("answers lock")
            .push(ScriptedAnswer {
                resource_fragment: resource_fragment.to_string(),
                body: serde_json::to_vec(&body).expect("scripted body"),
            });
        self
    }

    fn scripted(&self, path: &str) -> Option<Vec<u8>> {
        self.answers
            .lock()
            .expect("answers lock")
            .iter()
            .find(|answer| path.contains(answer.resource_fragment.as_str()))
            .map(|answer| answer.body.clone())
    }
}

#[async_trait::async_trait]
impl Context for RecordingCtx {
    async fn call_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream {
        // No config key is set here: the config block's answer for an unset
        // key is `NotFound`, which a read takes as "use the default". Config
        // reads are not recorded — they are not the hops these tests count.
        if block_name == "wafer-run/config" {
            return OutputStream::error(wafer_run::WaferError::new(
                wafer_run::ErrorCode::NotFound,
                "config key not set",
            ));
        }
        let body = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return OutputStream::error(e),
        };
        let scripted = self.scripted(msg.path());
        self.calls().push(RecordedCall {
            block_name: block_name.to_string(),
            msg,
            body,
        });
        OutputStream::respond(scripted.unwrap_or_else(|| br#"{"id":"entry-1"}"#.to_vec()))
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
        false
    }
    fn config_get(&self, _key: &str) -> Option<&str> {
        None
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

/// Wrap any context and fail the llm → messages **entry writes**, letting the
/// first `passes` of them through.
///
/// The persistence tests need a hop that fails, not a database that fails:
/// `TestContext::call_block` hands the *inner* `TestContext` to a registered
/// block, so a `FailingDbOpContext` around it is invisible to the messages
/// block's own database calls, and `TestContext::break_writes` breaks the
/// user turn and the assistant turn together. Failing the `call_block` hop
/// itself is the one seam that can single out either write.
#[derive(Clone)]
pub(in crate::blocks::llm) struct MessagesWriteFails {
    inner: Arc<dyn Context>,
    passes: Arc<std::sync::atomic::AtomicUsize>,
}

impl MessagesWriteFails {
    /// Fail every `create` addressed to `impresspress/messages` after letting
    /// `passes` of them through: `0` fails the user turn, `1` lets the user
    /// turn land and fails the assistant turn.
    pub(in crate::blocks::llm) fn after(inner: Arc<dyn Context>, passes: usize) -> Self {
        Self {
            inner,
            passes: Arc::new(std::sync::atomic::AtomicUsize::new(passes)),
        }
    }

    /// Consume one allowed pass. `true` when this write should still land.
    fn let_one_pass(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.passes
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }
}

#[async_trait::async_trait]
impl Context for MessagesWriteFails {
    async fn call_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream {
        if block_name == "impresspress/messages" && msg.action() == "create" && !self.let_one_pass()
        {
            return OutputStream::error(WaferError::new(
                ErrorCode::Internal,
                "simulated messages-block outage (MessagesWriteFails)",
            ));
        }
        self.inner.call_block(block_name, msg, input).await
    }
    fn check_resource_access(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> Result<(), WaferError> {
        self.inner
            .check_resource_access(resource, resource_type, access)
    }

    fn resource_access_admitted(
        &self,
        resource: &str,
        resource_type: wafer_run::ResourceType,
        access: wafer_block::ResourceAccess,
    ) -> bool {
        self.inner
            .resource_access_admitted(resource, resource_type, access)
    }
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
    fn registered_blocks(&self) -> &[BlockInfo] {
        self.inner.registered_blocks()
    }
    fn caller_id(&self) -> Option<&str> {
        self.inner.caller_id()
    }
    fn config_get(&self, key: &str) -> Option<&str> {
        self.inner.config_get(key)
    }
    fn clone_arc(&self) -> Arc<dyn Context> {
        Arc::new(self.clone())
    }
}

pub(in crate::blocks::llm) fn admin_msg(action: &str, path: &str) -> Message {
    let mut m = Message::new(format!("{action}:{path}"));
    m.set_meta(wafer_run::META_REQ_ACTION, action);
    m.set_meta(wafer_run::META_REQ_RESOURCE, path);
    m.set_meta(wafer_run::META_AUTH_USER_ID, "admin-user");
    m.set_meta("auth.user_roles", "admin");
    m
}

pub(in crate::blocks::llm) fn user_msg(action: &str, path: &str) -> Message {
    let mut m = Message::new(format!("{action}:{path}"));
    m.set_meta(wafer_run::META_REQ_ACTION, action);
    m.set_meta(wafer_run::META_REQ_RESOURCE, path);
    m.set_meta(wafer_run::META_AUTH_USER_ID, "regular-user");
    m.set_meta("auth.user_roles", "user");
    m
}

/// Run `msg` through the block's own route table so `{id}` / `{backend_id}`
/// / `{model_id}` are bound the way they are on the wire, then hand the
/// message to a handler directly. Panics when no row matches: a test that
/// sends an unroutable path would otherwise exercise the handler's
/// "missing id" branch by accident.
pub(in crate::blocks::llm) fn routed(mut msg: Message) -> Message {
    let route = crate::endpoint_match::dispatch(&mut msg, crate::blocks::llm::ROUTES);
    assert!(
        route.is_some(),
        "no llm route matches {} {}",
        msg.action(),
        msg.path()
    );
    msg
}

/// `ProviderAdmin` that keeps whatever `configure` hands it — including the
/// `api_key` that `reload_provider_service` resolves from `key_var` — and
/// answers `discover_models` with a fixed two-model list.
///
/// The wire-shape tests read the snapshot back to prove the secret reached
/// the handle the handlers hold, and only then check that it never reaches
/// the wire. A leak test whose fixture never had the secret in reach would
/// pass against a leaking handler.
#[derive(Default)]
pub(crate) struct RecordingProviderAdmin {
    configured: Mutex<Vec<ProviderConfig>>,
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl ProviderAdmin for RecordingProviderAdmin {
    fn manages_providers(&self) -> bool {
        true
    }

    fn configure(&self, providers: Vec<ProviderConfig>) -> Result<(), LlmError> {
        *self.configured.lock().expect("configured lock") = providers;
        Ok(())
    }

    fn providers_snapshot(&self) -> Vec<ProviderConfig> {
        self.configured.lock().expect("configured lock").clone()
    }

    async fn discover_models(&self, provider_name: &str) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(vec![
            ModelInfo::new(provider_name, "gpt-4o", "GPT-4o"),
            ModelInfo::new(provider_name, "gpt-4o-mini", "GPT-4o mini"),
        ])
    }
}

/// Stub `wafer-run/llm` service block with scripted answers: `llm.list_models`
/// returns `models`, `llm.status` returns `status`, `llm.chat` streams
/// `chat_chunks` one frame each, `llm.load_model` streams no progress and
/// ends, and `llm.unload_model` acknowledges with an empty body. Anything else errors loudly so a test cannot silently exercise
/// an op it did not script.
pub(crate) struct StubLlmServiceBlock {
    pub(crate) models: Vec<ModelInfo>,
    pub(crate) status: ModelStatus,
    pub(crate) chat_chunks: Vec<ChatChunk>,
    /// How many `llm.chat` requests reached the service. A test that a
    /// request refused *before* the model can only prove it by reading the
    /// provider's own count: asserting the handler's status says nothing
    /// about whether a paid backend was already called.
    pub(crate) chat_calls: Arc<std::sync::atomic::AtomicUsize>,
    /// A classified refusal to answer every op with, instead of the canned
    /// success above.
    ///
    /// The real service block maps each `LlmError` onto a code before it
    /// crosses the block boundary (`wafer-core`'s llm handler:
    /// `ModelNotFound` → `NotFound`, `InvalidRequest` → `InvalidArgument`,
    /// `NotSupported` → `Unimplemented`). A route test can only show that a
    /// handler preserves that classification if the stub can produce one.
    pub(crate) error: Option<(ErrorCode, String)>,
}

impl Default for StubLlmServiceBlock {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            status: ModelStatus::ready(),
            chat_chunks: Vec::new(),
            chat_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            error: None,
        }
    }
}

#[async_trait::async_trait]
impl Block for StubLlmServiceBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            "wafer-run/llm",
            "0.0.1",
            "llm@v1",
            "stub llm service block for route tests",
        )
        .category(BlockCategory::Service)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        // A scripted refusal answers every op, carrying the code the real
        // service block would have assigned. Checked before the canned
        // successes so a test can script the error for any op.
        if let Some((code, message)) = &self.error {
            return OutputStream::error(WaferError::new(*code, message.clone()));
        }
        match msg.kind.as_str() {
            ServiceOp::LLM_LIST_MODELS => OutputStream::respond(
                wafer_block::codec::encode(&self.models).expect("encode models"),
            ),
            ServiceOp::LLM_STATUS => OutputStream::respond(
                wafer_block::codec::encode(&self.status).expect("encode status"),
            ),
            ServiceOp::LLM_UNLOAD_MODEL => OutputStream::respond(Vec::new()),
            // A load that finishes at once: no progress frames.
            ServiceOp::LLM_LOAD_MODEL => OutputStream::respond_with_meta(Vec::new(), Vec::new()),
            ServiceOp::LLM_CHAT => {
                self.chat_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let frames: Vec<Vec<u8>> = self
                    .chat_chunks
                    .iter()
                    .map(|chunk| wafer_block::codec::encode(chunk).expect("encode chunk"))
                    .collect();
                OutputStream::from_producer(move |sink, _cancel| async move {
                    for frame in frames {
                        if sink.send_chunk(frame).await.is_err() {
                            return;
                        }
                    }
                    let _ = sink.complete(Vec::new()).await;
                })
            }
            other => OutputStream::error(WaferError::new(
                ErrorCode::Unimplemented,
                format!("StubLlmServiceBlock: unhandled op {other}"),
            )),
        }
    }

    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }
}
