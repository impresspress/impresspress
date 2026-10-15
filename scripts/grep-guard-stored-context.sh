#!/usr/bin/env bash
#
# grep-guard-stored-context.sh — no struct in non-test code keeps a runtime
# context unless the line says how long it lives.
#
# The rule is wafer-run's, on `Context::clone_arc`: the handle is for work
# that ends with the call that received it — a deferred task drained after
# the response, or the producer of an output stream that finishes when the
# stream does. Never store it in block or service state. The runtime's
# context owns every registered block, so a handle kept by a block (or by a
# service a block owns) is a reference cycle, and the runtime it belongs to
# is never freed. `AuthServiceImpl` did exactly that until 2026-10: it kept
# its `Init` context in a `OnceLock`, and every runtime build leaked the
# whole runtime (about 13 MB per build on a Cloudflare isolate capped at
# 128 MB). A service that needs to reach other blocks takes the `ctx` of
# each call as a parameter instead, as `AuthService` does.
#
# A grep guard, not the proof: the proof is the `Weak` probe that a dropped
# runtime's `wafer-run/auth` block is gone
# (`crates/impresspress-server/tests/runtime_drop.rs`, and under
# wasm-bindgen-test in `crates/impresspress-browser/src/boot/tests.rs`).
# This is the reviewer's prompt for the shape that caused it.
#
# What it flags: a struct field (named or tuple) or a `static` whose type
# holds `Arc<dyn …Context>`, `Rc<dyn …Context>` or `Arc`/`Rc` of a
# `RuntimeContext`, however wrapped (`Option<…>`, `OnceLock<…>`,
# `Mutex<…>`). A function parameter of that type is not storage and is not
# flagged.
#
# Where it looks: every tracked `crates/*/src/**/*.rs`, minus test code:
# files whose path names `test_support` (compiled for tests or the
# `test-support` feature only) and `#[cfg(test)]` modules.
#
# A field that is legitimately request-scoped carries, on its own line or the
# line above, `stored-context: allow (<what ends it>)`, e.g.
#     // stored-context: allow (a deferred task, drained when the request ends)
#
# Usage: scripts/grep-guard-stored-context.sh  (from anywhere in the repository)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

files=()
while IFS= read -r f; do files+=("$f"); done < <(
  git ls-files -- 'crates/*.rs' | grep -E '^crates/[^/]+/src/' | grep -v 'test_support' || true
)
if [ "${#files[@]}" -eq 0 ]; then
  echo "grep-guard-stored-context: no source files found; the file list is broken" >&2
  exit 1
fi

violations="$(
  for f in "${files[@]}"; do
    awk -v file="$f" '
      function braces(s,   t, o, c) {
        t = s; o = gsub(/\{/, "", t); t = s; c = gsub(/\}/, "", t); return o - c
      }
      BEGIN {
        ctxty = "(Arc|Rc)<[[:space:]]*(dyn[[:space:]]+([A-Za-z_]+::)*Context|([A-Za-z_]+::)*RuntimeContext)[[:space:]]*>"
        depth = 0; skip_at = -1; struct_at = -1; cfg_test = 0; prev = ""
      }
      {
        line = $0
        code = line; sub(/\/\/.*/, "", code)
        if (skip_at >= 0) {
          depth += braces(code)
          if (depth <= skip_at) skip_at = -1
          prev = line; next
        }
        if (code ~ /^[[:space:]]*#\[cfg\(test\)\]/) { cfg_test = 1; prev = line; next }
        if (cfg_test && code ~ /^[[:space:]]*(pub(\([^)]*\))?[[:space:]]+)?mod[[:space:]]+[A-Za-z_0-9]+[[:space:]]*\{/) {
          skip_at = depth; depth += braces(code); cfg_test = 0
          if (depth <= skip_at) skip_at = -1
          prev = line; next
        }
        if (code !~ /^[[:space:]]*#\[/ && code !~ /^[[:space:]]*$/) cfg_test = 0

        allowed = (line ~ /stored-context: allow/ || prev ~ /stored-context: allow/)
        in_struct = (struct_at >= 0)
        is_struct_line = (code ~ /(^|[[:space:]])struct[[:space:]]+[A-Za-z_0-9]+/)
        is_static = (code ~ /(^|[[:space:]])static[[:space:]]+[A-Za-z_0-9]+[[:space:]]*:/)
        if (!allowed && code ~ ctxty && (in_struct || is_struct_line || is_static))
          printf "%s:%d: a stored runtime context\n    %s\n", file, NR, line

        if (struct_at < 0 && is_struct_line && code ~ /\{/) {
          struct_at = depth
        }
        depth += braces(code)
        if (struct_at >= 0 && depth <= struct_at) struct_at = -1
        prev = line
      }
    ' "$f"
  done
)"

if [ -n "$violations" ]; then
  printf '%s\n' "$violations" >&2
  echo "grep-guard-stored-context: a context stored in block or service state is a runtime leak." >&2
  echo "  Take the ctx of each call as a parameter instead. A field that only lives as long as" >&2
  echo "  the request (a deferred task, a stream producer) says so with" >&2
  echo "  'stored-context: allow (<what ends it>)' on its line or the line above." >&2
  exit 1
fi
echo "grep-guard-stored-context: ${#files[@]} file(s) scanned, no stored runtime context."
