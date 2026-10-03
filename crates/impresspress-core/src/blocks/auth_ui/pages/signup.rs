//! GET /b/auth/signup — relocated from auth/pages/mod.rs::signup_page in Task 5.

use maud::{html, PreEscaped};
use wafer_run::{context::Context, Message, OutputStream};

use super::{api_post_script, pw_field, signup_script, site_config, PasswordPurpose};
use crate::{
    blocks::auth_ui::redirect::is_safe_local_redirect,
    config_vars::ALLOW_SIGNUP_KEY,
    ui::{
        self,
        components::{alert, auth_panel, AlertVariant},
        templates::auth_split,
    },
};

pub async fn handle(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let config = match site_config(ctx).await {
        Ok(site) => site,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "page: site config read failed")
        }
    };
    let allow_signup = match crate::config_vars::get_bool(ctx, ALLOW_SIGNUP_KEY, true).await {
        Ok(allowed) => allowed,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "Could not read the signup switch")
        }
    };
    // The minimum the API enforces, so the field's `minlength` and the
    // placeholder state the same number (see `PasswordPurpose::New`).
    let min_length = match crate::blocks::auth::helpers::password_min_length(ctx).await {
        Ok(n) => n,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "page: password policy read failed")
        }
    };
    let raw_redirect = msg.get_meta("req.query.redirect").to_string();
    // Validate redirect — only allow relative paths (prevent open redirect)
    let redirect = if is_safe_local_redirect(&raw_redirect) {
        raw_redirect
    } else {
        String::new()
    };
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
            auth_panel(&config, Some("Create your account.")),
            html! {
                div .login-container {
                    (alert(AlertVariant::Error, "error", ""))

                    div #success .auth-status hidden {
                        div .auth-status__icon .auth-status__icon--success aria-hidden="true" { (ui::icons::check()) }
                        h2 .auth-status__title { "Check your email" }
                        p #verify-msg .auth-status__message {}
                        a #back-to-signin .login-button .auth-status__action href={"/b/auth/login" (redirect_qs)} {
                            "Back to Sign In"
                        }
                    }

                    form #form .login-form {
                        input type="hidden" #redirect value=(redirect);

                        div .form-group {
                            label .form-label for="email" { "Email" }
                            input .form-input type="email" #email placeholder="you@example.com" autocomplete="email" required;
                        }

                        div .form-group {
                            label .form-label for="password" { "Password" }
                            (pw_field("password", &format!("Min {min_length} characters"), PasswordPurpose::New { min_length }))
                        }

                        button .login-button type="submit" #btn { "Create Account" }
                    }

                    div #signin-link .signup-link {
                        "Already have an account? "
                        a href={"/b/auth/login" (redirect_qs)} { "Sign in" }
                    }
                }

                script { (PreEscaped(api_post_script())) }
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let msg = Message::new("http.request");
        let html = output_html(handle(&ctx, &msg).await).await;

        let marker = "class=\"reveal-toggle\"";
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
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

    /// A new password: managers offer to generate one, and the browser
    /// enforces the server's configured minimum (8 by default; a configured
    /// 12 is what the field then says), not a literal of the page's own.
    #[tokio::test]
    async fn the_password_is_a_new_one_with_the_configured_minimum() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let html = output_html(handle(&ctx, &Message::new("http.request")).await).await;
        assert!(html.contains(r#"autocomplete="new-password""#), "{html}");
        assert!(html.contains(r#"minlength="8""#), "{html}");
        assert!(html.contains(r#"role="alert""#), "{html}");

        ctx.set_config(crate::blocks::auth::config::PASSWORD_MIN_LENGTH_KEY, "12");
        let html = output_html(handle(&ctx, &Message::new("http.request")).await).await;
        assert!(html.contains(r#"minlength="12""#), "{html}");
        assert!(html.contains("Min 12 characters"), "{html}");
    }
}
