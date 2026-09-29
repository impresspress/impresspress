//! REST endpoint handlers for the messages block.
//!
//! Thin layer: parse HTTP request → call service → format JSON response.
//! Every id-bearing handler reads `{id}` as the block's route table bound it
//! and verifies the caller owns the row through [`owned_record`] before
//! touching it; the pure-CRUD shells compose the id-taking `blocks::crud`
//! primitives on that verified row.

use wafer_core::clients::database::Record;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{
    contracts::{AddEntryRequest, CreateContextRequest, UpdateContextRequest},
    pages,
    service::{self, ListContextsParams, ListEntriesParams},
};
use crate::{
    blocks::crud,
    http::{err_bad_request, err_internal, ok_json},
    ui,
    util::parse_body_value,
};

/// Convert empty string to None (msg.query() returns "" for missing params).
fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// The row `{id}` names in `table`, once the caller's ownership of it is
/// verified, or the 400 / 401 / 404 to send instead.
///
/// The id is read only as `endpoint_match::dispatch` bound it. A message
/// that never went through the table binds nothing and is refused here
/// rather than parsed out of the path. `label` is the resource name the
/// error texts use (`"Context"`, `"Entry"`).
async fn owned_record(
    ctx: &dyn Context,
    msg: &Message,
    table: &str,
    label: &str,
) -> Result<Record, OutputStream> {
    let id = msg.var("id");
    if id.is_empty() {
        return Err(err_bad_request(&format!(
            "Missing {} ID",
            label.to_lowercase()
        )));
    }
    crud::verify_owner(ctx, table, id, "owner_id", msg.user_id(), label).await
}

// ---------------------------------------------------------------------------
// Context endpoints
// ---------------------------------------------------------------------------

pub async fn list_contexts(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let (_, page_size, offset) = msg.pagination_params(20);
    let params = ListContextsParams {
        owner_id: Some(msg.user_id().to_string()), // owner scope
        context_type: non_empty(msg.query("type")),
        status: non_empty(msg.query("status")),
        sender_id: non_empty(msg.query("sender_id")),
        parent_id: non_empty(msg.query("parent_id")),
        page_size: page_size as u32,
        offset: offset as i64,
    };
    match service::list_contexts(ctx, &params).await {
        Ok(result) => ok_json(&result),
        Err(e) => crud::db_error_internal(e, "list_contexts failed"),
    }
}

/// `POST /b/messages/api/contexts` — create a context.
///
/// Takes `&Message` to read the authenticated owner, and to tell the two
/// callers apart. The new-context form on `/b/messages/` is a plain htmx
/// `hx-post`, so it sends `application/x-www-form-urlencoded`; SDK callers
/// send JSON. [`parse_body_value`] accepts either. The form targets
/// `#context-list` with `hx-swap="afterbegin"`, so an `HX-Request` gets the
/// row back as HTML — a JSON body would be swapped into the list as markup
/// and render as its own source text.
pub async fn create_context(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let parsed = match parse_body_value(&raw) {
        Ok(value) => value,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    let body: CreateContextRequest = match serde_json::from_value(parsed) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    match service::create_context(
        ctx,
        msg.user_id(), // owner derived server-side, never from body
        &body.context_type,
        &body.title,
        &body.sender_id,
        &body.recipient_id,
        body.parent_id.as_deref(),
        body.metadata,
    )
    .await
    {
        Ok(record) if ui::is_htmx(msg) => ui::html_response(pages::context_card(&record)),
        Ok(record) => ok_json(&record),
        Err(e) => crud::db_error_internal(e, "create_context failed"),
    }
}

pub async fn get_context(ctx: &dyn Context, msg: &Message) -> OutputStream {
    match owned_record(ctx, msg, service::CONTEXTS_TABLE, "Context").await {
        Ok(record) => ok_json(&record),
        Err(resp) => resp,
    }
}

pub async fn update_context(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let id = match owned_record(ctx, msg, service::CONTEXTS_TABLE, "Context").await {
        Ok(record) => record.id,
        Err(resp) => return resp,
    };
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: UpdateContextRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    match service::update_context(ctx, &id, body.status, body.title, body.metadata).await {
        Ok(record) => ok_json(&record),
        Err(e) => crud::db_error(e, "Context not found", "Database error"),
    }
}

pub async fn delete_context(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match owned_record(ctx, msg, service::CONTEXTS_TABLE, "Context").await {
        Ok(record) => record.id,
        Err(resp) => return resp,
    };
    match service::delete_context(ctx, &id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(e) => crud::db_error(e, "Context not found", "delete_context failed"),
    }
}

// ---------------------------------------------------------------------------
// Entry endpoints
// ---------------------------------------------------------------------------

pub async fn list_entries(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let context_id = match owned_record(ctx, msg, service::CONTEXTS_TABLE, "Context").await {
        Ok(record) => record.id,
        Err(resp) => return resp,
    };
    let (_, page_size, offset) = msg.pagination_params(100);
    let params = ListEntriesParams {
        kind: match crud::enum_query(msg, "kind") {
            Ok(kind) => kind,
            Err(resp) => return resp,
        },
        role: match crud::enum_query(msg, "role") {
            Ok(role) => role,
            Err(resp) => return resp,
        },
        page_size: page_size as u32,
        offset: offset as i64,
    };
    match service::list_entries(ctx, &context_id, &params).await {
        Ok(result) => ok_json(&result),
        Err(e) => crud::db_error_internal(e, "list_entries failed"),
    }
}

/// `POST /b/messages/api/contexts/{id}/entries` — append an entry.
///
/// Both composers on the context detail page are plain htmx `hx-post` forms
/// sending `application/x-www-form-urlencoded`; SDK callers send JSON.
/// [`parse_body_value`] accepts either. The composers target `#entries-list`
/// with `hx-swap="beforeend"`, so an `HX-Request` gets the entry back as the
/// same card the page renders rather than a JSON record.
pub async fn add_entry(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let context_id = match owned_record(ctx, msg, service::CONTEXTS_TABLE, "Context").await {
        Ok(record) => record.id,
        Err(resp) => return resp,
    };
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let parsed = match parse_body_value(&raw) {
        Ok(value) => value,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    let body: AddEntryRequest = match serde_json::from_value(parsed) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    match service::add_entry(
        ctx,
        msg.user_id(), // owner derived server-side, never from body
        &context_id,
        body.kind,
        body.role,
        &body.sender_id,
        &body.content,
        body.content_type.as_deref(),
        body.metadata,
    )
    .await
    {
        Ok(record) if ui::is_htmx(msg) => match pages::entry_card(&record) {
            Ok(markup) => ui::html_response(markup),
            Err(e) => err_internal("add_entry card render failed", e),
        },
        Ok(record) => ok_json(&record),
        Err(e) => crud::db_error_internal(e, "add_entry failed"),
    }
}

pub async fn get_entry(ctx: &dyn Context, msg: &Message) -> OutputStream {
    match owned_record(ctx, msg, service::ENTRIES_TABLE, "Entry").await {
        Ok(record) => ok_json(&record),
        Err(resp) => resp,
    }
}

pub async fn delete_entry(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match owned_record(ctx, msg, service::ENTRIES_TABLE, "Entry").await {
        Ok(record) => record.id,
        Err(resp) => return resp,
    };
    match crud::delete_record(ctx, service::ENTRIES_TABLE, &id, "Entry").await {
        Ok(deleted) => ok_json(&deleted),
        Err(resp) => resp,
    }
}

#[cfg(test)]
mod tests {
    // Messages has no shared `tests/harness.rs` module yet (unlike
    // `blocks::products::tests::harness`, which this mirrors) — the block's
    // handlers are exercised directly via `MessagesBlock::handle`, dispatched
    // the same way the central router would after auth (auth itself is
    // enforced centrally, not in `handle()` — see the comment in `mod.rs`).
    use wafer_block::http_codec;
    use wafer_run::{Block, TerminalNotResponse};

    use super::*;
    use crate::{blocks::messages::MessagesBlock, test_support::TestContext};

    /// Build a `TestContext` with admin + auth + messages migrations
    /// applied, and this block registered under its own name — see
    /// `blocks::messages::test_support::ctx_with_messages`, which `blocks::llm`
    /// shares.
    async fn messages_ctx() -> TestContext {
        crate::blocks::messages::test_support::ctx_with_messages().await
    }

    /// Build a request `Message` + `InputStream`. Mirrors
    /// `blocks::products::tests::harness::request_msg`: `req.action`/
    /// `req.resource` meta drive `endpoint_match::dispatch`, `auth.user_id`
    /// meta is what `msg.user_id()` reads (verified against
    /// `wafer_block::meta::META_AUTH_USER_ID` and `test_support::auth_msg`).
    fn request(
        action: &str,
        path: &str,
        user_id: &str,
        body: serde_json::Value,
    ) -> (Message, InputStream) {
        let mut msg = Message::new("http.request");
        msg.set_meta("req.action", action);
        msg.set_meta("req.resource", path);
        if !user_id.is_empty() {
            msg.set_meta("auth.user_id", user_id);
        }
        let data = serde_json::to_vec(&body).expect("serialize body");
        (msg, InputStream::from_bytes(data))
    }

    /// Build the request one of the block's own htmx forms produces.
    ///
    /// `body` is the raw `application/x-www-form-urlencoded` payload a
    /// browser sends — percent-encoded, `+` for space — not a Rust struct
    /// serialized to JSON, because the bug these tests pin was exactly the
    /// difference between the two. `htmx` is whether the submit carries the
    /// `HX-Request` header htmx sets on every request it makes; a form client
    /// that is not htmx sends the same body without it.
    fn form_request(
        action: &str,
        path: &str,
        user_id: &str,
        body: &str,
        htmx: bool,
    ) -> (Message, InputStream) {
        let mut msg = Message::new("http.request");
        msg.set_meta("req.action", action);
        msg.set_meta("req.resource", path);
        msg.set_meta(
            "http.header.content-type",
            "application/x-www-form-urlencoded",
        );
        if htmx {
            msg.set_meta("http.header.hx-request", "true");
        }
        if !user_id.is_empty() {
            msg.set_meta("auth.user_id", user_id);
        }
        (msg, InputStream::from_bytes(body.as_bytes().to_vec()))
    }

    /// Dispatch through the real block `handle()` — same in-block routing
    /// (`endpoint_match::dispatch` + the `Route` match) production uses.
    async fn dispatch(ctx: &TestContext, msg: Message, input: InputStream) -> OutputStream {
        MessagesBlock::new().handle(ctx, msg, input).await
    }

    /// Status, `Content-Type` and body of a response, rendered the way the
    /// HTTP boundary renders it — so an error terminal reports its real
    /// status here instead of panicking.
    async fn http_parts(out: OutputStream) -> (u16, String, String) {
        let parts = wafer_block::http_codec::collect_http_response(out).await;
        let content_type = parts
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        let body = String::from_utf8(parts.body).expect("response body was not valid UTF-8");
        (parts.status, content_type, body)
    }

    /// Resolve an `OutputStream`'s HTTP status, including error terminals
    /// (`err_not_found`/`err_bad_request`/etc. return `OutputStream::error`,
    /// which `test_support::output_status` would panic on — this instead maps
    /// the `ErrorCode` to its canonical status via `wafer_block::http_codec`,
    /// the same mapping the real HTTP boundary uses).
    async fn status_of(out: OutputStream) -> u16 {
        match out.collect_buffered().await {
            Ok(buf) => http_codec::resolve_status(&buf.meta, 200),
            Err(TerminalNotResponse::Halt(buf)) => http_codec::resolve_status(&buf.meta, 200),
            Err(TerminalNotResponse::Error(e)) => http_codec::resolve_error_status(&e),
            Err(other) => panic!("unexpected terminal state: {other:?}"),
        }
    }

    fn listed_ids(listed: &serde_json::Value) -> Vec<String> {
        listed["records"]
            .as_array()
            .expect("records array")
            .iter()
            .map(|r| r["id"].as_str().expect("record id").to_string())
            .collect()
    }

    // --- Context request helpers ---

    async fn create_as(
        ctx: &TestContext,
        user_id: &str,
        body: serde_json::Value,
    ) -> serde_json::Value {
        let (msg, input) = request("create", "/b/messages/api/contexts", user_id, body);
        crate::test_support::output_json(dispatch(ctx, msg, input).await).await
    }

    async fn get_as(ctx: &TestContext, user_id: &str, id: &str) -> OutputStream {
        let (msg, input) = request(
            "retrieve",
            &format!("/b/messages/api/contexts/{id}"),
            user_id,
            serde_json::json!({}),
        );
        dispatch(ctx, msg, input).await
    }

    async fn list_as(ctx: &TestContext, user_id: &str) -> serde_json::Value {
        let (msg, input) = request(
            "retrieve",
            "/b/messages/api/contexts",
            user_id,
            serde_json::json!({}),
        );
        crate::test_support::output_json(dispatch(ctx, msg, input).await).await
    }

    async fn delete_as(ctx: &TestContext, user_id: &str, id: &str) -> OutputStream {
        let (msg, input) = request(
            "delete",
            &format!("/b/messages/api/contexts/{id}"),
            user_id,
            serde_json::json!({}),
        );
        dispatch(ctx, msg, input).await
    }

    async fn update_context_as(
        ctx: &TestContext,
        user_id: &str,
        id: &str,
        body: serde_json::Value,
    ) -> OutputStream {
        let (msg, input) = request(
            "update",
            &format!("/b/messages/api/contexts/{id}"),
            user_id,
            body,
        );
        dispatch(ctx, msg, input).await
    }

    // --- Entry request helpers ---

    async fn add_entry_as(
        ctx: &TestContext,
        user_id: &str,
        context_id: &str,
        body: serde_json::Value,
    ) -> OutputStream {
        let (msg, input) = request(
            "create",
            &format!("/b/messages/api/contexts/{context_id}/entries"),
            user_id,
            body,
        );
        dispatch(ctx, msg, input).await
    }

    async fn list_entries_as(ctx: &TestContext, user_id: &str, context_id: &str) -> OutputStream {
        let (msg, input) = request(
            "retrieve",
            &format!("/b/messages/api/contexts/{context_id}/entries"),
            user_id,
            serde_json::json!({}),
        );
        dispatch(ctx, msg, input).await
    }

    async fn get_entry_as(ctx: &TestContext, user_id: &str, id: &str) -> OutputStream {
        let (msg, input) = request(
            "retrieve",
            &format!("/b/messages/api/entries/{id}"),
            user_id,
            serde_json::json!({}),
        );
        dispatch(ctx, msg, input).await
    }

    async fn delete_entry_as(ctx: &TestContext, user_id: &str, id: &str) -> OutputStream {
        let (msg, input) = request(
            "delete",
            &format!("/b/messages/api/entries/{id}"),
            user_id,
            serde_json::json!({}),
        );
        dispatch(ctx, msg, input).await
    }

    // --- Tests ---

    /// Handlers read the id the table bound, nothing else: an unrouted
    /// message with an id in its path is refused, and the same message
    /// routed through `ROUTES` reaches the row.
    #[tokio::test]
    async fn handlers_read_only_the_bound_id() {
        use crate::{blocks::messages::test_support::routed, test_support::auth_msg};

        let ctx = messages_ctx().await;
        let created = create_as(&ctx, "user-a", serde_json::json!({"type": "task"})).await;
        let ctx_id = created["id"].as_str().expect("id").to_string();
        let path = format!("/b/messages/api/contexts/{ctx_id}");

        let unrouted = get_context(&ctx, &auth_msg("retrieve", &path, "user-a")).await;
        assert_eq!(
            status_of(unrouted).await,
            400,
            "an unrouted message binds no id and must be refused, not parsed"
        );

        let bound = get_context(&ctx, &routed(auth_msg("retrieve", &path, "user-a"))).await;
        assert_eq!(status_of(bound).await, 200);
    }

    #[tokio::test]
    async fn context_is_owner_scoped_across_users() {
        let ctx = messages_ctx().await;

        // User A creates a context; the body's sender_id is spoofed to
        // "user-b" — owner_id must come from the authenticated
        // msg.user_id(), never the body.
        let created = create_as(
            &ctx,
            "user-a",
            serde_json::json!({"type": "conversation", "sender_id": "user-b"}),
        )
        .await;
        let ctx_id = created["id"].as_str().expect("id").to_string();
        assert_eq!(
            created["data"]["owner_id"], "user-a",
            "owner_id must come from msg.user_id, not body"
        );
        assert_eq!(
            created["data"]["sender_id"], "user-b",
            "sender_id remains for A2A addressing only, unrelated to ownership"
        );

        // User B GET → 404 (existence must not leak).
        let got = get_as(&ctx, "user-b", &ctx_id).await;
        assert_eq!(status_of(got).await, 404);

        // User B list → does not include A's context.
        let listed = list_as(&ctx, "user-b").await;
        assert!(!listed_ids(&listed).contains(&ctx_id));

        // User B DELETE → 404 and the row still exists for A.
        assert_eq!(
            status_of(delete_as(&ctx, "user-b", &ctx_id).await).await,
            404
        );
        assert_eq!(status_of(get_as(&ctx, "user-a", &ctx_id).await).await, 200);
    }

    #[tokio::test]
    async fn update_context_is_owner_scoped() {
        let ctx = messages_ctx().await;

        let created = create_as(&ctx, "user-a", serde_json::json!({"type": "task"})).await;
        let ctx_id = created["id"].as_str().expect("id").to_string();

        let out = update_context_as(
            &ctx,
            "user-b",
            &ctx_id,
            serde_json::json!({"title": "hijacked"}),
        )
        .await;
        assert_eq!(status_of(out).await, 404);

        let got = crate::test_support::output_json(get_as(&ctx, "user-a", &ctx_id).await).await;
        assert_ne!(got["data"]["title"], "hijacked");
    }

    #[tokio::test]
    async fn entry_create_binds_owner_and_requires_parent_ownership() {
        let ctx = messages_ctx().await;

        let created = create_as(&ctx, "user-a", serde_json::json!({"type": "conversation"})).await;
        let ctx_id = created["id"].as_str().expect("id").to_string();

        // User B cannot add an entry to A's context, even knowing its id.
        let out = add_entry_as(
            &ctx,
            "user-b",
            &ctx_id,
            serde_json::json!({"content": "sneaky"}),
        )
        .await;
        assert_eq!(status_of(out).await, 404);

        // Nor list A's entries.
        let out = list_entries_as(&ctx, "user-b", &ctx_id).await;
        assert_eq!(status_of(out).await, 404);

        // User A adds an entry with a spoofed body sender_id — owner_id must
        // come from msg.user_id(), never the body.
        let entry = crate::test_support::output_json(
            add_entry_as(
                &ctx,
                "user-a",
                &ctx_id,
                serde_json::json!({"content": "hi", "sender_id": "user-b"}),
            )
            .await,
        )
        .await;
        let entry_id = entry["id"].as_str().expect("id").to_string();
        assert_eq!(entry["data"]["owner_id"], "user-a");
        assert_eq!(entry["data"]["sender_id"], "user-b");

        // User B cannot get or delete A's entry.
        assert_eq!(
            status_of(get_entry_as(&ctx, "user-b", &entry_id).await).await,
            404
        );
        assert_eq!(
            status_of(delete_entry_as(&ctx, "user-b", &entry_id).await).await,
            404
        );

        // The entry is still there for A.
        assert_eq!(
            status_of(get_entry_as(&ctx, "user-a", &entry_id).await).await,
            200
        );
    }

    #[tokio::test]
    async fn authenticated_api_accepts_assistant_role() {
        // Regression test for the LLM block's own assistant-message
        // persistence path: `blocks/llm/mod.rs::messages_create` builds
        // `{role: "assistant"}` and dispatches through this same
        // `rest::add_entry` handler via `ctx.call_block`. A prior commit
        // rejected `role in {assistant, system}` with 400, which silently
        // broke multi-turn chat by dropping every assistant reply. That
        // guard is gone; this locks in that assistant-role entries into a
        // context the caller owns succeed and round-trip the role.
        let ctx = messages_ctx().await;

        let created = create_as(&ctx, "user-a", serde_json::json!({"type": "conversation"})).await;
        let cid = created["id"].as_str().expect("id").to_string();

        for role in ["assistant", "system"] {
            let out = add_entry_as(
                &ctx,
                "user-a",
                &cid,
                serde_json::json!({"role": role, "content": "x"}),
            )
            .await;
            let entry = crate::test_support::output_json(out).await;
            assert_eq!(
                entry["data"]["role"], role,
                "role {role} must be accepted and round-trip"
            );
        }
    }

    /// B20's storage half. The composer offered `agent`, `blocks::llm` wrote
    /// `assistant`, and nothing reconciled them — so an entry posted as
    /// `agent` was replayed to the model as the *user's* own turn.
    ///
    /// `agent` is still accepted, because the rows and the clients using it
    /// are real, but it is an alias: the column holds `assistant`. The other
    /// half — that the history built from this column carries an assistant
    /// turn — is `blocks::llm::routes::chat`'s
    /// `an_entry_posted_as_agent_reaches_the_model_as_an_assistant_turn`.
    #[tokio::test]
    async fn an_agent_role_is_stored_as_the_assistant() {
        let ctx = messages_ctx().await;
        let created = create_as(&ctx, "user-a", serde_json::json!({"type": "conversation"})).await;
        let cid = created["id"].as_str().expect("id").to_string();

        let entry = crate::test_support::output_json(
            add_entry_as(
                &ctx,
                "user-a",
                &cid,
                serde_json::json!({"role": "agent", "content": "I did the thing"}),
            )
            .await,
        )
        .await;
        assert_eq!(
            entry["data"]["role"], "assistant",
            "`agent` is an alias of `assistant`, and `assistant` is the \
             spelling the column holds"
        );
    }

    /// Nothing validated `role` before it was a type: `{"role":"bot"}` was
    /// stored verbatim, and every later reader had to guess what it meant.
    #[tokio::test]
    async fn a_role_outside_the_set_is_refused() {
        let ctx = messages_ctx().await;
        let created = create_as(&ctx, "user-a", serde_json::json!({"type": "conversation"})).await;
        let cid = created["id"].as_str().expect("id").to_string();

        let out = add_entry_as(
            &ctx,
            "user-a",
            &cid,
            serde_json::json!({"role": "bot", "content": "x"}),
        )
        .await;
        assert_eq!(status_of(out).await, 400);

        let out = add_entry_as(
            &ctx,
            "user-a",
            &cid,
            serde_json::json!({"kind": "telegram", "content": "x"}),
        )
        .await;
        assert_eq!(status_of(out).await, 400);
    }

    /// `role`'s old default was `''`, and an empty role is invisible to the
    /// model — `llm::routes::chat::history_to_messages` drops it. So a body
    /// carrying only `content` posted a message the conversation it was
    /// posted into could not see. It is a user turn now.
    #[tokio::test]
    async fn an_omitted_role_stores_a_user_turn() {
        let ctx = messages_ctx().await;
        let created = create_as(&ctx, "user-a", serde_json::json!({"type": "conversation"})).await;
        let cid = created["id"].as_str().expect("id").to_string();

        let entry = crate::test_support::output_json(
            add_entry_as(&ctx, "user-a", &cid, serde_json::json!({"content": "hi"})).await,
        )
        .await;
        assert_eq!(entry["data"]["role"], "user");
        assert_eq!(
            entry["data"]["kind"], "message",
            "the kind default is unchanged — it is the column's own DEFAULT"
        );
    }

    /// A filter value outside the set used to reach the database as a
    /// literal that matched no row, so `?kind=nope` answered `200` with an
    /// empty page — "this context has no entries", which is a different
    /// sentence from "there is no such kind".
    #[tokio::test]
    async fn a_filter_value_outside_the_set_is_a_400_not_an_empty_page() {
        let ctx = messages_ctx().await;
        let created = create_as(&ctx, "user-a", serde_json::json!({"type": "conversation"})).await;
        let cid = created["id"].as_str().expect("id").to_string();
        add_entry_as(
            &ctx,
            "user-a",
            &cid,
            serde_json::json!({"role": "user", "content": "hi"}),
        )
        .await;

        let listing = |filter: Option<(&'static str, &'static str)>| {
            let (mut msg, input) = request(
                "retrieve",
                &format!("/b/messages/api/contexts/{cid}/entries"),
                "user-a",
                serde_json::json!({}),
            );
            if let Some((name, value)) = filter {
                msg.set_meta(format!("req.query.{name}"), value);
            }
            (msg, input)
        };

        for (name, value) in [("kind", "nope"), ("role", "bot")] {
            let (msg, input) = listing(Some((name, value)));
            assert_eq!(
                status_of(dispatch(&ctx, msg, input).await).await,
                400,
                "?{name}={value} must be refused"
            );
        }

        // The spellings the enum defines still filter, and no filter still
        // lists.
        for filter in [None, Some(("kind", "message")), Some(("role", "user"))] {
            let (msg, input) = listing(filter);
            let listed = crate::test_support::output_json(dispatch(&ctx, msg, input).await).await;
            assert_eq!(
                listed["records"].as_array().expect("records").len(),
                1,
                "filter {filter:?} must still match the stored entry"
            );
        }
    }

    // -----------------------------------------------------------------
    // The block's own htmx forms, posting the bytes a browser posts
    // -----------------------------------------------------------------

    /// The new-context form on `/b/messages/` carries no `hx-ext`, so htmx
    /// submits it with the browser's default encoding:
    /// `application/x-www-form-urlencoded`. The handler parsed JSON only, so
    /// every submit was a 400 and no context could be created from the UI.
    ///
    /// The field names and the URL are the ones
    /// `pages::context_list_page` renders — pinned there by
    /// `the_new_context_form_posts_what_create_context_reads`.
    #[tokio::test]
    async fn the_new_context_form_submit_creates_the_context() {
        let ctx = messages_ctx().await;

        let (msg, input) = form_request(
            "create",
            "/b/messages/api/contexts",
            "user-a",
            "type=conversation&title=Deploy+planning",
            true,
        );
        let (status, content_type, body) = http_parts(dispatch(&ctx, msg, input).await).await;

        assert_eq!(status, 200, "form submit was refused: {body}");
        // The form swaps the response into `#context-list`, so a JSON body
        // would be inserted into the page as markup and read as its own
        // source text.
        assert!(
            content_type.starts_with("text/html"),
            "an htmx submit must be answered with HTML, got {content_type}: {body}"
        );
        assert!(
            body.contains("messages-list__item"),
            "the swapped fragment must be the list row, got: {body}"
        );
        assert!(
            body.contains("Deploy planning"),
            "the title must be form-decoded (`+` is a space), got: {body}"
        );

        // …and the row really landed, so the fragment is not being rendered
        // out of the request body.
        let listed = list_as(&ctx, "user-a").await;
        assert_eq!(listed_ids(&listed).len(), 1, "one context must be stored");
        assert_eq!(listed["records"][0]["data"]["title"], "Deploy planning");
        assert_eq!(listed["records"][0]["data"]["type"], "conversation");
    }

    /// Both composers on the context detail page are plain htmx forms too.
    /// The default view sends `kind`/`role`/`content`; the conversation
    /// composer sends the same three with `kind` and `role` as hidden inputs.
    #[tokio::test]
    async fn the_entry_composer_submit_appends_the_entry() {
        let ctx = messages_ctx().await;
        let created = create_as(&ctx, "user-a", serde_json::json!({"type": "conversation"})).await;
        let cid = created["id"].as_str().expect("id").to_string();

        let (msg, input) = form_request(
            "create",
            &format!("/b/messages/api/contexts/{cid}/entries"),
            "user-a",
            "kind=message&role=user&content=ship+it",
            true,
        );
        let (status, content_type, body) = http_parts(dispatch(&ctx, msg, input).await).await;

        assert_eq!(status, 200, "composer submit was refused: {body}");
        assert!(
            content_type.starts_with("text/html"),
            "the composer swaps into `#entries-list`, so the answer must be \
             HTML, got {content_type}: {body}"
        );
        assert!(
            body.contains("message-card"),
            "the swapped fragment must be the entry card, got: {body}"
        );
        assert!(
            body.contains("ship it"),
            "the content must be form-decoded, got: {body}"
        );

        let listed =
            crate::test_support::output_json(list_entries_as(&ctx, "user-a", &cid).await).await;
        assert_eq!(listed["records"].as_array().expect("records").len(), 1);
        assert_eq!(listed["records"][0]["data"]["content"], "ship it");
        assert_eq!(listed["records"][0]["data"]["role"], "user");
    }

    /// The HTML answer is for the htmx swap, not for the encoding: the same
    /// form bytes without an `HX-Request` header — a curl or an SDK posting a
    /// form — still get the JSON record, and a JSON body still gets JSON.
    #[tokio::test]
    async fn only_the_htmx_caller_gets_html_back() {
        let ctx = messages_ctx().await;

        let (msg, input) = form_request(
            "create",
            "/b/messages/api/contexts",
            "user-a",
            "type=task&title=No+htmx+here",
            false,
        );
        let (status, content_type, body) = http_parts(dispatch(&ctx, msg, input).await).await;
        assert_eq!(status, 200, "a non-htmx form post was refused: {body}");
        assert!(
            content_type.starts_with("application/json"),
            "a caller that did not ask for a swap keeps the JSON record, got \
             {content_type}: {body}"
        );
        let record: serde_json::Value = serde_json::from_str(&body).expect("JSON record");
        assert_eq!(record["data"]["title"], "No htmx here");

        // The JSON path is untouched.
        let json_created = create_as(
            &ctx,
            "user-a",
            serde_json::json!({"type": "task", "title": "SDK"}),
        )
        .await;
        assert_eq!(json_created["data"]["title"], "SDK");
    }

    /// A body that is neither JSON nor a form the contract accepts is still a
    /// 400 — accepting form encoding must not turn a malformed request into a
    /// row with default values everywhere.
    #[tokio::test]
    async fn a_form_body_missing_the_required_field_is_still_refused() {
        let ctx = messages_ctx().await;

        let (msg, input) = form_request(
            "create",
            "/b/messages/api/contexts",
            "user-a",
            "title=no+type+field",
            true,
        );
        assert_eq!(
            status_of(dispatch(&ctx, msg, input).await).await,
            400,
            "`type` is required by the contract"
        );
        assert!(
            listed_ids(&list_as(&ctx, "user-a").await).is_empty(),
            "a refused create must store nothing"
        );
    }
}
