// Main entry point for the Impresspress SDK

export { ImpresspressClient, createImpresspressClient } from './client';

// Export all services
export { AuthService } from './services/auth.service';
export { StorageService } from './services/storage.service';
export { DatabaseService } from './services/database.service';
export { IAMService } from './services/iam.service';
export {
  ExtensionsService,
  CloudStorageExtension,
  ProductsExtension,
} from './services/extensions.service';

// Export types
export * from './types';

// Export static assets
export { IMPRESSPRESS_ASSETS, getImpresspressAssetPath, impresspressAssets } from './assets';

// Export service types
export type {
  SignUpOptions,
  SignInOptions,
  ResetPasswordOptions,
  UpdatePasswordOptions,
} from './services/auth.service';

export type {
  ListOptions,
  UploadFileOptions,
  MoveOptions,
  ShareOptions,
} from './services/storage.service';

export type {
  DatabaseRecord,
  CreateRecordOptions,
  UpdateRecordOptions,
  QueryBuilder,
} from './services/database.service';

export type {
  Extension,
} from './services/extensions.service';

// Default export
import { ImpresspressClient as Client } from './client';
export default Client;