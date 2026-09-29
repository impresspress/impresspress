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
use wafer_run::{context::Context, InputStream, Message, OutputStream, WaferError};
pub use wafer_run::{ConfigVar, InputType};

use crate::{
    blocks::admin::logs::audit_log,
    config_vars::is_truthy,
    http::{err_bad_request, err_internal, ok_json},
    util::{is_sensitive_key, validate_declared_config_value, MASKED_VALUE},
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
    /// Whether this optional section starts behind a native disclosure
    /// control. Existing pages remain expanded unless they opt in.
    pub collapsible: bool,
}

impl<'a> SettingsSection<'a> {
    /// Construct a section from a title, icon, and its variables.
    pub fn new(title: &'a str, icon: Markup, vars: &'a [ConfigVar]) -> Self {
        Self {
            title,
            icon,
            vars,
            description: "",
            collapsible: false,
        }
    }

    /// Set the one-line explanation rendered under the section heading.
    pub fn description(mut self, description: &'a str) -> Self {
        self.description = description;

        self
    }

    /// Make optional or advanced settings secondary to the page essentials.
    pub fn collapsible(mut self) -> Self {
        self.collapsible = true;
        self
    }
}

/// The value each of `vars` currently reads as — stored, else the boot map,
/// else the var's declared default — or `Err` when the `variables` table could
/// not be read.
///
/// Through [`crate::blocks::config::get_many`], never `config::get_default`:
/// that one answers an unreadable table from the boot map and then the
/// default, and `submit_js` posts every field, so a form rendered from it and
/// saved writes those over every stored value on the page.
///
/// `get_many` also refuses the whole batch when WRAP denies the caller ONE of
/// the keys, so a page that renders a var its block may not read is an error
/// page rather than a field showing the default: the page has no value to
/// show and its Save would have written the default over the stored one.
async fn current_values<'v>(
    ctx: &dyn Context,
    vars: impl IntoIterator<Item = &'v ConfigVar>,
) -> Result<HashMap<String, String>, WaferError> {
    let vars: Vec<&ConfigVar> = vars.into_iter().collect();
    let keys: Vec<&str> = vars.iter().map(|var| var.key.as_str()).collect();
    let mut values = crate::blocks::config::get_many(ctx, &keys).await?;
    for var in vars {
        values
            .entry(var.key.clone())
            .or_insert_with(|| var.default.clone());
    }
    Ok(values)
}

/// Load the current value for every var in `sections`.
async fn load_values(
    ctx: &dyn Context,
    sections: &[SettingsSection<'_>],
) -> Result<HashMap<String, String>, WaferError> {
    current_values(ctx, sections.iter().flat_map(|section| section.vars)).await
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
    // *or* the key rule says so — the `_SECRET`/`_KEY` suffix convention, or
    // the same key declared `Password`/`auto_generate` anywhere in the build —
    // so a var left on the default `Text` widget by mistake still gets
    // redacted.
    // `has_value` is captured from the real value (presence only, never its
    // content) so the placeholder can still distinguish "configured" from
    // "not configured" without ever exposing the secret itself.
    let is_sensitive = is_sensitive_key(&var.key, var.is_sensitive() as i64);
    let has_value = !value.is_empty();
    let value = if is_sensitive { "" } else { value };
    match var.input_type {
        InputType::Toggle => html! {
            div .form-group {
                label .form-checkbox {
                    input type="checkbox" name=(var.key) checked[is_truthy(value)];
                    (label)
                }
                @if !var.description.is_empty() {
                    p .form-hint { (var.description) }
                }
            }
        },
        InputType::Password => {
            // Built from `MASKED_VALUE` rather than spelled out, so the
            // placeholder and the mask every read path emits cannot drift into
            // two different strings. The admin Variables edit modal renders the
            // same pair.
            let placeholder = if has_value {
                format!("{MASKED_VALUE} (set)")
            } else {
                "Not configured".to_string()
            };
            html! {
                div .form-group {
                    label .form-label for=(var.key) { (label) }
                    div .value-reveal-wrapper {
                        input .form-input #(var.key) name=(var.key) type="password" value=(value)
                            placeholder=(placeholder);
                        button type="button" .btn .btn--ghost .btn--icon .btn-icon-right
                            data-action="reveal-toggle"
                            data-reveal-target=(var.key)
                            data-reveal-show="Reveal"
                            data-reveal-hide="Hide"
                            title="Reveal"
                            aria-label="Reveal value"
                        { (super::icons::eye()) }
                    }
                    @if !var.description.is_empty() {
                        p .form-hint { (var.description) }
                    }
                }
            }
        }
        InputType::Color => html! {
            div .form-group {
                label .form-label for=(var.key) { (label) }
                div .color-picker-row {
                    input .form-input #(var.key) name=(var.key) type="text" value=(value)
                        placeholder=(var.default);
                    input .color-swatch-input type="color" value=(value)
                        data-action="mirror-value" data-mirror-target=(var.key);
                }
                @if !var.description.is_empty() {
                    p .form-hint { (var.description) }
                }
            }
        },
        InputType::Textarea => html! {
            div .form-group {
                label .form-label for=(var.key) { (label) }
                textarea .form-input #(var.key) name=(var.key) rows="4"
                    placeholder=(var.default) { (value) }
                @if !var.description.is_empty() {
                    p .form-hint { (var.description) }
                }
            }
        },
        // A Select without declared options degrades to the plain text
        // widget below rather than rendering an empty, unusable dropdown.
        InputType::Select if !var.options.is_empty() => html! {
            div .form-group {
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
                    p .form-hint { (var.description) }
                }
            }
        },
        InputType::Number => html! {
            div .form-group {
                label .form-label for=(var.key) { (label) }
                input .form-input #(var.key) name=(var.key) type="number" value=(value)
                    placeholder=(var.default);
                @if !var.description.is_empty() {
                    p .form-hint { (var.description) }
                }
            }
        },
        InputType::Url | InputType::Text | InputType::Select => html! {
            div .form-group {
                label .form-label for=(var.key) { (label) }
                input .form-input #(var.key) name=(var.key) type="text" value=(value)
                    placeholder=(var.default);
                @if !var.description.is_empty() {
                    p .form-hint { (var.description) }
                }
            }
        },
    }
}

/// The single inline submit snippet shared by every settings form. Posts the
/// form as a JSON object to `post_url` and shows a toast with the result.
/// `post_url` is interpolated via `serde_json` so it can't break out of the
/// JS string literal.
///
/// The form binds this by being `#settings-form`, not by an `onsubmit`
/// attribute — see the delegated-action rule in `ui/assets/chrome.js`. The
/// listener is on `document`, so a form that arrives in an htmx swap is bound
/// too, which an attribute-free direct `addEventListener` here would miss.
fn submit_js(post_url: &str) -> String {
    let url = serde_json::to_string(post_url).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"
if (!window.__settingsFormInit) {{
    window.__settingsFormInit = true;
    document.addEventListener('submit', function (e) {{
        if (e.target && e.target.id === 'settings-form') submitSettings(e);
    }});
}}
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
    .finally(function() {{ btn.disabled = false; btn.textContent = 'Save settings'; }});
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
///
/// `Err` when the current values could not be read: the caller answers with
/// an error page, never with fields filled from defaults.
pub async fn render_sections(
    ctx: &dyn Context,
    sections: &[SettingsSection<'_>],
) -> Result<Markup, WaferError> {
    let values = load_values(ctx, sections).await?;
    let empty = String::new();
    Ok(html! {
        @for (i, section) in sections.iter().enumerate() {
            @if section.collapsible {
                details .card .mt-4 {
                    summary .summary-strong { (section.icon) " " (section.title) }
                    @if !section.description.is_empty() {
                        p .settings-section-desc { (section.description) }
                    }
                    @for var in section.vars {
                        (render_field(var, values.get(&var.key).unwrap_or(&empty)))
                    }
                }
            } @else {
                h3 class={
                    "settings-section-heading"
                    @if i == 0 { " settings-section-heading--first" }
                    @if !section.description.is_empty() { " settings-section-heading--with-desc" }
                } {
                    (section.icon) " " (section.title)
                }
                @if !section.description.is_empty() {
                    p .settings-section-desc { (section.description) }
                }
                @for var in section.vars {
                    (render_field(var, values.get(&var.key).unwrap_or(&empty)))
                }
            }
        }
    })
}

/// Render the full ConfigVar-driven settings form: a `#settings-form` posting
/// JSON to `post_url`, with one titled section per [`SettingsSection`], a
/// "Save Settings" button, and the shared submit snippet. Current values are
/// loaded from the config block internally; `Err` when they could not be read,
/// and the caller answers an error page instead of a form (see
/// [`render_sections`]) — [`crate::blocks::crud::db_error_page`], which keeps
/// a WRAP denial's 403 where [`crate::ui::server_error_response`] would make
/// it a 500.
///
/// `extra` is appended after the last section and before the submit button —
/// used by blocks that want an extra panel inside the form (e.g. legalpages'
/// live-preview links).
pub async fn settings_form(
    ctx: &dyn Context,
    post_url: &str,
    sections: &[SettingsSection<'_>],
    extra: Markup,
) -> Result<Markup, WaferError> {
    let fields = render_sections(ctx, sections).await?;
    Ok(html! {
        form #settings-form {
            (fields)
            (extra)
            button .btn .btn--primary .mt-4 type="submit" { "Save settings" }
        }
        script { (PreEscaped(submit_js(post_url))) }
    })
}

/// Generic settings save handler: parse the JSON body, and for every key in
/// the `allowed` ConfigVar allowlist that the body carries, `config::set` it.
/// Keys outside the allowlist are ignored (a block can only write the vars it
/// declared). Parse failure returns a real `400` (htmx clients branch on the
/// status, not a 200-with-`error`-key body — the residual finding folded in
/// from S1-I).
///
/// Writes one `settings.update` audit row naming `block_label` and the keys
/// this save actually wrote — the same trail
/// `ops::update_variable` writes for the identical
/// keys edited on the admin Variables page, so which page an operator used
/// stops deciding whether the change is recorded. `msg` carries the admin id
/// and remote address the row is attributed to.
pub async fn save_settings(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    allowed: &[ConfigVar],
    block_label: &str,
) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: HashMap<String, String> = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid request: {e}")),
    };
    // Validate every refusable value up front so one bad field can't leave a
    // half-applied save. `util::validate_declared_config_value` is the rule the
    // admin variables page and `config::set` run (SSRF for a URL, the declared
    // value rules), with `is_url()` (InputType::Url) also selecting the SSRF
    // check — shared so the write surfaces can't accept divergent inputs.
    //
    // The mask refusal belongs to the SAME pre-pass and for the same reason: a
    // refusal raised from inside the write loop below goes out after
    // `config::set` has already run for every var ahead of it in the allowlist,
    // which is precisely the half-applied save this pass exists to prevent. The
    // mask is not something this form can post — a sensitive field renders
    // blank, and a placeholder is not submitted — so a client that sends one
    // round-tripped a read, and storing it would replace the secret with eight
    // asterisks. See `util::is_masked_submission`.
    //
    // Checked for EVERY allowlisted var, not only the ones this module can see
    // are sensitive, and the asymmetry is the point. The writer
    // (`blocks::config::ConfigWrite::write`) decides on the STORED row's flag;
    // this module only has the declared `ConfigVar`, and the two diverge for a
    // declared var that is neither `Password`, `auto_generate`, `_SECRET` nor
    // `_KEY` but whose row an operator flagged sensitive — reachable from the
    // Variables edit modal and from `handle_create`'s absent-means-sensitive
    // default. It cannot close that gap by looking: WRAP denies four of
    // `save_settings`' five callers (legalpages, auth_ui, userportal, products;
    // `admin::pages::email` is the exception, since it runs as the admin block
    // itself) the admin `variables` table, which is the whole reason
    // `blocks::config` reads it through the raw `DatabaseService` — and a
    // shared helper has to work for the four. So it refuses what it cannot RULE
    // OUT, never a subset of the writer's refusals, which is the only shape
    // that guarantees no write has started when a refusal goes out.
    //
    // What keeps that from taking the page down with it: the refusal is for a
    // mask that would REPLACE a value, so a submission equal to what is already
    // stored is not refused at all. That case is not exotic — `render_field`
    // blanks only the fields it can see are sensitive, so a plain-declared row
    // whose value happens to be the mask is rendered back into its input and
    // posted by `submit_js` like any other, and refusing it 400'd the whole
    // form on every save with no way out (blank is a real write for a plain
    // field: it CLEARS). Such a row has four writers, not one — the JSON API,
    // the admin Variables edit modal (`ops::update_variable` deliberately does
    // not drop a typed mask), env seeding through `insert_if_absent`, and
    // `dev::data_snapshot::import`.
    //
    // "Already stored" is read through `current_values`, which is the exact
    // call `load_values` makes for `render_field` — so the question asked here
    // is the one the browser's answer was formed from. Reading it from
    // anywhere else would reintroduce the drift this guard exists to close. It
    // costs a read only when a mask is actually submitted, and a read that
    // fails refuses the save before any write, as the page itself would have
    // refused to render.
    //
    // `ConfigWrite::write` asks it the same way, against a non-empty row else
    // the boot map, and that is load-bearing rather than tidy: it compared
    // against the row alone once, so for a key with no row (or an empty one)
    // whose BOOT value is the mask, this pre-pass allowed and the writer
    // refused — mid-loop, page half saved. See
    // `a_boot_map_value_equal_to_the_mask_is_not_a_half_applied_save`. The two
    // sides ask one question; neither is left inferring the other's answer.
    let masked: Vec<&ConfigVar> = allowed
        .iter()
        .filter(|var| {
            body.get(&var.key)
                .is_some_and(|value| value == MASKED_VALUE)
        })
        .collect();
    let current = if masked.is_empty() {
        HashMap::new()
    } else {
        match current_values(ctx, masked).await {
            Ok(current) => current,
            Err(e) => return err_internal("Could not read the current settings", e),
        }
    };
    for var in allowed {
        let Some(value) = body.get(&var.key) else {
            continue;
        };
        // The declared type's URL rule, plus everything `config::set`'s
        // writer runs — checked here so a refusal cannot land mid-loop after
        // earlier fields have already been written.
        if let Err(e) = validate_declared_config_value(var, value) {
            return err_bad_request(&format!("{}: {e}", var.key));
        }
        if value == MASKED_VALUE && current.get(&var.key) != Some(value) {
            // The remedy differs by field, so the message does too — the same
            // sentence cannot be true of both. A sensitive field renders blank
            // and `save_settings` reads blank as "unchanged"; a plain field
            // renders its value and blank CLEARS it, so telling that operator
            // to blank the field would be telling them to destroy it.
            //
            // The plain branch names no other surface to go and do it on. It is
            // chosen from the DECLARED var, and the case that makes this guard
            // necessary at all is the declared-plain row an operator flagged —
            // for which the admin Variables page reads the ROW's flag and
            // refuses with the opposite explanation. Sending an operator there
            // would be sending them to a dead end.
            let remedy = if is_sensitive_key(&var.key, var.is_sensitive() as i64) {
                "Type the real value to change it, or leave the field blank to keep the \
                 stored one."
            } else {
                "Type the value you want stored — this form cannot store that literal \
                 string."
            };
            // No claim about what is stored now: this same refusal covers a key
            // with nothing stored at all, where the write would be a create.
            return err_bad_request(&format!(
                "{}: {MASKED_VALUE} is what a masked value reads back as, not a value. \
                 {remedy}",
                var.key
            ));
        }
    }
    // Which keys this save actually wrote, in allowlist order. Collected
    // rather than assumed from the body: a sensitive field submitted blank
    // means "not touched" and is skipped below, and a write can fail
    // mid-loop. The audit row names what happened, so a half-applied save is
    // recorded as the keys that landed rather than not at all.
    let mut written: Vec<&str> = Vec::new();
    let mut write_failure = None;
    for var in allowed {
        let Some(value) = body.get(&var.key) else {
            continue;
        };
        // SEC-060: `render_field` never echoes a sensitive var's real value
        // back into the DOM (it renders empty + a placeholder instead), so
        // when the admin re-submits the form without retyping the secret the
        // browser posts back an empty string. That is the widget saying "I did
        // not touch this" — the only thing a blank masked field can mean — so
        // the stored secret is left alone and only a genuinely retyped value
        // reaches `config::set`. Uses the same single-sourced
        // `is_sensitive_key` rule as the render path so the two can't disagree
        // on which fields this applies to.
        // The mask is a different case with a different answer, and it was
        // already refused by the pre-pass above — only an empty field reaches
        // here as "unchanged".
        let is_sensitive = is_sensitive_key(&var.key, var.is_sensitive() as i64);
        if is_sensitive && value.is_empty() {
            continue;
        }
        // Surface the first write failure instead of reporting a false
        // "saved" — htmx clients branch on the status, not a 200 body.
        //
        // A REFUSAL is forwarded with its own code and message rather than
        // flattened into `err_internal`. `ConfigWrite::write` answers
        // `InvalidArgument` for the guards it enforces — "Cannot set {key} to
        // an empty value" is the one that still reaches here, since clearing a
        // declared-plain field whose ROW an operator flagged is a thing the
        // rendered form can produce and the pre-pass above cannot predict
        // (it would have to refuse every clear to catch it, and clearing a
        // plain field is legitimate). Reported as a 500 "Failed to save X
        // settings", that told the operator the server broke and named nothing
        // they could act on; forwarded, it is a 400 naming the key and the
        // reason. The write it refuses has not happened, but writes ahead of it
        // in the allowlist have — the residual half-applied save this module
        // cannot close without the stored flag it is not allowed to read.
        if let Err(e) = config::set(ctx, &var.key, value).await {
            write_failure = Some(if e.code == wafer_run::ErrorCode::InvalidArgument {
                OutputStream::error(e)
            } else {
                err_internal(&format!("Failed to save {block_label} settings"), e)
            });
            break;
        }
        written.push(&var.key);
    }
    if !written.is_empty() {
        // Keys only, never values: a settings page writes secrets, and the
        // audit log is readable by every admin.
        audit_log(
            ctx,
            msg.user_id(),
            "settings.update",
            &format!("settings/{block_label} ({})", written.join(", ")),
            msg.remote_addr(),
        )
        .await;
    }
    if let Some(failure) = write_failure {
        return failure;
    }
    ok_json(&serde_json::json!({"message": "Settings saved"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        blocks::email::MAILGUN_API_KEY,
        config_vars::{APP_NAME_KEY, LOGO_URL_KEY},
    };

    fn var(key: &str, name: &str, input_type: InputType) -> ConfigVar {
        ConfigVar::new(key, "desc text", "def")
            .name(name)
            .input_type(input_type)
    }

    #[tokio::test]
    async fn section_description_renders_under_the_heading() {
        let ctx = crate::test_support::TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let vars = [var("X__A", "A", InputType::Text)];
        let sections = [
            SettingsSection::new("Checkout", super::super::icons::settings(), &vars)
                .description("Defaults applied to new offers."),
        ];
        let s = render_sections(&ctx, &sections)
            .await
            .expect("the current values are readable")
            .into_string();
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
        let v = var(APP_NAME_KEY, "App Name", InputType::Text);
        let s = render_field(&v, "MyApp").into_string();
        assert!(s.contains(&format!(r#"name="{APP_NAME_KEY}""#)));
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
        // The two labels are operands now, not a hand-written `onclick`
        // string: `reveal-toggle` in the modal section of
        // `ui/assets/chrome.js` swaps `title` and `aria-label` from them.
        assert!(set.contains(r#"data-action="reveal-toggle""#));
        assert!(set.contains(r#"data-reveal-target="X__PW""#));
        assert!(set.contains(r#"data-reveal-show="Reveal""#));
        assert!(set.contains(r#"data-reveal-hide="Hide""#));

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
        assert!(s.contains(r#"data-action="mirror-value" data-mirror-target="X__COLOR""#));
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
        save_settings(
            ctx,
            &crate::test_support::admin_msg("create", "/b/admin/settings"),
            input,
            allowed,
            "test",
        )
        .await
    }

    #[tokio::test]
    async fn save_settings_rejects_ssrf_url_for_url_typed_var() {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let allowed = [var(LOGO_URL_KEY, "Logo", InputType::Url)];
        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({LOGO_URL_KEY: "https://10.0.0.1/logo.png"}),
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
        // Every config::set fails; a loop that swallowed the error would
        // report success anyway.
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.refuse_config_writes();
        let allowed = [var(APP_NAME_KEY, "App", InputType::Text)];
        let out = run_save(&ctx, &allowed, serde_json::json!({APP_NAME_KEY: "MyApp"})).await;
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
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config(MAILGUN_API_KEY, "key-abcdef0123456789");
        let v = var(MAILGUN_API_KEY, "Mailgun API Key", InputType::Password);
        let sections = [SettingsSection::new(
            "Email",
            html! {},
            std::slice::from_ref(&v),
        )];
        let out = render_sections(&ctx, &sections)
            .await
            .expect("the current values are readable")
            .into_string();

        assert!(
            !out.contains("key-abcdef0123456789"),
            "raw secret must never reach the rendered HTML: {out}"
        );
        assert!(out.contains("(set)"));
        assert!(out.contains(r#"type="password""#));
    }

    // --- SEC-060: save_settings' unchanged-secret guard ---

    #[tokio::test]
    async fn save_settings_leaves_a_sensitive_field_unchanged_on_empty_submit() {
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
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
            config::get_default(&ctx, "X__API_SECRET", "")
                .await
                .expect("config read"),
            "original-secret",
            "an empty submit for a sensitive field must not clear/overwrite the stored secret"
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
            config::get_default(&ctx, "X__API_SECRET", "")
                .await
                .expect("config read"),
            "brand-new-secret",
            "a genuinely retyped secret must be saved"
        );
    }

    /// A literal round-trip of the mask is REFUSED, not silently dropped.
    ///
    /// It used to be folded in with the empty submit as "unchanged", which kept
    /// the secret safe but answered `200 {"message": "Settings saved"}` to a
    /// client whose write was discarded — and since the next read hands that
    /// client the same mask back, nothing it could do would reveal the write
    /// never happened. The two are different things and get different answers:
    /// a blank field is the widget's only way to say "I did not touch this",
    /// while the mask can only have come from a client round-tripping a read.
    /// Same answer as the admin variable surfaces, whose
    /// `ops::update_variable` refuses it identically — see
    /// `util::is_masked_submission`.
    #[tokio::test]
    async fn save_settings_refuses_the_mask_rather_than_storing_or_dropping_it() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config("X__API_SECRET", "original-secret");
        let allowed = [var("X__API_SECRET", "API Secret", InputType::Password)];

        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({"X__API_SECRET": MASKED_VALUE}),
        )
        .await;
        assert_eq!(
            crate::test_support::output_http_status(out).await,
            400,
            "a client that posts the mask must be told, not thanked"
        );
        assert_eq!(
            config::get_default(&ctx, "X__API_SECRET", "")
                .await
                .expect("config read"),
            "original-secret",
            "and the stored secret must survive the refusal"
        );
    }

    /// The refusal must not leave a half-applied save.
    ///
    /// URL validation is deliberately a PRE-PASS for exactly this reason —
    /// "one bad URL can't leave a half-applied save" — and a mask refusal
    /// raised from inside the write loop had the same defect it was hoisted to
    /// avoid: the vars before it in the allowlist were already written when the
    /// 400 went out, so the admin got an error for a form that had partly
    /// saved.
    #[tokio::test]
    async fn a_refused_mask_writes_nothing_at_all() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config(APP_NAME_KEY, "MyApp");
        ctx.set_config("X__API_SECRET", "original-secret");
        // App name first, so it would already be written by the time the mask
        // is reached if the check lived in the write loop.
        let allowed = [
            var(APP_NAME_KEY, "App Name", InputType::Text),
            var("X__API_SECRET", "API Secret", InputType::Password),
        ];

        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({
                APP_NAME_KEY: "Renamed",
                "X__API_SECRET": MASKED_VALUE,
            }),
        )
        .await;
        assert_eq!(crate::test_support::output_http_status(out).await, 400);
        assert_eq!(
            config::get_default(&ctx, APP_NAME_KEY, "")
                .await
                .expect("config read"),
            "MyApp",
            "a refused save must not have written the fields before the refusal"
        );
    }

    /// A var the OPERATOR flagged must not half-apply the save either.
    ///
    /// The pre-pass reads sensitivity from the DECLARED `ConfigVar`; the writer
    /// (`blocks::config::ConfigWrite::write`) reads it from the stored row. The
    /// two diverge for a declared var that is neither `Password`,
    /// `auto_generate`, `_SECRET` nor `_KEY` but whose row an operator flagged
    /// `sensitive = 1` — reachable from the Variables edit modal, and from
    /// `handle_create`'s absent-means-sensitive default. The pre-pass saw no
    /// mask, the writer did, and the refusal landed mid-loop with every var
    /// ahead of it already written: the half-applied save the pre-pass exists
    /// to prevent, reported as a 500.
    ///
    /// This surface cannot ask the writer's question — WRAP denies
    /// `ui::settings_form`'s callers the admin `variables` table, which is the
    /// whole reason `blocks::config` reads it through the raw `DatabaseService`
    /// — so the pre-pass refuses a mask it cannot RULE OUT instead. It is
    /// strictly more cautious than the writer, never less.
    #[tokio::test]
    async fn an_operator_flagged_var_is_refused_before_anything_is_written() {
        use crate::platform_state::variables::{self, NewVariable};

        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx.fixture())
            .await
            .expect("apply admin migrations");
        ctx.boot_config_service().await;

        variables::insert(
            &ctx,
            NewVariable {
                key: APP_NAME_KEY.to_string(),
                value: "MyApp".to_string(),
                name: String::new(),
                description: String::new(),
                warning: String::new(),
                sensitive: false,
                updated_by: String::new(),
                block: None,
            },
        )
        .await
        .expect("seed the app name");
        // Declared plain, flagged by the operator. Neither suffix, no
        // `Password` declaration — only the row says it is sensitive.
        variables::insert(
            &ctx,
            NewVariable {
                key: "X__PLAIN_NOTE".to_string(),
                value: "operator-flagged-value".to_string(),
                name: String::new(),
                description: String::new(),
                warning: String::new(),
                sensitive: true,
                updated_by: String::new(),
                block: None,
            },
        )
        .await
        .expect("seed the flagged note");

        let allowed = [
            var(APP_NAME_KEY, "App Name", InputType::Text),
            var("X__PLAIN_NOTE", "Note", InputType::Text),
        ];
        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({
                APP_NAME_KEY: "Renamed",
                "X__PLAIN_NOTE": MASKED_VALUE,
            }),
        )
        .await;

        assert_eq!(
            crate::test_support::output_http_status(out).await,
            400,
            "the refusal is a bad request that names the remedy, not a 500"
        );
        assert_eq!(
            config::get_default(&ctx, APP_NAME_KEY, "")
                .await
                .expect("config read"),
            "MyApp",
            "and nothing ahead of it in the allowlist may have been written"
        );
        assert_eq!(
            config::get_default(&ctx, "X__PLAIN_NOTE", "")
                .await
                .expect("config read"),
            "operator-flagged-value",
        );
    }

    // --- the save's audit row ---

    /// A save that wrote nothing must not claim in the trail that it did.
    /// A sensitive field submitted blank means "I did not touch this", so a
    /// form of one such field is a no-op, not a `settings.update`.
    #[tokio::test]
    async fn a_save_that_wrote_nothing_writes_no_audit_row() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config("X__API_SECRET", "original-secret");
        let allowed = [var("X__API_SECRET", "API Secret", InputType::Password)];

        let out = run_save(&ctx, &allowed, serde_json::json!({"X__API_SECRET": ""})).await;
        assert_eq!(output_json(out).await["message"], "Settings saved");
        assert_eq!(
            crate::test_support::audit_count(&ctx, "settings.update").await,
            0,
        );
    }

    /// The residual half-applied save this module cannot close still has to
    /// be readable afterwards: the writer refuses a var mid-loop, everything
    /// ahead of it in the allowlist is already written, and the audit row
    /// names exactly those keys rather than being skipped along with the
    /// failure.
    ///
    /// Same fixture as `an_operator_flagged_var_is_refused_before_anything_
    /// is_written`, submitting an EMPTY value instead of the mask: the
    /// pre-pass only looks for the mask, so this one reaches the writer,
    /// which refuses an empty value for a row the operator flagged.
    #[tokio::test]
    async fn a_half_applied_save_audits_the_keys_that_landed() {
        use crate::platform_state::variables::{self, NewVariable};

        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        variables::insert(
            &ctx.fixture(),
            NewVariable {
                key: "X__PLAIN_NOTE".to_string(),
                value: "operator-flagged-value".to_string(),
                name: String::new(),
                description: String::new(),
                warning: String::new(),
                sensitive: true,
                updated_by: String::new(),
                block: None,
            },
        )
        .await
        .expect("seed the flagged note");

        let allowed = [
            var(APP_NAME_KEY, "App Name", InputType::Text),
            var("X__PLAIN_NOTE", "Note", InputType::Text),
        ];
        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({
                APP_NAME_KEY: "Renamed",
                "X__PLAIN_NOTE": "",
            }),
        )
        .await;
        assert_eq!(
            crate::test_support::output_http_status(out).await,
            400,
            "precondition: the writer refuses the second var mid-loop"
        );
        assert_eq!(
            config::get_default(&ctx, APP_NAME_KEY, "")
                .await
                .expect("config read"),
            "Renamed",
            "precondition: the first var was written before the refusal"
        );

        let rows = crate::test_support::audit_rows(&ctx, "settings.update").await;
        assert_eq!(rows.len(), 1, "the writes that landed are recorded");
        assert_eq!(
            crate::util::RecordExt::str_field(&rows[0], "resource"),
            "settings/test (WAFER_RUN_SHARED__APP_NAME)",
            "and only the keys that landed are named"
        );
    }

    /// The two surfaces must agree when the BOOT MAP is what answers the read.
    ///
    /// `config::get_default` prefers a non-empty row and falls back to the boot
    /// map, so when the row is absent or empty the boot map is the value the
    /// form rendered and the pre-pass compares against. The writer compared
    /// against the row alone, which is `""` — so for a key whose boot value is
    /// the mask the pre-pass ALLOWED and the writer REFUSED, mid-loop, with
    /// everything ahead of it in the allowlist already written. The pre-pass
    /// being the stricter of the two was the claim; the fallback runs the other
    /// way.
    ///
    /// Reachable without a doctored fixture: an env-seeded
    /// `BOOTSTRAP_ADMIN_PASSWORD` of eight asterisks, cleared on the Variables
    /// page (the provisioning exemption permits it, leaving an empty row), then
    /// typed again on the auth settings form — a form with five vars ahead of
    /// it.
    #[tokio::test]
    async fn a_boot_map_value_equal_to_the_mask_is_not_a_half_applied_save() {
        use crate::platform_state::variables::{self, NewVariable};

        const BOOT_ONLY: &str = "X__THING_SECRET";

        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx.fixture())
            .await
            .expect("apply admin migrations");
        // No row for `BOOT_ONLY`; the boot map answers it, and with the mask.
        ctx.boot_config_service_with(&[(BOOT_ONLY, MASKED_VALUE)])
            .await;
        variables::insert(
            &ctx,
            NewVariable {
                key: APP_NAME_KEY.to_string(),
                value: "MyApp".to_string(),
                name: String::new(),
                description: String::new(),
                warning: String::new(),
                sensitive: false,
                updated_by: String::new(),
                block: None,
            },
        )
        .await
        .expect("seed the app name");

        assert_eq!(
            config::get_default(&ctx, BOOT_ONLY, "")
                .await
                .expect("config read"),
            MASKED_VALUE,
            "the fixture only means something if the boot map is what the form read"
        );
        assert!(
            variables::get_by_key(&ctx, BOOT_ONLY)
                .await
                .expect("read back")
                .is_none(),
            "...and if no row is what the writer would have compared against"
        );

        let allowed = [
            var(APP_NAME_KEY, "App Name", InputType::Text),
            var(BOOT_ONLY, "Thing Secret", InputType::Password),
        ];
        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({
                APP_NAME_KEY: "Renamed",
                BOOT_ONLY: MASKED_VALUE,
            }),
        )
        .await;

        assert_eq!(
            crate::test_support::output_http_status(out).await,
            200,
            "submitting the value the form was shown must not be refused by the writer \
             after the pre-pass allowed it"
        );
        assert_eq!(
            config::get_default(&ctx, APP_NAME_KEY, "")
                .await
                .expect("config read"),
            "Renamed",
        );
        assert_eq!(
            config::get_default(&ctx, BOOT_ONLY, "")
                .await
                .expect("config read"),
            MASKED_VALUE
        );
    }

    /// A refusal raised by the WRITER is reported as the refusal it is.
    ///
    /// The empty-value case is the one that still reaches `config::set` — a
    /// declared-plain field whose row an operator flagged, cleared on the form,
    /// which the pre-pass cannot predict (it would have to refuse every clear,
    /// and clearing a plain field is legitimate). `ConfigWrite::write` answers
    /// `InvalidArgument`; `save_settings` flattened every failure into
    /// `err_internal`, so the operator got a 500 that named nothing they could
    /// act on for a request the server had understood perfectly.
    ///
    /// Nothing else covers this: the pre-pass tests never reach `config::set`,
    /// and `save_settings_surfaces_config_set_failure` asserts only that SOME
    /// error terminal comes back.
    #[tokio::test]
    async fn a_writer_refusal_is_forwarded_as_a_bad_request() {
        use crate::platform_state::variables::{self, NewVariable};

        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx.fixture())
            .await
            .expect("apply admin migrations");
        ctx.boot_config_service().await;

        // Declared plain, flagged by the operator: only the row says it is
        // sensitive, so the pre-pass lets the empty value through and the
        // writer refuses it.
        variables::insert(
            &ctx,
            NewVariable {
                key: "X__PLAIN_NOTE".to_string(),
                value: "operator-flagged-value".to_string(),
                name: String::new(),
                description: String::new(),
                warning: String::new(),
                sensitive: true,
                updated_by: String::new(),
                block: None,
            },
        )
        .await
        .expect("seed the flagged note");

        let allowed = [var("X__PLAIN_NOTE", "Note", InputType::Text)];
        let out = run_save(&ctx, &allowed, serde_json::json!({"X__PLAIN_NOTE": ""})).await;

        let status = crate::test_support::output_http_status(out).await;
        assert_eq!(
            status, 400,
            "a guard the writer enforces is a bad request, not a server fault"
        );
        assert_eq!(
            config::get_default(&ctx, "X__PLAIN_NOTE", "")
                .await
                .expect("config read"),
            "operator-flagged-value",
        );
    }

    /// A plain field whose stored value ALREADY is the mask must not take the
    /// whole page down with it.
    ///
    /// `render_field` blanks only the fields it can see are sensitive, so such
    /// a row is rendered straight back into its input, and `submit_js` posts
    /// every named field. A pre-pass that refused the mask outright therefore
    /// 400'd the entire form on every save — every other setting on the page
    /// with it — without the operator having typed anything, and with no way
    /// out: the field cannot be left blank either, because for a plain field
    /// blank is a real write that CLEARS it.
    ///
    /// The refusal exists to stop a mask REPLACING a value. A submission equal
    /// to what is already stored replaces nothing, so there is nothing to
    /// refuse.
    #[tokio::test]
    async fn a_plain_field_already_holding_the_mask_does_not_break_the_page() {
        use crate::platform_state::variables::{self, NewVariable};

        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx.fixture())
            .await
            .expect("apply admin migrations");
        ctx.boot_config_service().await;

        for (key, value) in [(APP_NAME_KEY, "MyApp"), ("X__NOTE", MASKED_VALUE)] {
            variables::insert(
                &ctx,
                NewVariable {
                    key: key.to_string(),
                    value: value.to_string(),
                    name: String::new(),
                    description: String::new(),
                    warning: String::new(),
                    sensitive: false,
                    updated_by: String::new(),
                    block: None,
                },
            )
            .await
            .expect("seed the row");
        }

        let allowed = [
            var(APP_NAME_KEY, "App Name", InputType::Text),
            var("X__NOTE", "Note", InputType::Text),
        ];
        // The page really does hand the mask back to the browser: this is what
        // makes the save below the form's own unedited submission, not a
        // hand-built request.
        let sections = [SettingsSection::new("Settings", html! {}, &allowed)];
        let rendered = render_sections(&ctx, &sections)
            .await
            .expect("the current values are readable")
            .into_string();
        assert!(
            rendered.contains(MASKED_VALUE),
            "a plain field's stored value is rendered into its input, mask or not: {rendered}"
        );

        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({
                APP_NAME_KEY: "Renamed",
                "X__NOTE": MASKED_VALUE,
            }),
        )
        .await;

        assert_eq!(
            crate::test_support::output_http_status(out).await,
            200,
            "saving the page unedited must not be refused"
        );
        assert_eq!(
            config::get_default(&ctx, APP_NAME_KEY, "")
                .await
                .expect("config read"),
            "Renamed",
            "and the edit the operator actually made must land"
        );
        assert_eq!(
            config::get_default(&ctx, "X__NOTE", "")
                .await
                .expect("config read"),
            MASKED_VALUE,
            "the untouched row is unchanged",
        );
    }

    /// THIS surface refuses a mask the operator INTRODUCES, even for a plain
    /// field, and that is a deliberate difference from the JSON API.
    ///
    /// `blocks::admin::settings` judges the mask exactly — `"********"` is an
    /// ordinary value for a row nothing masks, and it can see the row's flag to
    /// tell. This module cannot: WRAP denies four of its five callers the admin
    /// `variables` table, and a shared helper has to work for the four. A
    /// pre-pass that guessed would miss an operator-flagged row and half-apply
    /// the save (`an_operator_flagged_var_is_refused_before_anything_is_written`),
    /// so it covers more than the writer does instead — only that can promise
    /// no write has started.
    ///
    /// What it gives up is CHANGING a plain setting to eight asterisks through
    /// a block settings form. Not saving a page that already holds them: a
    /// submission equal to the stored value replaces nothing and is allowed —
    /// see `a_plain_field_already_holding_the_mask_does_not_break_the_page`,
    /// which is the whole page this would otherwise have made unsavable.
    #[tokio::test]
    async fn save_settings_refuses_the_mask_even_for_a_plain_field() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config(APP_NAME_KEY, "MyApp");
        let allowed = [var(APP_NAME_KEY, "App Name", InputType::Text)];

        let out = run_save(
            &ctx,
            &allowed,
            serde_json::json!({APP_NAME_KEY: MASKED_VALUE}),
        )
        .await;
        assert_eq!(crate::test_support::output_http_status(out).await, 400);
        assert_eq!(
            config::get_default(&ctx, APP_NAME_KEY, "")
                .await
                .expect("config read"),
            "MyApp",
        );
    }

    #[tokio::test]
    async fn save_settings_still_clears_a_non_sensitive_field_on_empty_submit() {
        // Non-sensitive fields keep the pre-existing behavior: an empty
        // submit is a real write (clears the stored value), not "unchanged".
        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config(APP_NAME_KEY, "MyApp");
        let allowed = [var(APP_NAME_KEY, "App Name", InputType::Text)];

        let out = run_save(&ctx, &allowed, serde_json::json!({APP_NAME_KEY: ""})).await;
        let body = output_json(out).await;
        assert_eq!(body["message"], "Settings saved");
        assert_eq!(
            config::get_default(&ctx, APP_NAME_KEY, "")
                .await
                .expect("config read"),
            "",
            "a non-sensitive field's empty submit is a real write, unlike a sensitive field's"
        );
    }
}

/// CFG-01 reproduction for the second write surface. Separate module so the
/// boot-seeded config fixture stays out of the tests above.
#[cfg(test)]
mod config_store_reproduction {
    use super::*;
    use crate::test_support::TestContext;

    /// A setting saved through an admin settings form must survive a restart.
    ///
    /// `save_settings` is the write path behind five admin forms (products,
    /// legalpages, userportal, email, auth-ui). It calls `config::set`, which
    /// on native writes the `EnvConfigService`'s in-memory override map and
    /// nothing else — the `variables` table, the only durable store, never
    /// sees the value. The save therefore takes effect immediately and is
    /// gone on the next boot, the exact mirror of the `update_variable`
    /// defect: two write paths, neither syncing to the other.
    ///
    /// The restart is modelled by seeding a second config service from the
    /// same database through the production loader, which is all a fresh
    /// process does.
    #[tokio::test]
    async fn settings_form_save_survives_a_restart() {
        const KEY: &str = crate::config_vars::PRIMARY_COLOR_KEY;

        let mut ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx.fixture())
            .await
            .expect("apply admin migrations");
        ctx.boot_config_service().await;

        let saved = crate::test_support::unique_config_value();
        let allowed = [crate::config_vars::shared_var(KEY)];
        let body =
            serde_json::to_vec(&serde_json::json!({ KEY: saved })).expect("serialize request body");
        let status = crate::test_support::output_status(
            save_settings(
                &ctx,
                &crate::test_support::admin_msg("create", "/b/admin/settings"),
                InputStream::from_bytes(body),
                &allowed,
                "branding",
            )
            .await,
        )
        .await;
        assert_eq!(status, 200, "the settings form reported the save succeeded");

        // Live reads see it, which is why this looks like it works.
        assert_eq!(
            wafer_core::clients::config::get_default(&ctx, KEY, "unset")
                .await
                .expect("config read"),
            saved
        );

        // Restart: a fresh process seeds its config service from the table.
        ctx.boot_config_service().await;

        assert_eq!(
            wafer_core::clients::config::get_default(&ctx, KEY, "unset")
                .await
                .expect("config read"),
            saved,
            "a setting saved through an admin form must outlive the process that saved it"
        );
    }
}
