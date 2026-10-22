use maud::{html, Markup};
use wafer_block::GrantWrite;
use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    platform_state::wrap_grants,
    ui::{
        components::{self, badge, Action, ActionTarget, Badge, BadgeVariant, Card},
        icons,
    },
};

/// The Add access grant modal's id: the Custom grants card's trigger names
/// it in `data-modal-target`.
const ADD_GRANT_MODAL_ID: &str = "add-grant-modal";

/// Render JUST the permissions settings body. The parent `settings_page`
/// handler wraps this in the form-less `tabbed_page` shell — the
/// "Database & config" subtab's Add-Grant modal renders its own
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
            },
            components::Tab {
                active: active_subtab == "database",
                href: "/b/admin/settings/permissions?subtab=database",
                label: "Database & config",
            },
        ]))

        div #permissions-content {
            (content)
        }
    })
}

/// Full settings page for permissions — used by WRAP grant mutation handlers
/// that need to re-render the
/// complete page after a create/delete. Delegates to the canonical
/// `settings_page` so both call paths share one composition.
pub async fn permissions_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    super::settings::settings_page(ctx, msg, "permissions").await
}

/// The access a grant confers, as the grant tables badge it. Neutral, like
/// every classification badge (D5): the access level is a kind of grant, not
/// a status, and red/amber/green read as "broken/warning/fine" — the words
/// carry the difference.
fn access_badge(write: GrantWrite) -> Markup {
    let label = match write {
        GrantWrite::Full => "Read + write",
        GrantWrite::Append => "Append only",
        GrantWrite::None => "Read only",
    };
    badge(BadgeVariant::Secondary, label)
}

/// A code-declared grant as the "All" tab words it: the grantee, what it may
/// do, and whose resource — `block` declared the grant, so the resource is
/// the owning block's. The resource breaks between its parts
/// ([`components::breakable_id`]): a table name has no spaces, and unbroken
/// it set the column's minimum width and pushed the table past its card.
fn code_grant_sentence(grantee: &str, verb: &str, owner: &str, resource: &str) -> Markup {
    html! { (grantee) " " (verb) " " (owner) "'s " (components::breakable_id(resource)) }
}

/// What a grant row's hidden columns say, as one line under its grantee
/// (`.grant-table__meta`), shown only where the table is too narrow for its
/// columns (table.css `.grant-table`): the parts joined by a middle dot.
fn grant_meta(parts: Vec<Markup>) -> Markup {
    html! {
        span .grant-table__meta {
            @for (i, part) in parts.into_iter().enumerate() {
                @if i > 0 { " \u{b7} " }
                (part)
            }
        }
    }
}

/// The access a grant confers, as the permissions summary words it.
fn access_verb(write: GrantWrite) -> &'static str {
    match write {
        GrantWrite::Full => "can read and write",
        GrantWrite::Append => "can only add rows to",
        GrantWrite::None => "can read",
    }
}

fn grants_code_tab(ctx: &dyn Context) -> Markup {
    let blocks = ctx.registered_blocks();

    let rows: Vec<components::TableRow> = blocks
        .iter()
        .flat_map(|block| {
            block.grants.iter().map(move |grant| {
                let resource_type = match &grant.resource_type {
                    Some(rt) => badge(BadgeVariant::Info, human_resource_type(&rt.to_string())),
                    None => badge(BadgeVariant::Secondary, "All"),
                };
                let resource = html! { code { (components::breakable_id(&grant.resource)) } };
                components::TableRow::new(vec![
                    Badge::new(BadgeVariant::Info).render(components::breakable_id(&block.name)),
                    html! {
                        @if grant.grantee == "*" {
                            (badge(BadgeVariant::Secondary, "* (all blocks)"))
                        } @else {
                            code { (components::breakable_id(&grant.grantee)) }
                        }
                        (grant_meta(vec![
                            html! { (components::breakable_id(&block.name)) "'s" },
                            resource_type.clone(),
                            resource.clone(),
                        ]))
                    },
                    resource_type,
                    resource,
                    access_badge(grant.write),
                ])
            })
        })
        .collect();
    // Flush whether or not a block declares a grant: the table's frame is the
    // card's (layout rule 2), and with no rows the note takes the card's
    // inset in its place.
    Card::titled(
        "Grants declared in code",
        html! {
            div .grant-table .grant-table--code {
                (components::DataTable::new(&CODE_GRANT_COLUMNS)
                    .rows(rows)
                    .empty_note("No grants are declared in block source code.")
                    .render())
            }
        },
    )
    .subtitle(
        "These grants are declared in block source code via BlockInfo.grants and cannot be \
         modified here.",
    )
    .flush()
    .render()
}

pub(crate) async fn grants_custom_tab(
    ctx: &dyn Context,
    _msg: &Message,
) -> Result<Markup, wafer_run::WaferError> {
    let grants = wrap_grants::list(ctx).await?;

    // Collect registered block names for the grantee dropdown
    let blocks = ctx.registered_blocks();
    let block_names: Vec<&str> = blocks.iter().map(|b| b.name.as_str()).collect();

    let rows: Vec<components::TableRow> = grants
        .iter()
        .map(|grant| {
            let grantee = grant.grantee.as_str();
            let rt = grant.resource_type.as_str();
            let resource_type = if rt.is_empty() {
                badge(BadgeVariant::Secondary, "All")
            } else {
                badge(BadgeVariant::Info, human_resource_type(rt))
            };
            let resource = html! { code { (components::breakable_id(&grant.resource)) } };
            let mut meta = vec![resource_type.clone(), resource.clone()];
            if !grant.description.is_empty() {
                meta.push(html! { (grant.description) });
            }
            components::TableRow::new(vec![
                html! {
                    @if grantee == "*" {
                        (badge(BadgeVariant::Secondary, "* (all blocks)"))
                    } @else {
                        code { (components::breakable_id(grantee)) }
                    }
                    (grant_meta(meta))
                },
                resource_type,
                resource,
                access_badge(grant.write),
                html! { (grant.description) },
                html! {
                    button .btn .btn--quiet-danger .btn--icon
                        type="button"
                        title="Delete grant"
                        aria-label=(format!("Delete grant of {} to {grantee}", grant.resource))
                        hx-delete={"/b/admin/grants/rules/" (grant.id)}
                        hx-target="#content"
                        hx-confirm="Delete this grant?"
                        data-confirm-label="Delete grant"
                        data-confirm-tone="danger"
                    { (icons::trash()) }
                },
            ])
        })
        .collect();

    // "Add grant" is the card's own action, so a Secondary: a card head is
    // never where a page's primary lives (`Card::action`). The modal's
    // submit is the dialog's one primary.
    Ok(html! {
        (Card::titled(
            "Custom grants",
            html! {
                div .grant-table {
                    (components::DataTable::new(&CUSTOM_GRANT_COLUMNS)
                        .rows(rows)
                        .empty_note("No custom grants configured.")
                        .render())
                }
            },
        )
        .subtitle(
            "Add grants for third-party or WASM blocks. These are loaded at startup alongside \
             code-declared grants.",
        )
        .action(
            Action::secondary("Add grant", ActionTarget::Modal(ADD_GRANT_MODAL_ID))
                .icon(icons::plus()),
        )
        .flush()
        .render())

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

        (components::modal(ADD_GRANT_MODAL_ID, "Add access grant", html! {
            form hx-post="/b/admin/grants/rules" hx-target="#content" {
                div .form-group {
                    label .form-label for="grantee" { "Which block needs access?" }
                    select .form-input #grantee name="grantee" required autofocus {
                        option value="" disabled selected { "Select a block..." }
                        option value="*" { "All blocks" }
                        @for name in &block_names {
                            option value=(name) { (name) }
                        }
                    }
                    p .form-hint {
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
                    p .form-hint {
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
                    p .form-hint {
                        "If unchecked, the block can only read the data."
                    }
                }
                div .form-group {
                    label .form-label for="description" { "Why is this needed? (optional)" }
                    input .form-input type="text" #description name="description"
                        placeholder="e.g. Analytics block needs to read user profiles";
                }
                (components::modal_footer(html! {
                    (components::modal_cancel())
                    (Action::primary("Add grant", ActionTarget::Submit { form: None }).render())
                }))
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
    sentence: Markup,
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
        let sentence =
            html! { (grantee_display) " " (verb) " " (components::breakable_id(resource)) };
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

    // The type is a category, not a status: one neutral badge for all of
    // them (D5) — status colours (amber, green) read as "warning" and "OK".
    let rows: Vec<components::TableRow> = all_rows
        .iter()
        .map(|row| {
            components::TableRow::new(vec![
                badge(BadgeVariant::Secondary, &row.type_label),
                row.sentence.clone(),
                badge(
                    BadgeVariant::Secondary,
                    if row.origin == "code" {
                        "Code"
                    } else {
                        "Custom"
                    },
                ),
            ])
        })
        .collect();
    // The table is the card's whole body: flush, so its frame is the card's,
    // and with no rows the note takes the card's inset in its place.
    Ok(Card::plain(
        components::DataTable::new(&PERMISSION_COLUMNS)
            .rows(rows)
            .empty_note("No permissions configured yet.")
            .render(),
    )
    .flush()
    .render())
}

/// "Database & config" tab: wraps the existing grants_code_tab and grants_custom_tab.
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
    components::TableCol::new("Block (owner)"),
    components::TableCol::new("Grantee").primary(),
    components::TableCol::new("Type"),
    components::TableCol::new("Resource pattern"),
    components::TableCol::new("Access"),
];

const CUSTOM_GRANT_COLUMNS: [components::TableCol<'static>; 6] = [
    components::TableCol::new("Grantee").primary(),
    components::TableCol::new("Type"),
    components::TableCol::new("Resource pattern"),
    components::TableCol::new("Access"),
    components::TableCol::new("Description").optional(),
    components::TableCol::new("Actions").actions().width("60px"),
];

const PERMISSION_COLUMNS: [components::TableCol<'static>; 3] = [
    components::TableCol::new("Type").width("110px"),
    components::TableCol::new("Permission").primary(),
    components::TableCol::new("Origin").width("80px"),
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

    /// Both tabs' grant tables are flush cards, so the table's frame is the
    /// card's and no framed table sits inside a card (layout rule 2). The
    /// code-declared grants card is an h2 under the page's h1.
    #[tokio::test]
    async fn the_grant_tables_are_flush_cards() {
        use wafer_run::Block;

        use crate::test_support::dom;

        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.register_block_info(
            crate::blocks::admin::ADMIN_BLOCK_ID,
            crate::blocks::admin::AdminBlock::new().info(),
        );

        let msg = admin_msg("retrieve", "/b/admin/settings/permissions");
        let all = output_html(settings_page(&ctx, &msg, "permissions").await).await;
        assert!(all.contains(r#"<div class="card card--flush">"#), "{all}");
        assert!(dom::card_level_inside_card(&all).is_empty(), "{all}");

        let mut msg = admin_msg("retrieve", "/b/admin/settings/permissions");
        msg.set_meta("req.query.subtab", "database");
        let database = output_html(settings_page(&ctx, &msg, "permissions").await).await;
        assert!(
            database.contains(r#"<section class="card card--flush">"#),
            "{database}"
        );
        assert!(
            dom::heading_outline(&database).contains(&(2, "Grants declared in code".to_string())),
            "{database}"
        );
        // The code table narrows by its own width (table.css `.grant-table`):
        // the owner, type and resource are one line under the grantee there.
        assert!(
            database
                .contains(r#"<div class="grant-table grant-table--code"><div class="data-table">"#),
            "{database}"
        );
        assert!(
            database.contains(r#"<span class="grant-table__meta">impresspress/<wbr>admin's · "#),
            "{database}"
        );
        // A table name in the All tab's sentence breaks between its parts,
        // so it no longer sets the column's minimum width.
        assert!(
            all.contains("impresspress/admin's impresspress__<wbr>admin__"),
            "{all}"
        );
        assert!(
            dom::card_level_inside_card(&database).is_empty(),
            "{database}"
        );
    }
    async fn database_subtab(ctx: &TestContext) -> String {
        let mut msg = admin_msg("retrieve", "/b/admin/settings/permissions");
        msg.set_meta("req.query.subtab", "database");
        output_html(settings_page(ctx, &msg, "permissions").await).await
    }

    /// "Custom grants" is a titled flush card whose head holds "Add grant" as
    /// a Secondary (`Card::action`: a card head never holds a page's
    /// primary). The tab's one primary is the dialog's own "Add grant"
    /// submit, so the page shows none. With no grants the table is a note in
    /// the card's inset.
    #[tokio::test]
    async fn custom_grants_is_a_card_whose_head_opens_the_grant_dialog() {
        use crate::test_support::dom;

        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let html = database_subtab(&ctx).await;
        assert!(
            html.contains(concat!(
                r#"<section class="card card--flush"><header class="card__head"><div>"#,
                r#"<h2 class="card__title">Custom grants</h2><p class="card__subtitle">Add grants for third-party or WASM blocks."#,
            )),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<div class="card__actions"><button class="btn btn--secondary" type="button" data-action="modal-open" data-modal-target="add-grant-modal">"#
            ),
            "{html}"
        );
        assert!(
            html.contains(r#"<div class="card__body"><div class="grant-table"><p class="empty-note">No custom grants configured.</p></div></div>"#),
            "{html}"
        );
        let (page, dialogs) = dom::visible_primaries(&html);
        assert!(page.is_empty(), "{page:?}");
        assert!(
            dialogs
                .iter()
                .any(|d| d == &[r#"button.btn.btn--primary "Add grant""#.to_string()]),
            "{dialogs:?}"
        );
        assert!(
            html.contains(
                r#"<h2 class="modal__title" id="add-grant-modal-title">Add access grant</h2>"#
            ),
            "{html}"
        );
        assert!(dom::card_level_inside_card(&html).is_empty(), "{html}");
        assert!(!html.contains("text-center"), "{html}");
    }

    /// A stored grant is a row of the custom card's table, its columns and
    /// badges in sentence case; the code-declared card is flush with no rows
    /// as with rows.
    #[tokio::test]
    async fn a_custom_grant_is_a_row_in_sentence_case() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::platform_state::wrap_grants::create(
            &ctx,
            crate::platform_state::wrap_grants::NewWrapGrant {
                grantee: "impresspress/files".to_string(),
                resource: "impresspress__admin__audit_logs".to_string(),
                write: wafer_block::GrantWrite::Append,
                resource_type: "db".to_string(),
                description: "Audit the uploads".to_string(),
            },
        )
        .await
        .expect("seed a grant");
        let html = database_subtab(&ctx).await;
        for text in [
            r#"<th data-label="Resource pattern">Resource pattern</th>"#,
            r#"<th data-label="Grantee">Grantee</th>"#,
            r#"<span class="badge badge-info">DB</span>"#,
            r#"<span class="badge badge-secondary">Append only</span>"#,
            r#"<td data-label="Description">Audit the uploads</td>"#,
            // Where the table is narrow its hidden columns are one line
            // under the grantee (table.css `.grant-table`).
            concat!(
                r#"<code>impresspress/<wbr>files</code><span class="grant-table__meta">"#,
                r#"<span class="badge badge-info">DB</span> · <code>impresspress__<wbr>admin__<wbr>audit_<wbr>logs</code> · Audit the uploads</span>"#,
            ),
            r#"<div class="grant-table"><div class="data-table">"#,
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
        assert!(!html.contains("Resource Pattern"), "{html}");
        // No block declares a grant in this fixture: the code card is still
        // flush, its note in place of the table.
        assert!(
            html.contains(
                r#"<p class="empty-note">No grants are declared in block source code.</p>"#
            ),
            "{html}"
        );
        assert_eq!(
            html.matches(r#"<section class="card card--flush">"#)
                .count(),
            2,
            "{html}"
        );

        let msg = admin_msg("retrieve", "/b/admin/settings/permissions");
        let all = output_html(settings_page(&ctx, &msg, "permissions").await).await;
        assert!(
            all.contains(r#"<span class="badge badge-secondary">Custom</span>"#),
            "{all}"
        );
    }

    /// The code-declared table names the owning block's column in sentence
    /// case.
    #[test]
    fn the_code_grant_columns_are_sentence_case() {
        let labels: Vec<&str> = super::CODE_GRANT_COLUMNS
            .iter()
            .map(|c| c.label())
            .collect();
        assert_eq!(
            labels,
            [
                "Block (owner)",
                "Grantee",
                "Type",
                "Resource pattern",
                "Access"
            ]
        );
    }
}
