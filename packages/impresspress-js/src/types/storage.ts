// Shared storage types
import type { StorageObject } from './models';

// Metadata structure (stored as JSON string in StorageObject.metadata)
export interface StorageObjectMetadata {
	icon?: string;
	description?: string;
	date?: string;
	order?: number;
	path?: string;
	tags?: string[];
	color?: string;
	starred?: boolean;
	[key: string]: any;
}

// Helper functions
export function isFolder(obj: StorageObject): boolean {
	return obj.contentType === 'application/x-directory';
}

export function isFile(obj: StorageObject): boolean {
	return obj.contentType !== 'application/x-directory';
}

export function parseMetadata(obj: StorageObject): StorageObjectMetadata | null {
	if (!obj.metadata) return null;
	try {
		return typeof obj.metadata === 'string' ? JSON.parse(obj.metadata) : obj.metadata;
	} catch {
		return null;
	}
}

export function getDisplayName(obj: StorageObject): string {
	return obj.objectName || 'Unnamed';
}

export function getFileExtension(obj: StorageObject): string {
	if (isFolder(obj)) return '';
	const parts = obj.objectName.split('.');
	return parts.length > 1 ? parts[parts.length - 1] : '';
}
