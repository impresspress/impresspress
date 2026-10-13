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
#   * the bundle must be BUILT with `[sandbox] mode = "workspace"` (see
#     `impresspress.toml`), which is what renders
#     `const SANDBOX = 'workspace';` into `sw.js` and puts `/seed/` on the
#     service worker's bypass list.
# Building either half without the other is refused by `impresspress build`,
# which reads the wasm's `impresspress.sandbox_mode` section and names both
# modes; a bundle assembled around it some other way has a service worker
# that stops before `initialize()` (it compares `const SANDBOX` with the
# wasm's `compiled_sandbox_mode()`, fatal at stage `build`, nothing erased).
# So both are done here rather than left to the caller.
#
# The same holds for the EXPORT SHELL the sandbox carries — the static files
# for the site its export hands the user, built as a site rather than copied
# from the sandbox: the wasm is compiled with `--features
# browser-site-runtime` alone (`pkg-site/`, no workspace compiled in), and the
# shell is bundled from `export-shell/impresspress.toml` (`[sandbox] mode =
# "site"`) into `export-shell/dist/`, which the sandbox's own configuration
# names as `[sandbox] export_shell`. Before either bundle is assembled,
# `check-runtime-markers.mjs` checks the two compiled wasms against each other:
# no workspace endpoint, tool or compiler path in the site runtime, all of them
# in the sandbox's. After, the shell the sandbox shipped is compared byte for
# byte with the site wasm this run compiled.
#
# Usage:
#   examples/dev-sandbox/build.sh                  # build dist/ from seeds/blank
#   examples/dev-sandbox/build.sh --seed NAME       # build dist/ from seeds/NAME
#   examples/dev-sandbox/build.sh --seed NAME --out ../dist-NAME  # move the bundle there
#   examples/dev-sandbox/build.sh --pkg-dir DIR     # assemble from DIR instead of compiling the wasm
#   examples/dev-sandbox/build.sh --site-pkg-dir DIR  # the export shell's wasm from DIR, likewise
#   examples/dev-sandbox/build.sh --check           # verify every seed and the compiler tree
#
# A build needs `compiler/dist/` in place for `compiler/PIN.json`, and this
# script never builds it: it refuses, naming the command that does. Put it in
# place with `compiler/fetch-dist.sh` (the published tree for the pin), or —
# for a pin with no published tree yet — `compiler/build-compiler.sh`. See
# step 0 below.
#
# A relative `--out` is relative to the directory the script is run from; it
# must be outside `examples/dev-sandbox/` and either not exist, be empty, or be
# a bundle this script made.
#
# `--pkg-dir DIR` skips step 1 below: DIR is a wasm-pack output of
# `impresspress-web` built with `--features browser-devtools` — `pkg-dev/` as
# an earlier run of this script left it, or a copy of it — and the bundle is
# assembled from that. It is how a second bundle with a DIFFERENT runtime is
# built without compiling one twice: `sw-update.spec.ts` deploys one bundle
# over another, and the second is assembled from a restamped copy of
# `pkg-dev/` (`crates/impresspress-web/tests/e2e/fixtures/next-runtime-pkg.mjs`).
# Everything after step 1 is the same recipe either way.
#
# `--site-pkg-dir DIR` does the same for step 1b: DIR is a wasm-pack output of
# `impresspress-web` built with `--features browser-site-runtime` — `pkg-site/`
# as an earlier run left it — and the export shell is assembled from that.
# The two are independent: the next deployment `sw-update.spec.ts` deploys
# needs another sandbox runtime, not another site runtime, so CI passes both.
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
# and, when the seed has them, its `sandbox.json` (which names the title its
# boot page is built with — required, and no two seeds may share one), its
# `guide.md` and the
# `llms.txt` generated from `seeds/llms-preamble.md` and that guide, and that
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
  log "verifying seeds/*/manifest.json against seeds/*/site/**, sandbox.json, guide.md and the generated llms.txt, and vendor.json pins"
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
  rm -rf "$HERE/seed" "$HERE/boot-title.txt"
  mkdir -p "$HERE/seed"
  cp "$src/manifest.json" "$HERE/seed/manifest.json"
  cp -R "$src/site" "$HERE/seed/site"
  # The guide rides the bundle when the seed carries a sandbox block; the
  # manifest names it, so a seed with one and no file fails the check above.
  if [ -f "$src/guide.md" ]; then cp "$src/guide.md" "$HERE/seed/guide.md"; fi
  # llms.txt is generated, not copied: seedlib builds it from
  # seeds/llms-preamble.md and the seed's guide, exactly as it hashed it for
  # the manifest. It is staged at seed/llms.txt, which the bundle then carries
  # twice (impresspress.toml): at /seed/llms.txt, where the importer fetches
  # and verifies it like the guide, and at /llms.txt, where the static host
  # answers a reader that has no service worker. A seed with no sandbox block
  # has none — and impresspress.toml's overlay of it then fails the build,
  # which is right: this deployment is the sandbox.
  #
  # The boot page's title is staged the same way and for the same reason: it
  # is the seed's (sandbox.json), and impresspress.toml's `[app] title_file`
  # names the one file it is staged to. Beside seed/, not inside it — seed/ is
  # overlaid onto the bundle wholesale, and the title is the boot page's, not
  # a file anything fetches.
  python3 -B - "$HERE/seeds" "$SEED" "$HERE/seed/llms.txt" "$HERE/boot-title.txt" <<'STAGESANDBOX'
import pathlib, sys
sys.path.insert(0, sys.argv[1])
import seedlib
try:
    seed = seedlib.seed_dir(sys.argv[2])
    text = seedlib.staged_llms(seed)
    title = seedlib.boot_title(seed)
except seedlib.SeedError as e:
    raise SystemExit(f"build.sh: {e}")
if text is not None:
    pathlib.Path(sys.argv[3]).write_bytes(text)
if title is not None:
    pathlib.Path(sys.argv[4]).write_text(title + "\n", encoding="utf-8")
STAGESANDBOX
}

# The browser toolchain (`compiler/`) is 365 MiB of composed wasm and takes
# ~55 minutes to build from cold — and its `wasm-opt` pass peaked at 12.6 GB
# RSS when it was measured, so it CANNOT run on a 7 GB CI runner: cache
# `compiler/dist/` on `compiler/PIN.json` (which fully determines it) or use a
# large runner. It is therefore never built here: when it is missing, or
# `PIN.json` has moved since the tree in `compiler/dist/` was produced, the
# build refuses and names `compiler/fetch-dist.sh` (step 0 below).
# Everything about that tree — the rubrc commit, the sysroot, the tools —
# comes from that one file, so comparing it against the built manifest is the
# whole staleness test.
#
# Exits 0 when the tree is current; otherwise prints the one reason it is not
# to stdout and exits 1.
compiler_is_current() {
  python3 - "$HERE" <<'COMPILERPIN'
import json, pathlib, sys

def stale(reason):
    print(reason)
    raise SystemExit(1)

compiler = pathlib.Path(sys.argv[1], "compiler")
manifest = compiler / "dist" / "manifest.json"
if not manifest.is_file():
    stale("dist/manifest.json is missing")
pin = json.loads((compiler / "PIN.json").read_text())
try:
    built = json.loads(manifest.read_text())
except ValueError as e:
    stale(f"dist/manifest.json is not JSON ({e})")
if not isinstance(built, dict) or not isinstance(built.get("rubrc", {}), dict):
    stale("dist/manifest.json is not a manifest object (no {..., \"rubrc\": {...}})")
if built.get("version") != pin["version"]:
    stale(f"built for compiler {built.get('version')}; PIN.json pins {pin['version']}")
built_sha = built.get("rubrc", {}).get("sha")
if built_sha != pin["rubrc"]["sha"]:
    stale(f"built from rubrc {str(built_sha)[:8]}; PIN.json pins {pin['rubrc']['sha'][:8]}")
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
PKG_DIR=""
SITE_PKG_DIR=""
CHECK_ONLY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --check) CHECK_ONLY=1 ;;
    --seed) SEED="${2:-}"; [ -n "$SEED" ] || { echo "build.sh: --seed needs a name" >&2; exit 1; }; shift ;;
    --out) OUT="${2:-}"; [ -n "$OUT" ] || { echo "build.sh: --out needs a directory" >&2; exit 1; }; shift ;;
    --pkg-dir) PKG_DIR="${2:-}"; [ -n "$PKG_DIR" ] || { echo "build.sh: --pkg-dir needs a directory" >&2; exit 1; }; shift ;;
    --site-pkg-dir) SITE_PKG_DIR="${2:-}"; [ -n "$SITE_PKG_DIR" ] || { echo "build.sh: --site-pkg-dir needs a directory" >&2; exit 1; }; shift ;;
    *) echo "build.sh: unknown argument '$1' (usage: build.sh [--check] [--seed NAME] [--out DIR] [--pkg-dir DIR] [--site-pkg-dir DIR])" >&2; exit 1 ;;
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

# `--pkg-dir` and `--site-pkg-dir`, like `--out`, are the caller's paths:
# resolved here, before the `cd` further down, and checked before anything is
# built. `resolve_pkg_dir FLAG DIR` prints the resolved path.
resolve_pkg_dir() {
  local flag="$1" dir
  dir="$(python3 -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$2")"
  for file in impresspress_web.js impresspress_web_bg.wasm; do
    [ -f "$dir/$file" ] || {
      echo "build.sh: $flag '$dir' has no $file — it must be a wasm-pack output of impresspress-web" >&2
      exit 1
    }
  done
  [ -d "$dir/snippets" ] || {
    echo "build.sh: $flag '$dir' has no snippets/ — the JS glue cannot resolve its imports" >&2
    exit 1
  }
  printf '%s\n' "$dir"
}
if [ -n "$PKG_DIR" ]; then PKG_DIR="$(resolve_pkg_dir --pkg-dir "$PKG_DIR")"; fi
if [ -n "$SITE_PKG_DIR" ]; then SITE_PKG_DIR="$(resolve_pkg_dir --site-pkg-dir "$SITE_PKG_DIR")"; fi

if [ "$CHECK_ONLY" = 1 ]; then
  check_seed
  check_compiler
  exit 0
fi

check_seed
stage_seed

# 0. The browser toolchain, overlaid onto the bundle at
#    `/__impresspress_dev/compiler/` (see `impresspress.toml`). It must already
#    be in place for `compiler/PIN.json`: this script never builds it, because
#    building it is the ~55 minute, 12.6 GB composition described above. The
#    refusal comes before `check_compiler`, before any wasm-pack run and
#    before the CLI runs. It comes after `check_seed` and `stage_seed` on
#    purpose: a stale seed manifest is the cheaper failure to report first,
#    and staging only writes `seed/` and `boot-title.txt` in this directory,
#    which every build rewrites.
if reason="$(compiler_is_current)"; then
  log "compiler/dist is current for compiler/PIN.json"
else
  {
    echo "build.sh: compiler/dist is not the toolchain compiler/PIN.json pins — $reason."
    echo "  Fetch the published, verified tree for this pin (what the deploy runs):"
    echo "    $HERE/compiler/fetch-dist.sh"
    echo "  A pin with no published tree yet (a pin bump) has to be built — ~55 min, 12.6 GB RSS:"
    echo "    $HERE/compiler/build-compiler.sh   (then pack-dist.sh; compiler/README.md \"Publishing the compiler\")"
    echo "  then re-run this script. build.sh never builds the compiler itself."
  } >&2
  exit 1
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
#    `--pkg-dir` names one that already exists, and nothing is compiled.
#    Both builds go through `scripts/wasm-pack-locked.sh`, which refuses a
#    `Cargo.lock` that is out of date before wasm-pack can rewrite it.
if [ -n "$PKG_DIR" ]; then
  log "using the wasm-pack output at $PKG_DIR"
else
  PKG_DIR="$REPO/crates/impresspress-web/pkg-dev"
  log "wasm-pack build --features browser-devtools -> $PKG_DIR"
  "$REPO/scripts/wasm-pack-locked.sh" "$REPO/crates/impresspress-web" build --target web --release --out-dir pkg-dev -- --features browser-devtools
fi

# 1b. The site runtime — the sandbox's runtime half alone, with no workspace
#     compiled in — for the export shell, into its own `pkg-site/`.
#     `--site-pkg-dir` names one that already exists, and nothing is compiled.
if [ -n "$SITE_PKG_DIR" ]; then
  log "using the site-runtime wasm-pack output at $SITE_PKG_DIR"
else
  SITE_PKG_DIR="$REPO/crates/impresspress-web/pkg-site"
  log "wasm-pack build --features browser-site-runtime -> $SITE_PKG_DIR"
  "$REPO/scripts/wasm-pack-locked.sh" "$REPO/crates/impresspress-web" build --target web --release --out-dir pkg-site -- --features browser-site-runtime
fi

# 1c. The guarantee the feature split is for, checked on the two artifacts:
#     no workspace marker in the site runtime, every one in the sandbox's.
log "checking the site runtime for workspace code (check-runtime-markers.mjs)"
#     Its report goes to stderr with the rest of the log: stdout is the
#     bundle path alone (callers keep its last line).
node "$HERE/check-runtime-markers.mjs" "$SITE_PKG_DIR/impresspress_web_bg.wasm" "$PKG_DIR/impresspress_web_bg.wasm" >&2

# 2b, 2. The sealed × web flow, twice: the export shell, then the sandbox
#    that carries it. Neither `examples/dev-sandbox` nor `export-shell/` has a
#    `Cargo.toml`, so the CLI's mode detection (`mode.rs`) takes the sealed
#    path — which honours `IMPRESSPRESS_WEB_PKG_DIR` rather than rebuilding
#    impresspress-web itself, and copies the wasm from it unchanged.
[ -n "$IMPRESSPRESS_BIN" ] || {
  echo "build.sh: no impresspress CLI on PATH (or at \$IMPRESSPRESS='${IMPRESSPRESS:-}')." >&2
  echo "  cargo install --path '$REPO/crates/impresspress' --locked --root '$REPO/out'" >&2
  echo "  IMPRESSPRESS='$REPO/out/bin/impresspress' examples/dev-sandbox/build.sh" >&2
  exit 1
}

# 2b. The export shell, into `export-shell/dist/` — the directory the
#     sandbox's `[sandbox] export_shell` names.
cd "$HERE/export-shell"
log "assembling the export shell with $IMPRESSPRESS_BIN"
IMPRESSPRESS_WEB_PKG_DIR="$SITE_PKG_DIR" \
  "$IMPRESSPRESS_BIN" build --target web --release
SHELL_DIST="$HERE/export-shell/dist"
# A site's worker: the mode the site runtime is compiled for, and nothing of
# the sandbox's own trees (no compiler bypass, no export shell of its own).
grep -q "const SANDBOX = 'site';" "$SHELL_DIST/sw.js" || {
  echo "build.sh: $SHELL_DIST/sw.js does not declare \"const SANDBOX = 'site';\" — is [sandbox] mode = \"site\" set in export-shell/impresspress.toml?" >&2
  exit 1
}
if grep -q "/__impresspress_dev/" "$SHELL_DIST/sw.js"; then
  echo "build.sh: $SHELL_DIST/sw.js names /__impresspress_dev/ — the export shell must not carry the sandbox's trees" >&2
  exit 1
fi
# Its manifest lists its files, states the rules its worker bypasses and
# gives each file's digest — the three things the export takes from it (the
# CLI checks the digests against the files when it lays the shell down).
python3 - "$SHELL_DIST/asset-manifest.json" <<'SHELLMANIFEST' || exit 1
import json, sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
if not manifest.get("files"):
    raise SystemExit(f"build.sh: {sys.argv[1]} lists no files")
if not isinstance(manifest.get("bypass"), dict):
    raise SystemExit(f"build.sh: {sys.argv[1]} states no bypass rules")
if not isinstance(manifest.get("sha256"), dict):
    raise SystemExit(f"build.sh: {sys.argv[1]} gives no file digests")
SHELLMANIFEST

# 2. The sandbox, with the shell laid down at /__impresspress_dev/export-shell/.
cd "$HERE"
log "assembling the bundle with $IMPRESSPRESS_BIN"
IMPRESSPRESS_WEB_PKG_DIR="$PKG_DIR" \
  "$IMPRESSPRESS_BIN" build --target web --release

DIST="$HERE/dist"
[ -f "$DIST/sw.js" ] || { echo "build.sh: $DIST/sw.js was not produced" >&2; exit 1; }
# The one build-time constant `sw.js` renders `[sandbox] mode` into
# (`impresspress-bundle`'s `sw.js.tmpl`): the worker's check that holds the
# runtime to that mode before `initialize()` (`compiled_sandbox_mode()`; the
# CLI has already refused a runtime of another mode) and the isolation-header
# passthrough both read it, so this single line is the whole of "this bundle
# is the workspace".
grep -q "const SANDBOX = 'workspace';" "$DIST/sw.js" || {
  echo "build.sh: $DIST/sw.js does not declare \"const SANDBOX = 'workspace';\" — is [sandbox] mode = \"workspace\" set?" >&2
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
# The two things a reader with no service worker gets from this deployment:
# the sandbox's llms.txt at the root, and a boot page that says what this is.
cmp -s "$DIST/llms.txt" "$DIST/seed/llms.txt" || {
  echo "build.sh: $DIST/llms.txt is missing or is not the seed's llms.txt — check impresspress.toml's [[assets.overlay]]" >&2
  exit 1
}
grep -q 'href="/llms.txt"' "$DIST/index.html" || {
  echo "build.sh: $DIST/index.html does not carry the boot notice — check [app] boot_notice in impresspress.toml," >&2
  echo "  or an impresspress CLI older than it: $IMPRESSPRESS_BIN" >&2
  exit 1
}
# …and that page is headed with the title of the seed that was built, not
# another seed's and not a title from anywhere else.
python3 -B - "$HERE/seeds" "$SEED" "$DIST/index.html" <<'BOOTTITLE' || exit 1
import html, pathlib, re, sys
sys.path.insert(0, sys.argv[1])
import seedlib
title = seedlib.boot_title(seedlib.seed_dir(sys.argv[2]))
page = pathlib.Path(sys.argv[3]).read_text(encoding="utf-8")
shown = [html.unescape(t) for t in re.findall(r"<title>(.*?)</title>", page, re.S)]
shown += [html.unescape(t) for t in re.findall(r"<span data-app-title>(.*?)</span>", page, re.S)]
if not shown or any(t != title for t in shown):
    raise SystemExit(
        f"build.sh: {sys.argv[3]} is titled {shown!r}, not the {sys.argv[2]} seed's {title!r} — "
        f"check [app] title_file in impresspress.toml, or an impresspress CLI older than it"
    )
BOOTTITLE
[ -f "$DIST/__impresspress_dev/compiler/manifest.json" ] || {
  echo "build.sh: $DIST/__impresspress_dev/compiler/manifest.json was not overlaid — check impresspress.toml's [[assets.overlay]]" >&2
  exit 1
}
# The export shell: the sandbox's worker hands its runtime the shell's root,
# and the shell laid down there runs the site wasm THIS run compiled (or was
# handed with --site-pkg-dir) — byte for byte, not merely a shell of the same
# name from an earlier build. The sealed flow copies the wasm-pack output
# unchanged and only renames it, so the shell's own manifest says which file
# it is.
grep -q 'const EXPORT_SHELL = "/__impresspress_dev/export-shell/";' "$DIST/sw.js" || {
  echo "build.sh: $DIST/sw.js does not hand its runtime the export shell — is [sandbox] export_shell set in impresspress.toml?" >&2
  exit 1
}
SHIPPED_SHELL="$DIST/__impresspress_dev/export-shell"
SHELL_WASM="$(python3 - "$SHIPPED_SHELL/asset-manifest.json" <<'SHELLWASM'
import json, sys
assets = json.load(open(sys.argv[1], encoding="utf-8"))["assets"]
print(assets["impresspress_web_bg.wasm"].lstrip("/"))
SHELLWASM
)" || { echo "build.sh: $SHIPPED_SHELL/asset-manifest.json does not name the shell's wasm" >&2; exit 1; }
cmp "$SHIPPED_SHELL/$SHELL_WASM" "$SITE_PKG_DIR/impresspress_web_bg.wasm" || {
  echo "build.sh: the export shell's $SHELL_WASM is not $SITE_PKG_DIR/impresspress_web_bg.wasm — the sandbox shipped a shell built from another wasm" >&2
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
