import { test, expect, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { bootServiceWorker, loginAdmin, WELCOME_PHRASE } from './fixtures/dev-sandbox';

/**
 * Plan 1 checkpoint for the browser development sandbox.
 *
 * Everything under `crates/impresspress-web/src/dev_runtime.rs` — wasmi
 * `inspect`/`probe`, the runtime rebuild, `replace_wafer`, the boot context,
 * seed-on-boot — only exists inside a service worker. Nothing in it can be
 * covered by a host test, so this file is the first and only place it runs.
 * The bundle it serves is built by `examples/dev-sandbox/build.sh` (a
 * `browser-devtools` wasm through the sealed web flow with `[dev] enabled =
 * true`, the welcome starter site at `examples/dev-sandbox/seed/` overlaid
 * onto `dist/seed/`).
 *
 * The first test is the anonymous half: **seed-on-boot** (a fresh origin
 * serving `/seed/manifest.json` imports generation 0 and publishes its site),
 * the published site's `no-cache` header, and design §13 — `/b/dev` is
 * admin-only at the router, so an anonymous visitor gets a `401`.
 *
 * The second test is the pipeline, in order:
 *
 *  1. **`/b/dev` exists on a cold boot with no blocks.** `attach` runs before
 *     the first `factory.build`; if it regressed, the page would 404 until
 *     something rebuilt — which on a block-less instance is never.
 *  2. **Writes are hash-checked**, and a `site/**` write publishes.
 *  3. **A refusal is a diagnostic, not a transport error.** The proof guest
 *     names itself `browser/hello`; the sandbox requires `site/hello`.
 *  4. **A real guest, staged over HTTP, is compiled, probed and served.** The
 *     single most valuable assertion in Plan 1.
 *  5. **A refused build does not poison the live runtime.**
 *  6. **A service-worker restart rebuilds the active generation** from
 *     `artifacts/<sha>.wasm` and re-serves both halves.
 *  7. **Rollback removes the block** and leaves the site it did not change.
 *
 * # Two product defects this file found on its first run
 *
 * Both are fixed (Task 10, fix round 1) and both are asserted here the way the
 * design requires, so a regression is a failure rather than a workaround:
 *
 *  * **`/` must serve the published site.** A sandbox bundle force-sets
 *    `WAFER_RUN_SHARED__HAS_LANDING_PAGE` (`impresspress-web/src/config.rs`).
 *    Seeding it could not work: the admin block's `Init` writes the declared
 *    `"false"` default before that hook runs, and "insert if absent" never
 *    beats a row that is already there.
 *  * **Cookie-authenticated mutations must be allowed.** A service worker's
 *    `FetchEvent.request` carries none of `Sec-Fetch-Site`, `Origin`,
 *    `Referer` or `Host`, so `csrf::enforce_origin_policy` used to reach its
 *    fail-closed tail for EVERY mutation in the browser bundle — the whole
 *    admin UI, not just the sandbox. `impresspress-browser`'s
 *    `convert::request_to_message` now synthesizes the two the worker can
 *    prove (`Host` from its own location, `Sec-Fetch-Site` from
 *    `Request::mode` + `Request::referrer`), failing closed on anything else.
 *    Every `postJson` below is that path.
 */

/** The proof guest (`experiments/browser-service-worker-blocks/guest`). */
const GUEST = readFileSync(process.env.PROOF_GUEST_WASM ?? missingGuest());

function missingGuest(): never {
  throw new Error(
    'PROOF_GUEST_WASM is not set. Build the proof guest and point at it:\n' +
      '  cargo build --release --target wasm32-wasip1 \\\n' +
      '    --manifest-path experiments/browser-service-worker-blocks/guest/Cargo.toml\n' +
      '  export PROOF_GUEST_WASM=experiments/browser-service-worker-blocks/guest/target/' +
      'wasm32-wasip1/release/browser_compiled_wafer_block.wasm',
  );
}

/**
 * The guest's own `BlockInfo`, and the same bytes renamed to what the sandbox
 * requires.
 *
 * The name is a plain string in the module's data section and the replacement
 * is byte-length-identical (three trailing spaces inside the JSON object), so
 * the packed `(ptr, len)` the guest returns from `__wafer_info` stays valid and
 * every other offset in the module is untouched. `latin1` is what makes that
 * true for a `String` round trip: it maps bytes 0..255 one-to-one.
 */
const GUEST_AS_TEXT = Buffer.from(GUEST).toString('latin1');
const DECLARED_NAME = '"name":"browser/hello"';
const REQUIRED_NAME = '"name":"site/hello"   ';
if (!GUEST_AS_TEXT.includes(DECLARED_NAME)) {
  throw new Error(`the proof guest does not contain ${DECLARED_NAME}; has its INFO changed?`);
}
if (REQUIRED_NAME.length !== DECLARED_NAME.length) {
  throw new Error('the INFO patch must not change the module length');
}
const GUEST_B64 = Buffer.from(GUEST).toString('base64');
const PATCHED_B64 = Buffer.from(
  GUEST_AS_TEXT.replace(DECLARED_NAME, REQUIRED_NAME),
  'latin1',
).toString('base64');

/** What the proof guest answers on its route. */
const GUEST_GREETING = 'Hello from a browser-compiled WAFER block!';

/**
 * SHA-256 of `seed/site/index.html`, read from `examples/dev-sandbox/seed/
 * manifest.json` at test time rather than pinned here — it is what the
 * *first* write has to send as `expected_sha256`: the seed already published
 * `site/index.html`, so a write claiming to expect nothing there is a
 * conflict, not a create. `build.sh --check` fails the build if the manifest
 * and the file disagree, which is what keeps this read honest.
 */
const seedManifestPath = fileURLToPath(
  new URL('../../../../examples/dev-sandbox/seed/manifest.json', import.meta.url),
);
const seedManifest: { site: { path: string; sha256: string }[] } = JSON.parse(
  readFileSync(seedManifestPath, 'utf8'),
);
const seedIndexHtml = seedManifest.site.find((f) => f.path === 'index.html');
if (!seedIndexHtml) {
  throw new Error(`${seedManifestPath} has no "index.html" entry in "site"`);
}
const SEED_SHA256 = seedIndexHtml.sha256;

/** `GET` a sandbox endpoint as JSON from inside the page. */
function getJson(page: Page, path: string) {
  return page.evaluate(async (p) => (await fetch(p)).json(), path);
}

/** `POST` a JSON body to a sandbox endpoint from inside the page. */
function postJson(page: Page, path: string, body: unknown) {
  return page.evaluate(
    async ([p, b]) =>
      (
        await fetch(p as string, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: b as string,
        })
      ).json(),
    [path, JSON.stringify(body)] as const,
  );
}

/** The same, keeping the status — for the endpoints whose refusal is a `4xx`. */
function postWithStatus(
  page: Page,
  path: string,
  body: unknown,
): Promise<{ status: number; body: any }> {
  return page.evaluate(
    async ([p, b]) => {
      const r = await fetch(p as string, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: b as string,
      });
      return { status: r.status, body: await r.json() };
    },
    [path, JSON.stringify(body)] as const,
  );
}

test('a fresh origin seeds itself, serves the seeded site, and keeps the sandbox private', async ({
  page,
}) => {
  test.setTimeout(300_000);

  // A fresh browser context is a fresh origin: nothing has ever been published
  // here, so `install` fetches `/seed/manifest.json`, imports it as generation
  // 0 and activates it — which publishes its `index.html` into
  // `wafer-run/web/site`. All of that has to have happened before the worker
  // answers its first fetch, because `initialize()` resolves before
  // `handle_request` is ever called.
  const bootStart = Date.now();
  await bootServiceWorker(page);
  console.log(`cold boot (seed import, no blocks): ${Date.now() - bootStart} ms`);

  await page.goto('/', { waitUntil: 'commit' });
  await expect(page.locator('body')).toContainText(WELCOME_PHRASE, { timeout: 60_000 });
  // `no-cache` on the entrypoint — but note this one does NOT discriminate:
  // `wafer-block-web` answers `no-cache` for any `text/html` whatever its
  // `cache_mode`. The assertion that the sandbox's
  // `cache_mode: "no-cache"` really reached the block is on a *stylesheet*,
  // in the test below, because a `.css` is `public, max-age=3600` without it.
  const siteCache = await page.evaluate(
    async () => (await fetch('/')).headers.get('cache-control'),
  );
  expect(siteCache).toBe('no-cache');

  // Design §13's other half: `/b/dev` is registered as an `Admin` extra route,
  // and that router registration — not a check in any handler — is the whole
  // gate. A `fetch` sends `Accept: */*`, so an anonymous caller gets the JSON
  // `401` rather than the browser redirect to the login page.
  const sandbox = await page.evaluate(async () => (await fetch('/b/dev/api/status')).status);
  expect(sandbox).toBe(401);

  // The CSP the sandbox relaxes, and the other half of `smoke.spec.ts`'s
  // "the default bundle has no dev block": that one asserts the unrelaxed
  // header on a feature-off bundle, this asserts the relaxation is really
  // there when the sandbox is on. Neither means anything without the other —
  // a constant satisfies either alone, which is exactly what happened before
  // the fix round that added these: `flows::register_site_main` replaced the
  // factory's whole `wafer-run/security-headers` config with the shared
  // directives, so the two bundles served a byte-identical policy and the
  // feature-off assertion passed for the wrong reason.
  const csp = await page.evaluate(
    async () => (await fetch('/b/auth/login')).headers.get('content-security-policy'),
  );
  // The `/b/dev` page's compiler worker is a module worker that spawns
  // blob-URL subordinates; Plan 2 cannot start it without this.
  expect(csp, `CSP was: ${csp}`).toContain("worker-src 'self' blob:");
  // Its live-site preview is a same-origin iframe.
  expect(csp, `CSP was: ${csp}`).toContain("frame-ancestors 'self'");
});

test('the dev sandbox stages a guest, survives a restart and rolls back', async ({ page }) => {
  // A cold boot compiles the wasm, creates the OPFS database, runs every
  // block's migrations and imports the seed; the restart below does most of
  // it a second time. The default 60 s is a boot budget, not a test budget.
  test.setTimeout(600_000);

  // --- 1. Seed on boot --------------------------------------------------
  await bootServiceWorker(page);

  // --- 2. The control plane answers on a block-less instance ------------
  await loginAdmin(page);

  const status = await getJson(page, '/b/dev/api/status');
  expect(status.active_generation, JSON.stringify(status)).not.toBeNull();
  expect(status.active_generation.cause).toBe('seed');
  expect(status.active_generation.status).toBe('active');
  expect(status.blocks).toEqual([]);
  // Nothing rebuilt the runtime yet: the seed carries no blocks, so the
  // block set never changed and `install` skipped its own rebuild.
  expect(status.runtime_generation).toBe(0);

  // Design §12: every `/b/dev` response is `no-store`, so the page's progress
  // polling can never be answered from a cache.
  const statusCache = await page.evaluate(
    async () => (await fetch('/b/dev/api/status')).headers.get('cache-control'),
  );
  expect(statusCache).toBe('no-store');

  // --- 3. A site write publishes ----------------------------------------
  // `expected_sha256: null` means "I expect no file here", and the seed put
  // one there, so this is a 409 carrying the hash the file actually has —
  // which is also how the caller learns what to send next.
  const conflict = await postWithStatus(page, '/b/dev/api/files/write', {
    path: 'site/index.html',
    content: '<h1>sandbox v1</h1>',
    expected_sha256: null,
  });
  expect(conflict.status).toBe(409);
  expect(conflict.body.current_sha256).toBe(SEED_SHA256);

  const written = await postJson(page, '/b/dev/api/files/write', {
    path: 'site/index.html',
    content: '<h1>sandbox v1</h1>',
    expected_sha256: SEED_SHA256,
  });
  expect(written.generation, JSON.stringify(written)).not.toBeNull();
  expect(written.generation.cause).toBe('site_write');
  expect(written.generation.status).toBe('active');

  await page.goto('/', { waitUntil: 'commit' });
  await expect(page.locator('h1')).toHaveText('sandbox v1', { timeout: 60_000 });

  // A sandbox republishes the site under the same URLs on every keystroke, so
  // a cached asset shows the previous generation. `runtime_factory.rs`
  // declares `cache_mode: "no-cache"` on `wafer-run/web` for exactly that,
  // and this is the assertion that it survives the flow's own config for the
  // same block: a stylesheet is `public, max-age=3600` under the default
  // mode and `no-cache` only under this one. (The entrypoint cannot show it —
  // `wafer-block-web` never caches `text/html`.)
  const stylesheet = await postJson(page, '/b/dev/api/files/write', {
    path: 'site/style.css',
    content: 'body { color: rebeccapurple }',
    expected_sha256: null,
  });
  expect(stylesheet.generation, JSON.stringify(stylesheet)).not.toBeNull();
  const assetCache = await page.evaluate(
    async () => (await fetch('/style.css')).headers.get('cache-control'),
  );
  expect(assetCache).toBe('no-cache');

  // --- 4. The proof guest's own name is refused, as a diagnostic --------
  const refused = await postJson(page, '/b/dev/api/builds/stage', {
    block_name: 'hello',
    artifact_base64: GUEST_B64,
    compiler_version: 'proof',
    diagnostics: [],
  });
  expect(refused.success, JSON.stringify(refused.diagnostics)).toBe(false);
  expect(refused.diagnostics.map((d: { code: string }) => d.code)).toContain('name-mismatch');
  expect(refused.generation).toBeNull();

  // --- 5. The renamed guest is compiled, probed and served --------------
  const stageStart = Date.now();
  const staged = await postJson(page, '/b/dev/api/builds/stage', {
    block_name: 'hello',
    artifact_base64: PATCHED_B64,
    compiler_version: 'proof',
    diagnostics: [],
  });
  expect(staged.success, JSON.stringify(staged.diagnostics)).toBe(true);
  expect(staged.generation.cause).toBe('block_compile');
  expect(staged.generation.status).toBe('active');
  expect(staged.generation.blocks).toBe(1);
  const stageMs = Date.now() - stageStart;
  console.log(`stage → validate → probe → rebuild → serve: ${stageMs} ms`);

  const hello = await page.evaluate(async () => (await fetch('/b/hello/')).text());
  expect(hello).toContain(GUEST_GREETING);

  const afterStage = await getJson(page, '/b/dev/api/status');
  // The only externally visible proof `rebuild` ran to completion.
  expect(afterStage.runtime_generation).toBe(status.runtime_generation + 1);
  expect(afterStage.blocks).toHaveLength(1);
  expect(afterStage.blocks[0].name).toBe('site/hello');
  expect(afterStage.blocks[0].routes).toEqual([{ prefix: '/b/hello/', access: 'Public' }]);

  // The site the block compile did not touch is still the one that is live.
  await page.goto('/', { waitUntil: 'commit' });
  await expect(page.locator('h1')).toHaveText('sandbox v1', { timeout: 60_000 });

  // --- 6. A refused build leaves the live runtime alone -----------------
  const notWasm = await postJson(page, '/b/dev/api/builds/stage', {
    block_name: 'broken',
    artifact_base64: Buffer.from('this is not a wasm module').toString('base64'),
    compiler_version: 'proof',
    diagnostics: [],
  });
  expect(notWasm.success, JSON.stringify(notWasm.diagnostics)).toBe(false);
  expect(notWasm.diagnostics.map((d: { code: string }) => d.code)).toContain('guest-load');
  const stillThere = await page.evaluate(async () => (await fetch('/b/hello/')).text());
  expect(stillThere).toContain(GUEST_GREETING);
  const afterRefusal = await getJson(page, '/b/dev/api/status');
  expect(afterRefusal.runtime_generation).toBe(afterStage.runtime_generation);
  expect(afterRefusal.blocks).toHaveLength(1);

  // --- 7. A service-worker restart rebuilds from the ledger -------------
  // Unregistering drops the worker AND its in-memory `Rc<Wafer>`; the next
  // load registers a fresh one, which finds the instance no longer fresh
  // (so no re-seed), converges on the journal and rebuilds the active
  // generation's blocks from `artifacts/<sha>.wasm`.
  const restartStart = Date.now();
  await page.evaluate(async () => {
    for (const r of await navigator.serviceWorker.getRegistrations()) await r.unregister();
  });
  await bootServiceWorker(page);
  const restartMs = Date.now() - restartStart;
  console.log(`service-worker restart (rebuild with one block): ${restartMs} ms`);
  await page.goto('/', { waitUntil: 'commit' });
  await expect(page.locator('h1')).toHaveText('sandbox v1', { timeout: 60_000 });

  const helloAgain = await page.evaluate(async () => (await fetch('/b/hello/')).text());
  expect(helloAgain).toContain(GUEST_GREETING);

  const afterRestart = await getJson(page, '/b/dev/api/status');
  // A new worker means a new `BrowserRuntimeControl`, whose counter starts at
  // zero. Exactly one rebuild happened on the way up — the one `install` runs
  // because the ledger's block set is not the empty set the cold-start runtime
  // was built with.
  expect(afterRestart.runtime_generation).toBe(1);
  expect(afterRestart.blocks).toHaveLength(1);
  expect(afterRestart.blocks[0].name).toBe('site/hello');
  expect(afterRestart.blocks[0].artifact_sha256).toBe(afterStage.blocks[0].artifact_sha256);

  // --- 8. Rollback to the generation before the block -------------------
  const listed = await getJson(page, '/b/dev/api/generations');
  const causes = listed.generations.map((g: { cause: string }) => g.cause);
  // Newest first: the block compile, the stylesheet write, the index write,
  // the seed.
  expect(causes).toEqual(['block_compile', 'site_write', 'site_write', 'seed']);
  // `find` takes the newest `site_write` — the stylesheet one, which is the
  // generation immediately before the block and carries every seed site file
  // plus the `site/style.css` this test added (step 3's index write did not
  // grow the count — it overwrote a file the seed already published).
  const beforeTheBlock = listed.generations.find(
    (g: { cause: string }) => g.cause === 'site_write',
  );
  expect(beforeTheBlock.site_files).toBe(seedManifest.site.length + 1);
  const rolledBack = await postJson(
    page,
    `/b/dev/api/generations/${beforeTheBlock.id}/rollback`,
    {},
  );
  expect(rolledBack.generation, JSON.stringify(rolledBack)).toBeTruthy();
  expect(rolledBack.generation.cause).toBe('rollback');
  expect(rolledBack.generation.blocks).toBe(0);

  const gone = await page.evaluate(async () => (await fetch('/b/hello/')).status);
  expect(gone).toBe(404);

  // The rollback target's site is republished, so the block left and the page
  // it never touched stayed.
  await page.goto('/', { waitUntil: 'commit' });
  await expect(page.locator('h1')).toHaveText('sandbox v1', { timeout: 60_000 });

  const afterRollback = await getJson(page, '/b/dev/api/status');
  expect(afterRollback.blocks).toEqual([]);
  expect(afterRollback.runtime_generation).toBe(afterRestart.runtime_generation + 1);
});
