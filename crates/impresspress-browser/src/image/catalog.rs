//! Default image-model catalog for the browser backend.

use wafer_core::interfaces::image::service::{ModelCapabilities, ModelInfo};

pub const BACKEND_ID: &str = "transformers-image";

#[derive(Debug, Clone)]
pub struct ModelCatalog {
    models: Vec<ModelInfo>,
}

impl ModelCatalog {
    pub fn new(models: Vec<ModelInfo>) -> Self {
        Self { models }
    }

    pub fn models(&self) -> &[ModelInfo] {
        &self.models
    }
}

impl Default for ModelCatalog {
    fn default() -> Self {
        Self::new(default_models())
    }
}

pub fn default_catalog() -> ModelCatalog {
    ModelCatalog::default()
}

fn janus_pro_caps() -> ModelCapabilities {
    // Janus-Pro is autoregressive (not diffusion). Output is 384×384; `steps`
    // / `guidance_scale` / `negative_prompt` are ignored by the engine. The
    // capability flags here describe what the UI should expose; we set
    // `supports_negative_prompt = false` and leave `max_steps = None` because
    // those knobs don't apply to the autoregressive token loop.
    ModelCapabilities {
        max_width: Some(384),
        max_height: Some(384),
        supports_negative_prompt: false,
        max_steps: None,
    }
}

fn default_models() -> Vec<ModelInfo> {
    vec![ModelInfo::new(
        BACKEND_ID,
        "onnx-community/Janus-Pro-1B-ONNX",
        "Janus-Pro 1B (≈700 MB – 1.5 GB)",
    )
    .with_capabilities(janus_pro_caps())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_catalog_has_janus_pro() {
        let cat = default_catalog();
        assert_eq!(cat.models().len(), 1);
        assert_eq!(cat.models()[0].model_id, "onnx-community/Janus-Pro-1B-ONNX");
        assert_eq!(cat.models()[0].backend_id, BACKEND_ID);
        assert_eq!(cat.models()[0].capabilities.max_width, Some(384));
        // Janus is autoregressive: no negative prompt, no step count.
        assert!(!cat.models()[0].capabilities.supports_negative_prompt);
        assert_eq!(cat.models()[0].capabilities.max_steps, None);
    }
}
