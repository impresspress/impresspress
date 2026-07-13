#!/usr/bin/env bash
# Phase 5c grep-guard: full-page HTML must originate from impresspress-core/src/ui/.
# Catches blocks shipping their own page chrome instead of using a template.
#
# Signals matched (in .rs source):
#   - Maud's (DOCTYPE html) single-token compact form.
#   - Maud's (DOCTYPE) two-token form (followed by `html lang=...`).
#   - Raw "<html" or "<!DOCTYPE" string literals.
#
# Phase 5d removed the legalpages exemption: legal pages now render via the
# `public_page` template in impresspress-core/src/ui/templates.rs.
set -euo pipefail
hits=$(grep -rlnE --include='*.rs' \
  '\(DOCTYPE\s+html\)|\(DOCTYPE\)|<!DOCTYPE|<html\b' \
  crates/ \
  | grep -vE '^crates/impresspress-core/src/ui/' \
  || true) # grep exits 1 on empty input under pipefail; || true normalises that
if [ -n "$hits" ]; then
  echo "ERROR: full-page HTML markers found outside crates/impresspress-core/src/ui/:" >&2
  echo "$hits" >&2
  echo "" >&2
  echo "Page-level HTML must come from a template in impresspress-core/src/ui/templates.rs" >&2
  echo "or impresspress-core/src/ui/layout.rs. If this is intentional, update the guard." >&2
  exit 1
fi
echo "OK: no full-page HTML markers outside crates/impresspress-core/src/ui/."
