import { HttpClient } from "../http-client";
import { ImpresspressError } from "../error";
import { ImpresspressConfig } from "../types";

export interface RequestConfig {
  method: string;
  url: string;
  data?: any;
  headers?: Record<string, string>;
  params?: Record<string, any>;
}

export class BaseService {
  protected http: HttpClient;
  protected config: ImpresspressConfig;

  constructor(config: ImpresspressConfig) {
    this.config = config;
    this.http = new HttpClient({
      url: config.url,
      apiKey: config.apiKey,
      headers: config.headers,
      timeout: config.timeout,
    });
  }

  /**
   * The real server has no response envelope: a success response IS the
   * JSON value (`ok_json(&value)` serializes `value` directly, not
   * `{success, data: value}`), and a failure is a non-2xx status whose body
   * is `{error, message}` — already thrown as an `ImpresspressError` by
   * `HttpClient`. So this is a thin pass-through, not an unwrapper.
   */
  protected async request<T>(config: RequestConfig): Promise<T> {
    const options = config.headers || config.params ? { headers: config.headers, params: config.params } : undefined;
    switch (config.method.toUpperCase()) {
      case "GET":
        return this.http.get<T>(config.url, options);
      case "POST":
        return this.http.post<T>(config.url, config.data, options);
      case "PUT":
        return this.http.put<T>(config.url, config.data, options);
      case "PATCH":
        return this.http.patch<T>(config.url, config.data, options);
      case "DELETE":
        return this.http.delete<T>(config.url, options);
      default:
        throw new Error(`Unsupported HTTP method: ${config.method}`);
    }
  }

  /**
   * Send a FormData request (for file uploads). Uses `fetch` directly since
   * `HttpClient` always JSON-encodes the body.
   */
  protected async requestFormData<T>(
    url: string,
    formData: FormData,
    headers?: Record<string, string>,
  ): Promise<T> {
    const fullUrl = this.config.url + url;
    const fetchHeaders: Record<string, string> = { ...headers };
    if (this.config.apiKey) {
      fetchHeaders["Authorization"] = `Bearer ${this.config.apiKey}`;
    }
    // Don't set Content-Type — fetch auto-sets multipart/form-data with boundary.

    const fetchOpts: RequestInit = {
      method: "POST",
      headers: fetchHeaders,
      body: formData,
    };
    if (typeof globalThis.window !== "undefined") {
      fetchOpts.credentials = "include";
    }

    const res = await globalThis.fetch(fullUrl, fetchOpts);
    const contentType = res.headers.get("content-type") ?? "";
    const data = contentType.includes("application/json") ? await res.json() : null;

    if (!res.ok) {
      const body = (data ?? {}) as Record<string, unknown>;
      throw new ImpresspressError(
        typeof body.error === "string" ? body.error : "internal_error",
        typeof body.message === "string" ? body.message : `HTTP ${res.status}`,
        res.status,
        data,
      );
    }
    return data as T;
  }

  /** Fetch a response as a Blob (for file downloads). */
  protected async requestBlob(url: string): Promise<Blob> {
    const fullUrl = this.config.url + url;
    const headers: Record<string, string> = {};
    if (this.config.apiKey) {
      headers["Authorization"] = `Bearer ${this.config.apiKey}`;
    }

    const fetchOpts: RequestInit = { method: "GET", headers };
    if (typeof globalThis.window !== "undefined") {
      fetchOpts.credentials = "include";
    }

    const res = await globalThis.fetch(fullUrl, fetchOpts);
    if (!res.ok) {
      let message = `HTTP ${res.status}`;
      let code = "internal_error";
      const contentType = res.headers.get("content-type") ?? "";
      if (contentType.includes("application/json")) {
        try {
          const body = await res.json();
          if (typeof body?.message === "string") message = body.message;
          if (typeof body?.error === "string") code = body.error;
        } catch {
          // Body wasn't valid JSON despite the content-type — fall back to
          // the generic HTTP-status message above.
        }
      }
      throw new ImpresspressError(code, message, res.status);
    }
    return res.blob();
  }

  protected buildQueryString(params: Record<string, any>): string {
    const query = new URLSearchParams();
    Object.entries(params).forEach(([key, value]) => {
      if (value !== undefined && value !== null) {
        if (typeof value === "object") {
          query.append(key, JSON.stringify(value));
        } else {
          query.append(key, String(value));
        }
      }
    });
    return query.toString();
  }

  /**
   * Set API key for server-to-server authentication.
   * In browser environments, cookie-based auth is used automatically.
   */
  public setApiKey(apiKey: string) {
    this.config.apiKey = apiKey;
    this.http.setApiKey(apiKey);
  }

  /**
   * Remove API key (for server-to-server auth).
   * In browser environments, use logout() to clear the auth cookie.
   */
  public removeApiKey() {
    delete this.config.apiKey;
    this.http.removeApiKey();
  }
}
