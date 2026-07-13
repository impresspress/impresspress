//! Publish/archive business logic for the legalpages block.
//!
//! Plain async functions — no HTTP awareness (mirrors the messages block's
//! service layering). Both publish surfaces route through
//! [`publish_document`]:
//!
//! - the JSON API handler (`POST /b/legalpages/api/documents/{id}/publish`)
//! - the admin-UI editor handler (`POST /b/legalpages/admin/publish`)
//!
//! so the publish-then-archive ordering and version handling exist exactly
//! once.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, WaferError};

use super::COLLECTION;
use crate::util::json_map;

/// Inputs for [`publish_document`].
pub(super) struct PublishRequest<'a> {
    /// Document type (`terms` / `privacy`); drives version computation and
    /// which previously published siblings get archived.
    pub doc_type: &'a str,
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
    pub record: db::Record,
    /// The version it was published as.
    pub version: i64,
}

/// Publish a document, then archive previously published documents of the
/// same type.
///
/// Ordering matters: the new doc goes live first, and the archive pass
/// excludes it. Archiving up-front would leave the doc-type with no
/// published version if the publish step then failed.
pub(super) async fn publish_document(
    ctx: &dyn Context,
    req: PublishRequest<'_>,
) -> Result<Published, WaferError> {
    let version = if req.version > 0 {
        req.version
    } else {
        latest_version(ctx, req.doc_type).await + 1
    };

    let now = crate::util::now_rfc3339();
    let record = if req.doc_id.is_empty() {
        let mut data = json_map(serde_json::json!({
            "doc_type": req.doc_type,
            "status": "published",
            "version": version,
            "created_at": now,
            "updated_at": now,
            "published_at": now,
            "created_by": req.created_by,
        }));
        insert_opt(&mut data, "title", req.title);
        insert_opt(&mut data, "content", req.content);
        db::create(ctx, COLLECTION, data).await?
    } else {
        let mut data = json_map(serde_json::json!({
            "status": "published",
            "version": version,
            "published_at": now,
            "updated_at": now,
        }));
        insert_opt(&mut data, "title", req.title);
        insert_opt(&mut data, "content", req.content);
        db::update(ctx, COLLECTION, req.doc_id, data).await?
    };

    // New doc is live; safe to archive earlier published siblings now.
    archive_published(ctx, req.doc_type, &record.id).await;

    Ok(Published { record, version })
}

/// Create a new version-1 draft document.
///
/// Single owner of the draft-creation shape (`status: "draft"`,
/// `version: 1`, creation timestamps, `created_by`) — both create surfaces
/// delegate here so the shape can't drift:
///
/// - the JSON API handler (`POST /b/legalpages/api/documents`)
/// - the admin-UI editor save (`POST /b/legalpages/admin/save`, create branch)
pub(super) async fn create_draft(
    ctx: &dyn Context,
    doc_type: &str,
    title: &str,
    content: &str,
    created_by: &str,
) -> Result<db::Record, WaferError> {
    let mut data = json_map(serde_json::json!({
        "doc_type": doc_type,
        "title": title,
        "content": content,
        "status": "draft",
        "version": 1,
        "created_by": created_by,
    }));
    crate::util::stamp_created(&mut data);
    db::create(ctx, COLLECTION, data).await
}

/// Read a document's `version` field, tolerating integer or string storage.
pub(super) fn doc_version(record: &db::Record) -> Option<i64> {
    let v = record.data.get("version")?;
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Highest version recorded for any document of `doc_type` (0 when none).
async fn latest_version(ctx: &dyn Context, doc_type: &str) -> i64 {
    let opts = ListOptions {
        filters: vec![Filter {
            field: "doc_type".into(),
            operator: FilterOp::Equal,
            value: serde_json::json!(doc_type),
        }],
        sort: vec![SortField {
            field: "version".into(),
            desc: true,
        }],
        limit: 1,
        ..Default::default()
    };
    db::list(ctx, COLLECTION, &opts)
        .await
        .ok()
        .and_then(|r| r.records.first().and_then(doc_version))
        .unwrap_or(0)
}

/// Archive all published documents of a given type, except `except_id`
/// (the document that was just published).
async fn archive_published(ctx: &dyn Context, doc_type: &str, except_id: &str) {
    let existing = db::list_all(
        ctx,
        COLLECTION,
        vec![
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
    )
    .await;
    if let Ok(records) = existing {
        for r in records {
            if r.id == except_id {
                continue;
            }
            let upd = json_map(serde_json::json!({"status": "archived"}));
            if let Err(e) = db::update(ctx, COLLECTION, &r.id, upd).await {
                tracing::warn!("Failed to archive previous legal page version: {e}");
            }
        }
    }
}

fn insert_opt(data: &mut HashMap<String, serde_json::Value>, key: &str, value: Option<&str>) {
    if let Some(v) = value {
        data.insert(key.to_string(), serde_json::Value::String(v.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestContext;

    async fn ctx_with_schema() -> TestContext {
        use super::super::migrations;
        let ctx = TestContext::with_admin().await;
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

    async fn seed_doc(
        ctx: &TestContext,
        doc_type: &str,
        title: &str,
        status: &str,
        version: i64,
    ) -> db::Record {
        let now = crate::util::now_rfc3339();
        let data = json_map(serde_json::json!({
            "doc_type": doc_type,
            "title": title,
            "content": "body",
            "status": status,
            "version": version,
            "created_at": now,
            "updated_at": now,
            "created_by": "seed",
        }));
        db::create(ctx, COLLECTION, data).await.expect("seed doc")
    }

    #[tokio::test]
    async fn publish_existing_doc_auto_increments_and_archives_previous() {
        let ctx = ctx_with_schema().await;
        let live = seed_doc(&ctx, "terms", "Old Terms", "published", 3).await;
        let draft = seed_doc(&ctx, "terms", "New Terms", "draft", 1).await;

        let published = publish_document(
            &ctx,
            PublishRequest {
                doc_type: "terms",
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
        assert_eq!(published.record.id, draft.id);

        // The just-published doc must NOT be archived (except_id guard) and
        // keeps its stored title (JSON publish path passes None).
        let now_live = db::get(&ctx, COLLECTION, &draft.id).await.expect("get");
        assert_eq!(
            now_live.data.get("status").and_then(|v| v.as_str()),
            Some("published")
        );
        assert_eq!(doc_version(&now_live), Some(4));
        assert_eq!(
            now_live.data.get("title").and_then(|v| v.as_str()),
            Some("New Terms")
        );

        // The previously published sibling is archived.
        let archived = db::get(&ctx, COLLECTION, &live.id).await.expect("get");
        assert_eq!(
            archived.data.get("status").and_then(|v| v.as_str()),
            Some("archived")
        );
    }

    #[tokio::test]
    async fn publish_new_doc_creates_published_and_archives_previous() {
        let ctx = ctx_with_schema().await;
        let live = seed_doc(&ctx, "privacy", "Old Policy", "published", 1).await;

        let published = publish_document(
            &ctx,
            PublishRequest {
                doc_type: "privacy",
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
        assert_ne!(published.record.id, live.id);

        let created = db::get(&ctx, COLLECTION, &published.record.id)
            .await
            .expect("get created");
        assert_eq!(
            created.data.get("status").and_then(|v| v.as_str()),
            Some("published")
        );
        assert_eq!(
            created.data.get("title").and_then(|v| v.as_str()),
            Some("New Policy")
        );
        assert_eq!(
            created.data.get("created_by").and_then(|v| v.as_str()),
            Some("admin_1")
        );

        let archived = db::get(&ctx, COLLECTION, &live.id).await.expect("get");
        assert_eq!(
            archived.data.get("status").and_then(|v| v.as_str()),
            Some("archived")
        );
    }

    /// Both create surfaces — the JSON API (`POST /b/legalpages/api/documents`)
    /// and the admin editor save (`POST /b/legalpages/admin/save`, create
    /// branch) — must produce version-1 drafts of the identical stored shape,
    /// because both delegate to the one [`create_draft`] fn.
    #[tokio::test]
    async fn both_create_surfaces_produce_version_1_drafts_via_create_draft() {
        use wafer_run::InputStream;

        use crate::test_support::{admin_msg, output_json};

        let ctx = ctx_with_schema().await;
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

        let api_doc = db::get(&ctx, COLLECTION, &api_id).await.expect("api doc");
        let save_doc = db::get(&ctx, COLLECTION, &save_id).await.expect("save doc");
        for doc in [&api_doc, &save_doc] {
            assert_eq!(
                doc.data.get("status").and_then(|v| v.as_str()),
                Some("draft")
            );
            assert_eq!(doc_version(doc), Some(1));
            assert_eq!(
                doc.data.get("created_by").and_then(|v| v.as_str()),
                Some("admin_1")
            );
        }

        // Identical stored field set — the draft shape exists exactly once.
        let keys = |r: &db::Record| {
            let mut k: Vec<&str> = r.data.keys().map(String::as_str).collect();
            k.sort_unstable();
            k.into_iter().map(str::to_owned).collect::<Vec<_>>()
        };
        assert_eq!(
            keys(&api_doc),
            keys(&save_doc),
            "both surfaces must store the same draft shape"
        );
    }

    #[tokio::test]
    async fn publish_respects_explicit_version_and_other_doc_types_untouched() {
        let ctx = ctx_with_schema().await;
        let other_type = seed_doc(&ctx, "terms", "Terms", "published", 1).await;
        let draft = seed_doc(&ctx, "privacy", "Policy", "draft", 1).await;

        let published = publish_document(
            &ctx,
            PublishRequest {
                doc_type: "privacy",
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
        let untouched = db::get(&ctx, COLLECTION, &other_type.id)
            .await
            .expect("get");
        assert_eq!(
            untouched.data.get("status").and_then(|v| v.as_str()),
            Some("published")
        );
    }
}
