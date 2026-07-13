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
export default defineConfig({
  ...baseConfig,
  globalSetup: join(HERE, 'e2e/fixtures/global-setup.ts'),
});
