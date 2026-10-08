// Run with: node --test crates/impresspress-core/src/blocks/dev/assets/test/dev_refcount.test.mjs
//
// Exercises `dev.js`'s poll refcount / abort logic for real, rather than
// pinning its source text. See `harness.mjs` for how the tail is loaded
// without adding a test hook to the shipped file.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { instantiate } from './harness.mjs';

test('withProgress tracks one call: increments on start, decrements and stops polling on finish', async () => {
  const { handle } = instantiate();
  let release;
  const gate = new Promise((r) => (release = r));
  const call = handle.withProgress(async () => gate);

  const pending = call();
  await Promise.resolve();
  assert.equal(handle.outstanding, 1);
  assert.equal(handle.isPolling, true);

  release();
  await pending;
  assert.equal(handle.outstanding, 0);
  assert.equal(handle.isPolling, false);
});

test('two overlapping calls: polling stays up until the LAST one finishes', async () => {
  const { handle } = instantiate();
  let releaseA, releaseB;
  const gateA = new Promise((r) => (releaseA = r));
  const gateB = new Promise((r) => (releaseB = r));
  const callA = handle.withProgress(async () => gateA);
  const callB = handle.withProgress(async () => gateB);

  const pendingA = callA();
  const pendingB = callB();
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(handle.outstanding, 2);

  releaseA();
  await pendingA;
  assert.equal(handle.outstanding, 1, 'one call finished, one still in flight');
  assert.equal(handle.isPolling, true, 'polling must not stop until the LAST call finishes');

  releaseB();
  await pendingB;
  assert.equal(handle.outstanding, 0);
  assert.equal(handle.isPolling, false);
});

test('abort mid-flight: the count does not go negative when the stragglers resolve (I1)', async () => {
  const { handle } = instantiate();
  let releaseA, releaseB;
  const gateA = new Promise((r) => (releaseA = r));
  const gateB = new Promise((r) => (releaseB = r));
  const callA = handle.withProgress(async () => gateA);
  const callB = handle.withProgress(async () => gateB);

  const pendingA = callA();
  const pendingB = callB();
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(handle.outstanding, 2);

  // The session dies while both calls are still in flight — neither call
  // was given `abort.signal` (only `registerTool` gets it), so both WILL
  // still reach their own `finally`.
  handle.abort.abort();
  assert.equal(handle.outstanding, 0, 'abort resets the count');
  assert.equal(handle.isPolling, false, 'abort stops the interval');

  releaseA();
  releaseB();
  await pendingA;
  await pendingB;

  // Before the fix, each straggler's `finally` decremented unconditionally
  // and drove this to -2.
  assert.equal(handle.outstanding, 0, 'stragglers must not drive the count negative');
  assert.equal(handle.isPolling, false, 'stragglers must not leave polling running');
});

test('a call started after abort executes but is never tracked, and does not restart polling', async () => {
  const { handle } = instantiate();
  handle.abort.abort();
  assert.equal(handle.outstanding, 0);

  const call = handle.withProgress(async () => 'ok');
  const result = await call();

  assert.equal(result, 'ok', 'the call itself still runs and returns its result');
  assert.equal(handle.outstanding, 0, 'a post-abort call is never counted');
  assert.equal(handle.isPolling, false, 'a post-abort call must not start a new interval');
});

test('unregisterPageTools tolerates a browser with no navigator.modelContext at all', () => {
  // `hasModelContext: false` reproduces "this browser has no WebMCP
  // support" (`'modelContext' in navigator` is false). `pagehide` and every
  // 401/403 call `unregisterPageTools()` unconditionally regardless of
  // whether registration ever ran, so this must not throw.
  const { handle } = instantiate({ hasModelContext: false });
  assert.doesNotThrow(() => handle.unregisterPageTools());
  assert.deepEqual(handle.registered, []);
});

test('unregisterPageTools clears everything it registered when the browser supports WebMCP', () => {
  const { handle } = instantiate({ hasModelContext: true });
  handle.abort.abort(); // already fired once by the tail's own listener wiring in a real page; harmless here
  assert.doesNotThrow(() => handle.unregisterPageTools());
  assert.deepEqual(handle.registered, []);
});

test('registerPageTool wraps exactly the mutating tools in withProgress, dev_write_files included', async () => {
  // Through the real registration path: a wrapped `execute` is counted in
  // `outstanding` while it runs, an unwrapped one is not. Each `dev_` name is
  // matched whole, so `dev_write_files` is mutating in its own right — not
  // because `dev_write_file` happens to be a prefix of it — and a name that
  // merely starts with a mutator's is not.
  const mutating = [
    'dev_write_file',
    'dev_write_files',
    'dev_delete_file',
    'dev_create_block',
    'dev_rollback',
    'dev_remove_block',
    'shop_create_product',
    'shop_publish_offer'
  ];
  const reads = [
    'dev_status',
    'dev_list_files',
    'dev_read_file',
    'dev_list_generations',
    'dev_get_generation',
    'dev_read_reference',
    'dev_export_manifest',
    'dev_write_files_preview',
    'shop_list_products',
    'shop_list_offers'
  ];
  const { handle, tools } = instantiate({ hasModelContext: true });
  for (const name of [...mutating, ...reads]) {
    let release;
    const gate = new Promise((r) => (release = r));
    handle.registerPageTool({ name, execute: async () => gate });
    const pending = tools.get(name).execute({});
    await Promise.resolve();
    assert.equal(handle.outstanding, mutating.includes(name) ? 1 : 0, name);
    release({ content: [] });
    await pending;
    assert.equal(handle.outstanding, 0, name);
  }
});
