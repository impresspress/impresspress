//! Row-level access over `impresspress__files__objects`.
//!
//! Object metadata rows — one row per uploaded file (sibling of the raw
//! storage blob in `wafer-run/storage`). Tracks size, content type,
//! status, uploader and timestamps. A row is claimed `pending` *before* the
//! storage upload ([`reserve_upload`], whose write is refused when it would
//! take the uploader past a quota cap) and flipped to `complete` afterward;
//! quota accounting sums/counts by `uploaded_by` (including in-flight
//! `pending` reservations), while user-facing search and admin stats only see
//! `complete` rows.
//!
//! `(bucket, key)` is UNIQUE, so a re-upload reuses the existing row rather
//! than inserting a second one — see [`reserve_upload`].
//!
//! The row also says WHERE the object's bytes are: `blob_key`, the storage
//! key within the bucket. Each reservation stores its bytes under a key of
//! its own ([`claim_blob_key`]), so two uploads of one object key never write
//! the same blob, and every reader resolves the bytes through the row
//! ([`find_blob_key`]). A NULL `blob_key` is a row written before migration
//! 005, whose bytes are at the object's own key.

use std::collections::HashMap;

use wafer_block::{
    db::{Filter, FilterOp, ListOptions, SortField},
    wire::database::{self as wire, InsertGuardedResponse, UpdateGuardedResponse},
};
use wafer_core::clients::database::{self as db, CapGuard, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

use super::{
    super::{contracts::ObjectStatus, models::QuotaConfig},
    Page,
};
use crate::{
    db_read::{self, Bound},
    util::{enum_column_or, RecordExt},
};

/// Object metadata table — one row per uploaded file (sibling of the raw
/// storage blob in `wafer-run/storage`). Tracks size, content type, status,
/// uploader and timestamps.
pub const TABLE: &str = "impresspress__files__objects";

/// One object-metadata row, decoded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct ObjectRow {
    pub id: String,
    /// Bucket name; `(bucket, key)` is unique.
    pub bucket: String,
    /// Object key within the bucket.
    pub key: String,
    /// Size in bytes. `i64_field` so a TEXT-stored number still counts
    /// toward the quota rather than reading as zero.
    pub size: i64,
    pub content_type: String,
    /// `Pending` while the storage upload is in flight, `Complete` after.
    /// Quota accounting counts both; user-facing search and admin stats see
    /// only `Complete`.
    pub status: ObjectStatus,
    pub uploaded_by: String,
    /// When the upload was reserved — the timestamp the object browser
    /// renders as "modified", and the one `list_stale_pending` compares.
    pub uploaded_at: String,
    pub created_at: String,
    pub updated_at: String,
}

impl ObjectRow {
    /// The one decode of an object row.
    ///
    /// Fallible since `status` became a type: a row in neither `Pending` nor
    /// `Complete` is counted by neither the quota sum nor the listings, so
    /// it is reported naming the row rather than carried silently. An
    /// *empty* column reads as `Complete`, which is exactly what the
    /// column's own `DEFAULT 'complete'`
    /// (`migrations/001_initial_schema.sqlite.sql`) gives a row inserted
    /// without it; every production insert names the value.
    pub fn from_record(rec: &Record) -> Result<Self, WaferError> {
        Ok(Self {
            id: rec.id.clone(),
            bucket: rec.str_field("bucket").to_string(),
            key: rec.str_field("key").to_string(),
            size: rec.i64_field("size"),
            content_type: rec.str_field("content_type").to_string(),
            status: enum_column_or(rec, "status", ObjectStatus::Complete)?,
            uploaded_by: rec.str_field("uploaded_by").to_string(),
            uploaded_at: rec.str_field("uploaded_at").to_string(),
            created_at: rec.str_field("created_at").to_string(),
            updated_at: rec.str_field("updated_at").to_string(),
        })
    }
}

/// Filter matching only fully uploaded rows, excluding in-flight
/// [`ObjectStatus::Pending`] reservations.
fn complete_filter() -> [Filter; 1] {
    [status_is(ObjectStatus::Complete)]
}

/// An equality filter on the `status` column.
fn status_is(status: ObjectStatus) -> Filter {
    Filter {
        field: "status".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::json!(status),
    }
}

/// Filter matching all objects uploaded by `user_id` (the rows that count
/// toward that user's quota, including in-flight `pending` reservations).
fn owned_objects_filter(user_id: &str) -> Vec<Filter> {
    vec![Filter {
        field: "uploaded_by".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id.to_string()),
    }]
}

/// Escape SQL LIKE wildcards (`%`, `_`) and the escape char itself (`\`) in
/// user-supplied search terms so a user searching for `100% off` doesn't
/// also match arbitrary characters.
///
/// SQLite's `LIKE` has *no* default escape character — a bare backslash is
/// just a literal byte, so escaping here would be silently inert on its own.
/// What makes it effective is the `wafer-sql-utils` `FilterOp::Like` builder
/// (used by [`search_completed`]'s query below), which renders an explicit
/// `ESCAPE '\'` clause on every backend (SQLite/D1 and Postgres) — see
/// `wafer-sql-utils::query::leaf_expr`. Without that clause, a query
/// containing `_` or `%` would match as a wildcard instead of a literal
/// character.
fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(c);
            }
            other => out.push(other),
        }
    }
    out
}

/// The storage key a reservation stores its upload's bytes under, within the
/// object's bucket: the object's key with `claim_id` in front of its last
/// segment — `reports/q3.pdf` becomes `reports/{claim_id}~q3.pdf`.
///
/// A key per reservation is what stops an upload that lost its claim from
/// overwriting the bytes of the upload that took the key over: its late
/// write lands on a blob no row names. The last segment keeps the object's
/// file name, extension included, because the local-storage backend derives
/// a blob's content type from its key. `claim_id` is a fresh UUID (no `/`,
/// fixed length), so the key splits back into exactly one `(key, claim_id)`
/// pair, and no object key a user uploaded before it was minted can equal it.
pub fn claim_blob_key(key: &str, claim_id: &str) -> String {
    match key.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/{claim_id}~{name}"),
        None => format!("{claim_id}~{key}"),
    }
}

/// The storage key the row's bytes are under: its `blob_key`, or — for a row
/// written before migration 005, whose `blob_key` is NULL — its object key.
fn stored_blob_key(key: &str, blob_key: Option<String>) -> String {
    blob_key.unwrap_or_else(|| key.to_string())
}

/// The storage key that holds the bytes of `(bucket, key)`, or `None` when
/// the key has no row.
///
/// The one way a reader finds an object's bytes. The bytes are not at the
/// object key: they are wherever the reservation that stored them put them,
/// and the row names that place. A `Pending` row names the blob it serves
/// until its upload completes — the object it is replacing, or, for a first
/// upload, the blob that upload is writing, which is absent until it lands.
pub async fn find_blob_key(
    ctx: &dyn Context,
    bucket: &str,
    key: &str,
) -> Result<Option<String>, WaferError> {
    Ok(find_stored(ctx, bucket, key)
        .await?
        .map(|found| stored_blob_key(key, found.blob_key)))
}

/// The row for `(bucket, key)`, or `None` when the key holds no object.
///
/// `(bucket, key)` is UNIQUE (`idx_objects_bucket_key`, migration 001), so
/// there is at most one.
pub async fn find_by_bucket_key(
    ctx: &dyn Context,
    bucket: &str,
    key: &str,
) -> Result<Option<ObjectRow>, WaferError> {
    let records = db_read::list_bounded(
        ctx,
        TABLE,
        bucket_key_filters(bucket, key),
        Bound::UniqueKey("idx_objects_bucket_key on (bucket, key)"),
    )
    .await?;
    records.first().map(ObjectRow::from_record).transpose()
}

fn bucket_key_filters(bucket: &str, key: &str) -> Vec<Filter> {
    vec![
        Filter {
            field: "bucket".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(bucket.to_string()),
        },
        Filter {
            field: "key".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(key.to_string()),
        },
    ]
}

/// How long a [`ObjectStatus::Pending`] row is taken to belong to an upload
/// still in flight, in seconds. Past it the row is an orphan: an upload whose
/// request died between claiming the key and settling it. The largest
/// realistic upload finishes well inside an hour.
///
/// Two readers, one policy: [`reserve_upload`] refuses a key whose `Pending`
/// row is younger than this and takes over one that is older, and
/// `quota::sweep_stale_pending` deletes the uploader's rows past it.
pub const PENDING_RESERVATION_TTL_SECONDS: i64 = 3600;

/// The RFC 3339 instant before which a `Pending` row's `uploaded_at` makes it
/// stale — compared as a string, the way the column is written and the way
/// [`list_stale_pending`] compares it.
pub fn pending_reservation_cutoff() -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(PENDING_RESERVATION_TTL_SECONDS)).to_rfc3339()
}

/// The stored object a [`Reservation`] took the place of, as its row read
/// before the reservation overwrote it. [`release_reservation`] writes it back
/// when the storage upload fails. Always a `Complete` row: a reservation never
/// takes over an upload still in flight, and a stale `Pending` row is not an
/// object to restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacedObject {
    pub size: i64,
    pub content_type: String,
    pub uploaded_by: String,
    pub uploaded_at: String,
}

/// A claim on `(bucket, key)` held while a storage upload is in flight, taken
/// by [`reserve_upload`] and settled by [`mark_complete`] or
/// [`release_reservation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    /// Row id of the reservation — a new row, or the row it took over.
    pub id: String,
    /// The bucket the claimed key is in.
    pub bucket: String,
    /// The claimed key.
    pub key: String,
    /// This reservation's token, written to the row's `claim_id` column when
    /// the claim was taken. Random per reservation, so the row still carries
    /// it exactly as long as no other reservation has taken the row since —
    /// which is what [`mark_complete`] and [`release_reservation`] require
    /// before they touch it.
    pub claim_id: String,
    /// `Some` when this upload overwrites an object already stored under the
    /// key, carrying what that object's row said. `None` when there was no
    /// stored object to put back: a new key, or a stale `Pending` row.
    pub replaced: Option<ReplacedObject>,
    /// The storage key this reservation stores its bytes under
    /// ([`claim_blob_key`]) — the row's `blob_key` once [`mark_complete`]
    /// settles it.
    pub blob_key: String,
    /// Blobs no row names once this reservation's bytes are the row's: the
    /// object it replaces, and whatever an orphaned reservation it took over
    /// had stored. Deleted after [`mark_complete`] points the row at
    /// [`Self::blob_key`], or after [`release_reservation`] deletes the row —
    /// see [`Self::blobs_released_by_rollback`].
    pub superseded_blobs: Vec<String>,
}

impl Reservation {
    /// What a failed upload deletes from storage once [`release_reservation`]
    /// has settled the row: always its own blob (a `put` that failed may
    /// still have written part of it), and — when the row was deleted rather
    /// than put back — every blob the row named, since no row names them
    /// now. When the reservation put a replaced object back, that object's
    /// blob is served again and is kept.
    pub fn blobs_released_by_rollback(&self) -> Vec<&str> {
        let mut blobs = vec![self.blob_key.as_str()];
        if self.replaced.is_none() {
            blobs.extend(self.superseded_blobs.iter().map(String::as_str));
        }
        blobs
    }
}

/// Why [`reserve_upload`] did not claim the key.
#[derive(Debug)]
pub enum ReserveError {
    /// Another upload holds the key, or took it between this reservation's
    /// read and its write. A conflict the caller retries once that upload
    /// settles.
    Held,
    /// The key is held by a reservation of this same uploader that is younger
    /// than [`PENDING_RESERVATION_TTL_SECONDS`]: an upload of theirs still in
    /// flight, or one whose storage write finished but whose row could not be
    /// marked complete. The row cannot say which, so it is not taken over
    /// early — the row describes one upload in flight, and a second would
    /// leave the first with no row to settle.
    /// `since` is that reservation's `uploaded_at`; the key is free
    /// [`PENDING_RESERVATION_TTL_SECONDS`] after it.
    HeldByOwnEarlierUpload { since: String },
    /// The upload would take its uploader past `max_storage_bytes`: the bytes
    /// they store, in-flight reservations included, plus this upload's, less
    /// those of the row it replaces when that row is theirs.
    OverStorageQuota,
    /// The upload would be one object more than `max_files_per_bucket` among
    /// the rows its uploader holds in the bucket. Never for a row the
    /// uploader already holds there: replacing it adds none.
    OverFileCount,
    /// The database call itself failed.
    Db(WaferError),
}

impl From<WaferError> for ReserveError {
    fn from(error: WaferError) -> Self {
        Self::Db(error)
    }
}

/// Claim `(bucket, key)` for an upload of `size` bytes, BEFORE the storage
/// upload runs, within `quota`'s caps. `uploaded_at` is stamped with
/// [`crate::util::now_rfc3339`], and `claim_id` with a fresh random token the
/// returned [`Reservation`] carries.
///
/// The caps are enforced by the write itself: every insert or take-over this
/// makes is a guarded write ([`quota_guards`]) that the database refuses when
/// the uploader's stored bytes, or their rows in the bucket, as they stand at
/// that write, leave no room for it — [`ReserveError::OverStorageQuota`] or
/// [`ReserveError::OverFileCount`]. The check and the write are one atomic
/// step against every other guarded write to the table, so uploads racing
/// for the same cap cannot all be admitted. Rows in flight count: a
/// reservation charges its size to its uploader from the moment it is taken.
///
/// `(bucket, key)` is UNIQUE, so the key has at most one row, and what that
/// row says decides the claim:
///
/// - **No row**: a guarded insert. If another upload's insert got there
///   first — the unique index refuses this one — it decides again on the row
///   it reads back.
/// - **`Complete`**: a re-upload. The reservation TAKES OVER the row,
///   flipping it to [`ObjectStatus::Pending`] with the new size, content type
///   and uploader. The row stops counting against whoever it was charged to
///   and charges the new size to the new uploader, so a user replacing their
///   own object is charged the difference and adds no file.
///   [`release_reservation`] puts the old values back if the upload fails.
/// - **`Pending`, younger than [`PENDING_RESERVATION_TTL_SECONDS`]**: an
///   upload of the key is in flight, or was and could not be recorded.
///   Refused: the row records one upload in flight, and taking it over
///   would leave that upload's completion nothing to settle. [`ReserveError::HeldByOwnEarlierUpload`] when that reservation is
///   this uploader's own, [`ReserveError::Held`] otherwise.
/// - **`Pending`, older**: an orphan. Taken over as a fresh claim, with
///   nothing to put back.
///
/// Also [`ReserveError::Held`] when another upload claimed the row between
/// this one's read and its write (every take-over is conditional on the row
/// still carrying the `claim_id` it was read with), or when the insert lost
/// to another upload whose row is gone again by the read-back — that upload
/// failed and released the key, which is free to retry.
pub async fn reserve_upload(
    ctx: &dyn Context,
    bucket: &str,
    key: &str,
    size: usize,
    content_type: &str,
    uploaded_by: &str,
    quota: &QuotaConfig,
) -> Result<Reservation, ReserveError> {
    let uploaded_at = crate::util::now_rfc3339();
    let claim_id = uuid::Uuid::new_v4().to_string();
    let claim = PendingClaim {
        bucket,
        key,
        size,
        content_type,
        uploaded_by,
        uploaded_at: &uploaded_at,
        claim_id: &claim_id,
        quota,
    };

    if let Some(existing) = find_stored(ctx, bucket, key).await? {
        return claim_existing(ctx, existing, &claim).await;
    }
    let not_inserted = match insert_reservation(ctx, &claim).await? {
        Inserted::Created(id) => {
            return Ok(Reservation {
                id,
                bucket: bucket.to_string(),
                key: key.to_string(),
                blob_key: claim.blob_key(),
                claim_id,
                replaced: None,
                superseded_blobs: Vec::new(),
            })
        }
        Inserted::KeyTaken => ReserveError::Held,
        Inserted::Refused(refusal) => refusal,
    };
    // A refused insert is decided on the key's row as well when there is one
    // by now: an insert is refused on the caps before the unique index is
    // consulted, and a take-over of that row may fit where a new row did not.
    match find_stored(ctx, bucket, key).await? {
        Some(existing) => claim_existing(ctx, existing, &claim).await,
        None => Err(not_inserted),
    }
}

/// The column values a [`Reservation`] writes.
struct PendingClaim<'a> {
    bucket: &'a str,
    key: &'a str,
    size: usize,
    content_type: &'a str,
    uploaded_by: &'a str,
    uploaded_at: &'a str,
    claim_id: &'a str,
    /// The caps the claim's write must leave its uploader within.
    quota: &'a QuotaConfig,
}

impl PendingClaim<'_> {
    /// Where this claim's upload stores its bytes.
    fn blob_key(&self) -> String {
        claim_blob_key(self.key, self.claim_id)
    }
}

/// Index in [`quota_guards`] of the storage-bytes guard. It comes first, so an
/// upload over both caps is refused for its bytes.
const STORAGE_GUARD: usize = 0;
/// Index in [`quota_guards`] of the per-bucket file-count guard, present when
/// `max_files_per_bucket` is positive and the write does not replace a row
/// of the uploader's own.
const FILE_COUNT_GUARD: usize = 1;

/// The caps `claim`'s write must leave its uploader within, measured over the
/// table as it stands at the write. `replacing` is the row the write takes
/// over, as it was read, which the guards leave out: its size and its place
/// in the bucket are the claim's once the write lands, whoever they were
/// charged to before.
///
/// - [`STORAGE_GUARD`]: the uploader's `SUM(size)`, plus the claim's size, is
///   at most `max_storage_bytes`.
/// - [`FILE_COUNT_GUARD`]: the uploader's rows in the bucket number fewer
///   than `max_files_per_bucket`, so the write leaves at most that many. A
///   cap of `0` or less is no cap, and adds no guard. Nor does a write that
///   replaces a row the uploader already holds: it adds no file, so a user
///   over the cap — an admin lowered it below what they hold — can still
///   overwrite what they have. The take-over is conditional on the claim the
///   row was read with, so the row is still theirs when the write lands.
fn quota_guards(claim: &PendingClaim<'_>, replacing: Option<&ObjectRow>) -> Vec<CapGuard> {
    let mut owned = owned_objects_filter(claim.uploaded_by);
    if let Some(row) = replacing {
        owned.push(Filter {
            field: "id".to_string(),
            operator: FilterOp::NotEqual,
            value: serde_json::Value::String(row.id.clone()),
        });
    }
    let replaces_own = replacing.is_some_and(|row| row.uploaded_by == claim.uploaded_by);
    let size = i64::try_from(claim.size).unwrap_or(i64::MAX);
    let mut guards = vec![CapGuard::SumAtMost {
        field: "size".to_string(),
        filters: owned.clone(),
        add: size,
        cap: claim.quota.max_storage_bytes,
    }];
    if claim.quota.max_files_per_bucket > 0 && !replaces_own {
        let mut in_bucket = owned;
        in_bucket.push(Filter {
            field: "bucket".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(claim.bucket.to_string()),
        });
        guards.push(CapGuard::CountBelow {
            filters: in_bucket,
            cap: claim.quota.max_files_per_bucket,
        });
    }
    guards
}

/// The refusal a guarded write reports by [`quota_guards`] index.
fn refused_by(guard: usize) -> ReserveError {
    match guard {
        STORAGE_GUARD => ReserveError::OverStorageQuota,
        FILE_COUNT_GUARD => ReserveError::OverFileCount,
        other => ReserveError::Db(WaferError::new(
            ErrorCode::Internal,
            format!("an upload reservation was refused by guard {other}, which it did not set"),
        )),
    }
}

/// The key's row as a writer reads it: the decoded row, plus the `claim_id`
/// a take-over or a delete is conditional on and the `blob_key` it serves.
/// Both are kept off [`ObjectRow`], which is published: the token is the
/// row's lock and the blob key is where storage keeps the bytes, neither of
/// which a reader of the object's metadata needs.
pub struct StoredRow {
    pub row: ObjectRow,
    /// `None` for a row written before migration 004 added the column.
    claim_id: Option<String>,
    /// `None` for a row written before migration 005 added the column: its
    /// bytes are at the object key ([`stored_blob_key`]).
    blob_key: Option<String>,
}

impl StoredRow {
    /// Every blob this row may have caused to be stored — what must be
    /// deleted from storage once the row is gone: the one it serves; its
    /// object key, where a row from before migration 005 kept its bytes and
    /// where an isolate still running the previous release writes a
    /// replacement during a rollout; and the one its latest reservation's
    /// upload writes under [`claim_blob_key`].
    ///
    /// None of them is a blob another row serves. The object key is this
    /// row's own (`(bucket, key)` is unique), and a legacy row serves only
    /// its object key. A claim blob key is minted from a fresh random claim
    /// id and never returned by any API, so it equals another row's key only
    /// if a client uploaded an object whose key spells an unseen UUID in
    /// that position. For a claim id written before 005 the claim blob key
    /// was never written at all — the release that minted it stored at the
    /// object key — so deleting it removes nothing.
    pub fn blobs(&self) -> Vec<String> {
        let mut blobs = vec![stored_blob_key(&self.row.key, self.blob_key.clone())];
        let mut add = |blob: String| {
            if !blobs.contains(&blob) {
                blobs.push(blob);
            }
        };
        add(self.row.key.clone());
        if let Some(claim_id) = &self.claim_id {
            add(claim_blob_key(&self.row.key, claim_id));
        }
        blobs
    }
}

/// [`find_by_bucket_key`], keeping the row's `claim_id` and `blob_key`.
pub async fn find_stored(
    ctx: &dyn Context,
    bucket: &str,
    key: &str,
) -> Result<Option<StoredRow>, WaferError> {
    let records = db_read::list_bounded(
        ctx,
        TABLE,
        bucket_key_filters(bucket, key),
        Bound::UniqueKey("idx_objects_bucket_key on (bucket, key)"),
    )
    .await?;
    records.first().map(stored_row).transpose()
}

/// Decode a record into a [`StoredRow`].
fn stored_row(rec: &Record) -> Result<StoredRow, WaferError> {
    Ok(StoredRow {
        row: ObjectRow::from_record(rec)?,
        claim_id: rec.opt_str_field("claim_id"),
        blob_key: rec.opt_str_field("blob_key"),
    })
}

/// Filters matching row `id` only while it still carries `claim_id` — the
/// claim it was read with, or `None` for a row no reservation has written
/// since migration 004.
fn still_claimed_by(id: &str, claim_id: Option<&str>) -> Vec<Filter> {
    let claim = match claim_id {
        Some(claim_id) => Filter {
            field: "claim_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(claim_id.to_string()),
        },
        None => Filter {
            field: "claim_id".to_string(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        },
    };
    vec![
        Filter {
            field: "id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(id.to_string()),
        },
        claim,
    ]
}

/// Claim the key's existing row per [`reserve_upload`]'s rules: take over a
/// stored object or an orphaned reservation, refuse an upload in flight — and
/// refuse, the same way, a row another upload claimed since it was read.
async fn claim_existing(
    ctx: &dyn Context,
    existing: StoredRow,
    claim: &PendingClaim<'_>,
) -> Result<Reservation, ReserveError> {
    let superseded_blobs = existing.blobs();
    let StoredRow { row, claim_id, .. } = existing;
    // Before `row`'s fields move into `replaced`.
    let guards = quota_guards(claim, Some(&row));
    let replaced = match row.status {
        ObjectStatus::Complete => Some(ReplacedObject {
            size: row.size,
            content_type: row.content_type,
            uploaded_by: row.uploaded_by,
            uploaded_at: row.uploaded_at,
        }),
        ObjectStatus::Pending if row.uploaded_at < pending_reservation_cutoff() => None,
        ObjectStatus::Pending if row.uploaded_by == claim.uploaded_by => {
            return Err(ReserveError::HeldByOwnEarlierUpload {
                since: row.uploaded_at,
            })
        }
        ObjectStatus::Pending => return Err(ReserveError::Held),
    };
    let mut data = crate::util::json_map(serde_json::json!({
        "size": claim.size,
        "content_type": claim.content_type,
        "status": ObjectStatus::Pending,
        "uploaded_by": claim.uploaded_by,
        "uploaded_at": claim.uploaded_at,
        "updated_at": claim.uploaded_at,
        "claim_id": claim.claim_id,
    }));
    // A replaced object stays the row's blob, and is what readers get, until
    // this upload completes: its bytes are still stored, and a failure puts
    // its row back. An orphan has no object to serve — its bytes, if any
    // landed, belong to an upload that lost the key — so the row stops naming
    // them now.
    if replaced.is_none() {
        data.insert("blob_key".to_string(), serde_json::json!(claim.blob_key()));
    }
    // Conditional on the row still carrying the claim it was read with: two
    // uploads that both read the same `Complete` row (or the same orphan) must
    // not both take it over, or the row ends up describing whichever wrote
    // last while the other's failure puts values back over an upload in
    // flight. Every reservation writes a fresh random `claim_id`, so the first
    // take-over changes it and the second matches nothing — whatever the two
    // clocks said. And conditional on the caps, with this row left out of
    // them: what it held is replaced by what this claim writes.
    let unchanged = still_claimed_by(&row.id, claim_id.as_deref());
    match db::update_guarded(ctx, TABLE, &unchanged, data, &guards).await? {
        UpdateGuardedResponse::Updated { .. } => {}
        // The guards are checked before the claim filter, so a refusal does
        // not say the row was still as read. When another upload has claimed
        // it since, "another upload holds this key" is the answer that tells
        // the user what to do, so one read decides which to give.
        UpdateGuardedResponse::Refused { guard } => {
            let still_as_read = find_stored(ctx, claim.bucket, claim.key)
                .await?
                .is_some_and(|now| now.row.id == row.id && now.claim_id == claim_id);
            return Err(if still_as_read {
                refused_by(guard)
            } else {
                ReserveError::Held
            });
        }
        UpdateGuardedResponse::NoMatch => return Err(ReserveError::Held),
    }
    Ok(Reservation {
        id: row.id,
        bucket: claim.bucket.to_string(),
        key: claim.key.to_string(),
        claim_id: claim.claim_id.to_string(),
        blob_key: claim.blob_key(),
        replaced,
        superseded_blobs,
    })
}

/// What [`insert_reservation`] did.
enum Inserted {
    /// The row was inserted, with this id.
    Created(String),
    /// `(bucket, key)` already has a row; nothing was written.
    KeyTaken,
    /// A quota cap refused the insert; nothing was written.
    Refused(ReserveError),
}

/// Insert the `Pending` row `claim` describes, as a guarded write under
/// [`quota_guards`].
///
/// The id is minted here, so a created row is known by the id it was given.
async fn insert_reservation(
    ctx: &dyn Context,
    claim: &PendingClaim<'_>,
) -> Result<Inserted, WaferError> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::util::now_rfc3339();
    let data = crate::util::json_map(serde_json::json!({
        "id": id,
        "bucket": claim.bucket,
        "key": claim.key,
        "size": claim.size,
        "content_type": claim.content_type,
        "status": ObjectStatus::Pending,
        "uploaded_by": claim.uploaded_by,
        "uploaded_at": claim.uploaded_at,
        "claim_id": claim.claim_id,
        "blob_key": claim.blob_key(),
        "created_at": now,
        "updated_at": now,
    }));
    match db::insert_guarded(ctx, TABLE, data, &quota_guards(claim, None)).await {
        Ok(InsertGuardedResponse::Inserted { .. }) => Ok(Inserted::Created(id)),
        Ok(InsertGuardedResponse::Refused { guard }) => Ok(Inserted::Refused(refused_by(guard))),
        Err(e) if e.code == ErrorCode::AlreadyExists => Ok(Inserted::KeyTaken),
        Err(e) => Err(e),
    }
}

/// The refusal [`release_reservation`] answers when the row no longer carries
/// the reservation's `claim_id`: another upload took the key over after the
/// reservation passed [`PENDING_RESERVATION_TTL_SECONDS`], the uploader's
/// sweep removed it, or the object or its bucket was deleted meanwhile.
/// Whichever it was, the row is not this reservation's to put back or delete.
fn claim_lost() -> WaferError {
    WaferError::new(
        ErrorCode::Aborted,
        "the upload's reservation of this key was taken over",
    )
}

/// Give up a [`Reservation`] whose storage upload failed: delete the row it
/// claimed, or — when it took over the row of an object that is still stored
/// (`put` failed, so the old blob is in place) — put that object's values
/// back, so the blob keeps being described and charged as it was.
///
/// Only while the row still carries this reservation's `claim_id`; otherwise
/// [`ErrorCode::Aborted`] and nothing is written, because the row now belongs
/// to whichever upload took it over.
///
/// Putting a replaced object back is not held to the quota caps: its bytes
/// are still stored, and the row describes them whatever the caps say. It can
/// leave the object's uploader over a cap in one case — another user's
/// upload took the row over, the object's uploader stored more in the room
/// that freed, and the take-over then failed. Their next upload is refused
/// until they are back under it.
pub async fn release_reservation(
    ctx: &dyn Context,
    reservation: &Reservation,
) -> Result<(), WaferError> {
    let mine = still_claimed_by(&reservation.id, Some(&reservation.claim_id));
    let touched = match &reservation.replaced {
        None => db::delete_by_filters_count(ctx, TABLE, mine).await?,
        Some(previous) => {
            let data = crate::util::json_map(serde_json::json!({
                "size": previous.size,
                "content_type": previous.content_type,
                "status": ObjectStatus::Complete,
                "uploaded_by": previous.uploaded_by,
                "uploaded_at": previous.uploaded_at,
            }));
            db::update_by_filters_count(ctx, TABLE, mine, data).await?
        }
    };
    if touched == 0 {
        return Err(claim_lost());
    }
    Ok(())
}

/// How [`mark_complete`] settled a [`Reservation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// The row still carried this reservation and now records the upload.
    Completed,
    /// The key's row belongs to another upload: this reservation outlived
    /// [`PENDING_RESERVATION_TTL_SECONDS`] and was taken over, or the key was
    /// deleted and claimed again since.
    TakenOver,
    /// The key has no row at all: the object or its bucket was deleted while
    /// the upload was in flight (or the uploader's sweep removed the
    /// reservation). Nothing records the bytes the upload stored.
    Deleted,
}

/// Flip a reservation's [`ObjectStatus::Pending`] row to
/// [`ObjectStatus::Complete`] after its storage upload succeeded — the only
/// thing that settles a [`Reservation`] as a stored object — and point it at
/// the reservation's blob. From then on the reservation's
/// [`Reservation::superseded_blobs`] are named by no row.
///
/// Only while the row still carries this reservation's `claim_id`. When it
/// does not, nothing is written, and the key's row is read again to say why:
/// [`Completion::TakenOver`] when another upload holds the key,
/// [`Completion::Deleted`] when nothing does. A database failure is an `Err`,
/// never one of those.
///
/// A row left `pending` is swept within the hour
/// (`quota::sweep_stale_pending`), so an `Err` means the upload is not
/// recorded: the caller reports it rather than answering `uploaded: true`.
pub async fn mark_complete(
    ctx: &dyn Context,
    reservation: &Reservation,
) -> Result<Completion, WaferError> {
    let data = crate::util::json_map(serde_json::json!({
        "status": ObjectStatus::Complete,
        "blob_key": reservation.blob_key,
    }));
    let mine = still_claimed_by(&reservation.id, Some(&reservation.claim_id));
    if db::update_by_filters_count(ctx, TABLE, mine, data).await? > 0 {
        return Ok(Completion::Completed);
    }
    Ok(
        match find_by_bucket_key(ctx, &reservation.bucket, &reservation.key).await? {
            Some(_) => Completion::TakenOver,
            None => Completion::Deleted,
        },
    )
}

/// Delete every object row in `bucket` (bucket-deletion metadata cleanup).
pub async fn delete_for_bucket(ctx: &dyn Context, bucket: &str) -> Result<(), WaferError> {
    db::delete_by_field(
        ctx,
        TABLE,
        "bucket",
        serde_json::Value::String(bucket.to_string()),
    )
    .await
}

/// Delete `stored`'s row only while it is as it was read: the same
/// reservation's `claim_id` and the same status. `false` when it has changed
/// since — taken over, completed or put back — so the blobs the caller
/// deleted from what it read may not be all the row names now, and the
/// caller reads it again.
pub async fn delete_if_unchanged(
    ctx: &dyn Context,
    stored: &StoredRow,
) -> Result<bool, WaferError> {
    let mut unchanged = still_claimed_by(&stored.row.id, stored.claim_id.as_deref());
    unchanged.push(status_is(stored.row.status));
    Ok(db::delete_by_filters_count(ctx, TABLE, unchanged).await? > 0)
}

/// Test helper: delete the object row for `(bucket, key)` outright, as a
/// concurrent request deleting the object does to an upload's row.
#[cfg(test)]
pub async fn delete_by_bucket_key(
    ctx: &dyn Context,
    bucket: &str,
    key: &str,
) -> Result<i64, WaferError> {
    db::delete_by_filters_count(ctx, TABLE, bucket_key_filters(bucket, key)).await
}

/// Up to `limit` of `user_id`'s `pending`-status rows with `uploaded_at`
/// strictly before `cutoff` (an RFC 3339 timestamp, string-compared the same
/// way the column is written), oldest first. `quota::sweep_stale_pending`
/// deletes each with [`delete_if_unchanged`] and then its blobs; see it for
/// the policy.
pub async fn list_stale_pending(
    ctx: &dyn Context,
    user_id: &str,
    cutoff: &str,
    limit: u32,
) -> Result<Vec<StoredRow>, WaferError> {
    let filters = vec![
        Filter {
            field: "uploaded_by".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id.to_string()),
        },
        status_is(ObjectStatus::Pending),
        Filter {
            field: "uploaded_at".to_string(),
            operator: FilterOp::LessThan,
            value: serde_json::Value::String(cutoff.to_string()),
        },
    ];
    let opts = ListOptions {
        filters,
        sort: vec![SortField {
            field: "uploaded_at".to_string(),
            desc: false,
        }],
        limit: Some(limit),
        skip_count: true,
        ..Default::default()
    };
    db::list(ctx, TABLE, &opts)
        .await?
        .records
        .iter()
        .map(stored_row)
        .collect()
}

/// Search `user_id`'s `complete` objects whose key contains `query`
/// (case rules per backend `LIKE`), newest upload first. `query` is
/// LIKE-escaped here ([`escape_like`]) so `%`/`_` match literally.
pub async fn search_completed(
    ctx: &dyn Context,
    user_id: &str,
    query: &str,
    limit: u32,
    offset: i64,
) -> Result<Page<ObjectRow>, WaferError> {
    let opts = ListOptions {
        filters: vec![
            Filter {
                field: "key".to_string(),
                operator: FilterOp::Like,
                value: serde_json::Value::String(format!("%{}%", escape_like(query))),
            },
            // Only show the current user's files
            Filter {
                field: "uploaded_by".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(user_id.to_string()),
            },
            // Exclude pending uploads
            Filter {
                field: "status".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String("complete".to_string()),
            },
        ],
        sort: vec![SortField {
            field: "uploaded_at".to_string(),
            desc: true,
        }],
        limit: Some(limit),
        offset,
        skip_count: false,
        ..Default::default()
    };
    Page::try_decode(db::list(ctx, TABLE, &opts).await?, ObjectRow::from_record)
}

/// One page of the object rows in `bucket` whose key starts with `prefix`
/// (every row when it is empty), sorted by `key` ascending — the objects
/// `GET /b/storage/api/buckets/{name}/objects` lists. The prefix is
/// LIKE-escaped ([`escape_like`]), so `%`/`_` in it match literally; case
/// follows the backend's `LIKE`, as in [`search_completed`] (SQLite and D1
/// fold ASCII case, PostgreSQL does not).
pub async fn list_page_for_bucket(
    ctx: &dyn Context,
    bucket: &str,
    prefix: &str,
    limit: u32,
    offset: i64,
) -> Result<Page<ObjectRow>, WaferError> {
    let mut filters = vec![Filter {
        field: "bucket".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(bucket.to_string()),
    }];
    if !prefix.is_empty() {
        filters.push(Filter {
            field: "key".to_string(),
            operator: FilterOp::Like,
            value: serde_json::Value::String(format!("{}%", escape_like(prefix))),
        });
    }
    let opts = ListOptions {
        filters,
        sort: vec![SortField {
            field: "key".to_string(),
            desc: false,
        }],
        limit: Some(limit),
        offset,
        skip_count: false,
        ..Default::default()
    };
    Page::try_decode(db::list(ctx, TABLE, &opts).await?, ObjectRow::from_record)
}

/// List up to `limit` object rows in `bucket`, sorted by `key` ascending
/// (the SSR object-browser order).
pub async fn list_for_bucket(
    ctx: &dyn Context,
    bucket: &str,
    limit: u32,
) -> Result<Page<ObjectRow>, WaferError> {
    let opts = ListOptions {
        filters: vec![Filter {
            field: "bucket".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(bucket.to_string()),
        }],
        sort: vec![SortField {
            field: "key".to_string(),
            desc: false,
        }],
        limit: Some(limit),
        ..Default::default()
    };
    Page::try_decode(db::list(ctx, TABLE, &opts).await?, ObjectRow::from_record)
}

/// Object counts per bucket for the given bucket names, via a single
/// GROUP BY aggregate (one row per bucket) — avoids an N+1 `db::count` per
/// bucket. Counts ALL rows in each bucket regardless of `uploaded_by` or
/// status, matching the previous per-bucket `db::count` semantics. Buckets
/// with zero objects are simply absent from the returned map.
pub async fn count_by_bucket(
    ctx: &dyn Context,
    bucket_names: &[String],
) -> Result<HashMap<String, i64>, WaferError> {
    let names: Vec<serde_json::Value> = bucket_names
        .iter()
        .map(|s| serde_json::Value::String(s.clone()))
        .collect();
    let req = wire::AggregateRequest {
        collection: TABLE.to_string(),
        select_columns: vec!["bucket".into()],
        aggregates: vec![wire::AggregateColumnDef::Count {
            alias: "cnt".into(),
        }],
        filters: vec![wire::FilterNode::Leaf(wire::FilterDef {
            field: "bucket".into(),
            operator: "in".into(),
            value: serde_json::Value::Array(names),
            column: None,
        })],
        group_by: vec![wire::GroupByDef::Column("bucket".into())],
        sort: vec![],
        limit: 0,
    };
    let rows = db::aggregate(ctx, req).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let bucket = r.data.get("bucket").and_then(|v| v.as_str())?.to_string();
            let cnt = r.i64_field("cnt");
            Some((bucket, cnt))
        })
        .collect())
}

/// Number of `complete` object rows (admin stats).
pub async fn count_completed(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &complete_filter()).await
}

/// `SUM(size)` over `complete` object rows (admin stats).
pub async fn sum_size_completed(ctx: &dyn Context) -> Result<f64, WaferError> {
    db::sum(ctx, TABLE, "size", &complete_filter()).await
}

/// Number of object rows uploaded by `user_id` (quota accounting —
/// includes `pending` reservations).
pub async fn count_for_uploader(ctx: &dyn Context, user_id: &str) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &owned_objects_filter(user_id)).await
}

/// Test helper: number of object rows `user_id` uploaded into `bucket` — the
/// rows [`reserve_upload`]'s file-count guard counts, `pending` reservations
/// included.
#[cfg(test)]
pub async fn count_for_uploader_in_bucket(
    ctx: &dyn Context,
    user_id: &str,
    bucket: &str,
) -> Result<i64, WaferError> {
    let mut filters = owned_objects_filter(user_id);
    filters.push(Filter {
        field: "bucket".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(bucket.to_string()),
    });
    db::count(ctx, TABLE, &filters).await
}

/// `SUM(size)` over the rows uploaded by `user_id` (quota accounting —
/// includes `pending` reservations; no row materialization).
pub async fn sum_size_for_uploader(ctx: &dyn Context, user_id: &str) -> Result<f64, WaferError> {
    db::sum(ctx, TABLE, "size", &owned_objects_filter(user_id)).await
}

/// Test-fixture seeding: insert a raw row map exactly as given (no stamped
/// columns), so tests control the precise row shape.
#[cfg(test)]
pub async fn seed(
    ctx: &dyn Context,
    data: HashMap<String, serde_json::Value>,
) -> Result<ObjectRow, WaferError> {
    ObjectRow::from_record(&db::create(ctx, TABLE, data).await?)
}

/// Test helper: every object row, unfiltered.
#[cfg(test)]
pub async fn list_all(ctx: &dyn Context) -> Result<Vec<ObjectRow>, WaferError> {
    db_read::list_every(ctx, TABLE, vec![])
        .await?
        .iter()
        .map(ObjectRow::from_record)
        .collect()
}

/// Test helper: give the key's row a fresh `claim_id`, as another upload's
/// reservation of it does.
#[cfg(test)]
pub async fn reclaim(ctx: &dyn Context, bucket: &str, key: &str) -> Result<i64, WaferError> {
    db::update_by_filters_count(
        ctx,
        TABLE,
        bucket_key_filters(bucket, key),
        crate::util::json_map(serde_json::json!({ "claim_id": uuid::Uuid::new_v4().to_string() })),
    )
    .await
}

/// Test helper: set the `uploaded_at` of the key's row, to age a reservation.
#[cfg(test)]
pub async fn backdate_upload(
    ctx: &dyn Context,
    bucket: &str,
    key: &str,
    uploaded_at: &str,
) -> Result<i64, WaferError> {
    db::update_by_filters_count(
        ctx,
        TABLE,
        bucket_key_filters(bucket, key),
        crate::util::json_map(serde_json::json!({ "uploaded_at": uploaded_at })),
    )
    .await
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

    fn record(data: &[(&str, serde_json::Value)]) -> Record {
        Record {
            id: "o1".to_string(),
            data: data
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        }
    }

    #[test]
    fn from_record_decodes_the_whole_row() {
        let row = ObjectRow::from_record(&record(&[
            ("bucket", json!("photos")),
            ("key", json!("nested/a.png")),
            ("size", json!(1024)),
            ("content_type", json!("image/png")),
            ("status", json!(ObjectStatus::Complete)),
            ("uploaded_by", json!("alice")),
            ("uploaded_at", json!("2026-05-06T10:00:00Z")),
            ("created_at", json!("2026-05-06T10:00:00Z")),
            ("updated_at", json!("2026-05-06T10:00:01Z")),
        ]))
        .expect("the row decodes");
        assert_eq!(
            row,
            ObjectRow {
                id: "o1".to_string(),
                bucket: "photos".to_string(),
                key: "nested/a.png".to_string(),
                size: 1024,
                content_type: "image/png".to_string(),
                status: ObjectStatus::Complete,
                uploaded_by: "alice".to_string(),
                uploaded_at: "2026-05-06T10:00:00Z".to_string(),
                created_at: "2026-05-06T10:00:00Z".to_string(),
                updated_at: "2026-05-06T10:00:01Z".to_string(),
            }
        );
    }

    /// `size` is `INTEGER` in the schema but a TEXT-typed backend hands it
    /// back as a string. `i64_field` takes both, so a TEXT-stored size still
    /// counts toward the user's quota instead of reading as zero — the same
    /// class of bug as B13's `public`, on the column that decides whether an
    /// upload is admitted.
    #[test]
    fn from_record_reads_a_text_stored_size() {
        assert_eq!(
            ObjectRow::from_record(&record(&[("size", json!("2048"))]))
                .expect("the row decodes")
                .size,
            2048
        );
        assert_eq!(
            ObjectRow::from_record(&record(&[("size", json!(2048))]))
                .expect("the row decodes")
                .size,
            2048
        );
        assert_eq!(
            ObjectRow::from_record(&record(&[]))
                .expect("the row decodes")
                .size,
            0
        );
    }

    /// `status` decides whether an object is a completed upload or an
    /// in-flight reservation — quota counts both, search and the admin stats
    /// count only `complete`. A row holding anything else belongs to neither
    /// set, so it is a decode failure naming the row, not a value the block
    /// carries around and compares against two literals.
    #[test]
    fn a_status_outside_the_set_is_refused_and_names_the_row() {
        for stored in ["complete", "pending"] {
            assert!(ObjectRow::from_record(&record(&[("status", json!(stored))])).is_ok());
        }
        // An unset column is what the DDL's `DEFAULT 'complete'` produces.
        assert_eq!(
            ObjectRow::from_record(&record(&[]))
                .expect("an unset status decodes")
                .status,
            ObjectStatus::Complete
        );

        let err = ObjectRow::from_record(&record(&[("status", json!("half"))]))
            .expect_err("a status outside the set must not decode");
        assert_eq!(err.code, wafer_run::ErrorCode::Internal);
        assert!(err.message.contains("o1"), "{}", err.message);
        assert!(err.message.contains("status"), "{}", err.message);
        assert!(err.message.contains("half"), "{}", err.message);
    }

    /// A claim's blob key keeps the object's directory and file name and puts
    /// the claim in front of the name, so it splits back into one
    /// `(key, claim)` pair and keeps the extension a content type is guessed
    /// from.
    #[test]
    fn a_claims_blob_key_prefixes_the_file_name_with_the_claim() {
        assert_eq!(claim_blob_key("a.png", "c1"), "c1~a.png");
        assert_eq!(
            claim_blob_key("reports/2026/q3.pdf", "c1"),
            "reports/2026/c1~q3.pdf"
        );
    }

    /// A take-over is conditional on the claim the row was read with, not on
    /// a timestamp.
    ///
    /// The condition used to be the row's `updated_at`, and a claim stamps
    /// `updated_at` with its own `uploaded_at` — taken at the START of
    /// `reserve_upload`, on whatever clock the isolate has. A second upload
    /// that read the row before the first one's claim landed could therefore
    /// find the claim's stamp EQUAL to the one it read (same millisecond, or
    /// two clocks that disagree) and take the row too: two uploads in flight
    /// on one row. Pinning the first claim's stamp to the value both read is
    /// that collision, forced.
    #[tokio::test]
    async fn a_second_take_over_from_the_same_read_is_refused_whatever_the_clocks_say() {
        let ctx = crate::test_support::TestContext::with_files().await;
        let read_at = "2026-05-06T10:00:01Z";
        seed(
            &ctx,
            crate::util::json_map(json!({
                "bucket": "assets",
                "key": "same.txt",
                "size": 3,
                "status": ObjectStatus::Complete,
                "uploaded_by": "alice",
                "uploaded_at": "2026-05-06T10:00:00Z",
                "created_at": "2026-05-06T10:00:00Z",
                "updated_at": read_at,
            })),
        )
        .await
        .expect("seed a stored object");
        let first_read = find_stored(&ctx, "assets", "same.txt")
            .await
            .expect("read")
            .expect("the row");
        let second_read = find_stored(&ctx, "assets", "same.txt")
            .await
            .expect("read")
            .expect("the row");

        let quota = QuotaConfig::effective_default();
        let a = PendingClaim {
            bucket: "assets",
            key: "same.txt",
            size: 5,
            content_type: "text/plain",
            uploaded_by: "alice",
            uploaded_at: read_at,
            claim_id: "claim-a",
            quota: &quota,
        };
        claim_existing(&ctx, first_read, &a)
            .await
            .expect("the first take-over claims the row");

        let b = PendingClaim {
            uploaded_at: read_at,
            claim_id: "claim-b",
            size: 9,
            ..a
        };
        let second = claim_existing(&ctx, second_read, &b).await;

        assert!(
            matches!(second, Err(ReserveError::Held)),
            "the row was claimed since it was read: {second:?}"
        );
        let row = find_stored(&ctx, "assets", "same.txt")
            .await
            .expect("read")
            .expect("the row");
        assert_eq!(
            (row.row.size, row.claim_id.as_deref()),
            (5, Some("claim-a")),
            "the row still describes the first claim"
        );
    }

    /// A take-over refused by a cap after another upload claimed the row
    /// since it was read answers `Held` — retry once that upload settles —
    /// not the quota refusal: the guards are checked before the claim filter,
    /// so the refusal alone does not say the row was still as read.
    #[tokio::test]
    async fn a_refused_take_over_of_a_row_claimed_since_it_was_read_is_held() {
        let ctx = crate::test_support::TestContext::with_files().await;
        seed(
            &ctx,
            crate::util::json_map(json!({
                "bucket": "assets",
                "key": "same.txt",
                "size": 3,
                "status": ObjectStatus::Complete,
                "uploaded_by": "alice",
                "uploaded_at": "2026-05-06T10:00:00Z",
            })),
        )
        .await
        .expect("seed a stored object");
        let stale_read = find_stored(&ctx, "assets", "same.txt")
            .await
            .expect("read")
            .expect("the row");
        reserve_upload(
            &ctx,
            "assets",
            "same.txt",
            4,
            "text/plain",
            "carol",
            &QuotaConfig::effective_default(),
        )
        .await
        .expect("carol takes the row over");

        let tiny = QuotaConfig {
            max_storage_bytes: 1,
            ..QuotaConfig::effective_default()
        };
        let bob = PendingClaim {
            bucket: "assets",
            key: "same.txt",
            size: 9,
            content_type: "text/plain",
            uploaded_by: "bob",
            uploaded_at: "2026-05-06T10:00:02Z",
            claim_id: "claim-bob",
            quota: &tiny,
        };
        let refused = claim_existing(&ctx, stale_read, &bob).await;

        assert!(
            matches!(refused, Err(ReserveError::Held)),
            "carol's upload holds the key: {refused:?}"
        );
    }

    /// A reservation settles only the row it still holds.
    ///
    /// A reservation past [`PENDING_RESERVATION_TTL_SECONDS`] is an orphan to
    /// every other upload, which takes the row over. If the first upload then
    /// finishes, its `mark_complete` must not flip the NEW upload's row to
    /// `Complete` while that upload's bytes are still in flight, and its
    /// `release_reservation` must not delete the row out from under it — both
    /// used to act on the row id alone.
    #[tokio::test]
    async fn a_superseded_reservation_neither_completes_nor_releases_the_row() {
        let ctx = crate::test_support::TestContext::with_files().await;
        let alice = reserve_upload(
            &ctx,
            "assets",
            "same.txt",
            5,
            "text/plain",
            "alice",
            &crate::blocks::files::models::QuotaConfig::effective_default(),
        )
        .await
        .expect("alice reserves");
        // Alice's upload stalls past the TTL.
        let stale = (chrono::Utc::now()
            - chrono::Duration::seconds(2 * PENDING_RESERVATION_TTL_SECONDS))
        .to_rfc3339();
        db::update(
            &ctx,
            TABLE,
            &alice.id,
            crate::util::json_map(json!({ "uploaded_at": stale })),
        )
        .await
        .expect("age alice's reservation");
        let bob = reserve_upload(
            &ctx,
            "assets",
            "same.txt",
            9,
            "text/csv",
            "bob",
            &crate::blocks::files::models::QuotaConfig::effective_default(),
        )
        .await
        .expect("bob takes over the orphan");
        assert_eq!(bob.id, alice.id, "one key, one row");

        let completed = mark_complete(&ctx, &alice).await.expect("the row is read");
        let released = release_reservation(&ctx, &alice).await;

        assert_eq!(
            completed,
            Completion::TakenOver,
            "alice no longer holds the row"
        );
        assert_eq!(
            released.map_err(|e| e.code),
            Err(ErrorCode::Aborted),
            "alice no longer holds the row"
        );
        let rows = list_all(&ctx).await.expect("rows");
        assert_eq!(rows.len(), 1, "bob's reservation must not be deleted");
        assert_eq!(
            (rows[0].status, rows[0].uploaded_by.as_str(), rows[0].size),
            (ObjectStatus::Pending, "bob", 9),
            "bob's upload is still in flight and must stay so"
        );

        assert_eq!(
            mark_complete(&ctx, &bob).await.expect("the row is read"),
            Completion::Completed,
            "bob settles his own reservation"
        );
    }

    /// A key held by the uploader's OWN fresh reservation is refused with
    /// what holds it, not as someone else's upload.
    #[tokio::test]
    async fn a_key_held_by_the_uploaders_own_reservation_says_so() {
        let ctx = crate::test_support::TestContext::with_files().await;
        let first = reserve_upload(
            &ctx,
            "assets",
            "same.txt",
            5,
            "text/plain",
            "alice",
            &crate::blocks::files::models::QuotaConfig::effective_default(),
        )
        .await
        .expect("alice reserves");
        let began = find_by_bucket_key(&ctx, "assets", "same.txt")
            .await
            .expect("read")
            .expect("the row")
            .uploaded_at;

        let own = reserve_upload(
            &ctx,
            "assets",
            "same.txt",
            5,
            "text/plain",
            "alice",
            &crate::blocks::files::models::QuotaConfig::effective_default(),
        )
        .await;
        let other = reserve_upload(
            &ctx,
            "assets",
            "same.txt",
            5,
            "text/plain",
            "bob",
            &crate::blocks::files::models::QuotaConfig::effective_default(),
        )
        .await;

        match own {
            Err(ReserveError::HeldByOwnEarlierUpload { since }) => assert_eq!(since, began),
            other => panic!("alice's own reservation holds the key: {other:?}"),
        }
        assert!(
            matches!(other, Err(ReserveError::Held)),
            "bob is told another upload holds it: {other:?}"
        );
        assert_eq!(
            mark_complete(&ctx, &first).await.expect("the row is read"),
            Completion::Completed,
            "neither refusal touched alice's reservation"
        );
    }
}
