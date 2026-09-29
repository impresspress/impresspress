//! Row-level access over `wafer_run__auth__users`.

use std::collections::HashMap;

use serde_json::{json, Value};
use uuid::Uuid;
use wafer_block::{
    db::{Filter, FilterOp, FilterTree, ListOptions, SortField},
    wire::database as wire,
};
use wafer_core::clients::database::{self as db, Record};
use wafer_run::{context::Context, WaferError};

use super::{db_failed, internal_error, map_bool, map_opt_str, map_str, now_iso};
use crate::util::{daily_grouped, to_wire_filters, RecordExt};

pub const TABLE: &str = "wafer_run__auth__users";

/// Column, JWT-claim, and cache-key name for the per-user auth-version
/// counter (P2c: CODE_REVIEW_2026-07-16, "Access JWTs outlive account and
/// role changes"). Kept as a single constant so the users-table column
/// (migration `009_auth_version`), the access-JWT claim
/// (`blocks::auth::helpers::generate_tokens`, from [`UserRow::auth_version`]), and the verify-side cache key
/// (`blocks::auth::current_auth_version`) can't drift onto different
/// literals.
pub const AUTH_VERSION_FIELD: &str = "auth_version";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRow {
    pub id: String,
    pub email: String,
    pub display_name: String,
    /// The `name` column migration 006 added. [`insert`] and
    /// [`update_profile`] dual-write it with `display_name`; admin's
    /// `patch_admin_fields` writes it alone, so the two can differ on an
    /// account an admin has renamed. Published as `AdminUserView.name`.
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub role: String,
    pub disabled: bool,
    /// Soft-delete timestamp (ISO-8601). `None`/empty when the account is live.
    /// Column exists since migration 006; a soft-deleted account keeps its row
    /// but must not authenticate.
    pub deleted_at: Option<String>,
    pub email_verified: bool,
    /// What proved the address, if anything ever did: `"email_token"` for a
    /// redeemed verification link, `"oauth.<provider>"` for a provider
    /// assertion. `None`/empty means nobody proved it.
    ///
    /// Separate from [`email_verified`](Self::email_verified), which is a
    /// policy flag: signup writes it `!REQUIRE_VERIFICATION`, so on a
    /// deployment that does not require verification it says only that the
    /// deployment does not require verification. Anything that needs evidence
    /// of mailbox control — currently the OAuth callback's account-adoption
    /// rule — reads THIS field, never that one.
    pub email_verified_by: Option<String>,
    /// Stamped by [`touch_last_login`] on every successful sign-in; `None`
    /// for an account that has never signed in.
    pub last_login_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// The row's [`AUTH_VERSION_FIELD`], read with the rest of it. A token
    /// minted for this account carries this value, not one read later: it is
    /// the version the facts on this row were current at
    /// (`blocks::auth::helpers::TokenGrant`). `0` when the read did not carry
    /// the column, which only makes a token minted from it stale sooner.
    pub auth_version: i64,
}

impl UserRow {
    /// True when the account has been soft-deleted (a non-empty `deleted_at`).
    /// Treats both SQL `NULL`/absent and an empty string as "not deleted".
    pub fn is_deleted(&self) -> bool {
        self.deleted_at.as_deref().is_some_and(|s| !s.is_empty())
    }

    /// True when the account may authenticate: neither disabled nor
    /// soft-deleted. The single lifecycle-state predicate shared by every
    /// credential-verification path.
    pub fn is_active(&self) -> bool {
        !self.disabled && !self.is_deleted()
    }

    /// True when someone actually proved control of this address — a redeemed
    /// verification link or a provider assertion — rather than the deployment
    /// merely not asking for one. See
    /// [`email_verified_by`](Self::email_verified_by).
    pub fn email_is_proven(&self) -> bool {
        self.email_verified_by
            .as_deref()
            .is_some_and(|by| !by.is_empty())
    }
}

#[derive(Debug, Clone)]
pub struct NewUser {
    pub email: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    /// The account's initial role, written to the inline `users.role`
    /// column. This — not a `user_roles` row — is what makes a
    /// bootstrap-admin signup an admin: `helpers::get_user_roles` reads the
    /// inline column first and merges the grant rows onto it, so a
    /// `user_roles` row means "granted beyond the initial role" (spec
    /// 2.2.3). Neither signup path writes one.
    pub role: String,
    /// Whether the address is already verified. `true` for a bootstrapped
    /// admin (the operator who set the password) and for a signup on a
    /// deployment that does not require verification; `false` when a
    /// verification mail is about to go out.
    pub email_verified: bool,
    /// `sha256_hex` of the email-verification token, when one was minted.
    /// Carried on the insert so signup does not have to follow up with a
    /// second `UPDATE` on the row it just created.
    pub verification_token_hash: Option<String>,
}

fn row_from(id: String, m: &HashMap<String, Value>) -> Result<UserRow, WaferError> {
    Ok(UserRow {
        id,
        email: map_opt_str(m, "email").ok_or_else(|| internal_error("missing email"))?,
        display_name: map_str(m, "display_name"),
        name: map_opt_str(m, "name"),
        avatar_url: map_opt_str(m, "avatar_url"),
        role: map_opt_str(m, "role").unwrap_or_else(|| "user".into()),
        disabled: map_bool(m, "disabled"),
        deleted_at: map_opt_str(m, "deleted_at"),
        email_verified: map_bool(m, "email_verified"),
        email_verified_by: map_opt_str(m, "email_verified_by"),
        last_login_at: map_opt_str(m, "last_login_at"),
        created_at: map_str(m, "created_at"),
        updated_at: map_str(m, "updated_at"),
        auth_version: m.i64_field(AUTH_VERSION_FIELD),
    })
}

fn row_from_map(m: &HashMap<String, Value>) -> Result<UserRow, WaferError> {
    let id = map_opt_str(m, "id").ok_or_else(|| internal_error("missing id"))?;
    row_from(id, m)
}

/// Decode a listed row. A `columns`-projected `SELECT` need not carry the
/// `id` column in `data`, but the envelope always does, so the envelope is
/// the fallback.
fn row_from_record(rec: &Record) -> Result<UserRow, WaferError> {
    let id = map_opt_str(&rec.data, "id").unwrap_or_else(|| rec.id.clone());
    if id.is_empty() {
        return Err(internal_error("missing id"));
    }
    row_from(id, &rec.data)
}

/// The live-account predicate: `deleted_at IS NULL`. Every list, count and
/// aggregate over this table that must not see soft-deleted accounts builds
/// its filters from this one function — the five hand-written copies it
/// replaced were the reason a new admin surface could forget it.
fn active_filter() -> Filter {
    Filter {
        field: "deleted_at".to_string(),
        operator: FilterOp::IsNull,
        value: Value::Null,
    }
}

fn like_filter(field: &str, pattern: &str) -> Filter {
    Filter {
        field: field.to_string(),
        operator: FilterOp::Like,
        value: json!(pattern),
    }
}

/// Newest account first. `created_at` has one-second precision, so accounts
/// created in the same second tie on it; the database breaks the tie on the
/// primary key, `id`, in the direction of this sort (a sorted `list` ends its
/// `ORDER BY` with the table's key, and this table has one — `id TEXT PRIMARY
/// KEY`, `001_auth_schema.sqlite.sql`), which makes the order total — what
/// paging needs. [`insert`] mints `id` as a UUIDv7, which within one process
/// orders by creation, so there the tie also resolves newest first; ids
/// minted in the same millisecond by different processes (two Workers
/// isolates) order at random.
fn newest_first() -> Vec<SortField> {
    vec![SortField {
        field: "created_at".to_string(),
        desc: true,
    }]
}

/// The form every address is stored and looked up in: trimmed and
/// lowercased. `users.email` is UNIQUE byte for byte, so an address written
/// in any other form cannot be found by the lookups that normalize theirs,
/// and the same mailbox can then hold a second account.
pub fn normalize_email(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// The row [`insert`] writes for `new`, keyed by the id it mints.
fn new_row(new: NewUser) -> (String, HashMap<String, Value>) {
    let id = Uuid::now_v7().to_string();
    let now = now_iso();
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("id".into(), json!(id));
    data.insert("email".into(), json!(new.email));
    data.insert("display_name".into(), json!(new.display_name));
    data.insert("name".into(), json!(new.display_name));
    if let Some(a) = new.avatar_url.as_deref() {
        data.insert("avatar_url".into(), json!(a));
    }
    data.insert("role".into(), json!(new.role));
    data.insert("email_verified".into(), json!(new.email_verified));
    if let Some(hash) = new.verification_token_hash.as_deref() {
        data.insert("verification_token".into(), json!(hash));
    }
    data.insert("created_at".into(), json!(now));
    data.insert("updated_at".into(), json!(now));
    (id, data)
}

/// Insert an account row alone. An account created for someone to sign in
/// to needs a way in, written with it: [`insert_with_password`] for a
/// password account, [`insert_with_ops`] for anything else.
pub async fn insert(ctx: &dyn Context, new: NewUser) -> Result<UserRow, WaferError> {
    let (_, data) = new_row(new);
    let rec = db::create(ctx, TABLE, data)
        .await
        .map_err(|e| db_failed("insert", e))?;
    row_from_map(&rec.data)
}

/// Insert an account row and the rows `companions` builds for its id, as one
/// atomic write: every row lands, or — when any of them fails — none does.
///
/// Two separate writes leave an account without a way in whenever the
/// second one fails, and the address is then taken: every retry is told it
/// is already registered. `companions` gets the minted id so its rows can
/// point at the account they belong to.
///
/// A taken address fails the whole write with `AlreadyExists` (the
/// `users.email` UNIQUE constraint), and nothing is written.
pub async fn insert_with_ops(
    ctx: &dyn Context,
    new: NewUser,
    companions: impl FnOnce(&str) -> Vec<wire::BatchWrite>,
) -> Result<UserRow, WaferError> {
    let (id, data) = new_row(new);
    let mut ops = vec![wire::BatchWrite::Create {
        collection: TABLE.to_string(),
        data,
    }];
    ops.extend(companions(&id));
    let results = db::batch(ctx, ops)
        .await
        .map_err(|e| db_failed("insert with companion rows", e))?;
    match results.into_iter().next() {
        Some(wire::BatchWriteResult::Created(rec)) => row_from_map(&rec.data),
        other => Err(internal_error(format!(
            "account insert answered {other:?}, not the created row"
        ))),
    }
}

/// Insert a password account — the account row and its
/// `local_credentials` row — as one atomic write. See [`insert_with_ops`].
pub async fn insert_with_password(
    ctx: &dyn Context,
    new: NewUser,
    password_hash: &str,
) -> Result<UserRow, WaferError> {
    insert_with_ops(ctx, new, |id| {
        vec![super::local_credentials::create_op(
            id,
            password_hash,
            false,
        )]
    })
    .await
}

pub async fn find_by_email(ctx: &dyn Context, email: &str) -> Result<Option<UserRow>, WaferError> {
    use wafer_block::ErrorCode;
    match db::get_by_field(ctx, TABLE, "email", json!(email)).await {
        Ok(rec) => Ok(Some(row_from_map(&rec.data)?)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(db_failed("select by email", e)),
    }
}

/// Returns the number of rows currently in `wafer_run__auth__users`.
///
/// Used by the block's bootstrap logic to decide whether to create the first
/// admin user. A non-zero count means "already bootstrapped — no-op".
pub async fn count(ctx: &dyn Context) -> Result<u64, WaferError> {
    let n = db::count(ctx, TABLE, &[])
        .await
        .map_err(|e| db_failed("users count", e))?;
    Ok(n.max(0) as u64)
}

pub async fn find_by_id(ctx: &dyn Context, id: &str) -> Result<Option<UserRow>, WaferError> {
    use wafer_block::ErrorCode;
    match db::get(ctx, TABLE, id).await {
        Ok(rec) => Ok(Some(row_from_map(&rec.data)?)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(db_failed("select by id", e)),
    }
}

/// Read the `email_verified` flag for a user. Returns `Ok(false)` when the
/// user row is missing (the doc-claim path) and propagates other DB errors.
///
/// Accepts both SQLite TEXT-int (`'0'`/`'1'`), Postgres BOOLEAN, JSON `bool`,
/// and string `'true'`/`'false'` via `RecordExt::bool_field`.
pub async fn is_email_verified(ctx: &dyn Context, user_id: &str) -> Result<bool, WaferError> {
    use wafer_block::ErrorCode;

    use crate::util::RecordExt;

    match db::get(ctx, TABLE, user_id).await {
        Ok(r) => Ok(r.bool_field("email_verified")),
        Err(e) if e.code == ErrorCode::NotFound => Ok(false),
        Err(e) => Err(db_failed(&format!("get user {user_id}"), e)),
    }
}

/// Set the `email_verified` flag for a user. Stamps `updated_at` so admin
/// auditing reflects the change.
///
/// Stores the value as the JSON boolean — both `wafer-block-sqlite`
/// (TEXT-everything via JSON serialization) and `wafer-block-postgres`
/// (typed BOOLEAN) accept it. `RecordExt::bool_field` round-trips both.
pub async fn set_email_verified(
    ctx: &dyn Context,
    user_id: &str,
    verified: bool,
) -> Result<(), WaferError> {
    let mut data = std::collections::HashMap::new();
    data.insert("email_verified".to_string(), json!(verified));
    // Clearing the flag retracts the proof with it — a row nobody considers
    // verified must not still name what verified it. Setting the flag does
    // NOT mint one: only [`record_email_proof`] can, from a caller that
    // watched the proof happen.
    if !verified {
        data.insert("email_verified_by".to_string(), json!(""));
    }
    crate::util::stamp_updated(&mut data);

    db::update(ctx, TABLE, user_id, data)
        .await
        .map_err(|e| db_failed(&format!("set email_verified for {user_id}"), e))?;
    Ok(())
}

/// Find a user by the SHA-256 hex of their email-verification token.
///
/// The `verification_token` column stores `sha256_hex(raw)`; callers hash the
/// supplied raw token the same way before calling. Returns `Ok(None)` when no
/// row matches (the token is invalid/expired).
pub async fn find_by_verification_token(
    ctx: &dyn Context,
    token_hash: &str,
) -> Result<Option<UserRow>, WaferError> {
    use wafer_block::ErrorCode;
    match db::get_by_field(ctx, TABLE, "verification_token", json!(token_hash)).await {
        Ok(rec) => Ok(Some(row_from_map(&rec.data)?)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(db_failed("find by verification_token", e)),
    }
}

/// Record that someone proved control of this user's address, naming the
/// proof, and clear their `verification_token` in the same write. Stamps
/// `updated_at` with [`super::now_iso`].
///
/// With [`record_email_proof_op`], its batch form, the ONLY writer of
/// `email_verified_by`. Call either from a caller that has just watched the
/// proof happen — `auth_ui::api::verify` redeeming a mailed token
/// (`proof::EMAIL_TOKEN`), or `auth_ui::oauth::callback` receiving a verified
/// address from a provider that asserts one (`proof::oauth`). Setting the
/// policy flag alone is [`set_email_verified`], which deliberately cannot
/// manufacture a proof.
pub async fn record_email_proof(
    ctx: &dyn Context,
    user_id: &str,
    proof: &str,
) -> Result<(), WaferError> {
    db::update(ctx, TABLE, user_id, email_proof_data(proof))
        .await
        .map_err(|e| db_failed(&format!("record email proof for {user_id}"), e))?;
    Ok(())
}

/// [`record_email_proof`] as one write of a batch: for the OAuth sign-up
/// whose provider asserted the address, so the proof lands with the account
/// it is about.
pub fn record_email_proof_op(user_id: &str, proof: &str) -> wire::BatchWrite {
    wire::BatchWrite::Update {
        collection: TABLE.to_string(),
        id: user_id.to_string(),
        data: email_proof_data(proof),
    }
}

fn email_proof_data(proof: &str) -> HashMap<String, Value> {
    let mut data = HashMap::new();
    data.insert("email_verified".to_string(), json!(true));
    data.insert("email_verified_by".to_string(), json!(proof));
    data.insert("verification_token".to_string(), json!(""));
    data.insert("updated_at".to_string(), json!(now_iso()));
    data
}

/// The values [`record_email_proof`] writes. A proof names the act that
/// produced it, so a row can be read back and audited.
pub mod proof {
    /// A verification link mailed to the address was redeemed.
    pub const EMAIL_TOKEN: &str = "email_token";

    /// An OAuth provider that asserts verification returned this address.
    pub fn oauth(provider: &str) -> String {
        format!("oauth.{provider}")
    }
}

/// Read a user's `last_verification_sent` timestamp (the resend cooldown
/// anchor). Returns an empty string when unset/absent. `Ok(None)` would be
/// indistinguishable from "never sent" here, so absence collapses to `""`.
pub async fn last_verification_sent(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<String, WaferError> {
    use wafer_block::ErrorCode;

    use crate::util::RecordExt;

    match db::get(ctx, TABLE, user_id).await {
        Ok(r) => Ok(r.str_field("last_verification_sent").to_string()),
        Err(e) if e.code == ErrorCode::NotFound => Ok(String::new()),
        Err(e) => Err(db_failed("get last_verification_sent", e)),
    }
}

/// Store a freshly-minted email-verification token (its SHA-256 hex) and the
/// `last_verification_sent` cooldown timestamp. Stamps `updated_at`.
pub async fn set_verification_token(
    ctx: &dyn Context,
    user_id: &str,
    token_hash: &str,
    sent_at: &str,
) -> Result<(), WaferError> {
    let mut data = std::collections::HashMap::new();
    data.insert("verification_token".to_string(), json!(token_hash));
    data.insert("last_verification_sent".to_string(), json!(sent_at));
    data.insert("updated_at".to_string(), json!(now_iso()));
    db::update(ctx, TABLE, user_id, data)
        .await
        .map_err(|e| db_failed(&format!("set verification_token for {user_id}"), e))?;
    Ok(())
}

/// A user matched by their password-reset token, with the token's expiry so
/// the caller can reject expired tokens without a second read.
#[derive(Debug, Clone)]
pub struct ResetTokenUser {
    /// Stable user id.
    pub id: String,
    /// `reset_token_expires` column value (ISO-8601), empty if unset.
    pub reset_token_expires: String,
}

/// Find a user by the SHA-256 hex of their password-reset token.
///
/// The `reset_token` column stores `sha256_hex(raw)`. Returns the matched
/// user's id and the stored expiry so the handler can validate it in one
/// round-trip. `Ok(None)` when no row matches.
pub async fn find_by_reset_token(
    ctx: &dyn Context,
    token_hash: &str,
) -> Result<Option<ResetTokenUser>, WaferError> {
    use wafer_block::ErrorCode;

    use crate::util::RecordExt;

    match db::get_by_field(ctx, TABLE, "reset_token", json!(token_hash)).await {
        Ok(rec) => Ok(Some(ResetTokenUser {
            id: rec.id.clone(),
            reset_token_expires: rec.str_field("reset_token_expires").to_string(),
        })),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(db_failed("find by reset_token", e)),
    }
}

/// Store a password-reset token (its SHA-256 hex) and its absolute expiry.
/// Stamps `updated_at`.
pub async fn set_reset_token(
    ctx: &dyn Context,
    user_id: &str,
    token_hash: &str,
    expires_at: &str,
) -> Result<(), WaferError> {
    let mut data = std::collections::HashMap::new();
    data.insert("reset_token".to_string(), json!(token_hash));
    data.insert("reset_token_expires".to_string(), json!(expires_at));
    data.insert("updated_at".to_string(), json!(now_iso()));
    db::update(ctx, TABLE, user_id, data)
        .await
        .map_err(|e| db_failed(&format!("set reset_token for {user_id}"), e))?;
    Ok(())
}

/// Spend a password-reset token: clear `reset_token` and its expiry on
/// `user_id`'s row, but only while that row still carries `token_hash`.
/// Stamps `updated_at`.
///
/// The condition and the write are one statement, so of two requests
/// redeeming the same link only one sees `true`; the other finds the token
/// already spent and gets `false`. Callers spend the token before writing
/// anything it authorises, which is what makes a link single-use.
pub async fn take_reset_token(
    ctx: &dyn Context,
    user_id: &str,
    token_hash: &str,
) -> Result<bool, WaferError> {
    let filters = vec![
        Filter {
            field: "id".into(),
            operator: FilterOp::Equal,
            value: json!(user_id),
        },
        Filter {
            field: "reset_token".into(),
            operator: FilterOp::Equal,
            value: json!(token_hash),
        },
    ];
    let mut data = HashMap::new();
    data.insert("reset_token".to_string(), json!(""));
    data.insert("reset_token_expires".to_string(), json!(""));
    data.insert("updated_at".to_string(), json!(now_iso()));
    let n = db::update_by_filters_count(ctx, TABLE, filters, data)
        .await
        .map_err(|e| db_failed(&format!("take reset_token for {user_id}"), e))?;
    Ok(n > 0)
}

/// Put back a reset token [`take_reset_token`] spent, with its original
/// expiry, so a reset that failed part-way can be retried with the same link.
/// Stamps `updated_at`.
///
/// Only a row whose token is still clear is restored: a link requested since
/// the take (`set_reset_token` from forgot-password) is the newer one and is
/// kept. Returns whether the token was restored.
pub async fn restore_reset_token(
    ctx: &dyn Context,
    user_id: &str,
    token_hash: &str,
    expires_at: &str,
) -> Result<bool, WaferError> {
    let filters = vec![
        Filter {
            field: "id".into(),
            operator: FilterOp::Equal,
            value: json!(user_id),
        },
        Filter {
            field: "reset_token".into(),
            operator: FilterOp::Equal,
            value: json!(""),
        },
    ];
    let mut data = HashMap::new();
    data.insert("reset_token".to_string(), json!(token_hash));
    data.insert("reset_token_expires".to_string(), json!(expires_at));
    data.insert("updated_at".to_string(), json!(now_iso()));
    let n = db::update_by_filters_count(ctx, TABLE, filters, data)
        .await
        .map_err(|e| db_failed(&format!("restore reset_token for {user_id}"), e))?;
    Ok(n > 0)
}

/// Update a user's editable profile fields (`display_name`/`name` and
/// `avatar_url`) and return the refreshed row.
///
/// `name` writes BOTH `display_name` and the legacy `name` alias (the same
/// dual-write [`insert`] does) so the typed `UserRow` and the raw `name`
/// column stay in lockstep. `None` arguments leave the corresponding column
/// untouched. Stamps `updated_at`.
pub async fn update_profile(
    ctx: &dyn Context,
    user_id: &str,
    name: Option<&str>,
    avatar_url: Option<&str>,
) -> Result<UserRow, WaferError> {
    let mut data = std::collections::HashMap::new();
    if let Some(n) = name {
        data.insert("display_name".to_string(), json!(n));
        data.insert("name".to_string(), json!(n));
    }
    if let Some(a) = avatar_url {
        data.insert("avatar_url".to_string(), json!(a));
    }
    data.insert("updated_at".to_string(), json!(now_iso()));
    let rec = db::update(ctx, TABLE, user_id, data)
        .await
        .map_err(|e| db_failed(&format!("update profile for {user_id}"), e))?;
    row_from_map(&rec.data)
}

/// Read `user_id`'s current [`AUTH_VERSION_FIELD`] (P2c).
///
/// Returns `Ok(0)` when the user row is missing — mirrors
/// [`is_email_verified`]'s doc-claim collapse. The verify-side check
/// (`crate::crypto::verify_access_token`, via `blocks::auth::current_auth_version`)
/// doesn't otherwise require the row to exist for a JWT to authenticate, so a
/// missing row must not be indistinguishable from a real DB failure here.
pub async fn auth_version(ctx: &dyn Context, user_id: &str) -> Result<i64, WaferError> {
    use wafer_block::ErrorCode;

    use crate::util::RecordExt;

    match db::get(ctx, TABLE, user_id).await {
        Ok(r) => Ok(r.i64_field(AUTH_VERSION_FIELD)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(0),
        Err(e) => Err(db_failed(&format!("get auth_version for {user_id}"), e)),
    }
}

/// Atomically increment `user_id`'s [`AUTH_VERSION_FIELD`] by 1 (P2c).
///
/// Issues a single `UPDATE ... SET auth_version = auth_version + 1 WHERE id
/// = ?` via `db::increment_field_where` — a read-modify-write here could lose
/// a concurrent bump (e.g. an admin disabling a user at the same moment the
/// user changes their own password).
///
/// Callers should go through `blocks::auth::bump_auth_version`, which wraps
/// this with the verify-side cache invalidation so the two can't drift out
/// of sync; this function is the DB half only.
pub async fn bump_auth_version(ctx: &dyn Context, user_id: &str) -> Result<(), WaferError> {
    let filters = vec![Filter {
        field: "id".to_string(),
        operator: FilterOp::Equal,
        value: json!(user_id),
    }];
    db::increment_field_where(ctx, TABLE, AUTH_VERSION_FIELD, 1, &filters)
        .await
        .map_err(|e| db_failed(&format!("bump_auth_version for {user_id}"), e))?;
    Ok(())
}

/// Stamp `last_login_at` with the current time. Called on every successful
/// sign-in (password and OAuth); best-effort at the call sites, which log
/// and continue rather than failing a login that has already succeeded.
pub async fn touch_last_login(ctx: &dyn Context, user_id: &str) -> Result<(), WaferError> {
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert(
        "last_login_at".to_string(),
        json!(crate::util::now_rfc3339()),
    );
    data.insert("updated_at".to_string(), json!(now_iso()));
    db::update(ctx, TABLE, user_id, data)
        .await
        .map_err(|e| db_failed(&format!("touch last_login for {user_id}"), e))?;
    Ok(())
}

/// Set the `disabled` lifecycle flag and return the refreshed row.
///
/// This is the DB half only. A disable must also invalidate already-issued
/// access JWTs; that is `blocks::auth::bump_auth_version`, which pairs the
/// counter increment with the verify-side cache invalidation and therefore
/// stays with the caller (`admin::ops::set_user_disabled`).
pub async fn set_disabled(
    ctx: &dyn Context,
    user_id: &str,
    disabled: bool,
) -> Result<UserRow, WaferError> {
    let mut data: HashMap<String, Value> = HashMap::new();
    data.insert("disabled".to_string(), json!(disabled));
    data.insert("updated_at".to_string(), json!(now_iso()));
    let rec = db::update(ctx, TABLE, user_id, data)
        .await
        .map_err(|e| db_failed(&format!("set disabled for {user_id}"), e))?;
    row_from_record(&rec)
}

/// Soft-delete an account: stamps `deleted_at`, keeping the row so audit
/// trails and foreign keys survive. [`UserRow::is_active`] and
/// [`active_filter`] are what make the row invisible afterwards.
///
/// Same split as [`set_disabled`]: the `auth_version` bump belongs to the
/// caller.
pub async fn soft_delete(ctx: &dyn Context, user_id: &str) -> Result<(), WaferError> {
    db::soft_delete(ctx, TABLE, user_id)
        .await
        .map_err(|e| db_failed(&format!("soft-delete {user_id}"), e))?;
    Ok(())
}

/// The fields an admin may change on somebody else's account.
///
/// This type IS the whitelist: `admin::ops::update_user_fields` used to
/// build its column map by looping a `&["name", "disabled", "avatar_url"]`
/// array over the request body, so a fourth column was one array entry away
/// from being writable. Here there is no fourth field to name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminUserPatch {
    pub name: Option<String>,
    pub disabled: Option<bool>,
    pub avatar_url: Option<String>,
}

impl AdminUserPatch {
    /// Read the three writable fields out of a decoded JSON request body,
    /// ignoring everything else.
    ///
    /// `disabled` accepts the shapes [`super::map_bool`] accepts (`true`,
    /// `1`, `"1"`, `"true"`) because the admin UI has sent an integer for
    /// it. `name`/`avatar_url` accept JSON strings only — a number for a
    /// TEXT column is a malformed request, not a value.
    pub fn from_body(body: &HashMap<String, Value>) -> Self {
        let string_field = |key: &str| body.get(key).and_then(Value::as_str).map(str::to_owned);
        Self {
            name: string_field("name"),
            disabled: match body.get("disabled") {
                None | Some(Value::Null) => None,
                Some(_) => Some(map_bool(body, "disabled")),
            },
            avatar_url: string_field("avatar_url"),
        }
    }

    /// Whether this patch changes the `disabled` lifecycle flag — the
    /// question the caller asks before deciding to bump `auth_version`.
    pub fn touches_disabled(&self) -> bool {
        self.disabled.is_some()
    }
}

/// Apply an [`AdminUserPatch`] and return the refreshed row. An empty patch
/// still stamps `updated_at`, matching the previous behaviour of a `PATCH`
/// whose body named no writable field.
pub async fn patch_admin_fields(
    ctx: &dyn Context,
    user_id: &str,
    patch: &AdminUserPatch,
) -> Result<UserRow, WaferError> {
    let mut data: HashMap<String, Value> = HashMap::new();
    if let Some(name) = patch.name.as_deref() {
        data.insert("name".to_string(), json!(name));
    }
    if let Some(disabled) = patch.disabled {
        data.insert("disabled".to_string(), json!(disabled));
    }
    if let Some(avatar_url) = patch.avatar_url.as_deref() {
        data.insert("avatar_url".to_string(), json!(avatar_url));
    }
    data.insert("updated_at".to_string(), json!(now_iso()));
    let rec = db::update(ctx, TABLE, user_id, data)
        .await
        .map_err(|e| db_failed(&format!("patch admin fields for {user_id}"), e))?;
    row_from_record(&rec)
}

/// One page of the live-account list, as both admin surfaces ask for it.
#[derive(Debug, Clone)]
pub struct ActiveUserQuery {
    /// 1-based; values below 1 clamp to 1.
    pub page: i64,
    /// Rows per page; values below 1 fall back to 20.
    pub page_size: u32,
    /// Case-insensitive `LIKE '%…%'` over the email address AND the user
    /// id. Both admin surfaces asked a different one of those two questions
    /// before this function existed; one door means one answer.
    pub search: Option<String>,
}

/// A page of decoded rows plus the pagination envelope the callers echo.
#[derive(Debug, Clone)]
pub struct UserPage {
    pub rows: Vec<UserRow>,
    /// Total rows matching the filter across all pages.
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}

/// List live accounts, newest first. Soft-deleted rows are excluded by
/// [`active_filter`], and `total_count` is always the full matched count —
/// the SSR tab used to pass `skip_count: true` and render the in-page count
/// as the total in its pagination footer.
pub async fn list_active_page(
    ctx: &dyn Context,
    query: &ActiveUserQuery,
) -> Result<UserPage, WaferError> {
    let page = query.page.max(1);
    let page_size = if query.page_size < 1 {
        20
    } else {
        query.page_size
    };
    let offset = (page - 1).saturating_mul(i64::from(page_size));
    let search = query.search.as_deref().filter(|s| !s.is_empty());

    let opts = match search {
        Some(search) => {
            let like = format!("%{search}%");
            ListOptions {
                filter_tree: Some(vec![FilterTree::All(vec![
                    FilterTree::Leaf(active_filter()),
                    FilterTree::Any(vec![
                        FilterTree::Leaf(like_filter("email", &like)),
                        FilterTree::Leaf(like_filter("id", &like)),
                    ]),
                ])]),
                sort: newest_first(),
                limit: Some(page_size),
                offset,
                skip_count: false,
                ..Default::default()
            }
        }
        None => ListOptions {
            filters: vec![active_filter()],
            sort: newest_first(),
            limit: Some(page_size),
            offset,
            skip_count: false,
            ..Default::default()
        },
    };

    let list = db::list(ctx, TABLE, &opts)
        .await
        .map_err(|e| db_failed("list active users", e))?;
    let rows = list
        .records
        .iter()
        .map(row_from_record)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(UserPage {
        rows,
        total_count: list.total_count,
        page,
        page_size: i64::from(page_size),
    })
}

/// `(live accounts, of which created at or after `since_iso`)` in ONE
/// statement — both numbers share the `deleted_at IS NULL` predicate, so
/// the "created since" restriction rides along as a conditional sum instead
/// of costing a second round-trip. Each round-trip is a billed statement on
/// D1, which is why the admin dashboard folds its tiles this way.
pub async fn active_count_and_created_since(
    ctx: &dyn Context,
    since_iso: &str,
) -> Result<(i64, i64), WaferError> {
    let created_since = [Filter {
        field: "created_at".to_string(),
        operator: FilterOp::GreaterEqual,
        value: json!(since_iso),
    }];
    let req = wire::AggregateRequest {
        collection: TABLE.to_string(),
        select_columns: vec![],
        aggregates: vec![
            wire::AggregateColumnDef::Count {
                alias: "total".into(),
            },
            wire::AggregateColumnDef::CaseWhenSum {
                when: to_wire_filters(&created_since),
                alias: "since".into(),
            },
        ],
        filters: to_wire_filters(&[active_filter()]),
        group_by: vec![],
        sort: vec![],
        limit: 0,
    };
    let rows = db::aggregate(ctx, req)
        .await
        .map_err(|e| db_failed("user counts", e))?;
    let row = rows.first();
    let read = |k: &str| {
        row.and_then(|r| r.data.get(k))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    Ok((read("total"), read("since")))
}

/// One day's signup count. Shaped like
/// `platform_state::request_logs::DailyCounts` so the dashboard projects
/// both series the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailySignups {
    /// `YYYY-MM-DD`.
    pub day: String,
    pub count: i64,
}

/// Live accounts created per day at or after `since_iso`, in ONE grouped
/// statement. Days with no signups are absent; zero-filling is the chart's
/// job.
pub async fn daily_signups(
    ctx: &dyn Context,
    since_iso: &str,
) -> Result<Vec<DailySignups>, WaferError> {
    let rows = daily_grouped(
        ctx,
        TABLE,
        since_iso,
        vec![active_filter()],
        vec![wire::AggregateColumnDef::Count {
            alias: "cnt".into(),
        }],
    )
    .await
    .map_err(|e| db_failed("daily signups", e))?;
    Ok(rows
        .iter()
        .map(|r| DailySignups {
            day: r.data.str_field("created_at").to_string(),
            count: r.data.i64_field("cnt"),
        })
        .collect())
}

/// The `limit` most recently created live accounts, newest first — the
/// admin dashboard's "Recent Users" card. `skip_count: true`: the card
/// shows rows, never a total.
pub async fn list_recent_active(ctx: &dyn Context, limit: u32) -> Result<Vec<UserRow>, WaferError> {
    let opts = ListOptions {
        filters: vec![active_filter()],
        sort: newest_first(),
        limit: Some(limit),
        skip_count: true,
        ..Default::default()
    };
    let list = db::list(ctx, TABLE, &opts)
        .await
        .map_err(|e| db_failed("list recent users", e))?;
    list.records.iter().map(row_from_record).collect()
}

#[cfg(test)]
mod lifecycle_and_listing_tests {
    //! The functions the admin surfaces and the two login paths now reach
    //! this table through. Each one existed as a hand-built column map or a
    //! hand-built filter list in a page or handler before this module owned
    //! it; the assertions here are what those call sites used to assume
    //! without saying so.

    use super::*;
    use crate::test_support::TestContext;

    async fn ctx() -> TestContext {
        TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID)
    }

    async fn seed(ctx: &TestContext, email: &str) -> UserRow {
        insert(
            ctx,
            NewUser {
                email: email.to_string(),
                display_name: email.to_string(),
                avatar_url: None,
                role: "user".to_string(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .expect("seed user")
    }

    #[tokio::test]
    async fn insert_persists_the_verification_fields() {
        let ctx = ctx().await;
        let row = insert(
            &ctx,
            NewUser {
                email: "v@example.com".into(),
                display_name: "V".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: true,
                verification_token_hash: Some("tokhash".into()),
            },
        )
        .await
        .expect("insert");
        assert!(row.email_verified, "the inserted row is verified");
        assert!(is_email_verified(&ctx, &row.id).await.unwrap());
        // Signup used to need a follow-up UPDATE for exactly this.
        let found = find_by_verification_token(&ctx, "tokhash")
            .await
            .unwrap()
            .expect("findable by the token the insert carried");
        assert_eq!(found.id, row.id);
    }

    #[tokio::test]
    async fn insert_without_a_token_leaves_the_row_unverified_and_unfindable() {
        let ctx = ctx().await;
        let row = seed(&ctx, "plain@example.com").await;
        assert!(!row.email_verified);
        assert!(find_by_verification_token(&ctx, "")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn touch_last_login_stamps_the_column() {
        let ctx = ctx().await;
        let row = seed(&ctx, "login@example.com").await;
        assert_eq!(row.last_login_at, None, "never signed in");

        touch_last_login(&ctx, &row.id).await.expect("touch");
        let after = find_by_id(&ctx, &row.id).await.unwrap().unwrap();
        assert!(
            after.last_login_at.is_some_and(|t| !t.is_empty()),
            "last_login_at must be populated after a sign-in"
        );
    }

    #[tokio::test]
    async fn set_disabled_round_trips_and_drives_is_active() {
        let ctx = ctx().await;
        let row = seed(&ctx, "dis@example.com").await;
        assert!(row.is_active());

        let disabled = set_disabled(&ctx, &row.id, true).await.expect("disable");
        assert!(disabled.disabled);
        assert!(!disabled.is_active());
        assert!(!find_by_id(&ctx, &row.id)
            .await
            .unwrap()
            .unwrap()
            .is_active());

        let enabled = set_disabled(&ctx, &row.id, false).await.expect("enable");
        assert!(!enabled.disabled);
        assert!(enabled.is_active());
    }

    #[tokio::test]
    async fn soft_delete_marks_deleted_and_hides_the_row_from_active_reads() {
        let ctx = ctx().await;
        let row = seed(&ctx, "gone@example.com").await;
        soft_delete(&ctx, &row.id).await.expect("soft-delete");

        let after = find_by_id(&ctx, &row.id).await.unwrap().unwrap();
        assert!(after.is_deleted(), "the row survives, marked");
        assert!(!after.is_active());
        // Every active read shares one predicate, so one delete hides it
        // from all of them.
        let page = list_active_page(
            &ctx,
            &ActiveUserQuery {
                page: 1,
                page_size: 20,
                search: None,
            },
        )
        .await
        .unwrap();
        assert!(page.rows.is_empty());
        assert_eq!(page.total_count, 0);
        assert!(list_recent_active(&ctx, 5).await.unwrap().is_empty());
        assert_eq!(
            active_count_and_created_since(&ctx, "1970-01-01T00:00:00")
                .await
                .unwrap(),
            (0, 0)
        );
    }

    /// A lifecycle write against a user that is not there keeps the
    /// backend's `NotFound`, which is what lets `admin::ops` answer 404.
    /// The deleted `RepoError` had a dedicated `NotFound` variant for this
    /// one case; the code carries it for every case now, including the
    /// `PermissionDenied` the variant had no room for.
    #[tokio::test]
    async fn a_lifecycle_write_against_a_missing_user_is_not_found() {
        let ctx = ctx().await;
        for outcome in [
            set_disabled(&ctx, "nope", true).await.map(|_| ()),
            soft_delete(&ctx, "nope").await,
            patch_admin_fields(&ctx, "nope", &AdminUserPatch::default())
                .await
                .map(|_| ()),
        ] {
            assert_eq!(
                outcome.expect_err("no such user").code,
                wafer_block::ErrorCode::NotFound
            );
        }
    }

    #[test]
    fn admin_patch_reads_only_its_three_fields() {
        let body: HashMap<String, Value> = HashMap::from([
            ("name".to_string(), json!("New")),
            ("avatar_url".to_string(), json!("https://a/b.png")),
            ("disabled".to_string(), json!(1)),
            // Everything a caller might hope to smuggle in.
            ("role".to_string(), json!("admin")),
            ("email".to_string(), json!("other@example.com")),
            ("deleted_at".to_string(), json!("2026-01-01")),
            ("auth_version".to_string(), json!(99)),
        ]);
        let patch = AdminUserPatch::from_body(&body);
        assert_eq!(
            patch,
            AdminUserPatch {
                name: Some("New".into()),
                disabled: Some(true),
                avatar_url: Some("https://a/b.png".into()),
            }
        );
        assert!(patch.touches_disabled());
    }

    #[test]
    fn admin_patch_disabled_accepts_every_shape_the_column_round_trips() {
        let of = |v: Value| {
            AdminUserPatch::from_body(&HashMap::from([("disabled".to_string(), v)])).disabled
        };
        assert_eq!(of(json!(true)), Some(true));
        assert_eq!(of(json!(1)), Some(true));
        assert_eq!(of(json!("1")), Some(true));
        assert_eq!(of(json!("true")), Some(true));
        assert_eq!(of(json!(false)), Some(false));
        assert_eq!(of(json!(0)), Some(false));
        // Absent and explicit null both mean "do not touch the flag".
        assert_eq!(of(json!(null)), None);
        assert_eq!(AdminUserPatch::from_body(&HashMap::new()).disabled, None);
    }

    #[tokio::test]
    async fn patch_admin_fields_writes_only_the_whitelist() {
        let ctx = ctx().await;
        let row = seed(&ctx, "patch@example.com").await;
        let updated = patch_admin_fields(
            &ctx,
            &row.id,
            &AdminUserPatch {
                name: Some("Renamed".into()),
                disabled: Some(true),
                avatar_url: Some("https://a/b.png".into()),
            },
        )
        .await
        .expect("patch");
        assert_eq!(updated.name.as_deref(), Some("Renamed"));
        assert!(updated.disabled);
        assert_eq!(updated.avatar_url.as_deref(), Some("https://a/b.png"));
        assert_eq!(updated.email, "patch@example.com", "email is untouched");
        assert_eq!(updated.role, "user", "role is untouched");
    }

    #[tokio::test]
    async fn list_active_page_pages_newest_first() {
        let ctx = ctx().await;
        for i in 0..5 {
            seed(&ctx, &format!("p{i}@example.com")).await;
        }
        let q = |page| ActiveUserQuery {
            page,
            page_size: 2,
            search: None,
        };
        let first = list_active_page(&ctx, &q(1)).await.unwrap();
        assert_eq!(first.rows.len(), 2);
        assert_eq!(first.total_count, 5, "the full match count, not the page");
        assert_eq!((first.page, first.page_size), (1, 2));

        let third = list_active_page(&ctx, &q(3)).await.unwrap();
        assert_eq!(third.rows.len(), 1);
        assert_eq!(third.total_count, 5);

        // No row appears twice across the pages.
        let mut seen: Vec<String> = first.rows.iter().map(|r| r.id.clone()).collect();
        seen.extend(
            list_active_page(&ctx, &q(2))
                .await
                .unwrap()
                .rows
                .iter()
                .map(|r| r.id.clone()),
        );
        seen.extend(third.rows.iter().map(|r| r.id.clone()));
        let unique: std::collections::HashSet<&String> = seen.iter().collect();
        assert_eq!(unique.len(), 5);
    }

    /// The two admin surfaces used to disagree here: the JSON list searched
    /// `email` only, the SSR tab `email OR id`. One door, one answer.
    #[tokio::test]
    async fn list_active_page_search_matches_email_or_id() {
        let ctx = ctx().await;
        let target = seed(&ctx, "needle@example.com").await;
        seed(&ctx, "other@example.com").await;

        let by_email = list_active_page(
            &ctx,
            &ActiveUserQuery {
                page: 1,
                page_size: 20,
                search: Some("needle".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(by_email.rows.len(), 1);
        assert_eq!(by_email.rows[0].id, target.id);
        assert_eq!(by_email.total_count, 1);

        let by_id = list_active_page(
            &ctx,
            &ActiveUserQuery {
                page: 1,
                page_size: 20,
                search: Some(target.id.clone()),
            },
        )
        .await
        .unwrap();
        assert_eq!(by_id.rows.len(), 1, "a full user id must match");
        assert_eq!(by_id.rows[0].id, target.id);
    }

    #[tokio::test]
    async fn active_count_and_created_since_matches_separate_counts() {
        let ctx = ctx().await;
        for i in 0..3 {
            seed(&ctx, &format!("live{i}@example.com")).await;
        }
        let deleted = seed(&ctx, "dead@example.com").await;
        soft_delete(&ctx, &deleted.id).await.unwrap();

        let today_start = format!("{}T00:00:00", chrono::Utc::now().format("%Y-%m-%d"));
        let tomorrow_start = format!(
            "{}T00:00:00",
            (chrono::Utc::now() + chrono::Duration::days(1)).format("%Y-%m-%d")
        );

        // Against the same predicate spelled as separate counts.
        let expected_total = db::count(&ctx, TABLE, &[active_filter()]).await.unwrap();
        let (total, since) = active_count_and_created_since(&ctx, &today_start)
            .await
            .unwrap();
        assert_eq!(total, expected_total);
        assert_eq!((total, since), (3, 3), "the soft-deleted row is excluded");

        let (_, none_yet) = active_count_and_created_since(&ctx, &tomorrow_start)
            .await
            .unwrap();
        assert_eq!(none_yet, 0, "nothing was created tomorrow");
    }

    #[tokio::test]
    async fn daily_signups_buckets_by_day_and_excludes_soft_deleted() {
        let ctx = ctx().await;
        for i in 0..3 {
            seed(&ctx, &format!("s{i}@example.com")).await;
        }
        let deleted = seed(&ctx, "sdel@example.com").await;
        soft_delete(&ctx, &deleted.id).await.unwrap();

        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let rows = daily_signups(&ctx, "1970-01-01T00:00:00").await.unwrap();
        assert_eq!(rows.len(), 1, "every seeded row landed on one day");
        assert_eq!(rows[0].day, today);
        assert_eq!(rows[0].count, 3, "the soft-deleted signup is excluded");

        let future = format!(
            "{}T00:00:00",
            (chrono::Utc::now() + chrono::Duration::days(1)).format("%Y-%m-%d")
        );
        assert!(daily_signups(&ctx, &future).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_recent_active_is_capped_and_newest_first() {
        let ctx = ctx().await;
        for i in 0..4 {
            seed(&ctx, &format!("r{i}@example.com")).await;
        }
        let rows = list_recent_active(&ctx, 2).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows[0].created_at >= rows[1].created_at,
            "newest first: {rows:?}"
        );
        // Every row decodes fully — the dashboard's old projected SELECT
        // returned three columns and read them by hand.
        assert!(!rows[0].email.is_empty());
    }

    /// Accounts created in the same second tie on `created_at`. Both admin
    /// lists still come back newest first, and paging through the users tab
    /// visits every account exactly once. Without a tiebreak SQLite falls
    /// back to scan order — oldest first — and "the two most recent" are the
    /// two oldest of the second.
    #[tokio::test]
    async fn accounts_created_in_the_same_second_list_newest_first() {
        let ctx = ctx().await;
        let mut created = Vec::new();
        for i in 0..5 {
            created.push(seed(&ctx, &format!("tie{i}@example.com")).await.id);
        }
        // Whether real inserts straddle a second boundary is up to the clock;
        // pin them all into one second so the tie is certain.
        for id in &created {
            let mut data = HashMap::new();
            data.insert("created_at".to_string(), json!("2026-01-01T00:00:00Z"));
            db::update(&ctx, TABLE, id, data)
                .await
                .expect("pin created_at");
        }
        let newest_first: Vec<String> = created.iter().rev().cloned().collect();

        let recent: Vec<String> = list_recent_active(&ctx, 2)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(recent, newest_first[..2], "the two created last");

        let mut paged = Vec::new();
        for page in 1..=3 {
            let list = list_active_page(
                &ctx,
                &ActiveUserQuery {
                    page,
                    page_size: 2,
                    search: None,
                },
            )
            .await
            .unwrap();
            paged.extend(list.rows.into_iter().map(|r| r.id));
        }
        assert_eq!(paged, newest_first, "every account once, newest first");
    }
}

#[cfg(test)]
mod auth_version_tests {
    use super::*;
    use crate::test_support::TestContext;

    async fn seed(ctx: &TestContext) -> String {
        insert(
            ctx,
            NewUser {
                email: "av@example.com".into(),
                display_name: "AV".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn fresh_user_starts_at_version_zero() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        let id = seed(&ctx).await;
        assert_eq!(auth_version(&ctx, &id).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn missing_user_reads_as_version_zero() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        assert_eq!(auth_version(&ctx, "does-not-exist").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn bump_increments_atomically_and_is_readable() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        let id = seed(&ctx).await;

        bump_auth_version(&ctx, &id).await.unwrap();
        assert_eq!(auth_version(&ctx, &id).await.unwrap(), 1);

        bump_auth_version(&ctx, &id).await.unwrap();
        bump_auth_version(&ctx, &id).await.unwrap();
        assert_eq!(
            auth_version(&ctx, &id).await.unwrap(),
            3,
            "three sequential bumps must land as +1 each, not overwrite"
        );
    }

    #[tokio::test]
    async fn bump_does_not_affect_other_users() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        let a = seed(&ctx).await;
        let b = insert(
            &ctx,
            NewUser {
                email: "other@example.com".into(),
                display_name: "Other".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .unwrap()
        .id;

        bump_auth_version(&ctx, &a).await.unwrap();
        assert_eq!(auth_version(&ctx, &a).await.unwrap(), 1);
        assert_eq!(auth_version(&ctx, &b).await.unwrap(), 0);
    }
}

#[cfg(test)]
mod email_verified_tests {
    use super::*;
    use crate::test_support::TestContext;

    /// Seeds a user with `email_verified` 0 (unverified), through the
    /// shared fixture so the column list lives in one place.
    async fn seed_user(ctx: &TestContext, user_id: &str) {
        ctx.seed_auth_user(user_id).await;
    }

    #[tokio::test]
    async fn unverified_by_default_after_seed() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        assert!(!is_email_verified(&ctx, "user-a").await.unwrap());
    }

    #[tokio::test]
    async fn set_then_read_round_trips_true() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        set_email_verified(&ctx, "user-a", true).await.unwrap();
        assert!(is_email_verified(&ctx, "user-a").await.unwrap());
    }

    #[tokio::test]
    async fn set_then_read_round_trips_false() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        // First flip to true so the false-write isn't a no-op against the
        // default value.
        set_email_verified(&ctx, "user-a", true).await.unwrap();
        set_email_verified(&ctx, "user-a", false).await.unwrap();
        assert!(!is_email_verified(&ctx, "user-a").await.unwrap());
    }

    #[tokio::test]
    async fn missing_user_returns_false() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        // Doc-claim: missing user → Ok(false). Real DB errors still propagate
        // (verified separately by the per-backend integration tests).
        assert!(!is_email_verified(&ctx, "nonexistent").await.unwrap());
    }
}

#[cfg(test)]
mod typed_client_tests {
    use super::*;
    use crate::test_support::TestContext;

    /// `repo::users::insert` must succeed when the auth block calls it under
    /// WRAP enforcement (own-resource access). Today's `exec_raw` path
    /// requires admin and would fail; this test guards the typed-client
    /// rewrite.
    #[tokio::test]
    async fn insert_succeeds_under_wrap_for_auth_block() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let user = insert(
            &ctx,
            NewUser {
                email: "a@b.c".into(),
                display_name: "A".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .expect("insert under wrap");
        assert_eq!(user.email, "a@b.c");
        assert!(!user.id.is_empty());
    }

    #[tokio::test]
    async fn find_by_email_returns_inserted_row_under_wrap() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        insert(
            &ctx,
            NewUser {
                email: "x@y.z".into(),
                display_name: "X".into(),
                avatar_url: Some("https://example.com/a.png".into()),
                role: "admin".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .unwrap();
        let got = find_by_email(&ctx, "x@y.z").await.unwrap().unwrap();
        assert_eq!(got.role, "admin");
        assert_eq!(got.avatar_url.as_deref(), Some("https://example.com/a.png"));
    }

    #[tokio::test]
    async fn count_reports_zero_then_one() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        assert_eq!(count(&ctx).await.unwrap(), 0);
        insert(
            &ctx,
            NewUser {
                email: "c@d.e".into(),
                display_name: "C".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(count(&ctx).await.unwrap(), 1);
    }

    async fn seed_one(ctx: &TestContext) -> String {
        insert(
            ctx,
            NewUser {
                email: "tok@example.com".into(),
                display_name: "Tok".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn verification_token_round_trip_and_mark_verified() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let id = seed_one(&ctx).await;

        set_verification_token(&ctx, &id, "vhash", "2026-06-01T00:00:00Z")
            .await
            .unwrap();
        let found = find_by_verification_token(&ctx, "vhash")
            .await
            .unwrap()
            .expect("found by token");
        assert_eq!(found.id, id);
        assert!(!found.email_verified);
        assert_eq!(
            last_verification_sent(&ctx, &id).await.unwrap(),
            "2026-06-01T00:00:00Z"
        );

        record_email_proof(&ctx, &id, proof::EMAIL_TOKEN)
            .await
            .unwrap();
        assert!(is_email_verified(&ctx, &id).await.unwrap());
        // The proof is recorded, not merely the flag: this is the field the
        // OAuth adoption rule reads.
        let verified = find_by_id(&ctx, &id).await.unwrap().expect("row");
        assert!(verified.email_is_proven());
        assert_eq!(
            verified.email_verified_by.as_deref(),
            Some(proof::EMAIL_TOKEN)
        );
        // Token cleared → no longer findable.
        assert!(find_by_verification_token(&ctx, "vhash")
            .await
            .unwrap()
            .is_none());

        // Retracting the flag retracts the proof with it.
        set_email_verified(&ctx, &id, false).await.unwrap();
        assert!(!find_by_id(&ctx, &id)
            .await
            .unwrap()
            .expect("row")
            .email_is_proven());
    }

    /// The policy flag cannot manufacture a proof. `signup` writes
    /// `email_verified = !REQUIRE_VERIFICATION`, so a row can be "verified"
    /// having proved nothing; `email_is_proven` must still say no.
    #[tokio::test]
    async fn setting_the_flag_does_not_create_a_proof() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let id = seed_one(&ctx).await;

        set_email_verified(&ctx, &id, true).await.unwrap();
        let row = find_by_id(&ctx, &id).await.unwrap().expect("row");
        assert!(row.email_verified, "the policy flag is set");
        assert!(
            !row.email_is_proven(),
            "but nobody proved the address, so it is not proven"
        );
    }

    #[tokio::test]
    async fn reset_token_take_and_restore() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let id = seed_one(&ctx).await;

        set_reset_token(&ctx, &id, "rhash", "2099-01-01T00:00:00Z")
            .await
            .unwrap();
        let found = find_by_reset_token(&ctx, "rhash")
            .await
            .unwrap()
            .expect("found by reset token");
        assert_eq!(found.id, id);
        assert_eq!(found.reset_token_expires, "2099-01-01T00:00:00Z");

        assert!(
            !take_reset_token(&ctx, &id, "other-hash").await.unwrap(),
            "a token the row does not carry is not taken"
        );
        assert!(take_reset_token(&ctx, &id, "rhash").await.unwrap());
        assert!(find_by_reset_token(&ctx, "rhash").await.unwrap().is_none());
        assert!(
            !take_reset_token(&ctx, &id, "rhash").await.unwrap(),
            "a spent token is not taken twice"
        );

        assert!(
            restore_reset_token(&ctx, &id, "rhash", "2099-01-01T00:00:00Z")
                .await
                .unwrap()
        );
        let restored = find_by_reset_token(&ctx, "rhash")
            .await
            .unwrap()
            .expect("restored");
        assert_eq!(restored.reset_token_expires, "2099-01-01T00:00:00Z");

        // A newer link wins over a restore of the spent one.
        take_reset_token(&ctx, &id, "rhash").await.unwrap();
        set_reset_token(&ctx, &id, "newer", "2099-01-02T00:00:00Z")
            .await
            .unwrap();
        assert!(
            !restore_reset_token(&ctx, &id, "rhash", "2099-01-01T00:00:00Z")
                .await
                .unwrap()
        );
        assert!(find_by_reset_token(&ctx, "newer").await.unwrap().is_some());
        assert!(find_by_reset_token(&ctx, "rhash").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn update_profile_dual_writes_name_and_avatar() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let id = seed_one(&ctx).await;

        let updated = update_profile(&ctx, &id, Some("New Name"), Some("https://a/b.png"))
            .await
            .unwrap();
        assert_eq!(updated.display_name, "New Name");
        assert_eq!(updated.avatar_url.as_deref(), Some("https://a/b.png"));
        // The legacy `name` column is dual-written.
        let raw = db::get(&ctx, TABLE, &id).await.unwrap();
        use crate::util::RecordExt;
        assert_eq!(raw.str_field("name"), "New Name");
        assert_eq!(raw.str_field("display_name"), "New Name");
    }

    async fn seed_active(ctx: &TestContext) -> String {
        insert(
            ctx,
            NewUser {
                email: "life@example.com".into(),
                display_name: "Life".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn fresh_user_is_active() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let id = seed_active(&ctx).await;
        let row = find_by_id(&ctx, &id).await.unwrap().unwrap();
        assert!(row.is_active());
        assert!(!row.is_deleted());
        assert_eq!(row.deleted_at, None);
    }

    #[tokio::test]
    async fn disabled_user_is_not_active() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let id = seed_active(&ctx).await;
        let mut patch = std::collections::HashMap::new();
        patch.insert("disabled".to_string(), serde_json::json!(true));
        db::update(&ctx, TABLE, &id, patch).await.unwrap();

        let row = find_by_id(&ctx, &id).await.unwrap().unwrap();
        assert!(row.disabled);
        assert!(!row.is_active());
    }

    #[tokio::test]
    async fn soft_deleted_user_is_not_active() {
        let ctx = TestContext::with_auth().await.running_as("wafer-run/auth");
        let id = seed_active(&ctx).await;
        let mut patch = std::collections::HashMap::new();
        patch.insert(
            "deleted_at".to_string(),
            serde_json::json!("2026-01-01T00:00:00Z"),
        );
        db::update(&ctx, TABLE, &id, patch).await.unwrap();

        let row = find_by_id(&ctx, &id).await.unwrap().unwrap();
        assert!(row.is_deleted());
        assert!(!row.is_active());
    }
}
