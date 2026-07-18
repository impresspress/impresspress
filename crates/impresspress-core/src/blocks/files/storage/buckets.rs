//! Bucket lifecycle: list, create, delete. Bucket-name validation lives in
//! [`super::validation`]; ownership/access-control in [`super::access`].

use wafer_core::clients::storage as store;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{
    access::is_bucket_access_denied, params::extract_bucket_name, validation::is_valid_bucket_name,
};
use crate::{
    blocks::files::repo,
    http::{err_bad_request, err_forbidden, err_internal, ok_json},
};

pub(super) async fn handle_list_buckets(ctx: &dyn Context, msg: &Message) -> OutputStream {
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
        Ok(records) => {
            let names: Vec<&str> = records
                .iter()
                .filter_map(|r| r.data.get("name").and_then(|v| v.as_str()))
                .collect();
            ok_json(&serde_json::json!({"buckets": names}))
        }
        Err(e) => err_internal("Database error", e),
    }
}

pub(super) async fn handle_create_bucket(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        name: String,
        #[serde(default)]
        public: bool,
    }
    let raw = input.collect_to_bytes().await;
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

    // Create the blob-namespace folder first, then record the metadata row.
    if let Err(e) = store::create_folder(ctx, &body.name, body.public).await {
        return err_internal("Failed to create bucket", e);
    }

    // [`repo::buckets::TABLE`] is the source of truth for bucket existence,
    // so the metadata insert must succeed for the bucket to count as created.
    // If it fails, compensate by deleting the just-created folder rather than
    // warn-and-continue (which would leave an orphan folder invisible to every
    // listing path, which now all read the table).
    if let Err(e) = repo::buckets::insert(ctx, &body.name, body.public, msg.user_id()).await {
        if let Err(cleanup) = store::delete_folder(ctx, &body.name).await {
            tracing::error!(
                bucket = %body.name,
                error = %cleanup,
                "failed to roll back orphan storage folder after bucket metadata insert failed",
            );
        }
        return err_internal("Failed to create bucket", e);
    }
    ok_json(&serde_json::json!({"name": body.name, "created": true}))
}

pub(super) async fn handle_delete_bucket(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let bucket = extract_bucket_name(msg);
    let bucket = bucket.as_str();
    if bucket.is_empty() {
        return err_bad_request("Missing bucket name");
    }
    if !is_valid_bucket_name(bucket) {
        return err_bad_request("Invalid bucket name");
    }
    if is_bucket_access_denied(ctx, msg, bucket).await {
        return err_forbidden("Access denied to this bucket");
    }

    match store::delete_folder(ctx, bucket).await {
        Ok(()) => {
            // Clean up DB metadata for the bucket and its objects
            repo::buckets::delete_by_name(ctx, bucket).await.ok();
            repo::objects::delete_for_bucket(ctx, bucket).await.ok();
            ok_json(&serde_json::json!({"deleted": true}))
        }
        Err(e) => err_internal("Failed to delete bucket", e),
    }
}

#[cfg(test)]
mod integration_tests {
    use super::{super::test_helpers::seed_bucket, *};
    use crate::test_support::{admin_msg, auth_msg, output_json, TestContext};

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
}
