//! Bounded persistence helpers for the tickets block.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use super::models::{ActorType, AnalysisInput, TicketSource, ValidTicketType};
use crate::util::json_map;

pub const TYPES: &str = "impresspress__tickets__types";
pub const TICKETS: &str = "impresspress__tickets__tickets";
pub const EVENTS: &str = "impresspress__tickets__events";
pub const ANALYSES: &str = "impresspress__tickets__analyses";
pub const MAINTENANCE: &str = "impresspress__tickets__maintenance";

const INBOX_COLUMNS: &[&str] = &[
    "id",
    "reference",
    "type_id",
    "type_key_snapshot",
    "type_title_snapshot",
    "source",
    "status",
    "priority",
    "subject",
    "source_path",
    "subject_type",
    "subject_id",
    "assignee_id",
    "legal_hold",
    "created_at",
    "updated_at",
];

pub async fn list_types(
    ctx: &dyn Context,
    public_only: bool,
    limit: u32,
    offset: i64,
) -> Result<db::RecordList, WaferError> {
    let mut filters = Vec::new();
    if public_only {
        filters.push(eq("active", true));
        filters.push(eq("public_visible", true));
    }
    db::list(
        ctx,
        TYPES,
        &ListOptions {
            filters,
            sort: vec![
                SortField {
                    field: "sort_order".into(),
                    desc: false,
                },
                SortField {
                    field: "title".into(),
                    desc: false,
                },
            ],
            limit: Some(limit.clamp(1, 100)),
            offset: offset.max(0),
            skip_count: public_only,
            ..Default::default()
        },
    )
    .await
}

pub async fn count_public_types(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(
        ctx,
        TYPES,
        &[eq("active", true), eq("public_visible", true)],
    )
    .await
}

pub async fn create_type(
    ctx: &dyn Context,
    value: &ValidTicketType,
) -> Result<db::Record, WaferError> {
    let mut data = json_map(serde_json::json!({
        "id": uuid::Uuid::now_v7().to_string(),
        "key": value.key,
        "title": value.title,
        "description": value.description,
        "guidance": value.guidance,
        "default_priority": value.default_priority.as_str(),
        "escalation_kind": value.escalation_kind.as_str(),
        "public_visible": value.public_visible,
        "requires_contact": value.requires_contact,
        "requests_evidence": value.requests_evidence,
        "active": value.active,
        "sort_order": value.sort_order,
    }));
    crate::util::stamp_created(&mut data);
    db::create(ctx, TYPES, data).await
}

pub async fn get_type(ctx: &dyn Context, id: &str) -> Result<db::Record, WaferError> {
    db::get(ctx, TYPES, id).await
}

pub async fn update_type(
    ctx: &dyn Context,
    id: &str,
    data: HashMap<String, serde_json::Value>,
) -> Result<db::Record, WaferError> {
    db::update(ctx, TYPES, id, data).await
}

#[derive(Debug, Default)]
pub struct TicketFilters<'a> {
    pub status: Option<&'a str>,
    pub priority: Option<&'a str>,
    pub type_id: Option<&'a str>,
    pub source: Option<&'a str>,
    pub assignee_id: Option<&'a str>,
}

impl TicketFilters<'_> {
    fn db_filters(&self) -> Vec<Filter> {
        let mut filters = Vec::new();
        push_filter(&mut filters, "status", self.status);
        push_filter(&mut filters, "priority", self.priority);
        push_filter(&mut filters, "type_id", self.type_id);
        push_filter(&mut filters, "source", self.source);
        push_filter(&mut filters, "assignee_id", self.assignee_id);
        filters
    }
}

pub async fn list_tickets(
    ctx: &dyn Context,
    filters: &TicketFilters<'_>,
    limit: u32,
    offset: i64,
) -> Result<db::RecordList, WaferError> {
    db::list(
        ctx,
        TICKETS,
        &ListOptions {
            filters: filters.db_filters(),
            sort: vec![SortField {
                field: "created_at".into(),
                desc: true,
            }],
            limit: Some(limit.clamp(1, 100)),
            offset: offset.max(0),
            skip_count: false,
            columns: Some(INBOX_COLUMNS.iter().map(|s| (*s).to_string()).collect()),
            ..Default::default()
        },
    )
    .await
}

pub async fn get_ticket(ctx: &dyn Context, id: &str) -> Result<db::Record, WaferError> {
    db::get(ctx, TICKETS, id).await
}

pub async fn find_by_dedupe(
    ctx: &dyn Context,
    dedupe_hash: &str,
) -> Result<Option<db::Record>, WaferError> {
    let rows = db::list(
        ctx,
        TICKETS,
        &ListOptions {
            filters: vec![eq("dedupe_hash", dedupe_hash)],
            limit: Some(1),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    Ok(rows.records.into_iter().next())
}

pub async fn create_ticket(
    ctx: &dyn Context,
    data: HashMap<String, serde_json::Value>,
) -> Result<db::Record, WaferError> {
    db::create(ctx, TICKETS, data).await
}

pub async fn update_ticket(
    ctx: &dyn Context,
    id: &str,
    data: HashMap<String, serde_json::Value>,
) -> Result<db::Record, WaferError> {
    db::update(ctx, TICKETS, id, data).await
}

#[expect(
    clippy::too_many_arguments,
    reason = "`ctx` plus one argument for each value a timeline row records; a \
              struct would move the same names one level down"
)]
pub async fn append_event(
    ctx: &dyn Context,
    ticket_id: &str,
    event_type: &str,
    actor_type: ActorType,
    actor_id: &str,
    body: &str,
    metadata: &serde_json::Value,
    expires_at: Option<&str>,
) -> Result<db::Record, WaferError> {
    let data = json_map(serde_json::json!({
        "id": uuid::Uuid::now_v7().to_string(),
        "ticket_id": ticket_id,
        "event_type": event_type,
        "actor_type": actor_type.as_str(),
        "actor_id": actor_id,
        "body": body,
        "metadata_json": serde_json::to_string(metadata).unwrap_or_else(|_| "{}".into()),
        "expires_at": expires_at,
        "created_at": crate::util::now_rfc3339(),
    }));
    db::create(ctx, EVENTS, data).await
}

pub async fn list_events(
    ctx: &dyn Context,
    ticket_id: &str,
    limit: u32,
) -> Result<Vec<db::Record>, WaferError> {
    let rows = db::list(
        ctx,
        EVENTS,
        &ListOptions {
            filters: vec![eq("ticket_id", ticket_id)],
            sort: vec![SortField {
                field: "created_at".into(),
                desc: true,
            }],
            limit: Some(limit.clamp(1, 201)),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    Ok(rows.records)
}

pub async fn create_analysis(
    ctx: &dyn Context,
    ticket_id: &str,
    input: &AnalysisInput,
    expires_at: Option<&str>,
) -> Result<db::Record, WaferError> {
    db::create(
        ctx,
        ANALYSES,
        json_map(serde_json::json!({
            "id": uuid::Uuid::now_v7().to_string(),
            "ticket_id": ticket_id,
            "source": input.source,
            "model": input.model,
            "prompt_version": input.prompt_version,
            "summary": input.summary,
            "suggested_type_id": input.suggested_type_id,
            "suggested_priority": input.suggested_priority,
            "confidence": input.confidence,
            "suggested_actions_json": serde_json::to_string(&input.suggested_actions)
                .unwrap_or_else(|_| "[]".into()),
            "expires_at": expires_at,
            "created_at": crate::util::now_rfc3339(),
        })),
    )
    .await
}

pub async fn list_analyses(
    ctx: &dyn Context,
    ticket_id: &str,
    limit: u32,
) -> Result<Vec<db::Record>, WaferError> {
    let rows = db::list(
        ctx,
        ANALYSES,
        &ListOptions {
            filters: vec![eq("ticket_id", ticket_id)],
            sort: vec![SortField {
                field: "created_at".into(),
                desc: true,
            }],
            limit: Some(limit.clamp(1, 101)),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    Ok(rows.records)
}

pub async fn count_tickets(ctx: &dyn Context, filters: Vec<Filter>) -> Result<i64, WaferError> {
    db::count(ctx, TICKETS, &filters).await
}

pub fn eq(field: &str, value: impl Into<serde_json::Value>) -> Filter {
    Filter {
        field: field.into(),
        operator: FilterOp::Equal,
        value: value.into(),
    }
}

pub fn before(field: &str, value: &str) -> Filter {
    Filter {
        field: field.into(),
        operator: FilterOp::LessThan,
        value: serde_json::json!(value),
    }
}

pub fn not_null(field: &str) -> Filter {
    Filter {
        field: field.into(),
        operator: FilterOp::NotEqual,
        value: serde_json::Value::Null,
    }
}

fn push_filter(filters: &mut Vec<Filter>, field: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        filters.push(eq(field, value));
    }
}

pub fn source_actor(source: TicketSource) -> ActorType {
    match source {
        TicketSource::PublicForm => ActorType::Public,
        TicketSource::Admin => ActorType::Admin,
        TicketSource::Api => ActorType::Api,
        TicketSource::Ai => ActorType::Ai,
    }
}
