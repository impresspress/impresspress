#!/usr/bin/env bash
# Smoke test: `npm pack && npm install` from this package in a clean,
# out-of-monorepo directory. Guards against the SDK depending on anything
# that only resolves inside this checkout (e.g. the old `wafer-client-js`
# `file:../../../wafer-run/...` dependency, which made `npm install` fail
# for anyone who didn't have the exact sibling wafer-run checkout).
set -euo pipefail

PKG_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PKG_DIR"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "==> Building package"
npm run build --silent

echo "==> npm pack into a clean directory ($TMP_DIR)"
TARBALL_NAME="$(npm pack --silent --pack-destination "$TMP_DIR")"
TARBALL_PATH="$TMP_DIR/$TARBALL_NAME"

INSTALL_DIR="$TMP_DIR/consumer"
mkdir -p "$INSTALL_DIR"
cd "$INSTALL_DIR"
npm init -y >/dev/null

echo "==> npm install from the packed tarball (no monorepo context, no registry deps)"
npm install "$TARBALL_PATH" --no-audit --no-fund --silent

echo "==> Verifying the installed package loads and exposes the client factory"
node -e "
const sdk = require('@impresspress/sdk');
if (typeof sdk.createImpresspressClient !== 'function') {
  console.error('FAIL: createImpresspressClient export missing after clean install');
  process.exit(1);
}
const client = sdk.createImpresspressClient('http://localhost:8090');
if (!client.auth || !client.storage || !client.iam || !client.extensions) {
  console.error('FAIL: client services missing after clean install');
  process.exit(1);
}
if (typeof sdk.ImpresspressError !== 'function') {
  console.error('FAIL: ImpresspressError export missing after clean install');
  process.exit(1);
}
console.log('OK: @impresspress/sdk packs, installs, and loads cleanly outside the monorepo');
"
