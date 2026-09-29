//! Bucket lifecycle: list, create, delete. Bucket-name validation lives in
//! [`super::validation`]; ownership/access-control in [`super::access`].

use wafer_core::clients::storage as store;
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::{
    access::require_bucket_access, params::extract_bucket_name, validation::is_valid_bucket_name,
};
use crate::{
    blocks::{
        crud,
        files::{contracts, repo},
    },
    http::{err_bad_request, ok_json},
};

pub(in crate::blocks::files) async fn handle_list_buckets(
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    // [`repo::buckets::TABLE`] is the single source of truth for bucket
    // existence / ownership / visibility. Both the admin and user branches
    // read it (the admin sees every bucket, the user only their own) —
    // storage folders are a blob namespace, not a directory we enumerate
    // here, so the admin list no longer diverges from `store::list_folders`.
    let owner = if crate::util::is_admin(msg) {
        None
    } else {
        Some(msg.user_id())
    };
    match repo::buckets::list_visible(ctx, owner).await {
        Ok(rows) => ok_json(&contracts::BucketListResponse {
            truncated: rows.truncated,
            buckets: rows.rows.into_iter().map(|r| r.name).collect(),
        }),
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

pub(in crate::blocks::files) async fn handle_create_bucket(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    // `deny_unknown_fields`: an unknown key is a caller asking for a bucket
    // property this handler does not set, and answering 200 would claim it
    // was applied.
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Req {
        name: String,
        #[serde(default)]
        public: bool,
    }
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    if body.name.is_empty() {
        return err_bad_request("Bucket name is required");
    }
    if !is_valid_bucket_name(&body.name) {
        return err_bad_request("Invalid bucket name");
    }

    // The metadata row goes in FIRST, because it is the claim on the name:
    // `buckets.name` is UNIQUE (migration 002), so the insert is what decides
    // which of two users asking for the same name gets the folder, and it
    // decides it without a gap a competing create could slip through. A taken
    // name is a 409 — the key is held, pick another — and no storage call has
    // happened by then.
    //
    // Creating the folder first cannot be made safe, which is why the order is
    // this way round: a bucket name IS the blob-namespace folder name, every
    // backend's `create_folder` is idempotent, so a second user's create
    // succeeded against the first user's folder — and the compensating
    // `delete_folder` below would then have deleted that user's data on the
    // way to reporting the failure.
    let row = match repo::buckets::insert(ctx, &body.name, body.public, msg.user_id()).await {
        Ok(row) => row,
        Err(e) => {
            return crud::taken_key_or_db_error(
                e,
                crud::TakenKey::new("bucket", "name", &body.name),
                "Failed to create bucket",
            )
        }
    };

    // The row is the source of truth for bucket existence, so a bucket whose
    // folder could not be created must not keep its row: it would list a
    // namespace no object can be written to. Roll the claim back rather than
    // warn-and-continue.
    //
    // By ROW ID, not by name. A name-scoped delete is only safe while the
    // unique index exists, and a deployment that takes this code without
    // `--run-migrations` has the handler but not the index: there a second
    // user's create on a taken name still inserts, and rolling back by name
    // would delete the first owner's row too — trading the folder this
    // ordering protects for the row that lists it.
    if let Err(e) = store::create_folder(ctx, &body.name, body.public).await {
        if let Err(cleanup) = repo::buckets::delete(ctx, &row.id).await {
            tracing::error!(
                bucket = %body.name,
                bucket_id = %row.id,
                error = %cleanup,
                "failed to roll back the bucket row after its storage folder could not be created",
            );
        }
        return crud::db_error_internal(e, "Failed to create bucket");
    }
    ok_json(&contracts::BucketCreatedResponse {
        name: body.name,
        created: true,
    })
}

pub(in crate::blocks::files) async fn handle_delete_bucket(
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    let bucket = match extract_bucket_name(msg) {
        Ok(value) => value,
        Err(response) => return response,
    };
    if !is_valid_bucket_name(bucket) {
        return err_bad_request("Invalid bucket name");
    }
    if let Err(refusal) = require_bucket_access(ctx, msg, bucket).await {
        return refusal;
    }

    // Storage first, tolerating "already gone": if an earlier attempt removed
    // the folder but failed the metadata cleanup below, the retry must still
    // reach that cleanup.
    match store::delete_folder(ctx, bucket).await {
        Ok(()) => {}
        Err(e) if e.code == ErrorCode::NotFound => {}
        // The tail is the one mapping even though the call is to STORAGE,
        // not the database: the codes are the same set, and a
        // `PermissionDenied` from the storage service is a refusal the
        // caller can act on, not the 500 it used to ship as.
        Err(e) => return crud::db_error_internal(e, "Failed to delete bucket"),
    }

    // Metadata cleanup is reported, never swallowed: surviving object rows
    // keep charging quota and a surviving bucket row keeps listing a folder
    // that is gone. Object rows go first and the bucket row last — the
    // bucket row is what the access check above reads, so keeping it until
    // everything else is clean is what lets the owner retry.
    if let Err(e) = repo::objects::delete_for_bucket(ctx, bucket).await {
        return crud::db_error_internal(
            e,
            "Bucket deleted but its object records could not be removed",
        );
    }
    if let Err(e) = repo::buckets::delete_by_name(ctx, bucket).await {
        return crud::db_error_internal(e, "Bucket deleted but its record could not be removed");
    }
    ok_json(&contracts::DeletedResponse { deleted: true })
}

#[cfg(test)]
mod integration_tests {
    use super::{
        super::test_helpers::{
            ctx_with_storage, ctx_with_storage_handle, ctx_with_storage_without_the_unique_index,
            seed_bucket, seed_object_row,
        },
        *,
    };
    use crate::test_support::{
        admin_msg, auth_msg, output_http_status, output_is_error, output_json, FailingDbOpContext,
        TestContext,
    };

    fn bucket_names(v: &serde_json::Value) -> Vec<String> {
        v.get("buckets")
            .and_then(|b| b.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Single source of truth: the admin bucket listing now reads
    /// [`repo::buckets::TABLE`] (every bucket) instead of `store::list_folders`,
    /// can no longer diverge from the per-user listing that already read the
    /// table. An admin sees all buckets regardless of owner.
    #[tokio::test]
    async fn admin_list_buckets_reads_metadata_table_for_all_owners() {
        let ctx = TestContext::with_files().await;
        seed_bucket(&ctx, "alice-bucket", "alice").await;
        seed_bucket(&ctx, "bob-bucket", "bob").await;

        let out = handle_list_buckets(&ctx, &admin_msg("retrieve", "/storage/buckets")).await;
        let mut names = bucket_names(&output_json(out).await);
        names.sort();
        assert_eq!(names, vec!["alice-bucket", "bob-bucket"]);
    }

    /// A non-admin user sees only the buckets they own (same table, filtered).
    #[tokio::test]
    async fn user_list_buckets_is_owner_scoped() {
        let ctx = TestContext::with_files().await;
        seed_bucket(&ctx, "alice-bucket", "alice").await;
        seed_bucket(&ctx, "bob-bucket", "bob").await;

        let out =
            handle_list_buckets(&ctx, &auth_msg("retrieve", "/storage/buckets", "alice")).await;
        let names = bucket_names(&output_json(out).await);
        assert_eq!(names, vec!["alice-bucket"]);
    }

    /// Build the request body + message the router produces for
    /// `POST /b/storage/api/buckets`, as the create-bucket modal sends it
    /// (`files-browser.js`: `{"name": …, "public": …}`).
    fn create_bucket_msg(user: &str) -> Message {
        let mut msg = auth_msg("create", "/b/storage/api/buckets", user);
        msg.set_meta("req.content_type", "application/json");
        msg
    }

    fn create_bucket_body(name: &str, public: bool) -> InputStream {
        InputStream::from_bytes(
            serde_json::json!({ "name": name, "public": public })
                .to_string()
                .into_bytes(),
        )
    }

    /// A bucket name IS the blob-namespace folder name, so a second user
    /// creating a name someone else already holds used to be handed the first
    /// user's folder: `create_folder` is idempotent on every backend, the
    /// `buckets` table had no unique index, so the insert succeeded and
    /// `find_owned` then answered for the squatter. From there they could list
    /// and read every object in it, overwrite them, and delete the bucket —
    /// which deletes the folder.
    ///
    /// The name is refused with a 409 instead, and nothing about the first
    /// owner's bucket changes.
    #[tokio::test]
    async fn a_taken_bucket_name_is_refused_not_shared_with_the_second_creator() {
        let ctx = ctx_with_storage().await;
        let created = handle_create_bucket(
            &ctx,
            &create_bucket_msg("alice"),
            create_bucket_body("assets", false),
        )
        .await;
        assert_eq!(
            output_json(created).await["created"],
            serde_json::json!(true)
        );
        store::put(&ctx, "assets", "secret.txt", b"alice's bytes", "text/plain")
            .await
            .expect("alice stores an object");

        let taken = handle_create_bucket(
            &ctx,
            &create_bucket_msg("mallory"),
            create_bucket_body("assets", false),
        )
        .await;

        assert_eq!(
            output_http_status(taken).await,
            409,
            "a bucket name someone else holds is a conflict, not a second owner",
        );
        let told = crate::test_support::output_http_json(
            handle_create_bucket(
                &ctx,
                &create_bucket_msg("mallory"),
                create_bucket_body("assets", false),
            )
            .await,
        )
        .await;
        assert_eq!(
            told["message"],
            serde_json::json!(
                "A bucket with the name \"assets\" already exists. Choose a different name."
            ),
            "the refusal names the field and the value, and nothing of the schema",
        );
        assert!(
            repo::buckets::find_owned(&ctx, "assets", "mallory")
                .await
                .expect("bucket lookup")
                .is_none(),
            "the refused create must not leave mallory owning alice's bucket",
        );
        assert!(
            repo::buckets::find_owned(&ctx, "assets", "alice")
                .await
                .expect("bucket lookup")
                .is_some(),
            "alice must still own her bucket",
        );
        let (bytes, _) = store::get(&ctx, "assets", "secret.txt")
            .await
            .expect("alice's object must survive the refused create");
        assert_eq!(bytes, b"alice's bytes");
    }

    /// The row is the claim on the name, so it goes in first — which means a
    /// bucket whose folder could not be created must not keep its row. A
    /// surviving row would list a namespace no object can be written to, and
    /// would hold the name against the owner's own retry.
    #[tokio::test]
    async fn a_bucket_whose_folder_cannot_be_created_keeps_no_row() {
        let (ctx, storage) = ctx_with_storage_handle().await;
        storage.refuse("create_folder");

        let out = handle_create_bucket(
            &ctx,
            &create_bucket_msg("alice"),
            create_bucket_body("assets", false),
        )
        .await;

        assert!(output_is_error(out, "Internal").await);
        assert!(
            repo::buckets::find_owned(&ctx, "assets", "alice")
                .await
                .expect("bucket lookup")
                .is_none(),
            "the claim must be rolled back when the folder could not be created",
        );
    }

    /// The rollback deletes the row it inserted, not every row with that name.
    ///
    /// `RELEASE.md` anticipates a deployment that takes this code without
    /// `--run-migrations`: the handler is there, the unique index is not, and
    /// a second user's create on a taken name still inserts. If the folder
    /// then fails, a name-scoped rollback would delete the FIRST owner's row
    /// too — their bucket disappears from every listing while their objects
    /// keep charging their quota. That trades the folder this ordering
    /// protects for the row that lists it, so the rollback is by id and does
    /// not depend on the index at all.
    #[tokio::test]
    async fn the_create_rollback_removes_only_the_row_it_inserted() {
        let (ctx, storage) = ctx_with_storage_without_the_unique_index().await;
        let created = handle_create_bucket(
            &ctx,
            &create_bucket_msg("alice"),
            create_bucket_body("assets", false),
        )
        .await;
        assert_eq!(
            output_json(created).await["created"],
            serde_json::json!(true)
        );

        storage.refuse("create_folder");
        let out = handle_create_bucket(
            &ctx,
            &create_bucket_msg("mallory"),
            create_bucket_body("assets", false),
        )
        .await;

        assert!(output_is_error(out, "Internal").await);
        assert!(
            repo::buckets::find_owned(&ctx, "assets", "alice")
                .await
                .expect("bucket lookup")
                .is_some(),
            "the first owner's bucket must survive somebody else's failed create",
        );
        assert!(
            repo::buckets::find_owned(&ctx, "assets", "mallory")
                .await
                .expect("bucket lookup")
                .is_none(),
            "and the row the failed create inserted must be gone",
        );
    }

    /// Build the message the router produces for
    /// `DELETE /b/storage/api/buckets/{bucket}`.
    fn delete_bucket_msg(bucket: &str) -> Message {
        let mut msg = auth_msg(
            "delete",
            &format!("/b/storage/api/buckets/{bucket}"),
            "alice",
        );
        msg.set_meta("req.param.name", bucket);
        msg
    }

    /// A bucket `alice` owns: metadata row, storage folder, and one object.
    async fn ctx_with_owned_bucket() -> TestContext {
        let ctx = ctx_with_storage().await;
        seed_bucket(&ctx, "assets", "alice").await;
        store::create_folder(&ctx, "assets", false)
            .await
            .expect("create folder");
        store::put(&ctx, "assets", "pic.png", b"PNGDATA", "image/png")
            .await
            .expect("seed object");
        seed_object_row(&ctx, "assets", "pic.png", "alice", 7).await;
        ctx
    }

    async fn bucket_row_exists(ctx: &TestContext) -> bool {
        repo::buckets::find_owned(ctx, "assets", "alice")
            .await
            .expect("bucket lookup")
            .is_some()
    }

    #[tokio::test]
    async fn delete_bucket_removes_folder_and_metadata_rows() {
        let ctx = ctx_with_owned_bucket().await;

        let out = handle_delete_bucket(&ctx, &delete_bucket_msg("assets")).await;

        assert_eq!(output_json(out).await["deleted"], serde_json::json!(true));
        assert!(
            !bucket_row_exists(&ctx).await,
            "the bucket row must be gone"
        );
        assert_eq!(
            repo::objects::count_for_uploader(&ctx, "alice")
                .await
                .expect("count"),
            0,
            "the object rows must be gone"
        );
    }

    /// A surviving bucket row keeps listing a folder that is gone, and
    /// surviving object rows keep charging quota; the failure is reported.
    #[tokio::test]
    async fn delete_bucket_reports_metadata_cleanup_failure() {
        let ctx = ctx_with_owned_bucket().await;
        let failing = FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.delete_where", repo::buckets::TABLE)],
        );

        let out = handle_delete_bucket(&failing, &delete_bucket_msg("assets")).await;

        assert!(
            output_is_error(out, "Internal").await,
            "a metadata cleanup failure must not be reported as a successful delete"
        );
        assert!(
            bucket_row_exists(&ctx).await,
            "the bucket row survives, so the owner can retry"
        );
    }

    /// The folder may already be gone when the owner retries after a failed
    /// cleanup; the retry must still finish instead of failing on the folder.
    #[tokio::test]
    async fn delete_bucket_retry_finishes_cleanup_after_partial_failure() {
        let ctx = ctx_with_owned_bucket().await;
        let failing = FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.delete_where", repo::buckets::TABLE)],
        );
        let first = handle_delete_bucket(&failing, &delete_bucket_msg("assets")).await;
        assert!(output_is_error(first, "Internal").await);

        let retry = handle_delete_bucket(&ctx, &delete_bucket_msg("assets")).await;

        assert_eq!(
            output_json(retry).await["deleted"],
            serde_json::json!(true),
            "the retry must complete the cleanup"
        );
        assert!(!bucket_row_exists(&ctx).await);
    }
}
