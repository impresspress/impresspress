import { defineConfig } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

import visualConfig from './playwright.visual-baseline.config.ts';

const HERE = dirname(fileURLToPath(import.meta.url));

// `e2e:visual`: the visual-baseline config, plus the guard that the run left
// no refused or failed request in the server's request log, the table the
// dashboard counts (`fixtures/no-refusals.ts`; static assets and paths
// outside every block are not logged).
// `e2e:writes` runs the same config without it, on a server of its own,
// because refusing requests is part of what its specs test.
export default defineConfig({
  ...visualConfig,
  globalTeardown: join(HERE, 'e2e/fixtures/no-refusals.ts'),
});
