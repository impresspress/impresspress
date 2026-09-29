// Node module-customization hook for every Node-hosted run of this crate's
// tests: the `node --test` suites in this directory (`storage_paths.test.mjs`,
// `http_fetch.test.mjs`) AND `wasm-pack test --node`, whose generated test
// module imports bridge.js through wasm-bindgen's snippet loader and fails to
// load without it — see the `browser-wasm-test` CI job, which passes it to
// both via `--import` and `NODE_OPTIONS` respectively.
//
// bridge.js is a wasm-bindgen snippet module with a top-level
// `import initSqlJs from '/vendor/sql-wasm-esm.js'` that a browser/Service
// Worker resolves from the site root, but which plain Node can't resolve at
// all (no such path exists on disk). This hook resolves that one specifier to
// a small module wrapping the REAL sql.js the site serves — the vendored
// `impresspress-bundle/assets/vendor/sql-wasm-esm.js` and its `.wasm` — so a
// wasm test can run `dbInit()` and put the browser `DatabaseService` in front
// of the same SQLite build production uses (`database.rs`,
// `sql_js_conformance`). Importing bridge.js does not start sql.js; only a
// call to the wrapper's `initSqlJs` does.
//
// The wrapper makes two adjustments, neither of which a browser needs:
// - sql.js's Emscripten loader, seeing Node, calls `require("fs")` and reads
//   `__dirname`, neither of which exists in an ES module, so it gets both as
//   globals before it runs;
// - `locateFile` answers the vendored `.wasm`'s path on disk, where bridge.js
//   asks for the site-root `/vendor/sql-wasm.wasm`.
//
// Registered as the loader for the process via
// `node --import ./js/test/node-hooks.mjs --test ...`: on --import this
// file runs once in the main thread, where it self-registers (pointing
// `register()` at its own URL) so Node also loads it as the hooks module
// in the internal loader thread; `isMainThread` guards against
// re-registering when that second load happens. The `resolve`/`load` pair
// below then intercepts only the one specifier bridge.js can't resolve
// under Node and replaces it with the wrapper — every other specifier passes
// through to the default resolver/loader untouched.
import { register } from 'node:module';
import { fileURLToPath } from 'node:url';
import { isMainThread } from 'node:worker_threads';

const STUBBED_SPECIFIER = '/vendor/sql-wasm-esm.js';
const STUB_URL = 'node-hooks-stub:sql-wasm-esm';
const VENDOR = new URL('../../../impresspress-bundle/assets/vendor/', import.meta.url);
const SQL_JS_URL = new URL('sql-wasm-esm.js', VENDOR).href;
const SQL_WASM_PATH = fileURLToPath(new URL('sql-wasm.wasm', VENDOR));
const STUB_SOURCE = `import { createRequire } from 'node:module';
import realInitSqlJs from ${JSON.stringify(SQL_JS_URL)};

export default function initSqlJs(config) {
    globalThis.require ??= createRequire(${JSON.stringify(SQL_JS_URL)});
    globalThis.__dirname ??= '/';
    return realInitSqlJs({ ...config, locateFile: () => ${JSON.stringify(SQL_WASM_PATH)} });
}
`;

if (isMainThread) {
    register(new URL(import.meta.url), import.meta.url);
}

export async function resolve(specifier, context, nextResolve) {
    if (specifier === STUBBED_SPECIFIER) {
        return { url: STUB_URL, shortCircuit: true };
    }
    return nextResolve(specifier, context);
}

export async function load(url, context, nextLoad) {
    if (url === STUB_URL) {
        return { format: 'module', shortCircuit: true, source: STUB_SOURCE };
    }
    return nextLoad(url, context);
}
