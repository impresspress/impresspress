//! Row-level access over `wafer_run__auth__sessions` — one row per login
//! family (B12).
//!
//! A row here is a *device*: one signed-in login family. Its key is the
//! refresh rotation `family` that `helpers::generate_tokens` mints, so the row
//! created when a user signs in is the same row every subsequent token refresh
//! touches, and it expires exactly when the refresh row it mirrors does. It
//! feeds `/b/userportal/sessions`, the list of devices a user can see and
//! revoke.
//!
//! ## The row is the family's liveness
//!
//! A family whose row is gone is signed out: its next refresh is refused, and
//! every revocation deletes the row before it revokes the refresh rows
//! (`helpers::end_family`). Its first refresh row
//! and this row are written in one batch ([`create_op`] beside
//! `tokens::create_op`), so a family never has one without the other. A
//! rotation refreshes this row ([`touch`]) after writing its successor token,
//! and when the touch finds no row the family was revoked while the rotation
//! ran: the rotation revokes the family's refresh rows and refuses. Nothing
//! re-creates a missing row. Revoking a family therefore deletes its row FIRST
//! and revokes its refresh rows second; `helpers::end_family` says why that
//! order closes the race with a concurrent rotation.
//!
//! The refresh rows (`super::tokens`) answer a different question — is the
//! presented token the family's current generation, or a replayed copy — and
//! never this one.
//!
//! Before migration 012 the key was `sha256(access_token)`, which made every
//! token issuance a new row: a browser tab wrote roughly one row per 30
//! minutes, each lived 30 days, logout deleted none, and each showed on the
//! device list as its own device.
//!
//! Because the row is the family's liveness, no migration may drop this
//! table: the auth migrations re-run as a set whenever any of them changes,
//! and a drop there would sign every device out on every such change. That is
//! why the family-keyed layout is declared in `001_auth_schema` and 012 no
//! longer drops and recreates it.
//!
//! ## Schema convention
//!
//! Rows are written through `db::create`, which synthesizes a TEXT `id` and
//! stamps `created_at`/`updated_at`, on top of the `family` primary key. Both
//! bookkeeping columns are added by `010_strict_schema_columns` to the table
//! `001_auth_schema` declares. Every read and write below addresses rows by
//! `family` or `user_id` through filter-based operations, so the synthesized
//! `id` is never needed.

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::{
    db::{Filter, FilterOp, SortField},
    wire::database::BatchWrite,
};
use wafer_core::clients::database::{self as db, RecordData};
use wafer_run::{context::Context, WaferError};

use super::{db_failed, internal_error, map_str, now_iso};
use crate::db_read::{self, Bound};

pub const TABLE: &str = "wafer_run__auth__sessions";

/// One login family: a device the user is signed in on.
#[derive(Debug, Clone)]
pub struct SessionRow {
    /// The refresh rotation family — the primary key, and the value the
    /// access JWT's `family` claim carries so the userportal can mark the
    /// device making the request.
    pub family: String,
    pub user_id: String,
    /// How the session was established: `"password"`, `"oauth.<provider>"` or
    /// `"bootstrap"`. Preserved across rotation by the refresh token's own
    /// `auth_method` claim.
    pub auth_method: String,
    pub created_at: String,
    /// Bumped on every token refresh, so the list can order by recency.
    pub last_used_at: String,
    /// The expiry of the refresh row this family anchors. When it passes, the
    /// device really is signed out.
    pub expires_at: String,
}

#[derive(Debug, Clone)]
pub struct NewSession {
    pub family: String,
    pub user_id: String,
    pub auth_method: String,
    pub expires_at: String,
}

fn row_from_map(m: &RecordData) -> Result<SessionRow, WaferError> {
    Ok(SessionRow {
        family: super::map_opt_str(m, "family").ok_or_else(|| internal_error("missing family"))?,
        user_id: super::map_opt_str(m, "user_id")
            .ok_or_else(|| internal_error("missing user_id"))?,
        auth_method: map_str(m, "auth_method"),
        created_at: map_str(m, "created_at"),
        last_used_at: map_str(m, "last_used_at"),
        expires_at: map_str(m, "expires_at"),
    })
}

fn family_filter(family: &str) -> Filter {
    Filter {
        field: "family".into(),
        operator: FilterOp::Equal,
        value: json!(family),
    }
}

fn user_filter(user_id: &str) -> Filter {
    Filter {
        field: "user_id".into(),
        operator: FilterOp::Equal,
        value: json!(user_id),
    }
}

/// The row for a brand-new login family, as one write of the batch that also
/// writes the family's first refresh row (`tokens::create_op`). Issuance
/// writes the two together so that neither lands without the other.
pub fn create_op(new: NewSession) -> BatchWrite {
    BatchWrite::Create {
        collection: TABLE.to_string(),
        data: new_row(new),
    }
}

/// [`create_op`]'s row written on its own: test fixtures that sign an access
/// token outside the issuance path use it to give that token's `family` the
/// row a real sign-in would have written.
#[cfg(any(test, feature = "test-support"))]
pub async fn insert(ctx: &dyn Context, new: NewSession) -> Result<(), WaferError> {
    db::create(ctx, TABLE, new_row(new))
        .await
        .map_err(|e| db_failed("insert session", e))?;
    Ok(())
}

fn new_row(new: NewSession) -> HashMap<String, Value> {
    let now = now_iso();
    let mut data = HashMap::new();
    data.insert("family".into(), json!(new.family));
    data.insert("user_id".into(), json!(new.user_id));
    data.insert("auth_method".into(), json!(new.auth_method));
    data.insert("created_at".into(), json!(now));
    data.insert("last_used_at".into(), json!(now));
    data.insert("expires_at".into(), json!(new.expires_at));
    data
}

/// Bump `last_used_at` to now and carry `expires_at` forward for `family`.
///
/// Returns the number of rows affected. `0` means the family has no row: it
/// was revoked (or expired and was swept), and the rotation that asked must
/// refuse rather than continue it — see the module doc. `created_at` is
/// deliberately untouched: it is when the device signed in, and rotation is
/// not a new sign-in.
pub async fn touch(ctx: &dyn Context, family: &str, expires_at: &str) -> Result<u64, WaferError> {
    let mut data = HashMap::new();
    data.insert("last_used_at".into(), json!(now_iso()));
    data.insert("expires_at".into(), json!(expires_at));
    let n = db::update_by_filters_count(ctx, TABLE, vec![family_filter(family)], data)
        .await
        .map_err(|e| db_failed("session touch", e))?;
    Ok(n.max(0) as u64)
}

/// The row for `family`, but only if it belongs to `user_id`.
///
/// The ownership gate the userportal revoke needs: `tokens::revoke_family`
/// takes a family and no user, so the caller has to establish that the family
/// is theirs before asking for it to be burned.
pub async fn find_for_user(
    ctx: &dyn Context,
    user_id: &str,
    family: &str,
) -> Result<Option<SessionRow>, WaferError> {
    let records = db_read::list_bounded(
        ctx,
        TABLE,
        vec![family_filter(family), user_filter(user_id)],
        Bound::UniqueKey("sessions.family is the PRIMARY KEY"),
    )
    .await
    .map_err(|e| db_failed("session find_for_user", e))?;
    match records.first() {
        Some(r) => Ok(Some(row_from_map(&r.data)?)),
        None => Ok(None),
    }
}

/// Return all of `user_id`'s families, ordered by `last_used_at` DESC so the
/// most recently active device sorts first, and by family among devices last
/// used in the same second.
pub async fn list_for_user(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<Vec<SessionRow>, WaferError> {
    let records = db_read::list_bounded_sorted(
        ctx,
        TABLE,
        vec![user_filter(user_id)],
        // `last_used_at` has seconds resolution, so two devices used in the
        // same second tie; the family breaks the tie, or their order would
        // flip between loads of the list.
        vec![
            SortField {
                field: "last_used_at".into(),
                desc: true,
            },
            SortField {
                field: "family".into(),
                desc: false,
            },
        ],
        Bound::OnePer("device family one user has signed in from"),
    )
    .await
    .map_err(|e| db_failed("session list_for_user", e))?;
    records.iter().map(|r| row_from_map(&r.data)).collect()
}

/// Delete the row for `family`. Returns the number deleted (0 if absent).
/// This is what ends the family: see the module doc.
///
/// Unscoped on purpose: its one caller, `helpers::end_family`, is reached
/// only by code that already knows whose family it is — the userportal
/// revoke through [`find_for_user`], and refresh from the refresh row it
/// read. [`delete_all_for_user`] is what logout uses.
pub async fn delete(ctx: &dyn Context, family: &str) -> Result<u64, WaferError> {
    let n = db::delete_by_filters_count(ctx, TABLE, vec![family_filter(family)])
        .await
        .map_err(|e| db_failed("session delete", e))?;
    Ok(n.max(0) as u64)
}

/// Delete every row for `user_id`. Returns the number deleted.
///
/// Logout's counterpart to `tokens::revoke_all_for_user`: the two run
/// together so the device list and the refresh tokens agree about which
/// devices are still signed in. This one runs first, for the reason
/// `helpers::end_family` gives.
pub async fn delete_all_for_user(ctx: &dyn Context, user_id: &str) -> Result<u64, WaferError> {
    let n = db::delete_by_filters_count(ctx, TABLE, vec![user_filter(user_id)])
        .await
        .map_err(|e| db_failed("session delete_all_for_user", e))?;
    Ok(n.max(0) as u64)
}

/// Deletes rows whose `expires_at < cutoff`. Returns the number deleted.
///
/// `cutoff` is compared as an ISO-8601 string; rows store timestamps in the
/// same text format (see [`now_iso`]). Called by `auth::maintenance::sweep`.
pub async fn delete_expired(ctx: &dyn Context, cutoff: &str) -> Result<u64, WaferError> {
    let n = db::delete_by_filters_count(
        ctx,
        TABLE,
        vec![Filter {
            field: "expires_at".into(),
            operator: FilterOp::LessThan,
            value: json!(cutoff),
        }],
    )
    .await
    .map_err(|e| db_failed("session delete_expired", e))?;
    Ok(n.max(0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestContext;

    fn fake_session(user_id: &str, family: &str) -> NewSession {
        NewSession {
            family: family.into(),
            user_id: user_id.into(),
            auth_method: "password".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
        }
    }

    /// Seed a user row directly via SQL so the test can pin a deterministic
    /// `user_id` — `db::create` would generate one. The row provides every
    /// NOT NULL column required by the auth migration.
    async fn seed_user(ctx: &TestContext, user_id: &str) {
        ctx.seed_auth_user(user_id).await;
    }

    #[tokio::test]
    async fn list_for_user_returns_only_caller_sessions() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        for user_id in ["user-a", "user-b"] {
            seed_user(&ctx, user_id).await;
        }
        insert(&ctx, fake_session("user-a", "fam-a1"))
            .await
            .unwrap();
        insert(&ctx, fake_session("user-a", "fam-a2"))
            .await
            .unwrap();
        insert(&ctx, fake_session("user-b", "fam-b1"))
            .await
            .unwrap();

        let a = list_for_user(&ctx, "user-a").await.unwrap();
        let b = list_for_user(&ctx, "user-b").await.unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 1);
    }

    /// Devices last used in the same second (the column's resolution) keep
    /// one order, by family, rather than whatever the store returns.
    #[tokio::test]
    async fn list_for_user_breaks_a_last_used_tie_by_family() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        for family in ["fam-c", "fam-a", "fam-d", "fam-b"] {
            insert(&ctx, fake_session("user-a", family)).await.unwrap();
        }
        let rows = list_for_user(&ctx, "user-a").await.unwrap();
        assert_eq!(rows.len(), 4);
        // The four inserts land in one second or straddle a boundary; either
        // way each pair is newest first, and a tie is in family order.
        for pair in rows.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(
                a.last_used_at > b.last_used_at
                    || (a.last_used_at == b.last_used_at && a.family < b.family),
                "{} {} before {} {}",
                a.last_used_at,
                a.family,
                b.last_used_at,
                b.family
            );
        }
    }

    /// `find_for_user` is the ownership gate the revoke path depends on: it
    /// must not hand a user another user's family.
    #[tokio::test]
    async fn find_for_user_refuses_another_users_family() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        for user_id in ["user-a", "user-b"] {
            seed_user(&ctx, user_id).await;
        }
        insert(&ctx, fake_session("user-b", "fam-b1"))
            .await
            .unwrap();

        assert!(find_for_user(&ctx, "user-a", "fam-b1")
            .await
            .unwrap()
            .is_none());
        let mine = find_for_user(&ctx, "user-b", "fam-b1").await.unwrap();
        assert_eq!(mine.expect("own family").family, "fam-b1");
    }

    /// The property the whole re-key exists for: N refreshes leave one row.
    #[tokio::test]
    async fn touch_updates_the_one_row_and_reports_it() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-1")).await.unwrap();

        for _ in 0..5 {
            assert_eq!(
                touch(&ctx, "fam-1", "2100-01-01T00:00:00Z").await.unwrap(),
                1,
                "a rotation touches the family's existing row"
            );
        }
        let rows = list_for_user(&ctx, "user-a").await.unwrap();
        assert_eq!(rows.len(), 1, "five rotations, one device, one row");
        assert_eq!(
            rows[0].expires_at, "2100-01-01T00:00:00Z",
            "the row carries the new refresh expiry"
        );
    }

    /// `touch` on a family with no row reports zero rather than erroring —
    /// that is the signal issuance uses to insert one instead.
    #[tokio::test]
    async fn touch_reports_zero_for_an_unknown_family() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        assert_eq!(
            touch(&ctx, "fam-missing", "2100-01-01T00:00:00Z")
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn delete_and_delete_all_for_user_count_rows() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        for user_id in ["user-a", "user-b"] {
            seed_user(&ctx, user_id).await;
        }
        insert(&ctx, fake_session("user-a", "fam-a1"))
            .await
            .unwrap();
        insert(&ctx, fake_session("user-a", "fam-a2"))
            .await
            .unwrap();
        insert(&ctx, fake_session("user-b", "fam-b1"))
            .await
            .unwrap();

        assert_eq!(delete(&ctx, "fam-missing").await.unwrap(), 0);
        assert_eq!(delete(&ctx, "fam-a1").await.unwrap(), 1);
        assert_eq!(delete_all_for_user(&ctx, "user-a").await.unwrap(), 1);
        assert!(list_for_user(&ctx, "user-a").await.unwrap().is_empty());
        assert_eq!(
            list_for_user(&ctx, "user-b").await.unwrap().len(),
            1,
            "another user's devices are untouched"
        );
    }

    #[tokio::test]
    async fn delete_expired_removes_only_past_rows() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        insert(&ctx, fake_session("user-a", "fam-live"))
            .await
            .unwrap();
        insert(
            &ctx,
            NewSession {
                family: "fam-dead".into(),
                user_id: "user-a".into(),
                auth_method: "password".into(),
                expires_at: "1970-01-02T00:00:00Z".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            delete_expired(&ctx, "2000-01-01T00:00:00Z").await.unwrap(),
            1
        );
        let rows = list_for_user(&ctx, "user-a").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].family, "fam-live");
    }

    /// `insert` round-trips every column the device list renders.
    #[tokio::test]
    async fn insert_round_trips_the_family_and_auth_method() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth::AUTH_BLOCK_ID);
        seed_user(&ctx, "user-a").await;
        insert(
            &ctx,
            NewSession {
                family: "fam-oauth".into(),
                user_id: "user-a".into(),
                auth_method: "oauth.github".into(),
                expires_at: "2099-01-01T00:00:00Z".into(),
            },
        )
        .await
        .unwrap();

        let rows = list_for_user(&ctx, "user-a").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].family, "fam-oauth");
        assert_eq!(rows[0].auth_method, "oauth.github");
        assert!(rows[0].created_at.as_str() <= now_iso().as_str());
        assert!(rows[0].expires_at.as_str() > now_iso().as_str());
    }
}
