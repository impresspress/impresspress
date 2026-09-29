//! Shared Stripe HTTP boundary.
//!
//! Domain modules build provider requests and validate resource-specific
//! responses, while this module alone owns credentials, common headers,
//! form encoding, API URL selection, and network transport.

use std::collections::HashMap;

use serde_json::Value;
use wafer_core::clients::{config, network};
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{
    config::{STRIPE_API_URL, STRIPE_API_VERSION, STRIPE_SECRET_KEY},
    stripe_secret_operations_allowed,
};

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
        if !stripe_secret_operations_allowed(ctx) {
            return Err(WaferError::new(
                ErrorCode::FailedPrecondition,
                "Stripe secret-key operations are disabled in the browser runtime; configure a trusted remote commerce API instead",
            ));
        }
        let secret_key = config::get_default(ctx, STRIPE_SECRET_KEY, "").await?;
        let livemode = secret_livemode(&secret_key).ok_or_else(|| {
            WaferError::new(
                ErrorCode::FailedPrecondition,
                "Stripe secret key must be a test or live secret key",
            )
        })?;
        let api_version = config::get_default(ctx, STRIPE_API_VERSION, DEFAULT_API_VERSION).await?;
        if !super::stripe::is_stable_stripe_api_version(&api_version) {
            return Err(WaferError::new(
                ErrorCode::FailedPrecondition,
                "Stripe API version must be a stable named release",
            ));
        }
        let api_url = config::get_default(ctx, STRIPE_API_URL, "https://api.stripe.com").await?;
        Ok(Self {
            secret_key,
            api_url: api_url.trim_end_matches('/').to_string(),
            api_version,
            livemode,
        })
    }

    /// Send a request and decode its JSON body, classifying any failure.
    ///
    /// Every Stripe call in the block goes through here or through
    /// [`Self::request_json_optional`], so one place decides whether a
    /// failure is retryable. Hand-rolled copies of this decision disagreed in
    /// both directions: the catalog pair called a 503 a terminal rejection,
    /// and the Payment-Link writes called a deterministic 400 retryable.
    pub(crate) async fn request_json(
        &self,
        ctx: &dyn Context,
        method: &str,
        path: &str,
        stripe_account: Option<&str>,
        idempotency_key: Option<&str>,
        form: Option<Vec<(String, String)>>,
    ) -> Result<Value, WaferError> {
        let response = self
            .send(ctx, method, path, stripe_account, idempotency_key, form)
            .await?;
        if response.status_code >= 400 {
            return Err(classify(path, response.status_code, &response.body));
        }
        decode(&response.body)
    }

    /// [`Self::request_json`] with one addition: **404 means the object is
    /// not there**, which is a fact rather than a failure.
    ///
    /// The catalog reconciliation reads a stored Stripe id to find out
    /// whether the object still exists, and a Stripe object that was deleted
    /// in the dashboard answers 404. That policy lives here rather than in
    /// the caller so no call site has to re-derive a status from an error to
    /// find out what happened.
    pub(crate) async fn request_json_optional(
        &self,
        ctx: &dyn Context,
        method: &str,
        path: &str,
        stripe_account: Option<&str>,
    ) -> Result<Option<Value>, WaferError> {
        let response = self
            .send(ctx, method, path, stripe_account, None, None)
            .await?;
        if response.status_code == 404 {
            return Ok(None);
        }
        if response.status_code >= 400 {
            return Err(classify(path, response.status_code, &response.body));
        }
        decode(&response.body).map(Some)
    }

    async fn send(
        &self,
        ctx: &dyn Context,
        method: &str,
        path: &str,
        stripe_account: Option<&str>,
        idempotency_key: Option<&str>,
        form: Option<Vec<(String, String)>>,
    ) -> Result<network::NetworkResponse, WaferError> {
        let headers = request_headers(
            &self.secret_key,
            &self.api_version,
            stripe_account,
            idempotency_key,
        );
        let body = form.map(encode_form);
        send_raw(
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
        })
    }
}

/// The one place a Stripe HTTP status becomes an error code.
///
/// 409, 429 and 5xx are ambiguous, so they classify like a transport failure
/// (`Internal`) — callers keep their durable claim and retry with the same
/// idempotency key. A 5xx may have applied the mutation before failing; a 409
/// means another request under the same key is still executing and its
/// outcome is not known yet. Only the remaining 4xx responses are definite
/// rejections that terminally fail an operation.
///
/// The provider's own body is logged, never returned: it can name the
/// connected account and the request's parameters, and this error reaches a
/// storefront buyer.
fn classify(path: &str, status_code: u16, body: &[u8]) -> WaferError {
    let decoded: Value = serde_json::from_slice(body).unwrap_or_default();
    let code = provider_error_code(&decoded);
    tracing::error!(
        status = status_code,
        path = %path,
        body = %String::from_utf8_lossy(body),
        "Stripe request failed"
    );
    if matches!(status_code, 409 | 429) || status_code >= 500 {
        return WaferError::new(
            ErrorCode::Internal,
            format!("Stripe request could not be completed (HTTP {status_code}, code {code})"),
        );
    }
    WaferError::new(
        ErrorCode::FailedPrecondition,
        format!("Stripe rejected the request (HTTP {status_code}, code {code})"),
    )
}

fn decode(body: &[u8]) -> Result<Value, WaferError> {
    serde_json::from_slice(body).map_err(|_| {
        WaferError::new(
            ErrorCode::Internal,
            "Stripe returned an unreadable response",
        )
    })
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
