use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{contracts::QuotaUsageView, models::QuotaConfig, repo};
use crate::streaming::MAX_REQUEST_BODY_BYTES;

/// The user's effective quota: their override row when one exists, otherwise
/// the block defaults. Only a missing row means "defaults" — any other lookup
/// failure is returned, because treating an outage as "no override" would
/// silently lift an admin-lowered cap.
///
/// Either way the per-file cap is one an upload can reach:
/// [`repo::quota::QuotaRow::from_record`] clamps a stored row, and
/// [`QuotaConfig::effective_default`] is the clamped form of the defaults.
pub async fn get_user_quota(ctx: &dyn Context, user_id: &str) -> Result<QuotaConfig, WaferError> {
    match repo::quota::find_for_user(ctx, user_id).await {
        Ok(row) => Ok(row.config),
        Err(e) if e.code == ErrorCode::NotFound => Ok(QuotaConfig::effective_default()),
        Err(e) => Err(e),
    }
}

/// Lower `max_file_size_bytes` to [`MAX_REQUEST_BODY_BYTES`] when the stored
/// policy asks for more than an upload can carry.
///
/// The stored cap — default 100 MiB, admin-editable per user — is a policy
/// about stored objects; [`MAX_REQUEST_BODY_BYTES`] is the hard ceiling on a
/// request body, enforced by the transport before this block is reached, and
/// no transport streams a request body today. A stored cap above it is
/// unreachable: the upload is refused with a 413 the block never sees, so the
/// number the block reports and the number it enforces would describe
/// different limits. Clamping makes the advertised cap the enforced one, and
/// it happens where a stored row is decoded
/// ([`repo::quota::QuotaRow::from_record`]) so that every reader agrees: the
/// upload's own size check and its error message, the quota endpoint, the
/// admin quotas table, and the row an admin update echoes back.
///
/// Only the per-file cap is clamped. `max_storage_bytes` and the file count
/// are about accumulated objects, which no single request has to carry.
pub fn clamp_to_transport(mut config: QuotaConfig) -> QuotaConfig {
    let transport_ceiling = MAX_REQUEST_BODY_BYTES as i64;
    if config.max_file_size_bytes > transport_ceiling {
        config.max_file_size_bytes = transport_ceiling;
    }
    config
}

/// Total bytes used by `user_id`, computed as `SUM(size)` over the user's
/// object rows ([`repo::objects::sum_size_for_uploader`], no row
/// materialization).
pub async fn get_used_bytes(ctx: &dyn Context, user_id: &str) -> Result<i64, WaferError> {
    Ok(repo::objects::sum_size_for_uploader(ctx, user_id).await? as i64)
}

/// Number of object rows owned by `user_id`, across every bucket — the
/// usage figure the quota endpoint reports. The file-count cap is per bucket,
/// enforced by the upload's reservation ([`repo::objects::reserve_upload`]).
pub async fn get_file_count(ctx: &dyn Context, user_id: &str) -> Result<i64, WaferError> {
    repo::objects::count_for_uploader(ctx, user_id).await
}

/// Usage summary as exposed by the `/b/cloudstorage/quota` JSON endpoint.
pub async fn get_user_usage(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<QuotaUsageView, WaferError> {
    Ok(QuotaUsageView {
        total_bytes: get_used_bytes(ctx, user_id).await?,
        file_count: get_file_count(ctx, user_id).await?,
    })
}

/// How many stale reservations one sweep reclaims at most. The sweep runs
/// on every upload, and each reclaimed row costs a database delete and a
/// storage delete per blob it names; a small batch keeps the cost of one
/// upload bounded (Workers cap subrequests per invocation), and the next
/// upload takes the next batch.
const STALE_SWEEP_BATCH: u32 = 10;

/// Sweep the given user's `pending`-status object rows older than
/// [`repo::objects::PENDING_RESERVATION_TTL_SECONDS`], and the blobs they
/// name. A row is claimed `pending` before the actual storage upload, and
/// counts against its uploader's caps from then on
/// ([`repo::objects::reserve_upload`]); two failures can leave one behind: the upload errored
/// AND `release_reservation` errored too, or the upload succeeded but
/// `mark_complete` could not record it. Either way the row would otherwise
/// inflate that user's quota usage forever. Calling this best-effort on each
/// new upload keeps the table self-healing without a separate cron.
///
/// Until it is swept, such a row also holds the key. `reserve_upload` cannot
/// tell it from an upload still in flight, so an upload of that key is
/// refused until the row passes the TTL — for the row's own uploader with
/// `ReserveError::HeldByOwnEarlierUpload`, which names when the reservation
/// began, rather than as someone else's upload. The uploader's next upload
/// after that sweeps the row first and claims the key afresh.
///
/// Each row is deleted only while it is as it was listed
/// ([`repo::objects::delete_if_unchanged`]), and only then are its blobs
/// deleted: the ones it names are named by no other row
/// ([`repo::objects::StoredRow::blobs`]), so an upload that took the key
/// over meanwhile keeps its own. A blob whose delete fails is logged and left.
pub async fn sweep_stale_pending(ctx: &dyn Context, user_id: &str) {
    let cutoff = repo::objects::pending_reservation_cutoff();
    let stale = match repo::objects::list_stale_pending(ctx, user_id, &cutoff, STALE_SWEEP_BATCH)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, user_id = %user_id, "failed to list stale pending uploads");
            return;
        }
    };
    for row in stale {
        match repo::objects::delete_if_unchanged(ctx, &row).await {
            Ok(true) => {
                super::storage::delete_blobs(ctx, &row.row.bucket, &row.blobs()).await;
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, user_id = %user_id, "failed to sweep a stale pending upload");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::test_support::TestContext;

    #[tokio::test]
    async fn get_user_quota_returns_defaults_without_override_row() {
        let ctx = TestContext::with_files().await;
        let quota = get_user_quota(&ctx, "nobody")
            .await
            .expect("no override row means the defaults, not an error");
        assert_eq!(
            quota.max_storage_bytes,
            QuotaConfig::DEFAULT_MAX_STORAGE_BYTES
        );
    }

    #[tokio::test]
    async fn get_user_quota_applies_override_row() {
        let ctx = TestContext::with_files().await;
        let mut row: HashMap<String, serde_json::Value> = HashMap::new();
        row.insert("user_id".into(), json!("u1"));
        row.insert("max_storage_bytes".into(), json!(2048));
        repo::quota::seed(&ctx, row).await.expect("seed quota");

        let quota = get_user_quota(&ctx, "u1").await.expect("quota lookup");
        assert_eq!(quota.max_storage_bytes, 2048);
        // Fields without an explicit override keep the defaults. (The
        // migration declares DB-side column defaults, so a full row insert
        // materializes them; either way the value matches the const.) The
        // per-file default is above the transport's request-body ceiling, so
        // what comes back is the ceiling — see
        // `the_per_file_cap_is_clamped_to_what_a_request_body_can_carry`.
        assert_eq!(
            quota.max_file_size_bytes, MAX_REQUEST_BODY_BYTES as i64,
            "the default 100 MiB is clamped to the transport ceiling"
        );
    }

    /// **Fails on the pre-fix tree.** The block advertised a 100 MiB per-file
    /// cap that no request body could reach: every transport buffers the body
    /// under `streaming::MAX_REQUEST_BODY_BYTES` and refuses anything larger
    /// before this block runs. Reading the quota now yields the enforced
    /// number, so the upload check, its error message and the admin table all
    /// describe the same limit.
    #[tokio::test]
    async fn the_per_file_cap_is_clamped_to_what_a_request_body_can_carry() {
        let ctx = TestContext::with_files().await;
        let mut row: HashMap<String, serde_json::Value> = HashMap::new();
        row.insert("user_id".into(), json!("u1"));
        // An admin raising the cap cannot raise the transport's.
        row.insert("max_file_size_bytes".into(), json!(500 * 1024 * 1024));
        repo::quota::seed(&ctx, row).await.expect("seed quota");

        let quota = get_user_quota(&ctx, "u1").await.expect("quota lookup");
        assert_eq!(quota.max_file_size_bytes, MAX_REQUEST_BODY_BYTES as i64);
    }

    /// A cap BELOW the ceiling is policy and is left alone — clamping is a
    /// ceiling, not a floor, and an admin-lowered limit still lowers.
    #[tokio::test]
    async fn a_cap_below_the_transport_ceiling_is_untouched() {
        let ctx = TestContext::with_files().await;
        let mut row: HashMap<String, serde_json::Value> = HashMap::new();
        row.insert("user_id".into(), json!("u1"));
        row.insert("max_file_size_bytes".into(), json!(4096));
        repo::quota::seed(&ctx, row).await.expect("seed quota");

        let quota = get_user_quota(&ctx, "u1").await.expect("quota lookup");
        assert_eq!(quota.max_file_size_bytes, 4096);
        // And the other caps are about accumulated storage, not one request,
        // so the transport ceiling has nothing to say about them.
        assert_eq!(
            quota.max_storage_bytes,
            QuotaConfig::DEFAULT_MAX_STORAGE_BYTES
        );
    }

    #[tokio::test]
    async fn get_used_bytes_sums_object_sizes_per_user() {
        let ctx = TestContext::with_files().await;
        for (key, size, owner) in [("a", 1024, "u1"), ("b", 1024, "u1"), ("c", 4096, "u2")] {
            let mut row: HashMap<String, serde_json::Value> = HashMap::new();
            row.insert("bucket".into(), json!("photos"));
            row.insert("key".into(), json!(key));
            row.insert("size".into(), json!(size));
            row.insert("uploaded_by".into(), json!(owner));
            repo::objects::seed(&ctx, row).await.expect("seed");
        }

        assert_eq!(get_used_bytes(&ctx, "u1").await.expect("usage"), 2048);
        assert_eq!(get_used_bytes(&ctx, "u2").await.expect("usage"), 4096);
        assert_eq!(get_used_bytes(&ctx, "u3").await.expect("usage"), 0);
        assert_eq!(get_file_count(&ctx, "u1").await.expect("count"), 2);
    }
}
