//! Default browser model catalog — WebLLM models with f32 + f16 tiers.

use wafer_core::interfaces::llm::service::{ModelCapabilities, ModelInfo};

/// Browser-side model catalog. Wraps a `Vec<ModelInfo>`.
///
/// Consumers override via `BrowserLlmService::with_catalog(ModelCatalog::new(...))`.
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

fn caps() -> ModelCapabilities {
    ModelCapabilities {
        streaming: true,
        tools: true,
        ..Default::default()
    }
}

fn default_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo::new(
            "webllm",
            "SmolLM2-1.7B-Instruct-q4f32_1-MLC",
            "SmolLM2 1.7B (1.1GB)",
        )
        .with_capabilities(caps()),
        ModelInfo::new(
            "webllm",
            "Qwen2.5-1.5B-Instruct-q4f32_1-MLC",
            "Qwen 2.5 1.5B (1.2GB)",
        )
        .with_capabilities(caps()),
        ModelInfo::new("webllm", "gemma-2-2b-it-q4f32_1-MLC", "Gemma 2 2B (1.7GB)")
            .with_capabilities(caps()),
        ModelInfo::new(
            "webllm",
            "Phi-3.5-mini-instruct-q4f32_1-MLC",
            "Phi 3.5 Mini (2.6GB)",
        )
        .with_capabilities(caps()),
        ModelInfo::new(
            "webllm",
            "Llama-3.2-3B-Instruct-q4f32_1-MLC",
            "Llama 3.2 3B (2GB)",
        )
        .with_capabilities(caps()),
        ModelInfo::new(
            "webllm",
            "SmolLM2-1.7B-Instruct-q4f16_1-MLC",
            "SmolLM2 1.7B f16 (1GB)",
        )
        .with_capabilities(caps()),
        ModelInfo::new(
            "webllm",
            "Qwen2.5-1.5B-Instruct-q4f16_1-MLC",
            "Qwen 2.5 1.5B f16 (1GB)",
        )
        .with_capabilities(caps()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_catalog_has_seven_models() {
        let c = ModelCatalog::default();
        assert_eq!(c.models().len(), 7);
    }

    #[test]
    fn default_catalog_caps_have_tools_enabled() {
        let c = ModelCatalog::default();
        for m in c.models() {
            assert!(
                m.capabilities.tools,
                "model {} missing tool capability",
                m.model_id
            );
            assert!(m.capabilities.streaming);
        }
    }

    #[test]
    fn custom_catalog_overrides_default() {
        let c = ModelCatalog::new(vec![
            ModelInfo::new("webllm", "custom-1", "Custom 1").with_capabilities(caps())
        ]);
        assert_eq!(c.models().len(), 1);
        assert_eq!(c.models()[0].model_id, "custom-1");
    }
}
