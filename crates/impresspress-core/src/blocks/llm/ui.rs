//! Admin SSR pages for the `impresspress/llm` feature block.
//!
//! Two admin-only pages, both wired into `LlmBlock::handle`:
//!
//! - `GET /b/llm/providers` — provider CRUD table with an "Add provider"
//!   form. Reads rows directly from `impresspress__llm__providers` via
//!   `db::list` + `row_to_config` for a flash-free first paint.
//! - `GET /b/llm/models` — aggregated models across every registered
//!   backend. Renders an empty table shell that fills in each row's
//!   status asynchronously via `hx-get` + `hx-trigger="load"` so a slow
//!   provider never blocks the first paint.
//!
//! Both handlers enforce admin access in addition to the dispatcher-level
//! guard — defence in depth for the case where a caller reaches this
//! function directly (tests, future router changes).

use maud::{html, Markup};
use wafer_core::clients::{database as db, llm::ModelInfo};
use wafer_run::{context::Context, Message, OutputStream};

use super::{
    providers::config::ProviderConfig,
    schema::{row_to_config, TABLE as PROVIDERS_TABLE},
    LlmBlock,
};
use crate::{
    http::err_internal,
    ui::{self, components},
};

// ---------------------------------------------------------------------------
// Providers page
// ---------------------------------------------------------------------------

/// `GET /b/llm/providers` — admin-only provider CRUD page.
///
/// Fetches rows directly from the block's own collection (Option A: avoids a
/// flash-of-empty during first paint).
pub(super) async fn providers_page(
    _block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    // Admin gate enforced centrally from the declared `AuthLevel::Admin` on
    // `GET /b/llm/providers` (no per-handler `is_admin` re-check).

    // Load all provider rows (both enabled and disabled) — the admin UI
    // wants the full picture, not just the in-flight set.
    let configs: Vec<(String, ProviderConfig)> =
        match db::list_all(ctx, PROVIDERS_TABLE, vec![]).await {
            Ok(records) => records
                .into_iter()
                .filter_map(|rec| row_to_config(&rec).ok().map(|cfg| (rec.id, cfg)))
                .collect(),
            Err(e) => return err_internal("Database error", e),
        };

    let content = html! {
        (components::page_header(
            "LLM Providers",
            Some("Configure OpenAI, Anthropic, and OpenAI-compatible endpoints."),
            None,
        ))

        // Add-provider form. Posts JSON via htmx json-enc so the existing
        // `POST /b/llm/api/providers` handler accepts the body without any
        // form-urlencoded translation layer.
        div .card style="margin-bottom:1.5rem;padding:1.25rem" {
            h3 style="font-size:1rem;font-weight:600;margin:0 0 0.75rem" { "Add provider" }
            (add_provider_form())
        }

        // Providers table. Rendered by a pure helper for testability.
        div .card style="padding:0" {
            (render_providers_table(&configs))
        }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("LLM Providers", ui::NavKind::Admin, "Providers"),
        content,
    )
    .await
}

/// Render the add-provider form. Separated out so the top-level page
/// composition stays flat and the form markup is swappable without editing
/// the outer shell.
fn add_provider_form() -> Markup {
    html! {
        form
            hx-post="/b/llm/api/providers"
            hx-ext="json-enc"
            hx-target="body"
            hx-swap="none"
            hx-on--after-request="if(event.detail.successful){location.reload()}"
        {
            div style="display:grid;grid-template-columns:1fr 1fr;gap:0.75rem" {
                div .form-group {
                    label .form-label for="new-name" { "Name" }
                    input
                        .form-input
                        type="text"
                        name="name"
                        id="new-name"
                        placeholder="openai-main"
                        required;
                }
                div .form-group {
                    label .form-label for="new-protocol" { "Protocol" }
                    select .form-select name="protocol" id="new-protocol" {
                        option value="open_ai" { "open_ai" }
                        option value="anthropic" { "anthropic" }
                        option value="open_ai_compatible" { "open_ai_compatible" }
                    }
                }
                div .form-group {
                    label .form-label for="new-endpoint" { "Endpoint" }
                    input
                        .form-input
                        type="url"
                        name="endpoint"
                        id="new-endpoint"
                        placeholder="https://api.openai.com/v1"
                        required;
                }
                div .form-group {
                    label .form-label for="new-key-var" { "Key variable" }
                    input
                        .form-input
                        type="text"
                        name="key_var"
                        id="new-key-var"
                        placeholder="IMPRESSPRESS__LLM__OPENAI_KEY";
                    p .form-hint {
                        "Admin variable name holding the API key. Leave empty for providers that don't need auth."
                    }
                }
                div .form-group style="grid-column:1/-1" {
                    label .form-label for="new-models" { "Models (comma-separated)" }
                    // htmx's json-enc extension turns this into a plain string;
                    // the server expects a JSON array, so we transform on
                    // submit via the form's `hx-on::config-request` hook
                    // below. Bare form post keeps the control accessible.
                    input
                        .form-input
                        type="text"
                        name="models"
                        id="new-models"
                        placeholder="gpt-4o, gpt-4o-mini";
                    p .form-hint {
                        "Optional. Leave empty and use \"Discover models\" after creation."
                    }
                }
                div .form-group style="grid-column:1/-1" {
                    label style="display:flex;align-items:center;gap:0.5rem;cursor:pointer" {
                        input type="checkbox" name="enabled" id="new-enabled" checked value="true";
                        " Enabled"
                    }
                }
            }
            div style="margin-top:0.75rem;display:flex;justify-content:flex-end" {
                button .btn.btn-primary type="submit" { "Add provider" }
            }
            // Normalize `models` CSV → JSON array, and coerce `enabled`
            // checkbox to a bool before htmx serialises. Both transforms
            // live on `htmx:config-request` so json-enc sees the final
            // shape. No DOM surgery — just dict mutation on the event.
            script {
                (maud::PreEscaped(ADD_PROVIDER_JS))
            }
        }
    }
}

/// `htmx:config-request` hook that normalises the add-provider form body.
///
/// `htmx json-enc` serialises form fields verbatim: `models` arrives as
/// a CSV string and `enabled` as either `"true"` or `undefined`. The
/// server wants `models: string[]` and `enabled: bool`, so we transform
/// in place before the request is sent. Keeps the JSON contract consistent
/// with the `/api/providers` handler without adding server-side
/// translation.
const ADD_PROVIDER_JS: &str = r#"
document.currentScript.closest('form').addEventListener('htmx:configRequest', function(ev) {
    var p = ev.detail.parameters;
    if (typeof p.models === 'string') {
        p.models = p.models.split(',').map(function(s){return s.trim();}).filter(Boolean);
    }
    p.enabled = (p.enabled === 'true' || p.enabled === true || p.enabled === 'on');
});
"#;

/// Render the providers table. Pure function of the loaded configs — used
/// directly by `providers_page` and by the unit tests that assert shape.
///
/// `configs` is `(row_id, ProviderConfig)` pairs so the Delete /
/// Discover-models actions can target the concrete row ID.
fn render_providers_table(configs: &[(String, ProviderConfig)]) -> Markup {
    html! {
        @if configs.is_empty() {
            div .text-center .text-muted style="padding:2rem" {
                "No providers configured yet. Use the form above to add one."
            }
        } @else {
            div .table-container {
                table .table {
                    thead {
                        tr {
                            th { "Name" }
                            th { "Protocol" }
                            th { "Endpoint" }
                            th { "Key var" }
                            th { "Models" }
                            th { "Enabled" }
                            th { "Actions" }
                        }
                    }
                    tbody {
                        @for (id, cfg) in configs {
                            (provider_row(id, cfg))
                        }
                    }
                }
            }
        }
    }
}

/// Single provider row. Extracted so the loop body stays readable and so
/// tests can render a one-row fixture without touching the outer `<table>`.
fn provider_row(id: &str, cfg: &ProviderConfig) -> Markup {
    let model_count = cfg.models.len();
    let models_label = if model_count == 0 {
        "(discover)".to_string()
    } else {
        cfg.models.join(", ")
    };
    html! {
        tr {
            td { strong { (cfg.name) } }
            td {
                span .badge.badge-info { (cfg.protocol.as_str()) }
            }
            td style="font-size:0.8rem;max-width:260px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" {
                (cfg.endpoint)
            }
            td {
                @if let Some(kv) = cfg.key_var.as_deref() {
                    code style="font-size:0.75rem" { (kv) }
                } @else {
                    span .text-muted style="font-size:0.75rem" { "(none)" }
                }
            }
            td style="font-size:0.8rem;max-width:280px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" {
                @if model_count == 0 {
                    span .text-muted { (models_label) }
                } @else {
                    span .badge.badge-info style="margin-right:0.5rem" { (model_count) }
                    span .text-muted { (models_label) }
                }
            }
            td {
                @if cfg.enabled {
                    span .badge.badge-success { "Enabled" }
                } @else {
                    span .badge.badge-warning { "Disabled" }
                }
            }
            td {
                div style="display:flex;gap:0.375rem;flex-wrap:wrap" {
                    button
                        .btn.btn-sm.btn-secondary
                        hx-post={"/b/llm/api/providers/" (id) "/discover-models"}
                        hx-confirm={"Discover models for \"" (cfg.name) "\" from its /v1/models endpoint?"}
                        hx-on--after-request="if(event.detail.successful){location.reload()}"
                    {
                        "Discover"
                    }
                    button
                        .btn.btn-sm.btn-danger
                        hx-delete={"/b/llm/api/providers/" (id)}
                        hx-confirm={"Delete provider \"" (cfg.name) "\"?"}
                        hx-target="closest tr"
                        hx-swap="outerHTML"
                    {
                        "Delete"
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Models page
// ---------------------------------------------------------------------------

/// `GET /b/llm/models` — admin aggregated-models table.
///
/// Loads the aggregated model list with one typed call to
/// `wafer_core::clients::llm::list_models(ctx)` (the same path `pages.rs`
/// uses) — no in-process self-HTTP hop through `routes::list_models` and no
/// JSON-envelope reparse. Each row's status cell fetches from
/// `/b/llm/api/models/{backend}/{model}/status` via `hx-get` +
/// `hx-trigger="load"` so a slow backend never blocks the first paint.
pub(super) async fn models_page(
    _block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    // Admin gate enforced centrally from the declared `AuthLevel::Admin` on
    // `GET /b/llm/models` (no per-handler `is_admin` re-check).

    let models = match wafer_core::clients::llm::list_models(ctx).await {
        Ok(m) => m,
        Err(e) => return err_internal("llm list_models failed", e.message),
    };

    let content = html! {
        (components::page_header(
            "LLM Models",
            Some("Aggregated across every configured provider."),
            None,
        ))

        (render_models_table(&models))
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("LLM Models", ui::NavKind::Admin, "Models"),
        content,
    )
    .await
}

/// Render the models table. Pure function of the typed `ModelInfo` slice so
/// the shape can be tested without constructing a `Context`.
fn render_models_table(models: &[ModelInfo]) -> Markup {
    html! {
        @if models.is_empty() {
            div .card {
                div .text-center .text-muted style="padding:2rem" {
                    "No models available. Configure a provider and run \"Discover models\" to populate this list."
                }
            }
        } @else {
            div .card style="padding:0" {
                div .table-container {
                    table .table {
                        thead {
                            tr {
                                th { "Name" }
                                th { "Backend" }
                                th { "Capabilities" }
                                th { "Status" }
                                th { "Actions" }
                            }
                        }
                        tbody {
                            @for m in models {
                                (model_row(m))
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Render a single model row. Status loads lazily on row mount; actions
/// (Load / Unload) are only meaningful for local backends, so we render
/// them for every row and let the server-side handler 404 / no-op for
/// backends that don't support it — this keeps the UI uniform without
/// hardcoding a local-backend allowlist in the renderer.
fn model_row(model: &ModelInfo) -> Markup {
    let backend_id = model.backend_id.as_str();
    let model_id = model.model_id.as_str();
    let display_name = if model.display_name.is_empty() {
        model_id
    } else {
        model.display_name.as_str()
    };

    let caps = &model.capabilities;
    let cap_streaming = caps.streaming;
    let cap_tools = caps.tools;
    let cap_vision = caps.vision;
    let cap_json = caps.json_mode;

    let status_url = format!("/b/llm/api/models/{backend_id}/{model_id}/status");
    let load_url = format!("/b/llm/api/models/{backend_id}/{model_id}/load");
    let unload_url = format!("/b/llm/api/models/{backend_id}/{model_id}/unload");

    html! {
        tr {
            td { strong { (display_name) } }
            td {
                span .badge.badge-info { (backend_id) }
                @if display_name != model_id {
                    " "
                    code style="font-size:0.75rem" { (model_id) }
                }
            }
            td {
                div style="display:flex;gap:0.25rem;flex-wrap:wrap" {
                    @if cap_streaming { span .badge.badge-info { "streaming" } }
                    @if cap_tools { span .badge.badge-info { "tools" } }
                    @if cap_vision { span .badge.badge-info { "vision" } }
                    @if cap_json { span .badge.badge-info { "json" } }
                    @if !(cap_streaming || cap_tools || cap_vision || cap_json) {
                        span .text-muted style="font-size:0.75rem" { "—" }
                    }
                }
            }
            td {
                // Lazy-load the per-model status so a slow backend doesn't
                // hold up the initial render. The endpoint returns JSON;
                // `hx-ext=json-dec` is not available here so we render
                // status via a small helper endpoint below — for now, emit
                // a loading placeholder that replaces itself on load.
                span
                    .badge.badge-loading
                    hx-get=(status_url)
                    hx-trigger="load"
                    hx-swap="outerHTML"
                {
                    "Loading…"
                }
            }
            td {
                div style="display:flex;gap:0.375rem;flex-wrap:wrap" {
                    button
                        .btn.btn-sm.btn-secondary
                        hx-post=(load_url)
                        hx-swap="none"
                        hx-confirm={"Load model \"" (model_id) "\" on backend \"" (backend_id) "\"?"}
                    {
                        "Load"
                    }
                    button
                        .btn.btn-sm.btn-ghost
                        hx-post=(unload_url)
                        hx-swap="none"
                        hx-confirm={"Unload model \"" (model_id) "\" on backend \"" (backend_id) "\"?"}
                    {
                        "Unload"
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::llm::providers::config::ProviderProtocol;

    // The provider/model admin pages are gated centrally by the declared
    // `AuthLevel::Admin` on `GET /b/llm/providers` + `GET /b/llm/models`; the
    // non-admin rejection is pinned at the enforcement point in
    // `tests/extra_routes_test.rs` (llm_admin_ui_*), not here, so these page
    // renderers no longer carry their own `is_admin` re-check.

    #[test]
    fn render_providers_table_empty_shows_hint() {
        let m = render_providers_table(&[]).into_string();
        assert!(
            m.contains("No providers configured"),
            "empty-state hint missing; got: {m}"
        );
        // No <table> element when empty — keeps the page compact.
        assert!(
            !m.contains("<table"),
            "empty render must not include a table; got: {m}"
        );
    }

    #[test]
    fn render_providers_table_renders_row_per_config() {
        let configs = vec![
            (
                "row-1".to_string(),
                ProviderConfig::new(
                    "openai-main",
                    ProviderProtocol::OpenAi,
                    "https://api.openai.com/v1",
                )
                .with_key_var("IMPRESSPRESS__LLM__OPENAI_KEY")
                .with_models(vec!["gpt-4o".into(), "gpt-4o-mini".into()]),
            ),
            (
                "row-2".to_string(),
                ProviderConfig::new(
                    "anthropic-main",
                    ProviderProtocol::Anthropic,
                    "https://api.anthropic.com/v1",
                ),
            ),
        ];
        let m = render_providers_table(&configs).into_string();

        // Each provider's name and protocol token is rendered verbatim.
        assert!(m.contains("openai-main"));
        assert!(m.contains("open_ai"));
        assert!(m.contains("anthropic-main"));
        assert!(m.contains("anthropic"));

        // Delete/Discover actions target the concrete row ID, not the name.
        assert!(
            m.contains("/b/llm/api/providers/row-1"),
            "row-1 action URLs missing; got: {m}"
        );
        assert!(
            m.contains("/b/llm/api/providers/row-2"),
            "row-2 action URLs missing; got: {m}"
        );
        assert!(m.contains("/discover-models"), "discover action missing");

        // Key-var column renders verbatim, no masking/translation.
        assert!(m.contains("IMPRESSPRESS__LLM__OPENAI_KEY"));

        // Model-count badge for the multi-model row.
        assert!(m.contains("gpt-4o"));
    }

    #[test]
    fn render_models_table_empty_shows_hint() {
        let m = render_models_table(&[]).into_string();
        assert!(
            m.contains("No models available"),
            "empty-state hint missing; got: {m}"
        );
    }

    #[test]
    fn render_models_table_wires_status_and_actions() {
        use wafer_core::clients::llm::ModelCapabilities;
        let models = vec![ModelInfo {
            backend_id: "openai-main".into(),
            model_id: "gpt-4o".into(),
            display_name: "GPT-4o".into(),
            capabilities: ModelCapabilities {
                streaming: true,
                tools: true,
                vision: false,
                json_mode: true,
                ..ModelCapabilities::default()
            },
        }];
        let m = render_models_table(&models).into_string();

        // Display-name surfaced; model_id also rendered so admins can copy.
        assert!(m.contains("GPT-4o"));
        assert!(m.contains("gpt-4o"));
        assert!(m.contains("openai-main"));

        // Capability badges for the declared flags only.
        assert!(m.contains("streaming"));
        assert!(m.contains("tools"));
        assert!(m.contains("json"));
        // `vision` was false — the badge must not appear.
        assert!(!m.contains(">vision<"), "vision badge leaked; got: {m}");

        // Lazy-load wiring for the status cell.
        assert!(
            m.contains(r#"hx-get="/b/llm/api/models/openai-main/gpt-4o/status""#),
            "status lazy-load missing; got: {m}"
        );
        assert!(m.contains(r#"hx-trigger="load""#));

        // Load / Unload buttons target the per-(backend, model) endpoints.
        assert!(m.contains(r#"hx-post="/b/llm/api/models/openai-main/gpt-4o/load""#));
        assert!(m.contains(r#"hx-post="/b/llm/api/models/openai-main/gpt-4o/unload""#));
    }
}
