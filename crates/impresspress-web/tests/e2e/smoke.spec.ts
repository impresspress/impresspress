import { test, expect } from '@playwright/test';

/**
 * Lightweight smoke test that doesn't rebuild mid-test. Catches regressions
 * like the `/manifest.json` bypass bug and the `/sql-wasm-esm.js` import
 * path bug (both of which silently prevented SW registration).
 */
test('service worker registers and controls the page', async ({ page }) => {
  // `commit` is the right waitUntil here: this test exercises SW registration,
  // which `loader.js` triggers as soon as it parses (registration is async on
  // top of that). The downstream `waitForFunction(() => navigator.serviceWorker
  // .controller)` provides the actual assertion timing. Default `load` blocks
  // on every subresource; even `domcontentloaded` is delayed by deferred and
  // module scripts. Neither fires reliably here because the loader page imports
  // `/webllm-engine.js` and `/embed-engine.js` (type="module"), and a slow
  // jsdelivr CDN response for either one used to push the goto past the 60s
  // test timeout. Lazy-loading the WebLLM ESM (see webllm-engine.js) removed
  // most of the slowness, but `commit` is still the semantically correct
  // waitUntil for an SW-registration smoke and survives future regressions.
  await page.goto('/', { waitUntil: 'commit' });
  // Read the controller scriptURL inside the waitForFunction predicate so the
  // value is captured atomically. impresspress-web's loader.js redirects to
  // `boot_redirect` as soon as the SW takes control, which would otherwise
  // destroy the execution context between a separate `waitForFunction` +
  // `evaluate` pair.
  const handle = await page.waitForFunction(
    () => navigator.serviceWorker.controller?.scriptURL ?? null,
    null,
    { timeout: 20_000 },
  );
  const controllerURL = (await handle.jsonValue()) as string | null;
  expect(controllerURL).toMatch(/\/sw\.js$/);
});

test('boot redirect lands on the auth login page', async ({ page }) => {
  // boot_redirect is "/" (intercepted by SW → wasm router → 302 →
  // /b/auth/login for anonymous visitors). loader.js sets
  // `window.location.href = boot_redirect` once the SW takes control;
  // waiting for the resulting URL match avoids the
  // `net::ERR_ABORTED; maybe frame was detached?` race that an explicit
  // second goto would hit.
  //
  // Asserting on the rendered Sign In form rather than a non-empty body
  // catches the regression where boot_redirect pointed at /b/system/ —
  // an unclaimed path that returned a non-empty 404 page and silently
  // passed the smoke.
  await page.goto('/', { waitUntil: 'commit' });
  await page.waitForURL(/\/b\/auth\/login/, { timeout: 30_000 });
  await expect(page.locator('input#email')).toBeVisible();
  await expect(page.locator('input#password')).toBeVisible();
});

test('admin can log in and reach the dashboard', async ({ page }) => {
  // Regression guard for the browser-only JWT bug: the pipeline verified
  // access tokens against a secret snapshotted at build time. In the browser
  // target `WAFER_RUN__AUTH__JWT_SECRET` is auto-generated AFTER the runtime
  // is built, so that snapshot was the empty string while login signed with
  // the real seeded secret. Login returned a token, but every authenticated
  // request then 403'd and the user was bounced back to /b/auth/login.
  //
  // This is the only browser-WASM test that exercises a *protected* route
  // post-login — the anonymous boot smoke above can't catch a verify bug.
  await page.goto('/', { waitUntil: 'commit' });
  await page.waitForURL(/\/b\/auth\/login/, { timeout: 30_000 });

  await page.locator('input#email').fill('admin@example.com');
  await page.locator('input#password').fill('admin123');
  await page.getByRole('button', { name: /sign in/i }).click();

  // A usable session lands on the admin dashboard; the regression instead
  // redirected back to /b/auth/login?redirect=%2Fb%2Fadmin%2F.
  await page.waitForURL(/\/b\/admin\//, { timeout: 30_000 });
  await expect(page).toHaveURL(/\/b\/admin\//);
});
