import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const pagesPath = fileURLToPath(
  new URL(
    "../../../impresspress-core/src/blocks/products/pages.rs",
    import.meta.url,
  ),
);
const adminOrigin = "https://admin.example";

function productWizardScript() {
  const source = readFileSync(pagesPath, "utf8");
  const match = source.match(
    /const PRODUCT_WIZARD_JS: &str = r#"\n([\s\S]*?)\n"#;/,
  );
  if (!match) throw new Error("Could not extract PRODUCT_WIZARD_JS from pages.rs");
  return match[1];
}

const wizardFixture = `<!doctype html>
<html>
  <body>
    <p id="product-wizard-error" hidden></p>
    <input type="radio" name="product_template" value="simple_product" checked>
    <input id="wizard-name" value="Shipped artwork">
    <input id="wizard-currency" value="NZD">
    <input id="wizard-price" value="40.00">
    <input id="wizard-minimum-total" value="35.00">
    <input id="wizard-maximum-total" value="50.00">
    <select id="wizard-tax-behavior"><option value="exclusive" selected>Exclusive</option></select>
    <select id="wizard-interval"><option value="month" selected>Month</option></select>
    <input id="wizard-interval-count" value="1">
    <div id="wizard-variables"></div>
    <div id="wizard-components"></div>
    <input id="wizard-promotions" type="checkbox">
    <input id="wizard-automatic-tax" type="checkbox" checked>
    <input id="wizard-billing-address" type="checkbox">
    <input id="wizard-shipping-address" type="checkbox">
    <input id="wizard-create-customer" type="checkbox" checked>
    <input id="wizard-terms" type="checkbox">
    <input id="wizard-trial-days" value="0">
    <section id="wizard-shipping-settings" hidden>
      <input id="wizard-shipping-countries" value="nz, AU">
      <textarea id="wizard-shipping-options">Standard | 5.00 | 3 | 5 | business_day |
Express | 15.00 | 1 | 2 | day | shr_express_123</textarea>
    </section>
  </body>
</html>`;

test.describe("products guided wizard", () => {
  test("serializes validated shipping policy with exact amounts in a browser", async ({
    page,
  }) => {
    await page.route(`${adminOrigin}/**`, async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "text/html",
        body: wizardFixture,
      });
    });
    await page.goto(`${adminOrigin}/b/products/admin/new`);
    await page.addScriptTag({ content: productWizardScript() });

    const settings = page.locator("#wizard-shipping-settings");
    await expect(settings).toBeHidden();
    await page.locator("#wizard-shipping-address").check();
    await page.evaluate(() => (window as any).productWizardShippingChanged());
    await expect(settings).toBeVisible();

    const offer = await page.evaluate(() =>
      (window as any).buildProductWizardOffer(),
    );
    expect(offer).toMatchObject({
      name: "Shipped artwork",
      mode: "payment",
      currency: "NZD",
      tax_behavior: "exclusive",
      components: [
        {
          key: "price",
          amount: { type: "fixed", unit_amount_minor: 4000 },
        },
      ],
      checkout: {
        minimum_total_minor: 3500,
        maximum_total_minor: 5000,
        automatic_tax: true,
        collect_shipping_address: true,
        allowed_shipping_countries: ["NZ", "AU"],
        create_customer: true,
        shipping_options: [
          {
            display_name: "Standard",
            amount_minor: 500,
            tax_behavior: "exclusive",
            stripe_shipping_rate_id: "",
            delivery_estimate: {
              minimum: 3,
              maximum: 5,
              unit: "business_day",
            },
          },
          {
            display_name: "Express",
            amount_minor: 1500,
            stripe_shipping_rate_id: "shr_express_123",
            delivery_estimate: { minimum: 1, maximum: 2, unit: "day" },
          },
        ],
      },
    });

    await page.locator("#wizard-shipping-countries").fill("NZ, nz");
    const error = await page.evaluate(() => {
      try {
        (window as any).buildProductWizardOffer();
        return null;
      } catch (cause) {
        return cause instanceof Error ? cause.message : String(cause);
      }
    });
    expect(error).toBe("Shipping countries must be unique.");

    await page.locator("#wizard-shipping-countries").fill("NZ");
    await page
      .locator("#wizard-shipping-options")
      .fill("Express | 15.00 | 5 | 2 | day | shr_express_123");
    const estimateError = await page.evaluate(() => {
      try {
        (window as any).buildProductWizardOffer();
        return null;
      } catch (cause) {
        return cause instanceof Error ? cause.message : String(cause);
      }
    });
    expect(estimateError).toBe(
      "Shipping estimate minimums must not exceed maximums.",
    );

    await page
      .locator("#wizard-shipping-options")
      .fill("Express | 15.00 | 1 | 2 | day | shr_express_123");
    await page.locator("#wizard-minimum-total").fill("50.01");
    const totalError = await page.evaluate(() => {
      try {
        (window as any).buildProductWizardOffer();
        return null;
      } catch (cause) {
        return cause instanceof Error ? cause.message : String(cause);
      }
    });
    expect(totalError).toBe(
      "Minimum item total must not exceed maximum item total.",
    );
  });

  test("offers native date controls and serializes typed booking fields", async ({
    page,
  }) => {
    await page.route(`${adminOrigin}/**`, async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "text/html",
        body: wizardFixture,
      });
    });
    await page.goto(`${adminOrigin}/b/products/admin/new`);
    await page.addScriptTag({ content: productWizardScript() });

    await page.evaluate(() => {
      (window as any).addWizardVariable({
        key: "booking_date",
        label: "Booking date",
        kind: "date",
        required: true,
        minimum: "2026-07-01",
        maximum: "2026-07-31",
      });
      (window as any).addWizardVariable({
        key: "arrival_time",
        label: "Arrival time",
        kind: "date_time",
        required: true,
        minimum: "2026-07-01T09:00",
        maximum: "2026-07-31T17:00",
      });
    });

    const rows = page.locator("[data-variable-row]");
    await expect(rows).toHaveCount(2);
    await expect(rows.nth(0).locator("[data-variable-min]")).toHaveAttribute(
      "type",
      "date",
    );
    await expect(rows.nth(1).locator("[data-variable-min]")).toHaveAttribute(
      "type",
      "datetime-local",
    );
    await rows.nth(0).locator("[data-variable-default]").fill("2026-07-20");
    await rows
      .nth(1)
      .locator("[data-variable-default]")
      .fill("2026-07-20T13:30");

    const variables = await page.evaluate(() =>
      (window as any).collectWizardVariables(),
    );
    expect(variables).toMatchObject([
      {
        key: "booking_date",
        kind: "date",
        minimum: "2026-07-01",
        maximum: "2026-07-31",
        default_value: "2026-07-20",
      },
      {
        key: "arrival_time",
        kind: "date_time",
        minimum: "2026-07-01T09:00",
        maximum: "2026-07-31T17:00",
        default_value: "2026-07-20T13:30",
      },
    ]);
  });
});
