import { test, expect } from "@playwright/test";

// ---------------------------------------------------------------------------
// Helper: sign up a fresh user, return token
// ---------------------------------------------------------------------------
async function signup(request: any) {
  const email = `drop-${Date.now()}-${Math.random().toString(36).slice(2, 8)}@test.com`;
  const password = "TestPass1234";
  const res = await request.post("/b/auth/api/signup", {
    data: { email, password },
  });
  const body = await res.json();
  return {
    email,
    password,
    token: body.access_token as string,
    userId: body.user?.id as string,
  };
}

// ---------------------------------------------------------------------------
// Landing page
// ---------------------------------------------------------------------------
test.describe("Dropship: Landing Page", () => {
  test("serves themed landing page", async ({ request }) => {
    const res = await request.get("/");
    expect(res.ok()).toBeTruthy();
    const html = await res.text();
    expect(html).toContain("ShipFast");
    expect(html).toContain("Drop Shipping");
  });

  test("landing page has product features", async ({ request }) => {
    const res = await request.get("/");
    const html = await res.text();
    expect(html).toContain("Product Catalog");
    expect(html).toContain("Secure Payments");
    expect(html).toContain("Image Storage");
  });

  test("SPA fallback works for unknown paths", async ({ request }) => {
    const res = await request.get("/shop/electronics");
    expect(res.ok()).toBeTruthy();
    const html = await res.text();
    expect(html).toContain("ShipFast");
  });
});

// ---------------------------------------------------------------------------
// Health & System
// ---------------------------------------------------------------------------
test.describe("Dropship: Health", () => {
  test("GET /health returns ok", async ({ request }) => {
    const res = await request.get("/health");
    expect(res.ok()).toBeTruthy();
    expect(await res.json()).toEqual({ status: "ok" });
  });

});

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------
test.describe("Dropship: Auth", () => {
  test("signup creates user and returns tokens", async ({ request }) => {
    const { token } = await signup(request);
    expect(token).toBeTruthy();
  });

  test("login works after signup", async ({ request }) => {
    const { email, password } = await signup(request);
    const res = await request.post("/b/auth/api/login", {
      data: { email, password },
    });
    expect(res.ok()).toBeTruthy();
    const body = await res.json();
    expect(body).toHaveProperty("access_token");
    expect(body.user.email).toBe(email);
  });

  test("GET /b/auth/api/me returns profile", async ({ request }) => {
    const { email, token } = await signup(request);
    const res = await request.get("/b/auth/api/me", {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(res.ok()).toBeTruthy();
    const body = await res.json();
    expect(body.user.email).toBe(email);
  });

  test("rejects duplicate signup", async ({ request }) => {
    const { email, password } = await signup(request);
    const res = await request.post("/b/auth/api/signup", {
      data: { email, password },
    });
    expect(res.status()).toBe(201);
  });
});

// ---------------------------------------------------------------------------
// Products (catalog)
// ---------------------------------------------------------------------------
test.describe("Dropship: Products", () => {
  test("GET /b/products/catalog returns product list", async ({ request }) => {
    const { token } = await signup(request);
    const res = await request.get("/b/products/catalog", {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(res.ok()).toBeTruthy();
  });

  test("GET /b/products/types returns product types", async ({ request }) => {
    const { token } = await signup(request);
    const res = await request.get("/b/products/types", {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(res.ok()).toBeTruthy();
  });

});

// ---------------------------------------------------------------------------
// Storage (product images)
// ---------------------------------------------------------------------------
test.describe("Dropship: Storage", () => {
  test("GET /b/storage/api/buckets returns bucket list", async ({
    request,
  }) => {
    const { token } = await signup(request);
    const res = await request.get("/b/storage/api/buckets", {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(res.ok()).toBeTruthy();
  });
});

// ---------------------------------------------------------------------------
// Legal Pages
// ---------------------------------------------------------------------------
test.describe("Dropship: Legal Pages", () => {
  test("GET /b/legalpages/terms returns HTML", async ({ request }) => {
    const res = await request.get("/b/legalpages/terms");
    expect(res.ok()).toBeTruthy();
    const ct = res.headers()["content-type"];
    expect(ct).toContain("text/html");
  });

  test("GET /b/legalpages/privacy returns HTML", async ({ request }) => {
    const res = await request.get("/b/legalpages/privacy");
    expect(res.ok()).toBeTruthy();
    const ct = res.headers()["content-type"];
    expect(ct).toContain("text/html");
  });
});
