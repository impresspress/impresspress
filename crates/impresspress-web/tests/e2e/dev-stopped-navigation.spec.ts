import { test, expect, type Page } from '@playwright/test';
import { ADMIN_EMAIL, ADMIN_PASSWORD, bootServiceWorker } from './fixtures/dev-sandbox';
import { killRuntime, recordShellStatus, served } from './fixtures/stopped-runtime';

/**
 * A dead runtime shows its cause on ANY static host, and the person ends up
 * back on the page they were on.
 *
 * Once the wasm runtime is dead, the boot shell is the only thing that can
 * say why: `sw.js` leaves the cause for it, and `loader.js` shows it and
 * recovers. `sw.js` used to get a navigation to the shell by handing it to
 * the static host — which works where the host answers unknown paths with
 * `index.html` (Cloudflare's `not_found_handling = "single-page-application"`,
 * which only `examples/dev-sandbox/wrangler.toml` configures), and nowhere
 * else: a plain file server answers `/b/auth/login` with its own 404, the
 * shell never loads, and the person is told nothing. It also used to
 * unregister itself when it died, after which the host was asked for every
 * navigation, a reload of the page included.
 *
 * Now the dead worker stays registered and answers every navigation in its
 * scope with the shell, fetched from the shell's own address; the shell's
 * recovery replaces it in place — nothing is ever unregistered — and goes on,
 * on the same document, to the page that was asked for. The host in this job is that plain file server
 * (`python3 -m http.server`), and each test proves it rather than assuming
 * it. `tests/sw/` in `impresspress-bundle` pins the worker and the loader
 * against stubs; this is the real worker, the real wasm and a real host
 * together. `dev-workspace.spec.ts` does the same on an exported bundle,
 * whose worker is rendered with the sandbox off, and `recovery-wipe.spec.ts`
 * on a bundle that is allowed to erase.
 *
 * What this cannot cover, because no worker can: a FIRST visit straight to
 * `/b/auth/login` on such a host. Nothing is registered yet, so the host's
 * 404 is the whole answer — the app starts at `/`.
 */

const CAUSE = 'injected by dev-stopped-navigation.spec.ts';
const RUNTIME_PATH = '/b/auth/login';
const RESTARTING = `The app's runtime stopped: error handling request: Error: ${CAUSE} — restarting it; the data stored locally in this browser is kept…`;

/** Requests the page could not complete, from here on. */
function failedRequests(page: Page): string[] {
  const failed: string[] = [];
  page.on('requestfailed', (request) => {
    // A navigation that replaces the document aborts whatever it was loading.
    if (request.failure()?.errorText === 'net::ERR_ABORTED') return;
    failed.push(`${request.url()} ${request.failure()?.errorText}`);
  });
  page.on('response', (response) => {
    if (response.status() >= 400 && response.status() !== 503) {
      failed.push(`${response.url()} ${response.status()}`);
    }
  });
  return failed;
}

test('a navigation to a runtime path the runtime dies on shows the cause and comes back, with no host fallback', async ({
  page,
  request,
}) => {
  // The premise. `request` is Playwright's own client: no service worker is
  // in front of it, so this is what the host alone says to that path.
  expect((await request.get(RUNTIME_PATH)).status(), 'the host has no fallback').toBe(404);

  await bootServiceWorker(page);
  await page.goto(RUNTIME_PATH, { waitUntil: 'commit' });
  await expect(page.locator('input#email')).toBeVisible();

  const statusLines = await recordShellStatus(page);
  const failed = failedRequests(page);
  await killRuntime(page, CAUSE);
  const answer = await page.goto(RUNTIME_PATH, { waitUntil: 'commit' });

  // The worker answered, with a document — not the host, with its 404.
  expect(answer, 'the navigation was answered').not.toBeNull();
  expect(answer!.fromServiceWorker(), 'answered by the worker').toBe(true);
  expect(answer!.status()).toBe(200);
  // That document is the shell as the host has it: the worker adds no
  // cross-origin-isolation headers to it, as it does not for a first visit.
  expect(answer!.headers()['cross-origin-embedder-policy']).toBeUndefined();

  // The shell says what stopped the runtime and what it is doing.
  await expect
    .poll(() => statusLines, { message: 'the boot shell said why it was restarting' })
    .toContain(RESTARTING);

  // Its recovery replaces the worker under it and goes on HERE: the host is
  // never asked for this address.
  await served(page);
  expect(new URL(page.url()).pathname).toBe(RUNTIME_PATH);
  await expect(page.locator('input#email')).toBeVisible();
  // Everything the shell loaded, under the dead worker and after it, loaded.
  expect(failed).toEqual([]);
  // And the page the runtime serves again is cross-origin isolated, as every
  // page of a dev deployment is: the unisolated shell in between cost nothing.
  expect(await page.evaluate(() => window.crossOriginIsolated)).toBe(true);
});

test('after a request killed the runtime, a deep link still reaches the app', async ({
  page,
  request,
}) => {
  expect((await request.get(RUNTIME_PATH)).status(), 'the host has no fallback').toBe(404);

  // A login the runtime dies on: the page is left alone with the cause, and
  // the worker — dead — stays registered.
  await bootServiceWorker(page);
  await page.goto(RUNTIME_PATH, { waitUntil: 'commit' });
  await page.locator('input#email').fill(ADMIN_EMAIL);
  await page.locator('input#password').fill(ADMIN_PASSWORD);
  await killRuntime(page, CAUSE);
  const login = page.waitForResponse((r) => r.url().endsWith('/b/auth/api/login'));
  await page.getByRole('button', { name: /sign in/i }).click();
  expect((await login).status()).toBe(503);
  expect(
    await page.evaluate(async () => (await navigator.serviceWorker.getRegistrations()).length),
    'the dead worker is still registered',
  ).toBe(1);

  // Opening another page of the app — a path only the runtime serves — is
  // answered by that worker with the shell, which restarts and goes there.
  const statusLines = await recordShellStatus(page);
  const deep = `${RUNTIME_PATH}?redirect=%2Fb%2Fdev`;
  const answer = await page.goto(deep, { waitUntil: 'commit' });
  expect(answer!.fromServiceWorker(), 'answered by the worker').toBe(true);
  expect(answer!.status()).toBe(200);
  await expect.poll(() => statusLines).toContain(RESTARTING);

  await served(page);
  const landed = new URL(page.url());
  expect(landed.pathname + landed.search).toBe(deep);
  await expect(page.locator('input#email')).toBeVisible();
});
