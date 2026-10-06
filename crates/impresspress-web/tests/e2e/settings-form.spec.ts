import { test, expect, type Page, type Response } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * Saving the shared settings forms (`ui::settings_form`) for real, end to end.
 *
 * Every settings form saves through the admin block — the one WRAP lets
 * write shared (`WAFER_RUN_SHARED__*`) and block-scoped keys alike.
 * Authentication and Branding are admin Settings tabs; Legal's settings page
 * stays in the Legal section and posts to `/b/admin/settings/legal`. Each test saves through the live server,
 * reloads, and reads the stored value back off the page — then puts the
 * original value back, so the rest of `e2e:writes` sees the defaults.
 *
 * The gated section (`SettingsSection::gated_by`): fields behind a
 * switched-off gate are not posted, so the server neither validates nor
 * overwrites them — pinned in Rust by
 * `save_settings_leaves_a_key_the_body_does_not_carry_unvalidated_and_unchanged`.
 */
test.describe('settings form', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  const AUTHENTICATION = '/b/admin/settings/authentication';
  const BRANDING = '/b/admin/settings/branding';
  const BAD = 'https://10.0.0.1/callback';
  const URI_KEY = 'IMPRESSPRESS__AUTH_UI__OAUTH_REDIRECT_URI';
  const GATE_KEY = 'WAFER_RUN_SHARED__ENABLE_OAUTH';

  /** Click Save; return the body the form posted and the server's answer. */
  async function save(
    page: Page,
    url: string,
  ): Promise<{ body: Record<string, string>; response: Response }> {
    const answered = page.waitForResponse(
      (r) => r.url().endsWith(url) && r.request().method() === 'POST',
    );
    await page.getByRole('button', { name: 'Save settings' }).click();
    const response = await answered;
    return { body: response.request().postDataJSON() as Record<string, string>, response };
  }

  test('the old auth settings URL redirects to Settings › Authentication', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/auth/admin/settings', { waitUntil: 'networkidle' });
    await expect(page).toHaveURL(AUTHENTICATION);
    await expect(page.locator('nav.sidebar')).toHaveAttribute('data-nav', 'admin');
  });

  test('Authentication saves, and a field behind a switched-off gate is not posted', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.goto(AUTHENTICATION, { waitUntil: 'networkidle' });
    const oauth = page.getByRole('switch', { name: 'Enable OAuth' });
    const uri = page.getByLabel('OAuth Redirect URI');
    const redirect = page.getByLabel('Post-Login Redirect');
    const original = await redirect.inputValue();

    // Switched on, the field is posted with what it holds — and a private
    // address is refused by the server, naming the key, before any write.
    await oauth.check();
    await uri.fill(BAD);
    let saved = await save(page, AUTHENTICATION);
    expect(saved.body[GATE_KEY]).toBe('true');
    expect(saved.body[URI_KEY]).toBe(BAD);
    expect(saved.response.status()).toBe(400);
    expect(await saved.response.text()).toContain(URI_KEY);

    // Switched off, it is not posted at all, so it cannot fail the save; the
    // rest of the form saves through the admin block.
    await oauth.uncheck();
    await expect(uri).toBeHidden();
    await redirect.fill('/b/userportal/');
    saved = await save(page, AUTHENTICATION);
    expect(saved.body[GATE_KEY]).toBe('false');
    expect(URI_KEY in saved.body).toBe(false);
    expect(saved.response.status()).toBe(200);
    expect((await saved.response.json()).message).toBe('Settings saved');

    // Stored: a fresh render shows it.
    await page.reload({ waitUntil: 'networkidle' });
    await expect(redirect).toHaveValue('/b/userportal/');

    // Put it back.
    await redirect.fill(original);
    saved = await save(page, AUTHENTICATION);
    expect(saved.response.status()).toBe(200);
  });

  test('Branding saves through the admin block, and the old URL redirects there', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.goto('/b/userportal/admin/settings', { waitUntil: 'networkidle' });
    await expect(page).toHaveURL(BRANDING);

    const name = page.getByLabel('App Name');
    const original = await name.inputValue();
    await name.fill('Acme Settings E2E');
    let saved = await save(page, BRANDING);
    expect(saved.response.status()).toBe(200);
    expect((await saved.response.json()).message).toBe('Settings saved');

    await page.reload({ waitUntil: 'networkidle' });
    await expect(name).toHaveValue('Acme Settings E2E');
    // The chrome reads the stored name.
    await expect(page.locator('.sidebar__brand-name')).toHaveText('Acme Settings E2E');

    await name.fill(original);
    saved = await save(page, BRANDING);
    expect(saved.response.status()).toBe(200);
  });

  test('Legal settings save through the admin block', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/legalpages/admin/settings', { waitUntil: 'networkidle' });
    const back = page.getByLabel('Back Button URL');
    const original = await back.inputValue();
    await back.fill('/b/userportal/');
    let saved = await save(page, '/b/admin/settings/legal');
    expect(saved.response.status()).toBe(200);
    expect((await saved.response.json()).message).toBe('Settings saved');

    await page.reload({ waitUntil: 'networkidle' });
    await expect(back).toHaveValue('/b/userportal/');

    await back.fill(original);
    saved = await save(page, '/b/admin/settings/legal');
    expect(saved.response.status()).toBe(200);
  });
});
