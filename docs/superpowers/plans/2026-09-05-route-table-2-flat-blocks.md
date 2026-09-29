# Route table single source, PR 2: messages, vector, legalpages, tickets, dev

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate the five blocks whose dispatch is already a flat `EndpointRoute` table (`messages`, `vector`, `legalpages`, `tickets`, `dev`) so that each block's `const ROUTES` is the one description of its HTTP surface: it dispatches requests **and** generates `info().endpoints` through `endpoint_match::declare`. No declared path changes its auth level or schema; both per-block snapshots stay byte-identical. Every remaining hand-written path read in these blocks (`util::path_param`, `strip_prefix("/b..")`, the `crud::*` prefix-fallback readers) is replaced by `msg.var(..)` on the variable the matcher bound.

**Architecture:** PR 1 made `EndpointRoute<H>` carry the auth level, summary, description, schema producers, tags, deprecation and agent-tool a `BlockEndpoint` carries, with `public` / `authenticated` / `admin` constructors and `const fn` builders, and added `declare(&ROUTES) -> Vec<BlockEndpoint>` plus the two schema producers `request_schema_of::<T>` (deserialize contract: bodies, path params, query params) and `response_schema_of::<T>` (serialize contract: responses). Each block in this PR rewrites its rows with the metadata copied verbatim from its hand-written `info()` list, replaces `.endpoints(vec![..])` with `.endpoints(endpoint_match::declare(ROUTES))`, and reads path variables only through `msg.var`. Handler tests that build a message by hand and expect an id are routed through the block's real table by a `routed(..)` helper, as `llm` does.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e` (`BlockEndpoint`, `AuthLevel`, `HttpMethod`), `schemars` 1, `serde_json`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md` (this plan implements its "PR 2"). Model: `docs/superpowers/plans/2026-09-05-route-table-1-core-llm-system.md`.

## Global Constraints

- No change to wafer-run, `routing.rs`, `endpoint_match.rs`, or any block other than the five named. `EndpointRoute::new` stays for the tables not yet migrated (products, files, admin, auth_ui, userportal).
- Every `crates/impresspress-core/tests/snapshots/<block>.openapi.json` and `<block>.endpoints.json` is byte-identical at the end of every task. `UPDATE_OPENAPI_SNAPSHOTS=1` is never run in this PR. A diff means a row is wrong: compare the row with the deleted `info()` entry for the same path and fix the row.
- Every row names its auth level through `EndpointRoute::public`, `::authenticated` or `::admin`, exactly as the old `info()` list declared it. An endpoint the old list left unmarked was `Public` (the upstream `BlockEndpoint` default) and becomes `::public`. Nothing is tightened or loosened.
- Metadata is copied verbatim: summary, description, tags, `.deprecated()`, `.agent_tool(..)`. `.input::<T>()` becomes `.input(request_schema_of::<T>)`; `.output::<T>()` becomes `.output(response_schema_of::<T>)`; `.path_params::<T>()` / `.query_params::<T>()` become `.path_params(request_schema_of::<T>)` / `.query_params(request_schema_of::<T>)`; `.path_params_schema(f())` / `.query_params_schema(f())` / `.input_schema(f())` / `.output_schema(f())` become `.path_params(f)` / `.query_params(f)` / `.input(f)` / `.output(f)`; an inline `serde_json::json!({..})` schema moves into a named `fn xyz_schema() -> serde_json::Value` and the row names it.
- Table order is the dispatch order (specific templates before generic ones). Where the old `info()` order differs it does not matter: both snapshot tests sort.
- After each block: `grep -rn 'path_param(\|strip_prefix("/b\|starts_with("/b\|dispatch_path(' crates/impresspress-core/src/blocks/<block>` prints nothing outside test-only string assertions.
- `blocks/crud.rs`: the helpers products relies on (`path_id`, `get_owned`, `update_owned`, `crud_delete`, `crud_delete_owned`, `OwnedResource`) are not changed. The migrated blocks compose the id-taking primitives that already exist (`get_record`, `update_record`, `delete_record`, `verify_owner`, `read_json_body`), so no new helper is needed. The one edit to `crud.rs` is its module doc, which enumerates callers by block and would otherwise be false after this PR.
- TDD: write the test, run it and see it fail for the expected reason, then implement. Each task ends with a commit carrying the two trailer lines:
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Format with `cargo +nightly fmt -p impresspress-core`. Lint with `cargo clippy -p impresspress-core --all-targets -- -D warnings`. `cargo test -p impresspress-core --no-fail-fast` has one known unrelated failure, `lockfile_loads_remote_block`; every other test must pass.
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase1/route-table-flat-blocks` (created from `origin/main` at `480d6c96`). The session's shell guard refuses compound commands containing `git` or shell variables; those go in a script under the scratchpad directory and run with `bash <script>`.

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `crates/impresspress-core/src/blocks/messages/mod.rs` | `ROUTES` carries the metadata `info()` listed; four named schema fns replace the inline `json!` schemas; `info()` calls `declare(ROUTES)`; `table_tests` and a `test_support::routed`. |
| `crates/impresspress-core/src/blocks/messages/rest.rs` | Handlers read `{id}` through one `owned_record` helper (bound id, ownership check); the `CONTEXTS_PREFIX` / `ENTRIES_PREFIX` constants and the `crud_get_owned` / `crud_delete_owned` prefix readers go. |
| `crates/impresspress-core/src/blocks/messages/pages.rs` | `context_detail_page` reads `msg.var("id")`. |
| `crates/impresspress-core/src/blocks/vector/mod.rs` | `ROUTES` carries the metadata; `info()` calls `declare(ROUTES)`; `table_tests`. |
| `crates/impresspress-core/src/blocks/vector/pages.rs` | `delete_index` reads `msg.var("name")`; `extract_index_and_id` reads only the bound variables; the direct-call delete tests route through the table. |
| `crates/impresspress-core/src/blocks/vector/test_support.rs` | Gains `routed(msg)`. |
| `crates/impresspress-core/src/blocks/legalpages/mod.rs` | `ROUTES` carries the metadata; `info()` calls `declare(ROUTES)`; the JSON get/update/delete arms compose `crud` primitives on the bound id; `API_DOC_PREFIX` goes; `table_tests`. |
| `crates/impresspress-core/src/blocks/tickets/mod.rs` | `ROUTES` carries the metadata; `info()` calls `declare(ROUTES)`; the lockstep test becomes the table test. |
| `crates/impresspress-core/src/blocks/dev/mod.rs` | `ROUTES` carries the metadata (including descriptions); the workspace `info()` calls `declare(ROUTES)`; `table_tests`. |
| `crates/impresspress-core/src/blocks/crud.rs` | Module doc's caller list corrected (comment only). |
| `docs/superpowers/plans/2026-09-05-route-table-2-flat-blocks.md` | This plan. |

---

### Task 0: Commit this plan

- [ ] **Step 1: Commit**

```
docs: plan for phase 1 PR 2 (messages, vector, legalpages, tickets, dev)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 1: `messages` (11 rows)

**Files:**
- Modify: `crates/impresspress-core/src/blocks/messages/mod.rs` (imports, `ROUTES`, `info()` endpoints, tests)
- Modify: `crates/impresspress-core/src/blocks/messages/rest.rs` (prefix constants, seven id-reading handlers, tests)
- Modify: `crates/impresspress-core/src/blocks/messages/pages.rs` (`context_detail_page`)

**Old surface (from `info()`), which the rows reproduce exactly:** two `Admin` pages (`GET /b/messages/`, `GET /b/messages/contexts/{id}`, tag `ui`); nine `Authenticated` API endpoints tagged `contexts` / `entries`; `GET /b/messages/api/contexts` carries a hand-written query schema and output schema, `GET /b/messages/api/contexts/{id}` a hand-written path schema, `GET /b/messages/api/contexts/{id}/entries` a hand-written query schema; `POST .../contexts`, `PATCH .../contexts/{id}`, `POST .../{id}/entries` carry `.input::<T>()`.

- [ ] **Step 1 (RED): table test.** Add `mod table_tests` to `mod.rs` with `info_endpoints_come_from_the_table` (length equal, and per `zip` pair method, path and auth equal). Run `cargo test -p impresspress-core --lib blocks::messages::table_tests`. Expected: FAIL on the third row's `auth` (`ROUTES` rows are `EndpointRoute::new`, so `Admin`, while `info()` declares `Authenticated` for `GET /b/messages/api/contexts`; the first two pairs happen to agree).
- [ ] **Step 2 (GREEN): rows and `declare`.** Move the four inline schemas into `list_contexts_query_schema`, `context_list_schema`, `context_id_path_schema`, `list_entries_query_schema`; rewrite `ROUTES` with `::admin` / `::authenticated`, summaries, descriptions, tags and schemas copied from `info()`; replace `.endpoints(vec![..])` with `.endpoints(endpoint_match::declare(ROUTES))`; drop the `BlockEndpoint` / `AuthLevel` imports. Run the table test: PASS.
- [ ] **Step 3: snapshot gates.** `cargo test -p impresspress-core --test openapi_snapshot --test endpoint_surface`. Expected: PASS, no diff.
- [ ] **Step 4 (RED): a handler must not parse the path.** In `rest.rs` tests add `handlers_read_only_the_bound_id`: a `get_context` call on an unrouted `auth_msg("retrieve", "/b/messages/api/contexts/ctx-1", "user-a")` must answer 400, and the same message routed through `ROUTES` (via a new `test_support::routed` in `mod.rs`) must reach the row. Run `cargo test -p impresspress-core --lib blocks::messages::rest::tests::handlers_read_only_the_bound_id`. Expected: FAIL, the unrouted call answers 404 because `crud::crud_get_owned`'s prefix fallback still recovers `ctx-1` from the path.
- [ ] **Step 5 (GREEN): remove the path reads.** In `rest.rs` add `owned_record(ctx, msg, table, label)` (bound id or 400, then `crud::verify_owner`); route `get_context`, `update_context`, `delete_context`, `list_entries`, `add_entry`, `get_entry`, `delete_entry` through it (`delete_entry` then calls `crud::delete_record`); delete `CONTEXTS_PREFIX`, `ENTRIES_PREFIX`, the `util::path_param` import. In `pages.rs` replace `path_param(msg, "id", "/b/messages/contexts/")` with `msg.var("id")` and drop the import. Run `cargo test -p impresspress-core --lib blocks::messages`. Expected: PASS (the existing rest tests dispatch through `MessagesBlock::handle`, so their ids were always bound).
- [ ] **Step 6: gates, format, lint, commit.** Grep gate for `blocks/messages` prints nothing. `cargo +nightly fmt -p impresspress-core`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`; both snapshot tests again. Commit:

```
refactor(messages): declare the HTTP surface from the route table

`ROUTES` now carries the summaries, tags, auth levels and schemas that
`info()` listed by hand, and `info()` is `declare(ROUTES)`. Handlers read
`{id}` only as the table bound it: the `path_param` prefix fallback, the
two prefix constants and the `crud_*_owned` prefix readers go, replaced
by one `owned_record` helper over `crud::verify_owner`. OpenAPI and
endpoint-surface snapshots unchanged.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 2: `vector` (11 rows)

**Files:**
- Modify: `crates/impresspress-core/src/blocks/vector/mod.rs` (imports, `ROUTES`, schema fn docs, `info()` endpoints, tests)
- Modify: `crates/impresspress-core/src/blocks/vector/pages.rs` (module doc, `delete_index`, `extract_index_and_id`, tests)
- Modify: `crates/impresspress-core/src/blocks/vector/test_support.rs` (add `routed`)

**Old surface:** two `Admin` pages; nine `Authenticated` API endpoints with `.input::<T>()` / `.output::<T>()` and the two existing hand-written path schemas (`index_name_path_schema`, `vector_id_path_schema`).

- [ ] **Step 1 (RED): table test** in `mod.rs` (`VectorBlock::new().info()`). Run `cargo test -p impresspress-core --lib blocks::vector::table_tests`. Expected: FAIL on the third row's `auth`.
- [ ] **Step 2 (GREEN): rows and `declare`.** Rewrite `ROUTES`; `.endpoints(endpoint_match::declare(ROUTES))`; drop `AuthLevel` / `BlockEndpoint` imports; update the `index_name_path_schema` doc (`msg.var("name")`). Table test: PASS.
- [ ] **Step 3: snapshot gates.** Both PASS, no diff.
- [ ] **Step 4 (RED): remove the path reads.** In `pages.rs` change `delete_index` to `msg.var("name")` and `extract_index_and_id` to `(msg.var("index"), msg.var("id"))`; drop the `util::path_param` import; rewrite the module's "Route ordering note" and the `extract_index_and_id` doc. Run `cargo test -p impresspress-core --lib blocks::vector::pages::contract_tests::upsert_and_deletes_acknowledge`. Expected: FAIL: the test builds its two delete messages by hand and never ran dispatch, so `delete_index` now answers `InvalidArgument: index name is required` and `output_json` panics.
- [ ] **Step 5 (GREEN): route the test.** Add `routed(msg)` to `test_support.rs` (dispatches through `crate::blocks::vector::ROUTES`, panics if no row matches); wrap both delete messages. Add `delete_routes_bind_their_path_vars` (routed `DELETE /b/vector/api/indexes/docs` binds `name`; routed `DELETE /b/vector/api/docs/a` binds `index` and `id`; an unrouted message binds nothing). Run `cargo test -p impresspress-core --lib blocks::vector`. Expected: PASS.
- [ ] **Step 6: gates, format, lint, commit.**

```
refactor(vector): declare the HTTP surface from the route table

`ROUTES` now carries the summaries, auth levels and schemas that `info()`
listed by hand, and `info()` is `declare(ROUTES)`. `delete_index` and
`delete_single` read `{name}` / `{index}` / `{id}` only as the table bound
them; the `path_param` fallback and the `strip_prefix` split go. The
direct-call delete test runs its messages through the real table first.
OpenAPI and endpoint-surface snapshots unchanged.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 3: `legalpages` (17 rows)

**Files:**
- Modify: `crates/impresspress-core/src/blocks/legalpages/mod.rs` (imports, `ROUTES`, `API_DOC_PREFIX`, `handle_admin_publish`, three new `handle_admin_get/update/delete`, `info()` endpoints, `handle` arms, tests)
- Modify: `crates/impresspress-core/src/blocks/crud.rs` (module doc only)

**Old surface:** `GET /b/legalpages/terms` and `GET /b/legalpages/privacy` unmarked, so `Public`; the fifteen admin pages, mutations and JSON endpoints `Admin`. Summaries only, no schemas.

- [ ] **Step 1 (RED): table test** in `mod.rs` (`LegalPagesBlock::new().info()`). Expected: FAIL on the first row's `auth` (`new` declares `Admin`; `info()` declares `Public` for `/b/legalpages/terms`).
- [ ] **Step 2 (GREEN): rows and `declare`.** The two public rows use `EndpointRoute::public`, everything else `::admin`; summaries verbatim. `.endpoints(endpoint_match::declare(ROUTES))`; drop `BlockEndpoint` and the inner `AuthLevel` import. Table test: PASS.
- [ ] **Step 3: snapshot gates.** Both PASS, no diff.
- [ ] **Step 4 (RED): a handler must not parse the path.** Add `publish_reads_only_the_bound_id`: `handle_admin_publish` on an unrouted `admin_msg("update", "/b/legalpages/api/documents/doc-7/publish")` against `TestContext::with_auth()` must answer `InvalidArgument`; routed through `ROUTES` the message binds `doc-7`. Expected: FAIL, the fallback recovers `doc-7` and the handler proceeds to the database instead of refusing.
- [ ] **Step 5 (GREEN): remove the path reads.** `handle_admin_publish` reads `msg.var("id")` through a shared `document_id(msg) -> Result<&str, OutputStream>`; add `handle_admin_get`, `handle_admin_update`, `handle_admin_delete` composing `crud::get_record`, `crud::read_json_body` + `crud::update_record`, `crud::delete_record` on the bound id; the three `handle` arms call them; delete `API_DOC_PREFIX`. Correct the `crud.rs` module doc's caller list. Run `cargo test -p impresspress-core --lib blocks::legalpages`. Expected: PASS.
- [ ] **Step 6: gates, format, lint, commit.**

```
refactor(legalpages): declare the HTTP surface from the route table

`ROUTES` now carries the summaries and auth levels `info()` listed by
hand, and `info()` is `declare(ROUTES)`. The JSON document handlers read
`{id}` only as the table bound it, composing the id-taking `crud`
primitives directly; `API_DOC_PREFIX` and the `path_param` fallback go.
OpenAPI and endpoint-surface snapshots unchanged.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 4: `tickets` (21 rows)

**Files:**
- Modify: `crates/impresspress-core/src/blocks/tickets/mod.rs` (imports, `ROUTES`, `info()` endpoints, tests)

**Old surface:** three `Public` (`GET /submit`, `GET /submitted`, `POST /api/submissions` with `.input::<PublicSubmissionRequest>()` / `.output::<SubmissionAck>()`); six `Admin` pages; twelve `Admin` JSON endpoints with `.query_params::<T>()`, `.path_params_schema(id_path_schema())`, `.input::<T>()`, `.output::<T>()`. Handlers already read `msg.var("id")`; no path read to remove. `ENDPOINT_REFERENCE` (the admin reference page's table) is not a path read and stays.

- [ ] **Step 1 (RED): table test.** Replace `route_and_endpoint_contracts_stay_in_lockstep` (method + path, any order) with `info_endpoints_come_from_the_table` (method, path, auth, table order). Expected: FAIL on the first row's `auth`.
- [ ] **Step 2 (GREEN): rows and `declare`.** Rewrite `ROUTES`; `.endpoints(endpoint_match::declare(ROUTES))`; drop `BlockEndpoint` and the inner `AuthLevel` import. Run `cargo test -p impresspress-core --lib blocks::tickets` (also `only_three_http_endpoints_are_public`, which reads the declared order). Expected: PASS.
- [ ] **Step 3: snapshot gates.** Both PASS, no diff. Grep gate prints nothing.
- [ ] **Step 4: integration tests, format, lint, commit.** `cargo test -p impresspress-core --test tickets --test tickets_http`. Commit:

```
refactor(tickets): declare the HTTP surface from the route table

`ROUTES` now carries the summaries, auth levels and schemas `info()`
listed by hand, and `info()` is `declare(ROUTES)`. Handlers already read
`{id}` as the table bound it. The lockstep test becomes the table test.
OpenAPI and endpoint-surface snapshots unchanged.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 5: `dev` (19 rows, `--features block-dev`)

**Files:**
- Modify: `crates/impresspress-core/src/blocks/dev/mod.rs` (imports, `ROUTES`, `info()` endpoints, tests)

**Old surface:** nineteen `Admin` endpoints; the page and its three assets carry no schema; the JSON endpoints carry `.input::<T>()`, `.output::<T>()`, `.query_params::<T>()`, `.path_params::<T>()`; five carry descriptions. There is no `.agent_tool(..)` and no `.tags(..)` in this block's `info()` (the task brief expected some; `dev.endpoints.json` confirms no `tool=` line), and `GET /b/dev/api/tools.json` deliberately carries none. `runtime_only` declares no endpoints and keeps doing so. Handlers already read `msg.var("id")` / `msg.var("name")`; no path read to remove.

- [ ] **Step 1 (RED): table test** in `mod.rs` tests, building `DevBlock::with_workspace(DevShared::new(FakeControl::new(), Arc::new(FakeShell::new())))`. Every row and every declaration is `Admin` and the two lists are the same set in the same order, so method, path and auth already agree; the test therefore compares each pair's summary as well, which is what proves the rows carry the declaration rather than merely matching it. Run `cargo test -p impresspress-core --features block-dev --lib blocks::dev::tests::info_endpoints_come_from_the_table`. Expected: FAIL on the first row, `/b/dev`: declared summary `"The workspace document"`, row summary `""`, while the rows carry no metadata.
- [ ] **Step 2 (GREEN): rows and `declare`.** Rewrite `ROUTES` with `::admin`, summaries, descriptions and schemas verbatim; `base.endpoints(endpoint_match::declare(ROUTES))` in the workspace branch; drop `AuthLevel` / `BlockEndpoint` imports; move the three explanatory comments (no schema on the page and assets, no `.agent_tool` on tools.json, no `.output` on export) onto the rows. Table test: PASS.
- [ ] **Step 3: snapshot gates.** `cargo test -p impresspress-core --features block-dev --test openapi_snapshot --test endpoint_surface`. Both PASS, no diff. Grep gate prints nothing.
- [ ] **Step 4: dev suite, format, lint, commit.** `cargo test -p impresspress-core --features block-dev --no-fail-fast` (everything except `lockfile_loads_remote_block`). `cargo clippy -p impresspress-core --features block-dev --all-targets -- -D warnings`. Commit:

```
refactor(dev): declare the HTTP surface from the route table

`ROUTES` now carries the summaries, descriptions and schemas `info()`
listed by hand, and the workspace `info()` is `declare(ROUTES)`; an
exported bundle still declares nothing. Handlers already read `{id}` and
`{name}` as the table bound them. OpenAPI and endpoint-surface snapshots
unchanged.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 6: Verify and open the PR

- [ ] **Step 1: full verification**

```
cargo +nightly fmt --all -- --check
cargo clippy -p impresspress-core --all-targets -- -D warnings
cargo test -p impresspress-core --no-fail-fast
cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot
cargo test -p impresspress-core --features block-dev --no-fail-fast
grep -rn 'path_param(\|strip_prefix("/b\|starts_with("/b\|dispatch_path(' crates/impresspress-core/src/blocks/{messages,vector,legalpages,tickets,dev}
git status --short
git diff origin/main --stat -- crates/impresspress-core/tests/snapshots/
```

Expected: fmt clean; clippy clean; all tests pass except `lockfile_loads_remote_block`; grep prints nothing; working tree clean; no snapshot file differs from `origin/main`.

- [ ] **Step 2: push and open the PR** with `bash <scratchpad>/push-and-pr.sh "refactor(blocks): declare messages, vector, legalpages, tickets and dev from their route tables" <body-file>`. Body: per-block row count and what moved, both snapshot gates byte-identical, the grep-gate output, the tests routed through the table, deviations, trailer.

---

## Self-review

**Spec coverage (PR 2 scope):** section 3 (one table, wire paths, `msg.var` only) for the five blocks: Tasks 1–5. Section 5 "Blocks" testing bullet: each block gets `info_endpoints_come_from_the_table`; the blocks whose handlers change get a binding test through the real table. Section 6 item 2: snapshots byte-identical, checked in every task and in Task 6.

**Deviations recorded:** `crud.rs` receives a doc-comment correction rather than an additive helper, because the id-taking primitives it already exposes are what the migrated blocks compose; the three `Message`-reading one-liners that lose their callers (`crud_get`, `crud_update`, `crud_get_owned`) stay for PR 6/7. The `dev` block has no agent tools or tags to carry, contrary to the task brief.
