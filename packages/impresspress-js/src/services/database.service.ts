import { BaseService } from "./base.service";
import { Collection, QueryOptions, PaginatedResponse } from "../types";

export interface DatabaseRecord {
  id: string;
  [key: string]: any;
}

export interface CreateRecordOptions<T = DatabaseRecord> {
  collection: string;
  data: Omit<T, "id" | "created_at" | "updated_at">;
}

export interface UpdateRecordOptions<T = DatabaseRecord> {
  collection: string;
  id: string;
  data: Partial<Omit<T, "id" | "created_at" | "updated_at">>;
}

export interface QueryBuilder<T = DatabaseRecord> {
  select(fields: string[]): QueryBuilder<T>;
  where(field: string, operator: string, value: any): QueryBuilder<T>;
  orderBy(field: string, direction?: "asc" | "desc"): QueryBuilder<T>;
  limit(limit: number): QueryBuilder<T>;
  offset(offset: number): QueryBuilder<T>;
  execute(): Promise<PaginatedResponse<T>>;
  first(): Promise<T | null>;
  count(): Promise<number>;
}

export class DatabaseService extends BaseService {
  /**
   * Create a new collection
   */
  async createCollection(
    name: string,
    schema?: Record<string, any>,
  ): Promise<Collection> {
    return this.request<Collection>({
      method: "POST",
      url: "/api/collections",
      data: { name, schema },
    });
  }

  /**
   * Delete a collection
   */
  async deleteCollection(name: string): Promise<void> {
    await this.request<void>({
      method: "DELETE",
      url: `/api/collections/${name}`,
    });
  }

  /**
   * List all collections
   */
  async listCollections(): Promise<Collection[]> {
    return this.request<Collection[]>({
      method: "GET",
      url: "/api/collections",
    });
  }

  /**
   * Get collection details
   */
  async getCollection(name: string): Promise<Collection> {
    return this.request<Collection>({
      method: "GET",
      url: `/api/collections/${name}`,
    });
  }

  /**
   * Create a new record in a collection
   */
  async create<T = DatabaseRecord>(
    options: CreateRecordOptions<T>,
  ): Promise<T> {
    return this.request<T>({
      method: "POST",
      url: `/api/collections/${options.collection}/records`,
      data: options.data,
    });
  }

  /**
   * Get a record by ID
   */
  async findById<T = DatabaseRecord>(
    collection: string,
    id: string,
  ): Promise<T | null> {
    try {
      return await this.request<T>({
        method: "GET",
        url: `/api/collections/${collection}/records/${id}`,
      });
    } catch (error) {
      return null;
    }
  }

  /**
   * Update a record
   */
  async update<T = DatabaseRecord>(
    options: UpdateRecordOptions<T>,
  ): Promise<T> {
    return this.request<T>({
      method: "PATCH",
      url: `/api/collections/${options.collection}/records/${options.id}`,
      data: options.data,
    });
  }

  /**
   * Delete a record
   */
  async delete(collection: string, id: string): Promise<void> {
    await this.request<void>({
      method: "DELETE",
      url: `/api/collections/${collection}/records/${id}`,
    });
  }

  /**
   * Query records from a collection
   */
  async query<T = DatabaseRecord>(
    collection: string,
    options?: QueryOptions,
  ): Promise<PaginatedResponse<T>> {
    const queryString = options ? this.buildQueryString(options) : "";
    return this.request<PaginatedResponse<T>>({
      method: "GET",
      url: `/api/collections/${collection}/records${queryString ? `?${queryString}` : ""}`,
    });
  }

  /**
   * Create a query builder for more complex queries
   */
  from<T = DatabaseRecord>(collection: string): QueryBuilder<T> {
    const self = this;
    const queryParams: QueryOptions = {};
    let selectedFields: string[] = [];

    const builder: QueryBuilder<T> = {
      select(fields: string[]): QueryBuilder<T> {
        selectedFields = fields;
        return builder;
      },

      where(field: string, operator: string, value: any): QueryBuilder<T> {
        if (!queryParams.filter) {
          queryParams.filter = {};
        }
        queryParams.filter[`${field}[${operator}]`] = value;
        return builder;
      },

      orderBy(
        field: string,
        direction: "asc" | "desc" = "asc",
      ): QueryBuilder<T> {
        queryParams.order = `${direction === "desc" ? "-" : ""}${field}`;
        return builder;
      },

      limit(limit: number): QueryBuilder<T> {
        queryParams.limit = limit;
        return builder;
      },

      offset(offset: number): QueryBuilder<T> {
        queryParams.offset = offset;
        return builder;
      },

      async execute(): Promise<PaginatedResponse<T>> {
        const params = { ...queryParams };
        if (selectedFields.length > 0) {
          params.filter = {
            ...params.filter,
            select: selectedFields.join(","),
          };
        }
        return self.query<T>(collection, params);
      },

      async first(): Promise<T | null> {
        const result = await builder.limit(1).execute();
        return result.data[0] || null;
      },

      async count(): Promise<number> {
        const response = await self.request<{ count: number }>({
          method: "GET",
          url: `/api/collections/${collection}/count`,
          params: queryParams.filter,
        });
        return response.count;
      },
    };

    return builder;
  }

  /**
   * Perform a transaction
   */
  async transaction<T>(
    operations: Array<{
      type: "create" | "update" | "delete";
      collection: string;
      data?: any;
      id?: string;
    }>,
  ): Promise<T[]> {
    return this.request<T[]>({
      method: "POST",
      url: "/api/database/transaction",
      data: { operations },
    });
  }

  /**
   * Execute raw SQL (if supported and authorized)
   */
  async rawQuery<T = any>(sql: string, params?: any[]): Promise<T[]> {
    return this.request<T[]>({
      method: "POST",
      url: "/api/database/query",
      data: { sql, params },
    });
  }

  /**
   * Backup a collection
   */
  async backupCollection(collection: string): Promise<{ url: string }> {
    return this.request<{ url: string }>({
      method: "POST",
      url: `/api/collections/${collection}/backup`,
    });
  }

  /**
   * Restore a collection from backup
   */
  async restoreCollection(
    collection: string,
    backupUrl: string,
  ): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: `/api/collections/${collection}/restore`,
      data: { backup_url: backupUrl },
    });
  }

  /**
   * Get collection statistics
   */
  async getCollectionStats(collection: string): Promise<{
    total_records: number;
    size_bytes: number;
    indexes: string[];
    last_modified: string;
  }> {
    return this.request({
      method: "GET",
      url: `/api/collections/${collection}/stats`,
    });
  }

  /**
   * Create an index on a collection field
   */
  async createIndex(
    collection: string,
    field: string,
    options?: { unique?: boolean; sparse?: boolean },
  ): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: `/api/collections/${collection}/indexes`,
      data: { field, ...options },
    });
  }

  /**
   * Drop an index
   */
  async dropIndex(collection: string, field: string): Promise<void> {
    await this.request<void>({
      method: "DELETE",
      url: `/api/collections/${collection}/indexes/${field}`,
    });
  }
}
