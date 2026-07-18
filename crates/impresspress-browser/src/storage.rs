use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use wafer_core::interfaces::storage::service::{
    FolderInfo, ListOptions, ObjectInfo, ObjectList, StorageError, StorageService,
};

use crate::bridge;
// Pure, host-testable opaque list-cursor codec (envelope shared with the
// local-storage backend); see `storage_cursor` for the format contract.
use crate::storage_cursor as cursor;

pub struct BrowserStorageService;

// SAFETY: `BrowserStorageService` is a unit struct with no shared state.
// wasm32-unknown-unknown has no threads, so the `Send`/`Sync` bounds
// required by `Arc<dyn StorageService>` are satisfied trivially — no
// cross-thread aliasing or data races are possible.
unsafe impl Send for BrowserStorageService {}
unsafe impl Sync for BrowserStorageService {}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Convert a *resolved* JsValue to a String. The mutating bridge storage
/// calls that still route through `await_bridge` (`put`/`delete`/
/// `create_folder`/`delete_folder`) always resolve `undefined` — they carry
/// no payload. Anything else (including a bare string) would mean bridge.js
/// resolved with something unexpected — surface its message rather than
/// silently losing it (shares `bridge::describe`'s message extraction with
/// the rejection path in `await_bridge` below, so both use the same
/// Error/DOMException `.message` lookup).
///
/// `get`/`list`/`list_folders` resolve structured JS objects/arrays instead
/// and decode them directly with `serde_wasm_bindgen`, bypassing this
/// string-shaped helper entirely — see their own methods below.
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

// ─── Structured shapes decoded from the bridge (serde_wasm_bindgen, not JSON) ─

#[derive(Deserialize)]
struct GetResponse {
    /// Deserializes straight from the JS object's real `Uint8Array` field —
    /// no `Array<number>`/JSON round trip.
    data: Vec<u8>,
    meta: GetMeta,
}

#[derive(Deserialize)]
struct GetMeta {
    content_type: String,
    size: i64,
}

/// `storageList`'s resolved shape: the requested page of keys plus the
/// TRUE total of matching entries (before slicing to the page). `total`
/// drives both the offset-mode has-more check and cursor-mode continuation
/// (`start + page.len() < total`); see [`BrowserStorageService::list`] and
/// the [`cursor`] module.
#[derive(Deserialize)]
struct ListResponse {
    keys: Vec<String>,
    total: i64,
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
        // `storageGet` resolves a plain JS object `{ data: Uint8Array, meta }`
        // (see `bridge::storage_get`'s doc comment) — not a string, so this
        // bypasses `await_bridge`/`jsvalue_to_string` and maps the rejection
        // directly, then decodes the resolved object with
        // `serde_wasm_bindgen` in one step (no JSON round trip either
        // direction).
        let val = bridge::storage_get(folder, key)
            .await
            .map_err(map_rejection)?;

        let resp: GetResponse = serde_wasm_bindgen::from_value(val)
            .map_err(|e| StorageError::Internal(format!("decode storage get response: {e}")))?;

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
        // Cursor pagination takes precedence over offset (wafer-run #318,
        // `ListOptions::cursor`): when a cursor is present we decode it to the
        // resume offset and IGNORE `opts.offset`; otherwise we page by offset
        // exactly as before. An empty cursor (`Some("")`) means "before the
        // first object" and resolves to offset 0 — this begins a cursor walk.
        let cursor_mode = opts.cursor.is_some();
        let start: u64 = match &opts.cursor {
            Some(token) => cursor::decode(token)?,
            None => opts.offset.max(0) as u64,
        };

        let limit = if opts.limit > 0 { opts.limit as u32 } else { 0 };

        // `storageList` resolves `{ keys: string[], total: number }` — not a
        // string — with `total` the full matching-entry count (not the page
        // length; see `bridge::storage_list`'s doc comment). The JS bridge
        // sorts keys before slicing, matching the local-storage backend, so
        // offset/cursor paging is stable across calls.
        let val = bridge::storage_list(folder, &opts.prefix, limit, cursor::clamp_offset(start))
            .await
            .map_err(map_rejection)?;

        let resp: ListResponse = serde_wasm_bindgen::from_value(val)
            .map_err(|e| StorageError::Internal(format!("decode storage list response: {e}")))?;

        // Decide the continuation token BEFORE consuming `resp.keys`. In cursor
        // mode we emit `next_cursor` only when more objects follow this page;
        // offset callers always get `None` (they use `total_count` for
        // has-more), matching the local-storage backend's rule.
        let next_cursor = cursor::next_page_cursor(
            cursor_mode,
            start,
            resp.keys.len() as u64,
            resp.total.max(0) as u64,
        );

        // ObjectInfo fields beyond `key` are not available from the list
        // call; we use placeholder values (size=0, empty content_type,
        // current time).
        let now = Utc::now();
        let objects = resp
            .keys
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
            total_count: resp.total,
            next_cursor,
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
        // `storageListFolders` resolves a plain JS array of strings — not a
        // JSON string.
        let val = bridge::storage_list_folders()
            .await
            .map_err(map_rejection)?;

        // FolderInfo fields beyond `name` are not available; use defaults.
        let names: Vec<String> = serde_wasm_bindgen::from_value(val).map_err(|e| {
            StorageError::Internal(format!("decode storage list-folders response: {e}"))
        })?;

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

    use super::{jsvalue_to_string, map_rejection, GetResponse, ListResponse};

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

    // ── GetResponse decode (the structured `storageGet` shape) ──────────────
    //
    // `bridge::storage_get` used to resolve a JSON string that `get()`
    // re-parsed with `serde_json::from_str`; it now resolves the plain JS
    // object below, decoded in one step with `serde_wasm_bindgen`. These
    // tests exercise exactly the decode step `get()` performs, using the
    // same `Uint8Array`-in-a-plain-object shape `storageGet` in bridge.js
    // actually resolves.

    fn make_get_response_object(data: &[u8], content_type: &str, size: i64) -> JsValue {
        use js_sys::Uint8Array;

        let meta = Object::new();
        Reflect::set(
            &meta,
            &JsValue::from_str("content_type"),
            &JsValue::from_str(content_type),
        )
        .unwrap();
        Reflect::set(
            &meta,
            &JsValue::from_str("size"),
            &JsValue::from_f64(size as f64),
        )
        .unwrap();

        let obj = Object::new();
        Reflect::set(
            &obj,
            &JsValue::from_str("data"),
            &Uint8Array::from(data).into(),
        )
        .unwrap();
        Reflect::set(&obj, &JsValue::from_str("meta"), &meta).unwrap();
        obj.into()
    }

    #[wasm_bindgen_test]
    fn decodes_get_response_with_real_uint8array_in_one_step() {
        let bytes = b"hello world";
        let js_val = make_get_response_object(bytes, "text/plain", bytes.len() as i64);

        let decoded: GetResponse =
            serde_wasm_bindgen::from_value(js_val).expect("decode storage get response");

        assert_eq!(decoded.data, bytes.to_vec());
        assert_eq!(decoded.meta.content_type, "text/plain");
        assert_eq!(decoded.meta.size, bytes.len() as i64);
    }

    #[wasm_bindgen_test]
    fn decodes_get_response_with_empty_data() {
        let js_val = make_get_response_object(&[], "application/octet-stream", 0);

        let decoded: GetResponse =
            serde_wasm_bindgen::from_value(js_val).expect("decode storage get response");

        assert!(decoded.data.is_empty());
        assert_eq!(decoded.meta.size, 0);
    }

    // ── ListResponse decode (the structured `storageList` shape) ────────────
    //
    // Regression guard for the exact bug this task fixes: `storageList` used
    // to resolve a JSON string containing only the page, and the caller
    // reported the page length as the total. `ListResponse` carries a real
    // `total` distinct from `keys.len()` whenever the store has more
    // matching entries than fit on the requested page.

    fn make_list_response_object(keys: &[&str], total: i64) -> JsValue {
        use js_sys::Array;

        let js_keys = Array::new();
        for k in keys {
            js_keys.push(&JsValue::from_str(k));
        }

        let obj = Object::new();
        Reflect::set(&obj, &JsValue::from_str("keys"), &js_keys).unwrap();
        Reflect::set(
            &obj,
            &JsValue::from_str("total"),
            &JsValue::from_f64(total as f64),
        )
        .unwrap();
        obj.into()
    }

    #[wasm_bindgen_test]
    fn decodes_list_response_with_total_larger_than_page() {
        // A page of 2 keys out of 50 total matches — the bug this task
        // fixes reported `total: 2` (the page length) instead of `50`.
        let js_val = make_list_response_object(&["a", "b"], 50);

        let decoded: ListResponse =
            serde_wasm_bindgen::from_value(js_val).expect("decode storage list response");

        assert_eq!(decoded.keys, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(decoded.total, 50);
        assert_ne!(
            decoded.total as usize,
            decoded.keys.len(),
            "total must reflect the full matching-entry count, not the page length"
        );
    }

    #[wasm_bindgen_test]
    fn decodes_list_response_empty_folder() {
        let js_val = make_list_response_object(&[], 0);

        let decoded: ListResponse =
            serde_wasm_bindgen::from_value(js_val).expect("decode storage list response");

        assert!(decoded.keys.is_empty());
        assert_eq!(decoded.total, 0);
    }
}
