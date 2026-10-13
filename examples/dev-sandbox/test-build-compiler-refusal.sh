#!/usr/bin/env bash
#
# `build.sh` refuses a missing or stale `compiler/dist/` instead of building
# the compiler, and every wasm-pack build it (and measure.sh) runs passes
# `--locked`.
#
# Building the compiler is the ~55 minute, 12.6 GB composition a 7 GB runner
# cannot survive, so this never runs anything real. It copies `build.sh`,
# `seeds/` and `compiler/PIN.json` into a temp tree laid out like the repo
# (`build.sh` resolves everything from its own directory, and the repo root
# as `../..`), puts a fake `compiler/build-compiler.sh` there, and puts fake
# `wasm-pack` and `impresspress` on PATH. Each fake touches a sentinel, and the
# fake tools exit 1 — nothing can compile.
#
# Which assertions carry each stale case: the build-compiler sentinel is
# ABSENT, and stderr names `fetch-dist.sh`. Those two are what fail when the
# refusal is reverted (the old code ran the fake `build-compiler.sh`, then
# died in `check_compiler` because the temp tree has no verifier). The
# non-zero exit is asserted too, but it held before the refusal existed, so
# it proves nothing on its own.
#
# Usage: examples/dev-sandbox/test-build-compiler-refusal.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

PIN_VERSION="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["version"])' "$HERE/compiler/PIN.json")"
PIN_SHA="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["rubrc"]["sha"])' "$HERE/compiler/PIN.json")"
OTHER_SHA="0123456789abcdef0123456789abcdef01234567"
# The tree a pin bump leaves behind: the pin's version without its rebuild
# suffix (`807ace9e` for `807ace9e.2`), which is the stale tree that used to
# send build.sh into a composition.
OTHER_VERSION="${PIN_VERSION%.*}"
[ "$OTHER_VERSION" != "$PIN_VERSION" ] || OTHER_VERSION="$PIN_VERSION.0"

failures=0
ok() { printf 'ok   %s\n' "$1"; }
fail() {
  printf 'FAIL %s\n' "$1" >&2
  failures=$((failures + 1))
}

SENTINELS="$TMP/sentinels"
FAKE_BIN="$TMP/bin"
mkdir -p "$FAKE_BIN"
for tool in wasm-pack impresspress; do
  cat >"$FAKE_BIN/$tool" <<FAKE
#!/usr/bin/env bash
touch "$SENTINELS/$tool"
exit 1
FAKE
  chmod +x "$FAKE_BIN/$tool"
done

# A fresh copy of the dev-sandbox directory for each case, with `dist/` laid
# out as the case says: `none`, or a manifest carrying `version` and `sha`.
fake_tree() {
  local version="$1" sha="$2"
  rm -rf "$TMP/examples" "$SENTINELS"
  mkdir -p "$SENTINELS"
  local root="$TMP/examples/dev-sandbox"
  mkdir -p "$root/compiler"
  cp "$HERE/build.sh" "$root/build.sh"
  cp -R "$HERE/seeds" "$root/seeds"
  rm -rf "$root/seeds/__pycache__"
  cp "$HERE/compiler/PIN.json" "$root/compiler/PIN.json"
  cat >"$root/compiler/build-compiler.sh" <<FAKE
#!/usr/bin/env bash
touch "$SENTINELS/build-compiler"
exit 0
FAKE
  chmod +x "$root/compiler/build-compiler.sh"
  if [ "$version" != none ]; then
    mkdir -p "$root/compiler/dist"
    python3 - "$root/compiler/dist/manifest.json" "$version" "$sha" <<'MANIFEST'
import json, sys
json.dump({"version": sys.argv[2], "rubrc": {"sha": sys.argv[3]}}, open(sys.argv[1], "w"))
MANIFEST
  fi
}

# Runs the copied build.sh; sets STATUS and leaves stderr in $TMP/stderr.
run_build() {
  STATUS=0
  env -u IMPRESSPRESS PATH="$FAKE_BIN:$PATH" \
    "$TMP/examples/dev-sandbox/build.sh" >"$TMP/stdout" 2>"$TMP/stderr" || STATUS=$?
}

stderr_has() { grep -qF -- "$1" "$TMP/stderr"; }

# One stale case: everything a refusal must and must not do.
refused() {
  local what="$1" before="$failures"
  shift
  run_build
  # Load-bearing: the compiler was not built, and the fix was named.
  if [ -e "$SENTINELS/build-compiler" ]; then
    fail "$what: build.sh ran compiler/build-compiler.sh"
  else
    ok "$what: compiler/build-compiler.sh not run"
  fi
  if stderr_has "$TMP/examples/dev-sandbox/compiler/fetch-dist.sh"; then
    ok "$what: stderr names compiler/fetch-dist.sh"
  else
    fail "$what: stderr does not name $TMP/examples/dev-sandbox/compiler/fetch-dist.sh"
  fi
  # Supporting: nothing past the refusal ran, and it exited non-zero.
  if [ -e "$SENTINELS/wasm-pack" ] || [ -e "$SENTINELS/impresspress" ]; then
    fail "$what: build.sh went on to wasm-pack or the CLI"
  else
    ok "$what: no wasm-pack or CLI run"
  fi
  if [ "$STATUS" -ne 0 ]; then
    ok "$what: exits non-zero"
  else
    fail "$what: exited 0"
  fi
  local needle
  for needle in "$@"; do
    if stderr_has "$needle"; then
      ok "$what: stderr says '$needle'"
    else
      fail "$what: stderr does not say '$needle'"
    fi
  done
  if [ "$failures" -ne "$before" ]; then
    sed 's/^/    | /' "$TMP/stderr" >&2
  fi
}

fake_tree none none
refused "no compiler/dist" "dist/manifest.json is missing"

fake_tree "$OTHER_VERSION" "$PIN_SHA"
refused "a dist built for another version" "built for compiler $OTHER_VERSION;" "PIN.json pins $PIN_VERSION"

fake_tree "$PIN_VERSION" "$OTHER_SHA"
refused "a dist built from another rubrc" "built from rubrc ${OTHER_SHA:0:8};" "PIN.json pins ${PIN_SHA:0:8}"

# A current tree is accepted and not rebuilt. The run then stops in
# `check_compiler` (the temp tree has no verifier script), so its exit code
# says nothing here; the log line and the sentinel are the assertions.
fake_tree "$PIN_VERSION" "$PIN_SHA"
run_build
if stderr_has "compiler/dist is current for compiler/PIN.json"; then
  ok "a current dist: logged as current"
else
  fail "a current dist: no 'compiler/dist is current' line on stderr"
  sed 's/^/    | /' "$TMP/stderr" >&2
fi
if [ -e "$SENTINELS/build-compiler" ]; then
  fail "a current dist: build.sh ran compiler/build-compiler.sh"
else
  ok "a current dist: compiler/build-compiler.sh not run"
fi

# Grep guard: every wasm-pack INVOCATION in build.sh and measure.sh passes
# `--locked`, like every other wasm-pack build in CI. An invocation is
# `wasm-pack build` in command position — at the start of a line or after
# `(`, `;`, `&` or `|`; the comment, `log` and `echo` lines that only mention
# it are quoted or commented, and skipped. A file with no invocation left
# fails too, so the guard cannot pass by matching nothing.
for script in build.sh measure.sh; do
  invocations="$(grep -nE '(^|[(;&|])[[:space:]]*wasm-pack build' "$HERE/$script" | grep -Ev '^[0-9]+:[[:space:]]*#' || true)"
  if [ -z "$invocations" ]; then
    fail "grep guard: no wasm-pack build invocation found in $script"
    continue
  fi
  unlocked="$(printf '%s\n' "$invocations" | grep -v -- '--locked' || true)"
  if [ -n "$unlocked" ]; then
    fail "grep guard: $script runs wasm-pack build without --locked:"
    printf '    %s\n' "$unlocked" >&2
  else
    ok "grep guard: every wasm-pack build in $script passes --locked"
  fi
done

if [ "$failures" -ne 0 ]; then
  printf '\ntest-build-compiler-refusal.sh: %s failure(s)\n' "$failures" >&2
  exit 1
fi
printf '\ntest-build-compiler-refusal.sh: all cases pass\n'
