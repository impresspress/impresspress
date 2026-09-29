# Enums and error discipline, PR 1: one database-error mapping

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give a failed database call exactly one place that decides what the client sees. `crud::db_error` is that place: `NotFound` is a 404 with the caller's label, **`PermissionDenied` is a 403** (today every one of the 62 hand-written mappings collapses it into `500 Internal server error (ref: …)`, so a WRAP grant that is missing in production is indistinguishable from a corrupt row), `ResourceExhausted` keeps the 429 the wafer mapping already gives it, and everything else is the sanitized 500 with the context logged. `http::require_row` is the `Result<Option<_>>` half of the same sentence, `From<ErrorCode> for OutputStream` lets a match arm say `ErrorCode::NotFound.into()`, `crud::path_var` is the one empty-path-binding guard, `errors::ErrorCode::status_code()` — dead, and wrong about quota — is deleted, and `rate_limit::rate_limited_response` stops emitting the `"[code] message"` prefix that `errors.rs`'s own doc comment says is gone. A new `tests/error_door.rs` keeps the hand-written shape from coming back.

**Architecture:** Two helpers and one conversion, all in modules that already exist. `crud::db_error(error, not_found, context)` is the mapper; `crud.rs`'s own four `if e.code == ErrorCode::NotFound { … } else { err_internal("Database error", e) }` tails and its three bare `err_internal("Database error", e)` calls become one call each, which is what makes every block that reaches the database through `crud::{list_page, get_record, create_record, update_record, delete_record, verify_owner}` answer 403 for a denial without touching the block. `http::require_row(row, not_found)` sits beside `redirect`, because it turns an `Option` into a response and has nothing to do with the generic-table helpers. `crud::path_var(msg, var, missing)` generalises `path_id` from the hardcoded `{id}` binding to any bound segment, and `path_id(msg, label)` stays as the `{id}` spelling with the standard message; the hand-rolled `if x.is_empty() { err_bad_request("Missing … ID") }` copies call one or the other. `errors.rs` keeps `ErrorCode`, `as_str`, `Display`, `error_response` and gains `default_message` + `From`; it loses `status_code()`, whose only substantive claim (413 for quota) contradicts the 429 that ships.

**Tech Stack:** Rust, `wafer-run` at rev `7d47e5e` (`wafer_block::{err_*, http_codec}`, `wafer_core::clients::database`, `wafer_block::wrap::check_access`), `serde_json`, `tokio` tests, nightly `rustfmt`, `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-09-06-enums-and-error-discipline-design.md`, sections 1.4 (the inventory), 2.2.1, 2.2.2, 2.2.3, 2.2.5, 2.2.6, 2.2.7, 3.2 "Error model" and "`errors.rs`", and section 4's PR 1. Section 2.2.4 (`RepoError` folded into `WaferError`) is PR 2 and is not touched here.

## Decisions taken while planning (recorded, not re-litigated)

1. **`db_error` lives in `crud.rs`, as the spec names it, and `require_row` in `http.rs`.** The split reads oddly — both are response constructors — but `crud.rs` is where the four copies being deleted live and where the spec puts the door, and moving a response mapper into `http.rs` after the fact would be its own PR's diff. Recorded as an observation for a later hygiene pass, not acted on here.
2. **`PermissionDenied` returns a fixed `"Access denied"`, and logs the cause at `warn!`.** The wafer error's message names the missing grant and the resource (`wafer_block::wrap::check_access`), which is deployment topology and must not reach a client — the same reason `err_internal` sanitizes. But a denial is almost always a missing `ResourceGrant`, i.e. an operator's bug, so the cause has to be visible somewhere: `db_error` logs `error`, `context` and the code before returning the 403.
3. **`ResourceExhausted` keeps the service's own message.** It is a classified, client-actionable refusal (a quota), not an internal invariant, and this repo already echoes classified service messages — `handlers/provider.rs::provider_error`, `handlers/offers.rs::domain_error` and `handlers/product.rs::write_error` all pass `error.message` through for `InvalidArgument` and `FailedPrecondition`. Only `Internal`-class failures are sanitized.
4. **`db_error` gets four arms, not the six the three private block helpers carry.** `admin_error`, `domain_error` and `write_error` add `InvalidArgument → 400` and `FailedPrecondition | Aborted → 409`, which are domain classifications a *repo* raises, not classifications a *database* raises. Folding those in would change behaviour at every `crud` call site in the tree. The three helpers delegate their tail to `db_error` in PR 2/PR 4, keeping their extra arms.
5. **`path_id` keeps its signature; `path_var` is the general form.** `path_id(msg, "Product")` already produces exactly `"Missing product ID"`, which is what 30-odd hand-rolled guards spell, so those convert with no message change. Routes that bind something other than `{id}` (`{offer_id}`, `{product_id}`, `{preset_id}`, `{link_id}`, `{key}`, `{name}`) call `path_var(msg, var, missing)` with the message at the call site, because the noun is per-route ("Missing setting key", "Missing bucket name") and formatting it from a label would be the implicit mapping layer `CLAUDE.md` forbids.
6. **One message changes: `"Missing Payment Link ID"` becomes `"Missing payment link ID"`.** That casing drift is the reason the spec counts the guards as duplication. Nothing asserts the old string — verified across `crates/`, `packages/impresspress-js/src`, `packages/`, `examples/` (including `examples/tests/*.spec.ts`), `crates/impresspress-web/tests`, `docs/` and the blocks' embedded `assets/*.js`.
7. **The 62 hand-mapped sites are not all converted here.** PR 1 converts the seven inside `crud.rs` — which is what makes the fix reach every block through the CRUD primitives — plus the eight sites in the four files this PR already opens for their `require_row` conversion. The remaining list is written into the PR body and into this plan (task 8) so PR 2 inherits it rather than rediscovering it. Converting all 62 in one diff would put a 60-file mechanical sweep in the same review as the behaviour change it depends on.
8. **The anti-regression gate is a new `tests/error_door.rs`, not an addition to `repo_door.rs`.** Same source-scan shape (`crate_sources`, `code_only`, an exact-match allowlist with a reason per entry), different invariant. `repo_door.rs` answers "who may name this table"; this answers "who may hand-map a database error", and the two allowlists have nothing in common.
9. **The gate is allowlisted per file with a reason, and starts green.** It cannot start red: a failing committed test is not a gate. The allowlist is the remaining-sites list from decision 7, so PR 2 shrinks it and the file that comes off the list can never come back without a review.

## Global Constraints

- Both snapshot gates byte-identical for every block: `crates/impresspress-core/tests/snapshots/*.openapi.json` and `*.endpoints.json`. This PR declares no endpoint and changes no schema. `UPDATE_OPENAPI_SNAPSHOTS=1` is never run. The third gate, `tests/snapshots/dev.tools.json`, is checked under `--features block-dev` and is expected unchanged (no products contract moves).
- No change to wafer-run (rev `7d47e5e`). `err_*`, `http_codec` and `WaferError::with_detail_code` are used as they ship.
- No raw SQL; no `db::*` call moves between modules, so `scripts/audit-wrap-grants.sh` is unaffected.
- TDD: write the test, run it, see it fail for the expected reason, then implement, then see it pass. Commits carry the two trailer lines:
  `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01Vfp8g6U7PQTG1kgw2834Tp`.
- Verification before the PR: `cargo +nightly fmt --all -- --check`; `cargo clippy -p impresspress-core --all-targets -- -D warnings`; `cargo clippy -p impresspress-core --features block-dev,test-support --all-targets -- -D warnings`; `cargo test -p impresspress-core --no-fail-fast` (known unrelated failure `lockfile_loads_remote_block`); `cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot --test dev_tools_manifest --test error_door`; `git status --short crates/impresspress-core/tests/snapshots` empty after both passes.
- Work happens in the worktree `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` on branch `phase3/error-helpers` (from `origin/main` at `42684d0d`, the merge of PR #25). The session's shell guard refuses compound commands containing `git` or shell variables; those go in a script under the scratchpad directory and run with `bash <script>`.

---

## File structure

| File | Responsibility after this PR |
|---|---|
| `crates/impresspress-core/src/blocks/errors.rs` | `ErrorCode`, `as_str`, `default_message`, `Display`, `From<ErrorCode> for OutputStream`, `error_response`, `impresspress_error_code_to_wafer` (with a doc line naming `http_codec::error_code_to_http_status` as the status authority). `status_code()` and `test_error_code_status_codes` are gone; a test pins that `QuotaExceeded` resolves to **429**, the status that actually ships. |
| `crates/impresspress-core/src/blocks/crud.rs` | `db_error(error, not_found, context)` — the one database-error mapping. `path_var(msg, var, missing)` and `path_id(msg, label)`. Every `if e.code == ErrorCode::NotFound` tail and every bare `err_internal("Database error", e)` inside the file is one `db_error` call. |
| `crates/impresspress-core/src/http.rs` | `require_row(row, not_found)` beside `redirect`. |
| `crates/impresspress-core/src/blocks/rate_limit.rs` | `rate_limited_response` builds its `WaferError` through `with_detail_code`, keeps the `Retry-After` meta and drops the `"[rate_limit_exceeded] "` prefix. |
| `crates/impresspress-core/src/blocks/{products,admin,llm,files,legalpages,tickets,auth_ui}/…` | The hand-rolled empty-path-binding guards call `path_id`/`path_var`. The four files that also gain `require_row` (`legalpages/mod.rs`, `products/handlers/sellers.rs`, `admin/settings.rs`, `admin/users.rs`) drop their `Err(e) => err_internal("Database error", e)` tails for `db_error`. |
| `crates/impresspress-core/tests/error_door.rs` | New. Source scan: no file under `src/blocks/` outside `crud.rs` pairs an `ErrorCode::NotFound` arm with an `err_internal` tail, except the files on the allowlist — which is PR 2's worklist, each with the reason it is still there. |

---

## Tasks

### Task 1 — `db_error` maps `PermissionDenied` to 403

- [ ] Write `blocks/crud.rs`'s test module: `db_error` on each of `NotFound`, `PermissionDenied`, `ResourceExhausted` and `Internal` resolves through `test_support::output_http_status` to 404 / 403 / 429 / 500; the 403 body does **not** contain the wafer error's own message; the 500 body is the sanitized `"Internal server error (ref: "` prefix.
- [ ] Run it: fails to compile, `db_error` does not exist.
- [ ] Implement `db_error` with the four arms of decision 2/3/4 and the `warn!` on the denial arm.
- [ ] Run it: passes.

### Task 2 — a WRAP denial through `crud` is a 403, end to end

- [ ] Write the test on a `TestContext::with_wrap` whose grant list does not cover the table `crud::get_record` reads: the resulting `OutputStream` is 403, not 500. Assert through `output_http_status`, and assert the same for `update_record`, `delete_record` and `verify_owner`.
- [ ] Run it: fails, every one answers 500.
- [ ] Convert `crud.rs`'s four `ErrorCode::NotFound` tails and its three bare `err_internal("Database error", e)` calls to `db_error`.
- [ ] Run it: passes.

### Task 3 — `require_row`

- [ ] Write `http.rs`'s test: `require_row(None::<u8>, "User not found")` is a 404 carrying that message; `require_row(Some(7), …)` is `Ok(7)`.
- [ ] Run it: fails to compile.
- [ ] Implement, run it: passes.

### Task 4 — `From<ErrorCode> for OutputStream` and the `errors.rs` shrink

- [ ] Write the tests: `OutputStream::from(ErrorCode::NotFound)` carries the wafer `NotFound` code, the detail code `"not_found"` and a non-empty default message; `error_response(ErrorCode::QuotaExceeded, …)` resolves to **429** through `http_codec::resolve_error_status`.
- [ ] Run them: the `From` test fails to compile; the 429 test passes already (it pins the shipping behaviour that `status_code()`'s 413 contradicted).
- [ ] Implement `default_message` and the `From` impl; delete `status_code()` and `test_error_code_status_codes`; add the doc line naming the status authority.
- [ ] Run them: pass. `cargo clippy -p impresspress-core --all-targets -- -D warnings` proves `status_code` had no caller.

### Task 5 — `rate_limited_response` stops prefixing

- [ ] Write the test: the 429 carries `detail_code() == Some("rate_limit_exceeded")`, a message that does not start with `[`, and the `Retry-After` header.
- [ ] Run it: fails on both the detail code and the prefix.
- [ ] Rebuild the response through `WaferError::new(..).with_detail_code(..)` plus the existing meta.
- [ ] Run it: passes.

### Task 6 — one empty-path-binding guard

- [ ] Write the tests: `path_var(msg, "offer_id", "Missing offer ID")` on a message with no binding is a 400 with that message and on a bound message is the value; `path_id(msg, "Product")` is `"Missing product ID"`.
- [ ] Run them: `path_var` does not exist.
- [ ] Implement `path_var`; re-express `path_id` on it.
- [ ] Convert the hand-rolled guards, one block at a time, running that block's tests after each.

### Task 7 — `require_row` and `db_error` at their first call sites

- [ ] Convert `legalpages/mod.rs`, `products/handlers/sellers.rs`, `admin/settings.rs` and `admin/users.rs`: `Ok(Some(x)) / Ok(None) / Err(e)` becomes `require_row(… .map_err(|e| db_error(e, …, "Database error"))?, …)?`.
- [ ] Add one handler-level test per file that a denied read answers 403.

### Task 8 — the anti-regression gate

- [ ] Write `tests/error_door.rs`: scan every `.rs` under `src/`, comment-stripped; fail on any file under `blocks/` outside `crud.rs` where an `ErrorCode::NotFound` arm is followed within six lines by `err_internal`, unless the file is on the allowlist.
- [ ] Seed the allowlist with the sites this PR does not convert, each with the PR that will (PR 2 for auth/admin, PR 4 for products, PR 8–10 for the T4 groups).
- [ ] Run it green; then hand-check it by adding a temporary offending file and seeing it fail.

### Task 9 — verification and the PR

- [ ] The full verification list under Global Constraints, in order.
- [ ] Confirm all three snapshot files are byte-identical (`git status --short` on the snapshots directory, after both the default and the `--features block-dev` passes).
- [ ] PR body: the behaviour changes (403 for a denial, the 429 body, the one 400 message), the converted-site list, the remaining-site list, and the three gates' status.
