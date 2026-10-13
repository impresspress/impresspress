#!/usr/bin/env bash
#
# Two build guards, checked by running the real scripts:
#   * `build.sh` refuses a missing or stale `compiler/dist/` instead of
#     building the compiler;
#   * `build.sh`, `measure.sh` and the justfile refuse an out-of-date
#     `Cargo.lock` before wasm-pack runs, and leave the lock as it was. All
#     three go through `scripts/wasm-pack-locked.sh`, which is copied into the
#     temp tree with the `check-cargo-lock.sh` it runs, and run for real.
#
# Building the compiler is the ~55 minute, 12.6 GB composition a 7 GB runner
# cannot survive, so this compiles nothing: the only real tool it runs is
# `cargo metadata`, against a fixture workspace. It lays out a temp tree
# shaped like the repo (`build.sh` and `measure.sh` resolve everything from
# their own directory, and the repo root as `../..`):
#   * `examples/dev-sandbox/` with copies of `build.sh`, `measure.sh`,
#     `seeds/` and `compiler/PIN.json`, a fake `compiler/build-compiler.sh`
#     and a stub `compiler/scripts/verify-compiler-assets.mjs` that accepts
#     any tree, so a current tree gets past `check_compiler`;
#   * `scripts/wasm-pack-locked.sh` and the `check-cargo-lock.sh` it runs;
#   * a two-crate cargo workspace at the root — `crates/impresspress-web`
#     depending by path on `crates/lockdep` — with a committed-style
#     `Cargo.lock` that is current. Path dependencies only, so cargo never
#     touches the network.
# Fake `wasm-pack` and `impresspress` on PATH append their argv to a file and
# compile nothing; `wasm-pack` exits 0 so every wasm-pack step of a run is
# recorded, `impresspress` exits 1.
#
# The fake wasm-pack first runs a plain `cargo metadata` in the directory it
# was started from, because that is what the real one does before it builds
# anything (wasm-pack 0.14.0 and 0.15.0 both read the crate with
# `cargo metadata`, no `--locked`). That is the step that rewrites a stale
# lock, and the reason `wasm-pack build … -- --locked` guards nothing: with a
# stale lock the real wasm-pack exits 0 and leaves the lock rewritten, and so
# does this fake.
#
# Which assertions carry each stale-compiler case: the fake `build-compiler.sh` was
# NOT run, nothing reached wasm-pack or the CLI, and stderr names
# `fetch-dist.sh` and the reason. With the refusal reverted, the old code
# runs the fake `build-compiler.sh`, gets past `check_compiler` (the stub
# verifier, or no `dist/` to check) and reaches the fake wasm-pack, so all of
# those go red. The non-zero exit is asserted too, but the old code also
# exited non-zero (later, in `check-runtime-markers.mjs`, which the temp tree
# lacks), so it proves nothing on its own.
#
# The lock cases make the lock stale the way a forgotten `cargo update` does:
# `crates/impresspress-web` raises its version requirement on `lockdep` to
# one the lock does not have. Each entry point must then exit non-zero,
# never reach wasm-pack, and leave `Cargo.lock` byte-identical; with a
# current lock it must reach wasm-pack, started from the crate directory.
# Without the check `wasm-pack-locked.sh` runs (or with an entry point calling
# wasm-pack directly) the fake wasm-pack rewrites the lock and the stale
# cases go red. `build.sh` also checks the lock itself before step 1, so a
# run given both `--pkg-dir` and `--site-pkg-dir`, which reaches no
# wasm-pack, refuses too; that case's exit code and "no CLI" assertions hold
# even without the check (the run stops at the markers script the temp tree
# lacks), so its stderr assertion is the one that carries it. Two last cases
# run the wrapper directly: a missing lock is named as missing and not
# created, and a manifest cargo cannot parse is reported as a cargo failure,
# not as a stale lock.
#
# Usage: examples/dev-sandbox/test-build-guards.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$HERE/../.." && pwd -P)"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export CARGO_NET_OFFLINE=true

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
  local tool="$1" status="$2" first="${3:-}"
  cat >"$FAKE_BIN/$tool" <<FAKE
#!/usr/bin/env bash
$first
printf '%s\n' "\$*" >>"$CALLS/$tool"
exit $status
FAKE
  chmod +x "$FAKE_BIN/$tool"
}
# What the real wasm-pack does before anything else: read the crate with a
# plain `cargo metadata`, which rewrites a stale `Cargo.lock`. It also
# records the directory it was started in, because `--out-dir` resolves
# against it: the wrapper must run it from the crate.
fake_tool wasm-pack 0 "pwd -P >>\"$CALLS/wasm-pack.cwd\"; cargo metadata --format-version 1 >/dev/null || exit 101"
fake_tool impresspress 1

# The fixture workspace, with a current lock generated once and copied into
# every temp tree. `stale_lock` (below) moves both the crate's version and
# `impresspress-web`'s requirement on it.
WORKSPACE="$TMP/workspace"
mkdir -p "$WORKSPACE/crates/impresspress-web/src" "$WORKSPACE/crates/lockdep/src"
cat >"$WORKSPACE/Cargo.toml" <<'TOML'
[workspace]
members = ["crates/impresspress-web", "crates/lockdep"]
resolver = "2"
TOML
cat >"$WORKSPACE/crates/impresspress-web/Cargo.toml" <<'TOML'
[package]
name = "impresspress-web"
version = "0.1.0"
edition = "2021"

[dependencies]
lockdep = { path = "../lockdep", version = "0.1" }
TOML
cat >"$WORKSPACE/crates/lockdep/Cargo.toml" <<'TOML'
[package]
name = "lockdep"
version = "0.1.0"
edition = "2021"
TOML
: >"$WORKSPACE/crates/impresspress-web/src/lib.rs"
: >"$WORKSPACE/crates/lockdep/src/lib.rs"
(cd "$WORKSPACE" && cargo generate-lockfile --quiet)

ROOT="$TMP/repo"
SANDBOX="$ROOT/examples/dev-sandbox"
LOCK="$ROOT/Cargo.lock"

# A fresh temp repo for each case, with `compiler/dist/manifest.json` as the
# case says: `none` for no `dist/`, otherwise the literal file content.
fake_tree() {
  local manifest="$1"
  rm -rf "$ROOT" "$CALLS"
  cp -R "$WORKSPACE" "$ROOT"
  mkdir -p "$CALLS" "$SANDBOX/compiler/scripts" "$ROOT/scripts"
  cp "$HERE/build.sh" "$HERE/measure.sh" "$SANDBOX/"
  cp "$REPO/scripts/wasm-pack-locked.sh" "$REPO/scripts/check-cargo-lock.sh" "$ROOT/scripts/"
  cp -R "$HERE/seeds" "$SANDBOX/seeds"
  rm -rf "$SANDBOX/seeds/__pycache__"
  cp "$HERE/compiler/PIN.json" "$SANDBOX/compiler/PIN.json"
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

# Runs `$1` (a script in the temp tree) with the arguments after it and the
# fakes first on PATH; sets STATUS and leaves stderr in $TMP/stderr.
run_script() {
  STATUS=0
  env -u IMPRESSPRESS -u PKG_DIR -u PKG_SITE_DIR -u PKG_DEV_DIR PATH="$FAKE_BIN:$PATH" \
    "$@" >"$TMP/stdout" 2>"$TMP/stderr" || STATUS=$?
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

# Makes the temp tree's lock stale: the version requirement on `lockdep`
# moves to one the lock does not record. Keeps a copy of the lock as it was.
stale_lock() {
  sed -i.bak 's/^version = "0.1.0"$/version = "0.2.0"/' "$ROOT/crates/lockdep/Cargo.toml"
  sed -i.bak 's/version = "0.1" }/version = "0.2" }/' "$ROOT/crates/impresspress-web/Cargo.toml"
  rm -f "$ROOT/crates/lockdep/Cargo.toml.bak" "$ROOT/crates/impresspress-web/Cargo.toml.bak"
  if grep -q 'version = "0.2" }' "$ROOT/crates/impresspress-web/Cargo.toml" &&
    grep -q '^version = "0.2.0"$' "$ROOT/crates/lockdep/Cargo.toml"; then
    :
  else
    fail "fixture: the stale-lock edit did not apply — update this test"
  fi
  cp "$LOCK" "$TMP/Cargo.lock.before"
}

# The assertions every stale-lock case shares: `$1` names the entry point,
# STATUS and $TMP/stderr are the run's. `$2` is `wrapper` (the default) when
# the refusal comes from `wasm-pack-locked.sh`, `check` when the entry point
# runs `check-cargo-lock.sh` itself.
lock_refused() {
  local what="$1" via="${2:-wrapper}" before="$failures"
  if [ "$STATUS" -ne 0 ]; then
    ok "$what: a stale Cargo.lock exits non-zero"
  else
    fail "$what: a stale Cargo.lock exited 0"
  fi
  if cmp -s "$LOCK" "$TMP/Cargo.lock.before"; then
    ok "$what: Cargo.lock left as it was"
  else
    fail "$what: Cargo.lock was rewritten"
    diff "$TMP/Cargo.lock.before" "$LOCK" | sed 's/^/    | /' >&2 || true
  fi
  if [ -e "$CALLS/wasm-pack" ] || [ -e "$CALLS/impresspress" ]; then
    fail "$what: wasm-pack or the CLI ran on a stale Cargo.lock"
  else
    ok "$what: neither wasm-pack nor the CLI run"
  fi
  if stderr_has "check-cargo-lock.sh: Cargo.lock is out of date" &&
    { [ "$via" = check ] || stderr_has "wasm-pack-locked.sh: wasm-pack was not run"; }; then
    ok "$what: stderr says the lock is out of date"
  else
    fail "$what: stderr does not say the lock is out of date"
  fi
  if [ "$failures" -ne "$before" ]; then
    sed 's/^/    | /' "$TMP/stderr" >&2
  fi
}

# The wasm-pack calls a run with a current lock recorded: exactly `$2` of
# them, each building `--out-dir` every one of `$3…`.
built() {
  local what="$1" want="$2" calls n out
  shift 2
  calls="$(calls_of wasm-pack)"
  n="$(printf '%s' "$calls" | grep -c . || true)"
  if [ "$n" -eq "$want" ]; then
    ok "$what: a current Cargo.lock reaches wasm-pack ($n call(s))"
  else
    fail "$what: expected $want wasm-pack call(s) with a current Cargo.lock, got $n"
    printf '    | %s\n' "$calls" >&2
    sed 's/^/    | /' "$TMP/stderr" >&2
  fi
  for out in "$@"; do
    if printf '%s\n' "$calls" | grep -q -- "--out-dir $out\( \|$\)"; then
      ok "$what: builds $out"
    else
      fail "$what: no wasm-pack call builds $out"
    fi
  done
  local crate elsewhere
  crate="$(cd "$ROOT/crates/impresspress-web" && pwd -P)"
  elsewhere="$(grep -vxF -- "$crate" "$CALLS/wasm-pack.cwd" 2>/dev/null || true)"
  if [ -z "$elsewhere" ]; then
    ok "$what: wasm-pack runs from the crate directory"
  else
    fail "$what: wasm-pack ran outside $crate:"
    printf '    | %s\n' "$elsewhere" >&2
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
CURRENT="$(manifest_json "$PIN_VERSION" "$PIN_SHA")"
fake_tree "$CURRENT"
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
built "build.sh" 2 pkg-dev pkg-site

# The same current tree with a stale lock: refused by build.sh's own check,
# ahead of its first wasm-pack step, and the lock is untouched.
fake_tree "$CURRENT"
stale_lock
run_script "$SANDBOX/build.sh"
lock_refused "build.sh" check

# Handed both pkg dirs, build.sh compiles nothing and runs no wasm-pack, but
# still refuses a stale lock, before the CLI.
fake_pkg() {
  mkdir -p "$1/snippets"
  : >"$1/impresspress_web.js"
  : >"$1/impresspress_web_bg.wasm"
}
fake_tree "$CURRENT"
fake_pkg "$TMP/pkg-dev-given"
fake_pkg "$TMP/pkg-site-given"
stale_lock
run_script "$SANDBOX/build.sh" --pkg-dir "$TMP/pkg-dev-given" --site-pkg-dir "$TMP/pkg-site-given"
lock_refused "build.sh --pkg-dir --site-pkg-dir" check

# measure.sh builds each of its three pkg dirs that is missing — all three
# in the temp tree — and then stops, finding no wasm the fake produced.
fake_tree none
run_script "$SANDBOX/measure.sh"
built "measure.sh" 3 pkg pkg-site pkg-dev

fake_tree none
stale_lock
run_script "$SANDBOX/measure.sh"
lock_refused "measure.sh"

# The justfile's recipe lines that build the wasm, run the way `just` runs
# them: each line on its own, through `sh -c`, from the justfile's
# directory (here, the temp tree's root). Every line that names wasm-pack is run, so a line calling
# wasm-pack directly goes red in the stale case.
just_lines=()
while IFS= read -r line; do
  line="${line#"${line%%[![:space:]]*}"}"
  case "$line" in '#'* | '') continue ;; esac
  just_lines+=("$line")
done < <(grep -E '^[[:space:]]+.*wasm-pack' "$REPO/justfile" || true)
if [ "${#just_lines[@]}" -eq 0 ]; then
  fail "justfile: no recipe line runs wasm-pack — update this test"
fi
i=0
for line in "${just_lines[@]}"; do
  i=$((i + 1))
  fake_tree none
  STATUS=0
  (cd "$ROOT" && PATH="$FAKE_BIN:$PATH" sh -c "$line") >"$TMP/stdout" 2>"$TMP/stderr" || STATUS=$?
  built "justfile wasm-pack line $i" 1 pkg

  fake_tree none
  stale_lock
  STATUS=0
  (cd "$ROOT" && PATH="$FAKE_BIN:$PATH" sh -c "$line") >"$TMP/stdout" 2>"$TMP/stderr" || STATUS=$?
  lock_refused "justfile wasm-pack line $i"
done

# The wrapper itself, for what the entry points cannot show: a lock that is
# missing, and a cargo failure that is not about the lock at all. Neither may
# reach wasm-pack, and only a stale lock may be called out of date.
fake_tree none
rm "$LOCK"
run_script "$ROOT/scripts/wasm-pack-locked.sh" "$ROOT/crates/impresspress-web" build
if [ "$STATUS" -ne 0 ] && [ ! -e "$CALLS/wasm-pack" ] && [ ! -e "$LOCK" ] &&
  stderr_has "check-cargo-lock.sh: there is no Cargo.lock"; then
  ok "wasm-pack-locked.sh: a missing Cargo.lock is refused, named as missing, not created"
else
  fail "wasm-pack-locked.sh: a missing Cargo.lock was not refused as missing (exit $STATUS)"
  sed 's/^/    | /' "$TMP/stderr" >&2
fi

fake_tree none
cp "$LOCK" "$TMP/Cargo.lock.before"
printf 'not toml =\n' >>"$ROOT/crates/impresspress-web/Cargo.toml"
run_script "$ROOT/scripts/wasm-pack-locked.sh" "$ROOT/crates/impresspress-web" build
if [ "$STATUS" -ne 0 ] && [ ! -e "$CALLS/wasm-pack" ] && cmp -s "$LOCK" "$TMP/Cargo.lock.before" &&
  stderr_has "check-cargo-lock.sh: \`cargo metadata --locked\` failed" &&
  ! stderr_has "out of date"; then
  ok "wasm-pack-locked.sh: a broken manifest is refused as a cargo failure, not a stale lock"
else
  fail "wasm-pack-locked.sh: a broken manifest was not reported as a cargo failure (exit $STATUS)"
  sed 's/^/    | /' "$TMP/stderr" >&2
fi

if [ "$failures" -ne 0 ]; then
  printf '\ntest-build-guards.sh: %s failure(s)\n' "$failures" >&2
  exit 1
fi
printf '\ntest-build-guards.sh: all cases pass\n'
