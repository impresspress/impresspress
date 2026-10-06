//! Settings › Branding — the app name, logos, favicon and accent colour.
//!
//! Every var on it is a `WAFER_RUN_SHARED__*` key, which only the admin
//! block may write, so it is an admin page saving through the admin block.
//! `/b/userportal/admin/settings` redirects here.

use maud::{html, Markup};
use wafer_run::{context::Context, ConfigVar, InputStream, Message, OutputStream, WaferError};

use crate::{
    config_vars::{
        self, APP_NAME_KEY, AUTH_LOGO_URL_KEY, FAVICON_URL_KEY, LOGO_ICON_URL_KEY, LOGO_URL_KEY,
        PRIMARY_COLOR_KEY,
    },
    ui::settings_form::{self, SettingsSection},
};

/// Where the page's form posts — the page's own path.
pub(crate) const BRANDING_PATH: &str = "/b/admin/settings/branding";

/// The branding vars, from their central `config_vars::shared_var`
/// declarations.
fn branding_vars() -> Vec<ConfigVar> {
    [
        APP_NAME_KEY,
        LOGO_URL_KEY,
        LOGO_ICON_URL_KEY,
        AUTH_LOGO_URL_KEY,
        FAVICON_URL_KEY,
        PRIMARY_COLOR_KEY,
    ]
    .into_iter()
    .map(config_vars::shared_var)
    .collect()
}

/// The tab body: the settings form. `Err` when the current values could not
/// be read.
pub async fn settings_body(ctx: &dyn Context, _msg: &Message) -> Result<Markup, WaferError> {
    let vars = branding_vars();
    let sections = [SettingsSection::new("Branding", &vars)];
    settings_form::settings_form(ctx, BRANDING_PATH, &sections, html! {}).await
}

/// `POST /b/admin/settings/branding`.
pub async fn handle_save_branding_settings(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    settings_form::save_settings(ctx, msg, input, &branding_vars(), "branding").await
}

#[cfg(test)]
mod tests {
    use wafer_core::clients::config;

    use super::*;
    use crate::test_support::{admin_msg, output_json, TestContext};

    /// A deployment the router can reach the admin block in.
    async fn site() -> TestContext {
        let mut ctx = TestContext::with_admin().await;
        ctx.register_block(
            crate::blocks::admin::ADMIN_BLOCK_ID,
            std::sync::Arc::new(crate::blocks::admin::AdminBlock::new()),
        );
        ctx
    }

    /// B6: saving branding writes the shared keys, routed to the admin block
    /// with WRAP enforced in its frame.
    #[tokio::test]
    async fn saving_writes_the_shared_branding_keys() {
        let ctx = site().await;
        let body = serde_json::json!({
            APP_NAME_KEY: "Acme",
            PRIMARY_COLOR_KEY: "#123456",
        });
        let out = ctx
            .dispatch_resolved_json(admin_msg("create", BRANDING_PATH), &body)
            .await;
        assert_eq!(output_json(out).await["message"], "Settings saved");
        let fixture = ctx.fixture();
        assert_eq!(
            config::get_default(&fixture, APP_NAME_KEY, "")
                .await
                .unwrap(),
            "Acme"
        );
        assert_eq!(
            config::get_default(&fixture, PRIMARY_COLOR_KEY, "")
                .await
                .unwrap(),
            "#123456"
        );
    }

    /// The form's Save posts every field. When the stored branding cannot
    /// be read the page is the error page in the shell, never the form
    /// filled from the boot map and the declared defaults.
    #[tokio::test]
    async fn a_failed_branding_read_renders_no_form() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();
        let parts = crate::blocks::admin::test_support::browser_request(
            &ctx,
            admin_msg("retrieve", BRANDING_PATH),
        )
        .await;
        let html = String::from_utf8(parts.body).expect("utf-8");
        assert_eq!(parts.status, 500, "{html}");
        assert!(!html.contains("settings-form"), "{html}");
    }

    /// Control: a healthy read renders the form with the stored value.
    #[tokio::test]
    async fn the_branding_form_shows_the_stored_app_name() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let app_name = crate::test_support::unique_config_value();
        config::set(&ctx.fixture(), APP_NAME_KEY, &app_name)
            .await
            .expect("store the app name");
        let parts = crate::blocks::admin::test_support::browser_request(
            &ctx,
            admin_msg("retrieve", BRANDING_PATH),
        )
        .await;
        let html = String::from_utf8(parts.body).expect("utf-8");
        assert_eq!(parts.status, 200, "{html}");
        assert!(html.contains(&format!(r#"value="{app_name}""#)), "{html}");
    }
}
