# Externalize embedded static assets out of the CF worker wasm — benchmark & recommendation

**Date:** 2026-07-18
**Finding origin:** CODE_REVIEW_2026-07-16, benchmark-gated — *"Externalize static assets to Workers Static Assets / R2."*
**Status:** **DEFER.** Measured; the win is data-segment-only and does not relieve the
actual production constraint (the Workers startup-CPU cap). A clean externalization would
fork the asset subsystem CF-vs-native and touch the external worker scaffold, for a modest
download saving on a worker that already has comfortable headroom. Revisit only under the
trigger below.

---

## 1. What is embedded, and how much

Source: `crates/impresspress-core/src/ui/assets.rs` (`include_str!` / `include_bytes!` of
`crates/impresspress-core/src/ui/assets/`). Served entirely in-process by the
`impresspress/system` block (`blocks/system.rs`) at the `/b/static/…` prefix, with
content-hashed URLs (`short_hash()` = first 8 hex of SHA-256) and
`Cache-Control: public, max-age=31536000, immutable`.

Embedded in the **Cloudflare** worker build (`block-llm` is *off* on CF — the LLM provider
isn't wasm32-compatible; `block-files` is *on* via the `full` preset that the live deploy uses):

| Asset | Bytes | Kind |
|---|---:|---|
| tokens.css + base.css + components.css + layout.css + charts.css | 80,651 | text (compressible) |
| htmx.min.js | 50,917 | text |
| itim-latin.woff2 | 46,200 | binary (already compressed) |
| itim-latin-ext.woff2 | 38,312 | binary |
| impresspress-logo.png | 45,580 | binary |
| impresspress-logo-long.png | 9,593 | binary |
| favicon.ico | 5,799 | binary |
| files-browser.js (`block-files`) | 14,553 | text |
| **Total embedded on CF** | **291,605** (≈ 285 KB) | |

Excluded on CF because `block-llm` is off (do **not** count these against the CF wasm):
marked.min.js 36,546 + purify.min.js 22,216 + llm-chat.js 19,480 = 78,242 bytes.

## 2. Empirical `.wasm` attribution (measured, not estimated)

Method: build `impresspress-cloudflare` as a `wasm32-unknown-unknown` release cdylib
(`--features full`, worker 0.7.5 toolchain). A bare cdylib DCEs the assets (the worker entry
lives in the *consumer* crate — see §4), so a temporary `#[no_mangle]` root that observes each
asset's data **pointer** (`.as_ptr()` — `.len()` is a const the optimizer folds without
retaining the bytes) forces the data segments in. Then the same build with every embedded file
stubbed to empty. Identical code in both; the delta is pure asset data. (Temp root reverted;
this PR is docs-only.)

| Build | Raw `.wasm` | gzip | data section |
|---|---:|---:|---:|
| assets present | 1,006,303 | 430,311 | 388,308 |
| assets stubbed to empty | 630,112 | 156,133 | 12,163 |
| **Δ (asset attribution)** | **376,191** | **274,178** | **376,145** |

- The delta is **100 % data segment** — code changed by only 46 bytes. Embedded assets are
  raw bytes in the wasm data section; they are **not** compiled by V8's Liftoff startup compiler.
- The raw delta (376 KB) exceeds the 291.6 KB source sum by ~84 KB because LLVM **duplicates**
  the incompressible const binaries — they are referenced from two sites (the byte accessor
  *and* `short_hash()` inside the `*_url()` function). Verified in the wasm: `wOF2` header ×4
  (for 2 fonts), PNG `IHDR` ×5 (for 2 PNGs). The gzip delta is inflated by the same duplication
  (the 46 KB fonts exceed gzip's 32 KB window, so the copies don't fully dedupe).

## 3. Why this does not move the needle where it matters

The binding Cloudflare Workers constraint (see the workspace note
`wafer-site-wasm-size-budget`) is the **400 ms startup-CPU cap**: exceed it and every cold
start fails with `error code: 1102`. That cap is driven by **function compilation** (Liftoff,
~10–15 MB/s over the *code* section), not by data segments — active data segments are `memcpy`'d
into linear memory at instantiation (µs for 376 KB, effectively free).

Live deploy state at last measure (2026-05-19): **4.80 MB** uncompressed / 4.01 MB `wasm-opt -Oz`
/ **1.36 MB gz**, with a **~270 ms cushion** under the 400 ms cap.

Externalizing the assets would therefore deliver:

- Uncompressed wasm: −376 KB → ~4.42 MB (**−7.8 %**).
- Module download (gz): −274 KB → ~1.09 MB (**−20 %** transfer).
- **Startup-CPU cap relief: ≈ 0** — the 1102 risk is code-bound, not data-bound. Removing data
  segments does not reduce Liftoff work, so it buys no headroom against the actual cap.
- Secondary: `/b/static/*` requests would be served at the edge without spinning up the isolate
  → marginally fewer invocations / lower CPU-time billing. Small in practice: the assets are
  `immutable`, 1-year cached, so browsers and CF's cache already avoid re-hitting the worker
  after first load.

## 4. Where the change would have to happen (in-repo vs external scaffold)

`impresspress-cloudflare` is a **library** (`run()`); it has no `#[event(fetch)]` and no
`target-cloudflare` feature. `worker-build` runs in the **consumer repo root**
(`crates/impresspress/src/cli/helpers/cloudflare/build.rs`, `--features target-cloudflare`),
i.e. the deployable cdylib + `#[event(fetch)]` live **outside this repo** (e.g. wafer-site).
The `wrangler.toml`, however, **is** generated in-repo by
`crates/impresspress/src/cli/helpers/cloudflare/wrangler.rs` (`impresspress build --target cloudflare`).

**Workers Static Assets** (the correct target if we externalize at all) serves files by literal
path from an on-disk assets directory bound to the worker; matched GET requests are served at the
edge and never invoke the worker. That fits `/b/static/*` cleanly (dedicated prefix, no route
collision). **R2 is the wrong tool here**: serving app chrome from R2 either (a) still invokes the
isolate and adds a binding round-trip per asset, or (b) needs a public bucket on another origin —
which defeats the self-contained, no-cross-origin design the asset comments in `assets.rs`
explicitly call out. R2 remains correct for *user-uploaded* files (already used via the storage
service), not for app chrome.

The blocking coupling is that the content-hashed URLs are computed **at render time from the
embedded bytes**. `*_url()` (`css_url`, `htmx_js_url`, `favicon_url`, `logo_icon_url`,
`itim_latin_woff2_url`, …) call `short_hash(EMBEDDED_BYTES)` and are consumed all over the render
path (`ui/layout.rs`, `ui/shell.rs`, `config_vars.rs`, `blocks/auth_ui`, `blocks/files`,
`blocks/userportal`, `blocks/llm/pages.rs`, `blocks/system.rs`). To drop the bytes from the wasm
the worker must be **given** those hashes at build time instead of computing them — the bytes
cannot both leave the binary and remain hashable at runtime.

A clean (no-shim, single-source-of-truth) externalization is therefore a multi-part change:

1. **Build tool (in-repo).** Compute each asset's `short_hash`, write
   `<name>-<hash>.<ext>` into an assets dir, and emit the resulting URLs as build-time config —
   from the *same* computation that names the files, so filename and URL can never drift (avoids
   the magic-code hazard CLAUDE.md forbids). The CSS bundle's font-URL substitution must be
   reproduced here (or the bundling must move into the build tool) so the CSS hash matches.
2. **Render path (in-repo).** `*_url()` on CF returns the injected URL constant instead of
   hashing embedded bytes; the embedded bytes are dropped from the CF build.
3. **System block (in-repo).** Remove the `/b/static/*` serving table on CF (Static Assets serves
   it); keep it for native.
4. **wrangler generator (in-repo).** Add `[assets] directory = …` (+ routing so `/b/static/*`
   resolves to assets) to the generated `wrangler.toml`.
5. **External consumer scaffold (out of repo).** The assets dir must physically exist at the
   consumer repo root at deploy time (that's where `worker-build` runs), and the consumer's
   build must invoke the in-repo build tool to populate it.

Crucially, native (`impresspress-native`) legitimately embeds these assets — a single
self-contained binary with no external asset dir is a *feature* there ("ships its own glyphs,
no cross-origin runtime dependency", per the `assets.rs` comments). So to actually remove the
bytes from the CF wasm, CF must use a **different** asset-provisioning path than native. The
size win and a single shared asset path are mutually exclusive: keep one path and the bytes stay
in the wasm (no win); split the path and you fork the asset subsystem CF-vs-native.

## 5. Decision & trigger to revisit

**Defer.** The measured cost is ~376 KB of wasm **data** (~274 KB gz), it delivers **no relief**
against the startup-CPU cap that is the real production constraint, the live worker sits ~270 ms
under that cap, and a correct implementation forks the asset subsystem CF-vs-native (against
impresspress's self-contained design) and reaches into the external worker scaffold. Not worth
doing now.

**Revisit if and only if** the CF worker wasm approaches the ~6 MB budget **and** profiling
(`twiggy top` / `wasm-tools objdump`) shows the **data section** — not the code section — is the
binding contributor. In that specific case, implement §4 steps 1–5 as a single clean change
(CF-only asset path via build-time hash injection + `[assets]` in the generated `wrangler.toml`),
keeping the native embedded path intact. Until then, cheaper, code-section levers (block feature
gating, stripping `wasmi` from the CF build, `wasm-opt -Oz`, `panic = "abort"` — all in the size
budget note) dominate, because they cut the code that Liftoff actually compiles.

### Reproduce the measurement

```bash
# baseline
cargo build -p impresspress-cloudflare --target wasm32-unknown-unknown --release --features full
# add a temp #[no_mangle] root in impresspress-cloudflare/src/lib.rs that black_box'es
# each asset's .as_ptr() (NOT .len()), rebuild -> "assets present"
# `: > ` each embedded file under ui/assets/, rebuild -> "assets stubbed"
wasm-tools objdump target/wasm32-unknown-unknown/release/impresspress_cloudflare.wasm | grep '^  data'
gzip -c target/wasm32-unknown-unknown/release/impresspress_cloudflare.wasm | wc -c
```
