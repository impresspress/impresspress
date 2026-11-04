#!/usr/bin/env bash
#
# grep-guard-block-infos.sh — a runtime build never constructs the block set
# to read declarations; it reads the ones its registrations read.
#
# `impresspress_core::blocks::all_block_infos()` constructs every feature
# block and calls its `info()`. `impresspress/products`' declaration alone is
# 605 KB of JSON, so one such sweep was about 9 ms of a native cold build and
# about three times that on a Worker, where every cold isolate builds its
# runtime. Since wafer-run#478 the runtime reads each block's `BlockInfo` once,
# at registration, and lends it as `Wafer::block_infos()`; the prepared plan's
# application block inventory and the router's declarations come from there
# (`ImpresspressBuilder::build_with_plan_exporter`). Before that pin, the
# plan import and the plan exporter each swept the block set again on every
# Cloudflare build. The counting test
# (`builder::prepared::tests::a_prepared_build_reads_each_declaration_once`)
# catches a re-read of a consumer block's declaration; this catches the
# manifest sweep, which constructs blocks the test does not hold.
#
# What it flags: `all_block_infos`, in every tracked file under
# `crates/impresspress-core/src/builder/` (the build every target runs) and
# `crates/impresspress-cloudflare/src/` (the Worker's request-path build),
# by `git ls-files`, never a shell glob. One line is exempt, by file and exact
# content: the test that holds the plan's inventory to the manifest's set.
#
# Usage: scripts/grep-guard-block-infos.sh  (from anywhere in the repository)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

pattern='all_block_infos'

# The pattern against the shapes it exists for, so a regex that stops
# matching fails the guard instead of passing it.
self_check() {
  local want got
  want=$'let infos = crate::blocks::all_block_infos();\nimpresspress_core::blocks::all_block_infos()\n/// reads `all_block_infos()` per build'
  got="$(printf '%s\n' "$want" 'wafer.block_infos()' 'real_block_infos()' |
    grep -E "$pattern" || true)"
  if [ "$got" != "$want" ]; then
    echo "grep-guard-block-infos: self-check failed; the pattern cannot be trusted." >&2
    printf '  expected:\n%s\n  matched:\n%s\n' "$want" "$got" >&2
    return 1
  fi
}

self_check || exit 2

# `file:content` of each exempt line, matched exactly (the line number may
# move; the content may not).
exempt_lines=(
  'crates/impresspress-core/src/builder/prepared.rs:        let mut expected: Vec<String> = crate::blocks::all_block_infos()'
)
for entry in "${exempt_lines[@]}"; do
  file="${entry%%:*}" content="${entry#*:}"
  if ! grep -qxF -- "$content" "$file"; then
    echo "grep-guard-block-infos: exempted line no longer in $file; update the list: $content" >&2
    exit 1
  fi
done

if ! files="$(git ls-files -- crates/impresspress-core/src/builder crates/impresspress-cloudflare/src)" ||
  [ -z "$files" ]; then
  echo "grep-guard-block-infos: no files found; the file list is broken" >&2
  exit 1
fi

violations="$(printf '%s\n' "$files" | tr '\n' '\0' | xargs -0 grep -nE "$pattern" -- || true)"
# Drop the exempt lines: `file:N:content` whose file and content match one.
violations="$(printf '%s\n' "$violations" | while IFS= read -r hit; do
  [ -z "$hit" ] && continue
  file="${hit%%:*}" rest="${hit#*:}"
  content="${rest#*:}"
  for entry in "${exempt_lines[@]}"; do
    [ "$file:$content" = "$entry" ] && continue 2
  done
  printf '%s\n' "$hit"
done)"

if [ -n "$violations" ]; then
  printf '%s\n' "$violations" >&2
  echo "grep-guard-block-infos: a runtime build reads declarations from its registrations." >&2
  echo "  Use Wafer::block_infos() after the blocks are registered (see" >&2
  echo "  ImpresspressBuilder::register), not blocks::all_block_infos()." >&2
  exit 1
fi
echo "grep-guard-block-infos: self-check passed; $(printf '%s\n' "$files" | wc -l) file(s) scanned, no block-set sweep in a runtime build."
