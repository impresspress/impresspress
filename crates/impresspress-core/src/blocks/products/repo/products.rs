//! Row-level access over `impresspress__products__products`.
//!
//! This module is the sole place that issues `db::*` against the products
//! table, per the `repo`-module-owns-its-`TABLE` convention documented in
//! `repo/mod.rs`. Every read here carries the soft-delete filter, which is
//! why routing reads through it is a correctness requirement and not tidying:
//! the products table has carried `deleted_at` since migration 001 and the
//! partial unique slug index in 005 is defined on `deleted_at IS NULL`, but
//! before this module nothing ever wrote the column and the public catalog
//! filtered on `status` alone.
//!
//! Every function here returns `Result<_, WaferError>` — no `OutputStream`,
//! no `err_internal`/`err_not_found`. HTTP-response construction (and every
//! other call-site policy: authz, logging, Stripe-retry) stays at the call
//! site, per the convention documented just above `mod products;` in
//! `repo/mod.rs`.

use std::collections::HashMap;

use serde_json::Value;
use wafer_block::db::{Filter, FilterOp, SortField};
// `snapshot_upsert`'s only import, and it is `block-dev`-gated with it:
// nothing else in this module upserts.
#[cfg(feature = "block-dev")]
use wafer_block::wire::database::{BatchWrite, OnConflict, UpsertRequest};
use wafer_core::clients::database::{self as db, Record, RecordList};
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    blocks::products::contracts::{ApprovalStatus, ProductStatus},
    db_read::{self, Capped},
    util::RecordExt,
};

pub(crate) const TABLE: &str = "impresspress__products__products";

// Column invariant — `deleted_at` holds exactly two kinds of value:
//
//   * SQL NULL         — the product is live;
//   * an RFC3339 stamp — the instant it was soft-deleted.
//
// It is never the empty string, and never any other non-timestamp text. The
// only writers are `soft_delete` (a fresh `now_rfc3339()`) and `restore`
// (`Value::Null`); every handler that forwards a caller-supplied body refuses
// a body naming the field (`handlers::product::UNSETTABLE_FIELDS`), which
// `no_handler_path_can_write_an_empty_deleted_at` in
// `tests/seller_governance_tests.rs` pins end-to-end across all four
// create/update handlers. The invariant matters because `''` would otherwise be a
// third state: SQL (and so `live_filter`, `list_deleted`, and migration
// 005's partial unique slug index) reads it as deleted, while any
// string-emptiness check reads it as live.
/// `deleted_at IS NULL` — the predicate that distinguishes a live product
/// from a soft-deleted one.
pub(crate) fn live_filter() -> Filter {
    Filter {
        field: "deleted_at".to_string(),
        operator: FilterOp::IsNull,
        value: Value::Null,
    }
}

/// `deleted_at IS NOT NULL` — [`live_filter`]'s exact complement.
fn deleted_filter() -> Filter {
    Filter {
        field: "deleted_at".to_string(),
        operator: FilterOp::IsNotNull,
        value: Value::Null,
    }
}

/// Fetch one live product. A soft-deleted row answers `NotFound`, so callers
/// need no extra check and cannot forget one.
pub(crate) async fn get(ctx: &dyn Context, id: &str) -> Result<Record, WaferError> {
    let record = db::get(ctx, TABLE, id).await?;
    if is_deleted(&record) {
        return Err(WaferError::new(ErrorCode::NotFound, "Product not found"));
    }
    Ok(record)
}

/// List one page of live products. `filters` narrows the live set; it cannot
/// widen it. `sort` defaults to newest-first by `created_at` when `None`,
/// matching `blocks::crud::crud_list`.
pub(crate) async fn list_page(
    ctx: &dyn Context,
    page: i64,
    page_size: i64,
    mut filters: Vec<Filter>,
    sort: Option<Vec<SortField>>,
) -> Result<RecordList, WaferError> {
    filters.push(live_filter());
    let sort = sort.unwrap_or_else(|| {
        vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }]
    });
    db::paginated_list(ctx, TABLE, page, page_size, filters, sort).await
}

/// Fetch one soft-deleted product. A live (or missing) row answers
/// `NotFound` — the mirror image of [`get`].
///
/// Shares [`list_deleted`]'s named exception to this module's live-only
/// rule, and exists for the same reason: `restore` re-claims the row's slug
/// against the partial unique index from migration 005, so when that write
/// fails its caller has to be able to read the slug of a row that `get`
/// cannot see, in order to say which slug collided instead of surfacing the
/// index violation as a 500. A failed restore leaves the row deleted, which
/// is exactly why this read still finds it.
pub(crate) async fn get_deleted(ctx: &dyn Context, id: &str) -> Result<Record, WaferError> {
    let record = db::get(ctx, TABLE, id).await?;
    if !is_deleted(&record) {
        return Err(WaferError::new(ErrorCode::NotFound, "Product not deleted"));
    }
    Ok(record)
}

/// List one page of soft-deleted products. `filters` narrows the deleted
/// set; it cannot widen it — same append-only contract as `list_page`.
///
/// This is the one deliberate, named exception to this module's live-only
/// rule: every other read here is unreachable for a soft-deleted row by
/// design, which is exactly why an admin needs one door that finds them —
/// otherwise a deleted product could never be located in order to restore
/// it, making soft delete permanent in practice.
pub(crate) async fn list_deleted(
    ctx: &dyn Context,
    page: i64,
    page_size: i64,
    mut filters: Vec<Filter>,
    sort: Option<Vec<SortField>>,
) -> Result<RecordList, WaferError> {
    filters.push(deleted_filter());
    let sort = sort.unwrap_or_else(|| {
        vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }]
    });
    db::paginated_list(ctx, TABLE, page, page_size, filters, sort).await
}

/// Count live products matching `filters`.
pub(crate) async fn count(ctx: &dyn Context, filters: &[Filter]) -> Result<i64, WaferError> {
    let mut all = filters.to_vec();
    all.push(live_filter());
    db::count(ctx, TABLE, &all).await
}

/// EVERY live product matching `filters`, however many there are. `filters`
/// narrows the live set; it cannot widen it — same contract as `list_page`.
///
/// Exhaustive, for the one caller that has to round-trip the whole catalog:
/// the dev site export, whose bundle is restored over the live tables. A
/// listing that merely *shows* products wants [`list_capped_live`]; a figure
/// derived from the set wants [`count`] or an aggregate.
#[cfg(feature = "block-dev")]
pub(crate) async fn list_every_live(
    ctx: &dyn Context,
    mut filters: Vec<Filter>,
) -> Result<Vec<Record>, WaferError> {
    filters.push(live_filter());
    db_read::list_every(ctx, TABLE, filters).await
}

/// Live products matching `filters` for a listing, and whether more match
/// than were returned.
///
/// Nothing bounds a seller's catalog —
/// `IMPRESSPRESS__PRODUCTS__SELLER_MAX_PRODUCTS` defaults to unlimited — so a
/// listing has to be able to say it is showing a prefix.
pub(crate) async fn list_capped_live(
    ctx: &dyn Context,
    mut filters: Vec<Filter>,
) -> Result<Capped, WaferError> {
    filters.push(live_filter());
    db_read::list_capped(ctx, TABLE, filters).await
}

/// Live-product counts per owner id, as one `GROUP BY` rather than a scan of
/// every seller product on the platform.
///
/// One row per owner that still has a live product, so the result is the
/// size of the seller base and not of the catalog.
pub(crate) async fn live_counts_by_owner(
    ctx: &dyn Context,
    owner_kind: &str,
) -> Result<HashMap<String, i64>, WaferError> {
    use wafer_block::wire::database as wire;

    let filters = vec![
        Filter {
            field: "owner_kind".to_string(),
            operator: FilterOp::Equal,
            value: Value::String(owner_kind.to_string()),
        },
        live_filter(),
    ];
    let req = wire::AggregateRequest {
        collection: TABLE.to_string(),
        select_columns: vec!["owner_id".into()],
        aggregates: vec![wire::AggregateColumnDef::Count {
            alias: "products".into(),
        }],
        filters: crate::util::to_wire_filters(&filters),
        group_by: vec![wire::GroupByDef::Column("owner_id".into())],
        sort: vec![],
        limit: 0,
    };
    db::aggregate(ctx, req)
        .await?
        .iter()
        .map(|row| {
            Ok((
                row.str_field("owner_id").to_string(),
                // Through `aggregate_i64`, like every other aggregate read in
                // this block: a count that is not a JSON integer is a decode
                // fault, not the 0 `i64_field` would read it as.
                crate::util::aggregate_i64(row, "products")?,
            ))
        })
        .collect()
}

/// `owner_kind = 'user' AND status = pending_review AND approval_status =
/// pending` — the moderation queue's own predicate, named once so the page
/// that lists it and the count beside it cannot drift apart.
fn pending_review_filters() -> Vec<Filter> {
    vec![
        Filter {
            field: "owner_kind".to_string(),
            operator: FilterOp::Equal,
            value: Value::String("user".to_string()),
        },
        Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(ProductStatus::PendingReview),
        },
        Filter {
            field: "approval_status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(ApprovalStatus::Pending),
        },
    ]
}

/// The moderation queue: live seller listings waiting for a decision.
///
/// The predicate is in the query, not in a Rust filter over every seller
/// product on the platform — the queue is the small set, the catalog is not.
pub(crate) async fn list_pending_review(ctx: &dyn Context) -> Result<Capped, WaferError> {
    list_capped_live(ctx, pending_review_filters()).await
}

/// How many listings are waiting for a moderation decision.
pub(crate) async fn count_pending_review(ctx: &dyn Context) -> Result<i64, WaferError> {
    count(ctx, &pending_review_filters()).await
}

/// Every LIVE product `user_id` owns — a seller's catalog.
///
/// The `owner_id` filter was written out three times (the admin seller
/// detail endpoint, the admin seller detail page, and `sellers.rs`'s private
/// `owned_by`); it is one query with one name now. A deleted listing is not
/// part of a seller's catalog and does not belong in a catalog view, which is
/// why this goes through [`list_capped_live`] rather than
/// [`list_all_including_deleted`] — suspension, which does want every row the
/// seller owns, keeps its own read.
pub(crate) async fn list_owned_by(ctx: &dyn Context, user_id: &str) -> Result<Capped, WaferError> {
    list_capped_live(ctx, vec![owner_filter(user_id)]).await
}

/// Every product `user_id` owns, soft-deleted ones included — what
/// suspension and reactivation act on. See [`list_all_including_deleted`] for
/// why that set is deliberately different from [`list_owned_by`]'s.
pub(crate) async fn list_owned_by_including_deleted(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<Vec<Record>, WaferError> {
    list_all_including_deleted(ctx, vec![owner_filter(user_id)]).await
}

fn owner_filter(user_id: &str) -> Filter {
    Filter {
        field: "owner_id".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id.to_string()),
    }
}

/// List every product matching `filters` *regardless of soft-delete state*,
/// unpaged.
///
/// The second deliberate, named exception to this module's live-only rule,
/// alongside [`list_deleted`]/[`get_deleted`] — and the only one that spans
/// both sets at once.
///
/// It exists for seller suspension. Suspending a seller is a lifecycle and
/// fraud operation, so it must cover every row that seller owns: soft delete
/// touches nothing in Stripe, so a deleted product's Prices and Payment Links
/// are still live in the connected account and still take money. Reading that
/// set through [`list_all`] silently exempted exactly the deleted rows from
/// the archival guarantee, which is the opposite of what a fraud control is
/// for.
///
/// Not a general-purpose escape hatch: a read that merely *displays* products
/// wants [`list_capped_live`]. Use this only where "every row the owner has" is the
/// actual requirement.
///
/// Exhaustive, and it has to be: the seller product cap is a config value
/// that defaults to unlimited, so nothing bounds a seller's catalog. A row
/// this read stopped short of is a Stripe Price and Payment Link that the
/// suspension never archives and that goes on taking money.
pub(crate) async fn list_all_including_deleted(
    ctx: &dyn Context,
    filters: Vec<Filter>,
) -> Result<Vec<Record>, WaferError> {
    db_read::list_every(ctx, TABLE, filters).await
}

/// Fetch one product regardless of soft-delete state.
///
/// Third and last named exception on the read side, for the same reason as
/// [`list_all_including_deleted`] and reached from it: archiving a product's
/// Stripe catalog needs the row's `owner_kind`/`owner_id` (to address the
/// connected account) and its `stripe_product_id`, and has to keep working
/// once the product is soft-deleted — that is precisely when its catalog most
/// needs taking down. The read cannot leak a deleted product to anyone,
/// because archival only ever *removes* things from the live Stripe catalog.
///
/// The webhook that reconciles a paid Payment Link session shares it, for the
/// mirror-image reason: soft delete touches nothing in Stripe, so a deleted
/// product's Payment Links stay payable and a customer can still be charged
/// through one. Reading the product live-only there meant the reconciliation
/// answered `NotFound`, the delivery failed, and Stripe retried it forever —
/// money captured with no purchase row, no line items, and an order-status
/// page that never resolves. The row is read for the buyer's own receipt, so
/// again nothing about a deleted product reaches a third party.
///
/// Not for handlers. A read that decides whether a caller may see or edit a
/// product wants [`get`], which answers `NotFound` for a deleted row.
pub(crate) async fn get_including_deleted(
    ctx: &dyn Context,
    id: &str,
) -> Result<Record, WaferError> {
    db::get(ctx, TABLE, id).await
}

// `created_at`/`updated_at` are not stamped here: the database service's
// `DbExec::create`/`update` default impl (shared by every backend) already
// fills in whichever of the two the caller didn't supply — see
// `create_stamps_created_and_updated_at` / `update_stamps_a_new_updated_at`
// below, which pin that behaviour from this module's side of the call.
pub(crate) async fn create(
    ctx: &dyn Context,
    data: HashMap<String, Value>,
) -> Result<Record, WaferError> {
    db::create(ctx, TABLE, data).await
}

/// Update one product *regardless of its soft-delete state*.
///
/// The write-side twin of [`get_including_deleted`], and the only unfiltered
/// write in this module. Every handler-reachable update wants
/// [`update_live`]: an update that lands on an already-deleted row and
/// reports success is the bug `update_live` exists to prevent, and nothing in
/// the type system distinguishes a deliberate unfiltered write from a
/// forgotten filter — so the two are distinguished by name instead, and
/// `write_side_escape_hatches_are_allowlisted` in `tests/repo_door_test.rs`
/// fails the build for any new call site that is not justified there.
///
/// The two deliberate callers both write a fact that stays true after a
/// delete: seller suspension (`handlers::sellers::set_suspended`), which must
/// cover every row the seller owns because soft delete touches nothing in
/// Stripe; and the `stripe_product_id` write-back in `stripe.rs`, which
/// records a Stripe Product that has already been created — refusing to
/// record it because the product was deleted mid-sync would leave the Stripe
/// object orphaned with nothing pointing at it.
pub(crate) async fn update_including_deleted(
    ctx: &dyn Context,
    id: &str,
    data: HashMap<String, Value>,
) -> Result<Record, WaferError> {
    reject_id_rewrite(&data)?;
    db::update(ctx, TABLE, id, data).await
}

// A product's id is its primary key and every other table's foreign key
// (`line_items`, `offers`, `product_versions`, `entitlements` all carry a
// `product_id` that is `TEXT NOT NULL`). An update that sets it rewrites the
// key and orphans all of them — and does so invisibly, because a filtered
// update's `WHERE id = ?` still matches exactly one row, so the affected-row
// guard passes while the by-id re-read afterwards looks up an id that no
// longer exists and reports `NotFound`. The caller is told the write failed
// while the catalog has already been rewritten.
//
// The guard lives here rather than only in the handlers because this module
// is the single door: `handlers::product::UNSETTABLE_FIELDS` gives a caller a
// clear 400, but it only covers the four request bodies that exist today.
fn reject_id_rewrite(data: &HashMap<String, Value>) -> Result<(), WaferError> {
    if data.contains_key("id") {
        return Err(WaferError::new(
            ErrorCode::InvalidArgument,
            "a product's id is immutable",
        ));
    }
    Ok(())
}

/// Selects exactly one row by id. Paired with [`live_filter`] to make a
/// by-id write conditional on the row still being live.
fn id_filter(id: &str) -> Filter {
    Filter {
        field: "id".to_string(),
        operator: FilterOp::Equal,
        value: Value::String(id.to_string()),
    }
}

/// Update one product *if it is still live*, in a single round trip.
///
/// The default write, and what every handler-reachable update wants.
/// [`update_including_deleted`] is unconditional, so a caller wanting "only
/// while it is live" would have to [`get`] first — and the gap between that
/// read and the write is a window a concurrent delete fits through, after
/// which the write lands on an already-deleted row and reports success.
/// Making `deleted_at IS NULL` part of the write's own `WHERE` closes it: the
/// row's state at write time is what decides, and zero rows affected is
/// `NotFound`.
pub(crate) async fn update_live(
    ctx: &dyn Context,
    id: &str,
    data: HashMap<String, Value>,
) -> Result<Record, WaferError> {
    reject_id_rewrite(&data)?;
    let updated =
        db::update_by_filters_count(ctx, TABLE, vec![id_filter(id), live_filter()], data).await?;
    if updated == 0 {
        return Err(WaferError::new(ErrorCode::NotFound, "Product not found"));
    }
    // Re-read for the response body: a filtered update reports how many rows
    // it touched, not what they now hold. Raw rather than `get`, because the
    // write has already established that the row was live when it happened —
    // answering `NotFound` here for a delete that landed afterwards would
    // deny a change that did take effect.
    //
    // The re-read is by the SAME id the `WHERE` selected, which is only a
    // faithful read-back because `reject_id_rewrite` above has already
    // established that the write cannot have moved the row's identity.
    // Without that, a `data` carrying `id` produced the worst possible
    // answer: the write landed, the affected-row guard passed, and this read
    // missed — reporting `NotFound` for a change that had taken effect.
    db::get(ctx, TABLE, id).await
}

/// Hard-delete a row. Reserved for rolling back a product that failed
/// mid-creation and was never visible to anyone — see the cleanup path in
/// `handlers/product.rs`. Not the delete a user's action reaches.
pub(crate) async fn purge(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, TABLE, id).await
}

/// Soft-delete a product: stamp `deleted_at` and leave the row in place.
///
/// The row stays because several tables carry a `product_id` column that is
/// `TEXT NOT NULL` with no default — `line_items`, `offers`,
/// `product_versions`, and `entitlements` among them — so a hard delete
/// would orphan every one of them, most visibly a completed order's line
/// item. Stamping also frees the product's slug, since the unique index
/// from migration 005 is partial on `deleted_at IS NULL`.
///
/// A filtered write for the same reason as [`update_live`], and not a `get`
/// followed by an unconditional `update`: between those two statements a
/// concurrent `restore` fits, and the unconditional write then stamps
/// `deleted_at` back on top of a row the restore had just brought live —
/// answering 200 to both callers, one of whom was told their restore
/// succeeded. Two concurrent deletes race the same way, the second rewriting
/// the first's stamp and moving the row in the `deleted_at desc` view.
///
/// Deleting an already soft-deleted (or missing) row matches zero rows and
/// answers `NotFound`, matching every other read in this module: a caller
/// can't distinguish "double delete" from "never existed" any more than they
/// can for `get` itself.
pub(crate) async fn soft_delete(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    let data = HashMap::from([(
        "deleted_at".to_string(),
        Value::String(crate::util::now_rfc3339()),
    )]);
    let updated =
        db::update_by_filters_count(ctx, TABLE, vec![id_filter(id), live_filter()], data).await?;
    if updated == 0 {
        return Err(WaferError::new(ErrorCode::NotFound, "Product not found"));
    }
    Ok(())
}

/// Clear `deleted_at`, bringing a soft-deleted product back.
///
/// A filtered update rather than `get` + `update`: `get` refuses to find a
/// soft-deleted row by design, but restoring one is the one operation in this
/// module that must act on exactly that row.
///
/// The filter is `deleted_at IS NOT NULL`, so restoring a product that was
/// never deleted matches zero rows and issues no write at all.
/// `DbExec::update` stamps a fresh `updated_at` on every write, and the admin
/// product list sorts on the timestamps — an operation that changed nothing
/// must not reorder it.
pub(crate) async fn restore(ctx: &dyn Context, id: &str) -> Result<Record, WaferError> {
    let data = HashMap::from([("deleted_at".to_string(), Value::Null)]);
    // Zero rows affected means either "already live" (a no-op) or "no such
    // product". `db::get` below tells those apart, answering `NotFound` only
    // for the second — the same two responses this function has always given.
    db::update_by_filters_count(ctx, TABLE, vec![id_filter(id), deleted_filter()], data).await?;
    db::get(ctx, TABLE, id).await
}

/// The `db::batch` write that inserts-or-overwrites one row exactly as
/// `blocks::dev::data_snapshot`'s import found it — `deleted_at` included,
/// whatever the exported row said. Built here, on this module's own table
/// name, and handed back for the import to send in its batch.
///
/// Reserved for that one caller. Every other write above acts on a single
/// product by id and respects (or, named and justified, deliberately
/// bypasses per this module's door tests) the row's *current* soft-delete
/// state; this one restores a row wholesale from a trusted export, which is
/// a different operation from all of them and is not exposed more generally
/// — `data`/`conflict`/`update_columns` come from the snapshot row and the
/// allowlist entry verbatim, so this function does not itself decide what
/// "wholesale" means — and it is `block-dev`-gated because that caller is the
/// sandbox's, absent from every default build.
///
/// `conflict` is passed in rather than hardcoded here. The allowlist already
/// declares this table's conflict target (`Mode::Upsert(BY_ID)`) and
/// `data_snapshot::upsert_op` already computes the update set from it; a
/// second `vec!["id"]` written here would be the same fact stated twice, in
/// two files, with nothing keeping them equal.
#[cfg(feature = "block-dev")]
pub(crate) fn snapshot_upsert(
    data: Vec<(String, Value)>,
    conflict: Vec<String>,
    update_columns: Vec<String>,
) -> BatchWrite {
    BatchWrite::Upsert(UpsertRequest {
        collection: TABLE.to_string(),
        data,
        conflict_columns: conflict,
        on_conflict: OnConflict::SetColumns(update_columns),
    })
}

// The single definition of "deleted" for one already-loaded row, and the
// exact per-record twin of [`live_filter`]'s `deleted_at IS NULL`: a row is
// deleted iff its `deleted_at` is not SQL NULL.
//
// Deliberately reads `record.data` rather than `RecordExt::str_field`.
// `str_field` collapses a missing key, a JSON `Null` and the empty string all
// to `""`, so it cannot tell NULL from `''` — and SQL can: `'' IS NOT NULL`.
// A string-emptiness check therefore calls a `deleted_at = ''` row live while
// every list/count read (and the partial unique slug index from migration
// 005, which is also keyed on `deleted_at IS NULL`) calls it deleted. Reading
// the raw value keeps both sides on SQL's answer. A missing key and
// `Value::Null` are the two shapes an unset column decodes to on either
// backend, and both mean NULL.
fn is_deleted(record: &Record) -> bool {
    !matches!(record.data.get("deleted_at"), None | Some(Value::Null))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::{test_support::TestContext, util::RecordExt};

    async fn seed(ctx: &TestContext, id: &str, deleted_at: Option<&str>) {
        let mut data = HashMap::from([
            ("id".to_string(), json!(id)),
            ("name".to_string(), json!(id)),
            ("status".to_string(), json!("active")),
        ]);
        if let Some(ts) = deleted_at {
            data.insert("deleted_at".to_string(), json!(ts));
        }
        db::create(ctx, TABLE, data).await.expect("seed");
    }

    #[tokio::test]
    async fn get_refuses_a_soft_deleted_row() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "gone", Some("2026-09-01T00:00:00Z")).await;
        let err = get(&ctx, "gone").await.expect_err("must not resolve");
        assert_eq!(err.code, wafer_run::ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn get_returns_a_live_row() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "here", None).await;
        let row = get(&ctx, "here").await.expect("must resolve");
        assert_eq!(row.str_field("name"), "here");
    }

    #[tokio::test]
    async fn list_page_excludes_soft_deleted_rows() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "live", None).await;
        seed(&ctx, "gone", Some("2026-09-01T00:00:00Z")).await;
        let list = list_page(&ctx, 1, 50, vec![], None).await.expect("list");
        let ids: Vec<&str> = list.records.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["live"]);
    }

    #[tokio::test]
    async fn list_deleted_returns_only_soft_deleted_rows() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "live", None).await;
        seed(&ctx, "gone", Some("2026-09-01T00:00:00Z")).await;
        let list = list_deleted(&ctx, 1, 50, vec![], None)
            .await
            .expect("list_deleted");
        let ids: Vec<&str> = list.records.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["gone"]);
    }

    // Mirrors `caller_filters_are_added_to_the_soft_delete_filter` for the
    // inverse predicate: a caller filter that matches a live row must not
    // let that live row leak into the deleted view.
    #[tokio::test]
    async fn list_deleted_caller_filters_do_not_admit_live_rows() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "live", None).await;
        let status_active = Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: json!("active"),
        };
        let list = list_deleted(&ctx, 1, 50, vec![status_active], None)
            .await
            .expect("list_deleted");
        assert!(
            list.records.is_empty(),
            "a live active row must not appear in the deleted view"
        );
    }

    // `live_filter()` (`deleted_at IS NULL`) is the predicate every list and
    // count read hands to SQL, so the per-record check `get`/`get_deleted`
    // apply has to be the same predicate — otherwise "live" means one thing
    // to a single-row read and another to a listing. An empty-string
    // `deleted_at` is precisely where the two can disagree: SQL says
    // `'' IS NOT NULL`, while a string-emptiness check says "live".
    #[tokio::test]
    async fn an_empty_deleted_at_reads_the_same_way_through_every_door() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "blank", Some("")).await;

        let get_says_live = get(&ctx, "blank").await.is_ok();
        let list_says_live = !list_page(&ctx, 1, 50, vec![], None)
            .await
            .expect("list")
            .records
            .is_empty();
        assert_eq!(
            get_says_live, list_says_live,
            "`get` and `list_page` must classify the same row the same way"
        );

        let get_deleted_resolves = get_deleted(&ctx, "blank").await.is_ok();
        let list_deleted_shows_it = !list_deleted(&ctx, 1, 50, vec![], None)
            .await
            .expect("list_deleted")
            .records
            .is_empty();
        assert_eq!(
            get_deleted_resolves, list_deleted_shows_it,
            "`get_deleted` and `list_deleted` must classify the same row the same way"
        );

        // And the shared answer is SQL's: `'' IS NOT NULL`, so the row is
        // deleted. Pinning the direction as well as the agreement stops a
        // future "fix" from collapsing both onto the wrong side, where a
        // `''`-stamped row would list as live while the partial unique slug
        // index (`WHERE deleted_at IS NULL`) still claims its slug.
        assert!(
            !get_says_live,
            "an empty `deleted_at` is not NULL, so the row is deleted"
        );
    }

    #[tokio::test]
    async fn list_capped_live_excludes_soft_deleted_rows() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "live", None).await;
        seed(&ctx, "gone", Some("2026-09-01T00:00:00Z")).await;
        let records = list_capped_live(&ctx, vec![]).await.expect("live listing");
        let ids: Vec<&str> = records.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["live"]);
    }

    // A caller-supplied filter must narrow the live set, never replace the
    // soft-delete filter. Appending rather than replacing is the whole point
    // of routing reads through here.
    #[tokio::test]
    async fn caller_filters_are_added_to_the_soft_delete_filter() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "gone", Some("2026-09-01T00:00:00Z")).await;
        let status_active = Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: json!("active"),
        };
        let list = list_page(&ctx, 1, 50, vec![status_active], None)
            .await
            .expect("list");
        assert!(
            list.records.is_empty(),
            "a soft-deleted active row must not list"
        );
    }

    #[tokio::test]
    async fn count_excludes_soft_deleted_rows() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "live", None).await;
        seed(&ctx, "gone", Some("2026-09-01T00:00:00Z")).await;
        assert_eq!(count(&ctx, &[]).await.expect("count"), 1);
    }

    // Pins the timestamp behaviour that `crud::crud_create` used to provide
    // via `stamp_created` before call sites are migrated onto this module:
    // dropping that helper must not silently drop the timestamps. The
    // database service's own `DbExec::create` default impl fills in any of
    // `created_at`/`updated_at` the caller omitted, so a bare `db::create`
    // pass-through (no client-side stamping) still produces both.
    #[tokio::test]
    async fn create_stamps_created_and_updated_at() {
        let ctx = TestContext::with_products().await;
        let data = HashMap::from([
            ("id".to_string(), json!("stamped")),
            ("name".to_string(), json!("stamped")),
            ("status".to_string(), json!("active")),
        ]);
        let record = create(&ctx, data).await.expect("create");
        assert!(
            !record.str_field("created_at").is_empty(),
            "created_at must be stamped"
        );
        assert!(
            !record.str_field("updated_at").is_empty(),
            "updated_at must be stamped"
        );
    }

    // Same rationale as `create_stamps_created_and_updated_at`, for the
    // `stamp_updated` half: an update that doesn't set `updated_at` itself
    // must still come back with a fresh one.
    #[tokio::test]
    async fn update_stamps_a_new_updated_at() {
        let ctx = TestContext::with_products().await;
        let data = HashMap::from([
            ("id".to_string(), json!("stamped")),
            ("name".to_string(), json!("stamped")),
            ("status".to_string(), json!("active")),
        ]);
        let created = create(&ctx, data).await.expect("create");
        let original_updated_at = created.str_field("updated_at").to_string();

        // RFC3339-with-nanoseconds timestamps from two back-to-back
        // `Utc::now()` calls almost always differ already, but a short sleep
        // makes the "changed" assertion robust against coarse clock
        // resolution in CI rather than relying on that.
        tokio::time::sleep(Duration::from_millis(5)).await;

        let updated = update_including_deleted(
            &ctx,
            "stamped",
            HashMap::from([("name".to_string(), json!("stamped-v2"))]),
        )
        .await
        .expect("update");
        assert_ne!(
            updated.str_field("updated_at"),
            original_updated_at,
            "update must stamp a fresh updated_at"
        );
    }

    /// The bug this whole plan exists for: deleting a product used to remove
    /// the row outright, orphaning every NOT-NULL `product_id` reference to
    /// it (most visibly a completed order's line item). Soft delete must
    /// stamp `deleted_at` and leave the row resolvable by a raw `db::get`.
    #[tokio::test]
    async fn soft_delete_stamps_deleted_at_and_leaves_the_row_in_place() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "live", None).await;

        soft_delete(&ctx, "live").await.expect("soft delete");

        let raw = db::get(&ctx, TABLE, "live")
            .await
            .expect("the row must still exist");
        assert!(
            !raw.str_field("deleted_at").is_empty(),
            "deleted_at must be stamped"
        );
        let err = get(&ctx, "live")
            .await
            .expect_err("must not resolve as live");
        assert_eq!(err.code, wafer_run::ErrorCode::NotFound);
    }

    /// `soft_delete` used to `get` and then write unconditionally, which is
    /// the read-then-write race `update_live` exists to eliminate: between
    /// the two statements a concurrent `restore` commits and answers 200 with
    /// a live record, and the unconditional write then stamps `deleted_at`
    /// back on top of it — the restoring admin is told it worked and the
    /// product is gone, with no error anywhere.
    ///
    /// The liveness test has to be the write's own `WHERE`, so `soft_delete`
    /// must issue no read at all. Proven by failing every `database.get`
    /// against the products table: the old shape could not get past its first
    /// statement, the filtered write never issues one.
    #[tokio::test]
    async fn soft_delete_tests_liveness_in_the_write_not_in_a_separate_read() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "live", None).await;

        let no_reads = crate::test_support::FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.get", TABLE)],
        );
        soft_delete(&no_reads, "live")
            .await
            .expect("soft delete must not depend on a read of its own row");

        let raw = db::get(&ctx, TABLE, "live").await.expect("row still there");
        assert!(
            !raw.str_field("deleted_at").is_empty(),
            "deleted_at must be stamped"
        );
    }

    /// The other half of the same race: a second delete must match zero rows
    /// and write nothing, rather than re-stamping `deleted_at` and moving the
    /// row in the admin deleted view's `deleted_at desc` ordering.
    #[tokio::test]
    async fn a_second_soft_delete_is_not_found_and_leaves_the_stamp_alone() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "live", None).await;
        soft_delete(&ctx, "live").await.expect("first delete");
        let stamp = db::get(&ctx, TABLE, "live")
            .await
            .expect("row")
            .str_field("deleted_at")
            .to_string();

        tokio::time::sleep(Duration::from_millis(5)).await;

        let err = soft_delete(&ctx, "live")
            .await
            .expect_err("already deleted");
        assert_eq!(err.code, wafer_run::ErrorCode::NotFound);
        assert_eq!(
            db::get(&ctx, TABLE, "live")
                .await
                .expect("row")
                .str_field("deleted_at"),
            stamp,
            "a delete that deleted nothing must not move the stamp"
        );
    }

    #[tokio::test]
    async fn soft_delete_of_a_missing_row_is_not_found() {
        let ctx = TestContext::with_products().await;
        let err = soft_delete(&ctx, "missing")
            .await
            .expect_err("nothing to delete");
        assert_eq!(err.code, wafer_run::ErrorCode::NotFound);
    }

    /// The unique index added in migration 005 on `(owner_kind, owner_id,
    /// slug)` is partial on `deleted_at IS NULL`, so soft-deleting a product
    /// must free its slug for reuse rather than leaving it permanently
    /// claimed.
    #[tokio::test]
    async fn soft_delete_frees_the_slug_for_reuse() {
        let ctx = TestContext::with_products().await;
        create(
            &ctx,
            HashMap::from([
                ("id".to_string(), json!("first")),
                ("name".to_string(), json!("first")),
                ("status".to_string(), json!("active")),
                ("slug".to_string(), json!("jacket")),
            ]),
        )
        .await
        .expect("create first");

        soft_delete(&ctx, "first").await.expect("soft delete");

        create(
            &ctx,
            HashMap::from([
                ("id".to_string(), json!("second")),
                ("name".to_string(), json!("second")),
                ("status".to_string(), json!("active")),
                ("slug".to_string(), json!("jacket")),
            ]),
        )
        .await
        .expect("the freed slug must not conflict");
    }

    /// Restoring a product that was never deleted changes nothing, so it
    /// must not stamp a fresh `updated_at`: `DbExec::update` moves the
    /// column on every write, and the admin product list sorts on the
    /// timestamps. A no-op has no business reordering it.
    #[tokio::test]
    async fn restoring_a_live_product_does_not_stamp_a_new_updated_at() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "live", None).await;
        let before = db::get(&ctx, TABLE, "live")
            .await
            .expect("seeded")
            .str_field("updated_at")
            .to_string();

        // Same reason as `update_stamps_a_new_updated_at`: make the
        // "unchanged" assertion robust against coarse clock resolution
        // instead of relying on two `Utc::now()` calls differing.
        tokio::time::sleep(Duration::from_millis(5)).await;

        restore(&ctx, "live")
            .await
            .expect("restoring a live row is a no-op");

        assert_eq!(
            db::get(&ctx, TABLE, "live")
                .await
                .expect("still there")
                .str_field("updated_at"),
            before,
            "a restore that restored nothing must not move updated_at"
        );
    }

    #[tokio::test]
    async fn restore_brings_a_deleted_product_back() {
        let ctx = TestContext::with_products().await;
        seed(&ctx, "oops", None).await;
        soft_delete(&ctx, "oops").await.expect("delete");

        restore(&ctx, "oops").await.expect("restore");

        assert!(get(&ctx, "oops").await.is_ok());
    }
}
