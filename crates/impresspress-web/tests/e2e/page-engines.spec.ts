import { test, expect, type Page } from '@playwright/test';

/**
 * A model request from the worker reaches a page that runs its engine, or is
 * refused at once.
 *
 * The runtime's browser services run their models in a window, not in the
 * service worker (WebGPU is window-only): `bridge.js` posts an `embed-*`,
 * `image-*` or `llm-*` request to an open page and waits for that page's
 * engine script to answer. The incident: only the boot shell (`index.html`)
 * loaded all three engine scripts. The pages the runtime renders loaded
 * `/webllm-engine.js` alone, and `bridge.js` posted to whichever window came
 * first, so once the worker controlled the tab an embedding request
 * (`POST /b/vector/api/embed`, vector ingest) went to a page with no
 * embedding listener and was never answered (still pending after 90 s when
 * reproduced). And had the script been there, its Transformers.js import came
 * from `esm.run`, which the pages' CSP does not allow as a script source.
 *
 * These tests go through the real path — the worker, the wasm runtime, the
 * vector or llm block, `bridge.js`'s postMessage and the page's engine
 * script. Only the embedding model library is a stand-in: the Transformers.js module the engine
 * imports is answered by a route that returns a fixed vector per text, so
 * nothing is downloaded. That stand-in runs none of the library's own loading
 * (ONNX Runtime, its wasm glue, the module that glue is imported from), so it
 * cannot say whether the library loads under the pages' CSP; the real library
 * does that in `dev-page-engines.spec.ts`, on the cross-origin-isolated pages
 * where it once could not.
 */

/** Dimensions of the runtime's default embedding model (`multilingual-e5-small`). */
const DIMS = 384;

/** Long enough for a cold engine import; far short of a hang. */
const BOUND_MS = 20_000;

const FAKE_TRANSFORMERS = `
export const env = { backends: { onnx: { wasm: {} } } };
export async function pipeline(task, model) {
  if (task !== 'feature-extraction') throw new Error('unexpected task ' + task);
  return async (texts) => ({
    tolist: () => texts.map((_, i) => Array.from({ length: ${DIMS} }, (_, d) => (d === i ? 1 : 0))),
  });
}`;

async function standInTransformers(page: Page) {
  await page.route(/^https:\/\/cdn\.jsdelivr\.net\/npm\/@huggingface\/transformers@/, (route) =>
    route.fulfill({
      status: 200,
      contentType: 'text/javascript',
      headers: { 'access-control-allow-origin': '*' },
      body: FAKE_TRANSFORMERS,
    }),
  );
}

async function loginAsAdmin(page: Page) {
  await page.goto('/', { waitUntil: 'commit' });
  await page.waitForURL(/\/b\/auth\/login/, { timeout: 30_000 });
  await page.locator('input#email').fill('admin@example.com');
  await page.locator('input#password').fill('admin123');
  await page.getByRole('button', { name: /sign in/i }).click();
  await page.waitForURL(/\/b\/admin\//, { timeout: 30_000 });
}

/** `POST /b/vector/api/embed` from the page, settled or `null` after `BOUND_MS`. */
async function embedFromPage(page: Page, texts: string[]) {
  return page.evaluate(
    async ({ texts, bound }) => {
      const started = performance.now();
      const answer = await Promise.race([
        fetch('/b/vector/api/embed', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ texts }),
        }).then(async (r) => ({ status: r.status, body: await r.text() })),
        new Promise<null>((resolve) => setTimeout(() => resolve(null), bound)),
      ]);
      return { answer, ms: Math.round(performance.now() - started) };
    },
    { texts, bound: BOUND_MS },
  );
}

test('an embedding request from a runtime-rendered page is answered', async ({ page }) => {
  await standInTransformers(page);
  await loginAsAdmin(page);
  expect(await page.evaluate(() => navigator.serviceWorker.controller?.scriptURL)).toMatch(
    /\/sw\.js$/,
  );

  const { answer, ms } = await embedFromPage(page, ['first', 'second']);
  expect(answer, `no answer within ${BOUND_MS} ms: the request hung`).not.toBeNull();
  expect(answer!.status, answer!.body).toBe(200);
  const body = JSON.parse(answer!.body) as { dimensions: number; vectors: number[][] };
  expect(body.dimensions).toBe(DIMS);
  expect(body.vectors).toHaveLength(2);
  expect(body.vectors[0][0]).toBe(1);
  expect(body.vectors[1][1]).toBe(1);
  console.log(`page-engines: embed answered from an SSR page: ${ms} ms`);
});

test('with no page able to run the engine, an embedding request is a 503 that says why', async ({
  page,
}) => {
  // bridge.js refuses it with its `engine-unavailable` code; the embedding
  // bridge turns that into `VectorError::EngineUnavailable`, which the
  // runtime answers as a 503 carrying the refusal's own words — what the
  // caller has to do, not a sanitized "Internal server error (ref: …)".
  await standInTransformers(page);
  await loginAsAdmin(page);
  // A document the worker serves but no engine script runs in: a JSON
  // response, opened as a page. It is the only window of this origin, so no
  // page can answer the request — which must fail, not wait for one.
  await page.goto('/b/auth/api/me', { waitUntil: 'commit' });
  await expect.poll(() => page.evaluate(() => navigator.serviceWorker.controller !== null)).toBe(true);
  expect(await page.evaluate(() => document.querySelectorAll('script').length)).toBe(0);

  const { answer, ms } = await embedFromPage(page, ['orphan']);
  expect(answer, `no answer within ${BOUND_MS} ms: the request hung`).not.toBeNull();
  expect(answer!.status, answer!.body).toBe(503);
  expect(answer!.body).toContain(
    'no open page runs the embedding engine — open the app in a tab and try again',
  );
  expect(answer!.body).not.toContain('Internal server error');
  console.log(`page-engines: embed refused with no engine page: ${ms} ms`);
});

test('an embedding engine that cannot load in the page is a 503 that says what to do', async ({ page }) => {
  // The page runs the engine, but its library never arrives — as when the
  // pages' CSP refused ONNX Runtime's glue module, which took 37 s to surface
  // as a sanitized "Internal server error (ref: …)". The engine answers with
  // bridge.js's refusal code and a fixed message saying what to do; the
  // library's own error, which names the CDN URL it failed to load, goes to
  // the page's console and not to the caller.
  await page.route(/^https:\/\/cdn\.jsdelivr\.net\/npm\/@huggingface\/transformers@/, (route) =>
    route.abort('failed'),
  );
  await loginAsAdmin(page);

  const { answer, ms } = await embedFromPage(page, ['unreachable']);
  expect(answer, `no answer within ${BOUND_MS} ms: the request hung`).not.toBeNull();
  expect(answer!.status, answer!.body).toBe(503);
  expect(JSON.parse(answer!.body).message).toBe(
    'the embedding engine could not load in this page (its library, runtime or model failed to load) — reload the page and try again',
  );
  expect(answer!.body).not.toContain('jsdelivr');
  console.log(`page-engines: embed refused when the engine cannot load: ${ms} ms`);
});

/** A model in the browser runtime's WebLLM catalog. Never loaded here: nothing is downloaded. */
const WEBLLM_MODEL = 'SmolLM2-1.7B-Instruct-q4f32_1-MLC';

test('a chat for a model no page holds is a 503 that says why, on both chat routes', async ({
  page,
}) => {
  // The page runs the LLM engine (`webllm-engine.js`) but has loaded no
  // model, so it answers bridge.js's probe holding nothing, and the chat is
  // refused with the `engine-unavailable` code. The LLM bridge turns that
  // into `LlmError::EngineUnavailable`, wafer-core's llm handler returns it
  // as `Unavailable` with its reason, and the chat routes answer it as a 503
  // carrying that reason — the streaming one too, because the refusal is the
  // model's first item and is read before the route commits its status line.
  // It used to be a sanitized "Internal server error (ref: …)".
  await loginAsAdmin(page);
  const answers = await page.evaluate(
    async ({ model, bound }) => {
      const json = { 'content-type': 'application/json' };
      const created = await fetch('/b/messages/api/contexts', {
        method: 'POST',
        headers: json,
        body: JSON.stringify({ type: 'conversation', title: 'refused chat' }),
      });
      const thread = await created.json();
      const threadId = thread.id ?? thread.data?.id;
      const chat = (path: string) =>
        Promise.race([
          fetch(path, {
            method: 'POST',
            headers: json,
            body: JSON.stringify({ thread_id: threadId, message: 'hi', provider: 'webllm', model }),
          }).then(async (r) => ({
            status: r.status,
            type: r.headers.get('content-type'),
            body: await r.text(),
          })),
          new Promise<null>((resolve) => setTimeout(() => resolve(null), bound)),
        ]);
      return {
        created: created.status,
        threadId,
        buffered: await chat('/b/llm/api/chat'),
        streamed: await chat('/b/llm/api/chat/stream'),
      };
    },
    { model: WEBLLM_MODEL, bound: BOUND_MS },
  );
  expect(answers.created).toBe(200);
  expect(answers.threadId).toBeTruthy();

  const reason = `no open page has the LLM model '${WEBLLM_MODEL}' loaded — load it first`;
  for (const [route, answer] of [
    ['/b/llm/api/chat', answers.buffered],
    ['/b/llm/api/chat/stream', answers.streamed],
  ] as const) {
    expect(answer, `${route}: no answer within ${BOUND_MS} ms: the request hung`).not.toBeNull();
    expect(answer!.status, `${route}: ${answer!.body}`).toBe(503);
    expect(answer!.type, route).toContain('application/json');
    const message: string = JSON.parse(answer!.body).message;
    expect(message.endsWith(reason), `${route}: ${message}`).toBe(true);
    expect(answer!.body, route).not.toContain('Internal server error');
  }
  console.log('page-engines: chat refused with no page holding the model: 503 on both routes');
});
