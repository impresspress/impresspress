#!/usr/bin/env bash
#
# check-cargo-lock.sh — fail, without writing anything, if the committed
# `Cargo.lock` is out of date for a crate's manifests.
#
# Usage:
#   scripts/check-cargo-lock.sh CRATE_DIR
#
# Runs `cargo metadata --locked` for `CRATE_DIR/Cargo.toml`. That resolves
# the whole workspace the crate belongs to, and exits non-zero instead of
# rewriting the lock when the manifests ask for something the lock does not
# record (a changed version requirement, a new dependency). The lock records
# the resolve of the whole workspace across every feature and target, so
# the check does not depend on which features a later build turns on.
#
# It exists because two tools this repository builds with take `--locked`
# without it guarding anything against a stale lock:
#   * wasm-pack has no `--locked`, and `wasm-pack build … -- --locked` only
#     reaches `cargo build` after wasm-pack's own plain `cargo metadata` has
#     already rewritten the lock (see `wasm-pack-locked.sh`, which runs this
#     first);
#   * `cargo install --path DIR --locked` builds against a stale lock without
#     an error, resolving afresh in memory, where `cargo build --locked`
#     refuses. Its `--locked` still matters once the lock is current: without
#     it `cargo install` ignores the lock altogether.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 CRATE_DIR" >&2
  exit 2
fi

manifest="$1/Cargo.toml"
if [ ! -f "$manifest" ]; then
  echo "check-cargo-lock.sh: $manifest does not exist" >&2
  exit 2
fi

if ! cargo metadata --locked --format-version 1 --manifest-path "$manifest" >/dev/null; then
  echo "check-cargo-lock.sh: Cargo.lock is out of date for $manifest." >&2
  echo "  Update the lock (for example \`cargo update -p <crate>\` for the dependency you changed)" >&2
  echo "  and commit it with the manifest change." >&2
  exit 1
fi
