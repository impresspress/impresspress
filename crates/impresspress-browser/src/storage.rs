use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use wafer_core::interfaces::storage::service::{
    FolderInfo, ListOptions, ObjectInfo, ObjectList, StorageError, StorageService,
};

use crate::bridge;

pub struct BrowserStorageService;

// SAFETY: `BrowserStorageService` is a unit struct with no shared state.
// wasm32-unknown-unknown has no threads, so the `Send`/`Sync` bounds
// required by `Arc<dyn StorageService>` are satisfied trivially — no
// cross-thread aliasing or data races are possible.
unsafe impl Send for BrowserStorageService {}
unsafe impl Sync for BrowserStorageService {}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Convert a *resolved* JsValue to a String. Bridge storage calls resolve
/// either `undefined` (mutating calls with no payload) or a plain/JSON
/// string. Anything else would mean bridge.js resolved instead of rejected
/// with something unexpected — surface its message rather than silently
/// losing it (shares `bridge::describe`'s message extraction with the
/// rejection path in `await_bridge` below, so both use the same
/// Error/DOMException `.message` lookup).
fn jsvalue_to_string(val: wasm_bindgen::JsValue) -> Result<String, StorageError> {
    if val.is_null() || val.is_undefined() {
        return Ok(String::new());
    }
    match val.as_string() {
        Some(s) => Ok(s),
        None => Err(StorageError::Internal(bridge::describe(&val))),
    }
}

/// Map a rejected bridge JsValue to a typed `StorageError`. DOMException
/// `NotFoundError` — thrown by OPFS `getFileHandle`/`getDirectoryHandle`/
/// `removeEntry` when the requested folder or key doesn't exist — maps to
/// `StorageError::NotFound`; every other rejection (quota errors,
/// permission errors, etc.) collapses to `StorageError::Internal` carrying
/// the JS error's message.
///
/// Pulled out as a pure function (rather than inlined in `await_bridge`) so
/// the DOMException-name mapping can be exercised directly in a
/// `wasm_bindgen_test` without needing a real OPFS rejection — see the
/// `tests` module below.
fn map_rejection(err: wasm_bindgen::JsValue) -> StorageError {
    if bridge::error_name(&err).as_deref() == Some("NotFoundError") {
        StorageError::NotFound
    } else {
        StorageError::Internal(bridge::describe(&err))
    }
}

/// Await a bridge future, mapping a rejected JS promise to a typed
/// `StorageError` instead of letting wasm-bindgen panic the Service Worker
/// (the storage externs in `bridge.rs` are `#[wasm_bindgen(catch)]`).
async fn await_bridge(
    future: impl std::future::Future<Output = Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>>,
) -> Result<String, StorageError> {
    match future.await {
        Ok(val) => jsvalue_to_string(val),
        Err(err) => Err(map_rejection(err)),
    }
}

// ─── JSON shapes returned by the bridge ──────────────────────────────────────

#[derive(Deserialize)]
struct GetResponse {
    data: Vec<u8>,
    meta: GetMeta,
}

#[derive(Deserialize)]
struct GetMeta {
    content_type: String,
    size: i64,
}

// ─── StorageService impl ──────────────────────────────────────────────────────

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl StorageService for BrowserStorageService {
    async fn put(
        &self,
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<(), StorageError> {
        await_bridge(bridge::storage_put(folder, key, data, content_type))
            .await
            .map(|_| ())
    }

    async fn get(&self, folder: &str, key: &str) -> Result<(Vec<u8>, ObjectInfo), StorageError> {
        // `await_bridge` already maps a missing folder/key to
        // `StorageError::NotFound` via the OPFS `NotFoundError` DOMException
        // rejection (see its doc comment) — bridge.js's `storageGet` never
        // resolves an empty string on success, it always resolves a JSON
        // payload, so there is no separate empty-string case to guard here.
        let json = await_bridge(bridge::storage_get(folder, key)).await?;

        let resp: GetResponse = serde_json::from_str(&json)
            .map_err(|e| StorageError::Internal(format!("parse error: {e}")))?;

        let info = ObjectInfo {
            key: key.to_string(),
            size: resp.meta.size,
            content_type: resp.meta.content_type,
            last_modified: Utc::now(),
        };

        Ok((resp.data, info))
    }

    async fn delete(&self, folder: &str, key: &str) -> Result<(), StorageError> {
        await_bridge(bridge::storage_delete(folder, key))
            .await
            .map(|_| ())
    }

    async fn list(&self, folder: &str, opts: &ListOptions) -> Result<ObjectList, StorageError> {
        let limit = if opts.limit > 0 { opts.limit as u32 } else { 0 };
        let offset = opts.offset as u32;

        let json = await_bridge(bridge::storage_list(folder, &opts.prefix, limit, offset)).await?;

        // Bridge returns a JSON array of key strings.
        // ObjectInfo fields beyond `key` are not available from the list call;
        // we use placeholder values (size=0, empty content_type, current time).
        let keys: Vec<String> = serde_json::from_str(&json)
            .map_err(|e| StorageError::Internal(format!("parse error: {e}")))?;

        let now = Utc::now();
        let total_count = keys.len() as i64;
        let objects = keys
            .into_iter()
            .map(|k| ObjectInfo {
                key: k,
                size: 0,
                content_type: String::new(),
                last_modified: now,
            })
            .collect();

        Ok(ObjectList {
            objects,
            total_count,
        })
    }

    async fn create_folder(&self, name: &str, _public: bool) -> Result<(), StorageError> {
        // OPFS has no concept of "public" folders; the flag is ignored.
        await_bridge(bridge::storage_create_folder(name))
            .await
            .map(|_| ())
    }

    async fn delete_folder(&self, name: &str) -> Result<(), StorageError> {
        await_bridge(bridge::storage_delete_folder(name))
            .await
            .map(|_| ())
    }

    async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        let json = await_bridge(bridge::storage_list_folders()).await?;

        // Bridge returns a JSON array of folder name strings.
        // FolderInfo fields beyond `name` are not available; use defaults.
        let names: Vec<String> = serde_json::from_str(&json)
            .map_err(|e| StorageError::Internal(format!("parse error: {e}")))?;

        let now = Utc::now();
        let folders = names
            .into_iter()
            .map(|n| FolderInfo {
                name: n,
                public: false,
                created_at: now,
            })
            .collect();

        Ok(folders)
    }
}

pub fn make_storage_service(
) -> std::sync::Arc<dyn wafer_core::interfaces::storage::service::StorageService> {
    std::sync::Arc::new(BrowserStorageService)
}

// `bridge::storage_*` are `#[wasm_bindgen(module = "/js/bridge.js")]` externs,
// so `BrowserStorageService`'s trait methods (which call them through
// `await_bridge`) can't be exercised outside a real Service Worker/page
// context — the module path doesn't resolve under `wasm-pack test`. What CAN
// be verified in isolation — and is exactly what this task's `catch` fix
// makes reachable for the first time (a rejection used to panic before ever
// reaching this mapping) — is `map_rejection`: does an OPFS `NotFoundError`
// DOMException map to `StorageError::NotFound`, and does every other
// rejection carry its message through as `StorageError::Internal`.
#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use js_sys::{Object, Reflect};
    use wafer_core::interfaces::storage::service::StorageError;
    use wasm_bindgen::JsValue;
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{jsvalue_to_string, map_rejection};

    /// Build a JS object shaped like a rejected `DOMException`/`Error`:
    /// `{ name, message }`. This is exactly what OPFS's
    /// `getFileHandle`/`getDirectoryHandle`/`removeEntry` reject with when
    /// the requested folder or key doesn't exist (`name: "NotFoundError"`),
    /// and what bridge.js's other OPFS calls reject with on any other
    /// failure (e.g. `name: "QuotaExceededError"`).
    fn make_dom_exception(name: &str, message: &str) -> JsValue {
        let obj = Object::new();
        Reflect::set(&obj, &JsValue::from_str("name"), &JsValue::from_str(name)).unwrap();
        Reflect::set(
            &obj,
            &JsValue::from_str("message"),
            &JsValue::from_str(message),
        )
        .unwrap();
        obj.into()
    }

    #[wasm_bindgen_test]
    fn not_found_dom_exception_maps_to_storage_not_found() {
        let err = make_dom_exception("NotFoundError", "a file or directory could not be found");
        match map_rejection(err) {
            StorageError::NotFound => {}
            other => panic!("expected StorageError::NotFound, got {other:?}"),
        }
    }

    #[wasm_bindgen_test]
    fn other_dom_exception_maps_to_storage_internal_with_message() {
        let err = make_dom_exception("QuotaExceededError", "the quota has been exceeded");
        match map_rejection(err) {
            StorageError::Internal(msg) => {
                assert_eq!(msg, "the quota has been exceeded");
            }
            other => panic!("expected StorageError::Internal, got {other:?}"),
        }
    }

    #[wasm_bindgen_test]
    fn plain_thrown_string_maps_to_storage_internal_via_fallback() {
        // Not every rejection is an Error/DOMException — a JS caller can
        // reject/throw a bare string. `describe` falls back to the value
        // itself when there's no `.message`.
        let err = JsValue::from_str("boom");
        match map_rejection(err) {
            StorageError::Internal(msg) => assert_eq!(msg, "boom"),
            other => panic!("expected StorageError::Internal, got {other:?}"),
        }
    }

    #[wasm_bindgen_test]
    fn resolved_null_or_undefined_is_empty_string() {
        assert_eq!(jsvalue_to_string(JsValue::NULL).unwrap(), "");
        assert_eq!(jsvalue_to_string(JsValue::UNDEFINED).unwrap(), "");
    }

    #[wasm_bindgen_test]
    fn resolved_string_passes_through() {
        assert_eq!(
            jsvalue_to_string(JsValue::from_str("hello")).unwrap(),
            "hello"
        );
    }
}
