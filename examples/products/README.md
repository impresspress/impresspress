# Impresspress Products example suite

These ten standalone static sites exercise the real `<impresspress-product>` web component against deterministic API and Stripe responses. They never create a live charge.

## Built with Codex and GPT-5.6

We used Codex and GPT-5.6 to design and build the products feature through an extensive [14-phase implementation plan](../../docs/2026-07-19-products-stripe-implementation-plan.md). We began by auditing the existing products block and agreeing on the architecture and safety constraints, then worked in small, verified slices across the commerce schema, pricing engine, Stripe and Connect integrations, admin and seller experiences, buyer storefront, SDK, and documentation.

Each phase paired implementation with focused tests and a running verification log, allowing the plan to evolve without losing unfinished work or weakening its original safeguards. The final workflow was exercised through Rust and SDK suites, end-to-end Chromium journeys, responsive visual checks, and ten distinct example storefronts covering the supported product and payment experiences.

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
