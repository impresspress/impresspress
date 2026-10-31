//! SSR admin pages for the legal pages block.
//!
//! The Legal section of the admin: the Privacy Policy and Terms of Service
//! Markdown editors, the public pages' settings, and the endpoints reference.

use maud::{html, Markup, PreEscaped};
use wafer_run::{
    context::Context, AuthLevel, BlockEndpoint, InputStream, Message, OutputStream, WaferError,
};

use super::{
    contracts::{DocumentStatus, DocumentType, DOCUMENT_FIELDS},
    repo::documents::{self, DocumentRow, NewDraft},
    service,
};
use crate::{
    blocks::crud,
    endpoint_match,
    http::{err_bad_request, ok_json, ResponseBuilder},
    ui::{
        self,
        components::{
            self, Action, ActionTarget, BadgeVariant, Card, DataTable, HeaderAction, TableCol,
            TableRow,
        },
        icons, settings_form,
        shell::Crumb,
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

/// The block's first page, the Privacy policy editor: where the sidebar's
/// "Legal" leads, and the root of the sub-pages' trail.
const LEGAL_ROOT: &str = "/b/legalpages/admin/privacy";

/// The trail of a legal sub-page: `Legal › {h1}`, the block's first page
/// linked and the sub-page titled by its own name — the rule Storage's,
/// Tickets', Products' and LLM's sub-pages follow. The window title keeps
/// the block's name ("Legal settings").
fn legal_trail(h1: &str) -> Vec<Crumb<'_>> {
    vec![
        Crumb {
            label: "Legal",
            href: Some(LEGAL_ROOT),
        },
        Crumb {
            label: h1,
            href: None,
        },
    ]
}

/// A legal admin page whose read failed: drawn in the shell under the
/// block's section links, with a link back to the dashboard. `h1` is the
/// page's heading under the trail, when it is a sub-page.
async fn error_page(
    ctx: &dyn Context,
    msg: &Message,
    title: &str,
    h1: Option<&str>,
    section: &str,
    error: WaferError,
    context: &str,
) -> OutputStream {
    let shell = match h1 {
        Some(h1) => ui::Shell::admin(title, h1).trail(legal_trail(h1)),
        None => ui::Shell::admin(title, title),
    };
    ui::shell_error_page(
        ctx,
        msg,
        shell,
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
                None,
                wire_str(&doc_type).as_str(),
                e,
                "legal editor page: document read failed",
            )
            .await
        }
    };
    let wire = wire_str(&doc_type);
    let public_href = format!("/b/legalpages/{wire}");
    let view = editor_view(doc_type, &state, &public_href);
    let mut shell = ui::Shell::admin(doc_type.title(), doc_type.title());
    for action in view.secondary_actions {
        shell = shell.secondary_action(action);
    }
    ui::shell_page(
        ctx,
        msg,
        shell.primary_action(view.primary_action),
        ui::PageBody::from(view.body).with_subnav(section_links(wire.as_str())),
    )
    .await
}

/// The editor page's two halves: the page-level actions (open, save,
/// publish), which ride in the shell topbar, and the body.
pub(super) struct EditorView<'a> {
    pub secondary_actions: Vec<HeaderAction<'a>>,
    pub primary_action: HeaderAction<'a>,
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
/// `public_href` is the document's public page, which the header opens.
pub(super) fn editor_view<'a>(
    doc_type: DocumentType,
    state: &EditorState,
    public_href: &'a str,
) -> EditorView<'a> {
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
    // save before it.
    let secondary_actions = vec![
        HeaderAction::new("Open public page", ActionTarget::NewTab(public_href)),
        HeaderAction::new("Save draft", ActionTarget::Delegated("legalpages-save"))
            .id("btn-save")
            .data("legal-editor-action", "")
            .hidden(!started)
            .keyshortcuts("Control+S Meta+S")
            .title("Save draft (Ctrl+S)"),
    ];
    let primary_action =
        HeaderAction::new("Publish", ActionTarget::Delegated("legalpages-publish"))
            .id("btn-publish")
            .data("legal-editor-action", "")
            .hidden(!started);

    // Not started, the empty state's "Write …" is the page's one visible
    // primary: the header's Publish waits hidden, and editor.js removes this
    // empty state as it reveals Save draft and Publish, so the two primaries
    // are never on screen together.
    let write = format!("Write the {lower}");
    let body = html! {
        @if !started {
            div #legal-editor-empty {
                (components::empty_state(
                    icons::file_text(),
                    &format!("No {lower} yet"),
                    "Nothing has been saved or published yet, so the public page shows a \
                     placeholder until you publish.",
                    Some(Action::primary(&write, ActionTarget::Delegated("legalpages-start")).render()),
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

            label .form-label for="editor" { "Content" }
            // The `.tabs` strip, as buttons: Edit and Preview are two views
            // of one field, switched in place by `editor.js` (no request),
            // so they are a WAI-ARIA tablist rather than `tab_navigation`'s
            // htmx links.
            // `.legal-editor__tabs`: the strip is the top edge of the pane it
            // switches, not a row of its own above it (form.css).
            div .tabs .legal-editor__tabs role="tablist" aria-label="Content view" {
                button #editor-tab-edit .tab .active type="button"
                    role="tab" aria-selected="true" aria-controls="editor-edit-pane"
                    data-tab="edit" data-action="legalpages-editor-tab"
                { "Edit" }
                button #editor-tab-preview .tab type="button"
                    role="tab" aria-selected="false" aria-controls="editor-preview-pane"
                    tabindex="-1"
                    data-tab="preview" data-action="legalpages-editor-tab"
                { "Preview" }
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
    EditorView {
        secondary_actions,
        primary_action,
        body,
    }
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
            .trail(legal_trail("Endpoints"))
            .subtitle("Every HTTP endpoint the legal pages block serves"),
        ui::PageBody::from(endpoints_view()).with_subnav(section_links("endpoints")),
    )
    .await
}

const FIELD_COLUMNS: [TableCol<'static>; 3] = [
    TableCol::new("Field").primary().width("25%"),
    TableCol::new("Type"),
    TableCol::new("Description"),
];

/// The endpoints reference: what the block declares from `ROUTES` — the
/// table `handle()` dispatches on — grouped by who may call it, each group a
/// flush card holding the shared [`components::endpoint_table`], then the
/// document fields in one more. It cannot drift from what the block serves.
pub(super) fn endpoints_view() -> Markup {
    let declared = endpoint_match::declare(super::ROUTES);
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
            @let endpoints: Vec<BlockEndpoint> = declared
                .iter()
                .filter(|ep| ep.auth == level)
                .cloned()
                .collect();
            @if !endpoints.is_empty() {
                (Card::titled(
                    title,
                    components::endpoint_table(&format!("{title} endpoints"), &endpoints),
                )
                .subtitle(description)
                .flush()
                .render())
            }
        }
        (Card::titled("Document fields", DataTable::new(&FIELD_COLUMNS).rows(fields).render())
            .subtitle("The fields of each record's data object in the JSON API.")
            .flush()
            .render())
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
                Err(e) => return crud::db_error_internal(e, "Failed to save legal-page draft"),
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

    // The live-preview links are a card of their own after the Appearance
    // card, in the form's `extra` slot: the same width as the section cards,
    // above the Save row.
    let preview = components::Card::titled(
        "Preview",
        html! {
            div .flex .gap-2 .flex-wrap {
                (components::Action::quiet("Privacy policy", components::ActionTarget::NewTab("/b/legalpages/privacy"))
                    .icon(icons::eye())
                    .render())
                (components::Action::quiet("Terms of service", components::ActionTarget::NewTab("/b/legalpages/terms"))
                    .icon(icons::eye())
                    .render())
            }
        },
    )
    .subtitle("See how your changes look on the public pages.")
    .render();

    let content =
        match settings_form::settings_form(ctx, SETTINGS_SAVE_PATH, &sections, preview).await {
            Ok(form) => form,
            Err(e) => {
                return error_page(
                    ctx,
                    msg,
                    "Legal settings",
                    Some("Settings"),
                    "settings",
                    e,
                    "legalpages settings: current values read failed",
                )
                .await
            }
        };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::admin("Legal settings", "Settings")
            .trail(legal_trail("Settings"))
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

#[cfg(test)]
mod page_chrome_tests {
    //! What the legal admin pages render through the block's own dispatch:
    //! the sub-pages' trail, one primary at most, one level of cards.

    use wafer_run::{Block, InputStream};

    use super::*;
    use crate::{
        blocks::legalpages::LegalPagesBlock,
        test_support::{admin_msg, dom, output_html, TestContext},
    };

    /// The block with its two default documents, as a fresh deployment has
    /// them from migration 003.
    async fn seeded() -> TestContext {
        crate::blocks::legalpages::fresh_install_ctx().await
    }

    async fn page(ctx: &TestContext, path: &str) -> String {
        let mut msg = admin_msg("retrieve", path);
        msg.set_meta("http.header.accept", "text/html");
        output_html(
            LegalPagesBlock::new()
                .handle(ctx, msg, InputStream::empty())
                .await,
        )
        .await
    }

    /// Settings and Endpoints are titled by their own names under the trail
    /// `Legal ›`, which leads to the block's first page; the document
    /// editors are the block's own pages, with no trail. The window titles
    /// stay as they were.
    #[tokio::test]
    async fn the_sub_pages_are_titled_under_the_legal_trail() {
        let ctx = seeded().await;
        for (path, title, h1) in [
            ("/b/legalpages/admin/settings", "Legal settings", "Settings"),
            ("/b/legalpages/admin/endpoints", "Endpoints", "Endpoints"),
        ] {
            let html = page(&ctx, path).await;
            assert!(
                html.contains(&format!("<title>{title}")),
                "{path}: the window title stays {title:?}: {html}"
            );
            assert!(
                html.contains(&format!(
                    concat!(
                        r#"<nav class="topbar__crumbs" aria-label="Breadcrumb"><ol>"#,
                        r#"<li><a href="{root}">Legal</a></li></ol></nav>"#,
                        r#"<h1 class="topbar__title">{h1}</h1>"#
                    ),
                    root = LEGAL_ROOT,
                    h1 = h1
                )),
                "{path}: expected the trail Legal › {h1}: {html}"
            );
        }
        let privacy = page(&ctx, LEGAL_ROOT).await;
        assert!(!privacy.contains("topbar__crumbs"), "{privacy}");
    }

    /// The editor's one primary is the header's Publish; Settings' is Save
    /// settings; Endpoints has none. Every endpoint group and the fields are
    /// flush cards, and no page draws a card-level surface inside a card or
    /// skips a heading level.
    #[tokio::test]
    async fn each_page_has_one_primary_at_most_and_one_level_of_cards() {
        let ctx = seeded().await;
        for (path, primaries) in [
            (LEGAL_ROOT, vec![r#"button.btn.btn--primary "Publish""#]),
            (
                "/b/legalpages/admin/settings",
                vec![r#"button.btn.btn--primary "Save settings""#],
            ),
            ("/b/legalpages/admin/endpoints", vec![]),
        ] {
            let html = page(&ctx, path).await;
            assert_eq!(dom::visible_primaries(&html).0, primaries, "{path}: {html}");
            assert_eq!(
                dom::card_level_inside_card(&html),
                Vec::<String>::new(),
                "{path}"
            );
            let outline = dom::heading_outline(&html);
            assert!(
                outline.windows(2).all(|pair| pair[1].0 <= pair[0].0 + 1),
                "{path}: a heading skips a level: {outline:?}"
            );
        }
        let endpoints = endpoints_view().into_string();
        let groups = endpoints
            .matches(r#"<section class="card card--flush">"#)
            .count();
        assert!(
            groups >= 3,
            "each endpoint group and the fields is a flush card: {endpoints}"
        );
        assert!(!endpoints.contains("legal-endpoints__group"), "{endpoints}");
    }

    /// Not started, the empty state's "Write the privacy policy" is the
    /// page's one visible primary: Publish and Save draft wait hidden.
    #[test]
    fn a_document_not_started_offers_one_visible_primary_in_its_empty_state() {
        let state = EditorState {
            current: None,
            live_version: None,
            next_version: 1,
        };
        let view = editor_view(DocumentType::Privacy, &state, "/b/legalpages/privacy");
        let body = view.body.into_string();
        assert_eq!(
            dom::visible_primaries(&body).0,
            vec![r#"button.btn.btn--primary "Write the privacy policy""#],
            "{body}"
        );
        assert!(
            body.contains(r#"data-action="legalpages-start""#),
            "the empty state's action is the start verb editor.js handles: {body}"
        );
    }
}
