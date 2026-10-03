import { test, expect, type Page, type Request } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * Saving a settings form with a gated section (`SettingsSection::gated_by`):
 * fields behind a switched-off gate are not posted, so the server neither
 * validates nor overwrites them (`save_settings` leaves a key the body does
 * not carry unchanged — pinned in Rust by
 * `save_settings_leaves_a_key_the_body_does_not_carry_unvalidated_and_unchanged`).
 *
 * The POST is captured and answered in the browser: what is under test here
 * is what the page SENDS. Part of `e2e:writes` regardless, with the other
 * specs that drive settings forms.
 */
test.describe('settings form', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  const BAD = 'https://10.0.0.1/callback';
  const URI_KEY = 'IMPRESSPRESS__AUTH_UI__OAUTH_REDIRECT_URI';
  const GATE_KEY = 'WAFER_RUN_SHARED__ENABLE_OAUTH';

  /** Click Save and return the JSON body the form posted. */
  async function saved(page: Page): Promise<Record<string, string>> {
    const posted = page.waitForRequest(
      (r: Request) => r.url().endsWith('/b/auth/admin/settings') && r.method() === 'POST',
    );
    await page.getByRole('button', { name: 'Save settings' }).click();
    return (await posted).postDataJSON() as Record<string, string>;
  }

  test('a field behind a switched-off gate is not posted', async ({ page }) => {
    await loginAsAdmin(page);
    await page.route('**/b/auth/admin/settings', (route) =>
      route.request().method() === 'POST'
        ? route.fulfill({ status: 200, json: { message: 'Settings saved' } })
        : route.continue(),
    );
    await page.goto('/b/auth/admin/settings', { waitUntil: 'networkidle' });
    const oauth = page.getByRole('switch', { name: 'Enable OAuth' });
    const uri = page.getByLabel('OAuth Redirect URI');

    // Switched on, the field is posted with what it holds.
    await oauth.check();
    await uri.fill(BAD);
    let body = await saved(page);
    expect(body[GATE_KEY]).toBe('true');
    expect(body[URI_KEY]).toBe(BAD);

    // Switched off, it is not posted at all — so it cannot fail the save or
    // overwrite the stored value; the switch itself still is.
    await oauth.uncheck();
    await expect(uri).toBeHidden();
    body = await saved(page);
    expect(body[GATE_KEY]).toBe('false');
    expect(URI_KEY in body).toBe(false);
    // Fields outside the gated region are unaffected.
    expect('WAFER_RUN_SHARED__POST_LOGIN_REDIRECT' in body).toBe(true);

    // Back on, the value typed is still in the field.
    await oauth.check();
    await expect(uri).toHaveValue(BAD);
  });
});
