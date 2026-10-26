import { test, expect, request as apiRequest, type Locator, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';
import { WIDTHS } from './fixtures/layout';
import { seedBucket } from './fixtures/storage';

/**
 * The one confirm dialog (`components::confirm_dialog`, asked in by
 * chrome.js), driven from the keyboard alone through both ways in:
 *
 * - an htmx control's `hx-confirm` (a session's Revoke, a portal button's
 *   Delete): Cancel has focus as it opens, Tab stays inside it, Esc and
 *   Cancel answer no and hand focus back to the control, and a confirmed
 *   request that swaps the row out leaves focus on the same control in the
 *   row now in its place — whether htmx dropped just the row or re-rendered
 *   the whole list;
 * - a block script's `window.impresspressConfirm` (a share link's Revoke).
 *
 * And the dialog open at every width from the phone to 1280, asking a
 * question with a long unbroken file name in it.
 *
 * It revokes sessions, deletes portal buttons and share links, so it is part
 * of `e2e:writes`, which CI runs against its own fresh server.
 */

/** What has focus, as `tag#id` or `tag "accessible text"`. */
function focused(page: Page): Promise<string> {
  return page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    if (!el) return 'nothing';
    const tag = el.tagName.toLowerCase();
    return el.id ? `${tag}#${el.id}` : `${tag} ${JSON.stringify(el.getAttribute('aria-label') || el.textContent?.trim())}`;
  });
}

/** The open confirm dialog, checked to be the shared one asking `label`. */
async function asking(page: Page, label: string, question: string, tone: 'danger' | 'primary'): Promise<Locator> {
  const dialog = page.getByRole('alertdialog', { name: label, exact: true });
  await expect(dialog).toBeVisible();
  await expect(dialog).toHaveAttribute('id', 'confirm-dialog');
  await expect(dialog.locator('#confirm-dialog-question')).toHaveText(question);
  // Cancel has focus: a stray Enter cancels.
  await expect(dialog.getByRole('button', { name: 'Cancel', exact: true })).toBeFocused();
  // One confirm button, the action's own, in its tone: the dialog's one
  // primary.
  const shown = dialog.locator('[data-action="confirm-accept"]:visible');
  await expect(shown).toHaveCount(1);
  await expect(shown).toHaveText(label);
  await expect(shown).toHaveClass(tone === 'danger' ? /\bbtn--danger\b/ : /\bbtn--primary\b/);
  return dialog;
}

/** Tab round the open dialog twice over: focus never leaves it. */
async function tabStaysInside(page: Page, dialog: Locator): Promise<void> {
  for (const shift of [false, true]) {
    for (let i = 0; i < 6; i++) {
      await page.keyboard.press(shift ? 'Shift+Tab' : 'Tab');
      expect(await dialog.evaluate((d) => d.contains(document.activeElement)), `Tab ${i}`).toBe(true);
    }
  }
}

test.describe('an hx-confirm, keyboard only', () => {
  test("a session's Revoke: Esc and Cancel keep it, confirming drops the row and focus moves to the next one", async ({
    page,
    request,
    baseURL,
  }) => {
    const email = `confirm-${Date.now()}-${Math.floor(Math.random() * 1e6)}@example.com`;
    const password = 'confirm-dialog-horse-1';
    const signup = await request.post('/b/auth/api/signup', {
      data: { email, password },
      headers: { 'Content-Type': 'application/json' },
    });
    expect(signup.status(), await signup.text()).toBe(201);
    const login = await page.request.post('/b/auth/api/login', {
      data: { email, password },
      headers: { 'Content-Type': 'application/json' },
    });
    expect(login.status(), await login.text()).toBe(200);
    // Two other devices, so the list has rows on both sides of the one revoked.
    for (let i = 0; i < 2; i++) {
      const device = await apiRequest.newContext({ baseURL });
      const other = await device.post('/b/auth/api/login', {
        data: { email, password },
        headers: { 'Content-Type': 'application/json' },
      });
      expect(other.status(), await other.text()).toBe(200);
      await device.dispose();
    }

    await page.goto('/b/userportal/sessions', { waitUntil: 'networkidle' });
    const rows = page.locator('.data-table tbody tr');
    // This browser's, the sign-up's own and the two devices'.
    const before = await rows.count();
    expect(before).toBeGreaterThanOrEqual(3);
    const ids = await rows.locator('button[id^="revoke-"]').evaluateAll((bs) => bs.map((b) => b.id));
    const current = await rows.evaluateAll((trs) => trs.findIndex((tr) => tr.textContent!.includes('Current session')));
    // The first row that is not this browser's, so a row follows it if it can.
    const at = ids.findIndex((_, i) => i !== current);
    const revoke = page.locator(`#${ids[at]}`);

    // Esc answers no.
    await revoke.focus();
    await page.keyboard.press('Enter');
    let dialog = await asking(page, 'Revoke session', 'Revoke this session? That device will be signed out.', 'danger');
    await tabStaysInside(page, dialog);
    await page.keyboard.press('Escape');
    await expect(dialog).toBeHidden();
    await expect(revoke).toBeFocused();
    await expect(rows).toHaveCount(before);

    // So does Enter on Cancel, which has focus.
    await page.keyboard.press('Enter');
    dialog = await asking(page, 'Revoke session', 'Revoke this session? That device will be signed out.', 'danger');
    await page.keyboard.press('Enter');
    await expect(dialog).toBeHidden();
    await expect(revoke).toBeFocused();
    await expect(rows).toHaveCount(before);

    // The confirm button answers yes: the row goes, and focus lands on the
    // Revoke of the row now in its place, or of the one before it.
    await page.keyboard.press('Enter');
    dialog = await asking(page, 'Revoke session', 'Revoke this session? That device will be signed out.', 'danger');
    await page.keyboard.press('Tab');
    await expect(dialog.getByRole('button', { name: 'Revoke session', exact: true })).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(dialog).toBeHidden();
    await expect(rows).toHaveCount(before - 1);
    await expect(revoke).toHaveCount(0);
    const left = ids.filter((_, i) => i !== at);
    const next = left[Math.min(at, left.length - 1)];
    await expect.poll(() => focused(page)).toBe(`button#${next}`);

    // Revoked for real.
    await page.reload({ waitUntil: 'networkidle' });
    await expect(rows).toHaveCount(before - 1);
  });

  test.describe('as the admin', () => {
    test.use({ storageState: ADMIN_STATE_PATH });

    test("a portal button's Delete: the list re-renders whole, and focus stays at the row's place in it", async ({
      page,
    }) => {
      await loginAsAdmin(page);
      const stamp = Date.now();
      const labels = ['first', 'second', 'third'].map((n) => `Confirm ${n} ${stamp}`);
      await page.goto('/b/userportal/admin/buttons', { waitUntil: 'networkidle' });
      for (const label of labels) {
        await page.locator('#label').fill(label);
        await page.locator('#path').fill('/b/userportal/');
        await page.getByRole('button', { name: 'Add button' }).click();
        await expect(page.locator('#buttons-table tr', { hasText: label })).toHaveCount(1);
      }
      try {
        const del = page.getByRole('button', { name: `Delete ${labels[1]}` });
        const at = await del.evaluate((b) => {
          const tr = b.closest('tr')!;
          return [...tr.parentElement!.children].indexOf(tr);
        });

        await del.focus();
        await page.keyboard.press('Enter');
        const dialog = await asking(page, 'Delete button', `Delete the ${labels[1]} button?`, 'danger');
        await page.keyboard.press('Tab');
        await page.keyboard.press('Enter');
        await expect(dialog).toBeHidden();
        await expect(page.locator('#buttons-table tr', { hasText: labels[1] })).toHaveCount(0);

        // `hx-target="#buttons-table" hx-swap="outerHTML"`: the whole table is
        // new, and focus is on the same control in the row now at that place.
        await expect
          .poll(() =>
            page.evaluate(() => {
              const el = document.activeElement as HTMLElement;
              const tr = el.closest('tr');
              return {
                label: el.getAttribute('aria-label'),
                at: tr ? [...tr.parentElement!.children].indexOf(tr) : -1,
                inTable: !!el.closest('#buttons-table'),
              };
            }),
          )
          .toEqual({ label: `Delete ${labels[2]}`, at, inTable: true });

        // The rest go too. When the last row is deleted the answer is the
        // list's empty state, which says the list is empty: focus goes
        // there, not to the page body.
        for (const label of [labels[0], labels[2]]) {
          await page.getByRole('button', { name: `Delete ${label}` }).focus();
          await page.keyboard.press('Enter');
          const asked = await asking(page, 'Delete button', `Delete the ${label} button?`, 'danger');
          await page.keyboard.press('Tab');
          await page.keyboard.press('Enter');
          await expect(asked).toBeHidden();
          await expect(page.locator('#buttons-table tr', { hasText: label })).toHaveCount(0);
        }
        if ((await page.locator('#buttons-table tbody tr').count()) === 0) {
          await expect(page.locator('#buttons-table .empty')).toBeFocused();
        }
      } finally {
        await page.goto('/b/userportal/admin/buttons', { waitUntil: 'networkidle' });
        for (const label of [labels[0], labels[2]]) {
          const left = page.getByRole('button', { name: `Delete ${label}` });
          if ((await left.count()) === 0) continue;
          await left.click();
          await page.getByRole('alertdialog', { name: 'Delete button' }).getByRole('button', { name: 'Delete button' }).click();
          await expect(page.locator('#buttons-table tr', { hasText: label })).toHaveCount(0);
        }
      }
    });
  });
});

test.describe('window.impresspressConfirm, keyboard only', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  test("a share link's Revoke asks through the same dialog from the block's script", async ({ page, baseURL }) => {
    const seeded = await seedBucket(baseURL, ['minutes.txt']);
    await seeded.share('minutes.txt');
    try {
      await loginAsAdmin(page);
      await page.goto('/b/cloudstorage/', { waitUntil: 'networkidle' });
      const revoke = page.getByRole('button', { name: `Revoke the share link for ${seeded.bucket}/minutes.txt` });
      const question = `Revoke the link to ${seeded.bucket}/minutes.txt? Anyone who has it loses access.`;

      await revoke.focus();
      await page.keyboard.press('Enter');
      let dialog = await asking(page, 'Revoke link', question, 'danger');
      await tabStaysInside(page, dialog);
      await page.keyboard.press('Escape');
      await expect(dialog).toBeHidden();
      await expect(revoke).toBeFocused();

      await page.keyboard.press('Enter');
      dialog = await asking(page, 'Revoke link', question, 'danger');
      await page.keyboard.press('Enter');
      await expect(dialog).toBeHidden();
      await expect(revoke).toBeFocused();
      await expect(revoke).toHaveCount(1);

      await page.keyboard.press('Enter');
      dialog = await asking(page, 'Revoke link', question, 'danger');
      await page.keyboard.press('Tab');
      await page.keyboard.press('Enter');
      await expect(dialog).toBeHidden();
      await expect(page.locator('#toast-container')).toContainText('Share link revoked');
      await expect(revoke).toHaveCount(0);
      // The block's own choice once its row is gone: the list's heading.
      await expect(page.getByRole('heading', { level: 2, name: 'Share links', exact: true })).toBeFocused();
    } finally {
      await seeded.remove('minutes.txt');
      await seeded.drop();
    }
  });

  test('the dialog fits the screen at every width, asking about a long unbroken name', async ({ page, baseURL }) => {
    // Underscores, which are no break opportunity: only the question's own
    // `overflow-wrap` keeps it inside the dialog.
    const long = 'Northwind_Traders_Quarterly_Board_Report_2026_Final_Version_Approved_By_The_Directors.pdf';
    const seeded = await seedBucket(baseURL, [long]);
    const id = await seeded.share(long);
    try {
      await loginAsAdmin(page);
      const found: Record<number, unknown> = {};
      for (const width of WIDTHS) {
        await page.setViewportSize({ width, height: 800 });
        await page.goto('/b/cloudstorage/', { waitUntil: 'networkidle' });
        const revoke = page.getByRole('button', { name: `Revoke the share link for ${seeded.bucket}/${long}` });
        await revoke.focus();
        await page.keyboard.press('Enter');
        const dialog = await asking(
          page,
          'Revoke link',
          `Revoke the link to ${seeded.bucket}/${long}? Anyone who has it loses access.`,
          'danger',
        );
        const r = await dialog.evaluate((d) => {
          const box = d.getBoundingClientRect();
          const inside = (e: Element) => {
            const b = e.getBoundingClientRect();
            return b.left >= box.left - 0.5 && b.right <= box.right + 0.5 && b.top >= box.top - 0.5 && b.bottom <= box.bottom + 0.5;
          };
          const buttons = [...d.querySelectorAll<HTMLElement>('.modal__footer > .btn')].filter((b) => b.checkVisibility());
          return {
            page: document.documentElement.scrollWidth - document.documentElement.clientWidth,
            onScreen: box.left >= 0 && box.right <= window.innerWidth && box.top >= 0 && box.bottom <= window.innerHeight,
            overflow: [...d.querySelectorAll<HTMLElement>('*')]
              .filter((e) => e.checkVisibility() && e.scrollWidth > e.clientWidth + 1 && getComputedStyle(e).overflowX !== 'visible')
              .map((e) => e.className),
            question: (() => {
              const q = d.querySelector<HTMLElement>('#confirm-dialog-question')!;
              return q.scrollWidth <= q.clientWidth + 1 && inside(q);
            })(),
            buttons: buttons.map((b) => b.textContent!.trim()),
            buttonsInside: buttons.every(inside),
          };
        });
        const ok =
          r.page === 0 &&
          r.onScreen &&
          r.overflow.length === 0 &&
          r.question &&
          r.buttonsInside &&
          JSON.stringify(r.buttons) === JSON.stringify(['Cancel', 'Revoke link']);
        if (!ok) found[width] = r;
        await page.keyboard.press('Escape');
        await expect(dialog).toBeHidden();
        await expect(revoke).toBeFocused();
      }
      expect(found, 'the open confirm dialog left the screen or overflowed').toEqual({});
    } finally {
      await page.setViewportSize({ width: 1280, height: 900 });
      await seeded.revoke(id);
      await seeded.remove(long);
      await seeded.drop();
    }
  });
});
