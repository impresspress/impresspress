# Route table single source, PR 6: products

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `products` so one `const ROUTES: &[EndpointRoute<Route>]` over wire paths is the block's only description of its HTTP surface: it dispatches every request and generates `info().endpoints` through `endpoint_match::declare`. Delete the two-hop dispatch (`mod.rs::handle`'s `strip_prefix` chain that rewrites `/b/products/api/admin/...` to `/admin/b/products/...` and `/b/products/api/...` to `/b/products/...`, then `handlers/dispatch.rs`'s `ADMIN_ROUTES` / `USER_ROUTES` matched with `dispatch_path` over the rewritten form), the hand-matched SSR page arms, and the path-matching `PUBLIC_RATE_LIMIT_ROUTES`. The per-route preconditions (`requires_user_products`, `requires_unsuspended_seller`, the pages' own `user_products_enabled` gate) and the rate-limit buckets become functions of the `Route` variant. Handlers read `{id}`, `{product_id}`, `{offer_id}`, `{preset_id}` and `{link_id}` only as the table bound them, which retires `crud::crud_delete`, `crud::crud_delete_owned`, `crud::path_id`'s prefix fallback, `util::path_param`, `rate_limit::RouteLimit` and `rate_limit::check_route_limits`.

**Architecture:** PR 1 made `EndpointRoute<H>` carry the declaration and added `declare`, `request_schema_of`, `response_schema_of`. `products` today matches a path in four places: `mod.rs::handle` (SSR pages by `strip_prefix`/`match sub`, the webhook by `starts_with`, the two API prefixes by `strip_prefix`), `PUBLIC_RATE_LIMIT_ROUTES` (three `(action, path)` predicates over the wire path), `ADMIN_ROUTES` (46 rows over `/admin/b/products/...`) and `USER_ROUTES` (51 rows over `/b/products/...`), plus the id readers in the handlers (`util::path_param(msg, "id", PREFIX)`, `crud::path_id(msg, PREFIX, ..)`, and five hand-rolled `msg.var("id")`-or-`strip_prefix` fallbacks). All of that becomes one 123-row table in a new `blocks/products/routes.rs`, one `match route` fan-out, three `const fn`s on the variant (`user_products_refusal`, `requires_unsuspended_seller`, `rate_limit_for`), and leaves that read `msg.var(..)`.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e` (`BlockEndpoint`, `AuthLevel`, `HttpMethod`, `Message::var`), `schemars` 1, `serde_json`, `maud`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md` (this plan implements its "PR 6": section 3's **products** paragraph and sequencing item 6). Models: the five earlier plans under `docs/superpowers/plans/2026-09-05-route-table-*.md`, especially PR 5's (`...-5-admin.md`) for the served-path inventory and the render guard, and PR 3's auth-ui `rate_limit_for(Route)` for rate limits keyed on the variant.

## Reconciliation result (Task 1) — STOP POINT

The inventory in Task 1 finds **no declared endpoint without a handler** and **no served handler without a declaration**. It does find one structural discrepancy the brief said to stop on:

**Every one of the 51 `USER_ROUTES` rows is served at two wire spellings and declared at one.** `mod.rs::handle` enters `handle_user` twice: from `path.strip_prefix("/b/products/api")` (normalizing `/b/products/api/X` to `/b/products/X`) and from the raw `/b/products/X` fallthrough. So `GET /b/products/catalog` (declared, `public`) and `GET /b/products/api/catalog` (undeclared) both reach `catalog::handle_catalog`; `GET /b/products/api/products` (declared, `authenticated`) and `GET /b/products/products` (undeclared) both reach `product::handle_user_list_products`. 31 rows are declared under `/b/products/api/...` (own products, offers, presets, payment links, seller), 20 under `/b/products/...` (own groups, types, group-templates, catalog, storefront, orders, pricing, purchases, checkout, subscription, billing-portal). The other 51 spellings are served and undeclared; each resolves through `declared_access`'s fail-closed `Authenticated` fallback (the `/b/products` prefix tier is `Public`), which is never weaker than the declared twin (`handler_tests::dispatch_tables_are_backed_by_declared_endpoints` enforces exactly that), and for the 8 `public` rows is stricter.

Who uses the undeclared spellings: nobody outside the products tests. Grep over `crates/` (pages.rs, `assets/storefront.js`, the `impresspress-web` e2e specs), `packages/impresspress-js` (the SDK) and `docs/` finds only declared spellings. Inside `blocks/products/tests/` the undeclared spellings appear (a) as the normalized form the harness hands `handle_user` directly (`dispatch_user` with `/b/products/products`, `/b/products/seller/...`: 155 occurrences, not wire paths) and (b) in three router-level tests written to prove the aliases do not weaken auth: `restore_is_unreachable_for_a_non_admin_on_every_path_that_reaches_it`, `a_seller_cannot_restore_another_sellers_product_on_any_wire_spelling` (whose positive control requires the raw spelling to *work*), and `an_anonymous_caller_cannot_restore_a_deleted_product`.

Two smaller accidental shapes, same class as PR 5's decision 4: the webhook arm answers **any** method and any `/b/products/webhooks/...` suffix (declared: `POST /b/products/webhooks` only; Stripe, the e2e specs and the docs POST the exact path), and `strip_prefix("/b/products/api")` also admits `/b/products/apifoo` (which then 404s in the table).

**Options:**

- **(A) Declare every alias.** 51 new `products.endpoints.json` lines, and 43 of them carry schemas today, so `products.openapi.json` would gain 43 path entries; both snapshot gates break, and the block keeps two URLs per handler, which is the shape the restore escalation (`handler_tests.rs` docs) came from.
- **(B) One spelling per handler: the declared one.** `ROUTES` is exactly the 123 declared rows; the 51 aliases (and the webhook's extra shapes) 404 from the matcher. Both snapshots byte-identical. The three alias tests are rewritten to the single spelling (their negative halves still hold; the positive control on the raw spelling goes), `dispatch_tables_are_backed_by_declared_endpoints` is subsumed by `table_tests::info_endpoints_come_from_the_table` (the table *is* the declaration), and the harness's `dispatch_admin`/`dispatch_user` become one `dispatch` through `ProductsBlock::handle` with the tests spelling wire paths.

**Recommendation: (B).** It is what the brief's text already prescribes ("ONE `const ROUTES` over the wire paths exactly as `info()` declares them", no rewrite), it matches PR 5's precedent for accidental shapes, and it removes the "every spelling" class of auth bug structurally rather than by test. Tasks 2–4 below are written for (B); if (A) is chosen, Task 3 step 1 instead adds 51 rows (each a copy of its twin's metadata with the alternate path) and the snapshot step regenerates both files.

## Decisions taken while planning (recorded, not re-litigated once the stop point is resolved)

1. **The table lives in `blocks/products/routes.rs`; the fan-out stays in `handlers/dispatch.rs`.** 123 rows with 48 schema functions is ~1,000 lines; beside a 1,500-line `mod.rs` that is its own module. `routes.rs` holds `Route`, `ROUTES`, the schema functions, the three variant functions and their pinning tests; `handlers/dispatch.rs` keeps its name and becomes one `pub(in crate::blocks::products) async fn run(ctx, msg, route, input)` — the merge of `handle_admin`'s and `handle_user`'s two matches plus the page arms — so the leaf modules keep their `pub(super)` visibility. `mod.rs::handle` is dispatch → rate limit → gates → `handlers::run`. Nothing in `run` reads a path.
2. **Gates keep both refusal texts.** The pages answer `"User product selling is disabled"`, the API `"user products are not enabled"`; both are user-visible, and unifying them is a wording change outside this PR. `user_products_refusal(route) -> Option<&'static str>` returns the text the route has always answered (or `None`), so the gate and its wording are one exhaustive match. `requires_unsuspended_seller(route) -> bool` is copied as is. No test asserts either text today; the pinning test does.
3. **Rate limits: three IP buckets, per-user read/write for every other API row, none for pages and the webhook.** Today `check_route_limits` matched the three public routes by wire path and `check_user_rate_limit` covered everything that reached the API section (`retrieve` → `api_read`/`API_READ`, else `api_write`/`API_WRITE`, skipped without a user); SSR pages, the settings POST and the webhook returned before either. `rate_limit_for` restates that per variant. Order changes from rate-limit-then-match to match-then-rate-limit, so an unroutable path now 404s without spending a bucket; no routable request changes bucket.
4. **The shared helper is `rate_limit::apply_route_limit(limiter, ctx, msg, key, category, limit) -> Option<OutputStream>`**: the identity resolution `check_route_limits` and auth-ui's `apply_rate_limit` both carry (IP identity, or the user id, or skip when user-keyed and anonymous), then `check_rate_limit`, returning only the 429 (both callers discard `Allowed` headers; the follow-up noted in auth-ui moves here). auth-ui's `apply_rate_limit` becomes `rate_limit_for(route)?` plus one call. `RouteLimit`, `check_route_limits` and `check_user_rate_limit` (products was the last caller of all three; files uses `check_user_rate_limit_with`) are deleted with their tests, replaced by two tests on `apply_route_limit`.
5. **Two rows, one variant, for the two root spellings.** `GET /b/products` and `GET /b/products/` both dispatch `PortalHome`; `GET /b/products/admin` and `/b/products/admin/` both dispatch `AdminOverview`. Both spellings are declared today and must stay declared (snapshot), and the same handler, gate and bucket apply; auth-ui's `Verify` is the precedent.
6. **Page handlers keep their signatures; `handle` passes `msg.var("id")`.** `product_manager(ctx, msg, id, admin)`, `deleted_product_close(..)`, `admin_purchase_detail(..)`, `admin_seller_detail(..)`, `seller_order_detail(..)`, `my_purchase_detail(..)` take the id the arm used to slice and decode; the arm now passes the matcher's already-decoded binding. The 14 page render tests that call these functions directly stay unchanged.
7. **`crud::path_id(msg, label)` stays, reading only `msg.var("id")`.** `get_owned`/`update_owned`/`delete_owned` and `group::handle_update_group` compose it; `OwnedResource.path_prefix` goes. `crud_delete` / `crud_delete_owned` (one-liners over `delete_record` / `delete_owned` + `ok_json`) are inlined at their three callers and deleted, as the module doc has said they would be.
8. **`dispatch_path` is left in `endpoint_match.rs`** with no caller after this PR; PR 7 deletes it (brief constraint).
9. **`routing.rs` table untouched.** The new routing test asserts the anonymous webhook POST dispatches *and* that `declared_access`/`endpoint_auth` resolve it `Public` from `ProductsBlock::new().info()` alone; the carve-out (`router_final`) still short-circuits `route_to_block` today, so the dispatch half is not yet discriminating, the resolution half is what makes PR 7's deletion safe. The doc comment of `restore_product_endpoint_is_admin_only_end_to_end` that describes the two-spelling world is corrected (tests only).

## Global Constraints

- Only `crates/impresspress-core/src/blocks/products/**`, `blocks/crud.rs`, `blocks/rate_limit.rs`, `util.rs` (deletions only), `blocks/auth_ui/mod.rs` (only `apply_rate_limit` calling the shared helper), `routing.rs` (tests only), and this plan change. No change to wafer-run, `endpoint_match.rs`, the router table, or any other block. Snapshots: `products.openapi.json` byte-identical; `products.endpoints.json` byte-identical under option (B); every other `*.openapi.json` / `*.endpoints.json` byte-identical. `UPDATE_OPENAPI_SNAPSHOTS=1` is never run.
- Metadata copied verbatim: summary, description, tags, `.agent_tool(name, description)`; `.input::<T>()` → `.input(request_schema_of::<T>)`, `.output::<T>()` → `.output(response_schema_of::<T>)`, `.query_params::<T>()` → `.query_params(request_schema_of::<T>)`, `.path_params_schema(x.clone())` → `.path_params(x_fn)`, `.input_schema(json!(..))` / `.output_schema(..)` / `.query_params_schema(..)` → named `fn`s holding the same literal. Table order = today's `info()` order (`declare` preserves it; `serde_json` here has no `preserve_order`, so the OpenAPI document is order-independent anyway). Two orderings matter for first-match dispatch and are already right: `/b/products/my-products/new` before `/b/products/my-products/{id}`, `/b/products/storefront/config` before `/b/products/storefront/{product_id}`.
- Handlers read path variables only through `msg.var(..)`. After the migration `grep -rn 'path_param(\|strip_prefix("/b\|starts_with("/b\|strip_prefix("/admin\|dispatch_path(\|/admin/b/products' crates/impresspress-core/src/blocks/products crates/impresspress-core/src/blocks/crud.rs crates/impresspress-core/src/util.rs crates/impresspress-core/src/blocks/rate_limit.rs` prints nothing outside test-only string assertions.
- TDD: write the test, run it and see it fail for the expected reason, then implement. Commits carry the two trailer lines:
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Format with `cargo +nightly fmt --all`. Lint with `cargo clippy -p impresspress-core --all-targets -- -D warnings`. `cargo test -p impresspress-core --no-fail-fast` has one known unrelated failure, `lockfile_loads_remote_block`; every other test must pass. Also `cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot`.
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase1/route-table-products` (from `origin/main` at `b8b8f29d`). The session's shell guard refuses compound commands containing `git` or shell variables; those go in a script under the scratchpad directory and run with `bash <script>`.

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `crates/impresspress-core/src/blocks/products/routes.rs` | New. `Route` (121 variants), 123-row `ROUTES` over wire paths with every declaration's metadata, the 48 schema functions (moved from `info()`), `user_products_refusal`, `requires_unsuspended_seller`, `rate_limit_for`; `table_tests`, `gate_tests`, `rate_limit_tests`. |
| `crates/impresspress-core/src/blocks/products/mod.rs` | `info()` = `.endpoints(endpoint_match::declare(routes::ROUTES))`; `handle` = dispatch → `apply_route_limit` → gates → `handlers::run`. `PUBLIC_RATE_LIMIT_ROUTES`, `view_schema` (moves to routes.rs), the `strip_prefix` chain and the page `match` are gone. |
| `crates/impresspress-core/src/blocks/products/handlers/dispatch.rs` | One `run(ctx, msg, route, input)` fan-out over `Route`; `AdminRoute`, `UserRoute`, both tables, `handle_admin`, `handle_user` and their gate methods deleted; `user_products_enabled` stays here. |
| `crates/impresspress-core/src/blocks/products/handlers/mod.rs` | Re-exports `run` and `user_products_enabled`; the `ADMIN_ROUTES`/`USER_ROUTES` test re-export and the `handle_admin`/`handle_user` export go; module doc rewritten. |
| `crates/impresspress-core/src/blocks/products/handlers/{product,group,types,provider,catalog}.rs`, `purchase.rs` | Every id read is `msg.var(..)`; `ADMIN_*_PREFIX` constants, `path_param` calls, `crud::path_id(.., PREFIX, ..)` calls and the five `strip_prefix` fallbacks gone; `handle_delete_type`, `handle_delete_group`, `handle_user_delete_group` compose the primitives. |
| `crates/impresspress-core/src/blocks/products/pages.rs` | Doc comment at ~290 names `routes::user_products_refusal`; no code change. |
| `crates/impresspress-core/src/blocks/products/tests/harness.rs` | `dispatch(ctx, msg, input)` = `ProductsBlock::new().handle(..)` replaces `dispatch_admin`/`dispatch_user`; `dispatch_routed` unchanged. |
| `crates/impresspress-core/src/blocks/products/tests/*.rs` | Wire spellings (`/b/products/api/admin/...`, `/b/products/api/products...`, `/b/products/api/seller/...`); the three alias tests on one spelling; `dispatch_tables_are_backed_by_declared_endpoints` deleted; new `page_link_tests.rs` (render guard). |
| `crates/impresspress-core/src/blocks/crud.rs` | `crud_delete`, `crud_delete_owned`, `id_from_path` deleted; `path_id(msg, label)` reads `msg.var("id")`; `OwnedResource` loses `path_prefix`; module doc rewritten. |
| `crates/impresspress-core/src/util.rs` | `path_param` deleted. |
| `crates/impresspress-core/src/blocks/rate_limit.rs` | `apply_route_limit` added; `RouteLimit`, `check_route_limits`, `check_user_rate_limit` and their tests deleted. |
| `crates/impresspress-core/src/blocks/auth_ui/mod.rs` | `apply_rate_limit` calls `apply_route_limit`. |
| `crates/impresspress-core/src/routing.rs` | New test `stripe_webhook_is_public_from_the_products_declaration_alone`; one doc comment corrected. |
| `docs/superpowers/plans/2026-09-05-route-table-6-products.md` | This plan. |

---

### Task 0: Commit this plan

- [ ] **Step 1: Commit**

```
docs: plan for phase 1 PR 6 (products)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

Then stop and report the reconciliation (section above) before Task 2.

---

### Task 1: The declared / served inventory

Sources: `info()` (`mod.rs:533–1337`, 123 `BlockEndpoint`s = the 123 lines of `tests/snapshots/products.endpoints.json`); `mod.rs::handle` (`1342–1549`: the settings POST, the SSR `match`, the webhook `starts_with`, the two API `strip_prefix`s and the raw fallthrough); `handlers/dispatch.rs` `ADMIN_ROUTES` (46 rows, `/admin/b/products/...`) and `USER_ROUTES` (51 rows, `/b/products/...`) with `requires_user_products` (37 variants) and `requires_unsuspended_seller` (21); `PUBLIC_RATE_LIMIT_ROUTES` (`mod.rs:96–119`) and `check_user_rate_limit` (`rate_limit.rs:915–932`). Nothing reads `ctx.caller_id()`; there is no inter-block path to keep ahead of the matcher.

Column key: **UP** = gated on `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS` (`user_products_refusal` is `Some`; text "api" = `"user products are not enabled"`, "page" = `"User product selling is disabled"`); **S** = `requires_unsuspended_seller`; **bucket** = `rate_limit_for` (`R` = `User/api_read/API_READ`, `W` = `User/api_write/API_WRITE`, named = IP bucket, `—` = none). Every row's `auth` is the declared level in the snapshot. "Old" is the template/arm that served it.

**A. Admin JSON API, `/b/products/api/admin/…`** (46 rows; old: `handle` rewrote `/b/products/api/admin{rest}` → `/admin/b/products{rest}`, `ADMIN_ROUTES` matched it; all `admin`, no gates)

| # | Method, wire path | `Route` | Dispatches to | Bucket |
|---|---|---|---|---|
| 1 | `GET …/products` | `AdminListProducts` | `product::handle_list_products` | R |
| 2 | `POST …/products` | `AdminCreateProduct` | `product::handle_create_product` | W |
| 3 | `GET …/products/{id}` | `AdminGetProduct` | `product::handle_get_product` | R |
| 4 | `PATCH …/products/{id}` | `AdminUpdateProduct` | `product::handle_update_product` | W |
| 5 | `DELETE …/products/{id}` | `AdminDeleteProduct` | `product::handle_delete_product` | W |
| 6 | `POST …/products/{id}/duplicate` | `AdminDuplicateProduct` | `product::handle_duplicate_product` | W |
| 7 | `POST …/products/{id}/approve` | `AdminApproveProduct` | `sellers::approve_product` | W |
| 8 | `POST …/products/{id}/reject` | `AdminRejectProduct` | `sellers::reject_product` | W |
| 9 | `POST …/products/{id}/restore` | `AdminRestoreProduct` | `product::handle_restore_product` | W |
| 10 | `GET …/products/{product_id}/offers` | `AdminListOffers` | `offers::handle_list(.., OfferAccess::Admin)` | R |
| 11 | `POST …/products/{product_id}/offers` | `AdminCreateOffer` | `offers::handle_create` | W |
| 12 | `GET …/offers/{offer_id}` | `AdminGetOffer` | `offers::handle_get` | R |
| 13 | `POST …/offers/{offer_id}/preview` | `AdminPreviewOffer` | `offers::handle_preview` | W |
| 14 | `PATCH …/offers/{offer_id}` | `AdminUpdateOffer` | `offers::handle_update` | W |
| 15 | `POST …/offers/{offer_id}/publish` | `AdminPublishOffer` | `offers::handle_publish` | W |
| 16 | `POST …/offers/{offer_id}/sync` | `AdminSyncOffer` | `offers::handle_sync` | W |
| 17 | `POST …/offers/{offer_id}/duplicate` | `AdminDuplicateOffer` | `offers::handle_duplicate` | W |
| 18 | `DELETE …/offers/{offer_id}` | `AdminArchiveOffer` | `offers::handle_archive` | W |
| 19 | `GET …/offers/{offer_id}/presets` | `AdminListPresets` | `payment_links::list_presets` | R |
| 20 | `POST …/offers/{offer_id}/presets` | `AdminCreatePreset` | `payment_links::create_preset` | W |
| 21 | `GET …/presets/{preset_id}` | `AdminGetPreset` | `payment_links::get_preset` | R |
| 22 | `PATCH …/presets/{preset_id}` | `AdminUpdatePreset` | `payment_links::update_preset` | W |
| 23 | `DELETE …/presets/{preset_id}` | `AdminArchivePreset` | `payment_links::archive_preset` | W |
| 24 | `GET …/offers/{offer_id}/payment-links` | `AdminListPaymentLinks` | `payment_links::list_links` | R |
| 25 | `POST …/offers/{offer_id}/payment-links` | `AdminCreatePaymentLink` | `payment_links::create_link` | W |
| 26 | `DELETE …/payment-links/{link_id}` | `AdminDeactivatePaymentLink` | `payment_links::deactivate_link` | W |
| 27 | `GET …/groups` | `AdminListGroups` | `group::handle_list_groups` | R |
| 28 | `POST …/groups` | `AdminCreateGroup` | `group::handle_create_group` | W |
| 29 | `PATCH …/groups/{id}` | `AdminUpdateGroup` | `group::handle_update_group` (`crud::path_id`) | W |
| 30 | `DELETE …/groups/{id}` | `AdminDeleteGroup` | `group::handle_delete_group` (was `crud_delete`) | W |
| 31 | `GET …/types` | `AdminListTypes` | `types::handle_list_types` | R |
| 32 | `POST …/types` | `AdminCreateType` | `types::handle_create_type` | W |
| 33 | `DELETE …/types/{id}` | `AdminDeleteType` | `types::handle_delete_type` (was `crud_delete`) | W |
| 34 | `GET …/purchases` | `AdminListPurchases` | `purchase::handle_list_admin` | R |
| 35 | `GET …/purchases/{id}` | `AdminGetPurchase` | `purchase::handle_get_admin` | R |
| 36 | `POST …/purchases/{id}/refund` | `AdminRefundPurchase` | `purchase::handle_refund` | W |
| 37 | `GET …/stats` | `AdminStats` | `stats::handle_stats` | R |
| 38 | `GET …/stripe/status` | `AdminStripeStatus` | `provider::connection_status` | R |
| 39 | `GET …/webhook-events` | `AdminWebhookEvents` | `provider::webhook_events` | R |
| 40 | `POST …/webhook-events/{id}/replay` | `AdminReplayWebhookEvent` | `provider::replay_webhook_event` (was `path_param`) | W |
| 41 | `GET …/provider-operations` | `AdminProviderOperations` | `provider::provider_operations` | R |
| 42 | `POST …/provider-operations/reconcile` | `AdminReconcileProviderOperations` | `provider::reconcile_provider_operations` | W |
| 43 | `GET …/sellers` | `AdminListSellers` | `sellers::list` | R |
| 44 | `GET …/sellers/{id}` | `AdminGetSeller` | `sellers::get` | R |
| 45 | `POST …/sellers/{id}/suspend` | `AdminSuspendSeller` | `sellers::suspend` | W |
| 46 | `POST …/sellers/{id}/reactivate` | `AdminReactivateSeller` | `sellers::reactivate` | W |

**B. Seller JSON API, `/b/products/api/…`** (31 rows, all `authenticated`; old: `USER_ROUTES` over `/b/products/…`, reached from `/b/products/api/…` (declared) and `/b/products/…` (undeclared alias, see stop point). Every row is UP(api); S where marked.)

| # | Method, wire path | `Route` | Dispatches to | S | Bucket |
|---|---|---|---|---|---|
| 47 | `GET /b/products/api/products` | `ListOwnProducts` | `product::handle_user_list_products` | | R |
| 48 | `POST …/products` | `CreateOwnProduct` | `product::handle_user_create_product` | S | W |
| 49 | `GET …/products/{id}` | `GetOwnProduct` | `product::handle_user_get_product` | | R |
| 50 | `PATCH …/products/{id}` | `UpdateOwnProduct` | `product::handle_user_update_product` | S | W |
| 51 | `DELETE …/products/{id}` | `DeleteOwnProduct` | `product::handle_user_delete_product` | S | W |
| 52 | `POST …/products/{id}/restore` | `RestoreOwnProduct` | `product::handle_user_restore_product` | S | W |
| 53 | `POST …/products/{id}/duplicate` | `DuplicateOwnProduct` | `product::handle_user_duplicate_product` | S | W |
| 54 | `GET …/products/{product_id}/offers` | `ListOwnOffers` | `offers::handle_list(.., Owner)` | | R |
| 55 | `POST …/products/{product_id}/offers` | `CreateOwnOffer` | `offers::handle_create` | S | W |
| 56 | `GET …/offers/{offer_id}` | `GetOwnOffer` | `offers::handle_get` | | R |
| 57 | `POST …/offers/{offer_id}/preview` | `PreviewOwnOffer` | `offers::handle_preview` | | W |
| 58 | `PATCH …/offers/{offer_id}` | `UpdateOwnOffer` | `offers::handle_update` | S | W |
| 59 | `POST …/offers/{offer_id}/publish` | `PublishOwnOffer` | `offers::handle_publish` | S | W |
| 60 | `POST …/offers/{offer_id}/sync` | `SyncOwnOffer` | `offers::handle_sync` | S | W |
| 61 | `POST …/offers/{offer_id}/duplicate` | `DuplicateOwnOffer` | `offers::handle_duplicate` | S | W |
| 62 | `DELETE …/offers/{offer_id}` | `ArchiveOwnOffer` | `offers::handle_archive` | S | W |
| 63 | `GET …/offers/{offer_id}/presets` | `ListOwnPresets` | `payment_links::list_presets` | | R |
| 64 | `POST …/offers/{offer_id}/presets` | `CreateOwnPreset` | `payment_links::create_preset` | S | W |
| 65 | `GET …/presets/{preset_id}` | `GetOwnPreset` | `payment_links::get_preset` | | R |
| 66 | `PATCH …/presets/{preset_id}` | `UpdateOwnPreset` | `payment_links::update_preset` | S | W |
| 67 | `DELETE …/presets/{preset_id}` | `ArchiveOwnPreset` | `payment_links::archive_preset` | S | W |
| 68 | `GET …/offers/{offer_id}/payment-links` | `ListOwnPaymentLinks` | `payment_links::list_links` | | R |
| 69 | `POST …/offers/{offer_id}/payment-links` | `CreateOwnPaymentLink` | `payment_links::create_link` | S | W |
| 70 | `DELETE …/payment-links/{link_id}` | `DeactivateOwnPaymentLink` | `payment_links::deactivate_link` | S | W |
| 71 | `GET /b/products/api/seller/account` | `SellerAccount` | `provider::seller_status` | | R |
| 72 | `GET …/seller/stats` | `SellerStats` | `stats::handle_seller_stats` | | R |
| 73 | `GET …/seller/orders` | `SellerOrders` | `purchase::handle_list_seller` | | R |
| 74 | `GET …/seller/orders/{id}` | `SellerOrder` | `purchase::handle_get_seller` | | R |
| 75 | `POST …/seller/orders/{id}/refund` | `SellerRefund` | `purchase::handle_seller_refund` | S | W |
| 76 | `POST …/seller/onboarding` | `SellerOnboarding` | `provider::seller_onboarding` | S | W |
| 77 | `POST …/seller/dashboard` | `SellerDashboard` | `provider::seller_dashboard` | | W |

Undeclared aliases served today for this section: the same 31 `(method, path)` with `/b/products/api/` replaced by `/b/products/` (e.g. `GET /b/products/products`, `POST /b/products/seller/onboarding`).

**C. User and public JSON API, `/b/products/…`** (20 rows; old: `USER_ROUTES`, reached from the raw path (declared) and from `/b/products/api/…` (undeclared alias))

| # | Method, wire path | Auth | `Route` | Dispatches to | UP | S | Bucket |
|---|---|---|---|---|---|---|---|
| 78 | `GET /b/products/groups` | authenticated | `ListOwnGroups` | `group::handle_user_list_groups` | api | | R |
| 79 | `POST /b/products/groups` | authenticated | `CreateOwnGroup` | `group::handle_user_create_group` | api | S | W |
| 80 | `GET /b/products/groups/{id}` | authenticated | `GetOwnGroup` | `group::handle_user_get_group` (`crud::get_owned`) | api | | R |
| 81 | `PATCH /b/products/groups/{id}` | authenticated | `UpdateOwnGroup` | `group::handle_user_update_group` (`crud::update_owned`) | api | S | W |
| 82 | `DELETE /b/products/groups/{id}` | authenticated | `DeleteOwnGroup` | `group::handle_user_delete_group` (was `crud_delete_owned`) | api | S | W |
| 83 | `GET /b/products/groups/{id}/products` | authenticated | `OwnGroupProducts` | `group::handle_user_group_products` | api | | R |
| 84 | `GET /b/products/types` | authenticated | `ListTypes` | `types::handle_list_types` | | | R |
| 85 | `GET /b/products/group-templates` | authenticated | `ListGroupTemplates` | `group::handle_user_list_group_templates` | | | R |
| 86 | `GET /b/products/catalog` | public, tool `list_products` | `Catalog` | `catalog::handle_catalog` | | | R |
| 87 | `GET /b/products/catalog/{id}` | public | `CatalogItem` | `catalog::handle_get_product_public` | | | R |
| 88 | `GET /b/products/storefront.js` | public | `StorefrontWidget` | `commerce::handle_storefront_widget` | | | R |
| 89 | `GET /b/products/storefront/config` | public, tool `get_storefront_config` | `StorefrontConfig` | `commerce::handle_storefront_config` | | | R |
| 90 | `GET /b/products/storefront/{product_id}` | public, tool `get_product` | `StorefrontProduct` | `commerce::handle_storefront_product` | | | R |
| 91 | `GET /b/products/orders/{id}/status` | public, tool `get_order_status` | `GuestOrderStatus` | `commerce::handle_guest_order_status` | | | `Ip/products_receipt/PRODUCTS_RECEIPT` |
| 92 | `POST /b/products/pricing/preview` | public, tool `preview_price` | `PricingPreview` | `commerce::handle_preview` | | | `Ip/products_preview/PRODUCTS_PREVIEW` |
| 93 | `GET /b/products/purchases` | authenticated, tool `list_my_purchases` | `ListPurchases` | `purchase::handle_list_user` | | | R |
| 94 | `GET /b/products/purchases/{id}` | authenticated | `GetPurchase` | `purchase::handle_get` | | | R |
| 95 | `POST /b/products/checkout` | public, tool `start_checkout` | `Checkout` | `stripe::handle_checkout` | | | `Ip/products_checkout/PRODUCTS_CHECKOUT` |
| 96 | `GET /b/products/subscription` | authenticated | `Subscription` | `subscription::handle_subscription` | | | R |
| 97 | `POST /b/products/billing-portal` | authenticated | `BillingPortal` | `provider::billing_portal` | | | W |

Rows 86–92 are `public` and user-keyed `R` today only when a session exists (`check_user_rate_limit` skips anonymous callers); `apply_route_limit` keeps that skip. Undeclared aliases served today for this section: the same 20 with `/b/products/` replaced by `/b/products/api/` (e.g. `GET /b/products/api/catalog`, `POST /b/products/api/checkout`), each at the `Authenticated` fallback; the three IP buckets did not match the alias spelling (they compared the wire path), so an alias spent the user bucket instead.

**D. Webhook**

| # | Method, wire path | Auth | `Route` | Dispatches to | Bucket |
|---|---|---|---|---|---|
| 98 | `POST /b/products/webhooks` | public | `Webhook` | `stripe::handle_webhook` | — |

Old arm: `path == "/b/products/webhooks" || path.starts_with("/b/products/webhooks/")`, any action, before the rate limiter. Undeclared shapes narrowed: non-POST methods and any `/b/products/webhooks/…` suffix.

**E. SSR pages** (25 rows, 23 variants; old: `handle`'s `retrieve` `match` over `sub`; `create /b/products/admin/settings` arm; no rate limit)

| # | Method, wire path | Auth | `Route` | Dispatches to | UP |
|---|---|---|---|---|---|
| 99, 100 | `GET /b/products`, `GET /b/products/` | authenticated | `PortalHome` | `pages::portal_home` | |
| 101 | `GET /b/products/my-products` | authenticated | `MyProductsPage` | `pages::my_products` | page |
| 102 | `GET /b/products/my-products/new` | authenticated | `NewProductPage` | `pages::product_wizard(.., false)` | page |
| 103 | `GET /b/products/my-products/{id}` | authenticated | `MyProductPage` | `pages::product_manager(.., msg.var("id"), false)` | page |
| 104 | `GET /b/products/my-products/{id}/close` | authenticated | `MyProductClosePage` | `pages::deleted_product_close(.., msg.var("id"), false)` | page |
| 105 | `GET /b/products/my-purchases` | authenticated | `MyPurchasesPage` | `pages::my_purchases` | |
| 106 | `GET /b/products/my-purchases/{id}` | authenticated | `MyPurchasePage` | `pages::my_purchase_detail(.., msg.var("id"))` | |
| 107 | `GET /b/products/selling` | authenticated | `SellingPage` | `pages::seller_dashboard` | page |
| 108 | `GET /b/products/selling/orders` | authenticated | `SellingOrdersPage` | `pages::seller_orders` | page |
| 109 | `GET /b/products/selling/orders/{id}` | authenticated | `SellingOrderPage` | `pages::seller_order_detail(.., msg.var("id"))` | page |
| 110, 111 | `GET /b/products/admin`, `GET /b/products/admin/` | admin | `AdminOverview` | `pages::overview` | |
| 112 | `GET /b/products/admin/manage` | admin | `AdminManagePage` | `pages::manage_products` | |
| 113 | `GET /b/products/admin/new` | admin | `AdminNewProductPage` | `pages::product_wizard(.., true)` | |
| 114 | `GET /b/products/admin/products/{id}` | admin | `AdminProductPage` | `pages::product_manager(.., msg.var("id"), true)` | |
| 115 | `GET /b/products/admin/products/{id}/close` | admin | `AdminProductClosePage` | `pages::deleted_product_close(.., msg.var("id"), true)` | |
| 116 | `GET /b/products/admin/groups` | admin | `AdminGroupsPage` | `pages::groups` | |
| 117 | `GET /b/products/admin/purchases` | admin | `AdminPurchasesPage` | `pages::purchases` | |
| 118 | `GET /b/products/admin/purchases/{id}` | admin | `AdminPurchasePage` | `pages::admin_purchase_detail(.., msg.var("id"))` | |
| 119 | `GET /b/products/admin/sellers` | admin | `AdminSellersPage` | `pages::admin_sellers` | |
| 120 | `GET /b/products/admin/sellers/{id}` | admin | `AdminSellerPage` | `pages::admin_seller_detail(.., msg.var("id"))` | |
| 121 | `GET /b/products/admin/stripe` | admin | `AdminStripePage` | `pages::stripe_setup` | |
| 122 | `GET /b/products/admin/settings` | admin | `AdminSettingsPage` | `pages::settings` | |
| 123 | `POST /b/products/admin/settings` | admin | `AdminSaveSettings` | `pages::handle_save_settings(ctx, input)` | |

The old arms already refused an id containing `/` (`!rest.contains('/')`), so `{id}` binding one segment is the same shape; the old arms decoded the id with `util::url_path_decode`, which `dispatch` now does for every binding.

**Totals:** 123 rows, 121 variants; 123 declared before and after (option B). Gates: UP(api) on rows 47–83 (37, = the old `requires_user_products` list), UP(page) on rows 101–104 and 107–109 (7, = the old page arms), S on the 21 rows marked (= the old `requires_unsuspended_seller` list). Buckets: three IP rows (91, 92, 95), `—` on rows 98–123 (26 rows), `R`/`W` by method on every other row.

Paths that must stay unmatched: `/b/products/api`, `/b/products/api/admin`, `/b/products/apifoo`, `/b/products/admin/whatever`, `/b/products/admin/products/` (empty id), `/b/products/admin/products/x/y`, `POST /b/products/admin/manage`, `GET /b/products/webhooks`, `POST /b/products/webhooks/extra`, `/b/other`, `/`; and under option (B) the 51 aliases, spot-checked by `GET /b/products/api/catalog`, `GET /b/products/products`, `POST /b/products/seller/onboarding`, `POST /b/products/api/checkout`.

---

### Task 2 (RED): table, gate, rate-limit, render-guard and routing tests

**Files:** `products/routes.rs` (new, tests only at this step), `products/tests/page_link_tests.rs` (new), `products/tests/mod.rs`, `routing.rs`

- [ ] **Step 1: table tests.** Create `routes.rs` with `mod table_tests`: `info_endpoints_come_from_the_table` (length equals `ROUTES.len()`, per zipped pair method/path/auth equal); `every_path_the_block_served_resolves_to_a_row` over the 123 inventory entries as `(action, path, Route, bound vars)` including both root spellings, the two-variable offer paths, a percent-encoded id, plus the reverse check that every variant is reached; `paths_the_block_never_declared_stay_unmatched` over the list above. Register `mod routes;` in `mod.rs`. Run `cargo test -p impresspress-core --lib blocks::products::routes`. Expected: FAIL to compile (`ROUTES`, `Route` missing).
- [ ] **Step 2: gate pinning.** `mod gate_tests`: `OLD_USER_PRODUCTS_API: &[(HttpMethod, &str)]` (37 wire paths of section B and rows 78–83), `OLD_USER_PRODUCTS_PAGES: &[&str]` (7), `OLD_UNSUSPENDED_SELLER: &[(HttpMethod, &str)]` (21); `user_products_gate_is_the_old_assignment` asserts every listed entry is a row and, for every row, `user_products_refusal(row.handler)` equals `Some("User product selling is disabled")` / `Some("user products are not enabled")` / `None` per the lists; `seller_suspension_gate_is_the_old_assignment` likewise for `requires_unsuspended_seller`. Expected: FAIL to compile.
- [ ] **Step 3: rate-limit pinning.** `mod rate_limit_tests`: `rate_limits_are_the_old_assignments`: the three IP rules as `(HttpMethod, &str, &str category)`, the 26 unlimited `(HttpMethod, &str)` (rows 98–123), and for every other row the old `check_user_rate_limit_with` rule (`Get` → `("api_read", API_READ)`, else `("api_write", API_WRITE)`, `LimitKey::User`); assert key, category, `max_requests` and `window` per row. Expected: FAIL to compile.
- [ ] **Step 4: render guard.** `products/tests/page_link_tests.rs`, `every_link_a_products_page_emits_resolves_to_a_declared_row`: seed one live product with a published offer, one soft-deleted product with an offer and a payment link (`seed_a_deleted_product_with_a_money_surface`), one group, one purchase for `user_1` sold by seller `seller_a`, one seller account; `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS=true`. Render every row of section E through `ProductsBlock::new().handle(..)` (admin pages as `admin_1`, `manage` also with `?view=deleted`, `purchases` with `?status=completed`, user pages as `seller_a`/`user_1`). Extract, with an explicit `(needle, action)` table: `hx-get="`→retrieve, `hx-post="`→create, `hx-patch="`/`hx-put="`→update, `hx-delete="`→delete, `href="/b/`→retrieve, `data-offer-url="`/`data-presets-url="`/`data-links-url="`→retrieve, `data-preview-url="`→create, `fetch('`→retrieve except the `reconcile` literal (create), `commercePortalRedirect('`→create, and the JSON config keys `"action_url":"`/`"refund_url":"`→create, `"product_url":"`/`"product_collection":"`→retrieve. Every URL under `/b/products/` must `dispatch` against `ROUTES` with that action; URLs under another block's prefix (none expected; `/b/auth/` handled like admin's guard) resolve via `endpoint_auth`. Non-vacuity: the admin restore `hx-post`, the seller restore `hx-post`, both close-page `hx-delete`s, the seller `action_url`, the admin and seller `refund_url`s, the stripe page's four `fetch(` literals, the portal's three `commercePortalRedirect` literals, the purchases `hx-get` filter, and at least 40 URLs collected. Register `mod page_link_tests;` in `tests/mod.rs`. Expected: FAIL to compile.
- [ ] **Step 5: routing test.** In `routing.rs` tests beside `stripe_webhook_carveout_stays_reachable_with_no_session`: `stripe_webhook_is_public_from_the_products_declaration_alone`: `let infos = vec![ProductsBlock::new().info()]`; assert `endpoint_auth(&infos[0].endpoints, "create", "/b/products/webhooks") == Some(Public)` and `declared_access(&infos, "impresspress/products", &anon_msg("create", "/b/products/webhooks")) == RouteAccess::Public`; register `DispatchProbeBlock` as `impresspress/products` and assert `route_to_block(.., anon_msg(..), .., &infos, &[])` answers `DISPATCHED`. This one PASSES already (the declaration exists today); it is the guard for PR 7, not a RED step. Correct the doc comment of `restore_product_endpoint_is_admin_only_end_to_end` (drop the "more than one wire path" paragraph, point at `blocks::products::routes::table_tests`).

---

### Task 3 (GREEN): one table, one fan-out, gates and buckets on the variant

**Files:** `routes.rs`, `mod.rs`, `handlers/dispatch.rs`, `handlers/mod.rs`, `handlers/{product,group,types,provider,catalog}.rs`, `purchase.rs`, `pages.rs` (comment), `crud.rs`, `util.rs`, `rate_limit.rs`, `auth_ui/mod.rs`, tests

- [ ] **Step 1: `routes.rs`.** `enum Route` (121 variants, `Clone, Copy, Debug, PartialEq, Eq`, section order A–E). Move the schema `let`s out of `info()` as `fn`s returning `serde_json::Value`: `id_path_schema`, `product_id_path_schema`, `offer_path_schema`, `preset_path_schema`, `link_path_schema`, `offer_definition_schema`, `managed_offer_schema`, `offer_list_schema`, `product_duplicate_schema` (calls `view_schema::<contracts::ProductView>`, which moves here), and the inline literals as `webhook_event_list_query_schema`, `provider_operation_list_query_schema`, `reconcile_query_schema`, `catalog_item_path_schema` (no `additionalProperties`; not `id_path_schema`), `storefront_product_path_schema`, `webhook_event_schema`, `order_id_path_schema` (no `additionalProperties`), `receipt_token_query_schema`. `const ROUTES` in today's `info()` order with every row's metadata verbatim through `EndpointRoute::{public,authenticated,admin}`. `const fn user_products_refusal`, `const fn requires_unsuspended_seller`, `const fn rate_limit_for` as exhaustive matches.
- [ ] **Step 2: `mod.rs`.** `info()` = `.endpoints(endpoint_match::declare(routes::ROUTES))`, the schema block and `view_schema` gone; `PUBLIC_RATE_LIMIT_ROUTES` gone; `handle` = `dispatch` → `if let Some((key, category, limit)) = rate_limit_for(route) { apply_route_limit(..)? }` → `if let Some(refusal) = user_products_refusal(route) { if !user_products_enabled(ctx).await { return err_forbidden(refusal) } }` → `if requires_unsuspended_seller(route) { is_suspended(..) }` (same three outcomes as today) → `handlers::run(ctx, &msg, route, input)`. Imports trimmed (`util`, `crud`, `check_route_limits`, `check_user_rate_limit`, `RouteLimit`, `LimitKey`, `RateLimit`, `BlockEndpoint` go).
- [ ] **Step 3: `handlers/dispatch.rs`.** Delete `AdminRoute`, `UserRoute`, both tables, both gate methods, `handle_admin`, `handle_user`. Add `pub(in crate::blocks::products) async fn run(ctx, msg: &Message, route: Route, input) -> OutputStream` with the 121 arms per the inventory's "Dispatches to" column (page arms pass `msg.var("id")`). `handlers/mod.rs`: `pub(in crate::blocks::products) use dispatch::{run, user_products_enabled};` and the module doc rewritten (no `/admin/b/products` spelling).
- [ ] **Step 4: leaves on `msg.var`.** `product.rs`: the five `path_param` calls → `msg.var("id")`, `ADMIN_PRODUCT_PREFIX` gone. `provider.rs:89` → `msg.var("id").trim()`. `group.rs`: `ADMIN_GROUP_PREFIX` gone, `handle_update_group` → `crud::path_id(msg, "Group")`, `handle_delete_group` → `path_id` + `delete_record` + `ok_json`, `handle_user_delete_group` → `delete_owned` + `ok_json`, `handle_user_group_products` fallback gone, `USER_GROUP` loses `path_prefix`. `types.rs`: `ADMIN_TYPE_PREFIX` gone, `handle_delete_type` composes the primitives. `catalog.rs:52` and `purchase.rs:231,275,372` fallbacks → `msg.var("id")` (`admin_refund_id` deleted). `pages.rs:290` comment.
- [ ] **Step 5: shared deletions.** `crud.rs`: `id_from_path`, `crud_delete`, `crud_delete_owned` deleted; `path_id(msg, not_found_label)` reads `msg.var("id")`; `OwnedResource.path_prefix` removed; module doc's second layer paragraph rewritten. `util.rs`: `path_param` deleted. `rate_limit.rs`: add `apply_route_limit`; delete `RouteLimit`, `check_route_limits`, `check_user_rate_limit`, `TEST_ROUTES` and the two `check_route_limits_*` tests; add `apply_route_limit_limits_an_ip_bucket` and `apply_route_limit_skips_a_user_bucket_for_an_anonymous_caller`. `auth_ui/mod.rs`: `apply_rate_limit` = `let (key, category, limit) = rate_limit_for(route)?; apply_route_limit(limiter, ctx, msg, key, category, limit).await`; imports trimmed.
- [ ] **Step 6: tests on wire paths.** `harness.rs`: `dispatch_admin`/`dispatch_user` → `pub async fn dispatch(ctx, msg, input)` = `ProductsBlock::new().handle(ctx, msg, input)`. Scripted rewrite over `tests/*.rs`: `/admin/b/products/` → `/b/products/api/admin/`, `/b/products/products` → `/b/products/api/products`, `/b/products/seller/` → `/b/products/api/seller/`, `dispatch_admin(` / `dispatch_user(` → `dispatch(`. Then by hand: the three alias tests keep one spelling each (`restore_is_unreachable_for_a_non_admin_on_every_path_that_reaches_it` → the two declared paths that reach a restore handler; `a_seller_cannot_restore_another_sellers_product_on_any_wire_spelling` → renamed `..._product`, one spelling; `an_anonymous_caller_cannot_restore_a_deleted_product` → one spelling) with their doc comments rewritten; `dispatch_tables_are_backed_by_declared_endpoints` deleted; the `dispatch_path` comment at ~3028 → `dispatch`. Run `cargo test -p impresspress-core --lib blocks::products`. Expected: PASS.
- [ ] **Step 7: snapshots and gates.** `cargo test -p impresspress-core --test openapi_snapshot --test endpoint_surface`: both PASS with no file change (`git status --short -- crates/impresspress-core/tests/snapshots/` empty). Grep gate prints nothing outside test-only string assertions. `cargo +nightly fmt --all`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`. Commit as two commits: the migration (table, fan-out, leaves, harness, tests) and the shared deletions (`crud.rs`, `util.rs`, `rate_limit.rs`, `auth_ui`):

```
refactor(products): declare the products block from one route table over wire paths

`routes::ROUTES` (123 rows, wire paths, every declaration's summary,
schemas and agent tools) is now what `handle` dispatches on and what
`info()` is generated from. The two-hop dispatch goes: the
`strip_prefix` chain that rewrote `/b/products/api/admin/...` to
`/admin/b/products/...` and `/b/products/api/...` to `/b/products/...`,
the `ADMIN_ROUTES`/`USER_ROUTES` tables matched over the rewritten form
with `dispatch_path`, the hand-matched SSR page arms and the
path-matching `PUBLIC_RATE_LIMIT_ROUTES`. The per-route gates
(`ALLOW_USER_PRODUCTS`, seller suspension) and the rate-limit bucket are
exhaustive functions of the `Route` variant, each pinned against the
old assignment. Handlers read `{id}`, `{product_id}`, `{offer_id}`,
`{preset_id}` and `{link_id}` only as the table bound them.

Every `USER_ROUTES` row used to answer at two spellings (`/b/products/X`
and `/b/products/api/X`) with one declared; the undeclared spelling now
404s. No page, asset, SDK call or document used it. Both products
snapshots are byte-identical.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

```
refactor(rate-limit,crud): delete the helpers products was the last caller of

`crud::crud_delete`, `crud::crud_delete_owned`, `crud::path_id`'s
prefix-strip fallback, `OwnedResource::path_prefix`, `util::path_param`,
`rate_limit::RouteLimit`, `check_route_limits` and
`check_user_rate_limit` had products as their only caller. The identity
resolution `check_route_limits` and auth-ui's `apply_rate_limit` both
carried is one `apply_route_limit`, which both blocks call.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 4: Verify and open the PR

- [ ] **Step 1: full verification**

```
cargo +nightly fmt --all -- --check
cargo clippy -p impresspress-core --all-targets -- -D warnings
cargo test -p impresspress-core --no-fail-fast
cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot
grep -rn 'path_param(\|strip_prefix("/b\|starts_with("/b\|strip_prefix("/admin\|dispatch_path(\|/admin/b/products' crates/impresspress-core/src/blocks/products crates/impresspress-core/src/blocks/crud.rs crates/impresspress-core/src/util.rs crates/impresspress-core/src/blocks/rate_limit.rs
git status --short
git diff origin/main --stat -- crates/impresspress-core/tests/snapshots/
```

Expected: fmt clean; clippy clean; all tests pass except `lockfile_loads_remote_block`; grep prints nothing; working tree clean; the snapshot diff is empty.

- [ ] **Step 2: push and open the PR** with `bash <scratchpad>/push-and-pr.sh "refactor(products): declare the products block from one route table over wire paths" <body-file>`. Body: row count (123 before and after); the reconciliation (the 51 aliases, who used them, the decision); the snapshot diff (none); the deletions in `crud.rs`, `rate_limit.rs`, `util.rs`; the grep-gate output; tests routed through the table; the render-guard coverage; deviations; that `dispatch_path` is now caller-free for PR 7; trailer. Do not merge.

---

## Self-review

**Spec coverage (PR 6 scope):** sequencing item 6 ("two-hop dispatch merged, handler gates moved to enum data, webhook carve-out no longer needed"): Tasks 2–3 and decision 9. Section 3's products paragraph (one `ROUTES` over wire paths, gates as data on the variant, the webhook declared `public` and read by the router): Tasks 1–3, Task 2 step 5. Section 5 "Blocks" bullet (table test, served-paths test): Task 2 step 1. Carry-forward items for PR 6 (`crud` one-liners, `path_param`, `check_route_limits` and the shared identity helper, `dispatch_path` left for PR 7): Task 3 step 5, decision 8.

**Deviations recorded:** the stop point (51 undeclared alias spellings; option B recommended, awaiting the decision); the table lives in `routes.rs` and the fan-out in `handlers/dispatch.rs` rather than both in `mod.rs` (decision 1); two refusal texts kept (decision 2); match-before-rate-limit ordering (decision 3); `check_user_rate_limit` deleted alongside the two named helpers (decision 4).
