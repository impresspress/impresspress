import { readFileSync } from 'node:fs';
import { test, expect, request as apiRequest, type APIRequestContext, type Page } from '@playwright/test';
import { WIDTHS, isWrong, layout } from './fixtures/layout';

/**
 * The signed-in account pages in a browser: which frame each renders in, the
 * one change-password form on Security, and revoking a session. The markup
 * (autocomplete, minlength, the table's headers) is pinned by the Rust tests
 * beside `userportal/pages/*.rs` and `auth_ui/pages/orgs.rs`.
 *
 * Every test signs up its own account, so changing its password or revoking
 * its sessions touches nothing another spec relies on. Part of `e2e:writes`,
 * which CI runs against its own fresh server.
 */

const PASSWORD = 'account-pages-horse-1';

/** A fresh account, signed in in `page`'s browser context. `email` defaults
 * to a unique short address. */
async function signUpAndSignIn(
  page: Page,
  request: APIRequestContext,
  email = `account-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.com`,
): Promise<string> {
  const signup = await request.post('/b/auth/api/signup', {
    data: { email, password: PASSWORD },
    headers: { 'Content-Type': 'application/json' },
  });
  expect(signup.status(), await signup.text()).toBe(201);
  // Through the page's own request context, so the session cookie lands in
  // the browser that then navigates.
  const login = await page.request.post('/b/auth/api/login', {
    data: { email, password: PASSWORD },
    headers: { 'Content-Type': 'application/json' },
  });
  expect(login.status(), await login.text()).toBe(200);
  return email;
}

test.describe('account pages', () => {
  test('every account page renders in the portal shell', async ({ page, request }) => {
    await signUpAndSignIn(page, request);
    for (const [path, title] of [
      ['/b/userportal/', 'Overview'],
      ['/b/userportal/profile', 'Profile'],
      ['/b/userportal/security', 'Security'],
      ['/b/userportal/sessions', 'Sessions'],
      ['/b/auth/orgs', 'Organizations'],
    ] as const) {
      await page.goto(path, { waitUntil: 'networkidle' });
      await expect(page.locator('nav.sidebar'), path).toBeVisible();
      await expect(page.getByRole('heading', { level: 1 }), path).toHaveText(title);
      await expect(page.locator('.auth-split'), path).toHaveCount(0);
      await expect(page.locator('.account-card'), path).toHaveCount(0);
    }
    // Profile is one card and Security one per section, each a
    // `components::Card` on the page background.
    await page.goto('/b/userportal/profile', { waitUntil: 'networkidle' });
    await expect(page.locator('.account-sections > .card')).toHaveCount(1);
    await page.goto('/b/userportal/security', { waitUntil: 'networkidle' });
    await expect(page.locator('.account-sections > .card .card__title')).toHaveText([
      'Password',
      'Email verification',
      'Linked accounts',
    ]);
  });

  test('the old change-password address lands on Security', async ({ page, request }) => {
    await signUpAndSignIn(page, request);
    await page.goto('/b/auth/change-password', { waitUntil: 'networkidle' });
    await expect(page).toHaveURL(/\/b\/userportal\/security$/);
    await expect(page.getByRole('heading', { level: 1 })).toHaveText('Security');
  });

  test('signed-out pages keep the auth split', async ({ page }) => {
    for (const path of ['/b/auth/login', '/b/auth/signup']) {
      await page.goto(path, { waitUntil: 'networkidle' });
      await expect(page.locator('.auth-split'), path).toBeVisible();
      await expect(page.locator('nav.sidebar'), path).toHaveCount(0);
    }
  });

  test('changing the password on Security', async ({ page, request, baseURL }) => {
    const email = await signUpAndSignIn(page, request);
    await page.goto('/b/userportal/security', { waitUntil: 'networkidle' });

    const current = page.getByLabel('Current password', { exact: true });
    const next = page.getByLabel('New password', { exact: true });
    const confirm = page.getByLabel('Confirm new password', { exact: true });
    const submit = page.getByRole('button', { name: 'Change password' });
    const result = page.locator('#change-password-result');

    // Too short: the browser stops it with the configured minimum, so no
    // request is made and nothing is said in the result slot.
    await current.fill(PASSWORD);
    await next.fill('short');
    await confirm.fill('short');
    await submit.click();
    expect(await next.evaluate((el: HTMLInputElement) => el.validity.tooShort)).toBe(true);
    await expect(result).toBeEmpty();

    // A confirmation that differs.
    await next.fill('account-pages-horse-2');
    await confirm.fill('account-pages-horse-3');
    await submit.click();
    await expect(result.getByRole('alert')).toHaveText('New passwords do not match.');
    await expect(confirm).toBeFocused();
    await expect(confirm).toHaveAttribute('aria-invalid', 'true');

    // A wrong current password.
    await current.fill('not-the-password-at-all');
    await confirm.fill('account-pages-horse-2');
    await submit.click();
    await expect(result.getByRole('alert')).toHaveText('Current password is incorrect');
    await expect(current).toBeFocused();

    // Success: the form is replaced by the confirmation and the way back in.
    await current.fill(PASSWORD);
    await submit.click();
    const done = page.locator('#change-password-form');
    await expect(done.getByRole('status')).toContainText('Password changed.');
    await expect(page.locator('form#change-password-form')).toHaveCount(0);
    const signInAgain = page.getByRole('link', { name: 'Sign in again' });
    await expect(signInAgain).toBeVisible();
    await expect(signInAgain).toBeFocused();

    // Signed out for real: the page's own session no longer opens Security.
    await page.reload({ waitUntil: 'networkidle' });
    await expect(page).toHaveURL(/\/b\/auth\/login/);

    // The new password is the one that signs in now.
    const fresh = await apiRequest.newContext({ baseURL });
    const signIn = await fresh.post('/b/auth/api/login', {
      data: { email, password: 'account-pages-horse-2' },
      headers: { 'Content-Type': 'application/json' },
    });
    expect(signIn.status()).toBe(200);
    await fresh.dispose();
  });

  test('revoking another session removes it', async ({ page, request, baseURL }) => {
    const email = await signUpAndSignIn(page, request);
    // Another device: a sign-in from a context that holds no session of its
    // own (one carrying a cookie would be CSRF-checked).
    const device = await apiRequest.newContext({ baseURL });
    const other = await device.post('/b/auth/api/login', {
      data: { email, password: PASSWORD },
      headers: { 'Content-Type': 'application/json' },
    });
    expect(other.status(), await other.text()).toBe(200);
    await device.dispose();

    await page.goto('/b/userportal/sessions', { waitUntil: 'networkidle' });
    const rows = page.locator('.data-table tbody tr');
    const before = await rows.count();
    expect(before).toBeGreaterThanOrEqual(2);
    const current = rows.filter({ hasText: 'Current session' });
    await expect(current).toHaveCount(1);
    const target = rows.filter({ hasNotText: 'Current session' }).first();

    page.once('dialog', (dialog) => {
      expect(dialog.message()).toContain('Revoke this session?');
      void dialog.accept();
    });
    await target.getByRole('button', { name: 'Revoke' }).click();
    await expect(rows).toHaveCount(before - 1);
    await expect(current).toHaveCount(1);

    // Revoked for real: the list after a reload agrees.
    await page.reload({ waitUntil: 'networkidle' });
    await expect(rows).toHaveCount(before - 1);
  });

  test('revoking your own session signs you out on this device', async ({ page, request }) => {
    await signUpAndSignIn(page, request);
    await page.goto('/b/userportal/sessions', { waitUntil: 'networkidle' });
    const current = page.locator('.data-table tbody tr').filter({ hasText: 'Current session' });
    await expect(current).toHaveCount(1);

    page.once('dialog', (dialog) => {
      expect(dialog.message()).toContain('You will be signed out on this device');
      void dialog.accept();
    });
    await current.getByRole('button', { name: 'Revoke' }).click();
    await expect(page).toHaveURL(/\/b\/auth\/login/);

    // And it stays that way: the account pages send this browser to sign in.
    await page.goto('/b/userportal/sessions', { waitUntil: 'networkidle' });
    await expect(page).toHaveURL(/\/b\/auth\/login/);
  });

  /** The seeded Security, Sessions and Organizations pages' bodies, by name
   * (`<!-- page: name -->` lines): linked accounts, OAuth sessions and
   * claimed orgs, which this server cannot make. Kept equal to what the pages
   * render by `the_account_fixture_is_what_the_pages_render`. */
  const ACCOUNT_FIXTURE = Object.fromEntries(
    readFileSync(new URL('./fixtures/account-pages.html', import.meta.url), 'utf8')
      .split(/^<!-- page: ([a-z-]+) -->$/m)
      .slice(1)
      .reduce<string[][]>((pairs, part, i, all) => (i % 2 === 0 ? [...pairs, [part, all[i + 1]]] : pairs), []),
  );

  /** A session's date pairs ("Last used …") that wrap inside themselves:
   * the line under the method may break between them, never within one. */
  const brokenDates = (page: Page) =>
    page.evaluate(() =>
      [...document.querySelectorAll<HTMLElement>('.session-table__meta > span')]
        .filter((s) => {
          // One box per line the span sits on (and one per atomic child on
          // the same line): a box lower than the first is a second line.
          const lines = [...s.getClientRects()];
          return s.checkVisibility() && lines.some((r) => r.top > lines[0].top + 2);
        })
        .map((s) => s.textContent),
    );

  const PRIMARIES: Record<string, string[]> = {
    '/b/userportal/': [],
    '/b/userportal/profile': ['Save'],
    '/b/userportal/security': ['Change password'],
    '/b/userportal/sessions': [],
    '/b/auth/orgs': [],
  };
  // One account for every width: each sign-up and sign-in counts against
  // the server's rate limit, which the rest of `e2e:writes` shares.
  test("the account pages fit their frame from the phone to 1280px with a real account's values", async ({
    page,
    request,
    baseURL,
  }) => {
    // A mailbox with no break opportunity, wider than a phone's card, and
    // a URL pasted in as the display name.
    const email = `accountspayable.northwindtraders.wellington${Date.now()}${Math.floor(Math.random() * 1e6)}@example.com`;
    await signUpAndSignIn(page, request, email);
    for (let i = 0; i < 2; i++) {
      const device = await apiRequest.newContext({ baseURL });
      const other = await device.post('/b/auth/api/login', {
        data: { email, password: PASSWORD },
        headers: { 'Content-Type': 'application/json' },
      });
      expect(other.status(), await other.text()).toBe(200);
      await device.dispose();
    }
    await page.goto('/b/userportal/profile', { waitUntil: 'networkidle' });
    await page.getByLabel('Display name').fill(
      'https://northwind-traders-production.example.com/team/margaret-vandenberg-oyelaran',
    );
    await Promise.all([
      page.waitForResponse((r) => r.url().endsWith('/b/userportal/update-profile') && r.status() === 303),
      page.getByRole('button', { name: 'Save' }).click(),
    ]);
    await page.waitForLoadState('networkidle');

    const found: Record<string, unknown> = {};
    for (const width of WIDTHS) {
      await page.setViewportSize({ width, height: 900 });
      for (const [path, primaries] of Object.entries(PRIMARIES)) {
        await page.goto(path, { waitUntil: 'networkidle' });
        const r = await layout(page);
        const dates = await brokenDates(page);
        if (isWrong(r, primaries) || dates.length) found[`${path} at ${width}`] = { ...r, dates };
      }

      // The seeded pages, each mounted alone in this page's content column.
      for (const [name, html] of Object.entries(ACCOUNT_FIXTURE)) {
        await page.locator('main#content').evaluate((main, body) => {
          main.innerHTML = body;
        }, html);
        const r = await layout(page);
        const dates = await brokenDates(page);
        // A linked account's row lines up with its card's title: the list
        // sits at the card's inset, with none of its own.
        const indent = await page.evaluate(() =>
          [...document.querySelectorAll<HTMLElement>('.linked-provider__name')].map((name) =>
            Math.round(name.getBoundingClientRect().left - name.closest('.card')!.querySelector('.card__title')!.getBoundingClientRect().left),
          ),
        );
        if (r.page > 0 || r.tables.length || r.past.length || r.cards.length || dates.length || indent.some((d) => d !== 0)) {
          found[`${name} at ${width}`] = { ...r, dates, indent };
        }
      }
    }
    expect(Object.keys(ACCOUNT_FIXTURE)).toEqual(['security', 'sessions', 'orgs']);
    expect(found, 'a page scrolled sideways, a card overflowed or the primaries differ').toEqual({});
  });
});
