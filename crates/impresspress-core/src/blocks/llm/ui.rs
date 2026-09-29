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
use wafer_core::clients::llm::ModelInfo;
use wafer_run::{context::Context, Message, OutputStream};

use super::{
    providers::config::ProviderConfig,
    schema::{row_to_config, TABLE as PROVIDERS_TABLE},
    LlmBlock, EXAMPLE_KEY_VAR,
};
use crate::{
    blocks::crud,
    db_read::{self, Bound},
    ui::{self, components},
};

// ---------------------------------------------------------------------------
// Providers page
// ---------------------------------------------------------------------------

/// `GET /b/llm/providers` — admin-only provider CRUD page.
///
/// Fetches rows directly from the block's own collection (Option A: avoids a
/// flash-of-empty during first paint).
///
/// The page renders the create form and the per-row actions only when the
/// runtime can actually manage providers. A runtime built without the `llm`
/// cargo feature holds a `NoopProviderAdmin`, and its CRUD handlers answer
/// `501 Unimplemented` — but htmx does not swap on a non-2xx, so the
/// administrator clicked and nothing visible happened at all. Before the
/// handlers started refusing, the same click lied with a success. Neither is
/// right: the page reads the same predicate the handlers do
/// (`ProviderAdmin::manages_providers`) and says so instead of offering
/// controls that cannot work.
pub(super) async fn providers_page(
    block: &LlmBlock,
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    // Admin gate enforced centrally from the declared `AuthLevel::Admin` on
    // `GET /b/llm/providers` (no per-handler `is_admin` re-check).

    // Load all provider rows (both enabled and disabled) — the admin UI
    // wants the full picture, not just the in-flight set.
    let configs: Vec<(String, ProviderConfig)> = match db_read::list_bounded(
        ctx,
        PROVIDERS_TABLE,
        vec![],
        Bound::Curated("LLM providers are configured by an admin"),
    )
    .await
    {
        Ok(records) => records
            .into_iter()
            .filter_map(|rec| row_to_config(&rec).ok().map(|cfg| (rec.id, cfg)))
            .collect(),
        Err(e) => return crud::db_error_page(msg, e, "llm providers page: provider read failed"),
    };

    let manages = block.provider_admin.manages_providers();

    let content = html! {
        (components::page_header(
            "LLM Providers",
            Some(if manages {
                "Configure OpenAI, Anthropic, and OpenAI-compatible endpoints."
            } else {
                "Read-only on this deployment."
            }),
            None,
        ))

        @if manages {
            // Add-provider form. Posts `application/x-www-form-urlencoded`,
            // which `POST /b/llm/api/providers` accepts alongside JSON.
            div .card .mb-6 {
                h3 .card-title .mb-3 { "Add provider" }
                (add_provider_form())
            }
        } @else {
            (cannot_manage_providers_notice())
        }

        // Providers table. Rendered by a pure helper for testability.
        div .card .card--flush {
            (render_providers_table(&configs, manages))
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

/// What the page says instead of the create form when the runtime holds a
/// handle that cannot manage providers.
///
/// It names the two things an administrator needs: that the rows below are a
/// read-only view, and where provider configuration lives on such a
/// deployment. A browser runtime configures its providers inside
/// `BrowserLlmService`, not through this block.
fn cannot_manage_providers_notice() -> Markup {
    html! {
        div .card .mb-6 {
            h3 .card-title .mb-3 { "This deployment cannot manage providers" }
            p .text-muted {
                "No provider backend is compiled into this runtime, so \
                 creating, editing, discovering and deleting providers are \
                 unavailable here — the API answers 501 for all four. Any \
                 rows below are stored configuration, shown read-only."
            }
            p .text-muted .mt-2 {
                "A browser deployment configures its providers inside its own \
                 LLM service rather than through this page. A server \
                 deployment gets these controls by building with the `llm` \
                 feature."
            }
        }
    }
}

/// Render the add-provider form. Separated out so the top-level page
/// composition stays flat and the form markup is swappable without editing
/// the outer shell.
///
/// A plain htmx form: it posts `application/x-www-form-urlencoded`, which is
/// what the handler parses. Nothing here reshapes the body in the browser —
/// the `models` text input and the `enabled` checkbox are coerced by
/// `routes::providers::parse_create_provider_body`, which is one description
/// of the field shapes instead of two.
///
/// Like every control on this page it needs htmx: there is no `action` or
/// `method`, so the submit is htmx's or it is nothing.
///
/// `hx-swap="none"` because the response is the created provider as JSON for
/// SDK callers; the page picks up the new row by reloading.
fn add_provider_form() -> Markup {
    html! {
        form
            hx-post="/b/llm/api/providers"
            hx-swap="none"
            data-reload-on-success
        {
            div .form-row .gap-3 {
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
                        placeholder=(EXAMPLE_KEY_VAR);
                    p .form-hint {
                        "Admin variable name holding the API key. Leave empty for providers that don't need auth."
                    }
                }
                div .form-group {
                    label .form-label for="new-max-tokens-field" { "Token budget field" }
                    // The empty option is the ordinary case. A select always
                    // posts something, so `create_provider`'s form parser
                    // drops the empty value rather than handing serde a token
                    // the contract does not have.
                    select .form-select name="max_tokens_field" id="new-max-tokens-field" {
                        option value="" selected { "Follow the protocol" }
                        option value="max_tokens" { "max_tokens" }
                        option value="max_completion_tokens" { "max_completion_tokens" }
                    }
                    p .form-hint {
                        "Only for an OpenAI-shaped endpoint that wants the other \
                         spelling than its protocol's — an Azure OpenAI reasoning \
                         deployment on open_ai_compatible needs \
                         max_completion_tokens. The anthropic protocol refuses this."
                    }
                }
                div .form-group .col-span-full {
                    label .form-label for="new-models" { "Models (comma-separated)" }
                    // One text input; the handler splits it into the contract's
                    // `models` array.
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
                div .form-group .col-span-full {
                    label .form-checkbox {
                        input type="checkbox" name="enabled" id="new-enabled" checked value="true";
                        "Enabled"
                    }
                }
            }
            div .flex .justify-end .mt-3 {
                button .btn.btn--primary type="submit" { "Add provider" }
            }
        }
    }
}

/// Render the providers table. Pure function of the loaded configs — used
/// directly by `providers_page` and by the unit tests that assert shape.
///
/// `configs` is `(row_id, ProviderConfig)` pairs so the Delete /
/// Discover-models actions can target the concrete row ID.
///
/// `manages` is `ProviderAdmin::manages_providers()`. When it is false the
/// Actions column is dropped entirely rather than rendered disabled: the two
/// buttons in it are the only things there, and a runtime that answers 501 to
/// both has no action to offer.
fn render_providers_table(configs: &[(String, ProviderConfig)], manages: bool) -> Markup {
    html! {
        @if configs.is_empty() {
            div .empty-state {
                @if manages {
                    "No providers configured yet. Use the form above to add one."
                } @else {
                    "No providers are configured, and this deployment cannot \
                     add one."
                }
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
                            th { "Budget field" }
                            th { "Models" }
                            th { "Enabled" }
                            @if manages { th { "Actions" } }
                        }
                    }
                    tbody {
                        @for (id, cfg) in configs {
                            (provider_row(id, cfg, manages))
                        }
                    }
                }
            }
        }
    }
}

/// Single provider row. Extracted so the loop body stays readable and so
/// tests can render a one-row fixture without touching the outer `<table>`.
///
/// `manages` gates the Actions cell — see [`render_providers_table`].
fn provider_row(id: &str, cfg: &ProviderConfig, manages: bool) -> Markup {
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
            td .text-xs .truncate .llm-cell--endpoint {
                (cfg.endpoint)
            }
            td {
                @if let Some(kv) = cfg.key_var.as_deref() {
                    code .text-xs { (kv) }
                } @else {
                    span .text-muted .text-xs { "(none)" }
                }
            }
            td {
                @if let Some(field) = cfg.max_tokens_field {
                    code .text-xs { (field.as_str()) }
                } @else {
                    span .text-muted .text-xs { "(protocol)" }
                }
            }
            td .text-xs .truncate .llm-cell--models {
                @if model_count == 0 {
                    span .text-muted { (models_label) }
                } @else {
                    span .badge.badge-info .mr-2 { (model_count) }
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
            @if manages {
                td {
                    div .flex .gap-2 .flex-wrap {
                        button
                            .btn.btn--sm.btn--secondary
                            hx-post={"/b/llm/api/providers/" (id) "/discover-models"}
                            // The answer is the JSON model list; the page
                            // reloads to show it, so nothing is swapped.
                            hx-swap="none"
                            hx-confirm={"Discover models for \"" (cfg.name) "\" from its /v1/models endpoint?"}
                            data-reload-on-success
                        {
                            "Discover"
                        }
                        button
                            .btn.btn--sm.btn--danger
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
        Err(e) => return crud::db_error_page(msg, e, "llm models page: list_models failed"),
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
                div .empty-state {
                    "No models available. Configure a provider and run \"Discover models\" to populate this list."
                }
            }
        } @else {
            div .card .card--flush {
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
                    code .text-xs { (model_id) }
                }
            }
            td {
                div .flex .gap-1 .flex-wrap {
                    @if cap_streaming { span .badge.badge-info { "streaming" } }
                    @if cap_tools { span .badge.badge-info { "tools" } }
                    @if cap_vision { span .badge.badge-info { "vision" } }
                    @if cap_json { span .badge.badge-info { "json" } }
                    @if !(cap_streaming || cap_tools || cap_vision || cap_json) {
                        span .text-muted .text-xs { "—" }
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
                div .flex .gap-2 .flex-wrap {
                    button
                        .btn.btn--sm.btn--secondary
                        hx-post=(load_url)
                        hx-swap="none"
                        hx-confirm={"Load model \"" (model_id) "\" on backend \"" (backend_id) "\"?"}
                    {
                        "Load"
                    }
                    button
                        .btn.btn--sm.btn--ghost
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

    /// The add-provider form is submitted with the browser's own encoding,
    /// and `routes::providers::create_provider` parses that.
    ///
    /// The form used to declare `hx-ext="json-enc"` and carry a script that
    /// reshaped the parameters for it. Neither did anything: no json-enc
    /// extension is shipped with the chrome — asserted below against the
    /// bytes actually served — and htmx silently ignores an extension it was
    /// never given, so the body went out form-encoded either way and the
    /// handler answered 400 to every submit.
    #[test]
    fn the_add_provider_form_declares_no_encoding_extension() {
        let m = add_provider_form().into_string();

        assert!(
            !m.contains("hx-ext"),
            "no htmx extension is shipped, so declaring one only misdescribes \
             the request; got: {m}"
        );
        assert!(
            !m.contains("<script"),
            "the field coercions are the handler's, not the browser's; got: {m}"
        );
        assert!(
            m.contains(r#"hx-post="/b/llm/api/providers""#),
            "the form must post to the create endpoint; got: {m}"
        );
        for field in [
            "name",
            "protocol",
            "endpoint",
            "key_var",
            "max_tokens_field",
            "models",
            "enabled",
        ] {
            assert!(
                m.contains(&format!(r#"name="{field}""#)),
                "the form must send `{field}` — `routes::providers`'s form tests \
                 post exactly these; got: {m}"
            );
        }
    }

    /// What makes the assertion above true rather than merely asserted: no
    /// script this deployment serves mentions json-enc, so nothing can be
    /// registering it. Scanning htmx alone would have missed a
    /// `htmx.defineExtension('json-enc', …)` in the shared chrome or in a
    /// block's own bundle, which is exactly where a hand-rolled one would go.
    ///
    /// Ship an extension and this test is the place that says `hx-ext` may be
    /// used again.
    #[cfg(feature = "embed-assets")]
    #[test]
    fn no_shipped_script_registers_a_json_enc_extension() {
        let mut scanned: Vec<&str> = Vec::new();
        for asset in crate::ui::assets::ASSETS {
            if !asset.logical.ends_with(".js") {
                continue;
            }
            // A block's bundle is in the manifest even when that block is not
            // compiled into this build; only the embedded ones can be read.
            let Some(bytes) = crate::ui::assets::bytes(asset.logical) else {
                continue;
            };
            assert!(
                !String::from_utf8_lossy(bytes).contains("json-enc"),
                "{} mentions json-enc — check whether an extension is now \
                 registered before trusting `hx-ext`",
                asset.logical
            );
            scanned.push(asset.logical);
        }
        // Without this the scan passes whether it read anything or not.
        for required in ["htmx.min.js", "chrome.js"] {
            assert!(
                scanned.contains(&required),
                "the scan must reach {required}; it read {scanned:?}"
            );
        }
    }

    #[test]
    fn render_providers_table_empty_shows_hint() {
        let m = render_providers_table(&[], true).into_string();
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
                .with_key_var(EXAMPLE_KEY_VAR)
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
        let m = render_providers_table(&configs, true).into_string();

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
        assert!(m.contains(EXAMPLE_KEY_VAR));

        // Model-count badge for the multi-model row.
        assert!(m.contains("gpt-4o"));
    }

    /// A runtime that cannot manage providers offers no control that would
    /// answer 501.
    ///
    /// htmx does not swap on a non-2xx, so a rendered Discover or Delete
    /// button on such a deployment is a control that does nothing visible at
    /// all when clicked. The rows themselves stay: they are stored
    /// configuration and an administrator should be able to see it.
    #[test]
    fn a_runtime_that_cannot_manage_providers_renders_no_action_controls() {
        let configs = vec![(
            "row-1".to_string(),
            ProviderConfig::new(
                "openai-main",
                ProviderProtocol::OpenAi,
                "https://api.openai.com/v1",
            ),
        )];

        let m = render_providers_table(&configs, false).into_string();

        assert!(
            m.contains("openai-main"),
            "the stored rows must still be visible; got: {m}"
        );
        for absent in ["/discover-models", "hx-delete", "Actions"] {
            assert!(
                !m.contains(absent),
                "`{absent}` must not be rendered when the runtime cannot \
                 manage providers; got: {m}"
            );
        }
    }

    /// And it says why, rather than showing a create form whose submit does
    /// nothing visible.
    #[test]
    fn the_inert_notice_names_the_refusal_and_where_providers_are_configured() {
        let m = cannot_manage_providers_notice().into_string();

        assert!(m.contains("cannot manage providers"), "got: {m}");
        assert!(
            m.contains("501"),
            "the notice must name what the API actually answers; got: {m}"
        );
    }

    /// The empty state stops telling an administrator to use a form that is
    /// not on the page.
    #[test]
    fn the_empty_state_does_not_point_at_a_form_that_is_not_rendered() {
        let m = render_providers_table(&[], false).into_string();

        assert!(m.contains("No providers"), "got: {m}");
        assert!(
            !m.contains("form above"),
            "no create form is rendered on such a deployment; got: {m}"
        );
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
