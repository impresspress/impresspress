//! Cloudflare Turnstile Siteverify client.

use std::collections::HashMap;

use serde::Deserialize;
use wafer_core::clients::{config, network};
use wafer_run::context::Context;

use super::{abuse, config::TURNSTILE_SECRET_KEY};

const SITEVERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
// Cloudflare's published always-pass test secret. Dummy Siteverify responses
// use synthetic action/hostname metadata that need not match the page request,
// so local browser automation gets a separate, loopback-only validation path.
const ALWAYS_PASS_TEST_SECRET: &str = "1x0000000000000000000000000000000AA";
const MAX_TOKEN_BYTES: usize = 2_048;
const MAX_RESPONSE_BYTES: usize = 16 * 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    #[error("challenge token is missing")]
    MissingToken,
    #[error("challenge token is invalid")]
    InvalidToken,
    #[error("challenge verification is unavailable")]
    Unavailable,
    #[error("challenge verification was rejected")]
    Rejected,
}

#[derive(Debug, Deserialize)]
struct SiteverifyResponse {
    success: bool,
    #[serde(default)]
    action: String,
    #[serde(default)]
    hostname: String,
    #[serde(default, rename = "error-codes")]
    error_codes: Vec<String>,
}

pub async fn verify(
    ctx: &dyn Context,
    token: &str,
    remote_ip: &str,
    request_host: &str,
) -> Result<(), VerifyError> {
    if token.trim().is_empty() {
        return Err(VerifyError::MissingToken);
    }
    if token.len() > MAX_TOKEN_BYTES || token.chars().any(char::is_control) {
        return Err(VerifyError::InvalidToken);
    }
    let secret = config::get_default(ctx, TURNSTILE_SECRET_KEY, "")
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "turnstile secret read failed");
            VerifyError::Unavailable
        })?;
    if secret.trim().is_empty() {
        return Err(VerifyError::Unavailable);
    }
    let mut body = format!(
        "secret={}&response={}",
        crate::util::urlencode(secret.trim()),
        crate::util::urlencode(token.trim())
    );
    let remote_ip = abuse::canonical_ip(remote_ip);
    if remote_ip != "unknown" {
        body.push_str("&remoteip=");
        body.push_str(&crate::util::urlencode(&remote_ip));
    }
    let headers = HashMap::from([
        (
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        ),
        ("Accept".to_string(), "application/json".to_string()),
    ]);
    let response =
        network::do_request(ctx, "POST", SITEVERIFY_URL, &headers, Some(body.as_bytes()))
            .await
            .map_err(|_| VerifyError::Unavailable)?;
    if !(200..300).contains(&response.status_code) || response.body.len() > MAX_RESPONSE_BYTES {
        return Err(VerifyError::Unavailable);
    }
    let result: SiteverifyResponse =
        serde_json::from_slice(&response.body).map_err(|_| VerifyError::Unavailable)?;
    if !valid_siteverify_response(&result, request_host, secret.trim()) {
        tracing::warn!(
            success = result.success,
            action = %result.action,
            hostname = %result.hostname,
            request_host = %request_host,
            error_codes = ?result.error_codes,
            "Turnstile response failed action or hostname validation",
        );
        return Err(VerifyError::Rejected);
    }
    Ok(())
}

fn valid_siteverify_response(
    result: &SiteverifyResponse,
    request_host: &str,
    secret: &str,
) -> bool {
    let request_host = canonical_host(request_host);
    if secret == ALWAYS_PASS_TEST_SECRET {
        return result.success
            && matches!(request_host.as_str(), "localhost" | "127.0.0.1" | "::1");
    }
    result.success
        && result.action == "ticket_submit"
        && canonical_host(&result.hostname) == request_host
        && !request_host.is_empty()
}

fn canonical_host(value: &str) -> String {
    let value = value.trim().trim_end_matches('.').to_ascii_lowercase();
    if value.is_empty() {
        return String::new();
    }
    url::Url::parse(&format!("https://{value}"))
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| host.trim_end_matches('.').to_string())
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_host_strips_case_port_and_trailing_dot() {
        assert_eq!(canonical_host("Example.COM.:443"), "example.com");
        assert_eq!(canonical_host("example.com"), "example.com");
        assert_eq!(canonical_host(""), "");
    }

    #[test]
    fn published_test_secret_is_accepted_only_for_success_on_loopback() {
        let test_response = SiteverifyResponse {
            success: true,
            action: String::new(),
            hostname: "example.com".into(),
            error_codes: vec![],
        };
        assert!(valid_siteverify_response(
            &test_response,
            "127.0.0.1:8091",
            ALWAYS_PASS_TEST_SECRET,
        ));
        assert!(!valid_siteverify_response(
            &test_response,
            "godosomething.fun",
            ALWAYS_PASS_TEST_SECRET,
        ));
        assert!(!valid_siteverify_response(
            &test_response,
            "127.0.0.1:8091",
            "production-secret",
        ));
        let rejected_response = SiteverifyResponse {
            success: false,
            action: String::new(),
            hostname: "example.com".into(),
            error_codes: vec!["invalid-input-response".into()],
        };
        assert!(!valid_siteverify_response(
            &rejected_response,
            "127.0.0.1:8091",
            ALWAYS_PASS_TEST_SECRET,
        ));
    }

    #[test]
    fn production_secret_still_requires_exact_action_and_hostname() {
        let production_response = SiteverifyResponse {
            success: true,
            action: "ticket_submit".into(),
            hostname: "GoDoSomething.Fun".into(),
            error_codes: vec![],
        };
        assert!(valid_siteverify_response(
            &production_response,
            "godosomething.fun:443",
            "production-secret",
        ));
        assert!(!valid_siteverify_response(
            &production_response,
            "example.com",
            "production-secret",
        ));
    }
}
