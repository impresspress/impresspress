# WebMCP production fixes — design

**Date:** 2026-10-08
**Status:** draft, pending review
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
reads (a schema, a tool description, the manifest, an export) saying
something that is not true.

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
checked the code against our own fake of the API, not the real one.

How Chrome behaves, observed on 2026-10-08 (Chrome 146.0.7680.71 and
Playwright's bundled Chromium 147.0.7727.15):

| Behaviour | Chrome | What we assumed |
|---|---|---|
| Where the API lives | `navigator.modelContext` | `document.modelContext` |
| `registerTool(opts, { signal })` | **ignores** `signal`; the tool survives `abort()` | honoured |
| Registering a name that is already registered | **throws** `InvalidStateError: Duplicate tool name` | replaces it |
| `inputSchema` given as a string | throws `TypeError` | (we always pass objects) |
| Arguments that don't match `inputSchema` | passed through unvalidated | — |
| What `executeTool` returns | the `execute` result, serialized to a JSON string | — |

**Fixed means:**
- Every registrar uses `navigator.modelContext` and nothing else. Nothing
  reads or mentions `document.modelContext`: no fallback and no alias. The
  standard proposal (webmachinelearning/webmcp) and Chrome both put the API
  on `navigator`; a second spelling would be exactly the kind of compat shim
  `CLAUDE.md` forbids.
- No registrar ever tries to register a name that is already registered.
  Each registration is guarded, so one rejected tool doesn't lose the rest
  (`webmcp.js` already does this; `dev.js`'s direct registrations must too).
- Unregistering by name is the path we rely on, since Chrome ignores the
  signal. `dev.js` still passes `{ signal }`, because the proposal defines it
  and a browser that honours it is also correct.
- **The polyfill is deleted.** Every WebMCP e2e spec launches Chromium with
  `--enable-features=WebMCPTesting` and lists and executes tools through
  `navigator.modelContextTesting`. The test helpers (`registeredTools`,
  `waitForTool`, `execute`, `structured`) keep their names and signatures and
  are reimplemented on top of the real registry. Assertions that only made
  sense against the polyfill's internals (`__unregistered()`, signal
  bookkeeping) are rewritten as assertions about the registry Chrome reports
  (`listTools()` before and after).
- The docs, README, rustdoc, code comments, the `examples/webmcp-demo` README
  and the sandbox guides all say `navigator.modelContext`.

### F2 — Offer pricing has no schema (major)

`shop_create_offer`, `shop_update_offer` (and the admin `create_offer` /
`update_offer` endpoints behind them) declare `components`, `variables` and
`checkout` as `{"type":"object"}` items. Their real Rust types
(`OfferComponentDraft`, `AmountRule`, `VariableDefinition`, `CheckoutPolicy`,
`Condition`, …) all derive `JsonSchema`, but `products/routes.rs` keeps
`offer_definition_schema()` and `managed_offer_schema()` hand-written. The
comment above them gives the reason: the recursive `Condition` type leaves a
`$ref` that would dangle inside the OpenAPI document "until
`generate_openapi` hoists definitions into `components/schemas`". wafer-run
`5fbac83d` ("hoist $defs into components/schemas in the OpenAPI document")
removed that obstacle, and the pinned rev `477f5231` includes it. The reason
is gone and the comment is now false.

**Fixed means:**
- Every offer schema is derived: the endpoint inputs with
  `request_schema_of::<OfferDefinitionRequest>`, and `ManagedOffer`, the
  offer list and the duplication envelope with `response_schema_of` /
  `view_schema`. No hand-written offer schema remains, and the stale "NOT
  derivable" comments go.
- The fields an agent fills in carry doc comments, which become schema
  `description`s: what `key`, `label` and `amount` mean; that amounts are
  integer minor units; what each `AmountRule` variant does.
- The WebMCP projection of `shop_create_offer` (the dev manifest snapshot
  `impresspress-core/tests/snapshots/dev.tools.json`) shows the component
  shape, including the `AmountRule` variants and the `type` tag.
- A test builds a `components` argument by reading the projected schema and
  sends it through the real create-offer handler, which accepts it.
- `/openapi.json` stays valid: no dangling `$ref`, and `Condition` is in
  `components/schemas`.
- wafer-block's test comment that still calls the hoist future work
  (`types/endpoint.rs`, `recursive_types_never_reference_a_table_that_was_removed`)
  is corrected in a wafer-run PR. That PR changes only a comment, so
  impresspress needs no new pin.

### F3 — `start_checkout` is advertised where it can never work (minor)

In the browser runtime (`__IMPRESSPRESS_RUNTIME_KIND__ = "browser"`),
`POST /b/products/checkout` always answers 403 ("Stripe secret-key checkout
is disabled in the browser runtime"). Yet the manifest still publishes
`start_checkout` with a description telling the agent to hand the customer
a URL, and the storefront widget still shows "Continue to secure checkout".

**Fixed means:**
- An endpoint can declare that it is unavailable in the browser runtime.
  Every discovery surface — the WebMCP manifest, `/openapi.json` and the
  agent card — leaves such an endpoint out when the runtime is the browser.
  The declaration sits on the endpoint (`EndpointRoute`); it is not a list
  kept somewhere else. `start_checkout` is the first endpoint to use it. The
  other browser-disabled Stripe endpoints (sync, archival, payment link
  create and deactivate, webhooks) declare it too, so that discovery and
  behaviour agree for all of them.
- `StorefrontConfig` gains `checkout_available: bool`: true exactly when
  `POST /b/products/checkout` can succeed. The widget hides the checkout
  button when it is false and shows a single line in its place: "Checkout
  isn't available in this preview."
- The browser runtime's `get_storefront_config` reports
  `checkout_available: false`.

### F4 — Agent-facing docs: wrong audience, or missing (minor/docs)

- `FileWriteRequest.expected_sha256`'s doc comment, which ends up in the
  `dev_write_file` / `dev_write_files` schemas, explains serde internals. It
  should say only what a caller needs: send the hash you last read, `null`
  for a new file; leaving the field out means `null`; a mismatch returns
  409 with the current hash.
- The seed's suggested prompt is visible only inside a collapsed
  `<details>`, so an agent reading the page text sees just the heading.
  `dev_read_reference` returns it as `suggested_prompt` (empty when the seed
  has none), and both seeds' `llms.txt` say so. The `<details>` stays as it
  is for human visitors.
- Both seed guides (`examples/dev-sandbox/seeds/*/guide.md`, served as
  `site_markdown`) document: a complete working `shop_create_offer`
  argument with a fixed-price component, and the
  `<impresspress-product>` widget's theming custom properties (`--ip-*`),
  listed from the widget's own stylesheet.

### F5 — An export carries the sandbox's credentials (decision: drop)

`dev_export` writes the whole database snapshot into `seed/data.json`. That
includes `wafer_run__auth__local_credentials` (password hashes), sessions
and tokens, and the `admin__variables` rows.

**Decided 2026-10-08 (user):** the export leaves out credential material.

**Fixed means:**
- None of the auth credential, session or token tables are written:
  `local_credentials`, `sessions`, `tokens`, `jwt_blocklist`, `api_keys`,
  `personal_access_tokens`, `oauth_pkce_states`, `bootstrap_tokens` and
  `rate_limits` under `wafer_run__auth__*`. Neither is any variable whose
  key the variables layer classes as sensitive (the `_SECRET` / `_KEY`
  suffix rule). This is decided in one named place in `export.rs`: an
  explicit table allowlist or denylist, documented there. The block-owned
  `TABLE` constants are used, not literal strings.
- An exported site is still administrable. On first boot it creates its own
  admin through the runtime's normal bootstrap path, and rows that reference
  a user (product ownership and others) still resolve. If the users table
  has to be kept so ownership stays consistent, it is kept, and only the
  credential tables go. The implementation decides this from the code and
  writes the decision into `export.rs`'s module docs.
- `dev_export_manifest` reports the tables actually exported, and the
  export README says credentials are not carried.

### F6 — `published_at` stays null for an active product (minor)

When `shop_update_product` sets `status: "active"`, `published_at` stays
`null`; only the moderation publish flow sets it. **Fixed means:** every
path that makes a product active sets `published_at` if it is unset.
Deactivating leaves it as it is, unless an existing contract says to clear
it (`contracts.rs` documents the moderation path writing `""`; follow that).

### Not in scope

- `DEBUG: main started` in the compile output: it isn't in impresspress or
  wafer-run. It comes from the in-browser compiler toolchain, so it is out
  of scope here.
- Admin tools on the visitor page: they appeared because the test session
  was an admin. The anonymous manifest is pinned by
  `webmcp_manifest_for_anonymous_caller_contains_no_privileged_tools`. Task
  1's real-Chrome spec adds the live equivalent.
- Chrome rejected nothing in our schemas (`oneOf`, `const`,
  `["string","null"]`, `format: uint64`), so the schema format itself needs
  no change.

## Acceptance (whole program)

After merge and deploy, the subagent harness is run again against
`https://impresspress.org/build` **with the bridge turned off** (`BRIDGE=0`).
The agent builds a shop with priced offers, getting the offer shape right
first time from the schema. Then:
- the visitor page lists no `start_checkout`;
- the export's `seed/data.json` contains no credential table;
- `dev_read_reference` returns the suggested prompt.
