pub(crate) mod migrations;
pub mod pages;
pub mod rest;
pub mod service;

use wafer_run::{BlockEndpoint, BlockInfo, HttpMethod, InstanceMode};

use crate::{
    endpoint_match::{self, EndpointRoute},
    http::err_not_found,
};

/// In-block dispatch targets, one per declared HTTP endpoint.
#[derive(Clone, Copy)]
enum Route {
    ContextListPage,
    ContextDetailPage,
    ListContexts,
    CreateContext,
    GetContext,
    UpdateContext,
    DeleteContext,
    ListEntries,
    AddEntry,
    GetEntry,
    DeleteEntry,
}

/// Method + path-template dispatch table. Templates mirror the declared
/// `info().endpoints`; the matcher extracts `{id}` into `req.param.id`.
/// More-specific templates (`.../{id}/entries`) precede generic ones
/// (`.../{id}`) so ordering resolves them like the old `ends_with` guards.
const ROUTES: &[EndpointRoute<Route>] = &[
    EndpointRoute::new(HttpMethod::Get, "/b/messages/", Route::ContextListPage),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/messages/contexts/{id}",
        Route::ContextDetailPage,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/messages/api/contexts",
        Route::ListContexts,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/messages/api/contexts",
        Route::CreateContext,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/messages/api/contexts/{id}/entries",
        Route::ListEntries,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/messages/api/contexts/{id}/entries",
        Route::AddEntry,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/messages/api/contexts/{id}",
        Route::GetContext,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/b/messages/api/contexts/{id}",
        Route::UpdateContext,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/messages/api/contexts/{id}",
        Route::DeleteContext,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/messages/api/entries/{id}",
        Route::GetEntry,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/messages/api/entries/{id}",
        Route::DeleteEntry,
    ),
];

crate::impresspress_feature_block! {
    /// Unified message and context system (`impresspress/messages`).
    pub struct MessagesBlock;
    name: "impresspress/messages",
    info: |_this| {
        use wafer_block::types::ResourceGrant;
        use wafer_run::{AuthLevel, CollectionSchema};

        BlockInfo::new(
            "impresspress/messages",
            "0.0.1",
            "http-handler@v1",
            "Unified message and context system",
        )
        .instance_mode(InstanceMode::Singleton)
        .requires(vec!["wafer-run/database".into()])
        // The chat UI in `impresspress/llm` reads thread + entry rows directly
        // via the typed `db::list` client (see `llm/pages.rs`). Without these
        // grants the WRAP runtime denies the call and the chat sidebar
        // renders empty in production. Surfaced by the static
        // `scripts/audit-wrap-grants.sh` audit (PR #84).
        .grants(vec![
            ResourceGrant::read("impresspress/llm", service::CONTEXTS_TABLE),
            ResourceGrant::read("impresspress/llm", service::ENTRIES_TABLE),
        ])
        // Advisory table list — admin "Database tables" discovery + the WRAP
        // grant-UI read only `CollectionSchema::name`. The schema itself
        // (columns, indexes, FKs) lives solely in the block's hand-authored
        // `migrations/*.sqlite.sql` files (the single source for both runtime
        // `migrations::apply()` and the Cloudflare D1 build).
        .collections(vec![
            CollectionSchema::new(service::CONTEXTS_TABLE),
            CollectionSchema::new(service::ENTRIES_TABLE),
        ])
        .category(wafer_run::BlockCategory::Feature)
        .description(
            "Protocol-agnostic context + entry system. Supports chat conversations, \
             notifications, and future protocols. Contexts are \
             containers (conversations, tasks, channels). Entries are the universal \
             primitive (messages, artifacts, notifications, status changes).",
        )
        .endpoints(vec![
            // UI pages — admin-only (the chat/context inspector SSR pages).
            // Declared here so the central router enforces the admin tier
            // before dispatch; the block no longer hand-checks `is_admin`.
            BlockEndpoint::get("/b/messages/")
                .summary("Context list page")
                .auth(AuthLevel::Admin)
                .tags(&["ui"]),
            BlockEndpoint::get("/b/messages/contexts/{id}")
                .summary("Context detail page")
                .auth(AuthLevel::Admin)
                .tags(&["ui"]),
            // Contexts
            BlockEndpoint::get("/b/messages/api/contexts")
                .summary("List contexts")
                .description("List contexts with optional filters by type, status, sender_id, parent_id")
                .auth(AuthLevel::Authenticated)
                .query_params_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "type": {"type": "string", "description": "Filter by context type (conversation, task, notification)"},
                        "status": {"type": "string", "description": "Filter by status"},
                        "sender_id": {"type": "string", "description": "Filter by sender"},
                        "parent_id": {"type": "string", "description": "Filter by parent context"},
                        "page": {"type": "integer", "default": 1},
                        "page_size": {"type": "integer", "default": 20}
                    }
                }))
                .output_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "records": {"type": "array", "items": {"type": "object"}},
                        "total_count": {"type": "integer"}
                    }
                }))
                .tags(&["contexts"]),
            BlockEndpoint::post("/b/messages/api/contexts")
                .summary("Create context")
                .auth(AuthLevel::Authenticated)
                .input_schema(serde_json::json!({
                    "type": "object",
                    "required": ["type"],
                    "properties": {
                        "type": {"type": "string", "description": "Context type: conversation, task, notification, etc."},
                        "title": {"type": "string", "default": ""},
                        "sender_id": {"type": "string", "default": ""},
                        "recipient_id": {"type": "string", "default": ""},
                        "parent_id": {"type": "string", "description": "Parent context ID for sub-tasks/threads"},
                        "metadata": {"type": "object", "default": {}}
                    }
                }))
                .tags(&["contexts"]),
            BlockEndpoint::get("/b/messages/api/contexts/{id}")
                .summary("Get context")
                .auth(AuthLevel::Authenticated)
                .path_params_schema(serde_json::json!({
                    "type": "object",
                    "required": ["id"],
                    "properties": {
                        "id": {"type": "string", "description": "Context ID"}
                    }
                }))
                .tags(&["contexts"]),
            BlockEndpoint::patch("/b/messages/api/contexts/{id}")
                .summary("Update context")
                .auth(AuthLevel::Authenticated)
                .input_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "status": {"type": "string"},
                        "title": {"type": "string"},
                        "metadata": {"type": "object"}
                    }
                }))
                .tags(&["contexts"]),
            BlockEndpoint::delete("/b/messages/api/contexts/{id}")
                .summary("Delete context and its entries")
                .auth(AuthLevel::Authenticated)
                .tags(&["contexts"]),
            // Entries
            BlockEndpoint::get("/b/messages/api/contexts/{id}/entries")
                .summary("List entries in context")
                .auth(AuthLevel::Authenticated)
                .query_params_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "kind": {"type": "string", "description": "Filter by kind (message, artifact, notification, status)"},
                        "role": {"type": "string", "description": "Filter by role (user, agent, system)"},
                        "page": {"type": "integer", "default": 1},
                        "page_size": {"type": "integer", "default": 100}
                    }
                }))
                .tags(&["entries"]),
            BlockEndpoint::post("/b/messages/api/contexts/{id}/entries")
                .summary("Add entry to context")
                .auth(AuthLevel::Authenticated)
                .input_schema(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "kind": {"type": "string", "default": "message", "description": "Entry kind: message, artifact, notification, status"},
                        "role": {"type": "string", "default": "", "description": "Sender role: user, agent, system"},
                        "sender_id": {"type": "string", "default": ""},
                        "content": {"type": "string", "default": ""},
                        "content_type": {"type": "string", "default": "text/plain"},
                        "metadata": {"type": "object", "default": {}}
                    }
                }))
                .tags(&["entries"]),
            BlockEndpoint::get("/b/messages/api/entries/{id}")
                .summary("Get entry")
                .auth(AuthLevel::Authenticated)
                .tags(&["entries"]),
            BlockEndpoint::delete("/b/messages/api/entries/{id}")
                .summary("Delete entry")
                .auth(AuthLevel::Authenticated)
                .tags(&["entries"]),
        ])
        .can_disable(true)
        .default_enabled(true)
    },
    handle: |_this, ctx, msg, input| {
        // Auth is enforced centrally by `route_to_block` from the declared
        // endpoint `AuthLevel` (UI pages → Admin, API → Authenticated), so no
        // per-handler `user_id`/`is_admin` preamble is needed here. Dispatch
        // matches the same declared endpoint templates, extracting `{id}` into
        // `req.param.id` for the sub-handlers.
        let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
            return err_not_found("not found");
        };
        match route {
            Route::ContextListPage => pages::context_list_page(ctx, &msg).await,
            Route::ContextDetailPage => pages::context_detail_page(ctx, &msg).await,
            Route::ListContexts => rest::list_contexts(ctx, &msg).await,
            Route::CreateContext => rest::create_context(ctx, input).await,
            Route::GetContext => rest::get_context(ctx, &msg).await,
            Route::UpdateContext => rest::update_context(ctx, &msg, input).await,
            Route::DeleteContext => rest::delete_context(ctx, &msg).await,
            Route::ListEntries => rest::list_entries(ctx, &msg).await,
            Route::AddEntry => rest::add_entry(ctx, &msg, input).await,
            Route::GetEntry => rest::get_entry(ctx, &msg).await,
            Route::DeleteEntry => rest::delete_entry(ctx, &msg).await,
        }
    },
    lifecycle: |_this, ctx, event| {
        crate::migration_helper::lifecycle_init(
            ctx,
            &event,
            "impresspress/messages",
            migrations::SQLITE_MIGRATIONS,
            migrations::POSTGRES_MIGRATIONS,
        )
        .await
    },
}

#[cfg(test)]
mod tests {
    /// The `/a2a` JSON-RPC endpoint dispatched fully unauthenticated (no method
    /// handler checked the caller) and was removed. Guard against re-exposing it
    /// without an auth gate by asserting the real registered block info has no
    /// such endpoint.
    #[test]
    fn messages_block_does_not_expose_a2a_endpoint() {
        let info = crate::blocks::all_block_infos()
            .into_iter()
            .find(|i| i.name == "impresspress/messages")
            .expect("messages block must be in all_block_infos()");
        assert!(
            !info.endpoints.iter().any(|e| e.path == "/a2a"),
            "/a2a must not be exposed — it dispatched unauthenticated; re-add behind auth first"
        );
    }
}
