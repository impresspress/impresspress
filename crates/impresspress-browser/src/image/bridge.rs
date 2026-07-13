//! Rust-side wrapper over the SW↔page image postMessage bridge.
//!
//! Glue around the `image*` functions in `js/bridge.js`. Turns the async
//! postMessage exchanges into typed Rust calls. Not testable in native —
//! exercised via the `BrowserImageService` integration and the browser smoke
//! test.

use base64ct::{Base64, Encoding};
use wafer_core::interfaces::image::service::ImageError;

use crate::bridge::{
    image_cancel_stream, image_load_engine, image_next_frame, image_start_generate,
    image_unload_engine,
};

fn js_err(e: wasm_bindgen::JsValue) -> ImageError {
    ImageError::BackendError(format!(
        "image bridge: {}",
        e.as_string().unwrap_or_else(|| format!("{e:?}"))
    ))
}

pub async fn load_engine(model_id: &str) -> Result<(), ImageError> {
    image_load_engine(model_id)
        .await
        .map(|_| ())
        .map_err(js_err)
}

pub async fn unload_engine() -> Result<(), ImageError> {
    image_unload_engine().await.map(|_| ()).map_err(js_err)
}

pub async fn start_generate(body_json: &str) -> Result<String, ImageError> {
    let v = image_start_generate(body_json).await.map_err(js_err)?;
    v.as_string()
        .ok_or_else(|| ImageError::BackendError("image bridge: request id not a string".into()))
}

pub enum Frame {
    Progress {
        stage: String,
        bytes_downloaded: Option<u64>,
        bytes_total: Option<u64>,
    },
    Done {
        bytes: Vec<u8>,
        mime_type: String,
    },
    Error(String),
}

pub async fn next_frame(request_id: &str) -> Result<Frame, ImageError> {
    let v = image_next_frame(request_id).await.map_err(js_err)?;
    let s = v
        .as_string()
        .ok_or_else(|| ImageError::BackendError("image bridge: frame not a string".into()))?;
    let frame: serde_json::Value = serde_json::from_str(&s)
        .map_err(|e| ImageError::BackendError(format!("image bridge: frame parse: {e}")))?;
    let kind = frame.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let payload = frame.get("payload");
    match kind {
        "progress" => {
            let stage = payload
                .and_then(|p| p.get("stage"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let bytes_downloaded = payload
                .and_then(|p| p.get("bytes_downloaded"))
                .and_then(|v| v.as_u64());
            let bytes_total = payload
                .and_then(|p| p.get("bytes_total"))
                .and_then(|v| v.as_u64());
            Ok(Frame::Progress {
                stage,
                bytes_downloaded,
                bytes_total,
            })
        }
        "done" => {
            let data_b64 = payload
                .and_then(|p| p.get("data"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ImageError::BackendError("image bridge: done frame missing data".into())
                })?;
            let bytes = Base64::decode_vec(data_b64)
                .map_err(|e| ImageError::BackendError(format!("image bridge: base64: {e}")))?;
            let mime_type = payload
                .and_then(|p| p.get("mime_type"))
                .and_then(|v| v.as_str())
                .unwrap_or("image/png")
                .to_string();
            Ok(Frame::Done { bytes, mime_type })
        }
        "error" => Ok(Frame::Error(
            payload
                .and_then(|p| p.as_str())
                .unwrap_or("unknown")
                .to_string(),
        )),
        other => Err(ImageError::BackendError(format!(
            "image bridge: unknown frame kind '{other}'"
        ))),
    }
}

pub async fn cancel_stream(request_id: &str) -> Result<(), ImageError> {
    image_cancel_stream(request_id)
        .await
        .map(|_| ())
        .map_err(js_err)
}
