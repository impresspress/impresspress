import { expect, test, type APIRequestContext } from '@playwright/test';
import { ADMIN_STATE_PATH, adminBearer } from './fixtures/auth';
import { MODEL_CONTEXT_POLYFILL } from './fixtures/model-context-polyfill';
import { SHOP_OFFER, uniqueShopProduct } from './fixtures/shop-fixture';
import { execute, registeredTools } from './fixtures/webmcp-helpers';

/**
 * WebMCP end-to-end against the real native server (visual-baseline config:
 * port 8093 in CI, admin session via globalSetup).
 *
 * `MODEL_CONTEXT_POLYFILL` (shared with `smoke.spec.ts`) is the smallest
 * `document.modelContext` shim `ui/assets/webmcp.js` and `webmcp-core.js`
 * need. Everything on the other side of that boundary is real: the served
 * manifest, the registration script, the request `execute` builds, and the
 * endpoint that answers it. What this cannot test is whether an agent
 * *chooses* the right tool from its description; that needs a WebMCP-capable
 * browser and a human (plan 3, task 5).
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

test('refresh() re-registers the manifest without disturbing a tool it does not own', async ({ page }) => {
  // `refresh()` tracks and re-registers exactly the names webmcp.js itself
  // added from the manifest (see `registered` in `webmcp.js`) — not
  // everything `document.modelContext` currently knows about. That is
  // deliberate: `dev.js` (plan 2, task 3) registers its own page-scoped
  // tools directly against `document.modelContext` on `/b/dev`, and
  // webmcp.js's `refresh()` runs independently on manifest-generation
  // changes. If `refresh()` cleared every registered tool it would fight
  // `dev.js` for ownership of tools it never registered. `stale_tool` here
  // stands in for exactly that: something else's tool, registered directly
  // — it must survive a webmcp.js refresh untouched.
  //
  // The manifest is identical across both loads, so "unregister then
  // re-register the same set" and "do nothing" leave the polyfill's
  // name-keyed Map in the same end state — `after === before` alone can't
  // tell them apart. Two more assertions make it discriminating: (a)
  // `generation()` must actually increment (proving a fresh `load()` ran,
  // not a no-op), and (b) the polyfill's `__unregistered()` call log must
  // show every manifest tool name dropped exactly once — proving the
  // unregister half really fired — while `stale_tool` is never in it.
  await page.addInitScript(MODEL_CONTEXT_POLYFILL);
  await page.goto('/b/auth/login');
  await registeredTools(page, 1);
  const before = await page.evaluate(() => document.modelContext.__tools().map((t) => t.name).sort());
  const generationBefore = await page.evaluate(() => window.__impresspressWebmcp.generation());
  await page.evaluate(() =>
    document.modelContext.registerTool({
      name: 'stale_tool',
      description: 'x',
      inputSchema: { type: 'object' },
      execute: async () => ({ content: [] }),
    }),
  );
  await page.evaluate(() => window.__impresspressWebmcp.refresh());
  const after = await page.evaluate(() => document.modelContext.__tools().map((t) => t.name).sort());
  const generationAfter = await page.evaluate(() => window.__impresspressWebmcp.generation());
  const unregistered = await page.evaluate(() => document.modelContext.__unregistered());

  // (a) A real refresh happened, not a no-op that coincidentally left the
  // same set behind.
  expect(generationAfter).toBeGreaterThan(generationBefore);

  // (b) Every manifest tool was actually dropped — exactly once each — and
  // `stale_tool` was never touched by `unregisterTool` at all.
  for (const name of before) {
    expect(unregistered.filter((n: string) => n === name), name).toHaveLength(1);
  }
  expect(unregistered).not.toContain('stale_tool');

  // End state: the manifest tools are back (re-registered) and the foreign
  // tool survived untouched.
  expect(after).toEqual([...before, 'stale_tool'].sort());
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
  const productBody = (await productRes.json()) as { id?: string; data?: { id?: string } };
  const productId = productBody.id ?? productBody.data?.id;
  expect(productId, JSON.stringify(productBody)).toBeTruthy();

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
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(MODEL_CONTEXT_POLYFILL);
  });

  test('registers exactly the five Public storefront tools, each with both schemas', async ({ page }) => {
    await page.goto('/b/auth/login');
    const tools = await registeredTools(page, PUBLIC_TOOLS.length);

    expect(tools.map((t) => t.name).sort()).toEqual([...PUBLIC_TOOLS].sort());
    for (const tool of tools) {
      expect(tool.description.length, tool.name).toBeGreaterThan(20);
      expect(tool.inputSchema?.type, `${tool.name} inputSchema`).toBe('object');
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
});

test.describe('WebMCP tools against a seeded product', () => {
  let productId: string;
  let offerId: string;

  test.beforeAll(async ({ request }) => {
    ({ productId, offerId } = await seedProductWithOffer(request));
  });

  test.beforeEach(async ({ page }) => {
    await page.addInitScript(MODEL_CONTEXT_POLYFILL);
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
    expect(quote.amounts.currency.toLowerCase()).toBe('nzd');
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

  test.beforeEach(async ({ page }) => {
    await page.addInitScript(MODEL_CONTEXT_POLYFILL);
  });

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
