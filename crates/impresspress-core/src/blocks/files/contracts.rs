//! The caller-facing response types of the files block's JSON endpoints
//! (`/b/storage/api/...`, `/b/storage/admin/api/...`, `/b/cloudstorage/...`).
//!
//! Every response schema the route table publishes is derived from a type in
//! this module, and every one of those types is built here from the repo's
//! row types (the `From`/`TryFrom` impls below) — the only place a stored
//! row becomes a response body. The row types in `repo` are neither
//! `Serialize` nor `JsonSchema`, so a handler cannot answer a row directly
//! and a new column stays in the database until a view publishes it.
//!
//! `last_modified` on [`ObjectInfoResponse`] is a `chrono::DateTime<Utc>`,
//! which schemars' `chrono04` feature renders as
//! `{"type": "string", "format": "date-time"}`.

use serde::{Deserialize, Serialize};

use super::{models::QuotaConfig, repo, repo::Page};

/// Where an object row is in its upload: the `status` column of
/// `impresspress__files__objects`.
///
/// A row is inserted `Pending` *before* the storage upload, so a later quota
/// check counts it, and flipped to `Complete` afterwards.
/// The distinction is load-bearing in two directions at once: quota
/// accounting counts both, so an in-flight reservation is charged, while
/// search and the admin stats count only `Complete`, so a half-finished
/// upload is not listed as a file. Two literals compared in seven places
/// could not say that; one type can, and a row holding neither value is
/// now a decode failure rather than an object that is silently in neither
/// set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObjectStatus {
    // The row exists and counts against quota; the blob may not.
    // `repo::objects::list_page_for_bucket` has no status filter;
    // `search_completed` and the stats count/sum filter on `complete`.
    /// Reserved: the upload is in flight. It counts against quota and appears
    /// in the bucket's object listing, but search and the admin stats leave
    /// it out.
    Pending,
    /// Uploaded: the file is stored.
    Complete,
}

// A record's own `id`, which the list envelope lifts out beside it.
//
// The SDK reads the envelope's `id` (`flattenRecordList`: `{ id: r.id,
// ...r.data }`), and `data` has always carried the same `id` too, so both
// are published; this trait is how the envelope gets it from a typed view
// without re-serializing the view to look.
/// A view that is published as one record of a list.
pub trait RecordData: Serialize {
    /// The record's id, published both on the envelope and inside `data`.
    fn record_id(&self) -> &str;
}

// Generic in the view type rather than carrying an untyped
// `serde_json::Map<String, Value>`. The SDK reads *named fields* out of
// `data` — `FileMetadataRecord` / `FileViewRecord` in `storage.service.ts`,
// `ShareRecord` in `extensions.service.ts` — so an untyped `data` would
// publish a schema that says nothing about the fields those interfaces rely
// on, and the SDK's type-freshness gate would report green on exactly the
// fields that can drift.
/// One record of a list: its `id`, and the record itself under `data`, which
/// carries the same `id`.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct RecordView<T> {
    pub id: String,
    pub data: T,
}

impl<T: RecordData> RecordView<T> {
    /// Wrap one view in its record envelope.
    pub fn new(data: T) -> Self {
        Self {
            id: data.record_id().to_string(),
            data,
        }
    }

    /// Project one stored row and wrap it.
    pub fn from_row<R>(row: R) -> Self
    where
        T: From<R>,
    {
        Self::new(T::from(row))
    }
}

// This is a *published contract*, not an implementation detail. The repo
// returns [`Page`], which is not `Serialize`; this view is the single place a
// page becomes a response body, so the envelope cannot drift one endpoint at a
// time. `packages/impresspress-js/src/services/storage.service.ts` declares
// the matching `RecordListWire<T>` and names `/b/storage/api/search` and
// `/b/storage/api/recent` in its doc comment; that SDK has its own CI job and
// is the reason this shape is preserved rather than modernised here.
/// The envelope the block's JSON list endpoints answer: one page of
/// `records`, the page's `page` and `page_size`, and `total_count` across all
/// pages.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct RecordListView<T> {
    pub records: Vec<RecordView<T>>,
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}

impl<T: RecordData> RecordListView<T> {
    /// Build the envelope from a repo page of stored rows, projecting each
    /// row to its view.
    pub fn from_page<R>(page: Page<R>) -> Self
    where
        T: From<R>,
    {
        Self {
            records: page.rows.into_iter().map(RecordView::from_row).collect(),
            total_count: page.total,
            page: page.page,
            page_size: page.page_size,
        }
    }
}

// `GET /b/storage/api/search`. Not published from the stored row:
// `created_at`/`updated_at` are the table's bookkeeping stamps (`uploaded_at`
// already says when), and the stored blob key and claim id
// (`repo::objects::claim_blob_key`) are how the block finds the bytes.
/// One of the caller's stored files that a search matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ObjectView {
    /// Stable object identifier.
    pub id: String,
    /// The bucket holding the file. A bucket holds at most one file per key.
    pub bucket: String,
    /// The file's key within the bucket.
    pub key: String,
    /// Size in bytes.
    pub size: i64,
    pub content_type: String,
    // `search_completed` filters on `complete`; the field keeps the whole
    // column's type so the SDK's `'pending' | 'complete'` still fits.
    /// Always `complete`: search lists only files whose upload finished.
    #[schemars(extend("enum" = ["complete"]))]
    pub status: ObjectStatus,
    /// User id of the uploader: the caller, since search covers only the
    /// caller's own files.
    pub uploaded_by: String,
    // The column is nullable in migration 001; every upload writes it now.
    /// When the upload began, as an RFC 3339 stamp; empty for a file stored
    /// without one.
    pub uploaded_at: String,
}

impl From<repo::objects::ObjectRow> for ObjectView {
    fn from(row: repo::objects::ObjectRow) -> Self {
        Self {
            id: row.id,
            bucket: row.bucket,
            key: row.key,
            size: row.size,
            content_type: row.content_type,
            status: row.status,
            uploaded_by: row.uploaded_by,
            uploaded_at: row.uploaded_at,
        }
    }
}

impl RecordData for ObjectView {
    fn record_id(&self) -> &str {
        &self.id
    }
}

// `GET /b/storage/api/recent` pages the object-view log
// (`repo::views::list_recent_for_user`), one entry per tracked download, so
// this names the object and when — not its size, type or upload state. The
// log's `created_at`/`updated_at` stamps are not published: `viewed_at` is
// the instant, and an entry is never modified.
/// One recorded download of an object by the caller, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ViewedObjectView {
    /// Id of this recorded view.
    pub id: String,
    /// Bucket holding the viewed object.
    pub bucket: String,
    /// Object key within the bucket.
    pub key: String,
    /// The viewer: the caller, since the list covers only the caller's own
    /// views.
    pub user_id: String,
    /// RFC 3339 instant of the view.
    pub viewed_at: String,
}

impl From<repo::views::ViewRow> for ViewedObjectView {
    fn from(row: repo::views::ViewRow) -> Self {
        Self {
            id: row.id,
            bucket: row.bucket,
            key: row.key,
            user_id: row.user_id,
            viewed_at: row.viewed_at,
        }
    }
}

impl RecordData for ViewedObjectView {
    fn record_id(&self) -> &str {
        &self.id
    }
}

// `GET /b/cloudstorage/shares` (the caller's own) and
// `GET /b/cloudstorage/admin/shares` (every user's). The admin listing keeps
// `token`: it is how an admin finds the share behind a reported
// `/b/storage/direct/{token}` link, and an admin can read the shared object
// anyway. The stored `updated_at` is not published: no write ever modifies
// a share row (an opening bumps `access_count` through
// `increment_field_where`, which stamps nothing), so it is the database's
// copy of the creation instant `created_at` already carries.
/// One share link to an object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShareView {
    /// Id of the share — the `{id}` of `DELETE /b/cloudstorage/shares/{id}`.
    pub id: String,
    /// The opaque token embedded in the public `/b/storage/direct/{token}`
    /// URL: random bytes, hex-encoded, asserting nothing about the share.
    /// This share is what it addresses, and what decides whether the link
    /// still works. No two shares have the same token.
    pub token: String,
    pub bucket: String,
    pub key: String,
    // The ownership key `handle_delete_share` checks.
    /// User id of the share's creator — besides an admin, the only user who
    /// can delete it.
    pub created_by: String,
    /// RFC 3339 creation instant.
    pub created_at: String,
    /// How many times the public link has been opened.
    pub access_count: i64,
    // `None` is a SQL `NULL` or a stored empty string. Creating a share
    // cannot produce one — `NewShare` takes a non-optional expiry — and
    // migration 003 gave every historical row an end.
    /// When this share link stops working, as an RFC 3339 stamp.
    ///
    /// `null` is NOT "never expires": every share link has an end, and a
    /// share that records none cannot be shown to be live, so its public
    /// link is refused.
    pub expires_at: Option<String>,
    /// How many times the public link may be opened, or `null` for no limit.
    pub max_access_count: Option<i64>,
}

impl From<repo::shares::ShareRow> for ShareView {
    fn from(row: repo::shares::ShareRow) -> Self {
        Self {
            id: row.id,
            token: row.token,
            bucket: row.bucket,
            key: row.key,
            created_by: row.created_by,
            created_at: row.created_at,
            access_count: row.access_count,
            expires_at: row.expires_at,
            max_access_count: row.max_access_count,
        }
    }
}

impl RecordData for ShareView {
    fn record_id(&self) -> &str {
        &self.id
    }
}

// `GET /b/cloudstorage/admin/access-logs`. An entry is written once
// (`repo::shares::log_access`) and never modified, so its
// `created_at`/`updated_at` stamps say nothing `accessed_at` does not.
/// One recorded opening of a share's public link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AccessLogView {
    /// Id of this log entry.
    pub id: String,
    /// The share whose link was opened — its `id` in the shares listing.
    pub share_id: String,
    /// RFC 3339 instant of the recorded access.
    pub accessed_at: String,
    /// The address the request came from.
    pub ip_address: String,
    /// The `User-Agent` the request sent.
    pub user_agent: String,
}

impl From<repo::shares::AccessLogRow> for AccessLogView {
    fn from(row: repo::shares::AccessLogRow) -> Self {
        Self {
            id: row.id,
            share_id: row.share_id,
            accessed_at: row.accessed_at,
            ip_address: row.ip_address,
            user_agent: row.user_agent,
        }
    }
}

impl RecordData for AccessLogView {
    fn record_id(&self) -> &str {
        &self.id
    }
}

// A projection of `QuotaConfig`, the enforcement path's type, so a change to
// how quotas are enforced is not by itself a change to this response.
/// Storage caps, as they are enforced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaCapsView {
    /// Most bytes one user may store across all buckets, in-flight uploads
    /// included.
    pub max_storage_bytes: i64,
    /// Largest single file one user may upload, in bytes.
    pub max_file_size_bytes: i64,
    /// Most objects one user may hold in any one bucket, in-flight uploads
    /// included.
    pub max_files_per_bucket: i64,
}

impl From<QuotaConfig> for QuotaCapsView {
    fn from(config: QuotaConfig) -> Self {
        Self {
            max_storage_bytes: config.max_storage_bytes,
            max_file_size_bytes: config.max_file_size_bytes,
            max_files_per_bucket: config.max_files_per_bucket,
        }
    }
}

// `GET /b/cloudstorage/admin/quotas` and the
// `PATCH /b/cloudstorage/admin/quotas/{id}` echo. The caps are flat beside
// `user_id`, as the stored columns have always been answered. The stored
// `reset_period_days` column is not published: nothing enforces it, and an
// update naming it is refused.
/// One user's quota override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaView {
    /// Id of this override.
    pub id: String,
    // UNIQUE index `idx_cloud_quotas_user_id` (migration 001).
    /// The user this override applies to — the `{id}` of
    /// `PATCH /b/cloudstorage/admin/quotas/{id}`. A user has at most one
    /// override.
    pub user_id: String,
    /// The caps this user is held to: each one the override sets replaces
    /// the block default, field by field.
    #[serde(flatten)]
    pub caps: QuotaCapsView,
    /// When the override was first set.
    pub created_at: String,
    /// When an admin last set the override, as an RFC 3339 stamp.
    pub updated_at: String,
}

impl From<repo::quota::QuotaRow> for QuotaView {
    fn from(row: repo::quota::QuotaRow) -> Self {
        Self {
            id: row.id,
            user_id: row.user_id,
            caps: row.config.into(),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

impl RecordData for QuotaView {
    fn record_id(&self) -> &str {
        &self.id
    }
}

/// One object in a bucket listing, as its stored metadata records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ObjectInfoResponse {
    /// Object key.
    pub key: String,
    /// Size in bytes.
    pub size: i64,
    pub content_type: String,
    pub last_modified: chrono::DateTime<chrono::Utc>,
}

// `last_modified` is when the upload that stored it began (`uploaded_at`),
// the instant the SSR object browser shows as "modified"; a row with none —
// the column is nullable in migration 001 — falls back to the row's own
// `updated_at`, which every write stamps. A row with neither readable is
// reported, naming it.
impl TryFrom<repo::objects::ObjectRow> for ObjectInfoResponse {
    type Error = wafer_run::WaferError;

    fn try_from(row: repo::objects::ObjectRow) -> Result<Self, Self::Error> {
        let parse = |stamp: &str| {
            chrono::DateTime::parse_from_rfc3339(stamp)
                .ok()
                .map(|t| t.with_timezone(&chrono::Utc))
        };
        let Some(last_modified) = parse(&row.uploaded_at).or_else(|| parse(&row.updated_at)) else {
            return Err(wafer_run::WaferError::new(
                wafer_run::ErrorCode::Internal,
                format!(
                    "object row {} has no readable uploaded_at or updated_at ({:?}, {:?})",
                    row.id, row.uploaded_at, row.updated_at
                ),
            ));
        };
        Ok(Self {
            key: row.key,
            size: row.size,
            content_type: row.content_type,
            last_modified,
        })
    }
}

/// `GET /b/storage/api/buckets/{name}/objects` response body.
// Offset paging over the object rows (`page` / `page_size`), so there is no
// cursor to carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ObjectListResponse {
    /// Objects in this page.
    pub objects: Vec<ObjectInfoResponse>,
    /// Total number of objects matching the filter (across all pages).
    pub total_count: i64,
}

// Both routes reach the same handler
// ([`super::storage::buckets::handle_list_buckets`]), which differs only in
// whether it scopes the read to the caller.
/// `GET /b/storage/api/buckets` and `GET /b/storage/admin/api/buckets`
/// response body. The two answer the same shape; they differ only in whether
/// the list is scoped to the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BucketListResponse {
    // From `repo::buckets::TABLE` — the single source of truth for bucket
    // existence — not the blob namespace's folder list.
    /// Bucket names.
    pub buckets: Vec<String>,
    /// Whether more buckets are visible to the caller than `buckets` names.
    ///
    /// Buckets are created self-service, so the admin view of this listing
    /// grows with the deployment and is read up to a ceiling; this is how a
    /// client tells a complete list from a prefix of one.
    pub truncated: bool,
}

/// `POST /b/storage/api/buckets` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BucketCreatedResponse {
    /// The bucket that now exists, echoed back from the request.
    pub name: String,
    // Answered only after both the folder and the metadata row are in place.
    /// Always `true` — this body is answered only once the bucket exists.
    pub created: bool,
}

/// The body every delete on this block answers: bucket, object and share.
/// One type because it is one shape; a per-endpoint copy is how the three
/// would drift apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeletedResponse {
    /// Always `true` — a delete that did not happen is an error status, not
    /// `{"deleted": false}`.
    pub deleted: bool,
}

/// `POST /b/storage/api/buckets/{name}/objects` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ObjectUploadedResponse {
    pub bucket: String,
    /// The stored key. For a multipart upload sent without `?key=` it is the
    /// file part's `filename`, so it may differ from what the caller expected.
    pub key: String,
    /// Always `true`.
    pub uploaded: bool,
}

/// `GET /b/storage/admin/api/stats` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StorageStatsResponse {
    /// Objects whose upload completed. A `pending` reservation, whose upload
    /// is still in flight, is not counted.
    pub total_objects: i64,
    /// Sum of `size` over the same set.
    pub total_size_bytes: i64,
    // Rows in `repo::buckets::TABLE`, not folders in the blob namespace.
    /// Number of buckets.
    pub bucket_count: i64,
}

/// `POST /b/cloudstorage/shares` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShareCreatedResponse {
    /// Id of the new share — the `{id}` of `DELETE
    /// /b/cloudstorage/shares/{id}`.
    pub id: String,
    /// The opaque token embedded in `direct_url`. It carries no expiry of
    /// its own: the share's own `expires_at` and access cap are what end a
    /// link.
    pub token: String,
    /// Path of the public share link, relative to the deployment's origin.
    pub direct_url: String,
}

// Both numbers are computed over the caller's object rows, not read from a
// counter column.
/// The `usage` half of the quota response, counted from the caller's objects
/// when it is asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaUsageView {
    /// Total `size` of the caller's objects, `pending` reservations
    /// included.
    pub total_bytes: i64,
    /// Objects the caller owns across all buckets, `pending` included; not
    /// what the per-bucket `max_files_per_bucket` cap is checked against.
    pub file_count: i64,
}

/// `GET /b/cloudstorage/quota` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaResponse {
    /// The caller's effective caps: their per-user override if they have
    /// one, otherwise the block defaults.
    pub quota: QuotaCapsView,
    pub usage: QuotaUsageView,
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wafer_core::clients::database::Record;

    use super::*;
    use crate::blocks::files::repo;

    fn object_page() -> Page<repo::objects::ObjectRow> {
        Page {
            rows: vec![repo::objects::ObjectRow::from_record(&Record {
                id: "o1".to_string(),
                data: [
                    ("id", json!("o1")),
                    ("bucket", json!("photos")),
                    ("key", json!("nested/a.png")),
                    ("size", json!(1024)),
                    ("content_type", json!("image/png")),
                    ("status", json!(ObjectStatus::Complete)),
                    ("uploaded_by", json!("alice")),
                    ("uploaded_at", json!("2026-05-06T10:00:00Z")),
                    ("created_at", json!("2026-05-06T10:00:00Z")),
                    ("updated_at", json!("2026-05-06T10:00:01Z")),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            })
            .expect("the fixture row decodes")],
            total: 7,
            page: 2,
            page_size: 20,
        }
    }

    /// The envelope IS a published contract, and this is the test that says
    /// so. `packages/impresspress-js/src/services/storage.service.ts`
    /// declares `RecordListWire<T>` as `{ records: Array<{ id, data }>,
    /// total_count, page, page_size }` and flattens it with
    /// `{ id: r.id, ...r.data }`, naming `/b/storage/api/search` and
    /// `/b/storage/api/recent`. Every key asserted below is one that SDK
    /// reads; the SDK has its own CI job, so a Rust-only refactor that
    /// reshaped this would break it silently.
    #[test]
    fn the_envelope_matches_the_sdk_s_record_list_wire() {
        let body = serde_json::to_value(RecordListView::<ObjectView>::from_page(object_page()))
            .expect("the view serializes");

        assert_eq!(body["total_count"], json!(7));
        assert_eq!(body["page"], json!(2));
        assert_eq!(body["page_size"], json!(20));

        let records = body["records"].as_array().expect("records is an array");
        assert_eq!(records.len(), 1);
        // `flattenRecordList` takes `id` from the envelope, not from `data`.
        assert_eq!(records[0]["id"], json!("o1"));
        // ...and spreads `data` over it, so the columns live one level down.
        assert_eq!(records[0]["data"]["key"], json!("nested/a.png"));
        assert_eq!(records[0]["data"]["size"], json!(1024));
    }

    /// The backends put `id` in `data` as well as in the envelope
    /// (`row_to_record` inserts every column and copies `id` out), so the
    /// view does too — otherwise a consumer reading `data.id` would start
    /// seeing `undefined`.
    #[test]
    fn the_record_view_publishes_id_in_both_places() {
        let body = serde_json::to_value(RecordListView::<ObjectView>::from_page(object_page()))
            .expect("serializes");
        assert_eq!(body["records"][0]["id"], json!("o1"));
        assert_eq!(body["records"][0]["data"]["id"], json!("o1"));
    }

    /// A search hit publishes the file, not the stored row: the table's
    /// `created_at`/`updated_at` bookkeeping stays behind, and every field
    /// the SDK's `FileMetadataRecord` names is there.
    #[test]
    fn the_object_view_publishes_the_file_not_the_row() {
        let body = serde_json::to_value(RecordListView::<ObjectView>::from_page(object_page()))
            .expect("serializes");
        let data = body["records"][0]["data"]
            .as_object()
            .expect("data is an object")
            .clone();
        let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "bucket",
                "content_type",
                "id",
                "key",
                "size",
                "status",
                "uploaded_at",
                "uploaded_by",
            ],
        );
    }

    /// The second SDK consumer of this envelope:
    /// `packages/impresspress-js/src/services/extensions.service.ts`
    /// `CloudStorageExtension.listShares` reads `GET /b/cloudstorage/shares`
    /// as `{ records: Array<{ id, data }>, total_count, page, page_size }`
    /// and flattens it into `ShareRecord`. Every field that interface names
    /// must be published.
    #[test]
    fn the_share_envelope_carries_every_field_the_sdk_s_share_record_names() {
        let row = repo::shares::ShareRow::from_record(&Record {
            id: "s1".to_string(),
            data: [
                ("id", json!("s1")),
                ("token", json!("tok")),
                ("bucket", json!("photos")),
                ("key", json!("a.png")),
                ("created_by", json!("alice")),
                ("created_at", json!("2026-05-06T10:00:00Z")),
                ("access_count", json!(4)),
                ("expires_at", json!("2026-06-06T10:00:00Z")),
                ("max_access_count", json!(10)),
                ("updated_at", json!("2026-05-06T10:00:01Z")),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        });
        let body =
            serde_json::to_value(RecordView::<ShareView>::from_row(row)).expect("serializes");

        assert_eq!(body["id"], json!("s1"));
        for field in [
            "id",
            "token",
            "bucket",
            "key",
            "created_by",
            "created_at",
            "access_count",
            "expires_at",
            "max_access_count",
        ] {
            assert!(
                body["data"].get(field).is_some(),
                "`ShareRecord.{field}` missing from the published share row: {body}"
            );
        }
    }

    /// `QuotaView` groups its caps behind a `QuotaCapsView`, but the
    /// response has always answered them flat beside `user_id`, so the wire
    /// stays flat — `#[serde(flatten)]`. A `caps` key here would be a
    /// reshaped response body for `GET /b/cloudstorage/admin/quotas` and
    /// `PATCH /b/cloudstorage/admin/quotas/{id}`.
    #[test]
    fn the_quota_row_publishes_its_caps_flat_not_nested() {
        let row = repo::quota::QuotaRow::from_record(&Record {
            id: "q1".to_string(),
            data: [
                ("id", json!("q1")),
                ("user_id", json!("u-9")),
                ("max_storage_bytes", json!(2048)),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        });
        let body =
            serde_json::to_value(RecordView::<QuotaView>::from_row(row)).expect("serializes");

        assert_eq!(body["id"], json!("q1"));
        assert_eq!(body["data"]["user_id"], json!("u-9"));
        assert_eq!(body["data"]["max_storage_bytes"], json!(2048));
        assert!(
            body["data"].get("caps").is_none(),
            "the caps are columns, not a nested object: {body}"
        );
    }
}
