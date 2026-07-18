# ImpressPress security, WASM, and Cloudflare findings

**Audit date:** 2026-07-16  
**ImpressPress revision:** `ed94a3f61d55e2d80939b8dae75749c59e4c95cb`  
**Scope:** security, correctness, code quality, refactoring, browser WASM size/runtime, and Cloudflare Workers efficiency

## Executive summary

ImpressPress has good foundations: database values are generally bound rather than interpolated, OAuth state and PKCE handling are strong, release builds already use LTO and size-oriented settings, feature registration is centralized, and the supported Rust test suite is broad. The most important problems are concentrated in authorization boundaries, error handling, platform adapter drift, and unnecessary Cloudflare I/O.

Three findings should be treated as release blockers:

1. **Messages has cross-user IDORs that chain into stored admin XSS.** Any authenticated user can access other users' contexts and entries. On LLM-enabled deployments, attacker-controlled assistant content can execute in an administrator's browser.
2. **The Cloudflare and browser database adapters omit newer `DatabaseService` operations.** In particular, `upsert` inherits a hard-error default. WASM rate limiting uses `upsert` and explicitly fails open when it errors, so Cloudflare and browser rate limits are currently ineffective.
3. **Disabled or soft-deleted OAuth-linked users can authenticate again.** The existing-provider-link branch bypasses the lifecycle check performed by the email-merge branch.

The highest-confidence binary-size improvement is also straightforward. A real linked Cloudflare consumer was **4,877,353 bytes raw / 1,737,534 bytes gzip-9**. Disabling adapter default features that the consumer says it does not use reduced it to **4,171,741 / 1,489,572 bytes**, saving **705,612 raw bytes (14.5%)** and **247,962 compressed bytes (14.3%)** without an intended behavior change.

The largest Cloudflare runtime costs are architectural rather than instruction-level: one KV read before nearly every warm cache hit, repeated D1 schema introspection that turns logical CRUD operations into two to four queries, and full buffering/copying of request, response, R2, and outbound-network bodies.

## Scope and validation

The audit covered the Rust workspace, browser JavaScript bridge, TypeScript packages, Cloudflare adapter, representative linked Cloudflare consumer, CI workflows, manifests, embedded assets, migration payloads, and the exact WAFER revision in `Cargo.lock`.

Validation performed:

- `cargo test --workspace --exclude impresspress-web --exclude impresspress-cloudflare --locked` passed.
- WASM checks passed for `impresspress-cloudflare`, `impresspress-browser`, `impresspress-web`, and `minimal-browser` on `wasm32-unknown-unknown`.
- `cargo audit --no-fetch` found no unignored vulnerabilities. Two yanked `spin` releases remain warnings.
- Offline production npm audits reported no vulnerabilities.
- Representative browser and Cloudflare release artifacts were built and inspected with `wasm-tools`, `wasm-opt`, and gzip.
- No live Cloudflare deployment, destructive exploit attempt, or production load test was performed. Runtime impact estimates that lack a measured deployment result are identified as recommendations rather than measured savings.

## Measured WASM baseline

| Artifact | Raw bytes | gzip-9 bytes | Notes |
|---|---:|---:|---|
| Browser ImpressPress WASM | 4,294,963 | 1,589,564 | 3,225,439-byte code section; 1,045,456-byte data section |
| Browser sql.js WASM | 761,041 | 371,332 | Loaded separately; must be included in browser startup/download accounting |
| Cloudflare `site`, adapter defaults enabled | 4,877,353 | 1,737,534 | Representative linked application, not the standalone adapter cdylib |
| Same consumer, adapter defaults disabled | 4,171,741 | 1,489,572 | 3,208,809-byte code section; 940,383-byte data section |
| Feature-default saving | **705,612** | **247,962** | **14.5% raw / 14.3% gzip-9** |

The standalone `impresspress-cloudflare` cdylib is not a valid application-size measurement because its public Rust `run` function is not retained as a fetch entrypoint until a consumer calls it.

The current representative bundle remains below Cloudflare's compressed Worker limit. Current platform constraints and guidance relevant to this audit are documented in [Workers limits](https://developers.cloudflare.com/workers/platform/limits/): a 1-second startup limit, 128 MB memory per isolate, and compressed upload limits that vary by plan.

## P0: release-blocking security and correctness findings

### 1. Cross-user Messages IDOR and stored admin XSS

**Severity:** Critical on LLM-enabled deployments; High otherwise

Messages endpoints require an authenticated identity but do not enforce context or entry ownership:

- The block declares authenticated endpoints without an ownership layer: [`messages/mod.rs`](../crates/impresspress-core/src/blocks/messages/mod.rs#L141).
- Context listing is global and context creation accepts caller-selected identity fields: [`messages/rest.rs`](../crates/impresspress-core/src/blocks/messages/rest.rs#L35).
- Get, update, delete, and entry operations are ID-scoped without participant filters: [`messages/rest.rs`](../crates/impresspress-core/src/blocks/messages/rest.rs#L86).
- Entry creation accepts arbitrary roles, including `assistant`: [`messages/rest.rs`](../crates/impresspress-core/src/blocks/messages/rest.rs#L149).
- The service layer preserves the same unscoped behavior: [`messages/service.rs`](../crates/impresspress-core/src/blocks/messages/service.rs#L34).

The admin LLM page loads all contexts and entries and embeds serialized records using `PreEscaped`: [`llm/pages.rs`](../crates/impresspress-core/src/blocks/llm/pages.rs#L52). JSON escaping does not escape `<`, so a value containing `</script>` can terminate an `application/json` script element. The client also passes assistant content through `marked.parse` and assigns it to `innerHTML` without sanitization: [`llm-chat.js`](../crates/impresspress-core/src/ui/assets/llm-chat.js#L411).

An authenticated attacker can therefore read or modify other users' conversations. On LLM-enabled deployments, the attacker can create assistant content containing raw HTML or a script-element escape; it executes same-origin when an administrator opens that conversation.

**Required remediation:**

- Add immutable owner/participant fields and include them in every context/entry repository query.
- Derive sender identity from `msg.user_id`; do not accept it from request data.
- Reserve `assistant` and `system` roles for trusted internal calls.
- Model administrative access as an explicit bypass, not as missing ownership filters.
- Escape `<` as `\u003c` in embedded JSON, following the existing files-page pattern.
- Sanitize rendered Markdown using a strict allowlist before DOM insertion.
- Add two-user isolation tests and payload tests for `</script>`, raw HTML, event handlers, and dangerous URLs.
- Add per-user request, byte, and stored-message quotas.

### 2. Database adapter drift makes WASM rate limiting fail open

**Severity:** High security and correctness regression

The exact WAFER revision locked by ImpressPress defines `update_where_count`, `upsert`, and `aggregate`. The default implementations of `upsert` and `aggregate` return a not-implemented error: [`DatabaseService`](../../wafer-run/crates/wafer-core/src/interfaces/database/service.rs#L415).

Those methods are not implemented by:

- Cloudflare D1: [`impresspress-cloudflare/database.rs`](../crates/impresspress-cloudflare/src/database.rs#L147)
- The Cloudflare KV database decorator actually registered at runtime: [`kv_cached_db.rs`](../crates/impresspress-cloudflare/src/kv_cached_db.rs#L167)
- The browser database adapter: [`impresspress-browser/database.rs`](../crates/impresspress-browser/src/database.rs#L111)

The WASM rate limiter uses `db::upsert`: [`rate_limit.rs`](../crates/impresspress-core/src/blocks/rate_limit.rs#L192). Its error policy explicitly returns `FailedOpen` when that upsert fails: [`decide_rate_limit`](../crates/impresspress-core/src/blocks/rate_limit.rs#L283). As a result, rate-limited WASM routes are allowed rather than counted or rejected.

Other affected paths include product subscription upserts, OAuth provider-link upserts, file aggregates, and admin dashboards. Some return errors; some swallow the failure and display empty data.

**Required remediation:**

- Forward or implement every current trait method in D1, the KV decorator, and browser adapters.
- Ensure mutating decorator methods perform the correct cache invalidation/version bump.
- Add a backend conformance suite that invokes every `DatabaseService` operation.
- Generate proxy delegation or move default `DbExec` delegation into the trait owner so a newly added method cannot silently inherit an unsupported default.
- Split schema mutation into an explicit backend capability rather than returning success from D1 no-op schema methods.

### 3. Disabled OAuth-linked users can authenticate again

**Severity:** High

The existing provider-link branch directly reuses `link.user_id`: [`oauth/callback.rs`](../crates/impresspress-core/src/blocks/auth_ui/oauth/callback.rs#L405). The active-user check exists only in the email-merge branch. Token issuance happens afterward without a common lifecycle check: [`oauth/callback.rs`](../crates/impresspress-core/src/blocks/auth_ui/oauth/callback.rs#L485).

Admin deletion is soft deletion and provider links remain. A disabled, deleted, or compromised linked account can complete OAuth again and receive fresh access and refresh tokens.

**Required remediation:** resolve the selected user after all identity-resolution branches, then perform one mandatory lifecycle/verification check before provider-link mutation or token issuance. Add disabled, deleted, and pre-linked regression tests.

## P1: additional security findings

### System-role protection fails open

[`admin/iam.rs`](../crates/impresspress-core/src/blocks/admin/iam.rs#L100) uses `if let Ok(existing)` before enforcing the rule that system roles cannot be renamed. A database read failure skips the protection and falls through to the update. Match success, not-found, and infrastructure failures explicitly; infrastructure errors must reject the mutation.

### Password change and logout can report success without revocation

Password change updates the credential and then discards refresh-token revocation failure with `.await.ok()`: [`change_password.rs`](../crates/impresspress-core/src/blocks/auth_ui/api/change_password.rs#L73). Logout similarly ignores refresh-token and JWT blocklist failures. Password update and refresh-family revocation should be transactional where supported, or return a durable partial-failure state.

### Bootstrap tokens are not atomically single-use

Redemption performs validate, admin creation, and best-effort deletion as separate operations: [`bootstrap.rs`](../crates/impresspress-core/src/blocks/auth_ui/api/bootstrap.rs#L54). Concurrent requests can both validate and create privileged users. Implement an atomic `take_valid_by_hash` using `DELETE ... RETURNING` and include token consumption plus admin creation in one transaction when the backend supports it.

### Access JWTs outlive account and role changes

Request authentication trusts signed lifecycle and role claims until token expiry. Disable, delete, password change, and role demotion do not invalidate all active access JWTs. Add a per-user `auth_version` or `revoked_before` claim backed by a short-lived cache, bump it on security-relevant mutations, revoke refresh families at the same time, and cap configurable access-token lifetime.

### Stripe webhooks lack event idempotency

Signature handling is strong: raw-body verification, timestamp window, HMAC, and constant-time comparison are present. However, the handler does not persist Stripe `event.id`, so legitimate retries or replay within the accepted window can repeat downstream effects: [`stripe.rs`](../crates/impresspress-core/src/blocks/products/stripe.rs#L277). Persist the event ID under a unique constraint before side effects, ideally with a transactional outbox.

### Sensitive configuration is copied into KV

The Cloudflare KV database decorator serializes complete configuration records for up to 24 hours: [`kv_cached_db.rs`](../crates/impresspress-cloudflare/src/kv_cached_db.rs#L260). It does not honor the table's `sensitive` metadata, so OAuth, Stripe, email, and similar credentials can be duplicated into a globally replicated, eventually consistent store. Do not KV-cache sensitive rows. Prefer Worker secrets or encryption with a Worker-bound key, and namespace cache keys by deployment/database identity.

### Route declarations fail open

Missing endpoint metadata resolves to `Public`: [`routing.rs`](../crates/impresspress-core/src/routing.rs#L243). Existing sensitive handlers often add local checks, but a newly added handler is exposed if its declaration is forgotten. Deny undeclared paths by default and require explicit opt-in for public webhooks or handler-owned authorization.

### Cookie mutations lack an explicit CSRF layer

Cookie authentication relies on `SameSite=Lax` without CSRF tokens, Origin/Referer checks, or Fetch Metadata validation on unsafe methods. Validate `Origin` and `Sec-Fetch-Site` for cookie-authenticated mutations and use CSRF tokens for SSR forms. Consider `SameSite=Strict` where OAuth/navigation requirements permit it.

### Outbound URL checks do not cover DNS and redirects

The URL validator rejects direct private and loopback IP literals but accepts hostnames without resolving them and does not revalidate redirect destinations: [`util.rs`](../crates/impresspress-core/src/util.rs#L253). This matters for credential-bearing Mailgun and billing calls. Resolve immediately before connection, reject private/link-local/loopback/multicast/metadata destinations, disable or validate redirects, and prefer provider allowlists.

### Cloudflare returns internal errors to clients

The normal request error branch returns `format!("impresspress: {e}")`: [`impresspress-cloudflare/lib.rs`](../crates/impresspress-cloudflare/src/lib.rs#L271). SQL, binding, schema, or configuration details can therefore reach clients. Return a generic 500 plus correlation ID and keep the cause in logs.

### Deploy token comparison is not constant-time

The deploy guard hashes both values but converts them to hex strings and uses ordinary `!=`: [`impresspress-cloudflare/lib.rs`](../crates/impresspress-cloudflare/src/lib.rs#L305). Compare the fixed-size digest bytes with the existing constant-time primitive.

### Supply-chain checks are advisory

- WAFER dependencies claim to be revision-pinned but specify `branch = "main"`: [`Cargo.toml`](../Cargo.toml#L33). `Cargo.lock` pins a commit today, but dependency updates follow a moving source.
- CI commands commonly omit `--locked`.
- `cargo audit` uses `continue-on-error: true`: [`ci.yml`](../.github/workflows/ci.yml#L404), [`ci-main.yml`](../.github/workflows/ci-main.yml#L163).
- CI executes a moving remote wasm-pack installer with `curl | sh`.
- Audit exceptions lack owners and expiry dates.

Pin WAFER with `rev`, use `--locked`, provision checksum-pinned tools, make new advisories blocking, and attach owner/expiry metadata to temporary exceptions.

## Cloudflare runtime efficiency

### 1. Remove the KV read from nearly every warm request

`runtime_cache::get_or_build` always awaits `current_version`, including when an isolate-local runtime exists: [`runtime_cache.rs`](../crates/impresspress-cloudflare/src/runtime_cache.rs#L50), [`get_or_build`](../crates/impresspress-cloudflare/src/runtime_cache.rs#L103).

Use a jittered isolate-local probe deadline, initially 30-60 seconds. A mutation handled by the same isolate can mark it dirty immediately; other isolates sample the version. Cloudflare KV is already eventually consistent and cached changes may take 60 seconds or longer to appear, so per-request probing does not provide strong freshness: [How KV works](https://developers.cloudflare.com/kv/concepts/how-kv-works/).

Expected result: one fewer async KV binding operation from almost every warm request. Security-sensitive grant and policy invalidation should use D1 or a strongly consistent coordinator rather than relying on KV freshness.

### 2. Eliminate normal-request D1 schema introspection

The Cloudflare adapter delegates logical operations to shared `DbExec`, which repeatedly checks table and column metadata:

| Logical operation | Current D1 statements | Source |
|---|---:|---|
| List | 3-4 | [`exec.rs`](../../wafer-run/crates/wafer-core/src/interfaces/database/exec.rs#L309) |
| Count | 3 | [`exec.rs`](../../wafer-run/crates/wafer-core/src/interfaces/database/exec.rs#L410) |
| Create | 2 | [`exec.rs`](../../wafer-run/crates/wafer-core/src/interfaces/database/exec.rs#L443) |
| Update | 3 | [`exec.rs`](../../wafer-run/crates/wafer-core/src/interfaces/database/exec.rs#L485) |
| Update where | 4 | [`exec.rs`](../../wafer-run/crates/wafer-core/src/interfaces/database/exec.rs#L570) |

Cache table/column metadata per isolate and invalidate only after DDL or deployment. Add a strict production mode where successful migrations are authoritative and lazy schema mutation is disabled. Combine count/list and other multi-statement operations with D1 batch calls; Cloudflare documents batching as a way to reduce round trips: [D1 batch API](https://developers.cloudflare.com/d1/worker-api/d1-database/).

The admin dashboard's first seven logical queries expand to approximately 19 D1 statements, and the chart aggregates bring the page to roughly 22. Consolidate aggregates by table and batch operations rather than adding more concurrency.

### 3. Redesign request, response, storage, and network traits for streaming

The Cloudflare request path fully buffers the body: [`convert.rs`](../crates/impresspress-cloudflare/src/convert.rs#L34). Responses are fully collected before constructing the Worker response: [`convert.rs`](../crates/impresspress-cloudflare/src/convert.rs#L81). R2 upload creates another `Vec`, while R2 download buffers the entire object: [`storage.rs`](../crates/impresspress-cloudflare/src/storage.rs#L49). Outbound fetch performs similar Rust/JS copies: [`network_service.rs`](../crates/impresspress-cloudflare/src/network_service.rs#L32).

A 10 MB upload can transiently exist as the cloned JS request, Rust body, second R2 vector, and binding-side body. Concurrent uploads can approach the 128 MB isolate limit. Add owned and streaming variants to `InputStream`, `OutputStream`, `StorageService`, and `NetworkService`, with direct R2 `ReadableStream` passthrough. Cloudflare recommends streaming specifically to avoid buffering within the isolate memory limit: [Workers Streams](https://developers.cloudflare.com/workers/runtime-apis/streams/).

The existing Content-Length precheck is useful, but missing/chunked lengths are still buffered before the final cap. Map oversize-body errors to 413 instead of the generic 500 path.

### 4. Fix KV version-write throttling and invalidation

Every configuration-table mutation writes the same global version key: [`kv_cached_db.rs`](../crates/impresspress-cloudflare/src/kv_cached_db.rs#L141). The retry immediately repeats the same write. Cloudflare KV limits a key to one write per second, so an immediate retry after throttling is likely to fail again: [KV limits](https://developers.cloudflare.com/kv/platform/limits/).

Coalesce bumps to at most one per second and perform delayed retries through `waitUntil`, or move version coordination to a stronger store. The deploy funnel already suppresses individual bumps and emits one final bump, which should be preserved.

`update` invalidates keys derived from the old record but not the returned updated record: [`kv_cached_db.rs`](../crates/impresspress-cloudflare/src/kv_cached_db.rs#L355). If an identity field changes, a cached entry under the new identity can remain stale for 24 hours. Invalidate the union of old and new keys or namespace row-cache entries by configuration generation.

### 5. Batch audit-log persistence

Request-log rows are correctly moved off the response path with `waitUntil`, but they are inserted sequentially and each `create` currently performs column introspection: [`impresspress-cloudflare/lib.rs`](../crates/impresspress-cloudflare/src/lib.rs#L256). Add a batch/multi-row insert path and emit metrics or logs when background persistence fails.

### 6. Cache crypto construction and trim hot-path allocations

The Cloudflare crypto service clones the JWT secret and reconstructs/revalidates the engine for each sign/verify call: [`crypto_service.rs`](../crates/impresspress-cloudflare/src/crypto_service.rs#L34). Construct the validated engine once, or cache its construction error and fail consistently.

New token families also make two sequential crypto calls for 16 random bytes each. Request 32 bytes once and split it. The constrained Argon2 policy and dummy verification protect authentication behavior, so benchmark them on deployed Workers before changing security parameters.

### 7. Fix R2 pagination semantics

The storage adapter ignores `ListOptions.offset`, returns only the first R2 page, and reports that page's length as `total_count`: [`storage.rs`](../crates/impresspress-cloudflare/src/storage.rs#L109). Pages after the first can repeat data and totals are incorrect.

R2 uses an opaque cursor plus `truncated`, and may return fewer objects than requested: [R2 Workers API](https://developers.cloudflare.com/r2/api/workers/workers-api-reference/). Extend the storage wire type with cursor/next-cursor semantics. Avoid emulating offset by walking every preceding page.

## Binary-size and startup opportunities

### 1. Make lean features the safe default

Cloudflare's default feature set enables Files, Legal Pages, Messages, Products, and User Portal: [`impresspress-cloudflare/Cargo.toml`](../crates/impresspress-cloudflare/Cargo.toml#L15). The representative consumer states that it enables zero optional ImpressPress blocks but omitted `default-features = false`, causing the measured 14.5% bloat.

Recommended model:

- `default = []` or a deliberately minimal preset.
- An explicit `full` preset for convenience.
- Generated consumers declare only configured blocks.
- CI builds minimal and full feature matrices and enforces raw plus gzip size budgets.
- CI uses `cargo tree -e features` or equivalent checks to catch accidental feature unification.

### 2. Gate or externalize embedded assets

Embedded UI assets total **347,404 raw source bytes**. CSS, fonts, logos, htmx, marked, LLM chat JavaScript, and files-browser JavaScript are embedded unconditionally: [`ui/assets.rs`](../crates/impresspress-core/src/ui/assets.rs#L10). The system handler also allocates a new `Vec` for immutable content on every asset response: [`system.rs`](../crates/impresspress-core/src/blocks/system.rs#L45).

If strict single-WASM delivery is required, feature-gate LLM, Files, and Markdown assets, generate hashes/bundles at build time, and serve immutable bytes without per-request copies. If Cloudflare efficiency matters more than single-file packaging, move these files to Workers Static Assets or content-addressed R2 objects.

Cloudflare currently cannot enable `block-llm`, yet `llm-chat.js` is still linked. This is an immediate feature-gating candidate.

### 3. Gate backend-specific migration strings

Enabled migration modules embed **65,743 raw bytes** of SQL. Of that, **30,489 bytes is PostgreSQL SQL unusable by D1**. Generate backend-specific migration registries or gate SQL by backend capability so Cloudflare includes only SQLite/D1 statements.

### 4. Split SQL and host dependencies by target

`impresspress-core` and WAFER SQL utilities enable broad SeaQuery defaults, resulting in MySQL, PostgreSQL, SQLite, and derive features in the WASM graph: [`impresspress-core/Cargo.toml`](../crates/impresspress-core/Cargo.toml#L126). Add explicit backend feature sets and disable dependency defaults where possible.

The WAFER graph also includes host-oriented `dirs` and TOML support. Target/feature-gate lockfile and host configuration loading. Audit the always-on `url` dependency and its ICU4X data, full `futures` pulling `futures-executor`, and other direct dependencies with before/after linked builds. LTO may already strip some code, so accept removals only after measuring the final linked artifact.

### 5. Configure Cloudflare optimization deliberately

The workspace release profile is already strong for size: `opt-level = "z"`, LTO, one codegen unit, symbol stripping, and aborting panics: [`Cargo.toml`](../Cargo.toml#L79).

Browser release builds explicitly run `wasm-opt -Oz`, but `worker-build` 0.7 defaults to `-O`. Re-running the trimmed linked artifact with `-Oz --all-features` reduced raw size from 4,171,741 to 3,955,950 bytes, a 215,791-byte or 5.17% reduction, but reduced gzip-9 by only 138 bytes. This is potentially useful for startup compilation, not network transfer. Benchmark authentication, rendering, and database-heavy routes because `-Oz` can trade runtime speed for code size.

Local `just` builds enable `simd128`, while CI's wasm-pack command does not: [`justfile`](../justfile#L7), [`ci.yml`](../.github/workflows/ci.yml#L54). Standardize the artifact configuration. Benchmark SIMD specifically for Argon2 and Blake2, and consider package-level speed optimization for crypto rather than changing the whole application from size optimization.

## Browser WASM runtime opportunities

### Database bridge serialization and durability

Every browser database query serializes parameters to a JSON string, JavaScript parses them, SELECT results are JSON-stringified, and Rust parses them again: [`bridge.js`](../crates/impresspress-browser/js/bridge.js#L77), [`browser/database.rs`](../crates/impresspress-browser/src/database.rs#L47).

Every mutating SQL statement then exports the entire sql.js database and writes it to OPFS: [`bridge.js`](../crates/impresspress-browser/js/bridge.js#L94). A logical operation or migration containing several statements can therefore export the full database several times.

Return structured `JsValue` data and use `serde_wasm_bindgen` rather than JSON strings. Coalesce persistence at transaction or logical-request boundaries, with an explicit durability contract and one flush after the operation. Add a scheduled/debounced flush only if the acceptable crash-loss window is documented.

### Binary storage bridge

Browser storage converts `Uint8Array` to an `Array<number>`, JSON-stringifies it, and parses it into Rust bytes: [`bridge.js`](../crates/impresspress-browser/js/bridge.js#L151). This multiplies memory and transfer volume. Return a JS object containing `Uint8Array` plus metadata and deserialize with `serde_wasm_bindgen`.

Browser listing also scans and sorts every matching entry before slicing and reports page length rather than a real total. Return `{ keys, total }` at minimum and adopt cursor semantics for large stores.

## Code smells and refactoring opportunities

### Published TypeScript SDK has drifted from the server

The SDK calls database routes such as `/api/collections/*`, `/api/database/query`, and `/api/database/transaction` that do not have corresponding in-repository server routes: [`database.service.ts`](../packages/impresspress-js/src/services/database.service.ts#L35). Storage routes similarly diverge from the actual dispatcher. OAuth uses different paths/methods from the server, and reset/verification paths are also stale.

`ExtensionsService` exposes only `list` and `call`, while its tests call five nonexistent methods. CI builds/lints the SDK but does not run its tests. The published package also depends on `wafer-client-js` through a local `file:` path, so it cannot be installed cleanly outside this checkout.

Generate the SDK from `/openapi.json` or maintain shared route-contract fixtures. Delete unsupported APIs, publish or bundle `wafer-client-js`, run SDK tests, and add a clean-directory `npm pack && npm install` smoke test.

### Two divergent `impresspress-web` npm packages

`packages/impresspress-web` and `crates/impresspress-web/packages/impresspress-web` publish the same package name at different versions and expose different APIs/initialization behavior. Keep one canonical package and generate WASM output into it. Add one in-flight initialization promise to prevent concurrent WASM initialization and use exact route-boundary matching instead of `startsWith` checks such as `/health` matching `/healthfoo`.

### Admin and feature mutations swallow failures

WRAP grant creation/deletion discards database results. Block enable/disable can discard persistence errors and still write a success audit event. Feature settings treat every database read error as default settings with all blocks enabled: [`features.rs`](../crates/impresspress-core/src/features.rs#L326). Native and browser runtimes can also retain stale runtime settings after a successful database mutation.

Introduce mutation services that:

1. validate the request;
2. persist the mutation;
3. update or invalidate runtime state;
4. write the audit event only after success;
5. expose an explicit partial-failure policy if atomicity is unavailable.

Distinguish missing-table/pre-migration conditions from outages, corruption, and authorization errors. Preserve last-known-good state or fail closed on operational failures.

### Browser schema changes suppress all ALTER failures

The browser adapter ignores every `run_execute` error after deciding a column is missing: [`browser/database.rs`](../crates/impresspress-browser/src/database.rs#L223). Quota failures, malformed DDL, OPFS errors, and flush errors can appear successful. Treat only a verified duplicate-column race as benign, propagate everything else, and re-check the schema afterward.

### OAuth popup code is duplicated

The SDK contains two popup implementations with inconsistent timer/interval cleanup. One validates origin but not `event.source === popup`. Replace them with a single `PopupAuthSession` abstraction with an idempotent finalizer, source-window validation, timeout cleanup, and `AbortSignal` support.

### Large modules have natural responsibility seams

These should be split by domain responsibility rather than arbitrary line count:

- `blocks/llm/routes.rs`: chat/SSE, provider management, model management, and tests.
- `blocks/files/pages_user.rs`: buckets, objects, sharing, quota, and page composition.
- `blocks/files/storage.rs`: buckets, objects, search, admin statistics, and tests.
- `blocks/products/handlers.rs`: parsing, admin CRUD, catalog, ownership, subscriptions, and statistics.
- `builder.rs`: service registration, feature composition, lifecycle, and backend factories.

Smaller domain modules would also make feature compilation narrower and failure-policy tests easier to target.

### Error semantics fabricate successful defaults

Examples include malformed refund JSON becoming a default reason, repository failures becoming zero refund counts, SDK methods mapping arbitrary failures to `null`, and quota/statistics methods fabricating zero-valued responses. Only explicit not-found or unauthorized statuses should map to absence. Introduce a typed `ImpresspressError` in the SDK and retain structured server error categories to the response boundary.

## Observability and build-process findings

- The Cloudflare profile checker documents a stale startup threshold and confuses startup and request-CPU error codes. Replace raw-size heuristics with `wrangler deploy --dry-run`, compressed bundle reporting, and `wrangler check startup`.
- Build paths run `cargo install worker-build` repeatedly. Pin/provision it once and version-check rather than reinstalling every build.
- The Cloudflare logger allocates strings/vectors for all levels. Add a level filter and allocation-light formatting.
- Generated Wrangler configuration enables 100% head sampling. Make production sampling explicit and configurable.
- Add Server-Timing/metrics for runtime-cache hits, version probes, D1 primitive statements per logical call, D1 rows read/written, runtime builds, body sizes, and background log failures.

## Strengths to preserve

- `feature_block_manifest` is a useful central source for feature registration.
- OAuth state and PKCE verifier data are random, expiring, server-side, and atomically consumed.
- JWT verification checks expiry, token type, issuer, derived keys, and blocklist state.
- Password and refresh flows already enforce user lifecycle state.
- Stripe signature verification uses the raw body and constant-time comparison.
- D1 operations use prepared statements and bindings rather than caller-interpolated SQL.
- Storage is block-namespaced and WRAP-granted, with ownership/traversal regression tests.
- Legal-page Markdown strips raw HTML and unsafe URLs.
- Security headers, CORS, and readonly middleware are registered centrally.
- The release profile already has the right starting settings for small WASM.
- Cloudflare runtime caching and deploy-time version coalescing are good ideas; the warm-hit probe policy is the expensive part.

## Recommended implementation order

### Phase 0: security and platform correctness

1. Enforce Messages ownership and sanitize all LLM/admin rendering.
2. Implement/delegate every database operation in Cloudflare, KV-decorator, and browser adapters; add conformance tests.
3. Add the common OAuth lifecycle check for linked users.
4. Make IAM protection and password/session revocation fail closed.
5. Return opaque Cloudflare 500 responses and use constant-time deploy-token comparison.
6. Fix R2 cursor pagination and D1 integer handling beyond JavaScript's exact integer range.

### Phase 1: measured low-risk wins

1. Make Cloudflare default features minimal/empty and generate explicit features.
2. Add minimal/full raw and gzip WASM budgets to CI.
3. Add a 30-60-second jittered runtime-version probe interval.
4. Cache D1 schema metadata per isolate and batch multi-statement operations.
5. Batch audit-log persistence and coalesce KV version writes.

### Phase 2: boundary redesign

1. Add streaming request, response, storage, and network service APIs.
2. Gate or externalize static assets and backend-specific migrations.
3. Replace browser JSON/number-array bridges with structured `JsValue` and typed arrays.
4. Define transaction-level OPFS flush semantics.
5. Generate the TypeScript SDK from the server contract and consolidate the web npm package.

### Phase 3: broader hardening and cleanup

1. Add atomic bootstrap consumption, JWT auth-version invalidation, and Stripe event idempotency.
2. Add explicit CSRF and DNS/redirect-aware SSRF defenses.
3. Split backend/dependency features and benchmark `-Oz`, `simd128`, and package-level crypto optimization.
4. Split large modules along domain seams and centralize mutation/error policies.
5. Pin dependency revisions and make supply-chain checks blocking.

## Suggested success metrics

- Zero cross-user Messages access in two-user integration tests.
- All `DatabaseService` operations pass the same backend conformance suite.
- Rate limiting produces persisted counters and 429 responses on Cloudflare/browser.
- Default linked Cloudflare consumer at or below the measured trimmed baseline: 4,171,741 raw / 1,489,572 gzip-9 bytes.
- Warm requests perform no version-KV read until the local probe deadline.
- Common list/count/create/update paths perform one logical D1 call or one batch rather than two to four sequential calls.
- Large request, response, R2, and network bodies remain streaming and bounded below the isolate memory limit.
- SDK routes are generated or contract-tested against the server and install successfully from a packed tarball in a clean directory.
- Security-sensitive mutations never report or audit success after persistence/revocation failure.
