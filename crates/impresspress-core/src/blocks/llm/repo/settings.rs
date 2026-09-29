//! Per-thread provider/model overrides (`impresspress__llm__settings`).
//!
//! One row per messages-block context that has pinned a provider or a model.
//! Every query against the table is in this file, and every read hands back a
//! [`ThreadSettingRow`] rather than a `db::Record`, so the four column names
//! are spelled once.

use std::collections::HashMap;

use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    db_read::{self, CappedList},
    util::{stamp_created, stamp_updated, RecordExt},
};

pub(crate) const TABLE: &str = "impresspress__llm__settings";

/// One stored override, column for column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThreadSettingRow {
    /// Stable row identifier.
    pub id: String,
    /// The messages-block context id this override applies to.
    pub thread_id: String,
    /// Pinned provider name. Empty means "use the default provider".
    pub provider_block: String,
    /// Pinned model id. Empty means "use the default model".
    pub model: String,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    pub updated_at: String,
}

impl ThreadSettingRow {
    fn from_record(record: &Record) -> Self {
        Self {
            id: record.id.clone(),
            thread_id: record.str_field("thread_id").to_string(),
            provider_block: record.str_field("provider_block").to_string(),
            model: record.str_field("model").to_string(),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        }
    }
}

/// The override for `thread_id`, if the thread has one.
///
/// `Ok(None)` means "no override"; an `Err` means the question could not be
/// answered. The two used to be the same value: this read ended in `.ok()`,
/// so a database outage looked exactly like an absent row — which made
/// `resolve_provider` fall back to the global default model, and made
/// `handle_post_config` write a second override for a thread that already had
/// one.
pub(crate) async fn find_for_thread(
    ctx: &dyn Context,
    thread_id: &str,
) -> Result<Option<ThreadSettingRow>, WaferError> {
    match db::get_by_field(
        ctx,
        TABLE,
        "thread_id",
        serde_json::Value::String(thread_id.to_string()),
    )
    .await
    {
        Ok(record) => Ok(Some(ThreadSettingRow::from_record(&record))),
        Err(error) if error.code == ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Overrides for the settings page's table, and whether there are more than
/// the table lists.
///
/// One row per thread that pinned a provider or model, and threads are
/// user-created, so the set grows with use.
pub(crate) async fn list_all(
    ctx: &dyn Context,
) -> Result<CappedList<ThreadSettingRow>, WaferError> {
    Ok(db_read::list_capped(ctx, TABLE, vec![])
        .await?
        .map(|record| ThreadSettingRow::from_record(&record)))
}

/// Create the override for `thread_id`. Either field may be empty, which
/// means "fall through to the global default" for that field.
pub(crate) async fn insert(
    ctx: &dyn Context,
    thread_id: &str,
    provider_block: &str,
    model: &str,
) -> Result<ThreadSettingRow, WaferError> {
    let mut data = HashMap::from([
        (
            "thread_id".to_string(),
            serde_json::Value::String(thread_id.to_string()),
        ),
        (
            "provider_block".to_string(),
            serde_json::Value::String(provider_block.to_string()),
        ),
        (
            "model".to_string(),
            serde_json::Value::String(model.to_string()),
        ),
    ]);
    stamp_created(&mut data);
    db::create(ctx, TABLE, data)
        .await
        .map(|record| ThreadSettingRow::from_record(&record))
}

/// Update an existing override in place. `None` for a field leaves it as it
/// is — the wire contract for `POST /b/llm/api/config` is that a field absent
/// from the body is retained.
pub(crate) async fn update(
    ctx: &dyn Context,
    row: &ThreadSettingRow,
    provider_block: Option<&str>,
    model: Option<&str>,
) -> Result<ThreadSettingRow, WaferError> {
    let mut data = HashMap::from([
        (
            "thread_id".to_string(),
            serde_json::Value::String(row.thread_id.clone()),
        ),
        (
            "provider_block".to_string(),
            serde_json::Value::String(
                provider_block
                    .unwrap_or(row.provider_block.as_str())
                    .to_string(),
            ),
        ),
        (
            "model".to_string(),
            serde_json::Value::String(model.unwrap_or(row.model.as_str()).to_string()),
        ),
        (
            "created_at".to_string(),
            serde_json::Value::String(row.created_at.clone()),
        ),
    ]);
    stamp_updated(&mut data);
    db::update(ctx, TABLE, &row.id, data)
        .await
        .map(|record| ThreadSettingRow::from_record(&record))
}

/// Remove one override by row id. `Err(NotFound)` when there is no such row,
/// which the settings page's delete control answers as a 404.
pub(crate) async fn delete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, TABLE, id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FailingDbOpContext, TestContext};

    /// Every column round-trips through the row type, and the two "no
    /// override for this field" values stay empty rather than becoming
    /// `null`.
    #[tokio::test]
    async fn a_thread_setting_row_round_trips_every_column() {
        let ctx = TestContext::with_llm().await;

        let written = insert(&ctx, "t1", "openai-main", "gpt-4o")
            .await
            .expect("insert");
        assert_eq!(written.thread_id, "t1");
        assert_eq!(written.provider_block, "openai-main");
        assert_eq!(written.model, "gpt-4o");
        assert!(!written.id.is_empty());
        assert!(!written.created_at.is_empty());
        assert!(!written.updated_at.is_empty());

        assert_eq!(
            find_for_thread(&ctx, "t1").await.expect("read"),
            Some(written.clone())
        );
        assert_eq!(
            list_all(&ctx).await.expect("list").rows,
            vec![written.clone()]
        );

        let empty = insert(&ctx, "t2", "", "").await.expect("insert");
        assert_eq!(empty.provider_block, "");
        assert_eq!(empty.model, "");
    }

    /// `update` keeps the fields the caller did not name, and only those.
    #[tokio::test]
    async fn update_retains_the_fields_the_caller_did_not_name() {
        let ctx = TestContext::with_llm().await;
        let row = insert(&ctx, "t1", "openai-main", "gpt-4o")
            .await
            .expect("insert");

        let updated = update(&ctx, &row, None, Some("gpt-4o-mini"))
            .await
            .expect("update");
        assert_eq!(updated.id, row.id, "the same row is written");
        assert_eq!(updated.provider_block, "openai-main");
        assert_eq!(updated.model, "gpt-4o-mini");
        assert_eq!(updated.created_at, row.created_at);

        assert_eq!(list_all(&ctx).await.expect("list").rows.len(), 1);
    }

    /// An absent row is `Ok(None)`; a failed read is `Err`. These were the
    /// same value while the lookup ended in `.ok()`.
    #[tokio::test]
    async fn an_absent_override_is_none_and_a_failed_read_is_an_error() {
        let ctx = TestContext::with_llm().await;
        assert_eq!(
            find_for_thread(&ctx, "t_missing").await.expect("read"),
            None
        );

        let failing = FailingDbOpContext::new(ctx, vec![("database.list", TABLE)]);
        assert_eq!(
            find_for_thread(&failing, "t_missing")
                .await
                .expect_err("the outage surfaces")
                .code,
            ErrorCode::Internal
        );
    }

    #[tokio::test]
    async fn delete_removes_the_row_and_reports_a_missing_one() {
        let ctx = TestContext::with_llm().await;
        let row = insert(&ctx, "t1", "", "gpt-4o").await.expect("insert");

        delete(&ctx, &row.id).await.expect("delete");
        assert!(list_all(&ctx).await.expect("list").rows.is_empty());

        assert_eq!(
            delete(&ctx, &row.id)
                .await
                .expect_err("a second delete finds nothing")
                .code,
            ErrorCode::NotFound
        );
    }
}
