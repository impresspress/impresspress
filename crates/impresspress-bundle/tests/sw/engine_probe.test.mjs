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

/** The families `script` answers a probe for, and what each answer said. */
async function probe(script) {
    const sw = serviceWorkerContainer();
    Object.defineProperty(globalThis, 'navigator', {
        value: { serviceWorker: sw.container },
        configurable: true,
    });
    // A query string makes each load its own module instance.
    await import(`${pathToFileURL(script).href}?probe=${Math.random()}`);
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
            { type: 'engine-present', id: `probe-${family}`, loaded: [] },
        ]);
        assert.equal(owners[family], undefined, `${family} answered by ${owners[family]} and ${script}`);
        owners[family] = script;
    }
    assert.deepEqual(Object.keys(owners).sort(), [...FAMILIES].sort(), 'a family no engine answers');
});
