import { test, expect } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';
import { WIDTHS, isWrong, layout } from './fixtures/layout';

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

  test('the network group heading folds its routes', async ({ page }) => {
    await loginAsAdmin(page);
    await page.request.get('/b/admin/network/detail/inbound?method=GET&path=/e2e-group');
    await page.goto('/b/admin/settings/network', { waitUntil: 'networkidle' });
    const group = page.locator('[data-fold="route"]').first();
    const fold = group.locator('details.group-fold');
    const body = group.locator('.network-routes');
    await expect(fold).toHaveAttribute('open', '');
    await expect(body).toBeVisible();
    await group.locator('summary.group-fold__summary').click();
    await expect(fold).not.toHaveAttribute('open', '');
    await expect(body).toBeHidden();
    // The fold's head still says what it holds.
    await expect(group.locator('.group-fold__stats')).toContainText('request');
  });

  test('the variables search finds a key in any group and restores the groups when cleared', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/settings/variables', { waitUntil: 'networkidle' });

    // Each group is a card (`data-fold`) holding its `<details>`; the filter
    // hides the card, frame and all, and opens or closes the fold.
    const groups = page.locator('[data-fold="variable"]');
    expect(await groups.count()).toBeGreaterThan(1);
    const shared = groups.first().locator('details');
    await expect(shared).toHaveAttribute('open', '');
    const closed = page.locator('[data-fold="variable"] > .card__body > details:not([open])');
    const closedBefore = await closed.count();
    expect(closedBefore, 'per-block groups start closed').toBeGreaterThan(0);

    const search = page.getByRole('searchbox', { name: 'Search variables by key, name or description' });
    await search.fill('APP_NAME');
    await expect(page.getByRole('button', { name: 'Edit WAFER_RUN_SHARED__APP_NAME' })).toBeVisible();
    await expect(page.locator('[data-fold="variable"]:not([hidden])')).toHaveCount(1);
    await expect(page.locator('#var-filter-empty')).toBeHidden();

    await search.fill('zzz-no-such-variable');
    await expect(page.locator('#var-filter-empty')).toBeVisible();
    await expect(page.locator('[data-fold="variable"]:not([hidden])')).toHaveCount(0);

    await search.fill('');
    await expect(page.locator('[data-fold="variable"]:not([hidden])')).toHaveCount(await groups.count());
    await expect(closed).toHaveCount(closedBefore);
    await expect(page.locator('#var-filter-empty')).toBeHidden();
  });

  test('deleting a variable confirms, toasts, and keeps the search the row was found by', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/settings/variables', { waitUntil: 'networkidle' });
    const key = `E2E__CORE__DELETE_${Date.now()}`;
    await page.locator('[data-action="modal-open"][data-modal-target="create-var"]').click();
    await page.locator('#var-key').fill(key);
    await page.locator('#var-value').fill('to be deleted');
    await page.locator('#create-var').getByRole('button', { name: 'Create' }).click();
    await expect(page.locator('#create-var')).toBeHidden();

    const search = page.getByRole('searchbox', { name: 'Search variables by key, name or description' });
    await search.fill(key);
    const remove = page.getByRole('button', { name: `Delete ${key}` });
    await expect(remove).toBeVisible();

    let confirmText = '';
    page.once('dialog', (dialog) => {
      confirmText = dialog.message();
      void dialog.accept();
    });
    await remove.click();
    await expect(page.locator('#toast-container .toast', { hasText: 'Variable deleted' })).toBeVisible();
    expect(confirmText).toBe(`Delete ${key}? This cannot be undone.`);
    await expect(page.getByRole('button', { name: `Delete ${key}` })).toHaveCount(0);
    // The page was redrawn under the search box, which kept its query.
    await expect(page.getByRole('searchbox', { name: 'Search variables by key, name or description' })).toHaveValue(key);
    await expect(page.locator('#var-filter-empty')).toBeVisible();
  });

  /**
   * The grant tables narrow by their own width (table.css `.grant-table`):
   * from 721 to about 1150px, beside the settings rail, a card is too
   * narrow for five or six columns, and the table used to scroll sideways
   * with the custom grant's Delete out of view. Seeds one custom grant
   * through the modal and deletes it again.
   */
  test('every grant table fits its card with Delete in view from 721 to 1100px', async ({ page }) => {
    await loginAsAdmin(page);
    await page.setViewportSize({ width: 1024, height: 900 });
    const description = `E2E widths ${Date.now()}: a description long enough to wrap`;
    await page.goto('/b/admin/settings/permissions?subtab=database', { waitUntil: 'networkidle' });
    await page.locator('[data-action="modal-open"][data-modal-target="add-grant-modal"]').click();
    const dialog = page.locator('dialog#add-grant-modal');
    await dialog.locator('#grantee').selectOption('impresspress/files');
    await dialog.locator('#grant_owner').selectOption('impresspress/admin');
    await dialog.locator('#resource_type').selectOption('db');
    await dialog.locator('#description').fill(description);
    await dialog.getByRole('button', { name: 'Add grant' }).click();
    const row = page.locator('tr', { hasText: description });
    await expect(row).toHaveCount(1);

    try {
      for (const width of [721, 1024, 1100]) {
        await page.setViewportSize({ width, height: 900 });
        for (const path of ['/b/admin/settings/permissions?subtab=database', '/b/admin/settings/permissions']) {
          await page.goto(path, { waitUntil: 'networkidle' });
          const tables = await page.locator('.card .data-table').evaluateAll((all) =>
            all.map((scroller) => {
              const box = scroller.getBoundingClientRect();
              return {
                fits: scroller.querySelector('table')!.getBoundingClientRect().width <= (scroller as HTMLElement).clientWidth + 0.5,
                deleteInView: [...scroller.querySelectorAll('button[aria-label^="Delete grant"]')].every(
                  (b) => b.getBoundingClientRect().right <= box.right + 0.5,
                ),
              };
            }),
          );
          expect(tables.length, `${path} at ${width}`).toBeGreaterThan(0);
          expect(tables.filter((t) => !t.fits || !t.deleteInView), `${path} at ${width}`).toEqual([]);
        }
      }
    } finally {
      await page.setViewportSize({ width: 1280, height: 900 });
      await page.goto('/b/admin/settings/permissions?subtab=database', { waitUntil: 'networkidle' });
      page.once('dialog', (d) => void d.accept());
      await row.locator('button[aria-label^="Delete grant"]').click();
      await expect(row).toHaveCount(0);
    }
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

  /**
   * Portal buttons with what an admin really types: a long label and a
   * pasted URL as the path. At every width from the phone to 1280 the list
   * fits its frame, because the path breaks anywhere (table.css
   * `.portal-button-table`), and Add button is the page's one primary. Adds
   * two buttons and deletes them again.
   */
  test('the portal buttons list fits its frame from the phone to 1280px', async ({ page }) => {
    await loginAsAdmin(page);
    const stamp = Date.now();
    const labels = [
      `Northwind Traders production analytics dashboard ${stamp}`,
      `Lambton Quay opening hours ${stamp}`,
      // A host name as the label: no space to break at.
      `northwind-traders-production.example.com/reports/${stamp}`,
    ];
    await page.goto('/b/userportal/admin/buttons', { waitUntil: 'networkidle' });
    for (const [label, path] of [
      [labels[0], '/b/storage/northwind-traders-production/activities/wellington-cbd-store-lambton-quay/opening-hours'],
      [labels[1], 'https://northwind-traders-production.example.com/activities/wellington-cbd-store-lambton-quay/opening-hours?listing=listing_01HZX9Q2KZp7a9XyTqAbCdEfGhIjKlMnOpQrStUvWx'],
      [labels[2], '/b/products/'],
    ]) {
      await page.locator('#label').fill(label);
      await page.locator('#path').fill(path);
      await page.getByRole('button', { name: 'Add button' }).click();
      await expect(page.locator('#buttons-table tr', { hasText: label })).toHaveCount(1);
    }
    try {
      const found: Record<number, unknown> = {};
      for (const width of WIDTHS) {
        await page.setViewportSize({ width, height: 900 });
        await page.goto('/b/userportal/admin/buttons', { waitUntil: 'networkidle' });
        const r = await layout(page);
        if (isWrong(r, ['Add button'])) found[width] = r;
      }
      expect(found, 'the buttons page scrolled sideways or overflowed its frame').toEqual({});
    } finally {
      await page.setViewportSize({ width: 1280, height: 900 });
      await page.goto('/b/userportal/admin/buttons', { waitUntil: 'networkidle' });
      for (const label of labels) {
        page.once('dialog', (d) => void d.accept());
        await page.getByRole('button', { name: `Delete ${label}` }).click();
        await expect(page.locator('#buttons-table tr', { hasText: label })).toHaveCount(0);
      }
    }
  });

  /**
   * The card a new API key is shown in, named by what an operator pastes
   * into the name: it fits its frame at every width from the phone to 1280,
   * the key breaking anywhere and the name too (card.css `.api-key-value`,
   * `.api-key-name-hint`). Then the list the key joins, after a reload: it
   * fits too, because the name breaks anywhere and the list moves the
   * prefix, user, date and status under the name when it is narrow
   * (table.css `.api-key-table`).
   */
  test('the new API key card and the keys list fit their frame from the phone to 1280px', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/users?tab=api-keys', { waitUntil: 'networkidle' });
    await page.locator('[data-action="modal-open"][data-modal-target="create-api-key"]').first().click();
    const dialog = page.locator('dialog#create-api-key');
    // Named the way a deploy variable is: one word with no break opportunity.
    await dialog.locator('#key-name').fill(`NORTHWIND_TRADERS_PRODUCTION_DEPLOY_KEY_WELLINGTON_CBD_LAMBTON_QUAY_${Date.now()}`);
    await dialog.getByRole('button', { name: 'Create' }).click();
    const card = page.locator('#users-tab-content .card', { hasText: 'Key created' });
    await expect(card).toBeVisible();
    const found: Record<string, unknown> = {};
    for (const width of WIDTHS) {
      await page.setViewportSize({ width, height: 900 });
      const r = await layout(page);
      if (r.page > 0 || r.past.length || r.cards.length) found[`card at ${width}`] = r;
    }
    for (const width of WIDTHS) {
      await page.setViewportSize({ width, height: 900 });
      await page.goto('/b/admin/users?tab=api-keys', { waitUntil: 'networkidle' });
      await expect(page.locator('.api-key-table tbody tr', { hasText: 'NORTHWIND_TRADERS' }).first()).toBeVisible();
      const r = await layout(page);
      if (isWrong(r, ['Create API key'])) found[`list at ${width}`] = r;
    }
    expect(found, 'the key card or the keys list overflowed its frame').toEqual({});
  });
});
