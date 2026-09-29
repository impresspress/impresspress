import { BaseService } from "./base.service";

/**
 * One row of `GET /b/admin/api/iam/roles` — `AdminRoleView` on the server
 * (`blocks/admin/iam.rs`). snake_case, like every impresspress API
 * projection; `permissions` is advisory metadata for the IAM UI, WRAP
 * grants are what the runtime actually enforces.
 */
export interface IAMRole {
  id: string;
  /** Unique role name — the value stored in `user_roles.role`. */
  name: string;
  description: string;
  permissions: string[];
  /** Built-in roles cannot be renamed or deleted. */
  is_system: boolean;
  /** RFC 3339. */
  created_at: string;
  updated_at: string;
}

/**
 * `GET /b/admin/api/iam/roles` response — `AdminRoleListResponse` on the
 * server. The endpoint takes no query parameters and does not paginate:
 * `page` is always 1 and `page_size` the handler's fixed ceiling.
 */
export interface IAMRoleListResponse {
  records: IAMRole[];
  total_count: number;
  page: number;
  page_size: number;
}

/** `POST /b/admin/api/iam/roles` body — `CreateRoleRequest` on the server. */
export interface CreateRoleRequest {
  name: string;
  description?: string;
  permissions?: string[];
}

/**
 * `PATCH /b/admin/api/iam/roles/{id}` body — `UpdateRoleRequest` on the
 * server. Only the fields present are applied; `name` is refused on a
 * system role.
 */
export interface UpdateRoleRequest {
  name?: string;
  description?: string;
  permissions?: string[];
}

/**
 * `PATCH /b/admin/api/iam/roles/{id}` response — `AdminRoleUpdateResponse` on
 * the server: the role as it now is, plus `warning` when the update was saved
 * but a rename's grants did not all follow it. A rename rewrites each grant
 * separately, so one can stop part-way; the role is renamed either way, and
 * `warning` says which grants still name the old role and how to move them.
 */
export interface IAMRoleUpdateResponse extends IAMRole {
  warning?: string | null;
}

export class IAMService extends BaseService {
  /**
   * List every role, sorted by name. Unwraps the server's
   * `{ records, total_count, page, page_size }` envelope — the endpoint does
   * not paginate, so the envelope carries nothing a caller needs.
   */
  async getRoles(): Promise<IAMRole[]> {
    const res = await this.request<IAMRoleListResponse>({
      method: "GET",
      url: "/b/admin/api/iam/roles",
    });
    return res.records;
  }

  /** Create a role. The response is the same row projection `getRoles` lists. */
  async createRole(role: CreateRoleRequest): Promise<IAMRole> {
    return this.request<IAMRole>({
      method: "POST",
      url: "/b/admin/api/iam/roles",
      data: role,
    });
  }

  /**
   * Update a role by its `id` (as returned by `getRoles` / `createRole`),
   * not by name — the route is keyed by row id.
   */
  async updateRole(roleId: string, updates: UpdateRoleRequest): Promise<IAMRoleUpdateResponse> {
    return this.request<IAMRoleUpdateResponse>({
      method: "PATCH",
      url: `/b/admin/api/iam/roles/${roleId}`,
      data: updates,
    });
  }

  /** Delete a role by its `id`. System roles are refused server-side. */
  async deleteRole(roleId: string): Promise<void> {
    await this.request<{ deleted: boolean }>({
      method: "DELETE",
      url: `/b/admin/api/iam/roles/${roleId}`,
    });
  }
}
