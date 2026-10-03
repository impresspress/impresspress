import { test, expect, type Locator, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * Every admin modal is the one `components::modal`: a native `<dialog>` that
 * `ui/assets/chrome.js` opens with `showModal()`.
 *
 * The node tests (`ui/assets/test/chrome_modal.test.mjs`) pin chrome.js's own
 * logic against a stub document. What only a real browser can show is the
 * part the platform does: that the dialog is really modal (the page behind it
 * inert, the dialog in the top layer), that Esc closes it, that focus lands in
 * it and comes back, and that the layout keeps a form's buttons on screen on a
 * phone — the Add Access Grant modal's were below the fold at 390px.
 *
 * Writes nothing: no modal here is submitted, so the shared database the
 * visual baselines are taken from is untouched (see `e2e:visual`). The submit
 * half — a create that closes its modal through the `closeModal` trigger,
 * toasts over it and returns focus — writes a role, so it lives in
 * `htmx-success-effects.spec.ts`, which runs after the baselines.
 */

/** Open a modal through its trigger and return the dialog. */
async function openVia(page: Page, trigger: Locator, id: string): Promise<Locator> {
  await trigger.click();
  const dialog = page.locator(`dialog#${id}`);
  await expect(dialog).toBeVisible();
  // Let the opening animation finish before anything measures the dialog.
  await dialog.evaluate((el) =>
    Promise.all(el.getAnimations({ subtree: true }).map((a) => a.finished)),
  );
  return dialog;
}

/** The dialog is open as a MODAL: in the top layer, with the page inert. */
async function expectModal(dialog: Locator): Promise<void> {
  expect(await dialog.evaluate((el) => el.matches(':modal'))).toBe(true);
  // Named by its h2 title.
  const labelledBy = await dialog.getAttribute('aria-labelledby');
  expect(labelledBy).toBeTruthy();
  await expect(dialog.locator(`h2#${labelledBy}`)).toHaveCount(1);
  // The close button is labelled and at least 44px square.
  const close = dialog.getByRole('button', { name: 'Close' });
  const box = await close.boundingBox();
  // (Sub-pixel layout rounds a 44px box to 43.99…)
  expect(box && box.width >= 43.5 && box.height >= 43.5, JSON.stringify(box)).toBe(true);
}

test.describe('admin modals', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  test('keyboard: focus moves in, Tab stays in, Esc closes, focus returns', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/users?tab=roles', { waitUntil: 'networkidle' });

    const trigger = page.locator('[data-action="modal-open"][data-modal-target="create-role"]');
    await trigger.focus();
    await page.keyboard.press('Enter');

    const dialog = page.locator('dialog#create-role');
    await expect(dialog).toBeVisible();
    await expectModal(dialog);
    await expect(page.locator('#role-name')).toBeFocused();

    // Twice round the dialog's controls and then some, both ways: focus never
    // leaves it — not to the page behind, and not to the browser's own UI.
    for (const key of ['Tab', 'Shift+Tab']) {
      for (let i = 0; i < 12; i++) {
        await page.keyboard.press(key);
        expect(
          await page.evaluate(() => document.activeElement?.closest('dialog#create-role') !== null),
          `${key} #${i} left the dialog`,
        ).toBe(true);
      }
    }

    // The page behind is inert: even a script cannot focus a link in it.
    const stayed = await page.evaluate(() => {
      const behind = document.querySelector<HTMLElement>('.sidebar a[href]');
      behind?.focus();
      return document.activeElement?.closest('dialog#create-role') !== null;
    });
    expect(stayed).toBe(true);

    await page.keyboard.press('Escape');
    await expect(dialog).toBeHidden();
    await expect(trigger).toBeFocused();
  });

  test('the close button and Cancel close the modal and hand focus back', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/users?tab=api-keys', { waitUntil: 'networkidle' });
    const trigger = page.locator('[data-action="modal-open"][data-modal-target="create-api-key"]');

    let dialog = await openVia(page, trigger, 'create-api-key');
    await expectModal(dialog);
    await expect(page.locator('#key-name')).toBeFocused();
    await dialog.getByRole('button', { name: 'Close' }).click();
    await expect(dialog).toBeHidden();
    await expect(trigger).toBeFocused();

    dialog = await openVia(page, trigger, 'create-api-key');
    await dialog.getByRole('button', { name: 'Cancel' }).click();
    await expect(dialog).toBeHidden();
    await expect(trigger).toBeFocused();
  });

  test('a click on the backdrop closes the modal; a click inside does not', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/users?tab=roles', { waitUntil: 'networkidle' });
    const trigger = page.locator('[data-action="modal-open"][data-modal-target="create-role"]');
    const dialog = await openVia(page, trigger, 'create-role');

    await dialog.locator('.modal__body').click({ position: { x: 5, y: 5 } });
    await expect(dialog).toBeVisible();

    await page.mouse.click(5, 5);
    await expect(dialog).toBeHidden();
    await expect(trigger).toBeFocused();
  });

  test('an htmx-loaded modal (variable edit) opens and returns focus to its row button', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/settings/variables?tab=all', { waitUntil: 'networkidle' });

    const edit = page.getByRole('button', { name: 'Edit WAFER_RUN_SHARED__APP_NAME' });
    const dialog = await openVia(page, edit, 'edit-var');
    await expectModal(dialog);
    await expect(dialog.getByRole('heading', { name: 'Edit Variable' })).toBeVisible();

    await page.keyboard.press('Escape');
    await expect(dialog).toBeHidden();
    await expect(edit).toBeFocused();

    // A second row's edit replaces the first modal rather than stacking one.
    await openVia(page, edit, 'edit-var');
    await expect(page.locator('dialog#edit-var')).toHaveCount(1);
  });

  test('the block detail modal opens from its card', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/blocks', { waitUntil: 'networkidle' });

    const card = page.locator('.block-card').first();
    const name = (await card.locator('.block-card__title').textContent())?.trim() ?? '';
    expect(name).not.toBe('');
    const dialog = await openVia(page, card.locator('.block-card__summary'), 'block-detail');
    await expectModal(dialog);
    await expect(dialog.getByRole('heading', { level: 2 })).toHaveText(name);

    await page.keyboard.press('Escape');
    await expect(dialog).toBeHidden();
  });

  test('the storage "New bucket" modal is the shared modal', async ({ page }) => {
    await loginAsAdmin(page);
    for (const path of ['/b/storage/', '/b/storage/admin/buckets']) {
      await page.goto(path, { waitUntil: 'networkidle' });
      const trigger = page.locator('[data-action="modal-open"][data-modal-target="new-bucket"]');
      const dialog = await openVia(page, trigger, 'new-bucket');
      await expectModal(dialog);
      await expect(page.locator('#new-bucket-name')).toBeFocused();

      // The form's own check still runs in the shared modal (a name the
      // `pattern` lets through but S3 refuses), and a close resets it for the
      // next opening.
      await page.locator('#new-bucket-name').fill('ab--cd');
      await dialog.getByRole('button', { name: 'Create bucket' }).click();
      await expect(page.locator('#new-bucket-error')).toBeVisible();
      await dialog.getByRole('button', { name: 'Cancel' }).click();
      await expect(dialog).toBeHidden();
      await expect(trigger).toBeFocused();
      await openVia(page, trigger, 'new-bucket');
      await expect(page.locator('#new-bucket-name')).toHaveValue('');
      await expect(page.locator('#new-bucket-error')).toBeHidden();
      await page.keyboard.press('Escape');
    }
  });

  test.describe('at 390px', () => {
    test.use({ viewport: { width: 390, height: 844 } });

    /**
     * The failing case the rebuild is for: the Add Access Grant form is
     * taller than a phone, and its buttons were below the fold of a modal
     * capped at `90vh`. The footer now sticks to the bottom of the scrolling
     * body, so both buttons are on screen without scrolling.
     */
    test('the Add Access Grant buttons are on screen without scrolling', async ({ page }) => {
      await loginAsAdmin(page);
      await page.goto('/b/admin/settings/permissions?subtab=database', { waitUntil: 'networkidle' });
      const trigger = page.locator('[data-action="modal-open"][data-modal-target="add-grant-modal"]');
      const dialog = await openVia(page, trigger, 'add-grant-modal');
      await expectModal(dialog);

      for (const name of ['Cancel', 'Add Grant']) {
        await expect(dialog.getByRole('button', { name })).toBeInViewport({ ratio: 1 });
      }
      // The dialog fits the viewport, with its gutter.
      const box = await dialog.boundingBox();
      expect(box && box.y >= 0 && box.y + box.height <= 844, JSON.stringify(box)).toBe(true);
      expect(box && box.x >= 0 && box.x + box.width <= 390, JSON.stringify(box)).toBe(true);
      // Footer buttons share the row in full-width halves.
      const cancel = await dialog.getByRole('button', { name: 'Cancel' }).boundingBox();
      const submit = await dialog.getByRole('button', { name: 'Add Grant' }).boundingBox();
      expect(cancel && submit && Math.abs(cancel.width - submit.width) <= 1).toBe(true);
      expect(cancel && submit && Math.abs(cancel.y - submit.y) <= 1, JSON.stringify([cancel, submit])).toBe(true);
    });
  });
});
