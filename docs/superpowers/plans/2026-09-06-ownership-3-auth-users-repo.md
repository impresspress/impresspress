# Ownership and repo boundaries, PR 3: every users-table access goes through the repo

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the three cross-block table-name re-exports in `blocks/auth/mod.rs` (`USERS_TABLE`, `API_KEYS_TABLE`, `RATE_LIMITS_TABLE`) by giving `auth::repo::{users,api_keys,rate_limits}` the functions their consumers were hand-rolling. After this PR the `wafer_run__auth__users` column names are spelled in one Rust file, the `deleted_at IS NULL` predicate exists once, and no block outside `auth/repo/` builds a column map for a users row.

**Architecture:** `auth::repo::users` grows the eight functions spec 2.2.1 names, each returning `Result<_, RepoError>`; `UserRow` grows the two columns the admin projection still read raw (`name`, `last_login_at`) so `AdminUserView` can be built from a row instead of a `Record`; `NewUser` grows `email_verified` and `verification_token_hash` so signup and bootstrap each become a single `insert`. `repo::rate_limits` gains the windowed upsert that `blocks/rate_limit.rs` and `blocks/tickets/abuse.rs` copy between them today, plus the retention delete `tickets/maintenance.rs` runs. `repo::api_keys` gains the all-keys listing the admin API-keys tab runs.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e`, `wafer_core::clients::database`, `serde_json`, `maud`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-09-06-ownership-and-repo-boundaries-design.md`, sections 2.2.1–2.2.3, inventory 1.2, tests 3.2 "PR 3". PR 1 (`#19`) and PR 2 (`#20`) are merged; the spec's line references predate them and are re-resolved below.

## Verified against the tree before planning (the spec's claims, re-checked)

1. **`ensure_admin_role` already calls `platform_state::user_roles::assign`** (`blocks/auth/mod.rs:396`), and **`admin/iam.rs` already calls it** (`:328`), and **the OAuth callback's roles insert is already deleted** (`auth_ui/oauth/callback.rs:452-457` now carries the reasoning as a comment where the insert used to be). All of 2.2.3 landed in PR 1. Nothing to do; the evidence for the deletion is re-verified in Task 1 as a test, not re-argued.
2. **`auth/bootstrap.rs`'s "legacy columns" justification is stale.** `migrations/006_user_extended_fields.sqlite.sql` adds `name TEXT`, `disabled INTEGER NOT NULL DEFAULT 0`, `deleted_at TEXT`; the postgres file adds the same three with `IF NOT EXISTS` and `disabled BOOLEAN NOT NULL DEFAULT FALSE`. Both defaults are exactly what the hand-built map writes (`disabled: false`, `deleted_at: null`), and `insert` already dual-writes `name`. So the map's only remaining unique value is `email_verified: true`, which is what `NewUser.email_verified` is for.
3. **Inventory 1.2 is wrong about `API_KEYS_TABLE`'s consumer.** `admin/pages/users.rs`'s API-keys tab lists **every** key (`db::list(ctx, API_KEYS, &opts)` with `limit: 100`, sorted `created_at desc`, no `user_id` filter), not one user's. `list_for_user` already exists and is the wrong shape; the tab needs `api_keys::list_recent(ctx, limit)`.
4. **Inventory 1.2 is right about the two dead `remove("password_hash")` lines.** `migrations/001_auth_schema.sqlite.sql:20-26` puts `password_hash` on `wafer_run__auth__local_credentials`, a different table; `admin/contracts.rs:53-56` already documents the remove as a no-op.
5. **`RATE_LIMITS_TABLE` has three consumers, and two of them are `cfg(target_arch = "wasm32")`-only.** `blocks/rate_limit.rs:189` and `blocks/tickets/abuse.rs:30` are both wasm-gated; `tickets/maintenance.rs:129` is not. So the shared upsert must take `now` as a parameter rather than reading the clock, or it cannot be compiled — let alone tested — on the host. That is also what makes the spec's "one test shared by rate_limit and tickets" runnable in CI.

## Decisions taken while planning (recorded, not re-litigated)

1. **`UserRow` gains `name: Option<String>` and `last_login_at: Option<String>`.** `AdminUserView` publishes both (`admin/contracts.rs:66,80`) and is the projection every admin read path goes through. Without them the admin surfaces would still need the raw `Record`, which is the bypass this PR removes. The two columns exist since migration 006; the row type was simply incomplete.
2. **`AdminUserView::from_record` becomes `from_row(&UserRow, roles)`; `AdminUserListResponse::from_record_list` becomes `from_page(&UserPage, &roles)`.** The published field set, its order, and its JSON names are untouched, so `admin.openapi.json` stays byte-identical.
3. **`list_active_page` unifies the two search behaviours, and that is a deliberate behaviour change.** The JSON list (`admin/users.rs:26-31`) searches `email LIKE '%q%'` with a full `COUNT`; the SSR tab (`admin/pages/users.rs:118-146`) searches `email OR id LIKE '%q%'` with `skip_count: true`, so its pagination footer reported the in-page count as the total. One door means one answer: search matches **email or id**, and the total is always the full matched count. The JSON search widens to ids; the SSR pagination footer starts telling the truth. Both are stated in the PR body.
4. **`AdminUserPatch::from_body` lives in the repo, not in `admin/ops.rs`.** Spec 2.2.1 folds the `["name", "disabled", "avatar_url"]` whitelist into `patch_admin_fields`. Parsing the request map inside the repo makes the whitelist structural — a caller cannot name a fourth column because there is no field for it. `disabled` accepts the same shapes `repo::map_bool` accepts (`true`, `1`, `"1"`, `"true"`), so today's `{"disabled": 1}` still disables; `name`/`avatar_url` accept JSON strings only, so `{"name": 123}` now no-ops instead of writing a number into a TEXT column.
5. **The `auth_version` bump stays in `admin/ops.rs`.** It is not a users-table write — `blocks::auth::bump_auth_version` pairs the DB increment with the verify-side cache invalidation, and splitting that pair is the drift the P2c comment on `repo::users::bump_auth_version` warns about. `patch_admin_fields` reports nothing new; ops asks the patch whether it touched `disabled`.
6. **`patch_admin_fields` writes `name` only, not `display_name`.** Today's admin PATCH does the same (`ops.rs:224-228`), while `update_profile` dual-writes both. Making them agree changes what `GET /b/admin/api/users` returns for `display_name` after an admin rename, which is a product decision, not a repo-boundary one. Recorded as an out-of-scope finding instead.
7. **`repo::rate_limits::windowed_increment` returns `Result<i64, WaferError>` and `decide_rate_limit` takes that one result.** The upsert and the read-back are one operation ("increment this window's counter and tell me its count"); returning a struct of two `Result`s just to keep `decide_rate_limit`'s two arms would be the copy this task exists to delete, wearing a type. Which half failed survives in the error text (`rate_limits windowed upsert: …` / `rate_limits count read-back: …`), which is all the existing `tracing::warn!` arms distinguish. An empty read-back is still `Ok(0)` → allowed, not an error.
8. **`test_support::seed_auth_user` replaces six copies of the same raw-SQL fixture.** `userportal/pages/{security,sessions,dashboard}.rs`, `auth/repo/api_keys.rs`, `auth/repo/users.rs` and `test_support.rs` each hand-write `INSERT INTO wafer_run__auth__users …` because they need a user under a *caller-chosen* id that the test's authenticated `Message` names, and `users::insert` mints a UUID. One fixture helper keeps the raw-SQL exception (CLAUDE.md permits it for test-fixture setup) in one file and shrinks the door test's allowlist from ten files to six.
9. **The WRAP audit script is NOT extended in this PR.** Spec 3.1 scopes the `platform_state::<module>::` rule to PR 1. The `auth::repo::<module>` analogue is a bigger change than it looks — `blocks/rate_limit.rs` resolves to a caller block id (`impresspress/rate_limit`) that no block declares, so the rule would report a false MISSING the moment it was added. The audit's pair count and the exact pairs this PR moves out of its view are measured and reported instead.

## Global Constraints

- Both snapshot gates byte-identical: `crates/impresspress-core/tests/snapshots/*.openapi.json` and `*.endpoints.json`. This PR declares no endpoint and changes no published field. `UPDATE_OPENAPI_SNAPSHOTS=1` is never run.
- No change to wafer-run (rev `7d47e5e`). No migration, no `.sql` file, no schema change.
- No raw SQL outside test-fixture setup, and that setup is consolidated into `test_support::seed_auth_user` plus the migration-runner tests that must pin the DDL.
- TDD: write the test, run it, see it fail for the expected reason, then implement, then see it pass. Commits carry the two trailer lines:
  `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Verification before the PR: `cargo +nightly fmt --all -- --check`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`; `cargo test -p impresspress-core --no-fail-fast` (known unrelated failure `lockfile_loads_remote_block`); `cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot`; `cargo check -p impresspress-cloudflare --target wasm32-unknown-unknown` (the wasm-only rate-limit paths compile); `bash scripts/audit-wrap-grants.sh`. The CLI is not touched, so `cargo test -p impresspress` is not required.
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase2/auth-users-repo` (from `origin/main` at `f1277386`, the merge of PR #20).

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `crates/impresspress-core/src/blocks/auth/repo/users.rs` | The door. `UserRow` gains `name`/`last_login_at`; `NewUser` gains `email_verified`/`verification_token_hash`; `active_filter()`, `touch_last_login`, `set_disabled`, `soft_delete`, `AdminUserPatch`/`patch_admin_fields`, `ActiveUserQuery`/`UserPage`/`list_active_page`, `active_count_and_created_since`, `DailySignups`/`daily_signups`, `list_recent_active`. |
| `crates/impresspress-core/src/blocks/auth/repo/api_keys.rs` | `list_recent(ctx, limit)` for the admin API-keys tab. |
| `crates/impresspress-core/src/blocks/auth/repo/rate_limits.rs` | `windowed_increment(ctx, id, key, now, window_cutoff)` and `delete_updated_before(ctx, cutoff)`. |
| `crates/impresspress-core/src/blocks/auth/mod.rs` | The three re-exports deleted. `get_user_roles` reads the inline role through `users::find_by_id`. |
| `crates/impresspress-core/src/blocks/auth/bootstrap.rs` | One `users::insert` with `email_verified: true`; the eleven-column map and its stale comment deleted. |
| `crates/impresspress-core/src/blocks/auth/service.rs` | Test-only table names replaced by repo calls. |
| `crates/impresspress-core/src/blocks/auth_ui/api/login.rs` | `users::touch_last_login`. |
| `crates/impresspress-core/src/blocks/auth_ui/api/signup.rs` | The verification fields ride on `NewUser`; the follow-up `db::update` deleted. |
| `crates/impresspress-core/src/blocks/auth_ui/api/change_password.rs` | `users::find_by_id` for the existence check. |
| `crates/impresspress-core/src/blocks/auth_ui/api/bootstrap.rs` | Tests count through `users::count`. |
| `crates/impresspress-core/src/blocks/auth_ui/oauth/callback.rs` | `users::touch_last_login`; test fixtures through `users::{set_disabled,soft_delete}`. |
| `crates/impresspress-core/src/blocks/userportal/mod.rs` | `users::update_profile`. |
| `crates/impresspress-core/src/blocks/userportal/pages/profile.rs` | `users::find_by_id` (`display_name`, not the `name` alias). |
| `crates/impresspress-core/src/blocks/admin/ops.rs` | `users::{set_disabled,soft_delete,patch_admin_fields}`; both `remove("password_hash")` lines deleted. |
| `crates/impresspress-core/src/blocks/admin/users.rs` | `users::{list_active_page,find_by_id}`. |
| `crates/impresspress-core/src/blocks/admin/contracts.rs` | `AdminUserView::from_row`, `AdminUserListResponse::from_page`. |
| `crates/impresspress-core/src/blocks/admin/pages/users.rs` | `users::{list_active_page,find_by_id}`, `api_keys::list_recent`. |
| `crates/impresspress-core/src/blocks/admin/pages/dashboard.rs` | `users::{active_count_and_created_since,daily_signups,list_recent_active}`. |
| `crates/impresspress-core/src/blocks/rate_limit.rs` | `repo::rate_limits::windowed_increment`; `decide_rate_limit` takes one `Result`. |
| `crates/impresspress-core/src/blocks/tickets/abuse.rs` | The same `windowed_increment`. |
| `crates/impresspress-core/src/blocks/tickets/maintenance.rs` | `rate_limits::delete_updated_before`. |
| `crates/impresspress-core/src/test_support.rs` | `seed_auth_user(ctx, id, email)` — the one raw-SQL users fixture. |
| `crates/impresspress-core/tests/repo_door.rs` | Generalised to `(module, literal, qualifier)` doors; the auth users table joins the platform tables. |
| `docs/superpowers/plans/2026-09-06-ownership-3-auth-users-repo.md` | This plan. |

---

### Task 0: This plan

- [ ] Commit this file as the first commit on the branch.

### Task 1: The door test, and the 2.2.3 evidence

- [ ] Generalise `tests/repo_door.rs` from `(module, table)` to `(module, literal, qualifier)`, add `("users", "wafer_run__auth__users", "auth")`, and give it an allowlist. Run it: it fails, naming every file that still spells the users table.
- [ ] Add `tests/auth/…`-style unit tests (in `blocks/auth/mod.rs`) recording the 2.2.3 argument as executable evidence: a user whose inline `users.role` is `admin` and who has no `user_roles` row resolves as `admin` through `get_user_roles`, and `AuthServiceImpl::require_role(Role::Admin)` accepts them. This is what makes the deleted OAuth insert safe; it is asserted, not argued.

### Task 2: `test_support::seed_auth_user`

- [ ] Add the helper; move the six copies onto it.

### Task 3: `NewUser` completions and the two single-`insert` paths

- [ ] Test: `insert(NewUser { email_verified: true, verification_token_hash: Some("h"), .. })` yields `UserRow.email_verified == true` and is findable by `find_by_verification_token("h")`.
- [ ] Test: bootstrap through `insert` produces a row equal, column for column, to the one the eleven-column map produced (a fixture that writes the old map and compares).
- [ ] Implement; rewrite `bootstrap.rs` and `signup.rs`.

### Task 4: the lifecycle writers

- [ ] Tests for `touch_last_login`, `set_disabled`, `soft_delete`, `patch_admin_fields` (including the whitelist: a body naming `role` or `email` changes nothing).
- [ ] Implement; move `admin/ops.rs`, `auth_ui/api/login.rs`, `auth_ui/oauth/callback.rs`, `userportal/mod.rs` onto them; delete both `remove("password_hash")` lines.

### Task 5: the readers

- [ ] Tests for `active_filter` behaviour through `list_active_page` (soft-deleted rows excluded; search matches email and id; `total_count` is the full match count across pages), `active_count_and_created_since`, `daily_signups`, `list_recent_active`, `api_keys::list_recent`.
- [ ] Implement; move `admin/users.rs`, `admin/contracts.rs`, `admin/pages/{users,dashboard}.rs`, `userportal/pages/profile.rs`, `auth_ui/api/change_password.rs`, `auth/mod.rs::get_user_roles` onto them.

### Task 6: rate limits

- [ ] Test: `windowed_increment` twice inside one window returns 1 then 2; once past the cutoff resets to 1; `delete_updated_before` deletes only rows older than the cutoff.
- [ ] Implement; move `rate_limit.rs`, `tickets/abuse.rs`, `tickets/maintenance.rs`; rewrite `decide_rate_limit` and its tests.

### Task 7: delete the re-exports and close the door

- [ ] Delete `auth/mod.rs`'s three re-exports and their comment block. `tests/repo_door.rs` passes with the documented allowlist.
- [ ] Full verification list; both snapshots byte-identical; audit script green.
