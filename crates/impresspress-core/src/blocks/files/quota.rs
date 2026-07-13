use wafer_core::clients::database::Record;
use wafer_run::{context::Context, OutputStream};

use super::{models::QuotaConfig, repo};
use crate::{http::err_bad_request, util::RecordExt};

/// Map a quota-override row onto a `QuotaConfig`, falling back to the
/// block defaults field-by-field. Numeric fields accept both JSON numbers
/// and TEXT-stored numeric strings (see `RecordExt::opt_i64_field`) so a
/// TEXT-stored override is honored rather than silently replaced by the
/// default.
fn quota_from_record(record: &Record) -> QuotaConfig {
    let defaults = QuotaConfig::default();
    QuotaConfig {
        max_storage_bytes: record
            .opt_i64_field("max_storage_bytes")
            .unwrap_or(defaults.max_storage_bytes),
        max_file_size_bytes: record
            .opt_i64_field("max_file_size_bytes")
            .unwrap_or(defaults.max_file_size_bytes),
        max_files_per_bucket: record
            .opt_i64_field("max_files_per_bucket")
            .unwrap_or(defaults.max_files_per_bucket),
        reset_period_days: record
            .opt_i64_field("reset_period_days")
            .unwrap_or(defaults.reset_period_days),
    }
}

pub async fn get_user_quota(ctx: &dyn Context, user_id: &str) -> QuotaConfig {
    // Check for user-specific override
    match repo::quota::find_for_user(ctx, user_id).await {
        Ok(record) => quota_from_record(&record),
        Err(_) => QuotaConfig::default(),
    }
}

/// Total bytes used by `user_id`, computed as `SUM(size)` over the user's
/// object rows ([`repo::objects::sum_size_for_uploader`], no row
/// materialization).
pub async fn get_used_bytes(ctx: &dyn Context, user_id: &str) -> i64 {
    repo::objects::sum_size_for_uploader(ctx, user_id)
        .await
        .unwrap_or(0.0) as i64
}

/// Number of object rows owned by `user_id`.
pub async fn get_file_count(ctx: &dyn Context, user_id: &str) -> i64 {
    repo::objects::count_for_uploader(ctx, user_id)
        .await
        .unwrap_or(0)
}

/// Usage summary as exposed by the `/b/cloudstorage/quota` JSON endpoint.
pub async fn get_user_usage(ctx: &dyn Context, user_id: &str) -> serde_json::Value {
    serde_json::json!({
        "total_bytes": get_used_bytes(ctx, user_id).await,
        "file_count": get_file_count(ctx, user_id).await,
    })
}

pub async fn check_quota(
    ctx: &dyn Context,
    user_id: &str,
    file_size: i64,
) -> Result<(), OutputStream> {
    let quota = get_user_quota(ctx, user_id).await;

    if file_size > quota.max_file_size_bytes {
        return Err(err_bad_request(&format!(
            "File exceeds maximum size of {} bytes",
            quota.max_file_size_bytes
        )));
    }

    let current_bytes = get_used_bytes(ctx, user_id).await;
    if current_bytes + file_size > quota.max_storage_bytes {
        return Err(err_bad_request("Storage quota exceeded"));
    }

    if quota.max_files_per_bucket > 0
        && get_file_count(ctx, user_id).await >= quota.max_files_per_bucket
    {
        return Err(err_bad_request(&format!(
            "File count limit reached (max {})",
            quota.max_files_per_bucket
        )));
    }

    Ok(())
}

/// Sweep `pending`-status object rows older than `older_than_seconds` for
/// the given user. Pending rows are inserted before the actual storage
/// upload to close the quota TOCTOU window; if the upload errors AND the
/// compensating delete also errors, the row sticks around and inflates the
/// user's quota usage forever. Calling this best-effort on each new upload
/// keeps the table self-healing without a separate cron.
///
/// 1 hour is a comfortable cutoff: the largest realistic upload finishes
/// inside that window, and anything still pending afterward is almost
/// certainly an orphan.
pub async fn sweep_stale_pending(ctx: &dyn Context, user_id: &str, older_than_seconds: i64) {
    let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(older_than_seconds)).to_rfc3339();
    if let Err(e) = repo::objects::delete_stale_pending(ctx, user_id, &cutoff).await {
        tracing::warn!(error = %e, user_id = %user_id, "failed to sweep stale pending uploads");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;
    use crate::test_support::TestContext;

    fn record_with(data: &[(&str, serde_json::Value)]) -> Record {
        Record {
            id: "1".to_string(),
            data: data
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    /// Regression: the SQLite service returns TEXT-stored columns as JSON
    /// strings. `get_user_quota` used to read overrides with a bare
    /// `as_i64()`, so a TEXT-stored `max_storage_bytes` override silently
    /// fell back to the 1 GiB default and enforcement ignored the
    /// admin-configured cap.
    #[test]
    fn quota_from_record_honors_text_stored_overrides() {
        let record = record_with(&[
            ("max_storage_bytes", json!("2048")),
            ("max_file_size_bytes", json!("1024")),
            ("max_files_per_bucket", json!("5")),
            ("reset_period_days", json!("7")),
        ]);
        let quota = quota_from_record(&record);
        assert_eq!(
            quota.max_storage_bytes, 2048,
            "TEXT-stored override must be enforced, not replaced by the default"
        );
        assert_eq!(quota.max_file_size_bytes, 1024);
        assert_eq!(quota.max_files_per_bucket, 5);
        assert_eq!(quota.reset_period_days, 7);
    }

    #[test]
    fn quota_from_record_accepts_number_typed_overrides() {
        let record = record_with(&[
            ("max_storage_bytes", json!(4096)),
            ("max_file_size_bytes", json!(2048)),
        ]);
        let quota = quota_from_record(&record);
        assert_eq!(quota.max_storage_bytes, 4096);
        assert_eq!(quota.max_file_size_bytes, 2048);
    }

    #[test]
    fn quota_from_record_defaults_missing_and_junk_fields() {
        let record = record_with(&[("max_storage_bytes", json!("not-a-number"))]);
        let quota = quota_from_record(&record);
        assert_eq!(
            quota.max_storage_bytes,
            QuotaConfig::DEFAULT_MAX_STORAGE_BYTES
        );
        assert_eq!(
            quota.max_file_size_bytes,
            QuotaConfig::DEFAULT_MAX_FILE_SIZE_BYTES
        );
        assert_eq!(
            quota.max_files_per_bucket,
            QuotaConfig::DEFAULT_MAX_FILES_PER_BUCKET
        );
        assert_eq!(
            quota.reset_period_days,
            QuotaConfig::DEFAULT_RESET_PERIOD_DAYS
        );
    }

    #[tokio::test]
    async fn get_user_quota_returns_defaults_without_override_row() {
        let ctx = TestContext::with_files().await;
        let quota = get_user_quota(&ctx, "nobody").await;
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

        let quota = get_user_quota(&ctx, "u1").await;
        assert_eq!(quota.max_storage_bytes, 2048);
        // Fields without an explicit override keep the defaults. (The
        // migration declares DB-side column defaults, so a full row insert
        // materializes them; either way the value matches the const.)
        assert_eq!(
            quota.max_file_size_bytes,
            QuotaConfig::DEFAULT_MAX_FILE_SIZE_BYTES
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

        assert_eq!(get_used_bytes(&ctx, "u1").await, 2048);
        assert_eq!(get_used_bytes(&ctx, "u2").await, 4096);
        assert_eq!(get_used_bytes(&ctx, "u3").await, 0);
        assert_eq!(get_file_count(&ctx, "u1").await, 2);
    }

    /// End-to-end: an override row caps enforcement, so a file that fits
    /// the default 1 GiB quota but not the override is rejected.
    #[tokio::test]
    async fn check_quota_enforces_override_storage_cap() {
        let ctx = TestContext::with_files().await;
        let mut row: HashMap<String, serde_json::Value> = HashMap::new();
        row.insert("user_id".into(), json!("u1"));
        row.insert("max_storage_bytes".into(), json!(2048));
        repo::quota::seed(&ctx, row).await.expect("seed quota");

        assert!(check_quota(&ctx, "u1", 1024).await.is_ok());
        assert!(
            check_quota(&ctx, "u1", 4096).await.is_err(),
            "file above the override cap must be rejected"
        );
    }
}
