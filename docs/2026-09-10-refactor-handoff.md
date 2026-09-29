# Refactor handoff — 2026-09-10

> **Updated 2026-09-14**, against fork `main` at `f15a0b46`. PRs #63 through #80
> merged in the four days after this was written, and most of them landed
> squarely on what it describes. The 2026-09-10 observations below are left
> exactly as they were recorded; every section that has since expired carries an
> **Update (2026-09-14)** block saying what happened and where the answer now
> lives. Nothing has been quietly rewritten to look right in hindsight — where a
> claim is dead it says so, and where a recommendation was *not* taken it says
> that too. **What merged** (a record of what had merged by that date) and
> **What is not proven** needed no change. **Resuming the work** was updated only
> in its opening paragraph; its two subsections, **The process that worked** and
> **Verification before any merge**, still hold as written.
>
> Every claim added on 2026-09-14 was checked against the tree before it was
> written down. That is not a guarantee it is right: the first review of this
> update found a citation pointing at a plausible-looking neighbour of the code
> it named. Open the file at the line before relying on one.
>
> The rendered artifact linked below is the original and has not been
> re-rendered.

Fork `main` at `7f731605`. 61 pull requests merged, none open.

Seven phases of the 2026-09-05 architecture review are complete and verified
against a running server. What remains is one architectural decision, already
taken, and the reproduction that has to fail before anyone acts on it.

A rendered version of this document:
<https://claude.ai/code/artifact/0d9a2fa5-e4dc-421e-98cb-1e0986fe1f45>

---

## Where it stands

Phases 0 through 6 are merged. Every pull request was implemented test-first by
one agent, reviewed at high effort by a second, corrected, and merged only after
the reviewer signed off and the full dev-feature suite ran locally by hand.

After the last phase merged, a live server run exercised 332 of 351 declared
endpoints plus WebMCP. **It found breakage that 60 merged pull requests and a
fully green pipeline had all missed.** A user could upload a file but could
neither download nor share it. Both are fixed and confirmed on a live server by
comparing bytes, not status codes.

That run is why this handoff exists. The recurring failure across the whole
programme was never a missing fix. It was coverage that never exercised the real
path — a fixture certifying calls production refuses, a type test that could not
fail, a contract test passing because the published shape was wrong in the same
direction as its consumer.

> **Update (2026-09-14).** The declared endpoint surface is now **360** rows
> across *fourteen* `crates/impresspress-core/tests/snapshots/*.endpoints.json`
> files. The same count against `7f731605` returns exactly the `351` recorded
> here, so the two figures are comparable: the nine new rows are the signal
> block's five (#75, a fourteenth snapshot file that did not exist on
> 2026-09-10) and four added to admin. `email.endpoints.json` is still `[]`, and
> the dev block is still 19 of the total — so the `351`s in this document,
> including the "19 of 351" under **What is not proven**, are left as recorded.
> Count the rows, not the lines: the fourteen files are 387 lines and the
> difference is JSON brackets.

## What merged

| PRs | Work |
| --- | --- |
| #1–#53 | Phases 0–5: correctness bugs, route tables, platform state, error discipline, runtime lifecycle plus upstream `wafer-run` work, UI migration |
| #54 | Phase 6 spec landed, repository periphery swept |
| #55 | Unpinned third-party clone dropped, CI path filters corrected |
| #56 | A merge to `main` now runs everything a pull request runs |
| #57 | Reusable workflows — one body for both CI gates |
| #58 | Release-workflow residues; stale `packages/impresspress-web` deleted. First dry run in that workflow's history; it had never executed once |
| #59 | One SDK request client; 65 dishonest type exports removed. File transfers keep an unlimited timeout by explicit opt-out |
| #60 | SDK type-freshness gate. Endpoint coverage 56/73 → 73/73 call sites |
| #61 | Storage download and sharing restored, plus a third outage the dependency sweep found |

Upstream `wafer-run` also took #328, #330, #331, #332 during phase 4.

---

## The decision that is waiting (resolved 2026-09-14)

> **Update (2026-09-14).** Taken, and shipped — #63 through #65, #66, #68
> through #74 and #76. The position below was adopted as written: the
> `impresspress__admin__variables` table is the config store on every target.
>
> The decision record is no longer in `docs/` at all, and that is deliberate. It
> lives in doc-comments on the code it governs, which is where it cannot drift
> away from the thing it describes:
>
> - `crates/impresspress-core/src/blocks/config.rs:1-50` — why an impresspress
>   config block has to exist (a `ConfigService` is a *synchronous* trait and so
>   can never consult a database; a `Block`'s `handle` is async and can), and the
>   four-step read order it implements, runtime-owned keys first.
> - `crates/impresspress-core/src/config_generation.rs:1-26` — the
>   isolate-local write counter the cache invalidation rides on, why a build that
>   seeds nothing never bumps it, and why it is a `Cell` and never a `RefCell`.
> - `crates/impresspress-core/src/platform_state/variables.rs:967` — the
>   env-precedence contract itself, "Exactly what the transition promises",
>   inside `seed_and_load`'s doc-comment: an env value seeds a key, an admin edit
>   then wins permanently, and a one-time upgrade transition pins rows that
>   predate edit tracking. The ownership rules it rests on are at `:390-600`.
> - `crates/impresspress-cloudflare/src/config_service.rs:25-38` — why
>   `HashMapConfigService::set` stays a silent no-op now that nothing in the tree
>   calls it.

### Admin settings never reach a running server

Worse than it first appears. There are **three read surfaces and two write
surfaces**, and Cloudflare Workers is in a worse position than native on both
sides.

- **Read 1** — async `config::get_default`, ~101 call sites. Native reads
  `EnvConfigService`, seeded once from the variables table (`cli/server.rs`).
  Cloudflare reads `HashMapConfigService`, which by documented design holds only
  the JWT secret, CORS/CSP/strict-schema worker vars, block-settings JSON and
  consumer `request_config` (`impresspress-cloudflare/src/runtime_build.rs`).
  **No D1 variable row ever enters it.** So on Workers `SiteConfig::load`, the
  primary colour, app name, logo and the `IMPRESSPRESS__PRODUCTS__STRIPE_*`
  reads all return defaults whatever an admin saved.
- **Read 2** — sync `ctx.config_get`, 11 production sites, all branding.
  Boot-frozen on native; never holds D1 rows on Cloudflare.
- **Read 3** — `ConfigSource::load_for_block` at `Init`, block-declared keys
  only, so shared `WAFER_RUN_SHARED__*` rows never arrive this way.
- **Write 1** — `admin/ops.rs::update_variable` (the JSON API and the Variables
  page): writes the table only.
- **Write 2** — `ui/settings_form.rs::save_settings`, behind **five** admin
  forms (products, legalpages, userportal, email, auth-ui). Native: writes the
  in-memory overrides only, so it is live but **lost on restart**. Cloudflare:
  `HashMapConfigService::set` is a **documented no-op** and the handler still
  returns `200 Settings saved`.

`docs/2026-07-30-impresspress-optimization-review.md` §CFG-01 reached the same
conclusion six weeks earlier and asked for a reproduction. Nothing followed it.

### The position

**The `impresspress__admin__variables` table is authoritative on every target;
the in-memory maps become caches of it.** Only the table is durable, shared
across Worker isolates, already written by every writer, and already read by
every target's `ConfigSource`. An in-memory store cannot be authoritative on
Workers, and having a different design per target is what produced this.

### The order of work

1. **Reproduce, and stop if it passes.** The Workers case on the wasm harness
   (seed a non-default `WAFER_RUN_SHARED__APP_NAME`, render, assert), plus
   native round trips for `PATCH → get_default` and
   `save_settings → seed_and_load` across a restart. All three should be red
   today. This is the test 61 pull requests never had.
2. **Own `wafer-run/config` in impresspress** instead of registering
   `wafer_core`'s (`builder/registration.rs`). Reads take a variables snapshot;
   writes go through `platform_state::variables::upsert_by_key` with the
   sensitive/SSRF guards `update_variable` already carries. Invalidation reuses
   the generation counter in `config_generation.rs` and shares
   `D1ConfigSource::cached_snapshot`. The handler is async, so no sync bridge.
3. **Delete `EnvConfigService` and `HashMapConfigService`** from the targets,
   and `RuntimeConfig::extend_both(vars)` on native. Native converges on the
   Workers model rather than diverging from it.
4. **Move the 11 sync branding reads to the async client.** `ui/mod.rs` already
   reads the primary colour async while `auth_ui/pages/mod.rs` reads it sync;
   the sync ones are the outliers.

Leave `StaticConfigSource` / `Init`-time keys restart-bound. That is the
documented contract for Init config on every target.

> **Update (2026-09-14).** Steps 1, 2 and 4 shipped as written. Step 3 did not,
> and deliberately.
>
> 1. **Shipped** (#63, landed red by design). The reproductions survive as
>    regression tests and still carry the defect they were written against in
>    their doc-comments, so a revert reads as a failure with a reason attached —
>    `ui/settings_form.rs:1246`, `mod config_store_reproduction`, is the
>    settings-form one. One caveat, and it matters: the Workers case was NOT
>    reproduced on a wasm harness. It is covered host-side through
>    `blocks::config`, which is the code both targets now share
>    (`blocks/config.rs:855` seeds `WAFER_RUN_SHARED__APP_NAME`), while
>    `impresspress-cloudflare` still carries no test of its own for this. So the
>    first bullet of **What is not proven** below still stands as written.
> 2. **Shipped** (#64). `builder/registration.rs:148` calls
>    `blocks::config::register_with` instead of registering `wafer_core`'s
>    `ConfigBlock`, and the block awaits the table with no sync bridge.
> 3. **Not done, and no longer wanted.** `EnvConfigService` and
>    `HashMapConfigService` both survive, because the in-memory map turned out to
>    carry things the table cannot: worker and env bindings, the builder-time
>    CORS, CSP and `STRICT_SCHEMA` values, and the synthetic block-settings JSON.
>    What changed is that it stopped being a *copy* of the table and became a
>    *boot map* consulted after it. `crates/impresspress/src/cli/server.rs:234-293`
>    now seeds exactly the keys something must read synchronously — the JWT
>    secret, CORS origins, CSP directives — plus the block-settings JSON and the
>    `HAS_PROCESS_ENV` marker (`:282`), with `RUN_MIGRATIONS` (`:286`) and
>    `STRICT_SCHEMA` (`:290`) when they are set. Its own comment
>    is the reasoning: "Copying the whole table here is what step 1 made
>    redundant — and worse than redundant: every admin-editable key sitting on a
>    boot-frozen surface is a stale read waiting for its first caller."
>    `RuntimeConfig::extend_both` was left in the tree and is now dead API:
>    `builder/config.rs:77` defines it and the only call is at
>    `builder/config.rs:269`, inside its own unit test.
> 4. **Shipped.** No production `ctx.config_get` reads branding any more. What
>    the sync surface still carries is the JWT secret (`csrf.rs:181`,
>    `auth/service.rs:185`), the block-settings JSON (`admin/settings.rs:368`,
>    `routing.rs:581`, `migration_helper.rs:299`, `tickets/config.rs:137`), the
>    signal block's own Init keys (`signal/rest.rs:38`, `:44`, `:68`), the
>    migration gate (`migration_helper.rs:180`), and one probe that is not a
>    config read at all — `admin/pages/variables.rs:1200` asks whether an
>    arbitrary key is present in the boot map to choose which toast the Variables
>    page shows after a delete. Each is a key the boot map, not the table, is
>    supposed to own.

### Deliberately not doing

- **A native-only refresh on write.** Adds a fourth hand-kept copy and leaves
  Workers blind.
- **Changing `ConfigService::set` upstream.** Once we own the block, the raw
  service setter is no longer the interface.
- **The duplicate-key 409.** `DatabaseError` has only `NotFound`/`Internal`; a
  correct 409 needs a producer-side `Conflict` variant and a pre-check races.
  Admin-only endpoint.
- **An SDK version bump.** Nothing is published; the clock starts at first
  publish.

> **Update (2026-09-14).** Three of the four still hold. **The duplicate-key 409
> shipped** (#73), and the reasoning above was wrong about what it would cost.
> No producer-side `Conflict` variant was needed: `ErrorCode::AlreadyExists`
> already renders 409 in `wafer_block::http_codec::error_code_to_http_status`.
> And the pre-check race is avoided by probing *after* the write fails rather
> than before it — the insert has already been refused, so there is no gap in
> which a competing create can claim the key. The probe has three answers, not
> two: taken is the conflict, free is a genuine fault, and a probe that could not
> run keeps the write's own failure, so a transient read outage cannot turn a 500
> into a wrong 409 either way. See `admin/ops.rs:245-294`,
> `taken_key_or_db_error`. **Changing `ConfigService::set` upstream** is still
> recorded rather than done, with the reason now written on the no-op itself
> (`impresspress-cloudflare/src/config_service.rs:25-38`). **A native-only
> refresh on write** was never needed. **An SDK version bump** is still not
> wanted — see below.

---

## Open for the maintainer

Neither is an engineering call.

> **Update (2026-09-14).** The first was decided — the opposite way to the
> recommendation below. The second was never written down and still has not
> been, but nothing about it has changed.

**Recent files returns the wrong kind of row.** `GET /b/storage/api/recent`
returns object-*view audit* rows while both SDK consumers read it as object
metadata, and it has behaved that way since `views.rs` was written. #60 made the
published schema match today's behaviour, so either direction is now caught by
the gate. Recommendation: make it return object rows — that is what both
consumers expect, and nothing is published, so the change is free right now.

> **Decided, against this recommendation.** The endpoint still returns view-audit
> rows, and that is now the published contract rather than an accident — the
> SDK was changed to match the server, not the server to match the SDK. It is
> recorded in two places.
> `packages/impresspress-js/src/services/storage.service.ts:101` defines
> `FileViewRecord`, whose doc-comment says outright that this — **not**
> `FileMetadataRecord` — is what `/b/storage/api/recent` returns, one row per
> tracked download, carrying the viewer and the view instant rather than the
> object's size, type or upload state. `getRecentFiles()` at `:289` documents the
> result as audit rows and sends a caller who wants object metadata to `search`
> or `listObjects` instead. The "nothing is published, so the change is free"
> argument was sound and simply was not taken; anyone reopening it is reversing a
> decision, not filling a gap.

**SDK versioning after 65 removed exports.** Nothing exists on the public npm
registry under `@impresspress/sdk`, `@impresspress/types`, `impresspress-js`,
`impresspress-web` or `solobase-web` — all 404. Version sits at `1.0.0`.
Recommendation: note it in `RELEASE.md` and move on.

> **Still outstanding, and still trivial.** Nothing was added to `RELEASE.md` —
> its only SDK mention is an asset-rename note. `@impresspress/sdk` is still
> `1.0.0`, and `packages/impresspress-js` is now the only package left in the
> tree, so three of the five names above no longer exist locally either. The
> registry itself was not re-checked on 2026-09-14; if it is still empty, the
> recommendation stands unchanged and only the paperwork is owed.

---

## Known and unfixed

Found by the live run, recorded, deliberately deferred.

> **Update (2026-09-14).** Two of the four are closed outright, one is five-sixths
> closed, and one survives with a sentence of it now false. Only the llm threads
> page and the declared-dependencies gap are still open; each item says which.

**Registered is not the same as enabled.** Three symptoms, one cause, one pull
request. `handle_extensions` (`admin/mod.rs`) hardcodes `enabled: true` per
block and never reads `block_settings`. `retain_registered`
(`ui/nav_groups.rs`) gates the sidebar on registration, while
`register_feature_blocks` registers everything compiled in regardless of
enablement — so a disabled Tickets is registered, shown, and then 404s. The
toggle (`admin/pages/blocks.rs`) validates nothing, so a typo writes a permanent
phantom row.

> **Fixed** (#68, #70), all three symptoms. `handle_extensions`
> (`admin/mod.rs:733`) reads the router's own `Arc<RwLock<BlockSettings>>` — the
> same handle the router gates on, so no snapshot and no read stand between the
> two answers — and reports
> `features.is_block_enabled(routing::feature_gate_name(&b.name))`.
> `retain_registered` is gone: `ui/nav_groups.rs:54` is now `retain_reachable`,
> which drops a sidebar item when its block is unregistered **or**
> registered-but-disabled, asking enablement through the same
> `feature_gate_name` the router uses, so the `default_enabled(false)` Tickets
> entry no longer 404s on a default install. The toggle validates
> (`admin/pages/blocks.rs:300-310`): a registered block is toggleable only if it
> declares `can_disable`, an unregistered one only if a row already exists, and
> anything else is `err_not_found("Unknown block")` — so a typo mints no row
> (`toggle_rejects_an_unknown_block_and_mints_no_row` at `:783`,
> `toggle_refuses_a_block_that_cannot_be_disabled` at `:818`). #70 also made the
> toggle take effect without a restart, by writing the live snapshot after the
> table and only on a confirmed write.

**Four handlers answer 500 for a missing id** — llm model status, llm model
unload, llm threads page, userportal button delete. They bypass the classifier
that already exists for this (`crud.rs`, `DbFailure::Refused` → 404). Two
**public** payment endpoints (`products/stripe.rs`) also answer 500 on a default
install because Stripe is unconfigured, where 503 is right; being public, they
skew error metrics.

> **Five of six fixed. One survives — and it is still open.**
>
> Fixed: llm model status and unload route every service error through
> `llm_service_error` (`llm/routes/models.rs:77`), which sends
> `InvalidArgument`/`FailedPrecondition` to 400 and everything else to
> `crud::db_error`, so a caller naming a backend that does not exist gets a 404
> under *our* label rather than the runtime's (#67). userportal button delete
> (`handle_delete_button`, `userportal/pages/admin_buttons.rs:334`) sends the
> failed `db::delete` through the same classifier —
> `crud::db_error(e, "Button not found", "Failed to delete button")` at `:339` —
> because `DbExec::delete` answers `NotFound` when no row matched, so a stale id
> in the caller's own page is their 404 rather than this site reporting a fault
> and inviting a retry. Both public Stripe endpoints answer
> a real 503 through a dedicated `err_unavailable` constructor
> (`impresspress-core/src/http.rs:43-63`, called at `products/stripe.rs:543` and
> `:546`), whose doc-comment names the 2026-09-10 live run as the reason it
> exists — and is careful to record that it does not reduce Stripe's
> redeliveries, since Stripe retries on any non-2xx; the gain is an accurate
> status, not fewer retries (#72).
>
> **Still open: the llm threads page.** `blocks/llm/pages.rs:139` still returns
> `ui::server_error_response(msg)` when the selected thread's entry read fails,
> and an unknown thread id is exactly that failure: `messages_list` calls
> `GET /b/messages/api/contexts/{id}/entries`, whose handler resolves the context
> through `owned_record` (`messages/rest.rs:37`, called at `:137`) and answers
> 404 for one that does not exist. So a mistyped or deleted thread id is still a
> 500 on a page. The sibling read at `:128` — the thread *list* — is a genuine
> 500 and is correct as it stands; only the bound-id read needs the classifier.

**Environment variables are silently overridden.** Not a behaviour bug:
`WAFER_RUN_SHARED__*` values seed on first boot and existing rows then win,
which is intended. The defect is the silence. One `tracing::warn!` when an env
value differs from the stored row.

> **Fixed** (#74), and the fix went further than the one `warn!` asked for here.
> The precedence was settled rather than merely narrated: an env value seeds a
> key, and once an admin has edited the row the export is inert for that key
> permanently. A key whose stored value disagrees with a live export but predates
> edit tracking cannot be told apart from an admin edit, so it is pinned once at
> the upgrade under its own sentinel — not the user-edited one, because the log
> line and the Variables page both have to be able to say "nobody can tell which"
> — and logged with `"this environment variable has NO EFFECT from now on"`
> (`platform_state/variables.rs:1520`, `pin_at_upgrade`). The recovery advice is
> carried once per boot in a summary line rather than repeated per key, so the
> key names stay readable. The contract is written out on `seed_and_load` under
> "Exactly what the transition promises".

**Nothing enforces declared dependencies.** A block's `requires` list versus the
`wafer_core::clients::*` its module tree calls is checked by nobody. That gap
caused two of the three outages and was found by a hand sweep. #61 added an
enumeration guard over the storage shim's op dispatch; `access_type_for_op` is
still unguarded, so a new upstream *read* op would silently demand a write
grant. Fail-closed, but unguarded.

> **Still open — but the last two sentences above are now false.**
> `access_type_for_op` is guarded: `blocks/storage.rs:132` names the four read
> ops explicitly and classifies everything else, "including any op added upstream
> after this was written", as a write (`:146`), which is the fail-closed answer —
> it demands the stricter grant rather than admitting a mutation under a read
> grant. Its tests are `:590` and `:778`. (The shim's *other* fallthrough — the
> op dispatch in `rewrite_request_body` that #61 enumerated — is separately
> covered at `:740`, with a negative control at `:789`. Two different matches;
> do not read the second pair as evidence about the first.)
>
> The gap itself survives, and it is the one named in the first sentence:
> **nothing compares a block's declared `requires` against the
> `wafer_core::clients::*` its module tree actually calls.** Two partial guards
> now exist and neither closes it. Test fixtures enforce `requires` at *runtime*
> — `blocks/files/mod.rs:63` feeds the files block's own declaration into
> `TestContext::with_files`, so every files-block test is on the gate by
> construction and can no longer certify a `call_block` that production refuses;
> `legalpages/mod.rs:1285` does the same by hand. And `blocks/dev/validation.rs:564`
> checks `callable_blocks` against `requires` — but only for sandbox *guest*
> blocks, not for the blocks compiled into this repo. A first-party block that
> calls a client it never declared still ships, and is still caught only by a
> test that happens to exercise that call through a fixture that happens to
> apply the gate.

---

## What is not proven

Stated plainly so nobody inherits false confidence.

- **Every Cloudflare claim above.** The 332-endpoint run was native. The Workers
  analysis is static tracing corroborated by the July review, not observation.
  This is exactly why step 1 is a reproduction and not a fix.
- **Business logic behind the endpoint surface.** The probe drove parameterised
  paths with non-existent ids and `{}` bodies. 132 of the admin-authenticated
  404s and 49 of the 400s mean the handler was reached and its real work never
  ran. Reachability and auth are proven; behaviour is not.
- **That streaming actually streams.** The new download tests assert bytes, not
  memory behaviour, and the in-memory backend uses the buffering default. They
  would pass equally against a fully buffered path.
- **The Cloudflare/R2 storage path.** `impresspress-cloudflare` is excluded from
  the host suite; its `get_streaming` is a real implementation, and no test in
  the #61 run exercised R2, D1, or the wasm32 build of that path.
- **19 of 351 endpoints were never exercised** — the whole `dev` block, because
  `block-dev` is not in the CLI's default features, so those routes 404 in a
  release binary.

Also worth knowing: WebMCP's two routes (`/b/webmcp/manifest.json`,
`/b/webmcp/webmcp.js`) are pipeline-level and appear in **no**
`*.endpoints.json`, so the endpoint-surface contract does not cover them.

---

## Resuming the work

The working checkout is a git worktree, detached at the last merge:

```
/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0
```

> **Update (2026-09-14).** That worktree is no longer detached: it holds the
> branch `docs/refactor-handoff`, which is 86 commits behind `origin/main`
> because it was branched from `7f731605` and everything in this update landed
> after it. Check what is checked out before assuming the path tracks `main`.
> The two subsections below are unchanged and still describe how the work was
> done.

`origin` is the fork `Jsuppers/impresspress`. The organisation repo
`impresspress/impresspress` is frozen as a hackathon submission and its push URL
is deliberately `DISABLED`. Do not re-enable it.

### The process that worked

One agent implements a single pull request, test-first. A second reviews at high
effort, read-only. Findings are fixed. The full dev-feature suite runs locally,
by hand, before merge. Then merge, and branch the next from the result.

The briefs are in [`docs/process/`](process/):

- [`implementation-brief.md`](process/implementation-brief.md) — the standing
  brief every implementer reads first.
- [`review-brief.md`](process/review-brief.md) — the reviewer's shape.
- [`carry-forward.md`](process/carry-forward.md) — accumulated notes from
  earlier phases that later work was meant to pick up.

The review gate sent back every pull request in phases 4, 5 and 6 at least once,
and the finds were not cosmetic: a request path converted from write-free to
writing; a codec adoption that emptied a payments audit trail; a
request-forgery gate that did not survive a redirect; a gate that certified a
response shape the server never sends.

### Verification before any merge

```sh
cargo +nightly fmt --all -- --check
cargo clippy -p impresspress-core --features block-dev,test-support \
      --all-targets -- -D warnings
cargo test -p impresspress-core --features block-dev --no-fail-fast
```

One failure is expected and unrelated: `lockfile_loads_remote_block` fails
locally because the patched `wafer-run` checkout is built without the `wasmi`
feature. Everything else must pass.

**This step is not optional.** A pull request once merged green and broke `main`
because the dev-feature suite was skipped — the dev feature compiles tests the
default run does not.
