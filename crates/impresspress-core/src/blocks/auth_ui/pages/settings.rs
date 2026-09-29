//! GET / POST /b/auth/admin/settings — the auth admin settings page, rendered
//! through the shared `ui::settings_form` (ConfigVar-driven; no tuple table).

use maud::html;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::{auth::config as auth_config, crud},
    config_vars,
    config_vars::{ALLOW_SIGNUP_KEY, ENABLE_OAUTH_KEY, POST_LOGIN_REDIRECT_KEY},
    ui::{
        self, components, icons,
        settings_form::{self, SettingsSection},
    },
};

/// The config vars rendered on the auth settings page, grouped into the three
/// on-page sections. Each var is pulled from its declared [`ConfigVar`] source
/// — shared vars from `config_vars::shared_var`, the auth-identity vars from
/// `auth::config::auth_identity_config_vars`, and the OAuth provider creds from
/// the auth-ui block's own `config_vars()` — so nothing is re-declared here.
struct Sections {
    registration: Vec<wafer_run::ConfigVar>,
    admin: Vec<wafer_run::ConfigVar>,
    oauth: Vec<wafer_run::ConfigVar>,
}

fn sections() -> Sections {
    let identity = auth_config::auth_identity_config_vars();
    let oauth_creds = super::super::config_vars();

    let mut oauth = vec![config_vars::shared_var(ENABLE_OAUTH_KEY)];
    oauth.extend(oauth_creds);

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
    /// Flatten to a single save allowlist.
    fn all(&self) -> Vec<wafer_run::ConfigVar> {
        let mut v = self.registration.clone();
        v.extend(self.admin.iter().cloned());
        v.extend(self.oauth.iter().cloned());
        v
    }
}

pub async fn handle_get(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let s = sections();
    let form_sections = [
        SettingsSection::new("Registration", icons::users(), &s.registration),
        SettingsSection::new("Admin", icons::shield(), &s.admin),
        SettingsSection::new("OAuth Providers", icons::globe(), &s.oauth),
    ];
    let form =
        match settings_form::settings_form(ctx, "/b/auth/admin/settings", &form_sections, html! {})
            .await
        {
            Ok(form) => form,
            Err(e) => {
                return crud::db_error_page(msg, e, "auth settings: current values read failed")
            }
        };
    let content = html! {
        (components::page_header("Authentication Settings", Some("Configure registration, OAuth providers, and security"), None))
        (form)
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Auth Settings", ui::NavKind::Admin, "Auth Settings"),
        content,
    )
    .await
}

pub async fn handle_post(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    settings_form::save_settings(ctx, msg, input, &sections().all(), "auth-ui").await
}
