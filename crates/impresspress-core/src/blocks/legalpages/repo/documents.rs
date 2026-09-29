//! Row-level access over `impresspress__legalpages__documents`.
//!
//! One row per version of one legal document. `(doc_type, status)` is the
//! whole query language of this table — the public pages want the published
//! row for a type, the editor wants the latest draft and falls back to the
//! published one, publishing wants the highest version and every published
//! sibling — and before this module those four questions were five separately
//! spelled filter blocks in three files. Here they are two constructors,
//! [`of_type`] and [`of_type_with_status`], and the functions built on them.
//!
//! `status` is written by exactly three functions, [`insert_published`],
//! [`mark_published`] and [`mark_archived`], none of which takes the value
//! from a caller. A request body cannot reach the column: that is the B10
//! fix, held by construction rather than by a validation list.

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{
    super::contracts::{DocumentStatus, DocumentType},
    Page,
};
use crate::{
    db_read::{self, Bound},
    util::{enum_column, json_map, now_rfc3339, RecordExt},
};

/// Legal documents: one row per version of a `terms` / `privacy` document.
pub const TABLE: &str = "impresspress__legalpages__documents";

/// One stored legal document, decoded.
///
/// Mirrors the table column-for-column (`migrations/001_legalpages_schema.*`),
/// which is what lets [`super::super::contracts::DocumentView`] build the
/// published `Record` envelope by serializing the row: a column missing here
/// would silently vanish from a response body.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DocumentRow {
    pub id: String,
    /// Which document this is a version of.
    pub doc_type: DocumentType,
    pub title: String,
    /// Markdown source, rendered by `markdown_to_html` on the way out.
    pub content: String,
    /// Written only by [`insert_published`], [`mark_published`] and
    /// [`mark_archived`], none of which takes the value from a caller.
    pub status: DocumentStatus,
    pub version: i64,
    pub created_by: String,
    /// RFC 3339 instant of the last publish; `None` for a row that has never
    /// been published (the column is the table's only nullable one).
    pub published_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl DocumentRow {
    /// The one decode of a document row.
    ///
    /// Fallible since `doc_type` and `status` became types: a stored value
    /// outside either set is a data-integrity fault, and `util::enum_column`
    /// reports it naming the row rather than letting a page render a
    /// document that is neither terms nor privacy. `version` goes through
    /// `RecordExt::i64_field`, which accepts both a JSON number and a
    /// numeric string: rows created before migration 001 materialised the
    /// table through `ensure_table`, which gives every column TEXT affinity.
    pub fn from_record(rec: &Record) -> Result<Self, WaferError> {
        Ok(Self {
            id: rec.id.clone(),
            doc_type: enum_column(rec, "doc_type")?,
            title: rec.str_field("title").to_string(),
            content: rec.str_field("content").to_string(),
            status: enum_column(rec, "status")?,
            version: rec.i64_field("version"),
            created_by: rec.str_field("created_by").to_string(),
            published_at: rec.opt_str_field("published_at"),
            created_at: rec.str_field("created_at").to_string(),
            updated_at: rec.str_field("updated_at").to_string(),
        })
    }
}

/// A new draft, as [`insert_draft`] stores it.
pub struct NewDraft<'a> {
    pub doc_type: DocumentType,
    pub title: &'a str,
    pub content: &'a str,
    /// Recorded as `created_by`.
    pub created_by: &'a str,
}

/// A document published without ever having been a draft, as
/// [`insert_published`] stores it.
pub struct NewPublished<'a> {
    pub doc_type: DocumentType,
    pub title: &'a str,
    pub content: &'a str,
    pub version: i64,
    /// Recorded as `created_by`.
    pub created_by: &'a str,
    /// RFC 3339 instant stamped into `created_at`, `updated_at` and
    /// `published_at`.
    pub now: &'a str,
}

/// The editor's text, when a publish carries one. `None` keeps what is
/// stored (the JSON API's publish path sends no body).
#[derive(Default)]
pub struct PublishedContent<'a> {
    pub title: Option<&'a str>,
    pub content: Option<&'a str>,
}

/// Every row of one document type. The first of the two filter shapes this
/// table is ever queried by.
fn of_type(doc_type: DocumentType) -> Vec<Filter> {
    vec![Filter {
        field: "doc_type".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::json!(doc_type),
    }]
}

/// Every row of one document type in one status. The second filter shape.
fn of_type_with_status(doc_type: DocumentType, status: DocumentStatus) -> Vec<Filter> {
    let mut filters = of_type(doc_type);
    filters.push(Filter {
        field: "status".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::json!(status),
    });
    filters
}

/// The highest-`version` row matching `filters`, or `None`.
async fn newest_by_version(
    ctx: &dyn Context,
    filters: Vec<Filter>,
) -> Result<Option<DocumentRow>, WaferError> {
    first_matching(ctx, filters, "version").await
}

/// The first row matching `filters` under a descending sort on `sort_field`.
async fn first_matching(
    ctx: &dyn Context,
    filters: Vec<Filter>,
    sort_field: &str,
) -> Result<Option<DocumentRow>, WaferError> {
    let opts = ListOptions {
        filters,
        sort: vec![SortField {
            field: sort_field.to_string(),
            desc: true,
        }],
        limit: Some(1),
        ..Default::default()
    };
    db::list(ctx, TABLE, &opts)
        .await?
        .records
        .first()
        .map(DocumentRow::from_record)
        .transpose()
}

/// One document by id; `None` when there is no such row.
///
/// A missing row is `Ok(None)`, not an `Err`, so a caller cannot answer a
/// *failed read* the way it answers an absent one — which is exactly what
/// the editor's save handler used to do (B10).
pub async fn get(ctx: &dyn Context, id: &str) -> Result<Option<DocumentRow>, WaferError> {
    match db::get(ctx, TABLE, id).await {
        Ok(record) => DocumentRow::from_record(&record).map(Some),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The published document of `doc_type` — the highest version of it, for
/// legacy data that predates the one-published-row invariant.
pub async fn find_published(
    ctx: &dyn Context,
    doc_type: DocumentType,
) -> Result<Option<DocumentRow>, WaferError> {
    newest_by_version(
        ctx,
        of_type_with_status(doc_type, DocumentStatus::Published),
    )
    .await
}

/// The most recently edited draft of `doc_type`.
pub async fn find_latest_draft(
    ctx: &dyn Context,
    doc_type: DocumentType,
) -> Result<Option<DocumentRow>, WaferError> {
    first_matching(
        ctx,
        of_type_with_status(doc_type, DocumentStatus::Draft),
        "updated_at",
    )
    .await
}

/// The highest version recorded for `doc_type` in any status; `0` when the
/// type has no rows at all.
///
/// A read failure is an `Err`, not a `0`: answering `0` would make the next
/// publish restart the type's version numbering at 1.
pub async fn latest_version(ctx: &dyn Context, doc_type: DocumentType) -> Result<i64, WaferError> {
    Ok(newest_by_version(ctx, of_type(doc_type))
        .await?
        .map_or(0, |row| row.version))
}

/// Every published row of `doc_type`. More than one is the state a publish
/// exists to resolve.
pub async fn list_published(
    ctx: &dyn Context,
    doc_type: DocumentType,
) -> Result<Vec<DocumentRow>, WaferError> {
    db_read::list_bounded(
        ctx,
        TABLE,
        of_type_with_status(doc_type, DocumentStatus::Published),
        Bound::OnePer("published document of one type — publishing archives every other one"),
    )
    .await?
    .iter()
    .map(DocumentRow::from_record)
    .collect()
}

/// One page of documents, most recently edited first, optionally narrowed to
/// one `doc_type`.
///
/// The sort is `updated_at` rather than `crud::list_page`'s `created_at`
/// default: this backs the admin document list, where an editor expects what
/// they last touched at the top.
pub async fn list_page(
    ctx: &dyn Context,
    doc_type: Option<DocumentType>,
    page: i64,
    page_size: u32,
) -> Result<Page<DocumentRow>, WaferError> {
    let opts = ListOptions {
        filters: doc_type.map(of_type).unwrap_or_default(),
        sort: vec![SortField {
            field: "updated_at".to_string(),
            desc: true,
        }],
        limit: Some(page_size),
        offset: page.saturating_sub(1).saturating_mul(i64::from(page_size)),
        skip_count: false,
        ..Default::default()
    };
    Page::decode(db::list(ctx, TABLE, &opts).await?, DocumentRow::from_record)
}

/// How many documents the table holds, in any status.
pub async fn count(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &[]).await
}

/// Store a new version-1 draft.
///
/// The insert names every column the table has except `published_at`, which
/// is genuinely NULL for a document that has never been published — so the
/// row this returns is the row as stored, with no second read.
pub async fn insert_draft(ctx: &dyn Context, new: NewDraft<'_>) -> Result<DocumentRow, WaferError> {
    let now = now_rfc3339();
    let data = json_map(serde_json::json!({
        "doc_type": new.doc_type,
        "title": new.title,
        "content": new.content,
        "status": DocumentStatus::Draft,
        "version": 1,
        "created_by": new.created_by,
        "created_at": now,
        "updated_at": now,
    }));
    DocumentRow::from_record(&db::create(ctx, TABLE, data).await?)
}

/// Store a document that is published on creation — the shape a publish of a
/// type that has no row yet produces (the Init seed, and the editor's
/// Publish button on a document it has not saved).
///
/// One insert rather than a draft followed by a publish: a failure between
/// the two would leave a draft behind, and the Init seed's "is the table
/// already seeded?" count would then never let the type reach a published
/// document again.
///
/// One of the three writers of `status` in the crate, and like the other two
/// it does not take the value from a caller.
pub async fn insert_published(
    ctx: &dyn Context,
    new: NewPublished<'_>,
) -> Result<DocumentRow, WaferError> {
    let data = json_map(serde_json::json!({
        "doc_type": new.doc_type,
        "title": new.title,
        "content": new.content,
        "status": DocumentStatus::Published,
        "version": new.version,
        "created_by": new.created_by,
        "created_at": new.now,
        "updated_at": new.now,
        "published_at": new.now,
    }));
    DocumentRow::from_record(&db::create(ctx, TABLE, data).await?)
}

/// Replace a document's text, stamping `updated_at`. A `None` field is left
/// as stored.
///
/// This is the whole of what `PATCH /b/legalpages/api/documents/{id}` can
/// do. `status` and `version` are not parameters, so no request body reaches
/// them.
pub async fn update_content(
    ctx: &dyn Context,
    id: &str,
    title: Option<&str>,
    content: Option<&str>,
) -> Result<DocumentRow, WaferError> {
    let mut data = json_map(serde_json::json!({ "updated_at": now_rfc3339() }));
    if let Some(title) = title {
        data.insert("title".to_string(), serde_json::json!(title));
    }
    if let Some(content) = content {
        data.insert("content".to_string(), serde_json::json!(content));
    }
    DocumentRow::from_record(&db::update(ctx, TABLE, id, data).await?)
}

/// Publish `id` as `version`, stamping `published_at` and `updated_at` with
/// `now`, and applying the editor's text when it sent any.
///
/// One of the three writers of `status` in the crate. `service::publish_document`
/// is its only caller.
pub async fn mark_published(
    ctx: &dyn Context,
    id: &str,
    version: i64,
    now: &str,
    text: PublishedContent<'_>,
) -> Result<DocumentRow, WaferError> {
    let mut data = json_map(serde_json::json!({
        "status": DocumentStatus::Published,
        "version": version,
        "published_at": now,
        "updated_at": now,
    }));
    if let Some(title) = text.title {
        data.insert("title".to_string(), serde_json::json!(title));
    }
    if let Some(content) = text.content {
        data.insert("content".to_string(), serde_json::json!(content));
    }
    DocumentRow::from_record(&db::update(ctx, TABLE, id, data).await?)
}

/// Retire a previously published document.
///
/// `updated_at` is deliberately not stamped: archiving is a consequence of
/// *another* document's publish, not an edit of this one, and `updated_at` is
/// what orders the admin list by what an editor last touched.
pub async fn mark_archived(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::update(
        ctx,
        TABLE,
        id,
        json_map(serde_json::json!({ "status": DocumentStatus::Archived })),
    )
    .await
    .map(|_| ())
}

/// Remove a document. A missing row surfaces as the client's `NotFound`.
pub async fn delete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, TABLE, id).await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record(extra: &[(&str, serde_json::Value)]) -> Record {
        let mut data: std::collections::HashMap<String, serde_json::Value> = [
            ("doc_type".to_string(), json!("terms")),
            ("title".to_string(), json!("Terms of Service")),
            ("content".to_string(), json!("# Terms")),
            ("status".to_string(), json!("published")),
            ("version".to_string(), json!(4)),
            ("created_by".to_string(), json!("admin_1")),
            ("published_at".to_string(), json!("2026-09-01T00:00:00Z")),
            ("created_at".to_string(), json!("2026-08-01T00:00:00Z")),
            ("updated_at".to_string(), json!("2026-09-01T00:00:00Z")),
        ]
        .into_iter()
        .collect();
        for (k, v) in extra {
            data.insert((*k).to_string(), v.clone());
        }
        Record {
            id: "doc-1".to_string(),
            data,
        }
    }

    #[test]
    fn from_record_decodes_the_whole_row() {
        assert_eq!(
            DocumentRow::from_record(&record(&[])).expect("the row decodes"),
            DocumentRow {
                id: "doc-1".to_string(),
                doc_type: DocumentType::Terms,
                title: "Terms of Service".to_string(),
                content: "# Terms".to_string(),
                status: DocumentStatus::Published,
                version: 4,
                created_by: "admin_1".to_string(),
                published_at: Some("2026-09-01T00:00:00Z".to_string()),
                created_at: "2026-08-01T00:00:00Z".to_string(),
                updated_at: "2026-09-01T00:00:00Z".to_string(),
            }
        );
    }

    /// Rows written before migration 001 came through `ensure_table`, which
    /// gives every column TEXT affinity, so `version` reads back as a string.
    #[test]
    fn version_survives_text_storage() {
        let row =
            DocumentRow::from_record(&record(&[("version", json!("7"))])).expect("the row decodes");
        assert_eq!(row.version, 7);
    }

    /// `published_at` is the table's one nullable column, and a draft has
    /// never been published. Both shapes a NULL arrives in — the column
    /// absent from the map, and a JSON `null` — decode to `None`.
    #[test]
    fn an_unpublished_row_has_no_published_at() {
        let mut absent = record(&[]);
        absent.data.remove("published_at");
        assert_eq!(
            DocumentRow::from_record(&absent)
                .expect("the row decodes")
                .published_at,
            None
        );

        let null = record(&[("published_at", json!(null))]);
        assert_eq!(
            DocumentRow::from_record(&null)
                .expect("the row decodes")
                .published_at,
            None
        );
    }

    /// The row must mirror the table, or serializing it into the published
    /// `Record` envelope would drop a column. The DDL is the authority.
    #[test]
    fn the_row_mirrors_every_column_the_migration_declares() {
        let ddl = crate::blocks::legalpages::migrations::SQLITE_MIGRATIONS[0].1;
        let create = ddl
            .split_once(&format!("CREATE TABLE IF NOT EXISTS {TABLE} ("))
            .expect("the DDL creates the documents table")
            .1
            .split_once(')')
            .expect("the column list is parenthesised")
            .0;
        let mut columns: Vec<String> = create
            .split(',')
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_string)
            .collect();
        columns.sort();

        let serialized =
            serde_json::to_value(DocumentRow::from_record(&record(&[])).expect("the row decodes"))
                .expect("row serializes");
        let mut fields: Vec<String> = serialized
            .as_object()
            .expect("a row is a JSON object")
            .keys()
            .cloned()
            .collect();
        fields.sort();

        assert_eq!(
            fields, columns,
            "`DocumentRow` must mirror the documents table column-for-column"
        );
    }
}
