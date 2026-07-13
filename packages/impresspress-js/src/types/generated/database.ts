// Auto-generated from Go models - DO NOT EDIT MANUALLY
// Generated at: 2025-12-16T13:13:17+13:00
// Run 'go run scripts/generate-types.go' to regenerate

// ============================================
// Package: auth
// ============================================

export interface AuthAPIKey {
  id: string;
  userId: string;
  // User-friendly name like "Production Server"
  name: string;
  // First 8 chars for identification (e.g., "sb_live_")
  keyPrefix: string;
  // JSON array of scopes (for future use)
  scopes: string;
  // Optional expiration
  expiresAt?: string | Date | null;
  // Track usage
  lastUsedAt?: string | Date | null;
  // Last IP that used this key
  lastUsedIp?: string | null;
  // Soft revoke
  revokedAt?: string | Date | null;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface AuthToken {
  id: string;
  userId: string;
  // refresh, reset, confirm, oauth
  type: string;
  // google, github, microsoft, etc.
  provider?: string | null;
  expiresAt: string | Date;
  usedAt?: string | Date | null;
  revokedAt?: string | Date | null;
  createdAt: string | Date;
  // "Chrome on MacOS"
  deviceInfo?: string | null;
  // IPv6 max length
  ipAddress?: string | null;
}

export interface AuthUser {
  id: string;
  email: string;
  username: string;
  confirmed: boolean;
  firstName: string;
  lastName: string;
  displayName: string;
  phone: string;
  location: string;
  lastLogin?: string | Date | null;
  metadata: string;
  createdAt: string | Date;
  updatedAt: string | Date;
  deletedAt?: string | Date | null;
}

export interface AuthUserResponse {
  user?: any | null;
  roles: string[];
  permissions: string[];
}

// ============================================
// Package: cloudstorage
// ============================================

export interface CloudStorageRoleQuota {
  id: string;
  roleId: string;
  roleName: string;
  maxStorageBytes: number;
  maxBandwidthBytes: number;
  maxUploadSize: number;
  maxFilesCount: number;
  allowedExtensions: string;
  blockedExtensions: string;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface CloudStorageStorageAccessLog {
  id: string;
  objectId: string;
  userId?: string | null;
  ipAddress?: string | null;
  action: any;
  userAgent?: string | null;
  metadata: any;
  createdAt: string | Date;
}

export interface CloudStorageStorageQuota {
  id: string;
  userId: string;
  maxStorageBytes: number;
  maxBandwidthBytes: number;
  storageUsed: number;
  bandwidthUsed: number;
  resetBandwidthAt?: string | Date | null;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface CloudStorageStorageShare {
  id: string;
  objectId: string;
  sharedWithUserId?: string | null;
  sharedWithEmail?: string | null;
  permissionLevel: any;
  inheritToChildren: boolean;
  shareToken?: string | null;
  isPublic: boolean;
  expiresAt?: string | Date | null;
  createdBy: string;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface CloudStorageStorageShareWithObject {
  objectName: string;
  contentType: string;
  size: number;
  objectCreatedAt: string | Date;
  objectMetadata: any;
}

export interface CloudStorageUserQuotaOverride {
  id: string;
  userId: string;
  maxStorageBytes?: number | null;
  maxBandwidthBytes?: number | null;
  maxUploadSize?: number | null;
  maxFilesCount?: number | null;
  allowedExtensions?: string | null;
  blockedExtensions?: string | null;
  reason?: string | null;
  expiresAt?: string | Date | null;
  createdBy: string;
  createdAt: string | Date;
  updatedAt: string | Date;
}

// ============================================
// Package: iam
// ============================================

export interface IAMIAMAuditLog {
  id: string;
  userId: string;
  action: string;
  resource: string;
  // "allow" or "deny"
  result: string;
  reason: string;
  ipAddress: string;
  userAgent: string;
  metadata: Record<string, any>;
  createdAt: string | Date;
}

export interface IAMIAMPolicy {
  id: string;
  ptype: string;
  v0?: string | null;
  v1?: string | null;
  v2?: string | null;
  v3?: string | null;
  v4?: string | null;
  v5?: string | null;
  createdAt: string | Date;
}

export interface IAMRole {
  id: string;
  name: string;
  displayName: string;
  description: string;
  // "system" for protected roles, "custom" for user-created roles
  type: string;
  metadata: Record<string, any>;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface IAMRoleMetadata {
  // IP whitelist for access control
  allowedIps: string[];
  // List of disabled features for this role
  disabledFeatures: string[];
}

export interface IAMUserRole {
  id: string;
  userId: string;
  roleId: string;
  grantedBy: string;
  grantedAt: string | Date;
  expiresAt?: string | Date | null;
}

// ============================================
// Package: models
// ============================================

export interface ProductAppSettings {
  appName: string;
  appUrl: string;
  allowSignup: boolean;
  requireEmailConfirmation: boolean;
  smtpEnabled: boolean;
  smtpHost: string;
  smtpPort: number;
  smtpUser: string;
  storageProvider: string;
  s3Bucket: string;
  s3Region: string;
  maxUploadSize: number;
  allowedFileTypes: string;
  // in minutes
  sessionTimeout: number;
  passwordMinLength: number;
  enableApiLogs: boolean;
  enableDebugMode: boolean;
  maintenanceMode: boolean;
  maintenanceMessage: string;
  notification: string;
}

export interface ProductCustomTableDefinition {
  id: number;
  // Actual table name with custom_ prefix
  name: string;
  // User-friendly name without prefix
  displayName: string;
  description: string;
  // Column definitions
  fields: any[];
  // Index definitions
  indexes: any[];
  // Table options
  options: any;
  createdBy: string;
  // active, disabled, archived
  status: string;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface ProductCustomTableField {
  // Column name
  name: string;
  // Data type: string, int, float, bool, time, json
  type: string;
  // For varchar(n)
  size: number;
  nullable: boolean;
  defaultValue: any;
  isPrimaryKey: boolean;
  isUnique: boolean;
  isIndexed: boolean;
  autoIncrement: boolean;
  description: string;
  foreignKey?: any | null;
  validation: any;
}

export interface ProductCustomTableIndex {
  name: string;
  columns: string[];
  unique: boolean;
  // btree, hash, gin, gist (PostgreSQL specific)
  type: string;
}

export interface ProductCustomTableMigration {
  id: number;
  tableId: number;
  version: number;
  // create, alter, drop
  migrationType: string;
  oldSchema: any;
  newSchema: any;
  executedBy: string;
  executedAt: string | Date;
  rollbackAt?: string | Date | null;
  // pending, completed, failed, rolled_back
  status: string;
  errorMessage: string;
}

export interface ProductCustomTableOptions {
  // Add deleted_at field
  softDelete: boolean;
  // Add created_at, updated_at
  timestamps: boolean;
  // Add version field for optimistic locking
  versioning: boolean;
  // Track changes in audit log
  auditing: boolean;
  // Enable query caching
  cacheEnabled: boolean;
  // Maximum allowed rows
  maxRows: number;
}

export interface ProductFieldConstraints {
  required: boolean;
  // For numeric/range types
  min?: number | null;
  // For numeric/range types
  max?: number | null;
  // For text types
  minLength?: number | null;
  // For text types
  maxLength?: number | null;
  // Regex pattern for validation
  pattern: string;
  // For select/enum types
  options: string[];
  // Default value
  default: any;
  // UI placeholder text
  placeholder: string;
  // For textarea type
  rows?: number | null;
  // For numeric/range types
  step?: number | null;
  // Whether the user can edit this field
  editableByUser: boolean;
}

export interface ProductFieldDefinition {
  // For filter fields: "filter_text_1", etc. For custom fields: any unique ID
  id: string;
  // Display name for the field
  name: string;
  // text, numeric, boolean, enum, color, date, email, url, textarea, etc.
  type: string;
  required: boolean;
  description: string;
  // Section/tab this field belongs to (for UI organization)
  section: string;
  // Display order within section
  order: number;
  // All validation constraints
  constraints: any;
}

export interface ProductFieldValidation {
  minLength?: number | null;
  maxLength?: number | null;
  minValue?: number | null;
  maxValue?: number | null;
  // Regex pattern
  pattern: string;
  // Allowed values
  enumValues: string[];
  required: boolean;
}

export interface ProductForeignKeyDef {
  // Table to reference (with custom_ prefix)
  referenceTable: string;
  // Column in reference table
  referenceColumn: string;
  // CASCADE, SET NULL, RESTRICT
  onDelete: string;
  // CASCADE, SET NULL, RESTRICT
  onUpdate: string;
}

export interface ProductGroup {
  id: number;
  userId: string;
  groupTemplateId: number;
  groupTemplate: any;
  name: string;
  description: string;
  filterNumeric1?: number | null;
  filterNumeric2?: number | null;
  filterNumeric3?: number | null;
  filterNumeric4?: number | null;
  filterNumeric5?: number | null;
  filterText1?: string | null;
  filterText2?: string | null;
  filterText3?: string | null;
  filterText4?: string | null;
  filterText5?: string | null;
  filterBoolean1?: boolean | null;
  filterBoolean2?: boolean | null;
  filterBoolean3?: boolean | null;
  filterBoolean4?: boolean | null;
  filterBoolean5?: boolean | null;
  filterEnum1?: string | null;
  filterEnum2?: string | null;
  filterEnum3?: string | null;
  filterEnum4?: string | null;
  filterEnum5?: string | null;
  // Store as GeoJSON or lat,lng
  filterLocation1?: string | null;
  filterLocation2?: string | null;
  filterLocation3?: string | null;
  filterLocation4?: string | null;
  filterLocation5?: string | null;
  // Additional non-indexed fields
  customFields: Record<string, any>;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface ProductGroupTemplate {
  id: number;
  name: string;
  displayName: string;
  description: string;
  icon: string;
  // Filter field definitions
  filterFieldsSchema: any[];
  // active, pending, deleted
  status: string;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface ProductPricingTemplate {
  id: number;
  name: string;
  displayName: string;
  description: string;
  priceFormula: string;
  conditionFormula: string;
  variables: Record<string, any>;
  category: string;
  // active, pending, deleted
  status: string;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface ProductProduct {
  id: number;
  groupId: number;
  group: any;
  productTemplateId: number;
  productTemplate: any;
  name: string;
  description: string;
  basePrice: number;
  currency: string;
  filterNumeric1?: number | null;
  filterNumeric2?: number | null;
  filterNumeric3?: number | null;
  filterNumeric4?: number | null;
  filterNumeric5?: number | null;
  filterText1?: string | null;
  filterText2?: string | null;
  filterText3?: string | null;
  filterText4?: string | null;
  filterText5?: string | null;
  filterBoolean1?: boolean | null;
  filterBoolean2?: boolean | null;
  filterBoolean3?: boolean | null;
  filterBoolean4?: boolean | null;
  filterBoolean5?: boolean | null;
  filterEnum1?: string | null;
  filterEnum2?: string | null;
  filterEnum3?: string | null;
  filterEnum4?: string | null;
  filterEnum5?: string | null;
  // Store as GeoJSON or lat,lng
  filterLocation1?: string | null;
  filterLocation2?: string | null;
  filterLocation3?: string | null;
  filterLocation4?: string | null;
  filterLocation5?: string | null;
  // Additional non-indexed fields
  customFields: Record<string, any>;
  // Product-specific variable values
  variables: Record<string, any>;
  pricingFormula: string;
  active: boolean;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface ProductProductTemplate {
  id: number;
  name: string;
  displayName: string;
  description: string;
  category: string;
  icon: string;
  // Filter field definitions (indexed, mapped to filter columns)
  filterFieldsSchema: any[];
  // Custom field definitions (non-indexed, stored in CustomFields JSON)
  customFieldsSchema: any[];
  // IDs of pricing templates to use
  pricingTemplates: number[];
  // instant, approval
  billingMode: string;
  // one-time, recurring
  billingType: string;
  billingRecurringInterval?: string | null;
  billingRecurringIntervalCount?: number | null;
  // active, pending, deleted
  status: string;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface ProductSetting {
  id: string;
  key: string;
  value: string;
  // string, bool, int, json
  type: string;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface ProductStorageDownloadToken {
  id: string;
  token: string;
  fileId: string;
  bucket: string;
  // Parent folder ID (null for root)
  parentFolderId?: string | null;
  // The file name
  objectName: string;
  userId: string;
  fileSize: number;
  bytesServed: number;
  completed: boolean;
  expiresAt: string | Date;
  createdAt: string | Date;
  callbackAt?: string | Date | null;
  clientIp: string;
}

export interface ProductStorageUploadToken {
  id: string;
  token: string;
  bucket: string;
  // Parent folder ID (null for root)
  parentFolderId?: string | null;
  // The file name
  objectName: string;
  userId: string;
  // Maximum allowed file size
  maxSize: number;
  // Expected content type
  contentType: string;
  bytesUploaded: number;
  completed: boolean;
  // ID of created storage object
  objectId: string;
  expiresAt: string | Date;
  createdAt: string | Date;
  completedAt?: string | Date | null;
  clientIp: string;
}

export interface ProductVariable {
  id: number;
  name: string;
  displayName: string;
  // number, string, boolean
  valueType: string;
  // user, system
  type: string;
  defaultValue: any;
  description: string;
  // active, pending, deleted
  status: string;
  createdAt: string | Date;
  updatedAt: string | Date;
}

// ============================================
// Package: storage
// ============================================

export interface StorageStorageBucket {
  id: string;
  name: string;
  public: boolean;
  createdAt: string | Date;
  updatedAt: string | Date;
}

export interface StorageStorageObject {
  id: string;
  bucketName: string;
  // Just the name (file.txt or foldername)
  objectName: string;
  // ID of parent folder, null for root items
  parentFolderId?: string | null;
  size: number;
  // "application/x-directory" for folders
  contentType: string;
  // MD5 or SHA256 hash
  checksum: string;
  // JSON string
  metadata: string;
  createdAt: string | Date;
  updatedAt: string | Date;
  // Track when the item was last viewed
  lastViewed?: string | Date | null;
  userId: string;
  // Application ID, null for admin uploads
  appId?: string | null;
}

// ============================================
// Helper Types
// ============================================

export type UUID = string;
export type DateTime = string | Date;
export type NullableDateTime = DateTime | null;
export type JSONData = Record<string, any>;

// ============================================
// Table Names
// ============================================

export const TableNames = {
} as const;

export type TableName = typeof TableNames[keyof typeof TableNames];
