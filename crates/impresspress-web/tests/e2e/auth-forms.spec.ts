import { test, expect } from '@playwright/test';

/**
 * The signed-out auth forms' announcements and password-field semantics.
 *
 * NOT part of `e2e:visual`: a failed sign-in is a 401 the admin dashboard
 * counts as an error, and the visual baselines expect a run with none. CI
 * runs this after the baselines, beside `htmx-success-effects.spec.ts`.
 */
test.describe('auth forms', () => {
  test('a failed sign-in is announced as an alert', async ({ page }) => {
    await page.goto('/b/auth/login', { waitUntil: 'networkidle' });

    // The live region is there before anything goes wrong, and silent.
    await expect(page.getByRole('alert')).toBeHidden();

    await page.getByLabel('Email').fill('admin@example.com');
    await page.getByLabel('Password', { exact: true }).fill('not-the-password');
    await page.getByRole('button', { name: 'Sign In' }).click();

    const alert = page.getByRole('alert');
    await expect(alert).toBeVisible();
    await expect(alert).toHaveText(/invalid email or password/i);
  });

  test('password fields say what they hold, and the reveal is a toggle button', async ({ page }) => {
    await page.goto('/b/auth/login', { waitUntil: 'networkidle' });
    const current = page.getByLabel('Password', { exact: true });
    await expect(current).toHaveAttribute('autocomplete', 'current-password');
    await expect(current).not.toHaveAttribute('minlength', /.*/);

    await current.fill('secret');
    const toggle = page.getByRole('button', { name: 'Show password' });
    await expect(toggle).toHaveAttribute('aria-pressed', 'false');
    await toggle.click();
    await expect(current).toHaveAttribute('type', 'text');
    await expect(toggle).toHaveAttribute('aria-pressed', 'true');

    await page.goto('/b/auth/signup', { waitUntil: 'networkidle' });
    const chosen = page.getByLabel('Password', { exact: true });
    await expect(chosen).toHaveAttribute('autocomplete', 'new-password');
    // The server's default minimum (WAFER_RUN_SHARED__AUTH__PASSWORD_MIN_LENGTH).
    await expect(chosen).toHaveAttribute('minlength', '8');
  });
});
