#!/usr/bin/env bash
#
# grep-guard-call-budget.sh — nothing in the code names the per-statement D1
# budget that wafer-run replaced with a per-call one.
#
# Since wafer-run 2cf5a593 (WR-4) a backend with a per-invocation limit counts
# calls, a `db.batch()` counting once: `StatementBudget`,
# `DbExec::statement_budget`, `STATEMENT_BUDGET_EXHAUSTED` and
# `STATEMENT_BUDGET_EXCEEDS_LIMIT` are gone, and impresspress renamed its side
# to match (`D1CallCount`, `IMPRESSPRESS_D1_CALLS_PER_INVOCATION`,
# `[cloudflare].d1_calls_per_invocation`). The compiler catches a use of a
# removed item; it cannot catch a comment that still explains the budget by
# statements ("statement budget", spaced), a detail-code string, a Worker var
# a script sets, or a TOML key in a doc. Each of those describes a limit that
# no longer exists.
#
# What it flags, case-insensitively: `statement.?budget` (the underscore,
# space and hyphen forms, and `StatementBudget`), `exceeds_limit`,
# `QUERIES_PER_INVOCATION`, `d1_queries_per_invocation` and `D1QueryCount`.
#
# Where it looks: every tracked file under `crates/`, `packages/` and
# `scripts/` (by `git ls-files`, never a shell glob), except:
#   * this script, which names the patterns;
#   * `crates/impresspress/tests/helpers_cloudflare_env.rs`, whose test
#     writes the old TOML key to prove the strict `[cloudflare]` table refuses
#     it by name.
# RELEASE.md and `docs/` are outside the scan: they record what earlier
# releases did.
#
# Usage: scripts/grep-guard-call-budget.sh  (from anywhere in the repository)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

pattern='statement.?budget|exceeds_limit|QUERIES_PER_INVOCATION|d1_queries_per_invocation|D1QueryCount'

# The pattern against the shapes it exists for, so a regex that stops
# matching fails the guard instead of passing it.
self_check() {
  local want got
  want=$'fn statement_budget(&self)\nuse service::StatementBudget;\n// the D1 statement budget admits it\n"database.statement_budget_exceeds_limit"\nIMPRESSPRESS_D1_QUERIES_PER_INVOCATION = "50"\nd1_queries_per_invocation = 50\nlet queries = D1QueryCount::new();'
  got="$(printf '%s\n' "$want" 'D1CallCount::new()' 'IMPRESSPRESS_D1_CALLS_PER_INVOCATION' 'a statement of the batch' |
    grep -iE "$pattern" || true)"
  if [ "$got" != "$want" ]; then
    echo "grep-guard-call-budget: self-check failed; the pattern cannot be trusted." >&2
    printf '  expected:\n%s\n  matched:\n%s\n' "$want" "$got" >&2
    return 1
  fi
}

self_check || exit 2

exempt=(
  scripts/grep-guard-call-budget.sh
  crates/impresspress/tests/helpers_cloudflare_env.rs
)
for f in "${exempt[@]}"; do
  if [ ! -f "$f" ]; then
    echo "grep-guard-call-budget: exempted $f does not exist; update the list" >&2
    exit 1
  fi
done

if ! files="$(git ls-files -- crates packages scripts | grep -v -x -F -f <(printf '%s\n' "${exempt[@]}"))" ||
  [ -z "$files" ]; then
  echo "grep-guard-call-budget: no files found; the file list is broken" >&2
  exit 1
fi

violations="$(printf '%s\n' "$files" | tr '\n' '\0' | xargs -0 grep -niE "$pattern" -- || true)"

if [ -n "$violations" ]; then
  printf '%s\n' "$violations" >&2
  echo "grep-guard-call-budget: the D1 budget counts calls, not statements." >&2
  echo "  Name it as it is now: D1CallCount, IMPRESSPRESS_D1_CALLS_PER_INVOCATION," >&2
  echo "  [cloudflare].d1_calls_per_invocation, database.call_budget_exhausted." >&2
  exit 1
fi
echo "grep-guard-call-budget: self-check passed; $(printf '%s\n' "$files" | wc -l) file(s) scanned, no per-statement budget named."
