# ImpressPress architecture, code-smell and duplication review — 2026-09-05

Branch reviewed: `feat/improvements` at `b8b8f29d` plus the uncommitted working-tree diff (mostly `impresspress-cloudflare`, `impresspress-core/builder`, and the CLI's Cloudflare helpers). Findings say "uncommitted" where they only exist in that diff.

Scope: architecture smells, code smells, and duplication across all seven crates and the TypeScript packages. Security findings from `CODE_REVIEW_2026-07-16_FINDINGS.md` are out of scope, but correctness bugs found while looking for smells are listed in section 3.

**Base caveat, added after verification.** `feat/improvements` has no commits of its own and is 454 commits behind the repository's `main` at `fb781f01` (2026-09-04), which merged the dev-sandbox series, typed rows for products/llm/vector, and the admin redesign (PR #85, which moved `ui/` into `components/` and `styles/` directories). Line numbers below refer to the reviewed tree. The Phase 0 bugs in section 3 were re-checked against `main`: B1 (role-delete guard) and B5 (CSP overwrite) are already fixed there, although `IMPRESSPRESS_CSP` in `impresspress-web/src/lib.rs` is now an unused constant and deserves its own look; B2, B3, B4, B6, B7, B8, B12, B14, B15, B16, B17 and B24 are present on `main` as described. Findings in 5.5 and T8 that cite `admin/pages/*` or `ui/*` predate the redesign and should be re-verified before acting. The uncommitted diff on `feat/improvements` is a divergent draft of work that reached `main` through the dev-sandbox PRs (for example `runtime_cache.rs` still differs from `main` by about 1,300 lines); compare before rebasing or discarding it.

Method: twelve independent read-only review passes split by concern (core platform; products domain; products Stripe and pages; auth/auth-ui/userportal; admin and shared UI; shared block infrastructure and cross-block consistency; files/messages/legalpages; llm/vector/embedding; browser-vs-Cloudflare adapters; Cloudflare runtime/native/web/bundle; CLI; TypeScript packages, tests, CI and hygiene), seeded with a mechanical duplicate-window scan, git churn, and cfg-density signals. No code was built or modified. Twelve of the highest-impact claims were re-verified against source before writing this document.

Size of the codebase under review:

| Area | Lines (non-test) |
|---|---|
| `impresspress-core` | ~92k (of which `blocks/products` 26k) |
| `impresspress-cloudflare` | ~6.7k |
| `impresspress` (CLI) | ~5.8k |
| `impresspress-browser` | ~5.7k |
| `impresspress-bundle`, `-native`, `-web` | ~2.1k |
| Test files (`tests/`, `*_tests.rs`, `test_support.rs`) | ~19k |
| TypeScript (`packages/`) | ~4.7k |

---

## 1. Executive summary

1. **The route surface is the single biggest structural problem.** Routes are declared in three to five places (block `ROUTES` tables, `BlockInfo::endpoints`, the core `routing.rs` prefix table, hand-written schema literals, nav tables) and matched two to three times per request. Fourteen blocks use seven different dispatch mechanisms; four blocks rewrite the wire path before matching. Ten `router_declared_public` escape hatches exist because two files disagree about who owns a declaration. Fixing this one axis removes roughly a third of all findings.
2. **Layering is inverted in two directions.** Core knows every block's URL prefix, default-enabled flag, and admin table names; meanwhile platform state (variables, block settings, WRAP grants, request logs) is owned by the admin *feature block* and reached into from `pipeline`, `boot`, `features`, `cache_key`. `admin_schema.rs` and `messages_schema.rs` exist to launder cross-block table reads.
3. **Repository boundaries are advisory, not enforced.** A `USERS_TABLE` re-export lets four blocks write the users table with raw column maps; `products/pages.rs`, `legalpages`, `llm/pages.rs` and `admin` issue `db::*` directly against other modules' tables; files returns untyped `Record`s that every page re-decodes (one column is decoded as a bool on one page and a string on another).
4. **Failure is routinely swallowed into a safe-looking default.** Twenty-plus sites turn a database error into "not found", `false`, `0`, an empty list, or `true`. Several of those are on security guards (system-role delete, refresh-token reuse detection, share ownership, quota enforcement, block-enabled flag). Section 3 lists them.
5. **State is stringly-typed even where enums exist.** Products has 20+ contract enums but stores and transitions on string literals; statuses, roles, kinds and boolean config flags are parsed by hand with inconsistent truth tables across blocks.
6. **Runtime wiring is unified up to `build()` and duplicated after it.** Three post-build lifecycle implementations, two config surfaces mirrored by hand in every target, two WRAP-grant loaders, and a browser adapter that never took a dependency on `impresspress-core` and so re-implements streaming, SSRF and a whole SQLite vector backend that upstream already models.
7. **Hand-synced lists appear wherever the project rules forbid them:** block feature forwarding (already drifted), the wasm32 middleware list, the Cloudflare environment-identity list, `ENABLED_DEFAULTS`, per-block migration arrays, quota field whitelists, the SDK's service fan-out arrays, and two 500-line CI workflows that are verbatim copies.
8. **The periphery was never swept after the solobase rename:** visual baselines live in a Playwright-MCP scratch directory next to 97 committed session dumps, 34 GB of worktrees sit un-ignored under `.claude/`, the SDK exports Go-era types, two service workers implement the same wasm with different policies, and the release workflow has never run and cannot succeed as written.

What is healthy: the pure-logic core modules (`streaming`, `cache_key`, `kv`, `metrics`, `multipart`, `csrf`, `prepared_plan`), `offer_pricing.rs`, the `contracts.rs` typed boundary, the auth repo layer (9 of 12 modules follow the canonical pattern, all raw SQL confined to tests), the `stripe_provider`/`stripe_client` pair, the wafer-core `DbExec` layering in both wasm adapters, `ImpresspressBuilder` registration, and the shared `TestContext` harness. The gen-2 UI component set (`shell_page`, `data_table`, `button`, BEM classes) is good; products adopted it wholesale.

---

## 2. Cross-cutting themes

### T1. Routing: declared N times, matched M times, rewritten in between

Evidence (all in `crates/impresspress-core/src` unless noted):

- Core prefix table `routing.rs:184-293` names 11 blocks by string literal; `Route::router_declared_public` (`:79-110`) is used 10× (`:200, :209-224, :243-248`), 8 of them for auth-ui endpoints the block dispatches but never declares. The comments at `routing.rs:98-101` and `:209-211` say the real fix is to declare them in the block.
- Per-request match chain for `DELETE /b/vector/api/indexes/foo`: wafer glob (`flows/site_main.rs:35-44`) → `ImpresspressRouterBlock` (`blocks/router.rs:89-130`) → `starts_with` prefix scan (`routing.rs:422-478`) → `endpoint_auth` template match over every endpoint (`routing.rs:354-361` → `endpoint_match.rs:212-230`) → block-local `endpoint_match::dispatch` re-matching the same templates (`endpoint_match.rs:151-190`). Legacy `util::path_param` prefix-strip (`util.rs:33-44`) is a fifth mechanism; its comment about "native axum routing" is stale.
- Dispatch mechanisms across 14 blocks: `endpoint_match::dispatch` (messages, vector, legalpages, llm); `dispatch_path` over *rewritten* paths (products, `handlers/dispatch.rs:85-311, 451-695`); hand-rolled `match (action, path)` (auth_ui `mod.rs:379-447`, userportal `mod.rs:91-103, 177-211`); sync classifier enum plus a second-tier match on a rewritten `/admin/...` path (admin `route.rs:123-268` then `users.rs:24`, `iam.rs:33`, `settings.rs:115`, `database.rs:128`, `logs.rs:24`); `starts_with` guard chain (files `mod.rs:142-251`); prefix/suffix scan tables ×3 (system `:78-161`); `msg.kind` service match (email, storage).
- Wire-path rewriting: admin `mod.rs:143-146, 176-182` rewrites `/b/admin/api/X` → `/admin/X` and mutates `req.resource` before delegating to files, which matches the synthetic path (`files/mod.rs:152-157`); products `mod.rs:2111-2127` rewrites so that `/b/products/api/catalog` and `/b/products/catalog` both reach the same handler; auth_ui strips `/b`; userportal strips `/b/userportal`. `admin/route.rs:274-278` documents a bug already caused by the `req.resource` mutation.
- Products declares its 119 endpoints in `mod.rs:1101-1971` with wire spelling `/b/products/api/admin/...` but dispatches on `/admin/b/products/...`, so declared and dispatched surfaces cannot be cross-checked. Its 48 hand-written JSON schemas (`mod.rs:301-1014`, ~710 lines) have already drifted from `contracts.rs`: `mod.rs:331` lists `pending_review`, the code writes and reads `pending` (`handlers/product.rs:524`, `handlers/sellers.rs:102`, `pages.rs:545`).
- Files serves seven routes that are absent from `info().endpoints` (`files/mod.rs:55-58` admits "follow-up"); undeclared routes fall back to the prefix auth tier and are invisible to `/openapi.json`. Admin's `BlockInfo::endpoints` (`admin/mod.rs:110-128`) advertises four pages that are actually 308 redirects and omits `/settings/*`, `/api/{database,extensions,storage,cloudstorage}`, `/grants/rules`, `/iam/roles`, `/blocks/*/toggle`. Legalpages' hand-typed "Endpoints" page (`pages.rs:363-504`) documents `POST .../publish`; the route is `PATCH`.
- `endpoint_match.rs:246-265` `normalize_template` is a compat shim for exactly one `:hash`-style declaration (`userportal/mod.rs:51`); overlapping templates (`files/mod.rs:48` vs `:134`) force `endpoint_auth` to scan every endpoint and take the strictest.

Root-cause direction: one `EndpointRoute` table per block that *produces* `BlockInfo::endpoints` (with handler key, auth level, and flags such as `requires_user_products`), one matcher in the router that binds `req.param.*` and enforces the declared level, blocks consume `msg.var()` and the handler key. Delete `ROUTES` in `routing.rs`, `router_declared_public`, `Route::proxy`, all wire-path rewriting, `util::path_param`'s prefix argument, `crud::id_from_path`'s fallback, and `normalize_template`. Then the `impresspress_feature_block!` macro (which hides a `let mut` behind closure-looking syntax and still leaves the block name spelled three times per block, `feature_block.rs:48-52, 122-123`) can become an explicit `FeatureBlock` trait with `const NAME`, `fn routes()`, `fn migrations()`.

### T2. Inverted layering between core and blocks

- Core → blocks: `routing.rs` ROUTES; `features.rs:201-231` `ENABLED_DEFAULTS` (all seven entries `true`; comment `:211-214` excludes llm/vector "until the LlmService refactor lands", which `blocks/mod.rs:18-29` says already landed); `builder/registration.rs:84, 94` use the literal `"impresspress/admin"` beside an existing `ADMIN_BLOCK_ID`; `ui/assets.rs:174-274` gates llm and files assets behind `cfg(feature = "block-*")`, mirrored in `blocks/system.rs:123-153`.
- Blocks → platform state: `pipeline.rs:354,357`, `cache_key.rs:9`, `boot.rs:24,309,330` import table constants from `blocks::admin`; `admin_schema.rs` was meant to be the single source but three of four consumers bypass it and it lacks `WRAP_GRANTS_TABLE`. `messages_schema.rs` exists so `llm/pages.rs:123,144` can `db::list` the messages block's tables directly while the same block *writes* through `call_block` (`llm/mod.rs:186,204`); this needs two WRAP grants and a re-export shim, and lets llm's Cargo feature claim it "compiles cleanly without block-messages" while every chat turn is dropped at runtime.
- Block → other block internals: admin imports `auth::{USERS_TABLE, API_KEYS_TABLE}` and hardcodes `deleted_at IS NULL` at 5 sites and `record.data.remove("password_hash")` at 4 sites (`admin/pages/users.rs`, `dashboard.rs`, `admin/users.rs`, `ops.rs`); `auth/mod.rs:64` imports `admin::USER_ROLES_TABLE` and raw-inserts roles (`:404-424`), duplicated in `auth_ui/oauth/callback.rs:458-468`, while `auth_ui/api/signup.rs` writes no roles row at all (an OAuth-created admin and a password-created admin end up with different rows; `get_user_roles` merges both "by accident"). Email templates hardcode auth's URLs (`email.rs:267, 283, 301`). `rate_limit.rs:205` writes into auth's table.

Root-cause direction: a core `platform_state` module owns variables, block settings, WRAP grants and request logs (constants, migrations, repo functions); admin renders them. Delete `admin_schema.rs`, `messages_schema.rs`, and the `blocks::admin`/`blocks::auth` re-exports; seed `ENABLED_DEFAULTS` from `all_block_infos()`; blocks own their static assets and register them.

### T3. Repository boundaries are advisory

- `auth/mod.rs:49-59` re-exports `USERS_TABLE` "to keep existing consumers on stable identifiers". Raw-map writers: `auth_ui/api/login.rs:102-106`, `oauth/callback.rs:109-115`, `api/signup.rs:157-175` (while `users::set_verification_token` exists), `userportal/mod.rs:270-279` (while `users::update_profile` exists), `admin/ops.rs:115,167,231`, and `auth/bootstrap.rs:64-114` (11-column map with a stale justification about legacy columns that migration 006 already creates).
- `products/pages.rs` imports `wafer_block::db` and queries at 14 sites; three of those are verbatim copies of `handlers/sellers.rs` queries (`pages.rs:509-520 ≡ sellers.rs:43-55`, etc.).
- Legalpages has no repo layer: `mod.rs:158` `pub(crate) const COLLECTION` (the naming the rules forbid), direct `db::*` at 7 sites, the same filter block copied 5×, and a generic `PATCH` route (`mod.rs:643-645` → `crud.rs:135-142`) that bypasses `service::publish_document`, so `PATCH {"status":"published"}` leaves two published rows.
- Files repo returns `Record`; six page-side decoders exist, with `BucketRow`/`AdminBucketRow` and `ShareRow`/`AdminShareRow` as parallel decodings. `public` is read with `.as_bool()` in `pages_user/buckets.rs:152-156` and `str_field("public") == "true"` in `pages_admin.rs:269`.
- llm's settings table (`mod.rs:139`) is accessed as raw `HashMap` rows with repeated string keys at `mod.rs:234, 245, 345, 348, 359-360` and `pages.rs:468-470`.
- Auth's *own* two credential mechanisms are split: `AuthServiceImpl::require_user/require_role` (`auth/service.rs:312-440`) accept a `wafer_session` cookie or PAT bearer that nothing in production issues; the live flow is the `auth_token` JWT verified in `crypto.rs:100-140`. `auth_ui/mod.rs:11-13` claims auth-ui "calls the framework auth block via the `auth@v1` client"; zero such calls exist, and auth_ui imports `auth::repo` directly in 15 files.

Root-cause direction: add the missing repo functions (`users::touch_last_login`, `set_disabled`, `soft_delete`, verification fields on `NewUser`; `seller_accounts::list_contracts`; `legalpages/repo/documents.rs`; typed `BucketRow`/`ObjectRow`/`ShareRow`/`QuotaRow`), delete the re-exports, and decide the auth credential model (route JWT through `AuthServiceImpl`, or delete the dead branches).

### T4. Failure swallowed into a safe-looking default

Security-relevant sites are in section 3. The broader pattern:

- SSR pages render "No contexts yet", "Total Users 0", "No tables yet" during a DB outage: files `pages_user/*` (4 sites), `pages_admin.rs` (4), messages `pages.rs` (3, no log), admin `dashboard.rs:137,202,251,384,385`, `network.rs`, `storage.rs`, `blocks.rs`, `permissions.rs`. `ui::server_error_response` (`ui/mod.rs:420`) is unused by all of them. Seven admin tabs render errors; eleven swallow them.
- Legalpages `pages.rs:534-547` turns a lookup *error* into creating a new draft; `mod.rs:367` `db::count(..).unwrap_or(0)` re-runs `seed_defaults` on a count error.
- Products: `repo/purchases.rs:366` `let _ = delete_with_line_items` (compensation failure, no log); `handlers/mod.rs:144-149` `default_template_id` `.ok()` creates rows with no template during an outage; `repo/subscriptions.rs:468-527` and `repo/purchases.rs:1822-1854` return `Option`/`bool` so webhook handlers cannot distinguish "not ours" from "DB down" (callers `stripe.rs:3493, 3626, 4275, 4288`); `stripe.rs:1169-1256` five `let _ = mark_checkout_failed`; `stripe.rs:4273-4293` `user_owns_product` returns `false` on `Err`.
- llm: `mod.rs:185-191` `messages_create` returns `None` on any failure, no log; `chat.rs:140` and `streaming.rs:131` `let _ = messages_create` (user message may never be stored yet the model is still called; SSE sends `[DONE]`).
- Auth: 12 handler sites collapse `Err` to 404/401/empty (`reset_password.rs:38`, `api_keys.rs:180,202`, `me.rs:20`, `verify.rs:51-53,101`, `change_password.rs:41-45`, `refresh.rs:92,106-110`, `userportal/pages/sessions.rs:134` returns 200 having revoked nothing) while `login.rs:34-41` and `signup.rs:272-281` each carry a paragraph explaining why not to.
- Runtime: `pipeline.rs:354` `let _ = db::create(...)` drops the request-log row; `impresspress-cloudflare/src/config_service.rs:26-28` `set` is a silent no-op; `impresspress-bundle/src/bundle/mod.rs:240,348` swallow `remove_file` failures that leave a stale hashed artifact (the exact case its own comment warns about).
- Error *types*: six styles coexist (`errors.rs` `ErrorCode`/`error_response`, `http::err_*`, raw `OutputStream::error`, `Result<_, String>`, `Result<_, OutputStream>`, `bool`). `ErrorCode::status_code()` (`errors.rs:80-111`) has zero non-test callers and disagrees with the wafer mapping it sits next to (`QuotaExceeded` → 413 vs `ResourceExhausted` → 429). `errors.rs:117-120` says the `"[code] message"` prefix "is gone"; `rate_limit.rs:363-366` still emits it. Eight sites hand-map `NotFound → 404` and collapse everything else (including WRAP `PermissionDenied`) into `500 "Database error"`.

Root-cause direction: repo functions return `Result<Option<_>>`/`Result<bool>`; one `http::require_row` helper; one `crud::db_error` preserving the wafer code; `From<ErrorCode> for OutputStream` as the single constructor; page renderers take `Result` and use `server_error_response`.

### T5. Stringly-typed state and hand-parsed flags

- Products: `SellerAccount.status: String` with the same four-way ladder computed 3× (`repo/seller_accounts.rs:143-151, 207-215, 355-363`); `OfferStatus` exists but `repo/offers.rs:622, 651, 702, 729-735` use literals; `ProductStatus` never used to write; 23 order-status literals in `repo/purchases.rs`; webhook event types dispatched as `&str` in a 1,122-line match (`stripe.rs:2983-4104`) with the 12 strings re-listed in `pages.rs:2596-2609`; three unrelated literal lists for event/operation statuses (`stripe.rs:40-44`, `handlers/provider.rs:69, 104`, `repo/provider_operations.rs`).
- Files/messages/legalpages: object status, doc type/status, context type, entry kind and role are literals; `doc_type` is accepted unvalidated from request bodies (`legalpages/mod.rs:302-306`, `pages.rs:511-519`); the `"terms"/"privacy"` label mapping is copied 3×.
- llm writes role `assistant`; messages documents `agent`; messages UI accepts both; `role_from_str("agent")` → `ChatRole::User` (`chat.rs:44-51`), so an agent entry is replayed as a user turn. `DEFAULT_PROVIDER = "impresspress/provider-llm"` (`llm/mod.rs:143`) names a block that no longer exists and is translated to "first enabled provider" on every chat (`routes/chat.rs:101-114`).
- Auth: `REQUIRE_VERIFICATION` parsed with the same 3 lines in three handlers; `ALLOW_SIGNUP` and `ENABLE_OAUTH` accept `"true"|"1"` in the API path but only `"true"` in the page path, so `=1` enables signup but hides the link (`auth/mod.rs:433-436` vs `pages/login.rs:18-21, 52-55`). Three "is this key sensitive" rules (`boot.rs:210`, `util.rs:364-366`, `prepared_plan.rs:773-783`); URL-ness decided by `InputType` on one surface (`settings_form.rs:338-346`) and by key suffix on the other (`ops.rs:370, 458`).
- Timestamps: `now_iso` is declared "the single timestamp writer" (`auth/repo/mod.rs:45-55`) yet six copies of the formatter exist and a second `+00:00` format from `util::stamp_updated` is written into the same `updated_at` column (`users.rs:159` vs `:190, 223, 274, 287, 315`); expiry checks rely on string ordering.
- Money: purchases header columns are `*_cents` while everything else is `*_minor` (52 sites; `money.rs` supports 0- and 3-decimal currencies so "cents" is wrong for JPY/KWD); duplicate columns `total_cents`/`amount_cents` and `user_id`/`buyer_user_id` are kept in sync by every writer; two decimal parsers (`money.rs:41-94` vs `offer_pricing.rs:65-103`).

### T6. Runtime wiring after `build()` and the browser adapter

- Three post-build lifecycles: `builder/boot.rs:115-138` (tolerant), `deploy_init.rs:56-112` (reported), and the Cloudflare request path's hand-rolled `apply_db_wrap_grants → seal → strict_init_all_blocks → post_start` (`runtime_cache.rs:466-481, 656-669`). The CF path silently skips `seed_after_admin_init`. `boot.rs:60-63` claims CF and browser "collapse to a hook impl"; they do not.
- Two config surfaces (`ConfigService` and the config snapshot) are filled by hand in `cli/server.rs:133-177` ("Both must carry the same data"), `cloudflare/lib.rs:1085-1140`, and `web/lib.rs:190-221` (the web copy was only just added in the uncommitted diff because it was missing). Both `BootHooks` impls duplicate the block-settings republish.
- **Committed bug:** `impresspress-web/src/lib.rs:108-111` pushes the WebLLM/transformers CSP via `block_config`, applied at `registration.rs:253`; step 12 (`registration.rs:390`) then calls `register_site_main`, which `add_block_config`s the shared default CSP (`flows/mod.rs:58-61`, a `HashMap::insert`, last write wins). The browser build ships the Stripe default CSP.
- `cloudflare/lib.rs` is 1,783 lines with eight responsibility clusters (grew from 849 in the uncommitted diff); `build_runtime` (233 lines) and `request_services_for_dispatch` assemble the same service bundle twice; the environment-identity hash (`lib.rs:416-496`) enumerates 12 named components that must mirror every `env.var`/`secret` read by hand. `get_or_build` and `get_or_build_prepared` are 180-line twins (`runtime_cache.rs:336-521` vs `:530-710`); the prepared path silently omitted `apply_db_wrap_grants`. The "request-current vs kernel-safe" classification from the 07-24 design doc is enforced by comments, not types.
- `registration.rs:212-235` hand-lists six wasm32 middleware registrations that must mirror the `use_static_blocks!` anchor at `:16-23` (design doc §13 Stage 2 still open). Block-feature forwarding is hand-synced across three `Cargo.toml`s and has already drifted (cloudflare forwards 5, web 7, CLI 8; a CF consumer cannot enable `block-llm` at all).
- Browser adapter: `impresspress-browser` has no dependency on `impresspress-core` although `impresspress-web` (also wasm32) already does. Consequences: `convert.rs` copies `streaming.rs` helpers (its comment at `:283-285` says why), never consults `wants_streaming`/`resp.stream` (a marked download is fully buffered in the service worker), has no buffered-size cap, and `network.rs` has no SSRF gate, no response cap, no streaming, and flattens multi-value headers. `browser/vector/sql.rs` + `service.rs` re-implement wafer-sql-utils' vector schema, `apply_filter` verbatim, and an inline RRF that `fuse_scored` already provides, with their own flush path beside the database service's.
- KV-cached D1 wrapper inherits `take_where` and `delete_where_count` trait defaults (0 overrides in `kv_cached_db.rs`; D1 has atomic overrides at `database.rs:492-506`), so a `take_where` lists from possibly-stale KV then deletes per row. `conformance.rs:57-59` asserts it is a pure pass-through; the compile-time conformance check cannot see inherited defaults. `crypto_service.rs:6-9` records that this exact failure mode already caused a production auth regression.
- Platform asymmetry in committed code: browser and native re-parse TEXT that looks like JSON into structured values; CF's `json_to_record` does not, so a JSON column reads back as an object on two platforms and a string on D1.

Root-cause direction: one `builder::boot(wafer, hooks, InitPolicy)`; builder owns both config surfaces; browser takes `impresspress-core = { default-features = false }` and uses `streaming::*` and `ssrf::*`; typed `CfEnvironment::capture(env)`; `RequestScoped<T>` newtype. Most of the shared adapter pieces belong upstream in wafer-run (section 6).

### T7. Hand-synced lists

`ENABLED_DEFAULTS`; the `ROUTES` prefix table; nine auth-ui carve-outs; 48 product schemas; two products dispatch tables plus two `requires_*` variant lists; per-block migration arrays (`SQLITE_MIGRATIONS`/`POSTGRES_MIGRATIONS` plus 19 three-line `include_str!` pairs and a 38-entry test table per block; products and auth already differ in their `cfg`); `ALLOWED_QUOTA_FIELDS` duplicating `QuotaConfig`'s fields three files away; messages' `allowed = ["status","title","metadata"]`; the wasm32 middleware list; block-feature forwarding; the CF environment-identity list; `RELEASE_ASSET_*` read in three places; the SDK's six-element service arrays listed twice (`client.ts:49-57, 65-73`); two CI workflows copied by hand (`ci-main.yml` lacks the HTML guard, WRAP-grant audit, browser wasm test and all e2e jobs); the wire protocol between CLI and Worker re-declared as string literals and `Deserialize` twin structs with `deny_unknown_fields` (adding a field to `DeployInitReport` breaks every deploy at runtime).

### T8. Two UI generations and five ways to embed JS

- `ui/` contains gen-1 (`.btn-primary`, `div.table-container > table.table`, `templates::PageHeader`, `templates::StatTile`, 4-variant `BadgeVariant` against 6 CSS classes) and gen-2 (`components::button`, `data_table`, `page_header`, `stat_card`, BEM `.btn--primary`, `shell_page`). Products is fully gen-2 (114 component calls, 74 raw BEM); admin, which owns the chrome, is fully gen-1 (35 legacy buttons, 19 raw tables, 39 raw badges, 7 `PageHeader`s all with `title: ""`, and its own `admin_page` wrapper). `components.rs:7-13, 121-127, 203-209` are orphaned section headers with no code.
- `admin_page` (`admin/pages/mod.rs:41-61`) never calls `nav_groups::retain_registered`, which `shell_page` does (`ui/mod.rs:277-285`), so on Cloudflare and browser builds every admin page shows sidebar and ⌘K entries for `/b/llm/`, `/b/vector/`, `/b/messages/` that 404. `ui/mod.rs:262-265` says `shell_page` "replaced the six per-block wrappers"; admin's is the one that was not.
- JS is embedded as Rust raw-string functions (`ui/assets.rs:295-433` `palette_js`, 141 lines), `include_str!` files, `script { PreEscaped(..) }` inside pages (`permissions.rs:213-286`, `network.rs:95-121`, `sidebar.rs:152-178`), attribute handlers (`database.rs:90` ~430-char minified `oninput`; the eye-toggle copied 3×), and `format!`-generated scripts (`settings_form.rs:217-240`). Products embeds ~480 lines (`pages.rs:1118-1520` `PRODUCT_WIZARD_JS`) although `ui/assets.rs:229-266` already serves `llm-chat.js` as a hashed asset. `network.rs:101-104` documents why attribute JS is a stored-XSS sink and uses `data-*` delegation; the rule is not applied elsewhere. Messages' entry card is rendered in both Rust (`pages.rs:32-63`) and JS (`llm-chat.js`) with a "keep in sync" comment.

### T9. Periphery not swept since the rename

See section 5.12 and the bug table. Highlights: `.playwright-mcp/` holds 72 PNG baselines and 97 committed MCP accessibility dumps (a branch `chore/baselines-out-of-mcp-scratch` at `258fe344` already moves them; land it); `.claude/worktrees/` is 34 GB and not ignored; `packages/solobase-site/` is 89 MB of ignored leftovers with zero tracked files; root `package.json` is an empty stub; `payment-link-state.jpeg` and `.intentionally-empty-file.o` are tracked and unreferenced; `.githooks/pre-commit` runs stable `cargo fmt` against nightly-only `rustfmt.toml` options and looks for a `packages/cloud-dashboard` prettier that does not exist; `release.yml` lacks the wasm-pack step that `crates/impresspress/build.rs:23-37` hard-requires, lacks `--locked`, and no `v*` tag has ever been pushed; the `sdk` CI job still clones wafer-run to build a dependency the SDK removed; `NICE_TO_HAVE.md` lists four items that are done, moved, or refer to code that does not exist here.

---

## 3. Correctness bugs found while looking for smells

Ranked by consequence. All verified against source unless marked.

| # | Where | What happens | Fix shape |
|---|---|---|---|
| B1 | `blocks/admin/ops.rs:324-336` | `if let Ok(role) = db::get(..)` guard, then unconditional `db::delete`. A transient read error deletes the `admin` system role. Sibling `handle_update_role` (`iam.rs:113-123`) was made fail-closed in PR #19; this one was not. Reachable from JSON and SSR. | Match all three arms like `iam.rs`; add `FailingGetContext` test. |
| B2 | `blocks/auth_ui/api/refresh.rs:88-101` | SEC-039 reuse detection: `if let Ok(true) = family_has_live_row` skips the `Err` arm and `let _ = revoke_family` discards the result. A DB error leaves the attacker's rotated family valid. The three sibling handlers are explicitly fail-closed. | Propagate both; `FailingDbOpContext` test. |
| B3 | `blocks/files/quota.rs:30-52`, `cloud.rs:148-158` | `get_user_quota` restores the 1 GiB default on *any* error (an admin-lowered quota silently reverts); `get_used_bytes` `.unwrap_or(0.0)` so `check_quota` passes on a DB blip; `handle_delete_share` `if let Ok(share)` skips the ownership check on a lookup error and deletes anyway. | `Result<Option<_>>`, fail closed; match NotFound/Err/Ok. |
| B4 | `blocks/files/storage/objects.rs:268-278`, `buckets.rs:98-106` | Blob deleted, then `repo::objects::delete_by_bucket_key(..).await.ok()`. A surviving row keeps charging the uploader's quota (`repo/objects.rs:296-300` sums every row) for a blob that no longer exists. | Propagate; delete metadata first or mark `deleting`. |
| B5 | `impresspress-web/src/lib.rs:108-111` + `builder/registration.rs:253, 390` + `flows/mod.rs:58-61` | Browser build's CSP overwritten by the shared default at step 12. **Committed.** | Web supplies CSP via `CSP_DIRECTIVES_KEY`; builder applies consumer configs after `register_site_main`. |
| B6 | `impresspress-cloudflare/src/kv_cached_db.rs` (no `take_where`/`delete_where_count`) | Inherits trait defaults: `take_where` = `list` (served from possibly-stale KV) + per-row `delete`; bypasses D1's atomic `DELETE … RETURNING`; N+1 round-trips. Conformance test asserts pure pass-through. | Implement both; upstream forwarder macro so decorators must enumerate every op. |
| B7 | `crates/impresspress/src/cli/server_config.rs:34-99` | Raw rusqlite `SELECT … FROM impresspress__admin__wrap_grants`; every error → empty vec. With `IMPRESSPRESS_DB_TYPE=postgres` this opens a stray empty sqlite file and boots with zero dynamic WRAP grants, silently. Core has `boot::load_wrap_grants_from_db`. | Delete; call the core loader with the DB service built at `server.rs:64`; make it `Result`. |
| B8 | `blocks/llm/providers/service.rs:164-179` | `ProviderLlmService::new()` = `try_new().unwrap_or_else(|_| … reqwest::Client::new())`: on TLS-init failure the fallback client has no SSRF-revalidating redirect policy. `builder/registration.rs:132` uses `new()` for production. | Builder calls `try_new()?`; delete `new()` and `Default`. |
| B9 | `blocks/llm/provider_admin.rs:53-67`, `routes/providers.rs:200-314` | `NoopProviderAdmin::configure` is `{}`; `POST /b/llm/api/providers` persists, no-op reloads, returns 200; Discover returns a Debug-formatted 500. On every non-native build the admin creates providers that can never route. Cloudflare still excludes `block-llm`/`block-vector` entirely (`cloudflare/Cargo.toml:74-76` comment is stale). | fetch-based transport so `ProviderLlmService` compiles on wasm32; `Option<Arc<dyn ProviderAdmin>>` that omits the endpoints when `None`. |
| B10 | `blocks/legalpages/mod.rs:643-645` → `crud.rs:135-142`; `pages.rs:534-547`; `mod.rs:367` | Generic PATCH bypasses `publish_document` (two published rows; public page serves whichever has the higher version). A lookup *error* creates a new draft. A count error re-runs `seed_defaults`. | Typed patch through the service; propagate. |
| B11 | `blocks/products/mod.rs:331` vs `handlers/product.rs:524`, `sellers.rs:102`, `pages.rs:545`; `contracts.rs:30, 39` | Published API schema says `pending_review`; code writes and reads `pending`; `contracts.rs` has both `PendingReview` and `Pending`. | Derive schemas from contract types. |
| B12 | `blocks/auth/mod.rs:749-774`, `auth_ui/api/refresh.rs:155-168`, `auth/repo/sessions.rs:95-106` | A session row is inserted per access-token refresh (~48/day per tab at the 30-min access lifetime), lives 30 days, is never deleted on logout, and is shown as a separate device on `/b/userportal/sessions`. `delete_expired` for sessions, jwt_blocklist and oauth_pkce have no production caller (two are `#[allow(dead_code)]`, one says "TODO: not yet wired"). Three D1 tables grow unbounded. | Key by refresh family; touch on rotation; delete on logout; wire sweepers. |
| B13 | `blocks/files/pages_user/buckets.rs:152-156` vs `pages_admin.rs:269` | `public` decoded as bool on one page and as string `"true"` on another; different answers on SQLite TEXT. | Typed `BucketRow::from_record`. |
| B14 | `blocks/auth_ui/api/sync_user.rs:13-16` | Reads `WAFER_RUN__AUTH__INTERNAL_SECRET`, declared nowhere and not in `auth_grants()`; under WRAP the read is denied → `""` → 403 unconditionally. No in-repo caller. | Declare, grant and use — or delete the endpoint and its two `routing.rs` entries. |
| B15 | `blocks/admin/settings.rs:44-51` | `block_settings::is_enabled` maps any read error to `true`; it is the input to `handle_toggle_feature`. | `Result<bool>`. |
| B16 | `blocks/llm/pages.rs:494-499` | Renders `hx-delete="/b/llm/api/config/{id}"`; only `GET`/`POST /b/llm/api/config` exist (`mod.rs:110-111, 452-457`). Dead button. | Add the route; render test that every `hx-*` URL matches a declared endpoint. |
| B17 | `.github/workflows/release.yml:43-73` | No wasm-pack step (`build.rs` exits 1 without `pkg/impresspress_web_bg.wasm`), no `--locked`; never run; `RELEASE.md` documents it as working. | Reuse CI's `build-wasm` via `workflow_call`. |
| B18 | `blocks/vector/pages.rs:613-628` | `embedding_block_for_model(_model_id)` picks the block by `cfg(target_arch)`; a native build without `block-fastembed` (the default) dispatches to an unregistered block. | Resolve from `ctx.registered_blocks()` by protocol, as `vector/service.rs:70-74` does. |
| B19 | `blocks/vector/ingestion.rs:19-29, 94-165` | `add_context` has a no-op twin under `cfg(not(feature = "llm"))`, but the real one only uses `call_block`. On every non-`llm` build `contextual: true` is silently ignored. | Delete both cfgs. |
| B20 | `blocks/llm/routes/chat.rs:44-51` + `messages/mod.rs:228` | `agent` role (the messages block's documented value) → `ChatRole::User`. | One `EntryRole` enum owned by messages. |
| B21 | `blocks/products/stripe.rs:1345-1359, 1384-1398` vs `stripe_client.rs:100-113` | `stripe_catalog_post/get` classify every ≥400 as `FailedPrecondition`; `request_json` treats 429/5xx as retryable. A Stripe 503 during catalog sync is persisted as a terminal sync error. | Route all calls through `StripeClient`. |
| B22 | `blocks/products/stripe.rs:985-992, 1313-1320` vs `stripe_provider.rs:270-276` | `SELLER_APPLICATION_FEE_BPS` silently becomes 0 on garbage in checkout and payment links, hard error in onboarding. | One `configured_seller_fee(ctx) -> Result<u16>`. |
| B23 | `blocks/products/stripe.rs:747-757` | `platform_country` defaults to `"US"` while the ConfigVar and onboarding default to `""`; produces US-only shipping without error. | One `platform_country(ctx) -> Result<Option<_>>`. |
| B24 | `blocks/admin/pages/mod.rs:41-61` | Admin sidebar/⌘K shows entries for unregistered blocks on CF and browser builds (no `retain_registered`). | Use `shell_page`. |
| B25 | `impresspress-cloudflare/src/database.rs:680-700` vs browser/native | CF does not re-parse JSON-looking TEXT; other platforms do. Block code sees a different shape per platform. **Committed.** | Upstream shared row codec. |
| B26 | `impresspress-browser/src/convert.rs:255-263`, `network.rs:32-66` | No `wants_streaming`/`resp.stream` handling (marked downloads fully buffered), no 413 cap, no SSRF gate (service worker can reach `localhost`), multi-value headers flattened (repeated `Set-Cookie` lost). | Depend on core; reuse `streaming::*` and `ssrf::*`; `httpFetch` returns `[[k,v]]`. |
| B27 | `.playwright-mcp/` + `regen-visual-baselines.yml:154` | `git add .playwright-mcp/` wholesale commits any MCP session dump present on the runner (97 already tracked). | Land `chore/baselines-out-of-mcp-scratch`; narrow the `git add`. |
| B28 | `.githooks/pre-commit:8, 23` | Stable `cargo fmt` vs nightly-only `rustfmt.toml` options (CI uses `cargo +nightly fmt`); prettier path to a nonexistent package. | Fix or delete the hook. |

---

## 4. Duplication: the clusters that matter

Each entry names the abstraction that removes it. "Upstream" means the fix belongs in wafer-run first.

**Within products**
- Checkout Session vs Payment Link form builders: `stripe.rs:822-873 ≡ 2083-2130`, `883-935 ≡ 2132-2181`; bps→percent formatter 3×. → `push_checkout_options`/`push_line_items` + `CheckoutFormContext` + `money::format_basis_points`.
- Webhook lease filters and state writes ×3-4 (`stripe.rs:188-205, 236-252, 311-327, 470-481`; column maps at 168-186, 224-233, 279-304, 482-499) and the same lease pattern in `repo/provider_operations.rs`; `EVENT_MAX_ATTEMPTS = 8` vs a literal 8 in `stripe_provider.rs:856`. → `repo/stripe_events.rs` + shared `repo::lease`.
- "Commerce first, then platform-subscription fallback" ×4 inside `handle_webhook`. → `resolve_subscription_owner`.
- CAS retry loop hand-rolled 6× (`subscriptions.rs:213-266, 287-340`, `purchases.rs:1246-1320`, `refunds.rs:299-390`, `seller_accounts.rs:287-449`, `provider_operations.rs:201-260`); the correct primitive is private at `offers.rs:575-602`. → promote `update_if_current` + `retry_cas` (or upstream `db::update_if_matches`).
- `refund_purchase` (458 lines) Stripe/manual branches (`purchase.rs:359-373 ≡ 473-489`, `397-422 ≡ 503-528`, `RefundClaim` literal 4×, tail 575-602 ≡ 698-725). → `resolve_claim` + `finalize_succeeded`.
- Checkout metadata decoded 3× (`purchases.rs:766-778, 945-957, 1053-1061`); reconcile validation 697-727 ≡ 923-942. → typed `CheckoutMetadata` + `verify_session_identity`.
- Repo micro-patterns: get-by-field→Option ×6, `Filter` literal 80+ with 5 local `eq` helpers, `list_for_purchase` (`disputes.rs:242-265 ≡ refunds.rs:460-483`), enum→wire-string ×4, `replace_for_offer` (`offer_components.rs:120-163 ≡ variables.rs:129-173`). → upstream `Filter::eq`, `db::find_by_field`; local `replace_children`.
- Redirect-origin policy ×3 (`stripe.rs:1040-1058, 2262-2278`, `stripe_provider.rs:243-259`), each with the literal `"http://localhost:5173"` (also in `email.rs:241`, `auth/mod.rs:646`, `oauth/callback.rs:151`; central default at `config_vars.rs:86-89`). → `checkout_origin_policy(ctx)`, `config_vars::frontend_url(ctx)`.
- Seller status ladder ×3; seller context ×2 + fee parse ×3; `pages.rs` queries ≡ `handlers/sellers.rs` ×3; `pages.rs:435-447 ≡ 3451-3463`.

**Within auth/auth_ui/userportal**
- Repo shapes: get-or-None 7-line match ×14, list-for-user-sorted ×5, `Filter` literal ×34. → upstream constructors; local `find_one`/`list_for_user_sorted` (~150 lines).
- auth_ui tails: `SiteConfig` built 3 ways; `html_respond` 44-line page ×2 (`verify.rs:146-189 ≡ pages/reset_password.rs:105-148`, bypassing `site_config` so favicon/scripts are absent); post-login redirect block ×4; login/signup response JSON and page-header blocks ×2. → `redirect::resolve_post_login`, `IssuedLogin::json_body`, `pages::status_page`.
- Roles raw-insert ×2 (`auth/mod.rs:404-424 ≡ oauth/callback.rs:458-468`); timestamp formatter ×6.
- Test fixtures: `seed_user` raw INSERT ×12, `ctx_with_crypto` ×7, `signup_user` ×3, `button_data` ×2, identical a11y tests (`pages/login.rs:219-266 ≡ signup.rs:106-153`), disable-via-raw-update ×4. This explains most of the mechanical scan's auth hits. → `TestContext::seed_user/with_crypto/signup_user`.

**Within admin/ui**
- Shell preamble ×7 (`SiteConfig::load` + `UserInfo::from_message` + hardcoded `current_path` + `Topbar` literal). → `shell_page`.
- Modal shell ×3 (`variables.rs`, `blocks.rs`) despite `components::modal`; eye-toggle JS ×3; modal auto-open ×3; `render_field` 7 near-identical arms; `write` column decoded 2 ways and accepted 3 ways.

**Shared infra**
- `crud.rs`: empty-id guard ×6, NotFound/Internal tail ×6, owner preamble ×3 (`crud_update_owned ≡ crud_delete_owned`); `crud::id_from_path` vs `util::path_param` (two readers of `req.param.id`, each with a "for tests" prefix-strip fallback in production). → one `record_id`, one `db_result`.
- `system.rs`: scan loop ×3, test ×4. Five private test `Context` stubs (`system.rs`, `rate_limit.rs`, `email.rs`, `admin/iam.rs`, `llm/routes/test_support.rs`) despite `test_support::TestContext`. Two identical 14-method `unreachable!()` `DatabaseService` stubs (`boot.rs:502-614 ≡ features.rs:1037-1146`; this was the scan's top hit). → `test_support::FailingDb`, `NopContext`.
- `messages/rest.rs` owner-check preamble ×4, `OwnedResource` literal ×3. → `crud::require_owned`.
- Files: user vs admin row structs ×2 per table; octet-stream fallback ×3; legalpages filter block ×5.
- `builder/prepared.rs:119-153 ≡ 250-284`; `PreparedRouteAccess`/`RouteAccess`/`AuthLevel` are three copies of one three-tier ladder plus a fourth `auth_rank` because `AuthLevel` lacks `Ord`.

**Cross-crate**
- Browser ↔ Cloudflare (~270 lines): `DatabaseService → DbExec` forwarding ×18 methods in each (and again in wafer-block-sqlite and -postgres, ~130 lines ×4); crypto JWT forwarder (`browser/crypto.rs:150-177 ≡ cloudflare/crypto_service.rs:75-102`); scalar/record decode ×3; declared-config resolution ×3 (`core/config_source.rs:34-67`, `cloudflare/config_source.rs:108-144`, upstream `StaticConfigSource`); `ModelCatalog` newtype ×2. → upstream `DbExec::after_mutation` + forwarder macro, `with_password_scheme`, `codec::record_from_json_row`, `resolve_declared`.
- Browser ↔ wafer-run (~350 lines): `browser/vector/sql.rs` + `service.rs` vs `wafer-sql-utils::vector` + `wafer-block-sqlite::vector`. → upstream `VecStorage::Blob`, `MetadataFilter::matches`.
- Browser `ensure_schema_table` ≡ wafer-block-sqlite's (`database.rs:314-359` vs upstream `service.rs:564-620`), already drifted on error handling. → upstream `DbExec` default.
- OpenAI wire codec: `llm/providers/openai.rs:203-276, 377-435` vs `impresspress-browser/src/llm/openai_codec.rs:28-100, 135-239`, and they disagree (tool-call completion trigger, unknown finish reason, tool-call keying). → one codec in wafer-core `interfaces::llm`.
- WRAP-grant loader ×2 (`boot.rs:295-391` vs `cli/server_config.rs:34-99`); post-build lifecycle ×3; config surfaces ×3; `BootHooks` republish ×2; `RuntimeReleaseManifest` (`cloudflare/lib.rs:280-300`) vs CLI `ReleaseManifest` (`assets.rs:37-48`); `RELEASES_ROOT` ×2; `PreparedRuntimeStructure` empty literal ×5 (type derives no `Default`); CLI wire-protocol twins of core's `DeployInitReport`.
- Two service workers (`packages/impresspress-web/src/worker.ts` vs `crates/impresspress-bundle/assets/sw.js.tmpl`) with different intercept policy, `skipWaiting` behaviour and recovery; the npm one is neither built nor tested in CI. Two CI workflows (~500 lines each, verbatim for eight jobs); native-server start script byte-identical in `ci.yml:437-467` and `regen-visual-baselines.yml:83-104`. SDK: `buildQueryString` ×2, request plumbing ×3 (`requestFormData`/`requestBlob` lack the timeout/abort/`detailCode` handling `HttpClient` has), each service constructs its own `HttpClient` so `client.ts` fans `setApiKey` over a hand-listed array twice. CLI: two MIME tables (disagree on `text/javascript`), two recursive-copy functions (one silently overwrites, one refuses), deploy HTTP client rebuilt 6×, `impresspress.toml` parsed by two schemas with a hand-written TOML-key↔env-var table (`env.rs:91-127`).

---

## 5. Per-area findings (condensed)

Line references are relative to `crates/impresspress-core/src/` unless the path starts with another crate.

### 5.1 Core platform (`pipeline`, `routing`, `features`, `boot`, `builder/`, …)
- ROUTES god-table and five matching mechanisms (T1). `ImpresspressBuilder::build` is 375 lines (`builder/registration.rs:34-408`); `handle_request` is 208 lines with 9 parameters and `#[allow(clippy::too_many_arguments)]` plus a comment deferring the refactor (`pipeline.rs:91-105`).
- Thread-local side channel between pipeline and the CF adapter (`pipeline.rs:41-68` `set_request_log_mode`/`drain_queued_request_logs`; "native never calls it"). → return the log row from `handle_request`.
- `BlockState` has two conflicting defaults (derive → `enabled: false`, serde → `true`), a `from_map` "legacy" shim whose two callers pass an empty map, and `from_config_json` `unwrap_or_default` that fails open to all-enabled on malformed snapshot JSON while `:436-443` argues the opposite (`features.rs:27-41, 70-87, 135-138`).
- `util.rs` (884 lines) mixes time, path params, JSON coercion, formatting, auth identity, URL encoding, the SSRF write-gate `validate_url_value` (`:279-345`), secret masking, and form parsing. → move policy to `ssrf.rs`/`config_vars.rs`/`auth_meta.rs`/`form.rs`.
- Shared config keys as string literals (`pipeline.rs:123, 140`, `routing.rs:413`); `WAFER_RUN_SHARED__DATABASE__BACKEND` (`migration_helper.rs:41`) is undeclared and its extra `__` makes `key_block_prefix` misparse it; `config_vars.rs:195` pulls auth's shared vars in despite `:53-54` saying blocks must not declare them.
- Refuted leads: the `boot.rs`/`features.rs` scan hit is two test-only DB stubs; all raw SQL in `features.rs` is test fixture setup; the four ">1000-line" files are mostly test weight (non-test bodies 367/499/551/854).

### 5.2 Products domain
- Four-way route declaration and path rewrite (T1); 48 drifted schema literals (B11). `mod.rs` is a 1,670-line `info` closure beside a 160-line hand router; `dispatch.rs` repeats 30 variant names and 23 match arms across `AdminRoute`/`UserRoute` (differing only in `OfferAccess`), plus two hand-maintained `requires_*` variant lists.
- `_cents`/`_minor` and duplicate columns (T5); `refund_purchase` 458 lines; CAS ×6; metadata decode ×3; swallowed errors (T4); stringly statuses (T5); migration boilerplate (T7); two decimal parsers; dead `repo/entitlements.rs` and `repo/product_versions.rs` (tables created by migration 005, never read or written; `fulfillment_kind = "entitlement"` selectable in the UI but grants nothing; `current_version` protected but never incremented); five boolean flag parameters where `OfferAccess`/an enum should be.
- Refuted: the 4 `exec_raw` calls in `migrations/mod.rs` are inside `#[cfg(test)]`.

### 5.3 Products Stripe and pages
- `stripe.rs` (5,312 lines) never uses `StripeClient` (0 uses vs 8 in `stripe_provider.rs`); re-implements config/transport/error classification at 6 sites with a divergent retry policy (B21); `"https://api.stripe.com"` literal ×6; `DEFAULT_STRIPE_API_VERSION` duplicated; circular dependency (`stripe_client.rs:47` → `stripe::is_stable_stripe_api_version`, `stripe_provider.rs:252` → `stripe::is_allowed_checkout_url`).
- `handle_webhook` 1,122 lines (`:2983-4104`), string-dispatched, with a `fail_webhook!` macro because the function cannot use `?`; six functions over 150 lines.
- Platform-specific addon/plan billing (`extra_projects`, `extra_r2_bytes`, `extra_d1_bytes`, `plan.unwrap_or("free")`) hardcoded inside the generic products block (`:4296-4347, 3250-3291, 3474-3510, 4109-4162`), with weaker durability than the commerce path. Violates "no hardcoded domain-specific values in blocks". → move out (consumer subscribes to the existing `products.*` outbound webhook) or drive from a `ConfigVar` list.
- `pages.rs` renders correctly (maud + shared components) but bypasses the repo layer at 14 sites and embeds ~480 lines of JS as strings.
- Proposed split of `stripe.rs`: `repo/stripe_events.rs`, `checkout.rs`, `catalog.rs`, `payment_links.rs`, `webhook/{checkout_session,per-family}.rs`, `platform_billing.rs` (or out), tests to `tests/`; version/URL policy into `stripe_client.rs`.

### 5.4 Auth, auth_ui, userportal
- B2, B12, B14; `USERS_TABLE` shim (T3); two credential mechanisms (T3); boolean flag truth tables (T5); 12 error-collapsing sites (T4); six timestamp formatters (T5); repo micro-patterns and fixture copies (section 4).
- `auth/mod.rs` (1,132 lines: 440 of inline tests plus auth-version cache, helpers, API-key auth, and a `brand_panel` UI concern imported by six pages); `oauth/callback.rs` (1,067 lines: 450 of tests; `is_safe_frontend_url` belongs beside `is_safe_local_redirect` in `redirect.rs`); `signup::handle` 204 lines; `generate_tokens` 119.
- Refuted: `rt.block_on` at `service.rs:499` is inside `#[cfg(test)]`; all raw SQL in the slice is test-only.

### 5.5 Admin block and shared UI
- B1, B15, B24; two component generations and `admin_page` (T8); four route tables with a stale advertised list (T1); JS/CSS built five ways (T8); `ui/assets.rs` hosting block assets behind block cfgs (T2); admin reading auth's schema (T2); WRAP-grant mutations bypassing the `ops` layer (`admin/mod.rs:291-366`); URL validation by `InputType` vs suffix (T5); three hand-rolled modals.
- God functions: `dashboard` 269 lines, `grants_custom_tab` 248, `blocks_page` 216, `handle_block_detail` 201 (hand-paints badge colours as inline hex), `render_field` 124 (seven arms differing only in the input element).
- Confirmed compliant: `admin/database.rs`'s `query_raw` calls execute `wafer_sql_utils::introspect`-built SQL or the user-typed explorer; `settings_form.rs` is `ConfigVar`-driven.

### 5.6 Shared block infrastructure and cross-block consistency
- Consistency matrix (14 blocks): 2 struct styles, 7 dispatch mechanisms, 4 config-declaration styles, 3 migration styles (7 blocks carry an identical 7-line `lifecycle_init` call that the macro doc claims is folded in), 6 error styles, 3 inline auth helpers disagreeing 401 vs 403 (`files/mod.rs:181-184` 401, `userportal/mod.rs:246-249` 403 "Not authenticated", auth_ui renders the login page), 3 rate-limiting conventions. Only `ok_json` is uniform.
- `rate_limit.rs` (870 lines): six clusters, two `cfg(target_arch)` bodies for `UserRateLimiter::check` (every other platform concern is injected), writes into auth's table, synthesizes `WAFER_RUN_SHARED__RATE_LIMIT_{NAME}` keys that no `ConfigVar` declares (invisible to the admin UI); email ignores it and reads its own `IMPRESSPRESS__EMAIL__RATE_LIMIT_*`.
- `email.rs` is block + service + HTTP client + template engine; declared `service@v1` but registered through the feature-block macro; templates hardcode other blocks' URLs, colours and brand strings; `send_email` returns `bool` (HTTP 200 `{sent:false}` when Mailgun is down).
- `storage.rs` wraps the *block* rather than the *service*: MessagePack-decodes and re-encodes six ops to namespace a path, recomputes `wrap_resource` to satisfy another crate's cross-check, stringly `"read"`/`"write"`, six `let _ = log_storage_access`. → `NamespacedStorageService` decorator with the stock wafer-core `StorageBlock`.
- `all_block_infos()` instantiates every block to read `info()` (`blocks/mod.rs:97`), forcing fastembed's `OnceLock` dance and llm's `NoopProviderAdmin`. → upstream: `Block::info` as an associated fn.

### 5.7 files, messages, legalpages
- B3, B4, B10, B13; three dispatch idioms and seven undeclared routes in files (T1); untyped rows (T3); no repo layer in legalpages (T3); `messages_schema.rs` (T2); owner-check preamble ×4; hand-typed endpoints page; stringly types (T5); `ALLOWED_QUOTA_FIELDS` and messages' field whitelist where `deny_unknown_fields` structs should be; files declares zero config vars yet hardcodes share TTL 30 d, max expiry 1 y, sweep 3600 s, `Cache-Control` 3600, SSR cap 1000; SSR error swallowing (T4); three HTML/JS conventions (legalpages `EDITOR_CSS`/`EDITOR_JS` string consts with hex colours and `NavKind::Portal` on admin-tier pages; files hashed assets + `NavKind::Admin`; messages inline `style=` + `onmouseover`).
- Messages is the cleanest block in the repo (one `ROUTES` table, service layer, `crud` helpers, ownership per endpoint).
- Refuted: `files/pages_user/objects.rs` scan hit is test fixtures; pulldown-cmark is used consistently.

### 5.8 llm, vector, embedding
- B8, B9, B16, B18, B19, B20; direct messages-table reads (T2); OpenAI codec duplicated and divergent (section 4); legacy provider sentinel (T5); persistence errors swallowed end-to-end (T4); `openai_compatible.rs:20-37` encodes with a `"placeholder"` key then strips the header; three copies of "prefer router var, fall back to splitting the path"; settings table with no row codec; `pages.rs` (JSON API) and `pages_ui.rs` (SSR) named backwards, with a 250-line integration-test module inside the UI file.
- Confirmed clean: the `ProviderAdmin` trait seam itself (27 cfgs in `llm/`: 16 test, 6 `feature = "llm"` confined to `providers/mod.rs`, 5 postgres, 0 `target_arch`); no SSE re-implementation; no hardcoded model lists in core; HTML via shared `ui`. The fastembed/transformers_embed wrappers are near-minimal (one generic `EmbeddingBlock` would save ~60 lines).

### 5.9 Browser vs Cloudflare adapters
- B6, B25, B26; trait-completeness table: all three concrete adapters implement every required `DatabaseService` op (the 07-16 finding is closed there); the KV wrapper leaks two defaults; browser inherits `set_strict_schema` (silently drops `STRICT_SCHEMA`), `put_streaming`/`get_streaming` (bridge takes `&[u8]`), and `do_request_streaming`. The compile-time conformance wiring cannot detect inherited defaults.
- `D1DatabaseService::create_many` (`database.rs:117-190`) re-implements `DbExec::create`'s row policy by copying private upstream helpers, reachable only via a concrete-type escape hatch that bypasses the KV decorator.
- `cloudflare/config_source.rs:87-89` string-matches `no such column: block` to tolerate a pre-migration-002 D1 (a config source that knows migration history).
- `kv_cached_db.rs:69` `getrandom(..).expect` on the config-version write path while `primitives::random_bytes` returns `Result`; two wasm32 thread-safety strategies in one crate (`unsafe impl Send/Sync` in `config_service.rs`/`logger_service.rs` vs `MaybeSend` + `allow(arc_with_non_send_sync)` elsewhere).
- No new `impresspress-wasm-common` crate is needed: `impresspress-core` with default features off already is the shared wasm layer; the browser crate never adopted it.

### 5.10 Cloudflare runtime, native, web, bundle
- B5; three post-build lifecycles, two config surfaces, `lib.rs` god module, twin cache builders, hand-listed environment identity, wasm32 middleware list, drifted feature forwarding (T6/T7); request-scoped vs kernel-safe by convention.
- Positive: the uncommitted diff removes the request-derived `batch_db`/`kv` handles the committed `ReadyRuntime` cached, moves block-settings seeding off the request path, and makes the prepared-plan prototype real. The stale-JWT defect from the 07-24 doc is half-fixed (JWT still in the cached snapshot by admitted exception, `lib.rs:1137-1140`).
- `impresspress-native` has no boot function; the whole native boot is `cli/server.rs:31-264` (234 lines). `impresspress-web` is a thin wrapper except for the dead CSP. `impresspress-bundle`'s 9 non-test unwraps are infallible map lookups; the swallowed `remove_file` results matter more.
- Refuted: error→HTTP mapping is shared via `wafer_block::http_codec` (only the catch-all policy differs: CF opaque 500 + correlation id; browser surfaces raw `JsValue` text).

### 5.11 CLI
- B7; seven blocking `std::process::Command` shell-outs inside `async` (`deploy.rs:72-76, 102-106, 165-175, 515-527, 654-705`) bypassing the crate's own async `cmd::run` (only 3 of 15 spawn sites use it; the crate's own comments at `build.rs:83-87` state the rule); per-asset `r2 object put` + `get` are two blocking round-trips per file; deploy is invisible to the dry-run goldens.
- `embed_cloudflare::deploy` is a 175-line script hand-threading a runtime `TwoStageDeploymentGate` for a statically fixed order (→ typestate); `server::run` 234 lines; `wrangler::generate_named` 8 parameters, five wrappers (two test-only), `[vars]` lookup ×3, `.expect("base wrangler config is a table")` ×5.
- `impresspress.toml` parsed by two schemas with different discovery rules and a hand-written TOML-key↔env-var `pick` table; `env.rs:82` points to a nonexistent spec path. Wire contract with the Worker re-declared as literals and `Deserialize` twins (T7). Build layout as relative-path literals in three files (`'../../../build/worker/shim.mjs'` one level deeper than its sibling by coincidence of two constants). `release` flag accepted and ignored on three build paths (the uncommitted diff dropped `--release` from the worker build). Native wiring copied wholesale into `tests/deploy_init.rs:45-122` ("mirroring server::run's steps 5-11"), so the test exercises a runtime the binary does not build.
- Refuted: "146 unwraps" — 137 are in tests, 9 in production are infallible; nothing in `deploy.rs` exceeds 77 lines; `server.rs` does not re-implement native boot.

### 5.12 TypeScript packages, tests, CI, hygiene
- B17, B27, B28; SDK types describe the Go schema (`types/generated/database.ts:1-2` "the solobase-era Go generator no longer exists"; `AuthUser` has `username/firstName/lastName/phone`, the table has `email/display_name/avatar_url/role`; `IAMService.getRoles(): IAMRole[]` vs the server's `RecordList`; `Extension` fields vs `admin/mod.rs:160-166`; README says types are "generated from the backend models"; `types-compatibility.test.ts` pins stale against stale). The 07-17 SDK PR fixed routes, not types.
- Two service workers (section 4); `packages/impresspress-web` has vitest and a test file but no `test` script and zero CI references; `dist/` is ignored so the published package is unreproducible.
- Three Rust test conventions: products' in-tree `src/blocks/products/tests/` (12,601 lines) on `TestContext`; `crates/impresspress-core/tests/` where `auth/common.rs:18-66` still hand-rolls `MigrationTestCtx` although `Cargo.toml:34-36` says `test-support` exists to prevent exactly that (2 of 23 files use `TestContext`); 168 inline `#[cfg(test)]` modules, seven with their own `Context` fakes. `test_support.rs` is 1,176 lines.
- SDK: `ImpresspressError` promised, bare `Error` thrown at 9 sites (`popup-auth-session.ts` has no machine-readable cancel/timeout/blocked codes); 8 hand-written `any`s.
- CI: `sdk` job clones wafer-run for a removed dependency (`ci.yml:655-664`, `ci-main.yml:343-350`); regen uses `npm install` where ci uses `npm ci`.
- Docs: `CODE_REVIEW_2026-06-05_findings.json` (469 KB), a HANDOFF that says its artifacts are untracked (they are tracked) and records two Claude processes racing on a checkout, `documentation_review_report.md` linking `file:///home/joris/...` and four `docs/superpowers/*` paths that do not exist, the 07-16 findings citing deleted files with no "resolved" banner, and an untracked 72 KB 07-24 file in `git status`.

---

## 6. Upstream (wafer-run) work this review implies

Per the workspace rule, missing builders go upstream first, then consumers. In rough priority:

1. `DbExec::after_mutation` hook + `forward_database_service!` macro (deletes ~500 lines across four backends; forces decorators such as the KV wrapper to enumerate every op; closes B6 structurally).
2. `db::find_by_field(..) -> Result<Option<Record>>` and `Filter::eq/in/is_null/lt/gte` constructors (removes ~20 hand-rolled get-or-None blocks and 100+ `Filter` literals across products and auth).
3. `db::update_if_matches` CAS primitive (six hand-rolled loops in products).
4. `interfaces::database::codec::{record_from_json_row, scalar_i64, scalar_f64}` (B25).
5. `DbExec` default for `ensure_schema_table` with fail-loud semantics; `DbExec::create_many` over `run_batch`.
6. `config_source::resolve_declared(block, declared, lookup)`; `config_client::get_bool(ctx, key, default)`.
7. `VectorIndexSchema` `VecStorage::{Vec0, Blob}` and `MetadataFilter::matches`.
8. `Argon2JwtCryptoService::with_password_scheme(..)` + `set_jwt_secret`; PBKDF2 into `primitives`.
9. Derive `PartialOrd, Ord, Serialize, Deserialize` on `AuthLevel` and `ResourceType` (deletes three mirrored enums and `auth_rank`).
10. `Block::info` as an associated fn (so `all_block_infos` never constructs blocks).
11. `use_static_blocks!` emitting explicit registration on wasm32 (deletes the hand-synced middleware list).
12. One OpenAI chunk/body codec in `interfaces::llm` (deletes the divergent browser copy).
13. Endpoint template syntax with in-segment params (`app-{hash}.css`) and literal-beats-param precedence.

---

## 7. Recommended order

**Phase 0 — bugs, each a small PR (days).** B1, B2, B3, B4, B5, B7, B8, B15 first (security guards and a real Postgres failure); then B6, B10, B11, B14, B16, B17, B18, B19, B20, B21-23, B24, B28. Land `chore/baselines-out-of-mcp-scratch` (B27) and prune `.claude/worktrees` in the same sweep as the hygiene items in 5.12.

**Phase 1 — one route table per block (T1).** Start with products (largest payoff: deletes the path rewrite, two enums, 23 duplicate arms, both `requires_*` lists, every test-only prefix fallback, and the drifted schemas once schemas derive from `contracts.rs`), then files and admin. Then derive `routing.rs` from `BlockInfo`, delete `router_declared_public`, and replace the feature-block macro with an explicit trait. Declare auth-ui's ten undeclared endpoints.

**Phase 2 — ownership and repo boundaries (T2, T3).** Core `platform_state` module; delete `admin_schema.rs`/`messages_schema.rs`; llm reads messages via `call_block`; add the missing `auth::repo::users` functions and delete `USERS_TABLE`; typed rows for files and legalpages; `repo/documents.rs` for legalpages; products `repo::seller_accounts::list_contracts`. Decide the auth credential model and the session-row model (B12).

**Phase 3 — enums and error discipline (T4, T5).** Status/role/kind enums in `contracts.rs` and the three content blocks; `From<ErrorCode> for OutputStream` + `crud::db_error` + `http::require_row`; `_cents` → `_minor` migration; `money::Decimal`; `config_client::get_bool`.

**Phase 4 — runtime lifecycle and adapters (T6), with the upstream list in section 6 landed producer-first.** `builder::boot` with `InitPolicy`; builder owns both config surfaces; browser depends on core; `CfEnvironment`; split `cloudflare/lib.rs`; `stripe.rs` through `StripeClient` and split; async `Wrangler` layer + typestate deploy in the CLI.

**Phase 5 — UI (T8).** Delete gen-1 components; admin onto `shell_page`/`data_table`/`button`; one hashed `admin.js` with `data-action` delegation; block-owned static assets; products' wizard JS to a file.

**Phase 6 — CI and packages.** Reusable `workflow_call` workflows; fix `release.yml`; delete the wafer-client-js steps; one service worker; regenerate SDK types from `/openapi.json` with a CI freshness check; one `HttpClient`; single Rust test convention with fault injection folded into `TestContext`.

---

## 7a. Phase 0 and 1 status

**Phase 1 (2026-09-05 to 2026-09-06).** One route table per block (T1), designed in `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md` and landed as seven PRs on the fork, each with a plan under `docs/superpowers/plans/2026-09-05-route-table-*.md`, both per-block snapshot gates (`*.openapi.json`, `*.endpoints.json`) byte-identical except where the PR body lists the reviewed lines:

| PR | Step | Branch |
|---|---|---|
| #12 | 1 core + llm + system: `EndpointRoute` carries the declaration, `declare`, the endpoint-surface snapshot; system's one `{filename}` asset row | `phase1/route-table-core` |
| #13 | 2 messages, vector, legalpages, tickets, dev | `phase1/route-table-flat-blocks` |
| #14 | 3 userportal + auth-ui: `normalize_template` deleted, auth-ui's eleven undeclared paths declared, rate limits keyed on the variant | `phase1/route-table-userportal-auth-ui` |
| #15 | 4 files: admin storage APIs under the files prefixes, admin delegation deleted, `{key...}` and `{name...}/` | `phase1/route-table-files` |
| #16 | 5 admin: `route.rs` and `api_norm` replaced by the table, 37 served-but-undeclared rows declared, the API-key revoke control fixed | `phase1/route-table-admin` |
| #17 | 6 products: two-hop dispatch merged into one table over wire paths, gates on the variant, alias spellings narrowed; `crud_delete*`, `path_param`, `check_route_limits` deleted | `phase1/route-table-products` |
| #18 | 7 router cleanup: `router_final` and the carve-outs deleted, one prefix per block, plan schema 2, `EndpointRoute::new` and `dispatch_path` deleted | `phase1/route-table-router-cleanup` |

Left for later phases from this work: the admin users page's revoke response is JSON that htmx swaps into the tab (phase 5); `tickets::ENDPOINT_REFERENCE` and legalpages' `:id` HTML text are hand-spelled second listings of paths (phase 5); auth-ui's `POST /b/auth/api/oauth/sync-user` reads an undeclared, ungranted `INTERNAL_SECRET` (B14, phase 2/3); products' hand-written JSON schemas should derive from `contracts.rs` (B11, its own PR); wafer-core's OpenAPI projection should render `{key...}` as `{key}` (section 6).

**Phase 0 (2026-09-05, same day).** Eleven Phase 0 fixes shipped as independent PRs on the fork `Jsuppers/impresspress`, each branched from `main` with a test that fails before the change:

| PR | Finding | Branch |
|---|---|---|
| #1 | B2 refresh reuse detection fails closed | `fix/refresh-reuse-fail-closed` |
| #2 | B3 file quota and share ownership fail closed | `fix/files-quota-fail-closed` |
| #3 | B4 object and bucket delete report cleanup failures | `fix/files-delete-cleanup` |
| #4 | B7 native boot reads WRAP grants through the database service (also removes the CLI's copy of the boot sequence, 5.11) | `fix/cli-wrap-grants-db-service` |
| #5 | B15 block enabled flag fails closed | `fix/admin-block-enabled-fail-closed` |
| #6 | B24 admin pages use the shared shell, nav hides unregistered blocks | `fix/admin-shell-filters-nav` |
| #7 | B8 provider LLM client never built without its SSRF redirect policy | `fix/llm-provider-service-no-silent-fallback` |
| #8 | B16 `DELETE /b/llm/api/config/{id}` route | `fix/llm-config-delete-route` |
| #9 | B6 KV-cached D1 wrapper overrides `take_where` and `delete_where_count` | `fix/cf-kv-wrapper-bulk-ops` |
| #10 | B17 release workflow builds the wasm first and uses `--locked` | `fix/release-workflow-builds-wasm` |
| #11 | B28 pre-commit hook mirrors CI; `.claude/worktrees/` ignored (5.12) | `fix/repo-hygiene-hooks-gitignore` |

B1, B5 and B27 were already fixed on `main` before this work started (B27 by #93 on 2026-09-04). Still open from section 3: B9 to B14, B18 to B23, B25 and B26; these belong with the phases they fall under. The fork had no GitHub Actions runs at the time of writing; workflows need enabling once in the fork's Actions tab before these PRs get CI.

## 8. Leads from the mechanical scan that turned out to be test-only or compliant

Listed so nobody re-chases them: `boot.rs` ↔ `features.rs` (two test DB stubs); all `exec_raw`/`query_raw` in `features.rs`, `products/migrations/mod.rs`, `auth/repo/*`, `vector/pages_ui.rs` (`#[cfg(test)]` fixtures); `admin/database.rs` (introspect-built SQL + user-typed explorer); `files/pages_user/objects.rs:336-348`, `ui/shell.rs:167-182`, `ui/sidebar.rs:273-288`, `admin/mod.rs` ↔ `routing.rs` (test-vs-test); `rt.block_on` in `auth/service.rs:499` (test); "146 unwraps" in the CLI (137 in tests, 9 infallible); "`deploy.rs` giant functions" (max 77 lines; the scripts are `embed_cloudflare::deploy` and `server::run`); `cfg` soup in `llm/` (none on code paths); streaming re-implementation in llm (none); error→HTTP mapping duplicated across targets (shared via `http_codec`); `impresspress-native` boot duplicated by the CLI (not duplicated; native has no boot fn); migration `cfg(feature = "postgres")` density (a deliberate, documented wasm-size decision, though the two blocks' cfgs already differ).

---

## 9. Metrics

Longest functions found (non-test): `handle_webhook` 1,122 (`products/stripe.rs:2983-4104`); products `info` closure ~1,670 (`mod.rs:301-1971`); `refund_purchase` 458; `ImpresspressBuilder::build` 375; `handle_offer_checkout` 347; `reconcile_payment_link_session` 336; `sync_offer_catalog_inner` 304; `dashboard` 269; `grants_custom_tab` 248; `cli::server::run` 234; `build_runtime` 233 (uncommitted); `commerce_analytics` 225; `signup::handle` 204; `handle_request` 208.

Counts: 7 dispatch mechanisms across 14 blocks; 6 error-handling styles; 5 route-matching layers per request; 10 `router_declared_public` uses; 119 products endpoints declared vs 95 dispatch rows vs 48 schema literals; ~270 duplicated lines browser↔Cloudflare, ~350 browser↔wafer-run; 26 registered worktrees; 169 tracked files under `.playwright-mcp/` (72 baselines + 97 MCP dumps); 5 CI workflows, two of them ~500-line hand copies.
