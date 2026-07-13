import { describe, it, expect, beforeEach, vi } from 'vitest';
import { registerWithUpdates } from './update';

type Listener = (event: any) => void;

function makeFakeWorker() {
  const listeners: Record<string, Listener[]> = {};
  const postMessage = vi.fn();
  return {
    state: 'installing' as 'installing' | 'installed' | 'activating' | 'activated',
    postMessage,
    addEventListener(ev: string, cb: Listener) { (listeners[ev] ||= []).push(cb); },
    removeEventListener(ev: string, cb: Listener) {
      listeners[ev] = (listeners[ev] || []).filter(l => l !== cb);
    },
    _fire(ev: string, data: any = {}) { (listeners[ev] || []).forEach(l => l(data)); },
  };
}

function makeRegistration(installing: any = null, waiting: any = null) {
  const listeners: Record<string, Listener[]> = {};
  return {
    installing, waiting,
    update: vi.fn().mockResolvedValue(undefined),
    addEventListener(ev: string, cb: Listener) { (listeners[ev] ||= []).push(cb); },
    _fire(ev: string, data: any = {}) { (listeners[ev] || []).forEach(l => l(data)); },
  };
}

describe('registerWithUpdates', () => {
  beforeEach(() => {
    const waitingWorker = makeFakeWorker();
    const registration = makeRegistration(null, waitingWorker);
    const fakeNavigator = {
      serviceWorker: {
        register: vi.fn().mockResolvedValue(registration),
        controller: { postMessage: vi.fn() } as any,
        addEventListener: vi.fn(),
        removeEventListener: vi.fn(),
      },
    };
    Object.defineProperty(globalThis, 'navigator', {
      value: fakeNavigator,
      writable: true,
      configurable: true,
    });
    (globalThis as any)._fakes = { waitingWorker, registration };
  });

  it('resolves to a handle exposing the registration', async () => {
    const handle = await registerWithUpdates('/sw.js');
    expect(handle.registration).toBe((globalThis as any)._fakes.registration);
  });

  it('does not fire updateReady on first install (no existing controller)', async () => {
    navigator.serviceWorker.controller = null as any;
    const handle = await registerWithUpdates('/sw.js');
    const cb = vi.fn();
    handle.onUpdateReady(cb);
    const { registration } = (globalThis as any)._fakes;
    const newWorker = makeFakeWorker();
    registration.installing = newWorker;
    registration._fire('updatefound');
    newWorker.state = 'installed';
    newWorker._fire('statechange');
    expect(cb).not.toHaveBeenCalled();
  });

  it('fires updateReady when a new worker installs while an old one controls', async () => {
    const handle = await registerWithUpdates('/sw.js');
    const cb = vi.fn();
    handle.onUpdateReady(cb);
    const { registration } = (globalThis as any)._fakes;
    const newWorker = makeFakeWorker();
    registration.installing = newWorker;
    registration._fire('updatefound');
    newWorker.state = 'installed';
    newWorker._fire('statechange');
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('apply() posts skip-waiting to the waiting worker', async () => {
    const handle = await registerWithUpdates('/sw.js');
    const cb = vi.fn();
    handle.onUpdateReady(cb);
    const { registration } = (globalThis as any)._fakes;
    const newWorker = makeFakeWorker();
    registration.installing = newWorker;
    registration._fire('updatefound');
    newWorker.state = 'installed';
    registration.waiting = newWorker;
    newWorker._fire('statechange');
    const apply = cb.mock.calls[0][0];
    apply();
    expect(newWorker.postMessage).toHaveBeenCalledWith({ type: 'skip-waiting' });
  });

  it('checkForUpdate() calls registration.update()', async () => {
    const handle = await registerWithUpdates('/sw.js');
    await handle.checkForUpdate();
    expect((globalThis as any)._fakes.registration.update).toHaveBeenCalled();
  });
});
