use std::sync::{Arc, RwLock};

use maud::{html, Markup};
use wafer_run::{context::Context, Message, OutputStream};

use super::{admin_page, crumb};
use crate::{
    blocks::crud,
    features::BlockSettings,
    platform_state::block_settings,
    ui::{
        self,
        components::{self, empty_state, tab_navigation, Badge, BadgeVariant, Tab},
        icons,
        shell::Topbar,
        templates::list_page,
    },
};

/// Encode a block name (`org/block`) for use as a URL path segment. The
/// public admin URLs use `--` as the separator so the name occupies one
/// segment and the route table's `{name}` binds it whole.
fn encode_block_name(name: &str) -> String {
    name.replace('/', "--")
}

/// Inverse of [`encode_block_name`]: the `{name}` segment the route table
/// bound, back to the registered `org/block` form.
fn decode_block_name(encoded: &str) -> String {
    encoded.replace("--", "/")
}

/// The runtime-filter `<select>`'s behaviour, delegated.
///
/// It used to be an `onchange` attribute that concatenated the active tab
/// straight into a URL inside a JavaScript string literal. `active_tab` is one
/// of four literals so nothing could break out, but the shape is the one
/// `blocks/admin/pages/network.rs` warns about, and it also skipped
/// percent-encoding. The tab now travels as a `data-blocks-tab` operand and
/// `URLSearchParams` builds the query.
const RUNTIME_FILTER_JS: &str = r#"
(function () {
  if (window.__blocksRuntimeFilterInit) return;
  window.__blocksRuntimeFilterInit = true;
  document.addEventListener('change', function (e) {
    var el = e.target;
    if (!(el instanceof Element)) return;
    if (el.getAttribute('data-action') !== 'blocks-runtime-filter') return;
    var params = new URLSearchParams();
    params.set('tab', el.getAttribute('data-blocks-tab') || '');
    params.set('runtime', el.value);
    window.location.href = '/b/admin/blocks?' + params.toString();
  });
})();
"#;

pub async fn blocks_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
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
    // An empty settings table means "nobody has toggled anything", which the
    // map below renders as every block ENABLED — the state an untouched
    // deployment is in. A failed read must not be allowed to say that: an
    // operator checking whether they had disabled a block would be told they
    // had not.
    let block_settings_rows = match block_settings::list_all(ctx).await {
        Ok(rows) => rows,
        Err(e) => {
            return crud::db_error_page(msg, e, "admin blocks page: block-settings read failed")
        }
    };

    let block_enabled: std::collections::BTreeMap<String, bool> = block_settings_rows
        .iter()
        .map(|row| (row.block_name.clone(), row.enabled))
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
        div .flex .gap-2 {
            a .btn .btn--sm .btn--secondary .gap-1 href="https://wafer.run/registry" target="_blank"
            {
                (icons::arrow_up_right()) " Explore WASM blocks"
            }
            a .btn .btn--secondary .btn--sm href="/b/inspector/ui" target="_blank" {
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
                        data-action="blocks-runtime-filter"
                        data-blocks-tab=(active_tab)
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
                                h2 .block-card__title { (block.name) }
                                @if is_enabled {
                                    span .block-card__check title="Enabled" { (ui::icons::check()) }
                                } @else {
                                    span .block-card__check .block-card__check--off title="Disabled" { (ui::icons::x()) }
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
                                    a .btn .btn--sm .btn--primary .block-card__open
                                        href=(block.admin_url)
                                        data-stop-propagation
                                    { "Open" }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Block detail modal (content loaded via htmx)
        div .modal-overlay #block-detail-modal-overlay hidden data-modal-dismiss
        {
            div .modal .modal--lg {
                div #block-detail-modal {}
            }
        }

        script { (maud::PreEscaped(RUNTIME_FILTER_JS)) }
    };

    let body = list_page(None, tabs_and_body, None);

    admin_page(
        ctx,
        msg,
        "Blocks",
        Topbar {
            crumbs: crumb("Blocks"),
            primary_action: Some(page_action),
            subtitle: Some("Registered WAFER blocks"),
            show_palette: true,
        },
        body,
    )
    .await
}

/// `POST /b/admin/blocks/{name}/toggle` -- toggle a block's enabled state.
/// `{name}` is the `--`-encoded block name, read only as the route table
/// bound it.
pub async fn handle_toggle_feature(
    ctx: &dyn Context,
    msg: &Message,
    block_settings_handle: &Arc<RwLock<BlockSettings>>,
) -> OutputStream {
    let block_name = decode_block_name(msg.var("name"));
    let block_name = block_name.as_str();
    // One read answers both questions below: whether this name exists at all,
    // and what it is set to now. An unreadable state is an error, not
    // "enabled": the write below is derived from it, so a guess here would
    // flip the block off the back of an outage (audit finding #12).
    let rows = match block_settings::list_all(ctx).await {
        Ok(rows) => rows,
        Err(e) => return crud::db_error_internal(e, "Failed to read block setting"),
    };
    let row = rows.iter().find(|r| r.block_name == block_name);

    // `decode_block_name` only swaps `--` for `/`, so the name is entirely
    // caller-controlled and `set_enabled` upserts. An unchecked name both
    // mints rows nothing ever reaps and writes rows that must never exist.
    //
    // A REGISTERED block is toggleable only if it declares `can_disable`.
    // `BlockInfo::new` defaults that to `false` and `blocks::block_enabled_defaults`
    // filters the seed on it, so `impresspress/admin`, `impresspress/system`,
    // `impresspress/email`, `auth-ui` and the `wafer-run/*` middleware hold no
    // row at all — `blocks/mod.rs` records that "nothing can create one", and
    // the detail fragment renders them no toggle (see the `can_disable` gate
    // below). Admin is the dangerous one: `/b/admin/` is gated on
    // `impresspress/admin` (`routing.rs`), so a row at `enabled = 0` 404s
    // every admin route from the next boot, and `set_enabled` would also
    // stamp `USER_EDITED_SENTINEL` over the `seed_defaults_hash` column that
    // `admin::settings::seed_defaults` owns in a different format. The panel
    // that could undo it is the panel that just disappeared.
    //
    // NOT registered is still legitimate when a row already exists:
    // `blocks_page` deliberately lists those as unloaded ("restart to load")
    // and renders them a toggle from a placeholder declaring `can_disable(true)`.
    let toggleable = match ctx
        .registered_blocks()
        .iter()
        .find(|b| b.name == block_name)
    {
        Some(info) => info.can_disable,
        None => row.is_some(),
    };
    if !toggleable {
        return crate::http::err_not_found("Unknown block");
    }

    // No row ⇒ enabled, matching what `is_enabled` reports for a missing row.
    let current_enabled = row.is_none_or(|r| r.enabled);
    let new_enabled = !current_enabled;

    // Persist first. Only write the audit event — and only re-render the
    // page as a success — after a confirmed successful write. Previously
    // `let _ = set_enabled(..)` discarded the persistence result, so a
    // failed toggle still logged "block.enable"/"block.disable" as if it
    // had happened and re-rendered the page showing the new (unpersisted)
    // state.
    if let Err(e) = block_settings::set_enabled(ctx, block_name, new_enabled).await {
        return crud::db_error_internal(e, "Failed to persist block setting");
    }

    // Then the LIVE snapshot, in that order. `routing::route_to_block` gates
    // every route on the router's `Arc<dyn FeatureConfig>` — this same
    // `Arc<RwLock<BlockSettings>>` — and reads it per request, so without
    // this the toggle reached the table and stopped there. On native nothing
    // re-reads that table after `build()` (`NativeBootHooks::seed_after_admin_init`
    // is empty), so the router kept serving a disabled block until the
    // process restarted, while the blocks page — which reads the table —
    // showed it off.
    //
    // After the persist, never before: a failed write must not leave the
    // snapshot claiming a state the database does not hold, which is the
    // same ordering the audit row below already follows.
    //
    // Cloudflare and the browser reach the same place by their own routes
    // (a config-version bump rebuilds the writing isolate; the browser
    // republishes at boot), so this is one update that is correct on every
    // target rather than a native special case.
    match block_settings_handle.write() {
        Ok(mut settings) => settings.set_block_enabled(block_name, new_enabled),
        // Poisoned only if another holder panicked mid-write. That is already
        // terminal for routing, not a degraded mode: `impl FeatureConfig for
        // RwLock<BlockSettings>` reads with `.expect("BlockSettings RwLock
        // poisoned")`, so every routed request panics from here on, whatever
        // this handler does. Panicking again here would add nothing and lose
        // the one useful fact — the row IS written — so the toggle reports
        // the database success it actually achieved and says so in the log.
        Err(e) => {
            tracing::error!(
                block = %block_name,
                error = %e,
                "block settings snapshot poisoned; the live gate keeps its old value until restart"
            );
        }
    }

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

/// `GET /b/admin/blocks/{name}/detail` -- block detail modal content.
/// `{name}` is the `--`-encoded block name, read only as the route table
/// bound it.
pub async fn handle_block_detail(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let block_name = decode_block_name(msg.var("name"));
    let block_name = block_name.as_str();
    let blocks = ctx.registered_blocks();
    let block_opt = blocks.iter().find(|b| b.name == block_name);

    // Check block enabled state via shared helper (audit finding #12).
    let is_enabled = match block_settings::is_enabled(ctx, block_name).await {
        Ok(enabled) => enabled,
        Err(e) => return crud::db_error_internal(e, "Failed to read block setting"),
    };

    let encoded = encode_block_name(block_name);

    // Disabled block not in runtime -- show minimal modal with toggle.
    let Some(block) = block_opt else {
        let markup = html! {
            div .modal-header {
                h3 .modal-title { (block_name) }
                button .modal-close data-action="modal-close" data-modal-target="block-detail-modal-overlay" {
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
                p .modal-note {
                    @if is_enabled {
                        "Restart the server to see its full details."
                    } @else {
                        "Enable and restart the server to load this block and see its full details."
                    }
                }
            }
        };
        return ui::html_response_opening_modal(markup, "block-detail-modal-overlay");
    };

    let markup = html! {
        div .modal-header {
            div {
                div .flex .items-center .gap-2 {
                    h3 .modal-title { (block.name) }
                    (Badge::new(BadgeVariant::Info).classes("text-11").render(html! { "v" (block.version) }))
                    (Badge::new(BadgeVariant::ToneSlate).classes("text-11").render(html! { (format!("{:?}", block.category)) }))
                }
            }
            button .modal-close data-action="modal-close" data-modal-target="block-detail-modal-overlay" {
                (icons::x())
            }
        }
        div .modal-body {
            // Admin UI link + Block toggle (above description)
            div .flex .items-center .justify-between .mb-4 {
                div .flex .items-center .gap-2 {
                    @if is_enabled && !block.admin_url.is_empty() {
                        a .btn .btn--sm .btn--primary href=(block.admin_url) {
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
                p .modal-description { (block.description) }
            }

            // Endpoints
            @if !block.endpoints.is_empty() {
                h4 .modal-section-title { "Endpoints" }
                @let rows: Vec<Vec<Markup>> = block.endpoints.iter().map(|ep| vec![
                    Badge::new(method_badge_tone(ep.method)).classes("text-11").render(html! { (ep.method) }),
                    html! { code .text-xs { (ep.path) } },
                    html! { span .text-muted { (ep.summary) } },
                    Badge::new(auth_badge_tone(ep.auth)).classes("text-10").render(html! { (ep.auth) }),
                ]).collect();

                (components::data_table::<fn(usize) -> Option<String>>(
                    &ENDPOINT_COLUMNS,
                    rows,
                    None,
                    html! {},
                ))
            }

            // Config Keys
            @if !block.config_keys.is_empty() {
                h4 .modal-section-title { "Configuration" }
                @let rows: Vec<Vec<Markup>> = block.config_keys.iter().map(|ck| vec![
                    html! { code .text-xs { (ck.key) } },
                    html! { span .text-muted { (ck.description) } },
                    html! { code .text-11 { @if ck.default.is_empty() { "\u{2014}" } @else { (ck.default) } } },
                ]).collect();

                (components::data_table::<fn(usize) -> Option<String>>(
                    &CONFIG_KEY_COLUMNS,
                    rows,
                    None,
                    html! {},
                ))
            }

            // Technical details
            h4 .modal-section-title { "Technical" }
            div .modal-tech {
                div .mb-2 {
                    b { "Interface: " }
                    (Badge::new(BadgeVariant::ToneSlate).classes("text-11").render(html! { (block.interface) }))
                }
                @if !block.requires.is_empty() {
                    div .mb-2 {
                        b { "Requires: " }
                        @for req in &block.requires {
                            (Badge::new(BadgeVariant::Primary).classes("text-11 mr-1").render(html! { (req) }))
                        }
                    }
                }
                @if !block.collections.is_empty() {
                    div .mb-2 {
                        b { "Database tables: " }
                        @for col in &block.collections {
                            (Badge::new(BadgeVariant::ToneSlate).classes("text-11 mr-1").render(html! { (col.name) }))
                        }
                    }
                }
            }
        }
    };

    ui::html_response_opening_modal(markup, "block-detail-modal-overlay")
}

/// Tone variant for an endpoint's HTTP-method badge. Shares its colour set with
/// [`auth_badge_tone`] — `Post`/`Public` and `Patch`/`Authenticated` render
/// identically, so the tones live once in `styles/components/badge.css`
/// rather than being declared per-enum.
fn method_badge_tone(method: wafer_run::HttpMethod) -> BadgeVariant {
    match method {
        wafer_run::HttpMethod::Get => BadgeVariant::ToneBrand,
        wafer_run::HttpMethod::Post => BadgeVariant::ToneGreen,
        wafer_run::HttpMethod::Patch => BadgeVariant::ToneAmber,
        wafer_run::HttpMethod::Delete => BadgeVariant::ToneRed,
    }
}

/// Tone variant for an endpoint's auth-level badge. See [`method_badge_tone`].
fn auth_badge_tone(auth: wafer_run::AuthLevel) -> BadgeVariant {
    match auth {
        wafer_run::AuthLevel::Public => BadgeVariant::ToneGreen,
        wafer_run::AuthLevel::Admin => BadgeVariant::ToneRed,
        wafer_run::AuthLevel::Authenticated => BadgeVariant::ToneAmber,
    }
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
                    h2 .card__title { (icons::package()) " Custom Blocks" }
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

/// The block-detail modal's two tables' columns. Declared once each so the
/// `<td data-label>` the component stamps on every cell names the same column
/// its header does; the two widths are the ones the old `th .w-70` / `.w-80`
/// utility classes gave those headers.
const ENDPOINT_COLUMNS: [components::TableCol<'static>; 4] = [
    components::TableCol {
        label: "Method",
        width: Some("70px"),
    },
    components::TableCol {
        label: "Path",
        width: None,
    },
    components::TableCol {
        label: "Description",
        width: None,
    },
    components::TableCol {
        label: "Auth",
        width: Some("80px"),
    },
];

const CONFIG_KEY_COLUMNS: [components::TableCol<'static>; 3] = [
    components::TableCol {
        label: "Key",
        width: None,
    },
    components::TableCol {
        label: "Description",
        width: None,
    },
    components::TableCol {
        label: "Default",
        width: None,
    },
];

/// Regression coverage for the swallowed-failure finding: block enable/disable
/// must check the persistence result instead of discarding it
/// (`let _ = set_enabled(..)`), and must only write the audit-log row after a
/// confirmed successful write — a failed persist must not report success or
/// log "block.enable"/"block.disable" as if it happened.
#[cfg(test)]
mod toggle_feature_tests {

    use super::*;
    use crate::{
        blocks::admin::test_support::routed,
        test_support::{admin_msg, output_is_error, FailingDbOpContext, TestContext},
    };

    /// `POST /b/admin/blocks/impresspress--files/toggle`, with `{name}` bound
    /// by the table the way it is on the wire.
    fn toggle_files_msg() -> Message {
        routed(admin_msg(
            "create",
            "/b/admin/blocks/impresspress--files/toggle",
        ))
    }

    /// `with_admin` registers no blocks, but [`blocks_page`] only ever offers
    /// a toggle for a block that is registered or already carries a row — so
    /// a fixture toggling an unregistered, row-less name models a request the
    /// product cannot produce. Registering the `BlockInfo` puts these tests
    /// back on the path the page actually drives.
    /// `can_disable(true)` is not decoration: `BlockInfo::new` defaults it to
    /// `false`, and the handler refuses to toggle a block that does not
    /// declare it. The real `impresspress/files` declares it, so a fixture
    /// that left it off would model a block the product does not have.
    async fn ctx_with_files_registered() -> TestContext {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.register_block_info(
            "impresspress/files",
            wafer_run::BlockInfo::new("impresspress/files", "1.0.0", "http.handler", "files")
                .can_disable(true),
        );
        ctx
    }

    /// An enablement snapshot nothing else reads.
    ///
    /// These tests assert the DATABASE effect of a toggle. The live-snapshot
    /// effect — the handle the router reads per request — has its own test
    /// against a wired `AdminBlock`
    /// (`a_toggle_updates_the_live_enablement_snapshot`), because it is the
    /// block, not this handler, that owns the wiring.
    fn unwired_handle() -> Arc<RwLock<BlockSettings>> {
        Arc::new(RwLock::new(BlockSettings::default()))
    }

    async fn audit_count(ctx: &dyn Context, action: &str) -> usize {
        crate::db_read::list_every(
            ctx,
            crate::blocks::admin::AUDIT_LOGS_TABLE,
            vec![wafer_block::db::Filter {
                field: "action".to_string(),
                operator: wafer_block::db::FilterOp::Equal,
                value: serde_json::Value::String(action.to_string()),
            }],
        )
        .await
        .map(|rows| rows.len())
        .unwrap_or(0)
    }

    #[tokio::test]
    async fn toggle_success_persists_and_audits() {
        let ctx = ctx_with_files_registered().await;

        assert!(
            block_settings::is_enabled(&ctx, "impresspress/files")
                .await
                .expect("read block setting"),
            "no row yet ⇒ defaults enabled"
        );

        let _ = handle_toggle_feature(&ctx, &toggle_files_msg(), &unwired_handle())
            .await
            .collect_buffered()
            .await
            .expect("toggle against a healthy database must succeed");

        assert!(
            !block_settings::is_enabled(&ctx, "impresspress/files")
                .await
                .expect("read block setting"),
            "toggle must have persisted the disabled state"
        );
        assert_eq!(audit_count(&ctx, "block.disable").await, 1);
    }

    /// The state written by a toggle is derived from the state read. When
    /// that read fails, the handler must refuse rather than assume "enabled"
    /// and write "disabled": no row changes, no audit event claims it did.
    #[tokio::test]
    async fn toggle_refuses_when_current_state_cannot_be_read() {
        let ctx = ctx_with_files_registered().await;
        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.list", block_settings::TABLE)]);
        let out = handle_toggle_feature(&failing, &toggle_files_msg(), &unwired_handle()).await;

        assert!(
            output_is_error(out, "Internal").await,
            "an unreadable block state must surface as an error"
        );
        assert!(
            block_settings::is_enabled(&ctx, "impresspress/files")
                .await
                .expect("read block setting"),
            "nothing may be written when the current state could not be read"
        );
        assert_eq!(audit_count(&ctx, "block.disable").await, 0);
        assert_eq!(audit_count(&ctx, "block.enable").await, 0);
    }

    /// The core regression: a genuine persistence failure during the toggle
    /// must surface as an error response and must NOT write a
    /// "block.enable"/"block.disable" audit row claiming success. Before this
    /// fix, `let _ = set_enabled(..)` discarded the error and the handler
    /// unconditionally logged the audit event and re-rendered the blocks page
    /// as if the toggle had taken effect.
    #[tokio::test]
    async fn toggle_persist_failure_returns_error_without_audit() {
        let ctx = ctx_with_files_registered().await.break_writes();

        let out = handle_toggle_feature(&ctx, &toggle_files_msg(), &unwired_handle()).await;
        assert!(
            crate::test_support::output_is_error(out, "Internal").await,
            "a genuine persistence failure must surface as an error, not a fabricated success"
        );

        assert_eq!(
            audit_count(&ctx, "block.disable").await,
            0,
            "a failed persist must not write a success audit row"
        );
        assert_eq!(
            audit_count(&ctx, "block.enable").await,
            0,
            "a failed persist must not write a success audit row"
        );
    }

    /// A name matching no registered block and no existing row is a typo —
    /// and `upsert_fields` mints a row for whatever it is handed, forever,
    /// since nothing ever reaps them. `blocks_page` then lists that phantom
    /// as an unloaded block on every visit. `decode_block_name` does nothing
    /// but swap `--` for `/`, so the name is entirely caller-controlled.
    #[tokio::test]
    async fn toggle_rejects_an_unknown_block_and_mints_no_row() {
        let ctx = ctx_with_files_registered().await;
        let msg = routed(admin_msg(
            "create",
            "/b/admin/blocks/impresspress--fyles/toggle",
        ));

        let out = handle_toggle_feature(&ctx, &msg, &unwired_handle()).await;
        assert!(
            output_is_error(out, "NotFound").await,
            "an unknown block name must be refused, not written",
        );

        let rows = block_settings::list_all(&ctx).await.expect("list rows");
        let names: Vec<&str> = rows.iter().map(|r| r.block_name.as_str()).collect();
        assert!(
            !names.contains(&"impresspress/fyles"),
            "a typo must not leave a permanent phantom row: {names:?}",
        );
        assert_eq!(audit_count(&ctx, "block.disable").await, 0);
    }

    /// Registration is not enough on its own either: a registered block is
    /// toggleable only if it declares `can_disable`.
    ///
    /// `BlockInfo::new` defaults that to `false`, so admin, system, email,
    /// auth-ui and the `wafer-run/*` middleware all pass a registration-only
    /// check. Admin is the one that bites: `/b/admin/` is gated on
    /// `impresspress/admin`, so persisting `enabled = 0` for it 404s every
    /// admin route from the next boot — including the page that would undo
    /// the toggle — and stamps `USER_EDITED_SENTINEL` over a
    /// `seed_defaults_hash` column owned by `seed_defaults` in another
    /// format, which the seed then refuses to repair because the sentinel
    /// marks the row user-owned.
    #[tokio::test]
    async fn toggle_refuses_a_block_that_cannot_be_disabled() {
        let mut ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        // `BlockInfo::new` leaves `can_disable` false — the same shape the
        // real admin block registers with.
        ctx.register_block_info(
            "impresspress/admin",
            wafer_run::BlockInfo::new("impresspress/admin", "1.0.0", "http.handler", "admin"),
        );
        let msg = routed(admin_msg(
            "create",
            "/b/admin/blocks/impresspress--admin/toggle",
        ));

        let out = handle_toggle_feature(&ctx, &msg, &unwired_handle()).await;
        assert!(
            output_is_error(out, "NotFound").await,
            "a block that cannot be disabled must not be toggleable",
        );

        // `with_admin` already stamps admin's own migration row, so the
        // assertion is that its enablement is untouched, not that no row
        // exists.
        assert!(
            block_settings::is_enabled(&ctx, "impresspress/admin")
                .await
                .expect("read block setting"),
            "admin must not have been disabled",
        );
        assert_eq!(audit_count(&ctx, "block.disable").await, 0);
    }

    /// Validation cannot be registration alone. [`blocks_page`] deliberately
    /// lists blocks that hold a row but are not registered ("(disabled —
    /// restart to load)") and leaves them toggleable, so an operator must
    /// still be able to re-enable one.
    #[tokio::test]
    async fn toggle_still_works_for_an_unloaded_block_with_a_row() {
        let ctx = ctx_with_files_registered().await;
        block_settings::upsert_fields(
            &ctx,
            "impresspress/unloaded",
            block_settings::BlockSettingsPatch {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("seed an unloaded block's row");

        let msg = routed(admin_msg(
            "create",
            "/b/admin/blocks/impresspress--unloaded/toggle",
        ));
        let _ = handle_toggle_feature(&ctx, &msg, &unwired_handle())
            .await
            .collect_buffered()
            .await
            .expect("an unloaded block with a row stays toggleable");

        assert!(
            block_settings::is_enabled(&ctx, "impresspress/unloaded")
                .await
                .expect("read block setting"),
            "the toggle must have re-enabled the unloaded block",
        );
    }
}

#[cfg(test)]
mod badge_tone_tests {
    use maud::html;

    use super::*;

    /// The block-detail modal is the only place these two colour sets render,
    /// and the seeded block in `page_link_tests` declares no endpoints, so no
    /// page render exercises them. Pinned here against the exact class each
    /// arm emitted before the tones became `BadgeVariant` values.
    #[test]
    fn method_and_auth_tones_render_the_classes_they_always_did() {
        let rendered = |variant| {
            Badge::new(variant)
                .classes("text-11")
                .render(html! { "x" })
                .into_string()
        };
        for (method, class) in [
            (wafer_run::HttpMethod::Get, "badge--tone-brand"),
            (wafer_run::HttpMethod::Post, "badge--tone-green"),
            (wafer_run::HttpMethod::Patch, "badge--tone-amber"),
            (wafer_run::HttpMethod::Delete, "badge--tone-red"),
        ] {
            assert_eq!(
                rendered(method_badge_tone(method)),
                format!(r#"<span class="badge {class} text-11">x</span>"#),
                "{method:?}"
            );
        }
        for (auth, class) in [
            (wafer_run::AuthLevel::Public, "badge--tone-green"),
            (wafer_run::AuthLevel::Admin, "badge--tone-red"),
            (wafer_run::AuthLevel::Authenticated, "badge--tone-amber"),
        ] {
            assert_eq!(
                rendered(auth_badge_tone(auth)),
                format!(r#"<span class="badge {class} text-11">x</span>"#),
                "{auth:?}"
            );
        }
    }
}

#[cfg(test)]
mod outage_tests {
    //! An unreadable `block_settings` table used to render every block as
    //! ENABLED — the toggle state an untouched deployment has — so an
    //! operator checking whether they had disabled a block was told they had
    //! not.

    use super::*;
    use crate::test_support::{admin_msg, output_http_status, TestContext};

    #[tokio::test]
    async fn a_failing_block_settings_read_renders_the_error_page_not_all_enabled() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID)
            .break_reads();
        let msg = admin_msg("retrieve", "/b/admin/blocks");
        assert_eq!(output_http_status(blocks_page(&ctx, &msg).await).await, 500);
    }
}
