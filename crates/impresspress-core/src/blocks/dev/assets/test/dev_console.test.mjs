// Run with: node --test crates/impresspress-core/src/blocks/dev/assets/test/dev_console.test.mjs
//
// The Tool console is how an agent whose browser has no WebMCP calls the
// page's tools: it drives a select, a textarea and a button. What these tests
// pin is that the console is not a second implementation of anything — it
// lists the tools the page publishes and runs the `execute` WebMCP would have
// been handed, so the request, the session check and the mutating-tool
// catch-up are the same ones.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { instantiate } from './harness.mjs';

// Two entries in the shape `/b/dev/api/tools.json` publishes them
// (`wafer_core::discovery::generate_webmcp_selected`): a read with no
// arguments and a write whose arguments travel in the body.
const MANIFEST = {
  tools: [
    {
      name: 'dev_status',
      description: 'Read the sandbox state.',
      inputSchema: { type: 'object', properties: {}, additionalProperties: false },
      outputSchema: { type: 'object' },
      invocation: {
        method: 'get',
        path: '/b/dev/api/status',
        path_params: [],
        query_params: [],
        body_params: []
      }
    },
    {
      name: 'dev_write_file',
      description: 'Write a workspace file.',
      inputSchema: {
        type: 'object',
        properties: {
          path: { type: 'string' },
          content: { type: 'string' },
          encoding: { type: 'string', enum: ['utf8', 'base64'] },
          expected_sha256: { type: ['string', 'null'] }
        },
        required: ['path', 'content']
      },
      outputSchema: { type: 'object' },
      invocation: {
        method: 'post',
        path: '/b/dev/api/files/write',
        path_params: [],
        query_params: [],
        body_params: ['path', 'content', 'encoding', 'expected_sha256']
      }
    }
  ]
};

/** Let the page's first fetches — and the `.then` chains on them — settle. */
const settle = async () => {
  for (let i = 0; i < 20; i += 1) {
    await new Promise((resolve) => setImmediate(resolve));
  }
};

const optionNames = (elements) =>
  elements.get('dev-console-tool').children.map((option) => option.value);

test('without WebMCP the console still lists every tool, and the guide says to use it', async () => {
  const { handle, elements, tools } = instantiate({ toolsManifest: MANIFEST });
  await settle();

  assert.equal(handle.hasWebmcp, false);
  // The manifest's tools, then the two the page defines itself.
  assert.deepEqual(optionNames(elements), [
    'dev_status',
    'dev_write_file',
    'dev_compile_block',
    'dev_export'
  ]);
  assert.equal(tools.size, 0, 'there is no registrar to hand anything to');
  assert.deepEqual(handle.registered, []);
  assert.equal(
    elements.get('dev-webmcp-status').textContent,
    'This browser has no WebMCP: use the Tool console below, or the file editor.'
  );
  // The first tool is selected and described, and Run is on.
  assert.equal(elements.get('dev-console-tool').value, 'dev_status');
  assert.equal(elements.get('dev-console-description').textContent, 'Read the sandbox state.');
  assert.equal(elements.get('dev-console-args').value, '{}');
  assert.equal(elements.get('dev-console-run').disabled, false);
});

test('with WebMCP the console lists exactly the objects the registrar was handed', async () => {
  const { handle, elements, tools } = instantiate({
    hasModelContext: true,
    toolsManifest: MANIFEST
  });
  await settle();

  assert.equal(handle.hasWebmcp, true);
  assert.deepEqual(optionNames(elements), [...tools.keys()]);
  // The same OBJECTS, not copies with the same names: that identity is what
  // makes a console run and an agent's call one code path.
  for (const tool of handle.pageTools) {
    assert.equal(tools.get(tool.name), tool, tool.name);
  }
  // `webmcp.js` has not run on this page (no `__impresspressWebmcp`), so
  // the page states its own count and no total.
  assert.equal(
    elements.get('dev-webmcp-status').textContent,
    'This browser has WebMCP: the 4 workspace tools are registered for an agent in this tab, ' +
      "and the Tool console below runs the same tools. The site's own tools are still being " +
      'registered.'
  );
  // The log line the workspace e2e reads is still written, with the
  // manifest's own count.
  assert.match(elements.get('dev-log').textContent, /registered 2 workspace tools/);
});

// The tab's agent has two registrars' tools: this page's and the site's own
// (`webmcp.js`, on every page). The sentence counts both, whichever script
// finished first, and follows the site's set when it is refreshed.
const site = (count, generation = 1) => ({
  count: () => count,
  generation: () => generation
});
const BOTH = (total, workspace, own) =>
  `This browser has WebMCP: ${total} tools are registered for an agent in this tab: the ` +
  `${workspace} workspace tools, which the Tool console below also runs, and the site's ` +
  `own ${own}.`;

test('with the site’s tools already registered, the page states both counts and the total', async () => {
  const { elements } = instantiate({
    hasModelContext: true,
    toolsManifest: MANIFEST,
    siteRegistrar: site(10)
  });
  await settle();
  assert.equal(elements.get('dev-webmcp-status').textContent, BOTH(14, 4, 10));
});

test('the site’s tools arriving later, or changing, update the sentence', async () => {
  const registrar = { count: () => 0, generation: () => 0 };
  const { elements, fireWindow } = instantiate({
    hasModelContext: true,
    toolsManifest: MANIFEST,
    siteRegistrar: registrar
  });
  await settle();
  // Present but not yet loaded: no total is claimed.
  assert.match(elements.get('dev-webmcp-status').textContent, /still being registered\.$/);

  registrar.count = () => 10;
  registrar.generation = () => 1;
  fireWindow('impresspress:webmcp-loaded', {});
  assert.equal(elements.get('dev-webmcp-status').textContent, BOTH(14, 4, 10));

  // A refresh after a runtime rebuild (a compiled block's tools).
  registrar.count = () => 12;
  registrar.generation = () => 2;
  fireWindow('impresspress:webmcp-loaded', {});
  assert.equal(elements.get('dev-webmcp-status').textContent, BOTH(16, 4, 12));

  // A load that found none.
  registrar.count = () => 0;
  registrar.generation = () => 3;
  fireWindow('impresspress:webmcp-loaded', {});
  assert.equal(
    elements.get('dev-webmcp-status').textContent,
    'This browser has WebMCP: the 4 workspace tools are registered for an agent in this tab, ' +
      'and the Tool console below runs the same tools. The site has registered none of its own.'
  );
});

test('a site load before this page’s tools arrive, or after they are gone, writes nothing', async () => {
  const registrar = site(10);
  const { handle, elements, fireWindow } = instantiate({
    hasModelContext: true,
    toolsManifest: MANIFEST,
    siteRegistrar: registrar
  });
  // Before `tools.json` has answered: the sentence is the page's to write
  // once it knows its own tools.
  fireWindow('impresspress:webmcp-loaded', {});
  assert.equal(elements.get('dev-webmcp-status').textContent, '');
  await settle();

  handle.abort.abort();
  fireWindow('impresspress:webmcp-loaded', {});
  assert.equal(
    elements.get('dev-webmcp-status').textContent,
    'The session expired and the workspace tools were removed. Sign in again.'
  );
});

test('without WebMCP the site’s loads change nothing', async () => {
  const { elements, fireWindow } = instantiate({ toolsManifest: MANIFEST, siteRegistrar: site(10) });
  await settle();
  fireWindow('impresspress:webmcp-loaded', {});
  assert.equal(
    elements.get('dev-webmcp-status').textContent,
    'This browser has no WebMCP: use the Tool console below, or the file editor.'
  );
});

test('selecting a tool pre-fills its required arguments from the input schema', async () => {
  const { handle, elements } = instantiate({ toolsManifest: MANIFEST });
  await settle();

  elements.get('dev-console-tool').value = 'dev_write_file';
  handle.showConsoleTool();

  // Required properties only, each with a placeholder of its declared type;
  // `encoding` and `expected_sha256` are optional and stay out.
  assert.deepEqual(JSON.parse(elements.get('dev-console-args').value), { path: '', content: '' });
  assert.equal(elements.get('dev-console-description').textContent, 'Write a workspace file.');
  assert.deepEqual(
    JSON.parse(elements.get('dev-console-schema').textContent),
    MANIFEST.tools[1].inputSchema
  );
});

test('exampleArguments gives each required property a value of its type', () => {
  const { handle } = instantiate();
  const schema = {
    type: 'object',
    properties: {
      name: { type: 'string' },
      count: { type: 'integer' },
      ratio: { type: 'number' },
      on: { type: 'boolean' },
      tags: { type: 'array', items: { type: 'string' } },
      kind: { enum: ['hello', 'table'] },
      maybe: { type: ['string', 'null'] },
      nested: { type: 'object', properties: { id: { type: 'string' } }, required: ['id'] },
      shared: { $ref: '#/$defs/Mode' },
      preset: { type: 'string', default: 'x' },
      either: { anyOf: [{ type: 'integer' }, { type: 'null' }] },
      skipped: { type: 'string' }
    },
    required: [
      'name',
      'count',
      'ratio',
      'on',
      'tags',
      'kind',
      'maybe',
      'nested',
      'shared',
      'preset',
      'either'
    ],
    $defs: { Mode: { type: 'string', enum: ['draft', 'live'] } }
  };
  assert.deepEqual(handle.exampleArguments(schema), {
    name: '',
    count: 0,
    ratio: 0,
    on: false,
    tags: [],
    kind: 'hello',
    maybe: '',
    nested: { id: '' },
    shared: 'draft',
    preset: 'x',
    either: 0
  });
  // No schema, or one that requires nothing, is a complete call.
  assert.deepEqual(handle.exampleArguments(undefined), {});
  assert.deepEqual(handle.exampleArguments({ type: 'object', properties: {} }), {});
});

test('Run invokes the tool through the shared request path and shows its result', async () => {
  const { handle, elements, fetchCalls } = instantiate({
    toolsManifest: MANIFEST,
    status: { runtime_generation: 3, template: 'blank' }
  });
  await settle();
  fetchCalls.length = 0;

  elements.get('dev-console-tool').value = 'dev_status';
  handle.showConsoleTool();
  await handle.runConsoleTool();

  // The request `toolOptions` builds from the manifest's `invocation`
  // (webmcp-core.js) — the console has no request of its own to make.
  assert.deepEqual(fetchCalls[0], [
    '/b/dev/api/status',
    { method: 'GET', headers: {}, credentials: 'same-origin' }
  ]);
  const result = elements.get('dev-console-result');
  assert.deepEqual(JSON.parse(result.textContent), {
    isError: false,
    result: { runtime_generation: 3, template: 'blank' }
  });
  assert.equal(result.getAttribute('data-is-error'), 'false');
  assert.equal(elements.get('dev-console-run').disabled, false, 'Run comes back on');
  // A read is not a mutating tool: no poll was started and the file tree was
  // not re-fetched.
  assert.equal(fetchCalls.length, 1);
  assert.equal(handle.isPolling, false);
});

test('a mutating tool run from the console refreshes the page as an agent call does', async () => {
  const written = [];
  const { handle, elements, fetchCalls } = instantiate({
    toolsManifest: MANIFEST,
    endpoint: (url, init) => {
      if (url === '/b/dev/api/files/write') {
        written.push(JSON.parse(init.body));
        // A `blocks/` write: staged, no generation published.
        return { body: { path: 'blocks/hello/src/lib.rs', sha256: 'abc', generation: null } };
      }
      return undefined;
    }
  });
  await settle();
  fetchCalls.length = 0;

  // The preview frame the catch-up reloads when no generation was published
  // (so no service-worker push is coming for it).
  let reloads = 0;
  elements.set('dev-preview-frame', {
    contentWindow: { location: { reload: () => (reloads += 1) } }
  });

  elements.get('dev-console-tool').value = 'dev_write_file';
  handle.showConsoleTool();
  elements.get('dev-console-args').value = JSON.stringify({
    path: 'blocks/hello/src/lib.rs',
    content: 'fn main() {}'
  });
  const running = handle.runConsoleTool();
  // `withProgress` is live for the duration: the count is up and the status
  // poll is running, exactly as for a WebMCP call.
  assert.equal(handle.outstanding, 1);
  assert.equal(handle.isPolling, true);
  assert.equal(elements.get('dev-console-run').disabled, true, 'one console run at a time');
  await running;

  assert.deepEqual(written, [{ path: 'blocks/hello/src/lib.rs', content: 'fn main() {}' }]);
  assert.equal(handle.outstanding, 0);
  assert.equal(handle.isPolling, false);
  // The catch-up: the file tree is re-read and the preview reloaded.
  const urls = fetchCalls.map((call) => String(call[0]));
  assert.equal(urls[0], '/b/dev/api/files/write');
  assert.ok(urls.includes('/b/dev/api/files'), `the file tree was not refreshed: ${urls}`);
  assert.equal(reloads, 1);
  assert.deepEqual(JSON.parse(elements.get('dev-console-result').textContent), {
    isError: false,
    result: { path: 'blocks/hello/src/lib.rs', sha256: 'abc', generation: null }
  });
});

test('a refusal is shown as an error result, with the server’s own body', async () => {
  const { handle, elements } = instantiate({
    toolsManifest: MANIFEST,
    endpoint: (url) =>
      url === '/b/dev/api/files/write'
        ? { status: 400, body: { error: 'invalid_argument', message: 'path escapes the workspace' } }
        : undefined
  });
  await settle();

  elements.get('dev-console-tool').value = 'dev_write_file';
  handle.showConsoleTool();
  elements.get('dev-console-args').value = '{"path":"../x","content":""}';
  await handle.runConsoleTool();

  const result = elements.get('dev-console-result');
  const report = JSON.parse(result.textContent);
  assert.equal(report.isError, true);
  assert.match(report.result, /^Request failed \(400\): .*path escapes the workspace/);
  assert.equal(result.getAttribute('data-is-error'), 'true');
});

test('arguments that are not a JSON object are refused without calling the tool', async () => {
  const { handle, elements, fetchCalls } = instantiate({ toolsManifest: MANIFEST });
  await settle();
  fetchCalls.length = 0;

  for (const [text, expected] of [
    ['{not json', /not valid JSON/],
    ['[1, 2]', /must be a JSON object/],
    ['null', /must be a JSON object/]
  ]) {
    elements.get('dev-console-args').value = text;
    await handle.runConsoleTool();
    const report = JSON.parse(elements.get('dev-console-result').textContent);
    assert.equal(report.isError, true, text);
    assert.match(report.result, expected, text);
  }
  assert.equal(fetchCalls.length, 0);

  // An empty box is the empty object — a complete call for a tool that
  // requires nothing.
  elements.get('dev-console-args').value = '   ';
  await handle.runConsoleTool();
  assert.equal(JSON.parse(elements.get('dev-console-result').textContent).isError, false);
  // The run's outcome is announced in one short sentence, not as the JSON.
  assert.equal(elements.get('dev-console-status').textContent, 'Run finished: ok');
});

test('a tool call that finds the session gone empties the console', async () => {
  const { handle, elements } = instantiate({
    toolsManifest: MANIFEST,
    endpoint: (url) =>
      url === '/b/dev/api/files/write'
        ? { status: 401, body: { error: 'unauthenticated', message: 'sign in' } }
        : undefined
  });
  await settle();

  elements.get('dev-console-tool').value = 'dev_write_file';
  handle.showConsoleTool();
  await handle.runConsoleTool();

  // `withSessionCheck` read the 401 off the result and aborted the page —
  // the same rule an agent's call is under.
  assert.equal(handle.abort.signal.aborted, true);
  assert.deepEqual(handle.pageTools, []);
  assert.deepEqual(optionNames(elements), []);
  assert.equal(elements.get('dev-console-run').disabled, true);
  // The refusal itself is still what the result box shows.
  assert.equal(JSON.parse(elements.get('dev-console-result').textContent).isError, true);
  assert.equal(elements.get('dev-console-status').textContent, 'Run finished: error');
});

test('a schema that refers to itself cannot stop the console from rendering', async () => {
  // Directly, and through a required property of an object: both are cycles
  // an unbounded walk would never leave.
  const cyclic = {
    type: 'object',
    properties: { node: { $ref: '#/$defs/Node' }, self: { $ref: '#/properties/self' } },
    required: ['node', 'self'],
    $defs: {
      Node: {
        type: 'object',
        properties: { name: { type: 'string' }, next: { $ref: '#/$defs/Node' } },
        required: ['name', 'next']
      }
    }
  };
  const { handle, elements } = instantiate({
    toolsManifest: {
      tools: [{ ...MANIFEST.tools[0], name: 'dev_cyclic', inputSchema: cyclic }, MANIFEST.tools[1]]
    }
  });
  assert.doesNotThrow(() => handle.exampleArguments(cyclic));
  await settle();

  // The walk ended, in a value of the schema's shape as far as it went.
  const example = JSON.parse(elements.get('dev-console-args').value);
  assert.equal(example.self, null);
  assert.equal(example.node.name, '');
  assert.equal(example.node.next.name, '');
  // …and everything after `showConsoleTool` in the load still happened: the
  // other tools are listed and the guide's status line was written.
  assert.deepEqual(optionNames(elements).slice(0, 2), ['dev_cyclic', 'dev_write_file']);
  assert.match(elements.get('dev-webmcp-status').textContent, /^This browser has no WebMCP/);
  assert.equal(elements.get('dev-console-run').disabled, false);
});

test('when the tool manifest cannot be loaded the page says so and Run stays off', async () => {
  const { handle, elements } = instantiate({ toolsFailure: 500 });
  // What the markup ships (`page.rs`): Run is `disabled` until tools arrive.
  elements.get('dev-console-run').disabled = true;
  await settle();

  assert.deepEqual(handle.pageTools, []);
  assert.deepEqual(optionNames(elements), []);
  assert.match(
    elements.get('dev-webmcp-status').textContent,
    /^The tools could not be loaded, so the Tool console is empty\./
  );
  assert.equal(elements.get('dev-console-run').disabled, true);
  assert.match(elements.get('dev-log').textContent, /error: HTTP 500/);
  // Not a lost session: the page is still alive for the editor.
  assert.equal(handle.abort.signal.aborted, false);
});

test('a manifest refused for a lost session reports the session, not a load failure', async () => {
  const { handle, elements } = instantiate({ toolsFailure: 401 });
  await settle();
  assert.equal(handle.abort.signal.aborted, true);
  assert.match(elements.get('dev-webmcp-status').textContent, /^The session expired/);
});
