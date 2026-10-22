import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { expect, type Page } from '@playwright/test';

/**
 * The shell's own script (`ui/assets/chrome.js`), which opens every
 * `components::Modal` and asks every confirm.
 */
export const chromeJs = readFileSync(
  fileURLToPath(new URL('../../../../impresspress-core/src/ui/assets/chrome.js', import.meta.url)),
  'utf8',
);

/**
 * `components::confirm_dialog`, the one dialog every page asks "are you
 * sure?" in. Kept equal to what pages render by
 * `ui::components::modal::tests::the_confirm_dialog_fixture_is_what_pages_render`.
 */
export const confirmDialogHtml = readFileSync(
  fileURLToPath(new URL('./confirm-dialog.html', import.meta.url)),
  'utf8',
);

/**
 * What a page built by hand needs at the end of its body to confirm the way
 * a served page does: the shared dialog and the script that asks in it. A
 * products script under test calls `window.impresspressConfirm`, which only
 * chrome.js provides.
 */
export const confirmChrome = `${confirmDialogHtml}<script>${chromeJs}</script>`;

/**
 * Answer the shared confirm dialog that is asking now: it must be the one
 * titled `label`, saying `question` when given, and is answered by its
 * confirm button (named `label` too) or its Cancel.
 */
export async function answerConfirm(
  page: Page,
  label: string,
  answer: 'confirm' | 'cancel',
  question?: string,
): Promise<void> {
  const dialog = page.getByRole('alertdialog', { name: label, exact: true });
  await expect(dialog).toBeVisible();
  if (question !== undefined) await expect(dialog).toContainText(question);
  await dialog
    .getByRole('button', { name: answer === 'confirm' ? label : 'Cancel', exact: true })
    .click();
  await expect(dialog).toBeHidden();
}
