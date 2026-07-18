//! Admin storage JSON API, delegated from the admin block via `call_block`
//! on the real `/admin/storage/...` paths. Authorization is enforced by the
//! admin block's central tier before delegation.

use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::buckets::handle_list_buckets;
use crate::{
    blocks::files::repo,
    http::{err_not_found, ok_json},
};

pub async fn handle_admin(ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
    let action = msg.action();
    let path = msg.path();
    match (action, path) {
        ("retrieve", "/admin/storage/buckets") => handle_list_buckets(ctx, &msg).await,
        ("retrieve", "/admin/storage/stats") => handle_stats(ctx, &msg).await,
        _ => err_not_found("not found"),
    }
}

async fn handle_stats(ctx: &dyn Context, _msg: &Message) -> OutputStream {
    let total_objects = repo::objects::count_completed(ctx).await.unwrap_or(0);
    let total_size = repo::objects::sum_size_completed(ctx).await.unwrap_or(0.0);
    // Count buckets from the metadata table (single source of truth), the same
    // way the admin SSR overview does, rather than enumerating storage folders.
    let bucket_count = repo::buckets::count_all(ctx).await.unwrap_or(0);

    ok_json(&serde_json::json!({
        "total_objects": total_objects,
        "total_size_bytes": total_size as i64,
        "bucket_count": bucket_count
    }))
}

#[cfg(test)]
mod integration_tests {
    use super::{super::test_helpers::seed_bucket, *};
    use crate::test_support::{admin_msg, output_json, TestContext};

    /// `handle_stats` counts buckets from [`repo::buckets::TABLE`] (the same source
    /// admin SSR overview uses), not by enumerating storage folders.
    #[tokio::test]
    async fn stats_counts_buckets_from_metadata_table() {
        let ctx = TestContext::with_files().await;
        seed_bucket(&ctx, "one", "alice").await;
        seed_bucket(&ctx, "two", "bob").await;

        let out = handle_stats(&ctx, &admin_msg("retrieve", "/admin/storage/stats")).await;
        let body = output_json(out).await;
        assert_eq!(body.get("bucket_count").and_then(|v| v.as_i64()), Some(2));
    }
}
