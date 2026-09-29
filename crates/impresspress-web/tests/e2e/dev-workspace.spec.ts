import { test, expect, type Page } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  ADMIN_EMAIL,
  ADMIN_PASSWORD,
  bootServiceWorker,
  loginToWorkspace,
  MANIFEST_TOOLS,
  PAGE_TOOLS,
  serveDirectory,
  WELCOME_HEADING,
  WELCOME_PHRASE,
  WORKSPACE_EXPORT_PORT,
} from './fixtures/dev-sandbox';
import { MODEL_CONTEXT_POLYFILL } from './fixtures/model-context-polyfill';
import { SHOP_HEADING, SHOP_OFFER, SHOP_PRODUCT, shopPage } from './fixtures/shop-fixture';
import { execute, registeredTools, structured, waitForTool } from './fixtures/webmcp-helpers';

/**
 * Plan 2 checkpoint for the browser development sandbox: **an agent builds
 * the site and stocks the shop; a shopper browses it.**
 *
 * `dev-foundations.spec.ts` (Plan 1) proves the machinery underneath — seed
 * on boot, hash-checked writes, compile/probe/rebuild, rollback — by calling
 * `/b/dev/api/*` directly with `fetch`. This file is the layer above it: the
 * `/b/dev` page, the tools it registers, and the two audiences that meet on
 * one origin. Nothing here calls the sandbox API directly; every mutation
 * goes through a registered WebMCP tool, because "the agent could do it" is
 * the claim under test and a `fetch` would prove only that the endpoint
 * works.
 *
 * The polyfill (`model-context-polyfill.ts`) stands in for a WebMCP-capable
 * browser — Chromium has no `document.modelContext` — and it is the ONLY
 * substitution. `/b/dev/api/tools.json`, `dev.js`'s registration, the request
 * `webmcp-core.js` builds from each tool's `invocation`, the handlers that
 * answer it, the generation the write publishes and the page the shopper is
 * served are all the real things, running inside a service worker.
 *
 * Three tests, three separate claims:
 *
 *  1. **The end-to-end scenario** (design §16 scenarios 1, 3, 4, 5): sign in
 *     from the welcome page, get the page-scoped tool set, rewrite the site,
 *     create → price → publish → activate a product, and have an anonymous
 *     shopper on the same origin see it.
 *  2. **Cross-origin isolation** (§20.3, spec amendment 14): `/b/dev` is
 *     `crossOriginIsolated` for the in-browser compiler's `SharedArrayBuffer`,
 *     and — the load-bearing half — the site carries the same COEP so a COEP
 *     document can actually frame it. A blank preview iframe is what the
 *     `require-corp` posture produced, and no header assertion alone catches
 *     it: only rendering the frame does.
 *  3. **The editor's binary guard**: the one data-loss path the editor pane
 *     has. `blocks/dev/page.rs` pins both halves of the guard as source
 *     assertions and says outright that the behaviour itself can only be
 *     driven from here.
 *
 * Every test starts from a fresh Playwright context, which is a fresh origin:
 * its own OPFS database, its own service-worker registration, its own seed
 * import. So each pays a cold boot, and each is independent of the others.
 */

/** A 1×1 transparent PNG — the smallest honestly-binary file. 67 bytes. */
const PIXEL_PNG_BASE64 =
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGMAAQAABQABDQottAAAAABJRU5ErkJggg==';
const PIXEL_PNG_BYTES = 67;
const PIXEL_PNG_PATH = 'site/pixel.png';

/**
 * Sign in the way a first-time visitor does: from the welcome page's own
 * "Open workspace" link.
 *
 * Not `loginToWorkspace` (`fixtures/dev-sandbox.ts`), and not `goto('/b/dev')`
 * — either would skip the one navigation this deployment actually ships to get
 * a human into the workspace, which is a link the seed site carries and a
 * `?redirect=` the login page has to honour. Both are part of Plan 2's
 * surface, so both are walked here; the shared helper is for the specs whose
 * subject is what happens AFTER the workspace is open.
 */
async function openWorkspace(page: Page) {
  await expect(page.locator('body')).toContainText(WELCOME_PHRASE, { timeout: 60_000 });
  // The credentials are printed on the page for whoever lands here; that they
  // are is part of the starter site's contract, so the test reads them the
  // same way a visitor would rather than assuming them silently.
  await expect(page.locator('body')).toContainText(ADMIN_EMAIL);
  await page.getByRole('link', { name: /open workspace/i }).click();
  await page.locator('input#email').fill(ADMIN_EMAIL);
  await page.locator('input#password').fill(ADMIN_PASSWORD);
  await page.getByRole('button', { name: /sign in/i }).click();
  // `/b/auth/login?redirect=/b/dev` renders the target into the hidden
  // `#redirect` field and the login script prefers it over the role-aware
  // `default_redirect` — so landing anywhere else is the link being broken,
  // not a detail to paper over.
  await page.waitForURL((url) => url.pathname === '/b/dev', { timeout: 60_000 });
}

/** `contracts::ExportManifest`, trimmed to what this spec reads. */
type ExportManifest = {
  generation_id: string;
  files: { path: string; bytes: number }[];
  total_bytes: number;
  shell_files: number;
  site_files: number;
  blocks: number;
  tables: Record<string, number>;
};

type FileRead = { path: string; sha256: string; size: number; encoding: string; content: string };
type FileWrite = { path: string; sha256: string; size: number; generation: Generation | null };
type Generation = { id: string; cause: string; status: string };

test('an agent builds the shop on /b/dev and a shopper sees it at /', async ({
  page,
  browser,
}) => {
  // A cold boot compiles the wasm, creates the OPFS database, runs every
  // block's migrations and imports the seed. The default 60 s is a boot
  // budget, not a test budget.
  test.setTimeout(300_000);

  // Install the WebMCP shim before ANY navigation: `dev.js` and `webmcp.js`
  // both check `document.modelContext` as they run, and a polyfill added
  // afterwards would be a page with no agent on it.
  await page.addInitScript(MODEL_CONTEXT_POLYFILL);

  const bootStart = Date.now();
  await bootServiceWorker(page);
  console.log(`cold boot (seed import, no blocks): ${Date.now() - bootStart} ms`);

  await openWorkspace(page);

  // --- 1. The page registers its own tools, and only its own -------------
  //
  // Two registrars share `document.modelContext` here: `dev.js` adds the
  // page-scoped allowlist, `webmcp.js` adds the deployment-wide manifest for
  // the caller's tier, and they finish in whichever order their two fetches
  // complete. So both are waited for before anything is read, each by a name
  // it alone publishes: `list_products` for `webmcp.js`, and `dev_export`
  // for `dev.js` — the last tool `registerPageLocal` adds, after
  // `registerFromManifest` has finished with everything `tools.json`
  // returned. Waiting on names rather than a total (`PAGE_TOOLS.length`) is
  // what keeps this from pinning the *other* file's contract — the admin
  // manifest's size is `webmcp.spec.ts`'s subject, not this one's — and from
  // going silently racy if that manifest ever grows to `PAGE_TOOLS.length`
  // tools at the admin tier, at which point `waitForFunction`'s count could
  // be satisfied by `webmcp.js` alone, before `dev.js` had registered
  // anything.
  await waitForTool(page, 'list_products');
  await waitForTool(page, 'dev_export');
  const tools = (await registeredTools(page, 1)).map((t) => t.name);

  expect(tools.filter((n) => n.startsWith('dev_') || n.startsWith('shop_')).sort()).toEqual(
    PAGE_TOOLS,
  );
  // The site's own tools are still here: `dev.js`'s registrations are added
  // ALONGSIDE the manifest's, not instead of them. An agent on this page can
  // both build the shop and browse it.
  expect(tools).toContain('list_products');
  // …and the page said so in its own log, with the count `tools.json`
  // published. This is the only externally visible proof that
  // `registerFromManifest` consumed the whole manifest rather than losing
  // entries to its per-tool `try`.
  await expect(page.locator('#dev-log')).toContainText(
    `registered ${MANIFEST_TOOLS.length} workspace tools`,
  );

  // --- 2. `dev_status` is the first call the guide tells an agent to make --
  const status = structured<{
    active_generation: Generation | null;
    runtime_generation: number;
    blocks: unknown[];
  }>(await execute(page, 'dev_status', {}));
  // Generation 0 came from `/seed/manifest.json` on this origin's first
  // fetch, and it carries no blocks — so nothing has rebuilt the runtime.
  expect(status.active_generation?.cause).toBe('seed');
  expect(status.active_generation?.status).toBe('active');
  expect(status.blocks).toEqual([]);
  expect(status.runtime_generation).toBe(0);

  // --- 3. Read, then write the site --------------------------------------
  //
  // The seed already published `site/index.html`, so a write is an overwrite
  // and has to send the hash it expects to find. Reading it back for that
  // hash is the round trip the tool descriptions tell an agent to make, and
  // it is why `dev_read_file` reports `sha256` at all.
  const seeded = structured<FileRead>(await execute(page, 'dev_read_file', {
    path: 'site/index.html',
  }));
  expect(seeded.encoding).toBe('utf8');
  expect(seeded.content).toContain(WELCOME_PHRASE);

  const writeStart = Date.now();
  const wrote = structured<FileWrite>(await execute(page, 'dev_write_file', {
    path: 'site/index.html',
    content: shopPage(),
    expected_sha256: seeded.sha256,
  }));
  // A `site/**` write publishes immediately — there is no separate deploy.
  expect(wrote.generation, JSON.stringify(wrote)).not.toBeNull();
  expect(wrote.generation?.cause).toBe('site_write');
  expect(wrote.generation?.status).toBe('active');
  expect(wrote.sha256).not.toBe(seeded.sha256);

  // The progress channel (design §4.3): `dev.js` wraps every mutating tool in
  // `withProgress`, which polls `/b/dev/api/status` for the duration of the
  // call and logs each new live generation. Seeing the id this write returned
  // in the panel is the proof the human watching the page learns what the
  // agent did — and that the poll really ran, since nothing else writes that
  // line.
  await expect(page.locator('#dev-log')).toContainText(
    `live generation: ${wrote.generation?.id}`,
  );

  // …and the preview iframe is showing it. `withProgress`'s catch-up reloads
  // the frame after the last outstanding call, so this needs no nudge.
  await expect(page.frameLocator('#dev-preview-frame').locator('h1')).toHaveText(SHOP_HEADING, {
    timeout: 60_000,
  });
  console.log(`site write → published → preview shows it: ${Date.now() - writeStart} ms`);

  // --- 4. Stock the shop -------------------------------------------------
  const productStart = Date.now();
  const product = structured<{ id: string; name: string; status: string }>(
    await execute(page, 'shop_create_product', SHOP_PRODUCT),
  );
  // Created as a draft — invisible to shoppers until something says otherwise.
  // The shop below is empty at this point, which is the state the last step
  // has to move it out of.
  expect(product.status).toBe('draft');
  expect(product.name).toBe(SHOP_PRODUCT.name);

  const offer = structured<{ status: string; offer: { id: string } }>(
    await execute(page, 'shop_create_offer', { product_id: product.id, ...SHOP_OFFER }),
  );
  expect(offer.status).toBe('draft');

  const published = structured<{ status: string; offer: { id: string } }>(
    await execute(page, 'shop_publish_offer', {
      product_id: product.id,
      offer_id: offer.offer.id,
    }),
  );
  expect(published.status).toBe('active');
  expect(published.offer.id).toBe(offer.offer.id);

  const live = structured<{ id: string; status: string }>(
    await execute(page, 'shop_update_product', { id: product.id, status: 'active' }),
  );
  expect(live.status).toBe('active');
  console.log(`product → offer → publish → active: ${Date.now() - productStart} ms`);

  // The agent can see its own work: `shop_update_product` is a mutating tool,
  // so `withProgress` reloaded the preview, and the page the agent wrote
  // three steps ago now lists the product it just activated.
  await expect(
    page.frameLocator('#dev-preview-frame').locator('.shop-product-name'),
  ).toHaveText(SHOP_PRODUCT.name, { timeout: 60_000 });

  // --- 5. Export the shop ------------------------------------------------
  //
  // Design §1's fourth and last step: "the user exports the shop and it runs
  // from a local static server". Everything about what the archive CONTAINS
  // is pinned by `impresspress-core/tests/dev_export.rs`, which reads a real
  // zip back with the `zip` crate — what only a browser can prove is that
  // `dev_export` actually hands the user a file: an object URL over a blob,
  // an anchor click, and a real download the browser wrote to disk.
  //
  // Before it: `dev_export_manifest`, the read an agent makes to see what an
  // export would contain. It is an ordinary HTTP tool from `tools.json`, so
  // this also proves the new selection is projected and callable.
  const exportStart = Date.now();
  const preview = structured<ExportManifest>(await execute(page, 'dev_export_manifest', {}));
  // The site the agent wrote, the runtime shell it needs to run, and the one
  // product the shop was stocked with.
  expect(preview.site_files).toBeGreaterThan(0);
  expect(preview.shell_files).toBeGreaterThan(3);
  expect(preview.total_bytes).toBeGreaterThan(0);
  expect(preview.tables['impresspress__products__products']).toBe(1);
  expect(preview.files.map((f) => f.path)).toContain('seed/manifest.json');

  const downloadPromise = page.waitForEvent('download', { timeout: 120_000 });
  const exported = structured<ExportManifest>(await execute(page, 'dev_export', {}));
  const download = await downloadPromise;
  // The same generation both calls describe: the export is a snapshot of what
  // is live, and nothing published between the two.
  expect(exported.generation_id).toBe(preview.generation_id);
  expect(download.suggestedFilename()).toBe(
    `impresspress-site-${exported.generation_id.slice(0, 8)}.zip`,
  );

  const scratch = mkdtempSync(path.join(tmpdir(), 'dev-export-'));
  try {
    const zipPath = path.join(scratch, 'export.zip');
    await download.saveAs(zipPath);

    // Read the archive with Python's `zipfile` rather than a JS zip library:
    // it is in the standard library of an interpreter this repo's tooling
    // already requires (`examples/dev-sandbox/build.sh`), and reading the
    // sandbox's own writer back with an INDEPENDENT implementation is the
    // whole point — a reader that shared its code would prove nothing.
    const inspected = JSON.parse(
      execFileSync(
        'python3',
        [
          '-c',
          [
            'import json, sys, zipfile',
            'z = zipfile.ZipFile(sys.argv[1])',
            'bad = z.testzip()',
            'assert bad is None, bad',
            'print(json.dumps({',
            '  "names": sorted(z.namelist()),',
            '  "sw": z.read("sw.js").decode("utf-8"),',
            '  "seed": json.loads(z.read("seed/manifest.json")),',
            '  "data": json.loads(z.read("seed/data.json")),',
            '  "index": z.read("seed/site/index.html").decode("utf-8"),',
            '}))',
          ].join('\n'),
          zipPath,
        ],
        { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 },
      ),
    ) as {
      names: string[];
      sw: string;
      seed: { schema_version: number; source_generation: string; site: { path: string }[] };
      data: { schema_version: number; tables: Record<string, unknown[]> };
      index: string;
    };

    // Every entry the manifest promised, and nothing the archive invented.
    expect(inspected.names).toEqual(preview.files.map((f) => f.path).sort());
    for (const expected of ['README.md', 'index.html', 'sw.js', 'loader.js', 'seed/manifest.json']) {
      expect(inspected.names).toContain(expected);
    }
    // The compiler tree is 72 MiB of toolchain for a `/b/dev` the exported
    // site does not have. Nothing under it may be in here.
    expect(inspected.names.filter((n) => n.startsWith('__impresspress_dev/'))).toEqual([]);

    // Development mode is OFF, in the one line that decides it — which is
    // also what turns off the isolation-header passthrough, since both read
    // the same constant.
    expect(inspected.sw).toContain('const DEV_ENABLED = false;');
    expect(inspected.sw).not.toContain('const DEV_ENABLED = true;');
    // …and `/seed/` is still bypassed, or the exported folder could never
    // import the seed shipped beside it.
    expect(inspected.sw).toContain("url.pathname.startsWith('/seed/')");

    // `seed/manifest.json` parses as the very format `seed::import` reads,
    // and it describes the site the agent wrote.
    expect(inspected.seed.schema_version).toBe(1);
    expect(inspected.seed.source_generation).toBe(exported.generation_id);
    expect(inspected.seed.site.map((f) => f.path)).toContain('index.html');
    expect(inspected.index).toContain(SHOP_HEADING);
    // And the shop's data came with it.
    expect(inspected.data.tables['impresspress__products__products']).toHaveLength(1);
    console.log(`export → download → verified archive: ${Date.now() - exportStart} ms`);

    // --- 5b. …and the exported folder RUNS ------------------------------
    //
    // The whole claim of design §1 step 4 and §10.1, and the one thing
    // reading the archive cannot establish: unzip it, serve it, open it, and
    // find the shop.
    //
    // A NEW CONTEXT on a DIFFERENT ORIGIN, unlike the shopper below. That is
    // not a workaround, it is the subject: a different origin has its own
    // OPFS, its own service-worker registration and an empty database, so
    // this boots the exported bundle exactly as someone who received the zip
    // would — and the seed import is the only thing that can put a site
    // there. The bundle boots with `const DEV_ENABLED = false;`, so this also
    // proves the fix that split `SandboxMode`: with the runtime half keyed on
    // the flag, this page would be blank.
    const runStart = Date.now();
    const unpacked = path.join(scratch, 'site');
    execFileSync('python3', [
      '-c',
      'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])',
      zipPath,
      unpacked,
    ]);
    const server = await serveDirectory(unpacked, WORKSPACE_EXPORT_PORT);
    const exportedContext = await browser.newContext({
      baseURL: `http://127.0.0.1:${WORKSPACE_EXPORT_PORT}`,
    });
    try {
      const site = await exportedContext.newPage();
      await bootServiceWorker(site);

      // The site the agent wrote, served from a folder on a plain static
      // host. Its seed was imported on this origin's first boot.
      await expect(site.locator('h1')).toHaveText(SHOP_HEADING, { timeout: 120_000 });
      // …and the product came with it, through the catalog route the page
      // fetches — so the data snapshot imported too, and the products block
      // is serving on the exported runtime.
      await expect(site.locator('.shop-product-name')).toHaveText(SHOP_PRODUCT.name, {
        timeout: 60_000,
      });

      // No workspace. The `/b/dev` route is not registered in an exported
      // bundle, and the router is the gate — so this is a 404, not a 403.
      expect(await site.evaluate(async () => (await fetch('/b/dev/api/status')).status)).toBe(404);

      // And no cross-origin isolation: an exported site has no compiler
      // needing `SharedArrayBuffer` and no preview frame to keep loadable, so
      // it gets back the third-party iframes isolation would have cost it
      // (spec amendment 14's stated tradeoff, amendment 19's other half).
      expect(await site.evaluate(() => window.crossOriginIsolated)).toBe(false);
      console.log(`exported bundle: served, booted, shop renders: ${Date.now() - runStart} ms`);
    } finally {
      await exportedContext.close();
      server.kill('SIGKILL');
    }
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }

  // --- 6. The shopper ----------------------------------------------------
  //
  // A NEW PAGE IN THE SAME CONTEXT, not `browser.newContext()`. A Playwright
  // context is an isolated storage partition: its own OPFS and its own
  // service-worker registration, so a second context would boot a second,
  // empty sandbox from the seed and never see a single thing the admin built.
  // The whole sandbox — database included — lives in this origin's storage,
  // so "another visitor" can only mean "the same storage, no session".
  // Clearing the cookies is what makes this page anonymous.
  const shopperStart = Date.now();
  await page.context().clearCookies();
  const shop = await page.context().newPage();
  await shop.addInitScript(MODEL_CONTEXT_POLYFILL);
  await bootServiceWorker(shop);

  // Anonymous for real: `/b/dev` is registered as an `Admin` extra route, and
  // a shopper who could still reach it would mean the cookie clear did
  // nothing and everything below proved only that the admin can see their own
  // work.
  expect(await shop.evaluate(async () => (await fetch('/b/dev/api/status')).status)).toBe(401);

  await expect(shop.locator('h1')).toHaveText(SHOP_HEADING, { timeout: 60_000 });
  await expect(shop.locator('.shop-product-name')).toHaveText(SHOP_PRODUCT.name, {
    timeout: 60_000,
  });
  // The storefront widget resolved the product through
  // `/b/products/storefront/{id}` — a Public route that refuses a product
  // with no ACTIVE offer. Its shadow-root title carrying the name is
  // therefore the end of the chain: create → price → publish → activate all
  // landed, and an anonymous browser can buy from it.
  await expect(shop.locator('impresspress-product').locator('.title')).toHaveText(
    SHOP_PRODUCT.name,
    { timeout: 60_000 },
  );
  console.log(`shopper: anonymous page renders the product: ${Date.now() - shopperStart} ms`);

  // The shopper's agent gets the site's Public tools and NONE of the
  // workspace's: `dev.js` is served only on `/b/dev`, and the tools it
  // registers are scoped to that document's lifetime. What this page runs is
  // the same `webmcp.js` every SSR page runs, against the same
  // `/b/webmcp/manifest.json` — filtered here to the anonymous tier, because
  // the manifest is generated per caller and this caller has no session.
  await waitForTool(shop, 'list_products');
  const shopperTools = (await registeredTools(shop, 1)).map((t) => t.name);
  expect(shopperTools).toContain('list_products');
  expect(shopperTools.filter((n) => n.startsWith('dev_') || n.startsWith('shop_'))).toEqual([]);

  await shop.close();
});

test('the workspace is cross-origin isolated and its preview frames the live site', async ({
  page,
}) => {
  test.setTimeout(300_000);
  await page.addInitScript(MODEL_CONTEXT_POLYFILL);
  await bootServiceWorker(page);
  await openWorkspace(page);

  // `/b/dev` sets COOP + COEP on its own response (`blocks/dev/page.rs`) so
  // the in-browser Rust compiler can use `SharedArrayBuffer`. This is the
  // only place that pair can be observed as the browser sees it — a Rust test
  // can read the header, but only a browser computes the capability from it.
  expect(await page.evaluate(() => window.crossOriginIsolated)).toBe(true);

  // Both documents must carry the pair, and for different reasons. `/b/dev`
  // needs it to BE isolated; `/` needs it to be EMBEDDABLE by an isolated
  // document — per the HTML spec a COEP document may only frame nested
  // documents with a compatible COEP, and that check is origin-independent,
  // so same-origin does not exempt the site. `/` gets it deployment-wide from
  // `wafer-run/security-headers` (`runtime_factory.rs` sets
  // `cross_origin_isolation = "credentialless"` when the sandbox is on).
  const headers = await page.evaluate(async (paths: string[]) => {
    const out: Record<string, { coep: string | null; coop: string | null }> = {};
    for (const path of paths) {
      const response = await fetch(path);
      out[path] = {
        coep: response.headers.get('cross-origin-embedder-policy'),
        coop: response.headers.get('cross-origin-opener-policy'),
      };
    }
    return out;
  }, ['/b/dev', '/']);
  expect(headers['/b/dev']).toEqual({ coep: 'credentialless', coop: 'same-origin' });
  expect(headers['/']).toEqual({ coep: 'credentialless', coop: 'same-origin' });

  // …and the assertion the headers alone cannot make. `credentialless` rather
  // than `require-corp` is the whole point of spec amendment 14: under
  // `require-corp` this frame rendered blank and every header above still
  // read exactly as an operator intended. Only the frame having content
  // proves a COEP page can embed this site.
  await expect(page.frameLocator('#dev-preview-frame').locator('h1')).toHaveText(WELCOME_HEADING, {
    timeout: 60_000,
  });
  await expect(page.frameLocator('#dev-preview-frame').locator('body')).toContainText(
    WELCOME_PHRASE,
  );
});

/**
 * The workspace finds the toolchain this build shipped, and the isolation the
 * toolchain needs reaches the whole deployment.
 *
 * Two facts, tested together because each is the other's precondition. A
 * cross-origin-isolated document is the only place `SharedArrayBuffer` exists,
 * and Rubrc's threads need it; a compiler manifest is the only way the page
 * learns which worker to start. Either one alone would leave `#dev-compile` a
 * button that cannot work.
 *
 * # Why `/` is isolated too, when the design once said it must not be
 *
 * Spec amendment 14. A document with any non-`unsafe-none` COEP may only
 * embed nested documents that carry a compatible COEP, and that check is
 * origin-independent — so as long as `/b/dev` frames the live site, `/` has to
 * carry the pair as well, and the browser then reports it as isolated. The
 * earlier reading ("the published site is NOT isolated") described a
 * deployment whose preview pane rendered blank. `credentialless` rather than
 * `require-corp` is what keeps the cost of that bounded: a site an agent built
 * here can still show a cross-origin image with no CORP header.
 *
 * On this bundle those headers reach `/` from `wafer-run/security-headers`
 * (`runtime_factory.rs`), and reach the compiler's own static files from the
 * service worker (`impresspress-bundle`'s `sw.js.tmpl`) — the static host is
 * `python3 -m http.server` and sends neither.
 */
test('the workspace discovers the packaged compiler on a cross-origin-isolated deployment', async ({
  page,
}) => {
  test.setTimeout(300_000);
  await loginToWorkspace(page);

  // The header is `blocks/dev/page.rs`'s; this is the capability the browser
  // computed from it, which no Rust test can observe.
  expect(await page.evaluate(() => crossOriginIsolated)).toBe(true);
  // …and the one thing that capability is FOR. Rubrc's rustc runs threaded.
  expect(await page.evaluate(() => typeof SharedArrayBuffer)).toBe('function');

  // `dev.js` fetches this on load. A build with no compiler overlay 404s here
  // and leaves the button disabled with a reason on it — the branch
  // `dev_compiler_discovery.test.mjs` covers, since `build.sh` refuses to
  // produce a bundle this spec could drive it with.
  const manifest = await page.evaluate(
    async () => (await fetch('/__impresspress_dev/compiler/manifest.json')).json(),
  );
  expect(manifest.schema_version).toBe(1);
  expect(manifest.entry).toBe(`/__impresspress_dev/compiler/${manifest.version}/worker.js`);

  // What the page did with it. The version is the pinned rubrc sha every
  // compiler URL carries, so a page showing the wrong one is a page that would
  // start the wrong worker.
  await expect(page.locator('#dev-compiler-version')).toHaveText(
    new RegExp(`^Compiler v${manifest.version} · \\d+\\.\\d MiB$`),
  );
  // Compile needs a toolchain AND a block, and this workspace is the seed —
  // `site/**` and nothing under `blocks/`. So the button is still disabled,
  // and the reason on it is what proves the manifest half succeeded: a build
  // that had not found a compiler would say so instead
  // (`dev.js`'s `updateCompileButton`). `dev-compile-tool.spec.ts` is where
  // it becomes enabled, one `dev_create_block` later.
  await expect(page.locator('#dev-compile')).toBeDisabled();
  await expect(page.locator('#dev-compile')).toHaveAttribute(
    'title',
    /No block to compile yet/,
  );
  await expect(page.locator('#dev-compile-block option')).toHaveCount(0);

  // The published site, which the preview pane frames — isolated for the
  // reason in this test's header, not as an accident.
  await page.goto('/', { waitUntil: 'commit' });
  await expect(page.locator('body')).toContainText(WELCOME_PHRASE, { timeout: 60_000 });
  expect(await page.evaluate(() => crossOriginIsolated)).toBe(true);
});

test('the editor refuses to save a binary file over itself', async ({ page }) => {
  test.setTimeout(300_000);

  // `dev.js` uses `alert`/`confirm`/`prompt`; the only one reachable here is
  // `save()`'s refusal alert, which fires for ANY status the server refuses
  // with, not just a 409. Playwright auto-dismisses an unhandled dialog, so
  // without this the one thing a broken guard might surface would vanish
  // silently — the recorder turns it into an assertion instead.
  const dialogs: string[] = [];
  page.on('dialog', (dialog) => {
    dialogs.push(dialog.message());
    return dialog.dismiss();
  });

  await page.addInitScript(MODEL_CONTEXT_POLYFILL);
  await bootServiceWorker(page);
  await openWorkspace(page);
  // `dev_export` is the last tool `dev.js` registers (see the wait above) —
  // this is a readiness barrier, not a read, so waiting by name rather than
  // `PAGE_TOOLS.length` is what it needs, not what it happens to satisfy.
  await waitForTool(page, 'dev_export');

  // A `.png` is binary whatever its bytes: `paths::content_type_for` maps the
  // extension to `image/png`, `may_be_text` says no, and so `dev_read_file`
  // answers `base64` for it — which is exactly the case the editor cannot
  // hold as text.
  const written = structured<FileWrite>(await execute(page, 'dev_write_file', {
    path: PIXEL_PNG_PATH,
    content: PIXEL_PNG_BASE64,
    encoding: 'base64',
  }));
  expect(written.size).toBe(PIXEL_PNG_BYTES);
  // Under `site/`, so it published — which is what makes "no new generation"
  // below a real assertion rather than a property of a path that never
  // publishes at all.
  expect(written.generation?.cause).toBe('site_write');

  const before = structured<{ generations: Generation[] }>(
    await execute(page, 'dev_list_generations', {}),
  );

  // Open it from the file pane, the way the human would. `dev_write_file` is
  // a mutating tool, so `withProgress`'s catch-up has already reloaded the
  // list; the click auto-waits for the entry regardless.
  await page.locator(`#dev-file-list a[data-path="${PIXEL_PNG_PATH}"]`).click();
  await expect(page.locator('#dev-editor-title')).toHaveText(PIXEL_PNG_PATH);

  // The box holds a PLACEHOLDER, not the file. This is the whole hazard: the
  // stored `expected_sha256` still describes the real file, so a save would
  // be accepted, would replace 67 bytes of PNG with this sentence, and would
  // publish a generation for the loss.
  await expect(page.locator('#dev-editor-text')).toHaveValue(
    `(binary file, ${PIXEL_PNG_BYTES} bytes)`,
  );
  await expect(page.locator('#dev-editor-text')).toBeDisabled();
  await expect(page.locator('#dev-save')).toBeDisabled();

  // Half one: the button. A disabled `<button>` swallows the click in the
  // browser, so `force` (which skips Playwright's own actionability checks)
  // still reaches a no-op — nothing runs, nothing to wait for.
  await page.locator('#dev-save').click({ force: true });

  // Half two: `save()`'s own early return, which `page.rs` pins as a source
  // assertion precisely because "a caller could reach it another way". Here
  // that caller is the DOM: re-enable ONLY the button, leaving the textarea
  // disabled, and click it for real. Emptying the file list first is the
  // tripwire — `withProgress` reloads it in its `finally` whichever branch
  // `save()` took, so a repopulated list means the handler really ran and
  // this is not a click that quietly went nowhere.
  await page.evaluate(() => {
    document.getElementById('dev-file-list')!.replaceChildren();
    (document.getElementById('dev-save') as HTMLButtonElement).disabled = false;
  });
  await page.locator('#dev-save').click();
  await expect(page.locator('#dev-file-list a')).not.toHaveCount(0);

  // Nothing moved: same bytes, same hash, same ledger.
  const after = structured<FileRead>(await execute(page, 'dev_read_file', {
    path: PIXEL_PNG_PATH,
  }));
  expect(after.encoding).toBe('base64');
  expect(after.content).toBe(PIXEL_PNG_BASE64);
  expect(after.sha256).toBe(written.sha256);
  expect(after.size).toBe(PIXEL_PNG_BYTES);

  const afterGenerations = structured<{ generations: Generation[] }>(
    await execute(page, 'dev_list_generations', {}),
  );
  expect(afterGenerations.generations.map((g) => g.id)).toEqual(
    before.generations.map((g) => g.id),
  );

  // Any refusal the server sent back would have alerted — `save()` reports
  // every one of them, not just the 409. It refused before it asked.
  expect(dialogs).toEqual([]);
});
