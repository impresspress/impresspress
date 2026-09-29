use maud::{html, Markup};
use wafer_run::{context::Context, InputStream, Message, OutputStream, WaferError};

use crate::{
    blocks::{admin::ops, crud},
    http::err_not_found,
    // `key_can_be_seeded_from_env` lives in `platform_state::variables` because
    // it mirrors the two gates between the process environment and that table —
    // `impresspress_server::filter_to_declared_keys` and `seed_and_load`'s own
    // runtime-owned refusal — and because the bulk release's selection applies
    // it too, so the page and the action cannot disagree about which keys the
    // environment can set.
    platform_state::variables::{self, key_can_be_seeded_from_env},
    ui::{
        self,
        components::{self, Badge, BadgeVariant},
        icons,
    },
    util::parse_form_body,
};

/// Render JUST the variables settings body. The parent `settings_page`
/// handler wraps this in the form-less `tabbed_page` shell — this body's
/// Add-Variable modal renders its own `<form hx-post="/b/admin/variables">`
/// (and the htmx-loaded edit modal its own `<form hx-put=...>`), which is
/// only valid because the shell contributes no outer `<form>` to nest in.
///
/// `Err` when the variables table could not be listed: the caller answers
/// it, never a table drawn without the rows.
pub async fn settings_body(ctx: &dyn Context, msg: &Message) -> Result<Markup, WaferError> {
    let tab = msg.query("tab");
    let active_tab = if tab == "all" { "all" } else { "blocks" };
    // ONE read of the table and one of the process-environment marker per
    // render, here rather than inside each consumer. Both tabs and the bulk
    // control's count want the same rows, and the tabs used to take them
    // separately; a third read for the count would have made the page's answer
    // to "how many keys are pinned" depend on which snapshot you asked.
    let rows = variables::list_all(ctx).await?;
    let offer_reset = variables::deployment_seeds_from_process_env(ctx).await?;
    let upgrade_pins = bulk_release_count(&rows, offer_reset);

    Ok(html! {
        div .mb-3 .flex .gap-1 {
            button .btn .btn--primary .btn--sm data-action="modal-open" data-modal-target="create-var" {
                (icons::plus()) " Add Variable"
            }
            @if upgrade_pins > 0 {
                (reset_pinned_at_upgrade_button(upgrade_pins))
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
                (config_all_tab(&rows, offer_reset))
            } @else {
                (config_by_block_tab(ctx, &rows, offer_reset))
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
                        // Hidden first, checkbox second: `parse_form_body` keeps
                        // the last value for a repeated key, so a checked box
                        // posts `1` and an unchecked one still posts an explicit
                        // `0` rather than nothing. Checked by default — masking
                        // is the safe side to be wrong on.
                        input type="hidden" name="sensitive" value="0";
                        input type="checkbox" name="sensitive" value="1" checked;
                        " Sensitive (mask value in UI)"
                    }
                }
                div .form-actions {
                    button .btn .btn--secondary type="button" data-action="modal-close" data-modal-target="create-var" { "Cancel" }
                    button .btn .btn--primary type="submit" { "Create" }
                }
            }
        }))

        // Edit variable modal (content loaded dynamically via htmx)
        div .modal-overlay #edit-var-modal-overlay hidden data-modal-dismiss
        {
            div .modal {
                div #edit-var-modal {}
            }
        }
    })
}

/// Full settings page for variables — used by mutation handlers that need to
/// re-render the complete page after a create/update that landed (`done`).
/// Delegates to the canonical settings page so both call paths share one
/// composition; see [`super::settings::settings_page_after_write`] for what a
/// failed re-read answers.
async fn variables_page(ctx: &dyn Context, msg: &Message, done: &str) -> OutputStream {
    super::settings::settings_page_after_write(ctx, msg, "variables", done).await
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
    /// the SEC-060 key rule (suffix OR declaration) via
    /// `ops::is_sensitive_key`. `track_unset`
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
    /// Whether to render the declared default beneath the value
    /// (block-config tables).
    show_default: bool,
    /// Whether this row offers the delete control.
    ///
    /// Explicit rather than derived from [`ValueState`], because only the
    /// caller knows whether a stored row exists: an unowned variable always
    /// has one, a declared `ConfigVar` only when someone has overridden it,
    /// and a declared var with no override has nothing to delete. Shared
    /// (`WAFER_RUN_SHARED__*`) keys are never deletable —
    /// `ops::delete_variable` refuses them, so a button there would only ever
    /// produce an error.
    deletable: bool,
    /// Why this row outranks the process environment, when it does.
    ///
    /// Rendered as a badge whatever the target, because a pin is a real
    /// property of the row: it is what `variables::set_by_admin` records and
    /// what the boot log reports.
    pin: Option<variables::Pin>,
    /// Whether THIS DEPLOYMENT has a process environment to hand a key back to.
    ///
    /// Separate from [`Self::pin`], and per-render rather than per-row, because
    /// the two answer different questions: the pin says the row is claimed, this
    /// says whether un-claiming it means anything here. Cloudflare never runs
    /// `variables::seed_and_load` and the browser runs it with an empty batch,
    /// so on those targets the control's own toast — "replaced on the next
    /// restart" — would be false.
    offer_reset: bool,
}

/// Build one variable table row's cells, in column order against
/// [`VAR_COLUMNS`]: the variable (key, friendly name, description, pin badge
/// and warning), its value (masked per SEC-060, with the declared default
/// beneath it in the block-config tables), and the controls. Shared by every
/// variable table so the masking policy and edit affordance can't drift
/// between them. The `<td>`s around these belong to `components::data_table`.
///
/// Three columns, not one per field: these tables sit beside the settings
/// navigation, about 610px wide at a 1280px window, and five columns there
/// forced the description into a sliver (rows hundreds of pixels tall) and
/// pushed the controls out of the card. The description reads as part of the
/// variable it describes, and the default as part of the value.
fn var_row(row: &VarRow) -> Vec<Markup> {
    vec![
        variable_cell(row.key, row.name, row.description, row.pin, row.warning),
        html! {
            div .var-cell {
                span .text-13 .cell-wrap {
                    @match &row.value {
                        ValueState::Masked => code { "********" },
                        ValueState::Plain(v) => code { (v) },
                        ValueState::NotSet => span .text-muted { "(not set)" },
                    }
                }
                @if row.show_default {
                    @match row.default {
                        Some(d) if !d.is_empty() => span .var-cell__note .cell-wrap {
                            "Default: " code { (d) }
                        },
                        _ => @if row.auto_generate {
                            span .var-cell__note {
                                (Badge::new(BadgeVariant::Info).classes("text-11").render(html! { "auto-generated" }))
                            }
                        },
                    }
                }
            }
        },
        // All three controls share the final cell: the cells are columns
        // against `VAR_COLUMNS`, so a conditional extra cell would misalign
        // every row that has no delete control against every row that does.
        html! {
            div .flex .gap-1 {
                button .btn .btn--sm .btn--ghost
                    hx-get={"/b/admin/variables/" (row.key) "/edit"}
                    hx-target="#edit-var-modal"
                    hx-swap="innerHTML"
                    title="Edit"
                    aria-label=(format!("Edit {}", row.key))
                { (icons::edit()) }
                @if row.pin.is_some() && row.offer_reset && key_can_be_seeded_from_env(row.key) {
                    (reset_to_environment_button(row.key))
                }
                @if row.deletable {
                    (delete_button(row.key))
                }
            }
        },
    ]
}

/// The first cell of every variable table: the key, then what an operator
/// reads to recognise it — the declared friendly name, the description, the
/// badge saying why the row outranks the environment, and any warning.
fn variable_cell(
    key: &str,
    name: Option<&str>,
    description: &str,
    pin: Option<variables::Pin>,
    warning: &str,
) -> Markup {
    html! {
        div .var-cell {
            code .var-cell__key { (components::breakable_id(key)) }
            @if let Some(name) = name.filter(|name| !name.is_empty()) {
                span .var-cell__name { (name) }
            }
            @if !description.is_empty() {
                span .var-cell__note { (description) }
            }
            @if let Some(pin) = pin {
                span .var-cell__note { (pin_badge(pin)) }
            }
            @if !warning.is_empty() {
                span .var-warning-note { "Warning: " (warning) }
            }
        }
    }
}

/// What the per-block tables need from a stored row: the columns they render
/// that the `ConfigVar` declaration cannot supply.
///
/// A named struct rather than a wider tuple because the third member is not
/// obvious from its type — `Option<Pin>` beside a `String` and an `i64` reads
/// as nothing in particular at the call site, while `pin` reads as itself.
struct StoredVar {
    value: String,
    /// Kept as the `i64` `ops::is_sensitive_key` takes, so the SEC-060 key rule
    /// — the `_SECRET`/`_KEY` suffix or the key's own declaration — is applied
    /// at render time rather than trusted from the column.
    sensitive_flag: i64,
    pin: Option<variables::Pin>,
}

/// Build and render one row for a declared [`ConfigVar`] (the shared + per-block
/// tables): pulls the stored value + sensitive flag from `var_map`, falling
/// back to the var's declared sensitivity when no DB row exists, and shows the
/// declared default / auto-generate badge.
fn config_var_row(
    var: &wafer_run::ConfigVar,
    var_map: &std::collections::HashMap<String, StoredVar>,
    offer_reset: bool,
) -> Vec<Markup> {
    let stored = var_map.get(&var.key);
    let (db_value, sensitive_flag) = stored
        .map(|s| (s.value.as_str(), s.sensitive_flag))
        .unwrap_or(("", var.is_sensitive() as i64));
    var_row(&VarRow {
        pin: stored.and_then(|s| s.pin),
        offer_reset,
        key: &var.key,
        name: Some(&var.name),
        value: ValueState::resolve(&var.key, db_value, sensitive_flag, true),
        default: Some(&var.default),
        auto_generate: var.auto_generate,
        description: &var.description,
        warning: &var.warning,
        show_default: true,
        // Never, in the per-block tables. A row here exists because a block
        // DECLARES the key, not because the database does — so removing the
        // stored override must leave the row in place showing its default,
        // and this control's `outerHTML` swap would instead delete the row
        // from the table, stranding the declared key with nothing to edit
        // until a reload. "Reset to default" is a different affordance and
        // wants its own handler; the flat and unowned tables are where the
        // rows that can really be removed live.
        deletable: false,
    })
}

/// Render a titled card wrapping a variable table. `header` is the card's
/// `.card__head` content: the title, and the access line beneath it.
fn var_table(header: Markup, rows: Vec<Vec<Markup>>) -> Markup {
    html! {
        section .card .mt-4 {
            header .card__head { (header) }
            div .card__body {
                (components::data_table::<fn(usize) -> Option<String>>(
                    &VAR_COLUMNS,
                    rows,
                    None,
                    html! {},
                ))
            }
        }
    }
}

/// The variable tables' columns, the three [`var_row`] emits. The last column
/// only carries the controls; it keeps the 50px width the old `th .w-50` gave
/// it.
const VAR_COLUMNS: [components::TableCol<'static>; 3] = [
    components::TableCol {
        label: "Variable",
        width: None,
    },
    components::TableCol {
        label: "Value",
        width: None,
    },
    components::TableCol {
        label: "",
        width: Some("50px"),
    },
];

/// The "All Variables" tab's columns — the same shape as [`VAR_COLUMNS`], with
/// an explicitly labelled actions column.
const ALL_VAR_COLUMNS: [components::TableCol<'static>; 3] = [
    components::TableCol {
        label: "Variable",
        width: None,
    },
    components::TableCol {
        label: "Value",
        width: None,
    },
    components::TableCol {
        label: "Actions",
        width: None,
    },
];

/// The delete control, shared by every table that offers one so the affordance
/// and the confirm text cannot drift between them.
///
/// `closest tr` rather than a row id: these tables render through
/// `components::TableRow`, and only the flat "All Variables" tab gives its
/// rows ids. An empty response body is what removes the row.
fn delete_button(key: &str) -> Markup {
    html! {
        button .btn .btn--sm .btn--danger
            hx-delete={"/b/admin/variables/" (key)}
            hx-target="closest tr"
            hx-swap="outerHTML"
            hx-confirm={"Delete " (key) "? This cannot be undone."}
            title="Delete"
            aria-label=(format!("Delete {key}"))
        { (icons::trash()) }
    }
}

/// The control that hands one key back to the process environment, shared by
/// every table that offers one so the affordance and its confirm text cannot
/// drift between them.
///
/// The UI half of the recovery route [`variables::seed_and_load`]'s boot WARN
/// names. It is the ONLY route out of a pinned key: `ops::delete_variable`
/// refuses every declared `WAFER_RUN_SHARED__*` row, and `ops::update_variable`
/// re-stamps ownership on every write, so clearing the value would re-pin the
/// row it was meant to release.
///
/// `hx-swap="none"`, unlike [`delete_button`]'s `closest tr` / `outerHTML`:
/// nothing is removed and no row's identity changes, so the response body is
/// empty and the toast its `HX-Trigger` carries is the whole result. The pin
/// badge beside it goes stale until the next render, which is the honest cost
/// of not re-rendering a table from a row control — the change this reports
/// does not take effect until a restart either.
fn reset_to_environment_button(key: &str) -> Markup {
    html! {
        button .btn .btn--sm .btn--ghost type="button"
            hx-post={"/b/admin/variables/" (key) "/reset-to-environment"}
            hx-swap="none"
            hx-confirm={
                "Hand " (key) " back to the environment? The stored value stops taking \
                 precedence, and the next restart seeds this key from the process \
                 environment again."
            }
            data-error-label="Could not hand this variable back to the environment"
            title="Reset to environment"
            aria-label=(format!("Reset {key} to environment"))
        { (icons::refresh_cw()) }
    }
}

/// How many keys the bulk release would act on, or `0` when the page must not
/// offer it at all.
///
/// Two independent conditions, the same pair the per-row control is gated on
/// and for the same reasons:
///
/// - PER DEPLOYMENT, [`variables::deployment_seeds_from_process_env`], which
///   arrives here as `offer_reset`: Cloudflare never runs
///   `variables::seed_and_load` and the browser runs it with an empty batch, so
///   nothing there is pinned against a process environment and the action's
///   toast would be false.
/// - PER KEY, `key_can_be_seeded_from_env`, which
///   [`variables::count_pinned_at_upgrade`] applies for itself.
///
/// Counted over the rows [`settings_body`] already holds rather than from a
/// read of its own, and through the same selection the action uses. So the
/// number on the button is the number of rows below it carrying a "Pinned at
/// upgrade" badge and their own reset control — by construction, not by two
/// filters that happen to agree, and from the same snapshot, not from a second
/// read that could have moved.
///
/// An unreadable table means no count at all: [`settings_body`] does not call
/// this, and the control does not render. That is the honest answer — both
/// tabs report the failure in place of their tables, and a bulk button drawn
/// from a count nobody could take would offer work it has no evidence exists.
fn bulk_release_count(rows: &[variables::VariableRow], offer_reset: bool) -> usize {
    if !offer_reset {
        return 0;
    }
    variables::count_pinned_at_upgrade(rows)
}

/// The control that hands every key pinned at upgrade back to the process
/// environment at once.
///
/// The per-key control is the correctness fix; this is the one that matches
/// what the upgrade boot actually does. The transition pins precisely the keys
/// whose stored value disagreed with an export — the keys the operator had
/// configured — so "several" is the ordinary case, and an operator who decides
/// the environment was right all along faced one confirm dialog per key before
/// a single restart.
///
/// It names the count rather than the keys: the keys are listed on this page,
/// each carrying the "Pinned at upgrade" badge, and a confirm dialog carrying
/// ten `WAFER_RUN_SHARED__*` names is one nobody reads. The confirm text says
/// what it will NOT touch instead, because that is the question a bulk action
/// over configuration has to answer before it is pressed.
///
/// `hx-swap="none"` for the reason [`reset_to_environment_button`] gives:
/// nothing is removed and no row's identity changes, so the toast its
/// `HX-Trigger` carries is the whole result, and the pin badges go stale until
/// the next render — the change does not take effect until a restart either.
fn reset_pinned_at_upgrade_button(count: usize) -> Markup {
    let keys = pluralize_keys(count);
    html! {
        button .btn .btn--secondary .btn--sm type="button"
            hx-post="/b/admin/variables/reset-pinned-at-upgrade"
            hx-swap="none"
            hx-confirm={
                "Hand " (keys) " pinned at upgrade back to the environment? Their stored \
                 values stop taking precedence, and the next restart seeds those keys from \
                 the process environment again. Keys an admin edited here are not affected."
            }
            data-error-label="Could not hand the keys pinned at upgrade back to the environment"
            title="Reset every key the upgrade boot pinned"
        { (icons::refresh_cw()) " Reset all keys pinned at upgrade (" (count) ")" }
    }
}

/// `"1 key"` / `"4 keys"`, so the confirm dialog and the toast read as English
/// on the single-key case the bulk control still renders for.
fn pluralize_keys(count: usize) -> String {
    if count == 1 {
        "1 key".to_string()
    } else {
        format!("{count} keys")
    }
}

/// The badge that says WHY a row outranks the process environment.
///
/// Two wordings, because they are two different claims and only one of them is
/// about a person: an admin edit is a decision somebody made and this build
/// recorded, while an upgrade pin is this build saying it cannot tell. Calling
/// the second one an admin edit would be the same class of untruth the boot
/// WARN exists to avoid.
fn pin_badge(pin: variables::Pin) -> Markup {
    let (label, title) = match pin {
        variables::Pin::AdminEdit => (
            "Edited here",
            "An admin edited this through the admin UI, so the process environment no \
             longer sets it.",
        ),
        variables::Pin::PreUpgrade => (
            "Pinned at upgrade",
            "Kept when this deployment upgraded: the stored value predates edit tracking \
             and differed from the environment, so no admin edit can be proved either way.",
        ),
    };
    html! {
        span title=(title) {
            (Badge::new(BadgeVariant::Secondary).classes("text-11").render(html! { (label) }))
        }
    }
}

/// Whether the page offers a delete control for `key`, given the set of
/// shared vars this build still declares.
///
/// Mirrors `ops::delete_variable`'s refusals exactly, so the page never
/// renders a button that could only produce an error: the JWT signing secret
/// is never deletable, and a shared var is deletable only once it is no longer
/// declared (nothing re-seeds a stale row).
fn key_is_deletable(key: &str, declared_shared: &std::collections::HashSet<String>) -> bool {
    key != crate::blocks::auth::JWT_SECRET_KEY && !declared_shared.contains(key)
}

/// The shared keys this build declares, for [`key_is_deletable`]. Built once
/// per render rather than per row — `shared_config_vars()` allocates.
fn declared_shared_keys() -> std::collections::HashSet<String> {
    crate::config_vars::shared_config_vars()
        .into_iter()
        .map(|v| v.key)
        .collect()
}

/// "All Variables" tab -- flat table of all config variables from the DB.
///
/// The rows and `offer_reset` are the caller's — see [`settings_body`], which
/// takes each exactly once for the whole page.
fn config_all_tab(rows: &[variables::VariableRow], offer_reset: bool) -> Markup {
    let declared_shared = declared_shared_keys();

    html! {
        @let table_rows: Vec<components::TableRow> = rows.iter().map(|row| {
            let key = row.key.as_str();
            let description = row.description.as_str();
            let warning = row.warning.as_str();
            // SEC-060: mask via the shared rule, not the `sensitive`
            // flag alone.
            let masked = ops::is_sensitive_key(key, i64::from(row.sensitive));
            components::TableRow::new(vec![
                variable_cell(key, None, description, variables::pin_of(row), warning),
                html! {
                    span .text-13 .cell-wrap {
                        @if masked {
                            code { "********" }
                        } @else {
                            code { (row.value) }
                        }
                    }
                },
                html! {
                    div .flex .gap-1 {
                        button .btn .btn--sm .btn--ghost
                            hx-get={"/b/admin/variables/" (key) "/edit"}
                            hx-target="#edit-var-modal"
                            hx-swap="innerHTML"
                            title="Edit"
                            aria-label=(format!("Edit {key}"))
                        { (icons::edit()) }
                        // Same reasoning as the delete control below:
                        // the flat listing is where an operator sent
                        // here by a boot WARN naming one key actually
                        // looks for it, so it must offer what the By
                        // Block tables offer.
                        @if offer_reset
                            && variables::pin_of(row).is_some()
                            && key_can_be_seeded_from_env(key)
                        {
                            (reset_to_environment_button(key))
                        }
                        // The flat listing offers the same control as
                        // the Unowned table: this is where an operator
                        // scanning for a legacy key actually looks, and
                        // two tabs disagreeing about whether a row can
                        // be removed is its own defect.
                        @if key_is_deletable(key, &declared_shared) {
                            (delete_button(key))
                        }
                    }
                },
            ])
            .id(format!("var-row-{key}"))
        }).collect();

        (components::DataTable::new(&ALL_VAR_COLUMNS)
            .rows(table_rows)
            .empty(html! { p .text-center .text-muted { "No variables are set." } })
            .render())
    }
}

/// "By Block" tab -- groups config variables by owning block with WRAP access info.
///
/// `all_vars` and `offer_reset` are the caller's — see [`settings_body`]. The
/// rows arrive already read: a failed read never reaches this tab, because
/// [`settings_body`] answers it instead. It cannot fall back to the declared
/// vars and their defaults: a declared var reads as "at its default, not
/// pinned" here, so a failed read would tell the operator every value they
/// set is gone.
fn config_by_block_tab(
    ctx: &dyn Context,
    all_vars: &[variables::VariableRow],
    offer_reset: bool,
) -> Markup {
    let blocks = ctx.registered_blocks();
    let shared_vars = crate::config_vars::shared_config_vars();

    let var_map: std::collections::HashMap<String, StoredVar> = all_vars
        .iter()
        .map(|row| {
            (
                row.key.clone(),
                StoredVar {
                    value: row.value.clone(),
                    sensitive_flag: i64::from(row.sensitive),
                    pin: variables::pin_of(row),
                },
            )
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
    let mut grants_by_resource: std::collections::HashMap<
        String,
        Vec<(&str, wafer_block::GrantWrite)>,
    > = std::collections::HashMap::new();
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
                    div {
                        h3 .card__title {
                            (Badge::new(BadgeVariant::Warning).classes("mr-2").render(html! { "shared" }))
                            " Shared Platform Config"
                        }
                        p .card__subtitle {
                            "Any block can read. Only admin can write."
                        }
                    }
                },
                shared_vars.iter().map(|var| config_var_row(var, &var_map, offer_reset)).collect(),
            ))
        }

        // Per-block sections
        @for block in &blocks_with_config {
            (var_table(
                html! {
                    div {
                        h3 .card__title {
                            (Badge::new(BadgeVariant::Info).classes("mr-2").render(html! { (block.name) }))
                            " Configuration"
                        }
                        // Show WRAP access info for this block's config. The
                        // grants are looked up by exact resource pattern via the
                        // `grants_by_resource` map built above — used to be a
                        // cubic `blocks × grants × config_keys` loop per render.
                        p .card__subtitle {
                            "Owner: " code { (block.name) }
                            " \u{2014} Admin can read/write all. "
                            @for ck in &block.config_keys {
                                @for resource in [ck.key.clone(), format!("{}*", ck.key)] {
                                    @if let Some(matches) = grants_by_resource.get(&resource) {
                                        @for (grantee, write) in matches {
                                            @if *grantee != block.name {
                                                (Badge::new(BadgeVariant::Secondary).classes("mr-1 text-11").render(html! {
                                                    (grantee) ": "
                                                    (match write {
                                                        wafer_block::GrantWrite::Full => "read+write",
                                                        wafer_block::GrantWrite::Append => "append",
                                                        wafer_block::GrantWrite::None => "read",
                                                    })
                                                }))
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
                block.config_keys.iter().map(|var| config_var_row(var, &var_map, offer_reset)).collect(),
            ))
        }

        // Unowned variables section -- keys in DB not declared by any block or shared
        @let unowned_vars: Vec<_> = all_vars.iter()
            .filter(|row| !known_keys.contains(row.key.as_str()))
            .collect();
        @if !unowned_vars.is_empty() {
            (var_table(
                html! {
                    div {
                        h3 .card__title {
                            (Badge::new(BadgeVariant::Secondary).classes("mr-2").render(html! { "unowned" }))
                            " Unowned Variables"
                        }
                        p .card__subtitle {
                            "Variables in the database not declared by any block. These may be legacy or manually created."
                        }
                    }
                },
                unowned_vars.iter().map(|row| {
                    let key = row.key.as_str();
                    // SEC-060: mask via the shared rule. `track_unset` is
                    // false here so an empty value renders as an empty
                    // `code` cell, matching the prior flat layout.
                    var_row(&VarRow {
                        key,
                        name: None,
                        value: ValueState::resolve(
                            key,
                            &row.value,
                            i64::from(row.sensitive),
                            false,
                        ),
                        default: None,
                        auto_generate: false,
                        description: &row.description,
                        warning: "",
                        show_default: false,
                        // Every row here exists in the database by definition
                        // — that is what "unowned" means — so these are the
                        // rows an operator needs to be able to remove.
                        //
                        // Only the JWT secret is excluded. A declared shared
                        // var cannot reach this table at all (`known_keys`
                        // covers block-declared AND shared keys, and this
                        // table is what is left over), so a
                        // `WAFER_RUN_SHARED__*` row appearing here is stale by
                        // construction and removable — which is the point.
                        deletable: key != crate::blocks::auth::JWT_SECRET_KEY,
                        pin: variables::pin_of(row),
                        offer_reset,
                    })
                }).collect(),
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
    let bytes = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body = parse_form_body(&bytes);

    let key = body.get("key").map(|s| s.as_str()).unwrap_or("");
    let value = body.get("value").map(|s| s.as_str()).unwrap_or("");
    let description = body.get("description").map(|s| s.as_str());
    // Absent means sensitive, same as the JSON API. The modal always posts an
    // explicit value (a hidden `0` that a checked box overrides with `1`), so
    // "absent" here is a post that bypassed the form, and it fails safe.
    let sensitive = body
        .get("sensitive")
        .map(|value| crate::config_vars::is_truthy(value))
        .unwrap_or(true);

    // Key-required guard, URL/SSRF validation (the SSR path previously had
    // none), audit-log write, and the create live in the shared ops layer.
    if let Err(out) = ops::create_variable(ctx, msg, key, value, None, description, sensitive).await
    {
        return out;
    }

    // Re-render the variables page (htmx will swap #content)
    variables_page(ctx, msg, "Variable created").await
}

/// `GET /b/admin/variables/{key}/edit` -- return modal edit form content.
/// `{key}` is read only as the route table bound it.
pub async fn handle_edit_variable_form(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let var_key = msg.var("key");
    let row = match variables::get_by_key(ctx, var_key).await {
        Ok(Some(row)) => row,
        Ok(None) => return err_not_found("Variable not found"),
        Err(e) => return crud::db_error_internal(e, "Database error"),
    };

    let key = row.key;
    let sensitive = row.sensitive;
    let value = row.value;
    let description = row.description;
    let warning = row.warning;
    // A key the storage rule requires cannot be unflagged; show the control as
    // set-and-locked rather than offering a change that would be refused.
    let required_sensitive = crate::config_vars::is_sensitive_for_storage(&key);
    // Mask on the EFFECTIVE flag, not the stored one. A legacy row an older
    // build stored unflagged for a required key — the row
    // `repair_sensitive_flags` exists for, still unrepaired on a deployment
    // that has not rebooted, which on Cloudflare means no `/_deploy/init`
    // since the upgrade — would otherwise render its secret in a plain text
    // input under a hint saying the variable is always sensitive.
    //
    // Spelled as `is_sensitive_key` rather than re-derived, because
    // `handle_update_variable` has to ask the SAME question to read this
    // widget's answer back, and the tables on the page behind the modal ask it
    // too. One predicate, one union.
    let show_sensitive = ops::is_sensitive_key(&key, i64::from(sensitive));
    // Presence only, never the content: enough for the placeholder to tell a
    // configured secret from an unset one without publishing either.
    let masked_placeholder = if value.is_empty() {
        "Not configured".to_string()
    } else {
        format!("{} (set)", ops::MASKED_VALUE)
    };

    let markup = html! {
        div .modal-header {
            h3 .modal-title { "Edit Variable" }
            button .modal-close data-action="modal-close" data-modal-target="edit-var-modal-overlay" {
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
                    @if show_sensitive {
                        // SEC-060: the stored secret must not reach the DOM.
                        // This rendered `value=(value)` inside the password
                        // input, which is the pattern
                        // `ui::settings_form::render_field` refuses and says
                        // why: `type="password"` masks the glyphs, not the
                        // bytes, so the secret was plain in page source, in
                        // devtools and in the response body — one `hx-get` away
                        // from tables that all mask it.
                        //
                        // Blank instead, with the placeholder carrying the only
                        // thing an operator needs that the value itself was
                        // carrying: whether one is set. `handle_update_variable`
                        // reads an empty masked field back as "not supplied", so
                        // saving the form without retyping the secret keeps it
                        // and lands the rest of the edit, and typing a new one
                        // still rotates it. The reveal toggle stays: it now
                        // shows what the operator is TYPING, exactly as it does
                        // on the shared settings form, which renders this same
                        // blank-field-plus-eye pair.
                        div .value-reveal-wrapper {
                            input .form-input #edit-value
                                type="password"
                                name="value"
                                value=""
                                placeholder=(masked_placeholder);
                            button .btn .btn--ghost .btn--icon .btn-icon-right
                                type="button"
                                data-action="reveal-toggle"
                                data-reveal-target="edit-value"
                                data-reveal-show="Reveal"
                                data-reveal-hide="Hide"
                                title="Reveal"
                                aria-label="Reveal value"
                            { (icons::eye()) }
                        }
                        p .form-hint {
                            "Leave blank to keep the stored value. Typing a new one replaces it."
                        }
                    } @else {
                        input .form-input type="text" #edit-value name="value" value=(value);
                    }
                }
                div .form-group {
                    label .form-label for="edit-desc" { "Description" }
                    input .form-input type="text" #edit-desc name="description" value=(description);
                }
                div .form-group {
                    label .form-checkbox {
                        // A hidden field carries the answer and the checkbox
                        // overrides it, exactly as the Add Variable modal does:
                        // `parse_form_body` keeps the LAST value for a repeated
                        // key, so `sensitive` is always posted and the handler
                        // never has to guess whether the field was on the form.
                        //
                        // A DISABLED checkbox is not serialized — not by
                        // `FormData`, not by htmx's `shouldInclude` — so for a
                        // required key the hidden field must already say `1`.
                        // It said `0` here, under a separate presence marker,
                        // which made every required-sensitive variable
                        // uneditable: the form posted "not sensitive", the ops
                        // guard refused the unflag, and the admin's value or
                        // description edit was dropped with a 400. That hit
                        // `..._OAUTH_GOOGLE_CLIENT_SECRET`,
                        // `..._BOOTSTRAP_ADMIN_PASSWORD` and
                        // `WAFER_RUN__AUTH__JWT_SECRET` — the last of which is
                        // deliberately left rotatable by
                        // `reject_runtime_owned_key`.
                        @if required_sensitive {
                            input type="hidden" name="sensitive" value="1";
                            input type="checkbox" name="sensitive" value="1" checked disabled;
                        } @else {
                            input type="hidden" name="sensitive" value="0";
                            @if show_sensitive {
                                input type="checkbox" name="sensitive" value="1" checked;
                            } @else {
                                input type="checkbox" name="sensitive" value="1";
                            }
                        }
                        span { "Sensitive — mask this value in listings and keep it out of exports" }
                    }
                    @if required_sensitive {
                        p .form-hint {
                            "This variable is always sensitive: its declaration, or its \
                             _SECRET/_KEY name, requires it."
                        }
                    }
                }
                @if !warning.is_empty() {
                    div .var-warning-banner {
                        (ui::icons::triangle_alert()) (warning)
                    }
                }
                div .form-actions {
                    button .btn .btn--secondary type="button" data-action="modal-close" data-modal-target="edit-var-modal-overlay" { "Cancel" }
                    button .btn .btn--primary type="submit" { "Save" }
                }
            }
        }
    };

    ui::html_response_opening_modal(markup, "edit-var-modal-overlay")
}

/// `PUT`/`PATCH /b/admin/variables/{key}` -- update variable value (the row
/// is declared `PATCH`; the edit form sends `PUT`, which maps to the same
/// `update` action). `{key}` is read only as the route table bound it.
pub async fn handle_update_variable(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let var_key = msg.var("key");
    let bytes = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body = parse_form_body(&bytes);

    // What `handle_edit_variable_form` rendered for this key: a masked field is
    // rendered BLANK (SEC-060), so an empty `value` coming back from it is the
    // browser saying "I did not touch this", not "clear it". Dropping the field
    // is how that reaches `ops::update_variable` as the absence it is —
    // otherwise the sensitive-empty guard refuses the request and the admin's
    // description or flag edit dies with a 400, which is the same shape as the
    // `disabled`-checkbox bug: a form posting something the server cannot read
    // as "leave this alone".
    //
    // The stored flag is READ rather than derived from the key, because
    // `show_sensitive` is the `is_sensitive_key` union and an ad hoc row an
    // operator flagged sensitive is masked by it while the key alone says
    // nothing. Reading the widget back off a narrower rule than the one that
    // rendered it is exactly how the two drift.
    //
    // Through `crud::db_error_internal` rather than `err_internal`, for the
    // reason `ops::stored_sensitive_flag`'s sibling read gives: this read names
    // its own table, so a `NotFound` really is a 500 — but a WRAP refusal is a
    // 403 and a quota a 429, and flattening those into "Internal server error"
    // is the drift `tests/error_door.rs` exists to stop.
    let stored_flag = match variables::get_by_key(ctx, var_key).await {
        Ok(Some(row)) => i64::from(row.sensitive),
        Ok(None) => 0,
        Err(e) => return crate::blocks::crud::db_error_internal(e, "Database error"),
    };
    let field_was_masked = ops::is_sensitive_key(var_key, stored_flag);

    // Sensitive-empty guard, the masked-round-trip refusal, URL/SSRF validation
    // (the SSR path previously had none), audit-log write, and the upsert live
    // in the shared ops layer. A literal `MASKED_VALUE` is deliberately NOT
    // dropped here: this form never renders it, so one arriving was typed, and
    // the operator gets the same refusal the JSON API gives rather than a
    // silent no-op reported as a save.
    let update = ops::VariableUpdate {
        value: body
            .get("value")
            .map(|s| s.as_str())
            .filter(|value| !(field_was_masked && value.is_empty())),
        description: body.get("description").map(|s| s.as_str()),
        // Present whenever the surface offers the control — the edit modal
        // always posts it, hidden field plus checkbox. Absent means the caller
        // is not editing the flag, and the stored one is left alone.
        sensitive: body
            .get("sensitive")
            .map(|value| crate::config_vars::is_truthy(value)),
    };
    if let Err(out) = ops::update_variable(ctx, msg, var_key, update).await {
        return out;
    }

    variables_page(ctx, msg, "Variable updated").await
}

/// `POST /b/admin/variables/{key}/reset-to-environment` — the Variables page's
/// row control for handing a key back to the process environment.
///
/// The UI half of the recovery route the boot WARN names. Neither of the
/// controls already on this page can do it: delete refuses a declared shared
/// var, and an edit re-stamps ownership.
pub async fn handle_reset_variable_to_environment(
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let key = msg.var("key");
    if let Err(out) = ops::reset_variable_to_environment(ctx, msg, key).await {
        return out;
    }
    // The stored value is deliberately left in place; only the next boot
    // re-seeds it from the environment. Saying so avoids the obvious
    // misreading of a control called "reset".
    ui::html_response_with_toast(
        html! {},
        "Handed back to the environment — the stored value is replaced on the next restart",
        "success",
    )
}

/// `POST /b/admin/variables/reset-pinned-at-upgrade` — the Variables page's
/// bulk control for handing back every key the one-time upgrade transition
/// pinned.
///
/// Takes no key: which rows qualify is `ops::release_keys_pinned_at_upgrade`'s
/// to decide, and the type it decides with is what keeps an admin-edited row
/// out of reach. A body or a path variable here would be the widenable filter
/// that design exists to avoid.
///
/// Reports ZERO as a success rather than an error. The page only renders the
/// control when the count is non-zero, so reaching this with nothing to do has
/// three causes and none of them is a failure: the page went stale because
/// another admin released the keys; this admin released them from the per-row
/// controls beside them; or every selected key was skipped at write time by
/// `ops::ReleaseGuard::StillPinnedAtUpgrade`, which is the action working
/// exactly as intended. Saying so beats an error the operator cannot act on.
///
/// A PARTIAL skip needs no special case either: `released` is what actually
/// moved, so the toast counts that rather than what was selected.
pub async fn handle_reset_variables_pinned_at_upgrade(
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let released = match ops::release_keys_pinned_at_upgrade(ctx, msg).await {
        Ok(released) => released,
        Err(out) => return out,
    };
    // The stored values are deliberately left in place; only the next boot
    // re-seeds them from the environment. Same wording as the per-key toast,
    // because it is the same promise.
    let toast = if released.is_empty() {
        "No keys are pinned at upgrade — nothing to hand back".to_string()
    } else {
        format!(
            "Handed {} back to the environment — the stored values are replaced on the next \
             restart",
            pluralize_keys(released.len())
        )
    };
    ui::html_response_with_toast(html! {}, &toast, "success")
}

/// `DELETE /b/admin/variables/{key}` — the Variables page's delete row control.
///
/// The page had no delete affordance at all before this: a variable could only
/// be removed by calling `DELETE /b/admin/api/settings/{key}` by hand, which
/// is not a thing an operator can be expected to discover.
///
/// The shared-key guard, the delete and the audit row live in
/// `ops::delete_variable`, shared with that JSON surface, so the two cannot
/// drift on what they refuse.
///
/// Returns EMPTY markup rather than re-rendering the page the way
/// [`handle_update_variable`] does: the control targets `closest tr` with
/// `outerHTML`, so an empty body is what removes the row. Re-rendering the
/// whole page into a `<tr>` would nest a document inside a table row.
pub async fn handle_delete_variable(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let key = msg.var("key");
    if let Err(out) = ops::delete_variable(ctx, msg, key).await {
        return out;
    }
    // The row is gone — but an env-provided or auto-generated key is ALSO in
    // the boot map, which `blocks::config`'s read order falls back to when the
    // table holds no row. For those the value keeps being served and the row
    // is written again on the next boot, so reporting a flat "deleted" would
    // be untrue in exactly the case an operator is most likely to be trying to
    // turn something off.
    let toast = if ctx.config_get(key).is_some() {
        "Variable deleted — a boot-provided value is still in effect"
    } else {
        "Variable deleted"
    };
    ui::html_response_with_toast(html! {}, toast, "success")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config_vars::{APP_NAME_KEY, AUTH_HEADLINE_KEY},
        test_support::{admin_msg, output_html, TestContext},
    };

    /// Both tabs answer a failed read with the error page, instead of the
    /// "By Block" tab listing every declared var at its default, unpinned —
    /// which tells the operator every value they set is gone.
    #[tokio::test]
    async fn a_failed_read_is_the_error_page_not_the_defaults() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();

        for tab in ["", "all"] {
            let mut msg = crate::blocks::admin::test_support::routed(admin_msg(
                "retrieve",
                "/b/admin/settings/variables",
            ));
            msg.set_meta("http.header.accept", "text/html");
            if !tab.is_empty() {
                msg.set_meta("req.query.tab", tab);
            }
            let parts = wafer_block::http_codec::collect_http_response(
                crate::blocks::admin::pages::settings::settings_page(&ctx, &msg, "variables").await,
            )
            .await;
            let html = String::from_utf8_lossy(&parts.body);
            assert_eq!(parts.status, 500, "tab {tab:?}: {html}");
            assert!(
                !html.contains(APP_NAME_KEY),
                "tab {tab:?}: a declared var rendered from its default: {html}"
            );
        }
    }

    /// Serialize a rendered form the way a BROWSER would, so a test posts what
    /// a real submit posts.
    ///
    /// Two rules carry the weight, and both are why an ops-layer test cannot
    /// stand in for this one: a `disabled` control is never serialized (not by
    /// `FormData`, not by htmx's `shouldInclude`), and an unchecked checkbox
    /// posts nothing. A hidden field with the same `name` is what carries the
    /// answer in either case — the pattern both variable modals use.
    fn serialize_form(html: &str) -> std::collections::HashMap<String, String> {
        let mut out = std::collections::HashMap::new();
        for tag in html.split("<input").skip(1) {
            let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
            if tag.contains("disabled") {
                continue;
            }
            let is_checkbox = tag.contains(r#"type="checkbox""#);
            if is_checkbox && !tag.contains("checked") {
                continue;
            }
            let attr = |name: &str| -> Option<String> {
                let pat = format!("{name}=\"");
                let i = tag.find(&pat)? + pat.len();
                let rest = &tag[i..];
                Some(rest[..rest.find('"')?].to_string())
            };
            if let (Some(name), value) = (attr("name"), attr("value")) {
                // Later fields win, matching `parse_form_body`'s last-value rule.
                out.insert(name, value.unwrap_or_default());
            }
        }
        out
    }

    fn urlencode_form(fields: &std::collections::HashMap<String, String>) -> Vec<u8> {
        fields
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    crate::util::urlencode(k),
                    crate::util::urlencode(v)
                )
            })
            .collect::<Vec<_>>()
            .join("&")
            .into_bytes()
    }

    /// Editing a REQUIRED-sensitive variable through the modal must work.
    ///
    /// The modal's Sensitive checkbox is `disabled` for such a key, so a
    /// browser posts nothing for it; the hidden field beside it has to already
    /// say `1`. It said `0` under a separate presence marker, so the form
    /// posted "not sensitive", `update_variable` refused the unflag, and the
    /// admin's value edit was dropped with a 400 — on exactly the keys that
    /// most need the edit path, including `WAFER_RUN__AUTH__JWT_SECRET`, which
    /// `reject_runtime_owned_key` deliberately leaves rotatable.
    ///
    /// Drives the real path: render the modal, serialize it as a browser
    /// would, post that.
    #[tokio::test]
    async fn a_required_sensitive_variable_can_be_edited_through_the_modal() {
        for key in [
            crate::blocks::auth::JWT_SECRET_KEY,
            crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY,
        ] {
            let ctx = TestContext::with_admin()
                .await
                .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
            assert!(
                crate::config_vars::is_sensitive_for_storage(key),
                "{key} must be required-sensitive for this test to mean anything"
            );
            variables::insert(
                &ctx,
                variables::NewVariable {
                    key: key.to_string(),
                    value: "old-secret".to_string(),
                    name: String::new(),
                    description: String::new(),
                    warning: String::new(),
                    sensitive: true,
                    updated_by: String::new(),
                    block: variables::block_for_key(key),
                },
            )
            .await
            .expect("seed the row");

            let msg = crate::blocks::admin::test_support::routed(admin_msg(
                "retrieve",
                &format!("/b/admin/variables/{key}/edit"),
            ));
            let html = output_html(handle_edit_variable_form(&ctx, &msg).await).await;

            let mut fields = serialize_form(&html);
            assert_eq!(
                fields.get("sensitive").map(String::as_str),
                Some("1"),
                "a browser must post sensitive=1 for a required key, since the \
                 checkbox is disabled and not serialized: {html}"
            );
            fields.insert("value".to_string(), "rotated-secret".to_string());

            let put = crate::blocks::admin::test_support::routed(admin_msg(
                "update",
                &format!("/b/admin/variables/{key}"),
            ));
            let out = handle_update_variable(
                &ctx,
                &put,
                InputStream::from_bytes(urlencode_form(&fields)),
            )
            .await;
            let _ = output_html(out).await;

            let row = variables::get_by_key(&ctx, key)
                .await
                .expect("get")
                .expect("row");
            assert_eq!(
                row.value, "rotated-secret",
                "the admin's edit to {key} must land, not be dropped by the masking guard"
            );
            assert!(row.sensitive, "and the key stays masked");
        }
    }

    /// A required key whose stored row is still UNFLAGGED must render masked.
    ///
    /// That is the row `repair_sensitive_flags` exists for, seen on a
    /// deployment that has not rebooted since the upgrade — on Cloudflare, one
    /// that has had no `/_deploy/init`. Reading the checkbox and the input type
    /// off the stored flag put the secret in a plain text field directly under
    /// a hint saying the variable is always sensitive.
    #[tokio::test]
    async fn an_unrepaired_required_row_still_renders_masked() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let key = crate::blocks::auth::JWT_SECRET_KEY;

        // `into_row` would flag it on the way in, which is the whole point —
        // this is a row an OLDER build left behind. The fixture lives in
        // `test_support` so no block file names the variables table.
        variables::seed_row_with_flag(&ctx, key, "legacy-secret", 0).await;

        let msg = crate::blocks::admin::test_support::routed(admin_msg(
            "retrieve",
            &format!("/b/admin/variables/{key}/edit"),
        ));
        let html = output_html(handle_edit_variable_form(&ctx, &msg).await).await;

        assert!(
            !html.contains(r#"type="text" name="value""#),
            "an unrepaired required key must not render its secret in a plain text input: {html}"
        );
        assert!(
            html.contains(r#"type="password" name="value""#),
            "it must use the masked input: {html}"
        );
        assert!(
            html.contains(r#"type="checkbox" name="sensitive" value="1" checked disabled"#),
            "and the control must read as set-and-locked, not unchecked: {html}"
        );
    }

    /// The Variables PAGE must mask the same unrepaired row the edit modal
    /// masks.
    ///
    /// `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` is sensitive by
    /// DECLARATION only — neither `_SECRET` nor `_KEY` — so a row an older
    /// build stored unflagged was rendered in clear in the table while the edit
    /// modal one click away rendered it masked. Whatever source settles the
    /// modal has to settle the table.
    #[tokio::test]
    async fn an_unrepaired_declaration_only_row_is_masked_in_the_table() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let key = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        assert!(
            !crate::config_vars::has_sensitive_suffix(key),
            "the point of this test is a key the suffix rule cannot catch"
        );

        variables::seed_row_with_flag(&ctx, key, "hunter2", 0).await;

        let msg =
            crate::blocks::admin::test_support::routed(admin_msg("retrieve", "/b/admin/variables"));
        let html = output_html(
            crate::blocks::admin::pages::settings::settings_page(&ctx, &msg, "variables").await,
        )
        .await;

        assert!(
            html.contains(key),
            "the row must be on the page at all, or this test proves nothing: {html}"
        );
        assert!(
            !html.contains("hunter2"),
            "the Variables page rendered an unrepaired bootstrap password in clear: {html}"
        );
    }

    /// Render the Variables page the way `Route::SettingsVariablesPage`
    /// dispatches it, over a context that does or does not claim a process
    /// environment.
    ///
    /// `settings_page(ctx, msg, "variables")` is verbatim what `mod.rs`'s
    /// dispatch arm calls, which is the point: a helper that emits a control
    /// proves nothing about whether the page ever calls that helper, and the
    /// missing markup this test exists for was exactly that gap.
    async fn variables_page_html(ctx: &TestContext, tab: &str) -> String {
        let mut msg = crate::blocks::admin::test_support::routed(admin_msg(
            "retrieve",
            "/b/admin/settings/variables",
        ));
        if !tab.is_empty() {
            msg.set_meta("req.query.tab", tab);
        }
        output_html(
            crate::blocks::admin::pages::settings::settings_page(ctx, &msg, "variables").await,
        )
        .await
    }

    /// An admin context on a deployment that boots from a process environment,
    /// holding one pinned declared key.
    async fn ctx_with_a_pinned_key(key: &str, has_process_env: bool) -> TestContext {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        if has_process_env {
            ctx.set_config(variables::HAS_PROCESS_ENV_CONFIG_KEY, "1");
        }
        // Through the real admin surface, which is what stamps the pin.
        let msg = admin_msg("update", "/admin/settings");
        assert!(
            ops::create_variable(&ctx, &msg, key, "AdminChoice", None, None, false)
                .await
                .is_ok(),
            "the fixture's admin create must land"
        );
        assert!(
            variables::is_pinned(
                &variables::get_by_key(&ctx, key)
                    .await
                    .expect("get")
                    .expect("row")
            ),
            "the fixture has to leave the row pinned or the test proves nothing"
        );
        ctx
    }

    /// THE MISSING AFFORDANCE. The boot WARN and the reset toast both tell an
    /// operator to use "Reset to environment" on the admin Variables page —
    /// and no markup rendered it. The handler, both routes and their route-table
    /// tests all existed; the button did not.
    ///
    /// Asserted on the page the operator actually lands on
    /// (`/b/admin/settings/variables`, whose default tab is "By Block"), for a
    /// declared shared key, because that is where a pinned
    /// `WAFER_RUN_SHARED__*` row is shown.
    #[tokio::test]
    async fn the_variables_page_offers_the_reset_control_for_a_pinned_key() {
        let key = APP_NAME_KEY;
        let ctx = ctx_with_a_pinned_key(key, true).await;

        for tab in ["", "all"] {
            let html = variables_page_html(&ctx, tab).await;
            assert!(
                html.contains(key),
                "the row must be on the {tab:?} tab at all, or this proves nothing: {html}"
            );
            assert!(
                html.contains(&format!(
                    r#"hx-post="/b/admin/variables/{key}/reset-to-environment""#
                )),
                "the {tab:?} tab must offer the control the boot WARN names: {html}"
            );
            assert!(
                html.contains(&format!(r#"aria-label="Reset {key} to environment""#)),
                "an icon-only control must expose an accessible name: {html}"
            );
        }
    }

    /// A row nothing has pinned takes the environment already, so offering to
    /// hand it back would be an action with no effect.
    #[tokio::test]
    async fn an_unpinned_row_offers_no_reset_control() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config(variables::HAS_PROCESS_ENV_CONFIG_KEY, "1");
        variables::seed_row_with_flag(&ctx, APP_NAME_KEY, "Seeded", 0).await;

        let html = variables_page_html(&ctx, "all").await;
        assert!(
            html.contains(APP_NAME_KEY),
            "the row must be on the page: {html}"
        );
        assert!(
            !html.contains("reset-to-environment"),
            "an unpinned row must offer no reset control: {html}"
        );
    }

    /// On a target with no process environment there is nothing to hand a key
    /// back TO.
    ///
    /// Cloudflare never calls `variables::seed_and_load` and the browser calls
    /// it with an empty batch, so the control's toast ("replaced on the next
    /// restart") would be a plain lie there. Absent marker means absent
    /// control — see `variables::HAS_PROCESS_ENV_CONFIG_KEY` for why the
    /// default is the safe side.
    #[tokio::test]
    async fn a_target_without_a_process_environment_offers_no_reset_control() {
        let key = APP_NAME_KEY;
        let ctx = ctx_with_a_pinned_key(key, false).await;

        for tab in ["", "all"] {
            let html = variables_page_html(&ctx, tab).await;
            assert!(html.contains(key), "the row must be on the page: {html}");
            assert!(
                !html.contains("reset-to-environment"),
                "the {tab:?} tab must not offer to hand a key back to an environment this \
                 deployment does not have: {html}"
            );
        }
    }

    /// A pinned row the process environment can NEVER set offers no reset
    /// control.
    ///
    /// Two shapes, both reachable today:
    ///
    /// - an ad hoc key an operator created and edited. `filter_to_declared_keys`
    ///   keeps only declared keys, so nothing the environment says about it ever
    ///   reaches the seeder.
    /// - `WAFER_RUN__AUTH__JWT_SECRET`, which `reject_runtime_owned_key`
    ///   deliberately lets an admin edit — and which no `ConfigVar` declares, so
    ///   the same filter strips it from every env batch.
    ///
    /// For both, clearing the pin does nothing and the toast ("replaced on the
    /// next restart") would be false: nothing ever replaces it. Same rule as
    /// `key_is_deletable` — the page does not render a button whose action is
    /// inert.
    #[tokio::test]
    async fn a_key_the_environment_cannot_set_offers_no_reset_control() {
        for key in ["MY_LEGACY_THING", crate::blocks::auth::JWT_SECRET_KEY] {
            assert!(
                !key_can_be_seeded_from_env(key),
                "{key} must be one the env batch cannot carry, or this proves nothing"
            );
            let ctx = ctx_with_a_pinned_key(key, true).await;

            for tab in ["", "all"] {
                let html = variables_page_html(&ctx, tab).await;
                assert!(
                    html.contains(key),
                    "the row must be on the {tab:?} tab: {html}"
                );
                assert!(
                    !html.contains(&format!(
                        r#"hx-post="/b/admin/variables/{key}/reset-to-environment""#
                    )),
                    "{key} cannot be seeded from the environment, so the {tab:?} tab must \
                     not offer to hand it back: {html}"
                );
            }
        }
    }

    /// The page has to say WHICH claim pins a row, because the two are not the
    /// same claim and only one of them is about a person.
    #[tokio::test]
    async fn the_page_distinguishes_an_admin_edit_from_an_upgrade_pin() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config(variables::HAS_PROCESS_ENV_CONFIG_KEY, "1");
        let key = APP_NAME_KEY;
        variables::seed_row_with_owner(&ctx, key, "KeptAtUpgrade", variables::PRE_UPGRADE_SENTINEL)
            .await;

        let html = variables_page_html(&ctx, "all").await;
        assert!(
            html.contains("Pinned at upgrade"),
            "an upgrade pin must not read as an admin edit: {html}"
        );
        assert!(
            !html.contains("Edited here"),
            "and must not claim a person made a decision nobody recorded: {html}"
        );
    }

    /// The icon-only edit button must carry an accessible name derived from
    /// the row key (2026-07-11 review: 49 unlabeled icon buttons on the
    /// Variables page alone).
    #[test]
    fn var_row_edit_button_carries_accessible_name() {
        let cells = var_row(&VarRow {
            key: APP_NAME_KEY,
            name: None,
            value: ValueState::Plain("Impresspress".to_string()),
            default: None,
            auto_generate: false,
            description: "App name",
            warning: "",
            show_default: false,
            deletable: false,
            pin: None,
            offer_reset: false,
        });
        let s = components::TableRow::new(cells)
            .render(&VAR_COLUMNS, None)
            .into_string();
        assert!(
            s.contains(r#"aria-label="Edit WAFER_RUN_SHARED__APP_NAME""#),
            "edit button must expose an aria-label with the row key: {s}"
        );
    }

    fn row_html(key: &str, deletable: bool) -> String {
        let cells = var_row(&VarRow {
            key,
            name: None,
            value: ValueState::Plain("v".to_string()),
            default: None,
            auto_generate: false,
            description: "d",
            warning: "",
            show_default: false,
            deletable,
            pin: None,
            offer_reset: false,
        });
        components::TableRow::new(cells)
            .render(&VAR_COLUMNS, None)
            .into_string()
    }

    /// A stored row offers a delete control, and it carries an accessible
    /// name for the same reason the edit button does.
    #[test]
    fn a_deletable_row_offers_a_labelled_delete_control() {
        let s = row_html("LEGACY_THING", true);
        assert!(
            s.contains(r#"hx-delete="/b/admin/variables/LEGACY_THING""#),
            "delete control must post to the row's own key: {s}"
        );
        assert!(
            s.contains(r#"aria-label="Delete LEGACY_THING""#),
            "icon-only delete button must expose an aria-label: {s}"
        );
    }

    /// Seed one sensitive row and render its edit modal, returning the HTML.
    async fn sensitive_row_modal(ctx: &TestContext, key: &str, value: &str) -> String {
        variables::insert(
            ctx,
            variables::NewVariable {
                key: key.to_string(),
                value: value.to_string(),
                name: String::new(),
                description: "before".to_string(),
                warning: String::new(),
                sensitive: true,
                updated_by: String::new(),
                block: variables::block_for_key(key),
            },
        )
        .await
        .expect("seed the row");

        let msg = crate::blocks::admin::test_support::routed(admin_msg(
            "retrieve",
            &format!("/b/admin/variables/{key}/edit"),
        ));
        output_html(handle_edit_variable_form(ctx, &msg).await).await
    }

    /// POST a serialized edit form back through the real update handler.
    async fn submit_edit(
        ctx: &TestContext,
        key: &str,
        fields: &std::collections::HashMap<String, String>,
    ) -> u16 {
        let put = crate::blocks::admin::test_support::routed(admin_msg(
            "update",
            &format!("/b/admin/variables/{key}"),
        ));
        crate::test_support::output_http_status(
            handle_update_variable(ctx, &put, InputStream::from_bytes(urlencode_form(fields)))
                .await,
        )
        .await
    }

    /// SEC-060: the edit modal must not put the stored secret in the DOM.
    ///
    /// It rendered `value=(value)` inside a `type="password"` input, which is
    /// the exact pattern `ui::settings_form::render_field` refuses: the masking
    /// is a rendering of the character glyphs, not of the bytes, so the secret
    /// is plain in page source, in devtools, in a saved page and in anything
    /// that reads the response body. Every other admin surface masks this row;
    /// one `hx-get` away it was readable in full.
    ///
    /// Asserts on what the REAL page handler emits, and on what a browser would
    /// submit from it — a secret that is not in the serialized form is not in
    /// the document either.
    #[tokio::test]
    async fn the_edit_modal_does_not_render_the_stored_secret() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let key = "MAILER_API_KEY";
        let html = sensitive_row_modal(&ctx, key, "sk-live-realsecret").await;

        assert!(
            html.contains(r#"type="password" name="value""#),
            "the fixture must be taking the masked branch, or it proves nothing: {html}"
        );
        assert!(
            !html.contains("sk-live-realsecret"),
            "the edit modal put the stored secret in the page source: {html}"
        );
        assert_ne!(
            serialize_form(&html).get("value").map(String::as_str),
            Some("sk-live-realsecret"),
            "and a browser would have submitted it straight back: {html}"
        );
        assert!(
            html.contains("(set)"),
            "the blank field must still say the variable HAS a value, or an \
             operator cannot tell 'unchanged' from 'not configured': {html}"
        );
    }

    /// Saving the modal without retyping the secret must keep the secret and
    /// land the rest of the edit.
    ///
    /// This is the half a blank-the-field fix breaks on its own: the field the
    /// browser posts is empty, `ops::update_variable`'s sensitive-empty guard
    /// refuses an empty value for a sensitive key, and the admin's description
    /// edit dies with a 400 — the same shape as the `disabled`-checkbox bug,
    /// where the form posted something the server could not read as "leave this
    /// alone". An empty masked field means "not supplied", not "clear it".
    #[tokio::test]
    async fn saving_the_modal_without_retyping_the_secret_keeps_it() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let key = "MAILER_API_KEY";
        let html = sensitive_row_modal(&ctx, key, "sk-live-realsecret").await;

        let mut fields = serialize_form(&html);
        fields.insert("description".to_string(), "after".to_string());
        assert_eq!(submit_edit(&ctx, key, &fields).await, 200);

        let row = variables::get_by_key(&ctx, key)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(
            row.value, "sk-live-realsecret",
            "an untouched masked field must leave the stored secret alone",
        );
        assert_eq!(
            row.description, "after",
            "and the edit the admin actually made must land",
        );
    }

    /// Rotation still works: a value typed into the blank field is written.
    #[tokio::test]
    async fn a_secret_typed_into_the_blank_field_rotates_it() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let key = "MAILER_API_KEY";
        let html = sensitive_row_modal(&ctx, key, "sk-live-realsecret").await;

        let mut fields = serialize_form(&html);
        fields.insert("value".to_string(), "sk-live-rotated".to_string());
        assert_eq!(submit_edit(&ctx, key, &fields).await, 200);

        assert_eq!(
            variables::get_by_key(&ctx, key)
                .await
                .expect("get")
                .expect("row")
                .value,
            "sk-live-rotated",
        );
    }

    /// A row the OPERATOR flagged sensitive, whose key says nothing, behaves
    /// like a declared one — masked on render, and read back as masked.
    ///
    /// This is the case that pins where `field_was_masked` comes from. Both
    /// other modal fixtures are decided by the KEY (`MAILER_API_KEY`'s suffix,
    /// `SITE_MOTTO`'s absence of one), so swapping the handler's stored-row read
    /// for `config_vars::is_sensitive_for_storage(key)` passes them both. It
    /// cannot pass this one: the key half answers false here, the render masks
    /// off the stored flag anyway, and a handler reading the widget back off the
    /// narrower rule would forward the blank field to the sensitive-empty guard
    /// and drop the admin's edit with a 400.
    #[tokio::test]
    async fn an_operator_flagged_row_is_masked_and_read_back_as_masked() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let key = "MY_SERVICE_HANDLE";
        assert!(
            !crate::config_vars::is_sensitive_for_storage(key),
            "the point of this test is a key the declaration/suffix rule cannot catch"
        );
        let html = sensitive_row_modal(&ctx, key, "acme-prod-secret").await;

        assert!(
            !html.contains("acme-prod-secret"),
            "a row the operator flagged sensitive must be masked too: {html}"
        );

        let mut fields = serialize_form(&html);
        fields.insert("description".to_string(), "after".to_string());
        assert_eq!(submit_edit(&ctx, key, &fields).await, 200);

        let row = variables::get_by_key(&ctx, key)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(
            row.value, "acme-prod-secret",
            "the secret survives the save"
        );
        assert_eq!(row.description, "after", "and the edit lands");
    }

    /// A NON-sensitive variable keeps the editor it always had: its value is
    /// rendered, and clearing the field really does clear it.
    #[tokio::test]
    async fn a_plain_variable_still_shows_and_clears_its_value() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let key = "SITE_MOTTO";
        variables::insert(
            &ctx,
            variables::NewVariable {
                key: key.to_string(),
                value: "move fast".to_string(),
                name: String::new(),
                description: String::new(),
                warning: String::new(),
                sensitive: false,
                updated_by: String::new(),
                block: variables::block_for_key(key),
            },
        )
        .await
        .expect("seed the row");

        let msg = crate::blocks::admin::test_support::routed(admin_msg(
            "retrieve",
            &format!("/b/admin/variables/{key}/edit"),
        ));
        let html = output_html(handle_edit_variable_form(&ctx, &msg).await).await;
        assert!(
            html.contains(r#"value="move fast""#),
            "a plain variable's value is not a secret and is still editable in place: {html}"
        );

        let mut fields = serialize_form(&html);
        fields.insert("value".to_string(), String::new());
        assert_eq!(submit_edit(&ctx, key, &fields).await, 200);
        assert_eq!(
            variables::get_by_key(&ctx, key)
                .await
                .expect("get")
                .expect("row")
                .value,
            "",
            "an empty field on an unmasked variable is an explicit clear",
        );
    }

    /// A declared var showing its default has no stored row to delete, and a
    /// shared key is refused server-side — neither may render a control that
    /// could only fail.
    #[test]
    fn a_non_deletable_row_offers_no_delete_control() {
        let s = row_html(APP_NAME_KEY, false);
        assert!(
            !s.contains("hx-delete"),
            "a non-deletable row must render no delete control: {s}"
        );
        assert!(s.contains("hx-get"), "the edit control is unaffected: {s}");
    }

    // -----------------------------------------------------------------------
    // "Reset all keys pinned at upgrade"
    // -----------------------------------------------------------------------

    /// Three rows in the three states the bulk action has to tell apart:
    /// pinned by the upgrade transition (released), pinned by an admin edit
    /// (never touched), and claimed by nobody (already follows the
    /// environment, so nothing to release).
    const UPGRADE_PINNED: [&str; 2] = [APP_NAME_KEY, AUTH_HEADLINE_KEY];
    const ADMIN_EDITED: &str = crate::config_vars::ALLOW_SIGNUP_KEY;
    const UNCLAIMED: &str = crate::config_vars::PRIMARY_COLOR_KEY;

    /// An admin context holding one row in each pin state.
    ///
    /// The upgrade-pinned rows are staged through
    /// `variables::seed_row_with_owner`, which writes the `updated_by` column
    /// directly, because no surface a test can reach writes
    /// `PRE_UPGRADE_SENTINEL`: only `seed_and_load`'s one-time transition does.
    /// The admin-edited row goes through the REAL admin create, which is what
    /// stamps an admin pin — a hand-written marker there would prove nothing
    /// about the state the surface actually produces.
    async fn ctx_with_every_pin_state(has_process_env: bool) -> TestContext {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        if has_process_env {
            ctx.set_config(variables::HAS_PROCESS_ENV_CONFIG_KEY, "1");
        }
        for key in UPGRADE_PINNED {
            variables::seed_row_with_owner(
                &ctx,
                key,
                "KeptAtUpgrade",
                variables::PRE_UPGRADE_SENTINEL,
            )
            .await;
        }
        let msg = admin_msg("update", "/admin/settings");
        assert!(
            ops::create_variable(&ctx, &msg, ADMIN_EDITED, "false", None, None, false)
                .await
                .is_ok(),
            "the fixture's admin create must land"
        );
        variables::seed_row_with_owner(&ctx, UNCLAIMED, "#123456", "").await;

        assert_eq!(
            pin_of_key(&ctx, ADMIN_EDITED).await,
            Some(variables::Pin::AdminEdit),
            "the fixture's admin row has to be an admin pin, or the exclusion is untested"
        );
        assert_eq!(
            pin_of_key(&ctx, UPGRADE_PINNED[0]).await,
            Some(variables::Pin::PreUpgrade),
            "and the upgrade rows have to be upgrade pins"
        );
        ctx
    }

    async fn pin_of_key(ctx: &TestContext, key: &str) -> Option<variables::Pin> {
        variables::pin_of(
            &variables::get_by_key(ctx, key)
                .await
                .expect("get")
                .expect("row"),
        )
    }

    async fn updated_by_of(ctx: &TestContext, key: &str) -> String {
        variables::get_by_key(ctx, key)
            .await
            .expect("get")
            .expect("row")
            .updated_by
    }

    /// The bulk action as the wire reaches it: through the block's own
    /// dispatch, so the route table and the handler are both the production
    /// ones. Calling the handler directly would pass even with no route bound
    /// to it.
    ///
    /// The response is returned rather than collected here, because the tests
    /// that assert on the STORED rows must fail on those assertions — collecting
    /// an unrouted request through `output_html` panics inside the helper
    /// instead, which says nothing about whether the release happened.
    async fn bulk_release_request(ctx: &TestContext) -> OutputStream {
        use wafer_run::Block as _;
        crate::blocks::admin::AdminBlock::new()
            .handle(
                ctx,
                admin_msg("create", "/b/admin/variables/reset-pinned-at-upgrade"),
                InputStream::empty(),
            )
            .await
    }

    /// Drive the bulk action and discard the response, for a test whose subject
    /// is what the table now holds.
    async fn post_bulk_release(ctx: &TestContext) {
        crate::test_support::output_http_status(bulk_release_request(ctx).await).await;
    }

    /// THE ACTION. Every key the upgrade transition pinned is released in one
    /// press, and NOTHING else moves.
    ///
    /// The admin-edited row is the assertion that matters: "an admin edit wins
    /// permanently" is the contract the whole precedence design rests on, and a
    /// bulk control that quietly cleared one would be a worse defect than the
    /// clicking it saves.
    #[tokio::test]
    async fn the_bulk_action_releases_every_upgrade_pin_and_only_those() {
        let ctx = ctx_with_every_pin_state(true).await;

        post_bulk_release(&ctx).await;

        for key in UPGRADE_PINNED {
            assert_eq!(
                updated_by_of(&ctx, key).await,
                variables::RELEASED_TO_ENV_SENTINEL,
                "{key} must carry the same released marker the per-key control writes"
            );
        }
        assert_eq!(
            pin_of_key(&ctx, ADMIN_EDITED).await,
            Some(variables::Pin::AdminEdit),
            "an admin edit wins permanently; a bulk release must never clear one"
        );
        assert_eq!(
            updated_by_of(&ctx, UNCLAIMED).await,
            "",
            "a row nothing has claimed already follows the environment, and stamping it \
             released would hide it from the one-time upgrade transition"
        );
    }

    /// THE RACE. An admin edit that lands BETWEEN the selection and the write
    /// must still be safe.
    ///
    /// `PinnedAtUpgrade` proves a row was an upgrade pin when the set was READ.
    /// The loop then writes each key one at a time, and `release_each`'s
    /// per-key re-read is what decides whether the row still qualifies — so
    /// without a pin check there, a bulk release started before a colleague's
    /// edit lands would clear that edit, which is the one thing this action must
    /// never do. Recoverable rather than destructive (only `updated_by` moves,
    /// the stored value stands until the next boot), but only if somebody
    /// notices before that boot.
    ///
    /// Driven at the `release_each` boundary because that is where the window
    /// is: the set is collected first, the row is edited second, the writes run
    /// third. The interleaving is real even though the test is single-threaded.
    #[tokio::test]
    async fn an_admin_edit_landing_after_the_selection_is_not_released() {
        let ctx = ctx_with_every_pin_state(true).await;
        let msg = admin_msg("update", "/admin/settings");

        // The operator presses the button: the set is read, both keys qualify.
        let selected = variables::keys_pinned_at_upgrade(&ctx)
            .await
            .expect("select the upgrade pins");
        assert_eq!(
            selected.len(),
            UPGRADE_PINNED.len(),
            "both keys have to be in the selection, or the race is not staged"
        );

        // A colleague edits one of them before the writes land.
        let raced = UPGRADE_PINNED[1];
        assert!(
            ops::update_variable(
                &ctx,
                &msg,
                raced,
                ops::VariableUpdate {
                    value: Some("DecidedJustNow"),
                    description: None,
                    sensitive: None,
                },
            )
            .await
            .is_ok(),
            "the racing admin edit must land"
        );
        assert_eq!(
            pin_of_key(&ctx, raced).await,
            Some(variables::Pin::AdminEdit),
            "and it must really have re-pinned the row"
        );

        let Ok(released) = ops::release_each(&ctx, &msg, &selected).await else {
            panic!("the release must not fail over one racing edit")
        };

        assert_eq!(
            released,
            vec![UPGRADE_PINNED[0].to_string()],
            "only the key that still qualified at write time may be released"
        );
        assert_eq!(
            pin_of_key(&ctx, raced).await,
            Some(variables::Pin::AdminEdit),
            "the decision made during the window survives it"
        );
        assert_eq!(
            variables::get_by_key(&ctx, raced)
                .await
                .expect("get")
                .expect("row")
                .value,
            "DecidedJustNow",
            "and so does the value that decision set"
        );
    }

    /// One audit row per released key, naming the key.
    ///
    /// The same `variable.reset_to_environment` action the per-key control
    /// writes: the outcome is identical per key, so an operator filtering the
    /// audit log for who released a given key must find it whichever control
    /// was used.
    #[tokio::test]
    async fn the_bulk_action_audits_each_key_it_released() {
        let ctx = ctx_with_every_pin_state(true).await;

        post_bulk_release(&ctx).await;

        let rows = crate::db_read::list_every(
            &ctx,
            crate::blocks::admin::logs::AUDIT_LOGS_TABLE,
            vec![wafer_block::db::Filter {
                field: "action".to_string(),
                operator: wafer_block::db::FilterOp::Equal,
                value: serde_json::Value::String("variable.reset_to_environment".to_string()),
            }],
        )
        .await
        .expect("list audit rows");
        let resources: std::collections::BTreeSet<String> = rows
            .iter()
            .filter_map(|r| r.data.get("resource")?.as_str().map(str::to_string))
            .collect();

        assert_eq!(
            resources,
            UPGRADE_PINNED
                .iter()
                .map(|k| format!("variables/{k}"))
                .collect::<std::collections::BTreeSet<String>>(),
            "the trail has to say WHICH keys were released, and no others"
        );
    }

    /// The control renders only when there is something for it to do.
    #[tokio::test]
    async fn the_page_offers_the_bulk_release_when_a_key_is_pinned_at_upgrade() {
        let ctx = ctx_with_every_pin_state(true).await;

        for tab in ["", "all"] {
            let html = variables_page_html(&ctx, tab).await;
            assert!(
                html.contains(r#"hx-post="/b/admin/variables/reset-pinned-at-upgrade""#),
                "the {tab:?} tab must offer the bulk release: {html}"
            );
            assert!(
                html.contains("Reset all keys pinned at upgrade"),
                "and label it the way the pin badge names the state: {html}"
            );
        }
    }

    /// An admin edit is not an upgrade pin, and must not make the bulk control
    /// appear: pressing it would release nothing, and a control that offers to
    /// clear admin edits is the misreading this action must not invite.
    #[tokio::test]
    async fn an_admin_edit_alone_offers_no_bulk_release() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config(variables::HAS_PROCESS_ENV_CONFIG_KEY, "1");
        let msg = admin_msg("update", "/admin/settings");
        assert!(
            ops::create_variable(&ctx, &msg, ADMIN_EDITED, "false", None, None, false)
                .await
                .is_ok(),
            "the fixture's admin create must land"
        );

        for tab in ["", "all"] {
            let html = variables_page_html(&ctx, tab).await;
            assert!(
                html.contains(ADMIN_EDITED),
                "the row must be on the {tab:?} tab, or this proves nothing: {html}"
            );
            assert!(
                !html.contains("reset-pinned-at-upgrade"),
                "nothing is pinned at upgrade, so the {tab:?} tab must offer no bulk \
                 release: {html}"
            );
        }
    }

    /// The released rows have to hold against the ONE-TIME UPGRADE TRANSITION,
    /// not merely against later boots — the property
    /// [`variables::RELEASED_TO_ENV_SENTINEL`] exists for.
    ///
    /// The gate is recorded only by a boot that HAD exports, so a deployment
    /// whose early boots carried none reaches the admin UI with the transition
    /// still armed. If the bulk release emptied `updated_by` instead, the first
    /// boot that did carry an export would read every released row as
    /// never-considered and pin it straight back — the control silently not
    /// working, in bulk. Mirrors
    /// `variables::boot_tests::a_reset_is_not_undone_by_a_transition_that_has_not_run_yet`
    /// for the path that releases many keys at once.
    #[tokio::test]
    async fn keys_released_in_bulk_are_not_re_pinned_by_the_next_boot() {
        let ctx = ctx_with_every_pin_state(true).await;
        assert!(
            variables::get_by_key(&ctx, variables::ENV_PRECEDENCE_TRANSITION_KEY)
                .await
                .expect("get")
                .is_none(),
            "the premise: the transition has not run, so it could still re-pin a row"
        );

        post_bulk_release(&ctx).await;

        // The operator adds the exports they wanted all along, and restarts.
        let exports: Vec<(&str, &str)> = UPGRADE_PINNED
            .iter()
            .map(|key| (*key, "FromEnv"))
            .chain(std::iter::once((ADMIN_EDITED, "true")))
            .collect();
        ctx.seed_env_vars(&exports).await;

        for key in UPGRADE_PINNED {
            let row = variables::get_by_key(&ctx, key)
                .await
                .expect("get")
                .expect("row");
            assert_eq!(
                row.value, "FromEnv",
                "{key} was released, so the environment sets it from this boot on"
            );
            assert!(
                !variables::is_pinned(&row),
                "{key} must not be re-pinned by a transition that had not run yet"
            );
        }
        assert_eq!(
            variables::get_by_key(&ctx, ADMIN_EDITED)
                .await
                .expect("get")
                .expect("row")
                .value,
            "false",
            "the admin-edited key the bulk release passed over still outranks its export"
        );
    }

    /// A key pinned at upgrade that the environment can NEVER set is left
    /// alone, and does not make the control appear.
    ///
    /// `key_can_be_seeded_from_env` mirrors the two gates between the process
    /// environment and this table, so releasing such a key would change nothing
    /// and the toast ("replaced on the next restart") would be false — the same
    /// rule that keeps the per-row control off it. The two agreeing is what
    /// stops a bulk button appearing above a table where no row offers the
    /// single-key one.
    #[tokio::test]
    async fn a_key_the_environment_cannot_set_is_not_part_of_the_bulk_release() {
        let key = "MY_LEGACY_THING";
        assert!(
            !key_can_be_seeded_from_env(key),
            "the fixture's key must be one no env batch can carry, or this proves nothing"
        );
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config(variables::HAS_PROCESS_ENV_CONFIG_KEY, "1");
        variables::seed_row_with_owner(&ctx, key, "legacy", variables::PRE_UPGRADE_SENTINEL).await;

        for tab in ["", "all"] {
            let html = variables_page_html(&ctx, tab).await;
            assert!(
                html.contains(key),
                "the row must be on the {tab:?} tab: {html}"
            );
            assert!(
                !html.contains("reset-pinned-at-upgrade"),
                "no row on the {tab:?} tab can be handed back, so no bulk control: {html}"
            );
        }

        post_bulk_release(&ctx).await;
        assert_eq!(
            updated_by_of(&ctx, key).await,
            variables::PRE_UPGRADE_SENTINEL,
            "and the action itself leaves it pinned"
        );
    }

    /// The toast is the whole result — `hx-swap="none"` swaps no markup — so it
    /// has to say how many keys moved and that nothing changes until a restart.
    #[tokio::test]
    async fn the_bulk_action_reports_what_it_released() {
        let ctx = ctx_with_every_pin_state(true).await;

        let trigger =
            crate::test_support::output_header(bulk_release_request(&ctx).await, "HX-Trigger")
                .await
                .expect("the control's only result is its toast");

        assert!(
            trigger.contains("Handed 2 keys back to the environment"),
            "the toast must name how many keys moved: {trigger}"
        );
        assert!(
            trigger.contains("next restart"),
            "and that the stored values stand until then: {trigger}"
        );
    }

    /// Pressing the control on a stale page, after the keys have already been
    /// released, is news rather than a failure.
    #[tokio::test]
    async fn a_bulk_release_with_nothing_pinned_succeeds_and_says_so() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.set_config(variables::HAS_PROCESS_ENV_CONFIG_KEY, "1");

        let trigger =
            crate::test_support::output_header(bulk_release_request(&ctx).await, "HX-Trigger")
                .await
                .expect("a toast, not an error");
        assert!(
            trigger.contains("No keys are pinned at upgrade"),
            "an empty release must report the truth, not a failure: {trigger}"
        );
    }

    /// On a target with no process environment there is nothing to hand the
    /// keys back to — the same gate the per-key control uses, for the same
    /// reason.
    #[tokio::test]
    async fn a_target_without_a_process_environment_offers_no_bulk_release() {
        let ctx = ctx_with_every_pin_state(false).await;

        for tab in ["", "all"] {
            let html = variables_page_html(&ctx, tab).await;
            assert!(
                html.contains(UPGRADE_PINNED[0]),
                "the pinned row must be on the {tab:?} tab: {html}"
            );
            assert!(
                !html.contains("reset-pinned-at-upgrade"),
                "the {tab:?} tab must not offer to hand keys back to an environment this \
                 deployment does not have: {html}"
            );
        }
    }
}

#[cfg(test)]
mod create_form_tests {
    use wafer_run::InputStream;

    use super::*;
    use crate::test_support::{admin_msg, collect_or_panic, TestContext};

    async fn admin_ctx() -> TestContext {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        ctx
    }

    async fn sensitive_flag(ctx: &dyn Context, key: &str) -> bool {
        variables::get_by_key(ctx, key)
            .await
            .expect("get variable")
            .unwrap_or_else(|| panic!("{key} was not created"))
            .sensitive
    }

    async fn post_form(ctx: &dyn Context, body: &str) {
        let out = handle_create_variable(
            ctx,
            &admin_msg("create", "/admin/variables"),
            InputStream::from_bytes(body.as_bytes().to_vec()),
        )
        .await;
        collect_or_panic(out).await;
    }

    /// The Variables page's create form answers the SAME 409 the JSON API
    /// does for a key that is already stored, AND the operator learns why.
    ///
    /// Both halves matter. Both surfaces drive `ops::create_variable`, so the
    /// status is the half that would notice if this page started reshaping the
    /// refusal into a re-render (an htmx swap of the full page reads as
    /// "created"). The body is the half that makes the 409 worth having: htmx
    /// does not swap a 4xx, so the only thing the operator can see is what the
    /// global `htmx:responseError` listener in `ui/assets/chrome.js` raises as
    /// a toast — and that listener reads `message` out of exactly this
    /// envelope. A 409 whose body said nothing useful would look, to the person
    /// in front of the modal, precisely like the 500 this all started as.
    /// `ui/assets/test/chrome_error_toast.test.mjs` is the listener's half.
    #[tokio::test]
    async fn form_post_with_an_existing_key_answers_conflict() {
        let ctx = admin_ctx().await;
        post_form(&ctx, "key=SITE_MOTTO&value=one").await;

        let msg = admin_msg("create", "/admin/variables");
        let refused = || {
            handle_create_variable(
                &ctx,
                &msg,
                InputStream::from_bytes(b"key=SITE_MOTTO&value=two".to_vec()),
            )
        };
        assert_eq!(
            crate::test_support::output_http_status(refused().await).await,
            409,
        );

        let body = crate::test_support::output_http_json(refused().await).await;
        assert_eq!(body["error"], serde_json::json!("AlreadyExists"));
        let message = body["message"].as_str().unwrap_or_default();
        assert_eq!(
            message,
            "A variable with the key \"SITE_MOTTO\" already exists. Choose a different key.",
            "the toast has only this to show the operator",
        );
    }

    /// A form post that says nothing about sensitivity — a curl'd or
    /// hand-built post, or a form that lost its checkbox — fails safe.
    #[tokio::test]
    async fn form_post_without_the_flag_defaults_to_sensitive() {
        let ctx = admin_ctx().await;
        post_form(&ctx, "key=SITE_MOTTO&value=move+fast").await;
        assert!(sensitive_flag(&ctx, "SITE_MOTTO").await);
    }

    #[tokio::test]
    async fn form_post_with_an_explicit_zero_is_not_sensitive() {
        let ctx = admin_ctx().await;
        post_form(&ctx, "key=SITE_MOTTO&value=move+fast&sensitive=0").await;
        assert!(!sensitive_flag(&ctx, "SITE_MOTTO").await);
    }

    /// The modal posts a hidden `sensitive=0` followed by the checkbox's
    /// `sensitive=1` when checked; `parse_form_body` keeps the last value,
    /// which is what makes "unchecked" an explicit answer rather than an
    /// absence.
    #[tokio::test]
    async fn form_post_with_the_checkbox_checked_is_sensitive() {
        let ctx = admin_ctx().await;
        post_form(
            &ctx,
            "key=SITE_MOTTO&value=move+fast&sensitive=0&sensitive=1",
        )
        .await;
        assert!(sensitive_flag(&ctx, "SITE_MOTTO").await);
    }

    /// The create modal is checked by default and always posts an explicit
    /// value, so an admin who unchecks it is making a decision the server
    /// can see, and one who does not is protected.
    #[tokio::test]
    async fn create_modal_posts_the_flag_explicitly_and_is_checked_by_default() {
        let ctx = admin_ctx().await;
        let html = settings_body(&ctx, &admin_msg("retrieve", "/admin/settings"))
            .await
            .expect("the variables read succeeds")
            .into_string();
        assert!(
            html.contains(r#"type="hidden" name="sensitive" value="0""#),
            "the modal must post an explicit 0 when the box is unchecked: {html}"
        );
        assert!(
            html.contains(r#"type="checkbox" name="sensitive" value="1" checked"#),
            "the modal's checkbox must be checked by default: {html}"
        );
    }
}
