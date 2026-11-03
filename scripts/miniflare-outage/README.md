# Miniflare D1-outage harness

End-to-end scenarios for `impresspress-cloudflare`'s D1 outage handling on the
real `worker::Env` path: the fixture Worker (`fixture/`) is built and packaged
by the CLI's own deploy code (`crates/impresspress/examples/miniflare_deploy.rs`),
then served by Miniflare over a real local D1 that a control Worker can put into
an outage. `harness.mjs` says how; each scenario's header says what it covers.

What is real and what is not:

- `DB` is Miniflare's real local D1 behind a JavaScript wrapper with the
  binding's surface (`workers/d1-outage.mjs`). Before every call it asks the
  control Worker (`workers/d1-control.mjs`) whether the call fails (the outage
  modes) or waits (`hold()`), and the control Worker counts it with its SQL and
  bound values.
- `CONFIG_CACHE` is likewise Miniflare's real local KV behind a counting
  JavaScript wrapper (`workers/kv-ledger.mjs`), not the native KV binding. Each
  call reports to the control Worker: one extra round trip per call, the same
  trade as `DB`.
- `STORAGE` (R2) is Miniflare's native local R2, seeded with what the deploy's
  own upload code wrote.
- The Worker's console (workerd's stdout and stderr) is captured line by line.
  Scenarios find a refused request's cause under the reference its body names
  (`correlation_id=<ref>`), and the retry window a runtime armed.
- The password-hasher Worker is not run, because no scenario hashes a password.

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
(`shape`: one of the fixture's three, below), listed in `scenarios/index.mjs`:

- `wafer-site`: a content block over the release's `dist/`, with impresspress
  behind `/b/**` and `/health`, as wafer.run deploys it;
- `impresspress-site`: impresspress as an ordinary site;
- `wafer-site-init-reader`: `wafer-site` plus `miniflare/init-reader`, a block
  whose `Init` sends D1 a statement of its own (no block impresspress ships
  does), so the `init-reset` mode can fail that block alone.

`run` gets a fresh `Deployment` (`harness.mjs`): `deploy()`, `start()`/`restart()`
for cold isolates, `setMode()` for the outage, `hold()`/`release()` to keep D1
calls in flight, `fetchCounted()` for a request with its D1 and KV calls,
`logLine()` for the Worker's console, and `smoke()` for the CLI's
pre-promotion smoke. It fails by throwing.

Count runtime builds from the `d1_cost` lines (`buildsSince()` in
`scenarios/common.mjs`), never from D1 calls: since the D1 gate, a retry
over a failing D1 sends the same failed read and probe a build did
(`probedBackend()`) and assembles nothing (`build=gate`).
