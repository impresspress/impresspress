#!/usr/bin/env node
// Checks a finished web bundle (the output of `impresspress build --target
// web`) against the runtime it was bundled around:
//
//   1. the bundle's `sw.js` LINKS — every static import it makes, and every
//      import of the modules those reach, resolves to a binding the target
//      module exports. This is V8's own module linker (`vm.SourceTextModule`),
//      run without evaluating a line, so it fails exactly where a browser
//      fails to start the worker: `sw.js` statically imports
//      `compiled_sandbox_mode` from the runtime's JS glue, and a runtime
//      without that export never runs the worker at all;
//   2. the runtime's wasm carries exactly one `impresspress.sandbox_mode`
//      custom section, whose bytes are the expected mode's spelling, read by
//      the engine's own parser (`WebAssembly.Module.customSections`).
//
// Usage: node --experimental-vm-modules scripts/check-bundle-runtime.mjs \
//          <bundle-dir> <none|site|workspace>
//
// The wasm is the one the glue `sw.js` imports instantiates: the
// `*_bg-<hash>.wasm` beside it, which the bundler leaves as the only one.

import { readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';
import vm from 'node:vm';

const SECTION = 'impresspress.sandbox_mode';
const MODES = ['none', 'site', 'workspace'];

const [bundleArg, expected] = process.argv.slice(2);
if (!bundleArg || !MODES.includes(expected)) {
  console.error(`usage: check-bundle-runtime.mjs <bundle-dir> <${MODES.join('|')}>`);
  process.exit(2);
}
if (typeof vm.SourceTextModule !== 'function') {
  console.error('run with node --experimental-vm-modules (vm.SourceTextModule is unavailable)');
  process.exit(2);
}
const root = path.resolve(bundleArg);
const rootUrl = pathToFileURL(root + path.sep);

const failures = [];

// --- 1. sw.js links ---------------------------------------------------------

// A bundle is served from its own root, so `/x.js` names `<bundle>/x.js`.
function resolve(specifier, referrer) {
  if (specifier.startsWith('/')) return new URL(specifier.slice(1), rootUrl);
  if (specifier.startsWith('./') || specifier.startsWith('../')) {
    return new URL(specifier, referrer);
  }
  throw new Error(`${specifier} (imported by ${referrer}) is not a path a browser can load`);
}

const context = vm.createContext({});
const modules = new Map();
function load(url) {
  const key = url.href;
  if (!modules.has(key)) {
    const source = readFileSync(fileURLToPath(url), 'utf8');
    modules.set(key, new vm.SourceTextModule(source, { identifier: key, context }));
  }
  return modules.get(key);
}

const sw = new URL('sw.js', rootUrl);
try {
  await load(sw).link((specifier, referencing) =>
    load(resolve(specifier, new URL(referencing.identifier))),
  );
  console.log(`ok   sw.js links (${modules.size} modules, nothing evaluated)`);
} catch (e) {
  failures.push(`sw.js does not link, so a browser never starts the worker: ${e.message}`);
}

// --- 2. the wasm declares the mode -----------------------------------------

const wasms = readdirSync(root).filter((f) => /_bg-[0-9a-f]{8}\.wasm$/.test(f));
if (wasms.length !== 1) {
  failures.push(`expected exactly one hashed *_bg-<hash>.wasm in ${root}, found [${wasms.join(', ')}]`);
} else {
  const wasm = wasms[0];
  const module = new WebAssembly.Module(readFileSync(path.join(root, wasm)));
  const sections = WebAssembly.Module.customSections(module, SECTION).map((b) =>
    Buffer.from(b).toString('utf8'),
  );
  if (sections.length !== 1) {
    failures.push(`${wasm} has ${sections.length} '${SECTION}' custom sections, not 1`);
  } else if (sections[0] !== expected) {
    failures.push(`${wasm} declares sandbox mode '${sections[0]}', expected '${expected}'`);
  } else {
    console.log(`ok   ${wasm} declares sandbox mode '${expected}' (${SECTION})`);
  }
}

for (const f of failures) console.error(`FAIL ${f}`);
process.exit(failures.length ? 1 : 0);
