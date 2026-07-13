//! Smallest-possible consumer of `impresspress-browser`. No ImpresspressBuilder;
//! no impresspress-core. Builds a bare Wafer with browser platform services
//! and no registered blocks.
//!
//! Its purpose is to fail-loud at compile time if `impresspress-browser`
//! accidentally grows a dependency on `impresspress` or `impresspress-core`, or if
//! the framework contract requires app-level types that non-impresspress
//! consumers won't have.
//!
//! The wasm-bindgen entrypoints are gated behind `#[cfg(target_arch =
//! "wasm32")]` because they use `impresspress_browser::{db_init, store_wafer,
//! dispatch_request}` which are themselves wasm32-only. Native `cargo test
//! --workspace` compiles this crate as an empty cdylib, which is fine.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub async fn initialize() -> Result<(), JsValue> {
    if impresspress_browser::is_initialized() {
        return Ok(());
    }

    impresspress_browser::db_init().await?;

    let cfg_source: std::sync::Arc<dyn wafer_run::ConfigSource> =
        std::sync::Arc::new(wafer_run::StaticConfigSource::default());
    let mut wafer =
        wafer_run::Wafer::new(cfg_source).map_err(|e| JsValue::from_str(&e.to_string()))?;
    wafer.set_asset_loader(&impresspress_browser::make_sw_asset_loader());
    wafer
        .seal()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?;

    impresspress_browser::store_wafer(wafer).map_err(|e| JsValue::from_str(&e.to_string()))?;

    Ok(())
}

#[wasm_bindgen]
pub async fn handle_request(req: web_sys::Request) -> Result<web_sys::Response, JsValue> {
    impresspress_browser::dispatch_request(req).await
}
