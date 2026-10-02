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
export const RECOVERY_LOCK = '__impresspress_recovery';
export const RECOVERED_CACHE = '__impresspress_recovered';
export const RECOVERED_KEY = '/__impresspress_recovered';

function storage(initial = {}) {
  const map = new Map(Object.entries(initial));
  // Every value a key was set to, in order: a recovery and the probe that
  // ends it happen on one load now, so what a flag WAS is read here.
  const writes = [];
  return {
    writes,
    getItem: (k) => (map.has(k) ? map.get(k) : null),
    setItem: (k, v) => {
      writes.push([k, String(v)]);
      map.set(k, String(v));
    },
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
/// - `path`      — the path the shell was loaded at; `/` is its own, anything
///                 else is a shell sw.js answered a dead-runtime navigation
///                 with
/// - `search`    — the query string the shell was loaded with (the boot URL
///                 is `/`, so any makes this load a redirect, not a reload)
/// - `title`     — what the page's `[data-app-title]` element shows, or `null`
///                 for a page without one; `documentTitle` is `<title>`
/// - `timesOut`  — how many boot probes in a row never answer (`true`: all
///                 of them): the 60 s timer of each fires at once and its
///                 `fetch` rejects as an aborted request does
/// - `controlled` — what controls the page: `true`, the registered worker;
///                 `false`, nothing; `'dead'`, a worker that is not the
///                 registered one (a dead one another tab unregistered)
/// - `claims`    — whether the registered worker takes the page when asked
///                 to; `false` is a worker that never does, and the wait for
///                 it runs out at once
/// - `recovered` — the deaths already recorded as recovered from on this
///                 origin: `[{ id, at }]`
/// - `locks`     — whether the browser has Web Locks
/// - `registeredUrl` — the script URL of the worker the origin already has
///                 registered, if it has one
/// - `installs`  — whether a newly registered worker installs; `false` is one
///                 the browser discards (its state is `redundant`)
/// - `eraseFails` — OPFS entries that cannot be removed
/// - `opfsFiles` — the OPFS entries there are
/// - `onProbe`   — called when the probe is made, with `post` (sw.js posting a
///                 message to this page), before the probe is answered
/// - `registerFails` — `navigator.serviceWorker.register` rejects with this
export function loadShell({
  session = {},
  stop,
  probe,
  wipe = false,
  now = 1_000_000,
  path = '/',
  search = '',
  timesOut = false,
  onProbe,
  registerFails,
  controlled = true,
  claims = true,
  recovered,
  locks = true,
  eraseFails = [],
  registeredUrl,
  installs = true,
  opfsFiles = ['app.sqlite'],
  title = 'Kiln & Co',
  documentTitle = title
} = {}) {
  const sessionStorage = storage(session);
  const localStorage = storage();
  // `#status`, keeping every line written to it.
  const statusLines = [];
  const status = {
    ...element(),
    get textContent() {
      return statusLines.length ? statusLines[statusLines.length - 1] : 'Loading...';
    },
    set textContent(text) {
      statusLines.push(text);
    }
  };
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
    // `index.html` shows the title inside the card, so it is there until the
    // stuck UI replaces the card's content. `title: null` is a page with no
    // such element (a deployment's own boot page).
    querySelector: (selector) => {
      if (selector === '.loader') return card;
      if (selector === '[data-app-title]' && title !== null && card.innerHTML === '') {
        return { textContent: title };
      }
      return null;
    },
    title: documentTitle,
    body: card
  };

  // Cache Storage: cache name → (key → JSON body).
  const cacheStore = new Map([['assets-v1', new Map()]]);
  if (stop !== undefined) cacheStore.set(STOP_CACHE, new Map([[STOP_KEY, stop]]));
  if (recovered !== undefined) {
    cacheStore.set(RECOVERED_CACHE, new Map([[RECOVERED_KEY, { deaths: recovered }]]));
  }
  const caches = {
    has: async (name) => cacheStore.has(name),
    keys: async () => [...cacheStore.keys()],
    delete: async (name) => cacheStore.delete(name),
    open: async (name) => {
      if (!cacheStore.has(name)) cacheStore.set(name, new Map());
      const cache = cacheStore.get(name);
      return {
        match: async (key) =>
          cache.has(key) ? new Response(JSON.stringify(cache.get(key))) : undefined,
        put: async (key, response) => {
          cache.set(key, await response.json());
        }
      };
    }
  };

  const location = {
    href: `${ORIGIN}${path}${search}`,
    origin: ORIGIN,
    pathname: path,
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
  const controlListeners = [];
  const registeredUrls = [];
  const asked = [];
  const worker = {
    state: installs ? 'activated' : 'redundant',
    addEventListener: () => {},
    // The page asking the worker to take it.
    postMessage: (message) => {
      asked.push(message);
      if (!claims || message.type !== 'impresspress-claim') return;
      queueMicrotask(() => {
        controller = worker;
        controlListeners.forEach((l) => l({}));
      });
    }
  };
  let controller = null;
  if (controlled === true) controller = worker;
  if (controlled === 'dead') controller = { state: 'activated', scriptURL: `${ORIGIN}/sw.js` };
  const serviceWorker = {
    get controller() {
      return controller;
    },
    addEventListener: (type, listener) => {
      if (type === 'message') messageListeners.push(listener);
      if (type === 'controllerchange') controlListeners.push(listener);
    },
    register: async (url) => {
      if (registerFails) throw registerFails;
      registered += 1;
      registeredUrls.push(url);
      return { active: worker, update: async () => {} };
    },
    // The registration the origin already has, if any: `registeredUrl` is
    // its worker's script URL.
    getRegistration: async () =>
      registeredUrl === undefined ? undefined : { active: { scriptURL: registeredUrl } },
    getRegistrations: async () => [
      {
        unregister: async () => {
          unregistered += 1;
          return true;
        }
      }
    ]
  };
  const opfs = new Set(opfsFiles);
  const lockRequests = [];
  const navigator = {
    serviceWorker,
    storage: {
      getDirectory: async () => ({
        entries: async function* () {
          for (const name of [...opfs]) yield [name, {}];
        },
        removeEntry: async (name) => {
          if (eraseFails.includes(name)) {
            throw new DOMException('the file is in use', 'NoModificationAllowedError');
          }
          opfs.delete(name);
        }
      })
    }
  };
  if (locks) {
    // One tab, so the lock is always free: what a test reads is that the
    // work was done holding it.
    let held = false;
    navigator.locks = {
      request: async (name, act) => {
        if (held) throw new Error('the recovery lock was requested while held');
        lockRequests.push({ name, registrations: registered, unregistered, opfs: [...opfs] });
        held = true;
        try {
          return await act();
        } finally {
          held = false;
        }
      }
    };
  }

  const post = (data) => messageListeners.forEach((l) => l({ data }));
  // Whether the probe with this index (0 for the first) runs out of time.
  const probeTimesOut = (index) => timesOut === true || index < Number(timesOut);
  const probes = [];
  const fetch = async (url, init) => {
    const outOfTime = probeTimesOut(probes.length);
    probes.push({ url, init });
    if (onProbe) onProbe({ post });
    if (outOfTime) {
      if (!init.signal.aborted) throw new Error('the probe timer did not abort the request');
      throw new DOMException('The operation was aborted.', 'AbortError');
    }
    const answer = typeof probe === 'function' ? await probe() : probe;
    return answer ?? new Response('<html>', { status: 200 });
  };

  // `reload()` after a good probe is deferred with `setTimeout(…, 0)`; run it
  // at once so a test sees it without waiting.
  // The probe's 60 s abort timer is set just before its `fetch`, so the probe
  // it belongs to is the next one: fired at once for a probe that is to run
  // out of time, real otherwise — but never holding the process open.
  // The 10 s wait for control runs out at once for a worker that never
  // claims.
  const setTimeoutStub = (fn, ms) => {
    const now = ms === 0 || (ms === 10_000 ? !claims : probeTimesOut(probes.length));
    return now ? (fn(), 0) : setTimeout(fn, ms).unref();
  };
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
    /// Every line written to `#status`, in order.
    statusLines,
    /// What `sessionStorage` key `key` was set to, in order.
    written: (key) => sessionStorage.writes.filter(([k]) => k === key).map(([, v]) => v),
    location,
    session: sessionStorage,
    probes,
    /// The stuck UI's element with this id, or `null` if it is not shown.
    stuck: (id) => document.getElementById(id),
    registered: () => registered,
    unregistered: () => unregistered,
    cacheNames: () => [...cacheStore.keys()],
    /// The ids of the deaths recorded as recovered from.
    recovered: () =>
      (cacheStore.get(RECOVERED_CACHE)?.get(RECOVERED_KEY)?.deaths ?? []).map((d) => d.id),
    /// Each request for the recovery lock, with the state it was made in.
    lockRequests,
    /// What the page asked the registered worker.
    asked,
    /// Another tab recording, in the origin's Cache Storage, that it has
    /// recovered from the death `id`.
    recordElsewhere: async (id) => {
      const cache = await caches.open(RECOVERED_CACHE);
      await cache.put(RECOVERED_KEY, new Response(JSON.stringify({ deaths: [{ id, at: now }] })));
    },
    /// The script URLs this load registered.
    registeredUrls,
    /// The record of deaths recovered from, whole.
    recoveryRecord: () => cacheStore.get(RECOVERED_CACHE)?.get(RECOVERED_KEY)?.deaths ?? [],
    opfs: () => [...opfs],
    /// sw.js posting a message to this page.
    post
  };
}

/// sw.js's answer to a request for a dead runtime. `stage` is left out of the
/// body when not given, as a worker from before stages existed would.
export function stoppedResponse(cause, stage) {
  return new Response(
    JSON.stringify({ error: 'Unavailable', message: 'x', code: 'runtime_stopped', cause, stage }),
    { status: 503, headers: { 'Content-Type': 'application/json' } }
  );
}
