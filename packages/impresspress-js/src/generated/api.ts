/**
 * GENERATED FILE - do not edit.
 *
 * Produced by `npm run generate:types` from the committed per-block OpenAPI
 * snapshots in `crates/impresspress-core/tests/snapshots`, which are
 * themselves generated from each block's `EndpointRoute` table. Edit the
 * Rust contract, regenerate the snapshot, then regenerate this file.
 *
 * An endpoint appears here only if it declares a schema. The Rust test
 * `endpoints_the_sdk_calls_publish_a_response_schema` is what keeps that set
 * from silently shrinking to "whatever already had one".
 */
export interface paths {
    "/b/admin/api/extensions": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List registered blocks API */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Whether the block is enabled.
                             *
                             *     Read from the boot block-settings snapshot — the same source
                             *     `routing::route_to_block`'s feature gate consults — so `false` means
                             *     the router answers "endpoint not found" for every one of this block's
                             *     routes. A block with no stored row reports `true`.
                             */
                            enabled: boolean;
                            /** @description Interface identifier, e.g. `"http-handler@v1"`. */
                            interface: string;
                            /** @description Block name in the canonical `{org}/{block}` form. */
                            name: string;
                            /** @description One-line summary of what the block does. */
                            summary: string;
                            /** @description Semantic version of the block implementation. */
                            version: string;
                        }[];
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/admin/api/iam/roles": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List roles API */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page. Always 1 — the handler does not paginate.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page. Always the handler's fixed 1000-row ceiling.
                             */
                            page_size: number;
                            /** @description Roles, sorted by name ascending. */
                            records: {
                                /** @description RFC 3339 creation timestamp. */
                                created_at: string;
                                /** @description Human-readable description shown in the IAM UI. */
                                description: string;
                                /** @description Stable role identifier. */
                                id: string;
                                /**
                                 * @description Whether this is a built-in role. System roles cannot be renamed or
                                 *     deleted.
                                 */
                                is_system: boolean;
                                /**
                                 * @description Unique role name (`"admin"`, `"user"`, …). This is the value stored in
                                 *     `user_roles.role` and checked by the auth layer.
                                 */
                                name: string;
                                /**
                                 * @description Permission names attached to the role. Advisory metadata for the IAM
                                 *     UI — WRAP grants, not this list, are what the runtime enforces.
                                 */
                                permissions: string[];
                                /** @description RFC 3339 timestamp of the last modification. */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total roles defined.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create role API */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @description Human-readable description shown in the IAM UI. Empty when omitted. */
                        description?: string | null;
                        /**
                         * @description Unique role name. This is the value stored in `user_roles.role` and
                         *     checked by the auth layer.
                         */
                        name: string;
                        /**
                         * @description Permission names to attach. Advisory metadata for the IAM UI — WRAP
                         *     grants, not this list, are what the runtime enforces. None when
                         *     omitted.
                         */
                        permissions?: string[] | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description RFC 3339 creation timestamp. */
                            created_at: string;
                            /** @description Human-readable description shown in the IAM UI. */
                            description: string;
                            /** @description Stable role identifier. */
                            id: string;
                            /**
                             * @description Whether this is a built-in role. System roles cannot be renamed or
                             *     deleted.
                             */
                            is_system: boolean;
                            /**
                             * @description Unique role name (`"admin"`, `"user"`, …). This is the value stored in
                             *     `user_roles.role` and checked by the auth layer.
                             */
                            name: string;
                            /**
                             * @description Permission names attached to the role. Advisory metadata for the IAM
                             *     UI — WRAP grants, not this list, are what the runtime enforces.
                             */
                            permissions: string[];
                            /** @description RFC 3339 timestamp of the last modification. */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/admin/api/iam/roles/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Delete role API */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Always `true`: a delete that did not happen is an error response. */
                            deleted: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        /** Update role API */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        description?: string | null;
                        name?: string | null;
                        permissions?: string[] | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description RFC 3339 creation timestamp. */
                            created_at: string;
                            /** @description Human-readable description shown in the IAM UI. */
                            description: string;
                            /** @description Stable role identifier. */
                            id: string;
                            /**
                             * @description Whether this is a built-in role. System roles cannot be renamed or
                             *     deleted.
                             */
                            is_system: boolean;
                            /**
                             * @description Unique role name (`"admin"`, `"user"`, …). This is the value stored in
                             *     `user_roles.role` and checked by the auth layer.
                             */
                            name: string;
                            /**
                             * @description Permission names attached to the role. Advisory metadata for the IAM
                             *     UI — WRAP grants, not this list, are what the runtime enforces.
                             */
                            permissions: string[];
                            /** @description RFC 3339 timestamp of the last modification. */
                            updated_at: string;
                            /**
                             * @description Present when the update was saved but a rename's grants did not all
                             *     follow it: says which still name the old role, and how to move them.
                             */
                            warning?: string | null;
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/admin/api/logs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Audit logs API */
        get: {
            parameters: {
                query?: {
                    action?: string | null;
                    page?: number;
                    page_size?: number;
                    resource?: string | null;
                    user_id?: string | null;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /** @description Audit entries on this page, newest first. */
                            records: {
                                /** @description Action name (`"user.delete"`, `"role.create"`, …). */
                                action: string;
                                /** @description RFC 3339 timestamp the action was recorded at. */
                                created_at: string;
                                /** @description Stable entry identifier. */
                                id: string;
                                /**
                                 * @description Client IP the action came from, as seen by the request pipeline. Empty
                                 *     when the pipeline could not determine one.
                                 */
                                ip_address: string;
                                /** @description Target the action was applied to. */
                                resource: string;
                                /**
                                 * @description RFC 3339 write timestamp. Audit rows are never updated, so this always
                                 *     equals `created_at`.
                                 */
                                updated_at: string;
                                /**
                                 * @description Id of the admin who performed the action. Empty when the action had no
                                 *     authenticated actor.
                                 */
                                user_id: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total entries matching the filters, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/admin/api/settings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List variables API */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            settings: {
                                /** @description Variable name, e.g. `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL`. */
                                key: string;
                                /**
                                 * @description Whether `value` is masked. True when the row carries the sensitive
                                 *     flag, or the key is one this build knows to hold a secret: it ends in
                                 *     `_SECRET` or `_KEY`, or its declaration is a password-typed or
                                 *     auto-generated variable.
                                 */
                                sensitive: boolean;
                                /**
                                 * @description The stored value, or `"********"` when `sensitive` is true.
                                 *
                                 *     Typed as `any` rather than `string` because the stored column is text
                                 *     that the SQLite and D1 backends decode back into JSON when it looks
                                 *     like an object or an array: a variable holding `["a","b"]` reads back
                                 *     as an array, one holding `on` reads back as a string.
                                 */
                                value: unknown;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/admin/api/users": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List users API */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                    search?: string | null;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /** @description Users on this page, newest first. */
                            records: {
                                /** @description Avatar image URL, when set. */
                                avatar_url: string | null;
                                /** @description RFC 3339 creation timestamp. */
                                created_at: string;
                                /**
                                 * @description RFC 3339 soft-delete timestamp. Always `null` in the list response,
                                 *     which filters on `deleted_at IS NULL`.
                                 */
                                deleted_at: string | null;
                                /** @description Whether the account is disabled (blocked from signing in). */
                                disabled: boolean;
                                /** @description Display name shown in the UI. */
                                display_name: string;
                                /** @description Login email address. */
                                email: string;
                                /** @description Whether the email address has been verified. */
                                email_verified: boolean;
                                /** @description Stable user identifier. */
                                id: string;
                                /** @description RFC 3339 timestamp of the last successful sign-in, if any. */
                                last_login_at: string | null;
                                /** @description Full name, when the user supplied one. */
                                name: string | null;
                                /**
                                 * @description Legacy single-role column on the user row (`"user"` by default).
                                 *     Authorization uses `roles`; this field is retained because the column is
                                 *     still written by the signup path.
                                 */
                                role: string;
                                /** @description Role names assigned to this user in `impresspress__admin__user_roles`. */
                                roles: string[];
                                /** @description RFC 3339 timestamp of the last modification. */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total users matching the filter, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/auth/api/api-keys": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Create API key */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * Format: date-time
                         * @description Absolute expiry, RFC 3339 and in the future. Omit for a key that does
                         *     not expire.
                         */
                        expires_at?: string | null;
                        /** @description Label shown in the key list. Required and non-empty. */
                        name: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/auth/api/change-password": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Change password */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            message: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/auth/api/forgot-password": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Request a password reset email */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            message: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/auth/api/login": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Authenticate with email/password */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** Format: email */
                        email: string;
                        password: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            access_token: string;
                            /** @description Role-aware post-login redirect path */
                            default_redirect: string;
                            /**
                             * Format: uint64
                             * @description Access token lifetime in seconds
                             */
                            expires_in: number;
                            refresh_token: string;
                            /**
                             * @description The only `token_type` this API issues.
                             * @enum {string}
                             */
                            token_type: "Bearer";
                            /** @description The caller's identity, as returned by a successful authentication. */
                            user: {
                                email: string;
                                id: string;
                                name: string;
                                roles: string[];
                            };
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/auth/api/logout": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Sign out */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            message: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/auth/api/me": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get current user */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description The caller's own profile. */
                            user: {
                                avatar_url: string;
                                /** Format: date-time */
                                created_at: string;
                                email: string;
                                id: string;
                                name: string;
                                roles: string[];
                            };
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /** Update current user profile */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        avatar_url?: string | null;
                        name?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description The caller's own profile. */
                            user: {
                                avatar_url: string;
                                /** Format: date-time */
                                created_at: string;
                                email: string;
                                id: string;
                                name: string;
                                roles: string[];
                            };
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/auth/api/refresh": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Rotate an access/refresh token pair */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        refresh_token: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            access_token: string;
                            /**
                             * Format: uint64
                             * @description Access token lifetime in seconds
                             */
                            expires_in: number;
                            refresh_token: string;
                            /**
                             * @description The only `token_type` this API issues.
                             * @enum {string}
                             */
                            token_type: "Bearer";
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/auth/api/resend-verification": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Re-send the verification email */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            message: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/auth/api/reset-password": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Reset password with a reset token */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            message: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/auth/api/signup": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Create account */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** Format: email */
                        email: string;
                        /** @description Optional display name */
                        name?: string | null;
                        password: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            access_token: string;
                            /** @description Role-aware post-login redirect path */
                            default_redirect: string;
                            /** @constant */
                            email_verified: true;
                            /**
                             * Format: uint64
                             * @description Access token lifetime in seconds
                             */
                            expires_in: number;
                            refresh_token: string;
                            /**
                             * @description The only `token_type` this API issues.
                             * @enum {string}
                             */
                            token_type: "Bearer";
                            /** @description The caller's identity, as returned by a successful authentication. */
                            user: {
                                email: string;
                                id: string;
                                name: string;
                                roles: string[];
                            };
                        } | {
                            /** @constant */
                            email_verified: false;
                            message: string;
                            /** @description The address a signup awaiting verification was made for. */
                            user: {
                                email: string;
                            };
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/auth/oauth/login": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Start OAuth flow */
        get: {
            parameters: {
                query: {
                    provider: "google" | "github" | "microsoft";
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/cloudstorage/admin/access-logs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Share access logs (admin) */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** Format: int64 */
                            page: number;
                            /** Format: int64 */
                            page_size: number;
                            records: {
                                /**
                                 * @description One access-log row, decoded. The child audit table of a share: a log row
                                 *     is meaningless without the share it points at, which is why both tables
                                 *     live behind this one module.
                                 */
                                data: {
                                    /** @description RFC 3339 instant of the recorded access. */
                                    accessed_at: string;
                                    created_at: string;
                                    id: string;
                                    ip_address: string;
                                    share_id: string;
                                    updated_at: string;
                                    user_agent: string;
                                };
                                id: string;
                            }[];
                            /** Format: int64 */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/cloudstorage/admin/quotas": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Per-user quotas (admin) */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** Format: int64 */
                            page: number;
                            /** Format: int64 */
                            page_size: number;
                            records: {
                                /** @description One quota-override row, decoded. */
                                data: {
                                    created_at: string;
                                    id: string;
                                    /** Format: int64 */
                                    max_file_size_bytes: number;
                                    /**
                                     * Format: int64
                                     * @description Most objects one user may hold in any one bucket, in-flight uploads included.
                                     */
                                    max_files_per_bucket: number;
                                    /** Format: int64 */
                                    max_storage_bytes: number;
                                    updated_at: string;
                                    /** @description The user this override applies to. Unique across the table. */
                                    user_id: string;
                                };
                                id: string;
                            }[];
                            /** Format: int64 */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/cloudstorage/admin/quotas/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /** Set a user's quota (admin) */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description One quota-override row, decoded. */
                            data: {
                                created_at: string;
                                id: string;
                                /** Format: int64 */
                                max_file_size_bytes: number;
                                /**
                                 * Format: int64
                                 * @description Most objects one user may hold in any one bucket, in-flight uploads included.
                                 */
                                max_files_per_bucket: number;
                                /** Format: int64 */
                                max_storage_bytes: number;
                                updated_at: string;
                                /** @description The user this override applies to. Unique across the table. */
                                user_id: string;
                            };
                            id: string;
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/cloudstorage/admin/shares": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Recent shares, all users (admin) */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** Format: int64 */
                            page: number;
                            /** Format: int64 */
                            page_size: number;
                            records: {
                                /** @description One share row, decoded. */
                                data: {
                                    /** Format: int64 */
                                    access_count: number;
                                    bucket: string;
                                    /** @description RFC 3339 creation instant. */
                                    created_at: string;
                                    /**
                                     * @description User id of the share's creator — the ownership key
                                     *     `handle_delete_share` checks.
                                     */
                                    created_by: string;
                                    /**
                                     * @description The end this share link records, as an RFC 3339 stamp.
                                     *
                                     *     `None` is a SQL `NULL` or a stored empty string — one meaning, since
                                     *     the column is nullable and every caller treated `""` as unset. It is
                                     *     NOT "never expires": every share link has an end, and a row that
                                     *     records none cannot be shown to be live, so the public link refuses
                                     *     it. Creating a share cannot produce one — `NewShare` takes a
                                     *     non-optional expiry — and migration 003 gave every historical row an
                                     *     end, so a `None` here is a row that reached the table some other
                                     *     way.
                                     */
                                    expires_at: string | null;
                                    id: string;
                                    key: string;
                                    /**
                                     * Format: int64
                                     * @description Access cap, or `None` for unlimited. A non-positive stored value is
                                     *     `None` too, which is the meaning [`NewShare::max_access_count`]
                                     *     documents and the meaning
                                     *     [`increment_access_count_capped`] enforces.
                                     */
                                    max_access_count: number | null;
                                    /**
                                     * @description The opaque token embedded in the public `/b/storage/direct/{token}`
                                     *     URL: random bytes, hex-encoded, asserting nothing about the share.
                                     *     This row is what it addresses, and what decides whether the link
                                     *     still works. Unique across the table.
                                     */
                                    token: string;
                                    updated_at: string;
                                };
                                id: string;
                            }[];
                            /** Format: int64 */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/cloudstorage/quota": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** My quota and usage */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description The caller's effective caps: their override row if they have one,
                             *     otherwise the block defaults.
                             */
                            quota: {
                                /** Format: int64 */
                                max_file_size_bytes: number;
                                /**
                                 * Format: int64
                                 * @description Most objects one user may hold in any one bucket, in-flight uploads included.
                                 */
                                max_files_per_bucket: number;
                                /** Format: int64 */
                                max_storage_bytes: number;
                            };
                            /**
                             * @description The `usage` half of [`QuotaResponse`]. Both numbers are computed over the
                             *     caller's object rows, not read from a counter column.
                             */
                            usage: {
                                /**
                                 * Format: int64
                                 * @description Objects the caller owns across all buckets, `Pending` included; not
                                 *     what the per-bucket `max_files_per_bucket` cap is checked against.
                                 */
                                file_count: number;
                                /**
                                 * Format: int64
                                 * @description `SUM(size)` over the caller's rows, `Pending` reservations included.
                                 */
                                total_bytes: number;
                            };
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/cloudstorage/shares": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List my share links */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** Format: int64 */
                            page: number;
                            /** Format: int64 */
                            page_size: number;
                            records: {
                                /** @description One share row, decoded. */
                                data: {
                                    /** Format: int64 */
                                    access_count: number;
                                    bucket: string;
                                    /** @description RFC 3339 creation instant. */
                                    created_at: string;
                                    /**
                                     * @description User id of the share's creator — the ownership key
                                     *     `handle_delete_share` checks.
                                     */
                                    created_by: string;
                                    /**
                                     * @description The end this share link records, as an RFC 3339 stamp.
                                     *
                                     *     `None` is a SQL `NULL` or a stored empty string — one meaning, since
                                     *     the column is nullable and every caller treated `""` as unset. It is
                                     *     NOT "never expires": every share link has an end, and a row that
                                     *     records none cannot be shown to be live, so the public link refuses
                                     *     it. Creating a share cannot produce one — `NewShare` takes a
                                     *     non-optional expiry — and migration 003 gave every historical row an
                                     *     end, so a `None` here is a row that reached the table some other
                                     *     way.
                                     */
                                    expires_at: string | null;
                                    id: string;
                                    key: string;
                                    /**
                                     * Format: int64
                                     * @description Access cap, or `None` for unlimited. A non-positive stored value is
                                     *     `None` too, which is the meaning [`NewShare::max_access_count`]
                                     *     documents and the meaning
                                     *     [`increment_access_count_capped`] enforces.
                                     */
                                    max_access_count: number | null;
                                    /**
                                     * @description The opaque token embedded in the public `/b/storage/direct/{token}`
                                     *     URL: random bytes, hex-encoded, asserting nothing about the share.
                                     *     This row is what it addresses, and what decides whether the link
                                     *     still works. Unique across the table.
                                     */
                                    token: string;
                                    updated_at: string;
                                };
                                id: string;
                            }[];
                            /** Format: int64 */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create a share link */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Path of the public share link, relative to the deployment's origin. */
                            direct_url: string;
                            /**
                             * @description Row id of the new share — the `{id}` of `DELETE
                             *     /b/cloudstorage/shares/{id}`.
                             */
                            id: string;
                            /**
                             * @description The opaque token embedded in `direct_url`. It carries no expiry of
                             *     its own: the share's own `expires_at` and access cap are what end a
                             *     link.
                             */
                            token: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/cloudstorage/shares/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Delete a share link */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Always `true` — a delete that did not happen is an error status, not
                             *     `{"deleted": false}`.
                             */
                            deleted: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/blocks": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Scaffold a new block from a template
         * @description Writes blocks/<name>/{Cargo.toml, src/lib.rs, src/wafer_guest.rs}. The support module is written verbatim — it is the guest ABI and must not be hand-written or edited. Writing source activates nothing; compile the block to make it serve.
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * @description The block's short name, e.g. `newsletter`. It becomes the directory
                         *     `blocks/<name>/`, the crate name, the block id `site/<name>`, the
                         *     route prefix `/b/<name>/` and the collection prefix `site__<name>__`
                         *     (a hyphen spelled `_` there). 2 to 32 characters: a lowercase letter
                         *     followed by lowercase letters, digits and hyphens, with no doubled
                         *     hyphen and no trailing hyphen.
                         */
                        name: string;
                        /**
                         * @description Which starting point to write. `hello` is one public `GET` and
                         *     nothing else; `table` is a newsletter block with a database table, an
                         *     agent tool and two admin reads — start there for anything that stores
                         *     data.
                         */
                        template: "hello" | "table";
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description The files that were written, in path order. Read or edit them through
                             *     the files API; the block does not serve until it is compiled.
                             */
                            files: {
                                /** @description Content type the file is served with. */
                                content_type: string;
                                /**
                                 * @description Where the file lives. Workspace-relative (`site/index.html`) in the
                                 *     files API; relative to its area's root (`index.html`) in a
                                 *     generation's site manifest and in a block's source listing.
                                 */
                                path: string;
                                /** @description SHA-256 of the file's content-addressed blob, hex-encoded. */
                                sha256: string;
                                /**
                                 * Format: uint64
                                 * @description Size in bytes.
                                 */
                                size: number;
                            }[];
                            /** @description The block's short name, as written. */
                            name: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/blocks/{name}/remove": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Remove a block from the runtime */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    name: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description The generation that went live. */
                            generation: {
                                /** @description RFC 3339 time the generation went live, or null if it never did. */
                                activated_at: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of blocks in the generation's block manifest.
                                 */
                                blocks: number;
                                /** @description What created this generation. */
                                cause: "site_write" | "site_delete" | "block_compile" | "block_remove" | "rollback" | "seed";
                                /** @description RFC 3339 creation time. */
                                created_at: string;
                                /** @description Generation id. */
                                id: string;
                                /** @description The generation this one was derived from, or null for the first. */
                                parent_id: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of files in the generation's site manifest.
                                 */
                                site_files: number;
                                /** @description Where the generation sits in its lifecycle. */
                                status: "staged" | "validating" | "activating" | "active" | "failed" | "superseded";
                            };
                            /**
                             * @description One entry per phase the activation passed through, with how long it
                             *     took. The last is always `active`.
                             */
                            progress: {
                                /** @description Human-readable detail for the progress panel. */
                                detail: string;
                                /**
                                 * Format: uint64
                                 * @description Milliseconds spent in it.
                                 */
                                ms: number;
                                /** @description The phase this step covers. */
                                phase: "idle" | "validating" | "building_runtime" | "publishing" | "active" | "failed";
                            }[];
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/builds/stage": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Stage and activate a compiled block */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * @description The compiled module, standard base64 with padding. At most 4 MiB
                         *     decoded.
                         */
                        artifact_base64: string;
                        /**
                         * @description The block's short name, e.g. `hello` for the sources under
                         *     `blocks/hello/`. It is registered as `site/hello` and serves
                         *     `/b/hello/`; do not send either of those longer forms.
                         */
                        block_name: string;
                        /** @description Pinned toolchain revision that produced the artifact. */
                        compiler_version: string;
                        /**
                         * @description Diagnostics the compiler produced, warnings included. They are stored
                         *     with the build and returned alongside any the validator adds.
                         * @default []
                         */
                        diagnostics?: {
                            /**
                             * @description Stable machine-readable identifier, e.g. `cap-collection` for a
                             *     capability outside the block's namespace or `guest-init` for a trap
                             *     in the guest's `Init`. Match on this rather than on `message`.
                             *
                             *     `null` when whoever produced the diagnostic had no code for it. Every
                             *     diagnostic this crate produces has one — they are the constants above
                             *     — but a *compiler* diagnostic forwarded by `/b/dev` need not: rustc
                             *     numbers some of what it says (`E0425`) and not the rest, and the page
                             *     forwards what the compiler gave it. This field is optional for the
                             *     same reason `file`/`line`/`column` are: "when the compiler reported
                             *     one". Inventing a placeholder on the way in would put a value in the
                             *     build's stored record that nothing ever said.
                             * @default null
                             */
                            code?: string | null;
                            /**
                             * Format: uint32
                             * @description 1-based column in `file`, when the compiler reported one.
                             */
                            column?: number | null;
                            /**
                             * @description Workspace-relative source file the diagnostic is about, when the
                             *     compiler reported one.
                             */
                            file?: string | null;
                            /**
                             * Format: uint32
                             * @description 1-based line in `file`, when the compiler reported one.
                             */
                            line?: number | null;
                            /** @description What is wrong, and what to change. */
                            message: string;
                            /** @description How serious it is. Anything `error` means the build was refused. */
                            severity: "error" | "warning" | "note" | "help";
                        }[];
                        /**
                         * @description SHA-256 of the source manifest the compile ran against, so a stored
                         *     build can be traced back to the exact sources. Omit if the compiler
                         *     did not report one.
                         * @default null
                         */
                        source_manifest_sha256?: string | null;
                        /**
                         * Format: uint32
                         * @description The `WAFER_GUEST_VERSION` of the `src/wafer_guest.rs` the artifact was
                         *     compiled against, read out of that file by whoever ran the compile.
                         *
                         *     A value that is not the sandbox's own is refused with a
                         *     `wafer-guest-version` diagnostic: the vendored module IS the ABI, so a
                         *     block built against an older copy is talking a contract this runtime
                         *     no longer speaks. Replace the block's `src/wafer_guest.rs` with the
                         *     current module (`GET /b/dev/api/reference`, `wafer_guest_module`) and
                         *     compile again.
                         *
                         *     Omit it only if the compiler genuinely could not read the file. It is
                         *     then recorded as `0` — "unknown" — and nothing is checked.
                         * @default null
                         */
                        wafer_guest_version?: number | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description The stored build's id, or null when the request was refused before a
                             *     build could be recorded (an artifact over the size limit is never
                             *     stored, so there is nothing for a build row to point at).
                             */
                            build_id: string | null;
                            /**
                             * @description Everything known about this build: the diagnostics the compiler
                             *     reported, then any the validator added. `severity` tells them apart —
                             *     a refusal is always an `error`.
                             */
                            diagnostics: {
                                /**
                                 * @description Stable machine-readable identifier, e.g. `cap-collection` for a
                                 *     capability outside the block's namespace or `guest-init` for a trap
                                 *     in the guest's `Init`. Match on this rather than on `message`.
                                 *
                                 *     `null` when whoever produced the diagnostic had no code for it. Every
                                 *     diagnostic this crate produces has one — they are the constants above
                                 *     — but a *compiler* diagnostic forwarded by `/b/dev` need not: rustc
                                 *     numbers some of what it says (`E0425`) and not the rest, and the page
                                 *     forwards what the compiler gave it. This field is optional for the
                                 *     same reason `file`/`line`/`column` are: "when the compiler reported
                                 *     one". Inventing a placeholder on the way in would put a value in the
                                 *     build's stored record that nothing ever said.
                                 * @default null
                                 */
                                code: string | null;
                                /**
                                 * Format: uint32
                                 * @description 1-based column in `file`, when the compiler reported one.
                                 */
                                column: number | null;
                                /**
                                 * @description Workspace-relative source file the diagnostic is about, when the
                                 *     compiler reported one.
                                 */
                                file: string | null;
                                /**
                                 * Format: uint32
                                 * @description 1-based line in `file`, when the compiler reported one.
                                 */
                                line: number | null;
                                /** @description What is wrong, and what to change. */
                                message: string;
                                /** @description How serious it is. Anything `error` means the build was refused. */
                                severity: "error" | "warning" | "note" | "help";
                            }[];
                            /**
                             * @description The generation the accepted block went live in, or null when the
                             *     build was refused.
                             */
                            generation: {
                                /** @description RFC 3339 time the generation went live, or null if it never did. */
                                activated_at: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of blocks in the generation's block manifest.
                                 */
                                blocks: number;
                                /** @description What created this generation. */
                                cause: "site_write" | "site_delete" | "block_compile" | "block_remove" | "rollback" | "seed";
                                /** @description RFC 3339 creation time. */
                                created_at: string;
                                /** @description Generation id. */
                                id: string;
                                /** @description The generation this one was derived from, or null for the first. */
                                parent_id: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of files in the generation's site manifest.
                                 */
                                site_files: number;
                                /** @description Where the generation sits in its lifecycle. */
                                status: "staged" | "validating" | "activating" | "active" | "failed" | "superseded";
                            } | null;
                            /**
                             * @description One entry per phase the activation passed through, with how long it
                             *     took. Empty when nothing was activated.
                             */
                            progress: {
                                /** @description Human-readable detail for the progress panel. */
                                detail: string;
                                /**
                                 * Format: uint64
                                 * @description Milliseconds spent in it.
                                 */
                                ms: number;
                                /** @description The phase this step covers. */
                                phase: "idle" | "validating" | "building_runtime" | "publishing" | "active" | "failed";
                            }[];
                            /** @description Whether the block was accepted and activated. */
                            success: boolean;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/export/manifest": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Preview the export bundle
         * @description What `GET /b/dev/api/export` would produce, without producing it: every entry of the zip with its size, the totals, and the rows of each data table the snapshot carries. Read it to see what an export would contain before downloading one.
         */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: uint32
                             * @description How many compiled blocks the export carries. Each contributes its
                             *     `.wasm` plus its whole source tree.
                             */
                            blocks: number;
                            /** @description Every entry of the archive, in the order it is written. */
                            files: {
                                /**
                                 * Format: uint64
                                 * @description The entry's uncompressed size in bytes. The archive stores entries
                                 *     uncompressed, so this is also what it costs in the zip.
                                 */
                                bytes: number;
                                /** @description Path inside the archive (`sw.js`, `seed/site/index.html`). */
                                path: string;
                            }[];
                            /**
                             * @description The generation the export is a snapshot of — the one that is live now.
                             *     The downloaded file is named after its first eight characters.
                             */
                            generation_id: string;
                            /**
                             * Format: uint32
                             * @description How many of `files` are the runtime shell (the service worker, the
                             *     wasm, the loader — everything that makes the folder runnable).
                             */
                            shell_files: number;
                            /**
                             * Format: uint32
                             * @description How many are the site's own files, under `seed/site/`.
                             */
                            site_files: number;
                            /**
                             * @description Rows the data snapshot carries, per table — products, offers,
                             *     settings and accounts (`seed/data.json`).
                             */
                            tables: {
                                [key: string]: number;
                            };
                            /**
                             * Format: uint64
                             * @description Total size of every entry's content. The archive itself is slightly
                             *     larger: each entry carries a local header and a central directory
                             *     record naming it.
                             */
                            total_bytes: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/files": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List workspace files */
        get: {
            parameters: {
                query?: {
                    prefix?: string | null;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Matching files, in path order. */
                            files: {
                                /** @description Content type the file is served with. */
                                content_type: string;
                                /**
                                 * @description Where the file lives. Workspace-relative (`site/index.html`) in the
                                 *     files API; relative to its area's root (`index.html`) in a
                                 *     generation's site manifest and in a block's source listing.
                                 */
                                path: string;
                                /** @description SHA-256 of the file's content-addressed blob, hex-encoded. */
                                sha256: string;
                                /**
                                 * Format: uint64
                                 * @description Size in bytes.
                                 */
                                size: number;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/files/delete": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Delete a workspace file */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * @description The SHA-256 you expect the file to have right now. A mismatch — a
                         *     file that changed, or is already gone — is a `409`.
                         */
                        expected_sha256: string;
                        /** @description Workspace-relative path to remove. */
                        path: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description The generation this delete published, when it published one. A delete
                             *     under `site/` publishes; a delete under `blocks/` does not.
                             */
                            generation: {
                                /** @description RFC 3339 time the generation went live, or null if it never did. */
                                activated_at: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of blocks in the generation's block manifest.
                                 */
                                blocks: number;
                                /** @description What created this generation. */
                                cause: "site_write" | "site_delete" | "block_compile" | "block_remove" | "rollback" | "seed";
                                /** @description RFC 3339 creation time. */
                                created_at: string;
                                /** @description Generation id. */
                                id: string;
                                /** @description The generation this one was derived from, or null for the first. */
                                parent_id: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of files in the generation's site manifest.
                                 */
                                site_files: number;
                                /** @description Where the generation sits in its lifecycle. */
                                status: "staged" | "validating" | "activating" | "active" | "failed" | "superseded";
                            } | null;
                            /** @description Workspace-relative path that was removed. */
                            path: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/files/read": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Read a workspace file */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @description Workspace-relative path, e.g. `site/index.html`. */
                        path: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description The file's content, in `encoding`. */
                            content: string;
                            /**
                             * @description How `content` is encoded. Text files come back as `utf8`; anything
                             *     else, including text that is not valid UTF-8, comes back as `base64`.
                             */
                            encoding: "utf8" | "base64";
                            /** @description Workspace-relative path that was read. */
                            path: string;
                            /**
                             * @description SHA-256 of the content, hex-encoded. Pass it back as
                             *     `expected_sha256` to write over what you just read.
                             */
                            sha256: string;
                            /**
                             * Format: uint64
                             * @description Size in bytes of the decoded content.
                             */
                            size: number;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/files/write": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Write a workspace file */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @description The file's content, in `encoding`. */
                        content: string;
                        /**
                         * @description How `content` is encoded. Defaults to `utf8`.
                         * @default utf8
                         */
                        encoding?: "utf8" | "base64";
                        /**
                         * @description The SHA-256 you expect the file to have right now, or `null` if you
                         *     expect it not to exist yet. A mismatch is a `409` carrying the hash
                         *     the file actually has, so a caller that has fallen behind re-reads
                         *     instead of silently overwriting an edit it never saw.
                         *
                         *     Omitting the field means the same as `null` — serde defaults an
                         *     absent `Option` to `None`, and `#[serde(default)]` says so in the
                         *     source rather than leaving it to a rule the schema does not show. That
                         *     is a safe default rather than a lax one: over a file that exists,
                         *     "I expect nothing here" is itself a conflict.
                         * @default null
                         */
                        expected_sha256?: string | null;
                        /** @description Workspace-relative path under `site/` or `blocks/<name>/`. */
                        path: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description The generation this write published, when it published one. A write
                             *     under `site/` publishes; a write under `blocks/` does not — only a
                             *     compile turns block source into a published block.
                             */
                            generation: {
                                /** @description RFC 3339 time the generation went live, or null if it never did. */
                                activated_at: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of blocks in the generation's block manifest.
                                 */
                                blocks: number;
                                /** @description What created this generation. */
                                cause: "site_write" | "site_delete" | "block_compile" | "block_remove" | "rollback" | "seed";
                                /** @description RFC 3339 creation time. */
                                created_at: string;
                                /** @description Generation id. */
                                id: string;
                                /** @description The generation this one was derived from, or null for the first. */
                                parent_id: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of files in the generation's site manifest.
                                 */
                                site_files: number;
                                /** @description Where the generation sits in its lifecycle. */
                                status: "staged" | "validating" | "activating" | "active" | "failed" | "superseded";
                            } | null;
                            /** @description Workspace-relative path that was written. */
                            path: string;
                            /**
                             * @description SHA-256 of the stored content, hex-encoded. Pass it as the next
                             *     write's `expected_sha256`.
                             */
                            sha256: string;
                            /**
                             * Format: uint64
                             * @description Size in bytes.
                             */
                            size: number;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/generations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List generations */
        get: {
            parameters: {
                query?: {
                    limit?: number | null;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Matching generations, newest first. */
                            generations: {
                                /** @description RFC 3339 time the generation went live, or null if it never did. */
                                activated_at: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of blocks in the generation's block manifest.
                                 */
                                blocks: number;
                                /** @description What created this generation. */
                                cause: "site_write" | "site_delete" | "block_compile" | "block_remove" | "rollback" | "seed";
                                /** @description RFC 3339 creation time. */
                                created_at: string;
                                /** @description Generation id. */
                                id: string;
                                /** @description The generation this one was derived from, or null for the first. */
                                parent_id: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of files in the generation's site manifest.
                                 */
                                site_files: number;
                                /** @description Where the generation sits in its lifecycle. */
                                status: "staged" | "validating" | "activating" | "active" | "failed" | "superseded";
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/generations/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Read one generation */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description What this generation changed relative to the one it was derived from.
                             *     A generation with no parent adds everything it holds.
                             */
                            diff_from_parent: {
                                /** @description Blocks the generation adds. */
                                added_blocks: string[];
                                /** @description Site paths the generation adds. */
                                added_paths: string[];
                                /** @description Blocks whose spec changed. */
                                changed_blocks: string[];
                                /** @description Site paths whose content hash changed. */
                                changed_paths: string[];
                                /** @description Blocks the generation drops. */
                                removed_blocks: string[];
                                /** @description Site paths the generation drops. */
                                removed_paths: string[];
                            };
                            /** @description The manifest the generation publishes: its site files and its blocks. */
                            manifest: {
                                /** @description The blocks the generation's runtime is built from. */
                                blocks: {
                                    /** @description SHA-256 of the `wasm32-wasip1` artifact, hex-encoded. */
                                    artifact_sha256: string;
                                    /**
                                     * @description Capabilities the guest is loaded under. Deny-by-default; the caller
                                     *     validates the declared set against the block's own namespace before
                                     *     this ever reaches [`RuntimeControl`].
                                     *
                                     *     `BlockCapabilities` is a producer type that derives neither
                                     *     `JsonSchema` nor `PartialEq`. It is published as a free-form object
                                     *     here, and compared field-by-field in the `PartialEq` impl below.
                                     */
                                    capabilities: unknown;
                                    /** @description Registered block name (`site/{name}`). */
                                    name: string;
                                    /** @description Route prefixes the block serves. */
                                    routes: {
                                        /** @description Access tier the router enforces for the prefix. */
                                        access: "Public" | "Authenticated" | "Admin";
                                        /** @description Route prefix, normalized and under `/b/{block}/`. */
                                        prefix: string;
                                    }[];
                                    /**
                                     * Format: uint32
                                     * @description `wafer_guest.rs` ABI version the artifact was built against.
                                     */
                                    wafer_guest_version: number;
                                }[];
                                /** @description The generation this manifest describes. */
                                generation_id: string;
                                /** @description The generation this one was derived from, or null for the first. */
                                parent_id: string | null;
                                /**
                                 * Format: uint32
                                 * @description Manifest schema version. `1` for every manifest this build writes.
                                 */
                                schema_version: number;
                                /** @description The files the generation publishes. */
                                site: {
                                    /**
                                     * @description Every file the generation publishes, path relative to the site root.
                                     * @default []
                                     */
                                    files: {
                                        /** @description Content type the file is served with. */
                                        content_type: string;
                                        /**
                                         * @description Where the file lives. Workspace-relative (`site/index.html`) in the
                                         *     files API; relative to its area's root (`index.html`) in a
                                         *     generation's site manifest and in a block's source listing.
                                         */
                                        path: string;
                                        /** @description SHA-256 of the file's content-addressed blob, hex-encoded. */
                                        sha256: string;
                                        /**
                                         * Format: uint64
                                         * @description Size in bytes.
                                         */
                                        size: number;
                                    }[];
                                };
                            };
                            /** @description The ledger entry. */
                            summary: {
                                /** @description RFC 3339 time the generation went live, or null if it never did. */
                                activated_at: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of blocks in the generation's block manifest.
                                 */
                                blocks: number;
                                /** @description What created this generation. */
                                cause: "site_write" | "site_delete" | "block_compile" | "block_remove" | "rollback" | "seed";
                                /** @description RFC 3339 creation time. */
                                created_at: string;
                                /** @description Generation id. */
                                id: string;
                                /** @description The generation this one was derived from, or null for the first. */
                                parent_id: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of files in the generation's site manifest.
                                 */
                                site_files: number;
                                /** @description Where the generation sits in its lifecycle. */
                                status: "staged" | "validating" | "activating" | "active" | "failed" | "superseded";
                            };
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/generations/{id}/rollback": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Republish an earlier generation */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description The generation that went live. */
                            generation: {
                                /** @description RFC 3339 time the generation went live, or null if it never did. */
                                activated_at: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of blocks in the generation's block manifest.
                                 */
                                blocks: number;
                                /** @description What created this generation. */
                                cause: "site_write" | "site_delete" | "block_compile" | "block_remove" | "rollback" | "seed";
                                /** @description RFC 3339 creation time. */
                                created_at: string;
                                /** @description Generation id. */
                                id: string;
                                /** @description The generation this one was derived from, or null for the first. */
                                parent_id: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of files in the generation's site manifest.
                                 */
                                site_files: number;
                                /** @description Where the generation sits in its lifecycle. */
                                status: "staged" | "validating" | "activating" | "active" | "failed" | "superseded";
                            };
                            /**
                             * @description One entry per phase the activation passed through, with how long it
                             *     took. The last is always `active`.
                             */
                            progress: {
                                /** @description Human-readable detail for the progress panel. */
                                detail: string;
                                /**
                                 * Format: uint64
                                 * @description Milliseconds spent in it.
                                 */
                                ms: number;
                                /** @description The phase this step covers. */
                                phase: "idle" | "validating" | "building_runtime" | "publishing" | "active" | "failed";
                            }[];
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/reference": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The backend-block authoring reference
         * @description The guide for writing a block: the wafer_guest.rs API, the database / storage / config services, the namespace and capability rules, the limits, the diagnostic codes, and both templates in full.
         */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description The authoring guide, as Markdown: the API, the host services, the
                             *     namespace rules, the limits, the diagnostic codes, and both templates
                             *     in full.
                             */
                            markdown: string;
                            /**
                             * @description The current `src/wafer_guest.rs`, verbatim: what a block built against
                             *     an older copy writes over its own before compiling again.
                             */
                            wafer_guest_module: string;
                            /**
                             * Format: uint32
                             * @description The `WAFER_GUEST_VERSION` of the support module this reference
                             *     documents and `POST /b/dev/api/blocks` writes.
                             */
                            wafer_guest_version: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/dev/api/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Sandbox status */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description The activation in progress, if any. */
                            activation: {
                                /** @description Human-readable detail for the progress panel. */
                                detail: string;
                                /** @description The generation being activated. */
                                generation_id: string;
                                /** @description Which phase it has reached. */
                                phase: "idle" | "validating" | "building_runtime" | "publishing" | "active" | "failed";
                            } | null;
                            /** @description The active generation, or null on a fresh instance. */
                            active_generation: {
                                /** @description RFC 3339 time the generation went live, or null if it never did. */
                                activated_at: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of blocks in the generation's block manifest.
                                 */
                                blocks: number;
                                /** @description What created this generation. */
                                cause: "site_write" | "site_delete" | "block_compile" | "block_remove" | "rollback" | "seed";
                                /** @description RFC 3339 creation time. */
                                created_at: string;
                                /** @description Generation id. */
                                id: string;
                                /** @description The generation this one was derived from, or null for the first. */
                                parent_id: string | null;
                                /**
                                 * Format: uint32
                                 * @description Number of files in the generation's site manifest.
                                 */
                                site_files: number;
                                /** @description Where the generation sits in its lifecycle. */
                                status: "staged" | "validating" | "activating" | "active" | "failed" | "superseded";
                            } | null;
                            /** @description Blocks in the active generation. */
                            blocks: {
                                /** @description SHA-256 of the artifact the block was loaded from, hex-encoded. */
                                artifact_sha256: string;
                                /** @description Registered block name. */
                                name: string;
                                /**
                                 * @description Route prefixes the block serves, with the access tier the router
                                 *     enforces for each.
                                 */
                                routes: {
                                    /** @description Access tier the router enforces for the prefix. */
                                    access: "Public" | "Authenticated" | "Admin";
                                    /** @description Route prefix, normalized and under `/b/{block}/`. */
                                    prefix: string;
                                }[];
                            }[];
                            /**
                             * Format: uint64
                             * @description Bumped on every runtime rebuild; the page refreshes tool registrations
                             *     when it changes.
                             */
                            runtime_generation: number;
                            /**
                             * @description Why this instance's seed import was refused, if it was.
                             *
                             *     `None` on every healthy instance — including one that never had a seed
                             *     bundle to import. A sandbox whose seed was refused boots with an empty
                             *     site and no other sign of it (`dev_runtime::install` logs and carries
                             *     on, because a sandbox that refuses to boot is one whose `/b/dev` page
                             *     — the only thing that could fix it — never comes up), so this is what
                             *     makes the cause readable through `dev_status` instead of only through
                             *     the service worker's console. Read from the same row an admin sees on
                             *     `/b/admin/settings/variables`, which is the only surface an exported
                             *     site has (it has no `/b/dev`). `dev.js` does not render it: the page
                             *     polls this endpoint several times a second, so a log line would need
                             *     its own "said this already" state, and the agent that would act on a
                             *     refused seed reads `dev_status` rather than the log.
                             */
                            seed_error: string | null;
                            /** @description What the sandbox's content stores hold right now. */
                            storage: {
                                /**
                                 * Format: uint32
                                 * @description How many compiled block artifacts are stored.
                                 */
                                artifacts: number;
                                /**
                                 * Format: uint64
                                 * @description Total size of those artifacts, in bytes.
                                 */
                                artifacts_bytes: number;
                                /**
                                 * Format: uint32
                                 * @description How many content-addressed blobs are stored. Includes blobs no file
                                 *     names any more but a retained generation still does.
                                 */
                                blobs: number;
                                /**
                                 * Format: uint64
                                 * @description Total size of those blobs, in bytes.
                                 */
                                blobs_bytes: number;
                                /**
                                 * Format: uint32
                                 * @description How many generations the ledger is keeping: the retention window, plus
                                 *     anything older that is still serving or still in flight.
                                 */
                                retained_generations: number;
                                /**
                                 * Format: uint32
                                 * @description How many files the workspace currently holds.
                                 */
                                workspace_files: number;
                            };
                            /**
                             * Format: uint32
                             * @description `wafer_guest.rs` version the block scaffolder currently writes.
                             */
                            wafer_guest_version: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/legalpages/api/documents/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /** Update document */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @description New Markdown body. Omitted leaves the stored body. */
                        content?: string | null;
                        /** @description New document title. Omitted leaves the stored title. */
                        title?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        trace?: never;
    };
    "/b/llm/api/chat": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Send a chat message */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * Format: uint32
                         * @description Largest reply, in output tokens, this turn may generate. Omitted, the
                         *     deployment's `IMPRESSPRESS__LLM__DEFAULT_MAX_TOKENS` applies — every
                         *     request reaches the provider with a budget, because
                         *     Anthropic-protocol providers refuse one that carries none.
                         *
                         *     Must be at least 1: zero asks for no answer at all and is answered
                         *     `400` here rather than by the provider. There is no upper bound on
                         *     this side, deliberately — the real ceiling is the model's, it differs
                         *     per model and per provider, and the provider is the only party that
                         *     knows it. A value the model will not accept comes back as that
                         *     provider's own error rather than one invented here. (The configured
                         *     default is treated differently: a value that is not a positive integer
                         *     falls back with a warning, because an operator's typo must not break
                         *     every chat on the deployment at once.)
                         */
                        max_tokens?: number | null;
                        /** @description The user's message. */
                        message: string;
                        /** @description Model id within the provider. Same precedence as `provider`. */
                        model?: string | null;
                        /**
                         * @description Provider to route to, by name. A per-thread override set through
                         *     `POST /b/llm/api/config` takes precedence; when neither is set the
                         *     configured default provider is used.
                         */
                        provider?: string | null;
                        /**
                         * @description Messages-block context id the conversation lives in. The user turn is
                         *     stored there before the model runs and the assistant turn after it.
                         */
                        thread_id: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description The assistant's reply: every text delta the model produced,
                             *     concatenated.
                             */
                            content: string;
                            /**
                             * @description Id of the assistant entry persisted in the messages block. Always
                             *     populated: a reply the store refused is answered with a 500, not a
                             *     body (see `routes::chat::handle_chat`).
                             */
                            message_id: string;
                            /**
                             * @description The model the request was served by, after per-thread and default
                             *     resolution.
                             */
                            model: string;
                            /**
                             * @description `true` when `content` is a prefix of the answer rather than the whole
                             *     of it. Two ceilings can do that, and the flag does not distinguish
                             *     them because a caller's response to either is the same — ask again,
                             *     or ask for less:
                             *
                             *     * the model stopped at its output-token budget (the request's own
                             *       `max_tokens`, or `IMPRESSPRESS__LLM__DEFAULT_MAX_TOKENS`), which it
                             *       reports as a `length` finish reason;
                             *     * the reply exceeded the 1 MiB buffering cap of this endpoint, in
                             *       which case the text ends at the last delta that fitted and nothing
                             *       after it is appended, so it is never spliced across a gap.
                             */
                            truncated: boolean;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/llm/api/chat/stream": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Send a chat message (SSE streaming) */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * Format: uint32
                         * @description Largest reply, in output tokens, this turn may generate. Omitted, the
                         *     deployment's `IMPRESSPRESS__LLM__DEFAULT_MAX_TOKENS` applies — every
                         *     request reaches the provider with a budget, because
                         *     Anthropic-protocol providers refuse one that carries none.
                         *
                         *     Must be at least 1: zero asks for no answer at all and is answered
                         *     `400` here rather than by the provider. There is no upper bound on
                         *     this side, deliberately — the real ceiling is the model's, it differs
                         *     per model and per provider, and the provider is the only party that
                         *     knows it. A value the model will not accept comes back as that
                         *     provider's own error rather than one invented here. (The configured
                         *     default is treated differently: a value that is not a positive integer
                         *     falls back with a warning, because an operator's typo must not break
                         *     every chat on the deployment at once.)
                         */
                        max_tokens?: number | null;
                        /** @description The user's message. */
                        message: string;
                        /** @description Model id within the provider. Same precedence as `provider`. */
                        model?: string | null;
                        /**
                         * @description Provider to route to, by name. A per-thread override set through
                         *     `POST /b/llm/api/config` takes precedence; when neither is set the
                         *     configured default provider is used.
                         */
                        provider?: string | null;
                        /**
                         * @description Messages-block context id the conversation lives in. The user turn is
                         *     stored there before the model runs and the assistant turn after it.
                         */
                        thread_id: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/llm/api/config": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get default provider/model config */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Default model id (`IMPRESSPRESS__LLM__DEFAULT_MODEL`). Empty means the
                             *     provider's own default.
                             */
                            default_model: string;
                            /** @description Default provider name (`IMPRESSPRESS__LLM__DEFAULT_PROVIDER`). */
                            default_provider: string;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Update per-thread provider/model override */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * @description Not settable here: the global default is
                         *     `IMPRESSPRESS__LLM__DEFAULT_MODEL`. Sending it is refused.
                         */
                        default_model?: string | null;
                        /**
                         * @description Not settable here: the global default is
                         *     `IMPRESSPRESS__LLM__DEFAULT_PROVIDER`. Sending it is refused.
                         */
                        default_provider?: string | null;
                        /**
                         * @description Model id to pin the thread to. When creating an override, omitted
                         *     means `""` — use the default model.
                         */
                        model?: string | null;
                        /**
                         * @description Provider name to pin the thread to. When creating an override, omitted
                         *     means `""` — use the default provider.
                         */
                        provider_block?: string | null;
                        /**
                         * @description Thread to set the override on. Without it nothing is written and the
                         *     request is only acknowledged.
                         */
                        thread_id?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description RFC 3339 creation timestamp. */
                            created_at: string;
                            /** @description Stable row identifier. */
                            id: string;
                            /** @description Pinned model id. Empty means the default model. */
                            model: string;
                            /** @description Pinned provider name. Empty means the default provider. */
                            provider_block: string;
                            /** @description Messages-block context id the override applies to. */
                            thread_id: string;
                            /** @description RFC 3339 timestamp of the last modification. */
                            updated_at: string;
                        } | {
                            /**
                             * @description Always `true`, although nothing was written: the request named no
                             *     `thread_id`, so there was no override to change.
                             */
                            updated: boolean;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/llm/api/config/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Remove a per-thread provider/model override */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Always `true`: a delete that did not happen is an error response. */
                            deleted: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/llm/api/models": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List available models (aggregated across backends) */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            models: {
                                /** @description Backend (provider name) the model is hosted in. */
                                backend_id: string;
                                /** @description Declared model capabilities. */
                                capabilities: {
                                    /** @description Supports structured JSON output mode. */
                                    json_mode: boolean;
                                    /**
                                     * Format: uint32
                                     * @description Maximum input context window in tokens, when the backend reports one.
                                     */
                                    max_context_tokens: number | null;
                                    /**
                                     * Format: uint32
                                     * @description Maximum output tokens per request, when the backend reports one.
                                     */
                                    max_output_tokens: number | null;
                                    /** @description Supports streaming responses. */
                                    streaming: boolean;
                                    /** @description Supports tool / function calling. */
                                    tools: boolean;
                                    /** @description Accepts image inputs. */
                                    vision: boolean;
                                };
                                /** @description Human-readable display name. */
                                display_name: string;
                                /** @description Backend-side model identifier. */
                                model_id: string;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/llm/api/models/{backend_id}/{model_id}/load": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Load a model (SSE progress) */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    backend_id: string;
                    model_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/llm/api/models/{backend_id}/{model_id}/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Model status (ready / loading / unloaded) */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    backend_id: string;
                    model_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Lifecycle status of one model on one backend. Mirrors
                             *     `wafer_core::clients::llm::ModelStatus`.
                             */
                            status: {
                                /**
                                 * Format: float
                                 * @description Load progress in `0.0..=1.0`. Present only while `state` is
                                 *     `Loading`.
                                 */
                                progress?: number;
                                /** @description High-level state. */
                                state: "Ready" | "Loading" | "Unloaded" | {
                                    Error: {
                                        /** @description Failure message. */
                                        message: string;
                                    };
                                };
                            };
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/llm/api/models/{backend_id}/{model_id}/unload": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Unload a model */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    backend_id: string;
                    model_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Always `true`: an unload that did not happen is an error response. */
                            unloaded: boolean;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/llm/api/providers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List configured LLM providers */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            providers: {
                                /** @description Whether chat requests may route to this provider. */
                                enabled: boolean;
                                /** @description Base URL of the provider's API, e.g. `https://api.openai.com/v1`. */
                                endpoint: string;
                                /** @description Stable row identifier, used by the `/b/llm/api/providers/{id}` routes. */
                                id: string;
                                /**
                                 * @description Name of the admin configuration variable holding this provider's API
                                 *     key, or `null` for a provider that runs unauthenticated. The key
                                 *     itself is never published.
                                 */
                                key_var: string | null;
                                /**
                                 * @description Which field carries the output-token budget in this provider's chat
                                 *     bodies, or `null` to send the one its `protocol` implies.
                                 */
                                max_tokens_field: ("max_tokens" | "max_completion_tokens") | null;
                                /**
                                 * @description Explicit model list. Empty means the models are discovered from the
                                 *     provider's `/v1/models`.
                                 */
                                models: string[];
                                /** @description Unique provider name. This is the `backend_id` chat requests route on. */
                                name: string;
                                /**
                                 * @description Wire protocol a configured provider speaks.
                                 *
                                 *     `open_ai` and `anthropic` are the providers' native APIs.
                                 *     `open_ai_compatible` covers every third-party endpoint implementing
                                 *     OpenAI's `/v1` interface — Ollama, llama-server, LM Studio, vLLM, LocalAI,
                                 *     KoboldCpp, Azure OpenAI, Groq, Together, OpenRouter, Mistral API, Anyscale,
                                 *     and so on.
                                 * @enum {string}
                                 */
                                protocol: "open_ai" | "anthropic" | "open_ai_compatible";
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create LLM provider */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * @description Whether chat requests may route to this provider. Defaults to `true`
                         *     when the JSON body omits it; an unticked checkbox on the form path is
                         *     `false`, because that is the only thing an absent checkbox can mean.
                         */
                        enabled?: boolean | null;
                        /**
                         * @description Base URL of the provider's API. Must resolve to a public address:
                         *     private, link-local and cloud-metadata ranges are refused.
                         */
                        endpoint: string;
                        /**
                         * @description Name of the admin configuration variable holding the API key. Omit,
                         *     or send an empty string, for a provider that needs no key.
                         */
                        key_var?: string | null;
                        /**
                         * @description Which field carries the output-token budget in this provider's chat
                         *     bodies. Omit to send the one `protocol` implies, which is what all but
                         *     a handful of endpoints want. Refused on the `anthropic` protocol,
                         *     whose wire format has only one such field.
                         */
                        max_tokens_field?: ("max_tokens" | "max_completion_tokens") | null;
                        /**
                         * @description Explicit model list. Omitted or empty means the models are discovered
                         *     from the provider's `/v1/models`.
                         */
                        models?: string[] | null;
                        /** @description Unique provider name. Becomes the `backend_id` chat requests route on. */
                        name: string;
                        /**
                         * @description Wire protocol a configured provider speaks.
                         *
                         *     `open_ai` and `anthropic` are the providers' native APIs.
                         *     `open_ai_compatible` covers every third-party endpoint implementing
                         *     OpenAI's `/v1` interface — Ollama, llama-server, LM Studio, vLLM, LocalAI,
                         *     KoboldCpp, Azure OpenAI, Groq, Together, OpenRouter, Mistral API, Anyscale,
                         *     and so on.
                         * @enum {string}
                         */
                        protocol: "open_ai" | "anthropic" | "open_ai_compatible";
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Whether chat requests may route to this provider. */
                            enabled: boolean;
                            /** @description Base URL of the provider's API, e.g. `https://api.openai.com/v1`. */
                            endpoint: string;
                            /** @description Stable row identifier, used by the `/b/llm/api/providers/{id}` routes. */
                            id: string;
                            /**
                             * @description Name of the admin configuration variable holding this provider's API
                             *     key, or `null` for a provider that runs unauthenticated. The key
                             *     itself is never published.
                             */
                            key_var: string | null;
                            /**
                             * @description Which field carries the output-token budget in this provider's chat
                             *     bodies, or `null` to send the one its `protocol` implies.
                             */
                            max_tokens_field: ("max_tokens" | "max_completion_tokens") | null;
                            /**
                             * @description Explicit model list. Empty means the models are discovered from the
                             *     provider's `/v1/models`.
                             */
                            models: string[];
                            /** @description Unique provider name. This is the `backend_id` chat requests route on. */
                            name: string;
                            /**
                             * @description Wire protocol a configured provider speaks.
                             *
                             *     `open_ai` and `anthropic` are the providers' native APIs.
                             *     `open_ai_compatible` covers every third-party endpoint implementing
                             *     OpenAI's `/v1` interface — Ollama, llama-server, LM Studio, vLLM, LocalAI,
                             *     KoboldCpp, Azure OpenAI, Groq, Together, OpenRouter, Mistral API, Anyscale,
                             *     and so on.
                             * @enum {string}
                             */
                            protocol: "open_ai" | "anthropic" | "open_ai_compatible";
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/llm/api/providers/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Delete LLM provider */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Always `true`: a delete that did not happen is an error response. */
                            deleted: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        /** Update LLM provider */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        enabled?: boolean | null;
                        /** @description Re-validated on every change: must resolve to a public address. */
                        endpoint?: string | null;
                        /**
                         * @description Name of the admin configuration variable holding the API key, or
                         *     `null` (or `""`) to leave the provider unauthenticated.
                         */
                        key_var?: string | null;
                        /**
                         * @description Which field carries the output-token budget, or `null` to go back to
                         *     the one `protocol` implies.
                         */
                        max_tokens_field?: ("max_tokens" | "max_completion_tokens") | null;
                        models?: string[] | null;
                        name?: string | null;
                        /**
                         * @description Wire protocol a configured provider speaks.
                         *
                         *     `open_ai` and `anthropic` are the providers' native APIs.
                         *     `open_ai_compatible` covers every third-party endpoint implementing
                         *     OpenAI's `/v1` interface — Ollama, llama-server, LM Studio, vLLM, LocalAI,
                         *     KoboldCpp, Azure OpenAI, Groq, Together, OpenRouter, Mistral API, Anyscale,
                         *     and so on.
                         * @enum {string|null}
                         */
                        protocol?: "open_ai" | "anthropic" | "open_ai_compatible" | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Whether chat requests may route to this provider. */
                            enabled: boolean;
                            /** @description Base URL of the provider's API, e.g. `https://api.openai.com/v1`. */
                            endpoint: string;
                            /** @description Stable row identifier, used by the `/b/llm/api/providers/{id}` routes. */
                            id: string;
                            /**
                             * @description Name of the admin configuration variable holding this provider's API
                             *     key, or `null` for a provider that runs unauthenticated. The key
                             *     itself is never published.
                             */
                            key_var: string | null;
                            /**
                             * @description Which field carries the output-token budget in this provider's chat
                             *     bodies, or `null` to send the one its `protocol` implies.
                             */
                            max_tokens_field: ("max_tokens" | "max_completion_tokens") | null;
                            /**
                             * @description Explicit model list. Empty means the models are discovered from the
                             *     provider's `/v1/models`.
                             */
                            models: string[];
                            /** @description Unique provider name. This is the `backend_id` chat requests route on. */
                            name: string;
                            /**
                             * @description Wire protocol a configured provider speaks.
                             *
                             *     `open_ai` and `anthropic` are the providers' native APIs.
                             *     `open_ai_compatible` covers every third-party endpoint implementing
                             *     OpenAI's `/v1` interface — Ollama, llama-server, LM Studio, vLLM, LocalAI,
                             *     KoboldCpp, Azure OpenAI, Groq, Together, OpenRouter, Mistral API, Anyscale,
                             *     and so on.
                             * @enum {string}
                             */
                            protocol: "open_ai" | "anthropic" | "open_ai_compatible";
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/llm/api/providers/{id}/discover-models": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Discover provider models via /v1/models */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Model ids the provider reported — or its explicit list, when one is
                             *     configured — now stored as the provider's `models`.
                             */
                            models: string[];
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/messages/api/contexts": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List contexts
         * @description List contexts with optional filters by type, status, sender_id, parent_id
         */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                    parent_id?: string;
                    sender_id?: string;
                    status?: string;
                    type?: string;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            records?: Record<string, never>[];
                            total_count?: number;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create context */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        metadata?: unknown;
                        /** @description Parent context ID for sub-tasks/threads. */
                        parent_id?: string | null;
                        /** @default  */
                        recipient_id?: string;
                        /** @default  */
                        sender_id?: string;
                        /** @default  */
                        title?: string;
                        /** @description Context type: conversation, task, notification, etc. */
                        type: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/messages/api/contexts/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get context */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /** Update context */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        metadata?: unknown;
                        status?: string | null;
                        title?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        trace?: never;
    };
    "/b/messages/api/contexts/{id}/entries": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List entries in context */
        get: {
            parameters: {
                query?: {
                    kind?: "message" | "artifact" | "notification" | "status";
                    page?: number;
                    page_size?: number;
                    role?: "user" | "assistant" | "system";
                };
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        put?: never;
        /** Add entry to context */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @default  */
                        content?: string;
                        /**
                         * @description MIME type of `content`. Defaults to `text/plain` when omitted or
                         *     explicitly `null`.
                         * @default text/plain
                         */
                        content_type?: string | null;
                        /**
                         * @description What this entry is.
                         * @default message
                         * @enum {string}
                         */
                        kind?: "message" | "artifact" | "notification" | "status";
                        metadata?: unknown;
                        /**
                         * @description Who produced it. `agent` is accepted as an alias of `assistant` and
                         *     is stored as `assistant`.
                         * @default user
                         * @enum {string}
                         */
                        role?: "user" | "assistant" | "system";
                        /** @default  */
                        sender_id?: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/groups": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List groups */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /** @description Groups on this page. */
                            records: {
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /**
                                 * @description Id of the user who created the row: the administrator on the admin
                                 *     tier (the owner is `user_id`), the owner on the owner tier.
                                 */
                                created_by: string;
                                description: string;
                                /** @description Group template the group was created from. */
                                group_template_id: string;
                                /** @description Stable group identifier. */
                                id: string;
                                name: string;
                                /** @description `active` unless the group has been retired. */
                                status: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                                /**
                                 * @description Id of the user who owns the group. The owner tier lists and edits
                                 *     only groups whose `user_id` is the caller.
                                 */
                                user_id: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total groups matching the filters, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create group */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        description?: string | null;
                        group_template_id?: string | null;
                        name: string;
                        status?: string | null;
                        /** @description Owner of the group. Defaults to the administrator creating it. */
                        user_id?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /**
                             * @description Id of the user who created the row: the administrator on the admin
                             *     tier (the owner is `user_id`), the owner on the owner tier.
                             */
                            created_by: string;
                            description: string;
                            /** @description Group template the group was created from. */
                            group_template_id: string;
                            /** @description Stable group identifier. */
                            id: string;
                            name: string;
                            /** @description `active` unless the group has been retired. */
                            status: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                            /**
                             * @description Id of the user who owns the group. The owner tier lists and edits
                             *     only groups whose `user_id` is the caller.
                             */
                            user_id: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/groups/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Delete group */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Always `true`: a delete that did not happen is an error response. */
                            deleted: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        /** Update group */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        description?: string | null;
                        group_template_id?: string | null;
                        name?: string | null;
                        status?: string | null;
                        /** @description Owner of the group. */
                        user_id?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /**
                             * @description Id of the user who created the row: the administrator on the admin
                             *     tier (the owner is `user_id`), the owner on the owner tier.
                             */
                            created_by: string;
                            description: string;
                            /** @description Group template the group was created from. */
                            group_template_id: string;
                            /** @description Stable group identifier. */
                            id: string;
                            name: string;
                            /** @description `active` unless the group has been retired. */
                            status: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                            /**
                             * @description Id of the user who owns the group. The owner tier lists and edits
                             *     only groups whose `user_id` is the caller.
                             */
                            user_id: string;
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/products/api/admin/products": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List products */
        get: {
            parameters: {
                query?: {
                    group_id?: string | null;
                    page?: number;
                    page_size?: number;
                    search?: string | null;
                    status?: string | null;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /** @description Products on this page, newest first. */
                            records: {
                                /**
                                 * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                                 *     `rejected` or `suspended`. A different column and a different
                                 *     vocabulary from `status` — a listing awaiting review is
                                 *     `status = pending_review` and `approval_status = pending` at once.
                                 * @enum {string}
                                 */
                                approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                                category: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description Id of the user who created the row. */
                                created_by: string;
                                /** @description ISO 4217 presentment currency. */
                                currency: string;
                                /**
                                 * Format: int64
                                 * @description Version counter of the product's immutable offer definitions.
                                 */
                                current_version: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                                 *     soft-deleted.
                                 */
                                deleted_at: string | null;
                                description: string;
                                /**
                                 * @description How a purchase is fulfilled.
                                 * @enum {string}
                                 */
                                fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                                /** @description Group the product is listed under, or empty. */
                                group_id: string;
                                group_template_id: string;
                                /** @description Stable product identifier. */
                                id: string;
                                image_url: string;
                                /** @description Free-form key/value metadata attached by the product builder. */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                name: string;
                                /** @description Owning seller's user id; empty for platform products. */
                                owner_id: string;
                                /**
                                 * @description `platform` for an administrator-owned product, `user` for a seller's.
                                 * @enum {string}
                                 */
                                owner_kind: "platform" | "user";
                                /** @description Product builder template this product was created from. */
                                product_template_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the product last became active, or `null`.
                                 */
                                published_at: string | null;
                                /** @description Id of a product the buyer must already own before checkout, or empty. */
                                requires: string;
                                /** @description Seller account the product sells through; empty for platform products. */
                                seller_account_id: string;
                                /**
                                 * @description URL slug, unique per owner among non-deleted products. Empty when the
                                 *     product has none.
                                 */
                                slug: string;
                                /**
                                 * @description Publication state: `draft`, `pending_review` (seller product awaiting
                                 *     moderation), `active` (in the public catalog) or `archived`.
                                 * @enum {string}
                                 */
                                status: "draft" | "pending_review" | "active" | "archived";
                                /**
                                 * Format: int64
                                 * @description Units in stock; `0` when inventory is not tracked.
                                 */
                                stock: number;
                                /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                                stripe_product_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the seller last submitted the product for
                                 *     moderation, or `null`.
                                 */
                                submitted_at: string | null;
                                tags: string[];
                                /** @description Product type (taxonomy) id, or empty. */
                                type_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total products matching the filters, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create product */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        category?: string | null;
                        /**
                         * @description ISO 4217 currency. A seller product defaults to the platform default
                         *     currency when omitted.
                         */
                        currency?: string | null;
                        description?: string | null;
                        /** @enum {string|null} */
                        fulfillment_kind?: "none" | "manual" | "download" | "entitlement" | "webhook" | null;
                        /**
                         * @description Group to list the product under. A seller may only use a group they
                         *     own.
                         */
                        group_id?: string | null;
                        group_template_id?: string | null;
                        image_url?: string | null;
                        /** @description Free-form key/value metadata. */
                        metadata?: {
                            [key: string]: unknown;
                        } | null;
                        name: string;
                        /**
                         * @description Product builder template. A seller product defaults to the seeded
                         *     `default` template when omitted.
                         */
                        product_template_id?: string | null;
                        /** @description Id of a product the buyer must already own before checkout. */
                        requires?: string | null;
                        /**
                         * @description Web address of the product: lowercase letters and digits in runs
                         *     joined by single hyphens, at most 160 characters. Empty means none.
                         */
                        slug?: string | null;
                        /**
                         * @description Initial publication state. Defaults to `draft`; a seller product is
                         *     always created as `draft` regardless of this value.
                         * @enum {string|null}
                         */
                        status?: "draft" | "pending_review" | "active" | "archived" | null;
                        /** Format: int64 */
                        stock?: number | null;
                        tags?: string[] | null;
                        type_id?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                             *     `rejected` or `suspended`. A different column and a different
                             *     vocabulary from `status` — a listing awaiting review is
                             *     `status = pending_review` and `approval_status = pending` at once.
                             * @enum {string}
                             */
                            approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                            category: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /** @description Id of the user who created the row. */
                            created_by: string;
                            /** @description ISO 4217 presentment currency. */
                            currency: string;
                            /**
                             * Format: int64
                             * @description Version counter of the product's immutable offer definitions.
                             */
                            current_version: number;
                            /**
                             * Format: date-time
                             * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                             *     soft-deleted.
                             */
                            deleted_at: string | null;
                            description: string;
                            /**
                             * @description How a purchase is fulfilled.
                             * @enum {string}
                             */
                            fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                            /** @description Group the product is listed under, or empty. */
                            group_id: string;
                            group_template_id: string;
                            /** @description Stable product identifier. */
                            id: string;
                            image_url: string;
                            /** @description Free-form key/value metadata attached by the product builder. */
                            metadata: {
                                [key: string]: unknown;
                            };
                            name: string;
                            /** @description Owning seller's user id; empty for platform products. */
                            owner_id: string;
                            /**
                             * @description `platform` for an administrator-owned product, `user` for a seller's.
                             * @enum {string}
                             */
                            owner_kind: "platform" | "user";
                            /** @description Product builder template this product was created from. */
                            product_template_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the product last became active, or `null`.
                             */
                            published_at: string | null;
                            /** @description Id of a product the buyer must already own before checkout, or empty. */
                            requires: string;
                            /** @description Seller account the product sells through; empty for platform products. */
                            seller_account_id: string;
                            /**
                             * @description URL slug, unique per owner among non-deleted products. Empty when the
                             *     product has none.
                             */
                            slug: string;
                            /**
                             * @description Publication state: `draft`, `pending_review` (seller product awaiting
                             *     moderation), `active` (in the public catalog) or `archived`.
                             * @enum {string}
                             */
                            status: "draft" | "pending_review" | "active" | "archived";
                            /**
                             * Format: int64
                             * @description Units in stock; `0` when inventory is not tracked.
                             */
                            stock: number;
                            /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                            stripe_product_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the seller last submitted the product for
                             *     moderation, or `null`.
                             */
                            submitted_at: string | null;
                            tags: string[];
                            /** @description Product type (taxonomy) id, or empty. */
                            type_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/products/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get product */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                             *     `rejected` or `suspended`. A different column and a different
                             *     vocabulary from `status` — a listing awaiting review is
                             *     `status = pending_review` and `approval_status = pending` at once.
                             * @enum {string}
                             */
                            approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                            category: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /** @description Id of the user who created the row. */
                            created_by: string;
                            /** @description ISO 4217 presentment currency. */
                            currency: string;
                            /**
                             * Format: int64
                             * @description Version counter of the product's immutable offer definitions.
                             */
                            current_version: number;
                            /**
                             * Format: date-time
                             * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                             *     soft-deleted.
                             */
                            deleted_at: string | null;
                            description: string;
                            /**
                             * @description How a purchase is fulfilled.
                             * @enum {string}
                             */
                            fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                            /** @description Group the product is listed under, or empty. */
                            group_id: string;
                            group_template_id: string;
                            /** @description Stable product identifier. */
                            id: string;
                            image_url: string;
                            /** @description Free-form key/value metadata attached by the product builder. */
                            metadata: {
                                [key: string]: unknown;
                            };
                            name: string;
                            /** @description Owning seller's user id; empty for platform products. */
                            owner_id: string;
                            /**
                             * @description `platform` for an administrator-owned product, `user` for a seller's.
                             * @enum {string}
                             */
                            owner_kind: "platform" | "user";
                            /** @description Product builder template this product was created from. */
                            product_template_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the product last became active, or `null`.
                             */
                            published_at: string | null;
                            /** @description Id of a product the buyer must already own before checkout, or empty. */
                            requires: string;
                            /** @description Seller account the product sells through; empty for platform products. */
                            seller_account_id: string;
                            /**
                             * @description URL slug, unique per owner among non-deleted products. Empty when the
                             *     product has none.
                             */
                            slug: string;
                            /**
                             * @description Publication state: `draft`, `pending_review` (seller product awaiting
                             *     moderation), `active` (in the public catalog) or `archived`.
                             * @enum {string}
                             */
                            status: "draft" | "pending_review" | "active" | "archived";
                            /**
                             * Format: int64
                             * @description Units in stock; `0` when inventory is not tracked.
                             */
                            stock: number;
                            /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                            stripe_product_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the seller last submitted the product for
                             *     moderation, or `null`.
                             */
                            submitted_at: string | null;
                            tags: string[];
                            /** @description Product type (taxonomy) id, or empty. */
                            type_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        /** Delete product */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Always `true`: a delete that did not happen is an error response. */
                            deleted: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        /** Update product */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        category?: string | null;
                        /** @description ISO 4217 currency. */
                        currency?: string | null;
                        description?: string | null;
                        /** @enum {string|null} */
                        fulfillment_kind?: "none" | "manual" | "download" | "entitlement" | "webhook" | null;
                        group_id?: string | null;
                        group_template_id?: string | null;
                        image_url?: string | null;
                        /** @description Free-form key/value metadata. */
                        metadata?: {
                            [key: string]: unknown;
                        } | null;
                        name?: string | null;
                        /** @description Product builder template. */
                        product_template_id?: string | null;
                        /** @description Id of a product the buyer must already own before checkout. */
                        requires?: string | null;
                        /**
                         * @description Web address of the product: lowercase letters and digits in runs
                         *     joined by single hyphens, at most 160 characters. Empty means none.
                         */
                        slug?: string | null;
                        /**
                         * @description Publication state. A seller may set `draft`, `active` or `archived`;
                         *     `active` on a moderated product submits it for review instead.
                         * @enum {string|null}
                         */
                        status?: "draft" | "pending_review" | "active" | "archived" | null;
                        /** Format: int64 */
                        stock?: number | null;
                        tags?: string[] | null;
                        type_id?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                             *     `rejected` or `suspended`. A different column and a different
                             *     vocabulary from `status` — a listing awaiting review is
                             *     `status = pending_review` and `approval_status = pending` at once.
                             * @enum {string}
                             */
                            approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                            category: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /** @description Id of the user who created the row. */
                            created_by: string;
                            /** @description ISO 4217 presentment currency. */
                            currency: string;
                            /**
                             * Format: int64
                             * @description Version counter of the product's immutable offer definitions.
                             */
                            current_version: number;
                            /**
                             * Format: date-time
                             * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                             *     soft-deleted.
                             */
                            deleted_at: string | null;
                            description: string;
                            /**
                             * @description How a purchase is fulfilled.
                             * @enum {string}
                             */
                            fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                            /** @description Group the product is listed under, or empty. */
                            group_id: string;
                            group_template_id: string;
                            /** @description Stable product identifier. */
                            id: string;
                            image_url: string;
                            /** @description Free-form key/value metadata attached by the product builder. */
                            metadata: {
                                [key: string]: unknown;
                            };
                            name: string;
                            /** @description Owning seller's user id; empty for platform products. */
                            owner_id: string;
                            /**
                             * @description `platform` for an administrator-owned product, `user` for a seller's.
                             * @enum {string}
                             */
                            owner_kind: "platform" | "user";
                            /** @description Product builder template this product was created from. */
                            product_template_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the product last became active, or `null`.
                             */
                            published_at: string | null;
                            /** @description Id of a product the buyer must already own before checkout, or empty. */
                            requires: string;
                            /** @description Seller account the product sells through; empty for platform products. */
                            seller_account_id: string;
                            /**
                             * @description URL slug, unique per owner among non-deleted products. Empty when the
                             *     product has none.
                             */
                            slug: string;
                            /**
                             * @description Publication state: `draft`, `pending_review` (seller product awaiting
                             *     moderation), `active` (in the public catalog) or `archived`.
                             * @enum {string}
                             */
                            status: "draft" | "pending_review" | "active" | "archived";
                            /**
                             * Format: int64
                             * @description Units in stock; `0` when inventory is not tracked.
                             */
                            stock: number;
                            /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                            stripe_product_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the seller last submitted the product for
                             *     moderation, or `null`.
                             */
                            submitted_at: string | null;
                            tags: string[];
                            /** @description Product type (taxonomy) id, or empty. */
                            type_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/products/api/admin/products/{id}/approve": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Approve a seller product waiting for moderation */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                             *     `rejected` or `suspended`. A different column and a different
                             *     vocabulary from `status` — a listing awaiting review is
                             *     `status = pending_review` and `approval_status = pending` at once.
                             * @enum {string}
                             */
                            approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                            category: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /** @description Id of the user who created the row. */
                            created_by: string;
                            /** @description ISO 4217 presentment currency. */
                            currency: string;
                            /**
                             * Format: int64
                             * @description Version counter of the product's immutable offer definitions.
                             */
                            current_version: number;
                            /**
                             * Format: date-time
                             * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                             *     soft-deleted.
                             */
                            deleted_at: string | null;
                            description: string;
                            /**
                             * @description How a purchase is fulfilled.
                             * @enum {string}
                             */
                            fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                            /** @description Group the product is listed under, or empty. */
                            group_id: string;
                            group_template_id: string;
                            /** @description Stable product identifier. */
                            id: string;
                            image_url: string;
                            /** @description Free-form key/value metadata attached by the product builder. */
                            metadata: {
                                [key: string]: unknown;
                            };
                            name: string;
                            /** @description Owning seller's user id; empty for platform products. */
                            owner_id: string;
                            /**
                             * @description `platform` for an administrator-owned product, `user` for a seller's.
                             * @enum {string}
                             */
                            owner_kind: "platform" | "user";
                            /** @description Product builder template this product was created from. */
                            product_template_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the product last became active, or `null`.
                             */
                            published_at: string | null;
                            /** @description Id of a product the buyer must already own before checkout, or empty. */
                            requires: string;
                            /** @description Seller account the product sells through; empty for platform products. */
                            seller_account_id: string;
                            /**
                             * @description URL slug, unique per owner among non-deleted products. Empty when the
                             *     product has none.
                             */
                            slug: string;
                            /**
                             * @description Publication state: `draft`, `pending_review` (seller product awaiting
                             *     moderation), `active` (in the public catalog) or `archived`.
                             * @enum {string}
                             */
                            status: "draft" | "pending_review" | "active" | "archived";
                            /**
                             * Format: int64
                             * @description Units in stock; `0` when inventory is not tracked.
                             */
                            stock: number;
                            /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                            stripe_product_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the seller last submitted the product for
                             *     moderation, or `null`.
                             */
                            submitted_at: string | null;
                            tags: string[];
                            /** @description Product type (taxonomy) id, or empty. */
                            type_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/products/{id}/duplicate": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Duplicate product and editable offers */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offers: {
                                offer: {
                                    billing_scheme: string;
                                    checkout: Record<string, never>;
                                    components: Record<string, never>[];
                                    currency: string;
                                    id: string;
                                    interval_count: number;
                                    /** @enum {string} */
                                    mode: "payment" | "subscription";
                                    name: string;
                                    /** @enum {string} */
                                    pricing_model: "fixed" | "components";
                                    product_id: string;
                                    recurring_interval?: string | null;
                                    stripe_price_id: string;
                                    stripe_product_id: string;
                                    tax_behavior: string;
                                    usage_type: string;
                                    variables: Record<string, never>[];
                                    version: number;
                                };
                                /** @enum {string} */
                                status: "draft" | "active" | "archived";
                                sync_error: string;
                                sync_status: string;
                            }[];
                            /**
                             * ProductView
                             * @description A product row as published to its owner and to administrators: every
                             *     column of the products table.
                             */
                            product: {
                                /**
                                 * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                                 *     `rejected` or `suspended`. A different column and a different
                                 *     vocabulary from `status` — a listing awaiting review is
                                 *     `status = pending_review` and `approval_status = pending` at once.
                                 * @enum {string}
                                 */
                                approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                                category: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description Id of the user who created the row. */
                                created_by: string;
                                /** @description ISO 4217 presentment currency. */
                                currency: string;
                                /**
                                 * Format: int64
                                 * @description Version counter of the product's immutable offer definitions.
                                 */
                                current_version: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                                 *     soft-deleted.
                                 */
                                deleted_at: string | null;
                                description: string;
                                /**
                                 * @description How a purchase is fulfilled.
                                 * @enum {string}
                                 */
                                fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                                /** @description Group the product is listed under, or empty. */
                                group_id: string;
                                group_template_id: string;
                                /** @description Stable product identifier. */
                                id: string;
                                image_url: string;
                                /** @description Free-form key/value metadata attached by the product builder. */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                name: string;
                                /** @description Owning seller's user id; empty for platform products. */
                                owner_id: string;
                                /**
                                 * @description `platform` for an administrator-owned product, `user` for a seller's.
                                 * @enum {string}
                                 */
                                owner_kind: "platform" | "user";
                                /** @description Product builder template this product was created from. */
                                product_template_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the product last became active, or `null`.
                                 */
                                published_at: string | null;
                                /** @description Id of a product the buyer must already own before checkout, or empty. */
                                requires: string;
                                /** @description Seller account the product sells through; empty for platform products. */
                                seller_account_id: string;
                                /**
                                 * @description URL slug, unique per owner among non-deleted products. Empty when the
                                 *     product has none.
                                 */
                                slug: string;
                                /**
                                 * @description Publication state: `draft`, `pending_review` (seller product awaiting
                                 *     moderation), `active` (in the public catalog) or `archived`.
                                 * @enum {string}
                                 */
                                status: "draft" | "pending_review" | "active" | "archived";
                                /**
                                 * Format: int64
                                 * @description Units in stock; `0` when inventory is not tracked.
                                 */
                                stock: number;
                                /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                                stripe_product_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the seller last submitted the product for
                                 *     moderation, or `null`.
                                 */
                                submitted_at: string | null;
                                tags: string[];
                                /** @description Product type (taxonomy) id, or empty. */
                                type_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            };
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/products/{id}/reject": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Return a seller product to draft after moderation */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                             *     `rejected` or `suspended`. A different column and a different
                             *     vocabulary from `status` — a listing awaiting review is
                             *     `status = pending_review` and `approval_status = pending` at once.
                             * @enum {string}
                             */
                            approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                            category: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /** @description Id of the user who created the row. */
                            created_by: string;
                            /** @description ISO 4217 presentment currency. */
                            currency: string;
                            /**
                             * Format: int64
                             * @description Version counter of the product's immutable offer definitions.
                             */
                            current_version: number;
                            /**
                             * Format: date-time
                             * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                             *     soft-deleted.
                             */
                            deleted_at: string | null;
                            description: string;
                            /**
                             * @description How a purchase is fulfilled.
                             * @enum {string}
                             */
                            fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                            /** @description Group the product is listed under, or empty. */
                            group_id: string;
                            group_template_id: string;
                            /** @description Stable product identifier. */
                            id: string;
                            image_url: string;
                            /** @description Free-form key/value metadata attached by the product builder. */
                            metadata: {
                                [key: string]: unknown;
                            };
                            name: string;
                            /** @description Owning seller's user id; empty for platform products. */
                            owner_id: string;
                            /**
                             * @description `platform` for an administrator-owned product, `user` for a seller's.
                             * @enum {string}
                             */
                            owner_kind: "platform" | "user";
                            /** @description Product builder template this product was created from. */
                            product_template_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the product last became active, or `null`.
                             */
                            published_at: string | null;
                            /** @description Id of a product the buyer must already own before checkout, or empty. */
                            requires: string;
                            /** @description Seller account the product sells through; empty for platform products. */
                            seller_account_id: string;
                            /**
                             * @description URL slug, unique per owner among non-deleted products. Empty when the
                             *     product has none.
                             */
                            slug: string;
                            /**
                             * @description Publication state: `draft`, `pending_review` (seller product awaiting
                             *     moderation), `active` (in the public catalog) or `archived`.
                             * @enum {string}
                             */
                            status: "draft" | "pending_review" | "active" | "archived";
                            /**
                             * Format: int64
                             * @description Units in stock; `0` when inventory is not tracked.
                             */
                            stock: number;
                            /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                            stripe_product_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the seller last submitted the product for
                             *     moderation, or `null`.
                             */
                            submitted_at: string | null;
                            tags: string[];
                            /** @description Product type (taxonomy) id, or empty. */
                            type_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/products/{id}/restore": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Restore a soft-deleted product
         * @description Clears `deleted_at`, undoing `soft_delete`. A soft-deleted product is not editable through the normal admin PATCH until it is restored.
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                             *     `rejected` or `suspended`. A different column and a different
                             *     vocabulary from `status` — a listing awaiting review is
                             *     `status = pending_review` and `approval_status = pending` at once.
                             * @enum {string}
                             */
                            approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                            category: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /** @description Id of the user who created the row. */
                            created_by: string;
                            /** @description ISO 4217 presentment currency. */
                            currency: string;
                            /**
                             * Format: int64
                             * @description Version counter of the product's immutable offer definitions.
                             */
                            current_version: number;
                            /**
                             * Format: date-time
                             * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                             *     soft-deleted.
                             */
                            deleted_at: string | null;
                            description: string;
                            /**
                             * @description How a purchase is fulfilled.
                             * @enum {string}
                             */
                            fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                            /** @description Group the product is listed under, or empty. */
                            group_id: string;
                            group_template_id: string;
                            /** @description Stable product identifier. */
                            id: string;
                            image_url: string;
                            /** @description Free-form key/value metadata attached by the product builder. */
                            metadata: {
                                [key: string]: unknown;
                            };
                            name: string;
                            /** @description Owning seller's user id; empty for platform products. */
                            owner_id: string;
                            /**
                             * @description `platform` for an administrator-owned product, `user` for a seller's.
                             * @enum {string}
                             */
                            owner_kind: "platform" | "user";
                            /** @description Product builder template this product was created from. */
                            product_template_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the product last became active, or `null`.
                             */
                            published_at: string | null;
                            /** @description Id of a product the buyer must already own before checkout, or empty. */
                            requires: string;
                            /** @description Seller account the product sells through; empty for platform products. */
                            seller_account_id: string;
                            /**
                             * @description URL slug, unique per owner among non-deleted products. Empty when the
                             *     product has none.
                             */
                            slug: string;
                            /**
                             * @description Publication state: `draft`, `pending_review` (seller product awaiting
                             *     moderation), `active` (in the public catalog) or `archived`.
                             * @enum {string}
                             */
                            status: "draft" | "pending_review" | "active" | "archived";
                            /**
                             * Format: int64
                             * @description Units in stock; `0` when inventory is not tracked.
                             */
                            stock: number;
                            /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                            stripe_product_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the seller last submitted the product for
                             *     moderation, or `null`.
                             */
                            submitted_at: string | null;
                            tags: string[];
                            /** @description Product type (taxonomy) id, or empty. */
                            type_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/products/{product_id}/offers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List product offers */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offers: {
                                offer: {
                                    billing_scheme: string;
                                    checkout: Record<string, never>;
                                    components: Record<string, never>[];
                                    currency: string;
                                    id: string;
                                    interval_count: number;
                                    /** @enum {string} */
                                    mode: "payment" | "subscription";
                                    name: string;
                                    /** @enum {string} */
                                    pricing_model: "fixed" | "components";
                                    product_id: string;
                                    recurring_interval?: string | null;
                                    stripe_price_id: string;
                                    stripe_product_id: string;
                                    tax_behavior: string;
                                    usage_type: string;
                                    variables: Record<string, never>[];
                                    version: number;
                                };
                                /** @enum {string} */
                                status: "draft" | "active" | "archived";
                                sync_error: string;
                                sync_status: string;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create product offer */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @enum {string} */
                        billing_scheme: "per_unit" | "tiered";
                        checkout?: Record<string, never>;
                        components: Record<string, never>[];
                        currency: string;
                        /** @default 1 */
                        interval_count?: number;
                        /** @enum {string} */
                        mode: "payment" | "subscription";
                        name: string;
                        /** @enum {string} */
                        pricing_model: "fixed" | "components";
                        /** @enum {string|null} */
                        recurring_interval?: "day" | "week" | "month" | "year" | null;
                        /** @enum {string} */
                        tax_behavior: "unspecified" | "inclusive" | "exclusive";
                        /** @enum {string} */
                        usage_type: "licensed" | "metered";
                        variables?: Record<string, never>[];
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/products/{product_id}/offers/{offer_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get product offer */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        /** Archive offer */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        /** Update draft offer */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @enum {string} */
                        billing_scheme: "per_unit" | "tiered";
                        checkout?: Record<string, never>;
                        components: Record<string, never>[];
                        currency: string;
                        /** @default 1 */
                        interval_count?: number;
                        /** @enum {string} */
                        mode: "payment" | "subscription";
                        name: string;
                        /** @enum {string} */
                        pricing_model: "fixed" | "components";
                        /** @enum {string|null} */
                        recurring_interval?: "day" | "week" | "month" | "year" | null;
                        /** @enum {string} */
                        tax_behavior: "unspecified" | "inclusive" | "exclusive";
                        /** @enum {string} */
                        usage_type: "licensed" | "metered";
                        variables?: Record<string, never>[];
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/products/api/admin/products/{product_id}/offers/{offer_id}/duplicate": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Duplicate offer */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/products/{product_id}/offers/{offer_id}/payment-links": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List Payment Links */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            payment_links: {
                                active: boolean;
                                configuration_hash: string;
                                id: string;
                                offer_id: string;
                                /** @default  */
                                preset_id: string;
                                /** @default  */
                                sync_error: string;
                                sync_status: string;
                                /**
                                 * Format: uri
                                 * @description Stripe-hosted Payment Link the buyer opens.
                                 */
                                url: string;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create or reuse Payment Link */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * Format: uri
                         * @description Where Stripe sends the buyer once the Payment Link is paid.
                         * @default null
                         */
                        after_completion_url?: string | null;
                        /** @default null */
                        preset_id?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            active: boolean;
                            configuration_hash: string;
                            id: string;
                            offer_id: string;
                            /** @default  */
                            preset_id: string;
                            /** @default  */
                            sync_error: string;
                            sync_status: string;
                            /**
                             * Format: uri
                             * @description Stripe-hosted Payment Link the buyer opens.
                             */
                            url: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/products/{product_id}/offers/{offer_id}/payment-links/{link_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Deactivate Payment Link */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    link_id: string;
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            active: boolean;
                            configuration_hash: string;
                            id: string;
                            offer_id: string;
                            /** @default  */
                            preset_id: string;
                            /** @default  */
                            sync_error: string;
                            sync_status: string;
                            /**
                             * Format: uri
                             * @description Stripe-hosted Payment Link the buyer opens.
                             */
                            url: string;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List checkout presets */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            presets: {
                                active: boolean;
                                configuration_hash: string;
                                id: string;
                                inputs: {
                                    [key: string]: unknown;
                                };
                                name: string;
                                offer_id: string;
                                slug: string;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create checkout preset */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @default {} */
                        inputs?: {
                            [key: string]: unknown;
                        };
                        name: string;
                        /** @default  */
                        slug?: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            active: boolean;
                            configuration_hash: string;
                            id: string;
                            inputs: {
                                [key: string]: unknown;
                            };
                            name: string;
                            offer_id: string;
                            slug: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/products/{product_id}/offers/{offer_id}/presets/{preset_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get checkout preset */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    preset_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            active: boolean;
                            configuration_hash: string;
                            id: string;
                            inputs: {
                                [key: string]: unknown;
                            };
                            name: string;
                            offer_id: string;
                            slug: string;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        /** Archive checkout preset */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    preset_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            active: boolean;
                            configuration_hash: string;
                            id: string;
                            inputs: {
                                [key: string]: unknown;
                            };
                            name: string;
                            offer_id: string;
                            slug: string;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        /** Update checkout preset */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    preset_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @default {} */
                        inputs?: {
                            [key: string]: unknown;
                        };
                        name: string;
                        /** @default  */
                        slug?: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            active: boolean;
                            configuration_hash: string;
                            id: string;
                            inputs: {
                                [key: string]: unknown;
                            };
                            name: string;
                            offer_id: string;
                            slug: string;
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/products/api/admin/products/{product_id}/offers/{offer_id}/preview": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Preview draft or active product offer
         * @description Evaluate an owner-visible immutable or draft offer with the server pricing engine. Browser totals are never trusted.
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @default {} */
                        inputs?: {
                            [key: string]: unknown;
                        };
                        offer_id: string;
                        /**
                         * Format: uint64
                         * @default 1
                         */
                        quantity?: number;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            amounts: {
                                currency: string;
                                /** Format: int64 */
                                discount_minor: number;
                                /** Format: int64 */
                                platform_fee_minor: number;
                                /**
                                 * Format: int64
                                 * @default 0
                                 */
                                shipping_minor: number;
                                /** Format: int64 */
                                subtotal_minor: number;
                                /** Format: int64 */
                                tax_minor: number;
                                /** Format: int64 */
                                total_minor: number;
                            };
                            components: {
                                component_id: string;
                                included: boolean;
                                key: string;
                                label: string;
                                /** Format: uint64 */
                                quantity: number;
                                reason: string;
                                required: boolean;
                                /** Format: int64 */
                                total_amount_minor: number;
                                /** Format: int64 */
                                unit_amount_minor: number;
                            }[];
                            inputs: {
                                [key: string]: unknown;
                            };
                            offer_id: string;
                            /** Format: uint32 */
                            offer_version: number;
                            /** Format: uint64 */
                            quantity: number;
                            /** Format: uint32 */
                            schema_version: number;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/products/{product_id}/offers/{offer_id}/publish": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Publish offer */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/products/{product_id}/offers/{offer_id}/sync": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Synchronize immutable Product and fixed Prices to Stripe */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/provider-operations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List safe Stripe provider reconciliation state */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                    status?: "pending" | "processing" | "failed" | "succeeded" | "dead_letter";
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** Format: int64 */
                            page: number;
                            /** Format: int64 */
                            page_size: number;
                            records: {
                                aggregate_id: string;
                                /**
                                 * @description Currently only `refund`.
                                 * @enum {string}
                                 */
                                aggregate_type: "refund";
                                /** Format: uint64 */
                                attempts: number;
                                /** Format: date-time */
                                completed_at?: string;
                                /** Format: date-time */
                                created_at: string;
                                id: string;
                                /** @default  */
                                last_error: string;
                                /** Format: date-time */
                                next_attempt_at?: string;
                                /**
                                 * @description Currently only `refund.reconcile`.
                                 * @enum {string}
                                 */
                                operation_type: "refund.reconcile";
                                /** Format: date-time */
                                processing_started_at?: string;
                                /**
                                 * @description One of `pending`, `processing`, `failed`, `succeeded`, `dead_letter`.
                                 * @enum {string}
                                 */
                                status: "pending" | "processing" | "failed" | "succeeded" | "dead_letter";
                                /** @default  */
                                stripe_account_id: string;
                                /** Format: date-time */
                                terminal_at?: string;
                                /** Format: date-time */
                                updated_at: string;
                            }[];
                            /** Format: int64 */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/provider-operations/reconcile": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Claim and reconcile due Stripe provider operations
         * @description Safe for an authenticated scheduler or manual administrator recovery action; leases and original Stripe idempotency keys prevent duplicate mutations.
         */
        post: {
            parameters: {
                query?: {
                    limit?: number;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** Format: uint64 */
                            claimed: number;
                            /** Format: uint64 */
                            dead_letter: number;
                            /** Format: uint64 */
                            retry_scheduled: number;
                            /** Format: uint64 */
                            succeeded: number;
                            /**
                             * Format: uint64
                             * @description Operations whose state could not be written; a later run retries them.
                             */
                            unrecorded: number;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/purchases": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List purchases */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                    status?: string | null;
                    user_id?: string | null;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /** @description Orders on this page, newest first. */
                            records: {
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the order was approved, or `null`.
                                 */
                                approved_at: string | null;
                                /** @description Buyer's email address as captured at checkout, or empty. */
                                buyer_email: string;
                                /**
                                 * @description Signed-in buyer's user id, or empty for a guest order. The order's
                                 *     single buyer identity.
                                 */
                                buyer_user_id: string;
                                /**
                                 * @description Checkout presentation the order was started with.
                                 * @enum {string}
                                 */
                                checkout_mode: "hosted" | "embedded" | "payment_link";
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description ISO 4217 currency of every amount on the order. */
                                currency: string;
                                /** Format: int64 */
                                discount_cents: number;
                                /** @description Stable order identifier. */
                                id: string;
                                /** @description Whether the order was placed against the live Stripe environment. */
                                livemode: boolean;
                                /**
                                 * @description Immutable checkout snapshot: the offer id and version the order was
                                 *     priced against and the shipping amounts allowed at checkout.
                                 */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp payment was recorded, or `null`.
                                 */
                                payment_at: string | null;
                                /**
                                 * Format: int64
                                 * @description Provider timestamp (Unix seconds) of the PaymentIntent event that
                                 *     last updated the payment state; `0` until one arrives.
                                 */
                                payment_intent_event_created: number;
                                /** Format: int64 */
                                platform_fee_cents: number;
                                /**
                                 * @description Payment provider: `stripe`, or `manual` for orders recorded outside a
                                 *     provider.
                                 */
                                provider: string;
                                provider_payment_error_code: string;
                                provider_payment_error_message: string;
                                /** @description PaymentIntent id as recorded from the provider event stream, or empty. */
                                provider_payment_intent_id: string;
                                /**
                                 * @description Latest PaymentIntent state received from the provider.
                                 * @enum {string}
                                 */
                                provider_payment_status: "" | "succeeded" | "payment_failed" | "processing" | "requires_action" | "canceled";
                                /** @description Stripe Checkout Session id, or empty. */
                                provider_session_id: string;
                                reconciliation_error: string;
                                /**
                                 * @description Where the order stands against the provider's view of it. `pending`:
                                 *     no provider session yet; `awaiting_payment`: a Checkout Session exists;
                                 *     `reconciled`: the completed session matched the local snapshot;
                                 *     `provider_error`: the provider's answer was unusable or contradicted
                                 *     it (`reconciliation_error` says why); the `payment_*` values mirror
                                 *     the last PaymentIntent event received before Checkout completion.
                                 * @enum {string}
                                 */
                                reconciliation_status: "pending" | "awaiting_payment" | "reconciled" | "provider_error" | "payment_succeeded_awaiting_checkout" | "payment_failed" | "payment_processing" | "payment_requires_action" | "payment_canceled";
                                /** @description Operator note attached to the last refund, or empty. */
                                refund_reason: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last refund applied, or `null`.
                                 */
                                refunded_at: string | null;
                                /** @description User id of the operator who issued the last refund, or empty. */
                                refunded_by: string;
                                /**
                                 * Format: int64
                                 * @description Sum of succeeded refunds in minor units.
                                 */
                                refunded_total_cents: number;
                                /**
                                 * @description Seller account the order was placed against; empty for platform
                                 *     products.
                                 */
                                seller_account_id: string;
                                /** Format: int64 */
                                shipping_cents: number;
                                /**
                                 * @description Lifecycle state of the order. `pending`: created, checkout not yet
                                 *     claimed; `checkout_started`: a provider Checkout Session was claimed;
                                 *     `completed`: paid; `partially_refunded` / `refunded`: paid, then
                                 *     refunded in part or in full; `failed`: checkout or reconciliation
                                 *     failed (`reconciliation_error` says why).
                                 * @enum {string}
                                 */
                                status: "pending" | "checkout_started" | "completed" | "partially_refunded" | "refunded" | "failed";
                                /**
                                 * @description Stripe connected account the order was charged through; empty for
                                 *     platform products.
                                 */
                                stripe_account_id: string;
                                /** @description Stripe Customer id, or empty. */
                                stripe_customer_id: string;
                                /** @description Stripe PaymentIntent id, or empty. */
                                stripe_payment_intent_id: string;
                                /** @description Stripe Subscription id for subscription orders, or empty. */
                                stripe_subscription_id: string;
                                subscription_cancel_at_period_end: boolean;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the subscription was canceled, or `null`.
                                 */
                                subscription_canceled_at: string | null;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 end of the current billing period, or `null`.
                                 */
                                subscription_current_period_end: string | null;
                                /**
                                 * Format: int64
                                 * @description Provider timestamp (Unix seconds) of the subscription event that last
                                 *     updated the subscription state; `0` until one arrives.
                                 */
                                subscription_event_created: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the subscription state was last synchronized from
                                 *     the provider, or `null`.
                                 */
                                subscription_last_synced_at: string | null;
                                /**
                                 * @description Stripe subscription lifecycle state for subscription orders, or
                                 *     empty.
                                 * @enum {string}
                                 */
                                subscription_status: "" | "incomplete" | "incomplete_expired" | "trialing" | "active" | "past_due" | "unpaid" | "paused" | "canceled";
                                /** Format: int64 */
                                subtotal_cents: number;
                                /** Format: int64 */
                                tax_cents: number;
                                /**
                                 * Format: int64
                                 * @description Final charged amount in minor units. The order's single amount.
                                 */
                                total_cents: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total orders matching the filters, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/purchases/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get purchase */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Disputes, newest first. */
                            disputes: {
                                /** Format: int64 */
                                amount_minor: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the dispute closed, or `null`.
                                 */
                                closed_at: string | null;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                currency: string;
                                /**
                                 * Format: int64
                                 * @description Provider timestamp (Unix seconds) of the event that last updated the
                                 *     dispute.
                                 */
                                event_created: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 evidence deadline, or `null`.
                                 */
                                evidence_due_by: string | null;
                                /** @description Stable dispute identifier. */
                                id: string;
                                livemode: boolean;
                                payment_intent_id: string;
                                /** @description Stripe Charge id the dispute was raised against, or empty. */
                                provider_charge_id: string;
                                /** @description Stripe Dispute id. */
                                provider_dispute_id: string;
                                purchase_id: string;
                                /** @description Provider's dispute reason, or empty. */
                                reason: string;
                                seller_account_id: string;
                                /**
                                 * @description Where the dispute stands with the card network.
                                 * @enum {string}
                                 */
                                status: "warning_needs_response" | "warning_under_review" | "warning_closed" | "needs_response" | "under_review" | "won" | "lost" | "prevented";
                                stripe_account_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            line_items: {
                                /** @description Offer component the line resolved, or empty for a whole-offer line. */
                                component_id: string;
                                /** @description The component condition as it was evaluated at checkout. */
                                condition_snapshot: {
                                    [key: string]: unknown;
                                };
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** Format: int64 */
                                discount_minor: number;
                                /** @description Stable line identifier. */
                                id: string;
                                /** @description Customer inputs the line was priced with, as submitted at checkout. */
                                input_snapshot: {
                                    [key: string]: unknown;
                                };
                                /** @description Offer the line was priced from, or empty for legacy lines. */
                                offer_id: string;
                                /**
                                 * Format: int64
                                 * @description Version of that offer at checkout.
                                 */
                                offer_version: number;
                                product_id: string;
                                /** @description Product name as it was at checkout. */
                                product_name: string;
                                purchase_id: string;
                                /** Format: int64 */
                                quantity: number;
                                seller_account_id: string;
                                /** @description Stripe Price id the line was charged through, or empty. */
                                stripe_price_id: string;
                                /** Format: int64 */
                                subtotal_minor: number;
                                /** Format: int64 */
                                tax_minor: number;
                                /** Format: int64 */
                                total_minor: number;
                                /** Format: int64 */
                                unit_amount_minor: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * @description An order row: `impresspress__products__purchases`, as published to the
                             *     buyer, the seller and administrators.
                             */
                            purchase: {
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the order was approved, or `null`.
                                 */
                                approved_at: string | null;
                                /** @description Buyer's email address as captured at checkout, or empty. */
                                buyer_email: string;
                                /**
                                 * @description Signed-in buyer's user id, or empty for a guest order. The order's
                                 *     single buyer identity.
                                 */
                                buyer_user_id: string;
                                /**
                                 * @description Checkout presentation the order was started with.
                                 * @enum {string}
                                 */
                                checkout_mode: "hosted" | "embedded" | "payment_link";
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description ISO 4217 currency of every amount on the order. */
                                currency: string;
                                /** Format: int64 */
                                discount_cents: number;
                                /** @description Stable order identifier. */
                                id: string;
                                /** @description Whether the order was placed against the live Stripe environment. */
                                livemode: boolean;
                                /**
                                 * @description Immutable checkout snapshot: the offer id and version the order was
                                 *     priced against and the shipping amounts allowed at checkout.
                                 */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp payment was recorded, or `null`.
                                 */
                                payment_at: string | null;
                                /**
                                 * Format: int64
                                 * @description Provider timestamp (Unix seconds) of the PaymentIntent event that
                                 *     last updated the payment state; `0` until one arrives.
                                 */
                                payment_intent_event_created: number;
                                /** Format: int64 */
                                platform_fee_cents: number;
                                /**
                                 * @description Payment provider: `stripe`, or `manual` for orders recorded outside a
                                 *     provider.
                                 */
                                provider: string;
                                provider_payment_error_code: string;
                                provider_payment_error_message: string;
                                /** @description PaymentIntent id as recorded from the provider event stream, or empty. */
                                provider_payment_intent_id: string;
                                /**
                                 * @description Latest PaymentIntent state received from the provider.
                                 * @enum {string}
                                 */
                                provider_payment_status: "" | "succeeded" | "payment_failed" | "processing" | "requires_action" | "canceled";
                                /** @description Stripe Checkout Session id, or empty. */
                                provider_session_id: string;
                                reconciliation_error: string;
                                /**
                                 * @description Where the order stands against the provider's view of it. `pending`:
                                 *     no provider session yet; `awaiting_payment`: a Checkout Session exists;
                                 *     `reconciled`: the completed session matched the local snapshot;
                                 *     `provider_error`: the provider's answer was unusable or contradicted
                                 *     it (`reconciliation_error` says why); the `payment_*` values mirror
                                 *     the last PaymentIntent event received before Checkout completion.
                                 * @enum {string}
                                 */
                                reconciliation_status: "pending" | "awaiting_payment" | "reconciled" | "provider_error" | "payment_succeeded_awaiting_checkout" | "payment_failed" | "payment_processing" | "payment_requires_action" | "payment_canceled";
                                /** @description Operator note attached to the last refund, or empty. */
                                refund_reason: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last refund applied, or `null`.
                                 */
                                refunded_at: string | null;
                                /** @description User id of the operator who issued the last refund, or empty. */
                                refunded_by: string;
                                /**
                                 * Format: int64
                                 * @description Sum of succeeded refunds in minor units.
                                 */
                                refunded_total_cents: number;
                                /**
                                 * @description Seller account the order was placed against; empty for platform
                                 *     products.
                                 */
                                seller_account_id: string;
                                /** Format: int64 */
                                shipping_cents: number;
                                /**
                                 * @description Lifecycle state of the order. `pending`: created, checkout not yet
                                 *     claimed; `checkout_started`: a provider Checkout Session was claimed;
                                 *     `completed`: paid; `partially_refunded` / `refunded`: paid, then
                                 *     refunded in part or in full; `failed`: checkout or reconciliation
                                 *     failed (`reconciliation_error` says why).
                                 * @enum {string}
                                 */
                                status: "pending" | "checkout_started" | "completed" | "partially_refunded" | "refunded" | "failed";
                                /**
                                 * @description Stripe connected account the order was charged through; empty for
                                 *     platform products.
                                 */
                                stripe_account_id: string;
                                /** @description Stripe Customer id, or empty. */
                                stripe_customer_id: string;
                                /** @description Stripe PaymentIntent id, or empty. */
                                stripe_payment_intent_id: string;
                                /** @description Stripe Subscription id for subscription orders, or empty. */
                                stripe_subscription_id: string;
                                subscription_cancel_at_period_end: boolean;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the subscription was canceled, or `null`.
                                 */
                                subscription_canceled_at: string | null;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 end of the current billing period, or `null`.
                                 */
                                subscription_current_period_end: string | null;
                                /**
                                 * Format: int64
                                 * @description Provider timestamp (Unix seconds) of the subscription event that last
                                 *     updated the subscription state; `0` until one arrives.
                                 */
                                subscription_event_created: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the subscription state was last synchronized from
                                 *     the provider, or `null`.
                                 */
                                subscription_last_synced_at: string | null;
                                /**
                                 * @description Stripe subscription lifecycle state for subscription orders, or
                                 *     empty.
                                 * @enum {string}
                                 */
                                subscription_status: "" | "incomplete" | "incomplete_expired" | "trialing" | "active" | "past_due" | "unpaid" | "paused" | "canceled";
                                /** Format: int64 */
                                subtotal_cents: number;
                                /** Format: int64 */
                                tax_cents: number;
                                /**
                                 * Format: int64
                                 * @description Final charged amount in minor units. The order's single amount.
                                 */
                                total_cents: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            };
                            /** @description Refunds, newest first. */
                            refunds: {
                                /** Format: int64 */
                                amount_minor: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the refund reached a terminal state, or `null`.
                                 */
                                completed_at: string | null;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                currency: string;
                                /** @description Stable refund identifier. */
                                id: string;
                                last_error: string;
                                livemode: boolean;
                                /** @description Operator note kept on the platform, never sent to the provider. */
                                note: string;
                                /**
                                 * @description PaymentIntent the refund was issued against; empty for manual
                                 *     refunds.
                                 */
                                payment_intent_id: string;
                                /** @description Reason sent to the provider, or empty. */
                                provider_reason: string;
                                /** @description Stripe Refund id once the provider accepted it, or empty. */
                                provider_refund_id: string;
                                /**
                                 * @description The provider's own state for the refund (`pending`, `requires_action`,
                                 *     `succeeded`, `failed` or `canceled`). Empty until the provider answers;
                                 *     `succeeded` for a refund recorded without a provider, which the ledger
                                 *     completes itself.
                                 */
                                provider_status: string;
                                purchase_id: string;
                                /** @description User id of the operator who issued the refund. */
                                refunded_by: string;
                                /**
                                 * @description Ledger state.
                                 * @enum {string}
                                 */
                                status: "pending" | "provider_succeeded" | "succeeded" | "failed";
                                /** @description Stripe connected account the refund was issued through, or empty. */
                                stripe_account_id: string;
                                /**
                                 * Format: int64
                                 * @description Provider timestamp (Unix seconds) of the event that last updated the
                                 *     refund; `0` until one arrives.
                                 */
                                stripe_event_created: number;
                                /**
                                 * Format: int64
                                 * @description The order's `refunded_total_cents` once this refund succeeds.
                                 */
                                target_refunded_total_minor: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/purchases/{id}/refund": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Create an idempotent full or partial refund */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * Format: int64
                         * @description Exact amount in the order currency's minor unit. Omit to refund the
                         *     complete remaining refundable amount.
                         * @default null
                         */
                        amount_minor?: number | null;
                        /**
                         * @description Stable client operation key. Supplying a fresh key allows a deliberate
                         *     second partial refund of the same amount; retries must reuse the key.
                         * @default null
                         */
                        idempotency_key?: string | null;
                        /**
                         * @description Private operator note retained in ImpressPress, never sent to Stripe.
                         * @default null
                         */
                        note?: string | null;
                        /**
                         * @description Stripe's constrained provider reason. Human context belongs in `note`.
                         * @default null
                         * @enum {string|null}
                         */
                        provider_reason?: "duplicate" | "fraudulent" | "requested_by_customer" | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** Format: int64 */
                            amount_minor: number;
                            currency: string;
                            livemode: boolean;
                            /** Format: int64 */
                            order_total_minor: number;
                            /** @default  */
                            provider_refund_id: string;
                            /** @default  */
                            provider_status: string;
                            purchase_id: string;
                            /** @default  */
                            refund_id: string;
                            /** Format: int64 */
                            refunded_total_minor: number;
                            /** @enum {string} */
                            status: "pending" | "succeeded" | "failed";
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/sellers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List seller accounts and capability state */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            sellers: {
                                /**
                                 * @description Whether an administrator has suspended the account. Derived from
                                 *     `status`; the two move together.
                                 * @enum {string}
                                 */
                                approval_status: "approved" | "suspended";
                                capabilities: {
                                    charges_enabled: boolean;
                                    details_submitted: boolean;
                                    payouts_enabled: boolean;
                                    /** @default [] */
                                    requirements_due: string[];
                                };
                                /** @default  */
                                country: string;
                                /** @default  */
                                dashboard_type: string;
                                /** @default  */
                                default_currency: string;
                                /** @default  */
                                disabled_reason: string;
                                /**
                                 * Format: uint32
                                 * @description The platform application fee new Checkout Sessions and Payment Links carry, in basis points.
                                 */
                                fee_basis_points: number;
                                id: string;
                                /** @default  */
                                last_synced_at: string;
                                /** @default false */
                                livemode: boolean;
                                /**
                                 * @description How far the account has got with Stripe Connect.
                                 * @enum {string}
                                 */
                                status: "not_started" | "onboarding" | "restricted" | "active" | "suspended";
                                /** @default  */
                                stripe_account_id: string;
                                /** @default  */
                                sync_error: string;
                                user_id: string;
                            }[];
                            /**
                             * Format: int64
                             * @description How many seller accounts exist, which is not `sellers.len()` when the
                             *     listing is showing a prefix.
                             */
                            total_count: number;
                            /** @description Whether more seller accounts exist than `sellers` lists. */
                            truncated: boolean;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/sellers/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get seller account and owned products */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description The seller's live catalog — products the seller's user owns that have
                             *     not been soft-deleted, in any publication state.
                             */
                            products: {
                                /**
                                 * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                                 *     `rejected` or `suspended`. A different column and a different
                                 *     vocabulary from `status` — a listing awaiting review is
                                 *     `status = pending_review` and `approval_status = pending` at once.
                                 * @enum {string}
                                 */
                                approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                                category: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description Id of the user who created the row. */
                                created_by: string;
                                /** @description ISO 4217 presentment currency. */
                                currency: string;
                                /**
                                 * Format: int64
                                 * @description Version counter of the product's immutable offer definitions.
                                 */
                                current_version: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                                 *     soft-deleted.
                                 */
                                deleted_at: string | null;
                                description: string;
                                /**
                                 * @description How a purchase is fulfilled.
                                 * @enum {string}
                                 */
                                fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                                /** @description Group the product is listed under, or empty. */
                                group_id: string;
                                group_template_id: string;
                                /** @description Stable product identifier. */
                                id: string;
                                image_url: string;
                                /** @description Free-form key/value metadata attached by the product builder. */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                name: string;
                                /** @description Owning seller's user id; empty for platform products. */
                                owner_id: string;
                                /**
                                 * @description `platform` for an administrator-owned product, `user` for a seller's.
                                 * @enum {string}
                                 */
                                owner_kind: "platform" | "user";
                                /** @description Product builder template this product was created from. */
                                product_template_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the product last became active, or `null`.
                                 */
                                published_at: string | null;
                                /** @description Id of a product the buyer must already own before checkout, or empty. */
                                requires: string;
                                /** @description Seller account the product sells through; empty for platform products. */
                                seller_account_id: string;
                                /**
                                 * @description URL slug, unique per owner among non-deleted products. Empty when the
                                 *     product has none.
                                 */
                                slug: string;
                                /**
                                 * @description Publication state: `draft`, `pending_review` (seller product awaiting
                                 *     moderation), `active` (in the public catalog) or `archived`.
                                 * @enum {string}
                                 */
                                status: "draft" | "pending_review" | "active" | "archived";
                                /**
                                 * Format: int64
                                 * @description Units in stock; `0` when inventory is not tracked.
                                 */
                                stock: number;
                                /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                                stripe_product_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the seller last submitted the product for
                                 *     moderation, or `null`.
                                 */
                                submitted_at: string | null;
                                tags: string[];
                                /** @description Product type (taxonomy) id, or empty. */
                                type_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            seller: {
                                /**
                                 * @description Whether an administrator has suspended the account. Derived from
                                 *     `status`; the two move together.
                                 * @enum {string}
                                 */
                                approval_status: "approved" | "suspended";
                                capabilities: {
                                    charges_enabled: boolean;
                                    details_submitted: boolean;
                                    payouts_enabled: boolean;
                                    /** @default [] */
                                    requirements_due: string[];
                                };
                                /** @default  */
                                country: string;
                                /** @default  */
                                dashboard_type: string;
                                /** @default  */
                                default_currency: string;
                                /** @default  */
                                disabled_reason: string;
                                /**
                                 * Format: uint32
                                 * @description The platform application fee new Checkout Sessions and Payment Links carry, in basis points.
                                 */
                                fee_basis_points: number;
                                id: string;
                                /** @default  */
                                last_synced_at: string;
                                /** @default false */
                                livemode: boolean;
                                /**
                                 * @description How far the account has got with Stripe Connect.
                                 * @enum {string}
                                 */
                                status: "not_started" | "onboarding" | "restricted" | "active" | "suspended";
                                /** @default  */
                                stripe_account_id: string;
                                /** @default  */
                                sync_error: string;
                                user_id: string;
                            };
                            /**
                             * @description Whether the seller owns more live products than `products` lists.
                             *
                             *     Nothing caps a seller's catalog, so the listing is read up to a
                             *     ceiling; this is how a client tells a complete catalog from a prefix
                             *     of one.
                             */
                            truncated: boolean;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/sellers/{id}/reactivate": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Reactivate a seller for onboarding or sales */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Whether an administrator has suspended the account. Derived from
                             *     `status`; the two move together.
                             * @enum {string}
                             */
                            approval_status: "approved" | "suspended";
                            capabilities: {
                                charges_enabled: boolean;
                                details_submitted: boolean;
                                payouts_enabled: boolean;
                                /** @default [] */
                                requirements_due: string[];
                            };
                            /** @default  */
                            country: string;
                            /** @default  */
                            dashboard_type: string;
                            /** @default  */
                            default_currency: string;
                            /** @default  */
                            disabled_reason: string;
                            /**
                             * Format: uint32
                             * @description The platform application fee new Checkout Sessions and Payment Links carry, in basis points.
                             */
                            fee_basis_points: number;
                            id: string;
                            /** @default  */
                            last_synced_at: string;
                            /** @default false */
                            livemode: boolean;
                            /**
                             * @description How far the account has got with Stripe Connect.
                             * @enum {string}
                             */
                            status: "not_started" | "onboarding" | "restricted" | "active" | "suspended";
                            /** @default  */
                            stripe_account_id: string;
                            /** @default  */
                            sync_error: string;
                            user_id: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/sellers/{id}/suspend": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Suspend a seller after provider-safe offer archival */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Whether an administrator has suspended the account. Derived from
                             *     `status`; the two move together.
                             * @enum {string}
                             */
                            approval_status: "approved" | "suspended";
                            capabilities: {
                                charges_enabled: boolean;
                                details_submitted: boolean;
                                payouts_enabled: boolean;
                                /** @default [] */
                                requirements_due: string[];
                            };
                            /** @default  */
                            country: string;
                            /** @default  */
                            dashboard_type: string;
                            /** @default  */
                            default_currency: string;
                            /** @default  */
                            disabled_reason: string;
                            /**
                             * Format: uint32
                             * @description The platform application fee new Checkout Sessions and Payment Links carry, in basis points.
                             */
                            fee_basis_points: number;
                            id: string;
                            /** @default  */
                            last_synced_at: string;
                            /** @default false */
                            livemode: boolean;
                            /**
                             * @description How far the account has got with Stripe Connect.
                             * @enum {string}
                             */
                            status: "not_started" | "onboarding" | "restricted" | "active" | "suspended";
                            /** @default  */
                            stripe_account_id: string;
                            /** @default  */
                            sync_error: string;
                            user_id: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/stats": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Commerce analytics separated by currency */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** Format: int64 */
                            active_products: number;
                            /** @description One entry per currency the platform has transacted in. */
                            currency_analytics: {
                                /** Format: uint64 */
                                active_subscription_count: number;
                                /** Format: uint64 */
                                canceled_subscription_count: number;
                                currency: string;
                                /** Format: uint64 */
                                failed_order_count: number;
                                /** Format: int64 */
                                gross_volume_minor: number;
                                /** Format: uint64 */
                                lost_dispute_count: number;
                                /** Format: int64 */
                                lost_disputed_volume_minor: number;
                                /** Format: int64 */
                                net_volume_minor: number;
                                /** Format: uint64 */
                                open_dispute_count: number;
                                /** Format: int64 */
                                open_disputed_volume_minor: number;
                                /** Format: uint64 */
                                order_count: number;
                                /** Format: uint64 */
                                paid_order_count: number;
                                /** Format: uint64 */
                                past_due_subscription_count: number;
                                /** Format: int64 */
                                platform_fees_minor: number;
                                /** Format: uint64 */
                                refunded_order_count: number;
                                /** Format: int64 */
                                refunded_volume_minor: number;
                                top_products: {
                                    name: string;
                                    product_id: string;
                                    /** Format: uint64 */
                                    quantity: number;
                                    /** Format: int64 */
                                    revenue_minor: number;
                                }[];
                                /** Format: uint64 */
                                trialing_subscription_count: number;
                            }[];
                            /** Format: int64 */
                            total_groups: number;
                            /** Format: int64 */
                            total_products: number;
                            /** Format: int64 */
                            total_purchases: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/stripe/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Validate Stripe connection and account mode */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @default  */
                            account_id: string;
                            api_version: string;
                            /** @default  */
                            business_name: string;
                            /** @default {} */
                            capabilities: {
                                [key: string]: string;
                            };
                            charges_enabled: boolean;
                            configured: boolean;
                            /** @default  */
                            country: string;
                            /** @default  */
                            default_currency: string;
                            details_submitted: boolean;
                            /** @default  */
                            error: string;
                            livemode: boolean;
                            payouts_enabled: boolean;
                            publishable_key_configured: boolean;
                            /** @enum {string} */
                            state: "not_configured" | "connected_test" | "connected_live" | "misconfigured";
                            webhook_secret_configured: boolean;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/types": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List types */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /** @description Types on this page, newest first. */
                            records: {
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                description: string;
                                /** @description Stable type identifier. */
                                id: string;
                                /**
                                 * @description Whether the type is built in. System types are seeded by the block
                                 *     rather than created through the API.
                                 */
                                is_system: boolean;
                                name: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total types, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create type */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        description?: string | null;
                        /** @description Whether the type is built in. Defaults to `false`. */
                        is_system?: boolean | null;
                        name: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            description: string;
                            /** @description Stable type identifier. */
                            id: string;
                            /**
                             * @description Whether the type is built in. System types are seeded by the block
                             *     rather than created through the API.
                             */
                            is_system: boolean;
                            name: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/types/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Delete type */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Always `true`: a delete that did not happen is an error response. */
                            deleted: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/webhook-events": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List safe Stripe webhook processing state */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                    status?: "pending" | "processing" | "failed" | "processed" | "dead_letter";
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** Format: int64 */
                            page: number;
                            /** Format: int64 */
                            page_size: number;
                            records: {
                                /** Format: uint64 */
                                attempts: number;
                                /** Format: date-time */
                                created_at: string;
                                event_type: string;
                                id: string;
                                /** @default  */
                                last_error: string;
                                livemode: boolean;
                                /** Format: date-time */
                                next_retry_at?: string;
                                /** Format: date-time */
                                processed_at?: string;
                                /** Format: date-time */
                                processing_started_at?: string;
                                /**
                                 * @description One of `pending`, `processing`, `failed`, `processed`, `dead_letter`.
                                 * @enum {string}
                                 */
                                status: "pending" | "processing" | "failed" | "processed" | "dead_letter";
                                /** @default  */
                                stripe_account_id: string;
                                /** Format: date-time */
                                terminal_at?: string;
                                /** Format: date-time */
                                updated_at: string;
                            }[];
                            /** Format: int64 */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/admin/webhook-events/{id}/replay": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Replay a failed or dead-letter Stripe webhook */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Present only when the event exhausted its retry budget. */
                            dead_letter?: boolean;
                            /**
                             * @description Present only when the event id had already been recorded, in which
                             *     case no side effect ran.
                             */
                            duplicate?: boolean;
                            received: boolean;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/products": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List own products */
        get: {
            parameters: {
                query?: {
                    group_id?: string | null;
                    page?: number;
                    page_size?: number;
                    search?: string | null;
                    status?: string | null;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /** @description Products on this page, newest first. */
                            records: {
                                /**
                                 * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                                 *     `rejected` or `suspended`. A different column and a different
                                 *     vocabulary from `status` — a listing awaiting review is
                                 *     `status = pending_review` and `approval_status = pending` at once.
                                 * @enum {string}
                                 */
                                approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                                category: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description Id of the user who created the row. */
                                created_by: string;
                                /** @description ISO 4217 presentment currency. */
                                currency: string;
                                /**
                                 * Format: int64
                                 * @description Version counter of the product's immutable offer definitions.
                                 */
                                current_version: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                                 *     soft-deleted.
                                 */
                                deleted_at: string | null;
                                description: string;
                                /**
                                 * @description How a purchase is fulfilled.
                                 * @enum {string}
                                 */
                                fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                                /** @description Group the product is listed under, or empty. */
                                group_id: string;
                                group_template_id: string;
                                /** @description Stable product identifier. */
                                id: string;
                                image_url: string;
                                /** @description Free-form key/value metadata attached by the product builder. */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                name: string;
                                /** @description Owning seller's user id; empty for platform products. */
                                owner_id: string;
                                /**
                                 * @description `platform` for an administrator-owned product, `user` for a seller's.
                                 * @enum {string}
                                 */
                                owner_kind: "platform" | "user";
                                /** @description Product builder template this product was created from. */
                                product_template_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the product last became active, or `null`.
                                 */
                                published_at: string | null;
                                /** @description Id of a product the buyer must already own before checkout, or empty. */
                                requires: string;
                                /** @description Seller account the product sells through; empty for platform products. */
                                seller_account_id: string;
                                /**
                                 * @description URL slug, unique per owner among non-deleted products. Empty when the
                                 *     product has none.
                                 */
                                slug: string;
                                /**
                                 * @description Publication state: `draft`, `pending_review` (seller product awaiting
                                 *     moderation), `active` (in the public catalog) or `archived`.
                                 * @enum {string}
                                 */
                                status: "draft" | "pending_review" | "active" | "archived";
                                /**
                                 * Format: int64
                                 * @description Units in stock; `0` when inventory is not tracked.
                                 */
                                stock: number;
                                /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                                stripe_product_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the seller last submitted the product for
                                 *     moderation, or `null`.
                                 */
                                submitted_at: string | null;
                                tags: string[];
                                /** @description Product type (taxonomy) id, or empty. */
                                type_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total products matching the filters, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create own product */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        category?: string | null;
                        /**
                         * @description ISO 4217 currency. A seller product defaults to the platform default
                         *     currency when omitted.
                         */
                        currency?: string | null;
                        description?: string | null;
                        /** @enum {string|null} */
                        fulfillment_kind?: "none" | "manual" | "download" | "entitlement" | "webhook" | null;
                        /**
                         * @description Group to list the product under. A seller may only use a group they
                         *     own.
                         */
                        group_id?: string | null;
                        group_template_id?: string | null;
                        image_url?: string | null;
                        /** @description Free-form key/value metadata. */
                        metadata?: {
                            [key: string]: unknown;
                        } | null;
                        name: string;
                        /**
                         * @description Product builder template. A seller product defaults to the seeded
                         *     `default` template when omitted.
                         */
                        product_template_id?: string | null;
                        /** @description Id of a product the buyer must already own before checkout. */
                        requires?: string | null;
                        /**
                         * @description Web address of the product: lowercase letters and digits in runs
                         *     joined by single hyphens, at most 160 characters. Empty means none.
                         */
                        slug?: string | null;
                        /**
                         * @description Initial publication state. Defaults to `draft`; a seller product is
                         *     always created as `draft` regardless of this value.
                         * @enum {string|null}
                         */
                        status?: "draft" | "pending_review" | "active" | "archived" | null;
                        /** Format: int64 */
                        stock?: number | null;
                        tags?: string[] | null;
                        type_id?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                             *     `rejected` or `suspended`. A different column and a different
                             *     vocabulary from `status` — a listing awaiting review is
                             *     `status = pending_review` and `approval_status = pending` at once.
                             * @enum {string}
                             */
                            approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                            category: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /** @description Id of the user who created the row. */
                            created_by: string;
                            /** @description ISO 4217 presentment currency. */
                            currency: string;
                            /**
                             * Format: int64
                             * @description Version counter of the product's immutable offer definitions.
                             */
                            current_version: number;
                            /**
                             * Format: date-time
                             * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                             *     soft-deleted.
                             */
                            deleted_at: string | null;
                            description: string;
                            /**
                             * @description How a purchase is fulfilled.
                             * @enum {string}
                             */
                            fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                            /** @description Group the product is listed under, or empty. */
                            group_id: string;
                            group_template_id: string;
                            /** @description Stable product identifier. */
                            id: string;
                            image_url: string;
                            /** @description Free-form key/value metadata attached by the product builder. */
                            metadata: {
                                [key: string]: unknown;
                            };
                            name: string;
                            /** @description Owning seller's user id; empty for platform products. */
                            owner_id: string;
                            /**
                             * @description `platform` for an administrator-owned product, `user` for a seller's.
                             * @enum {string}
                             */
                            owner_kind: "platform" | "user";
                            /** @description Product builder template this product was created from. */
                            product_template_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the product last became active, or `null`.
                             */
                            published_at: string | null;
                            /** @description Id of a product the buyer must already own before checkout, or empty. */
                            requires: string;
                            /** @description Seller account the product sells through; empty for platform products. */
                            seller_account_id: string;
                            /**
                             * @description URL slug, unique per owner among non-deleted products. Empty when the
                             *     product has none.
                             */
                            slug: string;
                            /**
                             * @description Publication state: `draft`, `pending_review` (seller product awaiting
                             *     moderation), `active` (in the public catalog) or `archived`.
                             * @enum {string}
                             */
                            status: "draft" | "pending_review" | "active" | "archived";
                            /**
                             * Format: int64
                             * @description Units in stock; `0` when inventory is not tracked.
                             */
                            stock: number;
                            /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                            stripe_product_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the seller last submitted the product for
                             *     moderation, or `null`.
                             */
                            submitted_at: string | null;
                            tags: string[];
                            /** @description Product type (taxonomy) id, or empty. */
                            type_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/products/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get own product */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                             *     `rejected` or `suspended`. A different column and a different
                             *     vocabulary from `status` — a listing awaiting review is
                             *     `status = pending_review` and `approval_status = pending` at once.
                             * @enum {string}
                             */
                            approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                            category: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /** @description Id of the user who created the row. */
                            created_by: string;
                            /** @description ISO 4217 presentment currency. */
                            currency: string;
                            /**
                             * Format: int64
                             * @description Version counter of the product's immutable offer definitions.
                             */
                            current_version: number;
                            /**
                             * Format: date-time
                             * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                             *     soft-deleted.
                             */
                            deleted_at: string | null;
                            description: string;
                            /**
                             * @description How a purchase is fulfilled.
                             * @enum {string}
                             */
                            fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                            /** @description Group the product is listed under, or empty. */
                            group_id: string;
                            group_template_id: string;
                            /** @description Stable product identifier. */
                            id: string;
                            image_url: string;
                            /** @description Free-form key/value metadata attached by the product builder. */
                            metadata: {
                                [key: string]: unknown;
                            };
                            name: string;
                            /** @description Owning seller's user id; empty for platform products. */
                            owner_id: string;
                            /**
                             * @description `platform` for an administrator-owned product, `user` for a seller's.
                             * @enum {string}
                             */
                            owner_kind: "platform" | "user";
                            /** @description Product builder template this product was created from. */
                            product_template_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the product last became active, or `null`.
                             */
                            published_at: string | null;
                            /** @description Id of a product the buyer must already own before checkout, or empty. */
                            requires: string;
                            /** @description Seller account the product sells through; empty for platform products. */
                            seller_account_id: string;
                            /**
                             * @description URL slug, unique per owner among non-deleted products. Empty when the
                             *     product has none.
                             */
                            slug: string;
                            /**
                             * @description Publication state: `draft`, `pending_review` (seller product awaiting
                             *     moderation), `active` (in the public catalog) or `archived`.
                             * @enum {string}
                             */
                            status: "draft" | "pending_review" | "active" | "archived";
                            /**
                             * Format: int64
                             * @description Units in stock; `0` when inventory is not tracked.
                             */
                            stock: number;
                            /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                            stripe_product_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the seller last submitted the product for
                             *     moderation, or `null`.
                             */
                            submitted_at: string | null;
                            tags: string[];
                            /** @description Product type (taxonomy) id, or empty. */
                            type_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        /** Delete own product */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Always `true`: a delete that did not happen is an error response. */
                            deleted: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        /** Update own product */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        category?: string | null;
                        /** @description ISO 4217 currency. */
                        currency?: string | null;
                        description?: string | null;
                        /** @enum {string|null} */
                        fulfillment_kind?: "none" | "manual" | "download" | "entitlement" | "webhook" | null;
                        group_id?: string | null;
                        group_template_id?: string | null;
                        image_url?: string | null;
                        /** @description Free-form key/value metadata. */
                        metadata?: {
                            [key: string]: unknown;
                        } | null;
                        name?: string | null;
                        /** @description Product builder template. */
                        product_template_id?: string | null;
                        /** @description Id of a product the buyer must already own before checkout. */
                        requires?: string | null;
                        /**
                         * @description Web address of the product: lowercase letters and digits in runs
                         *     joined by single hyphens, at most 160 characters. Empty means none.
                         */
                        slug?: string | null;
                        /**
                         * @description Publication state. A seller may set `draft`, `active` or `archived`;
                         *     `active` on a moderated product submits it for review instead.
                         * @enum {string|null}
                         */
                        status?: "draft" | "pending_review" | "active" | "archived" | null;
                        /** Format: int64 */
                        stock?: number | null;
                        tags?: string[] | null;
                        type_id?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                             *     `rejected` or `suspended`. A different column and a different
                             *     vocabulary from `status` — a listing awaiting review is
                             *     `status = pending_review` and `approval_status = pending` at once.
                             * @enum {string}
                             */
                            approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                            category: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /** @description Id of the user who created the row. */
                            created_by: string;
                            /** @description ISO 4217 presentment currency. */
                            currency: string;
                            /**
                             * Format: int64
                             * @description Version counter of the product's immutable offer definitions.
                             */
                            current_version: number;
                            /**
                             * Format: date-time
                             * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                             *     soft-deleted.
                             */
                            deleted_at: string | null;
                            description: string;
                            /**
                             * @description How a purchase is fulfilled.
                             * @enum {string}
                             */
                            fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                            /** @description Group the product is listed under, or empty. */
                            group_id: string;
                            group_template_id: string;
                            /** @description Stable product identifier. */
                            id: string;
                            image_url: string;
                            /** @description Free-form key/value metadata attached by the product builder. */
                            metadata: {
                                [key: string]: unknown;
                            };
                            name: string;
                            /** @description Owning seller's user id; empty for platform products. */
                            owner_id: string;
                            /**
                             * @description `platform` for an administrator-owned product, `user` for a seller's.
                             * @enum {string}
                             */
                            owner_kind: "platform" | "user";
                            /** @description Product builder template this product was created from. */
                            product_template_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the product last became active, or `null`.
                             */
                            published_at: string | null;
                            /** @description Id of a product the buyer must already own before checkout, or empty. */
                            requires: string;
                            /** @description Seller account the product sells through; empty for platform products. */
                            seller_account_id: string;
                            /**
                             * @description URL slug, unique per owner among non-deleted products. Empty when the
                             *     product has none.
                             */
                            slug: string;
                            /**
                             * @description Publication state: `draft`, `pending_review` (seller product awaiting
                             *     moderation), `active` (in the public catalog) or `archived`.
                             * @enum {string}
                             */
                            status: "draft" | "pending_review" | "active" | "archived";
                            /**
                             * Format: int64
                             * @description Units in stock; `0` when inventory is not tracked.
                             */
                            stock: number;
                            /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                            stripe_product_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the seller last submitted the product for
                             *     moderation, or `null`.
                             */
                            submitted_at: string | null;
                            tags: string[];
                            /** @description Product type (taxonomy) id, or empty. */
                            type_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/products/api/products/{id}/duplicate": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Duplicate own product and editable offers */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offers: {
                                offer: {
                                    billing_scheme: string;
                                    checkout: Record<string, never>;
                                    components: Record<string, never>[];
                                    currency: string;
                                    id: string;
                                    interval_count: number;
                                    /** @enum {string} */
                                    mode: "payment" | "subscription";
                                    name: string;
                                    /** @enum {string} */
                                    pricing_model: "fixed" | "components";
                                    product_id: string;
                                    recurring_interval?: string | null;
                                    stripe_price_id: string;
                                    stripe_product_id: string;
                                    tax_behavior: string;
                                    usage_type: string;
                                    variables: Record<string, never>[];
                                    version: number;
                                };
                                /** @enum {string} */
                                status: "draft" | "active" | "archived";
                                sync_error: string;
                                sync_status: string;
                            }[];
                            /**
                             * ProductView
                             * @description A product row as published to its owner and to administrators: every
                             *     column of the products table.
                             */
                            product: {
                                /**
                                 * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                                 *     `rejected` or `suspended`. A different column and a different
                                 *     vocabulary from `status` — a listing awaiting review is
                                 *     `status = pending_review` and `approval_status = pending` at once.
                                 * @enum {string}
                                 */
                                approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                                category: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description Id of the user who created the row. */
                                created_by: string;
                                /** @description ISO 4217 presentment currency. */
                                currency: string;
                                /**
                                 * Format: int64
                                 * @description Version counter of the product's immutable offer definitions.
                                 */
                                current_version: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                                 *     soft-deleted.
                                 */
                                deleted_at: string | null;
                                description: string;
                                /**
                                 * @description How a purchase is fulfilled.
                                 * @enum {string}
                                 */
                                fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                                /** @description Group the product is listed under, or empty. */
                                group_id: string;
                                group_template_id: string;
                                /** @description Stable product identifier. */
                                id: string;
                                image_url: string;
                                /** @description Free-form key/value metadata attached by the product builder. */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                name: string;
                                /** @description Owning seller's user id; empty for platform products. */
                                owner_id: string;
                                /**
                                 * @description `platform` for an administrator-owned product, `user` for a seller's.
                                 * @enum {string}
                                 */
                                owner_kind: "platform" | "user";
                                /** @description Product builder template this product was created from. */
                                product_template_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the product last became active, or `null`.
                                 */
                                published_at: string | null;
                                /** @description Id of a product the buyer must already own before checkout, or empty. */
                                requires: string;
                                /** @description Seller account the product sells through; empty for platform products. */
                                seller_account_id: string;
                                /**
                                 * @description URL slug, unique per owner among non-deleted products. Empty when the
                                 *     product has none.
                                 */
                                slug: string;
                                /**
                                 * @description Publication state: `draft`, `pending_review` (seller product awaiting
                                 *     moderation), `active` (in the public catalog) or `archived`.
                                 * @enum {string}
                                 */
                                status: "draft" | "pending_review" | "active" | "archived";
                                /**
                                 * Format: int64
                                 * @description Units in stock; `0` when inventory is not tracked.
                                 */
                                stock: number;
                                /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                                stripe_product_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the seller last submitted the product for
                                 *     moderation, or `null`.
                                 */
                                submitted_at: string | null;
                                tags: string[];
                                /** @description Product type (taxonomy) id, or empty. */
                                type_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            };
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/products/{id}/restore": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Restore own soft-deleted product
         * @description Clears `deleted_at` on a product the caller owns, undoing their own delete. The admin route is `/b/products/api/admin/products/{id}/restore`; this one is scoped to the caller's own products and answers 404 for anyone else's.
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                             *     `rejected` or `suspended`. A different column and a different
                             *     vocabulary from `status` — a listing awaiting review is
                             *     `status = pending_review` and `approval_status = pending` at once.
                             * @enum {string}
                             */
                            approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                            category: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /** @description Id of the user who created the row. */
                            created_by: string;
                            /** @description ISO 4217 presentment currency. */
                            currency: string;
                            /**
                             * Format: int64
                             * @description Version counter of the product's immutable offer definitions.
                             */
                            current_version: number;
                            /**
                             * Format: date-time
                             * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                             *     soft-deleted.
                             */
                            deleted_at: string | null;
                            description: string;
                            /**
                             * @description How a purchase is fulfilled.
                             * @enum {string}
                             */
                            fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                            /** @description Group the product is listed under, or empty. */
                            group_id: string;
                            group_template_id: string;
                            /** @description Stable product identifier. */
                            id: string;
                            image_url: string;
                            /** @description Free-form key/value metadata attached by the product builder. */
                            metadata: {
                                [key: string]: unknown;
                            };
                            name: string;
                            /** @description Owning seller's user id; empty for platform products. */
                            owner_id: string;
                            /**
                             * @description `platform` for an administrator-owned product, `user` for a seller's.
                             * @enum {string}
                             */
                            owner_kind: "platform" | "user";
                            /** @description Product builder template this product was created from. */
                            product_template_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the product last became active, or `null`.
                             */
                            published_at: string | null;
                            /** @description Id of a product the buyer must already own before checkout, or empty. */
                            requires: string;
                            /** @description Seller account the product sells through; empty for platform products. */
                            seller_account_id: string;
                            /**
                             * @description URL slug, unique per owner among non-deleted products. Empty when the
                             *     product has none.
                             */
                            slug: string;
                            /**
                             * @description Publication state: `draft`, `pending_review` (seller product awaiting
                             *     moderation), `active` (in the public catalog) or `archived`.
                             * @enum {string}
                             */
                            status: "draft" | "pending_review" | "active" | "archived";
                            /**
                             * Format: int64
                             * @description Units in stock; `0` when inventory is not tracked.
                             */
                            stock: number;
                            /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                            stripe_product_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the seller last submitted the product for
                             *     moderation, or `null`.
                             */
                            submitted_at: string | null;
                            tags: string[];
                            /** @description Product type (taxonomy) id, or empty. */
                            type_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/products/{product_id}/offers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List own product offers */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offers: {
                                offer: {
                                    billing_scheme: string;
                                    checkout: Record<string, never>;
                                    components: Record<string, never>[];
                                    currency: string;
                                    id: string;
                                    interval_count: number;
                                    /** @enum {string} */
                                    mode: "payment" | "subscription";
                                    name: string;
                                    /** @enum {string} */
                                    pricing_model: "fixed" | "components";
                                    product_id: string;
                                    recurring_interval?: string | null;
                                    stripe_price_id: string;
                                    stripe_product_id: string;
                                    tax_behavior: string;
                                    usage_type: string;
                                    variables: Record<string, never>[];
                                    version: number;
                                };
                                /** @enum {string} */
                                status: "draft" | "active" | "archived";
                                sync_error: string;
                                sync_status: string;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create own product offer */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @enum {string} */
                        billing_scheme: "per_unit" | "tiered";
                        checkout?: Record<string, never>;
                        components: Record<string, never>[];
                        currency: string;
                        /** @default 1 */
                        interval_count?: number;
                        /** @enum {string} */
                        mode: "payment" | "subscription";
                        name: string;
                        /** @enum {string} */
                        pricing_model: "fixed" | "components";
                        /** @enum {string|null} */
                        recurring_interval?: "day" | "week" | "month" | "year" | null;
                        /** @enum {string} */
                        tax_behavior: "unspecified" | "inclusive" | "exclusive";
                        /** @enum {string} */
                        usage_type: "licensed" | "metered";
                        variables?: Record<string, never>[];
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/products/{product_id}/offers/{offer_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get own product offer */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        /** Archive own offer */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        /** Update own draft offer */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @enum {string} */
                        billing_scheme: "per_unit" | "tiered";
                        checkout?: Record<string, never>;
                        components: Record<string, never>[];
                        currency: string;
                        /** @default 1 */
                        interval_count?: number;
                        /** @enum {string} */
                        mode: "payment" | "subscription";
                        name: string;
                        /** @enum {string} */
                        pricing_model: "fixed" | "components";
                        /** @enum {string|null} */
                        recurring_interval?: "day" | "week" | "month" | "year" | null;
                        /** @enum {string} */
                        tax_behavior: "unspecified" | "inclusive" | "exclusive";
                        /** @enum {string} */
                        usage_type: "licensed" | "metered";
                        variables?: Record<string, never>[];
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/products/api/products/{product_id}/offers/{offer_id}/duplicate": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Duplicate own offer */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/products/{product_id}/offers/{offer_id}/payment-links": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List own Payment Links */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            payment_links: {
                                active: boolean;
                                configuration_hash: string;
                                id: string;
                                offer_id: string;
                                /** @default  */
                                preset_id: string;
                                /** @default  */
                                sync_error: string;
                                sync_status: string;
                                /**
                                 * Format: uri
                                 * @description Stripe-hosted Payment Link the buyer opens.
                                 */
                                url: string;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create or reuse own Payment Link */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * Format: uri
                         * @description Where Stripe sends the buyer once the Payment Link is paid.
                         * @default null
                         */
                        after_completion_url?: string | null;
                        /** @default null */
                        preset_id?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            active: boolean;
                            configuration_hash: string;
                            id: string;
                            offer_id: string;
                            /** @default  */
                            preset_id: string;
                            /** @default  */
                            sync_error: string;
                            sync_status: string;
                            /**
                             * Format: uri
                             * @description Stripe-hosted Payment Link the buyer opens.
                             */
                            url: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/products/{product_id}/offers/{offer_id}/payment-links/{link_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Deactivate own Payment Link */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    link_id: string;
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            active: boolean;
                            configuration_hash: string;
                            id: string;
                            offer_id: string;
                            /** @default  */
                            preset_id: string;
                            /** @default  */
                            sync_error: string;
                            sync_status: string;
                            /**
                             * Format: uri
                             * @description Stripe-hosted Payment Link the buyer opens.
                             */
                            url: string;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/products/{product_id}/offers/{offer_id}/presets": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List own checkout presets */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            presets: {
                                active: boolean;
                                configuration_hash: string;
                                id: string;
                                inputs: {
                                    [key: string]: unknown;
                                };
                                name: string;
                                offer_id: string;
                                slug: string;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create own checkout preset */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @default {} */
                        inputs?: {
                            [key: string]: unknown;
                        };
                        name: string;
                        /** @default  */
                        slug?: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            active: boolean;
                            configuration_hash: string;
                            id: string;
                            inputs: {
                                [key: string]: unknown;
                            };
                            name: string;
                            offer_id: string;
                            slug: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/products/{product_id}/offers/{offer_id}/presets/{preset_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get own checkout preset */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    preset_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            active: boolean;
                            configuration_hash: string;
                            id: string;
                            inputs: {
                                [key: string]: unknown;
                            };
                            name: string;
                            offer_id: string;
                            slug: string;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        /** Archive own checkout preset */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    preset_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            active: boolean;
                            configuration_hash: string;
                            id: string;
                            inputs: {
                                [key: string]: unknown;
                            };
                            name: string;
                            offer_id: string;
                            slug: string;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        /** Update own checkout preset */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    preset_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @default {} */
                        inputs?: {
                            [key: string]: unknown;
                        };
                        name: string;
                        /** @default  */
                        slug?: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            active: boolean;
                            configuration_hash: string;
                            id: string;
                            inputs: {
                                [key: string]: unknown;
                            };
                            name: string;
                            offer_id: string;
                            slug: string;
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/products/api/products/{product_id}/offers/{offer_id}/preview": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Preview own draft or active offer
         * @description Evaluate an owned immutable or draft offer with the server pricing engine. Browser totals are never trusted.
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @default {} */
                        inputs?: {
                            [key: string]: unknown;
                        };
                        offer_id: string;
                        /**
                         * Format: uint64
                         * @default 1
                         */
                        quantity?: number;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            amounts: {
                                currency: string;
                                /** Format: int64 */
                                discount_minor: number;
                                /** Format: int64 */
                                platform_fee_minor: number;
                                /**
                                 * Format: int64
                                 * @default 0
                                 */
                                shipping_minor: number;
                                /** Format: int64 */
                                subtotal_minor: number;
                                /** Format: int64 */
                                tax_minor: number;
                                /** Format: int64 */
                                total_minor: number;
                            };
                            components: {
                                component_id: string;
                                included: boolean;
                                key: string;
                                label: string;
                                /** Format: uint64 */
                                quantity: number;
                                reason: string;
                                required: boolean;
                                /** Format: int64 */
                                total_amount_minor: number;
                                /** Format: int64 */
                                unit_amount_minor: number;
                            }[];
                            inputs: {
                                [key: string]: unknown;
                            };
                            offer_id: string;
                            /** Format: uint32 */
                            offer_version: number;
                            /** Format: uint64 */
                            quantity: number;
                            /** Format: uint32 */
                            schema_version: number;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/products/{product_id}/offers/{offer_id}/publish": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Publish own offer */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/products/{product_id}/offers/{offer_id}/sync": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Synchronize own immutable Product and fixed Prices to Stripe */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    offer_id: string;
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            offer: {
                                billing_scheme: string;
                                checkout: Record<string, never>;
                                components: Record<string, never>[];
                                currency: string;
                                id: string;
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                product_id: string;
                                recurring_interval?: string | null;
                                stripe_price_id: string;
                                stripe_product_id: string;
                                tax_behavior: string;
                                usage_type: string;
                                variables: Record<string, never>[];
                                version: number;
                            };
                            /** @enum {string} */
                            status: "draft" | "active" | "archived";
                            sync_error: string;
                            sync_status: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/seller/account": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Seller Stripe account status */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Whether an administrator has suspended the account. Derived from
                             *     `status`; the two move together.
                             * @enum {string}
                             */
                            approval_status: "approved" | "suspended";
                            capabilities: {
                                charges_enabled: boolean;
                                details_submitted: boolean;
                                payouts_enabled: boolean;
                                /** @default [] */
                                requirements_due: string[];
                            };
                            /** @default  */
                            country: string;
                            /** @default  */
                            dashboard_type: string;
                            /** @default  */
                            default_currency: string;
                            /** @default  */
                            disabled_reason: string;
                            /**
                             * Format: uint32
                             * @description The platform application fee new Checkout Sessions and Payment Links carry, in basis points.
                             */
                            fee_basis_points: number;
                            id: string;
                            /** @default  */
                            last_synced_at: string;
                            /** @default false */
                            livemode: boolean;
                            /**
                             * @description How far the account has got with Stripe Connect.
                             * @enum {string}
                             */
                            status: "not_started" | "onboarding" | "restricted" | "active" | "suspended";
                            /** @default  */
                            stripe_account_id: string;
                            /** @default  */
                            sync_error: string;
                            user_id: string;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/seller/dashboard": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Create Stripe Express dashboard login link */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: uri
                             * @description Absolute provider-hosted URL the browser must be sent to.
                             */
                            url: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/seller/onboarding": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Create seller account and Stripe-hosted onboarding link */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * Format: uri
                         * @description Where Stripe returns the seller if the onboarding link expired.
                         */
                        refresh_url: string;
                        /**
                         * Format: uri
                         * @description Where Stripe returns the seller once onboarding is submitted.
                         */
                        return_url: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            account: {
                                /**
                                 * @description Whether an administrator has suspended the account. Derived from
                                 *     `status`; the two move together.
                                 * @enum {string}
                                 */
                                approval_status: "approved" | "suspended";
                                capabilities: {
                                    charges_enabled: boolean;
                                    details_submitted: boolean;
                                    payouts_enabled: boolean;
                                    /** @default [] */
                                    requirements_due: string[];
                                };
                                /** @default  */
                                country: string;
                                /** @default  */
                                dashboard_type: string;
                                /** @default  */
                                default_currency: string;
                                /** @default  */
                                disabled_reason: string;
                                /**
                                 * Format: uint32
                                 * @description The platform application fee new Checkout Sessions and Payment Links carry, in basis points.
                                 */
                                fee_basis_points: number;
                                id: string;
                                /** @default  */
                                last_synced_at: string;
                                /** @default false */
                                livemode: boolean;
                                /**
                                 * @description How far the account has got with Stripe Connect.
                                 * @enum {string}
                                 */
                                status: "not_started" | "onboarding" | "restricted" | "active" | "suspended";
                                /** @default  */
                                stripe_account_id: string;
                                /** @default  */
                                sync_error: string;
                                user_id: string;
                            };
                            /** Format: int64 */
                            expires_at: number;
                            /**
                             * Format: uri
                             * @description Single-use Stripe-hosted onboarding link.
                             */
                            url: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/seller/orders": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List seller-owned orders */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                    status?: string | null;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            records: {
                                /** @description Buyer's email address as captured at checkout, or empty. */
                                buyer_email: string;
                                /** @description Checkout presentation the order was started with. */
                                checkout_mode: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description ISO 4217 currency of every amount on the order. */
                                currency: string;
                                /** Format: int64 */
                                discount_cents: number;
                                /** @description Stable order identifier. */
                                id: string;
                                /** @description Whether the order was placed against the live Stripe environment. */
                                livemode: boolean;
                                /** @description Immutable checkout snapshot. */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp payment was recorded, or `null`.
                                 */
                                payment_at: string | null;
                                /**
                                 * Format: int64
                                 * @description Platform fee taken from this order, in minor units.
                                 */
                                platform_fee_cents: number;
                                /** @description Payment provider: `stripe`, or `manual`. */
                                provider: string;
                                provider_payment_error_code: string;
                                provider_payment_error_message: string;
                                /**
                                 * @description Latest PaymentIntent state received from the provider.
                                 * @enum {string}
                                 */
                                provider_payment_status: "" | "succeeded" | "payment_failed" | "processing" | "requires_action" | "canceled";
                                /** @description Stripe Checkout Session id, or empty. */
                                provider_session_id: string;
                                reconciliation_error: string;
                                /**
                                 * @description Where the order stands against the provider's view of it.
                                 * @enum {string}
                                 */
                                reconciliation_status: "pending" | "awaiting_payment" | "reconciled" | "provider_error" | "payment_succeeded_awaiting_checkout" | "payment_failed" | "payment_processing" | "payment_requires_action" | "payment_canceled";
                                /** @description Operator note attached to the last refund, or empty. */
                                refund_reason: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last refund applied, or `null`.
                                 */
                                refunded_at: string | null;
                                /** @description User id of the operator who issued the last refund, or empty. */
                                refunded_by: string;
                                /**
                                 * Format: int64
                                 * @description Sum of succeeded refunds in minor units.
                                 */
                                refunded_total_cents: number;
                                /** @description Seller account the order was placed against. */
                                seller_account_id: string;
                                /** Format: int64 */
                                shipping_cents: number;
                                /**
                                 * @description Lifecycle state of the order.
                                 * @enum {string}
                                 */
                                status: "pending" | "checkout_started" | "completed" | "partially_refunded" | "refunded" | "failed";
                                /** @description Stripe connected account the order was charged through, or empty. */
                                stripe_account_id: string;
                                /** @description Stripe PaymentIntent id, or empty. */
                                stripe_payment_intent_id: string;
                                subscription_cancel_at_period_end: boolean;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 end of the current billing period, or `null`.
                                 */
                                subscription_current_period_end: string | null;
                                /**
                                 * @description Stripe subscription lifecycle state, or empty.
                                 * @enum {string}
                                 */
                                subscription_status: "" | "incomplete" | "incomplete_expired" | "trialing" | "active" | "past_due" | "unpaid" | "paused" | "canceled";
                                /** Format: int64 */
                                subtotal_cents: number;
                                /** Format: int64 */
                                tax_cents: number;
                                /**
                                 * Format: int64
                                 * @description Final charged amount in minor units.
                                 */
                                total_cents: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total orders matching the query, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/seller/orders/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get seller-owned order */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            disputes: {
                                /** Format: int64 */
                                amount_minor: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the dispute closed, or `null`.
                                 */
                                closed_at: string | null;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                currency: string;
                                /**
                                 * Format: int64
                                 * @description Provider timestamp (Unix seconds) of the event that last updated the
                                 *     dispute.
                                 */
                                event_created: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 evidence deadline, or `null`.
                                 */
                                evidence_due_by: string | null;
                                /** @description Stable dispute identifier. */
                                id: string;
                                livemode: boolean;
                                payment_intent_id: string;
                                /** @description Stripe Charge id the dispute was raised against, or empty. */
                                provider_charge_id: string;
                                /** @description Stripe Dispute id. */
                                provider_dispute_id: string;
                                purchase_id: string;
                                /** @description Provider's dispute reason, or empty. */
                                reason: string;
                                seller_account_id: string;
                                /**
                                 * @description Where the dispute stands with the card network.
                                 * @enum {string}
                                 */
                                status: "warning_needs_response" | "warning_under_review" | "warning_closed" | "needs_response" | "under_review" | "won" | "lost" | "prevented";
                                stripe_account_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            line_items: {
                                /** @description Offer component the line resolved, or empty for a whole-offer line. */
                                component_id: string;
                                /** @description The component condition as it was evaluated at checkout. */
                                condition_snapshot: {
                                    [key: string]: unknown;
                                };
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** Format: int64 */
                                discount_minor: number;
                                /** @description Stable line identifier. */
                                id: string;
                                /** @description Customer inputs the line was priced with, as submitted at checkout. */
                                input_snapshot: {
                                    [key: string]: unknown;
                                };
                                /** @description Offer the line was priced from, or empty for legacy lines. */
                                offer_id: string;
                                /**
                                 * Format: int64
                                 * @description Version of that offer at checkout.
                                 */
                                offer_version: number;
                                product_id: string;
                                /** @description Product name as it was at checkout. */
                                product_name: string;
                                purchase_id: string;
                                /** Format: int64 */
                                quantity: number;
                                seller_account_id: string;
                                /** @description Stripe Price id the line was charged through, or empty. */
                                stripe_price_id: string;
                                /** Format: int64 */
                                subtotal_minor: number;
                                /** Format: int64 */
                                tax_minor: number;
                                /** Format: int64 */
                                total_minor: number;
                                /** Format: int64 */
                                unit_amount_minor: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * @description One order as the **seller** who fulfils it may read it.
                             *
                             *     Wider than the buyer's: a seller needs the fee that was taken, their own
                             *     connected account, the provider handles for their own charge, and the
                             *     buyer's email in order to fulfil. It still withholds the buyer's platform
                             *     identity (`user_id` / `buyer_user_id`) and their Stripe customer id, none
                             *     of which a seller needs to ship an order.
                             */
                            purchase: {
                                /** @description Buyer's email address as captured at checkout, or empty. */
                                buyer_email: string;
                                /** @description Checkout presentation the order was started with. */
                                checkout_mode: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description ISO 4217 currency of every amount on the order. */
                                currency: string;
                                /** Format: int64 */
                                discount_cents: number;
                                /** @description Stable order identifier. */
                                id: string;
                                /** @description Whether the order was placed against the live Stripe environment. */
                                livemode: boolean;
                                /** @description Immutable checkout snapshot. */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp payment was recorded, or `null`.
                                 */
                                payment_at: string | null;
                                /**
                                 * Format: int64
                                 * @description Platform fee taken from this order, in minor units.
                                 */
                                platform_fee_cents: number;
                                /** @description Payment provider: `stripe`, or `manual`. */
                                provider: string;
                                provider_payment_error_code: string;
                                provider_payment_error_message: string;
                                /**
                                 * @description Latest PaymentIntent state received from the provider.
                                 * @enum {string}
                                 */
                                provider_payment_status: "" | "succeeded" | "payment_failed" | "processing" | "requires_action" | "canceled";
                                /** @description Stripe Checkout Session id, or empty. */
                                provider_session_id: string;
                                reconciliation_error: string;
                                /**
                                 * @description Where the order stands against the provider's view of it.
                                 * @enum {string}
                                 */
                                reconciliation_status: "pending" | "awaiting_payment" | "reconciled" | "provider_error" | "payment_succeeded_awaiting_checkout" | "payment_failed" | "payment_processing" | "payment_requires_action" | "payment_canceled";
                                /** @description Operator note attached to the last refund, or empty. */
                                refund_reason: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last refund applied, or `null`.
                                 */
                                refunded_at: string | null;
                                /** @description User id of the operator who issued the last refund, or empty. */
                                refunded_by: string;
                                /**
                                 * Format: int64
                                 * @description Sum of succeeded refunds in minor units.
                                 */
                                refunded_total_cents: number;
                                /** @description Seller account the order was placed against. */
                                seller_account_id: string;
                                /** Format: int64 */
                                shipping_cents: number;
                                /**
                                 * @description Lifecycle state of the order.
                                 * @enum {string}
                                 */
                                status: "pending" | "checkout_started" | "completed" | "partially_refunded" | "refunded" | "failed";
                                /** @description Stripe connected account the order was charged through, or empty. */
                                stripe_account_id: string;
                                /** @description Stripe PaymentIntent id, or empty. */
                                stripe_payment_intent_id: string;
                                subscription_cancel_at_period_end: boolean;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 end of the current billing period, or `null`.
                                 */
                                subscription_current_period_end: string | null;
                                /**
                                 * @description Stripe subscription lifecycle state, or empty.
                                 * @enum {string}
                                 */
                                subscription_status: "" | "incomplete" | "incomplete_expired" | "trialing" | "active" | "past_due" | "unpaid" | "paused" | "canceled";
                                /** Format: int64 */
                                subtotal_cents: number;
                                /** Format: int64 */
                                tax_cents: number;
                                /**
                                 * Format: int64
                                 * @description Final charged amount in minor units.
                                 */
                                total_cents: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            };
                            refunds: {
                                /** Format: int64 */
                                amount_minor: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the refund reached a terminal state, or `null`.
                                 */
                                completed_at: string | null;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                currency: string;
                                /** @description Stable refund identifier. */
                                id: string;
                                last_error: string;
                                livemode: boolean;
                                /** @description Operator note kept on the platform, never sent to the provider. */
                                note: string;
                                /**
                                 * @description PaymentIntent the refund was issued against; empty for manual
                                 *     refunds.
                                 */
                                payment_intent_id: string;
                                /** @description Reason sent to the provider, or empty. */
                                provider_reason: string;
                                /** @description Stripe Refund id once the provider accepted it, or empty. */
                                provider_refund_id: string;
                                /**
                                 * @description The provider's own state for the refund (`pending`, `requires_action`,
                                 *     `succeeded`, `failed` or `canceled`). Empty until the provider answers;
                                 *     `succeeded` for a refund recorded without a provider, which the ledger
                                 *     completes itself.
                                 */
                                provider_status: string;
                                purchase_id: string;
                                /** @description User id of the operator who issued the refund. */
                                refunded_by: string;
                                /**
                                 * @description Ledger state.
                                 * @enum {string}
                                 */
                                status: "pending" | "provider_succeeded" | "succeeded" | "failed";
                                /** @description Stripe connected account the refund was issued through, or empty. */
                                stripe_account_id: string;
                                /**
                                 * Format: int64
                                 * @description Provider timestamp (Unix seconds) of the event that last updated the
                                 *     refund; `0` until one arrives.
                                 */
                                stripe_event_created: number;
                                /**
                                 * Format: int64
                                 * @description The order's `refunded_total_cents` once this refund succeeds.
                                 */
                                target_refunded_total_minor: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/seller/orders/{id}/refund": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Refund a seller-owned order */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * Format: int64
                         * @description Exact amount in the order currency's minor unit. Omit to refund the
                         *     complete remaining refundable amount.
                         * @default null
                         */
                        amount_minor?: number | null;
                        /**
                         * @description Stable client operation key. Supplying a fresh key allows a deliberate
                         *     second partial refund of the same amount; retries must reuse the key.
                         * @default null
                         */
                        idempotency_key?: string | null;
                        /**
                         * @description Private operator note retained in ImpressPress, never sent to Stripe.
                         * @default null
                         */
                        note?: string | null;
                        /**
                         * @description Stripe's constrained provider reason. Human context belongs in `note`.
                         * @default null
                         * @enum {string|null}
                         */
                        provider_reason?: "duplicate" | "fraudulent" | "requested_by_customer" | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** Format: int64 */
                            amount_minor: number;
                            currency: string;
                            livemode: boolean;
                            /** Format: int64 */
                            order_total_minor: number;
                            /** @default  */
                            provider_refund_id: string;
                            /** @default  */
                            provider_status: string;
                            purchase_id: string;
                            /** @default  */
                            refund_id: string;
                            /** Format: int64 */
                            refunded_total_minor: number;
                            /** @enum {string} */
                            status: "pending" | "succeeded" | "failed";
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/api/seller/stats": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Seller analytics separated by currency */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description One entry per currency this seller has transacted in. */
                            currency_analytics: {
                                /** Format: uint64 */
                                active_subscription_count: number;
                                /** Format: uint64 */
                                canceled_subscription_count: number;
                                currency: string;
                                /** Format: uint64 */
                                failed_order_count: number;
                                /** Format: int64 */
                                gross_volume_minor: number;
                                /** Format: uint64 */
                                lost_dispute_count: number;
                                /** Format: int64 */
                                lost_disputed_volume_minor: number;
                                /** Format: int64 */
                                net_volume_minor: number;
                                /** Format: uint64 */
                                open_dispute_count: number;
                                /** Format: int64 */
                                open_disputed_volume_minor: number;
                                /** Format: uint64 */
                                order_count: number;
                                /** Format: uint64 */
                                paid_order_count: number;
                                /** Format: uint64 */
                                past_due_subscription_count: number;
                                /** Format: int64 */
                                platform_fees_minor: number;
                                /** Format: uint64 */
                                refunded_order_count: number;
                                /** Format: int64 */
                                refunded_volume_minor: number;
                                top_products: {
                                    name: string;
                                    product_id: string;
                                    /** Format: uint64 */
                                    quantity: number;
                                    /** Format: int64 */
                                    revenue_minor: number;
                                }[];
                                /** Format: uint64 */
                                trialing_subscription_count: number;
                            }[];
                            recent_failures: {
                                /** Format: date-time */
                                created_at: string;
                                currency: string;
                                /** @default  */
                                error: string;
                                order_id: string;
                                /**
                                 * @description `failed` for a terminal failure; otherwise the state of an order whose
                                 *     last PaymentIntent event needs the seller's attention.
                                 * @enum {string}
                                 */
                                status: "pending" | "checkout_started" | "completed" | "partially_refunded" | "refunded" | "failed";
                                /** Format: int64 */
                                total_minor: number;
                            }[];
                            /** @description Empty when the user has not started Stripe onboarding. */
                            seller_account_id: string;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/billing-portal": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Create a Stripe Billing Portal session for an owned customer context */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @default null */
                        order_id?: string | null;
                        /**
                         * Format: uri
                         * @description Where Stripe returns the customer when they leave the portal.
                         */
                        return_url: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: uri
                             * @description Absolute provider-hosted URL the browser must be sent to.
                             */
                            url: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/catalog": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Browse catalog
         * @description Public list of active products, sorted by name.
         */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /** @description Active products on this page, sorted by name. */
                            records: {
                                category: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description ISO 4217 presentment currency. */
                                currency: string;
                                description: string;
                                /**
                                 * @description How a purchase is fulfilled.
                                 * @enum {string}
                                 */
                                fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                                /** @description Group the product is listed under, or empty. */
                                group_id: string;
                                group_template_id: string;
                                /** @description Stable product identifier. */
                                id: string;
                                image_url: string;
                                /** @description Free-form key/value metadata attached by the product builder. */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                name: string;
                                /** @description Product builder template this product was created from. */
                                product_template_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the product last became active, or `null`.
                                 */
                                published_at: string | null;
                                /** @description Id of a product the buyer must already own before checkout, or empty. */
                                requires: string;
                                /** @description URL slug, or empty. */
                                slug: string;
                                /**
                                 * @description Always `active`: the catalog lists active products only.
                                 * @enum {string}
                                 */
                                status: "active";
                                /**
                                 * Format: int64
                                 * @description Units in stock; `0` when inventory is not tracked.
                                 */
                                stock: number;
                                tags: string[];
                                /** @description Product type (taxonomy) id, or empty. */
                                type_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total active products, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/catalog/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Product detail */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            category: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /** @description ISO 4217 presentment currency. */
                            currency: string;
                            description: string;
                            /**
                             * @description How a purchase is fulfilled.
                             * @enum {string}
                             */
                            fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                            /** @description Group the product is listed under, or empty. */
                            group_id: string;
                            group_template_id: string;
                            /** @description Stable product identifier. */
                            id: string;
                            image_url: string;
                            /** @description Free-form key/value metadata attached by the product builder. */
                            metadata: {
                                [key: string]: unknown;
                            };
                            name: string;
                            /** @description Product builder template this product was created from. */
                            product_template_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp the product last became active, or `null`.
                             */
                            published_at: string | null;
                            /** @description Id of a product the buyer must already own before checkout, or empty. */
                            requires: string;
                            /** @description URL slug, or empty. */
                            slug: string;
                            /**
                             * @description Always `active`: the catalog lists active products only.
                             * @enum {string}
                             */
                            status: "active";
                            /**
                             * Format: int64
                             * @description Units in stock; `0` when inventory is not tracked.
                             */
                            stock: number;
                            tags: string[];
                            /** @description Product type (taxonomy) id, or empty. */
                            type_id: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/checkout": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Stripe checkout
         * @description Create a hosted or embedded Stripe Checkout Session from a public active offer. Guest checkout is supported and every amount is resolved from the immutable offer.
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * Format: email
                         * @default null
                         */
                        buyer_email?: string | null;
                        /**
                         * Format: uri
                         * @default null
                         */
                        cancel_url?: string | null;
                        /** @default {} */
                        inputs?: {
                            [key: string]: unknown;
                        };
                        offer_id: string;
                        /**
                         * @default hosted
                         * @enum {string}
                         */
                        presentation?: "hosted" | "embedded" | "payment_link";
                        /** @default null */
                        preset_id?: string | null;
                        /**
                         * Format: uint64
                         * @default 1
                         */
                        quantity?: number;
                        /**
                         * Format: uri
                         * @default null
                         */
                        success_url?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            amounts: {
                                currency: string;
                                /** Format: int64 */
                                discount_minor: number;
                                /** Format: int64 */
                                platform_fee_minor: number;
                                /**
                                 * Format: int64
                                 * @default 0
                                 */
                                shipping_minor: number;
                                /** Format: int64 */
                                subtotal_minor: number;
                                /** Format: int64 */
                                tax_minor: number;
                                /** Format: int64 */
                                total_minor: number;
                            };
                            /**
                             * Format: uri
                             * @default null
                             */
                            checkout_url: string | null;
                            /**
                             * @description Stripe Embedded Checkout client secret. Present only for the embedded
                             *     presentation; it is how the browser opens the session, so it is
                             *     returned here and nowhere else. Same handling as `receipt_token`:
                             *     never log it.
                             * @default null
                             */
                            client_secret: string | null;
                            order_id: string;
                            /**
                             * Format: uri
                             * @default null
                             */
                            payment_link_url: string | null;
                            /** @enum {string} */
                            presentation: "hosted" | "embedded" | "payment_link";
                            /**
                             * @description Returned once and never persisted in plaintext. Static storefronts use
                             *     it to poll the minimal guest order-status endpoint after Stripe returns.
                             *
                             *     A bearer capability: whoever holds it can read the order's status.
                             *     Treat it like a session token — never log it, never put it in a URL
                             *     that gets shared. This response is its only delivery, which is why it
                             *     is not `writeOnly`: that keyword claims a field is never present in a
                             *     response, and this one is always present in this one.
                             */
                            receipt_token: string;
                            /** Format: date-time */
                            receipt_token_expires_at: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/group-templates": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List group templates for the authenticated builder */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description Always `1`.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description The fixed ceiling on rows returned.
                             */
                            page_size: number;
                            /** @description Templates, sorted by name. */
                            records: {
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description Human-readable name shown in the product builder. */
                                display_name: string;
                                /** @description Stable template identifier. */
                                id: string;
                                /** @description Machine name (`default`, …). */
                                name: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total templates. Always equal to the number of records: the endpoint
                             *     does not paginate.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/groups": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List own product groups */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /** @description Groups on this page. */
                            records: {
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /**
                                 * @description Id of the user who created the row: the administrator on the admin
                                 *     tier (the owner is `user_id`), the owner on the owner tier.
                                 */
                                created_by: string;
                                description: string;
                                /** @description Group template the group was created from. */
                                group_template_id: string;
                                /** @description Stable group identifier. */
                                id: string;
                                name: string;
                                /** @description `active` unless the group has been retired. */
                                status: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                                /**
                                 * @description Id of the user who owns the group. The owner tier lists and edits
                                 *     only groups whose `user_id` is the caller.
                                 */
                                user_id: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total groups matching the filters, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create own product group */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        description?: string | null;
                        /**
                         * @description Group template. Defaults to the seeded `default` template when
                         *     omitted.
                         */
                        group_template_id?: string | null;
                        name: string;
                        status?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /**
                             * @description Id of the user who created the row: the administrator on the admin
                             *     tier (the owner is `user_id`), the owner on the owner tier.
                             */
                            created_by: string;
                            description: string;
                            /** @description Group template the group was created from. */
                            group_template_id: string;
                            /** @description Stable group identifier. */
                            id: string;
                            name: string;
                            /** @description `active` unless the group has been retired. */
                            status: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                            /**
                             * @description Id of the user who owns the group. The owner tier lists and edits
                             *     only groups whose `user_id` is the caller.
                             */
                            user_id: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/groups/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get own product group */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /**
                             * @description Id of the user who created the row: the administrator on the admin
                             *     tier (the owner is `user_id`), the owner on the owner tier.
                             */
                            created_by: string;
                            description: string;
                            /** @description Group template the group was created from. */
                            group_template_id: string;
                            /** @description Stable group identifier. */
                            id: string;
                            name: string;
                            /** @description `active` unless the group has been retired. */
                            status: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                            /**
                             * @description Id of the user who owns the group. The owner tier lists and edits
                             *     only groups whose `user_id` is the caller.
                             */
                            user_id: string;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        /** Delete own product group */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Always `true`: a delete that did not happen is an error response. */
                            deleted: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        /** Update own product group */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        description?: string | null;
                        group_template_id?: string | null;
                        name?: string | null;
                        status?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: date-time
                             * @description RFC 3339 creation timestamp.
                             */
                            created_at: string;
                            /**
                             * @description Id of the user who created the row: the administrator on the admin
                             *     tier (the owner is `user_id`), the owner on the owner tier.
                             */
                            created_by: string;
                            description: string;
                            /** @description Group template the group was created from. */
                            group_template_id: string;
                            /** @description Stable group identifier. */
                            id: string;
                            name: string;
                            /** @description `active` unless the group has been retired. */
                            status: string;
                            /**
                             * Format: date-time
                             * @description RFC 3339 timestamp of the last modification.
                             */
                            updated_at: string;
                            /**
                             * @description Id of the user who owns the group. The owner tier lists and edits
                             *     only groups whose `user_id` is the caller.
                             */
                            user_id: string;
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/products/groups/{id}/products": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List products in own group */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                };
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /** @description Products on this page, newest first. */
                            records: {
                                /**
                                 * @description Moderation state: `draft`, `pending` (submitted for review), `approved`,
                                 *     `rejected` or `suspended`. A different column and a different
                                 *     vocabulary from `status` — a listing awaiting review is
                                 *     `status = pending_review` and `approval_status = pending` at once.
                                 * @enum {string}
                                 */
                                approval_status: "draft" | "pending" | "approved" | "rejected" | "suspended";
                                category: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description Id of the user who created the row. */
                                created_by: string;
                                /** @description ISO 4217 presentment currency. */
                                currency: string;
                                /**
                                 * Format: int64
                                 * @description Version counter of the product's immutable offer definitions.
                                 */
                                current_version: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 soft-delete timestamp, or `null` unless the product has been
                                 *     soft-deleted.
                                 */
                                deleted_at: string | null;
                                description: string;
                                /**
                                 * @description How a purchase is fulfilled.
                                 * @enum {string}
                                 */
                                fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                                /** @description Group the product is listed under, or empty. */
                                group_id: string;
                                group_template_id: string;
                                /** @description Stable product identifier. */
                                id: string;
                                image_url: string;
                                /** @description Free-form key/value metadata attached by the product builder. */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                name: string;
                                /** @description Owning seller's user id; empty for platform products. */
                                owner_id: string;
                                /**
                                 * @description `platform` for an administrator-owned product, `user` for a seller's.
                                 * @enum {string}
                                 */
                                owner_kind: "platform" | "user";
                                /** @description Product builder template this product was created from. */
                                product_template_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the product last became active, or `null`.
                                 */
                                published_at: string | null;
                                /** @description Id of a product the buyer must already own before checkout, or empty. */
                                requires: string;
                                /** @description Seller account the product sells through; empty for platform products. */
                                seller_account_id: string;
                                /**
                                 * @description URL slug, unique per owner among non-deleted products. Empty when the
                                 *     product has none.
                                 */
                                slug: string;
                                /**
                                 * @description Publication state: `draft`, `pending_review` (seller product awaiting
                                 *     moderation), `active` (in the public catalog) or `archived`.
                                 * @enum {string}
                                 */
                                status: "draft" | "pending_review" | "active" | "archived";
                                /**
                                 * Format: int64
                                 * @description Units in stock; `0` when inventory is not tracked.
                                 */
                                stock: number;
                                /** @description Stripe Product id once the catalog has been synchronized, or empty. */
                                stripe_product_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the seller last submitted the product for
                                 *     moderation, or `null`.
                                 */
                                submitted_at: string | null;
                                tags: string[];
                                /** @description Product type (taxonomy) id, or empty. */
                                type_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total products matching the filters, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/orders/{id}/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Guest checkout status
         * @description Returns a minimal order projection when supplied with the short-lived receipt capability issued at checkout. Buyer and provider identifiers are omitted.
         */
        get: {
            parameters: {
                query: {
                    receipt_token: string;
                };
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            amounts: {
                                currency: string;
                                /** Format: int64 */
                                discount_minor: number;
                                /** Format: int64 */
                                platform_fee_minor: number;
                                /**
                                 * Format: int64
                                 * @default 0
                                 */
                                shipping_minor: number;
                                /** Format: int64 */
                                subtotal_minor: number;
                                /** Format: int64 */
                                tax_minor: number;
                                /** Format: int64 */
                                total_minor: number;
                            };
                            order_id: string;
                            /** Format: date-time */
                            paid_at?: string;
                            /**
                             * @description Where the order stands against the provider's view of it.
                             * @enum {string}
                             */
                            reconciliation_status: "pending" | "awaiting_payment" | "reconciled" | "provider_error" | "payment_succeeded_awaiting_checkout" | "payment_failed" | "payment_processing" | "payment_requires_action" | "payment_canceled";
                            /** Format: date-time */
                            refunded_at?: string;
                            /** Format: uint32 */
                            schema_version: number;
                            /**
                             * @description Lifecycle state of the order.
                             * @enum {string}
                             */
                            status: "pending" | "checkout_started" | "completed" | "partially_refunded" | "refunded" | "failed";
                            subscription_cancel_at_period_end: boolean;
                            /** Format: date-time */
                            subscription_current_period_end?: string;
                            /**
                             * @description Stripe subscription lifecycle state; absent for a one-time order.
                             * @enum {string}
                             */
                            subscription_status?: "incomplete" | "incomplete_expired" | "trialing" | "active" | "past_due" | "unpaid" | "paused" | "canceled";
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/pricing/preview": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Preview configured offer
         * @description Evaluate a persisted active offer from validated customer inputs. Amounts are returned in integer minor units.
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @default {} */
                        inputs?: {
                            [key: string]: unknown;
                        };
                        offer_id: string;
                        /**
                         * Format: uint64
                         * @default 1
                         */
                        quantity?: number;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            amounts: {
                                currency: string;
                                /** Format: int64 */
                                discount_minor: number;
                                /** Format: int64 */
                                platform_fee_minor: number;
                                /**
                                 * Format: int64
                                 * @default 0
                                 */
                                shipping_minor: number;
                                /** Format: int64 */
                                subtotal_minor: number;
                                /** Format: int64 */
                                tax_minor: number;
                                /** Format: int64 */
                                total_minor: number;
                            };
                            components: {
                                component_id: string;
                                included: boolean;
                                key: string;
                                label: string;
                                /** Format: uint64 */
                                quantity: number;
                                reason: string;
                                required: boolean;
                                /** Format: int64 */
                                total_amount_minor: number;
                                /** Format: int64 */
                                unit_amount_minor: number;
                            }[];
                            inputs: {
                                [key: string]: unknown;
                            };
                            offer_id: string;
                            /** Format: uint32 */
                            offer_version: number;
                            /** Format: uint64 */
                            quantity: number;
                            /** Format: uint32 */
                            schema_version: number;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/purchases": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List own purchases */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            records: {
                                /** @description Email captured at checkout, or empty. */
                                buyer_email: string;
                                /** @description Checkout presentation the order was started with. */
                                checkout_mode: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description ISO 4217 currency of every amount on the order. */
                                currency: string;
                                /** Format: int64 */
                                discount_cents: number;
                                /** @description Stable order identifier. */
                                id: string;
                                /**
                                 * @description Immutable checkout snapshot: the offer id and version the order was
                                 *     priced against and the shipping amounts allowed at checkout.
                                 */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp payment was recorded, or `null`.
                                 */
                                payment_at: string | null;
                                /**
                                 * @description Payment provider: `stripe`, or `manual` for orders recorded outside a
                                 *     provider.
                                 */
                                provider: string;
                                /**
                                 * @description Latest payment state received from the provider.
                                 * @enum {string}
                                 */
                                provider_payment_status: "" | "succeeded" | "payment_failed" | "processing" | "requires_action" | "canceled";
                                /**
                                 * @description Where the order stands against the provider's view of it.
                                 * @enum {string}
                                 */
                                reconciliation_status: "pending" | "awaiting_payment" | "reconciled" | "provider_error" | "payment_succeeded_awaiting_checkout" | "payment_failed" | "payment_processing" | "payment_requires_action" | "payment_canceled";
                                /** @description Reason given for the last refund, or empty. */
                                refund_reason: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last refund applied, or `null`.
                                 */
                                refunded_at: string | null;
                                /**
                                 * Format: int64
                                 * @description Sum of succeeded refunds in minor units.
                                 */
                                refunded_total_cents: number;
                                /** Format: int64 */
                                shipping_cents: number;
                                /**
                                 * @description Lifecycle state of the order.
                                 * @enum {string}
                                 */
                                status: "pending" | "checkout_started" | "completed" | "partially_refunded" | "refunded" | "failed";
                                subscription_cancel_at_period_end: boolean;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the subscription was canceled, or `null`.
                                 */
                                subscription_canceled_at: string | null;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 end of the current billing period, or `null`.
                                 */
                                subscription_current_period_end: string | null;
                                /**
                                 * @description Stripe subscription lifecycle state for subscription orders, or empty.
                                 * @enum {string}
                                 */
                                subscription_status: "" | "incomplete" | "incomplete_expired" | "trialing" | "active" | "past_due" | "unpaid" | "paused" | "canceled";
                                /** Format: int64 */
                                subtotal_cents: number;
                                /** Format: int64 */
                                tax_cents: number;
                                /**
                                 * Format: int64
                                 * @description Final charged amount in minor units.
                                 */
                                total_cents: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total orders matching the query, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/purchases/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get own purchase */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Disputes raised against this order. Kept because the SSR order page
                             *     already shows a buyer their own disputes, and this endpoint is not
                             *     opted into the WebMCP manifest — only the list above is, and it
                             *     carries no nested rows.
                             */
                            disputes: {
                                /** Format: int64 */
                                amount_minor: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the dispute closed, or `null`.
                                 */
                                closed_at: string | null;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                currency: string;
                                /**
                                 * Format: int64
                                 * @description Provider timestamp (Unix seconds) of the event that last updated the
                                 *     dispute.
                                 */
                                event_created: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 evidence deadline, or `null`.
                                 */
                                evidence_due_by: string | null;
                                /** @description Stable dispute identifier. */
                                id: string;
                                livemode: boolean;
                                payment_intent_id: string;
                                /** @description Stripe Charge id the dispute was raised against, or empty. */
                                provider_charge_id: string;
                                /** @description Stripe Dispute id. */
                                provider_dispute_id: string;
                                purchase_id: string;
                                /** @description Provider's dispute reason, or empty. */
                                reason: string;
                                seller_account_id: string;
                                /**
                                 * @description Where the dispute stands with the card network.
                                 * @enum {string}
                                 */
                                status: "warning_needs_response" | "warning_under_review" | "warning_closed" | "needs_response" | "under_review" | "won" | "lost" | "prevented";
                                stripe_account_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            line_items: {
                                /** @description Offer component the line resolved, or empty for a whole-offer line. */
                                component_id: string;
                                /** @description The component condition as it was evaluated at checkout. */
                                condition_snapshot: {
                                    [key: string]: unknown;
                                };
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** Format: int64 */
                                discount_minor: number;
                                /** @description Stable line identifier. */
                                id: string;
                                /** @description Customer inputs the line was priced with, as submitted at checkout. */
                                input_snapshot: {
                                    [key: string]: unknown;
                                };
                                /** @description Offer the line was priced from, or empty for legacy lines. */
                                offer_id: string;
                                /**
                                 * Format: int64
                                 * @description Version of that offer at checkout.
                                 */
                                offer_version: number;
                                product_id: string;
                                /** @description Product name as it was at checkout. */
                                product_name: string;
                                purchase_id: string;
                                /** Format: int64 */
                                quantity: number;
                                seller_account_id: string;
                                /** @description Stripe Price id the line was charged through, or empty. */
                                stripe_price_id: string;
                                /** Format: int64 */
                                subtotal_minor: number;
                                /** Format: int64 */
                                tax_minor: number;
                                /** Format: int64 */
                                total_minor: number;
                                /** Format: int64 */
                                unit_amount_minor: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * @description One order as its **buyer** may read it.
                             *
                             *     The narrowest of the three order projections, and the one that matters
                             *     most: `GET /b/products/purchases` is opted in as the `list_my_purchases`
                             *     WebMCP tool, so every field here is handed to whatever agent runs in the
                             *     buyer's page.
                             *
                             *     Withheld, deliberately: the platform's economics (`platform_fee_cents`),
                             *     the seller's identity and Stripe account, the buyer's own provider handles
                             *     (`stripe_customer_id`, the PaymentIntent and Checkout Session ids — a
                             *     buyer never needs to quote one, and they are the provider's namespace, not
                             *     ours), and the reconciliation and payment-error diagnostics, which describe
                             *     our integration rather than their purchase.
                             */
                            purchase: {
                                /** @description Email captured at checkout, or empty. */
                                buyer_email: string;
                                /** @description Checkout presentation the order was started with. */
                                checkout_mode: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /** @description ISO 4217 currency of every amount on the order. */
                                currency: string;
                                /** Format: int64 */
                                discount_cents: number;
                                /** @description Stable order identifier. */
                                id: string;
                                /**
                                 * @description Immutable checkout snapshot: the offer id and version the order was
                                 *     priced against and the shipping amounts allowed at checkout.
                                 */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp payment was recorded, or `null`.
                                 */
                                payment_at: string | null;
                                /**
                                 * @description Payment provider: `stripe`, or `manual` for orders recorded outside a
                                 *     provider.
                                 */
                                provider: string;
                                /**
                                 * @description Latest payment state received from the provider.
                                 * @enum {string}
                                 */
                                provider_payment_status: "" | "succeeded" | "payment_failed" | "processing" | "requires_action" | "canceled";
                                /**
                                 * @description Where the order stands against the provider's view of it.
                                 * @enum {string}
                                 */
                                reconciliation_status: "pending" | "awaiting_payment" | "reconciled" | "provider_error" | "payment_succeeded_awaiting_checkout" | "payment_failed" | "payment_processing" | "payment_requires_action" | "payment_canceled";
                                /** @description Reason given for the last refund, or empty. */
                                refund_reason: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last refund applied, or `null`.
                                 */
                                refunded_at: string | null;
                                /**
                                 * Format: int64
                                 * @description Sum of succeeded refunds in minor units.
                                 */
                                refunded_total_cents: number;
                                /** Format: int64 */
                                shipping_cents: number;
                                /**
                                 * @description Lifecycle state of the order.
                                 * @enum {string}
                                 */
                                status: "pending" | "checkout_started" | "completed" | "partially_refunded" | "refunded" | "failed";
                                subscription_cancel_at_period_end: boolean;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the subscription was canceled, or `null`.
                                 */
                                subscription_canceled_at: string | null;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 end of the current billing period, or `null`.
                                 */
                                subscription_current_period_end: string | null;
                                /**
                                 * @description Stripe subscription lifecycle state for subscription orders, or empty.
                                 * @enum {string}
                                 */
                                subscription_status: "" | "incomplete" | "incomplete_expired" | "trialing" | "active" | "past_due" | "unpaid" | "paused" | "canceled";
                                /** Format: int64 */
                                subtotal_cents: number;
                                /** Format: int64 */
                                tax_cents: number;
                                /**
                                 * Format: int64
                                 * @description Final charged amount in minor units.
                                 */
                                total_cents: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            };
                            refunds: {
                                /** Format: int64 */
                                amount_minor: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp the refund reached a terminal state, or `null`.
                                 */
                                completed_at: string | null;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                currency: string;
                                /** @description Stable refund identifier. */
                                id: string;
                                /**
                                 * @description The provider's own state for the refund. Empty until the provider
                                 *     answers; `succeeded` for a refund recorded without a provider. Kept
                                 *     for the buyer because "has my money actually gone back" is the
                                 *     question this endpoint exists to answer — it is a state, not a handle.
                                 */
                                provider_status: string;
                                purchase_id: string;
                                /**
                                 * @description Ledger state.
                                 * @enum {string}
                                 */
                                status: "pending" | "provider_succeeded" | "succeeded" | "failed";
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/storefront/{product_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Storefront product and offers
         * @description Safe public product detail with active offer summaries and public pricing inputs; internal ownership, provider, and pricing-rule fields are omitted.
         */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    product_id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @default  */
                            description: string;
                            /** @enum {string} */
                            fulfillment_kind: "none" | "manual" | "download" | "entitlement" | "webhook";
                            id: string;
                            /** @default  */
                            image_url: string;
                            name: string;
                            offers: {
                                checkout: {
                                    /** @default false */
                                    allow_promotion_codes: boolean;
                                    /** @default [] */
                                    allowed_shipping_countries: string[];
                                    /** @default false */
                                    automatic_tax: boolean;
                                    /** @default false */
                                    collect_billing_address: boolean;
                                    /** @default false */
                                    collect_shipping_address: boolean;
                                    /** @default false */
                                    create_customer: boolean;
                                    /**
                                     * Format: int64
                                     * @description Maximum evaluated item total before provider discounts, tax, or shipping.
                                     * @default null
                                     */
                                    maximum_total_minor: number | null;
                                    /**
                                     * Format: int64
                                     * @description Minimum evaluated item total before provider discounts, tax, or shipping.
                                     * @default null
                                     */
                                    minimum_total_minor: number | null;
                                    /** @default false */
                                    require_terms_consent: boolean;
                                    /** @default [] */
                                    shipping_options: {
                                        /** Format: int64 */
                                        amount_minor: number;
                                        /** @default null */
                                        delivery_estimate: {
                                            /**
                                             * Format: uint32
                                             * @default null
                                             */
                                            maximum: number | null;
                                            /**
                                             * Format: uint32
                                             * @default null
                                             */
                                            minimum: number | null;
                                            /** @enum {string} */
                                            unit: "hour" | "day" | "business_day" | "week" | "month";
                                        } | null;
                                        display_name: string;
                                        /** @default  */
                                        stripe_shipping_rate_id: string;
                                        /**
                                         * @default unspecified
                                         * @enum {string}
                                         */
                                        tax_behavior: "unspecified" | "inclusive" | "exclusive";
                                    }[];
                                    /**
                                     * Format: uint32
                                     * @default 0
                                     */
                                    trial_days: number;
                                };
                                currency: string;
                                id: string;
                                /** Format: uint32 */
                                interval_count: number;
                                /** @enum {string} */
                                mode: "payment" | "subscription";
                                name: string;
                                /** @default [] */
                                payment_links: {
                                    id: string;
                                    /** @default  */
                                    preset_id: string;
                                    /**
                                     * @description Immutable server-resolved pricing captured when the reusable link was
                                     *     synchronized. This lets static pages display the link's actual price
                                     *     without issuing a runtime checkout or evaluating unrelated inputs.
                                     */
                                    pricing: {
                                        amounts: {
                                            currency: string;
                                            /** Format: int64 */
                                            discount_minor: number;
                                            /** Format: int64 */
                                            platform_fee_minor: number;
                                            /**
                                             * Format: int64
                                             * @default 0
                                             */
                                            shipping_minor: number;
                                            /** Format: int64 */
                                            subtotal_minor: number;
                                            /** Format: int64 */
                                            tax_minor: number;
                                            /** Format: int64 */
                                            total_minor: number;
                                        };
                                        components: {
                                            component_id: string;
                                            included: boolean;
                                            key: string;
                                            label: string;
                                            /** Format: uint64 */
                                            quantity: number;
                                            reason: string;
                                            required: boolean;
                                            /** Format: int64 */
                                            total_amount_minor: number;
                                            /** Format: int64 */
                                            unit_amount_minor: number;
                                        }[];
                                        inputs: {
                                            [key: string]: unknown;
                                        };
                                        offer_id: string;
                                        /** Format: uint32 */
                                        offer_version: number;
                                        /** Format: uint64 */
                                        quantity: number;
                                        /** Format: uint32 */
                                        schema_version: number;
                                    };
                                    url: string;
                                }[];
                                /** @enum {string} */
                                pricing_model: "fixed" | "components";
                                /**
                                 * @default null
                                 * @enum {string|null}
                                 */
                                recurring_interval: "day" | "week" | "month" | "year" | null;
                                variables: {
                                    /** @default [] */
                                    allowed_values: string[];
                                    /** @default null */
                                    default_value: unknown;
                                    /** @default  */
                                    help_text: string;
                                    key: string;
                                    /** @enum {string} */
                                    kind: "number" | "integer" | "boolean" | "date" | "date_time" | "select" | "multi_select" | "text";
                                    label: string;
                                    /** @default null */
                                    maximum: string | null;
                                    /**
                                     * Format: uint
                                     * @default null
                                     */
                                    maximum_length: number | null;
                                    /** @default null */
                                    minimum: string | null;
                                    /** @default false */
                                    required: boolean;
                                    /**
                                     * Format: int32
                                     * @default 0
                                     */
                                    sort_order: number;
                                    /** @default null */
                                    step: string | null;
                                    /**
                                     * @default public
                                     * @enum {string}
                                     */
                                    visibility: "public" | "hidden" | "admin_only";
                                }[];
                                /** Format: uint32 */
                                version: number;
                            }[];
                            /** Format: uint32 */
                            schema_version: number;
                            slug: string;
                            /** @default [] */
                            tags: string[];
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/storefront/config": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Browser-safe storefront configuration
         * @description Returns only a validated Stripe publishable key and mode. Secret keys, webhook secrets, provider ids, and API URLs are never exposed.
         */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            embedded_checkout_available: boolean;
                            /** Format: uint32 */
                            schema_version: number;
                            /**
                             * @description Which Stripe environment a validated publishable key belongs to.
                             * @enum {string}
                             */
                            stripe_mode?: "test" | "live";
                            stripe_publishable_key?: string;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/subscription": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Platform subscription status */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description The caller's subscription, or `null` when they have none. */
                            subscription: {
                                /** Format: int64 */
                                addon_d1_bytes: number;
                                /**
                                 * Format: int64
                                 * @description Purchased add-on quantities; `0` when none.
                                 */
                                addon_projects: number;
                                /** Format: int64 */
                                addon_r2_bytes: number;
                                /** Format: int64 */
                                addon_requests: number;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 end of the grace period after a failed payment, or `null`.
                                 */
                                grace_period_end: string | null;
                                /** @description Stable subscription identifier. */
                                id: string;
                                /** @description Plan name. */
                                plan: string;
                                /**
                                 * @description Stripe subscription lifecycle state.
                                 * @enum {string}
                                 */
                                status: "" | "incomplete" | "incomplete_expired" | "trialing" | "active" | "past_due" | "unpaid" | "paused" | "canceled";
                                /** @description Stripe Subscription id, or empty. */
                                stripe_subscription_id: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            } | null;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/types": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List product types for the authenticated builder */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /** @description Types on this page, newest first. */
                            records: {
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 creation timestamp.
                                 */
                                created_at: string;
                                description: string;
                                /** @description Stable type identifier. */
                                id: string;
                                /**
                                 * @description Whether the type is built in. System types are seeded by the block
                                 *     rather than created through the API.
                                 */
                                is_system: boolean;
                                name: string;
                                /**
                                 * Format: date-time
                                 * @description RFC 3339 timestamp of the last modification.
                                 */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total types, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/products/webhooks": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Receive signed Stripe webhook events
         * @description Public transport endpoint authenticated by the Stripe-Signature HMAC header. Raw request bytes are verified before parsing or applying any side effect.
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        account?: string;
                        data: {
                            object: Record<string, never>;
                        };
                        id?: string;
                        livemode?: boolean;
                        type: string;
                    } & {
                        [key: string]: unknown;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Present only when the event exhausted its retry budget. */
                            dead_letter?: boolean;
                            /**
                             * @description Present only when the event id had already been recorded, in which
                             *     case no side effect ran.
                             */
                            duplicate?: boolean;
                            received: boolean;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/storage/admin/api/buckets": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List every bucket (admin) */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Bucket names, from `repo::buckets::TABLE` — the single source of
                             *     truth for bucket existence. Not the blob namespace's folder list.
                             */
                            buckets: string[];
                            /**
                             * @description Whether more buckets are visible to the caller than `buckets` names.
                             *
                             *     Buckets are created self-service, so the admin view of this listing
                             *     grows with the deployment and is read up to a ceiling; this is how a
                             *     client tells a complete list from a prefix of one.
                             */
                            truncated: boolean;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/storage/admin/api/stats": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Storage totals (admin) */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description Rows in `repo::buckets::TABLE`, not folders in the blob namespace.
                             */
                            bucket_count: number;
                            /**
                             * Format: int64
                             * @description Objects in `Complete` status. A `Pending` reservation is not a file.
                             */
                            total_objects: number;
                            /**
                             * Format: int64
                             * @description Sum of `size` over the same set.
                             */
                            total_size_bytes: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/storage/api/buckets": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List buckets */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Bucket names, from `repo::buckets::TABLE` — the single source of
                             *     truth for bucket existence. Not the blob namespace's folder list.
                             */
                            buckets: string[];
                            /**
                             * @description Whether more buckets are visible to the caller than `buckets` names.
                             *
                             *     Buckets are created self-service, so the admin view of this listing
                             *     grows with the deployment and is read up to a ceiling; this is how a
                             *     client tells a complete list from a prefix of one.
                             */
                            truncated: boolean;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create bucket */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Always `true` — the handler answers this body only after both the
                             *     folder and the metadata row are in place.
                             */
                            created: boolean;
                            /** @description The bucket that now exists, echoed back from the request. */
                            name: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/storage/api/buckets/{name}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Delete bucket */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    name: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Always `true` — a delete that did not happen is an error status, not
                             *     `{"deleted": false}`.
                             */
                            deleted: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/storage/api/buckets/{name}/objects": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List objects */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                    prefix?: string;
                };
                header?: never;
                path: {
                    name: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Objects in this page. */
                            objects: {
                                content_type: string;
                                /** @description Object key. */
                                key: string;
                                /** Format: date-time */
                                last_modified: string;
                                /**
                                 * Format: int64
                                 * @description Size in bytes.
                                 */
                                size: number;
                            }[];
                            /**
                             * Format: int64
                             * @description Total number of objects matching the filter (across all pages).
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Upload file */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    name: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            bucket: string;
                            /**
                             * @description The stored key. For a multipart upload this is the key the handler
                             *     resolved, which may differ from the one the caller sent.
                             */
                            key: string;
                            /** @description Always `true`. */
                            uploaded: boolean;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/storage/api/buckets/{name}/objects/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Download file
         * @description Returns the raw object bytes with the stored Content-Type — not a JSON envelope.
         */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    key: string;
                    name: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content?: never;
                };
            };
        };
        put?: never;
        post?: never;
        /** Delete file */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    key: string;
                    name: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Always `true` — a delete that did not happen is an error status, not
                             *     `{"deleted": false}`.
                             */
                            deleted: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/storage/api/recent": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Recently viewed objects
         * @description Object-view audit rows, newest first — one row per tracked download, naming the object viewed and when. Not object metadata.
         */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** Format: int64 */
                            page: number;
                            /** Format: int64 */
                            page_size: number;
                            records: {
                                /** @description One object-view audit row, decoded. */
                                data: {
                                    /** @description Bucket holding the viewed object. */
                                    bucket: string;
                                    created_at: string;
                                    id: string;
                                    /** @description Object key within the bucket. */
                                    key: string;
                                    updated_at: string;
                                    /** @description The viewer. */
                                    user_id: string;
                                    /** @description RFC 3339 instant of the view. */
                                    viewed_at: string;
                                };
                                id: string;
                            }[];
                            /** Format: int64 */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/storage/api/search": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Search objects */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** Format: int64 */
                            page: number;
                            /** Format: int64 */
                            page_size: number;
                            records: {
                                /** @description One object-metadata row, decoded. */
                                data: {
                                    /** @description Bucket name; `(bucket, key)` is unique. */
                                    bucket: string;
                                    content_type: string;
                                    created_at: string;
                                    id: string;
                                    /** @description Object key within the bucket. */
                                    key: string;
                                    /**
                                     * Format: int64
                                     * @description Size in bytes. `i64_field` so a TEXT-stored number still counts
                                     *     toward the quota rather than reading as zero.
                                     */
                                    size: number;
                                    /**
                                     * @description `Pending` while the storage upload is in flight, `Complete` after.
                                     *     Quota accounting counts both; user-facing search and admin stats see
                                     *     only `Complete`.
                                     */
                                    status: "pending" | "complete";
                                    updated_at: string;
                                    /**
                                     * @description When the upload was reserved — the timestamp the object browser
                                     *     renders as "modified", and the one `list_stale_pending` compares.
                                     */
                                    uploaded_at: string;
                                    uploaded_by: string;
                                };
                                id: string;
                            }[];
                            /** Format: int64 */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/tickets/api/admin/retention/prune": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Run bounded ticket retention */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description Expired analyses removed.
                             */
                            analyses_deleted: number;
                            /**
                             * @description Whether every delete in the pass succeeded. A `false` here is answered
                             *     with HTTP 503 so a scheduler retries.
                             */
                            complete: boolean;
                            /**
                             * @description Names of the deletes that failed (`"analyses"`, `"events"`,
                             *     `"tickets"`, `"rate-counters"`).
                             */
                            errors: string[];
                            /**
                             * Format: int64
                             * @description Expired audit events removed.
                             */
                            events_deleted: number;
                            /**
                             * Format: int64
                             * @description Stale submission rate-limit counters removed.
                             */
                            rate_counters_deleted: number;
                            /**
                             * Format: int64
                             * @description Expired tickets removed. Tickets under legal hold never expire.
                             */
                            tickets_deleted: number;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/tickets/api/admin/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Queue and security readiness */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Whether an audit-timeline write has failed since the flag was last
                             *     cleared. While true, the timeline may be incomplete.
                             */
                            audit_degraded: boolean;
                            /** @description The last retention pass, or `null` when none has run. */
                            last_maintenance: {
                                /**
                                 * @description Comma-joined names of the deletes that failed on the last pass, or `""`
                                 *     when it completed.
                                 */
                                last_prune_error: string;
                                /** @description RFC 3339 timestamp of the last pass, or `null` before the first one. */
                                last_pruned_at: string | null;
                                /** @description `YYYY-MM-DD` of the last pass, or `""` before the first one. */
                                last_pruned_day: string;
                            } | null;
                            /**
                             * Format: int64
                             * @description Tickets still in the `"new"` state.
                             */
                            new_tickets: number;
                            /**
                             * Format: int64
                             * @description Tickets in `"new"`, `"triaged"` or `"investigating"`.
                             */
                            open_tickets: number;
                            /**
                             * @description Whether protected public reporting is currently able to accept a
                             *     submission, and what is missing when it is not.
                             */
                            security: {
                                /** @description Whether the tickets block itself is enabled. */
                                block_enabled: boolean;
                                /** @description Whether at least one active, publicly visible ticket type exists. */
                                has_public_type: boolean;
                                /** @description Whether the abuse-digest identity secret is configured. */
                                identity_secret_configured: boolean;
                                /** @description Whether both submission rate limits have a positive cap and window. */
                                positive_limits: boolean;
                                /** @description Whether the operator has turned public submissions on. */
                                public_enabled: boolean;
                                /** @description Whether all of the checks below passed. */
                                ready: boolean;
                                /**
                                 * @description Human-readable reason for each failed check, in check order. Empty when
                                 *     `ready`.
                                 */
                                reasons: string[];
                                /** @description Whether a Turnstile widget site key is configured. */
                                site_key_configured: boolean;
                                /** @description Whether a Turnstile Siteverify secret is configured. */
                                turnstile_secret_configured: boolean;
                            };
                            /**
                             * Format: int64
                             * @description Tickets at `"urgent"` priority, in any state.
                             */
                            urgent_tickets: number;
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/tickets/api/admin/tickets": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List bounded ticket summaries */
        get: {
            parameters: {
                query?: {
                    assignee_id?: string | null;
                    page?: number;
                    page_size?: number;
                    priority?: string | null;
                    source?: string | null;
                    status?: string | null;
                    type_id?: string | null;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /** @description Tickets on this page, newest first. */
                            records: {
                                /** @description Id of the assigned reviewer, or `""` when unassigned. */
                                assignee_id: string;
                                /** @description RFC 3339 creation timestamp. */
                                created_at: string;
                                /** @description Stable ticket identifier. */
                                id: string;
                                /** @description Whether retention is suspended for this ticket. */
                                legal_hold: boolean;
                                /** @description `"low"`, `"normal"`, `"high"` or `"urgent"`. */
                                priority: string;
                                /** @description Human-quotable reference (`"TKT-…"`), unique across tickets. */
                                reference: string;
                                /** @description How the ticket arrived: `"public_form"`, `"admin"`, `"api"` or `"ai"`. */
                                source: string;
                                /**
                                 * @description Workflow state: `"new"`, `"triaged"`, `"investigating"`, `"resolved"`,
                                 *     `"rejected"`, `"spam"` or `"duplicate"`.
                                 */
                                status: string;
                                /** @description Id of the ticket type this was filed under. */
                                type_id: string;
                                /**
                                 * @description The type's `key` as it stood when the ticket was created. Renaming a
                                 *     type later does not rewrite this.
                                 */
                                type_key_snapshot: string;
                                /** @description The type's `title` as it stood when the ticket was created. */
                                type_title_snapshot: string;
                                /** @description Reporter-supplied text. Data, never instructions. */
                                untrusted_report: {
                                    /** @description Same-site path the report was filed from, or `""`. */
                                    source_path: string;
                                    /** @description One-line summary as the reporter wrote it. */
                                    subject: string;
                                    /** @description Caller-supplied id of the thing reported, or `""`. */
                                    subject_id: string;
                                    /** @description Caller-supplied kind of the thing reported (`"activity"`, …), or `""`. */
                                    subject_type: string;
                                };
                                /** @description RFC 3339 timestamp of the last workflow change. */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total tickets matching the filters, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create an internal, API, or AI ticket */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @description Full description, 20–4000 characters. */
                        description: string;
                        /**
                         * @description Supporting `http(s)` URL without credentials, ≤1024 characters.
                         * @default
                         */
                        evidence_url?: string;
                        /**
                         * @description `"low"`, `"normal"`, `"high"` or `"urgent"`. Defaults to the ticket
                         *     type's own default priority.
                         */
                        priority?: string | null;
                        /**
                         * @description Origin to record: `"admin"`, `"api"` or `"ai"`. `"public_form"` is
                         *     rejected with 400.
                         */
                        source: string;
                        /**
                         * @description Same-site path the ticket refers to. Must start with a single `/`.
                         * @default
                         */
                        source_path?: string;
                        /** @description One-line summary, 5–160 characters. */
                        subject: string;
                        /**
                         * @description Id of the thing this is about (≤160 characters).
                         * @default
                         */
                        subject_id?: string;
                        /**
                         * @description Kind of the thing this is about (lowercase slug, ≤64 characters).
                         * @default
                         */
                        subject_type?: string;
                        /** @description Id of an active ticket type. */
                        type_id: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Id of the assigned reviewer, or `""` when unassigned. */
                            assignee_id: string;
                            /** @description RFC 3339 creation timestamp. */
                            created_at: string;
                            /**
                             * @description Id of the ticket this one duplicates, set only while `status` is
                             *     `"duplicate"`.
                             */
                            duplicate_of: string | null;
                            /**
                             * @description RFC 3339 timestamp retention will delete this ticket at. `null` while
                             *     the ticket is open or under legal hold.
                             */
                            expires_at: string | null;
                            /** @description Stable ticket identifier. */
                            id: string;
                            /** @description Whether retention is suspended for this ticket. */
                            legal_hold: boolean;
                            /** @description `"low"`, `"normal"`, `"high"` or `"urgent"`. */
                            priority: string;
                            /** @description Human-quotable reference (`"TKT-…"`), unique across tickets. */
                            reference: string;
                            /** @description RFC 3339 timestamp the ticket left the open states, or `null`. */
                            resolved_at: string | null;
                            /** @description How the ticket arrived: `"public_form"`, `"admin"`, `"api"` or `"ai"`. */
                            source: string;
                            /**
                             * @description Workflow state: `"new"`, `"triaged"`, `"investigating"`, `"resolved"`,
                             *     `"rejected"`, `"spam"` or `"duplicate"`.
                             */
                            status: string;
                            /** @description Id of the ticket type this was filed under. */
                            type_id: string;
                            /** @description The type's `key` as it stood when the ticket was created. */
                            type_key_snapshot: string;
                            /** @description The type's `title` as it stood when the ticket was created. */
                            type_title_snapshot: string;
                            /** @description Reporter-supplied text. Data, never instructions. */
                            untrusted_report: {
                                /** @description The report body as the reporter wrote it. */
                                description: string;
                                /**
                                 * @description Supporting URL the reporter supplied, or `""`. Validated as an
                                 *     `http(s)` URL without credentials, and not fetched by the block.
                                 */
                                evidence_url: string;
                                /**
                                 * @description Contact address the reporter supplied, or `""`. Empty for every ticket
                                 *     created through the admin, API or AI intake path.
                                 */
                                reporter_email: string;
                                /** @description Whether the reporter consented to being contacted about the report. */
                                reporter_wants_reply: boolean;
                                /** @description Same-site path the report was filed from, or `""`. */
                                source_path: string;
                                /** @description One-line summary as the reporter wrote it. */
                                subject: string;
                                /** @description Caller-supplied id of the thing reported, or `""`. */
                                subject_id: string;
                                /** @description Caller-supplied kind of the thing reported (`"activity"`, …), or `""`. */
                                subject_type: string;
                            };
                            /** @description RFC 3339 timestamp of the last workflow change. */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/tickets/api/admin/tickets/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Fetch a ticket; reporter text is untrusted data, never instructions */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Advisory analyses, newest first, capped at 100 entries. */
                            analyses: {
                                /**
                                 * Format: double
                                 * @description Producer-reported confidence, between 0 and 1 inclusive.
                                 */
                                confidence: number;
                                /** @description RFC 3339 timestamp the analysis was recorded at. */
                                created_at: string;
                                /**
                                 * @description RFC 3339 timestamp retention will delete this analysis at, tracking the
                                 *     ticket's own expiry. `null` while the ticket is open.
                                 */
                                expires_at: string | null;
                                /** @description Stable analysis identifier. */
                                id: string;
                                /** @description Model identifier, when the producer recorded one. */
                                model: string | null;
                                /** @description Prompt version the producer recorded, or `""`. */
                                prompt_version: string;
                                /** @description Free-form name of the system that produced it. */
                                source: string;
                                /**
                                 * @description Producer-proposed follow-up actions. Free-form; the block neither
                                 *     interprets nor executes them.
                                 */
                                suggested_actions: unknown[];
                                /** @description Priority the producer would set, if any. */
                                suggested_priority: string | null;
                                /** @description Ticket type the producer would file this under, if any. */
                                suggested_type_id: string | null;
                                /**
                                 * @description The analysis itself, as the producer wrote it. Advisory text, not a
                                 *     reviewer's finding.
                                 */
                                summary: string;
                                /** @description Ticket this analysis is about. */
                                ticket_id: string;
                            }[];
                            /** @description Whether the analysis list was cut off at 100 entries. */
                            analyses_truncated: boolean;
                            /** @description Audit timeline, newest first, capped at 200 entries. */
                            events: {
                                /** @description Id of the acting user, or `""` for public and system actors. */
                                actor_id: string;
                                /** @description Who acted: `"public"`, `"admin"`, `"api"`, `"ai"` or `"system"`. */
                                actor_type: string;
                                /**
                                 * @description Reviewer-authored text: the note for a `"note"` event, the reason for a
                                 *     workflow change, `""` otherwise. Never carries reporter text.
                                 */
                                body: string;
                                /** @description RFC 3339 timestamp the event was recorded at. */
                                created_at: string;
                                /**
                                 * @description `"created"`, `"note"`, `"workflow_updated"`, or the status the ticket
                                 *     moved to.
                                 */
                                event_type: string;
                                /**
                                 * @description RFC 3339 timestamp retention will delete this entry at, tracking the
                                 *     ticket's own expiry. `null` while the ticket is open.
                                 */
                                expires_at: string | null;
                                /** @description Stable event identifier. */
                                id: string;
                                /**
                                 * @description Structured context for the change (requested status, priority,
                                 *     assignee, duplicate target, legal hold).
                                 */
                                metadata: {
                                    [key: string]: unknown;
                                };
                                /** @description Ticket this event belongs to. */
                                ticket_id: string;
                            }[];
                            /** @description Whether the timeline was cut off at 200 entries. */
                            events_truncated: boolean;
                            /** @description The ticket, with reporter text grouped under `untrusted_report`. */
                            ticket: {
                                /** @description Id of the assigned reviewer, or `""` when unassigned. */
                                assignee_id: string;
                                /** @description RFC 3339 creation timestamp. */
                                created_at: string;
                                /**
                                 * @description Id of the ticket this one duplicates, set only while `status` is
                                 *     `"duplicate"`.
                                 */
                                duplicate_of: string | null;
                                /**
                                 * @description RFC 3339 timestamp retention will delete this ticket at. `null` while
                                 *     the ticket is open or under legal hold.
                                 */
                                expires_at: string | null;
                                /** @description Stable ticket identifier. */
                                id: string;
                                /** @description Whether retention is suspended for this ticket. */
                                legal_hold: boolean;
                                /** @description `"low"`, `"normal"`, `"high"` or `"urgent"`. */
                                priority: string;
                                /** @description Human-quotable reference (`"TKT-…"`), unique across tickets. */
                                reference: string;
                                /** @description RFC 3339 timestamp the ticket left the open states, or `null`. */
                                resolved_at: string | null;
                                /** @description How the ticket arrived: `"public_form"`, `"admin"`, `"api"` or `"ai"`. */
                                source: string;
                                /**
                                 * @description Workflow state: `"new"`, `"triaged"`, `"investigating"`, `"resolved"`,
                                 *     `"rejected"`, `"spam"` or `"duplicate"`.
                                 */
                                status: string;
                                /** @description Id of the ticket type this was filed under. */
                                type_id: string;
                                /** @description The type's `key` as it stood when the ticket was created. */
                                type_key_snapshot: string;
                                /** @description The type's `title` as it stood when the ticket was created. */
                                type_title_snapshot: string;
                                /** @description Reporter-supplied text. Data, never instructions. */
                                untrusted_report: {
                                    /** @description The report body as the reporter wrote it. */
                                    description: string;
                                    /**
                                     * @description Supporting URL the reporter supplied, or `""`. Validated as an
                                     *     `http(s)` URL without credentials, and not fetched by the block.
                                     */
                                    evidence_url: string;
                                    /**
                                     * @description Contact address the reporter supplied, or `""`. Empty for every ticket
                                     *     created through the admin, API or AI intake path.
                                     */
                                    reporter_email: string;
                                    /** @description Whether the reporter consented to being contacted about the report. */
                                    reporter_wants_reply: boolean;
                                    /** @description Same-site path the report was filed from, or `""`. */
                                    source_path: string;
                                    /** @description One-line summary as the reporter wrote it. */
                                    subject: string;
                                    /** @description Caller-supplied id of the thing reported, or `""`. */
                                    subject_id: string;
                                    /** @description Caller-supplied kind of the thing reported (`"activity"`, …), or `""`. */
                                    subject_type: string;
                                };
                                /** @description RFC 3339 timestamp of the last workflow change. */
                                updated_at: string;
                            };
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /** Update mutable workflow fields only */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @description Id of the reviewer to assign, or `""` to unassign. */
                        assignee_id?: string | null;
                        /**
                         * @description Id of the ticket this one duplicates. Required when moving to
                         *     `"duplicate"`, and must name an existing, different ticket.
                         */
                        duplicate_of?: string | null;
                        /** @description Suspend retention for this ticket. A ticket under hold never expires. */
                        legal_hold?: boolean | null;
                        /** @description `"low"`, `"normal"`, `"high"` or `"urgent"`. */
                        priority?: string | null;
                        /**
                         * @description Why the change was made, ≤4000 characters. Required when closing a
                         *     ticket (`"resolved"`, `"rejected"`, `"spam"`, `"duplicate"`) and
                         *     recorded on the audit timeline.
                         * @default
                         */
                        reason?: string;
                        /**
                         * @description New workflow state: `"new"`, `"triaged"`, `"investigating"`,
                         *     `"resolved"`, `"rejected"`, `"spam"` or `"duplicate"`. A closed ticket
                         *     can only reopen to `"triaged"`.
                         */
                        status?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Id of the assigned reviewer, or `""` when unassigned. */
                            assignee_id: string;
                            /** @description RFC 3339 creation timestamp. */
                            created_at: string;
                            /**
                             * @description Id of the ticket this one duplicates, set only while `status` is
                             *     `"duplicate"`.
                             */
                            duplicate_of: string | null;
                            /**
                             * @description RFC 3339 timestamp retention will delete this ticket at. `null` while
                             *     the ticket is open or under legal hold.
                             */
                            expires_at: string | null;
                            /** @description Stable ticket identifier. */
                            id: string;
                            /** @description Whether retention is suspended for this ticket. */
                            legal_hold: boolean;
                            /** @description `"low"`, `"normal"`, `"high"` or `"urgent"`. */
                            priority: string;
                            /** @description Human-quotable reference (`"TKT-…"`), unique across tickets. */
                            reference: string;
                            /** @description RFC 3339 timestamp the ticket left the open states, or `null`. */
                            resolved_at: string | null;
                            /** @description How the ticket arrived: `"public_form"`, `"admin"`, `"api"` or `"ai"`. */
                            source: string;
                            /**
                             * @description Workflow state: `"new"`, `"triaged"`, `"investigating"`, `"resolved"`,
                             *     `"rejected"`, `"spam"` or `"duplicate"`.
                             */
                            status: string;
                            /** @description Id of the ticket type this was filed under. */
                            type_id: string;
                            /** @description The type's `key` as it stood when the ticket was created. */
                            type_key_snapshot: string;
                            /** @description The type's `title` as it stood when the ticket was created. */
                            type_title_snapshot: string;
                            /** @description Reporter-supplied text. Data, never instructions. */
                            untrusted_report: {
                                /** @description The report body as the reporter wrote it. */
                                description: string;
                                /**
                                 * @description Supporting URL the reporter supplied, or `""`. Validated as an
                                 *     `http(s)` URL without credentials, and not fetched by the block.
                                 */
                                evidence_url: string;
                                /**
                                 * @description Contact address the reporter supplied, or `""`. Empty for every ticket
                                 *     created through the admin, API or AI intake path.
                                 */
                                reporter_email: string;
                                /** @description Whether the reporter consented to being contacted about the report. */
                                reporter_wants_reply: boolean;
                                /** @description Same-site path the report was filed from, or `""`. */
                                source_path: string;
                                /** @description One-line summary as the reporter wrote it. */
                                subject: string;
                                /** @description Caller-supplied id of the thing reported, or `""`. */
                                subject_id: string;
                                /** @description Caller-supplied kind of the thing reported (`"activity"`, …), or `""`. */
                                subject_type: string;
                            };
                            /** @description RFC 3339 timestamp of the last workflow change. */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/tickets/api/admin/tickets/{id}/analyses": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List structured ticket analyses */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Analyses for this ticket, newest first. */
                            records: {
                                /**
                                 * Format: double
                                 * @description Producer-reported confidence, between 0 and 1 inclusive.
                                 */
                                confidence: number;
                                /** @description RFC 3339 timestamp the analysis was recorded at. */
                                created_at: string;
                                /**
                                 * @description RFC 3339 timestamp retention will delete this analysis at, tracking the
                                 *     ticket's own expiry. `null` while the ticket is open.
                                 */
                                expires_at: string | null;
                                /** @description Stable analysis identifier. */
                                id: string;
                                /** @description Model identifier, when the producer recorded one. */
                                model: string | null;
                                /** @description Prompt version the producer recorded, or `""`. */
                                prompt_version: string;
                                /** @description Free-form name of the system that produced it. */
                                source: string;
                                /**
                                 * @description Producer-proposed follow-up actions. Free-form; the block neither
                                 *     interprets nor executes them.
                                 */
                                suggested_actions: unknown[];
                                /** @description Priority the producer would set, if any. */
                                suggested_priority: string | null;
                                /** @description Ticket type the producer would file this under, if any. */
                                suggested_type_id: string | null;
                                /**
                                 * @description The analysis itself, as the producer wrote it. Advisory text, not a
                                 *     reviewer's finding.
                                 */
                                summary: string;
                                /** @description Ticket this analysis is about. */
                                ticket_id: string;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        /** Append advisory structured analysis */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * Format: double
                         * @description Reported confidence, between 0 and 1 inclusive.
                         */
                        confidence: number;
                        /** @description Model identifier, ≤160 characters. */
                        model?: string | null;
                        /**
                         * @description Prompt version, ≤80 characters.
                         * @default
                         */
                        prompt_version?: string;
                        /** @description Name of the system producing the analysis, 1–80 characters. */
                        source: string;
                        /**
                         * @description Proposed follow-up actions, ≤8192 bytes once encoded. Stored verbatim;
                         *     the block neither interprets nor executes them.
                         * @default []
                         */
                        suggested_actions?: unknown[];
                        /** @description `"low"`, `"normal"`, `"high"` or `"urgent"`. */
                        suggested_priority?: string | null;
                        /** @description Ticket type to suggest. Must name an active type. */
                        suggested_type_id?: string | null;
                        /** @description The analysis itself, 1–4000 characters. */
                        summary: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: double
                             * @description Producer-reported confidence, between 0 and 1 inclusive.
                             */
                            confidence: number;
                            /** @description RFC 3339 timestamp the analysis was recorded at. */
                            created_at: string;
                            /**
                             * @description RFC 3339 timestamp retention will delete this analysis at, tracking the
                             *     ticket's own expiry. `null` while the ticket is open.
                             */
                            expires_at: string | null;
                            /** @description Stable analysis identifier. */
                            id: string;
                            /** @description Model identifier, when the producer recorded one. */
                            model: string | null;
                            /** @description Prompt version the producer recorded, or `""`. */
                            prompt_version: string;
                            /** @description Free-form name of the system that produced it. */
                            source: string;
                            /**
                             * @description Producer-proposed follow-up actions. Free-form; the block neither
                             *     interprets nor executes them.
                             */
                            suggested_actions: unknown[];
                            /** @description Priority the producer would set, if any. */
                            suggested_priority: string | null;
                            /** @description Ticket type the producer would file this under, if any. */
                            suggested_type_id: string | null;
                            /**
                             * @description The analysis itself, as the producer wrote it. Advisory text, not a
                             *     reviewer's finding.
                             */
                            summary: string;
                            /** @description Ticket this analysis is about. */
                            ticket_id: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/tickets/api/admin/tickets/{id}/notes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Append an internal ticket note */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * @description Internal note, 1–4000 characters. Appended to the audit timeline; the
                         *     original report is never edited.
                         */
                        note: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Id of the acting user, or `""` for public and system actors. */
                            actor_id: string;
                            /** @description Who acted: `"public"`, `"admin"`, `"api"`, `"ai"` or `"system"`. */
                            actor_type: string;
                            /**
                             * @description Reviewer-authored text: the note for a `"note"` event, the reason for a
                             *     workflow change, `""` otherwise. Never carries reporter text.
                             */
                            body: string;
                            /** @description RFC 3339 timestamp the event was recorded at. */
                            created_at: string;
                            /**
                             * @description `"created"`, `"note"`, `"workflow_updated"`, or the status the ticket
                             *     moved to.
                             */
                            event_type: string;
                            /**
                             * @description RFC 3339 timestamp retention will delete this entry at, tracking the
                             *     ticket's own expiry. `null` while the ticket is open.
                             */
                            expires_at: string | null;
                            /** @description Stable event identifier. */
                            id: string;
                            /**
                             * @description Structured context for the change (requested status, priority,
                             *     assignee, duplicate target, legal hold).
                             */
                            metadata: {
                                [key: string]: unknown;
                            };
                            /** @description Ticket this event belongs to. */
                            ticket_id: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/tickets/api/admin/types": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List ticket types */
        get: {
            parameters: {
                query?: {
                    page?: number;
                    page_size?: number;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: int64
                             * @description 1-based index of this page.
                             */
                            page: number;
                            /**
                             * Format: int64
                             * @description Rows per page used to compute `page`.
                             */
                            page_size: number;
                            /**
                             * @description Ticket types on this page, ordered by `sort_order` then `title`.
                             *     Inactive and non-public types are included.
                             */
                            records: {
                                /** @description Whether the type accepts new tickets at all. */
                                active: boolean;
                                /** @description RFC 3339 creation timestamp. */
                                created_at: string;
                                /**
                                 * @description Priority applied to tickets filed under this type: `"low"`,
                                 *     `"normal"`, `"high"` or `"urgent"`.
                                 */
                                default_priority: string;
                                /** @description Short explanation of what belongs under this type. */
                                description: string;
                                /** @description Review track: `"none"`, `"legal"`, `"privacy"` or `"safety"`. */
                                escalation_kind: string;
                                /** @description Longer guidance shown on the public form. */
                                guidance: string;
                                /** @description Stable ticket type identifier. */
                                id: string;
                                /** @description Immutable lowercase slug identifying the type. */
                                key: string;
                                /** @description Whether the public form offers this type. */
                                public_visible: boolean;
                                /** @description Whether the form asks for an evidence URL. */
                                requests_evidence: boolean;
                                /** @description Whether a reporter must supply an email address and consent to a reply. */
                                requires_contact: boolean;
                                /**
                                 * Format: int64
                                 * @description Ordering weight on the public form and in the admin list.
                                 */
                                sort_order: number;
                                /** @description Title shown to reporters and reviewers. */
                                title: string;
                                /** @description RFC 3339 timestamp of the last modification. */
                                updated_at: string;
                            }[];
                            /**
                             * Format: int64
                             * @description Total ticket types defined, across all pages.
                             */
                            total_count: number;
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create ticket type */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * @description Whether the type accepts new tickets.
                         * @default true
                         */
                        active?: boolean;
                        /**
                         * @description Priority applied to tickets filed under this type: `"low"`,
                         *     `"normal"`, `"high"` or `"urgent"`.
                         * @default normal
                         */
                        default_priority?: string;
                        /**
                         * @description Short explanation of what belongs under this type, ≤500 characters.
                         * @default
                         */
                        description?: string;
                        /**
                         * @description Review track: `"none"`, `"legal"`, `"privacy"` or `"safety"`.
                         * @default none
                         */
                        escalation_kind?: string;
                        /**
                         * @description Longer guidance shown on the public form, ≤1000 characters.
                         * @default
                         */
                        guidance?: string;
                        /** @description Immutable lowercase slug, 2–48 characters, identifying the type. */
                        key: string;
                        /**
                         * @description Whether the public form offers this type.
                         * @default false
                         */
                        public_visible?: boolean;
                        /**
                         * @description Whether the form asks for an evidence URL.
                         * @default false
                         */
                        requests_evidence?: boolean;
                        /**
                         * @description Whether a reporter must supply an email address and consent to a reply.
                         * @default false
                         */
                        requires_contact?: boolean;
                        /**
                         * Format: int64
                         * @description Ordering weight, between -1000000 and 1000000.
                         * @default 0
                         */
                        sort_order?: number;
                        /** @description Title shown to reporters and reviewers, 2–80 characters. */
                        title: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Whether the type accepts new tickets at all. */
                            active: boolean;
                            /** @description RFC 3339 creation timestamp. */
                            created_at: string;
                            /**
                             * @description Priority applied to tickets filed under this type: `"low"`,
                             *     `"normal"`, `"high"` or `"urgent"`.
                             */
                            default_priority: string;
                            /** @description Short explanation of what belongs under this type. */
                            description: string;
                            /** @description Review track: `"none"`, `"legal"`, `"privacy"` or `"safety"`. */
                            escalation_kind: string;
                            /** @description Longer guidance shown on the public form. */
                            guidance: string;
                            /** @description Stable ticket type identifier. */
                            id: string;
                            /** @description Immutable lowercase slug identifying the type. */
                            key: string;
                            /** @description Whether the public form offers this type. */
                            public_visible: boolean;
                            /** @description Whether the form asks for an evidence URL. */
                            requests_evidence: boolean;
                            /** @description Whether a reporter must supply an email address and consent to a reply. */
                            requires_contact: boolean;
                            /**
                             * Format: int64
                             * @description Ordering weight on the public form and in the admin list.
                             */
                            sort_order: number;
                            /** @description Title shown to reporters and reviewers. */
                            title: string;
                            /** @description RFC 3339 timestamp of the last modification. */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/tickets/api/admin/types/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /** Update or deactivate ticket type */
        patch: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                };
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * @description Whether the type accepts new tickets. Deactivating the last public type
                         *     while public submissions are on is rejected with 409.
                         */
                        active?: boolean | null;
                        /** @description `"low"`, `"normal"`, `"high"` or `"urgent"`. */
                        default_priority?: string | null;
                        /** @description New description, ≤500 characters. */
                        description?: string | null;
                        /** @description `"none"`, `"legal"`, `"privacy"` or `"safety"`. */
                        escalation_kind?: string | null;
                        /** @description New guidance, ≤1000 characters. */
                        guidance?: string | null;
                        /**
                         * @description Accepted only when it equals the stored key — the key is immutable.
                         * @default null
                         */
                        key?: string | null;
                        /**
                         * @description Whether the public form offers this type. Removing the last public type
                         *     while public submissions are on is rejected with 409.
                         */
                        public_visible?: boolean | null;
                        /** @description Whether the form asks for an evidence URL. */
                        requests_evidence?: boolean | null;
                        /** @description Whether a reporter must supply an email address and consent to a reply. */
                        requires_contact?: boolean | null;
                        /**
                         * Format: int64
                         * @description Ordering weight, between -1000000 and 1000000.
                         */
                        sort_order?: number | null;
                        /** @description New title, 2–80 characters. */
                        title?: string | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Whether the type accepts new tickets at all. */
                            active: boolean;
                            /** @description RFC 3339 creation timestamp. */
                            created_at: string;
                            /**
                             * @description Priority applied to tickets filed under this type: `"low"`,
                             *     `"normal"`, `"high"` or `"urgent"`.
                             */
                            default_priority: string;
                            /** @description Short explanation of what belongs under this type. */
                            description: string;
                            /** @description Review track: `"none"`, `"legal"`, `"privacy"` or `"safety"`. */
                            escalation_kind: string;
                            /** @description Longer guidance shown on the public form. */
                            guidance: string;
                            /** @description Stable ticket type identifier. */
                            id: string;
                            /** @description Immutable lowercase slug identifying the type. */
                            key: string;
                            /** @description Whether the public form offers this type. */
                            public_visible: boolean;
                            /** @description Whether the form asks for an evidence URL. */
                            requests_evidence: boolean;
                            /** @description Whether a reporter must supply an email address and consent to a reply. */
                            requires_contact: boolean;
                            /**
                             * Format: int64
                             * @description Ordering weight on the public form and in the admin list.
                             */
                            sort_order: number;
                            /** @description Title shown to reporters and reviewers. */
                            title: string;
                            /** @description RFC 3339 timestamp of the last modification. */
                            updated_at: string;
                        };
                    };
                };
            };
        };
        trace?: never;
    };
    "/b/tickets/api/submissions": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Protected public ticket creation */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @description Full description, 20–4000 characters. */
                        description: string;
                        /**
                         * @description Supporting `http(s)` URL without credentials, ≤1024 characters.
                         * @default
                         */
                        evidence_url?: string;
                        /**
                         * @description Single-use token issued by the public form and valid for a bounded
                         *     window.
                         */
                        form_token: string;
                        /**
                         * @description Reporter's email address. Required by ticket types that set
                         *     `requires_contact`.
                         * @default
                         */
                        reporter_email?: string;
                        /**
                         * @description Whether the reporter consents to being contacted. Requires
                         *     `reporter_email`.
                         * @default false
                         */
                        reporter_wants_reply?: boolean;
                        /**
                         * @description Same-site path the report is about. Must start with a single `/`.
                         * @default
                         */
                        source_path?: string;
                        /** @description One-line summary, 5–160 characters. */
                        subject: string;
                        /**
                         * @description Id of the thing being reported (≤160 characters).
                         * @default
                         */
                        subject_id?: string;
                        /**
                         * @description Kind of the thing being reported (lowercase slug, ≤64 characters).
                         * @default
                         */
                        subject_type?: string;
                        /** @description Cloudflare Turnstile response token from the form's challenge widget. */
                        turnstile_token: string;
                        /** @description Id of an active, publicly visible ticket type. */
                        type_id: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Human-readable confirmation to show the reporter. */
                            message: string;
                            /**
                             * @description The new ticket's quotable reference (`"TKT-…"`), or `""` when no
                             *     reference was allocated.
                             */
                            reference: string;
                            /** @description Always `"received"`. */
                            status: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/vector/api/{index}/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Delete a single vector */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    id: string;
                    index: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Always `true`: a write that did not happen is an error response. */
                            ok: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/vector/api/embed": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Generate embeddings for raw text */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @description Embedding model id. Omitted means the catalog's default model. */
                        model?: string | null;
                        /**
                         * @description Texts to embed; one vector is returned per text, in order. May be
                         *     empty.
                         */
                        texts: string[];
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: uint32
                             * @description Vector dimensionality.
                             */
                            dimensions: number;
                            /** @description Embedding model that produced the vectors. */
                            model: string;
                            /** @description One vector per input text, in input order. */
                            vectors: number[][];
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/vector/api/indexes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List indexes */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Index names, in lexical order. Empty when no vector backend is
                             *     available on this deployment.
                             */
                            indexes: string[];
                        };
                    };
                };
            };
        };
        put?: never;
        /** Create a vector index */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * Format: uint32
                         * @description Consistency check only: when given it must equal the model's own
                         *     dimensionality, which is what the index is created with either way.
                         */
                        dimensions?: number | null;
                        /**
                         * @description Also store text for keyword and hybrid search.
                         * @default false
                         */
                        keyword_search?: boolean;
                        /** @description Distance metric. Omitted means `cosine`. */
                        metric?: ("cosine" | "euclidean" | "dotproduct") | null;
                        /**
                         * @description Embedding model id from the catalog. Omitted means the catalog's
                         *     default model.
                         */
                        model?: string | null;
                        /**
                         * @description Index name: `[A-Za-z0-9_]` only. Every other endpoint addresses the
                         *     index by this name.
                         */
                        name: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: uint32
                             * @description Vector dimensionality, taken from the model.
                             */
                            dimensions: number;
                            /** @description Whether the index also stores text for keyword and hybrid search. */
                            keyword_search: boolean;
                            /** @description Distance metric the index was created with. */
                            metric: "cosine" | "euclidean" | "dotproduct";
                            /** @description Embedding model id the index is bound to. */
                            model: string;
                            /** @description The name the index is addressed by. */
                            name: string;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/vector/api/indexes/{name}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Delete an index */
        delete: {
            parameters: {
                query?: never;
                header?: never;
                path: {
                    name: string;
                };
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Always `true`: a write that did not happen is an error response. */
                            ok: boolean;
                        };
                    };
                };
            };
        };
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/vector/api/ingest": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Chunk + embed + upsert a document */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /**
                         * @description Prepend an LLM-written one-paragraph summary of the document to every
                         *     chunk before embedding. Silently skipped when no default LLM is
                         *     configured.
                         * @default false
                         */
                        contextual?: boolean;
                        /** @description Caller-supplied document id. Chunk ids are `{document_id}:{n}`. */
                        document_id: string;
                        /** @description Index name. */
                        index: string;
                        /**
                         * @description Arbitrary JSON stored on every chunk as `user_metadata`, beside the
                         *     `document_id` and `chunk_index` the block adds.
                         */
                        metadata?: unknown;
                        /** @description The document text. */
                        text: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * Format: uint
                             * @description Chunks written. `0` when the text was empty or whitespace only.
                             */
                            chunks_created: number;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/vector/api/query": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Search vectors */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @description Restrict hits to rows whose metadata matches. */
                        filter?: {
                            /**
                             * @description Equality constraints: each key is a dot-path into the stored metadata
                             *     and the value must match exactly.
                             * @default {}
                             */
                            equals?: {
                                [key: string]: unknown;
                            };
                        } | null;
                        /** @description Index name. */
                        index: string;
                        /** @description Keyword query for keyword and hybrid mode. Omitted means `text`. */
                        keyword_query?: string | null;
                        /**
                         * @description Search modality. Omitted means `hybrid` for an index created with
                         *     `keyword_search`, `vector` otherwise.
                         */
                        mode?: ("vector" | "keyword" | "hybrid") | null;
                        /**
                         * @description Query text. Embedded with the model the index was created with when
                         *     `vector` is absent; never embedded when `vector` is present. In
                         *     keyword and hybrid mode it is also the keyword query unless
                         *     `keyword_query` is given.
                         */
                        text?: string | null;
                        /**
                         * Format: uint
                         * @description Maximum number of hits. Omitted means 10.
                         */
                        top_k?: number | null;
                        /**
                         * @description Pre-computed query vector, used as is; its length must match the
                         *     index's `dimensions`. When present, `text` is not embedded.
                         */
                        vector?: number[] | null;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Hits, best first. */
                            matches: {
                                /** @description Matched row id. */
                                id: string;
                                /** @description The metadata stored with the row. Absent when the row stored none. */
                                metadata?: unknown;
                                /**
                                 * Format: float
                                 * @description Similarity score; its scale depends on the index's metric.
                                 */
                                score: number;
                            }[];
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/vector/api/stats": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Index stats and usage */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /**
                             * @description Every index, in lexical order. Empty when no vector backend is
                             *     available on this deployment.
                             */
                            indexes: {
                                /**
                                 * Format: uint64
                                 * @description Rows currently stored. `0` when the count could not be read.
                                 */
                                count: number;
                                /** @description Index name. */
                                name: string;
                            }[];
                        };
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/b/vector/api/upsert": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Upsert pre-computed vectors */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody: {
                content: {
                    "application/json": {
                        /** @description Rows to insert or replace. */
                        entries: {
                            /** @description Caller-supplied row id. Upserting the same id again replaces the row. */
                            id: string;
                            /**
                             * @description Arbitrary JSON metadata stored alongside the vector and echoed on
                             *     query hits.
                             */
                            metadata?: unknown;
                            /**
                             * @description Text to index for keyword search. Required when the index was created
                             *     with `keyword_search`; ignored otherwise.
                             */
                            text?: string | null;
                            /** @description Embedding vector; its length must match the index's `dimensions`. */
                            vector: number[];
                        }[];
                        /** @description Index name. */
                        index: string;
                    };
                };
            };
            responses: {
                /** @description Successful response */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": {
                            /** @description Always `true`: a write that did not happen is an error response. */
                            ok: boolean;
                        };
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: never;
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export type operations = Record<string, never>;
