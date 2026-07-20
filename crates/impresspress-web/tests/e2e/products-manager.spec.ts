import { expect, test, type Route } from "@playwright/test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const pagesPath = fileURLToPath(
  new URL(
    "../../../impresspress-core/src/blocks/products/pages.rs",
    import.meta.url,
  ),
);
const adminOrigin = "https://admin.example";

function productManagerScript() {
  const source = readFileSync(pagesPath, "utf8");
  const match = source.match(
    /const PRODUCT_MANAGER_JS: &str = r#"\n([\s\S]*?)\n"#;/,
  );
  if (!match) throw new Error("Could not extract PRODUCT_MANAGER_JS from pages.rs");
  return match[1];
}

function productWizardScript() {
  const source = readFileSync(pagesPath, "utf8");
  const match = source.match(
    /const PRODUCT_WIZARD_JS: &str = r#"\n([\s\S]*?)\n"#;/,
  );
  if (!match) throw new Error("Could not extract PRODUCT_WIZARD_JS from pages.rs");
  return match[1];
}

function managerHtml(syncStatus: "failed" | "synced") {
  const label =
    syncStatus === "failed" ? "Retry Stripe sync" : "Reconcile Stripe";
  return `<!doctype html>
<html>
  <body>
    <p id="product-manager-error" role="alert" aria-live="assertive" hidden></p>
    <main>
      <section
        data-offer-card
        data-offer-url="/b/products/api/admin/products/product_1/offers/offer_1"
      >
        <h2>Quarterly care plan</h2>
        <p>Stripe sync: ${syncStatus}</p>
        <button type="button" onclick="productManagerOfferAction(this,'sync')">
          ${label}
        </button>
      </section>
    </main>
    <script>${productManagerScript()}</script>
    <script>initProductManager();</script>
  </body>
</html>`;
}

function configurableManagerHtml() {
  return `<!doctype html>
<html>
  <head><meta charset="utf-8"></head>
  <body>
    <p id="product-manager-error" role="alert" aria-live="assertive" hidden></p>
    <main>
      <section
        data-offer-card
        data-offer-id="offer_1"
        data-offer-url="/b/products/api/admin/products/product_1/offers/offer_1"
        data-preview-url="/b/products/api/admin/products/product_1/offers/offer_1/preview"
        data-presets-url="/b/products/api/admin/products/product_1/offers/offer_1/presets"
        data-links-url="/b/products/api/admin/products/product_1/offers/offer_1/payment-links"
        data-currency="NZD"
      >
        <p data-offer-error role="alert" aria-live="assertive" hidden></p>
        <h2>Team subscription</h2>
        <label for="preview-quantity">Checkout quantity</label>
        <input id="preview-quantity" data-preview-quantity type="number" min="1" step="1" value="2" required>
        <label for="preview-seats">Seats</label>
        <input id="preview-seats" data-offer-variable="preview" data-variable-key="seats" data-variable-kind="integer" type="number" min="1" step="1" value="3" required>
        <button type="button" onclick="productManagerPreview(this)">Calculate preview</button>
        <div data-pricing-preview aria-live="polite"></div>

        <label for="preset-name">Preset name</label>
        <input id="preset-name" data-preset-name value="Five seats">
        <label for="preset-slug">Preset slug</label>
        <input id="preset-slug" data-preset-slug pattern="[a-z0-9]+(?:-[a-z0-9]+)*">
        <label for="preset-seats">Seats</label>
        <input id="preset-seats" data-offer-variable="preset" data-variable-key="seats" data-variable-kind="integer" type="number" min="1" step="1" required>
        <label for="completion-url">After-completion URL (optional)</label>
        <input id="completion-url" data-link-completion-url type="url" value="https://shop.example/thanks">
        <button type="button" data-create-link onclick="productManagerCreateLink(this)">Create or reuse Payment Link</button>
        <div data-checkout-presets></div>
        <div data-payment-links></div>

        <div class="form-group">
          <textarea data-integration-snippet readonly>&lt;impresspress-product product-id="product_1" presentation="embedded"&gt;&lt;/impresspress-product&gt;</textarea>
          <button type="button" onclick="productManagerCopyField(this)">Copy embedded snippet</button>
        </div>
      </section>
    </main>
    <script>${productManagerScript()}</script>
    <script>initProductManager();</script>
  </body>
</html>`;
}

function visualDraftManagerHtml() {
  const definition = {
    name: "Team plan",
    mode: "subscription",
    currency: "NZD",
    pricing_model: "components",
    recurring_interval: "month",
    interval_count: 1,
    usage_type: "licensed",
    billing_scheme: "per_unit",
    tax_behavior: "exclusive",
    variables: [
      {
        key: "seats",
        kind: "integer",
        label: "Seats",
        help_text: "People with access",
        required: true,
        default_value: 2,
        minimum: "1",
        maximum: "100",
        step: "1",
        visibility: "public",
        sort_order: 0,
      },
      {
        key: "plan",
        kind: "select",
        label: "Plan",
        required: true,
        default_value: "basic",
        allowed_values: ["basic", "pro"],
        visibility: "public",
        sort_order: 1,
      },
    ],
    components: [
      {
        key: "base",
        label: "Base fee",
        description: "Workspace access",
        required: true,
        amount: { type: "fixed", unit_amount_minor: 1000 },
        quantity: { type: "fixed", value: 1 },
        condition: { op: "always" },
        recurrence: { interval: "month", interval_count: 1 },
        metadata: {},
      },
      {
        key: "seats",
        label: "Seats",
        required: true,
        amount: { type: "per_unit", input: "seats", unit_amount_minor: 250 },
        quantity: { type: "fixed", value: 1 },
        condition: { op: "greater_than", input: "seats", value: 1 },
        recurrence: { interval: "month", interval_count: 1 },
        metadata: {},
      },
      {
        key: "onboarding",
        label: "Onboarding",
        required: false,
        amount: { type: "fixed", unit_amount_minor: 5000 },
        quantity: {
          type: "from_input",
          input: "seats",
          minimum: 1,
          maximum: 10,
        },
        condition: {
          op: "any",
          conditions: [
            { op: "equals", input: "plan", value: "pro" },
            { op: "greater_than", input: "seats", value: 20 },
          ],
        },
        recurrence: { interval: "month", interval_count: 1 },
        metadata: { sku: "ONBOARD" },
      },
    ],
    checkout: { automatic_tax: true, maximum_total_minor: 100000 },
  };
  return `<!doctype html><html><head><meta charset="utf-8"></head><body>
    <p id="product-manager-error" role="alert" aria-live="assertive" hidden></p>
    <section data-offer-card data-offer-url="/b/products/api/admin/products/product_1/offers/offer_draft">
      <button type="button" onclick="productManagerOpenVisualEditor(this)">Edit visually</button>
      <textarea data-offer-definition>${JSON.stringify(definition)}</textarea>
    </section>
    <section id="product-manager-visual-editor" hidden>
      <h2 id="manager-visual-title">Edit pricing draft</h2>
      <label for="manager-visual-offer-name">Offer name</label><input id="manager-visual-offer-name">
      <label for="manager-visual-mode">Charge type</label><select id="manager-visual-mode" onchange="productManagerVisualModeChanged()"><option value="payment">Payment</option><option value="subscription">Subscription</option></select>
      <label for="manager-visual-currency">Currency</label><input id="manager-visual-currency">
      <div data-manager-recurring><label for="manager-visual-interval">Billing interval</label><select id="manager-visual-interval"><option value="month">Month</option><option value="year">Year</option></select></div>
      <div data-manager-recurring><label for="manager-visual-interval-count">Every</label><input id="manager-visual-interval-count" type="number"></div>
      <button type="button" onclick="addWizardVariable()">Add input</button>
      <div id="wizard-variables"></div>
      <button type="button" onclick="addWizardComponent()">Add row</button>
      <div id="wizard-components"></div>
      <button type="button" onclick="productManagerSaveVisualOffer(this)">Save visual changes</button>
    </section>
    <script>${productWizardScript()}</script>
    <script>${productManagerScript()}</script>
    <script>initProductManager();</script>
  </body></html>`;
}

async function json(route: Route, body: unknown, status = 200) {
  await route.fulfill({
    status,
    contentType: "application/json",
    body: JSON.stringify(body),
  });
}

test.describe("products manager Stripe catalog actions", () => {
  test("surfaces a failed sync, restores retry, and reloads into reconciliation", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 740 });
    let syncStatus: "failed" | "synced" = "failed";
    let syncAttempts = 0;
    const requests: Array<{ method: string; path: string }> = [];

    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      if (request.resourceType() === "document") {
        return route.fulfill({
          status: 200,
          contentType: "text/html",
          body: managerHtml(syncStatus),
        });
      }
      requests.push({ method: request.method(), path: url.pathname });
      if (
        request.method() === "POST" &&
        url.pathname ===
          "/b/products/api/admin/products/product_1/offers/offer_1/sync"
      ) {
        syncAttempts += 1;
        if (syncAttempts === 1) {
          return json(
            route,
            {
              message:
                "Stripe Price response did not match the active immutable offer row",
            },
            409,
          );
        }
        syncStatus = "synced";
        return json(route, { status: "active", sync_status: "synced" });
      }
      return json(route, { message: "Unexpected route" }, 404);
    });

    await page.goto(`${adminOrigin}/b/products/admin/products/product_1`);
    const action = page.getByRole("button", { name: "Retry Stripe sync" });
    await expect(action).toBeVisible();
    await action.click();
    await expect(page.getByRole("alert")).toHaveText(
      "Stripe Price response did not match the active immutable offer row",
    );
    await expect(action).toBeEnabled();
    await expect(action).toHaveText("Retry Stripe sync");

    await action.click();
    const reconcile = page.getByRole("button", { name: "Reconcile Stripe" });
    await expect(reconcile).toBeVisible();
    await reconcile.click();
    await expect(page.getByRole("button", { name: "Reconcile Stripe" })).toBeVisible();

    expect(requests).toEqual([
      {
        method: "POST",
        path: "/b/products/api/admin/products/product_1/offers/offer_1/sync",
      },
      {
        method: "POST",
        path: "/b/products/api/admin/products/product_1/offers/offer_1/sync",
      },
      {
        method: "POST",
        path: "/b/products/api/admin/products/product_1/offers/offer_1/sync",
      },
    ]);
  });

  test("previews itemized pricing and creates a typed reusable Payment Link", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 760 });
    let linkCreated = false;
    let presetActive = false;
    let presetName = "Five seats";
    let presetSlug = "";
    let presetSeats = 5;
    const bodies: Array<{ path: string; body: unknown }> = [];

    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      if (request.resourceType() === "document") {
        return route.fulfill({
          status: 200,
          contentType: "text/html",
          body: configurableManagerHtml(),
        });
      }
      const body = request.postDataJSON?.();
      if (body) bodies.push({ path: url.pathname, body });
      if (request.method() === "POST" && url.pathname.endsWith("/preview")) {
        return json(route, {
          offer_id: "offer_1",
          components: [
            {
              label: "Seats",
              included: true,
              total_amount_minor: 3600,
              reason: "included",
            },
            {
              label: "Onboarding",
              included: false,
              total_amount_minor: 0,
              reason: "condition did not match",
            },
          ],
          amounts: { currency: "NZD", total_minor: 3600 },
        });
      }
      if (request.method() === "POST" && url.pathname.endsWith("/presets")) {
        const requestBody = request.postDataJSON();
        presetActive = true;
        presetName = requestBody.name;
        presetSlug = requestBody.slug;
        presetSeats = requestBody.inputs.seats;
        return json(route, {
          id: "preset_1",
          name: presetName,
          slug: presetSlug,
          inputs: { seats: presetSeats },
          active: true,
        });
      }
      if (request.method() === "PATCH" && url.pathname.endsWith("/presets/preset_1")) {
        const requestBody = request.postDataJSON();
        presetName = requestBody.name;
        presetSlug = requestBody.slug;
        presetSeats = requestBody.inputs.seats;
        return json(route, {
          id: "preset_1",
          name: presetName,
          slug: presetSlug,
          inputs: { seats: presetSeats },
          active: true,
        });
      }
      if (request.method() === "DELETE" && url.pathname.endsWith("/presets/preset_1")) {
        presetActive = false;
        return json(route, { id: "preset_1", active: false });
      }
      if (request.method() === "GET" && url.pathname.endsWith("/presets")) {
        return json(route, {
          presets: presetName
            ? [
                {
                  id: "preset_1",
                  name: presetName,
                  slug: presetSlug,
                  inputs: { seats: presetSeats },
                  active: presetActive,
                },
              ]
            : [],
        });
      }
      if (request.method() === "POST" && url.pathname.endsWith("/payment-links")) {
        linkCreated = true;
        return json(route, { id: "link_1", active: true });
      }
      if (request.method() === "GET" && url.pathname.endsWith("/payment-links")) {
        return json(route, {
          payment_links: linkCreated
            ? [
                {
                  id: "link_1",
                  preset_id: "preset_1",
                  url: "https://buy.stripe.com/test_link",
                  active: true,
                  sync_status: "synced",
                },
              ]
            : [],
        });
      }
      return json(route, { message: "Unexpected route" }, 404);
    });

    await page.goto(`${adminOrigin}/b/products/admin/products/product_1`);
    await page.getByRole("button", { name: "Calculate preview" }).click();
    await expect(page.locator("[data-pricing-preview]")).toContainText("Seats");
    await expect(page.locator("[data-pricing-preview]")).toContainText(
      "Onboarding",
    );
    await expect(page.locator("[data-pricing-preview]")).toContainText(
      "not included",
    );
    await expect(page.locator("[data-pricing-preview]")).toContainText(
      "36.00 NZD",
    );

    await page
      .getByRole("button", { name: "Create or reuse Payment Link" })
      .click();
    await expect(page.locator("[data-offer-error]"))
      .toContainText("Seats is invalid");
    await expect(page.locator("#preset-seats")).toBeFocused();
    await expect(page.locator("#preset-name")).toHaveValue("Five seats");
    await expect(page.locator("#completion-url")).toHaveValue(
      "https://shop.example/thanks",
    );

    await page.locator("#preset-seats").fill("5");
    await page
      .getByRole("button", { name: "Create or reuse Payment Link" })
      .click();
    await expect(
      page.getByRole("link", { name: "Open hosted payment page" }),
    ).toHaveAttribute("href", "https://buy.stripe.com/test_link");
    await expect(
      page.getByRole("button", { name: "Copy", exact: true }),
    ).toBeVisible();
    await expect(page.locator("[data-integration-snippet]")).toContainText(
      'presentation="embedded"',
    );

    await page.getByRole("button", { name: "Edit preset" }).click();
    await expect(page.locator("#preset-seats")).toHaveValue("5");
    await page.locator("#preset-name").fill("Six seats");
    await page.locator("#preset-slug").fill("six-seats");
    await page.locator("#preset-seats").fill("6");
    await page
      .getByRole("button", { name: "Update preset and create/reuse link" })
      .click();
    await expect(page.locator("[data-checkout-presets]")).toContainText(
      "Six seats",
    );

    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Archive preset" }).click();
    await expect(page.locator("[data-checkout-presets]")).toContainText(
      "Archived",
    );

    expect(bodies).toEqual([
      {
        path: "/b/products/api/admin/products/product_1/offers/offer_1/preview",
        body: { offer_id: "offer_1", quantity: 2, inputs: { seats: 3 } },
      },
      {
        path: "/b/products/api/admin/products/product_1/offers/offer_1/presets",
        body: { name: "Five seats", slug: "", inputs: { seats: 5 } },
      },
      {
        path: "/b/products/api/admin/products/product_1/offers/offer_1/payment-links",
        body: {
          preset_id: "preset_1",
          after_completion_url: "https://shop.example/thanks",
        },
      },
      {
        path: "/b/products/api/admin/products/product_1/offers/offer_1/presets/preset_1",
        body: {
          name: "Six seats",
          slug: "six-seats",
          inputs: { seats: 6 },
        },
      },
      {
        path: "/b/products/api/admin/products/product_1/offers/offer_1/payment-links",
        body: {
          preset_id: "preset_1",
          after_completion_url: "https://shop.example/thanks",
        },
      },
    ]);
  });

  test("edits variables and itemized rows visually while preserving advanced rules", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 760 });
    let attempts = 0;
    const bodies: unknown[] = [];
    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      if (request.resourceType() === "document") {
        return route.fulfill({
          status: 200,
          contentType: "text/html; charset=utf-8",
          body: visualDraftManagerHtml(),
        });
      }
      if (request.method() === "PATCH") {
        attempts += 1;
        bodies.push(request.postDataJSON());
        if (attempts === 1) {
          return json(route, { message: "Maximum total is below this scenario" }, 409);
        }
        return json(route, { status: "draft" });
      }
      return json(route, { message: "Unexpected route" }, 404);
    });

    await page.goto(`${adminOrigin}/b/products/admin/products/product_1`);
    await page.getByRole("button", { name: "Edit visually" }).click();
    await expect(page.locator("#product-manager-visual-editor")).toBeVisible();
    await expect(page.locator("[data-variable-row]")).toHaveCount(2);
    await expect(page.locator("[data-component-row]")).toHaveCount(3);
    await expect(
      page.locator("[data-component-row]").nth(2).locator("[data-component-condition]"),
    ).toHaveValue("advanced_preserved");

    await page.locator("#manager-visual-offer-name").fill("Team plan 2026");
    await page
      .locator("[data-variable-row]")
      .nth(0)
      .locator("[data-variable-help]")
      .fill("Licensed team members");
    await page
      .locator("[data-component-row]")
      .nth(1)
      .locator("[data-component-amount]")
      .fill("3.00");
    await page.getByRole("button", { name: "Add input" }).click();
    const newInput = page.locator("[data-variable-row]").last();
    await newInput.locator("[data-variable-key]").fill("support");
    await newInput.locator("[data-variable-label]").fill("Priority support");
    await newInput.locator("[data-variable-kind]").selectOption("boolean");
    await newInput.locator("[data-variable-default]").fill("true");

    await page.getByRole("button", { name: "Save visual changes" }).click();
    await expect(page.getByRole("alert")).toHaveText(
      "Maximum total is below this scenario",
    );
    await expect(page.locator("#manager-visual-offer-name")).toHaveValue(
      "Team plan 2026",
    );
    await expect(newInput.locator("[data-variable-key]")).toHaveValue("support");

    await page.getByRole("button", { name: "Save visual changes" }).click();
    await expect.poll(() => attempts).toBe(2);

    const saved = bodies[1] as any;
    expect(saved.name).toBe("Team plan 2026");
    expect(saved.variables).toHaveLength(3);
    expect(saved.variables[0].help_text).toBe("Licensed team members");
    expect(saved.variables[2]).toMatchObject({
      key: "support",
      kind: "boolean",
      default_value: true,
    });
    expect(saved.components[1].amount).toEqual({
      type: "per_unit",
      input: "seats",
      unit_amount_minor: 300,
    });
    expect(saved.components[2].condition).toEqual({
      op: "any",
      conditions: [
        { op: "equals", input: "plan", value: "pro" },
        { op: "greater_than", input: "seats", value: 20 },
      ],
    });
    expect(saved.components[2].quantity).toEqual({
      type: "from_input",
      input: "seats",
      minimum: 1,
      maximum: 10,
    });
    expect(saved.components[2].metadata).toEqual({ sku: "ONBOARD" });
    expect(saved.checkout).toEqual({
      automatic_tax: true,
      maximum_total_minor: 100000,
    });
  });
});
