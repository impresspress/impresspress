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

use wafer_run::{context::Context, WaferError};

use super::{
    contracts::{DocumentStatus, DocumentType},
    repo::documents::{self, DocumentRow, NewDraft, PublishOutcome, PublishSource},
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
    /// `doc_id` is the live version and the request carries no text, so the
    /// publish would only renumber the text already live. `draft` is the
    /// type's newest draft — what such a request usually meant to publish.
    AlreadyLive { version: i64, draft: Option<String> },
    /// The number kept being taken by concurrent publishes
    /// ([`PUBLISH_ATTEMPTS`] times in a row).
    Contended,
    /// A database failure.
    Db(WaferError),
}

impl From<WaferError> for PublishError {
    fn from(e: WaferError) -> Self {
        Self::Db(e)
    }
}

/// Apply an edit to `row`, the one rule for both edit surfaces (the editor's
/// Save and `PATCH /b/legalpages/api/documents/{id}`): a draft is edited in
/// place; a published or archived version is never changed — the edit is
/// saved as a new draft of the same type, starting from that version's text
/// (a `None` field keeps it). Returns the draft that holds the edit.
///
/// A draft that another publish took between the caller's read and this
/// write is, by then, a published version, so the same rule applies to it:
/// the edit lands in a new draft rather than being lost.
pub(super) async fn edit_text(
    ctx: &dyn Context,
    row: &DocumentRow,
    title: Option<&str>,
    content: Option<&str>,
    created_by: &str,
) -> Result<DocumentRow, WaferError> {
    if row.status == DocumentStatus::Draft {
        if let Some(draft) = documents::update_draft_text(ctx, &row.id, title, content).await? {
            return Ok(draft);
        }
    }
    documents::insert_draft(
        ctx,
        NewDraft {
            doc_type: row.doc_type,
            title: title.unwrap_or(&row.title),
            content: content.unwrap_or(&row.content),
            created_by,
        },
    )
    .await
}

/// How many times a publish reads the next version and tries to take it
/// before reporting the clash. Each retry means another publish of the same
/// type committed in between, so a few cover any realistic contention.
const PUBLISH_ATTEMPTS: usize = 5;

/// Publish a document as the next version of its type and archive the
/// version it replaces.
///
/// A draft is published in place. Anything else — no `doc_id` (a type's
/// first version), or an archived row — is published as a new row carrying
/// that row's text (or the request's), so a published version is never
/// rewritten: the version it replaces stays, archived, as it was. The live
/// row itself is republished only with new text; without any it is refused
/// ([`PublishError::AlreadyLive`]).
///
/// The number is `latest_version + 1`, taken by the write itself
/// ([`documents::publish`]); when a concurrent publish took it first, this
/// reads the next number and tries again.
pub(super) async fn publish_document(
    ctx: &dyn Context,
    req: PublishRequest<'_>,
) -> Result<Published, PublishError> {
    publish_numbered(ctx, req, None).await
}

/// [`publish_document`], with the first attempt's number given rather than
/// read when `first_number` is `Some` — the seam a test uses to stand in for
/// a concurrent publish having taken the number between the read and the
/// write. Every later attempt reads.
async fn publish_numbered(
    ctx: &dyn Context,
    req: PublishRequest<'_>,
    first_number: Option<i64>,
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
        if row.status == DocumentStatus::Published && req.title.is_none() && req.content.is_none() {
            return Err(PublishError::AlreadyLive {
                version: row.version,
                draft: documents::find_latest_draft(ctx, req.doc_type)
                    .await?
                    .map(|draft| draft.id),
            });
        }
        Some(row)
    };

    let mut next = first_number;
    for _ in 0..PUBLISH_ATTEMPTS {
        let version = match next.take() {
            Some(number) => number,
            None => documents::latest_version(ctx, req.doc_type).await? + 1,
        };
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
        match documents::publish(ctx, req.doc_type, version, source).await? {
            PublishOutcome::Published(row) => return Ok(Published { row, version }),
            PublishOutcome::NotADraft => return Err(PublishError::AlreadyPublished),
            PublishOutcome::NumberTaken => continue,
        }
    }
    Err(PublishError::Contended)
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

    /// A number another row already holds is not taken: the write changes
    /// nothing (the live version stays live) and says so.
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

        for source in [
            PublishSource::Draft {
                id: &draft.id,
                title: None,
                content: None,
            },
            PublishSource::New {
                title: "New",
                content: "body",
                created_by: "admin_1",
            },
        ] {
            let outcome = documents::publish(&ctx, DocumentType::Terms, 4, source)
                .await
                .expect("the write runs");
            assert!(matches!(outcome, PublishOutcome::NumberTaken));
        }
        assert_eq!(stored(&ctx, &live.id).await, live);
        assert_eq!(stored(&ctx, &draft.id).await, draft);
        assert_eq!(documents::count(&ctx).await.expect("count"), 2);
    }

    /// A publish that finds its number taken by a concurrent one reads the
    /// next number and takes that: the first attempt is made at the live
    /// version's own number, as if another publish had committed it between
    /// the read and the write.
    #[tokio::test]
    async fn a_publish_that_loses_its_number_retries_with_the_next() {
        let ctx = test_ctx().await;
        seed_doc(
            &ctx,
            DocumentType::Terms,
            "Live Terms",
            DocumentStatus::Published,
            3,
        )
        .await;
        let draft = seed_doc(&ctx, DocumentType::Terms, "Next", DocumentStatus::Draft, 0).await;

        let published = publish_numbered(
            &ctx,
            PublishRequest {
                doc_type: DocumentType::Terms,
                doc_id: &draft.id,
                title: None,
                content: None,
                created_by: "admin_1",
            },
            Some(3),
        )
        .await
        .expect("the retry publishes");

        assert_eq!(published.version, 4);
        assert_eq!(published.row.id, draft.id);
        let live = documents::list_published(&ctx, DocumentType::Terms)
            .await
            .expect("list published");
        assert_eq!(live.len(), 1, "exactly one live version");
        assert_eq!(live[0].id, draft.id);
    }

    /// Publishing the live version with no new text would only renumber what
    /// is already live — the usual meaning is "publish my draft" — so it is
    /// refused, naming the draft.
    #[tokio::test]
    async fn republishing_the_live_version_without_text_is_refused() {
        let ctx = test_ctx().await;
        let live = seed_doc(
            &ctx,
            DocumentType::Terms,
            "Live Terms",
            DocumentStatus::Published,
            3,
        )
        .await;
        let draft = seed_doc(&ctx, DocumentType::Terms, "Edit", DocumentStatus::Draft, 0).await;

        let err = publish_document(
            &ctx,
            PublishRequest {
                doc_type: DocumentType::Terms,
                doc_id: &live.id,
                title: None,
                content: None,
                created_by: "admin_1",
            },
        )
        .await
        .err()
        .expect("refused");

        match err {
            PublishError::AlreadyLive {
                version,
                draft: named,
            } => {
                assert_eq!(version, 3);
                assert_eq!(named, Some(draft.id.clone()));
            }
            other => panic!("expected AlreadyLive, got {other:?}"),
        }
        assert_eq!(stored(&ctx, &live.id).await, live);
        assert_eq!(documents::count(&ctx).await.expect("count"), 2);
    }

    /// An edit to a draft another publish took meanwhile is not lost: the
    /// row is a published version by then, so the edit lands in a new draft.
    #[tokio::test]
    async fn an_edit_to_a_draft_published_meanwhile_lands_in_a_new_draft() {
        let ctx = test_ctx().await;
        let draft = seed_doc(&ctx, DocumentType::Terms, "Draft", DocumentStatus::Draft, 0).await;
        // The caller read `draft`; another tab publishes it before the save.
        let taken = documents::set_state_for_test(&ctx, &draft.id, DocumentStatus::Published, 1)
            .await
            .expect("published elsewhere");

        let saved = edit_text(&ctx, &draft, None, Some("my edit"), "admin_1")
            .await
            .expect("the edit is kept");

        assert_ne!(saved.id, draft.id);
        assert_eq!(saved.status, DocumentStatus::Draft);
        assert_eq!(saved.title, "Draft");
        assert_eq!(saved.content, "my edit");
        assert_eq!(
            stored(&ctx, &draft.id).await,
            taken,
            "the published row is untouched"
        );
    }
}
