// Shared auth types
import type { UserResponse } from './models';

export interface LoginRequest {
	email: string;
	password: string;
	remember?: boolean;
}

export interface LoginResponse {
	data: UserResponse;
	message?: string;
}

export interface SignupRequest {
	email: string;
	password: string;
	username?: string;
	firstName?: string;
	lastName?: string;
}

export interface LoginCredentials {
	email: string;
	password: string;
	remember?: boolean;
}

export interface RegisterCredentials {
	email: string;
	password: string;
	confirmPassword?: string;
	username?: string;
	firstName?: string;
	lastName?: string;
	termsAccepted?: boolean;
}

export interface OAuthProvider {
	id: string;
	name: string;
	enabled: boolean;
	clientId?: string;
	icon?: string;
	color?: string;
}

export interface PasswordResetRequest {
	email: string;
}

export interface PasswordReset {
	token: string;
	newPassword: string;
	confirmPassword: string;
}

export interface EmailVerification {
	token: string;
	email?: string;
}

export interface TwoFactorAuth {
	enabled: boolean;
	method: 'totp' | 'sms' | 'email';
	verified: boolean;
	backupCodes?: string[];
}

export interface Permission {
	id: string;
	name: string;
	description?: string;
	resource?: string;
	action?: string;
	createdAt: string;
}
