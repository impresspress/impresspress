import { test, expect } from '@playwright/test';
import { bootServiceWorker, loginAdmin } from './fixtures/dev-sandbox';

/**
 * A vector index created in the sandbox opens.
 *
 * The incident: in the browser build, `/b/vector/{name}/` answered the 500
 * page right after the index was created. The detail page reads the index's
 * columns through `vector.describe_index`, and the browser's vector backend
 * (`BrowserVectorService`) did not implement that op, so it got the trait
 * default — `Internal("describe_index not implemented by this backend")`.
 * The core test of that page passed throughout: it answers `describe_index`
 * with a stand-in block, which is exactly the part that was missing here.
 *
 * So this goes through the real thing end to end: the admin list page's own
 * create form, then the detail page, served by the sandbox's wasm runtime
 * over sql.js. The columns it shows are the ones the backend read off the
 * index's meta table.
 */

const NAME = 'e2e_detail';

test('an index created from the admin page opens on its detail page', async ({ page }) => {
  await bootServiceWorker(page);
  await loginAdmin(page);

  await page.goto('/b/vector/', { waitUntil: 'commit' });
  await page.getByRole('button', { name: '+ Create index' }).click();
  const form = page.locator('#create-vector-index form');
  await form.locator('input[name="name"]').fill(NAME);
  await form.locator('input[name="keyword_search"]').check();
  await form.getByRole('button', { name: 'Create' }).click();
  await expect(page.locator(`tr[data-index-name="${NAME}"]`)).toBeVisible({ timeout: 30_000 });

  const detail = await page.goto(`/b/vector/${NAME}/`, { waitUntil: 'commit' });
  expect(detail?.status()).toBe(200);

  const schema = page.locator('section', { has: page.getByRole('heading', { name: 'Schema' }) });
  const columns = schema.locator('td[data-label="Column"]');
  await expect(columns).toHaveText(['id', 'rowid', 'metadata', 'text']);
  await expect(schema.locator('td[data-label="Type"]')).toHaveText([
    'TEXT',
    'INTEGER',
    'TEXT',
    'TEXT',
  ]);
});
