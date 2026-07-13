# Impresspress TypeScript SDK

Official TypeScript SDK for Impresspress - a modern backend-as-a-service platform.

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

// Upload a file
const file = await impresspress.storage.upload(
  'my-bucket',
  fileData,
  'document.pdf'
);

// Query data
const products = await impresspress.database
  .from('products')
  .where('price', '>', 100)
  .limit(10)
  .execute();
```

## Type Safety

The SDK includes TypeScript types generated from the backend models, ensuring type safety across your application:

```typescript
import { AuthUser, StorageObject, IAMRole } from '@impresspress/sdk';
```

## Features

### Authentication

```typescript
// Sign up
const { user, tokens } = await impresspress.auth.signUp({
  email: 'user@example.com',
  password: 'SecurePassword123!',
  metadata: { name: 'John Doe' },
});

// Sign in
await impresspress.auth.signIn({
  email: 'user@example.com',
  password: 'SecurePassword123!',
});

// Sign out
await impresspress.signOut();

// Get current user
const user = await impresspress.getUser();

// OAuth sign in
const { url } = await impresspress.auth.signInWithOAuth('google');
```

### Storage

```typescript
// Create a bucket
await impresspress.storage.createBucket('images', true); // public bucket

// Upload file
const file = await impresspress.storage.upload(
  'images',
  imageFile,
  'photo.jpg',
  {
    contentType: 'image/jpeg',
    onProgress: (progress) => console.log(`${progress}%`),
  }
);

// Get signed URL
const url = await impresspress.storage.getSignedUrl('images', 'photo.jpg');

// List files
const { data } = await impresspress.storage.list('images', {
  limit: 20,
  offset: 0,
});

// Delete file
await impresspress.storage.delete('images', 'photo.jpg');
```

### Database/Collections

```typescript
// Create a collection
await impresspress.database.createCollection('products', {
  name: { type: 'string', required: true },
  price: { type: 'number', required: true },
  in_stock: { type: 'boolean', default: true },
});

// Insert data
const product = await impresspress.database.create({
  collection: 'products',
  data: {
    name: 'Laptop',
    price: 999.99,
    in_stock: true,
  },
});

// Query with builder
const results = await impresspress.database
  .from('products')
  .where('price', '<', 1000)
  .where('in_stock', '=', true)
  .orderBy('price', 'desc')
  .limit(10)
  .execute();

// Update record
await impresspress.database.update({
  collection: 'products',
  id: product.id,
  data: { price: 899.99 },
});

// Delete record
await impresspress.database.delete('products', product.id);
```

### IAM (Identity & Access Management)

```typescript
// Get all roles
const roles = await impresspress.iam.getRoles();

// Create a custom role
const role = await impresspress.iam.createRole({
  name: 'editor',
  display_name: 'Content Editor',
  description: 'Can edit content',
  metadata: {
    allowed_ips: ['192.168.1.0/24'],
    disabled_features: ['delete'],
  },
});

// Assign role to user
await impresspress.iam.assignRoleToUser(userId, 'editor');

// Check user roles
const userRoles = await impresspress.iam.getUserRoles(userId);

// Test permissions
const result = await impresspress.iam.testPermission(userId, 'content', 'edit');

// Create policy
await impresspress.iam.createPolicy({
  subject: 'editor',
  resource: 'content',
  action: 'edit',
  effect: 'allow',
});

// Get audit logs
const logs = await impresspress.iam.getAuditLogs({ limit: 10 });
```

### Extensions

```typescript
// List extensions
const extensions = await impresspress.extensions.list();

// Enable an extension
await impresspress.extensions.enable('cloudstorage', {
  defaultStorageLimit: 10737418240, // 10GB
  enableSharing: true,
});

// CloudStorage extension
const share = await impresspress.cloudStorage.share(fileId, {
  email: 'friend@example.com',
  permissions: 'view',
  expiresAt: new Date(Date.now() + 7 * 24 * 60 * 60 * 1000),
});

const quota = await impresspress.cloudStorage.getQuota();

// Products extension
const product = await impresspress.products.createProduct({
  group_id: 'group-123',
  name: 'Premium Plan',
  pricing_formula: 'base_price * quantity',
});

const pricing = await impresspress.products.calculatePrice(product.id, {
  base_price: 99,
  quantity: 3,
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
- **storage**: File storage service
- **database**: Database/collections service
- **extensions**: Extensions management
- **cloudStorage**: CloudStorage extension methods
- **products**: Products extension methods

### Shortcut Methods

The client provides convenient shortcut methods:

```typescript
// Instead of: impresspress.storage.upload(...)
await impresspress.upload('bucket', file, 'name.txt');

// Instead of: impresspress.database.query(...)
await impresspress.query('collection', { limit: 10 });

// Instead of: impresspress.auth.getUser()
await impresspress.getUser();

// Instead of: impresspress.auth.signIn(...)
await impresspress.signIn('email', 'password');

// Instead of: impresspress.auth.signOut()
await impresspress.signOut();
```

## Browser vs Node.js

The SDK works in both browser and Node.js environments:

### Browser
```typescript
const file = document.getElementById('file-input').files[0];
await impresspress.storage.upload('bucket', file, file.name);
```

### Node.js
```typescript
import fs from 'fs';

const buffer = fs.readFileSync('./document.pdf');
await impresspress.storage.upload('bucket', buffer, 'document.pdf');
```

## Error Handling

```typescript
try {
  await impresspress.auth.signIn({
    email: 'user@example.com',
    password: 'wrong-password',
  });
} catch (error) {
  if (error.error?.code === 'INVALID_CREDENTIALS') {
    console.error('Invalid email or password');
  }
}
```

## TypeScript Support

The SDK is written in TypeScript and provides full type definitions:

```typescript
import type {
  User,
  StorageObject,
  Collection
} from '@impresspress/sdk';
```

## License

MIT