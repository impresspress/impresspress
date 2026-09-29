import { describe, it, expect } from "vitest";
import type { paths } from "../src/generated/api";
import type {
  AuthSessionUser,
  AuthTokens,
  SignUpReply,
  SignUpResult,
} from "../src/services/auth.service";
import type { Extension, ShareRecord } from "../src/services/extensions.service";
import type { CloudStorageExtension } from "../src/services/extensions.service";
import type {
  GuestOrderStatus,
  PlatformSubscriptionResponse,
} from "../src/services/extensions.service";
import type {
  IAMRole,
  IAMRoleListResponse,
  IAMRoleUpdateResponse,
} from "../src/services/iam.service";
import type {
  FileMetadataRecord,
  FileViewRecord,
  ListObjectsResult,
  StorageObjectInfo,
} from "../src/services/storage.service";

/**
 * The generated types are only a gate if something is checked against them.
 *
 * `src/generated/api.ts` is regenerated from the committed OpenAPI snapshots
 * and CI diffs it, which catches "the file is stale". That alone would not
 * catch "the file changed and the SDK's own interfaces did not" — the diff
 * would be reviewed, waved through, and the hand-written interface beside it
 * would keep promising the old shape. The assertions below close that: each
 * says the server's published shape is assignable to what the SDK's exported
 * interface promises, so a reshaped response body is a COMPILE error in
 * `npm run typecheck`, not a diff someone has to read carefully.
 *
 * The direction is deliberate. `Server extends Sdk` — everything the server
 * sends must fit what the SDK's type says a caller will get. The reverse is
 * not required: the SDK narrows on purpose in places (it omits row columns a
 * consumer has no use for), and demanding identity would make every such
 * narrowing a build failure.
 */

/** The 200 `application/json` body of one operation, from the generated types. */
type Json200<P extends keyof paths, M extends keyof paths[P]> = paths[P][M] extends {
  responses: { 200: { content: { "application/json": infer T } } };
}
  ? T
  : never;

/**
 * Fails to compile unless `Server` is assignable to `Sdk`. The alias is never
 * read at runtime; instantiating it is the assertion.
 */
type ServerFits<Sdk, Server extends Sdk> = [Sdk, Server];

/** What `flattenRecordList` hands a consumer: the envelope's `id` over the row. */
type Flattened<T> = { id: string } & T;

// ── auth ──────────────────────────────────────────────────────────────────
type _MeUserFits = ServerFits<AuthSessionUser, Json200<"/b/auth/api/me", "get">["user"]>;
type _LoginUserFits = ServerFits<AuthSessionUser, Json200<"/b/auth/api/login", "post">["user"]>;
type _SignupUserFits = ServerFits<
  SignUpResult["user"],
  Json200<"/b/auth/api/signup", "post">["user"]
>;
// The whole reply, not just `user`: `SignUpReply` is a union discriminated on
// `email_verified`, and the server's body fits it only if the server's type
// is that union too, with the flag fixed per branch.
type _SignupReplyFits = ServerFits<SignUpReply, Json200<"/b/auth/api/signup", "post">>;
type _RefreshFits = ServerFits<AuthTokens, Json200<"/b/auth/api/refresh", "post">>;

// ── admin / iam ───────────────────────────────────────────────────────────
type _ExtensionFits = ServerFits<Extension, Json200<"/b/admin/api/extensions", "get">[number]>;
type _RoleListFits = ServerFits<IAMRoleListResponse, Json200<"/b/admin/api/iam/roles", "get">>;
type _RoleFits = ServerFits<IAMRole, Json200<"/b/admin/api/iam/roles", "post">>;
type _RoleUpdateFits = ServerFits<
  IAMRoleUpdateResponse,
  Json200<"/b/admin/api/iam/roles/{id}", "patch">
>;

// ── storage ───────────────────────────────────────────────────────────────
type ObjectList = Json200<"/b/storage/api/buckets/{name}/objects", "get">;
type _ObjectListFits = ServerFits<ListObjectsResult, ObjectList>;
type _ObjectInfoFits = ServerFits<StorageObjectInfo, ObjectList["objects"][number]>;

type SearchRow = Json200<"/b/storage/api/search", "get">["records"][number];
type _SearchRowFits = ServerFits<FileMetadataRecord, Flattened<SearchRow["data"]>>;
// `/recent` pages the object-view audit table, so the row is a
// `FileViewRecord`. Asserting `FileMetadataRecord` here compiled only while
// the route published `ObjectRow` — a shape the handler never sent.
type _RecentRowFits = ServerFits<
  FileViewRecord,
  Flattened<Json200<"/b/storage/api/recent", "get">["records"][number]["data"]>
>;

// ── cloud storage ─────────────────────────────────────────────────────────
type ShareRow = Json200<"/b/cloudstorage/shares", "get">["records"][number];
type _ShareRowFits = ServerFits<ShareRecord, Flattened<ShareRow["data"]>>;
type _QuotaFits = ServerFits<
  Awaited<ReturnType<CloudStorageExtension["getQuota"]>>,
  Json200<"/b/cloudstorage/quota", "get">
>;
type _ShareCreatedFits = ServerFits<
  Awaited<ReturnType<CloudStorageExtension["share"]>>,
  Json200<"/b/cloudstorage/shares", "post">
>;
type _BucketListFits = ServerFits<
  { buckets: string[] },
  Json200<"/b/storage/api/buckets", "get">
>;

// ── products ──────────────────────────────────────────────────────────────
type _GuestOrderStatusFits = ServerFits<
  GuestOrderStatus,
  Json200<"/b/products/orders/{id}/status", "get">
>;
type _SubscriptionFits = ServerFits<
  PlatformSubscriptionResponse,
  Json200<"/b/products/subscription", "get">
>;

/**
 * The type-level assertions above are the real content of this file; the
 * runtime cases below only stop a bundler or a future `skipLibCheck` sweep
 * from eliding the import and leaving the aliases unreferenced.
 */
describe("the SDK's exported types accept what the server publishes", () => {
  it("keeps every assertion instantiated", () => {
    const witnesses: Array<
      | _MeUserFits
      | _LoginUserFits
      | _SignupUserFits
      | _SignupReplyFits
      | _RefreshFits
      | _ExtensionFits
      | _RoleListFits
      | _RoleFits
      | _RoleUpdateFits
      | _ObjectListFits
      | _ObjectInfoFits
      | _SearchRowFits
      | _RecentRowFits
      | _ShareRowFits
      | _QuotaFits
      | _ShareCreatedFits
      | _BucketListFits
      | _GuestOrderStatusFits
      | _SubscriptionFits
    > = [];
    expect(witnesses).toHaveLength(0);
  });
});
