// Shared API types

export interface ApiResponse<T = any> {
	data?: T;
	error?: ApiError | string;
	message?: string;
	status?: number;
	success?: boolean;
	metadata?: ResponseMetadata;
}

export interface ApiError {
	code: string;
	message: string;
	details?: any;
	field?: string;
	timestamp?: string;
	requestId?: string;
}

export interface ResponseMetadata {
	page?: number;
	perPage?: number;
	total?: number;
	totalPages?: number;
	hasNext?: boolean;
	hasPrev?: boolean;
}

export interface PaginatedResponse<T> {
	data: T[];
	page: number;
	perPage: number;
	total: number;
	totalPages?: number;
	hasNext?: boolean;
	hasPrev?: boolean;
}

export interface UploadProgress {
	loaded: number;
	total: number;
	percentage: number;
}
