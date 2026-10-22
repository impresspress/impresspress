import { expect, test, type Locator, type Page, type Route } from "@playwright/test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { confirmChrome } from "./fixtures/confirm";

/**
 * The products actions that cannot be undone ask first, in the shared confirm
 * dialog, in danger, driven from the keyboard alone:
 *
 * - an order's Create refund (`products-order-detail.js`, through
 *   `window.impresspressConfirm`), whose question states the amount it will
 *   refund: the one typed, or for a blank amount the full remaining balance
 *   with its figure;
 * - a deleted product's Archive offer and Deactivate (`hx-confirm` on the
 *   close page's htmx controls).
 *
 * Cancel, by Esc and by Enter on the focused Cancel, sends no request at all;
 * the confirm button sends exactly one.
 *
 * The markup is what the pages render: `fixtures/product-confirms.html`, kept
 * equal to the Rust pages by
 * `products::tests::page_chrome_tests::the_product_confirms_fixture_is_what_the_pages_render`
 * (the e2e server has no orders, and no Stripe to publish an offer through).
 * The scripts are the files the server serves: the products bundle, htmx and
 * chrome.js. Every request is answered here, so the spec sees each one.
 */

const adminOrigin = "https://admin.example";

const fixtures = readFileSync(
  fileURLToPath(new URL("./fixtures/product-confirms.html", import.meta.url)),
  "utf8",
);

/** One page's `main#content` contents from the fixture. */
function fixturePage(name: string): string {
  const parts = fixtures.split(/^<!-- page: (\S+) -->$/m);
  const at = parts.indexOf(name);
  if (at < 0) throw new Error(`no page ${name} in product-confirms.html`);
  return parts[at + 1];
}

const assets = new URL("../../../impresspress-core/src/", import.meta.url);
const asset = (path: string) => readFileSync(fileURLToPath(new URL(path, assets)), "utf8");
const htmxJs = asset("ui/assets/htmx.min.js");
const orderDetailJs = asset("blocks/products/assets/products-order-detail.js");

/** A served page around `body`: htmx, then the shared dialog and chrome.js. */
function servedPage(body: string, script = ""): string {
  return `<!doctype html><html><head><meta charset="utf-8"><title>Products</title></head><body>
<main class="shell__body" id="content" tabindex="0">${body}</main>
<script>${htmxJs}</script>
${confirmChrome}
${script ? `<script>${script}</script>` : ""}
</body></html>`;
}

/**
 * Serve `html` for every page under the admin origin and answer every other
 * request with `{}`, recording it as `METHOD path` with its JSON body.
 */
async function serve(page: Page, html: string): Promise<Array<{ call: string; body: unknown }>> {
  const requests: Array<{ call: string; body: unknown }> = [];
  await page.route(`${adminOrigin}/**`, async (route: Route) => {
    const request = route.request();
    if (request.resourceType() === "document") {
      return route.fulfill({ status: 200, contentType: "text/html", body: html });
    }
    const url = new URL(request.url());
    requests.push({
      call: `${request.method()} ${url.pathname}`,
      body: request.postData() ? request.postDataJSON() : undefined,
    });
    return route.fulfill({ status: 200, contentType: "application/json", body: "{}" });
  });
  return requests;
}

/** Tab forward from where focus is until `target` has it. */
async function tabTo(page: Page, target: Locator): Promise<void> {
  for (let i = 0; i < 60; i++) {
    if (await target.evaluate((el) => el === document.activeElement)) return;
    await page.keyboard.press("Tab");
  }
  throw new Error("Tab never reached the control");
}

/** The open confirm dialog, the shared one, asking `question` in danger. */
async function asking(page: Page, label: string, question: string): Promise<Locator> {
  const dialog = page.getByRole("alertdialog", { name: label, exact: true });
  await expect(dialog).toBeVisible();
  await expect(dialog).toHaveAttribute("id", "confirm-dialog");
  await expect(dialog.locator("#confirm-dialog-question")).toHaveText(question);
  await expect(dialog.getByRole("button", { name: "Cancel", exact: true })).toBeFocused();
  const shown = dialog.locator('[data-action="confirm-accept"]:visible');
  await expect(shown).toHaveCount(1);
  await expect(shown).toHaveText(label);
  await expect(shown).toHaveClass(/\bbtn--danger\b/);
  return dialog;
}

/**
 * From `control`, which has focus: Esc answers no, Enter on the focused
 * Cancel answers no, and neither sends anything; then the confirm button
 * answers yes.
 */
async function cancelTwiceThenConfirm(
  page: Page,
  control: Locator,
  label: string,
  question: string,
  requests: Array<{ call: string }>,
): Promise<void> {
  await page.keyboard.press("Enter");
  let dialog = await asking(page, label, question);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(control).toBeFocused();
  expect(requests).toEqual([]);

  await page.keyboard.press("Enter");
  dialog = await asking(page, label, question);
  await page.keyboard.press("Enter");
  await expect(dialog).toBeHidden();
  await expect(control).toBeFocused();
  expect(requests).toEqual([]);

  await page.keyboard.press("Enter");
  dialog = await asking(page, label, question);
  await page.keyboard.press("Tab");
  await expect(dialog.getByRole("button", { name: label, exact: true })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(dialog).toBeHidden();
}

test.describe("products actions that cannot be undone confirm first, keyboard only", () => {
  const ORDER = "ord_1_8f2a6c1e9b7d4e0f";
  const REFUND = `POST /b/products/api/admin/purchases/${ORDER}/refund`;

  test("Create refund with a blank amount states the full remaining balance", async ({ page }) => {
    const requests = await serve(page, servedPage(fixturePage("refund"), orderDetailJs));
    await page.goto(`${adminOrigin}/b/products/admin/purchases/${ORDER}`);
    const refund = page.getByRole("button", { name: "Create refund", exact: true });
    // Asks first, so the page's own button is not drawn as the danger one.
    await expect(refund).toHaveClass(/\bbtn--secondary\b/);
    await expect(page.getByLabel("Amount (NZD)")).toHaveValue("");

    await tabTo(page, refund);
    await cancelTwiceThenConfirm(
      page,
      refund,
      "Create refund",
      // 123,456,789 minor units ordered, 200,000 refunded already.
      "Refund the full remaining balance, 1232567.89 NZD, on this order? This can't be undone.",
      requests,
    );
    await expect.poll(() => requests.map((r) => r.call)).toEqual([REFUND]);
    // A blank amount sends none: the server refunds what remains.
    expect(requests[0].body).toEqual({ note: "", idempotency_key: "ui_200000_full" });
  });

  test("Create refund with an amount states that amount", async ({ page }) => {
    const requests = await serve(page, servedPage(fixturePage("refund"), orderDetailJs));
    await page.goto(`${adminOrigin}/b/products/admin/purchases/${ORDER}`);
    const amount = page.getByLabel("Amount (NZD)");
    const refund = page.getByRole("button", { name: "Create refund", exact: true });

    // An amount that does not parse is reported, and nothing is asked.
    await tabTo(page, amount);
    await page.keyboard.type("12.345");
    await tabTo(page, refund);
    await page.keyboard.press("Enter");
    await expect(page.locator("#order-detail-error")).toHaveText(
      "The amount has too many decimal places for this currency.",
    );
    await expect(page.getByRole("alertdialog")).toHaveCount(0);
    expect(requests).toEqual([]);

    await amount.focus();
    await page.keyboard.press("ControlOrMeta+A");
    await page.keyboard.type("12.5");
    await tabTo(page, refund);
    await cancelTwiceThenConfirm(
      page,
      refund,
      "Create refund",
      "Refund 12.50 NZD on this order? This can't be undone.",
      requests,
    );
    await expect.poll(() => requests.map((r) => r.call)).toEqual([REFUND]);
    expect(requests[0].body).toEqual({
      note: "",
      idempotency_key: "ui_200000_1250",
      amount_minor: 1250,
    });
  });

  test("Archive offer on a deleted product's close page", async ({ page }) => {
    const requests = await serve(page, servedPage(fixturePage("close")));
    await page.goto(`${adminOrigin}/b/products/admin/products/gone/close`);
    const archive = page.getByRole("button", { name: "Archive offer", exact: true });
    await tabTo(page, archive);
    await cancelTwiceThenConfirm(
      page,
      archive,
      "Archive offer",
      "Archive the offer “Download”? Its prices stop selling in Stripe, and an archived offer can't be published again.",
      requests,
    );
    await expect
      .poll(() => requests.map((r) => r.call))
      .toEqual(["DELETE /b/products/api/admin/products/gone/offers/offer_1"]);
  });

  test("Deactivate a payment link on a deleted product's close page", async ({ page }) => {
    const requests = await serve(page, servedPage(fixturePage("close")));
    await page.goto(`${adminOrigin}/b/products/admin/products/gone/close`);
    const deactivate = page.getByRole("button", { name: "Deactivate", exact: true });
    await tabTo(page, deactivate);
    await cancelTwiceThenConfirm(
      page,
      deactivate,
      "Deactivate link",
      "Deactivate this payment link? It stops taking payments, and a deactivated link can't be turned back on.",
      requests,
    );
    await expect
      .poll(() => requests.map((r) => r.call))
      .toEqual(["DELETE /b/products/api/admin/products/gone/offers/offer_1/payment-links/link_1"]);
  });
});
