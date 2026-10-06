//! SSR pages for the messages block.
//!
//! Provides:
//! - Context list page (`GET /b/messages/`)
//! - Context detail page (`GET /b/messages/contexts/{id}`)

use maud::{html, Markup};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, Message, OutputStream, WaferError};

use super::{
    contracts::{EntryKind, EntryRole},
    service::{self, ListContextsParams, ListEntriesParams},
};
use crate::{
    ui::{
        self,
        components::{self, Badge, BadgeVariant},
        icons,
        shell::Crumb,
    },
    util::{enum_column_or, wire_str, RecordExt},
};

/// Render one context as the list page's row.
///
/// Shared with `rest::create_context`, which answers the new-context form's
/// htmx post with this same row so the swapped-in markup is the markup a
/// reload would render. A second, fragment-only copy of it would be free to
/// drift from the list it is prepended to.
pub fn context_card(record: &db::Record) -> Markup {
    let id = record.id.as_str();
    let title = record.str_field("title");
    let context_type = record.str_field("type");
    let status = record.str_field("status");
    let updated_at = record.str_field("updated_at");

    html! {
        a .messages-list__item href={"/b/messages/contexts/" (id)} {
            span .messages-list__type {
                (Badge::new(BadgeVariant::Secondary).classes("text-capitalize").render(html! { (context_type) }))
            }
            span .messages-list__title {
                @if title.is_empty() { "Untitled" } @else { (title) }
            }
            span .messages-list__status { (components::status_badge(status)) }
            @if !updated_at.is_empty() {
                span .messages-list__date .text-muted { (components::timestamp(updated_at)) }
            }
        }
    }
}

/// Render one entry.
///
/// `kind` and `role` are decoded through the crate's one enum door, so a
/// column holding a value the contract does not define stops the page rather
/// than being drawn as whatever the fall-through arm happened to be. Both
/// empty cases are the ones the table's own DDL produces: `kind` defaults to
/// `message`, `role` defaults to `''`, and a role-less entry has never
/// carried a role badge.
pub fn entry_card(record: &db::Record) -> Result<Markup, WaferError> {
    let kind: EntryKind = enum_column_or(record, "kind", EntryKind::Message)?;
    let role: Option<EntryRole> = enum_column_or(record, "role", None)?;
    let content = record.str_field("content");
    let content_type = record.str_field("content_type");
    let created_at = record.str_field("created_at");

    // Card accents: the person's own entries get a quiet slate tint, machine
    // entries stay neutral, and only genuinely semantic kinds keep a semantic
    // hue (notification/system = warning yellow). Keep in sync with
    // `messageCardHtml` in blocks/llm/assets/llm-chat.js — same cards,
    // JS-rendered.
    let (card_variant, kind_badge) = match kind {
        EntryKind::Artifact | EntryKind::Status => {
            ("message-card--neutral", BadgeVariant::Secondary)
        }
        EntryKind::Notification => ("message-card--warning", BadgeVariant::Warning),
        EntryKind::Message => match role {
            Some(EntryRole::User) => ("message-card--user", BadgeVariant::Secondary),
            Some(EntryRole::Assistant) | None => ("message-card--neutral", BadgeVariant::Secondary),
            Some(EntryRole::System) => ("message-card--warning", BadgeVariant::Warning),
        },
    };

    Ok(html! {
        div .card .(card_variant) {
            div .message-card__head {
                (Badge::new(kind_badge).classes("text-capitalize").render(html! { (wire_str(&kind)) }))
                @if let Some(role) = role {
                    (Badge::new(BadgeVariant::Secondary).classes("text-capitalize").render(html! { (wire_str(&role)) }))
                }
                @if kind == EntryKind::Artifact
                    && !content_type.is_empty()
                    && content_type != "text/plain"
                {
                    span .text-muted .text-xs { (content_type) }
                }
                @if !created_at.is_empty() {
                    span .message-card__date { (components::timestamp(created_at)) }
                }
            }
            p .message-card__content { (content) }
        }
    })
}

pub async fn context_list_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let params = ListContextsParams {
        owner_id: None, // admin SSR — explicit bypass, admins see all
        context_type: None,
        status: None,
        sender_id: None,
        parent_id: None,
        page_size: 50,
        offset: 0,
    };

    // "No contexts yet — create one above." is the empty state a fresh
    // deployment renders; a failed read must never reach it.
    let contexts = match service::list_contexts(ctx, &params).await {
        Ok(r) => r.records,
        Err(e) => {
            return ui::shell_error_page(
                ctx,
                msg,
                list_shell(),
                None,
                ui::BackLink::ADMIN_DASHBOARD,
                e,
                "messages context list page: read failed",
            )
            .await
        }
    };

    let content = html! {
        section .card .messages-new {
            header .card__head {
                h2 .card__title { "New context" }
            }
            div .card__body {
                form .messages-new__form
                    hx-post="/b/messages/api/contexts"
                    hx-target="#context-list"
                    hx-swap="afterbegin"
                    // The swap has just put a row in the list, so the empty
                    // state is now false. It is removed here rather than
                    // re-rendered server-side: the response is one row, and
                    // re-sending the whole list to delete one sentence would
                    // cost every reader the scroll position. The effects are
                    // applied by `ui/assets/chrome.js` (section 5).
                    data-reset-on-success
                    data-remove-on-success="context-list-empty"
                {
                    div .form-group {
                        label .form-label for="new-context-type" { "Type" }
                        select .form-input .messages-new__type #new-context-type name="type" {
                            option value="conversation" { "Conversation" }
                            option value="task" { "Task" }
                            option value="notification" { "Notification" }
                        }
                    }
                    div .form-group {
                        label .form-label for="new-context-title" { "Title" }
                        input .form-input .messages-new__title #new-context-title type="text" name="title" placeholder="e.g. Deploy planning" required;
                    }
                    button .btn .btn--primary type="submit" { "Create" }
                }
            }
        }

        div #context-list .messages-list {
            @if contexts.is_empty() {
                div #context-list-empty {
                    (components::empty_state(
                        icons::message_square(),
                        "No messages yet",
                        "Start a conversation, a task or a notification with the form above.",
                        None,
                    ))
                }
            } @else {
                @for context in &contexts {
                    (context_card(context))
                }
            }
        }
    };

    ui::shell_page(ctx, msg, list_shell(), content).await
}

/// The context list's chrome — its page and its error page share it. Named
/// "Messages", as the sidebar names it.
fn list_shell() -> ui::Shell<'static> {
    ui::Shell::admin("Messages", "Messages").subtitle("Conversations, tasks, and notifications")
}

/// A context page's trail: "Messages ›" above the context's title, whatever
/// its type.
fn detail_trail(title: &str) -> Vec<Crumb<'_>> {
    vec![
        Crumb {
            label: "Messages",
            href: Some("/b/messages/"),
        },
        Crumb {
            label: title,
            href: None,
        },
    ]
}

/// A context page whose read failed: drawn in the shell under the
/// "Messages" trail, with a link back to the list.
async fn detail_error_page(
    ctx: &dyn Context,
    msg: &Message,
    error: WaferError,
    context: &str,
) -> OutputStream {
    ui::shell_error_page(
        ctx,
        msg,
        ui::Shell::admin("Messages", "Messages").trail(detail_trail("Context")),
        None,
        ui::BackLink {
            label: "Back to Messages",
            href: "/b/messages/",
        },
        error,
        context,
    )
    .await
}

pub async fn context_detail_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let context_id = msg.var("id");

    if context_id.is_empty() {
        return ui::not_found_response(msg);
    }

    let context = match service::get_context(ctx, context_id).await {
        Ok(r) => r,
        Err(e) if e.code == ErrorCode::NotFound => return ui::not_found_response(msg),
        // The 404 above is the SSR not-found *page*, so only the tail goes
        // through the one mapping — which is what makes a WRAP denial a 403
        // here instead of the 500 it used to be.
        Err(e) => {
            return detail_error_page(ctx, msg, e, "messages detail page: context read failed")
                .await
        }
    };

    let entries_params = ListEntriesParams {
        kind: None,
        role: None,
        page_size: 200,
        offset: 0,
    };

    // A conversation rendered with no messages in it is what an empty
    // conversation looks like, so the read failing has to fail the page.
    let entries = match service::list_entries(ctx, context_id, &entries_params).await {
        Ok(r) => r.records,
        Err(e) => {
            return detail_error_page(ctx, msg, e, "messages detail page: entry list failed").await
        }
    };

    // Sibling conversations only loaded when this is a conversation context;
    // the chat_page template needs them for the thread-list pane.
    let siblings = if context.str_field("type") == "conversation" {
        let sibling_params = ListContextsParams {
            owner_id: None, // admin SSR — explicit bypass, admins see all
            context_type: Some("conversation".to_string()),
            status: None,
            sender_id: None,
            parent_id: None,
            page_size: 50,
            offset: 0,
        };
        // An empty sibling list renders as "this is the only conversation",
        // which is a claim about the caller's data — not something a failed
        // read is entitled to make.
        match service::list_contexts(ctx, &sibling_params).await {
            Ok(r) => r.records,
            Err(e) => {
                return detail_error_page(ctx, msg, e, "messages detail page: sibling list failed")
                    .await
            }
        }
    } else {
        vec![]
    };

    let context_title = context.str_field("title");
    let display_title = if context_title.is_empty() {
        "Untitled"
    } else {
        context_title
    };

    let body = match render_context_detail_body(&context, &entries, &siblings, context_id) {
        Ok(body) => body,
        Err(e) => {
            return detail_error_page(ctx, msg, e, "messages detail page: entry decode failed")
                .await
        }
    };

    // A task or notification shows its type and status beside its title; a
    // conversation is a chat, and needs neither.
    let actions = if context.str_field("type") == "conversation" {
        Vec::new()
    } else {
        vec![
            Badge::new(BadgeVariant::Secondary)
                .classes("text-capitalize")
                .render(html! { (context.str_field("type")) }),
            components::status_badge(context.str_field("status")),
        ]
    };
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::admin(display_title, display_title)
            .trail(detail_trail(display_title))
            .actions(actions),
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
) -> Result<crate::ui::PageBody, WaferError> {
    let context_type = context.str_field("type");

    // Why type=conversation diverges: conversation contexts are chat-shaped,
    // so they reuse the canonical chat_page template — same lens the LLM
    // block's /b/llm/ surface uses. Other types (task, notification) hold
    // artifacts and status updates that don't fit a chat composer.
    if context_type == "conversation" {
        return render_conversation_view(context, entries, siblings, context_id);
    }

    // Existing single-pane render path for non-conversation types.
    render_default_view(entries, context_id).map(Into::into)
}

/// Conversation-type view: chat_page template with sibling thread list,
/// entries via `entry_card`, simplified composer (kind=message/role=user).
fn render_conversation_view(
    context: &db::Record,
    entries: &[db::Record],
    siblings: &[db::Record],
    context_id: &str,
) -> Result<crate::ui::PageBody, WaferError> {
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
    let messages_pane = render_conversation_messages(entries)?;
    let composer = render_conversation_composer(&post_url);

    Ok(crate::ui::templates::chat_page(
        crate::ui::templates::ChatFocus::Conversation,
        crate::ui::templates::ChatPane {
            label: "Conversations",
            body: thread_list,
        },
        messages_pane,
        composer,
        None,
    ))
}

/// Default single-pane view for task/notification/etc.
fn render_default_view(entries: &[db::Record], context_id: &str) -> Result<Markup, WaferError> {
    // Rendered up front rather than inside the `@for`: maud's loop body has
    // no way to carry a `?` out, and a card that cannot be decoded must stop
    // the page rather than be skipped.
    let cards = entries
        .iter()
        .map(entry_card)
        .collect::<Result<Vec<_>, _>>()?;
    let post_url = format!("/b/messages/api/contexts/{context_id}/entries");

    Ok(html! {
        div #entries-list .entries-list--scroll .mb-6 {
            @if entries.is_empty() {
                div #entries-empty {
                    (components::empty_state(
                        icons::message_square(),
                        "No entries yet",
                        "Add the first one below.",
                        None,
                    ))
                }
            } @else {
                @for card in &cards {
                    (card)
                }
            }
        }

        section .card { div .card__body {
            (components::section_header("Add entry", None))
            form
                hx-post=(post_url)
                hx-target="#entries-list"
                hx-swap="beforeend"
                // `#entries-empty` is the "No entries yet" line, now
                // contradicted by the entry this swap appended. The effects
                // are applied by `ui/assets/chrome.js` (section 5).
                data-reset-on-success
                data-remove-on-success="entries-empty"
                data-scroll-on-success="entries-list"
            {
                div .entry-form__meta {
                    div .form-group {
                        label .form-label for="entry-kind" { "Kind" }
                        select .form-input #entry-kind name="kind" {
                            option value="message" { "Message" }
                            option value="artifact" { "Artifact" }
                            option value="notification" { "Notification" }
                            option value="status" { "Status" }
                        }
                    }
                    div .form-group {
                        label .form-label for="entry-role" { "Role" }
                        select .form-input #entry-role name="role" {
                            option value="user" { "User" }
                            // `assistant`, not `agent`: both reach the same
                            // stored value (`agent` is a deserialisation
                            // alias of it) and this is the spelling the
                            // column holds and the schema publishes.
                            option value="assistant" { "Assistant" }
                            option value="system" { "System" }
                        }
                    }
                }
                (composer_row("entry-content", "Entry", "Entry content", "Add", false))
            }
        } }
    })
}

/// A composer's text box and its submit button on one row: the box takes
/// the width, the button keeps its own and sits at the box's bottom edge. The
/// box is named by a visually hidden label — a placeholder is not a name.
/// `submit_on_enter` makes Enter send (Shift+Enter for a new line), for a
/// chat; a free-form entry keeps Enter as a line break.
fn composer_row(
    id: &str,
    label: &str,
    placeholder: &str,
    submit: &str,
    submit_on_enter: bool,
) -> Markup {
    html! {
        div .composer {
            label .sr-only for=(id) { (label) }
            textarea .form-input .composer__input
                id=(id)
                name="content"
                placeholder=(placeholder)
                rows="3"
                required
                data-submit-on-enter[submit_on_enter]
            {}
            button .btn .btn--primary .composer__submit type="submit" { (submit) }
        }
    }
}

fn render_conversation_thread_list(siblings: &[&db::Record], active_id: &str) -> Markup {
    html! {
        div .thread-pane {
            div .thread-pane__head {
                h2 .thread-pane__title {
                    "Conversations"
                }
            }
            div #conversation-list .thread-pane__scroll {
                @if siblings.is_empty() {
                    div .text-center .text-muted .thread-pane__empty {
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
                            .card .thread-card
                            href={"/b/messages/contexts/" (id)}
                            data-context-id=(id)
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
    }
}

fn render_conversation_messages(entries: &[db::Record]) -> Result<Markup, WaferError> {
    // See `render_default_view`: the cards are decoded before the markup so
    // a bad row is an error, not a silently missing message.
    let cards = entries
        .iter()
        .map(entry_card)
        .collect::<Result<Vec<_>, _>>()?;
    // The `chat_page` template's `.chat-messages` wrapper already owns
    // scroll, padding, and background for this pane (see
    // styles/layouts/page.css `.page--chat .chat-messages`). We just need
    // the #entries-list ID
    // for the htmx composer's hx-target — no extra scroll container or
    // we double-scroll and end up with a boxed-inside-boxed look.
    Ok(html! {
        div #entries-list {
            @if cards.is_empty() {
                div #entries-empty {
                    (components::empty_state(
                        icons::message_square(),
                        "No messages yet",
                        "Send the first one below.",
                        None,
                    ))
                }
            } @else {
                @for card in &cards {
                    (card)
                }
            }
        }
    })
}

fn render_conversation_composer(post_url: &str) -> Markup {
    html! {
        form
            hx-post=(post_url)
            hx-target="#entries-list"
            hx-swap="beforeend"
            // Scroll `#chat-messages` (the chat_page template's pane
            // wrapper) — `#entries-list` itself is not a scroll container in
            // the conversation view (see render_conversation_messages).
            // `#entries-empty` is the "No messages yet" line, now
            // contradicted by the message this swap appended. The effects
            // are applied by `ui/assets/chrome.js` (section 5).
            data-reset-on-success
            data-remove-on-success="entries-empty"
            data-scroll-on-success=(crate::ui::templates::CHAT_MESSAGES_ID)
        {
            // Hidden defaults: kind=message, role=user. Conversation lens is
            // an opinionated view — composers below the fold (settings page,
            // direct API) can still post other kinds/roles.
            input type="hidden" name="kind" value="message";
            input type="hidden" name="role" value="user";
            (composer_row("message-content", "Message", "Type your message…", "Send", true))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(id: &str) -> db::Record {
        db::Record {
            id: id.to_string(),
            data: Default::default(),
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

        let html = render_context_detail_body(&ctx_rec, &[], &[], "ctx-1")
            .expect("the fixture rows decode")
            .into_markup()
            .into_string();
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

        let html = render_context_detail_body(&ctx_rec, &[], &[], "ctx-1")
            .expect("the fixture rows decode")
            .into_markup()
            .into_string();
        assert!(
            !html.contains(r#"class="page--chat""#),
            "task type should keep the existing single-pane shell"
        );
        assert!(
            html.contains(r#"id="entries-list""#),
            "single-pane shell still has the entries-list container"
        );
        // The title is the topbar's; the body repeats no heading for it and
        // draws no second way back beside the trail.
        assert!(!html.contains("Do thing"), "{html}");
        assert!(!html.contains("page-title"), "{html}");
        // Both pickers are labelled, and the composer is the shared row.
        assert!(
            html.contains(r#"<label class="form-label" for="entry-kind">Kind</label>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<label class="form-label" for="entry-role">Role</label>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<div class="composer"><label class="sr-only" for="entry-content">Entry</label>"#
            ),
            "{html}"
        );
    }

    fn entry(role: &str) -> db::Record {
        let mut r = make_record("e-1");
        r.data
            .insert("kind".to_string(), serde_json::json!("message"));
        r.data.insert("role".to_string(), serde_json::json!(role));
        r.data
            .insert("content".to_string(), serde_json::json!("hi"));
        r.data.insert(
            "created_at".to_string(),
            serde_json::json!("2026-05-06T10:00:00Z"),
        );
        r
    }

    /// Entry badges come from the shared component: neutral for kind and
    /// role, warning for a system turn. The person's own turn is the
    /// `--user` card (a slate tint, see card.css), never a danger colour.
    #[test]
    fn entry_cards_draw_shared_badges() {
        let user = entry_card(&entry("user")).expect("decodes").into_string();
        assert!(
            user.contains(r#"class="card message-card--user""#),
            "{user}"
        );
        assert!(
            user.contains(r#"<span class="badge badge-secondary text-capitalize">message</span>"#),
            "{user}"
        );
        assert!(
            user.contains(r#"<span class="badge badge-secondary text-capitalize">user</span>"#),
            "{user}"
        );
        assert!(user.contains("<time"), "the date is a timestamp: {user}");
        assert!(!user.contains("danger"), "{user}");

        let system = entry_card(&entry("system")).expect("decodes").into_string();
        assert!(
            system.contains(r#"<span class="badge badge-warning text-capitalize">message</span>"#),
            "{system}"
        );
    }

    #[test]
    fn a_list_row_draws_shared_badges() {
        let mut r = make_record("c-1");
        r.data.insert("type".to_string(), serde_json::json!("task"));
        r.data
            .insert("status".to_string(), serde_json::json!("active"));
        r.data
            .insert("title".to_string(), serde_json::json!("Rotate keys"));
        let html = context_card(&r).into_string();
        assert!(
            html.contains(r#"<span class="badge badge-secondary text-capitalize">task</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="badge badge-success">active</span>"#),
            "{html}"
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
                .expect("the fixture rows decode")
                .into_markup()
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

        let html = render_context_detail_body(&ctx_rec, &[], &[], "ctx-1")
            .expect("the fixture rows decode")
            .into_markup()
            .into_string();

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

#[cfg(test)]
mod outage_tests {
    //! The two messages pages during a database outage.
    //!
    //! These were the block's only three swallow sites, and the only ones
    //! that did not even log — while four lines above the entries read, the
    //! context lookup already routes its tail through
    //! `crud::db_error_internal`.

    use super::*;
    use crate::{
        blocks::messages::{
            service::{self, CONTEXTS_TABLE, ENTRIES_TABLE},
            test_support::{ctx_with_messages, routed},
        },
        test_support::{admin_msg, output_http_status, FailingDbOpContext, TestContext},
    };

    async fn seed_conversation(ctx: &TestContext) -> String {
        service::create_context(
            ctx,
            "admin_1",
            "conversation",
            "Renewal",
            "",
            "",
            None,
            None,
        )
        .await
        .expect("seed a context")
        .id
    }

    /// "No messages yet" is what a fresh deployment renders. An outage
    /// rendered the same empty state.
    #[tokio::test]
    async fn a_failing_context_list_renders_the_error_page_not_no_contexts_yet() {
        let ctx = ctx_with_messages().await.break_reads();
        let out = context_list_page(&ctx, &routed(admin_msg("retrieve", "/b/messages/"))).await;
        assert_eq!(output_http_status(out).await, 500);
    }

    /// The context itself is found and only its entries fail: the page used
    /// to render the conversation with no messages in it.
    #[tokio::test]
    async fn a_failing_entry_list_renders_the_error_page_not_an_empty_conversation() {
        let ctx = ctx_with_messages().await;
        let id = seed_conversation(&ctx).await;

        let failing = FailingDbOpContext::new(ctx.clone(), vec![("database.list", ENTRIES_TABLE)]);
        let msg = routed(admin_msg("retrieve", &format!("/b/messages/contexts/{id}")));
        assert_eq!(
            output_http_status(context_detail_page(&failing, &msg).await).await,
            500
        );
    }

    /// The sibling thread list feeds the conversation view's thread pane —
    /// the same pane whose emptiness hid a dead read in the llm chat page for
    /// several PRs. A failed read must not render as "this is the only
    /// conversation".
    #[tokio::test]
    async fn a_failing_sibling_list_renders_the_error_page_not_a_lone_thread() {
        let ctx = ctx_with_messages().await;
        let id = seed_conversation(&ctx).await;

        // The context lookup is a `database.get` and still lands; only the
        // sibling listing on the contexts table fails.
        let failing = FailingDbOpContext::new(ctx.clone(), vec![("database.list", CONTEXTS_TABLE)]);
        let msg = routed(admin_msg("retrieve", &format!("/b/messages/contexts/{id}")));
        assert_eq!(
            output_http_status(context_detail_page(&failing, &msg).await).await,
            500
        );
    }
}

#[cfg(test)]
mod form_contract_tests {
    //! What the block's three htmx forms put on the wire.
    //!
    //! None of them declares an encoding — and no encoding extension is
    //! shipped with the chrome, so declaring one would change nothing — which
    //! means htmx submits them as `application/x-www-form-urlencoded` with
    //! these field names. `rest.rs`'s handler tests post exactly these bytes;
    //! this module is what keeps the two halves of that contract from
    //! drifting apart.

    use super::*;
    use crate::{
        blocks::messages::test_support::{ctx_with_messages, routed},
        test_support::{admin_msg, output_html},
    };

    fn context_of_type(context_type: &str) -> db::Record {
        let mut record = db::Record {
            id: "ctx-1".to_string(),
            data: Default::default(),
        };
        record
            .data
            .insert("type".to_string(), serde_json::json!(context_type));
        record
    }

    /// The empty state an empty list renders, and the handler that removes it,
    /// name the same element.
    ///
    /// A successful swap has just put a row into the list, so the sentence
    /// saying there is none is false from that moment until the next page
    /// load. Asserted on both ends because an id that only one side spells is
    /// exactly how this stops working.
    fn assert_drops_empty_state(html: &str, id: &str) {
        assert!(
            html.contains(&format!(r#"id="{id}""#)),
            "an empty list must render #{id}; got: {html}"
        );
        assert!(
            html.contains(&format!(r#"data-remove-on-success="{id}""#)),
            "the form that fills the list must drop #{id}; got: {html}"
        );
    }

    fn assert_posts_form_fields(html: &str, post_url: &str, fields: &[&str]) {
        assert!(
            html.contains(&format!(r#"hx-post="{post_url}""#)),
            "form must post to {post_url}; got: {html}"
        );
        assert!(
            !html.contains("hx-ext"),
            "an encoding extension would be inert — none is shipped — so the \
             body is form-encoded either way; got: {html}"
        );
        for field in fields {
            assert!(
                html.contains(&format!(r#"name="{field}""#)),
                "form must send `{field}`; got: {html}"
            );
        }
    }

    /// The list is "Messages" in the topbar, as it is in the sidebar, and a
    /// context's trail leads back to it under the same name.
    #[tokio::test]
    async fn the_pages_are_named_messages_as_the_nav_names_them() {
        let ctx = ctx_with_messages().await;
        let html = output_html(
            context_list_page(&ctx, &routed(admin_msg("retrieve", "/b/messages/"))).await,
        )
        .await;
        assert!(
            html.contains(r#"<h1 class="topbar__title">Messages</h1>"#),
            "{html}"
        );
        assert!(!html.contains("Contexts"), "{html}");
        assert!(
            html.contains(r#"<h2 class="empty__title">No messages yet</h2>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<button class="btn btn--primary" type="submit">Create</button>"#),
            "{html}"
        );
    }

    #[tokio::test]
    async fn the_new_context_form_posts_the_fields_create_context_reads() {
        let ctx = ctx_with_messages().await;
        let html = output_html(
            context_list_page(&ctx, &routed(admin_msg("retrieve", "/b/messages/"))).await,
        )
        .await;
        assert_posts_form_fields(&html, "/b/messages/api/contexts", &["type", "title"]);
        assert_drops_empty_state(&html, "context-list-empty");
    }

    #[test]
    fn the_conversation_composer_posts_the_fields_add_entry_reads() {
        let html = render_context_detail_body(&context_of_type("conversation"), &[], &[], "ctx-1")
            .expect("the fixture row decodes")
            .into_markup()
            .into_string();
        assert_posts_form_fields(
            &html,
            "/b/messages/api/contexts/ctx-1/entries",
            &["kind", "role", "content"],
        );
        assert_drops_empty_state(&html, "entries-empty");
    }

    #[test]
    fn the_default_view_composer_posts_the_fields_add_entry_reads() {
        let html = render_context_detail_body(&context_of_type("task"), &[], &[], "ctx-1")
            .expect("the fixture row decodes")
            .into_markup()
            .into_string();
        assert_posts_form_fields(
            &html,
            "/b/messages/api/contexts/ctx-1/entries",
            &["kind", "role", "content"],
        );
        assert_drops_empty_state(&html, "entries-empty");
    }
}
