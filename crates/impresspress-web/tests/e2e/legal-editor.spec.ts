import { test, expect, request as playwrightRequest, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, adminBearer, loginAsAdmin } from './fixtures/auth';

/**
 * The legal document editor (`blocks/legalpages/pages.rs`, script in
 * `blocks/legalpages/assets/editor.js`), driven against the live server.
 *
 * It writes — drafts and publishes the Terms of Service, deletes and
 * re-creates the Privacy Policy — so it belongs in `e2e:writes`.
 */
test.describe('legal editor', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  const TERMS = '/b/legalpages/admin/terms';
  const PRIVACY = '/b/legalpages/admin/privacy';

  /** Collect the requests the page sends to the editor's save endpoint. */
  function watchSaves(page: Page): string[] {
    const saves: string[] = [];
    page.on('request', (r) => {
      if (r.method() === 'POST' && r.url().endsWith('/b/legalpages/admin/save')) saves.push(r.url());
    });
    return saves;
  }

  /** Press Ctrl+S and report whether anything prevented the browser's default. */
  async function pressSave(page: Page): Promise<boolean> {
    await page.evaluate(() => {
      (window as unknown as { __savePrevented?: boolean }).__savePrevented = undefined;
      // The window sees the key after every document listener has run. The
      // Control key's own keydown comes first and is not the shortcut.
      const listener = (e: KeyboardEvent) => {
        if (e.key.toLowerCase() !== 's') return;
        (window as unknown as { __savePrevented?: boolean }).__savePrevented = e.defaultPrevented;
        window.removeEventListener('keydown', listener);
      };
      window.addEventListener('keydown', listener);
    });
    await page.keyboard.press('Control+s');
    return page.evaluate(
      () => (window as unknown as { __savePrevented?: boolean }).__savePrevented === true,
    );
  }

  test('edit, preview, save a draft, then publish as a chosen version', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto(TERMS, { waitUntil: 'networkidle' });

    const status = page.locator('#document-status .badge');
    const live = page.locator('#live-version');
    const version = page.getByLabel('Publish as version', { exact: true });
    const title = page.getByLabel('Title', { exact: true });
    const content = page.getByLabel('Content', { exact: true });
    const editTab = page.getByRole('tab', { name: 'Edit' });
    const previewTab = page.getByRole('tab', { name: 'Preview' });

    // A publish defaults to one past the highest version — never a draft's own 1.
    const liveText = (await live.textContent()) ?? '';
    const liveVersion = Number(/v(\d+)/.exec(liveText)?.[1]);
    expect(liveVersion).toBeGreaterThan(0);
    await expect(version).toHaveValue(String(liveVersion + 1));

    await title.fill('Terms of Service');
    await content.fill('## Use\n\nThe e2e terms, **in bold**.');

    // Preview: a tab that renders the Markdown into its own panel.
    await previewTab.click();
    await expect(previewTab).toHaveAttribute('aria-selected', 'true');
    await expect(editTab).toHaveAttribute('aria-selected', 'false');
    await expect(page.locator('#editor-edit-pane')).toBeHidden();
    await expect(page.locator('#editor-preview strong')).toHaveText('in bold');

    // Arrow keys move between the tabs and take the focus with them.
    await page.keyboard.press('ArrowLeft');
    await expect(editTab).toBeFocused();
    await expect(editTab).toHaveAttribute('aria-selected', 'true');
    await expect(content).toBeVisible();
    await page.keyboard.press('End');
    await expect(previewTab).toBeFocused();
    await page.keyboard.press('Home');
    await expect(editTab).toBeFocused();

    // Save draft.
    const saved = page.waitForResponse((r) => r.url().endsWith('/b/legalpages/admin/save'));
    await page.getByRole('button', { name: 'Save draft' }).click();
    expect((await saved).status()).toBe(200);
    await expect(status).toHaveText('Draft');

    // The draft is what the editor shows after a reload; the live version is unchanged.
    await page.reload({ waitUntil: 'networkidle' });
    await expect(content).toHaveValue('## Use\n\nThe e2e terms, **in bold**.');
    await expect(status).toHaveText('Draft');
    await expect(live).toHaveText(`Live: v${liveVersion}`);

    // Publish as an explicitly chosen version.
    const chosen = liveVersion + 5;
    await version.fill(String(chosen));
    const published = page.waitForResponse((r) => r.url().endsWith('/b/legalpages/admin/publish'));
    await page.getByRole('button', { name: 'Publish' }).click();
    const answer = await published;
    expect(answer.status()).toBe(200);
    expect((await answer.json()).version).toBe(chosen);
    await expect(status).toHaveText('Published');
    await expect(live).toHaveText(`Live: v${chosen}`);
    await expect(version).toHaveValue(String(chosen + 1));

    // The public page serves it.
    await page.goto('/b/legalpages/terms', { waitUntil: 'networkidle' });
    await expect(page.locator('.public-page__content')).toContainText('The e2e terms, in bold.');
    await expect(page.locator('.public-page__version')).toHaveText(`v${chosen}`);
  });

  test('Ctrl+S saves on the editor and is left alone once the editor is gone', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    const errors: string[] = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const saves = watchSaves(page);
    await page.goto(TERMS, { waitUntil: 'networkidle' });

    // On the editor: the shortcut saves a draft instead of the browser's "Save page".
    await page.getByLabel('Content', { exact: true }).fill('## Use\n\nSaved with the keyboard.');
    const saved = page.waitForResponse((r) => r.url().endsWith('/b/legalpages/admin/save'));
    expect(await pressSave(page)).toBe(true);
    expect((await saved).status()).toBe(200);
    expect(saves).toHaveLength(1);

    // An htmx swap replaces the editor while the document — and the script's
    // listeners — live on.
    await page.evaluate(
      () =>
        new Promise<void>((resolve) => {
          document.body.addEventListener('htmx:afterSettle', () => resolve(), { once: true });
          (window as unknown as { htmx: { ajax: (...a: unknown[]) => void } }).htmx.ajax(
            'GET',
            '/b/legalpages/admin/settings',
            { target: '#content', swap: 'innerHTML' },
          );
        }),
    );
    await expect(page.locator('#legal-editor')).toHaveCount(0);

    // Off the editor, Ctrl+S is the browser's again: not prevented, nothing sent, nothing thrown.
    expect(await pressSave(page)).toBe(false);
    await page.waitForTimeout(300);
    expect(saves).toHaveLength(1);
    expect(errors).toEqual([]);
  });

  test('a document with no version starts from the empty state', async ({ page, baseURL }) => {
    await loginAsAdmin(page);
    const errors: string[] = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const saves = watchSaves(page);

    // Remove every privacy version so the type has no row.
    // A cookie-less context: the bearer login is refused alongside a session
    // cookie that carries no Origin.
    const request = await playwrightRequest.newContext({
      baseURL,
      storageState: { cookies: [], origins: [] },
    });
    const auth = { Authorization: await adminBearer(request) };
    const list = await request.get('/b/legalpages/api/documents?type=privacy', { headers: auth });
    expect(list.status()).toBe(200);
    const { records } = (await list.json()) as {
      records: { id: string; data: { doc_type: string } }[];
    };
    for (const r of records.filter((r) => r.data.doc_type === 'privacy')) {
      const del = await request.delete(`/b/legalpages/api/documents/${r.id}`, { headers: auth });
      expect(del.status()).toBe(200);
    }
    await request.dispose();

    await page.goto(PRIVACY, { waitUntil: 'networkidle' });
    await expect(page.getByRole('heading', { level: 2, name: 'No privacy policy yet' })).toBeVisible();
    await expect(page.locator('#legal-editor')).toBeHidden();
    await expect(page.getByRole('button', { name: 'Save draft' })).toBeHidden();
    await expect(page.getByRole('button', { name: 'Publish' })).toBeHidden();

    // Nothing to save yet: the shortcut stays the browser's.
    expect(await pressSave(page)).toBe(false);
    expect(saves).toHaveLength(0);

    await page.getByRole('button', { name: 'Write the privacy policy' }).click();
    await expect(page.getByLabel('Title', { exact: true })).toBeFocused();
    await expect(page.getByLabel('Title', { exact: true })).toHaveValue('Privacy Policy');
    await expect(page.getByLabel('Publish as version', { exact: true })).toHaveValue('1');
    await expect(page.locator('#legal-editor-empty')).toBeHidden();

    // Publishing the first version puts the type back as it was.
    await page.getByLabel('Content', { exact: true }).fill('## Data\n\nWe keep what we need.');
    const published = page.waitForResponse((r) => r.url().endsWith('/b/legalpages/admin/publish'));
    await page.getByRole('button', { name: 'Publish' }).click();
    expect((await published).status()).toBe(200);
    await expect(page.locator('#live-version')).toHaveText('Live: v1');
    await expect(page.locator('#document-status .badge')).toHaveText('Published');
    expect(errors).toEqual([]);
  });
});
