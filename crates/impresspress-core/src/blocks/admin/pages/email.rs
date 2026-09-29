use maud::Markup;
use wafer_run::{context::Context, ConfigVar, InputStream, Message, OutputStream, WaferError};

use crate::{
    blocks::{
        email,
        email::{
            MAILGUN_API_KEY, MAILGUN_BASE_URL, MAILGUN_DOMAIN, MAILGUN_FROM, MAILGUN_REPLY_TO,
        },
    },
    config_vars,
    ui::{
        icons,
        settings_form::{self, SettingsSection},
    },
};

/// The Mailgun settings surfaced on the admin Email settings tab — a named
/// subset of `email::config_vars()` (the block's canonical declarations; the
/// block also declares rate-limit + allowed-recipient vars that aren't
/// editable from this page). Selected by key via `config_vars::var_in` so
/// this page never re-declares label/default/input_type/sensitivity in a
/// parallel table — the `ConfigVar` in `blocks/email.rs` is the single
/// source of truth, shared with `BlockInfo::config_keys` and the admin
/// Variables page.
const MAILGUN_KEYS: &[&str] = &[
    MAILGUN_API_KEY,
    MAILGUN_DOMAIN,
    MAILGUN_FROM,
    MAILGUN_REPLY_TO,
    MAILGUN_BASE_URL,
];

fn mailgun_vars() -> Vec<ConfigVar> {
    let own = email::config_vars();
    MAILGUN_KEYS
        .iter()
        .map(|key| config_vars::var_in(&own, key))
        .collect()
}

/// Render the email settings tab body. The parent `settings_page` handler
/// wraps this in the form-LESS `tabbed_page` shell, so this tab owns its
/// `<form>` outright: the full self-contained `settings_form` (its own
/// `<form id="settings-form">` + "Save Settings" button), posting JSON via
/// fetch to `POST /b/admin/email` — the `SaveEmailSettings` route that
/// [`handle_save_email_settings`] serves. Same pattern as every other
/// block's admin settings page (products / userportal / legalpages /
/// auth_ui). `Err` when the current values could not be read.
pub async fn settings_body(ctx: &dyn Context, _msg: &Message) -> Result<Markup, WaferError> {
    let vars = mailgun_vars();
    let section = SettingsSection::new("Mailgun Configuration", icons::globe(), &vars);
    settings_form::settings_form(ctx, "/b/admin/email", &[section], maud::html! {}).await
}

pub async fn handle_save_email_settings(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    settings_form::save_settings(ctx, msg, input, &mailgun_vars(), "email").await
}

#[cfg(test)]
mod tests {
    use wafer_core::clients::config;
    use wafer_run::{streams::output::TerminalNotResponse, InputStream};

    use super::*;
    use crate::{
        blocks::{
            admin::{test_support::routed, AdminBlock},
            email::{MAILGUN_API_KEY, MAILGUN_DOMAIN},
        },
        test_support::{admin_msg, anon_msg, audit_rows, output_json, TestContext},
        util::RecordExt,
    };

    fn email_body() -> serde_json::Value {
        serde_json::json!({
            MAILGUN_API_KEY: "key-123",
            MAILGUN_DOMAIN: "mg.example.com",
        })
    }

    #[tokio::test]
    async fn save_email_settings_reports_failure_when_config_set_fails() {
        // Every `config::set` fails (mirrors
        // `save_settings_surfaces_config_set_failure` in `ui/settings_form.rs`).
        // A save loop that swallowed the error via `let _ = config::set(...)`
        // would return success anyway; the shared `settings_form::save_settings`
        // helper this delegates to must not.
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.refuse_config_writes();
        let msg = anon_msg("create", "/b/admin/email");
        let input = InputStream::from_bytes(serde_json::to_vec(&email_body()).unwrap());

        let out = handle_save_email_settings(&ctx, &msg, input).await;

        assert!(
            matches!(
                out.collect_buffered().await,
                Err(TerminalNotResponse::Error(_))
            ),
            "a failed config::set must surface as an error, not a success toast"
        );
    }

    #[tokio::test]
    async fn save_email_settings_reports_success_when_all_writes_succeed() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        // Registers a real `wafer-run/config` service block so `config::set`
        // succeeds (see `TestContext::set_config`).
        ctx.set_config(MAILGUN_API_KEY, "");
        let msg = anon_msg("create", "/b/admin/email");
        let input = InputStream::from_bytes(serde_json::to_vec(&email_body()).unwrap());

        let out = handle_save_email_settings(&ctx, &msg, input).await;
        let body = output_json(out).await;

        // Generic message from the shared `settings_form::save_settings` —
        // every block using the shared helper reports the same wording (see
        // `ui/settings_form.rs`'s own `save_settings_*` tests); no block-name
        // string to preserve here since the old hand-rolled handler's
        // "Email settings saved" was itself just a hardcoded literal, not
        // something the page JS branches on (it only checks `d.error`).
        assert_eq!(body["message"], "Settings saved");

        // The value was actually persisted, not just reported as saved.
        let stored = config::get_default(&ctx, MAILGUN_API_KEY, "")
            .await
            .expect("config read");
        assert_eq!(stored, "key-123");
    }

    /// Editing these same keys on the admin Variables page writes a
    /// `variable.update` row (`ops::update_variable`). Saving them here wrote
    /// none, so whether an admin's change was recorded depended on which page
    /// they used. Driven through the block's own route table, so the handler
    /// gets the `Message` the router hands it.
    #[tokio::test]
    async fn saving_the_email_settings_page_audits_the_keys_it_wrote() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);

        let out = wafer_run::Block::handle(
            &AdminBlock::new(),
            &ctx,
            routed(admin_msg("create", "/b/admin/email")),
            InputStream::from_bytes(serde_json::to_vec(&email_body()).unwrap()),
        )
        .await;
        assert_eq!(output_json(out).await["message"], "Settings saved");

        let rows = audit_rows(&ctx, "settings.update").await;
        assert_eq!(rows.len(), 1, "one row per save");
        assert_eq!(rows[0].str_field("user_id"), "admin_1");
        assert_eq!(
            rows[0].str_field("resource"),
            "settings/email (IMPRESSPRESS__EMAIL__MAILGUN_API_KEY, \
             IMPRESSPRESS__EMAIL__MAILGUN_DOMAIN)",
            "the row names the page and the keys this save actually wrote"
        );
    }

    #[tokio::test]
    async fn settings_body_masks_a_stored_mailgun_api_key() {
        // SEC-060: the Mailgun API key is a `ConfigVar` with
        // `InputType::Password` (declared once in `blocks::email::config_vars`
        // and picked up here via `config_vars::var_in`) — `render_sections`
        // must render it exactly like every other shared-helper password
        // field: `type="password"` (visually masked, not plaintext), the eye
        // toggle to reveal/edit it, and the "(set)" placeholder rather than
        // "Not configured" once a value exists. Masking is now
        // source-redaction, not just visual: the raw value must never appear
        // in the rendered HTML at all (previously it sat in the `value=`
        // attribute in plaintext, readable via page source / devtools — see
        // `settings_form.rs`'s own `password_field_is_masked_with_eye_toggle_
        // and_never_echoes_the_raw_value` test for the same contract).
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config(MAILGUN_API_KEY, "super-secret-value");
        let msg = anon_msg("retrieve", "/b/admin/settings/email");

        let html = settings_body(&ctx, &msg)
            .await
            .expect("the current values are readable")
            .into_string();

        assert!(
            !html.contains("super-secret-value"),
            "the raw API key must never reach the rendered HTML: {html}"
        );
        assert!(
            html.contains(r#"type="password""#),
            "the API key field must render as a masked password input: {html}"
        );
        assert!(
            html.contains(r#"aria-label="Reveal value""#)
                && html.contains(r#"data-action="reveal-toggle""#)
                && html.contains(r#"data-reveal-hide="Hide""#),
            "the reveal/edit eye toggle must be present, with an accessible name: {html}"
        );
        assert!(
            html.contains("(set)"),
            "a configured secret shows the '(set)' placeholder: {html}"
        );
        assert!(
            !html.contains("Not configured"),
            "a configured secret must not show the empty-state placeholder: {html}"
        );
    }

    #[tokio::test]
    async fn settings_body_renders_mailgun_base_url_default_placeholder() {
        // The base-URL field keeps its documented default-as-placeholder
        // behavior (was `field.default` in the old hand-rolled render; now
        // sourced from the same `ConfigVar.default` the block declares).
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let msg = anon_msg("retrieve", "/b/admin/settings/email");

        let html = settings_body(&ctx, &msg)
            .await
            .expect("the current values are readable")
            .into_string();

        assert!(
            html.contains(email::DEFAULT_MAILGUN_BASE_URL),
            "unset base URL should show the default as a placeholder: {html}"
        );
    }
}
