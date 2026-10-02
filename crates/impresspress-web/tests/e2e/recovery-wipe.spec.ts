import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { test, expect, type Page } from '@playwright/test';
import { serveGated } from './fixtures/gated-server';
import {
  holdLoader,
  killRuntime,
  leftCause,
  recordShellStatus,
  served,
  stopWorkers,
  tellShell,
} from './fixtures/stopped-runtime';

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
 *  4. Two tabs are told of the one `initialize()` failure → one of them
 *     erases, once; the other joins the app the first one's recovery left,
 *     and what was written there in between is still there.
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

/**
 * Leave a database this build cannot use: not a database at all. Written
 * while no worker is running — a running one holds its own copy and would
 * write it back — so the next worker to start is the one that meets it.
 */
async function corruptDatabase(page: Page) {
  await stopWorkers(page);
  await page.evaluate(async (name) => {
    const root = await navigator.storage.getDirectory();
    const handle = await root.getFileHandle(name);
    const writable = await handle.createWritable();
    await writable.write(new TextEncoder().encode('this is not a database '.repeat(400)));
    await writable.close();
  }, DATABASE);
}

test('an initialize() failure erases the local data and the app starts clean', async ({ page }) => {
  await boot(page);
  await plantWitness(page);

  await corruptDatabase(page);

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

// Every tab of an origin shares the worker and the data, and a dead worker
// answers each of them with the shell and the cause. Uncoordinated, the
// second tab to act would unregister the worker the first one's recovery
// registered and erase what the person has done since — here, in a tab held
// back the way a throttled background tab is, well after the first is done.
test('two tabs told of one initialize() failure erase once, and what the first then writes survives the second', async ({
  context,
}) => {
  const first = await context.newPage();
  const second = await context.newPage();
  await boot(first);
  await boot(second);
  await plantWitness(first);

  const releaseFirst = await holdLoader(first);
  const releaseSecond = await holdLoader(second);
  const firstLines = await recordShellStatus(first);
  const secondLines = await recordShellStatus(second);
  await corruptDatabase(first);

  // Both tabs get the shell from the worker that failed to start, and both
  // sit there: neither loader has run.
  await first.reload({ waitUntil: 'commit' });
  await expect(first.locator('#status')).toHaveText('Loading...');
  await second.reload({ waitUntil: 'commit' });
  await expect(second.locator('#status')).toHaveText('Loading...');
  const death = await leftCause(second);
  expect(death.stage).toBe('initialize');
  expect(death.id).toMatch(/^[0-9a-f-]{36}$/);

  // The first tab recovers: erases, restarts, and the app is back.
  releaseFirst();
  await expect
    .poll(() => firstLines.filter((line) => line.endsWith(ERASED)).length, { timeout: 60_000 })
    .toBe(1);
  await served(first);
  await expect(first.locator('input#email')).toBeVisible({ timeout: 60_000 });
  expect(await stored(first)).not.toContain(WITNESS);

  // The person carries on there: data written AFTER the recovery.
  const AFTER = 'e2e-written-after-the-recovery.txt';
  await first.evaluate(async (name) => {
    const root = await navigator.storage.getDirectory();
    const writable = await (await root.getFileHandle(name, { create: true })).createWritable();
    await writable.write('what the person did after the app came back');
    await writable.close();
  }, AFTER);

  // Only now does the second tab get to run — holding the same death, as a
  // shell that was listening when the worker died holds it.
  // (What it says while it joins is replaced by its next progress line in
  // the same breath, so there is nothing on screen to wait for; what it did
  // and did not do is read below.)
  await tellShell(second, death);
  releaseSecond();
  await served(second);
  await expect(second.locator('input#email')).toBeVisible({ timeout: 60_000 });
  expect(new URL(second.url()).pathname).toBe(LOGIN);

  // Exactly one erase, by the first tab; the second erased and restarted
  // nothing — and it did act on what it was told: the breaker is consumed.
  expect(secondLines.filter((line) => line.endsWith(ERASED) || line.endsWith(KEPT))).toEqual([]);
  expect(secondLines.length, 'the second tab booted through the shell').toBeGreaterThan(0);
  expect(firstLines.filter((line) => line.endsWith(ERASED)).length).toBe(1);
  expect(await stored(first)).toEqual(expect.arrayContaining([DATABASE, AFTER]));
  // And the first tab's app is still the app: its worker answers.
  expect(await first.evaluate(async () => (await fetch('/b/auth/login')).status)).toBe(200);
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
