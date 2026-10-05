//! Settings › Authentication — registration, the bootstrap admin and OAuth.
//!
//! Admin-owned, like every settings page that shows a `WAFER_RUN_SHARED__*`
//! key: WRAP lets only the admin block write those
//! ("WAFER_RUN_SHARED__* = shared app config — any block reads, admin
//! writes"). A save run in another block's frame (auth-ui's) is refused
//! for the first shared key it writes; `/b/auth/admin/settings` redirects
//! here.
//!
//! The page also shows block-scoped keys — auth's
//! `WAFER_RUN__AUTH__REQUIRE_VERIFICATION` / `ALLOWED_EMAIL_DOMAINS` and
//! auth-ui's `IMPRESSPRESS__AUTH_UI__OAUTH_*` provider credentials. They stay
//! the owning blocks' keys: each is the `ConfigVar` that block declares (read
//! here from its declaration, never re-declared), under its own prefix, and
//! read only by its block. The ONE form saves them all in ONE request through
//! the admin block, which WRAP lets write any block's resources (the rule the
//! Variables and Email pages already rely on). Splitting the form across two
//! writers would make a save non-atomic: `save_settings` validates the whole
//! body before its first write so a refusal never leaves half a page saved,
//! and two endpoints cannot share that pre-pass.

use maud::{html, Markup};
use wafer_run::{context::Context, ConfigVar, InputStream, Message, OutputStream, WaferError};

use crate::{
    blocks::{auth::config as auth_config, auth_ui},
    config_vars::{self, ALLOW_SIGNUP_KEY, ENABLE_OAUTH_KEY, POST_LOGIN_REDIRECT_KEY},
    ui::settings_form::{self, SettingsSection},
};

/// Where the page's form posts — the page's own path.
pub(crate) const AUTHENTICATION_PATH: &str = "/b/admin/settings/authentication";

/// The config vars on the page, grouped into its three sections. Each is
/// pulled from its declaration — shared vars from
/// `config_vars::shared_var`, the auth-identity vars from
/// `auth::config::auth_identity_config_vars`, the OAuth provider creds from
/// the auth-ui block's own `config_vars()`.
struct Sections {
    registration: Vec<ConfigVar>,
    admin: Vec<ConfigVar>,
    oauth: Vec<ConfigVar>,
}

fn sections() -> Sections {
    let identity = auth_config::auth_identity_config_vars();
    let mut oauth = vec![config_vars::shared_var(ENABLE_OAUTH_KEY)];
    oauth.extend(auth_ui::config_vars());
    Sections {
        registration: vec![
            config_vars::shared_var(ALLOW_SIGNUP_KEY),
            config_vars::var_in(&identity, auth_config::REQUIRE_VERIFICATION_KEY),
            config_vars::var_in(&identity, auth_config::ALLOWED_EMAIL_DOMAINS_KEY),
            config_vars::shared_var(POST_LOGIN_REDIRECT_KEY),
        ],
        admin: vec![
            config_vars::shared_var(auth_config::BOOTSTRAP_ADMIN_EMAIL_KEY),
            config_vars::shared_var(auth_config::BOOTSTRAP_ADMIN_PASSWORD_KEY),
        ],
        oauth,
    }
}

impl Sections {
    /// Every var on the page: the save's allowlist.
    fn all(&self) -> Vec<ConfigVar> {
        let mut v = self.registration.clone();
        v.extend(self.admin.iter().cloned());
        v.extend(self.oauth.iter().cloned());
        v
    }
}

/// The tab body: the settings form. `Err` when the current values could not
/// be read.
pub async fn settings_body(ctx: &dyn Context, _msg: &Message) -> Result<Markup, WaferError> {
    let s = sections();
    let form_sections = [
        SettingsSection::new("Registration", &s.registration),
        SettingsSection::new("Admin", &s.admin),
        // Seven provider fields mean nothing while OAuth is off, so they
        // stay out of the way until it is switched on.
        SettingsSection::new("OAuth providers", &s.oauth).gated_by(ENABLE_OAUTH_KEY),
    ];
    settings_form::settings_form(ctx, AUTHENTICATION_PATH, &form_sections, html! {}).await
}

/// `POST /b/admin/settings/authentication`.
pub async fn handle_save_authentication_settings(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    settings_form::save_settings(ctx, msg, input, &sections().all(), "authentication").await
}

#[cfg(test)]
mod tests {
    use wafer_core::clients::config;

    use super::*;
    use crate::{
        blocks::auth_ui::{OAUTH_GITHUB_CLIENT_SECRET_KEY, OAUTH_REDIRECT_URI_KEY},
        test_support::{admin_msg, output_html, output_json, TestContext},
    };

    /// A deployment the router can reach the admin block in (and the auth-ui block, for its old URL).
    async fn site() -> TestContext {
        let mut ctx = TestContext::with_auth().await;
        ctx.register_block(
            crate::blocks::admin::ADMIN_BLOCK_ID,
            std::sync::Arc::new(crate::blocks::admin::AdminBlock::new()),
        );
        ctx.register_block(
            crate::blocks::auth_ui::AUTH_UI_BLOCK_ID,
            std::sync::Arc::new(crate::blocks::auth_ui::AuthUiBlock::new()),
        );
        ctx
    }

    /// The provider credentials sit in the region the Enable OAuth switch
    /// controls (hidden by CSS while it is off).
    #[tokio::test]
    async fn oauth_credentials_are_gated_by_the_enable_oauth_switch() {
        let ctx = site().await;
        let html = output_html(
            ctx.dispatch_resolved(admin_msg("retrieve", AUTHENTICATION_PATH))
                .await,
        )
        .await;
        let region =
            format!(r#"<div class="settings-section__gated" id="{ENABLE_OAUTH_KEY}-section">"#);
        let toggle = html
            .find(&format!(
                r#"id="{ENABLE_OAUTH_KEY}" type="checkbox" role="switch""#
            ))
            .expect("Enable OAuth renders as a switch");
        let region_at = html.find(&region).expect("gated region");
        let secret_at = html
            .find(&format!(r#"id="{OAUTH_GITHUB_CLIENT_SECRET_KEY}""#))
            .expect("provider field");
        assert!(toggle < region_at && region_at < secret_at, "{html}");
        assert!(html.contains(&format!(r#"aria-controls="{ENABLE_OAUTH_KEY}-section""#)));
        assert!(
            html.contains(&format!(r#"fetch("{AUTHENTICATION_PATH}""#)),
            "posts to itself: {html}"
        );
    }

    /// B6: the page's save writes its shared keys AND the auth / auth-ui
    /// block keys, in one request routed to the admin block the way the
    /// browser's is — WRAP enforced in that block's frame, which refuses a
    /// shared write from any other block ("only admin can write
    /// WAFER_RUN_SHARED__ resources").
    #[tokio::test]
    async fn saving_writes_shared_and_block_scoped_keys_through_the_admin_block() {
        let ctx = site().await;
        let body = serde_json::json!({
            ALLOW_SIGNUP_KEY: "false",
            POST_LOGIN_REDIRECT_KEY: "/b/userportal/profile",
            auth_config::ALLOWED_EMAIL_DOMAINS_KEY: "example.com",
            ENABLE_OAUTH_KEY: "true",
            OAUTH_REDIRECT_URI_KEY: "https://app.example.com/b/auth/oauth/callback",
        });
        let out = ctx
            .dispatch_resolved_json(admin_msg("create", AUTHENTICATION_PATH), &body)
            .await;
        assert_eq!(output_json(out).await["message"], "Settings saved");

        let fixture = ctx.fixture();
        for (key, want) in [
            (ALLOW_SIGNUP_KEY, "false"),
            (POST_LOGIN_REDIRECT_KEY, "/b/userportal/profile"),
            (auth_config::ALLOWED_EMAIL_DOMAINS_KEY, "example.com"),
            (ENABLE_OAUTH_KEY, "true"),
            (
                OAUTH_REDIRECT_URI_KEY,
                "https://app.example.com/b/auth/oauth/callback",
            ),
        ] {
            let stored = config::get_default(&fixture, key, "")
                .await
                .expect("config read");
            assert_eq!(stored, want, "{key}");
        }
    }

    /// The form reads every value through the config service, which WRAP
    /// guards like the database: a frame refused those reads gets the 403
    /// page drawn in the shell, not a 500 and not a form of defaults.
    #[tokio::test]
    async fn a_config_denial_is_the_403_page_not_a_form() {
        let ctx = TestContext::with_auth().await.running_as("test/ungranted");
        let mut msg = admin_msg("retrieve", AUTHENTICATION_PATH);
        msg.set_meta("http.header.accept", "text/html");
        let parts = wafer_block::http_codec::collect_http_response(
            super::super::settings_page(&ctx, &msg, "authentication").await,
        )
        .await;
        let html = String::from_utf8_lossy(&parts.body);
        assert_eq!(parts.status, 403, "{html}");
        assert!(!html.contains("settings-form"), "{html}");
    }

    /// The old auth-ui URL is a redirect to this page; nothing there saves.
    #[tokio::test]
    async fn the_old_auth_ui_settings_url_redirects_here() {
        let ctx = site().await;
        let mut msg = admin_msg("retrieve", "/b/auth/admin/settings");
        msg.set_meta("http.header.accept", "text/html");
        let parts =
            wafer_block::http_codec::collect_http_response(ctx.dispatch_resolved(msg).await).await;
        assert_eq!(parts.status, 308);
        let location = parts
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("location"))
            .map(|(_, v)| v.as_str());
        assert_eq!(location, Some(AUTHENTICATION_PATH));
    }
}
