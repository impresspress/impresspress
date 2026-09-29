use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use wafer_block::{InputStream, OutputStream};
use wafer_core::interfaces::storage::service::{
    FolderInfo, ListOptions, ObjectInfo, ObjectList, StorageError, StorageService,
};
use wafer_run::{ErrorCode, WaferError};
use wasm_bindgen::JsCast;

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
    /// One bulk copy out of the JS object's real `Uint8Array` field. The
    /// `serde_bytes` adapter is what routes this through
    /// `serde_wasm_bindgen`'s `deserialize_byte_buf`; a bare `Vec<u8>` goes
    /// through `deserialize_seq` instead and crosses the boundary once per
    /// byte (see `BridgeReader::next_chunk`).
    #[serde(with = "serde_bytes")]
    data: Vec<u8>,
    meta: GetMeta,
}

#[derive(Deserialize)]
struct GetMeta {
    content_type: String,
    size: i64,
}

/// `storageGetStream`'s resolved shape: the metadata eagerly, plus the id of
/// the registered byte reader carrying the body.
#[derive(Deserialize)]
struct GetStreamStart {
    stream_id: String,
    meta: GetMeta,
}

/// Where [`drain_into`] pulls bytes from.
///
/// A trait rather than the bridge call inlined, because every rule the drain
/// loop enforces — the running cap, an `Error` terminal instead of a silent
/// truncation, releasing the source when the consumer stops early — is
/// unreachable from a `wasm_bindgen_test` otherwise: the one production
/// implementation is a `bridge.js` reader id backed by a real OPFS file handle
/// or a live HTTP connection, and `wasm-pack test --node` has neither.
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub(crate) trait ChunkSource {
    /// The next chunk, or `Ok(None)` at end of stream. An `Err` is terminal:
    /// [`drain_into`] reports it and releases the source.
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String>;

    /// Release the underlying resource. Idempotent, and cannot fail — it is
    /// called from paths that already have an error to report.
    async fn release(&mut self);
}

/// The production [`ChunkSource`]: a byte reader registered by `bridge.js`
/// (`storageGetStream`, `httpFetchStream`), pulled by id.
struct BridgeReader {
    stream_id: String,
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl ChunkSource for BridgeReader {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        let value = bridge::reader_next_chunk(&self.stream_id)
            .await
            .map_err(|e| bridge::describe(&e))?;

        // `null` — and only `null` — is end of stream. An unknown id rejects
        // above rather than answering `null`, so a bookkeeping bug cannot
        // present as a cleanly-finished body.
        if value.is_null() || value.is_undefined() {
            return Ok(None);
        }

        // Taken off the `Uint8Array` directly (one bulk copy) rather than
        // through `serde_wasm_bindgen`, whose `Vec<u8>` impl always routes
        // through `deserialize_seq`: its fast path covers a JavaScript `Array`
        // only, and the bulk path lives in `deserialize_byte_buf`, which
        // `Vec<u8>` never reaches. A byte array therefore fell back to
        // iterating element by element, allocating an iterator-result object
        // per byte — about 65,536 boundary crossings per 64 KiB chunk.
        match value.dyn_ref::<js_sys::Uint8Array>() {
            Some(bytes) => Ok(Some(bytes.to_vec())),
            None => Err(format!(
                "expected a Uint8Array chunk, got {}",
                bridge::describe(&value)
            )),
        }
    }

    async fn release(&mut self) {
        bridge::reader_cancel(&self.stream_id).await;
    }
}

/// Drain a registered byte reader into `sink`, chunk by chunk.
///
/// Shared by `get_streaming` here and `network::do_request_streaming`, because
/// the loop is the same and the two ways of getting it wrong are the same: an
/// unknown-id rejection must not be mistaken for end-of-stream, and a consumer
/// that stops early must release the reader instead of leaving it holding an
/// OPFS file handle (or an HTTP connection) for the life of the Service
/// Worker.
pub(crate) async fn drain_reader_into(
    stream_id: String,
    cap: Option<usize>,
    what: &str,
    sink: wafer_block::OutputSink,
    cancel: tokio_util::sync::CancellationToken,
) {
    drain_into(BridgeReader { stream_id }, cap, what, sink, cancel).await
}

/// Pump `source` into `sink` until it ends, breaches `cap`, fails, or the
/// consumer goes away — releasing `source` on every path but the clean one,
/// which releases itself when the source reports end of stream.
///
/// A read failure surfaces as an `Error` terminal AFTER the bytes already
/// forwarded — never a silent truncation reported as a clean `Complete`.
/// `cap` bounds the running total; `None` means no cap.
pub(crate) async fn drain_into<S: ChunkSource>(
    mut source: S,
    cap: Option<usize>,
    what: &str,
    sink: wafer_block::OutputSink,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut received: usize = 0;
    loop {
        // Race the pull against cancellation so a dropped consumer stops the
        // read promptly rather than after the next chunk resolves.
        let pulled = cancel.run_until_cancelled(source.next_chunk()).await;
        let Some(next) = pulled else {
            source.release().await;
            return;
        };

        let chunk = match next {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(e) => {
                let _ = sink
                    .error(WaferError::new(ErrorCode::Internal, format!("{what}: {e}")))
                    .await;
                // The JS side already dropped the entry on a read rejection,
                // and `readerCancel` is idempotent, so this is safe either way.
                source.release().await;
                return;
            }
        };

        if let Some(cap) = cap {
            received = received.saturating_add(chunk.len());
            if received > cap {
                let _ = sink
                    .error(WaferError::new(
                        ErrorCode::Unavailable,
                        format!("{what}: body exceeds cap of {cap} bytes"),
                    ))
                    .await;
                source.release().await;
                return;
            }
        }

        if sink.send_chunk(chunk).await.is_err() {
            // Consumer dropped the stream — stop reading and release the
            // source.
            source.release().await;
            return;
        }
    }
    let _ = sink.complete(vec![]).await;
}

/// Release the byte reader `bridge.js` registered inside `value` when `value`
/// itself fails to decode.
///
/// The id is *inside* the value, so a failed decode would otherwise strand a
/// reader that can be neither drained nor cancelled — an OPFS file handle or
/// an HTTP connection held for the life of the Service Worker, which is
/// exactly what `bridge.js`'s reader registry says must never happen. Best
/// effort by construction: if the id is not there either, there is nothing to
/// release.
pub(crate) async fn release_reader_in(value: &wasm_bindgen::JsValue) {
    if let Ok(id) = js_sys::Reflect::get(value, &wasm_bindgen::JsValue::from_str("stream_id")) {
        if let Some(id) = id.as_string() {
            bridge::reader_cancel(&id).await;
        }
    }
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

    /// Streams the object out of OPFS chunk by chunk instead of taking the
    /// trait default, which calls [`get`](Self::get) and wraps the whole
    /// buffered body as one chunk.
    ///
    /// That default is what ran here before, silently: a download served by
    /// `blocks::files` (`storage/objects.rs`, `share.rs`) went through
    /// `get_stream`, which reaches this method, so every object was read whole
    /// into the Service Worker's linear memory — the same memory the entire
    /// runtime, sql.js included, is sharing — before the first byte reached
    /// the client. `File.stream()` is a real `ReadableStream`, so there is no
    /// reason for this target to be the buffered one.
    ///
    /// `ObjectInfo.size` and the streamed bytes come from the SAME `File`
    /// snapshot (`storageGetStream` takes it once), so a concurrent writer
    /// cannot make the advertised length disagree with the bytes that arrive.
    ///
    /// A read failure surfaces as an `Error` terminal after whatever already
    /// streamed; a dropped consumer releases the OPFS reader.
    async fn get_streaming(
        &self,
        folder: &str,
        key: &str,
    ) -> Result<(OutputStream, ObjectInfo), StorageError> {
        let val = bridge::storage_get_stream(folder, key)
            .await
            .map_err(map_rejection)?;

        let started: GetStreamStart = match serde_wasm_bindgen::from_value(val.clone()) {
            Ok(started) => started,
            Err(e) => {
                // The reader id is inside the value that just failed to decode,
                // so without this the OPFS file handle it holds could be
                // neither drained nor cancelled for the life of the Service
                // Worker. Reachable: a metadata sidecar that parses as JSON but
                // carries no `content_type` fails this decode.
                release_reader_in(&val).await;
                return Err(StorageError::Internal(format!(
                    "decode storage get-stream response: {e}"
                )));
            }
        };

        let info = ObjectInfo {
            key: key.to_string(),
            size: started.meta.size,
            content_type: started.meta.content_type,
            // Parity with the buffered `get`, which does not read the OPFS
            // file's own timestamp either.
            last_modified: Utc::now(),
        };

        let what = format!("read {folder}/{key}");
        let stream = OutputStream::from_producer(move |sink, cancel| async move {
            // No cap: the caller asked to stream precisely so the object need
            // not fit in memory, and the object is local storage the operator
            // already owns.
            drain_reader_into(started.stream_id, None, &what, sink, cancel).await;
        });

        Ok((stream, info))
    }

    /// Writes the object incrementally instead of taking the trait default,
    /// which collects the whole `InputStream` to a `Vec` and forwards to
    /// [`put`](Self::put) — the buffering the streaming request path exists to
    /// avoid, and worse here than on a server because the buffer shares one
    /// linear memory with the runtime.
    ///
    /// The OPFS writable holds an exclusive lock on the file, so every path
    /// out of a started write ends in a `finish` or an abort. A body that
    /// fails part-way (an `Err` item) is one of those paths: it is aborted and
    /// answered [`StorageError::Body`], never finished as the prefix that
    /// arrived.
    ///
    /// What an interrupted upload leaves behind, precisely — OPFS has no
    /// multi-file transaction, so this is a statement about two files, not one:
    ///
    /// - A NEW key that is aborted, or whose `close()` fails (which is where a
    ///   quota failure surfaces, because `close` is what commits the swap
    ///   file), leaves nothing: `storagePutStreamAbort` removes the target file
    ///   it created along with any sidecar. The key is not listed and not
    ///   gettable.
    /// - An OVERWRITE that is aborted leaves the previous object and its
    ///   sidecar untouched: `createWritable()` writes to a swap file, so the
    ///   original bytes are only replaced by a successful `close()`.
    /// - If `close()` succeeds and the sidecar write then fails, the new body
    ///   IS committed while this call returns an error. For a new key the body
    ///   is removed again; for an overwrite the new bytes stay under the
    ///   previous sidecar, so the content type can be stale until the object is
    ///   rewritten. `get`/`get_streaming` both take the object's real length
    ///   from the file rather than the sidecar, so the size cannot disagree.
    async fn put_streaming(
        &self,
        folder: &str,
        key: &str,
        data: InputStream,
        content_type: &str,
    ) -> Result<(), StorageError> {
        use futures::StreamExt;

        let id = bridge::storage_put_stream_start(folder, key)
            .await
            .map_err(map_rejection)?;
        // No abort here, deliberately: the writer id IS the only handle to the
        // open writable, so a resolved value that is not a string leaves
        // nothing to abort with. `storagePutStreamStart` resolves a string by
        // construction and `js/test/storage_stream.test.mjs` pins that, which
        // is where the invariant belongs — this arm exists so a bridge that
        // broke it fails loudly instead of writing to a `JsValue::UNDEFINED`
        // id.
        let id = id.as_string().ok_or_else(|| {
            StorageError::Internal("storagePutStreamStart did not resolve a writer id".to_string())
        })?;

        let mut data = Box::pin(data);
        while let Some(chunk) = data.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(e) => {
                    bridge::storage_put_stream_abort(&id).await;
                    return Err(StorageError::Body(e));
                }
            };
            if let Err(e) = bridge::storage_put_stream_chunk(&id, &chunk).await {
                bridge::storage_put_stream_abort(&id).await;
                return Err(map_rejection(e));
            }
        }

        if let Err(e) = bridge::storage_put_stream_finish(&id, content_type).await {
            bridge::storage_put_stream_abort(&id).await;
            return Err(map_rejection(e));
        }
        Ok(())
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

/// The drain loop that both streaming reads share, driven against a scripted
/// [`ChunkSource`] instead of a `bridge.js` reader id.
///
/// The rules under test are the ones a real OPFS file or HTTP connection makes
/// unreachable from a test: the running byte cap (the only guard a
/// chunked/unknown-length response has, and one of the two enforcement points
/// for `MAX_NETWORK_RESPONSE_BYTES`), an `Error` terminal AFTER the bytes
/// already forwarded rather than a silent truncation reported as `Complete`,
/// and releasing the source on every path that stops early.
#[cfg(all(test, target_arch = "wasm32"))]
mod drain_loop {
    use std::{cell::Cell, rc::Rc};

    use async_trait::async_trait;
    use futures::StreamExt;
    use wafer_block::{OutputStream, StreamEvent};
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::{drain_into, ChunkSource};

    /// A scripted source: each `next_chunk` pops the front of `script`.
    struct Scripted {
        script: Vec<Result<Option<Vec<u8>>, String>>,
        at: usize,
        released: Rc<Cell<u32>>,
    }

    impl Scripted {
        fn new(script: Vec<Result<Option<Vec<u8>>, String>>) -> (Self, Rc<Cell<u32>>) {
            let released = Rc::new(Cell::new(0));
            (
                Self {
                    script,
                    at: 0,
                    released: released.clone(),
                },
                released,
            )
        }
    }

    #[async_trait(?Send)]
    impl ChunkSource for Scripted {
        async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
            let next = self
                .script
                .get(self.at)
                .cloned()
                .unwrap_or_else(|| panic!("drain pulled past the end of the script"));
            self.at += 1;
            next
        }

        async fn release(&mut self) {
            self.released.set(self.released.get() + 1);
        }
    }

    fn chunks(events: &[StreamEvent]) -> Vec<Vec<u8>> {
        events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Chunk(bytes) => Some(bytes.clone()),
                _ => None,
            })
            .collect()
    }

    /// Run the drain to completion, then collect what the consumer sees. The
    /// channel is wider than any script here, so the drain never blocks on it.
    async fn run(
        script: Vec<Result<Option<Vec<u8>>, String>>,
        cap: Option<usize>,
    ) -> (Vec<StreamEvent>, u32) {
        let (source, released) = Scripted::new(script);
        let (stream, sink, cancel) = OutputStream::new_streaming_with_capacity(32);
        drain_into(source, cap, "test read", sink, cancel).await;
        (stream.collect::<Vec<_>>().await, released.get())
    }

    #[wasm_bindgen_test]
    async fn a_clean_stream_forwards_every_chunk_and_completes() {
        let (events, released) = run(
            vec![Ok(Some(vec![1, 2])), Ok(Some(vec![3])), Ok(None)],
            None,
        )
        .await;

        assert_eq!(chunks(&events), vec![vec![1, 2], vec![3]]);
        assert!(matches!(events.last(), Some(StreamEvent::Complete { .. })));
        assert_eq!(
            released, 0,
            "a source that reported end of stream has already released itself"
        );
    }

    #[wasm_bindgen_test]
    async fn a_read_failure_is_an_error_terminal_after_the_bytes_already_sent() {
        let (events, released) = run(
            vec![Ok(Some(vec![1, 2])), Err("connection reset".to_string())],
            None,
        )
        .await;

        assert_eq!(chunks(&events), vec![vec![1, 2]]);
        match events.last() {
            Some(StreamEvent::Error(e)) => assert!(
                e.message.contains("connection reset"),
                "the underlying failure must survive into the terminal: {}",
                e.message
            ),
            other => panic!("a truncated body must not report a clean finish: {other:?}"),
        }
        assert_eq!(released, 1, "the reader was left holding its source");
    }

    /// The running total is the ONLY cap a chunked response has: it advertises
    /// no length, so `advertised_length_over_cap` never fires for it.
    #[wasm_bindgen_test]
    async fn the_running_total_stops_a_body_that_grows_past_the_cap() {
        let (events, released) = run(
            vec![
                Ok(Some(vec![0; 4])),
                Ok(Some(vec![0; 4])),
                Ok(Some(vec![0; 4])),
                Ok(None),
            ],
            Some(10),
        )
        .await;

        assert_eq!(
            chunks(&events).len(),
            2,
            "the chunk that breached the cap must not be forwarded"
        );
        match events.last() {
            Some(StreamEvent::Error(e)) => assert!(
                e.message.contains("exceeds cap of 10 bytes"),
                "unexpected terminal message: {}",
                e.message
            ),
            other => panic!("expected an Error terminal, got {other:?}"),
        }
        assert_eq!(released, 1);
    }

    #[wasm_bindgen_test]
    async fn a_body_exactly_at_the_cap_is_delivered_whole() {
        let (events, released) = run(vec![Ok(Some(vec![0; 10])), Ok(None)], Some(10)).await;

        assert_eq!(chunks(&events), vec![vec![0; 10]]);
        assert!(matches!(events.last(), Some(StreamEvent::Complete { .. })));
        assert_eq!(released, 0);
    }

    /// A dropped consumer must release the source rather than leave it holding
    /// an OPFS file handle or an HTTP connection for the life of the Service
    /// Worker.
    #[wasm_bindgen_test]
    async fn a_cancelled_consumer_releases_the_source() {
        let (source, released) = Scripted::new(vec![Ok(Some(vec![1]))]);
        let (_stream, sink, cancel) = OutputStream::new_streaming_with_capacity(4);
        cancel.cancel();

        drain_into(source, None, "test read", sink, cancel).await;

        assert_eq!(released.get(), 1);
    }

    #[wasm_bindgen_test]
    async fn a_vanished_consumer_releases_the_source() {
        let (source, released) = Scripted::new(vec![Ok(Some(vec![1])), Ok(Some(vec![2]))]);
        let (stream, sink, cancel) = OutputStream::new_streaming_with_capacity(4);
        drop(stream);

        drain_into(source, None, "test read", sink, cancel).await;

        assert_eq!(released.get(), 1);
    }
}
