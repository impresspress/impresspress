// sql.js ESM wrapper is statically imported. Dynamic import() is forbidden in
// Service Workers, so this must be a static import. The wrapper is vendored
// inside impresspress-browser and written to `/vendor/sql-wasm-esm.js` by the
// framework's `export-assets` bin; `/vendor/sql-wasm.wasm` is the matching
// binary loaded by sql.js at runtime via its `locateFile` callback.
import initSqlJs from '/vendor/sql-wasm-esm.js';

// Module-level state
let _db = null;
const SQL_WASM_PATH = '/vendor/sql-wasm.wasm';
const DB_FILENAME = 'impresspress.db';

// ─── Database (sql.js) ────────────────────────────────────────────────────────

/**
 * Load sql.js WASM, try to load existing DB from OPFS, create new if none exists.
 * Sets PRAGMA foreign_keys=ON.
 */
export async function dbInit() {
    const SQL = await initSqlJs({
        locateFile: () => SQL_WASM_PATH,
    });

    const root = await navigator.storage.getDirectory();
    let existingData = null;
    try {
        const fileHandle = await root.getFileHandle(DB_FILENAME);
        const file = await fileHandle.getFile();
        const buffer = await file.arrayBuffer();
        if (buffer.byteLength > 0) {
            existingData = new Uint8Array(buffer);
        }
    } catch (_e) {
        // File does not exist yet — start fresh
    }

    if (existingData) {
        _db = new SQL.Database(existingData);
    } else {
        _db = new SQL.Database();
    }

    _db.run('PRAGMA foreign_keys = ON;');

    // Custom scalar fn used by BrowserVectorService.upsert to ship f32 blobs
    // through JSON params (params can't carry binary). Rust packs the vector
    // as little-endian f32 BLOB → base64 → string param → BLOB column via
    // base64_decode() inside the INSERT.
    _db.create_function('base64_decode', (b64) => {
        if (!b64) return new Uint8Array(0);
        const bin = atob(b64);
        const out = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
        return out;
    });
}

/**
 * Execute SQL that modifies data (INSERT/UPDATE/DELETE/DDL).
 * @param {string} sql
 * @param {unknown[]} params - bind values, positional (sql.js accepts the
 *   array directly — no JSON encode/decode round trip on either side; see
 *   `db_codec::params_to_js`/`empty_params` on the Rust side).
 * @returns {number} rows-modified count. Does NOT flush to OPFS — see
 *   `dbFlush`'s doc comment for the durability contract.
 */
export function dbExecRaw(sql, params) {
    _db.run(sql, params);
    return _db.getRowsModified();
}

/**
 * Execute a SELECT SQL query.
 * @param {string} sql
 * @param {unknown[]} params - bind values, positional (see `dbExecRaw`)
 * @returns {Record<string, unknown>[]} row objects — a plain JS array, NOT a
 *   JSON string. Decoded on the Rust side with `serde_wasm_bindgen`
 *   (`db_codec::parse_rows`/`rows_from_js`) rather than
 *   `JSON.stringify` + `serde_json::from_str`.
 */
export function dbQueryRaw(sql, params) {
    const results = _db.exec(sql, params);
    if (!results || results.length === 0) {
        return [];
    }
    const { columns, values } = results[0];
    return values.map((row) => {
        const obj = {};
        columns.forEach((col, i) => {
            obj[col] = row[i];
        });
        return obj;
    });
}

/**
 * Export the sql.js DB to a Uint8Array and write it to OPFS at
 * `impresspress.db`.
 *
 * Durability contract: the Rust side (`BrowserDatabaseService::with_flush`
 * in `database.rs`) calls this exactly ONCE per logical `DatabaseService`
 * mutation (`create`/`update`/`delete`/`upsert`/`exec_raw`/schema changes),
 * not once per SQL statement — a logical mutation that issues several
 * statements (e.g. a lazy column-add ALTER before the INSERT) is one flush,
 * not N. The flush happens even when the logical operation's own result is
 * an error, since an earlier statement inside it may already have mutated
 * the in-memory sql.js DB. There is no background/debounced/timer-based
 * flush — every `DatabaseService` call that returns has already attempted
 * exactly one flush, so the only crash-loss window is "mid-flush" (the tab
 * or Service Worker is killed while `dbFlush` itself is exporting/writing),
 * which is an inherent OPFS/browser-crash risk independent of this
 * batching, not a window this change introduces.
 */
export async function dbFlush() {
    if (!_db) return;
    const data = _db.export();
    const root = await navigator.storage.getDirectory();
    const fileHandle = await root.getFileHandle(DB_FILENAME, { create: true });
    const writable = await fileHandle.createWritable();
    await writable.write(data);
    await writable.close();
}

// ─── Storage (OPFS) ──────────────────────────────────────────────────────────

const STORAGE_DIR = 'storage';

async function getStorageRoot() {
    const root = await navigator.storage.getDirectory();
    return root.getDirectoryHandle(STORAGE_DIR, { create: true });
}

async function getFolderHandle(storageRoot, folder, create = false) {
    return storageRoot.getDirectoryHandle(folder, { create });
}

/**
 * Write file + metadata to OPFS.
 * @param {string} folder
 * @param {string} key
 * @param {Uint8Array} data
 * @param {string} contentType
 */
export async function storagePut(folder, key, data, contentType) {
    const storageRoot = await getStorageRoot();
    const folderHandle = await getFolderHandle(storageRoot, folder, true);

    // Write file data
    const fileHandle = await folderHandle.getFileHandle(key, { create: true });
    const writable = await fileHandle.createWritable();
    await writable.write(data);
    await writable.close();

    // Write metadata
    const meta = { content_type: contentType, size: data.length };
    const metaHandle = await folderHandle.getFileHandle(`${key}.__meta__`, { create: true });
    const metaWritable = await metaHandle.createWritable();
    await metaWritable.write(JSON.stringify(meta));
    await metaWritable.close();
}

/**
 * Read file + metadata from OPFS.
 * @param {string} folder
 * @param {string} key
 * @returns {{data: Uint8Array, meta: {content_type: string, size: number}}}
 *   A plain JS object — NOT a JSON string. `storage.rs` decodes it directly
 *   with `serde_wasm_bindgen::from_value`; `data` deserializes straight into
 *   a Rust `Vec<u8>` from the real `Uint8Array` here, with no
 *   Uint8Array→Array<number>→JSON round trip in either direction.
 */
export async function storageGet(folder, key) {
    const storageRoot = await getStorageRoot();
    const folderHandle = await getFolderHandle(storageRoot, folder, false);

    // Read file data
    const fileHandle = await folderHandle.getFileHandle(key);
    const file = await fileHandle.getFile();
    const buffer = await file.arrayBuffer();
    const data = new Uint8Array(buffer);

    // Read metadata
    let meta = { content_type: 'application/octet-stream', size: data.length };
    try {
        const metaHandle = await folderHandle.getFileHandle(`${key}.__meta__`);
        const metaFile = await metaHandle.getFile();
        const metaText = await metaFile.text();
        meta = JSON.parse(metaText);
    } catch (_e) {
        // No metadata file — use defaults
    }

    return { data, meta };
}

/**
 * Delete file + metadata from OPFS.
 * @param {string} folder
 * @param {string} key
 */
export async function storageDelete(folder, key) {
    const storageRoot = await getStorageRoot();
    const folderHandle = await getFolderHandle(storageRoot, folder, false);
    await folderHandle.removeEntry(key);
    try {
        await folderHandle.removeEntry(`${key}.__meta__`);
    } catch (_e) {
        // Metadata may not exist
    }
}

/**
 * List files in a folder.
 * @param {string} folder
 * @param {string} prefix
 * @param {number} limit
 * @param {number} offset
 * @returns {string} JSON array of key strings
 */
export async function storageList(folder, prefix, limit, offset) {
    const storageRoot = await getStorageRoot();
    const folderHandle = await getFolderHandle(storageRoot, folder, false);

    const keys = [];
    for await (const [name] of folderHandle.entries()) {
        // Skip metadata files
        if (name.endsWith('.__meta__')) continue;
        if (!prefix || name.startsWith(prefix)) {
            keys.push(name);
        }
    }

    keys.sort();
    const page = keys.slice(offset, limit > 0 ? offset + limit : undefined);
    return JSON.stringify(page);
}

/**
 * Create OPFS directory under storage root.
 * @param {string} name
 */
export async function storageCreateFolder(name) {
    const storageRoot = await getStorageRoot();
    await storageRoot.getDirectoryHandle(name, { create: true });
}

/**
 * Remove OPFS directory recursively.
 * @param {string} name
 */
export async function storageDeleteFolder(name) {
    const storageRoot = await getStorageRoot();
    await storageRoot.removeEntry(name, { recursive: true });
}

/**
 * List top-level storage directories.
 * @returns {string} JSON array of folder name strings
 */
export async function storageListFolders() {
    const storageRoot = await getStorageRoot();
    const folders = [];
    for await (const [name, handle] of storageRoot.entries()) {
        if (handle.kind === 'directory') {
            folders.push(name);
        }
    }
    folders.sort();
    return JSON.stringify(folders);
}

// ─── Asset loader bridge (SW → main thread) ─────────────────────────────────
//
// The Rust SwAssetLoader (running inside this SW) calls loadAsset() to ask the
// main thread to fetch + verify + init an external asset (ffmpeg.wasm, etc).
// We postMessage a 'load-asset-request' to the first window client, then wait
// for the matching 'load-asset-response' to arrive at sw.js's message listener.
// sw.js routes the response back here via globalThis.__impresspressCompleteAssetLoad.

const _pendingAssetLoads = new Map(); // correlationId -> resolve fn

/**
 * Load an external asset by id by postMessaging the main thread.
 * @param {string} assetId
 * @param {string} manifestJson - JSON-serialised ExternalAsset {id, loader, version, url, sha256}
 * @returns {Promise<{status: 'ready'|'pending'|'failed', error?: string}>}
 */
export async function loadAsset(assetId, manifestJson) {
    const manifest = JSON.parse(manifestJson);

    // Find any window client. If none, fail fast — no point waiting.
    const clients = await self.clients.matchAll({ type: 'window', includeUncontrolled: false });
    if (clients.length === 0) {
        return { status: 'failed', error: 'no active page — open the app in a tab to load assets' };
    }

    const correlationId = `asset-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
    const replyPromise = new Promise((resolve) => {
        _pendingAssetLoads.set(correlationId, resolve);
        // Bound the wait so a misbehaving page can't block the SW forever.
        setTimeout(() => {
            if (_pendingAssetLoads.has(correlationId)) {
                _pendingAssetLoads.delete(correlationId);
                resolve({ status: 'failed', error: 'load-asset timed out' });
            }
        }, manifest.timeout_ms ?? 120_000);
    });

    clients[0].postMessage({
        type: 'load-asset-request',
        id: correlationId,
        manifest,
    });

    return await replyPromise;
}

/**
 * Resolve a pending loadAsset() call. Called from sw.js's message handler
 * when a 'load-asset-response' arrives from the main thread. Exposed on
 * globalThis so sw.js (a separate top-level script) can reach it without
 * importing this module — wasm-bindgen owns the import path here.
 *
 * @param {string} correlationId
 * @param {{status: 'ready'|'pending'|'failed', error?: string}} reply
 */
export function _completeAssetLoad(correlationId, reply) {
    const resolve = _pendingAssetLoads.get(correlationId);
    if (resolve) {
        _pendingAssetLoads.delete(correlationId);
        resolve(reply);
    }
}

globalThis.__impresspressCompleteAssetLoad = _completeAssetLoad;

// ─── LLM (SW → page postMessage bridge) ─────────────────────────────────────
//
// Mirrors the loadAsset pattern: correlation-id keyed postMessage to a window
// client; resolvers kept in a Map; sw.js routes replies via globalThis hook.
//
// One-shot operations (currently only `llmUnloadEngine`) use
// `_pendingLlmRequests`. Streamed operations (chat, create-engine) share a
// single `_activeLlmStreams` Map and a single page→SW frame envelope:
//   { type: 'llm-stream-frame', id, kind: 'chunk'|'progress'|'done'|'error', payload? }
// Each stream is a queue + waiter list so Rust can `await` one frame at a
// time while many frames are buffered in flight.

const _pendingLlmRequests = new Map();   // id -> { resolve, reject } (one-shot)
const _activeLlmStreams   = new Map();   // id -> { push, closeOk, closeErr, queue, waiters }

async function _postToWindowClient(payload) {
    const clients = await self.clients.matchAll({ type: 'window', includeUncontrolled: false });
    if (clients.length === 0) {
        throw new Error('no active page — open the app in a tab');
    }
    clients[0].postMessage(payload);
}

function _mkLlmId(prefix) {
    return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

/** Create the queue/waiter pair for a new stream and register it. */
function _registerStream(id) {
    const queue = [];
    const waiters = [];
    const push = (frame) => {
        if (waiters.length > 0) waiters.shift()(frame);
        else queue.push(frame);
    };
    _activeLlmStreams.set(id, {
        push,
        closeOk: () => push({ kind: 'done' }),
        closeErr: (err) => push({ kind: 'error', payload: err }),
        queue,
        waiters,
    });
}

/** Start a streamed LLM operation. Returns the stream id. */
async function _startLlmStream(requestType, idPrefix, extraPayload) {
    const id = _mkLlmId(idPrefix);
    _registerStream(id);
    await _postToWindowClient({ type: requestType, id, ...extraPayload });
    return id;
}

/**
 * Unload the engine on the page.
 * @param {string} modelId
 * @returns {Promise<void>}
 */
export async function llmUnloadEngine(modelId) {
    const id = _mkLlmId('llm-unload');
    const replyPromise = new Promise((resolve, reject) => {
        _pendingLlmRequests.set(id, { resolve, reject });
    });
    await _postToWindowClient({ type: 'llm-unload-request', id, modelId });
    return await replyPromise;
}

/**
 * Start a streaming chat completion. Returns a stream id; pump with
 * `llmNextStreamFrame`. Frames are `{kind:'chunk', payload:<openai chunk
 * JSON>}` then a terminal `{kind:'done'}` or `{kind:'error', payload}`.
 * @param {string} bodyJson - JSON request body as built by Rust encode_request_body
 * @returns {Promise<string>} stream id
 */
export async function llmChatStream(bodyJson) {
    return _startLlmStream('llm-chat-stream-request', 'llm-chat', { body: bodyJson });
}

/**
 * Pull the next frame from any LLM stream (chat OR create-engine). Blocks
 * until a frame arrives. After a terminal frame (done/error) the stream
 * entry is removed.
 * @param {string} id
 * @returns {Promise<string>} JSON-encoded frame:
 *   {kind:'chunk',payload}|{kind:'progress',payload}|{kind:'done'}|{kind:'error',payload}
 */
export async function llmNextStreamFrame(id) {
    const stream = _activeLlmStreams.get(id);
    if (!stream) {
        return JSON.stringify({ kind: 'error', payload: 'unknown stream id' });
    }
    let frame;
    if (stream.queue.length > 0) {
        frame = stream.queue.shift();
    } else {
        frame = await new Promise((resolve) => stream.waiters.push(resolve));
    }
    if (frame.kind === 'done' || frame.kind === 'error') {
        _activeLlmStreams.delete(id);
    }
    return JSON.stringify(frame);
}

/**
 * Cancel an in-flight stream.
 * @param {string} id
 */
export async function llmCancelStream(id) {
    const stream = _activeLlmStreams.get(id);
    if (stream) {
        // Terminate any pending awaiter with an error frame (no-op if the
        // Rust side has already broken out of its loop).
        stream.closeErr('cancelled');
        // Remove the entry now rather than waiting for the (possibly never
        // called) next pump call to notice the terminal frame — the Rust
        // side breaks its loop immediately after calling cancel_stream.
        _activeLlmStreams.delete(id);
    }
    await _postToWindowClient({ type: 'llm-stream-cancel', id });
}

/**
 * Called by sw.js when a page reply arrives. Routes to the pending request
 * or active stream by id.
 *
 * Page → SW message shapes:
 *   { type: 'llm-unload-response', id, error? }                         (one-shot)
 *   { type: 'llm-stream-frame', id, kind, payload? }                    (streams)
 *     where `kind` is 'chunk' | 'progress' | 'done' | 'error' and
 *     `payload` is the chunk/progress/error string (omitted for 'done').
 */
export function _completeLlmMessage(msg) {
    if (msg.type === 'llm-unload-response') {
        const pending = _pendingLlmRequests.get(msg.id);
        if (!pending) return;
        _pendingLlmRequests.delete(msg.id);
        if (msg.error) pending.reject(new Error(msg.error));
        else pending.resolve();
        return;
    }
    if (msg.type === 'llm-stream-frame') {
        const stream = _activeLlmStreams.get(msg.id);
        if (!stream) return;
        if (msg.kind === 'done') stream.closeOk();
        else if (msg.kind === 'error') stream.closeErr(msg.payload ?? 'unknown error');
        else stream.push({ kind: msg.kind, payload: msg.payload });
    }
}

globalThis.__impresspressCompleteLlmMessage = _completeLlmMessage;

// ─── Image (SW → page postMessage bridge) ───────────────────────────────────
//
// Mirrors the LLM bridge. One-shot operations (`imageLoadEngine`,
// `imageUnloadEngine`) use `_pendingImageRequests`. Streamed generation
// (`imageStartGenerate` + `imageNextFrame`) shares `_activeImageStreams` with
// a page→SW frame envelope:
//   { type: 'image-stream-frame', id, kind: 'progress'|'done'|'error', payload? }

const _pendingImageRequests = new Map(); // id -> { resolve, reject }
const _activeImageStreams   = new Map(); // id -> { push, closeOk, closeErr, queue, waiters }

function _mkImageId(prefix) {
    return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

function _registerImageStream(id) {
    const queue = [];
    const waiters = [];
    const push = (frame) => {
        if (waiters.length > 0) waiters.shift()(frame);
        else queue.push(frame);
    };
    _activeImageStreams.set(id, {
        push,
        closeOk: (payload) => push({ kind: 'done', payload }),
        closeErr: (err) => push({ kind: 'error', payload: err }),
        queue,
        waiters,
    });
}

/**
 * Load the page-side T2I engine for `modelId`. Resolves when the model is
 * fully loaded onto the WebGPU device. One-shot.
 * @param {string} modelId
 * @returns {Promise<void>}
 */
export async function imageLoadEngine(modelId) {
    const id = _mkImageId('image-load');
    const replyPromise = new Promise((resolve, reject) => {
        _pendingImageRequests.set(id, { resolve, reject });
    });
    await _postToWindowClient({ type: 'image-load-request', id, modelId });
    return await replyPromise;
}

/**
 * Unload the page-side T2I engine. One-shot.
 * @returns {Promise<void>}
 */
export async function imageUnloadEngine() {
    const id = _mkImageId('image-unload');
    const replyPromise = new Promise((resolve, reject) => {
        _pendingImageRequests.set(id, { resolve, reject });
    });
    await _postToWindowClient({ type: 'image-unload-request', id });
    return await replyPromise;
}

/**
 * Start a streamed image generation. Returns a request id; pump with
 * `imageNextFrame`. Frames are `{kind:'progress',payload}` (rare on SD-Turbo)
 * then a terminal `{kind:'done', payload:{data:<base64>, mime_type}}` or
 * `{kind:'error', payload:<string>}`.
 * @param {string} bodyJson - JSON-encoded ImageRequest
 * @returns {Promise<string>} request id
 */
export async function imageStartGenerate(bodyJson) {
    const id = _mkImageId('image-gen');
    _registerImageStream(id);
    await _postToWindowClient({ type: 'image-generate-stream-request', id, body: bodyJson });
    return id;
}

/**
 * Pull the next frame from an image generation. Blocks until a frame arrives.
 * After a terminal frame the stream entry is removed.
 * @param {string} id
 * @returns {Promise<string>} JSON-encoded frame
 */
export async function imageNextFrame(id) {
    const stream = _activeImageStreams.get(id);
    if (!stream) {
        return JSON.stringify({ kind: 'error', payload: 'unknown request id' });
    }
    let frame;
    if (stream.queue.length > 0) {
        frame = stream.queue.shift();
    } else {
        frame = await new Promise((resolve) => stream.waiters.push(resolve));
    }
    if (frame.kind === 'done' || frame.kind === 'error') {
        _activeImageStreams.delete(id);
    }
    return JSON.stringify(frame);
}

/**
 * Cancel an in-flight image generation.
 * @param {string} id
 */
export async function imageCancelStream(id) {
    const stream = _activeImageStreams.get(id);
    if (stream) {
        stream.closeErr('cancelled');
        _activeImageStreams.delete(id);
    }
    await _postToWindowClient({ type: 'image-stream-cancel', id });
}

/**
 * Called by sw.js when a page image reply arrives. Routes to the pending
 * one-shot or active stream by id.
 *
 * Page → SW message shapes:
 *   { type: 'image-load-response',   id, error? }                      (one-shot)
 *   { type: 'image-unload-response', id, error? }                      (one-shot)
 *   { type: 'image-stream-frame',    id, kind, payload? }              (streams)
 *     `kind` ∈ {'progress','done','error'}; payload shape varies by kind.
 */
export function _completeImageMessage(msg) {
    if (msg.type === 'image-load-response' || msg.type === 'image-unload-response') {
        const pending = _pendingImageRequests.get(msg.id);
        if (!pending) return;
        _pendingImageRequests.delete(msg.id);
        if (msg.error) pending.reject(new Error(msg.error));
        else pending.resolve();
        return;
    }
    if (msg.type === 'image-stream-frame') {
        const stream = _activeImageStreams.get(msg.id);
        if (!stream) return;
        if (msg.kind === 'done') stream.closeOk(msg.payload);
        else if (msg.kind === 'error') stream.closeErr(msg.payload ?? 'unknown error');
        else stream.push({ kind: msg.kind, payload: msg.payload });
    }
}

globalThis.__impresspressCompleteImageMessage = _completeImageMessage;

// ─── Embed (SW → page postMessage bridge) ───────────────────────────────────
//
// Mirrors the LLM bridge pattern: correlation-id keyed postMessage to a window
// client; resolvers kept in a Map; sw.js routes replies via globalThis hook.

const _pendingEmbedRequests = new Map(); // id -> { resolve, reject }

function _mkEmbedId(prefix) {
    return `${prefix}-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

/**
 * Embed `texts` using the page-resident Transformers.js pipeline for `modelId`.
 * Resolves to a JSON string `{"vectors":[[...]],"dims":<n>}`.
 * @param {string} modelId
 * @param {string} textsJson - JSON array of strings
 * @returns {Promise<string>}
 */
export async function embedRun(modelId, textsJson) {
    const id = _mkEmbedId('embed-run');
    const replyPromise = new Promise((resolve, reject) => {
        _pendingEmbedRequests.set(id, { resolve, reject });
    });
    await _postToWindowClient({ type: 'embed-run-request', id, modelId, texts: textsJson });
    return await replyPromise;
}

/**
 * Eagerly load the pipeline for `modelId` so the next `embedRun` is fast.
 * Optional — `embedRun` will lazy-load if needed.
 * @param {string} modelId
 * @returns {Promise<void>}
 */
export async function embedCreatePipeline(modelId) {
    const id = _mkEmbedId('embed-create');
    const replyPromise = new Promise((resolve, reject) => {
        _pendingEmbedRequests.set(id, { resolve, reject });
    });
    await _postToWindowClient({ type: 'embed-create-request', id, modelId });
    return await replyPromise;
}

/**
 * Free the page-resident pipeline for `modelId`. Optional.
 * @param {string} modelId
 * @returns {Promise<void>}
 */
export async function embedUnload(modelId) {
    const id = _mkEmbedId('embed-unload');
    const replyPromise = new Promise((resolve, reject) => {
        _pendingEmbedRequests.set(id, { resolve, reject });
    });
    await _postToWindowClient({ type: 'embed-unload-request', id, modelId });
    return await replyPromise;
}

/**
 * Called by sw.js when a page embed reply arrives. Routes to the pending
 * request by id.
 *
 * Page → SW message shapes:
 *   { type: 'embed-run-response',    id, result? (JSON string), error? }
 *   { type: 'embed-create-response', id, result?, error? }
 *   { type: 'embed-unload-response', id, result?, error? }
 */
export function _completeEmbedMessage(msg) {
    const pending = _pendingEmbedRequests.get(msg.id);
    if (!pending) return;
    _pendingEmbedRequests.delete(msg.id);
    if (msg.error) pending.reject(new Error(msg.error));
    else pending.resolve(msg.result ?? null);
}

globalThis.__impresspressCompleteEmbedMessage = _completeEmbedMessage;

// ─── Cookies (readable from SW via CookieStore API) ─────────────────────────
//
// The Service-Worker spec filters the `Cookie` header out of
// `FetchEvent.request.headers`: the SW cannot read it back from a Request.
// The cookies ARE sent over the wire for same-origin requests and are
// readable via `self.cookieStore.getAll()` (available in Chromium-based
// browsers; Firefox behind a flag). We surface them to Rust so
// `convert::request_to_message` can inject a synthetic `http.header.cookie`
// meta; downstream consumers (e.g. the `wafer-run/auth` block) then see
// the cookie exactly as they would on a native deployment.

/**
 * Read all cookies from the SW's CookieStore and format as a Cookie header.
 * Returns an empty string if CookieStore isn't available or no cookies exist.
 * @returns {Promise<string>}
 */
export async function readCookieHeader() {
    if (typeof self.cookieStore === 'undefined' || !self.cookieStore.getAll) {
        return '';
    }
    try {
        const cookies = await self.cookieStore.getAll();
        return cookies.map((c) => `${c.name}=${c.value}`).join('; ');
    } catch (_e) {
        return '';
    }
}

// ─── Network (fetch) ─────────────────────────────────────────────────────────

/**
 * Execute an HTTP fetch request.
 * @param {string} method
 * @param {string} url
 * @param {string} headersJson - JSON object of header key/value pairs
 * @param {Uint8Array|null} body
 * @returns {{status: number, headers: Object<string, string>, body: Uint8Array}}
 *   A plain JS object — NOT a JSON string. `network.rs` decodes it directly
 *   with `serde_wasm_bindgen::from_value`, so `body` is a real `Uint8Array`
 *   (deserializes straight into `Vec<u8>`) rather than a JSON number array.
 */
export async function httpFetch(method, url, headersJson, body) {
    const headersObj = JSON.parse(headersJson);
    const init = {
        method,
        headers: headersObj,
    };

    if (body && body.length > 0) {
        init.body = body;
    }

    const response = await fetch(url, init);

    const responseHeaders = {};
    response.headers.forEach((value, name) => {
        responseHeaders[name] = value;
    });

    const responseBuffer = await response.arrayBuffer();

    return {
        status: response.status,
        headers: responseHeaders,
        body: new Uint8Array(responseBuffer),
    };
}
