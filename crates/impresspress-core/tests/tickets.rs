#![cfg(feature = "block-tickets")]

use impresspress_core::{
    blocks::tickets::{
        models::{
            ActorType, AnalysisInput, CreateTicketInput, TicketSource, TicketTypeInput,
            TicketTypeUpdate, WorkflowUpdate,
        },
        repo::{self, TicketFilters},
        service, TicketsBlock,
    },
    test_support::{admin_msg, output_html, output_json, TestContext},
};
use wafer_core::clients::database as db;
use wafer_run::{Block as _, HttpMethod, InputStream};

fn ticket_type(key: &str) -> TicketTypeInput {
    TicketTypeInput {
        key: key.into(),
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
    }
}

fn ticket(type_id: &str) -> CreateTicketInput {
    CreateTicketInput {
        type_id: type_id.into(),
        subject: "Opening hours are incorrect".into(),
        description: "The listing says Monday opening is 8am, but the venue says it opens at 9am."
            .into(),
        source_path: "/activities/example".into(),
        subject_type: "activity".into(),
        subject_id: "example-1".into(),
        evidence_url: "https://example.test/hours".into(),
        reporter_email: String::new(),
        reporter_wants_reply: false,
        priority: None,
    }
}

#[tokio::test]
async fn migration_and_workflow_preserve_original_report() {
    let ctx = TestContext::with_tickets().await;
    let kind = service::create_type(&ctx, ticket_type("incorrect-info"))
        .await
        .expect("create type");
    let created = service::create_ticket(
        &ctx,
        ticket(&kind.id),
        TicketSource::Admin,
        ActorType::Admin,
        "admin-1",
        None,
    )
    .await
    .expect("create ticket");
    let original_description = service::str_field(&created, "description").to_string();
    let original_type_title = service::str_field(&created, "type_title_snapshot").to_string();

    service::update_type(
        &ctx,
        &kind.id,
        TicketTypeUpdate {
            key: Some("incorrect-info".into()),
            title: Some("Updated category title".into()),
            description: None,
            guidance: None,
            default_priority: None,
            escalation_kind: None,
            public_visible: None,
            requires_contact: None,
            requests_evidence: None,
            active: None,
            sort_order: None,
        },
    )
    .await
    .expect("edit type");

    service::update_workflow(
        &ctx,
        &created.id,
        WorkflowUpdate {
            status: Some("resolved".into()),
            priority: Some("high".into()),
            assignee_id: Some("admin-1".into()),
            duplicate_of: None,
            legal_hold: None,
            reason: "The listing was corrected.".into(),
        },
        ActorType::Admin,
        "admin-1",
    )
    .await
    .expect("resolve");

    let detail = service::detail(&ctx, &created.id).await.expect("detail");
    assert_eq!(detail.untrusted_report.description, original_description);
    for field in [
        "subject",
        "description",
        "source_path",
        "subject_type",
        "subject_id",
        "evidence_url",
        "reporter_email",
        "reporter_wants_reply",
    ] {
        assert!(
            !detail.ticket.data.contains_key(field),
            "{field} must exist only in untrusted_report"
        );
    }
    assert_eq!(
        service::str_field(&detail.ticket, "type_title_snapshot"),
        original_type_title
    );
    assert_eq!(service::str_field(&detail.ticket, "status"), "resolved");
    assert!(!service::str_field(&detail.ticket, "expires_at").is_empty());
    assert!(detail.events.iter().any(|event| {
        service::str_field(event, "event_type") == "resolved"
            && service::str_field(event, "body") == "The listing was corrected."
    }));

    let closed_expiry = "2099-01-02T03:04:05+00:00";
    let resolved_at = "2026-01-02T03:04:05+00:00";
    db::update(
        &ctx,
        repo::TICKETS,
        &created.id,
        std::collections::HashMap::from([
            ("expires_at".to_string(), serde_json::json!(closed_expiry)),
            ("resolved_at".to_string(), serde_json::json!(resolved_at)),
        ]),
    )
    .await
    .expect("pin lifecycle timestamps");
    let reprioritized = service::update_workflow(
        &ctx,
        &created.id,
        WorkflowUpdate {
            // The admin form submits the visible closed status and checkbox
            // value even when only priority changes.
            status: Some("resolved".into()),
            priority: Some("low".into()),
            assignee_id: None,
            duplicate_of: None,
            legal_hold: Some(false),
            reason: String::new(),
        },
        ActorType::Admin,
        "admin-1",
    )
    .await
    .expect("reprioritize closed ticket")
    .into_record();
    assert_eq!(
        service::str_field(&reprioritized, "expires_at"),
        closed_expiry
    );
    assert_eq!(
        service::str_field(&reprioritized, "resolved_at"),
        resolved_at
    );
    let reprioritized_detail = service::detail(&ctx, &created.id)
        .await
        .expect("reprioritized detail");
    let workflow_event = reprioritized_detail
        .events
        .iter()
        .find(|event| service::str_field(event, "event_type") == "workflow_updated")
        .expect("workflow audit event");
    assert_eq!(
        service::str_field(workflow_event, "expires_at"),
        closed_expiry
    );

    let held = service::update_workflow(
        &ctx,
        &created.id,
        WorkflowUpdate {
            status: None,
            priority: None,
            assignee_id: None,
            duplicate_of: None,
            legal_hold: Some(true),
            reason: String::new(),
        },
        ActorType::Admin,
        "admin-1",
    )
    .await
    .expect("place legal hold")
    .into_record();
    assert!(service::nullable_str_field(&held, "expires_at").is_none());
    assert_eq!(service::str_field(&held, "resolved_at"), resolved_at);

    let released = service::update_workflow(
        &ctx,
        &created.id,
        WorkflowUpdate {
            status: None,
            priority: None,
            assignee_id: None,
            duplicate_of: None,
            legal_hold: Some(false),
            reason: String::new(),
        },
        ActorType::Admin,
        "admin-1",
    )
    .await
    .expect("release legal hold")
    .into_record();
    assert!(!service::str_field(&released, "expires_at").is_empty());
    assert_eq!(service::str_field(&released, "resolved_at"), resolved_at);
}

#[tokio::test]
async fn inbox_projection_omits_report_body_and_analysis_is_append_only() {
    let ctx = TestContext::with_tickets().await;
    let kind = service::create_type(&ctx, ticket_type("listing-problem"))
        .await
        .expect("create type");
    let created = service::create_ticket(
        &ctx,
        ticket(&kind.id),
        TicketSource::Ai,
        ActorType::Ai,
        "triage-agent",
        None,
    )
    .await
    .expect("create ticket");

    let rows = repo::list_tickets(&ctx, &TicketFilters::default(), 25, 0)
        .await
        .expect("list");
    assert_eq!(rows.records.len(), 1);
    assert!(!rows.records[0].data.contains_key("description"));
    assert!(!rows.records[0].data.contains_key("reporter_email"));

    let analysis = service::add_analysis(
        &ctx,
        &created.id,
        AnalysisInput {
            source: "triage-agent".into(),
            model: Some("provider-independent-model".into()),
            prompt_version: "tickets-v1".into(),
            summary: "The report likely concerns stale listing content.".into(),
            suggested_type_id: Some(kind.id.clone()),
            suggested_priority: Some("high".into()),
            confidence: 0.91,
            suggested_actions: serde_json::json!([
                {"kind": "verify_source"},
                {"kind": "prepare_patch", "requires_human_approval": true}
            ]),
        },
    )
    .await
    .expect("append analysis");
    assert_eq!(service::str_field(&analysis, "source"), "triage-agent");
    let after = repo::get_ticket(&ctx, &created.id)
        .await
        .expect("re-read ticket");
    assert_eq!(
        service::str_field(&after, "status"),
        "new",
        "an analysis is advisory: it does not move the ticket"
    );

    let detail = service::detail(&ctx, &created.id).await.expect("detail");
    assert_eq!(
        detail.untrusted_report.subject,
        "Opening hours are incorrect"
    );
    assert!(!detail.ticket.data.contains_key("subject"));
    assert!(!detail.ticket.data.contains_key("description"));

    let msg = admin_msg(
        "retrieve",
        &format!("/b/tickets/admin/tickets/{}", created.id),
    );
    let html = output_html(
        TicketsBlock::new()
            .handle(&ctx, msg, InputStream::empty())
            .await,
    )
    .await;
    for expected in [
        "detail-hero",
        "provider-independent-model",
        "tickets-v1",
        "Suggested priority",
        "0.91",
        "prepare_patch",
        "<h4 class=\"ticket-analysis__heading\">Summary</h4>",
    ] {
        assert!(
            html.contains(expected),
            "analysis provenance or detail page marker missing: {expected}"
        );
    }
}

#[tokio::test]
async fn duplicate_invariant_survives_every_patch_shape() {
    let ctx = TestContext::with_tickets().await;
    let kind = service::create_type(&ctx, ticket_type("duplicate-workflow"))
        .await
        .expect("create type");
    let first = service::create_ticket(
        &ctx,
        ticket(&kind.id),
        TicketSource::Admin,
        ActorType::Admin,
        "admin-1",
        None,
    )
    .await
    .expect("create first");
    let target = service::create_ticket(
        &ctx,
        ticket(&kind.id),
        TicketSource::Admin,
        ActorType::Admin,
        "admin-1",
        None,
    )
    .await
    .expect("create target");

    let duplicate = service::update_workflow(
        &ctx,
        &first.id,
        WorkflowUpdate {
            status: Some("duplicate".into()),
            priority: None,
            assignee_id: None,
            duplicate_of: Some(target.id.clone()),
            legal_hold: None,
            reason: "Same report".into(),
        },
        ActorType::Admin,
        "admin-1",
    )
    .await
    .expect("mark duplicate")
    .into_record();
    assert_eq!(
        service::nullable_str_field(&duplicate, "duplicate_of"),
        Some(target.id.as_str())
    );

    for invalid_target in [String::new(), first.id.clone()] {
        let result = service::update_workflow(
            &ctx,
            &first.id,
            WorkflowUpdate {
                status: None,
                priority: None,
                assignee_id: None,
                duplicate_of: Some(invalid_target),
                legal_hold: None,
                reason: String::new(),
            },
            ActorType::Admin,
            "admin-1",
        )
        .await;
        assert!(result.is_err(), "invalid duplicate patch must fail");
    }

    let unchanged = repo::get_ticket(&ctx, &first.id)
        .await
        .expect("stored duplicate");
    assert_eq!(
        service::nullable_str_field(&unchanged, "duplicate_of"),
        Some(target.id.as_str())
    );

    let reopened = service::update_workflow(
        &ctx,
        &first.id,
        WorkflowUpdate {
            status: Some("triaged".into()),
            priority: None,
            assignee_id: None,
            duplicate_of: None,
            legal_hold: None,
            reason: String::new(),
        },
        ActorType::Admin,
        "admin-1",
    )
    .await
    .expect("reopen duplicate")
    .into_record();
    assert_eq!(service::str_field(&reopened, "status"), "triaged");
    assert!(service::nullable_str_field(&reopened, "duplicate_of").is_none());
}

#[tokio::test]
async fn contact_required_types_require_email_and_reply_permission() {
    let ctx = TestContext::with_tickets().await;
    let mut contact_type = ticket_type("legal-contact");
    contact_type.requires_contact = true;
    let kind = service::create_type(&ctx, contact_type)
        .await
        .expect("create contact type");
    let mut report = ticket(&kind.id);
    report.reporter_email = "reporter@example.test".into();

    let without_permission = service::create_ticket(
        &ctx,
        report.clone(),
        TicketSource::PublicForm,
        ActorType::Public,
        "",
        None,
    )
    .await;
    assert!(without_permission.is_err());

    report.reporter_wants_reply = true;
    service::create_ticket(
        &ctx,
        report,
        TicketSource::PublicForm,
        ActorType::Public,
        "",
        None,
    )
    .await
    .expect("contact report with permission");
}

#[tokio::test]
async fn database_checks_reject_invalid_status_and_duplicate_type_key() {
    let ctx = TestContext::with_tickets().await;
    let kind = service::create_type(&ctx, ticket_type("copyright"))
        .await
        .expect("create type");
    assert!(service::create_type(&ctx, ticket_type("copyright"))
        .await
        .is_err());
    let created = service::create_ticket(
        &ctx,
        ticket(&kind.id),
        TicketSource::Api,
        ActorType::Api,
        "api-client",
        None,
    )
    .await
    .expect("create ticket");
    let invalid = db::update(
        &ctx,
        repo::TICKETS,
        &created.id,
        std::collections::HashMap::from([("status".to_string(), serde_json::json!("invented"))]),
    )
    .await;
    assert!(
        invalid.is_err(),
        "database CHECK must reject invalid status"
    );
}

#[tokio::test]
async fn admin_inbox_renders_all_filters_and_filter_preserving_pagination() {
    let ctx = TestContext::with_tickets().await;
    let kind = service::create_type(&ctx, ticket_type("admin-inbox"))
        .await
        .expect("create type");
    // Two tickets the filters below all match, one per page at
    // `page_size=1`: the pagination bar (and so its filter-carrying links)
    // renders only when the result spans more than one page.
    for _ in 0..2 {
        let created = service::create_ticket(
            &ctx,
            ticket(&kind.id),
            TicketSource::Admin,
            ActorType::Admin,
            "admin-1",
            None,
        )
        .await
        .expect("create ticket");
        service::update_workflow(
            &ctx,
            &created.id,
            WorkflowUpdate {
                status: None,
                priority: None,
                assignee_id: Some("admin:one".into()),
                duplicate_of: None,
                legal_hold: None,
                reason: "Assign for the inbox filter.".into(),
            },
            ActorType::Admin,
            "admin-1",
        )
        .await
        .expect("assign ticket");
    }

    let mut msg = admin_msg("retrieve", "/b/tickets/admin/tickets");
    msg.set_meta("req.query.status", "new");
    msg.set_meta("req.query.source", "admin");
    msg.set_meta("req.query.assignee_id", "admin:one");
    msg.set_meta("req.query.page_size", "1");
    let html = output_html(
        TicketsBlock::new()
            .handle(&ctx, msg, InputStream::empty())
            .await,
    )
    .await;
    assert!(
        html.contains("All sources"),
        "source filter missing: {html}"
    );
    assert!(
        html.contains("Assignee ID"),
        "assignee filter missing: {html}"
    );
    assert!(
        html.contains(r#"<th data-label="Created">Created</th>"#),
        "created column missing: {html}"
    );
    assert!(
        html.contains("pagination"),
        "pagination controls missing: {html}"
    );
    assert!(
        html.contains(
            "status=new&amp;source=admin&amp;assignee_id=admin%3Aone&amp;page_size=1&amp;page="
        ),
        "pagination must retain encoded filters: {html}"
    );
}

/// The analyses surface as an admin client reaches it: through the router,
/// with the JSON body the endpoint declares. Appending twice leaves two
/// analyses, the first exactly as written; the ticket itself is not moved;
/// and the block declares no endpoint that could rewrite or remove one.
#[tokio::test]
async fn analyses_posted_through_the_route_are_append_only() {
    let ctx = TestContext::with_tickets().await;
    let kind = service::create_type(&ctx, ticket_type("analysis-route"))
        .await
        .expect("create type");
    let created = service::create_ticket(
        &ctx,
        ticket(&kind.id),
        TicketSource::Admin,
        ActorType::Admin,
        "admin-1",
        None,
    )
    .await
    .expect("create ticket");
    let before = repo::get_ticket(&ctx, &created.id)
        .await
        .expect("read ticket");
    let path = format!("/b/tickets/api/admin/tickets/{}/analyses", created.id);

    let mut posted = Vec::new();
    for (summary, priority) in [("First pass", "high"), ("Second pass", "low")] {
        let out = ctx
            .dispatch_resolved_json(
                admin_msg("create", &path),
                &serde_json::json!({
                    "source": "triage-agent",
                    "prompt_version": "tickets-v1",
                    "summary": summary,
                    "suggested_priority": priority,
                    "confidence": 0.5,
                }),
            )
            .await;
        let body = output_json(out).await;
        assert_eq!(body["summary"], summary, "201 body is the stored analysis");
        posted.push(body);
    }

    let after = repo::get_ticket(&ctx, &created.id)
        .await
        .expect("re-read ticket");
    for field in ["status", "priority", "updated_at"] {
        assert_eq!(
            service::str_field(&after, field),
            service::str_field(&before, field),
            "appending an analysis must not change the ticket's {field}"
        );
    }

    let listed = output_json(ctx.dispatch_resolved(admin_msg("retrieve", &path)).await).await;
    let records = listed["records"].as_array().expect("records array");
    assert_eq!(records.len(), 2, "both analyses are kept: {listed}");
    let first = records
        .iter()
        .find(|r| r["id"] == posted[0]["id"])
        .expect("the first analysis is still listed");
    assert_eq!(
        first, &posted[0],
        "the second append left the first exactly as written"
    );

    let rewriting: Vec<String> = TicketsBlock::new()
        .info()
        .endpoints
        .iter()
        .filter(|e| {
            e.path.contains("/analyses") && !matches!(e.method, HttpMethod::Get | HttpMethod::Post)
        })
        .map(|e| format!("{:?} {}", e.method, e.path))
        .collect();
    assert!(
        rewriting.is_empty(),
        "analyses are append-only, but these endpoints could rewrite one: {rewriting:?}"
    );
}

/// A read the inbox, the detail page or the status route depends on, beside
/// the one it is about, fails closed: the inbox's type filter, the detail
/// page's escalation and the status route's audit flag each answer the
/// door's refusal rather than an empty list, "none" or "healthy".
#[tokio::test]
async fn a_refused_dependent_read_is_a_refusal_not_a_default() {
    use impresspress_core::test_support::FailingDbOpContext;
    use wafer_block::ServiceOp;
    use wafer_run::{ErrorCode, WaferError};

    let ctx = TestContext::with_tickets().await;
    let kind = service::create_type(&ctx, ticket_type("incorrect-info"))
        .await
        .expect("create type");
    let created = service::create_ticket(
        &ctx,
        ticket(&kind.id),
        TicketSource::Admin,
        ActorType::Admin,
        "admin-1",
        None,
    )
    .await
    .expect("create ticket");
    let denied = |table: &'static str| {
        FailingDbOpContext::failing_with(
            ctx.clone(),
            ServiceOp::DATABASE_OPS
                .iter()
                .map(|op| (*op, table))
                .collect(),
            WaferError::new(
                ErrorCode::PermissionDenied,
                "WRAP: impresspress/tickets holds no grant on this table",
            ),
        )
    };
    let mut misses = Vec::new();

    for (table, path) in [
        (repo::TYPES, "/b/tickets/admin/tickets".to_string()),
        (
            repo::TYPES,
            format!("/b/tickets/admin/tickets/{}", created.id),
        ),
    ] {
        let mut msg = admin_msg("retrieve", &path);
        msg.set_meta("http.header.accept", "text/html");
        let out = TicketsBlock::new()
            .handle(&denied(table), msg, InputStream::empty())
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

    let mut msg = admin_msg("retrieve", "/b/tickets/api/admin/status");
    msg.set_meta("http.header.accept", "application/json");
    let out = TicketsBlock::new()
        .handle(&denied(repo::MAINTENANCE), msg, InputStream::empty())
        .await;
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap_or_default();
    if (parts.status, body["message"].as_str()) != (403, Some("Access denied")) {
        misses.push(format!(
            "/b/tickets/api/admin/status: {} {body}",
            parts.status
        ));
    }

    assert!(
        misses.is_empty(),
        "expected the door's refusal at every site:\n{}",
        misses.join("\n")
    );
}

/// Every tickets write path writes: the creation event, a note, an analysis, a
/// status change's expiry propagated onto the ticket's events and analyses, and
/// a maintenance pass's status row. Each names a column the host stamps
/// (`updated_at`, and `created_at` on a create), which only tickets' 002
/// declares on `events`, `analyses` and `maintenance`.
#[tokio::test]
async fn every_tickets_write_path_writes_on_the_migrated_schema() {
    let ctx = TestContext::with_tickets().await;
    let kind = service::create_type(&ctx, ticket_type("strict-writes"))
        .await
        .expect("create type");
    let created = service::create_ticket(
        &ctx,
        ticket(&kind.id),
        TicketSource::Admin,
        ActorType::Admin,
        "admin-1",
        None,
    )
    .await
    .expect("create ticket");
    service::add_note(&ctx, &created.id, "Checked.", ActorType::Admin, "admin-1")
        .await
        .expect("a note is written");
    service::add_analysis(
        &ctx,
        &created.id,
        AnalysisInput {
            source: "triage-agent".into(),
            model: None,
            prompt_version: "tickets-v1".into(),
            summary: "Stale listing.".into(),
            suggested_type_id: None,
            suggested_priority: None,
            confidence: 0.5,
            suggested_actions: serde_json::json!([]),
        },
    )
    .await
    .expect("an analysis is written");
    service::update_workflow(
        &ctx,
        &created.id,
        WorkflowUpdate {
            status: Some("resolved".into()),
            priority: None,
            assignee_id: None,
            duplicate_of: None,
            legal_hold: None,
            reason: "Corrected.".into(),
        },
        ActorType::Admin,
        "admin-1",
    )
    .await
    .expect("a status change propagates its expiry");

    let events = db::list_all(&ctx, repo::EVENTS, vec![])
        .await
        .expect("read the events");
    assert_eq!(events.len(), 3, "created, note, resolved: {events:?}");
    let analyses = db::list_all(&ctx, repo::ANALYSES, vec![])
        .await
        .expect("read the analyses");
    for row in events.iter().chain(&analyses) {
        assert!(
            row.data.get("expires_at").is_some_and(|v| v.is_string()),
            "the resolution's expiry reached every row: {row:?}"
        );
        assert!(
            row.data.get("updated_at").is_some_and(|v| v.is_string()),
            "{row:?}"
        );
    }

    let pass =
        impresspress_core::blocks::tickets::maintenance::prune(&ctx, chrono::Utc::now()).await;
    assert!(pass.complete, "{:?}", pass.errors);
    let status = db::get(&ctx, repo::MAINTENANCE, "singleton")
        .await
        .expect("read the maintenance row");
    assert!(
        status
            .data
            .get("last_pruned_at")
            .is_some_and(|v| v.is_string()),
        "the pass recorded itself: {status:?}"
    );
    assert_eq!(
        status.data.get("audit_degraded"),
        Some(&serde_json::json!(0)),
        "nothing failed, so the audit trail is not degraded: {status:?}"
    );
}

/// An audit event that cannot be written degrades: the ticket is saved and
/// the "audit degraded" flag is set. A flag that cannot be written either
/// is the caller's error — nothing would record the gap — and so is a
/// maintenance pass that cannot record itself (`"status"`, and not
/// complete, so the route answers 503). None of them creates the singleton
/// row, which tickets' 001 seeds.
#[tokio::test]
async fn a_failed_audit_write_degrades_and_a_failed_flag_write_is_reported() {
    use impresspress_core::test_support::FailingDbOpContext;
    use wafer_block::ServiceOp;

    let ctx = TestContext::with_tickets().await;
    let kind = service::create_type(&ctx, ticket_type("audit-failure"))
        .await
        .expect("create type");
    let failing = |tables: &[&'static str]| {
        FailingDbOpContext::new(
            ctx.clone(),
            tables
                .iter()
                .flat_map(|table| ServiceOp::DATABASE_OPS.iter().map(move |op| (*op, *table)))
                .collect(),
        )
    };

    // The event alone fails: degraded, and the ticket stands.
    let saved = service::create_ticket(
        &failing(&[repo::EVENTS]),
        ticket(&kind.id),
        TicketSource::Admin,
        ActorType::Admin,
        "admin-1",
        None,
    )
    .await
    .expect("a failed audit event does not fail the ticket");
    repo::get_ticket(&ctx, &saved.id)
        .await
        .expect("the ticket is saved");
    let flag = db::get(&ctx, repo::MAINTENANCE, "singleton")
        .await
        .expect("read the maintenance row");
    assert_eq!(
        flag.data.get("audit_degraded"),
        Some(&serde_json::json!(1)),
        "the gap is recorded: {flag:?}"
    );

    // The event and the flag fail: the caller hears of it.
    let refused = service::create_ticket(
        &failing(&[repo::EVENTS, repo::MAINTENANCE]),
        ticket(&kind.id),
        TicketSource::Admin,
        ActorType::Admin,
        "admin-1",
        None,
    )
    .await;
    assert!(
        matches!(refused, Err(service::ServiceError::Db(_))),
        "a flag that cannot be written is the caller's error: {refused:?}"
    );

    // A maintenance pass that cannot record itself does not complete.
    let pass = impresspress_core::blocks::tickets::maintenance::prune(
        &failing(&[repo::MAINTENANCE]),
        chrono::Utc::now(),
    )
    .await;
    assert!(!pass.complete);
    assert_eq!(pass.errors, ["status"]);
}
