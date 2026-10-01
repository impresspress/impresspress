#!/usr/bin/env bash
#
# Build the dev-sandbox bundle and print the directory to serve.
#
# This is the one bundle recipe dev.impresspress.org, CI's `e2e-dev-sandbox`
# job and the local e2e run all share (Plan 1 shipped a scratch copy under
# `crates/impresspress-web/tests/e2e/fixtures/`; this replaced it — see
# `crates/impresspress-web/tests/e2e/dev-foundations.spec.ts`, which reads the
# seed's manifest, `seeds/blank/manifest.json` in this directory, instead of
# pinning a hash).
#
# Two halves have to agree for the sandbox to exist at all (design §13):
#   * the wasm must be COMPILED with `--features browser-devtools`, and
#   * the bundle must be BUILT with `[dev] enabled = true` (see
#     `impresspress.toml`), which is what renders `const DEV_ENABLED = true;`
#     into `sw.js` and puts `/seed/` on the service worker's bypass list.
# Building either half without the other is a bundle with no `/b/dev`, so both
# are done here rather than left to the caller.
#
# Usage:
#   examples/dev-sandbox/build.sh                  # build dist/ from seeds/blank
#   examples/dev-sandbox/build.sh --seed NAME       # build dist/ from seeds/NAME
#   examples/dev-sandbox/build.sh --seed NAME --out ../dist-NAME  # move the bundle there
#   examples/dev-sandbox/build.sh --check           # verify every seed and the compiler tree
#
# A relative `--out` is relative to the directory the script is run from; it
# must be outside `examples/dev-sandbox/` and either not exist, be empty, or be
# a bundle this script made.
#
# `IMPRESSPRESS=/path/to/impresspress` overrides which CLI binary assembles
# the bundle. Default is whatever is on `PATH`, which is the trap this
# override exists for: a stale `~/.cargo/bin/impresspress` from an older
# checkout silently builds a bundle without the recursive-directory overlay
# (`cli/helpers/overlays.rs`), so `dist/seed/` never appears and the sanity
# check below fails with no hint that the CLI is the problem. Build a current
# one with `cargo install --path crates/impresspress --locked` (add
# `--root ./out` to keep it out of `~/.cargo/bin`) and point this at it.
#
# `--check` verifies every `seeds/<name>/manifest.json` against its `site/**`
# and, when the seed has them, its `sandbox.json` and `guide.md`, and that
# every file its `vendor.json` pins carries the pinned sha256
# (seeds/check-seeds.py) — and, when `compiler/dist/` has been
# built, that its files match `compiler/dist/manifest.json` and none of them
# is over Cloudflare's asset limit — exiting non-zero on drift, WITHOUT
# building anything — this is what CI runs to catch a seed file edited
# without regenerating the manifest. A plain build runs both of those checks
# too — the seed's before it builds anything, so a stale manifest fails fast
# rather than shipping a bundle `seed::import` will refuse at runtime, and the
# compiler's once the toolchain is in place, over whatever `dist/` the build
# is about to overlay.
#
# The whole wasm-pack output is bundled, not just the wasm + JS pair: the JS
# glue imports from `snippets/`, and a pkg dir missing that tree cannot load
# its own module (`IMPRESSPRESS_WEB_PKG_DIR`, resolved by
# `crates/impresspress/src/cli/helpers/wasm.rs`).
#
# The last line of stdout is the absolute path of the finished bundle —
# `dist/`, or the `--out` directory; CI captures it with `tail -1`.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$HERE/../.." && pwd)"

log() { printf '==> %s\n' "$*" >&2; }

# Resolved at this point, while the caller's cwd is still current: the build
# below runs
# from `$HERE`, so a relative override (`IMPRESSPRESS=./out/bin/impresspress`,
# typed from the repo root) would otherwise be looked up against
# `examples/dev-sandbox/` and not be found. `|| true` because `set -e` is on
# and "not found" is reported by the check further down, with instructions.
IMPRESSPRESS_BIN="$(command -v "${IMPRESSPRESS:-impresspress}" 2>/dev/null || true)"
case "$IMPRESSPRESS_BIN" in
  /* | '') ;;
  *) IMPRESSPRESS_BIN="$(cd "$(dirname "$IMPRESSPRESS_BIN")" && pwd)/$(basename "$IMPRESSPRESS_BIN")" ;;
esac

# Every seed under seeds/, not only the one being built: a manifest that has
# drifted from its files is what `seed::import` refuses at boot, and the
# check is cheap. The rules live in seeds/check-seeds.py.
check_seed() {
  log "verifying seeds/*/manifest.json against seeds/*/site/**, sandbox.json and guide.md, and vendor.json pins"
  python3 "$HERE/seeds/check-seeds.py"
}

# Copy the chosen seed into seed/, the directory impresspress.toml overlays
# onto dist/seed/. `[[assets.overlay]]` takes no parameters and the CLI has
# no environment substitution for it, so choosing a seed means staging it
# under the one name the overlay knows. Only what the manifest describes is
# staged: seeds/<name>/ may carry source files (a vendor pin, say) that are
# not bundle content.
stage_seed() {
  local src="$HERE/seeds/$SEED"
  if [ ! -f "$src/manifest.json" ]; then
    local available
    available="$(python3 -B -c 'import sys; sys.path.insert(0, sys.argv[1]); import seedlib; print(" ".join(p.name for p in seedlib.seed_dirs() if (p / "manifest.json").is_file()))' "$HERE/seeds")"
    echo "build.sh: no seed named '$SEED' under $HERE/seeds/ — available: ${available:-(none)}" >&2
    exit 1
  fi
  log "staging seeds/$SEED into seed/"
  rm -rf "$HERE/seed"
  mkdir -p "$HERE/seed"
  cp "$src/manifest.json" "$HERE/seed/manifest.json"
  cp -R "$src/site" "$HERE/seed/site"
  # The guide rides the bundle when the seed carries a sandbox block; the
  # manifest names it, so a seed with one and no file fails the check above.
  if [ -f "$src/guide.md" ]; then cp "$src/guide.md" "$HERE/seed/guide.md"; fi
}

# The browser toolchain (`compiler/`) is 365 MiB of composed wasm and takes
# ~55 minutes to build from cold — and its `wasm-opt` pass peaked at 12.6 GB
# RSS when it was measured, so it CANNOT run on a 7 GB CI runner: cache
# `compiler/dist/` on `compiler/PIN.json` (which fully determines it) or use a
# large runner. It is therefore built only when it is missing or when
# `PIN.json` has moved since the tree in `compiler/dist/` was produced.
# Everything about that tree — the rubrc commit, the sysroot, the tools —
# comes from that one file, so comparing it against the built manifest is the
# whole staleness test.
compiler_is_current() {
  python3 - "$HERE" <<'COMPILERPIN'
import json, pathlib, sys

compiler = pathlib.Path(sys.argv[1], "compiler")
manifest = compiler / "dist" / "manifest.json"
if not manifest.is_file():
    raise SystemExit(1)
pin = json.loads((compiler / "PIN.json").read_text())
built = json.loads(manifest.read_text())
if built.get("version") != pin["version"]:
    raise SystemExit(1)
if built.get("rubrc", {}).get("sha") != pin["rubrc"]["sha"]:
    raise SystemExit(1)
COMPILERPIN
}

# The compiler is only checked when it has been built: a tree that has never
# run `build-compiler.sh` is a normal state for anyone working on the seed or
# the wasm, and `--check` is meant to be cheap.
#
# The environment reaches the verifier untouched, which is how
# `IMPRESSPRESS_COMPILER_ALLOW_FAST=1` gets through: a CI job that only wants
# to know whether the compiler still works may build it with
# `build-compiler.sh --fast` and set that variable. The deploy workflow must
# never set it — a `--fast` component skips `wasm-opt` entirely.
check_compiler() {
  if [ ! -d "$HERE/compiler/dist" ]; then
    log "compiler/dist is not built — nothing to check"
    return 0
  fi
  log "verifying compiler/dist against its manifest and the 24 MiB asset limit"
  node "$HERE/compiler/scripts/verify-compiler-assets.mjs"
}

SEED="blank"
OUT=""
CHECK_ONLY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --check) CHECK_ONLY=1 ;;
    --seed) SEED="${2:-}"; [ -n "$SEED" ] || { echo "build.sh: --seed needs a name" >&2; exit 1; }; shift ;;
    --out) OUT="${2:-}"; [ -n "$OUT" ] || { echo "build.sh: --out needs a directory" >&2; exit 1; }; shift ;;
    *) echo "build.sh: unknown argument '$1' (usage: build.sh [--check] [--seed NAME] [--out DIR])" >&2; exit 1 ;;
  esac
  shift
done

# The seed name is held to the one rule check-seeds.py finds seeds by
# (seedlib.SEED_NAME, the runtime's block-name rule): a directory that rule
# skips is never verified, so it must never be staged either. Checked here,
# before anything else runs, and on `--check` too.
if ! reason="$(python3 -B - "$HERE/seeds" "$SEED" <<'SEEDNAME'
import sys
sys.path.insert(0, sys.argv[1])
import seedlib
try:
    seedlib.seed_dir(sys.argv[2])
except seedlib.SeedError as e:
    print(e)
    raise SystemExit(1)
SEEDNAME
)"; then
  echo "build.sh: --seed takes a seed name, not a path — $reason" >&2
  exit 1
fi

# `--out` is resolved against the CALLER's directory and checked before any
# build, because it is `rm -rf`ed: it must not be the default dist/, this
# directory, anything inside it (seeds/, compiler/, the staged seed/), or an
# ancestor of it — `/` included. Symlinks are resolved on both sides (`pwd -P`
# for this directory, `realpath` for `--out`), so a link into this directory
# is caught too. python3 rather than `realpath -m` — it is already a hard
# dependency of this script, and macOS's realpath has no -m.
if [ -n "$OUT" ]; then
  OUT="$(python3 -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$OUT")"
  case "$OUT" in
    "$HERE"/dist|"$HERE"|"$HERE"/*)
      echo "build.sh: --out must be outside $HERE (got '$OUT')" >&2
      exit 1 ;;
  esac
  case "$HERE/" in
    "${OUT%/}/"*)
      echo "build.sh: --out '$OUT' contains this directory" >&2
      exit 1 ;;
  esac
  # An existing --out is replaced only when it is empty or a bundle this
  # script produced (sw.js beside seed/manifest.json). Anything else — a
  # checkout, a home directory, a typo — is refused rather than rm -rf'ed.
  if [ -e "$OUT" ]; then
    if [ ! -d "$OUT" ]; then
      echo "build.sh: --out '$OUT' exists and is not a directory" >&2
      exit 1
    fi
    if [ -n "$(ls -A "$OUT")" ] && ! { [ -f "$OUT/sw.js" ] && [ -f "$OUT/seed/manifest.json" ]; }; then
      echo "build.sh: --out '$OUT' exists and is not a bundle this script made (no sw.js + seed/manifest.json) — remove it yourself or choose another directory" >&2
      exit 1
    fi
  fi
fi

if [ "$CHECK_ONLY" = 1 ]; then
  check_seed
  check_compiler
  exit 0
fi

check_seed
stage_seed

# 0. The browser toolchain, overlaid onto the bundle at
#    `/__impresspress_dev/compiler/` (see `impresspress.toml`).
if compiler_is_current; then
  log "compiler/dist is current for compiler/PIN.json"
else
  log "compiler/build-compiler.sh (dist is missing or built from another pin)"
  "$HERE/compiler/build-compiler.sh"
fi
# `compiler_is_current` answers one question — was this tree built from the
# pin in the file? — off two manifest fields, and never looks at the bytes
# beside it or at `manifest.build`. A `--fast` component is therefore
# "current" forever: `IMPRESSPRESS_COMPILER_ALLOW_FAST=1` is needed for the
# run of `build-compiler.sh` that PRODUCES one, and not for any later build
# that picks it up. So the verifier runs here as well, over whatever `dist/`
# is about to be overlaid — without it a plain build would quietly assemble an
# unoptimized toolchain, and an edited or truncated file under `dist/` would
# ship unhashed. This is the check every other file in this directory says is
# what keeps a `--fast` tree out of a deploy.
check_compiler

# 1. The feature-on wasm. `--out-dir pkg-dev` keeps it away from `pkg/`, which
#    is the ordinary (feature-off) bundle every other consumer serves — a
#    tree that has just built the ordinary bundle must not be disturbed by
#    this script, and vice versa.
log "wasm-pack build --features browser-devtools -> $REPO/crates/impresspress-web/pkg-dev"
(cd "$REPO/crates/impresspress-web" && wasm-pack build --target web --release --out-dir pkg-dev -- --features browser-devtools)

# 2. The sealed × web flow. `examples/dev-sandbox` has an `impresspress.toml`
#    but no `Cargo.toml`, so the CLI's mode detection (`mode.rs`) takes the
#    sealed path — which honours `IMPRESSPRESS_WEB_PKG_DIR` rather than
#    rebuilding impresspress-web itself.
cd "$HERE"
[ -n "$IMPRESSPRESS_BIN" ] || {
  echo "build.sh: no impresspress CLI on PATH (or at \$IMPRESSPRESS='${IMPRESSPRESS:-}')." >&2
  echo "  cargo install --path '$REPO/crates/impresspress' --locked --root '$REPO/out'" >&2
  echo "  IMPRESSPRESS='$REPO/out/bin/impresspress' examples/dev-sandbox/build.sh" >&2
  exit 1
}
log "assembling the bundle with $IMPRESSPRESS_BIN"
IMPRESSPRESS_WEB_PKG_DIR="$REPO/crates/impresspress-web/pkg-dev" \
  "$IMPRESSPRESS_BIN" build --target web --release

DIST="$HERE/dist"
[ -f "$DIST/sw.js" ] || { echo "build.sh: $DIST/sw.js was not produced" >&2; exit 1; }
# The one build-time constant `sw.js` renders `[dev] enabled` into
# (`impresspress-bundle`'s `sw.js.tmpl`): `initialize({ dev: DEV_ENABLED })`
# and the isolation-header passthrough both read it, so this single line is
# the whole of "the sandbox is on in this bundle".
grep -q 'const DEV_ENABLED = true;' "$DIST/sw.js" || {
  echo "build.sh: $DIST/sw.js does not declare 'const DEV_ENABLED = true;' — is [dev] enabled set?" >&2
  exit 1
}
[ -d "$DIST/snippets" ] || {
  echo "build.sh: $DIST/snippets is missing; the JS glue cannot resolve its imports" >&2
  exit 1
}
[ -f "$DIST/seed/manifest.json" ] || {
  echo "build.sh: $DIST/seed/manifest.json was not overlaid — check impresspress.toml's [[assets.overlay]]," >&2
  echo "  or an impresspress CLI older than the recursive-directory overlay (cli/helpers/overlays.rs):" >&2
  echo "  $IMPRESSPRESS_BIN" >&2
  exit 1
}
[ -f "$DIST/__impresspress_dev/compiler/manifest.json" ] || {
  echo "build.sh: $DIST/__impresspress_dev/compiler/manifest.json was not overlaid — check impresspress.toml's [[assets.overlay]]" >&2
  exit 1
}

# `--out DIR` moves the finished bundle out of dist/, so a second seed can be
# built into dist/ afterwards (CI builds the bootstrap seed, moves it aside,
# then builds blank). A relative DIR is relative to the directory the script
# was run from, and was resolved and checked at parse time, before the build
# (see the argument handling above): a DIR that exists here is empty or a
# bundle this script made, so replacing it discards nothing else.
if [ -n "$OUT" ]; then
  mkdir -p "$(dirname "$OUT")"
  rm -rf "$OUT"
  mv "$DIST" "$OUT"
  DIST="$OUT"
fi
log "dist ready ($SEED seed): $(du -sh "$DIST" | cut -f1)"
echo "$DIST"
