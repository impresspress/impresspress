import { BaseService } from "./base.service";

export interface Extension {
  name: string;
  version: string;
  description: string;
  author: string;
  enabled: boolean;
  config?: Record<string, any>;
  metadata?: {
    tags?: string[];
    homepage?: string;
    license?: string;
  };
}

export class ExtensionsService extends BaseService {
  /** List all available extensions (registered blocks). `GET /b/admin/api/extensions`. */
  async list(): Promise<Extension[]> {
    return this.request<Extension[]>({
      method: "GET",
      url: "/b/admin/api/extensions",
    });
  }

  /**
   * Call an arbitrary block endpoint at `/b/{extension}/{endpoint}`. This is
   * a raw passthrough — there is no generic extension lifecycle API
   * (enable/disable/configure/health) server-side, only each block's own
   * declared HTTP routes, which this method reaches directly.
   */
  async call<T = any>(
    extension: string,
    endpoint: string,
    options?: {
      method?: "GET" | "POST" | "PUT" | "DELETE" | "PATCH";
      data?: any;
      params?: Record<string, any>;
    },
  ): Promise<T> {
    const queryString = options?.params ? this.buildQueryString(options.params) : "";
    return this.request<T>({
      method: options?.method || "GET",
      url: `/b/${extension}/${endpoint}${queryString ? `?${queryString}` : ""}`,
      data: options?.data,
    });
  }
}

/**
 * One row of the `impresspress__files__cloud_shares` table (see
 * `crates/impresspress-core/src/blocks/files/repo/shares.rs`), flattened
 * from the wire `Record { id, data }` shape (`id` + the row's columns).
 */
export interface ShareRecord {
  id: string;
  token: string;
  bucket: string;
  key: string;
  created_by: string;
  created_at: string;
  access_count: number;
  expires_at?: string;
  max_access_count?: number;
}

export interface ListSharesResult {
  items: ShareRecord[];
  total: number;
}

/**
 * Aligned to the real `impresspress/files` cloud-storage surface in
 * `crates/impresspress-core/src/blocks/files/cloud.rs`: per-object share
 * links and the caller's own quota/usage. There is no user-facing
 * access-log or access-stats endpoint (`/admin/b/cloudstorage/access-logs`
 * is admin-only and reached through the admin block's delegated HTTP
 * surface, not this one; `access-stats` does not exist at all) — both were
 * removed rather than pointed at a route that would 404 or silently expose
 * the wrong auth boundary.
 */
export class CloudStorageExtension extends ExtensionsService {
  /** Create a share link for an object. `POST /b/cloudstorage/shares`. */
  async share(
    bucket: string,
    key: string,
    options?: { expiresInHours?: number; maxAccessCount?: number },
  ): Promise<{ id: string; token: string; direct_url: string }> {
    return this.call("cloudstorage", "shares", {
      method: "POST",
      data: {
        bucket,
        key,
        expires_in_hours: options?.expiresInHours,
        max_access_count: options?.maxAccessCount,
      },
    });
  }

  /**
   * List the current user's shares. `GET /b/cloudstorage/shares`.
   *
   * The handler serializes wafer-core's `RecordList` directly
   * (`ok_json(&result)` over `repo::shares::list_for_user`) — `{ records,
   * total_count, page, page_size }`, NOT a `{ data, total }` envelope. See
   * `wafer-block/src/wire/database.rs`.
   */
  async listShares(): Promise<ListSharesResult> {
    const result = await this.call<{
      records: Array<{ id: string; data: Omit<ShareRecord, "id"> }>;
      total_count: number;
      page: number;
      page_size: number;
    }>("cloudstorage", "shares");
    return {
      items: result.records.map((r) => ({ id: r.id, ...r.data })),
      total: result.total_count,
    };
  }

  /** Delete a share. `DELETE /b/cloudstorage/shares/{id}`. */
  async deleteShare(shareId: string): Promise<void> {
    await this.call("cloudstorage", `shares/${encodeURIComponent(shareId)}`, {
      method: "DELETE",
    });
  }

  /** Get the current user's storage quota and usage. `GET /b/cloudstorage/quota`. */
  async getQuota(): Promise<{
    quota: {
      max_storage_bytes: number;
      max_file_size_bytes: number;
      max_files_per_bucket: number;
      reset_period_days: number;
    };
    usage: Record<string, unknown>;
  }> {
    return this.call("cloudstorage", "quota");
  }
}

/**
 * Aligned to the real `impresspress/products` HTTP surface in
 * `crates/impresspress-core/src/blocks/products/mod.rs`. There is no public
 * (non-admin) product/group creation or price-calculation route — the
 * catalog is the only public listing surface, and mutations go through the
 * `Admin`-gated `/b/products/api/admin/*` routes (the server enforces the
 * auth tier; this SDK does not).
 */
export class ProductsExtension extends ExtensionsService {
  /** Browse the public product catalog. `GET /b/products/catalog`. */
  async listProducts(options?: { page?: number; page_size?: number }): Promise<{
    records: unknown[];
    total_count: number;
    page: number;
    page_size: number;
  }> {
    return this.call("products", "catalog", { params: options });
  }

  /** Create a product (admin). `POST /b/products/api/admin/products`. */
  async createProduct(data: {
    group_id: string;
    name: string;
    template_id?: string;
    custom_fields?: Record<string, any>;
    pricing_formula?: string;
  }): Promise<any> {
    return this.call("products", "api/admin/products", {
      method: "POST",
      data,
    });
  }

  /** List product groups (admin). `GET /b/products/api/admin/groups`. */
  async listGroups(): Promise<any[]> {
    return this.call("products", "api/admin/groups");
  }

  /** Create a product group (admin). `POST /b/products/api/admin/groups`. */
  async createGroup(data: {
    name: string;
    template_id: string;
    custom_fields?: Record<string, any>;
  }): Promise<any> {
    return this.call("products", "api/admin/groups", {
      method: "POST",
      data,
    });
  }
}
