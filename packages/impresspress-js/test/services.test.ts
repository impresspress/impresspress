import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { ImpresspressClient } from "../src/client";
import { ImpresspressError } from "../src/error";
import anonymousMe from "./anonymous-me.response.json";
import { fakeJsonResponse, fakeBlobResponse, hangingFetch, recordedResponse } from "./fixtures";

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

  it("signUp with verification off returns the signed-in branch and caches its tokens", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse(
        {
          email_verified: true,
          access_token: "a",
          refresh_token: "r",
          token_type: "Bearer",
          expires_in: 1800,
          default_redirect: "/b/userportal/",
          user: { id: "u1", email: "a@b.com", roles: ["user"], name: "" },
        },
        201,
      ),
    );

    const c = client();
    const result = await c.auth.signUp({ email: "a@b.com", password: "pw" });

    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/auth/api/signup");
    if (!result.emailVerified) throw new Error("expected the signed-in branch");
    expect(result.tokens.access_token).toBe("a");
    expect(result.user.id).toBe("u1");
    expect(result.default_redirect).toBe("/b/userportal/");
    expect(c.auth.isAuthenticated()).toBe(true);
  });

  it("signUp awaiting verification returns the pending branch and signs nobody in", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse(
        {
          email_verified: false,
          message: "Account created. Please verify your email before signing in.",
          user: { email: "a@b.com" },
        },
        201,
      ),
    );

    const c = client();
    const result = await c.auth.signUp({ email: "a@b.com", password: "pw" });

    expect(result).toEqual({
      emailVerified: false,
      message: "Account created. Please verify your email before signing in.",
      user: { email: "a@b.com" },
    });
    expect(c.auth.isAuthenticated()).toBe(false);
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

  it("updateUser unwraps the same {user} envelope as GET /me", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({
        user: { id: "u1", email: "a@b.com", roles: ["user"], name: "New Name" },
      }),
    );
    const c = client();
    const user = await c.auth.updateUser({ name: "New Name" });
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/auth/api/me");
    expect(init.method).toBe("PATCH");
    expect(user.name).toBe("New Name");
  });

  // The server's own answer to `GET /b/auth/api/me` with no session, byte for
  // byte: `the_sdk_fixture_is_what_an_anonymous_me_answers`
  // (crates/impresspress-core/tests/request_preamble.rs) fails when the
  // server stops sending exactly this.
  it("getUser returns null for the server's real anonymous /me answer", async () => {
    expect(anonymousMe.status).toBe(401);
    fetchMock.mockResolvedValueOnce(recordedResponse(anonymousMe));
    const user = await client().auth.getUser();
    expect(user).toBeNull();
  });

  it("getUser propagates a 500 rather than fabricating null", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({ error: "InternalError", message: "db down" }, 500),
    );
    await expect(client().auth.getUser()).rejects.toBeInstanceOf(ImpresspressError);
  });

  // The server answers a request whose credential it could not check (a
  // failed blocklist or auth_version read) with 503, or with the 403 a WRAP
  // refusal keeps. Neither means "signed out": getUser must propagate it and
  // leave the session in place, so the next call after the outage succeeds.
  for (const [status, error] of [
    [503, "Unavailable"],
    [403, "PermissionDenied"],
  ] as const) {
    it(`getUser propagates a ${status} and keeps the session`, async () => {
      fetchMock.mockResolvedValueOnce(
        fakeJsonResponse({
          access_token: "a",
          refresh_token: "r",
          token_type: "Bearer",
          expires_in: 1800,
          user: { id: "u1", email: "a@b.com", roles: ["user"] },
        }),
      );
      const c = client();
      await c.auth.signIn({ email: "a@b.com", password: "pw" });

      fetchMock.mockResolvedValueOnce(
        fakeJsonResponse(
          { error, message: "Authentication is temporarily unavailable" },
          status,
        ),
      );
      const err = await c.auth.getUser().then(
        () => null,
        (e: unknown) => e,
      );
      expect(err).toBeInstanceOf(ImpresspressError);
      expect((err as ImpresspressError).status).toBe(status);
      expect(c.auth.isAuthenticated()).toBe(true);
    });
  }

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

  it("signInWithOAuth returns the start URL to navigate to, without calling it", async () => {
    const res = await client().auth.signInWithOAuth("google");
    expect(res).toEqual({
      auth_url: "http://api.test/b/auth/oauth/login?provider=google",
      provider: "google",
    });
    // Navigating is the point: the start endpoint sets the cookie that binds
    // the flow to this browser, and a fetch from another origin cannot store
    // it. A request here would mean that binding is lost in Safari.
    expect(fetchMock).not.toHaveBeenCalled();
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

  it("search decodes the real RecordList shape ({records, total_count}), not {data, total}", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({
        records: [
          {
            id: "r1",
            data: {
              bucket: "my-bucket",
              key: "report.pdf",
              size: 42,
              content_type: "application/pdf",
              status: "complete",
              uploaded_by: "u1",
              uploaded_at: "2026-01-01T00:00:00Z",
            },
          },
        ],
        total_count: 1,
        page: 1,
        page_size: 20,
      }),
    );
    const result = await client().storage.search("report");
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/storage/api/search?q=report");
    expect(result.total).toBe(1);
    expect(result.items).toEqual([
      {
        id: "r1",
        bucket: "my-bucket",
        key: "report.pdf",
        size: 42,
        content_type: "application/pdf",
        status: "complete",
        uploaded_by: "u1",
        uploaded_at: "2026-01-01T00:00:00Z",
      },
    ]);
  });

  /**
   * The body here is an object-VIEW audit row, which is what
   * `handle_recent` actually pages (`repo::views::list_recent_for_user`) —
   * not an object-metadata row. The fixture used to send metadata columns,
   * which is the shape the route's schema wrongly claimed.
   */
  it("getRecentFiles calls /recent with no query parameters and decodes the view rows", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({
        records: [
          {
            id: "r2",
            data: {
              bucket: "b",
              key: "a.txt",
              user_id: "u1",
              viewed_at: "2026-01-02T00:00:00Z",
              created_at: "2026-01-02T00:00:00Z",
              updated_at: "2026-01-02T00:00:00Z",
            },
          },
        ],
        total_count: 1,
        page: 1,
        page_size: 20,
      }),
    );
    const result = await client().storage.getRecentFiles();
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/storage/api/recent");
    expect(result.total).toBe(1);
    expect(result.items[0].key).toBe("a.txt");
    expect(result.items[0].id).toBe("r2");
    expect(result.items[0].user_id).toBe("u1");
    expect(result.items[0].viewed_at).toBe("2026-01-02T00:00:00Z");
  });
});

/**
 * Transfer timeouts. Folding the upload/download paths into `HttpClient`
 * brought them under a client that applies a 30 s default — which would have
 * capped every large upload and download, silently, at half a minute. The
 * transfer paths therefore opt out of the timeout by default (`NO_TIMEOUT`)
 * and take a per-call override instead. These tests are the guard.
 */
describe("transfer timeouts", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("uploadFile is not capped by the 30 s JSON default", async () => {
    const { fetchFn, captured } = hangingFetch();
    fetchMock.mockImplementation(fetchFn);
    const controller = new AbortController();

    const promise = client().storage.uploadFile("b", new Blob(["x"]), {
      key: "big.bin",
      signal: controller.signal,
    });
    const assertion = expect(promise).rejects.toMatchObject({ code: "aborted" });

    // Ten minutes into a slow upload: nothing may have aborted it.
    await vi.advanceTimersByTimeAsync(600_000);
    expect(captured.signal?.aborted).toBe(false);

    controller.abort();
    await assertion;
  });

  it("uploadFile honours an explicit timeout when the caller wants one", async () => {
    const { fetchFn } = hangingFetch();
    fetchMock.mockImplementation(fetchFn);

    const promise = client().storage.uploadFile("b", new Blob(["x"]), {
      key: "f.txt",
      timeout: 5_000,
    });
    const assertion = expect(promise).rejects.toMatchObject({ code: "timeout" });
    await vi.advanceTimersByTimeAsync(5_000);
    await assertion;
  });

  it("downloadFile is not capped by the 30 s JSON default", async () => {
    const { fetchFn, captured } = hangingFetch();
    fetchMock.mockImplementation(fetchFn);
    const controller = new AbortController();

    const promise = client().storage.downloadFile("b", "big.bin", { signal: controller.signal });
    const assertion = expect(promise).rejects.toMatchObject({ code: "aborted" });

    await vi.advanceTimersByTimeAsync(600_000);
    expect(captured.signal?.aborted).toBe(false);

    controller.abort();
    await assertion;
  });

  it("downloadFile honours an explicit timeout when the caller wants one", async () => {
    const { fetchFn } = hangingFetch();
    fetchMock.mockImplementation(fetchFn);

    const promise = client().storage.downloadFile("b", "f.txt", { timeout: 5_000 });
    const assertion = expect(promise).rejects.toMatchObject({ code: "timeout" });
    await vi.advanceTimersByTimeAsync(5_000);
    await assertion;
  });

  it("a plain JSON call still gets the 30 s default", async () => {
    const { fetchFn } = hangingFetch();
    fetchMock.mockImplementation(fetchFn);

    const promise = client().storage.listBuckets();
    const assertion = expect(promise).rejects.toMatchObject({ code: "timeout" });
    await vi.advanceTimersByTimeAsync(30_000);
    await assertion;
  });
});

/**
 * Every failure the SDK raises itself carries a machine-readable `code`, so a
 * caller can branch on it without matching on message text.
 */
describe("SDK-raised error codes", () => {
  it("refreshSession without a token throws code no_refresh_token", async () => {
    const error = await client()
      .auth.refreshSession()
      .then(() => null)
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ImpresspressError);
    expect((error as ImpresspressError).code).toBe("no_refresh_token");
  });

  it("uploadFile with a non-file throws code invalid_file_type", async () => {
    const error = await client()
      .storage.uploadFile("b", "not a file" as unknown as Blob)
      .then(() => null)
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ImpresspressError);
    expect((error as ImpresspressError).code).toBe("invalid_file_type");
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

/**
 * The six services share ONE `HttpClient`, so credentials set anywhere apply
 * everywhere — there is no per-service copy to keep in sync.
 */
describe("shared HttpClient", () => {
  it("an API key set on one service is sent by every other service", async () => {
    const c = client();
    c.auth.setApiKey("k-123");

    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ buckets: [] }));
    await c.storage.listBuckets();
    expect(fetchMock.mock.calls[0][1].headers["Authorization"]).toBe("Bearer k-123");

    c.removeApiKey();
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ buckets: [] }));
    await c.storage.listBuckets();
    expect(fetchMock.mock.calls[1][1].headers["Authorization"]).toBeUndefined();
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

  it("listShares() decodes the real RecordList shape ({records, total_count}), not {data, total}", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({
        records: [
          {
            id: "s1",
            data: {
              token: "tok",
              bucket: "my-bucket",
              key: "f.txt",
              created_by: "u1",
              created_at: "2026-01-01T00:00:00Z",
              access_count: 0,
              expires_at: null,
              max_access_count: null,
            },
          },
        ],
        total_count: 1,
        page: 1,
        page_size: 100,
      }),
    );
    const result = await client().cloudStorage.listShares();
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/cloudstorage/shares");
    expect(result.total).toBe(1);
    expect(result.items).toEqual([
      {
        id: "s1",
        token: "tok",
        bucket: "my-bucket",
        key: "f.txt",
        created_by: "u1",
        created_at: "2026-01-01T00:00:00Z",
        access_count: 0,
        expires_at: null,
        max_access_count: null,
      },
    ]);
  });

  it("getQuota() returns the real {quota, usage} shape from /b/cloudstorage/quota", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({
        quota: { max_storage_bytes: 1 },
        usage: { total_bytes: 0, file_count: 0 },
      }),
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
    await client().products.createGroup({ name: "G", group_template_id: "t1" });
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/products/api/admin/groups");
    expect(JSON.parse(init.body)).toEqual({ name: "G", group_template_id: "t1" });
  });

  it("drives the public storefront, server pricing, checkout, and capability-protected status routes", async () => {
    fetchMock
      .mockResolvedValueOnce(fakeJsonResponse({ id: "product one", offers: [] }))
      .mockResolvedValueOnce(fakeJsonResponse({ offer_id: "offer_1", amounts: { total_minor: 1250 } }))
      .mockResolvedValueOnce(fakeJsonResponse({ order_id: "order_1", receipt_token: "receipt" }))
      .mockResolvedValueOnce(fakeJsonResponse({ order_id: "order_1", status: "completed" }));
    const products = client().products;
    await products.getStorefrontProduct("product one");
    await products.previewPrice({ offer_id: "offer_1", quantity: 2, inputs: { seats: 3 } });
    await products.checkout({
      offer_id: "offer_1",
      presentation: "embedded",
      success_url: "https://shop.test/return",
    });
    await products.getGuestOrderStatus("order_1", "receipt+token");

    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/products/storefront/product%20one");
    expect(fetchMock.mock.calls[1][0]).toBe("http://api.test/b/products/pricing/preview");
    expect(fetchMock.mock.calls[1][1].method).toBe("POST");
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({
      offer_id: "offer_1",
      quantity: 2,
      inputs: { seats: 3 },
    });
    expect(fetchMock.mock.calls[2][0]).toBe("http://api.test/b/products/checkout");
    expect(JSON.parse(fetchMock.mock.calls[2][1].body).presentation).toBe("embedded");
    expect(fetchMock.mock.calls[3][0]).toBe(
      "http://api.test/b/products/orders/order_1/status?receipt_token=receipt%2Btoken",
    );
  });

  it("separates admin from seller product ownership routes", async () => {
    fetchMock
      .mockResolvedValueOnce(fakeJsonResponse({ id: "p1" }))
      .mockResolvedValueOnce(fakeJsonResponse({ id: "p2" }))
      .mockResolvedValueOnce(fakeJsonResponse({ id: "p2" }));
    const products = client().products;
    await products.createProduct({ name: "Admin product", product_template_id: "simple_product" });
    await products.createSellerProduct({ name: "Seller product", product_template_id: "simple_subscription" });
    await products.updateSellerProduct("p/2", { status: "active" });

    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/products/api/admin/products");
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({
      name: "Admin product",
      product_template_id: "simple_product",
    });
    expect(fetchMock.mock.calls[1][0]).toBe("http://api.test/b/products/api/products");
    expect(fetchMock.mock.calls[2][0]).toBe("http://api.test/b/products/api/products/p%2F2");
    expect(fetchMock.mock.calls[2][1].method).toBe("PATCH");
  });

  it("preserves the server envelopes for product duplication and builder collections", async () => {
    fetchMock
      .mockResolvedValueOnce(fakeJsonResponse({
        product: { id: "copy_1", data: { name: "Copy" } },
        offers: [{ status: "draft", offer: { id: "offer_copy" } }],
      }))
      .mockResolvedValueOnce(fakeJsonResponse({ offers: [{ status: "active" }] }))
      .mockResolvedValueOnce(fakeJsonResponse({ presets: [{ id: "preset_1" }] }))
      .mockResolvedValueOnce(fakeJsonResponse({ payment_links: [{ id: "link_1" }] }))
      .mockResolvedValueOnce(fakeJsonResponse({ deleted: true }));

    const products = client().products;
    const duplicate = await products.duplicateProduct("product/1");
    const offers = await products.listOffers("product/1", "seller");
    const presets = await products.listCheckoutPresets("product/1", "offer/1", "seller");
    const links = await products.listPaymentLinks("product/1", "offer/1", "seller");
    const deleted = await products.deleteSellerProduct("product/1");

    expect(duplicate.product.id).toBe("copy_1");
    expect(duplicate.offers[0].offer.id).toBe("offer_copy");
    expect(offers.offers).toHaveLength(1);
    expect(presets.presets[0].id).toBe("preset_1");
    expect(links.payment_links[0].id).toBe("link_1");
    expect(deleted.deleted).toBe(true);
    expect(fetchMock.mock.calls.map((call) => call[0])).toEqual([
      "http://api.test/b/products/api/admin/products/product%2F1/duplicate",
      "http://api.test/b/products/api/products/product%2F1/offers",
      "http://api.test/b/products/api/products/product%2F1/offers/offer%2F1/presets",
      "http://api.test/b/products/api/products/product%2F1/offers/offer%2F1/payment-links",
      "http://api.test/b/products/api/products/product%2F1",
    ]);
    expect(fetchMock.mock.calls[0][1].method).toBe("POST");
    expect(fetchMock.mock.calls[4][1].method).toBe("DELETE");
  });

  it("previews owned draft offers through scoped admin and seller routes", async () => {
    fetchMock
      .mockResolvedValueOnce(fakeJsonResponse({ offer_id: "offer/1", amounts: { total_minor: 4000 } }))
      .mockResolvedValueOnce(fakeJsonResponse({ offer_id: "offer/2", amounts: { total_minor: 2500 } }));

    const products = client().products;
    await products.previewManagedOffer("product/1", "offer/1", {
      quantity: 2,
      inputs: { seats: 4 },
    });
    await products.previewManagedOffer("product/2", "offer/2", {}, "seller");

    expect(fetchMock.mock.calls[0][0]).toBe(
      "http://api.test/b/products/api/admin/products/product%2F1/offers/offer%2F1/preview",
    );
    expect(fetchMock.mock.calls[0][1].method).toBe("POST");
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({
      offer_id: "offer/1",
      quantity: 2,
      inputs: { seats: 4 },
    });
    expect(fetchMock.mock.calls[1][0]).toBe(
      "http://api.test/b/products/api/products/product%2F2/offers/offer%2F2/preview",
    );
    expect(JSON.parse(fetchMock.mock.calls[1][1].body)).toEqual({ offer_id: "offer/2" });
  });

  it("uses the immutable offer lifecycle under the selected admin or seller scope", async () => {
    fetchMock
      .mockResolvedValueOnce(fakeJsonResponse({ status: "draft" }))
      .mockResolvedValueOnce(fakeJsonResponse({ status: "active" }))
      .mockResolvedValueOnce(fakeJsonResponse({ status: "active", sync_status: "synced" }))
      .mockResolvedValueOnce(fakeJsonResponse({ status: "draft" }))
      .mockResolvedValueOnce(fakeJsonResponse({ status: "archived" }));
    const definition = {
      name: "Monthly",
      mode: "subscription" as const,
      currency: "NZD",
      pricing_model: "fixed" as const,
      recurring_interval: "month" as const,
      usage_type: "licensed" as const,
      billing_scheme: "per_unit" as const,
      tax_behavior: "exclusive" as const,
      components: [{ key: "base", label: "Base", amount: { type: "fixed" as const, unit_amount_minor: 2500 } }],
    };
    const products = client().products;
    await products.createOffer("product_1", definition, "seller");
    await products.publishOffer("product_1", "offer_1", "seller");
    await products.syncOffer("product_1", "offer_1", "seller");
    await products.duplicateOffer("product_1", "offer_1", "admin");
    await products.archiveOffer("product_1", "offer_1", "admin");

    expect(fetchMock.mock.calls[0][0]).toBe(
      "http://api.test/b/products/api/products/product_1/offers",
    );
    expect(fetchMock.mock.calls[1][0]).toBe(
      "http://api.test/b/products/api/products/product_1/offers/offer_1/publish",
    );
    expect(fetchMock.mock.calls[2][0]).toBe(
      "http://api.test/b/products/api/products/product_1/offers/offer_1/sync",
    );
    expect(fetchMock.mock.calls[3][0]).toBe(
      "http://api.test/b/products/api/admin/products/product_1/offers/offer_1/duplicate",
    );
    expect(fetchMock.mock.calls[4][1].method).toBe("DELETE");
  });

  it("manages checkout presets and Payment Links without inventing provider routes", async () => {
    fetchMock
      .mockResolvedValueOnce(fakeJsonResponse({ id: "preset_1" }))
      .mockResolvedValueOnce(fakeJsonResponse({ id: "link_1", url: "https://buy.stripe.com/x" }))
      .mockResolvedValueOnce(fakeJsonResponse({ id: "link_1", active: false }));
    const products = client().products;
    await products.createCheckoutPreset(
      "product_1",
      "offer_1",
      { name: "Five seats", inputs: { seats: 5 } },
      "seller",
    );
    await products.createPaymentLink(
      "product_1",
      "offer_1",
      { preset_id: "preset_1", after_completion_url: "https://shop.test/thanks" },
      "seller",
    );
    await products.deactivatePaymentLink("product_1", "offer_1", "link_1", "seller");

    expect(fetchMock.mock.calls[0][0]).toBe(
      "http://api.test/b/products/api/products/product_1/offers/offer_1/presets",
    );
    expect(fetchMock.mock.calls[1][0]).toBe(
      "http://api.test/b/products/api/products/product_1/offers/offer_1/payment-links",
    );
    expect(JSON.parse(fetchMock.mock.calls[1][1].body).preset_id).toBe("preset_1");
    expect(fetchMock.mock.calls[2][0]).toBe(
      "http://api.test/b/products/api/products/product_1/offers/offer_1/payment-links/link_1",
    );
    expect(fetchMock.mock.calls[2][1].method).toBe("DELETE");
  });

  it("maps buyer Billing Portal and seller Connect, analytics, order, and refund workflows", async () => {
    fetchMock
      .mockResolvedValueOnce(fakeJsonResponse({ url: "https://billing.stripe.com/x" }))
      .mockResolvedValueOnce(fakeJsonResponse({ account: { id: "seller_1" }, url: "https://connect.stripe.com/x" }))
      .mockResolvedValueOnce(fakeJsonResponse({
        seller_account_id: "seller_1",
        currency_analytics: [],
        recent_failures: [{
          order_id: "order_failed_1",
          status: "failed",
          currency: "NZD",
          total_minor: 1200,
          error: "Payment did not complete",
          created_at: "2026-07-20T00:00:00Z",
        }],
      }))
      .mockResolvedValueOnce(fakeJsonResponse({ records: [], total_count: 0 }))
      .mockResolvedValueOnce(fakeJsonResponse({ purchase_id: "order_1", status: "succeeded" }));
    const products = client().products;
    await products.createBillingPortal("https://shop.test/account", "order_1");
    await products.startSellerOnboarding(
      "https://shop.test/seller/return",
      "https://shop.test/seller/refresh",
    );
    const sellerStats = await products.getSellerStats();
    await products.listSellerOrders({ status: "completed" });
    await products.refundSellerOrder("order_1", {
      amount_minor: 500,
      provider_reason: "requested_by_customer",
      idempotency_key: "support_case_42",
    });

    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/products/billing-portal");
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({
      return_url: "https://shop.test/account",
      order_id: "order_1",
    });
    expect(fetchMock.mock.calls[1][0]).toBe("http://api.test/b/products/api/seller/onboarding");
    expect(fetchMock.mock.calls[2][0]).toBe("http://api.test/b/products/api/seller/stats");
    expect(sellerStats.recent_failures[0].order_id).toBe("order_failed_1");
    expect(fetchMock.mock.calls[3][0]).toBe(
      "http://api.test/b/products/api/seller/orders?status=completed",
    );
    expect(fetchMock.mock.calls[4][0]).toBe(
      "http://api.test/b/products/api/seller/orders/order_1/refund",
    );
    expect(JSON.parse(fetchMock.mock.calls[4][1].body).amount_minor).toBe(500);
  });

  it("uses currency-separated admin analytics and provider-first refund routes", async () => {
    fetchMock
      .mockResolvedValueOnce(fakeJsonResponse({ currency_analytics: [{ currency: "NZD" }] }))
      .mockResolvedValueOnce(fakeJsonResponse({ purchase_id: "order_9", status: "succeeded" }));
    const products = client().products;
    await products.getAdminStats();
    await products.refundAdminOrder("order_9", { note: "Customer request" });
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/products/api/admin/stats");
    expect(fetchMock.mock.calls[1][0]).toBe(
      "http://api.test/b/products/api/admin/purchases/order_9/refund",
    );
    expect(fetchMock.mock.calls[1][1].method).toBe("POST");
  });

  it("uses the admin seller governance and moderation routes", async () => {
    fetchMock
      .mockResolvedValueOnce(fakeJsonResponse({ sellers: [] }))
      .mockResolvedValueOnce(fakeJsonResponse({ seller: { id: "seller/1" }, products: [] }))
      .mockResolvedValueOnce(fakeJsonResponse({ id: "seller/1", status: "suspended" }))
      .mockResolvedValueOnce(fakeJsonResponse({ id: "seller/1", status: "active" }))
      .mockResolvedValueOnce(fakeJsonResponse({ id: "product/1" }))
      .mockResolvedValueOnce(fakeJsonResponse({ id: "product/2" }));
    const products = client().products;
    await products.listAdminSellers();
    await products.getAdminSeller("seller/1");
    await products.suspendAdminSeller("seller/1");
    await products.reactivateAdminSeller("seller/1");
    await products.approveSellerProduct("product/1");
    await products.rejectSellerProduct("product/2");

    expect(fetchMock.mock.calls.map((call) => call[0])).toEqual([
      "http://api.test/b/products/api/admin/sellers",
      "http://api.test/b/products/api/admin/sellers/seller%2F1",
      "http://api.test/b/products/api/admin/sellers/seller%2F1/suspend",
      "http://api.test/b/products/api/admin/sellers/seller%2F1/reactivate",
      "http://api.test/b/products/api/admin/products/product%2F1/approve",
      "http://api.test/b/products/api/admin/products/product%2F2/reject",
    ]);
    expect(fetchMock.mock.calls.slice(2).every((call) => call[1].method === "POST")).toBe(true);
  });

  it("lists safe webhook operations and replays a failed event through admin routes", async () => {
    fetchMock
      .mockResolvedValueOnce(
        fakeJsonResponse({ records: [], total_count: 0, page: 2, page_size: 25 }),
      )
      .mockResolvedValueOnce(fakeJsonResponse({ received: true }));
    const products = client().products;
    await products.getAdminWebhookEvents({
      page: 2,
      page_size: 25,
      status: "dead_letter",
    });
    await products.replayAdminWebhookEvent("evt/9");

    expect(fetchMock.mock.calls[0][0]).toBe(
      "http://api.test/b/products/api/admin/webhook-events?page=2&page_size=25&status=dead_letter",
    );
    expect(fetchMock.mock.calls[1][0]).toBe(
      "http://api.test/b/products/api/admin/webhook-events/evt%2F9/replay",
    );
    expect(fetchMock.mock.calls[1][1].method).toBe("POST");
  });

  it("lists and reconciles durable provider operations through admin routes", async () => {
    fetchMock
      .mockResolvedValueOnce(
        fakeJsonResponse({ records: [], total_count: 0, page: 1, page_size: 20 }),
      )
      .mockResolvedValueOnce(
        fakeJsonResponse({ claimed: 1, succeeded: 1, retry_scheduled: 0, dead_letter: 0 }),
      );
    const products = client().products;
    await products.getAdminProviderOperations({ status: "failed", page: 1, page_size: 20 });
    await products.reconcileAdminProviderOperations(10);

    expect(fetchMock.mock.calls[0][0]).toBe(
      "http://api.test/b/products/api/admin/provider-operations?status=failed&page=1&page_size=20",
    );
    expect(fetchMock.mock.calls[1][0]).toBe(
      "http://api.test/b/products/api/admin/provider-operations/reconcile?limit=10",
    );
    expect(fetchMock.mock.calls[1][1].method).toBe("POST");
  });

  it("pins every remaining Products method to its URL, verb, and body contract", async () => {
    const offer = {
      name: "Annual",
      mode: "subscription" as const,
      currency: "USD",
      pricing_model: "components" as const,
      recurring_interval: "year" as const,
      usage_type: "licensed" as const,
      billing_scheme: "per_unit" as const,
      tax_behavior: "exclusive" as const,
      components: [{
        key: "base",
        label: "Base",
        amount: { type: "fixed" as const, unit_amount_minor: 12000 },
      }],
    };
    const preset = { name: "Team", slug: "team", inputs: { seats: 8 } };
    const cases: Array<{
      name: string;
      invoke: (products: ImpresspressClient["products"]) => Promise<unknown>;
      path: string;
      method?: string;
      body?: unknown;
    }> = [
      {
        name: "listProducts options",
        invoke: (products) => products.listProducts({ page: 2, page_size: 5 }),
        path: "catalog?page=2&page_size=5",
      },
      { name: "getStorefrontConfig", invoke: (products) => products.getStorefrontConfig(), path: "storefront/config" },
      { name: "getProduct", invoke: (products) => products.getProduct("p/1"), path: "api/admin/products/p%2F1" },
      {
        name: "updateProduct",
        invoke: (products) => products.updateProduct("p/1", { name: "Renamed", product_template_id: "simple_product" }),
        path: "api/admin/products/p%2F1",
        method: "PATCH",
        body: { name: "Renamed", product_template_id: "simple_product" },
      },
      { name: "deleteProduct", invoke: (products) => products.deleteProduct("p/1"), path: "api/admin/products/p%2F1", method: "DELETE" },
      {
        name: "listSellerProducts",
        invoke: (products) => products.listSellerProducts({ page: 3, status: "draft", search: "field notes" }),
        path: "api/products?page=3&status=draft&search=field+notes",
      },
      { name: "getSellerProduct", invoke: (products) => products.getSellerProduct("p/1"), path: "api/products/p%2F1" },
      { name: "duplicateSellerProduct", invoke: (products) => products.duplicateSellerProduct("p/1"), path: "api/products/p%2F1/duplicate", method: "POST" },
      { name: "getOffer", invoke: (products) => products.getOffer("p/1", "o/1", "seller"), path: "api/products/p%2F1/offers/o%2F1" },
      { name: "updateOffer", invoke: (products) => products.updateOffer("p/1", "o/1", offer), path: "api/admin/products/p%2F1/offers/o%2F1", method: "PATCH", body: offer },
      { name: "updateCheckoutPreset", invoke: (products) => products.updateCheckoutPreset("p/1", "o/1", "pre/1", preset), path: "api/admin/products/p%2F1/offers/o%2F1/presets/pre%2F1", method: "PATCH", body: preset },
      { name: "archiveCheckoutPreset", invoke: (products) => products.archiveCheckoutPreset("p/1", "o/1", "pre/1", "seller"), path: "api/products/p%2F1/offers/o%2F1/presets/pre%2F1", method: "DELETE" },
      { name: "getStripeStatus", invoke: (products) => products.getStripeStatus(), path: "api/admin/stripe/status" },
      { name: "listAdminOrders", invoke: (products) => products.listAdminOrders({ page: 2, status: "completed" }), path: "api/admin/purchases?page=2&status=completed" },
      { name: "getAdminOrder", invoke: (products) => products.getAdminOrder("order/1"), path: "api/admin/purchases/order%2F1" },
      { name: "listPurchases", invoke: (products) => products.listPurchases({ page: 4, page_size: 10 }), path: "purchases?page=4&page_size=10" },
      { name: "getPurchase", invoke: (products) => products.getPurchase("order/1"), path: "purchases/order%2F1" },
      { name: "getSubscription", invoke: (products) => products.getSubscription(), path: "subscription" },
      { name: "getSellerAccount", invoke: (products) => products.getSellerAccount(), path: "api/seller/account" },
      { name: "createSellerDashboardLink", invoke: (products) => products.createSellerDashboardLink(), path: "api/seller/dashboard", method: "POST" },
      { name: "getSellerOrder", invoke: (products) => products.getSellerOrder("order/1"), path: "api/seller/orders/order%2F1" },
      { name: "listGroups", invoke: (products) => products.listGroups(), path: "api/admin/groups" },
    ];

    const products = client().products;
    for (const contract of cases) {
      fetchMock.mockResolvedValueOnce(fakeJsonResponse({ ok: true }));
      await contract.invoke(products);
      const [url, init] = fetchMock.mock.calls.at(-1)!;
      expect(url, contract.name).toBe(`http://api.test/b/products/${contract.path}`);
      expect(init.method, contract.name).toBe(contract.method ?? "GET");
      if (contract.body === undefined) {
        expect(init.body, contract.name).toBeUndefined();
      } else {
        expect(JSON.parse(init.body), contract.name).toEqual(contract.body);
      }
      expect(init.headers["Content-Type"], contract.name).toBe("application/json");
    }
  });
});

describe("IAMService", () => {
  const role = {
    id: "r1",
    name: "editor",
    description: "",
    permissions: ["posts.write"],
    is_system: false,
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
  };

  it("getRoles unwraps the {records, total_count, page, page_size} envelope the route returns", async () => {
    fetchMock.mockResolvedValueOnce(
      fakeJsonResponse({ records: [role], total_count: 1, page: 1, page_size: 1000 }),
    );
    const roles = await client().iam.getRoles();
    expect(fetchMock.mock.calls[0][0]).toBe("http://api.test/b/admin/api/iam/roles");
    expect(roles).toEqual([role]);
  });

  it("createRole posts the typed body and reads the flat role back", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse(role));
    const created = await client().iam.createRole({ name: "editor", permissions: ["posts.write"] });
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/admin/api/iam/roles");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body)).toEqual({ name: "editor", permissions: ["posts.write"] });
    expect(created.id).toBe("r1");
  });

  it("updateRole PATCHes by role id, not name", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ ...role, description: "Edits posts" }));
    const updated = await client().iam.updateRole("r1", { description: "Edits posts" });
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/admin/api/iam/roles/r1");
    expect(init.method).toBe("PATCH");
    expect(updated.description).toBe("Edits posts");
  });

  it("deleteRole DELETEs by role id", async () => {
    fetchMock.mockResolvedValueOnce(fakeJsonResponse({ deleted: true }));
    await client().iam.deleteRole("r1");
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe("http://api.test/b/admin/api/iam/roles/r1");
    expect(init.method).toBe("DELETE");
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
