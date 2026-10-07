# WebMCP Production Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Impresspress's WebMCP tools work in a browser that really implements WebMCP, and make every agent-facing surface (schemas, descriptions, manifest, page status text) tell the truth.

**Architecture:** Six PRs: one in wafer-run, five in impresspress, landed producer-first. Task 1 moves the registrars to `navigator.modelContext` and replaces the test polyfill (and the Node harness stubs) with Chrome's own WebMCP registry, so the suite tests the real API. Tasks 3–6 each fix one agent-facing defect where it starts: offer schemas derived from types, `server_only` endpoints left out of browser discovery, docs written for agents, and `published_at` set by the admin activation path. Task 7 is the live acceptance run.

**Tech Stack:** Rust (impresspress-core, wafer-block), schemars 1.x, vanilla JS assets composed by `impresspress-core/build.rs`, Node's `node --test` for the asset harnesses, Playwright 1.59 with bundled Chromium 147 (`--enable-features=WebMCPTesting`).

**Spec:** `docs/superpowers/specs/2026-10-08-webmcp-production-fixes-design.md` (read it first; findings F1–F4 and F6 are referenced below. F5 was dropped on 2026-10-08 and is recorded under the spec's "Not in scope").

**Merging and deploy (user decision, 2026-10-08):** each PR merges as soon as its Opus review and its CI both pass, the wafer-run PR included; no further per-PR approval is needed. The production deploy that Task 7 needs is different: it requires the user's explicit yes before it runs.

## Global Constraints

- The API name is `navigator.modelContext`. The old spelling on `document` must not appear anywhere in tracked code, tests or current docs. Exceptions: historical dated docs under `docs/` (`2026-08-*`, `2026-09-*` plans/specs/handoffs), this program's own spec and plan (which describe the defect), and the 2026-10-08 harness under `docs/superpowers/plans/2026-10-08-webmcp-harness/` (whose `BRIDGE` copy is what Task 7 turns off).
- No compat shims, aliases or fallbacks between the two spellings (workspace `CLAUDE.md`: "No code smells, no compat shims").
- No raw SQL in block code; use `wafer-sql-utils` builders (`CLAUDE.md`).
- Table names come from each repo module's `pub const TABLE` (`auth/repo/users.rs:17` pattern); never string literals.
- Config naming: `IMPRESSPRESS_*` = infrastructure; `__…__` keys are runtime-owned and never served from the variables table (see `blocks/config.rs`). The runtime kind key is `"__IMPRESSPRESS_RUNTIME_KIND__"`, value `"browser"` in the service-worker runtime. Today the constant is `products::RUNTIME_KIND_CONFIG_KEY`; Task 4 moves it to `crate::runtime_kind`.
- Cross-repo order: the wafer-run PR (Task 2) merges before the impresspress PR that bumps the pin (Task 4). After a manifest change, re-resolve `Cargo.lock` and **commit it**.
- Merge policy: a PR merges once its Opus review and CI pass (see the header). Production deploy needs the user's explicit yes.
- Snapshot files are shared: Tasks 3, 4 and 5 all regenerate files under `crates/impresspress-core/tests/snapshots/`. Whichever of them merges second rebases onto main and regenerates rather than hand-merging a snapshot conflict.
- Before pushing Rust: `cargo +nightly fmt --all`, `cargo clippy --all-targets`, and the crate's tests. Before any long suite, check free disk (`df -h /`); it was at 96% on 2026-10-08.
- Branch + PR per task. Branch from `origin/main`, in its own worktree under `../impresspress-worktrees/<branch>` (wafer-run: a worktree of `../wafer-run` branched from `origin/main`; the local `wafer-run` checkout is behind it). Remove the worktree and its `target/` after merge.
- Visual baselines: a bot push from `regen-visual-baselines.yml` cannot retrigger CI. After a regen, push again yourself so the PR gets a fresh verdict.
- Commit trailer: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Two registrars on one page.** On `/b/dev`, `dev.js` and `webmcp.js` both register into `navigator.modelContext`, and Chrome *throws* on a duplicate name instead of replacing. Today they publish disjoint names: the `shop_*` rows in `blocks/dev/tools.rs` carry no `agent_tool` (the file's module doc says so), and `no_dev_or_shop_tool_leaks_into_the_global_manifest` (`tests/dev_tools_manifest.rs`) pins that no `dev_*`/`shop_*` name reaches the global manifest. Expected: every tool appears exactly once in `listTools()` after load and after `__impresspressWebmcp.refresh()`. → Task 1, Step 8.
2. **Session expiry with an ignored signal.** Chrome ignores `{ signal }`, so after a 401 the page's tools are removed only by unregistering each by name. Expected: after the session expires, `listTools()` lists no `dev_*` or `shop_*` tool and the page says the session expired. → Task 1, Step 9.
3. **The Tool console browser.** Once the WebMCP flag is on suite-wide, the "no WebMCP" console test would silently run in a WebMCP browser. Expected: it pins its browser with `--disable-features=WebMCP`, and its behaviour is unchanged, now detected as `!('modelContext' in navigator)`. → Task 1, Step 7.
4. **The recursive `Condition` schema in the WebMCP projection.** Confirmed a non-issue by the 2026-10-08 probe: Chrome accepts a schema with `$defs` and `"$ref": "#/$defs/Condition"` back-edges, and the `/b/dev` preview iframe has its own registry. Expected: a light assertion only — `shop_create_offer` registers in the real registry with its derived component shape, and the projection carries `$defs.Condition`. → Task 3, Steps 5 and 7.
5. **An export (or the sandbox) opened over plain http on a LAN address.** A non-secure page gets neither a service worker nor `navigator.modelContext`, so the runtime never boots, and the boot page blames the browser ("Service Workers not supported in this browser."). This is the most likely way a user meets the secure-context rule. Expected: the boot page says to open the site over https or on localhost, the export README says the same, and a real-browser test pins both the message and the F1 table's secure-context row. → Task 1, Step 10.

---

### Task 1: Register on `navigator.modelContext`; test against Chrome's real registry (F1)

**Repo/branch:** impresspress, `fix/webmcp-navigator-model-context`

**Files:**
- Modify: `crates/impresspress-core/src/ui/assets/webmcp.js`: the guard (line 10), `register` (15), `unregisterAll` (26–29).
- Modify: `crates/impresspress-core/src/blocks/dev/assets/dev.js`: `hasWebmcp` and its comment (577–581), the `pageTools` comment (587), the `registered` comment (594–598), `registerPageTool` (612–623), `unregisterPageTools` (625–644), the `pagehide` comment (972).
- Modify: `crates/impresspress-core/src/blocks/dev/page.rs`: the comment at 230 (`parent.navigator.modelContext`) and the assertion at 590 (`"navigator.modelContext.registerTool("`).
- Modify (Node harnesses CI runs): `crates/impresspress-core/src/blocks/dev/assets/test/harness.mjs` (doc at 32–33, comment at 281, stub at 317–327, `navigator` at 337), `crates/impresspress-core/src/blocks/dev/assets/test/dev_refcount.test.mjs` (97–104), `crates/impresspress-core/src/ui/assets/test/harness.mjs` (73–85).
- Modify: `crates/impresspress-bundle/assets/loader.js.tmpl` (`boot()`, 1226–1230) and `crates/impresspress-core/src/blocks/dev/templates/export-readme.md` ("Serve it").
- Modify (docs): `crates/impresspress-core/src/ui/assets.rs:252` (rustdoc), `README.md:70`, `examples/webmcp-demo/README.md:19,161`, `examples/webmcp-demo/src/lib.rs:9`.
- Delete: `crates/impresspress-web/tests/e2e/fixtures/model-context-polyfill.ts`
- Modify: `crates/impresspress-web/tests/e2e/fixtures/webmcp-helpers.ts`, `crates/impresspress-web/tests/playwright.config.ts`, `crates/impresspress-web/tests/playwright.visual-baseline.config.ts`, and the specs `webmcp.spec.ts`, `smoke.spec.ts`, `dev-workspace.spec.ts`, `dev-scenario.spec.ts`, `dev-compile.spec.ts`, `dev-compile-tool.spec.ts`, `dev-enter.spec.ts`.
- Check: `crates/impresspress-web/tests/playwright.live.config.ts` spreads `baseConfig.use`, so it inherits the flag (it runs `webmcp.spec.ts` over https, a secure context).

**Interfaces:**
- Produces (test helpers; same names and signatures as today except that `ToolRecord` loses `outputSchema`, so later tasks' specs use them unchanged):
  - `registeredTools(page: Page, atLeast: number): Promise<ToolRecord[]>`
  - `waitForTool(page: Page, name: string): Promise<void>`
  - `execute(page: Page, name: string, args: Record<string, unknown>): Promise<ToolResult>`
  - `structured<T>(result: ToolResult): T`
  - New: `toolNames(page: Page): Promise<string[]>` (sorted names from `listTools()`).
  - `ToolRecord = { name: string; description: string; inputSchema: Record<string, unknown> }`.

- [ ] **Step 1: Write the failing check.** Add to `webmcp.spec.ts` (the native-server spec) a test that loads `/b/auth/login` with **no init script** and asserts that the real registry lists the public tools:

```ts
test('a real WebMCP browser gets the public tools with no polyfill', async ({ page }) => {
  await page.goto('/b/auth/login');
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

Update the import line to `import { execute, registeredTools, toolNames, waitForTool } from './fixtures/webmcp-helpers';`.

- [ ] **Step 2: Rewrite `webmcp-helpers.ts` on the real registry.** Chrome's `listTools()` entries carry `name`, `description` and `inputSchema` (a JSON string) and **no `outputSchema`** (probed 2026-10-08), and `executeTool` returns the `execute` result serialized to a JSON string. Replace the module comment, `ToolRecord`, `registeredTools`, `waitForTool` and `execute`, add `toolNames`, and keep `ToolResult` and `structured` as they are:

```ts
import { expect, type Page } from '@playwright/test';

/**
 * Reading and driving the browser's WebMCP registry from a test.
 *
 * These helpers drive Chromium's own `navigator.modelContextTesting`
 * (`listTools`, `executeTool`), which exists only when the browser runs with
 * `--enable-features=WebMCPTesting` — `playwright.config.ts` sets that for
 * the whole suite. What they see is exactly what the page handed to
 * `navigator.modelContext.registerTool`; nothing here fakes the API.
 *
 * They live here, not in one spec, because the native-server spec
 * (`webmcp.spec.ts`) and the sandbox specs (`dev-workspace.spec.ts`,
 * `dev-compile.spec.ts`, `dev-scenario.spec.ts`, `smoke.spec.ts`) need the
 * same operations against different servers.
 */

/** The testing surface, as Chromium 147 exposes it. Type-only. */
type ModelContextTesting = {
  listTools(): Promise<Array<{ name: string; description: string; inputSchema: string }>>;
  executeTool(name: string, args: string): Promise<string>;
};

/**
 * One registration, as `listTools()` reports it. The registry does not
 * report output schemas; a test that checks one reads the served manifest
 * (`/b/webmcp/manifest.json`).
 */
export type ToolRecord = {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
};

/**
 * Wait until at least `atLeast` tools are registered, then return all of
 * them.
 *
 * "At least", not "exactly": on `/b/dev` both `dev.js` (the page-scoped
 * `dev_*`/`shop_*` allowlist) and `webmcp.js` (the deployment-wide manifest)
 * register into the same `navigator.modelContext`, and they finish in
 * whichever order their two fetches complete. A caller that needs a specific
 * tool from the slower registrar waits for it by name with [`waitForTool`].
 */
export async function registeredTools(page: Page, atLeast: number): Promise<ToolRecord[]> {
  await page.waitForFunction(
    async (n) =>
      (
        await (navigator as unknown as { modelContextTesting: ModelContextTesting }).modelContextTesting.listTools()
      ).length >= n,
    atLeast,
    { timeout: 15_000 },
  );
  return page.evaluate(async () =>
    (
      await (navigator as unknown as { modelContextTesting: ModelContextTesting }).modelContextTesting.listTools()
    ).map((t) => ({
      name: t.name,
      description: t.description,
      // The registry reports the schema as a JSON string; callers read objects.
      inputSchema: JSON.parse(t.inputSchema) as Record<string, unknown>,
    })),
  );
}

/**
 * Wait until a tool with this exact name is registered.
 *
 * The counting wait above cannot express "the other registrar has finished
 * too" without pinning a total that belongs to a different file's contract.
 * Waiting for one name it publishes is the same fact without the coupling.
 */
export async function waitForTool(page: Page, name: string): Promise<void> {
  await page.waitForFunction(
    async (n) =>
      (
        await (navigator as unknown as { modelContextTesting: ModelContextTesting }).modelContextTesting.listTools()
      ).some((t) => t.name === n),
    name,
    { timeout: 15_000 },
  );
}

/** Every registered tool's name, sorted. */
export async function toolNames(page: Page): Promise<string[]> {
  return page.evaluate(async () =>
    (
      await (navigator as unknown as { modelContextTesting: ModelContextTesting }).modelContextTesting.listTools()
    )
      .map((t) => t.name)
      .sort(),
  );
}

/** Invoke a registered tool through the registry and return its result. */
export async function execute(
  page: Page,
  name: string,
  args: Record<string, unknown>,
): Promise<ToolResult> {
  const raw = await page.evaluate(
    ([toolName, toolArgs]) =>
      (navigator as unknown as { modelContextTesting: ModelContextTesting }).modelContextTesting.executeTool(
        toolName as string,
        JSON.stringify(toolArgs),
      ),
    [name, args] as const,
  );
  // `executeTool` hands back the `execute` result serialized to JSON.
  return JSON.parse(raw) as ToolResult;
}
```

Then fix the one reader of `ToolRecord.outputSchema`: `webmcp.spec.ts:134-144` becomes

```ts
  test('registers exactly the Public storefront tools; the manifest gives each both schemas', async ({ page }) => {
    await page.goto('/b/auth/login');
    const tools = await registeredTools(page, PUBLIC_TOOLS.length);

    expect(tools.map((t) => t.name).sort()).toEqual([...PUBLIC_TOOLS].sort());
    for (const tool of tools) {
      expect(tool.description.length, tool.name).toBeGreaterThan(20);
      expect(tool.inputSchema?.type, `${tool.name} inputSchema`).toBe('object');
    }
    // The registry does not report output schemas; the manifest the page
    // registered from does, and `webmcp-core.js` hands it to `registerTool`.
    const manifest = (await (await page.request.get('/b/webmcp/manifest.json')).json()) as {
      tools: Array<{ name: string; outputSchema?: { type?: string } }>;
    };
    expect(manifest.tools.map((t) => t.name).sort()).toEqual([...PUBLIC_TOOLS].sort());
    for (const tool of manifest.tools) {
      expect(tool.outputSchema?.type, `${tool.name} outputSchema`).toBe('object');
    }
  });
```

- [ ] **Step 3: Enable the flag suite-wide, and confirm no visual baseline depends on it.** In `playwright.config.ts`, inside `use`:

```ts
    // Chromium's own WebMCP implementation (navigator.modelContext) plus the
    // testing surface (navigator.modelContextTesting) the helpers read. The
    // suite tests against the real registry; there is no polyfill. Chromium
    // only exposes navigator.modelContext in a secure context, which every
    // baseURL here (127.0.0.1, https) is.
    launchOptions: { args: ['--enable-features=WebMCPTesting'] },
```

In `playwright.visual-baseline.config.ts`, change `launchOptions: { args: ['--disable-partial-raster'] }` to `launchOptions: { args: ['--disable-partial-raster', '--enable-features=WebMCPTesting'] }` (it replaces the base `launchOptions` rather than merging, so it must carry the flag itself). `projects[0].use` spreads `devices['Desktop Chrome']`, which sets no `launchOptions`, so the top-level value stands.

The flag reaches every job that uses these two configs, including `e2e-visual` (`npm run e2e:visual`) and `products-browser` (`products-*.spec.ts`). The only visible effect of WebMCP is `dev.js`'s `#dev-webmcp-status` line on `/b/dev`. Confirm no screenshot captures it:

```bash
git grep -n "dev-webmcp-status\|/b/dev" -- crates/impresspress-web/tests/e2e/visual-baseline.spec.ts crates/impresspress-web/tests/e2e/products-lifecycle.spec.ts examples/tests/products-examples.spec.ts
```

Expected: no output (checked 2026-10-08), so no baseline changes. If CI's screenshot comparison fails anyway, run `regen-visual-baselines.yml` on the branch, read every changed PNG, then push again yourself (Global Constraints).

- [ ] **Step 4: Run Step 1's test and confirm it fails for the right reason.**
Run (needs a native server; see the header of `webmcp.spec.ts` and `package.json` `e2e:writes` for how CI starts it): `cd crates/impresspress-web && npx playwright test --config=tests/playwright.visual-baseline.config.ts tests/e2e/webmcp.spec.ts -g "no polyfill"`
Expected: FAIL. `waitForTool` times out: `listTools()` answers `[]` because `webmcp.js` returns early on the old guard.

- [ ] **Step 5: Fix the registrars.** In `webmcp.js`:

```js
// Browsers without WebMCP get nothing. This ships on every page, so it
// must never throw on an unsupported browser — or on an insecure page,
// where Chrome leaves `navigator.modelContext` undefined.
if (!('modelContext' in navigator) || typeof navigator.modelContext.registerTool !== 'function') {
  return;
}

function register(tool) {
  navigator.modelContext.registerTool(toolOptions(tool));
}
```

and in `unregisterAll`, `navigator.modelContext.unregisterTool` in both places.

In `dev.js`:

```js
// Whether this browser has WebMCP. Asked once: it is a property of the
// browser (and of this page being a secure context), and everything below
// that differs by it — whether a tool is handed to `navigator.modelContext`,
// what the guide says — must agree.
var hasWebmcp =
  'modelContext' in navigator && typeof navigator.modelContext.registerTool === 'function';
```

Rewrite the `registered` comment:

```js
// Every name this page registered with WebMCP. `registerTool`'s options bag
// takes an `AbortSignal`, and the proposal says aborting it unregisters the
// tool — but Chrome (146/147) ignores the signal, so this list is the path
// that actually runs: on abort, unregister exactly these by name. The signal
// is still passed, because a browser that honours it is also correct.
var registered = [];
```

`registerPageTool` guards its own registration, so every tool either registrar publishes goes through one guard (Chrome throws `InvalidStateError` on a duplicate name and `TypeError` on a schema it rejects):

```js
function registerPageTool(options) {
  if (MUTATING.test(options.name)) {
    options.execute = withProgress(options.execute);
  }
  // Before the WebMCP call, not after: a tool the browser's registrar
  // rejects is still a tool this page can run from the console.
  pageTools.push(options);
  if (hasWebmcp) {
    try {
      navigator.modelContext.registerTool(options, { signal: abort.signal });
      registered.push(options.name);
    } catch (error) {
      // One tool the browser rejected (a duplicate name, a schema it will
      // not take) is not a reason to lose the ones after it — the same
      // per-tool guard `webmcp.js` applies.
      logError(error);
    }
  }
}

function unregisterPageTools() {
  // Runs on every `pagehide` and every 401/403, whether or not registration
  // ever happened. `registered` is only ever filled when `hasWebmcp`, so a
  // browser without WebMCP has nothing to remove.
  if (!hasWebmcp) {
    registered = [];
    return;
  }
  registered.forEach(function (name) {
    try {
      navigator.modelContext.unregisterTool(name);
    } catch (error) {
      // Already gone: a browser that honours the signal removed it on abort.
      // Chrome does not, so on Chrome this call is the one that removes it.
    }
  });
  registered = [];
}
```

The `try` inside `registerFromManifest` stays: it also covers `toolOptions` throwing on a malformed manifest entry. Rewrite the `pageTools` comment (587) and the `pagehide` comment (972) to say `navigator.modelContext`. In `page.rs`, the comment at 230 becomes `` `parent.navigator.modelContext` (including `registerTool`) `` and the assertion at 590 becomes `js.matches("navigator.modelContext.registerTool(").count()`.

- [ ] **Step 6: The Node harnesses stub the real object.** CI's `test` job runs both suites (`.github/workflows/ci-shared.yml:299-300` and `:311-312`); they stub the old object today and would fail once the guards read `navigator`.

In `crates/impresspress-core/src/blocks/dev/assets/test/harness.mjs`, delete the `...(hasModelContext ? { modelContext: … } : {})` spread from `sandbox.document` and make `sandbox.navigator`:

```js
    navigator: {
      serviceWorker,
      ...(hasModelContext
        ? {
            modelContext: {
              registerTool(options) {
                tools.set(options.name, options);
              },
              unregisterTool(name) {
                tools.delete(name);
              }
            }
          }
        : {})
    },
```

Update the `@param` doc (line 32: "give the stub navigator a WebMCP registrar, as a browser that supports it would") and the comment at 281 ("Everything the stub `navigator.modelContext` was handed, by name."). In `dev_refcount.test.mjs`, the test at 97 becomes:

```js
test('unregisterPageTools tolerates a browser with no navigator.modelContext at all', () => {
  // `hasModelContext: false` reproduces "this browser has no WebMCP
  // support" (`'modelContext' in navigator` is false). `pagehide` and every
  // 401/403 call `unregisterPageTools()` unconditionally regardless of
  // whether registration ever ran, so this must not throw.
```

In `crates/impresspress-core/src/ui/assets/test/harness.mjs`, `sandbox.document` becomes `{}` and

```js
    navigator: {
      serviceWorker,
      modelContext: {
        registerTool(options) {
          registerCalls.push(options.name);
        },
        unregisterTool(name) {
          unregisterCalls.push(name);
        }
      }
    },
```

Run both, exactly as CI does:

```bash
node --test crates/impresspress-core/src/blocks/dev/assets/test/*.test.mjs
node --test crates/impresspress-core/src/ui/assets/test/*.test.mjs
```

Expected: PASS. Then revert the `navigator` half of one stub locally and confirm the suite fails, so the stubs are proved load-bearing; restore it.

- [ ] **Step 7: Pin the Tool console test to a browser without WebMCP (Review Focus 3).** In `dev-enter.spec.ts`, wrap the test "without WebMCP, the Tool console lists the tools, reads the status and publishes a file" in:

```ts
test.describe('a browser without WebMCP', () => {
  // Pinned off, not left to the default: the suite runs with
  // `--enable-features=WebMCPTesting`, and this test is about the browser
  // that has no WebMCP at all. `test.use` replaces the config's
  // `launchOptions`, so the enable flag is not in this worker's args.
  test.use({ launchOptions: { args: ['--disable-features=WebMCP'] } });

  // …the existing test, unchanged except for the detection line below…
});
```

Its detection line becomes `expect(await page.evaluate(() => 'modelContext' in navigator)).toBe(false);`; the expected status text stays as it is. Rewrite the file comment at 13–19, which says nothing in this file installs the polyfill and that the API does not exist on these pages:

```ts
 * Both exist because of one visitor's agent — a cloud browser with no WebMCP —
 * that could not get past the login form and, had it done so, would have found
 * no tools. The suite runs Chromium with WebMCP on
 * (`--enable-features=WebMCPTesting`), so the entry tests here run in a WebMCP
 * browser, which changes nothing about them. The Tool console test is the one
 * that needs a browser WITHOUT WebMCP, and pins it: its `describe` launches
 * Chromium with `--disable-features=WebMCP`.
```

- [ ] **Step 8: Duplicate names (Review Focus 1).** Add to `dev-workspace.spec.ts`'s first test, right after its existing `await waitForTool(page, 'dev_export');` (the wait for both registrars):

```ts
  // Chrome throws on a duplicate name instead of replacing, so a name both
  // registrars published would lose one registration. They publish disjoint
  // names (`no_dev_or_shop_tool_leaks_into_the_global_manifest` pins the
  // server half); this pins what the browser ends up holding, before and
  // after webmcp.js swaps its own set out.
  const names = await toolNames(page);
  expect(new Set(names).size, `duplicate registrations: ${names}`).toBe(names.length);
  const generation = await page.evaluate(() =>
    (window as unknown as { __impresspressWebmcp: { generation(): number } }).__impresspressWebmcp.generation(),
  );
  await page.evaluate(() =>
    (window as unknown as { __impresspressWebmcp: { refresh(): Promise<void> } }).__impresspressWebmcp.refresh(),
  );
  await page.waitForFunction(
    (before) =>
      (window as unknown as { __impresspressWebmcp: { generation(): number } }).__impresspressWebmcp.generation() > before,
    generation,
  );
  expect(await toolNames(page)).toEqual(names);
```

Add `toolNames` to the file's helper import. Expected: PASS once Step 5 lands.

- [ ] **Step 9: Session expiry, and the remaining registry assertions (Review Focus 2).** Add to `dev-workspace.spec.ts`:

```ts
test('an expired session removes the workspace tools from the registry', async ({ page }) => {
  test.setTimeout(300_000);
  await bootServiceWorker(page);
  await openWorkspace(page);
  await waitForTool(page, 'dev_export');
  const pageScoped = (names: string[]) =>
    names.filter((n) => n.startsWith('dev_') || n.startsWith('shop_'));
  expect(pageScoped(await toolNames(page))).not.toEqual([]);

  // The session goes; the next tool call is the 401 that tells dev.js so.
  await page.context().clearCookies();
  const refused = await execute(page, 'dev_status', {});
  expect(refused.isError).toBe(true);

  // Chrome ignores the registration signal, so what this observes is
  // `unregisterPageTools` removing each name.
  await expect.poll(async () => pageScoped(await toolNames(page))).toEqual([]);
  await expect(page.locator('#dev-webmcp-status')).toHaveText(
    'The session expired and the tools were removed. Sign in again.',
  );
});
```

Rewrite the polyfill-hook assertions elsewhere:

- `webmcp.spec.ts:33-85`, the `refresh()` test. The old version read the polyfill's call log (`__unregistered()`) to prove the unregister half ran; Chrome offers no such log. Make the second manifest fetch drop one tool instead, so a refresh that unregistered nothing would leave it behind:

```ts
test('refresh() re-registers the manifest without disturbing a tool it does not own', async ({ page }) => {
  // `refresh()` drops exactly the names webmcp.js itself registered (see
  // `registered` in `webmcp.js`) and registers what the manifest says now.
  // A tool something else registered — `dev.js` on `/b/dev`; `stale_tool`
  // here — must survive it.
  //
  // The second manifest fetch answers the first one minus one tool, so the
  // unregister half is observable: a refresh that unregistered nothing would
  // leave that tool registered. (Chrome ignores `registerTool`'s signal, so
  // whether a signal or a by-name call removed it is not observable and is
  // not asserted.)
  let fetches = 0;
  let dropped = '';
  await page.route('**/b/webmcp/manifest.json', async (route) => {
    fetches += 1;
    const response = await route.fetch();
    const body = (await response.json()) as { tools: Array<{ name: string }> };
    if (fetches > 1) {
      dropped = body.tools[0].name;
      body.tools = body.tools.slice(1);
    }
    await route.fulfill({ response, json: body });
  });
  await page.goto('/b/auth/login');
  await registeredTools(page, PUBLIC_TOOLS.length);
  const before = await toolNames(page);
  const generationBefore = await page.evaluate(() => window.__impresspressWebmcp.generation());
  await page.evaluate(() =>
    (navigator as unknown as { modelContext: { registerTool(options: unknown): void } }).modelContext.registerTool({
      name: 'stale_tool',
      description: 'A tool webmcp.js did not register.',
      inputSchema: { type: 'object' },
      execute: async () => ({ content: [] }),
    }),
  );
  await page.evaluate(() => window.__impresspressWebmcp.refresh());

  expect(await page.evaluate(() => window.__impresspressWebmcp.generation())).toBeGreaterThan(generationBefore);
  expect(dropped).not.toBe('');
  const after = await toolNames(page);
  expect(after).not.toContain(dropped);
  expect(after).toEqual([...before.filter((n) => n !== dropped), 'stale_tool'].sort());
});
```

- `smoke.spec.ts:242` becomes `expect(await toolNames(page)).toEqual([]);`, and `smoke.spec.ts:246-251` becomes `await waitForTool(page, 'list_products');` (import both from `./fixtures/webmcp-helpers`).
- `dev-compile.spec.ts:476-482` becomes `const afterRollback = await toolNames(visitor);` (add `toolNames` to its helper import).

Run `webmcp.spec.ts` (visual-baseline config) and the sandbox specs touched here. Expected: PASS.

- [ ] **Step 10: An insecure page says so (Review Focus 5).** Write the failing test first, in `smoke.spec.ts` (the service-worker build on 127.0.0.1:8080):

```ts
test.describe('served on plain http at a LAN address', () => {
  // `lan.test` resolves to the same static server, but `http://lan.test` is
  // not a secure context (only https, localhost and 127.0.0.1 are), so the
  // browser offers neither a service worker nor `navigator.modelContext`.
  test.use({
    launchOptions: {
      args: ['--enable-features=WebMCPTesting', '--host-resolver-rules=MAP lan.test 127.0.0.1'],
    },
  });

  test('the boot page says to use https or localhost', async ({ page, baseURL }) => {
    const port = new URL(baseURL as string).port;
    await page.goto(`http://lan.test:${port}/`);
    expect(await page.evaluate(() => window.isSecureContext)).toBe(false);
    // The spec's F1 secure-context row, against the real browser: the flag
    // is on, the testing surface is there, the API is not.
    expect(await page.evaluate(() => 'modelContextTesting' in navigator)).toBe(true);
    expect(await page.evaluate(() => 'modelContext' in navigator)).toBe(false);
    await expect(page.locator('#status')).toHaveText(/over https or on localhost/);
  });
});
```

Run it: `cd crates/impresspress-web && npx playwright test --config=tests/playwright.config.ts tests/e2e/smoke.spec.ts -g "LAN address"`. Expected: FAIL, the status reads "Service Workers not supported in this browser."

Then in `loader.js.tmpl`'s `boot()`, before the service-worker check:

```js
    // A service worker (and WebMCP) needs a secure page: https, localhost or
    // 127.0.0.1. On plain http anywhere else the browser has no
    // `navigator.serviceWorker` at all, and "not supported in this browser"
    // would blame the wrong thing.
    if (!window.isSecureContext) {
        status.textContent =
            'This site only runs over https or on localhost: browsers start its ' +
            'service worker only on a secure page, and ' + location.origin + ' is not one.';
        return;
    }
```

In `export-readme.md`, after "Then open <http://localhost:8000/>. …", add:

```markdown
Open it on `localhost`, as above, or over `https`. Browsers run a service
worker only on a secure page, so the same folder served at
`http://192.168.1.20:8000/` on your network shows a page saying so and
nothing else; put it behind https to share it.
```

Run the test again. Expected: PASS.

- [ ] **Step 11: Remove the polyfill and every install of it.** `git rm crates/impresspress-web/tests/e2e/fixtures/model-context-polyfill.ts`. Delete each import and each `addInitScript(MODEL_CONTEXT_POLYFILL)`:
  - `webmcp.spec.ts:3`, `:53`; the two `beforeEach` blocks that only install it (`:130-132`, `:237-239`) go whole; `:180` (keep the rest of that `beforeEach`).
  - `smoke.spec.ts:5`, `:203`.
  - `dev-workspace.spec.ts:17`, `:125`, `:591`, `:633`, `:767`, `:897`.
  - `dev-scenario.spec.ts:17`, `:242`, `:431`, `:545`.
  - `dev-compile.spec.ts:4`, `:222`, `:506`.
  - `dev-compile-tool.spec.ts:17`, `:291`.

  Rewrite the comments that explained the install timing or the fake, so they describe the real registry: `webmcp.spec.ts:12-18` (the file header: the browser is Chromium with WebMCPTesting, and everything on both sides of the API is real), `dev-workspace.spec.ts:42-47` (Chromium's real WebMCP, no substitution), `:122-124`, `:135`, `dev-scenario.spec.ts:239-241`, `:268`, `dev-compile.spec.ts:499-500`, and in `webmcp-helpers.ts` the lines Step 2 did not already replace (`:4`, `:11-18`, `:21`, `:43`).

- [ ] **Step 12: Docs sweep.** Update `ui/assets.rs:252` ("registers each tool via `navigator.modelContext.registerTool` (no-ops on browsers without WebMCP, and on insecure pages)"), `README.md:70` (`navigator.modelContext.registerTool({`), `examples/webmcp-demo/README.md:19` and `examples/webmcp-demo/src/lib.rs:9` (`navigator.modelContext`), and `examples/webmcp-demo/README.md:160-161`:

```markdown
- Open any page in Chrome started with `--enable-features=WebMCPTesting`
  (over https or on localhost) and run
  `await navigator.modelContextTesting.listTools()` in the console.
```

Then both of these must print nothing:

```bash
git grep -n "document\.modelContext" -- ':!docs/2026-0[89]*' ':!docs/superpowers/plans/2026-0[89]*' ':!docs/superpowers/specs/2026-0[89]*' ':!docs/superpowers/plans/2026-10-08-webmcp-production-fixes.md' ':!docs/superpowers/specs/2026-10-08-webmcp-production-fixes-design.md'
git grep -n -i "polyfill" -- crates/impresspress-web/tests crates/impresspress-core/src/blocks/dev/assets/test crates/impresspress-core/src/ui/assets/test
```

- [ ] **Step 13: Run the unit and e2e tests.**
  - `cargo test -p impresspress-core --features block-dev,wasm blocks::dev::page` (the `page.rs` assertion).
  - `node --test crates/impresspress-core/src/blocks/dev/assets/test/*.test.mjs` and `node --test crates/impresspress-core/src/ui/assets/test/*.test.mjs`.
  - `cd crates/impresspress-web && npx playwright test --config=tests/playwright.config.ts tests/e2e/smoke.spec.ts tests/e2e/recovery-wipe.spec.ts` (as CI's `e2e-smoke` runs it).
  - The dev-sandbox specs the way `e2e-dev-sandbox`, `e2e-dev-compile` and the scenario step run them (copy the commands from `.github/workflows/ci-shared.yml:1126`, `:1358`, `:1393`).
  - `npm run e2e:writes` (includes `webmcp.spec.ts`) and `npm run e2e:visual` against a native server.

  Expected: all pass. Step 1's test passes with no polyfill anywhere.

- [ ] **Step 14: Commit, PR, merge.** Message: `fix(webmcp): register on navigator.modelContext; test against Chrome's real registry`. The PR body explains that Chrome never had the API on `document`, that the polyfill and the Node stubs hid this, and includes the Chrome behaviour table from spec F1. Merge once the Opus review and CI pass.

---

### Task 2: wafer-run — `BlockEndpoint::server_only`, and correct the stale hoist comment (F3 producer, F2)

**Repo/branch:** wafer-run, `feat/endpoint-server-only` (from `origin/main`, currently `477f5231`, the rev impresspress pins).

**Files:**
- Modify: `crates/wafer-block/src/types/endpoint.rs`: the `BlockEndpoint` struct (129–164), `Default` (166–183), `new` (186–201), a builder next to `deprecated()` (272), and the test doc at 1059–1063.
- Test: the same file's `#[cfg(test)]` module.

**Interfaces:**
- Produces: `pub server_only: bool` on `BlockEndpoint` (`#[serde(default, skip_serializing_if = "std::ops::Not::not")]`, the same attributes as `deprecated`); `pub fn server_only(mut self) -> Self`. A guest or an older host that never heard of the field deserializes it as `false`, and a `false` is never serialized, so the guest boundary (serde JSON, wafer-guest) is unchanged and no `WAFER_GUEST_VERSION` bump is needed.

- [ ] **Step 1: Write the failing test.**

```rust
#[test]
fn server_only_round_trips_and_is_absent_when_false() {
    let plain = BlockEndpoint::post("/b/x/y");
    let json = serde_json::to_value(&plain).unwrap();
    assert!(json.get("server_only").is_none(), "false must not be serialized: {json}");
    // An older guest that never heard of the field still deserializes.
    let old: BlockEndpoint =
        serde_json::from_value(serde_json::json!({"method": "POST", "path": "/b/x/y"})).unwrap();
    assert!(!old.server_only);

    let marked = BlockEndpoint::post("/b/x/y").server_only();
    let back: BlockEndpoint =
        serde_json::from_value(serde_json::to_value(&marked).unwrap()).unwrap();
    assert!(back.server_only);
}
```

- [ ] **Step 2: Run it.** `cargo test -p wafer-block --features json-schema server_only_round_trips`. Expected: FAIL to compile (no field, no method).

- [ ] **Step 3: Implement it.** Add the field after `agent_tool`:

```rust
    /// The endpoint needs something only a server holds (a secret key, say)
    /// and is never callable when the runtime runs in a browser. Discovery
    /// for a browser runtime leaves it out. The handler stays the gate.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub server_only: bool,
```

set `server_only: false` in `Default` and in `new`, and add the builder after `deprecated()`:

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

`git grep -n "BlockEndpoint {" -- .` shows no struct literal outside `endpoint.rs` (checked 2026-10-08 at `477f5231`), so nothing else needs the field.

- [ ] **Step 4: Correct the stale comment.** In `recursive_types_never_reference_a_table_that_was_removed`'s doc, replace the paragraph at 1059–1063 ("The remaining gap is a *consumer* problem … not something to smuggle in here.") with:

```rust
    /// Inside an OpenAPI document both forms would resolve against the
    /// OpenAPI root rather than the embedded schema;
    /// `wafer_core::discovery::generate_openapi` closes that by hoisting
    /// `$defs` into `components/schemas` and rewriting the pointers
    /// (`hoist_defs_into_components`).
```

- [ ] **Step 5: Run the tests, fmt and clippy.** `cargo test -p wafer-block --features json-schema && cargo test -p wafer-core && cargo +nightly fmt --all && cargo clippy --all-targets`. Expected: PASS.

- [ ] **Step 6: Commit, PR, merge.** `feat(wafer-block): BlockEndpoint::server_only; drop stale hoist TODO`. Merge once the Opus review and CI pass. Record the merge SHA; Task 4 pins to it.

---

### Task 3: Derive the offer schemas (F2)

**Repo/branch:** impresspress, `fix/derived-offer-schemas`. **Depends on:** Task 1 merged, for Step 7's e2e (it uses the registry helpers).

**Files:**
- Modify: `crates/impresspress-core/src/blocks/products/routes.rs`: delete `view_schema` and its doc (179–196), the "NOT derivable" comment (345–353), `offer_definition_schema` (354), `managed_offer_schema` (376), `offer_list_schema` (412) and `product_duplicate_schema` (420–431). Replace their uses: `.input(offer_definition_schema)` at 686, 716, 1100, 1130; `.output(managed_offer_schema)` at 687, 696, 717, 726, 735, 744, 753, 1101, 1110, 1131, 1140, 1149, 1158, 1167; `.output(offer_list_schema)` at 677 and 1091; `.output(product_duplicate_schema)` at 638 and 1082.
- Modify: `crates/impresspress-core/src/blocks/products/contracts.rs`: add `OfferList`; add `schemars::JsonSchema` to `ProductDuplicateResponse`'s derive and delete the comment above it (2298–2302) that calls it non-derivable; doc comments on `OfferDefinitionRequest` (572), `OfferComponentDraft` (470), `AmountRule` (373) and its variants, `VariableDefinition` (271), `CheckoutPolicy` (492) and `Condition` (299).
- Modify: `crates/impresspress-core/src/blocks/products/handlers/offers.rs:144`: serialize `OfferList` instead of the ad-hoc `json!`.
- Modify: `crates/impresspress-core/tests/dev_tools_manifest.rs`: rewrite the doc on `shop_create_offer_merges_its_path_and_body_schemas` (50–73) and assert `$defs.Condition`.
- Regenerate: `crates/impresspress-core/tests/snapshots/products.openapi.json`, `products.endpoints.json`, `dev.tools.json`.
- Test: new `crates/impresspress-core/src/blocks/products/tests/offer_schema_tests.rs`, registered in `products/tests/mod.rs`.

**Interfaces:**
- Consumes: `endpoint_match::{request_schema_of, response_schema_of}` (already imported at `routes.rs:31`); `routes::ROUTES` (`pub(super) const ROUTES: &[EndpointRoute<Route>]`, `routes.rs:441`, visible from `products::tests`); `wafer_run::HttpMethod`; `endpoint_match::SchemaFn` (`fn() -> serde_json::Value`).
- Produces: `contracts::OfferList { pub offers: Vec<ManagedOffer> }`. Task 5's guide example must deserialize into `OfferDefinitionRequest`.

- [ ] **Step 1: Write the failing test.** In `offer_schema_tests.rs`:

```rust
//! The offer schemas an agent reads are derived from the types the handler
//! deserializes, so a value built by following the schema is one the
//! handler accepts.

use serde_json::Value;
use wafer_run::HttpMethod;

use super::harness::{admin_create_msg, ctx, dispatch, output_to_json};
use crate::blocks::products::routes::ROUTES;

const CREATE_OFFER: &str = "/b/products/api/admin/products/{product_id}/offers";

/// The input schema the route table declares for `method template`.
fn route_input(method: HttpMethod, template: &str) -> Value {
    let row = ROUTES
        .iter()
        .find(|r| r.method == method && r.template == template)
        .unwrap_or_else(|| panic!("{method} {template} is not in ROUTES"));
    (row.input.expect("the route declares an input"))()
}

#[test]
fn create_offer_schema_describes_components() {
    let schema = route_input(HttpMethod::Post, CREATE_OFFER);
    let item = &schema["properties"]["components"]["items"];
    for field in ["key", "label", "amount"] {
        assert!(item["properties"][field].is_object(), "component.{field} missing: {item}");
    }
    let amount = serde_json::to_string(&item["properties"]["amount"]).unwrap();
    assert!(amount.contains("unit_amount_minor"), "AmountRule variants not described: {amount}");
    assert!(amount.contains("\"fixed\""), "AmountRule's `type` tag not described: {amount}");
}
```

- [ ] **Step 2: Run it.** `cargo test -p impresspress-core create_offer_schema_describes_components`. Expected: FAIL (`components.items` is `{"type":"object"}`).

- [ ] **Step 3: Derive.** In `contracts.rs`:

```rust
/// Response body of the offer list endpoints: every offer of one product,
/// with its publication and sync state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OfferList {
    pub offers: Vec<ManagedOffer>,
}
```

and `#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]` on `ProductDuplicateResponse`. In `offers.rs:144`: `Ok(offers) => ok_json(&OfferList { offers }),` (import `OfferList` with the other contracts).

In `routes.rs`: `.input(offer_definition_schema)` → `.input(request_schema_of::<contracts::OfferDefinitionRequest>)`; `.output(managed_offer_schema)` → `.output(response_schema_of::<contracts::ManagedOffer>)`; `.output(offer_list_schema)` → `.output(response_schema_of::<contracts::OfferList>)`; `.output(product_duplicate_schema)` → `.output(response_schema_of::<contracts::ProductDuplicateResponse>)`. Delete the four functions, `view_schema` (its only user was `product_duplicate_schema`), and the stale comments listed under Files.

Add the doc comments. Each field an agent fills gets one plain sentence; amounts say "integer minor units (cents)". For example:

```rust
pub struct OfferComponentDraft {
    /// Stable identifier for this line within the offer, e.g. `"bag"`.
    /// Lowercase letters, digits and underscores.
    pub key: String,
    /// What the buyer sees on the line, e.g. `"250 g bag"`.
    pub label: String,
    …
    /// How this line's price is computed. `{"type": "fixed",
    /// "unit_amount_minor": 1450}` is a fixed price of 14.50 in the offer's
    /// currency; the other `type`s price from a customer input.
    pub amount: AmountRule,
```

and on each `AmountRule` variant: `Fixed` "The same price every time, in integer minor units (cents)."; `PerUnit` "`unit_amount_minor` times the number in input `input`."; `FlatPlusPerUnit`, `Lookup`, `Graduated`, `Volume`, `Package` likewise, one sentence each, saying what they multiply or look up.

- [ ] **Step 4: Run it, then add the round-trip test.** Step 1's test should pass. Then add a test that sends a component built the way the schema describes it through the real create-offer handler:

```rust
#[tokio::test]
async fn a_component_shaped_by_the_schema_is_accepted_by_create_offer() {
    let ctx = ctx().await;
    let (create, input) =
        admin_create_msg("/b/products/api/admin/products", serde_json::json!({"name": "Bag"}));
    let product = output_to_json(dispatch(&ctx, create, input).await).await;
    let product_id = product["id"].as_str().expect("product id").to_string();

    // `key` and `label` strings, and `amount` as the `fixed` variant of the
    // `type`-tagged `AmountRule`, as the derived schema describes them.
    let schema = route_input(HttpMethod::Post, CREATE_OFFER);
    let amount = &schema["properties"]["components"]["items"]["properties"]["amount"];
    assert!(serde_json::to_string(amount).unwrap().contains("\"fixed\""), "{amount}");
    let body = serde_json::json!({
        "name": "250 g bag", "mode": "payment", "currency": "nzd",
        "pricing_model": "fixed", "usage_type": "licensed",
        "billing_scheme": "per_unit", "tax_behavior": "unspecified",
        "components": [{"key": "bag", "label": "250 g bag",
                        "amount": {"type": "fixed", "unit_amount_minor": 1450}}]
    });
    let (create_offer, input) =
        admin_create_msg(&format!("/b/products/api/admin/products/{product_id}/offers"), body);
    let offer = output_to_json(dispatch(&ctx, create_offer, input).await).await;
    assert_eq!(
        offer["offer"]["components"][0]["amount"]["unit_amount_minor"], 1450,
        "{offer}"
    );
}
```

Run: `cargo test -p impresspress-core offer_schema`. Expected: PASS.

- [ ] **Step 5: Assert `$defs.Condition` in the projection.** In `tests/dev_tools_manifest.rs`, replace the doc on `shop_create_offer_merges_its_path_and_body_schemas` (50–73, which cites the hand-written schema at a stale `products/mod.rs:386` and explains why `$defs` is not asserted) with:

```rust
/// `shop_create_offer` merges two sources into one flat `inputSchema`: the
/// path template's `{product_id}` and the `POST` body, which is
/// `OfferDefinitionRequest`'s derived schema (the create-offer row in
/// `products/routes.rs`). Both halves must survive the merge intact — a
/// client that lost either could not build a working call.
///
/// The body reaches the recursive `Condition` (a component's `condition`
/// can hold `all`/`any`/`not` of further conditions), which no finite
/// inlining expresses, so the projection keeps it as a root-level
/// `$defs.Condition` with `"$ref": "#/$defs/Condition"` back-edges. Chrome
/// accepts that shape at `registerTool` (probed 2026-10-08). The merge must
/// carry the table along: a `$ref` left pointing at a table the merge
/// dropped would be a schema no client can resolve.
```

and add to the test body, after the `required` checks:

```rust
    // From the body's recursive `Condition`, closed with a root-level table.
    assert!(input["$defs"]["Condition"].is_object(), "{create}");
```

- [ ] **Step 6: Regenerate the snapshots and read the diff.**

```bash
UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --features block-dev,wasm --test openapi_snapshot --test endpoint_surface
UPDATE_DEV_TOOLS_SNAPSHOT=1 cargo test -p impresspress-core --features block-dev,wasm --test dev_tools_manifest
```

Read the diff in `dev.tools.json`: `shop_create_offer.inputSchema.properties.components.items` has `key`, `label`, `amount` and the `type`-tagged `AmountRule` variants, and the schema has `$defs.Condition`. Read `products.openapi.json`: `Condition` sits under `components/schemas`, and no `$ref` points at a `#/$defs/…` (every one points into `#/components/schemas/…`). Then run both commands without the variables and confirm they pass. Commit the snapshots with the code.

- [ ] **Step 7: Chrome registers the derived schema (Review Focus 4).** In `dev-workspace.spec.ts`'s first test, just before its existing `shop_create_offer` call, add:

```ts
  // The derived, recursive offer schema registers in Chrome's own registry
  // (Review Focus 4; confirmed by the 2026-10-08 probe, kept as a light
  // check) and carries the component shape an agent fills in.
  const createOffer = (await registeredTools(page, 1)).find((t) => t.name === 'shop_create_offer');
  expect(createOffer, 'shop_create_offer is registered').toBeTruthy();
  const items = (createOffer!.inputSchema as {
    properties: { components: { items: { properties: Record<string, unknown> } } };
  }).properties.components.items;
  expect(Object.keys(items.properties)).toEqual(expect.arrayContaining(['key', 'label', 'amount']));
```

The test's existing `shop_create_offer` call (`SHOP_OFFER`, a `per_unit` component) already goes through `execute()` and `structured()`, which is the round trip in the real browser. Run the dev-sandbox spec. Expected: PASS.

- [ ] **Step 8: Run the full crate tests, fmt, clippy, commit, PR, merge.** `cargo test -p impresspress-core --features block-dev,wasm`, `cargo +nightly fmt --all`, `cargo clippy --all-targets`. Commit: `fix(products): derive offer schemas now that OpenAPI hoists $defs`. Merge once the Opus review and CI pass.

---

### Task 4: Leave `server_only` endpoints out of browser discovery; `checkout_available` (F3)

**Repo/branch:** impresspress, `fix/browser-discovery-server-only`. **Blocked by:** Task 2 merged. **Depends on:** Task 1 merged, for Step 7's e2e (registry helpers).

**Files:**
- Modify: `Cargo.toml` (workspace): bump every `wafer-*` `rev` (lines 44–46 and the rest of the `wafer-*` entries) to Task 2's merge SHA; re-resolve and commit `Cargo.lock`.
- Create: `crates/impresspress-core/src/runtime_kind.rs` (declared `pub mod runtime_kind;` in `lib.rs`): the runtime-kind key and `is_browser`. It cannot stay in `blocks::products`, which is behind `#[cfg(feature = "block-products")]` (`blocks/mod.rs:44`) while `pipeline.rs` is not.
- Modify: every user of `products::RUNTIME_KIND_CONFIG_KEY` (`git grep -n RUNTIME_KIND_CONFIG_KEY -- crates`): `blocks/products/mod.rs:92,103-105` (definition and `stripe_secret_operations_allowed`), `blocks/config.rs:1106,1119`, `platform_state/variables.rs:495,498`, `impresspress-web/src/lib.rs:278`, `blocks/products/tests/config_tests.rs:57,66`, `blocks/products/tests/storefront_tests.rs:95,462`. No re-export is left behind.
- Modify: `crates/impresspress-core/src/endpoint_match.rs`: `EndpointRoute` gains `pub server_only: bool` (`false` in `with_auth`, 239–260) and `pub const fn server_only(mut self) -> Self`; `declare` (336–376) calls `ep = ep.server_only()` when it is set.
- Modify: `crates/impresspress-core/src/blocks/products/routes.rs`: `.server_only()` on exactly the rows whose handler refuses unconditionally in the browser runtime: `Route::Checkout` (1453), `Route::Webhook` (1431), `Route::AdminReplayWebhookEvent` (953), `Route::AdminSyncOffer` (731), `Route::SyncOwnOffer` (1145), `Route::AdminCreatePaymentLink` (814), `Route::CreateOwnPaymentLink` (1228). Not the archive-offer or deactivate-payment-link rows: `archive_offer_catalog` and `deactivate_payment_link` (`stripe.rs:1900`, `:2474`) succeed in the browser for an offer never synced and a link never sent to Stripe.
- Modify: `crates/impresspress-core/src/pipeline.rs`: replace `enabled_infos` (64–72) with `discoverable_infos(ctx, block_infos, features)` and call it at both sites (322 and 381).
- Modify: `crates/impresspress-core/src/blocks/products/contracts.rs` (`StorefrontConfig`, 648): add `pub checkout_available: bool`. `blocks/products/handlers/commerce.rs` (`handle_storefront_config`, 45–79): set it.
- Modify: `crates/impresspress-core/src/blocks/products/assets/storefront.js`: fetch the config on load for `hosted`/`embedded`, hide the button when `checkout_available` is false.
- Modify (mocks that mount a `hosted` or `embedded` widget, so they answer the config the widget now fetches on load): `crates/impresspress-web/tests/e2e/products-storefront.spec.ts` (tests at 171, 252, 318), `crates/impresspress-web/tests/e2e/products-lifecycle.spec.ts` (API routes at 277, 348, 531), `examples/tests/products-examples.spec.ts:120-126`.
- Regenerate: `products.openapi.json`, `products.endpoints.json` (gains `"server_only": true` on the seven rows; `StorefrontConfig`'s schema gains `checkout_available`).
- Test: `pipeline.rs`'s `discovery_tests` (next to `anonymous_manifest_exposes_the_storefront_purchase_path`, 2000), `blocks/products/tests/storefront_tests.rs`, `products-storefront.spec.ts`, `dev-workspace.spec.ts`.

**Interfaces:**
- Consumes: `wafer_block::BlockEndpoint::server_only()` and the `server_only` field (Task 2).
- Produces: `crate::runtime_kind::{RUNTIME_KIND_CONFIG_KEY, is_browser}`; `EndpointRoute::server_only()`; `StorefrontConfig.checkout_available: bool`.

- [ ] **Step 1: Write the failing pipeline test.** In `discovery_tests`, next to `anonymous_manifest_exposes_the_storefront_purchase_path`:

```rust
#[tokio::test]
async fn browser_runtime_discovery_omits_server_only_endpoints() {
    let mut ctx = TestContext::new().await;
    ctx.set_config(crate::runtime_kind::RUNTIME_KIND_CONFIG_KEY, "browser");
    let ctx = ctx.running_as(crate::blocks::router::ROUTER_BLOCK_ID);

    let body = webmcp_manifest(&ctx, None, &real_block_infos(), &AllEnabled).await;
    let names = tool_names(&body);
    assert!(!names.contains(&"start_checkout"), "browser manifest advertises checkout: {names:?}");
    assert!(names.contains(&"preview_price"), "non-server-only tools stay: {names:?}");

    let openapi =
        discovery_json_as(&ctx, "/openapi.json", "impresspress.example.com", None).await;
    assert!(
        openapi["paths"]["/b/products/checkout"].is_null(),
        "openapi still lists checkout"
    );
    assert!(
        !openapi["paths"]["/b/products/storefront/config"].is_null(),
        "a browser-callable Public endpoint is still described"
    );
}
```

(`set_config` is `TestContext::set_config(&mut self, key, value)`, `test_support.rs:320`; `running_as` consumes `self`, hence the rebinding. `discovery_json_as` is `test_support.rs:4256`.) The existing `anonymous_manifest_exposes_the_storefront_purchase_path` (server runtime, where `start_checkout` is published) must keep passing unchanged: it is the opposite case.

- [ ] **Step 2: Run it.** `cargo test -p impresspress-core browser_runtime_discovery_omits`. Expected: FAIL to compile (`crate::runtime_kind` does not exist yet). That is the right failure for now; it fails on the assertion once Step 3's move lands and before the filter does.

- [ ] **Step 3: Implement.** Bump the pin. Create `runtime_kind.rs`:

```rust
//! Which runtime this instance is: a server, or a browser's service worker.
//!
//! The browser adapter publishes `__IMPRESSPRESS_RUNTIME_KIND__ = "browser"`
//! on the synchronous `config_get` snapshot (`impresspress-web`'s
//! `RuntimeConfig::both`); a server publishes nothing and is the default.
//! The key is runtime-owned (`__…__`), never served from the variables table,
//! so no database or admin value can make a browser look like a server.

use wafer_run::context::Context;

/// The config key the browser adapter sets to `"browser"`.
pub const RUNTIME_KIND_CONFIG_KEY: &str = "__IMPRESSPRESS_RUNTIME_KIND__";

/// Whether this runtime runs inside a browser, where nothing secret can be
/// held. Read off the synchronous `config_get` snapshot, never through the
/// config client: see `products::stripe_secret_operations_allowed` for why a
/// client read of this key is refused.
pub fn is_browser(ctx: &dyn Context) -> bool {
    ctx.config_get(RUNTIME_KIND_CONFIG_KEY) == Some("browser")
}
```

`products/mod.rs` drops its constant; `stripe_secret_operations_allowed` keeps its doc and becomes `!crate::runtime_kind::is_browser(ctx)`. Update every other user listed under Files to `crate::runtime_kind::RUNTIME_KIND_CONFIG_KEY` (`impresspress_core::runtime_kind::RUNTIME_KIND_CONFIG_KEY` in `impresspress-web`).

In `endpoint_match.rs`:

```rust
    /// Set by [`Self::server_only`].
    pub server_only: bool,
…
    /// The endpoint needs something only a server holds (a secret key) and
    /// can never succeed in the browser runtime, so browser discovery leaves
    /// it out. The handler is still the gate.
    pub const fn server_only(mut self) -> Self {
        self.server_only = true;
        self
    }
```

with `server_only: false` in `with_auth` and, in `declare`, after the `deprecated` block:

```rust
            if row.server_only {
                ep = ep.server_only();
            }
```

Add `.server_only()` to the seven rows listed under Files. Then replace `enabled_infos` in `pipeline.rs`, keeping its existing doc paragraph about the feature toggle and adding the runtime half:

```rust
/// The registered blocks every discovery projection (`/openapi.json`, the
/// agent card and the WebMCP manifest) is generated from, in this runtime.
///
/// `block_infos` is every REGISTERED block, but `route_to_block` 404s any
/// block the toggle has turned off (routing.rs's feature gate, backed by the
/// live `block_settings` row). Describing a disabled block's endpoints would
/// hand the reader routes that 404 on every call, so the documents are built
/// from the enabled subset only — gated under the same name the router gates
/// with (`feature_gate_name`; the inspector's `BlockInfo` name and its
/// route's `block` name differ).
///
/// In the browser runtime every endpoint declared `server_only` is dropped
/// as well: it needs something only a server holds and can never succeed
/// there. Both discovery branches call this one function, so the three
/// documents agree.
fn discoverable_infos(
    ctx: &dyn Context,
    block_infos: &[BlockInfo],
    features: &dyn FeatureConfig,
) -> Vec<BlockInfo> {
    let browser = crate::runtime_kind::is_browser(ctx);
    block_infos
        .iter()
        .filter(|b| {
            crate::features::is_enabled(features, block_infos, routing::feature_gate_name(&b.name))
        })
        .cloned()
        .map(|mut info| {
            if browser {
                info.endpoints.retain(|ep| !ep.server_only);
            }
            info
        })
        .collect()
}
```

At 322 and 381: `let discoverable = discoverable_infos(ctx, block_infos, features);`, and pass `&discoverable` to `generate_openapi`, `generate_agent_card` and `generate_webmcp_report` where they read `&enabled_infos` today.

- [ ] **Step 4: Run it.** `cargo test -p impresspress-core browser_runtime_discovery_omits` and `cargo test -p impresspress-core pipeline::discovery_tests`. Expected: PASS, along with the existing discovery tests. Also `cargo test -p impresspress-core --test wafer_guest_parity`: PASS unchanged (the new field is never serialized as `false`).

- [ ] **Step 5: `checkout_available` test, then implementation.** In `storefront_tests.rs`:

```rust
/// `checkout_available` is true exactly when `POST /b/products/checkout`
/// gets past its availability guards: secret-key operations allowed in this
/// runtime, and a Stripe secret key configured.
#[tokio::test]
async fn storefront_config_reports_whether_checkout_can_run() {
    async fn reported(ctx: &crate::test_support::TestContext) -> serde_json::Value {
        let (msg, input) = get_msg("/b/products/storefront/config", "");
        output_to_json(dispatch(ctx, msg, input).await).await
    }
    let server = ctx_with(&[("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_configured")]).await;
    assert_eq!(reported(&server).await["checkout_available"], true);

    let unconfigured = ctx().await;
    assert_eq!(reported(&unconfigured).await["checkout_available"], false);

    let browser = ctx_with(&[
        (crate::runtime_kind::RUNTIME_KIND_CONFIG_KEY, "browser"),
        ("IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "sk_test_configured"),
    ])
    .await;
    let body = reported(&browser).await;
    assert_eq!(body["checkout_available"], false, "{body}");
    assert_eq!(body["embedded_checkout_available"], false, "{body}");
}
```

Run it and watch it fail. Implement: in `StorefrontConfig`, after `embedded_checkout_available`:

```rust
    /// Whether `POST /b/products/checkout` can run here: secret-key
    /// operations are allowed in this runtime and a Stripe secret key is
    /// configured. False in the browser runtime. `embedded_checkout_available`
    /// implies it; it does not imply `embedded_checkout_available`. Payment
    /// Links do not need it: they never call checkout.
    pub checkout_available: bool,
```

and in `handle_storefront_config`, `checkout_available: secret_operations_allowed && !secret.trim().is_empty(),` (both values are already read there). Run it and watch it pass.

- [ ] **Step 6: Widget, and its mocks.** In `storefront.js`: initialise `this.storefrontConfig = null;` in the constructor; add `<p class="checkout-unavailable" hidden>Checkout isn't available in this preview.</p>` after the `button.checkout` in `renderShell`, and `this.unavailableNode = this.shadowRoot.querySelector(".checkout-unavailable");` with the other nodes. In `load()`, fetch the config with the product, except for Payment Links:

```js
      try {
        // Payment Links never call checkout, so they need no config; the
        // other two presentations need to know whether checkout can run here.
        const [product, storefrontConfig] = await Promise.all([
          this.request(`/b/products/storefront/${encodeURIComponent(this.productId)}`),
          this.presentation === "payment_link" ? null : this.request("/b/products/storefront/config"),
        ]);
        this.product = product;
        this.storefrontConfig = storefrontConfig;
        this.renderProduct();
```

In `selectOffer`, the non-Payment-Link branch:

```js
      } else {
        // `POST /b/products/checkout` cannot succeed here (the browser
        // runtime, or no Stripe secret key): say so rather than offer a
        // button that can only fail. The price preview still runs.
        const available = Boolean(this.storefrontConfig && this.storefrontConfig.checkout_available);
        this.checkoutNode.hidden = !available;
        this.unavailableNode.hidden = available;
        this.schedulePreview(0);
      }
```

and in the `payment_link` branch, `this.checkoutNode.hidden = false; this.unavailableNode.hidden = true;`. In `checkout()`, the embedded branch uses `this.storefrontConfig` instead of fetching it again:

```js
        if (this.presentation === "embedded") {
          Stripe = await stripeJs();
          storefrontConfig = this.storefrontConfig;
          if (!storefrontConfig.embedded_checkout_available || !storefrontConfig.stripe_publishable_key) {
            throw new Error("Embedded Checkout is not configured");
          }
        }
```

Update every mock that mounts a `hosted` or `embedded` widget to answer the config, with `checkout_available: true` (they test a server that can check out): `products-storefront.spec.ts` tests at 171 and 318 gain a `/b/products/storefront/config` branch, and the one at 252 adds `checkout_available: true` to its existing answer; `products-lifecycle.spec.ts`'s three API routes (277, 348, 531) gain `if (url.pathname.endsWith("/storefront/config")) return json(route, { schema_version: 1, checkout_available: true, embedded_checkout_available: false });` before their fallback; `examples/tests/products-examples.spec.ts:124` adds `checkout_available: true`. The Payment Link test at 233 keeps asserting `apiPaths` is only `product_static`: it is the check that Payment Links make no config request.

Add to `products-storefront.spec.ts` two tests against the config the browser runtime serves (`mount` locates `impresspress-product` without an index, so each test mounts one widget; `product(true)` is the fixture whose offer carries a Payment Link):

```ts
  /** What the browser runtime's `/b/products/storefront/config` answers. */
  const BROWSER_RUNTIME_CONFIG = {
    schema_version: 1,
    checkout_available: false,
    embedded_checkout_available: false,
  };

  test("a runtime that cannot check out shows a line instead of the checkout button", async ({ page }) => {
    await page.route(`${apiOrigin}/**`, async (route) => {
      const path = new URL(route.request().url()).pathname;
      if (path === "/b/products/storefront/product_static") return json(route, product());
      if (path === "/b/products/pricing/preview") return json(route, quote());
      if (path === "/b/products/storefront/config") return json(route, BROWSER_RUNTIME_CONFIG);
      return json(route, { error: "unexpected route" }, 404);
    });

    await openStaticPage(page);
    const widget = await mount(page, "hosted");
    await expect(widget.getByText("Checkout isn't available in this preview.")).toBeVisible();
    await expect(widget.getByRole("button", { name: /checkout/i })).toHaveCount(0);
    // The price still previews: only the purchase step is unavailable.
    await expect(widget.locator(".total span:last-child")).toHaveText("NZD 64.00");
  });

  test("a Payment Link still sells where checkout cannot run", async ({ page }) => {
    const apiPaths: string[] = [];
    await page.route(`${apiOrigin}/**`, async (route) => {
      const path = new URL(route.request().url()).pathname;
      apiPaths.push(path);
      if (path === "/b/products/storefront/product_static") return json(route, product(true));
      if (path === "/b/products/storefront/config") return json(route, BROWSER_RUNTIME_CONFIG);
      return json(route, { error: "unexpected route" }, 500);
    });

    await openStaticPage(page);
    const widget = await mount(page, "payment_link");
    await expect(widget.getByRole("button", { name: "Buy with Stripe" })).toBeEnabled();
    await expect(widget.getByText("Checkout isn't available in this preview.")).toBeHidden();
    // A Payment Link never calls checkout, so it never asks whether it can.
    expect(apiPaths).toEqual(["/b/products/storefront/product_static"]);
  });
```

Run `npx playwright test --config=tests/playwright.config.ts tests/e2e/products-*.spec.ts` and `cd examples && npm run test:products`. Expected: PASS with no screenshot change. If one changes anyway, regenerate through `regen-visual-baselines.yml`, read each PNG, and push again yourself.

- [ ] **Step 7: The real browser runtime.** In `dev-workspace.spec.ts`'s shopper section (after the widget title assertion at ~609), add:

```ts
  // The browser runtime cannot run Stripe checkout: the widget says so
  // instead of showing a button that can only fail, and the shopper's agent
  // is not offered `start_checkout`.
  const widget = shop.locator('impresspress-product');
  await expect(widget.getByText("Checkout isn't available in this preview.")).toBeVisible();
  await expect(widget.getByRole('button', { name: /checkout/i })).toHaveCount(0);
```

and after its `shopperTools` read, `expect(shopperTools).not.toContain('start_checkout');`. Rewrite the comment above the widget title assertion, which ends "and an anonymous browser can buy from it": it now ends "and an anonymous browser can see it priced". Run the dev-sandbox spec. Expected: PASS.

- [ ] **Step 8: Regenerate snapshots, read the diff, run fmt, clippy and the crate tests, commit, PR, merge.** `UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --features block-dev,wasm --test openapi_snapshot --test endpoint_surface`; the diff is `"server_only": true` on exactly the seven rows in `products.endpoints.json` and `checkout_available` in `StorefrontConfig`'s schema. Commit: `fix(discovery): browser runtime omits server_only endpoints; storefront reports checkout_available`. Merge once the Opus review and CI pass.

---

### Task 5: Agent-facing docs (F4)

**Repo/branch:** impresspress, `docs/agent-facing-dev-docs`. **Soft dependency:** Task 3, because the guide example's description of `amount` matches the derived schema (the test below pins it against the type either way).

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/contracts.rs:318-329`: the `expected_sha256` doc comment.
- Modify: `crates/impresspress-core/src/blocks/dev/contracts.rs:556-575` (`ReferenceResponse`): add `pub suggested_prompt: String`.
- Modify: `crates/impresspress-core/src/blocks/dev/scaffold.rs:268-281` (`handle_reference`): fill it from `SeedInfo.suggested_prompt` (`blocks/dev/repo/seed_info.rs:33`).
- Modify: `examples/dev-sandbox/seeds/{bootstrap,blank}/guide.md`: a "Pricing an offer" section and a "Theming the product widget" section.
- Modify: `examples/dev-sandbox/seeds/llms-preamble.md`: one sentence about `suggested_prompt`. Each seed's `llms.txt` is not checked in: `examples/dev-sandbox/build.sh` generates it (`seeds/seedlib.py`'s `staged_llms`) from this preamble and the seed's guide, and `seeds/*/manifest.json` records both hashes.
- Regenerate: `examples/dev-sandbox/seeds/{bootstrap,blank}/manifest.json` with `seeds/write-manifest.py`; the `dev.tools.json`, `dev.openapi.json` and `dev.endpoints.json` snapshots.
- Test: `crates/impresspress-core/tests/dev_scaffold.rs` (`reference_returns_the_site_guide_the_seed_carried` at 436, `reference_without_a_seed_guide_answers_null_fields` at 467), and `crates/impresspress-core/tests/dev_seed_prompts.rs` for the guide example.

- [ ] **Step 1: Write the failing test.** In `dev_scaffold.rs`, `reference_returns_the_site_guide_the_seed_carried` already writes a `SeedInfo` with `suggested_prompt: "Build me a shop."`; add `assert_eq!(body["suggested_prompt"], "Build me a shop.");`. In `reference_without_a_seed_guide_answers_null_fields`, add `assert_eq!(body.get("suggested_prompt"), Some(&serde_json::Value::String(String::new())));` and update its doc ("both fields are null, the prompt is empty, and the call still answers"). Run `cargo test -p impresspress-core --features block-dev,wasm --test dev_scaffold reference_`. Expected: FAIL.

- [ ] **Step 2: Implement `suggested_prompt`.** In `ReferenceResponse`:

```rust
    /// The task this template was designed to walk an agent through, as the
    /// workspace page suggests it. Empty when the template suggests none or
    /// the sandbox carries no seed.
    pub suggested_prompt: String,
```

In `handle_reference`:

```rust
    no_store().json(&ReferenceResponse {
        wafer_guest_version: WAFER_GUEST_VERSION,
        markdown: reference_markdown(),
        template: seed.as_ref().map(|seed| seed.template.clone()),
        suggested_prompt: seed
            .as_ref()
            .map(|seed| seed.suggested_prompt.clone())
            .unwrap_or_default(),
        site_markdown: seed.map(|seed| seed.guide_markdown),
    })
```

Run the tests and see them pass.

- [ ] **Step 3: Rewrite `expected_sha256`'s doc comment.** Replace the whole comment (318–328) with:

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

An offer's price is a list of `components`. A fixed-price product has one.
This is a complete `shop_create_offer` argument (`product_id` is the id
`shop_create_product` returned):

```json
{"product_id": "<id from shop_create_product>",
 "name": "250 g bag", "mode": "payment", "currency": "nzd",
 "pricing_model": "fixed", "usage_type": "licensed",
 "billing_scheme": "per_unit", "tax_behavior": "unspecified",
 "components": [{"key": "bag", "label": "250 g bag",
                 "amount": {"type": "fixed", "unit_amount_minor": 1450}}]}
```

Amounts are integer minor units (1450 = 14.50). `currency` is a lowercase
three-letter ISO code. `shop_create_offer`'s schema lists the other
`amount` types, which price from a customer input.

## Theming the product widget

`<impresspress-product>` renders in a shadow root and reads five custom
properties. Set them on the element (or any ancestor):

| Property | Default | Used for |
|---|---|---|
| `--ip-accent` | `#2563eb` | buttons, checkbox accents |
| `--ip-bg` | `#fff` | the card background |
| `--ip-border` | `#dbe3ee` | borders and dividers |
| `--ip-muted` | `#617089` | descriptions, help text, status |
| `--ip-text` | `#172033` | body text |
````

(The five names and defaults come from the widget's `:host` rule, `storefront.js:142`; confirm with `grep -o -- "--ip-[a-z]*:[^;]*" crates/impresspress-core/src/blocks/products/assets/storefront.js | sort -u`.)

Add to `dev_seed_prompts.rs` (file-level `cfg` is `block-dev`; this test also needs products):

```rust
/// Every seed guide's "Pricing an offer" example is an argument the create-
/// offer endpoint accepts: `product_id` goes to the path, and the rest
/// deserializes into the handler's own type, `deny_unknown_fields` and all.
#[cfg(feature = "block-products")]
#[test]
fn every_guide_offer_example_is_a_valid_create_offer_argument() {
    use impresspress_core::blocks::products::contracts::OfferDefinitionRequest;
    for (path, text) in seed_files("guide.md") {
        let section = text
            .split("## Pricing an offer")
            .nth(1)
            .unwrap_or_else(|| panic!("{}: no \"## Pricing an offer\"", path.display()));
        let json = section
            .split("```json")
            .nth(1)
            .and_then(|rest| rest.split("```").next())
            .unwrap_or_else(|| panic!("{}: no json block under the heading", path.display()));
        let mut argument: serde_json::Value =
            serde_json::from_str(json).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(
            argument.as_object_mut().unwrap().remove("product_id").is_some(),
            "{}: the example must say where product_id goes",
            path.display()
        );
        let parsed: OfferDefinitionRequest = serde_json::from_value(argument)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(parsed.components.len(), 1, "{}", path.display());
    }
}
```

Run `cargo test -p impresspress-core --features block-dev,wasm --test dev_seed_prompts every_guide_offer_example`. Expected: PASS. (`every_seed_guide_names_only_tools_the_page_has` in the same file must also pass: the new text names only `shop_create_offer` and `shop_create_product`. `every_seed_llms_txt_is_the_preamble_then_the_guide_as_the_manifest_declares` passes only after Step 5 rewrites the manifests.)

- [ ] **Step 5: The llms preamble, and the seed manifests.** In `seeds/llms-preamble.md`, after the "How to start" list's tool names, add: "`dev_read_reference` also returns this template's suggested task as `suggested_prompt`: read it first, it is what the page suggests to a person." Then rewrite both manifests and verify:

```bash
examples/dev-sandbox/seeds/write-manifest.py bootstrap
examples/dev-sandbox/seeds/write-manifest.py blank
examples/dev-sandbox/build.sh --check
```

Expected: `--check` passes (CI runs it, `ci-shared.yml:1035` and `:1313`).

- [ ] **Step 6: Regenerate snapshots, run fmt, clippy and the tests, commit, PR, merge.** `UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --features block-dev,wasm --test openapi_snapshot --test endpoint_surface` and `UPDATE_DEV_TOOLS_SNAPSHOT=1 cargo test -p impresspress-core --features block-dev,wasm --test dev_tools_manifest`; the diff is the new `expected_sha256` description and `suggested_prompt` in `ReferenceResponse`. Commit: `docs(dev): agent-facing reference — suggested_prompt, offer pricing, widget theming, plain expected_sha256`. Merge once the Opus review and CI pass.

---

### Task 6: `published_at` on admin activation (F6)

**Repo/branch:** impresspress, `fix/published-at-on-activation`

**Files:**
- Modify: `crates/impresspress-core/src/blocks/products/handlers/product.rs`: `handle_update_product` (494–528).
- Test: `crates/impresspress-core/src/blocks/products/tests/handler_tests.rs` (next to `admin_update_product`, 82).

The seller PATCH (`handle_user_update_product`, 1183–1195) and moderation approval (`handlers/sellers.rs:172-184`) already put `published_at = now` into the handler's data map whenever they make a product active; the admin PATCH is the one writer that does not. Mirror them in the handler, with no read of the current row: the admin PATCH deliberately does one write whose `WHERE` is the liveness test (its comment at 514–521), and both existing writers stamp on every publishing write, matching `ProductView.published_at`'s contract ("the product last became active").

- [ ] **Step 1: Write the failing test.**

```rust
/// The admin PATCH is how an admin (and the sandbox agent's
/// `shop_update_product`) publishes a product, so it stamps `published_at`
/// the way the seller PATCH and moderation approval do. Another status
/// leaves the stamp alone.
#[tokio::test]
async fn admin_activation_sets_published_at() {
    let ctx = ctx().await;
    let (create, input) =
        admin_create_msg("/b/products/api/admin/products", serde_json::json!({"name": "Bag"}));
    let id = output_to_json(dispatch(&ctx, create, input).await).await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let patch = |body: serde_json::Value| {
        let (mut msg, input) =
            request_msg("update", &format!("/b/products/api/admin/products/{id}"), "admin_1", body);
        msg.set_meta("auth.user_roles", "admin");
        (msg, input)
    };

    let (msg, input) = patch(serde_json::json!({"status": "active"}));
    let active = output_to_json(dispatch(&ctx, msg, input).await).await;
    let stamped = active["published_at"].as_str().unwrap_or_default().to_string();
    assert!(!stamped.is_empty(), "activation must set published_at: {active}");

    let (msg, input) = patch(serde_json::json!({"status": "archived"}));
    let archived = output_to_json(dispatch(&ctx, msg, input).await).await;
    assert_eq!(archived["published_at"], stamped.as_str(), "{archived}");
}
```

Run `cargo test -p impresspress-core admin_activation_sets_published_at`. Expected: FAIL (`published_at` is null).

- [ ] **Step 2: Implement it.** In `handle_update_product`, read the requested status before `into_columns()` consumes the request, then stamp in the same data map:

```rust
    let activates = request.status == Some(ProductStatus::Active);
    let mut data = request.into_columns();
    // Publishing stamps `published_at` in the same write, as the seller
    // PATCH and moderation approval do. No read first: this handler's one
    // write is also its liveness test (below), and both other writers stamp
    // on every publishing write too.
    if activates {
        data.insert(
            "published_at".to_string(),
            serde_json::Value::String(now_rfc3339()),
        );
    }
    stamp_updated(&mut data);
```

(`ProductStatus` and `now_rfc3339` are already imported in `product.rs`.) Run the test and confirm it passes, along with `cargo test -p impresspress-core products::tests::handler_tests`.

- [ ] **Step 3: fmt, clippy, tests, commit, PR, merge.** Commit: `fix(products): admin activation sets published_at`. Merge once the Opus review and CI pass.

---

### Task 7: Live acceptance (whole program)

After Tasks 1–6 are merged **and deployed**. The deploy is production: ask the user and wait for an explicit yes before running it. The paths are in memory `dev-sandbox-deploy` and `build-sandboxes-program`.

- [ ] **Step 1:** Re-run the subagent harness from the 2026-10-08 session with `BRIDGE=0` (`docs/superpowers/plans/2026-10-08-webmcp-harness/`: `harness.mjs` drives Chrome with `--enable-features=WebMCPTesting`, `ctl.sh` sends it commands; `BRIDGE=0` turns off its copy of `navigator.modelContext`), against `https://impresspress.org/build`.
- [ ] **Step 2:** The agent's report must show:
  - tools registered with no bridge;
  - `shop_create_offer` succeeding on the first call;
  - no `start_checkout` on the visitor page, and the widget showing the preview line;
  - `dev_read_reference.suggested_prompt` non-empty;
  - a product the agent activated with a non-null `published_at`.
- [ ] **Step 3:** File anything new as follow-up issues; do not fold it into these PRs.
