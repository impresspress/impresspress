// How the rendered service worker gets its runtime binary.
//
// The incident: a returning visitor's worker had been stopped by the browser
// while idle. The visit after a deploy started that OLD worker again, which
// fetched its hashed runtime binary from the host — deleted by the deploy, a
// 404 — died at stage `load`, and sent the page into the boot shell's
// recovery while the browser was installing the new deployment's worker.
// `sw-update.spec.ts` timed out on it.
//
// So a worker keeps its own runtime in Cache Storage at `install` and boots
// from there (`RUNTIME_CACHE` in `sw.js.tmpl`). Who replaces a worker that
// dies anyway — the recovery or an update — is the loader's to decide
// (`updateUnderway`, `loader_recovery.test.mjs`).
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  captureConsole,
  CLIENT_URL,
  loadWorker,
  RUNTIME_BYTES,
  RUNTIME_CACHE,
  RUNTIME_URL,
  SHELL_HTML
} from './harness.mjs';

const LOGIN = '/b/auth/api/login';

/// `init()` as wasm-bindgen's glue does it: the binary it is handed is
/// awaited — handed none, it fetches the one it was built for from the host —
/// and one the host did not serve cannot be compiled.
const compiles = async (options) => {
  const binary = await (options?.module_or_path ?? fetch(RUNTIME_URL));
  if (!binary.ok) {
    throw new TypeError("Failed to execute 'compile' on 'WebAssembly': HTTP status code is not ok");
  }
};

/// A host that has the boot shell and nothing else: the deploy after this
/// worker's has deleted its runtime binary.
const deployedOver = async (request) =>
  request === '/'
    ? new Response(SHELL_HTML, { status: 200, headers: { 'Content-Type': 'text/html' } })
    : new Response('no such file', { status: 404 });

/// The binary the worker handed `init()` on its `n`th call, as text.
const handed = async (worker, n = 0) => {
  const options = worker.inits[n];
  assert.ok(options?.module_or_path, 'init() was handed the binary to load');
  return new Uint8Array(await (await options.module_or_path).arrayBuffer());
};

test('the runtime binary is keyed by the hashed name the bundler gave it', () => {
  assert.match(RUNTIME_URL, /^\/app_bg-[0-9a-f]{8}\.wasm$/);
});

test('installing keeps the runtime binary, and the runtime is loaded from what was kept', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ kept: false, init: compiles });

  await worker.lifecycle('install');
  assert.deepEqual(worker.runtimesKept(), [RUNTIME_URL]);
  assert.deepEqual(worker.network, [RUNTIME_URL]);

  worker.network.length = 0;
  const { response } = await worker.request(LOGIN, { method: 'POST' });
  assert.equal(await response.text(), 'from the runtime');
  assert.deepEqual(await handed(worker), RUNTIME_BYTES);
  assert.deepEqual(worker.network, [], 'the host is not asked for the binary again');
});

test('a restarted worker boots the runtime it kept, after a deploy deleted it from the host', async (t) => {
  const errors = captureConsole(t);
  const worker = await loadWorker({ init: compiles, host: deployedOver });

  const { response } = await worker.request('/b/auth/login', { mode: 'navigate' });

  assert.equal(await response.text(), 'from the runtime');
  assert.deepEqual(await handed(worker), RUNTIME_BYTES);
  assert.deepEqual(worker.network, []);
  assert.deepEqual(worker.posted, []);
  assert.deepEqual(worker.navigated, []);
  assert.equal(worker.leftForBootShell(), undefined);
  assert.deepEqual(errors, []);
});

test('a version whose runtime binary cannot be fetched does not install', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ kept: false, host: deployedOver });

  await assert.rejects(worker.lifecycle('install'), /could not be fetched: HTTP 404/);
  assert.deepEqual(worker.runtimesKept(), []);
});

test('a version installed again — a recovery’s replacement — keeps the binary it already has', async (t) => {
  captureConsole(t);
  const worker = await loadWorker();

  await worker.lifecycle('install');

  assert.deepEqual(worker.network, []);
  assert.deepEqual(worker.runtimesKept(), [RUNTIME_URL]);
});

test('activating drops the runtime binaries other versions kept, and claims the pages', async (t) => {
  captureConsole(t);
  const worker = await loadWorker();
  const cache = await caches.open(RUNTIME_CACHE);
  await cache.put('/app_bg-00000000.wasm', new Response('the previous version’s runtime'));
  assert.equal(worker.runtimesKept().length, 2);

  await worker.lifecycle('activate');

  assert.deepEqual(worker.runtimesKept(), [RUNTIME_URL]);
  assert.equal(worker.claimed(), 1);
});

test('a worker whose kept binary has gone loads it from the host', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ kept: false, init: compiles });

  const { response } = await worker.request(LOGIN, { method: 'POST' });

  assert.equal(await response.text(), 'from the runtime');
  assert.deepEqual(await handed(worker), RUNTIME_BYTES);
  assert.deepEqual(worker.network, [RUNTIME_URL]);
});

test('…and where neither has it, the worker dies at stage load and sends the pages to the boot shell', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ kept: false, init: compiles, host: deployedOver });

  const { response } = await worker.request('/', { method: 'GET' });

  const body = await response.json();
  assert.equal(response.status, 503);
  assert.equal(body.stage, 'load');
  assert.match(body.cause, /HTTP status code is not ok/);
  assert.equal(worker.leftForBootShell().stage, 'load');
  assert.deepEqual(worker.navigated, [CLIENT_URL]);
});

// A host that falls back to its app shell for unknown paths (the dev
// sandbox's `not_found_handling = "single-page-application"`) answers a
// binary it no longer has with a 200 and an HTML page. Kept, it would be a
// version that can never load — and its `activate` would drop the binary of
// the version that worked.
test('a 200 that is not a WebAssembly module is not kept, and the version does not install', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({
    kept: false,
    host: async () =>
      new Response('<!doctype html><title>the app shell</title>', {
        status: 200,
        headers: { 'Content-Type': 'text/html' }
      })
  });

  await assert.rejects(worker.lifecycle('install'), /is not a WebAssembly module \(the host answered text\/html\)/);
  assert.deepEqual(worker.runtimesKept(), []);
});

test('the binary is kept as WebAssembly, whatever the host labelled it', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({
    kept: false,
    host: async () => new Response(RUNTIME_BYTES, { status: 200, headers: { 'Content-Type': 'application/octet-stream' } })
  });

  await worker.lifecycle('install');

  const kept = worker.keptEntry(RUNTIME_URL);
  assert.deepEqual([...kept.body], [...RUNTIME_BYTES]);
  assert.deepEqual(kept.headers, [['content-type', 'application/wasm']]);
});

// A full quota must not refuse the version: the binary is on the host, and
// refusing would leave "Reset" — which erases only once its replacement is
// in — no way to free the space.
test('a full quota does not fail the install: the runtime is loaded from the host', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ kept: false, quotaFull: true, init: compiles });

  await worker.lifecycle('install');
  assert.deepEqual(worker.runtimesKept(), []);

  worker.network.length = 0;
  const { response } = await worker.request(LOGIN, { method: 'POST' });
  assert.equal(await response.text(), 'from the runtime');
  assert.deepEqual(await handed(worker), RUNTIME_BYTES);
  assert.deepEqual(worker.network, [RUNTIME_URL]);
});

// Once a version is active, a death an earlier version left for the boot
// shell is no longer anyone's to recover from: a shell opened within the
// minute it stays fresh would otherwise replace the version now in place.
test('activating drops the cause a replaced worker left for the boot shell', async (t) => {
  captureConsole(t);
  const worker = await loadWorker();
  const cache = await caches.open('__impresspress_sw_stopped');
  await cache.put('/__impresspress_sw_stopped', new Response(JSON.stringify({ reason: 'x', at: 1 })));
  assert.ok(worker.leftForBootShell());

  await worker.lifecycle('activate');

  assert.equal(worker.leftForBootShell(), undefined);
});

// Which version a worker is — dead or alive — is what the boot shell
// compares a death with (`updateUnderway` in `loader.js`).
test('a worker says which runtime it was built for, dead or alive', async (t) => {
  captureConsole(t);
  for (const runtime of [{}, { initialize: async () => { throw new Error('dead'); } }]) {
    const worker = await loadWorker(runtime);
    await worker.request('/', { method: 'GET' });
    const { port1, port2 } = new MessageChannel();
    const answer = new Promise((resolve) => {
      port1.onmessage = (event) => {
        port1.close();
        resolve(event.data);
      };
    });
    await worker.message({ type: 'impresspress-runtime' }, [port2]);
    assert.deepEqual(await answer, { runtime: RUNTIME_URL });
  }
});


// The runtime loads the app's database into memory and writes it back on
// every flush, so an erase must come BEFORE it loads, never after: the
// erase holds ERASE_LOCK from before the worker even exists until it is
// done, and the runtime waits for that lock before it loads anything.
test('the runtime waits for a pending erase before it loads the data', async (t) => {
  captureConsole(t);
  let finishErase;
  const erasePending = new Promise((resolve) => {
    finishErase = resolve;
  });
  const worker = await loadWorker({ erasePending });

  const answered = worker.request(LOGIN, { method: 'POST' });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(worker.timeline, ['lock __impresspress_erase'], 'nothing loaded while the erase is pending');

  finishErase();
  assert.equal(await (await answered).response.text(), 'from the runtime');
  assert.deepEqual(worker.timeline, ['lock __impresspress_erase', 'initialize']);
  assert.deepEqual(worker.initializeHeld, [[]], 'the lock is let go before loading');
});

// …and it never holds the lock while it starts, so a start that takes long
// holds up no tab's erase or recovery.
test('a slow start holds no lock: an erase elsewhere is not held up', async (t) => {
  captureConsole(t);
  let finishStart;
  const started = new Promise((resolve) => {
    finishStart = resolve;
  });
  const worker = await loadWorker({ initialize: () => started });

  const answered = worker.request(LOGIN, { method: 'POST' });
  while (!worker.timeline.includes('initialize')) await new Promise((resolve) => setImmediate(resolve));

  assert.equal(await worker.requestLock('__impresspress_erase', async () => 'erased'), 'erased');

  finishStart();
  assert.equal(await (await answered).response.text(), 'from the runtime');
});
