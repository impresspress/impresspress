// What the rendered service worker answers once its wasm runtime is dead.
//
// The incident these pin: a login `POST /b/auth/api/login` whose
// `handle_request` threw was handed to the static host, which answered an
// empty 405; the login page could only say "Something went wrong", and the
// cause was in the worker's console, which neither the person nor the agent
// driving the browser could read. See `runtimeStopped` in `sw.js.tmpl`.
import test from 'node:test';
import assert from 'node:assert/strict';
import { captureConsole, loadWorker } from './harness.mjs';

const LOGIN = '/b/auth/api/login';

const trap = (message = 'unreachable executed') => async () => {
  throw new Error(message);
};

async function assertStoppedAnswer(response, cause) {
  assert.equal(response.status, 503);
  assert.equal(response.headers.get('content-type'), 'application/json');
  assert.equal(response.headers.get('cache-control'), 'no-store');
  assert.deepEqual(await response.json(), {
    error: 'Unavailable',
    message: `The app's runtime stopped (${cause}). Reload the page to restart it.`,
    code: 'runtime_stopped',
    cause
  });
}

/// The page is left exactly as it is: not told, not navigated, nothing left
/// for a boot shell it is not going to.
function assertPageLeftAlone(worker) {
  assert.deepEqual(worker.posted, []);
  assert.deepEqual(worker.navigated, []);
  assert.equal(worker.leftForBootShell(), undefined);
}

/// A navigation is redirected to its own URL.
function assertSentRound(response, pathname) {
  assert.equal(response.status, 302);
  assert.equal(response.headers.get('location'), `https://app.example${pathname}`);
}

/// Every open page is sent to the boot shell, with the cause both ways it
/// can arrive there.
function assertSentToBootShell(worker, cause) {
  assert.deepEqual(worker.posted, [{ type: 'sw-self-destruct', reason: cause }]);
  assert.equal(worker.navigated.length, 1);
  const left = worker.leftForBootShell();
  assert.equal(left.reason, cause);
  assert.equal(typeof left.at, 'number');
}

test('a request from a page that the runtime dies on gets the cause, and the page is left alone', async (t) => {
  const errors = captureConsole(t);
  const worker = await loadWorker({ handle_request: trap() });

  const { response } = await worker.request(LOGIN, { method: 'POST' });

  await assertStoppedAnswer(response, 'error handling request: Error: unreachable executed');
  assert.deepEqual(worker.network, [], 'the static host must not be asked');
  assert.ok(errors.some((line) => line.includes('Error handling request')), errors.join('\n'));
  // Unregistered, so a reload gets a fresh worker — and NOT navigated, which
  // would replace the page the answer above was for.
  assert.equal(worker.unregistered(), 1);
  assertPageLeftAlone(worker);
});

test('a poisoned worker answers later requests the same way, with the first cause', async (t) => {
  captureConsole(t);
  let calls = 0;
  const worker = await loadWorker({
    handle_request: async () => {
      calls += 1;
      throw new Error(`trap ${calls}`);
    }
  });
  await worker.request(LOGIN, { method: 'POST' });

  const api = await worker.request('/b/auth/api/signup', { method: 'POST' });
  await assertStoppedAnswer(api.response, 'error handling request: Error: trap 1');
  assert.equal(calls, 1, 'a dead runtime is not called again');
  assert.deepEqual(worker.network, []);
  assert.equal(worker.unregistered(), 1, 'self-destruct runs once');
  assertPageLeftAlone(worker);

  // A navigation that still reaches it is sent round to the static host,
  // and the boot shell it lands on is left the cause.
  const navigation = await worker.request('/', { mode: 'navigate' });
  assertSentRound(navigation.response, '/');
  assert.deepEqual(worker.network, []);
  assert.equal(worker.leftForBootShell().reason, 'error handling request: Error: trap 1');
});

test('a navigation the runtime dies on goes to the network and sends the pages to the boot shell', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ handle_request: trap() });

  const first = await worker.request('/b/auth/login', { mode: 'navigate' });

  // Not answered from here, not even with the network's bytes: a redirect to
  // itself, which an unregistered worker no longer sees.
  assertSentRound(first.response, '/b/auth/login');
  assert.deepEqual(worker.network, []);
  assert.equal(worker.unregistered(), 1);
  assertSentToBootShell(worker, 'error handling request: Error: unreachable executed');

  // If it comes back anyway, it is not sent round a second time.
  const second = await worker.request('/b/auth/login', { mode: 'navigate' });
  assert.equal(second.response.status, 405, "the network's own answer");
  assert.deepEqual(worker.network, [second.sent]);
});

test('a worker that could not unregister answers a navigation from the network', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ handle_request: trap(), unregisters: false });

  const { response, sent } = await worker.request('/', { mode: 'navigate' });

  // A redirect would come straight back to this worker.
  assert.equal(response.status, 405);
  assert.deepEqual(worker.network, [sent]);
});

test('a runtime that fails to initialize answers with the cause and sends the pages to the boot shell', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ initialize: trap('migration 0007 failed') });
  const cause = 'runtime initialize() failed: Error: migration 0007 failed';

  // The boot shell's probe is the usual first request: not a navigation.
  const { response } = await worker.request('/', { method: 'GET' });

  await assertStoppedAnswer(response, cause);
  assertSentToBootShell(worker, cause);
});

test('a wasm module that fails to load does the same', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ init: trap('import failed') });
  const cause = 'wasm module load failed: Error: import failed';

  const { response } = await worker.request('/', { method: 'GET' });

  await assertStoppedAnswer(response, cause);
  assertSentToBootShell(worker, cause);
});

test('a client that refuses to be navigated is a warning, not an unhandled rejection', async (t) => {
  captureConsole(t);
  const warned = [];
  t.mock.method(console, 'warn', (...args) => warned.push(args.map(String).join(' ')));
  const unhandled = [];
  const onUnhandled = (reason) => unhandled.push(reason);
  process.on('unhandledRejection', onUnhandled);
  t.after(() => process.off('unhandledRejection', onUnhandled));
  const worker = await loadWorker({
    initialize: trap(),
    navigate: () => Promise.reject(new TypeError('not the active worker'))
  });

  await worker.request('/', { method: 'GET' });
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(unhandled, []);
  assert.ok(warned.some((line) => line.includes('not the active worker')), warned.join('\n'));
});

test('a long cause is cut, not sent whole', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ handle_request: trap('x'.repeat(5000)) });

  const { response } = await worker.request(LOGIN, { method: 'POST' });
  const { message, cause } = await response.json();

  assert.ok(message.includes('…). Reload the page to restart it.'), message);
  assert.ok(message.length < 400, `message is ${message.length} characters`);
  assert.ok(cause.endsWith('…') && cause.length < 320, `cause is ${cause.length} characters`);
});

test('a working runtime still answers, and a bypassed request is still left alone', async (t) => {
  captureConsole(t);
  const worker = await loadWorker();

  const handled = await worker.request(LOGIN, { method: 'POST' });
  assert.equal(await handled.response.text(), 'from the runtime');

  const bypassed = await worker.request('/loader.js');
  assert.equal(bypassed.response, undefined, 'the worker must not answer a bypassed path');
});
