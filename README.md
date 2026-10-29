# Impresspress

## Run Impresspress locally

Install the pinned Rust toolchain (`rust-toolchain.toml` names the version, components and WASM targets) and the build tools, then build the web assets and native binary from the repository root:

```sh
rustup toolchain install
cargo install just
cargo install wasm-pack --version 0.15.0
just build-debug
```

Start Impresspress with a first-run administrator account:

```sh
WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL=admin@example.com \
WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD=admin123 \
IMPRESSPRESS_LISTEN=127.0.0.1:8090 \
./target/debug/impresspress serve --target native
```

Open <http://127.0.0.1:8090/b/auth/login> and sign in with `admin@example.com` and `admin123`. Local data is stored under `data/` by default.

The email and password are read only on the first start, while there are no accounts: they create the admin, and are ignored after that. Set the email without the password (or a `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_TOKEN`) and that first start refuses to run.

The same variables can live in a `.env` file in the directory you start from (or the file `IMPRESSPRESS_ENV_FILE` names); a variable already exported wins. `serve --port` replaces the port of `IMPRESSPRESS_LISTEN` and keeps its host.

Every start applies the database's pending block migrations before the server takes a request, and refuses to start if a block fails to initialize or if the `.env` file cannot be read or has a line it cannot parse (quote a value that contains spaces: `KEY="two words"`). Before 1.0 an upgrade may require wiping `data/`; a change to auth's migrations signs every user out.

To pepper password hashes with a key kept outside the database, export
`IMPRESSPRESS_PASSWORD_PEPPER_KEY` (generate one with `openssl rand -base64 32`
and back it up: losing it locks out every account it peppered). The key is
read from the process environment only and is never stored or served as
config. `IMPRESSPRESS_PASSWORD_PEPPER_PREVIOUS_KEYS` (rotation) and
`IMPRESSPRESS_PASSWORD_PEPPER_REQUIRED` (`true`/`false`) complete it; read
RELEASE.md's "password pepper" upgrade note before setting either. On
Cloudflare, passwords are hashed by a separate password-hasher Worker, and the
pepper is that Worker's secret: see RELEASE.md's "password-hasher Worker" note.

If you commit from this checkout, point git at the repository's hooks once so formatting and clippy run the same way CI does (this needs the nightly toolchain's rustfmt: `rustup toolchain install nightly --component rustfmt`):

```sh
git config core.hooksPath .githooks
```

### Serving your own site

A repo's own static site is `frontend/build/` if it exists, else `public/`.
Every server target puts it in the storage folder `wafer-run/web/site`, which
the `wafer-run/web` block serves for any path no block claims: `impresspress
build`/`serve --target native` copy it to `data/storage/wafer-run/web/site/`,
and `--target cloudflare` uploads it to R2 under that folder.

`/` itself is the router's, not the site's: it redirects to the login page
(or the user portal when signed in) unless `WAFER_RUN_SHARED__HAS_LANDING_PAGE`
is `true`, in which case it serves the site's `index.html`. It defaults to
`false` on every target. Set it like any shared variable, on the admin's
Variables page (`/b/admin/settings/variables`) on any target, or natively
in the environment (`.env`) as well.

### Running the tests

```sh
cargo test --workspace --exclude impresspress-web --exclude impresspress-cloudflare
cargo test -p impresspress-cloudflare --target wasm32-unknown-unknown --lib   # needs wasm-bindgen-test-runner
cargo test -p impresspress-password --features durable-object --target wasm32-unknown-unknown --lib
NODE_OPTIONS=--import\ ./js/test/node-hooks.mjs scripts/wasm-pack-locked.sh crates/impresspress-browser test --node
```

The browser suite needs that `NODE_OPTIONS` import, which CI sets as a job
env. `crates/impresspress-browser/js/bridge.js` carries a top-level
`import initSqlJs from '/vendor/sql-wasm-esm.js'` — an absolute site-root path
a Service Worker resolves and plain Node cannot — and the hook stubs that one
specifier. Without it the whole suite fails to load with `ERR_MODULE_NOT_FOUND`
before a single test runs. The path is relative to `crates/impresspress-browser`,
which is where `wasm-pack` runs `cargo test` from, so do not export it for the
other suites.

## Try it without installing anything

`dev.impresspress.org` is a browser-local sandbox where a WebMCP-capable AI agent builds a site, writes backend blocks and stocks a shop entirely in your browser tab — nothing is installed, and nothing is deployed behind it. See [`docs/dev-sandbox.md`](docs/dev-sandbox.md).

## WebMCP

Every page Impresspress serves registers its tools with the browser's agent through the WebMCP API. The tools are not hand-written: each block declares typed HTTP endpoints, the runtime projects the ones marked as agent tools into a manifest at `/b/webmcp/manifest.json` (filtered to what the current visitor may call), and a small script turns each manifest entry into a `registerTool` call on every registry the browser provides — `navigator.modelContext` (Chrome) and `document.modelContext` (the OpenAI Codex / ChatGPT in-app browser), once if they are the same object:

```javascript
registry.registerTool({
  name: tool.name,                 // e.g. "search_products"
  description: tool.description,   // from the endpoint's doc comment
  inputSchema: tool.inputSchema,   // derived from the endpoint's typed request
  execute: async (input) => {      // one same-origin fetch of that endpoint
    const req = buildRequest(tool.invocation, input);
    const response = await fetch(req.url, req.init);
    return { content: [{ type: 'text', text: await response.text() }] };
  }
}, { signal });                    // aborting it takes the tool back
```

- [`crates/impresspress-core/src/ui/assets/webmcp-core.js`](crates/impresspress-core/src/ui/assets/webmcp-core.js) — `webmcpRegistrar` (finds the browser's registries, registers on each, takes the tools back), `buildRequest` and `toolOptions`: the shared half.
- [`crates/impresspress-core/src/ui/assets/webmcp.js`](crates/impresspress-core/src/ui/assets/webmcp.js) — registers the site's public tools on every page (also served at the stable `/b/webmcp/webmcp.js` for pages an agent writes itself).
- [`crates/impresspress-core/src/blocks/dev/assets/dev.js`](crates/impresspress-core/src/blocks/dev/assets/dev.js) — the dev sandbox's page-scoped `dev_*` / `shop_*` tools, registered only on `/b/dev`, plus `dev_compile_block` and `dev_export`.
- The manifest projection lives in wafer-run's `wafer_core::discovery` (`generate_webmcp_report`, `generate_webmcp_selected`); the sandbox's curated list is [`blocks/dev/tools.rs`](crates/impresspress-core/src/blocks/dev/tools.rs).
