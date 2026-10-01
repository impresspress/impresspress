/**
 * One product and one offer, as the products admin API takes them.
 *
 * Three specs stock a shop and they must stock the *same* shop:
 * `webmcp.spec.ts` seeds it over HTTP against the native server (to have
 * something for `get_product` / `preview_price` to answer with), and
 * `dev-workspace.spec.ts` and `dev-scenario.spec.ts` have an in-page agent
 * create it through the `shop_*` tools inside the browser sandbox. Same wire
 * shape, two very different transports — which is exactly the set that drifts
 * if each keeps its own literal.
 *
 * The offer is deliberately the interesting kind rather than a flat price:
 * `pricing_model: "components"` with one typed customer input (`pages`,
 * integer, 1–20) priced at 1500 minor units each. A fixed-price offer would
 * exercise none of the typed-input machinery the products block exists for,
 * and `webmcp.spec.ts` prices it (3 × 1500 = 4500) as its `preview_price`
 * assertion.
 *
 * Field names come from `products/contracts.rs`
 * (`CreateProductRequest`) and the hand-written `offer_definition_schema`
 * in `products/mod.rs`; both are published in
 * `impresspress-core/tests/snapshots/dev.tools.json` as the `shop_*` tools'
 * `inputSchema`, which is what an agent actually sees.
 */

/**
 * The product. No `status`: the API creates it as `draft`, and a draft is
 * invisible to shoppers — which is the state
 * `dev-workspace.spec.ts` then flips with `shop_update_product`, and the
 * reason a caller that wants it live immediately (`webmcp.spec.ts`) has to
 * say so explicitly.
 */
export const SHOP_PRODUCT = {
  name: 'Custom print',
  slug: 'custom-print',
  description: 'Made to order, priced by the page.',
  currency: 'nzd',
  fulfillment_kind: 'manual',
};

/**
 * The offer, minus the `product_id` the route carries. Created as `draft` and
 * unpurchasable until `shop_publish_offer` (or
 * `POST …/offers/{id}/publish`) publishes it.
 */
export const SHOP_OFFER = {
  name: 'Custom print',
  mode: 'payment',
  currency: 'nzd',
  pricing_model: 'components',
  usage_type: 'licensed',
  billing_scheme: 'per_unit',
  tax_behavior: 'exclusive',
  variables: [
    {
      key: 'pages',
      kind: 'integer',
      label: 'Pages',
      required: true,
      minimum: '1',
      maximum: '20',
      step: '1',
      sort_order: 0,
    },
  ],
  components: [
    {
      key: 'pages',
      label: 'Printed pages',
      sort_order: 0,
      required: true,
      amount: { type: 'per_unit', input: 'pages', unit_amount_minor: 1500 },
    },
  ],
  checkout: {},
};

/**
 * [`SHOP_PRODUCT`] with a per-run-unique `name`/`slug`, live on creation.
 *
 * Only the native-server spec needs this: its database outlives the run, and
 * `slug` is unique per owner among non-deleted products, so a second run of
 * the same suite against the same server would collide. The browser sandbox
 * creates a fresh OPFS database per Playwright context, so it uses the plain
 * constant and gets a readable, stable slug.
 */
export function uniqueShopProduct(stamp: string) {
  return {
    ...SHOP_PRODUCT,
    name: `${SHOP_PRODUCT.name} ${stamp}`,
    slug: `${SHOP_PRODUCT.slug}-${stamp}`,
    status: 'active',
  };
}

/** The heading the agent's page carries; the proof a write reached the site. */
export const SHOP_HEADING = 'The print shop';

/**
 * The page the agent writes over the welcome starter site.
 *
 * Shared by `dev-workspace.spec.ts` (one product) and `dev-scenario.spec.ts`
 * (three): both specs have an agent publish THIS page and then assert on what
 * a shopper sees, so a second copy of it would be a second storefront to keep
 * in step with the catalog contract below.
 *
 * Deliberately what design §4.1's suggested prompt asks an agent for, not a
 * placeholder: it reads the *public* catalog (`/b/products/catalog`, the
 * anonymous surface — the `shop_*` tools are admin-only and a shopper has
 * none of them) and mounts the shipped storefront widget for each product.
 * `page.records` is the list field `CatalogProductListResponse` publishes
 * (`products/contracts.rs`); a page that guessed `items` would render an
 * empty shop, and a spec that only checked its own writes would still "pass"
 * up to its last assertion.
 *
 * Built through the DOM rather than `innerHTML` so the product name is text,
 * not markup — the shop is the boundary a prompt injection would cross, and a
 * fixture that pasted names into HTML would be modelling the wrong thing.
 *
 * `.shop-product-name` rather than a bare `h2`: the storefront widget renders
 * its own `h2` inside a shadow root, and Playwright's CSS engine pierces open
 * shadow roots, so `locator('h2')` would match two elements and fail strict
 * mode. Distinct class names keep the page's own markup addressable.
 *
 * # Why the page has to ask for `webmcp.js` itself
 *
 * `/b/webmcp/webmcp.js` is the deployment's WebMCP registration script at its
 * stable path, and design §16 scenario 5 requires the shopper's own agent to
 * have `list_products` on this page. Nothing puts that script here for it:
 * `ui::layout` injects the content-hashed `/b/static/webmcp-{hash}.js` into
 * every SSR-rendered ImpressPress page, but `/` on a sandbox is a file the
 * agent wrote, served verbatim by `wafer-run/web`, which injects nothing into
 * anybody's HTML. So a site that wants its visitors' agents to have tools has
 * to include the script itself, exactly as it has to include `storefront.js`
 * to get the purchase widget — and it does so at the stable path
 * (`GET /b/webmcp/webmcp.js`, `pipeline.rs`, `ui::assets::
 * WEBMCP_JS_STABLE_PATH`) rather than the content-hashed one, since a page
 * the agent writes has no SSR document to read the current hash off. The
 * blank seed's site guide and the prompt `/b/dev` suggests tell the agent to
 * write exactly this tag (`examples/dev-sandbox/seeds/blank/guide.md`, and
 * `suggested_prompt` in that seed's `sandbox.json`).
 */
export function shopPage(): string {
  return `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <title>${SHOP_HEADING}</title>
  <script src="/b/products/storefront.js" defer></script>
  <script src="/b/webmcp/webmcp.js" defer></script>
</head>
<body>
  <h1>${SHOP_HEADING}</h1>
  <ul id="products"></ul>
  <script>
    fetch('/b/products/catalog')
      .then(function (response) { return response.json(); })
      .then(function (page) {
        var list = document.getElementById('products');
        page.records.forEach(function (product) {
          var item = document.createElement('li');
          var heading = document.createElement('h2');
          heading.className = 'shop-product-name';
          heading.textContent = product.name;
          var widget = document.createElement('impresspress-product');
          widget.setAttribute('product-id', product.id);
          item.appendChild(heading);
          item.appendChild(widget);
          list.appendChild(item);
        });
      });
  </script>
</body>
</html>
`;
}
