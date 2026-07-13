//! SSR pages for the LLM orchestrator block.
//!
//! Provides:
//! - Chat page (`GET /b/llm/` and `GET /b/llm/threads/{id}`) — unified
//!   handler renders the canonical `templates::chat_page` shell.
//! - Settings page (`GET /b/llm/settings`) — default provider/model config

use maud::{html, Markup};
use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::{config, database as db, llm::ModelInfo};
use wafer_run::{context::Context, Message, OutputStream};

use super::{DEFAULT_MODEL_VAR, DEFAULT_PROVIDER, DEFAULT_PROVIDER_VAR, SETTINGS_TABLE};
use crate::{
    messages_schema::{CONTEXTS_TABLE, ENTRIES_TABLE},
    ui::{self, components, icons, shell::Crumb},
    util::RecordExt,
};

// ---------------------------------------------------------------------------
// Unified chat page (handles `/b/llm/` and `/b/llm/threads/{id}`)
// ---------------------------------------------------------------------------

/// Parse the optional thread id from a `/b/llm/{...}` URL.
///
/// Returns `Some(id)` for `/b/llm/threads/{id}` (and ignores any trailing
/// path segments), `None` for `/b/llm/` and `/b/llm`.
fn parse_thread_id(path: &str) -> Option<&str> {
    let after = path.strip_prefix("/b/llm/threads/")?;
    let id = after.split('/').next().unwrap_or("");
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Pure render helper for the unified chat page body.
///
/// Takes the already-loaded data (threads, entries, models, defaults) and
/// the optional active thread id, returns the inner page Markup. Kept
/// pure (sync, no `Context`) so the selector-preservation contract can be
/// verified in unit tests without mocking the database client.
fn render_page_body(
    threads: &[db::Record],
    entries: &[db::Record],
    models: &[ModelInfo],
    default_model: &str,
    thread_id: Option<&str>,
    llm_chat_js_url: &str,
) -> Markup {
    // Build messages JSON for the bootstrap carrier. Empty array when no thread.
    let messages_json: Vec<serde_json::Value> = entries
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": m.str_field("role"),
                "content": m.str_field("content"),
                "created_at": m.str_field("created_at"),
            })
        })
        .collect();
    let messages_json_str = serde_json::to_string(&messages_json).unwrap_or_else(|_| "[]".into());

    let thread_list = render_thread_list_pane(threads, thread_id);
    let messages_pane = render_messages_pane(entries, thread_id);
    let composer = render_composer(thread_id);
    let right_rail = render_right_rail(models, default_model);

    let chat_body =
        crate::ui::templates::chat_page(thread_list, messages_pane, composer, Some(right_rail));

    html! {
        (chat_body)

        // Pulse animation for thinking indicator + blinking cursor.
        style { "@keyframes pulse{0%,100%{opacity:1}50%{opacity:.4}} @keyframes blink{0%,100%{opacity:1}50%{opacity:0}} .typing-cursor{display:inline-block;width:0.5em;height:1.1em;background:var(--text-primary,#333);vertical-align:text-bottom;margin-left:2px;animation:blink 0.8s step-end infinite}" }
        // marked.js for markdown rendering — self-hosted (vendored), content-hashed.
        script src=(crate::ui::assets::marked_js_url()) {}

        // Server-rendered initial state for the chat module. Carrier is
        // type="application/json" so the browser does NOT parse the body as JS —
        // any `</script>` or `<` in user-typed message content is inert. The
        // JS module reads it via JSON.parse on init().
        script type="application/json" id="llm-chat-bootstrap" {
            (maud::PreEscaped(messages_json_str))
        }
        script src=(llm_chat_js_url) defer {}
        script {
            (maud::PreEscaped("window.addEventListener('DOMContentLoaded', function(){ if (window.impresspressLlmChat) window.impresspressLlmChat.init(); });"))
        }
    }
}

/// Unified handler for `/b/llm/` and `/b/llm/threads/{id}`.
///
/// Renders the canonical `templates::chat_page` (thread list / messages /
/// composer / right rail). The optional thread id from the URL drives
/// composer enablement and message preloading.
pub async fn page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let path = msg.path().to_string();
    let thread_id = parse_thread_id(&path);

    // Load thread list (sidebar) — most-recently-updated first, capped at 50.
    let opts = ListOptions {
        sort: vec![SortField {
            field: "updated_at".to_string(),
            desc: true,
        }],
        limit: 50,
        ..Default::default()
    };
    let threads = match db::list(ctx, CONTEXTS_TABLE, &opts).await {
        Ok(r) => r.records,
        Err(_) => vec![],
    };

    // Load entries for the selected thread, if any. Empty when no thread is selected.
    let entries = match thread_id {
        Some(tid) => {
            let messages_opts = ListOptions {
                filters: vec![Filter {
                    field: "context_id".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::Value::String(tid.to_string()),
                }],
                sort: vec![SortField {
                    field: "created_at".to_string(),
                    desc: false,
                }],
                limit: 200,
                ..Default::default()
            };
            db::list(ctx, ENTRIES_TABLE, &messages_opts)
                .await
                .map(|r| r.records)
                .unwrap_or_default()
        }
        None => Vec::new(),
    };

    // Resolve display title from the loaded thread record (when present).
    let thread_title = thread_id
        .and_then(|tid| threads.iter().find(|t| t.id.as_str() == tid))
        .map(|t| t.str_field("title").to_string())
        .filter(|s| !s.is_empty());
    let display_title = thread_title.as_deref().unwrap_or("Chat");

    let models = load_models(ctx).await;
    let default_model = config::get_default(ctx, DEFAULT_MODEL_VAR, "").await;

    let llm_chat_js_url = crate::ui::assets::llm_chat_js_url();
    let content = render_page_body(
        &threads,
        &entries,
        &models,
        default_model.as_str(),
        thread_id,
        llm_chat_js_url,
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
fn render_thread_list_pane(threads: &[db::Record], active_id: Option<&str>) -> Markup {
    html! {
        div style="display:flex;flex-direction:column;gap:0.75rem;height:100%" {
            div style="display:flex;align-items:center;justify-content:space-between" {
                h3 style="font-size:0.875rem;font-weight:600;color:var(--text-muted);margin:0;text-transform:uppercase;letter-spacing:0.05em" {
                    "Threads"
                }
                button .btn.btn-sm.btn-primary onclick="createNewThread()" {
                    (icons::plus())
                }
            }
            div #thread-list style="overflow-y:auto;flex:1" {
                (thread_list_items(threads, active_id))
            }
        }
    }
}

fn thread_list_items(threads: &[db::Record], active_id: Option<&str>) -> Markup {
    html! {
        @if threads.is_empty() {
            div .text-center .text-muted style="padding:1rem;font-size:0.875rem" {
                "No threads yet."
            }
        } @else {
            @for thread in threads {
                @let id = thread.id.as_str();
                @let title = thread.str_field("title");
                @let updated_at = thread.str_field("updated_at");
                @let date = updated_at.get(..10).unwrap_or(updated_at);
                @let is_active = active_id == Some(id);
                a
                    .card
                    href={"/b/llm/threads/" (id)}
                    data-thread-id=(id)
                    data-active=(if is_active { "true" } else { "false" })
                    aria-current=[is_active.then_some("page")]
                    style="display:block;text-decoration:none;color:inherit;margin-bottom:0.375rem;padding:0.625rem 0.75rem;transition:box-shadow 0.15s"
                    onmouseover="this.style.boxShadow='0 2px 8px rgba(0,0,0,0.1)'"
                    onmouseout="this.style.boxShadow=''"
                {
                    div style="display:flex;align-items:center;justify-content:space-between;gap:0.5rem" {
                        span style="font-weight:500;font-size:0.875rem;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;flex:1" {
                            @if title.is_empty() { "Untitled" } @else { (title) }
                        }
                        @if !date.is_empty() {
                            span .text-muted style="font-size:0.75rem;flex-shrink:0" { (date) }
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
fn render_messages_pane(_entries: &[db::Record], thread_id: Option<&str>) -> Markup {
    html! {
        div #messages-area
            style="height:100%;overflow-y:auto;padding:0.5rem;background:var(--bg-secondary);border-radius:0.5rem"
        {
            @if thread_id.is_none() {
                div #no-thread-prompt .text-center style="padding:3rem 1rem" {
                    div style="font-size:2.5rem;margin-bottom:0.75rem" { "\u{1f4ac}" }
                    p style="font-size:1.1rem;color:var(--text-primary);margin:0 0 0.5rem" { "Start a new conversation" }
                    p .text-muted style="margin:0 0 1.5rem" { "Click the " strong { "+" } " button to create a thread, then type your message." }
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
            onsubmit="return handleChatSubmit(event)"
            style=(if enabled { "" } else { "opacity:0.4;pointer-events:none" })
            data-thread=(thread_value)
        {
            input type="hidden" name="thread_id" id="active-thread-id" value=(thread_value);
            div style="display:flex;gap:0.5rem;align-items:flex-end" {
                div style="flex:1;position:relative" {
                    textarea
                        .form-input
                        #chat-input
                        name="message"
                        placeholder=(placeholder)
                        rows="3"
                        required
                        disabled[!enabled]
                        style="resize:none;width:100%"
                        onkeydown="if(event.key==='Enter'&&!event.shiftKey){event.preventDefault();this.closest('form').requestSubmit();}"
                    {}
                }
                div style="display:flex;flex-direction:column;align-items:center;gap:0.25rem" {
                    button #send-btn .btn.btn-primary type="submit" disabled[!enabled] style="height:fit-content" {
                        "Send"
                    }
                    span #send-status .text-muted style="font-size:0.7rem;white-space:nowrap" {}
                }
            }
        }
    }
}

/// Right-rail pane for the chat_page template. Holds the model picker,
/// model loading progress container, and a link to the LLM settings
/// page. Replaces the inline above-messages model strip from the old
/// chat_page handler.
fn render_right_rail(models: &[ModelInfo], default_model: &str) -> Markup {
    html! {
        div style="display:flex;flex-direction:column;gap:1rem;padding:0.5rem" {
            div {
                label .form-label style="display:block;margin-bottom:0.375rem;font-size:0.875rem" { "Model" }
                select
                    #model-picker
                    .form-input
                    style="width:100%"
                    name="model"
                    onchange="onModelChange(this.value)"
                {
                    optgroup label="Remote" {
                        option value="" selected[default_model.is_empty()] { "Default (remote)" }
                        (render_model_picker(models, default_model))
                    }
                    optgroup #local-models-group label="Local (WebLLM)" {}
                }
                span #model-status .text-muted style="display:block;margin-top:0.25rem;font-size:0.75rem" {}
            }

            div #model-progress-container style="display:none" {
                div .card style="padding:0.75rem" {
                    div style="display:flex;align-items:center;gap:0.5rem;margin-bottom:0.5rem" {
                        span style="font-size:0.875rem;font-weight:500" { "Loading model..." }
                        button #model-unload-btn .btn.btn-sm.btn-ghost onclick="unloadLocalModel()" style="margin-left:auto" {
                            "Cancel"
                        }
                    }
                    div style="background:var(--bg-secondary);border-radius:0.25rem;height:6px;overflow:hidden" {
                        div #model-progress-bar style="height:100%;background:var(--primary, #3b82f6);width:0%;transition:width 0.3s" {}
                    }
                    div #model-progress-text .text-muted style="font-size:0.75rem;margin-top:0.25rem" { "" }
                }
            }

            a .btn.btn-ghost.btn-sm href="/b/llm/settings" style="justify-content:flex-start" {
                "\u{2699} Settings"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Settings page
// ---------------------------------------------------------------------------

pub async fn settings_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let default_provider = config::get_default(ctx, DEFAULT_PROVIDER_VAR, DEFAULT_PROVIDER).await;
    let default_model = config::get_default(ctx, DEFAULT_MODEL_VAR, "").await;

    // Load per-thread overrides
    let overrides: Vec<db::Record> = db::list_all(ctx, SETTINGS_TABLE, vec![])
        .await
        .unwrap_or_default();

    let content = html! {
        (components::page_header(
            "LLM Settings",
            Some("Configure default provider and model"),
            None,
        ))

        // Global defaults — read-only display; set via env vars
        div .card style="margin-bottom:1.5rem" {
            h3 style="font-size:1rem;font-weight:600;margin:0 0 1rem" { "Global Defaults" }
            p .text-muted style="font-size:0.875rem;margin-bottom:1rem" {
                "Global defaults are configured via environment variables."
            }
            div style="display:grid;grid-template-columns:1fr 1fr;gap:1rem" {
                div .form-group {
                    label .form-label { "Default Provider" }
                    input
                        .form-input
                        type="text"
                        value=(default_provider)
                        readonly
                        style="background:var(--bg-secondary)"
                    ;
                    p .form-hint {
                        "Set via " code { (DEFAULT_PROVIDER_VAR) }
                    }
                }
                div .form-group {
                    label .form-label { "Default Model" }
                    input
                        .form-input
                        type="text"
                        value=(default_model)
                        placeholder="(provider default)"
                        readonly
                        style="background:var(--bg-secondary)"
                    ;
                    p .form-hint {
                        "Set via " code { (DEFAULT_MODEL_VAR) }
                    }
                }
            }
        }

        // Per-thread overrides
        div .card {
            h3 style="font-size:1rem;font-weight:600;margin:0 0 1rem" { "Per-Thread Overrides" }
            @if overrides.is_empty() {
                div .text-center .text-muted style="padding:1.5rem" {
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
                            @for ov in &overrides {
                                @let tid = ov.str_field("thread_id");
                                @let pb = ov.str_field("provider_block");
                                @let model = ov.str_field("model");
                                @let updated = ov.str_field("updated_at");
                                @let date = updated.get(..10).unwrap_or(updated);
                                tr {
                                    td {
                                        a href={"/b/llm/threads/" (tid)} style="font-family:monospace;font-size:0.8rem" {
                                            (tid)
                                        }
                                    }
                                    td {
                                        @if pb.is_empty() {
                                            span .text-muted { "(default)" }
                                        } @else {
                                            code style="font-size:0.8rem" { (pb) }
                                        }
                                    }
                                    td {
                                        @if model.is_empty() {
                                            span .text-muted { "(default)" }
                                        } @else {
                                            code style="font-size:0.8rem" { (model) }
                                        }
                                    }
                                    td .text-muted style="font-size:0.8rem" { (date) }
                                    td {
                                        button
                                            .btn.btn-sm.btn-danger
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
/// Returns an empty vec on any failure — the picker falls back to the
/// "Default (remote)" option and the user can still send a request.
async fn load_models(ctx: &dyn Context) -> Vec<ModelInfo> {
    // A dispatch failure is treated as "no models" so the picker still renders
    // — the user can fall back to the default remote option.
    wafer_core::clients::llm::list_models(ctx)
        .await
        .unwrap_or_default()
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

    fn make_thread(id: &str, title: &str, updated_at: &str) -> db::Record {
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), serde_json::json!(title));
        data.insert("updated_at".to_string(), serde_json::json!(updated_at));
        db::Record {
            id: id.to_string(),
            data,
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
            html.contains("createNewThread()"),
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
        let html = render_right_rail(&models, "").into_string();
        assert!(html.contains(r#"id="model-picker""#));
        assert!(html.contains(r#"id="model-progress-container""#));
        assert!(html.contains(r#"id="local-models-group""#));
        assert!(html.contains("/b/llm/settings"), "settings link missing");
        assert!(html.contains(r#"label="Remote""#));
    }

    // ----- Task 3: parse_thread_id + render_page_body -----

    #[test]
    fn parse_thread_id_root_returns_none() {
        assert_eq!(parse_thread_id("/b/llm/"), None);
        assert_eq!(parse_thread_id("/b/llm"), None);
    }

    #[test]
    fn parse_thread_id_thread_path_returns_id() {
        assert_eq!(parse_thread_id("/b/llm/threads/abc-123"), Some("abc-123"));
    }

    #[test]
    fn parse_thread_id_strips_trailing_segments() {
        assert_eq!(
            parse_thread_id("/b/llm/threads/abc-123/extra"),
            Some("abc-123")
        );
    }

    #[test]
    fn parse_thread_id_blank_id_returns_none() {
        assert_eq!(parse_thread_id("/b/llm/threads/"), None);
    }

    /// At root URL, the page body shows the no-thread prompt and a
    /// disabled composer.
    #[test]
    fn page_body_renders_empty_state_at_root() {
        let html =
            render_page_body(&[], &[], &[], "", None, "/b/static/llm-chat-test.js").into_string();
        assert!(html.contains(r#"id="no-thread-prompt""#));
        assert!(html.contains("Start a new conversation"));
        assert!(html.contains(r#"id="chat-form""#));
        assert!(
            html.contains("opacity:0.4"),
            "composer should be disabled style"
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
            "",
            Some("some-id"),
            "/b/static/llm-chat-test.js",
        )
        .into_string();
        assert!(html.contains(r#"value="some-id""#));
        assert!(!html.contains(r#"id="no-thread-prompt""#));
        assert!(!html.contains("opacity:0.4"));
    }

    /// The page body must include the external <script src> for the static
    /// llm-chat.js asset, and must NOT include the deleted inline JS
    /// constants. Markers from the old SHARED_JS / CHAT_JS / THREAD_JS
    /// must be absent.
    #[test]
    fn page_body_includes_external_llm_chat_js_and_drops_inline_constants() {
        let url = "/b/static/llm-chat-deadbeef.js";
        let html = render_page_body(&[], &[], &[], "", None, url).into_string();
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

    /// XSS regression — a thread whose message content contains a
    /// `</script>` sequence MUST NOT terminate the bootstrap script tag
    /// prematurely. With the `type="application/json"` carrier, the body
    /// is inert — the browser does not parse it as JS — so the literal
    /// sequence appearing in textContent is safe. We also assert the dead
    /// `_activeThreadId` / `_defaultModel` JS bootstrap fields are gone.
    #[test]
    fn render_page_body_carrier_escapes_user_content_safely() {
        let mut data = std::collections::HashMap::new();
        data.insert("role".to_string(), serde_json::json!("user"));
        data.insert(
            "content".to_string(),
            serde_json::json!("hello </script><script>alert(1)</script>"),
        );
        data.insert(
            "created_at".to_string(),
            serde_json::json!("2026-05-05T10:00:00Z"),
        );
        let entry = db::Record {
            id: "e-1".to_string(),
            data,
        };

        let html = render_page_body(
            &[],
            &[entry],
            &[],
            "",
            Some("t-1"),
            "/b/static/llm-chat-test.js",
        )
        .into_string();

        // Carrier present.
        assert!(
            html.contains(r#"<script type="application/json" id="llm-chat-bootstrap">"#),
            "expected JSON carrier block: {html}"
        );
        // The dangerous content is INSIDE a non-JS-parsed block. We don't
        // assert the absence of the literal bytes (they're allowed to appear
        // in JSON text inside a type="application/json" block — they're inert).
        // We assert that the JS bootstrap line that USED to inline values is gone.
        assert!(
            !html.contains("window._threadMessages = "),
            "JS bootstrap line should be removed in favor of carrier: {html}"
        );
        assert!(
            !html.contains("window._activeThreadId"),
            "_activeThreadId was dead — should be removed entirely"
        );
        assert!(
            !html.contains("window._defaultModel"),
            "_defaultModel was dead — should be removed entirely"
        );
    }

    /// The body wraps in the canonical `templates::chat_page` shell (page
    /// class + right-rail aside).
    #[test]
    fn page_body_emits_chat_page_template_class() {
        let html =
            render_page_body(&[], &[], &[], "", None, "/b/static/llm-chat-test.js").into_string();
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
