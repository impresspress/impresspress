import { defineConfig, devices } from '@playwright/test';

const PORT = process.env.TEST_PORT ? parseInt(process.env.TEST_PORT) : 8091;

export default defineConfig({
  testDir: './tests',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: 1,
  reporter: [['html', { open: 'never' }], ['list']],
  timeout: 30_000,
  expect: {
    timeout: 10_000,
  },
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    // Present as a same-origin browser request. impresspress' CSRF defense
    // rejects credentialed (cookie-carrying) state-changing requests that
    // don't prove same-origin via Sec-Fetch metadata — a real browser form
    // POST always sends these, but Playwright's API request context does not
    // by default, so signup->login in one context would otherwise 403.
    extraHTTPHeaders: {
      'Origin': `http://127.0.0.1:${PORT}`,
      'Sec-Fetch-Site': 'same-origin',
      'Sec-Fetch-Mode': 'cors',
    },
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  projects: [
    {
      name: 'desktop-chrome',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
});
