# Ownership and repo boundaries, PR 5: a legalpages repo layer; publish is the only status transition (B10)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `impresspress__legalpages__documents` gets the one door every other table in this crate now has. `repo/documents.rs` owns the name, the columns and the row shape; `mod.rs`, `service.rs` and `pages.rs` stop issuing `db::*` and stop building column maps. On the way through, the three ways this block currently loses a write are closed by construction — that is B10: a generic PATCH that applies `status` and so bypasses `service::publish_document`, a save handler that turns a lookup *error* into "create a new draft", and an Init seed that re-runs on a count error. Two more swallowed errors on the publish path go with them: `latest_version` returning 0 on any failure (so the next publish silently restarts at version 1) and `archive_published` ignoring both its list error and each update error (leaving a half-archived doc type).

**Architecture:** `repo/documents.rs` owns `pub const TABLE`, `DocumentRow` (one `from_record`), `NewDraft`, and the functions spec 2.4 lists. The five copies of the `(doc_type, status)` filter block become two private constructors, `of_type` and `of_type_with_status`. `service.rs` keeps the publish-then-archive ordering but over `Result`-returning repo calls, so a publish either completes or reports. `Route::ApiUpdate` deserialises a typed `UpdateDocumentRequest { title?, content? }` with `deny_unknown_fields` and calls `update_content`, which makes `status` and `version` unreachable from PATCH and leaves `publish_document` the only status transition in the block. `contracts::DocumentView`/`DocumentListView` keep the `Record`/`RecordList` envelope the JSON endpoints have always published, built from the typed rows — the same shape `files::contracts::RecordListView` publishes for the same reason.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e`, `wafer_core::clients::database`, `serde_json`, `schemars`, `maud`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-09-06-ownership-and-repo-boundaries-design.md`, section 2.4, inventory 1.4, tests 3.2 "PR 5" and 3.1. PRs 1 (`#19`), 2 (`#20`), 3 (`#21`) and 4 (`#22`) are merged; the branch is cut from `origin/main` at `5e41ac6e`. The spec's line references predate those merges and are re-resolved below.

## Verified against the tree before planning (the spec's claims, re-checked)

1. **Inventory 1.4's site list is right; every line number in it is one low for `mod.rs` and exact for the other two files.** `COLLECTION` is at `mod.rs:190` (spec: 189). The `mod.rs` accesses are `:241` (`db::list`, public page), `:330` (`db::list`, admin list), `:378` (`db::get`, publish), `:413` (`crud::get_record`), `:435` (`crud::update_record`), `:447` (`crud::delete_record`), `:454` (`db::count`, seed) — the spec's `240,329,377,412,434,446,453`. Three of those seven are `crud::*` calls that take `COLLECTION` as a parameter rather than `db::*` calls, which the spec's prose ("`db::*` at …") does not say; the count of 17 sites naming the constant is nevertheless exactly right (4 `db::*` + 3 `crud::*` in `mod.rs`, 6 in `service.rs`, 4 in `pages.rs`). `service.rs:78,88,121,146,156,179` and `pages.rs:51,78,506,543` resolve verbatim.
2. **The five filter copies are where the spec says.** `mod.rs:220-239` (`doc_type` + `status=published`, sort `version` desc, limit 1), `service.rs:133-145` (`doc_type`, sort `version` desc, limit 1), `service.rs:159-170` (`doc_type` + `status=published`, `list_all`), `pages.rs:31-50` (`doc_type` + `status=draft`, sort `updated_at` desc, limit 1), `pages.rs:58-77` (`doc_type` + `status=published`, sort `updated_at` desc, limit 1).
3. **B10's three defects all reproduce.** `mod.rs:144-149` declares `PATCH /b/legalpages/api/documents/{id}` → `Route::ApiUpdate` → `handle_admin_update` (`:421-439`), which reads a `HashMap<String, Value>` and hands it to `crud::update_record` (`crud.rs:147-165`) — every key in the body becomes a column, `status` included. `pages.rs:506-515` matches `db::get`'s `Err(_)` to `should_create_new = true`. `mod.rs:454` is `db::count(..).await.unwrap_or(0)`, and `seed_defaults` returns `()`, so the lifecycle at `:723-725` cannot see a failure.
4. **The two extra swallowed errors are real.** `service.rs:132-151` `latest_version` is `db::list(..).await.ok().and_then(..).unwrap_or(0)`; `service.rs:155-184` `archive_published` returns `()` and both its `if let Ok(records)` and the per-row `if let Err(e) = db::update(..)` drop their failures.
5. **Legalpages has an endpoint-surface snapshot and no OpenAPI snapshot, and it declares no schema at all today.** `tests/snapshots/legalpages.endpoints.json` has 17 lines; `SNAPSHOTTED_BLOCKS` (`tests/openapi_snapshot.rs:28-37`) does not list legalpages. **The spec's "the second diff is one reviewable line" does not hold.** Not one legalpages row calls `.input`, `.output`, `.path_params` or `.query_params`, so `BlockEndpoint::has_schema()` is false for all 17 and the block contributes *zero* paths to `/openapi.json`. Its baseline from the current tree is the literal `{}` — confirmed by running the generator — and `openapi_matches_committed_snapshots` refuses to write an empty snapshot for a block that is not in `LEGITIMATELY_EMPTY`. The baseline commit therefore adds legalpages to both lists, and the `.input` commit removes it from `LEGITIMATELY_EMPTY` again; the second diff is the whole PATCH path object, every line of it produced by the one `.input(request_schema_of::<UpdateDocumentRequest>)` call. Both diffs go in the PR body.
6. **No consumer outside this block reads any legalpages JSON endpoint.** `grep -rn legalpages` over the whole repository (`crates/`, `packages/`, `examples/` including `examples/tests/*.spec.ts`, `crates/impresspress-web/tests`, `docs/`, the block's own embedded JS) finds: the two public HTML pages in `examples/tests/{blog,dropship,saas}.spec.ts` (`GET /b/legalpages/{terms,privacy}`), `/ext/legalpages/{terms,privacy}` links in three example `index.html` files, `ui/nav_groups.rs:133` linking `/b/legalpages/admin/privacy`, `userportal/mod.rs:214` reading the block-enabled flag, and the block's own `EDITOR_JS`, which posts to `/b/legalpages/admin/{save,publish,render-preview}` and reads `doc_id`, `version`, `status` and `message` off the response. `packages/impresspress-js` does not mention legalpages at all. Nothing anywhere reads `/b/legalpages/api/documents*`.
7. **The spec missed a second reason the gate would have been vacuous: `test_support::real_block_infos()` does not list legalpages.** That function is the block list `discovery_json` builds the generated `/openapi.json` from, and its own doc comment says a block absent from it "never appears in the document at all — regardless of how correct its own schema declarations are". So adding legalpages to `SNAPSHOTTED_BLOCKS` and declaring the PATCH schema would together still have produced `{}`, and the gate would have passed forever reviewing nothing. Legalpages joins `real_block_infos()` in the baseline commit, behind the same `block-legalpages` feature gate the block carries, so the `{}` baseline means exactly "declares no schema" and nothing else. Adding it changes no other test: `endpoint_surface.rs` builds from `blocks::all_block_infos()`, which already had legalpages, and the fourteen `pipeline::discovery_tests` assert on named endpoints rather than on the block list.
8. **Legalpages declares no `collections(..)` and no `grants(..)`, and needs none.** It touches exactly one table, which it owns, so `scripts/audit-wrap-grants.sh` classifies every call as own-table access and reports nothing for the block. The repo move keeps the calls under `src/blocks/legalpages/`, so the audit's view of the block is unchanged. Absent `collections(..)` also means no non-test file has to name the table for a declaration — which is why the literal allowlist for this door ends at the door itself.

## Decisions taken while planning (recorded, not re-litigated)

1. **`list_page` takes `doc_type` and pagination, not spec 2.4's `(status?, doc_type?, page)`.** The one caller, `handle_admin_list`, filters on `doc_type` only and has no status filter on the wire. A `status` parameter no caller passes would be an unexercised code path in a door whose whole purpose is that every query is visible in one file; `find_published`, `find_latest_draft` and `list_published` already express every status-scoped query the block makes. Deviation recorded in the PR body.
2. **`find_published` sorts by `version` desc, unifying two queries that sorted differently.** `mod.rs`'s public page sorted the published rows by `version` desc; `pages.rs`'s editor fallback sorted them by `updated_at` desc. Under the invariant this PR establishes — publish is the only status transition, and it archives every published sibling — there is at most one published row per type, so the two sorts cannot disagree. `version` desc is kept because it is the one that answers "the latest published version" for legacy data that predates the invariant. A test pins that a type with two published rows resolves to the higher version on both surfaces.
3. **Every JSON body describes a complete row, and no extra round trip buys it.** `db::create` hands back the map it was given plus an id; `db::update` re-fetches. So today `POST /b/legalpages/api/documents` and a *new-document* publish answer with a partial `data` (no `published_at`, no `id` inside `data`) while an *existing-document* publish answers with the full row — an inconsistency inside one endpoint pair. The fix is not `crud::create_record`'s re-read but the insert naming every column: `insert_draft` writes all nine non-id columns except `published_at`, which is genuinely NULL for a document that has never been published, and `insert_published` (added beyond spec 2.4's list, decision refined during implementation) writes all ten. `DocumentRow::from_record` on the created record is then the row as stored. This adds `published_at` and an `id` key inside `data` to two admin-only response bodies with no consumer in the repository (item 6 above). Called out in the PR body rather than smuggled through.
   `insert_published` exists rather than "insert a draft, then publish it" because a failure between those two writes would leave a draft behind, and the Init seed's "is the table already seeded?" count would then never let the type reach a published document again — reintroducing, in a new shape, the bug this PR is closing.
4. **The `Record`/`RecordList` envelope stays.** `contracts::{DocumentView, DocumentListView}` build it from the typed rows, exactly as `files::contracts::{RecordView, RecordListView}` do. `DocumentRow` mirrors the table column-for-column so serializing it cannot drop a column, and a test asserts that column set against the migration's DDL.
5. **`service::doc_version` is deleted.** Its TEXT-tolerant read is `RecordExt::i64_field`, which `DocumentRow::from_record` already uses; with a typed `version: i64` on the row, both callers (`mod.rs`'s public page, `pages.rs`'s editor) read the field. The `unwrap_or(1)` both applied disappears with it: `version` is `NOT NULL DEFAULT 1` in both migrations, so the fallback was unreachable for any stored row, and the "no document published yet" placeholder keeps its explicit `1`.
6. **`seed_defaults` propagates the publish error too, not only the count error.** Spec 2.4 names the count. A publish that fails mid-seed leaves the deployment with one of the two documents and an `Ok(())` Init, which is the same class of silence the count error is; Init failing loudly is the point.
7. **`archive_published` failing after the new document is live still returns `Err`.** The publish-then-archive ordering is unchanged and still deliberate (archiving first would leave the type with no published version if the publish then failed). What changes is that the caller learns: the new document is live and an older sibling may still say `published`, which is a state an operator must see, not one to log at `warn` and answer 200 to.
9. **The door test's IDENT allowlist keeps one entry: `blocks/legalpages/mod.rs`.** Its tests name `documents::TABLE` only to aim `FailingDbOpContext` at it, so the injected fault lands on the query under test and not on some other table's. That is the same category `blocks/admin/pages/blocks.rs`, `blocks/files/quota.rs` and `blocks/files/cloud.rs` are already listed under. The LITERAL allowlist ends at `blocks/legalpages/repo/documents.rs`, the door itself.

## Global Constraints

- `crates/impresspress-core/tests/snapshots/legalpages.endpoints.json` byte-identical: this PR declares no endpoint and changes no auth level.
- `crates/impresspress-core/tests/snapshots/*.openapi.json` byte-identical for every block but legalpages, which gains a snapshot in two commits: the empty baseline from the current tree first, then the PATCH request schema. Both diffs in the PR body.
- No change to wafer-run (rev `7d47e5e`). No migration, no `.sql` file, no schema change.
- Core only: no crate outside `impresspress-core` is touched.
- No raw SQL outside test-fixture setup.
- TDD: write the test, run it, see it fail for the expected reason, then implement, then see it pass. Commits carry the two trailer lines:
  `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Verification before the PR: `cargo +nightly fmt --all -- --check`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`; `cargo test -p impresspress-core --no-fail-fast` (known unrelated failure `lockfile_loads_remote_block`); `cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot`; `bash scripts/audit-wrap-grants.sh`. `prepared_plan.rs` and the CLI are untouched, so neither the wasm suite nor `cargo test -p impresspress` is required.
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase2/legalpages-repo`.

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `crates/impresspress-core/src/blocks/legalpages/repo/mod.rs` | The repo module root and `Page<T> { rows, total, page, page_size }`, the block's one paginated return shape. |
| `crates/impresspress-core/src/blocks/legalpages/repo/documents.rs` | `TABLE`, `DocumentRow`, `NewDraft`, `of_type`/`of_type_with_status` (the two filter constructors) and every function. The only file in the block that calls `db::*`. |
| `crates/impresspress-core/src/blocks/legalpages/contracts.rs` | `UpdateDocumentRequest { title?, content? }` (`deny_unknown_fields`, `JsonSchema`) and `DocumentView`/`DocumentListView`, the published envelope. |
| `crates/impresspress-core/src/blocks/legalpages/mod.rs` | `COLLECTION` deleted. Handlers call repo functions; `handle_admin_update` deserialises `UpdateDocumentRequest`; `seed_defaults` returns `Result` and the lifecycle propagates it; the PATCH row carries `.input(request_schema_of::<UpdateDocumentRequest>)`. |
| `crates/impresspress-core/src/blocks/legalpages/service.rs` | Publish-then-archive over repo functions; `latest_version` and `archive_published` errors propagate; `doc_version` deleted. |
| `crates/impresspress-core/src/blocks/legalpages/pages.rs` | `COLLECTION` alias deleted; `find_current_doc` over `find_latest_draft`/`find_published`; `handle_save` matches `Ok(Some)`/`Ok(None)`/`Err`. |
| `crates/impresspress-core/tests/repo_door.rs` | The documents table joins the doors; one IDENT allowlist entry, justified. |
| `crates/impresspress-core/tests/openapi_snapshot.rs` | `SNAPSHOTTED_BLOCKS` gains `("legalpages", &["/b/legalpages"])`. |
| `crates/impresspress-core/tests/snapshots/legalpages.openapi.json` | New: `{}` in the baseline commit, the PATCH request schema after. |
| `docs/superpowers/plans/2026-09-06-ownership-5-legalpages-repo.md` | This plan. |

---

### Task 0: This plan

- [ ] Commit this file as the first commit on the branch.

### Task 1: The OpenAPI baseline, from the current tree

- [ ] Add `("legalpages", &["/b/legalpages"])` to `SNAPSHOTTED_BLOCKS` and `"legalpages"` to `LEGITIMATELY_EMPTY`, with the comment saying why the block is empty today and that the next commit takes it off the list.
- [ ] Generate and commit `tests/snapshots/legalpages.openapi.json` (`{}`) on its own, before any source change.

### Task 2: The three B10 regressions, red first

- [ ] `PATCH {"status":"published"}` on a draft of a type that already has a published document: assert 400 and exactly one published row for the type. Run it — it answers 200 and leaves two published rows.
- [ ] `handle_save` with `doc_id` set, on a `FailingDbOpContext("database.get", TABLE)`: assert 500 and no new row. Run it — it answers 200 and creates a draft.
- [ ] Init on a `FailingDbOpContext("database.count", TABLE)`: assert `Err` and an empty table. Run it — Init returns `Ok` and seeds two documents.
- [ ] Record all three failure outputs for the PR body.

### Task 3: `repo/documents.rs` and the door

- [ ] Unit-test `DocumentRow::from_record` (integer and TEXT `version`, absent `published_at` → `None`) and the column-set assertion against the migration DDL.
- [ ] Write `repo/mod.rs`, `repo/documents.rs` and `contracts.rs`'s two views; move every one of the 17 sites in `mod.rs`, `service.rs` and `pages.rs`; delete `COLLECTION`, the `pages.rs` alias and `doc_version`.
- [ ] Extend `tests/repo_door.rs` with `("documents", "impresspress__legalpages__documents", "documents::TABLE", "legalpages")` and the two allowlists. Run all five door tests green.

### Task 4: The swallowed publish errors, and B10 defects 2 and 3

- [ ] Tests: `publish_document` on a failing `latest_version` returns `Err` and leaves the previously published row published; a failing `archive_published` surfaces as `Err`; then Task 2's save-handler and seed tests go green.
- [ ] `latest_version` and `archive_published` return `Result`; `publish_document` uses `?`; `seed_defaults` returns `Result` and the lifecycle propagates it; `handle_save` matches `Ok(Some)`/`Ok(None)`/`Err`.

### Task 5: B10 defect 1 — `UpdateDocumentRequest`

- [ ] Tests: `PATCH {"title": ..}` updates title and content only; `PATCH {"status": ..}` and `PATCH {"version": ..}` are 400; Task 2's PATCH regression goes green.
- [ ] `Route::ApiUpdate` deserialises `UpdateDocumentRequest` and calls `documents::update_content`. No schema declared yet.

### Task 6: The declared PATCH schema

- [ ] Add `.input(request_schema_of::<UpdateDocumentRequest>)` to the PATCH row, drop legalpages from `LEGITIMATELY_EMPTY`, regenerate `legalpages.openapi.json`. One commit, one snapshot diff.
- [ ] Assert `endpoint_surface` is byte-identical.

### Task 7: Verification

- [ ] Full verification list; `legalpages.endpoints.json` and every other block's OpenAPI snapshot byte-identical; audit script green. PR body carries the three red-before outputs, both snapshot diffs, the repo function list, the consumer search and the door allowlist with its reason.
