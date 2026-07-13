// Shared IAM types

export interface IAMPolicy {
	id?: string;
	subject: string;
	resource: string;
	action: string;
	effect: 'allow' | 'deny';
}

export interface IAMAuditLog {
	id: string;
	userId: string;
	action: string;
	resource: string;
	result: 'allowed' | 'denied';
	metadata?: Record<string, any>;
	createdAt: string;
}
