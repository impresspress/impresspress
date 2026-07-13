// Re-export all types (inlined from @impresspress/types)
export * from './generated/database';
export * from './models';
export * from './api';
export * from './auth';
export * from './storage';
export * from './iam';

// SDK-specific types

export interface ImpresspressConfig {
	url: string;
	/** URL for the auth UI (login page). Defaults to `url` if not specified. */
	authUrl?: string;
	apiKey?: string;
	headers?: Record<string, string>;
	timeout?: number;
}

export interface Collection {
	id: string;
	name: string;
	schema?: Record<string, any>;
	createdAt: string;
	updatedAt: string;
}

export interface QueryOptions {
	limit?: number;
	offset?: number;
	order?: string;
	filter?: Record<string, any>;
}

// SDK-specific upload options (different from frontend's UploadOptions)
export interface UploadOptions {
	contentType?: string;
	metadata?: Record<string, any>;
	public?: boolean;
	onProgress?: (progress: number) => void;
}
