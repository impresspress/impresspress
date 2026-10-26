import { readFileSync } from 'node:fs';
import { test, expect, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * The stat row (`components::stat_row`), laid out by the served stylesheet
 * from a phone to a wide desktop.
 *
 * The row picks its layout from its own width (`.stat-row` is a size
 * container), against a floor of 11rem per stat (`stat.css`):
 * - every stat in one row of equal columns when each gets the floor;
 * - otherwise two columns, an odd last stat spanning both (no stat alone
 *   beside an empty column);
 * - one column when two do not fit.
 * Whatever the layout, no value word breaks mid-word: the fixture carries the
 * widest figures and markers the pages show. A value is never free text (an
 * email), so no word is exempt.
 *
 * The markup is `fixtures/stat-row.html` (the dashboard, the dashboard during
 * an outage, an order, a seller's state words, and a single stat), which a Rust test
 * (`the_stat_row_e2e_fixture_is_what_stat_row_renders`) keeps equal to what
 * `stat_row` renders. It is mounted into a real admin page so it gets the
 * page's real stylesheet and shell. Writes nothing (see `e2e:visual`).
 */

const FIXTURE = readFileSync(new URL('./fixtures/stat-row.html', import.meta.url), 'utf8');
const HOST_PAGE = '/b/legalpages/admin/endpoints';
const PHONE = { width: 375, height: 812 } as const;
/** `stat.css`: the per-stat floor and the gap, in rem. */
const FLOOR_REM = 11;
const GAP_REM = 0.75;

async function mountFixture(page: Page) {
  await loginAsAdmin(page);
  await page.goto(HOST_PAGE, { waitUntil: 'networkidle' });
  await page.locator('main#content').evaluate((main, html) => {
    main.innerHTML = html;
  }, FIXTURE);
}

/** Each row's width, its stat boxes, and every value word that broke. */
async function layout(page: Page) {
  return page.locator('.stat-row').evaluateAll((rows) =>
    rows.map((row) => {
      const rem = parseFloat(getComputedStyle(document.documentElement).fontSize);
      const values = [...row.querySelectorAll('.stat__value')];
      const brokenWords = values.flatMap((value) => {
        const text = value.firstChild as Text;
        return [...text.data.matchAll(/\S+/g)]
          .filter((m) => {
            const range = document.createRange();
            range.setStart(text, m.index!);
            range.setEnd(text, m.index! + m[0].length);
            return range.getClientRects().length > 1;
          })
          .map((m) => m[0]);
      });
      return {
        widthRem: row.getBoundingClientRect().width / rem,
        brokenWords,
        stats: [...row.querySelectorAll('.stat')].map((stat) => {
          const b = stat.getBoundingClientRect();
          const label = getComputedStyle(stat.querySelector('.stat__label')!);
          const value = getComputedStyle(stat.querySelector('.stat__value')!);
          return {
            left: b.left,
            right: b.right,
            top: b.top,
            width: b.width,
            alert: stat.classList.contains('stat--alert'),
            labelSize: label.fontSize,
            labelCase: label.textTransform,
            valueColor: value.color,
          };
        }),
      };
    }),
  );
}

type Row = Awaited<ReturnType<typeof layout>>[number];
type Shape = 'one row' | 'two columns' | 'one column';

const near = (a: number, b: number) => Math.abs(a - b) < 0.5;
const needs = (columns: number) => columns * FLOOR_REM + (columns - 1) * GAP_REM;

/** The layout `stat.css` gives a row of this many stats at this width. */
function expectedShape(row: Row): Shape {
  const n = row.stats.length;
  if (n > 1 && row.widthRem >= needs(n)) return 'one row';
  if (row.widthRem >= needs(2)) return 'two columns';
  return 'one column';
}

/** The row has `shape`, and no stat sits alone beside an empty column. */
function expectShape(row: Row, shape: Shape, what: string) {
  const { stats } = row;
  const [first] = stats;
  const last = stats[stats.length - 1];
  if (shape === 'one row') {
    for (const [i, stat] of stats.entries()) {
      expect(near(stat.top, first.top), `${what} stat ${i} top`).toBe(true);
      expect(near(stat.width, first.width), `${what} stat ${i} width`).toBe(true);
    }
    return;
  }
  // Stacked layouts: every row of the grid starts at the first stat's left
  // edge, and the last stat ends at the row's right edge.
  const right = Math.max(...stats.map((s) => s.right));
  if (shape === 'one column') {
    for (const [i, stat] of stats.entries()) {
      expect(near(stat.left, first.left), `${what} stat ${i} left`).toBe(true);
      expect(near(stat.right, right), `${what} stat ${i} right`).toBe(true);
    }
    return;
  }
  for (let p = 0; 2 * p + 1 < stats.length; p++) {
    const [left, other] = [stats[2 * p], stats[2 * p + 1]];
    expect(near(left.top, other.top), `${what} pair ${p} top`).toBe(true);
    expect(near(left.left, first.left), `${what} pair ${p} left`).toBe(true);
    expect(near(left.width, other.width), `${what} pair ${p} width`).toBe(true);
    expect(other.left, `${what} pair ${p} columns`).toBeGreaterThan(left.right);
  }
  if (stats.length % 2 === 1) {
    expect(near(last.left, first.left), `${what} odd last left`).toBe(true);
    expect(near(last.right, right), `${what} odd last spans`).toBe(true);
  }
}

const NAMES = ['dashboard', 'outage', 'order', 'seller', 'single'];

test.describe('a stat row', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  for (const width of [1280, 1024, 900, 721]) {
    test(`at ${width}px each row fits its width and no figure breaks`, async ({ page }) => {
      await page.setViewportSize({ width, height: 860 });
      await mountFixture(page);
      const rows = await layout(page);
      expect(rows.map((r) => r.stats.length)).toEqual([5, 5, 3, 4, 1]);
      for (const [r, row] of rows.entries()) {
        expectShape(row, expectedShape(row), `${NAMES[r]} at ${width}px`);
        expect(row.brokenWords, `${NAMES[r]} at ${width}px`).toEqual([]);
      }
      if (width === 1280) {
        // The design target: a full dashboard and the rows of three and four,
        // each in one row.
        expect(rows.map(expectedShape).slice(0, 4)).toEqual([
          'one row',
          'one row',
          'one row',
          'one row',
        ]);
      }
      for (const stat of rows.flatMap((r) => r.stats)) {
        // Sentence case as written, at the 14px floor.
        expect(stat.labelCase).toBe('none');
        expect(stat.labelSize).toBe('14px');
      }
      const alert = rows[0].stats.find((s) => s.alert)!;
      expect(alert.valueColor).not.toBe(rows[0].stats[0].valueColor);
    });
  }

  test('on a phone the stats stack in one column and no figure breaks', async ({ page }) => {
    await page.setViewportSize(PHONE);
    await mountFixture(page);
    const rows = await layout(page);
    for (const [r, row] of rows.entries()) {
      expect(expectedShape(row), NAMES[r]).toBe('one column');
      expectShape(row, 'one column', `${NAMES[r]} on a phone`);
      expect(row.brokenWords, NAMES[r]).toEqual([]);
    }
  });
});
