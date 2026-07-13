//! GET/POST /b/auth/api/verify and POST /b/auth/api/resend-verification —
//! relocated from auth/login.rs in Task 5.

use maud::html;
use wafer_core::clients::crypto;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::auth::{brand_panel, repo::users},
    http::{err_bad_request, err_internal, ok_json},
    ui,
    ui::templates::auth_split,
    util::{hex_encode, sha256_hex},
};

pub async fn handle(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let logo_url = ctx
        .config_get("WAFER_RUN_SHARED__AUTH_LOGO_URL")
        .unwrap_or("")
        .to_string();
    let app_name = ctx
        .config_get("WAFER_RUN_SHARED__APP_NAME")
        .unwrap_or("Impresspress")
        .to_string();

    // Token comes from query param or body
    let token = {
        let q = msg.get_meta("req.query.token").to_string();
        if !q.is_empty() {
            q
        } else {
            #[derive(serde::Deserialize)]
            struct Req {
                token: String,
            }
            let raw = input.collect_to_bytes().await;
            match serde_json::from_slice::<Req>(&raw) {
                Ok(r) => r.token,
                Err(_) => return err_bad_request("Missing verification token"),
            }
        }
    };

    if token.is_empty() {
        return err_bad_request("Missing verification token");
    }

    // Find user by verification token. The DB column stores
    // `sha256_hex(raw)`; hash the supplied token the same way before
    // comparing.
    let Ok(Some(user)) =
        users::find_by_verification_token(ctx, &sha256_hex(token.as_bytes())).await
    else {
        return html_respond(
            "Invalid Link",
            "This verification link is invalid or has expired. Please request a new one.",
            false,
            &logo_url,
            &app_name,
        );
    };

    if user.email_verified {
        return html_respond(
            "Email Already Verified",
            "Your email has already been verified. You can sign in now.",
            true,
            &logo_url,
            &app_name,
        );
    }

    // Mark as verified + clear token in one typed write.
    if let Err(e) = users::mark_email_verified(ctx, &user.id).await {
        return err_internal("Failed to verify email", e.to_string());
    }

    html_respond(
        "Email Verified",
        "Your email has been verified successfully. You can now sign in.",
        true,
        &logo_url,
        &app_name,
    )
}

pub async fn handle_resend(ctx: &dyn Context, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        email: String,
    }
    let raw = input.collect_to_bytes().await;
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    let email_lower = body.email.trim().to_lowercase();
    let safe_msg = "If that email is registered, a verification link has been sent.";

    let Ok(Some(user)) = users::find_by_email(ctx, &email_lower).await else {
        return ok_json(&serde_json::json!({"message": safe_msg}));
    };

    if user.email_verified {
        return ok_json(&serde_json::json!({"message": "Email is already verified."}));
    }

    // Rate limit: 60 second cooldown
    let last_sent = users::last_verification_sent(ctx, &user.id)
        .await
        .unwrap_or_default();
    if !last_sent.is_empty() {
        if let Ok(last) = chrono::DateTime::parse_from_rfc3339(&last_sent) {
            let elapsed = chrono::Utc::now() - last.with_timezone(&chrono::Utc);
            let remaining = 60 - elapsed.num_seconds();
            if remaining > 0 {
                return ok_json(&serde_json::json!({
                    "message": format!("Please wait {} seconds before requesting another email.", remaining),
                    "retry_after": remaining
                }));
            }
        }
    }

    // Generate new token. The raw token goes in the email link; only its
    // SHA-256 hex digest is persisted so a row-read leak doesn't grant
    // verification.
    let new_token = match crypto::random_bytes(ctx, 32).await {
        Ok(bytes) => hex_encode(&bytes),
        Err(e) => return err_internal("Token generation failed", e),
    };
    let new_token_hash = sha256_hex(new_token.as_bytes());

    let now = crate::util::now_rfc3339();
    if let Err(e) = users::set_verification_token(ctx, &user.id, &new_token_hash, &now).await {
        return err_internal("Failed to update token", e.to_string());
    }

    super::send_template_email(ctx, "verification", &email_lower, &new_token).await;

    ok_json(&serde_json::json!({"message": safe_msg}))
}

/// Return an HTML page response (for verify endpoints opened in browser).
fn html_respond(
    title: &str,
    message: &str,
    success: bool,
    logo_url: &str,
    app_name: &str,
) -> OutputStream {
    let color = if success { "#10b981" } else { "#ef4444" };
    let config = ui::SiteConfig {
        app_name: app_name.to_string(),
        logo_url: logo_url.to_string(),
        logo_icon_url: String::new(),
        favicon_url: crate::ui::assets::favicon_url().to_string(),
        embedded_scripts: Vec::new(),
    };
    let markup = ui::layout::page(
        title,
        &config,
        auth_split(
            brand_panel(&config, "Verify your email."),
            html! {
                div .login-container {
                    div .login-logo {
                        @if !logo_url.is_empty() {
                            img .logo-image src=(logo_url) alt=(app_name);
                        }
                    }
                    div style="text-align:center" {
                        div style={"width:48px;height:48px;background:" (color) "15;border-radius:50%;display:flex;align-items:center;justify-content:center;margin:0 auto 1rem;font-size:1.5rem;color:" (color)} {
                            @if success { "✓" } @else { "✗" }
                        }
                        h2 style="font-size:1.25rem;font-weight:700;margin:0 0 .5rem" { (title) }
                        p .login-subtitle style="line-height:1.6;margin:0 0 1.5rem" { (message) }
                        a .login-button href="/b/auth/login" style="display:inline-block;width:auto;padding:.625rem 1.25rem;text-decoration:none" {
                            "Go to Sign In"
                        }
                    }
                }
            },
        ),
    );
    ui::html_response(markup)
}
