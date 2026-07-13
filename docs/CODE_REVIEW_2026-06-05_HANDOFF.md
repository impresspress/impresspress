# Impresspress P0 fixes — handoff (2026-06-05 → resume 2026-06-06)

Continuation of the 2026-06-05 multi-agent code review of impresspress. The user
asked to "fix the P0 batch" (separate PR per fix, per the workspace branch+PR
rule), then paused for the day. **Resume here tomorrow.**

## Source artifacts (in `impresspress/docs/`, untracked)
- `CODE_REVIEW_2026-06-05.md` — full prioritized report (exec summary, top
  architecture smells, convention violations, per-area notes, 24-row action list).
- `CODE_REVIEW_2026-06-05_findings.json` — all 213 verified findings (structured).

Review stats: 132 agents, ~7M tokens, 224 raw → 213 confirmed (11 refuted),
5 high / 32 medium / 176 low.

## ⚠️ CONCURRENCY HAZARD — read before resuming
At handoff time **two Claude Code sessions were running in this workspace**
(PIDs 36553 @10:14, 129998 @21:23, both cwd `/home/joris/Programs/impresspress/workspace`).
The *other* session was mid-refactor: changing `wafer-sql-utils` builders to
return `Result` (e.g. `build_daily_count` now returns
`Result<Statement, SqlBuildError>`) — **11 modified, uncommitted files in
`wafer-run/`** plus adapted consumers in impresspress (`dashboard.rs`, `Cargo.lock`).

Consequences observed:
- The impresspress build is **transiently broken on `main`**: `dashboard.rs:88`
  still treats `build_daily_count` as returning `Statement`, but the (uncommitted)
  wafer-run builder now returns `Result`. This is the other session's
  in-flight work, **not** related to any P0 fix.
- Two concurrent `git checkout`s raced; the other session's working-tree edit to
  `dashboard.rs` was reverted in the process. It's a trivial mechanical consumer
  update and that session owns it — left untouched.

**Before resuming any build/verify:** confirm you are the ONLY agent editing
`wafer-run` + `impresspress`, and that `wafer-run` is at a clean/committed state
(`cd ../wafer-run && git status`). impresspress builds against `../wafer-run` via a
path dependency, so an in-flight wafer-run breaks impresspress regardless of branch.

## Git state at handoff
- Working tree: branch **`main`** (restored to where the other session started).
- **PR1 committed on branch `fix/messages-remove-a2a` (`1ddaa75`)** — local only,
  NOT pushed, NOT build-verified (see hazard above). The RED regression test
  *did* compile+fail correctly before the concurrent refactor landed; GREEN was
  not verifiable due to the transient `dashboard.rs` break.
- `Cargo.lock` shows modified on `main` (other session).

## The 5 P0s → 4 PRs

### PR1 — Remove `/a2a` endpoint  ✅ CODE COMPLETE (branch `fix/messages-remove-a2a`)
User decision: **remove the endpoint entirely** (re-add later behind auth), rather
than retrofit authentication. The `/a2a` JSON-RPC endpoint dispatched fully
unauthenticated — pipeline routed `POST /a2a` into the messages block before any
auth gate, and no method handler (SendMessage/GetTask/ListTasks/CancelTask)
checked the caller.

Done in commit `1ddaa75`:
- Deleted `crates/impresspress-core/src/blocks/messages/a2a.rs`.
- `messages/mod.rs`: removed `pub(crate) mod a2a;`, the `BlockEndpoint::post("/a2a")`
  registration, the `handle()` dispatch, and the "A2A task lifecycle" phrase from
  the block description. Added regression test
  `messages_block_does_not_expose_a2a_endpoint` (asserts no `/a2a` in `info().endpoints`).
- `pipeline.rs`: removed the `/a2a` → `call_block("impresspress/messages", …)` bypass.
- `service.rs` is shared with the REST API → left unchanged (verified all 6 service
  fns the a2a handlers used are still called by rest.rs/pages: get_context×2,
  create_context, add_entry, update_context, list_contexts×3, list_entries×2).

**Next:** once wafer-run is clean, on branch `fix/messages-remove-a2a` run
`cargo test -p impresspress-core --lib messages_block_does_not_expose_a2a_endpoint`
(expect GREEN) + `cargo build -p impresspress-core`, then push + open PR.

### PR2 — Stored XSS in admin network page  ⬜ NOT STARTED
File `crates/impresspress-core/src/blocks/admin/pages/network.rs`. The inbound table
(~line 172) builds `onclick={"toggleDetail('" (row_id) "','" (detail_url) "')"}`
where `row_id`/`detail_url` embed request-log `method`/`path` (attacker-controlled:
any HTTP request with a crafted path is logged). maud escapes HTML-attribute
context but NOT JS-string-literal context → breakout XSS executing in an admin's
session.

Root-cause fix (house style): drop the JS-string interpolation. Render
`data-row-id=(row_id) data-detail-url=(detail_url)` attributes (maud escapes
attribute values correctly) and wire a delegated `addEventListener('click', …)`
that reads `el.dataset`. Mirror the existing pattern in
`crates/impresspress-core/src/ui/assets.rs:284-300` (palette `data-action`/`data-href`
delegation). The `toggleDetail(rowId, url)` JS is defined in network.rs:133-141.
**Check for an OUTBOUND table with the same `toggleDetail` pattern** and fix both.

TDD: render-fn test asserting a malicious path (e.g. `'); alert(1); //`) is
neutralized in the output (no raw breakout sequence in the rendered HTML). Test
pattern: call render fn → `.into_string()` → assert, per
`crates/impresspress-core/src/blocks/llm/ui.rs:636-696`. Run with
`cargo test -p impresspress-core --lib blocks::admin::pages::network`.

Related (NOT P0, note for follow-up): 6 other `onclick`-with-interpolation sites —
`userportal/mod.rs:584`, `userportal/pages/security.rs:78-88`,
`products/pages.rs:689`, `admin/pages/email.rs:85`, `auth_ui/pages/login.rs:90`,
`auth_ui/pages/settings.rs:272`. Most interpolate field keys / provider names that
may be safe; audit but don't bundle into the P0 PR.

### PR3 — Empty JWT secret fail-open (Cloudflare)  ⬜ NOT STARTED
`crates/impresspress-cloudflare/src/lib.rs:236-240` does
`cfg_svc_map.get(JWT_SECRET_KEY).cloned().unwrap_or_default()` then
`make_jwt_crypto_service(jwt_secret)` with NO empty check. An empty HMAC key makes
JWTs forgeable. **The report said "fix in `build()`" — that is WRONG:**
`builder.rs::build()` is shared with the browser/web path
(`impresspress-web/src/lib.rs`) which *intentionally* builds with an empty secret and
rotates it in afterward (Phase 3, before `init_all_blocks`). So:
- DO mirror the native guard (`impresspress/src/cli/server.rs:71-76` — rejects both
  missing and empty `JWT_SECRET_KEY`) in the **Cloudflare adapter only**, before
  `make_jwt_crypto_service`.
- DO NOT add an empty-secret guard to `builder.rs::build()` (would break browser).
- Note `seed_auto_generated` (cloudflare `runner`) should populate the secret at
  boot; the guard catches the case where seeding failed/was skipped.
- Determine the cloudflare boot fn's error type to fail loudly (Result vs panic) —
  check the call site around lib.rs:230-310.

TDD is harder here (WASM/worker boot). At minimum a unit test on a small extracted
helper `require_nonempty_jwt_secret(map) -> Result<String, _>`, or assert the guard
logic in a native-compilable unit. Run `cargo build -p impresspress-cloudflare`
(target wasm32 as CI does) + `cargo test -p impresspress-cloudflare`.

### PR4 — LLM `key_var` resolution + streaming persistence  🟡 BUG A DONE / BUG B DEFERRED
File `crates/impresspress-core/src/blocks/llm/`.

**STATUS 2026-06-13:** Bug A (key_var resolution) was implemented and merged as
part of the quality-fix program package **S1-H (llm-small-dedupe)** — impresspress
PR `fix/llm-small-dedupe` (#258). Resolution now happens in
`routes::reload_provider_service` via `config::get` after `row_to_config`, the
no-op `resolve_key`/`ProviderSnapshot` are gone, the three false doc claims are
fixed, and `legacy_providers` calls the shared reload so migrated rows resolve
too. Regression test:
`routes::tests::reload_provider_service_resolves_key_var_into_api_key`. Bug B
(streaming assistant persistence) was explicitly OUT of S1-H scope and remains
deferred — the `let _ = thread_id` suppression + `TODO(llm-phase-b-task-14)` in
`handle_chat_stream` stays, tagged.

**Bug A — `key_var` never resolved into `api_key`** (every secret-referencing
provider 401s):  ✅ DONE (S1-H / #258)
- `row_to_config` (`llm/schema.rs:72-131`) loads `key_var` (the config-var NAME)
  but sets `api_key: None`, and has NO `ctx` so it cannot resolve secrets.
- `resolve_key` (`providers/mod.rs:218-232`) just reads `cfg.api_key` → None →
  `EncodeError::MissingApiKey` → `LlmError::Unauthorized`.
- Fix: in the two reload paths that DO have `ctx`, after `row_to_config`, resolve
  `cfg.key_var` via `config::get(ctx, &key_var)` (returns `Result<String,_>`, see
  `products/stripe.rs:13-16` for the idiom) and set `cfg.api_key` before
  `provider_svc.configure(configs)`:
  - `reload_provider_service` (`routes.rs:412-436`)
  - `legacy_providers::reload_service` (`migrations/legacy_providers.rs:232-251`)
- Schema: `impresspress__llm__providers` has `key_var TEXT` only (no api_key column);
  secrets live in `impresspress__admin__variables`. So resolution at reload is correct.
- TDD: extract a pure `resolve_api_key(cfg, lookup_fn) -> ProviderConfig` (or test
  via a Context stub with config) asserting `api_key` is populated from `key_var`.

**Bug B — streaming chat never persists assistant turns** (corrupts history):  ⬜ DEFERRED (out of S1-H scope)
- `handle_chat_stream` (`routes.rs:277-353`) explicitly drops `ctx`/`msg`/`thread_id`
  (`let _ = ctx;` etc.) with a `TODO(llm-phase-b-task-14)` because `&dyn Context`
  isn't `'static` for the `OutputStream::from_producer` closure.
- The buffered path `handle_chat` (`routes.rs:206-272`) DOES persist: accumulates
  text deltas into `content`, then `messages_create(ctx, msg, &thread_id,
  "assistant", &content)` (mod.rs:62-112) at ~line 255.
- Fix: capture `ctx.clone_arc()` (Context has `clone_arc() -> Arc<dyn Context>`),
  plus an auth snapshot (user_id/email/roles — `messages_create` forwards them from
  the original Message; either clone the Message or snapshot the meta into a minimal
  Message) and `thread_id` into the producer. Accumulate `Text` chunk deltas while
  streaming; after the `[DONE]` frame, call `messages_create(&*ctx_arc, &msg_snap,
  &thread_id, "assistant", &content)`. Remove the TODO and the `let _ = …;` drops.
- TDD: harder (spawned producer). At least assert the accumulation + that a
  persistence call fires on stream completion via a stub messages block / PanicCtx
  variant (see existing `routes.rs:845-899` PanicCtx tests).

Run `cargo test -p impresspress-core --lib llm`.

## Execution notes / conventions (from CLAUDE.md)
- One branch + PR per fix. Never `git checkout -b NEW origin/main` (push-to-main
  footgun) — branch from local `main`.
- Commit message footer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Fix at root cause; no compat shims; no raw SQL outside allowed exceptions; no
  sync bridges; no hardcoded domain values.
- `cargo +nightly fmt` is what CI uses (stable pre-commit misses nightly rules).
- Default features include all the blocks, so `cargo test -p impresspress-core --lib`
  exercises them without extra `--features`.
