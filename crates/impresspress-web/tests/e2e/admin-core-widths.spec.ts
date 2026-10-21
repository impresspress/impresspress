import { test, expect, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * The admin core pages between the phone and the 1280px baselines, where
 * the sidebar leaves the content column narrow and the captures never look:
 * - Blocks: the tab row wraps its runtime filter onto a line of its own when
 *   the row is too narrow, and the current tab's underline must still sit on
 *   the tabs' rule, with no rule drawn further down under the filter
 *   (card.css `.block-tabs`).
 * - Database: each block's fold head keeps the block's name on one line and
 *   the name and org clear of the count, however narrow the Tables column
 *   (table.css `.db-table-group__head`).
 * - Settings › Network: each block's route table fits its card with the
 *   route keeping at least 10rem, because the table drops columns by its own
 *   width (table.css `.network-routes`); at 1280 it shows all seven.
 * - Settings › Permissions: "Add grant" stays on the Custom grants title's
 *   row beside the long subtitle (card.css `.card__head`). (Its grant tables
 *   at narrow widths, with a seeded grant, are in `admin-core-pages.spec.ts`,
 *   which may write.)
 * - The card head rule on the pages it reaches most, at 375, 721, 1024 and
 *   1200: on the dashboard and on Products › Stripe a head wraps only when
 *   its title's longest word and its other parts cannot share the line, and
 *   no dashboard head wraps at all.
 * - Settings › Variables: the value column keeps its share (`VAR_COLUMNS`),
 *   and "Add variable" ends the search's row.
 * - Storage, portal Files and Shares, and Vector indexes, at every width from
 *   the phone to 1280: no page scrolls sideways and no table scrolls inside
 *   its frame, because the file, share and share-link tables drop columns by
 *   their own width (table.css `.object-table`, `.share-table`). This
 *   server holds no share link and only short file names, so the tables'
 *   narrow layouts are checked with seeded rows in
 *   `files-messages-pages.spec.ts` (`e2e:writes`).
 * - Products' catalog pages (overview, All products and its Deleted view,
 *   Groups, the wizard), at every width from the phone to
 *   1280: no page scrolls sideways, no table scrolls inside its frame, and
 *   one primary is visible; the wizard's configurable pricing step and its
 *   shipping step fit their card, and the group editor's dialog fits the
 *   screen with its own one primary. This server's catalog is empty, so the
 *   product and group tables' narrow layouts (table.css `.product-table`,
 *   `.group-table`) are checked with seeded rows in `admin-products.spec.ts`
 *   (`e2e:writes`). My products is left out: this server has seller
 *   products off, so the page is a 403, and a refused request here would
 *   reach the dashboard capture's client-error count.
 * The widths: 800 (the filter wraps; the database cards stack; the settings
 * rail is a tab row), and 960 and 1024 (the database cards side by side, the
 * Tables column at its narrowest). Writes nothing beyond the page loads'
 * own request-log rows (see `e2e:visual`).
 */

const WIDTHS = [800, 960, 1024] as const;

async function open(page: Page, width: number, path: string) {
  await page.setViewportSize({ width, height: 900 });
  await loginAsAdmin(page);
  await page.goto(path, { waitUntil: 'networkidle' });
}

test.describe('admin core pages at in-between widths', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  for (const width of WIDTHS) {
    test(`blocks at ${width}px: the current tab's underline sits on the only rule`, async ({
      page,
    }) => {
      await open(page, width, '/b/admin/blocks');
      const found = await page.locator('.block-tabs').evaluate((row) => {
        const tabs = row.querySelector<HTMLElement>(':scope > .tabs')!;
        const active = tabs.querySelector<HTMLElement>('.tab.active')!;
        const tabsBox = tabs.getBoundingClientRect();
        // Every bottom border drawn in the row, by where it is drawn.
        const rules = [row, ...row.querySelectorAll<HTMLElement>('*')]
          .filter((el) => !el.matches('select, input, option, .form-input'))
          .filter((el) => parseFloat(getComputedStyle(el).borderBottomWidth) > 0)
          .map((el) => ({
            what: el.className || el.tagName,
            y: Math.round(el.getBoundingClientRect().bottom),
          }));
        return {
          rules,
          tabsBottom: Math.round(tabsBox.bottom),
          activeBottom: Math.round(active.getBoundingClientRect().bottom),
          wrapped:
            row.querySelector<HTMLElement>('.block-tabs__filter')!.getBoundingClientRect().top >=
            tabsBox.bottom,
        };
      });
      expect(found.rules, 'the tabs draw the row\'s one rule').toEqual([
        { what: 'tabs', y: found.tabsBottom },
      ]);
      // The underline is the active tab's ::after, 2px over the tabs' 1px
      // rule at the tab's bottom edge.
      expect(Math.abs(found.activeBottom - (found.tabsBottom - 1))).toBeLessThanOrEqual(1);
      if (width === 800) expect(found.wrapped, 'the filter wraps at 800px').toBe(true);
    });

    test(`database at ${width}px: each fold head keeps its name whole and clear of the count`, async ({
      page,
    }) => {
      await open(page, width, '/b/admin/database');
      const heads = await page.locator('.db-table-group__head').evaluateAll((all) =>
        all.map((head) => {
          const title = head.querySelector<HTMLElement>('.db-table-group__title')!;
          const org = head.querySelector<HTMLElement>('.db-table-group__org')!;
          const count = head.querySelector<HTMLElement>('.db-count')!.getBoundingClientRect();
          const lines = (el: HTMLElement) => {
            const range = document.createRange();
            range.selectNodeContents(el);
            return new Set([...range.getClientRects()].map((r) => Math.round(r.top))).size;
          };
          const clear = (el: HTMLElement) => el.getBoundingClientRect().right <= count.left + 0.5;
          return {
            name: `${title.textContent} ${org.textContent}`,
            titleLines: lines(title),
            orgWordsWhole: (org.textContent ?? '').split(' ').length >= lines(org),
            clear: clear(title) && clear(org),
          };
        }),
      );
      expect(heads.length).toBeGreaterThan(5);
      expect(
        heads.filter((h) => h.titleLines !== 1 || !h.orgWordsWhole || !h.clear),
        'a block name broke mid-word or ran into the count',
      ).toEqual([]);
      expect(heads.map((h) => h.name)).toContain('legalpages impresspress');
    });

    test(`permissions at ${width}px: Add grant stays on the custom grants title's row`, async ({
      page,
    }) => {
      await open(page, width, '/b/admin/settings/permissions?subtab=database');
      const card = page.locator('section.card', { has: page.locator('h2.card__title', { hasText: 'Custom grants' }) });
      const title = await card.locator('h2.card__title').boundingBox();
      const action = await card.locator('.card__actions .btn').boundingBox();
      expect(title && action, 'the title and its action').toBeTruthy();
      expect(action!.y, JSON.stringify({ title, action })).toBeLessThan(title!.y + title!.height);
      expect(action!.x).toBeGreaterThan(title!.x + title!.width);
    });

    test(`variables at ${width}px: the value column keeps its share and Add variable ends the search row`, async ({
      page,
    }) => {
      await open(page, width, '/b/admin/settings/variables');
      const share = await page
        .locator('[data-fold="variable"] table')
        .first()
        .evaluate((table) => {
          const value = [...table.querySelectorAll('thead th')].find((th) => th.textContent === 'Value')!;
          return value.getBoundingClientRect().width / table.getBoundingClientRect().width;
        });
      expect(share).toBeGreaterThanOrEqual(0.3);
      const search = await page.locator('.filter-bar .search-input').boundingBox();
      const add = await page.getByRole('button', { name: 'Add variable' }).boundingBox();
      expect(Math.abs(search!.y + search!.height / 2 - (add!.y + add!.height / 2))).toBeLessThanOrEqual(1);
    });
  }

  for (const width of [...WIDTHS, 1280]) {
    test(`network at ${width}px: each route table fits its card and the route keeps its share`, async ({
      page,
    }) => {
      await open(page, width, '/b/admin/settings/network');
      // A second load, so the first one's own request is listed.
      await page.reload({ waitUntil: 'networkidle' });
      const tables = await page.locator('.network-routes').evaluateAll((all) =>
        all.map((routes) => {
          const scroller = routes.querySelector<HTMLElement>(':scope > .data-table')!;
          const table = scroller.querySelector('table')!.getBoundingClientRect();
          const route = scroller.querySelector('tbody td.data-table__cell--primary')!.getBoundingClientRect();
          const columns = [...scroller.querySelectorAll('thead th')].filter(
            (th) => getComputedStyle(th).display !== 'none',
          ).length;
          return { fits: table.width <= scroller.clientWidth + 0.5, route: Math.round(route.width), columns };
        }),
      );
      expect(tables.length).toBeGreaterThan(0);
      expect(
        tables.filter((t) => !t.fits || t.route < 160),
        'a route table scrolled sideways or squeezed its route',
      ).toEqual([]);
      if (width === 1280) {
        expect(tables.map((t) => t.columns), 'all seven columns beside the rail at 1280').toEqual(
          tables.map(() => 7),
        );
      }
    });
  }

  for (const width of [375, 721, 1024, 1200]) {
    for (const path of ['/b/admin/', '/b/products/admin/stripe']) {
      test(`card heads at ${width}px on ${path}: a head wraps only when its parts cannot share a line`, async ({
        page,
      }) => {
        await open(page, width, path);
        const heads = await page.locator('.card__head').evaluateAll((all) =>
          all
            .filter((head) => head.children.length > 1)
            .map((head) => {
              const [first, ...rest] = [...head.children] as HTMLElement[];
              const title = head.querySelector('.card__title, h2, h3')!;
              const titleBottom = title.getBoundingClientRect().bottom;
              const wrapped = rest.some((el) => el.getBoundingClientRect().top >= titleBottom);
              // What the line would need: the title block at its narrowest
              // (its longest word) and every other part at its natural width.
              // Measured on an out-of-flow copy in the head itself, so the
              // flex sizing of the real one does not stand in for it.
              const natural = (el: HTMLElement, size: string) => {
                const copy = el.cloneNode(true) as HTMLElement;
                Object.assign(copy.style, { position: 'absolute', visibility: 'hidden', width: size, flex: 'none' });
                head.appendChild(copy);
                const w = copy.getBoundingClientRect().width;
                copy.remove();
                return w;
              };
              const cs = getComputedStyle(head);
              const gap = parseFloat(cs.columnGap) || 0;
              const inner = head.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight);
              const needs =
                natural(first, 'min-content') +
                rest.reduce((sum, el) => sum + natural(el, 'max-content') + gap, 0);
              return { title: title.textContent!.trim(), wrapped, couldShare: needs <= inner - 1 };
            }),
        );
        expect(heads.length).toBeGreaterThan(0);
        expect(
          heads.filter((h) => h.wrapped && h.couldShare).map((h) => h.title),
          'a head whose title and actions fit on one line wrapped anyway',
        ).toEqual([]);
        // The dashboard's heads are a title and a short link: they always
        // share the line (PR-15 review F1 measured all of them wrapping).
        if (path === '/b/admin/') {
          expect(heads.filter((h) => h.wrapped).map((h) => h.title)).toEqual([]);
        }
      });
    }
  }

  const STORAGE_PAGES = [
    '/b/storage/',
    '/b/storage/photos/',
    '/b/cloudstorage/',
    '/b/storage/admin/',
    '/b/storage/admin/buckets',
    '/b/storage/admin/shares',
    '/b/storage/admin/quotas',
    '/b/vector/',
  ];
  for (const width of [375, 721, 800, 900, 960, 1024, 1100, 1200, 1280]) {
    test(`storage and vector pages at ${width}px: nothing scrolls sideways`, async ({ page }) => {
      await open(page, width, STORAGE_PAGES[0]);
      const found: Record<string, unknown> = {};
      for (const path of STORAGE_PAGES) {
        await page.goto(path, { waitUntil: 'networkidle' });
        const r = await page.evaluate(() => ({
          page: document.documentElement.scrollWidth - document.documentElement.clientWidth,
          tables: [...document.querySelectorAll<HTMLElement>('.data-table')]
            .filter((t) => t.scrollWidth > t.clientWidth + 1)
            .map((t) => `${t.scrollWidth}/${t.clientWidth}`),
        }));
        if (r.page > 0 || r.tables.length) found[path] = r;
      }
      expect(found, 'a page or a table scrolled sideways').toEqual({});
      // The seeded bucket's file rows keep their menu in view.
      await page.goto('/b/storage/photos/', { waitUntil: 'networkidle' });
      const trigger = await page.getByRole('button', { name: 'Actions for a.png' }).boundingBox();
      expect(trigger!.x + trigger!.width).toBeLessThanOrEqual(width);
    });
  }

  const PRODUCT_PAGES = [
    '/b/products/admin/',
    '/b/products/admin/manage',
    '/b/products/admin/manage?view=deleted',
    '/b/products/admin/groups',
    '/b/products/admin/new',
  ];
  /** What overflows, and the primaries a reader sees outside any dialog. */
  const layout = (page: Page) =>
    page.evaluate(() => ({
      page: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      tables: [...document.querySelectorAll<HTMLElement>('.data-table')]
        .filter((t) => t.checkVisibility() && t.scrollWidth > t.clientWidth + 1)
        .map((t) => `${t.scrollWidth}/${t.clientWidth}`),
      cards: [...document.querySelectorAll<HTMLElement>('.card')]
        .filter((c) => c.checkVisibility() && c.scrollWidth > c.clientWidth + 1)
        .map((c) => c.querySelector('.card__title')?.textContent ?? 'card'),
      primaries: [...document.querySelectorAll<HTMLElement>('.btn--primary')]
        .filter((b) => b.checkVisibility() && !b.closest('dialog'))
        .map((b) => b.textContent!.trim()),
    }));
  for (const width of [375, 721, 800, 900, 960, 1024, 1100, 1200, 1280]) {
    test(`products catalog pages at ${width}px: nothing scrolls sideways and one primary shows`, async ({
      page,
    }) => {
      await open(page, width, PRODUCT_PAGES[0]);
      const found: Record<string, unknown> = {};
      for (const path of PRODUCT_PAGES) {
        await page.goto(path, { waitUntil: 'networkidle' });
        const r = await layout(page);
        // The Deleted view has nothing to create, so no primary.
        const primaries = path.endsWith('view=deleted') ? 0 : 1;
        if (r.page > 0 || r.tables.length || r.cards.length || r.primaries.length !== primaries) found[path] = r;
      }
      expect(found, 'a page scrolled sideways or showed other than one primary').toEqual({});

      // The wizard, stepped through in the browser (nothing is saved): the
      // configurable pricing step with its rows, then checkout with the
      // shipping settings open. Each step keeps one primary in view.
      await page.goto('/b/products/admin/new', { waitUntil: 'networkidle' });
      await page.getByText('Configurable one-time product', { exact: true }).click();
      await page.getByRole('button', { name: 'Continue' }).click();
      await page.getByLabel('Product name').fill('Width check');
      await page.getByRole('button', { name: 'Continue' }).click();
      await expect(page.locator('[data-wizard-step="3"] [data-component-row]')).toHaveCount(2);
      const pricing = await layout(page);
      await page.getByRole('button', { name: 'Continue' }).click();
      await page.getByText('Shipping address', { exact: true }).click();
      await expect(page.locator('#wizard-shipping-settings')).toBeVisible();
      const checkout = await layout(page);
      for (const [step, r] of Object.entries({ pricing, checkout })) {
        expect({ step, ...r }).toEqual({ step, page: 0, tables: [], cards: [], primaries: ['Continue'] });
      }

      // The group editor is a dialog that fits the screen, with its own
      // primary; the page behind keeps its one.
      await page.goto('/b/products/admin/groups', { waitUntil: 'networkidle' });
      await page.locator('.topbar__actions').getByRole('button', { name: 'New group' }).click();
      const dialog = page.getByRole('dialog', { name: 'New group' });
      await expect(dialog).toBeVisible();
      await expect(dialog.getByLabel('Name')).toBeFocused();
      const box = await dialog.boundingBox();
      expect(box!.x >= 0 && box!.x + box!.width <= width, JSON.stringify(box)).toBe(true);
      await expect(dialog.locator('.btn--primary')).toHaveText(['Save group']);
      expect((await layout(page)).primaries).toEqual(['New group']);
    });
  }
});
