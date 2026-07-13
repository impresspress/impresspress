//! Implements `wafer_run::asset_loader::LoadAssetCallback` for the
//! impresspress-web service-worker host by postMessage-bridging to the main
//! thread through `bridge::load_asset`. A main-thread listener does the
//! actual fetch, sha256 verification, and named-loader init; that
//! listener is currently not shipped (ai-bridge.js was removed in the
//! LLM refactor; no block in-tree declares external_assets, so
//! `load_asset` is never invoked today).
//!
//! See `docs/superpowers/specs/2026-04-18-gizza-ai-design.md` §
//! "External asset loading (host side)".

use serde::{Deserialize, Serialize};
use wafer_block::ExternalAsset;
use wafer_run::asset_loader::{AssetLoadError, AssetLoadStatus, LoadAssetCallback};

use crate::bridge;

/// Host-side loader that bridges to the main thread via postMessage.
///
/// Asset → manifest resolution happens lazily per `load()` call by walking
/// the runtime's currently-registered `BlockInfo::external_assets` (via
/// `Wafer::block_infos()`). This lets new blocks added after
/// `SwAssetLoader` is constructed still resolve.
pub struct SwAssetLoader;

impl SwAssetLoader {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SwAssetLoader {
    fn default() -> Self {
        Self::new()
    }
}

/// Wire shape of the manifest passed across the wasm-bindgen boundary.
/// Mirrors `ExternalAsset` exactly — kept as a separate struct so the
/// bridge contract is explicit at the JS edge.
#[derive(Serialize)]
struct ManifestForJs {
    id: String,
    loader: String,
    version: String,
    url: String,
    sha256: String,
}

/// Reply from `bridge::load_asset`. Mirrors the `{status, error?}` JS
/// object the (not-yet-shipped) main-thread listener posts back via `sw.js`.
#[derive(Deserialize)]
struct LoadAssetReply {
    status: String,
    #[serde(default)]
    error: Option<String>,
}

#[async_trait::async_trait(?Send)]
impl LoadAssetCallback for SwAssetLoader {
    async fn load(&self, asset_id: &str) -> AssetLoadStatus {
        let Some(manifest) = lookup_manifest(asset_id) else {
            return AssetLoadStatus::Failed(AssetLoadError::UnknownLoader(format!(
                "no external_asset with id='{asset_id}' in any registered block"
            )));
        };

        let manifest_json = match serde_json::to_string(&ManifestForJs {
            id: manifest.id.clone(),
            loader: manifest.loader.clone(),
            version: manifest.version.clone(),
            url: manifest.url.clone(),
            sha256: manifest.sha256.clone(),
        }) {
            Ok(s) => s,
            Err(e) => {
                return AssetLoadStatus::Failed(AssetLoadError::Unknown(format!(
                    "serialize manifest for asset_id={asset_id}: {e}"
                )));
            }
        };

        let js_result = match bridge::load_asset(asset_id, &manifest_json).await {
            Ok(v) => v,
            Err(e) => {
                return AssetLoadStatus::Failed(AssetLoadError::Unknown(format!(
                    "load_asset rejected for asset_id={asset_id}: {}",
                    bridge::describe(&e)
                )));
            }
        };
        let reply: LoadAssetReply = match serde_wasm_bindgen::from_value(js_result) {
            Ok(r) => r,
            Err(e) => {
                return AssetLoadStatus::Failed(AssetLoadError::Unknown(format!(
                    "deserialize load_asset reply for asset_id={asset_id}: {e}"
                )));
            }
        };

        // The main-thread listener only posts "ready" (on success) or
        // "failed" (on any error). No resumable/pending protocol exists
        // today — if it's added later, add a "pending" arm here.
        match reply.status.as_str() {
            "ready" => AssetLoadStatus::Ready,
            _ => AssetLoadStatus::Failed(AssetLoadError::Unknown(
                reply.error.unwrap_or_else(|| "load-asset failed".into()),
            )),
        }
    }
}

/// Walk every registered block's `external_assets` for one matching `id`.
///
/// The thread_local borrow is held only synchronously inside `with`; we
/// release it before any await. Wafer is single-shared and never replaced
/// after `initialize()`, so this is safe even when called concurrently
/// with `handle_request` (wasm32 is single-threaded — no actual concurrent
/// borrow can race).
fn lookup_manifest(asset_id: &str) -> Option<ExternalAsset> {
    crate::runtime::RUNTIME.with(|r| {
        let borrow = r.borrow();
        let wafer = borrow.as_ref()?;
        for info in wafer.block_infos() {
            for asset in info.external_assets {
                if asset.id == asset_id {
                    return Some(asset);
                }
            }
        }
        None
    })
}

/// Factory: returns an `Arc<dyn LoadAssetCallback>` that bridges `load_asset`
/// calls to the Service Worker via postMessage. Install via
/// `wafer.set_asset_loader(&impresspress_browser::make_sw_asset_loader())`
/// before calling `wafer.seal()`.
pub fn make_sw_asset_loader() -> std::sync::Arc<dyn wafer_run::LoadAssetCallback> {
    std::sync::Arc::new(SwAssetLoader::new())
}
