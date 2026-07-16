import { ImpresspressClient } from '../src';

// Initialize the client
const impresspress = new ImpresspressClient({
  url: 'http://localhost:8090',
  // apiKey: 'your-api-key', // Optional: Use API key for server-side usage
});

async function main() {
  try {
    // ========================================
    // Authentication Examples
    // ========================================

    // Sign up a new user (auto-signs in unless email verification is required)
    const signUpResult = await impresspress.auth.signUp({
      email: 'user@example.com',
      password: 'SecurePassword123!',
      name: 'John Doe',
    });
    console.log('User created:', signUpResult.user, 'verified:', signUpResult.emailVerified);

    // Sign in
    await impresspress.auth.signIn({
      email: 'user@example.com',
      password: 'SecurePassword123!',
    });

    // Get current user
    const currentUser = await impresspress.auth.getUser();
    console.log('Current user:', currentUser);

    // ========================================
    // Storage Examples
    // ========================================

    // Create a bucket
    const bucket = await impresspress.storage.createBucket('my-files', false);
    console.log('Bucket created:', bucket);

    // Upload a file (in Node.js)
    const buffer = Buffer.from('Hello, World!', 'utf-8');
    const uploadedFile = await impresspress.storage.uploadFile('my-files', buffer, {
      key: 'hello.txt',
    });
    console.log('File uploaded:', uploadedFile);

    // List files in the bucket
    const files = await impresspress.storage.listObjects('my-files', { page_size: 10 });
    console.log('Files in bucket:', files.objects);

    // Get the direct download URL for a file
    const downloadUrl = impresspress.storage.getDownloadUrl('my-files', 'hello.txt');
    console.log('Download URL:', downloadUrl);

    // ========================================
    // Cloud Storage (sharing + quota) Examples
    // ========================================

    // Share the uploaded file
    const share = await impresspress.cloudStorage.share('my-files', 'hello.txt', {
      expiresInHours: 24,
    });
    console.log('File shared:', share);

    // Get quota info
    const quota = await impresspress.cloudStorage.getQuota();
    console.log('Storage quota:', quota);

    // ========================================
    // IAM (Identity & Access Management) Examples
    // ========================================

    // Get all roles
    const roles = await impresspress.iam.getRoles();
    console.log('Available roles:', roles);

    // Create a custom role
    const customRole = await impresspress.iam.createRole({
      name: 'editor',
      display_name: 'Content Editor',
      description: 'Can edit content but not delete',
      metadata: {
        allowed_ips: ['192.168.1.0/24'],
        disabled_features: ['delete', 'admin'],
      },
    });
    console.log('Custom role created:', customRole);

    // ========================================
    // Extensions Examples
    // ========================================

    // List available extensions
    const extensions = await impresspress.extensions.list();
    console.log('Available extensions:', extensions);

    // Browse the public product catalog
    const catalog = await impresspress.products.listProducts();
    console.log('Product catalog:', catalog.records);

    // Sign out
    await impresspress.auth.signOut();
  } catch (error) {
    console.error('Error:', error);
  }
}

// Run the examples
main();
