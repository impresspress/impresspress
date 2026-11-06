#!/usr/bin/env bash
#
# build-sandbox-runtime.sh — compile one of the dev sandbox's two runtimes.
#
# Usage:
#   scripts/build-sandbox-runtime.sh pkg-dev|pkg-site
#
# The one statement of which features make which runtime, and the command that
# compiles it. `examples/dev-sandbox/build.sh` runs this for a deploy or a local
# build, and the `sandbox-runtime` job in `ci-shared.yml` runs it for the e2e
# jobs, so what CI tests is compiled the way what ships is compiled.
#
#   pkg-dev   the workspace: the site runtime plus the workspace
#   pkg-site  the site runtime alone, for the export shell
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 pkg-dev|pkg-site" >&2
  exit 2
fi

case "$1" in
  pkg-dev) features=browser-devtools ;;
  pkg-site) features=browser-site-runtime ;;
  *)
    echo "build-sandbox-runtime.sh: unknown runtime '$1' (pkg-dev or pkg-site)" >&2
    exit 2
    ;;
esac

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec "$repo/scripts/wasm-pack-locked.sh" "$repo/crates/impresspress-web" build --target web --release --out-dir "$1" -- --features "$features"
