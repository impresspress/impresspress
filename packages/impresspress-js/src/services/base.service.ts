import { WaferClient, WaferError } from "wafer-client-js";
import { ImpresspressConfig, ApiResponse } from "../types";

export interface RequestConfig {
  method: string;
  url: string;
  data?: any;
  headers?: Record<string, string>;
  params?: Record<string, any>;
  responseType?: string;
}

export class BaseService {
  protected wafer: WaferClient;
  protected config: ImpresspressConfig;

  constructor(config: ImpresspressConfig) {
    this.config = config;
    this.wafer = new WaferClient({
      url: config.url,
      apiKey: config.apiKey,
      headers: {
        "Content-Type": "application/json",
        ...config.headers,
      },
      timeout: config.timeout || 30000,
      credentials:
        typeof globalThis.window !== "undefined" ? "include" : undefined,
    });
  }

  protected async request<T>(config: RequestConfig): Promise<T> {
    // Build the full API path
    let path = config.url;

    // Append params as query string
    if (config.params) {
      const qs = this.buildQueryString(config.params);
      if (qs) {
        path += (path.includes("?") ? "&" : "?") + qs;
      }
    }

    try {
      const options = config.headers ? { headers: config.headers } : undefined;
      let result;

      switch (config.method.toUpperCase()) {
        case "GET":
          result = await this.wafer.get<ApiResponse<T>>(path, options);
          break;
        case "POST":
          result = await this.wafer.post<ApiResponse<T>>(
            path,
            config.data,
            options,
          );
          break;
        case "PUT":
          result = await this.wafer.put<ApiResponse<T>>(
            path,
            config.data,
            options,
          );
          break;
        case "PATCH":
          result = await this.wafer.patch<ApiResponse<T>>(
            path,
            config.data,
            options,
          );
          break;
        case "DELETE":
          result = await this.wafer.delete<ApiResponse<T>>(path, options);
          break;
        default:
          throw new Error(`Unsupported HTTP method: ${config.method}`);
      }

      const apiResp = result.data;
      if (apiResp?.success === false) {
        throw apiResp;
      }
      return (apiResp?.data !== undefined ? apiResp.data : apiResp) as T;
    } catch (error) {
      if (error instanceof WaferError) {
        const apiError: ApiResponse = {
          success: false,
          error: {
            code: error.code || "UNKNOWN_ERROR",
            message: error.message,
            details: error.data,
          },
        };
        throw apiError;
      }
      throw error;
    }
  }

  /**
   * Send a FormData request (for file uploads).
   * Uses fetch directly since WaferClient's transport JSON-stringifies bodies.
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
    // Don't set Content-Type — fetch auto-sets multipart/form-data with boundary

    const fetchOpts: RequestInit = {
      method: "POST",
      headers: fetchHeaders,
      body: formData,
    };
    if (typeof globalThis.window !== "undefined") {
      fetchOpts.credentials = "include";
    }

    const res = await globalThis.fetch(fullUrl, fetchOpts);
    const data = await res.json();

    if (!res.ok || data.success === false) {
      const apiError: ApiResponse = {
        success: false,
        error: {
          code: data?.error?.code || data?.error || "UPLOAD_ERROR",
          message:
            data?.error?.message || data?.message || `HTTP ${res.status}`,
          details: data,
        },
      };
      throw apiError;
    }
    return (data?.data !== undefined ? data.data : data) as T;
  }

  /**
   * Fetch a response as a Blob (for file downloads).
   */
  protected async requestBlob(url: string): Promise<Blob> {
    const fullUrl = this.config.url + url;
    const headers: Record<string, string> = {};

    if (this.config.apiKey) {
      headers["Authorization"] = `Bearer ${this.config.apiKey}`;
    }

    const fetchOpts: RequestInit = {
      method: "GET",
      headers,
    };
    if (typeof globalThis.window !== "undefined") {
      fetchOpts.credentials = "include";
    }

    const res = await globalThis.fetch(fullUrl, fetchOpts);
    if (!res.ok) {
      const apiError: ApiResponse = {
        success: false,
        error: {
          code: "DOWNLOAD_ERROR",
          message: `HTTP ${res.status}`,
        },
      };
      throw apiError;
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
    this.wafer.setApiKey(apiKey);
  }

  /**
   * Remove API key (for server-to-server auth).
   * In browser environments, use logout() to clear the auth cookie.
   */
  public removeApiKey() {
    delete this.config.apiKey;
    this.wafer.removeApiKey();
  }
}
