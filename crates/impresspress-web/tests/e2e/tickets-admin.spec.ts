import { expect, request as playwrightRequest, test, type APIRequestContext, type Page } from '@playwright/test';
import { ADMIN_STATE_PATH, adminBearer, loginAsAdmin } from './fixtures/auth';
import { WIDTHS, isWrong, layout } from './fixtures/layout';
import { PHONE, targetFloor } from './fixtures/targets';

/**
 * The tickets admin in a browser: create an internal ticket from the inbox's
 * modal, add a note, move it through the workflow (including the refusal
 * when closing without a reason), filter the inbox, and create a ticket type
 * from its modal. Every write is an htmx form answered by the block's own
 * admin routes; the markup is pinned by the Rust tests in
 * `blocks/tickets/pages.rs` and `forms.rs`.
 *
 * It writes and makes one refused request, so it is part of `e2e:writes`.
 * The tickets block ships disabled; the spec turns it on through the admin's
 * own block toggle when it is off.
 */

const UNIQUE = `${Date.now()}-${Math.floor(Math.random() * 1e6)}`;

/**
 * Click Save changes, first proving it is not covered: a trial click fails
 * when another element (a toast) would receive the pointer.
 */
async function saveChanges(page: Page): Promise<void> {
  const save = page.getByRole('button', { name: 'Save changes' });
  await save.scrollIntoViewIfNeeded();
  await save.click({ trial: true, timeout: 2000 });
  await save.click();
}

/**
 * Open the inbox filters: a closed disclosure below 720px, always shown above
 * it. Decided by the viewport, not by probing the page, and each step waits
 * for the state it needs.
 */
async function openFilters(page: Page): Promise<void> {
  const status = page.getByRole('combobox', { name: 'Status' });
  if ((page.viewportSize()?.width ?? 1440) <= 720) {
    const summary = page.locator('.ticket-filters-disclosure__summary');
    await expect(summary).toBeVisible();
    if (!(await status.isVisible())) await summary.click();
  }
  await expect(status).toBeVisible();
}

async function seeder(baseURL: string | undefined): Promise<{
  api: APIRequestContext;
  headers: Record<string, string>;
}> {
  const api = await playwrightRequest.newContext({
    baseURL,
    storageState: { cookies: [], origins: [] },
  });
  const headers = { Authorization: await adminBearer(api), 'Content-Type': 'application/json' };
  return { api, headers };
}

test.describe('tickets admin', () => {
  test.use({ storageState: ADMIN_STATE_PATH });

  let typeTitle = '';

  test.beforeAll(async ({ baseURL }) => {
    const { api, headers } = await seeder(baseURL);
    const probe = await api.get('/b/tickets/api/admin/types', { headers });
    if (probe.status() === 404) {
      const toggle = await api.post('/b/admin/blocks/impresspress--tickets/toggle', {
        headers: { Authorization: headers.Authorization, 'HX-Request': 'true' },
      });
      expect(toggle.status(), await toggle.text()).toBe(200);
    }
    typeTitle = `Content issue ${UNIQUE}`;
    const created = await api.post('/b/tickets/api/admin/types', {
      headers,
      data: { key: `content-${UNIQUE}`.slice(0, 48), title: typeTitle, sort_order: 1 },
    });
    expect(created.status(), await created.text()).toBe(201);
    await api.dispose();
  });

  for (const [label, device] of [
    ['1440', { viewport: { width: 1440, height: 900 } }],
    ['390', PHONE],
  ] as const) {
    test.describe(`at ${label}px`, () => {
      test.use(device);
      test(`create, note, move and filter a ticket`, async ({ page }) => {
        const subject = `Footer link is broken ${UNIQUE} ${label}`;
        await page.goto('/b/tickets/admin/tickets', { waitUntil: 'networkidle' });
        await loginAsAdmin(page);
        await expect(page.getByRole('heading', { level: 1 })).toHaveText('Tickets');

        // The New ticket modal, opened from the topbar.
        await page.getByRole('button', { name: 'New ticket' }).first().click();
        const dialog = page.getByRole('dialog', { name: 'New internal ticket' });
        await expect(dialog).toBeVisible();
        await dialog.getByLabel('Type').selectOption({ label: typeTitle });
        await dialog.getByLabel('Subject').fill(subject);
        await dialog.getByLabel('Description').fill('The privacy link in the footer answers 404 on every page.');
        await dialog.getByLabel('Priority').selectOption('high');
        await dialog.getByRole('button', { name: 'Create ticket' }).click();

        // A created ticket opens on its own page.
        await expect(page).toHaveURL(/\/b\/tickets\/admin\/tickets\/[^/?]+$/);
        await expect(page.getByRole('heading', { level: 1 })).toHaveText(/^TKT-/);
        await expect(page.getByRole('heading', { level: 2, name: subject })).toBeVisible();
        const hero = page.locator('.detail-hero');
        await expect(hero.getByText('New', { exact: true })).toBeVisible();
        await expect(hero.getByText('High', { exact: true })).toBeVisible();

        // An internal note lands on the timeline, with a toast.
        await page.getByRole('textbox', { name: 'Internal note' }).fill('Checked the footer template.');
        await page.getByRole('button', { name: 'Add note' }).click();
        const noted = page.locator('.toast-success', { hasText: 'Note added' });
        await expect(noted).toBeVisible();
        // Its dismiss control is a full target for the pointer (44px under a
        // finger), though the glyph is small.
        const floor = await targetFloor(page);
        expect(floor).toBe(label === '390' ? 44 : 24);
        const dismiss = await noted.getByRole('button', { name: 'Dismiss' }).boundingBox();
        expect(dismiss?.width).toBeGreaterThan(floor - 0.5);
        expect(dismiss?.height).toBeGreaterThan(floor - 0.5);
        await expect(page.locator('.ticket-timeline')).toContainText('Checked the footer template.');

        // Moving to Investigating re-renders the ticket with its new badge.
        await page.getByRole('combobox', { name: 'Status' }).selectOption('investigating');
        await saveChanges(page);
        await expect(page.locator('.toast-success', { hasText: 'Ticket updated' })).toBeVisible();
        await expect(hero.getByText('Investigating', { exact: true })).toBeVisible();
        await expect(page.locator('.ticket-timeline')).toContainText('Moved to Investigating');

        // Closing without a reason is refused with the server's sentence, and
        // nothing changes.
        await page.getByRole('combobox', { name: 'Status' }).selectOption('resolved');
        await saveChanges(page);
        await expect(page.locator('.toast-error', { hasText: 'a reason is required' })).toBeVisible();
        await expect(hero.getByText('Investigating', { exact: true })).toBeVisible();

        await page.getByRole('textbox', { name: 'Reason' }).fill('Fixed the link.');
        await saveChanges(page);
        await expect(hero.getByText('Resolved', { exact: true })).toBeVisible();

        // The inbox filters by the real status set.
        await page.goto('/b/tickets/admin/tickets', { waitUntil: 'networkidle' });
        await openFilters(page);
        await page.getByRole('combobox', { name: 'Status' }).selectOption('resolved');
        await page.getByRole('button', { name: 'Apply filters' }).click();
        await expect(page).toHaveURL(/status=resolved/);
        const row = page.locator('.data-table__row', { hasText: subject });
        await expect(row).toBeVisible();

        await openFilters(page);
        await page.getByRole('combobox', { name: 'Status' }).selectOption('spam');
        await page.getByRole('combobox', { name: 'Type' }).selectOption({ label: typeTitle });
        await page.getByRole('button', { name: 'Apply filters' }).click();
        await expect(page.getByRole('heading', { name: 'No tickets match these filters' })).toBeVisible();
        await page.getByRole('link', { name: 'Clear filters' }).click();
        await expect(page).toHaveURL(/\/b\/tickets\/admin\/tickets$/);

        // A row opens its ticket.
        await page.locator('.data-table__row', { hasText: subject }).click();
        await expect(page.getByRole('heading', { level: 2, name: subject })).toBeVisible();
      });
    });
  }

  // The pages with the long values a real deployment holds — a type title
  // in parentheses, a subject, an assignee id and a page path with no space,
  // a subject that is a pasted URL,
  // an analysis quoting ids — at every width from the phone to 1280: no page
  // scrolls sideways, no record table scrolls inside its frame, no card
  // overflows, and each page shows its one primary or none.
  test('the tickets pages fit their frame at every width with long values', async ({ page, baseURL }) => {
    test.setTimeout(180_000);
    const { api, headers } = await seeder(baseURL);
    const type = await api.post('/b/tickets/api/admin/types', {
      headers,
      data: {
        key: `business-info-${UNIQUE}`.slice(0, 48),
        title: `Incorrect business information (hours, address, phone) ${UNIQUE}`,
        escalation_kind: 'privacy',
        sort_order: 2,
      },
    });
    expect(type.ok(), await type.text()).toBe(true);
    const typeId = ((await type.json()) as { id: string }).id;
    const ticket = await api.post('/b/tickets/api/admin/tickets', {
      headers,
      data: {
        source: 'api',
        type_id: typeId,
        subject: 'Northwind Traders listing shows the wrong opening hours for the Wellington CBD store',
        description:
          "The listing says Monday opening is 8am, but the store opens at 9am. Stripe event evt_1NvQh2KZp7a9XyTqLongEventIdentifier names the account 'acct_1NvQh2KZp7a9XyTqLongAccountIdentifier'.",
        source_path: '/activities/northwind-traders-wellington-cbd-store-lambton-quay/opening-hours',
        subject_type: 'listing',
        subject_id: 'listing_01HZX9Q2KZp7a9XyTqAbCdEfGhIjKlMnOpQrSt',
        evidence_url: 'https://northwind-traders-production.example.com/stores/wellington-cbd/opening-hours?utm_source=report',
        priority: 'urgent',
      },
    });
    expect(ticket.ok(), await ticket.text()).toBe(true);
    const ticketId = ((await ticket.json()) as { id: string }).id;
    // What a public reporter pastes as the subject: the page's URL, with no
    // space to break at.
    const pasted = await api.post('/b/tickets/api/admin/tickets', {
      headers,
      data: {
        source: 'api',
        type_id: typeId,
        subject: 'https://northwind-traders-production.example.com/activities/wellington-cbd-store-lambton-quay/opening-hours?listing=listing_01HZX9Q2KZp7a9XyTqAbCdEfGh',
        description: 'The opening hours on this page are wrong: Monday opens at 9am, not 8am.',
        priority: 'high',
      },
    });
    expect(pasted.ok(), await pasted.text()).toBe(true);
    const pastedId = ((await pasted.json()) as { id: string }).id;
    const assigned = await api.patch(`/b/tickets/api/admin/tickets/${ticketId}`, {
      headers,
      data: { assignee_id: 'user_01HZX9Q2KZp7a9XyTqAbCdEfGh-ana.lucia.fernandez-gonzalez', reason: '' },
    });
    expect(assigned.ok(), await assigned.text()).toBe(true);
    const analysis = await api.post(`/b/tickets/api/admin/tickets/${ticketId}/analyses`, {
      headers,
      data: {
        source: 'triage-agent-northwind-traders-production',
        model: 'accounts/fireworks/models/llama-v3p1-405b-instruct',
        summary: "The report quotes evt_1NvQh2KZp7a9XyTqLongEventIdentifier and 'acct_1NvQh2KZp7a9XyTqLongAccountIdentifier'.",
        confidence: 0.82,
        suggested_actions: [{ action: 'update_listing_hours', listing_id: 'listing_01HZX9Q2KZp7a9XyTqAbCdEfGhIjKlMnOpQrSt' }],
      },
    });
    expect(analysis.ok(), await analysis.text()).toBe(true);
    await api.dispose();

    const pages: Record<string, string[]> = {
      '/b/tickets/admin/tickets': ['New ticket'],
      [`/b/tickets/admin/tickets/${ticketId}`]: ['Save changes'],
      [`/b/tickets/admin/tickets/${pastedId}`]: ['Save changes'],
      '/b/tickets/admin/types': ['New type'],
      '/b/tickets/admin/settings': [],
      '/b/tickets/admin/endpoints': [],
    };
    const found: Record<string, unknown> = {};
    for (const width of WIDTHS) {
      await page.setViewportSize({ width, height: 900 });
      for (const [path, primaries] of Object.entries(pages)) {
        await page.goto(path, { waitUntil: 'networkidle' });
        const r = await layout(page);
        if (isWrong(r, primaries)) found[`${path} at ${width}`] = r;
      }
    }
    expect(found, 'a page scrolled sideways, a card overflowed or the primaries differ').toEqual({});
  });

  test.describe('the filters disclosure on a phone', () => {
    test.use(PHONE);
    // A button lookalike takes the buttons' 8px corners (`--radius-md`), not
    // a pill's.
    test('its summary has the buttons\' corners', async ({ page }) => {
      await page.goto('/b/tickets/admin/tickets', { waitUntil: 'networkidle' });
      const summary = page.locator('.ticket-filters-disclosure__summary');
      await expect(summary).toBeVisible();
      const [summaryRadius, buttonRadius] = await page.evaluate(() => [
        getComputedStyle(document.querySelector('.ticket-filters-disclosure__summary')!).borderTopLeftRadius,
        getComputedStyle(document.querySelector('.topbar__actions .btn')!).borderTopLeftRadius,
      ]);
      expect(summaryRadius).toBe('8px');
      expect(summaryRadius).toBe(buttonRadius);
    });
  });

  test('create a ticket type from its modal', async ({ page }) => {
    const title = `Billing ${UNIQUE}`;
    await page.goto('/b/tickets/admin/types', { waitUntil: 'networkidle' });
    await page.getByRole('button', { name: 'New type' }).first().click();
    const dialog = page.getByRole('dialog', { name: 'New ticket type' });
    await expect(dialog).toBeVisible();
    await dialog.getByLabel('Key').fill(`billing-${UNIQUE}`.slice(0, 48));
    await dialog.getByLabel('Title').fill(title);
    await dialog.getByLabel('Offered on the public report form').check();
    await dialog.getByRole('button', { name: 'Create type' }).click();
    await expect(dialog).toBeHidden();
    await expect(page.locator('.toast-success', { hasText: 'Ticket type created' })).toBeVisible();
    await expect(page.locator('#ticket-types')).toContainText(title);

    // Its Edit button opens a modal with the stored values.
    await page.getByRole('button', { name: `Edit ${title}` }).click();
    const edit = page.getByRole('dialog', { name: title });
    await expect(edit.getByLabel('Title')).toHaveValue(title);
    await expect(edit.getByLabel('Offered on the public report form')).toBeChecked();
    await edit.getByRole('button', { name: 'Cancel' }).click();
    await expect(edit).toBeHidden();
  });
});
