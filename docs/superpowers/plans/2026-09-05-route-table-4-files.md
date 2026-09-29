# Route table single source, PR 4: files

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `files` so one `const ROUTES: &[EndpointRoute<Route>]` over wire paths in `blocks/files/mod.rs` is the block's only description of its HTTP surface: it dispatches every request and generates `info().endpoints` through `endpoint_match::declare`. Move the admin-side storage JSON API from the admin block's `call_block` delegation (wire paths `/b/admin/api/storage/...` and `/b/admin/api/cloudstorage/...`, rewritten to synthetic `/admin/storage/...` and `/admin/b/cloudstorage/...` before forwarding) under the prefixes the router already sends to files, as `admin` rows the router enforces from the declaration. Declare the user cloud-storage API and the three user storage-API paths the block served but never declared, at the level each handler already enforces. Delete the admin block's two delegation arms. Make a declared shape the matcher could not match (`{prefix...}/`) matchable.

**Architecture:** PR 1 made `EndpointRoute<H>` carry the declaration and added `declare`, `request_schema_of`, `response_schema_of`. `files` today has four path matchers: a `starts_with` guard chain in `mod.rs:141-249`, a nine-row `EndpointRoute::new` sub-table in `storage/mod.rs` reached for `/b/storage/api/*`, a `match (action, path)` in `cloud.rs` over both real (`/b/cloudstorage/...`) and synthetic (`/admin/b/cloudstorage/...`) paths, and a `match (action, path)` in `storage/admin.rs` over synthetic `/admin/storage/...` paths, plus prefix-strip readers in `storage/params.rs`, `share.rs` and `cloud.rs`. All of that becomes one 29-row table and handlers that read `msg.var(..)`. The admin block loses `AdminRoute::{StorageDelegate, CloudStorageDelegate}` and the `req.resource` rewrite.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e` (`BlockEndpoint`, `AuthLevel`, `HttpMethod`), `serde_json`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md` (this plan implements its "PR 4": section 3's **files** paragraph, sequencing item 4, and amends section 2 for the `{name...}/` shape). Models: the three earlier plans under `docs/superpowers/plans/2026-09-05-route-table-*.md`.

## Decisions taken while planning (recorded, not re-litigated)

The read-through found four places where the task as written could not be done; the coordinator decided each:

1. **Three more served-but-undeclared paths.** `storage/mod.rs`'s sub-table serves `GET /b/storage/api/search`, `GET /b/storage/api/recent` and `DELETE /b/storage/api/buckets/{name}`; none is declared and none was in the task's list. They have live SDK consumers (`packages/impresspress-js/src/services/storage.service.ts:122,204,218`), the handlers scope by user or owner, and the router already gates them `Authenticated` through the fail-closed default for an undeclared path. They become `authenticated` rows with no schema: thirteen new `files.endpoints.json` lines instead of ten.
2. **`{key}` declared, `{key...}` dispatched.** `info()` declares `GET`/`DELETE /b/storage/api/buckets/{name}/objects/{key}`; dispatch has always used `{key...}` because keys contain `/`. The rows use `{key...}`. Two `files.endpoints.json` lines change (`{key}` to `{key...}`, same level) and the `GET` path key in `files.openapi.json` changes (the `DELETE` row carries no schema and is not in that snapshot). The `path_params` schema keeps naming `key`. This is the one permitted OpenAPI diff in this PR; it is regenerated once, and the diff is read line by line before it is accepted.
3. **`GET /b/storage/{bucket}/{prefix...}/` is unmatchable.** `match_template` requires a rest parameter to be the final template segment (`endpoint_match.rs:82-85`); the trailing slash adds an empty final segment, so the template matches nothing. Nested folder pages are served only by the hand-written `ends_with('/')` split in `mod.rs:208-228`. The matcher learns the shape the block already declared: `{name...}` may also be the second-to-last segment with an empty final segment, in which case the path must end in `/`, the bound value is everything between the fixed prefix and that final slash, and it must be non-empty. `/b/storage/photos/` therefore still matches only `/b/storage/{bucket}/`, and `/b/storage/direct/abc` (no trailing slash) does not match the folder row, so `endpoint_auth`'s strictest-match keeps the public share link `Public`. Test-first in `endpoint_match.rs`; one sentence added to the spec's section 2.
4. **No consumer of the old admin paths exists.** The repo-wide grep finds the old wire paths only inside the delegation itself and in a doc comment at `packages/impresspress-js/src/services/extensions.service.ts:76`. `admin/pages/storage.rs` emits only `/b/admin/storage` and reads the admin block's own access-log table. The planned "every admin storage page URL is a declared files row" test would assert over an empty set and is dropped; the two tests that carry weight stay (admin `handle` answers 404 for each old path; the files table dispatches each new path to its variant with its variable bound). The TS doc comment is corrected.

## Global Constraints

- No change to wafer-run, `routing.rs`'s table, or any block other than `files` and the admin block's delegation (`admin/route.rs`, `admin/mod.rs`). `endpoint_match.rs` changes only inside `match_template` (plus its module-doc syntax list and tests). `EndpointRoute::new` stays for products and admin.
- Every `crates/impresspress-core/tests/snapshots/*.openapi.json` is byte-identical except `files.openapi.json`, whose only change is the `{key}` to `{key...}` path key (decision 2). Every `*.endpoints.json` is byte-identical except `files.endpoints.json`: thirteen added lines (four `authenticated` cloud rows, three `authenticated` storage rows, six `admin` rows) and two changed lines (`{key}` to `{key...}`). Each is listed in the PR body with the handler line that enforces the level. `admin.endpoints.json` and `admin.openapi.json` do not change: the admin block never declared the delegated paths.
- Every existing declared row keeps its method, path, auth, summary, description, tags and schemas verbatim (`.output::<T>()` becomes `.output(response_schema_of::<T>)`; inline `json!` schemas move into named `fn` producers the row names). Both `/b/storage/admin` and `/b/storage/admin/` stay declared.
- Handlers read path variables only through `msg.var(..)` after `endpoint_match::dispatch` bound them. After Part A: `grep -rn 'path_param(\|strip_prefix("/b\|starts_with("/b\|dispatch_path(\|/admin/b/\|/admin/storage' crates/impresspress-core/src/blocks/files crates/impresspress-core/src/blocks/admin` prints nothing outside test-only string assertions.
- Handler-side checks stay as belt and braces: the block-wide `user_id` requirement and per-user rate limit for user rows, `is_bucket_access_denied` / owner checks in the handlers, the IP-keyed limit inside the share handler. The admin rows are gated `Admin` by the router from the declaration; they were never rate-limited (the delegation returned before the preamble) and stay that way.
- TDD: write the test, run it and see it fail for the expected reason, then implement. One commit per part, each carrying the two trailer lines:
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Format with `cargo +nightly fmt --all`. Lint with `cargo clippy -p impresspress-core --all-targets -- -D warnings`. `cargo test -p impresspress-core --no-fail-fast` has one known unrelated failure, `lockfile_loads_remote_block`; every other test must pass.
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase1/route-table-files` (created from `origin/main` at `4d361a28`, the merge of PR 3). The session's shell guard refuses compound commands containing `git` or shell variables; those go in a script under the scratchpad directory and run with `bash <script>`.

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `crates/impresspress-core/src/endpoint_match.rs` | `match_template` accepts `{name...}/`; module doc lists the shape; six new tests. |
| `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md` | Section 2 gains the `{name...}/` sentence. |
| `crates/impresspress-core/src/blocks/files/mod.rs` | `Route` (28 variants), 29-row `ROUTES` over wire paths, three named schema producers, `user_preamble(Route)`, `info()` = `declare(ROUTES)`, `handle` = dispatch + preamble + one `match`; `test_support::routed`, `table_tests`, `handle_tests`. |
| `crates/impresspress-core/src/blocks/files/storage/mod.rs` | No table, no `handle`, no `handle_admin`; re-exports the nine user handlers and `handle_stats` at `pub(in crate::blocks::files)`; keeps `test_helpers`. |
| `crates/impresspress-core/src/blocks/files/storage/{buckets,objects,search}.rs` | Handlers become `pub(in crate::blocks::files)`; bodies unchanged. |
| `crates/impresspress-core/src/blocks/files/storage/admin.rs` | Only `handle_stats` (its test sends the new wire path). |
| `crates/impresspress-core/src/blocks/files/storage/params.rs` | `extract_bucket_name` / `extract_object_key` read `msg.var("name")` / `msg.var("key")` only; tests bind through the real table. |
| `crates/impresspress-core/src/blocks/files/cloud.rs` | No `handle`; eight `pub(super)` handlers; `handle_delete_share` and `handle_update_quota` read `msg.var("id")`; the direct-call tests route through the table. |
| `crates/impresspress-core/src/blocks/files/share.rs` | `handle_direct_access` reads `msg.var("token")`. |
| `crates/impresspress-core/src/pipeline.rs` | The storage OpenAPI test looks the download endpoint up under `{key...}`. |
| `crates/impresspress-core/tests/snapshots/files.endpoints.json` | Regenerated: +13 lines, 2 changed. |
| `crates/impresspress-core/tests/snapshots/files.openapi.json` | Regenerated: one path key. |
| `crates/impresspress-core/src/blocks/admin/route.rs` | `StorageDelegate` / `CloudStorageDelegate` gone; the old paths classify `ApiNotFound`; tests say so. |
| `crates/impresspress-core/src/blocks/admin/mod.rs` | Two arms and the `req.resource` rewrite gone; a `delegation_tests` module proves the old paths 404. |
| `packages/impresspress-js/src/services/extensions.service.ts` | Doc comment names the new admin paths. |
| `docs/superpowers/plans/2026-09-05-route-table-4-files.md` | This plan. |

---

### Task 0: Commit this plan

- [ ] **Step 1: Commit**

```
docs: plan for phase 1 PR 4 (files)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 1: `match_template` accepts a rest parameter followed by a trailing slash

**Files:**
- Modify: `crates/impresspress-core/src/endpoint_match.rs` (`match_template`, module doc "Template syntax", tests)
- Modify: `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md` (section 2)

- [ ] **Step 1 (RED):** add to `mod tests`: `rest_param_may_be_followed_by_a_trailing_slash` (`/b/storage/photos/2024/x/` against `/b/storage/{bucket}/{prefix...}/` binds `bucket = photos`, `prefix = 2024/x`); `rest_param_with_trailing_slash_requires_the_slash` (`/b/storage/photos/2024/x` does not match exactly); `rest_param_with_trailing_slash_requires_a_non_empty_remainder` (`/b/storage/photos/` and `/b/storage/photos//` do not match); `rest_param_with_trailing_slash_does_not_match_a_single_segment_path` (`/b/storage/direct/abc` does not match; `/b/x/{rest...}/y` still matches nothing); `dispatch_slash_retry_reaches_a_folder_listing` (`dispatch` on `GET /b/storage/photos/2024/x` resolves the folder row and binds `prefix = 2024/x`); `endpoint_auth_keeps_a_public_share_link_public_beside_a_folder_listing` (with `/b/storage/direct/{token}` `Public` and `/b/storage/{bucket}/{prefix...}/` `Authenticated` both declared, `/b/storage/direct/abc` resolves `Public` and `/b/storage/photos/2024/x/` resolves `Authenticated`). Run `cargo test -p impresspress-core --lib endpoint_match::tests`. Expected: the first, fifth and sixth fail (the template matches nothing today), the rest pass.
- [ ] **Step 2 (GREEN):** in `match_template`, a rest segment is accepted when it is the last template segment or the second-to-last with an empty last segment; in the second form the joined remainder must end with `/`, that slash is stripped, and the result must be non-empty. Add the shape to the module doc's syntax list. Run the module's tests: all pass. Run `cargo test -p impresspress-core --lib routing` and `--test extra_routes_test`: pass.
- [ ] **Step 3:** add to the spec's section 2, after the bullet list: "`{name...}` may also be followed by a trailing slash, meaning a folder-style listing; the path must end in `/` and the bound remainder must be non-empty. This is the shape files already declared for nested folder pages; the matcher did not support it."
- [ ] **Step 4:** format, lint, commit:

```
fix(core): match a rest parameter followed by a trailing slash

`files` declares `GET /b/storage/{bucket}/{prefix...}/` for nested folder
pages, but `match_template` only accepted `{name...}` as the final
template segment, so the declared shape matched nothing and the block
served those pages from a hand-written split. `{name...}/` now matches a
path that ends in `/` with a non-empty remainder before it; a bare
`/b/storage/photos/` still resolves to `{bucket}/` only, and a
single-segment path such as the public share link never matches the
folder row, so strictest-match cannot raise its level.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 2 (Part A): one table for the files block

**Files:** see the table above (`blocks/files/**`, `pipeline.rs` test, two files snapshots).

**Old declared surface (16 rows), reproduced exactly except the two `{key...}` templates:** `GET /b/storage/`, `GET /b/storage/{bucket}/`, `GET /b/storage/{bucket}/{prefix...}/`, `GET`/`POST /b/storage/api/buckets`, `GET /b/storage/api/buckets/{name}/objects` (hand-written path and query schemas, `.output::<contracts::ObjectListResponse>()`, tag `storage`), `POST .../objects`, `GET .../objects/{key}` (description, hand-written path schema, tag `storage`), `DELETE .../objects/{key}`, all `Authenticated`; `GET /b/storage/direct/{token}` `Public`; `GET /b/cloudstorage/` `Authenticated`; `GET /b/storage/admin`, `/b/storage/admin/`, `/b/storage/admin/buckets`, `/b/storage/admin/shares`, `/b/storage/admin/quotas` `Admin`.

**Old dispatch, every arm of which resolves to a row:** the delegation guards (`/admin/storage*`, `/admin/b/cloudstorage*`) become the six admin rows under the files prefixes; the `/b/storage/admin` GET chain becomes the five admin page rows; `/b/storage/direct/` becomes `DirectAccess`; the user pages become `BucketListPage`, `ObjectListPage`, `FolderListPage`, `CloudStoragePage`; `cloud::handle`'s four user arms become the four cloud rows; `storage::handle`'s nine rows become the nine storage rows.

**The thirteen new rows, each verified against its handler:**

| Row | Handler gate |
|---|---|
| `GET /b/cloudstorage/shares` authenticated | `cloud.rs` `handle_list_shares`: `list_for_user(ctx, msg.user_id(), ..)`; block preamble `mod.rs` requires a session |
| `POST /b/cloudstorage/shares` authenticated | `cloud.rs` `handle_create_share`: `is_bucket_access_denied` (owner or admin), `created_by: msg.user_id()` |
| `DELETE /b/cloudstorage/shares/{id}` authenticated | `cloud.rs` `handle_delete_share`: `owner != msg.user_id() && !is_admin` is forbidden |
| `GET /b/cloudstorage/quota` authenticated | `cloud.rs` `handle_get_quota`: `get_user_quota(ctx, msg.user_id())` |
| `GET /b/storage/api/search` authenticated | `storage/search.rs` `handle_search`: `search_completed(ctx, msg.user_id(), ..)` |
| `GET /b/storage/api/recent` authenticated | `storage/search.rs` `handle_recent`: `list_recent_for_user(ctx, msg.user_id(), 20)` |
| `DELETE /b/storage/api/buckets/{name}` authenticated | `storage/buckets.rs` `handle_delete_bucket`: `is_bucket_access_denied` |
| `GET /b/cloudstorage/admin/shares` admin | router, from the row; old wire path `/b/admin/api/cloudstorage/shares` was reached only through the admin block's `Admin` prefix |
| `GET /b/cloudstorage/admin/access-logs` admin | same |
| `GET /b/cloudstorage/admin/quotas` admin | same |
| `PATCH /b/cloudstorage/admin/quotas/{id}` admin | same; the old arm matched the `update` action |
| `GET /b/storage/admin/api/buckets` admin | same; `handle_list_buckets` additionally lists every owner only when `is_admin` |
| `GET /b/storage/admin/api/stats` admin | same |

- [ ] **Step 1 (RED): table tests.** Add `mod table_tests` to `files/mod.rs`: `info_endpoints_come_from_the_table`; `every_new_row_dispatches_to_its_handler` (each of the thirteen new `(action, path)` pairs resolves to the named `Route` and binds `id` / `name` where the template has one); `every_path_the_block_served_resolves_to_a_row` (the old arms' paths, with the expected variant and bound variables, including `/b/storage`, `/b/storage/photos/nested/`, `/b/storage/admin`, `/b/storage/admin/` and a nested `{key...}` download); `user_preamble_follows_the_declared_level` (for every row, `user_preamble(row.handler) == (row.auth == Authenticated)`); `declared_levels_gate_the_router` (`endpoint_auth` over `info().endpoints`: every admin row `Admin`, the share link `Public`, the user rows `Authenticated`, and the slash-variant `GET /b/storage/admin/buckets/` no more permissive than today's `Authenticated`). Run `cargo test -p impresspress-core --lib blocks::files::table_tests`. Expected: FAIL to compile, `cannot find value ROUTES`.
- [ ] **Step 2 (RED): the block serves the new paths.** Add `mod handle_tests` (tokio, `FilesBlock::new().handle(..)` on `TestContext::with_files()`): `GET /b/storage/admin/api/stats` as admin answers JSON with `bucket_count`; `PATCH /b/cloudstorage/admin/quotas/u-9` as admin with `{"max_storage_bytes": 5}` answers a record whose `user_id` is `u-9`; anonymous `GET /b/cloudstorage/quota` is refused (`Unauthenticated`). Expected: FAIL (`NotFound` today: the first path falls through the admin GET chain, the second reaches `cloud::handle` with no arm).
- [ ] **Step 3 (RED): handlers must not parse the path.** In `storage/params.rs` replace the two prefix-fallback tests with `bucket_and_key_are_bound_by_the_table` (an unrouted `GET /b/storage/api/buckets/photos/objects/dir/file.txt` binds nothing; the same message through `crate::blocks::files::test_support::routed` binds `name = photos`, `key = dir/file.txt`). In `cloud.rs` tests, the two delete-share tests and `delete_missing_share_is_not_found` call `handle_delete_share(&ctx, &routed(msg))`, and `quota_endpoint_surfaces_usage_outage` calls `handle_get_quota`. Expected: FAIL to compile (`routed` and the direct handlers do not exist yet) or, for params, FAIL because the fallback still recovers `photos`.
- [ ] **Step 4 (GREEN):** write `Route`, `ROUTES` (table order: admin pages, admin API, share link, storage API with `{key...}` rows before `objects` and `{name}`, cloud API, then the four user pages with `/b/storage/{bucket}/` and `/b/storage/{bucket}/{prefix...}/` last), the three schema producers, `user_preamble`, `info()` = `declare(ROUTES)`, `handle` = dispatch, preamble for user rows (the existing `user_id` check and `check_user_rate_limit_with(.., Some((RateLimit::UPLOAD, "upload")))`), one `match route`. `FolderListPage` passes `format!("{}/", msg.var("prefix"))` as the prefix (the bound value has no trailing slash and is percent-decoded, which the old split never did). Delete `storage::{Route, ROUTES, handle}`, `storage::admin::handle_admin`, `cloud::handle`; widen the handlers' visibility; `share.rs`, `cloud.rs` and `params.rs` read `msg.var(..)`. Add `mod test_support` with `routed`. Update the `pipeline.rs` storage OpenAPI test's path key. Run `cargo test -p impresspress-core --lib blocks::files` and `--lib pipeline`: PASS.
- [ ] **Step 5: snapshots.** `cargo test -p impresspress-core --test openapi_snapshot --test endpoint_surface`: both FAIL on `files` only. Regenerate each once with `env UPDATE_OPENAPI_SNAPSHOTS=1 ...`. `git diff -- crates/impresspress-core/tests/snapshots/` must show exactly: `files.endpoints.json` +13 lines and 2 changed lines; `files.openapi.json` one changed path key and nothing else. Anything else: stop and report. Run both tests again: PASS.
- [ ] **Step 6: gates, format, lint, commit.** Grep gate for `blocks/files` prints nothing outside test-only string assertions. `cargo +nightly fmt --all`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`. Commit:

```
refactor(files): declare the HTTP surface from one route table

`ROUTES` (29 rows over wire paths) now carries the summaries, auth
levels, schemas and tags `info()` listed by hand for 16 of them, and
`info()` is `declare(ROUTES)`. The `starts_with` chain, the
`/b/storage/api` sub-table, and the `match (action, path)` arms in
`cloud.rs` and `storage/admin.rs` go; `handle` dispatches through
`endpoint_match::dispatch` and every handler reads `{name}`, `{key}`,
`{id}`, `{token}`, `{bucket}` and `{prefix}` only as the table bound them.

The admin storage JSON API moves from the admin block's delegation
(`/b/admin/api/{storage,cloudstorage}/...`, rewritten to synthetic paths)
to six `admin` rows under the files prefixes, gated by the router from
the declaration. The user cloud-storage API and the three storage-API
paths the block served but never declared become `authenticated` rows at
the level each handler already enforces. The two object rows declare the
`{key...}` template dispatch has always used, which is the one OpenAPI
path-key change.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp
```

---

### Task 3 (Part B): delete the admin block's delegation

**Files:**
- Modify: `crates/impresspress-core/src/blocks/admin/route.rs`, `crates/impresspress-core/src/blocks/admin/mod.rs`
- Modify: `packages/impresspress-js/src/services/extensions.service.ts` (doc comment)

- [ ] **Step 1 (RED):** in `route.rs` tests, replace the three delegate cases with `ApiNotFound` expectations for `/b/admin/api/storage/buckets`, `/b/admin/api/cloudstorage/shares` and `/b/admin/api/cloudstorage`. In `mod.rs` add `mod delegation_tests` (tokio): on `TestContext::with_files()` with the real `FilesBlock` registered as `impresspress/files` (so that, while the delegation exists, the forwarded call is served and the test fails honestly), `AdminBlock::new().handle(..)` answers `NotFound` for `GET /b/admin/api/cloudstorage/shares`, `.../access-logs`, `.../quotas`, `PATCH .../quotas/u-1`, `GET /b/admin/api/storage/buckets`, `.../stats`. Run `cargo test -p impresspress-core --lib blocks::admin`. Expected: FAIL (the classifier returns the delegate variants; the forwarded calls answer 200).
- [ ] **Step 2 (GREEN):** delete the two variants and their arms in `route()`, the two arms in `handle()` with the `req.resource` rewrite, and the comment that describes the delegation; the `api_norm` comment says the normalized path is passed as an argument and `req.resource` is never rewritten. Fix the TS doc comment. Run `cargo test -p impresspress-core --lib blocks::admin`: PASS.
- [ ] **Step 3: gates, format, lint, commit.** Grep gate for `blocks/admin` prints nothing outside test-only string assertions. `cargo test -p impresspress-core --test openapi_snapshot --test endpoint_surface`: PASS, `admin.*` unchanged. Commit:

```
refactor(admin): drop the storage delegation to the files block

`/b/admin/api/storage/...` and `/b/admin/api/cloudstorage/...` were
served by rewriting `req.resource` to a synthetic path and forwarding to
`impresspress/files` through `call_block`; the files block now declares
those APIs itself under `/b/storage/admin/api/...` and
`/b/cloudstorage/admin/...`. The two `AdminRoute` variants, their arms
and the rewrite go; the old paths answer 404 from this block, and the
SDK comment that named one of them now names the new location. The
admin snapshots do not change: the block never declared the delegated
paths.

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
grep -rn 'path_param(\|strip_prefix("/b\|starts_with("/b\|dispatch_path(\|/admin/b/\|/admin/storage' crates/impresspress-core/src/blocks/files crates/impresspress-core/src/blocks/admin
git status --short
git diff origin/main --stat -- crates/impresspress-core/tests/snapshots/
```

Expected: fmt clean; clippy clean; all tests pass except `lockfile_loads_remote_block`; grep prints nothing; working tree clean; the snapshot diff is `files.endpoints.json` and `files.openapi.json` and nothing else.

- [ ] **Step 2: push and open the PR** with `bash <scratchpad>/push-and-pr.sh "refactor(files): declare the files block from its route table and drop the admin delegation" <body-file>`. Body: row count; the exact `files.endpoints.json` diff with the enforcing handler line per added or changed row; the `files.openapi.json` diff under its own heading; the consumers of the old admin paths that were repointed; the grep-gate output; the tests routed through the table; the matcher tests added; deviations; trailer. Do not merge.

---

## Self-review

**Spec coverage (PR 4 scope):** section 3's files paragraph (admin APIs under the files prefixes as `admin` rows, delegation and rewrite deleted, cloud user paths declared `authenticated`, `starts_with` chain replaced): Tasks 2 and 3. Sequencing item 4: the whole plan, with the surface snapshot growing by thirteen rows rather than the spec's eight (decision 1) and two rows re-templated (decision 2). Section 2 amended for `{name...}/` (decision 3): Task 1. Section 5 "Blocks" bullet: the table test, the served-paths test and the binding tests in Task 2.

**Deviations recorded:** the four decisions above, plus: the `pipeline.rs` OpenAPI test is edited (it pins the changed path key); `admin/pages/storage.rs` is not touched (it never consumed the delegated paths); the nested folder page now receives a percent-decoded prefix, which the old split did not decode.
