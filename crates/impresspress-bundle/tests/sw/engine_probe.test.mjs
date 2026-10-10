// Run by `bundle_integration.rs`'s `the_engine_scripts_answer_the_probe_for_their_own_engine`,
// which hands it the shipped page engines in `PAGE_ENGINES_JSON` (a JSON file
// of their paths on disk, from `impresspress_bundle::assets::PAGE_ENGINES`).
//
// The worker's bridge (`impresspress-browser/js/bridge.js`, "Page-side
// engines") sends an LLM, image or embedding request only to a page that has
// answered its `engine-probe` for that family with `engine-present`, and
// routes by the `loaded` model list that answer carries. So the probe answer
// is each engine script's half of the contract: a script that does not answer
// is a page the worker never uses, and one that answers for another family's
// probe is a page the worker sends requests nothing there listens for — the
// request hang this protocol exists to end.
//
// The families are the protocol's (bridge.js's request families), not a list
// of scripts: every family must be answered by exactly one shipped engine.
// Each script is loaded as it ships, under a stand-in
// `navigator.serviceWorker` that records what it posts back to the worker.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { register } from 'node:module';

// The engines import their model libraries from cdn.jsdelivr.net, which Node
// cannot load. Every such import is answered with `cdn-stub.mjs`, whose model
// loads wait on a gate the test opens and whose model work is recorded.
//
// And every engine loads as an ES module, as a page's `<script type="module">`
// loads it. Node would otherwise take a script with no `import`/`export` of
// its own (embed-engine.js) for CommonJS, whose cache ignores the query
// string, so a second load would be the first one again.
register(
    'data:text/javascript,' +
        encodeURIComponent(`
let stubUrl;
export async function initialize(data) {
    stubUrl = data.stubUrl;
}
export async function resolve(specifier, context, next) {
    if (specifier.startsWith('https://cdn.jsdelivr.net/')) {
        return { url: stubUrl, shortCircuit: true };
    }
    return next(specifier, context);
}
export async function load(url, context, next) {
    if (url.startsWith('file:') && /-engine[.]js([?]|$)/.test(url)) {
        return { ...(await next(url, { ...context, format: 'module' })), format: 'module' };
    }
    return next(url, context);
}`),
    { data: { stubUrl: new URL('./cdn-stub.mjs', import.meta.url).href } },
);

const FAMILIES = ['llm', 'image', 'embed'];

// The refusal code of the worker's bridge (`ENGINE_UNAVAILABLE` in
// impresspress-browser's `js/bridge.js`), read from bridge.js's own source, so
// an engine whose copy of it drifts fails here. (Read, not imported: bridge.js
// imports the site's `/vendor/sql-wasm-esm.js`, which only a worker resolves.)
const BRIDGE_JS = new URL('../../../impresspress-browser/js/bridge.js', import.meta.url);
const ENGINE_UNAVAILABLE = readFileSync(BRIDGE_JS, 'utf8').match(
    /^export const ENGINE_UNAVAILABLE = '([^']+)';$/m,
)?.[1];
assert.ok(ENGINE_UNAVAILABLE, `no ENGINE_UNAVAILABLE in ${BRIDGE_JS}`);
// The stand-in Transformers.js module's `env`: the instance the engines import.
const { env: transformersEnv } = await import(new URL('./cdn-stub.mjs', import.meta.url).href);

const listPath = process.env.PAGE_ENGINES_JSON;
assert.ok(listPath, 'PAGE_ENGINES_JSON is not set: run this through bundle_integration.rs');
const ENGINES = JSON.parse(readFileSync(listPath, 'utf8'));

/** A page's `navigator.serviceWorker`: its listeners, and what it posted to the worker. */
function serviceWorkerContainer() {
    const listeners = [];
    const posted = [];
    return {
        posted,
        container: {
            addEventListener(type, fn) {
                if (type === 'message') listeners.push(fn);
            },
            ready: Promise.resolve({ active: { postMessage: (msg) => posted.push(msg) } }),
        },
        /** The worker posting `data` to this page. */
        async deliver(data) {
            for (const fn of listeners) fn({ data });
            // Replies go out after `await navigator.serviceWorker.ready`.
            await new Promise((r) => setTimeout(r, 0));
        },
    };
}

/** Load `script` as a page would, under a fresh stand-in container. */
async function loadEngine(script) {
    const sw = serviceWorkerContainer();
    Object.defineProperty(globalThis, 'navigator', {
        // A WebGPU adapter without `shader-f16`, for the image engine.
        value: {
            serviceWorker: sw.container,
            gpu: { requestAdapter: async () => ({ features: new Set() }) },
        },
        configurable: true,
    });
    // A query string makes each load its own module instance.
    sw.module = await import(`${pathToFileURL(script).href}?probe=${Math.random()}`);
    return sw;
}

/** A closed model gate, and the function that opens it. */
function closeModelGate() {
    let open;
    globalThis.__modelCalls = [];
    globalThis.__modelLoadError = undefined;
    globalThis.__modelGate = new Promise((resolve) => { open = resolve; });
    return open;
}

/** The families `script` answers a probe for, and what each answer said. */
async function probe(script) {
    const sw = await loadEngine(script);
    const answered = {};
    for (const family of FAMILIES) {
        const before = sw.posted.length;
        await sw.deliver({ type: 'engine-probe', id: `probe-${family}`, family });
        const replies = sw.posted.slice(before);
        if (replies.length > 0) answered[family] = replies;
    }
    return answered;
}

test('every page engine answers the probe for exactly one family, holding no model yet', async () => {
    assert.ok(ENGINES.length > 0, 'no page engines listed');
    const owners = {};
    for (const script of ENGINES) {
        const answered = await probe(script);
        const families = Object.keys(answered);
        assert.equal(families.length, 1, `${script} answered ${JSON.stringify(families)}`);
        const [family] = families;
        assert.deepEqual(answered[family], [
            { type: 'engine-present', id: `probe-${family}`, loaded: [], loading: [] },
        ]);
        assert.equal(owners[family], undefined, `${family} answered by ${owners[family]} and ${script}`);
        owners[family] = script;
    }
    assert.deepEqual(Object.keys(owners).sort(), [...FAMILIES].sort(), 'a family no engine answers');
});

/** The shipped script that answers `family`'s probe. */
async function engineFor(family) {
    for (const script of ENGINES) {
        if (family in (await probe(script))) return script;
    }
    throw new Error(`no engine answers ${family}`);
}

test('a model being loaded is reported as loading, and a second load shares the first', async () => {
    // The embedding engine is the one whose load the worker drives over
    // messages alone, so it is where this is observable without a GPU.
    const sw = await loadEngine(await engineFor('embed'));
    const open = closeModelGate();
    const model = 'multilingual-e5-small';

    await sw.deliver({ type: 'embed-create-request', id: 'load-1', modelId: model });
    await sw.deliver({ type: 'engine-probe', id: 'mid-load', family: 'embed' });
    assert.deepEqual(sw.posted.at(-1), {
        type: 'engine-present',
        id: 'mid-load',
        loaded: [],
        loading: [model],
    });

    await sw.deliver({ type: 'embed-create-request', id: 'load-2', modelId: model });
    open();
    await new Promise((r) => setTimeout(r, 10));
    assert.deepEqual(globalThis.__modelCalls, [`pipeline:Xenova/${model}`], 'the model was loaded twice');
    const replies = sw.posted.filter((m) => m.type === 'embed-create-response');
    assert.deepEqual(replies.map((m) => [m.id, m.result]).sort(), [['load-1', 'ok'], ['load-2', 'ok']]);

    await sw.deliver({ type: 'engine-probe', id: 'after-load', family: 'embed' });
    assert.deepEqual(sw.posted.at(-1), {
        type: 'engine-present',
        id: 'after-load',
        loaded: [model],
        loading: [],
    });
});

/** The frames `sw` posted for stream `id`. */
const framesOf = (sw, type, id) => sw.posted.filter((m) => m.type === type && m.id === id);

test('a chat cancelled while its model is still loading never starts', async () => {
    const sw = await loadEngine(await engineFor('llm'));
    const open = closeModelGate();
    const loading = sw.module.loadEngine('Llama-3.2-1B'); // page-direct, as a page loads WebLLM

    await sw.deliver({ type: 'llm-chat-stream-request', id: 'c1', body: '{"messages":[]}' });
    await sw.deliver({ type: 'llm-stream-cancel', id: 'c1' });
    open();
    await loading;
    await new Promise((r) => setTimeout(r, 10));

    assert.ok(!globalThis.__modelCalls.includes('chat'), `the chat ran: ${globalThis.__modelCalls}`);
    assert.deepEqual(
        framesOf(sw, 'llm-stream-frame', 'c1').map((f) => [f.kind, f.payload]),
        [['error', 'cancelled']],
    );
});

test('a generation cancelled while its model is still loading never starts', async () => {
    const sw = await loadEngine(await engineFor('image'));
    const open = closeModelGate();

    await sw.deliver({ type: 'image-load-request', id: 'l1', modelId: 'janus-pro-1b' });
    await sw.deliver({ type: 'image-generate-stream-request', id: 'g1', body: '{"prompt":"x"}' });
    await sw.deliver({ type: 'image-stream-cancel', id: 'g1' });
    open();
    await new Promise((r) => setTimeout(r, 20));

    assert.deepEqual(framesOf(sw, 'image-load-response', 'l1'), [{ type: 'image-load-response', id: 'l1' }]);
    assert.ok(!globalThis.__modelCalls.includes('generate'), `the generation ran: ${globalThis.__modelCalls}`);
    assert.deepEqual(
        framesOf(sw, 'image-stream-frame', 'g1').map((f) => [f.kind, f.payload]),
        [['error', 'cancelled']],
    );
});

/** Let the engine settle what it was sent: its replies go out asynchronously. */
const settle = () => new Promise((r) => setTimeout(r, 20));

test('the Transformers.js engines run ONNX Runtime on one thread, set before their first model loads', async () => {
    // With more than one, a cross-origin-isolated page has ONNX Runtime import
    // its wasm glue from a `blob:` URL, which the runtime's pages refuse as a
    // script source, and no backend loads (embed-engine.js says why). ONNX
    // Runtime reads the setting once, when it creates its first session, so
    // it has to be in place before the first model is created — on whichever
    // engine the page asks first.
    const loads = [
        ['embed', (sw) => sw.deliver({ type: 'embed-create-request', id: 'e1', modelId: 'multilingual-e5-small' })],
        ['image', (sw) => sw.deliver({ type: 'image-load-request', id: 'i1', modelId: 'janus-pro-1b' })],
    ];
    for (const [family, startLoad] of loads) {
        const sw = await loadEngine(await engineFor(family));
        closeModelGate()();
        delete transformersEnv.backends.onnx.wasm.numThreads;
        globalThis.__onnxThreadsAtLoad = [];
        await startLoad(sw);
        await settle();
        assert.ok(globalThis.__onnxThreadsAtLoad.length > 0, `${family}: no model was loaded`);
        assert.deepEqual(
            [...new Set(globalThis.__onnxThreadsAtLoad)],
            [1],
            `${family}: numThreads when its models loaded`,
        );
    }
    globalThis.__onnxThreadsAtLoad = undefined;
});

test('an embedding engine that cannot load refuses with the refusal code and its reason', async () => {
    const sw = await loadEngine(await engineFor('embed'));
    closeModelGate()();
    globalThis.__modelLoadError = 'no available backend found';

    await sw.deliver({ type: 'embed-run-request', id: 'r1', modelId: 'multilingual-e5-small', texts: '["a"]' });
    await sw.deliver({ type: 'embed-create-request', id: 'c1', modelId: 'multilingual-e5-small' });
    await settle();

    for (const [type, id] of [['embed-run-response', 'r1'], ['embed-create-response', 'c1']]) {
        assert.deepEqual(framesOf(sw, type, id), [{
            type,
            id,
            error: 'the embedding engine could not load in the page: no available backend found',
            code: ENGINE_UNAVAILABLE,
        }]);
    }
});

test('an embedding that fails once its engine has loaded is a fault, not a refusal', async () => {
    const sw = await loadEngine(await engineFor('embed'));
    closeModelGate()();
    // Not a JSON array: the run itself throws, after the pipeline loaded.
    await sw.deliver({ type: 'embed-run-request', id: 'bad', modelId: 'multilingual-e5-small', texts: 'not json' });
    await settle();
    const [reply] = framesOf(sw, 'embed-run-response', 'bad');
    assert.ok(reply.error, `no error: ${JSON.stringify(reply)}`);
    assert.equal(reply.code, undefined, 'a failed run was reported as a refusal');
});

test('an image engine that cannot load refuses with the refusal code and its reason', async () => {
    const sw = await loadEngine(await engineFor('image'));
    closeModelGate()();
    globalThis.__modelLoadError = 'no available backend found';

    await sw.deliver({ type: 'image-load-request', id: 'l1', modelId: 'janus-pro-1b' });
    await settle();
    assert.deepEqual(framesOf(sw, 'image-load-response', 'l1'), [{
        type: 'image-load-response',
        id: 'l1',
        error: 'the image engine could not load in the page: no available backend found',
        code: ENGINE_UNAVAILABLE,
    }]);
});

test('a page without WebGPU refuses an image load, keeping the marker callers match on', async () => {
    const sw = await loadEngine(await engineFor('image'));
    closeModelGate()();
    Object.defineProperty(globalThis, 'navigator', {
        value: { serviceWorker: globalThis.navigator.serviceWorker },
        configurable: true,
    });

    await sw.deliver({ type: 'image-load-request', id: 'l1', modelId: 'janus-pro-1b' });
    await settle();
    const [reply] = framesOf(sw, 'image-load-response', 'l1');
    assert.equal(reply.code, ENGINE_UNAVAILABLE);
    assert.match(reply.error, /^the image engine could not load in the page: webgpu-unavailable: /);
});

test('a request whose model failed to load in the page is refused, not run', async () => {
    // Sent while its page reported the model as loading, which the worker
    // routes to that page; the load then failed. The engine cannot take it.
    const image = await loadEngine(await engineFor('image'));
    let open = closeModelGate();
    globalThis.__modelLoadError = 'download failed';
    await image.deliver({ type: 'image-load-request', id: 'l1', modelId: 'janus-pro-1b' });
    await image.deliver({ type: 'image-generate-stream-request', id: 'g1', body: '{"prompt":"x"}' });
    open();
    await settle();
    assert.deepEqual(
        framesOf(image, 'image-stream-frame', 'g1').map((f) => [f.kind, f.code]),
        [['error', ENGINE_UNAVAILABLE]],
    );

    const llm = await loadEngine(await engineFor('llm'));
    open = closeModelGate();
    globalThis.__modelLoadError = 'download failed';
    const loading = llm.module.loadEngine('Llama-3.2-1B').catch(() => {});
    await llm.deliver({ type: 'llm-chat-stream-request', id: 'c1', body: '{"messages":[]}' });
    open();
    await loading;
    await settle();
    assert.deepEqual(
        framesOf(llm, 'llm-stream-frame', 'c1').map((f) => [f.kind, f.code]),
        [['error', ENGINE_UNAVAILABLE]],
    );
    assert.ok(!globalThis.__modelCalls.includes('chat'), 'the chat ran');
});
