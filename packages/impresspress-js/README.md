# Impresspress TypeScript SDK

Official TypeScript SDK for Impresspress - a modern backend-as-a-service platform.

Every method in this SDK calls a route that actually exists on the server
(`crates/impresspress-core/src/blocks/**`) — there is no generic
database/collections API, no client-side query builder, and no `/api/*`
surface. Storage is bucket + key based, not id based; extension helpers
(`cloudStorage`, `products`) wrap each block's own declared HTTP routes.

## Installation

```bash
npm install @impresspress/sdk
# or
yarn add @impresspress/sdk
# or
pnpm add @impresspress/sdk
```

## Quick Start

```typescript
import { ImpresspressClient } from '@impresspress/sdk';

// Initialize the client
const impresspress = new ImpresspressClient({
  url: 'http://localhost:8090',
  apiKey: 'your-api-key', // Optional: for server-side usage
});

// Sign in a user
await impresspress.auth.signIn({
  email: 'user@example.com',
  password: 'password123',
});

// Create a bucket and upload a file
await impresspress.storage.createBucket('my-bucket');
const uploaded = await impresspress.storage.uploadFile('my-bucket', fileData, {
  key: 'document.pdf',
});

// List objects in a bucket
const { objects } = await impresspress.storage.listObjects('my-bucket');
```

## Type Safety

The SDK includes TypeScript types generated from the backend models, ensuring type safety across your application:

```typescript
import { AuthUser, StorageObject, IAMRole } from '@impresspress/sdk';
```

## Features

### Authentication

```typescript
// Sign up — auto-signs in unless email verification is required
const signUpResult = await impresspress.auth.signUp({
  email: 'user@example.com',
  password: 'SecurePassword123!',
  name: 'John Doe',
});
console.log(signUpResult.user, signUpResult.emailVerified, signUpResult.tokens);

// Sign in
const { user, tokens } = await impresspress.auth.signIn({
  email: 'user@example.com',
  password: 'SecurePassword123!',
});

// Sign out
await impresspress.auth.signOut();

// Get the current user (returns null if not signed in; propagates other errors)
const current = await impresspress.auth.getUser();

// Update profile (only `name`/`avatar_url` are server-accepted fields)
await impresspress.auth.updateUser({ name: 'New Name' });

// Password reset flow
await impresspress.auth.resetPassword({ email: 'user@example.com' }); // sends the email
await impresspress.auth.confirmPasswordReset('token-from-email', 'NewPassword123!');

// Change password while signed in
await impresspress.auth.updatePassword({
  currentPassword: 'old',
  newPassword: 'NewPassword123!',
});

// Email verification
await impresspress.auth.verifyEmail('token-from-email');
await impresspress.auth.resendVerification('user@example.com');

// Refresh the access/refresh token pair (uses the token cached from signIn/signUp
// by default; pass one explicitly for server-side usage)
await impresspress.auth.refreshSession();
```

#### OAuth

Only `google`, `github`, and `microsoft` are implemented server-side.

```typescript
// Redirect-based flow
const { auth_url } = await impresspress.auth.signInWithOAuth('google');
window.location.href = auth_url;

// Popup-based flow (single consolidated implementation — see PopupAuthSession)
const user = await impresspress.auth.signInWithOAuthPopup('google');

// Cancellable via AbortSignal
const controller = new AbortController();
const signInPromise = impresspress.auth.signInWithOAuthPopup('github', {
  signal: controller.signal,
});
// controller.abort() to cancel
```

### Storage

Buckets are name-addressed; objects are key-addressed (keys may contain `/`).
There is no folder/rename/move/metadata-update API — only bucket and object
CRUD, search, and "recent".

```typescript
// Buckets
await impresspress.storage.createBucket('images', true); // public bucket
const bucketNames = await impresspress.storage.listBuckets();
await impresspress.storage.deleteBucket('images');

// Objects
const uploaded = await impresspress.storage.uploadFile('images', imageFile, {
  key: 'photo.jpg',
});
const { objects, total_count } = await impresspress.storage.listObjects('images', {
  prefix: 'photo',
  page_size: 20,
});
const blob = await impresspress.storage.downloadFile('images', 'photo.jpg');
const url = impresspress.storage.getDownloadUrl('images', 'photo.jpg');
await impresspress.storage.deleteObject('images', 'photo.jpg');

// Search the current user's uploads / recently viewed objects
const results = await impresspress.storage.search('photo');
const recent = await impresspress.storage.getRecentFiles();
```

### CloudStorage (sharing + quota)

```typescript
const share = await impresspress.cloudStorage.share('images', 'photo.jpg', {
  expiresInHours: 24,
});
// share.direct_url is a public, tokenized link — GET /b/storage/direct/{token}

const shares = await impresspress.cloudStorage.listShares();
await impresspress.cloudStorage.deleteShare(share.id);

const { quota, usage } = await impresspress.cloudStorage.getQuota();
```

### IAM (Identity & Access Management)

```typescript
const roles = await impresspress.iam.getRoles();

const role = await impresspress.iam.createRole({
  name: 'editor',
  display_name: 'Content Editor',
  description: 'Can edit content',
  metadata: {
    allowed_ips: ['192.168.1.0/24'],
    disabled_features: ['delete'],
  },
});

await impresspress.iam.updateRole('editor', { description: 'Updated description' });
await impresspress.iam.deleteRole('editor');
```

### Products

```typescript
// Public catalog (no admin auth required)
const { records } = await impresspress.products.listProducts({ page: 1 });

// Admin-gated management routes (the server enforces the auth tier)
const group = await impresspress.products.createGroup({
  name: 'My Restaurant',
  template_id: 'restaurant_template',
});
const product = await impresspress.products.createProduct({
  group_id: group.id,
  name: 'Margherita Pizza',
});
```

### Extensions

`extensions.list()` and `extensions.call()` are the only generic extension
methods — there is no server-side enable/disable/configure/health lifecycle
API. `call()` is a raw passthrough to any block's declared HTTP routes; use
it for a block's endpoints that don't have a dedicated wrapper.

```typescript
const extensions = await impresspress.extensions.list();

const result = await impresspress.extensions.call('cloudstorage', 'quota', {
  method: 'GET',
});
```

## API Reference

### Client Initialization

```typescript
new ImpresspressClient(config: ImpresspressConfig | string)
```

Config options:
- `url`: The Impresspress server URL
- `apiKey`: Optional API key for authentication
- `headers`: Additional headers to include
- `timeout`: Request timeout in milliseconds

### Services

- **auth**: Authentication service
- **storage**: File storage service (buckets + key-addressed objects)
- **iam**: Roles service
- **extensions**: Generic block-route passthrough (`list`/`call`)
- **cloudStorage**: `impresspress/files` sharing + quota routes
- **products**: `impresspress/products` catalog + admin routes

## Browser vs Node.js

The SDK works in both browser and Node.js environments:

### Browser
```typescript
const file = document.getElementById('file-input').files[0];
await impresspress.storage.uploadFile('bucket', file);
```

### Node.js
```typescript
import fs from 'fs';

const buffer = fs.readFileSync('./document.pdf');
await impresspress.storage.uploadFile('bucket', buffer, { key: 'document.pdf' });
```

## Error Handling

Every service throws a typed `ImpresspressError` (never a plain object) for a
non-2xx response or transport failure. `code`/`message` mirror the server's
real `{ "error": "<code>", "message": "<msg>" }` wire shape; `status` is the
HTTP status (`0` for network/timeout/abort failures).

```typescript
import { ImpresspressError, isUnauthorizedError } from '@impresspress/sdk';

try {
  await impresspress.auth.signIn({
    email: 'user@example.com',
    password: 'wrong-password',
  });
} catch (error) {
  if (error instanceof ImpresspressError) {
    console.error(error.code, error.message, error.status);
  }
}
```

Only `getUser()` folds a failure into an absence value (`null`), and only for
the 401/404 "not signed in" case — every other failure propagates rather
than being silently swallowed:

```typescript
const user = await impresspress.auth.getUser(); // null if signed out, throws on a real outage
```

## TypeScript Support

The SDK is written in TypeScript and provides full type definitions:

```typescript
import type {
  AuthSessionUser,
  StorageObjectInfo,
  IAMRole,
} from '@impresspress/sdk';
```

## License

MIT
