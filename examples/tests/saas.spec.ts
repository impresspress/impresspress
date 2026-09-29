import { test, expect } from "@playwright/test";

// ---------------------------------------------------------------------------
// Helper: sign up a fresh user, return token
// ---------------------------------------------------------------------------
async function signup(request: any) {
  const email = `saas-${Date.now()}-${Math.random().toString(36).slice(2, 8)}@test.com`;
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
    roles: body.user?.roles as string[],
  };
}

// ---------------------------------------------------------------------------
// Landing page
// ---------------------------------------------------------------------------
test.describe("SaaS: Landing Page", () => {
  test("serves themed landing page", async ({ request }) => {
    const res = await request.get("/");
    expect(res.ok()).toBeTruthy();
    const html = await res.text();
    expect(html).toContain("CloudDeploy");
    expect(html).toContain("SaaS Platform");
  });

  test("landing page has pricing section", async ({ request }) => {
    const res = await request.get("/");
    const html = await res.text();
    expect(html).toContain("Simple Pricing");
    expect(html).toContain("Starter");
    expect(html).toContain("Pro");
    expect(html).toContain("Enterprise");
  });

  test("landing page has platform features", async ({ request }) => {
    const res = await request.get("/");
    const html = await res.text();
    expect(html).toContain("One-Click Deploy");
    expect(html).toContain("Subscription Plans");
    expect(html).toContain("Team Management");
  });

  test("SPA fallback works", async ({ request }) => {
    const res = await request.get("/dashboard/overview");
    expect(res.ok()).toBeTruthy();
    const html = await res.text();
    expect(html).toContain("CloudDeploy");
  });
});

// ---------------------------------------------------------------------------
// Health & System
// ---------------------------------------------------------------------------
test.describe("SaaS: Health", () => {
  test("GET /health returns ok", async ({ request }) => {
    const res = await request.get("/health");
    expect(res.ok()).toBeTruthy();
    expect(await res.json()).toEqual({ status: "ok" });
  });
});

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------
test.describe("SaaS: Auth", () => {
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

  test("API key lifecycle", async ({ request }) => {
    const { token } = await signup(request);

    // Create API key
    const createRes = await request.post("/b/auth/api/api-keys", {
      headers: { Authorization: `Bearer ${token}` },
      data: { name: "CI/CD Key" },
    });
    expect(createRes.ok()).toBeTruthy();
    const key = await createRes.json();
    expect(key.key).toMatch(/^sb_/);

    // List API keys
    const listRes = await request.get("/b/auth/api/api-keys", {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(listRes.ok()).toBeTruthy();

    // Revoke
    const revokeRes = await request.delete(`/b/auth/api/api-keys/${key.id}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(revokeRes.ok()).toBeTruthy();
  });
});

// ---------------------------------------------------------------------------
// Products (subscription plans)
// ---------------------------------------------------------------------------
test.describe("SaaS: Products", () => {
  test("GET /b/products/catalog returns plan list", async ({ request }) => {
    const { token } = await signup(request);
    const res = await request.get("/b/products/catalog", {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(res.ok()).toBeTruthy();
  });

  test("GET /b/products/purchases returns user purchases", async ({
    request,
  }) => {
    const { token } = await signup(request);
    const res = await request.get("/b/products/purchases", {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(res.ok()).toBeTruthy();
  });
});

// ---------------------------------------------------------------------------
// Admin
// ---------------------------------------------------------------------------
test.describe("SaaS: Admin", () => {
  test("admin endpoints require auth", async ({ request }) => {
    const res = await request.get("/b/admin/api/users");
    expect(res.status()).toBe(401);
  });

  test("GET /b/admin/api/settings accessible with token", async ({
    request,
  }) => {
    const { token } = await signup(request);
    const res = await request.get("/b/admin/api/settings", {
      headers: { Authorization: `Bearer ${token}` },
    });
    // 200 if admin (first user), 403 otherwise
    expect([200, 403]).toContain(res.status());
  });

  test("GET /b/admin/api/logs accessible with token", async ({ request }) => {
    const { token } = await signup(request);
    const res = await request.get("/b/admin/api/logs", {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect([200, 403]).toContain(res.status());
  });
});

// ---------------------------------------------------------------------------
// Legal Pages
// ---------------------------------------------------------------------------
test.describe("SaaS: Legal Pages", () => {
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
