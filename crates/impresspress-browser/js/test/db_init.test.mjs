// Run with: node --import ./js/test/node-hooks.mjs --test js/test/db_init.test.mjs
// (see node-hooks.mjs's header comment for why the --import hook is needed).
//
// `dbInit` opens the database stored in OPFS. Only a file that does not exist
// (`getFileHandle` rejecting with `NotFoundError`) means a first run, which
// opens an empty database. Any other failure to read it rejects with an error
// named `DatabaseUnreadable` and leaves no database open. Treating every
// failure as "no file yet" turned a transient read error into a wipe: boot
// opened an empty database, migrated it, and the flush that followed wrote it
// over the user's data with `createWritable()`.
//
// The OPFS here is in memory, like `db_flush.test.mjs`'s, with two additions:
// the next `getFile()` can be made to reject, and every committed `close()`
// is counted.
import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';
import { dbInit, dbExecRaw, dbFlush, dbQueryRaw } from '../bridge.js';

const DB_FILENAME = 'impresspress.db';

/** The committed files of the in-memory OPFS. */
let files;
/** How many `close()`s committed a file. */
let commits;
/** The `DOMException` name the next `getFile()` rejects with, if any. */
let failNextRead;

function installMemoryOpfs() {
    files = new Map();
    commits = 0;
    failNextRead = null;
    const handle = (name) => ({
        async getFile() {
            if (failNextRead !== null) {
                const failure = failNextRead;
                failNextRead = null;
                throw new DOMException('the file could not be read', failure);
            }
            const data = files.get(name);
            return { async arrayBuffer() { return data.slice().buffer; } };
        },
        async createWritable() {
            let data = new Uint8Array(0);
            return {
                async write(chunk) { data = chunk; },
                async close() {
                    files.set(name, data);
                    commits += 1;
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
        value: { async getDirectory() { return root; } },
    });
}

beforeEach(() => {
    installMemoryOpfs();
});

// **Fails with the old `catch (_e) {}`**: `dbInit` resolves on an empty
// database, and the flush boot makes next writes it over the stored rows.
test('a file that could not be read is neither opened empty nor written over', async () => {
    await dbInit();
    dbExecRaw('CREATE TABLE t (id TEXT PRIMARY KEY)', []);
    dbExecRaw("INSERT INTO t (id) VALUES ('kept')", []);
    await dbFlush();
    const stored = files.get(DB_FILENAME);
    const committed = commits;

    failNextRead = 'NotReadableError';
    const failure = await dbInit().then(
        () => assert.fail('dbInit resolved over a file it could not read'),
        (e) => e,
    );
    assert.equal(failure.name, 'DatabaseUnreadable');
    assert.equal(failure.cause.name, 'NotReadableError');
    assert.match(String(failure), /^DatabaseUnreadable: .*NotReadableError/);

    // What boot does next: a flush. With no database open it writes nothing.
    await dbFlush();
    assert.equal(commits, committed, 'no close() was committed');
    assert.equal(files.get(DB_FILENAME), stored, 'the stored bytes are untouched');

    // The next open reads the file, and the row is there.
    await dbInit();
    assert.deepEqual(dbQueryRaw('SELECT id FROM t', []).values, [['kept']]);
});

// The browser reports a missing file as `NotFoundError` (the File System spec,
// and every engine): that alone is a first run.
test('no file yet is a first run: an empty database, and a flush creates the file', async () => {
    await dbInit();
    assert.deepEqual(dbQueryRaw("SELECT name FROM sqlite_master WHERE type = 'table'", []).values, []);
    assert.equal(files.has(DB_FILENAME), false);

    dbExecRaw('CREATE TABLE t (id TEXT PRIMARY KEY)', []);
    await dbFlush();
    assert.equal(commits, 1);
    assert.ok(files.get(DB_FILENAME).byteLength > 0, 'the flush wrote the database');
});

// Every other failure before the bytes reach sql.js is unreadable too: here,
// OPFS itself is unavailable.
test('an OPFS that cannot be opened is unreadable, not a first run', async () => {
    Object.defineProperty(globalThis.navigator, 'storage', {
        configurable: true,
        value: {
            async getDirectory() {
                throw new DOMException('storage is blocked', 'SecurityError');
            },
        },
    });

    const failure = await dbInit().then(
        () => assert.fail('dbInit resolved with no OPFS'),
        (e) => e,
    );
    assert.equal(failure.name, 'DatabaseUnreadable');
    assert.equal(failure.cause.name, 'SecurityError');
});
