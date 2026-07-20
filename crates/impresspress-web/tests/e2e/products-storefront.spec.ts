import { expect, test, type Page, type Route } from "@playwright/test";
import { fileURLToPath } from "node:url";

const widgetPath = fileURLToPath(
  new URL(
    "../../../impresspress-core/src/blocks/products/assets/storefront.js",
    import.meta.url,
  ),
);
const shopOrigin = "https://shop.example";
const apiOrigin = "https://api.example";

const checkoutPolicy = {
  allow_promotion_codes: false,
  automatic_tax: false,
  collect_billing_address: false,
  collect_shipping_address: false,
  require_terms_consent: false,
  trial_days: 0,
};

function quote(total = 6400) {
  return {
    schema_version: 1,
    offer_id: "offer_configurable",
    offer_version: 3,
    quantity: 1,
    inputs: { seats: 3, support: true },
    components: [
      {
        component_id: "component_base",
        key: "base",
        label: "Workspace",
        included: true,
        required: true,
        unit_amount_minor: 4000,
        quantity: 1,
        total_amount_minor: 4000,
        reason: "required",
      },
      {
        component_id: "component_support",
        key: "support",
        label: "Priority support",
        included: true,
        required: false,
        unit_amount_minor: total - 4000,
        quantity: 1,
        total_amount_minor: total - 4000,
        reason: "support selected",
      },
    ],
    amounts: {
      currency: "NZD",
      subtotal_minor: total,
      discount_minor: 0,
      tax_minor: 0,
      shipping_minor: 0,
      platform_fee_minor: 0,
      total_minor: total,
    },
  };
}

function product(paymentLink = false) {
  const pricing = quote(5500);
  return {
    schema_version: 1,
    id: "product_static",
    name: "Configurable workspace",
    slug: "configurable-workspace",
    description: "A product rendered by a plain custom element.",
    image_url: "",
    tags: ["static"],
    fulfillment_kind: "manual",
    offers: [
      {
        id: "offer_configurable",
        version: 3,
        name: "Build your workspace",
        mode: "payment",
        currency: "NZD",
        pricing_model: "components",
        recurring_interval: null,
        interval_count: 1,
        variables: [
          {
            key: "seats",
            kind: "integer",
            label: "Seats",
            help_text: "Choose between one and ten seats.",
            required: true,
            default_value: 2,
            allowed_values: [],
            minimum: "1",
            maximum: "10",
            step: "1",
            maximum_length: null,
            visibility: "public",
            sort_order: 1,
          },
          {
            key: "support",
            kind: "boolean",
            label: "Priority support",
            help_text: "Adds a dedicated support channel.",
            required: false,
            default_value: false,
            allowed_values: [],
            minimum: null,
            maximum: null,
            step: null,
            maximum_length: null,
            visibility: "public",
            sort_order: 2,
          },
        ],
        checkout: checkoutPolicy,
        payment_links: paymentLink
          ? [
              {
                id: "link_fixed",
                preset_id: "preset_team",
                url: `${shopOrigin}/product#stripe-payment-link`,
                pricing,
              },
            ]
          : [],
      },
    ],
  };
}

async function json(route: Route, body: unknown, status = 200) {
  await route.fulfill({
    status,
    contentType: "application/json",
    body: JSON.stringify(body),
  });
}

async function openStaticPage(page: Page, suffix = "/product") {
  await page.route(`${shopOrigin}/**`, async (route) => {
    await route.fulfill({
      status: 200,
      contentType: "text/html",
      body: "<!doctype html><html><head><meta charset=utf-8></head><body><main></main></body></html>",
    });
  });
  await page.goto(`${shopOrigin}${suffix}`);
  await page.addScriptTag({ path: widgetPath });
}

async function mount(page: Page, presentation: string) {
  await page.evaluate(
    ({ apiOrigin, presentation }) => {
      const widget = document.createElement("impresspress-product");
      widget.setAttribute("api-base", apiOrigin);
      widget.setAttribute("product-id", "product_static");
      widget.setAttribute("presentation", presentation);
      document.querySelector("main")!.appendChild(widget);
    },
    { apiOrigin, presentation },
  );
  const widget = page.locator("impresspress-product");
  await expect(widget.locator(".title")).toHaveText("Configurable workspace");
  return widget;
}

test.describe("products static storefront widget", () => {
  test("previews typed inputs and starts hosted Checkout from plain HTML", async ({ page }) => {
    const requestBodies: Array<{ path: string; body: Record<string, unknown> }> = [];
    await page.route(`${apiOrigin}/**`, async (route) => {
      const url = new URL(route.request().url());
      if (url.pathname === "/b/products/storefront/product_static") {
        return json(route, product());
      }
      const body = (route.request().postDataJSON() || {}) as Record<string, unknown>;
      requestBodies.push({ path: url.pathname, body });
      if (url.pathname === "/b/products/pricing/preview") return json(route, quote());
      if (url.pathname === "/b/products/checkout") {
        return json(route, {
          order_id: "order_hosted",
          receipt_token: "receipt_hosted",
          receipt_token_expires_at: "2026-07-26T00:00:00Z",
          presentation: "hosted",
          checkout_url: `${shopOrigin}/product#stripe-checkout`,
          client_secret: null,
          payment_link_url: null,
          amounts: quote().amounts,
        });
      }
      return json(route, { error: "unexpected route" }, 404);
    });

    await openStaticPage(page);
    const widget = await mount(page, "hosted");
    await widget.getByLabel("Seats").fill("3");
    await widget.getByLabel("Priority support").check();
    await widget.getByLabel("Quantity").fill("2");
    await widget.getByLabel("Email").fill("buyer@example.com");
    await expect(widget.locator(".total span:last-child")).toHaveText("NZD 64.00");

    await widget.getByRole("button", { name: "Continue to secure checkout" }).click();
    await expect(page).toHaveURL(`${shopOrigin}/product#stripe-checkout`);

    const checkout = requestBodies.filter((entry) => entry.path === "/b/products/checkout").at(-1);
    expect(checkout?.body).toMatchObject({
      offer_id: "offer_configurable",
      quantity: 2,
      inputs: { seats: 3, support: true },
      presentation: "hosted",
      buyer_email: "buyer@example.com",
    });
    expect(String(checkout?.body.success_url)).toContain("impresspress_checkout=success");
    expect(String(checkout?.body.success_url)).toContain("session_id={CHECKOUT_SESSION_ID}");
    expect(String(checkout?.body.cancel_url)).toContain("impresspress_checkout=cancel");

    const receipt = await page.evaluate(() =>
      JSON.parse(
        sessionStorage.getItem(
          "impresspress:receipt:https://api.example:product_static",
        ) || "null",
      ),
    );
    expect(receipt).toEqual({
      order_id: "order_hosted",
      receipt_token: "receipt_hosted",
      expires_at: "2026-07-26T00:00:00Z",
    });
  });

  test("renders an immutable Payment Link price without preview or checkout calls", async ({ page }) => {
    const apiPaths: string[] = [];
    await page.route(`${apiOrigin}/**`, async (route) => {
      const path = new URL(route.request().url()).pathname;
      apiPaths.push(path);
      if (path === "/b/products/storefront/product_static") return json(route, product(true));
      return json(route, { error: "Payment Link mode made an unexpected API call" }, 500);
    });

    await openStaticPage(page);
    const widget = await mount(page, "payment_link");
    await expect(widget.locator(".total span:last-child")).toHaveText("NZD 55.00");
    await expect(widget.locator(".variables")).toBeHidden();
    await expect(widget.locator(".quantity-wrap")).toBeHidden();
    await widget.getByRole("button", { name: "Buy with Stripe" }).click();
    await expect(page).toHaveURL(`${shopOrigin}/product#stripe-payment-link`);
    expect(apiPaths).toEqual(["/b/products/storefront/product_static"]);
  });

  test("mounts embedded Checkout using a publishable key and client secret only", async ({ page }) => {
    const requestBodies: Array<{ path: string; body?: Record<string, unknown> }> = [];
    await page.route(`${apiOrigin}/**`, async (route) => {
      const path = new URL(route.request().url()).pathname;
      const body = route.request().postData()
        ? (route.request().postDataJSON() as Record<string, unknown>)
        : undefined;
      requestBodies.push({ path, body });
      if (path === "/b/products/storefront/product_static") return json(route, product());
      if (path === "/b/products/pricing/preview") return json(route, quote());
      if (path === "/b/products/storefront/config") {
        return json(route, {
          schema_version: 1,
          embedded_checkout_available: true,
          stripe_publishable_key: "pk_test_browser_safe",
          stripe_mode: "test",
        });
      }
      if (path === "/b/products/checkout") {
        return json(route, {
          order_id: "order_embedded",
          receipt_token: "receipt_embedded",
          receipt_token_expires_at: "2026-07-26T00:00:00Z",
          presentation: "embedded",
          checkout_url: null,
          client_secret: "cs_test_secret_fragment",
          payment_link_url: null,
          amounts: quote().amounts,
        });
      }
      return json(route, { error: "unexpected route" }, 404);
    });

    await openStaticPage(page);
    await page.evaluate(() => {
      (window as any).Stripe = (publishableKey: string) => {
        (window as any).__stripePublishableKey = publishableKey;
        return {
          initEmbeddedCheckout: async ({ fetchClientSecret }: any) => {
            const clientSecret = await fetchClientSecret();
            return {
              mount(node: HTMLElement) {
                node.textContent = `Embedded Stripe mounted with ${clientSecret}`;
              },
              destroy() {},
            };
          },
        };
      };
    });
    const widget = await mount(page, "embedded");
    await expect(widget.locator(".total span:last-child")).toHaveText("NZD 64.00");
    await widget.getByRole("button", { name: "Open secure checkout" }).click();

    await expect(widget.locator(".embedded")).toContainText(
      "Embedded Stripe mounted with cs_test_secret_fragment",
    );
    await expect(widget.locator("form")).toBeHidden();
    expect(await page.evaluate(() => (window as any).__stripePublishableKey)).toBe(
      "pk_test_browser_safe",
    );
    expect(
      requestBodies.find((entry) => entry.path === "/b/products/checkout")?.body,
    ).toMatchObject({ presentation: "embedded", offer_id: "offer_configurable" });
  });

  test("confirms a guest receipt from server order state after Stripe returns", async ({ page }) => {
    const statusUrls: string[] = [];
    await page.route(`${apiOrigin}/**`, async (route) => {
      const url = new URL(route.request().url());
      if (url.pathname === "/b/products/storefront/product_static") {
        return json(route, product());
      }
      if (url.pathname === "/b/products/pricing/preview") return json(route, quote());
      if (url.pathname === "/b/products/orders/order_returned/status") {
        statusUrls.push(url.toString());
        await new Promise((resolve) => setTimeout(resolve, 50));
        return json(route, {
          schema_version: 1,
          order_id: "order_returned",
          status: "completed",
          reconciliation_status: "reconciled",
          amounts: quote().amounts,
          subscription_cancel_at_period_end: false,
          paid_at: "2026-07-19T04:05:06Z",
        });
      }
      return json(route, { error: "unexpected route" }, 404);
    });

    await openStaticPage(page, "/product?impresspress_checkout=success&session_id=untrusted");
    await page.evaluate(() => {
      sessionStorage.setItem(
        "impresspress:receipt:https://api.example:product_static",
        JSON.stringify({
          order_id: "order_returned",
          receipt_token: "receipt_capability",
          expires_at: "2026-07-26T00:00:00Z",
        }),
      );
    });
    const widget = await mount(page, "hosted");
    await expect(widget.locator(".status")).toHaveText("Payment confirmed — NZD 64.00.");
    expect(statusUrls).toHaveLength(1);
    expect(statusUrls[0]).toContain("receipt_token=receipt_capability");
    expect(statusUrls[0]).not.toContain("session_id=untrusted");
    expect(
      await page.evaluate(() =>
        sessionStorage.getItem(
          "impresspress:receipt:https://api.example:product_static",
        ),
      ),
    ).toBeNull();
  });
});
