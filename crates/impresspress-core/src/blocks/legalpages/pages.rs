//! SSR admin pages for the legal pages block.
//!
//! Provides a tabbed admin UI with:
//! - Privacy Policy editor (Quill rich text editor)
//! - Terms of Service editor (Quill rich text editor)
//! - API endpoints reference

use maud::{html, Markup, PreEscaped};
use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::service;
use crate::{
    http::{err_bad_request, err_internal, err_not_found, ok_json, ResponseBuilder},
    ui::{self, components, icons, settings_form},
    util::{json_map, RecordExt},
};

const COLLECTION: &str = super::COLLECTION;

// ---------------------------------------------------------------------------
// Document lookup
// ---------------------------------------------------------------------------

/// Find the current document for a given type.
/// Prefers the latest draft (so admin sees their in-progress edits),
/// then falls back to the published version.
async fn find_current_doc(ctx: &dyn Context, doc_type: &str) -> Option<db::Record> {
    // First try latest draft
    let opts = ListOptions {
        filters: vec![
            Filter {
                field: "doc_type".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(doc_type),
            },
            Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("draft"),
            },
        ],
        sort: vec![SortField {
            field: "updated_at".into(),
            desc: true,
        }],
        limit: 1,
        ..Default::default()
    };
    if let Ok(result) = db::list(ctx, COLLECTION, &opts).await {
        if let Some(record) = result.records.into_iter().next() {
            return Some(record);
        }
    }

    // Fall back to published
    let opts = ListOptions {
        filters: vec![
            Filter {
                field: "doc_type".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!(doc_type),
            },
            Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("published"),
            },
        ],
        sort: vec![SortField {
            field: "updated_at".into(),
            desc: true,
        }],
        limit: 1,
        ..Default::default()
    };
    if let Ok(result) = db::list(ctx, COLLECTION, &opts).await {
        result.records.into_iter().next()
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Editor page (Privacy / Terms)
// ---------------------------------------------------------------------------

pub async fn editor_page(ctx: &dyn Context, msg: &Message, doc_type: &str) -> OutputStream {
    let doc = find_current_doc(ctx, doc_type).await;
    let default_title = if doc_type == "privacy" {
        "Privacy Policy"
    } else {
        "Terms of Service"
    };

    let (doc_id, title, content, status, updated_at, version) = match &doc {
        Some(d) => {
            let t = d.str_field("title");
            let title = if t.is_empty() { default_title } else { t };
            let ver = super::service::doc_version(d).unwrap_or(1);
            (
                d.id.as_str(),
                title,
                d.str_field("content"),
                d.str_field("status"),
                d.str_field("updated_at"),
                ver,
            )
        }
        None => ("", default_title, "", "none", "", 1),
    };

    let page_content = editor_markup_for_test(
        doc_type, doc_id, title, content, status, updated_at, version,
    );

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple(default_title, ui::NavKind::Portal, default_title),
        page_content,
    )
    .await
}

/// Build the editor markup. Split out from `editor_page` so it can be
/// unit-tested without a `Context`.
pub(super) fn editor_markup_for_test(
    doc_type: &str,
    doc_id: &str,
    title: &str,
    content: &str,
    status: &str,
    updated_at: &str,
    version: i64,
) -> Markup {
    let default_title = if doc_type == "privacy" {
        "Privacy Policy"
    } else {
        "Terms of Service"
    };
    let badge_class = match status {
        "published" => "badge-success",
        "draft" => "badge-warning",
        _ => "badge-info",
    };
    let badge_text = match status {
        "published" => "Published",
        "draft" => "Draft",
        "archived" => "Archived",
        _ => "No document",
    };

    html! {
        // Status bar (compact, top of page)
        div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:0.75rem" {
            div style="display:flex;align-items:center;gap:0.5rem" {
                h2 style="font-size:1.25rem;font-weight:600;margin:0" { (default_title) }
                span #status-badge .badge .(badge_class) { (badge_text) }
                span .badge style="font-size:0.7rem;background:#f1f5f9;color:#64748b;cursor:pointer"
                    title="Click to change version"
                    onclick="promptVersion()"
                { "v" span #version-display { (version) } }
                @if !updated_at.is_empty() {
                    span .text-muted style="font-size:0.8rem" {
                        " \u{00b7} " (updated_at.get(..10).unwrap_or(updated_at))
                    }
                }
            }
            div style="display:flex;gap:0.5rem" {
                a .btn .btn-sm .btn-ghost
                    href={"/b/legalpages/" (doc_type)}
                    target="_blank"
                {
                    "Open public page"
                }
                button #btn-save .btn .btn-sm .btn-secondary onclick="saveDocument(false)" {
                    "Save Draft"
                }
                button #btn-publish .btn .btn-sm .btn-primary onclick="saveDocument(true)" {
                    "Publish"
                }
            }
        }

        // Title input
        input #title-input .form-input
            type="text"
            name="title"
            value=(title)
            placeholder="Document title"
            style="font-size:1.1rem;font-weight:600;margin-bottom:0.5rem";

        // Hidden fields used by save handler JS
        input #doc-type type="hidden" value=(doc_type);
        input #doc-id type="hidden" value=(doc_id);
        input #doc-version type="hidden" value=(version);

        // Tab strip
        style { (PreEscaped(EDITOR_CSS)) }
        div .editor-tabs {
            button .editor-tab .editor-tab--active type="button"
                data-tab="edit"
                onclick="setEditorTab('edit')"
            { "Edit" }
            button .editor-tab type="button"
                data-tab="preview"
                onclick="setEditorTab('preview')"
            { "Preview" }
        }

        // Edit pane (textarea)
        div #editor-edit-pane .editor-pane {
            textarea #editor .form-input .editor-textarea
                name="content"
                placeholder="Write your legal document in Markdown..."
            { (content) }
        }

        // Preview pane (vanilla JS fetch target)
        div #editor-preview-pane .editor-pane style="display:none" {
            div #editor-preview .preview-content {
                p .text-muted { "Click Preview above to render." }
            }
        }

        script { (PreEscaped(EDITOR_JS)) }
    }
}

const EDITOR_CSS: &str = r#"
.editor-tabs {
    display: flex; gap: 4px; margin-bottom: -1px; border-bottom: 1px solid #e2e8f0;
}
.editor-tab {
    background: none; border: 1px solid transparent; border-bottom: none;
    border-radius: 6px 6px 0 0; padding: 6px 14px; cursor: pointer;
    font-size: 0.875rem; color: #64748b;
}
.editor-tab:hover { background: #f1f5f9; color: #1e293b; }
.editor-tab--active {
    background: white; border-color: #e2e8f0; color: #1e293b; font-weight: 600;
}
.editor-pane {
    background: white; border: 1px solid #e2e8f0; border-radius: 0 6px 6px 6px;
    min-height: 500px;
}
.editor-textarea {
    width: 100%; min-height: 500px; padding: 1rem;
    border: none; outline: none; resize: vertical;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.9rem; line-height: 1.6; background: transparent;
}
.preview-content {
    padding: 1.5rem; min-height: 500px; font-family: Georgia, 'Times New Roman', serif;
    line-height: 1.8;
}
"#;

const EDITOR_JS: &str = r#"
(function() {
    // Preview wiring: vanilla JS fetch (no json-enc htmx extension loaded)
    window.setEditorTab = function(name) {
        document.querySelectorAll('.editor-tab').forEach(function(t) {
            t.classList.toggle('editor-tab--active', t.dataset.tab === name);
        });
        document.getElementById('editor-edit-pane').style.display = (name === 'edit') ? '' : 'none';
        document.getElementById('editor-preview-pane').style.display = (name === 'preview') ? '' : 'none';
        if (name === 'preview') {
            var content = document.getElementById('editor').value;
            fetch('/b/legalpages/admin/render-preview', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ content: content })
            })
            .then(function(r) {
                if (!r.ok) { throw new Error('HTTP ' + r.status); }
                return r.text();
            })
            .then(function(html) { document.getElementById('editor-preview').innerHTML = html; })
            .catch(function(err) {
                document.getElementById('editor-preview').innerHTML =
                    '<p style="color:#ef4444">Preview failed: ' + err.message + '</p>';
            });
        }
    };

    window.promptVersion = function() {
        var current = document.getElementById('doc-version').value;
        var v = prompt('Set version number:', current);
        if (v !== null && v.trim() !== '') {
            var num = parseInt(v, 10);
            if (num > 0) {
                document.getElementById('doc-version').value = num;
                document.getElementById('version-display').textContent = num;
            }
        }
    };

    // Ctrl+S / Cmd+S → save draft
    document.addEventListener('keydown', function(e) {
        if ((e.ctrlKey || e.metaKey) && e.key === 's') {
            e.preventDefault();
            saveDocument(false);
        }
    });

    // Save handler (reads textarea .value)
    window.saveDocument = function(publish) {
        var title = document.getElementById('title-input').value;
        var content = document.getElementById('editor').value;
        var docType = document.getElementById('doc-type').value;
        var docId = document.getElementById('doc-id').value;
        var version = parseInt(document.getElementById('doc-version').value, 10) || 0;
        var url = publish ? '/b/legalpages/admin/publish' : '/b/legalpages/admin/save';

        var btn = document.getElementById(publish ? 'btn-publish' : 'btn-save');
        var origText = btn.textContent;
        btn.disabled = true;
        btn.textContent = publish ? 'Publishing...' : 'Saving...';

        fetch(url, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ doc_type: docType, title: title, content: content, doc_id: docId, version: version })
        })
        .then(function(r) { return r.json(); })
        .then(function(data) {
            document.body.dispatchEvent(new CustomEvent('showToast', {
                detail: { message: data.message || (data.error || 'Done'), type: data.error ? 'error' : 'success' }
            }));
            if (data.doc_id) document.getElementById('doc-id').value = data.doc_id;
            if (data.version) {
                document.getElementById('doc-version').value = data.version;
                document.getElementById('version-display').textContent = data.version;
            }
            if (data.status) {
                var badge = document.getElementById('status-badge');
                if (badge) {
                    badge.className = 'badge ' + (data.status === 'published' ? 'badge-success' : 'badge-warning');
                    badge.textContent = data.status.charAt(0).toUpperCase() + data.status.slice(1);
                }
            }
        })
        .catch(function(err) {
            document.body.dispatchEvent(new CustomEvent('showToast', {
                detail: { message: 'Error: ' + err.message, type: 'error' }
            }));
        })
        .finally(function() {
            btn.disabled = false;
            btn.textContent = origText;
        });
    };
})();
"#;

// ---------------------------------------------------------------------------
// Endpoints page
// ---------------------------------------------------------------------------

pub async fn endpoints_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let content = html! {
        (components::page_header("API Endpoints", Some("Available endpoints for legal pages"), None))

        // Public endpoints
        h3 style="font-size:1rem;font-weight:600;margin-bottom:0.5rem" { "Public Endpoints" }
        p .text-muted style="font-size:0.875rem;margin-bottom:0.75rem" {
            "These endpoints are publicly accessible and return formatted HTML pages."
        }
        div .table-container style="margin-bottom:2rem" {
            table .table {
                thead {
                    tr {
                        th style="width:80px" { "Method" }
                        th { "Endpoint" }
                        th { "Description" }
                    }
                }
                tbody {
                    tr {
                        td { span .badge .badge-success { "GET" } }
                        td { code { "/b/legalpages/terms" } }
                        td { "View published Terms of Service page" }
                    }
                    tr {
                        td { span .badge .badge-success { "GET" } }
                        td { code { "/b/legalpages/privacy" } }
                        td { "View published Privacy Policy page" }
                    }
                }
            }
        }

        // Admin API
        h3 style="font-size:1rem;font-weight:600;margin-bottom:0.5rem" { "Admin API Endpoints" }
        p .text-muted style="font-size:0.875rem;margin-bottom:0.75rem" {
            "These endpoints require admin authentication and return JSON responses."
        }
        div .table-container {
            table .table {
                thead {
                    tr {
                        th style="width:80px" { "Method" }
                        th { "Endpoint" }
                        th { "Description" }
                    }
                }
                tbody {
                    tr {
                        td { span .badge .badge-success { "GET" } }
                        td { code { "/b/legalpages/api/documents" } }
                        td { "List all documents (supports " code { "?type=terms|privacy" } " filter)" }
                    }
                    tr {
                        td { span .badge .badge-info { "POST" } }
                        td { code { "/b/legalpages/api/documents" } }
                        td { "Create a new document " span .text-muted { "(body: doc_type, title, content)" } }
                    }
                    tr {
                        td { span .badge .badge-warning { "PATCH" } }
                        td { code { "/b/legalpages/api/documents/:id" } }
                        td { "Update a document" }
                    }
                    tr {
                        td { span .badge .badge-info { "POST" } }
                        td { code { "/b/legalpages/api/documents/:id/publish" } }
                        td { "Publish a document (archives previous published version)" }
                    }
                    tr {
                        td { span .badge .badge-danger { "DELETE" } }
                        td { code { "/b/legalpages/api/documents/:id" } }
                        td { "Delete a document" }
                    }
                }
            }
        }

        // Document schema
        h3 style="font-size:1rem;font-weight:600;margin:2rem 0 0.5rem" { "Document Schema" }
        p .text-muted style="font-size:0.875rem;margin-bottom:0.75rem" {
            "Each legal document has the following fields."
        }
        div .table-container {
            table .table {
                thead {
                    tr {
                        th { "Field" }
                        th { "Type" }
                        th { "Description" }
                    }
                }
                tbody {
                    tr {
                        td { code { "doc_type" } }
                        td { "string" }
                        td { "Document type: " code { "terms" } " or " code { "privacy" } }
                    }
                    tr {
                        td { code { "title" } }
                        td { "string" }
                        td { "Document title" }
                    }
                    tr {
                        td { code { "content" } }
                        td { "text" }
                        td { "HTML content of the document" }
                    }
                    tr {
                        td { code { "status" } }
                        td { "string" }
                        td {
                            "Document status: "
                            span .badge .badge-warning { "draft" }
                            " "
                            span .badge .badge-success { "published" }
                            " "
                            span .badge { "archived" }
                        }
                    }
                    tr {
                        td { code { "version" } }
                        td { "int" }
                        td { "Version number" }
                    }
                    tr {
                        td { code { "published_at" } }
                        td { "datetime" }
                        td { "When the document was last published" }
                    }
                }
            }
        }
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Endpoints", ui::NavKind::Portal, "Endpoints"),
        content,
    )
    .await
}

// ---------------------------------------------------------------------------
// Save / Publish handlers
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct SaveRequest {
    doc_type: String,
    title: String,
    content: String,
    #[serde(default)]
    doc_id: String,
    #[serde(default)]
    version: i64,
}

/// Save a draft document. If the current doc is published, creates a new draft
/// so the live version stays untouched until the admin explicitly publishes.
pub async fn handle_save(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let raw = input.collect_to_bytes().await;
    let body: SaveRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        // Previously returned 200 OK with an `error` key — htmx clients
        // would still treat that as success. Use the proper 4xx so the
        // caller can branch on status alone.
        Err(e) => return err_bad_request(&format!("Invalid request: {e}")),
    };

    // If editing a published document, create a new draft instead of modifying the live version
    let should_create_new = if body.doc_id.is_empty() {
        true
    } else {
        match db::get(ctx, COLLECTION, &body.doc_id).await {
            Ok(doc) => {
                doc.data
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    == "published"
            }
            Err(_) => true,
        }
    };

    if should_create_new {
        // Draft shape lives in `service::create_draft` (shared with the JSON
        // API's document-create handler in `mod.rs`).
        match service::create_draft(
            ctx,
            &body.doc_type,
            &body.title,
            &body.content,
            msg.user_id(),
        )
        .await
        {
            Ok(record) => ok_json(&serde_json::json!({
                "doc_id": record.id,
                "status": "draft",
                "message": "Draft saved"
            })),
            Err(e) => err_internal("Failed to save legal-page draft", e),
        }
    } else {
        let data = json_map(serde_json::json!({
            "title": body.title,
            "content": body.content,
            "updated_at": crate::util::now_rfc3339()
        }));
        match db::update(ctx, COLLECTION, &body.doc_id, data).await {
            Ok(_) => ok_json(&serde_json::json!({
                "doc_id": body.doc_id,
                "status": "draft",
                "message": "Draft saved"
            })),
            Err(e) => err_internal("Failed to save legal-page draft", e),
        }
    }
}

/// Save and publish a document. Archives any previously published document
/// of the same type (publish-then-archive ordering lives in
/// `service::publish_document`).
pub async fn handle_publish(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    let raw = input.collect_to_bytes().await;
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
            doc_type: &body.doc_type,
            doc_id: &body.doc_id,
            title: Some(&body.title),
            content: Some(&body.content),
            version: body.version,
            created_by: msg.user_id(),
        },
    )
    .await
    {
        Ok(p) => p,
        Err(e) if e.code == ErrorCode::NotFound => return err_not_found("Document not found"),
        Err(e) => return err_internal("Failed to publish legal page", e),
    };

    ok_json(&serde_json::json!({
        "doc_id": published.record.id,
        "status": "published",
        "version": published.version,
        "message": format!("Published as v{}", published.version)
    }))
}

// ---------------------------------------------------------------------------
// Settings page
// ---------------------------------------------------------------------------

pub async fn settings_page(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let vars = super::config_vars();
    let sections = [settings_form::SettingsSection::new(
        "Appearance",
        icons::settings(),
        &vars,
    )];

    // The live-preview links ride in the form's `extra` slot so they stay
    // inside the settings form (above the Save button), as before.
    let preview = html! {
        div .card style="margin-bottom:1.25rem;padding:1rem" {
            h4 style="font-size:0.9rem;font-weight:600;margin-bottom:0.5rem" { "Preview" }
            p .text-muted style="font-size:0.8rem;margin-bottom:0.75rem" {
                "See how your changes look on the public pages."
            }
            div style="display:flex;gap:0.5rem" {
                a .btn .btn-sm .btn-ghost href="/b/legalpages/privacy" target="_blank" {
                    (icons::eye()) " Privacy Policy"
                }
                a .btn .btn-sm .btn-ghost href="/b/legalpages/terms" target="_blank" {
                    (icons::eye()) " Terms of Service"
                }
            }
        }
    };

    let saved = msg.query("saved") == "1";

    let content = html! {
        (components::page_header("Settings", Some("Customize the public legal pages appearance"), None))

        @if saved {
            div .alert .alert-success style="margin-bottom:1rem" {
                "Settings saved successfully."
            }
        }

        (settings_form::settings_form(ctx, "/b/legalpages/admin/settings", &sections, preview).await)
    };

    ui::shell_page(
        ctx,
        msg,
        ui::Shell::simple("Settings", ui::NavKind::Portal, "Settings"),
        content,
    )
    .await
}

pub async fn handle_save_settings(ctx: &dyn Context, input: InputStream) -> OutputStream {
    settings_form::save_settings(ctx, input, &super::config_vars(), "legalpages").await
}

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
    let raw = input.collect_to_bytes().await;
    let body: PreviewRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid request: {e}")),
    };
    let fragment = render_preview_fragment(&body.content);
    ResponseBuilder::new().body(fragment.into_bytes(), "text/html; charset=utf-8")
}
