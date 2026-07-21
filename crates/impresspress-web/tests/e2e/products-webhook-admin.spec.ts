import { expect, test, type Page, type Route } from "@playwright/test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const pagesPath = fileURLToPath(
  new URL(
    "../../../impresspress-core/src/blocks/products/pages.rs",
    import.meta.url,
  ),
);
const adminOrigin = "https://admin.example";

function stripeSetupScript() {
  const source = readFileSync(pagesPath, "utf8");
  const match = source.match(
    /fn stripe_setup_js\(\) -> &'static str \{\s*r#"\n([\s\S]*?)\n"#\s*\}/,
  );
  if (!match) throw new Error("Could not extract stripe_setup_js from pages.rs");
  return match[1];
}

function webhookEvent(
  overrides: Partial<Record<string, unknown>> = {},
): Record<string, unknown> {
  return {
    id: "evt_dead_123",
    event_type: "checkout.session.completed",
    status: "dead_letter",
    livemode: false,
    attempts: 5,
    stripe_account_id: "acct_seller_123",
    last_error: "Checkout amount did not match the signed order snapshot.",
    next_retry_at: null,
    updated_at: "2026-07-20T01:02:03Z",
    // Unknown server fields must never become part of the rendered operations UI.
    payload: "<img src=x onerror=window.__payloadExecuted=true>",
    processing_owner: "private-worker-lease-token",
    ...overrides,
  };
}

function providerOperation(
  overrides: Partial<Record<string, unknown>> = {},
): Record<string, unknown> {
  return {
    id: "op_refund_123",
    operation_type: "refund.reconcile",
    aggregate_type: "refund",
    aggregate_id: "refund_123",
    stripe_account_id: "acct_seller_123",
    status: "dead_letter",
    attempts: 8,
    last_error: "Stripe refund status could not be retrieved.",
    updated_at: "2026-07-20T02:03:04Z",
    request_json: "private-request-snapshot",
    idempotency_key: "private-idempotency-key",
    processing_owner: "private-provider-worker-token",
    ...overrides,
  };
}

async function json(route: Route, body: unknown, status = 200) {
  await route.fulfill({
    status,
    contentType: "application/json",
    body: JSON.stringify(body),
  });
}

async function openWebhookOperations(page: Page) {
  await page.goto(`${adminOrigin}/b/products/admin/stripe`);
  await page.addScriptTag({ content: stripeSetupScript() });
}

const operationsHtml = `<!doctype html>
<html>
  <body>
    <button id="stripe-test-button" type="button" onclick="testStripeConnection()">Test connection</button>
    <span id="stripe-state"></span>
    <p id="stripe-error"></p>
    <section aria-label="Webhook delivery health">
      <label for="stripe-webhook-filter">Status</label>
      <select id="stripe-webhook-filter" onchange="loadStripeWebhookEvents()">
        <option value="dead_letter" selected>Needs manual review</option>
        <option value="failed">Waiting to retry</option>
        <option value="processing">Processing</option>
        <option value="processed">Processed</option>
        <option value="">All events</option>
      </select>
      <button type="button" onclick="loadStripeWebhookEvents()">Refresh</button>
      <p id="stripe-webhook-summary" aria-live="polite"></p>
      <p id="stripe-webhook-error" role="alert" aria-live="assertive" hidden></p>
      <div id="stripe-webhook-events" aria-live="polite">Loading webhook events…</div>
    </section>
    <section aria-label="Provider reconciliation">
      <label for="stripe-provider-filter">Status</label>
      <select id="stripe-provider-filter" onchange="loadStripeProviderOperations()">
        <option value="dead_letter" selected>Needs manual review</option>
        <option value="failed">Waiting to retry</option>
        <option value="pending">Pending</option>
        <option value="processing">Processing</option>
        <option value="succeeded">Succeeded</option>
        <option value="">All operations</option>
      </select>
      <button id="stripe-provider-reconcile" type="button" onclick="reconcileStripeProviderOperations(this)">Reconcile due operations</button>
      <button type="button" onclick="loadStripeProviderOperations()">Refresh</button>
      <p id="stripe-provider-summary" aria-live="polite"></p>
      <p id="stripe-provider-reconcile-result" role="status" aria-live="polite"></p>
      <p id="stripe-provider-error" role="alert" aria-live="assertive" hidden></p>
      <div id="stripe-provider-operations-list" aria-live="polite">Loading provider operations…</div>
    </section>
  </body>
</html>`;

test.describe("products admin webhook recovery", () => {
  test("tests a configured test-mode Stripe account without exposing credentials", async ({
    page,
  }) => {
    const requests: Array<{ method: string; path: string; body: string | null }> = [];
    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      if (request.resourceType() === "document") {
        return route.fulfill({
          status: 200,
          contentType: "text/html",
          body: operationsHtml,
        });
      }
      requests.push({
        method: request.method(),
        path: url.pathname,
        body: request.postData(),
      });
      if (url.pathname === "/b/products/api/admin/stripe/status") {
        return json(route, {
          state: "connected_test",
          configured: true,
          livemode: false,
          account_id: "acct_platform_test",
          country: "NZ",
          default_currency: "NZD",
          business_name: "Test Studio",
          charges_enabled: true,
          payouts_enabled: true,
          details_submitted: true,
          capabilities: { card_payments: "active" },
          publishable_key_configured: true,
          webhook_secret_configured: true,
          api_version: "2026-02-25.clover",
          error: "",
        });
      }
      if (url.pathname === "/b/products/api/admin/webhook-events") {
        return json(route, { records: [], total_count: 0, page: 1, page_size: 50 });
      }
      if (url.pathname === "/b/products/api/admin/provider-operations") {
        return json(route, { records: [], total_count: 0, page: 1, page_size: 50 });
      }
      return json(route, { message: "Unexpected route" }, 404);
    });

    await openWebhookOperations(page);
    await page.getByRole("button", { name: "Test connection" }).click();
    await expect(page.locator("#stripe-state")).toHaveText("Connected — test mode");
    await expect(page.locator("#stripe-state")).toHaveClass(/badge-info/);
    await expect(page.locator("#stripe-error")).toHaveText("Connection test completed.");
    await expect(page.getByRole("button", { name: "Test connection" })).toBeEnabled();
    expect(
      requests.filter((request) => request.path.endsWith("/stripe/status")),
    ).toEqual([
      {
        method: "GET",
        path: "/b/products/api/admin/stripe/status",
        body: null,
      },
    ]);
    expect(JSON.stringify(requests)).not.toContain("sk_test");
  });

  test("filters safe event summaries and replays a dead letter through the normal endpoint", async ({
    page,
  }) => {
    const requests: Array<{ method: string; url: string }> = [];
    let replayed = false;

    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      if (request.resourceType() === "document") {
        return route.fulfill({
          status: 200,
          contentType: "text/html",
          body: operationsHtml,
        });
      }

      requests.push({ method: request.method(), url: url.toString() });
      if (
        request.method() === "POST" &&
        url.pathname ===
          "/b/products/api/admin/webhook-events/evt_failed_456/replay"
      ) {
        replayed = true;
        return json(route, { accepted: true });
      }
      if (
        request.method() === "GET" &&
        url.pathname === "/b/products/api/admin/webhook-events"
      ) {
        const status = url.searchParams.get("status");
        if (status === "dead_letter") {
          return json(route, {
            records: [webhookEvent()],
            total_count: 1,
            page: 1,
            page_size: 50,
          });
        }
        if (status === "failed" && !replayed) {
          return json(route, {
            records: [
              webhookEvent({
                id: "evt_failed_456",
                event_type: "invoice.payment_failed",
                status: "failed",
                livemode: true,
                attempts: 2,
                stripe_account_id: null,
                last_error: "Temporary database outage.",
                next_retry_at: "2026-07-20T01:07:03Z",
              }),
            ],
            total_count: 1,
            page: 1,
            page_size: 50,
          });
        }
        return json(route, {
          records: [],
          total_count: 0,
          page: 1,
          page_size: 50,
        });
      }
      if (
        request.method() === "GET" &&
        url.pathname === "/b/products/api/admin/provider-operations"
      ) {
        return json(route, { records: [], total_count: 0, page: 1, page_size: 50 });
      }
      return json(route, { message: "Unexpected route" }, 404);
    });

    await openWebhookOperations(page);

    const operations = page.getByRole("region", {
      name: "Webhook delivery health",
    });
    await expect(operations).toContainText("checkout.session.completed");
    await expect(operations).toContainText("evt_dead_123");
    await expect(operations).toContainText("Test · 5");
    await expect(operations).toContainText("1 event match this filter.");
    await expect(operations.locator("img")).toHaveCount(0);
    await expect(operations).not.toContainText("private-worker-lease-token");
    expect(await page.evaluate(() => (window as any).__payloadExecuted)).toBeUndefined();
    expect(requests[0]).toEqual({
      method: "GET",
      url: `${adminOrigin}/b/products/api/admin/webhook-events?page=1&page_size=50&status=dead_letter`,
    });

    await operations.getByLabel("Status").selectOption("failed");
    await expect(operations).toContainText("invoice.payment_failed");
    await expect(operations).toContainText("Live · 2");

    page.once("dialog", async (dialog) => dialog.dismiss());
    await operations
      .getByRole("button", { name: "Replay webhook evt_failed_456" })
      .click();
    expect(requests.filter((request) => request.method === "POST")).toHaveLength(0);

    page.once("dialog", async (dialog) => dialog.accept());
    await operations
      .getByRole("button", { name: "Replay webhook evt_failed_456" })
      .click();
    await expect(operations).toContainText("No matching webhook events.");
    expect(requests.filter((request) => request.method === "POST")).toEqual([
      {
        method: "POST",
        url: `${adminOrigin}/b/products/api/admin/webhook-events/evt_failed_456/replay`,
      },
    ]);

    const getCount = requests.filter((request) => request.method === "GET").length;
    await operations.getByRole("button", { name: "Refresh" }).click();
    await expect
      .poll(() => requests.filter((request) => request.method === "GET").length)
      .toBe(getCount + 1);
  });

  test("surfaces list and replay failures without leaving a disabled action", async ({
    page,
  }) => {
    let listAttempts = 0;
    let replayShouldFail = true;

    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      if (request.resourceType() === "document") {
        return route.fulfill({
          status: 200,
          contentType: "text/html",
          body: operationsHtml,
        });
      }
      if (request.method() === "POST") {
        if (replayShouldFail) {
          replayShouldFail = false;
          return json(route, { message: "Replay integrity check failed." }, 409);
        }
        return json(route, { accepted: true });
      }
      if (
        request.method() === "GET" &&
        url.pathname === "/b/products/api/admin/webhook-events"
      ) {
        listAttempts += 1;
        if (listAttempts === 1) {
          return json(route, { message: "Webhook storage is unavailable." }, 503);
        }
        return json(route, {
          records: [webhookEvent()],
          total_count: 1,
          page: 1,
          page_size: 50,
        });
      }
      if (
        request.method() === "GET" &&
        url.pathname === "/b/products/api/admin/provider-operations"
      ) {
        return json(route, { records: [], total_count: 0, page: 1, page_size: 50 });
      }
      return json(route, { message: "Unexpected route" }, 404);
    });

    await openWebhookOperations(page);
    const operations = page.getByRole("region", {
      name: "Webhook delivery health",
    });
    await expect(operations.getByRole("alert")).toHaveText(
      "Webhook storage is unavailable.",
    );

    await operations.getByRole("button", { name: "Refresh" }).click();
    const replay = operations.getByRole("button", {
      name: "Replay webhook evt_dead_123",
    });
    await expect(replay).toBeEnabled();
    page.once("dialog", async (dialog) => dialog.accept());
    await replay.click();
    await expect(operations.getByRole("alert")).toHaveText(
      "Replay integrity check failed.",
    );
    await expect(replay).toBeEnabled();
    await expect(replay).toHaveText("Replay");
  });

  test("renders safe provider operations and runs bounded reconciliation", async ({
    page,
  }) => {
    const requests: Array<{ method: string; url: string }> = [];
    let reconciled = false;
    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      if (request.resourceType() === "document") {
        return route.fulfill({ status: 200, contentType: "text/html", body: operationsHtml });
      }
      requests.push({ method: request.method(), url: url.toString() });
      if (
        request.method() === "GET" &&
        url.pathname === "/b/products/api/admin/webhook-events"
      ) {
        return json(route, { records: [], total_count: 0, page: 1, page_size: 50 });
      }
      if (
        request.method() === "GET" &&
        url.pathname === "/b/products/api/admin/provider-operations"
      ) {
        return json(route, reconciled
          ? { records: [], total_count: 0, page: 1, page_size: 50 }
          : { records: [providerOperation()], total_count: 1, page: 1, page_size: 50 });
      }
      if (
        request.method() === "POST" &&
        url.pathname === "/b/products/api/admin/provider-operations/reconcile"
      ) {
        reconciled = true;
        return json(route, { claimed: 1, succeeded: 1, retry_scheduled: 0, dead_letter: 0 });
      }
      return json(route, { message: "Unexpected route" }, 404);
    });

    await openWebhookOperations(page);
    const operations = page.getByRole("region", { name: "Provider reconciliation" });
    await expect(operations).toContainText("refund.reconcile");
    await expect(operations).toContainText("refund_123");
    await expect(operations).toContainText("1 operation match this filter.");
    await expect(operations).not.toContainText("private-request-snapshot");
    await expect(operations).not.toContainText("private-idempotency-key");
    await expect(operations).not.toContainText("private-provider-worker-token");

    await operations.getByRole("button", { name: "Reconcile due operations" }).click();
    await expect(operations.getByRole("status")).toHaveText(
      "Claimed 1; completed 1; retry scheduled 0; manual review 0.",
    );
    await expect(operations).toContainText("No matching provider operations.");
    await expect(operations.getByRole("button", { name: "Reconcile due operations" })).toBeEnabled();
    expect(requests.filter((request) => request.method === "POST")).toEqual([
      {
        method: "POST",
        url: `${adminOrigin}/b/products/api/admin/provider-operations/reconcile?limit=50`,
      },
    ]);
  });
});
