//! Response types for the `/b/storage/api/...` JSON surface that the
//! `.output::<T>()` derive migration can actually reach.
//!
//! [`ObjectInfoResponse`] / [`ObjectListResponse`] are what
//! [`super::storage::objects::handle_list_objects`] builds from the bucket's
//! object rows (`repo::objects::list_page_for_bucket`) and serializes, so the
//! type this schema is derived from is the type that goes out on the wire.
//! Their field names match `wafer_core::clients::storage::{ObjectInfo,
//! ObjectList}`, and `packages/impresspress-js` reads them. `last_modified` is a
//! `chrono::DateTime<Utc>`, which schemars' `chrono04` feature renders as
//! `{"type": "string", "format": "date-time"}`.

use serde::{Deserialize, Serialize};

use super::repo::Page;

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
    /// Reserved: the row exists and counts against quota, the blob may not.
    Pending,
    /// Uploaded: the blob is in storage and the object is listable.
    Complete,
}

/// One record in the [`RecordListView`] envelope: the row's `id` beside the
/// row's columns, exactly as `wafer_core::clients::database::Record`
/// serializes. The backends put `id` in BOTH places (`row_to_record` inserts
/// every column into `data` and copies `id` out to the envelope), and
/// `packages/impresspress-js` reads the envelope one
/// (`flattenRecordList`: `{ id: r.id, ...r.data }`), so both are published.
///
/// Generic in the row type rather than carrying an untyped
/// `serde_json::Map<String, Value>`. The SDK reads *named columns* out of
/// `data` — `FileMetadataRecord` in `storage.service.ts`, `ShareRecord` in
/// `extensions.service.ts` — so an untyped `data` would publish a schema
/// that says nothing about the fields those interfaces rely on, and the
/// SDK's type-freshness gate would report green on exactly the columns that
/// can drift. `T` is the row struct the handler already had in hand, so the
/// bytes are unchanged.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct RecordView<T> {
    pub id: String,
    pub data: T,
}

impl<T: Serialize> RecordView<T> {
    /// Build the record envelope for one typed row.
    ///
    /// The row serializes to its columns — every row type mirrors its table
    /// column-for-column precisely so this cannot drop one — and `id` is
    /// lifted out to the envelope while staying in `data`.
    pub fn from_row(row: T) -> Self {
        let id = match serde_json::to_value(&row) {
            Ok(serde_json::Value::Object(map)) => map
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            // Unreachable: every row type is a plain struct of scalars. A
            // non-object would mean a row grew a serde attribute that
            // changes its shape, which the round-trip test below catches.
            _ => String::new(),
        };
        Self { id, data: row }
    }
}

/// The `RecordList` envelope the block's JSON list endpoints publish:
/// `{ records, total_count, page, page_size }`.
///
/// This is a *published contract*, not an implementation detail. The repo
/// returns [`Page`], which is not `Serialize`; this view is the single place
/// a page becomes a response body, so the envelope cannot drift one endpoint
/// at a time. `packages/impresspress-js/src/services/storage.service.ts`
/// declares the matching `RecordListWire<T>` and names
/// `/b/storage/api/search` and `/b/storage/api/recent` in its doc comment;
/// that SDK has its own CI job and is the reason this shape is preserved
/// rather than modernised here. Changing it is a deliberate, separate
/// change that moves the SDK in lockstep — see the follow-up in the PR that
/// introduced this type.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct RecordListView<T> {
    pub records: Vec<RecordView<T>>,
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}

impl<T: Serialize> RecordListView<T> {
    /// Build the envelope from a repo page of typed rows.
    pub fn from_page(page: Page<T>) -> Self {
        Self {
            records: page.rows.into_iter().map(RecordView::from_row).collect(),
            total_count: page.total,
            page: page.page,
            page_size: page.page_size,
        }
    }
}

/// One object in a bucket listing, as its metadata row records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ObjectInfoResponse {
    /// Object key.
    pub key: String,
    /// Size in bytes.
    pub size: i64,
    pub content_type: String,
    pub last_modified: chrono::DateTime<chrono::Utc>,
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

/// `GET /b/storage/api/buckets` and `GET /b/storage/admin/api/buckets`
/// response body — both routes reach the same handler
/// ([`super::storage::buckets::handle_list_buckets`]), which differs only in
/// whether it scopes the read to the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BucketListResponse {
    /// Bucket names, from `repo::buckets::TABLE` — the single source of
    /// truth for bucket existence. Not the blob namespace's folder list.
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
    /// Always `true` — the handler answers this body only after both the
    /// folder and the metadata row are in place.
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
    /// The stored key. For a multipart upload this is the key the handler
    /// resolved, which may differ from the one the caller sent.
    pub key: String,
    /// Always `true`.
    pub uploaded: bool,
}

/// `GET /b/storage/admin/api/stats` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StorageStatsResponse {
    /// Objects in `Complete` status. A `Pending` reservation is not a file.
    pub total_objects: i64,
    /// Sum of `size` over the same set.
    pub total_size_bytes: i64,
    /// Rows in `repo::buckets::TABLE`, not folders in the blob namespace.
    pub bucket_count: i64,
}

/// `POST /b/cloudstorage/shares` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShareCreatedResponse {
    /// Row id of the new share — the `{id}` of `DELETE
    /// /b/cloudstorage/shares/{id}`.
    pub id: String,
    /// The opaque token embedded in `direct_url`. It carries no expiry of
    /// its own: the share's own `expires_at` and access cap are what end a
    /// link.
    pub token: String,
    /// Path of the public share link, relative to the deployment's origin.
    pub direct_url: String,
}

/// The `usage` half of [`QuotaResponse`]. Both numbers are computed over the
/// caller's object rows, not read from a counter column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaUsageView {
    /// `SUM(size)` over the caller's rows, `Pending` reservations included.
    pub total_bytes: i64,
    /// Objects the caller owns across all buckets, `Pending` included; not
    /// what the per-bucket `max_files_per_bucket` cap is checked against.
    pub file_count: i64,
}

/// `GET /b/cloudstorage/quota` response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QuotaResponse {
    /// The caller's effective caps: their override row if they have one,
    /// otherwise the block defaults.
    pub quota: super::models::QuotaConfig,
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
        let body = serde_json::to_value(RecordListView::from_page(object_page()))
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
        let body =
            serde_json::to_value(RecordListView::from_page(object_page())).expect("serializes");
        assert_eq!(body["records"][0]["id"], json!("o1"));
        assert_eq!(body["records"][0]["data"]["id"], json!("o1"));
    }

    /// Every column of the objects table reaches the wire. The row type is
    /// what the view serializes, so a column missing from the row is a
    /// column missing from the response — this is the assertion that makes
    /// "the row mirrors the table" load-bearing rather than aspirational.
    #[test]
    fn the_object_row_publishes_every_column_of_its_table() {
        let body =
            serde_json::to_value(RecordListView::from_page(object_page())).expect("serializes");
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
                "created_at",
                "id",
                "key",
                "size",
                "status",
                "updated_at",
                "uploaded_at",
                "uploaded_by",
            ],
            "the columns `migrations/001_initial_schema.sqlite.sql` declares \
             for the objects table, plus its `id` primary key"
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
        let body = serde_json::to_value(RecordView::from_row(row)).expect("serializes");

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

    /// `QuotaRow` groups its cap columns behind a `QuotaConfig` for the
    /// enforcement path, but they ARE columns of one table, so the wire
    /// stays flat — `#[serde(flatten)]`. A `config` key here would be a
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
        let body = serde_json::to_value(RecordView::from_row(row)).expect("serializes");

        assert_eq!(body["id"], json!("q1"));
        assert_eq!(body["data"]["user_id"], json!("u-9"));
        assert_eq!(body["data"]["max_storage_bytes"], json!(2048));
        assert!(
            body["data"].get("config").is_none(),
            "the caps are columns, not a nested object: {body}"
        );
    }
}
