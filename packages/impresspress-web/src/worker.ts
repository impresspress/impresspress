// Re-export the WASM module's initialize and handleRequest for composable mode.
// Developers who have an existing SW can import these directly.

import init, { initialize as wasmInitialize, handle_request as wasmHandleRequest } from './wasm/impresspress_web.js';

let initialized = false;
let initPromise: Promise<void> | null = null;
let routes: string[] = ['/b/', '/health', '/openapi.json', '/.well-known/agent.json'];

/**
 * Initialize the Impresspress WASM runtime.
 * Safe to call multiple times, including concurrently (e.g. two `fetch`
 * events arriving before the first `initialize()` call resolves) — only
 * the first call does work; every other caller awaits the same in-flight
 * promise instead of racing a second `init()`/`wasmInitialize()` pass.
 */
export async function initialize(): Promise<void> {
  if (initialized) return;
  if (initPromise) return initPromise;

  initPromise = (async () => {
    await init();
    await wasmInitialize();
    initialized = true;
  })();

  return initPromise;
}

/**
 * Handle an incoming fetch request through the Impresspress WASM runtime.
 */
export async function handleRequest(request: Request): Promise<Response> {
  if (!initialized) {
    return new Response('Impresspress not initialized', { status: 503 });
  }
  return await wasmHandleRequest(request);
}

/**
 * Check if a URL path should be handled by Impresspress.
 *
 * Exact route-boundary matching, not a bare `startsWith`: a route ending in
 * `/` (e.g. `/b/`) matches itself and anything nested under it; a route
 * with no trailing slash (e.g. `/health`) matches only exactly or a nested
 * path under it (`/health/x`) — never a same-prefix sibling like
 * `/healthfoo`, which `pathname.startsWith('/health')` would wrongly allow.
 */
function shouldIntercept(pathname: string): boolean {
  return routes.some((route) => matchesRoute(pathname, route));
}

function matchesRoute(pathname: string, route: string): boolean {
  if (route.endsWith('/')) {
    return pathname === route.slice(0, -1) || pathname.startsWith(route);
  }
  return pathname === route || pathname.startsWith(`${route}/`);
}

// --- Batteries-included SW entry point ---
// When this file is loaded as a Service Worker directly, it auto-initializes
// and intercepts matching fetch events.

declare const self: ServiceWorkerGlobalScope;

if (typeof ServiceWorkerGlobalScope !== 'undefined') {
  self.addEventListener('install', (event) => {
    event.waitUntil(initialize());
    // NOTE: no skipWaiting here. Consumers opt in by posting
    // { type: 'skip-waiting' } from the main thread when they want to
    // apply an update. The standalone pkg/ site uses its own sw.js
    // which does call skipWaiting.
  });

  self.addEventListener('activate', (event) => {
    event.waitUntil(self.clients.claim());
  });

  self.addEventListener('message', (event) => {
    if (event.data?.type === 'skip-waiting') {
      self.skipWaiting();
      return;
    }
    if (event.data?.type === 'impresspress:config' && Array.isArray(event.data.routes)) {
      routes = event.data.routes;
    }
  });

  self.addEventListener('fetch', (event) => {
    const url = new URL(event.request.url);
    if (shouldIntercept(url.pathname)) {
      event.respondWith(handleRequest(event.request));
    }
  });
}
