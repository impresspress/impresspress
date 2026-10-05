import { test, expect, request as apiRequest, type APIRequestContext, type Page } from '@playwright/test';

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

/** A fresh account, signed in in `page`'s browser context. */
async function signUpAndSignIn(page: Page, request: APIRequestContext): Promise<string> {
  const email = `account-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.com`;
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
});
