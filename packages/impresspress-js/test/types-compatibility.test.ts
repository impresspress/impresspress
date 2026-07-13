// Test file to verify type compatibility between manual and generated types
import {
  User,
  StorageObject,
  Bucket,
  IAMRole,
  AuthUser,
  StorageStorageObject,
  StorageStorageBucket
} from '../src/types';

// Test that User type is compatible with AuthUser
const authUser: AuthUser = {
  id: '123',
  email: 'test@example.com',
  username: 'testuser',
  confirmed: true,
  first_name: 'Test',
  last_name: 'User',
  display_name: 'Test User',
  phone: '1234567890',
  location: 'Test Location',
  metadata: '{}',
  created_at: new Date(),
  updated_at: new Date()
};

// User extends AuthUser with roles
const user: User = {
  ...authUser,
  roles: ['admin', 'user']
};

// Test that StorageObject is correctly aliased
const storageObject: StorageObject = {
  id: '456',
  bucket_name: 'test-bucket',
  object_name: 'test.txt',
  parent_folder_id: null,
  size: 1024,
  content_type: 'text/plain',
  checksum: 'abc123',
  metadata: '{"test": true}',
  created_at: new Date(),
  updated_at: new Date(),
  last_viewed: null,
  user_id: '123',
  app_id: null
};

// Test that Bucket is correctly aliased
const bucket: Bucket = {
  id: '789',
  name: 'test-bucket',
  public: false,
  created_at: new Date(),
  updated_at: new Date()
};

// Test that IAMRole is correctly aliased
const role: IAMRole = {
  id: '321',
  name: 'admin',
  display_name: 'Administrator',
  description: 'Full system access',
  type: 'system',
  metadata: {
    allowed_ips: ['192.168.1.1'],
    disabled_features: []
  },
  created_at: new Date(),
  updated_at: new Date()
};

// Verify that the generated types can be used directly
const directStorageObject: StorageStorageObject = storageObject;
const directBucket: StorageStorageBucket = bucket;
const directAuthUser: AuthUser = authUser;

console.log('Type compatibility test passed! ✅');
console.log('- User type extends AuthUser with roles field');
console.log('- StorageObject is aliased to StorageStorageObject');
console.log('- Bucket is aliased to StorageStorageBucket');
console.log('- IAMRole is properly imported from generated types');

export {};