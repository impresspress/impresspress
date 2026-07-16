// Re-export all types (inlined from @impresspress/types)
export * from './generated/database';
export * from './models';
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
