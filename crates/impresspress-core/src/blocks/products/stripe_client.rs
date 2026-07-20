//! Shared Stripe HTTP boundary.
//!
//! Domain modules build provider requests and validate resource-specific
//! responses, while this module alone owns credentials, common headers,
//! form encoding, API URL selection, and network transport.

use std::collections::HashMap;

use serde_json::Value;
use wafer_core::clients::{config, network};
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::stripe_secret_operations_allowed;

pub(crate) const DEFAULT_API_VERSION: &str = "2026-02-25.clover";

#[derive(Debug, Clone)]
pub(crate) struct StripeClient {
    secret_key: String,
    api_url: String,
    api_version: String,
    pub(crate) livemode: bool,
}

impl StripeClient {
    pub(crate) async fn load(ctx: &dyn Context) -> Result<Self, WaferError> {
        if !stripe_secret_operations_allowed(ctx).await {
            return Err(WaferError::new(
                ErrorCode::FailedPrecondition,
                "Stripe secret-key operations are disabled in the browser runtime; configure a trusted remote commerce API instead",
            ));
        }
        let secret_key =
            config::get_default(ctx, "IMPRESSPRESS__PRODUCTS__STRIPE_SECRET_KEY", "").await;
        let livemode = secret_livemode(&secret_key).ok_or_else(|| {
            WaferError::new(
                ErrorCode::FailedPrecondition,
                "Stripe secret key must be a test or live secret key",
            )
        })?;
        let api_version = config::get_default(
            ctx,
            "IMPRESSPRESS__PRODUCTS__STRIPE_API_VERSION",
            DEFAULT_API_VERSION,
        )
        .await;
        if !super::stripe::is_stable_stripe_api_version(&api_version) {
            return Err(WaferError::new(
                ErrorCode::FailedPrecondition,
                "Stripe API version must be a stable named release",
            ));
        }
        let api_url = config::get_default(
            ctx,
            "IMPRESSPRESS__PRODUCTS__STRIPE_API_URL",
            "https://api.stripe.com",
        )
        .await;
        Ok(Self {
            secret_key,
            api_url: api_url.trim_end_matches('/').to_string(),
            api_version,
            livemode,
        })
    }

    pub(crate) async fn request_json(
        &self,
        ctx: &dyn Context,
        method: &str,
        path: &str,
        stripe_account: Option<&str>,
        idempotency_key: Option<&str>,
        form: Option<Vec<(String, String)>>,
    ) -> Result<Value, WaferError> {
        let headers = request_headers(
            &self.secret_key,
            &self.api_version,
            stripe_account,
            idempotency_key,
        );
        let body = form.map(encode_form);
        let response = send_raw(
            ctx,
            method,
            &format!("{}{}", self.api_url, path),
            &headers,
            body.as_deref().map(str::as_bytes),
        )
        .await
        .map_err(|error| {
            WaferError::new(
                ErrorCode::Internal,
                format!("Stripe request could not be completed: {error}"),
            )
        })?;
        if response.status_code >= 400 {
            let decoded: Value = serde_json::from_slice(&response.body).unwrap_or_default();
            let code = provider_error_code(&decoded);
            return Err(WaferError::new(
                ErrorCode::FailedPrecondition,
                format!(
                    "Stripe rejected the request (HTTP {}, code {code})",
                    response.status_code
                ),
            ));
        }
        serde_json::from_slice(&response.body).map_err(|_| {
            WaferError::new(
                ErrorCode::Internal,
                "Stripe returned an unreadable response",
            )
        })
    }
}

pub(crate) fn secret_livemode(key: &str) -> Option<bool> {
    if key.starts_with("sk_test_") {
        Some(false)
    } else if key.starts_with("sk_live_") {
        Some(true)
    } else {
        None
    }
}

pub(crate) fn publishable_livemode(key: &str) -> Option<bool> {
    if key.starts_with("pk_test_") {
        Some(false)
    } else if key.starts_with("pk_live_") {
        Some(true)
    } else {
        None
    }
}

pub(crate) fn encode_form(pairs: Vec<(String, String)>) -> String {
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={}", crate::util::url_path_encode(&value)))
        .collect::<Vec<_>>()
        .join("&")
}

pub(crate) fn request_headers(
    secret_key: &str,
    api_version: &str,
    stripe_account: Option<&str>,
    idempotency_key: Option<&str>,
) -> HashMap<String, String> {
    let mut headers = HashMap::from([
        ("Authorization".to_string(), format!("Bearer {secret_key}")),
        ("Stripe-Version".to_string(), api_version.to_string()),
        (
            "Content-Type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        ),
    ]);
    if let Some(account) = stripe_account.filter(|value| !value.is_empty()) {
        headers.insert("Stripe-Account".to_string(), account.to_string());
    }
    if let Some(key) = idempotency_key.filter(|value| !value.is_empty()) {
        headers.insert("Idempotency-Key".to_string(), key.to_string());
    }
    headers
}

pub(crate) fn provider_error_code(value: &Value) -> &str {
    value
        .pointer("/error/code")
        .or_else(|| value.pointer("/error/type"))
        .and_then(Value::as_str)
        .unwrap_or("provider_error")
}

pub(crate) async fn send_raw(
    ctx: &dyn Context,
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: Option<&[u8]>,
) -> Result<network::NetworkResponse, WaferError> {
    network::do_request(ctx, method, url, headers, body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encoding_cannot_inject_provider_fields() {
        assert_eq!(
            encode_form(vec![(
                "metadata[order_id]".to_string(),
                "order&mode=subscription".to_string(),
            )]),
            "metadata[order_id]=order%26mode%3Dsubscription"
        );
    }

    #[test]
    fn headers_pin_version_account_and_idempotency_without_secrets_in_values() {
        let headers = request_headers(
            "sk_test_secret",
            DEFAULT_API_VERSION,
            Some("acct_seller"),
            Some("operation_1"),
        );
        assert_eq!(headers["Stripe-Version"], DEFAULT_API_VERSION);
        assert_eq!(headers["Stripe-Account"], "acct_seller");
        assert_eq!(headers["Idempotency-Key"], "operation_1");
        assert_eq!(headers["Content-Type"], "application/x-www-form-urlencoded");
    }

    #[test]
    fn provider_error_decoder_is_bounded_and_stable() {
        assert_eq!(
            provider_error_code(&serde_json::json!({"error":{"code":"card_declined"}})),
            "card_declined"
        );
        assert_eq!(
            provider_error_code(&serde_json::json!({"private":"body"})),
            "provider_error"
        );
    }
}
