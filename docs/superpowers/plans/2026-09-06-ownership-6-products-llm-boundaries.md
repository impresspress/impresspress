# Ownership and repo boundaries, PR 6: products repo boundaries; llm reads messages through `call_block`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two boundaries that leak today. Inside products, `pages.rs` reaches past `repo/` at seven sites, three of which are verbatim copies of queries `handlers/sellers.rs` already runs, and four table constants live in `handlers/mod.rs` where no door can own them. Across blocks, `llm/pages.rs` reads the messages block's two tables directly with `db::list` — which is why `messages_schema.rs` exists, why `messages/mod.rs` declares two WRAP grants, and why `block-llm = []` can claim llm compiles without messages while every chat turn it writes goes through `call_block`. After this PR every products table has exactly one door under `repo/`, and the llm chat page reads messages the same way it already writes them.

**Architecture:** `repo/seller_accounts.rs` grows `list_contracts`/`get_contract` (the typed reads `handlers/sellers.rs` and `pages.rs` each hand-rolled); `repo/products.rs` grows `list_owned_by` (promoting `sellers.rs`'s private `owned_by`/`seller_live_products`); four new repo modules — `groups`, `types`, `group_templates`, `product_templates` — take the constants out of `handlers/mod.rs` and own the queries against them. `llm/repo/settings.rs` owns `SETTINGS_TABLE` and a typed `ThreadSettingRow`, and `get_thread_setting`'s `.ok()` becomes a propagated error: a config read that fails must not silently fall back to the default model. `llm/pages.rs` loses both `db::list` calls to `messages_list_contexts` and the existing `messages_list`, both over `ctx.call_block("impresspress/messages", ..)` with `util::block_request`; `messages_schema.rs` is deleted, its two constants go back to `messages/service.rs`, the two grants go, and `block-llm = ["block-messages"]` states what `llm.requires` already declares.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e`, `wafer_core::clients::database`, `serde_json`, `maud`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-09-06-ownership-and-repo-boundaries-design.md`, sections 2.5 and 2.6, inventory 1.5 and 1.6, tests 3.2 "PR 6". PRs 1 (`#19`), 2 (`#20`), 3 (`#21`), 4 (`#22`) and 5 (`#23`) are merged; the branch is cut from `origin/main` at `c8d3dd10`. Every line reference below was re-resolved against that tree.

## Verified against the tree before planning (the spec's claims, re-checked)

1. **Inventory 1.5 is exact, line for line.** `pages.rs:216` `db::count(GROUPS_TABLE)`, `:224` `db::count(repo::offers::TABLE)`, `:712` `db::list_all(repo::seller_accounts::TABLE)` + `to_contract`, `:831` `db::get(repo::seller_accounts::TABLE)` + `to_contract`, `:842-850` the `owner_id` filter, `:2346` `db::list(GROUPS_TABLE)`, `:3011` `db::count(PURCHASES_TABLE)` by `user_id`; `handlers/stats.rs:32` `db::count(GROUPS_TABLE)`; `handlers/mod.rs:44,47,50,53` the four constants; `handlers/sellers.rs:36` `owned_by`, `:43` `seller_live_products`, `:60` `list`, `:76` `get`; `repo/seller_accounts.rs:62` `to_contract`, `:101` `get_for_user`. Nothing moved.
2. **Two claims in 1.5 are incomplete, not wrong.** (a) `pages.rs:842-850` is already `repo::products::list_all(..)` with a hand-built `owner_id` filter, not a raw `db::*` call — so the seventh "direct `db::*`" site is six; `list_owned_by` still removes the duplicated filter. (b) `handlers/sellers.rs` has a *third* `db::get(seller_accounts::TABLE)`, at `:178` in `set_suspended`, which the inventory does not list. It takes `get_contract` too, so no raw read of that table survives outside the door.
3. **Inventory 1.6 is right in substance; four of its line references drifted.** Correct as written: `llm/mod.rs:201` `SETTINGS_TABLE`, `:355` the delete, `llm/pages.rs:387` `list_all(..).unwrap_or_default()`, `llm/contracts.rs:372` `ThreadOverrideView`, `llm/mod.rs:547-548` `requires`, `Cargo.toml:111` `block-llm = []`, `messages/mod.rs:215-218` the two grants, `messages/rest.rs:57-60` the owner-scoped list. Drifted: `resolve_provider`'s reads are `mod.rs:294-311` (spec: 301-311); `get_thread_setting` is `mod.rs:336-355` with the `.ok()` at `:354` (spec: 333-344); the two `call_block` helpers are `mod.rs:219-274` (spec: 228-274); **`util::block_request` is `util.rs:291-304`, not `243-267`** — `util.rs` grew in PRs 1-5 and 243-267 is now `format_timestamp`/`stamp_created`.
4. **The grant deletion is sound at rev `7d47e5e`, and the spec's reasoning is slightly off while its conclusion holds.** `RuntimeContext::check_resource_access` authorizes `self.caller_id`, not `self.node_id` (`context.rs:486-494`, and the doc comment at `:477-485` says so explicitly). It is reached on the *database service block's* context: a block calling `db::list` dispatches `call_block("wafer-run/database", ..)` (`wafer-core/src/clients/database.rs:37`), and `dispatch_call` builds that sub-context with `caller_id: Some(self.node_id.clone())` (`context.rs:302-309`). So when `MessagesBlock` runs under llm's `call_block`, its own `node_id` is `impresspress/messages` (`:295-302`) and its database reads are authorized as `impresspress/messages`. `wrap::check_access` Rule 3 (`wafer-block/src/wrap.rs:277-283`) admits a caller that owns the resource, and `resource_owner("impresspress__messages__contexts")` is `impresspress/messages`. The grants are therefore dead the moment the direct read goes. The spec's shorthand "`call_block` attributes the callee as itself" is true of `node_id`; what matters is that `node_id` becomes the `caller_id` of the database sub-context one level down.
5. **The behaviour change is smaller than it looks, and the visual baseline does not move.** `ui/assets/llm-chat.js:368` already creates threads through `POST /b/messages/api/contexts` (owner derived server-side, `rest.rs:74`), and `:416`/`:261` already read entries through `GET /b/messages/api/contexts/{id}/entries?kind=message` — which is owner-scoped through `owned_record` (`rest.rs:139`). So every thread a user can reach from the chat UI was created under their own id already; the SSR sidebar was the one surface listing everyone's. `crates/impresspress-web/tests/e2e/visual-baseline.spec.ts:47,147` screenshots `/b/llm/`, and nothing in the repository seeds a messages context, so that page renders "No threads yet." before and after.
6. **The entries read gains a `kind=message` filter it did not have.** `pages.rs:136` lists every entry of the thread; the existing `messages_list` appends `?kind=message` (`mod.rs:256`). The llm block only ever writes `kind: "message"` entries (`mod.rs:232`), and `llm-chat.js:261,416` already reads the filtered list, so the rendered set is the same — but it is a narrowing, and it is stated rather than assumed.
7. **No consumer outside the repository's own Rust reads any surface this PR touches.** Grepped `crates/`, `packages/impresspress-js/src` and `test`, `examples/` (including `examples/tests/*.spec.ts`), `crates/impresspress-web/tests` (Playwright), `docs/`, and the blocks' embedded JS under `src/**/assets/*.js`: the products SSR pages appear only in `visual-baseline.spec.ts` and `products-{catalog-admin,seller-governance}.spec.ts` (page paths, unchanged); the SDK's `extensions.service.ts:1268-1273` calls `GET`/`POST /b/products/api/admin/groups`, whose handlers are untouched; `llm-chat.js` calls `/b/messages/api/contexts*` and `/b/llm/api/chat*`, all unchanged; nothing outside `impresspress-core/src` names `impresspress__messages__*` or `impresspress__llm__*`.
8. **`dev.tools.json` is unaffected.** It is generated from the WebMCP tool declarations, and no `.tool(..)` row changes here.

## Decisions taken while planning (recorded, not re-litigated)

1. **All four `handlers/mod.rs` constants move, and each gets a real repo module.** `groups` (with `count`, `list_by_name`, `get`), `types`, `group_templates` and `product_templates` (each with the queries its handlers actually run). `default_template_id(ctx, table)` — a generic helper that took the table as a parameter — is deleted and becomes `group_templates::default_id` / `product_templates::default_id`, one per table, which is what the door convention means.
2. **The group/type CRUD that runs through `blocks/crud.rs` keeps naming `repo::<m>::TABLE`, and those call sites go on the door's IDENT allowlist with that reason.** `crud::{list_page, create_record, update_record, delete_record, verify_owner, get_owned, update_owned, delete_owned}` are a shared pass-through whose table comes from the caller — the same shape that made `crud.rs` carry an `// audit-allow-file:` pragma for the WRAP audit. Folding them into per-table repo functions changes the HTTP error mapping they encapsulate and is a separate PR; recorded in carry-forward, not smuggled in here.
3. **The crate-level `tests/repo_door.rs` absorbs the three scans in `blocks/products/tests/repo_door_test.rs`.** That file's header already says the crate-level gate "generalises" it and that "the block repos join it one PR at a time". Keeping two allowlists for one table is the failure mode the door is meant to prevent. `repo_door_test.rs` keeps what is unique to it: the write-side gate over `update_including_deleted`/`purge` call sites, and `the_old_products_table_const_is_gone`.
4. **A messages-block boundary test, not a messages door.** The spec asks for products + the llm settings table. Proving llm no longer names the messages tables needs a gate, but a full `contexts`/`entries` door would have to allowlist `messages/rest.rs`, whose `owned_record`/`crud::delete_record` calls are genuine queries taking the table as a parameter — a standing exemption bought for nothing. Instead: `the_messages_tables_are_named_only_inside_the_messages_block`, which is exactly the claim this PR makes and needs no per-file allowlist.
5. **`ThreadSettingRow` is the typed row; `ThreadOverrideView: From<&ThreadSettingRow>` replaces `from_record`.** `from_record` had one caller shape (`ConfigUpdateResponse::Override`) and one test; the row type is now the only thing that reads a settings record's columns.
6. **`resolve_provider` propagates the settings read error.** `get_thread_setting`'s `.ok()` is the bug named in 2.6; propagating it means `resolve_provider` returns `Result<(String, String), WaferError>` and its callers map the failure to a 500. Falling back to the default provider/model on a database outage silently bills a thread's traffic to the wrong backend.
7. **`messages_list_contexts` returns `Result<Vec<ContextView>, WaferError>`; entries stay on the existing `messages_list`.** That is spec 2.6 verbatim. `ContextView { id, title, updated_at }` is the sidebar's whole requirement, so the page's render helpers take it instead of `db::Record` and become independent of the wire envelope. The entries list keeps `Vec<serde_json::Value>` because `chat.rs` shares it and its `Option`/swallow discipline is T4 (Phase 3, spec 6 non-goals).

## Global Constraints

- Both snapshot gates byte-identical: this PR declares no endpoint, changes no auth level and changes no published schema. `dev.tools.json` unchanged.
- No change to wafer-run (rev `7d47e5e`). No migration, no `.sql` file, no schema change.
- Core only: no crate outside `impresspress-core` is touched, plus the Cargo feature edge checked with `cargo check -p impresspress-core --no-default-features --features block-llm`.
- No raw SQL outside test-fixture setup.
- TDD: write the test, run it, see it fail for the expected reason, then implement, then see it pass. Commits carry the two trailer lines:
  `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Verification before the PR: `cargo +nightly fmt --all -- --check`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`; `cargo test -p impresspress-core --no-fail-fast` (known unrelated failure `lockfile_loads_remote_block`); `cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot`; `cargo check -p impresspress-core --no-default-features --features block-llm`; `bash scripts/audit-wrap-grants.sh`. `prepared_plan.rs` and the CLI are untouched, so neither the wasm suite nor the CLI suite is required.
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase2/products-llm-boundaries`.

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `.../blocks/products/repo/groups.rs` | `TABLE` (from `handlers/mod.rs:44`), `count`, `list_by_name`, `get`. |
| `.../blocks/products/repo/types.rs` | `TABLE` (from `handlers/mod.rs:47`). |
| `.../blocks/products/repo/group_templates.rs` | `TABLE` (from `handlers/mod.rs:50`), `list_by_name`, `default_id`. |
| `.../blocks/products/repo/product_templates.rs` | `TABLE` (from `handlers/mod.rs:53`), `default_id`. |
| `.../blocks/products/repo/seller_accounts.rs` | `+ list_contracts`, `+ get_contract`. |
| `.../blocks/products/repo/products.rs` | `+ list_owned_by`. |
| `.../blocks/products/handlers/{mod,sellers,stats,group,types,product}.rs` | No table constant, no `db::*` on a table another module owns. |
| `.../blocks/products/pages.rs` | No `db::*` at all. |
| `.../blocks/llm/repo/settings.rs` | `SETTINGS_TABLE`, `ThreadSettingRow`, `find_for_thread`, `list_all`, `insert`, `update`, `delete`. |
| `.../blocks/llm/pages.rs` | Reads threads and entries through `call_block`; no `db::*`. |
| `.../blocks/llm/contracts.rs` | `ContextView`; `ThreadOverrideView: From<&ThreadSettingRow>`. |
| `.../blocks/messages/service.rs` | Owns `CONTEXTS_TABLE`/`ENTRIES_TABLE` again as plain `pub const`. |
| `.../src/messages_schema.rs` | Deleted. |
| `.../tests/repo_door.rs` | Every products table, `llm::settings`, and the messages-boundary test. |

---

## Task list

- [ ] **Task 1 — the plan.** This file.
- [ ] **Task 2 (RED→GREEN): the products doors.** Extend `tests/repo_door.rs` with every products table and the allowlists that must stay; watch it fail on `handlers/mod.rs`, `pages.rs`, `handlers/sellers.rs` and `handlers/stats.rs`; then add the four repo modules, `list_contracts`, `get_contract`, `list_owned_by`, and move every consumer.
- [ ] **Task 3: the products repo tests.** `list_contracts`/`get_contract` equal the handler's former output for the same rows; `groups::count` and the stats endpoint surface a `FailingDbOpContext` failure instead of rendering 0.
- [ ] **Task 4 (RED→GREEN): `llm/repo/settings.rs`.** `ThreadSettingRow` round-trip; `resolve_provider` on a failing settings read returns `Err` rather than the default model.
- [ ] **Task 5 (RED→GREEN): llm reads messages through `call_block`.** `messages_list_contexts`; `pages.rs` over it and `messages_list`; delete `messages_schema.rs`; the two constants back to `messages/service.rs`; delete the two grants; `block-llm = ["block-messages"]`.
- [ ] **Task 6: the llm and messages door entries.** `llm::settings` joins `TABLES`; `the_messages_tables_are_named_only_inside_the_messages_block`.
- [ ] **Task 7: verification and the PR.**
