# dev-sandbox

The bundle behind `dev.impresspress.org`: a browser-local WebMCP development
sandbox. `impresspress.toml` sets `[dev] enabled = true`, which turns on the
`impresspress/dev` block (`/b/dev`) and the service worker's seed-on-boot
import (`impresspress-core::blocks::dev::seed`). `seeds/blank/` is the
welcome starter site every fresh origin boots with — a generated
`manifest.json` plus `site/{index.html,styles.css}`. `build.sh --seed NAME`
stages `seeds/NAME/` into the gitignored `seed/`, which `[[assets.overlay]]`
copies onto `dist/seed/` wholesale; `seeds/write-manifest.py NAME` regenerates
a manifest after editing a seed's files.

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

1. Verifies every `seeds/*/manifest.json` against its `site/**`
   (`seeds/check-seeds.py`) — see `--check` below. Runs first so a stale
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
Sign in at `http://localhost:8080/b/auth/login?redirect=/b/dev` to reach the
workspace.

## Deploying

The bundle is deployed as a Cloudflare Worker, `impresspress-dev-sandbox`,
serving `dist/` as static assets with SPA fallback (`wrangler.toml`).
`worker.js` is a pass-through (`env.ASSETS.fetch(req)`) so response headers
can be added later without moving off static assets.

**One-time setup**, done by hand, not by any workflow:

1. `impresspress.org` added as a zone on the Cloudflare account these
   secrets belong to.
2. A custom domain `dev.impresspress.org` attached to the
   `impresspress-dev-sandbox` worker (`wrangler.toml`'s `routes` declares
   this; Cloudflare still needs the domain provisioned once against the
   zone).
3. Two repository secrets — `CLOUDFLARE_API_TOKEN` and
   `CLOUDFLARE_ACCOUNT_ID` — set on this repo for the
   [`deploy-dev-sandbox`](/.github/workflows/deploy-dev-sandbox.yml) workflow
   to use.

**Automatic deploys**: the `deploy-dev-sandbox` workflow runs on every push
to `main` that touches `examples/dev-sandbox/**`,
`crates/impresspress-web/**`, `crates/impresspress-core/**`,
`crates/impresspress-browser/**`, `crates/impresspress-bundle/**`, or the
workflow file itself, plus on manual `workflow_dispatch`. It builds this
bundle the same way `build.sh` does locally, then runs `wrangler deploy`.

**Manual deploy**, from a machine with `wrangler` logged in to the same
Cloudflare account:

```sh
examples/dev-sandbox/build.sh
cd examples/dev-sandbox && wrangler deploy
```

Live URL (once deployed): `https://dev.impresspress.org`

## Credentials

The seeded admin account (`WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL` /
`_PASSWORD`, seeded by every browser build — see
`crates/impresspress-web/src/config.rs`):

- Email: `admin@example.com`
- Password: `admin123`

This is a throwaway per-browser instance with no data of any consequence
behind it, which is why the credentials are public in the welcome page
itself.

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
