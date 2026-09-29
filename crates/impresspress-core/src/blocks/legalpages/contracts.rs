//! Wire types for the legalpages JSON API: what a request may say, and what
//! a response body looks like.

use serde::{Deserialize, Serialize};

use super::repo::{documents::DocumentRow, Page};

/// Which legal document a row is: the `doc_type` column of
/// `impresspress__legalpages__documents`.
///
/// Two variants because the block serves two documents: `ROUTES` declares
/// `/b/legalpages/terms` and `/b/legalpages/privacy` and nothing else, and
/// the admin editor is reachable at `/b/legalpages/admin/{terms,privacy}`.
/// `doc_type` used to arrive from a request body unvalidated — the JSON
/// create, the editor's save and the editor's publish were three doors onto
/// the column — so `{"doc_type":"cookies"}` stored a row every public route
/// answers 404 for and no editor page can reach. The type is what closes all
/// three at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentType {
    Terms,
    Privacy,
}

impl DocumentType {
    /// The document's name, as every page that renders it shows it.
    ///
    /// One table, because there were three that disagreed: `mod.rs`'s public
    /// page fell back to "Privacy Policy" for an unrecognised type while
    /// both of `pages.rs`'s fell back to "Terms of Service", so one
    /// unvalidated `doc_type` was labelled two different things on two
    /// pages. With the type there is no fall-back case left to disagree
    /// about.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Terms => "Terms of Service",
            Self::Privacy => "Privacy Policy",
        }
    }
}

/// Where a document version is in its lifecycle: the `status` column.
///
/// Written by exactly three repo functions, none of which takes the value
/// from a caller — that is the B10 fix, and the type is what makes the three
/// spellings they write one definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentStatus {
    Draft,
    Published,
    Archived,
}

// This type is the B10 fix, and the doc comment below is deliberately not
// the place that says so: it is published verbatim as the endpoint's schema
// description in `/openapi.json`, so it describes the contract, not its
// history. The history is that the handler used to read the body into a
// `HashMap<String, Value>` and hand the whole map to `crud::update_record`,
// which writes every key as a column — so `PATCH {"status":"published"}`
// published a document without going through `service::publish_document`,
// and therefore without archiving the sibling published before it, leaving
// the doc type with two rows claiming to be live. `deny_unknown_fields` is
// what makes the refusal explicit rather than silent: a client that sends
// `status` or `version` gets a 400 naming the field, not a 200 that quietly
// ignored it.

/// Body of `PATCH /b/legalpages/api/documents/{id}`: a document's text.
///
/// `title` and `content` are the only columns this endpoint can reach, and
/// any other field is refused by name. A document's `status` and `version`
/// change only through a publish.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateDocumentRequest {
    /// New document title. Omitted leaves the stored title.
    pub title: Option<String>,
    /// New Markdown body. Omitted leaves the stored body.
    pub content: Option<String>,
}

/// One record in the [`DocumentListView`] envelope: the row's `id` beside the
/// row's columns, exactly as `wafer_core::clients::database::Record`
/// serializes. The backends put `id` in both places (`row_to_record` inserts
/// every column into `data` and copies `id` out to the envelope), so both are
/// published.
#[derive(Debug, Clone, Serialize)]
pub struct DocumentView {
    pub id: String,
    pub data: serde_json::Map<String, serde_json::Value>,
}

impl DocumentView {
    /// Build the record envelope for one document row.
    ///
    /// [`DocumentRow`] mirrors the table column-for-column — a test in the
    /// repo module asserts that against the migration's DDL — so this cannot
    /// drop a column.
    pub fn from_row(row: &DocumentRow) -> Self {
        let data = match serde_json::to_value(row) {
            Ok(serde_json::Value::Object(map)) => map,
            // Unreachable: `DocumentRow` is a plain struct of scalars. A
            // non-object would mean it grew a serde attribute that changes
            // its shape, which the column-set test catches.
            _ => serde_json::Map::new(),
        };
        Self {
            id: row.id.clone(),
            data,
        }
    }
}

/// The `RecordList` envelope `GET /b/legalpages/api/documents` publishes:
/// `{ records, total_count, page, page_size }`.
///
/// The repo returns [`Page`], which is not `Serialize`; this view is the one
/// place a page becomes a response body, so the envelope cannot drift.
#[derive(Debug, Clone, Serialize)]
pub struct DocumentListView {
    pub records: Vec<DocumentView>,
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}

impl DocumentListView {
    /// Build the envelope from a repo page of typed rows.
    pub fn from_page(page: &Page<DocumentRow>) -> Self {
        Self {
            records: page.rows.iter().map(DocumentView::from_row).collect(),
            total_count: page.total,
            page: page.page,
            page_size: page.page_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn row(id: &str, version: i64) -> DocumentRow {
        DocumentRow {
            id: id.to_string(),
            doc_type: DocumentType::Terms,
            title: "Terms".to_string(),
            content: "body".to_string(),
            status: DocumentStatus::Published,
            version,
            created_by: "admin_1".to_string(),
            published_at: Some("2026-09-01T00:00:00Z".to_string()),
            created_at: "2026-08-01T00:00:00Z".to_string(),
            updated_at: "2026-09-01T00:00:00Z".to_string(),
        }
    }

    /// The envelope is the `Record` shape the endpoint has always published:
    /// `id` at the top level and every column inside `data`, `id` included.
    #[test]
    fn a_document_view_is_the_record_envelope() {
        let body = serde_json::to_value(DocumentView::from_row(&row("doc-1", 2)))
            .expect("the view serializes");
        assert_eq!(body["id"], json!("doc-1"));
        assert_eq!(body["data"]["id"], json!("doc-1"));
        assert_eq!(body["data"]["doc_type"], json!("terms"));
        assert_eq!(body["data"]["version"], json!(2));
        assert_eq!(body["data"]["published_at"], json!("2026-09-01T00:00:00Z"));
    }

    #[test]
    fn a_list_view_is_the_record_list_envelope() {
        let page = Page {
            rows: vec![row("doc-1", 2), row("doc-2", 1)],
            total: 7,
            page: 2,
            page_size: 20,
        };
        let body =
            serde_json::to_value(DocumentListView::from_page(&page)).expect("the view serializes");

        assert_eq!(body["total_count"], json!(7));
        assert_eq!(body["page"], json!(2));
        assert_eq!(body["page_size"], json!(20));
        let records = body["records"].as_array().expect("records is an array");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["id"], json!("doc-1"));
        assert_eq!(records[1]["data"]["version"], json!(1));
    }

    /// The whole point of the type: a body that names a column the endpoint
    /// does not own is refused, and the error names the field.
    #[test]
    fn a_status_or_version_field_is_refused_by_name() {
        for body in [r#"{"status":"published"}"#, r#"{"version":9}"#] {
            let err = serde_json::from_str::<UpdateDocumentRequest>(body)
                .expect_err("PATCH must not accept a status or version write");
            assert!(
                err.to_string().contains("unknown field"),
                "the refusal must name the field: {err}"
            );
        }
    }

    #[test]
    fn both_text_fields_are_optional_and_independent() {
        let only_title: UpdateDocumentRequest =
            serde_json::from_str(r#"{"title":"New"}"#).expect("title-only body");
        assert_eq!(only_title.title.as_deref(), Some("New"));
        assert_eq!(only_title.content, None);

        let empty: UpdateDocumentRequest = serde_json::from_str("{}").expect("empty body");
        assert_eq!(empty, UpdateDocumentRequest::default());
    }
}
