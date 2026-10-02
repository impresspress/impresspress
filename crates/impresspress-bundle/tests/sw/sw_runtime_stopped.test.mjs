// What the rendered service worker answers once its wasm runtime is dead.
//
// The incident these pin: a login `POST /b/auth/api/login` whose
// `handle_request` threw was handed to the static host, which answered an
// empty 405; the login page could only say "Something went wrong", and the
// cause was in the worker's console, which neither the person nor the agent
// driving the browser could read. See `runtimeStopped` in `sw.js.tmpl`.
//
// And what came after: a dead worker STAYS REGISTERED, so that every
// navigation in its scope still reaches it and gets the boot shell — on a
// plain file server, which answers a path only the runtime serves with its
// own 404, that is the only way the person ever sees the cause. It reports
// WHERE it failed (`stage`) beside the cause, on every road to the loader.
import test from 'node:test';
import assert from 'node:assert/strict';
import { captureConsole, CLIENT_URL, loadWorker, SHELL_HTML } from './harness.mjs';

const LOGIN = '/b/auth/api/login';

const trap = (message = 'unreachable executed') => async () => {
  throw new Error(message);
};

// What the answer says a reload of the page does — see `runtimeStopped`.
const RESTART = 'Reload the page to restart it; the data this browser stores for the app is kept.';
const ERASES =
  'Reloading the page may run a recovery that erases the data this browser stores for the app.';

const TRAPPED = 'error handling request: Error: unreachable executed';

async function assertStoppedAnswer(response, cause, stage, next = RESTART) {
  assert.equal(response.status, 503);
  assert.equal(response.headers.get('content-type'), 'application/json');
  assert.equal(response.headers.get('cache-control'), 'no-store');
  assert.deepEqual(await response.json(), {
    error: 'Unavailable',
    message: `The app's runtime stopped (${cause}). ${next}`,
    code: 'runtime_stopped',
    cause,
    stage
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

/// Every open page is sent to the boot shell — by a navigation to its OWN
/// address, which this still-registered worker answers with the shell — with
/// the cause and the stage both ways they can arrive there.
function assertSentToBootShell(worker, cause, stage) {
  assert.deepEqual(worker.posted, [{ type: 'sw-self-destruct', reason: cause, stage }]);
  assert.deepEqual(worker.navigated, [CLIENT_URL]);
  const left = worker.leftForBootShell();
  assert.equal(left.reason, cause);
  assert.equal(left.stage, stage);
  assert.equal(typeof left.at, 'number');
}

test('a request from a page that the runtime dies on gets the cause, and the page is left alone', async (t) => {
  const errors = captureConsole(t);
  const worker = await loadWorker({ handle_request: trap() });

  const { response } = await worker.request(LOGIN, { method: 'POST' });

  await assertStoppedAnswer(response, TRAPPED, 'request');
  assert.deepEqual(worker.network, [], 'the static host must not be asked');
  assert.ok(errors.some((line) => line.includes('Error handling request')), errors.join('\n'));
  // NOT navigated, which would replace the page the answer above was for.
  assertPageLeftAlone(worker);
});

test('a dead worker stays registered, whatever killed it', async (t) => {
  captureConsole(t);
  for (const runtime of [{ init: trap() }, { initialize: trap() }, { handle_request: trap() }]) {
    for (const mode of ['cors', 'navigate']) {
      const worker = await loadWorker(runtime);
      await worker.request('/b/auth/login', { mode });
      await worker.request('/b/auth/login', { mode });
      assert.equal(worker.unregistered(), 0);
    }
  }
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
  await assertStoppedAnswer(api.response, 'error handling request: Error: trap 1', 'request');
  assert.equal(calls, 1, 'a dead runtime is not called again');
  assert.deepEqual(worker.network, []);
  assertPageLeftAlone(worker);

  // The reload the answer speaks of — or any other navigation in scope, to a
  // path only the runtime served — is answered with the boot shell, which is
  // left the cause and the stage.
  const navigation = await worker.request('/b/products/', { mode: 'navigate' });
  await assertAnsweredWithBootShell(worker, navigation.response);
  assert.equal(worker.leftForBootShell().reason, 'error handling request: Error: trap 1');
  assert.equal(worker.leftForBootShell().stage, 'request');
  assert.equal(calls, 1);
});

// The harness's host is a plain file server: it has the shell at `/` and a
// 404 for `/b/auth/login`. Handing it the navigation — which is what this did
// before — gets the 404, the shell never loads, and the cause is never shown.
test('a navigation the runtime dies on is answered with the boot shell, whatever the host does with unknown paths', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ handle_request: trap() });

  const { response } = await worker.request('/b/auth/login', { mode: 'navigate' });

  await assertAnsweredWithBootShell(worker, response);
  assertSentToBootShell(worker, TRAPPED, 'request');
});

test('a deep link opened after the runtime failed to start gets the boot shell too', async (t) => {
  captureConsole(t);
  let starts = 0;
  const worker = await loadWorker({
    initialize: async () => {
      starts += 1;
      throw new Error('migration 0007 failed');
    }
  });
  await worker.request('/', { method: 'GET' });
  worker.network.length = 0;

  const { response } = await worker.request('/b/products/42', { mode: 'navigate' });

  await assertAnsweredWithBootShell(worker, response);
  assert.equal(worker.leftForBootShell().stage, 'initialize');
  assert.equal(starts, 1, 'this instance does not try the runtime again');
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

// The last sentence of the answer says what a reload does, and it follows
// the same gate as the loader's recovery: only a failure of `initialize()`,
// in a build rendered to erase, costs data.
test('the answer says what a reload will do, and erasing is said only where it happens', async (t) => {
  captureConsole(t);
  const post = (worker) => worker.request(LOGIN, { method: 'POST' });
  const cases = [
    [{ handle_request: trap() }, TRAPPED, 'request'],
    [{ init: trap() }, 'wasm module load failed: Error: unreachable executed', 'load'],
    [{ initialize: trap() }, 'runtime initialize() failed: Error: unreachable executed', 'initialize']
  ];
  for (const [runtime, cause, stage] of cases) {
    for (const wipe of [false, true]) {
      const worker = await loadWorker(runtime, { wipe });
      const next = wipe && stage === 'initialize' ? ERASES : RESTART;
      await assertStoppedAnswer((await post(worker)).response, cause, stage, next);
    }
  }
});

test('a runtime that fails to initialize answers with the cause and sends the pages to the boot shell', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ initialize: trap('migration 0007 failed') });
  const cause = 'runtime initialize() failed: Error: migration 0007 failed';

  // The boot shell's probe is the usual first request: not a navigation.
  const { response } = await worker.request('/', { method: 'GET' });

  await assertStoppedAnswer(response, cause, 'initialize');
  assertSentToBootShell(worker, cause, 'initialize');
});

test('a wasm module that fails to load does the same, as its own stage', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({ init: trap('Failed to fetch') });
  const cause = 'wasm module load failed: Error: Failed to fetch';

  const { response } = await worker.request('/', { method: 'GET' });

  await assertStoppedAnswer(response, cause, 'load');
  assertSentToBootShell(worker, cause, 'load');
});

// The stage is stated by the code that caught the failure, never read back
// out of the text: a request-stage error whose message happens to quote the
// initialize failure's wording is still a request-stage error.
test('the stage does not come from the wording of the cause', async (t) => {
  captureConsole(t);
  const worker = await loadWorker(
    { handle_request: trap('runtime initialize() failed: not really') },
    { wipe: true }
  );

  const { response } = await worker.request(LOGIN, { method: 'POST' });

  const body = await response.json();
  assert.equal(body.stage, 'request');
  assert.ok(body.message.endsWith(RESTART), body.message);
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
  assert.ok(message.length < 440, `message is ${message.length} characters`);
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
