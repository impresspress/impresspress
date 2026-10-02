import { test, expect, type Page, type Worker } from '@playwright/test';
import { once } from 'node:events';
import { type ChildProcess } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import {
  bootServiceWorker,
  enterFromWelcome,
  runFromConsole,
  serveDirectory,
  SW_UPDATE_PORT,
} from './fixtures/dev-sandbox';

/**
 * A new deployment replaces the one a browser already has — and the browser
 * keeps what it stored.
 *
 * Every deploy relies on this and nothing else in the suite runs it: each
 * other spec starts from an origin that has never had a service worker. A
 * returning visitor has one, with the previous build's runtime loaded into
 * it, and nothing on the page they are looking at knows a newer build exists
 * (`loader.js`, which asks for an update, is only on the boot shell — a page
 * the runtime renders does not carry it). What moves them on is the browser's
 * own update check on a navigation, plus one line of `sw.js`: `skipWaiting()`
 * in `install`. Without it the new worker sits in `waiting` for as long as
 * any tab of the site stays open, through every reload, and the visitor stays
 * on the old runtime. With it the new worker activates at once and the
 * browser hands it the pages the old one controlled. (`clients.claim()` in
 * `activate` is not part of this: it is for a page NO worker controls yet —
 * the first visit, which every other sandbox spec starts with. Removing it
 * leaves this test passing, and removing `skipWaiting()` fails it.)
 *
 * Three bundles, all built by `examples/dev-sandbox/build.sh`:
 *
 *  - `DEV_DIST` — the blank seed's bundle, the first deployment;
 *  - `UPDATE_DIST` — the same seed assembled from the next build of the
 *    runtime (`--pkg-dir`, over `fixtures/next-runtime-pkg.mjs`'s copy of
 *    `pkg-dev/`), the second deployment;
 *  - `BOOTSTRAP_DIST` — another seed built by a separate run from the same
 *    source, which is what "a rebuild that changed nothing" looks like.
 *
 * A deployment is the static host starting to answer with another directory:
 * the server on `SW_UPDATE_PORT` is stopped and started again on the next
 * bundle, so the origin — and with it the registration, the cookies and OPFS
 * — stays the browser's own.
 */
function bundle(variable: string, how: string): string {
  const dir = process.env[variable];
  if (!dir) {
    throw new Error(`${variable} is not set — ${how}, and point ${variable} at the directory it prints`);
  }
  return dir;
}

const DEV_DIST = bundle('DEV_DIST', 'build the first deployment with `examples/dev-sandbox/build.sh`');
const UPDATE_DIST = bundle(
  'UPDATE_DIST',
  'build the second deployment with `node crates/impresspress-web/tests/e2e/fixtures/next-runtime-pkg.mjs ' +
    'crates/impresspress-web/pkg-dev <pkg>` and `examples/dev-sandbox/build.sh --pkg-dir <pkg> --out <dir>`',
);
const BOOTSTRAP_DIST = bundle(
  'BOOTSTRAP_DIST',
  'build the seed with `examples/dev-sandbox/build.sh --seed bootstrap --out <dir>`',
);

const ORIGIN = `http://127.0.0.1:${SW_UPDATE_PORT}`;

/** The worker script a bundle ships — what the browser compares, byte for byte. */
const workerScript = (dist: string) => readFileSync(path.join(dist, 'sw.js'), 'utf8');

/** The URL path of the runtime binary a bundle's worker loads. */
function runtimeOf(dist: string): string {
  const manifest = JSON.parse(readFileSync(path.join(dist, 'asset-manifest.json'), 'utf8'));
  const hashed: string = manifest.assets['impresspress_web_bg.wasm'];
  expect(hashed).toMatch(/^\/impresspress_web_bg-[0-9a-f]+\.wasm$/);
  return hashed;
}

/** Stop a static host and wait until it is gone, so the port is free for the next. */
async function stop(server: ChildProcess) {
  if (server.exitCode === null && server.signalCode === null) {
    const gone = once(server, 'exit');
    server.kill('SIGKILL');
    await gone;
  }
}

/**
 * Every URL a service worker fetched for itself, read inside that worker.
 *
 * The runtime is loaded by the worker (`init()` fetches the hashed `.wasm`),
 * not by any page, so the page's own network log never names it. A worker
 * that has not handled a request yet has loaded none: `sw.js` initialises on
 * its first fetch.
 */
const fetchedBy = (worker: Worker) =>
  worker.evaluate(() => performance.getEntriesByType('resource').map((entry) => new URL(entry.name).pathname));

/** The page's registration, as states — `null` where there is no such worker. */
const registrationOf = (page: Page) =>
  page.evaluate(async () => {
    const registration = await navigator.serviceWorker.getRegistration();
    return {
      installing: registration?.installing?.state ?? null,
      waiting: registration?.waiting?.state ?? null,
      active: registration?.active?.state ?? null,
      // The same worker, not merely a worker: the page is answered by the
      // registration's active worker, not by one it has since replaced.
      activeControlsThisPage:
        !!registration?.active && navigator.serviceWorker.controller === registration.active,
    };
  });

const SETTLED = { installing: null, waiting: null, active: 'activated', activeControlsThisPage: true };

/** `GET path` from the page, so through whichever worker controls it. */
const viaWorker = (page: Page, urlPath: string) =>
  page.evaluate(async (target) => {
    const response = await fetch(target, { cache: 'no-store' });
    return { status: response.status, text: await response.text() };
  }, urlPath);

test('a rebuild that changed nothing ships the same worker, and a new runtime ships a new one', () => {
  // Two runs of the build over one source tree: the browser compares `sw.js`
  // byte for byte, so identical here is what "no spurious update" means.
  expect(workerScript(BOOTSTRAP_DIST)).toBe(workerScript(DEV_DIST));
  expect(runtimeOf(BOOTSTRAP_DIST)).toBe(runtimeOf(DEV_DIST));
  // …and a different runtime binary is a different hashed name, which is a
  // different worker script: the only thing that makes the browser install.
  expect(runtimeOf(UPDATE_DIST)).not.toBe(runtimeOf(DEV_DIST));
  expect(workerScript(UPDATE_DIST)).not.toBe(workerScript(DEV_DIST));
});

test('a returning browser moves to the new deployment’s worker and runtime, with its data', async ({
  browser,
  request,
}) => {
  test.setTimeout(420_000);
  const first = runtimeOf(DEV_DIST);
  const next = runtimeOf(UPDATE_DIST);

  let server = await serveDirectory(DEV_DIST, SW_UPDATE_PORT);
  const context = await browser.newContext({ baseURL: ORIGIN });
  // Every service worker this browser starts for the origin, in order.
  const workers: Worker[] = [];
  context.on('serviceworker', (worker) => workers.push(worker));
  try {
    const page = await context.newPage();

    // ---- the first deployment, used --------------------------------------
    await bootServiceWorker(page);
    expect(workers).toHaveLength(1);
    expect(await fetchedBy(workers[0])).toContain(first);
    expect(await registrationOf(page)).toEqual(SETTLED);

    // Something only this browser has: a page written into the workspace and
    // published, both of which live in OPFS.
    const KEPT = '<!doctype html><title>kept</title><p>written before the update</p>\n';
    await enterFromWelcome(page);
    const written = await runFromConsole(page, 'dev_write_file', { path: 'site/kept.html', content: KEPT });
    expect(written.isError, JSON.stringify(written)).toBe(false);
    expect(await viaWorker(page, '/kept.html')).toEqual({ status: 200, text: KEPT });

    // ---- the same build, deployed again -----------------------------------
    // The host restarts on the same files. The browser checks, finds the
    // worker it already has, and installs nothing.
    await stop(server);
    server = await serveDirectory(DEV_DIST, SW_UPDATE_PORT);
    await page.goto('/', { waitUntil: 'load' });
    await page.evaluate(async () => {
      const registration = await navigator.serviceWorker.getRegistration();
      await registration!.update();
    });
    expect(await registrationOf(page)).toEqual(SETTLED);
    expect(workers).toHaveLength(1);

    // ---- the next deployment ----------------------------------------------
    await stop(server);
    server = await serveDirectory(UPDATE_DIST, SW_UPDATE_PORT);
    // As after any deploy, the previous runtime's file is no longer served:
    // nothing from here on can be loading it.
    expect((await request.get(`${ORIGIN}${first}`)).status()).toBe(404);
    expect((await request.get(`${ORIGIN}${next}`)).status()).toBe(200);

    // A visit, and nothing else: no `update()` call, no boot shell. The page
    // is answered by the worker the browser already had; the browser's own
    // check after the navigation is what finds the new script.
    await page.goto('/', { waitUntil: 'load' });
    await expect.poll(() => workers.length, { timeout: 120_000 }).toBe(2);
    // The new worker did not wait for this tab to close (`skipWaiting`): it
    // is the active one, nothing is left waiting, and the open page is its.
    await expect.poll(() => registrationOf(page), { timeout: 60_000 }).toEqual(SETTLED);

    // ---- the site, on the new runtime ---------------------------------------
    // The next request is the new worker's first, so this is where it loads
    // its runtime — the new deployment's, over the storage the old one left.
    expect(await viaWorker(page, '/kept.html')).toEqual({ status: 200, text: KEPT });
    const fetched = await fetchedBy(workers[1]);
    expect(fetched).toContain(next);
    expect(fetched).not.toContain(first);

    // Still the same person's sandbox: the session is kept, the workspace has
    // the file, and a write through the new runtime is published.
    await page.goto('/b/dev', { waitUntil: 'commit' });
    await expect(page.locator('#dev-progress-steps li').first()).toBeAttached({ timeout: 60_000 });
    const read = await runFromConsole(page, 'dev_read_file', { path: 'site/kept.html' });
    expect(read.isError, JSON.stringify(read)).toBe(false);
    expect(read.result.content).toBe(KEPT);
    const AFTER = '<!doctype html><title>after</title><p>written after the update</p>\n';
    const later = await runFromConsole(page, 'dev_write_file', { path: 'site/after.html', content: AFTER });
    expect(later.isError, JSON.stringify(later)).toBe(false);
    expect(await viaWorker(page, '/after.html')).toEqual({ status: 200, text: AFTER });

    // Neither a recovery nor a second update happened on the way.
    expect(await registrationOf(page)).toEqual(SETTLED);
    expect(workers).toHaveLength(2);
  } finally {
    await context.close();
    await stop(server);
  }
});
