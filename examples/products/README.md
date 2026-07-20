# Impresspress Products example suite

These ten standalone static sites exercise the real `<impresspress-product>` web component against deterministic API and Stripe responses. They never create a live charge.

## Run the suite

From `examples/`:

```sh
npm ci
npx playwright install chromium
npm run test:products
```

To browse the pages without the mocked API, serve the directory and open the matrix:

```sh
python3 -m http.server 4178 -d products
```

The pages intentionally use `http://127.0.0.1:4179` as a separate API origin. The automated suite intercepts that origin, serves the repository's real `storefront.js`, and mocks pricing, Checkout, embedded Stripe.js, Payment Links, and the marketplace ownership guard.

## What each directory contains

- `index.html` — a distinct customer-facing static website and embedded product widget.
- `commerce.fixture.json` — safe storefront data, an executable admin seed definition, operator/developer journeys, and the deterministic browser scenario.
- `README.md` — the example-specific setup and lifecycle notes.

`matrix.html` summarizes the template, pricing, presentation, ownership, fulfillment, and provider coverage. `shared/` contains only common presentation/runtime helpers; all commerce behavior remains in the public Impresspress component and API.

## Use with a real site

Replace the sample API origin and product identifier with the deployed Impresspress values:

```html
<script src="https://commerce.example/b/products/storefront.js" defer></script>
<impresspress-product
  api-base="https://commerce.example"
  product-id="your-product-id"
  presentation="hosted"
  credentials="omit">
</impresspress-product>
```

Use `credentials="omit"` for a public cross-origin static page. Configure the exact page origin in the admin allowlist, keep Stripe secret keys server-side, and publish immutable offers before embedding them.

## Visual baselines

The suite asserts desktop and 390px mobile screenshots for every site. Intentionally refresh baselines with `npm run test:products:update`, review every PNG change, then run `npm run test:products` once more without update mode.
