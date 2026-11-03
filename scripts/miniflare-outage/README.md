# Miniflare D1-outage harness

End-to-end scenarios for `impresspress-cloudflare`'s D1 outage handling on the
real `worker::Env` path: the fixture Worker (`fixture/`) is built and packaged
by the CLI's own deploy code (`crates/impresspress/examples/miniflare_deploy.rs`),
then served by Miniflare over a real local D1 that a control Worker can put into
an outage. `harness.mjs` says how; each scenario's header says what it covers.

CI runs it as the `miniflare-outage` job in `.github/workflows/ci-shared.yml`.

## Running it locally

Prerequisites:

- Node 22 and `npm ci` here (exact pins, committed lockfile).
- The impresspress-web pkg at `crates/impresspress-web/pkg/`: the deploy driver
  is an example of the `impresspress` crate, whose build script embeds it.
  `scripts/wasm-pack-locked.sh crates/impresspress-web build --target web --release --out-dir pkg`
- worker-build at exactly the CLI's pinned version (`WORKER_BUILD_VERSION` in
  `crates/impresspress/src/cli/helpers/cloudflare/build.rs`), first on `PATH`.
  The harness refuses any other version, because the CLI's build would otherwise
  install it globally. Install it somewhere of its own:
  `cargo install worker-build --version =<pin> --locked --root <dir>`, then put
  `<dir>/bin` first on `PATH`.

Then, from this directory:

```sh
node run.mjs                       # every scenario
node run.mjs stale-plan dynamic-path   # the named ones
```

Environment:

- `MINIFLARE_OUTAGE_STATE=<dir>`: keep the run's state (local D1, KV, R2, the
  staged release) in `<dir>`, emptied first. Unset, each run uses a fresh
  temporary directory, removed when every scenario passes.
- `MINIFLARE_OUTAGE_VERBOSE=1`: echo the Worker's log.
- `CARGO_TARGET_DIR` is honoured by the build, as for any cargo build.

A run writes only under `fixture/` (worker-build's `build/`, the CLI's staging
under `target/`), the cargo target directory and the state directory, apart
from worker-build's own download cache under `~/.cache/worker-build`.

## Adding a scenario

A module under `scenarios/` exporting `{ name, shape, run(deployment) }`
(`shape`: `wafer-site` or `impresspress-site`), listed in `scenarios/index.mjs`.
`run` gets a fresh `Deployment` (`harness.mjs`): `deploy()`, `start()`/`restart()`
for cold isolates, `setMode()` for the outage, `hold()`/`release()` to keep D1
calls in flight, `fetchCounted()` for a request with its D1 and KV calls,
`logLine()` for the Worker's console, and `smoke()` for the CLI's
pre-promotion smoke. It fails by throwing.
