# The dev sandbox's compiler

`dev.impresspress.org` compiles agent-written Rust blocks **in the browser**.
The compiler is [Rubrc](https://github.com/oligamiq/rubrc) — rustc, cargo and
LLVM built to wasm and composed into one component — packaged here as
versioned static assets and wrapped in a message protocol the `/b/dev` page
speaks.

Nothing in this directory is a fork. `build-compiler.sh` checks out rubrc at
the commit `PIN.json` names, builds its component with rubrc's own recipe, and
bundles **our** worker against two of its source trees. The pieces we wrote are
`src/worker-entry.ts` (the protocol, the WASI farm, the artifact capture),
`src/vfs-runner.ts` (rubrc's `util_cmd.ts` with the UI taken out) and the
scripts. Everything else is rubrc's, at its own licence: MIT OR Apache-2.0.
The toolchain wasm it embeds inherits rustc's, cargo's and rust-analyzer's
MIT OR Apache-2.0 — except `llvm_opt.wasm`, which is a build of LLVM and is
therefore **Apache-2.0 WITH LLVM-exception**. `PIN.json`'s `licenses` records
each of them; `dist/manifest.json`'s single `license` field reports the
package as a whole.

## Layout

```
compiler/
  PIN.json                       every input: rubrc's commit, the composer, Binaryen, the sysroot
  src/ansi.ts                    ANSI stripping for the shell transcript
  build-compiler.sh              PIN.json -> dist/<version>/            (~55 min cold, 6-50 s incremental, ~9 min on a version bump)
  pack-dist.sh                   a built dist/ -> the release asset       ("Publishing the compiler")
  fetch-dist.sh                  the release asset -> dist/              (what a deploy runs)
  src/protocol.ts                the page <-> worker contract
  src/worker-entry.ts            the worker the page creates
  src/vfs-runner.ts              the worker that runs the toolchain component
  src/probe.html                 the verification page (see "What was confirmed")
  scripts/prepare-vfs-asset.mjs  brotli + split the composed wasm
  scripts/write-manifest.mjs     dist/manifest.json
  scripts/verify-compiler-assets.mjs   what `build.sh --check` runs
  scripts/compose-decision.sh    whether phase 3 may reuse the component on disk
  scripts/test-build-kind.sh     that decision, over a fake tree            (`bash` test)
  scripts/serve-probe.mjs        the probe's server, with the sandbox's COOP/COEP
  scripts/run-probe.mjs          runs the probe headlessly and prints the numbers
  dist/                          gitignored build output, overlaid at /__impresspress_dev/compiler/
  .rubrc/ .cache/ node_modules/  gitignored build inputs
```

`dist/` is not committed: it is 72 MB (365 MiB before compression), and it is
fully determined by `PIN.json`. A deploy gets it from a **release asset** the
developer publishes for that pin (see "Publishing the compiler"); the CI jobs
that only test whether the compiler still works try the same asset and fall
back to a cache keyed on the pin, and then to a `--fast` composition.

**`dist/` holds exactly one version.** The whole directory is overlaid onto the
bundle, so a version directory left behind by a pin bump would be deployed
alongside the current one without anything having checked it.
`build-compiler.sh` removes the others and `verify-compiler-assets.mjs` fails
on anything under `dist/` that is not `manifest.json` or the version the
manifest names.

## Building

```bash
compiler/build-compiler.sh          # or: examples/dev-sandbox/build.sh, which calls it when stale
compiler/build-compiler.sh --fast   # local iteration only; see below
```

Needs node >= 22, rustup, curl and tar. It downloads what it needs (rubrc,
`wasi_virt_layer-cli`, Binaryen, the sysroot tarball), checks every download
against the sha256 in `PIN.json`, and caches each phase.

Measured on a 24-core box, and these are the real numbers, not estimates:

| | |
| --- | --- |
| everything, from an empty tree | **~55 minutes** |
| of which `wasm-opt -Oz` (phase 3) | ~35 minutes, peaking at **12.6 GB RSS** |
| of which brotli + split (phase 5) | 9 min 26 s |
| `src/**` changed, component unchanged | **6 s** warm, **~50 s** cold (page cache) |

**A 7 GB CI runner will be OOM-killed in phase 3.** The final `wasm-opt` pass
runs over a 410 MB merged module and wants 12.6 GB. `dist/` is fully
determined by `PIN.json`, so CI should cache it on that file's hash and never
build it; a machine that must build it needs a large runner.

That re-run is what it costs to change `src/worker-entry.ts`: the checkout, the
composition and the split are all skipped, and what is left is vite hashing and
copying the 365 MiB component into `dist/` and `prepare-vfs-asset.mjs` hashing
it and every part to prove the split on disk is a split of exactly those bytes.
Both figures were measured: 6.3 s with the component still in the page cache
from a previous run, 52.4 s reading it from disk.

`--fast` composes with rubrc's `vfs:build:prod:no-opt`, which skips `wasm-opt`
altogether: minutes instead of ~35, at the price of a much larger component —
more parts, more download, more memory in the browser. It is for iterating on
the packaging itself. `dist/manifest.json` records `"build": "fast"` and
`verify-compiler-assets.mjs` refuses that, so `build.sh --check` fails and a
`--fast` tree cannot reach a deploy by accident. (Implemented, not exercised —
see "Not confirmed".)

Which kind a component IS is recorded beside it, in `.build-kind`, at the
moment it is composed, and the manifest reports that rather than what the
current run asked for. Phase 3 is skipped when `vfs.core.wasm` is already
there, so `--fast` followed by a plain run would otherwise stamp
`"build": "full"` on a component nobody optimized — and the verifier, which is
the only thing keeping `--fast` out of a deploy, would accept it. A plain run
over a `fast` component recomposes; over a component composed before
`.build-kind` existed it stops and says which one line to write, rather than
spending 35 minutes to find out. `scripts/test-build-kind.sh` is that whole
decision table, run against a fake tree in under a second.

The one exception is deliberate and explicit:

```bash
IMPRESSPRESS_COMPILER_ALLOW_FAST=1 examples/dev-sandbox/build.sh --check
```

**Only the CI job that tests whether the compiler still works may set that**,
so it can build with `--fast` instead of paying 55 minutes for an answer it
does not need optimised. The verifier prints a warning saying the tree must
not be deployed, and everything else about the check still applies. **The
deploy workflow never sets it** — a `--fast` component is a compiler nobody
optimised, and the whole point of `dist/` is that what was verified is what
ships. `build.sh` passes the environment through untouched, which is the only
reason this works; there is no flag to plumb.

Two things the script insists on that are easy to get wrong by hand:

* **Binaryen is pinned to an upstream release.** `wasi_virt_layer` passes
  `--enable-shared-everything`, which Binaryen <= 116 rejects — including the
  0.116.1 it vendors — and npm's `binaryen` package is a JS port that is ~13x
  slower here and runs into node's heap ceiling on the 94 MB LLVM module.
* **`wasm32-wasip1-threads` on both toolchains, plus `rust-src` on nightly.**
  The composition builds rubrc's VFS crate with `-Zbuild-std=std,panic_unwind`.

### Why the sysroot is vendored

Rubrc fetches its standard library at runtime from
`https://oligamiq.github.io/rust_wasm/v0.2.0/<triple>.tar.br`
(`lib/src/sysroot.ts`). A sandbox whose compiler's standard library comes from
a third-party host is a supply chain we do not control, so `build-compiler.sh`
downloads that tarball at build time, checks its sha256 against `PIN.json`,
and vendors it into `dist/<version>/sysroot/`. `worker-entry.ts` answers the
component's `sysrootStartFetch` bridge call from there — the browser makes no
cross-origin request at all.

### Why the component is split

Parts are capped at 24 MiB (25 165 824 bytes) — deliberately under
Cloudflare's 25 MiB (26 214 400 byte) static-asset limit, which nothing here
has ever observed from a real upload — and the composed component is 365 MiB
(the four toolchain modules going in are ~230 MB; single-memory lowering grows
them). `prepare-vfs-asset.mjs` brotli-compresses it and splits it into `vfs.core-<hash>.wasm.br.part-NNN` beside a
`vfs.core-<hash>.wasm.br.json` describing them; `vfs-runner.ts` fetches the
parts in order, pipes them through a brotli decoder into
`WebAssembly.compileStreaming`, and caches the compiled module in IndexedDB so
the second visit skips the download. The manifest shape is rubrc's own, so
their loader and ours read the same files.

That manifest also carries a `sha256` of the component and of **each part**.
Those exist for the build, not the browser: a rebuild reuses an existing split
only after hashing every part and finding what it expects, because a part
corrupted in place keeps its size, survives being set aside across the vite
build, and would then be hashed into `dist/manifest.json` as though it were
correct — the manifest would agree with the disk, and the disk would be wrong.
At runtime the reassembled stream is checked by brotli itself and against
`originalSize`.

### `v1-dist`

Rubrc's `prepare-vfs-asset.mjs` fetches a `v1-dist` branch, so the first thing
this task checked was whether that branch already carries a composed and split
component we could pin instead of composing. It does not: `v1-dist`
(`a8521e69d5eb5369d897022bf38b8d0627fb4c98`, "Preserve previous deployment as
v1") is a snapshot of the old *page* — Monaco, xterm, their chunks — with no
`vfs.core-*` file in it at all. So we compose, from the pinned sources, with
pinned tools.

## Publishing the compiler

**The deploy does not build the compiler. It downloads the one you built.**

`build-compiler.sh` without `--fast` is ~55 minutes and peaks at 12.6 GB of
RSS. No GitHub-hosted runner has that, so `deploy-dev-sandbox.yml` runs
`fetch-dist.sh` instead, which pulls a release asset and fails — loudly, and
with these instructions — when there is none for the pinned version. Publish
one whenever `PIN.json` moves, and before the pin bump reaches `main`:

```bash
compiler/build-compiler.sh    # once, on a machine with the memory
compiler/pack-dist.sh         # verifies, tars, and prints the command below
gh release create compiler-<version> .cache/compiler-dist-<version>.tar \
  --title 'Compiler dist <version>' --notes '…'
```

`<version>` is `PIN.json`'s `version` — rubrc's commit at eight characters,
then a packaging revision (see "Updating the pin") — so the tag, the asset and
the tree inside it all move together and a checkout can only ever be handed
the toolchain its own pin asks for. An existing
release takes `gh release upload <tag> <asset> --clobber` instead.

Both halves refuse a `--fast` tree, and refuse it directly rather than
through `IMPRESSPRESS_COMPILER_ALLOW_FAST`: the correctness-CI jobs export
that variable for their whole job and they run `fetch-dist.sh` too, so a check
that honoured it would be no check at all there. `pack-dist.sh` will not pack
a `"build": "fast"` manifest and `fetch-dist.sh` will not accept one.

`fetch-dist.sh` uses `gh` when it is installed and `curl` otherwise, unpacks
into an emptied `dist/`, and runs `verify-compiler-assets.mjs` over what
landed — every file against the manifest's sha256, nothing over the 24 MiB
per-part cap, and the manifest's pin against `PIN.json`'s. It is a no-op when
`dist/` already holds a verified tree for the pin, so calling it twice costs
one hash pass.

## The protocol

`src/protocol.ts` is the contract and the types are the documentation; this is
the shape of a session. The page creates the worker from
`manifest.json`'s `entry` (`/__impresspress_dev/compiler/<version>/worker.js`,
`{ type: "module" }`) and then:

```
page → { type: 'init', id,
         guest: { files: { 'Cargo.toml': '…', 'src/lib.rs': '…' },          (optional)
                  warmup: { crateName: 'hello', files: { … } } } }
     ← { type: 'progress', id, stage: 'download', loaded, total }   (repeatedly)
     ← { type: 'progress', id, stage: 'initializing', detail }      (including the warm-up build)
     ← { type: 'ready', id, rustcVersion }

page → { type: 'compile', id, crateName, files: { 'Cargo.toml': '…', 'src/lib.rs': '…' },
         target: 'wasm32-wasip1', release: true }
     ← { type: 'progress', id, stage: 'compiling', detail }         (repeatedly)
     ← { type: 'result', id, success, artifact?, stdout, stderr, diagnostics, elapsedMs }

page → { type: 'cancel', id }
     ← { type: 'result', id, success: false, cancelled: true, … }
```

* **`guest` is built before `ready`.** The page hands over what
  `GET /b/dev/api/guest` returned — the `wafer_guest` crate and a block to
  build it with — and the worker builds that block once, which compiles the
  guest into `/target`, before it answers `ready`. A guest that does not build
  fails `init` (`{ type: 'error' }`, "the guest crate does not build on this
  toolchain: …"). Without `guest` the worker is ready as soon as the sysroot
  is loaded, and it still builds a self-contained crate.
* **One compile at a time.** A `compile` that arrives while another is in
  flight is answered with a failed `result`, not queued. That request is
  refused; the worker is fine.
* **`broken` is terminal.** A failed `init` and any `cancel` put the worker
  there and nothing takes it out. `compile` on a `broken` (or not yet `ready`)
  worker is answered `{ type: 'error' }` — the adapter's signal to
  `terminate()` and start a fresh one, rather than to retry.
* **A `cancel` with nothing in flight is refused, not obeyed.** It answers
  `{ type: 'error', id, message: 'nothing in flight' }` and changes no state:
  a double click, or a cancel that raced the result it meant to cancel, must
  not be able to brick a healthy worker.
* **The 120 s compile budget is the adapter's, not the worker's.** The
  worker's own 10-minute ceiling is a backstop for a shell that has wedged.
  The sandbox's promise is enforced page-side, by sending `cancel` and then
  terminating.
* **The artifact is transferred**, not copied: after `result`, the buffer
  belongs to the page.
* **`cancel` spends the worker.** Rubrc's shell runs a command on a session
  thread that nothing outside it can unwind, so the worker cannot abandon a
  compile in progress. It answers `{ cancelled: true }` and marks itself
  broken; **the adapter must `terminate()` it and `init` a fresh one.** That
  costs a re-instantiation, not a re-download — the compiled module is in
  IndexedDB.
* **`diagnostics`** are `{ file, line, column, severity, message, code? }`.
* **`stdout` and `stderr` are split by content, not by file descriptor**,
  because the guest's streams arrive already merged into one terminal
  transcript. `stderr` is the build as a human would have seen it: rustc's own
  `rendered` text for each diagnostic, then cargo's status output, then
  anything written to fd 2 outside the shell's stream. `stdout` is the rest of
  the session — `cargo clean -p <crate>`, `rm` and the `download` that reads
  the artifact out of the VFS. Cargo's `--message-format=json` protocol lines appear in
  neither: they are what `diagnostics` is made of, so `stdout` is not a wall
  of JSON.

### How a compile actually happens

Worth knowing before changing `worker-entry.ts`: the component is a *terminal*.
There is no API for "build this crate". The VFS is laid out like an export
archive — a block at `/blocks/<crate>/`, the guest SDK it depends on by path
(`../../wafer_guest`) at `/wafer_guest/` — and a compile is

1. each file written under `/blocks/<crate>/` through the VFS's write-file
   event (`input_string` with session `0xEEEEEEEE`, a JSON `{path, content}`),
2. `cargo clean -p <crate>` (same `--release`, `--manifest-path`,
   `--target-dir` and `--target`), which removes that one package's
   artifacts so cargo rebuilds it, and leaves `/wafer_guest`'s build alone —
   because cargo's mtime comparison cannot be trusted in this VFS (below),
3. `rm -f /target/wasm32-wasip1/release/<crate>.wasm` — the path step 5
   reads — so that a build that fails leaves nothing there (below),
4. `cargo build --release --manifest-path /blocks/<crate>/Cargo.toml
   --target-dir /target --target wasm32-wasip1 --message-format=json` typed
   into session 0 one code point at a time, and a wait for the shell's
   `<cwd> $ ` prompt to come back — the only completion signal there is,
5. `download /target/wasm32-wasip1/release/<crate>.wasm`, which streams the
   file back out through the host bridge as chunks.

**The warm-up.** `init` with a `guest` writes `/wafer_guest/**` and the
warm-up block, runs the same `cargo clean -p` and `rm` over the warm-up,
runs the same `cargo build`, and checks with `download` that the module is
there before it posts `ready`. That compiles `wafer_guest` into `/target`,
and every build in the session shares that one `--target-dir`, so a later
`compile` recompiles only the block: `cargo clean -p` names only the block's
package, and cargo keeps the guest's build (its `Fresh wafer_guest` line).
That is the whole speed-up —
the guest is most of what a block compiles, and it is compiled once per
worker instead of once per compile. Nothing is deleted between compiles; a
file an earlier compile left in `/blocks/<crate>/src/` is harmless, because
rustc only compiles what `lib.rs` reaches through `mod`, and every file the
block still has is rewritten.

**Cargo's freshness check is not trusted.** The VFS's write-file event does
not move a file's mtime, and the VFS's timestamps are nanosecond-scale
counters rather than times, so cargo's mtime comparison cannot be trusted:
a crate with no dependencies, edited, came back `Fresh` with the previous
module. A block that depends on `wafer_guest` rebuilds every time today only
because the counters make cargo think the guest was rebuilt ("the dependency
`wafer_guest` was rebuilt (… 325ns after last build at 0.000000001s)"),
which is an accident, not a mechanism. `cargo clean -p <crate>` is cargo's
own way to rebuild one package while keeping its dependencies, and it costs
~0.4 s a compile. **`touch` was tried on 2026-09-30 and does not work**: a
touched dependency-free crate still came back `Fresh` — whatever `touch`
sets in this VFS, cargo does not see it as newer than its last build. Do not
retry it. The probe's
dependency-free crate (`probe_fresh`: build answering 1, edit `src/lib.rs`
to answer 2, call it) is the regression test: with the clean taken out it
answered 1.

**Cargo's verdict is not trusted, and the previous module is removed.** On
this toolchain, cargo's `build-finished` success does not reflect a rustc
failure: a syntax error comes back with rustc's error rendered and then
cargo's `Finished` and `"build-finished", "success": true`. A build can also
fail before cargo emits any JSON and without a `-->` span the worker could
parse (a path dependency that is not there), which leaves no diagnostic at
all. With `/target` kept across compiles, either one would have `download`
hand back the PREVIOUS build's module as this compile's — a green build of
code nobody wrote. Two things stop that:

* **`rm -f` before every build** (the warm-up and each `compile`), so a
  build that fails for any reason leaves nothing at the path `download`
  reads, and "File not found" is answered `success: false` with an
  `artifact-missing` diagnostic and cargo's own output in `stderr` (and, for
  the warm-up, with `init` failing). The probe's missing-path-dependency
  compile and its no-diagnostic broken guest are the regression tests: with
  the `rm` taken out, the first came back `success: true` with the previous
  `hello.wasm` — with `cargo clean -p` in place too, since a manifest cargo
  cannot resolve fails the clean as well as the build.
* **An error diagnostic fails the build**, whatever `build-finished` says,
  so the answer carries rustc's diagnostics rather than only "artifact
  missing". The probe's syntax-error build is the test.

These, and `cargo clean -p`, are workarounds for rubrc: the root-cause
fixes (real file timestamps in the VFS, moved by the write-file event;
cargo's `build-finished` success reflecting rustc's failure) are upstream
and are candidates for the next pin bump.

The other structural surprise is the worker pair. The WASI *farm* services
calls for every thread of the guest and those threads block on `Atomics.wait`
until it does, so the farm cannot share a thread with the guest: `worker.js`
is the farm and the protocol, and it spawns `vfs-runner` for the guest, which
in turn spawns thread workers.

## What was confirmed

Run for real against `dist/`, by `scripts/run-probe.mjs` (which serves
`src/probe.html` with the sandbox's own
`Cross-Origin-Embedder-Policy: credentialless` and drives it in headless
chromium):

```bash
node scripts/run-probe.mjs
```

### 2026-09-03: the vendored-module era

Every line in this section is from a run on 2026-09-03 against
`dist/807ace9e` (rubrc `807ace9e`), chromium 146 headless, on a 24-core linux
box — when a block carried the whole guest SDK as a module (`wafer_guest.rs`)
and every compile started from a cleaned `/target`. The 2026-09-30 run below
supersedes its compile times; its other findings still hold. Its `compile`
and artifact rows were built with `lto = true`: a re-run on 2026-09-30
against the same dist on the same box, after the templates dropped LTO,
compiled the same vendored-module `hello` in 21 458 ms (cargo's own figure:
21.16 s) to a 111 130-byte artifact.

| | |
| --- | --- |
| `ready` (cold: nothing cached) | **11 329 ms** (7.1-11.9 s over five runs — it varies with what else the machine is doing) |
| `ready` (warm: component in IndexedDB) | **7 019 ms** (6.8-8.0 s) |
| `compile` of the `hello` template, release, `wasm32-wasip1` | **37 805 ms** (cargo's own figure: 37.57 s) |
| artifact | **88 892 bytes**, instantiates, exports the whole wafer ABI |
| `compile` of the same crate with a syntax error | 5 585 ms |
| total download to first `ready` | **75.1 MB** (13 files: 55.4 MB of component parts, 18.9 MB sysroot, 0.8 MB JS) |
| largest single file | **25 165 824 bytes** — `vfs.core-*.wasm.br.part-001`, exactly our 24 MiB cap |

1. **The worker starts from a same-origin module URL, and its subordinate
   workers resolve theirs after bundling.** `new Worker('./807ace9e/worker.js',
   { type: 'module' })` starts, spawns `vfs-runner`, and that spawns eight
   `thread_spawn` workers plus the background worker, all from hashed URLs
   vite emitted. Confirmed.
2. **`crossOriginIsolated === true` in the page and in a worker, under
   `Cross-Origin-Embedder-Policy: credentialless`** — the value the sandbox
   deploys, not rubrc's `require-corp`. `SharedArrayBuffer` is available in
   the worker. Confirmed.
3. **Machine-readable diagnostics, no regex needed.** `cargo build
   --message-format=json` works through the shell: the transcript carries
   `{"reason":"compiler-message",…}` and `{"reason":"build-finished",…}` lines,
   and the deliberate error came back as
   `{ file: "src/lib.rs", line: 46, column: 18, severity: "error", message: "expected `;`, found `value`" }`.
   The regex fallback in `worker-entry.ts` stays for the case where a build
   dies before cargo emits JSON (a malformed `Cargo.toml`), but it was not
   needed here. Confirmed.
4. **The release build of the std-only guest is 88 892 bytes and
   instantiates** (with `lto = true`; 111 130 bytes without it on
   2026-09-30), exporting the whole wafer ABI — the probe asserts all five
   of `__wafer_alloc`, `__wafer_info`, `__wafer_handle`, `__wafer_lifecycle`
   and `__wafer_host_codec` rather than printing what it found, because a
   module that links but is missing one is not a block and the sandbox would
   only discover that at activation. Under the 200 KB the design assumed and
   well under the sandbox's 4 MiB limit. Confirmed.
5. **Sizes.** Largest file 25 165 824 bytes (a part, at our 24 MiB cap); total
   75.1 MB, of which 55.4 MB is the component's three brotli parts (365.3 MiB
   of wasm compressed to 52.8 MiB), 18.9 MB the vendored sysroot and 0.8 MB
   the JS. Confirmed.
6. **A stray `cancel` does not brick the worker.** `cancel` with nothing in
   flight is answered `{ type: 'error', message: 'nothing in flight' }`, and
   the compile the probe runs immediately afterwards still succeeds — which is
   the actual proof, since a broken worker would answer `error` there instead
   of compiling. Confirmed.
7. **`stdout` and `stderr` carry what they claim.** On the failing build,
   `stderr` begins with rustc's own rendering (``error: expected `;`, found
   `value` `` with the source excerpt and the `help:` line) and continues with
   cargo's status output; `stdout` holds the shell's own lines and the
   `download`. Neither contains a byte of cargo's JSON. Confirmed.
8. **Times.** Cold 11.3 s, warm 7.0 s — but cold ranged 7.1-11.9 s across
   five runs while warm stayed 6.8-8.0 s, so the honest reading is that on
   localhost the two are close and the cache buys little. The download is not
   what costs: instantiating a 365 MiB module and streaming the sysroot
   tarball into the VFS is, and the IndexedDB path only skips the fetch and
   `compileStreaming`. Over a real network the gap should open up; do not
   promise users a fast second visit on the strength of these numbers.

Two things this run also settled, neither of them predicted:

* **Cargo's freshness check cannot be trusted here.** The VFS's write-file
  event replaces a file's contents without moving its mtime, so the second
  compile of an edited crate came back `"fresh": true` with the *first*
  build's artifact — a green build of code nobody wrote. The worker then
  emptied `/target` before every build; since 2026-09-30 it runs
  `cargo clean -p <crate>` instead, which rebuilds the block and keeps the
  guest's build (`touch` was tried and does not work; see "How a compile
  actually happens"). The root-cause fix is real file timestamps in rubrc's
  VFS, upstream, and belongs to the next pin bump.
* **Vite rewrites `new URL(`./x/${v}`, import.meta.url)` into a build-time
  glob lookup.** Ours resolved to `undefined` because `sysroot/` does not
  exist until the build vendors it. `worker-entry.ts` reads `import.meta.url`
  through a variable to keep that resolution at runtime; do not "simplify"
  it back.

### 2026-09-30: the guest built once per session

`node scripts/run-probe.mjs 8095` against `dist/807ace9e.2` (rubrc
`807ace9e`, packaging revision 2, `"build": "full"`), chromium headless, the
same 24-core linux box. The probe `init`s with the guest from
`crates/wafer-guest` and the `hello` template as the warm-up — what
`GET /b/dev/api/guest` hands the page — and every step passed. The numbers
are from the final run of the committed worker (`cargo clean -p` before each
build); earlier runs that day, before the clean, were ~0.4 s faster per
compile.

| | |
| --- | --- |
| `ready`, including the warm-up build | **35 394 ms** (~8 s toolchain, ~27 s building `wafer_guest` through `hello`) |
| `ready` again, component in IndexedDB | 32 905 ms |
| `compile` of `hello` after the warm-up | **2 289 ms**, artifact 115 327 bytes, whole wafer ABI |
| `compile` of `hello` with its greeting edited | 2 275 ms, 115 295 bytes, contents differ from the first |
| `compile` of `newsletter` (the `table` template, its scaffolded profile) | **5 728 ms**, artifact 133 784 bytes, whole wafer ABI |
| `compile` of a self-contained `hello` (the SDK as a module, no dependencies) | 20 408 ms, artifact 111 133 bytes, whole wafer ABI |
| `compile` of `probe_fresh` (no dependencies), then again with only `src/lib.rs` edited | 1 664 ms / 1 636 ms; answers 1, then 2 |
| `compile` of `hello` with a syntax error | 988 ms, `success: false`, no artifact, `src/lib.rs:46:18` |
| `compile` of `hello` with a truncated `Cargo.toml` (`[package` …) | 39 ms, `success: false`, no artifact, `blocks/hello/Cargo.toml:1:9 unclosed table` |
| `compile` of `hello` with a path dependency that is not there | 733 ms, `success: false`, no artifact, `artifact-missing` plus cargo's "failed to get `wafer_guest` as a dependency" in `stderr` |
| `init` with a guest whose `src/lib.rs` is `fn { broken` | `error` in 8 297 ms: "the guest crate does not build on this toolchain: /wafer_guest/src/lib.rs:1: this file contains an unclosed delimiter" |
| `init` with a guest whose `Cargo.toml` names a missing path dependency | `error`: "the guest crate does not build on this toolchain: the warm-up left no /target/wasm32-wasip1/release/hello.wasm …; cargo said: … failed to get `no_such_crate` …" |
| total download to first `ready` | 75.1 MB (13 files), largest `vfs.core-*.wasm.br.part-001` at 25 165 824 bytes |

1. **A block compile no longer rebuilds the guest.** `hello` went from ~20 s
   (the self-contained row, which is what every compile cost before) to
   2.3 s, and `newsletter` builds in 5.7 s. The probe fails if `hello` takes
   15 s or more, so a regression to rebuilding the guest cannot pass as
   merely slow. Confirmed.
2. **An edit is always rebuilt.** A dependency-free crate edited in
   `src/lib.rs` alone answers the new value — and with `cargo clean -p`
   taken out for one run it answered the old one (`probe_value() = 1`,
   cargo `Fresh`), as it also did with `touch` in its place. For `hello`, an
   edited greeting gives a different module and a syntax error right after
   reaches rustc; those pass without the clean too, because a block that
   depends on `wafer_guest` is marked dirty every time on this pin, so they
   are asserted as the real case but are not the proof. Confirmed.
3. **A block from before this change still builds.** The self-contained
   `hello` — `Cargo.toml` with an empty `[dependencies]`, the guest crate's
   `lib.rs` as `src/wafer_guest.rs`, the five exports written out — is what a
   seed archive exported before then contains; it compiles against the same
   worker, in the same `/target`, and exports the whole ABI. Confirmed.
4. **A guest that does not build fails `init`**, with rustc's first error in
   the message, rather than posting `ready` and failing every compile after.
   Confirmed.
5. **Cargo reports success for a syntax error, and a build can fail with no
   diagnostic at all** (see "How a compile actually happens"). Found by this
   run's error build, which came back `success: true` with an error
   diagnostic and the previous artifact. The worker now removes the previous
   module before every build and fails any build with an error diagnostic;
   with the `rm` taken out for one run, the missing-path-dependency compile
   came back `success: true` with the previous `hello.wasm` (115 327 bytes),
   so that probe step is load-bearing. With the error-diagnostic check taken
   out instead, the syntax-error build still failed (no module to download),
   so the `rm` alone closes the stale-artifact hole and the check adds the
   diagnostics. `rm -f` on a path that does not exist returns the prompt
   cleanly (the warm-up's first `rm`). Confirmed, and guarded; not fixed at
   its root.
6. **The warm-up is the new cost of `ready`.** About 27 s of the 35 s is the
   warm-up build, paid once per worker. A cancelled compile spends the
   worker, so the fresh worker pays it again.


## Not confirmed

Everything above was measured. These were not, and should not be assumed:

* **One target only.** `wasm32-wasip1` is the only sysroot vendored;
  `load_sysroot` for any other triple resolves to a file that is not there and
  fails. That is deliberate — the alternative is fetching from a third party —
  but the sandbox is single-target until another tarball is pinned.
* **`cancel` → `terminate()` → fresh worker was not run end to end.** The
  refusal path is (a stray `cancel` is answered `error` and the worker keeps
  working — the probe checks it); cancelling a *running* compile, terminating
  and re-initialising is not. The adapter (Task 3) is where that gets
  exercised.
* **No page reload was measured.** The "warm" figure is a second worker in the
  same page, which is the same IndexedDB but not the same code path a returning
  visitor takes.
* **Eleven compiles per worker, not more.** Nothing here says what a worker does
  after twenty, or how the VFS's memory behaves over a long session.
* **The Cloudflare edge is untested.** Everything ran against
  `scripts/serve-probe.mjs`, which sets the same two headers the deployment
  does but is not a CDN: no range requests, no compression negotiation, no
  cache. The 24 MiB (25 165 824 byte) per-part cap is our own, enforced by
  our verifier; Cloudflare's 25 MiB limit it sits under was never observed
  from a real upload.
* **`rustc 1.83.0-dev` is whatever rubrc's pinned commit embeds**, not a
  version we chose or can bump independently. The templates and
  `crates/wafer-guest` have to keep compiling on it, and a pin bump can move it.
* **Why cargo reports a syntax error as success was not traced.** What was
  seen is rustc's rendered error followed by cargo's `Finished`; where in
  rubrc's process layer rustc's status is lost was not established. It is an
  upstream defect — cargo's `build-finished` success does not reflect a
  rustc failure — and a candidate for the next pin bump.
  An internal compiler error and a linker failure were not tried; the `rm`
  before every build is what covers them.
* **Why `touch` does not reach cargo was not traced.** The VFS's timestamps
  are counters (cargo prints them as nanoseconds since the epoch) and the
  write-file event does not move them; what `touch` sets was not inspected.
  The upstream fix is real file timestamps in rubrc's VFS, a candidate for
  the next pin bump, after which `cargo clean -p` could go.
* **`--fast` has not been run.** It selects rubrc's own `no-opt` recipe and
  marks the manifest so the verifier refuses it, but the composition has not
  been exercised that way here.
* **Nothing was checked against a browser other than chromium 146.**

## Updating the pin

Change `PIN.json`, run `build-compiler.sh`, run the probe, and update the
numbers above.

`version` is `<rubrc sha at eight characters>.<packaging revision>` — today
`807ace9e.2`. The sha names the toolchain; the revision names what we wrapped
around it. A change to `src/**` or `scripts/**` changes what ships in `dist/`
without moving rubrc, so it increments the revision (`807ace9e.2` →
`807ace9e.3`) and the new version is published as its own release asset
(see "Publishing the compiler"); a pin bump starts the new sha at `.1`. That
is what keeps the promise that a checkout is only ever handed the worker its
own tree describes: the page loads `/__impresspress_dev/compiler/<version>/`,
and two different workers never share a version. Every script treats
`version` as an opaque string — `build-compiler.sh`, `write-manifest.mjs`,
`verify-compiler-assets.mjs`, `pack-dist.sh`, `fetch-dist.sh` and
`build.sh --check` only ever compare it with `PIN.json`'s or use it as a path
segment.

A new version re-runs brotli + split (~9 minutes here, because the split
cache is keyed on the version) even when the component is on disk; after
that, an edit to `src/**` is the 6-50 s path again.

Between a packaging change landing on a branch and its release asset being
published, CI cannot fetch the dist: `ci-shared.yml` keys its compiler cache
on `PIN.json` and `src/**` (among others), so that misses too, and those jobs
fall back to a `--fast` composition. That is the expected state until the
asset for the new version is published — slower CI, not a broken one. The
deploy workflow has no such fallback and fails until the asset exists.

`dist/` holds one version, and nothing is served from a version the manifest
does not name.
