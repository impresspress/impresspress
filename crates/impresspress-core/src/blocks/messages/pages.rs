//! SSR pages for the messages block.
//!
//! Provides:
//! - Context list page (`GET /b/messages/`)
//! - Context detail page (`GET /b/messages/contexts/{id}`)

use maud::{html, Markup};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, Message, OutputStream};

use super::service::{self, ListContextsParams, ListEntriesParams};
use crate::{
    http::err_internal,
    ui::{self, shell::Crumb},
    util::{path_param, RecordExt},
};

pub fn entry_card(record: &db::Record) -> Markup {
    let kind = record.str_field("kind");
    let role = record.str_field("role");
    let content = record.str_field("content");
    let content_type = record.str_field("content_type");
    let created_at = record.str_field("created_at");
    let date = created_at.get(..10).unwrap_or(created_at);

    let (bg_style, badge_class) = match kind {
        "artifact" => (
            "background:#fdf4ff;border-left:3px solid #a855f7",
            "badge-warning",
        ),
        "notification" => (
            "background:#fefce8;border-left:3px solid #eab308",
            "badge-warning",
        ),
        "status" => (
            "background:#f0f9ff;border-left:3px solid #0ea5e9",
            "badge-info",
        ),
        _ => match role {
            "user" => (
                "background:#eff6ff;border-left:3px solid #3b82f6",
                "badge-info",
            ),
            "agent" | "assistant" => (
                "background:#f8fafc;border-left:3px solid var(--text-muted)",
                "badge",
            ),
            "system" => (
                "background:#fefce8;border-left:3px solid #eab308",
                "badge-warning",
            ),
            _ => (
                "background:#f0fdf4;border-left:3px solid #22c55e",
                "badge-success",
            ),
        },
    };

    html! {
        div .card style={"margin-bottom:0.75rem;" (bg_style)} {
            div style="display:flex;align-items:center;gap:0.5rem;margin-bottom:0.5rem" {
                span .badge .(badge_class) style="text-transform:capitalize" { (kind) }
                @if !role.is_empty() {
                    span .badge style="text-transform:capitalize" { (role) }
                }
                @if kind == "artifact" && !content_type.is_empty() && content_type != "text/plain" {
                    span .text-muted style="font-size:0.7rem" { (content_type) }
                }
                @if !date.is_empty() {
                    span .text-muted style="font-size:0.75rem;margin-left:auto" { (date) }
                }
            }
            p style="margin:0;white-space:pre-wrap;word-break:break-word" { (content) }
        }
    }
}

pub async fn context_list_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let params = ListContextsParams {
        context_type: None,
        status: None,
        sender_id: None,
        parent_id: None,
        page_size: 50,
        offset: 0,
    };

    let contexts = match service::list_contexts(ctx, &params).await {
        Ok(r) => r.records,
        Err(_) => vec![],
    };

    let content = html! {
        section .card .messages-new {
            header .card__head {
                h3 .card__title { "New context" }
            }
            div .card__body {
                form .messages-new__form
                    hx-post="/b/messages/api/contexts"
                    hx-target="#context-list"
                    hx-swap="afterbegin"
                    hx-on--after-request="if(event.detail.successful){this.reset()}"
                {
                    select .form-input .messages-new__type name="type" {
                        option value="conversation" { "Conversation" }
                        option value="task" { "Task" }
                        option value="notification" { "Notification" }
                    }
                    input .form-input .messages-new__title type="text" name="title" placeholder="Title" required;
                    button .btn .btn-primary type="submit" { "Create" }
                }
            }
        }

        div #context-list .messages-list {
            @if contexts.is_empty() {
                div .messages-list__empty {
                    p { "No contexts yet — create one above." }
                }
            } @else {
                @for context in &contexts {
                    @let id = context.id.as_str();
                    @let title = context.str_field("title");
                    @let context_type = context.str_field("type");
                    @let status = context.str_field("status");
                    @let updated_at = context.str_field("updated_at");
                    @let date = updated_at.get(..10).unwrap_or(updated_at);
                    a .messages-list__item href={"/b/messages/contexts/" (id)} {
                        span .badge .badge-info .messages-list__type { (context_type) }
                        span .messages-list__title {
                            @if title.is_empty() { "Untitled" } @else { (title) }
                        }
                        span .messages-list__status .badge { (status) }
                        @if !date.is_empty() {
                            span .messages-list__date .text-muted { (date) }
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
            title: "Messages",
            nav: ui::NavKind::Admin,
            crumbs: vec![Crumb {
                label: "Contexts",
                href: None,
            }],
            subtitle: Some("Conversations, tasks, and notifications"),
            primary_action: None,
        },
        content,
    )
    .await
}

pub async fn context_detail_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let context_id = path_param(msg, "id", "/b/messages/contexts/");

    if context_id.is_empty() {
        return ui::not_found_response(msg);
    }

    let context = match service::get_context(ctx, context_id).await {
        Ok(r) => r,
        Err(e) if e.code == ErrorCode::NotFound => return ui::not_found_response(msg),
        Err(e) => return err_internal("Database error", e),
    };

    let entries_params = ListEntriesParams {
        kind: None,
        role: None,
        page_size: 200,
        offset: 0,
    };

    let entries = match service::list_entries(ctx, context_id, &entries_params).await {
        Ok(r) => r.records,
        Err(_) => vec![],
    };

    // Sibling conversations only loaded when this is a conversation context;
    // the chat_page template needs them for the thread-list pane.
    let siblings = if context.str_field("type") == "conversation" {
        let sibling_params = ListContextsParams {
            context_type: Some("conversation".to_string()),
            status: None,
            sender_id: None,
            parent_id: None,
            page_size: 50,
            offset: 0,
        };
        service::list_contexts(ctx, &sibling_params)
            .await
            .map(|r| r.records)
            .unwrap_or_default()
    } else {
        vec![]
    };

    let context_title = context.str_field("title");
    let display_title = if context_title.is_empty() {
        "Untitled"
    } else {
        context_title
    };

    let body = render_context_detail_body(&context, &entries, &siblings, context_id);

    // Build crumbs locally so the conversation branch can carry a working
    // [Messages] link back to /b/messages/. The default branch keeps a
    // single crumb (matches its inline "← Back" affordance in
    // render_default_view). `shell_page` supports a full `Vec<Crumb>`, so
    // the variable crumb shape rides through without a bespoke wrapper.
    let crumbs = if context.str_field("type") == "conversation" {
        vec![
            Crumb {
                label: "Messages",
                href: Some("/b/messages/"),
            },
            Crumb {
                label: display_title,
                href: None,
            },
        ]
    } else {
        vec![Crumb {
            label: display_title,
            href: None,
        }]
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell {
            title: display_title,
            nav: ui::NavKind::Admin,
            crumbs,
            subtitle: None,
            primary_action: None,
        },
        body,
    )
    .await
}

/// Pure render helper: branches on `context.type`. Conversation contexts
/// use the canonical chat_page template (no right rail). Other types keep
/// the existing single-pane shell.
///
/// Why split: keeps the async data-loading shell separate from the markup,
/// so unit tests can exercise both branches without mocking Context.
fn render_context_detail_body(
    context: &db::Record,
    entries: &[db::Record],
    siblings: &[db::Record],
    context_id: &str,
) -> Markup {
    let context_type = context.str_field("type");

    // Why type=conversation diverges: conversation contexts are chat-shaped,
    // so they reuse the canonical chat_page template — same lens the LLM
    // block's /b/llm/ surface uses. Other types (task, notification) hold
    // artifacts and status updates that don't fit a chat composer.
    if context_type == "conversation" {
        return render_conversation_view(context, entries, siblings, context_id);
    }

    // Existing single-pane render path for non-conversation types.
    render_default_view(context, entries, context_id)
}

/// Conversation-type view: chat_page template with sibling thread list,
/// entries via `entry_card`, simplified composer (kind=message/role=user).
fn render_conversation_view(
    context: &db::Record,
    entries: &[db::Record],
    siblings: &[db::Record],
    context_id: &str,
) -> Markup {
    let post_url = format!("/b/messages/api/contexts/{context_id}/entries");

    // Ensure the active context is always present in the thread list (a
    // freshly created conversation may not yet appear in the sibling query
    // results; without this the active marker would have nothing to attach
    // to).
    let mut combined: Vec<&db::Record> = Vec::with_capacity(siblings.len() + 1);
    if !siblings.iter().any(|s| s.id == context.id) {
        combined.push(context);
    }
    combined.extend(siblings.iter());

    let thread_list = render_conversation_thread_list(&combined, context_id);
    let messages_pane = render_conversation_messages(entries);
    let composer = render_conversation_composer(&post_url);

    crate::ui::templates::chat_page(thread_list, messages_pane, composer, None)
}

/// Default single-pane view for task/notification/etc.
fn render_default_view(context: &db::Record, entries: &[db::Record], context_id: &str) -> Markup {
    let context_title = context.str_field("title");
    let context_type = context.str_field("type");
    let context_status = context.str_field("status");
    let display_title = if context_title.is_empty() {
        "Untitled"
    } else {
        context_title
    };
    let post_url = format!("/b/messages/api/contexts/{context_id}/entries");

    html! {
        div style="display:flex;align-items:center;gap:0.75rem;margin-bottom:1.5rem" {
            a .btn .btn-ghost .btn-sm href="/b/messages/" { "\u{2190} Back" }
            h2 .page-title style="margin:0" { (display_title) }
            span .badge style="text-transform:capitalize" { (context_type) }
            span .badge { (context_status) }
        }

        div #entries-list style="margin-bottom:1.5rem;max-height:60vh;overflow-y:auto" {
            @if entries.is_empty() {
                div .text-center .text-muted style="padding:2rem" {
                    "No entries yet. Add one below."
                }
            } @else {
                @for e in entries {
                    (entry_card(e))
                }
            }
        }

        div .card {
            h3 style="font-size:0.9rem;font-weight:600;margin:0 0 0.75rem;color:var(--text-muted)" {
                "Add Entry"
            }
            form
                hx-post=(post_url)
                hx-target="#entries-list"
                hx-swap="beforeend"
                hx-on--after-request="if(event.detail.successful){this.reset();var list=document.getElementById('entries-list');list.scrollTop=list.scrollHeight;}"
            {
                div style="display:flex;gap:0.5rem;margin-bottom:0.5rem" {
                    select .form-input name="kind" style="width:auto" {
                        option value="message" { "message" }
                        option value="artifact" { "artifact" }
                        option value="notification" { "notification" }
                        option value="status" { "status" }
                    }
                    select .form-input name="role" style="width:auto" {
                        option value="user" { "user" }
                        option value="agent" { "agent" }
                        option value="system" { "system" }
                    }
                }
                div style="display:flex;gap:0.5rem;align-items:flex-end" {
                    textarea .form-input
                        name="content"
                        placeholder="Entry content"
                        rows="3"
                        required
                        style="flex:1;resize:vertical"
                    {}
                    button .btn .btn-primary type="submit" { "Add" }
                }
            }
        }
    }
}

fn render_conversation_thread_list(siblings: &[&db::Record], active_id: &str) -> Markup {
    html! {
        div style="display:flex;flex-direction:column;gap:0.75rem;height:100%" {
            div style="display:flex;align-items:center;justify-content:space-between" {
                h3 style="font-size:0.875rem;font-weight:600;color:var(--text-muted);margin:0;text-transform:uppercase;letter-spacing:0.05em" {
                    "Conversations"
                }
            }
            div #conversation-list style="overflow-y:auto;flex:1" {
                @if siblings.is_empty() {
                    div .text-center .text-muted style="padding:1rem;font-size:0.875rem" {
                        "No conversations yet."
                    }
                } @else {
                    @for c in siblings {
                        @let id = c.id.as_str();
                        @let title = c.str_field("title");
                        @let updated_at = c.str_field("updated_at");
                        @let date = updated_at.get(..10).unwrap_or(updated_at);
                        @let is_active = id == active_id;
                        a
                            .card
                            href={"/b/messages/contexts/" (id)}
                            data-context-id=(id)
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
    }
}

fn render_conversation_messages(entries: &[db::Record]) -> Markup {
    // The `chat_page` template's `.chat-messages` wrapper already owns
    // scroll, padding, and background for this pane (see layout.css
    // `.page--chat .chat-messages`). We just need the #entries-list ID
    // for the htmx composer's hx-target — no extra scroll container or
    // we double-scroll and end up with a boxed-inside-boxed look.
    html! {
        div #entries-list {
            @if entries.is_empty() {
                div .text-center .text-muted style="padding:2rem" {
                    "No messages yet. Send the first one below."
                }
            } @else {
                @for e in entries {
                    (entry_card(e))
                }
            }
        }
    }
}

fn render_conversation_composer(post_url: &str) -> Markup {
    html! {
        form
            hx-post=(post_url)
            hx-target="#entries-list"
            hx-swap="beforeend"
            // Scroll the parent `.chat-messages` (the chat_page template's
            // pane wrapper) — `#entries-list` itself is no longer a scroll
            // container in the conversation view (see render_conversation_messages).
            hx-on--after-request="if(event.detail.successful){this.reset();var list=document.getElementById('entries-list').parentElement;list.scrollTop=list.scrollHeight;}"
        {
            // Hidden defaults: kind=message, role=user. Conversation lens is
            // an opinionated view — composers below the fold (settings page,
            // direct API) can still post other kinds/roles.
            input type="hidden" name="kind" value="message";
            input type="hidden" name="role" value="user";
            div style="display:flex;gap:0.5rem;align-items:flex-end" {
                textarea
                    .form-input
                    name="content"
                    placeholder="Type your message..."
                    rows="3"
                    required
                    style="flex:1;resize:none"
                    onkeydown="if(event.key==='Enter'&&!event.shiftKey){event.preventDefault();this.closest('form').requestSubmit();}"
                {}
                button .btn .btn-primary type="submit" style="height:fit-content" { "Send" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(id: &str) -> db::Record {
        db::Record {
            id: id.to_string(),
            data: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn context_detail_body_uses_chat_page_for_conversation_type() {
        let mut ctx_rec = make_record("ctx-1");
        ctx_rec
            .data
            .insert("type".to_string(), serde_json::json!("conversation"));
        ctx_rec
            .data
            .insert("title".to_string(), serde_json::json!("Hello"));
        ctx_rec
            .data
            .insert("status".to_string(), serde_json::json!("active"));

        let html = render_context_detail_body(&ctx_rec, &[], &[], "ctx-1").into_string();
        assert!(
            html.contains(r#"class="page--chat""#),
            "conversation type should use chat_page template; got: {html}"
        );
        // Messages does NOT enable the right rail.
        assert!(
            !html.contains(r#"class="chat-rail""#),
            "Messages chat view should NOT enable the right rail; got: {html}"
        );
    }

    #[test]
    fn context_detail_body_uses_default_shell_for_task_type() {
        let mut ctx_rec = make_record("ctx-1");
        ctx_rec
            .data
            .insert("type".to_string(), serde_json::json!("task"));
        ctx_rec
            .data
            .insert("title".to_string(), serde_json::json!("Do thing"));
        ctx_rec
            .data
            .insert("status".to_string(), serde_json::json!("open"));

        let html = render_context_detail_body(&ctx_rec, &[], &[], "ctx-1").into_string();
        assert!(
            !html.contains(r#"class="page--chat""#),
            "task type should keep the existing single-pane shell"
        );
        assert!(
            html.contains(r#"id="entries-list""#),
            "single-pane shell still has the entries-list container"
        );
    }

    #[test]
    fn context_detail_body_conversation_renders_sibling_thread_list() {
        let mut active = make_record("ctx-1");
        active
            .data
            .insert("type".to_string(), serde_json::json!("conversation"));
        active
            .data
            .insert("title".to_string(), serde_json::json!("Active"));

        let mut sibling = make_record("ctx-2");
        sibling
            .data
            .insert("type".to_string(), serde_json::json!("conversation"));
        sibling
            .data
            .insert("title".to_string(), serde_json::json!("Sibling"));
        sibling.data.insert(
            "updated_at".to_string(),
            serde_json::json!("2026-05-04T10:00:00Z"),
        );

        let html =
            render_context_detail_body(&active, &[], std::slice::from_ref(&sibling), "ctx-1")
                .into_string();
        assert!(
            html.contains(r#"href="/b/messages/contexts/ctx-2""#),
            "sibling link missing: {html}"
        );
        assert!(html.contains("Sibling"));
        assert!(html.contains("Conversations"), "thread-list header missing");
        // Active marker on the active context, not the sibling.
        assert!(html.contains(r#"data-context-id="ctx-1""#));
        assert!(html.contains(r#"data-active="true""#));
    }

    #[test]
    fn context_detail_body_conversation_messages_pane_has_no_inner_scroll() {
        let mut ctx_rec = make_record("ctx-1");
        ctx_rec
            .data
            .insert("type".to_string(), serde_json::json!("conversation"));
        ctx_rec
            .data
            .insert("title".to_string(), serde_json::json!("Hello"));

        let html = render_context_detail_body(&ctx_rec, &[], &[], "ctx-1").into_string();

        // #entries-list still exists for htmx hx-target.
        assert!(html.contains(r#"id="entries-list""#));
        // But it must not carry the redundant overflow/height styling — the
        // .chat-messages wrapper from the chat_page template owns scroll.
        // Heuristic: the entries-list opening tag should NOT include
        // height:100% or overflow-y:auto in its style attribute.
        let entries_pos = html
            .find(r#"id="entries-list""#)
            .expect("entries-list must exist");
        // Walk back from entries-list to the opening `<div`.
        let div_start = html[..entries_pos].rfind("<div").expect("opening div");
        let tag_end = html[div_start..].find('>').expect("tag close") + div_start;
        let opening_tag = &html[div_start..=tag_end];
        assert!(
            !opening_tag.contains("height:100%"),
            "conversation entries-list opening tag still has height:100% — double scroll: {opening_tag}"
        );
        assert!(
            !opening_tag.contains("overflow-y:auto"),
            "conversation entries-list opening tag still has overflow-y:auto — double scroll: {opening_tag}"
        );
    }
}
