import { BaseService } from "./base.service";
import { NO_TIMEOUT } from "../http-client";
import { ImpresspressError } from "../error";

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

/**
 * Bounds for a byte transfer. Uploads and downloads deliberately run with NO
 * timeout by default — their duration is a function of file size and link
 * speed, so any fixed ceiling is a cap on how big a file the SDK can move.
 * Pass `timeout` to impose one, and/or `signal` to cancel.
 */
export interface TransferOptions {
  /** Milliseconds before the transfer aborts. Omitted means: no limit. */
  timeout?: number;
  /** Cancel the transfer (the only bound that applies by default). */
  signal?: AbortSignal;
}

export interface UploadFileOptions extends TransferOptions {
  /** Object key. Required unless `file` is a `File` (its `.name` is used as a fallback). */
  key?: string;
  contentType?: string;
}

/**
 * One row of the `impresspress__files__objects` metadata table (see
 * `crates/impresspress-core/src/blocks/files/repo/objects.rs`), flattened
 * from the wire `Record { id, data }` shape (`id` + the row's columns).
 */
export interface FileMetadataRecord {
  id: string;
  bucket: string;
  key: string;
  size: number;
  content_type: string;
  /**
   * `pending` while the upload is in flight, `complete` once the blob is in
   * storage. The two values the server's `ObjectStatus` enum defines — it is
   * the type of the column now, so a row can hold nothing else.
   *
   * Search and recent listings only ever return `complete` rows; `pending`
   * reaches a client only through a listing that does not filter on status.
   */
  status: 'pending' | 'complete';
  uploaded_by: string;
  uploaded_at: string;
}

/**
 * One row of the `impresspress__files__views` object-view audit table (see
 * `crates/impresspress-core/src/blocks/files/repo/views.rs`), flattened from
 * the wire `Record { id, data }` shape (`id` + the row's columns).
 *
 * This — not [`FileMetadataRecord`] — is what `/b/storage/api/recent`
 * returns: `handle_recent` pages `repo::views::list_recent_for_user`, one row
 * per tracked download, so the response carries the viewer and the view
 * instant rather than the object's size, type or upload state.
 */
export interface FileViewRecord {
  id: string;
  /** Bucket holding the viewed object. */
  bucket: string;
  /** Object key within the bucket. */
  key: string;
  /** The viewer. */
  user_id: string;
  /** RFC 3339 instant of the view. */
  viewed_at: string;
  created_at: string;
  updated_at: string;
}

/** The flattened form of the server's `RecordList` envelope. */
export interface RecordListResult<T> {
  items: T[];
  total: number;
}

/** `search`'s result: object-metadata rows. */
export type SearchResult = RecordListResult<FileMetadataRecord>;

/** `getRecentFiles`'s result: object-view audit rows. */
export type RecentViewsResult = RecordListResult<FileViewRecord>;

/**
 * Wire shape of wafer-core's `RecordList` (see
 * `wafer-block/src/wire/database.rs`): `{ records, total_count, page,
 * page_size }`. `/b/storage/api/search` and `/b/storage/api/recent` both
 * serialize a `RecordList` directly (`ok_json(&result)`), NOT a `{ data,
 * total }` envelope.
 */
interface RecordListWire<T> {
  records: Array<{ id: string; data: T }>;
  total_count: number;
  page: number;
  page_size: number;
}

function flattenRecordList<T extends object>(
  result: RecordListWire<T>,
): RecordListResult<{ id: string } & T> {
  return {
    items: result.records.map((r) => ({ id: r.id, ...r.data })),
    total: result.total_count,
  };
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
    return this.request<ListObjectsResult>({
      method: "GET",
      url: `/b/storage/api/buckets/${encodeURIComponent(bucketName)}/objects`,
      params: options as Record<string, unknown> | undefined,
    });
  }

  /**
   * Download an object's raw bytes. Runs with no timeout unless
   * `options.timeout` sets one — see `TransferOptions`.
   */
  async downloadFile(bucketName: string, key: string, options?: TransferOptions): Promise<Blob> {
    return this.request<Blob>({
      method: "GET",
      url: `/b/storage/api/buckets/${encodeURIComponent(bucketName)}/objects/${encodeObjectKey(key)}`,
      responseType: "blob",
      timeout: options?.timeout ?? NO_TIMEOUT,
      signal: options?.signal,
    });
  }

  /** Direct URL for downloading an object (e.g. for `<img src>` / `<a href>`). */
  getDownloadUrl(bucketName: string, key: string): string {
    return `${this.config.url}/b/storage/api/buckets/${encodeURIComponent(bucketName)}/objects/${encodeObjectKey(key)}`;
  }

  /**
   * Upload a file to a bucket. `options.key` is required unless `file` is a
   * `File` (browser), whose `.name` is used as a fallback — mirrors the
   * server's multipart handling in `handle_upload_object`.
   *
   * Runs with no timeout unless `options.timeout` sets one — see
   * `TransferOptions`.
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
      throw new ImpresspressError(
        "invalid_file_type",
        "Invalid file type: expected a File, Blob, or Buffer",
      );
    }

    return this.request({
      method: "POST",
      url: `/b/storage/api/buckets/${encodeURIComponent(bucketName)}/objects`,
      data: formData,
      params: options?.key ? { key: options.key } : undefined,
      timeout: options?.timeout ?? NO_TIMEOUT,
      signal: options?.signal,
    });
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
  ): Promise<SearchResult> {
    const result = await this.request<
      RecordListWire<Omit<FileMetadataRecord, "id">>
    >({
      method: "GET",
      url: "/b/storage/api/search",
      params: { q: query, ...options },
    });
    return flattenRecordList(result);
  }

  /**
   * The current user's 20 most recent object views, newest first.
   * `GET /b/storage/api/recent` — takes no query parameters server-side.
   *
   * Returns audit rows ([`FileViewRecord`]), not object metadata: the
   * endpoint pages `impresspress__files__views`, so each item names the
   * object viewed (`bucket`, `key`) and when, and carries none of the
   * object's own columns. Read the object's metadata with `search` or
   * `listObjects` if you need size, content type or upload state.
   */
  async getRecentFiles(): Promise<RecentViewsResult> {
    const result = await this.request<
      RecordListWire<Omit<FileViewRecord, "id">>
    >({
      method: "GET",
      url: "/b/storage/api/recent",
    });
    return flattenRecordList(result);
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
