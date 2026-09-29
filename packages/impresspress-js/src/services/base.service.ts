import { HttpClient, HttpMethod } from "../http-client";
import { ImpresspressConfig } from "../types";

export interface RequestConfig {
  method: HttpMethod;
  url: string;
  data?: unknown;
  headers?: Record<string, string>;
  params?: Record<string, unknown>;
  /** Milliseconds before the request aborts; `NO_TIMEOUT` (0) disables it. */
  timeout?: number;
  signal?: AbortSignal;
  /** `"blob"` returns the raw response bytes instead of parsed JSON. */
  responseType?: "json" | "blob";
}

/**
 * Shared plumbing for the six services. Every service is handed the SAME
 * `HttpClient` by `ImpresspressClient`, so the bearer key, headers,
 * credentials and timeout policy are configured in exactly one place — there
 * is no per-service copy of the transport to keep in sync.
 */
export class BaseService {
  protected config: ImpresspressConfig;
  protected http: HttpClient;

  constructor(config: ImpresspressConfig, http: HttpClient) {
    this.config = config;
    this.http = http;
  }

  /**
   * The real server has no response envelope: a success response IS the
   * JSON value (`ok_json(&value)` serializes `value` directly, not
   * `{success, data: value}`), and a failure is a non-2xx status whose body
   * is `{error, message}` — already thrown as an `ImpresspressError` by
   * `HttpClient`. So this is a thin pass-through, not an unwrapper.
   */
  protected request<T>(config: RequestConfig): Promise<T> {
    const { method, url, data, ...options } = config;
    return this.http.request<T>(method, url, data, options);
  }

  /**
   * Set API key for server-to-server authentication. Applies to every
   * service on the client — they share one transport.
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
