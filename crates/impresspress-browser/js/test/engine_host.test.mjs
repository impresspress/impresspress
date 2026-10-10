// Run with: node --import ./js/test/node-hooks.mjs --test js/test/engine_host.test.mjs
// (see node-hooks.mjs's header comment for why the --import hook is needed).
//
// Which page runs a page-side engine. The runtime's browser services run their
// models in a window (WebGPU is window-only): bridge.js posts a request to an
// open page and waits for that page's engine script to answer. It used to
// post to `clients[0]` and wait unconditionally, so a request reaching a page
// without that engine's script — a runtime page that loaded only the LLM
// engine, a site page with no engine at all — was never answered and the
// request hung. Now bridge.js asks every open page which of them runs the
// engine, sends the request to one that says so, refuses at once when none
// does, and fails the request if that page goes away before it answers.
//
// The pages here are stand-ins for `navigator.serviceWorker` listeners; what
// they post back goes through the same `globalThis.__impresspressComplete*`
// hooks `sw.js` routes page replies to.
import { test, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import {
    embedRun,
    imageLoadEngine,
    imageStartGenerate,
    imageNextFrame,
    imageCancelStream,
    llmChatStream,
    llmNextStreamFrame,
    ENGINE_PROBE_TIMEOUT_MS,
} from '../bridge.js';

/** What `sw.js` does with a page's reply: hand it to bridge.js's hook. */
function deliver(msg) {
    if (msg.type === 'engine-present') globalThis.__impresspressCompleteEngineProbe(msg);
    else if (msg.type.startsWith('embed-')) globalThis.__impresspressCompleteEmbedMessage(msg);
    else if (msg.type.startsWith('image-')) globalThis.__impresspressCompleteImageMessage(msg);
    else if (msg.type.startsWith('llm-')) globalThis.__impresspressCompleteLlmMessage(msg);
}

/**
 * A window client. `engines` are the families its scripts listen for;
 * `handle(msg)` answers a request of one of them (or not, to leave it
 * pending). Every message it is sent is kept in `received`.
 */
function page(id, engines, handle = () => {}) {
    const received = [];
    return {
        id,
        received,
        postMessage(msg) {
            received.push(msg);
            queueMicrotask(() => {
                if (msg.type === 'engine-probe') {
                    if (engines.includes(msg.family)) deliver({ type: 'engine-present', id: msg.id });
                    return;
                }
                const family = msg.type.split('-')[0];
                if (engines.includes(family)) handle(msg, deliver);
            });
        },
    };
}

/** Install a worker global whose open windows are `pages` (mutable). */
function worker(pages) {
    globalThis.self = {
        clients: {
            matchAll: async (opts) => {
                assert.deepEqual(opts, { type: 'window', includeUncontrolled: false });
                return [...pages];
            },
            get: async (id) => pages.find((p) => p.id === id),
        },
    };
    return pages;
}

const embedAnswer = (msg, reply) =>
    reply({ type: 'embed-run-response', id: msg.id, result: JSON.stringify({ vectors: [[1, 0]], dims: 2 }) });

/** Settles to 'pending' when `promise` has not settled within `ms`. */
async function within(promise, ms) {
    let timer;
    const pending = new Promise((resolve) => {
        timer = setTimeout(() => resolve('pending'), ms);
    });
    try {
        return await Promise.race([promise.then((v) => ({ ok: v }), (e) => ({ err: e })), pending]);
    } finally {
        clearTimeout(timer);
    }
}

afterEach(() => {
    delete globalThis.self;
});

test('the request goes to the page that runs the engine, not the first page', async () => {
    const llmOnly = page('a', ['llm']);
    const embedder = page('b', ['embed'], embedAnswer);
    worker([llmOnly, embedder]);

    const result = await within(embedRun('multilingual-e5-small', '["x"]'), 1_000);
    assert.deepEqual(result, { ok: JSON.stringify({ vectors: [[1, 0]], dims: 2 }) });
    assert.equal(llmOnly.received.filter((m) => m.type === 'embed-run-request').length, 0);
    assert.equal(embedder.received.filter((m) => m.type === 'embed-run-request').length, 1);
});

test('one request is sent once, to one page, even when several run the engine', async () => {
    const one = page('a', ['embed'], embedAnswer);
    const two = page('b', ['embed'], embedAnswer);
    worker([one, two]);

    await embedRun('multilingual-e5-small', '["x"]');
    const sent = [one, two].flatMap((p) => p.received.filter((m) => m.type === 'embed-run-request'));
    assert.equal(sent.length, 1);
});

test('no page runs the engine: the request is refused within the probe bound', async () => {
    worker([page('a', ['llm']), page('b', [])]);

    const started = Date.now();
    const result = await within(embedRun('multilingual-e5-small', '["x"]'), ENGINE_PROBE_TIMEOUT_MS + 1_000);
    assert.notEqual(result, 'pending', 'the request hung');
    assert.match(String(result.err?.message), /no open page runs the embedding engine/);
    assert.ok(Date.now() - started >= ENGINE_PROBE_TIMEOUT_MS - 50);
});

test('no page open at all: refused at once', async () => {
    worker([]);
    const result = await within(imageLoadEngine('janus-pro-1b'), 200);
    assert.match(String(result.err?.message), /no open page runs the image engine/);
});

test('the page running a request goes away: the request fails instead of waiting', async () => {
    const pages = worker([page('a', ['embed'] /* never answers */)]);
    const request = embedRun('multilingual-e5-small', '["x"]');
    await new Promise((r) => setTimeout(r, 50));
    pages.length = 0; // the tab is closed

    const result = await within(request, 5_000);
    assert.notEqual(result, 'pending', 'the request hung');
    assert.match(String(result.err?.message), /page running the embedding engine was closed/);
});

test('a stream with no page to run it is refused before it starts', async () => {
    worker([page('a', ['llm'])]);
    const result = await within(imageStartGenerate('{"prompt":"x"}'), ENGINE_PROBE_TIMEOUT_MS + 1_000);
    assert.match(String(result.err?.message), /no open page runs the image engine/);
});

test('a stream whose page goes away ends with an error frame', async () => {
    const pages = worker([page('a', ['llm'] /* never answers */)]);
    const id = await llmChatStream('{"messages":[]}');
    pages.length = 0;
    const frame = await within(llmNextStreamFrame(id), 5_000);
    assert.notEqual(frame, 'pending', 'the stream hung');
    assert.match(JSON.parse(frame.ok).payload, /page running the LLM engine was closed/);
});

test('a cancel reaches the page running the stream', async () => {
    const other = page('a', []);
    const runner = page('b', ['image']);
    worker([other, runner]);
    const id = await imageStartGenerate('{"prompt":"x"}');
    await imageCancelStream(id);
    assert.deepEqual(
        runner.received.filter((m) => m.type === 'image-stream-cancel').map((m) => m.id),
        [id],
    );
    assert.equal(other.received.filter((m) => m.type === 'image-stream-cancel').length, 0);
    assert.equal(JSON.parse(await imageNextFrame(id)).kind, 'error');
});
