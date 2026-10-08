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
/// The runtime binary the registered worker was built for — the version a
/// death in these tests is a death of — and the one a newer version an update
/// brings in was built for. Each worker answers its own when asked
/// (`impresspress-runtime`).
export const OLD_RUNTIME = '/app_bg-aaaaaaaa.wasm';
export const NEW_RUNTIME = '/app_bg-bbbbbbbb.wasm';

/// A worker answering the shell's `impresspress-runtime` question.
function answerRuntime(message, ports, runtime) {
  if (message.type === 'impresspress-runtime') ports[0].postMessage({ runtime });
}

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
///                 registered, if it has one; or `{ installing, waiting,
///                 active }`, a script URL for each version there is
/// - `installs`  — whether a newly registered worker installs; `false` is one
///                 the browser discards (its state is `redundant`)
/// - `eraseFails` — OPFS entries that cannot be removed
/// - `opfsFiles` — the OPFS entries there are
/// - `onProbe`   — called when the probe is made, with `post` (sw.js posting a
///                 message to this page) and `leave` (a navigation away from
///                 this page beginning — a link, the address bar, an agent's
///                 `goto`), before the probe is answered
/// - `registerFails` — `navigator.serviceWorker.register` rejects with this
/// - `update`    — what the registration's update check finds: `'installs'`,
///                 a newer version that installs and then activates (taking
///                 the page); `'fails'`, one the browser discards while
///                 installing; `'active'`, a newer version that had already
///                 activated before this page asked — and, where the page
///                 is `controlled`, already took it; nothing, by default
/// - `answersRuntime` — whether the registered (older) worker answers the
///                 shell's question about its version; `false` is a worker
///                 from before the question existed, and the wait for its
///                 answer runs out at once
/// - `heldElsewhere` — the Web Locks another tab holds: a request for one
///                 asked only if available gets `null`
/// - `installs: 'stalls'` — a newly registered worker that installs and then
///                 never activates (as Chromium has been seen to leave one);
///                 the wait for it runs out at once. `'late'`: one that
///                 activates a moment after that wait has run out
/// - `secure`    — whether the page is a secure context (`window
///                 .isSecureContext`); `false` is the same shell served over
///                 plain http at a LAN address
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
  update,
  answersRuntime = true,
  heldElsewhere = [],
  opfsFiles = ['app.sqlite'],
  title = 'Kiln & Co',
  documentTitle = title,
  secure = true
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
  // The page's own listeners, by event type: `leave` fires `beforeunload`
  // on them, which is what a browser does the moment a navigation away from
  // this document begins.
  const windowListeners = [];
  const window = {
    location,
    isSecureContext: secure,
    addEventListener: (type, listener) => windowListeners.push({ type, listener })
  };
  const leave = () =>
    windowListeners.filter((l) => l.type === 'beforeunload').forEach((l) => l.listener({}));
  // This document coming back from the back/forward cache.
  const comeBack = () =>
    windowListeners
      .filter((l) => l.type === 'pageshow')
      .forEach((l) => l.listener({ persisted: true }));

  const messageListeners = [];
  let registered = 0;
  let unregistered = 0;
  const controlListeners = [];
  // Registrations and erasures, in the order they happened.
  const events = [];
  const registeredUrls = [];
  const asked = [];
  const workerListeners = new Set();
  const worker = {
    state:
      installs === 'stalls' || installs === 'late' ? 'installed' : installs ? 'activated' : 'redundant',
    addEventListener: (type, l) => type === 'statechange' && workerListeners.add(l),
    removeEventListener: (type, l) => workerListeners.delete(l),
    // The page asking the worker to take it.
    postMessage: (message, ports) => {
      if (answersRuntime) answerRuntime(message, ports, OLD_RUNTIME);
      asked.push(message);
      if (!claims || message.type !== 'impresspress-claim') return;
      queueMicrotask(() => {
        controller = worker;
        controlListeners.forEach((l) => l({}));
      });
    }
  };
  let controller = null;
  let registration = null;
  let updates = 0;
  // A version of the registration other than the stub `worker`: it keeps
  // its own state, tells its listeners when that changes, and takes the page
  // when asked to.
  const version = (state) => {
    const listeners = new Set();
    const next = {
      state,
      fire: () => [...listeners].forEach((l) => l({})),
      scriptURL: `${ORIGIN}/sw.js`,
      addEventListener: (type, l) => type === 'statechange' && listeners.add(l),
      removeEventListener: (type, l) => listeners.delete(l),
      postMessage: (message, ports) => {
        answerRuntime(message, ports, NEW_RUNTIME);
        asked.push(message);
        if (message.type !== 'impresspress-claim') return;
        queueMicrotask(() => {
          controller = next;
          controlListeners.forEach((l) => l({}));
        });
      }
    };
    return next;
  };
  // The newer version already in place (`update: 'active'`), and the page
  // it took, if any.
  const already = update === 'active' ? version('activated') : null;
  // The newer version an update check finds (`update`): `installing` at
  // first, then — a turn later — installed and activated, taking the page;
  // or discarded.
  const incoming = () => {
    const next = version('installing');
    const fire = next.fire;
    setTimeout(() => {
      events.push(update === 'installs' ? 'update installed' : 'update discarded');
      registration.installing = null;
      if (update !== 'installs') {
        next.state = 'redundant';
        fire();
        return;
      }
      next.state = 'installed';
      registration.waiting = next;
      fire();
      setTimeout(() => {
        registration.waiting = null;
        registration.active = next;
        next.state = 'activated';
        fire();
      }, 1);
    }, 1);
    return next;
  };
  if (controlled === true) controller = already ?? worker;
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
      registeredHolding.push([...held]);
      if (registerFails) throw registerFails;
      registered += 1;
      registeredUrls.push(url);
      events.push(`register ${url}`);
      if (installs === 'late') {
        // Activates a moment after the wait for it has already run out.
        setTimeout(() => {
          events.push('activated late');
          worker.state = 'activated';
          [...workerListeners].forEach((l) => l({}));
        }, 20);
      }
      return { active: worker, update: async () => {} };
    },
    // The registration the origin already has, if any: `registeredUrl` is
    // its worker's script URL. Its active worker is the one that controls
    // the page, unless `update` is `'active'`.
    getRegistration: async () => {
      if (registeredUrl === undefined) return undefined;
      if (registration) return registration;
      const urls = typeof registeredUrl === 'string' ? { active: registeredUrl } : registeredUrl;
      registration = Object.fromEntries(
        Object.entries(urls).map(([slot, scriptURL]) => [
          slot,
          slot === 'active' && update === 'active'
            ? already
            : slot === 'active' && controlled === true
              ? Object.assign(worker, { scriptURL })
              : {
                  scriptURL,
                  addEventListener: () => {},
                  removeEventListener: () => {},
                  postMessage: (message, ports) => {
                    if (answersRuntime) answerRuntime(message, ports, OLD_RUNTIME);
                  }
                }
        ])
      );
      registration.installing ??= null;
      registration.waiting ??= null;
      registration.update = async () => {
        updates += 1;
        if (update === 'installs' || update === 'fails') registration.installing = incoming();
      };
      return registration;
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
  const opfs = new Set(opfsFiles);
  const lockRequests = [];
  // The locks held at each OPFS removal, and at each registration.
  const erasedHolding = [];
  const registeredHolding = [];
  const navigator = {
    serviceWorker,
    storage: {
      getDirectory: async () => ({
        entries: async function* () {
          for (const name of [...opfs]) yield [name, {}];
        },
        removeEntry: async (name) => {
          events.push(`erase ${name}`);
          erasedHolding.push([...held]);
          if (eraseFails.includes(name)) {
            throw new DOMException('the file is in use', 'NoModificationAllowedError');
          }
          opfs.delete(name);
        }
      })
    }
  };
  // The Web Locks this tab holds now, by name.
  const held = new Set();
  if (locks) {
    // One tab, so a lock is always free: what a test reads is that the
    // work was done holding it. The recovery lock's requests are recorded
    // with the state they were made in.
    navigator.locks = {
      request: async (name, options, callback) => {
        const act = typeof options === 'function' ? options : callback;
        const ifAvailable = typeof options === 'object' && options.ifAvailable;
        if (heldElsewhere.includes(name) || held.has(name)) {
          if (ifAvailable) return act(null);
          throw new Error(`the lock ${name} was requested while held`);
        }
        if (name === RECOVERY_LOCK) {
          lockRequests.push({ name, registrations: registered, unregistered, opfs: [...opfs] });
        }
        held.add(name);
        try {
          return await act({ name });
        } finally {
          held.delete(name);
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
    if (onProbe) onProbe({ post, leave });
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
    const now =
      ms === 0 ||
      (ms === 10_000
        ? !claims || installs === 'stalls' || installs === 'late'
        : ms === 2_000
          ? !answersRuntime
          : probeTimesOut(probes.length));
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
    /// The locks this tab held at each OPFS removal, and at each
    /// registration (made or refused), and those it holds now.
    erasedHolding,
    registeredHolding,
    heldNow: () => [...held],
    /// What the page asked the registered worker.
    asked,
    /// Another tab recording, in the origin's Cache Storage, that it has
    /// recovered from the death `id`.
    recordElsewhere: async (id) => {
      const cache = await caches.open(RECOVERED_CACHE);
      await cache.put(RECOVERED_KEY, new Response(JSON.stringify({ deaths: [{ id, at: now }] })));
    },
    /// Registrations (`register <url>`) and OPFS removals (`erase <name>`),
    /// in the order they happened.
    events,
    /// The script URLs this load registered.
    registeredUrls,
    /// How many update checks were asked of the existing registration.
    updates: () => updates,
    /// The worker that controls the page now.
    controller: () => controller,
    /// The record of deaths recovered from, whole.
    recoveryRecord: () => cacheStore.get(RECOVERED_CACHE)?.get(RECOVERED_KEY)?.deaths ?? [],
    opfs: () => [...opfs],
    /// sw.js posting a message to this page.
    post,
    /// A navigation away from this page beginning.
    leave,
    /// This page restored from the back/forward cache.
    comeBack
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
