# Route table single source, PR 7: router cleanup

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close phase 1. Every block now declares its whole HTTP surface from one table (PRs #12–#17), so the router-side machinery those declarations replaced goes: `Route::router_declared_public`, the `router_final` field and `is_router_final`, the ten per-path carve-outs (eight `/b/auth/...`, `/b/static/`, `/b/products/webhooks`), the `router_final` branches in `route_to_block` and `effective_access`, and the three prefix entries whose tier the blocks' rows now fully express (`/b/admin/settings`, `/b/legalpages/admin`, `/b/legalpages/api`). `PreparedRoute.router_final` goes with the plan schema bumped to 2. `EndpointRoute::new` and `dispatch_path` go from the matcher. The prefix table ends as one prefix per block plus the inspector proxy, hand-written, with a two-way test against `blocks::all_block_infos()` that keeps it honest. Every deletion is proven redundant by a test that passes on both sides of it.

**Architecture:** The access decision in `route_to_block` becomes unconditionally `route.access.max(declared_access(..))`, with `declared_access`'s fail-closed `Authenticated` default for an undeclared path; `effective_access` mirrors it without the `router_final` arm. A carve-out was a *prefix* the router admitted for any method; a declaration is an exact `(method, template)` row. The three things that used to be kept public by a carve-out (a hashed asset, an OAuth callback or reset link, the Stripe webhook) are now public because `system`, `auth-ui` and `products` declare them public, and the router reads the declaration. Nothing in a block changes.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e` (`BlockEndpoint`, `AuthLevel`, `HttpMethod`, `BlockInfo`), `serde` / `serde_json`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`, `wasm-bindgen-test` for the Cloudflare crate.

**Spec:** `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md` (section 4 "Router" and sequencing item 7). Inputs: the six merged plans under `docs/superpowers/plans/2026-09-05-route-table-*.md` and the carry-forward notes from PRs #12–#17.

## Decisions taken while planning (recorded, not re-litigated)

1. **Tests before deletions, in git.** Task 1 writes every redundancy test against the *current* table and commits it green; Task 3 deletes and the same tests stay green. The two commits are the "passes on both sides" evidence. The prepared-plan change (Task 2) lands between them so `Route::is_router_final` and `PreparedRoute.router_final` are deleted together with their one caller, and no commit carries a transient `router_final: false` literal.
2. **The prefix table is kept hand-written.** Sequencing item 7 left "derived or kept" open. Kept: a derived table would need the same one-off decisions the hand-written one records (the inspector proxy, `/health` outside `/b/`, files' two prefixes, slash-suffixed vs bare spellings), and deriving it would make `routing.rs` depend on every block module. The two-way test is what makes the hand-written table honest.
3. **The inspector proxy is the one entry the two-way test exempts.** `wafer_block_inspector::InspectorBlock::info()` declares no `BlockEndpoint`s (it is `.infrastructure()`), its `BlockInfo` is not in `all_block_infos()`, and its per-request gating is its own `AccessPolicy` on top of the `Admin` prefix. The test names the exemption: exactly one entry's block is absent from `all_block_infos()`, it is `/b/inspector`, and it is the one `Route::proxy`.
4. **The table becomes order-independent, and a test says so.** After the deletions no entry's prefix is served by another entry's, so the "order matters, most-specific-first" doc comment and the two ordering tests (`legalpages_admin_routes_require_admin`'s positions check, `router_declared_public_routes_precede_their_general_prefix`) are replaced by one pairwise-disjointness test that fails before the deletion and passes after.
5. **A carve-out admitted a prefix; a declaration admits a template. That tightening is pinned.** `Route::router_declared_public("/b/products/webhooks", ..)` matched `starts_with`, so an anonymous `GET /b/products/webhooks` or `POST /b/products/webhooks/x` reached the block (which 404'd it); likewise `GET /b/auth/api/verify/extra` and `GET /b/static/a/b`. After this PR the router denies them before dispatch (undeclared → `Authenticated`). No consumer used those shapes (PR 6 already narrowed the block to the declared spelling; the auth-ui and system tables are exact). The new test is red before the deletion and green after — the deletion's own red test.
6. **An undeclared path under a former `Admin` prefix entry now falls to `Authenticated`, and that is by design.** `GET /b/legalpages/admin/does-not-exist` for a logged-in non-admin was 403 (prefix tier `Admin`); it is now admitted to the block, whose table dispatch answers 404. Spec section 4 states the rule; the legalpages and admin `handle` closures read nothing from the path before `endpoint_match::dispatch`. Pinned by a test in `routing.rs` and by the rewrite of `extra_routes_test::undeclared_path_falls_back_to_prefix_tier`.
7. **`EndpointRoute::new` test callers become `EndpointRoute::admin`**, the level `new` declared, so the matcher tests keep the same rows; `dispatch_ignores_row_metadata` already proves the level is irrelevant to matching.
8. **`PreparedRoute.refine_undeclared` loses `#[serde(default)]`.** Its comment said the default let a plan exported before the field existed import; every such plan is version 1 and the version gate now rejects it, so the default would only let a hand-edited version-2 plan omit a field every exporter writes. The field itself stays (spec: keep `refine_undeclared`).
9. **The review report is committed.** `docs/CODE_REVIEW_2026-09-05.md` is cited by the spec as its origin and by the brief as the file to update, but it exists only as an untracked file in the main checkout (byte-identical to the scratchpad copy) and in no git ref. It is added to the repository in its own commit with section 7a updated, beside the earlier committed reviews, so the reviewer can drop that commit if the report was meant to stay out of the tree.
10. **`docs/2026-08-28-webmcp-handoff.md` keeps its `router_final` mention.** It is a dated handoff describing the code on that date, not a living document.

## Global Constraints

- Both snapshot gates byte-identical for every block: `crates/impresspress-core/tests/snapshots/*.openapi.json` and `*.endpoints.json`. This PR declares nothing new. `UPDATE_OPENAPI_SNAPSHOTS=1` is never run.
- No change to wafer-run (rev `7d47e5e`), to any block's `ROUTES` table, or to any block's `info()`. Block files change only in doc comments that describe the router (`blocks/system.rs`, `blocks/admin/mod.rs`).
- The spec's success criteria 2 and 3 are greps pasted into the PR body: `grep -rn 'starts_with("/b\|strip_prefix("/b\|path_param(' crates/impresspress-core/src/blocks/` prints only test-only string assertions; `grep -n 'router_declared_public\|router_final' crates/impresspress-core/src/routing.rs` prints nothing.
- TDD: write the test, run it and see it fail for the expected reason (or, for a redundancy test, see it pass on the current table and again after the deletion), then implement. Commits carry the two trailer lines:
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Verification: `cargo +nightly fmt --all -- --check`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`; `cargo test -p impresspress-core --no-fail-fast` (known unrelated failure `lockfile_loads_remote_block`); `cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot`; because `prepared_plan.rs` changes, `env CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner cargo test -p impresspress-cloudflare --target wasm32-unknown-unknown`; because the CLI exports plans, `cargo test -p impresspress` and `cargo clippy -p impresspress --all-targets` (four pre-existing lints on main; add none).
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase1/route-table-router-cleanup` (from `origin/main` at `bdb2625c`, the merge of PR #17). The session's shell guard refuses compound commands containing `git` or shell variables; those go in a script under the scratchpad directory and run with `bash <script>`.

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `crates/impresspress-core/src/routing.rs` | `Route { prefix, access, block, dispatch_to }` with `new` and `proxy` only; `ROUTES` = 14 entries (one prefix per block, `/health` and `/b/static/` for system, `/b/storage/` and `/b/cloudstorage/` for files, the inspector proxy); `route_to_block` and `effective_access` always `access.max(declared)`; doc comments no longer describe `router_final`; `declared_access` and `route_to_block` get the doc comments that were sitting on the wrong item. Tests: the redundancy tests (Task 1), the two-way table test and the disjointness test (Task 3), the rewrites named in the brief. |
| `crates/impresspress-core/src/prepared_plan.rs` | `PREPARED_RUNTIME_PLAN_SCHEMA_VERSION = 2`; `PreparedRoute` without `router_final`, `refine_undeclared` required; tests for the version gate and the round trip. |
| `crates/impresspress-core/src/builder/prepared.rs` | Export mapping without `router_final`. |
| `crates/impresspress-core/src/endpoint_match.rs` | `dispatch` owns the slash retry (no `dispatch_path`, no `dispatch_exact`); no `EndpointRoute::new`; module docs describe the three template forms and the decode step on `dispatch`. |
| `crates/impresspress-core/src/util.rs` | `url_path_decode`'s doc names `endpoint_match::dispatch` only (it already does; the sentence about products is confirmed gone). |
| `crates/impresspress-core/src/blocks/system.rs` | `webmcp_script_asset_is_publicly_reachable`'s doc comment describes the declaration, not the carve-out. |
| `crates/impresspress-core/src/blocks/admin/mod.rs` | `ROUTES` doc comment names the `/b/admin/` prefix only. |
| `crates/impresspress-core/tests/extra_routes_test.rs` | `undeclared_path_falls_back_to_prefix_tier` rewritten to the fail-closed `Authenticated` rule. |
| `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md` | Status "Implemented (PRs #12-#17 and this one)"; short "As built" section at the end. |
| `docs/CODE_REVIEW_2026-09-05.md` | Committed; section "7a. Phase 0 and 1 status" with the phase 1 table. |
| `docs/superpowers/plans/2026-09-05-route-table-7-router-cleanup.md` | This plan. |

## The prefix table, before and after

Before (26 entries): `/health`; `/b/static/` (carve-out); `/b/inspector` (proxy); eight auth-ui carve-outs (`/b/auth/oauth/callback`, `/b/auth/api/oauth/sync-user`, `/b/auth/api/oauth/providers`, `/b/auth/reset-password`, `/b/auth/api/reset-password`, `/b/auth/api/forgot-password`, `/b/auth/api/verify`, `/b/auth/api/resend-verification`); `/b/auth/`; `/b/admin/settings` (Admin); `/b/admin/`; `/b/storage/`; `/b/cloudstorage/`; `/b/products/webhooks` (carve-out); `/b/products`; `/b/tickets`; `/b/legalpages/admin` (Admin); `/b/legalpages/api` (Admin); `/b/legalpages`; `/b/userportal`; `/b/messages`; `/b/llm`; `/b/vector/`.

After (14 entries): `/health` Public system; `/b/static/` Public system; `/b/inspector` Admin proxy; `/b/auth/` Public auth-ui; `/b/admin/` Admin admin; `/b/storage/` Public files; `/b/cloudstorage/` Public files; `/b/products` Public products; `/b/tickets` Public tickets; `/b/legalpages` Public legalpages; `/b/userportal` Public userportal; `/b/messages` Public messages; `/b/llm` Public llm; `/b/vector/` Public vector.

Why each deletion is redundant (the row that carries the level today): the static carve-out by `GET /b/static/{filename} public` (system); the eight auth-ui carve-outs by the nine `public` rows PR #14 added (`auth_ui.endpoints.json`); the webhook carve-out by `POST /b/products/webhooks public`; `/b/admin/settings` by the `/b/admin/` entry itself (already `Admin`) and by admin's five `GET /b/admin/settings/... admin` rows; `/b/legalpages/admin` and `/b/legalpages/api` by legalpages' fifteen `admin` rows under those prefixes (`legalpages.endpoints.json`: every row under `/admin` and `/api` is `admin`; the only `public` rows are `/terms` and `/privacy`).

---

### Task 0: Commit this plan

- [ ] **Step 1: Commit**

```
docs: plan for phase 1 PR 7 (router cleanup)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 1: Prove every deletion redundant on the current table

All tests in `routing.rs::tests`, driving `route_to_block` with `DispatchProbeBlock` and the real `info()` of the block concerned (or `blocks::all_block_infos()`), against the table *as it is today*. Each must pass now and again after Task 3.

- [ ] **Step 1: Rewrite the three carve-out tests to pass the owning block's info**
  - `anonymous_static_asset_request_is_not_denied`: `block_infos = vec![SystemBlock::new().info()]`; comment says the declaration `GET /b/static/{filename} public` is what admits the request.
  - `webmcp_script_asset_is_publicly_reachable`: same info, same URL from `assets::webmcp_js_url()`.
  - `stripe_webhook_carveout_stays_reachable_with_no_session` becomes `stripe_webhook_stays_reachable_with_no_session`: `vec![ProductsBlock::new().info()]`, anonymous `POST /b/products/webhooks` dispatches. `stripe_webhook_is_public_from_the_products_declaration_alone` keeps the `endpoint_auth` / `declared_access` resolution asserts and drops its dispatch half (now owned by the sibling); its doc loses the "today the carve-out still short-circuits" sentence.
- [ ] **Step 2: Add the auth-ui end-to-end tests**
  - `auth_ui_session_less_paths_dispatch_anonymously_from_the_declaration`: for each of the nine `(action, path)` pairs, anonymous request with `AuthUiBlock::new().info()` → `DISPATCHED`.
  - `auth_ui_api_key_rows_need_a_session_from_the_declaration`: `update` and `delete` on `/b/auth/api/api-keys/k-1`: anonymous → `PermissionDenied`; `auth_msg(.., "user_1")` → `DISPATCHED`.
- [ ] **Step 3: Add the prefix-entry tests with `blocks::all_block_infos()`**
  - `admin_settings_paths_are_denied_without_the_admin_role`: `GET /b/admin/settings/`, `GET /b/admin/settings/email`, `GET /b/admin/settings/not-a-tab` for anonymous and `user_1` → `PermissionDenied`; `admin_msg` on the two declared ones → `DISPATCHED`.
  - `legalpages_admin_and_api_paths_are_denied_without_the_admin_role`: `GET /b/legalpages/admin`, `GET /b/legalpages/admin/terms`, `POST /b/legalpages/admin/save`, `GET /b/legalpages/api/documents`, `POST /b/legalpages/api/documents`, `PATCH /b/legalpages/api/documents/d-1`, `DELETE /b/legalpages/api/documents/d-1` for anonymous and `user_1` → `PermissionDenied`; `admin_msg` on each → `DISPATCHED`; anonymous `GET /b/legalpages/terms` → `DISPATCHED` (the public rows are not over-gated).
- [ ] **Step 4: Run** `cargo test -p impresspress-core --lib routing::` — every test above passes on the current table.
- [ ] **Step 5: Commit**

```
test(routing): prove the carve-outs and the three prefix entries redundant

Every path a router carve-out or an Admin prefix entry kept reachable or
gated resolves to the same level from the owning block's declaration
alone, driven through route_to_block with the real info(). These pass on
the current table and must keep passing when the entries are deleted.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 2: Prepared plan schema 2 without `router_final`

- [ ] **Step 1: Write the failing tests in `prepared_plan.rs::tests`**
  - `a_version_1_plan_is_rejected_at_import`: serialize a valid plan to a `Value`, set `schema_version = 1`, `from_json` → `UnsupportedSchema { actual: 1, expected: 2 }`. (Red today: a plan hash mismatch, not the version gate.)
  - `a_plan_exported_with_router_final_is_rejected_at_import`: same, plus `router_final: false` on every route → `PreparedPlanError::Json` naming `router_final`. (Red today: version 1 and the field are both accepted.)
  - `a_version_2_plan_round_trips_and_carries_no_router_final`: `PREPARED_RUNTIME_PLAN_SCHEMA_VERSION == 2`; the plan's JSON has no `router_final` under any route; `from_json(to_json_pretty())` equals the plan.
- [ ] **Step 2: Run** `cargo test -p impresspress-core --lib prepared_plan::` — the three fail as described.
- [ ] **Step 3: Implement**: constant 1 → 2; delete `PreparedRoute.router_final`; drop `#[serde(default)]` and its comment from `refine_undeclared` (the version gate is the compatibility boundary); `builder/prepared.rs` mapping loses both `router_final` lines and the "gated by `router_final` above" comment; delete `Route::is_router_final` (caller-free) and the `refines_undeclared` doc sentence that cites it; fixture in `prepared_plan.rs::tests::structure()` loses the field.
- [ ] **Step 4: Run** the three tests, then `cargo test -p impresspress-core --lib prepared_plan:: builder::prepared::`, then `cargo test -p impresspress`, `cargo clippy -p impresspress --all-targets`, and the wasm suite.
- [ ] **Step 5: Commit**

```
refactor(plan): schema 2 drops PreparedRoute.router_final

A plan exported by an older build is rejected at import by the version
gate rather than read with a field missing.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 3: Delete `router_final`, the carve-outs and the three prefix entries

- [ ] **Step 1: Write the red tests** (fail on the current table, pass after the deletion)
  - `routing.rs`: `a_declaration_admits_its_template_where_a_carve_out_admitted_a_prefix`: with the real system, auth-ui and products infos, anonymous `GET /b/products/webhooks`, `POST /b/products/webhooks/extra`, `GET /b/auth/api/verify/extra`, `GET /b/static/a/b` → `PermissionDenied` (today: `DISPATCHED` through the prefix carve-outs).
  - `routing.rs`: `an_undeclared_path_under_a_former_admin_prefix_entry_falls_to_authenticated`: `GET /b/legalpages/admin/does-not-exist` with `all_block_infos()`: anonymous → `PermissionDenied`; `user_1` → `DISPATCHED` (today: `PermissionDenied` from the `Admin` prefix entry); comment: the block's table dispatch answers 404.
  - `routing.rs`: `prefix_entries_are_pairwise_disjoint`: for every ordered pair of distinct entries, `!route_prefix_matches(a.prefix, b.prefix)` (today: the carve-outs sit under `/b/auth/`, `/b/products`, and `/b/admin/settings` under `/b/admin/`).
  - `routing.rs`: `every_declared_endpoint_sits_under_its_blocks_prefix_and_every_prefix_is_declared_against`: direction 1, for every `info` in `all_block_infos()` and every `ep`, the first `ROUTES` entry whose prefix matches `ep.path` names `info.name` as `block` or `dispatch_to`; direction 2, for every entry, the block it names declares at least one endpoint under the prefix, except the one `Route::proxy` (`/b/inspector`), whose block is absent from `all_block_infos()` by design. (Passes on both sides; it is the guard the brief asks for.)
  - `extra_routes_test.rs`: `undeclared_path_falls_back_to_prefix_tier` becomes `undeclared_path_falls_back_to_authenticated_not_the_prefix_tier`: anonymous `GET /b/legalpages/api/documents` with the hand-rolled `legalpages_infos()` → 403; `user-1` → 200 dispatched (today 403 from the prefix entry).
- [ ] **Step 2: Run** those tests and see them fail for the stated reasons.
- [ ] **Step 3: Delete** in `routing.rs`: the `router_final` field and its initialisers; `Route::router_declared_public`; the nine carve-out entries (the static one becomes `Route::new(STATIC_PREFIX, RouteAccess::Public, "impresspress/system")`); the `/b/admin/settings`, `/b/legalpages/admin`, `/b/legalpages/api` entries; the `router_final` arms in `route_to_block` and `effective_access`. Rewrite the `Route` struct doc, the `ROUTES` doc (order no longer matters; the disjointness test pins it), the entry comments, the `ExtraRoute` and `declared_access` doc sentences that cite `router_final` / `router_declared_public`, the `effective_access` doc, and move the two doc comments that sit on the wrong item (`declared_access`'s onto `declared_access`, `route_to_block`'s onto `route_to_block`).
- [ ] **Step 4: Rewrite the remaining tests**: `effective_access_agrees_with_the_router_for_a_router_final_route` → `effective_access_agrees_with_the_router_for_a_declared_public_row_under_a_public_prefix` (the real system info's `/b/static/{filename}` row: resolver says `Public`, router dispatches anonymously; and a synthetic `Admin` row under the same prefix: resolver says `Admin`, router denies — the direction `router_final` used to override); `auth_ui_declares_every_path_the_router_carves_out` → `auth_ui_declares_its_nine_session_less_and_two_api_key_paths` (keep the eleven assertions, drop the carve-out set comparison); delete `static_prefix_route_is_router_declared_public`, `router_declared_public_routes_precede_their_general_prefix`, `legalpages_admin_routes_require_admin`; `undeclared_products_path_other_than_the_webhook_carveout_requires_auth` → `undeclared_products_path_requires_auth`; `non_admin_routes_dont_require_admin` uses `STATIC_PREFIX` instead of the stale `"/static/"` and gains `/b/legalpages`; `route_table_maps_expected_paths` gains `/b/admin/settings/email` and `/b/legalpages/admin/terms`. Doc comments in `blocks/system.rs` (`webmcp_script_asset_is_publicly_reachable`) and `blocks/admin/mod.rs` (`ROUTES`).
- [ ] **Step 5: Run** `cargo test -p impresspress-core --lib routing::`, `--test extra_routes_test`, and `grep -n 'router_declared_public\|router_final' crates/impresspress-core/src/routing.rs` (nothing).
- [ ] **Step 6: Commit**

```
refactor(routing): delete router_final and the carve-outs; one prefix per block

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 4: `endpoint_match` cleanup

- [ ] **Step 1: Grep** `EndpointRoute::new\b` and `dispatch_path` across `crates/` — only `endpoint_match.rs` (its own definition and its tests) may appear.
- [ ] **Step 2: Delete** `EndpointRoute::new` and its `new_declares_admin` test; the 17 test callers use `EndpointRoute::admin`. Delete `dispatch_path`; fold `dispatch_exact` into `dispatch` (exact pass, then the trailing-slash retry, same comment). Rewrite the module docs (`dispatch` decodes bound variables; the products split is gone; the three template forms `{name}`, `{name...}`, `{name...}/`); `match_template`'s and `endpoint_auth`'s docs name `dispatch`. Confirm `util::url_path_decode`'s doc names only `endpoint_match::dispatch`.
- [ ] **Step 3: Run** `cargo test -p impresspress-core --lib endpoint_match::` — the slash-retry tests and every other matcher test pass unchanged.
- [ ] **Step 4: Commit**

```
refactor(endpoint_match): delete EndpointRoute::new and dispatch_path

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 5: Spec and review report

- [ ] **Step 1: Spec**: `**Status:** Implemented (PRs #12-#17 and this one)`; append `## As built` with one line per deviation and its PR: system's one-row asset table (#12); schema producers through the upstream builders (#12); `endpoint_auth`'s trailing-slash retry (#12); `{name...}/` (#15); files' thirteen rows and `{key...}` (#15); products' alias narrowing (#17); this PR's carve-out-to-template tightening and the `Authenticated` fallback under the former `Admin` prefix entries. Design sections untouched.
- [ ] **Step 2: Review report**: add `docs/CODE_REVIEW_2026-09-05.md` from the scratchpad copy; rename 7a to "7a. Phase 0 and 1 status" and add the phase 1 table (#12–#17 and this PR) in the same shape as the phase 0 table.
- [ ] **Step 3: Commit** (two commits: `docs(spec): ...` and `docs: commit the 2026-09-05 review report with the phase 1 status`).

---

### Task 6: Verification and PR

- [ ] **Step 1: Run** the full verification list from Global Constraints. Snapshot gates: `git status --short crates/impresspress-core/tests/snapshots/` is empty.
- [ ] **Step 2: Greps** for the PR body (spec success criteria 2 and 3).
- [ ] **Step 3: Ship** with `push-and-pr.sh "refactor(routing): delete router_final and the carve-outs; one prefix per block" <body-file>`. Do not merge.
