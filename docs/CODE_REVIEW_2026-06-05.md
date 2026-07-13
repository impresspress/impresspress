# Impresspress Code & Architecture Review

## Executive summary

The codebase is broadly healthy and convention-compliant on the load-bearing rules: no raw-SQL-in-blocks outside the documented exceptions, no sync bridges, typed `db::*` data access throughout, and the post-#249 typed-error direction mostly held. The dominant problems are **(1) security gaps on the most exposed surfaces** — an entirely unauthenticated A2A JSON-RPC endpoint and a stored-XSS in the admin network page being the two that demand immediate attention — and **(2) pervasive duplication of canonical mappings and metadata** (config-var lists, table-name encoding, timestamp helpers, boolean decoders, error formatting) that the project's "single source of truth / no magic mapping" rules exist to prevent. A third theme is **silent error-swallowing on write paths** (settings saves, WRAP-grant mutations, email dispatch) that report success while dropping failures. The single highest-leverage fix is **closing the A2A auth gap** (`messages/a2a.rs`); after that, the **JWT-secret-defaults-to-empty** fail-open at build time is the most dangerous latent issue.

The two LLM bugs (`key_var` never resolved into `api_key`; streaming chat never persisting assistant turns) are high-severity correctness breaks that make documented features silently not work, and are cheap to fix.

## Top architecture smells

### 1. Duplicated canonical mappings — the recurring "no magic / single source of truth" violation
This is the single most repeated structural problem. The same logical value or transform is independently re-derived in multiple files, already drifting in several cases:

- **Config-var metadata re-declared in feature blocks** instead of driven from `config_vars.rs`/`ConfigVar`: `userportal::portal_settings_keys()` (`userportal/mod.rs:649-700`, FAVICON_URL default already wrong), `auth_ui SETTINGS_KEYS` (`auth_ui/pages/settings.rs:18-108`), and `llm` constants re-declared in `pages.rs:28-30` shadowing `mod.rs:44-46`. `userportal` even ships a *second admin UI* for the shared branding `_URL` vars that bypasses the SSRF/URL validation the admin block enforces (`userportal/mod.rs:702-783`).
- **Block-name ↔ identifier encoding** computed inline three+ ways (`permissions.rs:236-247` reimplements the existing `wafer_block::wrap::resource_prefix`; `blocks.rs` URL form; `database.rs::group_label`; `impresspress-cloudflare/config_source.rs:69-78` `screaming_block`).
- **Timestamp formatting**: identical `now_iso()` copy-pasted across 10 auth repo modules (`auth/repo/users.rs:35-39` et al), with `tokens.rs` diverging to `now_rfc3339()` — two ISO spellings coexist in one block.
- **Boolean JSON decode** hand-rolled in 5 sites (`auth/repo/users.rs:45-50`, `orgs.rs`, `local_credentials.rs`, `llm/schema.rs`, `llm/migrations`) with three different semantics for `"TRUE"`/missing, ignoring the canonical `RecordExt::bool_field`.
- **Error `[code]` decoration**: `errors::error_response` (`blocks/errors.rs:188-196`) bakes the machine code into the human message *and* sets a structured wafer code; `rate_limit.rs:260-275` re-implements the same formatting.

**Fix direction:** for each, promote one canonical helper and call it everywhere. Highest value: drive all settings pages off `config_vars.rs`/`ConfigVar`, and centralize block-name encoding + the boolean/timestamp helpers in `blocks/helpers.rs`.

### 2. Missing repo/ data layers and scattered query construction
Only `auth` and `products` have a `repo/` module; `files`, `legalpages`, `llm`, `messages`, `vector` scatter `db::*` calls and identical filter-building across `mod.rs` + page/handler files. `files` is the worst (no repo, 1659-line `pages_user.rs`, ownership filter reimplemented in `storage.rs`/`pages_user.rs`, `load_quota_info` re-sums via `list_all`). `legalpages` rebuilds the same `doc_type/status='published'` filter in three places (`pages.rs:65-119, 894-921`; `mod.rs:283-298`) and uses the non-conventional `COLLECTION` const name (`legalpages/mod.rs:37`). This is a maintainability/testability concern, not a correctness one — no raw SQL is involved.

**Fix direction:** introduce `repo/` modules owning `pub const TABLE` + typed row access per the `auth/repo` pattern, collapsing the duplicated filters.

### 3. Shared abstractions exist but adoption stalled
The codebase repeatedly built the right abstraction and then didn't use it: `blocks/crud.rs` (used only by products; ~47 inline reimplementations elsewhere), `ui::components::tab_navigation` (the dead helper while 8 handlers hand-roll tab bars — `users.rs:36-58` et al), and per-block `handle()` dispatch reimplemented 22× with inconsistent catch-alls. Each is a duplication-only smell.

**Fix direction:** migrate consumers onto the existing helpers (and add direct tests for `crud.rs`, which currently has none), extending them rather than open-coding.

### 4. Two parallel mechanisms for one concept
Several blocks maintain two divergent implementations of the same thing: legalpages has **two CRUD surfaces** (JSON API vs SSR) with different version/publish-ordering semantics — the safer publish-then-archive ordering exists in only one copy (`legalpages/mod.rs:201-333` vs `pages.rs:651-723`); vector has **two index-enumeration paths** (sqlite_master scan vs registry) that can disagree (`vector/pages.rs:255-274` vs `service.rs:163`); admin mutations split between **SSR (audited) and JSON-API (unaudited)** surfaces.

## Convention violations

**Real violations:**

| Issue | Location | Note |
|---|---|---|
| Hand-written `json_extract` raw SQL in a request handler | `vector/pages.rs:697-715` | Not an allowed exception; reaches into backend-owned `_meta` table. Fix: add metadata-filtered delete op or a sql-utils builder. |
| Hand-written `sqlite_master` existence probe | `vector/pages.rs:598-609` | Comment admits the missing builder; per CLAUDE.md, add `introspect::build_table_exists` to wafer-sql-utils. |
| Hardcoded `"Impresspress"` brand in email from-address | `email.rs:344-351` | Config var `WAFER_RUN_SHARED__APP_NAME` already exists; thread it in. |
| Config-var metadata hardcoded in blocks | `userportal/mod.rs:649-700`, `auth_ui/pages/settings.rs:18-108` | "No hardcoded lists" — drive from `ConfigVar` declarations. |
| Magic byte-offset + legacy meta prefixes | `impresspress-browser/convert.rs:170-198` | `&k[17..]` magic number; sibling cloudflare uses `strip_prefix`. Likely dead legacy branches. |
| `COLLECTION` const naming | `legalpages/mod.rs:37` | Only block not using `pub const TABLE`. |
| `BLOCK_SETTINGS_TABLE` re-declared to dodge a (non-real) circular dep | `migration_helper.rs:31-33` | Move const to `features.rs`; cross-module refs are legal in Rust. |
| DB-write failure returns 400 + leaks raw error | `legalpages/pages.rs:694-718` | Should be `err_internal` (500, sanitized) per #249; sibling `handle_save` does it right. |
| WaferError → String flattening forcing wrong HTTP status | `messages/service.rs:33-105` | NotFound/PermissionDenied become 500; return `WaferError` and map at the boundary. |
| Self-flagged legacy DROP/CREATE compat shim | `impresspress-web/config.rs:152-199` | CLAUDE.md forbids compat shims; deprecation window can close (OPFS-local, wipeable). |
| Legacy `resource_type` missing-column compat shim | `impresspress/cli/server_config.rs:97-129` | Canonical migration always provides the column; delete the fallback. |

**Allowed exceptions (correctly used — not violations):** admin DB explorer raw SQL (`admin/pages/database.rs`), test-fixture `exec_raw` (`auth_ui/pages/orgs.rs:84-98`, provider_links tests), boot-time rusqlite reads before the data layer exists, the vector block's other raw introspection going through `introspect::*` builders, and the cloudflare/browser backend `DatabaseService` impls (raw SQL is expected at the backend layer).

**Notable false-positive pattern:** several "stringly-typed error / #249 regression" findings were refuted — the auth repo's `RepoError::Db(String)` and `crypto.rs`'s `Result<_,String>` are defensible domain-boundary collapses where no caller recovers the code, and #249 was a narrow `builder.rs`-only change, not a codebase-wide ban. Treat new "stringly-typed" flags with skepticism.

## Code smells & robustness

**Silent error-swallowing on write paths (theme, ~6 sites):** settings saves discard every `config::set` Result and unconditionally report success — `auth_ui/pages/settings.rs:215-228`, `userportal/mod.rs:777-782`, `admin/pages/email.rs:124-129`, plus products/legalpages (which at least `warn!`). WRAP-grant create/delete swallow DB errors with `let _ =` on a *security-policy* surface (`admin/mod.rs:401,412`). `email.rs::send_email` returns `bool` with zero logging despite a "log but don't fail" comment, blinding operators to verification/reset delivery failures. **Fix:** at minimum `tracing::warn!`; for settings, surface failures to the toast.

**Audit-logging gap (medium):** admin JSON-API mutation handlers (roles, user-roles, users, settings) skip the `audit_log` the SSR handlers perform (`admin/iam.rs:138-157,235-315`; `settings.rs`; `users.rs`). The audit trail is surface-dependent. **Fix:** push `audit_log` into leaf handlers or the dispatcher.

**TOCTOU / race patterns (low, mostly benign):** role assignment without a unique constraint (`admin/iam.rs:247-283`), `provider_links::upsert` and `orgs::upsert_claimed` read-then-create, legalpages version `MAX+1` read-modify-write. All admin-gated or constraint-backstopped; the proper fixes are DB unique constraints / composite-key upsert via `build_upsert` (which already supports multi-column conflicts).

**Cascade/atomicity bugs (low–medium):** `messages::delete_context` deletes the parent even when child-entry cascade fails, returning Ok (`service.rs:124-138`); products line-item rollback orphans already-inserted items (`purchase.rs:199-253`); products checkout claim isn't reverted on Stripe JSON-parse failure, permanently parking the purchase in `checkout_started` (`stripe.rs:191-194`). **Fix:** propagate the error / wrap post-claim work to revert on any Err.

**Off-schema writes & dual columns:** `stripe.rs:205-208` writes `provider_session_id` (in neither schema nor migration) — on SQLite/D1 the whole UPDATE fails, silently dropping the `provider='stripe'` write too. Products has `price` vs `base_price` (display reads one, pricing reads the other — `pages.rs:147,427` vs `pricing.rs:46`) and dead `amount_cents`/`stripe_payment_intent_id` columns. **Fix:** collapse to single columns; add a migration or stop writing the off-schema column.

**Wall-clock duration subtraction (low):** `storage.rs:312-336,373,391` does `(now_millis() - start) as i64` on wall-clock time — wraps/panics on clock step-back. The codebase already has the correct `saturating_sub`+`try_from` idiom in `pipeline.rs:189-194`; apply it (don't use `Instant` — it panics on wasm32).

**Request-body limit checked after buffering (medium):** `impresspress-cloudflare/convert.rs:35-40` reads the full body into memory *then* checks the cap; bounded by CF's edge limit but still a per-request OOM vector. Check `content-length` first or stream.

**Dead code / stale comments:** `ErrorCode::status_code()` (`errors.rs:80-111`, zero callers, already drifts from the real mapping), `impresspress-cloudflare/helpers.rs::json_response` + unused `maud` dep, 5 unused icon fns (`icons.rs`), `wafer_info::handle_flows` hardcoded list (`wafer_info.rs:40-58`, the live `FlowIntrospection` API already exists), and several stale doc comments (vector "Unimplemented" claims, provider_links "raw SQL", legalpages "Quill editor", `is_expired` mixed-format claim).

**God-files / large functions (low):** `builder.rs::build` (~320-line 12-phase method), `files/pages_user.rs` (1659 lines), `email.rs::handle_send_template` (inline HTML templates), `cloudflare/runner.rs::load_block_settings` (~150 lines).

## Per-area notes

**auth / auth-repo:** Mostly clean and convention-compliant. Real bug: `must_reset` written as JSON int `1/0` while every sibling bool is a JSON boolean (`local_credentials.rs:64`) — breaks under Postgres typed BOOLEAN, masked on SQLite/D1. Otherwise duplication-heavy (10× `now_iso`, 5× bool decode, `orgs.rs` parallel error enum). Bootstrap `UserId("bootstrap")` sentinel and `auth_grants()` literal table names are latent smells, not live bugs.

**admin:** Highest-severity item is the **stored XSS** in `pages/network.rs:169-172` (unauthenticated attacker plants request-log path/method → executes in admin session via unescaped `onclick`). Also: JSON-API audit-logging gap, WRAP-grant error-swallowing, `validate_url_value` localhost-prefix HTTPS bypass (`settings.rs:159-187`), N+1 table-count queries (`database.rs:59-86`).

**products:** Several medium correctness bugs concentrated here — checkout-claim leak, off-schema `provider_session_id`, `price`/`base_price` split, formula evaluator silently ignoring trailing tokens (`pricing.rs:143-148`, money path). Hard-delete despite a soft-delete schema (`handlers.rs:312-321`).

**llm:** Two **high-severity** correctness breaks: `key_var` is never resolved into `api_key` so every secret-referencing OpenAI/Anthropic provider fails `Unauthorized` (`routes.rs:412-436`), and streaming chat never persists assistant turns, corrupting conversation history (`routes.rs:295-353`). The model-picker sends composite `"backend:model"` to the backend unsplit (`pages.rs:582-618`). External CDN `marked@14` is the only non-self-hosted, un-SRI'd script in SSR (`pages.rs:90-92`).

**files:** No repo layer; download serves user content-type inline (residual XSS mitigated by the global nosniff+CSP from `security-headers`, so low); `i64_field` doesn't coerce TEXT integers so admin/quota render 0 (`pages_admin.rs:448,553-556`).

**vector:** Two raw-SQL convention violations + the `_meta` count reaching around the backend (`service.rs:137-150`). Global unscoped index namespace — any authenticated user can read/delete any index (`pages.rs`), though this mirrors the `messages` block and impresspress has no tenancy model, so it's a design observation.

**userportal:** Duplicate branding config metadata + a second admin settings UI bypassing URL validation (`mod.rs:649-783`).

**messages:** **High:** `/a2a` is fully unauthenticated despite comments claiming otherwise (`a2a.rs:39-69`) — any caller can enumerate/read/cancel/create tasks. Also WaferError→String status-code flattening and the cascade-delete swallow.

**legalpages:** No repo layer, `COLLECTION` naming, two CRUD surfaces with divergent publish ordering, a 400-with-leaked-error publish path, and a 200-OK-with-error-key handler (`pages.rs:642-647`) — the exact anti-pattern the adjacent `handle_save` was fixed away from.

**ui:** Active wrong-icon render bug — "Security" nav uses `"lock"` which has no `nav_icon` arm and falls through to the package box (`nav_groups.rs:60`, `sidebar.rs:8-30`); root cause is stringly-typed icon dispatch with a silent fallback. Form components hand-roll HTML escaping instead of using maud (`components.rs:306-405`).

**core-runtime:** **High:** JWT secret defaults to empty string when unset (`builder.rs:313-315`) — native CLI guards it, but the Cloudflare embedder does not, so production can boot fail-open with a forgeable HMAC key. Extra (downstream) routes bypass the feature-flag gate (`routing.rs:264-297`) so the admin disable toggle silently no-ops for project blocks. Cross-repo HKDF JWT-key derivation is hand-duplicated against wafer-block-crypto with a comment-only "must match" coupling that already broke prod once (`crypto.rs:153-165`).

**cloudflare/browser/web/native crates:** `exec_raw` always returns 0 rows-affected contradicting its contract (`cloudflare/database.rs:460-474`, latent — no consumer reads it). Native boot hand-mirrors the admin variables schema via raw CREATE TABLE before the migration runs (`cli/server.rs:215-241`) — the schema-drift footgun the browser path was refactored away from, and the cross-crate comment is now inaccurate. JWT secret seeded outside the ConfigVar/auto_generate pipeline (`cli/server.rs:339-350`), inconsistent across all three targets. Browser `*const Wafer` held across `.await` via avoidable `unsafe` (use `Rc<Wafer>`).

## Prioritized action list

| Priority | Theme | Location(s) | Effort | Why |
|---|---|---|---|---|
| **P0** | Unauthenticated A2A JSON-RPC endpoint | `messages/a2a.rs:39-69` | M | Any caller can enumerate/read/cancel/create tasks; comments falsely claim auth. Gate on `msg.user_id()` in `handle_a2a`. |
| **P0** | Stored XSS in admin network page | `admin/pages/network.rs:169-172` | S | Unauthenticated path plant → JS exec in admin session. Move to `data-*` + delegated handler. **Quick win.** |
| **P0** | JWT secret defaults to empty (fail-open) | `builder.rs:313-315` | S | Forgeable admin tokens on Cloudflare; reject empty secret in `build()`. **Quick win.** |
| **P0** | `key_var` never resolved → all secret providers 401 | `llm/routes.rs:412-436`, `migrations/legacy_providers.rs` | M | Documented LLM secret path is completely broken. Resolve `key_var` via `config::get` in both reload paths. |
| **P0** | Streaming chat drops assistant-message persistence | `llm/routes.rs:295-353` | M | Corrupts conversation history. `clone_arc()` + `messages_create` after stream. |
| **P1** | Silent error-swallowing on write paths | `auth_ui/settings.rs:215-228`, `userportal/mod.rs:777-782`, `admin/email.rs:124-129`, `admin/mod.rs:401,412` | S | Success toast on failed/denied writes; WRAP-grant case is security-policy. **Quick wins.** |
| **P1** | `must_reset` JSON int breaks Postgres | `auth/repo/local_credentials.rs:64` | S | Write `json!(must_reset)` to match convention. **Quick win.** |
| **P1** | Stripe checkout-claim leak + off-schema write | `products/stripe.rs:191-194,205-208` | S | Purchase stuck forever; provider/updated_at silently dropped on D1. |
| **P1** | Pricing formula ignores trailing tokens | `products/pricing.rs:143-148` | S | Wrong-but-plausible money totals. Return Err if `pos != tokens.len()`. **Quick win.** |
| **P1** | Vector raw-SQL convention violations | `vector/pages.rs:697-715, 598-609` | M | Add metadata-delete op + `introspect::build_table_exists`. |
| **P1** | Extra routes bypass feature gate | `routing.rs:264-297` | S | Admin disable toggle no-ops for downstream blocks. **Quick win.** |
| **P1** | Admin JSON-API audit-logging gap | `admin/iam.rs`, `settings.rs`, `users.rs` | M | Incomplete audit trail on security-relevant block. |
| **P1** | Config-var/branding metadata duplication | `userportal/mod.rs:649-783`, `auth_ui/pages/settings.rs:18-108`, `llm/pages.rs:28-30` | M | Drift + URL-validation bypass; drive from `ConfigVar`. |
| **P1** | Cross-repo HKDF key duplication | `crypto.rs:153-165` | M | Comment-only coupling already broke prod once; expose/verify-via wafer-block-crypto or add a shared test vector. |
| **P1** | Native boot hand-mirrors variables schema | `cli/server.rs:215-241` | M | Re-introduces the schema-drift footgun the browser path removed; missing `block` column already. |
| **P1** | Compat shims to delete | `impresspress-web/config.rs:152-199`, `cli/server_config.rs:97-129` | S | Self-flagged; canonical migration is now the source of truth. **Quick wins.** |
| **P1** | Request-body limit after buffering | `impresspress-cloudflare/convert.rs:35-40` | M | Per-request OOM vector; pre-check `content-length`. |
| **P2** | Block-name encoding centralization | `permissions.rs:236-247`, `blocks.rs`, `database.rs`, `cloudflare/config_source.rs:69-78` | S | Use existing `wafer_block::wrap::resource_prefix`. |
| **P2** | Timestamp/bool/decode helper consolidation | auth repos (10× `now_iso`), 5× bool decode | S | Move to `blocks/helpers.rs`, delete copies. |
| **P2** | Adopt existing shared abstractions | `crud.rs`, `ui::components::tab_navigation`, tab bars | M–L | Helpers built but unused; migrate + add `crud.rs` tests. |
| **P2** | Introduce `repo/` layers | `files`, `legalpages`, `vector` | L | Maintainability/testability; no correctness impact. |
| **P2** | Wall-clock duration subtraction | `storage.rs:312-336,373,391` | S | Use the `saturating_sub`+`try_from` idiom from `pipeline.rs:189-194`. |
| **P2** | Wrong "Security" nav icon | `nav_groups.rs:60` | S | Add `"lock"` arm; cosmetic. **Quick win.** |
| **P2** | Dead code cleanup | `ErrorCode::status_code()`, `cloudflare/helpers.rs`+`maud` dep, unused icons, `wafer_info::handle_flows` | S | Reduce surface + WASM size. |
| **P2** | legalpages dual CRUD / publish ordering | `legalpages/mod.rs:201-333` vs `pages.rs:651-723` | L | Collapse onto shared repo functions (safe publish-then-archive). |
| **P2** | `exec_raw` returns 0 rows-affected | `cloudflare/database.rs:460-474` | S | Latent contract bug; copy the `meta().changes` pattern from `increment_field_where`. **Quick win.** |