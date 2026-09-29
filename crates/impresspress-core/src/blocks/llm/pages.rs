//! SSR pages for the LLM orchestrator block.
//!
//! Provides:
//! - Chat page (`GET /b/llm/` and `GET /b/llm/threads/{id}`) — unified
//!   handler renders the canonical `templates::chat_page` shell.
//! - Settings page (`GET /b/llm/settings`) — default provider/model config

use maud::{html, Markup};
use wafer_core::clients::{config, llm::ModelInfo};
use wafer_run::{context::Context, Message, OutputStream};

use super::{
    messages_list, messages_list_contexts, record_field, repo, ContextView, DEFAULT_MODEL_VAR,
    DEFAULT_PROVIDER, DEFAULT_PROVIDER_VAR,
};
use crate::{
    blocks::crud,
    ui::{self, components, icons, shell::Crumb},
};

// ---------------------------------------------------------------------------
// Unified chat page (handles `/b/llm/` and `/b/llm/threads/{id}`)
// ---------------------------------------------------------------------------

/// The thread the page shows: the `{id}` the route table bound for
/// `/b/llm/threads/{id}`, or `None` on the root chat page, whose row binds
/// nothing.
fn selected_thread(msg: &Message) -> Option<&str> {
    Some(msg.var("id")).filter(|id| !id.is_empty())
}

/// Pure render helper for the unified chat page body.
///
/// Takes the already-loaded data (threads, entries, models, defaults) and
/// the optional active thread id, returns the inner page Markup. Kept
/// pure (sync, no `Context`) so the selector-preservation contract can be
/// verified in unit tests without mocking the database client.
fn render_page_body(
    threads: &[ContextView],
    entries: &[serde_json::Value],
    models: &[ModelInfo],
    models_unavailable: bool,
    default_model: &str,
    thread_id: Option<&str>,
    llm_chat_js_url: &str,
) -> Markup {
    // Build messages JSON for the bootstrap carrier. Empty array when no thread.
    let messages_json: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "role": record_field(entry, "role"),
                "content": record_field(entry, "content"),
                "created_at": record_field(entry, "created_at"),
            })
        })
        .collect();
    // Escape `<` so no `</script>` can terminate the application/json carrier
    // element. serde_json does not escape `<`; the browser terminates a
    // <script> element on literal `</script>` regardless of the type attribute.
    let messages_json_str = serde_json::to_string(&messages_json).unwrap_or_else(|_| "[]".into());
    let messages_json_str = crate::ui::script_json_escape(&messages_json_str);

    let thread_list = render_thread_list_pane(threads, thread_id);
    let messages_pane = render_messages_pane(entries, thread_id);
    let composer = render_composer(thread_id);
    let right_rail = render_right_rail(models, models_unavailable, default_model);

    let chat_body =
        crate::ui::templates::chat_page(thread_list, messages_pane, composer, Some(right_rail));

    html! {
        (chat_body)

        // Pulse animation for thinking indicator + blinking cursor.
        style { "@keyframes pulse{0%,100%{opacity:1}50%{opacity:.4}} @keyframes blink{0%,100%{opacity:1}50%{opacity:0}} .typing-cursor{display:inline-block;width:0.5em;height:1.1em;background:var(--text-primary,#333);vertical-align:text-bottom;margin-left:2px;animation:blink 0.8s step-end infinite}" }
        // DOMPurify must load before marked.js/llm-chat.js so `window.DOMPurify`
        // exists when renderMarkdown() sanitizes marked's output (P0 stored-XSS fix).
        script src=(super::assets::purify_js_url()) {}
        // marked.js for markdown rendering — self-hosted (vendored), content-hashed.
        script src=(super::assets::marked_js_url()) {}

        // Server-rendered initial state for the chat module. `messages_json_str`
        // has every literal `<` replaced with the JSON escape sequence
        // "backslash u 0 0 3 c" (see above) so a `</script>` in user-typed
        // message content cannot terminate this element —
        // `type="application/json"` does NOT prevent element termination on
        // its own. The JS module reads it via JSON.parse on init().
        script type="application/json" id="llm-chat-bootstrap" {
            (maud::PreEscaped(messages_json_str))
        }
        script src=(llm_chat_js_url) defer {}
        script {
            (maud::PreEscaped("window.addEventListener('DOMContentLoaded', function(){ if (window.impresspressLlmChat) window.impresspressLlmChat.init(); });"))
        }
    }
}

/// What the model picker says when the remote model list could not be read.
///
/// The list is the one read on the chat page that is genuinely optional: the
/// picker's "Default (remote)" entry routes without it, and the browser-side
/// WebLLM models in `#local-models-group` never needed it — so a failure here
/// marks the picker instead of failing the page. It must still be marked:
/// an empty `<optgroup>` is exactly what a deployment with no remote models
/// renders.
const MODELS_UNAVAILABLE: &str = "Remote model list unavailable";

/// Unified handler for `/b/llm/` and `/b/llm/threads/{id}`.
///
/// Renders the canonical `templates::chat_page` (thread list / messages /
/// composer / right rail). The optional thread id from the URL drives
/// composer enablement and message preloading.
pub async fn page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let thread_id = selected_thread(msg);

    // Load the thread list (sidebar) and the selected thread's entries
    // through `impresspress/messages`, the block that owns both tables — the
    // same way this block already writes them. The list route is
    // owner-scoped, so the sidebar shows the caller's own threads.
    //
    // Both propagate. An empty sidebar and an empty message pane are what a
    // brand-new account renders, and this page has already shipped both of
    // them for a fortnight on a read that answered 404 to every request —
    // exactly because these two lines swallowed it.
    let threads = match messages_list_contexts(ctx, msg).await {
        Ok(threads) => threads,
        Err(e) => return crud::db_error_page(msg, e, "llm chat page: thread list failed"),
    };

    // Entries for the selected thread, if any. Empty when no thread is
    // selected — that is the one genuinely empty case here.
    let entries = match thread_id {
        Some(tid) => match messages_list(ctx, msg, tid).await {
            Ok(entries) => entries,
            Err(e) => return crud::db_error_page(msg, e, "llm chat page: entry list failed"),
        },
        None => Vec::new(),
    };

    // Resolve display title from the loaded thread record (when present).
    let thread_title = thread_id
        .and_then(|tid| threads.iter().find(|thread| thread.id == tid))
        .map(|thread| thread.title.clone())
        .filter(|title| !title.is_empty());
    let display_title = thread_title.as_deref().unwrap_or("Chat");

    // See [`MODELS_UNAVAILABLE`]: this one is optional, so it marks the
    // picker instead of failing the page — but it is marked, not dropped.
    let (models, models_unavailable) = match load_models(ctx).await {
        Ok(models) => (models, false),
        Err(e) => {
            tracing::error!(error = %e, "llm chat page: remote model list failed");
            (Vec::new(), true)
        }
    };
    let default_model = match config::get_default(ctx, DEFAULT_MODEL_VAR, "").await {
        Ok(model) => model,
        Err(e) => return crud::db_error_page(msg, e, "llm chat page: default model read failed"),
    };

    let llm_chat_js_url = super::assets::llm_chat_js_url();
    let content = render_page_body(
        &threads,
        &entries,
        &models,
        models_unavailable,
        default_model.as_str(),
        thread_id,
        &llm_chat_js_url,
    );

    // Build mobile-friendly crumbs:
    //  - On /b/llm/: just `[Chat]`.
    //  - On /b/llm/threads/{id}: `[Threads] / [thread title]` so the mobile
    //    single-pane view has a visible back-link to the thread list.
    let crumbs = match thread_id {
        Some(_) => vec![
            Crumb {
                label: "Threads",
                href: Some("/b/llm/"),
            },
            Crumb {
                label: display_title,
                href: None,
            },
        ],
        None => vec![Crumb {
            label: "Chat",
            href: None,
        }],
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell {
            title: display_title,
            nav: ui::NavKind::Admin,
            crumbs,
            subtitle: Some("Chat with a configured provider or local model"),
            primary_action: None,
        },
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// chat_page render helpers (consumed by the unified `page` handler above)
// ---------------------------------------------------------------------------

/// Thread-list pane for the chat_page template. Includes the section
/// header + "+" new-thread button + the scrollable list. Pure function of
/// the loaded threads and the (optional) active thread id.
fn render_thread_list_pane(threads: &[ContextView], active_id: Option<&str>) -> Markup {
    html! {
        div .thread-pane {
            div .thread-pane__head {
                h2 .thread-pane__title {
                    "Threads"
                }
                button .btn.btn--sm.btn--primary type="button" data-action="llm-new-thread" {
                    (icons::plus())
                }
            }
            div #thread-list .thread-pane__scroll {
                (thread_list_items(threads, active_id))
            }
        }
    }
}

fn thread_list_items(threads: &[ContextView], active_id: Option<&str>) -> Markup {
    html! {
        @if threads.is_empty() {
            div .text-center .text-muted .thread-pane__empty {
                "No threads yet."
            }
        } @else {
            @for thread in threads {
                @let id = thread.id.as_str();
                @let title = thread.title.as_str();
                @let updated_at = thread.updated_at.as_str();
                @let date = updated_at.get(..10).unwrap_or(updated_at);
                @let is_active = active_id == Some(id);
                a
                    .card .thread-card
                    href={"/b/llm/threads/" (id)}
                    data-thread-id=(id)
                    data-active=(if is_active { "true" } else { "false" })
                    aria-current=[is_active.then_some("page")]
                {
                    div .thread-card__row {
                        span .thread-card__title {
                            @if title.is_empty() { "Untitled" } @else { (title) }
                        }
                        @if !date.is_empty() {
                            span .text-muted .thread-card__date { (date) }
                        }
                    }
                }
            }
        }
    }
}

/// Messages pane for the chat_page template. When no thread is selected,
/// shows the "Create a thread first" empty-state element. When a thread
/// IS selected, renders an empty `#messages-area` that the JS bootstrap
/// fills from the `<script type="application/json" id="llm-chat-bootstrap">`
/// carrier emitted by `render_page_body`.
fn render_messages_pane(_entries: &[serde_json::Value], thread_id: Option<&str>) -> Markup {
    html! {
        // The chat_page template's `.chat-messages` wrapper owns scroll,
        // padding, and surface for this pane (same lesson as the Messages
        // block — see render_conversation_messages there). The old inline
        // `height:100%;overflow-y:auto;background:var(--bg-secondary)` painted
        // a grey slab inside the white pane and double-scrolled.
        div #messages-area {
            @if thread_id.is_none() {
                div #no-thread-prompt .chat-empty-state {
                    div .chat-empty-state__icon { (ui::icons::message_square()) }
                    p .text-lg .mb-2 { "Start a new conversation" }
                    p .text-muted .mb-6 { "Click the " strong { "+" } " button to create a thread, then type your message." }
                }
            }
            // When thread_id is Some, the JS bootstrap fills #messages-area
            // by JSON.parse-ing the #llm-chat-bootstrap carrier on init().
        }
    }
}

/// Composer pane for the chat_page template. Disabled state when no
/// thread is selected (matches the original empty-state behavior).
fn render_composer(thread_id: Option<&str>) -> Markup {
    let enabled = thread_id.is_some();
    let thread_value = thread_id.unwrap_or("");
    let placeholder = if enabled {
        "Type your message..."
    } else {
        "Create a thread first..."
    };

    html! {
        form
            id="chat-form"
            class={ "chat-form" @if !enabled { " chat-form--disabled" } }
            data-thread=(thread_value)
        {
            input type="hidden" name="thread_id" id="active-thread-id" value=(thread_value);
            div .flex .gap-2 .items-end {
                div .flex-1 .relative {
                    textarea
                        .form-input .resize-none .w-full
                        #chat-input
                        name="message"
                        placeholder=(placeholder)
                        rows="3"
                        required
                        disabled[!enabled]
                        data-submit-on-enter
                    {}
                }
                div .flex .flex-col .items-center .gap-1 {
                    button #send-btn .btn.btn--primary .h-fit type="submit" disabled[!enabled] {
                        "Send"
                    }
                    span #send-status .text-muted .text-xs .nowrap {}
                }
            }
        }
    }
}

/// Right-rail pane for the chat_page template. Holds the model picker,
/// model loading progress container, and a link to the LLM settings
/// page. Replaces the inline above-messages model strip from the old
/// chat_page handler.
fn render_right_rail(
    models: &[ModelInfo],
    models_unavailable: bool,
    default_model: &str,
) -> Markup {
    html! {
        div .flex .flex-col .gap-4 .p-2 {
            div {
                label .form-label .d-block .text-sm { "Model" }
                select
                    #model-picker
                    .form-input .w-full
                    name="model"
                    data-action="llm-model-change"
                {
                    optgroup label="Remote" {
                        option value="" selected[default_model.is_empty()] { "Default (remote)" }
                        (render_model_picker(models, default_model))
                    }
                    optgroup #local-models-group label="Local (WebLLM)" {}
                }
                // Rendered only when the list could not be read, so a healthy
                // page is byte-identical. `#model-status` beside it is owned
                // by llm-chat.js and is overwritten at runtime.
                @if models_unavailable {
                    span .text-muted .d-block .mt-1 .text-xs { (MODELS_UNAVAILABLE) }
                }
                span #model-status .text-muted .d-block .mt-1 .text-xs {}
            }

            div #model-progress-container .hidden {
                div .card .p-3 {
                    div .flex .items-center .gap-2 .mb-2 {
                        span .text-sm .font-medium { "Loading model..." }
                        button #model-unload-btn .btn.btn--sm.btn--ghost type="button" data-action="llm-unload-model" .ml-auto {
                            "Cancel"
                        }
                    }
                    div .model-progress-track {
                        // NB: the token is `--primary-color` — the old
                        // `var(--primary, #3b82f6)` referenced a nonexistent
                        // var, so the blue fallback ALWAYS won. `.model-progress-fill`'s
                        // width starts at 0% and is updated at runtime by
                        // `bar.style.width = pct + '%'` in blocks/llm/assets/llm-chat.js.
                        div #model-progress-bar .model-progress-fill {}
                    }
                    div #model-progress-text .text-muted .text-xs .mt-1 { "" }
                }
            }

            a .btn.btn--ghost.btn--sm .justify-start href="/b/llm/settings" {
                (ui::icons::settings()) " Settings"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Settings page
// ---------------------------------------------------------------------------

pub async fn settings_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let default_provider = match config::get_default(ctx, DEFAULT_PROVIDER_VAR, DEFAULT_PROVIDER)
        .await
    {
        Ok(provider) => provider,
        Err(e) => {
            return crud::db_error_page(msg, e, "llm settings page: default provider read failed")
        }
    };
    let default_model = match config::get_default(ctx, DEFAULT_MODEL_VAR, "").await {
        Ok(model) => model,
        Err(e) => {
            return crud::db_error_page(msg, e, "llm settings page: default model read failed")
        }
    };

    // Load per-thread overrides. An empty list means "no thread pins a
    // provider" — the state an untouched deployment is in — so an unreadable
    // settings table fails the page rather than borrowing that sentence.
    let overrides = match repo::settings::list_all(ctx).await {
        Ok(rows) => rows,
        Err(e) => return crud::db_error_page(msg, e, "llm settings page: override read failed"),
    };

    let content = html! {
        (components::page_header(
            "LLM Settings",
            Some("Configure default provider and model"),
            None,
        ))

        // Global defaults — read-only display; set via env vars
        div .card .mb-6 {
            h3 .card-title .mb-4 { "Global Defaults" }
            p .text-muted .text-sm .mb-4 {
                "Global defaults are configured via environment variables."
            }
            div .form-row {
                div .form-group {
                    // `for`/`id` pair: both fields showed a visible
                    // `.form-label` but never associated it, so the accessible
                    // name was empty and a screen reader announced only the
                    // value.
                    label .form-label for="llm-default-provider" { "Default Provider" }
                    input #llm-default-provider
                        .form-input .form-input--readonly
                        type="text"
                        value=(default_provider)
                        readonly
                    ;
                    p .form-hint {
                        "Set via " code { (DEFAULT_PROVIDER_VAR) }
                    }
                }
                div .form-group {
                    label .form-label for="llm-default-model" { "Default Model" }
                    input #llm-default-model
                        .form-input .form-input--readonly
                        type="text"
                        value=(default_model)
                        placeholder="(provider default)"
                        readonly
                    ;
                    p .form-hint {
                        "Set via " code { (DEFAULT_MODEL_VAR) }
                    }
                }
            }
        }

        // Per-thread overrides
        div .card {
            h3 .card-title .mb-4 { "Per-Thread Overrides" }
            @if overrides.truncated {
                p .form-hint { "Showing the first " (overrides.rows.len()) " overrides." }
            }
            @if overrides.rows.is_empty() {
                div .empty-state {
                    "No thread overrides configured."
                }
            } @else {
                div .table-container {
                    table .table {
                        thead {
                            tr {
                                th { "Thread ID" }
                                th { "Provider Block" }
                                th { "Model" }
                                th { "Updated" }
                                th { "Actions" }
                            }
                        }
                        tbody {
                            @for ov in &overrides.rows {
                                @let tid = ov.thread_id.as_str();
                                @let pb = ov.provider_block.as_str();
                                @let model = ov.model.as_str();
                                @let updated = ov.updated_at.as_str();
                                @let date = updated.get(..10).unwrap_or(updated);
                                tr {
                                    td {
                                        a .font-mono .text-xs href={"/b/llm/threads/" (tid)} {
                                            (tid)
                                        }
                                    }
                                    td {
                                        @if pb.is_empty() {
                                            span .text-muted { "(default)" }
                                        } @else {
                                            code .text-xs { (pb) }
                                        }
                                    }
                                    td {
                                        @if model.is_empty() {
                                            span .text-muted { "(default)" }
                                        } @else {
                                            code .text-xs { (model) }
                                        }
                                    }
                                    td .text-muted .text-xs { (date) }
                                    td {
                                        button
                                            .btn.btn--sm.btn--danger
                                            hx-delete={"/b/llm/api/config/" (ov.id)}
                                            hx-confirm={"Remove override for thread " (tid) "?"}
                                            hx-target="closest tr"
                                            hx-swap="outerHTML"
                                        {
                                            (icons::trash())
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell {
            title: "LLM Settings",
            nav: ui::NavKind::Admin,
            crumbs: vec![Crumb {
                label: "Settings",
                href: None,
            }],
            subtitle: Some("LLM defaults and provider routing"),
            primary_action: None,
        },
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Load the aggregated list of available models from the `wafer-run/llm`
/// service block.
///
/// One typed call to `wafer_core::clients::llm::list_models(ctx)` — page
/// rendering inlines the picker options from the returned `Vec<ModelInfo>`
/// without an HTTP roundtrip or a JSON re-encode.
///
/// Returns `Err` when the service block could not be reached or its answer
/// could not be decoded. The caller does not fail the page for it — see
/// [`MODELS_UNAVAILABLE`] for why this one read is optional — but it does not
/// get to silently look like a deployment with no remote models either.
async fn load_models(ctx: &dyn Context) -> Result<Vec<ModelInfo>, wafer_run::WaferError> {
    wafer_core::clients::llm::list_models(ctx).await
}

/// Render the `<option>` list for the remote-model picker.
///
/// Each option carries the bare `model_id` as its `value` and the `backend_id`
/// as a `data-backend-id` attribute — two separate fields, NOT a
/// `"{backend_id}:{model_id}"` composite. The composite was ambiguous (model
/// ids such as `llama3:8b` contain colons) and was forwarded to the backend
/// verbatim as the model id; the JS now sends `model` + `provider` separately.
/// The visible label prefers `display_name`, falling back to `model_id`, with
/// `backend_id` appended in parens to disambiguate the same model on different
/// backends.
///
/// Entries with an empty `model_id` are skipped — the resulting
/// `<option value="">` would collide with the "Default (remote)" entry and
/// pick the wrong model on submit.
fn render_model_picker(models: &[ModelInfo], default_model: &str) -> Markup {
    html! {
        @for m in models {
            @let backend_id = m.backend_id.as_str();
            @let model_id = m.model_id.as_str();
            @if !model_id.is_empty() {
                @let display = if m.display_name.is_empty() {
                    model_id
                } else {
                    m.display_name.as_str()
                };
                option
                    value=(model_id)
                    data-backend-id=(backend_id)
                    selected[model_id == default_model]
                {
                    (display)
                    @if !backend_id.is_empty() {
                        " (" (backend_id) ")"
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

    /// Empty input produces no `<option>` tags — the "Default (remote)" entry
    /// rendered alongside this helper stays the only option.
    #[test]
    fn render_model_picker_empty_list_emits_no_options() {
        let markup = render_model_picker(&[], "");
        let html = markup.into_string();
        assert!(
            !html.contains("<option"),
            "expected zero <option> tags for empty model list, got: {html}"
        );
    }

    /// Build a `ModelInfo` for picker tests. Capabilities are irrelevant to
    /// the picker, so they default.
    fn mi(backend_id: &str, model_id: &str, display_name: &str) -> ModelInfo {
        ModelInfo::new(backend_id, model_id, display_name)
    }

    /// A typical aggregated `Vec<ModelInfo>` payload renders one option per
    /// entry with the bare `model_id` value, a `data-backend-id` attribute, and
    /// the `display_name` label.
    #[test]
    fn render_model_picker_typical_list_emits_one_option_per_model() {
        let models = vec![
            mi("openai", "gpt-4o", "GPT-4o"),
            mi("anthropic", "claude-3-5-sonnet", "Claude 3.5 Sonnet"),
        ];
        let markup = render_model_picker(&models, "claude-3-5-sonnet");
        let html = markup.into_string();

        // One <option> per model entry.
        assert_eq!(
            html.matches("<option").count(),
            2,
            "expected 2 <option> tags, got: {html}"
        );
        // Value is the bare model_id; backend_id rides in data-backend-id —
        // never a `backend:model` composite.
        assert!(
            html.contains(r#"value="gpt-4o" data-backend-id="openai""#),
            "missing openai option shape: {html}"
        );
        assert!(
            !html.contains(r#"value="openai:gpt-4o""#),
            "must not emit the ambiguous composite value: {html}"
        );
        // Human label comes from display_name.
        assert!(html.contains("GPT-4o"), "missing GPT-4o label: {html}");
        assert!(
            html.contains("Claude 3.5 Sonnet"),
            "missing Claude label: {html}"
        );
        // Backend id is appended in parens for disambiguation.
        assert!(html.contains("(openai)"), "missing backend suffix: {html}");
        // The entry whose model_id matches default_model is pre-selected.
        assert!(
            html.contains(r#"value="claude-3-5-sonnet" data-backend-id="anthropic" selected"#),
            "expected selected attr on matching default_model entry: {html}"
        );
    }

    /// Entries with a blank `model_id` must be skipped rather than rendered as
    /// a junk `value=""` option (which would collide with the "Default
    /// (remote)" entry rendered alongside). Empty `display_name` falls back to
    /// the model id. Empty `backend_id` renders a value without the `:` prefix
    /// and no parenthesized suffix.
    #[test]
    fn render_model_picker_skips_malformed_entries() {
        let models = vec![
            // Blank model_id — skip.
            mi("openai", "", "Orphan"),
            // Empty display_name — label falls back to model_id.
            mi("openai", "gpt-4o-mini", ""),
            // Empty backend_id — value has no `:` prefix, no parens suffix.
            mi("", "solo-model", "Solo"),
        ];
        let markup = render_model_picker(&models, "");
        let html = markup.into_string();

        // Two valid entries out of three (the blank-model_id one is skipped).
        assert_eq!(
            html.matches("<option").count(),
            2,
            "expected 2 <option> tags, got: {html}"
        );
        // Empty display_name falls back to model id as the visible label; the
        // value is the bare model_id with backend_id in data-backend-id.
        assert!(
            html.contains(r#"value="gpt-4o-mini" data-backend-id="openai""#),
            "expected gpt-4o-mini option shape: {html}"
        );
        assert!(
            html.contains("gpt-4o-mini"),
            "expected gpt-4o-mini label fallback: {html}"
        );
        // No backend_id — value is the bare model_id, empty data-backend-id, no
        // `(...)` suffix.
        assert!(
            html.contains(r#"value="solo-model""#),
            "expected bare-model value when backend_id is missing: {html}"
        );
        assert!(
            !html.contains("(solo-model)"),
            "expected no parens suffix when backend_id is missing: {html}"
        );
        // The orphan with no model_id must NOT appear as value="".
        assert!(
            !html.contains(r#"value="""#),
            "expected no empty-value <option>, got: {html}"
        );
    }

    // ----- Task 2 helpers: render_thread_list_pane / render_messages_pane /
    //       render_composer / render_right_rail -----

    fn make_thread(id: &str, title: &str, updated_at: &str) -> ContextView {
        ContextView {
            id: id.to_string(),
            title: title.to_string(),
            updated_at: updated_at.to_string(),
        }
    }

    #[test]
    fn render_thread_list_pane_empty() {
        let html = render_thread_list_pane(&[], None).into_string();
        assert!(
            html.contains("No threads yet"),
            "empty hint missing: {html}"
        );
        assert!(
            html.contains(r#"data-action="llm-new-thread""#),
            "new-thread button missing"
        );
        assert!(html.contains("Threads"));
    }

    #[test]
    fn render_thread_list_pane_marks_active_thread() {
        let t = make_thread("thread-42", "My chat", "2026-05-05T10:00:00Z");
        let html = render_thread_list_pane(&[t], Some("thread-42")).into_string();
        assert!(html.contains("My chat"));
        assert!(html.contains(r#"data-thread-id="thread-42""#));
        assert!(
            html.contains("data-active=\"true\"") || html.contains("aria-current"),
            "active thread should be marked: {html}"
        );
    }

    #[test]
    fn render_messages_pane_empty_renders_no_thread_prompt() {
        let html = render_messages_pane(&[], None).into_string();
        assert!(html.contains(r#"id="no-thread-prompt""#));
        assert!(html.contains("Start a new conversation"));
    }

    #[test]
    fn render_messages_pane_with_thread_renders_messages_area() {
        let html = render_messages_pane(&[], Some("thread-1")).into_string();
        assert!(html.contains(r#"id="messages-area""#));
        assert!(!html.contains(r#"id="no-thread-prompt""#));
    }

    #[test]
    fn render_composer_disabled_when_no_thread() {
        let html = render_composer(None).into_string();
        assert!(html.contains(r#"id="chat-form""#));
        assert!(html.contains(r#"id="active-thread-id""#));
        assert!(
            html.contains(r#"value="""#),
            "thread id hidden input should be empty"
        );
        assert!(html.contains("disabled"), "composer should be disabled");
        assert!(html.contains("Create a thread first"));
    }

    #[test]
    fn render_composer_enabled_with_thread() {
        let html = render_composer(Some("thread-7")).into_string();
        assert!(html.contains(r#"id="chat-form""#));
        assert!(html.contains(r#"value="thread-7""#));
        assert!(!html.contains("Create a thread first"));
    }

    #[test]
    fn render_right_rail_contains_picker_progress_settings() {
        let models: Vec<ModelInfo> = vec![];
        let html = render_right_rail(&models, false, "").into_string();
        assert!(html.contains(r#"id="model-picker""#));
        assert!(html.contains(r#"id="model-progress-container""#));
        assert!(html.contains(r#"id="local-models-group""#));
        assert!(html.contains("/b/llm/settings"), "settings link missing");
        assert!(html.contains(r#"label="Remote""#));
    }

    // ----- Task 3: parse_thread_id + render_page_body -----

    /// The page reads the thread id the route table bound for
    /// `/b/llm/threads/{id}`; the root chat page binds nothing.
    fn routed_page_msg(path: &str) -> Message {
        let mut msg = Message::new(format!("retrieve:{path}"));
        msg.set_meta(wafer_run::META_REQ_ACTION, "retrieve");
        msg.set_meta(wafer_run::META_REQ_RESOURCE, path);
        assert!(
            crate::endpoint_match::dispatch(&mut msg, crate::blocks::llm::ROUTES).is_some(),
            "no llm route matches GET {path}"
        );
        msg
    }

    #[test]
    fn selected_thread_is_none_at_the_root_page() {
        assert_eq!(selected_thread(&routed_page_msg("/b/llm/")), None);
        // The bare form is routed through the matcher's trailing-slash retry.
        assert_eq!(selected_thread(&routed_page_msg("/b/llm")), None);
    }

    #[test]
    fn selected_thread_is_the_bound_id_on_a_thread_page() {
        assert_eq!(
            selected_thread(&routed_page_msg("/b/llm/threads/abc-123")),
            Some("abc-123")
        );
    }

    /// At root URL, the page body shows the no-thread prompt and a
    /// disabled composer.
    #[test]
    fn page_body_renders_empty_state_at_root() {
        let html = render_page_body(&[], &[], &[], false, "", None, "/b/static/llm-chat-test.js")
            .into_string();
        assert!(html.contains(r#"id="no-thread-prompt""#));
        assert!(html.contains("Start a new conversation"));
        assert!(html.contains(r#"id="chat-form""#));
        assert!(
            html.contains("chat-form--disabled"),
            "composer should carry the disabled-state class"
        );
    }

    /// With a thread id, the body wires the active thread into the
    /// composer's hidden input and drops the empty-state prompt.
    #[test]
    fn page_body_renders_with_thread_id() {
        let threads = vec![make_thread("some-id", "Some Chat", "2026-05-05T10:00:00Z")];
        let html = render_page_body(
            &threads,
            &[],
            &[],
            false,
            "",
            Some("some-id"),
            "/b/static/llm-chat-test.js",
        )
        .into_string();
        assert!(html.contains(r#"value="some-id""#));
        assert!(!html.contains(r#"id="no-thread-prompt""#));
        assert!(!html.contains("chat-form--disabled"));
    }

    /// The page body must include the external <script src> for the static
    /// llm-chat.js asset, and must NOT include the deleted inline JS
    /// constants. Markers from the old SHARED_JS / CHAT_JS / THREAD_JS
    /// must be absent.
    #[test]
    fn page_body_includes_external_llm_chat_js_and_drops_inline_constants() {
        let url = "/b/static/llm-chat-deadbeef.js";
        let html = render_page_body(&[], &[], &[], false, "", None, url).into_string();
        assert!(
            html.contains(&format!(r#"src="{url}""#)),
            "missing external llm-chat.js script tag (expected src={url}): {html}"
        );
        assert!(html.contains("impresspressLlmChat.init"));
        // No leaked giant inline JS — these are markers from the old SHARED_JS.
        assert!(!html.contains("function handleChatSubmit"));
        assert!(!html.contains("function selectThread"));
        assert!(!html.contains("function createNewThread"));
    }

    /// DOMPurify must load before both marked.js and llm-chat.js — the P0
    /// fix relies on `window.DOMPurify` existing by the time llm-chat.js's
    /// `defer`red script runs `renderMarkdown()`. Order matters: a script
    /// tag present but placed after llm-chat.js would not help since the
    /// bug is about sanitizing marked's output, not about a race — but
    /// pinning the order here guards against a future edit silently
    /// reordering these tags and defeating the sanitizer at parse time.
    #[test]
    fn page_body_loads_purify_before_marked_and_llm_chat_js() {
        let url = "/b/static/llm-chat-deadbeef.js";
        let html = render_page_body(&[], &[], &[], false, "", None, url).into_string();

        let purify_url = crate::blocks::llm::assets::purify_js_url();
        let marked_url = crate::blocks::llm::assets::marked_js_url();
        assert!(
            html.contains(&format!(r#"src="{purify_url}""#)),
            "missing external purify.js script tag (expected src={purify_url}): {html}"
        );
        assert!(
            html.contains(&format!(r#"src="{marked_url}""#)),
            "missing external marked.js script tag (expected src={marked_url}): {html}"
        );

        let purify_pos = html.find(&purify_url).expect("purify script tag present");
        let marked_pos = html.find(&marked_url).expect("marked script tag present");
        let llm_chat_pos = html.find(url).expect("llm-chat.js script tag present");
        assert!(
            purify_pos < marked_pos,
            "purify.js must be loaded before marked.js: {html}"
        );
        assert!(
            marked_pos < llm_chat_pos,
            "marked.js must be loaded before llm-chat.js: {html}"
        );
    }

    /// Selector preservation contract — every ID the JS module depends on
    /// must appear in the rendered markup of the with-thread page. Single
    /// guard test; failure points at exactly which selector regressed.
    #[test]
    fn page_body_preserves_required_selectors() {
        let threads = vec![make_thread("sel-test", "Sel Chat", "2026-05-05T10:00:00Z")];
        let html = render_page_body(
            &threads,
            &[],
            &[],
            false,
            "",
            Some("sel-test"),
            "/b/static/llm-chat-test.js",
        )
        .into_string();

        let required_ids = [
            "chat-form",
            "chat-input",
            "active-thread-id",
            "messages-area",
            "model-picker",
            "thread-list",
            "model-progress-container",
            "model-progress-bar",
            "model-progress-text",
            "model-unload-btn",
            "local-models-group",
            "model-status",
            "send-btn",
            "send-status",
        ];
        for id in required_ids {
            assert!(
                html.contains(&format!(r#"id="{id}""#)),
                "selector preservation contract violated — missing id={id}; render: {html}"
            );
        }
    }

    /// One entry as the messages block delivers it — the `{id, data: {…}}`
    /// envelope `records_of` hands back — with the given `content` and
    /// placeholder role/created_at. Mirrors `make_thread` above.
    fn record_with_content(content: &str) -> serde_json::Value {
        serde_json::json!({
            "id": "e-1",
            "data": {
                "role": "user",
                "content": content,
                "created_at": "2026-05-05T10:00:00Z",
            }
        })
    }

    /// XSS regression — a thread whose message content contains a literal
    /// `</script>` sequence must NOT appear verbatim in the rendered page.
    /// serde_json does not escape `<`, and the browser terminates a
    /// `<script>` element on the first literal `</script>` byte sequence
    /// regardless of the `type="application/json"` attribute — so an
    /// unescaped `<` would close the carrier early and let the rest of the
    /// entry content run as live script.
    #[test]
    fn render_page_body_carrier_escapes_script_close() {
        // An entry whose content contains a literal </script> must NOT appear
        // verbatim in the rendered page — it would terminate the JSON carrier
        // and inject live script. `type="application/json"` does NOT prevent
        // element termination.
        let entries = vec![record_with_content("</script><img src=x onerror=alert(1)>")];
        let markup = render_page_body(&[], &entries, &[], false, "", None, "/x.js");
        let html = markup.into_string();
        assert!(
            !html.contains("</script><img"),
            "raw </script> must not survive into the carrier: {html}"
        );
        assert!(
            html.contains("\\u003c/script"),
            "the < of </script> must be JSON-escaped as \\u003c"
        );
    }

    /// The body wraps in the canonical `templates::chat_page` shell (page
    /// class + right-rail aside).
    #[test]
    fn page_body_emits_chat_page_template_class() {
        let html = render_page_body(&[], &[], &[], false, "", None, "/b/static/llm-chat-test.js")
            .into_string();
        assert!(
            html.contains(r#"class="page--chat""#),
            "expected templates::chat_page wrapper class"
        );
        assert!(
            html.contains(r#"class="chat-rail""#),
            "right rail expected (LLM enables it)"
        );
    }
}

#[cfg(test)]
mod messages_boundary_tests {
    use wafer_run::InputStream;

    use super::*;
    use crate::blocks::llm::routes::test_support::{admin_msg, routed, RecordedCall, RecordingCtx};

    /// The chat page reads the messages block's rows through the messages
    /// block, not out of its tables.
    ///
    /// Both reads were `db::list` against `impresspress__messages__{contexts,
    /// entries}` — the reason `messages_schema.rs` existed and the reason
    /// `messages/mod.rs` had to grant `impresspress/llm` read access to two
    /// tables it does not own, while the same page's writes already went
    /// through `ctx.call_block`. A recording context proves the direction:
    /// two calls to `impresspress/messages`, and none to
    /// `wafer-run/database`.
    #[tokio::test]
    async fn the_chat_page_reads_threads_and_entries_through_the_messages_block() {
        // The entries answer is scripted first because first match wins and
        // the thread-list path is a prefix of the entries path. (Before
        // `util::block_request` split the query string off, the thread-list
        // fragment ended in `?`, which is what kept the two apart.)
        let ctx = RecordingCtx::default()
            .answering(
                "/entries",
                serde_json::json!({
                    "records": [{
                        "id": "e1",
                        "data": {
                            "role": "user",
                            "content": "hello",
                            "created_at": "2026-09-06T10:00:00Z",
                        }
                    }],
                    "total_count": 1,
                }),
            )
            .answering(
                "/b/messages/api/contexts",
                serde_json::json!({
                    "records": [{
                        "id": "t1",
                        "data": {
                            "title": "Renewal questions",
                            "updated_at": "2026-09-06T10:00:00Z",
                        }
                    }],
                    "total_count": 1,
                }),
            );

        let msg = routed(admin_msg("retrieve", "/b/llm/threads/t1"));
        let out = page(&ctx, &msg).await;
        let html = match out.collect_buffered().await {
            Ok(buf) => String::from_utf8(buf.body).expect("utf-8 page"),
            other => panic!("the chat page must render: {other:?}"),
        };

        let calls = ctx.calls();
        let to_messages: Vec<&RecordedCall> = calls
            .iter()
            .filter(|call| call.block_name == "impresspress/messages")
            .collect();
        // The path is the *path*, and the filter rides in `req.query.*`, as
        // it does on a real request. This assertion used to read
        // `"…/contexts?page_size=50"` — the whole URL sitting in
        // `req.resource`, where `endpoint_match::dispatch` compares it
        // segment by segment against the route template. It matched nothing,
        // so both reads answered 404 and both callers swallowed it: the
        // sidebar and the model history were empty on every request
        // (`util::block_request` splits the query off now).
        assert_eq!(
            to_messages
                .iter()
                .map(|call| call.msg.path())
                .collect::<Vec<_>>(),
            vec![
                "/b/messages/api/contexts",
                "/b/messages/api/contexts/t1/entries",
            ],
            "the sidebar and the message pane are both read through the block"
        );
        assert_eq!(to_messages[0].msg.query("page_size"), "50");
        assert_eq!(to_messages[1].msg.query("kind"), "message");
        assert!(
            !calls
                .iter()
                .any(|call| call.block_name == "wafer-run/database"),
            "the page must issue no database call of its own: {:?}",
            calls
                .iter()
                .map(|call| call.block_name.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            html.contains("Renewal questions"),
            "the thread the messages block reported must appear in the sidebar"
        );
        assert!(
            html.contains("hello"),
            "the entry the messages block reported must reach the bootstrap carrier"
        );
    }

    /// The caller's identity is forwarded, which is what makes the sidebar
    /// owner-scoped: `GET /b/messages/api/contexts` filters on
    /// `owner_id = msg.user_id()` for every caller.
    #[tokio::test]
    async fn the_thread_list_call_carries_the_callers_identity() {
        let ctx = RecordingCtx::default();
        let _ = messages_list_contexts(&ctx, &admin_msg("retrieve", "/b/llm/")).await;

        let calls = ctx.calls();
        let call = calls
            .iter()
            .find(|call| call.block_name == "impresspress/messages")
            .expect("the thread list is a messages call");
        assert_eq!(call.msg.get_meta("auth.user_id"), "admin-user");
        assert_eq!(call.msg.get_meta("auth.user_roles"), "admin");
        assert_eq!(call.msg.action(), "retrieve");
        assert_eq!(call.msg.get_meta("http.method"), "GET");
        assert!(
            call.body.is_empty(),
            "a list is a GET with no body, not a query smuggled into one"
        );
    }

    /// A failing thread list is an error the page can see, not an empty
    /// sidebar. (The page still renders an empty list — the SSR error
    /// discipline is Phase 3 — but the helper reports it.)
    #[tokio::test]
    async fn a_failing_thread_list_is_an_error() {
        struct Failing;
        #[async_trait::async_trait]
        impl Context for Failing {
            async fn call_block(
                &self,
                _block: &str,
                _msg: Message,
                _input: InputStream,
            ) -> OutputStream {
                OutputStream::error(wafer_run::WaferError::new(
                    wafer_run::ErrorCode::PermissionDenied,
                    "denied",
                ))
            }
            /// Admits nothing, as the fail-closed `check_resource_access` default
            /// this context keeps does.
            fn resource_access_admitted(
                &self,
                _resource: &str,
                _resource_type: wafer_run::ResourceType,
                _access: wafer_block::ResourceAccess,
            ) -> bool {
                false
            }
            fn is_cancelled(&self) -> bool {
                false
            }
            fn config_get(&self, _key: &str) -> Option<&str> {
                None
            }
            fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
                std::sync::Arc::new(Failing)
            }
        }

        assert_eq!(
            messages_list_contexts(&Failing, &admin_msg("retrieve", "/b/llm/"))
                .await
                .expect_err("the refusal surfaces")
                .code,
            wafer_run::ErrorCode::PermissionDenied
        );
    }
}

#[cfg(test)]
mod outage_tests {
    //! The chat page during an outage — and the reason this file is not a
    //! cosmetic sweep.
    //!
    //! PR #24 moved both of this page's reads onto `call_block` with a query
    //! string that `endpoint_match::dispatch` could never match, so both
    //! 404'd on every request: the thread sidebar was empty and the model
    //! history was gone. It merged green because BOTH callers swallowed the
    //! failure into an empty list, and because the test that covered it
    //! asserted the call had been MADE. PR #29 found it end-to-end.
    //!
    //! The assertions here are the ones that were missing: that the result
    //! REACHED the page, and that a read which did not succeed cannot look
    //! like a page with nothing on it.

    use wafer_run::{ErrorCode, InputStream, WaferError};

    use super::*;
    use crate::{
        blocks::llm::routes::test_support::{admin_msg, routed, RecordingCtx},
        test_support::{output_html, output_http_status},
    };

    /// Answers every non-messages call the way `RecordingCtx` does and
    /// refuses every call to `impresspress/messages`.
    #[derive(Clone)]
    struct MessagesDown;

    #[async_trait::async_trait]
    impl Context for MessagesDown {
        async fn call_block(
            &self,
            block: &str,
            _msg: Message,
            _input: InputStream,
        ) -> OutputStream {
            if block == "impresspress/messages" {
                return OutputStream::error(WaferError::new(ErrorCode::Internal, "messages down"));
            }
            OutputStream::respond(br#"{}"#.to_vec())
        }
        /// Admits nothing, as the fail-closed `check_resource_access` default
        /// this context keeps does.
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_run::ResourceType,
            _access: wafer_block::ResourceAccess,
        ) -> bool {
            false
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }
        fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
            std::sync::Arc::new(self.clone())
        }
    }

    #[tokio::test]
    async fn an_unreachable_messages_block_renders_the_error_page_not_an_empty_chat() {
        let msg = routed(admin_msg("retrieve", "/b/llm/threads/t1"));
        assert_eq!(
            output_http_status(page(&MessagesDown, &msg).await).await,
            500,
            "a dead thread/history read must not render as a chat with no history"
        );
    }

    #[tokio::test]
    async fn an_unreachable_messages_block_fails_the_root_chat_page_too() {
        let msg = routed(admin_msg("retrieve", "/b/llm/"));
        assert_eq!(
            output_http_status(page(&MessagesDown, &msg).await).await,
            500,
            "the sidebar is read at the root page too, and its failure is not an empty sidebar"
        );
    }

    /// The positive half: what the messages block reported reaches the
    /// rendered page, and neither empty state is on it.
    ///
    /// `the_chat_page_reads_threads_and_entries_through_the_messages_block`
    /// asserts the two calls were made and their paths; this asserts the
    /// answers arrived, which is the assertion whose absence let the dead
    /// read merge.
    #[tokio::test]
    async fn the_answers_from_the_messages_block_reach_the_rendered_page() {
        let ctx = RecordingCtx::default()
            .answering(
                "/entries",
                serde_json::json!({
                    "records": [{
                        "id": "e1",
                        "data": {
                            "role": "user",
                            "content": "does my plan renew",
                            "created_at": "2026-09-06T10:00:00Z",
                        }
                    }],
                    "total_count": 1,
                }),
            )
            .answering(
                "/b/messages/api/contexts",
                serde_json::json!({
                    "records": [{
                        "id": "t1",
                        "data": {
                            "title": "Renewal questions",
                            "updated_at": "2026-09-06T10:00:00Z",
                        }
                    }],
                    "total_count": 1,
                }),
            );

        let html =
            output_html(page(&ctx, &routed(admin_msg("retrieve", "/b/llm/threads/t1"))).await)
                .await;

        assert!(
            html.contains("Renewal questions"),
            "the thread the messages block reported must be in the sidebar: {html}"
        );
        assert!(
            !html.contains("No threads yet."),
            "the sidebar must not render its empty state when a thread was returned: {html}"
        );
        assert!(
            html.contains("does my plan renew"),
            "the entry the messages block reported must reach the page: {html}"
        );
    }

    /// The model list is the one read on this page that is genuinely
    /// optional — the picker's "Default (remote)" entry and the browser-side
    /// WebLLM models work without it — so its failure marks the picker
    /// rather than failing the page. What it must not do is what it did:
    /// render exactly like a deployment that has no remote models.
    #[tokio::test]
    async fn an_unavailable_model_list_marks_the_picker_and_still_renders_the_chat() {
        let ctx = RecordingCtx::default().answering(
            "/b/messages/api/contexts",
            serde_json::json!({ "records": [], "total_count": 0 }),
        );

        let html = output_html(page(&ctx, &routed(admin_msg("retrieve", "/b/llm/"))).await).await;

        assert!(
            html.contains(MODELS_UNAVAILABLE),
            "an unreadable model list must say so on the picker: {html}"
        );
        assert!(
            html.contains(r#"id="chat-input""#),
            "the chat itself still renders: {html}"
        );
    }
}

#[cfg(test)]
mod settings_outage_tests {
    //! The settings page's per-thread override list.

    use super::*;
    use crate::test_support::{admin_msg, output_http_status, TestContext};

    /// An empty override list means "no thread pins a provider", which is
    /// what an untouched deployment renders. A failed read must not say it.
    #[tokio::test]
    async fn a_failing_override_read_renders_the_error_page_not_no_overrides() {
        let ctx = TestContext::with_llm().await.break_reads();
        let msg = admin_msg("retrieve", "/b/llm/settings");
        assert_eq!(
            output_http_status(settings_page(&ctx, &msg).await).await,
            500
        );
    }
}
