import { BaseService } from "./base.service";
import { isNotFoundError, isUnauthorizedError } from "../error";
import { PopupAuthSession } from "../popup-auth-session";

/**
 * The only providers `crates/impresspress-core/src/blocks/auth_ui/oauth/spec.rs`
 * (`OAUTH_PROVIDERS`) actually implements. There is no "facebook" provider
 * server-side.
 */
export type OAuthProviderName = "google" | "github" | "microsoft";

/**
 * The real shape returned under the `user` key by `POST /login`,
 * `POST /signup`, and `GET /me` — see `blocks/auth_ui/api/{login,signup,me}.rs`.
 * Distinct from the generated `AuthUser`/`User` DB-row types, which model a
 * different (out of date) column set that these endpoints never return.
 */
export interface AuthSessionUser {
  id: string;
  email: string;
  name?: string;
  roles: string[];
  created_at?: string;
  avatar_url?: string;
}

export interface AuthTokens {
  access_token: string;
  refresh_token: string;
  token_type: string;
  expires_in: number;
}

export interface SignInResult {
  user: AuthSessionUser;
  tokens: AuthTokens;
  default_redirect: string;
}

export interface SignUpResult {
  user: { id: string; email: string; name?: string; roles?: string[] };
  emailVerified: boolean;
  message?: string;
  tokens?: AuthTokens;
  default_redirect?: string;
}

export interface SignUpOptions {
  email: string;
  password: string;
  name?: string;
}

export interface SignInOptions {
  email: string;
  password: string;
}

export interface ResetPasswordOptions {
  email: string;
}

export interface UpdatePasswordOptions {
  currentPassword: string;
  newPassword: string;
}

export interface OAuthPopupOptions {
  signal?: AbortSignal;
}

export class AuthService extends BaseService {
  private currentUser: AuthSessionUser | null = null;
  private tokens: AuthTokens | null = null;
  // Authentication is enforced via an httpOnly cookie set by the server;
  // `tokens` is cached only so `refreshSession()` has something to send
  // without the caller re-plumbing the refresh token through by hand.

  /** Create an account. Auto-signs in unless email verification is required. */
  async signUp(options: SignUpOptions): Promise<SignUpResult> {
    const res = await this.request<{
      email_verified: boolean;
      message?: string;
      access_token?: string;
      refresh_token?: string;
      token_type?: string;
      expires_in?: number;
      default_redirect?: string;
      user: { id: string; email: string; name?: string; roles?: string[] };
    }>({
      method: "POST",
      url: "/b/auth/api/signup",
      data: options,
    });

    const tokens =
      res.access_token && res.refresh_token
        ? {
            access_token: res.access_token,
            refresh_token: res.refresh_token,
            token_type: res.token_type ?? "Bearer",
            expires_in: res.expires_in ?? 0,
          }
        : undefined;

    if (tokens) {
      this.tokens = tokens;
      this.currentUser = res.user as AuthSessionUser;
    }

    return {
      user: res.user,
      emailVerified: res.email_verified,
      message: res.message,
      tokens,
      default_redirect: res.default_redirect,
    };
  }

  /** Sign in with email/password. */
  async signIn(options: SignInOptions): Promise<SignInResult> {
    const res = await this.request<{
      access_token: string;
      refresh_token: string;
      token_type: string;
      expires_in: number;
      default_redirect: string;
      user: AuthSessionUser;
    }>({
      method: "POST",
      url: "/b/auth/api/login",
      data: options,
    });

    this.tokens = {
      access_token: res.access_token,
      refresh_token: res.refresh_token,
      token_type: res.token_type,
      expires_in: res.expires_in,
    };
    this.currentUser = res.user;

    return { user: res.user, tokens: this.tokens, default_redirect: res.default_redirect };
  }

  /** Sign out the current user. */
  async signOut(): Promise<void> {
    try {
      await this.request<void>({
        method: "POST",
        url: "/b/auth/api/logout",
      });
    } finally {
      this.currentUser = null;
      this.tokens = null;
    }
  }

  /**
   * Get the current authenticated user (`GET /b/auth/api/me`, which returns
   * `{ user }`). Resolves `null` only for the "not signed in" case
   * (401/404) — every other failure (outage, WRAP denial, ...) propagates
   * rather than being silently swallowed into "logged out".
   */
  async getUser(): Promise<AuthSessionUser | null> {
    try {
      const res = await this.request<{ user: AuthSessionUser }>({
        method: "GET",
        url: "/b/auth/api/me",
      });
      this.currentUser = res.user;
      return res.user;
    } catch (error) {
      if (isUnauthorizedError(error) || isNotFoundError(error)) {
        this.currentUser = null;
        return null;
      }
      throw error;
    }
  }

  /**
   * Update the current user's profile. Only `name` and `avatar_url` are
   * accepted server-side (`api/me.rs::handle_update`); unlike `GET /me`,
   * the update response is the user object directly, not `{ user }`.
   */
  async updateUser(updates: { name?: string; avatar_url?: string }): Promise<AuthSessionUser> {
    const user = await this.request<AuthSessionUser>({
      method: "PATCH",
      url: "/b/auth/api/me",
      data: updates,
    });
    this.currentUser = user;
    return user;
  }

  /** Request a password-reset email. `POST /b/auth/api/forgot-password`. */
  async resetPassword(options: ResetPasswordOptions): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: "/b/auth/api/forgot-password",
      data: options,
    });
  }

  /** Confirm a password reset with the emailed token. */
  async confirmPasswordReset(token: string, newPassword: string): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: "/b/auth/api/reset-password",
      data: { token, new_password: newPassword },
    });
  }

  /** Change password for the authenticated user. */
  async updatePassword(options: UpdatePasswordOptions): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: "/b/auth/api/change-password",
      data: {
        current_password: options.currentPassword,
        new_password: options.newPassword,
      },
    });
  }

  /**
   * Refresh the access/refresh token pair. Uses the token cached from the
   * last `signIn`/`signUp` unless one is passed explicitly — the server
   * requires `refresh_token` in the body (it is not read from a cookie).
   */
  async refreshSession(refreshToken?: string): Promise<AuthTokens> {
    const token = refreshToken ?? this.tokens?.refresh_token;
    if (!token) {
      throw new Error("No refresh token available — sign in first or pass one explicitly");
    }
    const tokens = await this.request<AuthTokens>({
      method: "POST",
      url: "/b/auth/api/refresh",
      data: { refresh_token: token },
    });
    this.tokens = tokens;
    return tokens;
  }

  /** Verify email with the emailed token. `token` may also be a query param. */
  async verifyEmail(token: string): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: "/b/auth/api/verify",
      data: { token },
    });
  }

  /** Resend the verification email. */
  async resendVerification(email: string): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: "/b/auth/api/resend-verification",
      data: { email },
    });
  }

  /**
   * Start an OAuth flow. `GET /b/auth/oauth/login?provider=`, which returns
   * `{ auth_url, provider }` for the caller to navigate to (full-page
   * redirect or a popup — see `signInWithOAuthPopup`).
   */
  async signInWithOAuth(
    provider: OAuthProviderName,
  ): Promise<{ auth_url: string; provider: string }> {
    return this.request<{ auth_url: string; provider: string }>({
      method: "GET",
      url: `/b/auth/oauth/login?provider=${encodeURIComponent(provider)}`,
    });
  }

  /**
   * Sign in via an OAuth popup. Single consolidated implementation (see
   * `PopupAuthSession`) — the server sets an httpOnly cookie and redirects
   * the popup to `FRONTEND_URL`, with no `postMessage` contract of its own,
   * so the session is finalized by polling for the popup closing and then
   * verifying the cookie via `getUser()`. A `postMessage({type: "oauth-success"
   * | "oauth-error", error?})` from the popup (e.g. a consumer-built bridge
   * page at `FRONTEND_URL`) is honored as a faster opt-in path but is never
   * required.
   */
  async signInWithOAuthPopup(
    provider: OAuthProviderName,
    options?: OAuthPopupOptions,
  ): Promise<AuthSessionUser> {
    const { auth_url } = await this.signInWithOAuth(provider);

    const expectedOrigin = new URL(this.config.url).origin;
    const allowedOrigins = [expectedOrigin];
    if (typeof window !== "undefined" && window.location.origin !== expectedOrigin) {
      allowedOrigins.push(window.location.origin);
    }

    await PopupAuthSession.open<true>({
      url: auth_url,
      signal: options?.signal,
      allowedOrigins,
      onMessage: (data) => {
        const message = data as { type?: string; error?: string } | null | undefined;
        if (
          !message ||
          (message.type !== "oauth-success" &&
            message.type !== "oauth-error" &&
            message.type !== "oauth_callback")
        ) {
          return undefined;
        }
        if (message.error) {
          throw new Error(message.error);
        }
        return true;
      },
      // No bridge-page message ever arrives against the shipped server —
      // the popup closing (after the cookie-setting redirect completes) is
      // the expected, guaranteed-working completion signal.
      onClosed: () => true,
    });

    const user = await this.getUser();
    if (!user) {
      throw new Error("Authentication failed");
    }
    return user;
  }

  /** Get the current user from memory (without an API call). */
  getCurrentUser(): AuthSessionUser | null {
    return this.currentUser;
  }

  /** Check if a user is authenticated (based on the cached user). */
  isAuthenticated(): boolean {
    return this.currentUser !== null;
  }
}
