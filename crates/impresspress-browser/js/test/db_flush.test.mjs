// Run with: node --import ./js/test/node-hooks.mjs --test js/test/db_flush.test.mjs
// (see node-hooks.mjs's header comment for why the --import hook is needed).
//
// `dbFlush` exports the whole sql.js database and writes it to OPFS through
// `createWritable()`, whose swap file is committed by `close()`. Two
// overlapping calls would each open a swap file of their own, and the LAST
// `close` to land would win — so an earlier, larger export that finishes after
// a later one would put an older snapshot back on disk. The service worker
// handles several requests at once, so two flushes can overlap (each request's
// flush scope flushes at its own end). These tests pin that `dbFlush` calls
// are serialized: each one exports only after the previous one has written,
// so the file on disk is always the newest export.
//
// The OPFS here is in memory, like `database.rs`'s wasm `test_support`, with
// one addition: a `close()` can be held open until the test lets it finish.
import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { dbInit, dbExecRaw, dbFlush, dbQueryRaw } from '../bridge.js';

const DB_FILENAME = 'impresspress.db';

/** The committed files of the in-memory OPFS. */
let files;
/** Releases the `close()` being held, once one is. */
let releaseHeldClose;
/** Whether the next writable's `close()` waits for the test to release it. */
let holdNextClose;
/** Whether the next `getDirectory()` fails, as a lost OPFS would. */
let failNextDirectory;

function installMemoryOpfs() {
    files = new Map();
    releaseHeldClose = null;
    holdNextClose = false;
    failNextDirectory = false;
    const handle = (name) => ({
        async getFile() {
            const data = files.get(name);
            return { async arrayBuffer() { return data.slice().buffer; } };
        },
        async createWritable() {
            let data = new Uint8Array(0);
            const held = holdNextClose;
            holdNextClose = false;
            return {
                async write(chunk) { data = chunk; },
                async close() {
                    if (held) {
                        await new Promise((release) => {
                            releaseHeldClose = release;
                        });
                    }
                    files.set(name, data);
                },
            };
        },
    });
    const root = {
        async getFileHandle(name, options) {
            if (!files.has(name)) {
                if (!(options && options.create)) {
                    throw new DOMException('no such file', 'NotFoundError');
                }
                files.set(name, new Uint8Array(0));
            }
            return handle(name);
        },
    };
    Object.defineProperty(globalThis.navigator, 'storage', {
        configurable: true,
        value: {
            async getDirectory() {
                if (failNextDirectory) {
                    failNextDirectory = false;
                    throw new Error('quota');
                }
                return root;
            },
        },
    });
}

/** Let every pending promise job and timer run. */
function settle() {
    return new Promise((resolve) => setTimeout(resolve, 20));
}

async function rowsOnDisk() {
    // Reopen from the file the flushes left behind.
    await dbInit();
    return dbQueryRaw('SELECT id FROM t ORDER BY id', []).values.map(([id]) => id);
}

beforeEach(async () => {
    installMemoryOpfs();
    await dbInit();
    dbExecRaw('CREATE TABLE t (id TEXT PRIMARY KEY)', []);
    await dbFlush();
});

// **Fails without the serialization**: the second flush opens its own swap
// file and commits {a, b} while the first is still inside `close()`; the first
// then commits {a} over it.
test('a flush that finishes late cannot put an older snapshot back', async () => {
    dbExecRaw("INSERT INTO t (id) VALUES ('a')", []);
    holdNextClose = true;
    const first = dbFlush();
    dbExecRaw("INSERT INTO t (id) VALUES ('b')", []);
    const second = dbFlush();

    await settle();
    assert.ok(releaseHeldClose, 'the first flush is inside its close()');
    releaseHeldClose();
    await Promise.all([first, second]);

    assert.deepEqual(await rowsOnDisk(), ['a', 'b'], 'the newest export is on disk');
});

// Each caller's promise resolves only after an export that holds its own
// mutations has been written, even while an earlier flush is still running.
test('a caller is answered only after its mutations are on disk', async () => {
    dbExecRaw("INSERT INTO t (id) VALUES ('a')", []);
    holdNextClose = true;
    const first = dbFlush();
    dbExecRaw("INSERT INTO t (id) VALUES ('b')", []);
    let secondDone = false;
    const second = dbFlush().then(() => {
        secondDone = true;
    });

    await settle();
    assert.equal(secondDone, false, 'the second flush waits for the first');

    releaseHeldClose();
    await first;
    await second;
    const onDisk = new Set(await rowsOnDisk());
    assert.ok(onDisk.has('b'), 'the second caller\'s row is on disk when it is answered');
});

// A failed flush fails its own caller and does not stop the next one.
test('a failed flush does not block the next', async () => {
    dbExecRaw("INSERT INTO t (id) VALUES ('a')", []);
    failNextDirectory = true;
    const failed = dbFlush();
    const next = dbFlush();

    await assert.rejects(failed, /quota/);
    await next;
    assert.deepEqual(await rowsOnDisk(), ['a']);
});

// `dbQueryRaw` answers sql.js's positional shape, so the Rust side sees the
// SELECT's column order exactly — an object per row would enumerate the
// integer-like `1` first and keep only one of the two `v` columns.
test('a query answers its columns in SELECT order', () => {
    dbExecRaw("INSERT INTO t (id) VALUES ('a')", []);
    assert.deepEqual(dbQueryRaw("SELECT 'x' AS b, 1, id, 2 AS v, 3 AS v FROM t", []), {
        columns: ['b', '1', 'id', 'v', 'v'],
        values: [['x', 1, 'a', 2, 3]],
    });
    assert.deepEqual(dbQueryRaw("SELECT id FROM t WHERE id = 'none'", []), {
        columns: [],
        values: [],
    });
});
