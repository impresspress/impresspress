import type { Page } from '@playwright/test';

/** The widths a page is checked at: the phone, then every step between the
 * sidebar's 721px breakpoint and the 1280px baselines. */
export const WIDTHS = [375, 721, 800, 900, 960, 1024, 1100, 1200, 1280] as const;

/** How a page of the content column lays out: what scrolls sideways or runs
 * past the column, which framed surface's content is wider than it, the
 * primaries a reader sees outside any dialog, and how wide the chat's
 * conversation is when one is shown. */
export interface Layout {
  /** How far the document scrolls sideways. */
  page: number;
  /** Record tables that scroll inside their frame (`scrollWidth/clientWidth`).
   * An endpoint reference is a labelled scroll region by design
   * (`components::endpoint_table`), so it is left out. */
  tables: string[];
  /** Elements drawn past the content column's right edge. */
  past: string[];
  /** Cards, the chat frame, message and thread cards whose content is wider
   * than they are: a long unbroken word overflows its line without moving
   * any box, so only this catches it. */
  cards: string[];
  /** The visible primaries' labels, outside any dialog. */
  primaries: string[];
  /** The chat's conversation pane width beside another pane, or null when
   * none is shown or it is the chat's only pane (a phone's one-pane chat is
   * as wide as the screen allows). */
  conversation: number | null;
}

export function layout(page: Page): Promise<Layout> {
  return page.evaluate(() => {
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
        .filter((e) => visible(e) && !e.closest('.subnav, .tabs, .filter-links, .data-table--scroll, dialog'))
        .filter((e) => e.getBoundingClientRect().width > 0 && e.getBoundingClientRect().right > edge + 1)
        .map(name)
        .slice(0, 4),
      cards: [...document.querySelectorAll<HTMLElement>('main#content :is(.card, .page--chat, .message-card, .thread-card)')]
        .filter((c) => visible(c) && c.scrollWidth > c.clientWidth + 1)
        .map((c) => `${c.querySelector('.card__title')?.textContent ?? name(c)} ${c.scrollWidth}/${c.clientWidth}`),
      primaries: [...document.querySelectorAll<HTMLElement>('.btn--primary')]
        .filter((b) => visible(b) && !b.closest('dialog'))
        .map((b) => b.textContent!.trim()),
      conversation:
        main && visible(main) && main.getBoundingClientRect().width < main.parentElement!.clientWidth - 1
          ? Math.round(main.getBoundingClientRect().width)
          : null,
    };
  });
}

/** Whether `r` shows anything wrong: overflow of any kind, a conversation
 * squeezed under 360px beside another pane, or primaries other than
 * `primaries`. */
export function isWrong(r: Layout, primaries: string[]): boolean {
  return (
    r.page > 0 ||
    r.tables.length > 0 ||
    r.past.length > 0 ||
    r.cards.length > 0 ||
    (r.conversation !== null && r.conversation < 360) ||
    JSON.stringify(r.primaries) !== JSON.stringify(primaries)
  );
}
