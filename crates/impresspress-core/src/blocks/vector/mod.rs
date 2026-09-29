pub mod contracts;
#[cfg(test)]
mod error_mapping_tests;
pub mod ingestion;
mod legacy_names;
pub(crate) mod migrations;
pub mod pages;
pub mod pages_ui;
pub mod service;
#[cfg(test)]
pub(crate) mod test_support;

use wafer_run::{BlockInfo, HttpMethod, InstanceMode};

use crate::endpoint_match::{self, request_schema_of, response_schema_of, EndpointRoute};

/// In-block dispatch targets. UI pages and the JSON API share ONE matcher
/// table; the per-route access tier comes from the declared endpoint
/// `AuthLevel` and is enforced centrally (every row → Admin).
#[derive(Clone, Copy)]
enum Route {
    IndexListPage,
    IndexDetailPage,
    ApiCreateIndex,
    ApiListIndexes,
    ApiDeleteIndex,
    ApiUpsert,
    ApiQuery,
    ApiIngest,
    ApiEmbed,
    ApiStats,
    ApiDeleteSingle,
}

/// The block's HTTP surface: what `handle()` dispatches on and what
/// `info().endpoints` is generated from. The specific `api/indexes/{name}`
/// delete precedes the generic `api/{index}/{id}` delete so index-deletes
/// win (the old ordering invariant). The matcher binds `{name}` / `{index}`
/// / `{id}` into `req.param.*` for the handlers' `msg.var` readers.
///
/// Every row is `Admin`, pages and JSON API alike, and the central router
/// enforces that from the declaration, so the block holds no `user_id` /
/// `is_admin` preamble.
///
/// `Admin` rather than `Authenticated` because an index is a deployment-wide
/// resource, not a per-user one: the registry keys a row by `prefixed_name`
/// alone (`migrations/001_vector_schema.sqlite.sql`), the index name is the
/// whole namespace a query or a delete addresses, and one index pins one
/// (model, dimensions, backend) for everyone who reads it. There is no owner
/// to scope a request to, so `Authenticated` meant every logged-in caller
/// could list, query, re-ingest and delete every other tenant's corpus
/// through the JSON API while the equivalent UI stayed admin-only.
///
/// End-user RAG is served by a feature block calling the `wafer-run/vector`
/// service on the user's behalf — an inter-block call, gated by that block's
/// `requires` list and its WRAP grants — not by browsers reaching these
/// routes, so raising the tier costs no real caller.
const ROUTES: &[EndpointRoute<Route>] = &[
    // UI pages
    EndpointRoute::admin(HttpMethod::Get, "/b/vector/", Route::IndexListPage)
        .summary("Vector indexes admin list"),
    EndpointRoute::admin(HttpMethod::Get, "/b/vector/{name}/", Route::IndexDetailPage)
        .summary("Vector index detail"),
    // The admin modal posts this same endpoint as a URL-encoded form with
    // an `HX-Request` header and gets the index list back as HTML. The
    // schemas describe the programmatic JSON path; the form path builds the
    // same request type through `contracts::CreateIndexRequest::from_form`.
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/vector/api/indexes",
        Route::ApiCreateIndex,
    )
    .summary("Create a vector index")
    .input(request_schema_of::<contracts::CreateIndexRequest>)
    .output(response_schema_of::<contracts::CreateIndexResponse>),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/vector/api/indexes",
        Route::ApiListIndexes,
    )
    .summary("List indexes")
    .output(response_schema_of::<contracts::IndexListResponse>),
    EndpointRoute::admin(HttpMethod::Post, "/b/vector/api/upsert", Route::ApiUpsert)
        .summary("Upsert pre-computed vectors")
        .input(request_schema_of::<contracts::UpsertRequest>)
        .output(response_schema_of::<contracts::AckResponse>),
    EndpointRoute::admin(HttpMethod::Post, "/b/vector/api/query", Route::ApiQuery)
        .summary("Search vectors")
        .input(request_schema_of::<contracts::QueryRequest>)
        .output(response_schema_of::<contracts::QueryResponse>),
    EndpointRoute::admin(HttpMethod::Post, "/b/vector/api/ingest", Route::ApiIngest)
        .summary("Chunk + embed + upsert a document")
        .input(request_schema_of::<contracts::IngestRequest>)
        .output(response_schema_of::<contracts::IngestResponse>),
    EndpointRoute::admin(HttpMethod::Post, "/b/vector/api/embed", Route::ApiEmbed)
        .summary("Generate embeddings for raw text")
        .input(request_schema_of::<contracts::EmbedRequest>)
        .output(response_schema_of::<contracts::EmbedResponse>),
    EndpointRoute::admin(HttpMethod::Get, "/b/vector/api/stats", Route::ApiStats)
        .summary("Index stats and usage")
        .output(response_schema_of::<contracts::IndexStatsResponse>),
    // Deletes: the specific `indexes/{name}` row before the generic
    // `{index}/{id}` row.
    EndpointRoute::admin(
        HttpMethod::Delete,
        "/b/vector/api/indexes/{name}",
        Route::ApiDeleteIndex,
    )
    .summary("Delete an index")
    .path_params(index_name_path_schema)
    .output(response_schema_of::<contracts::AckResponse>),
    EndpointRoute::admin(
        HttpMethod::Delete,
        "/b/vector/api/{index}/{id}",
        Route::ApiDeleteSingle,
    )
    .summary("Delete a single vector")
    .path_params(vector_id_path_schema)
    .output(response_schema_of::<contracts::AckResponse>),
];

/// Path-parameter schema for `DELETE /b/vector/api/indexes/{name}`.
///
/// Hand-written rather than derived: the handler reads the name with
/// `msg.var("name")` by name, so a struct declared only to feed
/// `request_schema_of::<T>` would have no runtime user (the `tickets` /
/// `messages` precedent).
fn index_name_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["name"],
        "properties": {
            "name": {
                "type": "string",
                "description": "Index name, as returned by `GET /b/vector/api/indexes`."
            }
        }
    })
}

/// Path-parameter schema for `DELETE /b/vector/api/{index}/{id}`. Hand-written
/// for the same reason as [`index_name_path_schema`]: `pages::extract_index_and_id`
/// reads both by name.
fn vector_id_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["index", "id"],
        "properties": {
            "index": {
                "type": "string",
                "description": "Index name, as returned by `GET /b/vector/api/indexes`."
            },
            "id": {
                "type": "string",
                "description": "Row id, as supplied on upsert (or `{document_id}:{n}` for an ingested chunk)."
            }
        }
    })
}

crate::impresspress_feature_block! {
    /// Vector search, RAG ingestion, and embedding generation (`impresspress/vector`).
    pub struct VectorBlock;
    name: "impresspress/vector",
    info: |_this| {
        BlockInfo::new(
            "impresspress/vector",
            "0.0.1",
            "http-handler@v1",
            "Vector search, RAG ingestion, and embedding generation",
        )
        .instance_mode(InstanceMode::Singleton)
        .requires(vec![
            // Registry table reads/writes + per-index counts go through the
            // database service; without this entry caller_requires denies
            // every db::* call and the admin list silently renders empty.
            "wafer-run/database".into(),
            // `migration_helper::db_backend` reads the database backend through
            // the config client when the block's migrations run at Init.
            "wafer-run/config".into(),
        ])
        // Soft dependencies: on the call allowlist, not checked at seal, and a
        // call to one that is absent answers `Unimplemented`.
        .optional_requires(vec![
            // The runtime vector service (typed index/query/introspection
            // ops). Registered only where a vector store is: the
            // `native-embedding` build (`builder::boot::register_vector_block`)
            // and a runtime that injected a vector service
            // (`ImpresspressBuilder::vector_service`). Without it the runtime
            // still boots, and each call the block makes to it answers
            // `Unimplemented`.
            "wafer-run/vector".into(),
            // Embedding for ingest / query-by-text: one of the two is
            // registered on a given runtime, and the other is never called.
            "impresspress/fastembed".into(),
            "impresspress/transformers-embed".into(),
            // Contextual retrieval (`ingestion::add_context`, reached from
            // `POST /b/vector/api/ingest` with `contextual: true`) makes two
            // calls: `impresspress/llm` for the deployment's default
            // (provider, model) pair, and `wafer-run/llm` for the completion
            // itself via `wafer_core::clients::llm::chat`. `add_context`
            // degrades to the raw chunks when they are absent, which is why
            // `block-vector` can ship without an llm backend.
            "impresspress/llm".into(),
            "wafer-run/llm".into(),
        ])
        .category(wafer_run::BlockCategory::Feature)
        .endpoints(endpoint_match::declare(ROUTES))
        .can_disable(true)
        .default_enabled(true)
    },
    handle: |_this, ctx, mut msg, input| {
        // Auth is enforced centrally by `route_to_block` from the declared
        // endpoint `AuthLevel` (every row → Admin; see `ROUTES`), so the
        // block holds no `user_id`/`is_admin` preamble. The matcher binds
        // `{name}`/`{index}`/`{id}` into `req.param.*`.
        let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
            return crate::http::err_not_found("not found");
        };
        match route {
            Route::IndexListPage => pages_ui::index_list_page(ctx, &msg).await,
            Route::IndexDetailPage => {
                let name = msg.var("name").to_string();
                pages_ui::index_detail_page(ctx, &msg, &name).await
            }
            Route::ApiCreateIndex => pages::create_index(ctx, &msg, input).await,
            Route::ApiListIndexes => pages::list_indexes(ctx).await,
            Route::ApiDeleteIndex => pages::delete_index(ctx, &msg).await,
            Route::ApiUpsert => pages::upsert(ctx, input).await,
            Route::ApiQuery => pages::query(ctx, input).await,
            Route::ApiIngest => pages::ingest(ctx, input).await,
            Route::ApiEmbed => pages::embed(ctx, input).await,
            Route::ApiStats => pages::stats(ctx).await,
            Route::ApiDeleteSingle => pages::delete_single(ctx, &msg).await,
        }
    },
    lifecycle: |_this, ctx, event| {
        crate::migration_helper::lifecycle_init(
            ctx,
            &event,
            "impresspress/vector",
            migrations::SQLITE_MIGRATIONS,
            migrations::POSTGRES_MIGRATIONS,
        )
        .await?;
        if matches!(event.event_type, wafer_run::LifecycleType::Init) {
            legacy_names::rename_legacy_indexes(ctx).await?;
        }
        Ok(())
    },
}

#[cfg(test)]
mod table_tests {
    use wafer_run::Block as _;

    use super::*;

    /// `info().endpoints` is generated from `ROUTES`; nothing else declares
    /// an endpoint for this block.
    #[test]
    fn info_endpoints_come_from_the_table() {
        let declared = VectorBlock::new().info().endpoints;
        assert_eq!(declared.len(), ROUTES.len());
        for (ep, row) in declared.iter().zip(ROUTES) {
            assert_eq!(ep.method, row.method, "{}", row.template);
            assert_eq!(ep.path, row.template);
            assert_eq!(ep.auth, row.auth, "{}", row.template);
        }
    }
}

/// `seal()` refuses a block whose `requires` names a block that is not
/// registered. The vector block runs without the runtime vector service (a
/// build with no embedding backend) and without an llm backend, so it must
/// boot beside nothing but the database and config blocks.
#[cfg(test)]
mod seal_tests {
    use std::sync::Arc;

    use super::VectorBlock;

    #[tokio::test]
    async fn the_block_seals_without_its_soft_dependencies() {
        let mut wafer = wafer_run::Wafer::builder()
            .disable_inventory()
            .disable_lockfile()
            .build()
            .expect("build a bare runtime");
        let db: Arc<dyn wafer_core::interfaces::database::service::DatabaseService> = Arc::new(
            wafer_block_sqlite::service::SQLiteDatabaseService::open_in_memory()
                .expect("open in-memory sqlite"),
        );
        wafer_core::service_blocks::database::register_with(&mut wafer, db)
            .expect("register the database block");
        wafer_core::service_blocks::config::register_with(
            &mut wafer,
            Arc::new(wafer_core::service_blocks::config::EnvConfigService::new()),
        )
        .expect("register the config block");
        wafer
            .register_block("impresspress/vector", Arc::new(VectorBlock::new()))
            .expect("register the vector block");
        wafer
            .seal()
            .await
            .expect("vector must seal without wafer-run/vector, an embedding block or llm");
    }
}

#[cfg(test)]
mod access_tests {
    use std::sync::Arc;

    use wafer_run::{AuthLevel, Block as _};

    use super::*;
    use crate::{
        endpoint_match::action_for_method,
        test_support::{anon_msg, output_http_status, Session, TestContext},
    };

    /// A context that routes `/b/vector/*` to the real block and can sign
    /// people in, so each request below presents a real token.
    async fn ctx() -> TestContext {
        let mut ctx = TestContext::with_vector().await.with_sign_in_added();
        ctx.register_block("impresspress/vector", Arc::new(VectorBlock::new()));
        ctx
    }

    async fn signed_in(ctx: &TestContext, role: &str) -> Session {
        let email = format!("{role}@example.com");
        ctx.seed_account(&email, "correct-horse-battery-staple", role)
            .await;
        ctx.sign_in(&email, "correct-horse-battery-staple").await
    }

    /// An index is a deployment-wide resource with no owner column, so
    /// "logged in" was never an answer to "may this caller read it". Asserted
    /// on the declaration because that is what the router reads: a new row
    /// added at a lower tier would not fail a handler test, it would publish
    /// the corpus.
    #[test]
    fn every_declared_endpoint_is_admin() {
        let endpoints = VectorBlock::new().info().endpoints;
        assert!(!endpoints.is_empty());
        for ep in &endpoints {
            assert_eq!(
                ep.auth,
                AuthLevel::Admin,
                "{} {} must stay admin-only",
                ep.method,
                ep.path
            );
        }
    }

    /// A concrete request path for `template`: every `{name}` / `{rest...}`
    /// segment filled with a literal the matcher will bind. Derived rather
    /// than hand-listed so a route added to `ROUTES` is exercised by the test
    /// below without anyone remembering to add it.
    fn concrete_path(template: &str) -> String {
        template
            .split('/')
            .map(|seg| {
                if seg.starts_with('{') && seg.ends_with('}') {
                    "probe"
                } else {
                    seg
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// The declaration above, enforced: every route the block serves refuses
    /// a logged-in non-admin, driven through the router block and the
    /// pipeline — the credential resolved as production resolves it, then
    /// the router's own gate, not a copy of it.
    #[tokio::test]
    async fn every_route_refuses_a_non_admin_session() {
        let ctx = ctx().await;
        let member = signed_in(&ctx, "user").await;
        for row in ROUTES {
            let path = concrete_path(row.template);
            let action = action_for_method(row.method);
            assert_eq!(
                output_http_status(ctx.request(member.bearer(anon_msg(action, &path))).await).await,
                403,
                "{action} {path} must not be reachable by a logged-in non-admin"
            );
        }
    }

    /// The counterpart: an admin still reaches the block. Asserted on the two
    /// routes that answer without a `wafer-run/vector` backend registered
    /// (both report "no indexes"), so a 200 here is the block's own answer
    /// and not an artifact of the fixture.
    #[tokio::test]
    async fn an_admin_still_reaches_the_api() {
        let ctx = ctx().await;
        let admin = signed_in(&ctx, "admin").await;
        for path in ["/b/vector/api/indexes", "/b/vector/api/stats"] {
            assert_eq!(
                output_http_status(ctx.request(admin.bearer(anon_msg("retrieve", path))).await)
                    .await,
                200,
                "{path} must still serve an admin"
            );
        }
    }
}
