use maud::html;
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream};

use super::{admin_page, crumb};
use crate::{
    blocks::admin::BLOCK_SETTINGS_TABLE as BLOCK_SETTINGS,
    ui::{
        self,
        components::{empty_state, tab_navigation, Tab},
        icons,
        shell::Topbar,
        templates::{list_page, PageHeader},
        SiteConfig, UserInfo,
    },
};

/// Encode a block name (`org/block`) for use as a URL path segment. The
/// public admin URLs use `--` as the separator so the path stays parseable
/// after a `/`-stripped route match.
fn encode_block_name(name: &str) -> String {
    name.replace('/', "--")
}

pub async fn blocks_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let config = SiteConfig::load(ctx).await;
    let user = UserInfo::from_message(msg);
    let tab = msg.query("tab");
    let active_tab = match tab {
        "services" => "services",
        "infrastructure" => "infrastructure",
        "custom" => "custom",
        _ => "features",
    };

    // `registered_blocks` is already deterministically ordered by the runtime.
    let mut all_blocks: Vec<wafer_run::BlockInfo> = ctx.registered_blocks().to_vec();

    // Load block enabled/disabled state from block_settings table. Collect
    // into a `BTreeMap` so the downstream iteration order is stable across
    // process restarts (a `HashMap` would randomize per-process).
    let block_settings_rows = db::list_all(ctx, BLOCK_SETTINGS, vec![])
        .await
        .unwrap_or_default();

    let block_enabled: std::collections::BTreeMap<String, bool> = block_settings_rows
        .iter()
        .map(|r| {
            let name = r
                .data
                .get("block_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let enabled = r.data.get("enabled").and_then(|v| v.as_i64()).unwrap_or(1) != 0;
            (name, enabled)
        })
        .collect();

    // Append unloaded blocks (in block_settings but not in the runtime) as
    // placeholder BlockInfo. Iteration order is deterministic because the
    // source map is a BTreeMap.
    let registered_names: std::collections::HashSet<String> =
        all_blocks.iter().map(|b| b.name.clone()).collect();
    for (name, enabled) in &block_enabled {
        if !registered_names.contains(name) {
            let summary = if *enabled {
                "(enabled \u{2014} restart to load)"
            } else {
                "(disabled \u{2014} restart to load)"
            };
            all_blocks.push(
                wafer_run::BlockInfo::new(name, "0.0.1", "http.handler", summary)
                    .instance_mode(wafer_run::InstanceMode::Singleton)
                    .category(wafer_run::BlockCategory::Feature)
                    .can_disable(true)
                    .default_enabled(false),
            );
        }
    }
    // Sort the combined list deterministically by block name. The runtime
    // already returns registered blocks sorted, but appending unloaded
    // entries breaks that invariant.
    all_blocks.sort_by(|a, b| a.name.cmp(&b.name));

    let page_action = html! {
        div style="display:flex;gap:8px" {
            a .btn .btn-sm .btn-secondary href="https://wafer.run/registry" target="_blank"
                style="display:inline-flex;align-items:center;gap:4px;background:#f5f3ff;color:#6d28d9;border-color:#ddd6fe"
            {
                (icons::arrow_up_right()) " Explore WASM blocks"
            }
            a .btn .btn-secondary .btn-sm href="/b/inspector/ui" target="_blank" {
                (icons::globe()) " Open Inspector"
            }
        }
    };

    let tabs_and_body = html! {
        (tab_navigation(vec![
            Tab {
                active: active_tab == "features",
                href: "/b/admin/blocks",
                label: "Features",
                icon: Some(icons::package()),
            },
            Tab {
                active: active_tab == "services",
                href: "/b/admin/blocks?tab=services",
                label: "Services",
                icon: Some(icons::server()),
            },
            Tab {
                active: active_tab == "infrastructure",
                href: "/b/admin/blocks?tab=infrastructure",
                label: "Infrastructure",
                icon: Some(icons::settings()),
            },
            Tab {
                active: active_tab == "custom",
                href: "/b/admin/blocks?tab=custom",
                label: "Custom",
                icon: Some(icons::package()),
            },
        ]))

        div #blocks-tab-content {
            @if active_tab == "custom" {
                (custom_tab_content())
            } @else {
                @let runtime_filter = msg.query("runtime");
                @let filtered: Vec<_> = all_blocks.iter().filter(|b| {
                    let cat_match = match active_tab {
                        "services" => b.category == wafer_run::BlockCategory::Service,
                        "infrastructure" => b.category == wafer_run::BlockCategory::Infrastructure,
                        _ => b.category == wafer_run::BlockCategory::Feature,
                    };
                    cat_match && match runtime_filter {
                        "native" => b.runtime == wafer_run::BlockRuntime::Native,
                        "wasm" => b.runtime == wafer_run::BlockRuntime::Wasm,
                        _ => true,
                    }
                }).collect();

                // Runtime filter dropdown
                div .block-cards__filter {
                    select .form-input
                        onchange={"window.location.href='/b/admin/blocks?tab=" (active_tab) "&runtime='+this.value"}
                    {
                        option value="" selected[runtime_filter.is_empty()] { "All runtimes" }
                        option value="native" selected[runtime_filter == "native"] { "Native only" }
                        option value="wasm" selected[runtime_filter == "wasm"] { "WASM only" }
                    }
                }

                @if filtered.is_empty() {
                    (empty_state(
                        icons::package(),
                        "No blocks",
                        "No blocks registered in this category.",
                        None,
                    ))
                }

                div .block-cards {
                    @for block in &filtered {
                        @let is_enabled = block_enabled.get(&block.name).copied().unwrap_or(true);
                        @let encoded_name = encode_block_name(&block.name);
                        div class={ "block-card" @if !is_enabled { " block-card--disabled" } }
                            hx-get={"/b/admin/blocks/" (encoded_name) "/detail"}
                            hx-target="#block-detail-modal"
                            hx-swap="innerHTML"
                        {
                            div .block-card__head {
                                h3 .block-card__title { (block.name) }
                                @if is_enabled {
                                    span .block-card__check title="Enabled" { "\u{2713}" }
                                } @else {
                                    span .block-card__check .block-card__check--off title="Disabled" { "\u{2717}" }
                                }
                            }
                            p .block-card__summary { (block.summary) }
                            div .block-card__meta {
                                @if block.runtime == wafer_run::BlockRuntime::Wasm {
                                    span .block-card__runtime .block-card__runtime--wasm { "WASM" }
                                } @else {
                                    span .block-card__runtime { "Native" }
                                }
                                span .block-card__version { "v" (block.version) }
                                @if is_enabled && !block.admin_url.is_empty() {
                                    a .btn .btn-sm .btn-primary .block-card__open
                                        href=(block.admin_url)
                                        onclick="event.stopPropagation()"
                                    { "Open" }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Block detail modal (content loaded via htmx)
        div .modal-overlay #block-detail-modal-overlay hidden
            onclick="if(event.target===this)closeModal('block-detail-modal-overlay')"
        {
            div .modal style="max-width:700px;max-height:85vh;overflow-y:auto" {
                div #block-detail-modal {}
            }
        }
    };

    let body = list_page(
        PageHeader {
            title: "",
            subtitle: None,
            primary_action: None,
        },
        None,
        tabs_and_body,
        None,
    );

    admin_page(
        "Blocks",
        &config,
        "/b/admin/blocks",
        user.as_ref(),
        Topbar {
            crumbs: crumb("Blocks"),
            primary_action: Some(page_action),
            subtitle: Some("Registered WAFER blocks"),
            show_palette: true,
        },
        body,
        msg,
    )
}

/// POST /b/admin/blocks/{name}/toggle -- toggle a block's enabled state
pub async fn handle_toggle_feature(
    ctx: &dyn Context,
    msg: &Message,
    block_name: &str,
) -> OutputStream {
    // Read current state and toggle via shared helper (audit finding #12).
    let current_enabled = super::super::settings::block_settings::is_enabled(ctx, block_name).await;
    let new_enabled = !current_enabled;
    let _ = super::super::settings::block_settings::set_enabled(ctx, block_name, new_enabled).await;

    let admin_id = msg.user_id().to_string();
    let ip = msg.remote_addr().to_string();
    let action = if new_enabled {
        "block.enable"
    } else {
        "block.disable"
    };
    super::super::logs::audit_log(ctx, &admin_id, action, &format!("blocks/{block_name}"), &ip)
        .await;

    // Re-render the blocks page
    blocks_page(ctx, msg).await
}

/// GET /b/admin/blocks/{name}/detail -- block detail modal content
pub async fn handle_block_detail(
    ctx: &dyn Context,
    _msg: &Message,
    block_name: &str,
) -> OutputStream {
    let blocks = ctx.registered_blocks();
    let block_opt = blocks.iter().find(|b| b.name == block_name);

    // Check block enabled state via shared helper (audit finding #12).
    let is_enabled = super::super::settings::block_settings::is_enabled(ctx, block_name).await;

    let encoded = encode_block_name(block_name);

    // Disabled block not in runtime -- show minimal modal with toggle.
    let Some(block) = block_opt else {
        let markup = html! {
            div .modal-header {
                h3 .modal-title { (block_name) }
                button .modal-close onclick="closeModal('block-detail-modal-overlay')" {
                    (icons::x())
                }
            }
            div .modal-body {
                div .flex .items-center .justify-between .mb-4 {
                    span .text-muted {
                        @if is_enabled {
                            "This block is enabled but not loaded. Restart the server to load it."
                        } @else {
                            "This block is currently disabled."
                        }
                    }
                    label .toggle {
                        input type="checkbox"
                            checked[is_enabled]
                            hx-post={"/b/admin/blocks/" (encoded) "/toggle"}
                            hx-target="#content";
                        span .toggle-slider {}
                    }
                }
                p style="font-size:0.875rem;color:var(--text-muted);margin-top:1rem" {
                    @if is_enabled {
                        "Restart the server to see its full details."
                    } @else {
                        "Enable and restart the server to load this block and see its full details."
                    }
                }
            }
            script { (maud::PreEscaped("document.getElementById('block-detail-modal-overlay').removeAttribute('hidden');")) }
        };
        return ui::html_response(markup);
    };

    let markup = html! {
        div .modal-header {
            div {
                div .flex .items-center .gap-2 {
                    h3 .modal-title { (block.name) }
                    span .badge .badge-info style="font-size:11px" { "v" (block.version) }
                    span .badge style="font-size:11px;background:#f1f5f9;color:#475569" { (format!("{:?}", block.category)) }
                }
            }
            button .modal-close onclick="closeModal('block-detail-modal-overlay')" {
                (icons::x())
            }
        }
        div .modal-body {
            // Admin UI link + Block toggle (above description)
            div .flex .items-center .justify-between .mb-4 {
                div .flex .items-center .gap-2 {
                    @if is_enabled && !block.admin_url.is_empty() {
                        a .btn .btn-sm .btn-primary href=(block.admin_url) {
                            (icons::settings()) " Open Admin UI"
                        }
                    }
                }
                @if block.can_disable {
                    div .flex .items-center .gap-2 {
                        span .text-sm .text-muted { "Enabled" }
                        label .toggle {
                            @let encoded = encode_block_name(&block.name);
                            input type="checkbox"
                                checked[is_enabled]
                                hx-post={"/b/admin/blocks/" (encoded) "/toggle"}
                                hx-target="#content";
                            span .toggle-slider {}
                        }
                    }
                } @else {
                    span .text-sm .text-muted { "Always enabled (core block)" }
                }
            }

            // Description
            @if !block.description.is_empty() {
                p style="font-size:0.875rem;color:#64748b;line-height:1.6;margin-bottom:1rem" { (block.description) }
            }

            // Endpoints
            @if !block.endpoints.is_empty() {
                h4 style="font-size:0.875rem;font-weight:600;margin:1rem 0 0.5rem" { "Endpoints" }
                div .table-container {
                    table .table {
                        thead {
                            tr {
                                th style="width:70px" { "Method" }
                                th { "Path" }
                                th { "Description" }
                                th style="width:80px" { "Auth" }
                            }
                        }
                        tbody {
                            @for ep in &block.endpoints {
                                tr {
                                    td {
                                        span .badge style={"font-size:11px;" (match ep.method {
                                            wafer_run::HttpMethod::Get => "background:#dbeafe;color:#1d4ed8",
                                            wafer_run::HttpMethod::Post => "background:#dcfce7;color:#166534",
                                            wafer_run::HttpMethod::Patch => "background:#fef3c7;color:#92400e",
                                            wafer_run::HttpMethod::Delete => "background:#fce4ec;color:#c62828",
                                        })} { (ep.method) }
                                    }
                                    td .text-sm { code style="font-size:12px" { (ep.path) } }
                                    td .text-sm .text-muted { (ep.summary) }
                                    td {
                                        span .badge style={"font-size:10px;" (match ep.auth {
                                            wafer_run::AuthLevel::Public => "background:#dcfce7;color:#166534",
                                            wafer_run::AuthLevel::Admin => "background:#fce4ec;color:#c62828",
                                            wafer_run::AuthLevel::Authenticated => "background:#fef3c7;color:#92400e",
                                        })} { (ep.auth) }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Config Keys
            @if !block.config_keys.is_empty() {
                h4 style="font-size:0.875rem;font-weight:600;margin:1rem 0 0.5rem" { "Configuration" }
                div .table-container {
                    table .table {
                        thead {
                            tr {
                                th { "Key" }
                                th { "Description" }
                                th { "Default" }
                            }
                        }
                        tbody {
                            @for ck in &block.config_keys {
                                tr {
                                    td { code style="font-size:12px" { (ck.key) } }
                                    td .text-sm .text-muted { (ck.description) }
                                    td .text-sm { code style="font-size:11px" { @if ck.default.is_empty() { "\u{2014}" } @else { (ck.default) } } }
                                }
                            }
                        }
                    }
                }
            }

            // Technical details
            h4 style="font-size:0.875rem;font-weight:600;margin:1rem 0 0.5rem" { "Technical" }
            div style="font-size:13px;color:#64748b" {
                div .mb-2 {
                    b { "Interface: " }
                    span .badge style="font-size:11px;background:#f1f5f9;color:#475569" { (block.interface) }
                }
                @if !block.requires.is_empty() {
                    div .mb-2 {
                        b { "Requires: " }
                        @for req in &block.requires {
                            span .badge .badge-primary style="font-size:11px;margin-right:4px" { (req) }
                        }
                    }
                }
                @if !block.collections.is_empty() {
                    div .mb-2 {
                        b { "Database tables: " }
                        @for col in &block.collections {
                            span .badge style="font-size:11px;margin-right:4px;background:#f1f5f9;color:#475569" { (col.name) }
                        }
                    }
                }
            }
        }
        // Auto-open
        script { (maud::PreEscaped("document.getElementById('block-detail-modal-overlay').removeAttribute('hidden');")) }
    };

    ui::html_response(markup)
}

// ---------------------------------------------------------------------------
// Custom tab
// ---------------------------------------------------------------------------

/// Informational notice for the Custom tab. Local deployments discover
/// custom blocks from the `blocks/` directory at startup — there is no
/// runtime install/upload surface.
fn custom_tab_content() -> maud::Markup {
    html! {
        div .custom-tab {
            section .card {
                header .card__head {
                    h3 .card__title { (icons::package()) " Custom Blocks" }
                }
                div .card__body {
                    p .custom-tab__hint {
                        "Custom blocks are auto-discovered from the "
                        code { "blocks/" }
                        " directory. Use "
                        code { "wafer build" }
                        " to compile blocks locally, then restart the server. Browse the "
                        a href="https://wafer.run/registry" target="_blank" { "WAFER registry" }
                        " for available WASM blocks."
                    }
                }
            }
        }
    }
}
