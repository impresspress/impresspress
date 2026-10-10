//! Typed wrapper over the SW↔page embed RPC.

use wafer_core::interfaces::vector::service::VectorError;

use crate::bridge;

/// A refusal (no page can run the embedding now) is
/// [`VectorError::EngineUnavailable`] with bridge.js's caller-facing message;
/// anything else is an internal fault.
fn js_err(e: wasm_bindgen::JsValue) -> VectorError {
    match bridge::engine_unavailable(&e) {
        Some(message) => VectorError::EngineUnavailable(message),
        None => VectorError::Internal(format!("embed bridge: {}", bridge::describe(&e))),
    }
}

pub async fn run(model_id: &str, texts: &[String]) -> Result<Vec<Vec<f32>>, VectorError> {
    let texts_json = serde_json::to_string(texts)
        .map_err(|e| VectorError::Internal(format!("encode texts: {e}")))?;
    let v = bridge::embed_run(model_id, &texts_json)
        .await
        .map_err(js_err)?;
    let s = v
        .as_string()
        .ok_or_else(|| VectorError::Internal("embed result not string".into()))?;
    #[derive(serde::Deserialize)]
    struct Out {
        vectors: Vec<Vec<f32>>,
    }
    let out: Out = serde_json::from_str(&s)
        .map_err(|e| VectorError::Internal(format!("parse embed result: {e}")))?;
    Ok(out.vectors)
}
