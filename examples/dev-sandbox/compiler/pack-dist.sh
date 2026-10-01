#!/usr/bin/env bash
#
# Pack the built `compiler/dist/` as the release asset `fetch-dist.sh` looks
# for, and print the `gh release create` that publishes it.
#
# This is the developer half of the deploy path. `compiler/dist/` is not in
# the repo and cannot be built in CI — the composition wants ~35 minutes and
# 12.6 GB of RSS — so it is built once per pin on a machine that has them,
# published as a release asset, and fetched everywhere else. See
# `README.md`, § "Publishing the compiler".
#
# Only a verified, OPTIMIZED tree is packed: a `--fast` component is refused
# here as it is at the deploy, and for the same reason — what was verified
# has to be what ships.
#
# Usage:
#   compiler/pack-dist.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'pack-dist.sh: %s\n' "$*" >&2; exit 1; }

command -v node >/dev/null || die "node is required (>= 22)"
command -v tar >/dev/null || die "tar is required"

VERSION="$(node -p "require('$HERE/PIN.json').version")"
TAG="compiler-$VERSION"
ASSET="compiler-dist-$VERSION.tar"

DIST="$HERE/dist"
CACHE="$HERE/.cache"
mkdir -p "$CACHE"
TARBALL="$CACHE/$ASSET"

[ -f "$DIST/manifest.json" ] || die "compiler/dist is not built — run build-compiler.sh first"

BUILD_KIND="$(node -p "require('$DIST/manifest.json').build")"
[ "$BUILD_KIND" = full ] || die "dist/manifest.json says \"build\": \"$BUILD_KIND\".

  Only an optimized tree may be published: a --fast component skips wasm-opt
  entirely, and the whole point of the release asset is that what was verified
  is what every deploy serves. Rebuild without --fast."

# The environment is cleared for this one run rather than trusted: a shell
# that still has IMPRESSPRESS_COMPILER_ALLOW_FAST=1 from testing the CI path
# must not be able to publish the tree that flag was set for.
log "verifying dist/ before packing"
env -u IMPRESSPRESS_COMPILER_ALLOW_FAST node "$HERE/scripts/verify-compiler-assets.mjs"

# `manifest.json` and the one version directory, which is exactly what
# `verify-compiler-assets.mjs` allows under `dist/` — so the tar is the
# directory, and `fetch-dist.sh` unpacks it into an empty one.
log "packing $ASSET"
rm -f "$TARBALL"
tar -cf "$TARBALL" -C "$DIST" manifest.json "$VERSION"

BYTES="$(node -p "require('node:fs').statSync('$TARBALL').size")"
log "$TARBALL — $((BYTES / 1024 / 1024)) MiB"

cat <<EOF

Publish it with:

  gh release create $TAG '$TARBALL' \\
    --title 'Compiler dist $VERSION' \\
    --notes 'Packaged Rubrc toolchain for compiler/PIN.json version $VERSION, built by build-compiler.sh without --fast. Fetched by examples/dev-sandbox/compiler/fetch-dist.sh; nothing else consumes this release.'

An existing release for this pin takes an upload instead:

  gh release upload $TAG '$TARBALL' --clobber
EOF
