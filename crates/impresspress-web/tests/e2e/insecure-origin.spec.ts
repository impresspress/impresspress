import { test, expect } from '@playwright/test';

/**
 * The boot page served on plain http at a LAN address.
 *
 * `lan.test` resolves to the same static server the smoke tests use, but
 * `http://lan.test` is not a secure context (only https, localhost and
 * 127.0.0.1 are), so the browser offers neither a service worker nor
 * `navigator.modelContext`.
 *
 * The mapping is a launch flag, and Playwright takes `launchOptions` only at
 * the top level of a file (it needs a browser of its own) — hence a file of
 * its own, so that the test runs on the suite's `page` fixture and records the
 * suite's trace on failure. `launchOptions` here REPLACES the config's, so the
 * args carry the suite's WebMCP flag too: the assertions below need the
 * testing surface present.
 */
test.use({
  launchOptions: {
    args: ['--enable-features=WebMCPTesting', '--host-resolver-rules=MAP lan.test 127.0.0.1'],
  },
});

test('the boot page says to use https or localhost', async ({ page, baseURL }) => {
  const port = new URL(baseURL as string).port;
  await page.goto(`http://lan.test:${port}/`);
  expect(await page.evaluate(() => window.isSecureContext)).toBe(false);
  // The spec's F1 secure-context row, against the real browser: the flag is
  // on, the testing surface is there, the API is not.
  expect(await page.evaluate(() => 'modelContextTesting' in navigator)).toBe(true);
  expect(await page.evaluate(() => 'modelContext' in navigator)).toBe(false);
  await expect(page.locator('#status')).toHaveText(/over https or on localhost/);
});
