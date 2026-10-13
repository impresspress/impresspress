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

crate_dir="$1"
manifest="$crate_dir/Cargo.toml"
if [ ! -f "$manifest" ]; then
  echo "check-cargo-lock.sh: $manifest does not exist" >&2
  exit 2
fi

# From inside the crate: rustup picks the toolchain by the working directory
# (`rust-toolchain.toml`), not by `--manifest-path`, and wasm-pack runs its
# cargo from the crate too. Checking from the caller's directory could run a
# different cargo than the build that follows.
err="$(mktemp)"
trap 'rm -f "$err"' EXIT
status=0
(cd "$crate_dir" && cargo metadata --locked --format-version 1 --manifest-path Cargo.toml) \
  >/dev/null 2>"$err" || status=$?
cat "$err" >&2
if [ "$status" -eq 0 ]; then
  exit 0
fi

# Cargo's wording when `--locked` stops it from creating a missing lock or
# rewriting a stale one. Anything else — a manifest error, a registry or network
# failure, a missing git rev — is reported as what it is, not as a stale lock.
if grep -q 'cannot create the lock file .* because --locked was passed' "$err"; then
  echo "check-cargo-lock.sh: there is no Cargo.lock for $manifest; generate it and commit it." >&2
elif grep -q 'because --locked was passed' "$err"; then
  echo "check-cargo-lock.sh: Cargo.lock is out of date for $manifest." >&2
  echo "  Update the lock (for example \`cargo update -p <crate>\` for the dependency you changed)" >&2
  echo "  and commit it with the manifest change." >&2
else
  echo "check-cargo-lock.sh: \`cargo metadata --locked\` failed for $manifest (cargo's error is above);" >&2
  echo "  Cargo.lock was not checked and not changed." >&2
fi
exit 1
