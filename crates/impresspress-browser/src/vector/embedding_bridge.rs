//! Typed wrapper over the SW↔page embed RPC.

use wafer_core::interfaces::vector::service::VectorError;

use crate::bridge;

fn js_err(e: wasm_bindgen::JsValue) -> VectorError {
    VectorError::Internal(format!(
        "embed bridge: {}",
        e.as_string().unwrap_or_else(|| format!("{e:?}"))
    ))
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
