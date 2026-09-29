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
 * Then sets the connection up — see `configureConnection`.
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

    configureConnection();
}

/**
 * Per-connection setup, which SQLite does not store in the database file:
 * foreign-key enforcement and the `base64_decode` scalar function.
 *
 * Run on every connection `_db` holds, not once per `dbInit`: sql.js's
 * `export()` (which `dbFlush` calls) closes the connection and opens a new
 * one on the same in-memory file, and drops every registered function on the
 * way, so the connection after a flush starts from SQLite's defaults.
 */
function configureConnection() {
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
 *   (`db_codec::rows_from_js`) rather than `JSON.stringify` +
 *   `serde_json::from_str`, then mapped to `Record`s by the shared
 *   `wafer_core::interfaces::database::codec::record_from_json_row`.
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
 *
 * sql.js's `export()` closes the connection and opens a new one, which rolls
 * back a transaction still open on it. The Rust side ends any such
 * transaction itself before calling this (`end_open_transaction` in
 * `database.rs`), so that rollback is reported rather than silent.
 */
export async function dbFlush() {
    if (!_db) return;
    const data = _db.export();
    // `export()` reopened the connection; set the new one up before anything
    // can run on it.
    configureConnection();
    const root = await navigator.storage.getDirectory();
    const fileHandle = await root.getFileHandle(DB_FILENAME, { create: true });
    const writable = await fileHandle.createWritable();
    await writable.write(data);
    await writable.close();
}

// ─── Storage (OPFS) ──────────────────────────────────────────────────────────

// Key-path helpers (dir/leaf splitting, metadata sidecar naming). Pure —
// no DOM/OPFS APIs — and `export`ed so `js/test/storage_paths.test.mjs`
// covers them directly with `node --test`, importing this file as its
// single source of truth (no separate `storage_paths.mjs` copy to drift
// out of sync). bridge.js's other top-level import
// (`/vendor/sql-wasm-esm.js`) doesn't resolve under plain Node, so the
// test run resolves it to the vendored build via `js/test/node-hooks.mjs`
// (`node --import ./js/test/node-hooks.mjs --test ...`); see that file's
// header comment. These helpers ARE also reachable from real
// request-handling code (storagePut/storageGet/storageDelete/storageList
// below), so — unlike a cross-file import — nothing here can 404 at
// runtime: wasm-bindgen only ever needs to find `bridge.js` itself
// (`#[wasm_bindgen(module = "/js/bridge.js")]` in `bridge.rs`), and these
// functions live inside it.
// Mirrored on the Rust side as `impresspress_core::blocks::dev::paths::
// META_SUFFIX`, which refuses it in a dev-sandbox workspace path so a file
// named after a sidecar can never reach `splitKey` below. Both sides carry
// the other's name: change one and change the other.
const META_SUFFIX = '.__meta__';

// Reject path separators and control characters (including DEL); spaces
// and other printable/unicode characters are legitimate in a file name
// (OPFS itself allows them) so they're accepted here. Matches the Rust-side
// path rules used for native storage (see Plan 1 Task 6), which also allow
// spaces.
const INVALID_SEGMENT_CHARS = /[\\/\x00-\x1f\x7f]/;

export function validateSegments(segments) {
    if (!Array.isArray(segments) || segments.length === 0) {
        throw new TypeError('storage path must have at least one segment');
    }
    for (const s of segments) {
        if (typeof s !== 'string' || s === '' || s === '.' || s === '..') {
            throw new TypeError(`invalid storage path segment: ${JSON.stringify(s)}`);
        }
        if (INVALID_SEGMENT_CHARS.test(s)) {
            throw new TypeError(`storage path segment contains an invalid character: ${JSON.stringify(s)}`);
        }
        // EVERY segment, not just the leaf: a DIRECTORY named `page.html.__meta__`
        // lands in the same OPFS directory as the sidecar of a sibling file
        // named `page.html`, and the two then fight over one name. The Rust
        // producer refuses the suffix on every segment for exactly this reason
        // (`paths.rs::validate_path`); this is the same rule at the boundary
        // that owns the sidecars.
        if (s.endsWith(META_SUFFIX)) {
            throw new TypeError(`storage path segment may not name a metadata sidecar: ${JSON.stringify(s)}`);
        }
    }
    return segments;
}

/** @returns {{dirs: string[], leaf: string}} */
export function splitKey(key) {
    if (typeof key !== 'string' || key === '' || key.endsWith('/')) {
        throw new TypeError(`invalid storage key: ${JSON.stringify(key)}`);
    }
    // `validateSegments` refuses META_SUFFIX on every segment, the leaf
    // included, so there is no separate leaf check here.
    const segments = validateSegments(key.split('/'));
    return { dirs: segments.slice(0, -1), leaf: segments[segments.length - 1] };
}

export function joinKey(dirs, leaf) {
    return [...dirs, leaf].join('/');
}

/** Sidecar name for `leaf`. The suffix is mirrored in Rust — see `META_SUFFIX`. */
export function metaName(leaf) {
    return `${leaf}${META_SUFFIX}`;
}

/** Whether `name` is a sidecar. The suffix is mirrored in Rust — see `META_SUFFIX`. */
export function isMetaName(name) {
    return name.endsWith(META_SUFFIX);
}

const STORAGE_DIR = 'storage';

async function getStorageRoot() {
    const root = await navigator.storage.getDirectory();
    return root.getDirectoryHandle(STORAGE_DIR, { create: true });
}

function storageFolderSegments(folder) {
    // StorageService folder names are logical paths. Native storage resolves
    // `wafer-run/web/site` below its storage root, but OPFS rejects `/` in a
    // single getDirectoryHandle() name. Walk each component so browser
    // storage has the same nested-folder semantics as the other backends.
    const segments = folder.split('/');
    if (segments.some((segment) => !segment || segment === '.' || segment === '..')) {
        throw new TypeError(`invalid storage folder: ${folder}`);
    }
    return segments;
}

async function getFolderHandle(storageRoot, folder, create = false) {
    let handle = storageRoot;
    for (const segment of storageFolderSegments(folder)) {
        handle = await handle.getDirectoryHandle(segment, { create });
    }
    return handle;
}

/**
 * Resolve the OPFS directory handle a key's leaf file lives in, walking the
 * key's own `dirs` segments (from `splitKey`) below the folder handle.
 * These are nested directories WITHIN a storage folder — distinct from
 * `getFolderHandle`'s folder-name segments above. Only `storagePut` passes
 * `create: true`; parents are created only by `put`, per the storage
 * contract (`get`/`delete` pass `create: false` and let a missing directory
 * surface as the same `NotFoundError` a missing file would).
 */
async function getKeyParent(folderHandle, dirs, create) {
    let handle = folderHandle;
    for (const segment of dirs) {
        handle = await handle.getDirectoryHandle(segment, { create });
    }
    return handle;
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
    const { dirs, leaf } = splitKey(key);
    const parent = await getKeyParent(folderHandle, dirs, true);

    // Write file data
    const fileHandle = await parent.getFileHandle(leaf, { create: true });
    const writable = await fileHandle.createWritable();
    await writable.write(data);
    await writable.close();

    // Write metadata
    const meta = { content_type: contentType, size: data.length };
    const metaHandle = await parent.getFileHandle(metaName(leaf), { create: true });
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
    const { dirs, leaf } = splitKey(key);
    const parent = await getKeyParent(folderHandle, dirs, false);

    // Read file data
    const fileHandle = await parent.getFileHandle(leaf);
    const file = await fileHandle.getFile();
    const buffer = await file.arrayBuffer();
    const data = new Uint8Array(buffer);

    // Read metadata
    // Merged over the defaults rather than replacing them: a sidecar that
    // parses but is missing a field would otherwise hand Rust an object it
    // cannot decode (`GetMeta` has no optional fields), and on the streaming
    // path that decode failure used to strand a registered reader.
    let meta = { content_type: 'application/octet-stream', size: data.length };
    try {
        const metaHandle = await parent.getFileHandle(metaName(leaf));
        const metaFile = await metaHandle.getFile();
        const metaText = await metaFile.text();
        meta = { ...meta, ...JSON.parse(metaText) };
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
    const { dirs, leaf } = splitKey(key);
    const parent = await getKeyParent(folderHandle, dirs, false);
    await parent.removeEntry(leaf);
    try {
        await parent.removeEntry(metaName(leaf));
    } catch (_e) {
        // Metadata may not exist
    }
    await pruneEmptyDirs(folderHandle, dirs);
}

/**
 * Drop the directories `dirs` names, deepest first, for as long as they are
 * empty. Stops at `folderHandle`, which is the storage folder itself and is
 * never removed.
 *
 * A storage key namespace is flat to its callers — `blog/post.html` is a key,
 * not a file in a directory — but OPFS makes `blog` a real directory, and a
 * directory OUTLIVES the last key under it. That leftover is not merely
 * untidy: a name is a directory or a file and never both, so an empty `blog`
 * makes `storagePut(folder, 'blog', …)` throw `TypeMismatchError` at
 * `getFileHandle(…, {create: true})` forever. The dev sandbox reaches that
 * state by publishing a site where a path stops being a directory and becomes
 * a page — see `publisher.rs`, which orders such a deletion before the write
 * precisely so this prune can free the name in time.
 *
 * Best-effort by construction: `removeEntry` without `recursive` throws
 * `InvalidModificationError` on a directory that is not empty, which is the
 * normal case (a sibling key still lives there) and the signal to stop
 * walking up.
 */
async function pruneEmptyDirs(folderHandle, dirs) {
    for (let depth = dirs.length; depth > 0; depth -= 1) {
        let parent = folderHandle;
        try {
            for (const segment of dirs.slice(0, depth - 1)) {
                parent = await parent.getDirectoryHandle(segment, { create: false });
            }
            await parent.removeEntry(dirs[depth - 1]);
        } catch (_e) {
            // Not empty, or already gone. Either way nothing above it can be
            // empty either, so stop.
            return;
        }
    }
}

/**
 * List files in a folder matching `prefix`, paginated by `limit`/`offset`.
 * Walks nested directories recursively so a hierarchical key like
 * `assets/app.js` (see `storagePut`/`storage_paths.mjs`) shows up as one
 * joined key rather than being hidden inside a subdirectory; `prefix`
 * matches against that full joined key.
 * @param {string} folder
 * @param {string} prefix
 * @param {number} limit
 * @param {number} offset
 * @returns {{keys: string[], total: number}} A plain JS object — NOT a JSON
 *   string. `total` is the full count of matching entries BEFORE slicing to
 *   the requested page (previously this returned only the page, and the
 *   caller reported the page length as the total).
 *
 *   OPFS's directory iterator (`FileSystemDirectoryHandle.entries()`) has no
 *   native pagination, count, or cursor/skip-ahead API — it's
 *   iterate-everything-or-nothing, and there is no separate persisted index
 *   of keys to consult instead. A true cursor (resuming a listing without
 *   re-scanning the directory) would require maintaining that index
 *   ourselves, which is a bigger change out of scope here; this instead
 *   returns an HONEST total by counting matches, during the one full
 *   enumeration this already required, before applying offset/limit.
 */
export async function storageList(folder, prefix, limit, offset) {
    const storageRoot = await getStorageRoot();
    const folderHandle = await getFolderHandle(storageRoot, folder, false);

    const keys = [];
    async function walk(handle, dirs) {
        for await (const [name, entry] of handle.entries()) {
            if (entry.kind === 'directory') {
                await walk(entry, [...dirs, name]);
            } else if (!isMetaName(name)) {
                const key = joinKey(dirs, name);
                if (!prefix || key.startsWith(prefix)) keys.push(key);
            }
        }
    }
    await walk(folderHandle, []);

    keys.sort();
    const total = keys.length;
    const page = keys.slice(offset, limit > 0 ? offset + limit : undefined);
    return { keys: page, total };
}

/**
 * Create OPFS directory under storage root.
 * @param {string} name
 */
export async function storageCreateFolder(name) {
    const storageRoot = await getStorageRoot();
    await getFolderHandle(storageRoot, name, true);
}

/**
 * Remove a nested OPFS directory recursively.
 * @param {string} name
 */
export async function storageDeleteFolder(name) {
    const storageRoot = await getStorageRoot();
    const segments = storageFolderSegments(name);
    const leaf = segments.pop();
    let parent = storageRoot;
    for (const segment of segments) {
        parent = await parent.getDirectoryHandle(segment, { create: false });
    }
    await parent.removeEntry(leaf, { recursive: true });
}

/**
 * List top-level storage directories.
 * @returns {string[]} A plain JS array of folder name strings — NOT a JSON
 *   string.
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
    return folders;
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
 * @param {string} bodyJson - JSON request body as built by Rust
 *   `impresspress_core::llm_wire::openai::encode_chat_body`
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
    // `typeof self` first: `self` is a worker/window global, and a bare
    // reference to it throws ReferenceError wherever there is none — a plain
    // Node host, or any main-thread caller. The Rust side already treats
    // "no worker global" as a normal case (see `convert::worker_location`),
    // and the answer is the same one a worker with no CookieStore gets: no
    // cookies.
    if (typeof self === 'undefined' || typeof self.cookieStore === 'undefined' || !self.cookieStore.getAll) {
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
 *
 * The caller has already run the URL through the shared SSRF gate
 * (`BrowserNetworkService::do_request`); this function does the transport.
 *
 * @param {string} method
 * @param {string} url
 * @param {string} headersJson - JSON object of header key/value pairs
 * @param {Uint8Array|null} body
 * @param {number} maxResponseBytes - response-body ceiling, from
 *   `impresspress_core::streaming::MAX_NETWORK_RESPONSE_BYTES` (the cap the
 *   Cloudflare adapter enforces too). Enforced here rather than in Rust
 *   because this is where the bytes are read: an advertised `Content-Length`
 *   over the cap is refused before the body is touched, and the running total
 *   is checked per chunk for a chunked response that advertises nothing.
 *   Reading the whole body first and measuring it afterwards would have
 *   already spent the memory the cap exists to protect — a Service Worker has
 *   one linear memory and shares it with the page's whole runtime.
 * @returns {{status: number, headers: Array<[string, string]>, body: Uint8Array}}
 *   A plain JS object — NOT a JSON string. `network.rs` decodes it directly
 *   with `serde_wasm_bindgen::from_value`, so `body` is a real `Uint8Array`
 *   (deserializes straight into `Vec<u8>`) rather than a JSON number array.
 *
 *   `headers` is an ARRAY OF PAIRS, not an object. Per the Fetch spec a
 *   `Headers` iteration combines repeated names into one comma-joined value
 *   *except* `Set-Cookie`, which it yields once per cookie — so an object
 *   keyed by name kept only the last one, and a response setting a session
 *   cookie and a CSRF cookie silently delivered one of them. Comma-joining
 *   `Set-Cookie` is not an alternative: its own grammar uses commas (in
 *   `Expires` dates, among others), so a joined value cannot be split back.
 */
/**
 * The `init` every outbound request is issued with — buffered (`httpFetch`)
 * and streaming (`httpFetchStream`) alike. One function because
 * `redirect: 'error'` is a security property and two copies of it are two
 * chances to lose one.
 *
 * @param {string} method
 * @param {string} headersJson
 * @param {Uint8Array} body
 * @returns {RequestInit}
 */
function fetchInit(method, headersJson, body) {
    const init = {
        method,
        headers: JSON.parse(headersJson),
        // The SSRF gate in `network.rs` inspects the URL the caller asked for
        // and nothing else. `fetch` defaults to `redirect: 'follow'`, so a
        // `302 Location: http://169.254.169.254/…` from a public-looking host
        // would reach an address that gate never saw and hand its body back to
        // the block. `'error'` fails the request closed instead.
        //
        // `'manual'` is not an alternative here: a cross-origin redirect
        // response is opaque, with no readable `Location`, so there is nothing
        // to revalidate. The native path can revalidate per hop (reqwest's
        // `ssrf_revalidating_redirect_policy`) and does; a Fetch-API caller
        // cannot, so it declines to follow at all. The cost is that a
        // legitimate redirect surfaces to the caller as a request error rather
        // than being followed silently — the right trade against a silent
        // fetch of an internal address.
        redirect: 'error',
    };

    if (body && body.length > 0) {
        init.body = body;
    }
    return init;
}

export async function httpFetch(method, url, headersJson, body, maxResponseBytes) {
    const response = await fetch(url, fetchInit(method, headersJson, body));

    const responseHeaders = [];
    response.headers.forEach((value, name) => {
        responseHeaders.push([name, value]);
    });

    return {
        status: response.status,
        headers: responseHeaders,
        body: await readCappedBody(response, maxResponseBytes),
    };
}

/**
 * Read a fetch response body into a `Uint8Array`, refusing anything over
 * `cap` bytes. Throws (rejecting the `httpFetch` promise, which `network.rs`
 * surfaces as a `NetworkError::RequestError`) rather than truncating: a
 * silently short body is a worse failure than a loud one.
 *
 * @param {Response} response
 * @param {number} cap
 * @returns {Promise<Uint8Array>}
 */
async function readCappedBody(response, cap) {
    const advertised = Number(response.headers.get('content-length'));
    if (Number.isFinite(advertised) && advertised > cap) {
        throw new Error(
            `response body ${advertised} bytes exceeds cap of ${cap} bytes`,
        );
    }

    // A bodyless response (204, HEAD) has `body === null`.
    if (!response.body) {
        return new Uint8Array(0);
    }

    const reader = response.body.getReader();
    const chunks = [];
    let received = 0;
    for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        received += value.byteLength;
        // The only guard for a chunked / unknown-length response.
        if (received > cap) {
            await reader.cancel();
            throw new Error(`response body exceeds cap of ${cap} bytes`);
        }
        chunks.push(value);
    }

    const out = new Uint8Array(received);
    let offset = 0;
    for (const chunk of chunks) {
        out.set(chunk, offset);
        offset += chunk.byteLength;
    }
    return out;
}

// ─── Chunked byte reads ──────────────────────────────────────────────────────
//
// wasm-bindgen cannot hand a live JS `ReadableStreamDefaultReader` to Rust as
// anything it can hold across awaits, so a chunked read is expressed the same
// way the LLM and image streams already are: start the read, get an opaque id
// back, then pull one chunk at a time by id. `storageGetStream` and
// `httpFetchStream` both register here, so there is one reader registry and
// one pull/cancel pair rather than one of each per producer.
//
// EVERY registered reader must be either drained to `null` or cancelled.
// `readerNextChunk` deletes the entry when the source reports `done`, so a
// fully-read stream cleans itself up; a Rust consumer that stops early (a
// dropped response, a cap breach) calls `readerCancel`, which also releases
// the underlying OPFS file handle or HTTP connection.

const _byteReaders = new Map();
let _nextByteReaderId = 1;

/**
 * @param {ReadableStreamDefaultReader<Uint8Array>} reader
 * @returns {string} the id `readerNextChunk`/`readerCancel` take.
 */
function registerByteReader(reader) {
    const id = `bytes-${_nextByteReaderId++}`;
    _byteReaders.set(id, reader);
    return id;
}

/**
 * Pull the next chunk of a registered byte stream.
 *
 * @param {string} id
 * @returns {Promise<Uint8Array|null>} `null` once the source is exhausted —
 *   the one signal Rust treats as end-of-stream. An unknown id THROWS rather
 *   than answering `null`: "this stream is finished" and "you are asking about
 *   a stream that does not exist" are different facts, and reporting the
 *   second as the first would turn a bookkeeping bug into a silently truncated
 *   response body.
 */
export async function readerNextChunk(id) {
    const reader = _byteReaders.get(id);
    if (!reader) {
        throw new Error(`readerNextChunk: unknown stream id ${JSON.stringify(id)}`);
    }
    let result;
    try {
        result = await reader.read();
    } catch (e) {
        _byteReaders.delete(id);
        throw e;
    }
    if (result.done) {
        _byteReaders.delete(id);
        return null;
    }
    return result.value;
}

/**
 * Release a registered byte stream that will not be drained. Idempotent, and
 * never throws — it is called from Rust drop/cancel paths, which have nowhere
 * to report a failure to.
 *
 * @param {string} id
 */
export async function readerCancel(id) {
    const reader = _byteReaders.get(id);
    if (!reader) {
        return;
    }
    _byteReaders.delete(id);
    try {
        await reader.cancel();
    } catch (_e) {
        // The source is already gone; there is nothing left to release.
    }
}

/**
 * Streaming counterpart of `storageGet`: resolve the object's metadata
 * eagerly and hand back a reader id for its body instead of the bytes.
 *
 * OPFS gives a real `ReadableStream` (`File.stream()`), so this is a genuine
 * chunked read — the file is never held whole in the Service Worker's memory,
 * which is the same linear memory the whole runtime is using.
 *
 * `size` comes from the same `File` snapshot the body is read from, so a
 * concurrent writer cannot make the advertised length disagree with the bytes
 * that actually stream.
 *
 * @param {string} folder
 * @param {string} key
 * @returns {{stream_id: string, meta: {content_type: string, size: number}}}
 */
export async function storageGetStream(folder, key) {
    const storageRoot = await getStorageRoot();
    const folderHandle = await getFolderHandle(storageRoot, folder, false);
    const { dirs, leaf } = splitKey(key);
    const parent = await getKeyParent(folderHandle, dirs, false);

    const fileHandle = await parent.getFileHandle(leaf);
    const file = await fileHandle.getFile();

    // Merged over the defaults, exactly as `storageGet` does and for the same
    // reason: a sidecar missing a field must not erase that field's default.
    let meta = { content_type: 'application/octet-stream', size: file.size };
    try {
        const metaHandle = await parent.getFileHandle(metaName(leaf));
        const metaFile = await metaHandle.getFile();
        meta = { ...meta, ...JSON.parse(await metaFile.text()) };
    } catch (_e) {
        // No metadata sidecar — use defaults, exactly as `storageGet` does.
    }
    // The sidecar's `size` was written at upload time; the file itself is the
    // authority for what is about to stream.
    meta.size = file.size;

    return { stream_id: registerByteReader(file.stream().getReader()), meta };
}

// ─── Chunked object writes ───────────────────────────────────────────────────
//
// The write half of the same pattern. OPFS `createWritable()` accepts
// incremental `write()` calls, so a large upload is never assembled in memory
// first.
//
// An open writer holds an exclusive lock on the file, so a start with no
// matching finish or abort leaves the object unwritable until the Service
// Worker restarts. Rust's `put_streaming` aborts on every error path.
//
// `createWritable()` needs a file handle, so opening a writer for a key that
// does not exist yet has to CREATE that key first — and an empty file is listed
// by `storageList` and served by both read paths, which fall back to a default
// content type when the sidecar is missing. Discarding a write is routine here
// (any upstream stream error takes that path), so a discarded write of a new
// key removes the file it created; an overwrite keeps the previous object,
// whose bytes the swap file never touched.

const _fileWriters = new Map();
let _nextFileWriterId = 1;

/**
 * Open `folder/key` for a chunked write.
 *
 * @param {string} folder
 * @param {string} key
 * @returns {Promise<string>} the id the chunk/finish/abort calls take.
 */
export async function storagePutStreamStart(folder, key) {
    const storageRoot = await getStorageRoot();
    const folderHandle = await getFolderHandle(storageRoot, folder, true);
    const { dirs, leaf } = splitKey(key);
    const parent = await getKeyParent(folderHandle, dirs, true);

    // Whether this write is what brings the key into existence decides what a
    // discard has to clean up; see `discardWriter`.
    let created = false;
    let fileHandle;
    try {
        fileHandle = await parent.getFileHandle(leaf);
    } catch (e) {
        if (e && e.name !== 'NotFoundError') {
            throw e;
        }
        fileHandle = await parent.getFileHandle(leaf, { create: true });
        created = true;
    }
    const writable = await fileHandle.createWritable();

    const id = `write-${_nextFileWriterId++}`;
    _fileWriters.set(id, { writable, parent, leaf, size: 0, created });
    return id;
}

/**
 * Append one chunk. Throws on an unknown id for the same reason
 * `readerNextChunk` does: silently dropping bytes would produce a short object
 * that reports success.
 *
 * @param {string} id
 * @param {Uint8Array} chunk
 */
export async function storagePutStreamChunk(id, chunk) {
    const entry = _fileWriters.get(id);
    if (!entry) {
        throw new Error(`storagePutStreamChunk: unknown writer id ${JSON.stringify(id)}`);
    }
    await entry.writable.write(chunk);
    entry.size += chunk.byteLength;
}

/**
 * Close the file and write its metadata sidecar — the same sidecar
 * `storagePut` writes, with the size counted from the chunks that actually
 * arrived.
 *
 * There is no OPFS transaction across those two files, so this rejects rather
 * than leaving a half-finished object wherever it can: a failed `close()`
 * discards the writer (and, for a key this write created, the file itself), and
 * a failed sidecar write removes a created key's body. The one residual is an
 * OVERWRITE whose sidecar write fails — the previous bytes are already gone by
 * then, so the new body stays under the previous sidecar. `storage.rs`'s
 * `put_streaming` doc states all three cases.
 *
 * @param {string} id
 * @param {string} contentType
 */
export async function storagePutStreamFinish(id, contentType) {
    const entry = _fileWriters.get(id);
    if (!entry) {
        throw new Error(`storagePutStreamFinish: unknown writer id ${JSON.stringify(id)}`);
    }

    // `close()` is what commits the swap file, so it is exactly where a quota
    // failure surfaces — and it is the one step here that can fail while the
    // exclusive lock is still held. The bookkeeping entry therefore survives
    // until it resolves, and this discards the writer itself rather than
    // relying on the caller's abort: dropping the entry first left the writable
    // un-aborted and the file locked for the life of the Service Worker, which
    // is the failure the note above this registry warns about.
    try {
        await entry.writable.close();
    } catch (e) {
        await discardWriter(id);
        throw e;
    }

    // The body is committed and the lock is gone; only the two files can still
    // need cleaning up.
    _fileWriters.delete(id);

    const meta = { content_type: contentType, size: entry.size };
    try {
        const metaHandle = await entry.parent.getFileHandle(metaName(entry.leaf), { create: true });
        const metaWritable = await metaHandle.createWritable();
        await metaWritable.write(JSON.stringify(meta));
        await metaWritable.close();
    } catch (e) {
        // A committed body with no sidecar would be listed and served as a
        // valid object while this call reports failure. That is removable for a
        // key this write created; for an overwrite the previous bytes are
        // already gone, so the new body stays under the previous sidecar and
        // `storage.rs::put_streaming` documents that residual.
        await removeCreatedFiles(entry);
        throw e;
    }
}

/**
 * Abandon a chunked write: release the file lock and remove the target file if
 * this write is what created it, so an interrupted upload of a NEW key leaves
 * nothing listed or gettable. Idempotent and non-throwing — every Rust error
 * path calls it, and a failure to clean up must not replace the error that
 * caused the abort.
 *
 * @param {string} id
 */
export async function storagePutStreamAbort(id) {
    await discardWriter(id);
}

/**
 * Release a writer and undo whatever it brought into existence: abort the
 * writable (which releases the exclusive lock and discards the swap file), then
 * remove the target file if this write is what created it.
 *
 * Never throws — every caller either has an error to report already or is a
 * Rust cleanup path with nowhere to report one.
 *
 * @param {string} id
 */
async function discardWriter(id) {
    const entry = _fileWriters.get(id);
    if (!entry) {
        return;
    }
    _fileWriters.delete(id);
    try {
        await entry.writable.abort();
    } catch (_e) {
        // Already closed or the handle is gone; the lock is released either way.
    }
    await removeCreatedFiles(entry);
}

/**
 * Remove the object and sidecar a discarded write created. A no-op for an
 * overwrite: that key's previous object is still the right answer, and the swap
 * file never touched it.
 *
 * @param {{parent: FileSystemDirectoryHandle, leaf: string, created: boolean}} entry
 */
async function removeCreatedFiles(entry) {
    if (!entry.created) {
        return;
    }
    for (const name of [entry.leaf, metaName(entry.leaf)]) {
        try {
            await entry.parent.removeEntry(name);
        } catch (_e) {
            // Never written, or already gone.
        }
    }
}

/**
 * Streaming counterpart of `httpFetch`: resolve the response head eagerly and
 * hand back a reader id for the body instead of the bytes.
 *
 * The request `init` is `httpFetch`'s, `redirect: 'error'` included — see that
 * function for why refusing a redirect is half of the outbound SSRF gate. The
 * two must not drift, so the init is built once in `fetchInit`.
 *
 * Unlike `httpFetch` this takes no byte cap: the cap on a streamed response is
 * a running total the Rust consumer keeps, because it is the side that decides
 * what to do with the bytes already delivered.
 *
 * @param {string} method
 * @param {string} url
 * @param {string} headersJson
 * @param {Uint8Array} body
 * @returns {{status: number, headers: [string, string][], stream_id: string|null}}
 *   `stream_id` is `null` for a bodyless response (204, HEAD).
 */
export async function httpFetchStream(method, url, headersJson, body) {
    const response = await fetch(url, fetchInit(method, headersJson, body));

    const responseHeaders = [];
    response.headers.forEach((value, name) => {
        responseHeaders.push([name, value]);
    });

    return {
        status: response.status,
        headers: responseHeaders,
        stream_id: response.body ? registerByteReader(response.body.getReader()) : null,
    };
}
