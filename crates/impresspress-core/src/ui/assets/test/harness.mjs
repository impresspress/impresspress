// Shared harness for `webmcp.js` unit tests (`node --test
// crates/impresspress-core/src/ui/assets/test/*.test.mjs`).
//
// `webmcp.js` is authored as a TAIL, not a script: `ui/assets.rs`'s
// `compose_webmcp_script` wraps `webmcp-core.js` and it together in one IIFE,
// and nothing it declares reaches `window` except the deliberate
// `__impresspressWebmcp` handle. This does the same composition — the same
// two files, the same order — except the wrapper is test-only and returns a
// handle onto the closure, which is how the tail's internals are reached
// without adding a test hook to the shipped file. Same shape as
// `blocks/dev/assets/test/harness.mjs`, for the same reason.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const assets = path.join(here, '..');
const core = fs.readFileSync(path.join(assets, 'webmcp-core.js'), 'utf8');
const tail = fs.readFileSync(path.join(assets, 'webmcp.js'), 'utf8');

/// A stub `navigator.serviceWorker`.
///
/// `controlled` is the state the whole service-worker gate turns on — see
/// `whenControlled` in `webmcp.js`. `hasRegistration` distinguishes a
/// service-worker build (a registration exists; the worker will claim this
/// page) from a native server, where `getRegistration()` resolves `undefined`
/// and nothing will ever claim it. It is a boolean rather than the
/// registration object itself on purpose: a `registration: undefined` option
/// would silently take the default instead of meaning "none", which is
/// exactly the case that most needs to be expressible here.
export function serviceWorkerStub({ controlled = false, hasRegistration = true } = {}) {
  const listeners = new Set();
  return {
    get controller() {
      return controlled ? { scriptURL: '/sw.js' } : null;
    },
    addEventListener(type, listener) {
      if (type === 'controllerchange') listeners.add(listener);
    },
    removeEventListener(type, listener) {
      if (type === 'controllerchange') listeners.delete(listener);
    },
    getRegistration() {
      return Promise.resolve(hasRegistration ? { scope: '/' } : undefined);
    },
    /// The worker calling `clients.claim()`.
    claim() {
      controlled = true;
      listeners.forEach((l) => l());
    },
    /// How many `controllerchange` listeners are still attached — a
    /// resolved wait must not leave one behind.
    listenerCount() {
      return listeners.size;
    }
  };
}

// Builds one fresh, isolated instance of the composed script. Each instance
// gets its own stub `document`/`window`/`fetch`, so no test can leak
// registered tools or a `generation` count into another.
export function instantiate({ serviceWorker = undefined, respond } = {}) {
  const fetchCalls = [];
  const registerCalls = [];
  const unregisterCalls = [];

  const defaultRespond = () => ({
    ok: true,
    status: 200,
    json: async () => ({ tools: [manifestTool('list_products')] })
  });

  // Every event the tail dispatched on `window`, by type, in order.
  const windowEvents = [];
  const sandbox = {
    document: {},
    window: {
      dispatchEvent(event) {
        windowEvents.push(event.type);
        return true;
      }
    },
    Event,
    navigator: {
      serviceWorker,
      modelContext: {
        registerTool(options) {
          registerCalls.push(options.name);
        },
        unregisterTool(name) {
          unregisterCalls.push(name);
        }
      }
    },
    fetch(...args) {
      fetchCalls.push(args);
      // A `respond` that throws stands for a network failure, and the real
      // `fetch` reports that as a REJECTED PROMISE, never as a synchronous
      // throw. Returning it any other way would test a shape the browser
      // cannot produce.
      try {
        return Promise.resolve((respond || defaultRespond)(...args));
      } catch (error) {
        return Promise.reject(error);
      }
    },
    URLSearchParams,
    Promise,
    JSON,
    Array
  };

  const factory = new Function(
    ...Object.keys(sandbox),
    `${core}
${tail}
return {
  generation: function () { return generation },
  get registered() { return registered.slice() },
  refresh,
  whenControlled
};`
  );
  const handle = factory(...Object.values(sandbox));
  return {
    handle,
    fetchCalls,
    registerCalls,
    unregisterCalls,
    // What the tail published for the rest of the page.
    published: sandbox.window.__impresspressWebmcp,
    windowEvents
  };
}

/// The shape one manifest entry has, trimmed to what `toolOptions` reads.
export function manifestTool(name) {
  return {
    name,
    description: name,
    inputSchema: { type: 'object' },
    invocation: { method: 'get', path: '/b/x' }
  };
}

/// Lets every already-queued microtask run. The gate is promise-based, so
/// "did it fetch yet" is only meaningful after the queue has drained.
export function settle() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}
