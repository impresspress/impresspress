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

async function assertStoppedAnswer(response, cause) {
  assert.equal(response.status, 503);
  assert.equal(response.headers.get('content-type'), 'application/json');
  assert.equal(response.headers.get('cache-control'), 'no-store');
  const body = await response.json();
  assert.equal(body.error, 'Unavailable');
  assert.equal(body.code, 'runtime_stopped');
  assert.equal(body.message, `The app's runtime stopped (${cause}). Reload the page to restart it.`);
}

test('a request whose handle_request throws gets a 503 that names the cause', async (t) => {
  const errors = captureConsole(t);
  const worker = await loadWorker({
    handle_request: async () => {
      throw new Error('unreachable executed');
    }
  });

  const { response } = await worker.request(LOGIN, { method: 'POST' });

  await assertStoppedAnswer(response, 'error handling request: Error: unreachable executed');
  assert.deepEqual(worker.network, [], 'the static host must not be asked');
  // The recovery path is untouched: still logged, still unregistered, the
  // page still told and still re-navigated.
  assert.ok(errors.some((line) => line.includes('Error handling request')), errors.join('\n'));
  assert.equal(worker.unregistered(), 1);
  assert.equal(worker.posted.length, 1);
  assert.equal(worker.posted[0].type, 'sw-self-destruct');
  assert.equal(worker.navigated.length, 1);
});

test('a navigation still falls through to the network when handle_request throws', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({
    handle_request: async () => {
      throw new Error('unreachable executed');
    }
  });

  const { response, sent } = await worker.request('/b/auth/login', { mode: 'navigate' });

  assert.equal(response.status, 405, "the network's own answer, untouched");
  assert.deepEqual(worker.network, [sent]);
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

  const navigation = await worker.request('/', { mode: 'navigate' });
  assert.equal(navigation.response.status, 405);
  assert.deepEqual(worker.network, [navigation.sent]);
  assert.equal(worker.unregistered(), 1, 'self-destruct runs once');
});

test('a runtime that fails to initialize says so to the request that started it', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({
    initialize: async () => {
      throw new Error('migration 0007 failed');
    }
  });

  const { response } = await worker.request(LOGIN, { method: 'POST' });

  await assertStoppedAnswer(response, 'runtime initialize() failed: Error: migration 0007 failed');
});

test('a long cause is cut, not sent whole', async (t) => {
  captureConsole(t);
  const worker = await loadWorker({
    handle_request: async () => {
      throw new Error('x'.repeat(5000));
    }
  });

  const { response } = await worker.request(LOGIN, { method: 'POST' });
  const { message } = await response.json();

  assert.ok(message.includes('…). Reload the page to restart it.'), message);
  assert.ok(message.length < 400, `message is ${message.length} characters`);
});

test('a working runtime still answers, and a bypassed request is still left alone', async (t) => {
  captureConsole(t);
  const worker = await loadWorker();

  const handled = await worker.request(LOGIN, { method: 'POST' });
  assert.equal(await handled.response.text(), 'from the runtime');

  const bypassed = await worker.request('/loader.js');
  assert.equal(bypassed.response, undefined, 'the worker must not answer a bypassed path');
});
