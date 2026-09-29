# Reviewer brief (opus, high effort) — fill in PR number and scope

Work at HIGH reasoning effort. Review pull request #<N> on `Jsuppers/impresspress`. Read-only: no fixes, no merge, no push.

## Step 1: run the code-review skill

Invoke the `Skill` tool with skill `code-review:code-review` and args `<N>` (fallback name `code-review`). Let it finish; it posts a PR comment only for findings scored >= 80. Capture everything it found, including items it dropped below its threshold.

## Step 2: independent invariant review

`gh pr diff <N> --repo Jsuppers/impresspress`, `gh pr view <N> --repo Jsuppers/impresspress`. The local checkout `/home/joris/Programs/suppers-ai/impresspress-worktrees/phase0` is on the PR's head branch. Read `docs/superpowers/specs/2026-09-05-route-table-single-source-design.md`, the PR's plan under `docs/superpowers/plans/`, and `CLAUDE.md`.

Shell guard: plain single commands only, or a script file under the scratchpad dir run with `bash <script>`. Do not run cargo builds/tests.

Check:
1. Auth preservation: every migrated row's method, path, auth, summary and schema slots against the deleted `info()` list; nothing looser.
2. Snapshot honesty: `*.openapi.json` byte-identical; `*.endpoints.json` byte-identical except where the PR body declares an addition, and every added line is a path the block already served at that level.
3. Path reads: no `path_param`, `strip_prefix("/b`, `starts_with("/b`, or hand parsing of `msg.path()` left in the migrated blocks; every `msg.var(..)` read sits behind a row that binds it.
4. Tests: each production change has a test that fails without it; no test made vacuous (e.g. a `routed(..)` helper masking a lost validation).
5. CLAUDE.md compliance; no shims, no hardcoded lists, no raw SQL.
6. Comments now false, dead code, visibility widened without need, `unwrap`/`expect` reachable from user input.
7. Anything a careful senior reviewer would block on.

## Output

**A. Findings that need fixing** — numbered; file:line at PR head; what, why, confidence 0-100, concrete fix. Verified items only; no style nits or compiler-caught issues. Include the skill's findings that pass your verification, marked as such.

**B. Verified OK** — one line per invariant checked and holding.
