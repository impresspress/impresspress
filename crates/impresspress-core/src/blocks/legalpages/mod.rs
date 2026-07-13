pub(crate) mod migrations;
mod pages;
mod service;

use maud::{html, Markup, PreEscaped};
use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{
    context::Context, BlockEndpoint, BlockInfo, ConfigVar, ErrorCode, HttpMethod, InputStream,
    InputType, InstanceMode, Message, OutputStream,
};

use crate::{
    blocks::crud,
    endpoint_match::{self, EndpointRoute},
    http::{err_bad_request, err_internal, err_not_found, ok_json, ResponseBuilder},
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

/// Method + path-template dispatch table, mirroring `info().endpoints`. The
/// JSON `.../{id}/publish` template precedes the generic `.../{id}` so the
/// specific publish route wins (replacing the old `ends_with("/publish")`
/// guard). The JSON publish is a PATCH (`update`) on the wire, matching the
/// handler's historical dispatch.
const ROUTES: &[EndpointRoute<Route>] = &[
    EndpointRoute::new(HttpMethod::Get, "/b/legalpages/terms", Route::PublicTerms),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/legalpages/privacy",
        Route::PublicPrivacy,
    ),
    EndpointRoute::new(HttpMethod::Get, "/b/legalpages/admin", Route::EditorPrivacy),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/legalpages/admin/privacy",
        Route::EditorPrivacy,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/legalpages/admin/terms",
        Route::EditorTerms,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/legalpages/admin/settings",
        Route::SettingsPage,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/legalpages/admin/endpoints",
        Route::EndpointsPage,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/legalpages/admin/save",
        Route::AdminSave,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/legalpages/admin/render-preview",
        Route::AdminRenderPreview,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/legalpages/admin/publish",
        Route::AdminPublish,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/legalpages/admin/settings",
        Route::AdminSaveSettings,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/legalpages/api/documents",
        Route::ApiList,
    ),
    EndpointRoute::new(
        HttpMethod::Post,
        "/b/legalpages/api/documents",
        Route::ApiCreate,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/b/legalpages/api/documents/{id}/publish",
        Route::ApiPublish,
    ),
    EndpointRoute::new(
        HttpMethod::Get,
        "/b/legalpages/api/documents/{id}",
        Route::ApiGet,
    ),
    EndpointRoute::new(
        HttpMethod::Patch,
        "/b/legalpages/api/documents/{id}",
        Route::ApiUpdate,
    ),
    EndpointRoute::new(
        HttpMethod::Delete,
        "/b/legalpages/api/documents/{id}",
        Route::ApiDelete,
    ),
];

/// The legalpages block's own declared config vars. Single source of truth for
/// both `BlockInfo::config_keys` and the admin settings page (rendered via
/// `ui::settings_form`, not a parallel tuple table that had drifted on the
/// `BG_COLOR` default).
pub(crate) fn config_vars() -> Vec<ConfigVar> {
    vec![
        ConfigVar::new(
            "IMPRESSPRESS__LEGALPAGES__BG_COLOR",
            "Background color for public legal pages (empty = use design token default)",
            "",
        )
        .name("Background Color")
        .input_type(InputType::Color)
        .optional(),
        ConfigVar::new(
            "IMPRESSPRESS__LEGALPAGES__BACK_URL",
            "Back button URL in the header (e.g., your website homepage)",
            "/",
        )
        .name("Back Button URL")
        .input_type(InputType::Url),
        ConfigVar::new(
            "IMPRESSPRESS__LEGALPAGES__FOOTER",
            "Custom footer text (HTML allowed)",
            "",
        )
        .name("Footer Text")
        .input_type(InputType::Textarea)
        .optional(),
    ]
}

pub(crate) const COLLECTION: &str = "impresspress__legalpages__documents";

/// Path prefix preceding the document id in the JSON API routes.
const API_DOC_PREFIX: &str = "/b/legalpages/api/documents/";

impl LegalPagesBlock {
    async fn handle_get_public(&self, ctx: &dyn Context, doc_type: &str) -> OutputStream {
        use wafer_core::clients::config;

        let site = SiteConfig::load(ctx).await;
        let bg_color = config::get_default(ctx, "IMPRESSPRESS__LEGALPAGES__BG_COLOR", "").await;
        let back_url = config::get_default(ctx, "IMPRESSPRESS__LEGALPAGES__BACK_URL", "/").await;
        let custom_footer = config::get_default(ctx, "IMPRESSPRESS__LEGALPAGES__FOOTER", "").await;
        let primary_color = config::get_default(ctx, "WAFER_RUN_SHARED__PRIMARY_COLOR", "").await;

        let type_label = if doc_type == "terms" {
            "Terms of Service"
        } else {
            "Privacy Policy"
        };

        let opts = ListOptions {
            filters: vec![
                Filter {
                    field: "doc_type".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::Value::String(doc_type.to_string()),
                },
                Filter {
                    field: "status".to_string(),
                    operator: FilterOp::Equal,
                    value: serde_json::Value::String("published".to_string()),
                },
            ],
            sort: vec![SortField {
                field: "version".to_string(),
                desc: true,
            }],
            limit: 1,
            ..Default::default()
        };

        let result = match db::list(ctx, COLLECTION, &opts).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "legalpages: db list failed");
                return err_internal("Database error", e);
            }
        };

        let (title, content, version, meta) = if result.records.is_empty() {
            (
                type_label.to_string(),
                markdown_to_html("No document has been published yet."),
                1_i64,
                String::new(),
            )
        } else {
            let record = &result.records[0];
            let title = record
                .data
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or(type_label)
                .to_string();
            let raw_content = record
                .data
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let content = markdown_to_html(raw_content);
            let published_at = record
                .data
                .get("published_at")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let version = service::doc_version(record).unwrap_or(1);
            let meta = if !published_at.is_empty() {
                format!(
                    "Last updated: {}",
                    published_at.get(..10).unwrap_or(published_at),
                )
            } else {
                String::new()
            };
            (title, content, version, meta)
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
        // Not a pure-CRUD list: it sorts by `updated_at` desc (editors expect
        // most-recently-touched first), whereas `crud::crud_list` is fixed to
        // `created_at` desc. Kept inline rather than widening the shared
        // helper's signature — `blocks::crud` is owned by the products package.
        let (_, page_size, offset) = msg.pagination_params(20);
        let doc_type = msg.query("type");
        let mut filters = Vec::new();
        if !doc_type.is_empty() {
            filters.push(Filter {
                field: "doc_type".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(doc_type.to_string()),
            });
        }
        let opts = ListOptions {
            filters,
            sort: vec![SortField {
                field: "updated_at".to_string(),
                desc: true,
            }],
            limit: page_size as i64,
            offset: offset as i64,
            skip_count: false,
            ..Default::default()
        };
        match db::list(ctx, COLLECTION, &opts).await {
            Ok(result) => ok_json(&result),
            Err(e) => err_internal("Database error", e),
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
            doc_type: String,
            title: String,
            content: String,
        }
        let raw = input.collect_to_bytes().await;
        let body: CreateDoc = match serde_json::from_slice(&raw) {
            Ok(b) => b,
            Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
        };

        // Draft shape lives in `service::create_draft` (shared with the
        // admin editor's save handler in `pages.rs`).
        match service::create_draft(
            ctx,
            &body.doc_type,
            &body.title,
            &body.content,
            msg.user_id(),
        )
        .await
        {
            Ok(record) => ok_json(&record),
            Err(e) => err_internal("Database error", e),
        }
    }

    async fn handle_admin_publish(&self, ctx: &dyn Context, msg: &Message) -> OutputStream {
        let id = crate::util::path_param(msg, "id", API_DOC_PREFIX);
        if id.is_empty() {
            return err_bad_request("Missing document ID");
        }

        // Fetch the document first: its `doc_type` drives version
        // computation and which published siblings get archived.
        let doc = match db::get(ctx, COLLECTION, id).await {
            Ok(r) => r,
            Err(e) if e.code == ErrorCode::NotFound => return err_not_found("Document not found"),
            Err(e) => return err_internal("Database error", e),
        };
        let doc_type = doc
            .data
            .get("doc_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match service::publish_document(
            ctx,
            service::PublishRequest {
                doc_type,
                doc_id: id,
                title: None,
                content: None,
                version: 0,
                created_by: msg.user_id(),
            },
        )
        .await
        {
            Ok(published) => ok_json(&published.record),
            Err(e) => err_internal("Database error", e),
        }
    }

    async fn seed_defaults(&self, ctx: &dyn Context) {
        let count = db::count(ctx, COLLECTION, &[]).await.unwrap_or(0);
        if count > 0 {
            return;
        }

        for (doc_type, title, content) in &[
            (
                "terms",
                "Terms of Service",
                "These are the default terms of service. Please update them in the admin panel.\n",
            ),
            (
                "privacy",
                "Privacy Policy",
                "This is the default privacy policy. Please update it in the admin panel.\n",
            ),
        ] {
            // Seed through the same service fn both publish surfaces use —
            // the published-document shape (status/version/published_at/…)
            // exists exactly once, in `service.rs`. The table is empty here
            // (count == 0 above), so the archive pass is a no-op.
            if let Err(e) = service::publish_document(
                ctx,
                service::PublishRequest {
                    doc_type,
                    doc_id: "",
                    title: Some(title),
                    content: Some(content),
                    version: 1,
                    created_by: "system",
                },
            )
            .await
            {
                tracing::warn!("Failed to seed default legal page '{doc_type}': {e}");
            }
        }
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
        use wafer_run::AuthLevel;

        BlockInfo::new("impresspress/legalpages", "0.0.1", "http-handler@v1", "Legal pages management with versioning and publishing")
            .instance_mode(InstanceMode::Singleton)
            .requires(vec!["wafer-run/database".into()])
            .category(wafer_run::BlockCategory::Feature)
            .description("Legal document management with versioning and publishing. Create and manage terms of service, privacy policies, and other legal documents. Supports draft/published workflow with version tracking.")
            // The admin SSR sub-pages and mutations are declared in full so
            // the central router enforces their `Admin` tier from the declared
            // `AuthLevel` — not merely from the `/b/legalpages/admin` prefix's
            // route-table ordering, which was the sole gate before (the #1
            // regression hazard this package closes).
            .endpoints(vec![
                BlockEndpoint::get("/b/legalpages/terms").summary("Published terms of service"),
                BlockEndpoint::get("/b/legalpages/privacy").summary("Published privacy policy"),
                BlockEndpoint::get("/b/legalpages/admin").summary("Admin editor (privacy)").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/legalpages/admin/privacy").summary("Admin editor (privacy)").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/legalpages/admin/terms").summary("Admin editor (terms)").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/legalpages/admin/settings").summary("Admin settings page").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/legalpages/admin/endpoints").summary("Endpoints reference").auth(AuthLevel::Admin),
                BlockEndpoint::post("/b/legalpages/admin/save").summary("Save draft").auth(AuthLevel::Admin),
                BlockEndpoint::post("/b/legalpages/admin/render-preview").summary("Render markdown preview").auth(AuthLevel::Admin),
                BlockEndpoint::post("/b/legalpages/admin/publish").summary("Publish from editor").auth(AuthLevel::Admin),
                BlockEndpoint::post("/b/legalpages/admin/settings").summary("Save settings").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/legalpages/api/documents").summary("List documents").auth(AuthLevel::Admin),
                BlockEndpoint::post("/b/legalpages/api/documents").summary("Create document").auth(AuthLevel::Admin),
                BlockEndpoint::get("/b/legalpages/api/documents/{id}").summary("Get document").auth(AuthLevel::Admin),
                BlockEndpoint::patch("/b/legalpages/api/documents/{id}/publish").summary("Publish document").auth(AuthLevel::Admin),
                BlockEndpoint::patch("/b/legalpages/api/documents/{id}").summary("Update document").auth(AuthLevel::Admin),
                BlockEndpoint::delete("/b/legalpages/api/documents/{id}").summary("Delete document").auth(AuthLevel::Admin),
            ])
            .config_keys(config_vars())
            .admin_url("/b/legalpages/admin")
            .can_disable(true)
            .default_enabled(false)
    },
    handle: |this, ctx, msg, input| {
        // Auth is enforced centrally by `route_to_block` from the declared
        // endpoint `AuthLevel` (public reads, admin everything else) — the
        // block holds no `is_admin` preamble. Dispatch matches the same
        // declared templates, extracting `{id}` into `req.param.id`.
        let Some(route) = endpoint_match::dispatch(&mut msg, ROUTES) else {
            return ui::not_found_response(&msg);
        };
        match route {
            Route::PublicTerms => this.handle_get_public(ctx, "terms").await,
            Route::PublicPrivacy => this.handle_get_public(ctx, "privacy").await,
            Route::EditorPrivacy => pages::editor_page(ctx, &msg, "privacy").await,
            Route::EditorTerms => pages::editor_page(ctx, &msg, "terms").await,
            Route::SettingsPage => pages::settings_page(ctx, &msg).await,
            Route::EndpointsPage => pages::endpoints_page(ctx, &msg).await,
            Route::AdminSave => pages::handle_save(ctx, &msg, input).await,
            Route::AdminRenderPreview => pages::handle_render_preview(ctx, input).await,
            Route::AdminPublish => pages::handle_publish(ctx, &msg, input).await,
            Route::AdminSaveSettings => pages::handle_save_settings(ctx, input).await,
            Route::ApiList => this.handle_admin_list(ctx, &msg).await,
            Route::ApiGet => {
                crud::crud_get(ctx, &msg, COLLECTION, API_DOC_PREFIX, "Document").await
            }
            Route::ApiCreate => this.handle_admin_create(ctx, &msg, input).await,
            Route::ApiPublish => this.handle_admin_publish(ctx, &msg).await,
            Route::ApiUpdate => {
                crud::crud_update(ctx, &msg, input, COLLECTION, API_DOC_PREFIX, "Document").await
            }
            Route::ApiDelete => {
                crud::crud_delete(ctx, &msg, COLLECTION, API_DOC_PREFIX, "Document").await
            }
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
            this.seed_defaults(ctx).await;
        }
        Ok(())
    },
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
            "terms",
            "doc-123",
            "Terms of Service",
            "# heading\n\nbody",
            "draft",
            "2026-05-19T00:00:00Z",
            1,
        );
        let s = markup.into_string();
        assert!(s.contains("<textarea"), "editor must use <textarea>");
        assert!(!s.contains("contenteditable"), "no contenteditable allowed");
        assert!(s.contains(r#"data-tab="edit""#));
        assert!(s.contains(r#"data-tab="preview""#));
        // Vanilla JS fetch path — URL lives in EDITOR_JS / onclick handler
        assert!(s.contains("/b/legalpages/admin/render-preview"));
    }
}
