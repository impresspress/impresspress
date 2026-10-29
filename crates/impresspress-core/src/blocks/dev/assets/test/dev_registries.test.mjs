// Run with: node --test crates/impresspress-core/src/blocks/dev/assets/test/dev_registries.test.mjs
//
// Pins WHERE `/b/dev` hands its workspace tools: to every WebMCP registry the
// browser provides — Chrome's `navigator.modelContext`, the OpenAI Codex /
// ChatGPT in-app browser's `document.modelContext` — and that "this browser
// has WebMCP", every count the page states, and the teardown all follow the
// same registries. A page that asked only `navigator` told a Codex agent
// "This browser has no WebMCP" and handed it nothing.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { instantiate } from './harness.mjs';
import { chromeRegistry, codexRegistry } from '../../../../ui/assets/test/registries.mjs';

const MANIFEST = {
  tools: [
    {
      name: 'dev_status',
      description: 'Read the sandbox state.',
      inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      invocation: { method: 'get', path: '/b/dev/api/status', path_params: [], query_params: [], body_params: [] }
    },
    {
      name: 'dev_list_files',
      description: 'List workspace files.',
      inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      invocation: { method: 'get', path: '/b/dev/api/files', path_params: [], query_params: [], body_params: [] }
    }
  ]
};

// The manifest's tools, then the two the page defines itself.
const WORKSPACE = ['dev_status', 'dev_list_files', 'dev_compile_block', 'dev_export'];

/**
 * Let the page's first fetches — and the `.then` chains on them — settle.
 * Through TIMERS, not only immediates: Codex's registry answers on a
 * `setTimeout`, and a run of `setImmediate`s can finish inside the
 * millisecond before that timer is due.
 */
const settle = async () => {
  for (let i = 0; i < 20; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
};

const held = (registry) => [...registry.tools.keys()];

test('a registry on navigator AND one on document each receive every workspace tool', async () => {
  const onNavigator = chromeRegistry();
  const onDocument = codexRegistry();
  const { handle, elements } = instantiate({
    registries: { navigator: onNavigator, document: onDocument },
    toolsManifest: MANIFEST
  });
  await settle();

  assert.equal(handle.hasWebmcp, true);
  assert.deepEqual(held(onNavigator), WORKSPACE);
  assert.deepEqual(held(onDocument), WORKSPACE);
  // The same objects the console runs, in both registries.
  for (const tool of handle.pageTools) {
    assert.equal(onNavigator.tools.get(tool.name), tool, tool.name);
    assert.equal(onDocument.tools.get(tool.name), tool, tool.name);
  }
  // A tool both registries hold is one tool to the agent, not two.
  assert.deepEqual(handle.registered, WORKSPACE);
  assert.match(
    elements.get('dev-webmcp-status').textContent,
    /^This browser has WebMCP: the 4 workspace tools are registered for an agent in this tab, /
  );
  assert.match(elements.get('dev-log').textContent, /registered 4 workspace tools(?!;)/);
});

test('a browser that offers only document.modelContext (Codex) has WebMCP and gets every tool', async () => {
  const onDocument = codexRegistry();
  const { handle, elements } = instantiate({
    registries: { document: onDocument },
    toolsManifest: MANIFEST
  });
  await settle();

  assert.equal(handle.hasWebmcp, true);
  assert.deepEqual(held(onDocument), WORKSPACE);
  assert.match(elements.get('dev-webmcp-status').textContent, /^This browser has WebMCP: the 4 workspace tools/);
});

test('the same registry in both places is handed each tool once', async () => {
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
  const { handle, elements } = instantiate({
    registries: { navigator: counting, document: counting },
    toolsManifest: MANIFEST
  });
  await settle();

  assert.equal(calls, WORKSPACE.length);
  assert.deepEqual(handle.registered, WORKSPACE);
  // A second hand-off would have been refused as a duplicate and logged.
  assert.doesNotMatch(elements.get('dev-log').textContent, /refused|Duplicate/);
});

test('no registry anywhere: no WebMCP, and the console still runs every tool', async () => {
  const { handle, elements } = instantiate({ registries: {}, toolsManifest: MANIFEST });
  await settle();

  assert.equal(handle.hasWebmcp, false);
  assert.deepEqual(handle.registered, []);
  assert.deepEqual(
    handle.pageTools.map((tool) => tool.name),
    WORKSPACE
  );
  assert.equal(
    elements.get('dev-webmcp-status').textContent,
    'This browser has no WebMCP: use the Tool console below, or the file editor.'
  );
});

test('a lost session takes every tool back from both registries, each the way it lets go', async () => {
  // Chrome lets go through `unregisterTool`; Codex's registry has none and
  // lets go when the registration's signal is aborted.
  const onNavigator = chromeRegistry();
  const onDocument = codexRegistry();
  const { handle } = instantiate({
    registries: { navigator: onNavigator, document: onDocument },
    toolsManifest: MANIFEST
  });
  await settle();

  handle.abort.abort();
  assert.deepEqual(held(onNavigator), []);
  assert.deepEqual(held(onDocument), []);
  assert.deepEqual(handle.registered, []);
});

test('a session lost while Codex is still answering leaves nothing behind and announces nothing', async () => {
  // Codex's `registerTool` settles on a later task. Teardown landing before
  // it does must still remove the tools, and the page must not then announce
  // tools it no longer has over the "session expired" sentence.
  const onDocument = codexRegistry();
  let answer;
  const answered = new Promise((resolve) => (answer = resolve));
  // Codex's registry, holding its answers until the test releases them.
  const slow = {
    tools: onDocument.tools,
    registerTool(options, init) {
      const settled = onDocument.registerTool(options, init);
      return answered.then(() => settled);
    }
  };
  const { handle, elements } = instantiate({
    registries: { document: slow },
    toolsManifest: MANIFEST
  });
  await settle();
  assert.deepEqual(held(onDocument), WORKSPACE, 'handed over, not yet answered');
  assert.deepEqual(handle.registered, [], 'nothing is counted before the registry answers');

  handle.abort.abort();
  answer();
  await settle();

  assert.deepEqual(held(onDocument), []);
  assert.deepEqual(handle.registered, []);
  assert.equal(
    elements.get('dev-webmcp-status').textContent,
    'The session expired and the workspace tools were removed. Sign in again.'
  );
});

test('a session lost before the tool list arrives hands no registry anything', async () => {
  // Another request's 401/403 aborts the workspace while `tools.json` is
  // still on its way; the abort handler has already said the tools are gone.
  const onNavigator = chromeRegistry();
  const onDocument = codexRegistry();
  const { handle, elements } = instantiate({
    registries: { navigator: onNavigator, document: onDocument },
    toolsManifest: MANIFEST
  });
  handle.abort.abort();
  await settle();

  assert.deepEqual(held(onNavigator), []);
  assert.deepEqual(held(onDocument), []);
  assert.equal(
    elements.get('dev-webmcp-status').textContent,
    'The session expired and the workspace tools were removed. Sign in again.'
  );
});
