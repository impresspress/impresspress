# Products and Stripe commerce

This guide covers production setup, product and seller workflows, public/static storefront integration, Stripe webhooks, and recovery operations for the `impresspress/products` block.

Impresspress owns product configuration, authoritative pricing, immutable order snapshots, permissions, and analytics. Stripe remains authoritative for payment state, refunds, disputes, balances, and payouts. A browser return URL is never treated as proof of payment.

## Runtime support and security boundary

| Runtime | Public catalog and pricing | Hosted/embedded Checkout | Stripe secret operations |
| --- | --- | --- | --- |
| Native server | Yes | Yes | Yes |
| Cloudflare Worker with server-side secrets | Yes | Yes | Yes |
| Standalone browser/service-worker WASM | Read-only/local UI only | Only through an explicitly configured remote Impresspress commerce API or an existing Payment Link | No; fails closed |

Never put a Stripe secret key, webhook secret, API key, guest receipt token, or embedded Checkout client secret into source control, static HTML, browser storage intended for long-term persistence, logs, screenshots, or analytics. The publishable key is browser-safe, but Impresspress still masks it on admin pages to reduce accidental disclosure.

## Initial platform setup

Open **Admin → Products → Stripe setup** and configure/test the account there. At minimum:

| Setting | Purpose |
| --- | --- |
| `IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY` | Server-only `sk_test_…` or `sk_live_…` key. |
| `IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY` | Matching `pk_test_…` or `pk_live_…` key for embedded Checkout. |
| `IMPRESSPRESS__PRODUCTS__STRIPE_WEBHOOK_SECRET` | Signing secret for `/b/products/webhooks`. Test and live destinations have different secrets. |
| `IMPRESSPRESS__PRODUCTS__STRIPE_API_VERSION` | Stripe version sent by every provider request; default is `2026-02-25.clover`. Configure the webhook destination consistently. |
| `IMPRESSPRESS__PRODUCTS__DEFAULT_CURRENCY` | ISO three-letter default for new products. Each order remains single-currency. |
| `IMPRESSPRESS__PRODUCTS__PLATFORM_COUNTRY` | Two-letter platform country used by tax/shipping and Connect defaults. |
| `IMPRESSPRESS__PRODUCTS__CHECKOUT_ALLOWED_ORIGINS` | Comma-separated HTTPS origins allowed in success/cancel/return URLs. Localhost HTTP is accepted for development. |
| `IMPRESSPRESS__PRODUCTS__AUTOMATIC_TAX` | Default automatic-tax choice for new offers. An offer can override it. |
| `IMPRESSPRESS__PRODUCTS__STRIPE_API_URL` | Stripe base URL. Leave as `https://api.stripe.com` outside contract tests. |

The setup page validates the credentials against Stripe and reports account ID, mode, submitted details, and charge/payout capability without returning the secret.

### Test/live isolation

- Use matching publishable and secret key modes.
- Create separate Stripe webhook destinations and signing secrets for test and live mode.
- Do not reuse Stripe Product, Price, Customer, Checkout Session, Payment Link, Connect Account, or refund identifiers across modes.
- Impresspress persists `livemode`, account, currency, amount, and provider identity snapshots and rejects mismatched responses/events.
- Run an end-to-end test-mode purchase and webhook completion before switching configuration to live keys.

## Allowing users to sell

User selling is off by default. Set `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS=true` to enable the seller portal, then review:

| Setting | Meaning |
| --- | --- |
| `IMPRESSPRESS__PRODUCTS__SELLER_MODERATION_REQUIRED` | Defaults to `true`; sellers submit listings and an admin approves them. |
| `IMPRESSPRESS__PRODUCTS__SELLER_APPLICATION_FEE_BPS` | Platform application fee in basis points, from 0 to 10,000. |
| `IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_TEMPLATES` | Optional IDs from `simple_product`, `simple_subscription`, `configurable_product`, `configurable_subscription`. Blank allows all. |
| `IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CURRENCIES` | Optional ISO currency allowlist. |
| `IMPRESSPRESS__PRODUCTS__SELLER_ALLOWED_CATEGORIES` | Optional seller category allowlist. |
| `IMPRESSPRESS__PRODUCTS__SELLER_MAX_PRODUCTS` | Maximum non-deleted listings per seller; `0` is unlimited. |

The implementation uses one Stripe Express connected account per seller and direct charges in that account with an optional application fee. A Checkout Session is structurally one offer/one seller; mixed-seller carts are rejected.

Seller lifecycle:

1. The seller opens **Portal → Products**, starts Stripe onboarding, and returns through an allowed URL.
2. `account.updated` synchronizes capabilities and outstanding requirements.
3. The seller creates a product/offer from a template and submits it.
4. An administrator approves or rejects it under **Products → Sellers**.
5. Checkout remains blocked until moderation and connected-account charge capability both pass.
6. Sellers use an authenticated Stripe Express Dashboard login link for provider-managed balances, payouts, and dispute evidence. Impresspress provides owned order/refund operations and safe local summaries.

Administrative suspension is provider-first and blocks seller catalog mutations. Reactivation refreshes Stripe state before restoring access.

## Product and offer model

The creation wizard starts with four editable defaults:

- **Simple product:** one fixed one-time row.
- **Simple subscription:** one recurring row and interval.
- **Configurable product:** typed customer inputs plus itemized/conditional one-time rows.
- **Configurable subscription:** recurring itemized rows driven by quantities and choices.

Variables support exact decimal numbers, integers, booleans, dates, local date-times, single/multiple choices, and text, with defaults, bounds, steps, maximum length, visibility, required state, and help text. Dates use `YYYY-MM-DD`; local booking times use `YYYY-MM-DDTHH:MM` and should be interpreted in the product or venue's documented timezone.

Price rows support:

- fixed amount;
- amount per numeric input;
- fixed base plus per-unit amount;
- choice lookup;
- graduated and volume tiers;
- packages/blocks with exact or round-up policy;
- typed conditions including equality, comparison, membership, containment, and presence;
- nested `all`/`any`/`not` conditions through the advanced definition (preserved by the visual editor).

All money is stored as integer minor units. Pricing is evaluated on the server; clients send inputs, never a trusted total. Published offers are immutable. Duplicate an active offer to create a new editable draft/version.

The product manager provides authoritative scenario previews, an itemized explanation, visual draft editing, Stripe catalog sync/reconciliation, named preset lifecycle, Payment Link status/retry/copy/open controls, and generated static widget snippets.

## Checkout presentations

### Hosted Checkout

The API returns `checkout_url`; redirect the top-level browser to Stripe. This is the simplest option and works from static pages.

### Embedded Checkout

The API returns a short-lived `client_secret`. The widget loads Stripe.js and mounts embedded Checkout. A matching publishable key and server-side secret configuration are required.

### Payment Links

An active offer with no variables can create a link directly. A configurable offer first saves a validated named preset, then creates/reuses an immutable Stripe Payment Link snapshot for those values. Payment Links can be copied, opened, retried after sync failure, and deactivated.

Payment Links require saved Stripe shipping-rate IDs (`shr_…`). Hosted and embedded Checkout may use validated inline shipping rates.

## Static HTML widget

Replace the placeholder with the Impresspress API origin and a public active product ID:

```html
<script
  src="https://YOUR-IMPRESSPRESS-DOMAIN/b/products/storefront.js"
  defer
></script>

<impresspress-product
  api-base="https://YOUR-IMPRESSPRESS-DOMAIN"
  product-id="PRODUCT_ID"
  presentation="hosted"
  credentials="omit"
></impresspress-product>
```

Set `presentation="embedded"` for embedded Checkout or `presentation="payment_link"` to open a reusable link. For a cross-origin static site, list its origin in `WAFER_RUN_SHARED__CORS_ALLOWED_ORIGINS` (comma-separated, or `*`) so the browser is allowed to read the storefront projection and create Checkout; it is empty by default and fails closed. `IMPRESSPRESS__PRODUCTS__CHECKOUT_ALLOWED_ORIGINS` is a separate control over redirect URL origins and is not a CORS switch.

The custom element loads only the safe storefront projection, renders public variables, debounces authoritative previews, shows itemized totals, creates Checkout, and dispatches:

- `impresspress:ready` when product data is ready;
- `impresspress:checkout` when checkout is created;
- `impresspress:status` for guest order reconciliation;
- `impresspress:error` on a handled failure.

Style the host with `--ip-accent`, `--ip-border`, `--ip-bg`, `--ip-text`, and `--ip-muted` CSS custom properties.

### Ten complete static examples

The [`examples/products`](../examples/products/README.md) gallery contains ten no-framework sites covering a digital download, physical boutique, SaaS and usage subscriptions, membership, tickets, a course configurator, professional services, a Connect marketplace, and a donation campaign. Each site includes a strict commerce fixture, copy-paste integration, buyer journey, and developer README. The [comparison matrix](../examples/products/matrix.html) maps their templates, pricing models, checkout presentations, ownership, fulfillment, and Stripe features.

Run their API/widget assertions and desktop/mobile visual journeys with:

```sh
npm --prefix examples run test:products
```

### Content Security Policy

Impresspress-served pages already ship a Content-Security-Policy that permits embedded Stripe.js: the `WAFER_RUN_SHARED__CSP_DIRECTIVES` shared config var defaults to the Stripe origins below and is merged over the security-headers block's restrictive baseline (it can only widen the policy, never weaken it). Hosted Checkout and Payment Links are top-level navigations and need no CSP allowance; only embedded Checkout, which dynamically loads `https://js.stripe.com/clover/stripe.js`, does. The default value is:

```text
script-src https://js.stripe.com;
frame-src https://js.stripe.com https://hooks.stripe.com https://checkout.stripe.com;
connect-src https://api.stripe.com https://r.stripe.com;
```

A **cross-origin** page that embeds the widget is served by *your* site, not Impresspress, so it must carry an equivalent CSP of its own (the `WAFER_RUN_SHARED__CSP_DIRECTIVES` default only governs Impresspress-served pages). Extend the var — never below the baseline — to allow additional embeds, and narrow/extend based on the Stripe payment methods you enable. Impresspress currently uses hosted Express Dashboard login links rather than Connect.js embedded components. If Connect embedded components are introduced, also allow Stripe's documented Connect.js script/frame/connect origins; do not add them pre-emptively to a tighter current policy.

Stripe references: [Checkout security guidance](https://docs.stripe.com/security/guide), [Stripe.js](https://docs.stripe.com/js), and [Connect embedded components](https://docs.stripe.com/connect/get-started-connect-embedded-components).

## TypeScript SDK example

```ts
import { ImpresspressClient } from "@impresspress/sdk";

const client = new ImpresspressClient("https://YOUR-IMPRESSPRESS-DOMAIN");
const product = await client.products.getStorefrontProduct("PRODUCT_ID");
const offer = product.offers[0];

const quote = await client.products.previewPrice({
  offer_id: offer.id,
  quantity: 1,
  inputs: { seats: 5, plan: "pro" },
});

const checkout = await client.products.checkout({
  offer_id: offer.id,
  quantity: 1,
  inputs: quote.inputs,
  presentation: "hosted",
  success_url: "https://shop.example/thanks",
  cancel_url: "https://shop.example/product",
  buyer_email: "buyer@example.com",
});

if (checkout.checkout_url) location.assign(checkout.checkout_url);
```

Admin and seller SDK calls accept a commerce scope where applicable. For example, `previewManagedOffer(productId, offerId, { inputs }, "seller")` can evaluate an owned draft; public `previewPrice` only accepts purchasable active offers.

## Direct HTTP example

```sh
API=https://YOUR-IMPRESSPRESS-DOMAIN
PRODUCT_ID=PRODUCT_ID
OFFER_ID=OFFER_ID

curl "$API/b/products/storefront/$PRODUCT_ID"

curl -X POST "$API/b/products/pricing/preview" \
  -H 'content-type: application/json' \
  -d "{\"offer_id\":\"$OFFER_ID\",\"quantity\":1,\"inputs\":{\"seats\":5}}"

curl -X POST "$API/b/products/checkout" \
  -H 'content-type: application/json' \
  -d "{\"offer_id\":\"$OFFER_ID\",\"quantity\":1,\"inputs\":{\"seats\":5},\"presentation\":\"hosted\",\"success_url\":\"https://shop.example/thanks\",\"cancel_url\":\"https://shop.example/product\"}"
```

Checkout returns a one-time `receipt_token`. Treat it as a bearer capability. A static return page may poll:

```text
GET /b/products/orders/{order_id}/status?receipt_token={receipt_token}
```

The response deliberately omits buyer details and provider identifiers. The token expires; never publish or log it.

## Public endpoint rate limits

Anonymous commerce traffic is limited independently by client IP before any authenticated user limiter. Defaults are deliberately tighter for provider-creating checkout calls:

| Configuration | Default | Protects |
| --- | --- | --- |
| `WAFER_RUN_SHARED__RATE_LIMIT_PRODUCTS_PREVIEW` | `120/60` | Storefront detail and authoritative pricing preview. |
| `WAFER_RUN_SHARED__RATE_LIMIT_PRODUCTS_CHECKOUT` | `30/60` | Checkout Session and reusable Payment Link creation. |
| `WAFER_RUN_SHARED__RATE_LIMIT_PRODUCTS_RECEIPT` | `120/60` | Guest receipt/status polling. |

Values use `requests/seconds`; for example, `60/60` allows 60 requests per minute per IP. A bare number changes the request count while retaining the default window. `0` disables that category, which is not recommended for an internet-facing deployment. Rate-limit responses include `Retry-After`; signed Stripe webhooks are protected separately by signature verification, event claims, and idempotent retry handling.

## Stripe webhook destination

Register this HTTPS endpoint:

```text
POST https://YOUR-IMPRESSPRESS-DOMAIN/b/products/webhooks
```

Subscribe to platform events and, when Connect selling is enabled, connected-account events:

- `account.updated`
- `checkout.session.completed`
- `checkout.session.async_payment_succeeded`
- `checkout.session.async_payment_failed`
- `payment_intent.succeeded`
- `payment_intent.payment_failed`
- `payment_intent.processing`
- `payment_intent.requires_action`
- `payment_intent.canceled`
- `customer.subscription.updated`
- `customer.subscription.deleted`
- `invoice.paid`
- `invoice.payment_succeeded`
- `invoice.payment_failed`
- `charge.dispute.created`
- `charge.dispute.updated`
- `charge.dispute.closed`
- `refund.created`
- `refund.updated`
- `refund.failed`
- `charge.refunded`

Impresspress verifies the exact raw body with Stripe's timestamped signature, claims event IDs atomically, stores an integrity-protected replay payload, applies bounded leases/backoff, and reconciles state using event creation timestamps. Duplicate/stale events are safe; stale state cannot overwrite a newer terminal state. A successful PaymentIntent alone does not fulfill an order—Checkout identity and amounts must reconcile.

Return success from infrastructure only after the handler response is complete. Do not transform the body before signature verification.

## Stripe mutation idempotency policy

- Checkout Sessions use the immutable local purchase/order ID.
- Payment Links use the durable local Payment Link row ID/configuration.
- Stripe Products and Prices use stable local product/offer/component version identity; reconciliation reuses the same keys.
- Refunds use a durable local refund ID and retain that key in the provider-operation queue across ambiguous retries.
- Payment Link/Product/Price deactivation uses the durable local resource ID.
- Connect account creation uses the durable seller-account row ID.
- Single-use Account Links, Express login links, and Billing Portal sessions intentionally use a fresh UUID because a new short-lived URL is requested each time; a single provider request still carries that key.
- Never reuse one key for different accounts, modes, currencies, amounts, or request bodies. Impresspress treats such reuse as an integrity error.

## Recovery and reconciliation runbook

Use **Admin → Products → Stripe setup** for the two health queues.

### Failed webhook

1. Filter webhook delivery health by `retryable` or `dead_letter`.
2. Read the safe error projection; raw signed bodies and lease tokens are not shown.
3. Fix the database/config/provider condition.
4. Use replay only after confirming the event is failed/dead-letter. Replay re-enters the normal signed pipeline and retains idempotency/order guards.
5. Verify the event becomes `processed` and the affected order/subscription/seller projection is correct.

Admin APIs:

```text
GET  /b/products/api/admin/webhook-events
POST /b/products/api/admin/webhook-events/{event_id}/replay
```

### Ambiguous or pending provider operation

Refund operations are durably enqueued before calling Stripe. A worker uses bounded leases, the original idempotency key, exponential backoff, and dead-lettering.

```text
GET  /b/products/api/admin/provider-operations
POST /b/products/api/admin/provider-operations/reconcile?limit=25
```

The POST endpoint is suitable for a scheduled job authenticated with the deployment's admin/API-key mechanism. Wafer does not currently provide an internal cron lifecycle, so configure an external scheduler. Never expose this endpoint anonymously.

If an operation dead-letters, inspect Stripe using the provider refund ID/account, compare mode/currency/PaymentIntent/amount with the safe local order, correct the underlying issue, then use the normal admin reconciliation action. Do not manually mark a local refund successful.

### Incident response

- **Suspected secret leak:** revoke/rotate the Stripe key or webhook secret in Stripe, update Impresspress, retest connection/signatures, and inspect recent provider/webhook operations. Publishable-key rotation must match mode.
- **Webhook backlog:** keep the destination enabled, repair the failure, replay dead letters, and allow normal Stripe retries. Do not delete event ledger rows.
- **Provider timeout:** do not repeat a mutation with a new key. Use the reconciliation queue or the same durable local action.
- **Account mismatch:** stop processing, verify platform versus connected-account webhook scope and stored seller account. Never rewrite ownership to make an event fit.
- **Mode mismatch:** restore matching keys/destination; do not migrate test IDs into live rows.
- **Seller suspension/dispute:** use Impresspress suspension for catalog control and Stripe Express Dashboard for provider evidence, balance, and payout actions.

## Upgrade, rollback, and disable behavior

Commerce migrations are append-only and mirrored for SQLite/D1 and PostgreSQL. Back up the database before upgrade and apply all product migrations before serving traffic. Do not roll application code back across a migration boundary unless that version is known to tolerate the newer columns/tables.

To stop new seller activity, set `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS=false`; existing immutable orders and webhook processing remain readable/operational. To stop public sales, archive products/offers/links through the UI so Stripe resources are deactivated provider-first. Removing Stripe credentials is an emergency stop for new provider mutations, but it also prevents reconciliation and should not replace orderly archival.

Never drop order, line-item, subscription, event, refund, dispute, or provider-operation tables during a rollback. They are the audit/recovery record needed to reconcile Stripe.
