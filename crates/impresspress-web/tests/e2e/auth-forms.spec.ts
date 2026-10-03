import { test, expect } from '@playwright/test';

/**
 * The signed-out auth forms in a browser: what a click does. The markup
 * (roles, autocomplete, minlength) is pinned by the Rust page tests beside
 * `auth_ui/pages/login.rs` and `signup.rs`.
 *
 * Part of `e2e:writes`, which CI runs against its own fresh server: a failed
 * sign-in is a 401 the admin dashboard counts, and the visual baselines are
 * captured on a server nothing has written to.
 */
test.describe('auth forms', () => {
  test('a failed sign-in is announced as an alert', async ({ page }) => {
    await page.goto('/b/auth/login', { waitUntil: 'networkidle' });
    await expect(page.getByRole('alert')).toBeHidden();

    await page.getByLabel('Email').fill('admin@example.com');
    await page.getByLabel('Password', { exact: true }).fill('not-the-password');
    await page.getByRole('button', { name: 'Sign In' }).click();

    const alert = page.getByRole('alert');
    await expect(alert).toBeVisible();
    await expect(alert).toHaveText(/invalid email or password/i);
  });

  test('the reveal toggle shows the password and reports it as pressed', async ({ page }) => {
    await page.goto('/b/auth/login', { waitUntil: 'networkidle' });
    const password = page.getByLabel('Password', { exact: true });
    const toggle = page.getByRole('button', { name: 'Show password' });

    // Nothing typed, nothing to show.
    await expect(toggle).toBeHidden();
    await password.fill('secret');
    await expect(toggle).toHaveAttribute('aria-pressed', 'false');
    await toggle.click();
    await expect(password).toHaveAttribute('type', 'text');
    await expect(toggle).toHaveAttribute('aria-pressed', 'true');
    await toggle.click();
    await expect(password).toHaveAttribute('type', 'password');
  });
});
