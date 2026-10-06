import { expect, test, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * The storage and messages pages, driven in a browser against the real server.
 *
 * - Files: "+ New bucket" opens the shared modal and lands in the new bucket;
 *   an upload comes back as a row whose checkbox is named after the file and
 *   whose "more actions" button opens a menu the keyboard drives.
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

  await page.locator('.topbar__actions').getByRole('button', { name: '+ New bucket' }).click();
  const dialog = page.getByRole('dialog', { name: 'New bucket' });
  await expect(dialog).toBeVisible();
  await dialog.getByLabel('Name').fill(bucket);
  await dialog.getByRole('button', { name: 'Create bucket' }).click();
  await expect(page).toHaveURL(new RegExp(`/b/storage/${bucket}/$`));
  await expect(page.getByRole('heading', { level: 1, name: bucket })).toBeVisible();

  // The empty bucket offers the upload it is waiting for.
  await expect(page.getByRole('heading', { level: 2, name: 'This folder is empty' })).toBeVisible();

  // Upload through the picker every "+ Upload" opens; the page reloads.
  const reloaded = page.waitForEvent('load');
  await page.locator('#file-upload-input').setInputFiles({
    name: 'notes.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('hello'),
  });
  await reloaded;

  await expect(page.getByRole('checkbox', { name: 'Select notes.txt' })).toBeVisible();
  await expect(page.getByRole('checkbox', { name: 'Select all files' })).toBeVisible();

  // The row's menu, from the keyboard.
  const trigger = page.getByRole('button', { name: 'Actions for notes.txt' });
  const box = await trigger.boundingBox();
  expect(box!.width).toBeGreaterThanOrEqual(44);
  expect(box!.height).toBeGreaterThanOrEqual(44);
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

  // Selecting the row shows the bulk bar with its count.
  await page.getByRole('checkbox', { name: 'Select notes.txt' }).check();
  await expect(page.getByText('1 file selected')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Delete selected' })).toBeVisible();
});

test.describe('at 390px', () => {
  test.use({ viewport: { width: 390, height: 844 } });

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
    expect(sendBox.height).toBeGreaterThanOrEqual(44);
    expect(sendBox.x + sendBox.width).toBeLessThanOrEqual(390);
    expect(await overflowX(page)).toBe(0);

    await input.fill('Ship it on Friday?');
    await send.click();
    await expect(page.locator('#entries-list')).toContainText('Ship it on Friday?');
    await expect(input).toHaveValue('');
  });
});
