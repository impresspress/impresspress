import { expect, type Page } from '@playwright/test';

/**
 * What the specs about a DEAD runtime share: the one way a test can kill it,
 * and the two ways it can watch the boot shell deal with that.
 * (`dev-auth-errors.spec.ts`, `dev-stopped-navigation.spec.ts`,
 * `recovery-wipe.spec.ts`, and the exported bundle in `dev-workspace.spec.ts`.)
 */

/** Stop every service worker, as a browser does with an idle one. The next
 * request starts a new instance, which runs `initialize()` afresh. */
export async function stopWorkers(page: Page) {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('ServiceWorker.enable');
  await cdp.send('ServiceWorker.stopAllWorkers');
  await cdp.detach();
}

/**
 * Kill the runtime of the worker now serving `page`, so that the next request
 * it handles throws an `Error` whose message is `cause` — in the one way a
 * test can reach: `sw.js` is a module, so `handle_request` and `poisoned` are not
 * on the worker's global, but the wasm glue calls `Headers.prototype.set`
 * through it. Every response the runtime builds sets a `Content-Type`, so the
 * next request it handles throws out of `handle_request` — the catch path
 * under test.
 *
 * The RUNTIME, and nothing else in the worker. `sw.js`'s own code sets
 * exactly two headers, the `Cross-Origin-*` pair `passthrough` adds to the
 * static files a dev bundle serves, and those still work: an earlier version
 * of this helper replaced `Headers` wholesale, which also broke `passthrough`
 * — so the boot shell could not load `loader.js`, and the test "found" a hang
 * that was its own doing.
 *
 * The ACTIVE worker, found by the page's controller: after a recovery there
 * is a new one, and the context still lists the old.
 */
export async function killRuntime(page: Page, cause: string) {
  const scriptURL = await page.evaluate(() => navigator.serviceWorker.controller?.scriptURL);
  expect(scriptURL, 'a service worker controls the page').toBeTruthy();
  const workers = page.context().serviceWorkers().filter((w) => w.url() === scriptURL);
  expect(workers.length, 'workers at the controller URL').toBeGreaterThan(0);
  // Every instance at that URL: a worker that already died stays listed, and
  // breaking it again is harmless.
  for (const worker of workers) {
    await worker
      .evaluate((cause) => {
        const set = Headers.prototype.set;
        Headers.prototype.set = function (name: string, value: string) {
          if (!/^cross-origin-/i.test(name)) throw new Error(cause);
          return set.call(this, name, value);
        };
      }, cause)
      .catch(() => {});
  }
}

/**
 * Wait until the runtime is serving the page again after a recovery: a worker
 * controls it and the boot shell (`#status` is its own, and on no page the
 * runtime serves) has been replaced. The same two conditions
 * `bootServiceWorker` ends on, for the same reason — the loader's last reload
 * is on a timer nothing here can see.
 */
export async function served(page: Page) {
  await page.waitForFunction(
    () => navigator.serviceWorker.controller !== null && document.getElementById('status') === null,
    null,
    { timeout: 120_000 },
  );
}

/**
 * Every line the boot shell writes to `#status` from here on, across
 * navigations. The shell's recovery says why it is recovering and then
 * replaces the document, so the line is on screen for a moment; this is how a
 * test knows whether it was said — or, as importantly, that it was NOT.
 *
 * Not by setting anything in `sessionStorage` from a runtime page. The tab has
 * one `sessionStorage` and the loader's flags live in it, but measured in
 * this job's Chromium the two kinds of page do not see it alike: what the
 * SHELL writes is there on the runtime pages that follow and on later shell
 * loads, while a value written on a RUNTIME page (cross-origin isolated in a
 * dev bundle, so a different process from the shell's) was absent the next
 * time the shell loaded — and present again on the runtime page after that.
 * So a runtime page can READ what the shell did (`dev-auth-errors.spec.ts`'s
 * navigation test does), but cannot stage state for it. The loader only ever writes and reads
 * its flags from the shell, so it does not depend on the half that fails.
 */
export async function recordShellStatus(page: Page): Promise<string[]> {
  const lines: string[] = [];
  await page.exposeFunction('__recordStatus', (text: string) => {
    lines.push(text);
  });
  await page.addInitScript(() => {
    document.addEventListener('DOMContentLoaded', () => {
      const status = document.getElementById('status');
      if (!status) return;
      // Written by the shell, for the test that reads the loader's flags
      // from a runtime page to show that such a read is meaningful.
      sessionStorage.setItem('__e2e_written_by_shell', 'yes');
      new MutationObserver(() => (window as any).__recordStatus(status.textContent)).observe(
        status,
        { childList: true, characterData: true, subtree: true },
      );
    });
  });
  return lines;
}
