//! Row-level access over `wafer_run__auth__users`.

use std::collections::HashMap;

use serde_json::{json, Value};
use uuid::Uuid;
use wafer_core::clients::database as db;
use wafer_run::context::Context;

use super::{map_bool, map_opt_str, map_str, now_iso, RepoError};

pub const TABLE: &str = "wafer_run__auth__users";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRow {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub role: String,
    pub disabled: bool,
    /// Soft-delete timestamp (ISO-8601). `None`/empty when the account is live.
    /// Column exists since migration 006; a soft-deleted account keeps its row
    /// but must not authenticate.
    pub deleted_at: Option<String>,
    pub email_verified: bool,
    pub created_at: String,
    pub updated_at: String,
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
}

#[derive(Debug, Clone)]
pub struct NewUser {
    pub email: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub role: String,
}

fn row_from_map(m: &HashMap<String, Value>) -> Result<UserRow, RepoError> {
    Ok(UserRow {
        id: map_opt_str(m, "id").ok_or_else(|| RepoError::Db("missing id".into()))?,
        email: map_opt_str(m, "email").ok_or_else(|| RepoError::Db("missing email".into()))?,
        display_name: map_str(m, "display_name"),
        avatar_url: map_opt_str(m, "avatar_url"),
        role: map_opt_str(m, "role").unwrap_or_else(|| "user".into()),
        disabled: map_bool(m, "disabled"),
        deleted_at: map_opt_str(m, "deleted_at"),
        email_verified: map_bool(m, "email_verified"),
        created_at: map_str(m, "created_at"),
        updated_at: map_str(m, "updated_at"),
    })
}

pub async fn insert(ctx: &dyn Context, new: NewUser) -> Result<UserRow, RepoError> {
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
    data.insert("created_at".into(), json!(now));
    data.insert("updated_at".into(), json!(now));

    let rec = db::create(ctx, TABLE, data)
        .await
        .map_err(|e| RepoError::Db(format!("insert: {e}")))?;
    row_from_map(&rec.data)
}

pub async fn find_by_email(ctx: &dyn Context, email: &str) -> Result<Option<UserRow>, RepoError> {
    use wafer_block::ErrorCode;
    match db::get_by_field(ctx, TABLE, "email", json!(email)).await {
        Ok(rec) => Ok(Some(row_from_map(&rec.data)?)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(RepoError::Db(format!("select by email: {e}"))),
    }
}

/// Returns the number of rows currently in `wafer_run__auth__users`.
///
/// Used by the block's bootstrap logic to decide whether to create the first
/// admin user. A non-zero count means "already bootstrapped — no-op".
pub async fn count(ctx: &dyn Context) -> Result<u64, RepoError> {
    let n = db::count(ctx, TABLE, &[])
        .await
        .map_err(|e| RepoError::Db(format!("users count: {e}")))?;
    Ok(n.max(0) as u64)
}

pub async fn find_by_id(ctx: &dyn Context, id: &str) -> Result<Option<UserRow>, RepoError> {
    use wafer_block::ErrorCode;
    match db::get(ctx, TABLE, id).await {
        Ok(rec) => Ok(Some(row_from_map(&rec.data)?)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(RepoError::Db(format!("select by id: {e}"))),
    }
}

/// Read the `email_verified` flag for a user. Returns `Ok(false)` when the
/// user row is missing (the doc-claim path) and propagates other DB errors.
///
/// Accepts both SQLite TEXT-int (`'0'`/`'1'`), Postgres BOOLEAN, JSON `bool`,
/// and string `'true'`/`'false'` via `RecordExt::bool_field`.
pub async fn is_email_verified(ctx: &dyn Context, user_id: &str) -> Result<bool, RepoError> {
    use wafer_block::ErrorCode;

    use crate::util::RecordExt;

    match db::get(ctx, TABLE, user_id).await {
        Ok(r) => Ok(r.bool_field("email_verified")),
        Err(e) if e.code == ErrorCode::NotFound => Ok(false),
        Err(e) => Err(RepoError::Db(format!("get user {user_id}: {e}"))),
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
) -> Result<(), RepoError> {
    let mut data = std::collections::HashMap::new();
    data.insert("email_verified".to_string(), json!(verified));
    crate::util::stamp_updated(&mut data);

    db::update(ctx, TABLE, user_id, data)
        .await
        .map_err(|e| RepoError::Db(format!("set email_verified for {user_id}: {e}")))?;
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
) -> Result<Option<UserRow>, RepoError> {
    use wafer_block::ErrorCode;
    match db::get_by_field(ctx, TABLE, "verification_token", json!(token_hash)).await {
        Ok(rec) => Ok(Some(row_from_map(&rec.data)?)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(RepoError::Db(format!("find by verification_token: {e}"))),
    }
}

/// Mark a user's email as verified and clear their `verification_token` in one
/// write. Stamps `updated_at` with [`super::now_iso`].
pub async fn mark_email_verified(ctx: &dyn Context, user_id: &str) -> Result<(), RepoError> {
    let mut data = std::collections::HashMap::new();
    data.insert("email_verified".to_string(), json!(true));
    data.insert("verification_token".to_string(), json!(""));
    data.insert("updated_at".to_string(), json!(now_iso()));
    db::update(ctx, TABLE, user_id, data)
        .await
        .map_err(|e| RepoError::Db(format!("mark verified for {user_id}: {e}")))?;
    Ok(())
}

/// Read a user's `last_verification_sent` timestamp (the resend cooldown
/// anchor). Returns an empty string when unset/absent. `Ok(None)` would be
/// indistinguishable from "never sent" here, so absence collapses to `""`.
pub async fn last_verification_sent(ctx: &dyn Context, user_id: &str) -> Result<String, RepoError> {
    use wafer_block::ErrorCode;

    use crate::util::RecordExt;

    match db::get(ctx, TABLE, user_id).await {
        Ok(r) => Ok(r.str_field("last_verification_sent").to_string()),
        Err(e) if e.code == ErrorCode::NotFound => Ok(String::new()),
        Err(e) => Err(RepoError::Db(format!("get last_verification_sent: {e}"))),
    }
}

/// Store a freshly-minted email-verification token (its SHA-256 hex) and the
/// `last_verification_sent` cooldown timestamp. Stamps `updated_at`.
pub async fn set_verification_token(
    ctx: &dyn Context,
    user_id: &str,
    token_hash: &str,
    sent_at: &str,
) -> Result<(), RepoError> {
    let mut data = std::collections::HashMap::new();
    data.insert("verification_token".to_string(), json!(token_hash));
    data.insert("last_verification_sent".to_string(), json!(sent_at));
    data.insert("updated_at".to_string(), json!(now_iso()));
    db::update(ctx, TABLE, user_id, data)
        .await
        .map_err(|e| RepoError::Db(format!("set verification_token for {user_id}: {e}")))?;
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
) -> Result<Option<ResetTokenUser>, RepoError> {
    use wafer_block::ErrorCode;

    use crate::util::RecordExt;

    match db::get_by_field(ctx, TABLE, "reset_token", json!(token_hash)).await {
        Ok(rec) => Ok(Some(ResetTokenUser {
            id: rec.id.clone(),
            reset_token_expires: rec.str_field("reset_token_expires").to_string(),
        })),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(RepoError::Db(format!("find by reset_token: {e}"))),
    }
}

/// Store a password-reset token (its SHA-256 hex) and its absolute expiry.
/// Stamps `updated_at`.
pub async fn set_reset_token(
    ctx: &dyn Context,
    user_id: &str,
    token_hash: &str,
    expires_at: &str,
) -> Result<(), RepoError> {
    let mut data = std::collections::HashMap::new();
    data.insert("reset_token".to_string(), json!(token_hash));
    data.insert("reset_token_expires".to_string(), json!(expires_at));
    data.insert("updated_at".to_string(), json!(now_iso()));
    db::update(ctx, TABLE, user_id, data)
        .await
        .map_err(|e| RepoError::Db(format!("set reset_token for {user_id}: {e}")))?;
    Ok(())
}

/// Clear a user's password-reset token + expiry after a successful reset.
/// Stamps `updated_at`.
pub async fn clear_reset_token(ctx: &dyn Context, user_id: &str) -> Result<(), RepoError> {
    let mut data = std::collections::HashMap::new();
    data.insert("reset_token".to_string(), json!(""));
    data.insert("reset_token_expires".to_string(), json!(""));
    data.insert("updated_at".to_string(), json!(now_iso()));
    db::update(ctx, TABLE, user_id, data)
        .await
        .map_err(|e| RepoError::Db(format!("clear reset_token for {user_id}: {e}")))?;
    Ok(())
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
) -> Result<UserRow, RepoError> {
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
        .map_err(|e| RepoError::Db(format!("update profile for {user_id}: {e}")))?;
    row_from_map(&rec.data)
}

#[cfg(test)]
mod email_verified_tests {
    use super::*;
    use crate::test_support::TestContext;

    /// Seeds a user with `email_verified` defaulted to 0 (unverified). Mirrors
    /// the migration's `INTEGER NOT NULL DEFAULT 0` shape.
    async fn seed_user(ctx: &TestContext, user_id: &str) {
        db::exec_raw(
            ctx,
            "INSERT INTO wafer_run__auth__users \
             (id, email, display_name, role, email_verified, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            &[
                json!(user_id),
                json!(format!("{user_id}@example.com")),
                json!(user_id),
                json!("user"),
                json!(0),
                json!("2026-01-01T00:00:00Z"),
                json!("2026-01-01T00:00:00Z"),
            ],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unverified_by_default_after_seed() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        assert!(!is_email_verified(&ctx, "user-a").await.unwrap());
    }

    #[tokio::test]
    async fn set_then_read_round_trips_true() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        set_email_verified(&ctx, "user-a", true).await.unwrap();
        assert!(is_email_verified(&ctx, "user-a").await.unwrap());
    }

    #[tokio::test]
    async fn set_then_read_round_trips_false() {
        let ctx = TestContext::with_auth().await;
        seed_user(&ctx, "user-a").await;
        // First flip to true so the false-write isn't a no-op against the
        // default value.
        set_email_verified(&ctx, "user-a", true).await.unwrap();
        set_email_verified(&ctx, "user-a", false).await.unwrap();
        assert!(!is_email_verified(&ctx, "user-a").await.unwrap());
    }

    #[tokio::test]
    async fn missing_user_returns_false() {
        let ctx = TestContext::with_auth().await;
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
        let ctx =
            TestContext::with_auth()
                .await
                .with_wrap("wafer-run/auth", vec![], "impresspress/admin");
        let user = insert(
            &ctx,
            NewUser {
                email: "a@b.c".into(),
                display_name: "A".into(),
                avatar_url: None,
                role: "user".into(),
            },
        )
        .await
        .expect("insert under wrap");
        assert_eq!(user.email, "a@b.c");
        assert!(!user.id.is_empty());
    }

    #[tokio::test]
    async fn find_by_email_returns_inserted_row_under_wrap() {
        let ctx =
            TestContext::with_auth()
                .await
                .with_wrap("wafer-run/auth", vec![], "impresspress/admin");
        insert(
            &ctx,
            NewUser {
                email: "x@y.z".into(),
                display_name: "X".into(),
                avatar_url: Some("https://example.com/a.png".into()),
                role: "admin".into(),
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
        let ctx =
            TestContext::with_auth()
                .await
                .with_wrap("wafer-run/auth", vec![], "impresspress/admin");
        assert_eq!(count(&ctx).await.unwrap(), 0);
        insert(
            &ctx,
            NewUser {
                email: "c@d.e".into(),
                display_name: "C".into(),
                avatar_url: None,
                role: "user".into(),
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
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn verification_token_round_trip_and_mark_verified() {
        let ctx =
            TestContext::with_auth()
                .await
                .with_wrap("wafer-run/auth", vec![], "impresspress/admin");
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

        mark_email_verified(&ctx, &id).await.unwrap();
        assert!(is_email_verified(&ctx, &id).await.unwrap());
        // Token cleared → no longer findable.
        assert!(find_by_verification_token(&ctx, "vhash")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn reset_token_round_trip_and_clear() {
        let ctx =
            TestContext::with_auth()
                .await
                .with_wrap("wafer-run/auth", vec![], "impresspress/admin");
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

        clear_reset_token(&ctx, &id).await.unwrap();
        assert!(find_by_reset_token(&ctx, "rhash").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn update_profile_dual_writes_name_and_avatar() {
        let ctx =
            TestContext::with_auth()
                .await
                .with_wrap("wafer-run/auth", vec![], "impresspress/admin");
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
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn fresh_user_is_active() {
        let ctx =
            TestContext::with_auth()
                .await
                .with_wrap("wafer-run/auth", vec![], "impresspress/admin");
        let id = seed_active(&ctx).await;
        let row = find_by_id(&ctx, &id).await.unwrap().unwrap();
        assert!(row.is_active());
        assert!(!row.is_deleted());
        assert_eq!(row.deleted_at, None);
    }

    #[tokio::test]
    async fn disabled_user_is_not_active() {
        let ctx =
            TestContext::with_auth()
                .await
                .with_wrap("wafer-run/auth", vec![], "impresspress/admin");
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
        let ctx =
            TestContext::with_auth()
                .await
                .with_wrap("wafer-run/auth", vec![], "impresspress/admin");
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
