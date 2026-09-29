# Dev Block Compile Speed Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A block edited in the dev sandbox compiles in seconds: the toolchain is running before the first compile, and the guest SDK is built once per session instead of once per build.

**Architecture:** The vendored `wafer_guest.rs` module becomes a real crate (`crates/wafer-guest`) that every block depends on by path; the compiler worker builds it once at session start (by compiling the `hello` template as a warm-up) and forces block rebuilds with `touch` instead of `cargo clean`, so cargo's own freshness tracking keeps the guest warm. The page starts the compiler session as soon as a block exists, and hands the worker the guest crate from a new `GET /b/dev/api/guest` endpoint.

**Tech Stack:** Rust (workspace crates, `macro_rules!`, `include_str!`), TypeScript compiler worker (Rubrc, vite), plain-JS page (`dev.js`, `compiler-adapter.js`, `node --test`), Playwright e2e, Cargo path dependencies.

**Spec:** `docs/superpowers/specs/2026-09-29-dev-block-compile-speed-design.md` (PR 1 of that spec, the profile change, is already dispatched separately; this plan is PR 2 and PR 3).

## Global Constraints

- `crates/wafer-guest` is std-only, has NO dependencies, edition `2021`, and its `Cargo.toml` uses no `workspace = true` inheritance — the same file is written verbatim into the compiler's VFS and into export archives, where there is no workspace.
- The pinned in-browser toolchain is `rustc 1.83.0-dev`; nothing in the guest crate or the templates may need a newer compiler or edition 2024.
- `WAFER_GUEST_VERSION` stays `2` (spec §2.3: the wire contract does not change).
- The relative dependency path is the single string `../../wafer_guest` in the templates, the export archive (`seed/blocks/<name>/` → `seed/wafer_guest/`) and the VFS (`/blocks/<crate>/` → `/wafer_guest/`). Nothing rewrites it.
- The VFS build command is exactly `cargo build --release --target wasm32-wasip1 --manifest-path /blocks/<crate>/Cargo.toml --target-dir /target --message-format=json`, run from `/`.
- Freshness is `touch` over every path the worker just wrote. No `cargo clean` anywhere in the worker.
- The compiler protocol stays backward compatible: `init` without `guest` skips the warm-up; `compile` of a self-contained crate still builds.
- `PIN.json` `version` becomes `807ace9e.2`; the release tag/asset for PR 3 is `compiler-807ace9e.2` / `compiler-dist-807ace9e.2.tar`.
- Block authors keep writing exactly `pub fn block() -> Block` and `pub fn init(ctx: &Ctx) -> Result<(), String>`.
- The block crate stays flat (`Cargo.toml` + `src/*.rs`); the `nested-source` rule in `dev.js` is unchanged.
- Repo rules from `CLAUDE.md`: fix at root cause, no compat shims, no sync bridges, comments are contracts (a comment that describes old behaviour is a bug).
- Every commit message ends with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. PR bodies end with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
- Never use `git stash`. Implementers name scratch files with their task number under the session scratchpad.

## Review Focus

1. **A block that edits its `[profile.release]`** (say `opt-level = 3`) must still compile after the warm-up; cargo rebuilds the guest under the new profile (29 s) and the artifact is produced. Pinned by the probe's `table` compile using the scaffolded profile (Task 6) and by the golden test's `build_scaffolded_with` edit hook (Task 2, `opt-level` edit case added there).
2. **A compile that arrives while the warm-up is running** must wait, not fail: the adapter already queues behind `initialize`. Pinned by the fake-worker adapter e2e (`dev-compiler.spec.ts`, "queues compiles") and by the fake worker accepting `init.guest` (Task 7).
3. **An old-shape block** (three files, `mod wafer_guest;`, from a seed archive exported before this change) must still compile and stage: the worker writes whatever files it is given and cargo builds a self-contained crate. Pinned by the probe's third compile, a self-contained crate assembled from the guest source and the template (Task 6).
4. **A session whose guest is older than the server's** (service worker updated under an open page) must be refused with a remedy that works: reload the page, compile again. Pinned by `tests/dev_scaffold.rs::staging_with_a_stale_module_version_is_a_diagnostic` (text assertion added in Task 3) and by `dev_compile_block.test.mjs` asserting the staging request carries the session's guest version (Task 5).
5. **A warm-up whose guest crate does not build** on the pinned toolchain must fail `init` loudly with the first rustc error in the message, not leave the worker `ready` with no guest. Pinned by a worker-side unit of logic that is only reachable through the probe: Task 6 adds a probe step that sends an `init` with a guest whose `src/lib.rs` has a syntax error to a second worker and asserts the `error` message names the guest.

---

# PR 2 — pre-warm (worktree `../dev-compile-speed-p2`, branch `feat/dev-compile-prewarm`, from `origin/main`)

### Task 1: Start the compiler when a block exists

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/assets/dev.js` (`discoverCompiler` ~817–834, `renderBlockChoices` ~903–937, `ensureCompiler` ~996–1021)
- Test: `crates/impresspress-core/src/blocks/dev/assets/test/dev_compiler_discovery.test.mjs`, `crates/impresspress-core/src/blocks/dev/assets/test/harness.mjs` (read it first: `instantiate({...})` injects a `BrowserRustCompiler` stub at ~line 37 and ~334)
- Modify: `crates/impresspress-web/tests/e2e/dev-compile.spec.ts:283-302` (the `ready_ms` derivation)
- Modify: `docs/dev-sandbox.md` "Compiling one" (~line 137)

**Interfaces:**
- Consumes: `ensureCompiler(onProgress)` (existing, idempotent), `compilerManifest`, `blockNames`, `log()`, `appendProgress`.
- Produces: `warmCompiler()` in `dev.js`; the log line `compiler: ready (<rustc version>)` when start-up finishes, which the e2e times.

- [ ] **Step 1: Write the failing page-side tests**

Read `harness.mjs` and the existing tests in `dev_compiler_discovery.test.mjs` to learn how a workspace listing and a manifest are injected and how the stubbed `BrowserRustCompiler` is supplied. Then add, using the same helpers (adapt names to the harness):

```js
// A BrowserRustCompiler stub that counts start-ups.
function countingCompiler() {
  const calls = { initialize: 0 };
  class Stub {
    constructor(manifest) { this.manifest = manifest; }
    async initialize() { calls.initialize += 1; return 'rustc 1.90.0-nightly (fake)'; }
    async compile() { throw new Error('not compiled in this test'); }
  }
  return { Stub, calls };
}

test('the toolchain starts on load when the workspace already has a block', async () => {
  const { Stub, calls } = countingCompiler();
  const { settle } = instantiate({ manifest: MANIFEST, workspace: [file('blocks/hello/Cargo.toml', '[package]\nname = "hello"\n')], compiler: Stub });
  await settle();
  assert.equal(calls.initialize, 1);
});

test('a workspace with no block never starts the toolchain', async () => {
  const { Stub, calls } = countingCompiler();
  const { settle } = instantiate({ manifest: MANIFEST, workspace: [file('site/index.html', '<h1>hi</h1>')], compiler: Stub });
  await settle();
  assert.equal(calls.initialize, 0);
});

test('without a compiler in the build nothing starts, blocks or not', async () => {
  const { Stub, calls } = countingCompiler();
  const { settle } = instantiate({ manifest: null, workspace: [file('blocks/hello/Cargo.toml', '[package]\nname = "hello"\n')], compiler: Stub });
  await settle();
  assert.equal(calls.initialize, 0);
});

test('the toolchain starts when the first block is scaffolded, and only once', async () => {
  const { Stub, calls } = countingCompiler();
  const { settle, handle } = instantiate({ manifest: MANIFEST, workspace: [], compiler: Stub });
  await settle();
  assert.equal(calls.initialize, 0);
  // The listing refresh every mutating call performs, now with a block in it.
  await handle.renderBlockChoices([file('blocks/hello/Cargo.toml', '[package]\nname = "hello"\n')]);
  await handle.renderBlockChoices([file('blocks/hello/Cargo.toml', '[package]\nname = "hello"\n')]);
  assert.equal(calls.initialize, 1);
});
```

If the harness exposes internals under a different name than `handle.renderBlockChoices`, use whatever it exposes (`dev_compile_block.test.mjs` reaches `handle.snapshotBlock`, so the pattern exists); if `renderBlockChoices` is not reachable, expose it the same way the harness exposes `snapshotBlock`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `node --test crates/impresspress-core/src/blocks/dev/assets/test/dev_compiler_discovery.test.mjs`
Expected: the four new tests FAIL (initialize count 0 where 1 is expected, or `warmCompiler` undefined).

- [ ] **Step 3: Implement `warmCompiler` in `dev.js`**

Add directly below `ensureCompiler`:

```js
// Whether the toolchain has been asked to start ahead of a compile.
//
// One-shot on purpose. `ensureCompiler` is idempotent while a start is in
// flight or has succeeded, but the adapter clears its latch when a start
// FAILS, and a warm-up re-armed by every listing refresh would retry a broken
// toolchain after every write. The compile path keeps its own retry — a
// failed warm-up costs the first compile its start-up and nothing else.
var compilerWarmStarted = false;

// Start the toolchain before anyone asks for a build.
//
// Two facts have to hold and they arrive in either order: a compiler in this
// build (`discoverCompiler`) and a block to compile (`renderBlockChoices`).
// Both call this, and whichever lands second starts the worker. A workspace
// with no block never starts it: a visitor editing site files pays neither
// the download nor the memory. Design: spec 2026-09-29 dev-block-compile-speed §2.2.
function warmCompiler() {
  if (compilerWarmStarted || !compilerManifest || blockNames.length === 0) {
    return;
  }
  compilerWarmStarted = true;
  log('compiler: starting ahead of the first compile');
  ensureCompiler(appendProgress)
    .then(function (version) {
      log('compiler: ready (' + version + ')');
    })
    .catch(function (error) {
      log('compiler: start-up failed (' + error.message + '); the first compile will retry');
    });
}
```

Call `warmCompiler();` as the last statement of `discoverCompiler` (after `log('compiler ' + … + ' available')`) and as the last statement of `renderBlockChoices` (after `updateCompileButton()`). Check `blockNames` is declared (`var blockNames = []`) above both call sites; if it is declared below `discoverCompiler`, move the declaration up beside `compilerManifest`.

- [ ] **Step 4: Run the page-side tests**

Run: `node --test crates/impresspress-core/src/blocks/dev/assets/test/*.test.mjs`
Expected: all PASS, including the existing discovery tests (a manifest with an empty workspace must still leave the button disabled with the "nothing to compile" title).

- [ ] **Step 5: Fix the e2e timing derivation**

In `crates/impresspress-web/tests/e2e/dev-compile.spec.ts` the `ready_ms` at ~283–302 is derived on the assumption that start-up happens inside the compile call. Replace it: record `const warmStarted = Date.now()` immediately after the `dev_create_block` call succeeds (that is what starts the worker now), then before calling `dev_compile_block` wait for the page log to contain `compiler: ready` (find the log element's id in `dev.js`'s `log()` function and use `expect(page.locator('#<id>')).toContainText('compiler: ready', { timeout: 6 * 60 * 1000 })`), and set `readyMs = Date.now() - warmStarted`. Keep the printed line format exactly `dev-compile: ready_ms=… compile_ms=… artifact_bytes=… first_compile_ms=…` — CI greps it. Update the comment above it to say what is now measured. Delete the sentence "`ensureCompiler` runs INSIDE the compile call".

Also check `dev-compile-tool.spec.ts:55-80`: its fake-worker manifest swap runs in `beforeAll`, before the page loads, so the warm-up picks up the fake worker — confirm by reading, and fix the spec's comment at ~331–335 only if it describes start-up timing that changed.

- [ ] **Step 6: Update `docs/dev-sandbox.md`**

In "Compiling one", replace the sentence about cold start with: the toolchain downloads and starts as soon as the workspace has a block (on page load, or when the first block is scaffolded), so a compile normally does not wait for it; a workspace with no block never loads the compiler.

- [ ] **Step 7: Run the e2e locally if a bundle can be built, otherwise state that CI will run it**

The e2e needs `examples/dev-sandbox/build.sh` (wasm-pack build of `impresspress-web` with `--features browser-devtools`, ~10 min) and `compiler/dist/` (copy from `../dev-compile-speed/examples/dev-sandbox/compiler/dist` if absent). If both are feasible, run `TEST_PORT=8083 npx playwright test --config=tests/playwright.config.ts tests/e2e/dev-compile-tool.spec.ts tests/e2e/dev-compile.spec.ts` from `crates/impresspress-web` with the bundle served on 8083 (see `.github/workflows/ci-shared.yml` ~line 1250 for the serve step). Report the `dev-compile:` line.

- [ ] **Step 8: Commit and open the PR**

```bash
git add crates/impresspress-core/src/blocks/dev/assets/dev.js crates/impresspress-core/src/blocks/dev/assets/test/dev_compiler_discovery.test.mjs crates/impresspress-web/tests/e2e/dev-compile.spec.ts docs/dev-sandbox.md
git commit -m "perf(dev): start the in-browser compiler as soon as a block exists"
git push -u origin feat/dev-compile-prewarm
gh pr create --base main --title "perf(dev): start the in-browser compiler before the first compile" --body "…"
```

---

# PR 3 — guest crate + compiler (worktree `../dev-compile-speed`, branch `feat/dev-compile-speed`)

Tasks 2–8 run in order in the same worktree. The tree compiles only at the END of Task 2 (the crate move, the templates and the tests that include them are one atomic change).

### Task 2: `crates/wafer-guest` — the SDK as a crate, `export!`, templates, parity and golden tests

**Files:**
- Create: `crates/wafer-guest/Cargo.toml`
- Move: `crates/impresspress-core/src/blocks/dev/templates/wafer_guest.rs` → `crates/wafer-guest/src/lib.rs` (`git mv`)
- Delete: the symlinks `crates/impresspress-core/src/blocks/dev/templates/hello/src/wafer_guest.rs` and `.../table/src/wafer_guest.rs`
- Modify: `Cargo.toml` (workspace `members` and `default-members`)
- Modify: `crates/impresspress-core/Cargo.toml` (`[dev-dependencies]`)
- Modify: `crates/impresspress-core/src/blocks/dev/templates/hello/{Cargo.toml,src/lib.rs}`, `.../table/{Cargo.toml,src/lib.rs}`
- Modify: `crates/impresspress-core/src/blocks/dev/scaffold.rs` (`Template::WAFER_GUEST` ~56–62, `Template::files` ~110–130, `handle_reference` ~326–332, unit test ~341–358, module doc 6–12)
- Modify: `crates/impresspress-core/src/blocks/dev/mod.rs:86-89` (doc of `WAFER_GUEST_VERSION`)
- Modify: `crates/impresspress-core/tests/wafer_guest_parity.rs`, `crates/impresspress-core/tests/wafer_guest_golden.rs`, `crates/impresspress-core/tests/dev_scaffold.rs`
- Modify: `crates/impresspress-core/src/blocks/dev/page.rs:456-480` (delete the regex test; the page stops parsing the file in Task 5, but the constant it asserts on disappears here)

**Interfaces:**
- Produces: crate `wafer_guest` with `pub mod abi { pub fn alloc(size: i32) -> i32; pub fn host_codec() -> i32; pub fn info(block: &Block) -> i64; pub fn handle(block: &Block, ptr: i32, len: i32) -> i64; pub fn lifecycle(init: fn(&Ctx) -> Result<(), String>, ptr: i32, len: i32) -> i64; }` and `#[macro_export] macro_rules! export`.
- Produces: in `scaffold.rs`, `pub const GUEST_CARGO_TOML: &str`, `pub const GUEST_LIB_RS: &str`, `pub fn guest_files() -> BTreeMap<String, String>` (keys `Cargo.toml`, `src/lib.rs`), and `Template::files(name)` returning TWO files.
- Consumed later by Task 3 (endpoint), Task 4 (export), Task 6 (probe reads the crate from disk).

- [ ] **Step 1: Create the crate**

`crates/wafer-guest/Cargo.toml` (self-contained; no workspace inheritance):

```toml
# The dev sandbox's guest SDK: the whole standard library of a sandbox block.
#
# Every block scaffolded by `dev_create_block` depends on this crate by path
# (`wafer_guest = { path = "../../wafer_guest" }`), the compiler worker builds
# it once per session, and an export archive carries it beside the blocks.
# It is written verbatim into places that have no Cargo workspace (the
# browser toolchain's VFS, an exported site), so nothing here may inherit
# from `[workspace.package]`, and it must keep building on the pinned
# in-browser toolchain (rustc 1.83, edition 2021, no registry access).
[package]
name = "wafer_guest"
version = "0.1.0"
edition = "2021"
license = "MIT OR Apache-2.0"
description = "Guest SDK for ImpressPress dev-sandbox blocks (std-only, no dependencies)"
publish = false

[lib]
crate-type = ["rlib"]

[dependencies]
```

`git mv crates/impresspress-core/src/blocks/dev/templates/wafer_guest.rs crates/wafer-guest/src/lib.rs`, then `git rm` the two symlinks.

Add `"crates/wafer-guest",` to both `members` and `default-members` in the root `Cargo.toml` (alphabetical position after `crates/impresspress-web`).

- [ ] **Step 2: Rewrite the crate's ABI as public functions plus the export macro**

In `crates/wafer-guest/src/lib.rs`:

1. Rewrite the module doc (lines 1–50). Drop "VENDORED — do not edit" and "Why it is vendored rather than a crate". Say: this crate IS the SDK; a block depends on it by path; the page hands its source to the in-browser compiler, which builds it once per session; the host's two wire contracts it implements (keep those two bullets verbatim); what a block author writes (keep), plus the one extra line `wafer_guest::export!(block, init);`.
2. Remove the crate-level `#![expect(dead_code, …)]` (lines ~52–55): in a library crate public items are not dead code, and an unfulfilled `expect` is itself a warning. If `cargo build -p wafer_guest` then reports a genuinely unused private item, delete the item or make it `pub`; do not add an `allow`.
3. Replace the `#[cfg(target_arch = "wasm32")] mod abi { … }` block (~lines 164–240) with:

```rust
// ---------------------------------------------------------------------------
// ABI exports
// ---------------------------------------------------------------------------

/// The bodies of the five exports the host calls.
///
/// A library cannot export `#[no_mangle]` symbols on behalf of the crate that
/// depends on it, and the host's exports have to reach the block's own
/// `block()` and `init()`, so the symbols themselves are stamped into the
/// block by [`export!`]. Each one is a single call into here, which is where
/// the pointer handling lives.
pub mod abi {
    use super::{dispatch, json, render_block_info, render_result, Ctx, Request, Response};

    /// Pack a slice as the `(ptr << 32) | len` the host unpacks.
    fn pack(bytes: &[u8]) -> i64 {
        ((bytes.as_ptr() as u32 as i64) << 32) | bytes.len() as i64
    }

    /// Leak a `String` so the pointer handed to the host outlives the call.
    ///
    /// Deliberate: the host reads the bytes *after* the export returns, so
    /// nothing here may free them. A guest instance is short-lived (the
    /// runtime builds a fresh one per call unless the block declares a
    /// state-retaining mode), so the leak is bounded by one request.
    fn leak(s: String) -> &'static [u8] {
        Box::leak(s.into_boxed_str()).as_bytes()
    }

    /// `__wafer_alloc`: `size` bytes for the host to write a frame into.
    pub fn alloc(size: i32) -> i32 {
        Box::leak(vec![0u8; size.max(0) as usize].into_boxed_slice()).as_mut_ptr() as i32
    }

    /// `__wafer_host_codec`: the JSON host-call codec
    /// (`wafer_block::abi::HOST_CODEC_JSON`).
    pub fn host_codec() -> i32 {
        1
    }

    /// `__wafer_info`: the block's `BlockInfo`, rendered from its declaration.
    pub fn info(block: &super::Block) -> i64 {
        pack(leak(render_block_info(block)))
    }

    /// `__wafer_handle`: decode the frame, route it, render the result.
    ///
    /// # Safety
    /// `ptr`/`len` name memory the host wrote through `__wafer_alloc`; the
    /// host guarantees that, and this is the only reader.
    pub fn handle(block: &super::Block, ptr: i32, len: i32) -> i64 {
        let frame = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
        let response = match Request::from_frame(frame) {
            Ok(request) => dispatch(block, &request),
            Err(detail) => Response::text(400, &format!("bad request frame: {detail}")),
        };
        pack(leak(render_result(&response)))
    }

    /// `__wafer_lifecycle`: run `init` on `Init`, nothing on the other transitions.
    ///
    /// The wire shape is `Result<(), WaferError>` in the v1 core ABI —
    /// `{"Ok":null}` or `{"Err":{"code":…,"message":…,"meta":[]}}` — which is
    /// serde's external tagging of a `Result`. An `Err` here fails the whole
    /// activation, which is the point: a block whose `init` could not create
    /// its tables must not start serving.
    pub fn lifecycle(init: fn(&Ctx) -> Result<(), String>, ptr: i32, len: i32) -> i64 {
        let event = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
        let is_init = json::Json::parse(&String::from_utf8_lossy(event))
            .ok()
            .and_then(|parsed| {
                parsed
                    .get("event_type")
                    .and_then(|kind| kind.as_str().map(|kind| kind == "Init"))
            })
            .unwrap_or(false);
        let out = if is_init {
            match init(&Ctx) {
                Ok(()) => r#"{"Ok":null}"#.to_string(),
                Err(message) => format!(
                    r#"{{"Err":{{"code":"Internal","message":{},"meta":[]}}}}"#,
                    json::escape(&message)
                ),
            }
        } else {
            r#"{"Ok":null}"#.to_string()
        };
        pack(leak(out))
    }
}

/// Stamp the host's five exports into a block crate.
///
/// Invoke once, at the crate root, naming the block's two functions:
///
/// ```ignore
/// wafer_guest::export!(block, init);
/// ```
///
/// Every export is gated on `wasm32`, so a template still compiles on the
/// host (the sandbox's parity test does exactly that).
#[macro_export]
macro_rules! export {
    ($block:path, $init:path) => {
        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        pub extern "C" fn __wafer_alloc(size: i32) -> i32 {
            $crate::abi::alloc(size)
        }

        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        pub extern "C" fn __wafer_host_codec() -> i32 {
            $crate::abi::host_codec()
        }

        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        pub extern "C" fn __wafer_info() -> i64 {
            $crate::abi::info(&$block())
        }

        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        pub extern "C" fn __wafer_handle(ptr: i32, len: i32) -> i64 {
            $crate::abi::handle(&$block(), ptr, len)
        }

        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        pub extern "C" fn __wafer_lifecycle(ptr: i32, len: i32) -> i64 {
            $crate::abi::lifecycle($init, ptr, len)
        }
    };
}
```

Check that `render_result`, `render_block_info`, `dispatch`, `Request::from_frame`, `json::escape` are reachable from `abi` (they were from the old module at the same level; keep visibilities as they are unless the compiler says otherwise — `render_block_info` and `dispatch` are already `pub`).

4. Update the comment on `#[cfg(test)] mod tests` (~line 2210): it now runs as this crate's own `cargo test`, and still for a block author who runs `cargo test` against the crate.

- [ ] **Step 3: Build and test the crate on both targets**

Run: `cargo test -p wafer_guest` — Expected: the JSON codec tests PASS, no warnings.
Run: `cargo build -p wafer_guest --target wasm32-wasip1` (install the target with `rustup target add wasm32-wasip1` if `rustup target list --installed` lacks it; CI installs it) — Expected: builds clean.
Run: `cargo clippy -p wafer_guest --all-targets` — Expected: no warnings.

- [ ] **Step 4: Rewrite the templates**

`templates/hello/Cargo.toml` and `templates/table/Cargo.toml`: replace the header comment (lines 1–5, about `[dependencies]` staying empty) and the `[dependencies]` table with:

```toml
# A sandbox block: one crate, one dependency.
#
# The browser toolchain that compiles this has no registry access, so the
# only dependency a block can have is the guest SDK beside it, by path — the
# same path in the workspace, in the compiler's VFS and in an export archive.
# Nothing else goes in this table.
```

```toml
[dependencies]
wafer_guest = { path = "../../wafer_guest" }
```

Keep the `[profile.release]` block exactly as PR 1 leaves it (if PR 1 has not merged when you start, apply the same edit here: `lto = false` and its comment; rebase later).

`templates/hello/src/lib.rs`: replace lines 11–21 (the comment about the vendored file, the cfg-gated `mod wafer_guest;` and `use crate::wafer_guest::*;`) with:

```rust
// The guest SDK. It is a crate beside the block (`../../wafer_guest`), not a
// file inside it: the compiler builds it once per session and every block
// links against that build. `export!` stamps the five entry points the host
// calls into this crate, wired to `block()` and `init()` below.
use wafer_guest::*;

wafer_guest::export!(block, init);
```

`templates/table/src/lib.rs`: the same replacement for lines 20–26.

- [ ] **Step 5: Update `scaffold.rs`**

Replace `Template::WAFER_GUEST` (~56–62) with module-level constants and a helper:

```rust
/// The guest SDK crate, byte for byte from `crates/wafer-guest`.
///
/// One source, three readers: `GET /b/dev/api/guest` hands it to the page
/// (which hands it to the compiler), the export archive carries it beside
/// the blocks, and the golden test builds the templates against it. A
/// scaffolded block never contains it — the block depends on it by path.
pub const GUEST_CARGO_TOML: &str = include_str!("../../../../wafer-guest/Cargo.toml");
pub const GUEST_LIB_RS: &str = include_str!("../../../../wafer-guest/src/lib.rs");

/// The crate as the compiler and the archive want it: crate-relative paths.
pub fn guest_files() -> std::collections::BTreeMap<String, String> {
    [
        ("Cargo.toml".to_string(), GUEST_CARGO_TOML.to_string()),
        ("src/lib.rs".to_string(), GUEST_LIB_RS.to_string()),
    ]
    .into_iter()
    .collect()
}
```

`Template::files` returns only the two instantiated files (drop the third tuple and rewrite its doc: "The two files a block starts as…"). Rewrite the module doc at lines 6–12 (it says the scaffolder writes the guest byte for byte). Keep `handle_reference` compiling by removing the `wafer_guest_module` field from the response — Task 3 reshapes that contract; here just delete the field from both the struct literal and `ReferenceResponse` in `contracts.rs` (lines ~516–525, drop the field and its doc). Update the unit test `a_scaffolded_block_is_three_files_under_its_own_directory` → `a_scaffolded_block_is_two_files_under_its_own_directory` (paths `Cargo.toml`, `src/lib.rs`). Fix `mod.rs:86-89`'s doc so it no longer talks about "the vendored module" and instead points at `crates/wafer-guest` and says the number here must equal `wafer_guest::WAFER_GUEST_VERSION` (the parity test asserts it). Delete `page.rs`'s test `the_page_can_read_the_version_out_of_the_vendored_guest_module`.

Add the dev-dependency in `crates/impresspress-core/Cargo.toml` `[dev-dependencies]`:

```toml
# The guest SDK, compiled for the host: the parity test renders `BlockInfo`
# JSON with it and parses the result with the real `wafer_block` types, and
# the golden test builds the templates beside it.
wafer_guest = { path = "../wafer-guest" }
```

Run `cargo metadata --manifest-path Cargo.toml > /dev/null` from the worktree to re-resolve `Cargo.lock` (the lockfile MUST be committed: a `Cargo.toml` change without it goes red on `--locked`).

- [ ] **Step 6: Update the parity test**

`tests/wafer_guest_parity.rs`: delete the `#[path = "../src/blocks/dev/templates/wafer_guest.rs"] mod wafer_guest;` declaration and its doc (lines 20–25); the crate is now an ordinary dependency, so `use wafer_guest::json::Json;` and every `wafer_guest::…` path in the file resolve to it unchanged. The two template `#[path]` modules stay: their `use wafer_guest::*;` now names the crate, and `wafer_guest::export!` expands to nothing on the host. Rewrite the module doc's "So the module is compiled **natively** here…" paragraph to say the crate is compiled natively as a dev-dependency. Replace `templates_carry_the_canonical_module_byte_for_byte` with:

```rust
/// The number the sandbox checks staged builds against is the number the
/// crate declares, and the source the API hands out is the crate's own.
#[test]
fn the_sandbox_and_the_crate_agree_on_the_guest_version() {
    assert_eq!(
        wafer_guest::WAFER_GUEST_VERSION,
        impresspress_core::blocks::dev::WAFER_GUEST_VERSION
    );
    let lib = impresspress_core::blocks::dev::scaffold::GUEST_LIB_RS;
    assert!(lib.contains(&format!(
        "pub const WAFER_GUEST_VERSION: u32 = {};",
        wafer_guest::WAFER_GUEST_VERSION
    )));
    assert!(
        impresspress_core::blocks::dev::scaffold::GUEST_CARGO_TOML.contains("name = \"wafer_guest\""),
        "the crate the API hands out must be the one the templates depend on"
    );
}
```

(If `scaffold` is not `pub` from `impresspress_core::blocks::dev`, make the two constants reachable through whatever the module already exports — `dev_scaffold.rs` reaches `Template`, so follow that path.)

- [ ] **Step 7: Update the golden test to the archive layout**

`tests/wafer_guest_golden.rs`: delete the `#[path]` `mod wafer_guest` (lines 62–63) — use the crate. Rewrite the three helpers so a build directory holds `wafer_guest/` and `blocks/<name>/`:

```rust
/// Lay the guest crate down beside the blocks, exactly as an export archive
/// and the compiler's VFS do (`seed/wafer_guest/`, `/wafer_guest/`), so the
/// template's `path = "../../wafer_guest"` resolves.
fn write_guest_crate(root: &Path) {
    for (path, content) in impresspress_core::blocks::dev::scaffold::guest_files() {
        let target = root.join("wafer_guest").join(path);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("create the directory");
        std::fs::write(&target, content).expect("write the guest crate");
    }
}

/// Build `templates/{name}` for `wasm32-wasip1` and return the module.
fn build_template(name: &str) -> Vec<u8> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/blocks/dev/templates")
        .join(name);
    let out = tempfile::tempdir().expect("tempdir");
    write_guest_crate(out.path());
    let block_dir = out.path().join("blocks").join(name);
    copy_dir_all(&source, &block_dir).expect("copy the template");
    build_crate(name, out.path(), &block_dir)
}
```

`build_scaffolded_with`: after creating the tempdir call `write_guest_crate(out.path())`, write each scaffolded file at `out.path().join(&path)` (the path already starts with `blocks/<name>/`, so stop stripping the prefix), and call `build_crate(name, out.path(), &out.path().join("blocks").join(name))`. `build_crate` gains the root: it runs `cargo build --release --target wasm32-wasip1 --offline --target-dir <root>/target` with `current_dir(block_dir)`, and its assertion message becomes `"the {name} template must build with plain cargo and no registry dependencies"`. `copy_dir_all`'s doc no longer needs to mention symlinks; simplify the doc, keep the function. Add one golden case pinning Review Focus 1: in the test that already uses `build_scaffolded_with` with an edit closure, add a second build whose closure replaces `opt-level = "z"` with `opt-level = 3` in `Cargo.toml` and assert it still produces a module that instantiates (the size assertion in `build_crate` still applies). `build_hostile_guest` (~688–700) refers to `wafer_guest::…` paths — they now resolve to the crate; adjust only if the compiler complains.

- [ ] **Step 8: Update `tests/dev_scaffold.rs`**

- `create_block_writes_the_template_and_the_module` → rename `…_two_files`, expect `["blocks/newsletter/Cargo.toml", "blocks/newsletter/src/lib.rs"]`, delete the byte-for-byte assertion on the third file, and assert `Cargo.toml` contains `wafer_guest = { path = "../../wafer_guest" }` and `lib.rs` contains `wafer_guest::export!(block, init);`.
- `a_create_that_runs_out_of_quota_part_way_stores_nothing`: it counts files — expect two.
- `reference_returns_the_authoring_guide`: drop the `wafer_guest_module` assertion (Task 3 adds the guest endpoint test).
- Any other assertion on `src/wafer_guest.rs` in `tests/dev_export.rs`, `tests/dev_seed.rs`, `tests/dev_status.rs`, `tests/dev_gc.rs`, `tests/dev_activation.rs`, `tests/dev_blocks.rs` that only exists because the scaffold wrote a third file: change to the two-file shape. Leave `tests/dev_export.rs`'s archive assertions for Task 4 (mark with a one-line comment only if the test must be temporarily adjusted to pass; prefer to fix it fully in Task 4 and run that file then).

- [ ] **Step 9: Run the suites**

Run: `cargo test -p impresspress-core --features block-dev` — Expected: PASS (unit + parity + scaffold; `dev_export.rs` may have the archive assertion failing until Task 4 — if so, run everything else with `--skip` for that one test name and say so in the commit message).
Run: `IMPRESSPRESS_GUEST_GOLDEN=1 cargo test -p impresspress-core --features block-dev,wasm --test wafer_guest_golden` — Expected: PASS; note the wall time.
Run: `cargo +nightly fmt --all -- --check` (or `cargo fmt`), `cargo clippy -p impresspress-core --features block-dev --all-targets` — Expected: no new warnings.

- [ ] **Step 10: Commit**

```bash
git add -A crates/wafer-guest crates/impresspress-core Cargo.toml Cargo.lock
git commit -m "refactor(dev): make the guest SDK a crate blocks depend on by path"
```

### Task 3: `GET /b/dev/api/guest`, the staging contract text, snapshots, SDK types

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/contracts.rs` (`StageBuildRequest` doc ~423–436; add `GuestResponse`, `WarmupCrate`)
- Modify: `crates/impresspress-core/src/blocks/dev/mod.rs` (`Route::ApiGuest`; the route table ~253; dispatch ~639; descriptions at ~238–240 and ~256 that mention `src/wafer_guest.rs`)
- Modify: `crates/impresspress-core/src/blocks/dev/scaffold.rs` (`handle_guest`)
- Modify: `crates/impresspress-core/src/blocks/dev/validation.rs:250-265` (`stale_guest_module` text), `blocks_api.rs:127-141` (comment)
- Test: `crates/impresspress-core/tests/dev_scaffold.rs`
- Regenerate: `crates/impresspress-core/tests/snapshots/dev.tools.json`, `dev.openapi.json` (find the snapshot mechanism: grep `snapshots/dev.openapi.json` in `crates/impresspress-core/tests/` for the env var or command that rewrites them), `packages/impresspress-js/src/generated/api.ts` (`npm run generate:types` in `packages/impresspress-js`, after `npm ci` there if needed).

**Interfaces:**
- Produces: `GET /b/dev/api/guest` → `GuestResponse { version: u32, files: BTreeMap<String,String>, warmup: WarmupCrate { crate_name: String, files: BTreeMap<String,String> } }`. `warmup` is `Template::Hello.files("hello")` with the `blocks/hello/` prefix stripped.
- Consumed by Task 5 (page) and Task 7 (e2e host build).

- [ ] **Step 1: Write the failing integration test**

In `tests/dev_scaffold.rs`:

```rust
/// What the page hands the compiler at start-up: the guest crate and a block
/// to build it with. Crate-relative paths, because that is what the worker
/// writes and what the export archive lays down.
#[tokio::test]
async fn the_guest_endpoint_hands_out_the_crate_and_a_warmup_block() {
    let ctx = TestContext::with_dev(FakeControl::new()).await;
    let body = output_json(
        ctx.dispatch_resolved(admin_msg("retrieve", "/b/dev/api/guest"))
            .await,
    )
    .await;
    assert_eq!(body["version"], WAFER_GUEST_VERSION);
    assert_eq!(
        body["files"]["Cargo.toml"].as_str(),
        Some(impresspress_core::blocks::dev::scaffold::GUEST_CARGO_TOML)
    );
    assert!(body["files"]["src/lib.rs"]
        .as_str()
        .expect("lib.rs")
        .contains("pub const WAFER_GUEST_VERSION"));
    assert_eq!(body["warmup"]["crate_name"], "hello");
    let warmup = body["warmup"]["files"].as_object().expect("warmup files");
    assert_eq!(
        warmup.keys().collect::<Vec<_>>(),
        vec!["Cargo.toml", "src/lib.rs"]
    );
    assert!(warmup["Cargo.toml"]
        .as_str()
        .expect("Cargo.toml")
        .contains("path = \"../../wafer_guest\""));
}
```

And extend `staging_with_a_stale_module_version_is_a_diagnostic`: assert the diagnostic message contains `"reload"` (the remedy) and does not contain `"wafer_guest_module"`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p impresspress-core --features block-dev --test dev_scaffold` — Expected: the new test FAILS (404 / missing route), the stale-version assertion FAILS on the old text.

- [ ] **Step 3: Implement**

`contracts.rs`:

```rust
/// Response of `GET /b/dev/api/guest`.
///
/// Everything the compiler session needs before the first compile: the
/// guest crate a block depends on, and a block to build it with.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuestResponse {
    /// `WAFER_GUEST_VERSION` of the crate below. A block compiled in a
    /// session started from this response reports this number to
    /// `POST /b/dev/api/builds/stage`.
    pub version: u32,
    /// The `wafer_guest` crate, crate-relative: `Cargo.toml`, `src/lib.rs`.
    pub files: std::collections::BTreeMap<String, String>,
    /// The `hello` template scaffolded as `hello`: what the compiler builds
    /// once at start-up so the crate above is compiled before any real block.
    pub warmup: WarmupCrate,
}

/// A block crate the compiler builds to warm its target directory.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WarmupCrate {
    /// Cargo's package name, which names the artifact and the VFS directory.
    pub crate_name: String,
    /// Crate-relative paths: `Cargo.toml`, `src/lib.rs`.
    pub files: std::collections::BTreeMap<String, String>,
}
```

Rewrite `StageBuildRequest.wafer_guest_version`'s doc: the version of the guest crate the compiler session was started with (`GET /b/dev/api/guest`, `version`); a mismatch is refused with `wafer-guest-version`; the remedy is to reload the workspace page (a fresh session picks up the current crate) and compile again; omit only if the compiler session has no guest (then recorded as `0`).

`scaffold.rs`:

```rust
/// `GET /b/dev/api/guest` — the guest crate and a warm-up block.
pub async fn handle_guest(_ctx: &dyn Context) -> OutputStream {
    let prefix = format!("{}hello/", workspace::BLOCKS_PREFIX);
    let files = Template::Hello
        .files("hello")
        .into_iter()
        .map(|(path, content)| {
            let relative = path
                .strip_prefix(&prefix)
                .expect("Template::files writes under blocks/<name>/");
            (relative.to_string(), content)
        })
        .collect();
    no_store().json(&GuestResponse {
        version: WAFER_GUEST_VERSION,
        files: guest_files(),
        warmup: WarmupCrate {
            crate_name: "hello".to_string(),
            files,
        },
    })
}
```

`mod.rs`: add `/// \`GET /b/dev/api/guest\`\n ApiGuest,` to `Route`; add after the reference route:

```rust
EndpointRoute::admin(HttpMethod::Get, "/b/dev/api/guest", Route::ApiGuest)
    .summary("The guest SDK crate the compiler builds against")
    .description(
        "The `wafer_guest` crate every block depends on by path, plus the block the \
         compiler builds once at session start to warm it. The page fetches this before \
         starting the toolchain; an agent only needs it to read the SDK's source.",
    )
    .output(response_schema_of::<contracts::GuestResponse>),
```

dispatch `Route::ApiGuest => scaffold::handle_guest(ctx).await,`. Fix the two descriptions that mention `src/wafer_guest.rs` (`POST /b/dev/api/blocks` writes `{Cargo.toml, src/lib.rs}`; the reference documents "the wafer_guest crate's API").

`validation.rs` `stale_guest_module`:

```rust
format!(
    "the artifact was compiled against wafer_guest version {reported}; this sandbox \
     speaks version {current}. The compiler session was started from an older \
     bundle than the one now serving: reload the workspace page (a fresh session \
     builds against the current crate) and compile again; the block's own files \
     are unchanged."
)
```

Rewrite the doc comment above it and the comment in `blocks_api.rs:127-135` to match (no "replace the module").

- [ ] **Step 4: Run, regenerate, run again**

Run: `cargo test -p impresspress-core --features block-dev --test dev_scaffold` — Expected: PASS.
Regenerate the two snapshots the way the snapshot test documents; run `cargo test -p impresspress-core --features block-dev` — Expected: PASS.
Run: `cd packages/impresspress-js && npm ci && npm run generate:types && git diff --stat -- src/generated/api.ts` — Expected: the generated file changes (new `GuestResponse`, `WarmupCrate`; `wafer_guest_module` gone). Run `npm run typecheck`.

- [ ] **Step 5: Commit**

```bash
git add crates/impresspress-core packages/impresspress-js/src/generated/api.ts
git commit -m "feat(dev): serve the guest crate and a warm-up block at /b/dev/api/guest"
```

### Task 4: The export archive carries the guest crate

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/export.rs` (after the block loop ~343; the layout doc 1–21; `ExportManifest` entries — read `handle_manifest` to see how archive entries are listed)
- Modify: `crates/impresspress-core/src/blocks/dev/templates/export-readme.md` (~42–45)
- Test: `crates/impresspress-core/tests/dev_export.rs` (`export_zip_contains_shell_seed_sources_and_data_with_dev_off` ~261, `the_manifest_describes_the_archive_entry_for_entry` ~811, `an_exported_seed_imports_into_a_fresh_instance` ~942)

**Interfaces:**
- Consumes: `scaffold::guest_files()` (Task 2).
- Produces: archive entries `seed/wafer_guest/Cargo.toml` and `seed/wafer_guest/src/lib.rs`, present iff the generation has at least one block; listed by `GET /b/dev/api/export/manifest`; NOT in `seed/manifest.json`.

- [ ] **Step 1: Write the failing tests**

In `export_zip_contains_shell_seed_sources_and_data_with_dev_off`, replace `"seed/blocks/hello/src/wafer_guest.rs"` with `"seed/wafer_guest/Cargo.toml"` and `"seed/wafer_guest/src/lib.rs"`, and add after the loop:

```rust
// The crate is beside the blocks, where `path = "../../wafer_guest"` finds
// it from `seed/blocks/<name>/`, and it is not a seed entry: an import must
// not mistake it for a block.
assert_eq!(
    entries["seed/wafer_guest/src/lib.rs"],
    impresspress_core::blocks::dev::scaffold::GUEST_LIB_RS.as_bytes()
);
let seed_manifest: serde_json::Value =
    serde_json::from_slice(&entries["seed/manifest.json"]).expect("seed manifest");
assert!(
    !seed_manifest.to_string().contains("wafer_guest"),
    "the guest crate is not a seed entry"
);
```

Add a test that an export of a generation with NO block carries no `seed/wafer_guest/` entries (use whichever fixture in the file builds a site-only generation; if none exists, build one from the site-only path the file already exercises).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p impresspress-core --features block-dev --test dev_export` — Expected: FAIL on the missing entries.

- [ ] **Step 3: Implement**

In `export.rs`, after the `for spec in &manifest.blocks { … }` loop:

```rust
// The crate every block depends on, once, beside the blocks. It is not a
// seed entry — `seed::import` reads `SeedManifest`, and a block that
// happened to be called `wafer_guest` would be exactly the confusion to
// avoid — so it is an archive entry the manifest above never lists. From
// `seed/blocks/<name>/` the template's `path = "../../wafer_guest"` resolves
// here, which is what makes an exported block buildable on a host toolchain.
if !manifest.blocks.is_empty() {
    for (path, content) in scaffold::guest_files() {
        seed_entries.push(Entry {
            path: format!("{}wafer_guest/{path}", archive_seed_prefix()),
            bytes: content.into_bytes(),
        });
    }
}
```

Make sure `handle_manifest` (the `X-Export-Bytes` / entry listing) sees these entries the same way it sees block sources — `the_manifest_describes_the_archive_entry_for_entry` will tell you. Update the layout doc at lines 1–21 and `export-readme.md` ~42–45: say the crate is at `seed/wafer_guest/` and that `cargo build --release --target wasm32-wasip1` inside `seed/blocks/<name>/` rebuilds a block with a host toolchain (no registry needed).

- [ ] **Step 4: Run**

Run: `cargo test -p impresspress-core --features block-dev --test dev_export --test dev_seed` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/impresspress-core
git commit -m "feat(dev): export the guest crate beside the blocks"
```

### Task 5: The page hands the compiler the guest and reports its version

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/assets/compiler-adapter.js` (constructor; `#start` ~416–461; `#runCompile` ~466–499; `CompileResult` typedef ~120–135)
- Modify: `crates/impresspress-core/src/blocks/dev/assets/dev.js` (`ensureCompiler` ~996–1021, `snapshotBlock` ~1100–1225, `compileBlock` staging ~1362–1385, tool description ~1405–1416)
- Test: `crates/impresspress-core/src/blocks/dev/assets/test/dev_compile_block.test.mjs`, `harness.mjs`
- Modify: `crates/impresspress-core/src/blocks/dev/page.rs` (the api-URL scrape test near ~440–455 counts `/b/dev/api/` URLs in `dev.js`; it may need the new URL added to its expected set)

**Interfaces:**
- Consumes: `GET /b/dev/api/guest` (Task 3). Protocol `init.guest` (Task 6, defined here so both sides match):
  `{ type: 'init', id, guest: { files: Record<string,string>, warmup: { crateName: string, files: Record<string,string> } } }`.
- Produces: `new BrowserRustCompiler(manifest, { guest })` where `guest` is the `GuestResponse` JSON; `CompileResult.guestVersion: number | null`; the staging request's `wafer_guest_version` = `built.guestVersion`.

- [ ] **Step 1: Write the failing page tests**

In `dev_compile_block.test.mjs`:
- Change `HELLO` to two files and the first `snapshotBlock` test to expect `['Cargo.toml', 'src/lib.rs']` with no `guestVersion` property at all (`assert.equal('guestVersion' in snapshot, false)`).
- Delete the two tests about the version read out of the module ("reports the guest version the block actually carries", "reports a module it cannot find a version in as unknown").
- Add:

```js
test('the compiler is constructed with the guest the API hands out, and the staging request carries its version', async () => {
  const seen = { guest: null, staged: null };
  const guest = {
    version: 2,
    files: { 'Cargo.toml': '[package]\nname = "wafer_guest"\n', 'src/lib.rs': 'pub const WAFER_GUEST_VERSION: u32 = 2;\n' },
    warmup: { crate_name: 'hello', files: { 'Cargo.toml': CARGO_TOML, 'src/lib.rs': LIB_RS } }
  };
  class Stub {
    constructor(manifest, options) { seen.guest = options.guest; }
    async initialize() { return 'rustc 1.90.0-nightly (fake)'; }
    async compile() { return { ...BUILT, guestVersion: 2 }; }
  }
  const { handle } = instantiate({
    workspace: HELLO,
    compiler: Stub,
    routes: { 'GET /b/dev/api/guest': guest, 'POST /b/dev/api/builds/stage': (body) => { seen.staged = body; return STAGED_OK; } }
  });
  await handle.compileBlock('hello');
  assert.deepEqual(seen.guest, guest);
  assert.equal(seen.staged.wafer_guest_version, 2);
});
```

Adapt `routes`/`STAGED_OK` to how the harness fakes `api.get`/`api.post` (read the existing "a successful compile stages the artifact" test and reuse its fixtures).

- [ ] **Step 2: Run to verify they fail**

Run: `node --test crates/impresspress-core/src/blocks/dev/assets/test/dev_compile_block.test.mjs` — Expected: FAIL.

- [ ] **Step 3: Implement the adapter**

`compiler-adapter.js`:
- Constructor: `constructor(manifest, { initSilenceMs, guest } = {})`. Validate `guest` when present: `version` a non-negative integer, `files` an object whose values are strings, `warmup.crate_name` a non-empty string, `warmup.files` likewise; throw `TypeError` otherwise (same style as the `compile` argument checks). Store `this.#guest = guest ?? null`.
- `#start`: the init message becomes

```js
const init = { type: 'init', id };
if (this.#guest) {
  // The protocol's spelling (`crateName`) for the API's (`crate_name`);
  // this is the one place the two meet.
  init.guest = {
    files: this.#guest.files,
    warmup: { crateName: this.#guest.warmup.crate_name, files: this.#guest.warmup.files }
  };
}
```

- `CompileResult` gains `guestVersion: number | null` = `this.#guest ? this.#guest.version : null`; set it where `compilerVersion` is set. Update the typedef and the header comment's session sketch (`initialize` now includes the warm-up build; say "~7 s, plus ~30 s building the guest crate once").

- [ ] **Step 4: Implement the page**

`dev.js`:
- `ensureCompiler`: before constructing, `if (!guestCrate) { guestCrate = await json(await api.get('/b/dev/api/guest')); }` (a module-level `var guestCrate = null;` beside `compilerManifest`), then `compiler = new BrowserRustCompiler(compilerManifest, { guest: guestCrate });`. Comment: the guest is fetched once per page, because the session it starts is built from it and a page that re-fetched it mid-session would report a version its worker did not build.
- `snapshotBlock`: delete `guestVersion` and the `src/wafer_guest.rs` regex branch and its comment; drop `guestVersion` from the returned object.
- `compileBlock`: `wafer_guest_version: built.guestVersion` in the staging body.
- Tool description for `dev_compile_block`: "Compile blocks/<name>/ with the in-browser Rust toolchain (wasm32-wasip1; the only dependency is the wafer_guest SDK crate, built once per session). …" keep the rest.
- `page.rs`: if the api-URL scrape test enumerates URLs, add `/b/dev/api/guest`.

- [ ] **Step 5: Run**

Run: `node --test crates/impresspress-core/src/blocks/dev/assets/test/*.test.mjs` — Expected: PASS.
Run: `cargo test -p impresspress-core --features block-dev page` — Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/impresspress-core/src/blocks/dev/assets crates/impresspress-core/src/blocks/dev/page.rs
git commit -m "feat(dev): start the compiler session from the guest crate the API serves"
```

### Task 6: The compiler worker builds the guest once and rebuilds blocks with `touch`

**Files:**
- Modify: `examples/dev-sandbox/compiler/src/protocol.ts` (`InitMessage`, `CompileMessage.files` doc, `ResultMessage.stdout` doc)
- Modify: `examples/dev-sandbox/compiler/src/worker-entry.ts` (header 20–25; `writeFile` ~168; `init` ~500–516; `compile` ~518–560; the `stdout` comments ~526, ~610)
- Modify: `examples/dev-sandbox/compiler/src/probe.html`, `examples/dev-sandbox/compiler/scripts/serve-probe.mjs`
- Modify: `examples/dev-sandbox/compiler/PIN.json` (`version`), `examples/dev-sandbox/compiler/README.md` (layout line 27, "The protocol", "How a compile actually happens", "What was confirmed", "Updating the pin")
- Modify: `crates/impresspress-web/tests/e2e/dev-workspace.spec.ts:564` (comment: the version is the pinned rubrc sha plus a packaging revision)

**Interfaces:**
- Consumes: the `init.guest` shape from Task 5; `crates/wafer-guest/{Cargo.toml,src/lib.rs}` and the two-file `hello` template on disk (Task 2).
- Produces: a worker that, given `init.guest`, writes `/wafer_guest/**` and `/blocks/<warmup.crateName>/**`, `touch`es them, builds the warm-up, then posts `ready`; and whose `compile` writes `/blocks/<crateName>/**`, `touch`es, builds with the Global-Constraints command, and downloads `/target/<target>/release/<crate>.wasm`.

- [ ] **Step 1: Protocol**

```ts
/** The guest SDK crate and a block to build it with, handed over at `init`. */
export type GuestCrate = {
  /** `Cargo.toml`, `src/lib.rs` — crate-relative, written under `/wafer_guest/`. */
  files: Record<string, string>;
  /** Built once after the sysroot loads, so `/target` holds the guest before the first real compile. */
  warmup: { crateName: string; files: Record<string, string> };
};

/**
 * `init` starts the toolchain: download, instantiate, load the sysroot — and,
 * when `guest` is given, build it once. `ready` is posted after that build,
 * so the first `compile` finds the guest already in `/target`. Without
 * `guest` the worker is ready as soon as the sysroot is loaded and builds
 * whatever crate it is handed, self-contained or not.
 */
export type InitMessage = { type: "init"; id: string; guest?: GuestCrate };
```

`CompileMessage.files` doc: "Paths relative to the crate root, written under `/blocks/<crateName>/`." `ResultMessage.stdout` doc: "`touch` and the `download` …" instead of `cargo clean`.

- [ ] **Step 2: Worker**

In `worker-entry.ts`:

```ts
/** Write a crate under `root` and return the absolute paths written. */
const writeCrate = (root: string, files: Record<string, string>): string[] => {
  const written: string[] = [];
  for (const [path, content] of Object.entries(files)) {
    const absolute = `${root}/${path.replace(/^\/+/, "")}`;
    writeFile(absolute, content);
    written.push(absolute);
  }
  return written;
};

/**
 * Make cargo see the files just written.
 *
 * The VFS's write-file event replaces a file's contents without moving its
 * mtime, so cargo — which compares source mtimes against the fingerprint of
 * the last build — would call an edited crate fresh and hand back the
 * previous artifact. `touch` moves the mtime, which is the one thing cargo
 * needs, and touches nothing else: a dependency that was not rewritten
 * (`/wafer_guest`) keeps its build. This is a workaround for the VFS, not a
 * property of cargo; the root-cause fix is upstream in rubrc's write-file
 * handler and belongs to the next pin bump.
 */
const touchAll = async (paths: string[], what: string): Promise<string> =>
  runCommand(`touch ${paths.join(" ")}`, COMPILE_TIMEOUT_MS, `touch for ${what}`);

const buildCommand = (crateName: string, target: string, release: boolean) =>
  `cargo build${release ? " --release" : ""} --manifest-path /blocks/${crateName}/Cargo.toml ` +
  `--target-dir /target --target ${target} --message-format=json`;
```

`init`, after `rustc --version` and before `state = "ready"`:

```ts
if (message.guest) {
  postProgress(id, "initializing", { detail: "writing the guest crate" });
  const written = [
    ...writeCrate("/wafer_guest", message.guest.files),
    ...writeCrate(`/blocks/${message.guest.warmup.crateName}`, message.guest.warmup.files),
  ];
  await touchAll(written, "the warm-up");
  postProgress(id, "initializing", { detail: "building wafer_guest once for this session" });
  const output = await runCommand(
    buildCommand(message.guest.warmup.crateName, SYSROOT_TRIPLE, true),
    COMPILE_TIMEOUT_MS,
    "the warm-up build",
  );
  const { diagnostics, buildFinished } = parseBuild(output);
  const firstError = diagnostics.find((d) => d.severity === "error");
  if (buildFinished === false || firstError) {
    throw new Error(
      `the guest crate does not build on this toolchain` +
        (firstError ? `: ${firstError.file}:${firstError.line}: ${firstError.message}` : ""),
    );
  }
}
```

(`init` is already wrapped so a throw becomes `state = "broken"` + `error`.) The `init` signature changes to take the message: `init(message: Extract<PageMessage, { type: "init" }>)`.

`compile`: replace the file loop + `cargo clean` with

```ts
const written = writeCrate(`/blocks/${message.crateName}`, message.files);
shellLog += await touchAll(written, message.crateName);
const output = await runCommand(
  buildCommand(message.crateName, message.target, message.release),
  COMPILE_TIMEOUT_MS,
  "cargo build",
);
```

Delete the `cargo clean` comment block and rewrite the two `stdout` comments. Rewrite the header (lines 20–25): the crate lives at `/blocks/<crate>/`, the guest at `/wafer_guest/`, one `/target` for the session. Note in a comment beside `writeCrate` why stale files in `/blocks/<crate>/src/` from an earlier compile are harmless: rustc compiles only files reached from `lib.rs` through `mod`, and every file the block still has is rewritten.

- [ ] **Step 3: Probe and its server**

`serve-probe.mjs`: add routes `/guest.json` → `{ version, files: { 'Cargo.toml', 'src/lib.rs' } from crates/wafer-guest, warmup: { crateName: 'hello', files: { 'Cargo.toml', 'src/lib.rs' } from templates/hello } }` (read `version` by regex `WAFER_GUEST_VERSION: u32 = (\d+)` from the crate source — this is a test tool, not the page); `/template/table.json` → `{ crateName: 'newsletter', files }` from `templates/table`; keep `/template/hello.json` (two files now). Also `/template/legacy-hello.json`: the hello template rewritten to the OLD shape — `Cargo.toml` with an empty `[dependencies]`, `src/lib.rs` with `#[cfg(target_arch = "wasm32")] mod wafer_guest; use crate::wafer_guest::*;` and the five exports appended, and `src/wafer_guest.rs` = the crate's `lib.rs` with `pub mod abi` and the `export!` macro removed — build this string in the server with plain string surgery (find `pub mod abi {` … up to `#[macro_export]` … end of the macro) and fail loudly if the markers are not found.

`probe.html`: send `init` with `guest` from `/guest.json`; record `ready_ms` (now including the warm-up); compile `hello` (expect `compile_ms` under 10 000 on this box — assert `< 15000` as a PASS/FAIL step so a regression to the old path is caught); compile `newsletter` from `/template/table.json` (assert success and exports, log the ms — Review Focus 1's profile is the scaffolded one); compile `/template/legacy-hello.json` as a self-contained crate (assert success — Review Focus 3); keep the broken build and the stray-cancel steps; for Review Focus 5 start a second worker with a guest whose `src/lib.rs` is `fn { broken` and assert the `init` answer is `{ type: 'error' }` whose message contains `guest crate does not build`; keep the warm re-init step as the last one.

- [ ] **Step 4: Compose the dist and run the probe**

The optimised component is being composed in this worktree already (`examples/dev-sandbox/compiler/.rubrc`, log at the coordinator's scratchpad `compose.log`). Once `compose.log` ends with `compose exit=0`, set `PIN.json` `version` to `"807ace9e.2"` and run `examples/dev-sandbox/compiler/build-compiler.sh` — with the component on disk this is the 6–50 s path (vite bundle + split), and it produces `dist/807ace9e.2/`. If the composition has not finished, wait for it (do not start a second one: it needs 12.6 GB).

Run: `node scripts/run-probe.mjs 8095` from `examples/dev-sandbox/compiler` — Expected: every step PASS; record `ready_ms`, hello `compile_ms`, newsletter ms, legacy ms.

- [ ] **Step 5: README and PIN docs**

`README.md`: line 27 layout timings; "The protocol" session sketch adds `guest` to `init`; "How a compile actually happens" steps become write → `touch` → `cargo build --manifest-path … --target-dir /target` → `download`, plus a paragraph on the warm-up; "What was confirmed" gets today's numbers as a new dated run (keep the 2026-09-03 table above it, labelled as the vendored-module era); "Updating the pin" explains `version = <rubrc sha>.<packaging revision>` and that a change to `src/**` or `scripts/**` without a pin bump increments the revision and republishes. `dev-workspace.spec.ts:564` comment updated.

- [ ] **Step 6: Commit**

```bash
git add examples/dev-sandbox/compiler crates/impresspress-web/tests/e2e/dev-workspace.spec.ts
git commit -m "perf(dev): build the guest crate once per compiler session"
```

### Task 7: End-to-end specs and the fake worker

**Files:**
- Modify: `crates/impresspress-web/tests/e2e/fixtures/fake-compiler-worker.js` (~175–190: accept `init.guest`, post one `initializing` progress with detail `building wafer_guest once for this session` when it is present, then `ready`)
- Modify: `crates/impresspress-web/tests/e2e/dev-compile.spec.ts` (~243–247 two files; the reference assertion ~237 no longer reads `wafer_guest_module`)
- Modify: `crates/impresspress-web/tests/e2e/dev-compile-tool.spec.ts` (`CRATE_FILES` ~122 two entries; `buildOnHost` ~195–218 also fetches `/b/dev/api/guest` through `page.evaluate(() => fetch('/b/dev/api/guest').then(r => r.json()))`, writes its `files` under `<tmp>/wafer_guest/` and the block under `<tmp>/blocks/<BLOCK>/`, runs cargo in the block dir with `--target-dir <tmp>/target`; the `--offline` comment now says "no registry dependencies")
- Modify: `crates/impresspress-web/tests/e2e/dev-compiler.spec.ts` only if it asserts the exact `init` message shape
- Modify: `crates/impresspress-web/tests/e2e/dev-scenario.spec.ts` (~514: also expect `seed/wafer_guest/src/lib.rs` in the export)

- [ ] **Step 1: Make the edits above**

- [ ] **Step 2: Build the bundle and run the four specs locally**

From the worktree: `cargo install --path crates/impresspress --locked --debug --root ./out` (needs `crates/impresspress-web/pkg/`, already present), then `IMPRESSPRESS=./out/bin/impresspress examples/dev-sandbox/build.sh` (wasm-pack build with `browser-devtools`, then the bundle; `compiler/dist/` from Task 6 is overlaid). Serve: `python3 -m http.server 8083 -d examples/dev-sandbox/dist &`. Then from `crates/impresspress-web`:

```
TEST_PORT=8083 npx playwright test --config=tests/playwright.config.ts tests/e2e/dev-compiler.spec.ts tests/e2e/dev-compile-tool.spec.ts tests/e2e/dev-compile.spec.ts tests/e2e/dev-scenario.spec.ts 2>&1 | tee /tmp/claude-1000/-home-joris-Programs-suppers-ai-workspace-impresspress/23b285cf-01a1-4c85-97f8-6596b383db56/scratchpad/t7-e2e.log
```

Expected: all PASS; the `dev-compile:` line shows `compile_ms` under 10 000 and the `dev-scenario:` step-2 line likewise. Kill the server afterwards.

- [ ] **Step 3: Commit**

```bash
git add crates/impresspress-web/tests/e2e
git commit -m "test(dev): e2e for the guest crate session"
```

### Task 8: Docs, reference, release note, spec amendment

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/templates/reference.md` (lines 8–10, 14–19 layout, 57–67 "no dependencies" section → "one dependency, no registry", 81–82, ~417–419 add the shared-profile cost, 462–465 the `wafer-guest-version` row)
- Modify: `docs/dev-sandbox.md` (~85–115: the layout, "those three files", the 1,500-lines paragraph, "Compiling one" timings)
- Modify: `RELEASE.md`: a new "Dev sandbox: blocks depend on the `wafer_guest` crate" entry above the ABI-2 one, in the same three-part shape (what changes, your blocks, what to do: re-scaffold or edit `Cargo.toml` + the two `lib.rs` lines; old blocks still compile, just slower; exports now carry `seed/wafer_guest/`)
- Modify: `docs/superpowers/specs/2026-09-02-dev-sandbox-design.md` §20: amendment #20 — one paragraph pointing at the 2026-09-29 spec, noting §4 (prefetch now real), §8 (guest crate, warm-up, `touch`), the "in-crate modules only" assumption at ~line 201 corrected.
- Modify: `examples/dev-sandbox/measure.sh:35-40` comment (the delta no longer includes a vendored module copy; the number is unchanged)
- Check: `crates/impresspress-core/src/blocks/dev/scaffold.rs:7` "1,500 lines" figure; `docs/dev-sandbox.md:113` same.

- [ ] **Step 1: Make the edits; run `cargo test -p impresspress-core --features block-dev --test dev_scaffold`** (the reference test pins needles in the markdown) — Expected: PASS.

- [ ] **Step 2: Commit**

```bash
git add crates/impresspress-core/src/blocks/dev/templates/reference.md docs RELEASE.md examples/dev-sandbox/measure.sh crates/impresspress-core/src/blocks/dev/scaffold.rs
git commit -m "docs(dev): the guest SDK is a crate; compile times"
```

### Task 9 (coordinator): publish the dist, open the PR

- [ ] With Task 6's worker final, run `examples/dev-sandbox/compiler/build-compiler.sh` (already-composed component: seconds), `examples/dev-sandbox/build.sh --check`, then `compiler/pack-dist.sh` and `gh release create compiler-807ace9e.2 .cache/compiler-dist-807ace9e.2.tar --title 'Compiler dist 807ace9e.2' --notes 'Worker builds the guest crate once per session; blocks rebuilt with touch. Rubrc pin unchanged (807ace9e).'` on `impresspress/impresspress`.
- [ ] `git push -u origin feat/dev-compile-speed`; `gh pr create --base main` with the measurements table, the verification list, and the three follow-ups: upstream VFS mtime fix at the next pin bump, dev.impresspress.org redeploy, and PR 2's rebase order.
- [ ] Delete `target/`, `out/`, `examples/dev-sandbox/dist`, and the `.rubrc`/`.cache` composition inputs ONLY after the release asset is verified downloadable (`compiler/fetch-dist.sh` into a temp copy).
