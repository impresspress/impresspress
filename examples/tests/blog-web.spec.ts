import { test, expect } from "@playwright/test";

const PORT = process.env.TEST_WEB_PORT ?? "8090";
const BASE = `http://127.0.0.1:${PORT}`;

test("blog-web bundle serves index + wasm", async ({ page, request }) => {
  const idx = await request.get(`${BASE}/index.html`);
  expect(idx.status()).toBe(200);

  const manifestRes = await request.get(`${BASE}/asset-manifest.json`);
  expect(manifestRes.status()).toBe(200);
  const manifest = await manifestRes.json();
  const wasmPath = manifest.assets["impresspress_web_bg.wasm"];
  expect(wasmPath).toBeTruthy();

  const wasm = await request.get(`${BASE}${wasmPath}`);
  expect(wasm.status()).toBe(200);
  expect(wasm.headers()["content-type"]).toBe("application/wasm");

  await page.goto(BASE);
  // Service worker activation can be slow; check the title is set.
  await expect(page).toHaveTitle(/blog|inkwell|impresspress/i, { timeout: 15_000 });
});
