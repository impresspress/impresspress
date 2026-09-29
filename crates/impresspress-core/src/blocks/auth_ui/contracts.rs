//! Request/response types for the `/b/auth/api/*` JSON surface.
//!
//! These are the *only* source of the OpenAPI schemas declared in
//! [`super::AuthUiBlock`]'s `BlockInfo::endpoints` — `.input::<T>()` /
//! `.output::<T>()` derive them from the same types the handlers deserialize
//! into and serialize out of. Before this module existed, each handler built
//! its response with an ad-hoc `serde_json::json!` literal and the schema was
//! hand-written alongside it in `mod.rs`, so the two could (and did) drift
//! with nothing to catch it.
//!
//! # Nothing credential-bearing belongs here
//!
//! Every type in this module is serialized straight to an HTTP response. The
//! password hash (`local_credentials.password_hash`), the refresh-token row's
//! `token_hash`, the verification-token digest and the JWT blocklist `jti` all
//! live on rows these handlers read — and none of them appears on any type
//! below. That is why the user-facing shapes are hand-written view types
//! ([`AuthenticatedUser`], [`PendingSignupUser`], [`MeUser`]) rather than
//! `repo::users::UserRow` re-exported: a view type cannot silently grow a
//! column when the table does.
//!
//! The access/refresh tokens on [`LoginResponse`] / [`SignupResponse`] /
//! [`RefreshResponse`] are the *product* of these endpoints, not a leak — they
//! are what the caller came for, and they were already in the hand-written
//! schemas.

use serde::{Deserialize, Serialize};

// Modelled as a single-variant enum rather than a `String` so a handler cannot
// emit anything else: the schema's constant and the Rust value are the same
// fact. Renders as `{"type": "string", "enum": ["Bearer"]}` — schemars' spelling
// of the `{"const": "Bearer"}` the hand-written schema used, same meaning.
/// The only `token_type` this API issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum TokenType {
    Bearer,
}

/// `POST /b/auth/api/login` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoginRequest {
    #[schemars(extend("format" = "email"))]
    pub email: String,
    pub password: String,
}

// A deliberate projection of `repo::users::UserRow` — `disabled`, `deleted_at`,
// `email_verified`, `updated_at` and the auth-version counter are
// account-lifecycle state that no authenticated caller needs and that a row
// re-export would have published. Keep this a view type.
/// The caller's identity, as returned by a successful authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AuthenticatedUser {
    pub id: String,
    pub email: String,
    pub roles: Vec<String>,
    pub name: String,
}

/// `POST /b/auth/api/login` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoginResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: TokenType,
    /// Access token lifetime in seconds
    pub expires_in: u64,
    /// Role-aware post-login redirect path
    pub default_redirect: String,
    pub user: AuthenticatedUser,
}

/// `POST /b/auth/api/signup` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SignupRequest {
    #[schemars(extend("format" = "email"))]
    pub email: String,
    pub password: String,
    /// Optional display name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `email_verified` fixed to `V`: it serializes as that literal, refuses the
/// other one on decode, and publishes `{"type": "boolean", "const": V}`.
// The reason it exists: `SignupResponse`'s two replies are told apart by this
// flag, so as a plain `bool` beside the tokens it could say "verified" on a
// reply that carries none (or the reverse), and the schema could not rule
// that out either. Fixed per variant, the flag and the tokens are one fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EmailVerified<const V: bool>;

impl<const V: bool> Serialize for EmailVerified<V> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(V)
    }
}

impl<'de, const V: bool> Deserialize<'de> for EmailVerified<V> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = bool::deserialize(deserializer)?;
        if value == V {
            Ok(Self)
        } else {
            Err(serde::de::Error::invalid_value(
                serde::de::Unexpected::Bool(value),
                &if V { "true" } else { "false" },
            ))
        }
    }
}

impl<const V: bool> schemars::JsonSchema for EmailVerified<V> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        if V {
            "EmailVerifiedTrue".into()
        } else {
            "EmailVerifiedFalse".into()
        }
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "boolean", "const": V })
    }
}

// Distinct from `AuthenticatedUser` because the verification-required and
// already-registered paths answer before any role lookup happens. Those two
// paths must answer identical bytes ([SEC-035]; `api::signup`'s
// `pending_verification`), and only one of them has an account id to send,
// so neither sends one.
/// The address a signup awaiting verification was made for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PendingSignupUser {
    pub email: String,
}

// Untagged, so each variant's JSON is exactly the flat object the endpoint
// has always answered; `email_verified` is the discriminant, fixed per
// variant by `EmailVerified`. Deserializing tries `SignedIn` first, and a
// body that is not wholly one variant or the other is refused.
//
// An address that is already registered gets `PendingVerification` either
// way (see `api::signup::pending_verification`), so with verification off
// the reply reveals whether an address is registered.
/// `POST /b/auth/api/signup` response body: signed in with tokens, or
/// awaiting email verification with none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum SignupResponse {
    /// Verification is not required: the new account is signed in.
    SignedIn {
        email_verified: EmailVerified<true>,
        access_token: String,
        refresh_token: String,
        token_type: TokenType,
        /// Access token lifetime in seconds
        expires_in: u64,
        /// Role-aware post-login redirect path
        default_redirect: String,
        user: AuthenticatedUser,
    },
    /// Verification is required, or the address is already registered: no
    /// tokens are issued.
    PendingVerification {
        email_verified: EmailVerified<false>,
        message: String,
        user: PendingSignupUser,
    },
}

/// The one-key body five endpoints on this surface answer: `POST
/// /b/auth/api/logout`, `/change-password`, `/forgot-password`,
/// `/reset-password` and `/resend-verification`.
///
/// `/change-password` answers it to JSON callers; that one endpoint answers a
/// browser form with HTML instead.
///
/// One type because it is one shape. The *text* differs per endpoint and is
/// deliberately constant per endpoint rather than per outcome — the
/// password-reset and verification pair answer the same sentence whatever
/// the address's state, so nothing about an account can be learned from the
/// response (`api::verify::resend_tests` pins that). A per-endpoint copy of
/// this struct would publish five schemas that must be kept identical by
/// hand.
// Which HTML, and where the branch is: `api::change_password`'s
// `changed_response` and `refused` answer an htmx caller with markup for the
// `#change-pw-result` slot it posts from, success and refusal alike. That
// detail stays out of the doc comment above because schemars publishes it as
// this schema's `description`, inlined at all five endpoints — the same
// reason the note on `MeUser` below is not a doc comment either.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MessageResponse {
    pub message: String,
}

// Same projection rule as `AuthenticatedUser` — a view type, never the row.
// `avatar_url` is flattened from the row's `Option<String>` to `""`, so the
// wire type is a plain string and the key is always present.
/// The caller's own profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MeUser {
    pub id: String,
    pub email: String,
    pub name: String,
    pub roles: Vec<String>,
    #[schemars(extend("format" = "date-time"))]
    pub created_at: String,
    pub avatar_url: String,
}

/// `GET /b/auth/api/me` and `PATCH /b/auth/api/me` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MeResponse {
    pub user: MeUser,
}

/// `PATCH /b/auth/api/me` request body. Every field is optional and only the
/// ones present are applied. `name` and `avatar_url` are the only
/// user-editable profile fields: email changes go through verification and
/// roles through the admin API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateMeRequest {
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

/// `POST /b/auth/api/refresh` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// `POST /b/auth/api/refresh` response body: the rotated token pair only.
/// Refresh does not re-resolve `default_redirect` or the user projection, so
/// neither is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RefreshResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: TokenType,
    /// Access token lifetime in seconds
    pub expires_in: u64,
}

/// `POST /b/auth/api/api-keys` request body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateApiKeyRequest {
    /// Label shown in the key list. Required and non-empty.
    pub name: String,
    // The handler normalizes this to UTC before it is stored, so a request
    // may state any offset and the row records the instant it named. An
    // empty string is the absent value an HTML form posts for a blank field
    // and means the same as omitting it.
    /// Absolute expiry, RFC 3339 and in the future. Omit for a key that does
    /// not expire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(extend("format" = "date-time"))]
    pub expires_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signup contract cannot describe a reply whose `email_verified`
    /// disagrees with its tokens, in either direction: such a body does not
    /// decode, so no Rust value (and no published schema branch) can say it.
    #[test]
    fn a_signup_reply_whose_flag_disagrees_with_its_tokens_is_not_the_contract() {
        let verified_without_tokens = serde_json::json!({
            "email_verified": true,
            "message": "Account created. Please verify your email before signing in.",
            "user": {"email": "someone@example.com"},
        });
        let unverified_with_tokens = serde_json::json!({
            "email_verified": false,
            "access_token": "a",
            "refresh_token": "r",
            "token_type": "Bearer",
            "expires_in": 1800,
            "default_redirect": "/b/userportal/",
            "user": {"id": "u1", "email": "someone@example.com", "roles": ["user"], "name": ""},
        });
        for body in [verified_without_tokens, unverified_with_tokens] {
            assert!(
                serde_json::from_value::<SignupResponse>(body.clone()).is_err(),
                "decoded a signup reply whose flag disagrees with its tokens: {body}"
            );
        }
    }

    /// The two replies still serialize as the flat objects the endpoint has
    /// always sent, key for key, so no client sees a wire change.
    #[test]
    fn each_signup_reply_serializes_as_its_flat_body() {
        let signed_in = SignupResponse::SignedIn {
            email_verified: EmailVerified,
            access_token: "a".into(),
            refresh_token: "r".into(),
            token_type: TokenType::Bearer,
            expires_in: 1800,
            default_redirect: "/b/userportal/".into(),
            user: AuthenticatedUser {
                id: "u1".into(),
                email: "someone@example.com".into(),
                roles: vec!["user".into()],
                name: "".into(),
            },
        };
        assert_eq!(
            serde_json::to_string(&signed_in).unwrap(),
            r#"{"email_verified":true,"access_token":"a","refresh_token":"r","token_type":"Bearer","expires_in":1800,"default_redirect":"/b/userportal/","user":{"id":"u1","email":"someone@example.com","roles":["user"],"name":""}}"#
        );
        let pending = SignupResponse::PendingVerification {
            email_verified: EmailVerified,
            message: "m".into(),
            user: PendingSignupUser {
                email: "someone@example.com".into(),
            },
        };
        assert_eq!(
            serde_json::to_string(&pending).unwrap(),
            r#"{"email_verified":false,"message":"m","user":{"email":"someone@example.com"}}"#
        );
    }
}
