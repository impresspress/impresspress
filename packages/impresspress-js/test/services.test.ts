import { describe, it, expect, vi, beforeEach } from "vitest";
import { ImpresspressClient } from "../src/client";
import { ImpresspressError } from "../src/error";
import { fakeJsonResponse, fakeBlobResponse } from "./fixtures";

/**
 * These tests pin every SDK method to the REAL server route it now calls
 * (confirmed against `crates/impresspress-core/src/blocks/**` — see the
 * route tables in `auth_ui/mod.rs`, `files/{mod,storage,cloud}.rs`,
 * `products/mod.rs`), not the imagined `/api/collections/*`,
 * `/api/database/*`, and stale OAuth/reset paths the SDK called before.
 * `fetch` is mocked at the transport boundary so no live server is needed.
 */

function client(url = "http://api.test") {
  return new ImpresspressClient(url);
}

let fetchMock: ReturnType<typeof vi.fn>;

beforeEach(() => {
  fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
});

describe("AuthService", () => {
  it("signIn posts the real /login route and returns a flat, unwrapped result", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({
        access_token: "a",
        refresh_token: "r",
        token_type: "Bearer",
        expires_in: 1800,
        default_redirect: "/b/userportal/",
        user: { id: "u1", email: "a@b.com", roles: ["user"], name: "A" },
      }),
    );

    const c = client();
    const result = await c.auth.signIn({ email: "a@b.com", password: "pw" });

    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/auth/api/login");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body)).toEqual({ email: "a@b.com", password: "pw" });
    expect(result.user.id).toBe("u1");
    expect(result.tokens.access_token).toBe("a");
    expect(c.auth.isAuthenticated()).toBe(true);
  });

  it("getUser unwraps the {user} envelope GET /me actually returns", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({ user: { id: "u1", email: "a@b.com", roles: ["user"] } }),
    );
    const c = client();
    const user = await c.auth.getUser();
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/auth/api/me");
    expect(user?.id).toBe("u1");
  });

  it("updateUser reads the update response directly (no {user} wrapper, unlike GET /me)", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({ id: "u1", email: "a@b.com", roles: ["user"], name: "New Name" }),
    );
    const c = client();
    const user = await c.auth.updateUser({ name: "New Name" });
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/auth/api/me");
    expect(init.method).toBe("PATCH");
    expect(user.name).toBe("New Name");
  });

  it("getUser maps a 401 to null instead of throwing", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({ error: "Unauthorized", message: "Not authenticated" }, 401),
    );
    const user = await client().auth.getUser();
    expect(user).toBeNull();
  });

  it("getUser propagates a 500 rather than fabricating null", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({ error: "InternalError", message: "db down" }, 500),
    );
    await expect(client().auth.getUser()).rejects.toBeInstanceOf(ImpresspressError);
  });

  it("resetPassword calls the real forgot-password route, not a phantom reset-password GET", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ message: "ok" }));
    await client().auth.resetPassword({ email: "a@b.com" });
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/auth/api/forgot-password");
  });

  it("confirmPasswordReset posts snake_case new_password to /reset-password", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ message: "ok" }));
    await client().auth.confirmPasswordReset("tok", "newpw");
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/auth/api/reset-password");
    expect(JSON.parse(init.body)).toEqual({ token: "tok", new_password: "newpw" });
  });

  it("updatePassword sends snake_case current_password/new_password", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ message: "ok" }));
    await client().auth.updatePassword({ currentPassword: "old", newPassword: "new" });
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/auth/api/change-password");
    expect(JSON.parse(init.body)).toEqual({ current_password: "old", new_password: "new" });
  });

  it("verifyEmail posts to /verify, not the stale /verify-email path", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({}));
    await client().auth.verifyEmail("tok");
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/auth/api/verify");
    expect(JSON.parse(init.body)).toEqual({ token: "tok" });
  });

  it("resendVerification requires and sends the email the server needs", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ message: "ok" }));
    await client().auth.resendVerification("a@b.com");
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/auth/api/resend-verification");
    expect(JSON.parse(init.body)).toEqual({ email: "a@b.com" });
  });

  it("signInWithOAuth passes provider as a query param and reads auth_url (not `url`)", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({ auth_url: "https://accounts.google.com/x", provider: "google" }),
    );
    const res = await client().auth.signInWithOAuth("google");
    expect(fetchMock.mock.calls[0][0]).toBe(
      "http://api.test/b/auth/oauth/login?provider=google",
    );
    expect(res.auth_url).toContain("accounts.google.com");
  });

  it("refreshSession requires refresh_token in the body (server rejects an empty one)", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({
        access_token: "a2",
        refresh_token: "r2",
        token_type: "Bearer",
        expires_in: 900,
      }),
    );
    await client().auth.refreshSession("r-explicit");
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/auth/api/refresh");
    expect(JSON.parse(init.body)).toEqual({ refresh_token: "r-explicit" });
  });

  it("refreshSession throws (rather than sending no body) when no token is cached or passed", async () => {
    await expect(client().auth.refreshSession()).rejects.toThrow(/refresh token/i);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe("StorageService", () => {
  it("listBuckets unwraps {buckets: string[]}, not a Bucket-object array", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ buckets: ["a", "b"] }));
    const buckets = await client().storage.listBuckets();
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/storage/api/buckets");
    expect(buckets).toEqual(["a", "b"]);
  });

  it("createBucket posts {name, public}", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ name: "b1", created: true }));
    await client().storage.createBucket("b1", true);
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/storage/api/buckets");
    expect(JSON.parse(init.body)).toEqual({ name: "b1", public: true });
  });

  it("listObjects hits the real per-bucket route with prefix/page params", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({ objects: [{ key: "a.txt", size: 1, content_type: "text/plain", last_modified: "now" }], total_count: 1 }),
    );
    const result = await client().storage.listObjects("my-bucket", { prefix: "a" });
    expect(fetchMock.mock.calls[0][0]).toBe(
      "http://api.test/b/storage/api/buckets/my-bucket/objects?prefix=a",
    );
    expect(result.total_count).toBe(1);
  });

  it("deleteObject encodes a multi-segment key without escaping its slashes", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ deleted: true }));
    await client().storage.deleteObject("my-bucket", "dir/sub dir/file.txt");
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe(
      "http://api.test/b/storage/api/buckets/my-bucket/objects/dir/sub%20dir/file.txt",
    );
    expect(init.method).toBe("DELETE");
  });

  it("downloadFile returns a Blob from the raw (non-JSON) object route", async () => {
    fetchMock.mockResolvedValueOnce(fakeBlobResponse("hello", "text/plain"));
    const blob = await client().storage.downloadFile("my-bucket", "hello.txt");
    expect(fetchMock.mock.calls[0][0]).toBe(
      "http://api.test/b/storage/api/buckets/my-bucket/objects/hello.txt",
    );
    expect(blob).toBeInstanceOf(Blob);
  });

  it("uploadFile multipart-posts to the objects route and returns {bucket,key,uploaded}", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ bucket: "my-bucket", key: "f.txt", uploaded: true }));
    const result = await client().storage.uploadFile("my-bucket", new Blob(["hi"]), { key: "f.txt" });
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/storage/api/buckets/my-bucket/objects?key=f.txt");
    expect(init.method).toBe("POST");
    expect(init.body).toBeInstanceOf(FormData);
    expect(result.uploaded).toBe(true);
  });

  it("search calls the real /search?q= route", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ data: [] }));
    await client().storage.search("report");
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/storage/api/search?q=report");
  });

  it("getRecentFiles calls /recent with no query parameters (the server ignores any)", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ data: [] }));
    await client().storage.getRecentFiles();
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/storage/api/recent");
  });
});

describe("ExtensionsService", () => {
  it("list() calls the real /b/admin/api/extensions route", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse([]));
    await client().extensions.list();
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/admin/api/extensions");
  });

  it("call() builds /b/{extension}/{endpoint} as a raw passthrough", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ ok: true }));
    await client().extensions.call("cloudstorage", "quota", { method: "GET" });
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/cloudstorage/quota");
  });
});

describe("CloudStorageExtension", () => {
  it("share() posts {bucket, key, expires_in_hours} to /b/cloudstorage/shares", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({ id: "s1", token: "tok", direct_url: "/b/storage/direct/tok" }),
    );
    await client().cloudStorage.share("my-bucket", "f.txt", { expiresInHours: 24 });
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/cloudstorage/shares");
    expect(JSON.parse(init.body)).toEqual({
      bucket: "my-bucket",
      key: "f.txt",
      expires_in_hours: 24,
      max_access_count: undefined,
    });
  });

  it("getQuota() returns the real {quota, usage} shape from /b/cloudstorage/quota", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({ quota: { max_storage_bytes: 1 }, usage: { storage_used: 0 } }),
    );
    const result = await client().cloudStorage.getQuota();
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/cloudstorage/quota");
    expect(result.quota.max_storage_bytes).toBe(1);
  });
});

describe("ProductsExtension", () => {
  it("listProducts() browses the public /b/products/catalog route, not a phantom /products", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({ records: [], total_count: 0, page: 1, page_size: 20 }),
    );
    await client().products.listProducts();
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/products/catalog");
  });

  it("createGroup() posts to the real admin-gated /b/products/api/admin/groups", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ id: "g1" }));
    await client().products.createGroup({ name: "G", template_id: "t1" });
    const [url] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/products/api/admin/groups");
  });
});

describe("IAMService", () => {
  it("getRoles calls the real /b/admin/api/iam/roles route", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse([]));
    await client().iam.getRoles();
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/admin/api/iam/roles");
  });
});

describe("ImpresspressError", () => {
  it("carries the real {error, message} wire fields as code/message", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({ error: "NotFound", message: "no such thing" }, 404),
    );
    const c = client();
    const promise = c.iam.getRoles();
    await expect(promise).rejects.toBeInstanceOf(ImpresspressError);
    await expect(promise).rejects.toMatchObject({
      code: "NotFound",
      message: "no such thing",
      status: 404,
    });
  });
});
