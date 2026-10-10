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
// engine and which models it holds, sends the request to the page holding
// its model (a load or an embedding, which loads on demand, may go to any
// page running the engine), refuses at once when no page can take it, and
// fails the request if that page goes away before it answers.
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
    embedUnload,
    llmChatStream,
    llmCancelStream,
    llmNextStreamFrame,
    ENGINE_PROBE_TIMEOUT_MS,
    ENGINE_UNAVAILABLE,
} from '../bridge.js';

/** What `sw.js` does with a page's reply: hand it to bridge.js's hook. */
function deliver(msg) {
    if (msg.type === 'engine-present') globalThis.__impresspressCompleteEngineProbe(msg);
    else if (msg.type.startsWith('embed-')) globalThis.__impresspressCompleteEmbedMessage(msg);
    else if (msg.type.startsWith('image-')) globalThis.__impresspressCompleteImageMessage(msg);
    else if (msg.type.startsWith('llm-')) globalThis.__impresspressCompleteLlmMessage(msg);
}

/**
 * A window client. `engines` maps each family its scripts listen for to the
 * models it has loaded (`{ image: ['janus-pro-1b'] }`), and `loading` to the
 * ones it is loading; it answers a probe after
 * `probeDelayMs`, as a busy tab does. `handle(msg, reply)` answers a request
 * of one of its families (or not, to leave it pending). Every message it is
 * sent is kept in `received`.
 */
function page(id, engines, { handle = () => {}, probeDelayMs = 0, loading = {} } = {}) {
    const received = [];
    return {
        id,
        received,
        requests: () => received.filter((m) => m.type !== 'engine-probe'),
        postMessage(msg) {
            received.push(msg);
            if (msg.type === 'engine-probe') {
                if (!(msg.family in engines)) return;
                setTimeout(
                    () => deliver({
                        type: 'engine-present',
                        id: msg.id,
                        loaded: engines[msg.family],
                        loading: loading[msg.family] ?? [],
                    }),
                    probeDelayMs,
                );
                return;
            }
            queueMicrotask(() => {
                const family = msg.type.split('-')[0];
                if (family in engines) handle(msg, deliver);
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
    const llmOnly = page('a', { llm: [] });
    const embedder = page('b', { embed: [] }, { handle: embedAnswer });
    worker([llmOnly, embedder]);

    const result = await within(embedRun('multilingual-e5-small', '["x"]'), 1_000);
    assert.deepEqual(result, { ok: JSON.stringify({ vectors: [[1, 0]], dims: 2 }) });
    assert.deepEqual(llmOnly.requests(), []);
    assert.equal(embedder.requests().length, 1);
});

test('a generation goes to the tab holding the model, though another tab answers first', async () => {
    // B runs the image engine too and answers the probe at once; the model
    // was loaded in A, which answers later. Sent to B, the generation would
    // be answered "model not loaded" (t2i-engine.js).
    const holder = page('a', { image: ['janus-pro-1b'] }, { probeDelayMs: 50 });
    const quick = page('b', { image: [] });
    worker([quick, holder]);

    const id = await imageStartGenerate('janus-pro-1b', '{"prompt":"x"}');
    await new Promise((r) => setTimeout(r, 100)); // every probe answered
    assert.deepEqual(holder.requests().map((m) => [m.type, m.id]), [['image-generate-stream-request', id]]);
    assert.deepEqual(quick.requests(), []);
    await imageCancelStream(id);
});

test('an embedding goes to the tab holding its pipeline when it answers within the grace', async () => {
    const holder = page('a', { embed: ['multilingual-e5-small'] }, { handle: embedAnswer, probeDelayMs: 50 });
    const quick = page('b', { embed: [] }, { handle: embedAnswer });
    worker([quick, holder]);
    await embedRun('multilingual-e5-small', '["x"]');
    assert.deepEqual(holder.requests().map((m) => m.type), ['embed-run-request']);
    assert.deepEqual(quick.requests(), []);
});

test('a chat goes to the tab that loaded the model, though another tab answers first', async () => {
    const holder = page('a', { llm: ['Llama-3.2-1B'] }, { probeDelayMs: 50 });
    const quick = page('b', { llm: [] });
    worker([quick, holder]);

    const id = await llmChatStream('Llama-3.2-1B', '{"messages":[]}');
    assert.deepEqual(holder.requests().map((m) => m.type), ['llm-chat-stream-request']);
    assert.deepEqual(quick.requests(), []);
    await llmCancelStream(id);
    assert.deepEqual(holder.requests().map((m) => m.type), ['llm-chat-stream-request', 'llm-stream-cancel']);
});

test('a second load goes to the tab already loading the model, not a quicker idle tab', async () => {
    // A is part-way through loading Janus; B answers first holding nothing.
    // Sent to B, the load would download and load the model a second time.
    const loader = page('a', { image: [] }, {
        loading: { image: ['janus-pro-1b'] },
        probeDelayMs: 50,
        handle: (msg, reply) => reply({ type: 'image-load-response', id: msg.id }),
    });
    const idle = page('b', { image: [] }, {
        handle: (msg, reply) => reply({ type: 'image-load-response', id: msg.id }),
    });
    worker([idle, loader]);
    await imageLoadEngine('janus-pro-1b');
    assert.deepEqual(loader.requests().map((m) => m.type), ['image-load-request']);
    assert.deepEqual(idle.requests(), []);
});

test('a generation sent while its model is still loading goes to the loading tab', async () => {
    const loader = page('a', { image: [] }, { loading: { image: ['janus-pro-1b'] } });
    worker([page('b', { image: [] }), loader]);
    const id = await imageStartGenerate('janus-pro-1b', '{"prompt":"x"}');
    assert.deepEqual(loader.requests().map((m) => m.type), ['image-generate-stream-request']);
    await imageCancelStream(id);
});

test('a chat no tab holds the model for is refused, not sent where it cannot run', async () => {
    const pages = [page('a', { llm: [] }), page('b', { llm: ['another-model'] })];
    worker(pages);
    const result = await within(llmChatStream('Llama-3.2-1B', '{"messages":[]}'), 1_000);
    assert.match(String(result.err?.message), /no open page has the LLM model 'Llama-3.2-1B' loaded — load it first/);
    assert.equal(result.err?.code, ENGINE_UNAVAILABLE);
    assert.deepEqual(pages.flatMap((p) => p.requests()), []);
});

test('a load no tab holds the model for goes to the first tab that runs the engine', async () => {
    const first = page('a', { image: [] }, {
        handle: (msg, reply) => reply({ type: 'image-load-response', id: msg.id }),
    });
    const later = page('b', { image: [] }, { probeDelayMs: 50 });
    worker([later, first]);
    await imageLoadEngine('janus-pro-1b');
    assert.deepEqual(first.requests().map((m) => m.type), ['image-load-request']);
    assert.deepEqual(later.requests(), []);
});

test('an unload no tab holds the model for sends nothing and succeeds', async () => {
    const pages = [page('a', { embed: ['another-model'] })];
    worker(pages);
    const result = await within(embedUnload('multilingual-e5-small'), 1_000);
    assert.deepEqual(result, { ok: undefined });
    assert.deepEqual(pages[0].requests(), []);
});

test('no page runs the engine: the request is refused within the probe bound', async () => {
    worker([page('a', { llm: [] }), page('b', {})]);

    const started = Date.now();
    const result = await within(embedRun('multilingual-e5-small', '["x"]'), ENGINE_PROBE_TIMEOUT_MS + 1_000);
    assert.notEqual(result, 'pending', 'the request hung');
    assert.match(String(result.err?.message), /no open page runs the embedding engine/);
    assert.equal(result.err?.code, ENGINE_UNAVAILABLE);
    assert.ok(Date.now() - started >= ENGINE_PROBE_TIMEOUT_MS - 50);
});

test('no page open at all: refused at once', async () => {
    worker([]);
    const result = await within(imageLoadEngine('janus-pro-1b'), 200);
    assert.match(String(result.err?.message), /no open page runs the image engine/);
    assert.equal(result.err?.code, ENGINE_UNAVAILABLE);
});

test('the page running a request goes away: the request fails instead of waiting', async () => {
    const pages = worker([page('a', { embed: [] } /* never answers the request */)]);
    const request = embedRun('multilingual-e5-small', '["x"]');
    await new Promise((r) => setTimeout(r, 50));
    pages.length = 0; // the tab is closed

    const result = await within(request, 5_000);
    assert.notEqual(result, 'pending', 'the request hung');
    assert.match(String(result.err?.message), /page running the embedding engine was closed/);
    assert.equal(result.err?.code, ENGINE_UNAVAILABLE);
});

test('a stream with no page to run it is refused before it starts', async () => {
    worker([page('a', { llm: [] })]);
    const result = await within(imageStartGenerate('janus-pro-1b', '{"prompt":"x"}'), ENGINE_PROBE_TIMEOUT_MS + 1_000);
    assert.match(String(result.err?.message), /no open page runs the image engine/);
});

test('a stream whose page goes away ends with an error frame', async () => {
    const pages = worker([page('a', { llm: ['m'] } /* never answers the request */)]);
    const id = await llmChatStream('m', '{"messages":[]}');
    pages.length = 0;
    const frame = await within(llmNextStreamFrame(id), 5_000);
    assert.notEqual(frame, 'pending', 'the stream hung');
    const ended = JSON.parse(frame.ok);
    assert.match(ended.payload, /page running the LLM engine was closed/);
    assert.equal(ended.code, ENGINE_UNAVAILABLE);
});

test('a cancel reaches the page running the stream', async () => {
    const other = page('a', {});
    const runner = page('b', { image: ['janus-pro-1b'] });
    worker([other, runner]);
    const id = await imageStartGenerate('janus-pro-1b', '{"prompt":"x"}');
    await imageCancelStream(id);
    assert.deepEqual(
        runner.received.filter((m) => m.type === 'image-stream-cancel').map((m) => m.id),
        [id],
    );
    assert.equal(other.received.filter((m) => m.type === 'image-stream-cancel').length, 0);
    assert.equal(JSON.parse(await imageNextFrame(id)).kind, 'error');
});
