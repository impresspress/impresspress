//! Product CRUD: admin (`/b/products/api/admin/products`) and user-owned
//! (`/b/products/api/products`, gated on `WAFER_RUN_SHARED__ALLOW_USER_PRODUCTS`).
//!
//! Every response is a `contracts::ProductView` (or a list of them) built
//! from the row; every write body is a typed request whose fields are the
//! only columns a client can reach.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::{config, database as db};
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::seller_policy;
use crate::{
    blocks::{
        crud,
        products::{
            config::{DEFAULT_CURRENCY, SELLER_MODERATION_REQUIRED},
            contracts::{
                check_product_slug, slug_from, ApprovalStatus, CreateProductRequest,
                ProductDuplicateResponse, ProductListQuery, ProductListResponse, ProductStatus,
                ProductView, UpdateProductRequest, PRODUCT_SLUG_MAX_LEN,
            },
            repo::{self, offers as offer_repo},
        },
    },
    http::{
        err_bad_request, err_forbidden, err_internal, err_not_found, err_unauthenticated, ok_json,
    },
    util::{enum_column, field_as_string, now_rfc3339, stamp_created, stamp_updated, RecordExt},
};

// Columns the products table owns internally: the row's identity, ownership,
// moderation state, the lifecycle stamps, and the Stripe/versioning linkage.
// None of them is a caller-supplied value on any tier or verb — each has a
// dedicated writer that maintains its invariants (`approve_product`/
// `reject_product`/`suspend` for `approval_status`, the delete/restore
// endpoints for `deleted_at`, the Stripe sync for `stripe_product_id`, the
// publish flow for `submitted_at`/`published_at`, seller onboarding for
// `owner_*`/`seller_account_id`, versioning for `current_version`, and the
// database layer's own UUID synthesis for `id`). A generic create or PATCH
// that let a body through verbatim would write them behind those writers'
// backs: an admin PATCH of `{"stripe_product_id": "prod_wrong"}` desyncs the
// row from the Stripe catalog, `{"owner_kind":…,"owner_id":…}` re-parents a
// seller's product, and a create carrying `deleted_at` lands a row that
// `ensure_product_capacity`'s live-only count cannot see.
//
// `id` is on the list for both verbs, and is the one that bites hardest.
// `line_items`, `offers`, `product_versions` and `entitlements` all carry a
// `product_id` that is `TEXT NOT NULL`, so a PATCH that rewrites the key
// orphans every one of them — the exact damage soft delete exists to
// prevent — while the write still reports one row affected. On create the
// field is not a rewrite, but it is still not the caller's to choose: the
// database layer synthesizes a UUID when `id` is absent and honours it when
// present, so a caller-supplied id means a public endpoint picking a primary
// key, which can only collide (a 500) or claim the key of a row that was
// purged. The products API therefore never takes an id from a body, on
// either verb.
//
// One list, four handlers: admin/user × create/update. The admin tier is not
// exempt — this is not a privilege boundary (an admin has other, deliberate
// doors to each of these fields) but an integrity one, and the two update
// handlers previously disagreeing about it is exactly the kind of drift a
// single shared constant prevents.
const UNSETTABLE_FIELDS: &[&str] = &[
    "id",
    "created_by",
    "owner_kind",
    "owner_id",
    "seller_account_id",
    "approval_status",
    "stripe_product_id",
    "current_version",
    "submitted_at",
    "published_at",
    "deleted_at",
];

/// Refuse a caller-supplied request body that names any [`UNSETTABLE_FIELDS`]
/// entry, naming the offending fields. `Ok(())` means the whole body is
/// writable as it stands.
///
/// A refusal rather than a silent drop. Dropping answered 200 with a body in
/// which the dropped field was plainly unchanged, so a client sending
/// `{"approval_status":"approved","name":"X"}` was told its write had
/// succeeded when half of it had been discarded — and, having no signal to
/// act on, would keep re-sending it. 400 is the same shape the seller PATCH
/// already uses for an unrecognized `status`, and it costs nothing the UI
/// wants: every request the shipped admin and seller pages issue sends only
/// caller-owned fields.
///
/// Handlers that legitimately write one of these fields do so *after* this
/// call, from their own computed value — so this must run on the raw parsed
/// body, before any server-supplied default or stamp is inserted.
fn reject_unsettable_fields(data: &HashMap<String, serde_json::Value>) -> Result<(), OutputStream> {
    let named: Vec<&str> = UNSETTABLE_FIELDS
        .iter()
        .copied()
        .filter(|field| data.contains_key(*field))
        .collect();
    if named.is_empty() {
        return Ok(());
    }
    Err(err_bad_request(&format!(
        "These fields are not settable through this endpoint: {}",
        named.join(", ")
    )))
}

/// Whether `user_id` may act on `product` through a user-facing (non-admin)
/// product route.
///
/// The single definition of that rule, deliberately: it used to be written
/// out at three separate doors onto the same product and one of them said
/// something different. `verify_product_owner` compared `created_by` alone,
/// while `offers::verify_product` and `pages::product_manager` accepted
/// `owner_id` OR `created_by` — so a product with `owner_id = user_1` and
/// `created_by = admin_1` (what an administrator creating a listing on a
/// seller's behalf leaves behind) rendered on the seller's own page and
/// accepted every offer and Payment Link route on it, while GET, PATCH,
/// DELETE and duplicate all answered 404 for the same caller on the same row.
///
/// They agree on the wider rule rather than the narrower one. The routes that
/// were already open are the ones that open a money surface — creating an
/// offer, opening a Payment Link — so narrowing to `created_by` would have
/// stranded a seller with a live money surface they could not read, edit or
/// shut down. `owner_id` is also the field the rest of the system treats as
/// authoritative for ownership: seller suspension and moderation scope on it,
/// and checkout routes the charge to `owner_id`'s connected account.
///
/// An empty `user_id` is never an owner — an unauthenticated caller must not
/// match a row whose `owner_id`/`created_by` happens to be blank, which is
/// exactly what every platform-owned product carries.
pub(in crate::blocks::products) fn is_owned_by(
    product: &wafer_core::clients::database::Record,
    user_id: &str,
) -> bool {
    !user_id.is_empty()
        && (field_as_string(product, "owner_id") == user_id
            || field_as_string(product, "created_by") == user_id)
}

/// Map a `repo::products` write failure onto a response.
///
/// `NotFound` is the 404 every product endpoint gives for a row that is
/// missing *or* soft-deleted — for a filtered write it is also "zero rows
/// matched", which is the same fact.
///
/// `InvalidArgument` is a CALLER error and carries a message saying what to
/// change: `repo::products::reject_id_rewrite` raises it as the backstop for
/// a caller that did not pass through [`reject_unsettable_fields`], and any
/// future repository guard will arrive the same way. Matching only `NotFound`
/// and funnelling the rest into `err_internal` answered 500 and threw the
/// message away, so the caller got an opaque server error, a correlation id,
/// and nothing to act on for a mistake that was entirely theirs.
///
/// Anything else is a genuine failure and keeps `context` for the log.
pub(in crate::blocks::products) fn write_error(
    error: wafer_run::WaferError,
    context: &str,
) -> OutputStream {
    match error.code {
        ErrorCode::NotFound => err_not_found("Product not found"),
        ErrorCode::InvalidArgument => err_bad_request(&error.message),
        _ => crud::db_error_internal(error, context),
    }
}

/// One product row as the view every product endpoint publishes, or the 500
/// a row outside the contract earns.
///
/// The single-row counterpart of [`ProductListResponse::from_record_list`]'s
/// row-not-page degradation, and the opposite trade for the opposite reason:
/// a page that drops one unreadable row still answers what the caller asked,
/// but here the row IS the response, so a `status` or `approval_status` the
/// contract does not define has to be said out loud.
pub(in crate::blocks::products) fn product_json(record: &db::Record) -> OutputStream {
    match ProductView::from_record(record) {
        Ok(view) => ok_json(&view),
        Err(error) => err_internal("Product row is outside the contract", error),
    }
}

/// Escape SQL LIKE wildcards (`%`, `_`) and the escape char (`\`) in user
/// input so a user searching for `100% off` doesn't also match arbitrary
/// characters.
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

/// Build a `name LIKE %search%` filter with LIKE wildcards escaped.
/// Returns `None` for an empty search term.
///
/// Also called from `pages::manage_products` (admin search box), hence the
/// wider-than-`handlers` visibility.
pub(in crate::blocks::products) fn name_like_filter(search: &str) -> Option<Filter> {
    if search.is_empty() {
        return None;
    }
    Some(Filter {
        field: "name".to_string(),
        operator: FilterOp::Like,
        value: serde_json::Value::String(format!("%{}%", escape_like(search))),
    })
}

/// The shared product list filters: `group_id` / `status` equality plus an
/// escaped `search` LIKE on `name`.
fn product_filters(query: &ProductListQuery) -> Vec<Filter> {
    let mut filters = Vec::new();
    if let Some(group_id) = &query.group_id {
        filters.push(Filter {
            field: "group_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(group_id.clone()),
        });
    }
    if let Some(status) = &query.status {
        filters.push(Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(status.clone()),
        });
    }
    if let Some(search) = query.search.as_deref().and_then(name_like_filter) {
        filters.push(search);
    }
    filters
}

/// One page of LIVE product rows, newest first, projected as a list response.
///
/// Through `repo::products::list_page` rather than a generic paginated read
/// against the table: the door decides which rows exist (it appends
/// `deleted_at IS NULL`), the view decides which of their columns are
/// published. Projecting a raw read would have published soft-deleted
/// products in a tidy typed envelope.
pub(super) async fn list_products(
    ctx: &dyn Context,
    query: &ProductListQuery,
    filters: Vec<Filter>,
) -> OutputStream {
    match repo::products::list_page(
        ctx,
        i64::from(query.page),
        i64::from(query.page_size),
        filters,
        None,
    )
    .await
    {
        Ok(list) => ok_json(&ProductListResponse::from_record_list(&list)),
        // `list_page` is told its table by the block, not by the request, so a
        // `NotFound` from it names no row of the caller's — a 500, not an
        // empty page reported as a 404.
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

/// Insert a product row and read it back, both through the door.
///
/// The re-read is not redundant. `db::create` answers with the columns it was
/// GIVEN; the response has to carry the ones the table DEFAULTED — notably
/// `current_version`, which every product endpoint now publishes as part of
/// `ProductView` and which reads back as 0 instead of 1 without this. The
/// untyped path got this for free from `crud::create_record`, whose own
/// create-then-`get` this replaces; the door has no equivalent because
/// `repo::products::create` is deliberately a single statement.
///
/// `get` rather than a raw read: the row was just inserted live, so the
/// soft-delete filter finds it, and this stays one fewer call site to keep
/// that filter out of.
async fn create_product_row(
    ctx: &dyn Context,
    data: HashMap<String, serde_json::Value>,
) -> Result<db::Record, wafer_run::WaferError> {
    let created = repo::products::create(ctx, data).await?;
    repo::products::get(ctx, &created.id).await
}

/// Read a caller-supplied write body once, refuse it if it names an internal
/// column, and only then project it onto the typed request `T`.
///
/// Both halves are load-bearing and neither subsumes the other. The typed
/// request is what decides which columns a write may SET — an unsettable
/// field is not one of its fields, so it can never reach the database. But
/// none of these request types is `deny_unknown_fields`, so serde DROPS such
/// a field silently: typing alone answers 200 to a body asking to set
/// `approval_status`, and the caller is told a write succeeded when half of
/// it was discarded. `reject_unsettable_fields` is what turns that into the
/// 400 `admin_patch_cannot_rewrite_a_products_id` and
/// `seller_patch_cannot_rewrite_a_products_id` require.
///
/// Runs on the raw parsed body, before any server-supplied default or stamp
/// is inserted, for the reason given above `reject_unsettable_fields`:
/// handlers that legitimately write one of these fields do so afterwards
/// from their own computed value.
async fn read_write_body<T: serde::de::DeserializeOwned>(
    input: InputStream,
) -> Result<T, OutputStream> {
    let raw = input
        .collect_to_bytes()
        .await
        .map_err(OutputStream::error)?;
    let named: HashMap<String, serde_json::Value> =
        serde_json::from_slice(&raw).map_err(|e| err_bad_request(&format!("Invalid body: {e}")))?;
    reject_unsettable_fields(&named)?;
    serde_json::from_slice(&raw).map_err(|e| err_bad_request(&format!("Invalid body: {e}")))
}

/// The row a product write replaces, for [`refuse_new_invalid_slug`].
enum SlugOwner<'a> {
    /// A create: there is no stored slug.
    New,
    /// An update of this row, already read by the handler.
    Row(&'a db::Record),
    /// An update of the row with this id, read only if it is needed.
    Id(&'a str),
}

/// Refuse a write that sets a slug outside the grammar
/// ([`check_product_slug`]), with a 400 that states it.
///
/// An update that sends back the slug its row already holds sets nothing,
/// so it is accepted whatever that slug is. A stored slug can break the
/// grammar — the column accepted any text until the API enforced it, and a
/// data-snapshot import restores rows as they were exported — and the edit
/// form sends the slug with every save. Refusing it would make the product
/// uneditable until it was renamed, and renaming changes its public address.
/// The row is read only when the sent slug breaks the grammar.
async fn refuse_new_invalid_slug(
    ctx: &dyn Context,
    slug: Option<&str>,
    owner: SlugOwner<'_>,
) -> Result<(), OutputStream> {
    let Some(slug) = slug else {
        return Ok(());
    };
    let Err(message) = check_product_slug(slug) else {
        return Ok(());
    };
    let unchanged = match owner {
        SlugOwner::New => false,
        SlugOwner::Row(row) => row.str_field("slug") == slug,
        SlugOwner::Id(id) => match repo::products::get(ctx, id).await {
            Ok(row) => row.str_field("slug") == slug,
            Err(e) => return Err(crud::db_error(e, "Product not found", "Database error")),
        },
    };
    if unchanged {
        Ok(())
    } else {
        Err(err_bad_request(&message))
    }
}

/// Fetch a product and verify the caller may act on it ([`is_owned_by`]),
/// routing the lookup through `repo::products::get` so a soft-deleted product
/// answers 404 the same as one that never existed — the generic
/// `crud::verify_owner` reads its collection raw and would let an owner keep
/// fetching/editing a soft-deleted row. Mirrors `crud::verify_owner`'s
/// response shape: 401 unauthenticated, 404 for both "missing" and "not
/// yours" (existence must not leak to a non-owner).
async fn verify_product_owner(
    ctx: &dyn Context,
    id: &str,
    user_id: &str,
) -> Result<wafer_core::clients::database::Record, OutputStream> {
    if user_id.is_empty() {
        return Err(err_unauthenticated("Not authenticated"));
    }
    match repo::products::get(ctx, id).await {
        Ok(record) => {
            if !is_owned_by(&record, user_id) {
                return Err(err_not_found("Product not found"));
            }
            Ok(record)
        }
        Err(e) => Err(crud::db_error(e, "Product not found", "Database error")),
    }
}

/// Fetch a SOFT-DELETED product and verify the caller may act on it — the
/// deleted-set twin of [`verify_product_owner`], and the gate on every
/// owner-scoped door that only makes sense once a product is deleted (the
/// seller's restore endpoint and close-only manager).
///
/// The ownership rule is [`is_owned_by`], the same one every other seller
/// door uses. Only the read differs, and it has to: `repo::products::get`
/// answers `NotFound` for a soft-deleted row by design, so a check built on
/// it would refuse exactly the rows these routes exist for.
///
/// `get_deleted` rather than `get_including_deleted`, deliberately. It is the
/// narrower read — it answers `NotFound` for a LIVE row as well as a missing
/// one — so a seller route built on it can never be turned into an existence
/// oracle for the live catalog, and a caller who reaches one of these doors
/// for a product that was never deleted gets the same 404 as for one that is
/// not theirs. (The admin restore stays a no-op 200 in that case; it does not
/// need an ownership read at all, so it never looks the row up first.)
///
/// Response shapes mirror [`verify_product_owner`]: 401 unauthenticated, 404
/// for "missing", "not deleted" and "not yours" alike — existence must not
/// leak to a non-owner.
async fn verify_deleted_product_owner(
    ctx: &dyn Context,
    id: &str,
    user_id: &str,
) -> Result<wafer_core::clients::database::Record, OutputStream> {
    if user_id.is_empty() {
        return Err(err_unauthenticated("Not authenticated"));
    }
    match repo::products::get_deleted(ctx, id).await {
        Ok(record) => {
            if !is_owned_by(&record, user_id) {
                return Err(err_not_found("Product not found"));
            }
            Ok(record)
        }
        Err(e) => Err(crud::db_error(e, "Product not found", "Database error")),
    }
}

// --- Product CRUD (admin) ---

pub(super) async fn handle_list_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let query = ProductListQuery::from_message(msg);
    let filters = product_filters(&query);
    list_products(ctx, &query, filters).await
}

pub(super) async fn handle_get_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Product") {
        Ok(value) => value,
        Err(response) => return response,
    };
    match repo::products::get(ctx, id).await {
        Ok(record) => product_json(&record),
        Err(e) => crud::db_error(e, "Product not found", "Database error"),
    }
}

pub(super) async fn handle_create_product(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let request: CreateProductRequest = match read_write_body(input).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Err(response) =
        refuse_new_invalid_slug(ctx, request.slug.as_deref(), SlugOwner::New).await
    {
        return response;
    }
    let mut data = request.into_columns();
    stamp_created(&mut data);
    // `or_insert`, and it always inserts: none of these five is a field of
    // `CreateProductRequest`, and `read_write_body` has already refused a
    // body that named one anyway.
    for (key, value) in [
        ("status", serde_json::json!(ProductStatus::Draft)),
        ("created_by", serde_json::json!(msg.user_id())),
        ("owner_kind", serde_json::json!("platform")),
        ("owner_id", serde_json::json!("")),
        (
            "approval_status",
            serde_json::json!(ApprovalStatus::Approved),
        ),
    ] {
        data.entry(key.to_string()).or_insert(value);
    }
    let slug = written_slug(&data);
    match create_product_row(ctx, data).await {
        Ok(record) => product_json(&record),
        // An insert names no row of the caller's, so its `NotFound` is a 500.
        Err(e) => slug_write_error(e, slug.as_deref(), |e| {
            crud::db_error_internal(e, "Database error")
        }),
    }
}

pub(super) async fn handle_update_product(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let id = match crud::path_id(msg, "Product") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let request: UpdateProductRequest = match read_write_body(input).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Err(response) =
        refuse_new_invalid_slug(ctx, request.slug.as_deref(), SlugOwner::Id(id)).await
    {
        return response;
    }
    let mut data = request.into_columns();
    stamp_updated(&mut data);
    // A soft-deleted product must go through `restore` before it is editable
    // again, so the generic PATCH refuses one outright rather than silently
    // applying unrelated field changes to a dead row. The liveness test is
    // the write's own `WHERE`, not a `get` before it: a separate read leaves
    // a window in which a concurrent delete commits and the PATCH then writes
    // to the dead row and answers 200 — precisely the outcome this guard
    // exists to prevent. `NotFound` matches the response every other admin
    // product endpoint gives for a soft-deleted row.
    let slug = written_slug(&data);
    match repo::products::update_live(ctx, id, data).await {
        Ok(record) => product_json(&record),
        Err(e) => slug_write_error(e, slug.as_deref(), |e| write_error(e, "Database error")),
    }
}

pub(super) async fn handle_delete_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Product") {
        Ok(value) => value,
        Err(response) => return response,
    };
    match repo::products::soft_delete(ctx, id).await {
        Ok(()) => ok_json(&crud::Deleted::done()),
        Err(e) => crud::db_error(e, "Product not found", "Database error"),
    }
}

/// Restore a soft-deleted product — the door back out of `soft_delete`,
/// without which a deleted product would be unreachable by any UI.
///
/// `POST /b/products/api/admin/products/{id}/restore`, declared
/// `AuthLevel::Admin` in `routes::ROUTES`, which is also the only thing
/// `ProductsBlock::handle` dispatches on — so the one wire path that reaches
/// this handler is the one its declaration matches. It previously sat on a
/// separate user dispatch table that the block entered from two wire
/// spellings, so the same handler also answered at
/// `/b/products/products/{id}/restore` — a spelling matching no declaration
/// at all, and so resolving to the `Authenticated` fallback. That was a live
/// privilege escalation: any logged-in user could resurrect any soft-deleted
/// product. One table over the declared wire paths is what closed it;
/// `routes::table_tests` pins that the former second spellings 404.
pub(super) async fn handle_restore_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Product") {
        Ok(value) => value,
        Err(response) => return response,
    };
    restore_product(ctx, id).await
}

/// Restore a soft-deleted product the caller has already been authorized for.
///
/// The shared body of [`handle_restore_product`] (admin tier) and
/// [`handle_user_restore_product`] (owner-scoped). It contains NO
/// authorization of its own — the tier gate and the ownership check are the
/// callers' — and everything below it is about the one thing the two tiers
/// answer identically: what a restore write means when it fails.
///
/// Shared rather than duplicated because that failure handling is the hard
/// half. A second copy for the seller route would have started as a `restore`
/// call whose error went straight to `write_error`, which is exactly the
/// opaque 500 the branches below exist to replace — on the one door out of
/// soft delete, behind a button that only reloads on success.
async fn restore_product(ctx: &dyn Context, id: &str) -> OutputStream {
    // Soft delete FREES the product's slug — migration 005's unique index is
    // partial on `deleted_at IS NULL` — and nothing stops a product created
    // afterwards from claiming it. Restoring the original then violates that
    // index's `(owner_kind, owner_id, slug)` key, which arrives here as
    // `AlreadyExists`: the only unique key a restore's `UPDATE` can collide
    // on, since it changes nothing but `deleted_at`. The Deleted view's
    // Restore button only reloads on success, so a refusal that names nothing
    // leaves the one door out of soft delete looking broken. Name the
    // collision instead, so an admin can free the slug and retry.
    //
    // The write is what asks the question. This used to be a pre-check
    // *before* `restore`, which left the answer stale by exactly the gap
    // between the two statements — a slug claimed inside that gap produced
    // the same opaque 500 the check existed to prevent. Reading the collision
    // off the failed write cannot be raced that way: the write either landed,
    // in which case there was no collision, or it did not, in which case the
    // row is still deleted and still there to be probed. It also drops two
    // reads from every successful restore, which is the common case.
    //
    // What the probe reads afterwards CAN still move — see the
    // `SlugProbe::Clear` arm below for what is done about that.
    match repo::products::restore(ctx, id).await {
        Ok(record) => product_json(&record),
        // The index refused the write. Anything else — the filtered write
        // matching zero rows (no such product, or one never deleted), a fault
        // — is not a slug collision and does not go near the probe.
        Err(e) if e.code == ErrorCode::AlreadyExists => {
            match restore_slug_conflict(ctx, id).await {
                SlugProbe::Claimed(slug) => slug_taken(&slug),
                // Nothing holds the slug, so nothing stands between this product
                // and the catalog — try again rather than reporting a failure the
                // database would no longer produce.
                //
                // The probe reads rows that go on changing after the write it is
                // explaining, which is the one thing writing first does NOT fix:
                // a claimant renamed or deleted in that gap leaves the probe with
                // nothing to blame, and reporting the first write's refusal would
                // turn away a restore that would now succeed. A clear probe is therefore a
                // reason to retry, not an answer.
                //
                // Exactly one retry, and its failure is CLASSIFIED rather than
                // forwarded. The retry is itself a write, so the gap the probe
                // closed reopens behind it: a claimant arriving between the clear
                // probe and the retry violates the same index the first write
                // did, and handing that second error to `write_error` would answer
                // a conflict that names no slug — on a request whose slug the
                // probe has right here. A retry
                // LOOP is not the fix: a competing request is free to go on
                // re-claiming the slug, and the answer worth giving (someone
                // holds it; free it and restore again) is already known.
                //
                // One retry stays safe for the reason it always was: `restore`
                // only clears `deleted_at` on the same already-deleted row, so
                // repeating it creates no duplicate record and no ancillary
                // state.
                //
                // (The write's own error says the slug was taken when it ran, but
                // not whether it still is. A clear probe is what says the restore
                // would now go through.)
                SlugProbe::Clear(slug) => match repo::products::restore(ctx, id).await {
                    Ok(record) => product_json(&record),
                    // Refused twice with a clear probe in between: the slug was
                    // free when it was read and is not free now, which is a
                    // claimant that arrived in the gap. Report the conflict.
                    Err(again) if again.code == ErrorCode::AlreadyExists => slug_taken(&slug),
                    // Anything else is the retry's own failure. A `NotFound` is
                    // the row having stopped being a deleted product in the
                    // meantime — a concurrent restore landed first, or it was
                    // purged — which is not this caller's slug conflict, and the
                    // 404 every other product endpoint gives for a row it cannot
                    // act on is the honest answer.
                    Err(again) => write_error(again, RESTORE_FAILED),
                },
                // The probe could not run. "Could not tell" is not "clear" — a
                // retry would be guessing — so the write's own answer stands: the
                // 409 its `AlreadyExists` is, without a slug to name.
                SlugProbe::Unknown => write_error(e, RESTORE_FAILED),
            }
        }
        Err(e) => write_error(e, RESTORE_FAILED),
    }
}

/// Log context for a restore that could not be explained as a slug conflict.
const RESTORE_FAILED: &str = "Database error";

/// The 409 a restore answers when a live product of the same owner holds the
/// slug it has to re-claim.
///
/// One definition, two callers: the collision probe that named the claimant,
/// and the retry that a clear probe earned and a newcomer failed anyway. They
/// are the same fact for the admin, so they say the same thing — and the
/// advice is what makes the response actionable, which is the whole reason
/// this is not a 500.
fn slug_taken(slug: &str) -> OutputStream {
    crud::TakenKey::new("product", "slug", slug)
        .conflict_with("Rename or delete that product, then restore this one.")
}

/// The slug a product write sets, when the unique index can refuse it.
///
/// Migration 005's `impresspress__products__products_owner_slug_uniq` is the
/// products table's one caller-chosen unique key — `(owner_kind, owner_id,
/// slug)`, partial on `slug <> ''` and `deleted_at IS NULL` — and no
/// create or update route lets a caller set the owner or `deleted_at`. So a
/// write that sets a non-empty slug and is refused as a duplicate collided on
/// that slug, and a write that sets none cannot collide at all.
fn written_slug(data: &HashMap<String, serde_json::Value>) -> Option<String> {
    data.get("slug")
        .and_then(serde_json::Value::as_str)
        .filter(|slug| !slug.is_empty())
        .map(str::to_string)
}

/// What a product write that set `slug` answers when it failed: the named
/// 409 for a duplicate (see [`written_slug`]), `otherwise` for the rest.
fn slug_write_error(
    error: wafer_run::WaferError,
    slug: Option<&str>,
    otherwise: impl FnOnce(wafer_run::WaferError) -> OutputStream,
) -> OutputStream {
    match slug {
        Some(slug) => crud::taken_key_or(
            error,
            crud::TakenKey::new("product", "slug", slug),
            otherwise,
        ),
        None => otherwise(error),
    }
}

/// What [`restore_slug_conflict`] found. Three answers, not two: "no
/// claimant" and "no answer" lead to opposite responses — one retries, one
/// gives up — and a probe that returned a plain "is it claimed?" boolean
/// would fold them together, which is how a transient read failure gets
/// reported as a clear slug.
enum SlugProbe {
    /// A live product of the same owner holds the slug, named here so the
    /// admin can free it.
    Claimed(String),
    /// Nothing holds the slug. It is carried anyway, because the retry a
    /// clear probe earns can be refused on that same slug — and then the slug
    /// is exactly what the response has to name.
    Clear(String),
    /// Neither answer is available: the probe could not run, or the row
    /// carries no slug and so cannot have collided on one at all.
    Unknown,
}

/// Whether a live product has claimed the slug a failed
/// [`handle_restore_product`] write was trying to re-claim.
///
/// Safe to call only after the write failed: the row is then still
/// soft-deleted, so `get_deleted` can still read the slug in question. A
/// `get_deleted` that comes back `NotFound` is [`SlugProbe::Unknown`] rather
/// than [`SlugProbe::Clear`] — the row moved under us and there is nothing
/// left to reason about. So is a row carrying no slug, which the index the
/// write was refused by does not apply to at all.
async fn restore_slug_conflict(ctx: &dyn Context, id: &str) -> SlugProbe {
    let Ok(deleted) = repo::products::get_deleted(ctx, id).await else {
        return SlugProbe::Unknown;
    };
    let slug = deleted.str_field("slug").to_string();
    // Migration 005's index is partial on `slug <> ''` as well as on
    // `deleted_at IS NULL`, so any number of rows may hold an empty slug and
    // a row holding one cannot have been refused by that index. Neither
    // "claimed" nor "clear" describes such a write's failure — it is not a
    // slug question at all — so the write's own error is what the caller
    // gets, and no retry is attempted for a collision that cannot exist.
    if slug.is_empty() {
        return SlugProbe::Unknown;
    }
    match slug_is_claimed(ctx, &deleted, &slug).await {
        Ok(true) => SlugProbe::Claimed(slug),
        Ok(false) => SlugProbe::Clear(slug),
        Err(_) => SlugProbe::Unknown,
    }
}

/// Whether a LIVE product of the same owner already holds `slug`, the
/// non-empty slug `deleted` would re-claim on restore.
///
/// Keyed on `(owner_kind, owner_id, slug)` because that is the unique index's
/// own key.
///
/// A read failure is returned, not folded into `Ok(false)`: "no collision"
/// and "could not tell" are different answers, and only the caller knows what
/// to do with the second one.
async fn slug_is_claimed(
    ctx: &dyn Context,
    deleted: &db::Record,
    slug: &str,
) -> Result<bool, wafer_run::WaferError> {
    let filters = vec![
        eq_filter("owner_kind", deleted.str_field("owner_kind")),
        eq_filter("owner_id", deleted.str_field("owner_id")),
        eq_filter("slug", slug),
    ];
    // `count` appends the live-only filter, so a second soft-deleted row
    // sharing the slug is correctly not a collision. A count, not a row read:
    // the question is only whether the key is taken.
    Ok(repo::products::count(ctx, &filters).await? > 0)
}

fn eq_filter(field: &str, value: &str) -> Filter {
    Filter {
        field: field.to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(value.to_string()),
    }
}

fn copied_name(source: &wafer_core::clients::database::Record) -> String {
    let base = source
        .str_field("name")
        .chars()
        .take(150)
        .collect::<String>();
    format!("{base} copy")
}

/// The copy's slug: the source's, brought into the grammar
/// ([`slug_from`]) and cut short enough for the `-copy-` suffix, or
/// `product` when nothing of it survives. Valid whatever the source holds,
/// because the duplicate writes it without [`refuse_new_invalid_slug`].
fn copied_slug(source: &wafer_core::clients::database::Record) -> String {
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let suffix = &suffix[suffix.len() - 8..];
    let tail = format!("-copy-{suffix}");
    let base = slug_from(source.str_field("slug"), PRODUCT_SLUG_MAX_LEN - tail.len());
    let base = if base.is_empty() {
        "product".to_string()
    } else {
        base
    };
    format!("{base}{tail}")
}

async fn duplicate_product(ctx: &dyn Context, msg: &Message, owner_only: bool) -> OutputStream {
    let source_id = match crud::path_id(msg, "Product") {
        Ok(value) => value,
        Err(response) => return response,
    };
    let source = if owner_only {
        match verify_product_owner(ctx, source_id, msg.user_id()).await {
            Ok(source) => source,
            Err(response) => return response,
        }
    } else {
        match repo::products::get(ctx, source_id).await {
            Ok(source) => source,
            Err(error) => {
                return crud::db_error(error, "Product not found", "Could not load product")
            }
        }
    };
    if owner_only {
        if let Err(response) = seller_policy::ensure_product_capacity(ctx, msg.user_id()).await {
            return response;
        }
        if let Err(response) = seller_policy::validate_product_record(ctx, &source).await {
            return response;
        }
    }

    let mut data = HashMap::new();
    for field in [
        "description",
        "currency",
        "group_id",
        "image_url",
        "product_template_id",
        "fulfillment_kind",
        "tags",
        "metadata",
    ] {
        if let Some(value) = source.data.get(field) {
            data.insert(field.to_string(), value.clone());
        }
    }
    data.insert(
        "name".to_string(),
        serde_json::Value::String(copied_name(&source)),
    );
    data.insert(
        "slug".to_string(),
        serde_json::Value::String(copied_slug(&source)),
    );
    data.insert(
        "status".to_string(),
        serde_json::Value::String("draft".to_string()),
    );
    data.insert(
        "created_by".to_string(),
        serde_json::Value::String(msg.user_id().to_string()),
    );
    if owner_only {
        let moderation_required = match seller_moderation_required(ctx).await {
            Ok(required) => required,
            Err(e) => return crud::db_error_internal(e, "Could not read the moderation policy"),
        };
        data.insert(
            "owner_kind".to_string(),
            serde_json::Value::String("user".to_string()),
        );
        data.insert(
            "owner_id".to_string(),
            serde_json::Value::String(msg.user_id().to_string()),
        );
        data.insert(
            "approval_status".to_string(),
            serde_json::json!(if moderation_required {
                ApprovalStatus::Draft
            } else {
                ApprovalStatus::Approved
            }),
        );
        if let Some(account_id) = source.data.get("seller_account_id") {
            data.insert("seller_account_id".to_string(), account_id.clone());
        }
    } else {
        data.insert(
            "owner_kind".to_string(),
            serde_json::Value::String("platform".to_string()),
        );
        data.insert(
            "owner_id".to_string(),
            serde_json::Value::String(String::new()),
        );
        data.insert(
            "approval_status".to_string(),
            serde_json::json!(ApprovalStatus::Approved),
        );
    }
    stamp_created(&mut data);
    // Re-read after the insert so the response carries the table defaults
    // the copy did not set.
    let created = match create_product_row(ctx, data).await {
        Ok(created) => created,
        Err(error) => return crud::db_error_internal(error, "Could not duplicate product"),
    };
    let duplicated_offers = match offer_repo::duplicate_for_product(
        ctx,
        source_id,
        &created.id,
        msg.user_id(),
    )
    .await
    {
        Ok(offers) => offers,
        Err(error) => {
            if let Err(cleanup_error) = offer_repo::delete_for_product(ctx, &created.id).await {
                tracing::error!(product_id = %created.id, error = %cleanup_error, "could not compensate duplicated offers");
            }
            // Hard-delete, not the door: this product was never visible to
            // anyone (creation failed before returning), so a soft-deleted
            // husk would needlessly consume its slug against the partial
            // unique index instead of freeing it for retry.
            if let Err(cleanup_error) = repo::products::purge(ctx, &created.id).await {
                tracing::error!(product_id = %created.id, error = %cleanup_error, "could not compensate duplicated product");
            }
            return crud::db_error_internal(error, "Could not duplicate product pricing");
        }
    };
    let product = match ProductView::from_record(&created) {
        Ok(product) => product,
        Err(error) => return err_internal("Product row is outside the contract", error),
    };
    ok_json(&ProductDuplicateResponse {
        product,
        offers: duplicated_offers,
    })
}

pub(super) async fn handle_duplicate_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    duplicate_product(ctx, msg, false).await
}

// --- User's own products ---

async fn seller_moderation_required(ctx: &dyn Context) -> Result<bool, wafer_run::WaferError> {
    crate::config_vars::get_bool(ctx, SELLER_MODERATION_REQUIRED, true).await
}

pub(super) async fn handle_user_list_products(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return err_unauthenticated("Not authenticated");
    }

    let query = ProductListQuery::from_message(msg);
    let mut filters = vec![Filter {
        field: "created_by".to_string(),
        operator: FilterOp::Equal,
        value: serde_json::Value::String(user_id),
    }];
    filters.extend(product_filters(&query));
    list_products(ctx, &query, filters).await
}

pub(super) async fn handle_user_get_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Product") {
        Ok(value) => value,
        Err(response) => return response,
    };
    match verify_product_owner(ctx, id, msg.user_id()).await {
        Ok(record) => product_json(&record),
        Err(response) => response,
    }
}

pub(super) async fn handle_user_create_product(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let user_id = msg.user_id().to_string();
    if user_id.is_empty() {
        return err_unauthenticated("Not authenticated");
    }

    // The unsettable-field refusal happens inside `read_write_body`, and so
    // before the capacity check rather than after it: a body carrying
    // `deleted_at` would otherwise create a row the check's live-only count
    // cannot see, leaving the seller's slot free for the next create and the
    // one after that.
    let request: CreateProductRequest = match read_write_body(input).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Err(response) =
        refuse_new_invalid_slug(ctx, request.slug.as_deref(), SlugOwner::New).await
    {
        return response;
    }
    if let Err(response) = seller_policy::ensure_product_capacity(ctx, &user_id).await {
        return response;
    }

    // Verify user owns the group (if provided)
    if let Some(group_id) = request.group_id.as_deref().filter(|id| !id.is_empty()) {
        match repo::groups::get(ctx, group_id).await {
            Ok(group) => {
                if field_as_string(&group, "user_id") != user_id {
                    return err_bad_request("You don't own this group");
                }
            }
            Err(error) if error.code == ErrorCode::NotFound => {
                return err_bad_request("Group not found")
            }
            Err(error) => return crud::db_error_internal(error, "Could not load group"),
        }
    }

    let mut data = request.into_columns();
    let moderation_required = match seller_moderation_required(ctx).await {
        Ok(required) => required,
        Err(e) => return crud::db_error_internal(e, "Could not read the moderation policy"),
    };
    data.insert(
        "status".to_string(),
        serde_json::json!(ProductStatus::Draft),
    );
    data.insert(
        "approval_status".to_string(),
        serde_json::json!(if moderation_required {
            ApprovalStatus::Draft
        } else {
            ApprovalStatus::Approved
        }),
    );
    data.insert(
        "owner_kind".to_string(),
        serde_json::Value::String("user".to_string()),
    );
    data.insert(
        "owner_id".to_string(),
        serde_json::Value::String(user_id.clone()),
    );
    data.insert("created_by".to_string(), serde_json::Value::String(user_id));
    if data
        .get("currency")
        .is_none_or(|value| value.as_str().is_some_and(str::is_empty))
    {
        let currency = match config::get_default(ctx, DEFAULT_CURRENCY, "USD").await {
            Ok(currency) => currency,
            Err(e) => return crud::db_error_internal(e, "Could not read the default currency"),
        };
        data.insert("currency".to_string(), serde_json::json!(currency));
    }
    stamp_created(&mut data);
    // Default product_template_id to the seeded "default" template's real
    // (UUIDv7) id if the caller didn't specify one. The previous fallback
    // to the literal integer `1` would never match a seeded record (ids
    // are UUIDs, not integers).
    if data
        .get("product_template_id")
        .is_none_or(|value| value.as_str().is_some_and(str::is_empty))
    {
        match repo::product_templates::default_id(ctx).await {
            Ok(Some(default_id)) => {
                data.insert(
                    "product_template_id".to_string(),
                    serde_json::Value::String(default_id),
                );
            }
            // No default template seeded: proceed without one, as this create
            // always has.
            Ok(None) => {}
            // Same reasoning as the group create: a failed read is not "there
            // is no default template", and writing the product anyway leaves
            // a template-less row behind that reads as a choice.
            Err(error) => return crud::db_error_internal(error, "Could not load product template"),
        }
    }
    if let Err(response) = seller_policy::validate_product_fields(ctx, &data).await {
        return response;
    }

    let slug = written_slug(&data);
    match create_product_row(ctx, data).await {
        Ok(record) => product_json(&record),
        // An insert names no row of the caller's, so its `NotFound` is a 500.
        Err(e) => slug_write_error(e, slug.as_deref(), |e| {
            crud::db_error_internal(e, "Database error")
        }),
    }
}

pub(super) async fn handle_user_update_product(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let id = match crud::path_id(msg, "Product") {
        Ok(value) => value.to_string(),
        Err(response) => return response,
    };
    let current = match verify_product_owner(ctx, &id, msg.user_id()).await {
        Ok(record) => record,
        Err(response) => return response,
    };

    let request: UpdateProductRequest = match read_write_body(input).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Err(response) =
        refuse_new_invalid_slug(ctx, request.slug.as_deref(), SlugOwner::Row(&current)).await
    {
        return response;
    }
    // The ownership, moderation and provider columns the untyped path had to
    // strip here are not fields of `UpdateProductRequest`, so they cannot
    // arrive; `read_write_body` above is what REFUSES a body naming one
    // rather than dropping it silently.
    let requested_status = request.status;
    let mut data = request.into_columns();
    if let Err(response) = seller_policy::validate_product_fields(ctx, &data).await {
        return response;
    }

    match requested_status {
        None => {}
        Some(ProductStatus::PendingReview) => {
            return err_bad_request("Seller product status must be draft, active, or archived");
        }
        Some(ProductStatus::Draft | ProductStatus::Archived) => {}
        Some(ProductStatus::Active) => {
            if let Err(response) =
                seller_policy::validate_product_record_with_patch(ctx, &current, &data).await
            {
                return response;
            }
            let approval = match enum_column::<ApprovalStatus>(&current, "approval_status") {
                Ok(approval) => approval,
                Err(error) => {
                    return err_internal("Product row is outside the contract", error);
                }
            };
            if approval == ApprovalStatus::Suspended {
                return err_forbidden("Suspended products cannot be published");
            }
            let moderation_required = match seller_moderation_required(ctx).await {
                Ok(required) => required,
                Err(e) => {
                    return crud::db_error_internal(e, "Could not read the moderation policy")
                }
            };
            if moderation_required && approval != ApprovalStatus::Approved {
                // The two columns a seller submission moves, to two different
                // values: `status` is the publication state a buyer sees and
                // `approval_status` is the moderation state an administrator
                // acts on. Review bug B11 read the pair as one vocabulary
                // spelled twice; `tests::status_enum_tests` pins that it is not.
                data.insert(
                    "status".to_string(),
                    serde_json::json!(ProductStatus::PendingReview),
                );
                data.insert(
                    "approval_status".to_string(),
                    serde_json::json!(ApprovalStatus::Pending),
                );
                data.insert(
                    "submitted_at".to_string(),
                    serde_json::Value::String(now_rfc3339()),
                );
            } else {
                data.insert(
                    "status".to_string(),
                    serde_json::json!(ProductStatus::Active),
                );
                data.insert(
                    "approval_status".to_string(),
                    serde_json::json!(ApprovalStatus::Approved),
                );
                data.insert(
                    "published_at".to_string(),
                    serde_json::Value::String(now_rfc3339()),
                );
            }
        }
    }

    stamp_updated(&mut data);
    // `update_live`, for the same reason the admin PATCH uses it: the
    // ownership check above is a separate read, and the validation between it
    // and this write only widens the window a concurrent delete can land in.
    // The write itself has to be the thing that tests liveness.
    let slug = written_slug(&data);
    match repo::products::update_live(ctx, &id, data).await {
        Ok(record) => product_json(&record),
        Err(error) => {
            slug_write_error(error, slug.as_deref(), |e| write_error(e, "Database error"))
        }
    }
}

pub(super) async fn handle_user_delete_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Product") {
        Ok(value) => value.to_string(),
        Err(response) => return response,
    };
    if let Err(response) = verify_product_owner(ctx, &id, msg.user_id()).await {
        return response;
    }
    match repo::products::soft_delete(ctx, &id).await {
        Ok(()) => ok_json(&crud::Deleted::done()),
        Err(e) => crud::db_error(e, "Product not found", "Database error"),
    }
}

/// Restore a soft-deleted product the CALLER OWNS —
/// `POST /b/products/api/products/{id}/restore`, declared
/// `AuthLevel::Authenticated`.
///
/// The seller-side twin of [`handle_restore_product`], and the reason the
/// admin one is no longer the only door: a seller could soft-delete their own
/// product but not see it, restore it, or recover its id, while its Stripe
/// Prices and Payment Links stayed live in the connected account and went on
/// taking money. Before soft delete, a hard delete at least left nothing
/// behind.
///
/// The tier is `Authenticated`, which admits every logged-in caller — so
/// [`verify_deleted_product_owner`] is the entire authorization boundary, and
/// it uses [`is_owned_by`], the single ownership rule this block already
/// shares between the product CRUD routes, the offer routes and
/// `pages::product_manager`. A second rule here is how those three disagreed
/// with each other once already.
///
/// `POST /b/products/api/products/{id}/restore`, declared `Authenticated`
/// in `routes::ROUTES` and served at that one spelling: the raw
/// `/b/products/products/{id}/restore` the block used to answer as well is
/// not a route (`routes::table_tests` and
/// `the_former_alias_spelling_of_a_seller_route_is_not_found` pin that).
/// The tier admits every logged-in caller, so ownership — the
/// `verify_deleted_product_owner` check above, not the URL — is what refuses
/// seller B.
pub(super) async fn handle_user_restore_product(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Product") {
        Ok(value) => value.to_string(),
        Err(response) => return response,
    };
    if let Err(response) = verify_deleted_product_owner(ctx, &id, msg.user_id()).await {
        return response;
    }
    // The ownership read above is a separate statement, but nothing it
    // established can go stale under the write: `owner_kind`/`owner_id`/
    // `created_by` are all on `UNSETTABLE_FIELDS`, so no endpoint on any tier
    // re-parents a product. A concurrent restore CAN land in between, and
    // `repo::products::restore` already answers that correctly — its filtered
    // write matches zero rows and the re-read hands back the now-live record.
    restore_product(ctx, &id).await
}

pub(super) async fn handle_user_duplicate_product(
    ctx: &dyn Context,
    msg: &Message,
) -> OutputStream {
    duplicate_product(ctx, msg, true).await
}
