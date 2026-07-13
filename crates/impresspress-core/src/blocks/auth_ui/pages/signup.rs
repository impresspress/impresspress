//! GET /b/auth/signup — relocated from auth/pages/mod.rs::signup_page in Task 5.

use maud::{html, PreEscaped};
use wafer_run::{context::Context, Message, OutputStream};

use super::{pw_field, pw_toggle_js, signup_script, site_config};
use crate::{
    blocks::{auth::brand_panel, auth_ui::redirect::is_safe_local_redirect},
    ui::{self, templates::auth_split},
};

pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let config = site_config(ctx);
    let app_name = &config.app_name;
    let allow_signup = ctx
        .config_get("WAFER_RUN_SHARED__ALLOW_SIGNUP")
        .unwrap_or("true")
        == "true";
    let raw_redirect = msg.get_meta("req.query.redirect").to_string();
    // Validate redirect — only allow relative paths (prevent open redirect)
    let redirect = if is_safe_local_redirect(&raw_redirect) {
        raw_redirect
    } else {
        String::new()
    };
    let logo_url = &config.logo_url;

    if !allow_signup {
        return super::login::handle(ctx, msg).await;
    }

    let redirect_qs = if redirect.is_empty() {
        String::new()
    } else {
        format!("?redirect={redirect}")
    };

    let markup = ui::layout::page(
        "Sign Up",
        &config,
        auth_split(
            brand_panel(&config, "Create your account."),
            html! {
                div .login-container {
                    div .login-logo {
                        @if !logo_url.is_empty() {
                            img .logo-image src=(logo_url) alt=(app_name);
                        } @else {
                            span .login-app-name { (app_name) }
                        }
                        p .login-subtitle { "Create your " (app_name) " account" }
                    }

                    div #error .login-error style="display:none" {}

                    div #success style="text-align:center;display:none" {
                        div style="width:48px;height:48px;background:#ecfdf5;border-radius:50%;display:flex;align-items:center;justify-content:center;margin:0 auto 1rem;font-size:1.5rem;color:#10b981" { "✓" }
                        h2 style="font-size:1.25rem;font-weight:700;margin:0 0 .5rem" { "Check your email" }
                        p #verify-msg style="font-size:.875rem;color:#64748b;line-height:1.6;margin:0 0 1.5rem" {}
                        a #back-to-signin .login-button href={"/b/auth/login" (redirect_qs)} style="display:inline-block;width:auto;padding:.625rem 1.25rem;text-decoration:none" {
                            "Back to Sign In"
                        }
                    }

                    form #form .login-form onsubmit="return handleSignup(event)" {
                        input type="hidden" #redirect value=(redirect);

                        div .form-group {
                            label .form-label for="email" { "Email" }
                            input .form-input type="email" #email placeholder="you@example.com" required;
                        }

                        div .form-group {
                            label .form-label for="password" { "Password" }
                            (pw_field("password", "Min 8 characters", Some("8")))
                        }

                        button .login-button type="submit" #btn { "Create Account" }
                    }

                    div #signin-link .signup-link {
                        "Already have an account? "
                        a href={"/b/auth/login" (redirect_qs)} { "Sign in" }
                    }
                }

                script { (PreEscaped(pw_toggle_js())) }
                script { (PreEscaped(signup_script())) }
            },
        ),
    );

    ui::html_response(markup)
}

#[cfg(test)]
mod tests {
    use wafer_run::Message;

    use super::handle;
    use crate::test_support::{output_html, TestContext};

    /// A11y: the visible "Email"/"Password" labels must be programmatically
    /// associated with their inputs via `<label for>` + matching `id`, not
    /// bare `<div>`s a screen reader can't tie to the field.
    #[tokio::test]
    async fn email_and_password_labels_are_associated_with_their_inputs() {
        let ctx = TestContext::new().await;
        let msg = Message::new("http.request");
        let html = output_html(handle(&ctx, &msg).await).await;

        assert!(
            html.contains(r#"<label class="form-label" for="email">Email</label>"#),
            "email label must be a <label for=\"email\"> tied to the #email input: {html}"
        );
        assert!(
            html.contains(r#"id="email""#),
            "input must carry the id the label's for= references: {html}"
        );

        assert!(
            html.contains(r#"<label class="form-label" for="password">Password</label>"#),
            "password label must be a <label for=\"password\"> tied to the #password input: {html}"
        );
        assert!(
            html.contains(r#"id="password""#),
            "input must carry the id the label's for= references: {html}"
        );
    }

    /// A11y: the icon-only password-reveal toggle must have an accessible
    /// name (aria-label), since it renders no visible text.
    #[tokio::test]
    async fn password_toggle_button_has_non_empty_aria_label() {
        let ctx = TestContext::new().await;
        let msg = Message::new("http.request");
        let html = output_html(handle(&ctx, &msg).await).await;

        let marker = "class=\"pw-toggle\"";
        let idx = html
            .find(marker)
            .expect("password toggle button must be present");
        let tag_end = html[idx..]
            .find('>')
            .map(|end| idx + end)
            .unwrap_or(html.len());
        let button_tag = &html[idx..tag_end];

        assert!(
            button_tag.contains("aria-label=\"") && !button_tag.contains("aria-label=\"\""),
            "password toggle button must have a non-empty aria-label: {button_tag}"
        );
    }

    /// A11y (regression guard): the signup page's brand-panel tagline must
    /// be signup-appropriate, not a copy-paste of the login copy. Fixed
    /// upstream in 5a47de0 ("fixed the brand panel tagline being hardcoded
    /// to login copy on every auth page"); this locks that fix in place.
    #[tokio::test]
    async fn brand_panel_tagline_is_signup_appropriate() {
        let ctx = TestContext::new().await;
        let msg = Message::new("http.request");
        let html = output_html(handle(&ctx, &msg).await).await;

        assert!(
            html.contains("Create your account."),
            "signup brand panel must use signup-appropriate copy: {html}"
        );
        assert!(
            !html.contains("Sign in to continue."),
            "signup page must not carry over the login page's brand-panel tagline: {html}"
        );
    }
}
