import { test, expect, Page } from '@playwright/test';

const ADMIN_EMAIL = 'admin@example.com';
const ADMIN_PASSWORD = 'admin123';

/**
 * Log in via the JSON API (POST /b/auth/api/login) and wait for the service
 * worker to activate.  The login endpoint sets an auth cookie that subsequent
 * page.request calls automatically carry, so no manual token handling is needed.
 */
async function loginAndWaitForSW(page: Page) {
  // Navigate first so the SW can register and control the page.
  await page.goto('/');
  await page.waitForFunction(
    () => navigator.serviceWorker.controller !== null,
    null,
    { timeout: 30_000 },
  );

  // The auth block exposes a JSON login endpoint — no form-fill required.
  const res = await page.request.post('/b/auth/api/login', {
    data: { email: ADMIN_EMAIL, password: ADMIN_PASSWORD },
  });
  expect(res.status()).toBe(200);
}

// ─── helpers ──────────────────────────────────────────────────────────────────

const dims = 384;
const zeros = (): number[] => Array.from({ length: dims }, () => 0);
const align = (i: number): number[] => {
  const v = zeros();
  v[i] = 1;
  return v;
};

// ─── tests ────────────────────────────────────────────────────────────────────

test('create + upsert + query (vector mode) over a small index', async ({ page }) => {
  await loginAndWaitForSW(page);

  // 1. Create a 384-d cosine index without keyword search.
  const create = await page.request.post('/b/vector/api/indexes', {
    data: {
      config: {
        name: 'smoke',
        model: 'multilingual-e5-small',
        dimensions: dims,
        metric: 'cosine',
        keyword_search: false,
      },
    },
  });
  expect(create.status()).toBe(200);

  // 2. Upsert pre-computed vectors (no model call needed — bypass /ingest).
  //    doc-A is the standard basis vector e_0; the query vector is also e_0,
  //    so doc-A wins on cosine similarity (score ≈ 1.0), doc-B loses.
  const upsert = await page.request.post('/b/vector/api/upsert', {
    data: {
      index: 'smoke',
      entries: [
        { id: 'doc-A', vector: align(0) },
        { id: 'doc-B', vector: align(100) },
      ],
    },
  });
  expect(upsert.status()).toBe(200);

  // 3. Query with a vector identical to doc-A's; expect it to be top result.
  const query = await page.request.post('/b/vector/api/query', {
    data: {
      index: 'smoke',
      vector: align(0),
      top_k: 2,
      mode: 'vector',
    },
  });
  expect(query.status()).toBe(200);
  const body = await query.json();
  expect(body.matches[0].id).toBe('doc-A');
  expect(body.matches[0].score).toBeGreaterThan(0.99);
  expect(body.matches[1].id).toBe('doc-B');

  // 4. Cleanup.
  const del = await page.request.delete('/b/vector/api/indexes/smoke');
  expect(del.status()).toBe(200);
});

test('hybrid search returns FTS + vector matches via RRF', async ({ page }) => {
  await loginAndWaitForSW(page);

  // 1. Create an index with keyword_search enabled.
  const create = await page.request.post('/b/vector/api/indexes', {
    data: {
      config: {
        name: 'hybrid',
        model: 'multilingual-e5-small',
        dimensions: dims,
        metric: 'cosine',
        keyword_search: true,
      },
    },
  });
  expect(create.status()).toBe(200);

  // 2. Upsert three entries: doc-A is both the nearest vector and a keyword
  //    hit; doc-B is only a keyword hit; doc-C is neither.
  const upsert = await page.request.post('/b/vector/api/upsert', {
    data: {
      index: 'hybrid',
      entries: [
        { id: 'doc-A', vector: align(0),   text: 'apples and oranges' },
        { id: 'doc-B', vector: align(100),  text: 'apples in autumn' },
        { id: 'doc-C', vector: align(200),  text: 'completely unrelated' },
      ],
    },
  });
  expect(upsert.status()).toBe(200);

  // 3. Hybrid query: vector close to doc-A, keyword "apples" matches A and B.
  //    RRF should rank doc-A first (wins both legs), doc-B second (keyword
  //    hit), doc-C last (no overlap).
  const query = await page.request.post('/b/vector/api/query', {
    data: {
      index: 'hybrid',
      vector: align(0),
      top_k: 3,
      mode: 'hybrid',
      keyword_query: 'apples',
    },
  });
  expect(query.status()).toBe(200);
  const body = await query.json();
  expect(body.matches[0].id).toBe('doc-A');
  expect(body.matches.map((m: { id: string }) => m.id)).toContain('doc-B');
  expect(body.matches[body.matches.length - 1].id).toBe('doc-C');

  // 4. Cleanup.
  await page.request.delete('/b/vector/api/indexes/hybrid');
});
