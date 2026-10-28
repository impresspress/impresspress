#!/usr/bin/env bash
#
# grep-guard-strict-schema.sh — nothing in this repository names the
# strict-schema switch wafer-run no longer has.
#
# Since wafer-run 1272fd63 strict schema is the only behaviour of the
# database executor on every target: a write naming an undeclared column
# fails, a missing table is the backend's error, and there is no
# `WAFER_RUN__DATABASE__STRICT_SCHEMA` / `WAFER_RUN__POSTGRES__STRICT_SCHEMA`
# key and no `set_strict_schema`. The compiler catches a call to the removed
# API; it cannot catch a Worker var the CLI writes into `wrangler.toml`, an
# env export a script or a README sets, or a comment that still explains the
# executor by the switch. Each of those claims a setting that does nothing.
#
# What it flags: `STRICT_SCHEMA` and the identifier `strict_schema` (any
# `strict_schema…` name except the `…_strict_schema_columns` migration
# basenames, which are file names, not the switch).
#
# Where it looks: every tracked file under `crates/`, `.github/`, `scripts/`
# and `examples/`, plus `justfile` and `README.md`. Not RELEASE.md and not
# `docs/`, which record what earlier releases did. Not the migration files
# (`*.sql`): auth 010, products 004 and signal 001 explain themselves by the
# switch in their header comments, and a shipped migration's text is history
# (its hash and ledger row pin what ran), so it is not rewritten. This
# script names the pattern, so it is left out too.
#
# Usage: scripts/grep-guard-strict-schema.sh  (from anywhere in the repository)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

pattern='STRICT_SCHEMA|strict_schema(?!_columns)'

# The pattern against the shapes it exists for, so a regex that stops
# matching fails the guard instead of passing it.
self_check() {
  local want got
  want=$'WAFER_RUN__DATABASE__STRICT_SCHEMA = "true"\ndb.set_strict_schema(true);\nfn strict_schema(&self) -> bool'
  got="$(printf '%s\n' "$want" 'include_str!("010_strict_schema_columns.sqlite.sql")' 'strict schema is the only behaviour' |
    grep -P "$pattern" || true)"
  if [ "$got" != "$want" ]; then
    echo "grep-guard-strict-schema: self-check failed; the pattern cannot be trusted." >&2
    printf '  expected:\n%s\n  matched:\n%s\n' "$want" "$got" >&2
    return 1
  fi
}

self_check || exit 2

if ! files="$(git ls-files -- crates .github scripts examples justfile README.md |
  grep -v -e '\.sql$' -e '^scripts/grep-guard-strict-schema\.sh$')" || [ -z "$files" ]; then
  echo "grep-guard-strict-schema: no files found; the file list is broken" >&2
  exit 1
fi

violations="$(printf '%s\n' "$files" | tr '\n' '\0' | xargs -0 grep -nP "$pattern" -- || true)"

if [ -n "$violations" ]; then
  printf '%s\n' "$violations" >&2
  echo "grep-guard-strict-schema: wafer-run has no strict-schema switch any more." >&2
  echo "  Strict is the only behaviour on every target; describe the executor as it is, and" >&2
  echo "  set no such var or key." >&2
  exit 1
fi
echo "grep-guard-strict-schema: self-check passed; $(printf '%s\n' "$files" | wc -l) file(s) scanned, no strict-schema switch named."
