import { test, expect } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * The core admin pages' own controls, in a real browser: the network page's
 * route expansion by keyboard (for a path whose characters broke the old
 * selector-built id), the variables search, and the logs page's two error
 * filters.
 *
 * Each case makes refused (4xx) requests of its own, which the dashboard
 * counts as client errors — so this is part of `e2e:writes`, which CI runs on
 * its own server, not the one the visual baselines are taken on.
 */
test.describe('core admin pages', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  test('a route with "." and ":" in its path expands and collapses from the keyboard', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    // Matches the variable-edit route's template, so the log keeps the path
    // rather than collapsing it to the unmatched label; no such variable, so
    // each answer is a 404.
    const path = `/b/admin/variables/E2E.NET:${Date.now()}/edit`;
    for (let i = 0; i < 2; i++) {
      const res = await page.request.get(path);
      expect(res.status()).toBe(404);
    }

    await page.goto(`/b/admin/settings/network?search=${encodeURIComponent(path)}`, {
      waitUntil: 'networkidle',
    });
    await expect(page.locator('tr.network-row')).toHaveCount(1);
    const toggle = page.locator('tr.network-row button[data-action="network-detail"]');
    await expect(toggle).toHaveAttribute('aria-expanded', 'false');
    const detail = page.locator(`#${await toggle.getAttribute('aria-controls')}`);
    await expect(detail).toBeHidden();

    await toggle.focus();
    await page.keyboard.press('Enter');
    await expect(toggle).toHaveAttribute('aria-expanded', 'true');
    await expect(detail).toBeVisible();
    // The detail fragment loaded for this path: both requests, both 404s.
    await expect(detail.locator('tbody tr')).toHaveCount(2);
    await expect(detail).toContainText('404');

    await page.keyboard.press('Space');
    await expect(toggle).toHaveAttribute('aria-expanded', 'false');
    await expect(detail).toBeHidden();
  });

  test('the network group heading collapses its routes', async ({ page }) => {
    await loginAsAdmin(page);
    await page.request.get('/b/admin/network/detail/inbound?method=GET&path=/e2e-group');
    await page.goto('/b/admin/settings/network', { waitUntil: 'networkidle' });
    const group = page.locator('.network-group').first();
    const head = group.locator('button[data-action="network-group-toggle"]');
    const body = page.locator(`#${await head.getAttribute('aria-controls')}`);
    await expect(body).toBeVisible();
    await head.click();
    await expect(head).toHaveAttribute('aria-expanded', 'false');
    await expect(body).toBeHidden();
  });

  test('the variables search finds a key in any group and restores the groups when cleared', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/settings/variables', { waitUntil: 'networkidle' });

    const groups = page.locator('[data-var-group]');
    expect(await groups.count()).toBeGreaterThan(1);
    const shared = groups.first();
    await expect(shared).toHaveAttribute('open', '');
    const closedBefore = await page.locator('[data-var-group]:not([open])').count();
    expect(closedBefore, 'per-block groups start closed').toBeGreaterThan(0);

    const search = page.getByRole('searchbox', { name: 'Search variables by key, name or description' });
    await search.fill('APP_NAME');
    await expect(page.getByRole('button', { name: 'Edit WAFER_RUN_SHARED__APP_NAME' })).toBeVisible();
    await expect(page.locator('[data-var-group]:not([hidden])')).toHaveCount(1);
    await expect(page.locator('#var-filter-empty')).toBeHidden();

    await search.fill('zzz-no-such-variable');
    await expect(page.locator('#var-filter-empty')).toBeVisible();
    await expect(page.locator('[data-var-group]:not([hidden])')).toHaveCount(0);

    await search.fill('');
    await expect(page.locator('[data-var-group]:not([hidden])')).toHaveCount(await groups.count());
    await expect(page.locator('[data-var-group]:not([open])')).toHaveCount(closedBefore);
    await expect(page.locator('#var-filter-empty')).toBeHidden();
  });

  test('the logs page filters server and client errors through two pressed toggles', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    const missing = `/b/admin/variables/E2E_LOGS_${Date.now()}/edit`;
    expect((await page.request.get(missing)).status()).toBe(404);

    await page.goto('/b/admin/logs', { waitUntil: 'networkidle' });
    const server = page.getByRole('button', { name: 'Server errors' });
    const client = page.getByRole('button', { name: 'Client errors' });
    await expect(server).toHaveAttribute('aria-pressed', 'false');
    await expect(client).toHaveAttribute('aria-pressed', 'false');

    await client.click();
    await expect(page).toHaveURL(/[?&]client_errors=1/);
    await expect(page.getByRole('button', { name: 'Client errors' })).toHaveAttribute('aria-pressed', 'true');
    await expect(page.locator('#content')).toContainText(missing.split('/')[4]);
    // Every listed status is a 4xx.
    const statuses = await page.locator('td[data-label="Status"]').allTextContents();
    expect(statuses.length).toBeGreaterThan(0);
    for (const status of statuses) expect(Number(status.trim())).toBeGreaterThanOrEqual(400);
    for (const status of statuses) expect(Number(status.trim())).toBeLessThan(500);

    await page.getByRole('button', { name: 'Client errors' }).click();
    await expect(page).not.toHaveURL(/client_errors/);
    await expect(page.getByRole('button', { name: 'Client errors' })).toHaveAttribute('aria-pressed', 'false');
  });
});
