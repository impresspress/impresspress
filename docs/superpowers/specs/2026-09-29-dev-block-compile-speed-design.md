# Dev block compile speed

**Date:** 2026-09-29
**Status:** approved in conversation; amends `2026-09-02-dev-sandbox-design.md` (§4, §8, §20)
**Goal:** an agent editing a backend block in the sandbox waits seconds for a
compile, not most of a minute.

## 1. Where the time goes today

Measured 2026-09-29 against the published `compiler-807ace9e` dist with the
probe in `examples/dev-sandbox/compiler/src/probe.html` and a throwaway
variant of it, on a 24-core box over localhost, warm worker:

| `hello` template | build | artifact |
| --- | --- | --- |
| current profile (`opt-level = "z"`, `lto = true`, `codegen-units = 1`) | 36–38 s | 89 KB |
| same, `lto = false` | 20–21 s | 111 KB |
| `table` template (211 lines), current profile | 46 s | — |
| `table` template, `lto = false` | 29 s | — |
| `lto = "thin"` | 139 s | 108 KB |
| 16 codegen units, no LTO | 51 s | 116 KB |
| `opt-level = 0`, no LTO | 11 s | 215 KB |
| any compile error (parse, type or borrow) | 5.6 s | — |
| trivial crate (cargo + rustc start + link) | 1.4 s | 108 B |
| guest SDK prebuilt as a dependency, block only | **3.0 s** | 115 KB |
| same, `table` template (211 lines) | **6.4 s** (46 s today) | 134 KB |
| one-time build of the guest dependency | 29 s | — |

The toolchain's `ready` takes ~7 s over localhost (download 1.5, instantiate
0.5, shell boot 2.5, sysroot 2.3) and the page starts it only inside the
first `dev_compile_block`, so the first compile pays it too.

Two things eat the 38 s: fat LTO over the whole standard library (~18 s), and
recompiling the 2,300-line vendored `wafer_guest.rs` on every build (~15 s).

Dead ends, verified so nobody retries them: rustc's incremental compilation
gives zero reuse in this toolchain (1 and 256 codegen units both rebuild
fully); cargo's `checksum-freshness` is accepted by cargo but the embedded
rustc rejects `-Zchecksum-hash-algorithm`; thin LTO, more codegen units and
`opt-level = 0` are all worse than the changes below; the Rubrc pin is
already upstream's newest commit.

## 2. The three changes

### 2.1 No LTO in the template profile

`lto = false` in both template `Cargo.toml`s. Halves every build for 22 KB of
artifact against a 4 MiB limit; the artifact instantiates and exports the
whole wafer ABI (checked by the probe). One codegen unit stays: parallel
codegen is a loss inside wasm rustc. The validator's "build with lto" advice
and the reference's profile paragraph say the same thing the templates do.

### 2.2 The compiler starts before the first compile

Spec §4 already promises this; the code does not do it. The page starts the
compiler session (`ensureCompiler`) on load when the workspace already has a
block, and otherwise the moment `dev_create_block` scaffolds one. Start-up
progress reaches the log exactly as it does inside a compile today. A visitor
who never creates a block never pays the download or the memory.

With §2.3 the session's start-up also builds the guest dependency, so by the
time an agent has scaffolded and edited a block, a compile is 3–6 s.

### 2.3 The guest SDK is a crate, built once per session

Today `dev_create_block` writes `src/wafer_guest.rs` into every block and
`lib.rs` says `mod wafer_guest;`. The SDK's `#[no_mangle]` exports live in
that module and call `crate::block()` / `crate::init()`, which is why it has
to be a module of the block crate and why every compile rebuilds it. The
spec assumed Rubrc could only share code through in-crate modules; a plain
path dependency builds and links fine (measured above), so the vendored copy
was a workaround for a constraint that does not exist.

**The crate.** `crates/wafer-guest/` is a workspace member, package
`wafer_guest`, std-only, no dependencies, edition 2021 (the toolchain is
rustc 1.83). It is the file `templates/wafer_guest.rs` moved, with its
`mod abi` turned into ordinary public functions (`abi::alloc`,
`abi::host_codec`, `abi::info(&Block)`, `abi::handle(&Block, ptr, len)`,
`abi::lifecycle(init, ptr, len)`) and one `macro_rules!` macro:

```rust
// blocks/<name>/src/lib.rs
use wafer_guest::*;

wafer_guest::export!(block, init);

pub fn block() -> Block { … }
pub fn init(ctx: &Ctx) -> Result<(), String> { … }
```

`export!` expands to exactly the five `#[no_mangle] extern "C"` exports the
host calls, each a one-line call into `abi`, all gated on
`target_arch = "wasm32"` so a template still compiles on the host. The
unsafe pointer handling stays inside `abi`. `WAFER_GUEST_VERSION` stays in
the crate at 2: the wire contract between guest and host does not change,
so an artifact built from the vendored module is still a valid block, and a
seed archive that carries one still imports.

`impresspress-core` embeds the crate's `Cargo.toml` and `src/lib.rs` with
`include_str!` for the API below, and its parity test (the one that renders
`BlockInfo` JSON on the host and parses it with the real `wafer_block`
types) takes `wafer_guest` as a normal dev-dependency instead of compiling
the file through a host shim.

**The block.** `dev_create_block` writes two files: `Cargo.toml`, whose
`[dependencies]` is `wafer_guest = { path = "../../wafer_guest" }` and whose
profile is §2.1's, and `src/lib.rs` as above. The flat-crate `nested-source`
rule is unchanged; the guest is not in the workspace at all. The reference
endpoint documents the dependency and the macro and stops carrying the
module's source (`wafer_guest_module`); an agent that wants the SDK's source
reads `GET /b/dev/api/guest`, which returns
`{ version, files: { "Cargo.toml", "src/lib.rs" } }` — the same payload the
page hands the compiler. The `wafer-guest-version` diagnostic stops telling
the agent to overwrite `src/wafer_guest.rs` and says to recompile instead.

**One layout everywhere.** The workspace puts blocks at `blocks/<name>/`;
the export archive puts them at `seed/blocks/<name>/` and the guest at
`seed/wafer_guest/` (outside the seed manifest's block list, so an import
does not mistake it for a block); the compiler's VFS puts them at
`/blocks/<crate>/` and `/wafer_guest/`. In all three the dependency is the
same string, `../../wafer_guest`, and nothing rewrites it. The VFS directory
is named after the crate the page already reports (`crateName`, the
`[package] name`, which the page's `package-name` rule keeps equal to the
block name), so the protocol gains no field. The worker builds with
`cargo build --release --target wasm32-wasip1 --manifest-path
/blocks/<crate>/Cargo.toml --target-dir /target --message-format=json` from
`/`, so every block shares one target directory and the guest built for the
first block is reused by the next (measured: 6.4 s for the table template
after `hello`).

**The session.** `init` carries the guest crate's files and a warm-up crate
(the scaffolded `hello` template). After the sysroot loads, the worker
writes `/wafer_guest/**` and `/blocks/hello/**` and runs the warm-up build;
`ready` is posted only when it has finished, and the ready message reports
the guest's version. A `compile` that arrives during the warm-up queues
behind it exactly as one arriving during the sysroot load does today. The
warm-up is an ordinary compile of an ordinary block, not a special path.

**Freshness.** Rubrc's write-file event replaces a file's contents without
moving its mtime, so cargo would call an edited block fresh. Today's answer
is `cargo clean` before every build, which with a dependency would cost the
29 s it just saved. The worker instead runs `cargo clean -p <crate>` before
each build, cargo's own way to rebuild one package: it removes the block's
artifacts and leaves the guest's build in `/target` alone (measured
2026-09-30: an edit to a dependency-free crate is rebuilt, and hello costs
~0.4 s more). `touch` was tried first and does not work: a touched
dependency-free crate still came back `Fresh`. The VFS's timestamps behave
as counters, and what `touch` sets was not inspected. The root-cause fix is real file timestamps in Rubrc's VFS, upstream,
for the next pin bump, and the worker comment says so.

**Staging.** `POST /b/dev/api/builds/stage` keeps its `wafer_guest_version`
field and its `wafer-guest-version` refusal. The page fills it from the
guest it handed the worker at `init` (the API's `version`), not by parsing a
Rust file out of the workspace. The refusal still catches the real case: a
page holding a compiler session built from an older bundle than the service
worker that is now validating its output.

**Export.** The archive gains `seed/wafer_guest/Cargo.toml` and
`seed/wafer_guest/src/lib.rs` whenever it contains a block, so
`cargo build --release --target wasm32-wasip1` inside `seed/blocks/<name>`
works on a host toolchain, which is what the export README's "edited and
recompiled" promise needs. A seed archive from before this change (three
files per block, `mod wafer_guest;`) still imports and still compiles: the
worker writes whatever files the block has and cargo builds a
self-contained crate the way it always did, only slower.

**The profile is shared.** Cargo applies the root package's
`[profile.release]` to the dependency, so a block that edits its profile
triggers a guest rebuild (29 s). Scaffolded blocks share one profile; the
reference says what changing it costs.

## 3. What does not change

* One compile at a time, the 120 s compile budget, the 4 MiB artifact limit,
  the 16-block limit, validation, activation, generations.
* The block author's code: `block()`, `init()`, handlers, `db`, `storage`,
  `config`, the JSON types.
* Rubrc's pin. The composed component is untouched; only our worker sources
  and packaging change.

## 4. Packaging and publishing the compiler

`examples/dev-sandbox/compiler/src/worker-entry.ts` and `protocol.ts` change,
so the dist must be rebuilt and republished. `PIN.json`'s `version` becomes
`<rubrc sha>.<packaging revision>` (`807ace9e.2`), so the dist directory, the
release tag, the asset and the CI cache key all move together and an old
checkout keeps fetching the asset its own pin names. Landing checklist for
the compiler PR: `build-compiler.sh` (optimised, ~55 min, 12.6 GB RSS),
`pack-dist.sh`, `gh release create compiler-807ace9e.2 …`. Until the asset
exists, CI's dev e2e jobs fall back to a `--fast` composition, as designed.

The compiler and the crate land in one PR: the worker's warm-up path is
only testable with the crate in the tree, and the page's `init` is only
correct with the new worker. The dist for that PR is built from its branch
and published before its CI runs. `init` without guest files still skips the
warm-up and a self-contained crate still builds, which is what keeps the
probe, the fake-worker fixture and imported old archives working.

## 5. Delivery

Three PRs, in order:

1. **profile** — §2.1: two `Cargo.toml`s, the validator's advice text, the
   reference's profile paragraph, `docs/dev-sandbox.md`'s timings, this
   design doc.
2. **pre-warm** — §2.2: `dev.js` starts the session on load or on scaffold;
   page-side tests; the `dev-compile` e2e's `ready_ms` derivation;
   `docs/dev-sandbox.md`.
3. **guest crate** — §2.3 and §4: `crates/wafer-guest` and `export!`;
   protocol, worker, adapter, probe, compiler README, `PIN.json` version and
   the published dist; scaffold, `GET /b/dev/api/guest`, staging, export
   archive, seed; `dev.js` and its tests; parity, golden, scaffold, export
   and seed tests; e2e specs and the fake-worker fixture; the generated JS
   SDK; reference, `docs/dev-sandbox.md`, `RELEASE.md`, spec §20 amendment.

## 6. Testing

* `crates/wafer-guest`: its existing unit tests move with it. The parity
  test in `impresspress-core` takes it as a dev-dependency and keeps
  `#[path]`-including both template `lib.rs` files, whose `use
  wafer_guest::*` now resolves to the crate. The golden test (real cargo,
  `wasm32-wasip1`, offline) builds each template beside a `wafer_guest/`
  crate in the archive layout, which is also what proves the exported
  layout builds on a host.
* Scaffold, staging and export unit tests updated for the two-file block and
  the archive's guest crate.
* `dev_compile_block.test.mjs` (page side) for `init` carrying the guest and
  the version reaching the staging request.
* The probe, run against a rebuilt dist, reports the `hello` build under 5 s
  warm and the guest version in `ready`.
* CI's `dev-compile` and `dev-scenario` e2e timing lines are the acceptance
  numbers: `compile_ms` under 10 s on a GitHub runner for the newsletter
  block.
