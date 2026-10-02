import { test, expect, type Page } from '@playwright/test';
import {
  ADMIN_EMAIL,
  ADMIN_PASSWORD,
  bootServiceWorker,
  WELCOME_HEADING,
} from './fixtures/dev-sandbox';
import { killRuntime as kill, recordShellStatus, served } from './fixtures/stopped-runtime';

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
 * Three claims, one test each:
 *
 *  1. **The page** shows what it was told, when the answer is not the app's
 *     JSON at all. (`auth_ui/assets/test/api_post.test.mjs` pins every branch
 *     of the wording; this pins that the login page a browser is served uses
 *     it.)
 *  2. **A request the runtime dies on** is answered by the worker itself,
 *     with the cause — and the page that asked is LEFT ALONE, so the cause is
 *     still on screen seconds later and after a second try. The first version
 *     of this fix returned the answer and re-navigated the page in the same
 *     breath; the reloaded page was a clean login form that said nothing.
 *  3. **A navigation the runtime dies on** lands on the boot shell, which
 *     shows the cause and recovers once on its own; if it fails again before
 *     the app has answered, the shell stops with the cause on screen until
 *     the person chooses what to do.
 *
 * (`impresspress-bundle`'s `tests/sw/sw_runtime_stopped.test.mjs` pins the
 * worker's branches against stubs; these are the real worker, the real wasm
 * and the real pages together. Nothing in the worker is stubbed here except
 * the one thing that kills the runtime.)
 */

const CAUSE = 'injected by dev-auth-errors.spec.ts';
const killRuntime = (page: Page) => kill(page, CAUSE);

async function openLogin(page: Page) {
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

test('a login the runtime dies on shows the cause, and keeps showing it', async ({ page }) => {
  await openLogin(page);
  await killRuntime(page);

  const signIn = page.getByRole('button', { name: /sign in/i });
  const answered = page.waitForResponse((r) => r.url().endsWith('/b/auth/api/login'));
  await signIn.click();
  const response = await answered;

  const cause = `error handling request: Error: ${CAUSE}`;
  // What the worker says to do next names the boot shell's address.
  const stopped = `The app's runtime stopped (${cause}). Open ${new URL('/', page.url()).href} to restart it.`;
  expect(response.status()).toBe(503);
  expect(response.fromServiceWorker(), 'answered by the worker, not the static host').toBe(true);
  expect(response.headers()['cache-control']).toBe('no-store');
  expect(await response.json()).toEqual({
    error: 'Unavailable',
    message: stopped,
    code: 'runtime_stopped',
    cause,
  });

  const error = page.locator('#error');
  const shown = stopped;
  await expect(error).toBeVisible();
  await expect(error).toHaveText(shown);

  // What the person sees a moment later is the point: the page was not
  // re-navigated out from under the message. Same document, same text.
  await page.evaluate(() => {
    (window as any).__sameDocument = true;
  });
  await page.waitForTimeout(2500);
  expect(await page.evaluate(() => (window as any).__sameDocument)).toBe(true);
  expect(new URL(page.url()).pathname).toBe('/b/auth/login');
  await expect(error).toHaveText(shown);

  // A second try — what an agent does next — says the same thing, from the
  // poisoned worker, with the FIRST cause.
  await expect(signIn).toBeEnabled();
  const again = page.waitForResponse((r) => r.url().endsWith('/b/auth/api/login'));
  await signIn.click();
  expect((await again).status()).toBe(503);
  await expect(error).toHaveText(shown);
  await page.waitForTimeout(1000);
  expect(await page.evaluate(() => (window as any).__sameDocument)).toBe(true);
  await expect(error).toHaveText(shown);

  // The forgot-password link does not claim an email was sent, either.
  await page.getByRole('button', { name: /forgot password/i }).click();
  await expect(error).toHaveText(shown);
  await expect(page.locator('#info')).toBeHidden();

  // And the message's last sentence is true: opening the address it names is
  // an ordinary boot with a fresh worker — no recovery ran, so nothing was
  // wiped. (It names `/` and not "reload the page" because the worker has
  // unregistered, and the static host in this job — like any with no
  // fallback — answers `/b/auth/login` with its own 404.)
  const statusLines = await recordShellStatus(page);
  await page.goto('/', { waitUntil: 'commit' });
  await served(page);
  await expect(page.getByRole('heading', { name: WELCOME_HEADING })).toBeVisible();
  expect(statusLines.length, 'the boot shell was loaded and booted').toBeGreaterThan(0);
  expect(statusLines.filter((line) => line.includes('runtime stopped'))).toEqual([]);
});

test('a navigation the runtime dies on lands on a boot shell that shows the cause and stops', async ({
  page,
}) => {
  await bootServiceWorker(page);
  const heading = page.getByRole('heading', { name: WELCOME_HEADING });
  await expect(heading).toBeVisible();

  // First failure in this tab. The worker answers the navigation with the
  // boot shell, which reads the cause the worker left and recovers by
  // itself — once: drop the worker and the caches, register afresh.
  const statusLines = await recordShellStatus(page);
  await killRuntime(page);
  await page.reload({ waitUntil: 'commit' });
  const cause = `error handling request: Error: ${CAUSE}`;
  await expect
    .poll(() => statusLines, { message: 'the boot shell said why it was recovering' })
    .toContain(`The app's runtime stopped: ${cause} — recovering…`);
  await served(page);
  await expect(heading).toBeVisible();

  // The app answered the boot probe, so that recovery is over and done. The
  // flag the recovery set is gone — read here from a runtime page, which sees
  // what the shell wrote: the marker proves that, on this very load.
  expect(
    await page.evaluate(() => ({
      marker: sessionStorage.getItem('__e2e_written_by_shell'),
      recoveryDone: sessionStorage.getItem('__impresspress_recovery_done'),
    })),
  ).toEqual({ marker: 'yes', recoveryDone: null });
  // So a later failure in the same tab is recovered from automatically
  // again, instead of going straight to the stuck screen.
  const recovering = `The app's runtime stopped: ${cause} — recovering…`;
  await killRuntime(page);
  await page.reload({ waitUntil: 'commit' });
  await expect
    .poll(() => statusLines.filter((line) => line === recovering).length)
    .toBe(2);
  await served(page);
  await expect(heading).toBeVisible();

  // A failure BEFORE the app has answered again: the automatic recovery has
  // been spent, so the shell shows the cause and waits. That state cannot be
  // staged for real here — it needs a worker that dies on its very first
  // request, and a test can only reach a worker after it has served one — so
  // the loader's own flag is set, in the shell, before the loader runs.
  // (`loader_recovery.test.mjs` drives the real sequence against the rendered
  // loader; what only a browser can show is the screen itself.)
  await page.addInitScript(() => {
    if (new URLSearchParams(location.search).has('recovery-spent')) {
      sessionStorage.setItem('__impresspress_recovery_done', '1');
    }
  });
  await killRuntime(page);
  await page.goto('/?recovery-spent', { waitUntil: 'commit' });

  const stopped = page.locator('#impresspress-stopped-cause');
  await expect(stopped).toHaveText(`The app's runtime stopped: ${cause}`, { timeout: 60_000 });
  const retry = page.getByRole('button', { name: 'Try again' });
  await expect(retry).toBeVisible();
  await expect(page.getByRole('button', { name: 'Reset local data and reload' })).toBeVisible();

  // It stays: no reload, no redirect, the same document and the same text.
  await page.evaluate(() => {
    (window as any).__sameDocument = true;
  });
  await page.waitForTimeout(3000);
  expect(await page.evaluate(() => (window as any).__sameDocument)).toBe(true);
  await expect(stopped).toHaveText(`The app's runtime stopped: ${cause}`);

  // "Try again" is a real way out: nothing is wrong with a fresh worker.
  await retry.click();
  await served(page);
  await expect(heading).toBeVisible();
});
