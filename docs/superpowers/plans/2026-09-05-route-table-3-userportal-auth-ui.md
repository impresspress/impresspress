# Route table single source, PR 3: userportal + auth-ui

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `userportal` and `auth-ui` so each block's `const ROUTES: &[EndpointRoute<Route>]` over wire paths is the one description of its HTTP surface: it dispatches requests **and** generates `info().endpoints` through `endpoint_match::declare`. Delete the matcher's colon-style `normalize_template` shim once the last `:name` template (`userportal`'s `/sessions/:hash`) is rewritten to `{hash}`. Declare auth-ui's eleven served-but-undeclared method-and-path pairs at the level each handler already enforces, remove auth-ui's `/b`-stripping so every arm compares the wire path, and key its rate limits on the matched `Route` variant so the block has exactly one path matcher. Prove with a routing test that every auth path the router carves out today resolves to `Public` from the block's declaration alone, so PR 7 can delete the carve-outs.

**Architecture:** PR 1 made `EndpointRoute<H>` carry the auth level, summary, description, schema producers, tags, deprecation and agent-tool a `BlockEndpoint` carries, with `public` / `authenticated` / `admin` constructors and `const fn` builders, `declare(&ROUTES) -> Vec<BlockEndpoint>`, and `request_schema_of::<T>` / `response_schema_of::<T>`. `userportal` today has no table at all: a hand-rolled `match (action, sub)` over a `/b/userportal`-stripped path plus a second `handle_admin` matcher; it becomes one 14-row table. `auth-ui` today declares 18 pairs and serves 29 through a `match (action, path)` over a `/b`-stripped path, and its `RATE_LIMIT_ROUTES` matches the stripped path a second time; it becomes one 29-row table over wire paths plus a `const fn rate_limit_for(Route) -> Option<(LimitKey, &str, RateLimit)>` applied after `dispatch`. `endpoint_auth_exact` then matches `ep.path` directly.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e` (`BlockEndpoint`, `AuthLevel`, `HttpMethod`), `schemars` 1, `serde_json`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md` (this plan implements its "PR 3": section 3's **auth-ui** paragraph, section 2's last paragraph on `normalize_template`, sequencing item 3). Models: `docs/superpowers/plans/2026-09-05-route-table-1-core-llm-system.md`, `docs/superpowers/plans/2026-09-05-route-table-2-flat-blocks.md`.

## Global Constraints

- No change to wafer-run, `blocks/rate_limit.rs`, or any block other than `userportal` and `auth_ui`. `routing.rs` gains a test only; its `ROUTES` table and the eight `router_declared_public("/b/auth/...")` carve-outs stay (PR 7 deletes `router_final` and every carve-out together). `EndpointRoute::new` stays for products, files and admin.
- `endpoint_match.rs` changes only by deleting `normalize_template` and its test `normalize_colon_style`, and by `endpoint_auth_exact` matching `ep.path` directly.
- Every `crates/impresspress-core/tests/snapshots/<block>.openapi.json` is byte-identical at the end of every task. `UPDATE_OPENAPI_SNAPSHOTS=1` is never run against the `openapi_snapshot` test. `userportal` has no OpenAPI snapshot (it declares no schema); `auth_ui.openapi.json` stays byte-identical because none of the eleven new rows carries a schema.
- `*.endpoints.json` is byte-identical for every block except the two deliberate diffs, each reviewed line by line and listed in the PR body: `userportal.endpoints.json` changes one line, `DELETE /b/userportal/sessions/:hash authenticated` becomes `DELETE /b/userportal/sessions/{hash} authenticated` (same method, same level, the colon-to-brace rewrite the spec requires); `auth_ui.endpoints.json` gains exactly the eleven lines named in Task 2, each at the level the handler already enforces (nine `public`, two `authenticated`).
- Every existing row names its auth level through `EndpointRoute::public`, `::authenticated` or `::admin`, exactly as the old `info()` list declared it (unmarked = the upstream default `Public`). Metadata is copied verbatim (`.input::<T>()` becomes `.input(request_schema_of::<T>)`, `.output::<T>()` becomes `.output(response_schema_of::<T>)`). The eleven new auth-ui rows get a summary and nothing else.
- Migrated handlers read path variables only through `msg.var(..)` after `endpoint_match::dispatch` bound them. Nothing else in either block reads a path. After each block: `grep -rn 'path_param(\|strip_prefix("/b\|starts_with("/b\|dispatch_path(\|normalize_template' crates/impresspress-core/src/blocks/<block> crates/impresspress-core/src/endpoint_match.rs` prints nothing outside test-only string assertions.
- Every existing rate-limit assignment is preserved exactly: which routes are IP-keyed vs user-keyed, which category and which `RateLimit` each uses. The existing rate-limit tests keep passing, adapted to call `rate_limit_for`.
- TDD: write the test, run it and see it fail for the expected reason, then implement. Each task ends with a commit carrying the two trailer lines:
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Format with `cargo +nightly fmt --all`. Lint with `cargo clippy -p impresspress-core --all-targets -- -D warnings`. `cargo test -p impresspress-core --no-fail-fast` has one known unrelated failure, `lockfile_loads_remote_block`; every other test must pass.
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase1/route-table-userportal-auth-ui` (created from `origin/main` at `018adc52`, the merge of PR 2). The session's shell guard refuses compound commands containing `git` or shell variables; those go in a script under the scratchpad directory and run with `bash <script>`.

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `crates/impresspress-core/src/blocks/userportal/mod.rs` | `Route` enum, 14-row `ROUTES` over `/b/userportal/...` (colon template rewritten to `{hash}`), `info()` calls `declare(ROUTES)`, `handle` dispatches through the matcher; `handle_admin`, the `/b/userportal` strip, the non-`/b/userportal` `handle_config` fallback and the caller-less `/internal/list-buttons` action are gone; `table_tests` and a `test_support::routed`. |
| `crates/impresspress-core/src/blocks/userportal/pages/sessions.rs` | `handle_revoke(ctx, msg)` reads `msg.var("hash")`; its tests route their messages through the table. |
| `crates/impresspress-core/src/blocks/userportal/pages/admin_buttons.rs` | Unchanged (handlers already take `id: &str`); one doc comment corrected. |
| `crates/impresspress-core/src/endpoint_match.rs` | `normalize_template` and `normalize_colon_style` deleted; `endpoint_auth_exact` matches `ep.path` directly. |
| `crates/impresspress-core/src/blocks/auth_ui/mod.rs` | `Route` enum, 29-row `ROUTES` over `/b/auth/...`, `info()` calls `declare(ROUTES)`, `handle` dispatches through the matcher then applies `rate_limit_for(route)`; `RATE_LIMIT_ROUTES`, the `/b` strip and the `match_template` guard arms are gone; rate-limit tests adapted, `table_tests`. |
| `crates/impresspress-core/src/blocks/auth_ui/api/api_keys.rs` | `handle_revoke` / `handle_delete` read `msg.var("id")` instead of `path.rsplit_once('/')`; new tests that an unrouted message is refused and a routed one reaches the row. |
| `crates/impresspress-core/src/routing.rs` | New test: every auth path carved out by `router_declared_public` resolves to `Public` through `AuthUiBlock::new().info()` alone, and the two api-key rows to `Authenticated`. |
| `crates/impresspress-core/tests/snapshots/userportal.endpoints.json` | One line changes (`:hash` to `{hash}`). |
| `crates/impresspress-core/tests/snapshots/auth_ui.endpoints.json` | Eleven lines added. |
| `docs/superpowers/plans/2026-09-05-route-table-3-userportal-auth-ui.md` | This plan. |

---

### Task 0: Commit this plan

- [ ] **Step 1: Commit**

```
docs: plan for phase 1 PR 3 (userportal, auth-ui)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 1: `userportal` (14 rows) and the `normalize_template` deletion

**Files:**
- Modify: `crates/impresspress-core/src/blocks/userportal/mod.rs`
- Modify: `crates/impresspress-core/src/blocks/userportal/pages/sessions.rs`
- Modify: `crates/impresspress-core/src/blocks/userportal/pages/admin_buttons.rs` (doc comment)
- Modify: `crates/impresspress-core/src/endpoint_match.rs`
- Regenerate: `crates/impresspress-core/tests/snapshots/userportal.endpoints.json`

**Old surface (from `info()`), which the rows reproduce exactly:** six `Authenticated` user pages/actions (`GET /b/userportal/`, `GET /profile`, `POST /update-profile`, `GET /sessions`, `DELETE /sessions/:hash`, `GET /security`); one unmarked, so `Public`, `GET /config`; seven `Admin` (`GET`/`POST /admin/settings`, `GET`/`POST /admin/buttons`, `GET /admin/buttons/{id}/edit`, `PATCH`/`DELETE /admin/buttons/{id}`). Summaries only, no schemas.

**Old dispatch, every arm of which resolves to a row:** `handle()` strips `/b/userportal`, routes `/admin/*` to `handle_admin` (six arms, three of them `strip_prefix("/admin/buttons/")` id readers), and matches the rest by hand. `("retrieve", "" | "/")` is the `GET /b/userportal/` row (the matcher's bare-path retry covers `/b/userportal`). Two arms do not resolve to a row and are deleted: the `!path.starts_with("/b/userportal") => handle_config` fallback (only reachable through `ctx.call_block` with a foreign path; no caller exists anywhere in the repo) and `("retrieve", "/internal/list-buttons")` (documented as an inter-block action for the auth block's dashboard; that caller no longer exists, the only references in the repo are its own two tests, and unlike `llm`'s internal target it had no `caller_id` guard, so it was in fact an undeclared HTTP path served to any authenticated user). Its `handle_list_buttons` and `cross_block_tests` go with it.

- [ ] **Step 1 (RED): table test.** Add `mod table_tests` to `mod.rs` with `info_endpoints_come_from_the_table` (length equal, and per `zip` pair method, path and auth equal). Run `cargo test -p impresspress-core --lib blocks::userportal::table_tests`. Expected: FAIL to compile, `cannot find value ROUTES` (the block has no table yet).
- [ ] **Step 2 (RED): the revoke handler must not parse the path.** In `sessions.rs` tests add `revoke_reads_only_the_bound_hash`: `handle_revoke(&ctx, &msg)` on an unrouted `auth_msg("delete", "/b/userportal/sessions/<64 hex>", "user-a")` answers 400 (nothing bound), while the same message through a new `super::super::test_support::routed(..)` deletes the session and answers 200. Expected: FAIL to compile (`handle_revoke` still takes the `sub` argument; `routed` does not exist).
- [ ] **Step 3 (GREEN): the table.** In `mod.rs` add `enum Route` (14 variants) and `const ROUTES` with the metadata above copied verbatim and `/b/userportal/sessions/{hash}`; `.endpoints(endpoint_match::declare(ROUTES))`; `handle` becomes `let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else { return err_not_found("not found") }` plus one `match route` arm per variant; delete `handle_admin`, `handle_list_buttons`, `cross_block_tests`, the `BlockEndpoint` import and the inner `AuthLevel` import. Admin button arms pass `msg.var("id")`. Add `mod test_support` with `routed(msg)` (dispatches through `ROUTES`, panics if no row matches). In `sessions.rs`, `handle_revoke(ctx, msg)` reads `msg.var("hash")`; its five existing revoke tests wrap their messages in `routed(..)`. Correct the comment in `admin_buttons.rs` that describes how the id reaches `handle_edit_button_form`. Run `cargo test -p impresspress-core --lib blocks::userportal`. Expected: PASS.
- [ ] **Step 4: snapshot gate.** `cargo test -p impresspress-core --test openapi_snapshot --test endpoint_surface`. Expected: `openapi_snapshot` PASS; `endpoint_surface` FAIL on `userportal` only. Regenerate with `env UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test endpoint_surface`, then `git diff -- crates/impresspress-core/tests/snapshots/` must show exactly one changed line in `userportal.endpoints.json` (`:hash` to `{hash}`) and nothing else. Run both tests again: PASS.
- [ ] **Step 5 (RED): the shim is gone.** Grep the crate for the last colon-style template: `grep -rn '/:[a-z_]*' crates/impresspress-core/src --include='*.rs'`. Expected after Step 3: only `endpoint_match.rs`'s own `normalize_colon_style` test and three display strings in `legalpages/pages.rs` (a reference table's markup, not templates). Delete `normalize_template` and `normalize_colon_style`; make `endpoint_auth_exact` call `match_template(&ep.path, path)`. Run `cargo test -p impresspress-core --lib endpoint_match`. Expected: PASS, and `grep -n normalize_template crates/impresspress-core/src/endpoint_match.rs` prints nothing.
- [ ] **Step 6: gates, format, lint, commit.** Grep gate for `blocks/userportal` and `endpoint_match.rs` prints nothing. `cargo +nightly fmt --all`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`; both snapshot tests again. Commit:

```
refactor(userportal): declare the HTTP surface from the route table

`ROUTES` (14 rows over wire paths) now carries the summaries and auth
levels `info()` listed by hand, and `info()` is `declare(ROUTES)`. The
hand-rolled `match (action, sub)` over a `/b/userportal`-stripped path
and the `handle_admin` sub-matcher go; `handle` dispatches through
`endpoint_match::dispatch` and the handlers read `{id}` / `{hash}` only
as the table bound them. `/sessions/:hash` is written `{hash}`, which is
the one surface-snapshot line that changes, and with the last colon-style
template gone `endpoint_match::normalize_template` is deleted and
`endpoint_auth_exact` matches the declared path directly.

The caller-less `/internal/list-buttons` action and the non-`/b/userportal`
`handle_config` fallback are removed with the matcher they lived in: no
`call_block` into this block exists, so the only thing either did was
serve an undeclared path to any authenticated HTTP caller.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 2: `auth-ui` (29 rows) with rate limits keyed on the variant

**Files:**
- Modify: `crates/impresspress-core/src/blocks/auth_ui/mod.rs`
- Modify: `crates/impresspress-core/src/blocks/auth_ui/api/api_keys.rs`
- Regenerate: `crates/impresspress-core/tests/snapshots/auth_ui.endpoints.json`

**Old declared surface (18 rows), reproduced exactly:** `GET`/`POST /b/auth/admin/settings` `Admin`; `GET /login`, `GET /signup`, `GET /oauth/login`, `GET /bootstrap`, `POST /api/bootstrap` unmarked so `Public`; `GET /change-password`, `GET /orgs` `Authenticated`; `POST /api/login` and `POST /api/signup` `Public` with `.input`/`.output` contracts and tag `auth`; `POST /api/logout`, `GET /api/me`, `PATCH /api/me` `Authenticated` with contracts and tag `auth`; `POST /api/refresh` `Public` with contracts and tag `auth`; `POST /api/change-password`, `GET /api/api-keys`, `POST /api/api-keys` `Authenticated`.

**The eleven served-but-undeclared pairs, each verified against its handler before the row is written.** Nine `public`; the router already admits each anonymously through a `router_declared_public` carve-out (`routing.rs:309-316`), so the row changes no effective access:

| Row | Handler gate |
|---|---|
| `GET /b/auth/reset-password` | `pages/reset_password.rs:30-31`: no `token` query parameter renders the "Invalid Link" page, never the form. The page verifies nothing else; the reset itself is `POST /api/reset-password` below. Logged-out by definition. |
| `GET /b/auth/oauth/callback` | `oauth/callback.rs:42-44`: `oauth_pkce::take(state)` is the single-use PKCE state check; an unknown or expired state is 400. |
| `GET /b/auth/api/verify`, `POST /b/auth/api/verify` | `api/verify.rs:60`: `find_by_verification_token(sha256(token))`; no match renders "Invalid Link". |
| `POST /b/auth/api/resend-verification` | `api/verify.rs:113-115`: takes an email, answers the same message whether or not it exists, and only emails that address's owner. Issues a token rather than consuming one; IP rate-limited (`auth`). |
| `POST /b/auth/api/forgot-password` | `api/forgot_password.rs:24-26`: same shape as resend-verification; the raw token goes only to the email owner, the row stores its hash. IP rate-limited (`auth`). |
| `POST /b/auth/api/reset-password` | `api/reset_password.rs:38-61`: `find_by_reset_token(sha256(token))` plus expiry; both failures are typed errors. |
| `GET /b/auth/api/oauth/providers` | `oauth/providers.rs:14-17`: returns only which providers have a client id configured, the same fact the login page renders as buttons for anonymous visitors. No gate because there is nothing to gate. |
| `POST /b/auth/api/oauth/sync-user` | `api/sync_user.rs:13-22`: refuses unless `WAFER_RUN__AUTH__INTERNAL_SECRET` is configured and `x-internal-secret` matches under `constant_time_eq`. |

Two `authenticated`; today the router's fail-closed default for an undeclared path under the public `/b/auth/` prefix is what gates them, and the row states that level explicitly:

| Row | Handler gate |
|---|---|
| `PATCH /b/auth/api/api-keys/{id}` | `api/api_keys.rs` `handle_revoke`: `key.user_id != user_id && !is_admin(msg)` is forbidden; an empty (anonymous) user id never equals an owner. |
| `DELETE /b/auth/api/api-keys/{id}` | `api/api_keys.rs` `handle_delete`: same check. |

**Rate limits.** `RATE_LIMIT_ROUTES` (five rules over the stripped path) becomes `const fn rate_limit_for(route: Route) -> Option<(LimitKey, &'static str, RateLimit)>`, applied after `dispatch` has chosen the variant. The assignment is preserved exactly: `Login | Signup | Bootstrap` and `ForgotPassword | ResetPassword | ResendVerification | Verify` are `(Ip, "auth", AUTH)`; `Refresh` is `(Ip, "refresh", REFRESH)`; `Me | ListApiKeys` are `(User, "auth_read", API_READ)`; `UpdateMe | RevokeApiKey | DeleteApiKey | ChangePassword | CreateApiKey` are `(User, "auth_write", API_WRITE)`; every other row is `None`. The identity a key resolves to (`ip_identity` for `Ip`; `msg.user_id()` for `User`, skipped when empty) and the `check_rate_limit` call stay as `check_route_limits` did them. One consequence is recorded rather than preserved: the old last rule matched *any* `update`/`delete` action, so an unrouted `PATCH /b/auth/nothing` spent the caller's `auth_write` budget before its 404; a request no row matches is now a 404 and spends nothing.

- [ ] **Step 1 (RED): table test.** Add `mod table_tests` with `info_endpoints_come_from_the_table`. Run `cargo test -p impresspress-core --lib blocks::auth_ui::table_tests`. Expected: FAIL to compile, `cannot find value ROUTES`.
- [ ] **Step 2 (RED): the api-key handlers must not parse the path.** In `api_keys.rs` add `mod tests` with `revoke_reads_only_the_bound_id` and `delete_reads_only_the_bound_id`: against `TestContext::with_auth()` with one inserted key, the handler on an unrouted `auth_msg("update"|"delete", "/b/auth/api/api-keys/<id>", owner)` answers `InvalidArgument` ("Missing key ID"); the same message through `super::super::test_support::routed(..)` revokes/deletes the key. Run `cargo test -p impresspress-core --lib blocks::auth_ui::api::api_keys`. Expected: FAIL, the unrouted call succeeds because `rsplit_once('/')` still recovers the id from the path (or fails to compile on the missing `routed`).
- [ ] **Step 3 (GREEN): the table, the dispatch, the rate-limit function.** In `mod.rs`: `enum Route` (28 variants; `Verify` serves both the `GET` and `POST` rows), `const ROUTES` with the 18 old rows' metadata verbatim and the eleven new rows with a summary each, `.endpoints(endpoint_match::declare(ROUTES))`, `rate_limit_for`, an `apply_rate_limit(limiter, ctx, msg, route) -> Option<OutputStream>` that resolves the identity and calls `check_rate_limit`, and `handle` = dispatch, rate limit, `match route`. Delete `RATE_LIMIT_ROUTES`, the `/b` strip, the `match_template` guard arms, and the `AuthLevel` / `BlockEndpoint` / `RouteLimit` / `check_route_limits` imports. Add `mod test_support` with `routed`. In `api_keys.rs`, `handle_revoke` / `handle_delete` read `msg.var("id")`. Run `cargo test -p impresspress-core --lib blocks::auth_ui`. Expected: PASS.
- [ ] **Step 4: rate-limit tests.** Adapt `public_bootstrap_redemption_is_ip_rate_limited` to `rate_limit_for(Route::Bootstrap)` and `bootstrap_form_get_does_not_spend_the_redemption_budget` to `rate_limit_for(Route::BootstrapPage).is_none()`. Add `rate_limits_are_the_old_route_table_assignments`: for every row, the `(key, category)` `rate_limit_for(row.handler)` yields equals what the old five rules yielded for that row's `(action, wire path)`, written out as a test-local table of the old assignments keyed by wire path. Run `cargo test -p impresspress-core --lib blocks::auth_ui`. Expected: PASS.
- [ ] **Step 5: snapshot gates.** `cargo test -p impresspress-core --test openapi_snapshot --test endpoint_surface`. Expected: `openapi_snapshot` PASS (no new row has a schema); `endpoint_surface` FAIL on `auth_ui` only. Regenerate with `env UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test endpoint_surface`; `git diff -- crates/impresspress-core/tests/snapshots/auth_ui.endpoints.json` must show exactly the eleven added lines, no other change. Run both tests again: PASS. `cargo test -p impresspress-core --test auth` (the `login_session_row` suite drives `AuthUiBlock::handle` with the wire path).
- [ ] **Step 6: gates, format, lint, commit.** Grep gate for `blocks/auth_ui` prints nothing. `cargo +nightly fmt --all`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`. Commit:

```
refactor(auth-ui): declare the HTTP surface from the route table

`ROUTES` (29 rows over wire paths) now carries the summaries, auth
levels, schemas and tags `info()` listed by hand for 18 of them, and
`info()` is `declare(ROUTES)`. The eleven pairs the block served but
never declared become rows at the level each handler already enforces:
nine `public` (reset-password page, OAuth callback, verify GET and POST,
resend-verification, forgot-password, reset-password, OAuth providers,
sync-user), two `authenticated` (api-key revoke and delete). The `/b`
strip and the `match (action, path)` over the stripped form go; `handle`
dispatches through `endpoint_match::dispatch`, and the api-key handlers
read `{id}` only as the table bound it.

`RATE_LIMIT_ROUTES` matched the stripped path a second time; it becomes
`rate_limit_for(Route)`, applied after dispatch, with every assignment
preserved (IP-keyed `auth`/`refresh`, user-keyed `auth_read`/`auth_write`).
OpenAPI snapshot unchanged; the surface snapshot gains the eleven lines.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 3: the router reads the auth-ui declaration

**Files:**
- Modify: `crates/impresspress-core/src/routing.rs` (tests only)

- [ ] **Step 1 (RED, before Task 2's Step 3 lands): the test.** In `routing.rs`'s `mod tests` add `auth_ui_declares_every_path_the_router_carves_out`: build `AuthUiBlock::new().info()`; assert `endpoint_match::endpoint_auth(&info.endpoints, action, path) == Some(AuthLevel::Public)` for the nine public pairs and `Some(AuthLevel::Authenticated)` for `update` and `delete` on `/b/auth/api/api-keys/k-1`; and assert that the set of `router_final` auth-ui prefixes in `ROUTES` is exactly the set of paths the public list covers, so a carve-out this test does not cover cannot exist. Run `cargo test -p impresspress-core --lib routing::tests::auth_ui_declares_every_path_the_router_carves_out`. Expected (against the 18-row declaration): FAIL, `None` for the first undeclared path.
- [ ] **Step 2 (GREEN):** after Task 2 the same command passes.
- [ ] **Step 3: commit.**

```
test(routing): the auth-ui declaration alone makes its carve-out paths public

Every `/b/auth/...` path `routing.rs` still admits through
`router_declared_public` now resolves to `Public` from
`AuthUiBlock::info()` through `endpoint_match::endpoint_auth`, and the
two api-key rows to `Authenticated`. The carve-outs stay until PR 7
deletes `router_final`; this test is what makes that deletion safe.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 4: Verify and open the PR

- [ ] **Step 1: full verification**

```
cargo +nightly fmt --all -- --check
cargo clippy -p impresspress-core --all-targets -- -D warnings
cargo test -p impresspress-core --no-fail-fast
cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot
grep -rn 'path_param(\|strip_prefix("/b\|starts_with("/b\|dispatch_path(\|normalize_template' crates/impresspress-core/src/blocks/userportal crates/impresspress-core/src/blocks/auth_ui crates/impresspress-core/src/endpoint_match.rs
git status --short
git diff origin/main --stat -- crates/impresspress-core/tests/snapshots/
```

Expected: fmt clean; clippy clean; all tests pass except `lockfile_loads_remote_block`; grep prints nothing; working tree clean; the snapshot diff is `userportal.endpoints.json` (1 line changed) and `auth_ui.endpoints.json` (11 lines added) and nothing else.

- [ ] **Step 2: push and open the PR** with `bash <scratchpad>/push-and-pr.sh "refactor(auth): declare userportal and auth-ui from their route tables" <body-file>`. Body: per-block row count, the exact snapshot diff lines, the eleven rows each with the enforcing handler file and line, the grep-gate output, the tests routed through the table, deviations, trailer. Do not merge.

---

## Self-review

**Spec coverage (PR 3 scope):** section 3's auth-ui paragraph (eleven rows, `/b` strip removed, rate limits keyed on the variant): Task 2. Section 2's last paragraph (`normalize_template` deleted in the PR that rewrites `:hash`): Task 1. Section 5 "Blocks" bullet (table test per block; handlers that change get a binding test through the real table): Tasks 1 and 2. Section 5 "Router" bullet is PR 7's, but Task 3 lays the test PR 7 needs. Sequencing item 3: the whole plan.

**Deviations recorded:** (1) userportal's `/internal/list-buttons` action and its non-`/b/userportal` fallback are deleted rather than kept behind a handler-owned guard as `llm`'s internal target is, because unlike that target they have no caller. (2) Three of the nine public auth-ui rows (`resend-verification`, `forgot-password`, `oauth/providers`) issue a token or return configuration instead of consuming a token, signature or secret; the spec's wording "gated by a token" is inexact for them, and `public` is nonetheless the only level that keeps them working and is the level the router grants today. (3) The identity resolution for a `LimitKey` (`ip_identity` vs `msg.user_id()`) is repeated in auth-ui's `apply_rate_limit` rather than factored out of `rate_limit::check_route_limits`, because `rate_limit.rs` is outside this PR's files and products still uses `check_route_limits`; when PR 6 migrates products, `RouteLimit` and `check_route_limits` lose their last caller and the shared helper is the natural refactor.
