import { test, expect } from '@playwright/test';
import { bootServiceWorker, loginAdmin } from './fixtures/dev-sandbox';
import { DIMS, expectedVector, modelFiles } from './fixtures/tiny-embedding-model';

/**
 * The embedding engine loads its REAL library on a cross-origin-isolated page
 * under the runtime's CSP, and answers.
 *
 * The incident (dev.impresspress.org, 2026-10-11): `POST /b/vector/api/embed`
 * from a page the runtime rendered answered a sanitized 500 after 37 s. Every
 * page of a dev-sandbox deployment is cross-origin isolated, and there ONNX
 * Runtime (inside Transformers.js) runs several threads; its threads are
 * workers started from its wasm glue module, which must be same-origin, so for
 * a glue module on the CDN it imported a copy from a `blob:` URL. The pages'
 * `script-src` has no `blob:`: the import was refused and no backend loaded
 * ("no available backend found"). `page-engines.spec.ts` could not see it —
 * its Transformers.js is a stand-in with no ONNX Runtime to load.
 *
 * So here only the model's FILES are stand-ins (`fixtures/tiny-embedding-model.ts`,
 * a real ONNX graph a few kilobytes long, instead of a 100 MB download from
 * the hub). The library is the one the engine imports from cdn.jsdelivr.net,
 * and it loads ONNX Runtime's glue module and wasm from there as it does in
 * production; the request goes through the worker, the wasm runtime, the
 * vector block, `bridge.js` and the page's `embed-engine.js`; and the answer
 * is checked against what only running the graph produces.
 */

/** A cold load of the library and its wasm runtime from the CDN; far short of the incident's 37 s hang. */
const BOUND_MS = 30_000;

/** Where Transformers.js fetches the default embedding model's files. */
const MODEL_FILES = /^https:\/\/huggingface\.co\/Xenova\/multilingual-e5-small\/resolve\/main\/(.+)$/;

test('the real embedding engine loads on a cross-origin-isolated page under the runtime CSP', async ({ page }) => {
  const files = modelFiles();
  await page.route(MODEL_FILES, (route) => {
    const file = files[MODEL_FILES.exec(route.request().url())![1]];
    return route.fulfill(
      file
        ? {
            status: 200,
            contentType: file.contentType,
            headers: { 'access-control-allow-origin': '*' },
            body: file.body,
          }
        : { status: 404, headers: { 'access-control-allow-origin': '*' }, body: 'not found' },
    );
  });
  const refusedScripts: string[] = [];
  page.on('console', (message) => {
    const text = message.text();
    if (text.includes('violates the following Content Security Policy directive: "script-src')) {
      refusedScripts.push(text);
    }
  });

  await bootServiceWorker(page);
  await loginAdmin(page);
  const [adminPage] = await Promise.all([
    page.waitForResponse((r) => r.request().isNavigationRequest() && new URL(r.url()).pathname === '/b/admin/'),
    page.goto('/b/admin/', { waitUntil: 'commit' }),
  ]);

  // The conditions the incident needed, so this cannot pass by testing
  // something milder: the page is cross-origin isolated (ONNX Runtime goes
  // multi-threaded), and its CSP allows no `blob:` script.
  expect(await page.evaluate(() => crossOriginIsolated)).toBe(true);
  const scriptSrc = (await adminPage.headerValue('content-security-policy'))
    ?.split(';')
    .map((directive) => directive.trim())
    .find((directive) => directive.startsWith('script-src '));
  expect(scriptSrc, 'the page has no script-src').toBeDefined();
  expect(scriptSrc).not.toContain('blob:');

  const { answer, ms } = await page.evaluate(
    async ({ bound }) => {
      const started = performance.now();
      const answer = await Promise.race([
        fetch('/b/vector/api/embed', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ texts: ['hello world', 'something else'] }),
        }).then(async (r) => ({ status: r.status, body: await r.text() })),
        new Promise<null>((resolve) => setTimeout(() => resolve(null), bound)),
      ]);
      return { answer, ms: Math.round(performance.now() - started) };
    },
    { bound: BOUND_MS },
  );
  expect(answer, `no answer within ${BOUND_MS} ms`).not.toBeNull();
  expect(answer!.status, `${answer!.body}\nCSP refusals: ${refusedScripts.join('\n')}`).toBe(200);
  const body = JSON.parse(answer!.body) as { dimensions: number; vectors: number[][] };
  expect(body.dimensions).toBe(DIMS);
  expect(body.vectors).toHaveLength(2);
  const expected = expectedVector();
  for (const vector of body.vectors) {
    expect(vector).toHaveLength(DIMS);
    for (const [d, value] of vector.entries()) expect(value).toBeCloseTo(expected[d], 5);
  }
  expect(refusedScripts).toEqual([]);
  console.log(`dev-page-engines: real embedding engine answered on an isolated page: ${ms} ms`);
});
