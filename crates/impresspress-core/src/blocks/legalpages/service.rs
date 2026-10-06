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
//! so version numbering and archiving exist exactly once. They are also the
//! *only* way a document's `status` or `version` changes: the repo writes both
//! from one function, [`documents::publish`], and this file is its sole
//! caller.

use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{
    contracts::{DocumentStatus, DocumentType},
    repo::documents::{self, DocumentRow, PublishOutcome, PublishSource},
};

/// Inputs for [`publish_document`]. There is no version: the server numbers
/// every publish, one past the highest version of the type.
pub(super) struct PublishRequest<'a> {
    /// Which document this publishes: the type a new row is created as, and
    /// the type an existing `doc_id` must be.
    pub doc_type: DocumentType,
    /// Existing document to publish; empty string = create a new one.
    pub doc_id: &'a str,
    /// New title from the editor; `None` keeps the stored value
    /// (JSON API publish path).
    pub title: Option<&'a str>,
    /// New content from the editor; `None` keeps the stored value.
    pub content: Option<&'a str>,
    /// Recorded as `created_by` when a new row is created.
    pub created_by: &'a str,
}

/// Outcome of a successful [`publish_document`] call.
pub(super) struct Published {
    /// The published document as stored.
    pub row: DocumentRow,
    /// The version it was published as.
    pub version: i64,
}

/// Why a publish did not happen.
#[derive(Debug)]
pub(super) enum PublishError {
    /// `doc_id` names no document.
    NotFound,
    /// `doc_id` is a document of another type than the request's.
    WrongType,
    /// The draft was published by someone else between the read and the
    /// write; reloading shows what is live now.
    AlreadyPublished,
    /// A database failure, including a version number another publish kept
    /// taking first ([`PUBLISH_ATTEMPTS`] times in a row).
    Db(WaferError),
}

impl From<WaferError> for PublishError {
    fn from(e: WaferError) -> Self {
        Self::Db(e)
    }
}

/// How many times a publish reads the next version and tries to take it
/// before reporting the clash. Each retry means another publish of the same
/// type committed in between, so a few cover any realistic contention.
const PUBLISH_ATTEMPTS: usize = 5;

/// Publish a document as the next version of its type and archive the
/// version it replaces.
///
/// A draft is published in place. Anything else — no `doc_id` (a type's
/// first version), or the live or an archived row — is published as a new
/// row carrying that row's text (or the editor's), so a published version is
/// never rewritten: the version it replaces stays, archived, as it was.
///
/// The number is `latest_version + 1`, taken by the write itself under the
/// unique `(doc_type, version)` index; when a concurrent publish took it
/// first, the write fails as a whole and this reads the next number and
/// tries again.
pub(super) async fn publish_document(
    ctx: &dyn Context,
    req: PublishRequest<'_>,
) -> Result<Published, PublishError> {
    let existing = if req.doc_id.is_empty() {
        None
    } else {
        let row = documents::get(ctx, req.doc_id)
            .await?
            .ok_or(PublishError::NotFound)?;
        if row.doc_type != req.doc_type {
            return Err(PublishError::WrongType);
        }
        Some(row)
    };

    let mut attempt = 0;
    loop {
        attempt += 1;
        let version = documents::latest_version(ctx, req.doc_type).await? + 1;
        let source = match &existing {
            Some(row) if row.status == DocumentStatus::Draft => PublishSource::Draft {
                id: &row.id,
                title: req.title,
                content: req.content,
            },
            Some(row) => PublishSource::New {
                title: req.title.unwrap_or(&row.title),
                content: req.content.unwrap_or(&row.content),
                created_by: req.created_by,
            },
            None => PublishSource::New {
                title: req.title.unwrap_or_default(),
                content: req.content.unwrap_or_default(),
                created_by: req.created_by,
            },
        };
        match documents::publish(ctx, req.doc_type, version, source).await {
            Ok(PublishOutcome::Published(row)) => return Ok(Published { row, version }),
            Ok(PublishOutcome::NotADraft) => return Err(PublishError::AlreadyPublished),
            Err(e) if e.code == ErrorCode::AlreadyExists && attempt < PUBLISH_ATTEMPTS => continue,
            Err(e) => return Err(PublishError::Db(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        super::{seed_doc, stored, test_ctx},
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
    /// branch) — must produce unnumbered drafts of the identical stored shape,
    /// because both go through the one `documents::insert_draft`.
    #[tokio::test]
    async fn both_create_surfaces_produce_identical_unnumbered_drafts() {
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
            assert_eq!(doc.version, 0);
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

    /// Numbering and archiving are per type: a type's first publish is v1
    /// whatever another type has reached, and leaves that type alone.
    #[tokio::test]
    async fn numbering_and_archiving_are_per_type() {
        let ctx = test_ctx().await;
        let other_type = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Terms",
            DocumentStatus::Published,
            6,
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
                created_by: "admin_1",
            },
        )
        .await
        .expect("publish the first privacy version");

        assert_eq!(published.version, 1);

        // Archiving is scoped to the published doc_type.
        assert_eq!(
            stored(&ctx, &other_type.id).await.status,
            DocumentStatus::Published
        );
    }

    /// Publishing the live version (no draft) makes a new row: the version it
    /// replaces is archived exactly as it was, never rewritten in place.
    #[tokio::test]
    async fn publishing_the_live_version_archives_it_intact() {
        let ctx = test_ctx().await;
        let live = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Live Terms",
            DocumentStatus::Published,
            3,
        )
        .await;

        let published = publish_document(
            &ctx,
            PublishRequest {
                doc_type: DocumentType::Terms,
                doc_id: &live.id,
                title: Some("Edited Terms"),
                content: Some("edited body"),
                created_by: "admin_1",
            },
        )
        .await
        .expect("publish over the live version");

        assert_eq!(published.version, 4);
        assert_ne!(published.row.id, live.id);
        assert_eq!(published.row.content, "edited body");
        assert_eq!(published.row.status, DocumentStatus::Published);

        let archived = stored(&ctx, &live.id).await;
        assert_eq!(archived.status, DocumentStatus::Archived);
        assert_eq!(archived.version, 3);
        assert_eq!(archived.title, "Live Terms");
        assert_eq!(archived.content, live.content);
        assert_eq!(archived.published_at, live.published_at);
    }

    /// The JSON API's publish of an archived version republishes its text as
    /// the next number and leaves the archived row as it was.
    #[tokio::test]
    async fn republishing_an_archived_version_copies_it() {
        let ctx = test_ctx().await;
        let old = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Old Terms",
            DocumentStatus::Archived,
            1,
        )
        .await;
        let live = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Live Terms",
            DocumentStatus::Published,
            2,
        )
        .await;

        let published = publish_document(
            &ctx,
            PublishRequest {
                doc_type: DocumentType::Terms,
                doc_id: &old.id,
                title: None,
                content: None,
                created_by: "admin_1",
            },
        )
        .await
        .expect("republish");

        assert_eq!(published.version, 3);
        assert_eq!(published.row.title, "Old Terms");
        assert_eq!(stored(&ctx, &old.id).await, old);
        assert_eq!(
            stored(&ctx, &live.id).await.status,
            DocumentStatus::Archived
        );
    }

    /// A draft another publish has already taken is not published again,
    /// and nothing is archived.
    #[tokio::test]
    async fn a_draft_that_is_no_longer_a_draft_publishes_nothing() {
        let ctx = test_ctx().await;
        let live = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Live Terms",
            DocumentStatus::Published,
            2,
        )
        .await;
        let taken = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Taken",
            DocumentStatus::Published,
            1,
        )
        .await;

        let outcome = documents::publish(
            &ctx,
            DocumentType::Terms,
            3,
            PublishSource::Draft {
                id: &taken.id,
                title: None,
                content: None,
            },
        )
        .await
        .expect("the write runs");

        assert!(matches!(outcome, PublishOutcome::NotADraft));
        assert_eq!(stored(&ctx, &taken.id).await, taken);
        assert_eq!(stored(&ctx, &live.id).await, live);
    }

    /// A number another publish already took fails the write, before the
    /// archive step runs, so the live version stays live.
    #[tokio::test]
    async fn a_taken_version_number_changes_nothing() {
        let ctx = test_ctx().await;
        let live = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Live Terms",
            DocumentStatus::Published,
            4,
        )
        .await;
        let draft = seed_doc(&ctx, DocumentType::Terms, "Next", DocumentStatus::Draft, 0).await;

        let err = documents::publish(
            &ctx,
            DocumentType::Terms,
            4,
            PublishSource::Draft {
                id: &draft.id,
                title: None,
                content: None,
            },
        )
        .await
        .err()
        .expect("v4 is taken");

        assert_eq!(err.code, ErrorCode::AlreadyExists, "{err:?}");
        assert_eq!(stored(&ctx, &live.id).await, live);
        assert_eq!(stored(&ctx, &draft.id).await, draft);
    }

    /// Two publishes of one type at once: each gets its own number (the one
    /// that loses the race reads the next and retries), the higher one is
    /// live, and the type is never left with nothing published.
    #[tokio::test]
    async fn concurrent_publishes_number_apart_and_leave_one_live() {
        let ctx = test_ctx().await;
        seed_doc(
            &ctx,
            DocumentType::Terms,
            "Live Terms",
            DocumentStatus::Published,
            3,
        )
        .await;
        let a = seed_doc(&ctx, DocumentType::Terms, "A", DocumentStatus::Draft, 0).await;
        let b = seed_doc(&ctx, DocumentType::Terms, "B", DocumentStatus::Draft, 0).await;
        let request = |id| PublishRequest {
            doc_type: DocumentType::Terms,
            doc_id: id,
            title: None,
            content: None,
            created_by: "admin_1",
        };

        let (first, second) = tokio::join!(
            publish_document(&ctx, request(&a.id)),
            publish_document(&ctx, request(&b.id)),
        );
        let mut versions = vec![
            first.expect("first publish").version,
            second.expect("second publish").version,
        ];
        versions.sort_unstable();
        assert_eq!(versions, vec![4, 5]);

        let live = documents::list_published(&ctx, DocumentType::Terms)
            .await
            .expect("list published");
        assert_eq!(live.len(), 1, "exactly one live version");
        assert_eq!(live[0].version, 5);
    }
}
