// Run with: node --test crates/impresspress-core/src/blocks/dev/assets/test/dev_enter.test.mjs
//
// The `/b/dev/enter` page's script, composed the way `blocks/dev/enter.rs`
// emits it: the auth forms' helpers (`auth_ui/assets/api_post.js`) first,
// then `enter.js`. What these pin is that the entry page signs in through
// the forms' own request path and cookie writer, uses a session that already
// exists, and — when it cannot sign in — says why.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const helpers = fs.readFileSync(
  path.join(here, '..', '..', '..', 'auth_ui', 'assets', 'api_post.js'),
  'utf8'
);
const enter = fs.readFileSync(path.join(here, '..', 'enter.js'), 'utf8');

const json = (status, body) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

/**
 * Run the page once.
 *
 * @param {(url: string, init?: object) => Promise<Response>} fetch
 * @param {{cookie?: string}} [options]  the document's cookies as the page
 *   loads — `auth_token=…` for a visitor who was signed in before.
 * @returns the page as the script left it, once every promise has settled.
 */
async function run(fetch, { cookie = '' } = {}) {
  const attributes = {
    'data-login': '/b/auth/api/login',
    'data-session-probe': '/b/dev/api/status',
    'data-email': 'owner@example.com',
    'data-password': 'seeded',
    'data-workspace': '/b/dev'
  };
  const elements = {
    'dev-enter': { getAttribute: (name) => attributes[name] },
    'dev-enter-status': { textContent: 'Signing in…' },
    // Ships `hidden` (`enter.rs`).
    'dev-enter-fallback': { hidden: true }
  };
  const calls = [];
  const replaced = [];
  const document = { cookie, getElementById: (id) => elements[id] };
  const location = { protocol: 'https:', replace: (url) => replaced.push(url) };
  new Function('document', 'location', 'fetch', `${helpers}\n${enter}`)(
    document,
    location,
    (url, init) => {
      calls.push({ url: String(url), method: (init && init.method) || 'GET', init });
      return fetch(String(url), init);
    }
  );
  for (let i = 0; i < 20; i += 1) {
    await new Promise((resolve) => setImmediate(resolve));
  }
  return {
    calls,
    replaced,
    cookie: document.cookie,
    status: elements['dev-enter-status'].textContent,
    fallbackHidden: elements['dev-enter-fallback'].hidden
  };
}

test('a first-time visitor is signed in through the login endpoint, with no probe', async () => {
  const page = await run(async () => json(200, { access_token: 'tok', expires_in: 600 }));

  // One request, and it is the sign-in: with no kept session there is
  // nothing to probe, and a probe would be a refused request in the console.
  assert.deepEqual(
    page.calls.map((call) => `${call.method} ${call.url}`),
    ['POST /b/auth/api/login']
  );
  // The request the login form makes, through `apiPost`.
  assert.equal(page.calls[0].init.headers['Content-Type'], 'application/json');
  assert.equal(page.calls[0].init.body, '{"email":"owner@example.com","password":"seeded"}');
  // The cookie `keepSession` writes — the login page's, attribute for attribute.
  assert.equal(page.cookie, 'auth_token=tok; Path=/; SameSite=Lax; Max-Age=600; Secure');
  assert.deepEqual(page.replaced, ['/b/dev']);
  assert.equal(page.fallbackHidden, true);
});

test('an admin session that already exists is used, not replaced', async () => {
  const page = await run(async () => json(200, { runtime_generation: 1 }), {
    cookie: 'theme=dark; auth_token=mine'
  });

  assert.deepEqual(
    page.calls.map((call) => `${call.method} ${call.url}`),
    ['GET /b/dev/api/status']
  );
  assert.equal(page.cookie, 'theme=dark; auth_token=mine', 'the session it came with is untouched');
  assert.deepEqual(page.replaced, ['/b/dev']);
});

test('a kept session the app no longer accepts is replaced by signing in', async () => {
  const page = await run(
    async (url) =>
      url === '/b/dev/api/status'
        ? json(401, { error: 'Unauthenticated', message: 'sign in' })
        : json(200, { access_token: 'fresh', expires_in: 600 }),
    { cookie: 'auth_token=expired' }
  );

  assert.deepEqual(
    page.calls.map((call) => `${call.method} ${call.url}`),
    ['GET /b/dev/api/status', 'POST /b/auth/api/login']
  );
  assert.equal(page.cookie, 'auth_token=fresh; Path=/; SameSite=Lax; Max-Age=600; Secure');
  assert.deepEqual(page.replaced, ['/b/dev']);
});

test('a refused password says one-click entry is off and offers the login page', async () => {
  // No kept cookie, so the only request is the sign-in.
  const page = await run(async () =>
    json(401, { error: 'Unauthenticated', message: 'Invalid email or password' })
  );

  assert.match(page.status, /^One-click entry is off for this sandbox: the admin password was changed/);
  assert.equal(page.fallbackHidden, false);
  assert.deepEqual(page.replaced, []);
  assert.equal(page.cookie, '');
});

test('any other refusal is shown in the app’s own words', async () => {
  const page = await run(async () =>
    json(429, { error: 'ResourceExhausted', message: 'Too many attempts. Try again later.' })
  );

  assert.equal(
    page.status,
    'Could not sign in automatically: Too many attempts. Try again later.'
  );
  assert.equal(page.fallbackHidden, false);
  assert.deepEqual(page.replaced, []);
});

test('a runtime that has stopped shows its cause, as the login form does', async () => {
  // What the service worker answers once its runtime is gone
  // (`impresspress-bundle`'s `sw.js.tmpl`): a 503 in the runtime's error
  // shape whose message carries the cause.
  const message =
    "The app's runtime stopped (error handling request: RuntimeError: unreachable). Open https://app.example/ to restart it.";
  const page = await run(async () =>
    json(503, { error: 'Unavailable', message, code: 'runtime_stopped' })
  );

  // A first-time visitor: no probe is sent, and the sign-in's own answer is
  // what the page reports.
  assert.deepEqual(
    page.calls.map((call) => `${call.method} ${call.url}`),
    ['POST /b/auth/api/login']
  );
  assert.equal(page.status, 'Could not sign in automatically: ' + message);
  assert.equal(page.fallbackHidden, false);
  assert.deepEqual(page.replaced, []);
});

test('a request that never reaches the app says so, with the browser’s reason', async () => {
  const page = await run(async () => {
    throw new TypeError('Failed to fetch');
  });

  assert.match(
    page.status,
    /^Could not sign in automatically: The request did not reach the app \(Failed to fetch\)\./
  );
  assert.equal(page.fallbackHidden, false);
});

test('an answer that carries no session is a failure, not an entry', async () => {
  const page = await run(async () => json(200, {}));

  assert.match(page.status, /^Could not sign in automatically: the app answered without a session/);
  assert.deepEqual(page.replaced, []);
  assert.equal(page.cookie, '');
});

// ---- the probe, for a visitor who arrives with a kept session --------------
//
// Only a 200 from the probe means "signed in". Every other outcome falls
// through to the sign-in, and when that fails too, its failure is what the
// page reports — the probe's own answer is never shown and never trusted.

const BOTH = ['GET /b/dev/api/status', 'POST /b/auth/api/login'];
const requests = (page) => page.calls.map((call) => `${call.method} ${call.url}`);

test('a kept session and a stopped runtime: the probe’s 503 is not an entry, the sign-in reports the cause', async () => {
  const message =
    "The app's runtime stopped (error handling request: RuntimeError: unreachable). Open https://app.example/ to restart it.";
  const page = await run(
    async () => json(503, { error: 'Unavailable', message, code: 'runtime_stopped' }),
    { cookie: 'auth_token=x' }
  );

  // The probe WAS sent, its 503 was read as "not signed in", and the sign-in
  // that followed is what reported the cause.
  assert.deepEqual(requests(page), BOTH);
  assert.equal(page.status, 'Could not sign in automatically: ' + message);
  assert.equal(page.fallbackHidden, false);
  assert.deepEqual(page.replaced, [], 'a 503 must never open the workspace');
  assert.equal(page.cookie, 'auth_token=x');
});

test('a kept session and an unreachable app: the probe’s network error is not an entry either', async () => {
  const page = await run(
    async () => {
      throw new TypeError('Failed to fetch');
    },
    { cookie: 'auth_token=x' }
  );

  assert.deepEqual(requests(page), BOTH);
  assert.match(
    page.status,
    /^Could not sign in automatically: The request did not reach the app \(Failed to fetch\)\./
  );
  assert.equal(page.fallbackHidden, false);
  assert.deepEqual(page.replaced, []);
});

test('no probe answer but 200 counts as signed in', async () => {
  // Each of these is an answer a kept cookie can meet: the session expired,
  // it lost the admin role, the route is gone (an older runtime), the app is
  // down, or something in front of it answered a 2xx/3xx that is not the
  // status endpoint's 200.
  for (const status of [204, 302, 401, 403, 404, 500, 503]) {
    const page = await run(
      async (url) =>
        url === '/b/dev/api/status'
          ? new Response(null, { status })
          : json(401, { error: 'Unauthenticated', message: 'Invalid email or password' }),
      { cookie: 'auth_token=x' }
    );

    assert.deepEqual(requests(page), BOTH, `probe ${status}`);
    assert.deepEqual(page.replaced, [], `a probe answering ${status} must not open the workspace`);
    assert.match(page.status, /^One-click entry is off/, `probe ${status}`);
  }
});
