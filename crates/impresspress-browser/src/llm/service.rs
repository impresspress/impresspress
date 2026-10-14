//! `BrowserLlmService` — `wafer_core::LlmService` impl backed by WebLLM
//! running in the page via the SW↔page postMessage bridge.

use futures::{channel::mpsc, sink::SinkExt, stream::BoxStream};
use impresspress_core::llm_wire::openai::{encode_chat_body, MaxTokensField, OpenAiSseDecoder};
use tokio_util::sync::CancellationToken;
use wafer_core::interfaces::llm::service::{
    ChatChunk, ChatRequest, LlmError, LlmService, ModelInfo, ModelStatus,
};

use crate::llm::{
    bridge::{self, StreamFrame},
    catalog::{default_catalog, ModelCatalog},
};

const WEBLLM_BACKEND: &str = "webllm";

/// `LlmService` impl backed by WebLLM running in the page.
///
/// Single-engine — WebLLM keeps one model in memory at a time. Model loading
/// is page-direct (via the `loadEngine` ESM export in webllm-engine.js);
/// chat / unload / cancel still go through the SW↔page postMessage bridge.
pub struct BrowserLlmService {
    catalog: ModelCatalog,
}

impl BrowserLlmService {
    pub fn new() -> Self {
        Self {
            catalog: default_catalog(),
        }
    }

    pub fn with_catalog(catalog: ModelCatalog) -> Self {
        Self { catalog }
    }
}

impl Default for BrowserLlmService {
    fn default() -> Self {
        Self::new()
    }
}

fn one_shot_err<T: 'static>(err: LlmError) -> BoxStream<'static, Result<T, LlmError>> {
    Box::pin(futures::stream::once(async move { Err(err) }))
}

#[async_trait::async_trait(?Send)]
impl LlmService for BrowserLlmService {
    async fn chat_stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ChatChunk, LlmError>> {
        if !self.claims_backend(&req.backend_id) {
            return one_shot_err(LlmError::BackendError(format!(
                "backend '{}' not claimed by webllm",
                req.backend_id
            )));
        }

        // One OpenAI encoder for every target. WebLLM's `chat.completions.
        // create` reads `messages` and `tools` and ignores the rest of the
        // OpenAI request shape, so the extra fields this emits (`model`,
        // `stream`, the sampling params) cost a few bytes on a postMessage and
        // buy the browser the encoder's multimodal support, which the deleted
        // copy refused outright.
        //
        // The budget is spelled `max_tokens` because that is the original
        // OpenAI field and the bridge is not OpenAI's API. WebLLM reads
        // neither spelling, so the choice costs nothing here and keeps the
        // browser body identical to the OpenAI-compatible servers'.
        let body_json = match encode_chat_body(&req, MaxTokensField::MaxTokens) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(e) => {
                    return one_shot_err(LlmError::BackendError(format!(
                        "webllm: request body is not utf-8: {e}"
                    )))
                }
            },
            Err(e) => {
                return one_shot_err(LlmError::BackendError(format!(
                    "webllm: encode request: {e}"
                )))
            }
        };

        let (mut tx, rx) = mpsc::channel::<Result<ChatChunk, LlmError>>(16);
        // The chat runs on the page that holds this model (bridge.js routes
        // by it), the one the model was loaded into page-direct.
        let model = req.model;

        wasm_bindgen_futures::spawn_local(async move {
            let stream_id = match bridge::start_chat_stream(&model, &body_json).await {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };

            // The page generates until its stream ends or is cancelled. Every
            // exit below that is not the stream's own end (`Done`, an error
            // frame, a failed pull, which the bridge has already closed)
            // cancels it, so the page stops working for a reader that left.
            let mut decoder = OpenAiSseDecoder::new();
            loop {
                // Raced against the token `wafer-run/llm`'s handler fires when
                // its output is dropped: while the model is still thinking —
                // prefill, a long pause — no frame arrives to fail a send on,
                // so without the race the page generated on, for nobody, until
                // its next chunk.
                let frame = match cancel
                    .run_until_cancelled(bridge::next_chunk(&stream_id))
                    .await
                {
                    None => {
                        let _ = bridge::cancel_stream(&stream_id).await;
                        return;
                    }
                    Some(Ok(f)) => f,
                    Some(Err(e)) => {
                        let _ = tx.send(Err(e)).await;
                        break;
                    }
                };
                match frame {
                    // The bridge hands over one already-de-framed OpenAI chunk
                    // JSON per message, so this is the decoder's frame-level
                    // entry point rather than its SSE one. A malformed chunk is
                    // logged and skipped (the shared policy for both providers)
                    // instead of killing the turn — and "logged" is literal
                    // here only because `logger::init_console_tracing` is
                    // installed at startup; with no subscriber the skip was
                    // silent, and a truncated answer looked like a complete
                    // one.
                    StreamFrame::Chunk(s) => {
                        for chunk in decoder.push_frame(&s).chunks {
                            if tx.send(Ok(chunk)).await.is_err() {
                                // The reader dropped the stream: the page is
                                // still generating, so stop it.
                                let _ = bridge::cancel_stream(&stream_id).await;
                                return;
                            }
                        }
                    }
                    // WebLLM signals the end out-of-band; that is this
                    // transport's `[DONE]`, and it must close any tool call the
                    // model left open (a no-op when a `finish_reason` already
                    // closed them). The page's stream has ended, so a reader
                    // that left by now leaves nothing to cancel.
                    StreamFrame::Done => {
                        for chunk in decoder.finish() {
                            if tx.send(Ok(chunk)).await.is_err() {
                                return;
                            }
                        }
                        break;
                    }
                    StreamFrame::Error(msg) => {
                        let _ = tx
                            .send(Err(LlmError::BackendError(format!("webllm: {msg}"))))
                            .await;
                        break;
                    }
                }
            }
        });

        Box::pin(rx)
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(self.catalog.models().to_vec())
    }

    async fn status(&self, backend_id: &str, _model_id: &str) -> Result<ModelStatus, LlmError> {
        if backend_id != WEBLLM_BACKEND {
            return Err(LlmError::BackendError(format!(
                "backend '{backend_id}' not claimed by webllm"
            )));
        }
        // Page-direct load means engine state lives in webllm-engine.js, not on
        // the SW side — we don't have direct visibility. Returning `unloaded` is
        // a safe-but-degraded answer: the admin LLM page renders WebLLM models as
        // unloaded even when they're actually live in someone's window. Acceptable
        // because the admin page is a server-side surface and WebLLM is a
        // browser-only backend; if a real consumer ever needs accurate status,
        // add a `llm-engine-state` postMessage round-trip then.
        Ok(ModelStatus::unloaded())
    }

    async fn unload_model(&self, backend_id: &str, model_id: &str) -> Result<(), LlmError> {
        if backend_id != WEBLLM_BACKEND {
            return Err(LlmError::BackendError(format!(
                "backend '{backend_id}' not claimed by webllm"
            )));
        }
        bridge::unload_engine(model_id).await
    }

    fn claims_backend(&self, backend_id: &str) -> bool {
        backend_id == WEBLLM_BACKEND
    }
}

/// The service against a stand-in page: a worker global whose one window
/// runs the LLM engine, holds model `m`, answers a chat with one chunk and
/// then thinks — sends nothing more until the test makes it — and records the
/// `llm-stream-cancel` it is sent. Everything between `chat_stream` and the
/// page is the real `bridge.js`.
#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use std::future::Future;

    use futures::StreamExt;
    use js_sys::Reflect;
    use wafer_core::interfaces::llm::service::{ChatMessage, ChatParams, ChunkDelta};
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_name = setTimeout)]
        fn set_timeout(callback: &js_sys::Function, delay_ms: i32) -> JsValue;
        #[wasm_bindgen(js_name = clearTimeout)]
        fn clear_timeout(id: &JsValue);
    }

    /// `fut`'s output, or a failure naming `what` after five seconds. The
    /// timer only turns a hang into a failure, and is cleared either way.
    async fn bounded<F: Future + Unpin>(fut: F, what: &str) -> F::Output {
        let mut timer_id = JsValue::UNDEFINED;
        let timer = JsFuture::from(js_sys::Promise::new(&mut |resolve, _reject| {
            timer_id = set_timeout(&resolve, 5_000);
        }));
        let outcome = futures::future::select(fut, timer).await;
        clear_timeout(&timer_id);
        match outcome {
            futures::future::Either::Left((output, _)) => output,
            futures::future::Either::Right(_) => panic!("{what}: nothing within 5 s"),
        }
    }

    /// Install the stand-in page; its handle is `globalThis.__llmTestPage`.
    fn install_page() -> JsValue {
        js_sys::Function::new_no_args(
            r#"
            const chunk = (text) => JSON.stringify({ choices: [{ delta: { content: text } }] });
            let cancelled;
            const page = {
                id: 'llm-test-page',
                stream: null,
                cancelled: new Promise((resolve) => { cancelled = resolve; }),
                send(text) {
                    globalThis.__impresspressCompleteLlmMessage({
                        type: 'llm-stream-frame', id: page.stream, kind: 'chunk', payload: chunk(text),
                    });
                },
                postMessage(msg) {
                    if (msg.type === 'engine-probe') {
                        queueMicrotask(() => globalThis.__impresspressCompleteEngineProbe({
                            type: 'engine-present', id: msg.id, loaded: ['m'], loading: [],
                        }));
                    } else if (msg.type === 'llm-chat-stream-request') {
                        page.stream = msg.id;
                        queueMicrotask(() => page.send('one'));
                    } else if (msg.type === 'llm-stream-cancel') {
                        cancelled(msg.id);
                    }
                },
            };
            globalThis.self = {
                clients: {
                    matchAll: async () => [page],
                    get: async (id) => (id === page.id ? page : undefined),
                },
            };
            globalThis.__llmTestPage = page;
            return page;
            "#,
        )
        .call0(&JsValue::UNDEFINED)
        .expect("install the stand-in page")
    }

    fn remove_page() {
        let global = js_sys::global();
        Reflect::delete_property(&global, &JsValue::from_str("self")).expect("remove self");
        Reflect::delete_property(&global, &JsValue::from_str("__llmTestPage"))
            .expect("remove the page");
    }

    /// The id of the stream the page was sent a cancel for.
    async fn cancelled(page: &JsValue, what: &str) -> String {
        let promise: js_sys::Promise = Reflect::get(page, &JsValue::from_str("cancelled"))
            .expect("the page's cancel promise")
            .unchecked_into();
        bounded(JsFuture::from(promise), what)
            .await
            .expect("the cancel promise resolves")
            .as_string()
            .expect("a stream id")
    }

    fn stream_id(page: &JsValue) -> String {
        Reflect::get(page, &JsValue::from_str("stream"))
            .expect("the page's stream")
            .as_string()
            .expect("a stream was started")
    }

    async fn chat(cancel: CancellationToken) -> BoxStream<'static, Result<ChatChunk, LlmError>> {
        BrowserLlmService::new()
            .chat_stream(
                ChatRequest {
                    backend_id: WEBLLM_BACKEND.to_string(),
                    model: "m".to_string(),
                    messages: vec![ChatMessage::user("hi")],
                    params: ChatParams::default(),
                    tools: Vec::new(),
                    extra: serde_json::Value::Null,
                },
                cancel,
            )
            .await
    }

    async fn first_text(stream: &mut BoxStream<'static, Result<ChatChunk, LlmError>>) {
        let first = bounded(stream.next(), "the first chunk")
            .await
            .expect("a first item")
            .expect("a chunk");
        assert!(
            matches!(&first.delta, ChunkDelta::Text(t) if t == "one"),
            "{first:?}"
        );
    }

    /// A chat whose reader leaves while the model is thinking — the handler
    /// fires the token, no frame arrives — tells the page to stop.
    ///
    /// The producer checked the token only between frames, so while the
    /// page was silent it waited on the next frame and the page generated
    /// on until it sent one.
    #[wasm_bindgen_test]
    async fn a_cancel_while_the_model_is_thinking_stops_the_page() {
        let page = install_page();
        let cancel = CancellationToken::new();
        let mut stream = chat(cancel.clone()).await;
        first_text(&mut stream).await;

        cancel.cancel();

        assert_eq!(
            cancelled(&page, "the page's cancel while it thinks").await,
            stream_id(&page)
        );
        remove_page();
    }

    /// A reader that drops the stream is found out at the next chunk, whose
    /// send fails — and the page, still generating, is told to stop.
    ///
    /// The producer returned there without cancelling the page's stream, so
    /// the page generated to the end of the answer for nobody.
    #[wasm_bindgen_test]
    async fn a_reader_that_drops_the_stream_stops_the_page_at_the_next_chunk() {
        let page = install_page();
        let mut stream = chat(CancellationToken::new()).await;
        first_text(&mut stream).await;

        drop(stream);
        js_sys::Function::new_no_args("globalThis.__llmTestPage.send('two');")
            .call0(&JsValue::UNDEFINED)
            .expect("the page sends its next chunk");

        assert_eq!(
            cancelled(&page, "the page's cancel after a failed send").await,
            stream_id(&page)
        );
        remove_page();
    }
}
