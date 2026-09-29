//! Publish/archive business logic for the legalpages block.
//!
//! Plain async functions — no HTTP awareness (mirrors the messages block's
//! service layering), and no database awareness either: every statement runs
//! in `repo::documents`. Both publish surfaces route through
//! [`publish_document`]:
//!
//! - the JSON API handler (`PATCH /b/legalpages/api/documents/{id}/publish`)
//! - the admin-UI editor handler (`POST /b/legalpages/admin/publish`)
//!
//! so the publish-then-archive ordering and version handling exist exactly
//! once. Since this PR they are also the *only* way a document's `status`
//! changes: the repo writes the column from three functions, and this file is
//! the sole caller of all three.

use wafer_run::{context::Context, WaferError};

use super::{
    contracts::DocumentType,
    repo::documents::{self, DocumentRow, NewPublished, PublishedContent},
};

/// Inputs for [`publish_document`].
pub(super) struct PublishRequest<'a> {
    /// Which document this publishes; drives version computation and which
    /// previously published siblings get archived.
    pub doc_type: DocumentType,
    /// Existing document to publish; empty string = create a new one.
    pub doc_id: &'a str,
    /// New title from the editor; `None` keeps the stored value
    /// (JSON API publish path).
    pub title: Option<&'a str>,
    /// New content from the editor; `None` keeps the stored value.
    pub content: Option<&'a str>,
    /// Explicit version when `> 0`, otherwise auto-increment past the
    /// highest existing version for this `doc_type`.
    pub version: i64,
    /// Recorded as `created_by` when a new document is created.
    pub created_by: &'a str,
}

/// Outcome of a successful [`publish_document`] call.
pub(super) struct Published {
    /// The published document as stored.
    pub row: DocumentRow,
    /// The version it was published as.
    pub version: i64,
}

/// Publish a document, then archive previously published documents of the
/// same type.
///
/// Ordering matters: the new doc goes live first, and the archive pass
/// excludes it. Archiving up-front would leave the doc-type with no
/// published version if the publish step then failed.
///
/// Every step reports. A `latest_version` that could not be read used to
/// answer `0`, which silently restarted the type's version numbering at 1;
/// an archive pass that failed used to be logged at `warn` and answered
/// `200`, leaving the type with two rows claiming to be published. A publish
/// now either completes or returns the error — including when it fails
/// *after* the new document is live, which is precisely the state an
/// operator has to be told about.
pub(super) async fn publish_document(
    ctx: &dyn Context,
    req: PublishRequest<'_>,
) -> Result<Published, WaferError> {
    let version = if req.version > 0 {
        req.version
    } else {
        documents::latest_version(ctx, req.doc_type).await? + 1
    };

    let now = crate::util::now_rfc3339();
    let row = if req.doc_id.is_empty() {
        documents::insert_published(
            ctx,
            NewPublished {
                doc_type: req.doc_type,
                title: req.title.unwrap_or_default(),
                content: req.content.unwrap_or_default(),
                version,
                created_by: req.created_by,
                now: &now,
            },
        )
        .await?
    } else {
        documents::mark_published(
            ctx,
            req.doc_id,
            version,
            &now,
            PublishedContent {
                title: req.title,
                content: req.content,
            },
        )
        .await?
    };

    // New doc is live; safe to archive earlier published siblings now.
    archive_published(ctx, req.doc_type, &row.id).await?;

    Ok(Published { row, version })
}

/// Archive all published documents of a given type, except `except_id`
/// (the document that was just published).
async fn archive_published(
    ctx: &dyn Context,
    doc_type: DocumentType,
    except_id: &str,
) -> Result<(), WaferError> {
    for row in documents::list_published(ctx, doc_type).await? {
        if row.id == except_id {
            continue;
        }
        documents::mark_archived(ctx, &row.id).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        super::{contracts::DocumentStatus, seed_doc, stored, test_ctx},
        *,
    };

    #[tokio::test]
    async fn publish_existing_doc_auto_increments_and_archives_previous() {
        let ctx = test_ctx().await;
        let live = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Old Terms",
            DocumentStatus::Published,
            3,
        )
        .await;
        let draft = seed_doc(
            &ctx,
            DocumentType::Terms,
            "New Terms",
            DocumentStatus::Draft,
            1,
        )
        .await;

        let published = publish_document(
            &ctx,
            PublishRequest {
                doc_type: DocumentType::Terms,
                doc_id: &draft.id,
                title: None,
                content: None,
                version: 0,
                created_by: "admin_1",
            },
        )
        .await
        .expect("publish draft");

        // Auto-increment past the highest existing version (3 → 4).
        assert_eq!(published.version, 4);
        assert_eq!(published.row.id, draft.id);

        // The just-published doc must NOT be archived (except_id guard) and
        // keeps its stored title (JSON publish path passes None).
        let now_live = stored(&ctx, &draft.id).await;
        assert_eq!(now_live.status, DocumentStatus::Published);
        assert_eq!(now_live.version, 4);
        assert_eq!(now_live.title, "New Terms");
        assert!(now_live.published_at.is_some());

        // The previously published sibling is archived.
        assert_eq!(
            stored(&ctx, &live.id).await.status,
            DocumentStatus::Archived
        );
    }

    #[tokio::test]
    async fn publish_new_doc_creates_published_and_archives_previous() {
        let ctx = test_ctx().await;
        let live = seed_doc(
            &ctx,
            DocumentType::Privacy,
            "Old Policy",
            DocumentStatus::Published,
            1,
        )
        .await;

        let published = publish_document(
            &ctx,
            PublishRequest {
                doc_type: DocumentType::Privacy,
                doc_id: "",
                title: Some("New Policy"),
                content: Some("fresh body"),
                version: 0,
                created_by: "admin_1",
            },
        )
        .await
        .expect("publish new doc");

        assert_eq!(published.version, 2);
        assert_ne!(published.row.id, live.id);

        let created = stored(&ctx, &published.row.id).await;
        assert_eq!(created.status, DocumentStatus::Published);
        assert_eq!(created.title, "New Policy");
        assert_eq!(created.created_by, "admin_1");
        assert!(created.published_at.is_some());

        assert_eq!(
            stored(&ctx, &live.id).await.status,
            DocumentStatus::Archived
        );
    }

    /// Both create surfaces — the JSON API (`POST /b/legalpages/api/documents`)
    /// and the admin editor save (`POST /b/legalpages/admin/save`, create
    /// branch) — must produce version-1 drafts of the identical stored shape,
    /// because both go through the one `documents::insert_draft`.
    #[tokio::test]
    async fn both_create_surfaces_produce_identical_version_1_drafts() {
        use wafer_run::InputStream;

        use crate::test_support::{admin_msg, output_json};

        let ctx = test_ctx().await;
        let body = serde_json::to_vec(&serde_json::json!({
            "doc_type": "terms",
            "title": "Terms",
            "content": "the terms",
        }))
        .expect("serialize create body");

        // JSON API surface.
        let block = super::super::LegalPagesBlock::new();
        let msg = admin_msg("create", "/b/legalpages/api/documents");
        let out = block
            .handle_admin_create(&ctx, &msg, InputStream::from_bytes(body.clone()))
            .await;
        let api_resp = output_json(out).await;
        let api_id = api_resp["id"]
            .as_str()
            .expect("api create returns the record")
            .to_string();

        // Admin editor save surface (create branch: no doc_id).
        let msg = admin_msg("create", "/b/legalpages/admin/save");
        let out = super::super::pages::handle_save(&ctx, &msg, InputStream::from_bytes(body)).await;
        let save_resp = output_json(out).await;
        let save_id = save_resp["doc_id"]
            .as_str()
            .expect("save returns doc_id")
            .to_string();

        let api_doc = stored(&ctx, &api_id).await;
        let save_doc = stored(&ctx, &save_id).await;
        for doc in [&api_doc, &save_doc] {
            assert_eq!(doc.status, DocumentStatus::Draft);
            assert_eq!(doc.version, 1);
            assert_eq!(doc.created_by, "admin_1");
            assert_eq!(doc.published_at, None);
        }

        // Identical stored shape apart from the identity and the clock — the
        // draft shape exists exactly once.
        assert_eq!(
            DocumentRow {
                id: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
                ..api_doc
            },
            DocumentRow {
                id: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
                ..save_doc
            },
        );
    }

    #[tokio::test]
    async fn publish_respects_explicit_version_and_other_doc_types_untouched() {
        let ctx = test_ctx().await;
        let other_type = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Terms",
            DocumentStatus::Published,
            1,
        )
        .await;
        let draft = seed_doc(
            &ctx,
            DocumentType::Privacy,
            "Policy",
            DocumentStatus::Draft,
            1,
        )
        .await;

        let published = publish_document(
            &ctx,
            PublishRequest {
                doc_type: DocumentType::Privacy,
                doc_id: &draft.id,
                title: Some("Policy"),
                content: Some("body"),
                version: 7,
                created_by: "admin_1",
            },
        )
        .await
        .expect("publish with explicit version");

        assert_eq!(published.version, 7);

        // Archiving is scoped to the published doc_type.
        assert_eq!(
            stored(&ctx, &other_type.id).await.status,
            DocumentStatus::Published
        );
    }
}
