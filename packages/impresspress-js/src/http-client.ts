/**
 * Minimal fetch-based HTTP client for talking to a single impresspress
 * server. This SDK previously depended on `wafer-client-js` via a local
 * `file:` path into the sibling `wafer-run` checkout, which meant `npm
 * install` could not succeed outside that exact monorepo layout (and
 * `npm pack && npm install` in a clean directory failed outright).
 *
 * `wafer-client-js` is a generic multi-backend Wafer transport; this SDK
 * only ever talks to one impresspress HTTP server — so rather than
 * re-adding an external dependency (versioned or git-based) this is a
 * small, self-contained client with zero runtime dependencies.
 *
 * It is the ONLY request path in the SDK: JSON calls, multipart uploads and
 * raw downloads all go through `request`, so credentials, the bearer key,
 * query-string encoding, abort handling and error decoding are defined once.
 *
 * Wire format: success responses are the raw JSON body with no envelope
 * (`ok_json(&value)` on the server just serializes `value`); error
 * responses are `{ "error": "<code>", "message": "<msg>" }` — see
 * `wafer_block::http_codec::collect_http_response` on the server side.
 */
import { ImpresspressError } from "./error";

/** The methods this client can send. Anything else is refused up front. */
export type HttpMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";

const HTTP_METHODS: readonly string[] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

/** Default timeout for a JSON control-plane call, in milliseconds. */
export const DEFAULT_TIMEOUT_MS = 30_000;

/**
 * `timeout` value that disables the timeout entirely: the request runs until
 * the server ends it or the caller's `signal` fires. Used by the transfer
 * paths (upload/download), whose duration is a function of file size and
 * link speed and so has no defensible fixed ceiling.
 */
export const NO_TIMEOUT = 0;

export interface HttpClientConfig {
  url: string;
  apiKey?: string;
  headers?: Record<string, string>;
  /**
   * Default request timeout in milliseconds. `NO_TIMEOUT` (0) disables it.
   * Defaults to `DEFAULT_TIMEOUT_MS`.
   */
  timeout?: number;
  /** Override for `fetch` (tests / non-browser environments). */
  fetch?: typeof fetch;
  /** Override for request credentials. Defaults to 'include' in a browser. */
  credentials?: RequestCredentials;
}

export interface HttpRequestOptions {
  headers?: Record<string, string>;
  params?: Record<string, unknown>;
  /**
   * Milliseconds before this request is aborted. `NO_TIMEOUT` (0) disables
   * the timeout for this request only. Falls back to the client's `timeout`,
   * then to `DEFAULT_TIMEOUT_MS`.
   */
  timeout?: number;
  signal?: AbortSignal;
  /**
   * `"json"` (the default) parses the response body as JSON; `"blob"`
   * returns the raw bytes untouched, which is what a file download needs
   * even when the object's own content-type happens to be JSON.
   */
  responseType?: "json" | "blob";
}

function defaultCredentials(): RequestCredentials | undefined {
  return typeof globalThis.window !== "undefined" ? "include" : undefined;
}

function buildQueryString(params?: Record<string, unknown>): string {
  if (!params) return "";
  const qs = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === null) continue;
    qs.append(key, typeof value === "object" ? JSON.stringify(value) : String(value));
  }
  const s = qs.toString();
  return s ? `?${s}` : "";
}

/**
 * True for a value `fetch` can send as-is. These are the `BodyInit` members
 * the SDK can encounter; everything else (plain objects, strings, numbers)
 * is JSON-encoded, which is what every impresspress JSON endpoint wants.
 * A raw body must NOT be JSON-encoded and must NOT carry an explicit
 * `Content-Type`, so `fetch` can set the multipart boundary itself.
 */
function isRawBody(data: unknown): data is BodyInit {
  return (
    (typeof FormData !== "undefined" && data instanceof FormData) ||
    (typeof Blob !== "undefined" && data instanceof Blob) ||
    (typeof URLSearchParams !== "undefined" && data instanceof URLSearchParams) ||
    (typeof ArrayBuffer !== "undefined" &&
      (data instanceof ArrayBuffer || ArrayBuffer.isView(data)))
  );
}

export class HttpClient {
  private config: HttpClientConfig;

  constructor(config: HttpClientConfig) {
    this.config = { ...config, url: config.url.replace(/\/$/, "") };
  }

  /** Base URL this client talks to, with any trailing slash stripped. */
  get baseUrl(): string {
    return this.config.url;
  }

  setApiKey(apiKey: string): void {
    this.config.apiKey = apiKey;
  }

  removeApiKey(): void {
    delete this.config.apiKey;
  }

  get<T = unknown>(path: string, options?: HttpRequestOptions): Promise<T> {
    return this.request<T>("GET", path, undefined, options);
  }

  post<T = unknown>(path: string, data?: unknown, options?: HttpRequestOptions): Promise<T> {
    return this.request<T>("POST", path, data, options);
  }

  put<T = unknown>(path: string, data?: unknown, options?: HttpRequestOptions): Promise<T> {
    return this.request<T>("PUT", path, data, options);
  }

  patch<T = unknown>(path: string, data?: unknown, options?: HttpRequestOptions): Promise<T> {
    return this.request<T>("PATCH", path, data, options);
  }

  delete<T = unknown>(path: string, options?: HttpRequestOptions): Promise<T> {
    return this.request<T>("DELETE", path, undefined, options);
  }

  async request<T>(
    method: HttpMethod,
    path: string,
    data?: unknown,
    options?: HttpRequestOptions,
  ): Promise<T> {
    const normalized = String(method).toUpperCase();
    if (!HTTP_METHODS.includes(normalized)) {
      throw new ImpresspressError(
        "unsupported_method",
        `Unsupported HTTP method: ${method}. Expected one of ${HTTP_METHODS.join(", ")}.`,
      );
    }

    const fetchFn = this.config.fetch ?? globalThis.fetch;
    const timeout = options?.timeout ?? this.config.timeout ?? DEFAULT_TIMEOUT_MS;
    const url = `${this.config.url}${path}${buildQueryString(options?.params)}`;

    const raw = isRawBody(data);
    // A download asks for bytes and sends none. `requestBlob` sent no content
    // type at all before these paths were folded together, and restoring that
    // is not cosmetic: `application/json` is not CORS-safelisted, so adding it
    // turns a cross-origin download from a simple request into a preflighted
    // one. A raw body carries its own type (fetch adds the multipart
    // boundary), so the JSON default would corrupt it. Every OTHER request
    // keeps the JSON content type, bodyless ones included — that is what the
    // SDK has always sent and what `services.test.ts` pins.
    const download = options?.responseType === "blob";
    const headers: Record<string, string> = {
      ...(raw || download ? {} : { "Content-Type": "application/json" }),
      ...this.config.headers,
      ...options?.headers,
    };
    // `config.headers` is merged after the default above, so a client-wide
    // `Content-Type` would survive and corrupt the multipart boundary fetch
    // is about to set. `requestFormData` ignored `config.headers` entirely;
    // dropping just the one key keeps every other client header working.
    if (raw) {
      for (const key of Object.keys(headers)) {
        if (key.toLowerCase() === "content-type") delete headers[key];
      }
    }
    if (this.config.apiKey) {
      headers["Authorization"] = `Bearer ${this.config.apiKey}`;
    }

    let body: BodyInit | undefined;
    if (raw) {
      body = data as BodyInit;
    } else if (data !== undefined) {
      body = JSON.stringify(data);
    }

    const controller = new AbortController();
    const externalSignal = options?.signal;
    let timeoutId: ReturnType<typeof setTimeout> | undefined;
    // Named so the `finally` can detach it. `{ once: true }` only self-removes
    // when the event FIRES; on a request that completes normally the listener
    // would stay on the caller's signal forever, holding this request's
    // controller. The README teaches one long-lived controller across many
    // transfers, which is exactly the shape that accumulates them.
    const onExternalAbort = () => controller.abort(externalSignal!.reason);
    if (externalSignal?.aborted) {
      controller.abort(externalSignal.reason);
    } else {
      externalSignal?.addEventListener("abort", onExternalAbort, { once: true });
      if (timeout > 0) {
        timeoutId = setTimeout(() => controller.abort("timeout"), timeout);
      }
    }

    const credentials = this.config.credentials ?? defaultCredentials();

    let res: Response;
    try {
      res = await fetchFn(url, {
        method: normalized,
        headers,
        body,
        signal: controller.signal,
        ...(credentials ? { credentials } : {}),
      });
    } catch (err: unknown) {
      if (err instanceof DOMException && err.name === "AbortError") {
        if (externalSignal?.aborted) {
          throw new ImpresspressError("aborted", "Request aborted");
        }
        throw new ImpresspressError("timeout", `Request timed out after ${timeout}ms`);
      }
      const message = err instanceof Error ? err.message : "Network request failed";
      throw new ImpresspressError("network_error", message);
    } finally {
      if (timeoutId !== undefined) clearTimeout(timeoutId);
      externalSignal?.removeEventListener("abort", onExternalAbort);
    }

    // A blob response is handed back untouched. This has to happen BEFORE any
    // body read: a response body can only be consumed once, and an object
    // whose own content-type is `application/json` would otherwise be eaten
    // by the JSON branch below and never reach the caller.
    if (res.ok && options?.responseType === "blob") {
      return (await res.blob()) as T;
    }

    const contentType = res.headers.get("content-type") ?? "";
    let parsed: unknown = null;
    if (contentType.includes("application/json")) {
      const rawText = await res.text();
      if (rawText.length > 0) {
        try {
          parsed = JSON.parse(rawText);
        } catch {
          parsed = null;
        }
      }
    }

    if (!res.ok) {
      let code = "internal_error";
      let message = `HTTP ${res.status}`;
      let detailCode: string | undefined;
      if (parsed && typeof parsed === "object") {
        const errorBody = parsed as Record<string, unknown>;
        if (typeof errorBody.error === "string") code = errorBody.error;
        if (typeof errorBody.message === "string") message = errorBody.message;
        if (typeof errorBody.code === "string") detailCode = errorBody.code;
      }
      throw new ImpresspressError(code, message, res.status, parsed, detailCode);
    }

    return parsed as T;
  }
}
