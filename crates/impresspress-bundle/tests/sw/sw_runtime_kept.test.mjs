// How the rendered service worker gets its runtime binary, and who replaces
// it when it dies while an update is on its way.
//
// The incident: a returning visitor's worker had been stopped by the browser
// while idle. The visit after a deploy started that OLD worker again, which
// fetched its hashed runtime binary from the host — deleted by the deploy, a
// 404 — died at stage `load`, and sent the page into the boot shell's
// recovery while the browser was installing the new deployment's worker. Two
// replacements of one worker at once; `sw-update.spec.ts` timed out on it.
//
// So: a worker keeps its own runtime in Cache Storage at `install` and boots
// from there (`RUNTIME_CACHE` in `sw.js.tmpl`), and a dead worker that a
// newer version is already coming to replace reports no death, leaving the
// transition to the update (`beingReplaced`).
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

const trap = (message = 'unreachable executed') => async () => {
  throw new Error(message);
};

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
  return (await options.module_or_path).text();
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
  assert.equal(await handed(worker), RUNTIME_BYTES);
  assert.deepEqual(worker.network, [], 'the host is not asked for the binary again');
});

test('a restarted worker boots the runtime it kept, after a deploy deleted it from the host', async (t) => {
  const errors = captureConsole(t);
  const worker = await loadWorker({ init: compiles, host: deployedOver });

  const { response } = await worker.request('/b/auth/login', { mode: 'navigate' });

  assert.equal(await response.text(), 'from the runtime');
  assert.equal(await handed(worker), RUNTIME_BYTES);
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
  assert.equal(await handed(worker), RUNTIME_BYTES);
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

// One of the two owns the transition: a newer version already coming in is
// the update's, so the dead worker tells nobody — no message, no navigation,
// no cause for a boot shell to recover from. The shell a navigation gets
// from it boots onto the version coming in.
for (const coming of ['installing', 'waiting']) {
  test(`a dead worker that a newer version is ${coming} to replace reports no death`, async (t) => {
    captureConsole(t);
    const worker = await loadWorker({
      initialize: trap('migration 0007 failed'),
      update: async () => coming
    });

    const probe = await worker.request('/', { method: 'GET' });
    // The request itself still gets the truth.
    assert.equal(probe.response.status, 503);
    assert.equal((await probe.response.json()).stage, 'initialize');
    assert.ok(worker.updates() >= 1, 'it asked whether a newer version is coming');
    assert.deepEqual(worker.posted, []);
    assert.deepEqual(worker.navigated, []);
    assert.equal(worker.leftForBootShell(), undefined);

    const navigation = await worker.request('/b/products/', { mode: 'navigate' });
    assert.equal(await navigation.response.text(), SHELL_HTML);
    assert.equal(worker.leftForBootShell(), undefined, 'the shell boots, it does not recover');
  });
}

test('a dead worker asked for after a deploy leaves no cause for the shell either', async (t) => {
  captureConsole(t);
  let deployed = false;
  const worker = await loadWorker({
    handle_request: trap(),
    update: async () => (deployed ? 'installing' : null)
  });
  // Dead on a request from a page: the page keeps the answer, nothing else.
  await worker.request(LOGIN, { method: 'POST' });

  deployed = true;
  const { response } = await worker.request('/b/auth/login', { mode: 'navigate' });

  assert.equal(await response.text(), SHELL_HTML);
  assert.equal(worker.leftForBootShell(), undefined);
});

test('a dead worker that nothing is replacing reports its death, after checking', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ initialize: trap(), update: async () => null });

  await worker.request('/', { method: 'GET' });

  assert.ok(worker.updates() >= 1);
  assert.equal(worker.posted.length, 1);
  assert.deepEqual(worker.navigated, [CLIENT_URL]);
  assert.equal(worker.leftForBootShell().stage, 'initialize');
});

test('an update check that cannot reach the host is not a newer version', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({
    initialize: trap(),
    update: async () => {
      throw new TypeError('Failed to update a ServiceWorker: network error');
    }
  });

  await worker.request('/', { method: 'GET' });

  assert.equal(worker.posted.length, 1);
  assert.equal(worker.leftForBootShell().stage, 'initialize');
});
