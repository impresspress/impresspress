import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { HttpClient } from "../src/http-client";
import { ImpresspressError, isStatementBudgetError } from "../src/error";
import { fakeBlobResponse, fakeJsonResponse, hangingFetch } from "./fixtures";

/**
 * The single request path every service now goes through. These tests pin the
 * three behaviours the fold-in of `requestFormData`/`requestBlob` had to get
 * right: raw bodies are not JSON-encoded, a blob response is not pre-read as
 * text, and the 30 s default timeout can be switched off rather than silently
 * capping a large transfer.
 */
describe("HttpClient", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  describe("timeout", () => {
    it("aborts a JSON request at the 30 s default", async () => {
      const { fetchFn, captured } = hangingFetch();
      const http = new HttpClient({ url: "http://api.test", fetch: fetchFn as typeof fetch });

      const promise = http.get("/x");
      const assertion = expect(promise).rejects.toMatchObject({
        name: "ImpresspressError",
        code: "timeout",
      });
      await vi.advanceTimersByTimeAsync(30_000);
      await assertion;
      expect(captured.signal?.aborted).toBe(true);
    });

    it("honours an explicit per-request timeout", async () => {
      const { fetchFn } = hangingFetch();
      const http = new HttpClient({ url: "http://api.test", fetch: fetchFn as typeof fetch });

      const promise = http.get("/x", { timeout: 1_000 });
      const assertion = expect(promise).rejects.toMatchObject({ code: "timeout" });
      await vi.advanceTimersByTimeAsync(1_000);
      await assertion;
    });

    it("arms no timer at all for timeout: 0, and still honours the caller's signal", async () => {
      const { fetchFn, captured } = hangingFetch();
      const http = new HttpClient({ url: "http://api.test", fetch: fetchFn as typeof fetch });
      const controller = new AbortController();

      const promise = http.get("/big", { timeout: 0, signal: controller.signal });
      const assertion = expect(promise).rejects.toMatchObject({ code: "aborted" });

      // Ten minutes of a slow transfer: nothing must have aborted it.
      await vi.advanceTimersByTimeAsync(600_000);
      expect(captured.signal?.aborted).toBe(false);

      controller.abort();
      await assertion;
    });
  });

  describe("request bodies", () => {
    it("sends a FormData body raw, with no JSON encoding and no Content-Type", async () => {
      const fetchFn = vi.fn().mockResolvedValue(fakeJsonResponse({ uploaded: true }));
      const http = new HttpClient({ url: "http://api.test", fetch: fetchFn as unknown as typeof fetch });

      const form = new FormData();
      form.append("file", new Blob(["hi"]), "f.txt");
      await http.request("POST", "/upload", form);

      const [, init] = fetchFn.mock.calls[0];
      expect(init.body).toBe(form);
      expect(init.headers["Content-Type"]).toBeUndefined();
    });

    it("still JSON-encodes a plain object body", async () => {
      const fetchFn = vi.fn().mockResolvedValue(fakeJsonResponse({}));
      const http = new HttpClient({ url: "http://api.test", fetch: fetchFn as unknown as typeof fetch });

      await http.post("/x", { a: 1 });

      const [, init] = fetchFn.mock.calls[0];
      expect(JSON.parse(init.body)).toEqual({ a: 1 });
      expect(init.headers["Content-Type"]).toBe("application/json");
    });
  });

  describe("headers the fold-in had to preserve", () => {
    it("sends no Content-Type on a download, so a cross-origin GET stays preflight-free", async () => {
      const fetchFn = vi.fn().mockResolvedValue(fakeBlobResponse("bytes", "application/octet-stream"));
      const http = new HttpClient({ url: "http://api.test", fetch: fetchFn as unknown as typeof fetch });

      await http.request("GET", "/f", undefined, { responseType: "blob" });

      const [, init] = fetchFn.mock.calls[0];
      // `application/json` is not a CORS-safelisted request header, so setting
      // it here would turn a simple cross-origin download into a preflighted
      // one. `requestBlob` sent no headers at all before the fold-in.
      expect(Object.keys(init.headers).map((k) => k.toLowerCase())).not.toContain("content-type");
    });

    it("drops a client-wide Content-Type for a raw body, so the multipart boundary survives", async () => {
      const fetchFn = vi.fn().mockResolvedValue(fakeJsonResponse({ ok: true }));
      const http = new HttpClient({
        url: "http://api.test",
        fetch: fetchFn as unknown as typeof fetch,
        headers: { "Content-Type": "application/json" },
      });

      const form = new FormData();
      form.append("file", "x");
      await http.request("POST", "/u", form);

      const [, init] = fetchFn.mock.calls[0];
      // fetch sets `multipart/form-data; boundary=...` itself; any
      // Content-Type we send survives and corrupts it.
      expect(Object.keys(init.headers).map((k) => k.toLowerCase())).not.toContain("content-type");
    });
  });

  describe("external signal lifecycle", () => {
    it("detaches its abort listener when the request completes", async () => {
      // A fresh response per call: a body can only be read once, and this
      // test issues three requests.
      const fetchFn = vi.fn().mockImplementation(() => Promise.resolve(fakeJsonResponse({ ok: true })));
      const http = new HttpClient({ url: "http://api.test", fetch: fetchFn as unknown as typeof fetch });
      const controller = new AbortController();

      const added: string[] = [];
      const removed: string[] = [];
      const realAdd = controller.signal.addEventListener.bind(controller.signal);
      const realRemove = controller.signal.removeEventListener.bind(controller.signal);
      controller.signal.addEventListener = ((t: string, ...rest: unknown[]) => {
        added.push(t);
        return (realAdd as Function)(t, ...rest);
      }) as typeof controller.signal.addEventListener;
      controller.signal.removeEventListener = ((t: string, ...rest: unknown[]) => {
        removed.push(t);
        return (realRemove as Function)(t, ...rest);
      }) as typeof controller.signal.removeEventListener;

      // Three requests on one long-lived controller — the shape the README
      // teaches for transfers. `{ once: true }` only self-removes when the
      // event FIRES, so without an explicit detach these accumulate.
      await http.request("GET", "/a", undefined, { signal: controller.signal });
      await http.request("GET", "/b", undefined, { signal: controller.signal });
      await http.request("GET", "/c", undefined, { signal: controller.signal });

      expect(added.filter((t) => t === "abort")).toHaveLength(3);
      expect(removed.filter((t) => t === "abort")).toHaveLength(3);
    });
  });

  describe("responseType", () => {
    it("returns the raw body for responseType 'blob' even when the server labels it JSON", async () => {
      const fetchFn = vi
        .fn()
        .mockResolvedValue(fakeBlobResponse('{"not":"an envelope"}', "application/json"));
      const http = new HttpClient({ url: "http://api.test", fetch: fetchFn as unknown as typeof fetch });

      const blob = await http.get("/objects/data.json", { responseType: "blob" });

      expect(blob).toBeInstanceOf(Blob);
      expect((blob as Blob).size).toBe('{"not":"an envelope"}'.length);
      expect((blob as Blob).type).toBe("application/json");
    });

    it("still decodes a JSON error body on a failed blob request", async () => {
      const fetchFn = vi
        .fn()
        .mockResolvedValue(fakeJsonResponse({ error: "NotFound", message: "no such object" }, 404));
      const http = new HttpClient({ url: "http://api.test", fetch: fetchFn as unknown as typeof fetch });

      await expect(http.get("/objects/gone", { responseType: "blob" })).rejects.toMatchObject({
        code: "NotFound",
        status: 404,
        message: "no such object",
      });
    });
  });

  describe("the statement-budget refusal", () => {
    // The exact bodies the server sends: `wafer_block::http_codec` renders the
    // error's detail code as `code` (see `crates/impresspress/tests/statement_budget_http.rs`).
    const refused = async (body: unknown, status: number) => {
      const fetchFn = vi.fn().mockResolvedValue(fakeJsonResponse(body, status));
      const http = new HttpClient({ url: "http://api.test", fetch: fetchFn as unknown as typeof fetch });
      return http.post("/b/auth/api/signup", {}).then(
        () => null,
        (e: unknown) => e,
      );
    };

    it("is told apart from a rate limit that shares its 429", async () => {
      const budget = await refused(
        {
          error: "ResourceExhausted",
          message: "batch runs 2 statements; this invocation has 1 of its 1000 left",
          code: "database.statement_budget_exhausted",
        },
        429,
      );
      expect(budget).toMatchObject({ status: 429, detailCode: "database.statement_budget_exhausted" });
      expect(isStatementBudgetError(budget)).toBe(true);

      const rateLimited = await refused(
        { error: "ResourceExhausted", message: "Too many requests", code: "rate_limit_exceeded" },
        429,
      );
      expect(isStatementBudgetError(rateLimited)).toBe(false);
    });

    it("covers the write larger than the whole limit", async () => {
      const tooLarge = await refused(
        {
          error: "InvalidArgument",
          message: "batch runs 2000 statements; the limit is 1000",
          code: "database.statement_budget_exceeds_limit",
        },
        400,
      );
      expect(isStatementBudgetError(tooLarge)).toBe(true);
      expect(isStatementBudgetError(new Error("x"))).toBe(false);
    });
  });

  it("rejects a method it cannot send with a machine-readable code", async () => {
    const fetchFn = vi.fn();
    const http = new HttpClient({ url: "http://api.test", fetch: fetchFn as unknown as typeof fetch });

    const error = await http
      .request("TRACE" as never, "/x")
      .then(() => null)
      .catch((e: unknown) => e);

    expect(error).toBeInstanceOf(ImpresspressError);
    expect((error as ImpresspressError).code).toBe("unsupported_method");
    expect(fetchFn).not.toHaveBeenCalled();
  });
});
