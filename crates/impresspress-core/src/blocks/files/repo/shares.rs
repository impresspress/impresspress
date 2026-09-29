//! Row-level access over `impresspress__files__cloud_shares` and its child
//! audit table `impresspress__files__cloud_access_logs`.
//!
//! A share row is one generated public link (token, source object,
//! optional expiry / access cap, running `access_count`). Every recorded
//! access appends an access-log row ([`log_access`]). Both tables are
//! owned here because the log rows are meaningless without their share.

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, WaferError};

use super::Page;
use crate::{
    db_read::{self, CappedList},
    util::RecordExt,
};

/// Public share-link table — one row per generated token.
pub const TABLE: &str = "impresspress__files__cloud_shares";

/// Access log table — one row per recorded share access (audit trail).
pub const ACCESS_LOGS_TABLE: &str = "impresspress__files__cloud_access_logs";

/// One share row, decoded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct ShareRow {
    pub id: String,
    /// The opaque token embedded in the public `/b/storage/direct/{token}`
    /// URL: random bytes, hex-encoded, asserting nothing about the share.
    /// This row is what it addresses, and what decides whether the link
    /// still works. Unique across the table.
    pub token: String,
    pub bucket: String,
    pub key: String,
    /// User id of the share's creator — the ownership key
    /// `handle_delete_share` checks.
    pub created_by: String,
    /// RFC 3339 creation instant.
    pub created_at: String,
    /// The end this share link records, as an RFC 3339 stamp.
    ///
    /// `None` is a SQL `NULL` or a stored empty string — one meaning, since
    /// the column is nullable and every caller treated `""` as unset. It is
    /// NOT "never expires": every share link has an end, and a row that
    /// records none cannot be shown to be live, so the public link refuses
    /// it. Creating a share cannot produce one — `NewShare` takes a
    /// non-optional expiry — and migration 003 gave every historical row an
    /// end, so a `None` here is a row that reached the table some other
    /// way.
    pub expires_at: Option<String>,
    pub access_count: i64,
    /// Access cap, or `None` for unlimited. A non-positive stored value is
    /// `None` too, which is the meaning [`NewShare::max_access_count`]
    /// documents and the meaning
    /// [`increment_access_count_capped`] enforces.
    pub max_access_count: Option<i64>,
    pub updated_at: String,
}

impl ShareRow {
    /// The one decode of a share row.
    pub fn from_record(rec: &Record) -> Self {
        Self {
            id: rec.id.clone(),
            token: rec.str_field("token").to_string(),
            bucket: rec.str_field("bucket").to_string(),
            key: rec.str_field("key").to_string(),
            created_by: rec.str_field("created_by").to_string(),
            created_at: rec.str_field("created_at").to_string(),
            expires_at: rec.opt_str_field("expires_at").filter(|s| !s.is_empty()),
            access_count: rec.i64_field("access_count"),
            max_access_count: rec.opt_i64_field("max_access_count").filter(|n| *n > 0),
            updated_at: rec.str_field("updated_at").to_string(),
        }
    }
}

/// One access-log row, decoded. The child audit table of a share: a log row
/// is meaningless without the share it points at, which is why both tables
/// live behind this one module.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct AccessLogRow {
    pub id: String,
    pub share_id: String,
    /// RFC 3339 instant of the recorded access.
    pub accessed_at: String,
    pub ip_address: String,
    pub user_agent: String,
    pub created_at: String,
    pub updated_at: String,
}

impl AccessLogRow {
    /// The one decode of an access-log row.
    pub fn from_record(rec: &Record) -> Self {
        Self {
            id: rec.id.clone(),
            share_id: rec.str_field("share_id").to_string(),
            accessed_at: rec.str_field("accessed_at").to_string(),
            ip_address: rec.str_field("ip_address").to_string(),
            user_agent: rec.str_field("user_agent").to_string(),
            created_at: rec.str_field("created_at").to_string(),
            updated_at: rec.str_field("updated_at").to_string(),
        }
    }
}

/// Insert payload for [`insert`]. Borrowed fields — the caller keeps
/// ownership. `created_at` is caller-supplied (not stamped here) because
/// the share's `expires_at` is derived from the same instant.
#[derive(Debug, Clone, Copy)]
pub struct NewShare<'a> {
    pub token: &'a str,
    pub bucket: &'a str,
    pub key: &'a str,
    pub created_by: &'a str,
    /// RFC 3339 creation instant (also the base of `expires_at`).
    pub created_at: &'a str,
    /// Absolute expiry (RFC 3339). Not optional: every share link has an
    /// end, and the row is the only thing that records it — the token says
    /// nothing. A share with no expiry would be a permanently live public
    /// link, so the type does not let one be inserted.
    pub expires_at: &'a str,
    /// Optional access cap; `None` (or a non-positive stored value) means
    /// unlimited.
    pub max_access_count: Option<i64>,
}

/// Insert a share row (`access_count` starts at 0) and return it.
pub async fn insert(ctx: &dyn Context, new: NewShare<'_>) -> Result<ShareRow, WaferError> {
    let mut data = crate::util::json_map(serde_json::json!({
        "token": new.token,
        "bucket": new.bucket,
        "key": new.key,
        "created_by": new.created_by,
        "created_at": new.created_at,
        "access_count": 0,
    }));
    data.insert(
        "expires_at".to_string(),
        serde_json::Value::String(new.expires_at.to_string()),
    );
    if let Some(max) = new.max_access_count {
        data.insert("max_access_count".to_string(), serde_json::json!(max));
    }
    db::create(ctx, TABLE, data)
        .await
        .map(|r| ShareRow::from_record(&r))
}

/// Look up a share by its raw token (the value embedded in the public
/// `/b/storage/direct/{token}` URL).
pub async fn find_by_token(ctx: &dyn Context, token: &str) -> Result<ShareRow, WaferError> {
    db::get_by_field(
        ctx,
        TABLE,
        "token",
        serde_json::Value::String(token.to_string()),
    )
    .await
    .map(|r| ShareRow::from_record(&r))
}

/// Look up a share by its primary `id`.
pub async fn find_by_id(ctx: &dyn Context, id: &str) -> Result<ShareRow, WaferError> {
    db::get(ctx, TABLE, id)
        .await
        .map(|r| ShareRow::from_record(&r))
}

/// Hard-delete a share row by id.
pub async fn delete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, TABLE, id).await
}

/// Up to `limit` of `user_id`'s shares, newest first (JSON API listing).
pub async fn list_for_user(
    ctx: &dyn Context,
    user_id: &str,
    limit: u32,
) -> Result<Page<ShareRow>, WaferError> {
    let opts = ListOptions {
        filters: vec![Filter {
            field: "created_by".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id.to_string()),
        }],
        sort: vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }],
        limit: Some(limit),
        ..Default::default()
    };
    Ok(Page::decode(
        db::list(ctx, TABLE, &opts).await?,
        ShareRow::from_record,
    ))
}

/// `user_id`'s shares, newest first, for the SSR shares page — and whether
/// there are more than the page lists.
///
/// A user can share every object they own, so the set grows with their
/// storage; the page has to be able to say it is showing a prefix.
pub async fn list_all_for_user(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<CappedList<ShareRow>, WaferError> {
    let records = db_read::list_capped_sorted(
        ctx,
        TABLE,
        vec![Filter {
            field: "created_by".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id.to_string()),
        }],
        vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }],
    )
    .await?;
    Ok(records.map(|record| ShareRow::from_record(&record)))
}

/// Newest shares across ALL users (admin listing).
pub async fn list_recent(
    ctx: &dyn Context,
    limit: u32,
    offset: i64,
) -> Result<Page<ShareRow>, WaferError> {
    let opts = ListOptions {
        sort: vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }],
        limit: Some(limit),
        offset,
        ..Default::default()
    };
    Ok(Page::decode(
        db::list(ctx, TABLE, &opts).await?,
        ShareRow::from_record,
    ))
}

/// Total number of share rows (admin stats).
pub async fn count_all(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &[]).await
}

/// CAS-style increment of `access_count` for a share row. Returns `Ok(true)`
/// if a row was updated (and the cap, if any, still allowed the access),
/// `Ok(false)` if the row was already at its cap, or `Err` on DB failure.
///
/// `max <= 0` means unlimited — we only filter on id. Otherwise we add
/// `access_count < max` to the WHERE so two concurrent accesses can't both
/// pass a 1-access cap:
///   UPDATE shares SET access_count = access_count + 1
///   WHERE id = ? AND access_count < max
/// With the cap inside the WHERE clause, at most one updater wins per row
/// and rowcount 0 ⇒ cap reached.
pub async fn increment_access_count_capped(
    ctx: &dyn Context,
    share_id: &str,
    max: i64,
) -> Result<bool, WaferError> {
    let mut filters: Vec<Filter> = vec![Filter {
        field: "id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(share_id.to_string()),
    }];
    if max > 0 {
        filters.push(Filter {
            field: "access_count".to_string(),
            operator: FilterOp::LessThan,
            value: serde_json::json!(max),
        });
    }
    let rows = db::increment_field_where(ctx, TABLE, "access_count", 1, &filters).await?;
    Ok(rows > 0)
}

/// Append an access-log row for `share_id` (`accessed_at` stamped with
/// [`crate::util::now_rfc3339`]).
pub async fn log_access(
    ctx: &dyn Context,
    share_id: &str,
    ip_address: &str,
    user_agent: &str,
) -> Result<AccessLogRow, WaferError> {
    let data = crate::util::json_map(serde_json::json!({
        "share_id": share_id,
        "accessed_at": crate::util::now_rfc3339(),
        "ip_address": ip_address,
        "user_agent": user_agent,
    }));
    db::create(ctx, ACCESS_LOGS_TABLE, data)
        .await
        .map(|r| AccessLogRow::from_record(&r))
}

/// Access-log rows, newest first, optionally restricted to one share
/// (admin audit listing).
pub async fn list_access_logs(
    ctx: &dyn Context,
    share_id: Option<&str>,
    limit: u32,
    offset: i64,
) -> Result<Page<AccessLogRow>, WaferError> {
    let mut filters = Vec::new();
    if let Some(share_id) = share_id {
        filters.push(Filter {
            field: "share_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(share_id.to_string()),
        });
    }
    let opts = ListOptions {
        filters,
        sort: vec![SortField {
            field: "accessed_at".to_string(),
            desc: true,
        }],
        limit: Some(limit),
        offset,
        skip_count: false,
        ..Default::default()
    };
    Ok(Page::decode(
        db::list(ctx, ACCESS_LOGS_TABLE, &opts).await?,
        AccessLogRow::from_record,
    ))
}

/// Test-fixture seeding: insert a raw share row map exactly as given (no
/// stamped columns), so tests control the precise row shape.
#[cfg(test)]
pub async fn seed(
    ctx: &dyn Context,
    data: std::collections::HashMap<String, serde_json::Value>,
) -> Result<ShareRow, WaferError> {
    db::create(ctx, TABLE, data)
        .await
        .map(|r| ShareRow::from_record(&r))
}

/// Test helper: every row of `TABLE`, undecoded — each column exactly as
/// stored, for asserting that something left the table alone.
#[cfg(test)]
pub async fn raw_rows(
    ctx: &dyn Context,
) -> Result<Vec<wafer_core::clients::database::Record>, WaferError> {
    crate::db_read::list_every(ctx, TABLE, vec![]).await
}

/// Test helper: every row of `ACCESS_LOGS_TABLE`, undecoded — each column exactly as
/// stored, for asserting that something left the table alone.
#[cfg(test)]
pub async fn raw_access_log_rows(
    ctx: &dyn Context,
) -> Result<Vec<wafer_core::clients::database::Record>, WaferError> {
    crate::db_read::list_every(ctx, ACCESS_LOGS_TABLE, vec![]).await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn record(data: &[(&str, serde_json::Value)]) -> Record {
        Record {
            id: "s1".to_string(),
            data: data
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    #[test]
    fn from_record_decodes_the_whole_row() {
        let row = ShareRow::from_record(&record(&[
            ("token", json!("tok12345abcdef")),
            ("bucket", json!("photos")),
            ("key", json!("a.png")),
            ("created_by", json!("alice-1234")),
            ("created_at", json!("2026-05-06T10:00:00Z")),
            ("expires_at", json!("2026-06-06T10:00:00Z")),
            ("access_count", json!(4)),
            ("max_access_count", json!(10)),
            ("updated_at", json!("2026-05-06T10:00:01Z")),
        ]));
        assert_eq!(
            row,
            ShareRow {
                id: "s1".to_string(),
                token: "tok12345abcdef".to_string(),
                bucket: "photos".to_string(),
                key: "a.png".to_string(),
                created_by: "alice-1234".to_string(),
                created_at: "2026-05-06T10:00:00Z".to_string(),
                expires_at: Some("2026-06-06T10:00:00Z".to_string()),
                access_count: 4,
                max_access_count: Some(10),
                updated_at: "2026-05-06T10:00:01Z".to_string(),
            }
        );
    }

    /// An absent key, a SQL `NULL` and a stored empty string are one state
    /// — "this row records no end" — so the decode makes all three `None`
    /// and the serving path refuses that state once, in one place.
    #[test]
    fn expires_at_is_none_for_every_shape_of_unset() {
        assert_eq!(ShareRow::from_record(&record(&[])).expires_at, None);
        assert_eq!(
            ShareRow::from_record(&record(&[("expires_at", json!(null))])).expires_at,
            None
        );
        assert_eq!(
            ShareRow::from_record(&record(&[("expires_at", json!(""))])).expires_at,
            None
        );
    }

    /// "Unlimited" is an absent cap or a non-positive one — the meaning
    /// `NewShare::max_access_count` documents and
    /// `increment_access_count_capped` enforces. The number is read with
    /// `opt_i64_field`, so the `INTEGER` column SQLite hands back as a JSON
    /// number is a cap; the admin shares table used to read it with
    /// `str_field(..).parse()`, which is empty for a JSON number, so a
    /// capped share showed no cap at all on SQLite.
    #[test]
    fn max_access_count_is_none_only_when_the_share_is_uncapped() {
        for shape in [json!(10), json!("10")] {
            assert_eq!(
                ShareRow::from_record(&record(&[("max_access_count", shape.clone())]))
                    .max_access_count,
                Some(10),
                "a cap stored as {shape} is a cap"
            );
        }
        for shape in [json!(0), json!(-1), json!(null)] {
            assert_eq!(
                ShareRow::from_record(&record(&[("max_access_count", shape.clone())]))
                    .max_access_count,
                None,
                "{shape} means unlimited"
            );
        }
        assert_eq!(ShareRow::from_record(&record(&[])).max_access_count, None);
    }

    #[test]
    fn access_log_from_record_decodes_the_whole_row() {
        let row = AccessLogRow::from_record(&Record {
            id: "l1".to_string(),
            data: [
                ("share_id".to_string(), json!("s1")),
                ("accessed_at".to_string(), json!("2026-05-06T10:00:00Z")),
                ("ip_address".to_string(), json!("203.0.113.7")),
                ("user_agent".to_string(), json!("curl/8")),
                ("created_at".to_string(), json!("2026-05-06T10:00:00Z")),
                ("updated_at".to_string(), json!("2026-05-06T10:00:01Z")),
            ]
            .into_iter()
            .collect(),
        });
        assert_eq!(
            row,
            AccessLogRow {
                id: "l1".to_string(),
                share_id: "s1".to_string(),
                accessed_at: "2026-05-06T10:00:00Z".to_string(),
                ip_address: "203.0.113.7".to_string(),
                user_agent: "curl/8".to_string(),
                created_at: "2026-05-06T10:00:00Z".to_string(),
                updated_at: "2026-05-06T10:00:01Z".to_string(),
            }
        );
    }
}
