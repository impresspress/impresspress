#!/usr/bin/env node
// Make "the next build of the runtime" out of the one that was just built.
//
//   node next-runtime-pkg.mjs <pkg-dir> <out-dir>
//
// `sw-update.spec.ts` deploys one bundle over another and asserts the browser
// moves to the second. The two bundles have to differ the way two real
// deployments do — a different runtime binary, so a different content hash,
// so a different `sw.js` — and compiling `impresspress-web` a second time
// from a changed source costs a CI job most of its budget to produce exactly
// that: a wasm with other bytes.
//
// So this copies a wasm-pack output and appends one custom section to the
// copy's `*_bg.wasm`. A custom section is part of the module's bytes and of
// nothing else: the engine skips it, so the copy is the same program under a
// new hash. `examples/dev-sandbox/build.sh --pkg-dir <out-dir>` then assembles
// the second bundle with the same recipe as the first.
import { cpSync, existsSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';

const [, , pkgDir, outDir] = process.argv;
if (!pkgDir || !outDir) {
  console.error('usage: next-runtime-pkg.mjs <pkg-dir> <out-dir>');
  process.exit(2);
}

const wasms = readdirSync(pkgDir).filter((name) => name.endsWith('_bg.wasm'));
if (wasms.length !== 1) {
  console.error(`${pkgDir}: expected exactly one *_bg.wasm, found ${JSON.stringify(wasms)}`);
  process.exit(1);
}

/** Unsigned LEB128, the encoding every size in a wasm module uses. */
function leb128(value) {
  const bytes = [];
  do {
    let byte = value & 0x7f;
    value >>>= 7;
    if (value !== 0) byte |= 0x80;
    bytes.push(byte);
  } while (value !== 0);
  return Buffer.from(bytes);
}

if (existsSync(outDir)) rmSync(outDir, { recursive: true });
cpSync(pkgDir, outDir, { recursive: true });

const wasmPath = path.join(outDir, wasms[0]);
const wasm = readFileSync(wasmPath);
if (wasm.subarray(0, 8).compare(Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00])) !== 0) {
  console.error(`${wasmPath}: not a version-1 wasm module`);
  process.exit(1);
}
// Section id 0 (custom): a size, then a name (length-prefixed), then bytes
// that are the section's own business.
const name = Buffer.from('impresspress-e2e-next-runtime', 'utf8');
const payload = Buffer.concat([leb128(name.length), name, Buffer.from('a later build', 'utf8')]);
writeFileSync(wasmPath, Buffer.concat([wasm, Buffer.from([0x00]), leb128(payload.length), payload]));

console.log(path.resolve(outDir));
