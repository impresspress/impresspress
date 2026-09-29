# Dev Sandbox Site-Edit Speed Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `dev_write_file` under `site/` is visible in the sandbox preview in well under 150 ms and stays that fast for a whole session.

**Architecture:** The browser database flushes once per request instead of once per mutation (a per-request deferred-flush scope opened by the service worker's dispatch); the request-log row, retention and GC run after the reply through the after-response queue core already has for Cloudflare, kept alive by `event.waitUntil`; a site-only activation validates only the blobs it adds; the browser storage service keeps a bounded read cache; the page learns of activations by a message from the service worker and reloads (or hot-swaps CSS) without an extra status round trip.

**Tech Stack:** Rust (wasm32 for `impresspress-browser`/`impresspress-web`, host for `impresspress-core`), `wasm-bindgen`/`web-sys`, sql.js + OPFS, the service worker template in `impresspress-bundle`, plain-JS page (`dev.js`) with `node --test`, Playwright e2e.

**Spec:** `docs/superpowers/specs/2026-09-30-dev-site-edit-speed-design.md`

## Global Constraints

- A reply from `dev_write_file` still means the generation is active and durable: the request's one flush happens before the response is returned. Only the request-log row and `maintain` (retention + GC) move after the reply.
- Every write still creates a generation; retention stays at 20; the activation queue and coalescing are unchanged.
- The service worker is the only writer of OPFS and the sql.js database; nothing else may cache or write them.
- The flush scope is explicit at one call site (`dispatch_request`) and correct under interleaved requests: each request flushes once at its own end if it mutated, never zero times.
- Request-log policy for browser builds defaults to `errors` (`IMPRESSPRESS_REQUEST_LOG`), set as infrastructure config in the browser runtime factory, never stored in the database.
- The storage read cache is bounded (16 MiB total, entries ≤ 1 MiB) and invalidated by the same service's `put`/`put_streaming`/`delete`/`delete_folder`.
- Validation: a site-only generation probes only shas not referenced by the previous active generation's site manifest; a manifest naming content that is not stored is still refused (422); rollbacks still probe fully.
- The page reacts to `{ type: "dev-generation", id, cause, changed_paths }` messages from the service worker; a CSS-only generation swaps `<link>` hrefs instead of reloading; a generation the page has already shown is ignored.
- The e2e prints exactly one line `dev-workspace: site_write_ms=<n> served_ms=<n> preview_ms=<n>` and CI's summary greps `dev-workspace:`.
- Repo rules (`CLAUDE.md`): fix at root cause, no compat shims, no sync bridges, comments are contracts, config keys keep their prefixes everywhere, no raw SQL in block code.
- Commit trailers per the harness; PR bodies end with the Claude Code footer lines.
- Implementers name scratch files with their task id under the session scratchpad and never touch `examples/dev-sandbox/compiler/` beyond reading.

## Review Focus

1. **Two requests interleaved in the service worker** (a status poll arriving while a write awaits OPFS): each must flush its own mutations at its own end; the poll must not flush the write's rows early nor the write flush zero times. Pinned by A1's `interleaved_scopes_each_flush_once` wasm test.
2. **Service worker terminated between the reply and the after-response work**: the audit row and GC may be lost, the generation may not. Pinned by A2's ordering test (flush before reply; audit after) and by the design (waitUntil keeps the worker alive for the queue).
3. **A blob the previous generation referenced but GC removed** (cannot happen: GC keeps every retained generation's blobs) — the diff-only validation must still refuse a manifest naming content that is really missing. Pinned by B1's retained 422 tests plus a new one where the previous generation's blob is deleted behind its back and a rollback to it is refused.
4. **A generation activated by another tab** must reload this tab's preview. Pinned by C1's page test (message for an unseen id reloads) and the e2e (message-driven reload, no page-local reload).
5. **A site with many files** must not get slower than today because of the cache: entries over 1 MiB are not cached, the budget evicts LRU. Pinned by B2's wasm tests (over-size object bypasses; budget eviction).

---

# PR A — storage path (worktree `../dev-site-speed-a`, branch `feat/dev-site-speed-a`, from `origin/main`)

### Task A1: One flush per request — the deferred-flush scope

**Files:**
- Create: `crates/impresspress-browser/src/flush_scope.rs`
- Modify: `crates/impresspress-browser/src/database.rs` (`with_flush_mapped` ~158, `flush_through_bridge` ~186, module doc 47–68)
- Modify: `crates/impresspress-browser/src/lib.rs` (`pub mod flush_scope;`)
- Modify: `crates/impresspress-browser/src/runtime.rs` (`dispatch_request` ~107)
- Test: inline wasm tests in `flush_scope.rs` and `database.rs` (`sql_js_conformance` provides `installMemoryOpfs` / `opfsWrites()`); `crates/impresspress-browser/src/vector/service.rs:890-935` (`MUTATING_SITES` and the "no direct dbFlush" test stay valid — the scope adds no site)

**Interfaces:**
- Produces: `flush_scope::run<F: Future<Output = T>, T>(future: F) -> (T, Result<(), String>)` — runs `future` with a fresh owed-flag current on every poll (the `Scoped` pattern of `impresspress_core::after_response`), then flushes once through `database::flush_through_bridge` if the flag was set; returns the future's output and the flush result. `flush_scope::note_mutation()` sets the current owed flag; returns `false` when no scope is current (caller flushes itself).
- Consumed by A2 (`dispatch_request` wraps the whole request), and by every `with_flush_mapped` call.

- [ ] **Step 1: Write the failing wasm tests**

In `flush_scope.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::test_support::{installMemoryOpfs, opfs_writes};
    use wasm_bindgen_test::*;

    /// Three mutations inside one scope are one export.
    #[wasm_bindgen_test]
    async fn a_scope_with_three_mutations_flushes_once() {
        let db = crate::database::test_support::fresh_db().await;
        let before = opfs_writes();
        let (_, flush) = run(async {
            for i in 0..3 {
                db.create(&format!("t{i}"), row()).await.unwrap();
            }
        })
        .await;
        flush.unwrap();
        assert_eq!(opfs_writes() - before, 1);
    }

    /// A scope that only reads writes nothing.
    #[wasm_bindgen_test]
    async fn a_scope_with_no_mutation_flushes_nothing() { /* opfs_writes() unchanged */ }

    /// Nested scopes are one scope.
    #[wasm_bindgen_test]
    async fn nested_scopes_flush_once() { /* run(async { run(async { mutate }).await; mutate }) → 1 */ }

    /// Two requests interleaved on one thread each flush once, at their own end.
    #[wasm_bindgen_test]
    async fn interleaved_scopes_each_flush_once() {
        // Drive two `run` futures with futures::join!: A mutates, yields (a
        // `yield_now`), B mutates and completes, A completes. Expect exactly
        // two exports, and that B's flush happened before A's completion
        // (record opfs_writes() inside each future right before it returns).
    }

    /// Outside any scope the old contract holds: one flush per mutation.
    #[wasm_bindgen_test]
    async fn without_a_scope_every_mutation_flushes() { /* 2 creates → 2 exports */ }
}
```

`test_support` does not exist yet: extract the `installMemoryOpfs` / `opfs_writes` / fresh-database helpers the `sql_js_conformance` module already has into a `pub(crate) mod test_support` (cfg(test)) in `database.rs` so both files use one copy.

- [ ] **Step 2: Run them, see them fail to compile**

Run: `NODE_OPTIONS=--import ./js/test/node-hooks.mjs wasm-pack test --node crates/impresspress-browser` (from the repo root; CI's exact command is in `.github/workflows/ci-shared.yml` ~628–728).

- [ ] **Step 3: Implement `flush_scope.rs`**

```rust
//! One flush per request.
//!
//! The browser database persists by exporting the whole sql.js database to
//! OPFS (`bridge.js` `dbFlush`), which costs the same whether one row or nine
//! changed. A request that activates a generation makes about nine
//! mutations; this scope lets them share one export. `dispatch_request` opens
//! it around a request; `with_flush_mapped` asks `note_mutation()` whether a
//! scope is current and, if so, records that a flush is owed instead of
//! exporting; `run` exports once when the request's work is done, BEFORE the
//! reply is returned, so a reply still means the change is durable.
//!
//! Interleaved requests: the service worker handles several fetch events on
//! one thread, so a thread-local "current scope" would let a status poll
//! close a write's scope. The flag is therefore installed on every poll of
//! the scoped future and restored afterwards (the same shape as
//! `impresspress_core::after_response::Scoped`), so each request sees only
//! its own flag.
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

thread_local! {
    static CURRENT: RefCell<Option<Rc<Cell<bool>>>> = const { RefCell::new(None) };
}

/// Record that the current scope owes a flush. `false` when no scope is
/// current: the caller must flush itself.
pub(crate) fn note_mutation() -> bool {
    CURRENT.with(|c| match c.borrow().as_ref() {
        Some(owed) => { owed.set(true); true }
        None => false,
    })
}

pub struct Scoped<F> { owed: Rc<Cell<bool>>, inner: Pin<Box<F>> }

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let previous = CURRENT.with(|c| c.replace(Some(Rc::clone(&self.owed))));
        let result = self.inner.as_mut().poll(cx);
        CURRENT.with(|c| *c.borrow_mut() = previous);
        result
    }
}

/// Run `future` as one flush scope. Nested scopes share the outer flag.
pub async fn run<F: Future>(future: F) -> (F::Output, Result<(), String>) {
    let outer = CURRENT.with(|c| c.borrow().clone());
    let owed = outer.clone().unwrap_or_else(|| Rc::new(Cell::new(false)));
    let output = Scoped { owed: Rc::clone(&owed), inner: Box::pin(future) }.await;
    if outer.is_some() {
        return (output, Ok(())); // the outer scope flushes
    }
    let flush = if owed.get() { crate::database::flush_through_bridge().await } else { Ok(()) };
    (output, flush)
}
```

In `database.rs`, `with_flush_mapped` becomes: run `end_open_transaction()` as today, `let result = op.await;` then `if crate::flush_scope::note_mutation() { return result; }` else `flush_through_bridge().await` + `resolve_flush_outcome` as today. Make `flush_through_bridge` `pub(crate)`. Update the module doc (47–68): one flush per logical mutation *outside a scope*, one per request inside.

Do NOT touch `dispatch_request` in this task (A2 does); the tests drive `run` directly.

- [ ] **Step 4: Run the wasm tests; all pass; existing `flush_precedence`/`sql_js_transactions` flush-count tests still pass (they run outside a scope)**

- [ ] **Step 5: Commit** — `perf(browser): flush the database once per request scope`

### Task A2: After-response work in the service worker, and the request-log policy

**Files:**
- Modify: `crates/impresspress-browser/src/runtime.rs` (`dispatch_request` ~107)
- Modify: `crates/impresspress-web/src/lib.rs` (`handle_request` ~269; `initialize` ~87: `deferred::set_mode(Queued)`)
- Modify: `crates/impresspress-bundle/assets/sw.js.tmpl` (fetch listener ~145–240, `handleFetch` ~276–302)
- Modify: `crates/impresspress-web/src/runtime_factory.rs` (~340–346: `.both(REQUEST_LOG_CONFIG_KEY, "errors")`)
- Modify: `crates/impresspress-core/src/config_vars.rs` (~111: doc for the browser default) and `RELEASE.md` (one entry)
- Test: wasm tests in `runtime.rs` (`runtime.rs:121` module exists); a `js/test/*.test.mjs` if the SW template is testable in node (check `crates/impresspress-bundle` for an existing sw test; if none, the e2e covers it); core test `pipeline.rs:3100` still passes.

**Interfaces:**
- Consumes: `impresspress_core::after_response::{AfterResponse, scope, persist_audit_row}`, `impresspress_core::deferred::{set_mode, DeferMode}`, `flush_scope::run` (A1).
- Produces: wasm export `handle_request(request) -> Promise<{ response: Response, after: Promise<void> }>` — the JS object carries the response and a promise for the after-response work; `dispatch_request` returns `(web_sys::Response, js_sys::Promise)`.

- [ ] **Step 1: Write the failing tests**

wasm test in `runtime.rs`: dispatching a request that writes a row (use an existing writable route in the test harness of that module, or a `DatabaseService::create` inside the scope) returns a response whose promise resolves BEFORE the request-log row exists, and the `after` promise resolving makes the row exist (policy `all` in the test) — order pinned. Second test: with policy `errors`, a 200 leaves no row after `after` resolves; a 500 does.

- [ ] **Step 2: Implement**

`dispatch_request`:

```rust
pub async fn dispatch_request(request: web_sys::Request) -> Result<(web_sys::Response, js_sys::Promise), JsValue> {
    let wafer = /* as today */;
    let after = impresspress_core::after_response::AfterResponse::new();
    let (result, flush) = crate::flush_scope::run(
        impresspress_core::after_response::scope(Rc::clone(&after), async move {
            let (msg, input) = convert::request_to_message(&request).await?;
            let output = wafer.run("site-main", msg, input).await;
            convert::output_to_response(output).await
        }),
    )
    .await;
    // The generation is durable before the reply leaves; a flush failure is
    // the response's failure.
    flush.map_err(|e| JsValue::from_str(&e))?;
    let response = result?;
    let db = /* the runtime's DatabaseService handle, as the audit path needs it */;
    let audit = after.take_audit_row();
    let tasks = after.take_tasks();
    let work = wasm_bindgen_futures::future_to_promise(async move {
        // Its own scope: the audit row and every deferred task share one export.
        let ((), flush) = crate::flush_scope::run(async move {
            if let Some(row) = audit {
                if let Err(e) = impresspress_core::after_response::persist_audit_row(db.as_ref(), row).await {
                    tracing::warn!(error = %e, "request log row not written");
                }
            }
            futures::future::join_all(tasks).await;
        })
        .await;
        if let Err(e) = flush { tracing::warn!(error = %e, "after-response flush failed"); }
        Ok(JsValue::UNDEFINED)
    });
    Ok((response, work))
}
```

Read how Cloudflare's `run` (`impresspress-cloudflare/src/lib.rs:382–456`) releases the reservation (`release_reservation`) and mirror it. `handle_request` in `impresspress-web/src/lib.rs` builds a `js_sys::Object` with `response` and `after`. `initialize` calls `impresspress_core::deferred::set_mode(DeferMode::Queued)` so `defer`red tasks join the request's queue (today they are bare `spawn_local`s nothing keeps alive).

`sw.js.tmpl`: `event.respondWith(handleFetch(event))`; in `handleFetch(event)`: `const { response, after } = await handle_request(event.request); event.waitUntil(after); return response;` — `waitUntil` is legal while the `respondWith` promise is pending. Keep the error path (`selfDestruct`) as is.

`runtime_factory.rs`: add `.both(impresspress_core::config_vars::REQUEST_LOG_CONFIG_KEY, "errors")` beside `RUN_MIGRATIONS_KEY`, with a comment: a browser build is one user's local instance whose database is exported whole on every flush; logging every request would grow that export without bound. `config_vars.rs` doc: browser builds default to `errors`. `RELEASE.md`: entry "Browser builds: request log defaults to errors only".

- [ ] **Step 3: Run wasm tests + `cargo test -p impresspress-core pipeline` + build the bundle once (`examples/dev-sandbox/build.sh` needs the CLI: `cargo install --path crates/impresspress --locked --debug --root ./out`) and open it in Playwright headless to confirm a site write still activates and `GET /b/dev/api/status` works (the e2e `dev-workspace.spec.ts` "site write → published → preview shows it" test is the check: `TEST_PORT=8083 npx playwright test --config=tests/playwright.config.ts tests/e2e/dev-workspace.spec.ts` from `crates/impresspress-web` with `python3 -m http.server 8083 -d examples/dev-sandbox/dist` running)**

- [ ] **Step 4: Commit** — `perf(browser): write the request log after the reply and default it to errors`

### Task A3: `maintain` after the reply, and real listing sizes

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/activation.rs` (`activate_staged` ~763; `maintain` ~800)
- Modify: `crates/impresspress-core/src/blocks/dev/files.rs` (~431 `collect_if_unpublished`)
- Modify: `crates/impresspress-core/src/blocks/dev/gc.rs` (~319: stop resetting totals when listing sizes are unknown is no longer needed once sizes are real; keep `reset_blob_totals` but it now sees real numbers)
- Modify: `crates/impresspress-browser/src/storage.rs` (~461–510 `list`), `crates/impresspress-browser/js/bridge.js` (`storageList` ~414–435: return `sizes` alongside `keys`, from `entry.getFile()` `.size`, or from the sidecar if that is cheaper — measure once and say which)
- Modify: `crates/impresspress-core/src/test_support` (or wherever `TestContext` dispatches requests): a `drain_deferred()` that runs queued after-response tasks so tests can await GC deterministically
- Test: `tests/dev_gc.rs` (218, 275, 330, 612, 686), `tests/dev_activation.rs::only_the_last_twenty_generations_are_retained` (821): each gains a `ctx.drain_deferred().await` after the write it asserts on; a new test in `tests/dev_activation.rs`: the write's response arrives while the collector has not yet run (assert the prunable generation still exists right after the response, and is gone after `drain_deferred`). Browser wasm test: `list` reports the byte size that `put` wrote.

**Interfaces:**
- Consumes: `impresspress_core::deferred::defer` (Queued in the browser after A2; Spawn on native).
- Produces: `maintain` scheduled via `defer` with owned handles; `TestContext::drain_deferred()`.

- [ ] **Step 1: Find how existing `defer(...)` callers obtain owned handles** (`grep -rn 'deferred::defer\|defer(' crates/impresspress-core/src` — the LLM/vector blocks queue tasks with owned service handles). `maintain(ctx, shared)` takes `&dyn Context` and `&DevShared`; the deferred task needs owned equivalents: the `DevShared` is reachable as `Rc`/`Arc` from the block (check `mod.rs:460–530` for how the block holds it), and the context's database/storage handles can be captured as owned service handles (`ctx.database()`/`ctx.storage()` return `Arc<dyn …>`; `gc.rs` and `retention.rs` only need those two plus the block's `DevShared`). Refactor `maintain`, `retention::prune` and `gc::collect` to take the owned handles they use (a `MaintainHandles { db, storage }` struct built from `ctx` at the call site) rather than `&dyn Context`. Comments are contracts: update the `activation.rs` module doc ("Activation is not atomic. It journals, may rebuild the runtime…") for the new order.

- [ ] **Step 2: Write the failing tests (Step list above), run, fail**

- [ ] **Step 3: Implement**: `activate_staged` replaces `maintain(ctx, shared).await` with `deferred::defer(maintain(handles, shared_rc))` right after the commit; `files.rs:431` likewise; `bridge.js` `storageList` returns `{ keys, sizes, total }`; `storage.rs` maps `size: sizes[i]`; delete the `size: 0` comment and any GC code path that existed only because sizes were unknown.

- [ ] **Step 4: Run** `cargo test -p impresspress-core --features block-dev` (green at base), the browser wasm tests, fmt, clippy.

- [ ] **Step 5: Commit** — `perf(dev): run retention and GC after the reply; report object sizes in browser listings`

### Task A4: Measure and record

- [ ] Build the bundle (`cargo install … --root ./out`, `IMPRESSPRESS=./out/bin/impresspress examples/dev-sandbox/build.sh`), serve on 8083, run the review's measurement script (copy `scratchpad/lt-measure.spec.ts` into `crates/impresspress-web/tests/e2e/` as `dev-site-latency.spec.ts` if it is not there yet; it is a measurement, mark it `test.describe.configure` as its own file and DO NOT add it to CI's default list), and write the before/after table (15 writes: round trip, served, preview; status GET floor) into `.superpowers/sdd/<plan>/pr-a-measurements.md`. Delete `out/` and `examples/dev-sandbox/dist` afterwards.

# PR B — validation and cache (worktree `../dev-site-speed-b`, branch `feat/dev-site-speed-b`, from `origin/main`)

### Task B1: Validate only the diff for a site-only generation

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/activation.rs` (`missing_content` ~970–994 and its call at ~680; `activate_staged` has `previous: Option<&(GenerationRow, GenerationManifest)>`)
- Test: `tests/dev_activation.rs::a_site_write_reads_in_full_only_the_content_it_publishes` (166) — invert the stylesheet assertion (the unchanged stylesheet is NOT probed; the new blob IS); keep 549 and 583 (422s); add `a_rollback_probes_every_blob_it_republishes` (rollback passes `previous = None`-equivalent full check) and `a_site_write_whose_new_blob_is_missing_is_refused`.

- [ ] Signature: `async fn missing_content(ctx, manifest, previous_site: Option<&SiteManifest>) -> Result<Vec<String>, ActivationError>`; for each site sha, skip the probe when `previous_site` references the same sha (the previous generation was validated at its own activation and GC keeps every retained generation's blobs); artifacts unchanged. `activate_staged` passes `previous.map(|(_, m)| &m.site)` for `GenerationCause::SiteWrite | SiteDelete | BlockCompile | BlockRemove`, and `None` for `Rollback` and boot convergence (a rollback republishes an old generation whose blobs may have aged out). Comment says exactly this.
- [ ] Run `cargo test -p impresspress-core --features block-dev --test dev_activation`; commit — `perf(dev): validate only the blobs a site generation adds`

### Task B2: Bounded read cache in the browser storage service

**Files:**
- Modify: `crates/impresspress-browser/src/storage.rs` (`BrowserStorageService` ~282–536)
- Create: `crates/impresspress-browser/src/storage_cache.rs` (the LRU: `HashMap<(String, String), Entry>` + `VecDeque` order, byte budget 16 MiB, per-entry cap 1 MiB; no new dependency)
- Test: wasm tests in `storage_cache.rs` (budget eviction, over-size bypass, invalidate on delete/delete_folder/put) and in `storage.rs` (a `get` after `put` performs no OPFS read — count bridge calls with the memory OPFS from `database::test_support`; `get_streaming` served from cache yields identical bytes)

- [ ] `get`: cache hit returns a clone; miss reads OPFS and inserts if ≤ 1 MiB. `get_streaming`: hit → a stream over the cached bytes; miss → today's path (do not cache streams). `put` (≤ 1 MiB) inserts the written bytes + `ObjectInfo`; `put_streaming`, `delete` evict the key; `delete_folder` evicts the folder prefix. `list` untouched. Module doc: single writer, so the cache cannot go stale; what it buys (workspace.json, publishing blob reads, every site file the web block serves).
- [ ] Run the browser wasm tests; commit — `perf(browser): cache recently read storage objects in memory`

# PR C — page (worktree `../dev-site-speed-c`, branch `feat/dev-site-speed-c`, from `origin/main`)

### Task C1: Activation push and preview reload

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/control.rs` (`RuntimeControl` ~252: `fn announce_active(&self, generation: &GenerationAnnouncement)`), `activation.rs` (call it after the commit, before `maintain`), `test_support.rs` (`FakeControl` records announcements)
- Modify: `crates/impresspress-web/src/dev_runtime.rs` (`BrowserRuntimeControl` ~333: `clients.matchAll({type:'window'})` → `postMessage({ type: 'dev-generation', id, cause, changed_paths })`; add `web-sys` features `Clients`, `Client`, `ClientQueryOptions`, `ClientType` to `crates/impresspress-web/Cargo.toml`)
- Modify: `crates/impresspress-core/src/blocks/dev/assets/dev.js` (`refreshAfterChange` ~341–352 drops the awaited status GET and `reloadPreview()`; new `navigator.serviceWorker` message listener → `onGenerationActive(message)`: ignore ids already shown, CSS-only → swap `<link rel=stylesheet>` hrefs whose path is in `changed_paths` with `?g=<id>`, else `reloadPreview()`; log `live generation: <id>` there), `assets/test/harness.mjs` (stub `navigator.serviceWorker` as an `EventTarget`), tests in `assets/test/dev_status_poll.test.mjs`/new `dev_generation_push.test.mjs`; `dev_compile_block.test.mjs:660–706` (the "status reads > before" assertion goes: the catch-up GET no longer exists — assert the message path instead)
- Modify: `contracts.rs` `FileWriteResponse` (+ `progress: Vec<ProgressStep>`), `files.rs` `publish_if_site` returns the outcome's progress too; regenerate `tests/snapshots/*.json` (`UPDATE_OPENAPI_SNAPSHOTS=1 UPDATE_DEV_TOOLS_SNAPSHOT=1`) and `packages/impresspress-js` types (`npm run generate:types`)
- Test: core unit test that a site write announces `{cause: SiteWrite, changed_paths: [path]}` through `FakeControl`; page tests as listed

- [ ] TDD per file; `GenerationAnnouncement { id, cause, changed_paths: Vec<String> }` computed in `activate_staged` from the site diff (`publisher::publish_site` already knows the changed set — return it).
- [ ] Commit — `feat(dev): push activations to the page and hot-swap CSS`

### Task C2: The site-write timing line

- [ ] `crates/impresspress-web/tests/e2e/dev-workspace.spec.ts` (~193–220): time the write (`site_write_ms`), poll `fetch('/index.html', {cache:'no-store'})` every 20 ms until the new heading is served (`served_ms`), and the iframe heading (`preview_ms`); print `dev-workspace: site_write_ms=… served_ms=… preview_ms=…`; `.github/workflows/ci-shared.yml` ~1081–1089 greps `dev-workspace:` into the summary (check it already does; add if not). Run the spec locally against a bundle; commit — `test(dev): measure the site-write loop in the e2e`

# Coordinator

- [ ] After PR A's final review: rebuild + measure (A4), push, open PR with the before/after table.
- [ ] PRs B and C in parallel worktrees; each with its own final review; push; open PRs (C's e2e line is the acceptance for all three once they merge).
