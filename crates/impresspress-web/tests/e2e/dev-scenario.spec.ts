import { test, expect, type BrowserContext, type Page } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import {
  ADMIN_EMAIL,
  bootServiceWorker,
  loginAdmin,
  loginToWorkspace,
  PAGE_TOOLS,
  SCENARIO_EXPORT_PORT,
  serveDirectory,
  WELCOME_PHRASE,
} from './fixtures/dev-sandbox';
import { MODEL_CONTEXT_POLYFILL } from './fixtures/model-context-polyfill';
import { SHOP_HEADING, SHOP_OFFER, SHOP_PRODUCT, shopPage } from './fixtures/shop-fixture';
import { execute, registeredTools, structured, waitForTool } from './fixtures/webmcp-helpers';

/**
 * The definition of done (design §21), as one test: design §16's seven-step
 * scenario, start to finish, on one origin with no server anywhere.
 *
 * Every other dev spec proves one layer of this and substitutes the rest.
 * `dev-foundations.spec.ts` drives `/b/dev/api/*` with `fetch` and stages a
 * guest `cargo` built on the host. `dev-workspace.spec.ts` has an agent build
 * the site and stock the shop through the tools, with no Rust in it.
 * `dev-compile.spec.ts` runs the real rubrc toolchain but touches neither the
 * shop nor the export. This file substitutes nothing and skips nothing: the
 * welcome page a first-time visitor lands on, the login, the whole tool set,
 * 75 MiB of toolchain compiling a Rust block IN THE PAGE, the site the agent
 * writes over the starter, three products priced and published, an anonymous
 * shopper who browses and prices them, an export unzipped onto a second static
 * host that boots the SAME shop from nothing but the archive, and finally a
 * rollback and a service-worker restart that the ledger survives.
 *
 * It runs in `e2e-dev-compile` beside `dev-compile.spec.ts`, and for the same
 * reason: step 2 needs `examples/dev-sandbox/compiler/dist/`, which is ~55
 * minutes and 12.6 GB of RSS to compose and is therefore cached on `PIN.json`
 * (`compiler/README.md`).
 *
 * # What each step is allowed to prove, and what it is not
 *
 * The seven steps are one test rather than seven because each one's SUBJECT is
 * the state the previous one left behind: an export that carried no compiled
 * block would prove nothing about dynamic blocks travelling, and a rollback
 * with nothing to roll back is a no-op. Splitting them would mean rebuilding
 * that state per test — three more cold boots and three more real compiles —
 * to test strictly less.
 *
 * The consequence is that a failure anywhere stops the rest, which is why
 * every step logs a `dev-scenario:` timing line as it finishes (CI greps them
 * into the job summary): the last line printed says how far the scenario got,
 * without reading the whole log.
 *
 * # Who is in which browser context, and why
 *
 * Two audiences, and they are NOT arranged the same way:
 *
 *  * **The shopper (step 5)** is a second PAGE in the SAME context. A
 *    Playwright context is an isolated storage partition, so
 *    `browser.newContext()` would boot a second, empty sandbox from the seed
 *    and never see a product this one created. The whole sandbox — database
 *    included — lives in this origin's storage, so "another visitor" can only
 *    mean "the same storage, no session"; anonymity comes from clearing the
 *    context's cookie jar, which signs the admin page out too, hence the
 *    second `loginAdmin` before steps 6 and 7.
 *  * **The exported bundle (step 6)** is a NEW context on a DIFFERENT ORIGIN
 *    (`SCENARIO_EXPORT_PORT`). Here the isolation is not a workaround but the
 *    subject: its own OPFS, its own service-worker registration and an empty
 *    database is exactly what someone who received the zip has, and the seed
 *    inside the archive is the only thing that can put the shop there.
 */

/** The block the agent scaffolds and compiles, and the two routes it serves. */
const BLOCK = 'newsletter';
const SUBSCRIBE = '/b/newsletter/subscribe';

/** The one endpoint the `table` template opts into `.agent_tool(..)`. */
const TOOL = 'subscribe_newsletter';

/**
 * The three products the shop is stocked with.
 *
 * `SHOP_PRODUCT` verbatim plus two siblings, because §16.4 asks for three and
 * `list_products` returning "some products" would not distinguish a working
 * catalog from one that lost a row. `slug` is unique per owner among
 * non-deleted products, and each is spelled out rather than derived from the
 * name so that a rename cannot silently produce a collision.
 */
const PRODUCTS = [
  SHOP_PRODUCT,
  { ...SHOP_PRODUCT, name: 'Poster print', slug: 'poster-print' },
  { ...SHOP_PRODUCT, name: 'Photo book', slug: 'photo-book' },
];

/**
 * The names the catalog must show, in the order it must show them.
 *
 * `/b/products/catalog` is "active products, sorted by name" — its own
 * summary — so the shopper's page and `list_products` both have a defined
 * order rather than an incidental one, and pinning it is what keeps a passing
 * assertion from depending on which product happened to be created first.
 */
const CATALOG_NAMES = PRODUCTS.map((p) => p.name).sort();

/** The group §16.4 asks for, over the products above. */
const GROUP = 'Prints';

/**
 * What the shopper's agent asks `preview_price` for, and what it must cost.
 *
 * The same inputs `webmcp.spec.ts` prices the seeded product with, against the
 * same `SHOP_OFFER`: one typed customer input (`pages`), 3 of them at the
 * offer's 1500 minor units each. Reusing the pair keeps the two specs — the
 * native server's and the browser sandbox's — pricing one shop rather than
 * two.
 */
const PRICE_INPUT = { quantity: 1, inputs: { pages: 3 } };
const PRICE_TOTAL_MINOR = 4500;

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

type Generation = { id: string; cause: string; status: string; site_files: number; blocks: number };
type FileRead = { path: string; sha256: string; size: number; encoding: string; content: string };
type FileWrite = { path: string; sha256: string; size: number; generation: Generation | null };
type Compile = {
  success: boolean;
  generation: Generation | null;
  diagnostics: Array<{ severity: string; message: string }>;
  elapsed_ms: number;
  compiler_version: string | null;
};

/**
 * The tables the data snapshot must carry out of this shop, and their row
 * counts.
 *
 * Names as `data_snapshot::TABLE_ALLOWLIST` spells them (each block's own
 * `TABLE` const). Counts as this scenario created them: three products, one
 * offer each, one group. Asserting the numbers rather than "the key exists" is
 * what makes this a check on the SNAPSHOT — a snapshot that exported the
 * products but lost the offers would still have every key.
 */
const EXPECTED_ROWS: Record<string, number> = {
  impresspress__products__products: PRODUCTS.length,
  impresspress__products__offers: PRODUCTS.length,
  impresspress__products__groups: 1,
};

/**
 * `POST /b/newsletter/subscribe`, from inside whichever document is passed.
 *
 * The body comes back as TEXT because this is also called when the route is
 * gone — after the rollback to the seed, and against the exported bundle
 * before its block is active. A 404 from the router is not the block's JSON,
 * and `response.json()` would throw before the caller could assert on the
 * status it was actually checking.
 */
async function subscribe(page: Page, email: string) {
  return page.evaluate(
    async ([url, address]) => {
      const response = await fetch(url, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ email: address }),
      });
      return { status: response.status, body: await response.text() };
    },
    [SUBSCRIBE, email] as const,
  );
}

/** `GET /b/dev/api/status`'s HTTP status, from inside a document. */
async function devStatusCode(page: Page): Promise<number> {
  return page.evaluate(async () => (await fetch('/b/dev/api/status')).status);
}

/**
 * Collect everything an exported bundle's service worker says.
 *
 * The one failure this test could otherwise report as an unexplained blank
 * page. A seed import that refuses — a schema version it does not read, a
 * table outside the allowlist, a block whose artifact will not load — is
 * logged by the runtime with `console::error_1` and then swallowed:
 * `dev_runtime::install` deliberately keeps booting, because a sandbox that
 * refuses to start is a sandbox whose diagnostics nobody can reach. Inside a
 * service worker those lines go nowhere Playwright looks by default, so a
 * broken export would surface here as "expected 3, got 0" and nothing else.
 *
 * `context.on('serviceworker')` has to be armed BEFORE the first navigation:
 * the worker registers during the boot shell's first load, and a listener
 * added afterwards would miss the boot it exists to explain.
 */
function captureServiceWorkerConsole(context: BrowserContext): string[] {
  const lines: string[] = [];
  context.on('serviceworker', (worker) => {
    worker.on('console', (message) => {
      lines.push(`[${message.type()}] ${message.text()}`);
    });
  });
  return lines;
}

test('the spec scenario: welcome → login → block → site → shop → shopper → export → rollback → restart', async ({
  browser,
  page,
}) => {
  // The bill, measured at ~70 s on a 24-core box (`total_ms=68322` and
  // `68562` on two runs): a cold sandbox boot (wasm compile, OPFS create,
  // migrations, seed import), 75 MiB of toolchain into the page, a release
  // build of the block, a runtime rebuild for it, three products through four
  // calls each, a second cold boot for the exported bundle on its own origin,
  // two rollbacks and a service-worker restart. Fifteen minutes is the
  // ceiling CI is given — 13× the measured figure: a runner is slower than
  // this box and none of the caches this test depends on are its to control.
  test.setTimeout(15 * 60 * 1000);
  const scenarioStart = Date.now();

  // Nothing here drives a button that alerts, but an unhandled dialog BLOCKS
  // the page rather than failing it — a hang with no message is the worst
  // possible way for a fifteen-minute test to break.
  const dialogs: string[] = [];
  page.on('dialog', (dialog) => {
    dialogs.push(dialog.message());
    return dialog.dismiss();
  });

  // Before ANY navigation: `dev.js` and `webmcp.js` both read
  // `document.modelContext` as they run, and a polyfill installed afterwards
  // would be a page with no agent on it.
  await page.addInitScript(MODEL_CONTEXT_POLYFILL);

  // --- 1. The welcome page, the login, and the tool set -------------------
  //
  // Design §16.1. The first load of this origin gets the static boot shell;
  // what `bootServiceWorker` waits for is the runtime serving `/` in its
  // place, which on a fresh origin means the wasm booted, migrated and
  // imported `/seed/manifest.json`.
  const step1Start = Date.now();
  await bootServiceWorker(page);

  // The starter site tells whoever lands here how to get in. Both halves are
  // part of its contract, so both are read the way a visitor reads them
  // rather than assumed.
  await expect(page.locator('body')).toContainText(WELCOME_PHRASE, { timeout: 60_000 });
  await expect(page.locator('body')).toContainText(ADMIN_EMAIL);

  // `loginToWorkspace` boots again before signing in. That is a handful of
  // waits that are already satisfied — the worker is controlling, `/b/auth/
  // login` answers, the shell is gone — not a second cold boot, and it keeps
  // "get me into the workspace" one call rather than three copied lines.
  await loginToWorkspace(page);

  // Two registrars share `document.modelContext` on `/b/dev`: `dev.js` adds
  // the page-scoped allowlist, `webmcp.js` adds the deployment manifest for
  // this caller's tier, and they finish in whichever order their fetches
  // complete. Each is waited for by a name only it publishes — a total would
  // pin the OTHER file's contract and could be satisfied by one registrar
  // alone.
  await waitForTool(page, 'list_products');
  await waitForTool(page, 'dev_export');
  const workspaceTools = (await registeredTools(page, 1)).map((t) => t.name);
  // "Exactly the expected tool set" (§16.1), not "at least": a `dev_*` or
  // `shop_*` tool this page published without a spec saying so is a surface
  // nobody reviewed.
  expect(workspaceTools.filter((n) => n.startsWith('dev_') || n.startsWith('shop_')).sort()).toEqual(
    PAGE_TOOLS,
  );
  console.log(`dev-scenario: step1_welcome_login_ms=${Date.now() - step1Start}`);

  // --- 2. A Rust block, compiled in the browser --------------------------
  //
  // Design §16.2. `dev_create_block` writes source and activates nothing;
  // `dev_compile_block` runs rubrc's rustc/cargo/LLVM inside a page worker,
  // stages what comes out, validates it, rebuilds the wasmi runtime and
  // publishes the generation.
  const step2Start = Date.now();
  structured<{ name: string }>(
    await execute(page, 'dev_create_block', { name: BLOCK, template: 'table' }),
  );
  // Source is not a deployment.
  expect((await subscribe(page, 'too-early@newsletter.test')).status).toBe(404);

  const compiled = structured<Compile>(await execute(page, 'dev_compile_block', { name: BLOCK }));
  expect(compiled.success, JSON.stringify(compiled.diagnostics)).toBe(true);
  expect(
    compiled.diagnostics.filter((d) => d.severity === 'error'),
    JSON.stringify(compiled.diagnostics),
  ).toEqual([]);
  expect(compiled.generation?.cause).toBe('block_compile');
  expect(compiled.generation?.status).toBe('active');
  // `rustc --version` as run inside the worker's own VFS — a page cannot
  // synthesize it, so this is the toolchain identifying itself.
  expect(compiled.compiler_version).toContain('rustc');

  // The block's table exists because `db::ensure_table` ran in the guest's
  // `init` on the collection it claimed, and the route is live.
  const subscribed = await subscribe(page, 'admin@newsletter.test');
  expect(subscribed.status, subscribed.body).toBe(200);
  expect(JSON.parse(subscribed.body)).toEqual({ ok: true });

  // …and an ANONYMOUS caller is offered its curated tool. `credentials:
  // 'omit'` is what makes this the anonymous tier without signing this
  // context out: a same-origin fetch that sends no cookie is exactly the
  // request a visitor's browser makes, and the manifest is generated per
  // caller. This is the point of the whole sandbox — a block written minutes
  // ago is a tool another agent can use, with no deploy step in between.
  const anonManifest = await page.evaluate(
    async () =>
      (await fetch('/b/webmcp/manifest.json', { credentials: 'omit' })).json() as Promise<{
        tools: Array<{ name: string }>;
      }>,
  );
  expect(anonManifest.tools.map((t) => t.name)).toContain(TOOL);
  console.log(
    `dev-scenario: step2_block_ms=${Date.now() - step2Start} compile_ms=${compiled.elapsed_ms}`,
  );

  // --- 3. The site the agent writes --------------------------------------
  //
  // Design §16.3. The seed already published `site/index.html`, so this is an
  // overwrite and has to send the hash it expects to find — the round trip
  // `dev_read_file` reports `sha256` for.
  const step3Start = Date.now();
  const seeded = structured<FileRead>(
    await execute(page, 'dev_read_file', { path: 'site/index.html' }),
  );
  expect(seeded.content).toContain(WELCOME_PHRASE);

  const wrote = structured<FileWrite>(
    await execute(page, 'dev_write_file', {
      path: 'site/index.html',
      content: shopPage(),
      expected_sha256: seeded.sha256,
    }),
  );
  // A `site/**` write publishes immediately — there is no separate deploy.
  expect(wrote.generation, JSON.stringify(wrote)).not.toBeNull();
  expect(wrote.generation?.cause).toBe('site_write');
  expect(wrote.generation?.status).toBe('active');
  // The generation the rollback in step 7 comes back to. It carries the shop
  // page AND the block compiled in step 2, since it was derived from that
  // generation's manifests.
  const siteGeneration = wrote.generation!;
  expect(siteGeneration.blocks).toBe(1);

  // `withProgress` reloads the preview after the last outstanding mutating
  // call, so the frame needs no nudge. A frame with an `h1` in it is also the
  // only proof that a COEP document can embed this site at all.
  await expect(page.frameLocator('#dev-preview-frame').locator('h1')).toHaveText(SHOP_HEADING, {
    timeout: 60_000,
  });
  console.log(`dev-scenario: step3_site_ms=${Date.now() - step3Start}`);

  // --- 4. The shop -------------------------------------------------------
  //
  // Design §16.4. Four calls per product, because that is what the products
  // block requires and the agent has to know it: a product is created as a
  // draft, an offer is created as a draft, publishing the offer makes it
  // purchasable, and only `status: "active"` makes the product visible.
  const step4Start = Date.now();
  const offerIds: string[] = [];
  for (const spec of PRODUCTS) {
    const product = structured<{ id: string; name: string; status: string }>(
      await execute(page, 'shop_create_product', spec),
    );
    expect(product.status).toBe('draft');
    expect(product.name).toBe(spec.name);

    const offer = structured<{ status: string; offer: { id: string } }>(
      await execute(page, 'shop_create_offer', { product_id: product.id, ...SHOP_OFFER }),
    );
    expect(offer.status).toBe('draft');
    offerIds.push(offer.offer.id);

    const published = structured<{ status: string; offer: { id: string } }>(
      await execute(page, 'shop_publish_offer', {
        product_id: product.id,
        offer_id: offer.offer.id,
      }),
    );
    expect(published.status).toBe('active');

    const live = structured<{ id: string; status: string }>(
      await execute(page, 'shop_update_product', { id: product.id, status: 'active' }),
    );
    expect(live.status).toBe('active');
  }
  const group = structured<{ id: string; name: string }>(
    await execute(page, 'shop_create_group', { name: GROUP }),
  );
  expect(group.name).toBe(GROUP);

  // The agent can see its own work: the page it wrote in step 3 reads the
  // public catalog, and `withProgress` reloaded the frame after the last
  // mutating call.
  await expect(page.frameLocator('#dev-preview-frame').locator('.shop-product-name')).toHaveText(
    CATALOG_NAMES,
    { timeout: 60_000 },
  );
  console.log(`dev-scenario: step4_shop_ms=${Date.now() - step4Start}`);

  // --- 5. The anonymous shopper ------------------------------------------
  //
  // Design §16.5. A second page in the SAME context with the cookie jar
  // emptied — see the note at the top of this file for why a new context
  // would be the wrong shopper.
  const step5Start = Date.now();
  await page.context().clearCookies();
  const shop = await page.context().newPage();
  shop.on('dialog', (dialog) => {
    dialogs.push(dialog.message());
    return dialog.dismiss();
  });
  await shop.addInitScript(MODEL_CONTEXT_POLYFILL);
  await bootServiceWorker(shop);

  // Anonymous for real. `/b/dev` is an `Admin` extra route; a shopper who
  // could still reach it would mean the cookie clear did nothing and
  // everything below proved only that the admin can see their own work.
  expect(await devStatusCode(shop)).toBe(401);

  await expect(shop.locator('h1')).toHaveText(SHOP_HEADING, { timeout: 60_000 });
  // Every product, by name and in the catalog's order — not just a count, so
  // three rows of the wrong product would fail here rather than pass.
  //
  // `.shop-product-name` rather than a bare `h2`: the storefront widget
  // renders its own `h2` inside a shadow root and Playwright's CSS engine
  // pierces open shadow roots, so `h2` would match twice per product.
  await expect(shop.locator('.shop-product-name')).toHaveText(CATALOG_NAMES, {
    timeout: 60_000,
  });
  // The widget resolved a product through `/b/products/storefront/{id}`, a
  // Public route that refuses a product with no ACTIVE offer — so this is the
  // end of the create → price → publish → activate chain, seen by a browser
  // that could buy from it.
  await expect(shop.locator('impresspress-product').first().locator('.title')).toHaveText(
    CATALOG_NAMES[0],
    { timeout: 60_000 },
  );

  // The shopper's own agent. `webmcp.js` is on this page because the page the
  // agent wrote includes it (there is no SSR layout to inject it), and the
  // manifest it fetched is the anonymous tier's.
  await waitForTool(shop, 'list_products');
  const shopperTools = (await registeredTools(shop, 1)).map((t) => t.name);
  // §16.1's other half: no dev or shop tool on `/`. `dev.js` is served only
  // on `/b/dev` and its registrations are scoped to that document.
  expect(shopperTools.filter((n) => n.startsWith('dev_') || n.startsWith('shop_'))).toEqual([]);

  const listed = structured<{ records: Array<{ id: string; name: string }>; total_count: number }>(
    await execute(shop, 'list_products', {}),
  );
  expect(listed.records.map((r) => r.name)).toEqual(CATALOG_NAMES);
  expect(listed.total_count).toBe(PRODUCTS.length);

  // …and it can price one. The offer is `pricing_model: "components"` with a
  // typed customer input, so a quote is the server computing from the
  // published definition, not a number the page had lying around.
  const quote = structured<{
    offer_id: string;
    amounts: { currency: string; total_minor: number };
  }>(await execute(shop, 'preview_price', { offer_id: offerIds[0], ...PRICE_INPUT }));
  expect(quote.offer_id).toBe(offerIds[0]);
  expect(quote.amounts.total_minor).toBe(PRICE_TOTAL_MINOR);
  await shop.close();
  console.log(`dev-scenario: step5_shopper_ms=${Date.now() - step5Start}`);

  // Back as the admin: the cookie clear above emptied the whole context's
  // jar, so the control plane needs a session again before steps 6 and 7.
  await loginAdmin(page);
  await page.goto('/b/dev', { waitUntil: 'commit' });
  await expect(page.locator('#dev-progress-steps li').first()).toBeAttached({ timeout: 60_000 });
  await waitForTool(page, 'dev_export');

  // --- 6. Export, served on another port, booting the same shop ----------
  //
  // Design §16.6 and §10.1, and the claim no amount of reading the archive
  // can establish. `dev_export` hands the user a FILE — an object URL over a
  // blob and an anchor click — so the download is captured as a download; the
  // manifest is what the tool result carries.
  const step6Start = Date.now();
  const downloadPromise = page.waitForEvent('download', { timeout: 180_000 });
  const exported = structured<ExportManifest>(await execute(page, 'dev_export', {}));
  const download = await downloadPromise;
  expect(download.suggestedFilename()).toBe(
    `impresspress-site-${exported.generation_id.slice(0, 8)}.zip`,
  );

  // The counts, against what this scenario actually built. One compiled
  // block, the live generation's site files, and every row of the shop.
  expect(exported.blocks).toBe(1);
  expect(exported.site_files).toBe(siteGeneration.site_files);
  expect(exported.shell_files).toBeGreaterThan(3);
  expect(exported.total_bytes).toBeGreaterThan(0);
  for (const [table, rows] of Object.entries(EXPECTED_ROWS)) {
    expect(exported.tables[table], `${table} in ${JSON.stringify(exported.tables)}`).toBe(rows);
  }
  // The block travelled as both artifact and source, so the folder can serve
  // it and its owner can keep working on it.
  const entries = exported.files.map((f) => f.path);
  expect(entries).toContain(`seed/blocks/${BLOCK}.wasm`);
  expect(entries).toContain(`seed/blocks/${BLOCK}/src/lib.rs`);
  expect(entries).toContain('seed/data.json');

  const scratch = mkdtempSync(path.join(tmpdir(), 'dev-scenario-export-'));
  try {
    const zipPath = path.join(scratch, 'site.zip');
    await download.saveAs(zipPath);

    // Python's `zipfile`, not a JS zip library: it is in the standard library
    // of an interpreter this repo's tooling already requires
    // (`examples/dev-sandbox/build.sh`), and reading the sandbox's own writer
    // back with an INDEPENDENT implementation is the point.
    execFileSync('python3', ['-m', 'zipfile', '-e', zipPath, path.join(scratch, 'out')]);
    const server = await serveDirectory(path.join(scratch, 'out'), SCENARIO_EXPORT_PORT);
    const bundleStart = Date.now();
    const fresh = await browser.newContext({ baseURL: `http://127.0.0.1:${SCENARIO_EXPORT_PORT}` });
    // Armed before the first navigation, or it would miss the boot it exists
    // to explain.
    const swConsole = captureServiceWorkerConsole(fresh);
    try {
      const site = await fresh.newPage();
      site.on('dialog', (dialog) => dialog.dismiss());
      await site.addInitScript(MODEL_CONTEXT_POLYFILL);
      await bootServiceWorker(site);

      // The SAME shop, on a different origin with an empty database, from
      // nothing but the archive: the same three products, by name, in the same
      // order — which means `seed/data.json` was imported and the products
      // block is serving on the exported runtime.
      //
      // A seed import that refused was logged and swallowed by
      // `dev_runtime::install`, so a bare assertion failure here would be
      // "expected 3, got 0" with no cause. Re-raising with the service
      // worker's own console is what turns a broken export into a diagnosis.
      try {
        await expect(site.locator('.shop-product-name')).toHaveText(CATALOG_NAMES, {
          timeout: 180_000,
        });
      } catch (failure) {
        const said = swConsole.length
          ? swConsole.join('\n')
          : '(the exported service worker logged nothing at all)';
        throw new Error(
          `the exported bundle did not render the shop.\n` +
            `--- service worker console (${SCENARIO_EXPORT_PORT}) ---\n${said}\n` +
            `--- assertion ---\n${failure instanceof Error ? failure.message : String(failure)}`,
        );
      }
      await expect(site.locator('h1')).toHaveText(SHOP_HEADING);

      // The block came too, was re-imported from `seed/blocks/`, and is
      // ACTIVE — the route answers on an origin whose runtime was built from
      // the archive's manifests, not from a compile.
      const fromBundle = await subscribe(site, 'shopper@newsletter.test');
      expect(fromBundle.status, `${fromBundle.body}\n${swConsole.join('\n')}`).toBe(200);
      expect(JSON.parse(fromBundle.body)).toEqual({ ok: true });

      // No workspace: `/b/dev` is not registered in an exported bundle and
      // the router is the gate, so this is a 404 rather than a 403.
      expect(await devStatusCode(site)).toBe(404);
      // And no cross-origin isolation: an exported site has no compiler
      // needing `SharedArrayBuffer` and no preview frame to keep loadable, so
      // it gets back the third-party iframes isolation would have cost it.
      expect(await site.evaluate(() => window.crossOriginIsolated)).toBe(false);
      console.log(
        `dev-scenario: step6_export_ms=${Date.now() - step6Start} ` +
          `archive_bytes=${exported.total_bytes} bundle_boot_ms=${Date.now() - bundleStart}`,
      );
    } finally {
      await fresh.close();
      server.kill('SIGKILL');
    }
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }

  // --- 7. Rollback, and a service-worker restart -------------------------
  //
  // Design §16.7. Two rollbacks, because they prove different things.
  const step7Start = Date.now();

  // The first is to the generation the step-3 write published, which is the
  // one that is live: a republish that changes neither the site manifest nor
  // the block set. It must leave the shop exactly as it is — a rollback that
  // "restored" its own target into something else would be a corrupt ledger,
  // and this is the only shape of rollback that can show it.
  const republished = structured<{ generation: Generation }>(
    await execute(page, 'dev_rollback', { id: siteGeneration.id }),
  );
  expect(republished.generation.cause).toBe('rollback');
  expect(republished.generation.status).toBe('active');
  await expect(page.frameLocator('#dev-preview-frame').locator('h1')).toHaveText(SHOP_HEADING, {
    timeout: 60_000,
  });
  expect((await subscribe(page, 'still-here@newsletter.test')).status).toBe(200);

  // The ledger, newest first. Every entry and no others: `dev_create_block`
  // stages source, the twelve `shop_*` calls write rows and `dev_export`
  // reads — none of them publishes, and a generation appearing for one of
  // them would mean the sandbox rebuilds its runtime for a database write.
  const ledger = structured<{ generations: Generation[] }>(
    await execute(page, 'dev_list_generations', {}),
  );
  expect(ledger.generations.map((g) => g.cause)).toEqual([
    'rollback',
    'site_write',
    'block_compile',
    'seed',
  ]);

  // The second rollback is the real one: all the way back to generation 0,
  // which carries the starter site and no blocks at all.
  const seedGeneration = ledger.generations.find((g) => g.cause === 'seed')!;
  expect(seedGeneration.blocks).toBe(0);
  const reverted = structured<{ generation: Generation }>(
    await execute(page, 'dev_rollback', { id: seedGeneration.id }),
  );
  await expect(page.frameLocator('#dev-preview-frame').locator('body')).toContainText(ADMIN_EMAIL, {
    timeout: 60_000,
  });
  // …and the block went with the site. The runtime was rebuilt without it, so
  // its route is gone from the router rather than merely refusing.
  expect((await subscribe(page, 'nobody@newsletter.test')).status).toBe(404);
  const rollbackMs = Date.now() - step7Start;

  // The restart. Unregistering drops the worker AND its in-memory `Rc<Wafer>`;
  // the next load registers a fresh one, which finds the instance no longer
  // fresh (so it does NOT re-seed), converges on the activation journal and
  // rebuilds the active generation's block set — here, the empty one.
  const restartStart = Date.now();
  await page.evaluate(async () => {
    for (const registration of await navigator.serviceWorker.getRegistrations()) {
      await registration.unregister();
    }
  });
  await bootServiceWorker(page);
  await expect(page.locator('body')).toContainText(WELCOME_PHRASE, { timeout: 60_000 });
  await expect(page.locator('body')).toContainText(ADMIN_EMAIL);

  // What the page renders would also be true of a re-seeded instance. The
  // ledger is what tells the two apart: the active generation is still the
  // rollback this test published, not a fresh generation 0.
  const afterRestart = await page.evaluate(
    async () =>
      (await fetch('/b/dev/api/status')).json() as Promise<{
        active_generation: Generation | null;
        blocks: unknown[];
      }>,
  );
  expect(afterRestart.active_generation?.id).toBe(reverted.generation.id);
  expect(afterRestart.active_generation?.cause).toBe('rollback');
  expect(afterRestart.active_generation?.status).toBe('active');
  expect(afterRestart.blocks).toEqual([]);
  console.log(
    `dev-scenario: step7_rollback_ms=${rollbackMs} restart_ms=${Date.now() - restartStart}`,
  );

  expect(dialogs).toEqual([]);
  console.log(`dev-scenario: total_ms=${Date.now() - scenarioStart}`);
});
