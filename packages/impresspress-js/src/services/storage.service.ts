import { BaseService } from "./base.service";
import {
  StorageObject,
  Bucket,
  StorageObjectMetadata,
} from "../types";

export interface ListOptions {
  parent_folder_id?: string;
  search?: string;
  type?: "file" | "folder" | "all";
  sort?: "name" | "size" | "date" | "type";
  order?: "asc" | "desc";
  page?: number;
  limit?: number;
}

export interface UploadFileOptions {
  path?: string;
  parent_folder_id?: string;
  metadata?: Record<string, any>;
  onProgress?: (progress: number) => void;
}

export interface MoveOptions {
  parent_folder_id?: string;
  overwrite?: boolean;
}

export interface ShareOptions {
  expires_in?: number;
  password?: string;
  permissions?: string[];
  max_downloads?: number;
}

export class StorageService extends BaseService {
  /**
   * List files and folders in a bucket
   * @param bucketName - The name of the bucket
   * @param options - List options
   */
  async listObjects(
    bucketName: string,
    options?: ListOptions,
  ): Promise<StorageObject[]> {
    const queryString = options ? this.buildQueryString(options) : "";
    const response = await this.request<StorageObject[]>({
      method: "GET",
      url: `/b/storage/api/buckets/${bucketName}/objects${queryString ? `?${queryString}` : ""}`,
    });
    return Array.isArray(response) ? response : [];
  }

  /**
   * Get a specific object by ID
   * @param bucketName - The name of the bucket
   * @param objectId - The ID of the object
   */
  async getObject(
    bucketName: string,
    objectId: string,
  ): Promise<StorageObject> {
    return this.request<StorageObject>({
      method: "GET",
      url: `/b/storage/api/buckets/${bucketName}/objects/${objectId}`,
    });
  }

  /**
   * Upload a file to storage
   * @param bucketName - The name of the bucket
   * @param file - The file to upload
   * @param options - Upload options
   */
  async uploadFile(
    bucketName: string,
    file: File | Buffer | Blob,
    options?: UploadFileOptions,
  ): Promise<StorageObject> {
    const formData = new FormData();

    // Handle different file types
    if (typeof globalThis.window !== "undefined" && file instanceof File) {
      formData.append("file", file);
    } else if (file instanceof Blob) {
      formData.append("file", file, "file");
    } else if (typeof Buffer !== "undefined" && Buffer.isBuffer(file)) {
      formData.append("file", new Blob([new Uint8Array(file)]), "file");
    } else {
      throw new Error("Invalid file type");
    }

    if (options?.path) {
      formData.append("path", options.path);
    }
    if (options?.parent_folder_id) {
      formData.append("parent_folder_id", options.parent_folder_id);
    }
    if (options?.metadata) {
      formData.append("metadata", JSON.stringify(options.metadata));
    }

    return this.requestFormData<StorageObject>(
      `/b/storage/api/buckets/${bucketName}/upload`,
      formData,
    );
  }

  /**
   * Create a folder
   * @param bucketName - The name of the bucket
   * @param name - The name of the folder
   * @param path - Optional path
   * @param parentId - Optional parent folder ID
   */
  async createFolder(
    bucketName: string,
    name: string,
    path?: string,
    parentId?: string,
  ): Promise<StorageObject> {
    const data: any = { name };
    if (path) data.path = path;
    if (parentId) data.parent_folder_id = parentId;

    return this.request<StorageObject>({
      method: "POST",
      url: `/b/storage/api/buckets/${bucketName}/folders`,
      data,
    });
  }

  /**
   * Download a file from storage
   * @param bucketName - The name of the bucket
   * @param objectId - The ID of the object to download
   */
  async downloadFile(bucketName: string, objectId: string): Promise<Blob> {
    return this.requestBlob(
      `/b/storage/api/buckets/${bucketName}/objects/${objectId}/download`,
    );
  }

  /**
   * Get download URL for a file
   * @param bucketName - The name of the bucket
   * @param objectId - The ID of the object
   */
  getDownloadUrl(bucketName: string, objectId: string): string {
    return `${this.config.url}/b/storage/api/buckets/${bucketName}/objects/${objectId}/download`;
  }

  /**
   * Rename an object (file or folder)
   * @param bucketName - The name of the bucket
   * @param objectId - The ID of the object
   * @param newName - The new name
   */
  async renameObject(
    bucketName: string,
    objectId: string,
    newName: string,
  ): Promise<StorageObject> {
    return this.request<StorageObject>({
      method: "PATCH",
      url: `/b/storage/api/buckets/${bucketName}/objects/${objectId}/rename`,
      data: { name: newName },
    });
  }

  /**
   * Update object metadata
   * @param bucketName - The name of the bucket
   * @param objectId - The ID of the object
   * @param metadata - The metadata to update
   */
  async updateMetadata(
    bucketName: string,
    objectId: string,
    metadata: StorageObjectMetadata | Record<string, any>,
  ): Promise<StorageObject> {
    return this.request<StorageObject>({
      method: "PATCH",
      url: `/b/storage/api/buckets/${bucketName}/objects/${objectId}/metadata`,
      data: { metadata: JSON.stringify(metadata) },
    });
  }

  /**
   * Move an object to a different parent folder
   * @param bucketName - The name of the bucket
   * @param objectId - The ID of the object
   * @param options - Move options
   */
  async moveObject(
    bucketName: string,
    objectId: string,
    options: MoveOptions,
  ): Promise<StorageObject> {
    return this.request<StorageObject>({
      method: "PATCH",
      url: `/b/storage/api/buckets/${bucketName}/objects/${objectId}/move`,
      data: options,
    });
  }

  /**
   * Share an object
   * @param bucketName - The name of the bucket
   * @param objectId - The ID of the object
   * @param options - Share options
   */
  async shareObject(
    bucketName: string,
    objectId: string,
    options?: ShareOptions,
  ): Promise<{ url: string; expires_at?: Date }> {
    return this.request({
      method: "POST",
      url: `/b/storage/api/buckets/${bucketName}/objects/${objectId}/share`,
      data: options || {},
    });
  }

  /**
   * Delete an object (file or folder)
   * @param bucketName - The name of the bucket
   * @param objectId - The ID of the object
   */
  async deleteObject(bucketName: string, objectId: string): Promise<void> {
    await this.request<void>({
      method: "DELETE",
      url: `/b/storage/api/buckets/${bucketName}/objects/${objectId}`,
    });
  }

  /**
   * Delete multiple objects
   * @param bucketName - The name of the bucket
   * @param objectIds - Array of object IDs
   */
  async deleteObjects(bucketName: string, objectIds: string[]): Promise<void> {
    // Delete one by one for now
    for (const id of objectIds) {
      await this.deleteObject(bucketName, id);
    }
  }

  /**
   * Get storage quota information
   */
  async getQuota(): Promise<{
    used: number;
    total: number;
    percentage: number;
  }> {
    try {
      return await this.request({
        method: "GET",
        url: "/b/storage/api/quota",
      });
    } catch (error) {
      // Return default quota on error
      return { used: 0, total: 5 * 1024 * 1024 * 1024, percentage: 0 };
    }
  }

  /**
   * Get storage statistics
   */
  async getStats(): Promise<{
    totalSize: number;
    fileCount: number;
    folderCount: number;
    sharedCount: number;
    trashedCount: number;
  }> {
    try {
      return await this.request({
        method: "GET",
        url: "/b/storage/api/stats",
      });
    } catch (error) {
      // Return default stats on error
      return {
        totalSize: 0,
        fileCount: 0,
        folderCount: 0,
        sharedCount: 0,
        trashedCount: 0,
      };
    }
  }

  /**
   * Search for files and folders
   * @param query - Search query
   * @param options - Search options
   */
  async search(
    query: string,
    options?: { type?: "file" | "folder" | "all" },
  ): Promise<StorageObject[]> {
    const params = new URLSearchParams({ q: query });
    if (options?.type && options.type !== "all") {
      params.append("type", options.type);
    }

    const response = await this.request<StorageObject[]>({
      method: "GET",
      url: `/b/storage/api/search?${params.toString()}`,
    });
    return Array.isArray(response) ? response : [];
  }

  /**
   * Get recent files
   * @param limit - Number of files to return
   */
  async getRecentFiles(limit: number = 10): Promise<StorageObject[]> {
    const response = await this.request<StorageObject[]>({
      method: "GET",
      url: `/b/storage/api/recent?limit=${limit}`,
    });
    return Array.isArray(response) ? response : [];
  }

  /**
   * Get shared files
   */
  async getSharedFiles(): Promise<StorageObject[]> {
    const response = await this.request<StorageObject[]>({
      method: "GET",
      url: "/b/storage/api/shared",
    });
    return Array.isArray(response) ? response : [];
  }

  /**
   * Get trashed files
   */
  async getTrashedFiles(): Promise<StorageObject[]> {
    const response = await this.request<StorageObject[]>({
      method: "GET",
      url: "/b/storage/api/trash",
    });
    return Array.isArray(response) ? response : [];
  }

  /**
   * Restore file from trash
   * @param itemId - The ID of the item to restore
   */
  async restoreFromTrash(itemId: string): Promise<void> {
    await this.request({
      method: "POST",
      url: `/b/storage/api/trash/${itemId}/restore`,
      data: {},
    });
  }

  /**
   * Empty trash
   */
  async emptyTrash(): Promise<void> {
    await this.request({
      method: "DELETE",
      url: "/b/storage/api/trash",
    });
  }

  /**
   * Create a new bucket
   * @param name - The name of the bucket
   * @param isPublic - Whether the bucket should be public
   */
  async createBucket(name: string, isPublic: boolean = false): Promise<Bucket> {
    return this.request<Bucket>({
      method: "POST",
      url: "/b/storage/api/buckets",
      data: {
        name,
        public: isPublic,
      },
    });
  }

  /**
   * Delete a bucket
   * @param name - The name of the bucket
   */
  async deleteBucket(name: string): Promise<void> {
    await this.request<void>({
      method: "DELETE",
      url: `/b/storage/api/buckets/${name}`,
    });
  }

  /**
   * List all buckets
   */
  async listBuckets(): Promise<Bucket[]> {
    return this.request<Bucket[]>({
      method: "GET",
      url: "/b/storage/api/buckets",
    });
  }
}
