import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { test, expect, type BrowserContext, type Page } from '@playwright/test';
import { MODEL_CONTEXT_POLYFILL } from './fixtures/model-context-polyfill';

/**
 * Lightweight smoke test that doesn't rebuild mid-test. Catches regressions
 * like the `/manifest.json` bypass bug and the `/sql-wasm-esm.js` import
 * path bug (both of which silently prevented SW registration).
 */
test('service worker registers and controls the page', async ({ page }) => {
  // `commit` is the right waitUntil here: this test exercises SW registration,
  // which `loader.js` triggers as soon as it parses (registration is async on
  // top of that). The downstream `waitForFunction(() => navigator.serviceWorker
  // .controller)` provides the actual assertion timing. Default `load` blocks
  // on every subresource; even `domcontentloaded` is delayed by deferred and
  // module scripts. Neither fires reliably here because the loader page imports
  // `/webllm-engine.js` and `/embed-engine.js` (type="module"), and a slow
  // jsdelivr CDN response for either one used to push the goto past the 60s
  // test timeout. Lazy-loading the WebLLM ESM (see webllm-engine.js) removed
  // most of the slowness, but `commit` is still the semantically correct
  // waitUntil for an SW-registration smoke and survives future regressions.
  await page.goto('/', { waitUntil: 'commit' });
  // Read the controller scriptURL inside the waitForFunction predicate so the
  // value is captured atomically. impresspress-web's loader.js redirects to
  // `boot_redirect` as soon as the SW takes control, which would otherwise
  // destroy the execution context between a separate `waitForFunction` +
  // `evaluate` pair.
  const handle = await page.waitForFunction(
    () => navigator.serviceWorker.controller?.scriptURL ?? null,
    null,
    { timeout: 20_000 },
  );
  const controllerURL = (await handle.jsonValue()) as string | null;
  expect(controllerURL).toMatch(/\/sw\.js$/);
});

test('boot redirect lands on the auth login page', async ({ page }) => {
  // boot_redirect is "/" (intercepted by SW → wasm router → 302 →
  // /b/auth/login for anonymous visitors). loader.js sets
  // `window.location.href = boot_redirect` once the SW takes control;
  // waiting for the resulting URL match avoids the
  // `net::ERR_ABORTED; maybe frame was detached?` race that an explicit
  // second goto would hit.
  //
  // Asserting on the rendered Sign In form rather than a non-empty body
  // catches the regression where boot_redirect pointed at /b/system/ —
  // an unclaimed path that returned a non-empty 404 page and silently
  // passed the smoke.
  await page.goto('/', { waitUntil: 'commit' });
  await page.waitForURL(/\/b\/auth\/login/, { timeout: 30_000 });
  await expect(page.locator('input#email')).toBeVisible();
  await expect(page.locator('input#password')).toBeVisible();
});

test('admin can log in and reach the dashboard', async ({ page }) => {
  // Regression guard for the browser-only JWT bug: the pipeline verified
  // access tokens against a secret snapshotted at build time. In the browser
  // target `WAFER_RUN__AUTH__JWT_SECRET` is auto-generated AFTER the runtime
  // is built, so that snapshot was the empty string while login signed with
  // the real seeded secret. Login returned a token, but every authenticated
  // request then 403'd and the user was bounced back to /b/auth/login.
  //
  // This is the only browser-WASM test that exercises a *protected* route
  // post-login — the anonymous boot smoke above can't catch a verify bug.
  await page.goto('/', { waitUntil: 'commit' });
  await page.waitForURL(/\/b\/auth\/login/, { timeout: 30_000 });

  await page.locator('input#email').fill('admin@example.com');
  await page.locator('input#password').fill('admin123');
  await page.getByRole('button', { name: /sign in/i }).click();

  // A usable session lands on the admin dashboard; the regression instead
  // redirected back to /b/auth/login?redirect=%2Fb%2Fadmin%2F.
  await page.waitForURL(/\/b\/admin\//, { timeout: 30_000 });
  await expect(page).toHaveURL(/\/b\/admin\//);
});

test('the default bundle has no dev block', async ({ page }) => {
  // The sandbox's security model (design §13) is that `impresspress/dev` is
  // ABSENT from a normal deployment, not merely disabled in one. `pkg/` is
  // built without `browser-devtools` and without `[dev] enabled`, so both
  // halves of that must hold: no route, and no relaxed header for the
  // compiler worker / preview iframe the sandbox would need.
  //
  // Wait for the SW-served boot redirect to land rather than for
  // `navigator.serviceWorker.controller`: loader.js navigates to
  // `boot_redirect` the moment the SW takes control, which destroys the
  // execution context out from under an `evaluate()` that raced it. Landing
  // on /b/auth/login means the wasm router has already answered a request,
  // which is the precondition both fetches below need anyway.
  await page.goto('/', { waitUntil: 'commit' });
  await page.waitForURL(/\/b\/auth\/login/, { timeout: 30_000 });

  const status = await page.evaluate(async () => (await fetch('/b/dev/api/status')).status);
  expect(status).toBe(404);

  const csp = await page.evaluate(
    async () => (await fetch('/b/auth/login')).headers.get('content-security-policy'),
  );
  expect(csp).not.toBeNull();
  // `worker-src` exists ONLY when the sandbox is active — nothing else in the
  // bundle spawns a worker.
  expect(csp).not.toContain('worker-src');
  // The sandbox previews the live site in a same-origin iframe and relaxes
  // this to `'self'` (`runtime_factory.rs`); a normal deployment refuses
  // framing outright.
  expect(csp).toContain("frame-ancestors 'none'");
  // Deliberately NOT `expect(csp).not.toContain('frame-src')`, which is what
  // this line used to say. The products block declares
  // `frame-src https://js.stripe.com https://hooks.stripe.com
  // https://checkout.stripe.com` for Stripe Checkout, so a feature-OFF bundle
  // has a `frame-src` too — and this assertion failed the first time it was
  // run against a real built `pkg/` (Plan 1 Task 10's fix round).
  //
  // Both lines above really do discriminate: `dev-foundations.spec.ts`
  // asserts the opposite of each on a `browser-devtools` + `[dev] enabled`
  // bundle. They did not until `flows::register_site_main` stopped replacing
  // the factory's whole `wafer-run/security-headers` config with the shared
  // CSP directives — until then both bundles served the same policy and this
  // test passed for the wrong reason.
});

test('the default bundle serves the products catalog', async ({ page }) => {
  // Regression guard for the browser target's config source (`bc1e8070`).
  //
  // `run_init_pipeline` resolves a block's DECLARED `ConfigVar`s through the
  // runtime's `ConfigSource` BEFORE it calls that block's `lifecycle(Init)`,
  // and a required key it cannot resolve is `InitError::Permanent` — the
  // block is then dead for the life of the runtime, however well its handlers
  // would have coped with the value being absent. The browser built every
  // runtime with a permanently EMPTY `StaticConfigSource`, so
  // `impresspress/products` — which declares
  // `IMPRESSPRESS__PRODUCTS__WEBHOOK_SECRET` (auto-generated, empty default,
  // not optional) — answered
  // `412 FailedPrecondition: block \`impresspress/products\` init failed
  // permanently` on EVERY products route, in every browser bundle, while
  // `/b/webmcp/manifest.json` went on advertising `list_products`,
  // `get_product`, `preview_price` and `start_checkout` to any agent that
  // asked. `impresspress-web` now hands its runtimes a `SharedConfigSource`
  // that the post-admin-init boot hook fills with the seeded variables.
  //
  // This lives in the SMOKE spec, not the sandbox one, precisely because the
  // defect was never sandbox-specific: `dev-workspace.spec.ts` covers the
  // `browser-devtools` bundle, and this covers the ordinary `pkg/` one that
  // every other browser deployment ships. Nothing else asserted the default
  // bundle can answer a products route at all.
  //
  // The catalog is the right route to ask for: `Public` (so no login), a pure
  // read, and empty on a fresh instance — so this asserts the block INITED,
  // not that anything seeded it.
  await page.goto('/', { waitUntil: 'commit' });
  await page.waitForURL(/\/b\/auth\/login/, { timeout: 30_000 });

  const catalog = await page.evaluate(async () => {
    const response = await fetch('/b/products/catalog');
    return { status: response.status, body: await response.text() };
  });
  expect(catalog.status, catalog.body).toBe(200);
  // `records` is the list field `CatalogProductListResponse` publishes
  // (`products/contracts.rs`) — parsed rather than string-matched so a body
  // that is not JSON at all fails here rather than passing on a substring.
  const page1 = JSON.parse(catalog.body) as { records: unknown[]; total_count: number };
  expect(Array.isArray(page1.records), catalog.body).toBe(true);
  expect(page1.total_count).toBe(0);
});

test('a cold visitor gets WebMCP tools without a reload', async ({ page }) => {
  // The race this guards: `webmcp.js`'s deferred script runs while
  // `navigator.serviceWorker.controller` is still null (the SW hasn't taken
  // control of THIS document yet) — a cold visit to a production host with
  // SPA-fallback routing lands here (see
  // `docs/2026-08-28-browser-demo-design-note.md` §3). It is not
  // reproducible by simply `goto`-ing a deep link on this harness:
  // `python3 -m http.server -d pkg` (what this spec and CI's e2e-smoke job
  // both serve `pkg/` with) has no SPA fallback, so a direct `goto` at
  // `/b/auth/login` 404s before any script runs. And reaching the real page
  // the only way this server allows — through `/`, like every other test
  // above — proves nothing either: `impresspress-bundle`'s `loader.js`
  // already reloads the tab once on first registration, and by the time
  // that reload's navigation lands on `/b/auth/login` the SW is already
  // active and controlling, so `controller` is non-null before `webmcp.js`
  // ever runs (verified empirically — this harness's redirect chain closes
  // the race on its own, fix or no fix).
  //
  // So: reach the real page normally, then shadow `navigator.serviceWorker`
  // on it ONLY (the loader shell at `/` is untouched, so registration still
  // happens for real) to force exactly the state the fix handles, and prove
  // `webmcp.js` waits for it instead of registering nothing.
  //
  // The state being forced is UNCONTROLLED, not "no worker yet": `controller`
  // reads null and no `controllerchange` has fired. That distinction is the
  // fix. `navigator.serviceWorker.ready` — which this stub deliberately does
  // not expose — resolves on `registration.active`, and `active` is populated
  // at the *activating* state while `sw.js.tmpl` calls `clients.claim()`
  // inside `activate`'s `waitUntil`. So `ready` can resolve in exactly the
  // window this test holds open, and a manifest fetch issued there goes to
  // the network: on a host with SPA fallback that answers `index.html` with
  // 200, `r.json()` throws, webmcp.js's `.catch` swallows it, and the page
  // ends up with no tools at all and no error. `controller` is the signal
  // that actually means "my fetches reach the worker".
  await page.addInitScript(MODEL_CONTEXT_POLYFILL);
  await page.addInitScript(() => {
    if (location.pathname !== '/b/auth/login') return;
    const real = navigator.serviceWorker;
    let controlled = false;
    const listeners = new Set<() => void>();
    (window as unknown as { __releaseWebmcpControl: () => void }).__releaseWebmcpControl = () => {
      controlled = true;
      listeners.forEach((l) => l());
    };
    Object.defineProperty(navigator, 'serviceWorker', {
      configurable: true,
      get() {
        return {
          // Null until this document is claimed — the real worker is
          // controlling underneath, which is what makes the fetch after
          // release actually reach the wasm router.
          get controller() { return controlled ? real.controller : null; },
          addEventListener(type: string, listener: () => void) {
            if (type === 'controllerchange') listeners.add(listener);
          },
          removeEventListener(type: string, listener: () => void) {
            if (type === 'controllerchange') listeners.delete(listener);
          },
          getRegistration: real.getRegistration.bind(real),
        };
      },
    });
  });
  await page.goto('/', { waitUntil: 'commit' });
  await page.waitForURL(/\/b\/auth\/login/, { timeout: 30_000 });

  // While the page is uncontrolled, webmcp.js must not have registered
  // anything — this is the assertion that fails (immediately, not a
  // timeout) against a script that fetches the manifest unconditionally, and
  // against one that settles for `.ready`: this stub has no `ready` at all,
  // so reading it would throw straight into the `.then(load, load)` fallback
  // and register the network's answer.
  await page.waitForTimeout(500);
  expect(await page.evaluate(() => document.modelContext.__tools().length)).toBe(0);

  // The claim landing (`controllerchange`) lets it proceed — without a
  // reload, and without the test navigating again.
  await page.evaluate(() => (window as unknown as { __releaseWebmcpControl: () => void }).__releaseWebmcpControl());
  const names = await page.waitForFunction(() => {
    const t = document.modelContext.__tools();
    return t.length > 0 ? t.map((x) => x.name) : null;
  }, null, { timeout: 10_000 });
  expect(await names.jsonValue()).toContain('list_products');
});

// ── Migrations across service-worker restarts ───────────────────────────────
//
// The vendored sql.js the worker runs, loaded into the PAGE so a test can edit
// the OPFS database while the worker is stopped. Inlined rather than fetched:
// the page is controlled, so any fetch would start the worker again, and a
// running worker flushes its own copy over the edit. The ESM build's one
// `export default` becomes a global so it can go in as a classic script.
const VENDOR = new URL('../../../impresspress-bundle/assets/vendor/', import.meta.url);
const SQL_JS_SOURCE = readFileSync(fileURLToPath(new URL('sql-wasm-esm.js', VENDOR)), 'utf8')
  .replace(/export default _sqlJs;\s*$/, 'globalThis.__initSqlJs = _sqlJs;');
const SQL_WASM_BASE64 = readFileSync(fileURLToPath(new URL('sql-wasm.wasm', VENDOR))).toString('base64');

const ADMIN = 'impresspress/admin';
// Created by admin's 001 with `IF NOT EXISTS`, so re-applying the set brings
// it back, and nothing reads it: dropping it is a harmless marker.
const MARKER_INDEX = 'impresspress__admin__request_logs_created_at_idx';

/** Worker-console lines the test reads: runtime starts, and SQLite's
 * duplicate-column refusals — which only a migration re-run produces. */
function watchWorker(context: BrowserContext) {
  const seen = { starts: 0, duplicateColumns: [] as string[] };
  context.on('console', (msg) => {
    if (msg.page() !== null) return;
    const text = msg.text();
    if (text.includes('impresspress: WAFER runtime started')) seen.starts += 1;
    if (/duplicate column name/i.test(text)) seen.duplicateColumns.push(text);
  });
  return seen;
}

/** Stop every service worker, as a browser does with an idle one. */
async function stopWorkers(page: Page) {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('ServiceWorker.enable');
  await cdp.send('ServiceWorker.stopAllWorkers');
  await cdp.detach();
}

/** Reload, and wait until a NEW worker has booted a runtime and served the
 * login page — the start count is what proves the reload did not land on a
 * worker that survived `stopWorkers`. */
async function bootAgain(page: Page, seen: { starts: number }) {
  const before = seen.starts;
  await page.reload({ waitUntil: 'commit' });
  await expect(page.locator('input#email')).toBeVisible({ timeout: 30_000 });
  await expect.poll(() => seen.starts, { timeout: 30_000 }).toBe(before + 1);
}

/** Run `statements` against the OPFS database, in the page, while no worker
 * is running; answer admin's recorded migration hash and whether the marker
 * index exists. */
async function onStoredDatabase(page: Page, statements: string[]) {
  await page.addScriptTag({ content: SQL_JS_SOURCE });
  return page.evaluate(
    async ({ wasm, statements, admin, marker }) => {
      const bytes = Uint8Array.from(atob(wasm), (c) => c.charCodeAt(0));
      const init = (globalThis as unknown as { __initSqlJs: (c: object) => Promise<any> }).__initSqlJs;
      const SQL = await init({ wasmBinary: bytes });
      const root = await navigator.storage.getDirectory();
      const handle = await root.getFileHandle('impresspress.db');
      const db = new SQL.Database(new Uint8Array(await (await handle.getFile()).arrayBuffer()));
      for (const sql of statements) db.run(sql);
      if (statements.length > 0) {
        const writable = await handle.createWritable();
        await writable.write(db.export());
        await writable.close();
      }
      const hash = db.exec(
        'SELECT current_hash FROM impresspress__admin__block_settings WHERE block_name = ?',
        [admin],
      )[0]?.values[0]?.[0] as string | undefined;
      const index = db.exec("SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ?", [marker]);
      db.close();
      return { hash, markerIndex: index.length > 0 };
    },
    { wasm: SQL_WASM_BASE64, statements, admin: ADMIN, marker: MARKER_INDEX },
  );
}

test('a changed migration applies once, and a restart re-runs nothing', async ({ page, context }) => {
  // A browser install's migrations run inside every block's `lifecycle(Init)`,
  // gated on the hash recorded in `block_settings`. Two ways that gate went
  // wrong, one per half of this test:
  //
  // - The runtime used to be built before that state was read, so every boot
  //   looked like a fresh install and re-ran every block's whole set — the
  //   "duplicate column name" warnings in the worker console (admin's ADD
  //   COLUMNs), seen live on the dev sandbox.
  // - Read the state but give no consent, and the gate treats a changed hash
  //   as drift an operator must bless — which nobody can do in a browser, so
  //   an install created by an older bundle would keep its schema forever.
  //
  // A "bundle with a new migration" is staged by rewinding what the database
  // records instead: admin's hash set to one this bundle does not have, as an
  // older bundle would have left it, and one of admin's indexes dropped so
  // that re-applying the set is visible in the schema itself.
  const seen = watchWorker(context);

  // Boot 1: a fresh profile.
  await page.goto('/', { waitUntil: 'commit' });
  await page.waitForURL(/\/b\/auth\/login/, { timeout: 30_000 });
  await expect(page.locator('input#email')).toBeVisible();
  await expect.poll(() => seen.starts, { timeout: 30_000 }).toBe(1);

  await stopWorkers(page);
  const fresh = await onStoredDatabase(page, [
    `UPDATE impresspress__admin__block_settings SET current_hash = 'older-bundle', ` +
      `blessed_hash = 'older-bundle' WHERE block_name = '${ADMIN}'`,
    `DROP INDEX ${MARKER_INDEX}`,
  ]);
  expect(fresh).toEqual({ hash: 'older-bundle', markerIndex: false });

  // Boot 2: the changed set applies, and is recorded as applied.
  await bootAgain(page, seen);
  await stopWorkers(page);
  const upgraded = await onStoredDatabase(page, []);
  expect(upgraded.markerIndex, 'admin\'s changed migrations were not applied').toBe(true);
  expect(upgraded.hash).toMatch(/^[0-9a-f]{64}$/);
  // Re-applying admin's set re-runs its ADD COLUMNs, so this boot warned.
  expect(seen.duplicateColumns.length).toBeGreaterThan(0);

  // Boot 3: nothing changed, so nothing re-runs.
  seen.duplicateColumns.length = 0;
  await bootAgain(page, seen);
  await stopWorkers(page);
  expect(await onStoredDatabase(page, [])).toEqual(upgraded);
  expect(seen.duplicateColumns).toEqual([]);
});
