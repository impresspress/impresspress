import { expect, type Page } from '@playwright/test';

/**
 * The runtime half of the one-primary rule (PLAN rule 6): at most one primary
 * button (`.btn--primary`) a reader can see on the page, and at most one in
 * each open dialog, which is a context of its own.
 *
 * The Rust crawl guard (`htmx_guard::every_page_has_at_most_one_visible_primary`)
 * reads the server's markup, where `[hidden]`, `<template>` and a closed
 * `<dialog>` decide what is visible. This reads the page as the browser lays
 * it out after its scripts ran, so it also sees what a script revealed or
 * hid — the wizard's Continue / publish swap, the groups editor, the legal
 * editor's not-started state — and anything CSS hides. Visible means
 * `checkVisibility()`: rendered, and not under `display: none`,
 * `content-visibility: hidden` or a closed `<details>`.
 */
export interface Primaries {
  /** The labels of the visible primaries outside every dialog. */
  page: string[];
  /** The labels of the visible primaries in each open dialog, in order. */
  dialogs: string[][];
}

export function visiblePrimaries(page: Page): Promise<Primaries> {
  return page.evaluate(() => {
    const label = (b: Element) => b.textContent!.trim();
    const shown = [...document.querySelectorAll<HTMLElement>('.btn--primary')].filter((b) => b.checkVisibility());
    return {
      page: shown.filter((b) => !b.closest('dialog')).map(label),
      dialogs: [...document.querySelectorAll<HTMLDialogElement>('dialog[open]')].map((d) =>
        shown.filter((b) => b.closest('dialog') === d).map(label),
      ),
    };
  });
}

/** The page shows at most one primary, and each open dialog at most one. */
export async function expectAtMostOnePrimary(page: Page): Promise<void> {
  const found = await visiblePrimaries(page);
  expect(found.page.length, `more than one primary on ${page.url()}: ${JSON.stringify(found.page)}`).toBeLessThanOrEqual(1);
  for (const dialog of found.dialogs) {
    expect(dialog.length, `more than one primary in a dialog on ${page.url()}: ${JSON.stringify(dialog)}`).toBeLessThanOrEqual(1);
  }
}
