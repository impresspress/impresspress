import { BaseService } from "./base.service";
import { User } from "../types";

export interface SignUpOptions {
  email: string;
  password: string;
  metadata?: Record<string, any>;
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

export class AuthService extends BaseService {
  private currentUser: User | null = null;
  // Authentication is handled via httpOnly cookies
  // No client-side token storage

  /**
   * Sign up a new user
   */
  async signUp(options: SignUpOptions): Promise<User> {
    // Signup returns just the user, need to login after
    await this.request<User>({
      method: "POST",
      url: "/b/auth/api/signup",
      data: options,
    });

    // Now sign in (httpOnly cookie will be set by backend)
    const loginResponse = await this.request<{ user: User }>({
      method: "POST",
      url: "/b/auth/api/login",
      data: {
        email: options.email,
        password: options.password,
      },
    });

    this.currentUser = loginResponse.user;
    return loginResponse.user;
  }

  /**
   * Sign in an existing user
   */
  async signIn(options: SignInOptions): Promise<User> {
    const response = await this.request<{ user: User }>({
      method: "POST",
      url: "/b/auth/api/login",
      data: options,
    });

    this.currentUser = response.user;
    // Auth token is set as httpOnly cookie by backend
    return response.user;
  }

  /**
   * Sign out the current user
   */
  async signOut(): Promise<void> {
    try {
      await this.request<void>({
        method: "POST",
        url: "/b/auth/api/logout",
      });
    } finally {
      this.currentUser = null;
      // Cookie will be cleared by backend
    }
  }

  /**
   * Get the current authenticated user
   */
  async getUser(): Promise<User | null> {
    try {
      const user = await this.request<User>({
        method: "GET",
        url: "/b/auth/api/me",
      });
      this.currentUser = user;
      return user;
    } catch (error) {
      return null;
    }
  }

  /**
   * Update the current user's profile
   */
  async updateUser(updates: Partial<User>): Promise<User> {
    const user = await this.request<User>({
      method: "PATCH",
      url: "/b/auth/api/me",
      data: updates,
    });
    this.currentUser = user;
    return user;
  }

  /**
   * Request a password reset
   */
  async resetPassword(options: ResetPasswordOptions): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: "/b/auth/api/reset-password",
      data: options,
    });
  }

  /**
   * Confirm password reset with token
   */
  async confirmPasswordReset(
    token: string,
    newPassword: string,
  ): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: "/b/auth/api/confirm-reset",
      data: {
        token,
        password: newPassword,
      },
    });
  }

  /**
   * Update password for authenticated user
   */
  async updatePassword(options: UpdatePasswordOptions): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: "/b/auth/api/change-password",
      data: options,
    });
  }

  /**
   * Refresh the session (cookie-based)
   */
  async refreshSession(): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: "/b/auth/api/refresh",
    });
  }

  /**
   * Verify email with token
   */
  async verifyEmail(token: string): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: "/b/auth/api/verify-email",
      data: { token },
    });
  }

  /**
   * Resend verification email
   */
  async resendVerification(): Promise<void> {
    await this.request<void>({
      method: "POST",
      url: "/b/auth/api/resend-verification",
    });
  }

  /**
   * Get the current user from memory (without API call)
   */
  getCurrentUser(): User | null {
    return this.currentUser;
  }

  /**
   * Check if user is authenticated (based on cached user)
   */
  isAuthenticated(): boolean {
    return this.currentUser !== null;
  }

  /**
   * OAuth sign in (redirect)
   */
  async signInWithOAuth(
    provider: "google" | "github" | "facebook" | "microsoft",
  ): Promise<{ url: string }> {
    return this.request<{ url: string }>({
      method: "GET",
      url: `/b/auth/oauth/${provider}`,
    });
  }

  /**
   * Sign in with OAuth using popup window (for specific provider)
   * @internal Used internally by the Impresspress login page
   */
  async signInWithOAuthPopup(
    provider: "google" | "github" | "facebook" | "microsoft",
  ): Promise<User> {
    return new Promise(async (resolve, reject) => {
      try {
        // Get OAuth URL from backend
        const { url } = await this.signInWithOAuth(provider);

        // Calculate popup position (centered)
        const width = 500;
        const height = 600;
        const left = window.screenX + (window.innerWidth - width) / 2;
        const top = window.screenY + (window.innerHeight - height) / 2;

        // Open popup window
        const popup = window.open(
          url,
          `${provider}_auth_popup`,
          `width=${width},height=${height},left=${left},top=${top},toolbar=no,menubar=no,location=no,status=no`,
        );

        if (!popup) {
          throw new Error(
            "Failed to open authentication popup. Please check your popup blocker settings.",
          );
        }

        // Setup message listener for OAuth callback
        const handleMessage = (event: MessageEvent) => {
          // Validate message origin matches our backend URL
          const expectedOrigin = new URL(this.config.url).origin;

          // Allow both backend origin and same origin (for OAuth callback page)
          if (
            event.origin !== expectedOrigin &&
            event.origin !== window.location.origin
          ) {
            return;
          }

          // Check for OAuth callback data
          if (event.data?.type === "oauth_callback") {
            // Clean up
            window.removeEventListener("message", handleMessage);
            if (popup && !popup.closed) {
              popup.close();
            }

            if (event.data.error) {
              reject(new Error(event.data.error));
              return;
            }

            if (event.data.success) {
              // The auth cookie has been set by the backend
              // Fetch user info to verify authentication
              this.getUser()
                .then((user) => {
                  if (user) {
                    this.currentUser = user;
                    resolve(user);
                  } else {
                    reject(new Error("Failed to fetch user information"));
                  }
                })
                .catch(reject);
            } else {
              reject(new Error("Authentication failed"));
            }
          }
        };

        // Listen for messages from the popup
        window.addEventListener("message", handleMessage);

        // Check if popup was closed manually
        const checkClosed = setInterval(() => {
          if (popup.closed) {
            clearInterval(checkClosed);
            window.removeEventListener("message", handleMessage);
            reject(new Error("Authentication popup was closed"));
          }
        }, 500);

        // Timeout after 5 minutes
        setTimeout(
          () => {
            clearInterval(checkClosed);
            window.removeEventListener("message", handleMessage);
            if (popup && !popup.closed) {
              popup.close();
            }
            reject(new Error("Authentication timeout"));
          },
          5 * 60 * 1000,
        );
      } catch (error) {
        reject(error);
      }
    });
  }

  /**
   * Sign in using popup window that opens the Impresspress login page
   * The login page handles email/password and OAuth options
   */
  async signInWithPopup(): Promise<User> {
    return new Promise((resolve, reject) => {
      try {
        // Calculate popup position (centered)
        const width = 500;
        const height = 650;
        const left = window.screenX + (window.innerWidth - width) / 2;
        const top = window.screenY + (window.innerHeight - height) / 2;

        // Use authUrl if provided, otherwise fall back to url
        const baseAuthUrl = this.config.authUrl || this.config.url;
        // Open popup to the Impresspress login page with popup=true parameter
        const loginUrl = `${baseAuthUrl}/b/auth/login?popup=true`;
        const popup = window.open(
          loginUrl,
          "impresspress_auth_popup",
          `width=${width},height=${height},left=${left},top=${top},toolbar=no,menubar=no,location=no,status=no`,
        );

        if (!popup) {
          throw new Error(
            "Failed to open authentication popup. Please check your popup blocker settings.",
          );
        }

        // Setup message listener for auth callback
        const handleMessage = (event: MessageEvent) => {
          // Validate message origin matches our auth URL or backend URL
          const expectedAuthOrigin = new URL(baseAuthUrl).origin;
          const expectedApiOrigin = new URL(this.config.url).origin;

          if (
            event.origin !== expectedAuthOrigin &&
            event.origin !== expectedApiOrigin
          ) {
            return;
          }

          // Check for auth callback data
          if (
            event.data?.type === "auth-success" ||
            event.data?.type === "oauth-success"
          ) {
            // Clean up
            window.removeEventListener("message", handleMessage);
            if (popup && !popup.closed) {
              popup.close();
            }

            // The auth cookie has been set by the backend
            // Fetch user info to verify authentication
            this.getUser()
              .then((user) => {
                if (user) {
                  this.currentUser = user;
                  resolve(user);
                } else {
                  reject(new Error("Failed to fetch user information"));
                }
              })
              .catch(reject);
          } else if (
            event.data?.type === "auth-error" ||
            event.data?.type === "oauth-error"
          ) {
            window.removeEventListener("message", handleMessage);
            if (popup && !popup.closed) {
              popup.close();
            }
            reject(new Error(event.data.error || "Authentication failed"));
          }
        };

        // Listen for messages from the popup
        window.addEventListener("message", handleMessage);

        // Track if we've already resolved/rejected
        let settled = false;

        // Check if popup was closed
        const checkClosed = setInterval(() => {
          if (popup.closed && !settled) {
            clearInterval(checkClosed);
            window.removeEventListener("message", handleMessage);

            // Popup closed - try to fetch user in case auth succeeded
            // (cross-origin postMessage may not work, but cookies are set)
            this.getUser()
              .then((user) => {
                if (user && !settled) {
                  settled = true;
                  this.currentUser = user;
                  resolve(user);
                } else if (!settled) {
                  settled = true;
                  reject(new Error("Authentication popup was closed"));
                }
              })
              .catch(() => {
                if (!settled) {
                  settled = true;
                  reject(new Error("Authentication popup was closed"));
                }
              });
          }
        }, 500);

        // Timeout after 5 minutes
        setTimeout(
          () => {
            if (!settled) {
              settled = true;
              clearInterval(checkClosed);
              window.removeEventListener("message", handleMessage);
              if (popup && !popup.closed) {
                popup.close();
              }
              reject(new Error("Authentication timeout"));
            }
          },
          5 * 60 * 1000,
        );
      } catch (error) {
        reject(error);
      }
    });
  }

  /**
   * Handle OAuth callback
   */
  async handleOAuthCallback(provider: string, code: string): Promise<User> {
    const response = await this.request<{ user: User }>({
      method: "POST",
      url: `/b/auth/oauth/${provider}/callback`,
      data: { code },
    });

    this.currentUser = response.user;
    return response.user;
  }

  /**
   * Delete account
   */
  async deleteAccount(): Promise<void> {
    await this.request<void>({
      method: "DELETE",
      url: "/b/auth/api/account",
    });

    this.currentUser = null;
  }
}
