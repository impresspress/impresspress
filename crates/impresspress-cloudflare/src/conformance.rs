//! Shared `DatabaseService` conformance wiring for the Cloudflare adapters.
//!
//! wafer-run #319 ships a backend-agnostic conformance suite
//! ([`run_conformance`]) that drives every [`DatabaseService`] op against a
//! live service and asserts the concrete observable behavior — CRUD
//! round-trips, the full `FilterOp` surface, sorted / paginated / projected /
//! OR-grouped `list`, atomic `increment_field_where`, insert-then-update and
//! windowed-counter `upsert`, grouped aggregates, raw SQL, and schema
//! management. It is the anti-drift mechanism: an impl that silently no-ops or
//! fails-open on any single op fails an assertion instead of passing silently
//! (the failure mode that matters most here — a rate-limit `upsert` that never
//! increments limits *fails open*). Native SQLite and gated PostgreSQL already
//! run it inside wafer-run; this module wires the two Cloudflare adapters in.
//!
//! # Coverage achieved here: COMPILE-TIME conformance (no live run)
//!
//! Each `_*_is_conformable` fn below typechecks — for the real, shipping
//! `wasm32-unknown-unknown` target — the exact `run_conformance(&adapter)` call
//! the reference invocation would make: the `&Adapter -> &dyn DatabaseService`
//! coercion the suite requires, plus the `.await`. Typechecking it proves the
//! adapter satisfies the full `DatabaseService` surface the suite drives, and
//! that the suite entry point exists under the enabled `conformance` feature.
//! If the trait surface drifts (a new required op, or a changed signature) or
//! the suite entry is removed/renamed/re-gated, this stops compiling —
//! surfacing the drift at the consumer rather than only inside wafer-run.
//!
//! This whole module is gated behind the off-by-default `conformance-check`
//! crate feature (which pulls in `wafer-core/conformance`), so the ~1.3k-line
//! suite never enters the production Worker wasm. CI enforces it with a
//! dedicated `cargo check -p impresspress-cloudflare --features
//! conformance-check --target wasm32-unknown-unknown` step; the functions are
//! never called, so `cargo check` typechecks them without codegen or a live
//! binding.
//!
//! # Gap: a live behavioral run needs Cloudflare-runtime infrastructure
//!
//! Neither adapter can execute the suite in the current CI:
//!
//! - [`D1DatabaseService`] wraps a `worker::D1Database` handle, which only
//!   exists inside the Cloudflare Workers runtime (workerd) bound to a live D1
//!   database. It cannot be constructed under host `cargo test` (the `worker`
//!   crate is wasm32-only and its `!Send` `JsFuture`s don't link on host), nor
//!   under `wasm-pack test --node`/`--headless` (a browser/Node has no D1
//!   binding). Smallest change that would close the gap: a `wrangler dev` /
//!   miniflare (workerd) test harness with a `[[d1_databases]]` binding — or
//!   the `@cloudflare/vitest-pool-workers` runner — invoking
//!   `run_conformance(&D1DatabaseService::new(env.d1("DB")?)).await`. The suite
//!   already drops-then-creates its own `conf_*` tables, so it is safe against
//!   a persistent D1 instance.
//!
//! - [`KvCachedD1DatabaseService`] wraps `Arc<dyn DatabaseService>` (the inner
//!   D1 adapter) plus `Arc<dyn KvBackend>`. Its cache-specific logic
//!   (`classify_table`, `read_key`, `invalidate_keys`, the version-bump
//!   coalescing) is deliberately extracted to `impresspress-core` and unit-
//!   tested there natively (`impresspress_core::cache_key`,
//!   `impresspress_core::kv`). The conformance suite is orthogonal to that: it
//!   uses `conf_*` tables, which `classify_table` returns `None` for, so the
//!   wrapper is a pure pass-through to `inner` for every suite op — the suite
//!   would validate that forwarding is faithful. Running it still needs a live
//!   inner `DatabaseService`, and the only ones available in this wasm32-only
//!   crate are D1 (needs the binding above). Smallest change that would close
//!   the gap: under the same workerd harness, wrap the live D1 adapter and a
//!   real (or in-memory) `KvBackend` and run `run_conformance(&wrapper).await`;
//!   or make `worker` a `cfg(target_arch = "wasm32")`-only dependency so the
//!   crate builds on host, then drive the wrapper natively with
//!   `wafer_block_sqlite::SQLiteDatabaseService::open_in_memory()` as `inner`
//!   and a `HashMap`-backed `KvBackend` mock (a larger, production-structure
//!   change, out of scope for a test-only wiring).

use wafer_core::interfaces::database::{conformance::run_conformance, service::DatabaseService};

use crate::{database::D1DatabaseService, kv_cached_db::KvCachedD1DatabaseService};

/// Compile-time proof (never executed) that the D1 adapter is a valid argument
/// to the shared conformance suite. See the module doc for what this enforces
/// and why a live run is infeasible without a workerd D1 binding.
#[allow(dead_code)]
async fn _d1_adapter_is_conformable(svc: &D1DatabaseService) {
    run_conformance(svc as &dyn DatabaseService).await;
}

/// Compile-time proof (never executed) that the KV-cached wrapper is a valid
/// argument to the shared conformance suite. See the module doc.
#[allow(dead_code)]
async fn _kv_cached_adapter_is_conformable(svc: &KvCachedD1DatabaseService) {
    run_conformance(svc as &dyn DatabaseService).await;
}
