import { expect, request as playwrightRequest, test, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, adminBearer, loginAsAdmin } from './fixtures/auth';
import { WIDTHS, layout, overflows } from './fixtures/layout';
import { visiblePrimaries } from './fixtures/primaries';
import { SHOP_OFFER, uniqueShopProduct } from './fixtures/shop-fixture';

/**
 * The products admin pages, driven in a browser against the real server.
 *
 * - The wizard announces its current step (`aria-current="step"`), and the
 *   marker moves with Continue; at 390px its five steps stay on one row.
 * - A product page has one visible primary, the topbar's (Publish): the
 *   details form's Save, a draft offer's Publish and the revealed pricing
 *   editor's Save are secondary. The wizard shows one primary at every step
 *   (Continue, then the publish button in its place), and the group editor
 *   is a dialog with its own.
 * - The seeded product and group tables fit their frame at every width from
 *   the phone to 1280 (table.css `.product-table`, `.group-table`): the
 *   live list, both Deleted views and My products, with seller products
 *   turned on through the Settings form for the run and back off after.
 * - The Active / Deleted views are filter links, and Manage's search box
 *   sits in the filter row rather than a card of its own.
 * - The portal buttons page's add form collapses to one column at 390px.
 * - A product without pricing adds one from its empty pricing state: Add a
 *   price opens the offer editor, Create price saves a draft offer, and the
 *   draft publishes.
 *
 * It seeds two products (one with a draft offer, one without pricing) over
 * the API, creates and publishes an offer through the page, and deletes both
 * products again at the end, so it is part of `e2e:writes`, which CI runs
 * against its own fresh server.
 */

test.use({ storageState: ADMIN_STATE_PATH });

/** A real word with no break opportunity, wider than any table's narrowest
 * name column (a seller can name a product anything). */
const WORST_WORD = 'Wolfeschlegelsteinhausenbergerdorff';

test.describe.serial('products admin pages', () => {
  const stamp = `pages${Date.now().toString(36)}`;
  const product = { ...uniqueShopProduct(stamp), status: 'draft' };
  // Its own stamp, not one extending `stamp`: Manage's search for `stamp`
  // must match `product` alone.
  const unpriced = { ...uniqueShopProduct(`bare${Date.now().toString(36)}`), status: 'draft' };
  let productId = '';
  let unpricedId = '';

  test.beforeAll(async ({ baseURL }) => {
    const api = await playwrightRequest.newContext({
      baseURL,
      storageState: { cookies: [], origins: [] },
    });
    const headers = { Authorization: await adminBearer(api), 'Content-Type': 'application/json' };
    const created = await api.post('/b/products/api/admin/products', { headers, data: product });
    expect(created.status(), await created.text()).toBe(200);
    const body = (await created.json()) as { id?: string; data?: { id?: string } };
    productId = (body.id ?? body.data?.id) as string;
    expect(productId, JSON.stringify(body)).toBeTruthy();
    const offer = await api.post(`/b/products/api/admin/products/${productId}/offers`, {
      headers,
      data: SHOP_OFFER,
    });
    expect(offer.status(), await offer.text()).toBe(200);
    const bare = await api.post('/b/products/api/admin/products', { headers, data: unpriced });
    expect(bare.status(), await bare.text()).toBe(200);
    const bareBody = (await bare.json()) as { id?: string; data?: { id?: string } };
    unpricedId = (bareBody.id ?? bareBody.data?.id) as string;
    expect(unpricedId, JSON.stringify(bareBody)).toBeTruthy();
    await api.dispose();
  });

  test.afterAll(async ({ baseURL }) => {
    const api = await playwrightRequest.newContext({
      baseURL,
      storageState: { cookies: [], origins: [] },
    });
    const headers = { Authorization: await adminBearer(api) };
    await api.delete(`/b/products/api/admin/products/${productId}`, { headers });
    await api.delete(`/b/products/api/admin/products/${unpricedId}`, { headers });
    await api.dispose();
  });

  test('the wizard marks its current step and moves the marker on Continue', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/products/admin/new', { waitUntil: 'networkidle' });
    const steps = page.getByRole('navigation', { name: 'Product setup progress' }).locator('li');
    await expect(steps).toHaveCount(5);
    await expect(steps.nth(0)).toHaveAttribute('aria-current', 'step');
    await expect(page.locator('[aria-current="step"]')).toHaveCount(1);

    await page.getByRole('button', { name: 'Continue' }).click();
    await expect(steps.nth(1)).toHaveAttribute('aria-current', 'step');
    await expect(steps.nth(0)).not.toHaveAttribute('aria-current', /.*/);
    await expect(page.locator('[aria-current="step"]')).toHaveCount(1);
    // The finished step keeps its layout classes and shows its check mark.
    await expect(steps.nth(0)).toHaveClass(/badge--center/);
    await expect(steps.nth(0).locator('.wizard-step-check')).toBeVisible();
  });

  test('at 390px the wizard steps fit one row', async ({ page }) => {
    await loginAsAdmin(page);
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto('/b/products/admin/new', { waitUntil: 'networkidle' });
    const tops = await page
      .locator('.product-wizard-progress li')
      .evaluateAll((items) => items.map((li) => Math.round(li.getBoundingClientRect().top)));
    expect(new Set(tops).size, JSON.stringify(tops)).toBe(1);
    // The current step's name is shown; the others keep theirs for screen
    // readers.
    await expect(page.locator('li[aria-current="step"] .wizard-step__label')).toBeVisible();
    await expect(page.getByRole('navigation', { name: 'Product setup progress' })).toContainText(
      'Checkout',
    );
    expect((await layout(page)).page).toBeLessThanOrEqual(0);
  });

  test('a product page has one visible primary, in the topbar', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto(`/b/products/admin/products/${encodeURIComponent(productId)}`, {
      waitUntil: 'networkidle',
    });
    await expect(page.locator('h1')).toHaveText(product.name);
    await expect(
      page.getByRole('navigation', { name: 'Breadcrumb' }).getByRole('link', { name: 'All products' }),
    ).toBeVisible();
    await expect(page.locator('.topbar__status .badge')).toHaveText('Draft');

    await expect(visiblePrimaries(page)).resolves.toEqual({ page: ['Publish product'], dialogs: [] });
    await expect(page.locator('#product-manager-form').getByRole('button', { name: 'Save product details' })).toHaveClass(/btn--secondary/);
    const offer = page.locator('[data-offer-card]');
    await expect(offer).toHaveCount(1);
    await expect(offer.getByRole('button', { name: 'Publish', exact: true })).toHaveClass(/btn--secondary/);

    // Revealing the pricing editor shows its Save, a secondary: still one.
    await offer.getByRole('button', { name: 'Edit visually' }).click();
    await expect(page.locator('#product-manager-visual-editor')).toBeVisible();
    await expect(visiblePrimaries(page)).resolves.toEqual({ page: ['Publish product'], dialogs: [] });
    await page.locator('#product-manager-visual-editor').getByRole('button', { name: 'Cancel' }).click();

    await expect(page.getByText('Create a product with pricing')).toHaveCount(0);
  });

  test('the wizard shows one primary at every step', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/products/admin/new', { waitUntil: 'networkidle' });
    await expect(visiblePrimaries(page)).resolves.toEqual({ page: ['Continue'], dialogs: [] });
    await page.getByRole('button', { name: 'Continue' }).click();
    await page.getByLabel('Product name').fill('One primary');
    for (let step = 2; step < 5; step++) {
      await expect(visiblePrimaries(page)).resolves.toEqual({ page: ['Continue'], dialogs: [] });
      await page.getByRole('button', { name: 'Continue' }).click();
    }
    await expect(page.locator('[data-wizard-step="5"]')).toBeVisible();
    await expect(visiblePrimaries(page)).resolves.toEqual({ page: ['Create and publish'], dialogs: [] });
  });

  test('the group editor is a dialog with its own primary', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/products/admin/groups', { waitUntil: 'networkidle' });
    const opener = page.locator('.topbar__actions').getByRole('button', { name: 'New group' });
    await opener.click();
    const dialog = page.getByRole('dialog', { name: 'New group' });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByLabel('Name')).toBeFocused();
    await expect(visiblePrimaries(page)).resolves.toEqual({ page: ['New group'], dialogs: [['Save group']] });
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await expect(dialog).toBeHidden();
    await expect(opener).toBeFocused();
  });

  /**
   * Every narrow layout of the products lists, on seeded rows at every width
   * from the phone to 1280 (table.css `.product-table--live`, `--deleted`,
   * `--own`, `--own-deleted`, and `.group-table`): no page or table scrolls
   * sideways and every row control stays in view.
   *
   * It seeds a long-named group, a long-named administration product it then
   * deletes (the Deleted view), and turns seller products on through the
   * products Settings form so My products renders, with one live and one
   * deleted product of the admin's own. The `finally` puts the setting back
   * and deletes what it made.
   */
  test('the seeded product and group tables fit their frame at every width', async ({ page, baseURL }) => {
    const api = await playwrightRequest.newContext({ baseURL, storageState: { cookies: [], origins: [] } });
    const auth = { Authorization: await adminBearer(api) };
    const headers = { ...auth, 'Content-Type': 'application/json' };
    const idOf = async (res: Awaited<ReturnType<typeof api.post>>) => {
      expect(res.status(), await res.text()).toBe(200);
      const body = (await res.json()) as { id?: string; data?: { id?: string } };
      return (body.id ?? body.data?.id) as string;
    };
    // Every name carries `WORST_WORD`, an unbreakable word wider than the
    // run's `stamp` (13 characters, 79-106px in the CI font, by its digits),
    // so a table that does not wrap the name fails on every run, not only
    // when the clock gives a wide stamp.
    const long = `annual-enterprise-subscription-with-priority-support ${WORST_WORD} ${stamp}`;
    const groupId = await idOf(
      await api.post('/b/products/api/admin/groups', {
        headers,
        data: { name: `Printed goods and other physical merchandise ${WORST_WORD} ${stamp}`, description: 'Posters, mugs, and anything else that ships in a box', status: 'active' },
      }),
    );
    const deletedId = await idOf(
      await api.post('/b/products/api/admin/products', {
        headers,
        data: { ...uniqueShopProduct(`gone${stamp}`), name: `Deleted ${long}`, status: 'draft' },
      }),
    );
    expect((await api.delete(`/b/products/api/admin/products/${deletedId}`, { headers: auth })).status()).toBe(200);

    const settings = '/b/products/admin/settings';
    const userProducts = page.getByRole('switch', { name: 'User products' });
    /** The settings page, with the collapsed seller-products section open. */
    const openSettings = async () => {
      await page.goto(settings, { waitUntil: 'networkidle' });
      await page.getByText('Seller products (optional)', { exact: true }).click();
      await expect(userProducts).toBeVisible();
    };
    const saveSettings = async (on: boolean) => {
      await openSettings();
      // The switch's whole label row is its hit area.
      if ((await userProducts.isChecked()) !== on) {
        await page.locator('label.form-switch', { has: userProducts }).click();
      }
      await expect(userProducts).toBeChecked({ checked: on });
      // The form posts to the shared settings endpoint, not its own page.
      const answered = page.waitForResponse((r) => r.url().endsWith('/b/admin/settings/products') && r.request().method() === 'POST');
      await page.getByRole('button', { name: 'Save settings' }).click();
      expect((await answered).status()).toBeLessThan(400);
    };
    const own: string[] = [];
    let wasOn: boolean | null = null;
    try {
      await loginAsAdmin(page);
      await openSettings();
      wasOn = await userProducts.isChecked();
      if (!wasOn) await saveSettings(true);
      own.push(
        await idOf(
          await api.post('/b/products/api/products', {
            headers,
            data: { ...uniqueShopProduct(`mine${stamp}`), name: `Mine ${long}`, status: 'draft' },
          }),
        ),
      );
      own.push(
        await idOf(
          await api.post('/b/products/api/products', {
            headers,
            data: { ...uniqueShopProduct(`minegone${stamp}`), name: `Mine deleted ${long}`, status: 'draft' },
          }),
        ),
      );
      expect((await api.delete(`/b/products/api/products/${own[1]}`, { headers: auth })).status()).toBe(200);

      const paths = [
        '/b/products/admin/manage',
        `/b/products/admin/manage?search=${stamp}`,
        '/b/products/admin/manage?view=deleted',
        '/b/products/admin/groups',
        '/b/products/my-products',
        '/b/products/my-products?view=deleted',
      ];
      const found: Record<string, unknown> = {};
      for (const width of WIDTHS) {
        await page.setViewportSize({ width, height: 900 });
        for (const path of paths) {
          const res = await page.goto(path, { waitUntil: 'networkidle' });
          expect(res!.status(), path).toBe(200);
          const r = await layout(page);
          const rows = await page.locator('.data-table tbody tr').count();
          // Every row control in view (the Deleted views' two ways out).
          const clipped = await page.evaluate(() =>
            [...document.querySelectorAll<HTMLElement>('.data-table .btn')]
              .filter((b) => b.checkVisibility() && b.getBoundingClientRect().right > window.innerWidth)
              .map((b) => b.getAttribute('aria-label') ?? b.textContent),
          );
          expect(rows, `${path}: the seeded rows render`).toBeGreaterThan(0);
          if (overflows(r) || clipped.length) found[`${path} @${width}`] = { ...r, clipped };
        }
      }
      expect(found, 'a table scrolled sideways or hid a row control').toEqual({});
    } finally {
      if (wasOn === false) await saveSettings(false);
      for (const id of own) await api.delete(`/b/products/api/products/${id}`, { headers: auth });
      await api.delete(`/b/products/api/admin/groups/${encodeURIComponent(groupId)}`, { headers: auth });
      await api.dispose();
    }
  });

  test('a product without pricing adds a price from its empty state, and it publishes', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.goto(`/b/products/admin/products/${encodeURIComponent(unpricedId)}`, {
      waitUntil: 'networkidle',
    });
    const pricing = page.locator('#product-pricing');
    await expect(pricing.getByRole('heading', { name: 'No prices yet' })).toBeVisible();
    await expect(pricing.locator('[data-offer-card]')).toHaveCount(0);

    await pricing.getByRole('button', { name: 'Add a price' }).click();
    const editor = page.locator('#product-manager-visual-editor');
    await expect(editor).toBeVisible();
    await expect(editor.getByRole('heading', { name: 'Add a price' })).toBeVisible();
    await expect(page.locator('#manager-visual-offer-name')).toHaveValue(unpriced.name);
    await expect(page.locator('#manager-visual-currency')).toHaveValue('NZD');
    const amount = editor.locator('[data-component-amount]');
    await expect(amount).toBeFocused();

    // Nothing priced yet: refused beside the button, nothing created.
    await editor.getByRole('button', { name: 'Create price' }).click();
    await expect(editor.getByRole('alert')).toHaveText('Enter an amount');
    await expect(amount).toHaveAttribute('aria-invalid', 'true');
    await expect(amount).toBeFocused();

    await amount.fill('25.00');
    await editor.getByRole('button', { name: 'Create price' }).click();
    await expect(page.locator('.toast-success')).toContainText('Price added as a draft');
    await expect(editor).toBeHidden();
    const offer = pricing.locator('[data-offer-card]');
    await expect(offer).toHaveCount(1);
    await expect(offer.getByRole('heading', { name: unpriced.name })).toBeVisible();
    await expect(pricing.getByText('No prices yet')).toHaveCount(0);
    // The draft's Publish and adding another price are both secondary: the
    // topbar's Publish product stays the page's one primary.
    await expect(pricing.locator('.btn--primary')).toHaveCount(0);
    await expect(visiblePrimaries(page)).resolves.toEqual({ page: ['Publish product'], dialogs: [] });
    await expect(pricing.getByRole('button', { name: 'Add a price' })).toHaveClass(/btn--secondary/);

    await offer.getByRole('button', { name: 'Publish' }).click();
    await expect(page.locator('#product-pricing [data-offer-card] .badge', { hasText: 'active' })).toBeVisible();
    await expect(page.locator('#product-pricing [data-offer-card]').getByRole('button', { name: 'Publish' })).toHaveCount(0);
  });

  test('Manage filters with view links and searches in the filter row', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/products/admin/manage', { waitUntil: 'networkidle' });
    const views = page.getByRole('navigation', { name: 'Product views' });
    await expect(views.getByRole('link', { name: 'Active' })).toHaveAttribute('aria-current', 'true');

    // A search swaps the page body: one search box, one title, the result.
    const search = page.getByRole('searchbox', { name: 'Search by product name' });
    await search.fill(stamp);
    await expect(page.locator('tr', { hasText: product.name })).toBeVisible();
    await expect(page.getByRole('searchbox')).toHaveCount(1);
    await expect(page.locator('h1')).toHaveCount(1);
    await expect(page.locator('tr').filter({ hasText: 'Open to edit' })).toHaveCount(0);

    await views.getByRole('link', { name: 'Deleted' }).click();
    await expect(page).toHaveURL(/view=deleted$/);
    await expect(
      page.getByRole('navigation', { name: 'Product views' }).getByRole('link', { name: 'Deleted' }),
    ).toHaveAttribute('aria-current', 'true');
    // A view, not a section: the section link stays on Products.
    await expect(
      page.getByRole('navigation', { name: 'Products sections' }).getByRole('link', { name: 'Products' }),
    ).toHaveAttribute('aria-current', 'page');
  });

  test('the portal buttons add form is one column at 390px', async ({ page }) => {
    await loginAsAdmin(page);
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto('/b/userportal/admin/buttons', { waitUntil: 'networkidle' });
    const lefts = await page
      .locator('.admin-buttons-form-grid > *')
      .evaluateAll((cells) => cells.map((c) => Math.round(c.getBoundingClientRect().left)));
    expect(new Set(lefts).size, JSON.stringify(lefts)).toBe(1);
    expect((await layout(page)).page).toBeLessThanOrEqual(0);
  });
});
