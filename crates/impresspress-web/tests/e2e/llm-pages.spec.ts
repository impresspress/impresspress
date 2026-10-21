import { expect, request as playwrightRequest, test, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, adminBearer, loginAsAdmin } from './fixtures/auth';
import { WIDTHS, isWrong, layout } from './fixtures/layout';
import { PHONE, targetFloor } from './fixtures/targets';

/**
 * The LLM block's admin pages, driven in a browser against the real server.
 *
 * - Providers: the list is the page; Add provider and each row's Edit open a
 *   native `<dialog>` with the form, the save reloads the page, and Delete is
 *   a labelled icon.
 * - Models: with no model listed, the empty state leads to Providers.
 * - Chat: every control has a name, and at 390px the page shows one pane at
 *   a time — the thread list, or a conversation with the model picker above
 *   it — without scrolling sideways.
 *
 * - At every width from the phone to 1280, with the long names, endpoints,
 *   model ids, thread titles and error text a real deployment holds: no
 *   page scrolls sideways, no card or message overflows, the conversation
 *   keeps its room, and each page shows its one primary or none.
 *
 * It creates, edits and deletes a provider and creates a thread, so it is
 * part of `e2e:writes`, which CI runs against its own fresh server. The
 * provider it creates is deleted again at the end, so the models page it
 * fills is empty once more for whatever runs next.
 */

test.use({ storageState: ADMIN_STATE_PATH });

/** No horizontal scroll: the document is no wider than the viewport. */
async function overflowX(page: Page): Promise<number> {
  return page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
}

test.describe.serial('LLM providers and models', () => {
  const name = `e2e-provider-${Date.now().toString(36)}`;

  test('with no model listed, the models page leads to Providers', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/llm/models');
    await expect(page.getByRole('heading', { level: 2, name: 'No models yet' })).toBeVisible();
    await page.getByRole('link', { name: 'Go to providers' }).click();
    await expect(page).toHaveURL(/\/b\/llm\/providers$/);
    await expect(page.getByRole('heading', { level: 1, name: 'Providers' })).toBeVisible();
  });

  test('Add provider opens a modal, and the saved provider is listed', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/llm/providers');

    const dialog = page.getByRole('dialog', { name: 'Add provider' });
    await expect(dialog).toBeHidden();
    // The topbar's primary action (the empty state may offer the same one).
    await page.getByRole('button', { name: 'Add provider' }).first().click();
    await expect(dialog).toBeVisible();
    // Untouched, it closes on the first Esc: nothing in it reads as an
    // unsaved change (chrome.js asks for a second Esc when one does).
    await page.keyboard.press('Escape');
    await expect(dialog).toBeHidden();

    await page.getByRole('button', { name: 'Add provider' }).first().click();
    await expect(dialog).toBeVisible();
    await dialog.getByLabel('Name').fill(name);
    await dialog.getByLabel('Endpoint').fill('https://llm.example.com/v1');
    const reloaded = page.waitForEvent('load');
    await dialog.getByRole('button', { name: 'Add provider' }).click();
    await reloaded;

    await expect(dialog).toBeHidden();
    const row = page.locator('tr', { hasText: name });
    await expect(row).toBeVisible();
    await expect(row).toContainText('None yet');
  });

  test('Edit opens the row’s own modal, filled in, and saves through it', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/llm/providers');

    await page.getByRole('button', { name: `Edit ${name}` }).click();
    const dialog = page.getByRole('dialog', { name: `Edit ${name}` });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByLabel('Name')).toHaveValue(name);
    await expect(dialog.getByLabel('Endpoint')).toHaveValue('https://llm.example.com/v1');
    await expect(dialog.getByLabel('Enabled')).toBeChecked();

    await dialog.getByLabel('Models').fill('e2e-model-a, e2e-model-b');
    const reloaded = page.waitForEvent('load');
    await dialog.getByRole('button', { name: 'Save changes' }).click();
    await reloaded;

    const row = page.locator('tr', { hasText: name });
    await expect(row).toContainText('e2e-model-a, e2e-model-b');

    // The models it now lists reach the models page, with a status badge
    // (the status route answers the cell with one, not with JSON text).
    await page.goto('/b/llm/models');
    const model = page.locator('tr', { hasText: 'e2e-model-a' });
    await expect(model).toBeVisible();
    await expect(model.locator('td[data-label="Status"] .badge')).toBeVisible();
    await expect(model).not.toContainText('{');
  });

  test.describe('on a phone', () => {
    test.use(PHONE);

    test('the providers list fits and its row actions are 44px targets', async ({ page }) => {
      await loginAsAdmin(page);
      await page.goto('/b/llm/providers');
      expect(await overflowX(page)).toBe(0);
      expect(await targetFloor(page)).toBe(44);
      for (const label of [`Edit ${name}`, `Delete ${name}`]) {
        const box = await page.getByRole('button', { name: label }).boundingBox();
        expect(box, label).not.toBeNull();
        expect(box!.width, label).toBeGreaterThan(43.5);
        expect(box!.height, label).toBeGreaterThan(43.5);
      }
    });
  });

  test('Delete is a labelled icon, confirmed, and the row goes', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/llm/providers');
    page.once('dialog', (confirm) => confirm.accept());
    const reloaded = page.waitForEvent('load');
    await page.getByRole('button', { name: `Delete ${name}` }).click();
    await reloaded;
    await expect(page.locator('tr', { hasText: name })).toHaveCount(0);
  });
});

test.describe('LLM chat', () => {
  test('every chat control has a name', async ({ page }) => {
    await loginAsAdmin(page);
    await page.goto('/b/llm/');
    await expect(page.getByRole('button', { name: 'New thread' })).toBeVisible();
    await expect(page.getByLabel('Model', { exact: true })).toBeVisible();
    await expect(page.getByLabel('Message')).toBeAttached();
    await expect(page.getByRole('complementary', { name: 'Threads' })).toBeVisible();
    await expect(page.getByRole('complementary', { name: 'Chat options' })).toBeVisible();
  });

  test('at 390px the chat shows the list, then the new thread with its model picker', async ({
    page,
  }) => {
    await loginAsAdmin(page);
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto('/b/llm/');

    // The list leads; the conversation and its options wait for a thread.
    await expect(page.getByRole('complementary', { name: 'Threads' })).toBeVisible();
    await expect(page.getByLabel('Model', { exact: true })).toBeHidden();
    expect(await overflowX(page)).toBe(0);

    // New thread on a phone goes to the thread's own page.
    await page.getByRole('button', { name: 'New thread' }).click();
    await expect(page).toHaveURL(/\/b\/llm\/threads\/[^/]+$/);

    await expect(page.getByRole('complementary', { name: 'Threads' })).toBeHidden();
    await expect(page.getByLabel('Model', { exact: true })).toBeVisible();
    await expect(page.getByLabel('Message')).toBeEnabled();
    const settings = page.getByRole('complementary', { name: 'Chat options' }).getByRole('link', {
      name: 'Settings',
    });
    await expect(settings).toBeVisible();
    // The crumb is the way back to the list.
    await expect(page.getByRole('link', { name: 'Chat' }).first()).toBeVisible();
    expect(await overflowX(page)).toBe(0);
  });
});

test.describe('LLM pages with long values', () => {
  test('providers, models, a thread and settings fit their frame at every width', async ({ page, baseURL }) => {
    test.setTimeout(240_000);
    const api = await playwrightRequest.newContext({ baseURL, storageState: { cookies: [], origins: [] } });
    const headers = { Authorization: await adminBearer(api), 'Content-Type': 'application/json' };
    const tag = Date.now().toString(36);
    const created: { providers: string[]; overrides: string[] } = { providers: [], overrides: [] };
    try {
      // What a real deployment holds: hyphenated names with no space, an
      // Azure deployment URL, long model ids, and a disabled provider.
      for (const [name, enabled] of [
        [`openai-compatible-northwind-traders-production-eu-west-1-${tag}`, true],
        [`anthropic-backup-${tag}`, false],
      ] as const) {
        const res = await api.post('/b/llm/api/providers', {
          headers,
          data: {
            name,
            protocol: 'open_ai_compatible',
            endpoint: 'https://northwind-traders-production.openai.azure.com/openai/deployments/gpt-4o-2024-08-06-enterprise/v1',
            key_var: 'IMPRESSPRESS__LLM__NORTHWIND_TRADERS_PRODUCTION_API_KEY',
            max_tokens_field: 'max_completion_tokens',
            models: ['gpt-4o-2024-08-06-enterprise-northwind', 'accounts/fireworks/models/llama-v3p1-405b-instruct'],
            enabled,
          },
        });
        expect(res.ok(), await res.text()).toBe(true);
        created.providers.push(((await res.json()) as { id: string }).id);
      }
      // A thread with a long title and messages quoting the ids an error
      // names, one of them unbroken.
      const thread = await api.post('/b/messages/api/contexts', {
        headers,
        data: {
          type: 'conversation',
          title: `Renewal questions for the Northwind Traders enterprise agreement, order ord_1NvQh2KZp7a9XyTqAbCdEfGh ${tag}`,
        },
      });
      expect(thread.ok(), await thread.text()).toBe(true);
      const threadId = ((await thread.json()) as { id: string }).id;
      for (const [role, content] of [
        ['user', "Why did invoice in_1NvQh2KZp7a9XyTqAbCdEfGhIjKlMn fail? Stripe said: No such customer: 'cus_1NvQh2KZp7a9XyTqAbCdEfGh'"],
        ['assistant', 'The customer was deleted before the retry. See https://dashboard.stripe.com/test/customers/cus_1NvQh2KZp7a9XyTqAbCdEfGh/payment_methods/pm_1NvQh2KZp7a9XyTqAbCdEfGh'],
      ] as const) {
        const entry = await api.post(`/b/messages/api/contexts/${threadId}/entries`, {
          headers,
          data: { kind: 'message', role, content },
        });
        expect(entry.ok(), await entry.text()).toBe(true);
      }
      const override = await api.post('/b/llm/api/config', {
        headers,
        data: {
          thread_id: threadId,
          provider_block: `impresspress/provider-llm-northwind-traders-production-${tag}`,
          model: 'accounts/fireworks/models/llama-v3p1-405b-instruct',
        },
      });
      expect(override.ok(), await override.text()).toBe(true);
      created.overrides.push(((await override.json()) as { id: string }).id);

      const pages: Record<string, (width: number) => string[]> = {
        '/b/llm/providers': () => ['Add provider'],
        '/b/llm/models': () => [],
        // The thread's conversation is on screen at every width.
        [`/b/llm/threads/${threadId}`]: () => ['Send'],
        '/b/llm/': (width) => (width >= 960 ? ['Send'] : []),
        '/b/llm/settings': () => [],
      };
      await loginAsAdmin(page);
      const found: Record<string, unknown> = {};
      for (const width of WIDTHS) {
        await page.setViewportSize({ width, height: 900 });
        for (const [path, primaries] of Object.entries(pages)) {
          await page.goto(path, { waitUntil: 'networkidle' });
          const r = await layout(page);
          if (isWrong(r, primaries(width))) found[`${path} at ${width}`] = r;
        }
      }
      expect(found, 'a page scrolled sideways, a card overflowed, the chat was squeezed or the primaries differ').toEqual({});
    } finally {
      for (const id of created.overrides) await api.delete(`/b/llm/api/config/${id}`, { headers });
      for (const id of created.providers) await api.delete(`/b/llm/api/providers/${id}`, { headers });
      await api.dispose();
    }
  });
});
