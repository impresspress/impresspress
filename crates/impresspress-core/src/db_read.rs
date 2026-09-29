//! Unpaged reads that cannot truncate in silence.
//!
//! `wafer_core::clients::database::list_all` and `list_sorted` both send
//! `limit: 10_000` and hand back a plain `Vec<Record>`. Nothing in that
//! signature distinguishes "these are all the rows" from "these are the first
//! ten thousand of them", so a call over a table that grows with traffic
//! quietly returns a prefix — and an aggregate, a count or an
//! act-on-every-row loop built on that prefix is wrong with no symptom.
//!
//! This module replaces both of them. Every read here states, in its type,
//! what happens when the row set is larger than the caller assumed:
//!
//! | call | contract |
//! |---|---|
//! | [`list_bounded`] / [`list_bounded_sorted`] | the caller names a [`Bound`] that makes a large result impossible; exceeding it is a data-integrity fault and returns `Err` |
//! | [`list_capped`] / [`list_capped_sorted`] | the caller gets [`Capped::truncated`] and must show it |
//! | [`list_every`] / [`page_after`] | every matching row, by keyset pagination — no cap at all |
//!
//! The ceiling lives here rather than upstream: these helpers ask the
//! database for `limit + 1` rows and treat the extra row as the overflow
//! signal, so the answer does not depend on a cap defined in `wafer-core`
//! and never moves when that cap does.
//!
//! `tests/db_read_guard.rs` fails the build if `db::list_all` or
//! `db::list_sorted` reappears anywhere under `src/`.

use std::fmt;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, ErrorCode, WaferError};

/// Row ceiling for the one-shot reads ([`list_bounded`], [`list_capped`]).
///
/// Matches the limit `wafer-core`'s `list_all` used, so no call site's
/// behaviour changes below the ceiling; above it, this module reports rather
/// than truncates.
pub const UNPAGED_LIMIT: u32 = 10_000;

/// Rows per round-trip for the exhaustive reads ([`list_every`],
/// [`page_after`]).
pub const KEYSET_PAGE: u32 = 1_000;

/// Why a one-shot read of `collection` cannot return a large result.
///
/// Each variant carries the concrete reason, which is what the error message
/// quotes when the read overflows anyway: the operator reading that log needs
/// to know which assumption broke, not that "a limit was hit".
#[derive(Debug, Clone, Copy)]
pub enum Bound {
    /// The filters pin a `UNIQUE` / `PRIMARY KEY` column or composite, so the
    /// database itself permits at most one match. The string names the
    /// constraint.
    UniqueKey(&'static str),
    /// One row per member of a set that is itself far below the ceiling — one
    /// per registered block, per OAuth provider a user linked, per offer on a
    /// product. The string says which set, and why it is small.
    OnePer(&'static str),
    /// Rows exist only because an operator or a migration created them: a
    /// permission catalogue, a provider list, a config-key table. Traffic
    /// cannot add to them. The string says who writes the table.
    Curated(&'static str),
}

impl fmt::Display for Bound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UniqueKey(reason) => write!(f, "unique key: {reason}"),
            Self::OnePer(reason) => write!(f, "one row per {reason}"),
            Self::Curated(reason) => write!(f, "curated: {reason}"),
        }
    }
}

/// A one-shot read that stopped at [`UNPAGED_LIMIT`], and said so.
///
/// `truncated` is the signal a display surface owes its reader: a list that
/// hit the ceiling is a prefix, and rendering it as if it were the whole set
/// misstates what exists.
#[derive(Debug, Clone)]
pub struct Capped {
    /// The rows read, at most [`UNPAGED_LIMIT`] of them.
    pub rows: Vec<Record>,
    /// Whether more rows match than were returned.
    pub truncated: bool,
}

impl Capped {
    /// Project the rows, keeping the truncation flag.
    pub fn map<T>(self, f: impl FnMut(Record) -> T) -> CappedList<T> {
        CappedList {
            rows: self.rows.into_iter().map(f).collect(),
            truncated: self.truncated,
        }
    }

    /// Project the rows through a fallible decode, keeping the flag.
    pub fn try_map<T, E>(self, f: impl FnMut(Record) -> Result<T, E>) -> Result<CappedList<T>, E> {
        let truncated = self.truncated;
        Ok(CappedList {
            rows: self
                .rows
                .into_iter()
                .map(f)
                .collect::<Result<Vec<T>, E>>()?,
            truncated,
        })
    }
}

/// [`Capped`] after decoding — a list of `T` that knows whether it is whole.
#[derive(Debug, Clone)]
pub struct CappedList<T> {
    /// The decoded rows.
    pub rows: Vec<T>,
    /// Whether more rows match than are present in `rows`.
    pub truncated: bool,
}

impl<T> CappedList<T> {
    /// Project each row, keeping the truncation flag.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> CappedList<U> {
        CappedList {
            rows: self.rows.into_iter().map(f).collect(),
            truncated: self.truncated,
        }
    }

    /// An empty, untruncated list — the "read failed, show nothing" fallback
    /// that display call sites use in place of `Vec::default()`.
    pub fn empty() -> Self {
        Self {
            rows: Vec::new(),
            truncated: false,
        }
    }
}

impl<T> Default for CappedList<T> {
    fn default() -> Self {
        Self::empty()
    }
}

/// Ask for one row more than `limit`, and report whether it came back.
async fn over_read(
    ctx: &dyn Context,
    collection: &str,
    filters: Vec<Filter>,
    sort: Vec<SortField>,
    limit: u32,
) -> Result<Capped, WaferError> {
    let result = db::list(
        ctx,
        collection,
        &ListOptions {
            filters,
            sort,
            // The extra row is the signal. `skip_count` keeps the backend off
            // the `SELECT COUNT(*)` round-trip that no caller here reads.
            limit: Some(limit.saturating_add(1)),
            skip_count: true,
            ..Default::default()
        },
    )
    .await?;
    let mut rows = result.records;
    let truncated = rows.len() > limit as usize;
    if truncated {
        rows.truncate(limit as usize);
    }
    Ok(Capped { rows, truncated })
}

fn bound_broken(collection: &str, bound: Bound) -> WaferError {
    WaferError::new(
        ErrorCode::Internal,
        format!(
            "bounded read of {collection} returned more than {UNPAGED_LIMIT} rows, \
             so its stated bound no longer holds ({bound}); this read has to become \
             a paginated or aggregated one"
        ),
    )
}

/// Every row matching `filters`, for a `collection` whose matching set cannot
/// be large — `bound` says why.
///
/// Returns `Err(Internal)` if the read overflows [`UNPAGED_LIMIT`] anyway:
/// at that point the stated bound is false, and every figure the caller was
/// about to derive from a prefix would be wrong. Failing loudly is the only
/// answer that does not publish a wrong number.
pub async fn list_bounded(
    ctx: &dyn Context,
    collection: &str,
    filters: Vec<Filter>,
    bound: Bound,
) -> Result<Vec<Record>, WaferError> {
    let capped = over_read(ctx, collection, filters, Vec::new(), UNPAGED_LIMIT).await?;
    if capped.truncated {
        return Err(bound_broken(collection, bound));
    }
    Ok(capped.rows)
}

/// [`list_bounded`] with an `ORDER BY`.
pub async fn list_bounded_sorted(
    ctx: &dyn Context,
    collection: &str,
    filters: Vec<Filter>,
    sort: Vec<SortField>,
    bound: Bound,
) -> Result<Vec<Record>, WaferError> {
    let capped = over_read(ctx, collection, filters, sort, UNPAGED_LIMIT).await?;
    if capped.truncated {
        return Err(bound_broken(collection, bound));
    }
    Ok(capped.rows)
}

/// The first [`UNPAGED_LIMIT`] rows matching `filters`, plus whether there
/// are more.
///
/// For display surfaces over a table that grows with traffic. The caller must
/// surface [`Capped::truncated`] to whoever reads the list — that is the
/// whole difference between this and the silent read it replaces.
pub async fn list_capped(
    ctx: &dyn Context,
    collection: &str,
    filters: Vec<Filter>,
) -> Result<Capped, WaferError> {
    over_read(ctx, collection, filters, Vec::new(), UNPAGED_LIMIT).await
}

/// [`list_capped`] with an `ORDER BY`.
pub async fn list_capped_sorted(
    ctx: &dyn Context,
    collection: &str,
    filters: Vec<Filter>,
    sort: Vec<SortField>,
) -> Result<Capped, WaferError> {
    over_read(ctx, collection, filters, sort, UNPAGED_LIMIT).await
}

/// One keyset page: up to `KEYSET_PAGE` rows matching `filters` whose `id`
/// sorts after `after_id`, in ascending `id` order.
///
/// Keyset, not `OFFSET`: `id > after_id ORDER BY id` walks the table exactly
/// once. An `OFFSET` walk over the same data would re-skip the rows it
/// already read on every page, and would silently repeat or drop rows when a
/// concurrent insert or delete shifts the offsets under it.
///
/// **Precondition:** `collection` must give every row a distinct, non-empty
/// `id`. That is not true of every table this crate reads — `auth.sessions`
/// is keyed on `family`, `signal.rooms` on `code`, and
/// `personal_access_tokens`, `oauth_pkce_states` and `jwt_blocklist` carry a
/// *nullable* `id` added by a later migration — so it is a condition on the
/// caller, not a property of the schema. A row whose `id` is empty cannot be
/// a cursor, and walking past it would silently drop every row after it;
/// [`walk_needs_an_id`] makes that an error instead.
///
/// `columns` narrows the projection when the caller only needs a few fields;
/// `None` reads the whole row. Whatever `columns` says, `id` is always
/// requested: it is the cursor.
pub async fn page_after(
    ctx: &dyn Context,
    collection: &str,
    filters: Vec<Filter>,
    columns: Option<Vec<String>>,
    after_id: Option<&str>,
) -> Result<Vec<Record>, WaferError> {
    let mut filters = filters;
    if let Some(after) = after_id {
        filters.push(Filter {
            field: "id".to_string(),
            operator: FilterOp::GreaterThan,
            value: serde_json::Value::String(after.to_string()),
        });
    }
    let columns = columns.map(|mut named| {
        if !named.iter().any(|column| column == "id") {
            named.push("id".to_string());
        }
        named
    });
    let result = db::list(
        ctx,
        collection,
        &ListOptions {
            filters,
            sort: vec![SortField {
                field: "id".to_string(),
                desc: false,
            }],
            limit: Some(KEYSET_PAGE),
            skip_count: true,
            columns,
            ..Default::default()
        },
    )
    .await?;
    for record in &result.records {
        if record.id.is_empty() {
            return Err(walk_needs_an_id(collection));
        }
    }
    Ok(result.records)
}

/// A keyset walk over a table that does not give every row an `id`.
///
/// Loud rather than latent: the cursor is the `id`, so a blank one either
/// stops the walk early or repeats a page forever. Either way the caller —
/// a fraud control, a rename cascade, an export restored over live tables —
/// would act on the wrong set, which is the exact failure this module exists
/// to stop.
fn walk_needs_an_id(collection: &str) -> WaferError {
    WaferError::new(
        ErrorCode::Internal,
        format!(
            "keyset walk of {collection} met a row with no id; this read pages on \
             `id`, so the table must give every row a distinct non-empty one"
        ),
    )
}

/// EVERY row matching `filters`, in ascending `id` order, however many there
/// are — [`page_after`] driven to exhaustion, and subject to its `id`
/// precondition.
///
/// The read is unbounded by design, so it belongs only where "all of them" is
/// the actual requirement: a compensating write over every row a suspended
/// seller owns, a cascade that must touch every grant of a renamed role, an
/// export that has to round-trip the whole table. A display list wants
/// [`list_capped`]; a total wants an aggregate.
///
/// It holds every matching row in memory, and the caller pays one round-trip
/// per `KEYSET_PAGE` rows. On a memory- or subrequest-limited runtime that is
/// a real ceiling — it is the price of being exact, and it is the reason this
/// is the narrowest of the three shapes here.
pub async fn list_every(
    ctx: &dyn Context,
    collection: &str,
    filters: Vec<Filter>,
) -> Result<Vec<Record>, WaferError> {
    let mut all: Vec<Record> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = page_after(ctx, collection, filters.clone(), None, cursor.as_deref()).await?;
        let short = page.len() < KEYSET_PAGE as usize;
        cursor = page.last().map(|row| row.id.clone());
        all.extend(page);
        if short || cursor.is_none() {
            return Ok(all);
        }
    }
}
