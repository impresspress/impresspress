mod contracts;
#[cfg(test)]
mod error_mapping_tests;
pub(crate) mod migrations;
mod pages;
mod repo;
mod service;

use maud::{html, Markup, PreEscaped};
use wafer_run::{
    context::Context, BlockInfo, ConfigVar, HttpMethod, InputStream, InputType, InstanceMode,
    Message, OutputStream, WaferError,
};

use self::{
    contracts::{DocumentListView, DocumentType, DocumentView, UpdateDocumentRequest},
    repo::documents::{self, NewDraft},
};
use crate::{
    blocks::crud,
    config_vars::PRIMARY_COLOR_KEY,
    endpoint_match::{self, request_schema_of, EndpointRoute},
    http::{err_bad_request, ok_json, require_row, ResponseBuilder},
    ui::{self, templates, SiteConfig},
};

/// In-block dispatch targets, one per declared HTTP endpoint.
#[derive(Clone, Copy)]
enum Route {
    PublicTerms,
    PublicPrivacy,
    EditorPrivacy,
    EditorTerms,
    SettingsPage,
    EndpointsPage,
    AdminSave,
    AdminRenderPreview,
    AdminPublish,
    AdminSaveSettings,
    ApiList,
    ApiGet,
    ApiCreate,
    ApiPublish,
    ApiUpdate,
    ApiDelete,
}

/// The block's HTTP surface: what `handle()` dispatches on and what
/// `info().endpoints` is generated from. The JSON `.../{id}/publish` template
/// precedes the generic `.../{id}` so the specific publish route wins
/// (replacing the old `ends_with("/publish")` guard). The JSON publish is a
/// PATCH (`update`) on the wire, matching the handler's historical dispatch.
/// The matcher binds `{id}` into `req.param.id` for the handlers' `msg.var`
/// readers.
///
/// The two published documents are `Public`. Every admin SSR sub-page,
/// mutation and JSON endpoint is declared `Admin`, and these declarations
/// are the gate: the router carries one `Public` prefix entry for
/// `/b/legalpages` and no `Admin` entry above it, and the handlers do not
/// re-check `is_admin`. `tests/snapshots/legalpages.endpoints.json` pins
/// every level.
const ROUTES: &[EndpointRoute<Route>] = &[
    // Published documents
    EndpointRoute::public(HttpMethod::Get, "/b/legalpages/terms", Route::PublicTerms)
        .summary("Published terms of service"),
    EndpointRoute::public(
        HttpMethod::Get,
        "/b/legalpages/privacy",
        Route::PublicPrivacy,
    )
    .summary("Published privacy policy"),
    // Admin editor pages
    EndpointRoute::admin(HttpMethod::Get, "/b/legalpages/admin", Route::EditorPrivacy)
        .summary("Admin editor (privacy)"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/legalpages/admin/privacy",
        Route::EditorPrivacy,
    )
    .summary("Admin editor (privacy)"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/legalpages/admin/terms",
        Route::EditorTerms,
    )
    .summary("Admin editor (terms)"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/legalpages/admin/settings",
        Route::SettingsPage,
    )
    .summary("Admin settings page"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/legalpages/admin/endpoints",
        Route::EndpointsPage,
    )
    .summary("Endpoints reference"),
    // Admin editor mutations
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/legalpages/admin/save",
        Route::AdminSave,
    )
    .summary("Save draft"),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/legalpages/admin/render-preview",
        Route::AdminRenderPreview,
    )
    .summary("Render markdown preview"),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/legalpages/admin/publish",
        Route::AdminPublish,
    )
    .summary("Publish from editor"),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/legalpages/admin/settings",
        Route::AdminSaveSettings,
    )
    .summary("Save settings"),
    // JSON API (specific `{id}/publish` before the generic `{id}` rows)
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/legalpages/api/documents",
        Route::ApiList,
    )
    .summary("List documents"),
    EndpointRoute::admin(
        HttpMethod::Post,
        "/b/legalpages/api/documents",
        Route::ApiCreate,
    )
    .summary("Create document"),
    EndpointRoute::admin(
        HttpMethod::Patch,
        "/b/legalpages/api/documents/{id}/publish",
        Route::ApiPublish,
    )
    .summary("Publish document"),
    EndpointRoute::admin(
        HttpMethod::Get,
        "/b/legalpages/api/documents/{id}",
        Route::ApiGet,
    )
    .summary("Get document"),
    EndpointRoute::admin(
        HttpMethod::Patch,
        "/b/legalpages/api/documents/{id}",
        Route::ApiUpdate,
    )
    .summary("Update document")
    .input(request_schema_of::<UpdateDocumentRequest>)
    .path_params(id_path_schema),
    EndpointRoute::admin(
        HttpMethod::Delete,
        "/b/legalpages/api/documents/{id}",
        Route::ApiDelete,
    )
    .summary("Delete document"),
];

/// Path-parameter schema for the `{id}` routes.
///
/// Hand-written rather than derived, the same way `tickets::id_path_schema`
/// is: every handler reads the id with `msg.var("id")` by name, so a struct
/// declared only to feed `request_schema_of::<T>` would have no runtime user.
/// `tests/openapi_snapshot.rs::path_placeholders_and_path_parameters_agree`
/// requires it of any published path that carries a `{…}` placeholder.
fn id_path_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id"],
        "properties": {
            "id": {
                "type": "string",
                "description": "Document identifier, as returned by the list endpoint."
            }
        }
    })
}

/// Config key: the public legal pages' background colour.
pub(crate) const BG_COLOR_KEY: &str = "IMPRESSPRESS__LEGALPAGES__BG_COLOR";
/// Config key: where the public legal pages' back button points.
pub(crate) const BACK_URL_KEY: &str = "IMPRESSPRESS__LEGALPAGES__BACK_URL";
/// Config key: custom footer HTML on the public legal pages.
pub(crate) const FOOTER_KEY: &str = "IMPRESSPRESS__LEGALPAGES__FOOTER";

/// The legalpages block's own declared config vars. Single source of truth for
/// both `BlockInfo::config_keys` and the admin settings page (rendered via
/// `ui::settings_form`, not a parallel tuple table that had drifted on the
/// `BG_COLOR` default).
pub(crate) fn config_vars() -> Vec<ConfigVar> {
    vec![
        ConfigVar::new(
            BG_COLOR_KEY,
            "Background color for public legal pages (empty = use design token default)",
            "",
        )
        .name("Background Color")
        .input_type(InputType::Color)
        .optional(),
        ConfigVar::new(
            BACK_URL_KEY,
            "Back button URL in the header (e.g., your website homepage)",
            "/",
        )
        .name("Back Button URL")
        .input_type(InputType::Url),
        ConfigVar::new(FOOTER_KEY, "Custom footer text (HTML allowed)", "")
            .name("Footer Text")
            .input_type(InputType::Textarea)
            .optional(),
    ]
}

/// The document id `{id}` as the route table bound it, or the 400 a missing
/// one turns into. A message that never went through
/// `endpoint_match::dispatch` binds nothing and is refused here rather than
/// parsed out of the path.
fn document_id(msg: &Message) -> Result<&str, OutputStream> {
    crud::path_id(msg, "Document")
}

impl LegalPagesBlock {
    async fn handle_get_public(&self, ctx: &dyn Context, doc_type: DocumentType) -> OutputStream {
        use wafer_core::clients::config;

        let chrome = async {
            Ok::<_, wafer_run::WaferError>((
                SiteConfig::load(ctx).await?,
                config::get_default(ctx, BG_COLOR_KEY, "").await?,
                config::get_default(ctx, BACK_URL_KEY, "/").await?,
                config::get_default(ctx, FOOTER_KEY, "").await?,
                config::get_default(ctx, PRIMARY_COLOR_KEY, "").await?,
            ))
        };
        let (site, bg_color, back_url, custom_footer, primary_color) = match chrome.await {
            Ok(chrome) => chrome,
            Err(e) => return crud::db_error_internal(e, "legalpages: page config read failed"),
        };

        let type_label = doc_type.title();

        let published = match documents::find_published(ctx, doc_type).await {
            Ok(row) => row,
            Err(e) => {
                return crud::db_error_internal(e, "legalpages: published document read failed")
            }
        };

        let (title, content, version, meta) = match published {
            None => (
                type_label.to_string(),
                markdown_to_html("No document has been published yet."),
                1_i64,
                String::new(),
            ),
            Some(doc) => {
                let title = if doc.title.is_empty() {
                    type_label.to_string()
                } else {
                    doc.title
                };
                let content = markdown_to_html(&doc.content);
                let published_at = doc.published_at.unwrap_or_default();
                let meta = if published_at.is_empty() {
                    String::new()
                } else {
                    format!(
                        "Last updated: {}",
                        published_at.get(..10).unwrap_or(&published_at),
                    )
                };
                (title, content, doc.version, meta)
            }
        };

        let markup = render_legal_page(LegalPageInputs {
            site: &site,
            title: &title,
            content: &content,
            version,
            meta: &meta,
            back_url: &back_url,
            bg_color: &bg_color,
            primary_color: &primary_color,
            custom_footer: &custom_footer,
        });
        ResponseBuilder::new().body(
            markup.into_string().into_bytes(),
            "text/html; charset=utf-8",
        )
    }

    async fn handle_admin_list(&self, ctx: &dyn Context, msg: &Message) -> OutputStream {
        let (page, page_size, _) = msg.pagination_params(20);
        // A `?type=` outside the set is refused rather than answered with
        // an empty page: "no such document type" and "this type has no
        // documents" are different sentences and the client can act on only
        // one of them.
        let doc_type = match crud::enum_query::<DocumentType>(msg, "type") {
            Ok(value) => value,
            Err(resp) => return resp,
        };
        match documents::list_page(ctx, doc_type, page as i64, page_size as u32).await {
            Ok(page) => ok_json(&DocumentListView::from_page(&page)),
            Err(e) => crud::db_error_internal(e, "Database error"),
        }
    }

    async fn handle_admin_create(
        &self,
        ctx: &dyn Context,
        msg: &Message,
        input: InputStream,
    ) -> OutputStream {
        #[derive(serde::Deserialize)]
        struct CreateDoc {
            /// Typed, so a `doc_type` no route can serve is a 400 rather
            /// than a row the block can store and never show.
            doc_type: DocumentType,
            title: String,
            content: String,
        }
        let raw = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return OutputStream::error(e),
        };
        let body: CreateDoc = match serde_json::from_slice(&raw) {
            Ok(b) => b,
            Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
        };

        // Draft shape lives in `repo::documents::insert_draft` (shared with
        // the admin editor's save handler in `pages.rs`).
        match documents::insert_draft(
            ctx,
            NewDraft {
                doc_type: body.doc_type,
                title: &body.title,
                content: &body.content,
                created_by: msg.user_id(),
            },
        )
        .await
        {
            Ok(row) => ok_json(&DocumentView::from_row(&row)),
            Err(e) => crud::db_error_internal(e, "Database error"),
        }
    }

    async fn handle_admin_publish(&self, ctx: &dyn Context, msg: &Message) -> OutputStream {
        let id = match document_id(msg) {
            Ok(id) => id,
            Err(resp) => return resp,
        };

        // Fetch the document first: its `doc_type` drives version
        // computation and which published siblings get archived.
        let doc = match documents::get(ctx, id)
            .await
            .map_err(|e| crud::db_error(e, "Document not found", "Database error"))
            .and_then(|row| require_row(row, "Document not found"))
        {
            Ok(doc) => doc,
            Err(response) => return response,
        };

        match service::publish_document(
            ctx,
            service::PublishRequest {
                doc_type: doc.doc_type,
                doc_id: id,
                title: None,
                content: None,
                version: 0,
                created_by: msg.user_id(),
            },
        )
        .await
        {
            Ok(published) => ok_json(&DocumentView::from_row(&published.row)),
            Err(e) => crud::db_error_internal(e, "Database error"),
        }
    }

    /// `GET /b/legalpages/api/documents/{id}`: the row.
    async fn handle_admin_get(&self, ctx: &dyn Context, msg: &Message) -> OutputStream {
        let id = match document_id(msg) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        match documents::get(ctx, id)
            .await
            .map_err(|e| crud::db_error(e, "Document not found", "Database error"))
            .and_then(|row| require_row(row, "Document not found"))
        {
            Ok(row) => ok_json(&DocumentView::from_row(&row)),
            Err(response) => response,
        }
    }

    /// `PATCH /b/legalpages/api/documents/{id}`: the document's text.
    ///
    /// The body is a typed [`UpdateDocumentRequest`], not a column map. That
    /// is the B10 fix: the handler used to hand whatever arrived to
    /// `crud::update_record`, which writes every key as a column, so
    /// `{"status":"published"}` published a document without going through
    /// `service::publish_document` — and therefore without archiving the row
    /// that was published before it. `deny_unknown_fields` makes the refusal
    /// a 400 naming the field rather than a silently ignored key.
    async fn handle_admin_update(
        &self,
        ctx: &dyn Context,
        msg: &Message,
        input: InputStream,
    ) -> OutputStream {
        let id = match document_id(msg) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        let body: UpdateDocumentRequest = match crud::read_json_body(input).await {
            Ok(body) => body,
            Err(resp) => return resp,
        };
        match documents::update_content(ctx, id, body.title.as_deref(), body.content.as_deref())
            .await
        {
            Ok(row) => ok_json(&DocumentView::from_row(&row)),
            Err(e) => crud::db_error(e, "Document not found", "Database error"),
        }
    }

    /// `DELETE /b/legalpages/api/documents/{id}`.
    async fn handle_admin_delete(&self, ctx: &dyn Context, msg: &Message) -> OutputStream {
        let id = match document_id(msg) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        match documents::delete(ctx, id).await {
            Ok(()) => ok_json(&crud::Deleted::done()),
            Err(e) => crud::db_error(e, "Document not found", "Database error"),
        }
    }

    /// Seed the two default documents, once, on Init.
    ///
    /// Returns `Result` and the lifecycle propagates it. The count used to be
    /// read through `unwrap_or(0)`, which made a count that *failed*
    /// indistinguishable from an empty table, so Init seeded a second set of
    /// documents on top of the existing ones and still reported success
    /// (B10).
    async fn seed_defaults(&self, ctx: &dyn Context) -> Result<(), WaferError> {
        if documents::count(ctx).await? > 0 {
            return Ok(());
        }

        for (doc_type, content) in &[
            (
                DocumentType::Terms,
                "These are the default terms of service. Please update them in the admin panel.\n",
            ),
            (
                DocumentType::Privacy,
                "This is the default privacy policy. Please update it in the admin panel.\n",
            ),
        ] {
            // Seed through the same service fn both publish surfaces use —
            // the published-document shape (status/version/published_at/…)
            // exists exactly once, in `repo::documents`. The table is empty
            // here (count == 0 above), so the archive pass is a no-op.
            //
            // A failure here fails Init too. Logging it left the deployment
            // with one of the two documents and a successful boot, which is
            // the same silence the count read used to have.
            service::publish_document(
                ctx,
                service::PublishRequest {
                    doc_type: *doc_type,
                    doc_id: "",
                    title: Some(doc_type.title()),
                    content: Some(content),
                    version: 1,
                    created_by: "system",
                },
            )
            .await?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Public page rendering
// ---------------------------------------------------------------------------

struct LegalPageInputs<'a> {
    site: &'a SiteConfig,
    title: &'a str,
    content: &'a str, // rendered HTML (output of markdown_to_html)
    version: i64,
    meta: &'a str,
    back_url: &'a str,
    bg_color: &'a str,      // empty string = use template default
    primary_color: &'a str, // empty string = use template default
    custom_footer: &'a str, // empty string = auto "© YEAR APP_NAME"
}

/// Render the legal-document body (title + meta + content) and delegate to
/// `templates::public_page` for the chrome.
fn render_legal_page(inputs: LegalPageInputs<'_>) -> Markup {
    let body = html! {
        div .public-page__head {
            h1 { (inputs.title) }
            @if !inputs.meta.is_empty() || inputs.version > 0 {
                div .public-page__meta {
                    @if !inputs.meta.is_empty() { span { (inputs.meta) } }
                    @if inputs.version > 0 {
                        span .public-page__version { "v" (inputs.version) }
                    }
                }
            }
        }
        div .public-page__content { (PreEscaped(inputs.content)) }
    };

    let footer_text = if !inputs.custom_footer.is_empty() {
        inputs.custom_footer.to_string()
    } else if !inputs.site.app_name.is_empty() {
        let year = chrono::Utc::now().format("%Y");
        format!(
            "\u{00a9} {} {}. All rights reserved.",
            year, inputs.site.app_name
        )
    } else {
        String::new()
    };
    let footer = if footer_text.is_empty() {
        None
    } else {
        // `custom_footer` allows admin-authored HTML; rendered with PreEscaped
        // here matches prior behavior. No user input on this path beyond what
        // the admin set.
        Some(html! { (PreEscaped(footer_text)) })
    };

    let bg_color = if inputs.bg_color.is_empty() {
        None
    } else {
        Some(inputs.bg_color)
    };
    let accent_color = if inputs.primary_color.is_empty() {
        None
    } else {
        Some(inputs.primary_color)
    };
    let back_url = if inputs.back_url.is_empty() {
        None
    } else {
        Some(inputs.back_url)
    };

    templates::public_page(
        templates::PublicPage {
            title: inputs.title,
            config: inputs.site,
            meta_description: None,
            back_url,
            bg_color,
            accent_color,
            footer,
        },
        body,
    )
}

/// Render admin-authored Markdown to HTML.
///
/// Uses `pulldown-cmark` with raw-HTML passthrough disabled (the default).
/// `<script>`, inline event handlers, and any other arbitrary HTML in the
/// source are emitted as escaped text rather than parsed — XSS-safe by
/// construction, replacing the previous ammonia sanitizer.
///
/// Link and image URLs are filtered at the event-stream level (before
/// HTML generation) so dangerous schemes like `javascript:` /
/// `JavaScript:` (case-insensitive), `data:`, and `vbscript:` are
/// rewritten to `#`. Matches ammonia's default behaviour of allowing
/// only `http`, `https`, `mailto`, `tel`, `ftp`, and `magnet`.
pub(super) fn markdown_to_html(input: &str) -> String {
    use pulldown_cmark::{html, Event, Options, Parser, Tag};

    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    // Filter raw HTML events (emitted as Event::Html or Event::InlineHtml)
    // so that `<script>` and other HTML in the source is not passed through,
    // then remap Link/Image dest_url through the scheme allow-list.
    let parser = Parser::new_ext(input, opts)
        .filter(|event| !matches!(event, Event::Html(_) | Event::InlineHtml(_)))
        .map(|event| match event {
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => Event::Start(Tag::Link {
                link_type,
                dest_url: safe_url(dest_url),
                title,
                id,
            }),
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) => Event::Start(Tag::Image {
                link_type,
                dest_url: safe_url(dest_url),
                title,
                id,
            }),
            other => other,
        });

    let mut out = String::with_capacity(input.len() + input.len() / 4);
    html::push_html(&mut out, parser);
    out
}

/// Allow-list URL schemes that ammonia's default config permitted.
/// Anything else (`javascript:`, `data:`, `vbscript:`, custom schemes)
/// becomes `#`. Scheme detection is case-insensitive per RFC 3986.
fn safe_url(url: pulldown_cmark::CowStr<'_>) -> pulldown_cmark::CowStr<'_> {
    const ALLOWED: &[&str] = &["http", "https", "mailto", "tel", "ftp", "magnet"];
    // Relative URLs (no scheme) are always safe.
    let scheme = match url.find(':') {
        Some(i) => &url[..i],
        None => return url,
    };
    // Fragment-only / query-only / path-only links never contain ':' at all
    // and were caught above. A leading `//` (protocol-relative URL) or `/`
    // (absolute path) starts with a non-alpha char, so won't match here.
    if !scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
    {
        return url;
    }
    if ALLOWED.iter().any(|s| scheme.eq_ignore_ascii_case(s)) {
        url
    } else {
        pulldown_cmark::CowStr::Borrowed("#")
    }
}

// ---------------------------------------------------------------------------
// Block trait implementation
// ---------------------------------------------------------------------------

crate::impresspress_feature_block! {
    /// Legal pages management with versioning and publishing (`impresspress/legalpages`).
    pub struct LegalPagesBlock;
    name: "impresspress/legalpages",
    info: |_this| {
        BlockInfo::new("impresspress/legalpages", "0.0.1", "http-handler@v1", "Legal pages management with versioning and publishing")
            .instance_mode(InstanceMode::Singleton)
            // `wafer-run/config`: `handle_get_public` reads this block's three
            // theming keys plus `WAFER_RUN_SHARED__PRIMARY_COLOR`, and
            // `SiteConfig::load` reads the shared site keys; without the
            // entry the runtime refuses every one of those calls and the
            // public page answers the refusal.
            .requires(vec!["wafer-run/database".into(), "wafer-run/config".into()])
            .category(wafer_run::BlockCategory::Feature)
            .description("Legal document management with versioning and publishing. Create and manage terms of service, privacy policies, and other legal documents. Supports draft/published workflow with version tracking.")
            .endpoints(endpoint_match::declare(ROUTES))
            .config_keys(config_vars())
            .admin_url("/b/legalpages/admin")
            .can_disable(true)
            // Ships enabled. This is the value the boot seed has written since
            // it was introduced; the declaration used to say `false` and only
            // the seed table was read, so nothing observed the disagreement.
            // Whether legal pages *should* default off is a product decision
            // (spec 5.6), and changing this line now re-seeds every row that
            // an admin has not toggled.
            .default_enabled(true)
    },
    handle: |this, ctx, mut msg, input| {
        // Auth is enforced centrally by `route_to_block` from the declared
        // endpoint `AuthLevel` (public reads, admin everything else) — the
        // block holds no `is_admin` preamble. Dispatch matches the same
        // declared templates, extracting `{id}` into `req.param.id`.
        let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
            return ui::not_found_response(&msg);
        };
        match route {
            Route::PublicTerms => this.handle_get_public(ctx, DocumentType::Terms).await,
            Route::PublicPrivacy => this.handle_get_public(ctx, DocumentType::Privacy).await,
            Route::EditorPrivacy => pages::editor_page(ctx, &msg, DocumentType::Privacy).await,
            Route::EditorTerms => pages::editor_page(ctx, &msg, DocumentType::Terms).await,
            Route::SettingsPage => pages::settings_page(ctx, &msg).await,
            Route::EndpointsPage => pages::endpoints_page(ctx, &msg).await,
            Route::AdminSave => pages::handle_save(ctx, &msg, input).await,
            Route::AdminRenderPreview => pages::handle_render_preview(ctx, input).await,
            Route::AdminPublish => pages::handle_publish(ctx, &msg, input).await,
            Route::AdminSaveSettings => pages::handle_save_settings(ctx, &msg, input).await,
            Route::ApiList => this.handle_admin_list(ctx, &msg).await,
            Route::ApiGet => this.handle_admin_get(ctx, &msg).await,
            Route::ApiCreate => this.handle_admin_create(ctx, &msg, input).await,
            Route::ApiPublish => this.handle_admin_publish(ctx, &msg).await,
            Route::ApiUpdate => this.handle_admin_update(ctx, &msg, input).await,
            Route::ApiDelete => this.handle_admin_delete(ctx, &msg).await,
        }
    },
    lifecycle: |this, ctx, event| {
        crate::migration_helper::lifecycle_init(
            ctx,
            &event,
            "impresspress/legalpages",
            migrations::SQLITE_MIGRATIONS,
            migrations::POSTGRES_MIGRATIONS,
        )
        .await?;
        // Seed the default draft documents after migrations, only on Init.
        if matches!(event.event_type, wafer_run::LifecycleType::Init) {
            this.seed_defaults(ctx).await?;
        }
        Ok(())
    },
}

/// A `TestContext` with the legalpages schema applied. Shared by every test
/// module in the block so the fixture exists once.
#[cfg(test)]
pub(super) async fn test_ctx() -> crate::test_support::TestContext {
    let ctx = crate::test_support::TestContext::with_admin()
        .await
        .running_as(crate::blocks::legalpages::LegalPagesBlock::BLOCK_NAME);
    let sqlite: Vec<&str> = migrations::SQLITE_MIGRATIONS
        .iter()
        .map(|(_, sql)| *sql)
        .collect();
    crate::migration_helper::apply_migrations(
        &ctx,
        "impresspress/legalpages",
        &sqlite,
        migrations::POSTGRES_MIGRATIONS,
    )
    .await
    .expect("apply legalpages migrations");
    ctx
}

/// One stored document in whichever status the test needs, built the way the
/// block builds one: a draft, optionally taken through a status transition.
/// Nothing outside `repo::documents` spells the table.
#[cfg(test)]
pub(super) async fn seed_doc(
    ctx: &dyn Context,
    doc_type: DocumentType,
    title: &str,
    status: contracts::DocumentStatus,
    version: i64,
) -> documents::DocumentRow {
    use contracts::DocumentStatus;

    let draft = documents::insert_draft(
        ctx,
        NewDraft {
            doc_type,
            title,
            content: "body",
            created_by: "seed",
        },
    )
    .await
    .expect("seed draft");

    match status {
        DocumentStatus::Draft => draft,
        DocumentStatus::Published => documents::mark_published(
            ctx,
            &draft.id,
            version,
            &crate::util::now_rfc3339(),
            documents::PublishedContent::default(),
        )
        .await
        .expect("seed published"),
        DocumentStatus::Archived => {
            documents::mark_archived(ctx, &draft.id)
                .await
                .expect("seed archived");
            stored(ctx, &draft.id).await
        }
    }
}

/// The document `id`, which the caller knows exists.
#[cfg(test)]
pub(super) async fn stored(ctx: &dyn Context, id: &str) -> documents::DocumentRow {
    documents::get(ctx, id)
        .await
        .expect("read document")
        .expect("the document exists")
}

/// Every way a legalpages write used to be lost, each pinned as a regression.
///
/// Three of them are review bug B10: the generic PATCH that applied `status`
/// and so bypassed `service::publish_document`, the save handler that read a
/// lookup *error* as "create a new draft", and the Init seed that re-ran on a
/// count error. Two more are the errors the publish path swallowed: a
/// `latest_version` read that answered `0` on failure, and an archive pass
/// that logged its failures at `warn` and answered `200`.
#[cfg(test)]
mod write_loss_tests {
    use wafer_run::{Block as _, InputStream, LifecycleEvent, LifecycleType};

    use super::{contracts::DocumentStatus, *};
    use crate::test_support::{admin_msg, output_http_status, FailingDbOpContext};

    /// Every document of `doc_type` currently in `published`, by id.
    async fn published_ids(ctx: &dyn Context, doc_type: DocumentType) -> Vec<String> {
        let mut ids: Vec<String> = documents::list_published(ctx, doc_type)
            .await
            .expect("list published")
            .into_iter()
            .map(|row| row.id)
            .collect();
        ids.sort();
        ids
    }

    async fn row_count(ctx: &dyn Context) -> i64 {
        documents::count(ctx).await.expect("count rows")
    }

    /// B10, defect 1. `PATCH /b/legalpages/api/documents/{id}` applies the
    /// request body as a column map, so a client can set `status` directly
    /// and skip `service::publish_document` — which is the only code that
    /// archives the previously published sibling. The doc type is then left
    /// with two rows claiming to be published, and the public page shows
    /// whichever sorts first.
    #[tokio::test]
    async fn patch_cannot_write_the_status_column() {
        let ctx = test_ctx().await;
        let live = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Live Terms",
            DocumentStatus::Published,
            3,
        )
        .await;
        let draft = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Draft Terms",
            DocumentStatus::Draft,
            1,
        )
        .await;

        let out = LegalPagesBlock::new()
            .handle(
                &ctx,
                admin_msg(
                    "update",
                    &format!("/b/legalpages/api/documents/{}", draft.id),
                ),
                InputStream::from_bytes(br#"{"status":"published"}"#.to_vec()),
            )
            .await;

        let status = output_http_status(out).await;
        assert_eq!(
            published_ids(&ctx, DocumentType::Terms).await,
            vec![live.id],
            "publish must stay the only transition into `published`"
        );
        assert_eq!(
            status, 400,
            "PATCH must refuse a body that names a column it does not own"
        );
    }

    /// B10, defect 2. `handle_save` mapped the lookup's `Err(_)` onto "create
    /// a new draft", so a transient read failure silently forked the document
    /// the admin was editing into a second row instead of reporting.
    #[tokio::test]
    async fn save_reports_a_failed_lookup_instead_of_forking_the_document() {
        let ctx = test_ctx().await;
        let draft = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Draft Terms",
            DocumentStatus::Draft,
            1,
        )
        .await;

        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.get", documents::TABLE)]);
        let body = serde_json::to_vec(&serde_json::json!({
            "doc_type": "terms",
            "title": "Draft Terms",
            "content": "edited",
            "doc_id": draft.id,
            "version": 1,
        }))
        .expect("serialize save body");

        let out = pages::handle_save(
            &failing,
            &admin_msg("create", "/b/legalpages/admin/save"),
            InputStream::from_bytes(body),
        )
        .await;

        let status = output_http_status(out).await;
        assert_eq!(
            row_count(&ctx).await,
            1,
            "the failed save must not have created a second document"
        );
        assert_eq!(
            status, 500,
            "a failed lookup must be reported, not read as `create a new draft`"
        );
    }

    /// B10, defect 3. `seed_defaults` read its "is the table already seeded?"
    /// count through `unwrap_or(0)`, so a count that *failed* was
    /// indistinguishable from an empty table and Init seeded a duplicate set
    /// of documents on top of the existing ones. It returned `()`, so the
    /// lifecycle could not see the failure either.
    #[tokio::test]
    async fn init_fails_when_the_seed_count_fails() {
        let ctx = test_ctx().await;
        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.count", documents::TABLE)]);

        let result = LegalPagesBlock::new()
            .lifecycle(
                &failing,
                LifecycleEvent {
                    event_type: LifecycleType::Init,
                    data: Vec::new(),
                },
            )
            .await;

        assert_eq!(
            row_count(&ctx).await,
            0,
            "Init must not have seeded on a count it could not read"
        );
        assert!(
            result.is_err(),
            "a count the seed cannot read must fail Init, not read as `table is empty`"
        );
    }

    /// The counterpart of the two `FailingDbOpContext` tests above: on a
    /// healthy context Init still seeds exactly the two default documents,
    /// and running it twice does not duplicate them.
    #[tokio::test]
    async fn init_seeds_the_two_defaults_once() {
        let ctx = test_ctx().await;
        let event = || LifecycleEvent {
            event_type: LifecycleType::Init,
            data: Vec::new(),
        };

        LegalPagesBlock::new()
            .lifecycle(&ctx, event())
            .await
            .expect("first init");
        assert_eq!(row_count(&ctx).await, 2);

        LegalPagesBlock::new()
            .lifecycle(&ctx, event())
            .await
            .expect("second init");
        assert_eq!(row_count(&ctx).await, 2, "Init is idempotent");
    }

    /// A `latest_version` that could not be read used to answer `0`, so the
    /// next publish restarted the type at version 1 — and then, because the
    /// publish itself succeeded, archived the real live document behind it.
    /// The read now reports and nothing moves.
    #[tokio::test]
    async fn a_failed_version_read_stops_the_publish() {
        let ctx = test_ctx().await;
        let live = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Live Terms",
            DocumentStatus::Published,
            5,
        )
        .await;
        let draft = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Next Terms",
            DocumentStatus::Draft,
            1,
        )
        .await;

        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.list", documents::TABLE)]);
        let result = service::publish_document(
            &failing,
            service::PublishRequest {
                doc_type: DocumentType::Terms,
                doc_id: &draft.id,
                title: None,
                content: None,
                version: 0,
                created_by: "admin_1",
            },
        )
        .await;

        assert!(
            result.is_err(),
            "a version read that failed must not be read as `this type has no versions`"
        );
        let untouched = stored(&ctx, &live.id).await;
        assert_eq!(untouched.status, DocumentStatus::Published);
        assert_eq!(untouched.version, 5);
        assert_eq!(stored(&ctx, &draft.id).await.status, DocumentStatus::Draft);
    }

    /// The archive pass runs after the new document is live, so a failure
    /// there leaves the type with two published rows. That used to be a
    /// `warn` and a `200`; it is now the caller's error, because it is a
    /// state an operator has to be told about.
    #[tokio::test]
    async fn a_failed_archive_pass_surfaces() {
        let ctx = test_ctx().await;
        seed_doc(
            &ctx,
            DocumentType::Terms,
            "Live Terms",
            DocumentStatus::Published,
            5,
        )
        .await;
        let draft = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Next Terms",
            DocumentStatus::Draft,
            1,
        )
        .await;

        // The publish itself is the first update; the archive pass is the
        // one that follows it.
        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.update", documents::TABLE)])
                .after_passing(1);
        let result = service::publish_document(
            &failing,
            service::PublishRequest {
                doc_type: DocumentType::Terms,
                doc_id: &draft.id,
                title: None,
                content: None,
                version: 6,
                created_by: "admin_1",
            },
        )
        .await;

        assert!(
            result.is_err(),
            "an archive pass that failed must be reported, not logged and answered 200"
        );
        assert_eq!(
            stored(&ctx, &draft.id).await.status,
            DocumentStatus::Published
        );
    }

    /// The typed PATCH still does what a PATCH is for.
    #[tokio::test]
    async fn patch_updates_the_text_and_nothing_else() {
        let ctx = test_ctx().await;
        let draft = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Draft Terms",
            DocumentStatus::Draft,
            1,
        )
        .await;

        let out = LegalPagesBlock::new()
            .handle(
                &ctx,
                admin_msg(
                    "update",
                    &format!("/b/legalpages/api/documents/{}", draft.id),
                ),
                InputStream::from_bytes(br#"{"title":"Revised Terms"}"#.to_vec()),
            )
            .await;
        assert_eq!(output_http_status(out).await, 200);

        let after = stored(&ctx, &draft.id).await;
        assert_eq!(after.title, "Revised Terms");
        assert_eq!(after.content, draft.content, "content was not sent");
        assert_eq!(after.status, DocumentStatus::Draft);
        assert_eq!(after.version, draft.version);
    }

    /// `version` is refused by name for the same reason `status` is: neither
    /// is a column this endpoint owns.
    #[tokio::test]
    async fn patch_cannot_write_the_version_column() {
        let ctx = test_ctx().await;
        let draft = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Draft Terms",
            DocumentStatus::Draft,
            1,
        )
        .await;

        let out = LegalPagesBlock::new()
            .handle(
                &ctx,
                admin_msg(
                    "update",
                    &format!("/b/legalpages/api/documents/{}", draft.id),
                ),
                InputStream::from_bytes(br#"{"version":99}"#.to_vec()),
            )
            .await;

        assert_eq!(output_http_status(out).await, 400);
        assert_eq!(stored(&ctx, &draft.id).await.version, 1);
    }

    /// The editor's save handler on a *published* document still forks a new
    /// draft rather than editing the live text — the `Ok(Some)` branch that
    /// the three-way match had to keep.
    #[tokio::test]
    async fn saving_a_published_document_creates_a_draft() {
        let ctx = test_ctx().await;
        let live = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Live Terms",
            DocumentStatus::Published,
            2,
        )
        .await;

        let body = serde_json::to_vec(&serde_json::json!({
            "doc_type": "terms",
            "title": "Live Terms",
            "content": "an edit",
            "doc_id": live.id,
            "version": 2,
        }))
        .expect("serialize save body");
        let out = pages::handle_save(
            &ctx,
            &admin_msg("create", "/b/legalpages/admin/save"),
            InputStream::from_bytes(body),
        )
        .await;
        assert_eq!(output_http_status(out).await, 200);

        assert_eq!(row_count(&ctx).await, 2, "a new draft was created");
        let untouched = stored(&ctx, &live.id).await;
        assert_eq!(untouched.status, DocumentStatus::Published);
        assert_eq!(untouched.content, "body", "the live text is untouched");
    }

    /// Saving a *draft* edits it in place. Two saves in a row must leave one
    /// row, not three.
    #[tokio::test]
    async fn saving_a_draft_edits_it_in_place() {
        let ctx = test_ctx().await;
        let draft = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Draft Terms",
            DocumentStatus::Draft,
            1,
        )
        .await;

        for text in ["first edit", "second edit"] {
            let body = serde_json::to_vec(&serde_json::json!({
                "doc_type": "terms",
                "title": "Draft Terms",
                "content": text,
                "doc_id": draft.id,
                "version": 1,
            }))
            .expect("serialize save body");
            let out = pages::handle_save(
                &ctx,
                &admin_msg("create", "/b/legalpages/admin/save"),
                InputStream::from_bytes(body),
            )
            .await;
            assert_eq!(output_http_status(out).await, 200);
        }

        assert_eq!(row_count(&ctx).await, 1);
        assert_eq!(stored(&ctx, &draft.id).await.content, "second edit");
    }

    /// Both surfaces that resolve "the published document of this type" now
    /// go through one repo function, so a type that legacy data left with two
    /// published rows resolves to the same one on both. `version` desc is the
    /// order that answers "the latest published version".
    #[tokio::test]
    async fn the_public_page_and_the_editor_agree_on_which_row_is_published() {
        use crate::test_support::{anon_msg, output_html};

        let ctx = test_ctx().await;
        seed_doc(
            &ctx,
            DocumentType::Terms,
            "Older Terms",
            DocumentStatus::Published,
            1,
        )
        .await;
        seed_doc(
            &ctx,
            DocumentType::Terms,
            "Newer Terms",
            DocumentStatus::Published,
            9,
        )
        .await;

        let public = output_html(
            LegalPagesBlock::new()
                .handle(
                    &ctx,
                    anon_msg("retrieve", "/b/legalpages/terms"),
                    InputStream::from_bytes(Vec::new()),
                )
                .await,
        )
        .await;
        assert!(public.contains("Newer Terms"), "{public}");

        let editor = output_html(
            pages::editor_page(
                &ctx,
                &admin_msg("retrieve", "/b/legalpages/admin/terms"),
                DocumentType::Terms,
            )
            .await,
        )
        .await;
        assert!(editor.contains("Newer Terms"), "{editor}");
    }

    /// The public page's theming comes out of config, so the block has to be
    /// allowed to call the config block.
    ///
    /// Found by the requires sweep that Bug 2 (the files block calling
    /// `wafer-run/crypto` undeclared) prompted: this block reads four config
    /// keys in `handle_get_public` while `info().requires` named only
    /// `wafer-run/database`, so the runtime refused every one of those calls
    /// at the `call_block` boundary and the page rendered unthemed. A refused
    /// read now fails the page instead, so without the entry this test sees
    /// an error rather than an unthemed page.
    ///
    /// The fixture must run as the block (`running_as`) or the bug is
    /// invisible here exactly as it was invisible in CI: the fixture's own
    /// frame is unrestricted.
    #[tokio::test]
    async fn the_public_page_reads_its_theming_config() {
        use crate::test_support::{anon_msg, output_html};

        let mut ctx = test_ctx().await;
        ctx.set_config(BG_COLOR_KEY, "#123456");
        let ctx = ctx.running_as(LegalPagesBlock::BLOCK_NAME);
        seed_doc(
            &ctx,
            DocumentType::Terms,
            "Published Terms",
            DocumentStatus::Published,
            1,
        )
        .await;

        let html = output_html(
            LegalPagesBlock::new()
                .handle(
                    &ctx,
                    anon_msg("retrieve", "/b/legalpages/terms"),
                    InputStream::from_bytes(Vec::new()),
                )
                .await,
        )
        .await;

        assert!(
            html.contains("#123456"),
            "the configured background colour must reach the rendered page: {html}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site() -> SiteConfig {
        SiteConfig {
            app_name: "Acme".to_string(),
            logo_url: String::new(),
            logo_icon_url: String::new(),
            favicon_url: "/favicon.ico".to_string(),
            primary_color: String::new(),
            embedded_scripts: Vec::new(),
            auth_headline: String::new(),
            auth_tagline: String::new(),
        }
    }

    #[test]
    fn render_legal_page_uses_public_page_template() {
        let site_cfg = site();
        let html = render_legal_page(LegalPageInputs {
            site: &site_cfg,
            title: "Terms of Service",
            content: "<p>The terms.</p>",
            version: 3,
            meta: "Last updated: 2026-04-01",
            back_url: "/",
            bg_color: "#fafafa",
            primary_color: "#6366f1",
            custom_footer: "",
        })
        .into_string();

        // Came from the shared template, not bare page chrome in this file.
        // grep-guard-html.sh forbids the page-chrome literals here, so we
        // assert on the public_page wrapper class instead.
        assert!(html.contains(r#"<main class="public-page">"#));
        assert!(html.contains("public-page__head"));
        assert!(html.contains("public-page__content"));
        assert!(html.contains("public-page__version"));
        assert!(html.contains(">v3<"));
        assert!(html.contains("Last updated: 2026-04-01"));
        assert!(html.contains("Terms of Service"));
        assert!(html.contains("The terms."));
        // Color overrides applied as inline custom properties.
        assert!(html.contains("--public-page-bg:#fafafa"));
        assert!(html.contains("--public-page-accent:#6366f1"));
        // Auto footer (year + app name).
        assert!(html.contains("public-page__footer"));
        assert!(html.contains("Acme"));
        assert!(html.contains("All rights reserved"));
        // Standard CSS bundle (not bespoke inline blob).
        assert!(html.contains(r#"<link rel="stylesheet" href="/b/static/app-"#));
    }

    #[test]
    fn render_legal_page_omits_color_inline_when_empty() {
        let site_cfg = site();
        let html = render_legal_page(LegalPageInputs {
            site: &site_cfg,
            title: "Privacy Policy",
            content: "<p>x</p>",
            version: 1,
            meta: "",
            back_url: "/",
            bg_color: "",
            primary_color: "",
            custom_footer: "Custom <strong>footer</strong>",
        })
        .into_string();

        assert!(!html.contains("--public-page-bg"));
        assert!(!html.contains("--public-page-accent"));
        // Custom footer renders verbatim (PreEscaped).
        assert!(html.contains("Custom <strong>footer</strong>"));
    }

    #[test]
    fn render_legal_page_no_meta_section_when_meta_empty_and_version_zero() {
        let site_cfg = site();
        let html = render_legal_page(LegalPageInputs {
            site: &site_cfg,
            title: "x",
            content: "",
            version: 0,
            meta: "",
            back_url: "/",
            bg_color: "",
            primary_color: "",
            custom_footer: "",
        })
        .into_string();
        assert!(!html.contains("public-page__meta"));
    }

    #[test]
    fn markdown_to_html_renders_basic_commonmark() {
        let md = "# Heading\n\nParagraph with **bold** and *italic*.\n\n- one\n- two\n";
        let html = markdown_to_html(md);
        assert!(html.contains("<h1>Heading</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("<em>italic</em>"));
        assert!(html.contains("<ul>"));
        assert!(html.contains("<li>one</li>"));
    }

    #[test]
    fn markdown_to_html_drops_raw_script_tags() {
        // pulldown-cmark default config does NOT pass raw HTML through —
        // the `html` writer treats `<script>` as plain text. Verify that
        // assumption holds (it's the whole reason we ditched ammonia).
        let md = "Hello\n\n<script>alert('xss')</script>\n";
        let html = markdown_to_html(md);
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn markdown_to_html_renders_links_safely() {
        let md = "[OK](https://example.com)\n\n[BAD](javascript:alert(1))\n";
        let html = markdown_to_html(md);
        assert!(html.contains(r#"href="https://example.com""#));
        // pulldown-cmark does not filter javascript: URLs on its own —
        // we filter in markdown_to_html. Verify the filter holds.
        assert!(!html.contains("javascript:"));
    }

    #[test]
    fn markdown_to_html_filters_uppercase_javascript_scheme() {
        let md = "[BAD](JAVASCRIPT:alert(1))\n\n[BAD](JavaScript:alert(1))\n";
        let html = markdown_to_html(md);
        assert!(!html.to_ascii_lowercase().contains("javascript:"));
    }

    #[test]
    fn markdown_to_html_filters_data_and_vbscript_schemes() {
        let md = "[X](data:text/html,<script>alert(1)</script>)\n\n[Y](vbscript:msgbox)\n";
        let html = markdown_to_html(md);
        assert!(!html.contains("data:"));
        assert!(!html.contains("vbscript:"));
    }

    #[test]
    fn markdown_to_html_allows_safe_schemes_and_relative_urls() {
        let md = "[a](https://x.test) [b](http://y.test) [c](mailto:z@x.test) [d](tel:+1234) [e](/local/path) [f](#anchor)\n";
        let html = markdown_to_html(md);
        assert!(html.contains(r#"href="https://x.test""#));
        assert!(html.contains(r#"href="http://y.test""#));
        assert!(html.contains(r#"href="mailto:z@x.test""#));
        assert!(html.contains(r#"href="tel:+1234""#));
        assert!(html.contains(r#"href="/local/path""#));
        assert!(html.contains("href=\"#anchor\""));
    }

    #[test]
    fn render_preview_fragment_returns_rendered_html() {
        let md = "## Section\n\nHello **world**.";
        let html = super::pages::render_preview_fragment(md);
        assert!(html.contains("<h2>Section</h2>"));
        assert!(html.contains("<strong>world</strong>"));
        // Wrapped in the public-page__content div so it picks up the same
        // typography as the live page.
        assert!(html.starts_with(r#"<div class="public-page__content">"#));
    }

    #[test]
    fn editor_page_uses_textarea_not_contenteditable() {
        let markup = super::pages::editor_markup_for_test(
            DocumentType::Terms,
            "doc-123",
            "Terms of Service",
            "# heading\n\nbody",
            Some(contracts::DocumentStatus::Draft),
            "2026-05-19T00:00:00Z",
            1,
        );
        let s = markup.into_string();
        assert!(s.contains("<textarea"), "editor must use <textarea>");
        assert!(!s.contains("contenteditable"), "no contenteditable allowed");
        assert!(s.contains(r#"data-tab="edit""#));
        assert!(s.contains(r#"data-tab="preview""#));
        // Vanilla JS fetch path — the URL lives in EDITOR_JS, reached
        // through the delegated `legalpages-editor-tab` action
        assert!(s.contains("/b/legalpages/admin/render-preview"));
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
        let declared = LegalPagesBlock::new().info().endpoints;
        assert_eq!(declared.len(), ROUTES.len());
        for (ep, row) in declared.iter().zip(ROUTES) {
            assert_eq!(ep.method, row.method, "{}", row.template);
            assert_eq!(ep.path, row.template);
            assert_eq!(ep.auth, row.auth, "{}", row.template);
        }
    }

    /// The JSON handlers read the id the table bound, nothing else: an
    /// unrouted message with an id in its path is refused, and the same
    /// message routed through `ROUTES` binds the id.
    #[tokio::test]
    async fn publish_reads_only_the_bound_id() {
        use crate::test_support::{admin_msg, output_is_error, TestContext};

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::legalpages::LegalPagesBlock::BLOCK_NAME);
        let path = "/b/legalpages/api/documents/doc-7/publish";

        let unrouted = LegalPagesBlock::new()
            .handle_admin_publish(&ctx, &admin_msg("update", path))
            .await;
        assert!(
            output_is_error(unrouted, "InvalidArgument").await,
            "an unrouted message binds no id and must be refused, not parsed"
        );

        let mut msg = admin_msg("update", path);
        assert!(matches!(
            endpoint_match::dispatch(&mut msg, ROUTES),
            Some(Route::ApiPublish)
        ));
        assert_eq!(msg.var("id"), "doc-7");
    }

    /// A WRAP refusal on the document read is a **403**, not the
    /// `500 Internal server error (ref: …)` the old `Err(e) => err_internal`
    /// tail produced. Before `crud::db_error` there was no arm in this repo
    /// that could tell a missing grant from a corrupt row.
    #[tokio::test]
    async fn a_denied_document_read_is_403_not_500() {
        use crate::test_support::{admin_msg, output_http_status, TestContext};

        let ctx = TestContext::with_auth().await.running_as("test/ungranted");
        let mut msg = admin_msg("retrieve", "/b/legalpages/api/documents/doc-7");
        assert!(matches!(
            endpoint_match::dispatch(&mut msg, ROUTES),
            Some(Route::ApiGet)
        ));

        let out = LegalPagesBlock::new().handle_admin_get(&ctx, &msg).await;
        assert_eq!(output_http_status(out).await, 403);
    }

    /// `doc_type` reached the column straight from the request body, and the
    /// two routes that serve a document are hardcoded to `terms` and
    /// `privacy` — so `{"doc_type":"cookies"}` created a row that every
    /// public route 404s and no editor page can reach. It is a 400 now, and
    /// the two spellings the block serves are the two `DocumentType`
    /// defines.
    #[tokio::test]
    async fn a_doc_type_no_route_can_serve_is_refused() {
        use crate::test_support::{admin_msg, output_http_status};

        let ctx = test_ctx().await;
        let body = serde_json::json!({
            "doc_type": "cookies",
            "title": "Cookie Policy",
            "content": "# Cookies",
        });
        let out = LegalPagesBlock::new()
            .handle_admin_create(
                &ctx,
                &admin_msg("create", "/b/legalpages/api/documents"),
                InputStream::from_bytes(serde_json::to_vec(&body).expect("body")),
            )
            .await;
        assert_eq!(output_http_status(out).await, 400);
        assert_eq!(
            documents::count(&ctx).await.expect("count"),
            0,
            "an unservable doc_type must not reach the table"
        );

        // The two the block does serve still create.
        for doc_type in ["terms", "privacy"] {
            let body = serde_json::json!({
                "doc_type": doc_type,
                "title": "T",
                "content": "# T",
            });
            let out = LegalPagesBlock::new()
                .handle_admin_create(
                    &ctx,
                    &admin_msg("create", "/b/legalpages/api/documents"),
                    InputStream::from_bytes(serde_json::to_vec(&body).expect("body")),
                )
                .await;
            assert_eq!(output_http_status(out).await, 200, "{doc_type} must create");
        }
    }

    /// The editor's save and publish handlers take `doc_type` from their own
    /// body, so they are a second door onto the same column.
    #[tokio::test]
    async fn the_editor_refuses_a_doc_type_no_route_can_serve() {
        use crate::test_support::{admin_msg, output_http_status};

        let ctx = test_ctx().await;
        let body = serde_json::to_vec(&serde_json::json!({
            "doc_type": "cookies",
            "title": "Cookie Policy",
            "content": "# Cookies",
        }))
        .expect("body");

        let saved = pages::handle_save(
            &ctx,
            &admin_msg("create", "/b/legalpages/admin/save"),
            InputStream::from_bytes(body.clone()),
        )
        .await;
        assert_eq!(output_http_status(saved).await, 400);

        let published = pages::handle_publish(
            &ctx,
            &admin_msg("create", "/b/legalpages/admin/publish"),
            InputStream::from_bytes(body),
        )
        .await;
        assert_eq!(output_http_status(published).await, 400);
        assert_eq!(documents::count(&ctx).await.expect("count"), 0);
    }
}
