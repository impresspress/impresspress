# WebMCP production fixes — design

**Date:** 2026-10-08
**Status:** revised after review (2026-10-08)
**Origin:** a live test on 2026-10-08. An Opus subagent drove a real Google
Chrome 146 started with `--enable-features=WebMCPTesting`. It started at
`https://impresspress.org/build` and built a three-product shop with a
compiled backend block, listing and calling tools only through Chrome's own
registry (`navigator.modelContextTesting`).

## Problem

A browser that really has WebMCP gets **no tools at all** from any
Impresspress page. The build worked only because the test harness copied
`navigator.modelContext` onto `document.modelContext` before the page loaded.
With that bridge in place the agent finished the build, but it hit a set of
smaller defects along the way. Every one of them comes from a surface an agent
reads (a schema, a tool description, the manifest, a page's status text)
saying something that is not true.

## Findings and what "fixed" means

### F1 — Tools are registered on the wrong object (blocker)

Chrome implements WebMCP as `navigator.modelContext` (a `ModelContext` with
`registerTool`, `unregisterTool`, `provideContext` and `clearContext`).
`navigator.modelContextTesting` (`listTools`, `executeTool`,
`registerToolsChangedCallback`) exists only with the `WebMCPTesting` feature.
`document.modelContext` does not exist. Both `ui/assets/webmcp.js` and
`blocks/dev/assets/dev.js` test for `document.modelContext` and return early,
so they register nothing.

The e2e suite missed this because every spec installs a polyfill on
`document.modelContext` (`fixtures/model-context-polyfill.ts`): the tests
checked the code against our own fake of the API, not the real one. The Node
unit harnesses that drive `dev.js` and `webmcp.js` in CI
(`blocks/dev/assets/test/harness.mjs`, `ui/assets/test/harness.mjs`) stub the
same wrong object.

How Chrome behaves, observed on 2026-10-08 (Chrome 146.0.7680.71 and
Playwright's bundled Chromium 147.0.7727.15):

| Behaviour | Chrome | What we assumed |
|---|---|---|
| Where the API lives | `navigator.modelContext` | `document.modelContext` |
| When it exists | only with `--enable-features=WebMCPTesting` (which implies `WebMCP`). No flag: absent. `--disable-features=WebMCP`: absent. | always, in a "WebMCP browser" |
| Secure context | only in a secure context (https, `localhost`, `127.0.0.1`). On plain `http://` to any other host `navigator.modelContext` is absent while `navigator.modelContextTesting` is present. | not considered |
| `registerTool(opts, { signal })` | **ignores** `signal`; the tool survives `abort()` | honoured |
| Registering a name that is already registered | **throws** `InvalidStateError: Duplicate tool name` | replaces it |
| `inputSchema` given as a string | throws `TypeError` | (we always pass objects) |
| Arguments that don't match `inputSchema` | passed through unvalidated | — |
| What `listTools()` reports per tool | `name`, `description`, `inputSchema` (a JSON string). **No `outputSchema`.** | the polyfill returned the options object |
| What `executeTool` returns | the `execute` result, serialized to a JSON string | — |
| A same-origin sandboxed iframe (the `/b/dev` preview) | has its own registry; its tools do not collide with the top frame's | — |
| A schema with `$defs` and `"$ref": "#/$defs/Condition"` back-edges | accepted at `registerTool` | (untested) |

**Consequence of the secure-context row.** A page served over plain http on a
LAN address gets no `navigator.modelContext`, so `webmcp.js` correctly
registers nothing. For the service-worker build (the sandbox and every
export) it is worse: a service worker needs a secure context too, so the
runtime never boots, and the boot page (`impresspress-bundle/assets/loader.js.tmpl`)
says "Service Workers not supported in this browser." That is false — the
browser supports them, the page is not secure. It is the same class of defect
as the rest of this program (a surface saying something untrue), and the most
likely way a user meets the secure-context rule: serving an export on
`http://192.168.…`.

**Fixed means:**
- Every registrar uses `navigator.modelContext` and nothing else. Nothing
  reads or mentions `document.modelContext`: no fallback and no alias. The
  standard proposal (webmachinelearning/webmcp) and Chrome both put the API
  on `navigator`; a second spelling would be exactly the kind of compat shim
  `CLAUDE.md` forbids.
- No registrar ever tries to register a name that is already registered.
  Each registration is guarded, so one rejected tool doesn't lose the rest
  (`webmcp.js` already does this; `dev.js` guards inside `registerPageTool`,
  the one place both of its registrars go through).
- Unregistering by name is the path we rely on, since Chrome ignores the
  signal. `dev.js` still passes `{ signal }`, because the proposal defines it
  and a browser that honours it is also correct.
- **The polyfill is deleted.** Every WebMCP e2e spec runs Chromium with
  `--enable-features=WebMCPTesting` and lists and executes tools through
  `navigator.modelContextTesting`. The test helpers (`registeredTools`,
  `waitForTool`, `execute`, `structured`) keep their names and signatures and
  are reimplemented on top of the real registry. `ToolRecord` loses
  `outputSchema`, because the registry does not report it; an assertion about
  output schemas reads the served manifest (`/b/webmcp/manifest.json`).
  Assertions that only made sense against the polyfill's internals
  (`__unregistered()`, signal bookkeeping) are rewritten as assertions about
  the registry Chrome reports (`listTools()` before and after).
- The "no WebMCP" console test pins its browser explicitly with
  `--disable-features=WebMCP`, so it keeps testing a browser without WebMCP
  once the flag is on suite-wide.
- The Node unit harnesses stub `navigator.modelContext`, and CI's
  `node --test` steps keep running them.
- The boot page says what is wrong on an insecure page ("open it over https
  or on localhost"), checked before the service-worker check, and the export
  README's "Serve it" section says the same.
- The docs, README, rustdoc, code comments and the `examples/webmcp-demo`
  README all say `navigator.modelContext`; the demo README's "try it" line
  uses `navigator.modelContextTesting.listTools()` under the flag.

### F2 — Offer pricing has no schema (major)

`shop_create_offer`, `shop_update_offer` (and the admin `create_offer` /
`update_offer` endpoints behind them) declare `components`, `variables` and
`checkout` as `{"type":"object"}` items. Their real Rust types
(`OfferComponentDraft`, `AmountRule`, `VariableDefinition`, `CheckoutPolicy`,
`Condition`, …) all derive `JsonSchema`, but `products/routes.rs` keeps
`offer_definition_schema()`, `managed_offer_schema()`, `offer_list_schema()`
and `product_duplicate_schema()` hand-written. The comment above them gives
the reason: the recursive `Condition` type leaves a `$ref` that would dangle
inside the OpenAPI document "until `generate_openapi` hoists definitions into
`components/schemas`". wafer-run `5fbac83d` ("hoist $defs into
components/schemas in the OpenAPI document") removed that obstacle, and the
pinned rev `477f5231` includes it. The reason is gone and the comment is now
false. So are its echoes: `view_schema`'s doc in `routes.rs`, the comment on
`ProductDuplicateResponse` in `contracts.rs`, and the doc comment on
`shop_create_offer_merges_its_path_and_body_schemas` in
`tests/dev_tools_manifest.rs`, which explains why `$defs` is not asserted.

**Fixed means:**
- Every offer schema is derived: the endpoint inputs with
  `request_schema_of::<OfferDefinitionRequest>`, and `ManagedOffer`, the
  offer list (a new `OfferList` struct the list handler serializes) and the
  duplication envelope (`ProductDuplicateResponse`, which gains a
  `JsonSchema` derive) with `response_schema_of`. No hand-written offer schema
  remains, `view_schema` goes with its last user, and the stale comments go.
- The fields an agent fills in carry doc comments, which become schema
  `description`s: what `key`, `label` and `amount` mean; that amounts are
  integer minor units; what each `AmountRule` variant does.
- The WebMCP projection of `shop_create_offer` (the dev manifest snapshot
  `impresspress-core/tests/snapshots/dev.tools.json`) shows the component
  shape, including the `AmountRule` variants and the `type` tag, and carries
  `$defs.Condition`.
- A test builds a `components` argument the way the schema describes it and
  sends it through the real create-offer handler, which accepts it.
- `/openapi.json` stays valid: no dangling `$ref`, and `Condition` is in
  `components/schemas`.
- wafer-block's test comment that still calls the hoist future work
  (`types/endpoint.rs`, `recursive_types_never_reference_a_table_that_was_removed`)
  is corrected in a wafer-run PR.

### F3 — Browser-impossible endpoints are advertised (minor)

In the browser runtime (`__IMPRESSPRESS_RUNTIME_KIND__ = "browser"`),
`POST /b/products/checkout` always answers 403 ("Stripe secret-key checkout
is disabled in the browser runtime"). Yet the manifest still publishes
`start_checkout` with a description telling the agent to hand the customer
a URL, and the storefront widget still shows "Continue to secure checkout".

**Fixed means:**
- An endpoint can declare that it needs something only a server holds and so
  can never succeed in the browser runtime: `server_only` on wafer-block's
  `BlockEndpoint` (`#[serde(default, skip_serializing_if = …)]`, so older
  guests and `tests/wafer_guest_parity.rs` are unaffected and no
  `WAFER_GUEST_VERSION` bump is needed), set from `EndpointRoute::server_only()`
  in impresspress. The declaration sits on the endpoint, not in a list kept
  somewhere else.
- Exactly the endpoints whose handler refuses **unconditionally** in the
  browser runtime declare it: every success path passes
  `stripe_secret_operations_allowed`, directly or through
  `StripeClient::load`, and that check fails in the browser runtime. The rule decides, not a list; the endpoint-surface snapshot
  (`tests/snapshots/products.endpoints.json`, the ` server_only` suffix)
  records the current set for review. Offer archival and payment-link deactivation do **not**: in the
  browser they still succeed for an offer that was never synced and a link
  that was never sent to Stripe (`archive_offer_catalog`,
  `deactivate_payment_link` refuse only when Stripe holds something), so
  hiding them would hide a working endpoint.
- The discovery block set is computed in one place, `pipeline.rs`'s
  `discoverable_infos(ctx, block_infos, features)` (replacing
  `enabled_infos`), and both discovery branches use it: `/openapi.json` and
  the agent card, and `/b/webmcp/manifest.json`. In the browser runtime it
  drops every `server_only` endpoint, so the three documents agree. The
  filter belongs in the pipeline, not in a block's `info()`: the products
  block's `info()` has no context to read the runtime kind from.
- `/b/inspector/webmcp` (wafer-block-inspector) is **out of scope**. It is an
  admin debug view of what blocks *declare*: it reads
  `ctx.registered_blocks()` with each endpoint's declared `auth`, and already
  applies neither the feature toggle nor effective access. Listing a
  `server_only` endpoint there is consistent with that, and filtering it
  would put impresspress's runtime-kind concept into a generic wafer-run
  block.
- `StorefrontConfig` gains `checkout_available: bool`: true exactly when
  `POST /b/products/checkout` can get past its availability guards, i.e.
  `stripe_secret_operations_allowed(ctx)` and a non-empty
  `IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY` (the two checks at the top of
  `stripe::handle_checkout`). The API-version check after them is a
  misconfiguration answered with 500, not availability, and is left out.
  `embedded_checkout_available` (a valid publishable key whose mode matches
  the secret key's, in a runtime that allows secret operations) implies
  `checkout_available`; the converse does not hold. The browser runtime
  reports `checkout_available: false`, as it already reports
  `embedded_checkout_available: false`.
- The widget fetches `/b/products/storefront/config` once on load for the
  `hosted` and `embedded` presentations (today only `embedded` fetches it,
  and only on click). When `checkout_available` is false it renders no
  checkout button and shows one line: "Checkout isn't available on this
  site." (It is also what a server with no Stripe secret key shows, so it
  does not say "preview".) If the config request itself fails, the product
  and its price still render, no checkout button is shown, and the status
  line reports the failure instead of claiming checkout is unavailable. The `payment_link` presentation is untouched: it navigates to a
  pre-created Payment Link URL, never calls `/b/products/checkout`, works in
  the browser runtime, and still makes no config request.

### F4 — Agent-facing docs: wrong audience, or missing (minor/docs)

- `FileWriteRequest.expected_sha256`'s doc comment, which ends up in the
  `dev_write_file` / `dev_write_files` schemas, explains serde internals. It
  should say only what a caller needs: send the hash you last read, `null`
  for a new file; leaving the field out means `null`; a mismatch returns
  409 with the current hash.
- The seed's suggested prompt is visible only inside a collapsed
  `<details>`, so an agent reading the page text sees just the heading.
  `dev_read_reference` (`scaffold.rs`'s `handle_reference`) returns it as
  `suggested_prompt` (empty when the seed has none). The sandbox's `llms.txt`
  says so: it is generated by `examples/dev-sandbox/build.sh` (through
  `seeds/seedlib.py`) from `seeds/llms-preamble.md` and the seed's guide, so
  the sentence goes in `llms-preamble.md` and both seeds' `manifest.json`
  are rewritten with `seeds/write-manifest.py`. The `<details>` stays as it
  is for human visitors.
- Both seed guides (`examples/dev-sandbox/seeds/*/guide.md`, served as
  `site_markdown`) document: a complete working `shop_create_offer`
  argument with a fixed-price component, and the five theming custom
  properties of the `<impresspress-product>` widget (`--ip-accent`,
  `--ip-bg`, `--ip-border`, `--ip-muted`, `--ip-text`, with their defaults
  from the widget's own `:host` rule).

### F6 — `published_at` stays null after an admin activation (minor)

`shop_update_product` is the admin PATCH (`handle_update_product`). It turns
the request into columns and writes them with `repo::products::update_live`;
nothing sets `published_at`, so a product an admin (or the sandbox agent)
makes active keeps `published_at: null`. The two other writers that make a
product active already set it, in the handler's data map, to now:
the seller PATCH (`handle_user_update_product`) when it publishes without
moderation, and moderation approval (`handlers/sellers.rs`). `ProductView`
documents the column as "RFC 3339 timestamp the product last became active".

**Fixed means:** `handle_update_product` does what the other two writers do:
when the request sets `status: "active"`, it puts
`published_at = now_rfc3339()` into the same data map, so it lands in the
same `update_live` write. No row is read first. That matches both existing
writers, which also stamp on every publishing write rather than only on a
draft-to-active transition, and keeps the admin PATCH's one-write liveness
guarantee (its comment explains why it does not read first). Keeping the
*first* publish time instead would need either a read of the current row or
a conditional builder, and would make the admin path disagree with the other
two; neither is done. Any other status leaves `published_at` as it is, as
the seller PATCH does; only moderation rejection writes `""` (documented
above `timestamp_field` in `contracts.rs`), and that path is unchanged.

### Not in scope

- **Export credentials (formerly F5) — checked, no change.** The original
  finding said an export carries sessions, tokens and the
  `admin__variables` rows. That is false. `blocks/dev/data_snapshot.rs`
  exports a closed allowlist (`TABLE_ALLOWLIST`); every other table the
  products, admin and auth blocks declare is named in `TABLE_EXCLUDED`, which
  lists `sessions`, `tokens`, `api_keys`, `bootstrap_tokens`,
  `jwt_blocklist`, `oauth_pkce`, `pats`, `provider_links`, `rate_limits`,
  `orgs` and `maintenance`, and a test fails the build on a table in
  neither list. `impresspress__admin__variables` rows are filtered one by
  one through `variable_is_exportable`, which refuses any row
  `crate::util::is_sensitive_key` flags (the `sensitive` flag, the
  `_SECRET`/`_KEY` suffix, a declared secret `ConfigVar`) and any
  `IMPRESSPRESS_`-prefixed key. The identity set (`users`,
  `local_credentials`, `user_roles`) is exported deliberately, `Replace`d as
  a set, so the owner can sign in to the re-hosted copy with their own
  (hashed) password; the export README discloses this. Decided 2026-10-08
  (user): keep the current design.
- `DEBUG: main started` in the compile output: it isn't in impresspress or
  wafer-run. It comes from the in-browser compiler toolchain, so it is out
  of scope here.
- Admin tools on the visitor page: they appeared because the test session
  was an admin. The anonymous manifest is pinned by
  `webmcp_manifest_for_anonymous_caller_contains_no_privileged_tools`. Task
  1's real-Chrome spec adds the live equivalent.
- Chrome rejected nothing in our schemas (`oneOf`, `const`,
  `["string","null"]`, `format: uint64`, `$defs` with `$ref` back-edges), so
  the schema format itself needs no change.

## Acceptance (whole program)

After merge and deploy (production deploy only with the user's explicit
yes), the subagent harness is run again against
`https://impresspress.org/build` **with the bridge turned off** (the
harness default; `BRIDGE=1` turns it on, only for re-testing a pre-fix build).
The agent builds a shop with priced offers, getting the offer shape right
first time from the schema. Then:
- the visitor page lists no `start_checkout`, and the widget shows the line
  "Checkout isn't available on this site." in place of the checkout button;
- `dev_read_reference` returns the suggested prompt;
- a product the agent activated has a non-null `published_at`.
