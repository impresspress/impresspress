# Shared brief for impresspress phase work (read fully before starting)

You are implementing one PR of a multi-PR refactor of the `impresspress`
Rust monorepo on the fork `Jsuppers/impresspress`. The hackathon org repo is
frozen; `origin` is the fork and `upstream` has push disabled. Never push
anywhere but `origin`. Never merge; you open a PR and stop.

## Where to work

- Worktree: `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0`.
  Run everything from there. Do NOT `cd` into
  `/home/joris/Programs/suppers-ai/workspace/impresspress` (the main checkout).
- Start by running (each as its own plain command):
  `git fetch origin` then `git switch -c <branch> origin/main`.
  If `git switch` refuses because a branch is checked out, run
  `git status --short` and `git branch --show-current` and report.
- **Shell guard:** this environment refuses compound shell commands that
  contain `git` or shell variables (`$x`, `$(..)`, `for` loops). Plain single
  commands like `git status --short` or `cargo test ...` are fine. Anything
  compound goes into a script file under the session's own scratchpad
  directory and is run with `bash <script>`. Commit with
  `git commit -F <message-file>` from a script.

  (The original run kept helper scripts — `commit-files.sh`, `push-and-pr.sh`,
  `merge-pr.sh`, `wait-ci.sh` — in that scratchpad. They were session-local and
  are gone; they were thin wrappers around `git commit -F`, `gh pr create`,
  `gh pr merge` and a `gh run view` poll, so rewrite them as needed rather than
  hunting for them. The two references below are historical.)

  Historical, for context: a ready commit script
  `bash <scratchpad>/commit-files.sh <message-file> <path>...`
  and a push+PR script:
  `bash <scratchpad>/push-and-pr.sh "<title>" <body-file>`.
- The pre-commit hook may not be active; run fmt/clippy yourself.

## Project rules (from CLAUDE.md, binding)

- Fix the real issue. No shims, no compat layers, no quick fixes. If the right
  fix touches many files, touch them.
- No raw SQL in block code (`exec_raw` / `query_raw`); use `wafer-sql-utils`
  builders. Exceptions: admin SQL explorer, migration runners, test fixtures.
- No sync bridges (`poll_once`, `block_on`).
- No magic mapping layers, no hardcoded lists. Table constants are owned by
  repo modules (`pub const TABLE: &str = "{org}__{block}__{name}"`).
- Config var naming: `WAFER_RUN_SHARED__*` shared, `{ORG}__{BLOCK}__*`
  block-scoped, `IMPRESSPRESS_*` infrastructure.
- No change to wafer-run (pinned rev `7d47e5e`). If something is missing
  upstream, stop and report instead of copying upstream internals.

## Test-first, always

Write the failing test, run it, see it fail for the expected reason, then
implement, then see it pass. Do not write production code before a failing
test. Commit after each coherent task with this trailer at the end of the
message body:

the trailer given under "Attribution" at the end of this brief.

## Verification before you open the PR

```
cargo +nightly fmt --all -- --check
cargo clippy -p impresspress-core --all-targets -- -D warnings
cargo test -p impresspress-core --no-fail-fast
cargo test -p impresspress-core --features block-dev --test endpoint_surface --test openapi_snapshot
```

Known, unrelated failure to ignore: `lockfile_loads_remote_block` (wafer-run
compiled without `wasmi` in `-p` builds). Every other test must pass.
If you touch `crates/impresspress-core/src/prepared_plan.rs`, also run
`env CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner cargo test -p impresspress-cloudflare --target wasm32-unknown-unknown`.
If you touch `crates/impresspress/` (the CLI), run
`cargo test -p impresspress` and `cargo clippy -p impresspress --all-targets`
(the CLI has four pre-existing clippy lints on main; do not add more).

## The two snapshot gates

- `crates/impresspress-core/tests/snapshots/<block>.openapi.json` — the
  schema contract. Must stay byte-identical. Never regenerate it unless the
  PR deliberately changes a published schema, and then list every changed
  line in the PR body.
- `crates/impresspress-core/tests/snapshots/<block>.endpoints.json` — the
  auth contract, one `METHOD path auth [tool=name]` line per declared
  endpoint. Must stay byte-identical, except when the PR deliberately
  declares a path the block already served. Then regenerate with
  `env UPDATE_OPENAPI_SNAPSHOTS=1 cargo test -p impresspress-core --test endpoint_surface`
  (and once more with `--features block-dev` if the dev block is involved)
  and list every added or changed line in the PR body together with the
  handler line that enforces that level today.

## Shipping

Push with `bash <scratchpad>/push-and-pr.sh "<title>" <body-file>`. The PR
body ends with:

```
🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_019vEzsyo9iMd8MxTuteU83z
```

Report back: the PR URL, the list of snapshot lines that changed (if any),
every place you deviated from the spec and why, and anything you found that
is out of scope but should be recorded.

## Attribution (updated 2026-09-06)

Commit messages end with:

```
Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_019vEzsyo9iMd8MxTuteU83z
```

PR bodies end with:

```
🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_019vEzsyo9iMd8MxTuteU83z
```

## Verification correction (2026-09-07, after #25 broke main under block-dev)

The dev feature compiles tests the default run does not, and running only the
two snapshot gates under `--features block-dev` is NOT enough: PR #25 declared
a new table with no export decision and merged green because the failing test
(`dev_data_snapshot`) was never compiled. Every PR now runs the dev-feature
suite in FULL:

```
cargo test -p impresspress-core --features block-dev --no-fail-fast
cargo clippy -p impresspress-core --features block-dev,test-support --all-targets -- -D warnings
```

If your PR declares a table, adds a `collections(..)` entry, or touches
`blocks/dev/`, say in the report which export decision you made and why.
