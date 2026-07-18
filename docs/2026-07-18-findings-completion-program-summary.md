# CODE_REVIEW_2026-07-16 findings — completion program summary

**Date:** 2026-07-18  **Scope:** complete every finding in `docs/CODE_REVIEW_2026-07-16_FINDINGS.md`, including the benchmark-gated items (measure first, apply if it helps).

Producer(wafer-run)-first, then consumer(impresspress) pin-bump. Each change was worktree-isolated, adversarially reviewed by an independent agent, and CI-gated before squash-merge.

## Merged PRs

### wafer-run (producers) — #315–#321
| PR | What |
|----|------|
| #315 | Upload-direction streaming wired into dispatch (`storage.put_streaming`) |
| #316 | DNS + redirect-aware SSRF on the native outbound-fetch path (SEC-019) |
| #317 | Trim sea-query to needed backends (drop MySQL renderer + derive proc-macro stack) |
| #318 | Opaque cursor pagination for storage list (additive) |
| #319 | Backend-agnostic `DatabaseService` conformance suite (off-by-default `conformance` feature) |
| #320 | `DbExec::run_batch` primitive; batch list (count+select) and update (update+refetch) |
| #321 | 4 Postgres backend bugs surfaced by the conformance suite (timestamptz bind reverted as a documented column-type follow-up; aggregate `COALESCE`, rate-limiter upsert `ON CONFLICT` qualification, and NUMERIC decode fixed) |

### impresspress (consumers) — #42–#57
| PR | What |
|----|------|
| #42 | Split `files/pages_user.rs` → `buckets`/`objects`/`cloudstorage` submodules |
| #43 | Split `llm/routes.rs` (1596 lines) → domain submodules |
| #44 | CF D1 `run_batch` override — one round-trip list/update |
| #45 | CF R2 streaming PUT via multipart (R2 rejects unknown-length streams) + native list-cursor threading |
| #46 | `--locked` on all resolving CI cargo commands |
| #47 | Split `files/storage.rs` (1243 lines) → domain submodules |
| #48 | Split `products/handlers.rs` (1025 lines) → domain submodules |
| #49 | Split `builder.rs` (804 lines) → `builder/` submodules (5th and last module split) |
| #50 | Opaque cursor pagination for the browser (OPFS) storage adapter |
| #51 | Benchmark: externalize CF static assets → **DEFER** (docs-only; see below) |
| #52 | Drop the dead direct `sea-query` dep (consumer follow-up to #317) |
| #53 | Benchmark: wasm `simd128` + crypto `opt-level` → **both rejected** (docs+comment; see below) |
| #54 | Close the CF-path SSRF gap: `is_ssrf_blocked_url` helper wired into the CF fetch path, `validate_url_value`, and LLM provider write+call paths (consumer of #316) |
| #55 | Wire the #319 conformance suite into the D1/KV/browser adapters (compile-time conformance behind an off-by-default `conformance-check` feature) |
| #56 | SSRF follow-up: revalidate every LLM redirect hop + block the bare `metadata` host |
| #57 | Bump wafer-run pin 10eb4e3 → 915d992 (delivers #321's `wafer-sql-utils` fixes to impresspress) |

## Module splits (5/5)
`files/pages_user.rs` (#42), `llm/routes.rs` (#43), `files/storage.rs` (#47), `products/handlers.rs` (#48), `builder.rs` (#49). Each verified as a pure move: item counts before==after, tests moved verbatim with identical pass counts, visibility preserved via `pub(in crate::…)` re-exports (no over/under-exposure), route/dispatch tables byte-identical. The builder split additionally verified the `use_static_blocks!` linker anchors stay effective (crate-level `use ::krate as _;` linkage, independent of module placement).

## Benchmark-gated outcomes (measured, not guessed)
- **Static-asset externalization → DEFER (#51).** Embedded assets are ~285 KB source / ~376 KB in the CF `.wasm`, but that's **100% data segment**. The binding CF constraint is the 400 ms startup-CPU cap, driven by Liftoff compiling the **code** section; data segments are `memcpy`'d at instantiation, so removing them buys ≈0 cap headroom. A clean externalization would also fork the CF-vs-native asset path (native legitimately embeds for a self-contained binary). Revisit only if the CF `.wasm` approaches the ~6 MB budget **and** profiling shows data (not code) is the binding contributor.
- **wasm `simd128` + crypto `opt-level` → both rejected (#53).** `-Oz`/LTO/1-CGU/strip/panic=abort were already in place. A per-package `opt-level=3` on the crypto crates is a byte-for-byte no-op under `lto=true` (fat LTO re-optimizes the merged module at `-Oz`, proven by identical output hash). `simd128` saves ~150 B because the CF worker has no SIMD-eligible code (`-Oz` disables autovectorization; only the browser vector-search `f32x4` path benefits, and the browser build already sets it), and standardizing it collides with the `.cargo/config.toml` dev-patch workflow (local≠CI drift). Note: the browser wasm build (`just build`) already uses `+simd128`.
- **sea-query consumer trim → dead-dep removal (#52).** impresspress-core's direct `sea-query` dep was entirely unused (reached only transitively via `wafer-sql-utils`), and being default-on it silently re-enabled `backend-mysql` + derive graph-wide, defeating #317's trim. Removed it. Bundle is byte-identical (wasm-ld already DCE'd the MySQL renderer) — the win is build-graph hygiene, not shipped bytes.

## SSRF (#54 + #56)
`is_ssrf_blocked_url` composes #316's IP/URL classifiers (private/loopback/link-local/CGNAT/multicast/reserved + IPv6-embedded-v4 forms) with a cloud-metadata-host denylist. Wired into: the CF `WorkerFetchService` fetch precheck (buffered + streaming), `validate_url_value` (closing CGNAT/multicast/IPv6-embedded gaps the old hand-rolled predicate missed), and the LLM provider write+call paths. Adversarial review found no bypass on the gated paths (numeric-IP encodings all normalize→blocked; userinfo stripped). #56 additionally revalidates every LLM **redirect hop** against the same classifier — closing redirect-to-internal while preserving the `http://localhost` self-hosted-LLM affordance (the policy fires only on 3xx, never the initial request) — and blocks the bare `metadata` short-name. Honest residual: DNS rebinding on a fixed admin-set endpoint (no resolve-before-connect hook in a Worker / reqwest client); documented, not overstated.

## Conformance wiring (#55)
The three impresspress DB adapters (CF D1, CF KV-cached D1, browser OPFS) are wasm-only and need runtime infra CI doesn't provision, so each got a **compile-time conformance check**: a never-executed fn that typechecks the exact `run_conformance(&adapter).await` coercion against the pinned wafer-core signature — genuine anti-drift (a future suite-signature change fails impresspress compilation), behind an off-by-default `conformance-check` feature so the ~1.3k-line suite never enters the production Worker wasm. The browser check is wired into an unconditional CI step (`cargo check --locked --tests -p impresspress-browser --target wasm32`) so it fires on a root-only pin bump — the exact drift scenario. Documented per-adapter the honest ceiling (real behavioral runs need miniflare/workerd D1 or a Node sql.js+OPFS double).

## End-to-end validation
`examples/run-tests.sh` (build against the pinned wafer-run rev + drive each example app under Playwright): **dropship 13, saas 16, blog 14, blog web-target 1 — 44/44 passed, "All examples passed!"**

## Notes for the maintainer
- **Local dev environment:** the sibling `../wafer-run` checkout is on a stale `deleak-runtime` branch (~45 commits behind main, no `conformance` feature), and impresspress's local `.cargo/config.toml` patches wafer-* to it. A patched local `just build` therefore fails until `../wafer-run` is updated to main (or the patch is removed). The shipped code is unaffected — CI and this program's e2e build against the git-pinned rev, which is correct. (This is the known pin-vs-local-checkout drift, not a branch break.)
- **Unmerged local branch:** `fix-remaining-blue-purple` (commit `ed94a3f`, "finish the brand sweep — no blue/purple anywhere + working LLM thread creation") predates this program and is not part of it. It was preserved (not merged, not discarded) — decide separately whether to land it.
- **Optional future producer follow-ups (non-blocking, documented):** fold the cloud-metadata-host denylist into `wafer-net-security`/`wafer_core::security` so native fetch + registry downloads share it; expose the storage cursor codec so backends can share one encoder. Neither was needed for this program (all consumer changes reused the current pin).
