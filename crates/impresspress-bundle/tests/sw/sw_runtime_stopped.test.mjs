// What the rendered service worker answers once its wasm runtime is dead.
//
// The incident these pin: a login `POST /b/auth/api/login` whose
// `handle_request` threw was handed to the static host, which answered an
// empty 405; the login page could only say "Something went wrong", and the
// cause was in the worker's console, which neither the person nor the agent
// driving the browser could read. See `runtimeStopped` in `sw.js.tmpl`.
import test from 'node:test';
import assert from 'node:assert/strict';
import { captureConsole, loadWorker, ORIGIN, SHELL_HTML } from './harness.mjs';

const LOGIN = '/b/auth/api/login';

const trap = (message = 'unreachable executed') => async () => {
  throw new Error(message);
};

// What the answer says to do next, and what it costs — see `runtimeStopped`.
// The address is the boot shell's, which every host answers; "reload the
// page" is only true where the host answers the page's own path.
const RESTART = `Open ${ORIGIN}/ to restart it.`;
const RECOVER = `Open ${ORIGIN}/ to recover.`;
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

/// The navigation was answered with the boot shell the host has at `/`, and
/// the host was asked for nothing else — in particular not for the path that
/// was navigated to, which it would answer with its 404.
async function assertAnsweredWithBootShell(worker, response) {
  assert.equal(response.status, 200);
  assert.equal(response.headers.get('content-type'), 'text/html');
  assert.equal(await response.text(), SHELL_HTML);
  assert.deepEqual(worker.network, ['/']);
}

/// The page is left exactly as it is: not told, not navigated, nothing left
/// for a boot shell it is not going to.
function assertPageLeftAlone(worker) {
  assert.deepEqual(worker.posted, []);
  assert.deepEqual(worker.navigated, []);
  assert.equal(worker.leftForBootShell(), undefined);
}

/// Every open page is sent to the boot shell — to the shell's own address,
/// not back to the runtime path it was on (`/b/auth/login` in the harness),
/// which a host with no fallback cannot answer — with the cause both ways it
/// can arrive there.
function assertSentToBootShell(worker, cause) {
  assert.deepEqual(worker.posted, [{ type: 'sw-self-destruct', reason: cause }]);
  assert.deepEqual(worker.navigated, ['/']);
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

  // A navigation that still reaches it — to a path only the runtime served —
  // is answered with the boot shell, which is left the cause.
  const navigation = await worker.request('/b/products/', { mode: 'navigate' });
  await assertAnsweredWithBootShell(worker, navigation.response);
  assert.equal(worker.leftForBootShell().reason, 'error handling request: Error: trap 1');
});

// The harness's host is a plain file server: it has the shell at `/` and a
// 404 for `/b/auth/login`. Handing it the navigation — which is what this did
// before — gets the 404, the shell never loads, and the cause is never shown.
test('a navigation the runtime dies on is answered with the boot shell, whatever the host does with unknown paths', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ handle_request: trap() });

  const { response } = await worker.request('/b/auth/login', { mode: 'navigate' });

  await assertAnsweredWithBootShell(worker, response);
  assert.equal(worker.unregistered(), 1);
  assertSentToBootShell(worker, 'error handling request: Error: unreachable executed');
});

test('the shell answer is a document of its own, not the host’s response handed on', async (t) => {
  captureConsole(t);
  // A host that answers `/` by redirecting gives a response marked
  // `redirected`, which a browser refuses as the answer to a navigation.
  const fromHost = new Response(SHELL_HTML, { status: 200, headers: { 'Content-Type': 'text/html' } });
  Object.defineProperty(fromHost, 'redirected', { value: true });
  const worker = await loadWorker({ handle_request: trap(), host: async () => fromHost });

  const { response } = await worker.request('/b/auth/login', { mode: 'navigate' });

  assert.notEqual(response, fromHost);
  assert.equal(response.redirected, false);
  assert.equal(await response.text(), SHELL_HTML);
});

test('a host with no shell at its address is passed on as it answered', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({
    handle_request: trap(),
    host: async () => new Response('no such file', { status: 404 })
  });

  const { response } = await worker.request('/b/auth/login', { mode: 'navigate' });

  assert.equal(response.status, 404);
  assert.equal(await response.text(), 'no such file');
});

// The last sentence of the answer is a statement about what loading the app
// again does, and in one rendering that erases data.
test('the answer says what loading the app again will do, and what it erases', async (t) => {
  captureConsole(t);
  const cause = 'error handling request: Error: unreachable executed';
  const post = (worker) => worker.request(LOGIN, { method: 'POST' });

  // Left alone and unregistered: loading the app again is an ordinary boot,
  // in either rendering. Nothing is recovered, so nothing is erased.
  for (const wipe of [false, true]) {
    const worker = await loadWorker({ handle_request: trap() }, { wipe });
    await assertStoppedAnswer((await post(worker)).response, cause, RESTART);
  }

  // Could not unregister: a navigation comes back to this worker, which
  // leaves the cause for the boot shell, which recovers.
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
  assert.ok(message.length < 420, `message is ${message.length} characters`);
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
