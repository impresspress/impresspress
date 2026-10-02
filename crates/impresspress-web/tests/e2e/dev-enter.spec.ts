import { test, expect, type Page } from '@playwright/test';
import {
  ADMIN_EMAIL,
  ADMIN_PASSWORD,
  bootServiceWorker,
  PAGE_TOOLS,
  WELCOME_PHRASE,
} from './fixtures/dev-sandbox';

/**
 * The sandbox for an agent that can only drive the page: one-click entry, and
 * the Tool console.
 *
 * Both exist because of one visitor's agent — a cloud browser with no WebMCP —
 * that could not get past the login form and, had it done so, would have found
 * no tools. So NOTHING in this file installs the model-context polyfill the
 * other sandbox specs start with: `document.modelContext` does not exist on
 * these pages, which is the browser under test.
 *
 * Each test gets its own browser context, and so its own origin storage — its
 * own service worker, database and seed import (see `dev-workspace.spec.ts`).
 */

/** Every main-frame URL the page visits from here on, as pathnames. */
function recordNavigations(page: Page): string[] {
  const visited: string[] = [];
  page.on('framenavigated', (frame) => {
    if (frame === page.mainFrame()) visited.push(new URL(frame.url()).pathname);
  });
  return visited;
}

/** Follow the welcome page's own "Open workspace" link to the workspace. */
async function enterFromWelcome(page: Page) {
  await expect(page.locator('body')).toContainText(WELCOME_PHRASE, { timeout: 60_000 });
  await page.getByRole('link', { name: /open workspace/i }).first().click();
  await page.waitForURL((url) => url.pathname === '/b/dev', { timeout: 60_000 });
  // The script ran: the progress ladder is drawn by its first status read.
  await expect(page.locator('#dev-progress-steps li').first()).toBeAttached({ timeout: 60_000 });
}

/** Run one tool from the Tool console and return what the result box shows. */
async function runFromConsole(
  page: Page,
  tool: string,
  args: Record<string, unknown> | null,
): Promise<{ isError: boolean; result: any }> {
  await page.locator('#dev-console-tool').selectOption(tool);
  if (args !== null) {
    await page.locator('#dev-console-args').fill(JSON.stringify(args));
  }
  // Emptied first, so the wait below is for THIS run's report rather than
  // one a previous run left in the box.
  await page.locator('#dev-console-result').evaluate((el) => {
    el.removeAttribute('data-is-error');
  });
  await page.locator('#dev-console-run').click();
  await expect(page.locator('#dev-console-result')).toHaveAttribute('data-is-error', /^(true|false)$/, {
    timeout: 60_000,
  });
  return JSON.parse((await page.locator('#dev-console-result').textContent()) ?? '');
}

test('"Open workspace" lands on /b/dev signed in, with no form', async ({ page }) => {
  test.setTimeout(300_000);
  await bootServiceWorker(page);

  // Anonymous to begin with: the workspace's API refuses this page.
  expect(await page.evaluate(async () => (await fetch('/b/dev/api/status')).status)).toBe(401);
  // The link the seed ships, and the credentials still printed beside it —
  // they are how a human signs back in once this session expires.
  await expect(page.getByRole('link', { name: /open workspace/i })).toHaveAttribute(
    'href',
    '/b/dev/enter',
  );
  await expect(page.locator('body')).toContainText(ADMIN_EMAIL);
  await expect(page.locator('body')).toContainText(ADMIN_PASSWORD);

  const visited = recordNavigations(page);
  await enterFromWelcome(page);

  // One click, two documents, no login page in between and nothing typed.
  expect(visited).toEqual(['/b/dev/enter', '/b/dev']);
  // Signed in, as the admin the workspace needs.
  expect(await page.evaluate(async () => (await fetch('/b/dev/api/status')).status)).toBe(200);
  // `location.replace`: Back from the workspace is the welcome page, not a
  // page whose only act is to send the visitor forward again.
  await page.goBack({ waitUntil: 'commit' });
  await page.waitForURL((url) => url.pathname === '/', { timeout: 60_000 });

  // Following the link a second time, already signed in, mints no second
  // session: the entry page finds the one that is there and goes straight on.
  const logins: string[] = [];
  page.on('request', (request) => {
    if (new URL(request.url()).pathname === '/b/auth/api/login') logins.push(request.method());
  });
  await page.goto('/b/dev/enter', { waitUntil: 'commit' });
  await page.waitForURL((url) => url.pathname === '/b/dev', { timeout: 60_000 });
  expect(logins).toEqual([]);
});

test('once the admin password is changed, entry falls back to the login page and says why', async ({
  page,
  context,
}) => {
  test.setTimeout(300_000);
  await bootServiceWorker(page);
  await enterFromWelcome(page);

  // The owner makes the instance theirs.
  const NEW_PASSWORD = 'a-password-of-my-own-1';
  const changed = await page.evaluate(
    async ([current, next]) =>
      (
        await fetch('/b/auth/api/change-password', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ current_password: current, new_password: next }),
        })
      ).status,
    [ADMIN_PASSWORD, NEW_PASSWORD] as const,
  );
  expect(changed).toBe(200);
  // Changing the password ends the sessions made with the old one; cleared
  // here as well so what follows does not depend on that.
  await context.clearCookies();

  // The entry page still tries the seeded password — it is what the instance
  // is configured with — and the login route refuses it.
  await page.goto('/b/dev/enter', { waitUntil: 'commit' });
  await expect(page.locator('#dev-enter-status')).toContainText('One-click entry is off', {
    timeout: 60_000,
  });
  await expect(page.locator('#dev-enter-status')).toContainText('password was changed');
  expect(await page.evaluate(async () => (await fetch('/b/dev/api/status')).status)).toBe(401);

  // The way in is the normal login page, which the page links — and which
  // still works, through the form, with the password the owner chose.
  await page.locator('#dev-enter-login').click();
  await page.waitForURL((url) => url.pathname === '/b/auth/login', { timeout: 60_000 });
  await page.locator('input#email').fill(ADMIN_EMAIL);
  await page.locator('input#password').fill(NEW_PASSWORD);
  await page.getByRole('button', { name: /sign in/i }).click();
  await page.waitForURL((url) => url.pathname === '/b/dev', { timeout: 60_000 });

  // Signed in again, "Open workspace" works again: the entry page uses the
  // session that is there instead of signing in — which, with the seeded
  // password now wrong, it could not do. Without that check the owner of a
  // sandbox with its own password would be shown "one-click entry is off"
  // every time they followed the welcome page's link.
  await page.goto('/b/dev/enter', { waitUntil: 'commit' });
  await page.waitForURL((url) => url.pathname === '/b/dev', { timeout: 60_000 });
  expect(await page.evaluate(async () => (await fetch('/b/dev/api/status')).status)).toBe(200);
});

test('without WebMCP, the Tool console lists the tools, reads the status and publishes a file', async ({
  page,
}) => {
  test.setTimeout(300_000);
  await bootServiceWorker(page);
  await enterFromWelcome(page);

  // The browser under test: no WebMCP at all.
  expect(await page.evaluate(() => 'modelContext' in document)).toBe(false);
  // …and the page says so, where an agent reading it looks first.
  await expect(page.locator('#dev-webmcp-status')).toHaveText(
    'This browser has no WebMCP: use the Tool console below, or the file editor.',
    { timeout: 60_000 },
  );

  // Every tool the page publishes — `tools.json`'s and the two page-local
  // ones — and nothing else.
  await expect(page.locator('#dev-console-tool option')).toHaveCount(PAGE_TOOLS.length, {
    timeout: 60_000,
  });
  const listed = await page.locator('#dev-console-tool option').allTextContents();
  expect([...listed].sort()).toEqual(PAGE_TOOLS);
  await expect(page.locator('#dev-console')).toBeVisible();
  await expect(page.locator('#dev-console-run')).toBeEnabled();

  // --- dev_status: the first call the guide tells an agent to make --------
  await page.locator('#dev-console-tool').selectOption('dev_status');
  await expect(page.locator('#dev-console-description')).toContainText('Call this first');
  // A tool that requires nothing is pre-filled with a complete call.
  await expect(page.locator('#dev-console-args')).toHaveValue('{}');
  const status = await runFromConsole(page, 'dev_status', null);
  expect(status.isError).toBe(false);
  expect(status.result.template).toBe('blank');
  expect(status.result.active_generation).toBeTruthy();

  // --- dev_write_file: a new site file, live at its URL -------------------
  await page.locator('#dev-console-tool').selectOption('dev_write_file');
  // Pre-filled from the input schema: the required properties, as
  // placeholders an agent replaces.
  const prefilled = JSON.parse(await page.locator('#dev-console-args').inputValue());
  expect(Object.keys(prefilled).sort()).toEqual(['content', 'path']);

  const CONTENT =
    '<!doctype html><html><head><title>From the console</title></head>' +
    '<body><h1 id="made-by-console">Written from the Tool console</h1></body></html>';
  // Not there yet. (Compared as content, not as a status: the site answers
  // an unknown path with its own fallback document.)
  expect(await page.evaluate(async () => (await fetch('/console.html')).text())).not.toBe(CONTENT);
  const written = await runFromConsole(page, 'dev_write_file', {
    path: 'site/console.html',
    content: CONTENT,
  });
  expect(written.isError, JSON.stringify(written)).toBe(false);
  expect(written.result.path).toBe('site/console.html');
  expect(written.result.generation.cause).toBe('site_write');

  // Published: the file serves at its URL, byte for byte.
  expect(await page.evaluate(async () => (await fetch('/console.html')).text())).toBe(CONTENT);
  // …and the page caught up exactly as it does after an agent's call: the
  // file tree shows the new file (the `MUTATING` catch-up in `dev.js`).
  await expect(page.locator('#dev-file-list a[data-path="site/console.html"]')).toBeVisible({
    timeout: 60_000,
  });

  // A refusal comes back as an error result with the server's reason, not as
  // a silent nothing.
  const refused = await runFromConsole(page, 'dev_write_file', {
    path: 'outside/the-workspace.txt',
    content: 'x',
  });
  expect(refused.isError).toBe(true);
  expect(String(refused.result)).toMatch(/^Request failed \(4\d\d\): /);
  await expect(page.locator('#dev-console-result')).toHaveAttribute('data-is-error', 'true');
});
