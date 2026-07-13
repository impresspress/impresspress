use maud::{html, Markup};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::admin::WRAP_GRANTS_TABLE as WRAP_GRANTS,
    ui::{components, icons},
};

/// Render JUST the permissions settings body. The parent `settings_page`
/// handler wraps this in the form-less `tabbed_page` shell — the
/// "Database & Config" subtab's Add-Grant modal renders its own
/// `<form hx-post="/b/admin/grants/rules">`, which is only valid because
/// the shell contributes no outer `<form>` to nest in.
///
/// Internal sub-tabs use `?subtab=database|all` to avoid colliding with
/// the parent path-segment tab system (`/settings/{tab}`).
pub async fn settings_body(ctx: &dyn Context, msg: &Message) -> Markup {
    let subtab = msg.query("subtab");
    let active_subtab = match subtab {
        "database" => "database",
        _ => "all",
    };

    html! {
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
            @if active_subtab == "database" {
                (permissions_database_tab(ctx, msg).await)
            } @else {
                (permissions_all_tab(ctx, msg).await)
            }
        }
    }
}

/// Full settings page for permissions — used by WRAP grant mutation handlers
/// (and by the legacy `/b/admin/grants` route) that need to re-render the
/// complete page after a create/delete. Delegates to the canonical
/// `settings_page` so both call paths share one composition.
pub async fn permissions_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    super::settings::settings_page(ctx, msg, "permissions").await
}

pub async fn grants_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    permissions_page(ctx, msg).await
}

fn grants_code_tab(ctx: &dyn Context) -> Markup {
    let blocks = ctx.registered_blocks();

    html! {
        div .card .mt-4 {
            div .card-header {
                h3 .card-title { "Grants Declared in Code" }
                p .text-muted style="font-size:13px" {
                    "These grants are declared in block source code via BlockInfo.grants and cannot be modified here."
                }
            }
            div .card-body {
                table .table {
                    thead {
                        tr {
                            th { "Block (Owner)" }
                            th { "Grantee" }
                            th { "Type" }
                            th { "Resource Pattern" }
                            th { "Access" }
                        }
                    }
                    tbody {
                        @for block in blocks {
                            @for grant in &block.grants {
                                tr {
                                    td {
                                        span .badge .badge-info { (block.name) }
                                    }
                                    td {
                                        @if grant.grantee == "*" {
                                            span .badge .badge-warning { "* (all blocks)" }
                                        } @else {
                                            code { (grant.grantee) }
                                        }
                                    }
                                    td {
                                        @if let Some(ref rt) = grant.resource_type {
                                            span .badge .badge-info style="font-size:11px" { (rt) }
                                        } @else {
                                            span .badge .badge-secondary style="font-size:11px" { "all" }
                                        }
                                    }
                                    td {
                                        code style="font-size:12px" { (grant.resource) }
                                    }
                                    td {
                                        @if grant.write {
                                            span .badge .badge-danger { "read + write" }
                                        } @else {
                                            span .badge .badge-success { "read only" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

pub(crate) async fn grants_custom_tab(ctx: &dyn Context, _msg: &Message) -> Markup {
    let grants = db::list_all(ctx, WRAP_GRANTS, vec![])
        .await
        .unwrap_or_default();

    // Collect registered block names for the grantee dropdown
    let blocks = ctx.registered_blocks();
    let block_names: Vec<&str> = blocks.iter().map(|b| b.name.as_str()).collect();

    html! {
        div .card .mt-4 {
            div .card-header .flex .items-center .justify-between {
                div {
                    h3 .card-title { "Custom Grants" }
                    p .text-muted style="font-size:13px" {
                        "Add grants for third-party or WASM blocks. These are loaded at startup alongside code-declared grants."
                    }
                }
                button .btn .btn-primary .btn-sm onclick="openModal('add-grant-modal')" {
                    (icons::plus()) " Add Grant"
                }
            }
            div .card-body {
                @if grants.is_empty() {
                    p .text-muted { "No custom grants configured." }
                } @else {
                    table .table {
                        thead {
                            tr {
                                th { "Grantee" }
                                th { "Type" }
                                th { "Resource Pattern" }
                                th { "Access" }
                                th { "Description" }
                                th style="width:60px" {}
                            }
                        }
                        tbody {
                            @for grant in &grants {
                                @let id = &grant.id;
                                @let grantee = grant.data.get("grantee").and_then(|v| v.as_str()).unwrap_or("");
                                @let resource = grant.data.get("resource").and_then(|v| v.as_str()).unwrap_or("");
                                @let write = grant.data.get("write").map(|v| v.as_i64().unwrap_or(0) != 0 || v.as_str() == Some("1")).unwrap_or(false);
                                @let rt = grant.data.get("resource_type").and_then(|v| v.as_str()).unwrap_or("");
                                @let description = grant.data.get("description").and_then(|v| v.as_str()).unwrap_or("");
                                tr {
                                    td {
                                        @if grantee == "*" {
                                            span .badge .badge-warning { "* (all blocks)" }
                                        } @else {
                                            code { (grantee) }
                                        }
                                    }
                                    td {
                                        @if rt.is_empty() {
                                            span .badge .badge-secondary style="font-size:11px" { "all" }
                                        } @else {
                                            span .badge .badge-info style="font-size:11px" { (rt) }
                                        }
                                    }
                                    td {
                                        code style="font-size:12px" { (resource) }
                                    }
                                    td {
                                        @if write {
                                            span .badge .badge-danger { "read + write" }
                                        } @else {
                                            span .badge .badge-success { "read only" }
                                        }
                                    }
                                    td style="font-size:13px" { (description) }
                                    td {
                                        button .btn .btn-danger .btn-sm
                                            hx-delete={"/b/admin/grants/rules/" (id)}
                                            hx-target="#content"
                                            hx-confirm="Delete this grant?"
                                        { (icons::trash()) }
                                    }
                                }
                            }
                        }
                    }
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
                serde_json::to_string(&block_data).unwrap_or_default()
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
                    specificEl.style.display = 'none';
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
                    specificEl.style.display = '';
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
                    specificSelect.onchange = function() { resourceEl.value = this.value; };
                }
            }
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
                    p .text-muted style="font-size:12px;margin-top:4px" {
                        "The block that will receive this access permission."
                    }
                }
                div .form-group {
                    label .form-label for="grant_owner" { "Access to which block's data?" }
                    select .form-input #grant_owner
                        onchange="updateGrantForm()"
                    {
                        option value="" disabled selected { "Select the data owner..." }
                        @for b in blocks.iter().filter(|b| b.name.contains('/')) {
                            option value=(b.name) { (b.name) }
                        }
                    }
                    p .text-muted style="font-size:12px;margin-top:4px" {
                        "Each block owns its own database tables, config keys, and storage. Pick the block whose data you want to share."
                    }
                }
                div .form-group {
                    label .form-label for="resource_type" { "What kind of data?" }
                    select .form-input #resource_type name="resource_type"
                        onchange="updateGrantForm()"
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
                        onchange="updateGrantForm()"
                    {
                        option value="all" { "All resources of this type" }
                        option value="specific" { "A specific resource" }
                    }
                }
                div .form-group #specific_group style="display:none" {
                    label .form-label for="specific_resource" { "Pick a resource" }
                    select .form-input #specific_resource {}
                }
                // Hidden field that holds the computed resource pattern
                input type="hidden" #resource name="resource";
                div .form-group {
                    label .form-label .flex .items-center .gap-2 {
                        input type="checkbox" #write name="write" value="on";
                        " Allow write access"
                    }
                    p .text-muted style="font-size:12px;margin-top:4px" {
                        "If unchecked, the block can only read the data."
                    }
                }
                div .form-group {
                    label .form-label for="description" { "Why is this needed? (optional)" }
                    input .form-input type="text" #description name="description"
                        placeholder="e.g. Analytics block needs to read user profiles";
                }
                div .form-actions {
                    button .btn .btn-secondary type="button" onclick="closeModal('add-grant-modal')" { "Cancel" }
                    button .btn .btn-primary type="submit" { "Add Grant" }
                }
            }
        }))
    }
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
async fn permissions_all_tab(ctx: &dyn Context, _msg: &Message) -> Markup {
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
            let verb = if grant.write {
                "can read and write"
            } else {
                "can read"
            };
            let sentence = format!("{} {} {}' {}", grantee, verb, block.name, grant.resource);
            all_rows.push(PermRow {
                type_label,
                sentence,
                origin: "code",
                sort_key: block.name.clone(),
                order: 1,
            });
        }
    }

    // 2. Custom DB grants
    let custom_grants = db::list_all(ctx, WRAP_GRANTS, vec![])
        .await
        .unwrap_or_default();
    for grant in &custom_grants {
        let grantee = grant
            .data
            .get("grantee")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let resource = grant
            .data
            .get("resource")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let write = grant
            .data
            .get("write")
            .map(|v| v.as_i64().unwrap_or(0) != 0 || v.as_str() == Some("1"))
            .unwrap_or(false);
        let rt = grant
            .data
            .get("resource_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
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
        let verb = if write {
            "can read and write"
        } else {
            "can read"
        };
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

    html! {
        div .card .mt-4 {
            div .card-body {
                @if all_rows.is_empty() {
                    p .text-muted style="padding:2rem;text-align:center" {
                        "No permissions configured yet."
                    }
                } @else {
                    table .table {
                        thead {
                            tr {
                                th style="width:110px" { "Type" }
                                th { "Permission" }
                                th style="width:80px" { "Origin" }
                            }
                        }
                        tbody {
                            @for row in &all_rows {
                                tr {
                                    td {
                                        @let badge_class = match row.type_label.as_str() {
                                            "DB" | "DB/Config" => "badge-info",
                                            "Config" => "badge-info",
                                            "Storage" => "badge-warning",
                                            "Network" => "badge-success",
                                            "Crypto" => "badge-secondary",
                                            _ => "badge-secondary",
                                        };
                                        span .badge .(badge_class) style="font-size:11px" { (row.type_label) }
                                    }
                                    td style="font-size:13px" { (row.sentence) }
                                    td {
                                        @if row.origin == "code" {
                                            span .badge .badge-secondary style="font-size:10px" { "code" }
                                        } @else {
                                            span .badge .badge-primary style="font-size:10px" { "custom" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// "Database & Config" tab: wraps the existing grants_code_tab and grants_custom_tab.
async fn permissions_database_tab(ctx: &dyn Context, msg: &Message) -> Markup {
    html! {
        (grants_custom_tab(ctx, msg).await)
        (grants_code_tab(ctx))
    }
}
