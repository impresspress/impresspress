import { test, expect } from '@playwright/test';
import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const PKG = join(import.meta.dirname, '../../pkg');

function readManifestBuildId(): string {
  const body = readFileSync(join(PKG, 'asset-manifest.json'), 'utf8');
  return JSON.parse(body).buildId as string;
}

test('a new build causes the SW to update and fetch new hashed WASM', async ({ page }) => {
  await page.goto('/');
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
  const initialBuildId = readManifestBuildId();

  const initialWasm = await new Promise<string>((resolve) => {
    page.on('request', function listener(req) {
      if (req.url().match(/impresspress_web_bg-[a-f0-9]+\.wasm/)) {
        page.off('request', listener);
        resolve(req.url());
      }
    });
    page.reload();
  });

  execSync(
    `touch crates/impresspress-web/src/lib.rs && cd crates/impresspress-web && make build`,
    { cwd: join(import.meta.dirname, '../../../..'), stdio: 'inherit' },
  );

  const newBuildId = readManifestBuildId();
  expect(newBuildId).not.toBe(initialBuildId);

  const newWasm = await new Promise<string>((resolve) => {
    page.on('request', function listener(req) {
      if (req.url().match(/impresspress_web_bg-[a-f0-9]+\.wasm/) && req.url() !== initialWasm) {
        page.off('request', listener);
        resolve(req.url());
      }
    });
    page.reload();
  });

  expect(newWasm).not.toBe(initialWasm);
});

test('a no-op rebuild does not trigger a SW update', async ({ page }) => {
  await page.goto('/');
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
  const buildId1 = readManifestBuildId();

  execSync(
    `cd crates/impresspress-web && make build`,
    { cwd: join(import.meta.dirname, '../../../..'), stdio: 'inherit' },
  );

  const buildId2 = readManifestBuildId();
  expect(buildId2).toBe(buildId1);
});
