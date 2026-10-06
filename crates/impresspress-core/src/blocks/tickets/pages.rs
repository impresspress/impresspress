//! Server-rendered administration pages for tickets and ticket types.
//!
//! Every write these pages make goes through an htmx form posted to one of
//! the block's own admin routes (`forms`), which answers with the region it
//! changed and a toast — never `fetch` plus a page reload.

use maud::{html, Markup};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ConfigVar, Message, OutputStream, WaferError};

use super::{
    config::SecurityReadiness,
    models::{EscalationKind, Priority, TicketSource, TicketStatus},
    repo, service,
};
use crate::{
    blocks::admin::masked_config::MaskedValue,
    endpoint_match,
    ui::{
        self,
        components::{self, BadgeVariant, CalloutTone, DataTable, Modal, TableCol, TableRow},
        icons,
        shell::Crumb,
        templates::{self, DetailHero, DetailMeta},
    },
};

/// The inbox, and the prefix of every ticket's own page.
pub(super) const INBOX: &str = "/b/tickets/admin/tickets";
/// The ticket types page, and the prefix of every type's update route.
pub(super) const TYPES: &str = "/b/tickets/admin/types";
/// The "New ticket" modal on the inbox.
const CREATE_TICKET_MODAL: &str = "create-ticket";
/// The "New ticket type" modal on the types page.
pub(super) const CREATE_TYPE_MODAL: &str = "create-type";
/// The element a ticket's workflow and note forms swap with the re-rendered
/// ticket.
pub(super) const DETAIL_REGION: &str = "ticket-detail";
/// The element the type forms swap with the re-rendered type list.
pub(super) const TYPES_REGION: &str = "ticket-types";
/// The inbox's filter parameters, in the order the pagination links carry
/// them.
const FILTER_KEYS: [&str; 5] = ["status", "priority", "type_id", "source", "assignee_id"];
/// What every submit button carries: htmx disables it while its request is
/// in flight, so a slow answer cannot be submitted twice.
const DISABLE_WHILE_SENDING: &str = "find button[type='submit']";

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
            Err(error) => {
                return error_page(ctx, msg, Section::Inbox, error, "Could not load tickets").await
            }
        };
    // The type filter and the New ticket form are drawn from this list, so an
    // empty one would say the deployment has no ticket types; a failed read
    // fails the page instead.
    let types = match repo::list_types(ctx, false, 100, 0).await {
        Ok(rows) => rows.records,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                Section::Inbox,
                error,
                "Could not load ticket types",
            )
            .await
        }
    };
    let filtered = FILTER_KEYS.iter().any(|key| !msg.query(key).is_empty());
    let pagination = std::num::NonZeroU32::new(page_size.min(100) as u32).map(|per_page| {
        components::pagination(
            tickets.page as u32,
            per_page,
            tickets.total_count as u32,
            &inbox_pagination_base(msg),
        )
    });
    let content = html! {
        (templates::list_page(
            Some(filter_form(msg, &types, filtered)),
            ticket_table(&tickets.records, filtered),
            pagination,
        ))
        (create_ticket_modal(&types))
    };
    shell(
        ctx,
        msg,
        ui::Shell::admin("Tickets", "Tickets")
            .subtitle("Public reports and internal work items")
            .actions(vec![new_ticket_button()]),
        Section::Inbox,
        content,
    )
    .await
}

pub async fn detail(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let view = match TicketPage::load(ctx, msg.var("id")).await {
        Ok(view) => view,
        Err(error) if error.code == wafer_run::ErrorCode::NotFound => {
            return ui::not_found_response(msg)
        }
        Err(error) => {
            return error_page(ctx, msg, Section::Inbox, error, "Could not load ticket").await
        }
    };
    let reference = view.reference().to_string();
    shell(
        ctx,
        msg,
        ui::Shell::admin(&reference, &reference).trail(vec![
            Crumb {
                label: "Tickets",
                href: Some(INBOX),
            },
            Crumb {
                label: &reference,
                href: None,
            },
        ]),
        Section::Inbox,
        detail_region(&view),
    )
    .await
}

pub async fn types(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let types = match repo::list_types(ctx, false, 100, 0).await {
        Ok(rows) => rows.records,
        Err(error) => {
            return error_page(
                ctx,
                msg,
                Section::Types,
                error,
                "Could not load ticket types",
            )
            .await
        }
    };
    let content = html! {
        (types_region(&types))
        (Modal::new(CREATE_TYPE_MODAL, "New ticket type").render(html! {
            form hx-post=(TYPES) hx-target={ "#" (TYPES_REGION) } hx-swap="outerHTML"
                hx-disabled-elt=(DISABLE_WHILE_SENDING) data-reset-on-success {
                (type_fields("type-new", None))
                (components::modal_footer(html! {
                    (components::modal_cancel())
                    button .btn .btn--primary .btn--block type="submit" { "Create type" }
                }))
            }
        }))
    };
    shell(
        ctx,
        msg,
        ui::Shell::admin("Ticket types", "Tickets")
            .subtitle("The categories every ticket is filed under")
            .actions(vec![new_type_button()]),
        Section::Types,
        content,
    )
    .await
}

pub async fn settings(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let readiness = match SecurityReadiness::load(ctx, msg).await {
        Ok(readiness) => readiness,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Section::Settings,
                e,
                "ticket settings page: config read failed",
            )
            .await
        }
    };
    let vars = super::config::config_vars();
    // Masked by the admin block, which owns the variables table and the
    // rule (the stored sensitive flag included) for what may be shown.
    let current = match crate::blocks::admin::masked_config::read_own(ctx).await {
        Ok(values) => values,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                Section::Settings,
                e,
                "ticket settings page: masked config read failed",
            )
            .await
        }
    };
    let public_form = html! {
        a .btn .btn--secondary href="/b/tickets/submit" target="_blank" rel="noopener" {
            "Open the public form"
        }
    };
    let rows = vars
        .iter()
        .map(|var| {
            let value = current.iter().find(|value| value.key == var.key);
            TableRow::new(vec![
                html! {
                    span .data-table__title { (var.name) }
                    code .ticket-setting-key { (components::breakable_id(&var.key)) }
                },
                setting_value(var, value),
                html! { (var.description) },
            ])
        })
        .collect();
    let content = html! {
        div .ticket-page {
            @if readiness.ready {
                (components::callout(
                    CalloutTone::Info,
                    "Public reporting is on",
                    html! { p { "The public form accepts reports. Every check below passes." } },
                    Some(public_form),
                ))
            } @else {
                (components::callout(
                    CalloutTone::Warning,
                    "Public reporting is off",
                    html! {
                        p { "The public form accepts reports only once all of these are fixed:" }
                        ul .ticket-reasons { @for reason in &readiness.reasons { li { (humanize(reason)) } } }
                    },
                    Some(public_form),
                ))
            }
            section {
                (components::section_header(
                    "Configuration",
                    Some(html! {
                        a .btn .btn--secondary href="/b/admin/settings/variables" {
                            "Edit in Variables"
                        }
                    }),
                ))
                p .ticket-section__intro {
                    "These settings are changed on the Variables page. Secrets are never shown "
                    "here, only whether they are set. Open tickets and tickets under legal hold "
                    "are never deleted, whatever the retention settings say."
                }
                (DataTable::new(&SETTING_COLUMNS).rows(rows).render())
            }
        }
    };
    shell(
        ctx,
        msg,
        ui::Shell::admin("Ticket settings", "Tickets").subtitle("Public reporting and retention"),
        Section::Settings,
        content,
    )
    .await
}

pub async fn endpoints(ctx: &dyn Context, msg: &Message) -> OutputStream {
    // A row that answers JSON publishes its response schema; every other row
    // is a page or a form action of these admin pages.
    let (api, pages): (Vec<_>, Vec<_>) = endpoint_match::declare(super::ROUTES)
        .into_iter()
        .partition(|endpoint| endpoint.output_schema.is_some());
    let content = html! {
        div .ticket-page {
            section {
                (components::section_header("JSON API", None))
                p .ticket-section__intro {
                    "For scripts and agents. Admin routes need an administrator's session or "
                    "API key."
                }
                (components::endpoint_table("JSON API endpoints", &api))
            }
            section {
                (components::section_header("Pages and form actions", None))
                p .ticket-section__intro {
                    "The public report form, and what these admin pages load and submit."
                }
                (components::endpoint_table("Page and form action endpoints", &pages))
            }
        }
    };
    shell(
        ctx,
        msg,
        ui::Shell::admin("Ticket endpoints", "Tickets")
            .subtitle("Every route the tickets block serves"),
        Section::Endpoints,
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// Inbox
// ---------------------------------------------------------------------------

const TICKET_COLUMNS: [TableCol<'static>; 6] = [
    TableCol::new("Ticket").primary(),
    TableCol::new("Status"),
    TableCol::new("Priority"),
    TableCol::new("Type").optional(),
    TableCol::new("Source"),
    TableCol::new("Created"),
];

fn new_ticket_button() -> Markup {
    html! {
        button .btn .btn--primary type="button" data-action="modal-open"
            data-modal-target=(CREATE_TICKET_MODAL) {
            (icons::plus()) "New ticket"
        }
    }
}

/// The inbox filters: one labelled control per filter, every option drawn
/// from the value set the list endpoint accepts.
fn filter_form(msg: &Message, types: &[db::Record], filtered: bool) -> Markup {
    let statuses: Vec<(&str, String)> = TicketStatus::ALL
        .iter()
        .map(|s| (s.as_str(), humanize(s.as_str())))
        .collect();
    let priorities: Vec<(&str, String)> = Priority::ALL
        .iter()
        .map(|p| (p.as_str(), humanize(p.as_str())))
        .collect();
    let kinds: Vec<(&str, String)> = types
        .iter()
        .map(|kind| {
            (
                kind.id.as_str(),
                service::str_field(kind, "title").to_string(),
            )
        })
        .collect();
    let sources: Vec<(&str, String)> = TicketSource::ALL
        .iter()
        .map(|s| (s.as_str(), source_label(s.as_str()).to_string()))
        .collect();
    let active = FILTER_KEYS
        .iter()
        .filter(|key| !msg.query(key).is_empty())
        .count();
    html! {
        // A disclosure below 720px, so a phone sees the tickets first; above
        // it the summary is hidden and the filters always show
        // (`.ticket-filters-disclosure` in card.css).
        details .ticket-filters-disclosure {
        summary .ticket-filters-disclosure__summary {
            "Filters" @if active > 0 { " (" (active) ")" }
        }
        form .ticket-filters method="get" action=(INBOX) {
            (filter_select("status", "Status", "All statuses", &statuses, msg))
            (filter_select("priority", "Priority", "All priorities", &priorities, msg))
            (filter_select("type_id", "Type", "All types", &kinds, msg))
            (filter_select("source", "Source", "All sources", &sources, msg))
            div .form-group {
                label .form-label for="ticket-filter-assignee_id" { "Assignee ID" }
                input #ticket-filter-assignee_id .form-input name="assignee_id"
                    value=(msg.query("assignee_id")) maxlength="160" autocomplete="off";
            }
            div .ticket-filters__actions {
                button .btn .btn--secondary type="submit" { "Apply filters" }
                @if filtered { a .btn .btn--ghost href=(INBOX) { "Clear" } }
            }
        }
        }
    }
}

fn filter_select(
    name: &str,
    label: &str,
    all: &str,
    options: &[(&str, String)],
    msg: &Message,
) -> Markup {
    let id = format!("ticket-filter-{name}");
    let selected = msg.query(name);
    html! {
        div .form-group {
            label .form-label for=(id) { (label) }
            select .form-select id=(id) name=(name) {
                option value="" { (all) }
                @for (value, text) in options {
                    option value=(value) selected[*value == selected] { (text) }
                }
            }
        }
    }
}

fn ticket_table(tickets: &[db::Record], filtered: bool) -> Markup {
    let rows = tickets
        .iter()
        .map(|ticket| {
            TableRow::new(vec![
                html! {
                    span .data-table__title { (service::str_field(ticket, "subject")) }
                    span .ticket-ref { (service::str_field(ticket, "reference")) }
                },
                status_badge(service::str_field(ticket, "status")),
                priority_badge(service::str_field(ticket, "priority")),
                html! { (service::str_field(ticket, "type_title_snapshot")) },
                html! { (source_label(service::str_field(ticket, "source"))) },
                components::timestamp(service::str_field(ticket, "created_at")),
            ])
        })
        .collect();
    let table = DataTable::new(&TICKET_COLUMNS).rows(rows).row_href(|i| {
        tickets
            .get(i)
            .map(|ticket| format!("{INBOX}/{}", ticket.id))
    });
    let table = if filtered {
        table.empty_state(
            "No tickets match these filters",
            "Try another status or priority, or clear the filters to see every ticket.",
            Some(html! { a .btn .btn--secondary href=(INBOX) { "Clear filters" } }),
        )
    } else {
        table.empty_state(
            "No tickets yet",
            "Reports sent through the public form and tickets you create appear here.",
            Some(new_ticket_button()),
        )
    };
    table.render()
}

fn create_ticket_modal(types: &[db::Record]) -> Markup {
    let active: Vec<&db::Record> = types
        .iter()
        .filter(|kind| service::bool_field(kind, "active"))
        .collect();
    components::modal(
        CREATE_TICKET_MODAL,
        "New internal ticket",
        html! {
            @if active.is_empty() {
                (components::callout(
                    CalloutTone::Info,
                    "No active ticket type",
                    html! { p { "Every ticket is filed under a type. Create or activate one first." } },
                    Some(html! { a .btn .btn--secondary href=(TYPES) { "Manage types" } }),
                ))
            } @else {
                // A created ticket answers with a redirect to its own page,
                // so nothing on this page is swapped.
                form hx-post=(INBOX) hx-swap="none" hx-disabled-elt=(DISABLE_WHILE_SENDING) {
                    div .form-group {
                        label .form-label .required for="new-ticket-type" { "Type" }
                        select #new-ticket-type .form-select name="type_id" required {
                            @for kind in &active {
                                option value=(kind.id) { (service::str_field(kind, "title")) }
                            }
                        }
                    }
                    div .form-group {
                        label .form-label .required for="new-ticket-subject" { "Subject" }
                        input #new-ticket-subject .form-input name="subject" minlength="5"
                            maxlength="160" required aria-describedby="new-ticket-subject-hint";
                        p #new-ticket-subject-hint .form-hint { "One line, 5–160 characters." }
                    }
                    div .form-group {
                        label .form-label .required for="new-ticket-description" { "Description" }
                        textarea #new-ticket-description .form-textarea name="description" rows="5"
                            minlength="20" maxlength="4000" required
                            aria-describedby="new-ticket-description-hint" {}
                        p #new-ticket-description-hint .form-hint {
                            "At least 20 characters. Kept as the original report and cannot be "
                            "edited later."
                        }
                    }
                    div .form-group {
                        label .form-label for="new-ticket-priority" { "Priority" }
                        select #new-ticket-priority .form-select name="priority" {
                            option value="" { "The type's default" }
                            @for priority in Priority::ALL {
                                option value=(priority.as_str()) { (humanize(priority.as_str())) }
                            }
                        }
                    }
                    (components::modal_footer(html! {
                        (components::modal_cancel())
                        button .btn .btn--primary .btn--block type="submit" { "Create ticket" }
                    }))
                }
            }
        },
    )
}

// ---------------------------------------------------------------------------
// Ticket detail
// ---------------------------------------------------------------------------

/// Everything the ticket page draws, read once.
pub(super) struct TicketPage {
    detail: service::TicketDetail,
    /// The ticket type's review track; `None` when the type row is gone.
    escalation: Option<String>,
}

impl TicketPage {
    /// The ticket `id`, its timeline and analyses, and its type's escalation.
    /// A ticket whose type row is gone has no escalation; a type read that
    /// failed does not say so, and is returned rather than shown as "none".
    pub(super) async fn load(ctx: &dyn Context, id: &str) -> Result<Self, WaferError> {
        let detail = service::detail(ctx, id).await?;
        let escalation =
            match repo::get_type(ctx, service::str_field(&detail.ticket, "type_id")).await {
                Ok(row) => Some(service::str_field(&row, "escalation_kind").to_string()),
                Err(error) if error.code == wafer_run::ErrorCode::NotFound => None,
                Err(error) => return Err(error),
            };
        Ok(Self { detail, escalation })
    }

    fn reference(&self) -> &str {
        service::str_field(&self.detail.ticket, "reference")
    }
}

/// The whole ticket page body: what the workflow and note forms swap.
pub(super) fn detail_region(view: &TicketPage) -> Markup {
    let ticket = &view.detail.ticket;
    let report = &view.detail.untrusted_report;
    let id = ticket.id.as_str();
    let status = service::str_field(ticket, "status");
    let legal_hold = service::bool_field(ticket, "legal_hold");
    let escalated = view
        .escalation
        .as_deref()
        .filter(|kind| *kind != EscalationKind::None.as_str());

    let mut badges = vec![
        status_badge(status),
        priority_badge(service::str_field(ticket, "priority")),
    ];
    if legal_hold {
        badges.push(components::badge(BadgeVariant::Warning, "Legal hold"));
    }

    let mut sections = Vec::new();
    if let Some(kind) = escalated {
        sections.push(html! {
            div .ticket-section {
                (components::callout(
                    CalloutTone::Warning,
                    &format!("{} review required", humanize(kind)),
                    html! {
                        p {
                            "A person must review this ticket. Do not contact the reporter or "
                            "change content automatically."
                        }
                    },
                    None,
                ))
            }
        });
    }
    sections.push(html! {
        section .ticket-section {
            (components::section_header("Original report", None))
            p .ticket-report .pre-wrap { (&report.description) }
        }
    });
    sections.push(workflow_section(id, ticket));
    sections.push(activity_section(id, &view.detail));
    sections.push(analyses_section(&view.detail));

    let assignee = service::str_field(ticket, "assignee_id");
    let duplicate_of = service::nullable_str_field(ticket, "duplicate_of").unwrap_or("");
    let expires_at = service::nullable_str_field(ticket, "expires_at").unwrap_or("");
    let mut meta = vec![
        DetailMeta {
            key: "Type",
            value: html! { (service::str_field(ticket, "type_title_snapshot")) },
        },
        DetailMeta {
            key: "Source",
            value: html! { (source_label(service::str_field(ticket, "source"))) },
        },
        DetailMeta {
            key: "Assignee",
            value: html! {
                @if assignee.is_empty() { span .text-muted { "Unassigned" } }
                @else { code { (components::breakable_id(assignee)) } }
            },
        },
        DetailMeta {
            key: "Created",
            value: components::timestamp(service::str_field(ticket, "created_at")),
        },
        DetailMeta {
            key: "Updated",
            value: components::timestamp(service::str_field(ticket, "updated_at")),
        },
        DetailMeta {
            key: "Deleted after",
            value: html! {
                @if !expires_at.is_empty() { (components::timestamp(expires_at)) }
                @else if legal_hold { "Never (legal hold)" }
                @else { "Kept while open" }
            },
        },
    ];
    if !duplicate_of.is_empty() {
        meta.push(DetailMeta {
            key: "Duplicate of",
            value: html! {
                a .ticket-link href={ (INBOX) "/" (duplicate_of) } { code { (components::breakable_id(duplicate_of)) } }
            },
        });
    }
    if !report.source_path.is_empty() {
        meta.push(DetailMeta {
            key: "Page",
            value: html! { a .ticket-link href=(&report.source_path) { (components::breakable_id(&report.source_path)) } },
        });
    }
    if !report.subject_type.is_empty() || !report.subject_id.is_empty() {
        meta.push(DetailMeta {
            key: "About",
            value: html! {
                (report.subject_type) " "
                code { (components::breakable_id(&report.subject_id)) }
            },
        });
    }
    if !report.evidence_url.is_empty() {
        meta.push(DetailMeta {
            key: "Evidence",
            value: html! {
                a .ticket-link href=(&report.evidence_url) target="_blank" rel="noopener noreferrer" {
                    "Open evidence link"
                }
            },
        });
    }
    if !report.reporter_email.is_empty() {
        meta.push(DetailMeta {
            key: "Reporter",
            value: html! { (components::breakable_id(&report.reporter_email)) },
        });
        meta.push(DetailMeta {
            key: "Reply allowed",
            value: html! { @if report.reporter_wants_reply { "Yes" } @else { "No" } },
        });
    }
    meta.push(DetailMeta {
        key: "Ticket ID",
        value: html! { code { (components::breakable_id(id)) } },
    });

    html! {
        div .ticket-detail id=(DETAIL_REGION) {
            (templates::detail_page(
                DetailHero {
                    icon: None,
                    title: &report.subject,
                    subtitle: None,
                    badges,
                    action_menu: None,
                },
                sections,
                meta,
            ))
        }
    }
}

fn workflow_section(id: &str, ticket: &db::Record) -> Markup {
    let current = service::str_field(ticket, "status");
    let current_status = current.parse::<TicketStatus>().ok();
    let reachable: Vec<TicketStatus> = TicketStatus::ALL
        .iter()
        .copied()
        .filter(|next| current_status.is_none_or(|from| from.can_transition_to(*next)))
        .collect();
    let priority = service::str_field(ticket, "priority");
    let duplicate_of = service::nullable_str_field(ticket, "duplicate_of").unwrap_or("");
    html! {
        section .ticket-section {
            (components::section_header("Workflow", None))
            form hx-patch={ (INBOX) "/" (id) } hx-target={ "#" (DETAIL_REGION) }
                hx-swap="outerHTML" hx-disabled-elt=(DISABLE_WHILE_SENDING) {
                div .ticket-workflow__grid {
                    div .form-group {
                        label .form-label for="workflow-status" { "Status" }
                        select #workflow-status .form-select name="status"
                            aria-describedby=[current_status
                                .filter(|s| !s.is_open())
                                .map(|_| "workflow-status-hint")] {
                            @for next in &reachable {
                                option value=(next.as_str()) selected[next.as_str() == current] {
                                    (humanize(next.as_str()))
                                }
                            }
                        }
                        @if current_status.is_some_and(|s| !s.is_open()) {
                            p #workflow-status-hint .form-hint {
                                "A closed ticket can only be reopened as Triaged."
                            }
                        }
                    }
                    div .form-group {
                        label .form-label for="workflow-priority" { "Priority" }
                        select #workflow-priority .form-select name="priority" {
                            @for option in Priority::ALL {
                                option value=(option.as_str()) selected[option.as_str() == priority] {
                                    (humanize(option.as_str()))
                                }
                            }
                        }
                    }
                    div .form-group {
                        label .form-label for="workflow-assignee" { "Assignee ID" }
                        input #workflow-assignee .form-input name="assignee_id" maxlength="160"
                            value=(service::str_field(ticket, "assignee_id")) autocomplete="off"
                            aria-describedby="workflow-assignee-hint";
                        p #workflow-assignee-hint .form-hint { "Leave empty to unassign." }
                    }
                    div .form-group {
                        label .form-label for="workflow-duplicate" { "Duplicate of" }
                        input #workflow-duplicate .form-input name="duplicate_of" maxlength="160"
                            value=(duplicate_of) autocomplete="off"
                            aria-describedby="workflow-duplicate-hint";
                        p #workflow-duplicate-hint .form-hint {
                            "Needed when the status is Duplicate: the Ticket ID of the original, "
                            "shown on its page."
                        }
                    }
                }
                div .form-group {
                    label .form-label for="workflow-reason" { "Reason" }
                    textarea #workflow-reason .form-textarea name="reason" rows="3" maxlength="4000"
                        aria-describedby="workflow-reason-hint" {}
                    p #workflow-reason-hint .form-hint {
                        "Needed when closing the ticket (resolved, rejected, spam or duplicate). "
                        "Recorded on the timeline."
                    }
                }
                label .form-checkbox {
                    input type="checkbox" name="legal_hold" value="true"
                        checked[service::bool_field(ticket, "legal_hold")];
                    "Legal hold: keep this ticket past its retention period"
                }
                div .ticket-form-actions {
                    button .btn .btn--primary type="submit" { "Save changes" }
                }
            }
        }
    }
}

fn activity_section(id: &str, detail: &service::TicketDetail) -> Markup {
    html! {
        section .ticket-section {
            (components::section_header("Activity", None))
            form hx-post={ (INBOX) "/" (id) "/notes" } hx-target={ "#" (DETAIL_REGION) }
                hx-swap="outerHTML" hx-disabled-elt=(DISABLE_WHILE_SENDING) {
                div .form-group {
                    label .form-label for="ticket-note" { "Internal note" }
                    textarea #ticket-note .form-textarea name="note" rows="3" maxlength="4000"
                        required aria-describedby="ticket-note-hint" {}
                    p #ticket-note-hint .form-hint { "Seen by administrators only." }
                }
                div .ticket-form-actions {
                    button .btn .btn--secondary type="submit" { "Add note" }
                }
            }
            @if detail.events_truncated {
                p .ticket-section__intro { "Only the 200 most recent events are shown." }
            }
            ol .ticket-timeline {
                @for event in &detail.events {
                    @let body = service::str_field(event, "body");
                    li .ticket-event {
                        div .ticket-event__head {
                            strong { (event_label(service::str_field(event, "event_type"))) }
                            span .ticket-event__actor {
                                "by " (humanize(service::str_field(event, "actor_type")))
                            }
                            (components::timestamp(service::str_field(event, "created_at")))
                        }
                        @if !body.is_empty() { p .ticket-event__body .pre-wrap { (body) } }
                    }
                }
            }
        }
    }
}

fn analyses_section(detail: &service::TicketDetail) -> Markup {
    html! {
        section .ticket-section {
            (components::section_header("Analyses", None))
            @if detail.analyses_truncated {
                p .ticket-section__intro { "Only the 100 most recent analyses are shown." }
            }
            @if detail.analyses.is_empty() {
                p .ticket-section__intro {
                    "No analysis is attached. An analysis is advisory: it never changes the ticket."
                }
            }
            @for analysis in &detail.analyses {
                @let field = |name: &str| service::str_field(analysis, name).to_string();
                article .ticket-analysis {
                    h3 .ticket-analysis__title { (field("source")) }
                    dl .ticket-analysis__meta {
                        @if !field("model").is_empty() { dt { "Model" } dd { (field("model")) } }
                        @if !field("prompt_version").is_empty() {
                            dt { "Prompt version" } dd { (field("prompt_version")) }
                        }
                        @if !field("suggested_type_id").is_empty() {
                            dt { "Suggested type" } dd { code { (field("suggested_type_id")) } }
                        }
                        @if !field("suggested_priority").is_empty() {
                            dt { "Suggested priority" } dd { (humanize(&field("suggested_priority"))) }
                        }
                        dt { "Confidence" } dd { (scalar_field(analysis, "confidence")) }
                        dt { "Created" } dd { (components::timestamp(&field("created_at"))) }
                        dt { "Analysis ID" } dd { code { (components::breakable_id(&analysis.id)) } }
                    }
                    h4 .ticket-analysis__heading { "Summary" }
                    p .pre-wrap { (field("summary")) }
                    h4 .ticket-analysis__heading { "Suggested actions" }
                    pre .ticket-analysis__actions {
                        (pretty_json_field(analysis, "suggested_actions_json"))
                    }
                }
            }
        }
    }
}

/// A timeline entry's heading: a status the ticket moved to, a note, or the
/// event type as written.
fn event_label(event_type: &str) -> String {
    match event_type.parse::<TicketStatus>() {
        Ok(status) => format!("Moved to {}", humanize(status.as_str())),
        Err(_) if event_type == "note" => "Internal note".into(),
        Err(_) => humanize(event_type),
    }
}

// ---------------------------------------------------------------------------
// Ticket types
// ---------------------------------------------------------------------------

const TYPE_COLUMNS: [TableCol<'static>; 7] = [
    TableCol::new("Type").primary(),
    TableCol::new("State"),
    TableCol::new("Public form"),
    TableCol::new("Default priority"),
    TableCol::new("Escalation"),
    TableCol::new("Order"),
    TableCol::new("Actions").actions(),
];

fn new_type_button() -> Markup {
    html! {
        button .btn .btn--primary type="button" data-action="modal-open"
            data-modal-target=(CREATE_TYPE_MODAL) {
            (icons::plus()) "New type"
        }
    }
}

/// The edit modal of the type `id`.
pub(super) fn edit_type_modal_id(id: &str) -> String {
    format!("edit-type-{id}")
}

/// The type list and each type's edit modal: what the type forms swap.
pub(super) fn types_region(types: &[db::Record]) -> Markup {
    let rows = types
        .iter()
        .map(|kind| {
            let title = service::str_field(kind, "title");
            let escalation = service::str_field(kind, "escalation_kind");
            TableRow::new(vec![
                html! {
                    span .data-table__title { (title) }
                    code .ticket-setting-key { (service::str_field(kind, "key")) }
                },
                if service::bool_field(kind, "active") {
                    components::badge(BadgeVariant::Success, "Active")
                } else {
                    components::badge(BadgeVariant::Secondary, "Inactive")
                },
                html! { @if service::bool_field(kind, "public_visible") { "Offered" } @else { "Internal only" } },
                priority_badge(service::str_field(kind, "default_priority")),
                html! {
                    @if escalation == EscalationKind::None.as_str() { "None" }
                    @else { (humanize(escalation)) " review" }
                },
                html! { span .ticket-number { (number_field(kind, "sort_order")) } },
                html! {
                    button .btn .btn--ghost .btn--icon type="button"
                        id={ "edit-type-open-" (kind.id) }
                        data-action="modal-open" data-modal-target=(edit_type_modal_id(&kind.id))
                        aria-label={ "Edit " (title) } {
                        (icons::edit())
                    }
                },
            ])
        })
        .collect();
    html! {
        div id=(TYPES_REGION) {
            (DataTable::new(&TYPE_COLUMNS)
                .rows(rows)
                .empty_state(
                    "No ticket types yet",
                    "Every ticket is filed under a type. Create one to start taking reports.",
                    Some(new_type_button()),
                )
                .render())
            @for kind in types {
                @let modal_id = edit_type_modal_id(&kind.id);
                (Modal::new(&modal_id, service::str_field(kind, "title")).render(html! {
                    form hx-patch={ (TYPES) "/" (kind.id) } hx-target={ "#" (TYPES_REGION) }
                        hx-swap="outerHTML" hx-disabled-elt=(DISABLE_WHILE_SENDING) {
                        (type_fields(&format!("type-{}", kind.id), Some(kind)))
                        (components::modal_footer(html! {
                            (components::modal_cancel())
                            button .btn .btn--primary .btn--block type="submit" { "Save type" }
                        }))
                    }
                }))
            }
        }
    }
}

/// The fields of the create form (`kind` is `None`) or of `kind`'s edit form.
/// `prefix` keeps the ids of the forms on one page apart.
fn type_fields(prefix: &str, kind: Option<&db::Record>) -> Markup {
    let value = |name| kind.map_or("", |kind| service::str_field(kind, name));
    let flag = |name, default| kind.map_or(default, |kind| service::bool_field(kind, name));
    let default_priority = kind.map_or(Priority::Normal.as_str(), |kind| {
        service::str_field(kind, "default_priority")
    });
    let escalation = kind.map_or(EscalationKind::None.as_str(), |kind| {
        service::str_field(kind, "escalation_kind")
    });
    let id = |field: &str| format!("{prefix}-{field}");
    html! {
        @match kind {
            None => div .form-group {
                label .form-label .required for=(id("key")) { "Key" }
                input .form-input id=(id("key")) name="key" required autocomplete="off"
                    pattern="[a-z0-9][a-z0-9_\\-]{0,46}[a-z0-9]" maxlength="48"
                    aria-describedby=(id("key-hint"));
                p .form-hint id=(id("key-hint")) {
                    "Lowercase letters, digits, - and _, 2–48 characters. Cannot be changed later."
                }
            },
            Some(kind) => p .ticket-type-key {
                "Key " code { (service::str_field(kind, "key")) } " · cannot be changed"
            },
        }
        div .form-group {
            label .form-label .required for=(id("title")) { "Title" }
            input .form-input id=(id("title")) name="title" minlength="2" maxlength="80" required
                value=(value("title"));
        }
        div .form-group {
            label .form-label for=(id("description")) { "Description" }
            textarea .form-textarea id=(id("description")) name="description" rows="2"
                maxlength="500" aria-describedby=(id("description-hint")) { (value("description")) }
            p .form-hint id=(id("description-hint")) { "What belongs under this type." }
        }
        div .form-group {
            label .form-label for=(id("guidance")) { "Guidance for reporters" }
            textarea .form-textarea id=(id("guidance")) name="guidance" rows="3" maxlength="1000"
                aria-describedby=(id("guidance-hint")) { (value("guidance")) }
            p .form-hint id=(id("guidance-hint")) { "Shown on the public form when this type is chosen." }
        }
        div .ticket-type-grid {
            div .form-group {
                label .form-label for=(id("default_priority")) { "Default priority" }
                select .form-select id=(id("default_priority")) name="default_priority" {
                    @for option in Priority::ALL {
                        option value=(option.as_str()) selected[option.as_str() == default_priority] {
                            (humanize(option.as_str()))
                        }
                    }
                }
            }
            div .form-group {
                label .form-label for=(id("escalation_kind")) { "Escalation" }
                select .form-select id=(id("escalation_kind")) name="escalation_kind"
                    aria-describedby=(id("escalation-hint")) {
                    @for option in EscalationKind::ALL {
                        option value=(option.as_str()) selected[option.as_str() == escalation] {
                            (humanize(option.as_str()))
                        }
                    }
                }
                p .form-hint id=(id("escalation-hint")) {
                    "Escalated tickets are marked for review by a person."
                }
            }
            div .form-group {
                label .form-label for=(id("sort_order")) { "Order" }
                input .form-input id=(id("sort_order")) type="number" name="sort_order"
                    min="-1000000" max="1000000" step="1"
                    value=(kind.map_or(0, |kind| number_field(kind, "sort_order")))
                    aria-describedby=(id("sort_order-hint"));
                p .form-hint id=(id("sort_order-hint")) { "Lower numbers are listed first." }
            }
        }
        fieldset .fieldset-reset .ticket-type-options {
            legend .form-label { "Options" }
            label .form-checkbox {
                input type="checkbox" name="active" value="true" checked[flag("active", true)];
                "Active: accepts new tickets"
            }
            label .form-checkbox {
                input type="checkbox" name="public_visible" value="true"
                    checked[flag("public_visible", false)];
                "Offered on the public report form"
            }
            label .form-checkbox {
                input type="checkbox" name="requires_contact" value="true"
                    checked[flag("requires_contact", false)];
                "Reporter must give an email address"
            }
            label .form-checkbox {
                input type="checkbox" name="requests_evidence" value="true"
                    checked[flag("requests_evidence", false)];
                "Ask the reporter for an evidence link"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

const SETTING_COLUMNS: [TableCol<'static>; 3] = [
    TableCol::new("Setting").primary(),
    TableCol::new("Current value"),
    TableCol::new("What it does"),
];

/// A setting's current value as the settings table shows it: a sensitive
/// one (as the admin block judged it) says only whether it is set, an unset
/// one says what applies instead.
fn setting_value(var: &ConfigVar, value: Option<&MaskedValue>) -> Markup {
    match value {
        Some(value) if value.sensitive && value.set => {
            components::badge(BadgeVariant::Success, "Set")
        }
        Some(value) if value.sensitive => components::badge(BadgeVariant::Secondary, "Not set"),
        Some(MaskedValue {
            value: Some(stored),
            ..
        }) => html! { code { (components::breakable_id(stored)) } },
        _ if var.default.is_empty() => html! { span .text-muted { "Not set" } },
        _ => html! {
            span .text-muted { "Default: " } code { (components::breakable_id(&var.default)) }
        },
    }
}

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

/// A ticket status as a badge: new work stands out, open work is neutral,
/// resolved is green, and every other closed state is quiet.
fn status_badge(status: &str) -> Markup {
    let variant = match status.parse::<TicketStatus>() {
        Ok(TicketStatus::New) => BadgeVariant::Warning,
        Ok(TicketStatus::Triaged | TicketStatus::Investigating) => BadgeVariant::Info,
        Ok(TicketStatus::Resolved) => BadgeVariant::Success,
        Ok(TicketStatus::Rejected | TicketStatus::Spam | TicketStatus::Duplicate) | Err(_) => {
            BadgeVariant::Secondary
        }
    };
    components::badge(variant, &humanize(status))
}

/// A priority as a badge: only urgent and high carry a colour.
fn priority_badge(priority: &str) -> Markup {
    let variant = match priority.parse::<Priority>() {
        Ok(Priority::Urgent) => BadgeVariant::Danger,
        Ok(Priority::High) => BadgeVariant::Warning,
        Ok(Priority::Normal | Priority::Low) | Err(_) => BadgeVariant::Secondary,
    };
    components::badge(variant, &humanize(priority))
}

/// Where a ticket came from, as a reader names it. A value outside the set
/// is shown as stored.
fn source_label(source: &str) -> &str {
    match source.parse::<TicketSource>() {
        Ok(TicketSource::PublicForm) => "Public form",
        Ok(TicketSource::Admin) => "Admin",
        Ok(TicketSource::Api) => "API",
        Ok(TicketSource::Ai) => "AI",
        Err(_) => source,
    }
}

/// A stored value as a reader sees it: `public_form` → `Public form`, and a
/// sentence starts with a capital.
fn humanize(value: &str) -> String {
    let spaced = value.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The block's admin sections — one link each above every tickets admin
/// page.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Inbox,
    Types,
    Settings,
    Endpoints,
}

fn sections(active: Section) -> Markup {
    let tab = |section: Section, href: &'static str, label: &'static str| components::Tab {
        active: section == active,
        href,
        label,
        icon: None,
    };
    components::subnav(
        "Tickets sections",
        vec![
            tab(Section::Inbox, INBOX, "Inbox"),
            tab(Section::Types, TYPES, "Types"),
            tab(Section::Settings, "/b/tickets/admin/settings", "Settings"),
            tab(
                Section::Endpoints,
                "/b/tickets/admin/endpoints",
                "Endpoints",
            ),
        ],
    )
}

/// A tickets page whose read failed: drawn in the shell under its section's
/// links, with a link back to the inbox.
async fn error_page(
    ctx: &dyn Context,
    msg: &Message,
    section: Section,
    error: WaferError,
    context: &str,
) -> OutputStream {
    ui::shell_error_page(
        ctx,
        msg,
        ui::Shell::admin("Tickets", "Tickets"),
        Some(sections(section)),
        ui::BackLink {
            label: "Back to the inbox",
            href: INBOX,
        },
        error,
        context,
    )
    .await
}

async fn shell(
    ctx: &dyn Context,
    msg: &Message,
    chrome: ui::Shell<'_>,
    section: Section,
    content: Markup,
) -> OutputStream {
    ui::shell_page(
        ctx,
        msg,
        chrome,
        ui::PageBody::from(content).with_subnav(sections(section)),
    )
    .await
}

fn option(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn inbox_pagination_base(msg: &Message) -> String {
    let query = FILTER_KEYS
        .into_iter()
        .chain(["page_size"])
        .filter_map(|key| {
            let value = msg.query(key);
            (!value.is_empty()).then(|| format!("{key}={}", crate::util::urlencode(value)))
        })
        .collect::<Vec<_>>()
        .join("&");
    if query.is_empty() {
        INBOX.into()
    } else {
        format!("{INBOX}?{query}")
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
            if parts.status != 403
                || !html.contains("status-page--in-shell")
                || html.contains("holds no grant")
            {
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

#[cfg(test)]
mod pagination_base_tests {
    use super::*;
    use crate::test_support::admin_msg;

    /// Filter values are query-encoded into the pagination base: an `&` in a
    /// value is `%26`, so it cannot split into a parameter of its own, and
    /// the parameters themselves join with a bare `&`.
    #[test]
    fn filter_values_are_encoded_into_the_pagination_base() {
        let mut msg = admin_msg("retrieve", "/b/tickets/admin/tickets");
        msg.set_meta("req.query.status", "new");
        msg.set_meta("req.query.assignee_id", "admin&one");
        msg.set_meta("req.query.page_size", "1");
        assert_eq!(
            inbox_pagination_base(&msg),
            "/b/tickets/admin/tickets?status=new&assignee_id=admin%26one&page_size=1"
        );
    }

    #[test]
    fn no_filters_is_the_bare_inbox_path() {
        let msg = admin_msg("retrieve", "/b/tickets/admin/tickets");
        assert_eq!(inbox_pagination_base(&msg), "/b/tickets/admin/tickets");
    }
}

/// What the admin pages render, through the block's own dispatch.
#[cfg(test)]
pub(super) mod render_tests {
    use wafer_run::{Block, InputStream};

    use super::*;
    use crate::{
        blocks::tickets::{
            models::{ActorType, CreateTicketInput, TicketTypeInput},
            TicketsBlock,
        },
        test_support::{admin_msg, output_html, TestContext},
    };

    /// A deployment with one active, public ticket type and one ticket of
    /// it: `(ctx, type id, ticket id)`.
    pub(in crate::blocks::tickets) async fn seeded() -> (TestContext, String, String) {
        let ctx = TestContext::with_tickets().await;
        let kind = service::create_type(
            &ctx,
            TicketTypeInput {
                key: "incorrect-info".into(),
                title: "Incorrect information".into(),
                description: "Report information that is inaccurate.".into(),
                guidance: String::new(),
                default_priority: "normal".into(),
                escalation_kind: "privacy".into(),
                public_visible: true,
                requires_contact: false,
                requests_evidence: true,
                active: true,
                sort_order: 10,
            },
        )
        .await
        .expect("create ticket type");
        let ticket = service::create_ticket(
            &ctx,
            CreateTicketInput {
                type_id: kind.id.clone(),
                subject: "Opening hours are incorrect".into(),
                description: "The listing says Monday opening is 8am, but it is 9am.".into(),
                source_path: "/activities/example".into(),
                subject_type: String::new(),
                subject_id: String::new(),
                evidence_url: "https://example.test/hours".into(),
                reporter_email: String::new(),
                reporter_wants_reply: false,
                priority: Some("urgent".into()),
            },
            TicketSource::Admin,
            ActorType::Admin,
            "admin_1",
            None,
        )
        .await
        .expect("create ticket");
        (ctx, kind.id, ticket.id)
    }

    pub(in crate::blocks::tickets) async fn page(
        ctx: &TestContext,
        path_and_query: &str,
    ) -> String {
        let (path, query) = path_and_query
            .split_once('?')
            .unwrap_or((path_and_query, ""));
        let mut msg = admin_msg("retrieve", path);
        msg.set_meta("http.header.accept", "text/html");
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            msg.set_meta(format!("req.query.{key}"), value.into_owned());
        }
        output_html(
            TicketsBlock::new()
                .handle(ctx, msg, InputStream::empty())
                .await,
        )
        .await
    }

    /// Every `<select>` and text field of the inbox filters has a `<label
    /// for>` naming it, and the status options are the statuses the list
    /// endpoint accepts.
    #[tokio::test]
    async fn every_inbox_filter_is_labelled_and_offers_real_values() {
        let (ctx, _, _) = seeded().await;
        let html = page(&ctx, INBOX).await;
        for (id, label) in [
            ("ticket-filter-status", "Status"),
            ("ticket-filter-priority", "Priority"),
            ("ticket-filter-type_id", "Type"),
            ("ticket-filter-source", "Source"),
            ("ticket-filter-assignee_id", "Assignee ID"),
        ] {
            assert!(
                html.contains(&format!(
                    r#"<label class="form-label" for="{id}">{label}</label>"#
                )),
                "no label for {id}: {html}"
            );
            assert!(html.contains(&format!(r#"id="{id}""#)), "no control {id}");
        }
        for status in TicketStatus::ALL {
            assert!(
                html.contains(&format!(r#"<option value="{}">"#, status.as_str())),
                "status {status} missing from the filter"
            );
        }
        assert!(!html.contains(r#"value="open""#), "`open` is not a status");
    }

    /// The inbox is a linked DataTable with coloured badges, the New ticket
    /// form is a modal opened from the topbar, and the page carries no inline
    /// stylesheet, no `<details>` form and no script of its own.
    #[tokio::test]
    async fn the_inbox_is_a_linked_table_with_a_new_ticket_modal() {
        let (ctx, _, ticket) = seeded().await;
        let html = page(&ctx, INBOX).await;
        assert!(html.contains("data-table__row--linked"), "{html}");
        assert!(
            html.contains(&format!(r#"href="{INBOX}/{ticket}""#)),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="badge badge-warning">New</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="badge badge-danger">Urgent</span>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"data-modal-target="create-ticket""#),
            "New ticket opens the modal: {html}"
        );
        assert!(
            html.contains(r#"<dialog class="modal" id="create-ticket""#),
            "{html}"
        );
        assert!(html.contains(&format!(r#"hx-post="{INBOX}""#)), "{html}");
        for absent in [
            "<style",
            "Create internal ticket</summary>",
            "data-json-form",
            "alert(",
            "location.reload",
        ] {
            assert!(!html.contains(absent), "{absent} on the inbox: {html}");
        }
    }

    /// A filter nothing matches renders the shared empty state with a way
    /// back, not a table row of text.
    #[tokio::test]
    async fn a_filter_nothing_matches_offers_to_clear_it() {
        let (ctx, _, _) = seeded().await;
        let html = page(&ctx, &format!("{INBOX}?status=spam")).await;
        assert!(html.contains("No tickets match these filters"), "{html}");
        assert!(html.contains(r#"class="empty""#), "{html}");
        assert!(html.contains(">Clear filters</a>"), "{html}");
        assert!(!html.contains("<table"), "no table over nothing: {html}");
    }

    /// The ticket page: the reference as the last crumb under Tickets, the
    /// subject with its badges in the hero, the workflow and note forms with
    /// every field labelled, and the escalation callout.
    #[tokio::test]
    async fn the_ticket_page_is_a_detail_page_with_labelled_forms() {
        let (ctx, _, ticket) = seeded().await;
        let html = page(&ctx, &format!("{INBOX}/{ticket}")).await;
        assert!(
            html.contains(&format!(r#"href="{INBOX}">Tickets</a>"#)),
            "crumb: {html}"
        );
        assert!(
            html.contains(r#"<h2 class="detail-hero__title">Opening hours are incorrect</h2>"#),
            "{html}"
        );
        assert!(html.contains(r#"id="ticket-detail""#), "{html}");
        assert!(html.contains("detail-meta"), "{html}");
        assert!(html.contains("Privacy review required"), "{html}");
        for id in [
            "workflow-status",
            "workflow-priority",
            "workflow-assignee",
            "workflow-duplicate",
            "workflow-reason",
            "ticket-note",
        ] {
            assert!(
                html.contains(&format!(r#"for="{id}""#)),
                "no label for {id}: {html}"
            );
        }
        assert!(
            html.contains(&format!(r#"hx-patch="{INBOX}/{ticket}""#)),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"hx-post="{INBOX}/{ticket}/notes""#)),
            "{html}"
        );
        for absent in [
            "<style",
            "data-json-form",
            "data-ticket-workflow",
            "location.reload",
        ] {
            assert!(
                !html.contains(absent),
                "{absent} on the ticket page: {html}"
            );
        }
    }

    /// A closed ticket offers only the statuses it can move to: itself and
    /// Triaged.
    #[tokio::test]
    async fn a_closed_ticket_offers_only_reachable_statuses() {
        let (ctx, _, ticket) = seeded().await;
        service::update_workflow(
            &ctx,
            &ticket,
            crate::blocks::tickets::models::WorkflowUpdate {
                status: Some("resolved".into()),
                priority: None,
                assignee_id: None,
                duplicate_of: None,
                legal_hold: None,
                reason: "Fixed".into(),
            },
            ActorType::Admin,
            "admin_1",
        )
        .await
        .expect("resolve");
        let html = page(&ctx, &format!("{INBOX}/{ticket}")).await;
        let start = html.find(r#"id="workflow-status""#).expect("status select");
        let select = &html[start..start + html[start..].find("</select>").unwrap()];
        let offered: Vec<&str> = TicketStatus::ALL
            .iter()
            .map(|s| s.as_str())
            .filter(|s| select.contains(&format!(r#"value="{s}""#)))
            .collect();
        assert_eq!(offered, ["triaged", "resolved"], "{select}");
        assert!(select.contains(r#"value="resolved" selected"#), "{select}");
    }

    /// The types page is a table with one edit modal per type, opened by a
    /// labelled icon button; with no types it is the empty state.
    #[tokio::test]
    async fn the_types_page_is_a_table_with_edit_modals() {
        let (ctx, kind, _) = seeded().await;
        let html = page(&ctx, TYPES).await;
        assert!(html.contains("data-table"), "{html}");
        assert!(
            html.contains(r#"aria-label="Edit Incorrect information""#),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"<dialog class="modal" id="edit-type-{kind}""#)),
            "{html}"
        );
        assert!(
            html.contains(&format!(r#"hx-patch="{TYPES}/{kind}""#)),
            "{html}"
        );
        assert!(
            html.contains(r#"<dialog class="modal" id="create-type""#),
            "{html}"
        );
        assert!(!html.contains("<details"), "{html}");

        let empty = page(&TestContext::with_tickets().await, TYPES).await;
        assert!(empty.contains("No ticket types yet"), "{empty}");
    }

    /// Store `key` = `value` in the variables table, flagged `sensitive` as
    /// an administrator would.
    async fn store(ctx: &TestContext, key: &str, value: &str, sensitive: bool) {
        crate::platform_state::variables::insert(
            &ctx.fixture(),
            crate::platform_state::variables::NewVariable {
                key: key.into(),
                value: value.into(),
                name: String::new(),
                description: String::new(),
                warning: String::new(),
                sensitive,
                updated_by: "test".into(),
                block: None,
            },
        )
        .await
        .expect("store variable");
    }

    /// Values come masked from the admin block: a secret, a `…_KEY`, and a
    /// plain-named setting an administrator flagged sensitive each say only
    /// whether they are set; a plain value is shown; an unset value says
    /// which default applies; every key can wrap.
    #[tokio::test]
    async fn settings_show_only_what_the_admin_block_unmasks() {
        use super::super::config;

        let ctx = TestContext::with_tickets().await;
        store(&ctx, config::TURNSTILE_SECRET_KEY, "do-not-show-me", false).await;
        store(&ctx, config::TURNSTILE_SITE_KEY, "site-key-value", false).await;
        store(&ctx, config::SUPPORT_EMAIL, "flagged@example.test", true).await;
        store(&ctx, config::BACK_URL, "/help", false).await;
        let html = page(&ctx, "/b/tickets/admin/settings").await;
        for hidden in ["do-not-show-me", "site-key-value", "flagged@example.test"] {
            assert!(!html.contains(hidden), "{hidden} shown: {html}");
        }
        assert_eq!(
            html.matches(r#"<span class="badge badge-success">Set</span>"#)
                .count(),
            3,
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="badge badge-secondary">Not set</span>"#),
            "the identity secret is unset: {html}"
        );
        assert!(html.contains("<code>/help</code>"), "{html}");
        assert!(html.contains("Default: </span><code>3600</code>"), "{html}");
        assert!(html.contains("IMPRESSPRESS__<wbr>TICKETS__<wbr>"), "{html}");
        assert!(html.contains("Public reporting is off"), "{html}");
    }

    /// With filters applied, the phone's Filters disclosure says how many.
    #[tokio::test]
    async fn the_filter_disclosure_counts_the_active_filters() {
        let (ctx, _, _) = seeded().await;
        let html = page(&ctx, &format!("{INBOX}?status=new&priority=urgent")).await;
        assert!(html.contains("Filters (2)</summary>"), "{html}");
        let html = page(&ctx, INBOX).await;
        assert!(html.contains(">Filters</summary>"), "{html}");
    }

    /// The endpoint reference is generated from the route table: every row
    /// appears once, in one of the two tables.
    #[tokio::test]
    async fn the_endpoints_page_lists_every_declared_route() {
        let ctx = TestContext::with_tickets().await;
        let html = page(&ctx, "/b/tickets/admin/endpoints").await;
        let rows = html.matches("data-table__row").count();
        assert_eq!(rows, super::super::ROUTES.len(), "{html}");
        for route in super::super::ROUTES {
            let wrapped = components::breakable_id(route.template).into_string();
            assert!(html.contains(&wrapped), "{} missing", route.template);
        }
    }
}
