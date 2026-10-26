import { test, expect, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';
import { layout, overflows } from './fixtures/layout';
import { expectAtMostOnePrimary } from './fixtures/primaries';
import { targetFloor } from './fixtures/targets';

/**
 * The admin and portal pages on a phone (PLAN §5.5): the visual suite's
 * `MOBILE_VIEWPORT`, 375×812 with touch, so these are a phone's controls and
 * not a narrow desktop window's. Where the baselines compare pixels, this
 * names the layout rules a phone page keeps:
 * - nothing scrolls sideways: the document is exactly the screen's width,
 *   and no table, card or stat overflows (`fixtures/layout.ts`);
 * - the page header stacks: the title on its row, with the subject's status
 *   beside it (`.topbar__status`, never wrapped into the buttons' row), and
 *   the actions on a row of their own below;
 * - a stat row is one column (stat.css; `stat-row.spec.ts` pins the
 *   in-between widths on fixtures, this the real pages);
 * - settings labels sit above their controls (`.form--aligned` aligns only
 *   from 40rem of form);
 * - header actions and tabs meet the touch floor (`targetFloor`, 44px);
 * - a row of buttons spans the screen where its row says so: a form modal's
 *   footer (`.modal__footer`), and the portal Profile and Security submits
 *   (`.account-form__actions`), since the `.btn--block` class is gone and
 *   the row carries the layout;
 * - at most one primary shows (`fixtures/primaries.ts`).
 *
 * The dashboard runs the layout checks but is not captured anywhere at this
 * width (its request figures drift with the run; see `visual-baseline.spec.ts`).
 * Reads only, so part of `e2e:visual`.
 */

const MOBILE_VIEWPORT = { width: 375, height: 812 } as const;

const PAGES = [
  '/b/admin/',
  '/b/admin/users',
  '/b/admin/database',
  '/b/admin/blocks',
  '/b/admin/logs',
  '/b/admin/email',
  '/b/storage/admin/',
  '/b/products/admin/',
  '/b/userportal/security',
];

/** The header's parts, as boxes, and the controls a finger must hit. */
async function header(page: Page) {
  return page.evaluate(() => {
    const box = (sel: string) => {
      const el = document.querySelector<HTMLElement>(`header.topbar > ${sel}`);
      if (!el || !el.checkVisibility()) return null;
      const r = el.getBoundingClientRect();
      return { top: Math.round(r.top), bottom: Math.round(r.bottom), left: Math.round(r.left), right: Math.round(r.right) };
    };
    const title = document.querySelector('.topbar__title')!.getBoundingClientRect();
    const titleLine = { top: Math.round(title.top), bottom: Math.round(title.bottom) };
    const small = [...document.querySelectorAll<HTMLElement>('.topbar__actions .btn, .tabs .tab')]
      .filter((el) => el.checkVisibility())
      .map((el) => ({ name: el.textContent!.trim(), height: el.getBoundingClientRect().height }));
    return { heading: box('.topbar__heading'), title: titleLine, status: box('.topbar__status'), actions: box('.topbar__actions'), small };
  });
}

/** Each element in `sel`'s row spans its parent's content box together with
 * its siblings: the first starts at the row's left edge and the last ends at
 * its right, on one line. */
async function fillsItsRow(page: Page, row: string) {
  return page.locator(row).first().evaluate((el) => {
    const cs = getComputedStyle(el);
    const r = el.getBoundingClientRect();
    const inner = { left: r.left + parseFloat(cs.paddingLeft), right: r.right - parseFloat(cs.paddingRight) };
    const buttons = [...el.children].filter((c) => c.classList.contains('btn') && (c as HTMLElement).checkVisibility());
    const boxes = buttons.map((b) => b.getBoundingClientRect());
    return {
      count: boxes.length,
      startsAtEdge: Math.abs(boxes[0].left - inner.left) <= 1,
      endsAtEdge: Math.abs(boxes[boxes.length - 1].right - inner.right) <= 1,
      oneLine: new Set(boxes.map((b) => Math.round(b.top))).size === 1,
    };
  });
}

test.describe('admin and portal pages on a phone', () => {
  test.use({ storageState: ADMIN_STATE_PATH, viewport: MOBILE_VIEWPORT, hasTouch: true });
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
  });

  for (const path of PAGES) {
    test(`${path} keeps the phone layout`, async ({ page }) => {
      await page.goto(path, { waitUntil: 'networkidle' });
      expect(await targetFloor(page)).toBe(44);

      const r = await layout(page);
      expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(MOBILE_VIEWPORT.width);
      expect(overflows(r), JSON.stringify(r)).toBe(false);
      await expectAtMostOnePrimary(page);

      const h = await header(page);
      if (h.actions) {
        expect(h.actions.top, `the actions sit below the title: ${JSON.stringify(h)}`).toBeGreaterThanOrEqual(h.heading!.bottom);
      }
      if (h.status) {
        // On the title's row: it overlaps the title line vertically, and it
        // is above the actions.
        expect(h.status.top < h.title.bottom && h.status.bottom > h.title.top, `the status sits on the title's row: ${JSON.stringify(h)}`).toBe(true);
        if (h.actions) expect(h.status.bottom).toBeLessThanOrEqual(h.actions.top);
      }
      expect(h.small.filter((c) => c.height < 44 - 0.5), 'a header action or tab under the touch floor').toEqual([]);

      const stats = await page.locator('.stat').evaluateAll((all) =>
        all.filter((s) => (s as HTMLElement).checkVisibility()).map((s) => Math.round(s.getBoundingClientRect().left)),
      );
      expect(new Set(stats).size, `the stats stack in one column: ${JSON.stringify(stats)}`).toBeLessThanOrEqual(1);

      const labels = await page.locator('.form-group:has(> .form-label)').evaluateAll((groups) =>
        groups
          .filter((g) => (g as HTMLElement).checkVisibility())
          .map((g) => {
            const label = g.querySelector(':scope > .form-label')!.getBoundingClientRect();
            const control = g.querySelector(':scope > :not(.form-label)')!.getBoundingClientRect();
            return { label: g.querySelector(':scope > .form-label')!.textContent!.trim(), above: label.bottom <= control.top + 0.5 };
          })
          .filter((g) => !g.above)
          .map((g) => g.label),
      );
      expect(labels, 'a settings label beside its control').toEqual([]);
    });
  }

  test('the database status sits beside the title, not among the actions', async ({ page }) => {
    await page.goto('/b/admin/database', { waitUntil: 'networkidle' });
    const h = await header(page);
    expect(h.status, 'the database page declares its backend as a status').not.toBeNull();
    expect(h.status!.top < h.title.bottom && h.status!.bottom > h.title.top, JSON.stringify(h)).toBe(true);
  });

  test('a form modal footer spans the screen', async ({ page }) => {
    await page.goto('/b/admin/settings/permissions?subtab=database', { waitUntil: 'networkidle' });
    await page.locator('[data-action="modal-open"][data-modal-target="add-grant-modal"]').click();
    const dialog = page.locator('dialog#add-grant-modal');
    await expect(dialog).toBeVisible();
    const row = await fillsItsRow(page, 'dialog#add-grant-modal .modal__footer');
    expect(row).toEqual({ count: 2, startsAtEdge: true, endsAtEdge: true, oneLine: true });
  });

  for (const path of ['/b/userportal/profile', '/b/userportal/security']) {
    test(`the ${path} submit spans the screen`, async ({ page }) => {
      await page.goto(path, { waitUntil: 'networkidle' });
      const row = await fillsItsRow(page, '.account-form__actions');
      expect(row).toEqual({ count: 1, startsAtEdge: true, endsAtEdge: true, oneLine: true });
    });
  }
});
