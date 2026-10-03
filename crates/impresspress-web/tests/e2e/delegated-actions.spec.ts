import { test, expect } from '@playwright/test';
import { ADMIN_STATE_PATH, loginAsAdmin } from './fixtures/auth';

/**
 * Behaviour cover for the delegated-action rule.
 *
 * Page markup carries no `on*=` attribute: a control declares
 * `data-action="<verb>"` plus its `data-*` operands, and a delegated listener
 * reads them back. `crates/impresspress-core/src/ui/assets/chrome.js` states
 * the rule and owns the shared verbs; a Rust test
 * (`ui::tests::pages_carry_no_event_handler_attributes`) enforces that no page
 * emits a handler attribute.
 *
 * What neither of those can see is whether the listeners actually fire. These
 * three cases are the ones with a visible effect, so a screenshot would not
 * catch a break either:
 *
 * 1. the database table filter, which used to be a 478-character minified
 *    `oninput` attribute — the longest literal handler in the tree (the
 *    longest handler outright was a ~521-character one built with `format!`
 *    on the account-security page);
 * 2. `modal-open` / `modal-close`, which replaced 16 `openModal('…')` /
 *    `closeModal('…')` attribute strings;
 * 3. `reveal-toggle` (a toggle button: `aria-pressed` and its icon follow the
 *    field), which replaced two copies of a hand-written password-reveal
 *    handler — plus the settings form's CSS-only behaviours beside it: a
 *    gated section following its switch, and the colour swatch's
 *    `mirror-value` pair;
 * 4. that a delegated listener is bound ONCE no matter how many times its page
 *    is swapped in, which the first three cannot see because each of them does
 *    a fresh navigation.
 * 5. the command palette's combobox wiring (`aria-activedescendant` following
 *    the selection), which has no visible effect at all.
 *
 * They also prove the load-order assumption: `chrome.js` is `defer`red from
 * `<head>`, so its listeners must be installed before a user can click.
 */
test.describe('delegated actions', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  test('the database table filter hides non-matching rows and shows the empty hint', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/database', { waitUntil: 'networkidle' });

    const rows = page.locator('[data-db-table]');
    const shown = page.locator('[data-db-table]:not([hidden])');
    await expect(rows.first()).toBeVisible();
    const total = await rows.count();
    expect(total).toBeGreaterThan(1);

    // Every deployment has the auth block's users table, and its full name
    // matches nothing else.
    const users = page.locator('[data-db-table="wafer_run__auth__users"]');
    const filter = page.locator('#db-filter');

    await filter.fill('wafer_run__auth__users');
    await expect(users).toBeVisible();
    await expect(shown).toHaveCount(1);
    await expect(page.locator('#db-filter-empty')).toBeHidden();

    // A query nothing matches collapses every group and reveals the hint.
    await filter.fill('zzz-no-such-table');
    await expect(page.locator('#db-filter-empty')).toBeVisible();
    await expect(users).toBeHidden();

    // Clearing restores the full list.
    await filter.fill('');
    await expect(users).toBeVisible();
    await expect(shown).toHaveCount(total);
    await expect(page.locator('#db-filter-empty')).toBeHidden();
  });

  test('modal-open and modal-close drive the create-variable modal', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/variables', { waitUntil: 'networkidle' });

    const modal = page.locator('#create-var');
    await expect(modal).toBeHidden();

    await page.locator('[data-action="modal-open"][data-modal-target="create-var"]').click();
    await expect(modal).toBeVisible();

    // Two controls in this modal carry the verb — the page's own Cancel button
    // and the × that `components::modal` renders. Both are exercised, one per
    // open, rather than matched by one selector that resolves to both.
    await modal.getByRole('button', { name: 'Cancel' }).click();
    await expect(modal).toBeHidden();

    // The close button `components::modal` renders is the same verb.
    await page.locator('[data-action="modal-open"][data-modal-target="create-var"]').click();
    await expect(modal).toBeVisible();
    await modal.locator('button.modal-close').click();
    await expect(modal).toBeHidden();
  });

  test('reveal-toggle unmasks a typed secret and reports it through aria-pressed', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/email', { waitUntil: 'networkidle' });

    const field = page.locator('#IMPRESSPRESS__EMAIL__MAILGUN_API_KEY');
    const toggle = page.getByRole('button', { name: 'Show Mailgun API Key' });

    // A sensitive field renders blank: nothing to show, so no eye.
    await expect(field).toHaveAttribute('type', 'password');
    await expect(field).toHaveValue('');
    await expect(toggle).toBeHidden();

    // Typed into (never saved — this spec writes nothing), it can be shown.
    await field.fill('typed-not-saved');
    await expect(toggle).toBeVisible();
    await expect(toggle).toHaveAttribute('aria-pressed', 'false');

    // One constant name; the pressed state and the icon carry the change.
    await toggle.click();
    await expect(field).toHaveAttribute('type', 'text');
    await expect(toggle).toHaveAttribute('aria-pressed', 'true');
    await expect(toggle.locator('.reveal-toggle__hide')).toBeVisible();
    await expect(toggle.locator('.reveal-toggle__show')).toBeHidden();

    await toggle.click();
    await expect(field).toHaveAttribute('type', 'password');
    await expect(toggle).toHaveAttribute('aria-pressed', 'false');
    await expect(toggle.locator('.reveal-toggle__show')).toBeVisible();
  });

  test('a gated settings section follows its switch, and hints describe their fields', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.goto('/b/auth/admin/settings', { waitUntil: 'networkidle' });

    const oauth = page.getByRole('switch', { name: 'Enable OAuth' });
    const clientId = page.getByLabel('GitHub Client ID');

    // Off on a fresh server: the provider fields are out of the way.
    await expect(oauth).not.toBeChecked();
    await expect(clientId).toBeHidden();
    await expect(oauth).toHaveAccessibleDescription('Enable third-party OAuth login');

    // The switch's whole label row is its hit area.
    await page.locator('label.form-switch', { has: oauth }).getByText('Enable OAuth').click();
    await expect(oauth).toBeChecked();
    await expect(clientId).toBeVisible();
    await expect(clientId).toHaveAccessibleDescription('GitHub OAuth client ID');

    await oauth.press('Space');
    await expect(oauth).not.toBeChecked();
    await expect(clientId).toBeHidden();
    // Nothing is saved: the page is left without submitting.
  });

  test('the colour swatch and its hex box mirror each other, and an empty box is unset', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.goto('/b/legalpages/admin/settings', { waitUntil: 'networkidle' });

    const box = page.getByRole('textbox', { name: 'Background Color', exact: true });
    const swatch = page.getByLabel('Background Color picker', { exact: true });

    // Nothing set: the swatch says so instead of showing black.
    await expect(box).toHaveValue('');
    await expect(swatch).toHaveAttribute('data-unset', '');

    await box.fill('#3366cc');
    await expect(swatch).toHaveValue('#3366cc');
    await expect(swatch).not.toHaveAttribute('data-unset', '');

    await box.fill('');
    await expect(swatch).toHaveAttribute('data-unset', '');

    // Picking on the swatch fills the box (and clears the unset mark).
    await swatch.fill('#112233');
    await expect(box).toHaveValue('#112233');
    await expect(swatch).not.toHaveAttribute('data-unset', '');
  });

  /**
   * The hole the other three cases cannot see.
   *
   * The products admin tabs are `hx-get` + `hx-target="#content"`, so a tab is
   * a partial swap, not a navigation. `ui::shell_page` answers a request
   * carrying `HX-Request` with the page body verbatim; the page's `<script>`
   * blocks are IN that body; htmx executes scripts in what it swapped in; and
   * `document` survives the swap. So a `document.addEventListener` written at
   * the top level of one of those blocks is registered again on every visit.
   *
   * Groups, Orders, Groups leaves the catalog script's listeners bound twice
   * without an initialisation guard, and one click on Save then issues two
   * POSTs — or, on Delete, two confirmation dialogs and two DELETEs, the second
   * of which 404s and paints an error. This asserts exactly one request leaves
   * the page, which is the observable that fails without the guard and cannot
   * be seen by a screenshot, by the markup gate, or by a single-visit case.
   *
   * The API call is intercepted and refused, so the run writes nothing.
   */
  test('a delegated listener survives a tab hop without binding twice', async ({ page }) => {
    await loginAsAdmin(page);

    const groupPosts: string[] = [];
    await page.route('**/b/products/api/admin/groups', async (route) => {
      groupPosts.push(route.request().method());
      await route.fulfill({
        status: 503,
        contentType: 'application/json',
        body: JSON.stringify({ message: 'refused by the delegation test' }),
      });
    });

    await page.goto('/b/products/admin/groups', { waitUntil: 'networkidle' });
    await expect(page.locator('[data-action="pc-new"]').first()).toBeVisible();

    // Hop to another tab and back. Both are htmx swaps of `#content`, so the
    // catalog script is parsed and executed a second time.
    await page.locator('.tab', { hasText: 'Orders' }).click();
    await expect(page).toHaveURL(/\/b\/products\/admin\/purchases$/);
    await page.locator('.tab', { hasText: 'Groups' }).click();
    await expect(page).toHaveURL(/\/b\/products\/admin\/groups$/);
    await expect(page.locator('[data-action="pc-new"]').first()).toBeVisible();

    // One click on a control whose handler makes a request.
    await page.locator('[data-action="pc-new"]').first().click();
    await page.locator('#group-editor-name').fill('delegation guard');
    await page.locator('#group-editor button[type="submit"]').click();

    // The refusal is painted once, from one request.
    await expect(page.locator('#catalog-admin-error')).toBeVisible();
    expect(groupPosts).toEqual(['POST']);
  });

  /**
   * The command palette is an ARIA combobox (`ui/palette.rs`): focus stays in
   * the input, and the selected option is announced through the input's
   * `aria-activedescendant`, which chrome.js keeps in step with the arrow
   * keys and the filter. Nothing on screen shows the attribute, so a broken
   * wiring is invisible to a screenshot.
   */
  test('the palette input tracks the selected option in aria-activedescendant', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.goto('/b/admin/', { waitUntil: 'networkidle' });

    await page.keyboard.press('Control+k');
    const input = page.locator('#cmdk-input');
    await expect(input).toBeFocused();
    await expect(input).toHaveAttribute('role', 'combobox');

    // The active descendant is always the one option marked selected.
    const selectedId = () =>
      page.locator('#cmdk-list [role="option"][aria-selected="true"]').getAttribute('id');
    const first = await selectedId();
    expect(first).toBeTruthy();
    await expect(input).toHaveAttribute('aria-activedescendant', first!);

    await page.keyboard.press('ArrowDown');
    const second = await selectedId();
    expect(second).not.toBe(first);
    await expect(input).toHaveAttribute('aria-activedescendant', second!);
    await expect(input).toBeFocused();

    await page.keyboard.press('ArrowUp');
    await expect(input).toHaveAttribute('aria-activedescendant', first!);

    // A filter that matches nothing leaves nothing to point at.
    await input.fill('zzz-no-such-page');
    await expect(page.locator('#cmdk-list [role="option"][aria-selected="true"]')).toHaveCount(0);
    await expect(input).not.toHaveAttribute('aria-activedescendant', /.*/);

    // Clearing the filter selects the first option again.
    await input.fill('');
    await expect(input).toHaveAttribute('aria-activedescendant', first!);
  });
});
