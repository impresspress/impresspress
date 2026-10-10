# Browser-compiled blocks in ImpressPress

## Verdict

This is feasible with a split architecture:

- ImpressPress already runs as a module service worker.
- A separately compiled wasm32-wasip1 WAFER block can be loaded from bytes and
  executed inside that service worker by enabling WAFER's wasmi runtime.
- Rubrc (or a similar in-browser Rust toolchain) must run in a **dedicated
  worker owned by an editor page**, not in the service worker. Its compiler
  currently creates nested workers and uses SharedArrayBuffer/Atomics; service
  workers cannot create those workers and are not reliable hosts for
  long-running compilation.
- Adding new guest blocks is realistic. Editing ImpressPress's existing native
  blocks in-place is not: those blocks are linked into the outer
  wasm32-unknown-unknown application and require rebuilding the whole
  wasm-bindgen service-worker package.

The practical first version is therefore a browser IDE for small, dynamically
loaded WAFER guest blocks rather than a browser rebuild of ImpressPress itself.

## What this spike proves

The dynamic-wasm-blocks feature enables wafer-run/wasmi for impresspress-web.
The guest in this directory is deliberately dependency-free and exports WAFER's
stable ABI-v1 JSON interface by hand. That matches the currently documented
overlap between Rubrc and WAFER: Rubrc targets wasm32-wasip1, while external
dependencies and procedural macros are not yet a dependable part of its public
workflow.

The spike's native verifier loaded the resulting bytes with
WasmiBlock::load_from_bytes, checked the block metadata and lifecycle, invoked
the handler, and received:

    Hello from a browser-compiled WAFER block!

The following gates passed:

| Check | Result |
| --- | --- |
| ImpressPress browser host with wafer-run/wasmi | cargo check passed |
| Full wasm-bindgen package with the feature | wasm-pack build passed |
| Dependency-free guest for wasm32-wasip1 | 28,469 bytes |
| Native verifier executing the guest via wasmi | passed |

The native verifier has since been removed. The dev sandbox's end-to-end test
(`crates/impresspress-web/tests/e2e/dev-foundations.spec.ts`) stages this guest
into the browser sandbox's runtime on every CI run, which loads it with wasmi
and serves its route, and asserts the same greeting. That covers what the
verifier did, in the host the guest is for.

Host-size cost measured from release builds:

| Artifact | Baseline | With wasmi | Increase |
| --- | ---: | ---: | ---: |
| Raw Wasm | 8,382,892 | 10,482,007 | 2,099,115 (25.0%) |
| wasm-opt -Oz | 6,694,877 | 8,213,495 | 1,518,618 (22.7%) |
| Optimized + gzip -9 | 2,315,677 | 2,819,373 | 503,696 (21.8%) |

This is an opt-in feature so the current production bundle does not pay that
cost.

## Proposed browser architecture

    isolated /b/dev editor page
      |
      +-- Monaco/source files
      |
      +-- dedicated compiler Worker
      |     +-- Rubrc toolchain
      |     +-- wasm32-wasip1 guest.wasm
      |
      +-- install message + content hash
            |
            v
    ImpressPress service worker
      +-- persist source/artifact/manifest in OPFS or Cache Storage
      +-- WasmiBlock::load_from_bytes(guest.wasm)
      +-- rebuild WAFER router with extra_block + add_route
      +-- atomically replace the active runtime
            |
            v
    same-origin requests execute the new block

The compiler assets should be served from the same origin. The editor route
needs cross-origin isolation headers:

    Cross-Origin-Opener-Policy: same-origin
    Cross-Origin-Embedder-Policy: require-corp

Apply these to the editor route and its compiler assets, not automatically to
the whole site. ImpressPress's popup OAuth flow uses window.open, event.source,
and popup.closed; a global opener policy can disrupt that relationship. The
existing CSP already includes wasm-unsafe-eval, which is needed to compile
dynamically supplied WebAssembly when CSP is enabled.

## Runtime work still required

The current browser service-worker runtime is intentionally single-shot:
store_runtime installs one Wafer, and dispatch keeps a raw pointer across an
await. That is valid only while the runtime can never be replaced.

Before live block installation, change the stored runtime to
RefCell<Option<Rc<Wafer>>>. Dispatch should clone the Rc before awaiting.
Installation can then:

1. Validate the guest and its manifest.
2. Rebuild a complete, sealed WAFER router from the base registrations plus all
   enabled dynamic blocks.
3. Replace the stored Rc in one synchronous step.

In-flight requests retain the old Rc; new requests see the new runtime. This
avoids mutating WAFER after it is sealed and avoids invalidating a pointer.
Initialization should also be split so rebuilding the router does not rerun
one-time database setup.

## Rubrc constraints

Rubrc is currently a pre-release application rather than a small embeddable
compiler SDK. Its public app owns the editor, virtual filesystem, compiler
worker, and several subordinate workers. Its published compiler manifest also
describes a large payload: approximately 384 MB uncompressed and 55.9 MB
compressed.

The normal WAFER guest authoring path uses the wafer-sdk crate and the
#[wafer_block] procedural macro. The current Rubrc documentation says external
dependencies and procedural macros are unsupported, so the proof guest uses a
tiny handwritten ABI adapter. There are three sensible ways forward:

1. Start with dependency-free block templates and generate the ABI adapter.
2. Wait for Rubrc's Cargo dependency/procedural-macro support to stabilize.
3. Extract or contribute a reusable Rubrc compiler component and explicitly
   support a preloaded wafer-sdk dependency graph.

The first option is enough to prove an end-to-end browser editor now.

## Delivery sequence

1. Add an artifact store and runtime rebuilding/safe swap. Initially install a
   precompiled guest through an authenticated development endpoint.
2. Add the isolated editor page and run Rubrc in its dedicated compiler worker.
3. Compile this no-dependency template, validate it, persist it, and ask the
   service worker to install it.
4. Add generated bindings or full wafer-sdk support when the compiler can
   resolve dependencies and procedural macros.
5. Harden before accepting untrusted code: admin-only installation, immutable
   content hashes, route ownership checks, capability declarations defaulting
   to none, execution/memory limits, last-known-good rollback, and retained
   source/build logs.

## Reproduce the proof

From the repository root:

    cargo check --locked -p impresspress-web \
      --target wasm32-unknown-unknown \
      --features dynamic-wasm-blocks

    cargo build --offline --release \
      --target wasm32-wasip1 \
      --manifest-path experiments/browser-service-worker-blocks/guest/Cargo.toml

    cd crates/impresspress-web
    wasm-pack build --target web --release -- \
      --features dynamic-wasm-blocks

To run the guest in the sandbox, point `PROOF_GUEST_WASM` at
`experiments/browser-service-worker-blocks/guest/target/wasm32-wasip1/release/browser_compiled_wafer_block.wasm`
and run `npm run e2e:dev` in `crates/impresspress-web` against a dev-sandbox
build, as the `e2e-dev-sandbox` job in `.github/workflows/ci-shared.yml`
does.

## References

- Rubrc: <https://github.com/oligamiq/rubrc>
- Worker API availability: <https://developer.mozilla.org/en-US/docs/Web/API/Worker>
- Service Worker API restrictions:
  <https://developer.mozilla.org/en-US/docs/Web/API/Service_Worker_API>
- Cross-origin isolation:
  <https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Cross-Origin-Embedder-Policy>
- WebAssembly CSP:
  <https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Content-Security-Policy/script-src>
- Service-worker background lifetime:
  <https://developer.mozilla.org/en-US/docs/Web/Progressive_web_apps/Guides/Offline_and_background_operation>
