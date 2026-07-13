//! GET /b/auth/oauth/login — relocated from auth/oauth.rs::handle_oauth_login in Task 5.

use sha2::{Digest, Sha256};
use wafer_block_crypto::primitives;
use wafer_core::clients::config;
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::auth::repo::oauth_pkce::{self, NewPkceState},
    http::{err_bad_request, err_forbidden, err_internal, ok_json},
    util::urlencode,
};

/// PKCE state TTL: 10 minutes. OAuth round-trips complete in seconds; this
/// is forgiving enough for a slow user on a captive-portal Wi-Fi without
/// keeping abandoned-flow rows around indefinitely.
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
    let enable_oauth = config::get_default(ctx, "WAFER_RUN_SHARED__ENABLE_OAUTH", "false").await;
    if enable_oauth != "true" && enable_oauth != "1" {
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
    let Ok(client_id) = config::get(ctx, &client_id_key).await else {
        return err_bad_request(&format!("OAuth provider '{provider}' not configured"));
    };

    let redirect_uri = config::get_default(
        ctx,
        "IMPRESSPRESS__AUTH_UI__OAUTH_REDIRECT_URI",
        "http://localhost:8090/b/auth/oauth/callback",
    )
    .await;

    // Generate PKCE code verifier and challenge.
    let code_verifier = match generate_pkce_verifier() {
        Ok(v) => v,
        Err(e) => return err_internal("Failed to generate PKCE verifier", e),
    };
    let code_challenge = pkce_challenge(&code_verifier);

    // SEC-040: the PKCE `code_verifier` is the client-side secret half of
    // PKCE. Previously it rode in a client-visible JWT (defeats the point of
    // PKCE entirely). Persist it server-side keyed by a random `state_id`
    // and send only the opaque id to the provider.
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
        return err_internal("Failed to persist OAuth state", e);
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

    ok_json(&serde_json::json!({
        "auth_url": auth_url,
        "provider": provider
    }))
}
