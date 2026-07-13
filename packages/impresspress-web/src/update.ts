export interface UpdateHandle {
  registration: ServiceWorkerRegistration;
  /**
   * Subscribe to updates. The callback receives an `apply` function that
   * posts `skip-waiting` to the installed-but-waiting SW; call it when the
   * consumer is ready to switch over (e.g., after user clicks a toast).
   * Returns an unsubscribe function.
   */
  onUpdateReady(cb: (apply: () => Promise<void>) => void): () => void;
  /** Force an update check. Wraps `registration.update()`. */
  checkForUpdate(): Promise<void>;
}

export async function registerWithUpdates(
  scriptURL: string,
  opts?: { scope?: string; type?: WorkerType },
): Promise<UpdateHandle> {
  const registration = await navigator.serviceWorker.register(scriptURL, {
    scope: opts?.scope ?? '/',
    type: opts?.type ?? 'module',
    updateViaCache: 'none',
  });

  const callbacks = new Set<(apply: () => Promise<void>) => void>();

  registration.addEventListener('updatefound', () => {
    const installing = registration.installing;
    if (!installing) return;
    installing.addEventListener('statechange', () => {
      if (installing.state !== 'installed') return;
      // Only treat as "update" when there's an existing controller.
      if (!navigator.serviceWorker.controller) return;
      const apply = () => applyUpdate(registration);
      for (const cb of callbacks) cb(apply);
    });
  });

  return {
    registration,
    onUpdateReady(cb) {
      callbacks.add(cb);
      return () => callbacks.delete(cb);
    },
    async checkForUpdate() {
      await registration.update();
    },
  };
}

function applyUpdate(registration: ServiceWorkerRegistration): Promise<void> {
  const waiting = registration.waiting ?? registration.installing;
  if (!waiting) return Promise.resolve();
  return new Promise<void>((resolve) => {
    const onChange = () => {
      navigator.serviceWorker.removeEventListener('controllerchange', onChange);
      resolve();
    };
    navigator.serviceWorker.addEventListener('controllerchange', onChange);
    waiting.postMessage({ type: 'skip-waiting' });
  });
}
