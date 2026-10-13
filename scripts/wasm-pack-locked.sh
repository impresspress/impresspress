#!/usr/bin/env bash
#
# wasm-pack-locked.sh — run wasm-pack for a crate of this workspace, refusing
# first if the committed `Cargo.lock` is out of date.
#
# Usage:
#   scripts/wasm-pack-locked.sh CRATE_DIR WASM_PACK_ARGS...
#
#   scripts/wasm-pack-locked.sh crates/impresspress-web \
#     build --target web --release --out-dir pkg -- --features browser-devtools
#   scripts/wasm-pack-locked.sh crates/impresspress-browser test --node
#
# wasm-pack runs from CRATE_DIR, so `--out-dir` and any relative path in the
# environment resolve against the crate, exactly as `cd CRATE_DIR && wasm-pack
# …` would. The environment (RUSTFLAGS, NODE_OPTIONS, …) passes through.
#
# Why this exists: wasm-pack has no `--locked`, and `wasm-pack build … --
# --locked` does not lock anything. Before it runs `cargo build`, wasm-pack
# reads the crate with a plain `cargo metadata` (no `--locked`, no
# `--no-deps`), which resolves the dependency graph and rewrites a stale
# `Cargo.lock` to match the manifests. By the time `cargo build --locked`
# runs the lock is already current, so the flag can never fire. Checked with
# wasm-pack 0.14.0 and 0.15.0 (the version CI pins): a dependency version
# requirement changed without updating the lock, then `wasm-pack build … --
# --locked`, exits 0 and leaves the lock rewritten.
#
# So `check-cargo-lock.sh` checks the lock first, with `cargo metadata
# --locked` for the same manifest: the same resolve wasm-pack's own `cargo
# metadata` does, but failing without writing anything. Once it passes, the
# lock is current, wasm-pack's `cargo metadata` has nothing to rewrite, and a
# `-- --locked` on the build would add nothing, which is why no caller passes
# one.
set -euo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 CRATE_DIR WASM_PACK_ARGS..." >&2
  exit 2
fi

crate_dir="$1"
shift
status=0
"$(dirname "${BASH_SOURCE[0]}")/check-cargo-lock.sh" "$crate_dir" || status=$?
if [ "$status" -ne 0 ]; then
  echo "wasm-pack-locked.sh: wasm-pack was not run." >&2
  exit "$status"
fi

cd "$crate_dir"
exec wasm-pack "$@"
