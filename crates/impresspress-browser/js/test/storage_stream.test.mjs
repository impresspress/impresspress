// Run with: node --import ./js/test/node-hooks.mjs --test js/test/storage_stream.test.mjs
// (see node-hooks.mjs's header comment for why the --import hook is needed).
//
// The chunked OPFS read/write bridge behind `storage.rs`'s `get_streaming` and
// `put_streaming`. Neither can be exercised from Rust — the bridge functions
// are wasm-bindgen extern imports and `wasm-pack test --node` has no OPFS — so
// the File System Access surface bridge.js actually uses is faked here and the
// real bridge functions are driven against it.
//
// What the fake has to be right about is small: directory/file handles that
// create on demand, `entries()` for the listing walk, `createWritable()`
// accepting repeated `write()` calls, and `getFile()` answering a `Blob`-shaped
// object with `size`, `text()` and `stream()`. Node's own `Blob` supplies the
// last three, so the round trip below reads the same bytes back through a real
// `ReadableStream`.
//
// It also has to be able to FAIL where OPFS fails: `close()` is what commits
// the swap file, so it is where a quota error surfaces, and the two-file
// commit (object + metadata sidecar) has no transaction behind it. Both are
// injectable below.
import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import {
    metaName,
    storageGet,
    storageGetStream,
    storageList,
    storagePut,
    storagePutStreamStart,
    storagePutStreamChunk,
    storagePutStreamFinish,
    storagePutStreamAbort,
    readerNextChunk,
    readerCancel,
} from '../bridge.js';

// ─── An in-memory stand-in for OPFS ──────────────────────────────────────────

/** File names whose `close()` rejects, as a quota failure would. */
let failCloseOn = new Set();
/** Names whose writable was aborted, in order. */
let abortedFiles = [];

function makeDir() {
    const dirs = new Map();
    const files = new Map();
    const handle = {
        kind: 'directory',
        async getDirectoryHandle(name, opts = {}) {
            if (!dirs.has(name)) {
                if (!opts.create) {
                    throw notFound(name);
                }
                dirs.set(name, makeDir());
            }
            return dirs.get(name);
        },
        async getFileHandle(name, opts = {}) {
            if (!files.has(name)) {
                if (!opts.create) {
                    throw notFound(name);
                }
                files.set(name, makeFile(name));
            }
            return files.get(name);
        },
        async removeEntry(name) {
            if (!files.delete(name) && !dirs.delete(name)) {
                throw notFound(name);
            }
        },
        async *entries() {
            for (const [name, entry] of dirs) yield [name, entry];
            for (const [name, entry] of files) yield [name, entry];
        },
    };
    return handle;
}

function makeFile(name) {
    let bytes = new Uint8Array(0);
    return {
        kind: 'file',
        async getFile() {
            // A `Blob` gives `size`, `text()` and a real `stream()`, which is
            // what the bridge reads.
            return new Blob([bytes]);
        },
        async createWritable() {
            const parts = [];
            let closed = false;
            return {
                async write(chunk) {
                    if (closed) throw new Error('write after close');
                    parts.push(
                        typeof chunk === 'string'
                            ? new TextEncoder().encode(chunk)
                            : new Uint8Array(chunk),
                    );
                },
                async close() {
                    // OPFS commits the swap file here, so this is where a
                    // quota failure lands — and the swap file's contents are
                    // NOT applied when it does.
                    if (failCloseOn.has(name)) {
                        closed = true;
                        throw quotaExceeded(name);
                    }
                    closed = true;
                    const total = parts.reduce((n, p) => n + p.byteLength, 0);
                    const out = new Uint8Array(total);
                    let at = 0;
                    for (const p of parts) {
                        out.set(p, at);
                        at += p.byteLength;
                    }
                    bytes = out;
                },
                async abort() {
                    closed = true;
                    abortedFiles.push(name);
                },
            };
        },
    };
}

function notFound(name) {
    const err = new Error(`no such entry: ${name}`);
    err.name = 'NotFoundError';
    return err;
}

function quotaExceeded(name) {
    const err = new Error(`quota exceeded writing ${name}`);
    err.name = 'QuotaExceededError';
    return err;
}

beforeEach(() => {
    failCloseOn = new Set();
    abortedFiles = [];
    const root = makeDir();
    // Node defines `navigator` as a getter-only global, so it is redefined
    // rather than assigned.
    Object.defineProperty(globalThis, 'navigator', {
        configurable: true,
        value: { storage: { getDirectory: async () => root } },
    });
});

async function drain(streamId) {
    const chunks = [];
    for (;;) {
        const chunk = await readerNextChunk(streamId);
        if (chunk === null) break;
        chunks.push(...chunk);
    }
    return chunks;
}

/** Every key `storageList` reports for `folder`, sidecars excluded as usual. */
async function listKeys(folder) {
    const { keys } = await storageList(folder, '', 0, 0);
    return keys;
}

// ─── Tests ───────────────────────────────────────────────────────────────────

test('a chunked write is readable as one object, with the size it actually received', async () => {
    const id = await storagePutStreamStart('docs', 'nested/report.bin');
    assert.equal(
        typeof id,
        'string',
        // `storage.rs::put_streaming` errors WITHOUT aborting on a non-string
        // id, because the id is the only handle to the open writable — there
        // would be nothing to abort with. This is where that invariant lives.
        'the writer id is the only handle to the open writable, so it must be a string',
    );
    await storagePutStreamChunk(id, new Uint8Array([1, 2, 3]));
    await storagePutStreamChunk(id, new Uint8Array([4, 5]));
    await storagePutStreamFinish(id, 'application/octet-stream');

    const started = await storageGetStream('docs', 'nested/report.bin');
    assert.equal(started.meta.content_type, 'application/octet-stream');
    assert.equal(
        started.meta.size,
        5,
        'the sidecar size must be the bytes that arrived, not a length declared up front',
    );
    assert.deepEqual(await drain(started.stream_id), [1, 2, 3, 4, 5]);
});

test('an aborted chunked write of a new key leaves nothing listed or gettable', async () => {
    const id = await storagePutStreamStart('docs', 'half.bin');
    await storagePutStreamChunk(id, new Uint8Array([1, 2, 3]));
    await storagePutStreamAbort(id);

    // The abort path is routine here — any upstream stream error takes it —
    // and `createWritable()` needs the file to exist, so the start had to
    // create it. Listing returns every non-sidecar file and both read paths
    // fall back to a default content type when the sidecar is missing, so a
    // file left behind here is served as a valid empty object.
    assert.deepEqual(await listKeys('docs'), [], 'the abandoned key is still listed');
    await assert.rejects(() => storageGet('docs', 'half.bin'), { name: 'NotFoundError' });
    await assert.rejects(() => storageGetStream('docs', 'half.bin'), { name: 'NotFoundError' });

    // Abort is called from every Rust error path and must not itself throw,
    // including when it is called twice or on an id that never existed.
    await storagePutStreamAbort(id);
    await storagePutStreamAbort('write-does-not-exist');

    // A chunk against the abandoned id must be refused rather than silently
    // dropped — dropped bytes would produce a short object reporting success.
    await assert.rejects(
        () => storagePutStreamChunk(id, new Uint8Array([9])),
        /unknown writer id/,
    );
    await assert.rejects(
        () => storagePutStreamFinish(id, 'text/plain'),
        /unknown writer id/,
    );
});

test('an aborted overwrite leaves the previous object exactly as it was', async () => {
    await storagePut('docs', 'a.txt', new TextEncoder().encode('hello'), 'text/plain');

    const id = await storagePutStreamStart('docs', 'a.txt');
    await storagePutStreamChunk(id, new TextEncoder().encode('XXXXXXXXXX'));
    await storagePutStreamAbort(id);

    // `createWritable()` writes to a swap file, so only a successful `close()`
    // replaces the original bytes — and the cleanup that removes a NEW key
    // must not touch an existing one.
    assert.deepEqual(await listKeys('docs'), ['a.txt']);
    const { data, meta } = await storageGet('docs', 'a.txt');
    assert.equal(new TextDecoder().decode(data), 'hello');
    assert.equal(meta.content_type, 'text/plain');
});

test('a close that fails aborts the writable and leaves a new key unwritten', async () => {
    failCloseOn.add('quota.bin');

    const id = await storagePutStreamStart('docs', 'quota.bin');
    await storagePutStreamChunk(id, new Uint8Array([1, 2, 3]));

    await assert.rejects(() => storagePutStreamFinish(id, 'text/plain'), {
        name: 'QuotaExceededError',
    });

    // The lock is the point: `close()` is the one step that can fail while the
    // exclusive writable is still open, so dropping the bookkeeping entry
    // before it resolves left the caller's abort with nothing to find and the
    // file locked for the life of the Service Worker.
    assert.deepEqual(
        abortedFiles,
        ['quota.bin'],
        'the writable was never aborted, so its exclusive lock is still held',
    );
    assert.deepEqual(await listKeys('docs'), []);
    await assert.rejects(() => storageGet('docs', 'quota.bin'), { name: 'NotFoundError' });

    // And the entry really is gone, so the Rust error path's follow-up abort
    // is a no-op rather than a second cleanup.
    await storagePutStreamAbort(id);
    assert.deepEqual(abortedFiles, ['quota.bin']);
});

test('a sidecar write that fails does not leave a new key served as a valid object', async () => {
    failCloseOn.add(metaName('orphan.bin'));

    const id = await storagePutStreamStart('docs', 'orphan.bin');
    await storagePutStreamChunk(id, new Uint8Array([1, 2, 3]));

    await assert.rejects(() => storagePutStreamFinish(id, 'text/plain'), {
        name: 'QuotaExceededError',
    });

    // The body committed before the sidecar failed, so without cleanup the key
    // would be listed and served under the default content type while the
    // caller was told the upload failed.
    assert.deepEqual(await listKeys('docs'), []);
    await assert.rejects(() => storageGet('docs', 'orphan.bin'), { name: 'NotFoundError' });
});

test('a buffered put is readable through the streaming path, and the reverse', async () => {
    // The two write paths and the two read paths write and read the same
    // object and the same sidecar; nothing about an object records which one
    // produced it.
    await storagePut('docs', 'a.txt', new TextEncoder().encode('hello'), 'text/plain');

    const started = await storageGetStream('docs', 'a.txt');
    assert.equal(started.meta.content_type, 'text/plain');
    assert.equal(started.meta.size, 5);
    assert.deepEqual(await drain(started.stream_id), [104, 101, 108, 108, 111]);
});

test('a sidecar missing a field keeps that field default rather than erasing it', async () => {
    // `GetMeta` on the Rust side has no optional fields, so a partial sidecar
    // that replaced the defaults produced a value neither read path could
    // decode — and on the streaming path that decode failure stranded a
    // registered reader holding an OPFS file handle.
    await storagePut('docs', 'partial.bin', new Uint8Array([7, 7]), 'text/plain');

    // Reach the same directory the bridge writes into: everything lives under
    // `STORAGE_DIR` below the OPFS root, not at the root itself.
    const root = await navigator.storage.getDirectory();
    const folder = await (await root.getDirectoryHandle('storage')).getDirectoryHandle('docs');

    const sidecar = await folder.getFileHandle(metaName('partial.bin'), { create: true });
    const writable = await sidecar.createWritable();
    await writable.write(JSON.stringify({ size: 2 }));
    await writable.close();

    const buffered = await storageGet('docs', 'partial.bin');
    assert.equal(buffered.meta.content_type, 'application/octet-stream');
    const started = await storageGetStream('docs', 'partial.bin');
    assert.equal(started.meta.content_type, 'application/octet-stream');
    await readerCancel(started.stream_id);
});

test('streaming a missing object rejects as NotFoundError, like the buffered read', async () => {
    await assert.rejects(() => storageGetStream('docs', 'absent.txt'), (err) => {
        // `storage.rs::map_rejection` keys on exactly this name to answer
        // `StorageError::NotFound` rather than `Internal`.
        assert.equal(err.name, 'NotFoundError');
        return true;
    });
});

test('an empty object streams as zero chunks, not as an error', async () => {
    const id = await storagePutStreamStart('docs', 'empty.bin');
    await storagePutStreamFinish(id, 'application/octet-stream');

    const started = await storageGetStream('docs', 'empty.bin');
    assert.equal(started.meta.size, 0);
    assert.deepEqual(await drain(started.stream_id), []);
});

test('abandoning a read releases the OPFS reader', async () => {
    await storagePut('docs', 'a.txt', new TextEncoder().encode('hello'), 'text/plain');
    const started = await storageGetStream('docs', 'a.txt');

    await readerCancel(started.stream_id);
    await assert.rejects(() => readerNextChunk(started.stream_id), /unknown stream id/);
});
