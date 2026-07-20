import { defineConfig, devices } from "@playwright/test";

const port = Number(process.env.PRODUCT_EXAMPLES_PORT || 4178);

export default defineConfig({
  testDir: "./tests",
  testMatch: "products-examples.spec.ts",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 4 : 5,
  reporter: [["list"], ["html", { open: "never", outputFolder: "playwright-report-products" }]],
  timeout: 30_000,
  expect: { timeout: 8_000 },
  use: {
    ...devices["Desktop Chrome"],
    baseURL: `http://127.0.0.1:${port}`,
    trace: "on-first-retry",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  webServer: {
    command: `python3 -m http.server ${port} -d products`,
    url: `http://127.0.0.1:${port}/matrix.html`,
    reuseExistingServer: !process.env.CI,
    timeout: 20_000,
  },
});
