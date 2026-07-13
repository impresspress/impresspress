// Shared model extensions - built on top of generated types
import type {
	AuthUser,
	AuthToken,
	StorageStorageObject,
	StorageStorageBucket,
	IAMRole
} from './generated/database';

// Re-export base types
export type { AuthUser, AuthToken, StorageStorageObject, StorageStorageBucket, IAMRole };

/**
 * User type - alias for AuthUser (for SDK and backwards compatibility)
 */
export type User = AuthUser;

/**
 * UserResponse - API response structure for user data
 * Separates database fields (user) from runtime fields (roles, permissions)
 * Matches Go's auth.UserResponse struct
 */
export interface UserResponse {
	user: AuthUser;
	roles: string[];
	permissions?: string[];
}

/**
 * Token type - alias for AuthToken
 */
export type Token = AuthToken;

/**
 * Role type - alias for IAMRole
 */
export type Role = IAMRole;

/**
 * StorageObject type - alias for StorageStorageObject
 */
export type StorageObject = StorageStorageObject;

/**
 * Bucket type - alias for StorageStorageBucket
 */
export type Bucket = StorageStorageBucket;

/**
 * StorageBucket - alias for Bucket
 */
export type StorageBucket = Bucket;
