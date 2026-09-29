// Main entry point for the Impresspress SDK

export { ImpresspressClient, createImpresspressClient } from './client';

// Export all services
export { AuthService } from './services/auth.service';
export { StorageService } from './services/storage.service';
export { IAMService } from './services/iam.service';
export * from './services/extensions.service';

// Export the SDK config type (every wire shape is exported from its service)
export type { ImpresspressConfig } from './types';

// Export the error type and helpers every service throws/maps
export {
  ImpresspressError,
  isNotFoundError,
  isStatementBudgetError,
  isUnauthorizedError,
  STATEMENT_BUDGET_DETAIL_CODES,
} from './error';
export type { SdkErrorCode } from './error';

// Export the one HTTP client every service shares, and its timeout vocabulary
export { HttpClient, DEFAULT_TIMEOUT_MS, NO_TIMEOUT } from './http-client';
export type { HttpClientConfig, HttpRequestOptions, HttpMethod } from './http-client';

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
  TransferOptions,
  UploadFileOptions,
  FileMetadataRecord,
  FileViewRecord,
  RecordListResult,
  SearchResult,
  RecentViewsResult,
} from './services/storage.service';

export type {
  IAMRole,
  IAMRoleListResponse,
  IAMRoleUpdateResponse,
  CreateRoleRequest,
  UpdateRoleRequest,
} from './services/iam.service';

// Default export
import { ImpresspressClient as Client } from './client';
export default Client;
