# Products and Stripe implementation plan

Date: 2026-07-19
Status: in progress
Owner: ImpressPress products session

This file is the durable execution checklist for the products/commerce work. Check a box only when the implementation and the listed verification are both complete. Add short dated notes under a task when reality changes the design; do not silently remove unfinished scope.

## Outcome

ImpressPress will provide a production-capable product system for platform-owned and, when enabled, user-owned products. It will support simple products, subscriptions, and configurable offers made from conditional line items and typed pricing inputs. Stripe remains authoritative for money movement while ImpressPress owns product configuration, pricing explanation, order state, permissions, presentation, and analytics.

The buyer surfaces must work from static sites backed by a native or Cloudflare ImpressPress API. A standalone browser-WASM backend must never receive or store a Stripe secret.

## Fixed architecture decisions

- [x] Keep the existing `impresspress/products` block and evolve its routes, migrations, repositories, and UI instead of replacing it.
- [x] Store every monetary value as integer minor units plus an ISO currency; no floating-point commerce fields are created.
- [x] Allow guest checkout so a static public site can sell without requiring an ImpressPress login; associate authenticated buyers when a session exists.
- [x] Treat webhook-confirmed Stripe state as authoritative. A browser return URL is never proof of payment.
- [x] Use hosted Checkout redirects, embedded Checkout, Payment Links/buy buttons, and Billing Portal rather than collecting raw card data.
- [x] Resolve dynamic/conditional pricing on the server and send the resolved immutable line-item snapshot to Stripe.
- [x] Generate reusable Payment Links only for fixed or named-preset configurations. Arbitrary runtime variables require a new Checkout Session.
- [x] Use the platform Stripe account for admin-owned products.
- [x] When user selling is enabled, onboard each seller with Stripe Connect and use direct charges with an optional application fee. Enforce one seller per Checkout Session; do not pretend a multi-seller cart is atomic.
- [x] Use stable Stripe APIs and pin an explicit API version in the adapter. Do not build production behavior on preview Accounts v2 endpoints.
- [x] Keep Stripe secret keys and webhook secrets in sensitive server-side block config. Only publishable keys, link URLs, and short-lived client secrets may reach a storefront.
- [x] Fail closed for secret-key Stripe operations in the standalone browser-WASM runtime. It may consume an explicitly configured remote commerce API or render pre-created Payment Links.
- [x] Require moderation for user-created products by default: draft -> submitted -> approved/published (or rejected/suspended). A user cannot self-publish into the global catalog.
- [x] Preserve block-owned config declarations and SQL portability: `ConfigVar` is the config source of truth; SQLite and PostgreSQL migrations stay mirrored; block code uses `wafer-sql-utils`, not raw SQL.

## Pre-production reset (2026-07-20)

- [x] Removed formula pricing templates, generic variable CRUD, aggregate cart creation, purchase-ID checkout, deprecated refund verbs, and SDK template aliases.
- [x] Removed floating-point product/line-item price columns and duplicate input snapshots from fresh-install schemas.
- [x] Kept typed offer customer fields—including date fields for bookings—itemized price rows, order history, seller ownership, and Stripe checkout.

## Phase 0 — Audit and baseline

- [x] Read repository instructions and confirm there is no applicable `AGENTS.md`.
- [x] Inventory the existing products block, schema, routes, admin UI, user seller routes, SDK, static/browser runtimes, and examples.
- [x] Review current official Stripe capabilities and constraints for Checkout, Payment Links, Connect, webhooks, pricing, and embedded components.
- [x] Confirm the worktree is clean before implementation.
- [x] Establish the focused baseline against the checked-in Wafer revision: 137 products tests pass.
  - Command: run Cargo from `/tmp` with `--manifest-path <repo>/Cargo.toml` so the stale local `.cargo/config.toml` patch does not replace the lockfile pin.
- [x] Record important existing defects to cover with regression tests:
  - Local-only refund does not call Stripe.
  - Some post-claim checkout failures can leave an order stuck.
  - Webhook processing can mark events complete after swallowed subscription write failures.
  - Concurrent pending webhook deliveries can duplicate non-atomic side effects.
  - Checkout completion does not fully reconcile amount, currency, account, and session identity.
  - Portal navigation points to a not-found products page and seller pages are not fully declared/gated.
  - SDK product fields drift from the server and most commerce routes are missing.

## Phase 1 — Contracts, config, and safety rails

- [x] Write the public commerce contract: template, product, offer, variable, pricing preview, checkout, order, subscription, seller, and analytics JSON shapes.
- [x] Add shared typed Rust domain enums/structs with strict serde validation and stable wire names.
- [x] Add `IMPRESSPRESS__PRODUCTS__STRIPE_PUBLISHABLE_KEY` as a browser-safe but admin-masked block config value.
- [x] Add explicit Stripe API version, platform country/default currency, automatic-tax default, seller fee basis points, seller moderation, and checkout-origin allowlist settings.
- [x] Add connection status that validates credentials server-side through Stripe and returns account ID, livemode, capability summary, and actionable errors without exposing secrets.
- [x] Separate “not configured”, “test mode”, “live mode”, and “misconfigured” states in admin UI/API.
- [x] Reject secret-key operations when the runtime cannot protect secrets; cover browser-WASM with a regression test.
- [x] Define idempotency-key rules for every Stripe mutation.
- [x] Define safe return/cancel URL validation against configured origins; hosted Checkout uses it now and embedded Checkout must reuse it.

## Phase 2 — Commerce schema and repositories

- [x] Add mirrored SQLite/PostgreSQL migrations for versioned commerce data.
- [x] Extend products with explicit owner kind/ID, seller account, approval state, fulfillment kind, Stripe product ID, current version, and soft-delete/publish timestamps.
- [x] Add uniqueness and lookup indexes for owner-scoped slugs and Stripe identifiers.
- [x] Expand system templates from the placeholder `default` row to at least:
  - [x] Simple one-time product.
  - [x] Simple recurring subscription.
  - [x] Configurable one-time offer.
  - [x] Configurable recurring offer.
- [x] Add versioned offers/prices with integer amount, currency, one-time/recurring mode, interval, interval count, tax behavior, billing scheme, usage semantics, and Stripe Price ID.
- [x] Add ordered offer components (row items) with label, amount/formula, quantity rule, required/optional state, condition tree, recurrence, and Stripe Price mapping.
- [x] Expand variable definitions with type, label/help, required/default, allowed values, numeric bounds/step, and visibility.
- [x] Add checkout presets and Payment Link records with active state, Stripe IDs/URLs, configuration hash, and last-sync error.
- [x] Add seller accounts with user ownership, Connect account ID, onboarding/capability/charge/payout state, fee policy, and timestamps.
- [x] Extend purchases/orders with buyer identity/email, seller/platform account, amount breakdown, Stripe session/intent/customer IDs, checkout presentation, livemode, and reconciliation state.
- [x] Extend line-item snapshots with offer/component IDs, integer unit/subtotal/discount/tax/total amounts, seller, Stripe Price ID, and normalized input snapshot.
- [x] Add subscription-item and entitlement records without coupling the general commerce model to the existing platform-plan addon columns.
- [x] Add a durable provider-operation/outbox table for retryable Stripe mutations and outbound notifications.
- [x] Strengthen Stripe event claims with atomic processing ownership, attempts, next retry, terminal/dead-letter status, and last error.
- [x] Add repository modules beside each commerce-v2 owned table and keep ownership filters explicit.
- [x] Keep fresh-install migrations and schema parity tests mirrored for SQLite and PostgreSQL.

## Phase 3 — Pricing and template engine

- [x] Parse decimal admin input into integer minor units without binary floating-point conversion.
- [x] Use one typed evaluator for all offers; customer fields, conditions, and itemized rows resolve exact minor-unit amounts.
- [x] Implement typed input validation for number, integer, boolean, select, multi-select, and text inputs.
- [x] Implement a versioned JSON condition tree with `all`, `any`, and `not`, plus explicit comparison operators.
- [x] Implement row/component resolution with deterministic ordering and an explanation for included and excluded rows.
- [x] Support fixed, per-unit/per-seat, graduated-tier, volume-tier, package, and named-preset pricing.
- [x] Support one-time and recurring components in subscription Checkout within Stripe’s line-item limits.
- [x] Validate that every recurring component in one offer has compatible currency and Checkout mode.
- [x] Implement bounds, quantity limits, allowed combinations, min/max totals, and explicit rounding policy.
  - [x] Enforce typed input bounds/steps, offer quantity limits, conditional combinations, and explicit package rounding/rejection.
  - [x] Add offer-level minimum/maximum total policies and their checkout/storefront validation.
- [x] Return a signed/expiring pricing quote or recreate and compare the quote at checkout so client totals cannot be trusted.
- [x] Persist the exact evaluated inputs, conditions, component breakdown, template version, and offer version on the order.
- [x] Add a pricing preview/explanation API used by both admin builder and storefront.
- [x] Add property-focused tests for rounding, boundary conditions, condition nesting, invalid schemas, overflow, currency mismatch, and deterministic evaluation.

## Phase 4 — Stripe platform adapter

- [x] Split the current Stripe module into a shared typed client/adapter, provider-status adapter, and domain orchestration modules.
- [x] Add encoded request builders and strict resource-specific response decoding for Accounts, Products, Prices, Checkout Sessions, Payment Links, Refunds, Billing Portal, and Connect account links/login links.
- [x] Sync publishable products and immutable versioned prices to Stripe; create a new Stripe Price when price-defining fields change and deactivate obsolete prices safely.
  - [x] Create and durably reuse Stripe Products and immutable fixed-row Prices with stable idempotency keys, strict mode/identity/amount response checks, connected-account support, and saved-Price reuse in Checkout and Payment Links; keep evaluated dynamic rows as inline price data.
  - [x] Reconcile Product metadata and active state, strictly reconcile fixed Price identity/mode/amount/recurrence, repair missing Products and dependent Prices, and provider-first deactivate active Payment Links and fixed Prices when an immutable offer is archived.
- [x] Store and surface sync state/errors; provide admin retry and reconciliation actions.
  - [x] Persist syncing/synced/failed state and safe errors, and expose centrally authorized admin/seller sync and retry actions in the product manager and SDK.
  - [x] Make the explicit sync action reconcile remote objects, refresh mutable Product fields, reactivate valid inactive Prices, replace missing dependencies with repair-specific idempotency keys, and fail closed on immutable mismatches.
- [x] Create hosted Checkout Sessions and return a redirect URL.
- [x] Create embedded Checkout Sessions and return only the client secret/publishable configuration required by Stripe.js.
- [x] Create, update/deactivate, copy, and render shareable Payment Links and buy-button snippets for fixed/preset offers.
- [x] Pass real resolved components as Stripe line items rather than one aggregate “Order” line.
- [x] Support payment and subscription modes, promotion-code policy, automatic tax, billing/shipping address collection, shipping options, customer creation, trials, and consent settings from validated offer config.
  - Hosted/embedded Checkout accepts validated inline fixed shipping rates or saved Stripe rate IDs. Payment Links require saved `shr_` IDs because Stripe's Payment Link API does not accept inline rate data.
  - Selected shipping is reconciled against the immutable allowed-rate snapshot, stored separately on the order, included in the provider total equation, and rejected when the amount is not allowed.
- [x] Add server-side Stripe refunds and update local state only from a successful response/webhook; support partial refund accounting if exposed.
- [x] Add Billing Portal session creation for buyers to manage subscriptions and payment methods.
- [x] Reconcile checkout completion against expected order ID, account, seller, currency, amount, mode, session, and payment/subscription status.
- [x] Make webhook processing retryable: do not mark an event processed when required local writes fail.
- [x] Ensure only one worker owns a webhook event attempt and outbound side effects are idempotent.
- [x] Handle the required checkout, payment, refund, invoice, subscription, dispute, and Connect account events without relying on event order.
  - [x] Defer unpaid `checkout.session.completed` deliveries without fulfillment, then reconcile `checkout.session.async_payment_succeeded` or atomically fail the exact matching order on `checkout.session.async_payment_failed`; reusable Payment Links create no local order until payment succeeds.
  - [x] Propagate immutable order/offer metadata to payment-mode PaymentIntents and reconcile `payment_intent.succeeded`, `payment_intent.payment_failed`, `payment_intent.processing`, `payment_intent.requires_action`, and `payment_intent.canceled` into a timestamp-guarded diagnostic projection with account/livemode/currency/schema/status checks. PaymentIntent success alone never fulfills an order; exact Checkout Session reconciliation remains authoritative.
  - [x] Reconcile `invoice.payment_failed`, `invoice.paid`, and `invoice.payment_succeeded` for commerce subscriptions with connected-account/livemode validation, Clover invoice parent support, timestamp-guarded out-of-order writes, past-due-only recovery, and canceled-subscription resurrection prevention.
  - [x] Apply the same timestamp ordering to the legacy platform-billing projection: race-safe Checkout insert/update, stale Checkout protection after subscription/invoice/cancellation events, legitimate newer re-subscription support, and retryable webhook failures when the projection cannot be persisted.
  - [x] Persist and reconcile `charge.dispute.created`, `charge.dispute.updated`, and `charge.dispute.closed` in a timestamp-guarded durable ledger with immutable order/account/livemode/payment/currency/amount validation; expose safe order history and tenant-isolated open/lost dispute analytics while keeping evidence, balance, and payout actions in Stripe.
  - [x] Order `refund.created`, `refund.updated`, and `refund.failed` ledger projections and `account.updated` Connect capability projections by Stripe event creation time; equal-second Connect ambiguity merges toward the safer restricted state, stale events cannot regress state, and legitimate newer re-enablement remains supported.
- [x] Add scheduled/manual reconciliation for incomplete operations and dead-letter replay.
  - [x] Add integrity-checked, administrator-only dead-letter inspection and manual replay through the normal signed webhook pipeline.
  - [x] Add leased, bounded provider-operation reconciliation for ambiguous/pending refunds, preserving the original Stripe idempotency key and exact account/mode/amount/PaymentIntent snapshot; expose a safe administrator health panel and endpoint suitable for an authenticated external scheduler because Wafer has no native cron lifecycle.
- [x] Add mocked Stripe contract tests for success, decline, malformed response, timeout, retry, duplicate event, out-of-order event, account mismatch, and livemode mismatch.

## Phase 5 — Seller/Stripe Connect workflow

- [x] Keep the global `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` switch as the hard feature gate.
- [x] Add admin policy for application fee, moderation requirement, allowed templates/currencies/categories, and seller limits.
  - [x] Reflect allowed templates/currencies in the seller wizard and enforce every policy on direct product/offer create, update, duplicate, and publish calls.
- [x] Create one Connect account per seller and generate single-use authenticated Account Links for onboarding/updates.
- [x] Show requirements, charges enabled, payouts enabled, and remediation actions in the seller portal.
- [x] Use direct charges in the seller account context with an application fee and explicit connected-account webhook handling.
- [x] Block publication and checkout until required moderation and Stripe capabilities are satisfied.
- [x] Enforce seller ownership in every product, offer, preset, link, order, refund, analytics, and Connect route.
- [x] Enforce a single seller per cart/session and return an actionable validation error for mixed-seller carts.
  - [x] Keep typed Checkout structurally single-offer/single-seller and reject seller-owned products from the legacy aggregate platform cart so Connect context and fees cannot be bypassed.
- [x] Provide seller access to Stripe-hosted/embedded payment, refund, dispute, balance, and payout workflows appropriate to the chosen account configuration.
  - [x] Use hosted/embedded Checkout for buyers, owned refund actions in Impresspress, and authenticated Stripe Express Dashboard login links for provider-managed disputes, balances, and payouts.
- [x] Add admin seller list/detail, suspension, moderation queue, and capability/error views.
  - [x] Fail closed when Stripe rejects connected-catalog archival, keep the seller/product locally active for retry, and block seller catalog mutations while administratively suspended.
- [x] Add tests for disabled selling, incomplete onboarding, ownership isolation, moderation, suspension, fee calculation, connected-account headers, and mixed-seller rejection.
  - [x] Cover disabled selling, incomplete onboarding, ownership isolation, moderation/resubmission, suspension/reactivation, fee calculation, connected-account headers, and provider-failure retry.
  - [x] Reject the legacy multi-item seller cart with an actionable single-`offer_id` error and verify no local order remains.

## Phase 6 — Admin experience

- [x] Correct product admin navigation so it uses the admin shell and every link targets a declared route.
- [x] Build a guided Stripe setup screen: mode warning, masked credentials, connection test, webhook URL/events, status, and go-live checklist.
- [x] Build a product wizard: choose template -> basics/media -> pricing/rows/conditions -> checkout options -> preview -> save draft/publish.
- [x] Provide an advanced editor without making simple products require advanced fields.
- [x] Add complete create/edit/duplicate/archive flows for products, offers, variables, components, groups, and pricing presets.
  - [x] Add a shared admin/seller product detail page with metadata editing, publish/moderation/archive actions, immutable offer version summaries, draft JSON editing, offer publish/duplicate/archive actions, and clickable product rows.
  - [x] Add server-owned whole-product duplication with safe metadata copying, non-archived offers cloned as drafts, provider/link/preset isolation, seller ownership/moderation enforcement, and compensating cleanup.
  - [x] Finish the remaining visual variable/component/group/preset lifecycle forms.
- [x] Provide validation inline and retain submitted values after errors.
- [x] Add pricing preview with an itemized explanation and representative variable scenarios.
- [x] Add Stripe sync and Payment Link panels with copy buttons, status, retry, hosted preview, embedded snippet, and static-site usage guidance.
  - [x] Show offer sync status/errors and create/reuse/list/copy/deactivate immutable Payment Links, including named configurable-input presets.
  - [x] Add explicit sync retry, hosted preview, embedded snippet, and static-site usage guidance.
- [x] Add order detail with provider IDs, timeline, item/amount breakdown, buyer/seller, refund controls, and reconciliation state.
- [x] Add subscription detail and customer-portal action.
- [x] Add useful admin stats: gross/net revenue by currency, orders, conversion/completion, refunds, active/trialing/past-due subscriptions, MRR where meaningful, top products, seller fees, and failures requiring action.
  - [x] Add currency-separated open/lost dispute counts and disputed amounts sourced from the durable ledger.
- [x] Never combine currencies into a misleading single revenue value.
- [x] Add an administrator webhook-delivery health panel with safe status/error projection, retry timing, mode/account context, filters, and guarded replay actions.
- [x] Add accessible responsive UI tests for keyboard use, labels, focus/error state, narrow screens, and secret masking.

## Phase 7 — Buyer storefront and static-page developer surface

- [x] Add a public product/offer detail API that exposes only safe storefront configuration.
- [x] Add guest and authenticated pricing-preview/checkout APIs with independent per-IP preview, checkout, and guest-receipt abuse controls.
- [x] Provide a small progressively enhanced storefront script/custom element for product cards, variable forms, itemized quotes, hosted checkout, and embedded checkout.
- [x] Ensure the widget works in plain static HTML without a framework or build step.
- [x] Provide a Payment Link/buy-button path requiring no runtime ImpressPress API call after page load.
- [x] Implement checkout return/cancel states that poll/retrieve server order status and never claim success from query parameters alone.
- [x] Provide buyer order receipt/history for authenticated users and a safe guest receipt lookup mechanism.
- [x] Provide subscription status and Billing Portal launch.
- [x] Add CSP/script/origin documentation for Stripe.js and Connect.js.
- [x] Test native/Cloudflare-backed static pages and explicit browser-WASM fail-closed/remote-API behavior.

## Phase 8 — Seller portal experience

- [x] Fix the portal Products link and explicitly declare/gate all seller pages.
- [x] Hide seller navigation and reject seller pages/APIs when user selling is disabled.
- [x] Add seller onboarding/status dashboard with clear next actions.
- [x] Reuse the product wizard constrained by admin seller policy.
- [x] Add seller products, offers, links, orders, refunds, subscriptions, and moderation status pages.
  - [x] Add ownership-isolated seller product/offer/Payment Link management with moderation status and hard selling-feature gates.
  - [x] Add seller order, refund, and subscription pages.
- [x] Add seller stats by currency: gross sales, refunds, platform fees, clearly qualified pre-provider-fee proceeds, orders, subscriptions, top products, and recent failures.
  - [x] Add tenant-isolated gross sales, refunds, net sales, configured platform fees, orders, subscription states, top products, and failure counts per currency.
  - [x] Add safe recent-failure summaries and label locally derived proceeds as before Stripe fees, disputes, reserves, and payout adjustments.
  - [x] Include seller-owned PaymentIntent failures, required-action states, and cancellations in the safe recent-attention feed without misclassifying them as terminal failed orders.
  - [x] Add tenant-isolated open/lost dispute counts and disputed amounts with a clear Stripe-attention prompt.
  - [x] Never label locally derived proceeds as exact seller net: qualify them as before Stripe fees/disputes/reserves/payout adjustments and keep provider-exact fee, balance, and payout accounting in the authenticated Stripe Express surface.
- [x] Integrate appropriate Stripe Connect embedded/hosted management for payments, disputes, balances, and payouts.
- [x] Verify tenant isolation in SSR, JSON APIs, exports, analytics, and provider actions.

## Phase 9 — SDK, schemas, and documentation

- [x] Complete `BlockEndpoint` declarations and JSON schemas for every admin, seller, public, checkout, order, subscription, webhook-adjacent, and Connect route.
  - [x] Require every dispatched products JSON operation to have an explicit discovery schema and cover the guard with a regression test.
  - [x] Document commerce-v2 product rows, dual Checkout families, exact money, builder CRUD, offers, presets, Payment Links, orders/refunds, subscriptions, seller/Connect, admin operations, and safe signed-webhook recovery projections.
- [x] Remove SDK/server field drift and generate or share typed commerce interfaces where practical.
- [x] Expand `ProductsExtension` with catalog/detail, quote, checkout, order, subscription/portal, admin builder, Payment Link, seller, and administrator webhook-recovery methods.
- [x] Document secret handling, server versus browser-WASM deployment, webhooks, Connect, test/live modes, static widget usage, and recovery/reconciliation.
- [x] Include copy-paste plain HTML, TypeScript, and direct HTTP examples.
- [x] Add exhaustive table-driven SDK unit tests for every Products method URL, verb, body, auth expectation, and response type.

## Phase 10 — Continuous automated verification

- [x] Keep focused products tests green after every domain slice.
- [x] Add route-level tests through central routing so auth declarations and dispatch are tested together.
- [x] Add repository tests with real SQLite for state transitions and ownership boundaries.
- [x] Add PostgreSQL migration/type checks in CI with a PostgreSQL 16 service and full migration application.
- [x] Add Stripe mock-server contract coverage across provider mutations and webhook families.
- [x] Run the complete Rust workspace suite.
- [x] Run Rust formatting and clippy for affected targets/features.
- [x] Run JavaScript SDK tests/typecheck/build.
- [x] Run browser-WASM tests/build where products is enabled.
- [x] Run Cloudflare wasm checks/build with `block-products`.
- [x] Run the examples suite.
- [x] Record commands/results in the verification log below.

## Phase 11 — Playwright functional and visual coverage

- [x] Add deterministic seed helpers for admin, buyer, seller, products/offers, and mocked Stripe states.
- [x] Test admin Stripe setup/test-mode status without real credentials.
- [x] Test simple one-time product creation, preview, publish, hosted checkout, webhook completion, order display, and refund.
- [x] Test simple subscription creation, checkout, webhook lifecycle, and Billing Portal launch.
- [x] Test configurable rows/conditions with multiple variable combinations and itemized totals.
- [x] Test Payment Link creation/copy/buy-button rendering.
- [x] Test embedded Checkout mounting with Stripe.js mocked at the provider boundary.
- [x] Test administrator webhook filtering, safe projection, confirmation, replay, refresh, and failure recovery in Chromium.
- [x] Test guided-wizard shipping visibility, exact minor-unit serialization, country normalization, estimates, saved rate IDs, and invalid policies in Chromium.
- [x] Test seller disabled/enabled navigation, onboarding, moderation, publication, sales stats, and ownership isolation.
- [x] Test static no-framework storefront purchase flow.
- [x] Add narrow/mobile and desktop visual snapshots for the main admin, seller, and buyer states.
- [x] Replace the old not-found baseline with intentional buyer commerce-home and lifecycle assertions.
- [x] Run Playwright during implementation and again after the complete suite.

## Phase 12 — Ten example websites and experience mocks

- [x] 2026-07-20 — The user resumed this phase; all ten examples, strict fixtures, comparison matrix, browser journeys, visual baselines, and CI jobs are implemented.

Each example includes a distinct static site, seed/config fixture, developer README/snippet, buyer journey, relevant admin/seller journey, automated API assertions, and Playwright coverage. Provider calls use deterministic mocks/test mode; examples do not require live charges.

- [x] 1. Digital download shop — fixed one-time product, automatic fulfillment entitlement, hosted Checkout.
- [x] 2. Boutique physical store — size/color inputs, quantity, inventory, shipping address/rate, tax configuration.
- [x] 3. SaaS plans — monthly/annual subscription, per-seat quantity, optional onboarding fee, Billing Portal.
- [x] 4. Usage SaaS — fixed base plus graduated usage component/meter semantics and invoice lifecycle.
- [x] 5. Membership site — monthly/yearly choices, trial, member entitlement, past-due recovery state.
- [x] 6. Event tickets — ticket tiers, capacity/quantity limits, optional merchandise, confirmation receipt.
- [x] 7. Course configurator — base course plus conditional modules/certification rows and itemized quote.
- [x] 8. Professional services — package preset, variable hours/add-ons, deposit or recurring retainer.
- [x] 9. Multi-seller marketplace — two sellers with Connect onboarding, direct charges, moderation, fees, payouts; mixed-seller cart rejection is demonstrated.
- [x] 10. Donation/static campaign — customer-chosen amount within bounds, shareable fixed links, plain HTML widget, hosted and embedded variants.
- [x] Add a matrix page comparing template, pricing, checkout presentation, ownership, fulfillment, and Stripe features across all ten examples.
- [x] Run all ten examples in CI without port/database collisions.

## Phase 13 — Final production-readiness gate

- [x] All prior checkboxes are complete or their safety-preserving scope is explicitly documented.
- [x] No secret appears in HTML, JS bundles, logs, fixtures, screenshots, error responses, or browser-WASM storage.
- [x] Test/live Stripe resources cannot be mixed; livemode/account/currency/amount reconciliation is enforced.
- [x] Webhook retries, out-of-order delivery, duplicate delivery, and dead-letter replay are tested.
- [x] Refund and subscription state cannot report success when Stripe rejected the operation.
- [x] Seller data and Stripe account operations are ownership-isolated.
- [x] Accessibility and responsive checks pass for primary admin, seller, buyer, and example flows.
- [x] Full in-scope Rust, SDK, build, and Playwright suites pass from documented commands.
- [x] Upgrade/migration and rollback/disable behavior is documented.
- [x] Operator runbook covers credentials, webhook registration, Connect, reconciliation, incident response, and key rotation.
- [x] Developer and user experience review is exercised by ten distinct example sites, strict fixtures, responsive visual baselines, and deterministic browser journeys.

## Verification log

- [x] 2026-07-19 — Clean baseline: `cargo test --manifest-path <repo>/Cargo.toml -p impresspress-core blocks::products --locked` from `/tmp`; 137 passed, 0 failed.
- [x] 2026-07-19 — Focused products suite after config/currency/Checkout safety slice: 149 passed, 0 failed.
- [x] 2026-07-19 — Commerce-v2 SQLite upgrade/backfill and PostgreSQL parity tests: 4 passed, 0 failed.
- [x] 2026-07-19 — Focused products suite after commerce-v2 schema slice: 151 passed, 0 failed (before adding the parity-only test).
- [x] 2026-07-19 — Public commerce contracts, exact money parsing, and typed pricing/condition slice: 164 products tests passed, 0 failed.
- [x] 2026-07-19 — Persisted pricing preview, versioned offer lifecycle CRUD, seller ownership/moderation, and safe storefront detail API: 174 products tests passed, 0 failed.
- [x] 2026-07-19 — Typed hosted/embedded guest Checkout, exact component/order snapshots, Connect direct charges/application fees, recurring inline prices, and subscription webhook identity/item reconciliation: 180 products tests passed, 0 failed.
- [x] 2026-07-19 — Named checkout presets, reusable/deactivatable Payment Links, safe static-storefront link projection, immutable quote snapshots, exact Payment Link webhook reconciliation, replay/tamper protection, and SQLite/PostgreSQL migration parity: 184 products tests passed, 0 failed; `git diff --check` passed.
- [x] 2026-07-19 — Stripe credential/account health, Connect Express onboarding/status/dashboard links, connected-account capability webhooks, buyer Billing Portal ownership/account/mode safety, provider-first full/partial refunds, durable serialized refund ledger, exact cumulative refund webhooks, and tamper rejection: 205 products tests passed, 0 failed.
- [x] 2026-07-19 — Admin commerce shell/navigation, guided Stripe setup and safe go-live checklist, intentional buyer commerce home, feature-gated seller navigation/routes, local Connect capability/requirements dashboard, and billing/onboarding/dashboard actions: 210 products tests passed, 0 failed; 15 shared navigation tests passed, 0 failed.
- [x] 2026-07-19 — Shared admin/seller product wizard with four default templates, exact currency conversion, typed variables, conditional itemized rows, subscription recurrence, checkout options, review/draft/publish flow, moderation gating, JavaScript parse validation, and end-to-end handler/API sequences: 214 products tests passed, 0 failed.
- [x] 2026-07-19 — Clickable admin/seller product lists, centrally authorized lifecycle pages, product editing/publish/archive, moderation visibility, immutable offer inspection/draft editing/publish/duplicate/archive, offer sync state, configurable presets, Payment Link create/reuse/list/copy/deactivate, seller ownership isolation, JavaScript parse checks, and approved ten-example deferral: 216 products tests passed, 0 failed; `git diff --check` passed.
- [x] 2026-07-19 — Authenticated whole-product duplication for admin and sellers, safe metadata allowlist, fresh slug/identity, non-archived offer definitions cloned as drafts, Stripe IDs/links/presets excluded, seller ownership/moderation retained, cross-seller rejection, and compensating cleanup: 218 products tests passed, 0 failed; `git diff --check` passed.
- [x] 2026-07-19 — Authoritative commerce subscription lifecycle persistence and signed webhooks; currency-separated exact-minor-unit admin/seller analytics; seller-owned order/refund APIs; clickable admin/buyer/seller order lists; ownership-safe order/subscription detail with provider IDs, timeline, itemized amounts, reconciliation, refund history/actions, and Billing Portal: 225 products tests passed, 0 failed.
- [x] 2026-07-19 — Framework-free static storefront widget exercised through Chromium for hosted Checkout, immutable Payment Links, mocked embedded Checkout, and capability-protected return polling: 4 Playwright tests passed, 0 failed.
- [x] 2026-07-19 — Browser-WASM products build/check passed with Stripe secret operations fail-closed; JavaScript SDK tests/build passed with 49 tests before the webhook recovery API addition.
- [x] 2026-07-19 — Strict Checkout and Payment Link completion reconciliation, exact account/livemode/session/offer/mode/currency/subtotal/final-state validation, atomic webhook leases, bounded exponential retries, dead-letter state, payload tamper detection, and mirrored migrations: 239 products tests passed, 0 failed.
- [x] 2026-07-20 — Integrity-preserving Base64 storage for exact signed webhook bytes, safe administrator event projection, guarded dead-letter replay, central auth/dispatch coverage, and Stripe recovery-panel SSR coverage: 22 focused Stripe tests plus route/UI integration tests passed.
- [x] 2026-07-20 — Products SDK webhook inspection/replay types and methods: 50 SDK tests passed, 0 failed; CJS, ESM, and declaration builds passed.
- [x] 2026-07-20 — Exact graduated-tier, volume-tier, and package pricing with tier/schema/type/overflow validation; advanced shared wizard controls for all typed calculations and condition operators: 244 products tests passed, 0 failed; 50 SDK tests passed and CJS/ESM/declaration builds succeeded.
- [x] 2026-07-20 — Administrator webhook recovery browser coverage for safe rendering, filtering, refresh, cancelled/accepted replay, list failures, and replay failures: 2 Chromium Playwright tests passed, 0 failed.
- [x] 2026-07-20 — Validated shipping countries/rates/estimates, optional one-time Customer creation, correct declared automatic-tax/platform-country config lookup, Checkout inline rates, Payment Link saved-rate enforcement, itemized order shipping migration, and immutable shipping-total reconciliation/tamper rejection across hosted/embedded and Payment Link webhooks: 247 products tests passed, 0 failed; 50 SDK tests passed; 7 Chromium product tests passed; `git diff --check` passed.
- [x] 2026-07-20 — Delayed Checkout payment lifecycle: unpaid completion deferral, strict typed-order async success/failure reconciliation, exact-session legacy failure fallback, Payment Link order creation only after settlement, required-event setup guidance, and identity-tamper rejection: 248 products tests passed, 0 failed; 29 focused Stripe tests passed; 2 webhook-admin Playwright tests passed in desktop Chrome; `cargo fmt` and `git diff --check` passed.
- [x] 2026-07-20 — Currency-aware offer minimum/maximum item totals with strict schema validation, inclusive exact-minor-unit boundaries, explicit preview/Checkout/Payment Link/static-storefront enforcement, guided-wizard controls/review, manager visibility, and SDK types: 250 products tests passed, 0 failed; 13 pricing/API tests plus direct Checkout/Payment Link rejection passed; 1 guided-wizard Playwright test passed in desktop Chrome; 50 SDK tests and CJS/ESM/declaration builds passed.
- [x] 2026-07-20 — Durable Stripe Product/fixed-Price catalog sync with strict response validation, stable idempotency, connected-account context, persisted safe failure/retry state, resumable Product reuse, mixed saved-Price/dynamic-inline Checkout and Payment Link line items, central admin/seller routes, manager actions, and SDK method: 252 products tests passed, 0 failed; 2 end-to-end catalog mock tests plus route/UI tests passed; 50 SDK tests and CJS/ESM/declaration builds passed. Remote reconciliation and obsolete-Price deactivation remain unchecked Phase 4 work.
- [x] 2026-07-20 — Explicit Stripe catalog reconciliation and repair for Product metadata/active state, missing Products/dependent Prices, inactive Prices, strict one-time/subscription recurrence checks, platform/Connect contexts, and provider-first offer archival that deactivates shared Payment Links and fixed Prices before local visibility changes: 257 products tests passed, 0 failed; retry/idempotency/provider-rejection paths passed; 1 product-manager Playwright test passed in desktop Chrome for failed sync, restored retry, and reconcile reload.
- [x] 2026-07-20 — Administrator seller governance with Connect capability/error list and detail pages, seller-product moderation queue, approve/reject/resubmit lifecycle, provider-first suspension/reactivation, suspended mutation enforcement, typed SDK methods, and browser failure/retry flows: 262 products tests passed, 0 failed; 51 SDK tests and CJS/ESM/declaration builds passed; all 10 products Playwright tests passed in desktop Chrome.
- [x] 2026-07-20 — Seller policy enforcement and useful tenant-isolated analytics: seller template/currency/category/product-limit controls across API and wizard paths; legacy cross-seller cart rejection; per-currency sales/refund/platform-fee/subscription/top-product metrics; clearly qualified before-Stripe-fees proceeds; safe recent payment failures in SSR/API/SDK. The broad Rust products filter passed 270 tests plus 3 central authorization integration tests; 51 SDK tests and CJS/ESM/declaration builds passed; all 10 products Playwright tests passed in desktop Chrome.
- [x] 2026-07-20 — Complete products discovery contract: every dispatched JSON operation has a schema; missing group/type/template/pricing routes are explicitly authenticated; admin/seller products, offers, presets, Payment Links, legacy builders, buyer/order/subscription, seller/Connect, Stripe health, and webhook recovery use typed request/response/path/query contracts; product responses cover commerce-v2 ownership/moderation/catalog fields; OpenAPI secret/payload projections are pinned. The broad Rust products filter passed 271 tests plus 4 central route integration tests; focused exhaustive-schema and OpenAPI tests passed; SDK duplication and collection envelope drift was corrected, with 52 SDK tests and CJS/ESM/declaration builds passing; `cargo fmt` and `git diff --check` passed.
- [x] 2026-07-20 — Order-aware recurring invoice lifecycle: mirrored migration 014 stores Stripe event creation times; commerce/platform subscription writes use timestamp predicates; failed invoices mark past due; newer paid/succeeded invoices recover only past-due subscriptions; Clover nested subscription references are accepted; older failures and post-cancellation final invoices cannot overwrite/resurrect commerce state. The broad Rust products filter passed 273 tests plus 4 central route integration tests; focused invoice ordering, lifecycle identity, migration parity, and cancellation tests passed; `cargo fmt --check` and `git diff --check` passed.
- [x] 2026-07-20 — Durable Stripe dispute lifecycle: mirrored migration 015 and repository reconciliation persist created/updated/closed events with timestamp guards and immutable tenant/order/provider checks; administrator, buyer, and owning-seller order details show a safe dispute timeline; admin/seller analytics report currency-separated open/lost counts and amounts without crossing seller boundaries; Stripe-hosted evidence/balance/payout management remains the operational action surface. The broad Rust products filter passed 275 tests plus 4 central route integration tests; focused stats, webhook ordering, migration parity, order visibility, and OpenAPI tests passed; all 52 SDK tests and CJS/ESM/declaration builds passed.
- [x] 2026-07-20 — Platform subscription event ordering: replaced the unconditional Checkout conflict update with deterministic insert plus atomic timestamp-conditional updates and race recovery; stale Checkout/invoice events cannot overwrite newer subscription/cancellation state; a later genuine re-subscription updates provider identity and plan; platform subscription write failures are no longer acknowledged, allowing Stripe retries. The broad Rust products filter passed 277 tests plus 4 central route integration tests; focused stale-Checkout, invoice recovery/cancellation, re-subscription, and fault-injected persistence tests passed.
- [x] 2026-07-20 — PaymentIntent operational lifecycle: mirrored migration 016 stores ordered provider payment status and bounded safe failure diagnostics; Checkout sends server-owned order/offer metadata to one-time PaymentIntents and subscription identity metadata to Subscriptions; succeeded/failed/processing/requires-action/canceled events are tenant/account/mode/currency/schema/status validated and timestamp guarded; PaymentIntent success cannot bypass exact Checkout fulfillment; buyer/admin/seller order views and seller-owned recent-attention stats surface safe state. The broad Rust products filter passed 280 tests plus 4 central route integration tests; focused ordering, non-fulfillment, tamper, migration, form, SSR, tenant-isolation, and OpenAPI regressions passed; all 52 SDK tests and CJS/ESM/declaration builds passed.
- [x] 2026-07-20 — Refund/Connect event ordering and provider-operation recovery: mirrored migrations 017/018 timestamp-guard refund and seller capability projections and add atomic provider worker leases; stale/equal-time events fail closed without blocking legitimate newer recovery; every Stripe refund is enqueued before mutation; pending or ambiguous refunds are safely recreated/retrieved with their original idempotency/account/mode/identity/amount snapshot; bounded backoff/dead-lettering, an admin/scheduler API, safe UI, OpenAPI, and SDK methods are implemented. The broad Rust products filter passed 283 tests plus 4 central route integration tests; all 53 SDK tests and CJS/ESM/declaration builds passed; 3 desktop Chromium recovery tests passed; `cargo fmt` and the pinned lockfile were clean.
- [x] 2026-07-20 — Final product-management and developer-experience slice: owner/admin draft pricing previews; typed visual variables and component rows with safe preservation of nested conditions, quantity rules, metadata, and checkout settings; inline retained-value validation; preset create/edit/archive; Payment Link retry/deactivate/copy/hosted and embedded snippets; group and legacy pricing-template lifecycle forms; complete static-site/SDK/direct-HTTP/operator documentation. The broad Products Rust filter passed 283 tests plus 4 central authorization tests; 54 SDK tests and CJS/ESM/declaration builds passed; all 15 Products Chromium tests passed.
- [x] 2026-07-20 — Final release matrix: `cargo test --locked --workspace --exclude impresspress-web --exclude impresspress-cloudflare` passed (including 1,208 core unit tests plus all native integration/doc tests); `cargo check --locked -p impresspress-cloudflare --target wasm32-unknown-unknown --features block-products` passed; strict core clippy passed with warnings denied apart from the two known non-Products argument-count APIs, and the repository's official workspace clippy command passed; `cargo fmt --all -- --check`, `git diff --check`, and the Cargo.lock immutability check passed. All 15 Products Chromium flows were rerun after cleanup and passed.
- [x] 2026-07-20 — Completed resumed examples/release slice: shared Stripe client adapter, safe malformed/timeout decoding, independent public IP rate limits, exhaustive Products SDK request contracts, deterministic full lifecycle browser mocks, and all ten static experience sites. The broad Products Rust suite passed 284 tests; all 19 Products Chromium tests and all 21 example matrix tests passed with committed desktop/mobile baselines; 55 SDK tests plus CJS/ESM/declaration builds passed.
- [x] 2026-07-20 — Applied all 18 PostgreSQL migrations to a clean PostgreSQL 16 container and asserted 22 Products tables, four default templates, all commerce purchase fields, and provider workflow lease/order fields. Both CI workflows parse successfully and include PostgreSQL, Products browser, and ten-example jobs.
- [x] 2026-07-20 — Final rerun after documentation/formatting: the native workspace passed, including 1,214 core tests plus every native integration/doc test; strict core clippy passed with warnings denied except the explicitly allowed argument-count category; browser-WASM and Cloudflare `block-products` WASM checks passed; Rust formatting, patch whitespace, pinned Cargo.lock, and workflow YAML checks passed; the 19-test Products lifecycle suite and 21-test example suite passed concurrently without port collisions.
- [x] Full Rust workspace tests — CI-equivalent native workspace suite passed on 2026-07-20, including 1,214 core tests (Wasm-only crates verified separately).
- [x] Rust fmt/clippy — formatting passed; official workspace clippy passed with only two pre-existing non-Products argument-count warnings; strict Products/core clippy passed with warnings denied apart from that known category.
- [x] SDK tests/typecheck/build — 55 tests passed; CJS, ESM, and declaration builds succeeded on 2026-07-20; lint has 0 errors and 9 existing `no-explicit-any` warnings.
- [x] Browser-WASM tests/build — products-enabled browser check passed on 2026-07-20.
- [x] Cloudflare wasm check/build — `impresspress-cloudflare` passed for `wasm32-unknown-unknown` with `block-products`.
- [x] Examples API tests — all ten fixtures strictly deserialize through the Rust domain model and assert exact pricing components/totals; browser tests assert exact storefront/preview/checkout payloads.
- [x] Playwright functional suite — all 19 Products Chromium tests pass: static storefront, hosted/embedded/Payment Link checkout, guest receipt, one-time/refund and subscription lifecycle, guided wizard, product manager/editor, catalog admin lifecycle, webhook/provider recovery, seller gate/onboarding/moderation/stats/ownership, accessibility, and responsive states.
- [x] Playwright visual suite — committed desktop/mobile baselines cover the primary admin, seller, buyer, and all ten example storefront states.
- [x] Ten-example matrix run — 21 Playwright tests pass across ten isolated static sites plus the comparison matrix.

## Initial audit snapshot (before implementation)

The current block already provides product/group/type CRUD, a public catalog, formula pricing, purchase snapshots, hosted one-time Stripe Checkout, signed/idempotency-recorded webhooks, subscriptions used by the platform plan, admin SSR pages, user-owned product/group CRUD behind a shared switch, SDK stubs, and 137 focused tests.

The implementation must not confuse those foundations with complete commerce. Existing templates contain only names, Checkout sends one aggregate line, refunds are local-only, subscription checkout is absent, seller rows have no Connect identity or moderation, the portal products link is broken, stats are minimal and currency-unsafe, the SDK is incomplete, and existing Playwright/examples do not execute a real product lifecycle.
