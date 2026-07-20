import { expect, test, type Page, type Route } from "@playwright/test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const pagesPath = fileURLToPath(
  new URL("../../../impresspress-core/src/blocks/products/pages.rs", import.meta.url),
);
const widgetPath = fileURLToPath(
  new URL(
    "../../../impresspress-core/src/blocks/products/assets/storefront.js",
    import.meta.url,
  ),
);
const pagesSource = readFileSync(pagesPath, "utf8");
const adminOrigin = "https://admin.example";
const shopOrigin = "https://shop.example";
const apiOrigin = "https://api.example";
const checkoutOrigin = "https://checkout.stripe.test";
const connectOrigin = "https://connect.stripe.test";
const billingOrigin = "https://billing.stripe.test";

function rustConst(name: string) {
  const match = pagesSource.match(
    new RegExp(`const ${name}: &str = r#"\\n([\\s\\S]*?)\\n"#;`),
  );
  if (!match) throw new Error(`Could not extract ${name} from pages.rs`);
  return match[1];
}

function rustFunction(name: string) {
  const match = pagesSource.match(
    new RegExp(
      `fn ${name}\\(\\) -> &'static str \\{\\s*r#"\\n([\\s\\S]*?)\\n"#\\s*\\}`,
    ),
  );
  if (!match) throw new Error(`Could not extract ${name} from pages.rs`);
  return match[1];
}

const productWizardScript = rustConst("PRODUCT_WIZARD_JS");
const orderDetailScript = rustConst("ORDER_DETAIL_JS");
const commercePortalScript = rustFunction("commerce_portal_js");

const styles = `
  :root{font-family:Inter,ui-sans-serif,system-ui,sans-serif;color:#182128;background:#f3f6f4}
  *{box-sizing:border-box}body{margin:0}header.app{background:#12372a;color:white;padding:18px 24px}
  header.app strong{font-size:20px}nav{display:flex;gap:16px;flex-wrap:wrap;margin-top:10px}nav a{color:#d8f6e8}
  main{width:min(980px,calc(100% - 32px));margin:28px auto}.card{background:white;border:1px solid #d7dfda;border-radius:14px;padding:20px;margin:14px 0;box-shadow:0 8px 24px #18352910}
  .grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(210px,1fr));gap:14px}.form-group{display:grid;gap:6px;margin:10px 0}
  input,select,textarea,button{font:inherit}input,select,textarea{width:100%;padding:10px;border:1px solid #8fa198;border-radius:8px;background:white}
  button,.btn{display:inline-block;border:0;border-radius:9px;padding:10px 15px;background:#176c4b;color:white;font-weight:700;text-decoration:none;cursor:pointer}
  button[hidden]{display:none}.secondary{background:#e6eee9;color:#183529}.danger{background:#a92d37}.badge{display:inline-block;border-radius:999px;padding:4px 9px;background:#e6eee9;font-size:13px}.badge-success{background:#d6f4e4;color:#145d40}.badge-warning{background:#fff0c7;color:#725000}
  .actions{display:flex;gap:10px;flex-wrap:wrap}.metric{padding:14px;border-radius:10px;background:#edf5f0}.metric strong{display:block;font-size:22px;margin-top:5px}
  [role=alert]{color:#9c2430}.text-muted{color:#5d6c65}.text-sm{font-size:14px}.sr-only{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0,0,0,0)}
  table{width:100%;border-collapse:collapse}th,td{text-align:left;padding:10px;border-bottom:1px solid #d7dfda}
  @media(max-width:520px){header.app{padding:15px}main{width:min(100% - 20px,980px);margin:14px auto}.card{padding:14px}.actions button,.actions .btn{width:100%}table,thead,tbody,tr,th,td{display:block}thead{display:none}td{padding:7px 0}}
`;

function shell(title: string, body: string, nav = "") {
  return `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>${title}</title><style>${styles}</style></head><body><header class="app"><strong>ImpressPress Commerce</strong><nav>${nav}</nav></header><main><h1>${title}</h1>${body}</main></body></html>`;
}

function wizardHtml(template: "simple_product" | "simple_subscription", seller = false) {
  const radio = (value: string, label: string) =>
    `<label><input type="radio" name="product_template" value="${value}" ${value === template ? "checked" : ""} onchange="productWizardTemplateChanged()"> ${label}</label>`;
  return shell(
    seller ? "Create seller product" : "Create product",
    `<nav aria-label="Product setup progress">${[1, 2, 3, 4, 5]
      .map((step) => `<span class="badge" data-wizard-indicator="${step}">${step}</span>`)
      .join("")}</nav>
    <form id="product-wizard-form" novalidate onsubmit="return false">
      <p id="product-wizard-error" role="alert" aria-live="assertive" hidden></p>
      <section class="card" data-wizard-step="1"><fieldset><legend>Choose a template</legend>${radio("simple_product", "Simple product")}${radio("simple_subscription", "Simple subscription")}</fieldset></section>
      <section class="card" data-wizard-step="2" hidden><div class="grid">
        <div class="form-group"><label for="wizard-name">Name</label><input id="wizard-name" required></div>
        <div class="form-group"><label for="wizard-slug">URL slug</label><input id="wizard-slug" pattern="[a-z0-9]+(?:-[a-z0-9]+)*"></div>
        <div class="form-group"><label for="wizard-image">Image URL</label><input id="wizard-image" type="url"></div>
        <div class="form-group"><label for="wizard-fulfillment">Fulfillment</label><select id="wizard-fulfillment"><option value="download">Digital download</option><option value="entitlement">Access entitlement</option></select></div>
      </div><div class="form-group"><label for="wizard-description">Description</label><textarea id="wizard-description"></textarea></div><div class="form-group"><label for="wizard-tags">Tags</label><input id="wizard-tags"></div></section>
      <section class="card" data-wizard-step="3" hidden><div class="grid">
        <div class="form-group"><label for="wizard-currency">Currency</label><input id="wizard-currency" value="NZD"></div>
        <div class="form-group" data-simple-pricing><label for="wizard-price">Price</label><input id="wizard-price" value="0.00"></div>
        <div class="form-group"><label for="wizard-tax-behavior">Tax behavior</label><select id="wizard-tax-behavior"><option value="exclusive">Tax exclusive</option></select></div>
        <div class="form-group"><label for="wizard-minimum-total">Minimum</label><input id="wizard-minimum-total"></div>
        <div class="form-group"><label for="wizard-maximum-total">Maximum</label><input id="wizard-maximum-total"></div>
        <div class="form-group" data-subscription-field><label for="wizard-interval">Interval</label><select id="wizard-interval"><option value="month">Monthly</option><option value="year">Yearly</option></select></div>
        <div class="form-group" data-subscription-field><label for="wizard-interval-count">Every</label><input id="wizard-interval-count" value="1" type="number"></div>
      </div><div id="wizard-advanced-pricing" hidden><div id="wizard-variables"></div><div id="wizard-components"></div></div></section>
      <section class="card" data-wizard-step="4" hidden><div class="grid">
        <label><input id="wizard-promotions" type="checkbox"> Allow promotions</label><label><input id="wizard-automatic-tax" type="checkbox"> Automatic tax</label>
        <label><input id="wizard-billing-address" type="checkbox"> Billing address</label><label><input id="wizard-shipping-address" type="checkbox" onchange="productWizardShippingChanged()"> Shipping address</label>
        <label><input id="wizard-create-customer" type="checkbox"> Create customer</label><label><input id="wizard-terms" type="checkbox"> Terms consent</label>
        <div data-subscription-field><label for="wizard-trial-days">Trial days</label><input id="wizard-trial-days" type="number" value="0"></div>
      </div><div id="wizard-shipping-settings" hidden><label for="wizard-shipping-countries">Countries</label><input id="wizard-shipping-countries" value="NZ"><label for="wizard-shipping-options">Rates</label><textarea id="wizard-shipping-options"></textarea></div></section>
      <section class="card" data-wizard-step="5" hidden><h2>Review</h2><div id="wizard-review" aria-live="polite"></div></section>
      <div class="actions"><button id="wizard-previous" type="button" onclick="productWizardPrevious()" hidden>Back</button><button id="wizard-next" type="button" onclick="productWizardNext()">Continue</button><button id="wizard-save-draft" type="button" onclick="submitProductWizard('draft')" hidden>Save draft</button><button id="wizard-publish" type="button" onclick="submitProductWizard('publish')" hidden>${seller ? "Submit for publication" : "Create and publish"}</button></div>
    </form><script>window.__productWizardConfig={admin:${!seller},product_collection:"/b/products/api/${seller ? "products" : "admin/products"}",return_url:"/b/products/${seller ? "my-products" : "admin/manage"}"};${productWizardScript};initProductWizard();</script>`,
    `<a href="/b/products/admin/manage">Products</a><a href="/b/products/admin/stripe">Stripe</a>`,
  );
}

function managedListHtml(name: string, status: string, seller = false) {
  return shell(
    seller ? "My products" : "Products",
    `<section class="card"><h2>${name}</h2><p><span class="badge ${status === "active" ? "badge-success" : "badge-warning"}">${status.replaceAll("_", " ")}</span></p><p class="text-muted">Pricing is synced from an immutable offer version.</p></section>`,
    seller
      ? `<a href="/b/products/">Overview</a><a href="/b/products/my-products">My products</a><a href="/b/products/selling">Sales</a>`
      : `<a href="/b/products/admin/manage">Products</a><a href="/b/products/admin/purchases">Orders</a>`,
  );
}

function orderHtml(options: {
  status: string;
  refunded?: boolean;
  subscriptionStatus?: string;
  buyer?: boolean;
}) {
  const access = options.buyer ? "Your order" : "Order order_1";
  const refund =
    !options.buyer && !options.refunded
      ? `<section class="card"><h2>Create refund</h2><div class="form-group"><label for="order-refund-amount">Amount (NZD)</label><input id="order-refund-amount"></div><div class="form-group"><label for="order-refund-note">Private note</label><textarea id="order-refund-note"></textarea></div><button class="danger" type="button" onclick="submitOrderRefund(this)">Create refund</button></section>`
      : options.refunded
        ? `<section class="card"><h2>Refunds</h2><table><thead><tr><th>Status</th><th>Amount</th></tr></thead><tbody><tr><td>successful</td><td>NZD 49.00</td></tr></tbody></table></section>`
        : "";
  const portal = options.subscriptionStatus
    ? `<section class="card"><h2>Subscription</h2><p>Status: <strong>${options.subscriptionStatus}</strong></p><button type="button" onclick="manageOrderBilling()">Manage billing in Stripe</button></section>`
    : "";
  return shell(
    access,
    `<p id="order-detail-error" role="alert" aria-live="assertive" hidden></p><section class="card"><div class="grid"><div class="metric">Payment<strong>${options.status}</strong></div><div class="metric">Total<strong>NZD 49.00</strong></div><div class="metric">Reconciliation<strong>reconciled</strong></div></div><h2>Items</h2><table><thead><tr><th>Item</th><th>Total</th></tr></thead><tbody><tr><td>${options.subscriptionStatus ? "Studio membership" : "Field guide PDF"}</td><td>NZD 49.00</td></tr></tbody></table></section>${portal}${refund}<script>window.__orderDetailConfig={order_id:"order_1",refund_url:"/b/products/api/admin/purchases/order_1/refund",currency_exponent:2,refunded_total:${options.refunded ? 4900 : 0}};${commercePortalScript};${orderDetailScript}</script>`,
    options.buyer
      ? `<a href="/b/products/">Commerce</a><a href="/b/products/my-purchases">Purchases</a>`
      : `<a href="/b/products/admin/purchases">Orders</a><a href="/b/products/admin/manage">Products</a>`,
  );
}

function storefrontProduct(subscription = false) {
  return {
    schema_version: 1,
    id: subscription ? "product_sub" : "product_1",
    name: subscription ? "Studio membership" : "Field guide PDF",
    slug: subscription ? "studio-membership" : "field-guide-pdf",
    description: subscription ? "Monthly member access." : "A downloadable field guide.",
    image_url: "",
    tags: [subscription ? "membership" : "download"],
    fulfillment_kind: subscription ? "entitlement" : "download",
    offers: [
      {
        id: subscription ? "offer_sub" : "offer_1",
        version: 1,
        name: subscription ? "Monthly membership" : "Download",
        mode: subscription ? "subscription" : "payment",
        currency: "NZD",
        pricing_model: "fixed",
        recurring_interval: subscription ? "month" : null,
        interval_count: 1,
        variables: [],
        checkout: {
          allow_promotion_codes: false,
          automatic_tax: false,
          collect_billing_address: false,
          collect_shipping_address: false,
          require_terms_consent: false,
          trial_days: subscription ? 7 : 0,
        },
        payment_links: [],
      },
    ],
  };
}

function pricing(offerId: string) {
  return {
    schema_version: 1,
    offer_id: offerId,
    offer_version: 1,
    quantity: 1,
    inputs: {},
    components: [
      {
        component_id: "component_price",
        key: "price",
        label: "Price",
        included: true,
        required: true,
        unit_amount_minor: 4900,
        quantity: 1,
        total_amount_minor: 4900,
        reason: "required",
      },
    ],
    amounts: {
      currency: "NZD",
      subtotal_minor: 4900,
      discount_minor: 0,
      tax_minor: 0,
      shipping_minor: 0,
      platform_fee_minor: 0,
      total_minor: 4900,
    },
  };
}

async function json(route: Route, body: unknown, status = 200) {
  await route.fulfill({
    status,
    contentType: "application/json",
    headers: { "Access-Control-Allow-Origin": "*" },
    body: JSON.stringify(body),
  });
}

async function mountStorefront(page: Page, subscription = false, expectedTitle?: string) {
  await page.goto(`${shopOrigin}/${subscription ? "membership" : "download"}`);
  await page.addScriptTag({ path: widgetPath });
  await page.evaluate(
    ({ apiOrigin, productId }) => {
      const widget = document.createElement("impresspress-product");
      widget.setAttribute("api-base", apiOrigin);
      widget.setAttribute("product-id", productId);
      widget.setAttribute("presentation", "hosted");
      document.querySelector("main")!.appendChild(widget);
    },
    { apiOrigin, productId: subscription ? "product_sub" : "product_1" },
  );
  const widget = page.locator("impresspress-product");
  await expect(widget.locator(".title")).toHaveText(
    expectedTitle ?? (subscription ? "Studio membership" : "Field guide PDF"),
  );
  return widget;
}

async function completeWizard(page: Page, name: string, price: string, trialDays = "0") {
  const next = page.getByRole("button", { name: "Continue" });
  await next.focus();
  await page.keyboard.press("Enter");
  await next.click();
  await expect(page.getByRole("alert")).toHaveText("Product name is required.");
  await expect(page.getByLabel("Name")).toBeFocused();
  await page.getByLabel("Name").fill(name);
  await page.getByLabel("Description").fill(`Useful ${name.toLowerCase()}.`);
  await next.click();
  await page.getByLabel("Price").fill(price);
  await next.click();
  if (trialDays !== "0") await page.getByLabel("Trial days").fill(trialDays);
  await next.click();
  await expect(page.locator("#wizard-review")).toContainText(name);
}

test.describe("products complete browser lifecycles", () => {
  test("one-time product: create, publish, static checkout, completion, order and refund", async ({
    page,
  }) => {
    const calls: Array<{ method: string; path: string; body: any }> = [];
    let refunded = false;
    let paid = false;
    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      if (request.resourceType() === "document") {
        if (url.pathname.endsWith("/new")) {
          return route.fulfill({ status: 200, contentType: "text/html", body: wizardHtml("simple_product") });
        }
        if (url.pathname.includes("/purchases/order_1")) {
          return route.fulfill({ status: 200, contentType: "text/html", body: orderHtml({ status: refunded ? "refunded" : paid ? "completed" : "pending", refunded }) });
        }
        return route.fulfill({ status: 200, contentType: "text/html", body: managedListHtml("Field guide PDF", "active") });
      }
      const body = request.postData() ? request.postDataJSON() : undefined;
      calls.push({ method: request.method(), path: url.pathname, body });
      if (request.method() === "POST" && url.pathname.endsWith("/admin/products")) return json(route, { id: "product_1" });
      if (request.method() === "POST" && url.pathname.endsWith("/product_1/offers")) return json(route, { offer: { id: "offer_1" } });
      if (request.method() === "POST" && url.pathname.endsWith("/offer_1/publish")) return json(route, { offer: { id: "offer_1", status: "active" } });
      if (request.method() === "PATCH" && url.pathname.endsWith("/product_1")) return json(route, { id: "product_1", status: "active" });
      if (request.method() === "POST" && url.pathname.endsWith("/refund")) {
        refunded = true;
        return json(route, { id: "refund_1", status: "succeeded" });
      }
      return json(route, { message: "Unexpected admin route" }, 404);
    });
    await page.route(`${shopOrigin}/**`, (route) =>
      route.fulfill({ status: 200, contentType: "text/html", body: shell("Field Notes Press", "<section class=card><main></main></section>") }),
    );
    await page.route(`${apiOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      const body = request.postData() ? request.postDataJSON() : undefined;
      calls.push({ method: request.method(), path: url.pathname, body });
      if (url.pathname.endsWith("/storefront/product_1")) return json(route, storefrontProduct());
      if (url.pathname.endsWith("/pricing/preview")) return json(route, pricing("offer_1"));
      if (url.pathname.endsWith("/checkout")) return json(route, { order_id: "order_1", receipt_token: "receipt_1", receipt_token_expires_at: "2026-07-27T00:00:00Z", presentation: "hosted", checkout_url: `${checkoutOrigin}/session/order_1`, client_secret: null, payment_link_url: null, amounts: pricing("offer_1").amounts });
      if (url.pathname.endsWith("/webhooks")) {
        paid = true;
        return json(route, { received: true });
      }
      return json(route, { message: "Unexpected commerce route" }, 404);
    });
    await page.route(`${checkoutOrigin}/**`, (route) =>
      route.fulfill({ status: 200, contentType: "text/html", body: shell("Stripe test Checkout", `<section class="card"><p>Test mode · NZD 49.00</p><button id="pay" type="button">Complete payment</button></section><script>document.getElementById('pay').onclick=async()=>{await fetch('${apiOrigin}/b/products/webhooks',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({type:'checkout.session.completed',id:'evt_checkout_1'})});location.assign('${adminOrigin}/b/products/admin/purchases/order_1')}</script>`) }),
    );

    await page.goto(`${adminOrigin}/b/products/admin/new`);
    await completeWizard(page, "Field guide PDF", "49.00");
    await page.getByRole("button", { name: "Create and publish" }).click();
    await expect(page).toHaveURL(/\/b\/products\/admin\/manage\?created=product_1&published=1$/);
    expect(calls.slice(0, 4).map(({ method, path }) => `${method} ${path}`)).toEqual([
      "POST /b/products/api/admin/products",
      "POST /b/products/api/admin/products/product_1/offers",
      "POST /b/products/api/admin/products/product_1/offers/offer_1/publish",
      "PATCH /b/products/api/admin/products/product_1",
    ]);
    expect(calls[0].body).toMatchObject({ name: "Field guide PDF", product_template_id: "simple_product", fulfillment_kind: "download" });
    expect(calls[1].body.components[0].amount.unit_amount_minor).toBe(4900);

    const widget = await mountStorefront(page);
    await widget.getByLabel("Email").fill("buyer@example.com");
    await widget.getByRole("button", { name: "Continue to secure checkout" }).click();
    await expect(page).toHaveURL(`${checkoutOrigin}/session/order_1`);
    await page.getByRole("button", { name: "Complete payment" }).click();
    await expect(page).toHaveURL(`${adminOrigin}/b/products/admin/purchases/order_1`);
    await expect(page.getByText("completed", { exact: true })).toBeVisible();
    await page.getByLabel("Amount (NZD)").fill("49.00");
    await page.getByLabel("Private note").fill("Customer requested cancellation");
    await page.getByRole("button", { name: "Create refund" }).click();
    await expect(page.getByText("refunded", { exact: true })).toBeVisible();
    const refundCall = calls.find((call) => call.path.endsWith("/refund"));
    expect(refundCall?.body).toEqual({ amount_minor: 4900, note: "Customer requested cancellation", idempotency_key: "ui_0_4900" });
    await expect(page).toHaveScreenshot("products-admin-refunded-order-desktop.png", { fullPage: true });
    await page.setViewportSize({ width: 375, height: 812 });
    await page.reload();
    await expect(page).toHaveScreenshot("products-admin-refunded-order-mobile.png", { fullPage: true });
  });

  test("subscription: create, checkout, out-of-order recovery and Billing Portal", async ({ page }) => {
    const calls: Array<{ method: string; path: string; body: any }> = [];
    let subscriptionStatus = "pending";
    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      if (request.resourceType() === "document") {
        if (url.pathname.endsWith("/new")) return route.fulfill({ status: 200, contentType: "text/html", body: wizardHtml("simple_subscription") });
        if (url.pathname.includes("/purchases/order_1")) return route.fulfill({ status: 200, contentType: "text/html", body: orderHtml({ status: subscriptionStatus === "pending" ? "pending" : "completed", subscriptionStatus, buyer: true }) });
        return route.fulfill({ status: 200, contentType: "text/html", body: managedListHtml("Studio membership", "active") });
      }
      const body = request.postData() ? request.postDataJSON() : undefined;
      calls.push({ method: request.method(), path: url.pathname, body });
      if (request.method() === "POST" && url.pathname.endsWith("/admin/products")) return json(route, { id: "product_sub" });
      if (request.method() === "POST" && url.pathname.endsWith("/product_sub/offers")) return json(route, { offer: { id: "offer_sub" } });
      if (url.pathname.endsWith("/offer_sub/publish")) return json(route, { offer: { id: "offer_sub", status: "active" } });
      if (request.method() === "PATCH" && url.pathname.endsWith("/product_sub")) return json(route, { id: "product_sub", status: "active" });
      if (url.pathname.endsWith("/billing-portal")) return json(route, { url: `${billingOrigin}/session/sub_1` });
      return json(route, { message: "Unexpected admin route" }, 404);
    });
    await page.route(`${shopOrigin}/**`, (route) => route.fulfill({ status: 200, contentType: "text/html", body: shell("Common Ground", "<main></main>") }));
    await page.route(`${apiOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      const body = request.postData() ? request.postDataJSON() : undefined;
      calls.push({ method: request.method(), path: url.pathname, body });
      if (url.pathname.endsWith("/storefront/product_sub")) return json(route, storefrontProduct(true));
      if (url.pathname.endsWith("/pricing/preview")) return json(route, pricing("offer_sub"));
      if (url.pathname.endsWith("/checkout")) return json(route, { order_id: "order_1", receipt_token: "receipt_sub", receipt_token_expires_at: "2026-07-27T00:00:00Z", presentation: "hosted", checkout_url: `${checkoutOrigin}/session/sub_1`, client_secret: null, payment_link_url: null, amounts: pricing("offer_sub").amounts });
      if (url.pathname.endsWith("/webhooks")) {
        if (body.type === "invoice.payment_failed") subscriptionStatus = "past_due";
        else subscriptionStatus = "active";
        return json(route, { received: true });
      }
      return json(route, { message: "Unexpected commerce route" }, 404);
    });
    await page.route(`${checkoutOrigin}/**`, (route) => route.fulfill({ status: 200, contentType: "text/html", body: shell("Stripe subscription Checkout", `<section class="card"><p>7 day trial · then NZD 49.00 monthly</p><button type="button" id="subscribe">Start membership</button></section><script>subscribe.onclick=async()=>{await fetch('${apiOrigin}/b/products/webhooks',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({type:'customer.subscription.updated'})});location.assign('${adminOrigin}/b/products/admin/purchases/order_1')}</script>`) }));
    await page.route(`${billingOrigin}/**`, (route) => route.fulfill({ status: 200, contentType: "text/html", body: shell("Stripe Billing Portal", "<section class=card><h2>Studio membership</h2><p>Payment methods, invoices and cancellation are managed here.</p></section>") }));

    await page.goto(`${adminOrigin}/b/products/admin/new`);
    await completeWizard(page, "Studio membership", "49.00", "7");
    await page.getByRole("button", { name: "Create and publish" }).click();
    await expect(page).toHaveURL(
      /\/b\/products\/admin\/manage\?created=product_sub&published=1$/,
    );
    expect(calls[1].body).toMatchObject({ mode: "subscription", recurring_interval: "month", interval_count: 1, checkout: { trial_days: 7 } });

    const widget = await mountStorefront(page, true);
    await widget.getByLabel("Email").fill("member@example.com");
    await widget.getByRole("button", { name: "Continue to secure checkout" }).click();
    await page.getByRole("button", { name: "Start membership" }).click();
    await expect(page.getByText("active", { exact: true })).toBeVisible();
    await page.evaluate((api) => fetch(`${api}/b/products/webhooks`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ type: "invoice.payment_failed" }) }), apiOrigin);
    await page.reload();
    await expect(page.getByText("past_due", { exact: true })).toBeVisible();
    await page.evaluate((api) => fetch(`${api}/b/products/webhooks`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ type: "invoice.paid" }) }), apiOrigin);
    await page.reload();
    await expect(page.getByText("active", { exact: true })).toBeVisible();
    await expect(page).toHaveScreenshot("products-buyer-subscription-desktop.png", { fullPage: true });
    await page.setViewportSize({ width: 375, height: 812 });
    await page.reload();
    await expect(page).toHaveScreenshot("products-buyer-subscription-mobile.png", { fullPage: true });
    await page.getByRole("button", { name: "Manage billing in Stripe" }).click();
    await expect(page).toHaveURL(`${billingOrigin}/session/sub_1`);
    const portalCall = calls.find((call) => call.path.endsWith("/billing-portal"));
    expect(portalCall?.body).toMatchObject({ order_id: "order_1" });
  });

  test("seller gate, Connect onboarding, moderation, stats and ownership isolation", async ({
    page,
  }) => {
    let sellingEnabled = false;
    let connected = false;
    let productStatus = "none";
    const calls: Array<{ method: string; path: string; body: any }> = [];
    const portal = () =>
      shell(
        "Commerce",
        `<p id="commerce-portal-error" role="alert" hidden></p><section class="card"><h2>Purchases</h2><p>Your receipts and subscriptions live here.</p></section>${
          sellingEnabled
            ? `<section class="card" aria-label="Seller account"><h2>Stripe seller account</h2><p><span class="badge ${connected ? "badge-success" : "badge-warning"}">${connected ? "Ready to sell" : "Not connected"}</span></p>${connected ? `<a class="btn" href="/b/products/my-products/new">Create listing</a>` : `<button type="button" onclick="startSellerOnboarding()">Connect Stripe to sell</button>`}</section>${connected ? `<section class="card"><h2>Sales snapshot</h2><div class="grid"><div class="metric">Gross sales<strong>NZD 1,240.00</strong></div><div class="metric">Platform fees<strong>NZD 62.00</strong></div><div class="metric">Before Stripe fees<strong>NZD 1,178.00</strong></div></div><p>Exact payouts and provider fees are available in Stripe.</p>${productStatus !== "none" ? `<p>Listing: <span class="badge ${productStatus === "active" ? "badge-success" : "badge-warning"}">${productStatus.replaceAll("_", " ")}</span></p>` : ""}</section>` : ""}`
            : `<section class="card"><h2>Selling is disabled</h2><p>An administrator must enable user products before seller tools appear.</p></section>`
        }<script>${commercePortalScript}</script>`,
        sellingEnabled
          ? `<a href="/b/products/">Overview</a><a href="/b/products/my-products">Products</a><a href="/b/products/selling">Sales</a>`
          : `<a href="/b/products/">Overview</a><a href="/b/products/my-purchases">Purchases</a>`,
      );
    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      if (request.resourceType() === "document") {
        if (url.pathname.endsWith("/my-products/new")) return route.fulfill({ status: 200, contentType: "text/html", body: wizardHtml("simple_product", true) });
        if (url.pathname.endsWith("/my-products")) return route.fulfill({ status: 200, contentType: "text/html", body: managedListHtml("Seller field kit", productStatus, true) });
        return route.fulfill({ status: 200, contentType: "text/html", body: portal() });
      }
      const body = request.postData() ? request.postDataJSON() : undefined;
      calls.push({ method: request.method(), path: url.pathname, body });
      if (!sellingEnabled && url.pathname.includes("/api/products")) return json(route, { message: "User product selling is disabled" }, 403);
      if (url.pathname.endsWith("/api/seller/onboarding")) {
        connected = true;
        return json(route, { url: `${connectOrigin}/onboarding/acct_seller_1`, expires_at: 1900000000, account: { status: "pending" } });
      }
      if (request.method() === "POST" && url.pathname.endsWith("/api/products")) return json(route, { id: "seller_product_1" });
      if (request.method() === "POST" && url.pathname.endsWith("/seller_product_1/offers")) return json(route, { offer: { id: "seller_offer_1" } });
      if (url.pathname.endsWith("/seller_offer_1/publish")) return json(route, { offer: { id: "seller_offer_1", status: "active" } });
      if (request.method() === "PATCH" && url.pathname.endsWith("/seller_product_1")) {
        productStatus = "pending_review";
        return json(route, { id: "seller_product_1", status: productStatus });
      }
      if (url.pathname.endsWith("/approve")) {
        productStatus = "active";
        return json(route, { id: "seller_product_1", status: productStatus });
      }
      if (url.pathname.endsWith("/seller/orders/order_other")) return json(route, { message: "Order not found" }, 404);
      return json(route, { message: "Unexpected seller route" }, 404);
    });
    await page.route(`${connectOrigin}/**`, (route) => route.fulfill({ status: 200, contentType: "text/html", body: shell("Stripe Connect test onboarding", `<section class="card"><p>Identity and payout details are hosted by Stripe.</p><a class="btn" href="${adminOrigin}/b/products/">Return to ImpressPress</a></section>`) }));

    await page.goto(`${adminOrigin}/b/products/`);
    await expect(page.getByRole("heading", { name: "Selling is disabled" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Sales" })).toHaveCount(0);
    const denied = await page.evaluate(async () => {
      const response = await fetch("/b/products/api/products", { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
      return { status: response.status, body: await response.json() };
    });
    expect(denied).toMatchObject({ status: 403, body: { message: "User product selling is disabled" } });

    sellingEnabled = true;
    await page.reload();
    await expect(page.getByRole("link", { name: "Sales" })).toBeVisible();
    await page.getByRole("button", { name: "Connect Stripe to sell" }).click();
    await expect(page).toHaveURL(`${connectOrigin}/onboarding/acct_seller_1`);
    await page.getByRole("link", { name: "Return to ImpressPress" }).click();
    await expect(page.getByText("Ready to sell")).toBeVisible();
    await page.getByRole("link", { name: "Create listing" }).click();
    await completeWizard(page, "Seller field kit", "80.00");
    await page.getByRole("button", { name: "Submit for publication" }).click();
    await expect(page.getByText("pending review")).toBeVisible();
    await page.evaluate(() =>
      fetch("/b/products/api/admin/products/seller_product_1/approve", {
        method: "POST",
      }).then((response) => response.status),
    );
    await page.goto(`${adminOrigin}/b/products/`);
    await expect(page.getByText("active", { exact: true })).toBeVisible();
    await expect(page.getByText("Before Stripe fees")).toBeVisible();
    const isolation = await page.evaluate(async () => {
      const response = await fetch("/b/products/api/seller/orders/order_other");
      return { status: response.status, text: await response.text() };
    });
    expect(isolation.status).toBe(404);
    expect(isolation.text).not.toContain("other-seller-private-data");
    expect(
      calls.find(
        (call) =>
          call.path.endsWith("/api/products") &&
          call.body?.product_template_id === "simple_product",
      )?.body,
    ).toMatchObject({ product_template_id: "simple_product" });
    await expect(page).toHaveScreenshot("products-seller-ready-desktop.png", { fullPage: true });
    await page.setViewportSize({ width: 375, height: 812 });
    await page.reload();
    await expect(page).toHaveScreenshot("products-seller-ready-mobile.png", { fullPage: true });
  });

  test("booking storefront uses native bounded date and date-time controls", async ({ page }) => {
    const previews: any[] = [];
    const product = storefrontProduct();
    product.name = "Guided field session";
    product.description = "Choose a date and arrival time for your guided outdoor session.";
    product.offers[0].variables = [
      {
        key: "booking_date",
        kind: "date",
        label: "Booking date",
        help_text: "Choose the day you want to arrive.",
        required: true,
        default_value: "2026-07-20",
        allowed_values: [],
        minimum: "2026-07-01",
        maximum: "2026-07-31",
        sort_order: 0,
      },
      {
        key: "arrival_time",
        kind: "date_time",
        label: "Arrival time",
        help_text: "Times use the venue's local timezone.",
        required: true,
        default_value: "2026-07-20T13:30",
        allowed_values: [],
        minimum: "2026-07-20T09:00",
        maximum: "2026-07-31T17:00",
        sort_order: 1,
      },
    ];

    await page.route(`${shopOrigin}/**`, (route) =>
      route.fulfill({
        status: 200,
        contentType: "text/html",
        body: shell("Book a field session", "<main></main>"),
      }),
    );
    await page.route(`${apiOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      if (url.pathname.endsWith("/storefront/product_1")) return json(route, product);
      if (url.pathname.endsWith("/pricing/preview")) {
        previews.push(request.postDataJSON());
        return json(route, pricing("offer_1"));
      }
      return json(route, { message: "Unexpected booking route" }, 404);
    });

    const widget = await mountStorefront(page, false, "Guided field session");
    const bookingDate = widget.getByLabel("Booking date");
    const arrivalTime = widget.getByLabel("Arrival time");
    await expect(bookingDate).toHaveAttribute("type", "date");
    await expect(bookingDate).toHaveAttribute("min", "2026-07-01");
    await expect(bookingDate).toHaveAttribute("max", "2026-07-31");
    await expect(arrivalTime).toHaveAttribute("type", "datetime-local");
    await bookingDate.fill("2026-07-22");
    await arrivalTime.fill("2026-07-22T14:45");
    await expect
      .poll(() =>
        previews.some(
          (preview) =>
            preview.inputs?.booking_date === "2026-07-22" &&
            preview.inputs?.arrival_time === "2026-07-22T14:45",
        ),
      )
      .toBe(true);
    await page.getByRole("heading", { name: "Book a field session" }).click();

    await expect(page).toHaveScreenshot("products-booking-fields-desktop.png", {
      fullPage: true,
    });
    await page.setViewportSize({ width: 375, height: 812 });
    await expect(page).toHaveScreenshot("products-booking-fields-mobile.png", {
      fullPage: true,
    });
  });
});
