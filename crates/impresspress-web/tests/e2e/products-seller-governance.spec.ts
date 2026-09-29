import { expect, test, type Route } from "@playwright/test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const bundlesUrl = new URL(
  "../../../impresspress-core/src/blocks/products/assets/",
  import.meta.url,
);

/**
 * A products browser bundle, read from the very file the server serves at
 * `/b/static/products-*.js`. These used to be Rust string constants dug out
 * of `pages.rs` with a regular expression; they are real files now, so the
 * spec reads the file — the same way `products-storefront.spec.ts` has always
 * read `storefront.js`.
 */
function bundle(name: string) {
  return readFileSync(fileURLToPath(new URL(name, bundlesUrl)), "utf8");
}

const adminOrigin = "https://admin.example";

function moderationHtml(pending: boolean) {
  return `<!doctype html>
<html><body>
  <p id="product-manager-error" role="alert" aria-live="assertive" hidden></p>
  <main>
    <h1>Seller print</h1>
    ${pending ? `
      <button data-moderation-action="approve" data-action="pm-moderate">Approve listing</button>
      <button data-moderation-action="reject" data-action="pm-moderate">Return to seller</button>
    ` : '<p>Approval: approved</p>'}
  </main>
  <script>window.__productManagerConfig={product_url:'/b/products/api/admin/products/product_1',detail_base_url:'/b/products/admin/products/'};</script>
  <script>${bundle("products-manager.js")}</script>
</body></html>`;
}

function sellerHtml(status: "active" | "suspended") {
  const action = status === "suspended" ? "reactivate" : "suspend";
  const label = status === "suspended" ? "Reactivate seller" : "Suspend seller";
  return `<!doctype html>
<html><body>
  <p id="seller-admin-error" role="alert" aria-live="assertive" hidden></p>
  <main>
    <h1>maker_1</h1><p>Seller status: ${status}</p>
    <button data-seller-action="${action}" data-action="psa-set-state">${label}</button>
  </main>
  <script>window.__sellerAdminConfig={action_url:'/b/products/api/admin/sellers/seller_1/${action}',action:'${action}'};</script>
  <script>${bundle("products-seller-admin.js")}</script>
</body></html>`;
}

async function json(route: Route, body: unknown, status = 200) {
  await route.fulfill({
    status,
    contentType: "application/json",
    body: JSON.stringify(body),
  });
}

test.describe("products seller governance", () => {
  test("restores failed moderation and reloads after approval", async ({ page }) => {
    let pending = true;
    let attempts = 0;
    const requests: string[] = [];
    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      const path = new URL(request.url()).pathname;
      if (request.resourceType() === "document") {
        return route.fulfill({
          status: 200,
          contentType: "text/html",
          body: moderationHtml(pending),
        });
      }
      requests.push(`${request.method()} ${path}`);
      if (
        request.method() === "POST" &&
        path === "/b/products/api/admin/products/product_1/approve"
      ) {
        attempts += 1;
        if (attempts === 1) {
          return json(
            route,
            { message: "seller Stripe account is not ready to accept charges" },
            409,
          );
        }
        pending = false;
        return json(route, { id: "product_1", status: "active" });
      }
      return json(route, { message: "Unexpected route" }, 404);
    });

    await page.goto(`${adminOrigin}/b/products/admin/products/product_1`);
    const approve = page.getByRole("button", { name: "Approve listing" });
    await approve.click();
    await expect(page.getByRole("alert")).toHaveText(
      "seller Stripe account is not ready to accept charges",
    );
    await expect(approve).toBeEnabled();
    await expect(approve).toHaveText("Approve listing");
    await approve.click();
    await expect(page.getByText("Approval: approved")).toBeVisible();
    expect(requests).toEqual([
      "POST /b/products/api/admin/products/product_1/approve",
      "POST /b/products/api/admin/products/product_1/approve",
    ]);
  });

  test("fails closed on suspension and supports retry plus reactivation", async ({
    page,
  }) => {
    let status: "active" | "suspended" = "active";
    let suspensionAttempts = 0;
    const requests: string[] = [];
    page.on("dialog", (dialog) => dialog.accept());
    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      const path = new URL(request.url()).pathname;
      if (request.resourceType() === "document") {
        return route.fulfill({
          status: 200,
          contentType: "text/html",
          body: sellerHtml(status),
        });
      }
      requests.push(`${request.method()} ${path}`);
      if (path.endsWith("/suspend")) {
        suspensionAttempts += 1;
        if (suspensionAttempts === 1) {
          return json(
            route,
            { message: "Stripe rejected catalog reconciliation" },
            409,
          );
        }
        status = "suspended";
        return json(route, { id: "seller_1", status });
      }
      if (path.endsWith("/reactivate")) {
        status = "active";
        return json(route, { id: "seller_1", status });
      }
      return json(route, { message: "Unexpected route" }, 404);
    });

    await page.goto(`${adminOrigin}/b/products/admin/sellers/seller_1`);
    const suspend = page.getByRole("button", { name: "Suspend seller" });
    await suspend.click();
    await expect(page.getByRole("alert")).toHaveText(
      "Stripe rejected catalog reconciliation",
    );
    await expect(suspend).toBeEnabled();
    await expect(suspend).toHaveText("Suspend seller");
    await suspend.click();
    const reactivate = page.getByRole("button", { name: "Reactivate seller" });
    await expect(reactivate).toBeVisible();
    await reactivate.click();
    await expect(page.getByRole("button", { name: "Suspend seller" })).toBeVisible();
    expect(requests).toEqual([
      "POST /b/products/api/admin/sellers/seller_1/suspend",
      "POST /b/products/api/admin/sellers/seller_1/suspend",
      "POST /b/products/api/admin/sellers/seller_1/reactivate",
    ]);
  });
});
