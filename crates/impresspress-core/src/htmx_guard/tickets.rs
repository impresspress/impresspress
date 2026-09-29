//! `impresspress/tickets`: the public submit form and its confirmation page,
//! and the admin triage pages.
//!
//! The submit form posts with `fetch` and the admin pages act through the
//! JSON API with `fetch`, not htmx, so no page carries a mutating htmx
//! control today and `must_fire` is empty. The pages are still rendered, so a
//! control added to one of them is fired from then on.

use std::sync::Arc;

use wafer_run::{Block, Message};

use super::{Entry, Exempt};
use crate::{
    blocks::tickets::{
        models::{ActorType, CreateTicketInput, TicketSource, TicketTypeInput},
        service, TicketsBlock,
    },
    test_support::{
        admin_msg, anon_msg,
        htmx::{Fixture, Page, Site},
        TestContext,
    },
};

pub(super) fn entry() -> Entry {
    Entry {
        block: "impresspress/tickets",
        fixture: Some(|| Box::pin(fixture())),
        // `/b/tickets/admin` answers 302 to `/b/tickets/admin/tickets`.
        exempt: &[("/b/tickets/admin", Exempt::Redirect)],
        // No mutating htmx control on any tickets page; see the module doc.
        must_reach: &[],
        cannot_succeed: &[],
        must_fire: &[],
    }
}

/// The submit form and its confirmation are public; triage is the admin's.
fn caller(action: &str, path: &str) -> Message {
    if path.starts_with("/b/tickets/admin") {
        admin_msg(action, path)
    } else {
        anon_msg(action, path)
    }
}

/// One public ticket type and one ticket of it, so the submit form offers a
/// type, the lists render a row and the detail page has a ticket to open.
async fn fixture() -> Fixture {
    let ctx = TestContext::with_tickets().await;
    let kind = service::create_type(
        &ctx,
        TicketTypeInput {
            key: "incorrect-info".into(),
            title: "Incorrect information".into(),
            description: "Report information that is inaccurate or outdated.".into(),
            guidance: "Include the page and the corrected facts where possible.".into(),
            default_priority: "normal".into(),
            escalation_kind: "none".into(),
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
            description: "The listing says Monday opening is 8am, but the venue opens at 9am."
                .into(),
            source_path: "/activities/example".into(),
            subject_type: "activity".into(),
            subject_id: "example-1".into(),
            evidence_url: "https://example.test/hours".into(),
            reporter_email: String::new(),
            reporter_wants_reply: false,
            priority: None,
        },
        TicketSource::Admin,
        ActorType::Admin,
        "admin_1",
        None,
    )
    .await
    .expect("create ticket");

    Fixture {
        ctx,
        site: Site(vec![Arc::new(TicketsBlock::new()) as Arc<dyn Block>]),
        caller,
        pages: vec![
            Page::at("/b/tickets/submit"),
            Page::at("/b/tickets/submitted"),
            Page::at("/b/tickets/admin/tickets"),
            Page::at(format!("/b/tickets/admin/tickets/{}", ticket.id)),
            Page::at("/b/tickets/admin/types"),
            Page::at("/b/tickets/admin/settings"),
            Page::at("/b/tickets/admin/endpoints"),
        ],
        probes: vec![
            (
                "/b/tickets/api/admin/tickets/{id}",
                format!("/b/tickets/api/admin/tickets/{}", ticket.id),
            ),
            (
                "/b/tickets/api/admin/tickets/{id}/analyses",
                format!("/b/tickets/api/admin/tickets/{}/analyses", ticket.id),
            ),
        ],
        operator_input: &[],
    }
}
