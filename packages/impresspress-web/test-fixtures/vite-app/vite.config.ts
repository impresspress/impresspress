import { defineConfig } from 'vite';

export default defineConfig({
  build: {
    outDir: 'dist',
    rollupOptions: {
      input: {
        main: './index.html',
        sw: './src/sw.ts',
      },
      output: {
        entryFileNames: (info) => info.name === 'sw' ? 'sw.js' : 'assets/[name]-[hash].js',
      },
      // sql-wasm-esm.js is a runtime-served asset (not bundled) referenced by an absolute
      // path in the WASM bridge. Externalize it so Rollup doesn't try to resolve it.
      external: ['/sql-wasm-esm.js'],
    },
  },
});
