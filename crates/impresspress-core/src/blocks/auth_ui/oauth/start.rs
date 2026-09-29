//! GET /b/auth/oauth/login — the browser's entry into an OAuth sign-in.
//!
//! This endpoint is **navigated to**, not fetched: it answers `302` to the
//! provider's authorize URL and sets the cookie that binds the flow to this
//! browser (`state_binding`). Both halves depend on that.
//!
//! * The binding cookie is only usable if the browser is at this origin when
//!   it is set. Answering JSON for a caller to `fetch` put that write in a
//!   third-party context whenever the page was served from another origin —
//!   Safari blocks it, Firefox partitions it — and the callback would then
//!   find no binding and refuse every sign-in. A top-level navigation is
//!   first-party in the tab or popup the user is signing in with, wherever
//!   the page that sent them here was served from.
//! * A `302` is also what the provider round trip already is: the caller has
//!   nothing to do with `auth_url` except go to it.
//!
//! `impresspress-js`'s `signInWithOAuth` therefore builds this URL rather
//! than calling it, and `signInWithOAuthPopup` opens the popup here.

use sha2::{Digest, Sha256};
use wafer_block_crypto::primitives;
use wafer_core::clients::config;
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{
        auth::repo::oauth_pkce::{self, NewPkceState},
        auth_ui::OAUTH_REDIRECT_URI_KEY,
        crud,
    },
    config_vars::ENABLE_OAUTH_KEY,
    http::{err_bad_request, err_forbidden, err_internal, ResponseBuilder},
    util::urlencode,
};

/// PKCE state TTL: 10 minutes. OAuth round-trips complete in seconds; this
/// is forgiving enough for a slow user on a captive-portal Wi-Fi without
/// keeping abandoned-flow rows around indefinitely. The browser-binding
/// cookie carries the same lifetime, so both halves of a flow expire
/// together.
const PKCE_STATE_TTL_SECS: i64 = 600;

/// Generate a PKCE code verifier (43-128 chars, URL-safe).
fn generate_pkce_verifier() -> Result<String, String> {
    let bytes = primitives::random_bytes(32).map_err(|e| e.to_string())?;
    Ok(primitives::b64url_encode(&bytes))
}

/// Compute S256 code challenge from a verifier.
fn pkce_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    primitives::b64url_encode(&hash)
}

/// Random opaque OAuth `state` parameter sent to the provider. 32 random
/// bytes hex-encoded (64 chars) — fits any provider's state-length limit.
fn generate_state_id() -> Result<String, String> {
    let bytes = primitives::random_bytes(32).map_err(|e| e.to_string())?;
    Ok(crate::util::hex_encode(&bytes))
}

pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    // Check ENABLE_OAUTH flag
    let enable_oauth = match crate::config_vars::get_bool(ctx, ENABLE_OAUTH_KEY, false).await {
        Ok(enabled) => enabled,
        Err(e) => return crud::db_error_internal(e, "Could not read the OAuth switch"),
    };
    if !enable_oauth {
        return err_forbidden("OAuth login is not enabled");
    }

    let provider = msg.query("provider");
    if provider.is_empty() {
        return err_bad_request("Missing provider parameter");
    }

    let client_id_key = format!(
        "IMPRESSPRESS__AUTH_UI__OAUTH_{}_CLIENT_ID",
        provider.to_uppercase()
    );
    // Unset and empty both mean the provider is not configured; a read the
    // config block refused is a fault, not an unconfigured provider.
    let client_id = match config::get_optional(ctx, &client_id_key).await {
        Ok(Some(client_id)) if !client_id.is_empty() => client_id,
        Ok(_) => {
            return err_bad_request(&format!("OAuth provider '{provider}' not configured"));
        }
        Err(e) => return crud::db_error_internal(e, "Could not read the OAuth client id"),
    };

    let redirect_uri = config::get_default(
        ctx,
        OAUTH_REDIRECT_URI_KEY,
        "http://localhost:8090/b/auth/oauth/callback",
    )
    .await;
    let redirect_uri = match redirect_uri {
        Ok(redirect_uri) => redirect_uri,
        Err(e) => return crud::db_error_internal(e, "Could not read the OAuth redirect URI"),
    };

    // Generate PKCE code verifier and challenge.
    let code_verifier = match generate_pkce_verifier() {
        Ok(v) => v,
        Err(e) => return err_internal("Failed to generate PKCE verifier", e),
    };
    let code_challenge = pkce_challenge(&code_verifier);

    // SEC-040: the `code_verifier` is the secret half of PKCE and never
    // leaves the server. It is persisted keyed by a random `state_id`, and
    // only that opaque id travels to the provider.
    let state_id = match generate_state_id() {
        Ok(s) => s,
        Err(e) => return err_internal("Failed to generate state", e),
    };
    let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(PKCE_STATE_TTL_SECS))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    if let Err(e) = oauth_pkce::insert(
        ctx,
        NewPkceState {
            state_id: &state_id,
            provider,
            code_verifier: &code_verifier,
            redirect_uri: &redirect_uri,
            expires_at: &expires_at,
        },
    )
    .await
    {
        return crud::db_error_internal(e, "Failed to persist OAuth state");
    }

    // urlencode every interpolation site uniformly. `client_id` / `redirect_uri`
    // come from operator config and could contain `&` / `=` / `?` characters
    // that would otherwise corrupt the query string.
    let client_id_enc = urlencode(&client_id);
    let redirect_uri_enc = urlencode(&redirect_uri);
    let state_enc = urlencode(&state_id);
    let challenge_enc = urlencode(&code_challenge);
    let auth_url = match super::spec::lookup(provider) {
        Some(spec) => spec.build_authorize_url(
            &client_id_enc,
            &redirect_uri_enc,
            &state_enc,
            &challenge_enc,
        ),
        None => return err_bad_request(&format!("Unsupported provider: {provider}")),
    };

    // Bind the flow to this browser. The provider echoes `state_id` back to
    // whichever browser follows the callback URL; only the browser holding
    // this cookie may redeem it (see `state_binding`).
    let binding = match super::state_binding::issue(ctx, &state_id, PKCE_STATE_TTL_SECS).await {
        Ok(binding) => binding,
        Err(e) => return crud::db_error_internal(e, "Could not build the OAuth binding cookie"),
    };

    ResponseBuilder::new()
        .status(302)
        .set_cookie(&binding)
        .set_header("Location", &auth_url)
        // The provider's authorize URL is not a secret, but it carries this
        // flow's `state`, and a cached copy of it would hand a second browser
        // a URL whose binding cookie it does not hold.
        .set_header("Cache-Control", "no-store")
        .body(Vec::new(), "")
}
