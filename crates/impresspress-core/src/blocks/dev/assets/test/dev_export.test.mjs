// Run with: node --test crates/impresspress-core/src/blocks/dev/assets/test/dev_export.test.mjs
//
// `exportSite` — the download half of `dev_export`, which is the half no Rust
// test can reach.
//
// `blocks/dev/export.rs` owns everything about what the archive CONTAINS
// (`tests/dev_export.rs` reads a real zip back with the `zip` crate). What is
// left is what the page does with the response, and it is all browser
// behaviour: two requests in the right order, an object URL, an anchor with
// the right `download` name appended to the document and clicked, and the URL
// revoked afterwards rather than immediately. `dev-workspace.spec.ts` drives
// the same path end to end in a real browser and asserts on the downloaded
// file; this covers the parts that have nothing to do with a real file — the
// ordering, the filename, the deferred revoke, the button's one condition and
// the failure paths.
//
// See `harness.mjs` for how the tail is loaded without adding a test hook to
// the shipped file.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { instantiate } from './harness.mjs';

/** One macrotask, which is long enough for the tail's load-time work. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** An `ExportManifest` (`contracts.rs`), trimmed to what the page reads. */
const MANIFEST = {
  generation_id: 'a1b2c3d4e5f6a7b8',
  files: [
    { path: 'README.md', bytes: 2048 },
    { path: 'sw.js', bytes: 9000 },
    { path: 'seed/manifest.json', bytes: 512 }
  ],
  total_bytes: 11560,
  shell_files: 1,
  site_files: 1,
  blocks: 0,
  tables: { impresspress__products__products: 2 }
};

/** A stand-in for the archive body — the page never looks inside it. */
const ZIP = 'PK' + 'x'.repeat(96);

test('exportSite asks for the manifest, then the archive, and downloads it', async () => {
  const { handle, fetchCalls, downloads } = instantiate({ exportManifest: MANIFEST });
  await settle();

  const manifest = await handle.exportSite();

  // The manifest is what the tool returns as `structuredContent`: an agent
  // that exported learns what it downloaded without a second call.
  assert.deepEqual(manifest, MANIFEST);

  // Manifest first, archive second. The order is load-bearing: the download's
  // filename comes out of the manifest, so a page that fetched the archive
  // first would hold 15 MB while it worked out what to call it.
  const exportCalls = fetchCalls
    .map(([url]) => String(url))
    .filter((url) => url.startsWith('/b/dev/api/export'));
  assert.deepEqual(exportCalls, ['/b/dev/api/export/manifest', '/b/dev/api/export']);

  // One download, named after the first eight characters of the generation —
  // the same eight the `Content-Disposition` header uses (`export.rs`).
  assert.equal(downloads.length, 1);
  assert.equal(downloads[0].name, 'impresspress-site-a1b2c3d4.zip');
  // ...from an object URL over the blob, not from the endpoint URL: the
  // request that produced the bytes carried the session cookie, and a plain
  // link to `/b/dev/api/export` would be a second request the browser makes
  // on its own terms.
  assert.match(downloads[0].href, /^blob:/);
});

test('the object URL outlives the click rather than being revoked in the same turn', async () => {
  const { handle, downloads, revoked } = instantiate({
    exportManifest: MANIFEST,
    exportZip: { status: 200, body: ZIP }
  });
  await settle();

  await handle.exportSite();

  // Revoking in the same turn is the classic bug: `click()` starts the
  // download asynchronously and a URL revoked immediately can be gone before
  // the browser has read a byte of it.
  assert.deepEqual(revoked, []);
  await settle();
  assert.deepEqual(revoked, [], 'the revoke is deferred, not merely queued');
  // And the URL the anchor was given is the one over the blob the archive
  // response produced — which is what the deferred revoke will release.
  assert.equal(downloads[0].href, 'blob:fake/' + ZIP.length);
});

test('an instance with nothing published reports the refusal, not a broken download', async () => {
  // `exportManifest: null` is the 400 the endpoint answers before anything
  // has been published — the one refusal `exportSite` meets before it has
  // asked for a byte of archive.
  const { handle, downloads } = instantiate({ exportManifest: null });
  await settle();

  await assert.rejects(() => handle.exportSite(), /HTTP 400/);
  assert.deepEqual(downloads, [], 'a refused export must not start a download');
});

test('a failed archive request names the status and the server own message', async () => {
  const { handle, downloads } = instantiate({
    exportManifest: MANIFEST,
    exportZip: { status: 500, body: 'the static shell could not be read' }
  });
  await settle();

  await assert.rejects(
    () => handle.exportSite(),
    (error) => {
      assert.match(error.message, /export failed: HTTP 500/);
      // The server's own message, not a bare status: "the static shell could
      // not be read" is the difference between a bug report and a shrug.
      assert.match(error.message, /the static shell could not be read/);
      return true;
    }
  );
  assert.deepEqual(downloads, []);
});

test('dev_export is a page-local tool whose result carries the manifest both ways', async () => {
  const { tools, downloads } = instantiate({
    hasModelContext: true,
    exportManifest: MANIFEST
  });
  await settle();

  const devExport = tools.get('dev_export');
  assert.ok(devExport, 'dev.js registers dev_export itself — it is not in tools.json');
  assert.equal(devExport.inputSchema.type, 'object');

  const result = await devExport.execute({});
  assert.ok(!result.isError, JSON.stringify(result));
  // Both halves, because an agent may read either: the text block is what a
  // client without `outputSchema` support shows, `structuredContent` what one
  // with it reads.
  assert.deepEqual(result.structuredContent, MANIFEST);
  assert.deepEqual(JSON.parse(result.content[0].text), MANIFEST);
  assert.equal(downloads.length, 1);
});

test('a failed export reaches the agent as isError, never as a silent success', async () => {
  const { tools } = instantiate({ hasModelContext: true, exportManifest: null });
  await settle();

  const result = await tools.get('dev_export').execute({});
  assert.equal(result.isError, true);
  assert.match(result.content[0].text, /^dev_export: /);
});

// ---- a refusal that names a file -------------------------------------------
//
// The export refuses a site whose files the exported worker would shadow, and
// the answer names the file, the URL and the rule (`export.rs`'s
// `ShadowedSiteFile`). That sentence is the whole of what the person can act
// on, so it has to arrive whole on every surface the page offers — and none of
// them is the handler: the button, the `dev_export` tool, and the
// `dev_export_manifest` tool run from WebMCP or from the Tool console.
//
// The body is not written here. It is the file
// `tests/dev_export.rs` compares the two routes' real answer with
// (`the_export_routes_answer_a_shadowed_site_file_with_a_400_that_names_it`),
// so these tests are fed what the handler says, not a paraphrase of it.
const SHADOWED = JSON.parse(
  fs.readFileSync(new URL('./fixtures/export-shadowed-site-file.json', import.meta.url), 'utf8')
);
const SHADOWED_REFUSAL = { status: 400, body: SHADOWED };

/** `dev_export_manifest` as `/b/dev/api/tools.json` publishes it. */
const MANIFEST_TOOL = {
  name: 'dev_export_manifest',
  description: 'What an export would contain.',
  inputSchema: { type: 'object', properties: {}, additionalProperties: false },
  outputSchema: { type: 'object' },
  invocation: {
    method: 'get',
    path: '/b/dev/api/export/manifest',
    path_params: [],
    query_params: [],
    body_params: []
  }
};

/**
 * The handler's answer, out of the text a tool result carries it in: both
 * tools put the response body after a prefix that says which request failed.
 */
function refusalIn(text, prefix) {
  assert.ok(text.startsWith(prefix), text);
  return JSON.parse(text.slice(prefix.length));
}

test('the fixture is the refusal these tests are about', () => {
  // A guard on the file, so a fixture regenerated from some other refusal
  // cannot leave the tests below passing on a message that names nothing.
  assert.match(SHADOWED.message, /"site\/manifest\.json"/);
  assert.match(SHADOWED.message, /"\/manifest\.json"/);
  assert.match(SHADOWED.message, /Delete the file or move it/);
});

test('dev_export passes on a refusal that names a file, whole, and downloads nothing', async () => {
  const { tools, downloads, fetchCalls, elements } = instantiate({
    hasModelContext: true,
    exportManifest: MANIFEST,
    exportRefusal: SHADOWED_REFUSAL
  });
  await settle();
  fetchCalls.length = 0;

  const result = await tools.get('dev_export').execute({});

  assert.equal(result.isError, true);
  assert.equal(result.structuredContent, undefined);
  assert.deepEqual(refusalIn(result.content[0].text, 'dev_export: HTTP 400: '), SHADOWED);
  // Refused at the manifest: the archive was never asked for, and nothing
  // was handed to the browser as a download.
  assert.deepEqual(
    fetchCalls.map(([url]) => String(url)).filter((url) => url.startsWith('/b/dev/api/export')),
    ['/b/dev/api/export/manifest']
  );
  assert.deepEqual(downloads, []);
  // The person watching the page is told the same thing the agent is.
  assert.ok(
    elements.get('dev-log').textContent.includes(JSON.stringify(SHADOWED.message).slice(1, -1)),
    elements.get('dev-log').textContent
  );
});

test('dev_export_manifest passes the same refusal on through WebMCP', async () => {
  const { tools } = instantiate({
    hasModelContext: true,
    toolsManifest: { tools: [MANIFEST_TOOL] },
    exportRefusal: SHADOWED_REFUSAL
  });
  await settle();

  const result = await tools.get('dev_export_manifest').execute({});

  assert.equal(result.isError, true);
  assert.deepEqual(refusalIn(result.content[0].text, 'Request failed (400): '), SHADOWED);
});

test('the Tool console shows that refusal for both export tools', async () => {
  // No WebMCP: the console is the only way this agent calls a tool.
  const { handle, elements } = instantiate({
    toolsManifest: { tools: [MANIFEST_TOOL] },
    exportRefusal: SHADOWED_REFUSAL
  });
  await settle();

  for (const [tool, prefix] of [
    ['dev_export_manifest', 'Request failed (400): '],
    ['dev_export', 'dev_export: HTTP 400: ']
  ]) {
    elements.get('dev-console-tool').value = tool;
    handle.showConsoleTool();
    await handle.runConsoleTool();

    const box = elements.get('dev-console-result');
    assert.equal(box.getAttribute('data-is-error'), 'true', tool);
    const report = JSON.parse(box.textContent);
    assert.equal(report.isError, true, tool);
    // What an agent reading the box gets: the handler's own answer, with the
    // file's name in it.
    assert.deepEqual(refusalIn(report.result, prefix), SHADOWED, tool);
    assert.equal(elements.get('dev-console-run').disabled, false, 'Run comes back on');
  }
});

test('the Export button’s own export rejects with that refusal and leaves no export in flight', async () => {
  const { handle, downloads } = instantiate({ exportRefusal: SHADOWED_REFUSAL });
  await settle();

  await assert.rejects(
    () => handle.exportSite(),
    (error) => {
      assert.deepEqual(refusalIn(error.message, 'HTTP 400: '), SHADOWED);
      return true;
    }
  );
  assert.deepEqual(downloads, []);
  assert.equal(handle.exportInFlight, null);
});

test('a second export joins the one in flight rather than starting another', async () => {
  // The archive request parks on this until the test releases it, which is
  // the only way to observe the page mid-export — nothing else in the harness
  // yields for long enough.
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const { handle, elements, fetchCalls, downloads } = instantiate({
    exportManifest: MANIFEST,
    exportGate: gate,
    status: {
      runtime_generation: 0,
      blocks: [],
      active_generation: { id: 'gen_1', cause: 'site_write', status: 'active' }
    }
  });
  await settle();

  const button = elements.get('dev-export');
  // Enabled to begin with: something is published.
  assert.equal(button.disabled, false);

  const first = handle.exportSite();
  await settle();

  // An export reads the whole shell twice and builds a multi-megabyte archive
  // in memory; a second one on top of it would do all that again for a
  // download the browser is about to replace anyway.
  assert.ok(handle.exportInFlight, 'an export is in flight');
  assert.equal(button.disabled, true);
  assert.match(button.title, /in progress/i);

  // The BUTTON is disabled, but the button is not the only caller: a
  // `dev_export` tool call racing a click reaches `exportSite` directly. It
  // gets the running export's own promise back — the same object — and starts
  // nothing.
  const second = handle.exportSite();
  assert.equal(second, first, 'the second call is the first call');
  await settle();

  // A status poll landing mid-export must NOT re-enable the button — one
  // owner decides, and it knows about both facts.
  handle.updateExportButton();
  assert.equal(button.disabled, true);

  release();
  assert.deepEqual(await second, await first, 'and resolves to the same manifest');

  assert.equal(handle.exportInFlight, null);
  assert.equal(button.disabled, false);
  assert.equal(button.title, '');
  // One download and one pair of requests for the two calls, not two of each.
  assert.equal(downloads.length, 1);
  const exportCalls = fetchCalls
    .map(([url]) => String(url))
    .filter((url) => url.startsWith('/b/dev/api/export'));
  assert.deepEqual(exportCalls, ['/b/dev/api/export/manifest', '/b/dev/api/export']);
});

test('a failed export gives the button back', async () => {
  // The guard must be released on EVERY exit. A refusal that left the button
  // disabled would take the feature away for the life of the page — a worse
  // outcome than the refusal itself.
  const { handle, elements } = instantiate({
    exportManifest: MANIFEST,
    exportZip: { status: 500, body: 'the static shell could not be read' },
    status: {
      runtime_generation: 0,
      blocks: [],
      active_generation: { id: 'gen_1', cause: 'site_write', status: 'active' }
    }
  });
  await settle();

  await assert.rejects(() => handle.exportSite());

  assert.equal(handle.exportInFlight, null);
  const button = elements.get('dev-export');
  assert.equal(button.disabled, false);
  assert.equal(button.title, '');
});

test('the Export button is disabled until a generation is live', async () => {
  // A fresh instance: the status carries no `active_generation`, and
  // `renderStatus` is what decides — the endpoint would answer 400.
  const { elements } = instantiate({ status: { runtime_generation: 0, blocks: [] } });
  await settle();

  const button = elements.get('dev-export');
  assert.equal(button.disabled, true);
  assert.match(button.title, /Nothing published yet/);
});

test('the Export button is enabled once something has been published', async () => {
  const { elements } = instantiate({
    status: {
      runtime_generation: 0,
      blocks: [],
      active_generation: { id: 'gen_1', cause: 'site_write', status: 'active' }
    }
  });
  await settle();

  const button = elements.get('dev-export');
  assert.equal(button.disabled, false);
  assert.equal(button.title, '');
});
