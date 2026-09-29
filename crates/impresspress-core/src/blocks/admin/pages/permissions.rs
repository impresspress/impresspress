use maud::{html, Markup};
use wafer_block::GrantWrite;
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    platform_state::wrap_grants,
    ui::{
        components::{self, badge, Badge, BadgeVariant},
        icons,
    },
};

/// Render JUST the permissions settings body. The parent `settings_page`
/// handler wraps this in the form-less `tabbed_page` shell — the
/// "Database & Config" subtab's Add-Grant modal renders its own
/// `<form hx-post="/b/admin/grants/rules">`, which is only valid because
/// the shell contributes no outer `<form>` to nest in.
///
/// Internal sub-tabs use `?subtab=database|all` to avoid colliding with
/// the parent path-segment tab system (`/settings/{tab}`).
///
/// Returns `Err` when the custom-grant read behind either subtab fails.
/// "No custom grants" reads as "no one has been given extra access", which
/// is the most misleading sentence this admin surface can print, so the
/// parent renders the error page instead of it.
pub async fn settings_body(
    ctx: &dyn Context,
    msg: &Message,
) -> Result<Markup, wafer_run::WaferError> {
    let subtab = msg.query("subtab");
    let active_subtab = match subtab {
        "database" => "database",
        _ => "all",
    };

    let content = if active_subtab == "database" {
        permissions_database_tab(ctx, msg).await?
    } else {
        permissions_all_tab(ctx, msg).await?
    };

    Ok(html! {
        (components::tab_navigation(vec![
            components::Tab {
                active: active_subtab == "all",
                href: "/b/admin/settings/permissions",
                label: "All",
                icon: Some(icons::shield()),
            },
            components::Tab {
                active: active_subtab == "database",
                href: "/b/admin/settings/permissions?subtab=database",
                label: "Database & Config",
                icon: Some(icons::database()),
            },
        ]))

        div #permissions-content {
            (content)
        }
    })
}

/// Full settings page for permissions — used by WRAP grant mutation handlers
/// (and by the legacy `/b/admin/grants` route) that need to re-render the
/// complete page after a create/delete. Delegates to the canonical
/// `settings_page` so both call paths share one composition.
pub async fn permissions_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    super::settings::settings_page(ctx, msg, "permissions").await
}

/// The access a grant confers, as the grant tables badge it.
fn access_badge(write: GrantWrite) -> Markup {
    match write {
        GrantWrite::Full => badge(BadgeVariant::Danger, "read + write"),
        GrantWrite::Append => badge(BadgeVariant::Warning, "append only"),
        GrantWrite::None => badge(BadgeVariant::Success, "read only"),
    }
}

/// A code-declared grant as the "All" tab words it: the grantee, what it may
/// do, and whose resource — `block` declared the grant, so the resource is
/// the owning block's.
fn code_grant_sentence(grantee: &str, verb: &str, owner: &str, resource: &str) -> String {
    format!("{grantee} {verb} {owner}'s {resource}")
}

/// The access a grant confers, as the permissions summary words it.
fn access_verb(write: GrantWrite) -> &'static str {
    match write {
        GrantWrite::Full => "can read and write",
        GrantWrite::Append => "can only add rows to",
        GrantWrite::None => "can read",
    }
}

pub async fn grants_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    permissions_page(ctx, msg).await
}

fn grants_code_tab(ctx: &dyn Context) -> Markup {
    let blocks = ctx.registered_blocks();

    html! {
        div .card .mt-4 {
            header .card__head {
                div {
                    h3 .card__title { "Grants Declared in Code" }
                    p .card__subtitle {
                        "These grants are declared in block source code via BlockInfo.grants and cannot be modified here."
                    }
                }
            }
            div .card__body {
                @let rows: Vec<Vec<Markup>> = blocks.iter().flat_map(|block| {
                    block.grants.iter().map(move |grant| vec![
                        Badge::new(BadgeVariant::Info).render(components::breakable_id(&block.name)),
                        html! {
                            @if grant.grantee == "*" {
                                (badge(BadgeVariant::Warning, "* (all blocks)"))
                            } @else {
                                code { (components::breakable_id(&grant.grantee)) }
                            }
                        },
                        html! {
                            @if let Some(ref rt) = grant.resource_type {
                                (Badge::new(BadgeVariant::Info).classes("text-11").render(html! { (rt) }))
                            } @else {
                                (Badge::new(BadgeVariant::Secondary).classes("text-11").render(html! { "all" }))
                            }
                        },
                        html! { code .text-xs { (components::breakable_id(&grant.resource)) } },
                        html! {
                            (access_badge(grant.write))
                        },
                    ])
                }).collect();

                (components::data_table::<fn(usize) -> Option<String>>(
                    &CODE_GRANT_COLUMNS,
                    rows,
                    None,
                    // The raw table this replaced rendered its header over an
                    // empty body when no block declared a grant, which read as
                    // a broken table. The component renders the empty slot in
                    // place of the whole table, so the slot has to say it.
                    html! { p .text-center .text-muted { "No grants are declared in block source code." } },
                ))
            }
        }
    }
}

pub(crate) async fn grants_custom_tab(
    ctx: &dyn Context,
    _msg: &Message,
) -> Result<Markup, wafer_run::WaferError> {
    let grants = wrap_grants::list(ctx).await?;

    // Collect registered block names for the grantee dropdown
    let blocks = ctx.registered_blocks();
    let block_names: Vec<&str> = blocks.iter().map(|b| b.name.as_str()).collect();

    Ok(html! {
        div .card .mt-4 {
            header .card__head {
                div {
                    h3 .card__title { "Custom Grants" }
                    p .card__subtitle {
                        "Add grants for third-party or WASM blocks. These are loaded at startup alongside code-declared grants."
                    }
                }
                div .card__actions {
                    button .btn .btn--primary .btn--sm data-action="modal-open" data-modal-target="add-grant-modal" {
                        (icons::plus()) " Add Grant"
                    }
                }
            }
            div .card__body {
                @if grants.is_empty() {
                    p .text-muted { "No custom grants configured." }
                } @else {
                    @let rows: Vec<Vec<Markup>> = grants.iter().map(|grant| {
                        let grantee = grant.grantee.as_str();
                        let rt = grant.resource_type.as_str();
                        vec![
                            html! {
                                @if grantee == "*" {
                                    (badge(BadgeVariant::Warning, "* (all blocks)"))
                                } @else {
                                    code { (components::breakable_id(grantee)) }
                                }
                            },
                            html! {
                                @if rt.is_empty() {
                                    (Badge::new(BadgeVariant::Secondary).classes("text-11").render(html! { "all" }))
                                } @else {
                                    (Badge::new(BadgeVariant::Info).classes("text-11").render(html! { (rt) }))
                                }
                            },
                            html! { code .text-xs { (components::breakable_id(&grant.resource)) } },
                            html! {
                                (access_badge(grant.write))
                            },
                            html! { span .text-13 { (grant.description) } },
                            html! {
                                button .btn .btn--danger .btn--sm
                                    hx-delete={"/b/admin/grants/rules/" (grant.id)}
                                    hx-target="#content"
                                    hx-confirm="Delete this grant?"
                                { (icons::trash()) }
                            },
                        ]
                    }).collect();

                    (components::data_table::<fn(usize) -> Option<String>>(
                        &CUSTOM_GRANT_COLUMNS,
                        rows,
                        None,
                        html! {},
                    ))
                }
            }
        }

        // Build JSON data for the grant form JS
        script {
            (maud::PreEscaped("var grantBlocks = "))
            (maud::PreEscaped({
                let block_data: Vec<serde_json::Value> = blocks.iter()
                    .filter(|b| b.name.contains('/'))
                    .map(|b| {
                        let prefix = format!("{}__", b.name.replace('/', "__").replace('-', "_"));
                        let config_prefix = prefix.to_uppercase();
                        serde_json::json!({
                            "name": b.name,
                            "prefix": prefix,
                            "config_prefix": config_prefix,
                            "collections": b.collections.iter().map(|c| &c.name).collect::<Vec<_>>(),
                            "config_keys": b.config_keys.iter().map(|k| &k.key).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                crate::ui::script_json_escape(&serde_json::to_string(&block_data).unwrap_or_default())
            }))
            (maud::PreEscaped(r#";
            function updateGrantForm() {
                var owner = document.getElementById('grant_owner').value;
                var type = document.getElementById('resource_type').value;
                var scopeEl = document.getElementById('grant_scope');
                var specificEl = document.getElementById('specific_group');
                var resourceEl = document.getElementById('resource');
                var specificSelect = document.getElementById('specific_resource');
                if (!owner || !scopeEl) return;

                var block = grantBlocks.find(function(b) { return b.name === owner; });
                if (!block) return;

                // Update hidden resource field based on selections
                if (scopeEl.value === 'all') {
                    specificEl.hidden = true;
                    // Auto-fill resource pattern
                    if (type === 'config') {
                        resourceEl.value = block.config_prefix + '*';
                    } else if (type === 'storage') {
                        resourceEl.value = block.name + '/*';
                    } else if (type === 'crypto') {
                        resourceEl.value = block.name;
                    } else {
                        resourceEl.value = block.prefix + '*';
                    }
                } else {
                    specificEl.hidden = false;
                    // Populate specific resource dropdown
                    specificSelect.innerHTML = '';
                    var items = [];
                    if (type === 'db' || type === '') {
                        block.collections.forEach(function(c) { items.push(c); });
                    }
                    if (type === 'config' || type === '') {
                        block.config_keys.forEach(function(k) { items.push(k); });
                    }
                    if (items.length === 0) {
                        var opt = document.createElement('option');
                        opt.value = block.prefix + '*';
                        opt.text = 'All resources (' + block.prefix + '*)';
                        specificSelect.appendChild(opt);
                    }
                    items.forEach(function(item) {
                        var opt = document.createElement('option');
                        opt.value = item;
                        opt.text = item;
                        specificSelect.appendChild(opt);
                    });
                    resourceEl.value = specificSelect.value;
                }
            }
            // Guarded: this tab is reached by an htmx partial swap, which
            // returns the body verbatim (`ui/mod.rs:226`) and re-executes the
            // scripts in it against a `document` that outlived the swap. The
            // `grantBlocks` assignment and the function declaration above are
            // deliberately outside the guard -- they must be refreshed on
            // every swap, and re-running them is idempotent. Only the
            // registration accumulates.
            (function () {
                if (window.__grantFormDelegated) return;
                window.__grantFormDelegated = true;
                document.addEventListener('change', function (e) {
                    var el = e.target;
                    if (!(el instanceof Element)) return;
                    if (el.getAttribute('data-action') === 'grant-form-update') updateGrantForm();
                });
            })();
            "#))
        }

        (components::modal("add-grant-modal", "Add Access Grant", html! {
            form hx-post="/b/admin/grants/rules" hx-target="#content" {
                div .form-group {
                    label .form-label for="grantee" { "Which block needs access?" }
                    select .form-input #grantee name="grantee" required {
                        option value="" disabled selected { "Select a block..." }
                        option value="*" { "All blocks" }
                        @for name in &block_names {
                            option value=(name) { (name) }
                        }
                    }
                    p .text-muted .text-xs .mt-1 {
                        "The block that will receive this access permission."
                    }
                }
                div .form-group {
                    label .form-label for="grant_owner" { "Access to which block's data?" }
                    select .form-input #grant_owner
                        data-action="grant-form-update"
                    {
                        option value="" disabled selected { "Select the data owner..." }
                        @for b in blocks.iter().filter(|b| b.name.contains('/')) {
                            option value=(b.name) { (b.name) }
                        }
                    }
                    p .text-muted .text-xs .mt-1 {
                        "Each block owns its own database tables, config keys, and storage. Pick the block whose data you want to share."
                    }
                }
                div .form-group {
                    label .form-label for="resource_type" { "What kind of data?" }
                    select .form-input #resource_type name="resource_type"
                        data-action="grant-form-update"
                    {
                        option value="" { "All (database + config + storage)" }
                        option value="db" { "Database tables" }
                        option value="config" { "Config keys" }
                        option value="storage" { "Storage files" }
                        option value="crypto" { "Crypto signing keys" }
                    }
                }
                div .form-group {
                    label .form-label for="grant_scope" { "How much access?" }
                    select .form-input #grant_scope
                        data-action="grant-form-update"
                    {
                        option value="all" { "All resources of this type" }
                        option value="specific" { "A specific resource" }
                    }
                }
                div .form-group #specific_group hidden {
                    label .form-label for="specific_resource" { "Pick a resource" }
                    // Its value IS the computed resource pattern, so it mirrors
                    // straight into the hidden `#resource` field below. That
                    // used to be an `el.onchange = function() {…}` assignment
                    // handed out each time the dropdown was repopulated.
                    select .form-input #specific_resource
                        data-action="mirror-value" data-mirror-target="resource" {}
                }
                // Hidden field that holds the computed resource pattern
                input type="hidden" #resource name="resource";
                div .form-group {
                    label .form-label .flex .items-center .gap-2 {
                        input type="checkbox" #write name="write" value="on";
                        " Allow write access"
                    }
                    p .text-muted .text-xs .mt-1 {
                        "If unchecked, the block can only read the data."
                    }
                }
                div .form-group {
                    label .form-label for="description" { "Why is this needed? (optional)" }
                    input .form-input type="text" #description name="description"
                        placeholder="e.g. Analytics block needs to read user profiles";
                }
                div .form-actions {
                    button .btn .btn--secondary type="button" data-action="modal-close" data-modal-target="add-grant-modal" { "Cancel" }
                    button .btn .btn--primary type="submit" { "Add Grant" }
                }
            }
        }))
    })
}

// ---------------------------------------------------------------------------
// Permissions page tab functions
// ---------------------------------------------------------------------------

/// Map a wire-level resource_type string ("db", "config", …) to its
/// display label ("DB", "Config", …). Used by every render of the
/// permissions tabs — was inlined as a 6-arm `match` ladder at four sites.
fn human_resource_type(rt: &str) -> &'static str {
    match rt {
        "db" => "DB",
        "config" => "Config",
        "storage" => "Storage",
        "crypto" => "Crypto",
        "network" => "Network",
        // Unknown values pass through as best-effort — `match.expr` had a
        // wildcard arm returning the input. Returning a known &'static str
        // here trades flexibility for type-clarity; unknown values render
        // as "Other".
        _ => "Other",
    }
}

/// One row in the unified permissions table (see `permissions_all_tab`).
struct PermRow {
    /// Resource-type badge: "DB" / "Config" / "Storage" / "Network" / "Crypto" / etc.
    type_label: String,
    /// Human-readable sentence ("`<grantee>` can read `<owner>`'s `<resource>`").
    sentence: String,
    /// Origin: "code" (declared in BlockInfo.grants) or "custom"
    /// (DB-backed WRAP grants).
    origin: &'static str,
    /// Sort key — typically the owner block name (for code rows) or the
    /// grantee (for custom rows). Used for secondary sort within a group.
    sort_key: String,
    /// Group order — 0 = custom rules (shown first), 1 = code-declared
    /// grants. Custom rules surface first because they're admin-editable.
    order: u8,
}

/// "All" tab: combines code-declared and custom WRAP grants into one
/// unified table with human-readable descriptions.
async fn permissions_all_tab(
    ctx: &dyn Context,
    _msg: &Message,
) -> Result<Markup, wafer_run::WaferError> {
    let blocks = ctx.registered_blocks();

    // 1. Code grants (from block declarations)
    let mut all_rows: Vec<PermRow> = Vec::new();

    for block in blocks {
        for grant in &block.grants {
            let type_label = match &grant.resource_type {
                Some(rt) => human_resource_type(rt.to_string().as_str()).to_string(),
                None => "DB/Config".to_string(),
            };
            let grantee = if grant.grantee == "*" {
                "All blocks".to_string()
            } else {
                grant.grantee.clone()
            };
            let verb = access_verb(grant.write);
            let sentence = code_grant_sentence(&grantee, verb, &block.name, &grant.resource);
            all_rows.push(PermRow {
                type_label,
                sentence,
                origin: "code",
                sort_key: block.name.clone(),
                order: 1,
            });
        }
    }

    // 2. Custom DB grants. An unreadable grant table used to be dropped here
    // and the page then rendered only the code-declared rows, so a custom
    // grant an operator was auditing simply was not on the list.
    let custom_grants = wrap_grants::list(ctx).await?;
    for grant in &custom_grants {
        let grantee = grant.grantee.as_str();
        let resource = grant.resource.as_str();
        let rt = grant.resource_type.as_str();
        let type_label = if rt.is_empty() {
            "DB/Config"
        } else {
            human_resource_type(rt)
        };
        let grantee_display = if grantee == "*" {
            "All blocks"
        } else {
            grantee
        };
        let verb = access_verb(grant.write);
        let sentence = format!("{grantee_display} {verb} {resource}");
        all_rows.push(PermRow {
            type_label: type_label.to_string(),
            sentence,
            origin: "custom",
            sort_key: grantee.to_string(),
            order: 0,
        });
    }

    // Sort: custom (0) before code (1), then by sort_key
    all_rows.sort_by(|a, b| {
        a.order
            .cmp(&b.order)
            .then_with(|| a.sort_key.cmp(&b.sort_key))
    });

    Ok(html! {
        div .card .mt-4 {
            div .card__body {
                @if all_rows.is_empty() {
                    p .text-muted .p-8 .text-center {
                        "No permissions configured yet."
                    }
                } @else {
                    @let rows: Vec<Vec<Markup>> = all_rows.iter().map(|row| {
                        let variant = match row.type_label.as_str() {
                            "DB" | "DB/Config" => BadgeVariant::Info,
                            "Config" => BadgeVariant::Info,
                            "Storage" => BadgeVariant::Warning,
                            "Network" => BadgeVariant::Success,
                            "Crypto" => BadgeVariant::Secondary,
                            _ => BadgeVariant::Secondary,
                        };
                        vec![
                            Badge::new(variant).classes("text-11").render(html! { (row.type_label) }),
                            html! { span .text-13 { (row.sentence) } },
                            html! {
                                @if row.origin == "code" {
                                    (Badge::new(BadgeVariant::Secondary).classes("text-10").render(html! { "code" }))
                                } @else {
                                    (Badge::new(BadgeVariant::Primary).classes("text-10").render(html! { "custom" }))
                                }
                            },
                        ]
                    }).collect();

                    (components::data_table::<fn(usize) -> Option<String>>(
                        &PERMISSION_COLUMNS,
                        rows,
                        None,
                        html! {},
                    ))
                }
            }
        }
    })
}

/// "Database & Config" tab: wraps the existing grants_code_tab and grants_custom_tab.
async fn permissions_database_tab(
    ctx: &dyn Context,
    msg: &Message,
) -> Result<Markup, wafer_run::WaferError> {
    let custom = grants_custom_tab(ctx, msg).await?;
    Ok(html! {
        (custom)
        (grants_code_tab(ctx))
    })
}

/// The three permission tables' columns. Declared once each so the
/// `<td data-label>` the component stamps on every cell names the same column
/// its header does; the widths are the ones the old `th .w-60` / `.w-80` /
/// `.w-110` utility classes gave those headers, and the unlabelled column is
/// the one that only carries the delete control.
const CODE_GRANT_COLUMNS: [components::TableCol<'static>; 5] = [
    components::TableCol {
        label: "Block (Owner)",
        width: None,
    },
    components::TableCol {
        label: "Grantee",
        width: None,
    },
    components::TableCol {
        label: "Type",
        width: None,
    },
    components::TableCol {
        label: "Resource Pattern",
        width: None,
    },
    components::TableCol {
        label: "Access",
        width: None,
    },
];

const CUSTOM_GRANT_COLUMNS: [components::TableCol<'static>; 6] = [
    components::TableCol {
        label: "Grantee",
        width: None,
    },
    components::TableCol {
        label: "Type",
        width: None,
    },
    components::TableCol {
        label: "Resource Pattern",
        width: None,
    },
    components::TableCol {
        label: "Access",
        width: None,
    },
    components::TableCol {
        label: "Description",
        width: None,
    },
    components::TableCol {
        label: "",
        width: Some("60px"),
    },
];

const PERMISSION_COLUMNS: [components::TableCol<'static>; 3] = [
    components::TableCol {
        label: "Type",
        width: Some("110px"),
    },
    components::TableCol {
        label: "Permission",
        width: None,
    },
    components::TableCol {
        label: "Origin",
        width: Some("80px"),
    },
];

#[cfg(test)]
mod outage_tests {
    //! Both permissions subtabs read the custom WRAP grants, and both used to
    //! render an unreadable grant table as "no one has extra access" — the
    //! single most misleading empty state on the admin surface.

    use crate::{
        blocks::admin::pages::settings::settings_page,
        test_support::{admin_msg, output_http_status, TestContext},
    };

    #[tokio::test]
    async fn a_failing_grant_read_renders_the_error_page_not_no_custom_grants() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();
        let msg = admin_msg("retrieve", "/b/admin/settings/permissions");
        assert_eq!(
            output_http_status(settings_page(&ctx, &msg, "permissions").await).await,
            500
        );
    }

    #[tokio::test]
    async fn a_failing_grant_read_fails_the_database_subtab_too() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();
        let mut msg = admin_msg("retrieve", "/b/admin/settings/permissions");
        msg.set_meta("req.query.subtab", "database");
        assert_eq!(
            output_http_status(settings_page(&ctx, &msg, "permissions").await).await,
            500
        );
    }
}

#[cfg(test)]
mod wording_tests {
    use crate::{
        blocks::admin::pages::settings::settings_page,
        test_support::{admin_msg, output_html, TestContext},
    };

    /// The "All" tab words a code-declared grant as the grantee, what it may
    /// do, and the owning block's resource. The possessive was written as a
    /// bare apostrophe — "impresspress/admin' *" — on every code row.
    #[tokio::test]
    async fn a_code_grant_names_its_owner_with_a_possessive() {
        use wafer_run::Block;

        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        // The admin block's real declaration, so the rows are the grants it
        // ships.
        ctx.register_block_info(
            crate::blocks::admin::ADMIN_BLOCK_ID,
            crate::blocks::admin::AdminBlock::new().info(),
        );
        let msg = admin_msg("retrieve", "/b/admin/settings/permissions");
        let html = output_html(settings_page(&ctx, &msg, "permissions").await).await;
        // The admin block's network grant, whose resource is `*`: a platform
        // table name here would bypass that table's repo door.
        assert!(
            html.contains("All blocks can read impresspress/admin's *"),
            "{html}"
        );
        assert!(!html.contains("impresspress/admin' "), "{html}");
    }
}
