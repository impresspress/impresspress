import { test, expect, type Page } from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';
import { ADMIN_ROUTES, ANON_ROUTES, type Route } from './fixtures/routes';

/**
 * axe over every page the visual suite captures (`fixtures/routes.ts`), at
 * 1280 and on a phone (375×812 with touch), for the document's structure
 * only (PLAN §5.4, S8):
 * - `heading-order`: no heading level skipped;
 * - the `landmark-*` rules: one `main`, one top-level `banner`, unique
 *   names for repeated landmarks, none nested where it must not be;
 * - `region`: no content outside a landmark.
 * Colour contrast is not run here: the Rust value-based guards
 * (`text_and_background_pairs_meet_wcag_aa` and the token pins) own it, and
 * axe's pixel heuristic would report the same pairs a second time.
 *
 * First run against unmodified main (3e96a37f) on this branch: the only
 * findings were on `/totally-not-a-thing` (`landmark-one-main`, `region`),
 * which the server answers with a JSON error that the browser shows as text.
 * That is not an HTML page, so it is left out (`notAPage`). The four seller
 * pages the read-only server refuses (`refused`) are left out too: a refused
 * request ahead of the dashboard capture changes its figures, and this file
 * runs before `visual-baseline.spec.ts`. They render `ui::forbidden_response`,
 * the shared status page, whose headings the Rust crawl guard
 * (`htmx_guard::headings_descend_without_skipping`, which reads status pages
 * too) holds.
 *
 * Reads only, so part of `e2e:visual`.
 */

const RULES = [
  'heading-order',
  'region',
  'landmark-banner-is-top-level',
  'landmark-complementary-is-top-level',
  'landmark-contentinfo-is-top-level',
  'landmark-main-is-top-level',
  'landmark-no-duplicate-banner',
  'landmark-no-duplicate-contentinfo',
  'landmark-no-duplicate-main',
  'landmark-one-main',
  'landmark-unique',
];

const SIZES = [
  { name: 'desktop', viewport: { width: 1280, height: 720 }, hasTouch: false },
  { name: 'phone', viewport: { width: 375, height: 812 }, hasTouch: true },
] as const;

/** The pages axe reads: every captured route that is an HTML page the
 * read-only server answers. */
const checked = (routes: Route[]) => routes.filter((r) => !r.notAPage && !r.refused);

async function violations(page: Page) {
  const result = await new AxeBuilder({ page }).withRules(RULES).analyze();
  return result.violations.flatMap((v) => v.nodes.map((n) => `${v.id}: ${n.target.join(' ')}`));
}

for (const size of SIZES) {
  test.describe(`document structure, ${size.name}`, () => {
    test.use({ viewport: size.viewport, hasTouch: size.hasTouch });

    for (const r of checked(ANON_ROUTES)) {
      test(`anon ${r.name}`, async ({ page }) => {
        await page.goto(r.path, { waitUntil: 'networkidle' });
        expect(await violations(page)).toEqual([]);
      });
    }

    test.describe('admin', () => {
      test.use({ storageState: ADMIN_STATE_PATH });
      for (const r of checked(ADMIN_ROUTES)) {
        test(`admin ${r.name}`, async ({ page }) => {
          await loginAsAdmin(page);
          await page.goto(r.path, { waitUntil: 'networkidle' });
          expect(await violations(page)).toEqual([]);
        });
      }
    });
  });
}

test('the routes left out are the ones that say why', () => {
  const left = [...ANON_ROUTES, ...ADMIN_ROUTES].filter((r) => r.notAPage || r.refused).map((r) => r.name);
  expect(left).toEqual([
    'status-404',
    'portal-products-list',
    'portal-products-new',
    'portal-products-selling',
    'portal-products-selling-orders',
  ]);
});
