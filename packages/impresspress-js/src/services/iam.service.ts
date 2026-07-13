import { BaseService } from "./base.service";
import { IAMRole } from "../types";

export class IAMService extends BaseService {
  /**
   * Get all roles
   */
  async getRoles(): Promise<IAMRole[]> {
    return this.request<IAMRole[]>({
      method: "GET",
      url: "/b/admin/api/iam/roles",
    });
  }

  /**
   * Create a new role
   */
  async createRole(role: IAMRole): Promise<IAMRole> {
    return this.request<IAMRole>({
      method: "POST",
      url: "/b/admin/api/iam/roles",
      data: role,
    });
  }

  /**
   * Update a role
   */
  async updateRole(
    roleName: string,
    updates: Partial<IAMRole>,
  ): Promise<IAMRole> {
    return this.request<IAMRole>({
      method: "PUT",
      url: `/b/admin/api/iam/roles/${roleName}`,
      data: updates,
    });
  }

  /**
   * Delete a role
   */
  async deleteRole(roleName: string): Promise<void> {
    await this.request<void>({
      method: "DELETE",
      url: `/b/admin/api/iam/roles/${roleName}`,
    });
  }
}
