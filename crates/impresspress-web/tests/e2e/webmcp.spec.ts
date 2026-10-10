import { expect, test, type APIRequestContext } from '@playwright/test';
import { ADMIN_STATE_PATH, adminBearer } from './fixtures/auth';
import { SHOP_OFFER, uniqueShopProduct } from './fixtures/shop-fixture';
import { execute, registeredTools, toolNames, waitForTool } from './fixtures/webmcp-helpers';

/**
 * WebMCP end-to-end against the real native server (visual-baseline config,
 * admin session via globalSetup). It seeds a product, so it is part of
 * `e2e:writes` (its own fresh server in CI, port 8094), not `e2e:visual`.
 *
 * The browser is Chromium with `--enable-features=WebMCPTesting`
 * (`playwright.config.ts`), so the page registers into Chromium's own
 * `navigator.modelContext` and the helpers read and invoke tools through
 * `navigator.modelContextTesting`. Everything on both sides of the API is
 * real: the served manifest, the registration script, the browser's
 * registry, the request `execute` builds, and the endpoint that answers it.
 * What this cannot test is whether an agent *chooses* the right tool from its
 * description; that needs an agent and a human (plan 3, task 5).
 */

const PUBLIC_TOOLS = [
  'get_storefront_config',
  'list_products',
  'get_product',
  'preview_price',
  'start_checkout',
  'get_order_status',
];

/** Admin-tier read tools. Must never appear below the Admin tier. */
const ADMIN_TOOLS = ['list_users', 'list_roles', 'get_site_settings', 'list_audit_log'];

test('a real WebMCP browser gets the public tools with no polyfill', async ({ page }) => {
  await page.goto('/b/auth/login');
  await waitForTool(page, 'list_products');
  const names = await toolNames(page);
  expect(names).toContain('get_storefront_config');
  // Anonymous: nothing above Public is published (live twin of
  // webmcp_manifest_for_anonymous_caller_contains_no_privileged_tools).
  for (const forbidden of ['list_users', 'list_audit_log', 'get_site_settings', 'list_roles']) {
    expect(names).not.toContain(forbidden);
  }
});

test('refresh() re-registers the manifest without disturbing a tool it does not own', async ({ page }) => {
  // `refresh()` drops exactly the names webmcp.js itself registered (see
  // `registered` in `webmcp.js`) and registers what the manifest says now.
  // A tool something else registered — `dev.js` on `/b/dev`; `stale_tool`
  // here — must survive it.
  //
  // The second manifest fetch answers the first one minus one tool, so the
  // unregister half is observable: a refresh that unregistered nothing would
  // leave that tool registered. (Chrome ignores `registerTool`'s signal, so
  // whether a signal or a by-name call removed it is not observable and is
  // not asserted.)
  let fetches = 0;
  let dropped = '';
  await page.route('**/b/webmcp/manifest.json', async (route) => {
    fetches += 1;
    const response = await route.fetch();
    const body = (await response.json()) as { tools: Array<{ name: string }> };
    if (fetches > 1) {
      dropped = body.tools[0].name;
      body.tools = body.tools.slice(1);
    }
    await route.fulfill({ response, json: body });
  });
  await page.goto('/b/auth/login');
  await registeredTools(page, PUBLIC_TOOLS.length);
  const before = await toolNames(page);
  const generationBefore = await page.evaluate(() => window.__impresspressWebmcp.generation());
  await page.evaluate(() =>
    (navigator as unknown as { modelContext: { registerTool(options: unknown): void } }).modelContext.registerTool({
      name: 'stale_tool',
      description: 'A tool webmcp.js did not register.',
      inputSchema: { type: 'object' },
      execute: async () => ({ content: [] }),
    }),
  );
  await page.evaluate(() => window.__impresspressWebmcp.refresh());

  expect(await page.evaluate(() => window.__impresspressWebmcp.generation())).toBeGreaterThan(generationBefore);
  expect(dropped).not.toBe('');
  const after = await toolNames(page);
  expect(after).not.toContain(dropped);
  expect(after).toEqual([...before.filter((n) => n !== dropped), 'stale_tool'].sort());
});

/**
 * One product with one published, component-priced offer — the shared
 * `fixtures/shop-fixture.ts` payload, which `dev-workspace.spec.ts` creates
 * through the `shop_*` tools instead. `uniqueShopProduct` gives it a
 * per-run-unique slug so re-running against the same database never collides,
 * and makes it `active` so the Public tools below can see it.
 */
async function seedProductWithOffer(
  request: APIRequestContext,
): Promise<{ productId: string; offerId: string }> {
  const auth = { Authorization: await adminBearer(request), 'Content-Type': 'application/json' };
  const stamp = Date.now().toString(36);

  const productRes = await request.post('/b/products/api/admin/products', {
    headers: auth,
    data: uniqueShopProduct(stamp),
  });
  expect(productRes.status(), await productRes.text()).toBe(200);
  const productBody = (await productRes.json()) as { id?: string; currency?: string; data?: { id?: string } };
  const productId = productBody.id ?? productBody.data?.id;
  expect(productId, JSON.stringify(productBody)).toBeTruthy();
  // The fixture sends `nzd`; the product answers the one spelling its
  // offers and quotes answer too.
  expect(productBody.currency).toBe('NZD');

  const offerRes = await request.post(`/b/products/api/admin/products/${productId}/offers`, {
    headers: auth,
    data: SHOP_OFFER,
  });
  expect(offerRes.status(), await offerRes.text()).toBe(200);
  const offerBody = (await offerRes.json()) as { offer?: { id?: string }; id?: string };
  const offerId = offerBody.offer?.id ?? offerBody.id;
  expect(offerId, JSON.stringify(offerBody)).toBeTruthy();

  const publishRes = await request.post(
    `/b/products/api/admin/products/${productId}/offers/${offerId}/publish`,
    { headers: auth, data: {} },
  );
  expect(publishRes.status(), await publishRes.text()).toBe(200);
  const published = (await publishRes.json()) as { status?: string };
  expect(published.status).toBe('active');

  return { productId: productId as string, offerId: offerId as string };
}

test.describe('WebMCP registration on an anonymous page', () => {
  test('registers exactly the Public storefront tools; the manifest gives each both schemas', async ({ page }) => {
    await page.goto('/b/auth/login');
    const tools = await registeredTools(page, PUBLIC_TOOLS.length);

    expect(tools.map((t) => t.name).sort()).toEqual([...PUBLIC_TOOLS].sort());
    for (const tool of tools) {
      expect(tool.description.length, tool.name).toBeGreaterThan(20);
      expect(tool.inputSchema?.type, `${tool.name} inputSchema`).toBe('object');
    }
    // The registry does not report output schemas; the manifest the page
    // registered from does, and `webmcp-core.js` hands it to `registerTool`.
    const manifest = (await (await page.request.get('/b/webmcp/manifest.json')).json()) as {
      tools: Array<{ name: string; outputSchema?: { type?: string } }>;
    };
    expect(manifest.tools.map((t) => t.name).sort()).toEqual([...PUBLIC_TOOLS].sort());
    for (const tool of manifest.tools) {
      expect(tool.outputSchema?.type, `${tool.name} outputSchema`).toBe('object');
    }
  });

  test('get_storefront_config returns structured content from the real endpoint', async ({ page }) => {
    await page.goto('/b/auth/login');
    await registeredTools(page, PUBLIC_TOOLS.length);

    const result = await execute(page, 'get_storefront_config', {});
    expect(result.isError).toBeFalsy();
    expect(result.content[0]?.type).toBe('text');
    expect(result.structuredContent?.schema_version).toBe(1);
    expect(typeof result.structuredContent?.embedded_checkout_available).toBe('boolean');
  });

  test('get_order_status with a bad receipt is an error result, not data', async ({ page }) => {
    await page.goto('/b/auth/login');
    await registeredTools(page, PUBLIC_TOOLS.length);

    const result = await execute(page, 'get_order_status', {
      id: 'order_does_not_exist',
      receipt_token: 'not-a-receipt',
    });
    expect(result.isError).toBe(true);
    expect(result.content[0]?.text).toMatch(/^Request failed \(4\d\d\)/);
    expect(result.structuredContent).toBeUndefined();
  });

  test('a call missing a required argument says which one, and makes no request', async ({ page }) => {
    await page.goto('/b/auth/login');
    await registeredTools(page, PUBLIC_TOOLS.length);

    // `get_product`'s `product_id` fills the URL (`/b/products/storefront/
    // {product_id}`). Chromium's registry does not check arguments against
    // `inputSchema`, so this reaches the page's `execute`, which must refuse
    // it itself — not fetch `/b/products/storefront/undefined` and relay the
    // server's 404 as if the product were missing.
    const requests: string[] = [];
    page.on('request', (request) => requests.push(request.url()));
    for (const args of [{}, { product_id: '' }]) {
      const result = await execute(page, 'get_product', args);
      expect(result, JSON.stringify(args)).toEqual({
        isError: true,
        content: [{ type: 'text', text: 'Missing required argument: product_id' }],
      });
    }
    expect(requests.filter((url) => new URL(url).pathname.startsWith('/b/products/'))).toEqual([]);
  });
});

test.describe('WebMCP tools against a seeded product', () => {
  let productId: string;
  let offerId: string;

  test.beforeAll(async ({ request }) => {
    ({ productId, offerId } = await seedProductWithOffer(request));
  });

  test.beforeEach(async ({ page }) => {
    await page.goto('/b/auth/login');
    await registeredTools(page, PUBLIC_TOOLS.length);
  });

  test('get_product returns the seeded product and its published offer', async ({ page }) => {
    const result = await execute(page, 'get_product', { product_id: productId });
    expect(result.isError, result.content[0]?.text).toBeFalsy();

    const product = result.structuredContent as {
      id: string;
      offers: Array<{ id: string; variables: Array<{ key: string; kind: string }> }>;
    };
    expect(product.id).toBe(productId);
    expect(product.offers.map((o) => o.id)).toEqual([offerId]);
    expect(product.offers[0].variables.map((v) => v.key)).toEqual(['pages']);
  });

  test('preview_price prices the offer from the customer inputs', async ({ page }) => {
    const result = await execute(page, 'preview_price', {
      offer_id: offerId,
      quantity: 1,
      inputs: { pages: 3 },
    });
    expect(result.isError, result.content[0]?.text).toBeFalsy();

    const quote = result.structuredContent as {
      offer_id: string;
      amounts: { currency: string; total_minor: number; subtotal_minor: number };
      components: Array<{ key: string; total_amount_minor: number }>;
    };
    expect(quote.offer_id).toBe(offerId);
    // 3 pages × 1500 minor units per page.
    expect(quote.components.map((c) => [c.key, c.total_amount_minor])).toEqual([['pages', 4500]]);
    expect(quote.amounts.subtotal_minor).toBe(4500);
    expect(quote.amounts.total_minor).toBe(4500);
    expect(quote.amounts.currency).toBe('NZD');
  });

  test('start_checkout cannot complete a payment here: no provider, an error result', async ({ page }) => {
    // No Stripe key is configured on the test server. The tool must surface
    // that as `isError`, never as a success the agent could relay.
    const result = await execute(page, 'start_checkout', {
      offer_id: offerId,
      quantity: 1,
      inputs: { pages: 1 },
      presentation: 'hosted',
    });
    expect(result.isError).toBe(true);
    expect(result.content[0]?.text).toMatch(/^Request failed \(\d{3}\)/);
    expect(result.structuredContent).toBeUndefined();
  });
});

test.describe('WebMCP registration for a signed-in admin', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  test('adds the Authenticated tool on top of the Public set', async ({ page }) => {
    await page.goto('/b/admin/');
    const tools = await registeredTools(page, PUBLIC_TOOLS.length + 1 + ADMIN_TOOLS.length);
    expect(tools.map((t) => t.name).sort()).toEqual(
      [...PUBLIC_TOOLS, 'list_my_purchases', ...ADMIN_TOOLS].sort(),
    );
  });

  test('the inspector shows the manifest at every auth level', async ({ page }) => {
    const res = await page.request.get('/b/inspector/webmcp');
    expect(res.status(), await res.text()).toBe(200);
    const view = (await res.json()) as {
      levels: Array<{
        level: string;
        manifest: { tools: Array<{ name: string }> };
        refusals: unknown[];
        opted_in: number;
      }>;
    };

    const entry = (level: string) => {
      const found = view.levels.find((l) => l.level === level);
      expect(found, `level ${level} in ${JSON.stringify(view.levels.map((l) => l.level))}`).toBeTruthy();
      return found as NonNullable<typeof found>;
    };
    const names = (level: string) => entry(level).manifest.tools.map((t) => t.name).sort();
    const pub = names('public');
    const authed = names('authenticated');
    const admin = names('admin');

    expect(pub).toEqual([...PUBLIC_TOOLS].sort());
    for (const name of pub) expect(authed, 'monotone: public ⊆ authenticated').toContain(name);
    for (const name of authed) expect(admin, 'monotone: authenticated ⊆ admin').toContain(name);
    expect(authed).toContain('list_my_purchases');
    for (const name of ADMIN_TOOLS) {
      expect(admin, `admin tier publishes ${name}`).toContain(name);
      expect(pub, `${name} must not reach an anonymous page`).not.toContain(name);
      expect(authed, `${name} must not reach a signed-in non-admin`).not.toContain(name);
    }

    // Every opted-in endpoint produced a tool at every level: nothing refused,
    // and the count the page reports is the count it publishes.
    for (const level of view.levels) {
      expect(level.refusals, `${level.level} refusals`).toEqual([]);
      expect(level.manifest.tools.length, `${level.level} opted_in`).toBe(level.opted_in);
    }
  });
});
