import { test, expect } from "@playwright/test";

// ---------------------------------------------------------------------------
// Helper: sign up a fresh user, return token
// ---------------------------------------------------------------------------
async function signup(request: any) {
  const email = `blog-${Date.now()}-${Math.random().toString(36).slice(2, 8)}@test.com`;
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
test.describe("Blog: Landing Page", () => {
  test("serves themed landing page", async ({ request }) => {
    const res = await request.get("/");
    expect(res.ok()).toBeTruthy();
    const html = await res.text();
    expect(html).toContain("Inkwell");
    expect(html).toContain("Blog");
  });

  test("landing page has content features", async ({ request }) => {
    const res = await request.get("/");
    const html = await res.text();
    expect(html).toContain("Author Accounts");
    expect(html).toContain("Media Storage");
    expect(html).toContain("Admin Dashboard");
    expect(html).toContain("Custom Content");
  });

  test("SPA fallback works for blog routes", async ({ request }) => {
    const res = await request.get("/posts/my-first-article");
    expect(res.ok()).toBeTruthy();
    const html = await res.text();
    expect(html).toContain("Inkwell");
  });
});

// ---------------------------------------------------------------------------
// Health & System
// ---------------------------------------------------------------------------
test.describe("Blog: Health", () => {
  test("GET /health returns ok", async ({ request }) => {
    const res = await request.get("/health");
    expect(res.ok()).toBeTruthy();
    expect(await res.json()).toEqual({ status: "ok" });
  });
});

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------
test.describe("Blog: Auth", () => {
  test("signup creates author account", async ({ request }) => {
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

  test("update profile with author name", async ({ request }) => {
    const { token } = await signup(request);
    const res = await request.put("/b/auth/api/me", {
      headers: { Authorization: `Bearer ${token}` },
      data: { name: "Jane Author" },
    });
    expect(res.ok()).toBeTruthy();

    // Verify
    const meRes = await request.get("/b/auth/api/me", {
      headers: { Authorization: `Bearer ${token}` },
    });
    const body = await meRes.json();
    expect(body.user.name).toBe("Jane Author");
  });
});

// ---------------------------------------------------------------------------
// Storage (media uploads)
// ---------------------------------------------------------------------------
test.describe("Blog: Storage", () => {
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
// Admin
// ---------------------------------------------------------------------------
test.describe("Blog: Admin", () => {
  test("admin endpoints require auth", async ({ request }) => {
    const res = await request.get("/b/admin/api/users");
    expect(res.status()).toBe(401);
  });

  test("GET /b/admin/api/users accessible with token", async ({ request }) => {
    const { token } = await signup(request);
    const res = await request.get("/b/admin/api/users", {
      headers: { Authorization: `Bearer ${token}` },
    });
    // 200 if admin (first user), 403 otherwise
    expect([200, 403]).toContain(res.status());
  });

  test("GET /b/admin/api/settings accessible with token", async ({
    request,
  }) => {
    const { token } = await signup(request);
    const res = await request.get("/b/admin/api/settings", {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect([200, 403]).toContain(res.status());
  });

  test("GET /b/admin/api/database/collections accessible", async ({
    request,
  }) => {
    const { token } = await signup(request);
    const res = await request.get("/b/admin/api/database/collections", {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect([200, 403]).toContain(res.status());
  });
});

// ---------------------------------------------------------------------------
// Legal Pages
// ---------------------------------------------------------------------------
test.describe("Blog: Legal Pages", () => {
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
