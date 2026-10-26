import type { Page } from '@playwright/test';
import { visiblePrimaries } from './primaries';

/** The widths a page is checked at: the phone, then every step between the
 * sidebar's 721px breakpoint and the 1280px baselines. */
export const WIDTHS = [375, 721, 800, 900, 960, 1024, 1100, 1200, 1280] as const;

/** How a page of the content column lays out: what scrolls sideways or runs
 * past the column, which framed surface's content is wider than it, which
 * headline figure broke mid-word, the primaries a reader sees outside any
 * dialog, and how wide the chat's conversation is when one is shown. This is
 * the one measure the width specs share; a spec that needs another property
 * adds it here rather than measuring its own copy. */
export interface Layout {
  /** How far the document scrolls sideways. */
  page: number;
  /** Record tables that scroll inside their frame (`scrollWidth/clientWidth`).
   * An endpoint reference is a labelled scroll region by design
   * (`components::endpoint_table`), so it is left out. */
  tables: string[];
  /** Elements drawn past the content column's right edge. What scrolls
   * inside a row of its own is left out: a code block in a message, a
   * scrolling grid, the tab and filter rows, and the settings rail below
   * 1024px (`.form-tabs`, a scrolling tab row there). */
  past: string[];
  /** Cards, the chat frame, message and thread cards whose content is wider
   * than they are: a long unbroken word overflows its line without moving
   * any box, so only this catches it. */
  cards: string[];
  /** Words of a stat's value (`components::Stat`) split across two lines.
   * A stat is a headline figure or marker sized to fit its floor
   * (stat.css), so no word in one ever breaks. */
  stats: string[];
  /** Buttons whose label runs onto a second line. A button is one line at
   * every width: where its row is too narrow the row wraps, or the label is
   * shorter. */
  buttons: string[];
  /** The visible primaries' labels, outside any dialog (`fixtures/primaries.ts`). */
  primaries: string[];
  /** The chat's conversation pane width beside another pane, or null when
   * none is shown or it is the chat's only pane (a phone's one-pane chat is
   * as wide as the screen allows). */
  conversation: number | null;
}

export async function layout(page: Page): Promise<Layout> {
  const measured = await page.evaluate(() => {
    const visible = (e: Element) => (e as HTMLElement).checkVisibility();
    const name = (e: Element) => `${e.tagName.toLowerCase()}.${String(e.className).split(' ').join('.')}`;
    const edge = document.querySelector('main#content')!.getBoundingClientRect().right;
    const main = document.querySelector('.chat-main');
    return {
      page: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      tables: [...document.querySelectorAll<HTMLElement>('.data-table:not(.data-table--scroll)')]
        .filter((t) => visible(t) && t.scrollWidth > t.clientWidth + 1)
        .map((t) => `${t.scrollWidth}/${t.clientWidth}`),
      past: [...document.querySelectorAll<HTMLElement>('main#content *')]
        .filter((e) => visible(e) && !e.closest('.subnav, .tabs, .filter-links, .form-tabs, .data-table--scroll, .message-card__content pre, dialog'))
        .filter((e) => e.getBoundingClientRect().width > 0 && e.getBoundingClientRect().right > edge + 1)
        .map(name)
        .slice(0, 4),
      cards: [...document.querySelectorAll<HTMLElement>('main#content :is(.card, .page--chat, .message-card, .thread-card)')]
        .filter((c) => visible(c) && c.scrollWidth > c.clientWidth + 1)
        .map((c) => `${c.querySelector('.card__title')?.textContent ?? name(c)} ${c.scrollWidth}/${c.clientWidth}`),
      stats: [...document.querySelectorAll<HTMLElement>('main#content .stat__value')].filter(visible).flatMap((value) => {
        const walker = document.createTreeWalker(value, NodeFilter.SHOW_TEXT);
        const broken: string[] = [];
        for (let text = walker.nextNode() as Text | null; text; text = walker.nextNode() as Text | null) {
          for (const m of text.data.matchAll(/\S+/g)) {
            const range = document.createRange();
            range.setStart(text, m.index!);
            range.setEnd(text, m.index! + m[0].length);
            if (new Set([...range.getClientRects()].map((r) => Math.round(r.top))).size > 1) broken.push(m[0]);
          }
        }
        return broken;
      }),
      buttons: [...document.querySelectorAll<HTMLElement>('main#content .btn')].filter(visible).flatMap((button) => {
        // The lines its visible text sits on, by each text box's top; an
        // icon and screen-reader-only text are not drawn on a line.
        const walker = document.createTreeWalker(button, NodeFilter.SHOW_TEXT);
        const tops = new Set<number>();
        for (let text = walker.nextNode(); text; text = walker.nextNode()) {
          if (!text.textContent!.trim() || text.parentElement!.closest('.sr-only')) continue;
          const range = document.createRange();
          range.selectNodeContents(text);
          for (const r of range.getClientRects()) if (r.width > 0) tops.add(Math.round(r.top));
        }
        return tops.size > 1 ? [button.textContent!.trim()] : [];
      }),
      conversation:
        main && visible(main) && main.getBoundingClientRect().width < main.parentElement!.clientWidth - 1
          ? Math.round(main.getBoundingClientRect().width)
          : null,
    };
  });
  return { ...measured, primaries: (await visiblePrimaries(page)).page };
}

/** Whether `r` shows anything out of its frame: the page, a table, a card or
 * an element past the column scrolls or overflows sideways, a stat's value
 * breaks a word, a button's label takes two lines, or a conversation is
 * squeezed under 360px beside another pane. */
export function overflows(r: Layout): boolean {
  return (
    r.page > 0 ||
    r.tables.length > 0 ||
    r.past.length > 0 ||
    r.cards.length > 0 ||
    r.stats.length > 0 ||
    r.buttons.length > 0 ||
    (r.conversation !== null && r.conversation < 360)
  );
}

/** Whether `r` shows anything wrong: it `overflows`, or its primaries are
 * other than `primaries`. */
export function isWrong(r: Layout, primaries: string[]): boolean {
  return overflows(r) || JSON.stringify(r.primaries) !== JSON.stringify(primaries);
}
