#!/usr/bin/env bash
#
# Measure the dev-sandbox's two size regressions and print a Markdown table.
#
# The sandbox ships two wasm bundles that must both exist and diverge only by
# the `browser-devtools` feature (design §13, see build.sh's header): `pkg/`
# (the ordinary, feature-off bundle every other consumer serves) and
# `pkg-dev/` (the same crate compiled with
# `--features browser-devtools`, which is what dev.impresspress.org actually
# serves). The feature adds real weight (wasmi `inspect`/`probe`, the runtime
# rebuild path) — the point of this script is not that the delta is zero, but
# that it stays bounded, because `pkg-dev/` never ships in the ordinary
# (non-dev) bundle but a silent size creep there is still a signal the crate
# is dragging avoidable weight into a feature that everyone building the
# `browser-devtools` target pays for. It also measures
# `examples/dev-sandbox/compiler/dist/` — the in-browser Rust compiler, by
# far the biggest thing dev.impresspress.org ships — whenever that tree has
# been composed. `dist/` is gitignored and composed by
# `examples/dev-sandbox/compiler/build-compiler.sh` (restored from the
# `compiler-dist-*` cache in CI), so a fresh checkout that has not run it
# simply has no compiler section.
#
# Usage:
#   examples/dev-sandbox/measure.sh
#
# `PKG_DIR` / `PKG_DEV_DIR` override where the two bundles live (default
# `crates/impresspress-web/pkg` and `crates/impresspress-web/pkg-dev`, same
# as build.sh's `--out-dir`). A missing one is built with `wasm-pack build
# --target web --release` (`pkg-dev` additionally gets
# `-- --features browser-devtools`); an existing one is measured as-is and
# NOT rebuilt — wasm-pack's release profile already runs wasm-opt, so
# re-running this script never re-optimizes, it only re-measures.
#
# Thresholds (all overridable, all in the units named):
#   MEASURE_MAX_DEV_DELTA_KIB        default 1536  — dev gzip minus default
#                                                     gzip, in KiB
#     (the dev-only bundle carries the workspace page, the compiler adapter,
#     the wafer_guest crate's source (embedded once, for /b/dev/api/guest),
#     the templates and the reference; the guard is
#     against runaway growth, not this delta — Plan 2 measured +661 KiB
#     before Plan 3's additions)
#   MEASURE_MAX_COMPILER_TOTAL_MIB   default 80    — compiler/dist total, MiB
#   MEASURE_MAX_COMPILER_FILE_BYTES  default 25165824 (24 MiB) — any one
#                                                     compiler/dist asset
#
# The per-file default is not a round number picked here: it is the same
# constant `compiler/scripts/verify-compiler-assets.mjs` enforces as
# `MAX_ASSET_BYTES` and `prepare-vfs-asset.mjs` splits `vfs.core` on
# (`PART_BYTES`), i.e. the largest single object the deployment target
# accepts. The two agree by construction; a deliberate change belongs in
# both.
#
# The compiler thresholds apply to a `"build": "full"` manifest only. A
# `--fast` compose (`build-compiler.sh --fast`) skips wasm-opt, so its tree
# is legitimately larger and its sizes say nothing about what would ship —
# `verify-compiler-assets.mjs` refuses to deploy one at all. CI's
# `e2e-dev-sandbox` job composes `--fast` on a compiler-cache miss, so this
# is a state the usual caller really reaches: the sizes are still reported
# (they are the honest numbers for the tree on disk) and the two compiler
# thresholds are skipped, with a line saying so. The dev-delta threshold is
# unaffected — it measures `pkg/` vs `pkg-dev/`, which have nothing to do
# with the compiler compose.
#
# Exit status: non-zero if any threshold is breached, with a line naming
# which. The Markdown table is always printed to stdout first (even on a
# breach) so a caller redirecting stdout to $GITHUB_STEP_SUMMARY still gets
# the numbers.
#
# `examples/dev-sandbox/compiler/scripts/write-manifest.mjs` is the source of
# truth for manifest.json's shape; `compiler/README.md` documents it. This
# script reads four of its fields and parses them defensively:
#   { "build": "full" | "fast", "version": str, "total_bytes": int,
#     "assets": [{ "path": str, "bytes": int, "sha256": str }, ...] }
# "defensively" means: `total_bytes` is recomputed from `assets[].bytes` if
# it is missing or not an int, any asset missing a usable `bytes` field is
# skipped rather than crashing the script, and an absent or unrecognised
# `build` is treated as `full` — i.e. thresholds apply, because a manifest
# that cannot prove it was a fast compose gets gated.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
CRATE_DIR="$REPO/crates/impresspress-web"

log() { printf '==> %s\n' "$*" >&2; }
fail() { printf 'measure.sh: FAIL: %s\n' "$*" >&2; FAILED=1; }

FAILED=0

MEASURE_MAX_DEV_DELTA_KIB="${MEASURE_MAX_DEV_DELTA_KIB:-1536}"
MEASURE_MAX_COMPILER_TOTAL_MIB="${MEASURE_MAX_COMPILER_TOTAL_MIB:-80}"
MEASURE_MAX_COMPILER_FILE_BYTES="${MEASURE_MAX_COMPILER_FILE_BYTES:-25165824}"

# Resolve to an absolute path whether or not it exists yet (a not-yet-built
# --out-dir has no directory for `cd` to land in).
abspath() {
  local p="$1"
  if [ -d "$p" ]; then
    (cd "$p" && pwd)
  else
    local parent
    parent="$(cd "$(dirname "$p")" && pwd)"
    printf '%s/%s\n' "$parent" "$(basename "$p")"
  fi
}

PKG_DIR="$(abspath "${PKG_DIR:-$CRATE_DIR/pkg}")"
PKG_DEV_DIR="$(abspath "${PKG_DEV_DIR:-$CRATE_DIR/pkg-dev}")"

# wasm-pack's --out-dir is applied relative to the crate directory it runs
# from (its own --help calls this out), so an absolute override has to be
# turned back into one before it's passed on.
relative_to_crate() {
  python3 -c 'import os,sys; print(os.path.relpath(sys.argv[1], sys.argv[2]))' "$1" "$CRATE_DIR"
}

build_missing() {
  local dir="$1"
  shift
  local extra_args=("$@")
  if [ -f "$dir/impresspress_web_bg.wasm" ]; then
    log "using existing bundle: $dir"
    return
  fi
  local out_dir
  out_dir="$(relative_to_crate "$dir")"
  log "building missing bundle: wasm-pack build --target web --release --out-dir $out_dir ${extra_args[*]}"
  (cd "$CRATE_DIR" && wasm-pack build --target web --release --out-dir "$out_dir" "${extra_args[@]}")
}

build_missing "$PKG_DIR"
build_missing "$PKG_DEV_DIR" -- --features browser-devtools

DEFAULT_WASM="$PKG_DIR/impresspress_web_bg.wasm"
DEV_WASM="$PKG_DEV_DIR/impresspress_web_bg.wasm"

[ -f "$DEFAULT_WASM" ] || {
  echo "measure.sh: $DEFAULT_WASM does not exist after build_missing" >&2
  exit 1
}
[ -f "$DEV_WASM" ] || {
  echo "measure.sh: $DEV_WASM does not exist after build_missing" >&2
  exit 1
}

raw_size() { wc -c <"$1" | tr -d ' '; }
gz_size() { gzip -9 -c "$1" | wc -c | tr -d ' '; }

fmt_mib() { awk -v b="$1" 'BEGIN{printf "%.2f MiB (%d B)", b/1048576, b}'; }
fmt_kib() { awk -v b="$1" 'BEGIN{printf "%.1f KiB (%d B)", b/1024, b}'; }
fmt_signed_kib() { awk -v b="$1" 'BEGIN{s=(b>=0)?"+":"-"; v=(b>=0)?b:-b; printf "%s%.1f KiB (%d B)", s, v/1024, b}'; }

DEFAULT_RAW="$(raw_size "$DEFAULT_WASM")"
DEFAULT_GZ="$(gz_size "$DEFAULT_WASM")"
DEV_RAW="$(raw_size "$DEV_WASM")"
DEV_GZ="$(gz_size "$DEV_WASM")"
DELTA_GZ="$((DEV_GZ - DEFAULT_GZ))"

MAX_DELTA_BYTES="$((MEASURE_MAX_DEV_DELTA_KIB * 1024))"
DELTA_STATUS="PASS"
if [ "$DELTA_GZ" -gt "$MAX_DELTA_BYTES" ]; then
  DELTA_STATUS="FAIL"
  fail "dev gzip - default gzip = $(fmt_kib "$DELTA_GZ") > ${MEASURE_MAX_DEV_DELTA_KIB} KiB (MEASURE_MAX_DEV_DELTA_KIB)"
fi

echo "## Dev-sandbox size report"
echo
echo "wasm-opt: applied by \`wasm-pack build --release\` for both bundles (release profile default; not re-run here)."
echo
echo "| bundle | raw | gzip -9 |"
echo "| --- | --- | --- |"
echo "| default (\`pkg/\`) | $(fmt_mib "$DEFAULT_RAW") | $(fmt_mib "$DEFAULT_GZ") |"
echo "| dev (\`pkg-dev/\`, \`--features browser-devtools\`) | $(fmt_mib "$DEV_RAW") | $(fmt_mib "$DEV_GZ") |"
echo
echo "Δ gzip (dev − default): $(fmt_signed_kib "$DELTA_GZ") — threshold ≤ ${MEASURE_MAX_DEV_DELTA_KIB} KiB — **${DELTA_STATUS}**"
echo

# `dist/` is gitignored and composed by `build-compiler.sh` (restored from
# the `compiler-dist-*` cache in CI) — see the header. Its absence in a fresh
# checkout is expected, not an error.
COMPILER_MANIFEST="$HERE/compiler/dist/manifest.json"
echo "## Compiler bundle (\`examples/dev-sandbox/compiler/dist/\`)"
echo
if [ ! -f "$COMPILER_MANIFEST" ]; then
  echo "compiler: not built (no \`$COMPILER_MANIFEST\`)"
else
  COMPILER_STATS="$(python3 - "$COMPILER_MANIFEST" <<'PY'
import json, sys

with open(sys.argv[1]) as f:
    manifest = json.load(f)

assets = manifest.get("assets")
if not isinstance(assets, list):
    assets = []

usable = []
for a in assets:
    if not isinstance(a, dict):
        continue
    b = a.get("bytes")
    if not isinstance(b, int):
        continue
    usable.append((a.get("path", "?"), b))

total = manifest.get("total_bytes")
if not isinstance(total, int):
    total = sum(b for _, b in usable)

if usable:
    largest_path, largest_bytes = max(usable, key=lambda pb: pb[1])
else:
    largest_path, largest_bytes = "", 0

# Anything that is not the string "fast" is treated as a full build, so a
# manifest that cannot prove it skipped wasm-opt still gets gated.
build = "fast" if manifest.get("build") == "fast" else "full"

print(f"{total}\t{largest_bytes}\t{largest_path}\t{build}")
PY
  )"
  COMPILER_TOTAL_BYTES="$(cut -f1 <<<"$COMPILER_STATS")"
  COMPILER_LARGEST_BYTES="$(cut -f2 <<<"$COMPILER_STATS")"
  COMPILER_LARGEST_PATH="$(cut -f3 <<<"$COMPILER_STATS")"
  COMPILER_BUILD_KIND="$(cut -f4 <<<"$COMPILER_STATS")"

  MAX_COMPILER_TOTAL_BYTES="$((MEASURE_MAX_COMPILER_TOTAL_MIB * 1048576))"

  if [ "$COMPILER_BUILD_KIND" = "fast" ]; then
    # A --fast tree is unoptimized on purpose and can never be deployed
    # (`verify-compiler-assets.mjs` refuses it), so gating its size would
    # fail the job over a number that is not the shipping number. Report,
    # do not gate — see the header.
    TOTAL_STATUS="SKIPPED (fast build)"
    FILE_STATUS="SKIPPED (fast build)"
  else
    TOTAL_STATUS="PASS"
    if [ "$COMPILER_TOTAL_BYTES" -gt "$MAX_COMPILER_TOTAL_BYTES" ]; then
      TOTAL_STATUS="FAIL"
      fail "compiler total = $(fmt_mib "$COMPILER_TOTAL_BYTES") > ${MEASURE_MAX_COMPILER_TOTAL_MIB} MiB (MEASURE_MAX_COMPILER_TOTAL_MIB)"
    fi

    FILE_STATUS="PASS"
    if [ "$COMPILER_LARGEST_BYTES" -gt "$MEASURE_MAX_COMPILER_FILE_BYTES" ]; then
      FILE_STATUS="FAIL"
      fail "compiler largest file ($COMPILER_LARGEST_PATH) = $(fmt_mib "$COMPILER_LARGEST_BYTES") > $(fmt_mib "$MEASURE_MAX_COMPILER_FILE_BYTES") (MEASURE_MAX_COMPILER_FILE_BYTES)"
    fi
  fi

  echo "| metric | value | threshold | status |"
  echo "| --- | --- | --- | --- |"
  echo "| total bytes | $(fmt_mib "$COMPILER_TOTAL_BYTES") | ≤ ${MEASURE_MAX_COMPILER_TOTAL_MIB} MiB | $TOTAL_STATUS |"
  echo "| largest file | \`$COMPILER_LARGEST_PATH\` — $(fmt_mib "$COMPILER_LARGEST_BYTES") | ≤ $(fmt_mib "$MEASURE_MAX_COMPILER_FILE_BYTES") | $FILE_STATUS |"

  if [ "$COMPILER_BUILD_KIND" = "fast" ]; then
    echo
    echo "Compiler thresholds skipped: \`manifest.json\` says \`\"build\": \"fast\"\` — a"
    echo "\`build-compiler.sh --fast\` compose skips wasm-opt, so these sizes are"
    echo "larger than the shipping tree's and \`verify-compiler-assets.mjs\` refuses"
    echo "to deploy it. Sizes above are the honest numbers for the tree on disk."
  fi
fi

if [ "$FAILED" -ne 0 ]; then
  echo >&2
  echo "measure.sh: one or more thresholds breached (see FAIL lines above)" >&2
  exit 1
fi
