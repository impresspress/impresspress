//! Per-provider OAuth wiring as static data + a generic driver.
//!
//! All three supported providers run the same flow — build an authorize URL,
//! exchange the code for a token, fetch userinfo, and (for GitHub) read
//! `/user/emails` for a verified address. Only the *data* differs between them,
//! so each provider is one [`OAuthProviderSpec`] row and the flow handlers in
//! `start.rs` / `callback.rs` read these fields instead of matching on the
//! provider name. Adding a provider is a single table row.
//!
//! The security-bearing field is [`OAuthProviderSpec::email_assertion`]: it
//! records what, if anything, each provider promises about the address it
//! returns. `callback.rs` will not join an OAuth identity to an existing local
//! account, and will not mark a new account's address verified, unless the
//! provider actually asserts that the account holder controls that mailbox.

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

/// What a provider promises about the email address it hands back.
///
/// An OAuth login proves the caller controls an account *at the provider*. It
/// proves control of the mailbox only when the provider says so, and providers
/// differ on whether they say so at all. `callback.rs` reads this to decide two
/// things:
///
/// * whether the identity may be merged into a pre-existing local account with
///   the same address — without an assertion on both sides, anyone who can
///   register `victim@example.com` at a lax provider inherits that account;
/// * whether a newly created local account is `email_verified`, which is what
///   `WAFER_RUN__AUTH__REQUIRE_VERIFICATION` gates every later login on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmailAssertion {
    /// The userinfo payload carries a boolean claim about the address in that
    /// same payload; the field is the claim's name. `true` is an assertion of
    /// verification, anything else (absent, `false`, non-boolean) is not.
    Claim(&'static str),
    /// The userinfo payload says nothing, but a separate endpoint lists the
    /// account's addresses with a per-address `verified` flag. The field is
    /// that endpoint's URL. Only an entry carrying `verified: true` is
    /// accepted as verified.
    AddressList(&'static str),
    /// The provider asserts nothing about the address, so this deployment
    /// treats it as unverified.
    None,
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
    /// What this provider promises about the address it returns, and where
    /// that promise is read from. See [`EmailAssertion`].
    pub email_assertion: EmailAssertion,
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
        // The v2 userinfo endpoint spells the OIDC `email_verified` claim
        // `verified_email`. Google sets it for every address it issues a token
        // for, including Workspace addresses an administrator provisioned.
        email_assertion: EmailAssertion::Claim("verified_email"),
    },
    OAuthProviderSpec {
        name: "github",
        authorize_url: "https://github.com/login/oauth/authorize",
        token_url: "https://github.com/login/oauth/access_token",
        userinfo_url: "https://api.github.com/user",
        scope: "user:email",
        uses_pkce: false,
        userinfo_auth: UserinfoAuth::Token,
        // `/user` returns the public profile address, which GitHub does not
        // promise is confirmed (and returns as null when the user keeps it
        // private). `/user/emails` is the authoritative list and carries a
        // `verified` flag per address; the `user:email` scope above is what
        // grants access to it.
        email_assertion: EmailAssertion::AddressList("https://api.github.com/user/emails"),
    },
    OAuthProviderSpec {
        name: "microsoft",
        authorize_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        // The OIDC userinfo endpoint, not Graph `/v1.0/me`: `/me` answers with
        // `mail` / `userPrincipalName` and no `email` at all, so every
        // Microsoft sign-in ended at "No email returned by OAuth provider".
        // `/oidc/userinfo` answers the OIDC claim names this flow reads
        // (`sub`, `email`, `name`, `picture`) for the `openid email profile`
        // scope already requested above.
        userinfo_url: "https://graph.microsoft.com/oidc/userinfo",
        scope: "openid%20email%20profile",
        uses_pkce: true,
        userinfo_auth: UserinfoAuth::Bearer,
        // Microsoft returns no `email_verified` claim, and the `email` it does
        // return is the mutable `mail`/`otherMails` profile attribute: a
        // tenant administrator can set it to an address nobody in the tenant
        // controls (the "nOAuth" abuse), and a personal account can carry an
        // unconfirmed alias. So a Microsoft sign-in proves control of the
        // Microsoft account and nothing about the mailbox: it can create its
        // own local account, but never adopt one that already exists, and the
        // account it creates is not `email_verified`.
        email_assertion: EmailAssertion::None,
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

    /// Graph `/v1.0/me` answers `mail` / `userPrincipalName`; the flow reads
    /// `email`, so `/me` could never produce a sign-in. The OIDC userinfo
    /// endpoint answers the claim names the flow actually reads.
    #[test]
    fn microsoft_uses_the_oidc_userinfo_endpoint() {
        assert_eq!(
            spec("microsoft").userinfo_url,
            "https://graph.microsoft.com/oidc/userinfo"
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

    // --- endpoints pinned ---

    #[test]
    fn endpoints_pinned() {
        let g = spec("github");
        assert_eq!(g.token_url, "https://github.com/login/oauth/access_token");
        assert_eq!(g.userinfo_url, "https://api.github.com/user");
    }

    /// Each provider's promise about the address it returns, pinned. Changing
    /// a row here changes who may adopt an existing account, so the table is
    /// asserted rather than left to the reader.
    #[test]
    fn email_assertion_per_provider() {
        assert_eq!(
            spec("google").email_assertion,
            EmailAssertion::Claim("verified_email"),
        );
        assert_eq!(
            spec("github").email_assertion,
            EmailAssertion::AddressList("https://api.github.com/user/emails"),
        );
        assert_eq!(spec("microsoft").email_assertion, EmailAssertion::None);
    }
}
