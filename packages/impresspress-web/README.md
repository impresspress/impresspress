# impresspress-web

Impresspress backend running in the browser via Service Worker + WASM.

## Bundler integration

The package ships wasm-pack output unmodified at `dist/wasm/`. The embedded glue uses `new URL('impresspress_web_bg.wasm', import.meta.url)`, which Vite, Rollup, webpack 5, and esbuild all detect and bundle as a hashed asset automatically.

### Vite / Rollup
No config required in typical setups. The `.wasm` will be emitted to `dist/assets/` with a content hash.

### webpack 5
Make sure `experiments.asyncWebAssembly` is enabled in your webpack config; it handles the URL pattern out of the box.

### esbuild
Add a `.wasm` file loader:

```js
build({
  loader: { '.wasm': 'file' },
  // ...
});
```

## Service Worker update lifecycle

The exported `worker.ts` does **not** call `skipWaiting()` during install so consumers can control when an update takes effect. Three common patterns:

### Silent (pick up on next navigation)

```ts
import { registerWithUpdates } from 'impresspress-web';

await registerWithUpdates('/sw.js');
// Do nothing else. Next hard navigation uses the new SW.
```

### Auto-reload on update

```ts
import { registerWithUpdates } from 'impresspress-web';

const handle = await registerWithUpdates('/sw.js');
handle.onUpdateReady(async (apply) => {
  await apply();
  location.reload();
});
```

### Toast + opt-in reload

```ts
import { registerWithUpdates } from 'impresspress-web';

const handle = await registerWithUpdates('/sw.js');
handle.onUpdateReady((apply) => {
  showToast('New version available', async () => {
    await apply();
    location.reload();
  });
});
```
