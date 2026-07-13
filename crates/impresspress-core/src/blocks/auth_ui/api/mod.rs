//! JSON API handlers for the auth-ui block. One handler per leaf module;
//! routed from `auth_ui::AuthUiBlock::handle`.

use wafer_run::{context::Context, InputStream, Message};

pub mod api_keys;
pub mod bootstrap;
pub mod change_password;
pub mod forgot_password;
pub mod login;
pub mod logout;
pub mod me;
mod password_policy;
pub mod refresh;
pub mod reset_password;
pub mod signup;
pub mod sync_user;
pub mod verify;

/// Send a transactional email through the `impresspress/email` block.
///
/// Shared by the signup, email-verify and forgot-password handlers — every
/// caller builds the same `email.send_template` envelope `{template, to,
/// token}` and only the template name differs. A send failure is logged and
/// swallowed: email delivery is best-effort, and a 5xx from the email block
/// must not turn a successful signup / reset request into an error.
pub(crate) async fn send_template_email(ctx: &dyn Context, template: &str, to: &str, token: &str) {
    let req = serde_json::json!({
        "template": template,
        "to": to,
        "token": token,
    });
    let email_msg = Message {
        kind: "email.send_template".to_string(),
        meta: Vec::new(),
    };
    let body_bytes = serde_json::to_vec(&req).unwrap_or_default();
    let out = ctx
        .call_block(
            "impresspress/email",
            email_msg,
            InputStream::from_bytes(body_bytes),
        )
        .await;
    if let Err(e) = out.collect_buffered().await {
        tracing::warn!(
            template = %template,
            "Failed to send {template} email to {to}: {e:?}"
        );
    }
}
