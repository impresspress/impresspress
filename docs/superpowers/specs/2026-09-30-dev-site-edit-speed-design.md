# Dev sandbox site-edit speed

**Date:** 2026-09-30
**Status:** approved in conversation; follows `2026-09-29-dev-block-compile-speed-design.md` (block compiles) and amends `2026-09-02-dev-sandbox-design.md` §7 (activation) and §8.
**Goal:** a front-end edit in the sandbox (`dev_write_file` under `site/`) is visible in the
preview in well under 150 ms on a developer machine, and that number does not grow over a
session.

## 1. Where the time goes today

Measured 2026-09-30 on `feat/dev-compile-speed` (local bundle, 24-core box, medians of 15
edits to `site/index.html`): tool round trip 217 ms, new bytes served 129 ms, preview
updated 222 ms, against a 7.5 ms floor for one request through the service worker. The
full map is in the `site-edit-speed-review.md` report; the causes, largest first:

1. **A whole-database flush per mutation.** The browser `DatabaseService` wraps every
   mutating call in `with_flush`, and `dbFlush` runs sql.js `export()` (which
   serialises the whole database and closes/reopens the connection) then rewrites the
   whole OPFS file. A site-only activation makes ~9 mutations: generation insert,
   journal ×2, status ×4, runtime-state write, retention delete.
2. **A `request_logs` row written inline before every reply**, policy `All`, no
   pruning: one more flush per HTTP request, including every status poll and every
   `GET /`, and a database that only grows, so every later flush is slower.
3. **Retention and a full GC pass run before the reply**: two OPFS directory walks,
   ~20 manifest parses, and, because browser listings report every object as
   `size: 0`, a reset of the workspace byte count and another `workspace.json` save
   on almost every write. The 64 MiB quota is never enforced in the browser.
4. **A whole-site blob presence sweep on every activation**, one OPFS open per site
   file, although the handler just stored the only new blob.
5. **`workspace.json` read three times and rewritten once or twice per write**; every
   served site file is an OPFS read plus a JSON sidecar, with no cache.
6. **The page** awaits one more `GET /status` before reloading the preview, and the
   write response drops the activation's per-phase `progress`, so nothing shows where
   the time went.

## 2. The changes

### 2.1 One flush per request

The browser database keeps flushing after mutations, but the flush is *deferred* while a
request is being handled and runs once when the request's work is done. The service
worker's request path opens a deferred-flush scope around `handle_request`; inside it,
`with_flush` records that a flush is owed instead of exporting; when the scope closes
the database is exported and written once, before the reply is returned, so a reply
still means "durable". A request that mutated nothing flushes nothing. The scope is
explicit at its one call site and reentrant (an activation inside a write inside a
request is one scope). Expected: a site write drops from ~10 exports to 1.

The service-worker context has no synchronous OPFS access handles, so an incremental
SQLite VFS is not available there; one export per request is the ceiling, and with
request logging off the path (§2.2) the database stays small enough that the export is
a few milliseconds.

### 2.2 Request logging off the critical path, and bounded

The `request_logs` row is written *after* the reply in the browser, through the same
after-response queue Cloudflare already uses, drained by the service worker with
`event.waitUntil`. The sandbox and browser builds default the policy to a bounded one
(`IMPRESSPRESS_REQUEST_LOG` set in the sandbox's infrastructure config), so the table,
and therefore every flush, stays small. Expected: a status poll no longer flushes at
all (20 ms → the ~8 ms floor), `GET /` likewise.

### 2.3 Retention and GC after the reply

`maintain` (retention prune + blob GC) leaves the activation's critical path: it runs in
the same after-response queue, after the write's reply, still once per successful
activation. The browser storage listing reports real object sizes (each object file's own
`getFile().size`, not the sidecar), so GC stops resetting `blob_bytes`, stops re-saving
`workspace.json`, and the quota works.

### 2.4 Validate the diff, not the site

For a site-only generation, `missing_content` checks only the blobs the new manifest
adds relative to the previous active generation's manifest. The previous generation was
validated when it activated; a blob it referenced and that GC has not removed (GC keeps
every retained generation's blobs) is present by construction. A block change keeps the
full check for artifacts.

### 2.5 A bounded read cache in the browser storage service

The browser `StorageService` keeps a bounded in-memory cache of objects it has read or
written (bytes plus metadata), invalidated by its own `put`/`delete`. The service worker
is the only writer of OPFS, so the cache cannot go stale. It serves `workspace.json`
re-reads, blob reads during publishing, and every site-file request the web block makes
(`wafer-run/web/site/**`), so a preview reload's four fetches skip OPFS entirely.
Bounded (e.g. 16 MiB, LRU), so a large site degrades to today's behaviour, never worse.

### 2.6 The page learns of activation by push

When a generation becomes active, the dev runtime posts `{ type: "dev-generation",
id, cause }` to every client through `clients.matchAll()` + `postMessage`. The page
listens on `navigator.serviceWorker` and reloads the preview for a generation it has
not yet shown, so edits from any tab, or from an agent driving another tab, reload
every preview; the page-local reload after its own call goes away with the awaited
status read that delayed it. A CSS-only generation (every changed path ends in `.css`)
swaps the matching `<link>` elements' `href` with a cache-busting query instead of
reloading. The write response carries the activation's `progress` (the server already
computes it) so the ladder and the e2e can show phases.

### 2.7 Measured acceptance

`dev-workspace.spec.ts` prints `dev-workspace: site_write_ms=… served_ms=…
preview_ms=…` for a site write (tool round trip, time until `GET /index.html` serves
the new bytes, time until the preview iframe shows it), and CI's summary picks the line
up like the compile lines. Targets on this box: write ≤ 60 ms, preview ≤ 120 ms, and the
15th edit no slower than the first. The measurement script from the review is the
template.

## 3. What does not change

Every write still creates a generation (append-only history, rollback, retention of 20);
the activation queue and its coalescing; the limits; the seed/export format; a reply from
`dev_write_file` still means the generation is active and durable.

## 4. Delivery

Three PRs from `main`, independent of the compile-speed PRs (#101, #102, #103):

- **A — storage path**: §2.1, §2.2, §2.3 (browser database deferred flush, after-response
  queue in the service worker, request-log policy default, `maintain` after reply,
  listing sizes). `crates/impresspress-browser`, `crates/impresspress-web`,
  `crates/impresspress-core` (`pipeline.rs`, `blocks/dev/activation.rs`, `gc.rs`).
- **B — validation and cache**: §2.4, §2.5. `crates/impresspress-browser/src/storage.rs`,
  `blocks/dev/activation.rs`.
- **C — page**: §2.6, §2.7. `blocks/dev/assets/dev.js`, `page.rs`, `contracts.rs`
  (`FileWriteResponse.progress`), `files.rs`, `control.rs`/`dev_runtime.rs`, the e2e.

A first, then B and C in parallel.

## 5. Testing

- Browser wasm tests (`impresspress-browser`): a deferred scope with three mutations
  flushes once at close; a scope with no mutation flushes nothing; nested scopes flush
  once; the storage cache serves a `get` after `put` without touching OPFS and forgets
  on `delete`; listings report sizes.
- Core unit/integration tests: request log written through the after-response queue in
  the browser pipeline; `maintain` invoked after the reply (the activation response
  arrives before GC ran); diff-only `missing_content` (a blob referenced by the previous
  generation is not probed; a new blob that is missing still refuses).
- Page tests (`node --test`): reload on a `dev-generation` message for an unseen id,
  no reload for a seen id, CSS-only swap, no awaited status read on the write path.
- E2E: the site-write timing line, and the existing scenario/workspace specs green.
