//! REST endpoint handlers for the messages block.
//!
//! Thin layer: parse HTTP request → call service → format JSON response.
//! Pure-CRUD shells (get context/entry, delete entry) go through the shared
//! `blocks::crud` helpers instead.

use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::service::{self, ListContextsParams, ListEntriesParams};
use crate::{
    blocks::crud,
    http::{err_bad_request, err_internal, err_not_found, ok_json},
    util::path_param,
};

/// Path prefix preceding the context id in the REST routes.
const CONTEXTS_PREFIX: &str = "/b/messages/api/contexts/";

/// Path prefix preceding the entry id in the REST routes.
const ENTRIES_PREFIX: &str = "/b/messages/api/entries/";

/// Convert empty string to None (msg.query() returns "" for missing params).
fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
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
        page_size: page_size as i64,
        offset: offset as i64,
    };
    match service::list_contexts(ctx, &params).await {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("list_contexts failed", e),
    }
}

// create_context takes &Message to read the authenticated owner.
pub async fn create_context(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Body {
        #[serde(rename = "type")]
        context_type: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        sender_id: String,
        #[serde(default)]
        recipient_id: String,
        parent_id: Option<String>,
        metadata: Option<serde_json::Value>,
    }
    let raw = input.collect_to_bytes().await;
    let body: Body = match serde_json::from_slice(&raw) {
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
        Ok(record) => ok_json(&record),
        Err(e) => err_internal("create_context failed", e),
    }
}

pub async fn get_context(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_get_owned(
        ctx,
        msg,
        &crud::OwnedResource {
            collection: service::CONTEXTS_TABLE,
            path_prefix: CONTEXTS_PREFIX,
            owner_field: "owner_id",
            label: "Context",
        },
    )
    .await
}

pub async fn update_context(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let id = path_param(msg, "id", CONTEXTS_PREFIX).to_string();
    if id.is_empty() {
        return err_bad_request("Missing context ID");
    }
    if let Err(resp) = crud::verify_owner(
        ctx,
        service::CONTEXTS_TABLE,
        &id,
        "owner_id",
        msg.user_id(),
        "Context",
    )
    .await
    {
        return resp;
    }
    let raw = input.collect_to_bytes().await;
    let body: std::collections::HashMap<String, serde_json::Value> =
        match serde_json::from_slice(&raw) {
            Ok(b) => b,
            Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
        };
    match service::update_context(ctx, &id, body).await {
        Ok(record) => ok_json(&record),
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Context not found"),
        Err(e) => err_internal("Database error", e),
    }
}

pub async fn delete_context(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = path_param(msg, "id", CONTEXTS_PREFIX).to_string();
    if id.is_empty() {
        return err_bad_request("Missing context ID");
    }
    if let Err(resp) = crud::verify_owner(
        ctx,
        service::CONTEXTS_TABLE,
        &id,
        "owner_id",
        msg.user_id(),
        "Context",
    )
    .await
    {
        return resp;
    }
    match service::delete_context(ctx, &id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Context not found"),
        Err(e) => err_internal("delete_context failed", e),
    }
}

// ---------------------------------------------------------------------------
// Entry endpoints
// ---------------------------------------------------------------------------

pub async fn list_entries(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let context_id = path_param(msg, "id", CONTEXTS_PREFIX).to_string();
    if context_id.is_empty() {
        return err_bad_request("Missing context ID");
    }
    if let Err(resp) = crud::verify_owner(
        ctx,
        service::CONTEXTS_TABLE,
        &context_id,
        "owner_id",
        msg.user_id(),
        "Context",
    )
    .await
    {
        return resp;
    }
    let (_, page_size, offset) = msg.pagination_params(100);
    let params = ListEntriesParams {
        kind: non_empty(msg.query("kind")),
        role: non_empty(msg.query("role")),
        page_size: page_size as i64,
        offset: offset as i64,
    };
    match service::list_entries(ctx, &context_id, &params).await {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("list_entries failed", e),
    }
}

pub async fn add_entry(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let context_id = path_param(msg, "id", CONTEXTS_PREFIX).to_string();
    if context_id.is_empty() {
        return err_bad_request("Missing context ID");
    }
    if let Err(resp) = crud::verify_owner(
        ctx,
        service::CONTEXTS_TABLE,
        &context_id,
        "owner_id",
        msg.user_id(),
        "Context",
    )
    .await
    {
        return resp;
    }
    #[derive(serde::Deserialize)]
    struct Body {
        #[serde(default = "default_kind")]
        kind: String,
        #[serde(default)]
        role: String,
        #[serde(default)]
        sender_id: String,
        #[serde(default)]
        content: String,
        content_type: Option<String>,
        metadata: Option<serde_json::Value>,
    }
    fn default_kind() -> String {
        "message".to_string()
    }
    let raw = input.collect_to_bytes().await;
    let body: Body = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    if matches!(body.role.as_str(), "assistant" | "system") {
        return err_bad_request("role 'assistant' and 'system' are reserved for internal use");
    }
    match service::add_entry(
        ctx,
        msg.user_id(), // owner derived server-side, never from body
        &context_id,
        &body.kind,
        &body.role,
        &body.sender_id,
        &body.content,
        body.content_type.as_deref(),
        body.metadata,
    )
    .await
    {
        Ok(record) => ok_json(&record),
        Err(e) => err_internal("add_entry failed", e),
    }
}

pub async fn get_entry(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_get_owned(
        ctx,
        msg,
        &crud::OwnedResource {
            collection: service::ENTRIES_TABLE,
            path_prefix: ENTRIES_PREFIX,
            owner_field: "owner_id",
            label: "Entry",
        },
    )
    .await
}

pub async fn delete_entry(ctx: &dyn Context, msg: &Message) -> OutputStream {
    crud::crud_delete_owned(
        ctx,
        msg,
        &crud::OwnedResource {
            collection: service::ENTRIES_TABLE,
            path_prefix: ENTRIES_PREFIX,
            owner_field: "owner_id",
            label: "Entry",
        },
    )
    .await
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

    /// Build a `TestContext` with admin + auth + messages migrations applied.
    /// No `TestContext::with_messages()` exists yet (only files/products/
    /// userportal/vector have one) — this applies the block's migrations the
    /// same way those constructors do: through the production-gated
    /// `migration_helper::apply_migrations` path, after `with_auth()` so the
    /// `impresspress__admin__block_settings` tracking table exists first.
    async fn messages_ctx() -> TestContext {
        let ctx = TestContext::with_auth().await;
        let sqlite: Vec<&str> = crate::blocks::messages::migrations::SQLITE_MIGRATIONS
            .iter()
            .map(|(_, sql)| *sql)
            .collect();
        crate::migration_helper::apply_migrations(
            &ctx,
            "impresspress/messages",
            &sqlite,
            crate::blocks::messages::migrations::POSTGRES_MIGRATIONS,
        )
        .await
        .expect("apply messages migrations in test fixture");
        ctx
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

    /// Dispatch through the real block `handle()` — same in-block routing
    /// (`endpoint_match::dispatch` + the `Route` match) production uses.
    async fn dispatch(ctx: &TestContext, msg: Message, input: InputStream) -> OutputStream {
        MessagesBlock::new().handle(ctx, msg, input).await
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

    async fn create_as(ctx: &TestContext, user_id: &str, body: serde_json::Value) -> serde_json::Value {
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
        let out = add_entry_as(&ctx, "user-b", &ctx_id, serde_json::json!({"content": "sneaky"})).await;
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
    async fn authenticated_api_rejects_reserved_roles() {
        let ctx = messages_ctx().await;

        let created = create_as(&ctx, "user-a", serde_json::json!({"type": "conversation"})).await;
        let cid = created["id"].as_str().expect("id").to_string();

        for role in ["assistant", "system"] {
            let out = add_entry_as(&ctx, "user-a", &cid, serde_json::json!({"role": role, "content": "x"})).await;
            assert_eq!(status_of(out).await, 400, "role {role} must be rejected");
        }
    }
}
