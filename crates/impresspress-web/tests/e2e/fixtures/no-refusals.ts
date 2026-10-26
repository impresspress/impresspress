import { chromium, type FullConfig } from '@playwright/test';
import { ADMIN_STATE_PATH } from './global-setup';
import { ADMIN_ROUTES } from './routes';

/**
 * The read-only run's guard: after `e2e:visual`, the request log of the
 * server it ran against holds no request it refused or failed, apart from
 * the pages below that are captured for the refusal itself.
 *
 * `e2e:visual` captures the admin dashboard, whose "Requests today" stat
 * counts client errors and whose two error charts are compared unmasked
 * (`visual-baseline.spec.ts`, `volatileMasks`). A refused request made
 * before that capture changes the baseline, and which specs run before it
 * depends on the file order. So the run makes none: a spec that writes or is
 * refused belongs in `e2e:writes`, on its own server. Before this guard a
 * stray refusal surfaced as a dashboard regen surprise; now it fails the run
 * by name.
 *
 * It reads the server's own request log (the System logs tab, filtered to
 * server and client errors): the table the dashboard counts, so whatever
 * could move the dashboard's figures is caught, however a spec made the
 * request (a page's navigations and fetches, `page.request`, a second
 * browser context, an API context of its own). What that log does not
 * record is not seen here: a static asset (`/b/static/…`, a 404 included),
 * `/favicon.ico`, and a path outside every block (`/totally-not-a-thing`).
 * A refusal on a block's route, or under `/b/` for no block, is recorded.
 * The run needs a fresh server, as CI gives it.
 */

/** Refusals a capture makes on purpose: the routes `fixtures/routes.ts`
 * marks `refused`, pages the read-only server answers with 403 (seller
 * products are off on it) that the suite captures, after the dashboard in
 * `visual-baseline.spec.ts`. The dashboard capture asserts its own
 * client-error count is zero, so one of these moving ahead of it fails
 * there. */
const CAPTURED_REFUSALS = new Set(ADMIN_ROUTES.filter((r) => r.refused).map((r) => `${r.refused} GET ${r.path}`));

const PAGE_SIZE = 100;

export default async function noRefusals(config: FullConfig): Promise<void> {
  const baseURL = config.projects[0]?.use?.baseURL ?? `http://127.0.0.1:${process.env.TEST_PORT ?? 8080}`;
  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ baseURL, storageState: ADMIN_STATE_PATH });
    const logged: string[] = [];
    for (let n = 1; ; n++) {
      await page.goto(`/b/admin/logs?server_errors=1&client_errors=1&page=${n}&page_size=${PAGE_SIZE}`);
      const rows = await page.locator('#content table tbody tr').evaluateAll((trs) =>
        trs.map((tr) => {
          const cell = (label: string) => tr.querySelector(`td[data-label="${label}"]`)?.textContent?.trim() ?? '?';
          return `${cell('Status')} ${cell('Method')} ${cell('Path').replace(/\s+/g, '')}`;
        }),
      );
      logged.push(...rows);
      if (rows.length < PAGE_SIZE) break;
    }
    const unexpected = logged.filter((row) => !CAPTURED_REFUSALS.has(row));
    if (unexpected.length > 0) {
      throw new Error(
        `the read-only run (e2e:visual) made requests the server refused or failed; ` +
          `move the spec that made them to e2e:writes:\n  ${[...new Set(unexpected)].join('\n  ')}`,
      );
    }
  } finally {
    await browser.close();
  }
}
