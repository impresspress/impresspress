# {{TITLE}}

An ImpressPress site, exported from the development sandbox on
{{DATE}} — generation `{{GENERATION_ID}}`.

Everything here runs in your browser. There is no server to deploy, no
database to provision and nothing to configure: the runtime is a WebAssembly
service worker, the database is SQLite compiled to wasm, and the files it
serves live in this folder.

## Serve it

A static file server is all it needs — but it must be *served*, not opened
from the filesystem: a service worker cannot be registered from a `file://`
URL.

    python3 -m http.server 8000
    # or
    npx serve -l 8000

Then open <http://localhost:8000/>. The first load compiles the runtime and
imports `seed/`, so it takes a few seconds; every load after that is instant.

Open it on `localhost`, as above, or over `https`. Browsers run a service
worker only on a secure page, so the same folder served at
`http://192.168.1.20:8000/` on your network shows a page saying so and
nothing else; put it behind https to share it.

## What is in here

    index.html loader.js sw.js *.js *.wasm snippets/ vendor/
        The runtime shell — {{SHELL_FILES}} files, copied from the sandbox
        that exported this. Development mode is OFF in this copy
        (`const DEV_ENABLED = false;` in `sw.js`), so there is no `/b/dev`
        workspace, no in-browser compiler and no agent tooling here. This is
        the site, not the sandbox that built it.

    llms.txt
        Only if your site has one (`seed/site/llms.txt`): the same file,
        placed where your static host serves it to a reader that runs no
        JavaScript. Counted with the shell above. If you edit one, edit
        both — and the hash in `seed/manifest.json`.

    seed/manifest.json
        What the runtime imports on its first boot: every file below, with
        its SHA-256 and size. The runtime verifies all of them and refuses
        the whole import if any disagrees — so editing a file under `seed/`
        without updating its hash here stops the site from seeding at all.

    seed/site/**
        Your site's own files — {{SITE_FILES}} of them.

    seed/blocks/<name>.wasm
        Each compiled backend block ({{BLOCKS}} in total), plus its full Rust
        source under `seed/blocks/<name>/`. The source is included so this
        export can be edited and recompiled, not just run.

        The compiled artifact comes from the generation that is live; the
        source comes from the workspace as it stands, so the two can in
        principle disagree — if a block was edited and not recompiled before
        this export was taken. Per block, as checked at export time:

{{BLOCK_SOURCES}}

    seed/wafer_guest/**
        The guest SDK crate every block depends on by path
        (`wafer_guest = { path = "../../wafer_guest" }` in each block's
        `Cargo.toml`), present whenever there is at least one block. It is
        source for rebuilding blocks, not something the runtime imports, so
        `seed/manifest.json` does not list it. To rebuild a block with a Rust
        toolchain on your own machine, run
        `cargo build --release --target wasm32-wasip1` inside
        `seed/blocks/<name>/`; the SDK is the only dependency, so no crate
        registry is needed. The module lands at
        `seed/blocks/<name>/target/wasm32-wasip1/release/<crate>.wasm`, where
        `<crate>` is the block's name with hyphens spelled as underscores
        (`my-shop` builds `my_shop.wasm`). Putting it back means copying it
        over `seed/blocks/<name>.wasm` and updating that block's
        `spec.artifact_sha256` in `seed/manifest.json` to the new file's SHA-256 —
        the rule for `seed/manifest.json` above holds for anything you put
        back under `seed/`, edited sources included.

    seed/data.json
        A snapshot of the data the sandbox held: products, offers and their
        components, product groups, types and presets, non-sensitive site
        settings, and the user accounts — {{TABLE_ROWS}} rows in total.

## What is *not* in here, and what is

Deliberately left out: sessions, refresh and verification tokens, the audit
log, purchases, refunds, payment links, provider operations, Stripe events
and webhook leases, and every setting marked sensitive. Stripe linkage on the
exported products and offers is reset to "not synced" — the ids belonged to
the exporting instance's Stripe account, not to yours.

Deliberately included, and worth knowing about: **`seed/data.json` carries
your user accounts, including their password hashes.** They are yours — you
created them in your own browser-local sandbox — and they are in here so that
signing in to this copy works with the same credentials. They are
PBKDF2-HMAC-SHA256 hashes (600,000 iterations), not plaintext — the sandbox
runs in a browser, where Argon2id is too slow — but treat this folder the way
you would treat any export of an account table: do not publish it anywhere you
would not publish a password database.

**Change the admin password before serving this anywhere but localhost.** Sign
in at `/b/auth/login` as `{{ADMIN_EMAIL}}`, then change it on
`/b/userportal/security`. The sandbox seeds every instance with the same
starter password and the documentation prints it, so until you change it,
anyone who gets this folder — or who can reach the host you serve it from — is
an admin of this site. That was fine in the sandbox, which is a throwaway
instance in one browser; it stops being fine the moment this folder leaves it.

## Re-importing it

`seed/` is exactly the format the sandbox itself reads. Drop this folder's
`seed/` directory into another ImpressPress bundle and its first boot will
import the same site, blocks and data — that is how this export was produced
and how it is read back. Nothing about it is export-only.
