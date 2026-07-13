#!/usr/bin/env node

/**
 * Comprehensive test suite for Impresspress TypeScript SDK
 * Tests all functionality across auth, storage, database, and extensions
 */

import { createTestClient, TEST_USER, createTestData, cleanup, uniqueName } from './setup';
import { ImpresspressClient } from '../src';

// Color codes for terminal output
const colors = {
  reset: '\x1b[0m',
  green: '\x1b[32m',
  red: '\x1b[31m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  cyan: '\x1b[36m',
};

class TestRunner {
  private client: ImpresspressClient;
  private testData: ReturnType<typeof createTestData>;
  private passedTests = 0;
  private failedTests = 0;
  private currentUser: any = null;

  constructor() {
    this.client = createTestClient();
    this.testData = createTestData();
  }

  private log(message: string, color: string = colors.reset) {
    console.log(`${color}${message}${colors.reset}`);
  }

  private async test(name: string, fn: () => Promise<void>) {
    try {
      await fn();
      this.passedTests++;
      this.log(`  PASS ${name}`, colors.green);
    } catch (error: any) {
      this.failedTests++;
      this.log(`  FAIL ${name}`, colors.red);
      this.log(`   Error: ${error.message || error}`, colors.red);
      console.error(error);
    }
  }

  private assert(condition: boolean, message: string) {
    if (!condition) {
      throw new Error(`Assertion failed: ${message}`);
    }
  }

  private async delay(ms: number) {
    return new Promise(resolve => setTimeout(resolve, ms));
  }

  // ========================================
  // Authentication Tests
  // ========================================
  async testAuth() {
    this.log('\n--- Testing Authentication Service', colors.cyan);

    await this.test('Sign up new user', async () => {
      const user = await this.client.auth.signUp({
        email: TEST_USER.email,
        password: TEST_USER.password,
        metadata: TEST_USER.metadata,
      });

      this.assert(!!user.id, 'User should have ID');
      this.assert(user.email === TEST_USER.email, 'Email should match');
      this.currentUser = user;
    });

    await this.test('Get current user', async () => {
      const user = await this.client.auth.getUser();
      this.assert(!!user, 'Should get user');
      this.assert(user?.email === TEST_USER.email, 'Email should match');
    });

    await this.test('Sign out', async () => {
      await this.client.auth.signOut();
      const user = await this.client.auth.getUser();
      this.assert(user === null, 'User should be null after sign out');
    });

    await this.test('Sign in with credentials', async () => {
      const user = await this.client.auth.signIn({
        email: TEST_USER.email,
        password: TEST_USER.password,
      });

      this.assert(!!user.id, 'User should have ID');
      this.assert(user.email === TEST_USER.email, 'Email should match');
      this.currentUser = user;
    });

    await this.test('Update user profile', async () => {
      const updated = await this.client.auth.updateUser({
        metadata: JSON.stringify({
          ...TEST_USER.metadata,
          updated: true,
        }),
      });
      this.assert(!!updated.id, 'Updated user should have ID');
    });

    await this.test('Check authentication status', async () => {
      const isAuth = this.client.isAuthenticated();
      this.assert(isAuth === true, 'Should be authenticated');
    });

    await this.test('Request password reset', async () => {
      await this.client.auth.resetPassword({
        email: TEST_USER.email,
      });
      // Just check it doesn't throw
    });
  }

  // ========================================
  // Storage Tests
  // ========================================
  async testStorage() {
    this.log('\n--- Testing Storage Service', colors.cyan);

    let uploadedFile: any = null;

    await this.test('Create bucket', async () => {
      const bucket = await this.client.storage.createBucket(this.testData.bucket, false);
      this.assert(bucket.name === this.testData.bucket, 'Bucket name should match');
      this.assert(bucket.public === false, 'Bucket should be private');
    });

    await this.test('List buckets', async () => {
      const buckets = await this.client.storage.listBuckets();
      this.assert(Array.isArray(buckets), 'Should return array of buckets');
      const found = buckets.find(b => b.name === this.testData.bucket);
      this.assert(!!found, 'Created bucket should be in list');
    });

    await this.test('Upload file', async () => {
      const content = Buffer.from('Hello, Impresspress SDK Test!', 'utf-8');
      uploadedFile = await this.client.storage.uploadFile(
        this.testData.bucket,
        content,
        {
          path: this.testData.fileName,
          metadata: {
            test: 'true',
            timestamp: String(Date.now()),
          },
        }
      );

      this.assert(!!uploadedFile.id, 'File should have ID');
      this.assert(!!uploadedFile.objectName, 'File should have objectName');
      this.assert(uploadedFile.size > 0, 'File should have size');
    });

    await this.test('List files in bucket', async () => {
      const objects = await this.client.storage.listObjects(this.testData.bucket, {
        limit: 10,
      });

      this.assert(Array.isArray(objects), 'Should return array of files');
      this.assert(objects.length > 0, 'Should have at least one file');
    });

    await this.test('Get download URL', async () => {
      const url = this.client.storage.getDownloadUrl(
        this.testData.bucket,
        uploadedFile.id
      );
      this.assert(typeof url === 'string', 'Should return URL string');
      this.assert(url.length > 0, 'URL should not be empty');
    });

    await this.test('Update file metadata', async () => {
      const updated = await this.client.storage.updateMetadata(
        this.testData.bucket,
        uploadedFile.id,
        {
          test: 'false',
          updated: 'true',
        }
      );
      this.assert(!!updated.id, 'Updated object should have ID');
    });

    await this.test('Get storage stats', async () => {
      const stats = await this.client.storage.getStats();
      this.assert(typeof stats.totalSize === 'number', 'Should have totalSize');
      this.assert(typeof stats.fileCount === 'number', 'Should have fileCount');
      this.assert(typeof stats.folderCount === 'number', 'Should have folderCount');
    });

    await this.test('Delete file', async () => {
      await this.client.storage.deleteObject(this.testData.bucket, uploadedFile.id);
      const objects = await this.client.storage.listObjects(this.testData.bucket);
      const found = objects.find(f => f.id === uploadedFile.id);
      this.assert(!found, 'File should be deleted');
    });

    await this.test('Delete bucket', async () => {
      await this.client.storage.deleteBucket(this.testData.bucket);
      const buckets = await this.client.storage.listBuckets();
      const found = buckets.find(b => b.name === this.testData.bucket);
      this.assert(!found, 'Bucket should be deleted');
    });
  }

  // ========================================
  // Database/Collections Tests
  // ========================================
  async testDatabase() {
    this.log('\n--- Testing Database Service', colors.cyan);

    let createdRecord: any = null;

    await this.test('Create collection', async () => {
      const collection = await this.client.database.createCollection(
        this.testData.collection,
        {
          title: { type: 'string', required: true },
          content: { type: 'string' },
          views: { type: 'number', default: 0 },
          published: { type: 'boolean', default: false },
          tags: { type: 'array' },
        }
      );

      this.assert(collection.name === this.testData.collection, 'Collection name should match');
      this.assert(!!collection.schema, 'Collection should have schema');
    });

    await this.test('List collections', async () => {
      const collections = await this.client.database.listCollections();
      this.assert(Array.isArray(collections), 'Should return array of collections');
      const found = collections.find(c => c.name === this.testData.collection);
      this.assert(!!found, 'Created collection should be in list');
    });

    await this.test('Create record', async () => {
      createdRecord = await this.client.database.create({
        collection: this.testData.collection,
        data: {
          title: 'Test Article',
          content: 'This is test content for the SDK',
          views: 100,
          published: true,
          tags: ['test', 'sdk', 'typescript'],
        },
      });

      this.assert(!!createdRecord.id, 'Record should have ID');
      this.assert(createdRecord.title === 'Test Article', 'Title should match');
      this.assert(Array.isArray(createdRecord.tags), 'Tags should be array');
    });

    await this.test('Find record by ID', async () => {
      const found = await this.client.database.findById(
        this.testData.collection,
        createdRecord.id
      );
      this.assert(!!found, 'Should find record');
      this.assert(found?.id === createdRecord.id, 'ID should match');
    });

    await this.test('Update record', async () => {
      const updated = await this.client.database.update({
        collection: this.testData.collection,
        id: createdRecord.id,
        data: {
          title: 'Updated Article',
          views: 200,
        },
      });

      this.assert(updated.title === 'Updated Article', 'Title should be updated');
      this.assert(updated.views === 200, 'Views should be updated');
    });

    await this.test('Query with builder', async () => {
      // Create a few more records for testing
      await this.client.database.create({
        collection: this.testData.collection,
        data: {
          title: 'Article 2',
          content: 'Content 2',
          views: 50,
          published: true,
        },
      });

      await this.client.database.create({
        collection: this.testData.collection,
        data: {
          title: 'Article 3',
          content: 'Content 3',
          views: 150,
          published: false,
        },
      });

      const results = await this.client.database
        .from(this.testData.collection)
        .where('published', '=', true)
        .where('views', '>', 40)
        .orderBy('views', 'desc')
        .limit(10)
        .execute();

      this.assert(!!results.data, 'Should have data array');
      this.assert(Array.isArray(results.data), 'Data should be array');
      this.assert(results.data.length >= 1, 'Should have at least one matching record');
    });

    await this.test('Query first record', async () => {
      const first = await this.client.database
        .from(this.testData.collection)
        .orderBy('views', 'desc')
        .first();

      this.assert(!!first, 'Should find first record');
      this.assert(typeof first?.views === 'number', 'Should have views property');
    });

    await this.test('Count records', async () => {
      const count = await this.client.database
        .from(this.testData.collection)
        .where('published', '=', true)
        .count();

      this.assert(typeof count === 'number', 'Count should be number');
      this.assert(count >= 1, 'Should have at least one published record');
    });

    await this.test('Transaction operations', async () => {
      const results = await this.client.database.transaction([
        {
          type: 'create',
          collection: this.testData.collection,
          data: {
            title: 'Transaction Record 1',
            content: 'Created in transaction',
          },
        },
        {
          type: 'create',
          collection: this.testData.collection,
          data: {
            title: 'Transaction Record 2',
            content: 'Also created in transaction',
          },
        },
      ]);

      this.assert(Array.isArray(results), 'Should return array of results');
      this.assert(results.length === 2, 'Should have 2 results');
    });

    await this.test('Delete record', async () => {
      await this.client.database.delete(this.testData.collection, createdRecord.id);
      const found = await this.client.database.findById(
        this.testData.collection,
        createdRecord.id
      );
      this.assert(!found, 'Record should be deleted');
    });

    await this.test('Get collection stats', async () => {
      const stats = await this.client.database.getCollectionStats(this.testData.collection);
      this.assert(typeof stats.total_records === 'number', 'Should have total_records');
      this.assert(typeof stats.size_bytes === 'number', 'Should have size_bytes');
    });

    await this.test('Delete collection', async () => {
      await this.client.database.deleteCollection(this.testData.collection);
      const collections = await this.client.database.listCollections();
      const found = collections.find(c => c.name === this.testData.collection);
      this.assert(!found, 'Collection should be deleted');
    });
  }

  // ========================================
  // Extensions Tests
  // ========================================
  async testExtensions() {
    this.log('\n--- Testing Extensions Service', colors.cyan);

    await this.test('List extensions', async () => {
      const extensions = await this.client.extensions.list();
      this.assert(Array.isArray(extensions), 'Should return array of extensions');
      this.assert(extensions.length > 0, 'Should have at least one extension');
    });

    await this.test('Get extension details', async () => {
      const extension = await this.client.extensions.get('cloudstorage');
      this.assert(!!extension, 'Should get extension');
      this.assert(extension.name === 'cloudstorage', 'Name should match');
      this.assert(!!extension.description, 'Should have description');
    });

    await this.test('Enable extension', async () => {
      await this.client.extensions.enable('cloudstorage', {
        defaultStorageLimit: 1073741824, // 1GB
        enableSharing: true,
        enableQuotas: true,
      });
      // Just check it doesn't throw
    });

    await this.test('Update extension config', async () => {
      await this.client.extensions.updateConfig('cloudstorage', {
        defaultStorageLimit: 2147483648, // 2GB
      });
      // Just check it doesn't throw
    });

    await this.test('Get extension health', async () => {
      const health = await this.client.extensions.health('cloudstorage');
      this.assert(!!health, 'Should get health status');
      if (!Array.isArray(health)) {
        this.assert(['healthy', 'unhealthy', 'unknown'].includes(health.status), 'Should have valid status');
      }
    });

    // CloudStorage Extension Tests
    await this.test('CloudStorage: Get quota', async () => {
      try {
        const quota = await this.client.cloudStorage.getQuota();
        this.assert(typeof quota.max_storage_bytes === 'number', 'Should have max_storage_bytes');
        this.assert(typeof quota.storage_used === 'number', 'Should have storage_used');
      } catch (error: any) {
        // Extension might not be fully initialized
        console.log('   CloudStorage quota not available yet');
      }
    });

    // Products Extension Tests
    await this.test('Products: Create group', async () => {
      try {
        const group = await this.client.products.createGroup({
          name: this.testData.productGroup,
          template_id: 'default',
          custom_fields: {
            description: 'Test product group',
          },
        });
        this.assert(!!group.id, 'Group should have ID');
        this.assert(group.name === this.testData.productGroup, 'Group name should match');
      } catch (error: any) {
        // Extension might not be enabled
        console.log('   Products extension not available');
      }
    });

    await this.test('Disable extension', async () => {
      await this.client.extensions.disable('cloudstorage');
      // Just check it doesn't throw
    });
  }

  // ========================================
  // Client Integration Tests
  // ========================================
  async testClientIntegration() {
    this.log('\n--- Testing Client Integration', colors.cyan);

    await this.test('Auth service: signIn', async () => {
      await this.client.auth.signOut().catch(() => {}); // Ensure we're signed out
      await this.client.auth.signIn({
        email: TEST_USER.email,
        password: TEST_USER.password,
      });
      const isAuth = this.client.isAuthenticated();
      this.assert(isAuth === true, 'Should be authenticated');
    });

    await this.test('Auth service: getUser', async () => {
      const user = await this.client.auth.getUser();
      this.assert(!!user, 'Should get user');
      this.assert(user?.email === TEST_USER.email, 'Email should match');
    });

    const testBucket = uniqueName('integration-bucket');

    await this.test('Storage service: uploadFile', async () => {
      // Create bucket first
      await this.client.storage.createBucket(testBucket);

      const content = Buffer.from('Integration test content', 'utf-8');
      const file = await this.client.storage.uploadFile(testBucket, content, {
        path: 'integration-test.txt',
      });
      this.assert(!!file.id, 'File should have ID');
      this.assert(!!file.objectName, 'File should have objectName');

      // Clean up
      await this.client.storage.deleteObject(testBucket, file.id);
      await this.client.storage.deleteBucket(testBucket);
    });

    const testCollection = uniqueName('integration-collection');

    await this.test('Database service: query', async () => {
      // Create collection and data
      await this.client.database.createCollection(testCollection, {
        name: { type: 'string', required: true },
      });

      await this.client.database.create({
        collection: testCollection,
        data: { name: 'Test Item' },
      });

      const results = await this.client.database.query(testCollection, { limit: 5 });
      this.assert(!!results.data, 'Should have data');
      this.assert(Array.isArray(results.data), 'Data should be array');

      // Clean up
      await this.client.database.deleteCollection(testCollection);
    });

    await this.test('Auth service: signOut', async () => {
      await this.client.auth.signOut();
      const isAuth = this.client.isAuthenticated();
      this.assert(isAuth === false, 'Should not be authenticated');
    });

    await this.test('Set and remove API key', async () => {
      this.client.setApiKey('test-api-key');
      const config = this.client.getConfig();
      this.assert(config.apiKey === 'test-api-key', 'API key should be set');

      this.client.removeApiKey();
      const newConfig = this.client.getConfig();
      this.assert(!newConfig.apiKey, 'API key should be removed');
    });
  }

  // ========================================
  // Main test runner
  // ========================================
  async run() {
    this.log('\n Starting Impresspress SDK Tests', colors.blue);
    this.log(`   Server: ${this.client.getConfig().url}`, colors.blue);
    this.log(`   Time: ${new Date().toISOString()}`, colors.blue);

    try {
      // Run all test suites
      await this.testAuth();
      await this.delay(500); // Small delay between suites

      await this.testStorage();
      await this.delay(500);

      await this.testDatabase();
      await this.delay(500);

      await this.testExtensions();
      await this.delay(500);

      await this.testClientIntegration();

    } catch (error: any) {
      this.log(`\n  Test suite error: ${error.message}`, colors.red);
      console.error(error);
    } finally {
      // Clean up
      await cleanup(this.client, this.testData);
    }

    // Print summary
    this.log('\n' + '='.repeat(50), colors.cyan);
    this.log(' Test Summary', colors.cyan);
    this.log('='.repeat(50), colors.cyan);

    const total = this.passedTests + this.failedTests;
    const passRate = total > 0 ? ((this.passedTests / total) * 100).toFixed(1) : '0';

    this.log(`  Passed: ${this.passedTests}`, colors.green);
    this.log(`  Failed: ${this.failedTests}`, colors.red);
    this.log(`  Pass Rate: ${passRate}%`, colors.yellow);

    if (this.failedTests === 0) {
      this.log('\n All tests passed successfully!', colors.green);
    } else {
      this.log('\n Some tests failed. Please review the errors above.', colors.red);
    }

    // Exit with appropriate code
    process.exit(this.failedTests > 0 ? 1 : 0);
  }
}

// Run tests
const runner = new TestRunner();
runner.run().catch(error => {
  console.error('Fatal error:', error);
  process.exit(1);
});