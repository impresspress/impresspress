//! Generic CRUD helpers for block handlers.
//!
//! `Result`-returning primitives (`read_json_body`, `list_page`,
//! `get_record`, `create_record`, `update_record`, `delete_record` and the
//! `*_owned` variants) do one database step each and hand back either the
//! row or a ready-to-send error response. A handler that publishes a typed
//! view composes them: parse a typed request, turn it into the column map,
//! run the step, project the row through `View::from_record`. The record id
//! is read only as the block's route table bound it (`path_id`, `msg.var`);
//! the untyped `crud_*` one-liners that used to strip it off the path went
//! with their last caller.
//!
// audit-allow-file: pure pass-through helpers — every db::* call here takes
// the table name as a `collection: &str` parameter from the caller. WRAP
// coverage is the caller's responsibility; static analysis at this file
// would flag every line as unresolved without surfacing a real bug.

use std::collections::HashMap;

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use wafer_block::db::{Filter, SortField};
use wafer_core::clients::database::{self as db, Record, RecordList};
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use crate::{
    http::{err_bad_request, err_conflict, err_internal, err_not_found, err_unauthenticated},
    util::{field_as_string, stamp_created, stamp_updated},
};

/// The response a failed database call turns into — the one place that
/// decides it.
///
/// Every arm exists because collapsing it into the 500 loses something the
/// caller needs:
///
/// - [`ErrorCode::NotFound`] is the row the caller asked for, so it is a 404
///   labelled `not_found` (the full message, not a noun: a route knows what
///   it was looking for and this helper does not).
/// - [`ErrorCode::PermissionDenied`] is a WRAP refusal — either a row guard
///   the caller is not the owner of, or a [`wafer_run::ResourceGrant`] the
///   block never declared. It is a **403**. Before this helper existed, all
///   62 hand-written mappings in the tree fell through to `err_internal`, so
///   a block deployed without a grant answered `500 Internal server error
///   (ref: …)` and an operator had nothing to distinguish it from a corrupt
///   row. The refusal's own message names the missing grant and the target
///   table, which is deployment topology, so it is logged here and the
///   client is told only that access was denied.
/// - [`ErrorCode::ResourceExhausted`] is a quota, which
///   `wafer_block::http_codec` already renders as 429. Its message is a
///   classified, client-actionable refusal from the service — the same class
///   this repo already echoes for `InvalidArgument` — so it is passed
///   through rather than sanitized. One such quota is the database's
///   statement budget: on Cloudflare D1 a write that does not fit what the
///   request has left of D1's per-invocation query limit is refused before it
///   runs. That 429 means the request did too much, not "retry later": the
///   same request retried does the same work and is refused again, so a
///   client must not auto-retry it. Its message gives the numbers, and its
///   detail code — `database.statement_budget_exhausted`, the body's `code` —
///   is kept, because it is the only thing that tells a client this 429 from
///   a rate limit's.
/// - [`ErrorCode::InvalidArgument`] carrying the detail code
///   `database.statement_budget_exceeds_limit` is the same budget refusing a
///   write larger than a whole invocation's limit: no invocation can run it,
///   so it is the client's **400**, with its code, rather than a 500. Any
///   other `InvalidArgument` from the database is a statement this repo
///   built wrong, and stays internal (see below).
/// - [`ErrorCode::AlreadyExists`] is a write that duplicates a primary or
///   unique key — a request the database refused, not a fault — so it is a
///   **409**. Every `DatabaseService` reports a duplicate this way (see
///   [`taken_key_or`]). The driver's own text names the table and the
///   column, which is schema, so it is logged and the client is told only
///   [`DUPLICATE_KEY`]; a route that knows which key it wrote names it through
///   [`taken_key_or`] instead.
/// - Everything else is an internal failure: `context` is the fixed log
///   label, the cause is logged, and the client gets the sanitized
///   `"Internal server error (ref: <id>)"`.
///
/// Domain classifications a *repo* raises (`InvalidArgument` other than the
/// statement budget's, `FailedPrecondition`, `Aborted`) are deliberately NOT
/// here: they mean
/// different things per block, and the three block-private helpers that map
/// them (`products/handlers/{sellers,offers,product}.rs`) keep their own arms
/// and delegate only this tail.
pub fn db_error(error: wafer_run::WaferError, not_found: &str, context: &str) -> OutputStream {
    seal(classify_db_error(error, Some(not_found), context), context)
}

/// [`db_error`] for a call whose `NotFound` is NOT the client's row.
///
/// `db::paginated_list` and `db::create` are told the table by the block, not
/// by the request, and name no row of the caller's, so whatever `NotFound`
/// they return is not the caller's 404 — it stays a 500. Turning it into a
/// 404 would tell a caller their query found nothing when in fact the call
/// failed.
/// Everything else is classified exactly as [`db_error`] classifies it,
/// `PermissionDenied` included.
pub fn db_error_internal(error: wafer_run::WaferError, context: &str) -> OutputStream {
    seal(classify_db_error(error, None, context), context)
}

/// [`db_error_internal`] for a read a full page renders from.
///
/// A page whose read failed is never drawn from defaults (see
/// [`crate::ui::server_error_response`]), and what it answers instead is
/// classified here like every other failed database call: a WRAP denial is
/// the 403 page, a quota the 429 page and a duplicate key the 409 page
/// ([`crate::ui::refused_response`]), anything else is logged under `context`
/// and answered with the styled 500. An API caller (an `Accept` without
/// `text/html`) gets the same statuses as JSON.
pub fn db_error_page(msg: &Message, error: wafer_run::WaferError, context: &str) -> OutputStream {
    match classify_db_error(error, None, context) {
        DbFailure::Refused(refusal) => crate::ui::refused_response(msg, refusal),
        DbFailure::Internal(fault) => {
            tracing::error!(context = %context, error = %fault, "page read failed");
            crate::ui::server_error_response(msg)
        }
    }
}

/// [`db_error_page`] for a read behind an htmx swap: what the notice in place
/// of the fragment says went wrong.
///
/// A swap cannot answer the 403, 429 or 500 a page does — htmx 2 swaps only a
/// 2xx body, so the stale fragment would stay on screen — and so the caller
/// answers a 2xx notice ([`crate::ui::swap_error_response`] and its row
/// variant, or an alert in the swapped body) and puts this reason in it. It is
/// classified like every other failed database call: a WRAP denial says access
/// was denied, the statement budget says the read needs more database work
/// than one request may do, another quota says the usage limit and a
/// duplicate key says the entry already exists, with the denial's own text
/// (grant and table names) logged, never shown. Anything else is logged under
/// `context` and said as a fault.
pub fn db_error_notice(error: wafer_run::WaferError, context: &str) -> &'static str {
    match classify_db_error(error, None, context) {
        DbFailure::Refused(refusal) if refusal.is_statement_budget() => {
            "it needs more database work than one request may do"
        }
        DbFailure::Refused(refusal) if refusal.code() == ErrorCode::ResourceExhausted => {
            "it is over its usage limit right now"
        }
        DbFailure::Refused(refusal) if refusal.code() == ErrorCode::AlreadyExists => {
            "it duplicates an entry that already exists"
        }
        DbFailure::Refused(_) => "access to it was denied",
        DbFailure::Internal(fault) => {
            tracing::error!(context = %context, error = %fault, "fragment read failed");
            "something went wrong"
        }
    }
}

/// What [`db_error`] decided, before it is sealed into a response.
///
/// [`db_error`] seals this itself and is what almost every call site wants.
/// A block whose every response carries an extra header cannot use it —
/// an `OutputStream`'s meta is fixed when it is built, so the header has to
/// go on the error before it is sealed — and `blocks::dev` is that block:
/// design §12 makes every `/b/dev` response `Cache-Control: no-store`,
/// including its refusals. It seals this itself through
/// `dev::no_store_db_error`. **Nothing else may**: a third classification of
/// a database failure is exactly what `tests/error_door.rs` exists to stop.
///
/// Only [`classify_db_error`] can build one. Both variants wrap a type whose
/// field is private to this module, so a caller can take a `DbFailure` apart
/// but cannot assemble one from an unclassified error and hand it to a sealer
/// that trusts it was classified:
///
/// ```
/// use impresspress_core::blocks::crud::{classify_db_error, DbFailure};
/// use wafer_run::{ErrorCode, WaferError};
///
/// let failure = classify_db_error(WaferError::new(ErrorCode::Internal, "x"), None, "doc");
/// assert!(matches!(failure, DbFailure::Internal(fault) if fault.code() == ErrorCode::Internal));
/// ```
///
/// ```compile_fail
/// use impresspress_core::blocks::crud::{DbFailure, Refusal};
/// use wafer_run::{ErrorCode, WaferError};
///
/// // A raw WRAP denial passed off as already classified.
/// let _ = DbFailure::Refused(Refusal(WaferError::new(ErrorCode::PermissionDenied, "x")));
/// ```
///
/// ```compile_fail
/// use impresspress_core::blocks::crud::{DbFailure, Fault};
/// use wafer_run::{ErrorCode, WaferError};
///
/// // An unclassified error passed off as a fault the classifier kept.
/// let _ = DbFailure::Internal(Fault(WaferError::new(ErrorCode::PermissionDenied, "x")));
/// ```
pub enum DbFailure {
    /// A refusal the client is told about as it stands: the caller's 404,
    /// the 403 a WRAP denial becomes, the 429 a quota keeps, the 409 a
    /// duplicate key is. The cause, when
    /// it was one that must not be published, has already been logged and
    /// replaced.
    Refused(Refusal),
    /// An internal fault, carried back untouched — sanitizing it and minting
    /// its correlation id is [`crate::http::err_internal`]'s job, and doing
    /// it here would mean two places that log a 500.
    Internal(Fault),
}

/// [`DbFailure::Refused`]'s error: safe to send to the client as it stands.
pub struct Refusal(wafer_run::WaferError);

impl Refusal {
    /// The refusal's code: `NotFound`, `PermissionDenied`,
    /// `ResourceExhausted`, `AlreadyExists`, or `InvalidArgument` for a write
    /// over the whole statement budget.
    pub fn code(&self) -> ErrorCode {
        self.0.code
    }

    /// Whether the database's statement budget refused the call: the request
    /// needs more statements than its invocation has left, or than any
    /// invocation may run. Retrying it unchanged fails the same way.
    pub fn is_statement_budget(&self) -> bool {
        is_statement_budget_refusal(&self.0)
    }

    /// The error to answer the client with.
    pub fn into_error(self) -> wafer_run::WaferError {
        self.0
    }
}

/// [`DbFailure::Internal`]'s error: the untouched cause, never shown to the
/// client.
pub struct Fault(wafer_run::WaferError);

impl Fault {
    /// The cause's code.
    pub fn code(&self) -> ErrorCode {
        self.0.code
    }

    /// The cause, for the caller to log and seal.
    pub fn into_error(self) -> wafer_run::WaferError {
        self.0
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

/// Classify a failed database call. `not_found` is `Some` when a `NotFound`
/// from this call means the row the *caller* named (so it is their 404), and
/// `None` when the block chose the address itself — a `db::paginated_list`
/// or `db::create` against a table the request never named, where a
/// `NotFound` names no row of the caller's and is therefore a 500.
///
/// A missing table is not a `NotFound` on any backend under STRICT_SCHEMA
/// (every Cloudflare deploy): the statement fails, and that failure is
/// `Internal`, so it is a 500 whichever way `not_found` is set.
pub fn classify_db_error(
    error: wafer_run::WaferError,
    not_found: Option<&str>,
    context: &str,
) -> DbFailure {
    let refused = |code, message: &str| {
        DbFailure::Refused(Refusal(wafer_run::WaferError::new(code, message)))
    };
    if is_statement_budget_refusal(&error) {
        // Rebuilt from the code, the message and the detail code alone: the
        // detail code is what a client keys "do not retry this" on.
        let detail = error.detail_code().unwrap_or_default().to_string();
        return DbFailure::Refused(Refusal(
            wafer_run::WaferError::new(error.code, error.message).with_detail_code(detail),
        ));
    }
    match (error.code, not_found) {
        (ErrorCode::NotFound, Some(label)) => refused(ErrorCode::NotFound, label),
        (ErrorCode::PermissionDenied, _) => {
            tracing::warn!(
                context = %context,
                error = %error,
                "database access denied — a WRAP grant or a row guard refused this call",
            );
            refused(ErrorCode::PermissionDenied, "Access denied")
        }
        (ErrorCode::ResourceExhausted, _) => refused(ErrorCode::ResourceExhausted, &error.message),
        (ErrorCode::AlreadyExists, _) => {
            tracing::info!(
                context = %context,
                error = %error,
                "database refused a write that duplicates a unique key",
            );
            refused(ErrorCode::AlreadyExists, DUPLICATE_KEY)
        }
        _ => DbFailure::Internal(Fault(error)),
    }
}

/// Whether `error` is the database's statement-budget refusal, by the detail
/// code `wafer-core`'s database handler attaches: `ResourceExhausted` with
/// `database.statement_budget_exhausted`, or `InvalidArgument` with
/// `database.statement_budget_exceeds_limit`.
fn is_statement_budget_refusal(error: &wafer_run::WaferError) -> bool {
    use wafer_block::wire::database::{STATEMENT_BUDGET_EXCEEDS_LIMIT, STATEMENT_BUDGET_EXHAUSTED};
    matches!(
        (error.code, error.detail_code()),
        (
            ErrorCode::ResourceExhausted,
            Some(STATEMENT_BUDGET_EXHAUSTED)
        ) | (
            ErrorCode::InvalidArgument,
            Some(STATEMENT_BUDGET_EXCEEDS_LIMIT)
        )
    )
}

/// What a client is told when its write duplicated a unique key and the route
/// did not name the key (see [`classify_db_error`] and [`TakenKey`]).
pub const DUPLICATE_KEY: &str = "A record with the same key already exists";

/// [`DbFailure`] as the response every caller but `blocks::dev` wants.
fn seal(failure: DbFailure, context: &str) -> OutputStream {
    match failure {
        DbFailure::Refused(refusal) => OutputStream::error(refusal.into_error()),
        DbFailure::Internal(fault) => err_internal(context, fault.into_error()),
    }
}

// ---------------------------------------------------------------------------
// Duplicate natural keys
// ---------------------------------------------------------------------------

/// The user-facing unique field a write set, and the value it set: what a
/// duplicate of it is called when the database refuses the write.
///
/// Every table a route writes a caller-chosen unique value into has one such
/// natural key — `variables.key`, `roles.name`, `permissions.name`,
/// `buckets.name`, `providers.name`, a product's `slug`, a checkout preset's
/// `slug`, a ticket type's `key` — so a route that gets `AlreadyExists` back
/// from that write knows which field clashed without asking the database. The
/// driver's own text (table, column, index) is never read for it: it is
/// schema, and not every adapter spells it the same way.
///
/// One sentence for every such refusal, built here from the three words the
/// route supplies, so no two routes word the same fact differently:
/// `A role with the name "admin" already exists.`
#[derive(Debug, Clone, Copy)]
pub struct TakenKey<'a> {
    record: &'a str,
    field: &'a str,
    value: &'a str,
}

impl<'a> TakenKey<'a> {
    /// `record` is what the row is called ("role", "checkout preset"),
    /// `field` the unique field's user-facing name ("name", "slug"), and
    /// `value` what the write tried to set it to.
    pub const fn new(record: &'a str, field: &'a str, value: &'a str) -> Self {
        Self {
            record,
            field,
            value,
        }
    }

    /// The fact the 409 states: which record, which field, which value.
    pub fn taken(&self) -> String {
        let Self {
            record,
            field,
            value,
        } = self;
        format!("A {record} with the {field} \"{value}\" already exists.")
    }

    /// The 409 for a write whose value is taken, with `remedy` after the fact
    /// — what the caller can do about it.
    pub fn conflict_with(&self, remedy: &str) -> OutputStream {
        err_conflict(&format!("{} {remedy}", self.taken()))
    }

    /// The 409 for a write that tried to set the value itself: the fact, and
    /// the remedy that every such write shares.
    pub fn conflict(&self) -> OutputStream {
        self.conflict_with(&format!("Choose a different {}.", self.field))
    }
}

/// What a failed write that set `key` answers: [`TakenKey::conflict`] when the
/// database refused it as a duplicate, `otherwise(error)` for anything else.
///
/// The write's own error is the whole answer. Every `DatabaseService` this
/// workspace runs on reports a write that duplicates a primary or unique key
/// as `DatabaseError::AlreadyExists` — native SQLite and PostgreSQL from the
/// driver's code, Cloudflare D1 and the browser's sql.js from SQLite's text
/// ([`crate::sqlite_text_error::statement_error`]) — and the database
/// handler sends that on as [`ErrorCode::AlreadyExists`]. That is part of the
/// service contract, not a courtesy: an adapter that reports a duplicate as
/// anything else is a broken adapter, and each one's classification is
/// pinned by a test in its own crate. So nothing here re-reads the key to
/// find out what the write meant.
///
/// [`classify_db_error`] already answers a duplicate as a 409, with the
/// generic [`DUPLICATE_KEY`] because it cannot know which key a route wrote.
/// This is the same 409 naming the field, for the routes that do know.
/// `otherwise` is the route's own mapping for every other failure — a
/// `NotFound` that is the caller's 404, a domain refusal — which must still
/// end in `crud`'s classification of a database failure.
pub fn taken_key_or(
    error: wafer_run::WaferError,
    key: TakenKey<'_>,
    otherwise: impl FnOnce(wafer_run::WaferError) -> OutputStream,
) -> OutputStream {
    if error.code == ErrorCode::AlreadyExists {
        tracing::info!(
            error = %error,
            "database refused a write that duplicates a unique key",
        );
        return key.conflict();
    }
    otherwise(error)
}

/// [`taken_key_or`] for a write with no failure of its own to classify: any
/// other error is [`db_error_internal`]'s, under the caller's own `context`
/// ("Failed to create bucket"), so an operator reading the log still knows
/// which write failed.
pub fn taken_key_or_db_error(
    error: wafer_run::WaferError,
    key: TakenKey<'_>,
    context: &str,
) -> OutputStream {
    taken_key_or(error, key, |error| db_error_internal(error, context))
}

/// Response body of every CRUD delete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Deleted {
    /// Always `true`: a delete that did not happen is an error response.
    pub deleted: bool,
}

impl Deleted {
    /// The one value this type ever carries.
    pub const fn done() -> Self {
        Self { deleted: true }
    }
}

/// The value the block's route table bound to `{var}`, or the 400 an empty
/// binding turns into. The matcher never binds an empty segment, so the
/// guard only fires for a handler called with a message that did not go
/// through the table.
///
/// `missing` is the whole 400 message, not a noun to be formatted: the noun
/// is per-route (`"Missing bucket name"`, `"Missing setting key"`,
/// `"Missing offer ID"`) and deriving it from a label would be a mapping
/// layer that has to be read to be understood. [`path_id`] is the one
/// spelling common enough to be worth a shorthand.
pub fn path_var<'m>(msg: &'m Message, var: &str, missing: &str) -> Result<&'m str, OutputStream> {
    let value = msg.var(var);
    if value.is_empty() {
        return Err(err_bad_request(missing));
    }
    Ok(value)
}

/// A filter query parameter whose values are a closed set, as the enum that
/// defines them — or the 400 a value outside the set turns into.
///
/// An absent parameter is `Ok(None)`, which every caller reads as "no
/// filter". A value the enum does not define is refused rather than handed
/// to the database as a literal that matches no row: `?role=bot` and
/// `?type=cookies` used to answer `200` with an empty page, which reads as
/// "there are none of those" and is a different sentence from "there is no
/// such thing". serde's own unknown-variant text names the variants, so the
/// 400 lists them without any call site spelling them a second time.
pub fn enum_query<T: DeserializeOwned>(
    msg: &Message,
    param: &str,
) -> Result<Option<T>, OutputStream> {
    let raw = msg.query(param);
    if raw.is_empty() {
        return Ok(None);
    }
    serde_json::from_value::<T>(serde_json::Value::String(raw.to_string()))
        .map(Some)
        .map_err(|e| err_bad_request(&format!("Invalid `{param}` filter: {e}")))
}

/// The record id for a CRUD route — [`path_var`] on `{id}`, with the message
/// the great majority of routes want (`"Missing product ID"` for a label of
/// `"Product"`).
pub fn path_id<'m>(msg: &'m Message, not_found_label: &str) -> Result<&'m str, OutputStream> {
    path_var(
        msg,
        "id",
        &format!("Missing {} ID", not_found_label.to_lowercase()),
    )
}

// ---------------------------------------------------------------------------
// Typed primitives
// ---------------------------------------------------------------------------

/// Deserialize the request body into `T`, or the 400 a malformed body turns
/// into. The error text names the serde failure so a client learns which
/// field was wrong.
pub async fn read_json_body<T: DeserializeOwned>(input: InputStream) -> Result<T, OutputStream> {
    read_json_body_or(input, |detail| {
        err_bad_request(&format!("Invalid body: {detail}"))
    })
    .await
}

/// [`read_json_body`] for a block that must build the 400 itself.
///
/// `on_error` receives the serde failure text and returns the refusal to send.
/// The dev sandbox is the caller that needs this: every `/b/dev` response —
/// the refusals included — has to carry `Cache-Control: no-store`, which
/// [`err_bad_request`]'s plain error terminal does not. Parameterizing the
/// error here keeps one body reader rather than a second copy of it in the
/// block.
pub async fn read_json_body_or<T, F>(input: InputStream, on_error: F) -> Result<T, OutputStream>
where
    T: DeserializeOwned,
    F: FnOnce(String) -> OutputStream,
{
    let raw = input
        .collect_to_bytes()
        .await
        .map_err(OutputStream::error)?;
    serde_json::from_slice(&raw).map_err(|e| on_error(e.to_string()))
}

/// One page of `collection`, with caller-supplied filters and sort (`None` =
/// newest first by `created_at`).
pub async fn list_page(
    ctx: &dyn Context,
    collection: &str,
    page: i64,
    page_size: i64,
    filters: Vec<Filter>,
    sort: Option<Vec<SortField>>,
) -> Result<RecordList, OutputStream> {
    let sort = sort.unwrap_or_else(|| {
        vec![SortField {
            field: "created_at".to_string(),
            desc: true,
        }]
    });
    db::paginated_list(ctx, collection, page, page_size, filters, sort)
        .await
        .map_err(|e| db_error_internal(e, "Database error"))
}

/// Fetch `id` from `collection`, mapping a missing row to a 404 labelled
/// `not_found_label`.
pub async fn get_record(
    ctx: &dyn Context,
    collection: &str,
    id: &str,
    not_found_label: &str,
) -> Result<Record, OutputStream> {
    db::get(ctx, collection, id)
        .await
        .map_err(|e| db_error(e, &format!("{not_found_label} not found"), "Database error"))
}

/// Insert `data` into `collection`, stamping `created_at` / `updated_at`
/// when the caller did not, and return the row as stored.
///
/// `db::create` hands back the map it was given plus the id — not the row.
/// Every column the caller omitted and the table defaulted (`currency`,
/// `current_version`, `metadata`, …) is absent from that map, so a view
/// projected from it would report the zero value where the database holds
/// the default. `db::update` already re-fetches by id; this does the same so
/// a create response and a subsequent read describe the same row.
pub async fn create_record(
    ctx: &dyn Context,
    collection: &str,
    mut data: HashMap<String, serde_json::Value>,
) -> Result<Record, OutputStream> {
    stamp_created(&mut data);
    let created = db::create(ctx, collection, data)
        .await
        .map_err(|e| db_error_internal(e, "Database error"))?;
    db::get(ctx, collection, &created.id)
        .await
        .map_err(|e| db_error_internal(e, "Database error"))
}

/// Apply `data` to `id` in `collection`, stamping `updated_at`; a missing
/// row is a 404 labelled `not_found_label`.
pub async fn update_record(
    ctx: &dyn Context,
    collection: &str,
    id: &str,
    mut data: HashMap<String, serde_json::Value>,
    not_found_label: &str,
) -> Result<Record, OutputStream> {
    stamp_updated(&mut data);
    db::update(ctx, collection, id, data)
        .await
        .map_err(|e| db_error(e, &format!("{not_found_label} not found"), "Database error"))
}

/// Delete `id` from `collection`; a missing row is a 404 labelled
/// `not_found_label`.
pub async fn delete_record(
    ctx: &dyn Context,
    collection: &str,
    id: &str,
    not_found_label: &str,
) -> Result<Deleted, OutputStream> {
    db::delete(ctx, collection, id)
        .await
        .map(|()| Deleted::done())
        .map_err(|e| db_error(e, &format!("{not_found_label} not found"), "Database error"))
}

// ---------------------------------------------------------------------------
// Owner-scoped CRUD helpers
// ---------------------------------------------------------------------------

/// Identifies an owner-scoped resource for the `*_owned` helpers.
///
/// Owner-scoped resources are user-facing rows where access requires the
/// requesting user to match the row's owner column (e.g. a user's own
/// products or groups). The record is the `{id}` the route table bound.
pub struct OwnedResource<'a> {
    /// Table the records live in.
    pub collection: &'a str,
    /// Column holding the owning user's id (e.g. `"created_by"`).
    pub owner_field: &'a str,
    /// Human-readable label for error messages (e.g. `"Product"`).
    pub label: &'a str,
}

/// Fetch `id` from `collection` and verify `record[owner_field] == user_id`.
///
/// Returns the record on success. On failure returns a ready-to-send error
/// response: 401 for unauthenticated callers, 404 for both "row missing" and
/// "row owned by someone else" (existence must not leak to non-owners), and
/// whatever [`db_error`] makes of the database failure (403 for a WRAP
/// refusal, 500 for the rest).
pub async fn verify_owner(
    ctx: &dyn Context,
    collection: &str,
    id: &str,
    owner_field: &str,
    user_id: &str,
    not_found_label: &str,
) -> Result<Record, OutputStream> {
    if user_id.is_empty() {
        return Err(err_unauthenticated("Not authenticated"));
    }
    match db::get(ctx, collection, id).await {
        Ok(record) => {
            if field_as_string(&record, owner_field) != user_id {
                return Err(err_not_found(&format!("{not_found_label} not found")));
            }
            Ok(record)
        }
        Err(e) => Err(db_error(
            e,
            &format!("{not_found_label} not found"),
            "Database error",
        )),
    }
}

/// The owner-scoped record named by the path, after the ownership check.
pub async fn get_owned(
    ctx: &dyn Context,
    msg: &Message,
    res: &OwnedResource<'_>,
) -> Result<Record, OutputStream> {
    let id = path_id(msg, res.label)?;
    verify_owner(
        ctx,
        res.collection,
        id,
        res.owner_field,
        msg.user_id(),
        res.label,
    )
    .await
}

/// Apply `data` to the owner-scoped record named by the path, after the
/// ownership check, stamping `updated_at`.
pub async fn update_owned(
    ctx: &dyn Context,
    msg: &Message,
    res: &OwnedResource<'_>,
    data: HashMap<String, serde_json::Value>,
) -> Result<Record, OutputStream> {
    let id = path_id(msg, res.label)?.to_string();
    verify_owner(
        ctx,
        res.collection,
        &id,
        res.owner_field,
        msg.user_id(),
        res.label,
    )
    .await?;
    update_record(ctx, res.collection, &id, data, res.label).await
}

/// Delete the owner-scoped record named by the path, after the ownership
/// check.
pub async fn delete_owned(
    ctx: &dyn Context,
    msg: &Message,
    res: &OwnedResource<'_>,
) -> Result<Deleted, OutputStream> {
    let id = path_id(msg, res.label)?.to_string();
    verify_owner(
        ctx,
        res.collection,
        &id,
        res.owner_field,
        msg.user_id(),
        res.label,
    )
    .await?;
    delete_record(ctx, res.collection, &id, res.label).await
}

#[cfg(test)]
mod db_error_tests {
    use wafer_core::clients::database as db;
    use wafer_run::WaferError;

    use super::*;
    use crate::test_support::{output_http_status, TestContext};

    fn wafer_err(code: ErrorCode, message: &str) -> WaferError {
        WaferError::new(code, message)
    }

    #[tokio::test]
    async fn db_error_maps_not_found_to_404() {
        let out = db_error(
            wafer_err(ErrorCode::NotFound, "row 7 is not there"),
            "Product not found",
            "Database error",
        );
        assert_eq!(output_http_status(out).await, 404);
    }

    /// The behaviour fix. A WRAP row-guard denial is a `PermissionDenied`
    /// from the database client; every hand-written mapping in the tree
    /// falls through to `err_internal`, so a missing grant reaches the
    /// client as `500 Internal server error (ref: …)`.
    #[tokio::test]
    async fn db_error_maps_permission_denied_to_403() {
        let out = db_error(
            wafer_err(
                ErrorCode::PermissionDenied,
                "WRAP: block 'impresspress/products' has no grant for the table it read",
            ),
            "Product not found",
            "Database error",
        );
        assert_eq!(output_http_status(out).await, 403);
    }

    /// …and the denial's own message names the missing grant and the table,
    /// which is deployment topology. It is logged, not published.
    #[tokio::test]
    async fn the_403_does_not_republish_the_wrap_error_text() {
        let out = db_error(
            wafer_err(
                ErrorCode::PermissionDenied,
                "WRAP: no grant for secret_table",
            ),
            "Product not found",
            "Database error",
        );
        match out.collect_buffered().await {
            Err(wafer_run::TerminalNotResponse::Error(e)) => {
                assert!(
                    !e.message.contains("secret_table") && !e.message.contains("WRAP"),
                    "403 body must not carry the denial detail, got {:?}",
                    e.message
                );
            }
            other => panic!("expected an error terminal, got {other:?}"),
        }
    }

    /// `db_error_internal` is the same classification MINUS the 404: a
    /// `NotFound` from a call the block addressed names no row of the
    /// caller's, so it is a 500, not their missing row.
    #[tokio::test]
    async fn db_error_internal_keeps_a_not_found_a_500_but_still_403s_a_denial() {
        let not_found = db_error_internal(
            wafer_err(ErrorCode::NotFound, "record not found"),
            "Database error",
        );
        assert_eq!(output_http_status(not_found).await, 500);

        let denied = db_error_internal(
            wafer_err(ErrorCode::PermissionDenied, "WRAP: no grant"),
            "Database error",
        );
        assert_eq!(output_http_status(denied).await, 403);
    }

    /// The statement budget's two refusals reach the client with their
    /// detail code — the body's `code`, the only thing telling this 429
    /// from a rate limit's — and the over-the-limit one is the client's 400,
    /// not a sanitized 500.
    #[tokio::test]
    async fn db_error_keeps_the_statement_budget_detail_code() {
        use wafer_block::wire::database::{
            STATEMENT_BUDGET_EXCEEDS_LIMIT, STATEMENT_BUDGET_EXHAUSTED,
        };
        for (code, detail, status) in [
            (
                ErrorCode::ResourceExhausted,
                STATEMENT_BUDGET_EXHAUSTED,
                429,
            ),
            (
                ErrorCode::InvalidArgument,
                STATEMENT_BUDGET_EXCEEDS_LIMIT,
                400,
            ),
        ] {
            let out = db_error_internal(
                wafer_err(
                    code,
                    "batch runs 2 statements; this invocation has 1 of its 1000 left",
                )
                .with_detail_code(detail),
                "Database error",
            );
            match out.collect_buffered().await {
                Err(wafer_run::TerminalNotResponse::Error(e)) => {
                    assert_eq!(
                        wafer_block::http_codec::resolve_error_status(&e),
                        status,
                        "{detail}"
                    );
                    assert_eq!(e.detail_code(), Some(detail));
                    assert!(e.message.contains("1 of its 1000 left"), "{}", e.message);
                }
                other => panic!("expected an error terminal, got {other:?}"),
            }
        }

        // Any other `InvalidArgument` is a statement this repo built wrong.
        let other = db_error_internal(
            wafer_err(ErrorCode::InvalidArgument, "bad filter"),
            "Database error",
        );
        assert_eq!(output_http_status(other).await, 500);
    }

    /// A fragment whose read the budget refused says so, not "access denied"
    /// or "try later".
    #[test]
    fn a_budget_refusal_notice_names_the_budget() {
        use wafer_block::wire::database::{
            STATEMENT_BUDGET_EXCEEDS_LIMIT, STATEMENT_BUDGET_EXHAUSTED,
        };
        for (code, detail) in [
            (ErrorCode::ResourceExhausted, STATEMENT_BUDGET_EXHAUSTED),
            (ErrorCode::InvalidArgument, STATEMENT_BUDGET_EXCEEDS_LIMIT),
        ] {
            assert_eq!(
                db_error_notice(wafer_err(code, "x").with_detail_code(detail), "ctx"),
                "it needs more database work than one request may do"
            );
        }
        assert_eq!(
            db_error_notice(wafer_err(ErrorCode::ResourceExhausted, "quota"), "ctx"),
            "it is over its usage limit right now"
        );
    }

    #[tokio::test]
    async fn db_error_keeps_resource_exhausted_at_429() {
        let out = db_error(
            wafer_err(ErrorCode::ResourceExhausted, "storage quota exceeded"),
            "Object not found",
            "Database error",
        );
        assert_eq!(output_http_status(out).await, 429);
    }

    #[tokio::test]
    async fn db_error_sanitizes_everything_else_into_a_500() {
        let out = db_error(
            wafer_err(ErrorCode::Internal, "connection reset by peer"),
            "Product not found",
            "Database error",
        );
        match out.collect_buffered().await {
            Err(wafer_run::TerminalNotResponse::Error(e)) => {
                assert_eq!(wafer_block::http_codec::resolve_error_status(&e), 500);
                assert!(
                    e.message.starts_with("Internal server error (ref: "),
                    "500 body must be the sanitized form, got {:?}",
                    e.message
                );
            }
            other => panic!("expected an error terminal, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------
    // End to end: the same denial arriving through the CRUD primitives.
    // ---------------------------------------------------------------------

    /// A table this test owns, so `tests/repo_door.rs` does not see the
    /// fixture as `crud.rs` reaching past another block's door.
    const FOREIGN_TABLE: &str = "impresspress__crudtest__rows";

    /// A context acting as a block with NO grants, so every typed database
    /// call it makes is refused by the same `wrap::check_access` the runtime
    /// applies.
    async fn denied_ctx() -> TestContext {
        TestContext::new().await.running_as("test/ungranted")
    }

    /// Under STRICT_SCHEMA — what every Cloudflare deploy runs — a statement
    /// against a table that does not exist reaches native SQLite and fails.
    /// The routes that name a row (`get_record`, `update_record`,
    /// `delete_record`) must answer that as the fault it is, a 500, not as
    /// the caller's missing row. A real missing table, not a hand-built
    /// error: the fixture never creates it.
    #[tokio::test]
    async fn a_missing_table_under_strict_schema_is_a_500_not_the_callers_404() {
        const NEVER_CREATED: &str = "impresspress__crudtest__never_created";
        // Run as the block the table would belong to, so the only thing
        // wrong with each statement is the table it names.
        let ctx = TestContext::new().await.running_as("impresspress/crudtest");
        ctx.set_strict_schema(true);

        let got = get_record(&ctx, NEVER_CREATED, "any-id", "Row")
            .await
            .expect_err("the read fails");
        assert_eq!(output_http_status(got).await, 500, "get_record");

        let updated = update_record(&ctx, NEVER_CREATED, "any-id", HashMap::new(), "Row")
            .await
            .expect_err("the write fails");
        assert_eq!(output_http_status(updated).await, 500, "update_record");

        let deleted = delete_record(&ctx, NEVER_CREATED, "any-id", "Row")
            .await
            .expect_err("the delete fails");
        assert_eq!(output_http_status(deleted).await, 500, "delete_record");
    }

    #[tokio::test]
    async fn a_denied_read_through_get_record_is_403_not_500() {
        let ctx = denied_ctx().await;
        let out = get_record(&ctx, FOREIGN_TABLE, "any-id", "User")
            .await
            .expect_err("WRAP denies the read");
        assert_eq!(output_http_status(out).await, 403);
    }

    #[tokio::test]
    async fn a_denied_list_through_list_page_is_403_not_500() {
        let ctx = denied_ctx().await;
        let out = list_page(&ctx, FOREIGN_TABLE, 1, 10, Vec::new(), None)
            .await
            .expect_err("WRAP denies the list");
        assert_eq!(output_http_status(out).await, 403);
    }

    #[tokio::test]
    async fn a_denied_write_through_create_record_is_403_not_500() {
        let ctx = denied_ctx().await;
        let out = create_record(&ctx, FOREIGN_TABLE, HashMap::new())
            .await
            .expect_err("WRAP denies the write");
        assert_eq!(output_http_status(out).await, 403);
    }

    #[tokio::test]
    async fn a_denied_write_through_update_record_is_403_not_500() {
        let ctx = denied_ctx().await;
        let out = update_record(&ctx, FOREIGN_TABLE, "any-id", HashMap::new(), "User")
            .await
            .expect_err("WRAP denies the write");
        assert_eq!(output_http_status(out).await, 403);
    }

    #[tokio::test]
    async fn a_denied_delete_through_delete_record_is_403_not_500() {
        let ctx = denied_ctx().await;
        let out = delete_record(&ctx, FOREIGN_TABLE, "any-id", "User")
            .await
            .expect_err("WRAP denies the delete");
        assert_eq!(output_http_status(out).await, 403);
    }

    #[tokio::test]
    async fn a_denied_read_through_verify_owner_is_403_not_500() {
        let ctx = denied_ctx().await;
        let out = verify_owner(
            &ctx,
            FOREIGN_TABLE,
            "any-id",
            "created_by",
            "user-1",
            "User",
        )
        .await
        .expect_err("WRAP denies the read");
        assert_eq!(output_http_status(out).await, 403);
    }

    /// The grant path still answers as it did: a caller that may read the
    /// table gets the 404 a missing row deserves, so the 403 above is the
    /// denial and not a blanket refusal.
    #[tokio::test]
    async fn a_granted_read_of_a_missing_row_is_still_404() {
        let mut ctx = TestContext::new().await;
        ctx.add_deployment_grants(vec![wafer_run::ResourceGrant::read(
            "test/granted",
            FOREIGN_TABLE,
        )]);
        db::ensure_table(
            &ctx.fixture(),
            &wafer_block::wire::database::TableDef {
                name: FOREIGN_TABLE.to_string(),
                columns: vec![wafer_block::wire::database::ColumnDef {
                    name: "id".to_string(),
                    kind: "text".to_string(),
                    nullable: false,
                    primary_key: true,
                    auto_increment: false,
                    unique: false,
                    default: None,
                }],
                indexes: vec![],
                primary_key: vec![],
                unique_keys: vec![],
            },
        )
        .await
        .expect("the ungated fixture creates its table");
        let out = get_record(
            &ctx.running_as("test/granted"),
            FOREIGN_TABLE,
            "no-such-id",
            "Row",
        )
        .await
        .expect_err("the row does not exist");
        assert_eq!(output_http_status(out).await, 404);
    }
}

#[cfg(test)]
mod path_var_tests {
    use super::*;
    use crate::test_support::output_http_status;

    fn msg_with(var: &str, value: &str) -> Message {
        let mut m = Message::new("http.request");
        m.set_meta(format!("req.param.{var}"), value);
        m
    }

    #[test]
    fn a_bound_segment_is_its_value() {
        let m = msg_with("offer_id", "off_1");
        assert_eq!(
            path_var(&m, "offer_id", "Missing offer ID").ok(),
            Some("off_1")
        );
        let m = msg_with("id", "prod_1");
        assert_eq!(path_id(&m, "Product").ok(), Some("prod_1"));
    }

    #[tokio::test]
    async fn an_unbound_segment_is_a_400_carrying_the_caller_s_message() {
        let m = Message::new("http.request");
        let out = path_var(&m, "offer_id", "Missing offer ID").expect_err("no binding");
        match out.collect_buffered().await {
            Err(wafer_run::TerminalNotResponse::Error(e)) => {
                assert_eq!(e.message, "Missing offer ID");
            }
            other => panic!("expected an error terminal, got {other:?}"),
        }
        let out = path_var(&Message::new("http.request"), "id", "Missing product ID")
            .expect_err("no binding");
        assert_eq!(output_http_status(out).await, 400);
    }

    /// `path_id` produces exactly the message the hand-rolled guards it
    /// replaces spelled, so converting them changes no wire text.
    #[tokio::test]
    async fn path_id_spells_the_message_the_hand_rolled_guards_spelled() {
        for (label, expected) in [
            ("Product", "Missing product ID"),
            ("Seller", "Missing seller ID"),
            ("User", "Missing user ID"),
            ("Grant", "Missing grant ID"),
        ] {
            let out = path_id(&Message::new("http.request"), label).expect_err("no binding");
            match out.collect_buffered().await {
                Err(wafer_run::TerminalNotResponse::Error(e)) => assert_eq!(e.message, expected),
                other => panic!("expected an error terminal, got {other:?}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use wafer_run::{streams::output::TerminalNotResponse, ErrorCode, WaferError};

    use super::{
        db_error, db_error_internal, taken_key_or, taken_key_or_db_error, TakenKey, DUPLICATE_KEY,
    };

    /// A duplicate key is a 409 from both doors, with the driver's text —
    /// which names the table and the column — replaced. Folded into the
    /// 500 as it used to be, every route that creates a row under a unique
    /// key without naming that key answered a re-typed name with "Internal
    /// server error".
    #[tokio::test]
    async fn a_duplicate_key_is_a_sanitized_409_from_every_door() {
        let driver = || {
            WaferError::new(
                ErrorCode::AlreadyExists,
                "unique constraint violated: UNIQUE constraint failed: impresspress__llm__providers.name",
            )
        };
        for (door, out) in [
            (
                "db_error_internal",
                db_error_internal(driver(), "Database error"),
            ),
            (
                "db_error",
                db_error(driver(), "Row not found", "Database error"),
            ),
        ] {
            match out.collect_buffered().await {
                Err(TerminalNotResponse::Error(e)) => {
                    assert_eq!(e.code, ErrorCode::AlreadyExists, "{door}");
                    assert_eq!(e.message, DUPLICATE_KEY, "{door}");
                }
                other => panic!("{door}: expected the 409, got {other:?}"),
            }
        }
    }

    /// A route that knows the key says which: the same 409, naming the
    /// record, the field and the value, and nothing of the driver's text.
    #[tokio::test]
    async fn a_duplicate_key_is_the_named_field_conflict() {
        let out = taken_key_or_db_error(
            WaferError::new(
                ErrorCode::AlreadyExists,
                "UNIQUE constraint failed: impresspress__admin__roles.name",
            ),
            TakenKey::new("role", "name", "editor"),
            "Database error",
        );
        match out.collect_buffered().await {
            Err(TerminalNotResponse::Error(e)) => {
                assert_eq!(e.code, ErrorCode::AlreadyExists);
                assert_eq!(
                    e.message,
                    "A role with the name \"editor\" already exists. Choose a different name."
                );
            }
            other => panic!("expected the named 409, got {other:?}"),
        }
    }

    /// `taken_key_or` hands every other failure to the route's own mapping,
    /// untouched, so a route's 404 stays its 404.
    #[tokio::test]
    async fn taken_key_or_leaves_other_failures_to_the_route() {
        let out = taken_key_or(
            WaferError::new(ErrorCode::NotFound, "no row"),
            TakenKey::new("role", "name", "editor"),
            |error| db_error(error, "Role not found", "Database error"),
        );
        assert_eq!(crate::test_support::output_http_status(out).await, 404);
    }

    /// Nothing but `AlreadyExists` is a duplicate. An `Internal` is a fault
    /// whatever the key's state — the classification is the adapter's job,
    /// and nothing here re-reads the key to second-guess it — and a WRAP
    /// refusal keeps the 403 `crud` gives it.
    #[tokio::test]
    async fn only_already_exists_is_a_conflict() {
        let internal = taken_key_or_db_error(
            WaferError::new(ErrorCode::Internal, "disk I/O error"),
            TakenKey::new("role", "name", "editor"),
            "Database error",
        );
        assert_eq!(crate::test_support::output_http_status(internal).await, 500);

        let denied = taken_key_or_db_error(
            WaferError::new(ErrorCode::PermissionDenied, "denied"),
            TakenKey::new("role", "name", "editor"),
            "Database error",
        );
        assert_eq!(crate::test_support::output_http_status(denied).await, 403);
    }
}
