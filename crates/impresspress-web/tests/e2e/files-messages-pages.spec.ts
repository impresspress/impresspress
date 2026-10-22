import { expect, request as playwrightRequest, test, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, adminBearer, loginAsAdmin } from './fixtures/auth';
import { WIDTHS, isWrong, layout } from './fixtures/layout';
import { PHONE, targetFloor } from './fixtures/targets';
import { seedBucket } from './fixtures/storage';

/**
 * The storage and messages pages, driven in a browser against the real server.
 *
 * - Files: "New bucket" opens the shared modal and lands in the new bucket;
 *   an upload comes back as a row whose checkbox is named after the file and
 *   whose "more actions" button opens a menu the keyboard drives; a bulk
 *   delete asks through the shared dialog, and its outcome — including a
 *   file that could not be deleted — is announced, not wiped by a reload.
 * - Share links: revoking confirms in the same dialog and updates the list in
 *   place.
 * - Messages with long titles and entries quoting the ids an error names,
 *   at every width from the phone to 1280: the list, a conversation and a
 *   task scroll nothing sideways, no card or entry overflows, the
 *   conversation keeps its room, and each page shows its one primary.
 * - Messages at 390px: a context is created from the list, and its
 *   conversation is one pane with a usable composer, no sideways scroll.
 *
 * It creates a bucket, uploads a file, creates a context and posts to it, so
 * it is part of `e2e:writes`, which CI runs against its own fresh server.
 */

test.use({ storageState: ADMIN_STATE_PATH });

/** No horizontal scroll: the document is no wider than the viewport. */
async function overflowX(page: Page): Promise<number> {
  return page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
}

test('a bucket created from the modal takes an upload with labelled row controls', async ({ page }) => {
  const bucket = `e2e-${Date.now().toString(36)}`;
  await loginAsAdmin(page);
  await page.goto('/b/storage/', { waitUntil: 'networkidle' });

  await page.locator('.topbar__actions').getByRole('button', { name: 'New bucket' }).click();
  const dialog = page.getByRole('dialog', { name: 'New bucket' });
  await expect(dialog).toBeVisible();
  await dialog.getByLabel('Name').fill(bucket);
  await dialog.getByRole('button', { name: 'Create bucket' }).click();
  await expect(page).toHaveURL(new RegExp(`/b/storage/${bucket}/$`));
  await expect(page.getByRole('heading', { level: 1, name: bucket })).toBeVisible();

  // The empty bucket offers the upload it is waiting for: the header's
  // Upload is the page's one primary, so the empty state's is a secondary.
  await expect(page.getByRole('heading', { level: 2, name: 'This folder is empty' })).toBeVisible();
  await expect(page.locator('.empty__action').getByRole('button', { name: 'Upload' })).toHaveClass(/\bbtn--secondary\b/);
  await expect(page.locator('.btn--primary:visible')).toHaveCount(1);

  // Upload through the picker every "Upload" opens. The listing is
  // swapped in place, not the page reloaded, so the outcome stays on screen.
  await page.locator('#file-upload-input').setInputFiles({
    name: 'notes.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('hello'),
  });
  await expect(page.locator('#toast-container')).toContainText('1 file uploaded');
  await expect(page.locator('[data-bulk-count]')).toHaveText('1 file uploaded');

  await expect(page.getByRole('checkbox', { name: 'Select notes.txt' })).toBeVisible();
  await expect(page.getByRole('checkbox', { name: 'Select all files' })).toBeVisible();

  // The row's menu, from the keyboard.
  const trigger = page.getByRole('button', { name: 'Actions for notes.txt' });
  const box = await trigger.boundingBox();
  const floor = await targetFloor(page);
  expect(box!.width).toBeGreaterThan(floor - 0.5);
  expect(box!.height).toBeGreaterThan(floor - 0.5);
  await expect(trigger.locator('svg')).toHaveCount(1);
  await expect(trigger).toHaveAttribute('aria-expanded', 'false');

  await trigger.focus();
  await page.keyboard.press('Enter');
  const menu = page.getByRole('menu', { name: 'Actions for notes.txt' });
  await expect(menu).toBeVisible();
  await expect(trigger).toHaveAttribute('aria-expanded', 'true');
  await expect(menu.getByRole('menuitem', { name: 'Share' })).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(menu.getByRole('menuitem', { name: 'Copy link' })).toBeFocused();
  await page.keyboard.press('End');
  await expect(menu.getByRole('menuitem', { name: 'Delete' })).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(menu.getByRole('menuitem', { name: 'Share' })).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(menu).toHaveCount(0);
  await expect(trigger).toBeFocused();
  await expect(trigger).toHaveAttribute('aria-expanded', 'false');

  // ArrowUp opens it on the last item; Share opens the share modal.
  await page.keyboard.press('ArrowUp');
  await expect(menu.getByRole('menuitem', { name: 'Delete' })).toBeFocused();
  await page.keyboard.press('Home');
  await page.keyboard.press('Enter');
  await expect(page.getByRole('dialog', { name: 'Create share link' })).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(trigger).toBeFocused();

  // Tab out of the open menu closes it and moves on past its trigger.
  await page.keyboard.press('Enter');
  await expect(menu).toBeVisible();
  await page.keyboard.press('Tab');
  await expect(menu).toHaveCount(0);
  await expect(trigger).toHaveAttribute('aria-expanded', 'false');
  await expect(trigger).not.toBeFocused();

  // Selecting the row shows the bulk bar with its count.
  await expect(page.getByRole('button', { name: 'Delete selected' })).toBeHidden();
  await page.getByRole('checkbox', { name: 'Select notes.txt' }).check();
  await expect(page.locator('[data-bulk-count]')).toHaveText('1 file selected');
  const del = page.getByRole('button', { name: 'Delete selected' });
  await expect(del).toBeVisible();
  expect((await del.boundingBox())!.height).toBeGreaterThan(floor - 0.5);
});

test('a bulk delete confirms in the dialog, removes the rows and says so', async ({ page, baseURL }) => {
  const { bucket } = await seedBucket(baseURL, ['one.txt', 'two.txt', 'three.txt']);
  await loginAsAdmin(page);
  await page.goto(`/b/storage/${bucket}/`, { waitUntil: 'networkidle' });

  await page.getByRole('checkbox', { name: 'Select one.txt' }).check();
  await page.getByRole('checkbox', { name: 'Select two.txt' }).check();
  const del = page.getByRole('button', { name: 'Delete selected' });
  await del.click();

  const dialog = page.getByRole('alertdialog', { name: 'Delete files' });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("Delete 2 files? This can't be undone.");
  // Cancel, not the destructive button, has focus: a stray Enter cancels.
  await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();
  await page.keyboard.press('Enter');
  await expect(dialog).toBeHidden();
  await expect(page.getByRole('checkbox', { name: 'Select one.txt' })).toBeChecked();

  await del.click();
  await dialog.getByRole('button', { name: 'Delete files', exact: true }).click();
  await expect(page.locator('[data-bulk-count]')).toHaveText('2 files deleted');
  await expect(page.locator('#toast-container')).toContainText('2 files deleted');
  await expect(page.getByRole('checkbox', { name: 'Select one.txt' })).toHaveCount(0);
  await expect(page.getByRole('checkbox', { name: 'Select two.txt' })).toHaveCount(0);
  await expect(page.getByRole('checkbox', { name: 'Select three.txt' })).not.toBeChecked();
});

test('a file that could not be deleted is reported, not passed over', async ({ page, baseURL }) => {
  const { bucket, remove } = await seedBucket(baseURL, ['done.txt', 'gone.txt', 'stay.txt']);
  await loginAsAdmin(page);
  await page.goto(`/b/storage/${bucket}/`, { waitUntil: 'networkidle' });

  await page.getByRole('checkbox', { name: 'Select done.txt' }).check();
  await page.getByRole('checkbox', { name: 'Select gone.txt' }).check();
  await page.getByRole('button', { name: 'Delete selected' }).click();
  // Someone else deletes one of them while the dialog is open.
  await remove('gone.txt');
  await page.getByRole('alertdialog', { name: 'Delete files' }).getByRole('button', { name: 'Delete files', exact: true }).click();

  const outcome = "1 file deleted, 1 couldn't be deleted: gone.txt";
  await expect(page.locator('[data-bulk-count]')).toHaveText(outcome);
  await expect(page.locator('#toast-container [role="alert"]')).toContainText(outcome);
  // The list is what the bucket now holds: both rows are gone, the
  // untouched file is still there.
  await expect(page.getByRole('checkbox', { name: 'Select done.txt' })).toHaveCount(0);
  await expect(page.getByRole('checkbox', { name: 'Select gone.txt' })).toHaveCount(0);
  await expect(page.getByRole('checkbox', { name: 'Select stay.txt' })).toBeVisible();
});

test('revoking a share link confirms in the dialog and updates the list in place', async ({ page, baseURL }) => {
  const { bucket, share } = await seedBucket(baseURL, ['report.txt']);
  await share('report.txt');
  await loginAsAdmin(page);
  await page.goto('/b/cloudstorage/', { waitUntil: 'networkidle' });
  // Set on this document only: a reload would drop it.
  await page.evaluate(() => ((window as unknown as { __noReload: boolean }).__noReload = true));

  const revoke = page.getByRole('button', { name: `Revoke the share link for ${bucket}/report.txt` });
  await revoke.click();
  const dialog = page.getByRole('alertdialog', { name: 'Revoke link' });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText(`Revoke the link to ${bucket}/report.txt? Anyone who has it loses access.`);
  await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
  await expect(revoke).toBeFocused();

  await revoke.click();
  await dialog.getByRole('button', { name: 'Revoke link', exact: true }).click();
  await expect(page.locator('#toast-container')).toContainText('Share link revoked');
  await expect(revoke).toHaveCount(0);
  await expect(page.getByRole('heading', { level: 2, name: 'Share links', exact: true })).toBeFocused();
  expect(await page.evaluate(() => (window as unknown as { __noReload?: boolean }).__noReload)).toBe(true);
});

/**
 * The share-link and file tables narrow by their own width (table.css
 * `.share-table`, `.object-table`). Checked here, on the writes server,
 * because it needs a share link with an expiry and a long file name: seeded
 * on the baselines' server, they would show up in captures running beside
 * it (the storage overview's counts). At every width from the phone to 1280
 * no page or table scrolls sideways, and each row's control stays in view.
 */
test('the share and file tables fit their card at every width', async ({ page, baseURL }) => {
  const long = 'quarterly-report-2026-final-version-for-the-board.pdf';
  const seeded = await seedBucket(baseURL, [long]);
  const id = await seeded.share(long);
  try {
    await loginAsAdmin(page);
    const file = `${seeded.bucket}/${long}`;
    const found: Record<string, unknown> = {};
    for (const width of [375, 721, 800, 900, 960, 1024, 1100, 1200, 1280]) {
      await page.setViewportSize({ width, height: 900 });
      for (const [path, control] of [
        ['/b/cloudstorage/', `Revoke the share link for ${file}`],
        ['/b/storage/admin/shares', null],
        [`/b/storage/${seeded.bucket}/`, `Actions for ${long}`],
      ] as const) {
        await page.goto(path, { waitUntil: 'networkidle' });
        const r = await page.evaluate(() => ({
          page: document.documentElement.scrollWidth - document.documentElement.clientWidth,
          tables: [...document.querySelectorAll<HTMLElement>('.data-table')]
            .filter((t) => t.scrollWidth > t.clientWidth + 1)
            .map((t) => `${t.scrollWidth}/${t.clientWidth}`),
        }));
        const box = control ? await page.getByRole('button', { name: control }).boundingBox() : null;
        const offscreen = box ? box.x + box.width > width + 0.5 : false;
        if (r.page > 0 || r.tables.length || offscreen) found[`${path} at ${width}`] = { ...r, offscreen };
      }
    }
    expect(found, 'a page or table scrolled sideways, or a row control left the screen').toEqual({});
  } finally {
    await seeded.revoke(id);
    await seeded.remove(long);
    await seeded.drop();
  }
});

test.describe('on a phone', () => {
  test.use(PHONE);

  test('a context created from the list takes a message in a one-pane conversation', async ({ page }) => {
    const title = `e2e chat ${Date.now().toString(36)}`;
    await loginAsAdmin(page);
    await page.goto('/b/messages/', { waitUntil: 'networkidle' });
    await expect(page.getByRole('heading', { level: 1, name: 'Messages' })).toBeVisible();
    expect(await overflowX(page)).toBe(0);

    await page.getByLabel('Type').selectOption('conversation');
    await page.getByLabel('Title').fill(title);
    await page.getByRole('button', { name: 'Create' }).click();
    const row = page.locator('#context-list a', { hasText: title });
    await expect(row).toBeVisible();

    await row.click();
    await expect(page.getByRole('heading', { level: 1, name: title })).toBeVisible();
    // One pane: the conversation, not the thread list beside it.
    await expect(page.getByRole('complementary', { name: 'Conversations' })).toBeHidden();
    await expect(page.locator('.topbar__crumbs').getByRole('link', { name: 'Messages' })).toBeVisible();

    const input = page.getByRole('textbox', { name: 'Message' });
    const send = page.getByRole('button', { name: 'Send' });
    const inputBox = (await input.boundingBox())!;
    const sendBox = (await send.boundingBox())!;
    // The box keeps most of the row; the button sits beside it, on screen.
    expect(inputBox.width).toBeGreaterThan(200);
    expect(sendBox.height).toBeGreaterThan(43.5);
    expect(sendBox.x + sendBox.width).toBeLessThanOrEqual(390);
    expect(await overflowX(page)).toBe(0);

    await input.fill('Ship it on Friday?');
    await send.click();
    await expect(page.locator('#entries-list')).toContainText('Ship it on Friday?');
    await expect(input).toHaveValue('');
  });
});

test('messages with long titles and entries fit their frame at every width', async ({ page, baseURL }) => {
  test.setTimeout(180_000);
  const api = await playwrightRequest.newContext({ baseURL, storageState: { cookies: [], origins: [] } });
  const headers = { Authorization: await adminBearer(api), 'Content-Type': 'application/json' };
  const made: string[] = [];
  try {
    const context = async (type: string, title: string, entries: [string, string, string][]) => {
      const res = await api.post('/b/messages/api/contexts', { headers, data: { type, title } });
      expect(res.ok(), await res.text()).toBe(true);
      const { id } = (await res.json()) as { id: string };
      made.push(id);
      for (const [kind, role, content] of entries) {
        const entry = await api.post(`/b/messages/api/contexts/${id}/entries`, { headers, data: { kind, role, content } });
        expect(entry.ok(), await entry.text()).toBe(true);
      }
      return id;
    };
    const chat = await context('conversation', 'Renewal questions for the Northwind Traders enterprise agreement, order ord_1NvQh2KZp7a9XyTqAbCdEfGh', [
      ['message', 'user', "Why did invoice in_1NvQh2KZp7a9XyTqAbCdEfGhIjKlMn fail? Stripe said: No such customer: 'cus_1NvQh2KZp7a9XyTqAbCdEfGh'"],
      ['message', 'assistant', 'See https://dashboard.stripe.com/test/customers/cus_1NvQh2KZp7a9XyTqAbCdEfGh/payment_methods/pm_1NvQh2KZp7a9XyTqAbCdEfGh'],
    ]);
    const task = await context('task', 'Rotate the restricted API key key_01HZX9Q2KZp7a9XyTqAbCdEfGhIjKlMnOpQrStUvWxYz for the Northwind Traders account', [
      ['message', 'user', "The webhook endpoint keeps failing with: No such webhook endpoint: 'we_1NvQh2KZp7a9XyTqAbCdEfGh'"],
      ['artifact', 'assistant', 'https://northwind-traders-production.example.com/exports/2026-10-22/stripe-restricted-keys-rotation-report-final-v2.csv'],
      ['notification', 'system', 'Key rotated by ana.lucia.fernandez-gonzalez@northwind-traders-production.example.com'],
    ]);
    const pages: Record<string, string[]> = {
      '/b/messages/': ['Create'],
      [`/b/messages/contexts/${chat}`]: ['Send'],
      [`/b/messages/contexts/${task}`]: ['Add'],
    };
    await loginAsAdmin(page);
    const found: Record<string, unknown> = {};
    for (const width of WIDTHS) {
      await page.setViewportSize({ width, height: 900 });
      for (const [path, primaries] of Object.entries(pages)) {
        await page.goto(path, { waitUntil: 'networkidle' });
        const r = await layout(page);
        if (isWrong(r, primaries)) found[`${path} at ${width}`] = r;
      }
    }
    expect(found, 'a page scrolled sideways, a card overflowed, the chat was squeezed or the primaries differ').toEqual({});
  } finally {
    for (const id of made) await api.delete(`/b/messages/api/contexts/${id}`, { headers });
    await api.dispose();
  }
});
