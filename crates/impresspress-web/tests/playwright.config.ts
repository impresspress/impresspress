import { defineConfig, devices } from '@playwright/test';

const PORT = process.env.TEST_PORT ? parseInt(process.env.TEST_PORT) : 8080;

// Base config used by smoke.spec.ts (port 8080, browser-WASM static server,
// no admin login API). Visual-baseline tests use a separate config that
// extends this with `globalSetup` — see `playwright.visual-baseline.config.ts`.
export default defineConfig({
  testDir: './e2e',
  // Snapshot baselines live in Playwright's default location next to the
  // specs (`e2e/<spec>-snapshots/`). They must NOT be pointed at
  // `.playwright-mcp/` — that is the Playwright MCP server's scratch dir
  // (page snapshots, console logs) and is gitignored, which silently made
  // the committed baselines unstageable.
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0,
  workers: 1,
  // Paths below are resolved relative to THIS config file's directory
  // (`tests/`), not the working directory. CI uploads the report and the
  // actual/diff PNGs from the crate root, so both are lifted one level out
  // of `tests/`. Without this the html report was never written at all and
  // every `Upload Playwright report on failure` step in ci-shared.yml uploaded
  // nothing, leaving screenshot failures with no diff images to inspect.
  reporter: [['list'], ['html', { open: 'never', outputFolder: '../playwright-report' }]],
  outputDir: '../test-results',
  timeout: 60_000,
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    serviceWorkers: 'allow',
    // What it is for: the runtime sanitizes an internal error down to
    // `Internal server error (ref: <id>)` and logs the real cause behind that
    // ref, and in the browser that log goes to the service worker's console —
    // which, without this, nothing records. Four sanitized 500s across four
    // unrelated pull requests were diagnosed with their causes already gone.
    // The trace carries the failing request, its response and the console
    // alongside it, and both dev-sandbox CI jobs already upload the report
    // (which embeds the trace) on failure. See also
    // `forwardSandboxDiagnostics` in `e2e/fixtures/dev-sandbox.ts`, which puts
    // the same lines in the job log.
    //
    // What it COSTS, stated honestly: `retain-on-failure` is not `off` for a
    // passing test. Playwright records the trace for EVERY test and deletes it
    // when the test passes, so the recording overhead is paid on the whole
    // suite and only the artifact is conditional. These jobs are already
    // fighting timing-sensitive races and one of them publishes a timing
    // baseline, so that overhead is a real input to both. It is accepted here
    // because four undiagnosable failures cost more than a uniform slowdown
    // does — but it is a trade, not a free option, and `on-first-retry` is the
    // cheaper setting if the recording ever shows up in the baseline.
    trace: 'retain-on-failure',
  },
  projects: [
    { name: 'desktop-chrome', use: { ...devices['Desktop Chrome'] } },
  ],
});
