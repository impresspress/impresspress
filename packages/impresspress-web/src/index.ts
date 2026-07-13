export interface ImpresspressConfig {
  /** URL paths to intercept (default: ['/b/', '/health']) */
  routes?: string[];
  /** Service Worker scope (default: '/') */
  scope?: string;
}

const DEFAULT_ROUTES = ['/b/', '/health', '/openapi.json', '/.well-known/agent.json'];

/**
 * Register a Service Worker that runs the Impresspress WASM backend.
 * All matching requests are intercepted and handled by the WASM runtime.
 */
export { registerWithUpdates } from './update';
export type { UpdateHandle } from './update';

export async function setupImpresspress(config?: ImpresspressConfig): Promise<void> {
  if (!('serviceWorker' in navigator)) {
    throw new Error('Service Workers are not supported in this browser');
  }

  const scope = config?.scope ?? '/';
  const routes = config?.routes ?? DEFAULT_ROUTES;

  const registration = await navigator.serviceWorker.register(
    new URL('./worker.js', import.meta.url),
    { scope, type: 'module', updateViaCache: 'none' }
  );

  // Wait for the SW to be active
  const sw = registration.installing || registration.waiting || registration.active;
  if (sw && sw.state !== 'activated') {
    await new Promise<void>((resolve) => {
      sw.addEventListener('statechange', () => {
        if (sw.state === 'activated') resolve();
      });
      // Re-check after attaching the listener to close the race window
      // where the SW activated between the outer check and addEventListener.
      if (sw.state === 'activated') resolve();
    });
  }

  // Wait for navigator.serviceWorker.ready to guarantee registration.active is non-null.
  const ready = await navigator.serviceWorker.ready;
  ready.active!.postMessage({ type: 'impresspress:config', routes });
}
