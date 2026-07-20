// Main entry point for the Impresspress SDK

export { ImpresspressClient, createImpresspressClient } from './client';

// Export all services
export { AuthService } from './services/auth.service';
export { StorageService } from './services/storage.service';
export { IAMService } from './services/iam.service';
export * from './services/extensions.service';

// Export types
export * from './types';

// Export the error type and helpers every service throws/maps
export { ImpresspressError, isNotFoundError, isUnauthorizedError } from './error';

// Export the OAuth popup abstraction (advanced usage — most callers just use
// `client.auth.signInWithOAuthPopup`)
export { PopupAuthSession } from './popup-auth-session';
export type { PopupAuthSessionOptions } from './popup-auth-session';

// Export static assets
export { IMPRESSPRESS_ASSETS, getImpresspressAssetPath, impresspressAssets } from './assets';

// Export service types
export type {
  OAuthProviderName,
  AuthSessionUser,
  AuthTokens,
  SignInResult,
  SignUpResult,
  SignUpOptions,
  SignInOptions,
  ResetPasswordOptions,
  UpdatePasswordOptions,
  OAuthPopupOptions,
} from './services/auth.service';

export type {
  StorageObjectInfo,
  ListObjectsResult,
  ListOptions,
  UploadFileOptions,
} from './services/storage.service';

// Default export
import { ImpresspressClient as Client } from './client';
export default Client;
