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

const swPath = process.env.SW_JS;
if (!swPath) {
  throw new Error(
    'SW_JS is not set: these tests load a rendered sw.js, which ' +
      '`cargo test -p impresspress-bundle` renders and passes in'
  );
}
const rendered = fs.readFileSync(swPath, 'utf8');

const GLUE_IMPORT = /^import init, \{ initialize, handle_request \} from '[^']+';$/m;
if (!GLUE_IMPORT.test(rendered)) {
  throw new Error('the rendered sw.js no longer imports the wasm glue the way this harness replaces');
}
const source = rendered.replace(
  GLUE_IMPORT,
  'const { init, initialize, handle_request } = globalThis.__swRuntimeStubs;'
);

export const ORIGIN = 'https://app.example';

let instances = 0;

/// One fresh worker: its own module instance (so its own `poisoned` state),
/// its own stubs. `runtime` supplies `init` / `initialize` / `handle_request`;
/// each defaults to succeeding.
export async function loadWorker(runtime = {}) {
  const listeners = {};
  const network = [];
  const posted = [];
  const navigated = [];
  let unregistered = 0;
  const client = {
    url: `${ORIGIN}/b/auth/login`,
    postMessage: (message) => posted.push(message),
    navigate: (url) => navigated.push(url)
  };

  globalThis.__swRuntimeStubs = {
    init: runtime.init ?? (async () => {}),
    initialize: runtime.initialize ?? (async () => {}),
    handle_request:
      runtime.handle_request ??
      (async () => ({ response: new Response('from the runtime'), after: Promise.resolve() }))
  };
  globalThis.self = {
    location: { origin: ORIGIN },
    addEventListener: (type, listener) => {
      listeners[type] = listener;
    },
    skipWaiting: async () => {},
    registration: {
      unregister: async () => {
        unregistered += 1;
        return true;
      }
    },
    clients: { claim: async () => {}, matchAll: async () => [client] }
  };
  // What the static host would say. The 405 is the one the incident met.
  globalThis.fetch = async (request) => {
    network.push(request);
    return new Response(null, { status: 405 });
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
