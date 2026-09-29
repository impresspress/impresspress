import { expect, type BrowserContext, type Page } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { readFileSync } from 'node:fs';
import path from 'node:path';

/**
 * Getting a page onto the browser development sandbox.
 *
 * Every sandbox spec (`dev-foundations.spec.ts`, `dev-workspace.spec.ts`,
 * `dev-compiler.spec.ts`, `dev-compile.spec.ts`, `dev-scenario.spec.ts`)
 * starts the same way — boot the service worker, sign in as the seeded admin,
 * and for most of them land on `/b/dev` — and none of it is what any of them
 * is testing. The boot wait alone is subtle enough (three separate
 * conditions, each for its own reason) that a second copy of it would be a
 * second thing to get wrong.
 *
 * These helpers are for the SANDBOX bundle (`examples/dev-sandbox/build.sh`,
 * served on `TEST_PORT` by a plain static host), not for the native server the
 * rest of the suite uses. That is why they log in through the form instead of
 * reusing `fixtures/auth.ts`'s saved `storageState`: `global-setup.ts` posts to
 * a server that is not running in this job, and the sandbox's credentials are
 * the seeded ones every fresh origin creates for itself.
 */

/**
 * A stable phrase from the real welcome page
 * (`examples/dev-sandbox/seed/site/index.html`) — the "Open workspace" link
 * text, present on every render regardless of copy edits elsewhere on the
 * page.
 */
export const WELCOME_PHRASE = 'Open workspace';

/**
 * That page's heading. Used where the assertion has to be about what
 * RENDERED rather than what the document contains — an `<h1>` inside the
 * `/b/dev` preview iframe, say, which only has text if the frame really
 * loaded the site.
 */
export const WELCOME_HEADING = 'ImpressPress dev sandbox';

/** The credentials `impresspress-core`'s auth block seeds into a new instance. */
export const ADMIN_EMAIL = 'admin@example.com';
export const ADMIN_PASSWORD = 'admin123';

/**
 * Contexts already carrying a console forwarder, so a spec that boots twice in
 * one context does not print every line twice.
 */
const FORWARDING_CONSOLE = new WeakSet<BrowserContext>();

/**
 * Pages already carrying an uncaught-exception forwarder.
 *
 * A SEPARATE set from `FORWARDING_CONSOLE`, and the separation is the point:
 * `console` is a context-scoped event and `pageerror` is a page-scoped one, so
 * a single guard keyed on the context would arm the console listener on the
 * first page of a context and then silently attach NOTHING to the second —
 * and a second page is what several specs actually put under test (the
 * anonymous visitor in `dev-compile.spec.ts`, the storefront in
 * `dev-scenario.spec.ts` and `dev-workspace.spec.ts`). A listener that
 * silently captures nothing is the exact failure this forwarder exists to
 * prevent.
 */
const FORWARDING_PAGE_ERRORS = new WeakSet<Page>();

/**
 * Print the sandbox's own error and warning logs to the test runner's stdout.
 *
 * The runtime never sends an internal error's cause to the client: `err_internal`
 * sanitizes it to `Internal server error (ref: <id>)` and logs the real cause
 * behind that ref. On the native server that log is the process's stderr. In
 * the browser it is `console.error` **inside the service worker**
 * (`impresspress-browser`'s `tracing` bridge), and by default nothing in a
 * test run records it. So four intermittent sanitized 500s across four
 * unrelated pull requests were each diagnosed with the one piece of evidence
 * that would have named the cause already destroyed.
 *
 * Chromium routes a service worker's console message to the browser context,
 * to the worker handle, and to every page inside the worker's scope — all
 * three, in the Playwright this repo pins (see `playwright-core`'s
 * `browserContext` console dispatch). `dev-scenario.spec.ts` takes the
 * worker-handle route successfully, and that is the better one for a spec that
 * wants the worker ALONE. This forwarder wants both halves, so it takes the
 * context route for two reasons the worker route cannot give it: one listener
 * carries page messages as well as worker ones, and it can be armed before the
 * first navigation without first awaiting a `serviceworker` event — the worker
 * does not exist yet at that point, and its boot-time logs are the ones worth
 * having. `msg.page()` is null for a worker message, which is what labels the
 * origin below.
 *
 * Both halves go to stdout rather than to a Playwright attachment, because CI
 * tees the runner's output into the job log and that survives a run in which
 * the report artifact is not what anyone opens first.
 *
 * Errors and warnings only: the sandbox logs steadily at info while it boots,
 * migrates and seeds, and a forwarder that reprinted all of it would bury the
 * one line worth having.
 */
export function forwardSandboxDiagnostics(page: Page) {
  const context = page.context();
  if (!FORWARDING_CONSOLE.has(context)) {
    FORWARDING_CONSOLE.add(context);
    context.on('console', (msg) => {
      const level = msg.type();
      if (level !== 'error' && level !== 'warning') return;
      // A message with no page came from the service worker; the runtime's own
      // logs are all of that kind, and they are the ones that carry the ref.
      const origin = msg.page() === null ? 'worker' : 'page';
      console.log(`[sandbox ${origin} ${level}] ${msg.text()}`);
    });
  }
  // Page-scoped, so EVERY page a spec boots gets one — including the second
  // page of a context whose console listener is already armed.
  if (!FORWARDING_PAGE_ERRORS.has(page)) {
    FORWARDING_PAGE_ERRORS.add(page);
    page.on('pageerror', (error) => {
      console.log(`[sandbox page uncaught] ${error.message}`);
    });
  }
}

/**
 * Load `/` and wait until the service worker is serving it.
 *
 * The first load of an origin gets the static boot shell: `loader.js`
 * registers `sw.js`, the worker claims the page and the loader reloads, and
 * only the *second* document comes from the wasm runtime. The worker
 * initialises lazily on its first fetch, so by the time it controls a page it
 * has already run `initialize()` — wasm boot, migrations, seed import and
 * boot convergence included.
 *
 * Deliberately a *neutral* signal, not "the page renders X": what `/` serves
 * is itself under test in `dev-foundations.spec.ts`, and a boot helper that
 * asserted it would abort every test on one defect instead of reporting each
 * on its own line.
 *
 * `waitUntil: 'commit'` for the same reason `smoke.spec.ts` uses it — the boot
 * shell pulls `/webllm-engine.js` and `/embed-engine.js` as modules, and
 * neither `load` nor `domcontentloaded` fires promptly behind them.
 */
export async function bootServiceWorker(page: Page) {
  // Before the navigation that registers the worker: the listener has to be
  // in place before the worker exists, or its boot-time logs are lost.
  forwardSandboxDiagnostics(page);
  await page.goto('/', { waitUntil: 'commit' });
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null, null, {
    timeout: 120_000,
  });
  // `controller !== null` says a worker claimed the page; it does not say the
  // worker has finished `initialize()`, because `sw.js` initialises lazily on
  // its first fetch. One round trip through a route only the runtime can
  // answer (the static host would 404 it) is what makes the wait mean "the
  // sandbox is up" — and it is what the timings the specs print measure.
  await page.waitForFunction(async () => (await fetch('/b/auth/login')).status === 200, null, {
    timeout: 120_000,
  });
  // …and wait for the boot shell to be replaced. `loader.js` reloads the page
  // once the worker controls it, on a `setTimeout(…, 0)` this function cannot
  // see; navigating on top of that pending reload is the
  // `net::ERR_ABORTED at /` a caller would otherwise hit. `#status` is the
  // shell's own progress line (`index.html.tmpl`) and exists on no page the
  // runtime serves, so its absence means the reload has landed.
  await page.waitForFunction(() => document.getElementById('status') === null, null, {
    timeout: 120_000,
  });
}

/** Sign in as the seeded admin and land anywhere but the login page. */
export async function loginAdmin(page: Page) {
  await page.goto('/b/auth/login?redirect=/b/dev/api/status', { waitUntil: 'commit' });
  await page.locator('input#email').fill(ADMIN_EMAIL);
  await page.locator('input#password').fill(ADMIN_PASSWORD);
  await page.getByRole('button', { name: /sign in/i }).click();
  // The login script honours `?redirect=`, but falls back to the role-aware
  // `default_redirect` the API computed. Either lands a session; which one it
  // is says nothing about the sandbox, so this waits only for "off the login
  // page" and reads the API through `fetch` afterwards.
  await page.waitForURL((url) => !url.pathname.startsWith('/b/auth/login'), {
    timeout: 60_000,
  });
}

/**
 * Boot, sign in, and land on the workspace page with its script running.
 *
 * The last wait is what makes this more than a `goto`. `/b/dev/static/dev.js`
 * is an ES **module** — it imports `BrowserRustCompiler` from
 * `/b/dev/static/compiler-adapter.js` — and a module that fails to parse, or
 * whose import 404s, is dropped by the browser with nothing but a console
 * message: the page would still render, every pane would stay empty, and a
 * spec that only checked for `#dev-workspace` would pass. The progress ladder
 * is drawn by the script's own first status poll, so an element inside it is
 * proof the module loaded, resolved its import and ran.
 */
export async function loginToWorkspace(page: Page) {
  await bootServiceWorker(page);
  await loginAdmin(page);
  await page.goto('/b/dev', { waitUntil: 'commit' });
  await expect(page.locator('#dev-progress-steps li').first()).toBeAttached({ timeout: 60_000 });
}

// ---------------------------------------------------------------------------
// What the workspace publishes, and how the exported bundle is served.
//
// Both halves below are read by more than one spec — `dev-workspace.spec.ts`
// pins them as its own subject, and `dev-scenario.spec.ts` walks the whole of
// design §16 through them — so they live here rather than in whichever file
// happened to need them first. A second copy of a twenty-six name allowlist,
// or of "spawn a static host and wait for the port", is a second thing to keep
// in step with the contract it describes.
// ---------------------------------------------------------------------------

/**
 * The twelve `dev_*` tools `/b/dev/api/tools.json` projects, plus the two
 * `dev.js` registers itself.
 *
 * `dev_read_reference` and `dev_create_block` are Plan 3's — the guest-API
 * reference an agent reads before writing Rust, and the scaffolder that lays
 * a block down from a template. Both are ordinary HTTP tools
 * (`blocks/dev/tools.rs`'s `SELECTIONS`), so both are in `tools.json`.
 *
 * `dev_compile_block` and `dev_export` are not, for opposite reasons:
 * compiling happens in a page worker and never reaches the server as one
 * request, while exporting DOES have an endpoint whose answer is a multi-
 * megabyte zip — a file for the browser to download, not a tool result. Both
 * are page-local, which is why they belong in this list rather than in
 * `tools.json`. `dev_export_manifest` — what an export WOULD contain, as
 * small JSON — is an ordinary HTTP tool and is in the manifest.
 */
export const DEV_TOOLS = [
  'dev_status',
  'dev_list_files',
  'dev_read_file',
  'dev_read_reference',
  'dev_write_file',
  'dev_delete_file',
  'dev_list_generations',
  'dev_get_generation',
  'dev_rollback',
  'dev_create_block',
  'dev_remove_block',
  'dev_export_manifest',
  'dev_compile_block',
  'dev_export',
];

/** The products admin API, projected as the shop-building half of the page. */
export const SHOP_TOOLS = [
  'shop_list_products',
  'shop_create_product',
  'shop_update_product',
  'shop_delete_product',
  'shop_restore_product',
  'shop_list_groups',
  'shop_create_group',
  'shop_list_offers',
  'shop_create_offer',
  'shop_update_offer',
  'shop_publish_offer',
  'shop_archive_offer',
];

/** The two `dev.js` registers locally rather than from the manifest. */
export const PAGE_LOCAL_TOOLS = ['dev_compile_block', 'dev_export'];

/** Everything the `/b/dev` page itself registers, in either half. */
export const PAGE_TOOLS = [...DEV_TOOLS, ...SHOP_TOOLS].sort();

/**
 * What `/b/dev/api/tools.json` publishes — `PAGE_TOOLS` minus the two stubs.
 * `dev.js` logs this count after it registers the manifest, which is how the
 * page reports the size of the surface it was given.
 */
export const MANIFEST_TOOLS = PAGE_TOOLS.filter((name) => !PAGE_LOCAL_TOOLS.includes(name));

/**
 * Serve an unpacked export bundle from `dir` on `port` with Python's
 * `http.server`, and wait until the port is answering *for this bundle*.
 *
 * The exported bundle has to be SERVED to be proved: a service worker cannot
 * be registered from a `file://` URL, which is the first thing the export's
 * own README says. `python3` is already a hard dependency of this repo's
 * tooling (`examples/dev-sandbox/build.sh` uses it), so this needs nothing
 * installed that CI does not already have.
 *
 * The readiness check is a GET of `/seed/manifest.json` compared against the
 * copy the archive was unpacked to, not a TCP connect. A connect proves only
 * that *something* is listening — and something else listening on that port
 * (a leftover server from an interrupted run, another spec) would be served
 * to the browser in place of the bundle under test, which fails as a wrong
 * site rather than as a port collision. Comparing the one file that describes
 * the whole bundle turns that class into an immediate, named error.
 */
export async function serveDirectory(dir: string, port: number): Promise<ChildProcess> {
  const expected = readFileSync(path.join(dir, 'seed', 'manifest.json'), 'utf8');
  const server = spawn('python3', ['-m', 'http.server', String(port), '--bind', '127.0.0.1'], {
    cwd: dir,
    stdio: 'ignore',
  });
  const deadline = Date.now() + 30_000;
  for (;;) {
    if (server.exitCode !== null) {
      throw new Error(`static server for ${dir} exited with ${server.exitCode}`);
    }
    const served = await fetch(`http://127.0.0.1:${port}/seed/manifest.json`)
      .then((response) => (response.ok ? response.text() : null))
      .catch(() => null);
    if (served !== null) {
      if (served !== expected) {
        server.kill('SIGKILL');
        throw new Error(
          `127.0.0.1:${port} is serving a different bundle than ${dir} — something else is ` +
            `already listening on that port`,
        );
      }
      return server;
    }
    if (Date.now() > deadline) throw new Error(`static server for ${dir} never came up`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

/**
 * The ports the exported bundles are served on, beside the sandbox's own.
 *
 * One per spec, not one shared constant. `workers: 1` and
 * `fullyParallel: false` make an overlap unlikely rather than impossible —
 * the `finally` that kills each server is not reached if the process dies —
 * and a port two specs share is a port on which one run's leftovers can be
 * served to the next. Distinct ports plus `serveDirectory`'s bundle check
 * mean neither half of that can go unnoticed.
 */
export const WORKSPACE_EXPORT_PORT = 8098;
export const SCENARIO_EXPORT_PORT = 8099;
