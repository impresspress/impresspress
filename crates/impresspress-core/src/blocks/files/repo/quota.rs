//! Row-level access over `impresspress__files__cloud_quotas`.
//!
//! Per-user quota override table. Stores explicit byte/file caps that
//! override the block defaults for individual users (one row per user,
//! keyed by `user_id`). The interpretation of a row — field-by-field
//! fallback to `QuotaConfig` defaults — lives in `files::quota`; usage
//! accounting (sums/counts over object rows) lives in
//! [`super::objects`].

use std::collections::HashMap;

use wafer_block::db::{ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, WaferError};

use super::Page;
use crate::{blocks::files::models::QuotaConfig, util::RecordExt};

/// Per-user quota override table.
pub const TABLE: &str = "impresspress__files__cloud_quotas";

/// One quota-override row, decoded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct QuotaRow {
    pub id: String,
    /// The user this override applies to. Unique across the table.
    pub user_id: String,
    /// The effective caps: each column that is present overrides the block
    /// default, field by field.
    ///
    /// Flattened on the wire, because the caps are columns of this
    /// table — `QuotaConfig` groups them for the enforcement path, it does
    /// not nest them in the row.
    #[serde(flatten)]
    pub config: QuotaConfig,
    pub created_at: String,
    pub updated_at: String,
}

impl QuotaRow {
    /// The one decode of a quota row, defaults included.
    ///
    /// Every cap column is `INTEGER` in the schema, but a TEXT-typed backend
    /// hands it back as a string, so the numbers are read with
    /// `opt_i64_field` — which takes both — and only a genuinely missing (or
    /// unparseable) column falls back to the block default. Reading them with
    /// a bare `as_i64()` used to silently replace an admin-lowered cap with
    /// the 1 GiB default.
    ///
    /// The decoded config is then put through
    /// [`quota::clamp_to_transport`](crate::blocks::files::quota::clamp_to_transport),
    /// so a stored per-file cap above the request-body ceiling is reported as
    /// the ceiling by every reader of this row — the upload check, the quota
    /// endpoint, the admin table, and the row an admin update echoes back.
    /// Clamping in one reader and not another is what let an operator be told
    /// 100 MB by the write and 10 MB by the table.
    pub fn from_record(rec: &Record) -> Self {
        let defaults = QuotaConfig::default();
        let config = crate::blocks::files::quota::clamp_to_transport(QuotaConfig {
            max_storage_bytes: rec
                .opt_i64_field("max_storage_bytes")
                .unwrap_or(defaults.max_storage_bytes),
            max_file_size_bytes: rec
                .opt_i64_field("max_file_size_bytes")
                .unwrap_or(defaults.max_file_size_bytes),
            max_files_per_bucket: rec
                .opt_i64_field("max_files_per_bucket")
                .unwrap_or(defaults.max_files_per_bucket),
        });
        Self {
            id: rec.id.clone(),
            user_id: rec.str_field("user_id").to_string(),
            config,
            created_at: rec.str_field("created_at").to_string(),
            updated_at: rec.str_field("updated_at").to_string(),
        }
    }
}

/// Look up `user_id`'s quota-override row. Errors (including NotFound —
/// most users have no override) are surfaced for the caller to map to the
/// block defaults.
pub async fn find_for_user(ctx: &dyn Context, user_id: &str) -> Result<QuotaRow, WaferError> {
    db::get_by_field(
        ctx,
        TABLE,
        "user_id",
        serde_json::Value::String(user_id.to_string()),
    )
    .await
    .map(|r| QuotaRow::from_record(&r))
}

/// Up to `limit` override rows, unsorted (admin JSON listing).
pub async fn list(ctx: &dyn Context, limit: u32) -> Result<Page<QuotaRow>, WaferError> {
    let opts = ListOptions {
        limit: Some(limit),
        ..Default::default()
    };
    Ok(Page::decode(
        db::list(ctx, TABLE, &opts).await?,
        QuotaRow::from_record,
    ))
}

/// Newest override rows first (admin SSR listing).
pub async fn list_recent(ctx: &dyn Context, limit: u32) -> Result<Page<QuotaRow>, WaferError> {
    let opts = ListOptions {
        sort: vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }],
        limit: Some(limit),
        ..Default::default()
    };
    Ok(Page::decode(
        db::list(ctx, TABLE, &opts).await?,
        QuotaRow::from_record,
    ))
}

/// Total number of override rows (admin stats).
pub async fn count_all(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &[]).await
}

/// Create-or-replace `user_id`'s override row with the given (already
/// whitelisted — see SEC-059 in `cloud::handle_update_quota`) quota
/// `fields`. `user_id` and an `updated_at` stamp are written here so every
/// upsert path stays consistent.
pub async fn upsert_for_user(
    ctx: &dyn Context,
    user_id: &str,
    mut fields: HashMap<String, serde_json::Value>,
) -> Result<QuotaRow, WaferError> {
    fields.insert(
        "user_id".to_string(),
        serde_json::Value::String(user_id.to_string()),
    );
    fields.insert(
        "updated_at".to_string(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    db::upsert_by_field(
        ctx,
        TABLE,
        "user_id",
        serde_json::Value::String(user_id.to_string()),
        fields,
    )
    .await
    .map(|r| QuotaRow::from_record(&r))
}

/// Test-fixture seeding: insert a raw row map exactly as given (no stamped
/// columns), so tests control the precise row shape.
#[cfg(test)]
pub async fn seed(
    ctx: &dyn Context,
    data: HashMap<String, serde_json::Value>,
) -> Result<QuotaRow, WaferError> {
    db::create(ctx, TABLE, data)
        .await
        .map(|r| QuotaRow::from_record(&r))
}

/// Test helper: every row of `TABLE`, undecoded — each column exactly as
/// stored, for asserting that something left the table alone.
#[cfg(test)]
pub async fn raw_rows(
    ctx: &dyn Context,
) -> Result<Vec<wafer_core::clients::database::Record>, WaferError> {
    crate::db_read::list_every(ctx, TABLE, vec![]).await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record_with(data: &[(&str, serde_json::Value)]) -> Record {
        Record {
            id: "q1".to_string(),
            data: data
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    /// Regression, moved here with the decode it pins: the SQLite service
    /// returns TEXT-stored columns as JSON strings, and `get_user_quota` used
    /// to read overrides with a bare `as_i64()`, so a TEXT-stored
    /// `max_storage_bytes` override silently fell back to the 1 GiB default
    /// and enforcement ignored the admin-configured cap.
    #[test]
    fn from_record_honors_text_stored_overrides() {
        let row = QuotaRow::from_record(&record_with(&[
            ("user_id", json!("u1")),
            ("max_storage_bytes", json!("2048")),
            ("max_file_size_bytes", json!("1024")),
            ("max_files_per_bucket", json!("5")),
        ]));
        assert_eq!(row.user_id, "u1");
        assert_eq!(
            row.config.max_storage_bytes, 2048,
            "TEXT-stored override must be enforced, not replaced by the default"
        );
        assert_eq!(row.config.max_file_size_bytes, 1024);
        assert_eq!(row.config.max_files_per_bucket, 5);
    }

    #[test]
    fn from_record_accepts_number_typed_overrides() {
        let row = QuotaRow::from_record(&record_with(&[
            ("max_storage_bytes", json!(4096)),
            ("max_file_size_bytes", json!(2048)),
        ]));
        assert_eq!(row.config.max_storage_bytes, 4096);
        assert_eq!(row.config.max_file_size_bytes, 2048);
    }

    /// A missing or unparseable column falls back to the block default — and
    /// the decoded row is the *effective* quota, so the per-file default comes
    /// back clamped to the transport's request-body ceiling. Asserting the raw
    /// `QuotaConfig::default()` here would be asserting a cap no upload can
    /// reach.
    #[test]
    fn from_record_defaults_missing_and_junk_fields() {
        let row = QuotaRow::from_record(&record_with(&[(
            "max_storage_bytes",
            json!("not-a-number"),
        )]));
        assert_eq!(row.config, QuotaConfig::effective_default());
        assert_eq!(
            row.config.max_storage_bytes,
            QuotaConfig::DEFAULT_MAX_STORAGE_BYTES
        );
    }

    /// The clamp is applied where a row is decoded, so the number an admin
    /// update echoes back is the number the upload check will enforce. Before
    /// it moved here, the PATCH response said 100 MB while the table beside it
    /// said 10 MB.
    #[test]
    fn a_stored_cap_above_the_transport_ceiling_decodes_as_the_ceiling() {
        let row = QuotaRow::from_record(&record_with(&[(
            "max_file_size_bytes",
            json!(500 * 1024 * 1024),
        )]));
        assert_eq!(
            row.config.max_file_size_bytes,
            crate::streaming::MAX_REQUEST_BODY_BYTES as i64
        );
    }
}
