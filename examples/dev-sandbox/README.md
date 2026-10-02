# dev-sandbox

The bundle behind the build sandboxes — `dev.impresspress.org` and
`build-bootstrap.impresspress.org`: a browser-local WebMCP development
sandbox, one per seed. `impresspress.toml` sets `[dev] enabled = true`, which
turns on the `impresspress/dev` block (`/b/dev`) and the service worker's
seed-on-boot import (`impresspress-core::blocks::dev::seed`). What a fresh
origin boots with is a **seed** (see [Seeds](#seeds) below):
`build.sh --seed NAME` stages `seeds/NAME/` into the gitignored `seed/`,
which `[[assets.overlay]]` copies onto `dist/seed/` wholesale.

Every visitor who opens the deployed URL gets their **own** instance: a
service worker and an OPFS database created fresh in their browser on first
load. Nothing a visitor does — publishing a page, compiling a block, editing
the shop — reaches this repo, this build, or any other visitor. The only
thing every visitor shares is the seed bundle itself, which is static files
served by the host.

For what a visitor can do with the sandbox once it's deployed — the
workspace, backend blocks, stocking the shop, export, browser requirements —
see [`docs/dev-sandbox.md`](../../docs/dev-sandbox.md). This README covers
building, serving and deploying the bundle itself.

## Seeds

A seed is what a fresh origin boots with (generation 0). Each lives under
`seeds/<name>/`, and each is its own deployed sandbox:

| Seed        | Sandbox                                  | Ships |
|-------------|------------------------------------------|-------|
| `blank`     | https://dev.impresspress.org             | A minimal welcome page and stylesheet; the site guide without a framework |
| `bootstrap` | https://build-bootstrap.impresspress.org | Bootstrap 5.3.8 vendored under `site/vendor/bootstrap/`; a Bootstrap-built welcome page; the site guide for the framework and the shop pieces |

- `site/**` — the site files. `manifest.json` is **generated** from them by
  `seeds/write-manifest.py <name>`; run it after every edit and commit both.
- `sandbox.json` + `guide.md` — the template name, the prompt the workspace
  page suggests, and the site-authoring guide `dev_read_reference` serves as
  `site_markdown`. The generator puts them in the manifest's `sandbox` block.
- `llms.txt` — not a file in the seed: it is **generated** from
  `seeds/llms-preamble.md` (shared by every seed: what the sandbox is, that
  an agent is expected to build a site in it, how to get in, that a
  JavaScript-capable browser is required) followed by the seed's `guide.md`
  verbatim, so the building instructions are written once. The manifest's
  `sandbox.llms` declares its hash; `build.sh` writes the file. The bundle
  serves it twice: at `/llms.txt` from the static host, for a reader with no
  service worker, and at `/seed/llms.txt`, which the seed importer verifies
  and records. From then on the runtime answers `/llms.txt` — with this text
  while the site has no `site/llms.txt`, and with the site's own once it
  does. It is never a service-worker bypass and never part of an export.
  After editing `llms-preamble.md`, regenerate **every** seed's manifest.
- `../boot-notice.html` (shared) — the two sentences the boot page shows
  under its title (`[app] boot_notice` in `impresspress.toml`): the only
  text a reader gets before the service worker exists.
- `vendor.json` (bootstrap) — upstream URLs and sha256 pins of the vendored
  files. `seeds/vendor.py bootstrap` downloads and verifies them; the
  vendored bytes are identical to upstream.
- `seeds/check-seeds.py` — what `build.sh --check` runs: every manifest
  equals what the generator would write, and every vendored file matches its
  pin.

**Bumping Bootstrap:** edit `version` and the three URLs in
`seeds/bootstrap/vendor.json`, and the version the guide and the welcome
page name (`seeds/bootstrap/guide.md`, `seeds/bootstrap/site/index.html`),
then, from `examples/dev-sandbox/`:

```sh
seeds/vendor.py bootstrap --refresh      # downloads, rewrites the sha256 pins
seeds/write-manifest.py bootstrap        # regenerates the manifest
./build.sh --check                       # proves the three agree
```

`crates/impresspress-web/tests/e2e/dev-bootstrap.spec.ts` asserts the guide
names the version (`Bootstrap 5.3.8`, a literal) — change it with the rest.
Commit `vendor.json`, `manifest.json`, the vendored files and those edits
together.

## Build

```sh
cargo install --path crates/impresspress --locked --root ./out   # a current CLI
IMPRESSPRESS=./out/bin/impresspress examples/dev-sandbox/build.sh
```

`build.sh` assembles the bundle with whatever `impresspress` is on `PATH`
unless `IMPRESSPRESS` names one. Install first: a stale binary from an older
checkout (a `~/.cargo/bin/impresspress` left over from months ago, say) builds
without the recursive-directory overlay `[[assets.overlay]]` needs, so
`dist/seed/` never appears — the script's own sanity check catches that and
says so, but the fix is a fresh CLI, not a change to this directory.

This is the one recipe CI's `e2e-dev-sandbox` job and local e2e runs both use
(`crates/impresspress-web/tests/e2e/dev-foundations.spec.ts` and
`dev-workspace.spec.ts`). It:

1. Verifies every `seeds/*/manifest.json` against its `site/**`,
   `sandbox.json`, `guide.md` and generated `llms.txt`
   (`seeds/check-seeds.py`) — see `--check`
   below. Runs first so a stale
   manifest fails fast rather than paying for a wasm build before finding out
   the bundle cannot seed itself.
2. Builds `impresspress-web` to wasm with `--features browser-devtools` into
   `crates/impresspress-web/pkg-dev` (this is what puts the `/b/dev`
   control-plane code in the binary at all — `[dev] enabled` alone only wires
   the service-worker plumbing around it).
3. Runs `impresspress build --target web --release` from this directory
   (`IMPRESSPRESS_WEB_PKG_DIR` pointed at `pkg-dev`) to assemble `dist/`.

Last line of stdout is the absolute path of the finished bundle (`dist/`, or
the `--out` directory).

With no `--seed`, `build.sh` builds `blank`.
`build.sh --seed NAME --out ../dist-NAME` builds another seed and moves the
bundle to that directory, so `dist/` stays free for the next one. `--out` is
relative to where you run the script and must be outside
`examples/dev-sandbox/`; the target must not exist, be empty, or be a bundle
this script made (`sw.js` beside `seed/manifest.json`) — anything else is
refused before anything is built, and a previous bundle there is replaced.

`examples/dev-sandbox/build.sh --check` runs step 1 — verifies every seed's
manifest against its files — and, when `compiler/dist/` has been built, checks
that tree against `compiler/dist/manifest.json` and Cloudflare's asset limit;
it exits non-zero on drift and builds nothing either way. Run
`seeds/write-manifest.py <name>` after editing a seed's files, then this; a
manifest that has drifted from the files it describes is exactly what
`seed::import` refuses at runtime (a fresh origin would fail to boot).
`build.sh`'s normal path runs the same check first, so a stale manifest fails
the build fast rather than shipping a bundle that cannot seed itself.

## Serve locally

```sh
IMPRESSPRESS=./out/bin/impresspress examples/dev-sandbox/build.sh
python3 -m http.server 8080 -d examples/dev-sandbox/dist
```

Open `http://localhost:8080/` — the welcome page (generation 0, seeded).
Its "Open workspace" link (`http://localhost:8080/b/dev/enter`) signs you in
as the seeded admin and opens the workspace.

## Deploying

Each seed is its own Cloudflare Worker serving `dist/` as static assets with
SPA fallback, all from the same `wrangler.toml`: the blank seed is the
top-level config (`impresspress-dev-sandbox`, `dev.impresspress.org`); every
other seed is the wrangler *environment* of the same name (`[env.bootstrap]`
→ `impresspress-build-bootstrap`, `build-bootstrap.impresspress.org`), which
inherits `main`, `compatibility_date` and `[assets]` from the top level.
`worker.js` is a pass-through (`env.ASSETS.fetch(req)`) so response headers
can be added later without moving off static assets.

Because every Worker serves the same `./dist`, a deploy is always the build
and the deploy of ONE seed, back to back — never two builds and then two
deploys.

**One-time setup**, done by hand, not by any workflow:

1. `impresspress.org` added as a zone on the Cloudflare account these
   secrets belong to. This is the only thing that must exist beforehand.
2. The first `wrangler deploy` of the top-level config (the manual deploy
   below). It creates the `impresspress-dev-sandbox` Worker *and* attaches
   `dev.impresspress.org` to it: `wrangler deploy` attaches every route
   marked `custom_domain = true` in `wrangler.toml` itself, against the
   zone from step 1. Nothing is provisioned by hand in the dashboard.
3. Two repository secrets — `CLOUDFLARE_API_TOKEN` and
   `CLOUDFLARE_ACCOUNT_ID` — set on this repo for the
   [`deploy-dev-sandbox`](/.github/workflows/deploy-dev-sandbox.yml) workflow
   to use. Without them the workflow cannot deploy anything.
4. The first `wrangler deploy --env bootstrap`, the same way as step 2: it
   creates the `impresspress-build-bootstrap` Worker and attaches
   `build-bootstrap.impresspress.org`. Adding a seed's environment is
   therefore a resource-creating step — run it by hand, deliberately, before
   the workflow's job for that seed gets the chance to. The first deploy of
   a new Worker must be `wrangler deploy` (not `versions upload`).

**Automatic deploys**: the `deploy-dev-sandbox` workflow runs on every push
to `main` that touches one of the paths its `paths:` filter lists (this
directory, the crates the bundle and the CLI are built from, the workspace
manifest and lockfile, and the workflow file itself), plus on manual
`workflow_dispatch`. It runs once per seed as a matrix: each job builds its
seed with `build.sh --seed <seed>`, then runs `wrangler deploy` (blank) or
`wrangler deploy --env <seed>` (every other seed). One seed failing does not
cancel the other.

**Manual deploy**, from a machine with `wrangler` logged in to the same
Cloudflare account — the bootstrap sandbox:

```sh
examples/dev-sandbox/build.sh --seed bootstrap
cd examples/dev-sandbox && wrangler deploy --env bootstrap
```

and the blank one:

```sh
examples/dev-sandbox/build.sh
cd examples/dev-sandbox && wrangler deploy
```

(With an environment defined, a bare `wrangler deploy` warns that no target
environment was given; it still deploys the top-level config, which is the
blank sandbox.)

Live URLs (once deployed): `https://dev.impresspress.org` (blank),
`https://build-bootstrap.impresspress.org` (bootstrap)

## Credentials

The seeded admin account (`WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL` /
`_PASSWORD`, seeded by every browser build — see
`crates/impresspress-web/src/config.rs`).

The values are the browser build's defaults in that file; nothing here
repeats them, because an instance's password can be changed. A visitor does
not type them: the welcome page's **Open workspace** link goes to
`/b/dev/enter`, which signs in with whatever this instance is configured
with, and the workspace page shows the same pair for the login form. This is
a throwaway per-browser instance with no data of any consequence behind it,
which is why they are public at all.

## `seed/data.json` — read this before adding one to this bundle

`seed/manifest.json`'s `data` field can carry a `seed/data.json` snapshot —
rows for an explicit table allowlist, applied through typed database calls
by `impresspress_core::blocks::dev::{data_snapshot,seed}` (design §10.1,
amendments 9 and 17) — so an exported sandbox can carry its own products,
offers and admin account, not just its site and blocks. This bundle's own
welcome site carries none (`"data": null` in `seed/manifest.json`): the
welcome site has no shop data of its own, and its bootstrap admin comes from
`WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL`/`_PASSWORD` at build time,
not from a seeded row.

**`seed/**` is served by the static host as plain files, with no auth in
front of it** — that is what lets a fresh service worker fetch it before
anything else has booted. If a `data.json` is ever added to a seed
under `seeds/`, it will carry password hashes in a file anyone can `curl`.
Do not add one, or point one at a real account, without deciding how the
hash it carries is meant to be safe to publish (a disposable/rotated one,
most likely) — "static file next to the site" is not a place to put a real
credential. This is exactly what an export's own `seed/data.json` does
carry (the exporting visitor's own throwaway sandbox account) — see
[`docs/dev-sandbox.md`](../../docs/dev-sandbox.md#export).
