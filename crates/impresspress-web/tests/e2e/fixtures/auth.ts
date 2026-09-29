import { expect, type APIRequestContext, type Page } from '@playwright/test';
import { ADMIN_EMAIL, ADMIN_PASSWORD } from './global-setup';

export const ADMIN_STATE_PATH = new URL('../../.auth/admin-state.json', import.meta.url).pathname;

/**
 * Phase 5d Item A — admin auth no longer happens per-test.
 *
 * `tests/e2e/fixtures/global-setup.ts` runs once before the suite, posts to
 * `POST /b/auth/api/login`, and saves the resulting `auth_token` cookie to
 * `tests/.auth/admin-state.json`. Admin describe blocks then opt-in via
 * `test.use({ storageState: ADMIN_STATE_PATH })`.
 *
 * This helper is now a sanity check: it verifies the active context already
 * carries the `auth_token` cookie globalSetup wrote. If a describe block
 * forgot the `test.use({ storageState })` line — or the storageState file is
 * empty — this throws fast with a useful error instead of letting the test
 * silently redirect to `/b/auth/login` and produce a meaningless screenshot
 * diff.
 *
 * Calling this from a `test.beforeEach` is optional. The historical callers
 * in `visual-baseline.spec.ts` were preserved as guards for that exact
 * misconfiguration.
 */
export async function loginAsAdmin(page: Page): Promise<void> {
  const cookies = await page.context().cookies();
  const hasAuth = cookies.some((c) => c.name === 'auth_token');
  if (!hasAuth) {
    const names = cookies.map((c) => c.name);
    throw new Error(
      `loginAsAdmin: no auth_token cookie in context. ` +
        `Did this describe block opt into storageState via ` +
        `test.use({ storageState: ADMIN_STATE_PATH })? ` +
        `Got cookies: ${JSON.stringify(names)}`,
    );
  }
}

/**
 * Bearer for the bootstrap admin, for a spec that seeds data over the API.
 * Bearer auth is exempt from the CSRF origin policy; the `auth_token` cookie
 * the storageState carries is not, so an API write from `request` needs this.
 */
export async function adminBearer(request: APIRequestContext): Promise<string> {
  const res = await request.post('/b/auth/api/login', {
    data: { email: ADMIN_EMAIL, password: ADMIN_PASSWORD },
    headers: { 'Content-Type': 'application/json' },
  });
  expect(res.status(), await res.text()).toBe(200);
  const { access_token } = (await res.json()) as { access_token: string };
  return `Bearer ${access_token}`;
}
