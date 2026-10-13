#!/usr/bin/env bash
#
# grep-guard-locked.sh — every wasm-pack run and every `cargo install --path`
# this repository runs or tells people to run goes through the lock check.
#
# A grep guard, not a behaviour test: `test-build-guards.sh` proves the
# wrappers refuse a stale lock, but it cannot see a workflow step, a recipe
# or a README line that calls the tools directly. Both tools take `--locked`
# without guarding against a stale lock (see `check-cargo-lock.sh`), so a
# direct call quietly drops the guard. This fails on:
#   * `wasm-pack build` or `wasm-pack test` written as a command, instead of
#     `scripts/wasm-pack-locked.sh CRATE_DIR build|test …`;
#   * `cargo install … --path …` whose previous non-blank line does not run
#     `check-cargo-lock.sh`.
#
# Where it looks: the code lines (not comments, and not a step's `name:`) of
# `.github/workflows/*.yml`,
# the `justfile` and every tracked `*.sh` under `scripts/` and `examples/`;
# and the fenced code blocks of `README.md`, `examples/*/README.md`,
# `crates/*/README.md` and `docs/*.md` (top level only: `docs/superpowers/`
# holds dated plans and specs that record what was run, not instructions).
#
# Allowed exceptions are labelled on the line itself with `lock-guard: allow`
# and a reason, e.g. a log line that only prints the command it is about to
# run through the wrapper.
#
# Usage: scripts/grep-guard-locked.sh  (from anywhere in the repository)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

files=()
while IFS= read -r f; do files+=("$f"); done < <(
  git ls-files -- '.github/workflows/*.yml' justfile 'scripts/*.sh' 'examples/*.sh' \
    README.md 'examples/*/README.md' 'crates/*/README.md' 'docs/*.md' |
    grep -v '^docs/.*/' || true
)

# One awk pass per file. Code lines are the whole file for yml/justfile/sh,
# and fenced blocks for Markdown; comment lines are skipped (after stripping
# a diff block's ` `/`+` marker; a `-` line in a diff is removed text).
violations="$(
  for f in "${files[@]}"; do
    awk -v file="$f" '
      BEGIN { md = (file ~ /\.md$/); fence = 0; lang = ""; prev = "" }
      md && /^[[:space:]]*```/ {
        if (fence) { fence = 0 } else { fence = 1; lang = $0; sub(/^[[:space:]]*```/, "", lang) }
        prev = ""; next
      }
      md && !fence { next }
      {
        line = $0
        if (md && lang == "diff") {
          if (line ~ /^-/) next
          sub(/^[ +]/, "", line)
        }
        if (line ~ /^[[:space:]]*#/) next
        if (line ~ /^[[:space:]]*$/) next
        # A workflow step title names a command, it does not run one.
        if (file ~ /\.yml$/ && line ~ /^[[:space:]]*(-[[:space:]]+)?name:/) next
        allowed = (line ~ /lock-guard: allow/)
        if (!allowed && line ~ /(^|[^[:alnum:]_./-])wasm-pack[[:space:]]+(build|test)([^[:alnum:]_-]|$)/)
          printf "%s:%d: wasm-pack run directly; use scripts/wasm-pack-locked.sh\n    %s\n", file, NR, $0
        if (!allowed && line ~ /cargo[[:space:]]+install[[:space:]].*--path/ && prev !~ /check-cargo-lock\.sh/)
          printf "%s:%d: cargo install --path without scripts/check-cargo-lock.sh on the line before\n    %s\n", file, NR, $0 # lock-guard: allow (message text)
        prev = line
      }
    ' "$f"
  done
)"

if [ -n "$violations" ]; then
  printf '%s\n' "$violations" >&2
  echo "grep-guard-locked: a direct wasm-pack or cargo install --path drops the stale-lock check." >&2 # lock-guard: allow (message text)
  echo "  Route it through scripts/wasm-pack-locked.sh / scripts/check-cargo-lock.sh, or label" >&2
  echo "  a line that only prints a command with 'lock-guard: allow (<reason>)'." >&2
  exit 1
fi
echo "grep-guard-locked: ${#files[@]} file(s) scanned, no unchecked wasm-pack or cargo install --path." # lock-guard: allow (message text)
