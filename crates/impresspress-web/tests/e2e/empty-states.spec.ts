import { readFileSync } from 'node:fs';
import { test, expect, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * The two empty states (`components::empty_state`, `components::empty_note`),
 * laid out by the served stylesheet from a phone to a wide desktop.
 *
 * - The framed state is a small card-level surface: a 1px border on the
 *   surface fill, a 24px icon, a title at a card title's size and weight
 *   (`--text-base`/600), a 14px body held to 36rem, and the action under the
 *   body. No word in it breaks mid-word.
 * - The note is one 14px sentence with no frame. A flush card's body has no
 *   padding, so a note there takes the card's inset back: its text starts
 *   where the card's title does and sits the inset off the card's border
 *   (20px, 16px on a phone), never on it.
 *
 * The markup is `fixtures/empty-states.html`, which a Rust test
 * (`the_empty_states_e2e_fixture_is_what_the_builders_render`) keeps equal
 * to what the builders render. It is mounted into a real admin page so it
 * gets the page's real stylesheet and shell. Writes nothing (see
 * `e2e:visual`).
 */

const FIXTURE = readFileSync(new URL('./fixtures/empty-states.html', import.meta.url), 'utf8');
const HOST_PAGE = '/b/legalpages/admin/endpoints';
const DESKTOP_WIDTHS = [1280, 1024, 900, 721] as const;
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
  return page.locator('main#content').evaluate((main) => {
    const box = (el: Element) => el.getBoundingClientRect();
    const px = (v: string) => parseFloat(v);
    // Each word of the element's text that the browser split across lines.
    const brokenWords = (el: Element) => {
      const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
      const broken: string[] = [];
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        const text = node as Text;
        for (const m of text.data.matchAll(/\S+/g)) {
          const r = document.createRange();
          r.setStart(text, m.index!);
          r.setEnd(text, m.index! + m[0].length);
          if (r.getClientRects().length > 1) broken.push(m[0]);
        }
      }
      return broken;
    };
    const mainBox = box(main);
    const framed = [...main.querySelectorAll('.empty')].map((empty) => {
      const s = getComputedStyle(empty);
      const icon = empty.querySelector('.empty__icon svg')!;
      const title = empty.querySelector('.empty__title')!;
      const body = empty.querySelector('.empty__body')!;
      const action = empty.querySelector('.empty__action');
      return {
        border: s.borderTopWidth,
        radius: px(s.borderTopLeftRadius),
        fill: s.backgroundColor,
        left: box(empty).left,
        right: box(empty).right,
        icon: { width: box(icon).width, height: box(icon).height },
        titleSize: getComputedStyle(title).fontSize,
        titleWeight: getComputedStyle(title).fontWeight,
        bodySize: getComputedStyle(body).fontSize,
        bodyWidth: box(body).width,
        bodyBottom: box(body).bottom,
        actionTop: action ? box(action).top : null,
        broken: [...brokenWords(title), ...brokenWords(body)],
      };
    });
    const notes = [...main.querySelectorAll('.card--flush')].map((card) => {
      const note = card.querySelector('.empty-note')!;
      const title = card.querySelector('.card__title');
      const s = getComputedStyle(note);
      const c = box(card);
      const border = px(getComputedStyle(card).borderLeftWidth);
      const n = box(note);
      return {
        size: s.fontSize,
        border: s.borderTopWidth,
        // Where the sentence's text sits, against the card's inner edges.
        textLeft: n.left + px(s.paddingLeft) - (c.left + border),
        textTop: n.top + px(s.paddingTop) - (c.top + border),
        textBottomGap: c.bottom - border - (n.bottom - px(s.paddingBottom)),
        titleLeft: title ? box(title).left - (c.left + border) : null,
        broken: brokenWords(note),
      };
    });
    return {
      framed,
      notes,
      mainRight: mainBox.right,
      pageOverflows: document.documentElement.scrollWidth > window.innerWidth,
    };
  });
}

type Layout = Awaited<ReturnType<typeof layout>>;

function expectFramed(l: Layout, at: string) {
  expect(l.framed, at).toHaveLength(2);
  for (const [i, e] of l.framed.entries()) {
    const what = `${at} empty ${i}`;
    // A card-level surface: the card's frame on the surface fill.
    expect(e.border, what).toBe('1px');
    expect(e.radius, what).toBeGreaterThanOrEqual(8);
    expect(e.fill, what).not.toBe('rgba(0, 0, 0, 0)');
    // Small: a 24px mark, the card title's 15px, a 14px body to a readable
    // measure.
    expect(e.icon, what).toEqual({ width: 24, height: 24 });
    expect(e.titleSize, what).toBe('15px');
    expect(e.titleWeight, what).toBe('600');
    expect(e.bodySize, what).toBe('14px');
    expect(e.bodyWidth, what).toBeLessThanOrEqual(36 * 16 + 0.5);
    expect(e.right, what).toBeLessThanOrEqual(l.mainRight + 0.5);
    expect(e.broken, `${what} words broken mid-word`).toEqual([]);
  }
  // The action sits under the body.
  expect(l.framed[0].actionTop).not.toBeNull();
  expect(l.framed[0].actionTop!).toBeGreaterThanOrEqual(l.framed[0].bodyBottom);
  expect(l.framed[1].actionTop).toBeNull();
  expect(l.pageOverflows, `${at} page scrolls sideways`).toBe(false);
}

function expectNotes(l: Layout, inset: number, at: string) {
  expect(l.notes, at).toHaveLength(2);
  const [titled, plain] = l.notes;
  for (const [i, n] of l.notes.entries()) {
    const what = `${at} note ${i}`;
    expect(n.size, what).toBe('14px');
    expect(n.border, what).toBe('0px');
    // Off the card's border by the card's inset, not on it.
    expect(Math.abs(n.textLeft - inset), `${what} left inset`).toBeLessThan(0.5);
    expect(Math.abs(n.textBottomGap - inset), `${what} bottom inset`).toBeLessThan(0.5);
    expect(n.broken, `${what} words broken mid-word`).toEqual([]);
  }
  // Under a head it lines up with the title; with none it is inset on top.
  expect(Math.abs(titled.textLeft - titled.titleLeft!), `${at} titled note under its title`).toBeLessThan(0.5);
  expect(Math.abs(plain.textTop - inset), `${at} plain note top inset`).toBeLessThan(0.5);
}

test.describe('empty states', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  for (const width of DESKTOP_WIDTHS) {
    test(`at ${width}px the framed state is small and a flush card's note is inset`, async ({
      page,
    }) => {
      await page.setViewportSize({ width, height: 860 });
      await mountFixture(page);
      const l = await layout(page);
      expectFramed(l, `${width}px`);
      expectNotes(l, 20, `${width}px`);
    });
  }

  test('on a phone the framed state fits and a flush card note takes the phone inset', async ({
    page,
  }) => {
    await page.setViewportSize(PHONE);
    await mountFixture(page);
    const l = await layout(page);
    expectFramed(l, 'phone');
    expectNotes(l, 16, 'phone');
  });
});
