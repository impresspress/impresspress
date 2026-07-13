//! `BrowserEmbeddingService` — Transformers.js feature-extraction pipeline,
//! invoked over the SW↔page postMessage bridge.

use wafer_core::interfaces::vector::{
    catalog::get_model,
    service::{EmbeddingService, Result as VResult, VectorError},
};

use crate::vector::embedding_bridge;

pub struct BrowserEmbeddingService {
    model_id: String,
    dimensions: u32,
}

// SAFETY: `BrowserEmbeddingService` only holds owned `String` + `u32`.
// wasm32-unknown-unknown has no threads, so the `Send`/`Sync` bounds
// required by `Arc<dyn EmbeddingService>` are satisfied trivially — no
// cross-thread aliasing or data races are possible.
unsafe impl Send for BrowserEmbeddingService {}
unsafe impl Sync for BrowserEmbeddingService {}

impl BrowserEmbeddingService {
    pub fn new() -> Result<Self, String> {
        Self::with_model("multilingual-e5-small")
    }

    pub fn with_model(model_id: &str) -> Result<Self, String> {
        let info = get_model(model_id).ok_or_else(|| format!("unknown model: {model_id}"))?;
        if !info.runtimes.browser_transformers {
            return Err(format!(
                "model '{model_id}' is not browser-runnable (browser_transformers=false)"
            ));
        }
        Ok(Self {
            model_id: model_id.to_string(),
            dimensions: info.dimensions,
        })
    }
}

// `Default` is intentionally not implemented: constructing the service can
// fail (the model id might be missing from the registry), and a panicking
// `Default::default()` would hide that failure. Use `Self::new()` or
// `Self::with_model(...)` and propagate the `Result`.

#[async_trait::async_trait(?Send)]
impl EmbeddingService for BrowserEmbeddingService {
    fn model(&self) -> &str {
        &self.model_id
    }

    fn dimensions(&self) -> u32 {
        self.dimensions
    }

    async fn embed(&self, texts: Vec<String>) -> VResult<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let vectors = embedding_bridge::run(&self.model_id, &texts).await?;
        for (i, v) in vectors.iter().enumerate() {
            if v.len() as u32 != self.dimensions {
                return Err(VectorError::Internal(format!(
                    "embed[{}]: model returned {} dims, expected {}",
                    i,
                    v.len(),
                    self.dimensions
                )));
            }
        }
        Ok(vectors)
    }
}
