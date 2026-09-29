# Ownership and repo boundaries, PR 7: verify the access JWT in the auth service; one session row per refresh family (B12, B14)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Three things that are false today. (1) `AuthServiceImpl` — the impl behind the framework `wafer-run/auth` block's `auth@v1` interface — accepts a `wafer_session` cookie and a PAT bearer, and nothing in the repository issues either; the credential every real request carries is an `auth_token` cookie the router turns into `Bearer <access JWT>`. (2) A `wafer_run__auth__sessions` row is an access token: one is inserted on every issuance, so a browser tab writes ~48 rows a day, each living 30 days, none deleted on logout, all shown on `/b/userportal/sessions` as separate devices, and the per-device revoke deletes the row without revoking anything. (3) `POST /b/auth/api/oauth/sync-user` is an unauthenticated-shaped user-creation endpoint gated by a config var no `ConfigVar` declares and `auth_grants()` does not grant, so it answers 403 unconditionally and has no caller anywhere.

**Architecture:** `crypto::verify_access_token(ctx, token, secret, expected_iss) -> Option<AccessClaims>` becomes the one place an access JWT is checked (signature, `type == "access"`, issuer, blocklist, `auth_version`); `extract_auth_meta` keeps its behaviour by calling it and setting the meta from the returned claims, and `AuthServiceImpl::extract_creds` calls the same function so the service authenticates what production issues. A session row becomes a login family: migration 012 replaces the table with `(family PRIMARY KEY, user_id, auth_method, created_at, last_used_at, expires_at)`, `issue_tokens_and_cookie` touches the row it already has instead of inserting a new one, logout deletes the user's rows next to the refresh revocation it already does, and the userportal revoke calls `tokens::revoke_family` before `sessions::delete` so revoking a device signs it out. `SESSION_LIFETIME_DAYS` becomes the single refresh-TTL source (default 30 → 7) and `REFRESH_TOKEN_TTL_SECS` is deleted, so the row's expiry is the refresh row's expiry by construction. `auth/maintenance.rs::sweep` prunes all four expiry-bearing auth tables through `db::delete_by_filters_count`, run throttled from token issuance and behind an `auth.maintenance` message kind.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e`, `wafer_core::clients::{database, crypto, config}`, `wafer_block_crypto::primitives`, `maud`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-09-06-ownership-and-repo-boundaries-design.md`, sections 2.7, 2.8, 2.9, inventory 1.7-1.9, tests 3.2 "PR 7" and 3.1. PRs 1 (`#19`) through 6 (`#24`) are merged; the branch is cut from `origin/main` at `ee9e5af9`. Every line reference below was re-resolved against that tree.

## The 2.7 premise, verified before writing anything

The spec's recommendation only holds if nothing issues a `wafer_session` cookie and nothing calls the `auth@v1` client. Both were checked across the whole repository (not just `crates/`) and both hold:

1. **`wafer_session` is issued nowhere.** Every occurrence in the workspace is inside the thing being deleted or a description of it: `auth/service.rs:4` (module doc), `:109-117` (`session_cookie_from`), `:652,663,706` (the service's own unit tests), `auth/repo/sessions.rs:4` (a doc comment describing "the planned cookie flow"), `tests/auth/service_require_{user,role,token}.rs` (the three integration tests this PR rewrites), and prose in `docs/`. No handler, no `Set-Cookie`, no JS, no SDK, no example, no e2e spec writes that name. The cookie production sets is `auth_token` (`auth/helpers::build_auth_cookie`), which `blocks/router.rs` turns into an `Authorization: Bearer` header.
2. **Zero `auth@v1` callers.** `grep` for `clients::auth`, `auth@v1`, `call_block("wafer-run/auth"` over `crates/`, `packages/`, `examples/`, `docs/`, `.github/` finds no call site; the only `call_block` in `auth_ui/api/mod.rs:39` targets `impresspress/email`. In wafer-run at `7d47e5e` the same grep finds three hits, none of them a call: `clients/mod.rs:254` (a doc comment naming `clients::auth::require_user` as an example), `service_blocks/auth.rs:17` (`interface: "auth@v1"`, the registration), and `wafer-run/tests/integration_test.rs:1371` (a version-string parser fixture). `AuthServiceImpl` is referenced outside its own file only by `blocks/mod.rs:262` (registration) and the three integration tests.
3. **`pats::insert` has no production caller.** Only `tests/auth/repo_pats.rs` and `tests/auth/service_require_{user,token}.rs` call it. The PAT branch is kept anyway (2.7 keeps it), so this is context, not a deletion.

Nothing contradicts the premise, so 2.7 is implemented as written.

## Verified against the tree before planning (what the inventory got wrong)

1. **1.7's line numbers are all correct** on `ee9e5af9`: `extract_creds` at `service.rs:143-149`, `session_cookie_from` at `:109-117`, `require_user` `:318-352`, `require_token` `:354-384`, `require_role` `:386-425`, `auth/mod.rs:683` the `auth_token` cookie, `crypto.rs:51` `extract_auth_meta`, `pipeline.rs:184-195` its caller.
2. **1.8 is right about the shape and one claim in 2.8 is over-broad.** `sessions::delete_expired` (`repo/sessions.rs:176-196`) is list-then-delete-per-row, as stated. But `jwt_blocklist::delete_expired` (`:81-94`) and `oauth_pkce::delete_expired` (`:110-123`) **already** call `db::delete_by_filters_count`; 2.8's "each rewritten over `db::delete_by_filters_count` rather than list-then-delete" applies to `sessions` only. Both are `#[allow(dead_code)]` because they have no caller — this PR gives them one and the attribute goes.
3. **`tokens::delete_expired` really is absent** (1.8), and the table it would prune is the one that grows fastest: rotation never deletes, it revokes (`tokens.rs:118-140`), so a revoked tombstone is kept forever today.
4. **The access JWT does not carry `family`.** 2.8 could not confirm this from the excerpt it read; `generate_tokens` (`auth/mod.rs:481-597`) inserts `family` into `refresh_claims` (`:580-583`) and not into `access_claims` (`:530-556`). So the claim is added in this PR, along with an `auth.family` meta key set by `extract_auth_meta` from the verified token — the current-session badge must not read an unverified value off the request.
5. **The Phase 1 `routing.rs` carve-out is already gone.** 2.9 hedges "if PR 7 lands first there is nothing to do there"; it landed. `routing.rs` has one `/b/auth/` prefix row. What remains in that file is prose: the comment at `:258-261` naming "internal sync-user" and the `AUTH_UI_SESSION_LESS_PATHS` fixture at `:1882-1891` (nine pairs, one of them sync-user) with three doc comments that say "nine".
6. **The userportal revoke path changes its parameter name, so `userportal.endpoints.json` moves too.** 2.8 specifies `DELETE /b/userportal/sessions/{family}`; the declared row is `{hash}` (`userportal.endpoints.json:3`). The PR brief predicted "exactly one `auth_ui.endpoints.json` line removed"; the true diff is that line plus one changed `userportal.endpoints.json` line. Both are listed in the PR body. `userportal` has no `.openapi.json` (it is not in `SNAPSHOTTED_BLOCKS`), so no schema snapshot moves.
7. **`AuthConfig.session_lifetime_days` (`config.rs:165`) has no reader.** The live reader is the separate `helpers::session_lifetime_days` (`auth/mod.rs:675`). The field is left alone (it is the Init-time view of the same var, and removing it is unrelated churn); recorded as an out-of-scope observation.
8. **`admin` reads no session row** despite `auth_grants()`'s `read("impresspress/admin", "wafer_run__auth__*")` wildcard: `grep sessions crates/impresspress-core/src/blocks/admin/` is empty. The only consumers of `repo::sessions` are `auth/mod.rs` (issuance), `auth/service.rs` (the branch being deleted), `userportal/pages/sessions.rs`, `dev/data_snapshot.rs` (the export allowlist, by name only), and tests.
9. **Consumer search (the full checklist).** `crates/`, `packages/impresspress-js/src`, `packages/`, `examples/` including `examples/tests/*.spec.ts`, `crates/impresspress-web/tests` (Playwright), `docs/`, and the blocks' embedded JS under `src/**/assets/*.js`. Results: nothing anywhere calls `/b/auth/api/oauth/sync-user`; nothing outside `impresspress-core/src` and its tests names `wafer_run__auth__sessions`; the only external reference to the userportal sessions surface is `crates/impresspress-web/tests/e2e/visual-baseline.spec.ts:31,131`, which screenshots the *page* `/b/userportal/sessions` (unchanged columns, unchanged empty state) and never the revoke URL, so no baseline moves.

## Decisions taken while planning (recorded, not re-litigated)

1. **The maintenance throttle stamp does NOT live in `platform_state::variables`.** 2.8 says to keep it there "the way tickets keeps its singleton". Tickets keeps its singleton in its own `impresspress__tickets__maintenance` table, and that is what is copied here: migration 012 also creates `wafer_run__auth__maintenance`, owned by `auth/repo/maintenance.rs`. Writing to `platform_state::variables` from the issuance path would require granting `impresspress/auth-ui` `read_write` on `impresspress__admin__variables` — WRAP grants are per table, and that table holds `WAFER_RUN__AUTH__JWT_SECRET` and every other secret. Buying a GC timestamp with write access to the deployment's signing key is not a trade this PR makes. Secondary reasons: the row would render as an editable config variable on `/b/admin/variables`, and `D1ConfigSource` would hand it to the block as config. The auth-owned table needs no new grant at all — auth-ui's existing `read_write("impresspress/auth-ui", "wafer_run__auth__*")` wildcard covers it.
2. **`auth.maintenance` is handled by `AuthUiBlock`, not the framework auth block.** `wafer-core::service_blocks::auth::AuthBlock::handle` routes every message through wafer-core's own `interfaces::auth::handler`, and wafer-run is frozen at `7d47e5e`, so impresspress cannot add a kind there. `AuthUiBlock` is an impresspress block with its own `handle` and already holds the WRAP grants for every auth table; the kind check goes at the top of its `handle`, exactly mirroring `tickets/mod.rs:326`.
3. **`sessions::touch` reports rows affected, and issuance inserts when it touched nothing.** Migrations re-run in full whenever a block's SQL hash changes (`migration_helper.rs:1-13`), so 012's `DROP TABLE` fires again on every future auth migration. It also fires once for every family that exists today. Without a fallback, a device whose row was dropped (or that predates this migration, or that a sweep removed early) would never reappear on the list, because rotation only touches. `touch` returning `db::update_by_filters_count`'s count and issuance falling back to `insert` on `0` makes the row self-healing and removes the `family: None`/`Some` branch from the write path entirely.
4. **`current_session_hash` becomes `current_session_family`, reading `auth.family` meta.** The meta is set only by `extract_auth_meta` from a fully verified access token. Reading the `family` out of an unverified cookie on the request would let anyone with a well-formed JWT paint the badge on another user's row.
5. **`Creds::Jwt` is `Forbidden` at `require_token`, not `Unauthorized`.** That is the treatment `Creds::Session` had, for the reason its comment gives: scopes live only on PATs, so a valid non-PAT credential is a category error, not a missing one.
6. **The old `token_hash` column and every helper keyed on it go.** `SessionRow.token_hash`, `NewSession.token_hash`, `decode_token_hash`, `find_by_token_hash`, `find_record_by_hash`, `touch_last_used`, `delete_by_token_hash`, `create_for_user`, and `delete_for_user(user_id, hash)` are replaced by `family`-keyed equivalents. Nothing outside this crate names any of them (verified point 9 above).
7. **`REFRESH_TOKEN_TTL_SECS` is deleted rather than redefined.** Two constants for one lifetime is what let the row lie about when the device signs out. `helpers::refresh_ttl_secs(ctx)` derives the seconds from `session_lifetime_days(ctx)`, and `generate_tokens`/`store_refresh_token` both read it, so the JWT `exp` and the row's `expires_at` cannot drift.

## Global Constraints

- Snapshot gates: `auth_ui.endpoints.json` loses exactly one line (`POST /b/auth/api/oauth/sync-user public`); `userportal.endpoints.json` changes exactly one line (`{hash}` → `{family}`); every `*.openapi.json` byte-identical (`auth_ui`'s sync-user row declared neither `.input(..)` nor `.output(..)`, and `userportal` publishes no OpenAPI document at all). `dev.tools.json` unchanged.
- No change to wafer-run (rev `7d47e5e`).
- Core only (`crates/impresspress-core`). `prepared_plan.rs` and the CLI are untouched.
- No raw SQL outside migration files and test-fixture setup.
- TDD: write the test, run it, see it fail for the expected reason, then implement, then see it pass. Commits carry the two trailer lines:
  `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Verification before the PR: `cargo +nightly fmt --all -- --check`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`; `cargo test -p impresspress-core --no-fail-fast` (known unrelated failure `lockfile_loads_remote_block`); `cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot`; `bash scripts/audit-wrap-grants.sh`.
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase2/auth-credential-session-model`.

## Tasks

### Task 1 — `crypto::verify_access_token`, the `family` claim, the `auth.family` meta

- [ ] **RED.** In `crypto.rs` tests: `verify_access_token` returns the claims for a token minted by the test crypto service and `None` for a refresh token, a wrong issuer, a blocklisted jti and a stale `auth_version`; `extract_auth_meta` sets `auth.family` when the token carries one. In `auth/mod.rs`'s `generate_tokens` tests: the access token carries `family` equal to the refresh token's.
- [ ] **GREEN.** Add `pub struct AccessClaims { sub, email, roles, jti, exp, family }` and `pub async fn verify_access_token(..) -> Option<AccessClaims>` holding everything `extract_auth_meta` did before setting meta; `extract_auth_meta` becomes the meta-setting shell over it. Add `META_AUTH_FAMILY = "auth.family"`. `generate_tokens` inserts `family` into `access_claims`.

### Task 2 — `AuthServiceImpl` authenticates the access JWT

- [ ] **RED.** Rewrite `tests/auth/service_require_{user,role,token}.rs` to mint an access JWT with the test crypto service (`MigrationTestCtx` gains a `config_get` that serves `WAFER_RUN__AUTH__JWT_SECRET`): `require_user` accepts it, rejects a refresh JWT, an expired one, a blocklisted jti and a stale `auth_version`; a PAT still works; a `wafer_session` cookie is ignored.
- [ ] **GREEN.** `Creds::{Jwt(String), Pat(Vec<u8>)}`; `extract_creds` is async and calls `verify_access_token`; `session_cookie_from`, `Creds::Session` and the `wafer_session` name are deleted; `require_user` on `Jwt` runs `ensure_active`; `require_token` answers `Forbidden` on `Jwt`. Module doc rewritten.

### Task 3 — migration 012 and the sessions repo

- [ ] **RED.** `tests/auth/migrations_012_sessions_family.rs`: a fresh apply yields the new columns and none of the old; an apply over a database that already holds the 001 table with rows leaves the new shape and no rows. Repo tests for `insert`/`touch`/`delete`/`delete_for_user`/`list_for_user`/`delete_expired` on `family`.
- [ ] **GREEN.** `012_sessions_family.{sqlite,postgres}.sql` (drop, recreate, two indexes, plus the `wafer_run__auth__maintenance` singleton table); `repo/sessions.rs` rewritten on `family`; `repo/maintenance.rs` added.

### Task 4 — issuance, logout, revoke, and the one lifetime

- [ ] **RED.** Login inserts one row keyed by family and N refreshes leave the count at 1; a rotation whose row is missing re-creates it; logout deletes the user's rows; the userportal revoke revokes the family so a later refresh with it is rejected; `refresh_ttl_secs` equals `session_lifetime_days * 86400` and the default is 604 800.
- [ ] **GREEN.** `helpers::refresh_ttl_secs`; `REFRESH_TOKEN_TTL_SECS` deleted; `SESSION_LIFETIME_DAYS_DEFAULT` 30 → 7 with its doc rewritten; `issue_tokens_and_cookie` touch-then-insert; `logout.rs` deletes the user's session rows; `userportal/pages/sessions.rs` renders `family`, reads `auth.family` for the badge, and `handle_revoke` propagates `tokens::revoke_family` then `sessions::delete`; the route row becomes `{family}`.

### Task 5 — `auth/maintenance.rs::sweep` and its two entry points

- [ ] **RED.** A fixture with expired and live rows in all four tables: `sweep` deletes only the expired ones and returns the counts; a second `sweep` inside the hour is skipped; `auth.maintenance` on `AuthUiBlock` returns the result.
- [ ] **GREEN.** `tokens::delete_expired`; `sessions::delete_expired` over `db::delete_by_filters_count`; the two `#[allow(dead_code)]` attributes removed; `auth/maintenance.rs` with `SweepResult`, `sweep`, `sweep_if_due`; the call from `issue_tokens_and_cookie`; the `auth.maintenance` arm in `AuthUiBlock::handle`.

### Task 6 — B14: delete `sync-user`

- [ ] **RED.** A test that `POST /b/auth/api/oauth/sync-user` no longer dispatches (404), and the surface snapshot line is gone.
- [ ] **GREEN.** Delete `api/sync_user.rs`, `api/mod.rs:17`, the `ROUTES` row, `Route::SyncUser` and its two other mentions, the `auth_ui/mod.rs:9` doc line, the `routing.rs` fixture entry and the "nine" prose. Regenerate both endpoint snapshots.

### Task 7 — the doors

- [ ] `tests/repo_door.rs` gains `sessions`, `tokens` and `maintenance` doors for the auth block, each with the smallest allowlist the tree admits and a reason per entry.
