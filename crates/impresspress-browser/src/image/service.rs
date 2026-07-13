//! `BrowserImageService` — `wafer_core::ImageService` impl backed by
//! `@huggingface/transformers.js` running in the page via the SW↔page
//! postMessage bridge.
//!
//! Single-engine — transformers.js keeps one pipeline in memory at a time.
//! Model loading is one-shot (no intermediate progress events surfaced yet);
//! generation is streamed (progress / done / error frames) over the same
//! SW↔page bridge pattern as the LLM service.

use futures::{channel::mpsc, sink::SinkExt, stream::BoxStream};
use tokio_util::sync::CancellationToken;
use wafer_core::interfaces::image::service::{
    GeneratedImage, ImageError, ImageRequest, ImageResponse, ImageService, LoadProgress, ModelInfo,
    ModelStatus,
};

use crate::image::{
    bridge::{self, Frame},
    catalog::{default_catalog, ModelCatalog, BACKEND_ID},
};

pub struct BrowserImageService {
    catalog: ModelCatalog,
}

impl BrowserImageService {
    pub fn new() -> Self {
        Self {
            catalog: default_catalog(),
        }
    }

    pub fn with_catalog(catalog: ModelCatalog) -> Self {
        Self { catalog }
    }
}

impl Default for BrowserImageService {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait(?Send)]
impl ImageService for BrowserImageService {
    async fn generate(
        &self,
        req: ImageRequest,
        cancel: CancellationToken,
    ) -> Result<ImageResponse, ImageError> {
        if !self.claims_backend(&req.backend_id) {
            return Err(ImageError::InvalidRequest(format!(
                "backend '{}' not claimed by {BACKEND_ID}",
                req.backend_id
            )));
        }

        let body = serde_json::to_string(&req)
            .map_err(|e| ImageError::BackendError(format!("encode request: {e}")))?;
        let request_id = bridge::start_generate(&body).await?;

        loop {
            if cancel.is_cancelled() {
                let _ = bridge::cancel_stream(&request_id).await;
                return Err(ImageError::Cancelled);
            }
            match bridge::next_frame(&request_id).await? {
                Frame::Progress { .. } => {
                    // Generate progress is rapid on SD-Turbo (≤4 steps).
                    // Use load_model() if you want load-time progress.
                    continue;
                }
                Frame::Done { bytes, mime_type } => {
                    return Ok(ImageResponse::new(vec![GeneratedImage::new(
                        bytes, mime_type,
                    )]));
                }
                Frame::Error(msg) => {
                    return Err(ImageError::BackendError(format!("transformers: {msg}")));
                }
            }
        }
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ImageError> {
        Ok(self.catalog.models().to_vec())
    }

    async fn status(&self, backend_id: &str, _model_id: &str) -> Result<ModelStatus, ImageError> {
        if backend_id != BACKEND_ID {
            return Err(ImageError::InvalidRequest(format!(
                "backend '{backend_id}' not claimed by {BACKEND_ID}"
            )));
        }
        // Page-direct engine state isn't visible from the SW. Match the
        // BrowserLlmService policy: return Unloaded; UI tracks real state
        // via the load_model stream.
        Ok(ModelStatus::unloaded())
    }

    fn load_model(
        &self,
        backend_id: &str,
        model_id: &str,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<LoadProgress, ImageError>> {
        if backend_id != BACKEND_ID {
            let id = backend_id.to_string();
            return Box::pin(futures::stream::once(async move {
                Err(ImageError::InvalidRequest(format!(
                    "backend '{id}' not claimed by {BACKEND_ID}"
                )))
            }));
        }
        let model = model_id.to_string();
        let (mut tx, rx) = mpsc::channel::<Result<LoadProgress, ImageError>>(8);
        wasm_bindgen_futures::spawn_local(async move {
            if cancel.is_cancelled() {
                let _ = tx.send(Err(ImageError::Cancelled)).await;
                return;
            }
            // Page-side loadEngine is one round-trip. Intermediate download
            // progress is not exposed yet; emit a single terminal "ready"
            // frame so the UI can flip out of the loading state.
            match bridge::load_engine(&model).await {
                Ok(()) => {
                    let _ = tx.send(Ok(LoadProgress::new("ready"))).await;
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                }
            }
        });
        Box::pin(rx)
    }

    async fn unload_model(&self, backend_id: &str, _model_id: &str) -> Result<(), ImageError> {
        if backend_id != BACKEND_ID {
            return Err(ImageError::InvalidRequest(format!(
                "backend '{backend_id}' not claimed by {BACKEND_ID}"
            )));
        }
        bridge::unload_engine().await
    }

    fn claims_backend(&self, backend_id: &str) -> bool {
        backend_id == BACKEND_ID
    }
}
