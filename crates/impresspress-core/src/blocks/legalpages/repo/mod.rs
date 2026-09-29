//! Row-level data access for the `impresspress/legalpages` block.
//!
//! Mirrors `blocks/auth/repo/` and `blocks/files/repo/`: the submodule owns
//! its table — the `TABLE` const, the row type, its one `from_record`, and
//! every `db::*` statement that touches it — so `mod.rs`, `service.rs` and
//! `pages.rs` issue no database call and decode no column. The functions
//! surface the client's `WaferError` unchanged, so a handler keeps deciding
//! how a failure becomes a response.
//!
//! The block has one table, so there is one submodule:
//! - [`documents`] — `impresspress__legalpages__documents`
//!
//! What the door buys here is not only tidiness. Five copies of the same
//! `(doc_type, status)` filter block were spread across the three files
//! above, and `status` was written from four of them; that is how a generic
//! PATCH came to be able to publish a document without going through
//! `service::publish_document` (review bug B10). Inside this module `status`
//! is written by exactly two functions, [`documents::mark_published`] and
//! [`documents::mark_archived`], and `service::publish_document` is the only
//! caller of either.

use wafer_core::clients::database::{Record, RecordList};

pub mod documents;

/// One page of typed rows — the block's single return shape for a listing.
///
/// Deliberately not `Serialize`: `GET /b/legalpages/api/documents` publishes
/// the `RecordList` envelope it has always published, and
/// [`super::contracts::DocumentListView`] is the one place that envelope is
/// built.
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
    ///
    /// Fallible because `from_record` is: a row whose `doc_type` or `status`
    /// holds a value the contract does not define stops the listing rather
    /// than reaching a page as something it is not.
    fn decode(
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
