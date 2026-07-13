use maud::{html, Markup};
use wafer_core::clients::database::{self as db};
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use crate::{
    blocks::admin::{ops, VARIABLES_TABLE as VARIABLES},
    http::err_not_found,
    ui::{self, components, icons},
    util::{parse_form_body, RecordExt},
};

/// Render JUST the variables settings body. The parent `settings_page`
/// handler wraps this in the form-less `tabbed_page` shell — this body's
/// Add-Variable modal renders its own `<form hx-post="/b/admin/variables">`
/// (and the htmx-loaded edit modal its own `<form hx-put=...>`), which is
/// only valid because the shell contributes no outer `<form>` to nest in.
pub async fn settings_body(ctx: &dyn Context, msg: &Message) -> Markup {
    let tab = msg.query("tab");
    let active_tab = if tab == "all" { "all" } else { "blocks" };

    html! {
        div style="margin-bottom:0.75rem" {
            button .btn .btn-primary .btn-sm onclick="openModal('create-var')" {
                (icons::plus()) " Add Variable"
            }
        }

        (components::tab_navigation(vec![
            components::Tab {
                active: active_tab == "blocks",
                href: "/b/admin/settings/variables",
                label: "By Block",
                icon: Some(icons::package()),
            },
            components::Tab {
                active: active_tab == "all",
                href: "/b/admin/settings/variables?tab=all",
                label: "All Variables",
                icon: Some(icons::file_text()),
            },
        ]))

        div #variables-content {
            @if active_tab == "all" {
                (config_all_tab(ctx).await)
            } @else {
                (config_by_block_tab(ctx).await)
            }
        }

        // Create variable modal
        (components::modal("create-var", "Add Variable", html! {
            form hx-post="/b/admin/variables" hx-target="#variables-content" {
                div .form-group {
                    label .form-label .required for="var-key" { "Key" }
                    input .form-input type="text" #var-key name="key" placeholder="e.g. MY_SETTING" required;
                }
                div .form-group {
                    label .form-label for="var-value" { "Value" }
                    input .form-input type="text" #var-value name="value" placeholder="Value";
                }
                div .form-group {
                    label .form-label for="var-desc" { "Description" }
                    input .form-input type="text" #var-desc name="description" placeholder="Optional description";
                }
                div .form-group {
                    label .form-checkbox {
                        input type="checkbox" name="sensitive" value="1";
                        " Sensitive (mask value in UI)"
                    }
                }
                div .form-actions {
                    button .btn .btn-secondary type="button" onclick="closeModal('create-var')" { "Cancel" }
                    button .btn .btn-primary type="submit" { "Create" }
                }
            }
        }))

        // Edit variable modal (content loaded dynamically via htmx)
        div .modal-overlay #edit-var-modal-overlay hidden
            onclick="if(event.target===this)closeModal('edit-var-modal-overlay')"
        {
            div .modal {
                div #edit-var-modal {}
            }
        }
    }
}

/// Full settings page for variables — used by mutation handlers that need to
/// re-render the complete page after a create/update. Delegates to the
/// canonical `settings_page` so both call paths share one composition.
async fn variables_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    super::settings::settings_page(ctx, msg, "variables").await
}

/// How a variable's value cell should render. SEC-060: the masking decision
/// is made once, by `ops::is_sensitive_key`, so every table agrees on it.
enum ValueState {
    /// Sensitive value present — show the mask.
    Masked,
    /// Non-sensitive value present — show it verbatim.
    Plain(String),
    /// No value stored (block-config tables distinguish this from empty).
    NotSet,
}

impl ValueState {
    /// Resolve the value cell from a key + raw value + sensitive flag, applying
    /// the SEC-060 suffix rule via `ops::is_sensitive_key`. `track_unset`
    /// controls whether an empty value renders as `(not set)` (block-config
    /// tables) or as an empty `code` cell (flat DB-record tables).
    fn resolve(key: &str, value: &str, sensitive_flag: i64, track_unset: bool) -> Self {
        let sensitive = ops::is_sensitive_key(key, sensitive_flag);
        if track_unset && value.is_empty() {
            ValueState::NotSet
        } else if sensitive {
            ValueState::Masked
        } else {
            ValueState::Plain(value.to_string())
        }
    }
}

/// The data needed to render one variable table row. Built per-section, then
/// handed to [`var_row`] so the masking/edit-button/warning markup lives in
/// one place.
struct VarRow<'a> {
    key: &'a str,
    /// Friendly name shown under the key (block-config tables only).
    name: Option<&'a str>,
    value: ValueState,
    /// Declared default / auto-generate state (block-config tables only).
    default: Option<&'a str>,
    auto_generate: bool,
    description: &'a str,
    warning: &'a str,
    /// Whether to render the "Default" column cell (block-config tables).
    show_default: bool,
}

/// Render one variable table row: key (+ optional name), value cell (masked
/// per SEC-060), optional default column, description (+ optional warning),
/// and the edit button. Shared by all four variable tables so the masking
/// policy and edit affordance can't drift between them.
fn var_row(row: &VarRow) -> Markup {
    html! {
        tr {
            td .font-medium style="font-size:13px" {
                code { (row.key) }
                @if let Some(name) = row.name {
                    @if !name.is_empty() {
                        br;
                        span .text-muted style="font-size:12px" { (name) }
                    }
                }
            }
            td style="font-size:13px" {
                @match &row.value {
                    ValueState::Masked => code { "********" },
                    ValueState::Plain(v) => code { (v) },
                    ValueState::NotSet => span .text-muted { "(not set)" },
                }
            }
            @if row.show_default {
                td style="font-size:12px" {
                    @match row.default {
                        Some(d) if !d.is_empty() => code .text-muted { (d) },
                        _ => @if row.auto_generate {
                            span .badge .badge-info style="font-size:11px" { "auto-generated" }
                        },
                    }
                }
            }
            td style="font-size:12px" {
                (row.description)
                @if !row.warning.is_empty() {
                    div style="color:#92400e;font-size:11px;margin-top:2px" {
                        "Warning: " (row.warning)
                    }
                }
            }
            td {
                button .btn .btn-sm .btn-ghost
                    hx-get={"/b/admin/variables/" (row.key) "/edit"}
                    hx-target="#edit-var-modal"
                    hx-swap="innerHTML"
                    title="Edit"
                    aria-label=(format!("Edit {}", row.key))
                { (icons::edit()) }
            }
        }
    }
}

/// Build and render one row for a declared [`ConfigVar`] (the shared + per-block
/// tables): pulls the stored value + sensitive flag from `var_map`, falling
/// back to the var's declared sensitivity when no DB row exists, and shows the
/// declared default / auto-generate badge.
fn config_var_row(
    var: &wafer_run::ConfigVar,
    var_map: &std::collections::HashMap<String, (String, i64)>,
) -> Markup {
    let (db_value, sensitive_flag) = var_map
        .get(&var.key)
        .map(|(v, s)| (v.as_str(), *s))
        .unwrap_or(("", var.is_sensitive() as i64));
    var_row(&VarRow {
        key: &var.key,
        name: Some(&var.name),
        value: ValueState::resolve(&var.key, db_value, sensitive_flag, true),
        default: Some(&var.default),
        auto_generate: var.auto_generate,
        description: &var.description,
        warning: &var.warning,
        show_default: true,
    })
}

/// Render a titled card wrapping a variable table. `show_default` adds the
/// "Default" column header to match [`var_row`]'s default cell.
fn var_table(header: Markup, show_default: bool, body: Markup) -> Markup {
    html! {
        div .card .mt-4 {
            (header)
            div .card-body {
                div .table-container {
                    table .table {
                        thead {
                            tr {
                                th { "Key" }
                                th { "Value" }
                                @if show_default { th { "Default" } }
                                th { "Description" }
                                th style="width:50px" {}
                            }
                        }
                        tbody { (body) }
                    }
                }
            }
        }
    }
}

/// "All Variables" tab -- flat table of all config variables from the DB.
async fn config_all_tab(ctx: &dyn Context) -> Markup {
    let settings = db::list_all(ctx, VARIABLES, vec![]).await;

    html! {
        @match &settings {
            Ok(records) => {
                div .table-container {
                    table .table {
                        thead {
                            tr {
                                th { "Key" }
                                th { "Value" }
                                th { "Description" }
                                th { "Actions" }
                            }
                        }
                        tbody {
                            @for record in records {
                                @let key = record.str_field("key");
                                @let value = record.str_field("value");
                                @let description = record.str_field("description");
                                @let warning = record.str_field("warning");
                                // SEC-060: mask via the shared rule, not the
                                // `sensitive` flag alone.
                                @let masked = ops::is_sensitive_key(key, record.i64_field("sensitive"));
                                tr #{"var-row-" (key)} {
                                    td .font-medium { (key) }
                                    td .text-sm {
                                        @if masked {
                                            code { "********" }
                                        } @else {
                                            code { (value) }
                                        }
                                    }
                                    td .text-sm {
                                        @if !description.is_empty() {
                                            span .text-muted { (description) }
                                        }
                                        @if !warning.is_empty() {
                                            div style="color:#92400e;font-size:0.75rem;margin-top:0.25rem" {
                                                "\u{26a0} " (warning)
                                            }
                                        }
                                    }
                                    td {
                                        button .btn .btn-sm .btn-ghost
                                            hx-get={"/b/admin/variables/" (key) "/edit"}
                                            hx-target="#edit-var-modal"
                                            hx-swap="innerHTML"
                                            title="Edit"
                                            aria-label=(format!("Edit {key}"))
                                        { (icons::edit()) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                div .login-error { "Failed to load variables: " (e.message) }
            }
        }
    }
}

/// "By Block" tab -- groups config variables by owning block with WRAP access info.
async fn config_by_block_tab(ctx: &dyn Context) -> Markup {
    let blocks = ctx.registered_blocks();
    let shared_vars = crate::config_vars::shared_config_vars();

    // Load all variables from DB
    let all_vars = db::list_all(ctx, VARIABLES, vec![])
        .await
        .unwrap_or_default();

    // Build a map of key -> (value, sensitive-flag). The flag is kept as the
    // raw `i64` so the SEC-060 suffix rule can be applied via
    // `ops::is_sensitive_key` at render time.
    let var_map: std::collections::HashMap<String, (String, i64)> = all_vars
        .iter()
        .map(|r| {
            let key = r.str_field("key").to_string();
            let value = r.str_field("value").to_string();
            let sensitive = r.i64_field("sensitive");
            (key, (value, sensitive))
        })
        .collect();

    // Collect blocks that have config_keys
    let blocks_with_config: Vec<_> = blocks
        .iter()
        .filter(|b| !b.config_keys.is_empty())
        .collect();

    // Collect all known keys (block-declared + shared) to detect unowned DB vars
    let mut known_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for block in blocks {
        for ck in &block.config_keys {
            known_keys.insert(ck.key.clone());
        }
    }
    for sv in &shared_vars {
        known_keys.insert(sv.key.clone());
    }

    // Precompute grants keyed by exact resource pattern. The per-block render
    // below used to walk `blocks × grants × config_keys` looking for matches —
    // a cubic loop for every page render. We build a single map up front so
    // the inner template just does an O(1) lookup per config key.
    let mut grants_by_resource: std::collections::HashMap<String, Vec<(&str, bool)>> =
        std::collections::HashMap::new();
    for grant_block in blocks {
        for grant in &grant_block.grants {
            grants_by_resource
                .entry(grant.resource.clone())
                .or_default()
                .push((grant.grantee.as_str(), grant.write));
        }
    }

    html! {
        // Shared variables section
        @if !shared_vars.is_empty() {
            (var_table(
                html! {
                    div .card-header {
                        h3 .card-title {
                            span .badge .badge-warning .mr-2 { "shared" }
                            " Shared Platform Config"
                        }
                        p .text-muted style="font-size:12px" {
                            "Any block can read. Only admin can write."
                        }
                    }
                },
                true,
                html! {
                    @for var in &shared_vars {
                        (config_var_row(var, &var_map))
                    }
                },
            ))
        }

        // Per-block sections
        @for block in &blocks_with_config {
            (var_table(
                html! {
                    div .card-header {
                        h3 .card-title {
                            span .badge .badge-info .mr-2 { (block.name) }
                            " Configuration"
                        }
                        // Show WRAP access info for this block's config. The
                        // grants are looked up by exact resource pattern via the
                        // `grants_by_resource` map built above — used to be a
                        // cubic `blocks × grants × config_keys` loop per render.
                        p .text-muted style="font-size:12px" {
                            "Owner: " code { (block.name) }
                            " \u{2014} Admin can read/write all. "
                            @for ck in &block.config_keys {
                                @for resource in [ck.key.clone(), format!("{}*", ck.key)] {
                                    @if let Some(matches) = grants_by_resource.get(&resource) {
                                        @for (grantee, write) in matches {
                                            @if *grantee != block.name {
                                                span .badge .badge-secondary .mr-1 style="font-size:11px" {
                                                    (grantee) ": "
                                                    @if *write { "read+write" } @else { "read" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
                true,
                html! {
                    @for var in &block.config_keys {
                        (config_var_row(var, &var_map))
                    }
                },
            ))
        }

        // Unowned variables section -- keys in DB not declared by any block or shared
        @let unowned_vars: Vec<_> = all_vars.iter()
            .filter(|r| !known_keys.contains(r.str_field("key")))
            .collect();
        @if !unowned_vars.is_empty() {
            (var_table(
                html! {
                    div .card-header {
                        h3 .card-title {
                            span .badge .badge-secondary .mr-2 { "unowned" }
                            " Unowned Variables"
                        }
                        p .text-muted style="font-size:12px" {
                            "Variables in the database not declared by any block. These may be legacy or manually created."
                        }
                    }
                },
                false,
                html! {
                    @for record in &unowned_vars {
                        @let key = record.str_field("key");
                        // SEC-060: mask via the shared rule. `track_unset` is
                        // false here so an empty value renders as an empty
                        // `code` cell, matching the prior flat layout.
                        (var_row(&VarRow {
                            key,
                            name: None,
                            value: ValueState::resolve(
                                key,
                                record.str_field("value"),
                                record.i64_field("sensitive"),
                                false,
                            ),
                            default: None,
                            auto_generate: false,
                            description: record.str_field("description"),
                            warning: "",
                            show_default: false,
                        }))
                    }
                },
            ))
        }
    }
}

/// POST /b/admin/variables -- create a new variable
pub async fn handle_create_variable(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let bytes = input.collect_to_bytes().await;
    let body = parse_form_body(&bytes);

    let key = body.get("key").map(|s| s.as_str()).unwrap_or("");
    let value = body.get("value").map(|s| s.as_str()).unwrap_or("");
    let description = body.get("description").map(|s| s.as_str());
    let sensitive = body.get("sensitive").map(|s| s.as_str()).unwrap_or("0") == "1";

    // Key-required guard, URL/SSRF validation (the SSR path previously had
    // none), audit-log write, and the create live in the shared ops layer.
    if let Err(out) = ops::create_variable(ctx, msg, key, value, None, description, sensitive).await
    {
        return out;
    }

    // Re-render the variables page (htmx will swap #content)
    variables_page(ctx, msg).await
}

/// GET /b/admin/variables/{key}/edit -- return modal edit form content
pub async fn handle_edit_variable_form(
    ctx: &dyn Context,
    _msg: &Message,
    var_key: &str,
) -> OutputStream {
    let Ok(record) = db::get_by_field(
        ctx,
        VARIABLES,
        "key",
        serde_json::Value::String(var_key.to_string()),
    )
    .await
    else {
        return err_not_found("Variable not found");
    };

    let key = record.str_field("key").to_string();
    let sensitive = record.i64_field("sensitive") != 0;
    let value = record.str_field("value").to_string();
    let description = record.str_field("description").to_string();
    let warning = record.str_field("warning").to_string();

    let markup = html! {
        div .modal-header {
            h3 .modal-title { "Edit Variable" }
            button .modal-close onclick="closeModal('edit-var-modal-overlay')" {
                (icons::x())
            }
        }
        div .modal-body {
            form hx-put={"/b/admin/variables/" (key)} hx-target="#content" {
                div .form-group {
                    label .form-label { "Key" }
                    input .form-input type="text" value=(key) disabled;
                }
                div .form-group {
                    label .form-label for="edit-value" { "Value" }
                    @if sensitive {
                        div style="position:relative" {
                            input .form-input #edit-value
                                type="password"
                                name="value"
                                value=(value)
                                style="padding-right:3rem";
                            button .btn .btn-ghost .btn-icon
                                type="button"
                                style="position:absolute;right:0.25rem;top:50%;transform:translateY(-50%)"
                                onclick="var i=document.getElementById('edit-value');if(i.type==='password'){i.type='text';this.title='Hide';this.setAttribute('aria-label','Hide value')}else{i.type='password';this.title='Reveal';this.setAttribute('aria-label','Reveal value')}"
                                title="Reveal"
                                aria-label="Reveal value"
                            { (icons::eye()) }
                        }
                    } @else {
                        input .form-input type="text" #edit-value name="value" value=(value);
                    }
                }
                div .form-group {
                    label .form-label for="edit-desc" { "Description" }
                    input .form-input type="text" #edit-desc name="description" value=(description);
                }
                @if !warning.is_empty() {
                    div style="background:#fef3c7;border:1px solid #f59e0b;border-radius:8px;padding:0.75rem;margin-bottom:1rem;font-size:0.813rem;color:#92400e;display:flex;align-items:center;gap:0.5rem" {
                        "\u{26a0} " (warning)
                    }
                }
                div .form-actions {
                    button .btn .btn-secondary type="button" onclick="closeModal('edit-var-modal-overlay')" { "Cancel" }
                    button .btn .btn-primary type="submit" { "Save" }
                }
            }
        }
        // Auto-open the modal
        script { (maud::PreEscaped("document.getElementById('edit-var-modal-overlay').removeAttribute('hidden');")) }
    };

    ui::html_response(markup)
}

/// PUT /b/admin/variables/{key} -- update variable value
pub async fn handle_update_variable(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
    var_key: &str,
) -> OutputStream {
    let bytes = input.collect_to_bytes().await;
    let body = parse_form_body(&bytes);

    // Sensitive-empty guard, URL/SSRF validation (the SSR path previously had
    // none), audit-log write, and the upsert live in the shared ops layer.
    let update = ops::VariableUpdate {
        value: body.get("value").map(|s| s.as_str()),
        description: body.get("description").map(|s| s.as_str()),
    };
    if let Err(out) = ops::update_variable(ctx, msg, var_key, update).await {
        return out;
    }

    variables_page(ctx, msg).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The icon-only edit button must carry an accessible name derived from
    /// the row key (2026-07-11 review: 49 unlabeled icon buttons on the
    /// Variables page alone).
    #[test]
    fn var_row_edit_button_carries_accessible_name() {
        let s = var_row(&VarRow {
            key: "WAFER_RUN_SHARED__APP_NAME",
            name: None,
            value: ValueState::Plain("Impresspress".to_string()),
            default: None,
            auto_generate: false,
            description: "App name",
            warning: "",
            show_default: false,
        })
        .into_string();
        assert!(
            s.contains(r#"aria-label="Edit WAFER_RUN_SHARED__APP_NAME""#),
            "edit button must expose an aria-label with the row key: {s}"
        );
    }
}
