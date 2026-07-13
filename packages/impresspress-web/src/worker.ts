// Re-export the WASM module's initialize and handleRequest for composable mode.
// Developers who have an existing SW can import these directly.

import init, { initialize as wasmInitialize, handle_request as wasmHandleRequest } from './wasm/impresspress_web.js';

let initialized = false;
let routes: string[] = ['/b/', '/health', '/openapi.json', '/.well-known/agent.json'];

/**
 * Initialize the Impresspress WASM runtime.
 * Call once before handling requests.
 */
export async function initialize(): Promise<void> {
  if (initialized) return;
  await init();
  await wasmInitialize();
  initialized = true;
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
 */
function shouldIntercept(pathname: string): boolean {
  return routes.some((route) => pathname.startsWith(route));
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
