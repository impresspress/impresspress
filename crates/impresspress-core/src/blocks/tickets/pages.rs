//! Server-rendered administration pages for tickets and ticket types.

use maud::{html, Markup, PreEscaped};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, Message, OutputStream};

use super::{config::SecurityReadiness, repo, service};
use crate::{
    blocks::crud,
    ui::{self, components},
};

const ADMIN_FORM_JS: &str = r#"
document.querySelectorAll('form[data-json-form]').forEach((form) => {
  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    const payload = {};
    for (const element of form.elements) {
      if (!element.name) continue;
      if (element.type === 'checkbox') {
        payload[element.name] = element.checked;
      } else if (element.type === 'number') {
        payload[element.name] = Number(element.value || 0);
      } else if (element.value !== '' || element.dataset.keepEmpty === 'true') {
        payload[element.name] = element.value;
      }
    }
    const response = await fetch(form.dataset.endpoint, {
      method: form.dataset.method || 'POST',
      headers: {'Content-Type': 'application/json', 'Accept': 'application/json'},
      body: JSON.stringify(payload)
    });
    if (!response.ok) {
      alert((await response.text()) || 'The request failed');
      return;
    }
    location.reload();
  });
});
"#;

pub async fn inbox(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let (_, page_size, offset) = msg.pagination_params(25);
    let filters = repo::TicketFilters {
        status: option(msg.query("status")),
        priority: option(msg.query("priority")),
        type_id: option(msg.query("type_id")),
        source: option(msg.query("source")),
        assignee_id: option(msg.query("assignee_id")),
    };
    let tickets =
        match repo::list_tickets(ctx, &filters, page_size.min(100) as u32, offset as i64).await {
            Ok(rows) => rows,
            Err(error) => return crud::db_error_page(msg, error, "Could not load tickets"),
        };
    // The type filter is drawn from this list, so an empty one would say the
    // deployment has no ticket types; a failed read fails the page instead.
    let types = match repo::list_types(ctx, false, 100, 0).await {
        Ok(rows) => rows.records,
        Err(error) => return crud::db_error_page(msg, error, "Could not load ticket types"),
    };
    let pagination_base = inbox_pagination_base(msg);
    let content = html! {
        style {
            (maud::PreEscaped(r#"
                @media (max-width: 720px) {
                    .ticket-filter-grid {
                        grid-template-columns: repeat(2, minmax(0, 1fr)) !important;
                    }
                    .ticket-filter-assignee { grid-column: 1 / -1; }
                    .tickets-table .ticket-col-type,
                    .tickets-table .ticket-col-source,
                    .tickets-table .ticket-col-age { display: none; }
                }
            "#))
        }
        (components::page_header(
            "Tickets",
            Some("Review public reports and internal work items"),
            None,
        ))
        form method="get" action="/b/tickets/admin/tickets"
            .mb-4 {
            div .ticket-filter-grid {
                select .form-input name="status" {
                    option value="" { "All statuses" }
                    @for status in ["new", "triaged", "investigating", "resolved", "rejected", "spam", "duplicate"] {
                        option value=(status) selected[status == msg.query("status")] { (status) }
                    }
                }
                select .form-input name="priority" {
                    option value="" { "All priorities" }
                    @for priority in ["low", "normal", "high", "urgent"] {
                        option value=(priority) selected[priority == msg.query("priority")] { (priority) }
                    }
                }
                select .form-input name="type_id" {
                    option value="" { "All types" }
                    @for kind in &types {
                        option value=(kind.id) selected[kind.id == msg.query("type_id")] {
                            (service::str_field(kind, "title"))
                        }
                    }
                }
                select .form-input name="source" {
                    option value="" { "All sources" }
                    @for source in ["public_form", "admin", "api", "ai"] {
                        option value=(source) selected[source == msg.query("source")] { (source) }
                    }
                }
                input .form-input .ticket-filter-assignee name="assignee_id" value=(msg.query("assignee_id"))
                    placeholder="Assignee ID" maxlength="160";
            }
            div .ticket-filter-actions {
                button .btn .btn--secondary type="submit" { "Filter" }
                a .btn .btn--ghost href="/b/tickets/admin/types" { "Manage types" }
                a .btn .btn--ghost href="/b/tickets/admin/settings" { "Settings" }
            }
        }
        div .table-container {
            table .table .tickets-table {
                thead { tr {
                    th { "Reference" } th .ticket-col-type { "Type" } th { "Subject" }
                    th { "Priority" } th { "Status" } th .ticket-col-source { "Source" }
                    th .ticket-col-age { "Age" }
                } }
                tbody {
                    @if tickets.records.is_empty() {
                        tr { td colspan="7" { "No tickets match these filters." } }
                    }
                    @for ticket in &tickets.records {
                        tr {
                            td { a href={"/b/tickets/admin/tickets/" (ticket.id)} {
                                (service::str_field(ticket, "reference"))
                            } }
                            td .ticket-col-type { (service::str_field(ticket, "type_title_snapshot")) }
                            td { a href={"/b/tickets/admin/tickets/" (ticket.id)} {
                                (service::str_field(ticket, "subject"))
                            } }
                            td { span .badge { (service::str_field(ticket, "priority")) } }
                            td { span .badge { (service::str_field(ticket, "status")) } }
                            td .ticket-col-source { (service::str_field(ticket, "source")) }
                            td .ticket-col-age { (age(service::str_field(ticket, "created_at"))) }
                        }
                    }
                }
            }
        }
        @if let Some(per_page) = std::num::NonZeroU32::new(page_size.min(100) as u32) {
            (components::pagination(
                tickets.page as u32,
                per_page,
                tickets.total_count as u32,
                &pagination_base,
            ))
        }
        details .mt-6 {
            summary { "Create internal ticket" }
            form data-json-form data-endpoint="/b/tickets/api/admin/tickets"
                .ticket-form-grid .ticket-form-grid--create {
                input type="hidden" name="source" value="admin";
                label { "Type" select .form-input name="type_id" required {
                    @for kind in &types {
                        @if service::bool_field(kind, "active") {
                            option value=(kind.id) { (service::str_field(kind, "title")) }
                        }
                    }
                } }
                label { "Subject" input .form-input name="subject" minlength="5" maxlength="160" required; }
                label { "Description" textarea .form-input name="description" minlength="20" maxlength="4000" required {} }
                label { "Priority" select .form-input name="priority" {
                    option value="" { "Type default" }
                    @for priority in ["low", "normal", "high", "urgent"] {
                        option value=(priority) { (priority) }
                    }
                } }
                button .btn .btn--primary type="submit" { "Create ticket" }
            }
        }
        script { (PreEscaped(ADMIN_FORM_JS)) }
    };
    shell(ctx, msg, "Tickets", content).await
}

pub async fn detail(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = msg.var("id");
    let detail = match service::detail(ctx, id).await {
        Ok(detail) => detail,
        Err(error) => return crud::db_error(error, "Ticket not found", "Could not load ticket"),
    };
    let ticket = &detail.ticket;
    let report = &detail.untrusted_report;
    let current_duplicate = service::nullable_str_field(ticket, "duplicate_of").unwrap_or("");
    // A ticket whose type row is gone has no escalation; a read that failed
    // does not say so, and fails the page rather than showing "none".
    let ticket_type = match repo::get_type(ctx, service::str_field(ticket, "type_id")).await {
        Ok(row) => Some(row),
        Err(error) if error.code == wafer_run::ErrorCode::NotFound => None,
        Err(error) => return crud::db_error_page(msg, error, "Could not load ticket type"),
    };
    let escalation = ticket_type
        .as_ref()
        .map(|row| service::str_field(row, "escalation_kind"))
        .unwrap_or("none");
    let endpoint = format!("/b/tickets/api/admin/tickets/{id}");
    let note_endpoint = format!("{endpoint}/notes");
    let content = html! {
        style {
            (PreEscaped(r#"
                @media (max-width: 720px) {
                    .ticket-detail-grid {
                        grid-template-columns: minmax(0, 1fr) !important;
                    }
                    .ticket-note-form {
                        align-items: stretch;
                        flex-direction: column;
                    }
                }
                .ticket-analysis-meta {
                    display: grid;
                    grid-template-columns: max-content minmax(0, 1fr);
                    gap: .25rem .75rem;
                }
                .ticket-analysis-meta dt { font-weight: 600; }
                .ticket-analysis-meta dd { margin: 0; min-width: 0; }
                .ticket-analysis-actions {
                    overflow-x: auto;
                    white-space: pre-wrap;
                    overflow-wrap: anywhere;
                }
            "#))
        }
        (components::page_header(
            service::str_field(ticket, "reference"),
            Some(report.subject.as_str()),
            Some(html! { a .btn .btn--ghost href="/b/tickets/admin/tickets" { "Back to inbox" } }),
        ))
        @if escalation != "none" {
            div .alert .alert--warning .mb-4 {
                span aria-hidden="true" { (ui::icons::triangle_alert()) }
                div {
                    strong { "Human review required: " } (escalation)
                    ". Do not automatically contact the reporter or change content."
                }
            }
        }
        div .ticket-detail-grid {
            section .card .p-4 {
                h2 { "Original report" }
                dl {
                    dt { "Type" } dd { (service::str_field(ticket, "type_title_snapshot")) }
                    dt { "Subject" } dd { (&report.subject) }
                    dt { "Description" } dd .pre-wrap {
                        (&report.description)
                    }
                    @if !report.source_path.is_empty() {
                        dt { "Page" } dd { a href=(&report.source_path) {
                            (&report.source_path)
                        } }
                    }
                    @if !report.evidence_url.is_empty() {
                        dt { "Evidence" } dd { a href=(&report.evidence_url)
                            target="_blank" rel="noopener noreferrer" { "Open evidence URL" } }
                    }
                    @if !report.reporter_email.is_empty() {
                        dt { "Reporter email" } dd { (&report.reporter_email) }
                        dt { "Reply permitted" } dd {
                            @if report.reporter_wants_reply { "yes" } @else { "no" }
                        }
                    }
                }
            }
            aside .card .p-4 {
                h2 { "Workflow" }
                form data-json-form data-ticket-workflow data-current-status=(service::str_field(ticket, "status"))
                    data-endpoint=(endpoint) data-method="PATCH"
                    .ticket-form-grid {
                    label { "Status" select #ticket-workflow-status .form-input name="status" {
                        option value="" { "Keep current" }
                        @for status in ["new", "triaged", "investigating", "resolved", "rejected", "spam", "duplicate"] {
                            option value=(status) { (status) }
                        }
                    } }
                    label { "Priority" select .form-input name="priority" {
                        option value="" { "Keep current" }
                        @for priority in ["low", "normal", "high", "urgent"] {
                            option value=(priority) { (priority) }
                        }
                    } }
                    label { "Assignee ID" input .form-input name="assignee_id"
                        value=(service::str_field(ticket, "assignee_id")) data-keep-empty="true"; }
                    @if !current_duplicate.is_empty() {
                        p .text-muted {
                            "Current duplicate target: "
                            a href={"/b/tickets/admin/tickets/" (current_duplicate)} {
                                code { (current_duplicate) }
                            }
                        }
                    }
                    label {
                        "Duplicate ticket ID"
                        input #ticket-duplicate-target .form-input name="duplicate_of"
                            value=(current_duplicate) maxlength="160";
                        small .text-muted {
                            "Required only when marking this ticket duplicate. Copy the ID from the target ticket URL."
                        }
                    }
                    label { "Reason" textarea .form-input name="reason" maxlength="4000" {} }
                    label { input type="checkbox" name="legal_hold"
                        checked[service::bool_field(ticket, "legal_hold")]; " Legal hold" }
                    button .btn .btn--primary type="submit" { "Update workflow" }
                }
            }
        }
        section .mt-5 {
            h2 { "Internal note" }
            form .ticket-note-form data-json-form data-endpoint=(note_endpoint) {
                textarea .form-input name="note" maxlength="4000" required {}
                button .btn .btn--secondary type="submit" { "Add note" }
            }
        }
        section .mt-5 {
            h2 { "Timeline" }
            @if detail.events_truncated { p .alert .alert--warning { span aria-hidden="true" { (ui::icons::triangle_alert()) } "Older events are not shown." } }
            @for event in &detail.events {
                article .card .p-3 .mb-2 {
                    strong { (service::str_field(event, "event_type")) }
                    " · " (service::str_field(event, "actor_type"))
                    " · " (short_date(service::str_field(event, "created_at")))
                    @if !service::str_field(event, "body").is_empty() {
                        p .pre-wrap { (service::str_field(event, "body")) }
                    }
                }
            }
        }
        section .mt-5 {
            h2 { "Analyses" }
            @if detail.analyses_truncated { p .alert .alert--warning { span aria-hidden="true" { (ui::icons::triangle_alert()) } "Older analyses are not shown." } }
            @if detail.analyses.is_empty() { p .text-muted { "No analysis has been attached." } }
            @for analysis in &detail.analyses {
                article .card .p-3 .mb-2 {
                    h3 { (service::str_field(analysis, "source")) }
                    dl .ticket-analysis-meta {
                        dt { "Analysis ID" } dd { code { (&analysis.id) } }
                        @if !service::str_field(analysis, "model").is_empty() {
                            dt { "Model" } dd { (service::str_field(analysis, "model")) }
                        }
                        @if !service::str_field(analysis, "prompt_version").is_empty() {
                            dt { "Prompt version" } dd {
                                (service::str_field(analysis, "prompt_version"))
                            }
                        }
                        @if !service::str_field(analysis, "suggested_type_id").is_empty() {
                            dt { "Suggested type" } dd {
                                code { (service::str_field(analysis, "suggested_type_id")) }
                            }
                        }
                        @if !service::str_field(analysis, "suggested_priority").is_empty() {
                            dt { "Suggested priority" } dd {
                                (service::str_field(analysis, "suggested_priority"))
                            }
                        }
                        dt { "Confidence" } dd { (scalar_field(analysis, "confidence")) }
                        dt { "Created" } dd {
                            (short_date(service::str_field(analysis, "created_at")))
                        }
                    }
                    h4 { "Advisory summary" }
                    p .pre-wrap { (service::str_field(analysis, "summary")) }
                    h4 { "Suggested actions" }
                    pre .ticket-analysis-actions {
                        (pretty_json_field(analysis, "suggested_actions_json"))
                    }
                }
            }
        }
        script { (PreEscaped(ADMIN_FORM_JS)) }
        script {
            (PreEscaped(r#"(() => {
                const form = document.querySelector('form[data-ticket-workflow]');
                if (!form) return;
                const status = document.getElementById('ticket-workflow-status');
                const duplicate = document.getElementById('ticket-duplicate-target');
                const update = () => {
                    const effectiveStatus = status.value || form.dataset.currentStatus;
                    duplicate.required = effectiveStatus === 'duplicate';
                };
                status.addEventListener('change', update);
                update();
            })();"#))
        }
    };
    shell(ctx, msg, service::str_field(ticket, "reference"), content).await
}

pub async fn types(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let types = match repo::list_types(ctx, false, 100, 0).await {
        Ok(rows) => rows.records,
        Err(error) => return crud::db_error_page(msg, error, "Could not load ticket types"),
    };
    let content = html! {
        (components::page_header(
            "Ticket types",
            Some("Configure public report categories and internal work types"),
            Some(html! { a .btn .btn--ghost href="/b/tickets/admin/tickets" { "Back to inbox" } }),
        ))
        details {
            summary { "Create type" }
            form data-json-form data-endpoint="/b/tickets/api/admin/types"
                .ticket-form-grid .ticket-form-grid--type-create {
                label { "Immutable key" input .form-input name="key" pattern="[a-z0-9][a-z0-9_-]{0,46}[a-z0-9]" required; }
                label { "Title" input .form-input name="title" minlength="2" maxlength="80" required; }
                label { "Description" textarea .form-input name="description" maxlength="500" data-keep-empty="true" {} }
                label { "Guidance" textarea .form-input name="guidance" maxlength="1000" data-keep-empty="true" {} }
                label { "Default priority" select .form-input name="default_priority" {
                    @for value in ["low", "normal", "high", "urgent"] {
                        option value=(value) selected[value == "normal"] { (value) }
                    }
                } }
                label { "Escalation" select .form-input name="escalation_kind" {
                    @for value in ["none", "legal", "privacy", "safety"] {
                        option value=(value) { (value) }
                    }
                } }
                label { input type="checkbox" name="active" checked; " Active" }
                label { input type="checkbox" name="public_visible"; " Visible on public form" }
                label { input type="checkbox" name="requires_contact"; " Requires contact email" }
                label { input type="checkbox" name="requests_evidence"; " Requests evidence" }
                label { "Sort order" input .form-input type="number" name="sort_order" value="0"; }
                button .btn .btn--primary type="submit" { "Create type" }
            }
        }
        @for kind in &types {
            @let endpoint = format!("/b/tickets/api/admin/types/{}", kind.id);
            details .card .p-4 .mt-3 {
                summary {
                    strong { (service::str_field(kind, "title")) }
                    " · " code { (service::str_field(kind, "key")) }
                    @if service::bool_field(kind, "active") { span .badge { "active" } }
                    @if service::bool_field(kind, "public_visible") { span .badge { "public" } }
                }
                form data-json-form data-endpoint=(endpoint) data-method="PATCH"
                    .ticket-form-grid .ticket-form-grid--type-edit {
                    input type="hidden" name="key" value=(service::str_field(kind, "key"));
                    label { "Title" input .form-input name="title" value=(service::str_field(kind, "title")) required; }
                    label { "Description" textarea .form-input name="description" data-keep-empty="true" {
                        (service::str_field(kind, "description"))
                    } }
                    label { "Guidance" textarea .form-input name="guidance" data-keep-empty="true" {
                        (service::str_field(kind, "guidance"))
                    } }
                    label { "Default priority" select .form-input name="default_priority" {
                        @for value in ["low", "normal", "high", "urgent"] {
                            option value=(value)
                                selected[value == service::str_field(kind, "default_priority")] {
                                (value)
                            }
                        }
                    } }
                    label { "Escalation" select .form-input name="escalation_kind" {
                        @for value in ["none", "legal", "privacy", "safety"] {
                            option value=(value)
                                selected[value == service::str_field(kind, "escalation_kind")] {
                                (value)
                            }
                        }
                    } }
                    label { input type="checkbox" name="active" checked[service::bool_field(kind, "active")]; " Active" }
                    label { input type="checkbox" name="public_visible" checked[service::bool_field(kind, "public_visible")]; " Public" }
                    label { input type="checkbox" name="requires_contact" checked[service::bool_field(kind, "requires_contact")]; " Requires contact" }
                    label { input type="checkbox" name="requests_evidence" checked[service::bool_field(kind, "requests_evidence")]; " Requests evidence" }
                    label { "Sort order" input .form-input type="number" name="sort_order"
                        value=(number_field(kind, "sort_order")); }
                    button .btn .btn--secondary type="submit" { "Save type" }
                }
            }
        }
        script { (PreEscaped(ADMIN_FORM_JS)) }
    };
    shell(ctx, msg, "Ticket types", content).await
}

pub async fn settings(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let vars = super::config::config_vars();
    let readiness = match SecurityReadiness::load(ctx).await {
        Ok(readiness) => readiness,
        Err(e) => return crud::db_error_page(msg, e, "ticket settings page: config read failed"),
    };
    let content = html! {
        (components::page_header(
            "Ticket settings",
            Some("Public reporting security and retention"),
            None,
        ))
        div .card .p-4 .mb-4 {
            h3 { "Security readiness" }
            @if readiness.ready {
                div .alert .alert--success {
                    span aria-hidden="true" { (ui::icons::check()) }
                    "Ready for public submissions."
                }
            } @else {
                div .alert .alert--warning {
                    span aria-hidden="true" { (ui::icons::triangle_alert()) }
                    div {
                        strong { "Not ready: " }
                        ul { @for reason in &readiness.reasons { li { (reason) } } }
                    }
                }
            }
            p {
                "Configuration changes use the central Variables screen so this block keeps "
                "the reviewed HTTP contract to one read-only settings route."
            }
            a .btn .btn--secondary href="/b/admin/settings/variables" { "Manage variables" }
            a .btn .btn--ghost href="/b/tickets/submit" target="_blank" rel="noopener" {
                "Open public form"
            }
        }
        div .table-container { table .table {
            thead { tr { th { "Variable" } th { "Purpose" } } }
            tbody {
                @for var in &vars {
                    tr {
                        td { code { (var.key) } }
                        td {
                            strong { (var.name) }
                            @if !var.description.is_empty() {
                                p .text-muted { (var.description) }
                            }
                        }
                    }
                }
            }
        } }
        p .text-muted {
            "Default retention after closure: spam 30 days; rejected or duplicate 180 days; "
            "resolved 365 days. Open tickets and legal holds do not expire."
        }
    };
    shell(ctx, msg, "Ticket settings", content).await
}

pub async fn endpoints(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let content = html! {
        (components::page_header("Ticket endpoints", Some("All admin endpoints require admin authentication"), None))
        div .table-container { table .table {
            thead { tr { th { "Method" } th { "Path" } th { "Purpose" } } }
            tbody {
                @for (method, path, purpose) in super::ENDPOINT_REFERENCE {
                    tr { td { code { (method) } } td { code { (path) } } td { (purpose) } }
                }
            }
        } }
    };
    shell(ctx, msg, "Ticket endpoints", content).await
}

async fn shell(ctx: &dyn Context, msg: &Message, title: &str, content: Markup) -> OutputStream {
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple(title, ui::NavKind::Admin, "Tickets"),
        content,
    )
    .await
}

fn option(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn short_date(value: &str) -> &str {
    value.get(..10).unwrap_or(value)
}

fn age(value: &str) -> String {
    let Ok(created) = chrono::DateTime::parse_from_rfc3339(value) else {
        return short_date(value).to_string();
    };
    let seconds = (chrono::Utc::now() - created.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    if seconds >= 86_400 {
        format!("{}d", seconds / 86_400)
    } else if seconds >= 3_600 {
        format!("{}h", seconds / 3_600)
    } else if seconds >= 60 {
        format!("{}m", seconds / 60)
    } else {
        "now".into()
    }
}

fn inbox_pagination_base(msg: &Message) -> String {
    let query = [
        "status",
        "priority",
        "type_id",
        "source",
        "assignee_id",
        "page_size",
    ]
    .into_iter()
    .filter_map(|key| {
        let value = msg.query(key);
        (!value.is_empty()).then(|| format!("{key}={}", crate::util::urlencode(value)))
    })
    .collect::<Vec<_>>()
    .join("&");
    if query.is_empty() {
        "/b/tickets/admin/tickets".into()
    } else {
        format!("/b/tickets/admin/tickets?{query}")
    }
}

fn number_field(record: &db::Record, name: &str) -> i64 {
    record
        .data
        .get(name)
        .and_then(serde_json::Value::as_i64)
        .or_else(|| {
            record
                .data
                .get(name)
                .and_then(serde_json::Value::as_str)
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0)
}

fn scalar_field(record: &db::Record, name: &str) -> String {
    record.data.get(name).map_or_else(String::new, |value| {
        value
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string())
    })
}

fn pretty_json_field(record: &db::Record, name: &str) -> String {
    let Some(value) = record.data.get(name) else {
        return String::new();
    };
    let parsed = match value {
        serde_json::Value::String(value) => {
            serde_json::from_str(value).unwrap_or_else(|_| serde_json::json!(value))
        }
        value => value.clone(),
    };
    serde_json::to_string_pretty(&parsed).unwrap_or_default()
}

#[cfg(test)]
mod denial_tests {
    use super::*;
    use crate::{
        endpoint_match,
        test_support::{admin_msg, output_http_status, TestContext},
    };

    /// The SSR detail page reads through the same service as the JSON
    /// handler, and mapped everything but `NotFound` onto `err_internal`.
    #[tokio::test]
    async fn a_denied_ticket_detail_page_is_403_not_500() {
        let ctx = TestContext::with_tickets()
            .await
            .running_as("test/ungranted");
        let mut msg = admin_msg("retrieve", "/b/tickets/admin/tickets/any-id");
        endpoint_match::dispatch(&mut msg, crate::blocks::tickets::ROUTES);
        assert_eq!(output_http_status(detail(&ctx, &msg).await).await, 403);
    }

    /// The inbox and the ticket-types page each render from one list read. A
    /// refused read is the styled 403 page, with none of the denial's own
    /// text, through the block's own dispatch.
    #[tokio::test]
    async fn refused_list_pages_are_the_403_page() {
        use wafer_block::ServiceOp;
        use wafer_run::{Block, ErrorCode, InputStream, WaferError};

        use crate::{blocks::tickets::TicketsBlock, test_support::FailingDbOpContext};

        let ctx = TestContext::with_tickets().await;
        let mut misses = Vec::new();
        for (table, path) in [
            (repo::TICKETS, "/b/tickets/admin/tickets"),
            (repo::TYPES, "/b/tickets/admin/types"),
        ] {
            let failing = FailingDbOpContext::failing_with(
                ctx.clone(),
                ServiceOp::DATABASE_OPS
                    .iter()
                    .map(|op| (*op, table))
                    .collect(),
                WaferError::new(
                    ErrorCode::PermissionDenied,
                    "WRAP: impresspress/tickets holds no grant on this table",
                ),
            );
            let mut msg = admin_msg("retrieve", path);
            msg.set_meta("http.header.accept", "text/html");
            let out = TicketsBlock::new()
                .handle(&failing, msg, InputStream::empty())
                .await;
            let parts = wafer_block::http_codec::collect_http_response(out).await;
            let html = String::from_utf8_lossy(&parts.body);
            if parts.status != 403 || !html.contains("Go home") || html.contains("holds no grant") {
                misses.push(format!("{path}: {} {html}", parts.status));
            }
        }
        assert!(
            misses.is_empty(),
            "expected the 403 page at every site:\n{}",
            misses.join("\n")
        );
    }
}
