//! Shared, ConfigVar-driven admin settings form.
//!
//! Every block's admin settings page used to carry its own copy of: a
//! stringly-typed tuple table re-declaring `(key, label, help, default,
//! input_type)` that the block *already* declares as [`ConfigVar`] in its
//! `BlockInfo::config_keys` (or that `config_vars.rs` declares centrally for
//! `WAFER_RUN_SHARED__*`), a maud form loop with a special-cased color picker
//! and sensitive-field eye toggle, a copy-pasted inline-JS submit function,
//! and a POST handler that walked the same tuple table to `config::set` each
//! key. Five blocks, five drifting copies (verified live drifts: legalpages
//! `BG_COLOR` default, userportal `FAVICON_URL`/logo-URL input types).
//!
//! This module is the single renderer + the single save handler, driven
//! directly by [`ConfigVar`] metadata — the declared single source of truth.
//! The widget is derived from [`InputType`]: `Password` → masked field with an
//! eye toggle, `Color` → text input paired with a color picker, `Toggle` →
//! checkbox, `Url`/`Text` → plain text input. Each block's settings page becomes
//! "pick the ConfigVars to show, group them into sections, render".

use std::collections::HashMap;

use maud::{html, Markup, PreEscaped};
use wafer_core::clients::config;
use wafer_run::{context::Context, InputStream, OutputStream};
pub use wafer_run::{ConfigVar, InputType};

use crate::{
    http::{err_bad_request, err_internal, ok_json},
    util::{is_sensitive_key, validate_url_value, MASKED_VALUE},
};

/// One titled group of settings within a form (e.g. "Stripe", "OAuth Providers").
pub struct SettingsSection<'a> {
    /// Section heading text.
    pub title: &'a str,
    /// Section heading icon (a maud fragment, e.g. `icons::settings()`).
    pub icon: Markup,
    /// The config variables rendered in this section, in order.
    pub vars: &'a [ConfigVar],
    /// One-line explanation rendered under the heading; empty renders
    /// nothing.
    pub description: &'a str,
}

impl<'a> SettingsSection<'a> {
    /// Construct a section from a title, icon, and its variables.
    pub fn new(title: &'a str, icon: Markup, vars: &'a [ConfigVar]) -> Self {
        Self {
            title,
            icon,
            vars,
            description: "",
        }
    }

    /// Set the one-line explanation rendered under the section heading.
    pub fn description(mut self, description: &'a str) -> Self {
        self.description = description;
        self
    }
}

/// Load the current value for every var in `sections` via the config client.
async fn load_values(
    ctx: &dyn Context,
    sections: &[SettingsSection<'_>],
) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for section in sections {
        for var in section.vars {
            // `entry`-guard so a key declared in two sections is only fetched once.
            if !values.contains_key(&var.key) {
                let value = config::get_default(ctx, &var.key, &var.default).await;
                values.insert(var.key.clone(), value);
            }
        }
    }
    values
}

/// Render one field, deriving the widget from the var's [`InputType`].
fn render_field(var: &ConfigVar, value: &str) -> Markup {
    let label = if var.name.is_empty() {
        var.key.as_str()
    } else {
        var.name.as_str()
    };
    // SEC-060: a sensitive field's raw value must never reach the rendered
    // HTML — masking it only via `type="password"` still leaves the secret
    // readable in page source / devtools. `is_sensitive_key` is the single
    // source of truth shared with the admin Variables page
    // (`blocks::admin::ops::is_sensitive_key`, re-exported from
    // `crate::util`): sensitive when the var declares `InputType::Password`
    // *or* its key follows the `_SECRET`/`_KEY` suffix convention, so a var
    // left on the default `Text` widget by mistake still gets redacted.
    // `has_value` is captured from the real value (presence only, never its
    // content) so the placeholder can still distinguish "configured" from
    // "not configured" without ever exposing the secret itself.
    let is_sensitive = is_sensitive_key(&var.key, var.is_sensitive() as i64);
    let has_value = !value.is_empty();
    let value = if is_sensitive { "" } else { value };
    match var.input_type {
        InputType::Toggle => html! {
            div .form-group style="margin-bottom:1.25rem" {
                label style="display:flex;align-items:center;gap:0.75rem;cursor:pointer" {
                    input type="checkbox" name=(var.key)
                        checked[value == "true"]
                        style="width:1.25rem;height:1.25rem;accent-color:var(--primary)";
                    span .form-label style="margin:0" { (label) }
                }
                @if !var.description.is_empty() {
                    p .text-muted style="font-size:0.8rem;margin-top:0.25rem" { (var.description) }
                }
            }
        },
        InputType::Password => {
            html! {
                div .form-group style="margin-bottom:1.25rem" {
                    label .form-label for=(var.key) { (label) }
                    div style="display:flex;align-items:center;gap:0.5rem" {
                        input .form-input #(var.key) name=(var.key) type="password" value=(value)
                            placeholder=(if has_value { "******** (set)" } else { "Not configured" })
                            style="flex:1";
                        button type="button" .btn .btn-ghost .btn-sm
                            onclick={"var i=document.getElementById('" (var.key) "');if(i.type==='password'){i.type='text';this.title='Hide';this.setAttribute('aria-label','Hide value')}else{i.type='password';this.title='Reveal';this.setAttribute('aria-label','Reveal value')}"}
                            title="Reveal"
                            aria-label="Reveal value"
                        { (super::icons::eye()) }
                    }
                    @if !var.description.is_empty() {
                        p .text-muted style="font-size:0.8rem;margin-top:0.25rem" { (var.description) }
                    }
                }
            }
        }
        InputType::Color => html! {
            div .form-group style="margin-bottom:1.25rem" {
                label .form-label for=(var.key) { (label) }
                div style="display:flex;align-items:center;gap:0.75rem" {
                    input .form-input #(var.key) name=(var.key) type="text" value=(value)
                        placeholder=(var.default) style="flex:1";
                    input type="color" value=(value)
                        style="width:40px;height:36px;border:1px solid #e2e8f0;border-radius:6px;cursor:pointer;padding:2px"
                        onchange={"document.getElementById('" (var.key) "').value=this.value"};
                }
                @if !var.description.is_empty() {
                    p .text-muted style="font-size:0.8rem;margin-top:0.25rem" { (var.description) }
                }
            }
        },
        InputType::Textarea => html! {
            div .form-group style="margin-bottom:1.25rem" {
                label .form-label for=(var.key) { (label) }
                textarea .form-input #(var.key) name=(var.key) rows="4"
                    placeholder=(var.default) { (value) }
                @if !var.description.is_empty() {
                    p .text-muted style="font-size:0.8rem;margin-top:0.25rem" { (var.description) }
                }
            }
        },
        // A Select without declared options degrades to the plain text
        // widget below rather than rendering an empty, unusable dropdown.
        InputType::Select if !var.options.is_empty() => html! {
            div .form-group style="margin-bottom:1.25rem" {
                label .form-label for=(var.key) { (label) }
                select .form-select #(var.key) name=(var.key) {
                    @for option in &var.options {
                        option value=(option.value)
                            selected[option.value == value
                                || (value.is_empty() && option.value == var.default)]
                        { (option.label) }
                    }
                }
                @if !var.description.is_empty() {
                    p .text-muted style="font-size:0.8rem;margin-top:0.25rem" { (var.description) }
                }
            }
        },
        InputType::Number => html! {
            div .form-group style="margin-bottom:1.25rem" {
                label .form-label for=(var.key) { (label) }
                input .form-input #(var.key) name=(var.key) type="number" value=(value)
                    placeholder=(var.default);
                @if !var.description.is_empty() {
                    p .text-muted style="font-size:0.8rem;margin-top:0.25rem" { (var.description) }
                }
            }
        },
        InputType::Url | InputType::Text | InputType::Select => html! {
            div .form-group style="margin-bottom:1.25rem" {
                label .form-label for=(var.key) { (label) }
                input .form-input #(var.key) name=(var.key) type="text" value=(value)
                    placeholder=(var.default);
                @if !var.description.is_empty() {
                    p .text-muted style="font-size:0.8rem;margin-top:0.25rem" { (var.description) }
                }
            }
        },
    }
}

/// The single inline submit snippet shared by every settings form. Posts the
/// form as a JSON object to `post_url` and shows a toast with the result.
/// `post_url` is interpolated via `serde_json` so it can't break out of the
/// JS string literal.
fn submit_js(post_url: &str) -> String {
    let url = serde_json::to_string(post_url).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"
function submitSettings(e) {{
    e.preventDefault();
    var form = document.getElementById('settings-form');
    var data = {{}};
    form.querySelectorAll('input[name], select[name], textarea[name]').forEach(function(el) {{
        if (el.type === 'checkbox') {{ data[el.name] = el.checked ? 'true' : 'false'; }}
        else {{ data[el.name] = el.value; }}
    }});
    var btn = form.querySelector('button[type="submit"]');
    btn.disabled = true; btn.textContent = 'Saving...';
    fetch({url}, {{ method: 'POST', headers: {{ 'Content-Type': 'application/json' }}, body: JSON.stringify(data) }})
    .then(function(r) {{ return r.json(); }})
    .then(function(d) {{ document.body.dispatchEvent(new CustomEvent('showToast', {{ detail: {{ message: d.message || 'Saved', type: d.error ? 'error' : 'success' }} }})); }})
    .catch(function(err) {{ document.body.dispatchEvent(new CustomEvent('showToast', {{ detail: {{ message: 'Error: ' + err.message, type: 'error' }} }})); }})
    .finally(function() {{ btn.disabled = false; btn.textContent = 'Save Settings'; }});
    return false;
}}
"#
    )
}

/// Render just the field groups for each [`SettingsSection`] — a heading
/// plus its inputs, values loaded from the config client — with NO enclosing
/// `<form>`, submit button, or submit script.
///
/// Callers want the full self-contained form ([`settings_form`], which is a
/// thin wrapper around this). This lower-level entry point exists for a
/// caller that already lives inside another `<form>` element it doesn't own:
/// HTML forms can't nest — a second literal `<form>` there would have its
/// start tag silently dropped by the browser's parser and its close tag
/// would prematurely close the outer one — so such a caller renders fields
/// only and leaves form/submit ownership to its host. (The admin Settings
/// shell used to be such a host; it's form-less now — `ui::templates::
/// tabbed_page` — precisely so every tab can render a complete
/// [`settings_form`] instead.)
pub async fn render_sections(ctx: &dyn Context, sections: &[SettingsSection<'_>]) -> Markup {
    let values = load_values(ctx, sections).await;
    let empty = String::new();
    html! {
        @for (i, section) in sections.iter().enumerate() {
            h3 style=(format!(
                "font-size:1rem;font-weight:600;margin:{} 0 {};padding-bottom:0.5rem;border-bottom:1px solid var(--border-color)",
                if i == 0 { "0" } else { "1.5rem" },
                if section.description.is_empty() { "1rem" } else { "0.5rem" }
            )) {
                (section.icon) " " (section.title)
            }
            @if !section.description.is_empty() {
                p .text-muted style="font-size:0.85rem;margin:0 0 1rem" { (section.description) }
            }
            @for var in section.vars {
                (render_field(var, values.get(&var.key).unwrap_or(&empty)))
            }
        }
    }
}

/// Render the full ConfigVar-driven settings form: a `#settings-form` posting
/// JSON to `post_url`, with one titled section per [`SettingsSection`], a
/// "Save Settings" button, and the shared submit snippet. Current values are
/// loaded from the config client internally.
///
/// `extra` is appended after the last section and before the submit button —
/// used by blocks that want an extra panel inside the form (e.g. legalpages'
/// live-preview links).
pub async fn settings_form(
    ctx: &dyn Context,
    post_url: &str,
    sections: &[SettingsSection<'_>],
    extra: Markup,
) -> Markup {
    let fields = render_sections(ctx, sections).await;
    html! {
        form #settings-form onsubmit="return submitSettings(event)" {
            (fields)
            (extra)
            button .btn .btn-primary type="submit" style="margin-top:1rem" { "Save Settings" }
        }
        script { (PreEscaped(submit_js(post_url))) }
    }
}

/// Generic settings save handler: parse the JSON body, and for every key in
/// the `allowed` ConfigVar allowlist that the body carries, `config::set` it.
/// Keys outside the allowlist are ignored (a block can only write the vars it
/// declared). Parse failure returns a real `400` (htmx clients branch on the
/// status, not a 200-with-`error`-key body — the residual finding folded in
/// from S1-I).
pub async fn save_settings(
    ctx: &dyn Context,
    input: InputStream,
    allowed: &[ConfigVar],
    block_label: &str,
) -> OutputStream {
    let raw = input.collect_to_bytes().await;
    let body: HashMap<String, String> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid request: {e}")),
    };
    // Validate every URL-typed value up front so one bad URL can't leave a
    // half-applied save. `is_url()` (InputType::Url) is the declared rule, and
    // `validate_url_value` is the same SSRF check the admin variables page runs —
    // shared so the two write surfaces can't accept divergent inputs.
    for var in allowed {
        if var.is_url() {
            if let Some(value) = body.get(&var.key) {
                if let Err(e) = validate_url_value(value) {
                    return err_bad_request(&format!("{}: {e}", var.key));
                }
            }
        }
    }
    for var in allowed {
        let Some(value) = body.get(&var.key) else {
            continue;
        };
        // SEC-060: `render_field` never echoes a sensitive var's real value
        // back into the DOM (it renders empty + a placeholder instead), so
        // when the admin re-submits the form without retyping the secret the
        // browser posts back either an empty string or — if some client
        // literally round-trips the placeholder — the `MASKED_VALUE` mask
        // itself. Neither is a real value; treat both as "leave the stored
        // secret alone" instead of overwriting it with blank/mask bytes.
        // Only a genuinely retyped value reaches `config::set`. Uses the
        // same single-sourced `is_sensitive_key` rule as the render path so
        // the two can't disagree on which fields this guard applies to.
        let is_sensitive = is_sensitive_key(&var.key, var.is_sensitive() as i64);
        if is_sensitive && (value.is_empty() || value == MASKED_VALUE) {
            continue;
        }
        // Surface the first write failure instead of reporting a false
        // "saved" — htmx clients branch on the status, not a 200 body.
        if let Err(e) = config::set(ctx, &var.key, value).await {
            return err_internal(&format!("Failed to save {block_label} settings"), e);
        }
    }
    ok_json(&serde_json::json!({"message": "Settings saved"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(key: &str, name: &str, input_type: InputType) -> ConfigVar {
        ConfigVar::new(key, "desc text", "def")
            .name(name)
            .input_type(input_type)
    }

    #[tokio::test]
    async fn section_description_renders_under_the_heading() {
        let ctx = crate::test_support::TestContext::new().await;
        let vars = [var("X__A", "A", InputType::Text)];
        let sections = [
            SettingsSection::new("Checkout", super::super::icons::settings(), &vars)
                .description("Defaults applied to new offers."),
        ];
        let s = render_sections(&ctx, &sections).await.into_string();
        assert!(s.contains("Defaults applied to new offers."), "{s}");
    }

    #[test]
    fn select_field_renders_options_with_current_value_selected() {
        let v = var("X__CCY", "Default Currency", InputType::Select)
            .options(&[("USD", "USD — US Dollar"), ("NZD", "NZD — NZ Dollar")]);
        let s = render_field(&v, "NZD").into_string();
        assert!(s.contains("<select"), "renders a select: {s}");
        assert!(s.contains(r#"name="X__CCY""#));
        assert!(s.contains(r#"value="USD""#));
        assert!(
            s.contains(r#"value="NZD" selected"#),
            "current value is preselected: {s}"
        );
        assert!(s.contains("NZD — NZ Dollar"));
    }

    #[test]
    fn select_without_options_falls_back_to_text_input() {
        let v = var("X__MODE", "Mode", InputType::Select);
        let s = render_field(&v, "live").into_string();
        assert!(!s.contains("<select"), "no options, no select: {s}");
        assert!(s.contains(r#"type="text""#));
        assert!(s.contains(r#"value="live""#));
    }

    #[test]
    fn number_field_renders_number_input() {
        let v = var("X__FEE", "Fee (bps)", InputType::Number);
        let s = render_field(&v, "250").into_string();
        assert!(s.contains(r#"type="number""#), "number widget: {s}");
        assert!(s.contains(r#"value="250""#));
    }

    #[test]
    fn text_field_renders_text_input_with_label_and_help() {
        let v = var("WAFER_RUN_SHARED__APP_NAME", "App Name", InputType::Text);
        let s = render_field(&v, "MyApp").into_string();
        assert!(s.contains(r#"name="WAFER_RUN_SHARED__APP_NAME""#));
        assert!(s.contains(r#"type="text""#));
        assert!(s.contains(r#"value="MyApp""#));
        assert!(s.contains(">App Name<"));
        assert!(s.contains("desc text"));
    }

    #[test]
    fn password_field_is_masked_with_eye_toggle_and_never_echoes_the_raw_value() {
        // SEC-060: only masking a secret via `type="password"` still leaves
        // the raw bytes sitting in the HTML `value=` attribute — readable in
        // page source / devtools. The rendered markup must never contain the
        // secret at all, regardless of the visual widget.
        let v = var("X__PW", "Secret", InputType::Password);
        let set = render_field(&v, "hunter2").into_string();
        assert!(
            !set.contains("hunter2"),
            "the raw secret must never reach the rendered HTML: {set}"
        );
        assert!(set.contains(r#"type="password""#));
        assert!(
            set.contains(r#"value="""#),
            "value must render empty: {set}"
        );
        assert!(set.contains("(set)"));
        // Eye toggle present, with an accessible name that the handler keeps
        // in sync with the shown/hidden state (2026-07-11 a11y review).
        assert!(set.contains(r#"aria-label="Reveal value""#));
        assert!(set.contains(
            "i.type='text';this.title='Hide';this.setAttribute('aria-label','Hide value')"
        ));
        assert!(set.contains(
            "i.type='password';this.title='Reveal';this.setAttribute('aria-label','Reveal value')"
        ));

        let empty = render_field(&v, "").into_string();
        assert!(empty.contains("Not configured"));
    }

    #[test]
    fn key_with_secret_suffix_is_redacted_even_without_password_input_type() {
        // Defense-in-depth: a var accidentally left on the default `Text`
        // widget whose key still ends `_SECRET`/`_KEY` must not leak either
        // — the single-sourced `is_sensitive_key` suffix rule catches it
        // exactly like `blocks::admin::ops::is_sensitive_key` does for the
        // Variables table, independent of which widget `input_type` picked.
        let v = var(
            "LEGACY_APP__WEBHOOK_SECRET",
            "Webhook Secret",
            InputType::Text,
        );
        let s = render_field(&v, "shh-dont-tell").into_string();
        assert!(
            !s.contains("shh-dont-tell"),
            "a `_SECRET`-suffixed key must be redacted regardless of input_type: {s}"
        );
    }

    #[test]
    fn color_field_pairs_text_input_with_color_picker() {
        let v = var("X__COLOR", "Brand", InputType::Color);
        let s = render_field(&v, "#abcdef").into_string();
        assert!(s.contains(r#"type="color""#));
        assert!(s.contains("value=\"#abcdef\""));
        assert!(s.contains("onchange="));
    }

    #[test]
    fn toggle_field_renders_checkbox_checked_for_true() {
        let v = var("X__FLAG", "Flag", InputType::Toggle);
        let on = render_field(&v, "true").into_string();
        assert!(on.contains(r#"type="checkbox""#));
        assert!(on.contains("checked"));
        let off = render_field(&v, "false").into_string();
        assert!(!off.contains("checked"));
    }

    #[test]
    fn textarea_field_renders_multiline_with_value_as_content() {
        let v = var("X__FOOTER", "Footer Text", InputType::Textarea);
        let s = render_field(&v, "© 2026 Me").into_string();
        assert!(s.contains("<textarea"), "should render a textarea: {s}");
        assert!(s.contains(r#"name="X__FOOTER""#));
        // A textarea carries its value as element content, not a value= attr.
        assert!(s.contains("© 2026 Me"));
        assert!(
            !s.contains(r#"type="text""#),
            "textarea must not be a text input: {s}"
        );
        assert!(s.contains(">Footer Text<"));
    }

    #[test]
    fn field_falls_back_to_key_when_name_empty() {
        let v = ConfigVar::new("X__NONAME", "", "").input_type(InputType::Text);
        let s = render_field(&v, "").into_string();
        assert!(s.contains(">X__NONAME<"));
    }

    #[test]
    fn submit_js_interpolates_post_url_safely() {
        let js = submit_js("/b/products/admin/settings");
        assert!(js.contains(r#"fetch("/b/products/admin/settings""#));
        // a quote in the url must not break out of the string literal
        let js2 = submit_js(r#"/x"); alert(1);//"#);
        assert!(!js2.contains(r#"fetch("/x");"#));
    }

    // --- save_settings: URL validation (M16) + error surfacing (M24) ---
    use wafer_run::{streams::output::TerminalNotResponse, InputStream};

    use crate::test_support::{output_json, TestContext};

    async fn run_save(
        ctx: &TestContext,
        allowed: &[ConfigVar],
        body: serde_json::Value,
    ) -> OutputStream {
        let input = InputStream::from_bytes(serde_json::to_vec(&body).unwrap());
        save_settings(ctx, input, allowed, "test").await
    }

    #[tokio::test]
    async fn save_settings_rejects_ssrf_url_for_url_typed_var() {
        let ctx = TestContext::new().await;
        let allowed = [var(
            "WAFER_RUN_SHARED__BRANDING__LOGO_URL",
            "Logo",
            InputType::Url,
        )];
        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({"WAFER_RUN_SHARED__BRANDING__LOGO_URL": "https://10.0.0.1/logo.png"}),
        )
        .await;
        assert!(
            matches!(
                out.collect_buffered().await,
                Err(TerminalNotResponse::Error(_))
            ),
            "an SSRF/private-IP URL must be rejected, not silently saved"
        );
    }

    #[tokio::test]
    async fn save_settings_surfaces_config_set_failure() {
        // No `wafer-run/config` block registered → config::set fails. Before the
        // fix the loop swallowed the error and reported success anyway.
        let ctx = TestContext::new().await;
        let allowed = [var("WAFER_RUN_SHARED__APP_NAME", "App", InputType::Text)];
        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({"WAFER_RUN_SHARED__APP_NAME": "MyApp"}),
        )
        .await;
        assert!(
            matches!(
                out.collect_buffered().await,
                Err(TerminalNotResponse::Error(_))
            ),
            "a failed config::set must surface as an error, not a 'saved' success"
        );
    }

    // --- SEC-060: render_sections never leaks a raw secret into the DOM ---

    #[tokio::test]
    async fn render_sections_never_leaks_a_stored_secret_into_the_html() {
        let mut ctx = TestContext::new().await;
        ctx.set_config(
            "IMPRESSPRESS__EMAIL__MAILGUN_API_KEY",
            "key-abcdef0123456789",
        );
        let v = var(
            "IMPRESSPRESS__EMAIL__MAILGUN_API_KEY",
            "Mailgun API Key",
            InputType::Password,
        );
        let sections = [SettingsSection::new(
            "Email",
            html! {},
            std::slice::from_ref(&v),
        )];
        let out = render_sections(&ctx, &sections).await.into_string();

        assert!(
            !out.contains("key-abcdef0123456789"),
            "raw secret must never reach the rendered HTML: {out}"
        );
        assert!(out.contains("(set)"));
        assert!(out.contains(r#"type="password""#));
    }

    // --- SEC-060: save_settings' unchanged-secret guard ---

    #[tokio::test]
    async fn save_settings_leaves_a_sensitive_field_unchanged_on_empty_or_masked_submit() {
        let mut ctx = TestContext::new().await;
        // Registers a real `wafer-run/config` service block (TestContext::set_config)
        // and seeds the current stored secret.
        ctx.set_config("X__API_SECRET", "original-secret");
        let allowed = [var("X__API_SECRET", "API Secret", InputType::Password)];

        // Empty submit (what `render_field` actually emits for a set secret,
        // per the render-side fix) must not touch the stored value.
        let out = run_save(&ctx, &allowed, serde_json::json!({"X__API_SECRET": ""})).await;
        let body = output_json(out).await;
        assert_eq!(body["message"], "Settings saved");
        assert_eq!(
            config::get_default(&ctx, "X__API_SECRET", "").await,
            "original-secret",
            "an empty submit for a sensitive field must not clear/overwrite the stored secret"
        );

        // A literal round-trip of the mask placeholder must also be treated
        // as "unchanged", not stored as the literal mask string.
        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({"X__API_SECRET": MASKED_VALUE}),
        )
        .await;
        let body = output_json(out).await;
        assert_eq!(body["message"], "Settings saved");
        assert_eq!(
            config::get_default(&ctx, "X__API_SECRET", "").await,
            "original-secret",
            "submitting the mask placeholder must not overwrite the stored secret with it"
        );

        // A genuinely new value must still be written.
        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({"X__API_SECRET": "brand-new-secret"}),
        )
        .await;
        let body = output_json(out).await;
        assert_eq!(body["message"], "Settings saved");
        assert_eq!(
            config::get_default(&ctx, "X__API_SECRET", "").await,
            "brand-new-secret",
            "a genuinely retyped secret must be saved"
        );
    }

    #[tokio::test]
    async fn save_settings_still_clears_a_non_sensitive_field_on_empty_submit() {
        // Non-sensitive fields keep the pre-existing behavior: an empty
        // submit is a real write (clears the stored value), not "unchanged".
        let mut ctx = TestContext::new().await;
        ctx.set_config("WAFER_RUN_SHARED__APP_NAME", "MyApp");
        let allowed = [var(
            "WAFER_RUN_SHARED__APP_NAME",
            "App Name",
            InputType::Text,
        )];

        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({"WAFER_RUN_SHARED__APP_NAME": ""}),
        )
        .await;
        let body = output_json(out).await;
        assert_eq!(body["message"], "Settings saved");
        assert_eq!(
            config::get_default(&ctx, "WAFER_RUN_SHARED__APP_NAME", "").await,
            "",
            "a non-sensitive field's empty submit is a real write, unlike a sensitive field's"
        );
    }
}
