// Harness for the behaviour tests of the RENDERED service worker
// (`sw_runtime_stopped.test.mjs`).
//
// Run through `cargo test -p impresspress-bundle` — the
// `the_rendered_worker_answers_for_a_stopped_runtime` test in
// `tests/bundle_integration.rs` renders the shipped `sw.js.tmpl` with the real
// bundler and passes the result's path in `SW_JS`. Loading the rendered file
// rather than the template is the point: what is under test is what a browser
// is served, placeholders substituted.
//
// One substitution is made here, and only one. The worker's first statement
// imports the wasm-bindgen glue from an absolute URL (`/app-<hash>.js`), which
// nothing in Node can resolve; that line is replaced with the three bindings
// it would have produced, taken from the stubs a test passes in. Everything
// below it runs as shipped, against a stub `self` and a stub `fetch`.
import fs from 'node:fs';

const GLUE_IMPORT = /^import init, \{ initialize, handle_request \} from '[^']+';$/m;

function source(variable) {
  const file = process.env[variable];
  if (!file) {
    throw new Error(
      `${variable} is not set: these tests load a rendered sw.js, which ` +
        '`cargo test -p impresspress-bundle` renders and passes in'
    );
  }
  const rendered = fs.readFileSync(file, 'utf8');
  if (!GLUE_IMPORT.test(rendered)) {
    throw new Error('the rendered sw.js no longer imports the wasm glue the way this harness replaces');
  }
  return rendered.replace(
    GLUE_IMPORT,
    'const { init, initialize, handle_request } = globalThis.__swRuntimeStubs;'
  );
}

// `SW_JS` is the default rendering; `SW_JS_WIPE` the one with
// `opfs_wipe_on_recovery`, whose answers say what a reload erases.
const SOURCES = { plain: source('SW_JS'), wipe: source('SW_JS_WIPE') };

export const ORIGIN = 'https://app.example';
/// The address of the page the harness's one client is on: a path only the
/// runtime serves.
export const CLIENT_URL = `${ORIGIN}/b/auth/login`;
/// What the static host serves at the boot shell's URL.
export const SHELL_HTML = '<!DOCTYPE html><title>the boot shell</title>';

let instances = 0;

/// One fresh worker: its own module instance (so its own `poisoned` state),
/// its own stubs. `runtime` supplies `init` / `initialize` / `handle_request`;
/// each defaults to succeeding. `runtime.host(url)` is the static host's
/// answer to a request the worker makes; by default it has the boot shell at
/// `/` and nothing else — a plain file server, with no fallback for the paths
/// only the runtime serves. `wipe` picks the `opfs_wipe_on_recovery`
/// rendering.
export async function loadWorker(runtime = {}, { wipe = false } = {}) {
  const source = SOURCES[wipe ? 'wipe' : 'plain'];
  const listeners = {};
  const network = [];
  const posted = [];
  const navigated = [];
  const warnings = [];
  let unregistered = 0;
  const client = {
    url: CLIENT_URL,
    postMessage: (message) => posted.push(message),
    // Resolves or rejects as `runtime.navigate` says: a real `navigate()`
    // rejects for a client the worker does not control.
    navigate: (url) => {
      navigated.push(url);
      return runtime.navigate ? runtime.navigate(url) : Promise.resolve(client);
    }
  };
  // Cache Storage, as far as the worker uses it: `open(name).put(key, response)`.
  const stored = new Map();
  globalThis.caches = {
    open: async (name) => ({
      put: async (key, response) => {
        stored.set(`${name} ${key}`, await response.json());
      }
    })
  };

  globalThis.__swRuntimeStubs = {
    init: runtime.init ?? (async () => {}),
    initialize: runtime.initialize ?? (async () => {}),
    handle_request:
      runtime.handle_request ??
      (async () => ({ response: new Response('from the runtime'), after: Promise.resolve() }))
  };
  globalThis.self = {
    location: { origin: ORIGIN, href: `${ORIGIN}/sw.js` },
    addEventListener: (type, listener) => {
      listeners[type] = listener;
    },
    skipWaiting: async () => {},
    registration: {
      unregister: async () => {
        unregistered += 1;
        if (typeof runtime.unregisters === 'function') return runtime.unregisters();
        return runtime.unregisters ?? true;
      }
    },
    clients: { claim: async () => {}, matchAll: async () => [client] }
  };
  // What the static host would say. `network` records what it was asked for:
  // the URL string the worker passed, or the request object it forwarded.
  globalThis.fetch = async (request) => {
    network.push(request);
    if (runtime.host) return runtime.host(request);
    return request === '/'
      ? new Response(SHELL_HTML, { status: 200, headers: { 'Content-Type': 'text/html' } })
      : new Response('no such file', { status: 404 });
  };

  // A distinct URL per call, so each call evaluates the module afresh.
  instances += 1;
  const url = `data:text/javascript;base64,${Buffer.from(`${source}\n// ${instances}`).toString('base64')}`;
  await import(url);

  /// Dispatch a fetch event and resolve to what the worker answered with —
  /// `undefined` if it never called `respondWith` (a bypassed request).
  ///
  /// The request is a plain object, not a `Request`: `mode: 'navigate'` is the
  /// property under test and the constructor refuses it.
  async function request(pathname, { method = 'GET', mode = 'cors' } = {}) {
    let answer;
    const event = {
      request: { url: `${ORIGIN}${pathname}`, method, mode },
      respondWith: (promise) => {
        answer = promise;
      },
      waitUntil: () => {}
    };
    listeners.fetch(event);
    return { response: await answer, sent: event.request };
  }

  return {
    request,
    network,
    posted,
    navigated,
    /// What the worker left for the boot shell, or `undefined`.
    leftForBootShell: () => stored.get('__impresspress_sw_stopped /__impresspress_sw_stopped'),
    /// How many times the worker unregistered itself. It never should: a
    /// dead worker stays registered so that it can answer navigations.
    unregistered: () => unregistered
  };
}

/// Silence the worker's own `console.log` / `console.error` for one test and
/// hand back what it said at error level.
export function captureConsole(t) {
  const errors = [];
  t.mock.method(console, 'log', () => {});
  t.mock.method(console, 'warn', () => {});
  t.mock.method(console, 'error', (...args) => errors.push(args.map(String).join(' ')));
  return errors;
}
