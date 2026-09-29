import { describe, it, expect } from "vitest";
import * as sdk from "../src/index";
import type { AuthSessionUser } from "../src/services/auth.service";
import type { StorageObjectInfo } from "../src/services/storage.service";
import type { Extension } from "../src/services/extensions.service";
import type { IAMRole } from "../src/services/iam.service";

/**
 * Every exported type must describe a shape the server actually sends. The
 * three cases below used to check the hand-written `models.ts` aliases
 * against `types/generated/database.ts` — a "generated" file with no
 * generator behind it, describing a solobase-era column set that no live
 * endpoint has returned for a long time. Checking two fabrications against
 * each other proved only that they agreed with one another.
 *
 * They now check each type against the handler that produces it, cited in
 * the case. `npm run typecheck` compiles this file, so a drifted field is a
 * build failure, not just a green test.
 */
describe("exported types match the handlers that produce them", () => {
  it("AuthSessionUser is the projection GET /b/auth/api/me returns", () => {
    // `blocks/auth_ui/api/me.rs` — `{ user }`, id/email/name/roles, snake_case.
    const user: AuthSessionUser = {
      id: "u1",
      email: "a@b.com",
      name: "A",
      roles: ["user"],
      created_at: "2026-01-01T00:00:00Z",
      avatar_url: "https://cdn.example/a.png",
    };
    expect(user.roles).toEqual(["user"]);
  });

  it("StorageObjectInfo is a listing row from GET /b/storage/api/buckets/{name}/objects", () => {
    // `blocks/files/storage.rs` — objects are key-addressed and carry no id,
    // no parent folder, no checksum, no metadata blob.
    const object: StorageObjectInfo = {
      key: "dir/report.pdf",
      size: 1024,
      content_type: "application/pdf",
      last_modified: "2026-01-01T00:00:00Z",
    };
    expect(object.key).toBe("dir/report.pdf");
  });

  it("Extension is a registered-block row from GET /b/admin/api/extensions", () => {
    // `blocks/admin/mod.rs::handle_extensions` serializes exactly these five
    // keys off `BlockInfo`. There is no description/author/config/metadata.
    const extension: Extension = {
      name: "impresspress/admin",
      version: "0.1.0",
      interface: "feature@v1",
      summary: "Admin dashboard",
      enabled: true,
    };
    expect(extension.interface).toBe("feature@v1");
  });

  it("IAMRole is the admin API's role row (AdminRoleView), snake_case", () => {
    const role: IAMRole = {
      id: "321",
      name: "admin",
      description: "Full system access",
      permissions: ["*"],
      is_system: true,
      created_at: "2026-01-01T00:00:00Z",
      updated_at: "2026-01-01T00:00:00Z",
    };
    expect(role.is_system).toBe(true);
  });
});

/**
 * The fabricated surface is gone, not merely unused. These names described
 * tables and helpers no impresspress endpoint serves; shipping them told a
 * consumer that `parseMetadata(obj)` or an `AuthUser.confirmed` column was
 * something they could rely on.
 */
describe("the fabricated type surface is no longer exported", () => {
  const removedRuntimeExports = [
    "isFolder",
    "isFile",
    "parseMetadata",
    "getDisplayName",
    "getFileExtension",
    "TableNames",
  ];

  it.each(removedRuntimeExports)("does not export %s", (name) => {
    expect(Object.keys(sdk)).not.toContain(name);
  });
});
