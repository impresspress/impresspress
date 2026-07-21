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

function catalogScript() {
  const source = readFileSync(pagesPath, "utf8");
  const match = source.match(
    /const PRODUCT_CATALOG_ADMIN_JS: &str = r#"\n([\s\S]*?)\n"#;/,
  );
  if (!match) throw new Error("Could not extract PRODUCT_CATALOG_ADMIN_JS");
  return match[1];
}

function shell(body: string) {
  return `<!doctype html><html><head><meta charset="utf-8"></head><body>
    <p id="catalog-admin-error" role="alert" aria-live="assertive" hidden></p>
    ${body}
    <script>${catalogScript()}</script>
  </body></html>`;
}

function groupsHtml() {
  return shell(`
    <button type="button" onclick="productCatalogNew('group')">New group</button>
    <section id="group-editor" hidden>
      <h2 id="group-editor-title">New group</h2>
      <form onsubmit="productCatalogSaveGroup(event)">
        <input id="group-editor-id" type="hidden">
        <label for="group-editor-name">Name</label>
        <input id="group-editor-name" required maxlength="160">
        <label for="group-editor-description">Description</label>
        <textarea id="group-editor-description"></textarea>
        <label for="group-editor-status">Status</label>
        <select id="group-editor-status"><option value="active">Active</option><option value="archived">Archived</option></select>
        <button type="submit">Save group</button>
      </form>
    </section>
    <button type="button" data-record-id="group/1" data-record-name="Consulting" data-record-description="Service packages" data-record-status="active" onclick="productCatalogEditGroup(this)">Edit Consulting</button>
    <button type="button" data-record-id="group/1" data-record-name="Consulting" onclick="productCatalogDelete(this,'group')">Delete Consulting</button>
  `);
}

async function json(route: Route, body: unknown, status = 200) {
  await route.fulfill({
    status,
    contentType: "application/json",
    body: JSON.stringify(body),
  });
}

test.describe("products catalog admin lifecycle forms", () => {
  test("retains group values after a server error, then edits and deletes by encoded ID", async ({
    page,
  }) => {
    let createAttempts = 0;
    const requests: Array<{ method: string; path: string; body?: unknown }> = [];
    await page.route(`${adminOrigin}/**`, async (route) => {
      const request = route.request();
      const url = new URL(request.url());
      if (request.resourceType() === "document") {
        return route.fulfill({
          status: 200,
          contentType: "text/html; charset=utf-8",
          body: groupsHtml(),
        });
      }
      requests.push({
        method: request.method(),
        path: url.pathname,
        body: request.postData() ? request.postDataJSON() : undefined,
      });
      if (request.method() === "POST") {
        createAttempts += 1;
        if (createAttempts === 1) {
          return json(route, { message: "A group with this name already exists" }, 409);
        }
      }
      return json(route, { id: "group/1" });
    });

    await page.goto(`${adminOrigin}/b/products/admin/groups`);
    await page.getByRole("button", { name: "New group" }).click();
    await page.getByLabel("Name").fill("Consulting");
    await page.getByLabel("Description").fill("Service packages");
    await page.getByRole("button", { name: "Save group" }).click();
    await expect(page.getByRole("alert")).toHaveText(
      "A group with this name already exists",
    );
    await expect(page.getByLabel("Name")).toHaveValue("Consulting");
    await expect(page.getByLabel("Description")).toHaveValue(
      "Service packages",
    );

    await page.getByRole("button", { name: "Save group" }).click();
    await expect.poll(() => createAttempts).toBe(2);

    await page.getByRole("button", { name: "Edit Consulting" }).click();
    await expect(page.getByLabel("Name")).toHaveValue("Consulting");
    await page.getByLabel("Description").fill("Updated packages");
    await page.getByRole("button", { name: "Save group" }).click();
    await expect.poll(() => requests.some((request) => request.method === "PATCH")).toBe(true);

    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Delete Consulting" }).click();
    await expect.poll(() => requests.some((request) => request.method === "DELETE")).toBe(true);

    expect(requests).toContainEqual({
      method: "PATCH",
      path: "/b/products/api/admin/groups/group%2F1",
      body: {
        name: "Consulting",
        description: "Updated packages",
        status: "active",
      },
    });
    expect(requests).toContainEqual({
      method: "DELETE",
      path: "/b/products/api/admin/groups/group%2F1",
      body: undefined,
    });
  });
});
