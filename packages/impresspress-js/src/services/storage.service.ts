import { BaseService } from "./base.service";

/**
 * Aligned to the REAL dispatch table in
 * `crates/impresspress-core/src/blocks/files/storage.rs` (`const ROUTES`),
 * which is the single source of truth for the on-the-wire
 * `/b/storage/api/...` surface. That table only supports:
 *
 *   GET    /b/storage/api/buckets
 *   POST   /b/storage/api/buckets
 *   DELETE /b/storage/api/buckets/{name}
 *   GET    /b/storage/api/buckets/{name}/objects
 *   POST   /b/storage/api/buckets/{name}/objects
 *   GET    /b/storage/api/buckets/{name}/objects/{key...}
 *   DELETE /b/storage/api/buckets/{name}/objects/{key...}
 *   GET    /b/storage/api/search?q=
 *   GET    /b/storage/api/recent
 *
 * There is no folder/rename/move/metadata-update/quota/stats/trash surface
 * under `/b/storage/api` — objects are addressed by `key` (which may
 * contain `/`), not by an opaque id, and there is no `id` field on the
 * bucket or object wire shapes at all. Per-object sharing and quota DO
 * exist, but under `/b/cloudstorage/*` — see `CloudStorageExtension` in
 * `extensions.service.ts`.
 */

export interface StorageObjectInfo {
  key: string;
  size: number;
  content_type: string;
  last_modified: string;
}

export interface ListObjectsResult {
  objects: StorageObjectInfo[];
  total_count: number;
}

export interface ListOptions {
  /** Key prefix filter. */
  prefix?: string;
  page?: number;
  page_size?: number;
}

export interface UploadFileOptions {
  /** Object key. Required unless `file` is a `File` (its `.name` is used as a fallback). */
  key?: string;
  contentType?: string;
}

export class StorageService extends BaseService {
  /** List bucket names owned by the current user (or every bucket, for an admin). */
  async listBuckets(): Promise<string[]> {
    const res = await this.request<{ buckets: string[] }>({
      method: "GET",
      url: "/b/storage/api/buckets",
    });
    return res.buckets;
  }

  /** Create a new bucket. */
  async createBucket(
    name: string,
    isPublic = false,
  ): Promise<{ name: string; created: boolean }> {
    return this.request({
      method: "POST",
      url: "/b/storage/api/buckets",
      data: { name, public: isPublic },
    });
  }

  /** Delete a bucket and its objects. */
  async deleteBucket(name: string): Promise<void> {
    await this.request<{ deleted: boolean }>({
      method: "DELETE",
      url: `/b/storage/api/buckets/${encodeURIComponent(name)}`,
    });
  }

  /** List objects in a bucket. */
  async listObjects(bucketName: string, options?: ListOptions): Promise<ListObjectsResult> {
    const queryString = options ? this.buildQueryString(options) : "";
    return this.request<ListObjectsResult>({
      method: "GET",
      url: `/b/storage/api/buckets/${encodeURIComponent(bucketName)}/objects${queryString ? `?${queryString}` : ""}`,
    });
  }

  /** Download an object's raw bytes. */
  async downloadFile(bucketName: string, key: string): Promise<Blob> {
    return this.requestBlob(
      `/b/storage/api/buckets/${encodeURIComponent(bucketName)}/objects/${encodeObjectKey(key)}`,
    );
  }

  /** Direct URL for downloading an object (e.g. for `<img src>` / `<a href>`). */
  getDownloadUrl(bucketName: string, key: string): string {
    return `${this.config.url}/b/storage/api/buckets/${encodeURIComponent(bucketName)}/objects/${encodeObjectKey(key)}`;
  }

  /**
   * Upload a file to a bucket. `options.key` is required unless `file` is a
   * `File` (browser), whose `.name` is used as a fallback — mirrors the
   * server's multipart handling in `handle_upload_object`.
   */
  async uploadFile(
    bucketName: string,
    file: File | Buffer | Blob,
    options?: UploadFileOptions,
  ): Promise<{ bucket: string; key: string; uploaded: boolean }> {
    const formData = new FormData();

    if (typeof globalThis.window !== "undefined" && file instanceof File) {
      formData.append("file", file);
    } else if (file instanceof Blob) {
      formData.append("file", file, options?.key ?? "file");
    } else if (typeof Buffer !== "undefined" && Buffer.isBuffer(file)) {
      formData.append("file", new Blob([new Uint8Array(file)]), options?.key ?? "file");
    } else {
      throw new Error("Invalid file type");
    }

    const keyQuery = options?.key ? `?key=${encodeURIComponent(options.key)}` : "";
    return this.requestFormData(
      `/b/storage/api/buckets/${encodeURIComponent(bucketName)}/objects${keyQuery}`,
      formData,
    );
  }

  /** Delete an object. */
  async deleteObject(bucketName: string, key: string): Promise<void> {
    await this.request<{ deleted: boolean }>({
      method: "DELETE",
      url: `/b/storage/api/buckets/${encodeURIComponent(bucketName)}/objects/${encodeObjectKey(key)}`,
    });
  }

  /** Delete multiple objects (sequential — there is no bulk-delete route). */
  async deleteObjects(bucketName: string, keys: string[]): Promise<void> {
    for (const key of keys) {
      await this.deleteObject(bucketName, key);
    }
  }

  /**
   * Search the current user's completed uploads by key substring.
   * `GET /b/storage/api/search?q=`
   */
  async search(
    query: string,
    options?: { page?: number; page_size?: number },
  ): Promise<{ data: StorageObjectInfo[]; total?: number }> {
    const params = { q: query, ...options };
    return this.request({
      method: "GET",
      url: `/b/storage/api/search?${this.buildQueryString(params)}`,
    });
  }

  /**
   * The 20 most recently viewed objects for the current user.
   * `GET /b/storage/api/recent` — takes no query parameters server-side.
   */
  async getRecentFiles(): Promise<{ data: StorageObjectInfo[]; total?: number }> {
    return this.request({
      method: "GET",
      url: "/b/storage/api/recent",
    });
  }
}

/**
 * Encode an object key for use as a path segment. Keys may contain `/`
 * (the server binds them via a `{key...}` rest param, not a single
 * segment) — encode each segment individually so the slashes survive.
 */
function encodeObjectKey(key: string): string {
  return key.split("/").map(encodeURIComponent).join("/");
}
