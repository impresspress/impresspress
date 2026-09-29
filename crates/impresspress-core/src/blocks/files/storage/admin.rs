//! The admin stats endpoint, `GET /b/storage/admin/api/stats`: declared
//! `Admin` in the block's route table and gated by the router from that
//! declaration. Until this PR it was reached through the admin block's
//! `call_block` delegation on a synthetic path that never existed on the wire.

use wafer_run::{context::Context, Message, OutputStream};

use crate::{
    blocks::{
        crud::db_error_internal,
        files::{contracts, repo},
    },
    http::ok_json,
};

/// Three aggregates over tables this endpoint names itself, so a `NotFound`
/// from any of them is a 500, never the caller's row —
/// which is what makes [`db_error_internal`] the right door. Each one used to
/// fall back to zero, and this is a JSON body an operator's dashboard polls:
/// `{"total_objects":0,"total_size_bytes":0,"bucket_count":0}` during an
/// outage is indistinguishable from a deployment nobody has used yet.
pub(in crate::blocks::files) async fn handle_stats(
    ctx: &dyn Context,
    _msg: &Message,
) -> OutputStream {
    let total_objects = match repo::objects::count_completed(ctx).await {
        Ok(count) => count,
        Err(e) => return db_error_internal(e, "Storage stats: object count"),
    };
    let total_size = match repo::objects::sum_size_completed(ctx).await {
        Ok(size) => size,
        Err(e) => return db_error_internal(e, "Storage stats: total size"),
    };
    // Count buckets from the metadata table (single source of truth), the same
    // way the admin SSR overview does, rather than enumerating storage folders.
    let bucket_count = match repo::buckets::count_all(ctx).await {
        Ok(count) => count,
        Err(e) => return db_error_internal(e, "Storage stats: bucket count"),
    };

    ok_json(&contracts::StorageStatsResponse {
        total_objects,
        total_size_bytes: total_size as i64,
        bucket_count,
    })
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

        let out = handle_stats(&ctx, &admin_msg("retrieve", "/b/storage/admin/api/stats")).await;
        let body = output_json(out).await;
        assert_eq!(body.get("bucket_count").and_then(|v| v.as_i64()), Some(2));
    }
}

#[cfg(test)]
mod outage_tests {
    //! `GET /b/storage/admin/api/stats` is a JSON API, so a failed count
    //! propagates through the one database-error door rather than rendering
    //! anything: an operator polling it during an outage used to be told the
    //! deployment held zero objects, zero bytes and zero buckets.

    use super::*;
    use crate::test_support::{admin_msg, output_http_status, TestContext};

    #[tokio::test]
    async fn a_failing_count_is_an_error_not_a_zeroed_stats_body() {
        let ctx = TestContext::with_files().await.break_reads();
        let out = handle_stats(&ctx, &admin_msg("retrieve", "/b/storage/admin/api/stats")).await;
        assert_eq!(output_http_status(out).await, 500);
    }
}
