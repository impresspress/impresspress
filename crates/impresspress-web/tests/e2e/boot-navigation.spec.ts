import { createReadStream, existsSync, statSync } from 'node:fs';
import { createServer, type ServerResponse } from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { test, expect, type Browser } from '@playwright/test';

/**
 * A navigation that begins while the boot shell is waiting for the app is
 * where the tab goes — in every engine.
 *
 * Once the worker has taken the shell, the shell probes the app and, when the
 * app answers, reloads itself into it. A navigation the reader starts in that
 * window (a link, the address bar, an agent's `goto`) is held by the worker
 * for the same start, and a reload requested as both are answered CANCELS it:
 * the tab lands back on `/`. Chromium fires `beforeunload` as the navigation
 * begins, and the shell stands down on that. iOS Safari does not fire it
 * reliably; WebKit instead rejects the shell's probe as the navigation begins,
 * and every request the page makes after it. The shell goes on to the app only
 * on the app's answer, so a probe that could not be made is made again, not
 * taken for one — `loader.js.tmpl`'s `leaving` and `askApp` say the rest.
 *
 * In WebKit the case runs twice: as the browser delivers `beforeunload`, and
 * with it withheld from the page — iOS Safari's behaviour, in a WebKit that
 * can be launched here. Chromium always fires it, so there it runs once.
 *
 * The shell is the bundle's own (`index.html`, `loader.js` from `pkg/`); the
 * worker is `fixtures/stand-in-boot-worker.js`, which says why. The host holds
 * the stand-in's "runtime start" (`/__start`) until the test lets it go, so
 * the probe and the navigation are both waiting when it does.
 *
 * Chromium runs this in CI (the smoke job). WebKit is a project of its own,
 * `webkit`, only when `E2E_WEBKIT` is set (`playwright.config.ts`): the CI
 * image installs Chromium alone.
 */

const PORT = 8091;
const PKG = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../pkg');
const STAND_IN = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  'fixtures/stand-in-boot-worker.js',
);

const TYPES: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript',
  '.json': 'application/json',
};

/**
 * The bundle's files, with the stand-in at `/sw.js` and `/__start` held until
 * `start()`. `held()` is how many requests are waiting on it.
 */
async function serveShell() {
  let started = false;
  let waiting: ServerResponse[] = [];
  const send = (response: ServerResponse, file: string) => {
    if (!existsSync(file) || !statSync(file).isFile()) {
      response.writeHead(404, { 'Content-Type': 'text/plain', 'Cache-Control': 'no-store' });
      response.end('no such file');
      return;
    }
    response.writeHead(200, {
      'Content-Type': TYPES[path.extname(file)] ?? 'application/octet-stream',
      'Cache-Control': 'no-store',
    });
    createReadStream(file).pipe(response);
  };
  const server = createServer((request, response) => {
    const pathname = decodeURIComponent(new URL(request.url ?? '/', 'http://x').pathname);
    if (pathname === '/__start') {
      if (started) {
        response.end('started');
      } else {
        waiting.push(response);
      }
      return;
    }
    if (pathname === '/sw.js') {
      send(response, STAND_IN);
      return;
    }
    const file = path.join(PKG, pathname === '/' ? 'index.html' : pathname);
    if (!path.resolve(file).startsWith(PKG + path.sep)) {
      response.writeHead(404);
      response.end();
      return;
    }
    send(response, file);
  });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(PORT, '127.0.0.1', resolve);
  });
  return {
    held: () => waiting.length,
    start: () => {
      started = true;
      for (const response of waiting) response.end('started');
      waiting = [];
    },
    close: () =>
      new Promise<void>((resolve) => {
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  };
}

/**
 * Open the shell, wait until its probe is held on the app's start, start a
 * navigation to `to`, let the start go, and say where the tab ended up.
 */
async function navigateDuringBoot(
  browser: Browser,
  to: string,
  { beforeunload }: { beforeunload: boolean },
) {
  const host = await serveShell();
  const context = await browser.newContext({ baseURL: `http://127.0.0.1:${PORT}` });
  try {
    if (!beforeunload) {
      // What iOS Safari does: the page is not told. Every listener the page
      // adds for it is dropped, the shell's included.
      await context.addInitScript(() => {
        const add = EventTarget.prototype.addEventListener;
        EventTarget.prototype.addEventListener = function (
          this: EventTarget,
          type: string,
          ...rest: [EventListenerOrEventListenerObject | null, (boolean | AddEventListenerOptions)?]
        ) {
          if (type === 'beforeunload') return;
          return add.call(this, type, ...rest);
        };
      });
    }
    const page = await context.newPage();
    await page.goto('/', { waitUntil: 'commit' });
    // The worker has taken the shell and its probe is waiting on the start.
    await expect.poll(() => host.held(), { timeout: 30_000 }).toBe(1);
    await expect(page.locator('#status')).toHaveText(/^Loading /);

    const navigation = page.goto(to, { waitUntil: 'load' });
    // The navigation is waiting on the same start.
    await expect.poll(() => host.held(), { timeout: 30_000 }).toBeGreaterThanOrEqual(2);
    host.start();
    const response = await navigation;
    return {
      fromWorker: response?.fromServiceWorker() ?? null,
      path: new URL(page.url()).pathname,
      title: await page.title(),
    };
  } finally {
    await context.close();
    await host.close();
  }
}

for (const beforeunload of [true, false]) {
  const how = beforeunload ? 'with beforeunload' : 'with no beforeunload, as on iOS Safari';

  test(`a navigation begun while the app is starting lands where it was sent (${how})`, async ({
    browser,
    browserName,
  }) => {
    test.skip(!beforeunload && browserName !== 'webkit', 'Chromium always fires beforeunload');
    const landed = await navigateDuringBoot(browser, '/b/auth/signup', { beforeunload });
    expect(landed).toEqual({ fromWorker: true, path: '/b/auth/signup', title: 'app /b/auth/signup' });
  });
}
