import { request as playwrightRequest, type APIRequestContext, type FullConfig } from '@playwright/test';
import { mkdirSync, readFileSync } from 'node:fs';
import { dirname } from 'node:path';

/**
 * Phase 5d Item A — single login per test run, shared via storageState.
 *
 * Before this hook landed, every test in `visual-baseline.spec.ts` did a
 * fresh form-submit login (~38 logins per CI run). Past ~35 logins the auth
 * block's cumulative rate-limit kicked in and form-submit redirects started
 * timing out. PR-3 had to drop the `admin-mobile storage-buckets` baseline as
 * a result.
 *
 * The structural fix: log in once here, save the `auth_token` cookie via
 * Playwright's storageState, then admin-scoped describes opt-in via
 * `test.use({ storageState: ADMIN_STATE_PATH })`. Anonymous describes leave
 * the config default (no storageState) so they get a clean context.
 *
 * This file is referenced by `playwright.config.ts` via `globalSetup`. It runs
 * once before the test runner spawns workers. The output file lives at
 * `tests/.auth/admin-state.json` and is gitignored — it carries a real session
 * token and must not be committed.
 *
 * Login uses the JSON endpoint (`POST /b/auth/api/login`) rather than the
 * form-submit page so we don't depend on page rendering. The endpoint sets
 * the same `auth_token` HttpOnly cookie that form-submit does (see
 * `crates/impresspress-core/src/blocks/auth/mod.rs::build_auth_cookie`).
 */

export const ADMIN_EMAIL = process.env.WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL ?? 'admin@example.com';
export const ADMIN_PASSWORD = process.env.WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD ?? 'admin123';

export const ADMIN_STATE_PATH = new URL('../../.auth/admin-state.json', import.meta.url).pathname;

export default async function globalSetup(config: FullConfig): Promise<void> {
  const baseURL =
    config.projects[0]?.use?.baseURL ??
    `http://127.0.0.1:${process.env.TEST_PORT ?? 8080}`;

  // Ensure the output directory exists before storageState() tries to write.
  mkdirSync(dirname(ADMIN_STATE_PATH), { recursive: true });

  // Use a request-only context — no browser launch needed for a JSON login.
  // The resulting storageState is browser-compatible (cookies are domain-scoped,
  // not bound to a browser session).
  const requestContext = await playwrightRequest.newContext({ baseURL });

  const res = await requestContext.post('/b/auth/api/login', {
    data: { email: ADMIN_EMAIL, password: ADMIN_PASSWORD },
    headers: { 'Content-Type': 'application/json' },
  });

  if (res.status() !== 200) {
    const body = await res.text().catch(() => '<unreadable>');
    throw new Error(
      `globalSetup: admin login failed (status=${res.status()}). ` +
        `Is the server up at ${baseURL}? Body: ${body}`,
    );
  }

  // Phase 5d Item C — seed a deterministic `photos` bucket with one file at
  // root and one in a `nested/` prefix so the storage-objects baselines have
  // something to render. Seeding is best-effort: any failure logs a warning
  // and continues so a transient API hiccup doesn't tank the whole suite —
  // the affected baselines will then visibly diff against the empty state.
  await seedStoragePhotos(requestContext);

  await requestContext.storageState({ path: ADMIN_STATE_PATH });
  await requestContext.dispose();

  // Verify the cookie we expect actually landed in storageState. Without this
  // a misconfigured server (e.g. one that returned 200 but didn't set a cookie)
  // would silently produce an empty state file and every admin test would
  // redirect back to /b/auth/login.
  const saved = JSON.parse(readFileSync(ADMIN_STATE_PATH, 'utf8')) as {
    cookies: Array<{ name: string }>;
  };
  const hasAuthCookie = saved.cookies.some((c) => c.name === 'auth_token');
  if (!hasAuthCookie) {
    throw new Error(
      `globalSetup: login returned 200 but no auth_token cookie was saved. ` +
        `storageState cookies: ${JSON.stringify(saved.cookies.map((c) => c.name))}`,
    );
  }
}

/**
 * 1×1 transparent PNG. Deterministic byte payload — same content for every
 * upload so baseline filesystems match across runs. Inlined as a Buffer rather
 * than read from disk so the seed has zero filesystem deps.
 */
const TRANSPARENT_PNG_1X1 = Buffer.from([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49,
  0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06,
  0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44,
  0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d,
  0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42,
  0x60, 0x82,
]);

/**
 * Idempotent seed for the `photos` bucket used by visual-baseline storage-
 * objects routes. Creates the bucket (if missing), then puts `a.png` at root
 * and `nested/b.png` in a prefix. All writes are idempotent: re-creating the
 * bucket and re-uploading keys are both no-ops at the filesystem layer.
 *
 * Errors are logged and swallowed — see callsite comment in globalSetup.
 *
 * Upload API: `POST /b/storage/api/buckets/{bucket}/objects?key=<key>` with
 * the raw object body. `Content-Type` header drives the stored content-type.
 */
async function seedStoragePhotos(req: APIRequestContext): Promise<void> {
  const BUCKET = 'photos';

  // 1. Create the bucket. The handler returns 200 on success and 500 if the
  //    bucket already exists at the filesystem layer; we treat anything <500
  //    as a successful idempotent state. (The current backend `create_folder`
  //    is `mkdir -p` semantics, so re-creates also return 200.)
  try {
    const bucketRes = await req.post('/b/storage/api/buckets', {
      data: { name: BUCKET, public: false },
      headers: { 'Content-Type': 'application/json' },
    });
    if (bucketRes.status() >= 500) {
      const body = await bucketRes.text().catch(() => '<unreadable>');
      console.warn(
        `[globalSetup] seedStoragePhotos: bucket-create returned ${bucketRes.status()}; continuing. body=${body}`,
      );
    }
  } catch (err) {
    console.warn(`[globalSetup] seedStoragePhotos: bucket-create threw: ${err}; continuing.`);
    return;
  }

  // 2. Upload the two objects. `put` overwrites at the storage layer, so
  //    re-runs against the same bucket are idempotent at the file level.
  const uploads: Array<{ key: string }> = [{ key: 'a.png' }, { key: 'nested/b.png' }];

  for (const { key } of uploads) {
    try {
      const url = `/b/storage/api/buckets/${encodeURIComponent(BUCKET)}/objects?key=${encodeURIComponent(key)}`;
      const res = await req.post(url, {
        data: TRANSPARENT_PNG_1X1,
        headers: { 'Content-Type': 'image/png' },
      });
      if (res.status() >= 400) {
        const body = await res.text().catch(() => '<unreadable>');
        console.warn(
          `[globalSetup] seedStoragePhotos: upload ${key} returned ${res.status()}; continuing. body=${body}`,
        );
      }
    } catch (err) {
      console.warn(`[globalSetup] seedStoragePhotos: upload ${key} threw: ${err}; continuing.`);
    }
  }
}
