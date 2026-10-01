import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { BOOTSTRAP_PORT, bootServiceWorker, loginAdmin, serveDirectory } from './fixtures/dev-sandbox';

/**
 * The bootstrap seed (`examples/dev-sandbox/seeds/bootstrap/`): generation 0
 * of a fresh origin is a Bootstrap-built site with the framework vendored
 * and a site-authoring guide the reference serves.
 *
 * The bundle under test is NOT the one on `TEST_PORT` — that is the blank
 * seed the other specs share. CI builds this seed with
 * `build.sh --seed bootstrap --out <dir>` and passes the directory in
 * `BOOTSTRAP_DIST`; this spec serves it on `BOOTSTRAP_PORT` itself, the
 * way the export specs serve an unpacked bundle, in a context of its own
 * so `bootServiceWorker`'s relative navigations land on it.
 */
const BOOTSTRAP_DIST = process.env.BOOTSTRAP_DIST;
if (!BOOTSTRAP_DIST) {
  throw new Error(
    'BOOTSTRAP_DIST is not set — build the seed with ' +
      '`examples/dev-sandbox/build.sh --seed bootstrap --out <dir>` and point BOOTSTRAP_DIST at <dir>',
  );
}

const repoFile = (relative: string) => fileURLToPath(new URL(`../../../../${relative}`, import.meta.url));

test('generation 0 is a Bootstrap site with the framework vendored and a site guide', async ({ browser }) => {
  test.setTimeout(240_000);
  const server = await serveDirectory(BOOTSTRAP_DIST, BOOTSTRAP_PORT);
  const context = await browser.newContext({ baseURL: `http://127.0.0.1:${BOOTSTRAP_PORT}` });
  const uncaught: string[] = [];
  try {
    const page = await context.newPage();
    page.on('pageerror', (error) => uncaught.push(error.message));
    await bootServiceWorker(page);

    // Bootstrap-built: the navbar is the framework's, the heading is this seed's.
    // `display: flex` is Bootstrap's `.navbar` rule — a `nav` with no
    // stylesheet is `block` — so this is what proves the vendored CSS loaded.
    await expect(page.locator('nav.navbar')).toBeVisible({ timeout: 60_000 });
    await expect(page.locator('nav.navbar')).toHaveCSS('display', 'flex');
    await expect(page.locator('h1')).toHaveText('Build a website with your browser agent');
    await expect(page.locator('body')).toContainText('Open workspace');

    // The vendored files serve from generation 0 with the types and sizes the
    // bundle's own seed manifest declares (sizes in bytes, so compared as bytes).
    const manifest = JSON.parse(readFileSync(path.join(BOOTSTRAP_DIST, 'seed', 'manifest.json'), 'utf8'));
    const vendored: { path: string; size: number; content_type: string }[] = manifest.site.filter(
      (entry: { path: string }) => entry.path.startsWith('vendor/bootstrap/'),
    );
    expect(vendored.map((entry) => entry.path)).toEqual(
      expect.arrayContaining(['vendor/bootstrap/bootstrap.min.css', 'vendor/bootstrap/bootstrap.bundle.min.js']),
    );
    for (const entry of vendored) {
      const served = await page.evaluate(async (url) => {
        const r = await fetch(url);
        return { status: r.status, type: r.headers.get('content-type'), bytes: (await r.arrayBuffer()).byteLength };
      }, `/${entry.path}`);
      expect(served, entry.path).toEqual({ status: 200, type: entry.content_type, bytes: entry.size });
    }

    // The seed's sandbox block reached the runtime: status names the template,
    // the reference carries the guide.
    await loginAdmin(page);
    const status = await page.evaluate(async () => (await fetch('/b/dev/api/status')).json());
    expect(status.template).toBe('bootstrap');
    const reference = await page.evaluate(async () => (await fetch('/b/dev/api/reference')).json());
    expect(reference.template).toBe('bootstrap');
    expect(reference.site_markdown).toContain('Bootstrap 5.3.8');
    expect(reference.site_markdown).toContain('/vendor/bootstrap/bootstrap.min.css');
    expect(reference.markdown).toContain('Block::new');

    expect(uncaught, 'the welcome page ran without an uncaught error').toEqual([]);
  } finally {
    await context.close();
    server.kill('SIGKILL');
  }
});

/**
 * The storefront attributes a guide's `Attributes:` paragraph names: the
 * paragraph runs from the line that starts with `Attributes:` to the next
 * blank line, and an attribute is a backticked name OUTSIDE parentheses —
 * the parentheses are where the paragraph puts each attribute's values and
 * defaults (`hosted`, `same-origin`, `presentation="payment_link"`, …), so a
 * hyphenated value is not mistaken for an attribute.
 */
function guideAttributes(guide: string): Set<string> {
  const lines = guide.split('\n');
  const start = lines.findIndex((l) => l.trim().startsWith('Attributes:'));
  expect(start, 'the guide has an `Attributes:` paragraph').toBeGreaterThanOrEqual(0);
  const end = lines.findIndex((l, i) => i > start && l.trim() === '');
  const paragraph = lines.slice(start, end === -1 ? undefined : end).join(' ');

  const names = new Set<string>();
  let depth = 0;
  for (let i = 0; i < paragraph.length; i++) {
    const c = paragraph[i];
    if (c === '`') {
      const close = paragraph.indexOf('`', i + 1);
      expect(close, 'every backtick in the paragraph is closed').toBeGreaterThan(i);
      const span = paragraph.slice(i + 1, close);
      if (depth === 0 && /^[a-z][a-z-]*$/.test(span)) names.add(span);
      i = close;
    } else if (c === '(') {
      depth++;
    } else if (c === ')') {
      depth--;
    }
  }
  expect(depth, 'the paragraph closes every parenthesis it opens').toBe(0);
  return names;
}

/**
 * Drift guard, both ways: every `<impresspress-product>` attribute a guide
 * names is one the element reads, and every attribute the element reads is
 * one the guide names.
 *
 * The element reads most attributes as `getAttribute("…")` literals, but
 * `success-url` and `cancel-url` through one `getAttribute(attribute)` whose
 * name it picks at runtime — so those two are listed here, and pinned as
 * literals in the element, which is what fails if that read is renamed.
 */
const STOREFRONT = 'crates/impresspress-core/src/blocks/products/assets/storefront.js';
const DYNAMICALLY_READ = ['success-url', 'cancel-url'];

for (const seed of ['blank', 'bootstrap']) {
  test(`the ${seed} guide names exactly the storefront attributes the element reads`, () => {
    const guide = readFileSync(repoFile(`examples/dev-sandbox/seeds/${seed}/guide.md`), 'utf8');
    const storefront = readFileSync(repoFile(STOREFRONT), 'utf8');

    const named = guideAttributes(guide);
    expect(named.size, 'the paragraph names attributes').toBeGreaterThan(0);

    // (a) Every name the guide gives is a string the element uses.
    for (const name of named) {
      expect(storefront, `${name} (named by the ${seed} guide) appears in storefront.js`).toContain(
        `"${name}"`,
      );
    }

    // (b) Every attribute the element reads is one the guide names.
    for (const name of DYNAMICALLY_READ) {
      expect(storefront, `storefront.js still names ${name}`).toContain(`"${name}"`);
    }
    const read = [
      ...[...storefront.matchAll(/getAttribute\("([a-z-]+)"\)/g)].map((m) => m[1]),
      ...DYNAMICALLY_READ,
    ];
    expect(read.length).toBeGreaterThan(DYNAMICALLY_READ.length);
    for (const name of read) {
      expect([...named], `${name} (read by storefront.js) is named by the ${seed} guide`).toContain(name);
    }
  });
}
