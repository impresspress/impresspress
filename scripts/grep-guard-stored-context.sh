#!/usr/bin/env bash
#
# grep-guard-stored-context.sh — no struct, enum or static in non-test code
# keeps a runtime context unless the line says how long it lives.
#
# The rule is wafer-run's, on `Context::clone_arc`: the handle is for work
# that ends with the call that received it — a deferred task drained after
# the response, or the producer of an output stream that finishes when the
# stream does. Never store it in block or service state. The runtime's
# context owns every registered block, so a handle kept by a block (or by a
# service a block owns) is a reference cycle, and the runtime it belongs to
# is never freed. `AuthServiceImpl` did exactly that until 2026-10: it kept
# its `Init` context in a `OnceLock`, and every runtime build leaked the
# whole runtime (about 13 MB of Wasm memory per build on a Cloudflare
# isolate capped at 128 MB). A service that needs to reach other blocks
# takes the `ctx` of each call as a parameter instead, as `AuthService` does.
#
# A grep guard, not the proof: the proof is the `Weak` probe that a dropped
# runtime's `wafer-run/auth` block is gone
# (`crates/impresspress-server/tests/runtime_drop.rs`, and under
# wasm-bindgen-test in `crates/impresspress-browser/src/boot/tests.rs`). A
# probe only sees a context stored on a path its test runs, though; a block
# that stores one on its first request of some other kind is left to this.
#
# What it flags, where the type holds `Arc<dyn …Context …>` or
# `Rc<dyn …Context …>` (extra trait-object bounds such as `+ Send + Sync`
# included) or `Arc`/`Rc` of a `RuntimeContext`, however wrapped
# (`Option<…>`, `OnceLock<…>`, `Mutex<…>`):
#   * a field of a struct (named or tuple), including one whose header runs
#     over several lines (generics, a `where` clause);
#   * a variant of an enum, tuple or struct-like;
#   * a `static`;
#   * a `type` alias, which would otherwise smuggle the type into a field
#     under another name.
# A function parameter or a local of that type is not storage and is not
# flagged. Out of reach and left out on purpose: `Box<dyn Context>` and an
# `Arc<` split from its `dyn Context>` across lines — no owned context can be
# made from a `&dyn Context` except through `clone_arc`, which returns `Arc`.
#
# Where it looks: every tracked `crates/*/src/**/*.rs`, minus test code:
# files whose path names `test_support` (compiled for tests or the
# `test-support` feature only) and `#[cfg(test)]` modules.
#
# A holder that is legitimately request-scoped carries, on its own line or
# the line above, `stored-context: allow (<what ends it>)`, e.g.
#     // stored-context: allow (a deferred task; owned by no block)
#
# Every run first checks the scanner itself against a fixture of the shapes
# above (and of the shapes it must leave alone), so a regex that stops
# matching fails the guard instead of passing it.
#
# Usage: scripts/grep-guard-stored-context.sh  (from anywhere in the repository)
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

# Prints one violation per stored context in the files named on the command
# line.
scan() {
  local f
  for f in "$@"; do
    awk -v file="$f" '
      function braces(s,   t, o, c) {
        t = s; o = gsub(/\{/, "", t); t = s; c = gsub(/\}/, "", t); return o - c
      }
      function report() { printf "%s:%d: a stored runtime context\n    %s\n", file, NR, line }
      BEGIN {
        ctxty = "(Arc|Rc)<[[:space:]]*(dyn[[:space:]]+([A-Za-z_]+::)*Context([[:space:]]*\\+[^>]*)?|([A-Za-z_]+::)*RuntimeContext)[[:space:]]*>"
        item = "(^|[[:space:]])(struct|enum)[[:space:]]+[A-Za-z_0-9]+"
        depth = 0; skip_at = -1; body_at = -1; header = 0; cfg_test = 0; prev = ""
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
        starts_item = (body_at < 0 && !header && code ~ item)
        if (starts_item) header = 1
        is_static = (code ~ /(^|[[:space:]])static[[:space:]]+[A-Za-z_0-9]+[[:space:]]*:/)
        is_alias = (code ~ /(^|[[:space:]])type[[:space:]]+[A-Za-z_0-9]+.*=/)
        if (!allowed && code ~ ctxty && (header || body_at >= 0 || is_static || is_alias))
          report()

        # A header ends at the body it opens, or at the `;` of a unit or
        # tuple struct.
        if (header && code ~ /\{/) { header = 0; body_at = depth }
        else if (header && code ~ /;/) header = 0
        depth += braces(code)
        if (body_at >= 0 && depth <= body_at) body_at = -1
        prev = line
      }
    ' "$f"
  done
}

# The scanner against the shapes it exists for. Every line marked `FLAG`
# must be reported and nothing else may be.
self_check() {
  local tmp out want got
  tmp="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" RETURN
  cat > "$tmp/shapes.rs" <<'RUST'
use std::sync::{Arc, Mutex, OnceLock};
pub struct Plain {
    ctx: Arc<dyn Context>, // FLAG
}
pub struct Bounded {
    ctx: Arc<dyn Context + Send + Sync>, // FLAG
    other: std::sync::Arc<dyn wafer_run::context::Context + 'static>, // FLAG
}
pub(crate) struct Wrapped {
    cell: OnceLock<Arc<dyn Context>>, // FLAG
    slot: Arc<Mutex<Option<Rc<dyn wafer_block::Context>>>>, // FLAG
}
struct Tuple(Arc<dyn Context>); // FLAG
struct TupleLines(
    u8,
    Arc<dyn Context>, // FLAG
);
pub struct Generic<T>
where
    T: Clone,
{
    value: T,
    ctx: Arc<dyn Context>, // FLAG
}
enum State {
    Idle,
    Ready(Arc<dyn Context>), // FLAG
    Named { ctx: Arc<RuntimeContext> }, // FLAG
}
static GLOBAL: OnceLock<Arc<dyn Context>> = OnceLock::new(); // FLAG
pub type Ctx = Arc<dyn Context>; // FLAG
struct Labelled {
    // stored-context: allow (a deferred task; owned by no block)
    ctx: Arc<dyn Context>,
}
fn takes(ctx: Arc<dyn Context>) -> Arc<dyn Context> {
    let local: Arc<dyn Context> = ctx;
    local
}
fn takes_lines(
    ctx: Arc<dyn Context + Send>,
) {
    let _ = move || ctx.clone();
}
impl Plain {
    fn get(&self) -> Arc<dyn Context> {
        self.ctx.clone()
    }
}
#[cfg(test)]
mod tests {
    struct InTest {
        ctx: Arc<dyn Context>,
    }
}
RUST
  want="$(grep -n '// FLAG' "$tmp/shapes.rs" | cut -d: -f1 | sort -n | tr '\n' ' ')"
  out="$(scan "$tmp/shapes.rs")"
  got="$(grep -oE '^[^ ]+:[0-9]+:' <<< "$out" | cut -d: -f2 | sort -n | tr '\n' ' ' || true)"
  if [ "$want" != "$got" ]; then
    echo "grep-guard-stored-context: self-check failed; the scanner cannot be trusted." >&2
    echo "  expected flagged lines: $want" >&2
    echo "  flagged lines:          $got" >&2
    printf '%s\n' "$out" >&2
    return 1
  fi
}

self_check || exit 2

files=()
while IFS= read -r f; do files+=("$f"); done < <(
  git ls-files -- 'crates/*.rs' | grep -E '^crates/[^/]+/src/' | grep -v 'test_support' || true
)
if [ "${#files[@]}" -eq 0 ]; then
  echo "grep-guard-stored-context: no source files found; the file list is broken" >&2
  exit 1
fi

violations="$(scan "${files[@]}")"

if [ -n "$violations" ]; then
  printf '%s\n' "$violations" >&2
  echo "grep-guard-stored-context: a context stored in block or service state is a runtime leak." >&2
  echo "  Take the ctx of each call as a parameter instead. A holder that only lives as long as" >&2
  echo "  the work it was made for (a deferred task, a stream producer) says so with" >&2
  echo "  'stored-context: allow (<what ends it>)' on its line or the line above." >&2
  exit 1
fi
echo "grep-guard-stored-context: self-check passed; ${#files[@]} file(s) scanned, no stored runtime context."
