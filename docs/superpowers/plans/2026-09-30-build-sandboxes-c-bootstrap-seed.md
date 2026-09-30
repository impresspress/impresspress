# Build Sandboxes C — Bootstrap Seed Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `bootstrap` seed — Bootstrap 5.3.8 vendored and pinned by hash, a Bootstrap-built welcome page, a site-authoring guide for the framework and the shop pieces — plus an e2e spec that boots it and the CI step that builds it.

**Architecture:** `seeds/bootstrap/vendor.json` pins three upstream files by URL and sha256; `seeds/vendor.py` downloads and verifies them into `site/vendor/bootstrap/`; `check-seeds.py` re-verifies the pins. The seed's `sandbox.json` + `guide.md` ride the manifest (Plan B). `dev-bootstrap.spec.ts` serves the seed's own bundle on its own port from inside the spec — the way the export specs serve an unpacked bundle — so CI builds it once with `build.sh --seed bootstrap --out …` and passes the directory in `BOOTSTRAP_DIST`.

**Tech Stack:** Python 3 (stdlib `urllib`), Bootstrap 5.3.8 (MIT) from jsDelivr, Playwright, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-09-30-build-sandboxes-design.md` §5.4, §7, §11 (e2e), §12 (PR C).

## Global Constraints

- Branch from `main` after Plan B has merged.
- Bootstrap 5.3.8, three files, byte-identical to upstream — the `sourceMappingURL` comment stays. Pinned hashes (measured 2026-09-30 from `https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/`):
  - `dist/css/bootstrap.min.css` — 232 111 bytes — `d85327d99c7a3ee1f9b5d0500d1370acea3ad2db39c163c2f51f232baedbdede`
  - `dist/js/bootstrap.bundle.min.js` — 80 496 bytes — `e4fd49181388c48ec5040bd3fe66f57c29c8e67fcd8502b3354b96ec7ab47cc7`
  - `LICENSE` — 1 093 bytes — `4620c84ad5ce8602ff65640ed6b7c8b78ebb9e036584f0ebc1ccc88206a4bb51`
- Vendored paths carry no version: `vendor/bootstrap/bootstrap.min.css`, `vendor/bootstrap/bootstrap.bundle.min.js`, `vendor/bootstrap/LICENSE.txt` (`.txt` so the runtime serves it as `text/plain`).
- The welcome page ships no stylesheet of our own, links `/vendor/bootstrap/bootstrap.min.css`, loads the bundle and `/b/webmcp/webmcp.js`, keeps the "Open workspace" link text (`WELCOME_PHRASE` in the e2e fixtures).
- The guide's storefront line starts with `Attributes:` and names only attributes `storefront.js` reads with `getAttribute` (the drift test).
- Ports: the bootstrap bundle is served on `8097` by the spec (`WORKSPACE_EXPORT_PORT` is 8098, `SCENARIO_EXPORT_PORT` 8099).
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`; PR, never direct to `main`.

## Review Focus

1. `vendor.py bootstrap` on a machine where jsDelivr serves a newer 5.3.x under the pinned `@5.3.8` URL — impossible by construction (the URL names the exact version), but a changed byte must still be a refusal, never a rewrite, unless `--refresh` was passed. (Task 1, step 4.)
2. A vendored file edited by hand under `site/vendor/` — `check-seeds.py` must fail naming the file and pointing at `vendor.py`. (Task 1, step 6.)
3. The welcome page under the sandbox CSP (`style-src 'self'`, `script-src 'self'`, `img-src data:`) — no console errors; Bootstrap's inline SVG icons load. (Task 3, the e2e checks the page renders its navbar and reads no `pageerror`.)
4. An agent that `dev_read_file`s the 232 KiB stylesheet — the guide tells it not to; nothing else can stop it. (Task 2, guide section 1.)
5. `BOOTSTRAP_DIST` unset when the spec runs — the spec must fail at load with a message that says how to build it, not skip silently. (Task 3, step 1.)

---

### Task 1: Pin and vendor Bootstrap

**Files:**
- Create: `examples/dev-sandbox/seeds/bootstrap/vendor.json`
- Create: `examples/dev-sandbox/seeds/vendor.py`
- Modify: `examples/dev-sandbox/seeds/check-seeds.py` (vendor verification)
- Create (by the script): `examples/dev-sandbox/seeds/bootstrap/site/vendor/bootstrap/{bootstrap.min.css,bootstrap.bundle.min.js,LICENSE.txt}`
- Create: `.gitattributes` entry for vendored files

**Interfaces:**
- Produces: `seeds/vendor.py <name> [--refresh]`; `vendor.json` shape `{name, version, license, files: [{path, url, sha256}]}`.

- [ ] **Step 1: The pin**

`examples/dev-sandbox/seeds/bootstrap/vendor.json`:
```json
{
  "name": "bootstrap",
  "version": "5.3.8",
  "license": "MIT",
  "files": [
    {
      "path": "vendor/bootstrap/bootstrap.min.css",
      "url": "https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/dist/css/bootstrap.min.css",
      "sha256": "d85327d99c7a3ee1f9b5d0500d1370acea3ad2db39c163c2f51f232baedbdede"
    },
    {
      "path": "vendor/bootstrap/bootstrap.bundle.min.js",
      "url": "https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/dist/js/bootstrap.bundle.min.js",
      "sha256": "e4fd49181388c48ec5040bd3fe66f57c29c8e67fcd8502b3354b96ec7ab47cc7"
    },
    {
      "path": "vendor/bootstrap/LICENSE.txt",
      "url": "https://cdn.jsdelivr.net/npm/bootstrap@5.3.8/LICENSE",
      "sha256": "4620c84ad5ce8602ff65640ed6b7c8b78ebb9e036584f0ebc1ccc88206a4bb51"
    }
  ]
}
```

- [ ] **Step 2: The vendoring script**

`examples/dev-sandbox/seeds/vendor.py` (executable):
```python
#!/usr/bin/env python3
"""Vendor the files seeds/<name>/vendor.json pins into seeds/<name>/site/.

Usage:
  seeds/vendor.py <name>            download every entry; refuse any byte that
                                    does not match its pinned sha256
  seeds/vendor.py <name> --refresh  download, then rewrite the sha256 fields —
                                    the version-bump path, after editing
                                    `version` and the URLs by hand

After a --refresh: run seeds/write-manifest.py <name>, then commit vendor.json,
manifest.json and the files together. Vendored files are byte-identical to
upstream; the hash is what makes the pin mean something.
"""
import hashlib
import json
import pathlib
import sys
import urllib.request


def main() -> None:
    args = sys.argv[1:]
    refresh = "--refresh" in args
    names = [a for a in args if not a.startswith("--")]
    if len(names) != 1:
        raise SystemExit(__doc__)
    seed = pathlib.Path(__file__).resolve().parent / names[0]
    pin_path = seed / "vendor.json"
    if not pin_path.is_file():
        raise SystemExit(f"{pin_path}: no such pin")
    pin = json.loads(pin_path.read_text())
    for entry in pin["files"]:
        with urllib.request.urlopen(entry["url"], timeout=60) as response:
            data = response.read()
        actual = hashlib.sha256(data).hexdigest()
        if refresh:
            entry["sha256"] = actual
        elif actual != entry["sha256"]:
            raise SystemExit(
                f"{entry['url']}: downloaded bytes hash to {actual}, vendor.json pins "
                f"{entry['sha256']}. Nothing written. Pass --refresh only for a deliberate bump."
            )
        target = seed / "site" / entry["path"]
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        print(f"site/{entry['path']}: {len(data)} bytes, sha256 {actual}", file=sys.stderr)
    if refresh:
        pin_path.write_text(json.dumps(pin, indent=2) + "\n")
        print(f"{pin_path}: hashes rewritten — now run seeds/write-manifest.py {names[0]}", file=sys.stderr)


if __name__ == "__main__":
    main()
```

- [ ] **Step 3: Vendor the files**

```bash
mkdir -p examples/dev-sandbox/seeds/bootstrap/site
python3 examples/dev-sandbox/seeds/vendor.py bootstrap
ls -l examples/dev-sandbox/seeds/bootstrap/site/vendor/bootstrap/
```
Expected: three lines ending in the hashes from the pin, and files of 232111, 80496 and 1093 bytes.

- [ ] **Step 4: Negative check — a changed pin is refused, not rewritten**

```bash
sed -i 's/4620c84ad5ce8602ff65640ed6b7c8b78ebb9e036584f0ebc1ccc88206a4bb51/0000000000000000000000000000000000000000000000000000000000000000/' examples/dev-sandbox/seeds/bootstrap/vendor.json
python3 examples/dev-sandbox/seeds/vendor.py bootstrap; echo "exit=$?"
git checkout -- examples/dev-sandbox/seeds/bootstrap/vendor.json 2>/dev/null || sed -i 's/0000000000000000000000000000000000000000000000000000000000000000/4620c84ad5ce8602ff65640ed6b7c8b78ebb9e036584f0ebc1ccc88206a4bb51/' examples/dev-sandbox/seeds/bootstrap/vendor.json
```
Expected: `…/LICENSE: downloaded bytes hash to 4620c8…, vendor.json pins 000000…. Nothing written.` and `exit=1`.

- [ ] **Step 5: The check verifies pins**

In `examples/dev-sandbox/seeds/check-seeds.py`, add after `problems_for`:
```python
def vendor_problems(seed: pathlib.Path) -> list:
    """Every file vendor.json pins is present under site/ with the pinned bytes."""
    pin_path = seed / "vendor.json"
    if not pin_path.is_file():
        return []
    pin = json.loads(pin_path.read_text())
    problems = []
    for entry in pin["files"]:
        target = seed / "site" / entry["path"]
        if not target.is_file():
            problems.append(
                f"{seed.name}: vendor.json pins {entry['path']} but site/{entry['path']} is missing "
                f"— run seeds/vendor.py {seed.name}"
            )
        elif seedlib.sha256_hex(target.read_bytes()) != entry["sha256"]:
            problems.append(
                f"{seed.name}: site/{entry['path']} differs from the bytes vendor.json pins "
                f"({entry['url']}) — an edited vendored file is a bug; re-run seeds/vendor.py {seed.name}"
            )
    return problems
```
and in `main`'s loop change `found = problems_for(seed)` to `found = problems_for(seed) + vendor_problems(seed)`.

- [ ] **Step 6: Negative check — an edited vendored file fails the check**

(The bootstrap manifest does not exist yet; write a provisional one first so the seed is checkable.)
```bash
python3 examples/dev-sandbox/seeds/write-manifest.py bootstrap
python3 examples/dev-sandbox/seeds/check-seeds.py; echo "exit=$?"
echo '/* edited */' >> examples/dev-sandbox/seeds/bootstrap/site/vendor/bootstrap/bootstrap.min.css
python3 examples/dev-sandbox/seeds/check-seeds.py; echo "exit=$?"
python3 examples/dev-sandbox/seeds/vendor.py bootstrap && python3 examples/dev-sandbox/seeds/check-seeds.py; echo "exit=$?"
```
Expected: `exit=0`; then two problems (`differs from the bytes vendor.json pins` and the manifest's `site/vendor/bootstrap/bootstrap.min.css: manifest.json declares …`), `exit=1`; then `exit=0` again.

- [ ] **Step 7: Mark the vendored files**

Append to the repo root `.gitattributes` (create it if absent):
```
# Vendored, minified third-party files under a seed: not ours to diff or count.
examples/dev-sandbox/seeds/*/site/vendor/** -diff linguist-vendored
```

- [ ] **Step 8: Commit**

```bash
git add .gitattributes examples/dev-sandbox/seeds
git commit -m "dev-sandbox: vendor Bootstrap 5.3.8 into the bootstrap seed, pinned by hash

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: The welcome page, the prompt and the guide

**Files:**
- Create: `examples/dev-sandbox/seeds/bootstrap/site/index.html`
- Create: `examples/dev-sandbox/seeds/bootstrap/sandbox.json`
- Create: `examples/dev-sandbox/seeds/bootstrap/guide.md`
- Regenerate: `examples/dev-sandbox/seeds/bootstrap/manifest.json`

- [ ] **Step 1: `site/index.html`**

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>ImpressPress build sandbox</title>
  <link rel="stylesheet" href="/vendor/bootstrap/bootstrap.min.css" />
  <script src="/vendor/bootstrap/bootstrap.bundle.min.js" defer></script>
  <script src="/b/webmcp/webmcp.js" defer></script>
</head>
<body class="bg-body-tertiary">
  <nav class="navbar navbar-expand-lg bg-body border-bottom">
    <div class="container">
      <a class="navbar-brand fw-semibold" href="/">ImpressPress build sandbox</a>
      <a class="btn btn-primary" href="/b/auth/login?redirect=/b/dev">Open workspace</a>
    </div>
  </nav>

  <main class="container py-5">
    <section class="row align-items-center g-5 mb-5">
      <div class="col-lg-7">
        <h1 class="display-5 fw-bold">Build a website with your browser agent</h1>
        <p class="lead">
          This is a browser-local <strong>WebMCP</strong> sandbox seeded with Bootstrap 5.
          Open it in a browser whose agent supports WebMCP and the agent can build this
          site, compile Rust backend blocks and stock a shop — no server, no deploy.
        </p>
        <p>
          Every visitor gets their own instance: a service worker and an OPFS database
          created fresh in your browser on first load. Nothing you do here leaves the browser.
        </p>
      </div>
      <div class="col-lg-5">
        <div class="card shadow-sm">
          <div class="card-body">
            <h2 class="h5 card-title">Credentials</h2>
            <p class="small text-body-secondary">
              Public on purpose: this instance exists only in your browser.
            </p>
            <dl class="row mb-3">
              <dt class="col-4">Email</dt>
              <dd class="col-8"><code>admin@example.com</code></dd>
              <dt class="col-4">Password</dt>
              <dd class="col-8"><code>admin123</code></dd>
            </dl>
            <a class="btn btn-primary w-100" href="/b/auth/login?redirect=/b/dev">Open workspace</a>
          </div>
        </div>
      </div>
    </section>

    <section>
      <h2 class="h4 mb-3">What the agent can do here</h2>
      <div class="row g-4">
        <div class="col-md-6 col-lg-3">
          <div class="card h-100">
            <div class="card-body">
              <h3 class="h6 card-title">Build and publish this site</h3>
              <p class="card-text small text-body-secondary">Every write under <code>site/</code> goes live at once.</p>
            </div>
          </div>
        </div>
        <div class="col-md-6 col-lg-3">
          <div class="card h-100">
            <div class="card-body">
              <h3 class="h6 card-title">Write and compile Rust blocks</h3>
              <p class="card-text small text-body-secondary">Compiled in the browser, served under <code>/b/&lt;name&gt;/</code>.</p>
            </div>
          </div>
        </div>
        <div class="col-md-6 col-lg-3">
          <div class="card h-100">
            <div class="card-body">
              <h3 class="h6 card-title">Stock and configure a shop</h3>
              <p class="card-text small text-body-secondary">Products, offers and a storefront widget, through the <code>shop_*</code> tools.</p>
            </div>
          </div>
        </div>
        <div class="col-md-6 col-lg-3">
          <div class="card h-100">
            <div class="card-body">
              <h3 class="h6 card-title">Export the result</h3>
              <p class="card-text small text-body-secondary">One zip you can serve from any static host.</p>
            </div>
          </div>
        </div>
      </div>
    </section>
  </main>

  <footer class="container py-4 border-top small text-body-secondary">
    Seeded from the <strong>bootstrap</strong> template — Bootstrap 5.3.8, MIT
    (<a href="/vendor/bootstrap/LICENSE.txt">license</a>).
  </footer>
</body>
</html>
```

- [ ] **Step 2: `sandbox.json`**

```json
{
  "template": "bootstrap",
  "suggested_prompt": "Build me a small online shop for handmade ceramics. Read dev_read_reference's site_markdown first. Replace site/index.html with a Bootstrap page (the framework is already at /vendor/bootstrap/) that lists products from /b/products/catalog in a card grid and lets a visitor open one, using <impresspress-product> from /b/products/storefront.js, and include <script src=\"/b/webmcp/webmcp.js\" defer></script> in its <head> so a visitor's agent can use the shop's tools. Then create three products with shop_create_product, give each a published offer with shop_create_offer and shop_publish_offer, and set their status to active with shop_update_product. Show me the live site when you are done."
}
```

- [ ] **Step 3: `guide.md`**

```markdown
# Building the site in this sandbox

This sandbox seeded a site built on **Bootstrap 5.3.8**, vendored under
`site/vendor/bootstrap/` (stock build, nothing customised, MIT — see
`vendor/bootstrap/LICENSE.txt`). Keep that directory, and never
`dev_read_file` anything under it: a minified framework file is hundreds
of KiB of context that tells you nothing. The welcome page,
`site/index.html`, is a worked example of the whole framework in use.

## How `site/` works

- Every file under `site/` is published verbatim, and every write publishes
  a new generation immediately — there is no separate deploy.
- `site/index.html` is the entrypoint, served at `/`. A subdirectory is a
  route: `site/blog/index.html` serves at `/blog/`, `site/about.html` at
  `/about.html`.
- Read a file before overwriting it: `dev_write_file` takes the file's
  current `sha256` as `expected_sha256`; a new file takes `null`.
- `dev_rollback` republishes an earlier generation when something regresses.

## Page skeleton

Every page you write carries this `<head>`. No stylesheet of your own is
needed; add one only for what Bootstrap's utilities cannot express.

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Page title</title>
  <link rel="stylesheet" href="/vendor/bootstrap/bootstrap.min.css" />
  <script src="/vendor/bootstrap/bootstrap.bundle.min.js" defer></script>
  <script src="/b/webmcp/webmcp.js" defer></script>
</head>
```

`/b/webmcp/webmcp.js` gives a visitor's own browser agent the site's public
tools — the shop's, and any compiled block's agent tools. Without the tag a
visitor's agent sees a plain page.

## Bootstrap here

Prefer the components the framework already styles, in this order:

- Layout: `.container`, `.row` / `.col-*`, the spacing utilities (`py-5`,
  `mb-3`, `g-4`).
- Navigation: `.navbar` with `.navbar-brand`; a `.btn.btn-primary` for the
  main action.
- Content: `.card` / `.card-body` grids for products and features;
  `.display-5` and `.lead` for a hero; `.badge` for tags.
- Forms: `.form-control`, `.form-label`, `.form-select`, `.btn` — a form that
  posts to a block you compiled.
- Feedback: `.alert`, `.toast`; `.modal` and `.collapse` work because the
  bundle is loaded.
- Theme: add `data-bs-theme="dark"` on `<html>` for a dark site.

Bootstrap Icons are **not** vendored; use text, Unicode or an inline SVG.

## The shop

Products are managed with the `shop_*` tools on this page. A page reads them
through two public pieces:

- `GET /b/products/catalog` lists active products as JSON:
  `{"records": [...], "total_count": N, "page": 1, "page_size": M}`. Each
  record carries `id`, `name`, `slug`, `description`, `image_url`, `tags`,
  `category`, `currency`, `stock`, `metadata` and `fulfillment_kind`. Pass
  `?page=2` for the next page; `?page_size=` goes up to 100. Render the
  list into `.card`s with a small script:

```html
<div id="products" class="row g-4"></div>
<script>
  fetch('/b/products/catalog').then(r => r.json()).then(({ records }) => {
    document.getElementById('products').innerHTML = records.map(p => `
      <div class="col-md-4"><div class="card h-100">
        ${p.image_url ? `<img class="card-img-top" src="${p.image_url}" alt="">` : ''}
        <div class="card-body">
          <h3 class="h5 card-title">${p.name}</h3>
          <p class="card-text">${p.description}</p>
          <a class="btn btn-outline-primary" href="/product.html?id=${p.id}">View</a>
        </div>
      </div></div>`).join('');
  });
</script>
```

- `<impresspress-product product-id="…"></impresspress-product>` renders one
  product's price and buy button. Load
  `<script src="/b/products/storefront.js" defer></script>` once per page.
  Attributes: `product-id` (required), `presentation` (`hosted`, `embedded`
  or `payment_link`; default `hosted`), `api-base` (default: this origin),
  `credentials` (`same-origin`, `omit` or `include`).

A product appears in the catalog once `shop_update_product` sets
`status: "active"`; it can be bought once it has a published offer
(`shop_create_offer`, then `shop_publish_offer`).

## Calling a backend block from a page

A block you compiled serves under `/b/<name>/`. Call it with
`fetch('/b/<name>/…')` — same origin, so no CORS or credentials setup — and
send and read JSON. A newsletter form, for instance, `POST`s
`{"email": …}` to `/b/<name>/subscribe` and shows an `.alert-success`.

## What a write refuses

- A path outside `site/` or `blocks/<name>/`, a `..` segment, or a name that
  clashes with an existing file or directory.
- A file over 512 KiB; more than 2,000 files; more than 64 MiB of stored
  content in the workspace.
- A stale `expected_sha256`: the refusal carries the current hash, so
  re-read and retry.

## Workflow

1. `dev_status`, then this reference.
2. Read `site/index.html` (for its hash and as the example), then overwrite
   it with your page.
3. Add pages and assets with further writes; each write is one generation.
4. Stock the shop with `shop_*`, then check the live site at `/`.
5. `dev_export` when done.
```

- [ ] **Step 4: Generate the manifest and check**

```bash
python3 examples/dev-sandbox/seeds/write-manifest.py bootstrap
examples/dev-sandbox/build.sh --check
python3 -c "import json; m=json.load(open('examples/dev-sandbox/seeds/bootstrap/manifest.json')); print([e['path'] for e in m['site']], m['sandbox']['template'])"
```
Expected: check passes for both seeds; the last line prints `['index.html', 'vendor/bootstrap/LICENSE.txt', 'vendor/bootstrap/bootstrap.bundle.min.js', 'vendor/bootstrap/bootstrap.min.css'] bootstrap` (path order: uppercase `L` sorts before lowercase `b`).

- [ ] **Step 5: Commit**

```bash
git add examples/dev-sandbox/seeds/bootstrap
git commit -m "dev-sandbox: the bootstrap seed — welcome page, prompt and site guide

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: The e2e spec and the CI step

**Files:**
- Modify: `crates/impresspress-web/tests/e2e/fixtures/dev-sandbox.ts` (port constant)
- Create: `crates/impresspress-web/tests/e2e/dev-bootstrap.spec.ts`
- Modify: `.github/workflows/ci-shared.yml` (the `e2e-dev-sandbox` job around lines 1003–1045)

**Interfaces:**
- Consumes: `serveDirectory(dir, port)`, `bootServiceWorker(page)`, `loginAdmin(page)` from the fixtures; `dev_status.template` and `dev_read_reference.site_markdown` (Plan B).
- Produces: `BOOTSTRAP_PORT = 8097`; the env var `BOOTSTRAP_DIST`.

- [ ] **Step 1: The spec**

In `fixtures/dev-sandbox.ts`, after `SCENARIO_EXPORT_PORT`:
```ts
/** Where `dev-bootstrap.spec.ts` serves the bootstrap seed's own bundle. */
export const BOOTSTRAP_PORT = 8097;
```

Create `crates/impresspress-web/tests/e2e/dev-bootstrap.spec.ts`:
```ts
import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';
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
    await expect(page.locator('nav.navbar')).toBeVisible({ timeout: 60_000 });
    await expect(page.locator('h1')).toHaveText('Build a website with your browser agent');
    await expect(page.locator('body')).toContainText('Open workspace');

    // The vendored files serve from generation 0 with the types the manifest declares.
    const css = await page.evaluate(async () => {
      const r = await fetch('/vendor/bootstrap/bootstrap.min.css');
      return { status: r.status, type: r.headers.get('content-type'), length: (await r.text()).length };
    });
    expect(css).toEqual({ status: 200, type: 'text/css; charset=utf-8', length: 232111 });
    const js = await page.evaluate(async () => {
      const r = await fetch('/vendor/bootstrap/bootstrap.bundle.min.js');
      return { status: r.status, type: r.headers.get('content-type'), length: (await r.text()).length };
    });
    expect(js).toEqual({ status: 200, type: 'application/javascript; charset=utf-8', length: 80496 });

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
 * Drift guard: every `<impresspress-product>` attribute a guide names is one
 * the element reads. The guides' storefront paragraph has one line that
 * starts with `Attributes:`; the element reads its attributes with
 * `getAttribute("…")`.
 */
for (const seed of ['blank', 'bootstrap']) {
  test(`the ${seed} guide names only storefront attributes the element reads`, () => {
    const guide = readFileSync(repoFile(`examples/dev-sandbox/seeds/${seed}/guide.md`), 'utf8');
    const storefront = readFileSync(
      repoFile('crates/impresspress-core/src/blocks/products/assets/storefront.js'),
      'utf8',
    );
    const read = new Set([...storefront.matchAll(/getAttribute\("([a-z-]+)"\)/g)].map((m) => m[1]));
    const line = guide.split('\n').find((l) => l.trim().startsWith('Attributes:'));
    expect(line, 'the guide has an `Attributes:` line').toBeTruthy();
    // Attribute names on that line: the hyphenated ones, plus the two single-word
    // attributes; the backticked VALUES (`hosted`, `omit`, …) are neither.
    const named = [...line!.matchAll(/`([a-z-]+)`/g)]
      .map((m) => m[1])
      .filter((n) => n.includes('-') || ['presentation', 'credentials'].includes(n));
    expect(named.length).toBeGreaterThan(0);
    for (const name of named) {
      expect(read, `${name} is read by storefront.js`).toContain(name);
    }
  });
}
```
- [ ] **Step 2: Fail loudly without the env var**

```bash
cd crates/impresspress-web && npx playwright test --config=tests/playwright.config.ts tests/e2e/dev-bootstrap.spec.ts 2>&1 | grep -m1 BOOTSTRAP_DIST
```
Expected: the `BOOTSTRAP_DIST is not set — build the seed with …` message.

- [ ] **Step 3: The CI job builds the seed and runs the spec**

In `.github/workflows/ci-shared.yml`, in the first `e2e-dev-sandbox` job, insert before `- name: Build the dev-sandbox bundle` (around line 1003):
```yaml
      # The bootstrap seed's bundle, built first and moved aside so the blank
      # build below can take `dist/`. `dev-bootstrap.spec.ts` serves it on
      # its own port from inside the spec (`serveDirectory`), the way the
      # export specs serve an unpacked bundle, so nothing here serves it.
      - name: Build the bootstrap-seed bundle
        run: |
          BOOTSTRAP_DIST="$(examples/dev-sandbox/build.sh --seed bootstrap --out "$RUNNER_TEMP/dist-bootstrap" | tail -1)"
          echo "BOOTSTRAP_DIST=$BOOTSTRAP_DIST" >> "$GITHUB_ENV"
```
and add `tests/e2e/dev-bootstrap.spec.ts` to the end of the `npx playwright test …` list in `Run dev sandbox e2e (browser WASM, port 8082)`. `BOOTSTRAP_DIST` reaches that step through `GITHUB_ENV`.

- [ ] **Step 4: Commit**

```bash
git add crates/impresspress-web/tests/e2e .github/workflows/ci-shared.yml
git commit -m "e2e: boot the bootstrap seed and pin its guide to the storefront element

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: Run it locally, then the PR

- [ ] **Step 1: Build both seeds**

```bash
(cd crates/impresspress-web && wasm-pack build --target web --release --out-dir pkg -- --locked)
cargo install --path crates/impresspress --locked --root ./out
examples/dev-sandbox/compiler/fetch-dist.sh
IMPRESSPRESS=./out/bin/impresspress examples/dev-sandbox/build.sh --seed bootstrap --out /tmp/dist-bootstrap | tail -1
ls /tmp/dist-bootstrap/seed /tmp/dist-bootstrap/seed/site/vendor/bootstrap
```
Expected: `guide.md  manifest.json  site` and the three vendored files.

- [ ] **Step 2: Run the spec**

```bash
(cd crates/impresspress-web && npm ci && npx playwright install chromium && BOOTSTRAP_DIST=/tmp/dist-bootstrap npx playwright test --config=tests/playwright.config.ts tests/e2e/dev-bootstrap.spec.ts)
```
Expected: `3 passed` (the boot test and the two drift guards). The config's `TEST_PORT` default is irrelevant here; the spec serves and navigates its own port.

- [ ] **Step 3: PR**

```bash
git push -u origin HEAD
gh pr create --title "dev-sandbox: the bootstrap seed" --body "$(cat <<'EOF'
Plan C of the build-sandboxes design (docs/superpowers/specs/2026-09-30-build-sandboxes-design.md §5.4, §7, §12).

- `seeds/bootstrap/`: Bootstrap 5.3.8 vendored byte-for-byte and pinned by sha256 in `vendor.json` (`seeds/vendor.py`, `--refresh` is the bump path; `check-seeds.py` re-verifies the pins), a Bootstrap-built welcome page, a suggested prompt and a site guide for the framework and the shop pieces.
- `dev-bootstrap.spec.ts` boots the seed's own bundle (served on 8097 from inside the spec) and pins both guides' storefront attributes to `storefront.js`.
- CI's `e2e-dev-sandbox` job builds the seed with `--out` and runs the spec.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```
