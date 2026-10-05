import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { test, expect, type Browser, type BrowserContext, type Page } from '@playwright/test';
import { serveGated, type GatedServer } from './fixtures/gated-server';
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
 *  5. "Reset" while the worker is still starting → the data is really gone
 *     afterwards; and an erase the browser refuses is said, not assumed.
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

  // The erase happens under the dead worker, which is still registered (it
  // is replaced, not unregistered). If that worker held the database open
  // the erase could not complete, and the loader would say so here.
  const eraseFailures: string[] = [];
  page.on('console', (message) => {
    if (message.text().includes('OPFS wipe failed')) eraseFailures.push(message.text());
  });
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
  expect(eraseFailures, 'every entry was removed').toEqual([]);
  // The worker serving it is the replacement, registered in place.
  expect(
    await page.evaluate(async () => (await navigator.serviceWorker.getRegistration())!.active!.scriptURL),
  ).toMatch(/\/sw\.js\?recovery=\d+$/);
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
// second tab to act would replace the worker the first one's recovery
// brought in and erase what the person has done since — here, in a tab held
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

// A tab can be under a dead worker and have been told nothing: the cause
// the worker left was for a shell that never read it, or another tab's shell
// took it. Such a tab boots as any tab does — and its probe is what finds
// the worker dead, so it still ends in a recovery that keeps the data, on
// the page it was on.
test('a tab under a dead worker that was left no cause still gets the app back, data kept', async ({
  page,
}) => {
  await boot(page);
  await plantWitness(page);

  const release = await holdLoader(page);
  const statusLines = await recordShellStatus(page);
  const cause = 'injected by recovery-wipe.spec.ts with no cause left';
  await killRuntime(page, cause);
  await page.goto(LOGIN, { waitUntil: 'commit' });
  await expect(page.locator('#status')).toHaveText('Loading...');
  // The shell is standing there, its loader not yet run; what the worker
  // left for it goes.
  expect(await page.evaluate(() => caches.delete('__impresspress_sw_stopped'))).toBe(true);
  release();

  await expect
    .poll(() => statusLines, { timeout: 60_000 })
    .toContain(`The app's runtime stopped: error handling request: Error: ${cause} — ${KEPT}`);
  expect(statusLines.filter((line) => line.endsWith(ERASED))).toEqual([]);
  await served(page);
  await expect(page.locator('input#email')).toBeVisible({ timeout: 60_000 });
  expect(new URL(page.url()).pathname).toBe(LOGIN);
  expect(await stored(page)).toEqual(expect.arrayContaining([DATABASE, WITNESS]));
});

// The same two tabs, neither held back: the death navigates both at once and
// both loaders run together. One takes the lock and registers the
// replacement; the other gets the lock while that replacement is still
// installing and the dead worker is still the registration's active one. It
// must boot onto the replacement — registering the dead worker's script URL
// there would be a registration over it, discarding it under the first tab.
test('two tabs recovering at the same moment both end in the app, on one replacement', async ({
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
  await first.reload({ waitUntil: 'commit' });
  await expect(first.locator('#status')).toHaveText('Loading...');
  await second.reload({ waitUntil: 'commit' });
  await expect(second.locator('#status')).toHaveText('Loading...');

  releaseFirst();
  releaseSecond();

  for (const page of [first, second]) {
    await served(page);
    await expect(page.locator('input#email')).toBeVisible({ timeout: 60_000 });
    expect(new URL(page.url()).pathname).toBe(LOGIN);
  }
  const lines = [...firstLines, ...secondLines];
  // One erase between them, no replacement discarded, nobody left waiting.
  expect(lines.filter((line) => line.endsWith(ERASED)).length).toBe(1);
  expect(lines.filter((line) => line.startsWith('Error:'))).toEqual([]);
  expect(lines.filter((line) => line.includes('has not answered'))).toEqual([]);
  // Both are on the same worker: the one replacement.
  const workerOf = (page: Page) =>
    page.evaluate(async () => (await navigator.serviceWorker.getRegistration())!.active!.scriptURL);
  expect(await workerOf(first)).toMatch(/\/sw\.js\?recovery=\d+$/);
  expect(await workerOf(second)).toBe(await workerOf(first));
  expect(await stored(first)).not.toContain(WITNESS);
});

// Its own host, because the job's cannot be made slow: the worker's first act
// is to fetch the wasm module, and holding that answer is a start that has
// not finished — with the worker untouched.
const SLOW_PORT = 8094;
const PKG = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../pkg');

/**
 * Bring a tab to the waiting screen with the app's data in place and its
 * worker ALIVE but unfinished — held at the fetch of its wasm module — and
 * run `body` there.
 */
async function withSlowStart(
  browser: Browser,
  body: (at: {
    page: Page;
    context: BrowserContext;
    server: GatedServer;
    statusLines: string[];
  }) => Promise<void>,
) {
  const server = await serveGated(PKG, SLOW_PORT);
  const context = await browser.newContext({ baseURL: `http://127.0.0.1:${SLOW_PORT}` });
  try {
    const page = await context.newPage();
    await boot(page);
    await plantWitness(page);

    // A cold start behind the boot shell, as on a first visit to an origin
    // that has data: no registration, the data still there.
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
    await body({ page, context, server, statusLines });
  } finally {
    await context.close();
    await server.close();
  }
}

test('a start that does not answer in time is waited for, and keeps the local data', async ({
  browser,
}) => {
  await withSlowStart(browser, async ({ page, server, statusLines }) => {
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
  });
});

// "Reset" on the waiting screen is pressed while a worker is ALIVE. Erasing
// under it would not stick — a running worker writes its own copy back, or
// holds a file the erase then cannot remove — so the reset brings the
// replacement in first and erases after. What matters is the end: the data
// is really gone, and the app starts on a database of its own making.
test('a reset while the worker is still starting ends with the local data really gone', async ({
  browser,
}) => {
  await withSlowStart(browser, async ({ page, server }) => {
    await page.getByRole('button', { name: 'Reset local data and reload' }).click();
    // The replacement is registered at once, and waits: a browser does not
    // activate a new version while the old one is still in the middle of
    // something — here, its start. Nothing has been erased under it.
    const versions = () =>
      page.evaluate(async () => {
        const registration = await navigator.serviceWorker.getRegistration();
        return {
          active: registration?.active?.scriptURL ?? null,
          coming: (registration?.waiting ?? registration?.installing)?.scriptURL ?? null,
        };
      });
    await expect.poll(async () => (await versions()).coming).toMatch(/\/sw\.js\?recovery=\d+$/);
    await page.waitForTimeout(1500);
    expect((await versions()).active).toMatch(/\/sw\.js$/);
    expect(await stored(page)).toEqual(expect.arrayContaining([DATABASE, WITNESS]));

    // The old worker's start finishes; the replacement takes over; THEN the
    // data is erased, and the app starts on the replacement.
    server.release();

    await page.waitForURL(/\/b\/auth\/login/, { timeout: 60_000 });
    await expect(page.locator('input#email')).toBeVisible();
    const after = await stored(page);
    expect(after).toContain(DATABASE);
    expect(after).not.toContain(WITNESS);
  });
});

/** Stop the registration's ACTIVE worker, and no other, as a browser stops an idle one. */
async function stopActiveWorker(page: Page) {
  const cdp = await page.context().newCDPSession(page);
  const versions = new Map<string, { versionId: string; status: string; runningStatus: string }>();
  cdp.on('ServiceWorker.workerVersionUpdated', (event: any) => {
    for (const version of event.versions) versions.set(version.versionId, version);
  });
  await cdp.send('ServiceWorker.enable');
  const active = () =>
    [...versions.values()].find((v) => v.status === 'activated' && v.runningStatus === 'running');
  await expect.poll(() => active() !== undefined).toBe(true);
  await cdp.send('ServiceWorker.stopWorker', { versionId: active()!.versionId });
  await expect.poll(() => [...versions.values()].some((v) => v.status === 'activated' && v.runningStatus === 'stopped')).toBe(true);
  await cdp.detach();
}

// A reset holds the erase lock while it waits for its replacement to
// activate, and the replacement waits for that lock before it loads the
// data. If the browser restarts the OLD worker for another tab's request in
// the meantime, that fresh instance must not wait for the lock too: the
// browser activates the replacement only once the old worker is between
// events, so the reset would wait for an activation its own lock prevents.
// The old worker, superseded, answers without loading anything; its event
// ends, the replacement activates, the reset erases, and both tabs end in the
// app.
test('a reset completes when the browser restarts the old worker for another tab meanwhile', async ({
  browser,
}) => {
  await withSlowStart(browser, async ({ page, context, server }) => {
    const versions = () =>
      page.evaluate(async () => {
        const registration = await navigator.serviceWorker.getRegistration();
        return {
          active: registration?.active?.scriptURL ?? null,
          coming: (registration?.waiting ?? registration?.installing)?.scriptURL ?? null,
        };
      });
    await page.getByRole('button', { name: 'Reset local data and reload' }).click();
    // The replacement is coming in, its runtime binary held by the host
    // like everything else that ends in `.wasm`; the reset holds the lock.
    await expect.poll(async () => (await versions()).coming).toMatch(/\/sw\.js\?recovery=\d+$/);
    // Held: the old worker's start, and the replacement's install.
    await expect.poll(() => server.held(), { timeout: 30_000 }).toBe(2);

    // The browser stops the old worker — only it: the replacement is still
    // installing — and another tab asks for a page: the old worker, still
    // the active one, starts again for it, and asks for its runtime (the
    // reset dropped the copy it kept).
    await stopActiveWorker(page);
    const other = await context.newPage();
    const otherOpened = other.goto('/b/auth/login', { waitUntil: 'commit' });
    await expect.poll(() => server.held(), { timeout: 30_000 }).toBe(3);
    // The old worker gets its runtime first, and the replacement is still
    // installing. Superseded, with the reset's erase pending, the old worker
    // answers the other tab without loading anything — the other tab's
    // navigation commits, and the old worker's event is over.
    server.releaseNewest();
    await otherOpened;
    // Then the replacement's install finishes, with nothing in its way.
    // (Released together, the old worker's last event and the end of the
    // install can coincide, and Chromium has been seen to lose an
    // activation that way — see the stopped-worker case in
    // `sw-update.spec.ts`; that is not what this test is about.)
    server.release();

    // The reset completes: the replacement took over, the data is gone,
    // the app started on a database of its own making — in both tabs.
    await page.waitForURL(/\/b\/auth\/login/, { timeout: 60_000 });
    await expect(page.locator('input#email')).toBeVisible();
    expect((await versions()).active).toMatch(/\/sw\.js\?recovery=\d+$/);
    const after = await stored(page);
    expect(after).toContain(DATABASE);
    expect(after).not.toContain(WITNESS);
    await served(other);
    await expect(other.locator('input#email')).toBeVisible({ timeout: 60_000 });
  });
});

// An erase the browser refuses — here because another tab has one of the
// files open — is not passed off as done: the screen says so, and the app
// is not entered until the person has chosen.
test('a reset whose erase cannot complete says so instead of starting the app', async ({
  browser,
}) => {
  await withSlowStart(browser, async ({ page, context, server }) => {
    // Another tab of the origin, holding the witness open for writing. (A
    // file the host serves itself, so it loads without the worker.)
    const other = await context.newPage();
    await other.goto('/manifest.json');
    await other.evaluate(async (name) => {
      const root = await navigator.storage.getDirectory();
      (window as any).__held = await (await root.getFileHandle(name)).createWritable();
    }, WITNESS);

    await page.getByRole('button', { name: 'Reset local data and reload' }).click();
    // (The replacement activates once the old worker's start has finished.)
    server.release();
    await expect(page.locator('#impresspress-stopped-title')).toHaveText(
      /local data could not be erased$/,
      { timeout: 60_000 },
    );
    await expect(page.locator('#impresspress-stopped-cause')).toHaveText(
      'The data stored locally in this browser could not be erased.',
    );
    expect(await stored(other)).toContain(WITNESS);
    expect(new URL(page.url()).pathname, 'the app was not entered').toBe('/');

    // The other tab lets go; trying again erases, and the app starts clean.
    await other.evaluate(() => (window as any).__held.abort());
    await page.getByRole('button', { name: 'Erase local data and try again' }).click();
    await page.waitForURL(/\/b\/auth\/login/, { timeout: 60_000 });
    await expect(page.locator('input#email')).toBeVisible();
    expect(await stored(page)).not.toContain(WITNESS);
  });
});
