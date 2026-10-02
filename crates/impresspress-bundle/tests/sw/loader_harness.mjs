// Harness for the behaviour tests of the RENDERED boot loader
// (`loader_recovery.test.mjs`). The counterpart of `harness.mjs`, and run the
// same way: `the_rendered_loader_recovers_once_and_keeps_the_cause` in
// `tests/bundle_integration.rs` renders the shipped `loader.js.tmpl` with the
// real bundler — once per value of `opfs_wipe_on_recovery` — and passes the
// two paths in `LOADER_JS` and `LOADER_JS_WIPE`.
//
// `loader.js` is a classic script whose last statement is `boot();`. One
// substitution is made: that statement becomes `return boot();`, so a test can
// await the boot it started. Everything above it runs as shipped, as the body
// of a function whose parameters are the browser globals it reaches for.
import fs from 'node:fs';

function source(variable) {
  const file = process.env[variable];
  if (!file) {
    throw new Error(
      `${variable} is not set: these tests load a rendered loader.js, which ` +
        '`cargo test -p impresspress-bundle` renders and passes in'
    );
  }
  const rendered = fs.readFileSync(file, 'utf8');
  if (!/\nboot\(\);\n$/.test(rendered)) {
    throw new Error('the rendered loader.js no longer ends in the `boot();` this harness replaces');
  }
  return rendered.replace(/\nboot\(\);\n$/, '\nreturn boot();\n');
}

const SOURCES = { plain: source('LOADER_JS'), wipe: source('LOADER_JS_WIPE') };

export const ORIGIN = 'https://app.example';
export const STOP_CACHE = '__impresspress_sw_stopped';
export const STOP_KEY = '/__impresspress_sw_stopped';
export const BREAKER = '__impresspress_sw_recover';
export const RECOVERY_DONE = '__impresspress_recovery_done';

function storage(initial = {}) {
  const map = new Map(Object.entries(initial));
  return {
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => map.set(k, String(v)),
    removeItem: (k) => map.delete(k),
    clear: () => map.clear(),
    map
  };
}

/// A stub element: enough for `textContent`, `disabled`, and a click.
function element() {
  const listeners = {};
  return {
    textContent: '',
    disabled: false,
    addEventListener: (type, listener) => {
      listeners[type] = listener;
    },
    click: () => listeners.click()
  };
}

/// One page load of the boot shell.
///
/// - `session`   — sessionStorage as the previous load left it
/// - `stop`      — the body sw.js left in Cache Storage (any JSON value), or
///                 `undefined` for no entry
/// - `probe`     — `fetch`'s answer to the boot probe: a `Response`, or a
///                 function returning one / throwing
/// - `wipe`      — the `opfs_wipe_on_recovery` rendering
/// - `now`       — what `Date.now()` returns
/// - `search`    — the query string the shell was loaded with (the boot URL
///                 is `/`, so any makes this load a redirect, not a reload)
/// - `timesOut`  — the boot probe never answers: its 60 s timer fires at once
///                 and `fetch` rejects as an aborted request does
export function loadShell({
  session = {},
  stop,
  probe,
  wipe = false,
  now = 1_000_000,
  search = '',
  timesOut = false
} = {}) {
  const sessionStorage = storage(session);
  const localStorage = storage();
  const status = element();
  status.textContent = 'Loading...';
  const card = { innerHTML: '' };
  const ui = new Map();
  const document = {
    getElementById: (id) => {
      if (id === 'status') return status;
      // The stuck UI's elements exist once its markup has been written.
      if (!card.innerHTML.includes(`id="${id}"`)) return null;
      if (!ui.has(id)) ui.set(id, element());
      return ui.get(id);
    },
    querySelector: (selector) => (selector === '.loader' ? card : null),
    body: card
  };

  const cacheNames = new Set(['assets-v1']);
  if (stop !== undefined) cacheNames.add(STOP_CACHE);
  const caches = {
    has: async (name) => cacheNames.has(name),
    keys: async () => [...cacheNames],
    delete: async (name) => cacheNames.delete(name),
    open: async (name) => ({
      match: async (key) =>
        name === STOP_CACHE && key === STOP_KEY && stop !== undefined
          ? new Response(JSON.stringify(stop))
          : undefined
    })
  };

  const location = {
    href: `${ORIGIN}/${search}`,
    origin: ORIGIN,
    pathname: '/',
    reloads: 0,
    replaced: [],
    reload() {
      this.reloads += 1;
    },
    replace(url) {
      this.replaced.push(url);
    }
  };
  const window = { location };

  const messageListeners = [];
  let registered = 0;
  let unregistered = 0;
  const worker = { state: 'activated', addEventListener: () => {} };
  const serviceWorker = {
    controller: { scriptURL: `${ORIGIN}/sw.js` },
    addEventListener: (type, listener) => {
      if (type === 'message') messageListeners.push(listener);
    },
    register: async () => {
      registered += 1;
      return { active: worker, update: async () => {} };
    },
    getRegistrations: async () => [
      {
        unregister: async () => {
          unregistered += 1;
          return true;
        }
      }
    ]
  };
  const opfs = new Set(['app.sqlite']);
  const navigator = {
    serviceWorker,
    storage: {
      getDirectory: async () => ({
        entries: async function* () {
          for (const name of [...opfs]) yield [name, {}];
        },
        removeEntry: async (name) => opfs.delete(name)
      })
    }
  };

  const probes = [];
  const fetch = async (url, init) => {
    probes.push({ url, init });
    if (timesOut) {
      if (!init.signal.aborted) throw new Error('the probe timer did not abort the request');
      throw new DOMException('The operation was aborted.', 'AbortError');
    }
    const answer = typeof probe === 'function' ? await probe() : probe;
    return answer ?? new Response('<html>', { status: 200 });
  };

  // `reload()` after a good probe is deferred with `setTimeout(…, 0)`; run it
  // at once so a test sees it without waiting.
  // The probe's 60 s abort timer is real but must not hold the process open.
  const setTimeoutStub = (fn, ms) =>
    ms === 0 || timesOut ? (fn(), 0) : setTimeout(fn, ms).unref();
  const DateStub = { now: () => now };
  const consoleStub = { log() {}, warn() {}, error() {} };

  const run = new Function(
    'window',
    'document',
    'navigator',
    'sessionStorage',
    'localStorage',
    'caches',
    'fetch',
    'setTimeout',
    'Date',
    'console',
    SOURCES[wipe ? 'wipe' : 'plain']
  );
  const booted = run(
    window,
    document,
    navigator,
    sessionStorage,
    localStorage,
    caches,
    fetch,
    setTimeoutStub,
    DateStub,
    consoleStub
  );

  return {
    booted,
    status,
    location,
    session: sessionStorage,
    probes,
    /// The stuck UI's element with this id, or `null` if it is not shown.
    stuck: (id) => document.getElementById(id),
    registered: () => registered,
    unregistered: () => unregistered,
    cacheNames: () => [...cacheNames],
    opfs: () => [...opfs],
    /// sw.js posting a message to this page.
    post: (data) => messageListeners.forEach((l) => l({ data }))
  };
}

/// sw.js's answer to a request for a dead runtime.
export function stoppedResponse(cause) {
  return new Response(
    JSON.stringify({ error: 'Unavailable', message: 'x', code: 'runtime_stopped', cause }),
    { status: 503, headers: { 'Content-Type': 'application/json' } }
  );
}
