# Carry-forward notes for later PRs (from implementation and review reports)

## For PR 5 (admin)
- `admin/pages/users.rs:509` posts to `/b/auth/api/api-keys/{id}/revoke`, a path auth-ui never served; the revoke route is `PATCH /b/auth/api/api-keys/{id}`. The admin button 404s today. Fix the URL/method when admin is migrated and add a render test that every `hx-*` URL an admin page emits matches a declared endpoint of the target block.

## For PR 6 (products)
- `blocks/crud.rs`: `crud_delete`, `crud_delete_owned`, `path_id`'s prefix-strip fallback and `util::path_param` are kept only for products. Delete them when products dispatches on wire paths (`crud_get`, `crud_update`, `crud_get_owned` were already deleted in #13).
- `rate_limit.rs::check_route_limits` still exists for products; auth-ui's `apply_rate_limit` duplicates its 8-line identity resolution. When products migrates its rate limits to a per-variant function, extract the shared identity-resolution helper and delete `check_route_limits`.
- `endpoint_match::dispatch_path` has products as its only caller; PR 7 deletes it.

## For PR 7 (router cleanup)
- The routing test `auth_ui_declares_every_path_the_router_carves_out` (#14) proves the eight auth-ui carve-outs are redundant; `bare_index_path_is_gated_at_the_declared_level` and the system `{filename}` row prove the static one is.
- `tests/dev_status.rs::routes_and_endpoints_stay_in_lockstep` is partly tautological now; it still guards the duplicate-route and all-Admin invariants. Tighten or leave.

## For phase 5 (UI)
- `tickets::ENDPOINT_REFERENCE` (admin reference page) and `legalpages/pages.rs` `:id` HTML text are hand-spelled second listings of paths. Render them from `ROUTES`/`info().endpoints`.

## For phase 2/3
- auth-ui `POST /b/auth/api/oauth/sync-user` reads `WAFER_RUN__AUTH__INTERNAL_SECRET`, undeclared and ungranted (review bug B14); decide declare+grant or delete.

## PR 4 (files) decisions taken mid-implementation (for the reviewer)
- Three extra served-but-undeclared `/b/storage/api` paths (`GET search`, `GET recent`, `DELETE buckets/{name}`) are declared `authenticated`: SDK consumers exist, handlers scope by user/owner, router already gated them Authenticated by fail-closed default. 13 new surface lines, not 10.
- Object rows use `{key...}` (keys contain `/`; dispatch always did). `files.openapi.json` changes ONLY in the two path keys; `files.endpoints.json` two lines change accordingly.
- `match_template` gains support for `{name...}/` (rest param followed by a trailing slash; path must end in `/`; remainder non-empty). Spec section 2 gets one sentence. `/b/storage/direct/{token}` must stay Public under strictest-match (test added).
- The "admin storage page URLs are declared files rows" test was dropped: the page never called the files block. Tests kept: admin 404s the old delegated paths; files serves the new ones.
- `storage/params.rs` prefix-strip fallbacks removed in this PR.

## For phase 4 (upstream wafer-run list) — added from PR 4's review
- wafer-core's OpenAPI projection should render a rest parameter `{key...}` as `{key}` in the path key (OpenAPI cannot express multi-segment params; the parameter is still named `key`). Until then `tests/openapi_snapshot.rs` pins that every published template expression, modulo the `...` marker, has a matching path parameter.
- Residual from PR 4: no test exercises a bucket literally named `api`, `admin` or `direct` (creatable today; the SSR page now lists it like any other bucket).

## From PR 5 (admin, #16)
- PR 7: the `/b/admin/settings` router prefix entry is now fully expressed by admin's rows (all `admin`); drop it with the carve-outs.
- Phase 5 (UI): the users page's API-key revoke control now reaches `PATCH /b/auth/api/api-keys/{id}`, but that handler returns JSON which htmx swaps into `#users-tab-content`; needs a page-side re-render or an `hx-swap`/trigger that reloads the tab.
- Admin's `page_link_tests::every_link_an_admin_page_emits_resolves_to_a_declared_row` is the pattern to reuse for every block with SSR pages (phase 5).

## PR 6 (products) decision taken mid-implementation (for the reviewer)
- Option B: `ROUTES` = exactly the 123 declared rows. The 51 undeclared alias spellings (`/b/products/api/X` twins of `/b/products/X` and vice versa, produced by the old `strip_prefix("/b/products/api")` rewrite) and the webhook's non-POST / suffixed shapes now 404. No consumer outside `blocks/products/tests/` used them (SDK, storefront.js, e2e specs, docs grepped). The three router-level alias tests are rewritten: two against the declared spelling, one pinning that the former alias answers 404.
- Deleted with products as last caller: `rate_limit::{RouteLimit, check_route_limits, check_user_rate_limit}` (shared `apply_route_limit` helper extracted; auth-ui calls it), `crud::{crud_delete, crud_delete_owned}`, `OwnedResource.path_prefix`, `util::path_param`, `crud::path_id` prefix fallback.
- `endpoint_match::dispatch_path` is caller-free after PR 6; PR 7 deletes it.
- Follow-up PR (not in phase 1 scope): products' ~48 hand-written JSON schemas derive from `contracts.rs` (review bug B11, `pending_review` vs `pending`), with its own reviewed OpenAPI diff.

## From PR 6 (products, #17) for PR 7
- `endpoint_match::dispatch_path` is caller-free: delete it (and fold `dispatch_exact` back into `dispatch`).
- `util::url_path_decode`'s doc comment still names `dispatch_path` and "the products block's SSR page dispatch": reword.
- `router_declared_public("/b/products/webhooks", ..)` is fully expressed by the declaration (`routing::stripe_webhook_is_public_from_the_products_declaration_alone`).
- `products/tests/handler_tests.rs::every_products_json_endpoint_has_discovery_schema` filters by path prefix; could select non-page rows from `ROUTES` instead (phase 5 or PR 7, low priority).

## Fork infrastructure (for the final report to the user)
- The "Deploy dev-sandbox" workflow fails on every push to the fork's `main` at its "Deploy to Cloudflare Workers" step (never succeeded on the fork; earlier runs cancelled). The fork has no Cloudflare deploy credentials. Not a code failure; either add the secrets to the fork or restrict that workflow to the org repository. "CI Main" and "Deploy Browser Demo" are green on every merge.

## From Phase 2 PR 1 (#19)
- **Security-relevant, found by the audit fixture:** `scripts/audit-wrap-grants.sh` associated `.typed(..)` only when written on the same line as `ResourceGrant::read(..)`. rustfmt puts admin's `read("*","*").typed(Network)` and `read_write("*","*").typed(Crypto)` on continuation lines, so both were indexed as Database wildcards covering every admin-owned table for every caller: no missing grant on an admin table could ever be reported. Fixed in #19 along with two smaller parser gaps. Anything the fixed gate now catches on other blocks belongs in a follow-up.
- `blocks/system.rs:5` warns `unused_imports` for `ResponseBuilder` whenever `embed-assets` is off, so every `--target wasm32-unknown-unknown` check prints it. Pre-existing on main; the wasm lane is not warning-clean. Fix in phase 4 (adapters) or as a one-line hygiene PR.
- Plan files in this repo never tick their checkboxes (no committed plan has a `- [x]`); that is the convention, not an omission.

## WRAP audit follow-up (owed after Phase 2 PR 3, #21) — do as its own small PR
As blocks move from `db::*` calls to repo functions, those accesses leave the
audit script's view. After #21 three cross-block pairs are unaudited
(`admin → wafer_run__auth__users`, `admin → wafer_run__auth__api_keys`,
`userportal → wafer_run__auth__users`); pair count fell 71 → 68. All grants
are still declared and correct (verified: tickets/products/files have explicit
`wafer_run__auth__rate_limits` grants, auth-ui is covered by the wildcard), so
this is a future-regression risk, not a live hole.
The fix is the `auth::repo::` analogue of PR 1's `platform_state::` rule, plus
a notion the script lacks: `blocks/rate_limit.rs` is a shared helper module,
not a block, so a naive rule attributes its calls to a non-existent
`impresspress/rate-limit` caller and reports a false MISSING. `blocks/crud.rs`
handles the same problem with an `// audit-allow-file:` pragma, but crud is a
pure pass-through (table name comes from the caller) whereas rate_limit names
one table, so it needs the caller's identity instead.
Also owed: the audit's `db::*` regex misses `upsert`, `list_all`,
`paginated_list`, `aggregate`, `soft_delete`, `delete_by_filters_count`,
`get_by_field` — which is why `rate_limit.rs` and `tickets/abuse.rs`
contributed nothing to it even before the repo move.

## Consumer-search checklist for every later PR (two misses so far)
When a PR changes a path, a method or a response body, grep ALL of these, not
just `crates/`: `crates/`, `packages/impresspress-js/src` (the SDK, has its own
CI job), `packages/`, `examples/` (including `examples/tests/*.spec.ts`, which
`examples/run-tests.sh` runs against a real server and CI does not gate),
`crates/impresspress-web/tests` (Playwright e2e), `docs/`, and the block's own
embedded JS under `src/**/assets/*.js`.
Misses so far: products PR #17 missed `examples/tests` (three alias spellings);
files PR #22 missed the SDK's `storage.service.ts`, which reads
`{records,total_count}` from `/b/storage/api/search` and `/b/storage/api/recent`.

## Owed follow-up PR: files JSON surface (after Phase 2)
Declare `.output(response_schema_of::<..>)` on the seven files endpoints that
publish typed structs but declare no schema (that blind spot is why the
snapshot gates could not see PR #22's wire change), and decide deliberately
whether those bodies move from the `RecordList` envelope to `{rows,total}` —
moving `packages/impresspress-js/src/services/storage.service.ts`
(`RecordListWire`/`flattenRecordList`) in the same PR if so. `files.openapi.json`
moves either way, so it needs its own reviewed diff.

## From Phase 2 PR 5 (legalpages, #23)
- **The OpenAPI gate had a second, undocumented way to be vacuous.**
  `test_support::real_block_infos()` is the block list `discovery_json` builds
  `/openapi.json` from, and it listed neither `legalpages` (fixed in #23) nor
  `userportal`, `email`, `system`. Adding a block to `SNAPSHOTTED_BLOCKS`
  without adding it there produces `{}` forever. Any later PR that snapshots a
  new block must touch both. Blocks still absent from `real_block_infos()` and
  from `SNAPSHOTTED_BLOCKS`: `userportal`, `email`, `system` (each has an
  `.endpoints.json` and no `.openapi.json`).
- `openapi_snapshot.rs::path_placeholders_and_path_parameters_agree` (from PR 4)
  means any newly *published* path carrying `{…}` must also declare
  `.path_params(..)`, or the snapshot commit fails. Budget for it.
- **Phase 5 (UI):** `legalpages/pages.rs::endpoints_page` is still a
  hand-spelled second listing of the block's surface. It says `POST
  /b/legalpages/api/documents/:id/publish` where `ROUTES` declares `PATCH`, and
  omits `GET /b/legalpages/api/documents/{id}`. Render it from `ROUTES` (same
  item as `tickets::ENDPOINT_REFERENCE`).
- **Owed follow-up:** the four legalpages JSON endpoints publish typed bodies
  and declare no `.output(response_schema_of::<..>)`, so the snapshot gate
  cannot see a response-shape change on them — same blind spot as files after
  #22. #23 changed two of those bodies (create and create-publish now describe
  the complete row) and had to say so in prose because no gate could.
- The WRAP audit's `db::*` regex still misses `list_all` (already noted after
  #21): legalpages' archive query contributed nothing to it before or after the
  repo move. Pair count unchanged at 68; legalpages never appears because every
  access is own-table.

## From Phase 2 PR 6 (products + llm, this PR)
- **Products group/type/template CRUD still runs through `blocks/crud.rs`** with
  the table as a parameter, so `handlers/{group,types}.rs` stay on the
  `tests/repo_door.rs` IDENT allowlist (category 4). Folding those paths into
  per-table repo functions moves the HTTP error mapping `crud` encapsulates;
  own PR, own review.
- `repo/stripe_events.rs` owns its table name only; `blocks/products/stripe.rs`
  is the sole reader/writer and predates the convention. Pre-existing; the door
  entry now names it out loud.
- `repo::purchases` / `repo::subscriptions` spell their constants
  `PURCHASES_TABLE` / `LINE_ITEMS_TABLE` / `SUBSCRIPTIONS_TABLE` rather than
  `TABLE`, which is why those two doors allowlist themselves. Renaming touches
  ~60 call sites in the block's tests.
- `impresspress__llm__providers` has no repo module (`schema.rs` encodes it,
  `providers/` reads it). It joins `llm/repo/` when that pair is untangled.
- **The door tuple's ident field is a slice now.** Any door whose constant is
  re-exported under an alias must list the alias too, or `use
  blocks::products::OFFERS_TABLE` is a call site neither scan sees.
- Still swallowed in llm (T4, Phase 3): the settings page's `list_all`, the
  chat page's thread/entry lists, `messages_list`, `messages_create`'s `Option`,
  and the `let _ =` at `chat.rs:132` / `streaming.rs:131`.
- Inventory drift found: `util::block_request` is `util.rs:291-304` after PRs
  1-5, not `243-267`. Any later section quoting `util.rs` line numbers from the
  spec must re-resolve them.

## From Phase 2 PR 7 (#25) — operational note, not a defect
Auth migration 012 recreates `wafer_run__auth__sessions` with a `DROP TABLE`.
Migrations re-run in full whenever the concatenated hash changes, so that DROP
re-fires on every FUTURE auth migration and clears the userportal device list.
No user is signed out (nothing authenticates against the table; the `Creds`
enum has only Jwt and Pat), and issuance re-materialises a row for each device
on its next refresh, which is also the fallback that repopulates it. If a
future PR wants the list to survive, rename the table instead of dropping it.
Also owed from #25: `AuthConfig.session_lifetime_days` is a dead field;
refresh reuse-detection revokes the family but leaves the session row (not a
correctness issue since the family is dead); Phase 4 owes the Worker
`scheduled` handler for the sweeper; Phase 5 could render `auth_method` on the
device list (the column is written; a UI change was avoided because the
`portal-sessions` visual baseline is only verifiable from CI).

## Regression merged and later fixed — keep this in mind for the final live test
PR #24 moved llm to read the messages block through `call_block` with query
strings (`?page_size=50`, `?kind=message`). `util::block_request` put the whole
`path?query` into `req.resource`, which `endpoint_match::dispatch` compares
segment by segment, so BOTH filtered calls matched no route and 404'd: the LLM
chat had no history and the thread sidebar was empty on every request. It
merged green because the callers swallow the failure (`unwrap_or_default`,
a T4 site) and an existing test asserted the broken spelling. Found and fixed
in PR #29's required end-to-end test.

Two lessons for the rest of the run:
1. A test that asserts a call was MADE is not a test that it SUCCEEDED. When a
   PR moves a caller onto `call_block`, require an assertion on the response,
   not on the invocation.
2. T4 swallowing is what let it merge. The T4 sweep PRs (8-10) are not cosmetic.
The final local run must exercise: the LLM chat page (sidebar lists threads,
history replays), and any other page whose data arrives through `call_block`.

## schemars gotcha, found in PR #30 — read before typing any published field
Replacing a `String` field carrying `schemars(extend("enum" = [...]))` with the
real Rust enum is NOT byte-identical if any variant has a `///` doc comment:
schemars then emits the field as a `oneOf` of `const` subschemas instead of a
flat `{"enum": [...]}` array. PR #30's first regeneration did that to eight
fields, two of which the spec had predicted would be untouched.
Rule: a published enum carries NO per-variant doc comments — put the prose on
the type instead — and that includes a `#[serde(rename = "")] Unset` variant.
With that, the old flat array reproduces exactly.

## Also from PR #30
`error_door`'s eight products entries are still open; a follow-up PR takes
them (the allowlist comment now records the decision rather than a stale
"PR 4" pointer). Deferred with reasons: `StripeEventType` (PR 5), the two
admin `<select>` dropdowns (maud restructure, baseline only checkable in CI),
`routes.rs`'s ~48 hand-written schemas (already-owed follow-up), folding
`pages::commerce_wire` into `util::wire_str`.

## SCHEDULED: products error-mapping PR (declined twice, do not let it slide again)
The `error_door` allowlist's eight `products/` entries carry 29 `NotFound`
classifications (product.rs 9, purchase.rs 5, stripe.rs 5, pages.rs 3,
commerce.rs 3, offers.rs 2, catalog.rs 1, provider.rs 1). PRs #30 and #31 both
declined them for the same good reason: converting them is a behaviour change
(a WRAP denial stops answering 500) that wants per-file fault-injection tests,
and folding it into a PR that also moves enums or markup hides it from review.
It gets its own PR, dispatched after Phase 3 PR 6, titled along the lines of
"fix(products): a denied read stops answering 500". Empty the allowlist there.

## Largest remaining error hole (from PR #33) — owed its own PR after Phase 3
`error_door`'s allowlist is now EMPTY, but the gate has a third blind spot it
cannot see: a bare `err_internal` tail with NO `NotFound` arm above it. About
65 such sites remain, concentrated in `products/stripe.rs`'s webhook dispatcher
and `products/purchase.rs`'s refund orchestration. They mix database, Stripe-API
and orchestration failures in one tail, so separating them is a per-site reading
job, not a mechanical conversion. `error_door.rs`'s module doc now records this
so an empty allowlist is not mistaken for a finished surface.
Also from #33: a denial fixture on a config-reading path needs a `Config`-typed
grant, because config reads are `call_block`'d and WRAP-checked too — otherwise
the handler refuses at "not configured" before reaching the read under test.

## Owed after Phase 3 (from #36), each needing a deliberate decision
1. **The purchases internal read cannot be unified without the deferred
   migration.** `handle_list_user` filters on `user_id`; `Filter`s are AND-only
   so the ownership rule (`buyer_user_id` else `user_id`) is not expressible as
   one list filter, and migration 005 added `buyer_user_id DEFAULT ''` with NO
   backfill — switching outright hides every pre-005 order from its own buyer.
   A backfill belongs with the deferred `_cents` → `_minor` migration.
2. `verify.rs:151-153` `last_verification_sent(..).unwrap_or_default()` — a
   failed read reads as "never sent" and reopens the 60s cooldown. It sits on
   the anti-enumeration path so the RESPONSE must not change, but it should log
   like its neighbours now do.
3. `refresh.rs:78-81` — the refresh-token row lookup answers `InvalidToken` on
   `Err` with only a `warn!`. Same shape as the eight sites #36 fixed; it was
   not in the spec's list and wants its own decision.
4. `stripe.rs:587`'s `let _ = ctx;` is an unused-parameter suppression, not a
   swallow; rename to `_ctx` in a hygiene pass so grep sweeps stop flagging it.
5. `TestContext::call_block` hands the INNER context to a registered block, so
   `FailingDbOpContext` cannot reach a nested block's database calls. #35 built
   `MessagesWriteFails` in `llm/routes/test_support.rs` for this; promote it to
   `test_support.rs` when a second block needs the same shape.

## Upstream (wafer-run) findings from Phase 4 PRs #328 and #330 — for the user
Two CI coverage gaps in wafer-run, found while working there, NOT fixed:
1. `wafer-block-sqlite`'s `vectors` feature is enabled by no CI job and by
   nothing in `scripts/check.sh`, so `src/vector.rs` compiles in no gate.
2. `wafer-block-crypto` has zero in-repo consumers and is built for no wasm
   target in CI, though its docs assert wasm32 compatibility.
Both were run manually in those PRs; they deserve a CI job of their own.

Security defects found and FIXED upstream in #330, both in code impresspress
was about to inherit:
- PBKDF2 verification derived `stored.len()` bytes, so a hash truncated to 8
  bytes verified at 64 bits (PBKDF2 at a shorter dkLen returns a prefix).
  Now fixed at 32 bytes.
- An unrecognised password-hash scheme (scrypt, bcrypt) was reported as a
  password mismatch, so such a user could never sign in and the logs would say
  they kept mistyping. Now a distinct verify error.

Also: `AuthLevel` now derives `Ord` upstream, so the consumer's private
`auth_rank` ladder can be deleted in the consumer half — variant ORDER is now
load-bearing for access decisions, guarded upstream by an exhaustive
`ladder_position` match plus a nine-pair comparison test.

## From Phase 4 PR 5 (#38, one boot lifecycle + RuntimeConfig)
- **Deviation from ruling 5.5, needs the user's yes/no.** The CF *prepared*
  hydration path got `PreparedPlanBootHooks` (a written no-op), not
  `CfBootHooks`. 5.5's own evidence ("removes a copy rather than adding work")
  holds for the two dynamic builds and not for this one: `build_runtime` takes
  its settings from the plan and does zero D1 structural reads there, so the
  seed would add two D1 round trips to a ~132us hydration on every cold isolate.
  One line to flip if the ruling is meant literally.
- **Seed cost, as 5.5 required:** two D1 reads, zero writes on a settled
  deployment — one `find_by_key` for the single `auto_generate` var in the
  workspace (`IMPRESSPRESS__PRODUCTS__WEBHOOK_SECRET`) and one `block_settings`
  `read_rows`. Against 33-39 subrequests for a dynamic build.
- **The spec's "CF prepared path applies grants (fails today)" was wrong.**
  `apply_prepared_plan` copies `structure.wrap_grants` AND
  `deployment_wrap_grants` into the builder and `registration.rs:520-523`
  registers them pre-`build()`. The test shipped as a regression pin, not a red
  test. Anything else in the spec's PR-5 test list predicted to "fail today"
  should be re-verified rather than trusted.
- **The browser never loaded its admin-created WRAP grants** — same accident
  class as 5.5, fixed here (`GrantSource::Database`). It serves the same
  permissions page that writes them.
- **`clippy -p impresspress-cloudflare --target wasm32-unknown-unknown` ERRORS
  on `main`**: `runtime_cache.rs:631` "this loop never actually loops". No CI
  job gates that lane, so the crate has never been clippy-clean on wasm. Worth a
  one-line hygiene PR (a `#[allow]` with the reason, or restructure the loop),
  probably alongside the `blocks/system.rs:5 unused_imports` warning already
  recorded from Phase 2 PR 1.
- **The browser has no test lane for `RuntimeConfig`.** Native is pinned by
  `impresspress/tests/boot_lifecycle.rs`, Cloudflare by three wasm tests; the
  browser's fill lives in `BrowserBootHooks::seed_after_admin_init` and is
  covered by nothing. If phase 5 or 6 adds a browser host-test harness, this is
  the first thing to point at it.
- **`extend_with_request_config` still has two shapes.** `build_runtime` goes
  through `add_request_config` (RuntimeConfig), but `request_services_for_dispatch`
  still calls the plain-HashMap version, because a warm request fills only the
  async surface and has no snapshot to write. PR 6 (`CfEnvironment::capture`)
  should decide whether that second caller wants its own type rather than
  sharing the map helper.

## From PR 5's review fixes (#38, second round) — read before writing a Cloudflare wasm test

- **The Cloudflare wasm test lane was blind to every block.** `impresspress-cloudflare`'s
  `default = []` enables no `can_disable` block, so `blocks::block_enabled_defaults()`
  returns an EMPTY vec in the default test build. Any test about structural
  block-settings seeding passes there without a single write being attempted — which
  is why the request-path-seed regression could not be caught by the obvious test.
  `ci.yml`'s `cloudflare-wasm-test` job now runs the suite twice, the second time with
  `--features full`. A test that needs a non-empty block set must either use its own
  explicit defaults fixture or say out loud that only the `full` lane catches it.
- **`ci-main.yml` has no Cloudflare wasm test job at all** (only `cargo check`), so the
  wasm tests never run on a merge to `main` — only on PRs, and only when the path
  filter fires. Same shape for the browser lane. Worth a small CI PR.
- **The request path is write-free in its BOOT HOOK, not in its whole boot.** On a
  database that has never seen `/_deploy/init`, `migration_helper::apply_if_blessed`
  treats every block as a fresh install and bootstraps its migrations without operator
  consent, and `write_state` then writes a `block_settings` row per block — with
  `bump_on_write: true` on both request paths. That is pre-existing and unchanged, but
  anyone quoting "the request path is physically write-free" should mean the loader and
  the hook, not the whole lifecycle. (Migration-state rows are created with `enabled`
  defaulting to `true`, so a post-admin-init republish can never disable a block.)
- **Editing a migration file's COMMENTS changes the migration hash.** `apply_migrations`
  joins the ordered SQL into one string and `apply_if_blessed` hashes that, so a prose
  fix in `020_normalize_blank_deleted_at.*.sql` makes every request-path boot log
  `schema drift; redeploy with --run-migrations` until the next `/_deploy/init`, which
  then re-runs products' migrations from 001. Done deliberately here; budget for it,
  or leave migration comments alone.
- **`RuntimeConfig::install` changed shape.** It is now
  `install<T>(builder, FnOnce(map) -> (Arc<dyn ConfigService>, T)) -> (ImpresspressBuilder, T)`.
  Targets with nothing to keep return `()`; Cloudflare returns its request-current
  concrete service as `T`. A target whose service is filled by `set` should use the new
  `builder::fill_config_service(service, map)` rather than writing the loop — the
  browser's `|_empty| config_svc` (which discarded the map) was the reason it exists.
- **New Cloudflare seams for PR 6 (`CfEnvironment::capture`).**
  `read_structural_config_inputs(env, ..) -> StructuralConfigInputs` and
  `structural_runtime_config(inputs) -> (RuntimeConfig, overlay)` now split the
  `worker::Env` reads from the assembly (that split is what made the structural-key test
  real). `CfEnvironment` should absorb the first of the two, not both.
- **The three Cloudflare boot funnels are now three named functions**, not one function
  plus a policy argument: `boot_deploy_runtime` (seeds, `Reported`),
  `boot_dynamic_request_runtime` (read-only hook, `Strict`), `boot_prepared_runtime`
  (no-op hook, `PreInstalled` grants). Adding a fourth path means picking one, or
  writing a fourth with its own hook and a reason.
- Still open from the first round and NOT addressed here: the browser has no test lane
  for `RuntimeConfig` / `BrowserBootHooks::seed_after_admin_init`; `clippy -p
  impresspress-cloudflare --target wasm32-unknown-unknown` still errors on `main`
  (`runtime_cache.rs` "this loop never actually loops"); `extend_with_request_config`
  still has two shapes.

## Shipped migration .sql files are hash-immutable (2026-09-08, PR #38)

`migration_helper::apply_if_blessed` computes `sha256_hex(sql)` over the file's
**whole text**, so editing a `--` comment in a migration that has already
shipped moves the hash exactly as far as editing a statement does. On every
deployment that already applied it, `current_hash` and `blessed_hash` then both
differ, every boot logs `schema drift`, and clearing that needs a redeploy with
`--run-migrations`, which re-runs that block's migrations from 001.

PR #38's review finding 5 walked into this: it asked for stale prose naming the
deleted `init_all_blocks` / `strict_init_all_blocks` to be fixed everywhere, and
two of those places were comments inside products migration 020. Those two edits
are **reverted**; the Rust twin beside `slug_collision_cannot_fail_020` carries
the same explanation and is correct. The rule is now written in
`migration_helper`'s module doc next to the hashing step.

**Applies to any future prose sweep.** A grep-and-fix across the repo must skip
`**/migrations/*.sql`. Put the explanation in the block's `migrations/mod.rs`
instead, where it is not hash-addressed.

## From Phase 4 PR 6 (#39, CF split + CfEnvironment + B25)

- **The 900-line goal is unmet for five Cloudflare files and needs its own PR.**
  After #39: `runtime_cache.rs` 2,014 (it GREW — `finish_runtime` removes ~60
  duplicated lines and adds the shared type, its docs and two tests),
  `request_services.rs` 1,223, `kv_cached_db.rs` 1,025, `runtime_build.rs` 998,
  `environment.rs` 954. `lib.rs` is 527 (was 2,924). The real decomposition of
  `runtime_cache.rs` is three concerns — probe policy (`VersionProbe`,
  `probe_version`, the five predicates, the jitter/backoff math), the build slot
  (`BuildGuard`, `BUILD_*` thread-locals, `build_slot_active`,
  `runtime_while_building`), and the cache itself — and it needs
  `ReadyRuntime`'s fields to become `pub(super)`. It is the most
  correctness-sensitive file in the crate on a lane with no CI clippy job, so it
  wants its own review, not a fourth change bolted onto a PR.
- **`impresspress-browser`'s `db_codec.rs` is now the last private copy of the
  shared row codec** (PR 7's row). `build_records` + `first_scalar` become
  `codec::record_from_json_row` + `codec::first_scalar`. One behavioural
  difference to decide out loud: `build_records` returns `Err("expected row
  object")` for a non-object row, where `record_from_json_row` returns an empty
  `Record`. `coerce_param`/`params_to_js`/`rows_from_js`/`empty_params` are
  genuinely bridge-local and stay.
- **`worker::Env` is fakeable in a wasm test.** It is a `#[wasm_bindgen] extern
  "C"` type (a `JsValue` newtype), and `var`/`secret` are the same operation
  (`Secret` is a re-export of `StringBinding`, `Var` an alias) resolving through
  `js_sys::Reflect::get`. A `js_sys::Proxy` with a `get` trap therefore counts
  every read the crate makes, and `JsValue::from(proxy).unchecked_into::<Env>()`
  is a usable `Env`. `environment.rs`'s `RecordingEnv` is the harness; anything
  else in this crate that was "untestable because it needs a `worker::Env`" may
  not be.
- **The eight Cloudflare wasm clippy warnings are still there** (five
  `arc_with_non_send_sync` and two `assertions_on_constants` in test fixtures,
  one `redundant clone` in a `request_services` test). The `never_loop` ERROR is
  fixed, so `clippy --target wasm32-unknown-unknown` is error-free for the first
  time; a hygiene PR could take the warnings and `blocks/system.rs:5`'s
  `unused_imports` together.
- **Still no CI job runs `clippy --target wasm32-unknown-unknown`** for either
  wasm crate, so the lane can regress to an error again the moment someone
  writes one. Worth adding to the existing `cloudflare` job, which already runs
  five `cargo check` steps on that target.
- `impresspress-core` has **no `interfaces` module**; the spec's PR-6 row
  ("`impresspress-core/src/interfaces` re-exports") is wrong. Cloudflare uses
  `wafer_core::interfaces::database::codec` directly, which is also what
  CLAUDE.md's "no implicit mapping layers" wants.
- **Every line number in spec 2.6's split table is stale** (as the brief warned)
  and so is the "17 reads / 7 omissions" count in 2.5: the tree has 18 var and
  secret keys, and the old hand list omitted 8 of them. Any later section
  quoting 2.5/2.6 numbers must re-resolve them.

## From PR #39's review fixes (second round) — read before reading any DB column

- **`RecordExt::str_field` silently answers `""` for a structured value, and
  that is still true.** `#39` fixed the five sites that hit it and added
  `json_text_field` (raw-string arm verbatim, decoded arm re-encoded, absent
  stays `""`), but the accessor itself is unchanged. The louder shape with the
  smallest blast radius is a `debug_assert!` / `tracing::warn!` when
  `get(key)` returns `Some(non-string)`: silent for the absent/NULL case that
  legitimately means `""`, loud only on the exact confusion that hid a
  payments-audit bug for months, and no signature change. Making `str_field`
  return `Option` or split "absent" from "wrong type" touches ~180 call sites
  and wants its own PR.
- **The audit method that finds these.** Cross-check every distinct
  `str_field("…")` key in the tree against every JSON-encoded column: those
  with a `DEFAULT '{}'` / `'[]'` in a migration AND those written through
  `serde_json::to_string` (which caught `pricing_snapshot`, `requirements`,
  `scopes_json`, `diagnostics_json`, `block_info_json`, `condition_snapshot`,
  `inputs_json` — none of which are read via `str_field` today). A DDL-default
  grep alone misses columns written without one. As of #39 the set of
  `str_field`-read JSON columns is empty; re-run this whenever a column starts
  holding JSON.
- **A native-SQLite test reproduces a D1-only-looking codec bug.** The
  in-memory SQLite backend `TestContext` uses sniffs JSON-shaped TEXT on read,
  so it hands back the decoded arm — the same arm D1 now returns after #39.
  Any "this only breaks on Cloudflare" claim about a row codec should be
  checked with a plain `-p impresspress-core` test first.
- **`repo_door`'s `refunds` allowlist now includes `provider_tests.rs`.** No
  product path parks a refund row in `provider_succeeded` across requests
  (`refund_purchase` writes it and settles it a few lines later), and
  `record_provider_response` early-returns once a reconcile has stamped
  `stripe_event_created` — so that state can only be staged by a direct write.
  A future fault-injection harness (`FailingDbOpContext`-shaped, see the
  Phase-3 note above) could reach it legitimately and retire the entry.
- **`RuntimeKind::lifecycle()` vs `ReadyRuntime::kind_label()` are two
  deliberately different granularities** in `runtime_cache.rs`. `lifecycle`
  (`cached` / `transient` / `prepared`) is the *boot-failure* name and encodes
  blast radius: a `cached` failure poisons the isolate until a version or
  config change forces a rebuild, a `transient` one self-heals. `kind_label`
  (`dynamic` / `prepared`) is the log field and is derived from
  `config_version.is_some()` so it cannot be spelled differently at one of the
  four sites that emit it. Do not collapse them.
- **`cargo check --workspace --all-targets` natively reports ~169 errors on
  `main`** — `impresspress-browser`, `impresspress-cloudflare` and
  `impresspress-web` are wasm-only. Not a signal; use the per-crate wasm lanes.

## OWED: land the phase 4 spec and rulings in the repo (2026-09-08)

`phase4-spec-draft.md` and `phase4-rulings.md` live only in the session
scratchpad. Phases 2 and 3 landed theirs in `docs/superpowers/specs/`; phase 4
did not, and the rulings file says it should be prepended to the spec when it
enters the repo.

The cost is already paid: reviewing PR #40, one agent spent its entire budget
concluding that ruling 5.4 was fabricated, because nothing in the repository
records it. A ruling that overrules a committed code review has to be auditable
from the tree.

Land it as its own small PR after PR #40 merges, with ruling 5.5's amendment
(2026-09-07) included.

## From Phase 4 PR 7 fix round (#40, 2026-09-08)

- **CI jobs that shell out to `node` must pin a Node version.** `ci.yml`'s
  `browser-wasm-test` job and `ci-main.yml`'s `wasm` job both run `node --test`
  and `wasm-pack test --node` with **no `actions/setup-node` step**, so they use
  whatever the runner image ships. That was invisible while the step named one
  file; it became load-bearing the moment the step took a glob (the test runner
  only expands one itself from Node 21). Both now pin 22. Any future job that
  runs node needs the same.
- **A new `js/test/*.test.mjs` file is now picked up automatically** by both
  workflows. Do not go back to naming files.
- **`examples/minimal-browser` installs no tracing subscriber.** It builds its
  own `Wafer` and never calls `impresspress-web::initialize`, so
  `logger::init_console_tracing` is not installed there. Harmless today (it
  registers no blocks, so nothing on the LLM path runs), but any second
  non-impresspress consumer of `impresspress-browser` inherits the silent-log
  behaviour F4 fixed. If a real second consumer appears, the subscriber install
  belongs somewhere both entry points reach.
- **The `localhost` pseudo-domain rule lives in `impresspress-core::ssrf`, not
  upstream.** `wafer_net_security::is_blocked_url` matches the bare string
  `localhost` only. If a wafer-run bump ever adds the RFC 6761 rule upstream,
  `ssrf::is_loopback_host` becomes redundant and should be deleted rather than
  left as a second implementation.
- **Both wasm adapters now refuse redirects** (`redirect: 'error'` /
  `RequestRedirect::Error`). A future block that genuinely needs to follow one
  must gate it explicitly and revalidate the hop against `is_ssrf_blocked_url`;
  it must not restore the default. Native stays on
  `ssrf_revalidating_redirect_policy` — three different answers to the same
  question, one per platform, each because of what that platform's client
  exposes.
- **Response-cap constant is now `impresspress_core::streaming::MAX_NETWORK_
  RESPONSE_BYTES`.** Unlike the native `wafer_block_network::service::
  DEFAULT_MAX_RESPONSE_BYTES` it is NOT operator-tunable
  (`WAFER_RUN__NETWORK__MAX_RESPONSE_BYTES` is read only on native). If wasm
  adapters ever gain config plumbing, this is a candidate to unify further.
- **`impresspress-cloudflare` has eight pre-existing clippy lints** under
  `--target wasm32-unknown-unknown --all-targets -- -D warnings`
  (`runtime_cache.rs:1809,1999,2000`, `boot_hooks.rs:403,404`,
  `config_source.rs:344`, `kv_cached_db.rs:929`, `request_services.rs:1105`),
  all test-only. CI excludes the crate from clippy entirely, so they never
  surface. Same shape as the CLI's four known lints: do not add more.
- **`cargo clippy --workspace` cannot run locally** without
  `crates/impresspress-web/pkg/impresspress_web.js` (the `impresspress` CLI's
  build script refuses). CI downloads it as an artifact. Use per-crate clippy
  locally, or run `just build` first.
- **`url::Url::parse` does not normalise a trailing dot RUN.**
  `http://localhost../` parses and yields the host `localhost..` verbatim, so
  any hostname denylist that strips one trailing dot is defeated by a second.
  `ssrf::strip_root_dots` (`trim_end_matches('.')`) is the fix and both
  `is_cloud_metadata_host` and `is_loopback_host` go through it. Any future
  host-string predicate in this repo must use it, not `strip_suffix('.')`.

## FLAKY: the dev-sandbox WebMCP compile test (2026-09-08, PR #41)

`tests/e2e/dev-compile-tool.spec.ts:252` — "dev_compile_block compiles a
scaffolded block, stages it and puts it live; failures are results" — failed on
PR #41 with a sanitized 500 (`{"error":"Internal","message":"Internal server
error (ref: 0e1d793d9715f8fa)"}`) at spec line 396, ten of eleven siblings
passing. PR #41 changed one markdown file and one PNG, and the suite had passed
on PR #40 with an equivalent tree. Re-running the same job on the same commit
passed both dev-sandbox jobs.

So it is flaky, not a regression. But **note it for the final WebMCP validation**,
which is the last task of this run: a tool path that intermittently answers 500
will look like a broken tool when that validation runs. If it reappears there,
the sanitized ref is the thread to pull — the server-side log carries the real
cause behind that ref, and the Phase 3 error discipline is what put the ref
there.

Not yet diagnosed. Suspects worth checking first if it recurs: the real Rust
compile in that test is resource-hungry, and the runner was under load.
## From Phase 4 PR 8 (#44, runtime capability)

- **Building `impresspress-cloudflare` alone measures nothing.** The crate
  exports no `#[event(fetch)]`, so the linker discards every block and
  `target/wasm32-unknown-unknown/release/impresspress_cloudflare.wasm` is
  606,524 bytes with `default`, with `--features block-llm`, and with
  `--features full` alike. Any future wasm-size claim about this target must
  be measured against a real consumer — `examples/webmcp-demo` is the one in
  tree. Its numbers on this branch: 7,790,612 as shipped, +445,656 for
  `block-llm`, +736,120 for `block-vector`.
- **`cargo check -p impresspress-core --target wasm32-unknown-unknown
  --all-targets` cannot work.** `--all-targets` pulls the tokio/mio
  dev-dependencies, which do not build for wasm32 (mio fails with 48 errors).
  The crate's wasm32 lane can only check the lib. Any test written under
  `cfg(target_arch = "wasm32")` in `impresspress-core` is therefore never
  compiled by anything; it documents, it does not gate. The executable wasm32
  lane is `impresspress-cloudflare`.
- **`LlmError` is `#[non_exhaustive]` upstream**, so any match on it needs a
  wildcard. `routes/providers.rs::llm_error_response` puts the wildcard on the
  *sanitizing* arm deliberately — an error shape this repo has not classified
  is not one to echo to a client.
- **`wafer_core::interfaces::llm::handler::llm_error_to_block_error` is
  private upstream.** #44 mirrors its arms in
  `blocks/llm/routes/providers.rs` because the feature block's admin HTTP
  surface needs the same mapping. Making it `pub` upstream would delete the
  copy; worth a small wafer-run PR.
- **Phase 5 (UI):** the llm providers admin page (`blocks/llm/ui.rs`) renders
  a create form and per-row discover/delete buttons unconditionally. On a
  runtime with a `NoopProviderAdmin` those now answer 501 instead of a silent
  200 — better, but the page still offers controls that cannot work. The page
  should read `ProviderAdmin::manages_providers()` and say so. Same shape as
  the other "the page is a second listing of the block's surface" items.
- **`ProviderAdmin` gained `manages_providers()`.** Any new implementation
  must answer it consistently with `configure` — `inert_router_tests` pins
  both implementations, and a third would need its own pair.
- **`impresspress-core/tests/manifest_block_parity.rs` is the mechanism** that
  replaced the two "keep in sync" comments between the cloudflare and web
  manifests. A new `block-*` feature in `impresspress-core` now fails that
  test until it is either offered by all three crates or listed in `DIVERGENT`
  with a reason. `toml` is a new dev-dependency of `impresspress-core` for it.
- **The Cloudflare `block-llm`/`block-vector` passthroughs are in no preset**,
  so `ci.yml`'s new "Check impresspress-cloudflare optional blocks (wasm32)"
  step is the only thing that compiles them. If that step is ever dropped they
  rot exactly the way the comment they replaced did.
- **`registration.rs`'s middleware anchor is now the single list.** Adding a
  `wafer-run/*` middleware block is one edit, but the named crate must itself
  invoke `register_static_block!` — a crate anchored for any other reason has
  no `__WAFER_STATIC_BLOCK` and fails to compile on wasm32 only.
- Still open and untouched here: `impresspress-cloudflare`'s eight
  pre-existing test-only clippy lints on the wasm target, `blocks/system.rs:5`
  `unused_imports` under `embed-assets` off, and the absence of any CI job
  running `clippy --target wasm32-unknown-unknown`.

## From PR #44's review fixes (2026-09-08)

- **A `cfg(target_arch = "wasm32")` test in `impresspress-core` is compiled by
  nothing.** `cargo check -p impresspress-core --target wasm32-unknown-unknown
  --all-targets` cannot work — `--all-targets` pulls the tokio/mio
  dev-dependencies, which fail for wasm32 with 48 errors — and no CI job builds
  that crate for that target at all. The `wasm` in some existing CI lines is a
  cargo FEATURE name, not a target. So a wasm32-gated assertion written in
  `impresspress-core` documents; it does not gate.
  **The workaround the repo now uses:** factor the wasm-only logic into an
  ungated free function and assert it from `impresspress-cloudflare`, which has
  a real `wasm-bindgen-test` lane (89 tests) and a path-filtered CI job.
  `builder::register_middleware_blocks` + `builder::MIDDLEWARE_BLOCKS` and
  `impresspress-cloudflare::middleware_blocks_tests` are the worked example;
  `blocks/rate_limit.rs:226` is the older precedent. Do not write another
  assertion that nothing compiles.

- **The test harness enforced ONE of production's two call_block gates, and its
  comment claimed it enforced both.** `TestContext::call_block` checked WRAP
  grants and skipped `caller_requires` — the allowlist
  `Wafer::make_block_context` installs and `RuntimeContext::dispatch_call`
  checks ABOVE the grant check. Every cross-block call in the repository ran
  more permissively than production for as long as the harness existed, and it
  hid a real defect (`blocks::vector` calling two blocks it never declared).
  Now enforced. `with_wrap` takes the caller's `requires` as its second
  argument; a test acting as a real block must source it from
  `<Block>::new().info().requires`, never re-list it.

- **Two harness frames were being modelled as one, and adding the gate exposed
  it.** Both are now fixed, and both matter to anyone writing a fixture:
  1. `TestContext::dispatch` runs `routing::route_to_block`, which is the
     ROUTER's code. It now routes from `as_router()` (no allowlist), because
     the impresspress router block declares no `requires`. Routing through the
     impersonated block's own allowlist refused the block the request was
     addressed to — 36 dev-sandbox tests.
  2. `TestContext::call_block` now hands the callee a sub-context carrying the
     CALLEE's declared `requires` (`for_callee()`), which is what
     `dispatch_call` builds. A block's outbound calls are gated by its own
     declaration, never its caller's.
  Still NOT modelled (unchanged, and still the reason `FailingDbOpContext`
  cannot reach a nested block's database calls): production also re-points the
  sub-context's IDENTITY, so a callee's own database access is authorized as
  the callee. `TestContext` keeps `caller_id` fixed across the hop.

- **`grants` and `requires` are different kinds of data, and the harness API now
  says so.** A `ResourceGrant` is published by the block that OWNS a resource,
  naming who may reach it — which is why `userportal` tests pass
  `auth::service::auth_grants()`. A `requires` entry is owned by the CALLER.
  They are separate parameters for that reason; do not try to derive one from
  the other.

- **`requires` is a call-time allowlist, not a load-time dependency.** Naming a
  block that is not registered costs nothing (the call simply 404s, or the
  caller degrades). So the fix for "block A calls B but does not declare it" is
  always to declare it, never to add a cargo feature.

- **The webmcp-demo size command in `impresspress-cloudflare/Cargo.toml` was
  wrong and is fixed.** `examples/webmcp-demo` declares no `block-*` features
  of its own, so `--features target-cloudflare,block-llm` errors. The working
  spelling names the dependency:
  `--features target-cloudflare,impresspress-cloudflare/block-llm`.
  Numbers at this branch tip, for reference: 7,791,213 / 8,236,933 / 8,527,442
  bytes. They are quoted ROUNDED in the manifest on purpose — they move every
  commit, and wasm-opt + gzip preserve none of these ratios, so they are the
  relative cost of adding the blocks, not what a consumer pays against the
  platform limit.

- **A `DIVERGENT`-style table that names only the differing item is not a
  gate.** `manifest_block_parity`'s table said "this feature is not offered
  everywhere" and nothing more, so a divergent feature dropped from one MORE
  crate still matched. Each entry now names the crates that do offer it, and
  the test asserts that set. Any future "here is the list of known exceptions"
  table wants the same shape.

- **Cargo catches some of what the parity test would.** A block feature named in
  the CLI's own `[features].default` cannot be deleted without cargo itself
  refusing the manifest. The parity gate's real value is for features NOT in a
  default — `block-fastembed`, `block-dev` — which is exactly where the
  offering-set assertion bites.

- **`ingest` refuses a whitespace-only body on a deployment with no embedder**,
  where it used to clear the document's chunks and answer 200. Deliberate: the
  resolver moved above the delete so a runtime that cannot embed modifies
  nothing, and making "were my chunks cleared?" depend on the request body
  would be worse than either answer. An emptied document on a deployment that
  CAN embed still clears its chunks.

- **Phase 5 (UI), still open:** the llm providers page now hides its create form
  and per-row actions when `manages_providers()` is false, but `models_page`
  was not touched — it renders an aggregated table off `list_models`, which has
  no admin action to refuse, so there was nothing to gate. Re-check if that
  page ever gains a write control.

## FLAKY, second sighting — the same WebMCP 500 (2026-09-08, PR #44)

Updates the PR #41 entry above. Two occurrences now, and they are the **only**
two failures across seven surveyed runs:

| PR | job | spec | outcome |
|---|---|---|---|
| #41 | browser WASM + wasmi guest | `dev-compile-tool.spec.ts:252` | green on re-run |
| #44 | Rubrc compiles a block in the browser | `dev-compile.spec.ts:206` | green on re-run |

Different jobs, different spec files, **same helper** (`structured()` asserting
`isError` falsy) and the same shape: a WebMCP tool answering
`{"error":"Internal","message":"Internal server error (ref: …)"}`. Refs so far:
`0e1d793d9715f8fa` (#41) and `a17c66069e2a989c` (#44).

Ruled out for #44 specifically: the middleware-list deletion in that PR is not
the cause. The sibling job on the same run passed all eleven of its tests,
including the ones that boot the browser runtime, serve a seeded site, stage a
guest and survive a restart. Missing middleware would fail those first.

**Carry this into the final WebMCP validation, which is the last task of the
run.** Two sightings of a 500 in a tool path is not noise, and re-running until
green is not a diagnosis. The sanitized ref is the thread: the server-side log
carries the real cause behind it, and the Phase 3 error discipline is what put
the ref there. Start by capturing the server log alongside the failure rather
than only the client-side assertion.

Untested hypothesis, cheapest first: both failing tests do a real Rust compile
and the runner was under load, so a timeout or a resource limit surfaces as a
sanitized 500 rather than as a timeout. If that is it, the defect is that a
resource failure is being reported as an internal error.

## From Phase 5 PR 4 (#50, administration tables onto the shared component)

- **The HTML render-dump control needs normalising to be stable.** Dumping the
  20 administration renders twice from the *same* tree produces 7 differing
  files: seeded uuidv7 ids, `created_at` stamps and measured `duration_ms`
  values move between runs. Normalising uuids / ISO stamps / dates / `Nms` to
  placeholders before diffing makes it exact, and then the control is
  byte-precise. `p5pr4-normalize.py` in this scratchpad is the script; any
  future use of this control should start from it rather than diffing raw
  dumps and concluding the technique is noisy.
- **Local PNG regeneration cannot be compared to the committed baselines at
  all.** 52 of the 64 differ on this machine against CI-rendered images, for
  font-rendering reasons. That is why every baseline claim in this phase came
  from an HTML render diff, not from `regen-local.sh`.
- **`table.css:317` styles `.users-table td[data-label="Created"]` and no Rust
  code has ever emitted `.users-table`.** The rule has been dead since it was
  written. Applying that class to the migrated administration users table would
  bring it alive (nowrap on the Created column) — deliberately not done in #50
  because it is a rendered change unrelated to the migration. Either wire it up
  or delete the rule; ruling 5.6 item 6 is the natural home.
- **`DataTable` has no per-cell attribute affordance and should keep not having
  one.** The component stamps its own `<td>` attributes and carries the cell's
  inner markup through verbatim. Anything a caller must attach to a cell goes
  *inside* the cell — that is why PR #47's mask hooks (`<time>`,
  `<span data-volatile-metric>`) survived the migration untouched. The one case
  that looked like it needed row attributes (the network page's
  `data-detail-target` / `data-detail-url`) was solved instead by
  `TableRow::after`'s guarantee that the detail row is the row's
  `nextElementSibling`, with the URL on the detail pane the page still writes
  itself.
- **`page_link_tests::seeded_ctx` still leaves 4 of the 19 tables unrendered.**
  #50 seeded an error request log, a storage access row and an audit entry, so
  15 tables render a row. The database schema panel and SQL result grid need a
  selected table (`PAGES` renders `/b/admin/database` with no `?table=`), and
  the block-detail modal's endpoint and configuration tables need a probe block
  that declares endpoints and config keys — it declares neither. That second
  gap is the same one PR #49 hit for `method_badge_tone` / `auth_badge_tone`.
  Widening `PROBE_BLOCK`'s `BlockInfo` would close both, but it also changes the
  variables "by block" tab, the permissions page and the block list, so it wants
  its own change with its own before/after.
- **Two administration tables rendered a header over an empty `<tbody>` with no
  message** — the code-declared grants table and the "All Variables" tab. The
  component renders its empty slot *in place of* the whole table, so both had to
  be given words. Any future migration of a raw table should check for this: the
  raw markup's "silent empty table" has no equivalent in `data_table`.
- **Newly activated masks are a reporting obligation, not a defect.** Migrating
  a table whose column is named `Owner`, `Created` or `Created By` makes the
  visual suite's `td[data-label=…]` mask match for the first time, which repaints
  those cells magenta with no change to what the page draws. In #50 that is the
  users page and the dashboard's Recent Users card. The 10 remaining
  non-administration raw tables will do the same when they migrate.

### Added after PR #50's regeneration (the most reusable finding of the phase)

- **Every administration visual baseline is 1280×720 — one viewport, not the
  page.** `COMMON_OPTS` sets `fullPage: true`, but the administration shell
  scrolls its content pane internally instead of growing the document, so
  `fullPage` has nothing extra to capture. Verified on the committed PNGs
  (`admin-admin-{dashboard,users,network}-desktop` are all 1280×720).
  **A markup change only moves a baseline if it renders in the first
  viewport.** PR #50 migrated two tables on the dashboard and its baseline did
  not move, because "Recent Users" and "Recent Errors" start below 720px. Ruling
  5.6 item 6's ten remaining raw tables should be assessed by where they sit in
  frame, not by which page they are on.
- **Do not assume a byte-identical baseline is a stale one.** The sub-tolerance
  trap is real (PR #47), but it is a hypothesis, not a conclusion. The cheap
  test is the documented delete-and-regenerate: delete the baseline in a commit,
  run `regen-visual-baselines.yml`, and compare the recreated image to the old
  one. In PR #50 it came back byte-identical (67,165 bytes, `cmp` exit 0), which
  proved the page unchanged and made the delete/recreate pair a no-op that was
  dropped from the branch. Twelve minutes of CI, and it is the only way to tell
  "stale" from "unchanged" apart.
- **The regeneration itself is good evidence about scope.** PR #50 moved exactly
  4 images out of 92 across three baseline trees. That nothing outside
  administration moved is the independent confirmation that the `data_table`
  shorthand's bytes did not shift under the 16 products call sites — stronger
  than the unit test that pins it, because it exercises the real render.
- `gh pr edit --body-file` fails on this repository with the projects-classic
  GraphQL deprecation. `gh api -X PATCH repos/{owner}/{repo}/pulls/{n} --input`
  with a JSON `{"body": …}` works; `p5pr4-set-body.sh` in the scratchpad is the
  wrapper.
- **`push-only.sh` in the scratchpad hardcodes a branch name** (it still says
  `phase5/admin-badges`) and will push and recreate a merged branch on the fork
  if used from a different one. It did exactly that during PR #50 and the branch
  was deleted again immediately. Either parameterise it or use
  `git push origin <branch>` directly.

## FLAKY, third sighting — the same WebMCP 500 (2026-09-08, PR #50)

Updates the #41 / #44 entries above. **Three occurrences now**, and the third is
byte-for-byte the same shape as the second:

| PR | job | spec | ref | outcome |
|---|---|---|---|---|
| #41 | browser WASM + wasmi guest | `dev-compile-tool.spec.ts:252` | `0e1d793d9715f8fa` | green on re-run |
| #44 | Rubrc compiles a block in the browser | `dev-compile.spec.ts:206` | `a17c66069e2a989c` | green on re-run |
| #50 | Rubrc compiles a block in the browser | `dev-compile.spec.ts:206` | `d4aee51481740aa5` | re-run triggered |

PR #50 changes administration table markup and a UI component. It touches no
dev block, no compile path and no WebMCP tool. The sibling job on the same run
(`browser WASM + wasmi guest`, 21 minutes) passed. So this is now three
sightings across three unrelated pull requests, and the "resource pressure
surfacing as a sanitized 500" hypothesis is the one still standing — the two
`dev-compile.spec.ts:206` failures both took a real Rust compile in the browser
and both ran alongside a heavy sibling job.

**This is a recurring failure, not noise, and re-running until green is still
not a diagnosis.** Carry it into the final WebMCP validation with the same
instruction as before: capture the server-side log alongside the client
assertion, because the sanitized ref is the only thread to the real cause. If
it is a resource limit, the defect is that a resource failure is reported as an
internal error.

## DIAGNOSED AND FIXED — the four sanitized 500s were one bug (2026-09-09, PR #51)

Closes out the #41 / #44 / #50 entries above. The "resource pressure" hypothesis
was wrong; the cause is an unlocked read.

`DevShared::workspace` serialises the manifest's **writers against each other
and nothing against readers**. All five holders were mutators (`gc.rs:220`,
`files.rs:157`, `files.rs:251`, `activation.rs:546`, `scaffold.rs:218`) and
three read paths took no lock: `files::handle_read`, `files::handle_list` and
`gc::storage_usage` — the last of which backs `GET /b/dev/api/status`, the poll
the `/b/dev` page runs ~3×/s while a tool call is outstanding. In the browser
every storage call is a suspension point and fetch events dispatch
concurrently, so those reads interleave with the collector's delete-and-rewrite.
PR #51 makes all three take the lock, `handle_read` across the blob fetch too.

**Two distinct failure modes, and only one of them is testable on the host:**
1. `workspace::load` snapshots the object then reads its bytes; a rewrite
   between the two invalidates the snapshot and the browser storage layer
   reports it as an internal error. **Not reproducible in a fixture** — a `get`
   on `InMemoryStorageService` is atomic, so no interleaving produces a torn
   read. `handle_list` and `storage_usage` are covered by argument only.
2. A read holding a stale manifest fetches a blob the collector has since
   reclaimed. Reproducible, and PR #51's
   `dev_files::a_read_holds_off_the_write_and_the_collection_that_would_pull_its_blob`
   fails against the pre-fix handler with exactly the CI shape.

### CANDIDATE, out of scope and deliberately not attempted: per-key locking in the browser storage bridge

The dev block is only where this surfaced. The bridge serialises **nothing per
key**, so any block that reads and writes the same key concurrently has the same
race; the dev block is just the one with a garbage collector rewriting a file
other requests read. A per-key lock in the bridge is the principled cure. Do not
start it before the part-one evidence below has produced one real capture — the
whole reason this took four sightings to diagnose is that the cause was inferred
rather than read.

### New technique: a fixture CAN express an interleaving now

`InMemoryStorageService::hold_next_get(folder, key)` (and
`TestContext::hold_next_storage_get(block, folder, key)`) parks one named `get`
and hands back a `HeldGet`. This is the general answer to "nothing in the
fixture yields, so ordering bugs are invisible" — the same problem
`gc::GcInterleave` solves for the collector, but usable from any handler without
a production seam. **The park is bounded (256 polls) and the bound is
load-bearing, not defensive:** when the code under test is correct the other
half of the join blocks on the lock the parked read holds, so the release can
never arrive, and exhausting the budget is the outcome that proves
serialization. Assert `was_reached()` so the test cannot pass by never firing.

### Diagnostics that were missing, now present

`err_internal` logs the real cause behind the ref by design — but in the browser
that log is a `console.error` inside the **service worker**, and nothing recorded
it, which is why the cause behind all four refs is gone.
- `playwright.config.ts` now sets `trace: 'retain-on-failure'`; both dev-sandbox
  CI jobs already upload the report, which embeds the trace.
- `e2e/fixtures/dev-sandbox.ts::forwardSandboxDiagnostics`, called from
  `bootServiceWorker` (all six sandbox specs go through it), prints worker and
  page errors/warnings to the runner's stdout, which CI tees into the job log.
- **CORRECTED (2026-09-09, PR #51 review round 2): a service worker's console
  reaches the worker handle too.** In the pinned Playwright (1.59.1)
  `playwright-core`'s `browserContext` console dispatch emits ONE message to
  three places: `worker.emit(Worker.Console)`, `context.emit(
  BrowserContext.Console)`, and `page.emit(Page.Console)` for every page inside
  the worker's scope. `dev-scenario.spec.ts::captureServiceWorkerConsole`
  already uses `worker.on('console')` successfully. `msg.page()` IS null for a
  worker message, so that part stands.
  Prefer `worker.on('console')` when a spec wants the worker alone; prefer
  `context.on('console')` when it wants page and worker together, or when the
  listener must be armed before the first navigation (the worker does not exist
  yet, so the worker route needs `context.on('serviceworker')` first and would
  miss boot-time logs).

## From PR #51's review fixes (2026-09-09) — read before writing an async ordering test

- **`tokio::join!`'s halves do not start where you think.** A handler reaches
  its parked `get` only after some database reads, so which half of a join
  suspends first is not a property of the code under test. In PR #51's first
  activation test the MUTATION ran first, took the one-shot `hold_next_get`
  with its own manifest load, and left `was_reached()` AND `budget_expired()`
  both true for an interleaving that never happened. The fix is an explicit
  `once_parked(&hold)` at the top of the mutating half (poll `was_reached` with
  `tokio::task::yield_now`, bounded, panic on giving up). Any future use of
  `hold_next_storage_get` needs it.
- **`HeldGet::POLL_BUDGET` is not "defensive", it RACES the mutation.** The
  parked read is re-polled on every suspension of the other half, so a budget
  in the hundreds expires mid-write for reasons unrelated to the lock under
  test — the mutation has not landed, the read resumes on the old state, and
  the test passes against unfixed code. Measured on this branch: at 256, the
  read test passed 4 runs in 5 with `handle_read`'s own lock deleted. It is
  `100_000` now (tens of ms before a genuine deadlock is reported). This is the
  same shape `cc67a3d8` recorded, reproduced inside the test written to avoid
  it.
- **A park that ended on its budget and one that ended on a release resume
  identically.** `HeldGet::budget_expired()` was added because nothing else
  distinguishes them, and every serialization assertion in the suite rests on
  the difference. Assert it alongside `was_reached()`.
- **`TestContext::storage_get` / `hold_next_storage_get` composed the store
  folder wrongly for a block's namespace ROOT.** `format!("{block}/{folder}")`
  gives `impresspress/dev/` for `workspace::FOLDER` (`""`), where the store's
  key is `impresspress/dev` — `blocks::storage::resolve_folder` returns the
  caller id alone for an empty folder. A hold on the wrong key parks nothing
  and fails silently. `test_support::store_folder` is now the one rule, used by
  both.
- **OPFS failure-mode taxonomy, for anyone mapping a browser storage error.**
  `storagePut` commits an overwrite by swapping the file underneath the handle
  (`createWritable()` + `close()`), so a `File` snapshot taken before the swap
  fails `arrayBuffer()` with a `NotReadableError`, NOT a `NotFoundError`.
  `impresspress-browser`'s `map_rejection` sends everything but `NotFoundError`
  to `StorageError::Internal`. So "the read lost a race with a save" arrives as
  an INTERNAL error, and any handler trying to turn that race into a retry-able
  conflict by keying on `NotFound` will not catch it — the fix is to hold the
  lock over the read, not to remap the code.
- **The dev block now holds two deliberate and opposite strategies for one
  race, cross-referenced from both sides.** Manifest reads are always locked;
  content reads are locked when bounded (`files::handle_read`, capped at
  `MAX_FILE_BYTES`) and mapped to a retry-able 409 when unbounded
  (`export::assemble` + `content_gone`). Do not "unify" them without pricing
  the export.
- **`activation::workspace_site` is the only `DevShared::workspace`
  acquisition inside the activation queue.** Its deadlock argument is written
  out at the function; any future acquisition inside `activate` must re-check
  claim 2 (sequential with `adopt_site`, not nested) and claim 3 (`compile →
  workspace` is the only order and has no reverse edge).
- **The `/b/dev` status poll now has an in-flight guard** (`assets/dev.js`).
  Locking `gc::storage_usage` put a FIFO mutex behind a fixed-interval poll,
  which would have accrued one waiter per 300 ms tick during a collector pass
  and queued the user's next save behind all of them. Any future timer-driven
  poll that reaches a lock needs the same.
- **`harness.mjs` can deliver interval ticks now** (`fireInterval`, plus a
  `statusGate` option). A `dev.js` test about timer behaviour no longer has to
  wait 300 ms per tick.
- **`@playwright/test`'s declared floor was `^1.40.0`**, below the version the
  console routing was verified against; CI was safe only through the lockfile.
  Raised to `^1.59.1`. Any future dependency on a Playwright behaviour should
  raise the floor rather than rely on the lock.
- **`retain-on-failure` is not free on a green run.** Playwright records the
  trace for every test and deletes it on pass, so the recording overhead is
  paid by the whole suite — which matters in the dev-sandbox jobs, one of which
  publishes a timing baseline. `on-first-retry` is the cheaper setting if it
  ever shows up there.
- **Do not `git checkout -- <file>` to undo a temporary red-check edit** in a
  worktree whose branch tip predates your work: it restores from HEAD and
  silently discards every uncommitted change to that file. Copy the file to the
  scratchpad first and restore from the copy.

## From phase 6 PR 3 (#56, `phase6/ci-main-parity`) — the merge gate now equals the PR gate

### For PR 4, the reusable-workflow extraction — this is what changed under it

- **`ci-main.yml` is now built from `ci.yml`'s bodies.** Fourteen of the
  seventeen shared jobs are **byte-identical** (verified by extracting each job
  block and `diff`ing: 0 lines). The other three — `cloudflare-wasm-test`,
  `browser-wasm-test`, `sdk` — differ **only** by the skip gate. So the
  extraction has no judgement calls left, which was the whole point of ruling
  6.4's ordering.
- **The skip gate is the one thing that must become an input.** All three gates
  compute `git diff --name-only ${{ github.event.pull_request.base.sha }}..HEAD`
  and set `skip=true|false`; `base.sha` does not exist on `push`. The reusable
  job should take a `skip` **boolean input** computed by the caller (`ci.yml`
  computes it from the diff, `ci-main.yml` passes `false`), rather than each
  copy carrying its own gate. Note the gate also drags `fetch-depth: 0` onto the
  checkout — that must move with it.
- **`ci-main.yml`'s job id `wasm` is now `cloudflare`,** matching `ci.yml`. The
  display name was already identical, so no runs-interface listing changed.
- **Four browser wasm steps moved.** They had been folded into `ci-main`'s
  `wasm` job (unconditionally, as compensation for `browser-wasm-test` being
  PR-only); they now live in the real `browser-wasm-test` job. If PR 4's
  before/after job-listing diff shows them under a different job name on the
  merge side, that is this move, not a regression.
- **Two comments in `ci.yml` were reworded** so the same text is true in both
  files ("at PR time" → "in CI"; "path-filtered to …" → "path-filtered on the
  pull-request side to …"). Nothing else in `ci.yml` changed. If a future edit
  reintroduces PR-only language into a shared job body, byte-identity breaks.

### Candidate, deliberately not done here: `workflow_dispatch` on `ci-main.yml`

`ci-main.yml` cannot be run before it merges — it triggers on `push` to `main`
and has no manual dispatch — so PR 3 could not prove it green the way PR 2
proved its sibling. Adding dispatch is **not** a one-line change: the workflow
declares `concurrency: group: ci-main` with `cancel-in-progress: true` and a
**fixed** group name, so a dispatched run would cancel a real merge run in
flight. Making it safe means also making the group ref-aware
(`ci-main-${{ github.ref }}`), which is a second behaviour change.

**PR 4 wants this far more than PR 3 did**: ruling 6.5 requires the extraction to
diff the merge run's flattened job-and-step list before and after to empty, and
without dispatch the "after" listing only exists once the extraction has already
merged. Consider landing the concurrency-group change plus `workflow_dispatch` as
the first commit of PR 4.

### Candidate: `cross-compile` and `coverage` are still merge-only

Left alone by PR 3 on purpose (the asymmetry that loses coverage is the *merge*
gate checking less, not more). But a pull request that breaks the Windows or
darwin build still lands green and turns `main` red. Adding the matrix to the
pull-request gate costs ~16 runner-minutes and ~4 minutes of wall clock. PR 4
extracts that matrix into a reusable workflow anyway, at which point calling it
from `ci.yml` is one `uses:` line — decide there.

### What a merge now costs, measured

Baseline merge run `34296258715` (merge of PR #53): 66.2 runner-minutes, 25 min
wall. After PR 3, **≈ +11.6 runner-minutes (~78m, +18%)**, wall clock unchanged:
the critical path is still `build-wasm` (4.3m) → `E2E dev sandbox (Rubrc …)`
(10.9m), and the longest new chain (`build-wasm` → `e2e-build` 4.3m →
`e2e-visual` 1.5m ≈ 10.1m) fits inside it. Per-job deltas: cloudflare-wasm-test
+1.2m, browser-wasm-test +2.0m, e2e-build +4.3m, e2e-smoke +1.1m, e2e-visual
+1.5m, `Format & Lint` +3.0m (four added steps, two of them cargo builds),
`Cloudflare wasm32 check` **−1.5m** (four steps moved into their own parallel
job).

### Correction to the PR 3 brief: the intermittent failures are NOT in the added jobs

The brief warned that "two of the added end-to-end jobs are the ones that have
produced this run's only intermittent failures". They are not. Both sightings
(`flake-note.md`, `flake-note-2.md`: PR #41 `dev-compile-tool.spec.ts:252`, PR
#44 `dev-compile.spec.ts:206`, both a WebMCP tool answering a sanitized 500) were
in **`e2e-dev-sandbox` and `e2e-dev-compile`**, which already ran on every merge
before PR 3. The three jobs PR 3 adds — `e2e-build`, `e2e-smoke`, `e2e-visual` —
have no recorded intermittent failure. So a red `E2E dev sandbox` job on a merge
is the known flake and predates this change; a red `E2E visual baselines` is new
and should be treated as real (most likely a genuine baseline drift that used to
be caught only at pull-request time).

### The deploy path filters are complete — nothing left there

PR 3 re-read both after PR 2's finding. `deploy-demo.yml` and
`deploy-dev-sandbox.yml` each list the CLI's full dependency closure, including
`crates/impresspress-native/**` (the third path dependency PR 2's review caught),
plus `Cargo.toml` and `Cargo.lock`. No further gap.

### `scripts/**` in `ci-main.yml`'s filter is now load-bearing

PR 2 added it. PR 3 depends on it: the two lints it wires into `ci-main`'s
`check` job are `scripts/grep-guard-html.sh` and `scripts/audit-wrap-grants.sh`,
so without that filter entry the merge gate would carry a security audit whose
own script could change without ever running it. Do not drop `scripts/**`.

### Candidate: no automated guard keeps the two workflows in parity

PR 3 verified parity with three throwaway scripts (a flattened `(job, step)`
comparison, a per-job body `diff`, and a `yaml.safe_load` comparison of every
step's `uses`/`run`/`env`/`if`). None is committed, because ruling 6.5's evidence
requirement is that `ci-main` gain *exactly* seven steps and five jobs and
nothing else — a parity-guard step would have been an eighth. After PR 4 the
guard is moot: there is one body, and parity is structural rather than checked.
If PR 4 is ever abandoned, commit the checker instead.

### Amendment to the note above: it was three comments in `ci.yml`, not two

The first PR-3 commit reworded two. Sweeping the shared job bodies afterwards —
`grep` for `path.filter`, `skip`, `PR`, `pull.request` over every comment in
`ci.yml` — turned up a third and fourth line pair, both in the `cloudflare` job
and both describing `cloudflare-wasm-test` as path-filtered, which is true only
on the pull-request side. Fixed in a second commit. **Generalisation for PR 4:**
comments are the part of a shared job body that quietly stops being true when the
body is shared. Before extracting, grep the reusable body for language that
assumes one trigger ("at PR time", "path-filtered", "on PRs", "would skip it"),
because none of it fails a build.

## From phase 6 PR 4 (#57, `phase6/reusable-workflows`) — the two gates are one body now

### What the file layout is now

- `.github/workflows/ci-shared.yml` — the seventeen shared jobs, `workflow_call`,
  one input (`pull-request`, boolean). **This is where every CI job body lives.**
  `ci.yml` (78 lines) and `ci-main.yml` (194 lines) are callers.
- `.github/workflows/build-wasm.yml` — the wasm builder, its own `workflow_call`
  workflow. Both callers run it as job id `build-wasm` and hand the ordering to
  the shared call with `needs: build-wasm`. It is separate from `ci-shared.yml`
  **only** because `ci-main.yml`'s merge-only `cross-compile` and `coverage`
  depend on it, and a job cannot depend on a job inside a called workflow.
- `cross-compile` and `coverage` stay inline in `ci-main.yml`, byte-identical.

### Every job display name gained a prefix — this is unavoidable

`workflow_call` renames the called jobs to `<caller job name> / <job name>`:
`Format & Lint` → `ci / Format & Lint`, `Build impresspress-web wasm` →
`build-wasm / Build impresspress-web wasm`. There is **no way to suppress it**.
Both callers use the same two job ids (`build-wasm`, `ci`), so the prefix is the
same on both sides. Consequences for later work:

- Any before/after comparison of the runs interface must strip the prefix
  (`sed -E 's/^(ci|build-wasm) \/ //'`) or it diffs to noise.
- The fork has no branch protection and no rulesets today, so nothing broke. If
  required status checks are ever turned on (here or upstream), they must name
  the **prefixed** job names.

### actionlint DOES validate a caller's `with:` — but only when run in place

**Correction (2026-09-09, PR #57 review).** An earlier version of this note said
actionlint does not cross-check a caller's `with:` keys against a local
reusable workflow's declared inputs. **That is false, and the lesson was
backwards.** Re-run inside the real worktree, `actionlint` 1.7.7 catches it:

```
$ actionlint .github/workflows/ci.yml     # after s/pull-request:/pull_request_typo:/
ci.yml:76:11: input "pull-request" is required by "./.github/workflows/ci-shared.yml"
              reusable workflow [workflow-call]
ci.yml:78:7:  input "pull_request_typo" is not defined in
              "./.github/workflows/ci-shared.yml" reusable workflow.
              defined input is "pull-request" [workflow-call]
exit=1
```

Deleting the whole `with:` block exits 1 too, on the first of those.

**The reason the original experiment saw exit 0 is the thing to carry forward.**
It ran on a `cp` of `.github/workflows/` into a scratch directory with **no
`.git`**. actionlint resolves a local `uses: ./…` only after finding a project
root by walking up for `.git`; with no repository root it silently skips *every*
project-aware check — local workflow resolution, caller input and secret
checking, local action validation — and exits 0. Same tree, same binary, same
mutation: exit 1 in place, exit 0 on the copy.

**So: lint workflows IN PLACE. Never lint a copy.** A green actionlint on a
`workflow_call` change is meaningful — but a green one on a `.git`-less copy
means nothing at all, and looks identical.

Two riders:

* actionlint is **not wired into CI anywhere** in this repository — no workflow
  job, no `.githooks/pre-commit` step, no `scripts/` entry (`grep -rn actionlint`
  over the tree outside `target/` returns nothing). It is a manual gate however
  it is run, so a real run still has to back a `workflow_call` change.
  `gh workflow run ci-main.yml --ref <branch>` is that run: it resolves
  `uses: ./.github/workflows/…` against the branch, so it exercises the branch's
  shared body.
* In place over **all eight** workflow files the whole-repo run exits 1, on
  three pre-existing `SC2086:info` shellcheck findings in `release.yml`'s
  packaging steps. The four CI files on their own are clean (exit 0). Don't read
  the whole-repo exit code as a regression signal without looking at what it
  reported.

### The GitHub-expression ternary trap, for anyone editing the gates

`${{ inputs.pull-request && 0 || 1 }}` is **wrong**: GitHub treats the number `0`
as falsy, so the expression always yields `1`. The shared body uses
`${{ inputs.pull-request && '0' || '1' }}` (strings). If a future edit drops the
quotes, the three gated jobs get a shallow clone on the pull-request side and the
`git diff` gate silently fails against a commit the checkout does not have.

### Follow-ups this pull request deliberately did not take

- **`cross-compile` on the pull-request gate.** Still merge-only. A pull request
  that breaks the Windows or darwin build lands green and turns `main` red. Now
  one `uses:` line away once the matrix is its own reusable workflow. Costs
  ~16 runner-minutes and ~4 min wall clock per pull request. Not done here so the
  before/after listing stayed empty on both sides.
- **`release.yml` still hand-copies `build-wasm`** (and the cross-compile
  matrix). It could call `build-wasm.yml` in one line. Not done because
  `release.yml` has **never run** in either repository, so the swap cannot be
  proved the way this extraction was. The reason is recorded in `release.yml`
  itself, next to the copy. §1.4 item 3 calls the matrix share the
  highest-value change in the phase — it belongs with ruling 6.7 item 5, which
  is already the release workflow's pull request.
- **The native-server boot block** (§1.4 item 4) is still duplicated between
  `ci-shared.yml`'s `e2e-visual` and `regen-visual-baselines.yml`. That needs a
  *composite action*, not a reusable workflow, and it touches the
  highest-privilege workflow in the repository (`contents: write`, pushes
  commits), so it wants its own change.
- **The phase 6 spec was not amended.** PR 1 added inline `Correction (PR 1 …)`
  blockquotes; PRs #55, #56 and this one did not touch the document, so §1.2's
  and §1.3's line numbers and the §0 "Reusable `workflow_call` workflows —
  **Open**" row are now stale by construction. Consistent with the siblings;
  flagged here rather than silently.

### Eight jobs are now transitively gated on `build-wasm` — latency AND reporting

On `main` these eight carried **no `needs:` at all**: `audit`, `sdk`,
`test-postgres`, `products-browser`, `product-examples`, `products-postgres`
(since renamed `postgres-migrations`, which now runs the files block's
dialect files too),
`cloudflare-wasm-test` and `browser-wasm-test`. (The last two were missed in the
first version of this note and in the comment in `ci.yml`; neither downloads the
wasm artifact, and both moved from start+~1 s to start+~258 s in the two merge
runs.) The `needs: build-wasm` edge now lives on the whole shared call, so all
eight wait.

* **Latency:** all eight finish in under 3.5 minutes against a ~26-minute
  critical path (`build-wasm` → `e2e-dev-sandbox` → `e2e-dev-compile`), so wall
  clock and runner minutes are unchanged. If a future job in the shared set
  becomes long AND is off the critical path, this is where to look.
* **What gets reported when something breaks — the part that is not latency.**
  A failed `build-wasm` skips the whole call, so **all sixteen jobs in
  `ci-shared.yml` report nothing**: sixteen of `ci.yml`'s seventeen, sixteen of
  `ci-main.yml`'s twenty-three. A dependency bump that both breaks the wasm
  build and introduces a security advisory used to surface two red jobs; now it
  surfaces one, and the advisory is invisible until the build is fixed and CI
  re-runs. Same for a client-library (`sdk`) or migration
  (`postgres-migrations`) regression. Not a false green today — `build-wasm`
  is genuinely red when it fails, and the fork has no required checks — but a
  **skipped required check reports as passing**, so if required checks are ever configured on the
  prefixed names, sixteen of twenty-three would report green on a build
  failure. Make `build-wasm` required alongside them, or accept that.

### The ruling 6.5 evidence method is blind to failure behaviour

**Record this before the next workflow change in this phase.** Ruling 6.5's bar
is a job-and-step diff between a before run and an after run. Both runs are
green, by construction — that is what makes them comparable. So the method
compares *what ran when everything passed*, and it is **structurally blind to a
change in what happens when something fails**. The `build-wasm` gating above is
exactly that shape: it produces an identical green listing on both sides and
still changes which sixteen jobs report a verdict when one job goes red.

For the remaining workflow changes in this phase, the empty job-and-step diff is
necessary and not sufficient. Also ask, by reading the graph rather than the
listings: **which `needs:` edges did this add or move, and for each one, which
jobs now report nothing that used to report something?** A dependency added to a
`workflow_call` job is the worst case, because it silences the whole callee at
once.

### The stale-comment class is not exhausted

`crates/impresspress-browser/src/database.rs`'s third copy of the "path-filtered
to browser-crate changes" claim is fixed here, so all four copies PR #56 found
are closed. But the sweep that found them was a `grep` over comments for
`pull.request|PRs?|path.filter|this workflow|merge side`. **Ten further
references across nine files** named `ci.yml`/`ci-main.yml` as the home of a job
or a pin; those are fixed too. Anything added to the shared body from now on
inherits the obligation: a comment there has to be true on a pull request **and**
on a merge.

Three corrections the review of #57 had to make to that sweep, all the same
class as the thing being swept:

* The count was stated as eight-in-six and then nine-in-eight. It is
  **ten across nine**: the ninth file is `.githooks/pre-commit:4`, which says it
  mirrors the `check` job and pointed at `ci.yml` — a live tracked file, missed
  because the sweep only looked under `.github/`, `crates/`, `scripts/` and
  `.cargo/`. Sweep the whole tree, dotfile directories included.
* A comment written *by* the sweep undercounted its own subject (six jobs where
  it was eight). Re-derive counts from the file, not from the previous draft.
* A new comment in `build-wasm.yml` cited `release.yml:17-51`; the same branch's
  second commit added three lines to that file's comment and shifted the job to
  20-52, and it had been 17-49 on `main`, so the pointer was wrong in both
  directions. **Cite workflow jobs by name, not by line range** — that comment
  now says "`release.yml`'s own `build-wasm` job". Sweeps must re-check pointers
  their *own* earlier commits invalidated.

Everything under `docs/` (specs, plans, the two code-review documents) still
names `ci.yml`/`ci-main.yml` line numbers. Those are dated records of the state
at the time and were deliberately not rewritten; the phase 6 spec being stale by
construction is recorded separately above.

### Never `git checkout --` a file a mutation script also has uncommitted edits in

Found while re-running the linter for the #57 review fixes. The obvious way to
undo an in-place mutation (`sed` a typo into `ci.yml`, lint, put it back) is
`git checkout -- .github/workflows/ci.yml`. That restores the file to **HEAD**,
so it silently discarded the uncommitted review fix sitting in the same file,
and the next lint run reported line numbers from the old content with no error.
Copy the file to a `.bak` before mutating and `cp` it back. This bites any
"mutate → verify → restore" experiment run inside a dirty worktree, which is
exactly when such experiments get written.
