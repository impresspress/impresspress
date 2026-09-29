# Route table single source, PR 5: admin

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `admin` so one `const ROUTES: &[EndpointRoute<Route>]` over wire paths in `blocks/admin/mod.rs` is the block's only description of its HTTP surface: it dispatches every request and generates `info().endpoints` through `endpoint_match::declare`. Delete the two-tier dispatch (`route.rs`'s `AdminRoute` classifier over the wire path, then five sub-handler `match (action, path)` blocks over a normalized `/admin/...` form computed by `api_norm`). Declare the 37 paths the block serves but never declared, every one `admin`, which is the level the router already enforces for the whole `/b/admin/` prefix. Fix the admin users page's API-key revoke control, which posts to a path auth-ui never served, and add the render guard that would have caught it.

**Architecture:** PR 1 made `EndpointRoute<H>` carry the declaration and added `declare`, `request_schema_of`, `response_schema_of`. `admin` today matches a path in three places: `route.rs::route(path, action)` classifies into 40 `AdminRoute` variants using `strip_prefix` / `strip_suffix` chains and a literal `match sub`; `mod.rs::handle` computes `api_norm` (`/b/admin/api{rest}` becomes `/admin{rest}`) and hands it to `users::handle`, `database::handle`, `iam::handle`, `logs::handle`, `settings::handle`, each of which matches `(action, path)` again and extracts ids with `strip_prefix`; the page handlers receive ids the classifier sliced. All of that becomes one 57-row table, one `match route` in `handle`, and leaf handlers that read `msg.var(..)`. The `route` module is deleted.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e` (`BlockEndpoint`, `AuthLevel`, `HttpMethod`, `Message::var`), `schemars` 1, `serde_json`, `maud`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md` (this plan implements its "PR 5": sequencing item 5, "admin: `route.rs` and `api_norm` replaced by the table"). Models: the four earlier plans under `docs/superpowers/plans/2026-09-05-route-table-*.md`, especially PR 4's for a block declaring many previously undeclared rows with per-row justification.

## Decisions taken while planning (recorded, not re-litigated)

1. **Every new row is `admin`, and that is a restatement, not a change.** The router's prefix table carries `Route::new("/b/admin/", RouteAccess::Admin, "impresspress/admin")` (`routing.rs:328`) and `Route::new("/b/admin/settings", RouteAccess::Admin, ..)` (`routing.rs:320-324`), and `route_to_block` enforces `route.access.max(declared_access(..))`, so no request under `/b/admin/` reaches this block today without the `admin` role, declared or not. The 37 new rows make that visible in `admin.endpoints.json`; none lowers or raises what any caller can reach. No handler in the block re-checks `is_admin` (the block has always relied on the router), and that does not change. The inventory below found no path under `/b/admin/` that a non-admin is meant to reach.
2. **Two sub-handler arms are dead code, not served paths, and are deleted rather than declared.** `logs.rs:27` matches `("retrieve", "/admin/system-logs")`, but the classifier sends only `/b/admin/api/logs*` to `logs::handle` (first segment `logs`), so the wire path `/b/admin/api/system-logs` classifies `ApiNotFound` and answers 404 today; a repo-wide grep (`crates packages docs`) finds no consumer and the SSR logs page reads `request_logs` itself. `settings.rs:121-126` matches `/settings` and `/settings/...`, but the normalized path handed to `settings::handle` always begins `/admin/`. Declaring either would add a path the block never served, which is a new decision outside this PR's remit. `handle_system_logs` goes with its arm.
3. **The four settings tabs are four literal rows, not `/b/admin/settings/{tab}` plus a whitelist.** The classifier's `"email" | "network" | "variables" | "permissions"` arm is the whitelist; putting it in the table keeps the table the only place that decides what is served, and each row dispatches to `pages::settings_page(ctx, &msg, "<tab>")` with a literal. An unknown tab 404s from the matcher as it did from the classifier.
4. **Exact templates replace prefix/suffix strips; the accidental extra shapes they admitted go.** Today `/b/admin/api/users/{id}/anything` serves the user (`user_id_from` takes the first segment), `/b/admin/settings/email/extra` serves the email tab (`rest.split('/').next()`), `/b/admin/api/extensions/anything` with any action serves the extension list (the arm checked neither action nor suffix), and every SSR fallthrough page answers any action (`DELETE /b/admin/users` rendered the users page). The rows are exact and method-specific. This is the same narrowing every earlier migration made (`extra_path_segments_do_not_match`), and no consumer relies on the accidental forms: the SDK calls `GET /b/admin/api/extensions` and `/b/admin/api/iam/roles[/{id}]` exactly; the pages emit exact paths (inventory section C).
5. **The block-name `--` codec stays.** `/b/admin/blocks/{name}/detail` binds the encoded segment (`impresspress--files`); `pages/blocks.rs` gains `decode_block_name` beside `encode_block_name` and the two block handlers read `decode_block_name(msg.var("name"))`. Switching the pages to percent-encoded `/` would change URLs for no gain.
6. **The variables edit form keeps `hx-put`; the row is declared `PATCH`.** `HttpMethod` has no `Put`; both PUT and PATCH map to the `update` action the matcher compares, so the form keeps working. The existing iam comment ("`handle()` matches the `update` action, which both PUT and PATCH map to") already records this convention.
7. **The five sub-handler dispatchers are deleted, not re-keyed on the variant.** `users::handle`, `database::handle`, `iam::handle`, `logs::handle`, `settings::handle` exist only to match a path; with the table doing that, `handle`'s one `match route` calls each leaf directly. Leaves become `pub(super)` and read `msg.var("id")` / `msg.var("key")` / `msg.var("name")` where they took a sliced id or a path. Tests that hand a leaf a hand-built message route it through the table with `test_support::routed`.
8. **The API-key revoke control becomes `hx-patch="/b/auth/api/api-keys/{id}"`.** auth-ui declares `PATCH /b/auth/api/api-keys/{id}` `authenticated` as `Route::RevokeApiKey` (`auth_ui/mod.rs:190-196`), and `handle_revoke` admits another user's key when `is_admin(msg)`. Out of scope, recorded for later: `handle_revoke` answers `{"message": "API key revoked"}` as JSON, which htmx will swap into `#users-tab-content`; a page-side re-render belongs to the UI phase.
9. **The render guard treats `data-detail-url` as a GET too.** The network rows carry the inbound-detail URL in a data attribute that the page script fetches; it is a link an admin page emits under `/b/admin/` and must resolve for the same reason an `hx-get` must.

## Global Constraints

- Only `crates/impresspress-core/src/blocks/admin/**`, `crates/impresspress-core/tests/snapshots/admin.endpoints.json` and this plan change. No change to wafer-run, `routing.rs` (its `/b/admin/settings` prefix entry stays for PR 7), `endpoint_match.rs`, or any other block. `EndpointRoute::new` stays for products.
- `crates/impresspress-core/tests/snapshots/admin.openapi.json` is byte-identical: the 20 declared rows copy their summary, description, tags, agent tool and schemas verbatim (`.query_params::<T>()` becomes `.query_params(request_schema_of::<T>)`, `.input::<T>()` becomes `.input(request_schema_of::<T>)`, `.output::<T>()` becomes `.output(response_schema_of::<T>)`, `.path_params_schema(role_id_path_schema())` becomes `.path_params(role_id_path_schema)`), and no new row carries a schema. `UPDATE_OPENAPI_SNAPSHOTS=1` is never run against the `openapi_snapshot` test. Every other `*.openapi.json` and `*.endpoints.json` is byte-identical.
- `admin.endpoints.json` changes by exactly the 37 added lines in section A of the inventory, all `admin`; regenerated once at the end, and the diff goes into the PR body with the dispatching handler per line.
- Handlers read path variables only through `msg.var(..)` after `endpoint_match::dispatch` bound them. After the migration `grep -rn 'path_param(\|strip_prefix("/b\|starts_with("/b\|strip_prefix("/admin\|api_norm\|dispatch_path(' crates/impresspress-core/src/blocks/admin` prints nothing outside test-only string assertions.
- TDD: write the test, run it and see it fail for the expected reason, then implement. Commits carry the two trailer lines:
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Format with `cargo +nightly fmt --all`. Lint with `cargo clippy -p impresspress-core --all-targets -- -D warnings`. `cargo test -p impresspress-core --no-fail-fast` has one known unrelated failure, `lockfile_loads_remote_block`; every other test must pass.
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase1/route-table-admin` (created from `origin/main` at `d845fb4b`, the merge of PR 4). The session's shell guard refuses compound commands containing `git` or shell variables; those go in a script under the scratchpad directory and run with `bash <script>`.

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `crates/impresspress-core/src/blocks/admin/mod.rs` | `Route` (57 variants), 57-row `ROUTES` over wire paths, `info()` = `declare(ROUTES)`, `handle` = dispatch + one `match`; `redirect_308`, the two WRAP-grant handlers (reading `msg.var("id")`); `test_support::routed`, `table_tests`, `page_link_tests` (the render guard), existing test modules adapted. |
| `crates/impresspress-core/src/blocks/admin/route.rs` | Deleted. |
| `crates/impresspress-core/src/blocks/admin/users.rs` | No `handle`, no `user_id_from`; four `pub(super)` leaves, three reading `msg.var("id")`. |
| `crates/impresspress-core/src/blocks/admin/database.rs` | No `handle`; four `pub(super)` leaves, `handle_columns` reading `msg.var("name")`. |
| `crates/impresspress-core/src/blocks/admin/iam.rs` | No `handle`; ten `pub(super)` leaves, the four id-bearing ones reading `msg.var("id")`; tests routed. |
| `crates/impresspress-core/src/blocks/admin/logs.rs` | No `handle`, no `handle_system_logs`; `handle_list` `pub(super)`. |
| `crates/impresspress-core/src/blocks/admin/settings.rs` | No `handle`; six `pub(super)` leaves, the three key-bearing ones reading `msg.var("key")`; the `/settings/` fallback in `handle_get` gone; test routed. |
| `crates/impresspress-core/src/blocks/admin/pages/users.rs` | Three user handlers and `handle_delete_role` read `msg.var("id")`; revoke control is `hx-patch`; new render test. |
| `crates/impresspress-core/src/blocks/admin/pages/blocks.rs` | `decode_block_name`; both handlers read `decode_block_name(msg.var("name"))`; toggle tests routed. |
| `crates/impresspress-core/src/blocks/admin/pages/variables.rs` | `handle_edit_variable_form` / `handle_update_variable` read `msg.var("key")`. |
| `crates/impresspress-core/src/blocks/admin/pages/settings.rs` | Module doc's route list corrected (no behaviour change). |
| `crates/impresspress-core/tests/snapshots/admin.endpoints.json` | Regenerated: +37 lines. |
| `docs/superpowers/plans/2026-09-05-route-table-5-admin.md` | This plan. |

---

### Task 0: Commit this plan

- [ ] **Step 1: Commit**

```
docs: plan for phase 1 PR 5 (admin)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 1: The served-path inventory

Every `(method, wire path)` the block answers today, walked from `route.rs::route` (the classifier, `route.rs:118-259`) and from each sub-handler's `match (action, path)` over the `api_norm` form (`users.rs:27-39`, `database.rs:128-138`, `iam.rs:39-62`, `logs.rs:25-29`, `settings.rs:119-133`), plus the `ExtensionsApi` arm inlined in `mod.rs:251-266`. `route()` runs before anything else in `handle` and nothing reads `ctx.caller_id()`, so there is no inter-block guard to keep ahead of the matcher. "Declared" means present in `admin.endpoints.json` today (20 lines). Every row below is `EndpointRoute::admin` (decision 1).

**A. JSON API, `/b/admin/api/...`** (classifier arm `AdminRoute::{UsersApi, DatabaseApi, IamApi, LogsApi, SettingsApi, ExtensionsApi}` on the first segment after `/api`, then the sub-handler's match on `/admin{rest}`):

| # | Method, wire path | Declared | `Route` variant | Dispatches to | Old arm |
|---|---|---|---|---|---|
| 1 | `GET /b/admin/api/users` | yes, tool `list_users` | `ListUsersApi` | `users::handle_list` | `users.rs:28` |
| 2 | `GET /b/admin/api/users/{id}` | no | `GetUserApi` | `users::handle_get` (`msg.var("id")`) | `users.rs:29` |
| 3 | `PATCH /b/admin/api/users/{id}` | no | `UpdateUserApi` | `users::handle_update` | `users.rs:32` (`update`; PUT maps to it too) |
| 4 | `DELETE /b/admin/api/users/{id}` | no | `DeleteUserApi` | `users::handle_delete` | `users.rs:35` |
| 5 | `GET /b/admin/api/database/info` | no | `DatabaseInfoApi` | `database::handle_info` | `database.rs:129` |
| 6 | `GET /b/admin/api/database/tables` | no | `DatabaseTablesApi` | `database::handle_tables` | `database.rs:130` |
| 7 | `GET /b/admin/api/database/tables/{name}/columns` | no | `DatabaseColumnsApi` | `database::handle_columns` (`msg.var("name")`) | `database.rs:131` |
| 8 | `POST /b/admin/api/database/query` | no | `DatabaseQueryApi` | `database::handle_query` | `database.rs:136` |
| 9 | `GET /b/admin/api/iam/roles` | yes, tool `list_roles` | `ListRolesApi` | `iam::handle_list_roles` | `iam.rs:41` |
| 10 | `POST /b/admin/api/iam/roles` | yes | `CreateRoleApi` | `iam::handle_create_role` | `iam.rs:42` |
| 11 | `PATCH /b/admin/api/iam/roles/{id}` | yes | `UpdateRoleApi` | `iam::handle_update_role` (`msg.var("id")`) | `iam.rs:43` |
| 12 | `DELETE /b/admin/api/iam/roles/{id}` | yes | `DeleteRoleApi` | `iam::handle_delete_role` (`msg.var("id")`) | `iam.rs:46` |
| 13 | `GET /b/admin/api/iam/permissions` | no | `ListPermissionsApi` | `iam::handle_list_permissions` | `iam.rs:50` |
| 14 | `POST /b/admin/api/iam/permissions` | no | `CreatePermissionApi` | `iam::handle_create_permission` | `iam.rs:51` |
| 15 | `DELETE /b/admin/api/iam/permissions/{id}` | no | `DeletePermissionApi` | `iam::handle_delete_permission` (`msg.var("id")`) | `iam.rs:52` |
| 16 | `GET /b/admin/api/iam/user-roles` | no | `ListUserRolesApi` | `iam::handle_list_user_roles` | `iam.rs:56` |
| 17 | `POST /b/admin/api/iam/user-roles` | no | `AssignRoleApi` | `iam::handle_assign_role` | `iam.rs:57` |
| 18 | `DELETE /b/admin/api/iam/user-roles/{id}` | no | `RemoveRoleApi` | `iam::handle_remove_role` (`msg.var("id")`) | `iam.rs:58` |
| 19 | `GET /b/admin/api/logs` | yes, tool `list_audit_log` | `AuditLogsApi` | `logs::handle_list` | `logs.rs:26` |
| 20 | `GET /b/admin/api/settings` | yes, tool `get_site_settings` | `ListSettingsApi` | `settings::handle_list` | `settings.rs:121` |
| 21 | `GET /b/admin/api/settings/all` | no | `ListSettingsFullApi` | `settings::handle_list_full` | `settings.rs:120` (listed before #22, first-match) |
| 22 | `GET /b/admin/api/settings/{key}` | no | `GetSettingApi` | `settings::handle_get` (`msg.var("key")`) | `settings.rs:122` |
| 23 | `PATCH /b/admin/api/settings/{key}` | no | `SetSettingApi` | `settings::handle_set` (`msg.var("key")`) | `settings.rs:127` |
| 24 | `POST /b/admin/api/settings` | no | `CreateSettingApi` | `settings::handle_create` | `settings.rs:130` |
| 25 | `DELETE /b/admin/api/settings/{key}` | no | `DeleteSettingApi` | `settings::handle_delete` (`msg.var("key")`) | `settings.rs:131` |
| 26 | `GET /b/admin/api/extensions` | no | `ExtensionsApi` | `handle_extensions` (the inline list, moved to a function) | `route.rs:129` + `mod.rs:251` |

Dead arms found in the same walk, deleted and not declared (decision 2): `logs.rs:27` `/admin/system-logs`; `settings.rs:121` `("retrieve", "/settings")` and `settings.rs:123` `starts_with("/settings/")` with `handle_get`'s `.or_else(strip_prefix("/settings/"))`.

**B. Consolidated settings pages, `/b/admin/settings...`** (classifier `route.rs:134-144`):

| # | Method, wire path | Declared | `Route` variant | Dispatches to | Old arm |
|---|---|---|---|---|---|
| 27 | `GET /b/admin/settings/` (bare form via the matcher's slash retry) | no | `SettingsRedirect` | `redirect_308("/b/admin/settings/email")` | `route.rs:135`, `mod.rs:270` |
| 28 | `GET /b/admin/settings/email` | no | `SettingsEmailPage` | `pages::settings_page(.., "email")` | `route.rs:141`, `mod.rs:271` |
| 29 | `GET /b/admin/settings/network` | no | `SettingsNetworkPage` | `pages::settings_page(.., "network")` | same |
| 30 | `GET /b/admin/settings/variables` | no | `SettingsVariablesPage` | `pages::settings_page(.., "variables")` | same |
| 31 | `GET /b/admin/settings/permissions` | no | `SettingsPermissionsPage` | `pages::settings_page(.., "permissions")` | same |

**C. htmx mutations and fragments, `/b/admin/...`** (classifier `route.rs:149-238`, action-gated). Every URL an admin page emits under `/b/admin/` (grepped over `pages/*.rs` for `hx-*`, `href=`, `fetch(`, `data-detail-url`) is one of these or one of the pages in D:

| # | Method, wire path | Declared | `Route` variant | Dispatches to | Old arm / emitted by |
|---|---|---|---|---|---|
| 32 | `POST /b/admin/users/{id}/disable` | no | `UserDisable` | `pages::handle_user_disable` (`msg.var("id")`) | `route.rs:150`; `pages/users.rs:258` |
| 33 | `POST /b/admin/users/{id}/enable` | no | `UserEnable` | `pages::handle_user_enable` | `route.rs:158`; `pages/users.rs:251` |
| 34 | `DELETE /b/admin/users/{id}` | no | `UserDelete` | `pages::handle_user_delete` | `route.rs:193`; `pages/users.rs:267` |
| 35 | `POST /b/admin/iam/roles` | no | `CreateRole` | `pages::handle_create_role` | `route.rs:166`; `pages/users.rs:431` |
| 36 | `DELETE /b/admin/iam/roles/{id}` | no | `DeleteRole` | `pages::handle_delete_role` (`msg.var("id")`) | `route.rs:198`; `pages/users.rs:412` |
| 37 | `GET /b/admin/blocks/{name}/detail` | no | `BlockDetail` | `pages::handle_block_detail` (`decode_block_name(msg.var("name"))`) | `route.rs:210`; `pages/blocks.rs:166` |
| 38 | `POST /b/admin/blocks/{name}/toggle` | no | `BlockToggle` | `pages::handle_toggle_feature` | `route.rs:169`; `pages/blocks.rs:317,365` |
| 39 | `POST /b/admin/variables` | no | `CreateVariable` | `pages::handle_create_variable` | `route.rs:179`; `pages/variables.rs:53` |
| 40 | `GET /b/admin/variables/{key}/edit` | no | `EditVariableForm` | `pages::handle_edit_variable_form` (`msg.var("key")`) | `route.rs:220`; `pages/variables.rs:191,299` |
| 41 | `PATCH /b/admin/variables/{key}` | no | `UpdateVariable` | `pages::handle_update_variable` (`msg.var("key")`) | `route.rs:233` (`update`; the form sends PUT, decision 6); `pages/variables.rs:545` |
| 42 | `GET /b/admin/network/detail/inbound` | no | `NetworkInboundDetail` | `pages::network_inbound_detail` | `route.rs:228`; `pages/network.rs:180,298` |
| 43 | `POST /b/admin/grants/rules` | no | `CreateWrapGrant` | `handle_create_wrap_grant` | `route.rs:182`; `pages/permissions.rs:289` |
| 44 | `DELETE /b/admin/grants/rules/{id}` | no | `DeleteWrapGrant` | `handle_delete_wrap_grant` (`msg.var("id")`) | `route.rs:203`; `pages/permissions.rs:199` |
| 45 | `POST /b/admin/email` | yes | `SaveEmailSettings` | `pages::handle_save_email_settings` | `route.rs:185`; `pages/email.rs:48` (fetch) |
| 46 | `POST /b/admin/database/query` | yes | `DatabaseQuery` | `pages::handle_database_query` | `route.rs:188`; `pages/database.rs:280` |

**D. SSR pages, `/b/admin/...`** (classifier fallthrough `route.rs:242-255`, today action-agnostic; rows are `GET`, decision 4):

| # | Method, wire path | Declared | `Route` variant | Dispatches to |
|---|---|---|---|---|
| 47 | `GET /b/admin/` (bare `/b/admin` via slash retry) | yes | `Dashboard` | `pages::dashboard` |
| 48 | `GET /b/admin/users` | yes | `UsersPage` | `pages::users_page` |
| 49 | `GET /b/admin/storage` | yes | `StoragePage` | `pages::storage_page` |
| 50 | `GET /b/admin/blocks` | yes | `BlocksPage` | `pages::blocks_page` |
| 51 | `GET /b/admin/database` | yes | `DatabasePage` | `pages::database_page` |
| 52 | `GET /b/admin/logs` | yes | `LogsPage` | `pages::logs_page` |
| 53 | `GET /b/admin/email` | yes | `EmailRedirect` | `redirect_308("/b/admin/settings/email")` |
| 54 | `GET /b/admin/network` | yes | `NetworkRedirect` | `redirect_308("/b/admin/settings/network")` |
| 55 | `GET /b/admin/variables` | yes | `VariablesRedirect` | `redirect_308("/b/admin/settings/variables")` |
| 56 | `GET /b/admin/permissions` | yes | `PermissionsRedirect` | `redirect_308` to `/b/admin/settings/permissions`, carrying `?tab=` as `?subtab=` |
| 57 | `GET /b/admin/grants` | yes | `GrantsPage` | `pages::grants_page` |

Pages that answer a 308 today and stay rows: #27, #53, #54, #55, #56.

Paths the classifier deliberately answers 404 and that must stay unmatched: `/b/admin/api/wafer`, `/b/admin/api/custom-tables`, `/b/admin/api/storage/buckets`, `/b/admin/api/cloudstorage/shares`, `/b/admin/api/system-logs`, `/b/admin/settings/foobar`, `/b/admin/custom-blocks/install` (POST), `/b/admin/custom-blocks/impresspress--foo` (DELETE), `/b/admin/whatever`, `/b/admin/users//disable` (POST), `/b/admin/iam/roles/` (DELETE), `/b/other`, `/`.

**Totals:** 57 rows; 20 declared before, 57 after; 37 new lines in `admin.endpoints.json`, all `admin`.

---

### Task 2 (RED): the table tests and the two render tests

**Files:** `admin/mod.rs`, `admin/pages/users.rs`

- [ ] **Step 1: table tests.** Add `mod table_tests` to `admin/mod.rs`: `info_endpoints_come_from_the_table` (length equal to `ROUTES.len()`; per `zip` pair method, path and auth equal); `every_row_is_admin` (every row's `auth` is `AuthLevel::Admin`, and `endpoint_auth` over `info().endpoints` resolves every row's template with a probe value to `Admin`); `every_path_the_block_served_resolves_to_a_row` over the 57 inventory entries as `(action, path, Route, bound vars)`, including the bare `/b/admin` and `/b/admin/settings` forms and a `--`-encoded block name; `paths_the_classifier_refused_stay_unmatched` over the 404 list. Run `cargo test -p impresspress-core --lib blocks::admin::table_tests`. Expected: FAIL to compile, `cannot find value ROUTES`.
- [ ] **Step 2: the revoke control.** Add `mod tests` to `pages/users.rs` with `api_keys_tab_revokes_through_the_declared_patch_route`: on `TestContext::with_auth()` seed a user and an active API key row, render `users_page` with `?tab=api-keys` as admin, assert the HTML contains `hx-patch="/b/auth/api/api-keys/<id>"` and does not contain `/revoke`. Run it. Expected: FAIL (the page emits `hx-post=".../revoke"`).
- [ ] **Step 3: the render guard.** Add `mod page_link_tests` to `admin/mod.rs` with `every_link_an_admin_page_emits_resolves_to_a_declared_row`: on `TestContext::with_auth()` seed one user, one custom role, one active API key, one variable, one WRAP grant, one request-log row, and register a Feature-category probe `BlockInfo`; render every page and fragment through `AdminBlock::new().handle(..)` (dashboard; users ×3 tabs; storage; blocks + the probe's detail fragment; database; logs ×2 tabs; settings ×4 tabs + permissions `?subtab=database`; grants; the variable edit form; the network inbound-detail fragment); extract every `hx-get`/`hx-post`/`hx-patch`/`hx-put`/`hx-delete`/`data-detail-url` attribute value, HTML-unescape it, strip the query; every URL must start with `/b/admin/` or `/b/auth/`; `/b/admin/` URLs must `dispatch` against `ROUTES` with the attribute's action (`hx-post` → `create`, `hx-get`/`data-detail-url` → `retrieve`, `hx-patch`/`hx-put` → `update`, `hx-delete` → `delete`); `/b/auth/` URLs must resolve through `endpoint_auth(&AuthUiBlock::new().info().endpoints, ..)`. Non-vacuity: assert the collected set contains the user disable, role delete, variable edit, grant delete, block detail and API-key revoke URLs, and that at least 20 URLs were collected. Run it. Expected: FAIL to compile (`ROUTES` missing).

---

### Task 3 (GREEN): one table, one match, no classifier

**Files:** `admin/mod.rs`, delete `admin/route.rs`, `admin/{users,database,iam,logs,settings}.rs`, `admin/pages/{users,blocks,variables,settings}.rs`

- [ ] **Step 1: the table and dispatch.** In `mod.rs`: `enum Route` (57 variants, `Clone, Copy, Debug, PartialEq, Eq`); `const ROUTES` in inventory order (A, B, C, D), the 20 declared rows with their metadata verbatim, every row `EndpointRoute::admin`, #21 before #22; `info()` = `.endpoints(endpoint_match::declare(ROUTES))`; `handle` = `let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else { return err_not_found("not found") };` and one `match route`. Delete `mod route;`, `route.rs`, `api_norm`, the `BlockEndpoint` import; move the extensions list into `handle_extensions(ctx)`. `handle_delete_wrap_grant(ctx, msg)` reads `msg.var("id")`. Add `mod test_support` with `routed(msg)` (dispatches through `ROUTES`, panics when no row matches).
- [ ] **Step 2: the sub-handlers.** Delete `users::handle` + `user_id_from`, `database::handle`, `iam::handle`, `logs::handle` + `handle_system_logs`, `settings::handle`. Make the leaves `pub(super)`; `users::{handle_get, handle_update, handle_delete}`, `iam::{handle_update_role, handle_delete_role, handle_delete_permission, handle_remove_role}`, `settings::{handle_get, handle_set, handle_delete}` take `msg` and read `msg.var("id")` / `msg.var("key")`; `database::handle_columns` reads `msg.var("name")`. Keep each leaf's empty-id guard (belt and braces; the matcher never binds an empty segment). Rewrite the module docs that describe the normalized path.
- [ ] **Step 3: the pages.** `pages/users.rs`: `handle_user_disable/enable/delete` and `handle_delete_role` read `msg.var("id")`. `pages/blocks.rs`: add `decode_block_name`, both handlers read `decode_block_name(msg.var("name"))`; the three toggle tests send `admin_msg("create", "/b/admin/blocks/impresspress--files/toggle")` through `routed`. `pages/variables.rs`: `handle_edit_variable_form(ctx, msg)` and `handle_update_variable(ctx, msg, input)` read `msg.var("key")`. `pages/settings.rs`: the module doc lists the wire routes as the table declares them.
- [ ] **Step 4: tests routed through the table.** `iam.rs`: the seven `handle_update_role` / `handle_delete_role` / `handle_remove_role` calls build `admin_msg(action, "/b/admin/api/iam/...{id}")` through `routed`. `settings.rs::handle_get_masks_secret_suffix_without_flag` calls `handle_get(&ctx, &routed(admin_msg("retrieve", "/b/admin/api/settings/STRIPE_SECRET")))`. `mod.rs` wrap-grant tests route their delete messages. `delegation_tests` keep passing unchanged (the old paths still 404, now from the matcher). Run `cargo test -p impresspress-core --lib blocks::admin`. Expected: PASS except `page_link_tests` and the revoke render test.
- [ ] **Step 5: the revoke fix.** In `pages/users.rs:509`, `hx-post={"/b/auth/api/api-keys/" (record.id) "/revoke"}` becomes `hx-patch={"/b/auth/api/api-keys/" (record.id)}`. Run `cargo test -p impresspress-core --lib blocks::admin`. Expected: PASS.
- [ ] **Step 6: snapshots.** `cargo test -p impresspress-core --test openapi_snapshot --test endpoint_surface`: `openapi_snapshot` PASS; `endpoint_surface` FAIL on `admin` only. Regenerate once with `env UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test endpoint_surface`. `git diff -- crates/impresspress-core/tests/snapshots/` must show exactly 37 added lines in `admin.endpoints.json`, all ending `admin`, and nothing else. Anything else: stop and report. Run both tests again: PASS.
- [ ] **Step 7: gates, format, lint, commit.** Grep gate prints nothing outside test-only string assertions. `cargo +nightly fmt --all`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`. Commit the migration (table, dispatch, sub-handlers, classifier deletion, snapshot) as one commit and the revoke fix + render guard as a second:

```
refactor(admin): declare the admin block from its route table

`ROUTES` (57 rows over wire paths) now carries the summaries, schemas
and agent tools `info()` listed by hand for 20 of them, and `info()` is
`declare(ROUTES)`. The two-tier dispatch goes: `route.rs`'s `AdminRoute`
classifier, the `api_norm` rewrite to `/admin/...`, and the five
sub-handler `match (action, path)` blocks that matched the normalized
form a second time. `handle` dispatches through
`endpoint_match::dispatch` and every handler reads `{id}`, `{key}` and
`{name}` only as the table bound them.

The 37 paths the block served but never declared become `admin` rows.
The router already gates the whole `/b/admin/` prefix (and
`/b/admin/settings`) at `Admin`, so every row restates today's
effective level; `admin.endpoints.json` grows by those 37 lines and
`admin.openapi.json` is unchanged. Two unreachable arms
(`/admin/system-logs`, `/settings`) are deleted rather than declared.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

```
fix(admin): revoke API keys through the route auth-ui declares

The users page posted to `/b/auth/api/api-keys/{id}/revoke`, a path
auth-ui never served, so the button answered 404. Revocation is
`PATCH /b/auth/api/api-keys/{id}`; the control is now `hx-patch`. A
render guard proves every `hx-*` URL an admin page emits under
`/b/admin/` dispatches to a row of this block's table with the method
the attribute implies, and every one under `/b/auth/` resolves to a
declared auth-ui endpoint.

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
grep -rn 'path_param(\|strip_prefix("/b\|starts_with("/b\|strip_prefix("/admin\|api_norm\|dispatch_path(' crates/impresspress-core/src/blocks/admin
git status --short
git diff origin/main --stat -- crates/impresspress-core/tests/snapshots/
```

Expected: fmt clean; clippy clean; all tests pass except `lockfile_loads_remote_block`; grep prints nothing; working tree clean; the snapshot diff is `admin.endpoints.json` and nothing else.

- [ ] **Step 2: push and open the PR** with `bash <scratchpad>/push-and-pr.sh "refactor(admin): declare the admin block from its route table" <body-file>`. Body: row count (20 before, 57 after); the exact `admin.endpoints.json` diff with the dispatching handler per added line and the one-paragraph router-tier reasoning; the inventory sources; the grep-gate output; the tests routed through the table; the render-guard coverage; deviations; trailer. Do not merge.

---

## Self-review

**Spec coverage (PR 5 scope):** sequencing item 5 (`route.rs` and `api_norm` replaced by the table): Task 3. Section 3 ("one table, wire paths, `msg.var` only"): Tasks 1 and 3. Section 5 "Blocks" bullet (table test, served-paths test): Task 2. Carry-forward items for PR 5 (revoke control, render guard): Tasks 2 and 3.

**Deviations recorded:** the nine decisions above. In particular: two dead arms deleted rather than declared (decision 2); exact templates narrow the accidental shapes the strips admitted (decision 4); the settings tabs are four literal rows (decision 3).
