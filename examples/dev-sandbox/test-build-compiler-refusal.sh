#!/usr/bin/env bash
#
# `build.sh` refuses a missing or stale `compiler/dist/` instead of building
# the compiler, and every wasm-pack build that `build.sh`, `measure.sh` and
# the justfile run passes `--locked`.
#
# Building the compiler is the ~55 minute, 12.6 GB composition a 7 GB runner
# cannot survive, so this never runs anything real. It lays out a temp tree
# shaped like the repo (`build.sh` and `measure.sh` resolve everything from
# their own directory, and the repo root as `../..`):
#   * `examples/dev-sandbox/` with copies of `build.sh`, `measure.sh`,
#     `seeds/` and `compiler/PIN.json`, a fake `compiler/build-compiler.sh`
#     and a stub `compiler/scripts/verify-compiler-assets.mjs` that accepts
#     any tree, so a current tree gets past `check_compiler`;
#   * an empty `crates/impresspress-web/`, so the wasm-pack steps' `cd`
#     succeeds and the fake wasm-pack is really reached.
# Fake `wasm-pack` and `impresspress` on PATH append their argv to a file and
# compile nothing; `wasm-pack` exits 0 so every wasm-pack step of a run is
# recorded, `impresspress` exits 1.
#
# Which assertions carry each stale case: the fake `build-compiler.sh` was
# NOT run, nothing reached wasm-pack or the CLI, and stderr names
# `fetch-dist.sh` and the reason. With the refusal reverted, the old code
# runs the fake `build-compiler.sh`, gets past `check_compiler` (the stub
# verifier, or no `dist/` to check) and reaches the fake wasm-pack, so all of
# those go red. The non-zero exit is asserted too, but the old code also
# exited non-zero (later, in `check-runtime-markers.mjs`, which the temp tree
# lacks), so it proves nothing on its own.
#
# `--locked` is asserted on the argv each fake wasm-pack call RECEIVED, not
# by grepping the scripts: an env-assignment prefix or a trailing comment
# cannot hide an invocation from that.
#
# Usage: examples/dev-sandbox/test-build-compiler-refusal.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$HERE/../.." && pwd -P)"

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

CALLS="$TMP/calls"
FAKE_BIN="$TMP/bin"
mkdir -p "$FAKE_BIN"
fake_tool() {
  local tool="$1" status="$2"
  cat >"$FAKE_BIN/$tool" <<FAKE
#!/usr/bin/env bash
printf '%s\n' "\$*" >>"$CALLS/$tool"
exit $status
FAKE
  chmod +x "$FAKE_BIN/$tool"
}
fake_tool wasm-pack 0
fake_tool impresspress 1

ROOT="$TMP/repo"
SANDBOX="$ROOT/examples/dev-sandbox"

# A fresh temp repo for each case, with `compiler/dist/manifest.json` as the
# case says: `none` for no `dist/`, otherwise the literal file content.
fake_tree() {
  local manifest="$1"
  rm -rf "$ROOT" "$CALLS"
  mkdir -p "$CALLS" "$SANDBOX/compiler/scripts" "$ROOT/crates/impresspress-web"
  cp "$HERE/build.sh" "$HERE/measure.sh" "$SANDBOX/"
  cp -R "$HERE/seeds" "$SANDBOX/seeds"
  rm -rf "$SANDBOX/seeds/__pycache__"
  cp "$HERE/compiler/PIN.json" "$SANDBOX/compiler/PIN.json"
  cp "$REPO/justfile" "$ROOT/justfile"
  cat >"$SANDBOX/compiler/build-compiler.sh" <<FAKE
#!/usr/bin/env bash
printf '%s\n' "\$*" >>"$CALLS/build-compiler"
exit 0
FAKE
  chmod +x "$SANDBOX/compiler/build-compiler.sh"
  printf 'process.exit(0);\n' >"$SANDBOX/compiler/scripts/verify-compiler-assets.mjs"
  if [ "$manifest" != none ]; then
    mkdir -p "$SANDBOX/compiler/dist"
    printf '%s' "$manifest" >"$SANDBOX/compiler/dist/manifest.json"
  fi
}

manifest_json() {
  python3 -c 'import json, sys; print(json.dumps({"version": sys.argv[1], "rubrc": {"sha": sys.argv[2]}}))' "$1" "$2"
}

# Runs `$1` (a script in the temp tree) with the fakes first on PATH; sets
# STATUS and leaves stderr in $TMP/stderr.
run_script() {
  STATUS=0
  env -u IMPRESSPRESS -u PKG_DIR -u PKG_SITE_DIR -u PKG_DEV_DIR PATH="$FAKE_BIN:$PATH" \
    "$1" >"$TMP/stdout" 2>"$TMP/stderr" || STATUS=$?
}

stderr_has() { grep -qF -- "$1" "$TMP/stderr"; }
calls_of() { if [ -f "$CALLS/$1" ]; then cat "$CALLS/$1"; fi; }

# One stale case: everything a refusal must and must not do.
refused() {
  local what="$1" before="$failures"
  shift
  run_script "$SANDBOX/build.sh"
  if [ -e "$CALLS/build-compiler" ]; then
    fail "$what: build.sh ran compiler/build-compiler.sh"
  else
    ok "$what: compiler/build-compiler.sh not run"
  fi
  if [ -e "$CALLS/wasm-pack" ] || [ -e "$CALLS/impresspress" ]; then
    fail "$what: build.sh went on to wasm-pack or the CLI"
  else
    ok "$what: no wasm-pack or CLI run"
  fi
  if stderr_has "$SANDBOX/compiler/fetch-dist.sh"; then
    ok "$what: stderr names compiler/fetch-dist.sh"
  else
    fail "$what: stderr does not name $SANDBOX/compiler/fetch-dist.sh"
  fi
  if stderr_has "Traceback"; then
    fail "$what: stderr carries a Python traceback"
  else
    ok "$what: no traceback"
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

# Every recorded wasm-pack call in `$2` passes `--locked` to cargo (after the
# `--`), and there are exactly `$3` of them.
all_locked() {
  local what="$1" calls="$2" want="$3" n
  n="$(printf '%s' "$calls" | grep -c . || true)"
  if [ "$n" -ne "$want" ]; then
    fail "$what: expected $want wasm-pack call(s), got $n"
    printf '    | %s\n' "$calls" >&2
    return
  fi
  local unlocked
  unlocked="$(printf '%s\n' "$calls" | grep -Ev -- ' -- (.* )?--locked( |$)' || true)"
  if [ -n "$unlocked" ]; then
    fail "$what: wasm-pack called without --locked:"
    printf '    | %s\n' "$unlocked" >&2
  else
    ok "$what: all $n wasm-pack call(s) pass --locked"
  fi
}

fake_tree none
refused "no compiler/dist" "dist/manifest.json is missing"

fake_tree "$(manifest_json "$OTHER_VERSION" "$PIN_SHA")"
refused "a dist built for another version" "built for compiler $OTHER_VERSION;" "PIN.json pins $PIN_VERSION"

fake_tree "$(manifest_json "$PIN_VERSION" "$OTHER_SHA")"
refused "a dist built from another rubrc" "built from rubrc ${OTHER_SHA:0:8};" "PIN.json pins ${PIN_SHA:0:8}"

fake_tree '{not json'
refused "an unparseable manifest" "dist/manifest.json is not JSON"

fake_tree '[]'
refused "a manifest that is not an object" "dist/manifest.json is not a manifest object"

fake_tree "{\"version\": \"$PIN_VERSION\", \"rubrc\": \"$PIN_SHA\"}"
refused "a manifest whose rubrc is not an object" "dist/manifest.json is not a manifest object"

# A current tree is accepted, not rebuilt, and goes on to both wasm-pack
# builds. It then stops at `check-runtime-markers.mjs`, which the temp tree
# does not have, so its exit code says nothing here.
fake_tree "$(manifest_json "$PIN_VERSION" "$PIN_SHA")"
run_script "$SANDBOX/build.sh"
if stderr_has "compiler/dist is current for compiler/PIN.json"; then
  ok "a current dist: logged as current"
else
  fail "a current dist: no 'compiler/dist is current' line on stderr"
  sed 's/^/    | /' "$TMP/stderr" >&2
fi
if [ -e "$CALLS/build-compiler" ]; then
  fail "a current dist: build.sh ran compiler/build-compiler.sh"
else
  ok "a current dist: compiler/build-compiler.sh not run"
fi
build_calls="$(calls_of wasm-pack)"
all_locked "build.sh" "$build_calls" 2
for out in pkg-dev pkg-site; do
  if printf '%s\n' "$build_calls" | grep -q -- "--out-dir $out "; then
    ok "build.sh: builds $out"
  else
    fail "build.sh: no wasm-pack call builds $out"
  fi
done

# measure.sh builds each of its three pkg dirs that is missing — all three
# in the temp tree — and then stops, finding no wasm the fake produced.
fake_tree none
run_script "$SANDBOX/measure.sh"
all_locked "measure.sh" "$(calls_of wasm-pack)" 3

# The justfile's recipe lines, run the way `just` runs them: each line on its
# own, through `sh -c`, from the justfile's directory. Every line that names
# wasm-pack is run, so an invocation cannot hide behind a prefix.
fake_tree none
just_lines=0
while IFS= read -r line; do
  line="${line#"${line%%[![:space:]]*}"}"
  case "$line" in '#'* | '') continue ;; esac
  just_lines=$((just_lines + 1))
  (cd "$ROOT" && PATH="$FAKE_BIN:$PATH" sh -c "$line") >/dev/null 2>&1 || true
done < <(grep -E '^[[:space:]]+.*wasm-pack' "$ROOT/justfile" || true)
if [ "$just_lines" -eq 0 ]; then
  fail "justfile: no recipe line runs wasm-pack — update this test"
else
  all_locked "justfile" "$(calls_of wasm-pack)" "$just_lines"
fi

if [ "$failures" -ne 0 ]; then
  printf '\ntest-build-compiler-refusal.sh: %s failure(s)\n' "$failures" >&2
  exit 1
fi
printf '\ntest-build-compiler-refusal.sh: all cases pass\n'
