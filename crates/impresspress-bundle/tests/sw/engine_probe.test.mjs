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
// cannot load. Every such import is answered with a stand-in whose
// `pipeline()` (Transformers.js's entry point, the one the embedding engine
// calls) counts its calls and waits on a gate the test opens.
//
// And every engine loads as an ES module, as a page's `<script type="module">`
// loads it. Node would otherwise take a script with no `import`/`export` of
// its own (embed-engine.js) for CommonJS, whose cache ignores the query
// string, so a second load would be the first one again.
register(
    'data:text/javascript,' +
        encodeURIComponent(`
export async function resolve(specifier, context, next) {
    if (specifier.startsWith('https://cdn.jsdelivr.net/')) {
        return { url: 'stub:transformers', shortCircuit: true };
    }
    return next(specifier, context);
}
export async function load(url, context, next) {
    if (url.startsWith('file:') && /-engine[.]js([?]|$)/.test(url)) {
        return { ...(await next(url, { ...context, format: 'module' })), format: 'module' };
    }
    if (url === 'stub:transformers') {
        return {
            format: 'module',
            shortCircuit: true,
            source: 'export async function pipeline(task, model) {' +
                ' globalThis.__pipelineCalls.push(model); await globalThis.__pipelineGate;' +
                ' return async () => ({ tolist: () => [] }); }',
        };
    }
    return next(url, context);
}`),
);

const FAMILIES = ['llm', 'image', 'embed'];

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
        value: { serviceWorker: sw.container },
        configurable: true,
    });
    // A query string makes each load its own module instance.
    await import(`${pathToFileURL(script).href}?probe=${Math.random()}`);
    return sw;
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
    let open;
    globalThis.__pipelineCalls = [];
    globalThis.__pipelineGate = new Promise((resolve) => { open = resolve; });
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
    assert.equal(globalThis.__pipelineCalls.length, 1, 'the model was loaded twice');
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
