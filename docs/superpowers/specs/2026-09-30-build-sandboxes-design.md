# Build sandboxes — template-seeded dev sandboxes, and the path to them from impresspress.org

**Status:** design, agreed in conversation on 2026-09-30. Extends the dev
sandbox design (`2026-09-02-dev-sandbox-design.md`); nothing there is
superseded. An implementation plan follows this document.

## 1. Goal

A visitor tells a WebMCP-capable browser agent "build me a website on
impresspress.org". The agent reads impresspress.org, learns where to go,
opens a sandbox that already carries a CSS framework and instructions, and
builds a site that looks finished — without authoring a stylesheet and
without discovering the runtime's frontend pieces by trial and error.

Where the time goes today (measured 2026-09-29/30 during the dev-block
compile-speed work, PRs #101–#106): once those land, a warm block compile is
2–3 s and a site write is about 100 ms. The remaining cost of building a
site is the agent's own output — every line of HTML and CSS written from
scratch, the catalog API, the storefront element and `webmcp.js` discovered
by probing, one tool round trip per file. The seed gives the agent two files
and no guidance, and `dev_read_reference` documents Rust blocks only.

This design removes that cost by making a **template** the sandbox's seed,
and by telling agents on impresspress.org where the sandboxes are.

## 2. Decisions

Agreed in the 2026-09-30 brainstorm and recorded here so they are not
re-litigated.

1. **Bootstrap 5 is the default template.** Two drop-in files, no build
   step, MIT, and the class vocabulary every model already produces
   correctly. Pico CSS (classless) was the runner-up and was rejected as the
   default because a component vocabulary matters more here than 20 KiB of
   size. Astro, Svelte and every other build-time framework are out: the
   sandbox publishes `site/` verbatim and has no JS toolchain.
2. **A template is a seed plus a deployment.**
   `build-<template>.impresspress.org` is the existing sandbox bundle built
   with a different seed directory and deployed as its own Worker. There is
   no runtime template registry, no fetch of template files at runtime, no
   apply tool and no template config var — an earlier registry design was
   strictly more machinery for the same result.
3. **Nothing is downloaded on the agent's behalf, and the agent downloads
   nothing.** Every template file is in the seed at generation 0. An
   instruction such as "download Bootstrap and write it into the site" would
   push hundreds of KiB of base64 through the model per file.
4. **Template assets are vendored into the seed's `site/`, not linked from a
   CDN.** Offline-safe, pinned, and the export stays self-contained. The
   runtime's CSP already allows `cdn.jsdelivr.net` for scripts, so a CDN
   link would work; it is not what we ship.
5. **The template's instructions ride the seed** and are served by
   `dev_read_reference`, because the welcome page is the first thing the
   agent overwrites.
6. **`dev.impresspress.org` stays the blank template.** The current docs
   and e2e tests remain valid; `build-<name>` is for everything else.
7. **Stable default entry point: `impresspress.org/build`** redirects to the
   current default template's sandbox. `llms.txt` and the docs point at the
   stable path, so changing the default later is one line in the site repo.
8. **The first version is site-only.** No template carries Rust blocks, and
   the two block templates (`hello`, `table`) are unchanged.
9. **No WebMCP tool on impresspress.org.** A link plus an instruction is
   enough for a browser agent.
10. **Sequencing.** PRs #101–#106 (compile profile, pre-warm, guest crate,
    site-edit speed A/B/C) land first; this work branches from `main` after
    them. They edit `reference.md`, `docs/dev-sandbox.md` and the compiler
    README, which this work also edits.

## 3. What exists and what this reuses

- `examples/dev-sandbox/seed/{manifest.json, site/index.html, site/styles.css}`
  is overlaid wholesale onto `dist/seed/` by `[[assets.overlay]]` in
  `impresspress.toml`, and `impresspress-core::blocks::dev::seed` imports it
  as generation 0 on a fresh origin. Every file's sha256, size and content
  type are verified. The content type is checked against
  `paths::content_type_for(path)` — the runtime's own extension table — not
  against what the static host sends, so a vendored file only needs an
  extension that table knows (`.css`, `.js`, `.txt`, …).
- `build.sh` verifies the seed manifest (`--check`: every declared file
  exists with the declared hash and size, and every file is declared),
  builds the `browser-devtools` wasm, and assembles `dist/`. CI's two
  `e2e-dev-sandbox` jobs and the deploy workflow all use it.
- `dev_read_reference` returns `{ wafer_guest_version, markdown, wafer_guest_module }`,
  where `markdown` is `templates/reference.md` with both block templates
  spliced in at render time.
- `/b/dev` (`page.rs`) renders a static "How this workspace works" section
  and a `SUGGESTED_PROMPT` constant in a "Suggested prompt" disclosure.
- `dev_write_file` writes one file; a `site/` write publishes one generation
  through `activation::request(…, ActivationIntent::SiteOnly)`.
- The runtime already ships what a site needs: `/b/webmcp/webmcp.js`,
  `/b/products/storefront.js` (which defines `<impresspress-product>` with
  the attributes `product-id`, `api-base`, `presentation`, `credentials`
  and `payment-link-id`), and `/b/products/catalog`.
- impresspress.org is a Vite + Preact + Tailwind site (`impresspress/site`
  on GitHub, not in this workspace) hosted on Cloudflare. `/llms.txt`,
  `/llms-full.txt` and `/docs/<slug>.md` are generated at build time from
  `src/content/docs/*.md` and `DOCS_NAV` by `scripts/ai-files.mjs`; a docs
  page added there appears in `llms.txt` with no further work. `public/`
  holds only images today.
- Cloudflare: the sandbox is a static-assets Worker
  (`impresspress-dev-sandbox`, custom domain `dev.impresspress.org`).
  Deploys are currently manual, to `dev-test.impresspress.org` (decision of
  2026-09-26); the push-to-main deploy workflow lacks its secrets and is
  expected to fail. That situation is unchanged by this design.

## 4. Visitor flow

1. The visitor tells their browser agent "build me a website on
   impresspress.org".
2. The agent reads impresspress.org. The docs page "Build a website with an
   agent" — and therefore `llms.txt` — says: open
   `https://impresspress.org/build` in a WebMCP-capable Chromium browser,
   sign in with the credentials on the landing page, call `dev_status`, then
   `dev_read_reference`, and read its `site_markdown` before writing a file.
3. `/build` redirects to `https://build-bootstrap.impresspress.org`.
4. Generation 0 there is a Bootstrap-built welcome page showing the
   credentials and the "Open workspace" link, and `site/vendor/bootstrap/`
   already holds the framework. The agent overwrites `index.html`, adds
   pages, and keeps `vendor/`.
5. Everything after that is the existing loop (dev sandbox design §4.3):
   site writes, blocks, shop tools, export.

## 5. Seeds

### 5.1 Layout

```
examples/dev-sandbox/
  seeds/
    blank/
      sandbox.json        { "template": "blank", "suggested_prompt": "…" }
      guide.md            the site-authoring guide the reference serves for this template
      site/               index.html, styles.css  (today's seed/site, moved)
      manifest.json       GENERATED by write-manifest.py — never hand-edited
    bootstrap/
      sandbox.json
      guide.md
      vendor.json         upstream pins for every vendored file
      site/
        index.html
        vendor/bootstrap/bootstrap.min.css
        vendor/bootstrap/bootstrap.bundle.min.js
        vendor/bootstrap/LICENSE.txt
      manifest.json       GENERATED
    write-manifest.py     seeds/<name>/{sandbox.json, guide.md, site/**} -> manifest.json
    vendor.py             downloads seeds/<name>/vendor.json entries and verifies their sha256
  seed/                   BUILD OUTPUT, gitignored: the seed being built, staged for the overlay
```

`seed/` stays the overlay source in `impresspress.toml` (`from = "seed"`):
`[[assets.overlay]]` takes no parameters and the CLI has no environment
substitution for it, so `build.sh --seed NAME` copies
`seeds/NAME/manifest.json`, `guide.md` and `site/**` into `seed/` before
assembling. Only what the manifest names is staged — `sandbox.json` and
`vendor.json` are source, not bundle.

### 5.2 Manifest schema (additive)

`SeedManifest` gains one optional field. `SCHEMA_VERSION` stays 1: every
existing bundle still parses, and an exported bundle, which never carries the
field, still imports.

```json
"sandbox": {
  "template": "bootstrap",
  "suggested_prompt": "Build me a small online shop …",
  "guide": {
    "path": "guide.md",
    "sha256": "…",
    "size": 12345,
    "content_type": "text/markdown; charset=utf-8"
  }
}
```

- `template` matches `^[a-z][a-z0-9-]{0,31}$`.
- `guide` is a `SeedFile` relative to `/seed/`, fetched and verified like
  `data.json` (hash, size, content type), capped at 256 KiB. Its content
  type is fixed to `text/markdown; charset=utf-8`; the guide is not a
  workspace file and never goes through `content_type_for`.
- `suggested_prompt` is capped at 4 KiB. It is one paragraph the page shows
  verbatim.
- `export.rs` writes `sandbox: None`, with a comment saying why: an exported
  bundle boots with the workspace off (no `/b/dev`), so a guide there would
  describe tools the bundle does not have.

### 5.3 Generator and checks

- `write-manifest.py <name>` walks `seeds/<name>/site/**`, computes sha256
  and size, and assigns `content_type` from the same extension table as
  `paths::content_type_for` (`.html`, `.css`, `.js`/`.mjs`, `.txt`, `.json`,
  `.svg`, the image types). The table is duplicated deliberately, each copy
  commenting where the other lives; drift between them is caught at boot,
  because the import refuses the file, and therefore by the e2e job that
  boots the seed. A file whose extension the table does not know is refused
  by the generator rather than emitted as `application/octet-stream`.
  When `seeds/<name>/sandbox.json` exists, it also writes the `sandbox`
  block from that file and `guide.md`; without it, no `sandbox` block is
  emitted, which is how PR A ships before the runtime knows the field.
- `build.sh --check` runs over **every** directory under `seeds/`: forward
  (each manifest entry exists with the declared hash and size), reverse
  (each file is declared), the `sandbox` block matches `sandbox.json` and
  `guide.md`, and — where `vendor.json` exists — every vendored file matches
  its pinned sha256. `--check` stays cheap and builds nothing.
- `vendor.py <name>` downloads every `vendor.json` entry into `site/<path>`
  and verifies the pinned sha256. `vendor.py <name> --refresh` is the bump
  path: after the version and URLs are edited, it downloads, rewrites the
  hashes in `vendor.json`, and the caller re-runs `write-manifest.py` and
  commits all three. Without `--refresh`, a downloaded file that does not
  match its pin is an error, not a rewrite.

### 5.4 The Bootstrap pin

`vendor.json` for the bootstrap seed names Bootstrap 5.3.8 (released
2026-08-26, MIT) and three files fetched from the npm package on jsDelivr:
`dist/css/bootstrap.min.css` (232,111 bytes), `dist/js/bootstrap.bundle.min.js`
(80,496 bytes) and `LICENSE` (stored as `LICENSE.txt` so its content type is
`text/plain`). Both code files are well under `MAX_FILE_BYTES` (512 KiB).
Each entry carries `path`, `url` and `sha256`; the hashes are computed from
the downloaded bytes when the pin is created and committed with it.

The vendored files are byte-identical to upstream. `bootstrap.min.css` ends
with a `sourceMappingURL` comment pointing at a map we do not ship; that is a
404 in devtools only and is left alone so the pin means something. The
vendored path carries no version (`vendor/bootstrap/…`), so markup written
against it stays valid across bumps.

### 5.5 `build.sh`

- `--seed NAME`, default `blank`. Stages `seeds/NAME` into `seed/`, then
  runs the existing steps. The last stdout line remains the `dist/` path.
- `--check` behaves as in §5.3.
- Every reference to `examples/dev-sandbox/seed/manifest.json` — the e2e
  spec `dev-foundations.spec.ts`, the example README, `docs/dev-sandbox.md`,
  the CI comments — moves to `seeds/blank/manifest.json`.
- `examples/dev-sandbox/.gitignore` gains `seed/`.

## 6. Runtime changes (`impresspress-core::blocks::dev`)

### 6.1 Seed info is stored at import

A new single-row table `impresspress__dev__seed_info` (migration
`003_seed_info`, sqlite and postgres) with `singleton_id`, `template`,
`suggested_prompt`, `guide_markdown` and `imported_at`. Module
`repo/seed_info.rs` owns `pub const TABLE`, following `repo/runtime_state.rs`.
`seed::import` writes the row inside `import_bundle`, after the guide has
been fetched and verified and before generation 0 is activated, so a bundle
whose guide fails verification is refused as a whole, like any other file.
The row exists only on an instance whose seed carried a `sandbox` block;
nothing reads it on an exported bundle.

### 6.2 `dev_read_reference`

`ReferenceResponse` gains `template: Option<String>` and
`site_markdown: Option<String>`. `markdown` (the Rust guide) is unchanged.
The tool description becomes: "`markdown` is the backend-block guide;
`site_markdown` is this sandbox's site-authoring guide — the CSS framework it
ships, the page skeleton, the storefront element, the catalog API. Read
`site_markdown` before writing under `site/`."

### 6.3 `dev_status`

`StatusResponse` gains `template: Option<String>`, so an agent can tell
which sandbox it is in without reading the reference.

### 6.4 `/b/dev` page

- The "Suggested prompt" disclosure renders the row's `suggested_prompt`.
  The `SUGGESTED_PROMPT` constant moves, verbatim, into
  `seeds/blank/sandbox.json`. With no row, the disclosure is omitted.
- The "How this workspace works" section names the template and says that
  `dev_read_reference` returns two guides.
- `dev.js` is unchanged; every id it looks up stays.

### 6.5 Batch writes: `dev_write_files`

`POST /b/dev/api/files/write-batch`, tool `dev_write_files`.

Request: `{ "files": [ { "path", "content", "encoding"?, "expected_sha256"? }, … ] }`
with 1–64 entries, no duplicate paths, every entry under the same area —
all under `site/`, or all under one `blocks/<name>/`. Mixing areas is
refused: a site batch publishes, a block batch only stages, and one call
must mean one thing.

Semantics: every `expected_sha256` is checked before any byte is stored. A
mismatch anywhere refuses the whole call with `409`, listing every
conflicting path and the hash it actually has. Per-file and quota limits
apply exactly as for a single write, evaluated over the whole batch. Blobs
are stored, the workspace manifest is saved once, and then — for `site/` —
one generation is published with `GenerationCause::SiteWrite`. The response
is `{ files: [FileEntry…], generation, progress }`, the same shape as the
single write. `dev_write_file` stays as it is.

Why: a scaffold is five or six files. Today that is five or six generations,
iframe reloads and agent round trips.

## 7. The bootstrap seed's content

### 7.1 Welcome page (`site/index.html`)

The same purpose as today's welcome page (dev sandbox design §4.1), built
with Bootstrap: a navbar with the sandbox name, a hero saying what this is
and showing the credentials, the "Open workspace" button
(`/b/auth/login?redirect=/b/dev`), a card row for what the agent can do, a
footer. It links `/vendor/bootstrap/bootstrap.min.css`, loads
`/vendor/bootstrap/bootstrap.bundle.min.js` and `/b/webmcp/webmcp.js`, and
ships no stylesheet of our own: the page proves that the framework alone is
enough, and it is the one worked example the guide points at.

### 7.2 Guide (`guide.md`)

Sections, in order:

1. What this sandbox is: `site/` publishes verbatim; `index.html` is the
   entrypoint; a subdirectory is a route (`site/blog/index.html` serves at
   `/blog/`); keep `vendor/`, and never `dev_read_file` anything under it
   (a minified framework file is hundreds of KiB of context for nothing).
2. Page skeleton: the exact `<head>` to copy — viewport, the stylesheet,
   the bundle, `webmcp.js`.
3. Bootstrap here: the version, that it is the stock build with nothing
   customised, the vendored license, and the components to prefer (navbar,
   container and grid, cards, forms, alerts, modals via the bundle).
4. The shop pieces: `/b/products/catalog` with its request and response
   shape, taken from the handler's contract; `<impresspress-product>` and
   its attributes; `/b/webmcp/webmcp.js` and why the tag matters.
5. Talking to a block from a page: `fetch('/b/<name>/…')`, same-origin,
   JSON.
6. What a site write refuses: path rules, size limits and quotas, as in
   `docs/dev-sandbox.md` "Known limits".
7. Workflow: `dev_write_files` for a scaffold, one generation per write
   otherwise, `dev_rollback` when something regresses.

The blank seed's `guide.md` is the same document without section 2's
framework lines and without section 3 — the knowledge that today lives only
in the suggested prompt.

### 7.3 Drift guards

- The new e2e spec (§11) reads `guide.md` and `storefront.js` from the repo
  and asserts that every `<impresspress-product>` attribute the guide names
  is one `storefront.js` reads with `getAttribute`.
- The same spec proves generation 0 serves the vendored files with the
  content types the manifest declares.

## 8. Deployment

### 8.1 Workers and domains

| Seed      | Worker                        | Domain                            |
|-----------|-------------------------------|-----------------------------------|
| blank     | `impresspress-dev-sandbox` (exists) | `dev.impresspress.org`      |
| bootstrap | `impresspress-build-bootstrap`      | `build-bootstrap.impresspress.org` |

`wrangler.toml` keeps its top-level (blank) configuration and adds
`[env.bootstrap]` with its own `name` and `routes`. `[assets] directory = "./dist"`
is shared, so a deploy is always "build this seed, then deploy this
environment", never two seeds from one `dist/`. Whether `[assets]` is
inherited by a wrangler environment is verified in the plan with
`wrangler deploy --dry-run --env bootstrap`; if it is not, it is repeated
under the environment.

Manual deploy, the current practice: `build.sh --seed bootstrap`, then
`wrangler deploy --env bootstrap`. The dev-test procedure gains the same
flag.

### 8.2 Workflow

`deploy-dev-sandbox.yml` becomes a matrix over `seed: [blank, bootstrap]`.
Each job fetches the published compiler dist, runs `build.sh --seed`, and
deploys its environment. The path filter needs nothing new:
`examples/dev-sandbox/**` already covers `seeds/`. Its missing secrets are
out of scope, as they are today.

### 8.3 One-time setup, by hand like the existing one

The custom domain `build-bootstrap.impresspress.org` attached to the new
Worker. Creating the Worker and the domain is a new cloud resource: the
user does it, or gives an explicit go-ahead at that step.

## 9. impresspress.org (`impresspress/site`)

- A new docs page `src/content/docs/build-a-website.md`, its `DOCS_NAV`
  entry, `docs/build-a-website/index.html`, the page component and the Vite
  input, following the existing per-page pattern. Content: what a build
  sandbox is, the browser requirement, a table of the sandboxes (blank at
  `dev.impresspress.org`, bootstrap at `build-bootstrap.impresspress.org`)
  with the default marked, the credentials, the first three tool calls, and
  that everything stays in the visitor's browser. The generator puts it in
  `llms.txt` and at `/docs/build-a-website.md`.
- One sentence in the `llms.txt` intro: "To build a website with a browser
  agent, start at https://impresspress.org/build."
- `public/_redirects` with `/build https://build-bootstrap.impresspress.org 302`
  and `/templates /docs/build-a-website 301`. Cloudflare Pages and Workers
  static assets both honour `_redirects`; the plan verifies it on the live
  host and falls back to a zone redirect rule if the host does not.
- Ordering: this PR lands after `build-bootstrap.impresspress.org` answers,
  so the link never 404s.

## 10. Security

- No new network path. The seed is same-origin static content, verified
  as today. Bootstrap runs under the existing CSP: `style-src 'self'`,
  `script-src 'self'`, `img-src data:` for its inline SVG icons. It loads no
  fonts.
- The guide is text stored in the sandbox database and returned to the
  admin's agent; nothing renders it as HTML. The page shows the suggested
  prompt inside `<pre>`, escaped by Maud.
- A batch write obeys every rule of a single write, and the all-or-nothing
  conflict check means a stale agent cannot half-apply a scaffold.
- A vendored file is pinned by hash in `vendor.json` and again in the
  manifest; `--check` and the boot import both refuse a changed byte.

## 11. Verification

Unit tests (Rust, `--features block-dev`):

- `SeedManifest` parses with and without `sandbox`; an over-cap guide, a bad
  template name and a guide with the wrong content type are refused with
  messages naming the field.
- The import stores the seed-info row; `dev_read_reference` returns
  `site_markdown` and `template`; `dev_status` returns `template`; the page
  renders the row's prompt and omits the disclosure without a row.
- `dev_write_files`: one generation for N site files; mixed areas refused;
  any conflict refuses everything and lists every conflict; limits are
  evaluated over the batch; a `blocks/` batch stages without publishing.
- The export writes `sandbox: null`.

e2e (Playwright, in both CI jobs):

- The existing specs run against `build.sh --seed blank` unchanged apart
  from the manifest path.
- A new `dev-bootstrap.spec.ts` against `build.sh --seed bootstrap`:
  generation 0 serves `/vendor/bootstrap/bootstrap.min.css` as
  `text/css; charset=utf-8` and the bundle as
  `application/javascript; charset=utf-8`; the welcome page has a
  Bootstrap-classed navbar; `dev_read_reference` returns a `site_markdown`
  naming Bootstrap 5.3.8; `dev_status.template` is `"bootstrap"`;
  `dev_write_files` with three files yields exactly one new generation; the
  guide's storefront attributes exist in `storefront.js` (§7.3).

Build: `build.sh --check` covers both seeds in CI (an existing step), and a
re-run of `vendor.py bootstrap` produces no diff.

Site repo: `npm test` covers the generator, and the built `llms.txt` lists
the new page.

## 12. Phasing — PRs, producer first

In `impresspress`:

- **A. Seeds layout.** Move `seed/` to `seeds/blank/`, add
  `write-manifest.py`, `build.sh --seed` and the widened `--check`, the
  gitignore entry and every path update. No runtime change; no seed has a
  `sandbox.json` yet, so no manifest carries a `sandbox` block and the blank
  bundle is unchanged.
- **B. Runtime.** The `sandbox` manifest field, the seed-info table and
  import step, `dev_read_reference`, `dev_status` and the page; the blank
  seed's `sandbox.json` and `guide.md`; the prompt constant removed from
  `page.rs`.
- **C. Bootstrap seed.** `vendor.json`, `vendor.py`, the vendored files, the
  welcome page, the guide, and `dev-bootstrap.spec.ts`.
- **D. `dev_write_files`.** Independent of B and C; can run in parallel
  with them.
- **E. Deployment and docs.** The wrangler environment, the workflow
  matrix, the example README, `docs/dev-sandbox.md` (a "Templates"
  section), and the dev-test procedure.

In `impresspress/site`:

- **F.** The docs page, the `llms.txt` sentence and `_redirects`, after E is
  deployed and `build-bootstrap.impresspress.org` answers.

## 13. Non-goals

- Switching templates inside a workspace.
- Templates that carry blocks or data.
- A template registry or a `dev_apply_template` tool.
- A customised Bootstrap build, or component CSS of our own.
- WebMCP tools on the marketing site.

## 14. Open items

- Creating the `impresspress-build-bootstrap` Worker and its custom domain
  needs a go-ahead at deploy time.
- Whether wrangler environments inherit `[assets]` — verified in the plan.
- Whether impresspress.org's host honours `public/_redirects` — verified in
  the plan.
