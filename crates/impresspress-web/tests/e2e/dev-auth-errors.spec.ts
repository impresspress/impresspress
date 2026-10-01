import { test, expect } from '@playwright/test';
import { ADMIN_EMAIL, ADMIN_PASSWORD, bootServiceWorker } from './fixtures/dev-sandbox';

/**
 * A failed sign-in says what failed.
 *
 * The incident: an agent driving a cloud browser tried to sign in to a
 * sandbox whose wasm runtime had just died. `sw.js` handed the login `POST` to
 * the static host, which answered an empty 405; the login script's
 * `r.json()` threw; the page said "Something went wrong". The cause was in
 * the service worker's console, which neither the person nor the agent could
 * read, so there was nothing to act on and nothing to report.
 *
 * Two halves, one test each:
 *
 *  1. **The page** shows what it was told, when the answer is not the app's
 *     JSON at all. (`auth_ui/assets/test/api_post.test.mjs` pins every branch
 *     of the wording; this pins that the login page a browser is served uses
 *     it.)
 *  2. **The worker** answers a request for a dead runtime itself, with the
 *     cause, and the page shows that. (`impresspress-bundle`'s
 *     `tests/sw/sw_runtime_stopped.test.mjs` pins the worker's branches
 *     against stubs; this is the real worker, the real wasm and the real page
 *     together.)
 */

async function openLogin(page: import('@playwright/test').Page) {
  await bootServiceWorker(page);
  await page.goto('/b/auth/login', { waitUntil: 'commit' });
  await page.locator('input#email').fill(ADMIN_EMAIL);
  await page.locator('input#password').fill(ADMIN_PASSWORD);
}

test('the login page names the HTTP status of an answer that is not JSON', async ({ page }) => {
  await openLogin(page);

  // The static host's answer to the login POST, handed to the page's own
  // `fetch`. Not `page.route`: the request is answered by the service worker
  // and never reaches the network layer Playwright routes.
  await page.evaluate(() => {
    const real = window.fetch.bind(window);
    window.fetch = (input, init) =>
      String(input).endsWith('/b/auth/api/login')
        ? Promise.resolve(new Response(null, { status: 405 }))
        : real(input, init);
  });
  await page.getByRole('button', { name: /sign in/i }).click();

  const error = page.locator('#error');
  await expect(error).toBeVisible();
  await expect(error).toContainText('HTTP 405');
  await expect(error).not.toContainText('Something went wrong');
  // The form is usable again.
  await expect(page.getByRole('button', { name: /sign in/i })).toBeEnabled();
});

test('a login the runtime dies on gets the cause from the worker, and the page shows it', async ({
  page,
}) => {
  await openLogin(page);

  // Kill the runtime from outside, in the one way a test can reach: `sw.js`
  // is a module, so `handle_request` and `poisoned` are not on the worker's
  // global, but the wasm glue resolves `Headers` there at call time. Every
  // response the runtime builds constructs one, so the next request it
  // handles throws out of `handle_request` — which is exactly the catch path
  // under test. (`runtimeStopped` passes its headers as a plain object, so
  // the worker's own answer does not go through this.)
  //
  // `clients.matchAll` is emptied for one reason: `selfDestruct` re-navigates
  // every window it finds so `loader.js` can recover, and a page that is
  // being replaced cannot be asserted on. Unregistering, the console line and
  // the answer itself are untouched; the re-navigation is
  // `sw_runtime_stopped.test.mjs`'s to pin.
  const [worker] = page.context().serviceWorkers();
  expect(worker, 'the sandbox service worker').toBeTruthy();
  await worker.evaluate(() => {
    const scope = globalThis as any;
    scope.Headers = class {
      constructor() {
        throw new Error('injected by dev-auth-errors.spec.ts');
      }
    };
    scope.clients.matchAll = async () => [];
  });

  const answered = page.waitForResponse((r) => r.url().endsWith('/b/auth/api/login'));
  await page.getByRole('button', { name: /sign in/i }).click();
  const response = await answered;

  expect(response.status()).toBe(503);
  expect(response.fromServiceWorker(), 'answered by the worker, not the static host').toBe(true);
  expect(response.headers()['cache-control']).toBe('no-store');
  expect(await response.json()).toEqual({
    error: 'Unavailable',
    message:
      "The app's runtime stopped (error handling request: Error: injected by dev-auth-errors.spec.ts). Reload the page to restart it.",
    code: 'runtime_stopped',
  });

  const error = page.locator('#error');
  await expect(error).toBeVisible();
  await expect(error).toContainText("The app's runtime stopped");
  await expect(error).toContainText('injected by dev-auth-errors.spec.ts');
  await expect(error).toContainText('Reload the page');

  // Poisoned: the next request gets the same answer, with the first cause,
  // without the runtime being asked again.
  const again = await page.evaluate(async () => {
    const r = await fetch('/b/auth/api/signup', { method: 'POST', body: '{}' });
    return { status: r.status, body: await r.json() };
  });
  expect(again.status).toBe(503);
  expect(again.body.code).toBe('runtime_stopped');
  expect(again.body.message).toContain('injected by dev-auth-errors.spec.ts');
});
