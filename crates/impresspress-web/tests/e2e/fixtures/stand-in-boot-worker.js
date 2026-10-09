// A stand-in for the bundle's `sw.js`, for `boot-navigation.spec.ts`.
//
// That spec is about the boot shell (`loader.js`), in every engine the suite
// can launch — WebKit included, where the real worker cannot start: the
// runtime keeps its database in OPFS, and Playwright's WebKit build has no
// `navigator.storage`. So this file answers the shell the way `sw.js` does
// in everything the shell relies on, and nothing else:
//
//   * it takes every page as soon as it activates, and again when the shell
//     asks (`impresspress-claim`);
//   * it says which runtime it was built for when asked
//     (`impresspress-runtime`);
//   * it answers a page of the app — the shell's boot probe, or a navigation
//     — only once its "runtime" has started, which is when the host answers
//     `/__start`. Until the test lets that go, both wait, as they wait on a
//     real runtime's start. Files the host has (the shell's scripts) pass
//     through.
//
// Its answer to a page is a document titled `app <path>`, so a test can tell
// which page a tab ended up on and that the worker rendered it.
//
// It is loaded as `{ type: 'module' }`, like the real worker.

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));

self.addEventListener('message', (event) => {
  if (event.data?.type === 'impresspress-claim') event.waitUntil(self.clients.claim());
  if (event.data?.type === 'impresspress-runtime') {
    event.ports[0].postMessage({ runtime: '/stand-in.wasm' });
  }
});

/** Whether `url` is a page of the app: no file extension in its last segment. */
function isPage(url) {
  const last = url.pathname.split('/').pop();
  return !last.includes('.');
}

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);
  if (url.origin !== self.location.origin || url.pathname === '/__start' || !isPage(url)) return;
  event.respondWith(
    (async () => {
      await fetch('/__start', { cache: 'no-store' });
      const title = `app ${url.pathname}`;
      return new Response(`<!doctype html><title>${title}</title><p>${title}</p>`, {
        headers: { 'Content-Type': 'text/html; charset=utf-8' },
      });
    })(),
  );
});
