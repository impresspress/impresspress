//! SSR admin pages for the legal pages block.
//!
//! The Legal section of the admin: the Privacy Policy and Terms of Service
//! Markdown editors, the public pages' settings, and the endpoints reference.

use maud::{html, Markup, PreEscaped};
use wafer_run::{context::Context, AuthLevel, InputStream, Message, OutputStream, WaferError};

use super::{
    contracts::{DocumentStatus, DocumentType, DOCUMENT_FIELDS},
    repo::documents::{self, DocumentRow, NewDraft},
    service,
};
use crate::{
    blocks::crud,
    http::{err_bad_request, ok_json, ResponseBuilder},
    ui::{
        self,
        components::{self, Badge, BadgeVariant, DataTable, TableCol, TableRow},
        icons, settings_form,
    },
    util::wire_str,
};

// ---------------------------------------------------------------------------
// Section links
// ---------------------------------------------------------------------------

/// The block's admin sections, one link each, `active` marked current —
/// rendered above every legal admin page ([`ui::PageBody::with_subnav`]).
fn section_links(active: &str) -> Markup {
    let tab = |key: &str, href: &'static str, label: &'static str| components::Tab {
        active: active == key,
        href,
        label,
        icon: None,
    };
    components::subnav(
        "Legal pages sections",
        vec![
            tab("privacy", "/b/legalpages/admin/privacy", "Privacy"),
            tab("terms", "/b/legalpages/admin/terms", "Terms"),
            tab("settings", "/b/legalpages/admin/settings", "Settings"),
            tab("endpoints", "/b/legalpages/admin/endpoints", "Endpoints"),
        ],
    )
}

/// A legal admin page whose read failed: drawn in the shell under the
/// block's section links, with a link back to the dashboard.
async fn error_page(
    ctx: &dyn Context,
    msg: &Message,
    title: &str,
    section: &str,
    error: WaferError,
    context: &str,
) -> OutputStream {
    ui::shell_error_page(
        ctx,
        msg,
        ui::Shell::admin(title, title),
        Some(section_links(section)),
        ui::BackLink::ADMIN_DASHBOARD,
        error,
        context,
    )
    .await
}

// ---------------------------------------------------------------------------
// Editor state
// ---------------------------------------------------------------------------

/// What the editor shows for one document type.
pub(super) struct EditorState {
    /// The version being edited: the newest draft (so the admin sees their
    /// in-progress edits), else the published version; `None` when the type
    /// has no row at all.
    pub current: Option<DocumentRow>,
    /// The version number the public page shows, if one is published. A
    /// draft has no number of its own: it is stored as version 0
    /// until it is published.
    pub live_version: Option<i64>,
    /// What the next publish will be numbered: one past the highest version of
    /// this type in any status — the number `service::publish_document`
    /// would pick for an unnumbered publish.
    pub next_version: i64,
}

/// Read the editor's state.
///
/// A read failure is an `Err`, not an empty editor: rendering "no document"
/// over a database error invites the admin to type a replacement into a form
/// whose save then forks the document they could not see.
pub(super) async fn load_editor_state(
    ctx: &dyn Context,
    doc_type: DocumentType,
) -> Result<EditorState, WaferError> {
    let published = documents::find_published(ctx, doc_type).await?;
    let draft = documents::find_latest_draft(ctx, doc_type).await?;
    let next_version = documents::latest_version(ctx, doc_type).await? + 1;
    Ok(EditorState {
        live_version: published.as_ref().map(|row| row.version),
        current: draft.or(published),
        next_version,
    })
}

// ---------------------------------------------------------------------------
// Editor page (Privacy / Terms)
// ---------------------------------------------------------------------------

pub async fn editor_page(ctx: &dyn Context, msg: &Message, doc_type: DocumentType) -> OutputStream {
    let state = match load_editor_state(ctx, doc_type).await {
        Ok(state) => state,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                doc_type.title(),
                wire_str(&doc_type).as_str(),
                e,
                "legal editor page: document read failed",
            )
            .await
        }
    };
    let view = editor_view(doc_type, &state);
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::admin(doc_type.title(), doc_type.title()).actions(view.actions),
        ui::PageBody::from(view.body).with_subnav(section_links(wire_str(&doc_type).as_str())),
    )
    .await
}

/// The editor page's two halves: the page-level actions (open, save,
/// publish), which ride in the shell topbar, and the body.
pub(super) struct EditorView {
    pub actions: Vec<Markup>,
    pub body: Markup,
}

/// The editor's script: delegated listeners, each scoped to the editor being
/// on screen (see the file's header).
const EDITOR_JS: &str = include_str!("assets/editor.js");

/// Build the editor. Split out from `editor_page` so it can be unit-tested
/// without a `Context`.
///
/// The document's name is the topbar's `h1` and the Title field's value, and
/// nowhere else: the status row says what state the document is in, not what
/// it is called.
pub(super) fn editor_view(doc_type: DocumentType, state: &EditorState) -> EditorView {
    let doc = state.current.as_ref();
    let started = doc.is_some();
    let name = doc_type.title();
    let lower = name.to_lowercase();
    let title = doc
        .map(|d| d.title.as_str())
        .filter(|t| !t.is_empty())
        .unwrap_or(name);

    let (variant, status) = match doc.map(|d| d.status) {
        Some(DocumentStatus::Published) => (BadgeVariant::Success, "Published"),
        Some(DocumentStatus::Draft) => (BadgeVariant::Warning, "Draft"),
        Some(DocumentStatus::Archived) => (BadgeVariant::Secondary, "Archived"),
        None => (BadgeVariant::Secondary, "Not saved"),
    };

    // The page's actions live in the topbar (the page header); the delegated
    // listener in EDITOR_JS finds them by id wherever they render. Save and
    // Publish wait for the empty state's "Write …" — there is nothing to
    // save before it. Primary action last.
    let actions = vec![
        html! {
            a .btn .btn--sm .btn--ghost
                href={"/b/legalpages/" (wire_str(&doc_type))}
                target="_blank" rel="noopener"
            {
                "Open public page"
                span .sr-only { " (opens in a new tab)" }
            }
        },
        html! {
            button #btn-save .btn .btn--sm .btn--secondary type="button"
                data-action="legalpages-save"
                data-legal-editor-action
                hidden[!started]
                aria-keyshortcuts="Control+S Meta+S"
                title="Save draft (Ctrl+S)"
            { "Save draft" }
        },
        html! {
            button #btn-publish .btn .btn--sm .btn--primary type="button"
                data-action="legalpages-publish"
                data-legal-editor-action
                hidden[!started]
            { "Publish" }
        },
    ];

    let body = html! {
        @if !started {
            div #legal-editor-empty {
                (components::empty_state(
                    icons::file_text(),
                    &format!("No {lower} yet"),
                    "Nothing has been saved or published yet, so the public page shows a \
                     placeholder until you publish.",
                    Some(html! {
                        button .btn .btn--primary type="button" data-action="legalpages-start" {
                            "Write the " (lower)
                        }
                    }),
                ))
            }
        }

        div #legal-editor
            hidden[!started]
            data-doc-type=(wire_str(&doc_type))
            data-doc-id=(doc.map(|d| d.id.as_str()).unwrap_or(""))
            data-save-url="/b/legalpages/admin/save"
            data-publish-url="/b/legalpages/admin/publish"
            data-preview-url="/b/legalpages/admin/render-preview"
        {
            div .legal-editor__status {
                span #document-status { (components::badge(variant, status)) }
                span #live-version .legal-editor__meta {
                    @match state.live_version {
                        Some(v) => { "Live: v" (v) }
                        None => "Not published yet",
                    }
                }
                span #next-version .legal-editor__meta { "Publishes as v" (state.next_version) }
                span #saved-at .legal-editor__meta {
                    @if let Some(d) = doc {
                        "Saved " (components::timestamp(&d.updated_at))
                    }
                }
            }

            div .form-group {
                label .form-label for="title-input" { "Title" }
                input #title-input .form-input type="text" name="title"
                    value=(title) required autocomplete="off"
                    aria-describedby="title-input-hint";
                p #title-input-hint .form-hint { "The heading of the public page." }
            }

            div .legal-editor__content-head {
                label .form-label for="editor" { "Content" }
                div .editor-tabs role="tablist" aria-label="Content view" {
                    button #editor-tab-edit .editor-tab .editor-tab--active type="button"
                        role="tab" aria-selected="true" aria-controls="editor-edit-pane"
                        data-tab="edit" data-action="legalpages-editor-tab"
                    { "Edit" }
                    button #editor-tab-preview .editor-tab type="button"
                        role="tab" aria-selected="false" aria-controls="editor-preview-pane"
                        tabindex="-1"
                        data-tab="preview" data-action="legalpages-editor-tab"
                    { "Preview" }
                }
            }

            div #editor-edit-pane .editor-pane role="tabpanel" aria-labelledby="editor-tab-edit" {
                textarea #editor .editor-textarea name="content"
                    aria-describedby="editor-hint"
                { (content_of(doc)) }
            }
            div #editor-preview-pane .editor-pane role="tabpanel"
                aria-labelledby="editor-tab-preview" tabindex="0" hidden
            {
                div #editor-preview .preview-content aria-live="polite" {}
            }
            p #editor-hint .form-hint {
                "Markdown: " code { "## Section" } ", " code { "**bold**" } ", "
                code { "- item" } ", " code { "[text](https://…)" } ". The title is "
                "already the page heading, so start with your first section."
            }
        }

        script { (PreEscaped(EDITOR_JS)) }
    };
    EditorView { actions, body }
}

/// The editor textarea's text: the current version's Markdown, or nothing.
fn content_of(doc: Option<&DocumentRow>) -> &str {
    doc.map(|d| d.content.as_str()).unwrap_or("")
}

// ---------------------------------------------------------------------------
// Endpoints page
// ---------------------------------------------------------------------------

pub async fn endpoints_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    ui::shell_page(
        ctx,
        msg,
        ui::Shell::admin("Endpoints", "Endpoints")
            .subtitle("Every HTTP endpoint the legal pages block serves"),
        ui::PageBody::from(endpoints_view()).with_subnav(section_links("endpoints")),
    )
    .await
}

const ENDPOINT_COLUMNS: [TableCol<'static>; 3] = [
    TableCol::new("Path").primary().width("45%"),
    TableCol::new("Method").width("7rem"),
    TableCol::new("Description"),
];

const FIELD_COLUMNS: [TableCol<'static>; 3] = [
    TableCol::new("Field").primary().width("25%"),
    TableCol::new("Type"),
    TableCol::new("Description"),
];

/// The endpoints reference, generated from the block's `ROUTES` — the table
/// `handle()` dispatches on — so it lists what the block serves, grouped by
/// who may call it, and cannot drift from it.
pub(super) fn endpoints_view() -> Markup {
    let groups = [
        (
            AuthLevel::Public,
            "Public",
            "Anyone can open these; they return the published pages as HTML.",
        ),
        (
            AuthLevel::Authenticated,
            "Signed in",
            "These need a signed-in user.",
        ),
        (
            AuthLevel::Admin,
            "Admin",
            "These need an admin session or an admin bearer token.",
        ),
    ];
    let fields: Vec<TableRow> = DOCUMENT_FIELDS
        .iter()
        .map(|(field, ty, description)| {
            TableRow::new(vec![
                html! { code { (field) } },
                html! { (ty) },
                html! { (description) },
            ])
        })
        .collect();
    html! {
        @for (level, title, description) in groups {
            @let rows: Vec<TableRow> = super::ROUTES
                .iter()
                .filter(|route| route.auth == level)
                .map(|route| TableRow::new(vec![
                    html! { code .cell-wrap { (route.template) } },
                    Badge::new(BadgeVariant::for_method(route.method))
                        .render(html! { (route.method) }),
                    html! { (route.summary) },
                ]))
                .collect();
            @if !rows.is_empty() {
                section .legal-endpoints__group {
                    (components::section_header(title, None))
                    p .text-muted .text-sm .mb-3 { (description) }
                    (DataTable::new(&ENDPOINT_COLUMNS).rows(rows).render())
                }
            }
        }
        section .legal-endpoints__group {
            (components::section_header("Document fields", None))
            p .text-muted .text-sm .mb-3 {
                "Each record the JSON API returns carries these fields in its "
                code { "data" } "."
            }
            (DataTable::new(&FIELD_COLUMNS).rows(fields).render())
        }
    }
}

// ---------------------------------------------------------------------------
// Save / Publish handlers
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct SaveRequest {
    /// Typed, so the editor cannot save a document the block has no route
    /// to serve. Two other doors onto the same column — the JSON create in
    /// `mod.rs` and `handle_publish` below — are typed for the same reason.
    doc_type: DocumentType,
    title: String,
    content: String,
    #[serde(default)]
    doc_id: String,
}

/// Save the editor's text as a draft, by the one edit rule
/// (`service::edit_text`): a draft is edited in place; a published or
/// archived version stays as it is and the text becomes a new draft.
pub async fn handle_save(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: SaveRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        // Previously returned 200 OK with an `error` key — htmx clients
        // would still treat that as success. Use the proper 4xx so the
        // caller can branch on status alone.
        Err(e) => return err_bad_request(&format!("Invalid request: {e}")),
    };

    // Three outcomes, not two. The lookup used to fold its `Err` into "no
    // such document, create a draft", so a transient read failure forked the
    // document the admin was editing into a second row and answered 200
    // (B10). An error is now reported; only a genuinely absent row, or a
    // *published* one, creates a draft.
    let existing = if body.doc_id.is_empty() {
        None
    } else {
        match documents::get(ctx, &body.doc_id).await {
            Ok(found) => found,
            Err(e) => return crud::db_error_internal(e, "Failed to load the legal-page document"),
        }
    };

    // An existing row goes through the one edit rule (a draft is edited in
    // place, a published or archived version is never changed and the edit
    // becomes a new draft); no row starts the type's first draft.
    let saved = match existing {
        Some(doc) if doc.doc_type != body.doc_type => {
            return err_bad_request("The document is not of the type being saved")
        }
        Some(doc) => {
            match service::edit_text(
                ctx,
                &doc,
                Some(&body.title),
                Some(&body.content),
                msg.user_id(),
            )
            .await
            {
                Ok(draft) => draft,
                Err(e) => return super::edit_failed(e),
            }
        }
        None => match documents::insert_draft(
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
            Ok(draft) => draft,
            Err(e) => return crud::db_error_internal(e, "Failed to save legal-page draft"),
        },
    };

    ok_json(&serde_json::json!({
        "doc_id": saved.id,
        "status": DocumentStatus::Draft,
        "message": "Draft saved"
    }))
}

/// Save and publish a document as the next version of its type, archiving
/// the one it replaces (`service::publish_document` numbers it and decides
/// whether the row is published in place or as a new row).
pub async fn handle_publish(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: SaveRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        // Previously returned 200 OK with an `error` key — clients would
        // still treat that as success. Use the proper 4xx so the caller
        // can branch on status alone (matches `handle_save`).
        Err(e) => return err_bad_request(&format!("Invalid request: {e}")),
    };

    let published = match service::publish_document(
        ctx,
        service::PublishRequest {
            doc_type: body.doc_type,
            doc_id: &body.doc_id,
            title: Some(&body.title),
            content: Some(&body.content),
            created_by: msg.user_id(),
        },
    )
    .await
    {
        Ok(p) => p,
        Err(e) => return super::publish_failed(e),
    };

    ok_json(&serde_json::json!({
        "doc_id": published.row.id,
        "status": DocumentStatus::Published,
        "version": published.version,
        "message": format!("Published as v{}", published.version)
    }))
}

// ---------------------------------------------------------------------------
// Settings page
// ---------------------------------------------------------------------------

pub async fn settings_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let vars = super::config_vars();
    let sections = [settings_form::SettingsSection::new("Appearance", &vars)];

    // The live-preview links ride in the form's `extra` slot so they stay
    // inside the settings form (above the Save button), as before.
    let preview = html! {
        section .card .mt-6 .mb-5 .p-4 {
            (components::section_header("Preview", None))
            p .text-muted .text-xs .mb-3 {
                "See how your changes look on the public pages."
            }
            div .flex .gap-2 {
                a .btn .btn--sm .btn--ghost href="/b/legalpages/privacy" target="_blank" rel="noopener" {
                    (icons::eye()) " Privacy Policy"
                    span .sr-only { " (opens in a new tab)" }
                }
                a .btn .btn--sm .btn--ghost href="/b/legalpages/terms" target="_blank" rel="noopener" {
                    (icons::eye()) " Terms of Service"
                    span .sr-only { " (opens in a new tab)" }
                }
            }
        }
    };

    let saved = msg.query("saved") == "1";
    let form = match settings_form::settings_form(ctx, SETTINGS_SAVE_PATH, &sections, preview).await
    {
        Ok(form) => form,
        Err(e) => {
            return error_page(
                ctx,
                msg,
                "Legal settings",
                "settings",
                e,
                "legalpages settings: current values read failed",
            )
            .await
        }
    };

    let content = html! {
        @if saved {
            div .alert .alert--success .mb-4 {
                span aria-hidden="true" { (icons::check()) }
                "Settings saved successfully."
            }
        }

        (form)
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::admin("Legal settings", "Legal settings")
            .subtitle("Customize the public legal pages appearance"),
        ui::PageBody::from(content).with_subnav(section_links("settings")),
    )
    .await
}

/// Where the settings form posts: the admin block, which saves every
/// settings form (`admin::pages::legal_settings`) — one save path, in the one
/// frame WRAP lets write any key. The page stays in the Legal section.
pub(crate) const SETTINGS_SAVE_PATH: &str = "/b/admin/settings/legal";

// ---------------------------------------------------------------------------
// Preview rendering (used by editor's Preview tab via htmx)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct PreviewRequest {
    content: String,
}

/// Render Markdown into the same `<div class="public-page__content">`
/// wrapper used by the live `/b/legalpages/{terms,privacy}` pages, so
/// the Preview tab in the editor matches production typography exactly.
pub(super) fn render_preview_fragment(markdown: &str) -> String {
    let rendered = super::markdown_to_html(markdown);
    format!(r#"<div class="public-page__content">{rendered}</div>"#)
}

/// `POST /b/legalpages/admin/render-preview` — body: `{"content": "<markdown>"}`.
/// Returns the rendered HTML fragment for direct htmx swap into the
/// preview pane.
pub async fn handle_render_preview(_ctx: &dyn Context, input: InputStream) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: PreviewRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid request: {e}")),
    };
    let fragment = render_preview_fragment(&body.content);
    ResponseBuilder::new().body(fragment.into_bytes(), "text/html; charset=utf-8")
}
