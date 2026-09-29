//! Admin JSON API handlers.

use serde::de::DeserializeOwned;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{
    contracts::{
        AddNoteRequest, AdminCreateTicketRequest, AnalysisListResponse, TicketAnalysisView,
        TicketDetailResponse, TicketEventView, TicketListQuery, TicketListResponse,
        TicketTypeListQuery, TicketTypeListResponse, TicketTypeView, TicketView,
    },
    maintenance,
    models::{
        ActorType, AnalysisInput, CreateTicketInput, Priority, TicketSource, TicketStatus,
        TicketTypeInput, TicketTypeUpdate, WorkflowUpdate,
    },
    repo::{self, TicketFilters},
    service,
};
use crate::{
    blocks::crud,
    http::{err_bad_request, err_conflict, ok_json, ResponseBuilder},
};

const MAX_ADMIN_BODY: usize = 32 * 1_024;

pub async fn list_tickets(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let query = TicketListQuery::from_message(msg);
    if query.status.as_deref().is_some_and(invalid::<TicketStatus>) {
        return err_bad_request("Invalid status filter");
    }
    if query.priority.as_deref().is_some_and(invalid::<Priority>) {
        return err_bad_request("Invalid priority filter");
    }
    if query.source.as_deref().is_some_and(invalid::<TicketSource>) {
        return err_bad_request("Invalid source filter");
    }
    let offset = i64::from(query.page.saturating_sub(1)) * i64::from(query.page_size);
    let filters = TicketFilters {
        status: query.status.as_deref(),
        priority: query.priority.as_deref(),
        type_id: query.type_id.as_deref(),
        source: query.source.as_deref(),
        assignee_id: query.assignee_id.as_deref(),
    };
    match repo::list_tickets(ctx, &filters, query.page_size, offset).await {
        Ok(rows) => ok_json(&TicketListResponse::from_record_list(&rows)),
        Err(error) => crud::db_error_internal(error, "Could not list tickets"),
    }
}

pub async fn create_ticket(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let body: AdminCreateTicketRequest = match collect_json(input).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let source: TicketSource = match body.source.parse() {
        Ok(TicketSource::PublicForm) | Err(_) => {
            return err_bad_request("Admin creation source must be admin, api, or ai")
        }
        Ok(source) => source,
    };
    let actor = match source {
        TicketSource::Admin => ActorType::Admin,
        TicketSource::Api => ActorType::Api,
        TicketSource::Ai => ActorType::Ai,
        TicketSource::PublicForm => unreachable!(),
    };
    let ticket = CreateTicketInput {
        type_id: body.type_id,
        subject: body.subject,
        description: body.description,
        source_path: body.source_path,
        subject_type: body.subject_type,
        subject_id: body.subject_id,
        evidence_url: body.evidence_url,
        reporter_email: String::new(),
        reporter_wants_reply: false,
        priority: body.priority,
    };
    match service::create_ticket(ctx, ticket, source, actor, msg.user_id(), None).await {
        Ok(record) => ResponseBuilder::new()
            .status(201)
            .json(&TicketView::from_record(&record)),
        Err(error) => service_error(error),
    }
}

pub async fn get_ticket(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Ticket") {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service::detail(ctx, id).await {
        Ok(detail) => ok_json(&TicketDetailResponse::from_detail(detail)),
        Err(error) => crud::db_error(error, "Ticket not found", "Database error"),
    }
}

pub async fn update_ticket(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let body: WorkflowUpdate = match collect_json(input).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service::update_workflow(ctx, msg.var("id"), body, ActorType::Admin, msg.user_id()).await
    {
        Ok(record) => ok_json(&TicketView::from_record(&record)),
        Err(error) => service_error(error),
    }
}

pub async fn add_note(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let body: AddNoteRequest = match collect_json(input).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service::add_note(
        ctx,
        msg.var("id"),
        &body.note,
        ActorType::Admin,
        msg.user_id(),
    )
    .await
    {
        Ok(record) => ResponseBuilder::new()
            .status(201)
            .json(&TicketEventView::from_record(&record)),
        Err(error) => service_error(error),
    }
}

pub async fn list_analyses(ctx: &dyn Context, msg: &Message) -> OutputStream {
    match repo::list_analyses(ctx, msg.var("id"), 100).await {
        Ok(records) => ok_json(&AnalysisListResponse::from_records(&records)),
        Err(error) => crud::db_error_internal(error, "Could not list analyses"),
    }
}

pub async fn add_analysis(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let body: AnalysisInput = match collect_json(input).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service::add_analysis(ctx, msg.var("id"), body).await {
        Ok(record) => ResponseBuilder::new()
            .status(201)
            .json(&TicketAnalysisView::from_record(&record)),
        Err(error) => service_error(error),
    }
}

pub async fn list_types(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let query = TicketTypeListQuery::from_message(msg);
    let offset = i64::from(query.page.saturating_sub(1)) * i64::from(query.page_size);
    match repo::list_types(ctx, false, query.page_size, offset).await {
        Ok(rows) => ok_json(&TicketTypeListResponse::from_record_list(&rows)),
        Err(error) => crud::db_error_internal(error, "Could not list ticket types"),
    }
}

pub async fn create_type(ctx: &dyn Context, input: InputStream) -> OutputStream {
    let body: TicketTypeInput = match collect_json(input).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    // `types.key` is UNIQUE and the one unique value this write sets, so a
    // refused duplicate is that key being taken.
    let key = body.key.clone();
    match service::create_type(ctx, body).await {
        Ok(record) => ResponseBuilder::new()
            .status(201)
            .json(&TicketTypeView::from_record(&record)),
        Err(service::ServiceError::Db(error)) => crud::taken_key_or(
            error,
            crud::TakenKey::new("ticket type", "key", &key),
            |error| service_error(service::ServiceError::Db(error)),
        ),
        Err(error) => service_error(error),
    }
}

pub async fn update_type(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let body: TicketTypeUpdate = match collect_json(input).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    match service::update_type(ctx, msg.var("id"), body).await {
        Ok(record) => ok_json(&TicketTypeView::from_record(&record)),
        Err(error) => service_error(error),
    }
}

pub async fn status(ctx: &dyn Context) -> OutputStream {
    match maintenance::status(ctx).await {
        Ok(status) => ok_json(&status),
        Err(error) => crud::db_error_internal(error, "Could not load ticket status"),
    }
}

pub async fn prune(ctx: &dyn Context) -> OutputStream {
    let result = maintenance::prune(ctx, chrono::Utc::now()).await;
    if result.complete {
        ok_json(&result)
    } else {
        ResponseBuilder::new().status(503).json(&result)
    }
}

async fn collect_json<T: DeserializeOwned>(input: InputStream) -> Result<T, OutputStream> {
    let raw = input
        .collect_to_bytes()
        .await
        .map_err(OutputStream::error)?;
    if raw.len() > MAX_ADMIN_BODY {
        return Err(ResponseBuilder::new()
            .status(413)
            .body(b"Request is too large".to_vec(), "text/plain"));
    }
    serde_json::from_slice(&raw)
        .map_err(|error| err_bad_request(&format!("Invalid JSON request: {error}")))
}

/// The one mapping from a service failure to a response.
///
/// The two domain arms are the service's own vocabulary; the third is a
/// database failure and is classified by [`crud::db_error`], which is what
/// makes a WRAP refusal on `impresspress__tickets__*` a **403** instead of
/// the `500 Internal server error (ref: …)` this function used to answer for
/// everything that was not a `NotFound`.
fn service_error(error: service::ServiceError) -> OutputStream {
    match error {
        service::ServiceError::Validation(message) => err_bad_request(&message),
        service::ServiceError::Conflict(message) => err_conflict(&message),
        service::ServiceError::Db(error) => {
            crud::db_error(error, "Resource not found", "Database error")
        }
    }
}

/// Whether a filter value fails to parse as `T`, which is answered with 400.
fn invalid<T: std::str::FromStr>(value: &str) -> bool {
    value.parse::<T>().is_err()
}

#[cfg(test)]
mod denial_tests {
    use wafer_run::{ErrorCode, WaferError};

    use super::*;
    use crate::{
        blocks::tickets::service::ServiceError,
        endpoint_match,
        test_support::{admin_msg, output_http_status, TestContext},
    };

    /// A tickets fixture whose caller holds no WRAP grants, so every typed
    /// database call the block makes is refused by the same
    /// `wrap::check_access` the runtime applies. The schema is applied
    /// first, so the refusal is a denial and not a missing table.
    async fn denied_ctx() -> TestContext {
        TestContext::with_tickets()
            .await
            .running_as("test/ungranted")
    }

    fn routed(action: &str, path: &str) -> Message {
        let mut msg = admin_msg(action, path);
        assert!(
            endpoint_match::dispatch(&mut msg, crate::blocks::tickets::ROUTES).is_some(),
            "no tickets route matches {action} {path}"
        );
        msg
    }

    /// Every tickets JSON handler funnels its failures through
    /// [`service_error`], whose `Db` arm collapsed everything that was not a
    /// `NotFound` into `err_internal`. A WRAP refusal on
    /// `impresspress__tickets__*` therefore reached an admin as
    /// `500 Internal server error (ref: …)`, which is what an outage looks
    /// like — so the one failure an operator can fix read as the one they
    /// cannot.
    #[tokio::test]
    async fn service_error_classifies_a_denial_as_403() {
        let out = service_error(ServiceError::Db(WaferError::new(
            ErrorCode::PermissionDenied,
            "WRAP: block 'impresspress/tickets' has no grant for the table it read",
        )));
        assert_eq!(output_http_status(out).await, 403);
    }

    /// The other three arms are unchanged, so the 403 above is the new
    /// classification and not a blanket one.
    #[tokio::test]
    async fn service_error_keeps_its_other_classifications() {
        assert_eq!(
            output_http_status(service_error(ServiceError::Db(WaferError::new(
                ErrorCode::NotFound,
                "no such row"
            ))))
            .await,
            404
        );
        assert_eq!(
            output_http_status(service_error(ServiceError::Validation("bad".into()))).await,
            400
        );
        assert_eq!(
            output_http_status(service_error(ServiceError::Conflict("dupe".into()))).await,
            409
        );
        assert_eq!(
            output_http_status(service_error(ServiceError::Db(WaferError::new(
                ErrorCode::Internal,
                "connection reset"
            ))))
            .await,
            500
        );
    }

    /// `types.key` is UNIQUE: creating a second ticket type under a key one
    /// already has is a 409 naming the key, with nothing of the driver's
    /// text in it.
    #[tokio::test]
    async fn a_taken_type_key_is_a_409_naming_the_key() {
        let ctx = TestContext::with_tickets().await;
        let body = || InputStream::from_bytes(br#"{"key":"billing","title":"Billing"}"#.to_vec());
        assert_eq!(
            output_http_status(create_type(&ctx, body()).await).await,
            201
        );

        let parts =
            wafer_block::http_codec::collect_http_response(create_type(&ctx, body()).await).await;
        let json: serde_json::Value = serde_json::from_slice(&parts.body).unwrap_or_default();
        assert_eq!(parts.status, 409, "{json}");
        assert_eq!(
            json["message"],
            serde_json::json!(
                "A ticket type with the key \"billing\" already exists. Choose a different key."
            ),
        );
        let text = json.to_string();
        assert!(
            !text.contains("impresspress__tickets") && !text.contains("UNIQUE"),
            "schema leaked: {text}"
        );
    }

    /// End to end through the handler an admin actually calls.
    #[tokio::test]
    async fn a_denied_ticket_read_is_403_not_500() {
        let ctx = denied_ctx().await;
        let msg = routed("retrieve", "/b/tickets/api/admin/tickets/any-id");
        assert_eq!(output_http_status(get_ticket(&ctx, &msg).await).await, 403);
    }

    #[tokio::test]
    async fn a_denied_ticket_list_is_403_not_500() {
        let ctx = denied_ctx().await;
        let msg = routed("retrieve", "/b/tickets/api/admin/tickets");
        assert_eq!(
            output_http_status(list_tickets(&ctx, &msg).await).await,
            403
        );
    }
}
