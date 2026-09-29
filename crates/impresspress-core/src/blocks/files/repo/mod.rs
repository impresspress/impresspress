//! Row-level data access for the `impresspress/files` block.
//!
//! Mirrors `blocks/auth/repo/`: each submodule owns its table — the
//! `TABLE` const, the row type, its one `from_record`, and every `db::*`
//! statement that touches it live here, so the handler/page modules above
//! never issue database calls and never decode a column. Functions are thin
//! typed wrappers around the pre-existing queries (same filters, same
//! values) and surface the db client's `WaferError` unchanged, so each call
//! site classifies it — through `crud::db_error` / `crud::db_error_internal`
//! (or `crud::db_error_page` for a page), or by warning and using a default.
//!
//! One decode per column is the point, not a tidiness preference. `public`
//! on the buckets table is `INTEGER` on SQLite and `BOOLEAN` on Postgres and
//! is written as a JSON bool by every writer, so it reads back as three
//! different JSON shapes across the backends. When two pages each decoded it
//! their own way, the same bucket was public on one and private on the other
//! (B13). [`buckets::BucketRow::from_record`] decodes it once, through
//! `RecordExt::bool_field`, which is the accessor that accepts all three.
//!
//! Submodule → table map:
//! - [`buckets`] — `impresspress__files__buckets`
//! - [`objects`] — `impresspress__files__objects`
//! - [`views`] — `impresspress__files__views`
//! - [`shares`] — `impresspress__files__cloud_shares` +
//!   `impresspress__files__cloud_access_logs` (the access log is a child
//!   audit table of shares; one submodule owns both)
//! - [`quota`] — `impresspress__files__cloud_quotas`

use wafer_core::clients::database::{Record, RecordList};

pub mod buckets;
pub mod objects;
pub mod quota;
pub mod shares;
pub mod views;

/// One page of typed rows — the block's single return shape for a listing.
///
/// It replaces `RecordList` at the *repo* boundary and nowhere else. It is
/// deliberately NOT `Serialize`: the JSON list endpoints publish the
/// `RecordList` envelope they have always published, and
/// [`super::contracts::RecordListView`] is the one place that envelope is
/// built. Serializing a `Page` directly would silently reshape six response
/// bodies that `packages/impresspress-js` reads.
///
/// `page` and `page_size` ride along unchanged from the database service
/// rather than being recomputed at the boundary: they are what the service
/// derived from the `limit`/`offset` it was given, and the view must publish
/// exactly those.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// The rows on this page, in the query's sort order.
    pub rows: Vec<T>,
    /// Total rows matching the query across all pages.
    pub total: i64,
    /// 1-based index of this page, as the database service computed it.
    pub page: i64,
    /// Rows per page, as the database service computed it.
    pub page_size: i64,
}

impl<T> Page<T> {
    /// Decode a `RecordList` into a page of rows with the table's own
    /// `from_record`. The only place a listing crosses from records to rows.
    pub(super) fn decode(list: RecordList, from_record: fn(&Record) -> T) -> Self {
        Self {
            rows: list.records.iter().map(from_record).collect(),
            total: list.total_count,
            page: list.page,
            page_size: list.page_size,
        }
    }

    /// [`Self::decode`] for a table whose `from_record` can fail.
    ///
    /// `objects` is the one: its `status` is an [`ObjectStatus`], and a row
    /// holding neither `pending` nor `complete` belongs to neither the quota
    /// sum nor the listings — so it stops the page rather than joining it as
    /// something the block cannot classify. The other three row types decode
    /// every column infallibly and keep [`Self::decode`].
    ///
    /// [`ObjectStatus`]: super::contracts::ObjectStatus
    pub(super) fn try_decode(
        list: RecordList,
        from_record: fn(&Record) -> Result<T, wafer_run::WaferError>,
    ) -> Result<Self, wafer_run::WaferError> {
        Ok(Self {
            rows: list
                .records
                .iter()
                .map(from_record)
                .collect::<Result<Vec<T>, _>>()?,
            total: list.total_count,
            page: list.page,
            page_size: list.page_size,
        })
    }
}
