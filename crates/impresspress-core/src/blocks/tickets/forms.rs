//! The htmx form handlers behind the tickets admin pages.
//!
//! Each one reads the form body a browser posts, makes the same service call
//! as its JSON twin in `rest`, and answers with what the page swaps in: the
//! re-rendered region and a toast, or a redirect. A refusal is the JSON error
//! envelope `rest` answers, which htmx does not swap and the shared chrome
//! turns into an error toast.

use std::collections::HashMap;

use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{
    models::{
        ActorType, CreateTicketInput, TicketSource, TicketTypeInput, TicketTypeUpdate,
        WorkflowUpdate,
    },
    pages::{self, TicketPage},
    repo, rest, service,
};
use crate::{
    blocks::crud,
    http::{err_bad_request, ResponseBuilder},
    ui,
    util::parse_form_body,
};

/// `POST /b/tickets/admin/tickets`: the inbox's New ticket form. A created
/// ticket is opened: the answer redirects to its page.
pub async fn create_ticket(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let form = match read_form(input).await {
        Ok(form) => form,
        Err(response) => return response,
    };
    let ticket = CreateTicketInput {
        type_id: text(&form, "type_id"),
        subject: text(&form, "subject"),
        description: text(&form, "description"),
        source_path: String::new(),
        subject_type: String::new(),
        subject_id: String::new(),
        evidence_url: String::new(),
        reporter_email: String::new(),
        reporter_wants_reply: false,
        priority: chosen(&form, "priority"),
    };
    match service::create_ticket(
        ctx,
        ticket,
        TicketSource::Admin,
        ActorType::Admin,
        msg.user_id(),
        None,
    )
    .await
    {
        Ok(record) => ResponseBuilder::new()
            .set_header("HX-Redirect", &format!("{}/{}", pages::INBOX, record.id))
            .body(Vec::new(), "text/html; charset=utf-8"),
        Err(error) => rest::service_error(error),
    }
}

/// `PATCH /b/tickets/admin/tickets/{id}`: the ticket page's Workflow form.
///
/// The form always carries the status and priority it was rendered with, so
/// an unchanged select is the current value, which the service treats as no
/// change. An empty Duplicate of is left out rather than sent empty: it keeps
/// a duplicate ticket's target, and moving to any other status clears it.
pub async fn update_ticket(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let form = match read_form(input).await {
        Ok(form) => form,
        Err(response) => return response,
    };
    let id = msg.var("id");
    let update = WorkflowUpdate {
        status: chosen(&form, "status"),
        priority: chosen(&form, "priority"),
        assignee_id: Some(text(&form, "assignee_id").trim().to_string()),
        duplicate_of: chosen(&form, "duplicate_of"),
        legal_hold: Some(checked(&form, "legal_hold")),
        reason: text(&form, "reason"),
    };
    match service::update_workflow(ctx, id, update, ActorType::Admin, msg.user_id()).await {
        Ok(_) => ticket_swap(ctx, id, "Ticket updated").await,
        Err(error) => rest::service_error(error),
    }
}

/// `POST /b/tickets/admin/tickets/{id}/notes`: the ticket page's note form.
pub async fn add_note(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let form = match read_form(input).await {
        Ok(form) => form,
        Err(response) => return response,
    };
    let id = msg.var("id");
    match service::add_note(
        ctx,
        id,
        &text(&form, "note"),
        ActorType::Admin,
        msg.user_id(),
    )
    .await
    {
        Ok(_) => ticket_swap(ctx, id, "Note added").await,
        Err(error) => rest::service_error(error),
    }
}

/// `POST /b/tickets/admin/types`: the types page's New type form.
pub async fn create_type(ctx: &dyn Context, input: InputStream) -> OutputStream {
    let form = match read_form(input).await {
        Ok(form) => form,
        Err(response) => return response,
    };
    let sort_order = match sort_order(&form) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let kind = TicketTypeInput {
        key: text(&form, "key").trim().to_string(),
        title: text(&form, "title"),
        description: text(&form, "description"),
        guidance: text(&form, "guidance"),
        default_priority: text(&form, "default_priority"),
        escalation_kind: text(&form, "escalation_kind"),
        public_visible: checked(&form, "public_visible"),
        requires_contact: checked(&form, "requires_contact"),
        requests_evidence: checked(&form, "requests_evidence"),
        active: checked(&form, "active"),
        sort_order,
    };
    let key = kind.key.clone();
    match service::create_type(ctx, kind).await {
        Ok(_) => types_swap(ctx, pages::CREATE_TYPE_MODAL, "Ticket type created").await,
        Err(error) => rest::create_type_failure(error, &key),
    }
}

/// `PATCH /b/tickets/admin/types/{id}`: a type's Edit form. Every field is
/// on the form, so every field is sent; an unticked option is `false`.
pub async fn update_type(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let form = match read_form(input).await {
        Ok(form) => form,
        Err(response) => return response,
    };
    let sort_order = match sort_order(&form) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let id = msg.var("id");
    let update = TicketTypeUpdate {
        key: None,
        title: Some(text(&form, "title")),
        description: Some(text(&form, "description")),
        guidance: Some(text(&form, "guidance")),
        default_priority: Some(text(&form, "default_priority")),
        escalation_kind: Some(text(&form, "escalation_kind")),
        public_visible: Some(checked(&form, "public_visible")),
        requires_contact: Some(checked(&form, "requires_contact")),
        requests_evidence: Some(checked(&form, "requests_evidence")),
        active: Some(checked(&form, "active")),
        sort_order: Some(sort_order),
    };
    match service::update_type(ctx, id, update).await {
        Ok(_) => types_swap(ctx, &pages::edit_type_modal_id(id), "Ticket type saved").await,
        Err(error) => rest::service_error(error),
    }
}

/// The ticket page re-rendered after a write, with `applied` as the toast.
///
/// Called only once the write has landed, so a failed re-read must not look
/// like a failed write: the region becomes a notice saying the change was
/// made and the page needs a reload.
async fn ticket_swap(ctx: &dyn Context, id: &str, applied: &str) -> OutputStream {
    match TicketPage::load(ctx, id).await {
        Ok(view) => ui::html_response_with_toast(pages::detail_region(&view), applied, "success"),
        Err(error) => {
            let reason = crud::db_error_notice(error, "tickets admin: ticket re-read failed");
            ui::swap_error_response(
                pages::DETAIL_REGION,
                &format!(
                    "{applied}, but the ticket could not be loaded again: {reason}. Reload the \
                     page to see it."
                ),
            )
        }
    }
}

/// The type list re-rendered after a write from the modal `modal_id`, which
/// closes, with `applied` as the toast. A failed re-read is a notice in the
/// list's place, as in [`ticket_swap`].
async fn types_swap(ctx: &dyn Context, modal_id: &str, applied: &str) -> OutputStream {
    match repo::list_types(ctx, false, 100, 0).await {
        Ok(rows) => ui::html_response_closing_modal(
            pages::types_region(&rows.records),
            modal_id,
            applied,
            "success",
        ),
        Err(error) => {
            let reason = crud::db_error_notice(error, "tickets admin: type list re-read failed");
            ui::swap_error_response(
                pages::TYPES_REGION,
                &format!(
                    "{applied}, but the type list could not be loaded again: {reason}. Reload \
                     the page to see it."
                ),
            )
        }
    }
}

/// The posted form, refused with 413 past the admin body limit.
async fn read_form(input: InputStream) -> Result<HashMap<String, String>, OutputStream> {
    let raw = input
        .collect_to_bytes()
        .await
        .map_err(OutputStream::error)?;
    if raw.len() > rest::MAX_ADMIN_BODY {
        return Err(rest::too_large());
    }
    Ok(parse_form_body(&raw))
}

/// The field `name` as posted, or empty when the form did not send it.
fn text(form: &HashMap<String, String>, name: &str) -> String {
    form.get(name).cloned().unwrap_or_default()
}

/// The field `name` when it holds a value: an empty select option ("the
/// type's default") or an empty input means "not given".
fn chosen(form: &HashMap<String, String>, name: &str) -> Option<String> {
    form.get(name)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Whether the checkbox `name` was ticked: a browser sends a ticked box and
/// leaves an unticked one out.
fn checked(form: &HashMap<String, String>, name: &str) -> bool {
    form.contains_key(name)
}

/// The Order field as a number; an empty field is 0.
fn sort_order(form: &HashMap<String, String>) -> Result<i64, OutputStream> {
    match chosen(form, "sort_order") {
        None => Ok(0),
        Some(value) => value
            .parse()
            .map_err(|_| err_bad_request("Order must be a whole number.")),
    }
}

#[cfg(test)]
mod tests {
    use wafer_run::{Block, InputStream};

    use super::*;
    use crate::{
        blocks::tickets::{pages::render_tests::seeded, TicketsBlock},
        endpoint_match,
        test_support::{admin_msg, TestContext},
    };

    /// Post `fields` to `path` the way htmx submits a form, through the
    /// block's own dispatch.
    async fn submit(
        ctx: &TestContext,
        action: &str,
        path: &str,
        fields: &[(&str, &str)],
    ) -> wafer_block::http_codec::HttpResponseParts {
        let mut msg = admin_msg(action, path);
        msg.set_meta("http.header.hx-request", "true");
        msg.set_meta(
            "http.header.content-type",
            "application/x-www-form-urlencoded",
        );
        assert!(
            endpoint_match::dispatch(&mut msg, crate::blocks::tickets::ROUTES).is_some(),
            "no route for {action} {path}"
        );
        let mut body = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in fields {
            body.append_pair(name, value);
        }
        let out = TicketsBlock::new()
            .handle(
                ctx,
                msg,
                InputStream::from_bytes(body.finish().into_bytes()),
            )
            .await;
        wafer_block::http_codec::collect_http_response(out).await
    }

    fn header<'a>(parts: &'a wafer_block::http_codec::HttpResponseParts, name: &str) -> &'a str {
        parts
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map_or("", |(_, value)| value.as_str())
    }

    #[tokio::test]
    async fn a_new_ticket_opens_its_own_page() {
        let (ctx, kind, _) = seeded().await;
        let parts = submit(
            &ctx,
            "create",
            pages::INBOX,
            &[
                ("type_id", &kind),
                ("subject", "Footer link is broken"),
                ("description", "The privacy link in the footer answers 404."),
                ("priority", ""),
            ],
        )
        .await;
        assert_eq!(
            parts.status,
            200,
            "{}",
            String::from_utf8_lossy(&parts.body)
        );
        let target = header(&parts, "HX-Redirect");
        let id = target
            .strip_prefix(&format!("{}/", pages::INBOX))
            .expect("redirects to the ticket page");
        let created = repo::get_ticket(&ctx, id).await.expect("ticket exists");
        assert_eq!(service::str_field(&created, "source"), "admin");
        assert_eq!(
            service::str_field(&created, "priority"),
            "normal",
            "an empty priority is the type's default"
        );
    }

    /// A refused creation is the error envelope htmx does not swap, with the
    /// service's own sentence for the toast.
    #[tokio::test]
    async fn a_refused_ticket_is_a_400_with_the_reason() {
        let (ctx, kind, _) = seeded().await;
        let parts = submit(
            &ctx,
            "create",
            pages::INBOX,
            &[
                ("type_id", &kind),
                ("subject", "Hi"),
                ("description", "Too short"),
            ],
        )
        .await;
        assert_eq!(parts.status, 400);
        assert!(header(&parts, "HX-Redirect").is_empty());
        assert!(String::from_utf8_lossy(&parts.body).contains("subject"));
    }

    /// The Workflow form changes the status and answers with the ticket
    /// region (the new badge in it) and a toast.
    #[tokio::test]
    async fn the_workflow_form_moves_the_ticket_and_re_renders_it() {
        let (ctx, _, ticket) = seeded().await;
        let parts = submit(
            &ctx,
            "update",
            &format!("{}/{ticket}", pages::INBOX),
            &[
                ("status", "investigating"),
                ("priority", "high"),
                ("assignee_id", "admin_1"),
                ("duplicate_of", ""),
                ("reason", "Picked up"),
                ("legal_hold", "true"),
            ],
        )
        .await;
        let html = String::from_utf8_lossy(&parts.body);
        assert_eq!(parts.status, 200, "{html}");
        assert!(
            html.starts_with(r#"<div class="ticket-detail" id="ticket-detail">"#),
            "{html}"
        );
        assert!(html.contains(">Investigating</span>"), "{html}");
        assert!(html.contains(">Legal hold</span>"), "{html}");
        assert!(header(&parts, "HX-Trigger").contains("Ticket updated"));
        let stored = repo::get_ticket(&ctx, &ticket).await.expect("ticket");
        assert_eq!(service::str_field(&stored, "status"), "investigating");
        assert_eq!(service::str_field(&stored, "assignee_id"), "admin_1");
        assert!(service::bool_field(&stored, "legal_hold"));
    }

    /// The form re-sends every field it shows. Saving it unchanged writes
    /// nothing and adds no timeline entry; changing one field records only
    /// that field.
    #[tokio::test]
    async fn only_changed_workflow_fields_are_written_and_recorded() {
        let (ctx, _, ticket) = seeded().await;
        let unchanged = [
            ("status", "new"),
            ("priority", "urgent"),
            ("assignee_id", ""),
            ("duplicate_of", ""),
            ("reason", ""),
        ];
        let path = format!("{}/{ticket}", pages::INBOX);
        let before = repo::list_events(&ctx, &ticket, 50)
            .await
            .expect("events")
            .len();
        assert_eq!(submit(&ctx, "update", &path, &unchanged).await.status, 200);
        let after = repo::list_events(&ctx, &ticket, 50).await.expect("events");
        assert_eq!(after.len(), before, "an unchanged save is not an event");

        let mut changed = unchanged;
        changed[1] = ("priority", "low");
        assert_eq!(submit(&ctx, "update", &path, &changed).await.status, 200);
        let events = repo::list_events(&ctx, &ticket, 50).await.expect("events");
        assert_eq!(events.len(), before + 1);
        let metadata = events[0]
            .data
            .get("metadata_json")
            .or_else(|| events[0].data.get("metadata"))
            .cloned()
            .unwrap_or_default();
        let metadata: serde_json::Value = match metadata {
            serde_json::Value::String(text) => serde_json::from_str(&text).unwrap_or_default(),
            value => value,
        };
        assert_eq!(
            metadata,
            serde_json::json!({ "priority": "low" }),
            "{:?}",
            events[0]
        );
    }

    /// Closing without a reason is refused with the service's sentence, and
    /// nothing changes.
    #[tokio::test]
    async fn closing_without_a_reason_is_refused() {
        let (ctx, _, ticket) = seeded().await;
        let parts = submit(
            &ctx,
            "update",
            &format!("{}/{ticket}", pages::INBOX),
            &[
                ("status", "resolved"),
                ("priority", "urgent"),
                ("assignee_id", ""),
            ],
        )
        .await;
        assert_eq!(parts.status, 400);
        assert!(String::from_utf8_lossy(&parts.body).contains("a reason is required"));
        let stored = repo::get_ticket(&ctx, &ticket).await.expect("ticket");
        assert_eq!(service::str_field(&stored, "status"), "new");
    }

    #[tokio::test]
    async fn a_note_lands_on_the_timeline() {
        let (ctx, _, ticket) = seeded().await;
        let parts = submit(
            &ctx,
            "create",
            &format!("{}/{ticket}/notes", pages::INBOX),
            &[("note", "Called the venue.")],
        )
        .await;
        let html = String::from_utf8_lossy(&parts.body);
        assert_eq!(parts.status, 200, "{html}");
        assert!(html.contains("Internal note</strong>"), "{html}");
        assert!(html.contains("Called the venue."), "{html}");
        assert!(header(&parts, "HX-Trigger").contains("Note added"));
    }

    /// A created type closes its modal and comes back in the list; an
    /// unticked option is `false`.
    #[tokio::test]
    async fn a_new_type_closes_the_modal_and_joins_the_list() {
        let ctx = TestContext::with_tickets().await;
        let parts = submit(
            &ctx,
            "create",
            pages::TYPES,
            &[
                ("key", "billing"),
                ("title", "Billing"),
                ("description", ""),
                ("guidance", ""),
                ("default_priority", "high"),
                ("escalation_kind", "none"),
                ("sort_order", "5"),
                ("active", "true"),
            ],
        )
        .await;
        let html = String::from_utf8_lossy(&parts.body);
        assert_eq!(parts.status, 200, "{html}");
        assert!(html.starts_with(r#"<div id="ticket-types">"#), "{html}");
        assert!(html.contains(">Billing</span>"), "{html}");
        let trigger = header(&parts, "HX-Trigger");
        assert!(
            trigger.contains(r#""closeModal":{"id":"create-type"}"#),
            "{trigger}"
        );
        let rows = repo::list_types(&ctx, false, 10, 0)
            .await
            .expect("types")
            .records;
        assert!(service::bool_field(&rows[0], "active"));
        assert!(!service::bool_field(&rows[0], "public_visible"));
    }

    #[tokio::test]
    async fn a_taken_key_and_a_bad_order_are_refused() {
        let (ctx, _, _) = seeded().await;
        let taken = submit(
            &ctx,
            "create",
            pages::TYPES,
            &[
                ("key", "incorrect-info"),
                ("title", "Again"),
                ("default_priority", "normal"),
                ("escalation_kind", "none"),
            ],
        )
        .await;
        assert_eq!(taken.status, 409);
        let order = submit(
            &ctx,
            "create",
            pages::TYPES,
            &[
                ("key", "other"),
                ("title", "Other"),
                ("default_priority", "normal"),
                ("escalation_kind", "none"),
                ("sort_order", "first"),
            ],
        )
        .await;
        assert_eq!(order.status, 400);
        assert!(String::from_utf8_lossy(&order.body).contains("Order must be a whole number"));
    }

    /// The Edit form sends every field, so unticking an option turns it off.
    #[tokio::test]
    async fn editing_a_type_saves_every_field() {
        let (ctx, kind, _) = seeded().await;
        let parts = submit(
            &ctx,
            "update",
            &format!("{}/{kind}", pages::TYPES),
            &[
                ("title", "Wrong information"),
                ("description", ""),
                ("guidance", "Say what is wrong."),
                ("default_priority", "low"),
                ("escalation_kind", "none"),
                ("sort_order", "1"),
                ("active", "true"),
            ],
        )
        .await;
        assert_eq!(
            parts.status,
            200,
            "{}",
            String::from_utf8_lossy(&parts.body)
        );
        assert!(header(&parts, "HX-Trigger").contains(&format!("edit-type-{kind}")));
        let stored = repo::get_type(&ctx, &kind).await.expect("type");
        assert_eq!(service::str_field(&stored, "title"), "Wrong information");
        assert!(!service::bool_field(&stored, "requests_evidence"));
    }
}
