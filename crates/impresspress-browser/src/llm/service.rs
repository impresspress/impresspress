//! `BrowserLlmService` — `wafer_core::LlmService` impl backed by WebLLM
//! running in the page via the SW↔page postMessage bridge.

use futures::{channel::mpsc, sink::SinkExt, stream::BoxStream};
use tokio_util::sync::CancellationToken;
use wafer_core::interfaces::llm::service::{
    ChatChunk, ChatRequest, LlmError, LlmService, ModelInfo, ModelStatus,
};

use crate::llm::{
    bridge::{self, StreamFrame},
    catalog::{default_catalog, ModelCatalog},
    openai_codec::{encode_request_body, StreamingDecoder},
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

        let body_json = match encode_request_body(&req.messages, &req.tools) {
            Ok(s) => s,
            Err(e) => return one_shot_err(e),
        };

        let (mut tx, rx) = mpsc::channel::<Result<ChatChunk, LlmError>>(16);

        wasm_bindgen_futures::spawn_local(async move {
            let stream_id = match bridge::start_chat_stream(&body_json).await {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            };

            let mut decoder = StreamingDecoder::new();
            loop {
                if cancel.is_cancelled() {
                    let _ = bridge::cancel_stream(&stream_id).await;
                    break;
                }
                let frame = match bridge::next_chunk(&stream_id).await {
                    Ok(f) => f,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        break;
                    }
                };
                match frame {
                    StreamFrame::Chunk(s) => match decoder.feed(&s) {
                        Ok(chunks) => {
                            for chunk in chunks {
                                if tx.send(Ok(chunk)).await.is_err() {
                                    // consumer dropped
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e)).await;
                            break;
                        }
                    },
                    StreamFrame::Done => break,
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
