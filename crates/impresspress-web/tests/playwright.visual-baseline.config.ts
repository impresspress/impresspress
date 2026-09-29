import { defineConfig } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import baseConfig from './playwright.config.ts';

const HERE = dirname(fileURLToPath(import.meta.url));

// Visual-baseline tests run against the native impresspress server (port 8093 in
// CI, 8090 by default locally) and require an admin session. `global-setup.ts`
// performs ONE login per run, stores the cookie via Playwright `storageState`,
// and admin describes opt-in via `test.use({ storageState: ADMIN_STATE_PATH })`.
//
// Smoke tests (port 8080 static-file server) cannot use this config because
// they don't run a server with a login API — see `playwright.config.ts`.
//
// `--disable-partial-raster` keeps the screenshots deterministic. With
// partial raster, Chromium re-rasterizes only the invalidated part of a tile
// and keeps the rest, so an edge's anti-aliasing depends on which
// invalidations happened to land first. On the tall captures
// `visual-baseline.spec.ts` takes, that made the icons on the mobile products
// Settings page come out a few colour levels apart in about one run in ten
// (6 pixels, locally and in CI); with the flag, 120 repeats of that capture
// matched. The regen workflow runs this config too, so baselines and
// comparisons are rasterized the same way.
export default defineConfig({
  ...baseConfig,
  globalSetup: join(HERE, 'e2e/fixtures/global-setup.ts'),
  use: {
    ...baseConfig.use,
    launchOptions: { args: ['--disable-partial-raster'] },
  },
});
