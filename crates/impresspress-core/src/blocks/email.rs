//! Email block — sends emails via Mailgun HTTP API.
//!
//! Routes:
//! - `email.send` — Send a raw email (to, subject, html, text)
//! - `email.send_template` — Send one of the auth emails (`verification`,
//!   `password_reset`) by template name
//!
//! Uses the `wafer-run/network` block to make HTTP requests to Mailgun,
//! and `wafer-run/config` for MAILGUN_API_KEY, MAILGUN_DOMAIN, MAILGUN_FROM.

use std::{collections::HashMap, time::Duration};

use serde::{Deserialize, Serialize};
use wafer_core::clients::{config, network as net};
use wafer_run::{
    context::Context, BlockInfo, ConfigVar, ErrorCode, InputStream, InputType, InstanceMode,
    LifecycleType, OutputStream, WaferError,
};

use super::{
    crud,
    rate_limit::{RateLimit, UserRateLimiter},
};
use crate::{
    config_vars::{FRONTEND_URL_KEY, PRIMARY_COLOR_KEY},
    http::{err_bad_request, err_not_found, ok_json},
    util::urlencode,
};

/// Config key: the Mailgun account API key.
pub(crate) const MAILGUN_API_KEY: &str = "IMPRESSPRESS__EMAIL__MAILGUN_API_KEY";
/// Config key: the sending domain configured in Mailgun.
pub(crate) const MAILGUN_DOMAIN: &str = "IMPRESSPRESS__EMAIL__MAILGUN_DOMAIN";
/// Config key: the sender address; blank sends from `noreply@<domain>`.
pub(crate) const MAILGUN_FROM: &str = "IMPRESSPRESS__EMAIL__MAILGUN_FROM";
/// Config key: the reply-to address; blank omits the header.
pub(crate) const MAILGUN_REPLY_TO: &str = "IMPRESSPRESS__EMAIL__MAILGUN_REPLY_TO";
/// Config key: the Mailgun API base URL, defaulting to
/// [`DEFAULT_MAILGUN_BASE_URL`].
pub(crate) const MAILGUN_BASE_URL: &str = "IMPRESSPRESS__EMAIL__MAILGUN_BASE_URL";
/// Config key: the per-caller ceiling, defaulting to
/// [`DEFAULT_RATE_LIMIT_MAX`].
pub(crate) const RATE_LIMIT_MAX: &str = "IMPRESSPRESS__EMAIL__RATE_LIMIT_MAX";
/// Config key: the per-recipient limit, defaulting to
/// [`DEFAULT_RATE_LIMIT_PER_RECIPIENT_MAX`].
pub(crate) const RATE_LIMIT_PER_RECIPIENT_MAX: &str =
    "IMPRESSPRESS__EMAIL__RATE_LIMIT_PER_RECIPIENT_MAX";
/// Config key: the window both limits count over, defaulting to
/// [`DEFAULT_RATE_LIMIT_WINDOW_SECS`].
pub(crate) const RATE_LIMIT_WINDOW_SECS: &str = "IMPRESSPRESS__EMAIL__RATE_LIMIT_WINDOW_SECS";
/// Config key: the comma-separated recipient allow-list.
pub(crate) const ALLOWED_RECIPIENT_PATTERNS: &str =
    "IMPRESSPRESS__EMAIL__ALLOWED_RECIPIENT_PATTERNS";

/// Default per-caller rate limit: 100 emails per hour. A ceiling on what one
/// calling block can spend, not a per-recipient limit — see
/// [`DEFAULT_RATE_LIMIT_PER_RECIPIENT_MAX`].
const DEFAULT_RATE_LIMIT_MAX: u32 = 100;
/// Default per-recipient rate limit: 10 emails per hour to any one address.
///
/// Comfortably above what a real person can trigger for themselves — signup
/// verification, a resend or two (the resend endpoint has its own 60-second
/// cooldown) and a password reset — and far below the per-caller ceiling, so
/// one address can only ever spend a tenth of it.
const DEFAULT_RATE_LIMIT_PER_RECIPIENT_MAX: u32 = 10;
const DEFAULT_RATE_LIMIT_WINDOW_SECS: u64 = 3600;

/// Rate-limit bucket category for the per-recipient limit.
const RECIPIENT_LIMIT_CATEGORY: &str = "email_send_recipient";
/// Rate-limit bucket category for the per-calling-block ceiling.
const CALLER_LIMIT_CATEGORY: &str = "email_send";

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
        ConfigVar::new(MAILGUN_API_KEY, "API key from your Mailgun account.", "")
            .name("Mailgun API Key")
            .input_type(InputType::Password)
            .optional(),
        ConfigVar::new(
            MAILGUN_DOMAIN,
            "Sending domain configured in Mailgun (e.g. mg.example.com).",
            "",
        )
        .name("Mailgun Domain")
        .optional(),
        ConfigVar::new(
            MAILGUN_FROM,
            "Sender address for emails. Leave empty to send from \
             noreply@<Mailgun domain> under the App Name.",
            "",
        )
        .name("From Address")
        .optional(),
        ConfigVar::new(
            MAILGUN_REPLY_TO,
            "Reply-to address for emails. Leave empty to omit.",
            "",
        )
        .name("Reply-To Address")
        .optional(),
        ConfigVar::new(
            MAILGUN_BASE_URL,
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
            RATE_LIMIT_MAX,
            "Ceiling on emails one calling block may send per window, across \
             all recipients (0 disables this ceiling)",
            &DEFAULT_RATE_LIMIT_MAX.to_string(),
        )
        .name("Rate Limit (max emails per caller)")
        .input_type(InputType::Number)
        .optional(),
        ConfigVar::new(
            RATE_LIMIT_PER_RECIPIENT_MAX,
            "Maximum emails to any one recipient address per window (0 \
             disables the per-recipient limit). Keep it well below the \
             per-caller ceiling: it is what stops one address from spending \
             the quota every transactional email shares.",
            &DEFAULT_RATE_LIMIT_PER_RECIPIENT_MAX.to_string(),
        )
        .name("Rate Limit (max emails per recipient)")
        .input_type(InputType::Number)
        .optional(),
        ConfigVar::new(
            RATE_LIMIT_WINDOW_SECS,
            "Rate limit window in seconds, shared by both limits above",
            &DEFAULT_RATE_LIMIT_WINDOW_SECS.to_string(),
        )
        .name("Rate Limit Window (seconds)")
        .input_type(InputType::Number)
        .optional(),
        ConfigVar::new(
            ALLOWED_RECIPIENT_PATTERNS,
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
            .description("Email sending service via Mailgun HTTP API. Supports raw email sending and templated emails for email verification and password reset. Used internally by the auth block for email verification and password reset flows.")
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
            let patterns = allowed_recipient_patterns(ctx).await?;
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
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let req: SendReq = match serde_json::from_slice(&raw) {
        Ok(r) => r,
        Err(e) => return err_bad_request(&format!("invalid email.send: {e}")),
    };

    if let Err(e) = validate_recipient(&req.to) {
        return err_bad_request(&e);
    }
    if let Err(response) = check_recipient_allowed(ctx, &req.to).await {
        return response;
    }
    if let Err(e) = check_send_rate_limits(limiter, ctx, &req.to).await {
        return e;
    }

    match send_email(ctx, &req.to, &req.subject, &req.html, req.text.as_deref()).await {
        Ok(sent) => ok_json(&SendResp { sent }),
        Err(e) => crud::db_error_internal(e, "Could not read the mail settings"),
    }
}

// ---------------------------------------------------------------------------
// email.send_template
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TemplateReq {
    template: String,
    to: String,
    #[serde(default)]
    token: Option<String>,
}

async fn handle_send_template(
    limiter: &UserRateLimiter,
    ctx: &dyn Context,
    input: InputStream,
) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let req: TemplateReq = match serde_json::from_slice(&raw) {
        Ok(r) => r,
        Err(e) => return err_bad_request(&format!("invalid email.send_template: {e}")),
    };

    if let Err(e) = validate_recipient(&req.to) {
        return err_bad_request(&e);
    }
    if let Err(response) = check_recipient_allowed(ctx, &req.to).await {
        return response;
    }
    if let Err(e) = check_send_rate_limits(limiter, ctx, &req.to).await {
        return e;
    }

    let base_url = match config::get_default(ctx, FRONTEND_URL_KEY, "http://localhost:5173").await {
        Ok(base_url) => base_url,
        Err(e) => return crud::db_error_internal(e, "Could not read the frontend URL"),
    };
    let app_name = match app_name(ctx).await {
        Ok(app_name) => app_name,
        Err(e) => return crud::db_error_internal(e, "Could not read the app name"),
    };
    // Brand accent for CTA buttons and links. Same contract as the admin
    // chrome: a configured PRIMARY_COLOR wins, blank means the built-in
    // brand accent. (The old hardcoded `#0ea5e9` sky-blue predated the
    // rebrand and clashed with every other surface.)
    let accent = {
        let c = match config::get_default(ctx, PRIMARY_COLOR_KEY, "").await {
            Ok(c) => c,
            Err(e) => return crud::db_error_internal(e, "Could not read the brand colour"),
        };
        if c.trim().is_empty() {
            crate::ui::assets::BRAND_ACCENT_HEX.to_string()
        } else {
            c
        }
    };

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
                    Some((&url, "Verify Email", &accent)),
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
                    Some((&url, "Reset Password", &accent)),
                    Some("If you didn't request a password reset, you can ignore this email."),
                ),
                format!("Reset your {app_name} password: {url}"),
            )
        }
        other => {
            return err_bad_request(&format!("unknown email template: {other}"));
        }
    };

    match send_email(ctx, &req.to, &subject, &html, Some(&text)).await {
        Ok(sent) => ok_json(&SendResp { sent }),
        Err(e) => crud::db_error_internal(e, "Could not read the mail settings"),
    }
}

/// The configured display name, or the declared default when the row is
/// missing or blank — the same answer the admin settings page shows. A
/// failed read is returned.
async fn app_name(ctx: &dyn Context) -> Result<String, WaferError> {
    let name = config::get_default(
        ctx,
        crate::config_vars::APP_NAME_KEY,
        crate::config_vars::DEFAULT_APP_NAME,
    )
    .await?;
    Ok(if name.trim().is_empty() {
        crate::config_vars::DEFAULT_APP_NAME.to_string()
    } else {
        name
    })
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
) -> Result<bool, WaferError> {
    let api_key = config::get_default(ctx, MAILGUN_API_KEY, "").await?;
    let domain = config::get_default(ctx, MAILGUN_DOMAIN, "").await?;
    if api_key.is_empty() || domain.is_empty() {
        // Email not configured — don't fail the caller, but make the
        // resulting {"sent": false} diagnosable from the logs.
        tracing::warn!(
            to = %to,
            "email not sent: Mailgun is not configured (IMPRESSPRESS__EMAIL__MAILGUN_API_KEY \
             and/or IMPRESSPRESS__EMAIL__MAILGUN_DOMAIN unset)"
        );
        return Ok(false);
    }

    let from = {
        let f = config::get_default(ctx, MAILGUN_FROM, "").await?;
        if f.is_empty() {
            default_from(&app_name(ctx).await?, &domain)
        } else {
            f
        }
    };

    // Build form-encoded body
    let mut parts = vec![
        format!("from={}", urlencode(&from)),
        format!("to={}", urlencode(to)),
        format!("subject={}", urlencode(subject)),
        format!("html={}", urlencode(html)),
    ];
    let reply_to = config::get_default(ctx, MAILGUN_REPLY_TO, "").await?;
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
    let configured = config::get_default(ctx, MAILGUN_BASE_URL, "").await?;
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
            Ok(sent)
        }
        Err(e) => {
            tracing::warn!(error = %e, to = %to, "email not sent: Mailgun request failed");
            Ok(false)
        }
    }
}

/// The From mailbox used when `IMPRESSPRESS__EMAIL__MAILGUN_FROM` is unset:
/// the app name as the display name, at `noreply@{domain}`.
///
/// The app name is admin-edited free text and becomes part of a mail header,
/// so it goes through [`display_name_phrase`] rather than being pasted in —
/// a comma would otherwise split the From header into two mailboxes, and a
/// CR/LF would start a header of its own.
fn default_from(app_name: &str, domain: &str) -> String {
    let address = format!("noreply@{domain}");
    match display_name_phrase(app_name) {
        Some(phrase) => format!("{phrase} <{address}>"),
        None => address,
    }
}

/// Encode `name` as an RFC 5322 display-name phrase, or `None` when nothing
/// printable is left.
///
/// Control characters — CR and LF above all — cannot appear in a header
/// phrase, so each is turned into a space, and every run of whitespace then
/// collapses to one. Printable ASCII becomes a quoted-string with `\` and
/// `"` escaped, which keeps `,` `<` `@` `;` from being read as address
/// syntax. Anything else becomes RFC 2047 `B` encoded-words, split on
/// character boundaries so each stays within the 75-character limit.
fn display_name_phrase(name: &str) -> Option<String> {
    let cleaned = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() {
        return None;
    }
    if cleaned.is_ascii() {
        let escaped = cleaned.replace('\\', "\\\\").replace('"', "\\\"");
        return Some(format!("\"{escaped}\""));
    }
    Some(encoded_words(&cleaned))
}

/// Most UTF-8 bytes one RFC 2047 `B` encoded-word may carry: 45 bytes encode
/// to 60 base64 characters, and with the 12-character `=?UTF-8?B?` … `?=`
/// wrapper that is 72, inside the 75-character cap.
const ENCODED_WORD_MAX_BYTES: usize = 45;

/// `text` as space-separated `=?UTF-8?B?…?=` encoded-words. A chunk never
/// splits a character, since RFC 2047 requires each word to decode on its
/// own.
fn encoded_words(text: &str) -> String {
    use base64ct::Encoding;
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in text.chars() {
        if chunk.len() + c.len_utf8() > ENCODED_WORD_MAX_BYTES {
            words.push(std::mem::take(&mut chunk));
        }
        chunk.push(c);
    }
    words.push(chunk);
    words
        .iter()
        .map(|w| {
            format!(
                "=?UTF-8?B?{}?=",
                base64ct::Base64::encode_string(w.as_bytes())
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
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

/// `IMPRESSPRESS__EMAIL__ALLOWED_RECIPIENT_PATTERNS`, empty when unset.
async fn allowed_recipient_patterns(ctx: &dyn Context) -> Result<String, WaferError> {
    config::get_default(ctx, ALLOWED_RECIPIENT_PATTERNS, "").await
}

/// Check the recipient against `IMPRESSPRESS__EMAIL__ALLOWED_RECIPIENT_PATTERNS`,
/// answering the response when it is refused or the patterns cannot be read.
async fn check_recipient_allowed(ctx: &dyn Context, to: &str) -> Result<(), OutputStream> {
    let patterns = allowed_recipient_patterns(ctx)
        .await
        .map_err(|e| crud::db_error_internal(e, "Could not read the allowed recipients"))?;
    recipient_allowed(&patterns, to).map_err(OutputStream::error)
}

/// Whether `patterns` admit `to`. Empty = allow (startup warning already
/// emitted in lifecycle). A recipient no pattern admits is the caller's
/// `InvalidArgument`, a 400.
fn recipient_allowed(patterns: &str, to: &str) -> Result<(), WaferError> {
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
    Err(WaferError::new(
        ErrorCode::InvalidArgument,
        format!("recipient '{to}' does not match any allowed pattern"),
    ))
}

/// Read a numeric config value, falling back to `default` when unset or
/// unparseable. A failed read is returned.
async fn numeric_config<T>(ctx: &dyn Context, key: &str, default: T) -> Result<T, WaferError>
where
    T: std::str::FromStr + std::fmt::Display + Copy,
{
    Ok(config::get_default(ctx, key, &default.to_string())
        .await?
        .trim()
        .parse::<T>()
        .unwrap_or(default))
}

/// The rate-limit bucket identity for a recipient: the address trimmed,
/// lowercased, then hashed, so `" V@x.com"` and `"v@x.com"` share one
/// bucket.
///
/// The normalization is this function's own. [`validate_recipient`] trims a
/// copy for its own checks and never lowercases, and what goes to Mailgun is
/// the address as the caller wrote it — so a key that reused either of those
/// spellings would let padding or capitalization buy a second quota for one
/// mailbox.
///
/// Hashed because this identity is persisted on Cloudflare: `UserRateLimiter`
/// writes the composite key into the `wafer_run__auth__rate_limits` D1 table,
/// which until now held only user ids and IPs. A recipient address is
/// somebody's mailbox and does not belong in a table that exists to count
/// requests — the same reason `users.reset_token_hash` stores a digest. The
/// bucket only ever needs equality, which a digest preserves.
fn recipient_bucket_key(to: &str) -> String {
    crate::util::sha256_hex(to.trim().to_lowercase().as_bytes())
}

/// Outbound rate limits for one send: a per-recipient bucket and a
/// per-calling-block ceiling. Either limit set to `0` disables that bucket.
///
/// The recipient bucket is checked — and charged — FIRST, and a refusal
/// there returns without touching the caller bucket. That ordering is the
/// whole point. With only the caller bucket, every transactional email the
/// auth block sends (signup verification, resend, password reset) shared one
/// 100-per-hour quota keyed on the *sending block*, so anyone who could make
/// that block mail an address they own — `POST /b/auth/api/forgot-password`
/// against their own account, at the 30-per-minute rate the auth routes
/// allow — emptied it in about four minutes, and every other user's
/// verification and reset mail 429'd until the window rolled over. Charging
/// the shared ceiling only for mail the recipient bucket admitted caps any
/// one address's share of it at the per-recipient limit.
///
/// The recipient key is [`recipient_bucket_key`] — the trimmed, lowercased
/// address, hashed — so neither case nor surrounding whitespace opens a
/// second bucket for the same mailbox. The caller is `ctx.caller_id()`,
/// falling back to `"unknown"` when missing (a direct entry point with no
/// calling block).
async fn check_send_rate_limits(
    limiter: &UserRateLimiter,
    ctx: &dyn Context,
    to: &str,
) -> Result<(), OutputStream> {
    let unreadable = |e| crud::db_error_internal(e, "Could not read the mail rate limits");
    let window = Duration::from_secs(
        numeric_config(ctx, RATE_LIMIT_WINDOW_SECS, DEFAULT_RATE_LIMIT_WINDOW_SECS)
            .await
            .map_err(unreadable)?,
    );

    let per_recipient_max = numeric_config(
        ctx,
        RATE_LIMIT_PER_RECIPIENT_MAX,
        DEFAULT_RATE_LIMIT_PER_RECIPIENT_MAX,
    )
    .await
    .map_err(unreadable)?;
    if per_recipient_max > 0 {
        let key = UserRateLimiter::key(&recipient_bucket_key(to), RECIPIENT_LIMIT_CATEGORY);
        let limit = RateLimit {
            max_requests: per_recipient_max,
            window,
        };
        if let Err(retry_after) = limiter.check(ctx, &key, limit).await {
            // Routine, and self-inflicted by whoever is asking for the mail:
            // this address has already had `per_recipient_max` messages this
            // window. Nobody else's mail is affected.
            tracing::warn!(
                to = %to,
                max = per_recipient_max,
                retry_after,
                "email send refused: recipient reached its per-window limit"
            );
            return Err(super::rate_limit::rate_limited_response(retry_after));
        }
    }

    let caller_max = numeric_config(ctx, RATE_LIMIT_MAX, DEFAULT_RATE_LIMIT_MAX)
        .await
        .map_err(unreadable)?;
    if caller_max > 0 {
        let caller = ctx.caller_id().unwrap_or("unknown");
        let key = UserRateLimiter::key(caller, CALLER_LIMIT_CATEGORY);
        let limit = RateLimit {
            max_requests: caller_max,
            window,
        };
        if let Err(retry_after) = limiter.check(ctx, &key, limit).await {
            // A different class of event entirely: the deployment-wide
            // ceiling is gone, so transactional mail is now failing for
            // every recipient this block serves. Logged at `error` because
            // it needs an operator, not a retry.
            tracing::error!(
                caller = %caller,
                max = caller_max,
                retry_after,
                "email send refused: the per-caller outbound ceiling is exhausted — \
                 transactional mail is failing for every recipient of this caller"
            );
            return Err(super::rate_limit::rate_limited_response(retry_after));
        }
    }

    Ok(())
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
    use wafer_core::interfaces::network::service::{
        NetworkError, NetworkService, Request as NetRequest, Response as NetResponse,
    };
    use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

    use super::*;

    /// Minimal Context routing `wafer-run/config` `config.get` to an in-memory
    /// map. Mirrors `MockContext::handle_config_call` in products' tests but
    /// trimmed to the surface the email block needs. `wafer-run/network` is
    /// answered only when a test installs a block for it
    /// ([`ConfigCtx::with_mailgun`]).
    struct ConfigCtx {
        cfg: Mutex<HashMap<String, String>>,
        network: Option<Arc<dyn wafer_run::Block>>,
    }

    impl ConfigCtx {
        fn new() -> Self {
            Self {
                cfg: Mutex::new(HashMap::new()),
                network: None,
            }
        }

        /// A context with Mailgun configured and `wafer-run/network` served by
        /// the real `NetworkBlock` over a recording transport that answers
        /// 200, so a test can read the exact request the block sent.
        fn with_mailgun() -> (Self, Arc<Mutex<Vec<NetRequest>>>) {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let mut ctx = Self::new();
            ctx.network = Some(Arc::new(
                wafer_core::service_blocks::network::NetworkBlock::new(Arc::new(
                    RecordingMailgun {
                        requests: requests.clone(),
                    },
                )),
            ));
            ctx.set(MAILGUN_API_KEY, "key-test");
            ctx.set(MAILGUN_DOMAIN, "mg.example.com");
            (ctx, requests)
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
                network: self.network.clone(),
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
                let data = match input.collect_to_bytes().await {
                    Ok(bytes) => bytes,
                    Err(e) => return OutputStream::error(e),
                };
                let req: cfg_wire::GetRequest = match codec::decode(&data) {
                    Ok(r) => r,
                    Err(e) => return OutputStream::error(e.invalid_argument()),
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
            if block_name == "wafer-run/network" {
                if let Some(network) = &self.network {
                    return network.handle(self, msg, input).await;
                }
            }
            OutputStream::error(wafer_run::WaferError::new(
                ErrorCode::Unimplemented,
                format!("unhandled call: {block_name}/{}", msg.kind),
            ))
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }
        /// Outbound network access is what the email block is granted in
        /// production; this context stands in for that grant and nothing
        /// else.
        fn check_resource_access(
            &self,
            _resource: &str,
            resource_type: wafer_run::ResourceType,
            _access: wafer_block::ResourceAccess,
        ) -> Result<(), wafer_run::WaferError> {
            if resource_type == wafer_run::ResourceType::Network {
                Ok(())
            } else {
                Err(wafer_run::WaferError::new(
                    ErrorCode::PermissionDenied,
                    "ConfigCtx grants network access only",
                ))
            }
        }
        fn resource_access_admitted(
            &self,
            _resource: &str,
            resource_type: wafer_run::ResourceType,
            _access: wafer_block::ResourceAccess,
        ) -> bool {
            resource_type == wafer_run::ResourceType::Network
        }
        fn clone_arc(&self) -> Arc<dyn Context> {
            Arc::new(self.clone())
        }
    }

    /// A Mailgun stand-in: records every request and accepts it.
    struct RecordingMailgun {
        requests: Arc<Mutex<Vec<NetRequest>>>,
    }

    #[async_trait::async_trait]
    impl NetworkService for RecordingMailgun {
        async fn do_request(&self, request: &NetRequest) -> Result<NetResponse, NetworkError> {
            self.requests.lock().unwrap().push(request.clone());
            Ok(NetResponse {
                status_code: 200,
                headers: HashMap::new(),
                body: br#"{"message":"Queued. Thank you."}"#.to_vec(),
            })
        }
    }

    /// Drive `email.send_template` through the block's real dispatch and
    /// return the HTTP status it answered with and its body.
    async fn send_template(ctx: &ConfigCtx, body: serde_json::Value) -> (u16, String) {
        let out = wafer_run::Block::handle(
            &EmailBlock::new(),
            ctx,
            Message {
                kind: "email.send_template".to_string(),
                meta: Vec::new(),
            },
            InputStream::from_bytes(serde_json::to_vec(&body).expect("serialize body")),
        )
        .await;
        let parts = wafer_block::http_codec::collect_http_response(out).await;
        (
            parts.status,
            String::from_utf8(parts.body).expect("UTF-8 body"),
        )
    }

    /// The `from` field of the one form-encoded request sent to Mailgun.
    fn sent_from(requests: &Mutex<Vec<NetRequest>>) -> String {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "exactly one Mailgun request");
        let body = requests[0].body.as_deref().expect("a form body");
        crate::util::parse_form_body(body)
            .remove("from")
            .expect("a from field")
    }

    /// Decode one RFC 2047 `=?UTF-8?B?…?=` word, failing on anything else.
    fn decode_encoded_word(word: &str) -> String {
        use base64ct::Encoding;
        let b64 = word
            .strip_prefix("=?UTF-8?B?")
            .and_then(|w| w.strip_suffix("?="))
            .unwrap_or_else(|| panic!("not a UTF-8 B encoded-word: {word}"));
        String::from_utf8(base64ct::Base64::decode_vec(b64).expect("valid base64"))
            .expect("each word decodes to UTF-8 on its own")
    }

    // ---- templates ------------------------------------------------------------

    /// Only the two auth templates exist. `welcome` and `payment_failed` had
    /// no sender anywhere, and `welcome` linked to a hardcoded marketing
    /// domain; asking for either is now what asking for any unknown template
    /// is — a 400, and nothing is sent.
    #[tokio::test]
    async fn a_template_nothing_sends_is_refused() {
        for template in ["welcome", "payment_failed"] {
            let (ctx, requests) = ConfigCtx::with_mailgun();
            let (status, body) = send_template(
                &ctx,
                serde_json::json!({"template": template, "to": "a@example.com"}),
            )
            .await;
            assert_eq!(status, 400, "{template}: {body}");
            assert!(
                body.contains("unknown email template"),
                "{template}: {body}"
            );
            assert!(
                requests.lock().unwrap().is_empty(),
                "{template}: nothing may reach Mailgun"
            );
        }
    }

    /// The live templates still send, through the same recording transport,
    /// so the refusal above is about the template and not the harness.
    #[tokio::test]
    async fn the_auth_templates_still_send() {
        for template in ["verification", "password_reset"] {
            let (ctx, requests) = ConfigCtx::with_mailgun();
            let (status, body) = send_template(
                &ctx,
                serde_json::json!({"template": template, "to": "a@example.com", "token": "t"}),
            )
            .await;
            assert_eq!(status, 200, "{template}: {body}");
            assert!(body.contains(r#""sent":true"#), "{template}: {body}");
            assert_eq!(requests.lock().unwrap().len(), 1, "{template}");
        }
    }

    // ---- From header ------------------------------------------------------------

    /// With no `MAILGUN_FROM` set, the display name on a sent mail is the
    /// deployment's configured App Name — not the product's name spelled in
    /// this block.
    #[tokio::test]
    async fn the_default_from_carries_the_configured_app_name() {
        let (ctx, requests) = ConfigCtx::with_mailgun();
        ctx.set(crate::config_vars::APP_NAME_KEY, "Acme Mail");
        let (status, body) = send_template(
            &ctx,
            serde_json::json!({"template": "verification", "to": "a@example.com", "token": "t"}),
        )
        .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            sent_from(&requests),
            r#""Acme Mail" <noreply@mg.example.com>"#
        );
    }

    /// A blank App Name falls back to the declared default rather than
    /// sending an empty display name.
    #[tokio::test]
    async fn a_blank_app_name_sends_under_the_declared_default() {
        let (ctx, requests) = ConfigCtx::with_mailgun();
        ctx.set(crate::config_vars::APP_NAME_KEY, "   ");
        send_template(
            &ctx,
            serde_json::json!({"template": "verification", "to": "a@example.com", "token": "t"}),
        )
        .await;
        assert_eq!(
            sent_from(&requests),
            format!(
                "\"{}\" <noreply@mg.example.com>",
                crate::config_vars::DEFAULT_APP_NAME
            )
        );
    }

    /// An App Name carrying CR/LF, a comma and a quote still produces ONE
    /// mailbox on ONE header line: the line break cannot start a `Bcc:`
    /// header, and the comma cannot start a second address.
    #[tokio::test]
    async fn a_hostile_app_name_cannot_inject_a_header_or_a_mailbox() {
        let (ctx, requests) = ConfigCtx::with_mailgun();
        ctx.set(
            crate::config_vars::APP_NAME_KEY,
            "Acme, \"Inc\"\r\nBcc: victim@evil.test",
        );
        send_template(
            &ctx,
            serde_json::json!({"template": "password_reset", "to": "a@example.com", "token": "t"}),
        )
        .await;
        let from = sent_from(&requests);
        assert!(!from.contains('\r') && !from.contains('\n'), "{from}");
        assert_eq!(
            from,
            r#""Acme, \"Inc\" Bcc: victim@evil.test" <noreply@mg.example.com>"#
        );
    }

    /// An explicitly configured `MAILGUN_FROM` is sent as the operator wrote
    /// it; the App Name only fills the default.
    #[tokio::test]
    async fn a_configured_from_address_wins() {
        let (ctx, requests) = ConfigCtx::with_mailgun();
        ctx.set(crate::config_vars::APP_NAME_KEY, "Acme Mail");
        ctx.set(MAILGUN_FROM, "Support <help@acme.test>");
        send_template(
            &ctx,
            serde_json::json!({"template": "verification", "to": "a@example.com", "token": "t"}),
        )
        .await;
        assert_eq!(sent_from(&requests), "Support <help@acme.test>");
    }

    #[test]
    fn plain_ascii_names_are_quoted_and_escaped() {
        assert_eq!(
            default_from("Acme", "mg.x.test"),
            r#""Acme" <noreply@mg.x.test>"#
        );
        // `,` `<` `@` `;` are address syntax outside a quoted-string.
        assert_eq!(
            default_from("A, B <c@d>; e", "mg.x.test"),
            r#""A, B <c@d>; e" <noreply@mg.x.test>"#
        );
        // `"` and `\` are the two characters a quoted-string must escape.
        assert_eq!(
            default_from(r#"Say "hi" \ bye"#, "mg.x.test"),
            r#""Say \"hi\" \\ bye" <noreply@mg.x.test>"#
        );
    }

    #[test]
    fn control_characters_become_single_spaces() {
        assert_eq!(
            default_from("  Acme\r\n\tMail\u{0}  ", "mg.x.test"),
            r#""Acme Mail" <noreply@mg.x.test>"#
        );
        assert_eq!(
            default_from("\u{85}Acme\u{2028}Mail", "mg.x.test"),
            r#""Acme Mail" <noreply@mg.x.test>"#
        );
    }

    #[test]
    fn a_name_with_nothing_printable_sends_the_bare_address() {
        assert_eq!(default_from("", "mg.x.test"), "noreply@mg.x.test");
        assert_eq!(default_from(" \r\n\t ", "mg.x.test"), "noreply@mg.x.test");
    }

    #[test]
    fn non_ascii_names_are_rfc2047_encoded_words() {
        let from = default_from("Café \"Zoë\", Ltd", "mg.x.test");
        let (phrase, address) = from.split_once(" <").expect("phrase then address");
        assert_eq!(address, "noreply@mg.x.test>");
        assert!(from.is_ascii(), "{from}");
        assert_eq!(decode_encoded_word(phrase), "Café \"Zoë\", Ltd");
    }

    /// A long non-ASCII name splits into several words, each inside RFC
    /// 2047's 75-character cap and each decoding on its own — no multi-byte
    /// character is cut in half.
    #[test]
    fn long_non_ascii_names_split_on_character_boundaries() {
        let name = "Ärzte-Genossenschaft für Übersetzungen und Größenordnungen 日本語テキスト";
        let phrase = display_name_phrase(name).expect("printable");
        let words: Vec<&str> = phrase.split(' ').collect();
        assert!(words.len() > 1, "{phrase}");
        for word in &words {
            assert!(word.len() <= 75, "{word} is {} chars", word.len());
        }
        let decoded: String = words.iter().map(|w| decode_encoded_word(w)).collect();
        assert_eq!(decoded, name);
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
        let patterns = allowed_recipient_patterns(&ctx).await.expect("read");
        assert!(recipient_allowed(&patterns, "anyone@anywhere.io").is_ok());
    }

    #[tokio::test]
    async fn allow_list_blocks_unmatched_recipient() {
        let ctx = ConfigCtx::new();
        ctx.set(ALLOWED_RECIPIENT_PATTERNS, "*@example.com, admin@*");
        let patterns = allowed_recipient_patterns(&ctx).await.expect("read");
        let refusal = recipient_allowed(&patterns, "intruder@other.io")
            .expect_err("an unmatched recipient is refused");
        assert_eq!(
            refusal.code,
            ErrorCode::InvalidArgument,
            "the caller's recipient, so a 400"
        );
    }

    #[tokio::test]
    async fn allow_list_permits_matched_recipient() {
        let ctx = ConfigCtx::new();
        ctx.set(ALLOWED_RECIPIENT_PATTERNS, "*@example.com, admin@*");
        let patterns = allowed_recipient_patterns(&ctx).await.expect("read");
        assert!(recipient_allowed(&patterns, "alice@example.com").is_ok());
        assert!(recipient_allowed(&patterns, "admin@anywhere.io").is_ok());
    }

    // ---- rate limits, driven through the real `email.send_template` op ----

    /// Send one templated email through the block's real dispatch (the same
    /// `email.send_template` op `auth_ui::api::send_template_email` calls)
    /// and report the HTTP status it answered with. 429 = rate limited; 200
    /// = admitted (the Mailgun call itself fails here, since `ConfigCtx`
    /// routes no `wafer-run/network`, and that is reported in the body as
    /// `{"sent": false}` — the distinction this assertion does not need).
    async fn send_status(block: &EmailBlock, ctx: &ConfigCtx, to: &str) -> u16 {
        let body = serde_json::json!({
            "template": "verification",
            "to": to,
            "token": "t0ken",
        });
        let out = wafer_run::Block::handle(
            block,
            ctx,
            Message {
                kind: "email.send_template".to_string(),
                meta: Vec::new(),
            },
            InputStream::from_bytes(serde_json::to_vec(&body).expect("serialize body")),
        )
        .await;
        crate::test_support::output_http_status(out).await
    }

    fn limited_ctx(per_recipient: &str, per_caller: &str) -> ConfigCtx {
        let ctx = ConfigCtx::new();
        ctx.set(RATE_LIMIT_PER_RECIPIENT_MAX, per_recipient);
        ctx.set(RATE_LIMIT_MAX, per_caller);
        ctx.set(RATE_LIMIT_WINDOW_SECS, "60");
        ctx
    }

    /// The bug this block shipped with: one bucket keyed on the *calling*
    /// block, so every transactional email shared one quota. An attacker
    /// hitting forgot-password against an address they own emptied it, and
    /// everyone else's signup verification and password-reset mail 429'd.
    ///
    /// Here the attacker's address is allowed 2 and then refused; the
    /// refusals must not be charged to the shared ceiling, so an unrelated
    /// recipient still gets through with the ceiling set to exactly the
    /// number of sends the recipient bucket admitted plus one.
    #[tokio::test]
    async fn one_flooded_recipient_cannot_starve_everyone_else() {
        let block = EmailBlock::new();
        let ctx = limited_ctx("2", "3");

        assert_eq!(send_status(&block, &ctx, "attacker@example.com").await, 200);
        assert_eq!(send_status(&block, &ctx, "attacker@example.com").await, 200);
        for attempt in 0..5 {
            assert_eq!(
                send_status(&block, &ctx, "attacker@example.com").await,
                429,
                "attempt {attempt} past the per-recipient limit must be refused"
            );
        }

        assert_eq!(
            send_status(&block, &ctx, "victim@example.com").await,
            200,
            "a recipient who triggered nothing must still receive mail: the \
             refused sends above must not have been charged to the shared \
             per-caller ceiling"
        );
    }

    /// The per-recipient limit is also a mail-bomb limit: with the caller
    /// ceiling wide open, one address still stops at its own cap.
    #[tokio::test]
    async fn one_recipient_cannot_be_mail_bombed() {
        let block = EmailBlock::new();
        let ctx = limited_ctx("2", "0");

        assert_eq!(send_status(&block, &ctx, "target@example.com").await, 200);
        assert_eq!(send_status(&block, &ctx, "target@example.com").await, 200);
        assert_eq!(send_status(&block, &ctx, "target@example.com").await, 429);
    }

    /// Neither case nor surrounding whitespace is a second mailbox --
    /// `validate_recipient` trims before accepting the address, so the bucket
    /// key has to trim too or `" v@x.com"` buys a fresh quota. Reachable by
    /// any block granted `email.send`, which sets its own `to`.
    #[tokio::test]
    async fn the_recipient_bucket_ignores_surrounding_whitespace() {
        let block = EmailBlock::new();
        let ctx = limited_ctx("2", "0");

        assert_eq!(send_status(&block, &ctx, "bob@example.com").await, 200);
        assert_eq!(send_status(&block, &ctx, "  bob@example.com ").await, 200);
        assert_eq!(send_status(&block, &ctx, " bob@example.com").await, 429);
    }

    /// Case is not a second mailbox: `Alice@` and `alice@` share one bucket.
    #[tokio::test]
    async fn the_recipient_bucket_is_case_insensitive() {
        let block = EmailBlock::new();
        let ctx = limited_ctx("2", "0");

        assert_eq!(send_status(&block, &ctx, "alice@example.com").await, 200);
        assert_eq!(send_status(&block, &ctx, "ALICE@Example.com").await, 200);
        assert_eq!(send_status(&block, &ctx, "Alice@example.COM").await, 429);
    }

    /// The per-caller ceiling still bounds a spread-out sender — one address
    /// each, under the per-recipient limit, and the ceiling is what stops it.
    #[tokio::test]
    async fn the_per_caller_ceiling_still_bounds_many_recipients() {
        let block = EmailBlock::new();
        let ctx = limited_ctx("10", "2");

        assert_eq!(send_status(&block, &ctx, "a@example.com").await, 200);
        assert_eq!(send_status(&block, &ctx, "b@example.com").await, 200);
        assert_eq!(send_status(&block, &ctx, "c@example.com").await, 429);
    }

    /// `0` disables a limit; with both at `0` nothing is ever refused.
    #[tokio::test]
    async fn zero_disables_a_limit() {
        let block = EmailBlock::new();
        let ctx = limited_ctx("0", "0");
        for attempt in 0..25 {
            assert_eq!(
                send_status(&block, &ctx, "anyone@example.com").await,
                200,
                "attempt {attempt}"
            );
        }
    }
}
