import { readFileSync } from 'node:fs';
import { test, expect, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * The stat row (`components::stat_row`), laid out by the served stylesheet at
 * desktop and phone widths.
 *
 * On a wide screen every stat takes an equal column of one row, however many
 * there are, so a fifth stat is never left alone on a second line. Below
 * 720px the stats take two columns, and an odd last stat spans both.
 *
 * The markup is `fixtures/stat-row.html` (a row of five and a row of four),
 * which a Rust test (`the_stat_row_e2e_fixture_is_what_stat_row_renders`)
 * keeps equal to what `stat_row` renders. It is mounted into a real admin
 * page so it gets the page's real stylesheet. Writes nothing (see
 * `e2e:visual`).
 */

const FIXTURE = readFileSync(new URL('./fixtures/stat-row.html', import.meta.url), 'utf8');
const HOST_PAGE = '/b/legalpages/admin/endpoints';
const PHONE = { width: 375, height: 812 } as const;

async function mountFixture(page: Page) {
  await loginAsAdmin(page);
  await page.goto(HOST_PAGE, { waitUntil: 'networkidle' });
  await page.locator('main#content').evaluate((main, html) => {
    main.innerHTML = html;
  }, FIXTURE);
}

/** Each row's stat boxes in CSS pixels, plus the computed type the rules set. */
async function layout(page: Page) {
  return page.locator('.stat-row').evaluateAll((rows) =>
    rows.map((row) => ({
      overflows: row.scrollWidth > row.clientWidth,
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
          valueSize: value.fontSize,
          valueColor: value.color,
        };
      }),
    })),
  );
}

const near = (a: number, b: number) => Math.abs(a - b) < 0.5;

/** Each row on one line, its stats in equal columns, labelled at 14px. */
async function expectOneRowEach(page: Page) {
  const rows = await layout(page);
  expect(rows.map((r) => r.stats.length)).toEqual([5, 4]);
  for (const [r, row] of rows.entries()) {
    expect(row.overflows, `row ${r}`).toBe(false);
    const [first, ...rest] = row.stats;
    for (const [i, stat] of rest.entries()) {
      expect(near(stat.top, first.top), `row ${r} stat ${i + 1} top`).toBe(true);
      expect(near(stat.width, first.width), `row ${r} stat ${i + 1} width`).toBe(true);
      expect(stat.left, `row ${r} stat ${i + 1} left`).toBeGreaterThan(row.stats[i].right);
    }
  }
  for (const stat of rows.flatMap((r) => r.stats)) {
    // Sentence case as written, at the 14px floor; the value at 1.5rem.
    expect(stat.labelCase).toBe('none');
    expect(stat.labelSize).toBe('14px');
    expect(stat.valueSize).toBe('24px');
  }
  const alert = rows[0].stats.find((s) => s.alert)!;
  expect(alert.valueColor).not.toBe(rows[0].stats[0].valueColor);
}

test.describe('a stat row', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  // At 900px a grid of 180px auto-fit columns would wrap both rows.
  for (const width of [1280, 900]) {
    test(`at ${width}px every stat shares one row in equal columns`, async ({ page }) => {
      await page.setViewportSize({ width, height: 860 });
      await mountFixture(page);
      await expectOneRowEach(page);
    });
  }

  test('on a phone the stats take two columns and an odd last stat spans both', async ({
    page,
  }) => {
    await page.setViewportSize(PHONE);
    await mountFixture(page);
    const [five, four] = await layout(page);
    for (const [r, row] of [five, four].entries()) {
      expect(row.overflows, `row ${r}`).toBe(false);
      // Pairs: the left stat of each pair starts the row, the right one ends it.
      const pairs = Math.floor(row.stats.length / 2);
      for (let p = 0; p < pairs; p++) {
        const [left, right] = [row.stats[2 * p], row.stats[2 * p + 1]];
        expect(near(left.top, right.top), `row ${r} pair ${p} top`).toBe(true);
        expect(near(left.left, row.stats[0].left), `row ${r} pair ${p} left`).toBe(true);
        expect(near(left.width, right.width), `row ${r} pair ${p} width`).toBe(true);
        if (p > 0) {
          expect(left.top, `row ${r} pair ${p} wraps`).toBeGreaterThan(row.stats[2 * p - 2].top);
        }
      }
    }
    // The fifth stat has its own line and spans both columns.
    const last = five.stats[4];
    expect(last.top).toBeGreaterThan(five.stats[2].top);
    expect(near(last.left, five.stats[0].left)).toBe(true);
    expect(near(last.right, five.stats[1].right)).toBe(true);
    // An even row's last stat stays in its column.
    expect(near(four.stats[3].width, four.stats[0].width)).toBe(true);
    expect(near(four.stats[3].right, four.stats[1].right)).toBe(true);
  });
});
