/**
 * Minimal fetch-based HTTP client for talking to a single impresspress
 * server. This SDK previously depended on `wafer-client-js` via a local
 * `file:` path into the sibling `wafer-run` checkout, which meant `npm
 * install` could not succeed outside that exact monorepo layout (and
 * `npm pack && npm install` in a clean directory failed outright).
 *
 * `wafer-client-js` is a generic multi-backend Wafer transport; this SDK
 * only ever talks to one impresspress HTTP server and only needs
 * get/post/put/patch/delete with a bearer API key and cookie credentials —
 * so rather than re-adding an external dependency (versioned or git-based)
 * this is a small, self-contained client with zero runtime dependencies.
 *
 * Wire format: success responses are the raw JSON body with no envelope
 * (`ok_json(&value)` on the server just serializes `value`); error
 * responses are `{ "error": "<code>", "message": "<msg>" }` — see
 * `wafer_block::http_codec::collect_http_response` on the server side.
 */
import { ImpresspressError } from "./error";

export interface HttpClientConfig {
  url: string;
  apiKey?: string;
  headers?: Record<string, string>;
  timeout?: number;
  /** Override for `fetch` (tests / non-browser environments). */
  fetch?: typeof fetch;
  /** Override for request credentials. Defaults to 'include' in a browser. */
  credentials?: RequestCredentials;
}

export interface HttpRequestOptions {
  headers?: Record<string, string>;
  params?: Record<string, unknown>;
  timeout?: number;
  signal?: AbortSignal;
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

export class HttpClient {
  private config: HttpClientConfig;

  constructor(config: HttpClientConfig) {
    this.config = { ...config, url: config.url.replace(/\/$/, "") };
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

  private async request<T>(
    method: string,
    path: string,
    data: unknown,
    options?: HttpRequestOptions,
  ): Promise<T> {
    const fetchFn = this.config.fetch ?? globalThis.fetch;
    const timeout = options?.timeout ?? this.config.timeout ?? 30_000;
    const url = `${this.config.url}${path}${buildQueryString(options?.params)}`;

    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      ...this.config.headers,
      ...options?.headers,
    };
    if (this.config.apiKey) {
      headers["Authorization"] = `Bearer ${this.config.apiKey}`;
    }

    const body = data !== undefined ? JSON.stringify(data) : undefined;

    const controller = new AbortController();
    const externalSignal = options?.signal;
    let timeoutId: ReturnType<typeof setTimeout> | undefined;
    if (externalSignal?.aborted) {
      controller.abort(externalSignal.reason);
    } else {
      externalSignal?.addEventListener("abort", () => controller.abort(externalSignal.reason), {
        once: true,
      });
      timeoutId = setTimeout(() => controller.abort("timeout"), timeout);
    }

    const credentials = this.config.credentials ?? defaultCredentials();

    let res: Response;
    try {
      res = await fetchFn(url, {
        method,
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
    }

    const contentType = res.headers.get("content-type") ?? "";
    let parsed: unknown = null;
    if (contentType.includes("application/json")) {
      const raw = await res.text();
      if (raw.length > 0) {
        try {
          parsed = JSON.parse(raw);
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
        const body = parsed as Record<string, unknown>;
        if (typeof body.error === "string") code = body.error;
        if (typeof body.message === "string") message = body.message;
        if (typeof body.code === "string") detailCode = body.code;
      }
      throw new ImpresspressError(code, message, res.status, parsed, detailCode);
    }

    return parsed as T;
  }
}
