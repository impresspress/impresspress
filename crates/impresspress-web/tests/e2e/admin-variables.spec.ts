import { test, expect, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * The Add Variable modal's key rule, in a real browser.
 *
 * The server refuses a malformed or unnamespaced variable key with a 400 and a
 * taken one with a 409 (`config_vars::check_variable_key`,
 * `admin::ops::create_variable`). htmx swaps only a 2xx, so the modal's own
 * request gets the form back with the sentence under Key — and these cases
 * prove that lands where a person sees it: under the field, announced, with
 * the field marked invalid, the modal still open, and no toast instead.
 *
 * Writes nothing, but every submit is a refused (4xx) request, which the
 * admin dashboard counts as an error — so it is part of `e2e:writes`, which
 * CI runs on its own server, not the one the visual baselines are taken on.
 */
test.describe('admin variables', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  async function openAddVariable(page: Page) {
    await loginAsAdmin(page);
    await page.goto('/b/admin/variables', { waitUntil: 'networkidle' });
    await page.locator('[data-action="modal-open"][data-modal-target="create-var"]').click();
    await expect(page.locator('#create-var')).toBeVisible();
  }

  test("the key field's pattern agrees with the server's rule", async ({ page }) => {
    await openAddVariable(page);
    const key = page.locator('#var-key');

    // Mirrors `config_vars::variable_key_tests`: the browser's copy of the
    // rule must reach the same verdict as the server's on both sides of it.
    const valid = [
      'WAFER_RUN_SHARED__APP_NAME',
      'WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD',
      'WAFER_RUN__AUTH__JWT_SECRET',
      'IMPRESSPRESS__EMAIL__MAILGUN_API_KEY',
      'MY_ORG__MY_BLOCK__SETTING_2',
      'ACME__S3__2FA_REQUIRED',
      // The runtime accepts an org or block that starts with a digit.
      '3D__VIEWER__X',
      '1ACME__BLOCK__NAME',
    ];
    const invalid = [
      'bad key!',
      'BAD KEY!',
      'IMPRESSPRESS__EMAIL__from',
      'IMPRESSPRESS__EMAIL__FROM-ADDRESS',
      '_ACME__BLOCK__NAME',
      'MY_SETTING',
      'WAFER_RUN_SHARED',
      'IMPRESSPRESS__EMAIL',
      'ACME___BLOCK__NAME',
      'ACME__BLOCK__NAME_',
      'ACME____NAME',
      'WAFER_RUN_SHARED__',
      'IMPRESSPRESS_RUN_MIGRATIONS',
      '__IMPRESSPRESS_RUNTIME_KIND__',
    ];
    for (const [value, mismatch] of [
      ...valid.map((v) => [v, false] as const),
      ...invalid.map((v) => [v, true] as const),
    ]) {
      await key.fill(value);
      expect(
        await key.evaluate((el: HTMLInputElement) => el.validity.patternMismatch),
        value,
      ).toBe(mismatch);
    }
    await expect(key).toHaveAttribute('autocapitalize', 'characters');
    await expect(key).toHaveAttribute('aria-describedby', 'var-key-hint');
    await expect(page.locator('#var-key-hint')).toBeVisible();
  });

  test('a taken key is shown under the Key field, not in a toast', async ({ page }) => {
    await openAddVariable(page);
    const modal = page.locator('#create-var');

    // Seeded on every deployment by `seed_defaults`, so the create is a 409
    // and nothing is written.
    await page.locator('#var-key').fill('WAFER_RUN_SHARED__APP_NAME');
    await page.locator('#var-value').fill('Not saved');
    await modal.getByRole('button', { name: 'Create' }).click();

    const error = page.locator('#var-key-error');
    await expect(error).toHaveText(
      'A variable with the key "WAFER_RUN_SHARED__APP_NAME" already exists. Choose a different key.',
    );
    await expect(error).toHaveAttribute('role', 'alert');
    const key = page.locator('#var-key');
    await expect(key).toHaveAttribute('aria-invalid', 'true');
    await expect(key).toHaveAttribute('aria-describedby', 'var-key-hint var-key-error');
    await expect(key).toHaveValue('WAFER_RUN_SHARED__APP_NAME');
    await expect(key).toBeFocused();
    await expect(page.locator('#var-value')).toHaveValue('Not saved');
    await expect(modal).toBeVisible();
    await expect(page.locator('#toast-container .toast')).toHaveCount(0);
  });

  test("a malformed key that skips the browser's check is refused under the Key field", async ({
    page,
  }) => {
    await openAddVariable(page);
    const modal = page.locator('#create-var');

    // The pattern stops this in the browser; a request that skips it (a
    // stale page, a hand-built post) meets the server's 400 instead, and the
    // modal shows that too.
    await page.locator('#var-key').evaluate((el) => el.removeAttribute('pattern'));
    await page.locator('#var-key').fill('e2e bad key!');
    await modal.getByRole('button', { name: 'Create' }).click();

    const error = page.locator('#var-key-error');
    await expect(error).toContainText('"e2e bad key!" is not a valid variable key');
    await expect(error).toContainText('WAFER_RUN_SHARED__<NAME>');
    await expect(page.locator('#var-key')).toHaveAttribute('aria-invalid', 'true');
    await expect(modal).toBeVisible();
    await expect(page.locator('#toast-container .toast')).toHaveCount(0);

    // The page behind the modal gained no row.
    await modal.locator('button.modal-close').click();
    await page.goto('/b/admin/variables?tab=all', { waitUntil: 'networkidle' });
    await expect(page.getByText('e2e bad key!', { exact: true })).toHaveCount(0);
  });
});
