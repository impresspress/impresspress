#!/usr/bin/env bash
#
# Put the published, optimized `dist/` in place — the only way a deploy gets
# one.
#
# `compiler/dist/` is 72 MB of packaged toolchain and is not in the repo. It
# is fully determined by `PIN.json`, but BUILDING it costs ~35 minutes of
# `wasm-opt -Oz` peaking at 12.6 GB of RSS, which no ordinary CI runner has.
# So the developer builds it once per pin and publishes it as a release asset
# (`pack-dist.sh` prints the command; `README.md` § "Publishing the compiler"
# is the procedure), and this fetches it.
#
#   asset   compiler-dist-<version>.tar
#   tag     compiler-<version>
#   repo    this one
#
# `<version>` is `PIN.json`'s `version` — rubrc's commit at eight characters,
# then a packaging revision (`807ace9e.2`) — so the asset a tree asks for
# moves with its pin and with every change to the packaging, and can never be
# a toolchain this checkout did not ask for.
#
# What comes down is checked, not trusted: `verify-compiler-assets.mjs` hashes
# every file against the manifest inside the tar, refuses anything over the
# 24 MiB per-part cap, and refuses a tree whose manifest is for another pin.
# On top of that this script requires `"build": "full"` — an optimized
# component — and it requires it directly rather than leaving it to the
# verifier, because the CI jobs that only test whether the compiler still
# works export `IMPRESSPRESS_COMPILER_ALLOW_FAST=1` for the whole job and this
# script runs inside them.
#
# Usage:
#   compiler/fetch-dist.sh          # fetch, verify, leave dist/ ready
#
# Exits non-zero — loudly, and saying what to publish — when there is no asset
# for this pin.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'fetch-dist.sh: %s\n' "$*" >&2; exit 1; }

command -v node >/dev/null || die "node is required (>= 22)"
command -v tar >/dev/null || die "tar is required"

REPO_SLUG="${IMPRESSPRESS_COMPILER_DIST_REPO:-impresspress/impresspress}"

VERSION="$(node -p "require('$HERE/PIN.json').version")"
TAG="compiler-$VERSION"
ASSET="compiler-dist-$VERSION.tar"
URL="https://github.com/$REPO_SLUG/releases/download/$TAG/$ASSET"

DIST="$HERE/dist"
CACHE="$HERE/.cache"
mkdir -p "$CACHE"

# What a verified, optimized tree for this pin looks like from outside. Run
# before the download so a second call is free, and again after it so the
# thing that lands is held to the same bar.
#
# `IMPRESSPRESS_COMPILER_ALLOW_FAST` is cleared for the verifier's own run:
# the deploy path never sets it, but the correctness-CI jobs do, and this
# script must answer the same way in both.
dist_is_publishable() {
  [ -f "$DIST/manifest.json" ] || return 1
  node -e '
    const fs = require("node:fs");
    const manifest = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const pin = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
    if (manifest.version !== pin.version) process.exit(1);
    if (manifest.build !== "full") process.exit(1);
  ' "$DIST/manifest.json" "$HERE/PIN.json" || return 1
  env -u IMPRESSPRESS_COMPILER_ALLOW_FAST \
    node "$HERE/scripts/verify-compiler-assets.mjs" >/dev/null || return 1
}

if dist_is_publishable; then
  log "dist/$VERSION is already the published tree for this pin"
  exit 0
fi

TARBALL="$CACHE/$ASSET"
rm -f "$TARBALL"

# `gh` first: on a private repo it is the only one of the two that can
# authenticate, and on a runner it already holds a token. `curl` is the
# fallback for a machine without it — and for a `gh` that is installed but
# not logged in, which fails rather than falling through on its own.
if command -v gh >/dev/null; then
  log "gh release download $TAG --repo $REPO_SLUG"
  gh release download "$TAG" --repo "$REPO_SLUG" --pattern "$ASSET" --dir "$CACHE" \
    || rm -f "$TARBALL"
fi

if [ ! -f "$TARBALL" ]; then
  log "curl $URL"
  command -v curl >/dev/null || die "curl is required when gh cannot fetch the asset"
  if curl -fsSL --retry 3 -o "$TARBALL.tmp" "$URL"; then
    mv "$TARBALL.tmp" "$TARBALL"
  else
    rm -f "$TARBALL.tmp"
  fi
fi

[ -f "$TARBALL" ] || die "there is no $ASSET on release $TAG of $REPO_SLUG.

  compiler/PIN.json pins compiler $VERSION, and the optimized toolchain for that
  pin has not been published. Nothing here can build it: the composition needs
  ~35 minutes and 12.6 GB of RSS. On a machine that has them:

    examples/dev-sandbox/compiler/build-compiler.sh   # once, ~55 min
    examples/dev-sandbox/compiler/pack-dist.sh        # prints the gh command

  See compiler/README.md, \"Publishing the compiler\"."

# Unpacked into a fresh directory, never over what is there: `dist/` holds
# exactly one version plus the manifest naming it, and the verifier refuses
# anything else — so a leftover tree from another pin must not survive this.
log "extracting $ASSET into dist/"
rm -rf "$DIST"
mkdir -p "$DIST"
tar -xf "$TARBALL" -C "$DIST"

dist_is_publishable || die "$ASSET is not a verified, optimized tree for pin $VERSION —
  re-run scripts/verify-compiler-assets.mjs for the detail. Republish it with
  pack-dist.sh, which refuses to pack anything this would reject."

log "dist/$VERSION ready from $TAG ($(du -sh "$DIST" | cut -f1))"
