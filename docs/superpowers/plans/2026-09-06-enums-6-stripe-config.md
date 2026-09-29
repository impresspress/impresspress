# Enums and error discipline, PR 6: one Stripe classification, one seller fee, one country default, one truth table

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Four values that are read in more than one place stop having more than one answer. A Stripe response is classified as retryable or terminal by `StripeClient` alone, so a 503 during catalog sync is no longer persisted as `Stripe rejected catalog synchronization`. `SELLER_APPLICATION_FEE_BPS` is parsed once and a garbage value is an error everywhere instead of a silent zero platform fee on the two money paths. `PLATFORM_COUNTRY` has one default — the empty one its `ConfigVar` declares — instead of `""` in onboarding and `"US"` in checkout, payment links and the product wizard. And a boolean config flag has one truth table, so `ALLOW_SIGNUP=1` no longer opens the signup API while hiding the signup link.

**Architecture:** Four one-place functions, three of them new. `StripeClient` gains `request_json_optional` (the 404-means-absent half of `request_json`) and an internal `send`/`classify` split, and the five `stripe_client::send_raw` call sites in `stripe.rs` become `StripeClient` calls — deleting `stripe_catalog_headers`, `stripe_request_headers` and all five copies of the `https://api.stripe.com` literal. A new `blocks/products/config.rs` owns `seller_fee_bps(ctx) -> Result<u16>` and `platform_country(ctx) -> Result<Option<CountryCode>>`, the two config keys whose parsing was copied five and three times; `CountryCode` is a newtype so "validated two-letter code" is a fact of the type rather than a promise at each call site. `config_vars::is_truthy` is the one truth table (A: `1`, `true`, `yes`, `on`, trimmed, case-insensitive), `config_vars::get_bool` is its config-client reader and `config_vars::form_bool` its form-field reader, and every hand-rolled parser in `blocks/` calls one of the three.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e` (`wafer_core::clients::config`, `wafer_core::clients::network`, `wafer_run::{ErrorCode, WaferError}`), `maud`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-09-06-enums-and-error-discipline-design.md`, sections 1.6, 1.7, 2.4, 2.5, section 3.2's B21/B22/B23/`get_bool` bullets, and section 4's PR 6.

## Decisions taken while planning (recorded, not re-litigated)

1. **All five raw Stripe sites move, not just the catalog pair.** B21 names `stripe_catalog_post`/`stripe_catalog_get`, but the offer-checkout site (`stripe.rs:1208`), the Payment-Link create (`:2380`) and the Payment-Link deactivate (`:2498`) classify in the *opposite* direction — everything is `Internal`, so a deterministic 400 is retried forever — and they share the header building and the API-URL literal with the pair. Fixing half of a classification problem leaves two classifications. They all go.

2. **`request_json_optional`, not a status in `meta`.** The spec's smallest change was to carry `resp.status` in the `FailedPrecondition` error's meta so `stripe_catalog_get` could read 404 back out of it. That makes every caller re-derive a policy the client already knows. A second method whose contract is "404 means the object is absent" states it once, and no caller parses an error to find out what happened.

3. **`CountryCode` is a newtype, not a `String`.** One producer (`config::platform_country`), one validation, and `shipping_countries`/`push_shipping_address_collection` take `Option<&CountryCode>` — so the type says the value was checked. It is never serialized, so no published schema moves.

4. **A checkout that cannot determine its shipping countries is refused, not silently un-collected.** The spec says that with no `PLATFORM_COUNTRY` and no `allowed_shipping_countries` the form should emit no `shipping_address_collection[allowed_countries]` key, "which is Stripe's 'all countries' behaviour". That is wrong: `allowed_countries` is a *required* member of the `shipping_address_collection` hash, so omitting the keys omits the whole hash and Stripe collects **no** shipping address at all — for an offer whose `collect_shipping_address` is true. Trading a wrong country list for silently losing the buyer's address is the same class of failure B22 is about. Instead `build_offer_checkout_form` and `payment_link_form` return the error they already return for unbuildable forms, naming both fixes (set the platform country, or list the offer's allowed countries). This is the behaviour change `RELEASE.md` documents.

5. **Truth table A, and one predicate rather than one function.** Ruling 5 of the spec picks table A because it is the superset. But three of the sites read an HTML form field and one renders a checkbox from a stored string, and none of those has a `ctx` and a key to pass to `get_bool`. So the table itself is the shared thing: `is_truthy(&str)`, with `get_bool(ctx, key, default)` and `form_bool(form, key)` as the two readers defined on top of it. "Every hand-rolled parser goes through it" holds — through the predicate, which is where the truth table lives.

6. **The `auth_ui/pages/*` flags move from `ctx.config_get` to the config client.** That is the point of the fix: the page and the API must answer the same question the same way, and the API path reads the config client. It also means a value set in the admin settings UI (which lands in the database, not the context snapshot) now reaches the page — the same source the API already used.

7. **`llm/schema.rs::parse_enabled_field`, `auth/repo/mod.rs::map_bool` and `tickets/service.rs::bool_field` are not touched.** They decode *database* values, where `Bool` and `Number` are the real cases and one of them deliberately fails open. Spec 2.4.4 keeps them separate; this PR does not quietly change what "enabled" means for an LLM provider.

8. **The two `pages.rs` fee reads render the error.** They are display sites that swallowed a bad fee into `0`, which is a wrong number on the page an operator uses to check the fee. They propagate through the block's existing `err_internal`/`server_error_response` path rather than printing a false zero.

## Global Constraints

- All three snapshot gates byte-identical: `crates/impresspress-core/tests/snapshots/*.openapi.json`, `*.endpoints.json` and `dev.tools.json`. This PR declares no endpoint, publishes no new type and moves no contract. `UPDATE_OPENAPI_SNAPSHOTS=1` and `UPDATE_DEV_TOOLS_SNAPSHOT=1` are never run.
- The `tests/error_door.rs` allowlist is left exactly as it is, comment included. Two PRs have declined its eight `products/` entries; a third is scheduled to take them.
- No change to wafer-run (rev `7d47e5e`).
- TDD: write the test, run it, see it fail for the expected reason, then implement, then see it pass.
- Verification: `cargo +nightly fmt --all -- --check`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`; `cargo clippy -p impresspress-core --features block-dev,test-support --all-targets -- -D warnings`; `cargo test -p impresspress-core --no-fail-fast` (known unrelated failure `lockfile_loads_remote_block`); **`cargo test -p impresspress-core --features block-dev --no-fail-fast` in full** (per the 2026-09-07 verification correction).

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `src/config_vars.rs` | `is_truthy` (truth table A), `get_bool` (config client + default), `form_bool` (form field, absent is false). |
| `src/blocks/products/config.rs` | New. `CountryCode`, `platform_country(ctx) -> Result<Option<CountryCode>>`, `seller_fee_bps(ctx) -> Result<u16>`. The two keys' only readers. |
| `src/blocks/products/stripe_client.rs` | `send` + `classify` + `request_json` + `request_json_optional`. The only place a Stripe HTTP status becomes a `WaferError` code. |
| `src/blocks/products/stripe.rs` | No `send_raw`, no API-URL literal, no `stripe_request_headers`, no `stripe_catalog_headers`, no `configured_bool`, no `platform_country`, no inline fee parse. `shipping_countries`/`push_shipping_address_collection` take `Option<&CountryCode>` and refuse rather than defaulting. |
| `src/blocks/products/stripe_provider.rs` | `configured_fee` and the inline country parse are `config::seller_fee_bps` / `config::platform_country`. |
| `src/blocks/products/pages.rs` | The wizard's country prefill and the two fee reads come from `products::config`; a bad value renders an error, not a zero. |
| `src/blocks/{auth,auth_ui,tickets,vector,admin}/…`, `src/ui/settings_form.rs` | Every boolean flag read through `config_vars::{get_bool, form_bool, is_truthy}`. |
| `RELEASE.md` | The `PLATFORM_COUNTRY` behaviour change: what changes, who is affected, what to set. |

---

## Tasks

### Task 1 — one truth table

- [ ] Write `config_vars.rs`'s table test over `"1" "true" "TRUE" " on " "yes" "0" "false" "" "bogus"`, and an `auth_ui` page test that `WAFER_RUN_SHARED__ALLOW_SIGNUP=1` renders the signup link while the signup API accepts the request — the divergence that exists today.
- [ ] Run them: the table test does not compile, the page test fails (the link is hidden).
- [ ] Implement `is_truthy`, `get_bool`, `form_bool`.
- [ ] Convert every site in spec 1.6's tables A, B and C, plus `ui/settings_form.rs:117`. Run each block's tests after its conversion.

### Task 2 — one seller fee

- [ ] Write the tests: `seller_fee_bps` is `Err` on `"abc"`, `"20000"` and `""`, `Ok(250)` on `"250"`, `Ok(0)` on an unset key; and a checkout on a product owned by a seller, with a garbage fee configured, refuses instead of taking a zero fee.
- [ ] Run them: the unit test does not compile; the checkout test fails (checkout succeeds with `application_fee_amount` absent).
- [ ] Implement `config::seller_fee_bps`; route all five sites through it.

### Task 3 — one country default

- [ ] Write the tests: `platform_country` is `Ok(None)` on `""`, `Ok(Some(NZ))` on `"nz"`, `Err` on `"NZL"`; and a checkout with the key unset, `collect_shipping_address` on and no `allowed_shipping_countries` refuses rather than sending `allowed_countries[0]=US`.
- [ ] Run them: the unit test does not compile; the checkout test fails (the form carries `US`).
- [ ] Implement `CountryCode` and `config::platform_country`; delete `stripe.rs::platform_country` and the two hand-copied duplicates; thread `Option<&CountryCode>` through `shipping_countries` and `push_shipping_address_collection`.
- [ ] Write the `RELEASE.md` note.

### Task 4 — one Stripe classification

- [ ] Write the tests: a catalog sync whose first Stripe call answers 503 fails with `ErrorCode::Internal` and persists a sync error that does not claim Stripe rejected anything; a 400 stays `FailedPrecondition`; a 404 on the catalog GET is still "absent"; and the Payment-Link deactivate on a 400 is `FailedPrecondition`, not `Internal`.
- [ ] Run them: 503 is `FailedPrecondition` today, and the deactivate 400 is `Internal`.
- [ ] Implement `StripeClient::{send, classify, request_json_optional}`; route the five raw sites through the client; delete `stripe_catalog_headers`, `stripe_request_headers` and the API-URL literals.

### Task 5 — verification

- [ ] Full verification list, including the block-dev suite in full.
- [ ] `git status --short crates/impresspress-core/tests/snapshots` empty after both passes.
- [ ] Consumer search over `crates/`, `packages/impresspress-js/src`, `packages/`, `examples/` (including `examples/tests/*.spec.ts`), `crates/impresspress-web/tests`, `docs/` and the blocks' embedded `assets/*.js`.
