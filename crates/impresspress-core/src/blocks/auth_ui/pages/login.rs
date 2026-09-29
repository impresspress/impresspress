//! GET /b/auth/login — relocated from auth/pages/mod.rs::login_page in Task 5.

use maud::{html, PreEscaped};
use wafer_run::{context::Context, Message, OutputStream};

use super::{
    login_script, oauth_button_script, oauth_provider_configured, oauth_provider_icon,
    oauth_provider_label, pw_field, site_config,
};
use crate::{
    blocks::auth_ui::redirect::is_safe_local_redirect,
    config_vars::{ALLOW_SIGNUP_KEY, ENABLE_OAUTH_KEY},
    ui::{
        self,
        components::{alert, auth_panel, oauth_button, AlertVariant},
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
    let raw_redirect = msg.get_meta("req.query.redirect").to_string();
    // Validate redirect — only allow relative paths (prevent open redirect)
    let redirect = if is_safe_local_redirect(&raw_redirect) {
        raw_redirect
    } else {
        String::new()
    };
    // Signup UX (Fix 2): the signup page can send a brand-new user here with
    // `?email=...` after redirecting them for email verification, so they
    // don't have to retype it. Rendered as an attribute value — maud
    // HTML-escapes it, and an over-long value is simply dropped rather than
    // rendered.
    let raw_email = msg.get_meta("req.query.email").to_string();
    let prefill_email = if raw_email.len() <= 255 {
        raw_email
    } else {
        String::new()
    };
    let signup_redirect = if redirect.is_empty() {
        String::new()
    } else {
        format!("?redirect={redirect}")
    };

    // OAuth buttons appear only when ENABLE_OAUTH is on AND the provider's
    // full credential triple (CLIENT_ID + CLIENT_SECRET + REDIRECT_URL) is
    // present in env. Avoids rendering a "Continue with GitHub" button that
    // would 4xx as soon as it's clicked.
    let oauth_enabled = match crate::config_vars::get_bool(ctx, ENABLE_OAUTH_KEY, false).await {
        Ok(enabled) => enabled,
        Err(e) => {
            return crate::blocks::crud::db_error_page(msg, e, "Could not read the OAuth switch")
        }
    };
    // A loop rather than `.filter()`: the predicate reads config through the
    // async client now, and an async predicate has no place in `Iterator`.
    let mut oauth_providers: Vec<&'static str> = Vec::new();
    if oauth_enabled {
        for provider in ["github", "google", "microsoft"] {
            match oauth_provider_configured(ctx, provider).await {
                Ok(true) => oauth_providers.push(provider),
                Ok(false) => {}
                Err(e) => {
                    return crate::blocks::crud::db_error_page(
                        msg,
                        e,
                        "Could not read the OAuth provider config",
                    )
                }
            }
        }
    }

    let markup = ui::layout::page(
        "Sign In",
        &config,
        auth_split(
            // `None`: login is the one auth page that shows the brand
            // tagline (`config.auth_tagline`, via `auth_panel`'s fallback)
            // rather than a page-specific line — it reads as the product's
            // pitch, not a duplicate of the right-column heading pair below
            // (`.auth-form__title` / `.auth-form__subtitle`, "Welcome back"
            // / "Sign in to continue."), which is a different sentence.
            auth_panel(&config, None),
            html! {
                div .auth-form {
                    h2 .auth-form__title { "Welcome back" }
                    p .auth-form__subtitle { "Sign in to continue." }

                    (alert(AlertVariant::Error, "error", ""))
                    (alert(AlertVariant::Success, "info", ""))

                    @if !oauth_providers.is_empty() {
                        div .oauth-buttons {
                            @for provider in &oauth_providers {
                                (oauth_button(provider, oauth_provider_label(provider), oauth_provider_icon(provider)))
                            }
                        }
                        div .auth-divider { "or" }
                    }

                    form #form .login-form {
                        input type="hidden" #redirect value=(redirect);

                        div .form-group {
                            label .form-label for="email" { "Email" }
                            input .form-input type="email" #email placeholder="you@example.com" value=(prefill_email) required;
                        }

                        div .form-group {
                            label .form-label for="password" { "Password" }
                            (pw_field("password", "Enter your password", None))
                        }

                        div .auth-actions {
                            button type="button" class="btn btn--ghost btn--sm" data-action="auth-forgot" {
                                "Forgot password?"
                            }
                        }

                        button .login-button type="submit" #btn { "Sign In" }
                    }

                    @if allow_signup {
                        div .signup-link {
                            "Don't have an account? "
                            a href={"/b/auth/signup" (signup_redirect)} { "Sign up" }
                        }
                    }
                }

                script { (PreEscaped(login_script())) }
                @if !oauth_providers.is_empty() {
                    script { (PreEscaped(oauth_button_script())) }
                }
            },
        ),
    );

    ui::html_response(markup)
}

#[cfg(test)]
mod tests {
    use wafer_run::Message;

    use super::handle;
    use crate::{
        blocks::auth_ui::{
            OAUTH_GITHUB_CLIENT_ID_KEY, OAUTH_GITHUB_CLIENT_SECRET_KEY, OAUTH_REDIRECT_URI_KEY,
        },
        config_vars::{ALLOW_SIGNUP_KEY, ENABLE_OAUTH_KEY},
        test_support::{output_html, TestContext},
    };

    /// The page and the API must answer "is signup allowed?" the same way.
    ///
    /// `WAFER_RUN_SHARED__ALLOW_SIGNUP=1` opened the signup API
    /// (`auth::helpers::signup_allowed` accepted `"true"` or `"1"`) while
    /// this page compared against `"true"` alone and hid the link — so the
    /// only route to a form that works was to already know the URL.
    #[tokio::test]
    async fn signup_link_and_signup_api_read_the_same_truth_table() {
        for enabled in ["1", "true", "YES", " on "] {
            let mut ctx = TestContext::new()
                .await
                .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
            ctx.set_config(ALLOW_SIGNUP_KEY, enabled);
            let html = output_html(handle(&ctx, &login_msg(&[])).await).await;
            assert!(
                crate::blocks::auth::helpers::signup_allowed(&ctx)
                    .await
                    .expect("config read"),
                "the signup API must accept {enabled:?}"
            );
            assert!(
                html.contains("/b/auth/signup"),
                "the signup link must be rendered for {enabled:?}: {html}"
            );
        }
        for disabled in ["0", "false", "", "bogus"] {
            let mut ctx = TestContext::new()
                .await
                .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
            ctx.set_config(ALLOW_SIGNUP_KEY, disabled);
            let html = output_html(handle(&ctx, &login_msg(&[])).await).await;
            assert!(
                !crate::blocks::auth::helpers::signup_allowed(&ctx)
                    .await
                    .expect("config read"),
                "the signup API must refuse {disabled:?}"
            );
            assert!(
                !html.contains("/b/auth/signup"),
                "the signup link must be hidden for {disabled:?}: {html}"
            );
        }
    }

    /// The same divergence on the OAuth flag: `=1` let `oauth/start.rs`
    /// begin the flow while this page drew no button.
    #[tokio::test]
    async fn oauth_buttons_follow_the_same_truth_table_as_the_oauth_start_handler() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        ctx.set_config(ENABLE_OAUTH_KEY, "1");
        ctx.set_config(OAUTH_GITHUB_CLIENT_ID_KEY, "gh_id");
        ctx.set_config(OAUTH_GITHUB_CLIENT_SECRET_KEY, "gh_secret");
        ctx.set_config(OAUTH_REDIRECT_URI_KEY, "https://app/cb");
        let html = output_html(handle(&ctx, &login_msg(&[])).await).await;
        assert!(
            html.contains(r#"data-provider="github""#),
            "ENABLE_OAUTH=1 must render the provider button the start handler accepts: {html}"
        );
    }

    fn login_msg(query: &[(&str, &str)]) -> Message {
        let mut msg = Message::new("http.request");
        for (k, v) in query {
            msg.set_meta(format!("req.query.{k}"), *v);
        }
        msg
    }

    // Default branding on the sign-in card: the built-in pixel-art icon at
    // 64px — the 64-cell rendition at 1:1, served as a plain `<img>` (no
    // `srcset`, nothing for the browser to second-guess) — above the app
    // name as text. No raster wordmark.
    //
    // Plain comments, not doc comments: the test they described was dropped,
    // so a `///` block here is an outer attribute with no item to document.
    // Ported from origin/main's `default_branding_is_pixel_art_icon_plus_app_
    // name` during the main merge -- 2026-09-02 -- and dropped rather than
    // adapted: main's test asserted the login page's *form* panel renders
    // `.login-brand__icon`/`.login-app-name` (main's `brand_lockup`, re-added
    // to `.login-container`). This branch deliberately retired that per-page
    // logo header (see `styles/layouts/auth-split.css`'s comment) -- the
    // navy `.auth-split__brand` aside is the only place branding renders on
    // this page now, already covered below by
    // `brand_panel_shows_default_headline_and_tagline_without_duplicating_
    // subtitle` (see the coordinator's ruling in the merge report).

    /// Explicit, safe `?redirect=` params must still be honored — rendered
    /// into the hidden `#redirect` field the login script reads first,
    /// ahead of the role-aware `default_redirect` the JSON API returns.
    #[tokio::test]
    async fn renders_safe_redirect_into_hidden_field() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let msg = login_msg(&[("redirect", "/b/userportal/profile")]);
        let html = output_html(handle(&ctx, &msg).await).await;
        assert!(
            html.contains(r#"id="redirect" type="hidden" value="/b/userportal/profile""#),
            "safe redirect must be rendered into the hidden field: {html}"
        );
    }

    /// Open-redirect protection is unchanged by this fix: an unsafe
    /// `?redirect=` (protocol-relative, foreign scheme, etc.) must still be
    /// dropped rather than rendered.
    #[tokio::test]
    async fn rejects_unsafe_redirect_renders_empty_hidden_field() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let msg = login_msg(&[("redirect", "//evil.com")]);
        let html = output_html(handle(&ctx, &msg).await).await;
        assert!(
            html.contains(r#"id="redirect" type="hidden" value="""#),
            "unsafe redirect must not be rendered: {html}"
        );
        assert!(!html.contains("evil.com"));
    }

    /// Signup UX (Fix 2): the signup page can send a brand-new user here
    /// with `?email=...` so they don't have to retype it.
    #[tokio::test]
    async fn prefills_email_from_query_param() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let msg = login_msg(&[("email", "alice@example.com")]);
        let html = output_html(handle(&ctx, &msg).await).await;
        assert!(
            html.contains(r#"value="alice@example.com""#),
            "email query param must prefill the email input: {html}"
        );
    }

    /// Defensive cap — an absurd `?email=` value is dropped rather than
    /// rendered (mirrors the 255-char cap `api/signup.rs` enforces on input).
    #[tokio::test]
    async fn ignores_overlong_email_query_param() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let long_email = format!("{}@example.com", "a".repeat(300));
        let msg = login_msg(&[("email", &long_email)]);
        let html = output_html(handle(&ctx, &msg).await).await;
        assert!(!html.contains(&long_email));
    }

    /// A11y: the visible "Email"/"Password" labels must be programmatically
    /// associated with their inputs via `<label for>` + matching `id`, not
    /// bare `<div>`s a screen reader can't tie to the field.
    #[tokio::test]
    async fn email_and_password_labels_are_associated_with_their_inputs() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let msg = login_msg(&[]);
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

    /// The navy brand panel shows the configurable headline and the brand
    /// tagline (falls back from `auth_panel(&config, None)`) exactly once
    /// each, and the right column's "Sign in to continue." is not
    /// duplicated by the panel.
    #[tokio::test]
    async fn brand_panel_shows_default_headline_and_tagline_without_duplicating_subtitle() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let msg = login_msg(&[]);
        let html = output_html(handle(&ctx, &msg).await).await;

        assert_eq!(
            html.matches("The backend that lifts its own weight.")
                .count(),
            1,
            "headline must appear exactly once: {html}"
        );
        assert_eq!(
            html.matches("One binary. Batteries included. No lock-in.")
                .count(),
            1,
            "brand tagline must appear exactly once: {html}"
        );
        assert_eq!(
            html.matches("Sign in to continue.").count(),
            1,
            "right-column subtitle must not be duplicated by the brand panel: {html}"
        );
    }

    /// A11y: the icon-only password-reveal toggle must have an accessible
    /// name (aria-label), since it renders no visible text.
    #[tokio::test]
    async fn password_toggle_button_has_non_empty_aria_label() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let msg = login_msg(&[]);
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
}
