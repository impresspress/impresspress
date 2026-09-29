/// The block's wire types, including the two enums that define its `kind`
/// and `role` columns. `pub` because `blocks::llm` converts [`EntryRole`]
/// into `wafer_core::clients::llm::ChatRole` — the column is this block's,
/// so the type that names its values is too (B20).
///
/// [`EntryRole`]: contracts::EntryRole
pub mod contracts;
#[cfg(test)]
mod error_mapping_tests;
pub(crate) mod migrations;
pub mod pages;
pub mod rest;
pub mod service;

use wafer_run::{BlockInfo, HttpMethod, InstanceMode};

use crate::{
    endpoint_match::{self, request_schema_of, EndpointRoute},
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

/// The block's HTTP surface: what `handle()` dispatches on and what
/// `info().endpoints` is generated from. More-specific templates
/// (`.../{id}/entries`) precede generic ones (`.../{id}`) so ordering
/// resolves them like the old `ends_with` guards. The matcher binds `{id}`
/// into `req.param.id` for the handlers' `msg.var` readers.
///
/// The two SSR pages (the chat/context inspector) are `Admin` and the JSON
/// API is `Authenticated`. The central router enforces that from the
/// declaration, so the block hand-checks no `is_admin`.
const ROUTES: &[EndpointRoute<Route>] = &[
    // UI pages
    EndpointRoute::admin(HttpMethod::Get, "/b/messages/", Route::ContextListPage)
        .summary("Context list page")
        .tags(&["ui"]),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/messages/contexts/{id}",
        Route::ContextDetailPage,
    )
    .summary("Context detail page")
    .tags(&["ui"]),
    // Contexts
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/messages/api/contexts",
        Route::ListContexts,
    )
    .summary("List contexts")
    .description("List contexts with optional filters by type, status, sender_id, parent_id")
    .query_params(list_contexts_query_schema)
    .output(context_list_schema)
    .tags(&["contexts"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/messages/api/contexts",
        Route::CreateContext,
    )
    .summary("Create context")
    .input(request_schema_of::<contracts::CreateContextRequest>)
    .tags(&["contexts"]),
    // Entries under a context (before the generic `.../{id}` rows)
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/messages/api/contexts/{id}/entries",
        Route::ListEntries,
    )
    .summary("List entries in context")
    .path_params(context_id_path_schema)
    .query_params(list_entries_query_schema)
    .tags(&["entries"]),
    EndpointRoute::authenticated(
        HttpMethod::Post,
        "/b/messages/api/contexts/{id}/entries",
        Route::AddEntry,
    )
    .summary("Add entry to context")
    .path_params(context_id_path_schema)
    .input(request_schema_of::<contracts::AddEntryRequest>)
    .tags(&["entries"]),
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/messages/api/contexts/{id}",
        Route::GetContext,
    )
    .summary("Get context")
    .path_params(context_id_path_schema)
    .tags(&["contexts"]),
    EndpointRoute::authenticated(
        HttpMethod::Patch,
        "/b/messages/api/contexts/{id}",
        Route::UpdateContext,
    )
    .summary("Update context")
    .input(request_schema_of::<contracts::UpdateContextRequest>)
    .tags(&["contexts"]),
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/messages/api/contexts/{id}",
        Route::DeleteContext,
    )
    .summary("Delete context and its entries")
    .tags(&["contexts"]),
    // Entries by id
    EndpointRoute::authenticated(
        HttpMethod::Get,
        "/b/messages/api/entries/{id}",
        Route::GetEntry,
    )
    .summary("Get entry")
    .tags(&["entries"]),
    EndpointRoute::authenticated(
        HttpMethod::Delete,
        "/b/messages/api/entries/{id}",
        Route::DeleteEntry,
    )
    .summary("Delete entry")
    .tags(&["entries"]),
];

// The query and path schemas below stay hand-written: the filters come from
// `msg.query(..)` by name (rest.rs) and `id` from `msg.var("id")` as the
// table bound it, the same by-name shape as `files`'s bucket/key params and
// `products`'s `id_path_schema`. Nothing here deserializes a *struct*, so a
// type declared only to feed `request_schema_of::<T>` would have no runtime
// user.
//
// The two closed-set filters are the exception, and [`enum_filter`] is why:
// `rest::enum_query` deserializes `kind` and `role` into their enums, so the
// published value list is that enum's own schema rather than a third copy of
// the four spellings.

/// One closed-set query parameter: the enum's derived schema, with this
/// parameter's description attached.
///
/// Produces the same `{"description", "enum", "type"}` object a real enum
/// field publishes anywhere else, so the schema says what `rest::enum_query`
/// actually accepts — the descriptions used to carry the value list in prose
/// and could drift from it silently.
fn enum_filter<T: schemars::JsonSchema>(description: &str) -> serde_json::Value {
    let mut schema = request_schema_of::<T>();
    // The derive titles a *top-level* schema with its Rust type name; the
    // same enum as a field of `AddEntryRequest` carries no title, and a
    // query parameter is a field, not a document.
    if let Some(object) = schema.as_object_mut() {
        object.remove("title");
    }
    schema["description"] = serde_json::json!(description);
    schema
}

/// Query parameters of `GET /b/messages/api/contexts`.
fn list_contexts_query_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "type": {"type": "string", "description": "Filter by context type (conversation, task, notification)"},
            "status": {"type": "string", "description": "Filter by status"},
            "sender_id": {"type": "string", "description": "Filter by sender"},
            "parent_id": {"type": "string", "description": "Filter by parent context"},
            "page": {"type": "integer", "default": 1},
            "page_size": {"type": "integer", "default": 20}
        }
    })
}

/// Response of `GET /b/messages/api/contexts`.
///
/// Hand-written, not derived: `service::list_contexts` returns
/// `wafer_core::clients::database::RecordList`, a raw
/// `{records: [{id, data: <column map>}], total_count}` envelope with no
/// contract type behind `data` — same reasoning already recorded for
/// `products`'s `record_list_schema`. Typing it means typing the row shape
/// first (a behaviour change: an unlisted column would start being dropped
/// from the response), which is out of scope for a schema migration.
fn context_list_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "records": {"type": "array", "items": {"type": "object"}},
            "total_count": {"type": "integer"}
        }
    })
}

/// Path parameters of `GET /b/messages/api/contexts/{id}`.
fn context_id_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["id"],
        "properties": {
            "id": {"type": "string", "description": "Context ID"}
        }
    })
}

/// Query parameters of `GET /b/messages/api/contexts/{id}/entries`.
fn list_entries_query_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "kind": enum_filter::<contracts::EntryKind>("Filter by entry kind"),
            "role": enum_filter::<contracts::EntryRole>("Filter by sender role"),
            "page": {"type": "integer", "default": 1},
            "page_size": {"type": "integer", "default": 100}
        }
    })
}

crate::impresspress_feature_block! {
    /// Unified message and context system (`impresspress/messages`).
    pub struct MessagesBlock;
    name: "impresspress/messages",
    info: |_this| {
        use wafer_run::CollectionSchema;

        BlockInfo::new(
            "impresspress/messages",
            "0.0.1",
            "http-handler@v1",
            "Unified message and context system",
        )
        .instance_mode(InstanceMode::Singleton)
        .requires(vec![
            "wafer-run/database".into(),
            // `migration_helper::db_backend` reads the database backend through
            // the config client when the block's migrations run at Init.
            "wafer-run/config".into(),
        ])
        // No `grants(..)`. The two `ResourceGrant::read("impresspress/llm", ..)`
        // entries that used to be here existed only because the chat UI read
        // this block's tables directly with `db::list`. It reaches them
        // through `ctx.call_block("impresspress/messages", ..)` now, and a
        // call_block callee's own database access is authorized as itself
        // (`RuntimeContext::dispatch_call` makes this block's `node_id` the
        // `caller_id` of the database sub-context), so WRAP Rule 3 —
        // "the caller owns the resource" — admits it with no grant at all.
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
        .endpoints(endpoint_match::declare(ROUTES))
        .can_disable(true)
        .default_enabled(true)
    },
    handle: |_this, ctx, mut msg, input| {
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
            Route::CreateContext => rest::create_context(ctx, &msg, input).await,
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

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use wafer_run::Message;

    use crate::test_support::TestContext;

    /// A `TestContext` carrying this block's migrations **and** the block
    /// itself, so `ctx.call_block("impresspress/messages", ..)` reaches the
    /// real handlers.
    ///
    /// `blocks::llm` talks to this block only through `call_block`, so its
    /// own tests need the callee registered; without it `messages_create`
    /// silently returns `None` and a chat test proves nothing about the
    /// entries it thinks it wrote. Migrations are applied the way
    /// `TestContext::with_products` and friends apply theirs, after
    /// `with_auth()` so the migration-tracking table exists first.
    pub(crate) async fn ctx_with_messages() -> TestContext {
        let mut ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::messages::MessagesBlock::BLOCK_NAME);
        let sqlite: Vec<&str> = super::migrations::SQLITE_MIGRATIONS
            .iter()
            .map(|(_, sql)| *sql)
            .collect();
        crate::migration_helper::apply_migrations(
            &ctx,
            "impresspress/messages",
            &sqlite,
            super::migrations::POSTGRES_MIGRATIONS,
        )
        .await
        .expect("apply messages migrations in test fixture");
        ctx.register_block(
            "impresspress/messages",
            Arc::new(super::MessagesBlock::new()),
        );
        ctx
    }

    /// Run `msg` through the block's own route table so `{id}` is bound the
    /// way it is on the wire, then hand the message to a handler directly.
    /// Panics when no row matches: a test that sends an unroutable path
    /// would otherwise exercise the handler's "missing id" branch by
    /// accident.
    pub(crate) fn routed(mut msg: Message) -> Message {
        let route = crate::endpoint_match::dispatch(&mut msg, super::ROUTES);
        assert!(
            route.is_some(),
            "no messages route matches {} {}",
            msg.action(),
            msg.path()
        );
        msg
    }
}

#[cfg(test)]
mod table_tests {
    use wafer_run::Block as _;

    use super::*;

    /// `info().endpoints` is generated from `ROUTES`; nothing else declares
    /// an endpoint for this block.
    #[test]
    fn info_endpoints_come_from_the_table() {
        let declared = MessagesBlock::new().info().endpoints;
        assert_eq!(declared.len(), ROUTES.len());
        for (ep, row) in declared.iter().zip(ROUTES) {
            assert_eq!(ep.method, row.method, "{}", row.template);
            assert_eq!(ep.path, row.template);
            assert_eq!(ep.auth, row.auth, "{}", row.template);
        }
    }
}
