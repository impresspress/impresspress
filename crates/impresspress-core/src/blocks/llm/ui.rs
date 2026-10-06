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
    providers::config::{ProviderConfig, ProviderProtocol},
    schema::{row_to_config, TABLE as PROVIDERS_TABLE},
    LlmBlock, EXAMPLE_KEY_VAR,
};
use crate::{
    db_read::{self, Bound},
    llm_wire::openai::MaxTokensField,
    ui::{self, components, icons},
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
        Err(e) => {
            return super::pages::error_page(
                ctx,
                msg,
                super::pages::Section::Providers,
                "Providers",
                e,
                "llm providers page: provider read failed",
            )
            .await
        }
    };

    let manages = block.provider_admin.manages_providers();

    // The list is the page; the create and edit forms wait in modals, opened
    // from the topbar's Add provider and from each row's Edit.
    let content = html! {
        @if !manages {
            (cannot_manage_providers_notice())
        }
        (render_providers_table(&configs, manages))
        @if manages {
            (components::modal(ADD_PROVIDER_MODAL_ID, "Add provider", provider_form(ProviderForm::New)))
            @for (id, cfg) in &configs {
                (components::modal(
                    &edit_modal_id(id),
                    &format!("Edit {}", cfg.name),
                    provider_form(ProviderForm::Edit { id, cfg }),
                ))
            }
        }
    };

    let shell = ui::Shell::admin("LLM Providers", "Providers").subtitle(if manages {
        "OpenAI, Anthropic and OpenAI-compatible endpoints the chat can route to"
    } else {
        "Read-only on this deployment"
    });
    let shell = if manages {
        shell.actions(vec![add_provider_button()])
    } else {
        shell
    };
    ui::shell_page(
        ctx,
        msg,
        shell,
        ui::PageBody::from(content)
            .with_subnav(super::pages::sections(super::pages::Section::Providers)),
    )
    .await
}

/// The Add provider modal's element id, which its triggers name in
/// `data-modal-target`.
const ADD_PROVIDER_MODAL_ID: &str = "add-provider";

/// The element id of provider `id`'s edit modal.
fn edit_modal_id(id: &str) -> String {
    format!("edit-provider-{id}")
}

/// The page's primary action: opens the Add provider modal.
fn add_provider_button() -> Markup {
    html! {
        button .btn.btn--primary type="button"
            data-action="modal-open" data-modal-target=(ADD_PROVIDER_MODAL_ID)
        {
            (icons::plus()) "Add provider"
        }
    }
}

/// What the page says instead of offering controls when the runtime holds a
/// handle that cannot manage providers.
///
/// It names the two things an administrator needs: that the rows below are a
/// read-only view, and where provider configuration lives on such a
/// deployment. A browser runtime configures its providers inside
/// `BrowserLlmService`, not through this block.
fn cannot_manage_providers_notice() -> Markup {
    components::callout(
        components::CalloutTone::Warning,
        "This deployment cannot manage providers",
        html! {
            p {
                "No provider backend is compiled into this runtime, so \
                 creating, editing, discovering and deleting providers are \
                 unavailable here — the API answers 501 for all four. Any \
                 rows below are stored configuration, shown read-only."
            }
            p {
                "A browser deployment configures its providers inside its own \
                 LLM service rather than through this page. A server \
                 deployment gets these controls by building with the `llm` \
                 feature."
            }
        },
        None,
    )
}

/// Which provider form to render: the empty create form, or the edit form
/// of one stored provider, filled with what it holds.
#[derive(Clone, Copy)]
enum ProviderForm<'a> {
    New,
    Edit {
        id: &'a str,
        cfg: &'a ProviderConfig,
    },
}

/// The provider form, in the Add provider modal or in one provider's edit
/// modal.
///
/// A plain htmx form: it sends `application/x-www-form-urlencoded`, which
/// both `POST /b/llm/api/providers` and `PATCH /b/llm/api/providers/{id}`
/// parse. Nothing here reshapes the body in the browser — the `models` text
/// input and the `enabled` checkbox are coerced by
/// `routes::providers::parse_provider_body`, which is one description of the field
/// shapes for both routes instead of one per form.
///
/// Like every control on this page it needs htmx: there is no `action` or
/// `method`, so the submit is htmx's or it is nothing.
///
/// `hx-swap="none"` because the response is the provider as JSON for SDK
/// callers; the page picks up the change by reloading. A refusal is a toast
/// (chrome.js), which an open modal shows inside itself.
fn provider_form(form: ProviderForm<'_>) -> Markup {
    let (prefix, cfg) = match form {
        ProviderForm::New => ("new".to_string(), None),
        ProviderForm::Edit { id, cfg } => (format!("edit-{id}"), Some(cfg)),
    };
    let field = |name: &str| format!("{prefix}-{name}");
    // A new provider starts on `open_ai`, said with `selected` rather than
    // left to the browser's first-option default: the modal's Esc guard
    // compares every option with its `defaultSelected`, and an implicit
    // selection reads as a change nobody made.
    let protocol = cfg.map_or(ProviderProtocol::OpenAi, |c| c.protocol);
    let budget = cfg.and_then(|c| c.max_tokens_field);
    let models = cfg.map(|c| c.models.join(", ")).unwrap_or_default();
    let enabled = cfg.is_none_or(|c| c.enabled);
    let submit = if cfg.is_some() {
        "Save changes"
    } else {
        "Add provider"
    };
    let key_hint = field("key-var-hint");
    let budget_hint = field("max-tokens-field-hint");
    let models_hint = field("models-hint");
    html! {
        form
            hx-post=[matches!(form, ProviderForm::New).then_some("/b/llm/api/providers")]
            hx-patch=[match form {
                ProviderForm::Edit { id, .. } => Some(format!("/b/llm/api/providers/{id}")),
                ProviderForm::New => None,
            }]
            hx-swap="none"
            data-reload-on-success
        {
            div .form-group {
                label .form-label .required for=(field("name")) { "Name" }
                input .form-input type="text" name="name" id=(field("name"))
                    value=[cfg.map(|c| c.name.as_str())]
                    placeholder="openai-main"
                    autocomplete="off" spellcheck="false"
                    required;
            }
            div .form-group {
                label .form-label for=(field("protocol")) { "Protocol" }
                select .form-select name="protocol" id=(field("protocol")) {
                    @for p in [ProviderProtocol::OpenAi, ProviderProtocol::Anthropic, ProviderProtocol::OpenAiCompatible] {
                        option value=(p.as_str()) selected[protocol == p] { (p.as_str()) }
                    }
                }
            }
            div .form-group {
                label .form-label .required for=(field("endpoint")) { "Endpoint" }
                input .form-input type="url" name="endpoint" id=(field("endpoint"))
                    value=[cfg.map(|c| c.endpoint.as_str())]
                    placeholder="https://api.openai.com/v1"
                    autocomplete="off" spellcheck="false"
                    required;
            }
            div .form-group {
                label .form-label for=(field("key-var")) { "Key variable" }
                input .form-input type="text" name="key_var" id=(field("key-var"))
                    value=[cfg.and_then(|c| c.key_var.as_deref())]
                    placeholder=(EXAMPLE_KEY_VAR)
                    autocomplete="off" spellcheck="false"
                    aria-describedby=(key_hint);
                p .form-hint id=(key_hint) {
                    "Admin variable name holding the API key. Leave empty for providers that don't need auth."
                }
            }
            div .form-group {
                label .form-label for=(field("max-tokens-field")) { "Token budget field" }
                // The empty option is the ordinary case. A select always
                // posts something, so the form parser reads the empty value
                // as "no override" rather than handing serde a token the
                // contract does not have.
                select .form-select name="max_tokens_field" id=(field("max-tokens-field"))
                    aria-describedby=(budget_hint)
                {
                    option value="" selected[budget.is_none()] { "Follow the protocol" }
                    @for f in [MaxTokensField::MaxTokens, MaxTokensField::MaxCompletionTokens] {
                        option value=(f.as_str()) selected[budget == Some(f)] { (f.as_str()) }
                    }
                }
                p .form-hint id=(budget_hint) {
                    "Only for an OpenAI-shaped endpoint that wants the other \
                     spelling than its protocol's — an Azure OpenAI reasoning \
                     deployment on open_ai_compatible needs \
                     max_completion_tokens. The anthropic protocol refuses this."
                }
            }
            div .form-group {
                label .form-label for=(field("models")) { "Models" }
                // One text input; the handler splits it into the contract's
                // `models` array.
                input .form-input type="text" name="models" id=(field("models"))
                    value=(models)
                    placeholder="gpt-4o, gpt-4o-mini"
                    autocomplete="off" spellcheck="false"
                    aria-describedby=(models_hint);
                p .form-hint id=(models_hint) {
                    "Comma-separated. Optional: leave empty and use Discover on the provider once it is saved."
                }
            }
            div .form-group {
                label .form-checkbox {
                    input type="checkbox" name="enabled" id=(field("enabled")) checked[enabled] value="true";
                    " Enabled"
                }
            }
            (components::modal_footer(html! {
                (components::modal_cancel())
                button .btn.btn--primary.btn--block type="submit" { (submit) }
            }))
        }
    }
}

/// The providers table's columns. The key variable and the budget field are
/// optional: most deployments set neither on any provider.
const PROVIDER_COLUMNS: [components::TableCol<'static>; 8] = [
    components::TableCol::new("Name").primary(),
    components::TableCol::new("Protocol"),
    components::TableCol::new("Endpoint"),
    components::TableCol::new("Key variable").optional(),
    components::TableCol::new("Budget field").optional(),
    components::TableCol::new("Models"),
    components::TableCol::new("Status"),
    components::TableCol::new("Actions").actions(),
];

/// The same columns less Actions, for a runtime that cannot manage
/// providers: the buttons are the only things in that column, and a runtime
/// that answers 501 to every one of them has no action to offer.
const READ_ONLY_PROVIDER_COLUMNS: [components::TableCol<'static>; 7] = [
    PROVIDER_COLUMNS[0],
    PROVIDER_COLUMNS[1],
    PROVIDER_COLUMNS[2],
    PROVIDER_COLUMNS[3],
    PROVIDER_COLUMNS[4],
    PROVIDER_COLUMNS[5],
    PROVIDER_COLUMNS[6],
];

/// Render the providers table. Pure function of the loaded configs — used
/// directly by `providers_page` and by the unit tests that assert shape.
///
/// `configs` is `(row_id, ProviderConfig)` pairs so the Edit / Delete /
/// Discover-models actions can target the concrete row ID.
///
/// `manages` is `ProviderAdmin::manages_providers()`; see
/// [`READ_ONLY_PROVIDER_COLUMNS`].
fn render_providers_table(configs: &[(String, ProviderConfig)], manages: bool) -> Markup {
    let rows = configs
        .iter()
        .map(|(id, cfg)| components::TableRow::new(provider_cells(id, cfg, manages)))
        .collect();
    let columns: &[components::TableCol<'static>] = if manages {
        &PROVIDER_COLUMNS
    } else {
        &READ_ONLY_PROVIDER_COLUMNS
    };
    let table = components::DataTable::new(columns).rows(rows);
    if manages {
        table
            .empty_state(
                "No providers yet",
                "Add an OpenAI, Anthropic or OpenAI-compatible endpoint, and the chat can route to it.",
                Some(add_provider_button()),
            )
            .render()
    } else {
        table
            .empty_state(
                "No providers",
                "None are configured, and this deployment cannot add one.",
                None,
            )
            .render()
    }
}

/// One provider's cells, in [`PROVIDER_COLUMNS`] order; the Actions cell
/// only when `manages`.
fn provider_cells(id: &str, cfg: &ProviderConfig, manages: bool) -> Vec<Markup> {
    let mut cells = vec![
        html! { strong { (cfg.name) } },
        components::badge(components::BadgeVariant::Secondary, cfg.protocol.as_str()),
        html! { span .text-sm { (components::breakable_id(&cfg.endpoint)) } },
        match cfg.key_var.as_deref() {
            Some(kv) => html! { code .text-xs { (components::breakable_id(kv)) } },
            None => html! {},
        },
        match cfg.max_tokens_field {
            Some(field) => html! { code .text-xs { (field.as_str()) } },
            None => html! {},
        },
        if cfg.models.is_empty() {
            html! { span .text-muted { "None yet" } }
        } else {
            html! { span .text-sm { (cfg.models.join(", ")) } }
        },
        if cfg.enabled {
            components::badge(components::BadgeVariant::Success, "Enabled")
        } else {
            components::badge(components::BadgeVariant::Secondary, "Disabled")
        },
    ];
    if manages {
        cells.push(html! {
            button .btn.btn--sm.btn--icon.btn--ghost type="button"
                data-action="modal-open" data-modal-target=(edit_modal_id(id))
                aria-label={"Edit " (cfg.name)}
                title="Edit provider"
            {
                (icons::edit())
            }
            button
                .btn.btn--sm.btn--secondary
                type="button"
                hx-post={"/b/llm/api/providers/" (id) "/discover-models"}
                // The answer is the JSON model list; the page reloads to
                // show it, so nothing is swapped.
                hx-swap="none"
                hx-confirm={"Discover models for \"" (cfg.name) "\" from its /v1/models endpoint?"}
                data-reload-on-success
                aria-label={"Discover models for " (cfg.name)}
            {
                "Discover"
            }
            button
                .btn.btn--sm.btn--icon.btn--ghost-danger
                type="button"
                hx-delete={"/b/llm/api/providers/" (id)}
                hx-confirm={"Delete provider \"" (cfg.name) "\"?"}
                // The page reloads rather than dropping the row in place, so
                // deleting the last provider lands on the empty state.
                hx-swap="none"
                data-reload-on-success
                aria-label={"Delete " (cfg.name)}
                title="Delete provider"
            {
                (icons::trash())
            }
        });
    }
    cells
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
        Err(e) => {
            return super::pages::error_page(
                ctx,
                msg,
                super::pages::Section::Models,
                "Models",
                e,
                "llm models page: list_models failed",
            )
            .await
        }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::admin("LLM Models", "Models")
            .subtitle("Every model the configured providers offer"),
        ui::PageBody::from(render_models_table(&models))
            .with_subnav(super::pages::sections(super::pages::Section::Models)),
    )
    .await
}

/// The models table's columns.
const MODEL_COLUMNS: [components::TableCol<'static>; 5] = [
    components::TableCol::new("Model").primary(),
    components::TableCol::new("Provider"),
    components::TableCol::new("Capabilities"),
    components::TableCol::new("Status"),
    components::TableCol::new("Actions").actions(),
];

/// Render the models table. Pure function of the typed `ModelInfo` slice so
/// the shape can be tested without constructing a `Context`.
///
/// The empty state leads to the Providers page, which is where a model list
/// comes from: a provider's own `models`, or Discover on its row.
fn render_models_table(models: &[ModelInfo]) -> Markup {
    components::DataTable::new(&MODEL_COLUMNS)
        .rows(
            models
                .iter()
                .map(|m| components::TableRow::new(model_cells(m)))
                .collect(),
        )
        .empty_state(
            "No models yet",
            "Models appear here once a provider lists them. Add a provider, or use Discover on one.",
            Some(html! {
                a .btn.btn--primary href="/b/llm/providers" { "Go to providers" }
            }),
        )
        .render()
}

/// One model's cells, in [`MODEL_COLUMNS`] order. Status loads lazily on row
/// mount; actions (Load / Unload) are only meaningful for local backends,
/// so we render them for every row and let the server-side handler 404 /
/// no-op for backends that don't support it — this keeps the UI uniform
/// without hardcoding a local-backend allowlist in the renderer.
fn model_cells(model: &ModelInfo) -> Vec<Markup> {
    let backend_id = model.backend_id.as_str();
    let model_id = model.model_id.as_str();
    let display_name = if model.display_name.is_empty() {
        model_id
    } else {
        model.display_name.as_str()
    };

    let caps = &model.capabilities;
    let capabilities = [
        (caps.streaming, "streaming"),
        (caps.tools, "tools"),
        (caps.vision, "vision"),
        (caps.json_mode, "json"),
    ];
    let any_capability = capabilities.iter().any(|(on, _)| *on);

    let status_url = format!("/b/llm/api/models/{backend_id}/{model_id}/status");
    let load_url = format!("/b/llm/api/models/{backend_id}/{model_id}/load");
    let unload_url = format!("/b/llm/api/models/{backend_id}/{model_id}/unload");

    vec![
        html! {
            strong .data-table__title { (display_name) }
            @if display_name != model_id {
                " "
                code .text-xs { (model_id) }
            }
        },
        components::badge(components::BadgeVariant::Secondary, backend_id),
        html! {
            @if any_capability {
                div .flex .gap-1 .flex-wrap {
                    @for (on, name) in capabilities {
                        @if on { (components::badge(components::BadgeVariant::Secondary, name)) }
                    }
                }
            } @else {
                (components::NO_VALUE)
            }
        },
        // Lazy-load the per-model status so a slow backend doesn't hold up
        // the initial render. The status route answers an htmx request with
        // the badge itself (`routes::models::status_badge`).
        html! {
            span
                .text-muted .text-sm
                hx-get=(status_url)
                hx-trigger="load"
                hx-swap="outerHTML"
            {
                "Checking…"
            }
        },
        html! {
            button
                .btn.btn--sm.btn--secondary
                type="button"
                hx-post=(load_url)
                hx-swap="none"
                hx-confirm={"Load model \"" (model_id) "\" on backend \"" (backend_id) "\"?"}
                aria-label={"Load " (display_name)}
            {
                "Load"
            }
            button
                .btn.btn--sm.btn--ghost
                type="button"
                hx-post=(unload_url)
                hx-swap="none"
                hx-confirm={"Unload model \"" (model_id) "\" on backend \"" (backend_id) "\"?"}
                aria-label={"Unload " (display_name)}
            {
                "Unload"
            }
        },
    ]
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
        let m = provider_form(ProviderForm::New).into_string();

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
            m.contains("No providers yet"),
            "empty-state hint missing; got: {m}"
        );
        // The empty state's call to action opens the same modal the topbar's
        // primary action does.
        assert!(
            m.contains(r#"data-action="modal-open" data-modal-target="add-provider""#),
            "the empty state must offer Add provider; got: {m}"
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

        // Key-var column renders verbatim, no masking/translation (the
        // `<wbr>` break hints `breakable_id` adds are not characters).
        assert!(m.replace("<wbr>", "").contains(EXAMPLE_KEY_VAR));

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
            m.contains("No models yet"),
            "empty-state hint missing; got: {m}"
        );
        // A real way forward, not a sentence naming a button on another page.
        assert!(
            m.contains(
                r#"<a class="btn btn--primary" href="/b/llm/providers">Go to providers</a>"#
            ),
            "the empty state must link to the providers page; got: {m}"
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

    fn sample_configs() -> Vec<(String, ProviderConfig)> {
        vec![(
            "row-1".to_string(),
            ProviderConfig::new(
                "openai-main",
                ProviderProtocol::OpenAiCompatible,
                "https://llm.example.com/v1",
            )
            .with_key_var(EXAMPLE_KEY_VAR)
            .with_max_tokens_field(MaxTokensField::MaxCompletionTokens)
            .with_models(vec!["gpt-4o".into(), "gpt-4o-mini".into()]),
        )]
    }

    /// The list is a `DataTable`, so it collapses to cards on a phone
    /// instead of overflowing the viewport, and every row action is named.
    #[test]
    fn the_providers_list_is_a_data_table_with_named_row_actions() {
        let m = render_providers_table(&sample_configs(), true).into_string();
        assert!(m.contains(r#"<div class="data-table">"#), "got: {m}");
        assert!(
            m.contains(r#"data-action="modal-open" data-modal-target="edit-provider-row-1""#),
            "Edit opens the row's own modal; got: {m}"
        );
        assert!(m.contains(r#"aria-label="Edit openai-main""#), "got: {m}");
        assert!(
            m.contains(r#"aria-label="Discover models for openai-main""#),
            "got: {m}"
        );
        // The delete control is an icon: a ghost-danger icon button whose
        // only name is its label.
        assert!(
            m.contains(r#"class="btn btn--sm btn--icon btn--ghost-danger""#),
            "got: {m}"
        );
        assert!(m.contains(r#"aria-label="Delete openai-main""#), "got: {m}");
    }

    /// The create form waits in a modal and the edit form of each provider
    /// in its own, filled with what the provider holds and patching it.
    #[test]
    fn the_edit_form_patches_the_row_it_was_rendered_for_and_is_filled_in() {
        let configs = sample_configs();
        let (id, cfg) = &configs[0];
        let m = provider_form(ProviderForm::Edit { id, cfg }).into_string();

        assert!(
            m.contains(r#"hx-patch="/b/llm/api/providers/row-1""#),
            "got: {m}"
        );
        assert!(!m.contains("hx-post"), "an edit never creates; got: {m}");
        assert!(m.contains(r#"value="openai-main""#), "got: {m}");
        assert!(
            m.contains(r#"value="https://llm.example.com/v1""#),
            "got: {m}"
        );
        assert!(
            m.contains(&format!(r#"value="{EXAMPLE_KEY_VAR}""#)),
            "got: {m}"
        );
        assert!(m.contains(r#"value="gpt-4o, gpt-4o-mini""#), "got: {m}");
        assert!(
            m.contains(r#"<option value="open_ai_compatible" selected>"#),
            "got: {m}"
        );
        assert!(
            m.contains(r#"<option value="max_completion_tokens" selected>"#),
            "got: {m}"
        );
        // Field ids are the row's, so two edit modals on one page never
        // share a label target.
        assert!(m.contains(r#"for="edit-row-1-name""#), "got: {m}");
        assert!(m.contains(r#"id="edit-row-1-name""#), "got: {m}");
        assert!(m.contains("Save changes"), "got: {m}");
    }

    /// Every select in the Add provider form says which option it starts
    /// on. The modal's Esc guard (chrome.js) compares each option with its
    /// `defaultSelected`; a select left on the browser's implicit first
    /// option counts as changed, and an untouched form asked "Press Esc
    /// again to discard changes".
    #[test]
    fn every_select_in_the_new_provider_form_names_its_starting_option() {
        let m = provider_form(ProviderForm::New).into_string();
        assert_eq!(m.matches("<select").count(), 2, "got: {m}");
        assert_eq!(m.matches(" selected>").count(), 2, "got: {m}");
        assert!(
            m.contains(r#"<option value="open_ai" selected>"#),
            "got: {m}"
        );
    }

    /// A disabled provider's edit form comes up unticked: the box is what
    /// the save sends, so a box ticked by default would re-enable it.
    #[test]
    fn a_disabled_providers_edit_form_is_unticked() {
        let mut cfg = sample_configs().remove(0).1;
        cfg.enabled = false;
        let m = provider_form(ProviderForm::Edit {
            id: "row-1",
            cfg: &cfg,
        })
        .into_string();
        assert!(
            !m.contains("checked"),
            "the Enabled box must reflect the stored value; got: {m}"
        );
        let new = provider_form(ProviderForm::New).into_string();
        assert!(
            new.contains("checked"),
            "a new provider starts enabled; got: {new}"
        );
    }
}
