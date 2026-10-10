// Run by `bundle_integration.rs`'s `the_engine_scripts_answer_the_probe_for_their_own_engine`
// (or by hand: node --test crates/impresspress-bundle/tests/sw/engine_probe.test.mjs).
//
// The worker's bridge (`impresspress-browser/js/bridge.js`, "Page-side
// engines") sends an LLM, image or embedding request only to a page that has
// answered its `engine-probe` for that family with `engine-present`. So the
// probe answer is each engine script's half of the contract: a script that
// does not answer is a page the worker never uses, and one that answers for
// another family's probe is a page the worker sends requests nothing there
// listens for — the request hang this protocol exists to end.
//
// The three shipped scripts are loaded as they are, under a stand-in
// `navigator.serviceWorker` that records what each posts back to the worker.
import { test } from 'node:test';
import assert from 'node:assert/strict';

const ENGINES = {
    llm: '../../assets/webllm-engine.js',
    image: '../../assets/t2i-engine.js',
    embed: '../../assets/embed-engine.js',
};

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

for (const [family, path] of Object.entries(ENGINES)) {
    test(`${path.split('/').pop()} answers the ${family} probe and no other`, async () => {
        const sw = serviceWorkerContainer();
        Object.defineProperty(globalThis, 'navigator', {
            value: { serviceWorker: sw.container },
            configurable: true,
        });
        // A query string makes each test its own module instance.
        await import(`${path}?family=${family}`);

        for (const other of Object.keys(ENGINES).filter((f) => f !== family)) {
            await sw.deliver({ type: 'engine-probe', id: `probe-${other}`, family: other });
        }
        assert.deepEqual(sw.posted, [], 'answered a probe for another engine');

        await sw.deliver({ type: 'engine-probe', id: 'probe-own', family });
        assert.deepEqual(sw.posted, [{ type: 'engine-present', id: 'probe-own' }]);
    });
}
