//! Per-provider OAuth wiring as static data + a generic driver.
//!
//! All three supported providers run the same flow — build an authorize URL,
//! exchange the code for a token, fetch userinfo, and (for GitHub) fall back to
//! `/user/emails` for a verified address. Only the *data* differs between them,
//! so each provider is one [`OAuthProviderSpec`] row and the flow handlers in
//! `start.rs` / `callback.rs` read these fields instead of matching on the
//! provider name. Adding a provider is a single table row.

use crate::util::urlencode;

/// Authorization-header scheme for the userinfo request. Providers differ only
/// in the scheme word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserinfoAuth {
    /// `Authorization: Bearer <token>` — Google, Microsoft (OIDC).
    Bearer,
    /// `Authorization: token <token>` — GitHub REST API.
    Token,
}

/// Static OAuth wiring for one provider.
///
/// One row per supported provider in [`OAUTH_PROVIDERS`]. The generic flow in
/// `start.rs` / `callback.rs` reads these fields rather than branching on the
/// provider name.
pub struct OAuthProviderSpec {
    /// Provider key as it appears in config-var names
    /// (`IMPRESSPRESS__AUTH_UI__OAUTH_{NAME}_CLIENT_ID`) and the
    /// `provider_links.provider` column.
    pub name: &'static str,
    /// Authorization endpoint (the user-facing redirect target).
    pub authorize_url: &'static str,
    /// Token-exchange endpoint.
    pub token_url: &'static str,
    /// Userinfo endpoint.
    pub userinfo_url: &'static str,
    /// OAuth scope, stored in its exact on-the-wire form and interpolated
    /// verbatim into the authorize URL.
    ///
    /// It is deliberately *not* routed through [`urlencode`]: Google/Microsoft
    /// use a pre-encoded `openid%20email%20profile`, while GitHub uses
    /// `user:email` with a raw colon. `urlencode` (form-urlencoded) would
    /// render the space as `+` and the colon as `%3A`, changing both URLs.
    pub scope: &'static str,
    /// Whether the flow uses PKCE + OIDC `response_type=code` /
    /// `grant_type=authorization_code` / `code_verifier`. Google and Microsoft
    /// do; GitHub's legacy OAuth2 flow does not.
    pub uses_pkce: bool,
    /// Authorization-header scheme for the userinfo request.
    pub userinfo_auth: UserinfoAuth,
    /// Optional fallback endpoint for a verified primary email when the
    /// userinfo payload omits one (GitHub `/user/emails`). `None` for providers
    /// that always return an email.
    pub emails_url: Option<&'static str>,
}

impl OAuthProviderSpec {
    /// Build the provider's authorize URL. All `*_enc` arguments are expected
    /// already-`urlencode`d by the caller; [`scope`](Self::scope) is
    /// interpolated verbatim (see its docs).
    pub fn build_authorize_url(
        &self,
        client_id_enc: &str,
        redirect_uri_enc: &str,
        state_enc: &str,
        challenge_enc: &str,
    ) -> String {
        if self.uses_pkce {
            format!(
                "{}?client_id={client_id_enc}&redirect_uri={redirect_uri_enc}&response_type=code&scope={}&state={state_enc}&code_challenge={challenge_enc}&code_challenge_method=S256",
                self.authorize_url, self.scope
            )
        } else {
            format!(
                "{}?client_id={client_id_enc}&redirect_uri={redirect_uri_enc}&scope={}&state={state_enc}",
                self.authorize_url, self.scope
            )
        }
    }

    /// Build the `application/x-www-form-urlencoded` token-exchange request
    /// body. Every value is `urlencode`d; PKCE providers additionally send
    /// `grant_type=authorization_code` and the `code_verifier`.
    pub fn build_token_body(
        &self,
        code: &str,
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> String {
        let base = format!(
            "code={}&client_id={}&client_secret={}&redirect_uri={}",
            urlencode(code),
            urlencode(client_id),
            urlencode(client_secret),
            urlencode(redirect_uri),
        );
        if self.uses_pkce {
            format!(
                "{base}&grant_type=authorization_code&code_verifier={}",
                urlencode(code_verifier)
            )
        } else {
            base
        }
    }

    /// Build the `Authorization` header value for the userinfo request.
    pub fn userinfo_auth_header(&self, access_token: &str) -> String {
        match self.userinfo_auth {
            UserinfoAuth::Bearer => format!("Bearer {access_token}"),
            UserinfoAuth::Token => format!("token {access_token}"),
        }
    }
}

/// All supported OAuth providers. The enabled-provider list endpoint and the
/// login/callback flow both iterate or look up against this table.
pub const OAUTH_PROVIDERS: &[OAuthProviderSpec] = &[
    OAuthProviderSpec {
        name: "google",
        authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
        token_url: "https://oauth2.googleapis.com/token",
        userinfo_url: "https://www.googleapis.com/oauth2/v2/userinfo",
        scope: "openid%20email%20profile",
        uses_pkce: true,
        userinfo_auth: UserinfoAuth::Bearer,
        emails_url: None,
    },
    OAuthProviderSpec {
        name: "github",
        authorize_url: "https://github.com/login/oauth/authorize",
        token_url: "https://github.com/login/oauth/access_token",
        userinfo_url: "https://api.github.com/user",
        scope: "user:email",
        uses_pkce: false,
        userinfo_auth: UserinfoAuth::Token,
        emails_url: Some("https://api.github.com/user/emails"),
    },
    OAuthProviderSpec {
        name: "microsoft",
        authorize_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        userinfo_url: "https://graph.microsoft.com/v1.0/me",
        scope: "openid%20email%20profile",
        uses_pkce: true,
        userinfo_auth: UserinfoAuth::Bearer,
        emails_url: None,
    },
];

/// Look up a provider spec by its `name`. Returns `None` for unsupported
/// providers.
pub fn lookup(name: &str) -> Option<&'static OAuthProviderSpec> {
    OAUTH_PROVIDERS.iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> &'static OAuthProviderSpec {
        lookup(name).expect("provider exists")
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(lookup("twitter").is_none());
        assert!(lookup("").is_none());
    }

    // --- authorize URL: pinned byte-for-byte against the pre-refactor literals ---

    #[test]
    fn authorize_url_google() {
        assert_eq!(
            spec("google").build_authorize_url("CID", "RURI", "STATE", "CHAL"),
            "https://accounts.google.com/o/oauth2/v2/auth?client_id=CID&redirect_uri=RURI&response_type=code&scope=openid%20email%20profile&state=STATE&code_challenge=CHAL&code_challenge_method=S256"
        );
    }

    #[test]
    fn authorize_url_github() {
        // GitHub: no response_type, no PKCE challenge, raw `:` in scope.
        assert_eq!(
            spec("github").build_authorize_url("CID", "RURI", "STATE", "CHAL"),
            "https://github.com/login/oauth/authorize?client_id=CID&redirect_uri=RURI&scope=user:email&state=STATE"
        );
    }

    #[test]
    fn authorize_url_microsoft() {
        assert_eq!(
            spec("microsoft").build_authorize_url("CID", "RURI", "STATE", "CHAL"),
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id=CID&redirect_uri=RURI&response_type=code&scope=openid%20email%20profile&state=STATE&code_challenge=CHAL&code_challenge_method=S256"
        );
    }

    // --- token body: pinned against the pre-refactor literals (values are urlencoded) ---

    #[test]
    fn token_body_google_includes_pkce() {
        assert_eq!(
            spec("google").build_token_body("CODE", "cid", "secret", "https://app/cb", "VERIFIER"),
            "code=CODE&client_id=cid&client_secret=secret&redirect_uri=https%3A%2F%2Fapp%2Fcb&grant_type=authorization_code&code_verifier=VERIFIER"
        );
    }

    #[test]
    fn token_body_github_omits_pkce() {
        assert_eq!(
            spec("github").build_token_body("CODE", "cid", "secret", "https://app/cb", "VERIFIER"),
            "code=CODE&client_id=cid&client_secret=secret&redirect_uri=https%3A%2F%2Fapp%2Fcb"
        );
    }

    #[test]
    fn token_body_microsoft_includes_pkce() {
        assert_eq!(
            spec("microsoft").build_token_body("CODE", "cid", "secret", "https://app/cb", "VERIFIER"),
            "code=CODE&client_id=cid&client_secret=secret&redirect_uri=https%3A%2F%2Fapp%2Fcb&grant_type=authorization_code&code_verifier=VERIFIER"
        );
    }

    // --- userinfo auth header: Bearer (OIDC) vs token (GitHub) ---

    #[test]
    fn userinfo_auth_header_schemes() {
        assert_eq!(spec("google").userinfo_auth_header("TOK"), "Bearer TOK");
        assert_eq!(spec("microsoft").userinfo_auth_header("TOK"), "Bearer TOK");
        assert_eq!(spec("github").userinfo_auth_header("TOK"), "token TOK");
    }

    // --- endpoints + emails fallback pinned ---

    #[test]
    fn endpoints_and_emails_fallback() {
        let g = spec("github");
        assert_eq!(g.token_url, "https://github.com/login/oauth/access_token");
        assert_eq!(g.userinfo_url, "https://api.github.com/user");
        assert_eq!(g.emails_url, Some("https://api.github.com/user/emails"));
        assert_eq!(spec("google").emails_url, None);
        assert_eq!(spec("microsoft").emails_url, None);
    }
}
