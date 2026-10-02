// `apiPost` — the one function every auth form posts through
// (`pages/mod.rs`'s `api_post_script`, which is this directory's
// `api_post.js` verbatim). Run with
// `node --test crates/impresspress-core/src/blocks/auth_ui/assets/test/*.test.mjs`.
//
// What these pin is what a failed sign-in SAYS. The forms used to answer two
// of the three failures below with "Something went wrong" and the third with
// the error's code instead of its message.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const source = fs.readFileSync(path.join(here, '..', 'api_post.js'), 'utf8');

/// `apiPost` bound to a stub `fetch`. The file is a classic inline script (it
/// declares a global function), so it is evaluated as one, with `fetch` as
/// the only name it reaches outside itself.
function apiPostWith(fetchStub) {
  return new Function('fetch', `${source}\nreturn apiPost;`)(fetchStub);
}

const answering = (response) => async () => response;
const json = (status, body) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

test('posts the body as JSON and resolves to the answer', async () => {
  const calls = [];
  const apiPost = apiPostWith(async (url, init) => {
    calls.push({ url, init });
    return json(200, { access_token: 't' });
  });

  assert.deepEqual(await apiPost('/b/auth/api/login', { email: 'a@b.c' }), { access_token: 't' });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, '/b/auth/api/login');
  assert.equal(calls[0].init.method, 'POST');
  assert.equal(calls[0].init.headers['Content-Type'], 'application/json');
  assert.equal(calls[0].init.body, '{"email":"a@b.c"}');
});

test("a refusal in the runtime's error shape shows its message, not its code", async () => {
  const apiPost = apiPostWith(
    answering(
      json(401, {
        error: 'Unauthenticated',
        message: 'Invalid email or password',
        code: 'invalid_credentials'
      })
    )
  );
  await assert.rejects(apiPost('/x', {}), { message: 'Invalid email or password' });
});

test("the service worker's stopped-runtime answer is shown whole", async () => {
  const message =
    "The app's runtime stopped (error handling request: RuntimeError: unreachable). Reload the page to restart it.";
  const apiPost = apiPostWith(
    answering(json(503, { error: 'Unavailable', message, code: 'runtime_stopped' }))
  );
  await assert.rejects(apiPost('/x', {}), { message });
});

test('an error with no message falls back to its code, then to the status', async () => {
  await assert.rejects(apiPostWith(answering(json(403, { error: 'PermissionDenied' })))('/x', {}), {
    message: 'PermissionDenied'
  });
  await assert.rejects(apiPostWith(answering(json(500, {})))('/x', {}), {
    message: 'The app answered HTTP 500.'
  });
  await assert.rejects(apiPostWith(answering(json(200, null)))('/x', {}), {
    message: 'The app answered HTTP 200.'
  });
});

test('a 2xx body carrying `error` is still a refusal', async () => {
  const apiPost = apiPostWith(answering(json(200, { error: 'Internal', message: 'Nope' })));
  await assert.rejects(apiPost('/x', {}), { message: 'Nope' });
});

test('an answer that is not JSON names its HTTP status', async () => {
  // The static host's answer to a POST it has no route for: the incident.
  await assert.rejects(apiPostWith(answering(new Response(null, { status: 405 })))('/x', {}), {
    message: 'The app answered HTTP 405 with no message. Reload the page and try again.'
  });
  // An SPA fallback: 200, and a page of HTML.
  await assert.rejects(
    apiPostWith(answering(new Response('<!doctype html>', { status: 200 })))('/x', {}),
    { message: 'The app answered HTTP 200 with no message. Reload the page and try again.' }
  );
});

test('a fetch that throws says the request did not reach the app', async () => {
  const apiPost = apiPostWith(async () => {
    throw new TypeError('Failed to fetch');
  });
  await assert.rejects(apiPost('/x', {}), {
    message:
      'The request did not reach the app (Failed to fetch). Check your connection and try again.'
  });
});

test('the error says whether the app itself refused, and with what status', async () => {
  // `handleForgot` turns on these two: it hides what the app decided and
  // shows everything else.
  const thrown = async (stub) => apiPostWith(stub)('/x', {}).then(assert.fail, (e) => e);

  const refused = await thrown(answering(json(429, { error: 'ResourceExhausted', message: 'Slow down' })));
  assert.deepEqual([refused.status, refused.refused], [429, true]);

  const stopped = await thrown(answering(json(503, { error: 'Unavailable', code: 'runtime_stopped' })));
  assert.deepEqual([stopped.status, stopped.refused], [503, true]);

  const notJson = await thrown(answering(new Response(null, { status: 405 })));
  assert.deepEqual([notJson.status, notJson.refused], [405, false]);

  const unreached = await thrown(async () => {
    throw new TypeError('Failed to fetch');
  });
  assert.deepEqual([unreached.status, unreached.refused], [0, false]);
});

// `keepSession` — the one function that writes the session cookie. A page
// served by a service worker calls it with the login (or signup) answer,
// because a synthetic response's `Set-Cookie` is not persisted.

/// `keepSession` bound to a stub `document` and `location`.
function keepSessionWith(protocol) {
  const document = { cookie: '' };
  const keepSession = new Function('document', 'location', `${source}\nreturn keepSession;`)(
    document,
    { protocol },
  );
  return { keepSession, document };
}

test('keepSession writes the auth cookie from the answer', () => {
  const { keepSession, document } = keepSessionWith('http:');
  keepSession({ access_token: 'tok', expires_in: 900 });
  assert.equal(document.cookie, 'auth_token=tok; Path=/; SameSite=Lax; Max-Age=900');
});

test('keepSession marks the cookie Secure on https and defaults the lifetime', () => {
  const { keepSession, document } = keepSessionWith('https:');
  keepSession({ access_token: 'tok' });
  assert.equal(document.cookie, 'auth_token=tok; Path=/; SameSite=Lax; Max-Age=1800; Secure');
});

test('keepSession writes nothing for an answer that carries no token', () => {
  const { keepSession, document } = keepSessionWith('https:');
  keepSession({ email_verified: false });
  keepSession(null);
  assert.equal(document.cookie, '');
});

test('hasKeptSession sees the cookie keepSession writes, and only that one', () => {
  const kept = (cookie) =>
    new Function('document', `${source}\nreturn hasKeptSession;`)({ cookie })();
  assert.equal(kept(''), false);
  assert.equal(kept('theme=dark'), false);
  assert.equal(kept('not_auth_token=x'), false);
  assert.equal(kept('auth_token='), false, 'an emptied cookie is not a session');
  assert.equal(kept('auth_token=tok'), true);
  assert.equal(kept('theme=dark; auth_token=tok; other=1'), true);
});
