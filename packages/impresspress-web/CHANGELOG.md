# Changelog

## 0.2.0

### Breaking changes

- `worker.ts` no longer calls `self.skipWaiting()` during `install`. Consumers who want the old behavior should post `{ type: 'skip-waiting' }` to the registration from the main thread after `register()` resolves, or (recommended) use the new `registerWithUpdates` helper.

### New

- `registerWithUpdates(scriptURL, opts?)` — registers the SW and returns a handle with `onUpdateReady` and `checkForUpdate` for wiring update UX.
- `UpdateHandle` type re-exported from the package root.

## 0.1.0

- Initial release.
