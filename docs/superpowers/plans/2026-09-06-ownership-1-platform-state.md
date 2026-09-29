# Ownership and repo boundaries, PR 1: core `platform_state`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the five platform tables (`impresspress__admin__{variables, block_settings, wrap_grants, request_logs, user_roles}`) one owner each: a module under `crates/impresspress-core/src/platform_state/` that spells the table name, the column names and the row shape once, and offers both the boot flavour (over `Arc<dyn DatabaseService>`, before WRAP) and the runtime flavour (over `&dyn Context`, under WRAP) where the spec names them. Every other module — core, the admin block's pages, the framework auth block, the dev block, the Cloudflare adapter, the web crate and the CLI — reaches those tables through the module's functions. `admin_schema.rs` and the `blocks::admin` table re-exports go; the two stale admin grants go; the WRAP audit learns that a `platform_state::<module>::` call from a block is a database access on that module's table.

**Architecture:** One codec per table (`from_record` / `to_data` on the row struct) shared by both flavours, so a column name appears exactly once in Rust. The boot functions move out of `boot.rs` (which becomes empty and is deleted) and `features.rs` (which keeps the pure planner, the settings types and `ENABLED_DEFAULTS`); the runtime functions replace the raw column maps in `blocks/admin/{settings,ops,iam,mod}.rs`, `blocks/admin/pages/*`, `blocks/auth/mod.rs`, `blocks/auth_ui/oauth/callback.rs` (its roles insert is deleted, not moved) and `blocks/dev/{seed,data_snapshot}.rs`. `pipeline.rs` keeps the inline/queued switch and hands the queued row the table name from `request_logs::TABLE`. The migrations stay under `blocks/admin/migrations/` untouched (decision 5.4); `platform_state/mod.rs` names them as the schema source.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e` (`wafer_core::clients::database` typed client, `DatabaseService`, `ResourceGrant`, `ResourceType::parse_stored`), `serde_json`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`, `wasm-bindgen-test` for the Cloudflare crate, bash for `scripts/audit-wrap-grants.sh`.

**Spec:** `docs/superpowers/specs/2026-09-06-ownership-and-repo-boundaries-design.md`, sections 2.1.1–2.1.3, 2.2.3 (the `callback.rs` deletion), 3.1 (the audit rule) and 3.2 "Repo door" and "PR 1". Section 2.1.4 (`ENABLED_DEFAULTS` from `BlockInfo`) is PR 2 and is not touched here.

## Decisions taken while planning (recorded, not re-litigated)

1. **Every commit builds workspace-wide.** A module task moves a function and, in the same commit, switches every caller of it in every crate (`impresspress-cloudflare`, `impresspress-web`, `impresspress`), so no commit leaves another crate pointing at a deleted item and no temporary re-export bridges the gap. The four per-crate tasks (7–10) are therefore the cleanup and verification gates for each crate — deleting `admin_schema.rs` and the `blocks::admin` re-exports once nothing imports them, the `ADMIN_BLOCK_ID` literal, the door test, the wasm run, the CLI lint budget — not the place the imports change.
2. **`boot.rs` is deleted.** Every function in it moves (variables to `platform_state::variables`, the grant loader to `platform_state::wrap_grants`); an empty module with a doc comment pointing elsewhere is a shim. `builder/boot.rs` (the lifecycle orchestrator) is a different file and stays.
3. **`from_record` takes `(id, data)`.** The boot flavour reads `wafer_core::interfaces::database::service::Record` and the runtime flavour `wafer_core::clients::database::Record`; both carry `id: String` and `data: HashMap<String, Value>`, and the same column map is what the codec decodes. `RecordExt` gains an `impl` for the bare map so both flavours read columns through the one accessor set already in `util.rs`; the `Record` impl forwards to it.
4. **Runtime-flavour errors are `WaferError`; boot-flavour errors keep the types their callers already handle.** Admin handlers match `e.code == ErrorCode::NotFound` and pass errors to `err_internal`, so the typed client's error is the right currency there. `variables`/`wrap_grants` boot functions keep `String` (the CLI and web crates map it into `anyhow`/`String`); `block_settings::{load, load_and_seed}` keep `DatabaseError` (the Cloudflare adapter propagates it). A row that fails to decode surfaces as `ErrorCode::Internal` / `DatabaseError::Internal` / a `String`, never as a default row.
5. **`variables::upsert_by_key` is the non-atomic `db::upsert_by_field` shape everywhere.** `admin/ops.rs` and `settings::seed_defaults` already use it, and it is the one write path the Cloudflare KV row cache invalidates: `KvCachedD1DatabaseService::upsert` *refuses* the atomic `db::upsert` on a cached table. `dev/seed.rs::record_failure` used the atomic form (with a comment about the get-then-create race); it moves to `upsert_by_key`, and the race it avoided is the browser sandbox's single thread. Recorded here so the change is deliberate.
6. **`VariableRow` carries `updated_by`.** The spec's field list omits it; the column exists (`001_admin_schema`), admin writes it on every create/update, and dropping a written column is a behaviour change. Adding a field the schema has is not. `NewVariable`/`VariablePatch` derive `block` from the key (`config_vars::key_block_prefix`, the same rule migration 002 backfills with) on every insert, so a variable created through the admin UI for `IMPRESSPRESS__EMAIL__*` reaches the email block on Cloudflare — today it lands with a NULL `block` and `D1ConfigSource` drops it. Stated in the PR body as the one behaviour change.
7. **Request-log query helpers return typed results.** `pages/{logs,network,dashboard}.rs` build four different `ListOptions`/`AggregateRequest` shapes over the table and read the alias columns by name. The module owns those shapes (`paginated`, `list_recent_errors`, `list_for_path`, `summarise_by_path`, `today_counts`, `daily_counts`) and returns `RequestLogRow`, `PathSummary`, `TodayCounts` and `DailyCounts`, so an alias is spelled once. The `to_wire_filters` and grouped-by-day builders the dashboard wrote for both `request_logs` and `users` move to `util.rs`; the `users` call keeps using them from the dashboard until PR 3 gives `auth::repo::users` its own aggregates.
8. **`user_roles::assign` is the single writer.** `ensure_admin_role` (`assigned_by = ""`, as today), `admin/iam.rs::handle_assign_role` and the two `auth/service.rs` tests that seed an admin row go through it; the OAuth signup insert at `callback.rs` is deleted (spec 2.2.3: the initial role is the inline `users.role`, so password signup and OAuth signup produce the same rows). `AlreadyAssigned` replaces the handler's own existence check.
9. **The audit counts every `platform_state::<module>::` reference from `src/blocks/`, call or constant.** `dev/data_snapshot.rs` names `variables::TABLE` and `user_roles::TABLE` in its export allowlist and then reads them through a generic `db::list_all(ctx, table, ..)` the audit cannot resolve; counting the reference is what makes those reads visible. The dev block's own grants for those tables are declared by the dev block itself (`dev::wrap_grants()` maps `TABLE_ALLOWLIST`), which the runtime honours — `check_resource_access` matches against one flat grant list with no notion of who declared it — but the audit attributes grants to the declaring file's block, so dev's platform-table sites carry an `// audit-allow:` pragma naming that grant. They were not audited at all before this PR (`db::upsert`/`get_by_field`/`delete_by_filters` are outside the audited op set); now they are listed as allowed. That the runtime lets a block grant itself another block's table is recorded as an out-of-scope observation, not fixed here.
10. **`tests/repo_door.rs` strips full-line comments before the literal scan.** Doc comments name the platform tables in prose in a dozen files (`cache_key.rs`, `features.rs`, `builder/boot.rs`, …); a prose mention is not a query. Trailing comments on code lines are not stripped, so nothing hides behind a `//` on the same line as code. The identifier scan (`variables::TABLE` and the other four) is per-file allowlisted with a reason each, exactly as `blocks/products/tests/repo_door_test.rs` does.

## Global Constraints

- Both snapshot gates byte-identical for every block: `crates/impresspress-core/tests/snapshots/*.openapi.json` and `*.endpoints.json`. This PR declares no endpoint. `UPDATE_OPENAPI_SNAPSHOTS=1` is never run.
- No change to wafer-run (rev `7d47e5e`); no SQL file moved or edited (decision 5.4); `ENABLED_DEFAULTS`, `plan_seed_decisions`, `BlockState`, `BlockSettings`, `FeatureConfig` and `BLOCK_SETTINGS_CONFIG_KEY` stay in `features.rs` (PR 2 and `prepared_plan.rs` depend on them there).
- No raw SQL in block code; the moved functions use the typed `db::*` client and `DatabaseService` exactly as the code they replace did. Test fixtures may keep `exec_raw`/`query_raw` where they already do (`features.rs`'s `load_and_seed_tests`, moving with the functions).
- Every table constant is `pub const TABLE: &str = "impresspress__admin__…"` in its module; names unchanged.
- TDD: write the test, run it and see it fail for the expected reason (for a new module, the compile error naming the missing item), then implement, then see it pass. Commits carry the two trailer lines:
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Verification before the PR: `cargo +nightly fmt --all -- --check`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`; `cargo clippy -p impresspress-core --features block-dev,test-support --all-targets -- -D warnings` (the CI lane the dev-gated tests compile in); `cargo test -p impresspress-core --no-fail-fast` (known unrelated failure `lockfile_loads_remote_block`); `cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot --test repo_door --test dev_export --test dev_data_snapshot --test dev_seed`; `env CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner cargo test -p impresspress-cloudflare --target wasm32-unknown-unknown`; `cargo test -p impresspress`; `cargo clippy -p impresspress --all-targets` (four pre-existing lints on main; add none); `cargo check -p impresspress-web --target wasm32-unknown-unknown` if it builds locally; `bash scripts/audit-wrap-grants.sh`.
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase2/platform-state` (from `origin/main` at `cf7978d5`, the merge of PR #18). The session's shell guard refuses compound commands containing `git` or shell variables; those go in a script under the scratchpad directory and run with `bash <script>`.

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `crates/impresspress-core/src/platform_state/mod.rs` | Module docs naming `blocks/admin/migrations/001_admin_schema.*.sql` (+002, +003) as the schema source; `Page<T>`; the five submodules. |
| `crates/impresspress-core/src/platform_state/variables.rs` | `TABLE`, `VariableRow`, `NewVariable`, `VariablePatch`, the codec; boot `seed_if_absent`, `set`, `seed_auto_generated`, `seed_and_load`, `load_all`; runtime `list_all`, `get_by_key`, `insert`, `upsert_by_key`, `delete`, `delete_by_key`. Tests from `boot.rs` (`set_variable_tests`) move here. |
| `crates/impresspress-core/src/platform_state/block_settings.rs` | `TABLE`, `BlockSettingsRow` (+ `state()` → `BlockState`), `BlockSettingsPatch`, the codec; boot `load`, `load_and_seed`; runtime `is_enabled`, `set_enabled`, `upsert_fields`, `list_all`. Tests from `features.rs` (`load_and_seed_tests`, `operational_error_tests`) and `admin/settings.rs` (the four `block_settings_*` tests) move here. |
| `crates/impresspress-core/src/platform_state/wrap_grants.rs` | `TABLE`, `WrapGrantRow` (+ `into_resource_grant`), `NewWrapGrant`, the codec; boot `load`; runtime `list`, `create`, `delete`. Tests from `boot.rs` (`wrap_grants_tests`) move here. |
| `crates/impresspress-core/src/platform_state/request_logs.rs` | `TABLE`, `NewRequestLog` (the pipeline's row, with `to_data`), `RequestLogRow`, `PathSummary`, `TodayCounts`, `DailyCounts`; `insert`, `paginated`, `list_recent_errors`, `list_for_path`, `summarise_by_path`, `today_counts`, `daily_counts`. |
| `crates/impresspress-core/src/platform_state/user_roles.rs` | `TABLE`, `UserRoleRow`, `Assigned`, the codec; `list_for_user`, `list_for_users`, `list_all`, `list_by_role`, `get`, `assign`, `rename_role`, `remove`. |
| `crates/impresspress-core/src/boot.rs` | Deleted. |
| `crates/impresspress-core/src/admin_schema.rs` | Deleted. |
| `crates/impresspress-core/src/features.rs` | Keeps the types, `ENABLED_DEFAULTS`, the planner and `seed_plan_tests`; loses the loaders and their tests. |
| `crates/impresspress-core/src/migration_helper.rs` | `write_state` builds a `BlockSettingsPatch` and calls `block_settings::upsert_fields`; `upsert_block_settings_fields` is gone. |
| `crates/impresspress-core/src/{cache_key,config_generation,pipeline}.rs` | Import `platform_state::{variables, block_settings, wrap_grants, request_logs}::TABLE`; the pipeline's inline write is `request_logs::insert`, the queued row carries `request_logs::TABLE`. |
| `crates/impresspress-core/src/util.rs` | `impl RecordExt for HashMap<String, Value>` (the `Record` impl forwards); `to_wire_filters` and `daily_grouped` moved in from the dashboard. |
| `crates/impresspress-core/src/builder/{boot,registration}.rs`, `deploy_init.rs`, `config_vars.rs`, `config_generation.rs` | Doc references updated; `registration.rs` uses `ADMIN_BLOCK_ID`. |
| `crates/impresspress-core/src/blocks/admin/{mod,settings,ops,iam,logs}.rs`, `pages/{variables,blocks,permissions,network,logs,dashboard}.rs` | Read and write the five tables through `platform_state`; `mod.rs` keeps `ADMIN_BLOCK_ID`, `collections(..)`, `grants(..)` (minus the two stale rows) and the migrations; the `settings::block_settings` submodule and every table re-export are gone. |
| `crates/impresspress-core/src/blocks/auth/{mod,service}.rs`, `blocks/auth_ui/oauth/callback.rs` | `get_user_roles` and `ensure_admin_role` over `user_roles::{list_for_user, assign}`; the service tests seed through `assign`; the OAuth signup insert is deleted. |
| `crates/impresspress-core/src/blocks/dev/{seed,data_snapshot}.rs` | `seed.rs` over `variables::{get_by_key, upsert_by_key, delete_by_key}`; `data_snapshot.rs` names `platform_state::*::TABLE` in its allowlists (audit pragma, door-test entry). |
| `crates/impresspress-core/tests/repo_door.rs` | New. The generalised door test over `platform_state/`: literal scan (comment-stripped) and identifier scan with a per-file allowlist. |
| `crates/impresspress-core/tests/{admin/migrations_002_variables_block,dev_export,dev_data_snapshot}.rs`, `crates/impresspress/tests/{deploy_init,native_wrap_grants}.rs` | Import `platform_state::*::TABLE`. |
| `crates/impresspress-cloudflare/src/{config_source,kv_cached_db,lib}.rs` | `variables::TABLE`; `variables::seed_auto_generated`; `block_settings::{load, load_and_seed}`; `wrap_grants::load`. |
| `crates/impresspress-web/src/config.rs` | `variables::{seed_if_absent, set, seed_and_load}`; `block_settings::load_and_seed`. |
| `crates/impresspress/src/cli/server.rs` | `variables::seed_and_load`; `block_settings::load_and_seed`; `wrap_grants::load`. |
| `scripts/audit-wrap-grants.sh` | Phase 1 indexes `platform_state/*.rs`'s `TABLE`; Phase 3 walks `platform_state::<module>::` references under `src/blocks/` as database callsites on that table. |
| `docs/superpowers/plans/2026-09-06-ownership-1-platform-state.md` | This plan. |

---

### Task 0: Spec

Already committed as `8986127c` (`docs: design spec for ownership and repo boundaries (phase 2)`). This plan is the second commit.

---

### Task 1: `platform_state::variables`

**Files:**
- Create: `crates/impresspress-core/src/platform_state/mod.rs`, `crates/impresspress-core/src/platform_state/variables.rs`
- Delete: `crates/impresspress-core/src/boot.rs` (the grant loader moves in Task 3; until then it stays in `boot.rs` with the variable functions gone — see Step 4)
- Modify: `crates/impresspress-core/src/{lib,util,cache_key,config_generation,admin_schema}.rs`, `blocks/admin/{settings,ops}.rs`, `blocks/admin/pages/variables.rs`, `blocks/dev/seed.rs`, `blocks/dev/data_snapshot.rs`, `tests/admin/migrations_002_variables_block.rs`, `tests/dev_export.rs`, `tests/dev_data_snapshot.rs`, `crates/impresspress-cloudflare/src/{config_source,kv_cached_db,lib}.rs`, `crates/impresspress-web/src/config.rs`, `crates/impresspress/src/cli/server.rs`

**Interfaces:**
- Produces: `variables::{TABLE, VariableRow, NewVariable, VariablePatch, seed_if_absent, set, seed_auto_generated, seed_and_load, load_all, list_all, get_by_key, insert, upsert_by_key, delete, delete_by_key}`; `util::RecordExt for HashMap<String, Value>`.
- Consumes: `wafer_core::clients::database::{list_all, get_by_field, create, upsert_by_field, delete, delete_by_filters}`, `DatabaseService::{list, create, update}`, `config_vars::{key_block_prefix, screaming_block}`, `blocks::all_block_infos`, `blocks::auth::JWT_SECRET_KEY`.

- [ ] **Step 1: Write the failing tests.** In `platform_state/variables.rs`'s test module: a codec round trip on the real admin schema (`insert` a `NewVariable` through `TestContext::with_admin`, `get_by_key` it back, assert every field including `block` derived from the key and `sensitive` as a bool; `to_data` of the read row equals what a second `from_record` reproduces); `upsert_by_key` creates then updates one row; `delete_by_key` on an absent key is `Ok`. Carry `boot.rs`'s `set_variable_tests` over verbatim against the new names. Run `cargo test -p impresspress-core platform_state::variables` and see the compile error name the missing module.
- [ ] **Step 2: Implement the module.** `TABLE`; `VariableRow` with `from_record(id, data)` (requires `key`; `sensitive` via `bool_field`; `block` via `opt_str_field`) and `to_data` (omits `block` when `None` so the column stays NULL); `NewVariable::into_row` synthesises `var_<uuid>` id, derives `block`, stamps both timestamps; `VariablePatch::to_data(key)` emits `key`, the set fields, the derived `block` and `updated_at`. Move the boot functions from `boot.rs` under their new names; the runtime functions over the typed client.
- [ ] **Step 3: Switch the callers.** Core: `cache_key.rs`, `config_generation.rs` (`variables::TABLE`); `admin/settings.rs` (`handle_list_full`, `handle_list`, `handle_get`, `handle_delete`, `seed_defaults`), `admin/ops.rs` (`create_variable`, `update_variable`), `pages/variables.rs` (both tabs and the edit form), `dev/seed.rs` (`record_failure`, `clear_failure`, `last_failure`); `admin_schema::VARIABLES_TABLE` becomes `pub use crate::platform_state::variables::TABLE as VARIABLES_TABLE` for the two dev-gated integration tests and `migrations_002` until Task 7 (they switch in Task 7 with the deletion). Other crates: `config_source.rs`, `kv_cached_db.rs` tests, `lib.rs` (`seed_auto_generated`), `web/config.rs`, `cli/server.rs`.
- [ ] **Step 4: `boot.rs` keeps only `load_wrap_grants_from_db` and its tests** until Task 3 deletes the file.
- [ ] **Step 5: Run** `cargo test -p impresspress-core --no-fail-fast`, `cargo check -p impresspress-cloudflare --target wasm32-unknown-unknown`, `cargo check -p impresspress-web --target wasm32-unknown-unknown`, `cargo check -p impresspress`. All green (the known `lockfile_loads_remote_block` aside).
- [ ] **Step 6: Commit** `refactor(core): platform_state::variables owns the variables table`.

---

### Task 2: `platform_state::block_settings`

**Files:**
- Create: `crates/impresspress-core/src/platform_state/block_settings.rs`
- Modify: `features.rs`, `migration_helper.rs`, `admin_schema.rs`, `blocks/admin/settings.rs` (delete the `block_settings` submodule; `seed_defaults` stamps through `upsert_fields`), `blocks/admin/pages/blocks.rs`, `crates/impresspress-cloudflare/src/lib.rs`, `crates/impresspress-web/src/config.rs`, `crates/impresspress/src/cli/server.rs`, `crates/impresspress/tests/deploy_init.rs`

**Interfaces:**
- Produces: `block_settings::{TABLE, BlockSettingsRow, BlockSettingsPatch, load, load_and_seed, is_enabled, set_enabled, upsert_fields, list_all}`.
- Consumes: `features::{BlockSettings, BlockState, MigrationState, ExistingRow, plan_seed_decisions, USER_EDITED_SENTINEL}`, `cache_key::full_table_list_opts`.

- [ ] **Step 1: Failing tests.** Codec round trip through `upsert_fields` + `list_all` (every hash column, `enabled` as bool); `upsert_fields` on an absent block creates the row `enabled = true` and preserves `enabled` on a second patch; move `features.rs`'s `load_and_seed_tests` and `operational_error_tests`, and `admin/settings.rs`'s four `block_settings_*` tests, against the new names. Compile error names the missing module.
- [ ] **Step 2: Implement.** `from_record` requires `block_name` and `enabled`; `state()` maps to `BlockState`. Boot: `read_rows` over `full_table_list_opts`, `load`, `load_and_seed` (unchanged logic, `apply_seed_decision` builds its insert through `to_data`). Runtime: `is_enabled` (the `columns: ["enabled"]` projection kept), `set_enabled` = `upsert_fields` with `enabled` and the user-edited sentinel, `upsert_fields` (moved from `migration_helper::upsert_block_settings_fields`, typed patch), `list_all`.
- [ ] **Step 3: Callers.** `migration_helper::write_state`; `settings::seed_defaults`'s stamp; `pages/blocks.rs` (`blocks_page` list, toggle, detail, tests); `features.rs` loses the loaders; `admin_schema::BLOCK_SETTINGS_TABLE` re-points; Cloudflare `lib.rs` (three call sites), web `config.rs`, CLI `server.rs`, `tests/deploy_init.rs`.
- [ ] **Step 4: Run** the same set as Task 1 Step 5.
- [ ] **Step 5: Commit** `refactor(core): platform_state::block_settings owns the block_settings table`.

---

### Task 3: `platform_state::wrap_grants`

**Files:**
- Create: `crates/impresspress-core/src/platform_state/wrap_grants.rs`
- Delete: `crates/impresspress-core/src/boot.rs`
- Modify: `lib.rs`, `cache_key.rs`, `builder/boot.rs` (docs), `config_vars.rs` (docs), `config_generation.rs` (docs), `blocks/admin/mod.rs` (`handle_create_wrap_grant`, `handle_delete_wrap_grant`, `wrap_grant_mutation_tests`, `page_link_tests` seed), `blocks/admin/pages/permissions.rs`, `blocks/dev/data_snapshot.rs`, `crates/impresspress-cloudflare/src/lib.rs`, `crates/impresspress/src/cli/server.rs`, `crates/impresspress/tests/native_wrap_grants.rs`

**Interfaces:**
- Produces: `wrap_grants::{TABLE, WrapGrantRow, NewWrapGrant, load, list, create, delete}`; `WrapGrantRow::into_resource_grant`.

- [ ] **Step 1: Failing tests.** Codec round trip (`create` then `list`, `write` as bool from the integer column, empty `resource_type`); `FailingDbOpContext("database.list", TABLE)` makes `list` return `Err` (brief 3.2); move `boot.rs`'s `wrap_grants_tests` (the missing-table and existence-check-error cases) against `load`.
- [ ] **Step 2: Implement.** `from_record` requires `grantee`, `resource` and a bool-shaped `write`; `into_resource_grant` runs `ResourceType::parse_stored`. `load` keeps every warn-and-drop branch, now on the codec's `Err`. Runtime functions over the typed client.
- [ ] **Step 3: Callers** as listed; delete `boot.rs` and its `pub mod` line; update the three doc comments that pointed at `crate::boot::*`.
- [ ] **Step 4: Run** the Task 1 set plus `cargo test -p impresspress`.
- [ ] **Step 5: Commit** `refactor(core): platform_state::wrap_grants owns the wrap_grants table; boot.rs is gone`.

---

### Task 4: `platform_state::request_logs`

**Files:**
- Create: `crates/impresspress-core/src/platform_state/request_logs.rs`
- Modify: `pipeline.rs`, `util.rs` (`to_wire_filters`, `daily_grouped`), `admin_schema.rs`, `blocks/admin/logs.rs` (re-export gone), `blocks/admin/mod.rs` (`page_link_tests` seed), `blocks/admin/pages/{logs,network,dashboard}.rs`, `blocks/dev/data_snapshot.rs`

**Interfaces:**
- Produces: `request_logs::{TABLE, NewRequestLog, RequestLogRow, PathSummary, TodayCounts, DailyCounts, insert, paginated, list_recent_errors, list_for_path, summarise_by_path, today_counts, daily_counts}`; `util::{to_wire_filters, daily_grouped}`.

- [ ] **Step 1: Failing tests.** Codec round trip (`insert` then `paginated`, integers and strings back); `FailingDbOpContext("database.create", TABLE)` makes `insert` return `Err` (brief 3.2); `summarise_by_path`/`today_counts`/`daily_counts` against a seeded table equal the numbers the dashboard test computes with `db::count` (move the relevant assertions from `dashboard.rs`'s test module).
- [ ] **Step 2: Implement.** `NewRequestLog::to_data` is the map `pipeline.rs:531-549` built; `insert` is the inline `db::create`. The six readers own the `ListOptions`/`AggregateRequest` shapes the three pages built.
- [ ] **Step 3: Callers.** `pipeline.rs` (`write_request_log` builds `NewRequestLog`, inline → `insert`, queued → `enqueue_request_log(request_logs::TABLE, row.to_data())`; tests count through `paginated`); the three pages render typed rows; `dashboard.rs` keeps `user_counts`/the `USERS` daily series over the util helpers.
- [ ] **Step 4: Run** the Task 1 set.
- [ ] **Step 5: Commit** `refactor(core): platform_state::request_logs owns the request_logs table`.

---

### Task 5: `platform_state::user_roles`

**Files:**
- Create: `crates/impresspress-core/src/platform_state/user_roles.rs`
- Modify: `blocks/admin/{mod,iam,ops}.rs`, `blocks/auth/{mod,service}.rs`, `blocks/auth_ui/oauth/callback.rs`, `blocks/dev/data_snapshot.rs`

**Interfaces:**
- Produces: `user_roles::{TABLE, UserRoleRow, Assigned, list_for_user, list_for_users, list_all, list_by_role, get, assign, rename_role, remove}`.

- [ ] **Step 1: Failing tests.** Codec round trip (`assign` then `list_for_user`, `assigned_at` present, `assigned_by` as given); `assign` twice is `Created` then `AlreadyAssigned` with one row; `list_for_users` buckets two users in one query; `rename_role` moves a row.
- [ ] **Step 2: Implement.** `assign` = existence check by `(user_id, role)` then `create` with `assigned_at = now`.
- [ ] **Step 3: Callers.** `auth/mod.rs` (`get_user_roles`, `ensure_admin_role` — the `USER_ROLES_TABLE` import goes), `auth/service.rs` tests, `callback.rs` (insert deleted, `db`/`json_map` imports pruned if unused), `admin/iam.rs` (`cascade_role_rename`, `handle_list_user_roles`, `handle_assign_role`, `handle_remove_role`, tests), `admin/ops.rs::fetch_roles`, `admin/mod.rs` (`USER_ROLES_TABLE` in `collections`/`grants`/`grant_tests` → `user_roles::TABLE`), `iam.rs` loses the constant, `data_snapshot.rs`.
- [ ] **Step 4: Run** the Task 1 set.
- [ ] **Step 5: Commit** `refactor(core): platform_state::user_roles owns the user_roles table`.

---

### Task 6: The audit rule

**Files:**
- Modify: `scripts/audit-wrap-grants.sh`, `blocks/dev/{seed,data_snapshot}.rs` (pragmas)

- [ ] **Step 1: Prove the gap.** Add a temporary file `src/blocks/tickets/probe.rs` with `pub async fn p(ctx: &dyn Context) { let _ = crate::platform_state::variables::list_all(ctx).await; }` (not declared in `mod.rs`; the script reads files, not the module tree). Run `bash scripts/audit-wrap-grants.sh`: it passes — the gap.
- [ ] **Step 2: Implement.** Phase 1: index `crates/impresspress-core/src/platform_state/*.rs` `pub const TABLE` values into `PLATFORM_TABLE[module]`. Phase 3, after the `db::*` walk: grep `platform_state::[a-z_]+::` under `$BLOCKS_DIR` (templates excluded), resolve the module to its table, dedupe on `(caller, table)`, honour the two pragmas, `check_coverage` as for a `db::*` call, report under the same headings.
- [ ] **Step 3: Prove the rule.** Re-run with the probe: `MISSING grants (1): .../tickets/probe.rs: impresspress/tickets → impresspress__admin__variables (owned by impresspress/admin)`, exit 1. Delete the probe; re-run: OK. Paste both outputs into the PR body.
- [ ] **Step 4: Commit** `ci(audit): a platform_state call from a block is a database access on that table`.

---

### Task 7: Core consumers — delete the old names, the door test

**Files:**
- Delete: `crates/impresspress-core/src/admin_schema.rs`
- Modify: `lib.rs`, `blocks/admin/{mod,settings,logs}.rs` (re-exports), `builder/registration.rs` (`ADMIN_BLOCK_ID`), `tests/admin/migrations_002_variables_block.rs`, `tests/dev_export.rs`, `tests/dev_data_snapshot.rs`
- Create: `crates/impresspress-core/tests/repo_door.rs`

- [ ] **Step 1: The door test, failing.** `tests/repo_door.rs`: `crate_sources()` as in `blocks/products/tests/repo_door_test.rs`; for each of the five `(module, literal)` pairs, the literal scan over comment-stripped sources allows only `platform_state/<module>.rs` and `blocks/admin/migrations/mod.rs` (its tests assert on index names derived from the table names); the identifier scan for `<module>::TABLE` allows only the listed files with a reason each. Run it: it fails on `admin_schema.rs` and the `blocks::admin` re-export lines.
- [ ] **Step 2: Delete** `admin_schema.rs`, the three re-export sites, `admin::WRAP_GRANTS_TABLE`, `iam::USER_ROLES_TABLE`; switch the three integration tests; `registration.rs:84,94` use `crate::blocks::admin::ADMIN_BLOCK_ID`.
- [ ] **Step 3:** `grep -rn 'blocks::admin::\|admin_schema' crates/*/src crates/*/tests` shows only `ADMIN_BLOCK_ID`, `migrations`, `AdminBlock` and the auth/auth-ui block ids. Door test green.
- [ ] **Step 4: Commit** `refactor(core): delete admin_schema.rs and the admin table re-exports; door test for platform_state`.

---

### Task 8: The stale admin grants

**Files:**
- Modify: `blocks/admin/mod.rs`

- [ ] **Step 1: Failing test** in `grant_tests`: admin's `info().grants` contains no row `(wafer-run/auth, variables::TABLE)` and none `(impresspress/userportal, block_settings::TABLE)`. Fails on the current table.
- [ ] **Step 2: Remove** the two rows. `bash scripts/audit-wrap-grants.sh` still passes (no `db::*` or `platform_state::` reference under `blocks/auth/` names the variables table; none under `blocks/userportal/` names block_settings — it reads `BLOCK_SETTINGS_CONFIG_KEY`).
- [ ] **Step 3: Commit** `fix(admin): drop the two WRAP grants nothing reads through`.

---

### Task 9: Cloudflare adapter

- [ ] `grep -n 'blocks::admin::\|impresspress_core::boot\|impresspress_core::features::load' crates/impresspress-cloudflare/src/*.rs` shows only `ADMIN_BLOCK_ID`/`migrations`/`AdminBlock`.
- [ ] `env CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner cargo test -p impresspress-cloudflare --target wasm32-unknown-unknown` green.
- [ ] No commit unless the grep or the run finds something (then fix and commit `refactor(cloudflare): …`).

### Task 10: Web crate

- [ ] Same grep over `crates/impresspress-web/src`; `cargo check -p impresspress-web --target wasm32-unknown-unknown` (say in the PR body if the crate does not build locally).

### Task 11: CLI

- [ ] Same grep over `crates/impresspress/src` and `crates/impresspress/tests`; `cargo test -p impresspress`; `cargo clippy -p impresspress --all-targets` (four pre-existing lints; add none).

### Task 12: Verify and open the PR

- [ ] The full verification list from Global Constraints, in order. Both snapshot directories byte-identical (`git status --short crates/impresspress-core/tests/snapshots` is empty).
- [ ] PR body: module-by-module list of moved functions (old → new); every consumer file changed per crate; the audit rule and its fixture proof; the stale grants removed; the door-test allowlist with reasons; test runs per crate; deviations (`boot.rs` deleted; `updated_by` on `VariableRow`; `block` derived on every variable insert; `upsert_by_key` non-atomic for dev; the typed request-log readers); out-of-scope observations (self-declared grants at runtime; `settings.rs`'s `ADMIN_BLOCK_NAME` literal; `Page<T>` for PR 4).
- [ ] `bash <scratchpad>/push-and-pr.sh "refactor(core): platform_state owns variables, block settings, WRAP grants, request logs and user roles" <body-file>`. Do not merge.
