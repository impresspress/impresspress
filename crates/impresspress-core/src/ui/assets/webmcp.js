// Registers this site's WebMCP tools with the browser agent.
//
// The manifest is generated server-side from each block's endpoint
// declarations and filtered to this session's auth level, so whatever
// arrives here is exactly what this visitor is allowed to invoke. The
// script's only job is translating that into registerTool calls.

// Browsers without WebMCP get nothing. This ships on every page, so it
// must never throw on an unsupported browser — or on an insecure page,
// where Chrome does not expose `navigator.modelContext` at all
// (`'modelContext' in navigator` is false, which is what the guard reads).
if (!('modelContext' in navigator) || typeof navigator.modelContext.registerTool !== 'function') {
  return;
}

function register(tool) {
  navigator.modelContext.registerTool(toolOptions(tool));
}

// Names this script itself registered, so `refresh()` can drop exactly what
// it added rather than guessing at the agent's whole tool set. `generation`
// counts completed `load()` calls — a caller can poll it to notice a refresh
// landed without threading a callback through `refresh()`'s promise.
var registered = [];
var generation = 0;
// Whether a `load()` is in flight. A refresh empties `registered` before it
// fetches, so for that while the list says nothing about what the site
// offers — `count()` reports no number until the load has settled.
var loading = false;

// Dispatched on `window` each time a `load()` settles, after `generation`
// has counted it. A page that states how many tools its tab has — the
// `/b/dev` workspace, whose own tools sit alongside these — listens for it,
// because it cannot know whether this script runs before or after its own,
// nor when a refresh changes the set.
var LOADED_EVENT = 'impresspress:webmcp-loaded';

function unregisterAll() {
  if (typeof navigator.modelContext.unregisterTool === 'function') {
    registered.forEach(function (name) {
      try {
        navigator.modelContext.unregisterTool(name);
      } catch (e) {
        // already gone
      }
    });
  }
  registered = [];
}

function load() {
  loading = true;
  return fetch('/b/webmcp/manifest.json', { credentials: 'same-origin' })
    .then(function (r) { return r.ok ? r.json() : null; })
    .then(function (manifest) {
      if (!manifest || !Array.isArray(manifest.tools)) {
        return;
      }
      // Per-tool try/catch: `registerTool` throwing on one malformed tool
      // must not abort registration of every tool after it (the outer
      // `.catch` would swallow it silently, leaving a half-registered page).
      manifest.tools.forEach(function (tool) {
        try {
          register(tool);
          registered.push(tool.name);
        } catch (e) {
          // One tool the browser rejected is not a reason to lose the rest.
        }
      });
    })
    .catch(function () {
      // A failed manifest fetch means no tools. That is a degraded page,
      // not a broken one — never surface it to the visitor.
    })
    .then(function () {
      // Bumped on EVERY settled load, degraded ones included, and after the
      // `.catch` so nothing above can skip it. `generation` counts completed
      // `load()` calls — a poller waiting for a refresh to land is waiting
      // for the call to finish, not for it to find tools, and a load that
      // ends with zero tools has finished. Bumping only on the success path
      // would hang every such poller (`webmcp.spec.ts` is one) on exactly
      // the degraded page this file otherwise takes care to tolerate.
      generation += 1;
      loading = false;
      window.dispatchEvent(new Event(LOADED_EVENT));
    });
}

// Drops every tool this script registered and re-fetches the manifest. A
// page that changes what it can offer mid-session (e.g. the dev sandbox's
// workspace tools, which come and go with what is loaded) calls this instead
// of forcing a reload — the alternative is stale tool descriptions the agent
// can no longer act on, or worse, ones that still work but no longer mean
// what their description says.
function refresh() {
  unregisterAll();
  return load();
}

window.__impresspressWebmcp = {
  refresh: refresh,
  generation: function () { return generation; },
  // How many tools this script's last completed load registered with the
  // browser — only the ones the browser accepted (`load` keeps no others) —
  // or `null` before the first load has settled and while a refresh is in
  // flight, when there is no such number to state.
  count: function () { return loading || generation === 0 ? null : registered.length; }
};

var sw = navigator.serviceWorker;

// Resolves once this document is CONTROLLED by a service worker — not
// merely once one is active.
//
// The distinction is the whole point. `navigator.serviceWorker.ready`
// resolves on `registration.active`, which is populated at the *activating*
// state, while `sw.js.tmpl` calls `clients.claim()` inside its `activate`
// handler's `waitUntil`. So `ready` can resolve before the claim has taken
// effect, and a fetch issued in that window goes to the network rather than
// to the wasm router. On a host with SPA fallback (`not_found_handling =
// "single-page-application"`, which `examples/dev-sandbox/wrangler.toml`
// sets) the network answers `index.html` with **200** — `r.ok` is true,
// `r.json()` throws, the `.catch` swallows it, and the page silently ends up
// with no tools at all. `controller` is the signal that actually means "my
// fetches reach the worker"; `controllerchange` is when it arrives.
function whenControlled() {
  if (sw.controller) {
    return Promise.resolve();
  }
  return new Promise(function (resolve) {
    sw.addEventListener('controllerchange', function onChange() {
      sw.removeEventListener('controllerchange', onChange);
      resolve();
    });
  });
}

if (sw && sw.controller) {
  // Already controlled — a repeat visit. Nothing to wait for.
  load();
} else if (sw) {
  // In a service-worker build the first paint beats the worker: the manifest
  // route (`/b/webmcp/manifest.json`) is served by the worker, so fetching
  // it before the worker controls the page misses the wasm router (see
  // `whenControlled`).
  //
  // A native server also exposes `navigator.serviceWorker` (it is a
  // standard browser API, not something the SW build adds), but there is no
  // registration and no worker that will ever claim this page — waiting for
  // one would hang forever. `getRegistration()` tells the two apart: it
  // resolves with `undefined` when nothing is registered, so the native path
  // falls through to `load()` immediately.
  sw.getRegistration()
    .then(function (r) { return r ? whenControlled() : null; })
    .then(load, load);
} else {
  // No Service Worker support at all (or it was stripped by the embedder).
  load();
}
