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

/** What a tool's `execute` resolves to (the MCP `CallToolResult` shape). */
export type ToolResult = {
  isError?: boolean;
  content: Array<{ type: string; text: string }>;
  structuredContent?: Record<string, unknown>;
};

/** Everything the registry holds right now, exactly as `listTools()` says. */
async function listTools(page: Page): Promise<Array<{ name: string; description: string; inputSchema: string }>> {
  return page.evaluate(() =>
    (navigator as unknown as { modelContextTesting: ModelContextTesting }).modelContextTesting.listTools(),
  );
}

/**
 * Wait until at least `atLeast` tools are registered, then return all of
 * them.
 *
 * "At least", not "exactly": on `/b/dev` both `dev.js` (the page-scoped
 * `dev_*`/`shop_*` allowlist) and `webmcp.js` (the deployment-wide manifest)
 * register into the same `navigator.modelContext`, and they finish in
 * whichever order their two fetches complete. A caller that needs a specific
 * tool from the slower registrar waits for it by name with [`waitForTool`].
 *
 * The waits poll from the test side (`expect.poll`), not with
 * `page.waitForFunction`: `listTools()` answers with a promise, and
 * `waitForFunction` does not await its predicate — a promise is truthy, so
 * an async predicate there resolves at once whatever the registry holds.
 */
export async function registeredTools(page: Page, atLeast: number): Promise<ToolRecord[]> {
  await expect
    .poll(async () => (await listTools(page)).length, {
      message: `at least ${atLeast} WebMCP tools registered`,
      timeout: 15_000,
    })
    .toBeGreaterThanOrEqual(atLeast);
  return (await listTools(page)).map((t) => ({
    name: t.name,
    description: t.description,
    // The registry reports the schema as a JSON string; callers read objects.
    inputSchema: JSON.parse(t.inputSchema) as Record<string, unknown>,
  }));
}

/**
 * Wait until a tool with this exact name is registered.
 *
 * The counting wait above cannot express "the other registrar has finished
 * too" without pinning a total that belongs to a different file's contract.
 * Waiting for one name it publishes is the same fact without the coupling.
 */
export async function waitForTool(page: Page, name: string): Promise<void> {
  await expect
    .poll(async () => (await listTools(page)).map((t) => t.name), {
      message: `WebMCP tool ${name} registered`,
      timeout: 15_000,
    })
    .toContain(name);
}

/** Every registered tool's name, sorted. */
export async function toolNames(page: Page): Promise<string[]> {
  return (await listTools(page)).map((t) => t.name).sort();
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

/**
 * The structured half of a tool result, with "and it was not an error" folded
 * in.
 *
 * Every tool the sandbox specs call declares an `outputSchema`
 * (`impresspress-core/tests/snapshots/dev.tools.json`), so `webmcp-core.js`
 * parses each success body into `structuredContent` — a tool that came back
 * with only a text block either failed or lost its schema, and both are
 * defects rather than shapes to branch on. `content[0].text` is the message on
 * the failure path (`Request failed (409): …`), which is what makes a broken
 * assertion readable.
 *
 * Shared rather than copied: `dev-compile.spec.ts`, `dev-workspace.spec.ts`
 * and `dev-scenario.spec.ts` all unwrap tool results the same way, and three
 * copies of "assert not-an-error, then cast" would be three places for the
 * failure message to drift.
 */
export function structured<T>(result: ToolResult): T {
  expect(result.isError, result.content[0]?.text).toBeFalsy();
  expect(result.structuredContent, JSON.stringify(result)).toBeTruthy();
  return result.structuredContent as unknown as T;
}
