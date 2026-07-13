import { ImpresspressClient } from '../src';

// Test configuration
export const TEST_CONFIG = {
  url: process.env.IMPRESSPRESS_URL || 'http://localhost:8092',
  apiKey: process.env.IMPRESSPRESS_API_KEY,
};

// Test user credentials
export const TEST_USER = {
  email: `test-${Date.now()}@example.com`,
  password: 'TestPassword123!',
  metadata: {
    name: 'Test User',
    role: 'tester',
  },
};

// Create a test client
export function createTestClient(): ImpresspressClient {
  return new ImpresspressClient(TEST_CONFIG);
}

// Helper to generate unique names
export function uniqueName(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.random().toString(36).substr(2, 9)}`;
}

// Helper to create test data
export function createTestData() {
  return {
    bucket: uniqueName('test-bucket'),
    collection: uniqueName('test-collection'),
    fileName: uniqueName('test-file') + '.txt',
    productGroup: uniqueName('test-group'),
    product: uniqueName('test-product'),
  };
}

// Clean up helper
export async function cleanup(client: ImpresspressClient, data: any) {
  try {
    // Clean up storage
    if (data.bucket) {
      try {
        await client.storage.deleteBucket(data.bucket);
      } catch (e) {
        // Ignore errors
      }
    }

    // Clean up collections
    if (data.collection) {
      try {
        await client.database.deleteCollection(data.collection);
      } catch (e) {
        // Ignore errors
      }
    }

    // Sign out
    await client.auth.signOut().catch(() => {});
  } catch (error) {
    console.error('Cleanup error:', error);
  }
}