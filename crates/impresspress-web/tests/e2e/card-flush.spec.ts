import { readFileSync } from 'node:fs';
import { test, expect, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * A flush card (`components::Card::flush`) holding a data table, laid out by
 * the served stylesheet at desktop and phone widths.
 *
 * The card's frame is the table's (layout rule 2). Below 720px a data table
 * turns each row into a framed box of its own; inside a flush card those
 * boxes would be cards inside a card, clipped against its edges, so there
 * the rows are divided bands at the card's inset. On a wide screen the
 * table's outer cells take the card's inset, so the cells line up with the
 * head.
 *
 * The markup is `fixtures/card-flush.html`, which a Rust test
 * (`the_flush_card_e2e_fixture_is_what_card_renders`) keeps equal to what
 * `Card` renders. It is mounted into a real admin page so it gets the page's
 * real stylesheet. Writes nothing (see `e2e:visual`).
 */

const FIXTURE = readFileSync(new URL('./fixtures/card-flush.html', import.meta.url), 'utf8');
const HOST_PAGE = '/b/legalpages/admin/endpoints';
const PHONE = { width: 375, height: 812 } as const;

async function mountFixture(page: Page) {
  await loginAsAdmin(page);
  await page.goto(HOST_PAGE, { waitUntil: 'networkidle' });
  await page.locator('main#content').evaluate((main, html) => {
    main.innerHTML = html;
  }, FIXTURE);
}

/** Edges in CSS pixels, plus the computed values the rules set. */
async function layout(page: Page) {
  return page.locator('section.card--flush').evaluate((card) => {
    const box = (el: Element) => el.getBoundingClientRect();
    const title = card.querySelector('.card__title')!;
    const table = card.querySelector('.data-table')!;
    const firstTh = card.querySelector('thead th')!;
    const rows = [...card.querySelectorAll('tbody tr')].map((tr) => {
      const s = getComputedStyle(tr);
      const firstCell = tr.querySelector('td')!;
      return {
        left: box(tr).left,
        right: box(tr).right,
        bottom: box(tr).bottom,
        borderLeft: s.borderLeftWidth,
        borderRight: s.borderRightWidth,
        borderTop: s.borderTopWidth,
        marginBottom: s.marginBottom,
        textLeft: box(firstCell).left + parseFloat(getComputedStyle(firstCell).paddingLeft),
      };
    });
    const cardBox = box(card);
    const border = parseFloat(getComputedStyle(card).borderLeftWidth);
    return {
      inner: { left: cardBox.left + border, right: cardBox.right - border, bottom: cardBox.bottom - border },
      titleLeft: box(title).left,
      tableLeft: box(table).left,
      tableBorder: getComputedStyle(table).borderLeftWidth,
      headTextLeft: box(firstTh).left + parseFloat(getComputedStyle(firstTh).paddingLeft),
      rows,
    };
  });
}

test.describe('a flush card holding a data table', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  test('on a phone its rows are divided bands at the card inset, not cards', async ({ page }) => {
    await page.setViewportSize(PHONE);
    await mountFixture(page);
    const l = await layout(page);
    expect(l.rows).toHaveLength(2);
    for (const [i, row] of l.rows.entries()) {
      // No frame and no gap of its own: the card is the frame.
      expect(row.borderLeft, `row ${i}`).toBe('0px');
      expect(row.borderRight, `row ${i}`).toBe('0px');
      expect(row.marginBottom, `row ${i}`).toBe('0px');
      // Edge to edge inside the card's border.
      expect(Math.abs(row.left - l.inner.left), `row ${i} left`).toBeLessThan(0.5);
      expect(Math.abs(row.right - l.inner.right), `row ${i} right`).toBeLessThan(0.5);
      // Its text starts where the head's does.
      expect(Math.abs(row.textLeft - l.titleLeft), `row ${i} text`).toBeLessThan(0.5);
    }
    expect(l.rows[0].borderTop).toBe('0px');
    expect(l.rows[1].borderTop).toBe('1px');
    expect(Math.abs(l.rows[1].bottom - l.inner.bottom)).toBeLessThan(0.5);
  });

  test('on a wide screen the table has no frame and its cells line up with the head', async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1280, height: 860 });
    await mountFixture(page);
    const l = await layout(page);
    expect(l.tableBorder).toBe('0px');
    expect(Math.abs(l.tableLeft - l.inner.left)).toBeLessThan(0.5);
    expect(Math.abs(l.headTextLeft - l.titleLeft)).toBeLessThan(0.5);
    for (const [i, row] of l.rows.entries()) {
      expect(Math.abs(row.textLeft - l.titleLeft), `row ${i} text`).toBeLessThan(0.5);
    }
  });
});
