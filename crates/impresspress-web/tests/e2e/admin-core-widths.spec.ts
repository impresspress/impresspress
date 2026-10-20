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
 * The widths: 800 (the filter wraps; the database cards stack), and 960 and
 * 1024 (the database cards side by side, the Tables column at its
 * narrowest). Writes nothing (see `e2e:visual`).
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
  }
});
