use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/js/bridge.js")]
extern "C" {
    // ─── Database (sql.js) ────────────────────────────────────────────────────

    /// Load sql.js WASM, try to load existing DB from OPFS, create new if none.
    /// Sets PRAGMA foreign_keys=ON. Rejects if the sql.js WASM fails to load
    /// or OPFS is unavailable.
    #[wasm_bindgen(catch)]
    pub async fn dbInit() -> Result<JsValue, JsValue>;

    /// Execute SQL that modifies data (INSERT/UPDATE/DELETE/DDL).
    /// `params` is a structured JS array of bind values — build via
    /// `db_codec::params_to_js`/`empty_params`, not a JSON string. Returns
    /// the rows-modified count. Throws on SQL error. Does NOT flush to
    /// OPFS — see `BrowserDatabaseService::with_flush` in `database.rs` for
    /// the coalesced-flush durability contract.
    #[wasm_bindgen(catch, js_name = dbExecRaw)]
    pub fn db_exec_raw(sql: &str, params: JsValue) -> Result<f64, JsValue>;

    /// Execute a SELECT SQL query. `params` as above.
    /// Returns a JS array of plain row objects — NOT a JSON string. Decode
    /// with `db_codec::parse_rows`/`rows_from_js`
    /// (`serde_wasm_bindgen::from_value`).
    #[wasm_bindgen(catch, js_name = dbQueryRaw)]
    pub fn db_query_raw(sql: &str, params: JsValue) -> Result<JsValue, JsValue>;

    /// Export the sql.js DB to OPFS at `impresspress.db`. Rejects on an OPFS
    /// write failure (e.g. `QuotaExceededError`).
    #[wasm_bindgen(catch)]
    pub async fn dbFlush() -> Result<JsValue, JsValue>;

    // ─── Storage (OPFS) ───────────────────────────────────────────────────────

    /// Write file + metadata to OPFS.
    #[wasm_bindgen(catch, js_name = storagePut)]
    pub async fn storage_put(
        folder: &str,
        key: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<JsValue, JsValue>;

    /// Read file + metadata from OPFS.
    /// Returns a plain JS object `{ data: Uint8Array, meta: { content_type,
    /// size } }` — NOT a JSON string. Decode directly with
    /// `serde_wasm_bindgen::from_value` (`data` deserializes straight into a
    /// `Vec<u8>` from the real `Uint8Array`, matching `network.rs`'s
    /// `FetchResponse.body` pattern). Rejects with a `NotFoundError`
    /// `DOMException` if the folder or key doesn't exist.
    #[wasm_bindgen(catch, js_name = storageGet)]
    pub async fn storage_get(folder: &str, key: &str) -> Result<JsValue, JsValue>;

    /// Delete file + metadata from OPFS. Rejects with a `NotFoundError`
    /// `DOMException` if the folder or key doesn't exist.
    #[wasm_bindgen(catch, js_name = storageDelete)]
    pub async fn storage_delete(folder: &str, key: &str) -> Result<JsValue, JsValue>;

    /// List files in a folder matching a prefix, with pagination.
    /// Returns a plain JS object `{ keys: string[], total: number }` — NOT a
    /// JSON string. `total` is the full matching-entry count, not the page
    /// length. Decode with `serde_wasm_bindgen::from_value`. Rejects with a
    /// `NotFoundError` `DOMException` if the folder doesn't exist.
    #[wasm_bindgen(catch, js_name = storageList)]
    pub async fn storage_list(
        folder: &str,
        prefix: &str,
        limit: u32,
        offset: u32,
    ) -> Result<JsValue, JsValue>;

    /// Create an OPFS directory under the storage root.
    #[wasm_bindgen(catch, js_name = storageCreateFolder)]
    pub async fn storage_create_folder(name: &str) -> Result<JsValue, JsValue>;

    /// Remove an OPFS directory recursively. Rejects with a `NotFoundError`
    /// `DOMException` if the folder doesn't exist.
    #[wasm_bindgen(catch, js_name = storageDeleteFolder)]
    pub async fn storage_delete_folder(name: &str) -> Result<JsValue, JsValue>;

    /// List top-level storage directories.
    /// Returns a plain JS array of folder name strings — NOT a JSON string.
    /// Decode with `serde_wasm_bindgen::from_value`.
    #[wasm_bindgen(catch, js_name = storageListFolders)]
    pub async fn storage_list_folders() -> Result<JsValue, JsValue>;

    // ─── Cookies (read from SW via CookieStore API) ──────────────────────────

    /// Read all cookies via `self.cookieStore.getAll()` and return them
    /// formatted as a `Cookie` header value (e.g., `auth_token=xyz; foo=bar`).
    /// Empty string if CookieStore isn't available or no cookies exist.
    ///
    /// Workaround for the SW spec filtering the `Cookie` header out of
    /// `FetchEvent.request.headers`. See `bridge.js::readCookieHeader`.
    #[wasm_bindgen(js_name = readCookieHeader)]
    pub async fn read_cookie_header() -> JsValue;

    // ─── Network (fetch) ──────────────────────────────────────────────────────

    /// Execute an HTTP fetch request.
    /// `headers_json` is a JSON object of header key/value pairs.
    /// `body` is the request body bytes (pass empty slice for no body).
    /// Returns a plain JS object `{ status, headers, body: Uint8Array }` —
    /// NOT a JSON string. Decode directly with `serde_wasm_bindgen::from_value`.
    /// Rejects on a transport-level failure (network error, CORS, invalid
    /// URL, etc.) — the `fetch()` call itself throwing, not an HTTP error
    /// status (those resolve normally with `status` set).
    #[wasm_bindgen(catch, js_name = httpFetch)]
    pub async fn http_fetch(
        method: &str,
        url: &str,
        headers_json: &str,
        body: &[u8],
    ) -> Result<JsValue, JsValue>;

    // ─── Asset loader bridge (SW → main thread) ───────────────────────────────

    /// Load an external asset by id. Returns a Promise that resolves to an
    /// `AssetLoadStatus`-shaped JS object: `{ status: "ready" | "failed",
    /// error?: string }`.
    ///
    /// Called from `SwAssetLoader::load`. The id is the block's manifest
    /// asset id; `manifest_json` is a serialised `ExternalAsset` (`{id,
    /// loader, version, url, sha256}`) — the JS side does the actual fetch +
    /// sha256 verification + named-loader init by postMessaging the main
    /// thread (only place where `fetch`, `crypto.subtle`, and JS-level
    /// loaders like ffmpeg.wasm are reachable).
    #[wasm_bindgen(catch, js_name = loadAsset)]
    pub async fn load_asset(asset_id: &str, manifest_json: &str) -> Result<JsValue, JsValue>;

    // ─── LLM bridge ───────────────────────────────────────────────────────────

    /// Unload the engine on the page.
    #[wasm_bindgen(js_name = llmUnloadEngine, catch)]
    pub async fn llm_unload_engine(model_id: &str) -> Result<JsValue, JsValue>;

    /// Start a streaming chat completion. Returns the stream id as a JS string.
    #[wasm_bindgen(js_name = llmChatStream, catch)]
    pub async fn llm_chat_stream(body_json: &str) -> Result<JsValue, JsValue>;

    /// Pull the next frame from an LLM chat stream.
    /// Frame JSON: `{kind:'chunk', payload:<openai chunk json>}` |
    ///             `{kind:'done'}` | `{kind:'error', payload:<string>}`.
    #[wasm_bindgen(js_name = llmNextStreamFrame, catch)]
    pub async fn llm_next_stream_frame(stream_id: &str) -> Result<JsValue, JsValue>;

    /// Cancel an in-flight stream.
    #[wasm_bindgen(js_name = llmCancelStream, catch)]
    pub async fn llm_cancel_stream(stream_id: &str) -> Result<JsValue, JsValue>;

    // ─── Image bridge ─────────────────────────────────────────────────────────

    /// Load the page-side T2I engine for `model_id`. One-shot — resolves when
    /// the model is fully loaded onto the WebGPU device.
    #[wasm_bindgen(js_name = imageLoadEngine, catch)]
    pub async fn image_load_engine(model_id: &str) -> Result<JsValue, JsValue>;

    /// Unload the page-side T2I engine.
    #[wasm_bindgen(js_name = imageUnloadEngine, catch)]
    pub async fn image_unload_engine() -> Result<JsValue, JsValue>;

    /// Start an image generation. Returns the request id as a JS string. Pump
    /// frames with `imageNextFrame`.
    #[wasm_bindgen(js_name = imageStartGenerate, catch)]
    pub async fn image_start_generate(body_json: &str) -> Result<JsValue, JsValue>;

    /// Pull the next frame from an image generation. Frame JSON:
    ///   `{kind:'progress', payload:{stage, bytes_downloaded?, bytes_total?}}` |
    ///   `{kind:'done', payload:{data:<base64>, mime_type}}` |
    ///   `{kind:'error', payload:<string>}`.
    #[wasm_bindgen(js_name = imageNextFrame, catch)]
    pub async fn image_next_frame(request_id: &str) -> Result<JsValue, JsValue>;

    /// Cancel an in-flight generation.
    #[wasm_bindgen(js_name = imageCancelStream, catch)]
    pub async fn image_cancel_stream(request_id: &str) -> Result<JsValue, JsValue>;

    // ─── Embed bridge ─────────────────────────────────────────────────────────

    /// Embed `texts` using the page-resident Transformers.js pipeline for
    /// `model_id`. Resolves to a JSON string `{"vectors":[[...]],"dims":<n>}`.
    #[wasm_bindgen(catch, js_name = embedRun)]
    pub async fn embed_run(model_id: &str, texts_json: &str) -> Result<JsValue, JsValue>;

    /// Eagerly load the pipeline for `model_id`. Optional — `embedRun` will
    /// lazy-load if needed.
    #[wasm_bindgen(catch, js_name = embedCreatePipeline)]
    pub async fn embed_create_pipeline(model_id: &str) -> Result<JsValue, JsValue>;

    /// Free the page-resident pipeline for `model_id`. Optional.
    #[wasm_bindgen(catch, js_name = embedUnload)]
    pub async fn embed_unload(model_id: &str) -> Result<JsValue, JsValue>;
}

// ─── Rejection helpers ──────────────────────────────────────────────────────
//
// Shared by every call site that awaits one of the `#[wasm_bindgen(catch)]`
// externs above: turn a rejected `JsValue` (an `Error`, a `DOMException`, or
// an arbitrary thrown value) into something a typed Rust error can carry,
// without losing the distinction JS uses to signal specific failure kinds
// (a `DOMException`'s `.name`, e.g. `"NotFoundError"`, `"QuotaExceededError"`).

/// Human-readable message for a rejected `JsValue`. Prefers `.message`
/// (present on `Error`/`DOMException`); falls back to the value itself if
/// it's a bare string, then to its `Debug` representation.
pub fn describe(err: &JsValue) -> String {
    if let Ok(msg) = js_sys::Reflect::get(err, &JsValue::from_str("message")) {
        if let Some(s) = msg.as_string() {
            if !s.is_empty() {
                return s;
            }
        }
    }
    if let Some(s) = err.as_string() {
        return s;
    }
    format!("{err:?}")
}

/// The `.name` property of a rejected `JsValue`, if present. `DOMException`
/// sets this to a well-known string — callers match on it to map specific
/// rejection kinds (e.g. `"NotFoundError"`) to typed Rust errors instead of
/// collapsing every failure into a generic `Internal`/`Other` variant.
pub fn error_name(err: &JsValue) -> Option<String> {
    js_sys::Reflect::get(err, &JsValue::from_str("name"))
        .ok()
        .and_then(|v| v.as_string())
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use js_sys::{Object, Reflect};
    use wasm_bindgen_test::wasm_bindgen_test;

    use super::*;

    #[wasm_bindgen_test]
    fn describe_prefers_message_over_bare_string_fallback() {
        let obj = Object::new();
        Reflect::set(
            &obj,
            &JsValue::from_str("name"),
            &JsValue::from_str("NotFoundError"),
        )
        .unwrap();
        Reflect::set(
            &obj,
            &JsValue::from_str("message"),
            &JsValue::from_str("no such file"),
        )
        .unwrap();
        let val: JsValue = obj.into();

        assert_eq!(describe(&val), "no such file");
        assert_eq!(error_name(&val).as_deref(), Some("NotFoundError"));
    }

    #[wasm_bindgen_test]
    fn describe_falls_back_to_bare_string_when_no_message() {
        let val = JsValue::from_str("plain rejection reason");
        assert_eq!(describe(&val), "plain rejection reason");
        assert_eq!(error_name(&val), None);
    }

    #[wasm_bindgen_test]
    fn describe_falls_back_to_debug_repr_for_valueless_object() {
        // An object with neither `.message` nor being a string itself —
        // `describe` must not panic, it degrades to the Debug repr.
        let obj = Object::new();
        let val: JsValue = obj.into();
        // Just assert it doesn't panic and returns something non-empty.
        assert!(!describe(&val).is_empty());
        assert_eq!(error_name(&val), None);
    }

    #[wasm_bindgen_test]
    fn error_name_none_when_name_is_not_a_string() {
        let obj = Object::new();
        Reflect::set(&obj, &JsValue::from_str("name"), &JsValue::from_f64(1.0)).unwrap();
        let val: JsValue = obj.into();
        assert_eq!(error_name(&val), None);
    }
}
