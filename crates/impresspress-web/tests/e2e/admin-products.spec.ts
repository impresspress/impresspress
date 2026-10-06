import { expect, request as playwrightRequest, test, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, adminBearer, loginAsAdmin } from './fixtures/auth';
import { SHOP_OFFER, uniqueShopProduct } from './fixtures/shop-fixture';

/**
 * The products admin pages, driven in a browser against the real server.
 *
 * - The wizard announces its current step (`aria-current="step"`), and the
 *   marker moves with Continue; at 390px its five steps stay on one row.
 * - A product page has one primary action per section: the topbar's
 *   (Publish), the details form's (Save), and a draft offer's (Publish).
 * - The Active / Deleted views are filter links, and Manage's search box
 *   sits in the filter row rather than a card of its own.
 * - The portal buttons page's add form collapses to one column at 390px.
 *
 * It seeds a product and a draft offer over the API and deletes the product
 * again at the end, so it is part of `e2e:writes`, which CI runs against its
 * own fresh server.
 */

test.use({ storageState: ADMIN_STATE_PATH });

/** No horizontal scroll: the document is no wider than the viewport. */
async function overflowX(page: Page): Promise<number> {
  return page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
}

test.describe.serial('products admin pages', () => {
  const stamp = `pages${Date.now().toString(36)}`;
  const product = { ...uniqueShopProduct(stamp), status: 'draft' };
  let productId = '';

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
    await api.dispose();
  });

  test.afterAll(async ({ baseURL }) => {
    const api = await playwrightRequest.newContext({
      baseURL,
      storageState: { cookies: [], origins: [] },
    });
    const headers = { Authorization: await adminBearer(api) };
    await api.delete(`/b/products/api/admin/products/${productId}`, { headers });
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
    expect(await overflowX(page)).toBeLessThanOrEqual(0);
  });

  test('a product page has one primary action per section', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto(`/b/products/admin/products/${encodeURIComponent(productId)}`, {
      waitUntil: 'networkidle',
    });
    await expect(page.locator('h1')).toHaveText(product.name);
    await expect(
      page.getByRole('navigation', { name: 'Breadcrumb' }).getByRole('link', { name: 'All products' }),
    ).toBeVisible();

    const topbar = page.locator('.topbar__actions');
    await expect(topbar.locator('.btn--primary')).toHaveCount(1);
    await expect(topbar.locator('.btn--primary')).toHaveText('Publish product');

    const details = page.locator('#product-manager-form');
    await expect(details.locator('.btn--primary')).toHaveCount(1);
    await expect(details.locator('.btn--primary')).toHaveText('Save product details');

    const offer = page.locator('[data-offer-card]');
    await expect(offer).toHaveCount(1);
    await expect(offer.locator('.btn--primary')).toHaveCount(1);
    await expect(offer.locator('.btn--primary')).toHaveText('Publish');

    await expect(page.getByText('Create a product with pricing')).toHaveCount(0);
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
    expect(await overflowX(page)).toBeLessThanOrEqual(0);
  });
});
