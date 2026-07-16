import { describe, it, expect } from "vitest";
// Verifies type compatibility between the hand-written model aliases
// (`models.ts`) and the generated DB-schema types (`types/generated/database`)
// they wrap — a compile error here means the alias/generated pair has
// drifted. Field names are camelCase, matching the generated types exactly
// (there is no snake_case/camelCase translation layer — see the workspace's
// "no magic code / implicit mapping" rule).
import {
  User,
  StorageObject,
  Bucket,
  IAMRole,
  AuthUser,
  StorageStorageObject,
  StorageStorageBucket,
} from "../src/types";

describe("type compatibility between manual and generated types", () => {
  it("User is an alias of the generated AuthUser", () => {
    const authUser: AuthUser = {
      id: "123",
      email: "test@example.com",
      username: "testuser",
      confirmed: true,
      firstName: "Test",
      lastName: "User",
      displayName: "Test User",
      phone: "1234567890",
      location: "Test Location",
      metadata: "{}",
      createdAt: new Date(),
      updatedAt: new Date(),
    };
    const user: User = authUser;

    expect(user.id).toBe(authUser.id);
    expect(user.displayName).toBe("Test User");
  });

  it("StorageObject is aliased to the generated StorageStorageObject", () => {
    const storageObject: StorageObject = {
      id: "456",
      bucketName: "test-bucket",
      objectName: "test.txt",
      parentFolderId: null,
      size: 1024,
      contentType: "text/plain",
      checksum: "abc123",
      metadata: '{"test": true}',
      createdAt: new Date(),
      updatedAt: new Date(),
      lastViewed: null,
      userId: "123",
      appId: null,
    };
    const direct: StorageStorageObject = storageObject;
    expect(direct.bucketName).toBe("test-bucket");
  });

  it("Bucket is aliased to the generated StorageStorageBucket", () => {
    const bucket: Bucket = {
      id: "789",
      name: "test-bucket",
      public: false,
      createdAt: new Date(),
      updatedAt: new Date(),
    };
    const direct: StorageStorageBucket = bucket;
    expect(direct.name).toBe("test-bucket");
  });

  it("IAMRole is imported from generated types with its full shape", () => {
    const role: IAMRole = {
      id: "321",
      name: "admin",
      displayName: "Administrator",
      description: "Full system access",
      type: "system",
      metadata: {
        allowedIps: ["192.168.1.1"],
        disabledFeatures: [],
      },
      createdAt: new Date(),
      updatedAt: new Date(),
    };
    expect(role.name).toBe("admin");
  });
});
