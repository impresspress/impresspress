import { test, expect, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';
import { structureViolations } from './fixtures/axe';
import { expectAtMostOnePrimary } from './fixtures/primaries';
import { ADMIN_ROUTES, ANON_ROUTES } from './fixtures/routes';
import { targetFloor } from './fixtures/targets';


// The tolerance is measured, not guessed. Baselines are CI's own render
// (`regen-visual-baselines.yml`), so a matching page differs from its
// baseline only by rendering noise — and across repeated CI runs of this
// suite at zero tolerance that noise was a handful of pixels one colour
// level apart (a per-pixel YIQ distance of 0.004 on Playwright's 0-1 scale;
// nothing larger in any run). `threshold` sits just above that noise, and no
// pixel beyond it is allowed, so a real change of even one pixel fails.
//
// A value that varies from run to run is masked rather than tolerated — see
// `volatileMasks` below. A tolerance wide enough to absorb one would also
// absorb real regressions: a 1% pixel ratio lets a 1280x720 capture differ
// in 9,216 pixels, and Playwright's default per-pixel `threshold` of 0.2
// ignores a change between two light greys outright.
const COMMON_OPTS = {
  fullPage: true as const,
  threshold: 0.01,
  maxDiffPixels: 0,
};

// The scroll containers of the app shell (`ui/styles/layouts/shell.css`).
// `.shell` is exactly `100vh` tall and the page scrolls inside `.shell__body`
// (and the sidebar's nav list inside `.sidebar__groups`), so the document
// itself never scrolls and `fullPage` alone captures one viewport: the fold
// sat at 720px on desktop and 812px on mobile, and nothing below it was
// compared.
const SHELL_SCROLLERS = '.shell__body, .sidebar__groups';

// Grow the viewport until no on-screen shell scroller has anything left to
// scroll, so the capture holds the whole page as the app lays it out on a
// screen that tall. The page's own CSS stays untouched — overriding
// `.shell`'s height instead would change the layout under test (the chat
// page, for one, sizes itself to `100%` of `.shell__body`). Only scrollers
// intersecting the viewport count: on mobile the sidebar is an off-canvas
// drawer, and its nav list's overflow is not part of the page.
//
// The loop converges because the overflow it removes does not grow with the
// viewport; a page whose content did would never settle, and that fails here
// rather than producing an arbitrarily tall capture.
//
// Chromium's compositor caps a surface at 16,384px (its maximum texture
// size), so a capture near or past that is not one to trust. The cap below
// keeps a margin under it. The tallest capture today is the phone
// admin-variables one, at about 6,500px (the desktop one is about 3,400px).
// A page that outgrows the cap fails here by name, so the fix is a
// deliberate one — split the capture, or capture a filtered view — rather
// than a baseline that silently stopped at the cap.
const MAX_CAPTURE_HEIGHT = 16_000;

async function growViewportToContent(page: Page) {
  for (let pass = 0; pass < 5; pass++) {
    const overflow = await page.evaluate((selector) => {
      let most = 0;
      for (const el of document.querySelectorAll(selector)) {
        const box = el.getBoundingClientRect();
        if (box.right <= 0 || box.left >= window.innerWidth) continue;
        most = Math.max(most, el.scrollHeight - el.clientHeight);
      }
      return most;
    }, SHELL_SCROLLERS);
    if (overflow <= 0) return;
    const size = page.viewportSize();
    if (!size) throw new Error('growViewportToContent: the page has no fixed viewport');
    const height = size.height + overflow;
    if (height > MAX_CAPTURE_HEIGHT) {
      throw new Error(
        `growViewportToContent: ${page.url()} needs a ${height}px capture, over the ` +
          `${MAX_CAPTURE_HEIGHT}px cap Chromium can screenshot reliably; split or filter this capture`,
      );
    }
    await page.setViewportSize({ width: size.width, height });
  }
  throw new Error(`growViewportToContent: ${page.url()} still overflows after 5 passes`);
}

// Every capture also holds the one-primary rule as the browser shows the
// page (`fixtures/primaries.ts`): at most one primary outside the dialogs,
// and at most one in each open dialog.
async function expectPageScreenshot(page: Page, name: string, mask?: ReturnType<typeof volatileMasks>) {
  await expectAtMostOnePrimary(page);
  await growViewportToContent(page);
  await expect(page).toHaveScreenshot(name, mask ? { ...COMMON_OPTS, mask } : COMMON_OPTS);
}

// What the admin captures mask: values that differ from one run to the next,
// or with what ran before the capture.
//
// - Dates and timestamps, which the pages render as `<time>` — among them
//   every "Created" date the suite captures (users, the dashboard's Recent
//   Users, both bucket lists). In a table the owning `<td>` is masked, not
//   the `<time>`: the element is as wide as its text, so "Oct 3" and
//   "Oct 28" would paint different mask boxes, while the cell is as wide as
//   its column. Only times on screen are masked: the dashboard charts' "Show
//   values" tables sit closed in a `<details>`, yet still report a box per
//   date cell, so masking them painted a text-wide rectangle per day over
//   the cards below (seen 2026-10-06 on the admin dashboard).
// - The dashboard charts' first/last-day labels, which move daily because the
//   window ends today (`ui/components/chart.rs`). The whole
//   `.chart__range` row is masked, not its two `<time>` spans: a span is
//   as wide as its text, so "Aug 31" and "Sep 1" would paint different mask
//   boxes, while the row is as wide as the chart and the mask box stays put.
//   Masking the spans let a baseline captured with a two-digit first day
//   fail from the 1st to the 9th of the next month (seen 2026-09-30).
// - The storage-admin buckets table's Owner cell: the first 8 hex digits of
//   the bootstrap admin's freshly generated UUIDv7. `blocks/files/pages_admin.rs`
//   marks the value `data-volatile-id`; the mask covers the owning `<td>`, and
//   the value is monospaced, so a new one does not change the column's width
//   and the mask box stays put.
// - Latencies the run measured itself. `data-volatile-metric` wraps the
//   duration figures on the admin network page (`blocks/admin/pages/network.rs`).
//   The mask covers the owning `<td>`, not the span: the span is as wide as
//   its text, so "5ms" and "12ms" would paint different mask boxes. The
//   admin dashboard's "Avg response" stat is reached by its label instead,
//   because `components::Stat` takes its value as a plain `&str` with
//   nowhere to hang an attribute — a Rust test pins that label so a rename
//   cannot silently unmask the stat.
// - Counts of the requests the suite itself made, which change whenever a
//   spec or a capture is added ahead of the page. On the dashboard: the
//   "Requests today" stat and the "Requests" chart, each reached by its label
//   (pinned by the same Rust test). The chart is masked whole, because both
//   its bar height and its value axis scale with the count. On the admin
//   database page: the rows for the two log tables every request feeds
//   (`blocks/admin/pages/database.rs` names each row in `data-db-table`).
//   The whole row is masked, not the count: the count is as wide as its
//   digits, so "95" and "105" would paint different mask boxes. The admin
//   network page is captured against rows it seeds itself (see its test
//   below).
//
// What stays unmasked, deliberately: the dashboard's server-error figure,
// chart and "Recent server errors" card are zero and empty on a healthy run
// (the chart is then one sentence, "No server errors yet — …"), so an error
// there is a regression the capture should catch. The client errors under
// "Requests today" and in their chart are zero in the capture too ("0 client
// errors", and the chart's "No client errors yet — …" sentence), so they are
// compared as well: a refused request the server logs before the capture
// would change both. The capture asserts both figures are zero first, and
// the run's teardown (`fixtures/no-refusals.ts`) fails on any refused
// request anywhere in `e2e:visual`, so either names the request rather
// than leaving a changed baseline to explain. The new-user
// chart plots its one signup on the last day of the window, and "Total users"
// / "New today" are the one bootstrap admin, so none of them depends on what
// ran first. The charts and "New today" do depend on the UTC day: a run
// whose server starts before midnight UTC and captures the dashboard after it
// plots the traffic on two days and fails once; a rerun passes.
function volatileMasks(page: Page) {
  return [
    page
      .locator(
        'td:has(time, [data-relative-time], .relative-time):not(:has(.object-table__meta, .share-table__meta, .session-table__meta, .org-table__meta)), :is([data-relative-time], .relative-time, time):not(td *)',
      )
      .filter({ visible: true }),
    // A narrowing table's meta line (table.css `.object-table`,
    // `.share-table`, `.session-table`, `.org-table`) carries the row's
    // dates under its name: mask the dates, not the name's whole cell, so
    // the name and its badges are still compared. Hidden where the table is
    // wide.
    page.locator(':is(.object-table__meta, .share-table__meta, .session-table__meta, .org-table__meta) time').filter({ visible: true }),
    page.locator('.chart__range'),
    page.locator('td:has([data-volatile-id])'),
    page.locator('td:has([data-volatile-metric]), .stat:has-text("Avg response") .stat__value'),
    page.locator('.stat:has-text("Requests today") .stat__value'),
    page.locator('section.card:has(h2.card__title:text-is("Requests")) .chart'),
    page.locator('li[data-db-table$="__request_logs"], li[data-db-table$="__storage_access_logs"]'),
  ];
}

test.describe('visual baseline — anonymous', () => {
  for (const r of ANON_ROUTES) {
    test(`anon ${r.name}`, async ({ page }) => {
      await page.goto(r.path, { waitUntil: 'networkidle' });
      await expectPageScreenshot(page, `anon-${r.name}.png`);
    });
  }
});

test.describe('visual baseline — admin', () => {
  test.use({ storageState: ADMIN_STATE_PATH });
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
  });
  for (const r of ADMIN_ROUTES) {
    test(`admin ${r.name}`, async ({ page }) => {
      await page.goto(r.path, { waitUntil: 'networkidle' });
      if (r.name === 'admin-dashboard') {
        // Compared unmasked, so said out loud: nothing the run did before
        // this capture was refused. A refusal here names the order problem
        // the baseline would only show as a changed figure (the run's own
        // guard, `fixtures/no-refusals.ts`, names the request).
        await expect(page.locator('.stat:has-text("Requests today") .stat__detail')).toHaveText('0 client errors');
        await expect(page.locator('.stat:has-text("Server errors today") .stat__value')).toHaveText('0');
      }
      await expectPageScreenshot(page, `admin-${r.name}.png`, volatileMasks(page));
      // A refused page is the shared status page (`ui::forbidden_response`).
      // `a11y.spec.ts` cannot open it (it runs before the dashboard capture,
      // whose figures a refusal would move), so its structure is checked
      // here, where it is already requested.
      if (r.refused) expect(await structureViolations(page)).toEqual([]);
    });
  }

  // The inbound table summarises every request the server has logged, so
  // unfiltered it lists whatever the specs and captures before it happened
  // to request. Instead the capture filters the table to a path nothing else
  // in the suite requests (the per-request detail endpoint), after
  // requesting it a known number of times: one row, a known count, no errors.
  // The request log is written before the response on the native server
  // (`pipeline.rs`), so the rows exist once the requests return.
  //
  // The phone capture is taken in the same test, in a touch context of its
  // own, so both see the same three requests: a second test probing the same
  // path would make the count depend on which ran first.
  test('admin admin-network', async ({ page, browser }) => {
    const probe = '/b/admin/network/detail/inbound';
    for (let i = 0; i < 3; i++) {
      const res = await page.request.get(`${probe}?method=GET&path=/visual-baseline`);
      expect(res.status(), `probe request ${i + 1}`).toBe(200);
    }
    const searched = `/b/admin/settings/network?search=${encodeURIComponent(probe)}`;
    await page.goto(searched, { waitUntil: 'networkidle' });
    await expect(page.locator('tr.network-row')).toHaveCount(1);
    await expectPageScreenshot(page, 'admin-admin-network.png', volatileMasks(page));

    const phone = await browser.newContext({
      storageState: ADMIN_STATE_PATH,
      viewport: MOBILE_VIEWPORT,
      ...MOBILE_DEVICE,
    });
    try {
      const phonePage = await phone.newPage();
      await phonePage.goto(searched, { waitUntil: 'networkidle' });
      await expect(phonePage.locator('tr.network-row')).toHaveCount(1);
      expect(await targetFloor(phonePage)).toBe(44);
      await expectPageScreenshot(phonePage, 'admin-admin-network-mobile.png', volatileMasks(phonePage));
    } finally {
      await phone.close();
    }
  });
});

// ===== Phase 4 PR-3: 375px mobile pass =====

const MOBILE_VIEWPORT = { width: 375, height: 812 } as const;
// A phone has touch, and touch -- not the width -- is what raises every
// control to its 44px target (`--target-min`, tokens.css). Without it these
// would be a narrow desktop window: a mouse's compact controls.
const MOBILE_DEVICE = { hasTouch: true } as const;

const MOBILE_ANON_ROUTES = [
  { path: '/b/auth/login', name: 'auth-login' },
  { path: '/b/auth/signup', name: 'auth-signup' },
];

const MOBILE_ADMIN_ROUTES = [
  // The admin core pages at phone width, under the same masks as their
  // desktop captures (`volatileMasks`: the database page's two log-table
  // rows, every date cell). The admin dashboard is the one core page left
  // out: its "Recent server errors" card is fed by the request_log the rest
  // of the baseline run accumulates, so its content and width drift between
  // local and CI. Logs and the storage-access tab are left out for the same
  // reason on desktop too (see ADMIN_ROUTES).
  { path: '/b/admin/users', name: 'admin-users' },
  { path: '/b/admin/blocks', name: 'admin-blocks' },
  { path: '/b/admin/database', name: 'admin-database' },
  { path: '/b/admin/email', name: 'admin-email' },
  { path: '/b/admin/variables', name: 'admin-variables' },
  // The settings tabs. admin-network's phone capture is
  // taken beside its desktop one (above): it seeds the rows it shows.
  { path: '/b/admin/permissions', name: 'admin-permissions' },
  { path: '/b/admin/settings/permissions?subtab=database', name: 'admin-permissions-database' },
  // Portal pages — primary mobile audience.
  { path: '/b/userportal/', name: 'portal-dashboard' },
  { path: '/b/auth/orgs', name: 'portal-orgs' },
  { path: '/b/userportal/profile', name: 'portal-profile' },
  { path: '/b/userportal/sessions', name: 'portal-sessions' },
  { path: '/b/userportal/security', name: 'portal-security' },
  { path: '/b/products/admin/', name: 'products-overview' },
  { path: '/b/products/admin/manage', name: 'products-manage' },
  { path: '/b/products/admin/new', name: 'products-new' },
  { path: '/b/products/admin/groups', name: 'products-groups' },
  { path: '/b/products/admin/purchases', name: 'products-orders' },
  { path: '/b/products/admin/sellers', name: 'products-sellers' },
  { path: '/b/products/admin/stripe', name: 'products-stripe' },
  { path: '/b/products/admin/settings', name: 'products-settings' },
  { path: '/b/products/', name: 'portal-products' },
  { path: '/b/products/my-products', name: 'portal-products-list' },
  { path: '/b/products/my-products/new', name: 'portal-products-new' },
  { path: '/b/products/my-purchases', name: 'portal-products-orders' },
  { path: '/b/products/selling', name: 'portal-products-selling' },
  { path: '/b/products/selling/orders', name: 'portal-products-selling-orders' },
  { path: '/b/llm/', name: 'llm-chat' },
  // Phase 5d Item A's storageState refactor unblocks the cumulative auth
  // rate-limit that PR-3 hit, so `/b/storage/` can safely sit at the end
  // of MOBILE_ADMIN_ROUTES again.
  { path: '/b/storage/', name: 'storage-buckets' },
  // Phase 5d Item C: seeded photos bucket — exercises the table-as-cards
  // mobile rendering of the file browser, not just the bucket list.
  { path: '/b/storage/photos/', name: 'storage-objects' },
  // The share links' flush card and the quota card, and the admin storage
  // overview and bucket list, at phone width.
  { path: '/b/cloudstorage/', name: 'cloudstorage' },
  { path: '/b/storage/admin/', name: 'storage-admin-overview' },
  { path: '/b/storage/admin/buckets', name: 'storage-admin-buckets' },
  // No vector backend on this server: the callout, at phone width.
  { path: '/b/vector/', name: 'vector-list' },
];

// ===== Phase 5b PR-1: vector index admin pages =====
//
// The list page renders the same shell + empty-state composition the unit
// tests cover; the detail page requires a registry row + matching `_meta`
// table, which the production API path can only build via the
// `wafer-run/vector` runtime block (gated behind the `native-embedding`
// feature). The CI baseline runner uses a stock `cargo install` build
// without `native-embedding`, so seeding via `/b/vector/api/indexes`
// would fail there. We snapshot the list-page empty state only —
// detail-page rendering is exercised by `pages_ui::integration_tests`.

test.describe('visual baseline — admin vector', () => {
  test.use({ storageState: ADMIN_STATE_PATH });
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
  });

  test('admin-vector-list-desktop', async ({ page }) => {
    await page.goto('/b/vector/', { waitUntil: 'networkidle' });
    await expectPageScreenshot(page, 'admin-vector-list-desktop.png', volatileMasks(page));
  });
});

test.describe('visual baseline mobile — anonymous (375px)', () => {
  test.use(MOBILE_DEVICE);
  for (const r of MOBILE_ANON_ROUTES) {
    test(`anon-mobile ${r.name}`, async ({ page }) => {
      await page.setViewportSize(MOBILE_VIEWPORT);
      await page.goto(r.path, { waitUntil: 'networkidle' });
      expect(await targetFloor(page)).toBe(44);
      await expectPageScreenshot(page, `anon-${r.name}-mobile.png`);
      // Still touch after the capture, so the baseline is a phone's.
      expect(await targetFloor(page)).toBe(44);
    });
  }
});

test.describe('visual baseline mobile — admin (375px)', () => {
  test.use({ storageState: ADMIN_STATE_PATH, ...MOBILE_DEVICE });
  test.beforeEach(async ({ page }) => {
    await page.setViewportSize(MOBILE_VIEWPORT);
    await loginAsAdmin(page);
  });
  for (const r of MOBILE_ADMIN_ROUTES) {
    test(`admin-mobile ${r.name}`, async ({ page }) => {
      await page.setViewportSize(MOBILE_VIEWPORT);
      await page.goto(r.path, { waitUntil: 'networkidle' });
      expect(await targetFloor(page)).toBe(44);
      await expectPageScreenshot(page, `admin-${r.name}-mobile.png`, volatileMasks(page));
      // Still touch after the capture, so the baseline is a phone's.
      expect(await targetFloor(page)).toBe(44);
      // A refused page's structure, on a phone (see the desktop capture).
      if (ADMIN_ROUTES.find((a) => a.path === r.path)?.refused) {
        expect(await structureViolations(page)).toEqual([]);
      }
    });
  }
});
