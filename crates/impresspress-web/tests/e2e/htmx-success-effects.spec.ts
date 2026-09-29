import {
  expect,
  request as playwrightRequest,
  test,
  type APIRequestContext,
  type Page,
} from '@playwright/test';
import { ADMIN_STATE_PATH, adminBearer, loginAsAdmin } from './fixtures/auth';
import { SHOP_OFFER, uniqueShopProduct } from './fixtures/shop-fixture';

/**
 * The after-success effects of htmx controls, under the CSP the server really
 * serves.
 *
 * Those effects — reload the page, reset the form, drop an empty-state line,
 * scroll a list to its newest entry — were `hx-on--after-request` attributes.
 * htmx compiles an `hx-on` value with `new Function`, which is `eval` as far as
 * a content-security policy is concerned, and the policy
 * `wafer-run/security-headers` serves has no `'unsafe-eval'` (it refuses to
 * add one). So every one of them was dead: a created LLM provider did not
 * appear until a manual reload, a restored product stayed on the Deleted tab,
 * a posted message left its text in the composer. Nothing failed loudly; the
 * browser logged a CSP refusal and the page sat there.
 *
 * They are now `data-*-on-success` attributes read by one listener in
 * `crates/impresspress-core/src/ui/assets/chrome.js`, and the layout sets
 * htmx's `allowEval` to false so an eval-shaped attribute that comes back
 * fails with an `htmx:evalDisallowedError` instead of a silent CSP refusal.
 * The markup gate is `ui::tests::pages_carry_no_htmx_eval_attributes`; this
 * spec is the half it cannot see — that the listener fires, in a real browser,
 * on the real pages, with the real header.
 *
 * Every case asserts the VISIBLE effect, not the attribute, and every case
 * drives the real server: nothing is intercepted except the provider's
 * model-discovery call, which would otherwise need a live LLM endpoint.
 *
 * It writes (a provider, contexts, entries, a product), so it runs after the
 * visual baselines in CI, never before them.
 */

/** A console line that means a script was refused or an eval was attempted. */
const EVAL_REFUSAL = /unsafe-eval|evalDisallowed|Content Security Policy/i;

/**
 * Collect every console error on `page` that reports an eval refusal. Returned
 * as a live array so a test can assert it is still empty at the end.
 */
function watchEvalRefusals(page: Page): string[] {
  const seen: string[] = [];
  page.on('console', (message) => {
    if (message.type() === 'error' && EVAL_REFUSAL.test(message.text())) {
      seen.push(message.text());
    }
  });
  return seen;
}

/** Navigate, and prove the page came with a policy that forbids eval. */
async function gotoUnderCsp(page: Page, path: string): Promise<void> {
  const response = await page.goto(path, { waitUntil: 'networkidle' });
  expect(response, `no response for ${path}`).not.toBeNull();
  const csp = response!.headers()['content-security-policy'] ?? '';
  expect(csp, `${path} must be served with a script-src`).toMatch(/script-src/);
  expect(csp, `${path} must not allow eval`).not.toMatch(/'unsafe-eval'/);
}

/**
 * A cookie-less API context with the admin's bearer, for seeding.
 *
 * Not `page.request`: that one carries the storageState's `auth_token`
 * cookie, and a cookie-authenticated write without a same-origin `Origin` is
 * refused by the CSRF policy — bearer auth is what is exempt from it.
 */
async function seeder(
  baseURL: string | undefined,
): Promise<{ api: APIRequestContext; headers: Record<string, string> }> {
  // Explicitly empty: inside a test the runner hands `newContext` the
  // describe's `use` options, storageState included.
  const api = await playwrightRequest.newContext({
    baseURL,
    storageState: { cookies: [], origins: [] },
  });
  const headers = { Authorization: await adminBearer(api), 'Content-Type': 'application/json' };
  return { api, headers };
}

async function createContext(
  request: APIRequestContext,
  headers: Record<string, string>,
  type: string,
  title: string,
): Promise<string> {
  const res = await request.post('/b/messages/api/contexts', { headers, data: { type, title } });
  expect(res.status(), await res.text()).toBe(200);
  const body = (await res.json()) as { id?: string; data?: { id?: string } };
  const id = body.id ?? body.data?.id;
  expect(id, JSON.stringify(body)).toBeTruthy();
  return id as string;
}

/** Enough entries to overflow any scroll container these pages render. */
async function fillContext(
  request: APIRequestContext,
  headers: Record<string, string>,
  contextId: string,
): Promise<void> {
  for (let i = 0; i < 30; i++) {
    const res = await request.post(`/b/messages/api/contexts/${contextId}/entries`, {
      headers,
      data: { kind: 'message', role: 'user', content: `filler ${i}\nsecond line\nthird line` },
    });
    expect(res.status(), await res.text()).toBe(200);
  }
}

/** Whether `selector`'s element is scrolled to within a pixel of its bottom, and actually scrolls. */
async function scrolledToBottom(page: Page, selector: string): Promise<boolean> {
  return page.locator(selector).evaluate((el) => {
    const max = el.scrollHeight - el.clientHeight;
    return max > 0 && Math.abs(el.scrollTop - max) <= 1;
  });
}

test.describe('htmx after-success effects under the served CSP', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  test('creating a messages context resets the form and prepends the row', async ({ page }) => {
    await loginAsAdmin(page);
    const refusals = watchEvalRefusals(page);
    await gotoUnderCsp(page, '/b/messages/');

    const title = `csp context ${Date.now().toString(36)}`;
    const titleField = page.locator('#new-context-title');
    await titleField.fill(title);
    await page.locator('.messages-new__form button[type="submit"]').click();

    await expect(page.locator('#context-list')).toContainText(title);
    // The swap put a row in the list, so the "No contexts yet" line is gone
    // whether or not it was there before this run.
    await expect(page.locator('#context-list-empty')).toHaveCount(0);
    await expect(titleField).toHaveValue('');
    expect(refusals).toEqual([]);
  });

  test('the default-view composer resets, drops the empty line and scrolls the list', async ({
    page,
    baseURL,
  }) => {
    await loginAsAdmin(page);
    const refusals = watchEvalRefusals(page);
    const { api, headers } = await seeder(baseURL);
    const contextId = await createContext(api, headers, 'task', 'csp task');

    await gotoUnderCsp(page, `/b/messages/contexts/${contextId}`);
    await expect(page.locator('#entries-empty')).toBeVisible();

    const form = page.locator('form[hx-target="#entries-list"]');
    const content = form.locator('[name="content"]');
    await content.fill('first entry from the form');
    await form.locator('button[type="submit"]').click();

    await expect(page.locator('#entries-list')).toContainText('first entry from the form');
    await expect(page.locator('#entries-empty')).toHaveCount(0);
    await expect(content).toHaveValue('');

    // Now overflow the list, start at its top, and post again: the newest
    // entry has to be scrolled into view.
    await fillContext(api, headers, contextId);
    await gotoUnderCsp(page, `/b/messages/contexts/${contextId}`);
    await page.locator('#entries-list').evaluate((el) => {
      el.scrollTop = 0;
    });
    await content.fill('last entry from the form');
    await form.locator('button[type="submit"]').click();
    await expect(page.locator('#entries-list')).toContainText('last entry from the form');
    await expect.poll(() => scrolledToBottom(page, '#entries-list')).toBe(true);
    expect(refusals).toEqual([]);
  });

  test('the conversation composer resets, drops the empty line and scrolls the pane', async ({
    page,
    baseURL,
  }) => {
    await loginAsAdmin(page);
    const refusals = watchEvalRefusals(page);
    const { api, headers } = await seeder(baseURL);
    const contextId = await createContext(api, headers, 'conversation', 'csp chat');

    await gotoUnderCsp(page, `/b/messages/contexts/${contextId}`);
    await expect(page.locator('#entries-empty')).toBeVisible();

    const composer = page.locator('.chat-composer form');
    const content = composer.locator('[name="content"]');
    await content.fill('hello from the composer');
    await composer.locator('button[type="submit"]').click();

    await expect(page.locator('#entries-list')).toContainText('hello from the composer');
    await expect(page.locator('#entries-empty')).toHaveCount(0);
    await expect(content).toHaveValue('');

    await fillContext(api, headers, contextId);
    await gotoUnderCsp(page, `/b/messages/contexts/${contextId}`);
    await page.locator('#chat-messages').evaluate((el) => {
      el.scrollTop = 0;
    });
    await content.fill('newest message');
    await composer.locator('button[type="submit"]').click();
    await expect(page.locator('#entries-list')).toContainText('newest message');
    await expect.poll(() => scrolledToBottom(page, '#chat-messages')).toBe(true);
    expect(refusals).toEqual([]);
  });

  test('adding an LLM provider reloads the page to show it, and so does Discover', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    const refusals = watchEvalRefusals(page);
    await gotoUnderCsp(page, '/b/llm/providers');

    const name = `csp-provider-${Date.now().toString(36)}`;
    await page.locator('#new-name').fill(name);
    await page.locator('#new-endpoint').fill('https://llm.example.com/v1');
    const reloaded = page.waitForEvent('load');
    await page.locator('form[hx-post="/b/llm/api/providers"] button[type="submit"]').click();
    await reloaded;
    const row = page.locator('tr', { hasText: name });
    await expect(row).toBeVisible();

    // Discovery would call the provider's endpoint; answer it here so the
    // case needs no live LLM. The request still leaves the page through htmx,
    // and the success effect is what is under test.
    await page.route('**/b/llm/api/providers/*/discover-models', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: '[]' }),
    );
    page.once('dialog', (dialog) => dialog.accept());
    const rediscovered = page.waitForEvent('load');
    await row.getByRole('button', { name: 'Discover' }).click();
    await rediscovered;
    await expect(page.locator('tr', { hasText: name })).toBeVisible();
    expect(refusals).toEqual([]);
  });

  test('Archive offer and Restore reload the products admin pages', async ({
    page,
    baseURL,
  }) => {
    await loginAsAdmin(page);
    const refusals = watchEvalRefusals(page);
    const { api, headers } = await seeder(baseURL);
    const stamp = `csp${Date.now().toString(36)}`;
    const product = uniqueShopProduct(stamp);

    const productRes = await api.post('/b/products/api/admin/products', {
      headers,
      data: product,
    });
    expect(productRes.status(), await productRes.text()).toBe(200);
    const productBody = (await productRes.json()) as { id?: string; data?: { id?: string } };
    const productId = (productBody.id ?? productBody.data?.id) as string;
    expect(productId, JSON.stringify(productBody)).toBeTruthy();

    const offerRes = await api.post(`/b/products/api/admin/products/${productId}/offers`, {
      headers,
      data: SHOP_OFFER,
    });
    expect(offerRes.status(), await offerRes.text()).toBe(200);

    const deleteRes = await api.delete(`/b/products/api/admin/products/${productId}`, {
      headers,
    });
    expect(deleteRes.status(), await deleteRes.text()).toBe(200);

    // The close-only manager: archiving the offer reloads it without the button.
    await gotoUnderCsp(page, `/b/products/admin/products/${encodeURIComponent(productId)}/close`);
    const archive = page.getByRole('button', { name: 'Archive offer' });
    await expect(archive).toHaveCount(1);
    const archived = page.waitForEvent('load');
    await archive.click();
    await archived;
    await expect(page.getByRole('button', { name: 'Archive offer' })).toHaveCount(0);

    // The Deleted tab: restoring reloads it without the product.
    await gotoUnderCsp(page, '/b/products/admin/manage?view=deleted');
    const deletedRow = page.locator('tr', { hasText: product.name });
    await expect(deletedRow).toBeVisible();
    const restored = page.waitForEvent('load');
    await deletedRow.getByRole('button', { name: 'Restore' }).click();
    await restored;
    await expect(page.locator('tr', { hasText: product.name })).toHaveCount(0);
    expect(refusals).toEqual([]);
  });
});
