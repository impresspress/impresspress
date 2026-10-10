import { test, expect, type BrowserContext, type Page } from '@playwright/test';
import { once } from 'node:events';
import { type ChildProcess } from 'node:child_process';
import { mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, utimesSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  bootServiceWorker,
  enterFromWelcome,
  runFromConsole,
  serveDirectory,
  SW_UPDATE_PORT,
} from './fixtures/dev-sandbox';
import { killRuntime, recordShellStatus, served } from './fixtures/stopped-runtime';

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
 * browser hands it the pages the old one controlled. Removing
 * `skipWaiting()` fails this test.
 *
 * `clients.claim()` in `activate` is NOT asserted, here or by any other spec.
 * It is for a page no worker controls yet, and removing it leaves this test
 * passing. A first visit works without it too: `loader.js` reloads the boot
 * shell when the page has no controller, and the reloaded page is the
 * worker's.
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

/** The URL path of the glue module a bundle's worker imports. */
function glueOf(dist: string): string {
  const manifest = JSON.parse(readFileSync(path.join(dist, 'asset-manifest.json'), 'utf8'));
  const hashed: string = manifest.assets['impresspress_web.js'];
  expect(hashed).toMatch(/^\/impresspress_web-[0-9a-f]+\.js$/);
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

/**
 * Which worker answers the page, and which runtime it was installed with:
 * the active worker's script URL (path and query), whether that worker
 * controls the page, and the runtime binaries the origin's workers keep for
 * themselves (`RUNTIME_CACHE` in `sw.js.tmpl`) as URL paths.
 *
 * A version keeps its own binary at `install`, loads it from there, and
 * drops every other version's at `activate`; so once the registration has
 * settled, the one binary kept is the active worker's, and the active worker
 * is the one answering the page. Read together — the kept binary alone says
 * what the last version to activate kept, not who answers. (Asking a worker
 * what it FETCHED does not say it either: a worker loads its runtime from
 * what it kept, and the browser may have stopped and restarted the instance
 * a test holds, which then reports nothing — the recovery case below asked
 * that way and intermittently read an empty list.)
 */
const answering = (page: Page) =>
  page.evaluate(async () => {
    const active = (await navigator.serviceWorker.getRegistration())?.active ?? null;
    const script = active ? new URL(active.scriptURL) : null;
    let kept: string[] = [];
    if (await caches.has('__impresspress_runtime')) {
      const cache = await caches.open('__impresspress_runtime');
      kept = (await cache.keys()).map((request) => new URL(request.url).pathname);
    }
    return {
      script: script ? script.pathname + script.search : null,
      controls: active !== null && navigator.serviceWorker.controller === active,
      kept,
    };
  });

/** What `answering` reads when the plain `/sw.js` worker with `runtime` answers. */
const plainWorkerWith = (runtime: string) => ({ script: '/sw.js', controls: true, kept: [runtime] });

/**
 * Every death a worker of `context` reports from here on — the console line
 * `selfDestruct` in `sw.js.tmpl` writes when its runtime stops.
 */
function deathsIn(context: BrowserContext): string[] {
  const deaths: string[] = [];
  context.on('console', (message) => {
    if (message.text().includes('SW self-destructing')) deaths.push(message.text());
  });
  return deaths;
}

/** One `ServiceWorker.workerVersionUpdated` report of one version. */
type VersionState = { id: string; scriptURL: string; status: string; runningStatus: string };

/**
 * Every service-worker version of the page's origin as DevTools reports it
 * (`ServiceWorker.workerVersionUpdated`), first-hand rather than inferred
 * from a page: `status` is installing / installed / activating / activated /
 * redundant, `runningStatus` stopped / starting / running / stopping. `all`
 * is each version's latest state; `history` every report, in the order they
 * came.
 */
async function versionsOf(page: Page) {
  const cdp = await page.context().newCDPSession(page);
  const versions = new Map<string, VersionState>();
  const history: VersionState[] = [];
  cdp.on('ServiceWorker.workerVersionUpdated', (event: any) => {
    for (const v of event.versions) {
      const state = { id: v.versionId, scriptURL: v.scriptURL, status: v.status, runningStatus: v.runningStatus };
      versions.set(v.versionId, state);
      history.push(state);
    }
  });
  await cdp.send('ServiceWorker.enable');
  const all = () => [...versions.values()];
  return {
    all,
    history: () => [...history],
    /** The id of the version that is `activated`, once there is one. */
    activeId: async () => {
      await expect.poll(() => all().some((v) => v.status === 'activated')).toBe(true);
      return all().find((v) => v.status === 'activated')!.id;
    },
    /** Stop every worker, busy or not (`ServiceWorker.stopAllWorkers`). */
    stopAll: () => cdp.send('ServiceWorker.stopAllWorkers'),
  };
}

/**
 * Whether `history` (`versionsOf`) shows Chromium's stall rather than a
 * worker of ours that never goes idle: after a newer version than `oldId`
 * reached `installed`, the old version — running then — was stopped, which
 * Chromium does to a worker only once no event of it is in flight, and was
 * then started again. The stop must be one that happened after the install
 * (`stopping`, then `stopped`), not a `stopped` state merely reported again
 * after it.
 */
function stoppedIdleThenRestarted(history: VersionState[], oldId: string): boolean {
  const installed = history.findIndex((v) => v.id !== oldId && v.status === 'installed');
  if (installed < 0) return false;
  const old = history.slice(installed + 1).filter((v) => v.id === oldId).map((v) => v.runningStatus);
  const stopping = old.indexOf('stopping');
  const stopped = stopping < 0 ? -1 : old.indexOf('stopped', stopping);
  return stopped >= 0 && old.indexOf('starting', stopped) >= 0;
}

const SETTLED = { installing: null, waiting: null, active: 'activated', activeControlsThisPage: true };

/** `GET path` from the page, so through whichever worker controls it. */
const viaWorker = (page: Page, urlPath: string) =>
  page.evaluate(async (target) => {
    const response = await fetch(target, { cache: 'no-store' });
    return { status: response.status, text: await response.text() };
  }, urlPath);

/**
 * Whether the version with `runtime` answers `page` (`plainWorkerWith`) and
 * the registration has settled on it (`SETTLED`).
 */
const updatedTo = async (page: Page, runtime: string) =>
  JSON.stringify(await answering(page)) === JSON.stringify(plainWorkerWith(runtime)) &&
  JSON.stringify(await registrationOf(page)) === JSON.stringify(SETTLED);

/**
 * Wait until the next deployment's version answers `page`, with only its
 * `runtime` kept, and say how it got there. Called after the visit that made
 * the browser find the new script; `oldId` is the version that answered the
 * page before the deploy (`versions.activeId()`, `versionsOf` created before
 * the deploy). `inStall` runs in outcome (b), before the second visit.
 *
 * Two outcomes, told apart from DevTools' own report of the versions:
 *
 * (a) `normal` — the new version activates and controls the page.
 * (b) `chromium-stall` — the new version stays `installed`, waiting, and the
 *     old one stays the active worker, answering the page.
 *
 * (b) is Chromium's rule for a version that called `skipWaiting()`
 * (`ServiceWorkerRegistration::ActivateWaitingVersionWhenReady` and
 * `ServiceWorkerVersion::StartWorkerInternal` in content/browser/
 * service_worker): it activates only once the active worker has no work.
 * When it installs while that worker is busy — answering the visit — Chromium
 * asks the worker to stop as soon as it is idle, and waits. A request from the
 * open page that starts the old worker again (the visit's own subresources,
 * racing that stop) clears the ask, and nothing asks again: the restarted
 * worker stops only by its ordinary idle timeout. Under DevTools, which
 * Playwright attaches to every worker, an idle worker is never stopped unless
 * asked, so the new version waits for Chromium's five-minute limit on a
 * waiting `skipWaiting()` version. Nothing in `sw.js` can end that sooner: a
 * worker cannot stop itself, and a waiting version cannot activate itself.
 *
 * (b) is accepted only with that mechanism's signature in DevTools' history
 * (`stoppedIdleThenRestarted`): after the new version installed, the old
 * worker stopped — Chromium stops a worker only once no event of it is in
 * flight, so it was idle — and was started again. A worker of ours that
 * never goes idle (an event `sw.js` never lets end) never stops, shows no
 * such history, and fails here instead of being stopped by the test — the
 * held-worker case below checks that.
 *
 * Without DevTools the browser stops the old worker once it has been idle for
 * its timeout, and the new version activates then. So in (b) the test does
 * what the browser does — stops the old worker, idle by the signature above,
 * and visits again; that must end in (a). If the stall comes back on that
 * second visit too, the test fails, which is also what a regression that
 * stops activation altogether looks like.
 */
async function nextDeploymentTakesOver(
  page: Page,
  versions: Awaited<ReturnType<typeof versionsOf>>,
  oldId: string,
  runtime: string,
  inStall: () => Promise<void> = async () => {},
): Promise<'normal' | 'chromium-stall'> {
  const reached = await expect
    .poll(() => updatedTo(page, runtime), { timeout: 60_000 })
    .toBe(true)
    .then(() => true, () => false);
  if (reached) return 'normal';
  // (b) — read from DevTools, not inferred: a new version waiting, the old
  // one still the active one, serving the page, and the history of the
  // mechanism that leaves them so.
  const newer = versions.all().filter((v) => v.id !== oldId && v.scriptURL.endsWith('/sw.js'));
  expect(newer.map((v) => v.status), JSON.stringify(versions.all())).toContain('installed');
  expect(versions.all().find((v) => v.id === oldId)?.status).toBe('activated');
  expect(await answering(page)).toMatchObject({ script: '/sw.js', controls: true });
  expect(
    stoppedIdleThenRestarted(versions.history(), oldId),
    `the new version is waiting, but the old worker did not stop and start again after it ` +
      `installed — it was never idle: ${JSON.stringify(versions.history())}`,
  ).toBe(true);
  await inStall();

  // The next visit: idle workers stopped, a navigation.
  await versions.stopAll();
  await page.goto('/', { waitUntil: 'load' });
  await served(page);
  await expect.poll(() => updatedTo(page, runtime), { timeout: 60_000 }).toBe(true);
  return 'chromium-stall';
}

test('a rebuild that changed nothing ships the same worker, and a new runtime ships a new one', () => {
  // Two runs of the build over one source tree: the browser compares `sw.js`
  // byte for byte, so identical here is what "no spurious update" means.
  expect(workerScript(BOOTSTRAP_DIST)).toBe(workerScript(DEV_DIST));
  expect(runtimeOf(BOOTSTRAP_DIST)).toBe(runtimeOf(DEV_DIST));
  // …and a different runtime binary is a different worker script, which is
  // the only thing that makes the browser install. `sw.js` does not name the
  // wasm: it imports the glue, and the glue — which does name the wasm — is
  // hashed after that name is written into it, so its own name follows the
  // wasm's. (The build id in `sw.js`'s first line differs here too, but only
  // because this directory is not a git root; at one it is the commit, and
  // the glue's name is the whole difference.)
  expect(runtimeOf(UPDATE_DIST)).not.toBe(runtimeOf(DEV_DIST));
  expect(glueOf(UPDATE_DIST)).not.toBe(glueOf(DEV_DIST));
  expect(workerScript(UPDATE_DIST)).toContain(`from '${glueOf(UPDATE_DIST)}'`);
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
  try {
    const page = await context.newPage();

    // ---- the first deployment, used --------------------------------------
    await bootServiceWorker(page);
    expect(await answering(page)).toEqual(plainWorkerWith(first));
    expect(await registrationOf(page)).toEqual(SETTLED);
    const versions = await versionsOf(page);
    const oldId = await versions.activeId();

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
    // Nothing installed: the same worker, the same runtime.
    expect(await registrationOf(page)).toEqual(SETTLED);
    expect(await answering(page)).toEqual(plainWorkerWith(first));

    // ---- the next deployment ----------------------------------------------
    await stop(server);
    // A deployment is newer than the one it replaces, and here that has to be
    // made true: CI builds this bundle BEFORE the first one (the blank build
    // goes last, because it keeps `dist/`). `python3 -m http.server`
    // validates by modification time alone, so a worker script older than
    // the one the browser holds is answered `304 Not Modified` — the browser
    // is told nothing changed, whatever the bytes are — and no update is ever
    // installed. (A host that validates by content, as an ETag does, has no
    // such blind spot; this is the stand-in host's, not the bundle's.)
    const deployedAt = new Date();
    utimesSync(path.join(UPDATE_DIST, 'sw.js'), deployedAt, deployedAt);
    server = await serveDirectory(UPDATE_DIST, SW_UPDATE_PORT);
    // As after any deploy, the previous runtime's file is no longer served:
    // nothing from here on can be loading it.
    expect((await request.get(`${ORIGIN}${first}`)).status()).toBe(404);
    expect((await request.get(`${ORIGIN}${next}`)).status()).toBe(200);

    // A visit, and nothing else: no `update()` call, no boot shell. The page
    // is answered by the worker the browser already had; the browser's own
    // check after the navigation is what finds the new script.
    await page.goto('/', { waitUntil: 'load' });
    // The new worker did not wait for this tab to close (`skipWaiting`): it
    // is the active one, nothing is left waiting, and the open page is its —
    // by either outcome `nextDeploymentTakesOver` describes. Usually the
    // browser's check finds the new script after this visit, with the old
    // worker idle, and (a) follows at once. The check can also land BEFORE
    // the visit (the previous navigation's delayed update check, firing after
    // the host has switched — CI has met it), so the new version installs
    // while the old worker answers this visit, and that is (b)'s ordering.
    const branch = await nextDeploymentTakesOver(page, versions, oldId, next);
    test.info().annotations.push({ type: 'activation', description: branch });
    console.log(`sw-update returning-browser case: activation ${branch}`);

    // ---- the site, on the new runtime ---------------------------------------
    // The next request is the new worker's first, so this is where it loads
    // its runtime — the new deployment's, over the storage the old one left.
    // The previous one's binary is gone from the host and from the origin.
    expect(await viaWorker(page, '/kept.html')).toEqual({ status: 200, text: KEPT });
    expect(await answering(page)).toEqual(plainWorkerWith(next));

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
    expect(await answering(page)).toEqual(plainWorkerWith(next));
  } finally {
    await context.close();
    await stop(server);
  }
});

// The same deployment, met the way a returning visitor usually meets it: the
// browser has STOPPED their idle worker long before they come back. The visit
// after the deploy starts that old worker again, and it boots the runtime it
// was installed with — whose hashed binary the deploy has deleted from the
// host. It must still boot (it keeps its own runtime, `RUNTIME_CACHE` in
// `sw.js.tmpl`), answer the visit, and be replaced by the update the visit
// triggers: no death, no recovery, nothing lost.
//
// Before the worker kept its runtime, the restarted worker asked the host for
// the deleted binary, got a 404, died at stage `load` and sent the page into
// the boot shell's recovery while the browser was installing the update: two
// replacements of one worker at once, and the path behind an intermittent
// timeout of the case above, which met it only when Chrome happened to stop
// the worker. Stopping it here takes that path on every run.
//
// What the update then does has the two outcomes `nextDeploymentTakesOver`
// describes; here the restarted old worker is answering the visit when the
// new version installs, which is what makes (b) common in this case.
//
// The branch taken is recorded as the test's `activation` annotation.
test('a returning browser whose worker was stopped moves to the new deployment, with no recovery', async ({
  browser,
  request,
}) => {
  test.setTimeout(420_000);
  const first = runtimeOf(DEV_DIST);
  const next = runtimeOf(UPDATE_DIST);

  let server = await serveDirectory(DEV_DIST, SW_UPDATE_PORT);
  const context = await browser.newContext({ baseURL: ORIGIN });
  try {
    const page = await context.newPage();
    const shellSaid = await recordShellStatus(page);
    const deaths = deathsIn(context);
    await bootServiceWorker(page);
    const KEPT = '<!doctype html><title>kept</title><p>written before the update</p>\n';
    await enterFromWelcome(page);
    const written = await runFromConsole(page, 'dev_write_file', { path: 'site/kept.html', content: KEPT });
    expect(written.isError, JSON.stringify(written)).toBe(false);
    expect(await viaWorker(page, '/kept.html')).toEqual({ status: 200, text: KEPT });

    const versions = await versionsOf(page);
    const oldId = await versions.activeId();

    // The browser stops the idle worker; then the deploy.
    await versions.stopAll();
    await stop(server);
    const deployedAt = new Date();
    utimesSync(path.join(UPDATE_DIST, 'sw.js'), deployedAt, deployedAt);
    server = await serveDirectory(UPDATE_DIST, SW_UPDATE_PORT);
    expect((await request.get(`${ORIGIN}${first}`)).status()).toBe(404);

    /** Whatever the outcome: nothing died, nothing recovered, the data is intact. */
    const assertNoDeathNoRecovery = async () => {
      expect(deaths).toEqual([]);
      expect(shellSaid.filter((line) => line.includes('runtime stopped'))).toEqual([]);
      expect(versions.all().filter((v) => v.scriptURL.includes('?recovery='))).toEqual([]);
      expect(await viaWorker(page, '/kept.html')).toEqual({ status: 200, text: KEPT });
    };
    // The visit. The old worker starts again to answer it, from the runtime
    // it kept — the page is the app, not the boot shell.
    await page.goto('/', { waitUntil: 'load' });
    await served(page);

    const branch = await nextDeploymentTakesOver(page, versions, oldId, next, assertNoDeathNoRecovery);
    test.info().annotations.push({ type: 'activation', description: branch });
    console.log(`sw-update stopped-worker case: activation ${branch}`);

    await assertNoDeathNoRecovery();
    expect(await answering(page)).toEqual(plainWorkerWith(next));
    expect(await registrationOf(page)).toEqual(SETTLED);
  } finally {
    await context.close();
    await stop(server);
  }
});

/** The path a held bundle's worker answers with an event that never ends. */
const HOLD_PATH = '/__sw-update-hold';

/**
 * `dist` with a worker that can be held busy for good: every file is the
 * bundle's own (linked), except `sw.js`, whose `fetch` listener first answers
 * `HOLD_PATH` and keeps that event open with a `waitUntil` that never
 * settles — what a product bug that never lets an event end looks like.
 * Returns the directory; the caller removes it.
 */
function heldWorkerBundle(dist: string): string {
  const dir = mkdtempSync(path.join(tmpdir(), 'sw-update-held-'));
  for (const entry of readdirSync(dist)) {
    if (entry !== 'sw.js') symlinkSync(path.join(dist, entry), path.join(dir, entry));
  }
  const listener = "self.addEventListener('fetch', (event) => {";
  const script = workerScript(dist);
  expect(script, `${dist}/sw.js has no fetch listener to hold`).toContain(listener);
  writeFileSync(
    path.join(dir, 'sw.js'),
    script.replace(
      listener,
      `${listener}\n    if (new URL(event.request.url).pathname === '${HOLD_PATH}') {\n` +
        `        event.waitUntil(new Promise(() => {}));\n` +
        `        event.respondWith(new Response('held'));\n` +
        `        return;\n    }`,
    ),
  );
  return dir;
}

// The stall branch of `nextDeploymentTakesOver` is not a way round a worker
// of ours that never goes idle. Here the old worker is held busy for good —
// an event its `waitUntil` never lets end — so the new version can never
// activate (Chromium waits for the old worker to have no work), exactly the
// picture of (b) in every state DevTools reports. What tells them apart is
// the history: a busy worker is never stopped, so it shows no stop-and-
// restart after the install, and the helper must fail rather than stop it.
test('an old worker that never goes idle is not taken for Chromium’s stall', async ({ browser }) => {
  test.setTimeout(300_000);
  const next = runtimeOf(UPDATE_DIST);
  const held = heldWorkerBundle(DEV_DIST);
  let server = await serveDirectory(held, SW_UPDATE_PORT);
  const context = await browser.newContext({ baseURL: ORIGIN });
  try {
    const page = await context.newPage();
    await bootServiceWorker(page);
    const versions = await versionsOf(page);
    const oldId = await versions.activeId();
    expect(await viaWorker(page, HOLD_PATH)).toEqual({ status: 200, text: 'held' });

    await stop(server);
    const deployedAt = new Date();
    utimesSync(path.join(UPDATE_DIST, 'sw.js'), deployedAt, deployedAt);
    server = await serveDirectory(UPDATE_DIST, SW_UPDATE_PORT);
    await page.goto('/', { waitUntil: 'load' });

    const outcome = await nextDeploymentTakesOver(page, versions, oldId, next).then(
      (branch) => branch,
      (error: Error) => error,
    );
    expect(outcome, `the held worker was taken for: ${String(outcome)}`).toBeInstanceOf(Error);
    expect(String(outcome)).toContain('it was never idle');
    // And it really was the update that could not get in: installed, waiting.
    expect(await registrationOf(page)).toMatchObject({ waiting: 'installed', active: 'activated' });
  } finally {
    await context.close();
    await stop(server);
    rmSync(held, { recursive: true, force: true });
  }
});

// A recovery replaces a dead worker by registering the same file under a
// new script URL, `/sw.js?recovery=<time>` (`WORKER_URL` in `loader.js.tmpl`
// says why), and that URL is the registration's from then on. A deployment
// must still replace it: the browser's update check asks for the
// registration's script URL, query and all, and the static host answers the
// query-less file's new bytes.
//
// The one known cost of the query, which this host cannot show: a CDN that
// keys its cache on the query string and is purged of `/sw.js` only could
// answer `/sw.js?recovery=T` with the previous deployment's worker once.
// That worker cannot install — the runtime binary it keeps at install is
// gone from the host — so it replaces nothing (`WORKER_URL` in
// `loader.js.tmpl`), and nothing is erased.
test('a new deployment replaces a worker that a recovery registered', async ({ browser }) => {
  test.setTimeout(300_000);
  const next = runtimeOf(UPDATE_DIST);

  let server = await serveDirectory(DEV_DIST, SW_UPDATE_PORT);
  const context = await browser.newContext({ baseURL: ORIGIN });
  try {
    const page = await context.newPage();
    await bootServiceWorker(page);

    // A recovery: the runtime dies on a navigation, the shell replaces the
    // worker and the page comes back.
    await killRuntime(page, 'injected by sw-update.spec.ts before a recovery');
    await page.goto('/b/auth/login', { waitUntil: 'commit' });
    await page.waitForFunction(
      () => navigator.serviceWorker.controller !== null && document.getElementById('status') === null,
      null,
      { timeout: 120_000 },
    );
    await expect(page.locator('input#email')).toBeVisible();
    const scriptUrl = () =>
      page.evaluate(async () => (await navigator.serviceWorker.getRegistration())!.active!.scriptURL);
    const recovered = await scriptUrl();
    expect(recovered).toMatch(/\/sw\.js\?recovery=\d+$/);
    expect(await registrationOf(page)).toEqual(SETTLED);

    // The next deployment (dated as in the tests above), and a visit.
    await stop(server);
    const deployedAt = new Date();
    utimesSync(path.join(UPDATE_DIST, 'sw.js'), deployedAt, deployedAt);
    server = await serveDirectory(UPDATE_DIST, SW_UPDATE_PORT);
    await page.goto('/', { waitUntil: 'load' });
    const recoveredScript = new URL(recovered).pathname + new URL(recovered).search;
    await expect
      .poll(() => answering(page), { timeout: 120_000 })
      .toEqual({ script: recoveredScript, controls: true, kept: [next] });
    await expect.poll(() => registrationOf(page), { timeout: 60_000 }).toEqual(SETTLED);

    // The new deployment's runtime answers, under the same script URL: the
    // deploy changed the bytes behind it, not the registration.
    expect((await viaWorker(page, '/b/auth/login')).status).toBe(200);
    expect(await answering(page)).toEqual({ script: recoveredScript, controls: true, kept: [next] });
  } finally {
    await context.close();
    await stop(server);
  }
});

// A worker whose runtime has died stays registered (`poisoned` in
// `sw.js.tmpl` says why): it answers navigations with the boot shell and
// everything else with a 503. That must not make it a worker a deployment
// cannot replace. It is replaced the way any worker is — the browser's
// update check finds the new script, `skipWaiting()` activates it over the
// dead one, and the open page is handed to it — with no recovery and no
// reload.
test('a new deployment replaces a worker whose runtime has died', async ({ browser }) => {
  test.setTimeout(300_000);
  const next = runtimeOf(UPDATE_DIST);
  const cause = 'injected by sw-update.spec.ts';

  let server = await serveDirectory(DEV_DIST, SW_UPDATE_PORT);
  const context = await browser.newContext({ baseURL: ORIGIN });
  try {
    const page = await context.newPage();
    await bootServiceWorker(page);

    // The runtime dies on a request from the page. The page is left where it
    // is, and the worker — dead — is still the registered, active one.
    await killRuntime(page, cause);
    const dead = await viaWorker(page, '/b/auth/login');
    expect(dead.status).toBe(503);
    expect(JSON.parse(dead.text)).toMatchObject({
      code: 'runtime_stopped',
      stage: 'request',
      cause: `error handling request: Error: ${cause}`,
    });
    expect((await viaWorker(page, '/b/auth/login')).status, 'it stays dead').toBe(503);
    expect(await registrationOf(page)).toEqual(SETTLED);
    await page.evaluate(() => {
      (window as any).__sameDocument = true;
    });

    // The next deployment (dated as in the test above).
    await stop(server);
    const deployedAt = new Date();
    utimesSync(path.join(UPDATE_DIST, 'sw.js'), deployedAt, deployedAt);
    server = await serveDirectory(UPDATE_DIST, SW_UPDATE_PORT);

    // The update check — the one the browser makes after a navigation and
    // `loader.js` makes on every boot — asked for here directly, so that
    // nothing else is in play: no navigation, hence no boot shell, hence no
    // recovery.
    await page.evaluate(async () => {
      const registration = await navigator.serviceWorker.getRegistration();
      await registration!.update();
    });
    await expect.poll(() => answering(page), { timeout: 120_000 }).toEqual(plainWorkerWith(next));
    await expect.poll(() => registrationOf(page), { timeout: 60_000 }).toEqual(SETTLED);

    // The same page, never reloaded, is now answered by a live runtime — the
    // new deployment's.
    expect((await viaWorker(page, '/b/auth/login')).status).toBe(200);
    expect(await answering(page)).toEqual(plainWorkerWith(next));
    expect(await page.evaluate(() => (window as any).__sameDocument)).toBe(true);
  } finally {
    await context.close();
    await stop(server);
  }
});

// The same dead worker, met by a NAVIGATION after the deploy instead of an
// update check: the worker answers it with the boot shell, and the browser's
// update check after it is installing the new deployment's worker at the
// same time. Both the shell's recovery and the update could replace the
// dead worker here, and only one may: the shell asks first whether an update
// owns the transition (`updateUnderway` in `loader.js.tmpl`), finds the new
// version installed, and boots onto it — no recovery, no `?recovery=`
// worker, nothing erased.
test('a navigation to a dead worker after a deployment is the update’s, not a recovery’s', async ({ browser }) => {
  test.setTimeout(300_000);
  const next = runtimeOf(UPDATE_DIST);
  const cause = 'injected by sw-update.spec.ts before a navigation';

  let server = await serveDirectory(DEV_DIST, SW_UPDATE_PORT);
  const context = await browser.newContext({ baseURL: ORIGIN });
  try {
    const page = await context.newPage();
    const shellSaid = await recordShellStatus(page);
    await bootServiceWorker(page);

    // Dead on a request from the page, which keeps the answer.
    await killRuntime(page, cause);
    expect((await viaWorker(page, '/b/auth/login')).status).toBe(503);

    await stop(server);
    const deployedAt = new Date();
    utimesSync(path.join(UPDATE_DIST, 'sw.js'), deployedAt, deployedAt);
    server = await serveDirectory(UPDATE_DIST, SW_UPDATE_PORT);

    // The reload the 503 speaks of, to a path only the runtime serves.
    await page.goto('/b/auth/login', { waitUntil: 'commit' });
    await served(page);
    await expect(page.locator('input#email')).toBeVisible();
    await expect.poll(() => registrationOf(page), { timeout: 60_000 }).toEqual(SETTLED);

    const active = await page.evaluate(
      async () => (await navigator.serviceWorker.getRegistration())!.active!.scriptURL,
    );
    expect(active).toBe(`${ORIGIN}/sw.js`);
    expect(await answering(page)).toEqual(plainWorkerWith(next));
    // The shell said why it was waiting, and never that it restarted or
    // recovered the worker itself.
    const said = shellSaid.filter((line) => line.includes('runtime stopped'));
    expect(said.length, JSON.stringify(shellSaid)).toBeGreaterThan(0);
    for (const line of said) expect(line).toContain('a new version is replacing it');
  } finally {
    await context.close();
    await stop(server);
  }
});
