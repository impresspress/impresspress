import {
  test,
  expect,
  request as apiRequest,
  type APIRequestContext,
  type Locator,
  type Page,
} from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * The shared list search box (`ui::components::SearchInput`) in a real
 * browser, on two of its pages: the operator can type through a search that
 * is in flight without losing focus or characters, the search is in the URL
 * and a reload keeps it, and typing replaces the history entry rather than
 * adding one per term — Back leaves the searched page in one step. htmx keeps
 * no history snapshots (`ui::layout`'s htmx config), so admin lists never land
 * in localStorage, and Back to an htmx-pushed URL loads it whole. For a
 * screen reader the box is a search landmark, each search announces what it
 * found, and Clear puts focus back in the box.
 *
 * Signs up accounts (users) and makes requests the request log records
 * (logs), so it is part of `e2e:writes`, which CI runs on its own server.
 */

/** A list page with a search box, seeded so a search narrows its list. */
interface SearchPage {
  name: string;
  path: string;
  box: (page: Page) => Locator;
  /** Make rows that hold `term` and rows that do not both exist. */
  seed: (request: APIRequestContext, term: string) => Promise<void>;
}

const PAGES: SearchPage[] = [
  {
    name: 'users',
    path: '/b/admin/users',
    box: (page) => page.getByRole('searchbox', { name: 'Search by email or user ID...' }),
    seed: async (_request, term) => {
      // Each from a signed-out context of its own: a cookie-carrying context
      // (the admin's, which a context made inside this describe inherits
      // unless told otherwise, or one a signup just signed in) is refused a
      // write it did not make from the site's own pages.
      for (const email of [`${term}@example.com`, `other-${Date.now()}@example.com`]) {
        const anonymous = await apiRequest.newContext({
          baseURL: test.info().project.use.baseURL,
          storageState: { cookies: [], origins: [] },
        });
        const res = await anonymous.post('/b/auth/api/signup', {
          data: { email, password: 'search-history-horse-1' },
          headers: { 'Content-Type': 'application/json' },
        });
        expect(res.status(), await res.text()).toBe(201);
        await anonymous.dispose();
      }
    },
  },
  {
    name: 'system logs',
    path: '/b/admin/logs',
    box: (page) => page.getByRole('searchbox', { name: 'Search by path...' }),
    seed: async (request, term) => {
      // The log keeps the path of a request to a known route (an unmatched
      // one is filed under one label): a variable that does not exist is a
      // 404 on the variable-edit route.
      await request.get(`/b/admin/variables/${term}/edit`);
      await request.get('/b/auth/login');
    },
  },
];

const rows = (page: Page) => page.locator('#content table tbody tr');
const searchedFor = (term: string) => new RegExp(`[?&]search=${term}(&|$)`);

/** Every row holds `term`, and there is at least one. */
async function expectOnlyMatches(page: Page, term: string): Promise<void> {
  await expect(rows(page).first()).toContainText(term);
  await expect(rows(page).filter({ hasNotText: term })).toHaveCount(0);
}

/** The full list: the box is empty and rows without `term` are back. */
async function expectUnfiltered(page: Page, target: SearchPage, term: string): Promise<void> {
  await expect(page).toHaveURL(new RegExp(`${target.path}$`));
  await expect(target.box(page)).toHaveValue('');
  await expect(page.locator('.search-summary')).toHaveCount(0);
  await expect(rows(page).filter({ hasNotText: term }).first()).toBeVisible();
}

for (const target of PAGES) {
  test.describe(`${target.name} search`, () => {
    test.use({ storageState: ADMIN_STATE_PATH });

    test('typing on through an in-flight search keeps focus and every character', async ({
      page,
      request,
    }) => {
      await loginAsAdmin(page);
      const term = `srch${Date.now()}`;
      await target.seed(request, term);
      await page.goto(target.path, { waitUntil: 'networkidle' });
      const box = target.box(page);

      // Hold the first search's answer back, so the operator is still typing
      // when it lands — the case where swapping it in would put the shorter
      // term back in the box.
      const first = term.slice(0, 6);
      let held = 0;
      await page.route(
        (url) => url.pathname === target.path && url.searchParams.get('search') === first,
        async (route) => {
          held += 1;
          await new Promise((resolve) => setTimeout(resolve, 900));
          await route.continue();
        },
      );

      await box.click();
      await page.keyboard.type(first);
      // Past the 300ms debounce: the first search is now in flight.
      await page.waitForTimeout(450);
      await page.keyboard.type(term.slice(first.length), { delay: 40 });
      await page.waitForTimeout(1500);

      expect(held, 'the first search was sent').toBe(1);
      await expect(box).toBeFocused();
      await expect(box).toHaveValue(term);
      expect(await box.evaluate((el: HTMLInputElement) => el.selectionStart)).toBe(term.length);
      await expect(page).toHaveURL(searchedFor(term));
      await expectOnlyMatches(page, term);

      // A swap that DID land re-rendered the box: focus and caret moved to
      // the new one, so the next keystroke lands after the term.
      await page.keyboard.type('x');
      await expect(page).toHaveURL(searchedFor(`${term}x`));
      await expect(box).toBeFocused();
      await expect(box).toHaveValue(`${term}x`);
    });

    test('the search is in the URL, survives a reload, and Clear takes it out', async ({
      page,
      request,
    }) => {
      await loginAsAdmin(page);
      const term = `srch${Date.now()}`;
      await target.seed(request, term);
      await page.goto(target.path, { waitUntil: 'networkidle' });
      await expectUnfiltered(page, target, term);

      await target.box(page).fill(term);
      await expect(page).toHaveURL(searchedFor(term));
      await expectOnlyMatches(page, term);
      await expect(page.locator('.search-summary')).toContainText(term);

      // The live region sits outside the swapped body and says what the
      // search found.
      await expect(page.locator('#search-status')).toHaveText(`1 result for “${term}”`);
      await expect(page.getByRole('search').getByRole('searchbox')).toHaveCount(1);

      await page.reload({ waitUntil: 'networkidle' });
      await expect(target.box(page)).toHaveValue(term);
      await expectOnlyMatches(page, term);

      await page.locator('.search-summary').getByRole('link', { name: 'Clear' }).click();
      await expectUnfiltered(page, target, term);
      await expect(target.box(page)).toBeFocused();
      await expect(page.locator('#search-status')).toHaveText(/^\d+ results$/);
    });

    test('Back after searching returns to the page before the search, in one step', async ({
      page,
      request,
    }) => {
      await loginAsAdmin(page);
      const term = `srch${Date.now()}`;
      await target.seed(request, term);
      await page.goto('/b/admin/', { waitUntil: 'networkidle' });
      const before = page.url();
      await page.goto(target.path, { waitUntil: 'networkidle' });

      const box = target.box(page);
      await box.fill(term);
      await expect(page).toHaveURL(searchedFor(term));
      await box.fill(`${term}none`);
      await expect(page).toHaveURL(searchedFor(`${term}none`));
      await expect(page.locator('.search-summary')).toContainText(`${term}none`);

      await page.goBack({ waitUntil: 'networkidle' });
      await expect(page).toHaveURL(before);

      // The entry the search replaced holds the last term, not the first.
      await page.goForward({ waitUntil: 'networkidle' });
      await expect(page).toHaveURL(searchedFor(`${term}none`));
      await expect(target.box(page)).toHaveValue(`${term}none`);

      const cached = await page.evaluate(() => localStorage.getItem('htmx-history-cache'));
      expect(cached, 'htmx wrote no page snapshot to localStorage').toBeNull();
    });
  });
}

test.describe('htmx history', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  test('Back to an htmx-pushed page loads it whole, and nothing is cached', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/logs', { waitUntil: 'networkidle' });
    await page.getByRole('link', { name: 'Audit Logs' }).click();
    await expect(page).toHaveURL(/tab=audit/);
    await expect(page.getByRole('searchbox', { name: 'Search by resource...' })).toBeVisible();

    await page.goBack({ waitUntil: 'networkidle' });
    await expect(page).toHaveURL(/\/b\/admin\/logs$/);
    await expect(page.getByRole('searchbox', { name: 'Search by path...' })).toBeVisible();
    await expect(page.locator('main#content')).toHaveCount(1);
    await expect(page.getByRole('navigation', { name: 'Primary' })).toHaveCount(1);

    const cached = await page.evaluate(() => localStorage.getItem('htmx-history-cache'));
    expect(cached, 'htmx wrote no page snapshot to localStorage').toBeNull();
  });
});
