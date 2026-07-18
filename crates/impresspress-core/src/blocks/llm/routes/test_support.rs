//! Shared test fixtures for the `routes` submodules: minimal `Context`
//! stubs and message builders used across the chat/streaming/providers/
//! models unit tests.

use std::sync::Arc;

use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::blocks::llm::{provider_admin::NoopProviderAdmin, LlmBlock};

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

/// The parse-error tests reject before reaching the provider-admin surface,
/// so the no-op handle suffices.
pub(super) fn stub_block() -> LlmBlock {
    LlmBlock::new(Arc::new(NoopProviderAdmin))
}

/// One recorded `call_block` invocation on a [`RecordingCtx`].
pub(super) struct RecordedCall {
    pub(super) block_name: String,
    pub(super) msg: Message,
    pub(super) body: Vec<u8>,
}

/// Context that records every `call_block` invocation (block name, message,
/// drained input body) and answers with a canned OK JSON body. `clone_arc`
/// hands out a handle sharing the same call log, so a test can inspect calls
/// made through the cloned Arc.
#[derive(Clone, Default)]
pub(super) struct RecordingCtx {
    calls: Arc<std::sync::Mutex<Vec<RecordedCall>>>,
}

impl RecordingCtx {
    pub(super) fn calls(&self) -> std::sync::MutexGuard<'_, Vec<RecordedCall>> {
        self.calls.lock().expect("call log lock")
    }
}

#[async_trait::async_trait]
impl Context for RecordingCtx {
    async fn call_block(&self, block_name: &str, msg: Message, input: InputStream) -> OutputStream {
        let body = input.collect_to_bytes().await;
        self.calls().push(RecordedCall {
            block_name: block_name.to_string(),
            msg,
            body,
        });
        OutputStream::respond(br#"{"id":"entry-1"}"#.to_vec())
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

pub(super) fn admin_msg(action: &str, path: &str) -> Message {
    let mut m = Message::new(format!("{action}:{path}"));
    m.set_meta(wafer_run::META_REQ_ACTION, action);
    m.set_meta(wafer_run::META_REQ_RESOURCE, path);
    m.set_meta(wafer_run::META_AUTH_USER_ID, "admin-user");
    m.set_meta("auth.user_roles", "admin");
    m
}

pub(super) fn user_msg(action: &str, path: &str) -> Message {
    let mut m = Message::new(format!("{action}:{path}"));
    m.set_meta(wafer_run::META_REQ_ACTION, action);
    m.set_meta(wafer_run::META_REQ_RESOURCE, path);
    m.set_meta(wafer_run::META_AUTH_USER_ID, "regular-user");
    m.set_meta("auth.user_roles", "user");
    m
}
