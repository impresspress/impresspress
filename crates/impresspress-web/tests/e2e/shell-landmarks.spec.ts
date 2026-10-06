import { test, expect, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * The app shell's landmarks and its keyboard-reachable scroller
 * (`ui/shell.rs`, `ui/styles/layouts/shell.css`).
 *
 * The document never scrolls: `.shell` is `100vh` and the page scrolls inside
 * `main.shell__body`. So `main` is a Tab stop — a page with nothing focusable
 * in its body would otherwise be unscrollable from the keyboard once focus is
 * anywhere but `<body>`. The page used here is the legal pages' Endpoints
 * reference, which is long enough to overflow; its tables are focusable
 * scroll regions of their own, so the click test aims at plain text. The topbar is the one banner at every width; the phone's mobile
 * header is a labelled `nav`, not a second `header`. The ring is a box-shadow
 * plus a transparent outline, which is what forced-colors mode paints.
 *
 * Writes nothing (see `e2e:visual`).
 */

const STATIC_PAGE = '/b/legalpages/admin/endpoints';

async function scrollTop(page: Page): Promise<number> {
  return page.locator('main#content').evaluate((el) => el.scrollTop);
}

test.describe('shell landmarks and keyboard scrolling', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  test('the skip link focuses main, which the keyboard then scrolls', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto(STATIC_PAGE, { waitUntil: 'networkidle' });
    const main = page.locator('main#content');
    // The page must overflow for this to prove anything.
    expect(await main.evaluate((el) => el.scrollHeight > el.clientHeight)).toBe(true);

    // The skip link is the first Tab stop and moves focus to `main`.
    await page.keyboard.press('Tab');
    await expect(page.locator('a.skip-link')).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(main).toBeFocused();
    // Focus there is visible: the box-shadow ring, plus the transparent
    // outline forced-colors mode (which drops box-shadows) paints instead.
    const ring = await main.evaluate((el) => {
      const s = getComputedStyle(el);
      return { shadow: s.boxShadow, outlineStyle: s.outlineStyle, outlineWidth: s.outlineWidth };
    });
    expect(ring.shadow).not.toBe('none');
    expect(ring.outlineStyle).toBe('solid');
    expect(ring.outlineWidth).toBe('2px');

    // The browser's own scrolling of the focused element (chrome.js's
    // fallback only handles keys aimed at `<body>`).
    await page.keyboard.press('PageDown');
    await expect.poll(() => scrollTop(page)).toBeGreaterThan(0);
    await page.keyboard.press('End');
    await expect
      .poll(() => main.evaluate((el) => Math.abs(el.scrollTop - (el.scrollHeight - el.clientHeight)) <= 1))
      .toBe(true);
  });

  test('Tab reaches main after the chrome, and a mouse click shows no ring', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto(STATIC_PAGE, { waitUntil: 'networkidle' });
    const main = page.locator('main#content');

    // Tabbing through the sidebar and the topbar lands on `main`.
    let reached = false;
    for (let i = 0; i < 80 && !reached; i++) {
      await page.keyboard.press('Tab');
      reached = await main.evaluate((el) => document.activeElement === el);
    }
    expect(reached, 'Tab never reached main#content').toBe(true);
    await page.keyboard.press('ArrowDown');
    await expect.poll(() => scrollTop(page)).toBeGreaterThan(0);

    // A click on plain content in the card focuses it too (so arrow keys
    // scroll), but a pointer focus draws no ring. A section's description
    // paragraph, not a table: each table is its own focusable scroll region,
    // which a click on it rightly focuses instead.
    await page.goto(STATIC_PAGE, { waitUntil: 'networkidle' });
    await main.locator('p').first().click();
    await expect(main).toBeFocused();
    expect(await main.evaluate((el) => getComputedStyle(el).boxShadow)).toBe('none');
  });

  for (const width of [1440, 390]) {
    test(`exactly one banner at ${width}px, the mobile header a named nav`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      await loginAsAdmin(page);
      await page.goto('/b/admin/users', { waitUntil: 'networkidle' });
      await expect(page.getByRole('banner')).toHaveCount(1);
      await expect(page.getByRole('banner')).toHaveClass(/topbar/);
      if (width === 1440) {
        // A shared box-shadow ring on a button keeps its forced-colors outline.
        const palette = page.locator('button.topbar__palette');
        await palette.focus();
        await page.keyboard.press('Shift+Tab');
        await page.keyboard.press('Tab');
        await expect(palette).toBeFocused();
        expect(await palette.evaluate((el) => el.matches(':focus-visible'))).toBe(true);
        expect(await palette.evaluate((el) => getComputedStyle(el).outlineStyle)).toBe('solid');
      }
      if (width === 390) {
        const menu = page.getByRole('navigation', { name: 'Site' });
        await expect(menu).toBeVisible();
        await expect(menu.getByRole('button', { name: 'Open menu' })).toBeVisible();
        await expect(
          menu.getByRole('button', { name: 'Search pages (command palette)' }),
        ).toBeVisible();
      } else {
        await expect(page.getByRole('navigation', { name: 'Site' })).toBeHidden();
      }
    });
  }
});
