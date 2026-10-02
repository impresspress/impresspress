import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { test, expect, type Page } from '@playwright/test';
import { serveGated } from './fixtures/gated-server';
import { killRuntime, recordShellStatus, served, stopWorkers } from './fixtures/stopped-runtime';

/**
 * What a recovery does to the data this browser stores for the app, in a
 * bundle that is ALLOWED to erase it.
 *
 * `impresspress-web` is built with `opfs_wipe_on_recovery = true`
 * (`impresspress.toml`): a demo whose local data is throwaway, so that a
 * database an older build left behind does not trap a visitor in a boot that
 * can never succeed. That permission is the dangerous half of the recovery,
 * and the rule for using it is narrow: **only a failure of the runtime's
 * `initialize()` erases** — the one failure that can mean the stored data is
 * not what this build can use. Everything else that goes wrong keeps it:
 *
 *  1. `initialize()` fails on the stored database → the boot shell says so,
 *     erases, and the app starts clean on the page the person was on.
 *  2. A started runtime dies on a navigation → the shell says why and
 *     replaces the worker; the data is still there.
 *  3. The app does not answer in time → nothing is restarted and nothing is
 *     erased; the shell waits, then asks, and "Keep waiting" is how a slow
 *     start gets to finish.
 *
 * `impresspress-bundle`'s `tests/sw/loader_recovery.test.mjs` drives every
 * road through the rendered loader against stubs. This is the real worker,
 * the real wasm and a real OPFS, on a static host with no fallback for the
 * paths only the runtime serves — which is what this job's
 * `python3 -m http.server` is.
 *
 * The witness is a file of the test's own beside the app's database in OPFS.
 * The recovery's wipe removes every entry of the origin's root, so the
 * witness is gone exactly when the data is.
 */

const WITNESS = 'e2e-recovery-witness.txt';
const DATABASE = 'impresspress.db';
const LOGIN = '/b/auth/login';
const KEPT = 'restarting it; the data stored locally in this browser is kept…';
const ERASED = 'recovering; the data stored locally in this browser is being erased…';

/** Load the app and wait for the page its boot lands on. */
async function boot(page: Page) {
  await page.goto('/', { waitUntil: 'commit' });
  await page.waitForURL(/\/b\/auth\/login/, { timeout: 60_000 });
  await expect(page.locator('input#email')).toBeVisible();
}

/** Put the witness beside the app's database. */
async function plantWitness(page: Page) {
  await page.evaluate(async (name) => {
    const root = await navigator.storage.getDirectory();
    const handle = await root.getFileHandle(name, { create: true });
    const writable = await handle.createWritable();
    await writable.write('data the recovery must not touch unless it erases');
    await writable.close();
  }, WITNESS);
  expect(await stored(page)).toEqual(expect.arrayContaining([DATABASE, WITNESS]));
}

/** The names in the origin's OPFS root. */
async function stored(page: Page): Promise<string[]> {
  return page.evaluate(async () => {
    const root = await navigator.storage.getDirectory();
    const names: string[] = [];
    for await (const [name] of (root as any).entries()) names.push(name);
    return names.sort();
  });
}

test('an initialize() failure erases the local data and the app starts clean', async ({ page }) => {
  await boot(page);
  await plantWitness(page);

  // A database this build cannot use: not a database at all. Written while
  // no worker is running — a running one holds its own copy and would write
  // it back — so the next worker to start is the one that meets it.
  await stopWorkers(page);
  await page.evaluate(async (name) => {
    const root = await navigator.storage.getDirectory();
    const handle = await root.getFileHandle(name);
    const writable = await handle.createWritable();
    await writable.write(new TextEncoder().encode('this is not a database '.repeat(400)));
    await writable.close();
  }, DATABASE);

  const statusLines = await recordShellStatus(page);
  await page.reload({ waitUntil: 'commit' });

  // The shell says what failed and what it is doing about it…
  await expect
    .poll(() => statusLines.find((line) => line.endsWith(ERASED)) ?? null, {
      message: 'the boot shell said it was erasing',
      timeout: 60_000,
    })
    .toContain("The app's runtime stopped: runtime initialize() failed:");
  expect(statusLines.filter((line) => line.endsWith(KEPT))).toEqual([]);

  // …and the app comes back, on the page the person was on, with a database
  // of its own making and without the witness.
  await served(page);
  await expect(page.locator('input#email')).toBeVisible({ timeout: 60_000 });
  expect(new URL(page.url()).pathname).toBe(LOGIN);
  const after = await stored(page);
  expect(after).toContain(DATABASE);
  expect(after).not.toContain(WITNESS);
});

test('a navigation the runtime dies on shows the cause and keeps the local data', async ({
  page,
  request,
}) => {
  // The host alone has nothing at this path: only a registered worker can
  // answer a navigation to it.
  expect((await request.get(LOGIN)).status(), 'the host has no fallback').toBe(404);

  await boot(page);
  await plantWitness(page);

  const cause = 'injected by recovery-wipe.spec.ts';
  const statusLines = await recordShellStatus(page);
  await killRuntime(page, cause);
  const answer = await page.goto(LOGIN, { waitUntil: 'commit' });
  expect(answer!.fromServiceWorker(), 'answered by the worker').toBe(true);
  expect(answer!.status()).toBe(200);

  await expect
    .poll(() => statusLines, { message: 'the boot shell said why, and that the data is kept' })
    .toContain(`The app's runtime stopped: error handling request: Error: ${cause} — ${KEPT}`);
  expect(statusLines.filter((line) => line.endsWith(ERASED))).toEqual([]);

  await served(page);
  await expect(page.locator('input#email')).toBeVisible({ timeout: 60_000 });
  expect(new URL(page.url()).pathname).toBe(LOGIN);
  expect(await stored(page)).toEqual(expect.arrayContaining([DATABASE, WITNESS]));
});

// Its own host, because the job's cannot be made slow: the worker's first act
// is to fetch the wasm module, and holding that answer is a start that has
// not finished — with the worker untouched.
const SLOW_PORT = 8094;
const PKG = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../pkg');

test('a start that does not answer in time is waited for, and keeps the local data', async ({
  browser,
}) => {
  const server = await serveGated(PKG, SLOW_PORT);
  const context = await browser.newContext({ baseURL: `http://127.0.0.1:${SLOW_PORT}` });
  try {
    const page = await context.newPage();
    await boot(page);
    await plantWitness(page);

    // A cold start behind the boot shell, as on a later visit after the
    // browser has dropped the worker: no registration, the data still there.
    await page.evaluate(async () => {
      for (const registration of await navigator.serviceWorker.getRegistrations()) {
        await registration.unregister();
      }
    });
    await stopWorkers(page);

    // The page's clock is the test's from here: the loader's 60 s timer fires
    // when the test says a minute has passed. Only the page's — the worker
    // and the host are real, and the start really is unfinished.
    await page.clock.install();
    const statusLines = await recordShellStatus(page);
    server.hold('.wasm');
    await page.goto('/', { waitUntil: 'commit' });
    await expect.poll(() => server.held(), { timeout: 60_000 }).toBeGreaterThan(0);
    await expect(page.locator('#status')).toHaveText(/^Loading /);

    // One minute: the shell says it is still waiting, and does nothing else.
    await page.clock.fastForward(60_000);
    await expect(page.locator('#status')).toHaveText(
      'The app has not answered for 60 seconds. Still waiting — it may only be slow…',
    );
    // Two: it asks.
    await page.clock.fastForward(60_000);
    await expect(page.locator('#impresspress-stopped-title')).toHaveText(
      /is taking a long time to start$/,
    );
    await expect(page.locator('#impresspress-stopped-cause')).toHaveText(
      'The app has not answered for 120 seconds.',
    );
    const keepWaiting = page.getByRole('button', { name: 'Keep waiting' });
    await expect(keepWaiting).toBeVisible();
    await expect(page.getByRole('button', { name: 'Restart it' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Reset local data and reload' })).toBeVisible();

    // Nothing was restarted and nothing erased: the worker that is starting
    // is still the registered one, still waiting for its module, and the
    // data is where it was.
    expect(server.held()).toBe(1);
    expect(await page.evaluate(async () => (await navigator.serviceWorker.getRegistrations()).length)).toBe(1);
    expect(await stored(page)).toEqual(expect.arrayContaining([DATABASE, WITNESS]));
    expect(statusLines.filter((line) => line.includes('runtime stopped'))).toEqual([]);

    // The start finishes, and "Keep waiting" is what lets it: the same
    // worker answers the next probe and the app loads, data intact.
    server.release();
    await keepWaiting.click();
    await page.waitForURL(/\/b\/auth\/login/, { timeout: 60_000 });
    await expect(page.locator('input#email')).toBeVisible();
    expect(await stored(page)).toEqual(expect.arrayContaining([DATABASE, WITNESS]));
  } finally {
    await context.close();
    await server.close();
  }
});
