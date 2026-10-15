import { readFileSync } from 'node:fs';
import { test, expect, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * Buttons (`components::Action`) and a card head's action (`Card::action`),
 * laid out by the served stylesheet from a phone to a wide desktop.
 *
 * - Every button, of all five emphases, is an 8px-rounded rectangle, not a
 *   pill. Full-size buttons share one height, the control height.
 * - A card head's action sits at the end of the head, after its link. When
 *   the head runs out of room the link and action wrap under the title, and
 *   then the action under the link: neither label is squeezed onto two
 *   lines, and nothing overflows the card.
 *
 * The markup is `fixtures/card-head-action.html`, which a Rust test
 * (`the_card_head_e2e_fixture_is_what_the_builders_render`) keeps equal to
 * what the builders render. It is mounted into a real admin page so it gets
 * the page's real stylesheet and shell. Writes nothing (see `e2e:visual`).
 */

const FIXTURE = readFileSync(new URL('./fixtures/card-head-action.html', import.meta.url), 'utf8');
const HOST_PAGE = '/b/legalpages/admin/endpoints';
const DESKTOP_WIDTHS = [1280, 1024, 900, 721] as const;
const PHONE = { width: 375, height: 812 } as const;

async function mountFixture(page: Page) {
  await loginAsAdmin(page);
  await page.goto(HOST_PAGE, { waitUntil: 'networkidle' });
  await page.locator('main#content').evaluate((main, html) => {
    main.innerHTML = html;
    // The fixture's button row is a bare div: lay it out as a wrapping row
    // whose buttons keep their own height.
    Object.assign((main.querySelector('#buttons') as HTMLElement).style, {
      display: 'flex',
      flexWrap: 'wrap',
      alignItems: 'flex-start',
      gap: '8px',
      marginBottom: '16px',
    });
  }, FIXTURE);
}

/** Edges in CSS pixels, plus the computed values the rules set. */
async function layout(page: Page) {
  return page.locator('main#content').evaluate((main) => {
    const box = (el: Element) => el.getBoundingClientRect();
    // How many lines the element's longest text node runs over.
    const lines = (el: Element) => {
      const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
      let most = 0;
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        if (!(node as Text).data.trim()) continue;
        const r = document.createRange();
        r.selectNodeContents(node);
        const tops = new Set(
          [...r.getClientRects()].filter((q) => q.width > 0).map((q) => Math.round(q.top)),
        );
        most = Math.max(most, tops.size);
      }
      return most;
    };
    const controlHeight = parseFloat(getComputedStyle(main.querySelector('.btn--secondary')!).minHeight);
    const buttons = [...main.querySelectorAll('.btn')].map((btn) => ({
      label: btn.textContent!.trim(),
      radius: getComputedStyle(btn).borderTopLeftRadius,
      small: btn.classList.contains('btn--sm'),
      height: box(btn).height,
    }));
    const cards = [...main.querySelectorAll('section.card')].map((card) => {
      const border = parseFloat(getComputedStyle(card).borderRightWidth);
      const title = card.querySelector('.card__title')!;
      const items = [...card.querySelectorAll('.card__actions > *')];
      return {
        title: title.textContent!,
        innerRight: box(card).right - border,
        titleTop: box(title).top,
        titleBottom: box(title).bottom,
        items: items.map((item) => ({
          label: item.textContent!.trim(),
          top: box(item).top,
          bottom: box(item).bottom,
          right: box(item).right,
          lines: lines(item),
        })),
      };
    });
    return {
      controlHeight,
      buttons,
      cards,
      pageOverflows: document.documentElement.scrollWidth > window.innerWidth,
    };
  });
}

type Layout = Awaited<ReturnType<typeof layout>>;

function expectButtonsAndHeads(l: Layout) {
  expect(l.buttons.length).toBeGreaterThanOrEqual(8);
  for (const b of l.buttons) {
    expect(b.radius, b.label).toBe('8px');
    if (!b.small) expect(Math.abs(b.height - l.controlHeight), `${b.label} height`).toBeLessThan(0.5);
  }
  expect(l.cards).toHaveLength(2);
  for (const card of l.cards) {
    for (const item of card.items) {
      expect(item.lines, `${card.title}: ${item.label}`).toBe(1);
      expect(item.right, `${card.title}: ${item.label} right`).toBeLessThanOrEqual(card.innerRight + 0.5);
    }
  }
  expect(l.pageOverflows).toBe(false);
}

test.describe('buttons and a card head action', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  for (const width of DESKTOP_WIDTHS) {
    test(`at ${width}px every button is an 8px rectangle and the head action fits`, async ({ page }) => {
      await page.setViewportSize({ width, height: 860 });
      await mountFixture(page);
      const l = await layout(page);
      expectButtonsAndHeads(l);
      if (width === 1280) {
        // Room for both: the head's link and action share the title's row.
        for (const card of l.cards) {
          for (const item of card.items) {
            expect(item.top, `${card.title}: ${item.label}`).toBeLessThan(card.titleBottom);
          }
        }
      }
    });
  }

  test('on a phone the head link and action drop under the title, one per line', async ({ page }) => {
    await page.setViewportSize(PHONE);
    await mountFixture(page);
    const l = await layout(page);
    expectButtonsAndHeads(l);
    const defaults = l.cards[1];
    const [link, action] = defaults.items;
    expect(link.top).toBeGreaterThanOrEqual(defaults.titleBottom - 0.5);
    expect(action.top).toBeGreaterThanOrEqual(link.bottom - 0.5);
  });
});
