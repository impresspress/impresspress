// Run with: node --test crates/impresspress-core/src/ui/assets/test/webmcp_registries.test.mjs
//
// Pins WHERE `webmcp.js` hands the site's tools: to every WebMCP registry the
// browser provides. Chrome provides `navigator.modelContext`; the OpenAI
// Codex / ChatGPT in-app browser provides `document.modelContext` and nothing
// else. A script that asked only one of them registered zero tools in the
// other browser while its page went on looking fine.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { instantiate, manifestTool, serviceWorkerStub, settle } from './harness.mjs';
import { chromeRegistry, codexRegistry } from './registries.mjs';

const NAMES = ['list_products', 'get_product', 'search_products'];

function site(registries, names = NAMES) {
  let tools = names;
  const instance = instantiate({
    serviceWorker: serviceWorkerStub({ controlled: true }),
    registries,
    respond: () => ({ ok: true, status: 200, json: async () => ({ tools: tools.map(manifestTool) }) })
  });
  return Object.assign(instance, {
    offer(next) {
      tools = next;
    }
  });
}

// Every queued answer in: the manifest fetch, then Codex's registry, which
// answers on a timer of its own.
const drain = async () => {
  for (let i = 0; i < 10; i += 1) {
    await settle();
  }
};

const held = (registry) => [...registry.tools.keys()].sort();

test('a registry on navigator AND one on document each receive every site tool', async () => {
  const onNavigator = chromeRegistry();
  const onDocument = codexRegistry();
  const { published } = site({ navigator: onNavigator, document: onDocument });
  await drain();
  assert.deepEqual(held(onNavigator), [...NAMES].sort());
  assert.deepEqual(held(onDocument), [...NAMES].sort());
  assert.equal(published.count(), NAMES.length, 'a tool both registries hold is one tool');
});

test('a browser that offers only document.modelContext (Codex) gets every site tool', async () => {
  const onDocument = codexRegistry();
  const { published } = site({ document: onDocument });
  await drain();
  assert.deepEqual(held(onDocument), [...NAMES].sort());
  assert.equal(published.count(), NAMES.length);
});

test('the same registry in both places is handed each tool once', async () => {
  // Chrome's shape throws on a duplicate name, so a second hand-off of the
  // same tool would be a refusal — and a page that counted refusals would
  // under-report what is registered.
  const shared = chromeRegistry();
  let calls = 0;
  const counting = {
    tools: shared.tools,
    registerTool(options, init) {
      calls += 1;
      return shared.registerTool(options, init);
    },
    unregisterTool: shared.unregisterTool
  };
  const { published } = site({ navigator: counting, document: counting });
  await drain();
  assert.equal(calls, NAMES.length);
  assert.equal(published.count(), NAMES.length);
});

test('no registry anywhere: nothing is fetched, registered or published', async () => {
  const { fetchCalls, published } = site({});
  await drain();
  assert.equal(fetchCalls.length, 0);
  assert.equal(published, undefined, 'the script stops before publishing its handle');
});

test('a refresh takes every tool back from both registries, each the way it lets go', async () => {
  // Chrome lets go through `unregisterTool`; Codex's registry has none, and
  // lets go when the registration's signal is aborted. A refresh that only
  // unregistered by name would leave Codex holding the old tools, and every
  // re-registration after it would be refused as a duplicate.
  const onNavigator = chromeRegistry();
  const onDocument = codexRegistry();
  const page = site({ navigator: onNavigator, document: onDocument });
  await drain();

  page.offer(['list_products']);
  await page.published.refresh();
  assert.deepEqual(held(onNavigator), ['list_products']);
  assert.deepEqual(held(onDocument), ['list_products']);
  assert.equal(page.published.count(), 1);
});

test('a tool one registry refuses still counts when the other holds it; refused by all, it does not', async () => {
  const onNavigator = chromeRegistry();
  const onDocument = codexRegistry();
  // Another script already holds `get_product` on the document registry, and
  // `search_products` on both.
  await onDocument.registerTool({ name: 'get_product' });
  await onDocument.registerTool({ name: 'search_products' });
  onNavigator.registerTool({ name: 'search_products' });
  const { published } = site({ navigator: onNavigator, document: onDocument });
  await drain();
  assert.equal(published.count(), 2, 'list_products and get_product; search_products was refused everywhere');

  // The refusals are not ours to take back: a refresh leaves the other
  // script's tools where they are.
  await published.refresh();
  assert.ok(onNavigator.tools.has('search_products'));
  assert.ok(onDocument.tools.has('get_product'));
  assert.ok(onDocument.tools.has('search_products'));
});
