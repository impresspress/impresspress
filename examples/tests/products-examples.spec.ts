import { expect, test, type Page, type Route } from "@playwright/test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

type JsonObject = Record<string, unknown>;

type ExampleFixture = {
  slug: string;
  title: string;
  developer: {
    template: string;
    ownership: string;
    fulfillment: string;
    stripe_features: string[];
    journey: string[];
  };
  operator: { journey: string[] };
  product: {
    id: string;
    name: string;
    offers: Array<{
      id: string;
      mode: "payment" | "subscription";
      variables: Array<{ key: string; kind: string }>;
    }>;
  };
  scenario: {
    offer_id: string;
    quantity: number;
    inputs: Record<string, unknown>;
    presentation: "hosted" | "embedded" | "payment_link";
    expected_total_label: string;
    quote: JsonObject;
  };
};

type CapturedCalls = {
  previews: JsonObject[];
  checkouts: JsonObject[];
};

const examplesRoot = process.cwd();
const productsRoot = join(examplesRoot, "products");
const storefrontScript = readFileSync(
  join(examplesRoot, "..", "crates", "impresspress-core", "src", "blocks", "products", "assets", "storefront.js"),
  "utf8",
);
const apiOrigin = "http://127.0.0.1:4179";
const siteOrigin = `http://127.0.0.1:${process.env.PRODUCT_EXAMPLES_PORT || "4178"}`;
const slugs = [
  "digital-download",
  "boutique-store",
  "saas-plans",
  "usage-saas",
  "membership",
  "event-tickets",
  "course-configurator",
  "professional-services",
  "marketplace",
  "donation-campaign",
] as const;
const fixtures = slugs.map((slug) => JSON.parse(
  readFileSync(join(productsRoot, slug, "commerce.fixture.json"), "utf8"),
) as ExampleFixture);

function corsHeaders(contentType = "application/json; charset=utf-8") {
  return {
    "access-control-allow-origin": "*",
    "access-control-allow-headers": "content-type",
    "access-control-allow-methods": "GET,POST,OPTIONS",
    "content-type": contentType,
  };
}

async function setupCommerce(page: Page, fixture: ExampleFixture): Promise<CapturedCalls> {
  const calls: CapturedCalls = { previews: [], checkouts: [] };

  await page.addInitScript(() => {
    Object.defineProperty(window, "Stripe", {
      configurable: true,
      value: () => ({
        initEmbeddedCheckout: async ({ fetchClientSecret }: { fetchClientSecret: () => Promise<string> }) => {
          await fetchClientSecret();
          return {
            mount(node: HTMLElement) {
              node.dataset.testid = "embedded-checkout";
              node.textContent = "Embedded Checkout ready";
            },
            destroy() {},
          };
        },
      }),
    });
  });

  await page.route(`${apiOrigin}/**`, async (route: Route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (request.method() === "OPTIONS") {
      await route.fulfill({ status: 204, headers: corsHeaders() });
      return;
    }
    if (url.pathname === "/b/products/storefront.js") {
      await route.fulfill({
        status: 200,
        headers: corsHeaders("application/javascript; charset=utf-8"),
        body: storefrontScript,
      });
      return;
    }
    if (url.pathname === `/b/products/storefront/${fixture.product.id}`) {
      await route.fulfill({ status: 200, headers: corsHeaders(), json: fixture.product });
      return;
    }
    if (url.pathname === "/b/products/pricing/preview") {
      calls.previews.push(request.postDataJSON() as JsonObject);
      await route.fulfill({ status: 200, headers: corsHeaders(), json: fixture.scenario.quote });
      return;
    }
    if (url.pathname === "/b/products/storefront/config") {
      await route.fulfill({
        status: 200,
        headers: corsHeaders(),
        json: { stripe_publishable_key: "pk_test_examples", embedded_checkout_available: true },
      });
      return;
    }
    if (url.pathname === "/b/products/checkout") {
      calls.checkouts.push(request.postDataJSON() as JsonObject);
      await route.fulfill({
        status: 200,
        headers: corsHeaders(),
        json: {
          order_id: `order-${fixture.slug}`,
          receipt_token: `receipt-${fixture.slug}`,
          receipt_token_expires_at: "2099-01-01T00:00:00Z",
          presentation: fixture.scenario.presentation,
          checkout_url: `${siteOrigin}/${fixture.slug}/?mock_checkout=complete#stripe`,
          client_secret: `cs_test_${fixture.slug}`,
          amounts: (fixture.scenario.quote as { amounts: JsonObject }).amounts,
        },
      });
      return;
    }
    await route.fulfill({
      status: 404,
      headers: corsHeaders(),
      json: { message: `Unhandled mock endpoint: ${request.method()} ${url.pathname}` },
    });
  });

  return calls;
}

async function openExample(page: Page, fixture: ExampleFixture) {
  await page.goto(`/${fixture.slug}/`);
  await expect(page.getByTestId("example-hero")).toBeVisible();
  await expect(page.locator("impresspress-product .title")).toHaveText(fixture.product.name);
  await expect(page.locator("[data-template]")).toHaveText(fixture.developer.template);
  await expect(page.locator("[data-developer-journey] li")).toHaveCount(fixture.developer.journey.length);
  await expect(page.locator("[data-operator-journey] li")).toHaveCount(fixture.operator.journey.length);
  await expect(page.locator("[data-stripe-features] .badge")).toHaveCount(fixture.developer.stripe_features.length);
}

async function configureScenario(page: Page, fixture: ExampleFixture) {
  const widget = page.locator("impresspress-product");
  const offer = fixture.product.offers.find((candidate) => candidate.id === fixture.scenario.offer_id);
  expect(offer, `missing scenario offer ${fixture.scenario.offer_id}`).toBeTruthy();

  if (fixture.product.offers.length > 1) {
    await widget.locator(".offer").selectOption(fixture.scenario.offer_id);
  }
  if (fixture.scenario.presentation !== "payment_link") {
    await widget.locator(".quantity").fill(String(fixture.scenario.quantity));
    for (const variable of offer!.variables) {
      const value = fixture.scenario.inputs[variable.key];
      const input = widget.locator(`[data-variable="${variable.key}"]`);
      if (variable.kind === "boolean") {
        if (value === true) await input.check();
        else await input.uncheck();
      } else if (variable.kind === "select") {
        await input.selectOption(String(value));
      } else if (variable.kind === "multi_select") {
        await input.selectOption((value as unknown[]).map(String));
      } else {
        await input.fill(String(value));
      }
    }
  }
  await expect(widget.locator(".total span:last-child")).toHaveText(fixture.scenario.expected_total_label);
  await expect(widget.locator(".checkout")).toBeEnabled();
}

for (const fixture of fixtures) {
  test.describe(fixture.title, () => {
    test("static storefront completes its configured buyer journey", async ({ page }) => {
      const calls = await setupCommerce(page, fixture);
      await openExample(page, fixture);
      await configureScenario(page, fixture);

      await expect(page).toHaveScreenshot(`${fixture.slug}-desktop.png`, {
        fullPage: true,
        animations: "disabled",
        maxDiffPixelRatio: 0.015,
      });

      if (fixture.scenario.presentation !== "payment_link") {
        await expect.poll(() => calls.previews.at(-1)).toMatchObject({
          offer_id: fixture.scenario.offer_id,
          quantity: fixture.scenario.quantity,
          inputs: fixture.scenario.inputs,
        });
      }

      await page.locator("impresspress-product .checkout").click();
      if (fixture.scenario.presentation === "payment_link") {
        await expect(page).toHaveURL(/#stripe-link$/);
        expect(calls.checkouts).toHaveLength(0);
      } else if (fixture.scenario.presentation === "embedded") {
        await expect(page.locator("impresspress-product .embedded")).toHaveText("Embedded Checkout ready");
        expect(calls.checkouts.at(-1)).toMatchObject({
          offer_id: fixture.scenario.offer_id,
          quantity: fixture.scenario.quantity,
          inputs: fixture.scenario.inputs,
          presentation: "embedded",
        });
      } else {
        await expect(page).toHaveURL(new RegExp(`/${fixture.slug}/\\?mock_checkout=complete#stripe$`));
        expect(calls.checkouts.at(-1)).toMatchObject({
          offer_id: fixture.scenario.offer_id,
          quantity: fixture.scenario.quantity,
          inputs: fixture.scenario.inputs,
          presentation: "hosted",
        });
      }
    });

    test("mobile layout is accessible and stable", async ({ page }) => {
      await page.setViewportSize({ width: 390, height: 844 });
      await setupCommerce(page, fixture);
      await openExample(page, fixture);
      await configureScenario(page, fixture);

      expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
      const labeling = await page.locator("impresspress-product").evaluate((widget) => {
        const root = widget.shadowRoot!;
        return Array.from(root.querySelectorAll("input, select")).every((input) => {
          const id = input.getAttribute("id");
          return !!input.closest("label")
            || (!!id && !!root.querySelector(`label[for="${CSS.escape(id)}"]`));
        });
      });
      expect(labeling).toBe(true);
      await page.keyboard.press("Tab");
      expect(await page.evaluate(() => document.activeElement !== document.body)).toBe(true);
      await expect(page).toHaveScreenshot(`${fixture.slug}-mobile.png`, {
        fullPage: true,
        animations: "disabled",
        maxDiffPixelRatio: 0.015,
      });
    });
  });
}

test("matrix links every distinct example and documents the integration surface", async ({ page }) => {
  await page.goto("/matrix.html");
  await expect(page.getByRole("heading", { name: "Ten commerce patterns" })).toBeVisible();
  await expect(page.locator("[data-example-row]")).toHaveCount(fixtures.length);
  for (const fixture of fixtures) {
    await expect(page.getByRole("link", { name: fixture.title })).toHaveAttribute("href", `${fixture.slug}/`);
  }
});
