# WebMCP Production Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Impresspress's WebMCP tools work in a browser that really implements WebMCP, and make every agent-facing surface (schemas, descriptions, manifest, export) tell the truth.

**Architecture:** Seven PRs: one in wafer-run, six in impresspress, landed producer-first. Task 1 moves the registrars to `navigator.modelContext` and replaces the test polyfill with Chrome's own WebMCP registry, so the suite tests the real API. Tasks 3–7 each fix one agent-facing defect where it starts: offer schemas derived from types, `server_only` endpoints left out of discovery, docs written for agents, an export without credentials, and `published_at` set on activation.

**Tech Stack:** Rust (impresspress-core, wafer-block), schemars 1.x, vanilla JS assets composed by `impresspress-core/build.rs`, Playwright 1.59 with bundled Chromium 147 (`--enable-features=WebMCPTesting`).

**Spec:** `docs/superpowers/specs/2026-10-08-webmcp-production-fixes-design.md` (read it first; findings F1–F6 are referenced below).

## Global Constraints

- The API name is `navigator.modelContext`. The string `document.modelContext` must not appear anywhere in tracked code, tests or current docs. Exceptions: historical dated docs under `docs/` (`2026-08-*`, `2026-09-*` plans/specs/handoffs), which stay as written.
- No compat shims, aliases or fallbacks between the two spellings (workspace `CLAUDE.md`: "No code smells, no compat shims").
- No raw SQL in block code; use `wafer-sql-utils` builders (`CLAUDE.md`).
- Table names come from each repo module's `pub const TABLE` (`auth/repo/users.rs:17` pattern); never string literals.
- Config naming: `IMPRESSPRESS_*` = infrastructure; `__…__` keys are runtime-owned and never served from the variables table (see `blocks/config.rs`). The runtime kind key is `products::RUNTIME_KIND_CONFIG_KEY` = `"__IMPRESSPRESS_RUNTIME_KIND__"`, value `"browser"` in the service-worker runtime.
- Cross-repo order: the wafer-run PR (Task 2) merges before the impresspress PR that bumps the pin (Task 4). After a manifest change, re-resolve `Cargo.lock` and **commit it**.
- Before pushing Rust: `cargo +nightly fmt --all`, `cargo clippy --all-targets`, and the crate's tests. Before any long suite, check free disk (`df -h /`); it was at 95% on 2026-10-08.
- Branch + PR per task. Branch from `origin/main`, in its own worktree under `../impresspress-worktrees/<branch>`. Remove the worktree and its `target/` after merge.
- Commit trailer: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Two registrars on one page racing for a name.** On `/b/dev`, `dev.js` and `webmcp.js` both register into `navigator.modelContext`. Chrome *throws* on a duplicate name instead of replacing. A refresh (`__impresspressWebmcp.refresh()`), or a manifest tool also published by `dev.js`, must not leave the page with missing tools. Expected: every tool appears exactly once in `listTools()` after load and after a refresh. → Task 1, Step 6.
2. **Session expiry with an ignored signal.** Chrome ignores `{ signal }`, so after a 401 or a `pagehide` the page's tools are removed only by unregistering each by name. Expected: after the session expires, `listTools()` no longer lists the `dev_*` tools. → Task 1, Step 7.
3. **The Tool console browser.** Without WebMCP (no flag), the console path still works and the page says WebMCP is absent. Expected: unchanged behaviour of `dev-enter.spec.ts`'s console test, now detected as `!('modelContext' in navigator)`. → Task 1, Step 5.
4. **The recursive `Condition` schema in the WebMCP projection.** A derived offer schema contains a cycle; the projection keeps a `$defs` with back-edges. Expected: Chrome still accepts `shop_create_offer`'s schema at `registerTool`, and an agent building a nested `all`/`any` condition gets a schema it can follow. → Task 3, Step 6.
5. **An exported site after first boot.** With credentials stripped, the exported runtime must still produce a usable admin, and products the sandbox admin created must still show and be editable. Expected: boot the export, sign in as the bootstrap admin, see the products in the admin list. → Task 6, Step 5.

---

### Task 1: Register on `navigator.modelContext`; test against Chrome's real registry (F1)

**Repo/branch:** impresspress, `fix/webmcp-navigator-model-context`

**Files:**
- Modify: `crates/impresspress-core/src/ui/assets/webmcp.js`: guard plus every `document.modelContext` use.
- Modify: `crates/impresspress-core/src/blocks/dev/assets/dev.js`: `hasWebmcp` (~line 580), `registerPageTool` (~610), `unregisterPageTools` (~625), comments (~579, 587, 972); wrap the direct registrations (`dev_compile_block`, `dev_export`, ~line 700+) in the same per-tool guard as `registerFromManifest`.
- Modify: `crates/impresspress-core/src/blocks/dev/page.rs`: the comment at ~230 and the test assertion at ~590 (`"document.modelContext.registerTool("` → `"navigator.modelContext.registerTool("`).
- Modify: `crates/impresspress-core/src/ui/assets.rs:252` (rustdoc), `README.md:70`, `examples/webmcp-demo/README.md:19,161`, `examples/webmcp-demo/src/lib.rs:9`, `docs/dev-sandbox.md` (any mention), both seed guides and llms texts under `examples/dev-sandbox/seeds/*/` (grep).
- Delete: `crates/impresspress-web/tests/e2e/fixtures/model-context-polyfill.ts`
- Modify: `crates/impresspress-web/tests/e2e/fixtures/webmcp-helpers.ts`, `crates/impresspress-web/tests/playwright.config.ts`, and every spec that imports the polyfill: `webmcp.spec.ts`, `smoke.spec.ts`, `dev-workspace.spec.ts`, `dev-scenario.spec.ts`, `dev-compile.spec.ts`, `dev-compile-tool.spec.ts`, `dev-enter.spec.ts`.
- Check: `crates/impresspress-web/tests/playwright.visual-baseline.config.ts` and `playwright.live.config.ts`. Both spread `baseConfig.use`; the visual config overrides `launchOptions`, so it must keep the WebMCP flag (see Step 3).

**Interfaces:**
- Produces (test helpers, same names and signatures as today, so later tasks' specs use them unchanged):
  - `registeredTools(page: Page, atLeast: number): Promise<ToolRecord[]>`
  - `waitForTool(page: Page, name: string): Promise<void>`
  - `execute(page: Page, name: string, args: Record<string, unknown>): Promise<ToolResult>`
  - `structured<T>(result: ToolResult): T`
  - New: `toolNames(page: Page): Promise<string[]>` (sorted names from `listTools()`).

- [ ] **Step 1: Write the failing check.** Add to `webmcp.spec.ts` (the native-server spec) a test that loads `/` with **no init script** and asserts that the real registry lists the public tools:

```ts
test('a real WebMCP browser gets the public tools with no polyfill', async ({ page }) => {
  await page.goto('/');
  await waitForTool(page, 'list_products');
  const names = await toolNames(page);
  expect(names).toContain('get_storefront_config');
  // Anonymous: nothing above Public is published (live twin of
  // webmcp_manifest_for_anonymous_caller_contains_no_privileged_tools).
  for (const forbidden of ['list_users', 'list_audit_log', 'get_site_settings', 'list_roles']) {
    expect(names).not.toContain(forbidden);
  }
});
```

- [ ] **Step 2: Rewrite `webmcp-helpers.ts` on the real registry.** Replace the polyfill-hook bodies:

```ts
type Testing = {
  listTools(): Promise<Array<{ name: string; description: string; inputSchema: string; outputSchema?: string }>>;
  executeTool(name: string, args: string): Promise<string>;
};
const testing = () => (navigator as unknown as { modelContextTesting: Testing }).modelContextTesting;

export async function registeredTools(page: Page, atLeast: number): Promise<ToolRecord[]> {
  await page.waitForFunction(
    async (n) => (await (navigator as any).modelContextTesting.listTools()).length >= n,
    atLeast,
    { timeout: 15_000 },
  );
  return page.evaluate(async () =>
    (await (navigator as any).modelContextTesting.listTools()).map((t: any) => ({
      name: t.name,
      description: t.description,
      // Chrome reports schemas as JSON strings; the helpers' callers read objects.
      inputSchema: JSON.parse(t.inputSchema),
      outputSchema: t.outputSchema ? JSON.parse(t.outputSchema) : undefined,
    })),
  );
}

export async function waitForTool(page: Page, name: string): Promise<void> {
  await page.waitForFunction(
    async (n) => (await (navigator as any).modelContextTesting.listTools()).some((t: any) => t.name === n),
    name,
    { timeout: 15_000 },
  );
}

export async function toolNames(page: Page): Promise<string[]> {
  return page.evaluate(async () =>
    (await (navigator as any).modelContextTesting.listTools()).map((t: any) => t.name).sort(),
  );
}

export async function execute(page: Page, name: string, args: Record<string, unknown>): Promise<ToolResult> {
  const raw = await page.evaluate(
    ([n, a]) => (navigator as any).modelContextTesting.executeTool(n, JSON.stringify(a)),
    [name, args] as const,
  );
  return JSON.parse(raw) as ToolResult;
}
```

Check before relying on it: confirm that `outputSchema` appears in Chrome's `listTools()` entries. Log one entry the first time. If Chrome does not report `outputSchema`, remove that field from `ToolRecord`, and make every assertion that read `outputSchema` from the registry read the served manifest (`/b/webmcp/manifest.json`) instead. Leave `const testing` out if it is unused. Delete the module comment's polyfill paragraph and say instead that these helpers drive Chrome's `navigator.modelContextTesting`, which only exists under `--enable-features=WebMCPTesting`.

- [ ] **Step 3: Enable the flag suite-wide.** In `playwright.config.ts`, inside `use`:

```ts
    // Chromium's own WebMCP implementation (navigator.modelContext) plus the
    // testing surface (navigator.modelContextTesting) the helpers read. The
    // suite tests against the real registry; there is no polyfill.
    launchOptions: { args: ['--enable-features=WebMCPTesting'] },
```

In `playwright.visual-baseline.config.ts`, change `launchOptions: { args: ['--disable-partial-raster'] }` to `args: ['--disable-partial-raster', '--enable-features=WebMCPTesting']`. The `projects[0].use` spreads `devices['Desktop Chrome']`; confirm that it does not drop `launchOptions` by running Step 4 first.

- [ ] **Step 4: Run Step 1's test and confirm it fails for the right reason.**
Run (needs a native server; see the header of `webmcp.spec.ts` and `package.json` `e2e:writes` for how it is started in CI): `cd crates/impresspress-web && npx playwright test --config=tests/playwright.visual-baseline.config.ts tests/e2e/webmcp.spec.ts -g "no polyfill"`
Expected: FAIL. `waitForTool` times out because `webmcp.js` returns early (`document.modelContext` is absent).

- [ ] **Step 5: Fix the registrars.** In `webmcp.js`:

```js
// Browsers without WebMCP get nothing. This ships on every page, so it
// must never throw on an unsupported browser.
if (!('modelContext' in navigator) || typeof navigator.modelContext.registerTool !== 'function') {
  return;
}

function register(tool) {
  navigator.modelContext.registerTool(toolOptions(tool));
}
```

…and `unregisterAll` uses `navigator.modelContext.unregisterTool`. In `dev.js`, `hasWebmcp = 'modelContext' in navigator && typeof navigator.modelContext.registerTool === 'function'`, and `registerPageTool` / `unregisterPageTools` use `navigator.modelContext`. Rewrite the `registered` comment (~600). It must now say that Chrome (as of 146/147) ignores the `signal`, so unregistering by name is the path that actually runs, while the signal is still passed because the proposal defines it. Rewrite the `unregisterPageTools` catch comment the same way. Update `dev-enter.spec.ts:135` to `expect(await page.evaluate(() => 'modelContext' in navigator)).toBe(false)`, and put that console test in a `test.describe` with `test.use({ launchOptions: { args: [] } })` so it runs in a browser without WebMCP. Keep its expected status text unchanged.

- [ ] **Step 6: Duplicate names (Review Focus 1).** Chrome throws `InvalidStateError` on a duplicate name. In `dev.js`, give every direct `registerPageTool(...)` call outside `registerFromManifest` the same per-tool `try { … } catch (error) { logError(error); }`. Then add to `dev-workspace.spec.ts`, after its existing "both registrars finished" wait:

```ts
  const names = await toolNames(page);
  expect(new Set(names).size, `duplicate registrations: ${names}`).toBe(names.length);
  await page.evaluate(() => (window as any).__impresspressWebmcp.refresh());
  await page.waitForFunction(() => (window as any).__impresspressWebmcp.generation() >= 2);
  const again = await toolNames(page);
  expect(again).toEqual(names);
```

Expected: PASS once Step 5 lands. If `again` is missing names, a name is published by both registrars. Fix that at its source: drop it from whichever list should not carry it. The curated `/b/dev` list is `blocks/dev/tools.rs`.

- [ ] **Step 7: Session expiry (Review Focus 2).** Rewrite every polyfill-based assertion in `webmcp.spec.ts` (lines ~36–70 use `__tools()` / `__unregistered()`) and in `smoke.spec.ts` (~242–248). Each one now compares `toolNames(page)` before and after. The `__unregistered()` "exactly once" check becomes: after the abort or 401, `toolNames` contains none of the page's `dev_*` names. The signal-vs-by-name distinction cannot be observed in Chrome; it ignores the signal, so delete the assertions that tried to tell the two apart, and say why in the spec comment. In `dev-compile.spec.ts:479`, replace the inline `__tools()` read with `toolNames(page)`.

- [ ] **Step 8: Remove the polyfill and every install of it.** `git rm crates/impresspress-web/tests/e2e/fixtures/model-context-polyfill.ts`, delete each `addInitScript(MODEL_CONTEXT_POLYFILL)` line and its import, and rewrite the comments that explained the install timing (`dev-workspace.spec.ts:43,123,135`, `dev-scenario.spec.ts:240,268`, `dev-compile.spec.ts:500`, `webmcp.spec.ts:12-13,36-53`) to describe the real registry.

- [ ] **Step 9: Docs sweep.** `git grep -n "document.modelContext" -- ':!docs/2026-0[89]*' ':!docs/superpowers/plans/2026-0[89]*' ':!docs/superpowers/specs/2026-0[89]*'` must print nothing. Update the files in the Files list. Run `git grep -n "polyfill" crates/impresspress-web/tests` and fix every stale mention.

- [ ] **Step 10: Run the unit and e2e tests.**
`cargo test -p impresspress-core blocks::dev::page` (the `page.rs` assertion from the Files list).
`cd crates/impresspress-web && npx playwright test --config=tests/playwright.config.ts tests/e2e/smoke.spec.ts` (the service-worker build, as CI's `e2e-smoke` runs it), then the dev-sandbox specs the way the `e2e-dev-sandbox` CI job runs them (copy the command from `.github/workflows/ci-shared.yml`), then `webmcp.spec.ts` under the visual-baseline config.
Expected: all pass. Step 1's test passes with no polyfill installed anywhere.

- [ ] **Step 11: Commit and open the PR.** Message: `fix(webmcp): register on navigator.modelContext; test against Chrome's real registry`. The PR body explains that `document.modelContext` never existed in Chrome, that the polyfill hid this, and the Chrome behaviour table from spec F1.

---

### Task 2: wafer-run — `BlockEndpoint::server_only`, and correct the stale hoist comment (F3 producer, F2)

**Repo/branch:** wafer-run, `feat/endpoint-server-only`

**Files:**
- Modify: `crates/wafer-block/src/types/endpoint.rs`: the `BlockEndpoint` struct (~line 129), `Default`, `new`, and a builder method; the test comment at ~1061.
- Test: the same file's `#[cfg(test)]` module.

**Interfaces:**
- Produces: `pub server_only: bool` on `BlockEndpoint` (`#[serde(default, skip_serializing_if = "std::ops::Not::not")]`); `pub fn server_only(mut self) -> Self`. Semantics, written in the rustdoc: "The endpoint needs something only a server holds, such as a secret key, and is never callable when the runtime runs in a browser. Discovery for a browser runtime leaves it out. The handler is still the gate."

- [ ] **Step 1: Write the failing test.**

```rust
#[test]
fn server_only_round_trips_and_is_absent_when_false() {
    let plain = BlockEndpoint::post("/b/x/y");
    let json = serde_json::to_value(&plain).unwrap();
    assert!(json.get("server_only").is_none(), "false must not be serialized: {json}");
    // An older guest that never heard of the field still deserializes.
    let old: BlockEndpoint = serde_json::from_value(serde_json::json!({"method": "POST", "path": "/b/x/y"})).unwrap();
    assert!(!old.server_only);

    let marked = BlockEndpoint::post("/b/x/y").server_only();
    let back: BlockEndpoint = serde_json::from_value(serde_json::to_value(&marked).unwrap()).unwrap();
    assert!(back.server_only);
}
```

(Use whichever constructor the file actually exposes for POST; `BlockEndpoint::new` is private, so check for `post`/`get` helpers or the builder used by other tests in the module.)

- [ ] **Step 2: Run it.** `cargo test -p wafer-block --features json-schema server_only_round_trips`. Expected: FAIL (no field or method).

- [ ] **Step 3: Implement it.** Add the field after `agent_tool`, with the serde attributes above, set it to `false` in `Default` and `new`, and add the builder next to `deprecated()`:

```rust
    /// Mark this endpoint as needing something only a server holds (a
    /// secret key, say). A browser runtime cannot call it, so discovery for
    /// a browser runtime leaves it out of every document. The handler stays
    /// the gate; this only stops the endpoint being advertised.
    pub fn server_only(mut self) -> Self {
        self.server_only = true;
        self
    }
```

- [ ] **Step 4: Correct the stale comment.** In `recursive_types_never_reference_a_table_that_was_removed`'s doc, replace the paragraph "The remaining gap is a *consumer* problem … not something to smuggle in here." with: "Inside an OpenAPI document both forms would resolve against the OpenAPI root rather than the embedded schema; `wafer_core::discovery::generate_openapi` closes that by hoisting `$defs` into `components/schemas` and rewriting the pointers (`hoist_defs_into_components`)."

- [ ] **Step 5: Run the tests, fmt and clippy.** `cargo test -p wafer-block --features json-schema && cargo test -p wafer-core && cargo +nightly fmt --all && cargo clippy --all-targets`. Expected: PASS. Grep for exhaustive `BlockEndpoint { … }` struct literals in other crates (`git grep -n "BlockEndpoint {" crates`) and add the field wherever the compiler asks.

- [ ] **Step 6: Commit, PR, merge.** `feat(wafer-block): BlockEndpoint::server_only; drop stale hoist TODO`. Record the merge SHA; Task 4 pins to it.

---

### Task 3: Derive the offer schemas (F2)

**Repo/branch:** impresspress, `fix/derived-offer-schemas`

**Files:**
- Modify: `crates/impresspress-core/src/blocks/products/routes.rs`: delete `offer_definition_schema` (~354), `managed_offer_schema` and `offer_list_schema` (~374–417), and the "NOT derivable" comment (~345). Re-derive the duplication envelope (~420–430) and fix `view_schema`'s doc (~179–183), which also says "cannot be derived at all yet". Point every `.input(offer_definition_schema)` (~686, 716, 1100, 1130) and the `.output(...)` uses at derived schemas.
- Modify: `crates/impresspress-core/src/blocks/products/contracts.rs`: doc comments on `OfferComponentDraft` (~443), `OfferComponent` (~468), `AmountRule` (~371) and its variants, `VariableDefinition`, `CheckoutPolicy`, `Condition`, and `OfferDefinitionRequest` fields. Every field an agent fills gets one plain sentence; amounts say "integer minor units (cents)".
- Add, only if missing: an `OfferList` response struct in `contracts.rs` (`pub struct OfferList { pub offers: Vec<ManagedOffer> }`) when the list handler serializes an ad-hoc `json!` today. Make the handler serialize the struct, so the schema and the body are the same type.
- Regenerate: `crates/impresspress-core/tests/snapshots/*.openapi.json`, `*.endpoints.json`, `dev.tools.json` (`UPDATE_OPENAPI_SNAPSHOTS=1`).
- Test: `crates/impresspress-core/src/blocks/products/tests/` (new `offer_schema_tests.rs`, registered in the tests `mod.rs`).

**Interfaces:**
- Consumes: `endpoint_match::{request_schema_of, response_schema_of}` (already imported in `routes.rs:31`).
- Produces: nothing new for other tasks, except that Task 5's guide example must validate against this schema.

- [ ] **Step 1: Write the failing test.** In `offer_schema_tests.rs`:

```rust
//! The offer schemas an agent reads are derived from the types the handler
//! deserializes, so a value built by following the schema is one the
//! handler accepts.

use crate::blocks::products::routes;
use serde_json::Value;

fn create_offer_input() -> Value {
    let rows = routes::endpoint_routes(); // use whatever the table accessor is named
    let row = rows
        .iter()
        .find(|r| r.template == "/b/products/api/admin/products/{product_id}/offers"
            && r.method == wafer_run::types::HttpMethod::Post)
        .expect("create-offer route");
    (row.input.expect("create-offer declares an input"))()
}

#[test]
fn create_offer_schema_describes_components() {
    let schema = create_offer_input();
    let item = &schema["properties"]["components"]["items"];
    for field in ["key", "label", "amount"] {
        assert!(item["properties"][field].is_object(), "component.{field} missing: {item}");
    }
    let amount = serde_json::to_string(&item["properties"]["amount"]).unwrap();
    assert!(amount.contains("unit_amount_minor"), "AmountRule variants not described: {amount}");
    assert!(amount.contains("\"fixed\""), "AmountRule's `type` tag not described: {amount}");
}
```

Find the real name of the route-table accessor with `grep -n "pub.*fn .*EndpointRoute\|pub static\|pub const .*EndpointRoute" routes.rs`. If the table is private, put the test inside `routes.rs`'s own `#[cfg(test)]` module.

- [ ] **Step 2: Run it.** `cargo test -p impresspress-core create_offer_schema_describes_components`. Expected: FAIL (items are `{"type":"object"}`).

- [ ] **Step 3: Derive.** Replace each `.input(offer_definition_schema)` with `.input(request_schema_of::<contracts::OfferDefinitionRequest>)`. Replace `.output(managed_offer_schema)` with `.output(response_schema_of::<contracts::ManagedOffer>)`. Replace the list output with `response_schema_of::<contracts::OfferList>` (see Files). Build the duplication envelope from `view_schema::<contracts::ProductView>()` and `view_schema::<contracts::ManagedOffer>()`, or derive it whole if a struct exists for it. Delete the three hand-written functions and the stale comments. Add the doc comments listed under Files.

- [ ] **Step 4: Run it, then add the round-trip test.** Step 1's test should pass. Then add:

```rust
#[tokio::test]
async fn a_component_built_from_the_schema_is_accepted_by_create_offer() {
    // Build the argument the way an agent would: key/label strings, and
    // the `fixed` AmountRule variant read from the schema.
    let body = serde_json::json!({
        "name": "Bag", "mode": "payment", "currency": "EUR",
        "pricing_model": "fixed", "usage_type": "licensed",
        "billing_scheme": "per_unit", "tax_behavior": "unspecified",
        "components": [{"key": "bag", "label": "250 g bag",
                        "amount": {"type": "fixed", "unit_amount_minor": 1450}}]
    });
    let parsed: crate::blocks::products::contracts::OfferDefinitionRequest =
        serde_json::from_value(body).expect("the handler's own type accepts it");
    assert_eq!(parsed.components.len(), 1);
}
```

Run: `cargo test -p impresspress-core offer_schema`. Expected: PASS.

- [ ] **Step 5: Regenerate the snapshots and read the diff.** `UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test openapi_snapshot --test endpoint_surface`, plus the test that writes `dev.tools.json` (`git grep -n "dev.tools.json" crates/impresspress-core` to find it, and its update variable). Read the diff in `dev.tools.json`: `shop_create_offer.components.items` now has `key`, `label`, `amount` and a `oneOf`/tagged `AmountRule`. Read `products.openapi.json` too: `Condition` sits under `components/schemas`, and no `$ref` points at a `#/$defs/…` that isn't there. Commit the snapshots with the code.

- [ ] **Step 6: Chrome accepts the recursive schema (Review Focus 4).** In `dev-workspace.spec.ts` (it already loads `/b/dev` with tools; Task 1 must be merged first), add:

```ts
test('shop_create_offer registers with its derived, recursive schema', async ({ page }) => {
  // …same setup as the file's existing workspace tests…
  await waitForTool(page, 'shop_create_offer');
  const tool = (await registeredTools(page, 1)).find((t) => t.name === 'shop_create_offer')!;
  const items = (tool.inputSchema as any).properties.components.items;
  expect(Object.keys(items.properties)).toEqual(expect.arrayContaining(['key', 'label', 'amount']));
});
```

Then call `shop_create_offer` through `execute()` with the Step 4 body, and `structured()` must succeed. Expected: PASS.

- [ ] **Step 7: Run the full crate tests, fmt, clippy, commit, PR.** `fix(products): derive offer schemas now that OpenAPI hoists $defs`.

---

### Task 4: Leave `server_only` endpoints out of browser discovery; `checkout_available` (F3)

**Repo/branch:** impresspress, `fix/browser-discovery-server-only`. **Blocked by:** Task 2 merged.

**Files:**
- Modify: `Cargo.toml` (workspace): bump every `wafer-*` `rev` to Task 2's merge SHA; re-resolve and commit `Cargo.lock`.
- Modify: `crates/impresspress-core/src/endpoint_match.rs`: `EndpointRoute` gains `pub server_only: bool` (`false` in `with_auth`) and `pub const fn server_only(mut self) -> Self`. The conversion (~line 355–370) calls `ep = ep.server_only()` when it is set.
- Modify: `crates/impresspress-core/src/blocks/products/routes.rs`: add `.server_only()` to every route whose handler refuses in the browser runtime. Find them with `grep -n "disabled in the browser runtime" crates/impresspress-core/src/blocks/products/stripe.rs`, then trace each one to its `Route::` variant and table row. Today that is checkout, webhook replay, catalog sync, catalog archival, payment-link create, both payment-link deactivations and webhooks.
- Modify: `crates/impresspress-core/src/pipeline.rs` (~line 320, where `enabled_infos` is computed for discovery): build the discovery block set from `enabled_infos` with every `server_only` endpoint removed when `ctx.config_get(products::RUNTIME_KIND_CONFIG_KEY) == Some("browser")`. Feed that one filtered set to `generate_openapi`, `generate_agent_card` and `generate_webmcp_report`, so the three documents agree.
- Modify: `crates/impresspress-core/src/blocks/products/contracts.rs` (`StorefrontConfig` ~648): add `pub checkout_available: bool`. `blocks/products/handlers/commerce.rs` (~67): set it from `stripe_secret_operations_allowed(ctx)` together with a configured secret key, the same conditions `POST /b/products/checkout` needs to succeed.
- Modify: `crates/impresspress-core/src/blocks/products/assets/storefront.js` (~475, the checkout button): when `!storefrontConfig.checkout_available`, render no checkout button and instead a `<p class="ip-checkout-unavailable">Checkout isn't available in this preview.</p>`.
- Regenerate snapshots (`UPDATE_OPENAPI_SNAPSHOTS=1`). `StorefrontConfig`'s schema changes.
- Test: `pipeline.rs` test module (next to `anonymous_manifest_exposes_the_storefront_purchase_path`, ~2008), `blocks/products/tests/storefront_tests.rs`.

**Interfaces:**
- Consumes: `wafer_block::BlockEndpoint::server_only()` and the `server_only` field (Task 2).
- Produces: `EndpointRoute::server_only()`; `StorefrontConfig.checkout_available: bool`.

- [ ] **Step 1: Write the failing pipeline test.** Next to `anonymous_manifest_exposes_the_storefront_purchase_path`:

```rust
#[tokio::test]
async fn browser_runtime_discovery_omits_server_only_endpoints() {
    let ctx = TestContext::new()
        .await
        .running_as(crate::blocks::router::ROUTER_BLOCK_ID)
        .with_config(crate::blocks::products::RUNTIME_KIND_CONFIG_KEY, "browser");
    let body = webmcp_manifest(&ctx, None, &real_block_infos(), &AllEnabled).await;
    let names = tool_names(&body);
    assert!(!names.contains(&"start_checkout"), "browser manifest advertises checkout: {names:?}");
    assert!(names.contains(&"preview_price"), "non-server-only tools stay: {names:?}");

    let openapi = discovery_json_as(&ctx, "/openapi.json", "impresspress.example.com", None).await;
    assert!(openapi["paths"]["/b/products/checkout"].is_null(), "openapi still lists checkout");
}
```

Use the real `TestContext` method for setting config (`test_support.rs:1412` documents setting `__IMPRESSPRESS_RUNTIME_KIND__ = "browser"`; use that helper's actual name). The existing `anonymous_manifest_exposes_the_storefront_purchase_path` (server runtime) must keep passing unchanged, since it is the opposite case.

- [ ] **Step 2: Run it.** `cargo test -p impresspress-core browser_runtime_discovery_omits`. Expected: FAIL.

- [ ] **Step 3: Implement.** Make the pin bump, then the `EndpointRoute` field, conversion and builder, the `.server_only()` rows and the pipeline filter. Write the filter as one named function with a doc comment:

```rust
/// The block set discovery describes in this runtime: `enabled` minus every
/// endpoint declared `server_only` when the runtime is the browser, where
/// such an endpoint can never succeed. One set feeds the manifest, the
/// OpenAPI document and the agent card, so all three agree.
fn discoverable_infos(ctx: &dyn Context, enabled: Vec<BlockInfo>) -> Vec<BlockInfo> {
    if ctx.config_get(crate::blocks::products::RUNTIME_KIND_CONFIG_KEY) != Some("browser") {
        return enabled;
    }
    enabled
        .into_iter()
        .map(|mut info| {
            info.endpoints.retain(|ep| !ep.server_only);
            info
        })
        .collect()
}
```

(Match `enabled_infos`'s real return type and `BlockInfo`'s endpoint field name.) If `RUNTIME_KIND_CONFIG_KEY` reads awkwardly in the pipeline because it lives in `products`, move the constant to a runtime-level module (e.g. `crate::runtime_kind`) and update every user listed by `grep -rn RUNTIME_KIND_CONFIG_KEY crates`. Do not leave a re-export behind.

- [ ] **Step 4: Run it.** Expected: PASS, along with the existing discovery tests (`cargo test -p impresspress-core pipeline::`).

- [ ] **Step 5: `checkout_available` test, then implementation.** In `storefront_tests.rs`, assert that `GET /b/products/storefront/config` returns `checkout_available: false` under the browser runtime kind, and `true` on the server runtime with a test secret key configured (follow the file's existing config-setting pattern). Run it and watch it fail, implement the field and handler, then run it and watch it pass.

- [ ] **Step 6: Widget.** Update `storefront.js`. Add a check to the `smoke.spec.ts` service-worker build test (browser runtime) that opens a product page and asserts `getByText("Checkout isn't available in this preview.")` is visible and no `button` named like "checkout" is. Follow `dev-scenario.spec.ts`'s storefront setup if smoke has no product. Run the smoke suite and confirm it passes.

- [ ] **Step 7: Regenerate snapshots, read the diff, run fmt, clippy and the crate tests, commit, PR.** `fix(discovery): browser runtime omits server_only endpoints; storefront reports checkout_available`.

---

### Task 5: Agent-facing docs (F4)

**Repo/branch:** impresspress, `docs/agent-facing-dev-docs`. **Soft dependency:** Task 3, because the guide example must match the derived schema.

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/contracts.rs:318-329`: the `expected_sha256` doc comment.
- Modify: `crates/impresspress-core/src/blocks/dev/contracts.rs:559-575` (`ReferenceResponse`): add `pub suggested_prompt: String`. Modify the `dev_read_reference` handler to fill it from the seed info `page.rs` already reads (`seed.suggested_prompt`), or `""` when there is no seed.
- Modify: `examples/dev-sandbox/seeds/{bootstrap,blank}/guide.md`: add a "Pricing an offer" section and a "Theming the product widget" section.
- Modify: each seed's llms text (the `SeedFile` behind `seed.rs:155`; find the files with `ls examples/dev-sandbox/seeds/*/`): one sentence saying `dev_read_reference` returns the template's suggested prompt as `suggested_prompt`.
- Regenerate: `dev.tools.json` and `dev.openapi.json` snapshots.
- Test: the `dev` block's reference handler tests (`git grep -n "site_markdown" crates/impresspress-core/src/blocks/dev` to find them), and a guide-example test (Step 4).

- [ ] **Step 1: Write the failing test.** Next to the existing reference-handler test, assert that the response JSON has `suggested_prompt` equal to the seed's prompt. Use the fixture seed that `page.rs:433` uses (`"Build a shop."`). Run it and confirm it fails.

- [ ] **Step 2: Implement `suggested_prompt`.** Add the field, with the doc comment "The task this template was designed to walk an agent through, as the workspace page suggests it. Empty when the template suggests none.", fill it in, and run the test to see it pass.

- [ ] **Step 3: Rewrite `expected_sha256`'s doc comment.** Replace it with:

```rust
    /// The SHA-256 the file has now, as your last read returned it, or
    /// `null` if you expect the file not to exist yet. Leaving the field out
    /// is the same as `null`. If the file changed since you read it, the
    /// write is refused with `409` and the file's current hash: re-read it,
    /// then write again.
```

- [ ] **Step 4: Guide sections, pinned by a test.** In both `guide.md` files, add:

````markdown
## Pricing an offer

An offer's price is a list of `components`. A fixed-price product has one:

```json
{"name": "250 g bag", "mode": "payment", "currency": "EUR",
 "pricing_model": "fixed", "usage_type": "licensed",
 "billing_scheme": "per_unit", "tax_behavior": "unspecified",
 "components": [{"key": "bag", "label": "250 g bag",
                 "amount": {"type": "fixed", "unit_amount_minor": 1450}}]}
```

Amounts are integer minor units (1450 = €14.50). `shop_create_offer`'s schema lists the other `amount` types.
````

and a "Theming the product widget" section that lists every `--ip-*` custom property `storefront.js` (or its stylesheet) reads, each with its default value. Generate the list with `grep -o -- "--ip-[a-z-]*" crates/impresspress-core/src/blocks/products/assets/* | sort -u`. Add a Rust test that extracts the first ```json block under "## Pricing an offer" from each guide (both are `include_str!`-able from the test) and deserializes it into `OfferDefinitionRequest`. That way the guide example cannot drift from the type. Run it and confirm it passes.

- [ ] **Step 5: Regenerate snapshots, run fmt, clippy and the tests, commit, PR.** `docs(dev): agent-facing reference — suggested_prompt, offer pricing, widget theming, plain expected_sha256`.

---

### Task 6: Export leaves out credentials (F5)

**Repo/branch:** impresspress, `fix/export-without-credentials`

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/export.rs`: where `snapshot.tables` becomes `seed/data.json` (~368 and the code that serializes the rows), and the module docs.
- Read first (do not modify unless the investigation requires it): the exported runtime's first-boot admin bootstrap. Find it with `git grep -n "BOOTSTRAP_ADMIN" crates/impresspress-core/src crates/impresspress-web/src`. Also read how products reference their owner (`git grep -n "created_by\|owner_id\|seller_id" crates/impresspress-core/src/blocks/products/repo`).
- Test: `export.rs`'s test module (or `blocks/dev/tests/`), plus the existing export e2e (`git grep -ln "dev_export" crates/impresspress-web/tests/e2e`).

- [ ] **Step 1: Investigate and record the decision.** Answer from the code, and write the answers at the top of `export.rs`'s module docs under `# What an export does not carry`:
  (a) On first boot with a seed, does the exported runtime create an admin when no credential exists? With which email and password source?
  (b) Do products and offers reference a user id? If they do, keeping `wafer_run__auth__users` keeps ownership valid. Then the bootstrap admin must either be that same user (a credential is added to the existing row) or the bootstrap must pick a new admin, with the old owner left as a credential-less user. Pick the option that leaves products editable by the admin who signs in, and write down why.

  If (a) is "no", making the exported runtime bootstrap an admin is part of this task. Use the existing bootstrap path; do not add a second one.

- [ ] **Step 2: Write the failing test.** Build an export from a test sandbox that has an admin (the test setup the existing export tests use) and assert:

```rust
let data: serde_json::Value = serde_json::from_slice(&seed_data_json).unwrap();
let tables = data["tables"].as_object().expect("tables map"); // adjust to the real layout
for excluded in [
    auth::repo::local_credentials::TABLE,
    auth::repo::sessions::TABLE,
    auth::repo::tokens::TABLE,
    auth::repo::jwt_blocklist::TABLE,
    auth::repo::api_keys::TABLE,
    auth::repo::pats::TABLE,
    auth::repo::oauth_pkce::TABLE,
    auth::repo::bootstrap_tokens::TABLE,
    auth::repo::rate_limits::TABLE,
] {
    assert!(!tables.contains_key(excluded), "{excluded} was exported");
}
// No sensitive variable value leaves: every exported variables row has a key
// the variables layer does not class as sensitive.
```

Use the variables layer's own sensitivity predicate (`git grep -n "fn is_sensitive\|_SECRET\b" crates/impresspress-core/src/platform_state crates/impresspress-core/src/config_vars.rs`); do not re-implement the suffix rule. Run it and confirm it fails.

- [ ] **Step 3: Implement it.** Add one `const EXCLUDED_TABLES: &[&str] = &[ …TABLE constants… ];` with a doc comment saying why each group is out: credentials, sessions and tokens, and rate-limit state. Filter `snapshot.tables` through it before both the archive and the `dev_export_manifest` preview, so the preview reports what is actually exported. Filter the variables table's rows with the sensitivity predicate. Implement whatever Step 1 decided about bootstrapping. Update `render_readme` (~868) to add the line "Sign-in credentials are not exported: the exported site creates its own admin on first boot (…how…)."

- [ ] **Step 4: Run it.** Expected: PASS, along with the existing export tests.

- [ ] **Step 5: First boot of an export (Review Focus 5).** Extend the existing export e2e. Unzip the export, serve it the way the existing test does, sign in with the bootstrap admin from Step 1, open the admin product list, and assert that the product the sandbox created is listed and its edit page opens. Run it and confirm it passes.

- [ ] **Step 6: fmt, clippy, tests, commit, PR.** `fix(dev): export carries no credentials; exported site bootstraps its own admin`.

---

### Task 7: `published_at` on activation (F6)

**Repo/branch:** impresspress, `fix/published-at-on-activation`

**Files:**
- Modify: the product update path (`crates/impresspress-core/src/blocks/products/handlers/product.rs`, and its repo function in `repo/products.rs`). Find where `status` is written with `git grep -n "\"status\"" crates/impresspress-core/src/blocks/products/handlers/product.rs crates/impresspress-core/src/blocks/products/repo/products.rs`. Then read the moderation publish flow that does set `published_at` (`contracts.rs:1715` documents it writing `""` when a product goes back).
- Test: `crates/impresspress-core/src/blocks/products/tests/` (whichever file already covers admin product update).

- [ ] **Step 1: Write the failing test.** Create a draft product through the admin API helper the tests already use, update it to `status: "active"`, and read it back: `published_at` is `Some(non-empty)`. Update it again, still active, and `published_at` is unchanged, so the first-publish time is kept. Run it and confirm it fails.

- [ ] **Step 2: Implement it** in the repo function: when the new status is `active` and the stored `published_at` is null or `""`, set it to now, using the same timestamp helper the moderation path uses, in the same write (a `wafer-sql-utils` builder, no raw SQL). When a product leaves `active`, do whatever the moderation path does on that transition (write `""`); do not invent a third rule. Run the test and confirm it passes.

- [ ] **Step 3: fmt, clippy, tests, commit, PR.** `fix(products): activating a product sets published_at`.

---

### Task 8: Live acceptance (whole program)

After Tasks 1–7 are merged **and deployed**. Confirm the deploy with the user first: it is production. The paths are in memory `dev-sandbox-deploy` and `build-sandboxes-program`.

- [ ] **Step 1:** Re-run the subagent harness from the 2026-10-08 session with `BRIDGE=0` (`docs/superpowers/plans/2026-10-08-webmcp-harness/`: `harness.mjs` drives Chrome with `--enable-features=WebMCPTesting`, `ctl.sh` sends it commands; `BRIDGE=0` disables the `document`→`navigator` copy), against `https://impresspress.org/build`.
- [ ] **Step 2:** The agent's report must show:
  - tools registered with no bridge;
  - `shop_create_offer` succeeding on the first call;
  - no `start_checkout` on the visitor page, and the widget showing the preview line;
  - `dev_read_reference.suggested_prompt` non-empty;
  - no credential table in `dev_export_manifest`.
- [ ] **Step 3:** File anything new as follow-up issues; do not fold it into these PRs.
