import { test, expect } from '@playwright/test';
import { bootServiceWorker, WELCOME_HEADING } from './fixtures/dev-sandbox';
import { killRuntime, recordShellStatus, served } from './fixtures/stopped-runtime';

/**
 * A dead runtime shows its cause on ANY static host.
 *
 * Once the wasm runtime is dead, the boot shell is the only thing that can
 * say why: `sw.js` leaves the cause for it, and `loader.js` shows it and
 * recovers. `sw.js` used to get a navigation to the shell by handing it to
 * the static host — which works where the host answers unknown paths with
 * `index.html` (Cloudflare's `not_found_handling = "single-page-application"`,
 * which only `examples/dev-sandbox/wrangler.toml` configures), and nowhere
 * else: a plain file server answers `/b/auth/login` with its own 404, the
 * shell never loads, and the person is told nothing.
 *
 * The host in this job is that plain file server (`python3 -m http.server`),
 * and the first assertion below proves it rather than assuming it. The worker
 * now answers the navigation with the shell itself, fetched from the shell's
 * own address; `tests/sw/sw_runtime_stopped.test.mjs` in `impresspress-bundle`
 * pins that against a stub host, and this is the real worker, the real wasm
 * and a real host together. `dev-workspace.spec.ts` does the same on an
 * exported bundle, whose worker is rendered with the sandbox off.
 *
 * What this cannot cover, because no worker can: a FIRST visit straight to
 * `/b/auth/login` on such a host. Nothing is registered yet, so the host's
 * 404 is the whole answer — the app starts at `/`.
 */

const CAUSE = 'injected by dev-stopped-navigation.spec.ts';
const RUNTIME_PATH = '/b/auth/login';

test('a navigation to a runtime path the runtime dies on shows the cause, with no host fallback', async ({
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
  await killRuntime(page, CAUSE);
  const answer = await page.goto(RUNTIME_PATH, { waitUntil: 'commit' });

  // The worker answered, with a document — not the host, with its 404.
  expect(answer, 'the navigation was answered').not.toBeNull();
  expect(answer!.fromServiceWorker(), 'answered by the worker').toBe(true);
  expect(answer!.status()).toBe(200);

  // The document is the boot shell, and it says what stopped the runtime.
  const cause = `error handling request: Error: ${CAUSE}`;
  await expect
    .poll(() => statusLines, { message: 'the boot shell said why it was recovering' })
    .toContain(`The app's runtime stopped: ${cause} — recovering…`);

  // And the recovery it runs lands somewhere the host can answer with no
  // worker registered — the shell's own address — so the app comes back.
  // (Recovering "in place" would load `/b/auth/login?_freshen=…` from the
  // host: the 404 again, this time with nothing left to recover from it.)
  await served(page);
  expect(new URL(page.url()).pathname).toBe('/');
  await expect(page.getByRole('heading', { name: WELCOME_HEADING })).toBeVisible();
});
