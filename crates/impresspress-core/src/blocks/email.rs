//! Email block — sends emails via Mailgun HTTP API.
//!
//! Routes:
//! - `email.send` — Send a raw email (to, subject, html, text)
//! - `email.send_template` — Send a templated email (template name + variables)
//!
//! Uses the `wafer-run/network` block to make HTTP requests to Mailgun,
//! and `wafer-run/config` for MAILGUN_API_KEY, MAILGUN_DOMAIN, MAILGUN_FROM.

use std::{collections::HashMap, time::Duration};

use serde::{Deserialize, Serialize};
use wafer_core::clients::{config, network as net};
use wafer_run::{
    context::Context, BlockInfo, ConfigVar, InputStream, InputType, InstanceMode, LifecycleType,
    OutputStream,
};

use super::rate_limit::{RateLimit, UserRateLimiter};
use crate::{
    http::{err_bad_request, err_not_found, ok_json},
    util::urlencode,
};

/// Default per-caller rate limit: 100 emails per hour.
const DEFAULT_RATE_LIMIT_MAX: u32 = 100;
const DEFAULT_RATE_LIMIT_WINDOW_SECS: u64 = 3600;

/// Default Mailgun API base URL (US region). EU accounts use
/// `https://api.eu.mailgun.net`. Single source of truth for the
/// `IMPRESSPRESS__EMAIL__MAILGUN_BASE_URL` config var default, the admin settings
/// form, and the runtime fallback in [`resolve_base_url`].
pub(crate) const DEFAULT_MAILGUN_BASE_URL: &str = "https://api.mailgun.net";

/// Resolve the configured Mailgun base URL to an effective host: fall back to
/// the US default when unset/blank and trim any trailing slash (so the
/// `{base}/v3/...` join never produces a `//v3` double slash).
pub(crate) fn resolve_base_url(configured: &str) -> &str {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        DEFAULT_MAILGUN_BASE_URL
    } else {
        trimmed.trim_end_matches('/')
    }
}

/// The email block's own declared config vars. Single source of truth for
/// both `BlockInfo::config_keys` and the admin Email settings page (rendered
/// via `ui::settings_form`, not a parallel `EmailSettingField` tuple table —
/// see `blocks/admin/pages/email.rs`, which selects the Mailgun subset by key
/// via `config_vars::var_in` rather than re-declaring these).
pub(crate) fn config_vars() -> Vec<ConfigVar> {
    vec![
        ConfigVar::new(
            "IMPRESSPRESS__EMAIL__MAILGUN_API_KEY",
            "API key from your Mailgun account.",
            "",
        )
        .name("Mailgun API Key")
        .input_type(InputType::Password)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__EMAIL__MAILGUN_DOMAIN",
            "Sending domain configured in Mailgun (e.g. mg.example.com).",
            "",
        )
        .name("Mailgun Domain")
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__EMAIL__MAILGUN_FROM",
            "Sender address for emails. Leave empty for default (noreply@domain).",
            "",
        )
        .name("From Address")
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__EMAIL__MAILGUN_REPLY_TO",
            "Reply-to address for emails. Leave empty to omit.",
            "",
        )
        .name("Reply-To Address")
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__EMAIL__MAILGUN_BASE_URL",
            "Mailgun API base URL (US: https://api.mailgun.net, EU: https://api.eu.mailgun.net)",
            DEFAULT_MAILGUN_BASE_URL,
        )
        .name("Mailgun Base URL")
        // A real URL field (unlike the shared helper's other optional/blank
        // text fields) — SEC/SSRF: mark it `Url` so `settings_form::save_settings`
        // runs it through `validate_url_value` on write. The old hand-rolled
        // admin save loop never validated this field at all.
        .input_type(InputType::Url)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__EMAIL__RATE_LIMIT_MAX",
            "Maximum emails per caller per window (0 disables rate limiting)",
            &DEFAULT_RATE_LIMIT_MAX.to_string(),
        )
        .name("Rate Limit (max emails)")
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__EMAIL__RATE_LIMIT_WINDOW_SECS",
            "Rate limit window in seconds",
            &DEFAULT_RATE_LIMIT_WINDOW_SECS.to_string(),
        )
        .name("Rate Limit Window (seconds)")
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__EMAIL__ALLOWED_RECIPIENT_PATTERNS",
            "Comma-separated allow-list of recipient glob patterns (e.g. \
             `*@example.com,admin@*`). Empty = allow all (with startup warning).",
            "",
        )
        .name("Allowed Recipient Patterns")
        .optional(),
    ]
}

crate::impresspress_feature_block! {
    /// Email sending via the Mailgun HTTP API (`impresspress/email`).
    pub struct EmailBlock;
    fields: { limiter: UserRateLimiter },
    name: "impresspress/email",
    info: |_this| {
        BlockInfo::new("impresspress/email", "0.0.1", "service@v1", "Email sending via Mailgun")
            .instance_mode(InstanceMode::Singleton)
            .requires(vec!["wafer-run/network".into(), "wafer-run/config".into()])
            .category(wafer_run::BlockCategory::Service)
            .description("Email sending service via Mailgun HTTP API. Supports raw email sending and templated emails for verification, password reset, welcome messages, and payment notifications. Used internally by the auth block for email verification and password reset flows.")
            .config_keys(config_vars())
    },
    handle: |this, ctx, msg, input| {
        match msg.kind.as_str() {
            "email.send" => handle_send(&this.limiter, ctx, input).await,
            "email.send_template" => handle_send_template(&this.limiter, ctx, input).await,
            _ => err_not_found(&format!("unknown email op: {}", msg.kind)),
        }
    },
    lifecycle: |_this, ctx, event| {
        // No schema — the email block is stateless. The only Init work is a
        // config sanity warning (no migrations, so this does NOT go through
        // `migration_helper::lifecycle_init`).
        if event.event_type == LifecycleType::Init {
            let patterns =
                config::get_default(ctx, "IMPRESSPRESS__EMAIL__ALLOWED_RECIPIENT_PATTERNS", "").await;
            if patterns.trim().is_empty() {
                tracing::warn!(
                    "IMPRESSPRESS__EMAIL__ALLOWED_RECIPIENT_PATTERNS is unset — email block \
                     will accept any recipient address. Set this to limit who can be emailed."
                );
            }
        }
        Ok(())
    },
}

// ---------------------------------------------------------------------------
// email.send
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SendReq {
    to: String,
    subject: String,
    html: String,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Serialize)]
struct SendResp {
    sent: bool,
}

async fn handle_send(
    limiter: &UserRateLimiter,
    ctx: &dyn Context,
    input: InputStream,
) -> OutputStream {
    let raw = input.collect_to_bytes().await;
    let req: SendReq = match serde_json::from_slice(&raw) {
        Ok(r) => r,
        Err(e) => return err_bad_request(&format!("invalid email.send: {e}")),
    };

    if let Err(e) = validate_recipient(&req.to) {
        return err_bad_request(&e);
    }
    if let Err(e) = check_recipient_allowed(ctx, &req.to).await {
        return err_bad_request(&e);
    }
    if let Err(e) = check_caller_rate_limit(limiter, ctx).await {
        return e;
    }

    let sent = send_email(ctx, &req.to, &req.subject, &req.html, req.text.as_deref()).await;
    ok_json(&SendResp { sent })
}

// ---------------------------------------------------------------------------
// email.send_template
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TemplateReq {
    template: String,
    to: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    days_remaining: Option<u32>,
}

async fn handle_send_template(
    limiter: &UserRateLimiter,
    ctx: &dyn Context,
    input: InputStream,
) -> OutputStream {
    let raw = input.collect_to_bytes().await;
    let req: TemplateReq = match serde_json::from_slice(&raw) {
        Ok(r) => r,
        Err(e) => return err_bad_request(&format!("invalid email.send_template: {e}")),
    };

    if let Err(e) = validate_recipient(&req.to) {
        return err_bad_request(&e);
    }
    if let Err(e) = check_recipient_allowed(ctx, &req.to).await {
        return err_bad_request(&e);
    }
    if let Err(e) = check_caller_rate_limit(limiter, ctx).await {
        return e;
    }

    let base_url = config::get_default(
        ctx,
        "WAFER_RUN_SHARED__FRONTEND_URL",
        "http://localhost:5173",
    )
    .await;
    let site_url =
        config::get_default(ctx, "WAFER_RUN_SHARED__SITE_URL", "https://impresspress.org").await;
    let app_name = config::get_default(ctx, "WAFER_RUN_SHARED__APP_NAME", "Impresspress").await;

    let (subject, html, text) = match req.template.as_str() {
        "verification" => {
            let token = req.token.as_deref().unwrap_or("");
            let url = format!("{}/b/auth/api/verify?token={}", base_url, urlencode(token));
            (
                format!("Verify your {app_name} email"),
                email_shell(
                    "Verify your email",
                    "#1e293b",
                    r#"<p style="color:#64748b;line-height:1.6">Click the button below to verify your email address. This link expires in 24 hours.</p>"#,
                    Some((&url, "Verify Email", "#0ea5e9")),
                    Some("If you didn't create an account, you can ignore this email."),
                ),
                format!("Verify your {app_name} email: {url}"),
            )
        }
        "password_reset" => {
            let token = req.token.as_deref().unwrap_or("");
            let url = format!(
                "{}/b/auth/reset-password?token={}",
                base_url,
                urlencode(token)
            );
            (
                format!("Reset your {app_name} password"),
                email_shell(
                    "Reset your password",
                    "#1e293b",
                    r#"<p style="color:#64748b;line-height:1.6">Click the button below to reset your password. This link expires in 1 hour.</p>"#,
                    Some((&url, "Reset Password", "#0ea5e9")),
                    Some("If you didn't request a password reset, you can ignore this email."),
                ),
                format!("Reset your {app_name} password: {url}"),
            )
        }
        "payment_failed" => {
            let days = req.days_remaining.unwrap_or(7);
            let settings_url = format!("{base_url}/b/admin/#settings");
            let body = format!(
                r#"<p style="color:#64748b;line-height:1.6">We were unable to process your subscription payment. Your service will remain active for <strong>{days} more days</strong>. After that, your projects will be suspended.</p>"#
            );
            (
                format!("{app_name}: Payment failed — action required"),
                email_shell(
                    "Payment failed",
                    "#dc2626",
                    &body,
                    Some((&settings_url, "Update Payment Method", "#dc2626")),
                    Some(
                        "If you've already updated your payment method, you can ignore this email.",
                    ),
                ),
                format!(
                    "Your {app_name} payment failed. Update your payment method within {days} days."
                ),
            )
        }
        "welcome" => {
            let name = req.name.as_deref().unwrap_or("");
            let greeting = if name.is_empty() {
                "Welcome!".to_string()
            } else {
                format!("Welcome, {name}!")
            };
            let pricing_url = format!("{site_url}/pricing/");
            let dashboard_url = format!("{base_url}/b/admin/");
            let docs_url = format!("{site_url}/docs/");
            let body = format!(
                r#"<p style="color:#64748b;line-height:1.6">Your {app_name} account is ready. Here's how to get started:</p>
<ol style="color:#64748b;line-height:1.8">
<li>Choose a plan on the <a href="{pricing_url}" style="color:#0ea5e9">pricing page</a></li>
<li>Create your first project from the <a href="{dashboard_url}" style="color:#0ea5e9">dashboard</a></li>
<li>Read the <a href="{docs_url}" style="color:#0ea5e9">documentation</a></li>
</ol>"#
            );
            (
                format!("Welcome to {app_name}!"),
                email_shell(&greeting, "#1e293b", &body, None, None),
                format!("Welcome to {app_name}! Get started: {dashboard_url}"),
            )
        }
        other => {
            return err_bad_request(&format!("unknown email template: {other}"));
        }
    };

    let sent = send_email(ctx, &req.to, &subject, &html, Some(&text)).await;
    ok_json(&SendResp { sent })
}

/// Shared HTML wrapper for the templated emails: the outer card `div`
/// (font stack, max-width, padding), a colored `<h2>` heading, the
/// caller-provided body HTML, an optional CTA button
/// (`(url, label, background-color)`), and an optional small-print footnote.
/// Keeps the repeated inline-style markup in one place so each template arm
/// only supplies its content.
fn email_shell(
    heading: &str,
    heading_color: &str,
    body_html: &str,
    cta: Option<(&str, &str, &str)>,
    footnote: Option<&str>,
) -> String {
    let mut out = format!(
        r#"<div style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;max-width:500px;margin:0 auto;padding:2rem">
<h2 style="color:{heading_color}">{heading}</h2>
{body_html}"#
    );
    if let Some((url, label, background)) = cta {
        out.push('\n');
        out.push_str(&format!(
            r#"<a href="{url}" style="display:inline-block;background:{background};color:white;padding:0.75rem 1.5rem;border-radius:8px;text-decoration:none;font-weight:600;margin:1rem 0">{label}</a>"#
        ));
    }
    if let Some(note) = footnote {
        out.push('\n');
        out.push_str(&format!(
            r#"<p style="color:#64748b;font-size:0.813rem">{note}</p>"#
        ));
    }
    out.push_str("\n</div>");
    out
}

// ---------------------------------------------------------------------------
// Mailgun HTTP API
// ---------------------------------------------------------------------------

async fn send_email(
    ctx: &dyn Context,
    to: &str,
    subject: &str,
    html: &str,
    text: Option<&str>,
) -> bool {
    let api_key = config::get_default(ctx, "IMPRESSPRESS__EMAIL__MAILGUN_API_KEY", "").await;
    let domain = config::get_default(ctx, "IMPRESSPRESS__EMAIL__MAILGUN_DOMAIN", "").await;
    let from = {
        let f = config::get_default(ctx, "IMPRESSPRESS__EMAIL__MAILGUN_FROM", "").await;
        if f.is_empty() {
            format!("Impresspress <noreply@{domain}>")
        } else {
            f
        }
    };

    if api_key.is_empty() || domain.is_empty() {
        // Email not configured — don't fail the caller, but make the
        // resulting {"sent": false} diagnosable from the logs.
        tracing::warn!(
            to = %to,
            "email not sent: Mailgun is not configured (IMPRESSPRESS__EMAIL__MAILGUN_API_KEY \
             and/or IMPRESSPRESS__EMAIL__MAILGUN_DOMAIN unset)"
        );
        return false;
    }

    // Build form-encoded body
    let mut parts = vec![
        format!("from={}", urlencode(&from)),
        format!("to={}", urlencode(to)),
        format!("subject={}", urlencode(subject)),
        format!("html={}", urlencode(html)),
    ];
    let reply_to = config::get_default(ctx, "IMPRESSPRESS__EMAIL__MAILGUN_REPLY_TO", "").await;
    if !reply_to.is_empty() {
        parts.push(format!("h:Reply-To={}", urlencode(&reply_to)));
    }
    if let Some(text) = text {
        parts.push(format!("text={}", urlencode(text)));
    }
    let body = parts.join("&");

    // Base64-encode "api:{api_key}" for HTTP Basic auth.
    use base64ct::Encoding;
    let credentials = base64ct::Base64::encode_string(format!("api:{api_key}").as_bytes());

    // Call network block via the typed client. The buffered helper consumes
    // the two-frame response (header + body) and returns a typed
    // `NetworkResponse` whose `status_code` we use to decide success.
    let configured = config::get_default(ctx, "IMPRESSPRESS__EMAIL__MAILGUN_BASE_URL", "").await;
    let base = resolve_base_url(&configured);
    let url = format!("{base}/v3/{domain}/messages");
    let mut headers = HashMap::new();
    headers.insert("Authorization".to_string(), format!("Basic {credentials}"));
    headers.insert(
        "Content-Type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    );

    match net::do_request(ctx, "POST", &url, &headers, Some(body.as_bytes())).await {
        Ok(resp) => {
            let sent = (200..300).contains(&resp.status_code);
            if !sent {
                tracing::warn!(
                    status = resp.status_code,
                    to = %to,
                    "email not sent: Mailgun returned non-2xx status"
                );
            }
            sent
        }
        Err(e) => {
            tracing::warn!(error = %e, to = %to, "email not sent: Mailgun request failed");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Validation & rate limiting (SEC-051)
// ---------------------------------------------------------------------------

/// Reject blatantly malformed recipient addresses:
/// - empty
/// - missing `@`
/// - multiple `@`
/// - contains CR/LF (SMTP header injection)
/// - missing local-part or domain-part
fn validate_recipient(addr: &str) -> Result<(), String> {
    let trimmed = addr.trim();
    if trimmed.is_empty() {
        return Err("recipient address is empty".into());
    }
    if trimmed.contains('\r') || trimmed.contains('\n') {
        return Err("recipient address contains CR/LF (header injection)".into());
    }
    let at_count = trimmed.matches('@').count();
    if at_count == 0 {
        return Err("recipient address missing '@'".into());
    }
    if at_count > 1 {
        return Err("recipient address contains multiple '@'".into());
    }
    // `at_count == 1` already, but use a let-else for explicitness instead of
    // `.unwrap()`. If this somehow returned None we'd surface a clear error.
    let Some((local, domain)) = trimmed.split_once('@') else {
        return Err("recipient address missing '@'".into());
    };
    if local.is_empty() || domain.is_empty() {
        return Err("recipient address has empty local-part or domain".into());
    }
    Ok(())
}

/// Match `value` against a simple glob pattern. Supports `*` (zero-or-more
/// of any chars). Match is case-insensitive — email addresses are not
/// case-sensitive in practice.
fn glob_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.trim().to_lowercase();
    let value = value.trim().to_lowercase();
    glob_match_inner(pattern.as_bytes(), value.as_bytes())
}

fn glob_match_inner(pat: &[u8], val: &[u8]) -> bool {
    // Simple recursive matcher — patterns are short so depth is bounded.
    if pat.is_empty() {
        return val.is_empty();
    }
    if pat[0] == b'*' {
        // Match zero or more chars.
        if glob_match_inner(&pat[1..], val) {
            return true;
        }
        if val.is_empty() {
            return false;
        }
        return glob_match_inner(pat, &val[1..]);
    }
    if val.is_empty() {
        return false;
    }
    if pat[0] != val[0] {
        return false;
    }
    glob_match_inner(&pat[1..], &val[1..])
}

/// Check the recipient against `IMPRESSPRESS__EMAIL__ALLOWED_RECIPIENT_PATTERNS`.
/// Empty/unset = allow (startup warning already emitted in lifecycle).
async fn check_recipient_allowed(ctx: &dyn Context, to: &str) -> Result<(), String> {
    let patterns =
        config::get_default(ctx, "IMPRESSPRESS__EMAIL__ALLOWED_RECIPIENT_PATTERNS", "").await;
    let patterns = patterns.trim();
    if patterns.is_empty() {
        return Ok(());
    }
    for pattern in patterns.split(',') {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            continue;
        }
        if glob_match(pattern, to) {
            return Ok(());
        }
    }
    Err(format!(
        "recipient '{to}' does not match any allowed pattern"
    ))
}

/// Per-caller email rate limit. Caller identified by `ctx.caller_id()` —
/// falls back to `"unknown"` when missing (e.g. direct HTTP entry point).
async fn check_caller_rate_limit(
    limiter: &UserRateLimiter,
    ctx: &dyn Context,
) -> Result<(), OutputStream> {
    let max = config::get_default(
        ctx,
        "IMPRESSPRESS__EMAIL__RATE_LIMIT_MAX",
        &DEFAULT_RATE_LIMIT_MAX.to_string(),
    )
    .await
    .trim()
    .parse::<u32>()
    .unwrap_or(DEFAULT_RATE_LIMIT_MAX);
    if max == 0 {
        // Rate limiting disabled.
        return Ok(());
    }

    let window_secs = config::get_default(
        ctx,
        "IMPRESSPRESS__EMAIL__RATE_LIMIT_WINDOW_SECS",
        &DEFAULT_RATE_LIMIT_WINDOW_SECS.to_string(),
    )
    .await
    .trim()
    .parse::<u64>()
    .unwrap_or(DEFAULT_RATE_LIMIT_WINDOW_SECS);

    let caller = ctx.caller_id().unwrap_or("unknown");
    let key = UserRateLimiter::key(caller, "email_send");
    let limit = RateLimit {
        max_requests: max,
        window: Duration::from_secs(window_secs),
    };
    match limiter.check(ctx, &key, limit).await {
        Ok(_) => Ok(()),
        Err(retry_after) => Err(super::rate_limit::rate_limited_response(retry_after)),
    }
}

// ---------------------------------------------------------------------------
// Tests — SEC-051 rate limit + recipient allow-list + validation.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use wafer_block::{codec, wire::config as cfg_wire};
    use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

    use super::*;

    /// Minimal Context routing `wafer-run/config` `config.get` to an in-memory
    /// map. Mirrors `MockContext::handle_config_call` in products' tests but
    /// trimmed to the surface the email block needs.
    struct ConfigCtx {
        cfg: Mutex<HashMap<String, String>>,
    }

    impl ConfigCtx {
        fn new() -> Self {
            Self {
                cfg: Mutex::new(HashMap::new()),
            }
        }
        fn set(&self, k: &str, v: &str) {
            self.cfg
                .lock()
                .unwrap()
                .insert(k.to_string(), v.to_string());
        }
    }

    impl Clone for ConfigCtx {
        fn clone(&self) -> Self {
            let cfg = self.cfg.lock().unwrap().clone();
            Self {
                cfg: Mutex::new(cfg),
            }
        }
    }

    #[async_trait::async_trait]
    impl Context for ConfigCtx {
        async fn call_block(
            &self,
            block_name: &str,
            msg: Message,
            input: InputStream,
        ) -> OutputStream {
            if block_name == "wafer-run/config" && msg.kind == "config.get" {
                let data = input.collect_to_bytes().await;
                let req: cfg_wire::GetRequest = match codec::decode(&data) {
                    Ok(r) => r,
                    Err(e) => {
                        return OutputStream::error(wafer_run::WaferError::new(
                            ErrorCode::Internal,
                            e.message,
                        ));
                    }
                };
                let value = self
                    .cfg
                    .lock()
                    .unwrap()
                    .get(&req.key)
                    .cloned()
                    .unwrap_or_default();
                return match codec::encode(&cfg_wire::GetResponse { value }) {
                    Ok(bytes) => OutputStream::respond(bytes),
                    Err(e) => OutputStream::error(wafer_run::WaferError::new(
                        wafer_run::ErrorCode::Internal,
                        e.message,
                    )),
                };
            }
            OutputStream::error(wafer_run::WaferError::new(
                ErrorCode::NotFound,
                format!("unhandled call: {block_name}/{}", msg.kind),
            ))
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }
        fn clone_arc(&self) -> Arc<dyn Context> {
            Arc::new(self.clone())
        }
    }

    // ---- resolve_base_url ---------------------------------------------------

    #[test]
    fn resolve_base_url_falls_back_and_trims() {
        // Unset / blank → US default.
        assert_eq!(resolve_base_url(""), DEFAULT_MAILGUN_BASE_URL);
        assert_eq!(resolve_base_url("   "), DEFAULT_MAILGUN_BASE_URL);
        // EU region passes through unchanged.
        assert_eq!(
            resolve_base_url("https://api.eu.mailgun.net"),
            "https://api.eu.mailgun.net"
        );
        // Trailing slash trimmed so the `{base}/v3/...` join stays single-slash.
        assert_eq!(
            resolve_base_url("https://api.mailgun.net/"),
            "https://api.mailgun.net"
        );
    }

    // ---- email_shell ----------------------------------------------------------

    #[test]
    fn email_shell_renders_heading_body_cta_and_footnote() {
        let html = email_shell(
            "Test heading",
            "#1e293b",
            "<p>body content</p>",
            Some(("https://x.test/go", "Go Now", "#0ea5e9")),
            Some("Small print."),
        );
        assert!(html.starts_with(r#"<div style="font-family:"#));
        assert!(html.ends_with("</div>"));
        assert!(html.contains(r##"<h2 style="color:#1e293b">Test heading</h2>"##));
        assert!(html.contains("<p>body content</p>"));
        assert!(html.contains(r#"<a href="https://x.test/go""#));
        assert!(html.contains("background:#0ea5e9"));
        assert!(html.contains(">Go Now</a>"));
        assert!(html.contains("Small print."));
    }

    #[test]
    fn email_shell_omits_cta_and_footnote_when_absent() {
        let html = email_shell("H", "#1e293b", "<p>b</p>", None, None);
        assert!(!html.contains("<a href="), "no CTA expected: {html}");
        assert!(
            !html.contains("font-size:0.813rem"),
            "no footnote expected: {html}"
        );
    }

    // ---- validate_recipient -------------------------------------------------

    #[test]
    fn validate_recipient_accepts_normal_address() {
        assert!(validate_recipient("alice@example.com").is_ok());
        assert!(validate_recipient("a.b+tag@sub.example.co.uk").is_ok());
    }

    #[test]
    fn validate_recipient_rejects_empty() {
        assert!(validate_recipient("").is_err());
        assert!(validate_recipient("   ").is_err());
    }

    #[test]
    fn validate_recipient_rejects_missing_at() {
        assert!(validate_recipient("not-an-email").is_err());
    }

    #[test]
    fn validate_recipient_rejects_multiple_at() {
        assert!(validate_recipient("a@b@c.com").is_err());
    }

    #[test]
    fn validate_recipient_rejects_crlf_header_injection() {
        assert!(validate_recipient("alice@example.com\r\nBcc: evil@x.com").is_err());
        assert!(validate_recipient("alice@example.com\nBcc: evil@x.com").is_err());
        assert!(validate_recipient("alice@example.com\rBcc: evil@x.com").is_err());
    }

    #[test]
    fn validate_recipient_rejects_empty_parts() {
        assert!(validate_recipient("@example.com").is_err());
        assert!(validate_recipient("alice@").is_err());
    }

    // ---- glob_match ---------------------------------------------------------

    #[test]
    fn glob_match_exact_address() {
        assert!(glob_match("alice@example.com", "alice@example.com"));
        assert!(!glob_match("alice@example.com", "bob@example.com"));
    }

    #[test]
    fn glob_match_domain_wildcard() {
        assert!(glob_match("*@example.com", "alice@example.com"));
        assert!(glob_match("*@example.com", "bob@example.com"));
        assert!(!glob_match("*@example.com", "alice@other.com"));
    }

    #[test]
    fn glob_match_local_wildcard() {
        assert!(glob_match("admin@*", "admin@example.com"));
        assert!(glob_match("admin@*", "admin@other.io"));
        assert!(!glob_match("admin@*", "user@example.com"));
    }

    #[test]
    fn glob_match_case_insensitive() {
        assert!(glob_match("Alice@Example.COM", "alice@example.com"));
    }

    // ---- check_recipient_allowed -------------------------------------------

    #[tokio::test]
    async fn allow_list_empty_allows_all() {
        let ctx = ConfigCtx::new();
        // No pattern set → allow.
        assert!(check_recipient_allowed(&ctx, "anyone@anywhere.io")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn allow_list_blocks_unmatched_recipient() {
        let ctx = ConfigCtx::new();
        ctx.set(
            "IMPRESSPRESS__EMAIL__ALLOWED_RECIPIENT_PATTERNS",
            "*@example.com, admin@*",
        );
        assert!(check_recipient_allowed(&ctx, "intruder@other.io")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn allow_list_permits_matched_recipient() {
        let ctx = ConfigCtx::new();
        ctx.set(
            "IMPRESSPRESS__EMAIL__ALLOWED_RECIPIENT_PATTERNS",
            "*@example.com, admin@*",
        );
        assert!(check_recipient_allowed(&ctx, "alice@example.com")
            .await
            .is_ok());
        assert!(check_recipient_allowed(&ctx, "admin@anywhere.io")
            .await
            .is_ok());
    }

    // ---- check_caller_rate_limit -------------------------------------------

    #[tokio::test]
    async fn rate_limit_allows_under_threshold() {
        let ctx = ConfigCtx::new();
        ctx.set("IMPRESSPRESS__EMAIL__RATE_LIMIT_MAX", "3");
        ctx.set("IMPRESSPRESS__EMAIL__RATE_LIMIT_WINDOW_SECS", "60");
        let limiter = UserRateLimiter::new();
        // First 3 are allowed.
        for _ in 0..3 {
            assert!(check_caller_rate_limit(&limiter, &ctx).await.is_ok());
        }
    }

    #[tokio::test]
    async fn rate_limit_blocks_over_threshold() {
        let ctx = ConfigCtx::new();
        ctx.set("IMPRESSPRESS__EMAIL__RATE_LIMIT_MAX", "2");
        ctx.set("IMPRESSPRESS__EMAIL__RATE_LIMIT_WINDOW_SECS", "60");
        let limiter = UserRateLimiter::new();
        assert!(check_caller_rate_limit(&limiter, &ctx).await.is_ok());
        assert!(check_caller_rate_limit(&limiter, &ctx).await.is_ok());
        // 3rd send exceeds the configured cap and is rate-limited.
        assert!(check_caller_rate_limit(&limiter, &ctx).await.is_err());
    }

    #[tokio::test]
    async fn rate_limit_disabled_when_max_is_zero() {
        let ctx = ConfigCtx::new();
        ctx.set("IMPRESSPRESS__EMAIL__RATE_LIMIT_MAX", "0");
        let limiter = UserRateLimiter::new();
        // Should never block.
        for _ in 0..50 {
            assert!(check_caller_rate_limit(&limiter, &ctx).await.is_ok());
        }
    }
}
