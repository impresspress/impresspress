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

// What the answer says a reload will do — see `runtimeStopped`.
const RESTART = 'Reload the page to restart it.';
const RECOVER = 'Reload the page to recover.';
const ERASES =
  'Reloading the page may run a recovery that erases the data this browser stores for the app.';

async function assertStoppedAnswer(response, cause, next = RESTART) {
  assert.equal(response.status, 503);
  assert.equal(response.headers.get('content-type'), 'application/json');
  assert.equal(response.headers.get('cache-control'), 'no-store');
  assert.deepEqual(await response.json(), {
    error: 'Unavailable',
    message: `The app's runtime stopped (${cause}). ${next}`,
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

  // A navigation that still reaches it goes to the network, and the boot
  // shell it lands on is left the cause.
  const navigation = await worker.request('/', { mode: 'navigate' });
  assert.equal(navigation.response.status, 405);
  assert.deepEqual(worker.network, [navigation.sent]);
  assert.equal(worker.leftForBootShell().reason, 'error handling request: Error: trap 1');
});

test('a navigation the runtime dies on goes to the network and sends the pages to the boot shell', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ handle_request: trap() });

  const { response, sent } = await worker.request('/b/auth/login', { mode: 'navigate' });

  assert.equal(response.status, 405, "the network's own answer, untouched");
  assert.deepEqual(worker.network, [sent]);
  assert.equal(worker.unregistered(), 1);
  assertSentToBootShell(worker, 'error handling request: Error: unreachable executed');
});

// The last sentence of the answer is a statement about what a reload does,
// and in one rendering a reload erases data.
test('the answer says what a reload will do, and what it erases', async (t) => {
  captureConsole(t);
  const cause = 'error handling request: Error: unreachable executed';
  const post = (worker) => worker.request(LOGIN, { method: 'POST' });

  // Left alone and unregistered: a reload is an ordinary boot, in either
  // rendering. Nothing is recovered, so nothing is erased.
  for (const wipe of [false, true]) {
    const worker = await loadWorker({ handle_request: trap() }, { wipe });
    await assertStoppedAnswer((await post(worker)).response, cause, RESTART);
  }

  // Could not unregister: a reload comes back to this worker, which leaves
  // the cause for the boot shell, which recovers.
  const stuck = await loadWorker({ handle_request: trap(), unregisters: false });
  await assertStoppedAnswer((await post(stuck)).response, cause, RECOVER);
  const stuckWipe = await loadWorker({ handle_request: trap(), unregisters: false }, { wipe: true });
  await assertStoppedAnswer((await post(stuckWipe)).response, cause, ERASES);

  // Sent to the boot shell (the runtime never started): the same.
  const init = 'runtime initialize() failed: Error: unreachable executed';
  const dead = await loadWorker({ initialize: trap() }, { wipe: true });
  await assertStoppedAnswer((await post(dead)).response, init, ERASES);
});

test('a worker whose unregister throws is still poisoned and still answers', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({
    handle_request: trap(),
    unregisters: () => {
      throw new Error('registration is gone');
    }
  });

  const { response } = await worker.request(LOGIN, { method: 'POST' });

  await assertStoppedAnswer(
    response,
    'error handling request: Error: unreachable executed',
    RECOVER
  );
});

test('a runtime that fails to initialize answers with the cause and sends the pages to the boot shell', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ initialize: trap('migration 0007 failed') });
  const cause = 'runtime initialize() failed: Error: migration 0007 failed';

  // The boot shell's probe is the usual first request: not a navigation.
  const { response } = await worker.request('/', { method: 'GET' });

  await assertStoppedAnswer(response, cause, RECOVER);
  assertSentToBootShell(worker, cause);
});

test('a wasm module that fails to load does the same', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ init: trap('import failed') });
  const cause = 'wasm module load failed: Error: import failed';

  const { response } = await worker.request('/', { method: 'GET' });

  await assertStoppedAnswer(response, cause, RECOVER);
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

  assert.ok(message.endsWith(`…). ${RESTART}`), message);
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
