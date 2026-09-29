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
    blocks::crud,
    ui::{self, shell::Crumb},
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
    let date = updated_at.get(..10).unwrap_or(updated_at);

    html! {
        a .messages-list__item href={"/b/messages/contexts/" (id)} {
            span .badge .messages-list__type { (context_type) }
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
    let date = created_at.get(..10).unwrap_or(created_at);

    // Card accents follow the brand: user entries get the brand tint (same
    // pair as `.sidebar__nav-item.is-active`), machine entries stay neutral,
    // and only genuinely semantic kinds keep a semantic hue (notification/
    // system = warning yellow). The old palette hardcoded sky/indigo blues
    // that clashed with the orange brand. Keep in sync with
    // `messageCardHtml` in blocks/llm/assets/llm-chat.js — same cards, JS-rendered.
    let (card_variant, badge_class) = match kind {
        EntryKind::Artifact | EntryKind::Status => ("message-card--neutral", "badge"),
        EntryKind::Notification => ("message-card--warning", "badge-warning"),
        EntryKind::Message => match role {
            Some(EntryRole::User) => ("message-card--user", "badge"),
            Some(EntryRole::Assistant) | None => ("message-card--neutral", "badge"),
            Some(EntryRole::System) => ("message-card--warning", "badge-warning"),
        },
    };

    Ok(html! {
        div .card .(card_variant) {
            div .flex .items-center .gap-2 .mb-2 {
                span .badge .(badge_class) .text-capitalize { (wire_str(&kind)) }
                @if let Some(role) = role {
                    span .badge .text-capitalize { (wire_str(&role)) }
                }
                @if kind == EntryKind::Artifact
                    && !content_type.is_empty()
                    && content_type != "text/plain"
                {
                    span .text-muted .text-xs { (content_type) }
                }
                @if !date.is_empty() {
                    span .text-muted .text-xs .ml-auto { (date) }
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
        Err(e) => return crud::db_error_page(msg, e, "messages context list page: read failed"),
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
                div #context-list-empty .messages-list__empty {
                    p { "No contexts yet — create one above." }
                }
            } @else {
                @for context in &contexts {
                    (context_card(context))
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
        Err(e) => return crud::db_error_internal(e, "Database error"),
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
        Err(e) => return crud::db_error_page(msg, e, "messages detail page: entry list failed"),
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
                return crud::db_error_page(msg, e, "messages detail page: sibling list failed")
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
        Err(e) => return crud::db_error_internal(e, "Entry decode"),
    };

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
) -> Result<Markup, WaferError> {
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
) -> Result<Markup, WaferError> {
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
        thread_list,
        messages_pane,
        composer,
        None,
    ))
}

/// Default single-pane view for task/notification/etc.
fn render_default_view(
    context: &db::Record,
    entries: &[db::Record],
    context_id: &str,
) -> Result<Markup, WaferError> {
    // Rendered up front rather than inside the `@for`: maud's loop body has
    // no way to carry a `?` out, and a card that cannot be decoded must stop
    // the page rather than be skipped.
    let cards = entries
        .iter()
        .map(entry_card)
        .collect::<Result<Vec<_>, _>>()?;
    let context_title = context.str_field("title");
    let context_type = context.str_field("type");
    let context_status = context.str_field("status");
    let display_title = if context_title.is_empty() {
        "Untitled"
    } else {
        context_title
    };
    let post_url = format!("/b/messages/api/contexts/{context_id}/entries");

    Ok(html! {
        div .flex .items-center .gap-3 .mb-6 {
            a .btn .btn--ghost .btn--sm href="/b/messages/" { (ui::icons::arrow_left()) " Back" }
            h2 .page-title .m-0 { (display_title) }
            span .badge .text-capitalize { (context_type) }
            span .badge { (context_status) }
        }

        div #entries-list .entries-list--scroll .mb-6 {
            @if entries.is_empty() {
                div #entries-empty .text-center .text-muted .p-8 {
                    "No entries yet. Add one below."
                }
            } @else {
                @for card in &cards {
                    (card)
                }
            }
        }

        div .card {
            h3 .text-sm .font-semibold .text-muted .mb-3 {
                "Add Entry"
            }
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
                div .flex .gap-2 .mb-2 {
                    select .form-input .w-auto name="kind" {
                        option value="message" { "message" }
                        option value="artifact" { "artifact" }
                        option value="notification" { "notification" }
                        option value="status" { "status" }
                    }
                    select .form-input .w-auto name="role" {
                        option value="user" { "user" }
                        // `assistant`, not `agent`: both reach the same
                        // stored value (`agent` is a deserialisation alias
                        // of it) and this is the spelling the column holds
                        // and the schema publishes.
                        option value="assistant" { "assistant" }
                        option value="system" { "system" }
                    }
                }
                div .flex .gap-2 .items-end {
                    textarea .form-input .flex-1 .resize-vertical
                        name="content"
                        placeholder="Entry content"
                        rows="3"
                        required
                    {}
                    button .btn .btn--primary type="submit" { "Add" }
                }
            }
        }
    })
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
                div #entries-empty .text-center .text-muted .p-8 {
                    "No messages yet. Send the first one below."
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
            div .flex .gap-2 .items-end {
                textarea
                    .form-input .flex-1 .resize-none
                    name="content"
                    placeholder="Type your message..."
                    rows="3"
                    required
                    data-submit-on-enter
                {}
                button .btn .btn--primary .h-fit type="submit" { "Send" }
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

        let html = render_context_detail_body(&ctx_rec, &[], &[], "ctx-1")
            .expect("the fixture rows decode")
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
            .into_string();
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
                .expect("the fixture rows decode")
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

    /// "No contexts yet — create one above." is what a fresh deployment
    /// renders. An outage rendered the same sentence.
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
            data: std::collections::HashMap::new(),
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
            .into_string();
        assert_posts_form_fields(
            &html,
            "/b/messages/api/contexts/ctx-1/entries",
            &["kind", "role", "content"],
        );
        assert_drops_empty_state(&html, "entries-empty");
    }
}
