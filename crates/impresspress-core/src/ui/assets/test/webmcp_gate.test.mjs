// Run with: node --test crates/impresspress-core/src/ui/assets/test/webmcp_gate.test.mjs
//
// Pins WHEN `webmcp.js` fetches the manifest.
//
// `/b/webmcp/manifest.json` is served by the wasm router inside the service
// worker. A fetch issued before the worker CONTROLS this document goes to the
// network instead — and on a host with SPA fallback the network answers
// `index.html` with 200, so `r.ok` is true, `r.json()` throws, the `.catch`
// swallows it, and the page silently ends up with no tools and no error. The
// gate is therefore `navigator.serviceWorker.controller`, not
// `navigator.serviceWorker.ready`: `ready` resolves on `registration.active`,
// which is populated at the *activating* state, while `sw.js.tmpl` claims
// clients inside `activate`'s `waitUntil`.
//
// The other half is not hanging when nothing will ever claim the page: a
// native server exposes `navigator.serviceWorker` (it is a standard browser
// API) but has no registration at all.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { instantiate, manifestTool, serviceWorkerStub, settle } from './harness.mjs';

test('a page already controlled fetches the manifest immediately', async () => {
  const { fetchCalls, registerCalls } = instantiate({
    serviceWorker: serviceWorkerStub({ controlled: true })
  });
  await settle();
  assert.equal(fetchCalls.length, 1);
  assert.equal(fetchCalls[0][0], '/b/webmcp/manifest.json');
  assert.deepEqual(registerCalls, ['list_products']);
});

test('a browser with no service-worker support at all fetches immediately', async () => {
  const { fetchCalls } = instantiate({ serviceWorker: undefined });
  await settle();
  assert.equal(fetchCalls.length, 1);
});

test('a native server (no registration) fetches immediately instead of waiting forever', async () => {
  // `getRegistration()` resolving `undefined` is what tells a native build
  // apart from a service-worker build. Waiting for a claim that nothing will
  // ever make would leave the page with no tools for the whole session.
  const { fetchCalls } = instantiate({
    serviceWorker: serviceWorkerStub({ controlled: false, hasRegistration: false })
  });
  await settle();
  assert.equal(fetchCalls.length, 1);
});

test('a registered but UNCLAIMED page waits, then fetches when the claim lands', async () => {
  const sw = serviceWorkerStub({ controlled: false, hasRegistration: true });
  const { fetchCalls, registerCalls } = instantiate({ serviceWorker: sw });

  await settle();
  assert.equal(
    fetchCalls.length,
    0,
    'fetching here would miss the worker and, behind SPA fallback, silently register nothing'
  );

  sw.claim();
  await settle();
  assert.equal(fetchCalls.length, 1);
  assert.deepEqual(registerCalls, ['list_products']);
  assert.equal(sw.listenerCount(), 0, 'the controllerchange listener must be removed once it fires');
});

test('whenControlled resolves without a listener when the page is already controlled', async () => {
  const sw = serviceWorkerStub({ controlled: true });
  const { handle } = instantiate({ serviceWorker: sw });
  await handle.whenControlled();
  assert.equal(sw.listenerCount(), 0);
});

test('generation counts every completed load, including one that found nothing', async () => {
  // The doc comment promises a caller can poll `generation()` to notice a
  // refresh landed. A degraded manifest still LANDS — a poller waiting on it
  // must not hang because the page happened to have no tools to offer.
  let ok = true;
  const { handle } = instantiate({
    serviceWorker: serviceWorkerStub({ controlled: true }),
    respond: () =>
      ok
        ? { ok: true, status: 200, json: async () => ({ tools: [manifestTool('list_products')] }) }
        : { ok: false, status: 503, json: async () => null }
  });
  await settle();
  const afterFirst = handle.generation();
  assert.equal(afterFirst, 1);

  ok = false;
  await handle.refresh();
  assert.equal(handle.generation(), afterFirst + 1, 'a refused manifest is still a completed load');

  // And a fetch that rejects outright, not merely a non-ok response.
  const rejecting = instantiate({
    serviceWorker: serviceWorkerStub({ controlled: true }),
    respond: () => {
      throw new Error('offline');
    }
  });
  await settle();
  assert.equal(rejecting.handle.generation(), 1);
});

// The `/b/dev` workspace states how many tools an agent in its tab has, and
// half of them are this script's. `count()` is what it has registered with
// the browser now, and every settled load — a refresh included, a degraded
// one too — says so on `window`, so a page that shows the number can keep it
// true whichever of the two scripts finished first.
test('count() is what is registered now, and every settled load is announced on window', async () => {
  let tools = ['list_products', 'get_product'];
  const { published, windowEvents } = instantiate({
    serviceWorker: serviceWorkerStub({ controlled: true }),
    respond: () =>
      tools === null
        ? { ok: false, status: 503, json: async () => null }
        : { ok: true, status: 200, json: async () => ({ tools: tools.map(manifestTool) }) }
  });
  await settle();
  assert.equal(published.count(), 2);
  assert.deepEqual(windowEvents, ['impresspress:webmcp-loaded']);

  tools = ['list_products'];
  await published.refresh();
  assert.equal(published.count(), 1);
  assert.deepEqual(windowEvents, ['impresspress:webmcp-loaded', 'impresspress:webmcp-loaded']);

  tools = null;
  await published.refresh();
  assert.equal(published.count(), 0, 'a refused manifest leaves nothing registered');
  assert.equal(windowEvents.length, 3, 'and is announced like any other load');
});
