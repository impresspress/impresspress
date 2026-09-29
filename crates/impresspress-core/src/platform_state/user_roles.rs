//! `impresspress__admin__user_roles`: role grants beyond a user's initial
//! role — one row per `(user_id, role)`, which a unique index over the pair
//! enforces wherever admin migration 004 has run (every native and Cloudflare
//! deployment and every new browser install; not a browser install created
//! before it — see the note beside that migration) — read by the framework auth block on
//! every login (`get_user_roles` merges them with the inline `users.role`)
//! and managed by admin's IAM surface.
//!
//! Runtime flavour only (spec 2.1.2): nothing reads these rows before WRAP.
//! [`assign`] is the single writer — the login-time admin grant
//! (`auth::helpers::TokenGrant::resolve`) and admin's assign endpoint both go through it, so
//! every row has the same shape. Signup writes no row: the initial role is
//! the inline `users.role`, and a row here means "granted beyond it" (spec
//! 2.2.3).

use std::collections::HashMap;

use serde_json::{json, Value};
use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, WaferError};

use crate::{
    db_read::{self, Bound, CappedList},
    util::RecordExt,
};

pub const TABLE: &str = "impresspress__admin__user_roles";

/// One row of the user_roles table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRoleRow {
    pub id: String,
    pub user_id: String,
    /// The role NAME (`admin/iam.rs` renames cascade here), not a role id.
    pub role: String,
    /// When the grant was made; nullable in the schema.
    pub assigned_at: Option<String>,
    /// The admin who granted it; empty for a grant the system made.
    pub assigned_by: String,
    pub created_at: String,
    pub updated_at: String,
}

impl UserRoleRow {
    /// Decode one row. `user_id` and `role` are required (both `NOT NULL`);
    /// a row without them grants nothing and is refused rather than
    /// defaulted.
    pub fn from_record(id: &str, data: &HashMap<String, Value>) -> Result<Self, String> {
        let user_id = data.str_field("user_id");
        if user_id.is_empty() {
            return Err(format!("{TABLE} row `{id}` has no user_id"));
        }
        let role = data.str_field("role");
        if role.is_empty() {
            return Err(format!("{TABLE} row `{id}` has no role"));
        }
        Ok(Self {
            id: id.to_string(),
            user_id: user_id.to_string(),
            role: role.to_string(),
            assigned_at: data.opt_str_field("assigned_at"),
            assigned_by: data.str_field("assigned_by").to_string(),
            created_at: data.str_field("created_at").to_string(),
            updated_at: data.str_field("updated_at").to_string(),
        })
    }

    /// The column map this row inserts as. `assigned_at` is omitted when
    /// `None` so the nullable column stays NULL.
    pub fn to_data(&self) -> HashMap<String, Value> {
        let mut data = HashMap::new();
        data.insert("id".to_string(), json!(self.id));
        data.insert("user_id".to_string(), json!(self.user_id));
        data.insert("role".to_string(), json!(self.role));
        if let Some(assigned_at) = &self.assigned_at {
            data.insert("assigned_at".to_string(), json!(assigned_at));
        }
        data.insert("assigned_by".to_string(), json!(self.assigned_by));
        data.insert("created_at".to_string(), json!(self.created_at));
        data.insert("updated_at".to_string(), json!(self.updated_at));
        data
    }
}

/// What [`assign`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assigned {
    /// The grant did not exist and was written.
    Created(UserRoleRow),
    /// The user already held the role; nothing was written.
    AlreadyAssigned,
}

fn decode_error(e: String) -> WaferError {
    WaferError::new(ErrorCode::Internal, e)
}

fn eq(field: &str, value: &str) -> Filter {
    Filter {
        field: field.to_string(),
        operator: FilterOp::Equal,
        value: Value::String(value.to_string()),
    }
}

/// Decode grant rows, warning about and skipping a row that does not decode
/// — the policy the auth block's role merge and admin's bulk role fetch have
/// always applied to a malformed row.
fn decode_rows(records: Vec<db::Record>) -> Vec<UserRoleRow> {
    records
        .iter()
        .filter_map(|r| match UserRoleRow::from_record(&r.id, &r.data) {
            Ok(row) => Some(row),
            Err(e) => {
                tracing::warn!(error = %e, "user_roles table contains an undecodable row");
                None
            }
        })
        .collect()
}

/// List the grants `filters` selects, for a filter that pins the read to one
/// principal or one page of them.
///
/// One row per extra role a user holds, so the matching set is as small as
/// the number of roles the deployment defines. The unfiltered and
/// filtered-by-role reads are a different shape and have their own functions
/// ([`list_all`], [`list_by_role`]) — this table's row count grows with the
/// user base, so "every grant" is never a bounded read.
async fn list_for_principals(
    ctx: &dyn Context,
    filters: Vec<Filter>,
    bound: Bound,
) -> Result<Vec<UserRoleRow>, WaferError> {
    Ok(decode_rows(
        db_read::list_bounded(ctx, TABLE, filters, bound).await?,
    ))
}

/// Every grant `user_id` holds.
pub async fn list_for_user(
    ctx: &dyn Context,
    user_id: &str,
) -> Result<Vec<UserRoleRow>, WaferError> {
    list_for_principals(
        ctx,
        vec![eq("user_id", user_id)],
        Bound::OnePer("extra role one user holds"),
    )
    .await
}

/// Every grant any of `user_ids` holds, in one `In` query. The bulk lookup
/// behind admin's user list; no users, no query.
pub async fn list_for_users(
    ctx: &dyn Context,
    user_ids: &[&str],
) -> Result<Vec<UserRoleRow>, WaferError> {
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    let values: Vec<Value> = user_ids
        .iter()
        .map(|id| Value::String((*id).to_string()))
        .collect();
    list_for_principals(
        ctx,
        vec![Filter {
            field: "user_id".to_string(),
            operator: FilterOp::In,
            value: Value::Array(values),
        }],
        Bound::OnePer("extra role held by one of the user ids on one admin list page"),
    )
    .await
}

/// Grants across the whole deployment, for the IAM listing, and whether
/// there are more than the listing returned.
///
/// The table holds one row per extra role per user, so it grows with the user
/// base: this read is capped and says so, rather than presenting a prefix as
/// the complete grant list.
pub async fn list_all(ctx: &dyn Context) -> Result<CappedList<UserRoleRow>, WaferError> {
    let capped = db_read::list_capped(ctx, TABLE, vec![]).await?;
    Ok(CappedList {
        rows: decode_rows(capped.rows),
        truncated: capped.truncated,
    })
}

/// How many grants the table holds, deployment-wide — the honest
/// `total_count` beside [`list_all`]'s capped page.
pub async fn count_all(ctx: &dyn Context) -> Result<i64, WaferError> {
    db::count(ctx, TABLE, &[]).await
}

/// EVERY grant of `role`, for a rename to carry along or a delete to revoke.
///
/// Exhaustive on purpose. A role rename rewrites each grant, a role delete
/// removes each one, and both bump each grantee's auth version; a grant this
/// read missed would keep naming a role that no longer exists — its holder
/// would keep being minted tokens carrying that name, and would silently get
/// it back if a role of the same name were created again. There is no size at
/// which it is acceptable to stop early.
pub async fn list_by_role(ctx: &dyn Context, role: &str) -> Result<Vec<UserRoleRow>, WaferError> {
    Ok(decode_rows(
        db_read::list_every(ctx, TABLE, vec![eq("role", role)]).await?,
    ))
}

/// The grant with `id`, if any.
pub async fn get(ctx: &dyn Context, id: &str) -> Result<Option<UserRoleRow>, WaferError> {
    match db::get(ctx, TABLE, id).await {
        Ok(rec) => UserRoleRow::from_record(&rec.id, &rec.data)
            .map(Some)
            .map_err(decode_error),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether `user_id` holds a grant of `role` — the pair the table's unique
/// index is over.
async fn holds(ctx: &dyn Context, user_id: &str, role: &str) -> Result<bool, WaferError> {
    let rows = list_for_principals(
        ctx,
        vec![eq("user_id", user_id), eq("role", role)],
        Bound::OnePer("grant of one role to one user"),
    )
    .await?;
    Ok(!rows.is_empty())
}

/// Grant `role` to `user_id` unless they already hold it. `assigned_by` is
/// the granting admin's id, or empty for a grant the system makes. The
/// single writer for this table.
///
/// The unique index over `(user_id, role)` (admin migration 004, where it
/// has run) is what makes this safe to run concurrently — two logins of the bootstrap admin
/// both reach it through `TokenGrant::resolve`. The read first is only the
/// cheap answer for the common repeat; two callers can both pass it, and the
/// insert of whichever comes second is then refused by the index and
/// reported as [`Assigned::AlreadyAssigned`] rather than as a failure.
pub async fn assign(
    ctx: &dyn Context,
    user_id: &str,
    role: &str,
    assigned_by: &str,
) -> Result<Assigned, WaferError> {
    if holds(ctx, user_id, role).await? {
        return Ok(Assigned::AlreadyAssigned);
    }
    let now = crate::util::now_rfc3339();
    let row = UserRoleRow {
        id: format!("ur_{}", uuid::Uuid::new_v4()),
        user_id: user_id.to_string(),
        role: role.to_string(),
        assigned_at: Some(now.clone()),
        assigned_by: assigned_by.to_string(),
        created_at: now.clone(),
        updated_at: now,
    };
    let rec = match db::create(ctx, TABLE, row.to_data()).await {
        Ok(rec) => rec,
        // The unique index over `(user_id, role)` refused it: the grant is
        // held. `AlreadyExists` is how every backend reports that.
        Err(e) if e.code == ErrorCode::AlreadyExists => return Ok(Assigned::AlreadyAssigned),
        Err(e) => return Err(e),
    };
    UserRoleRow::from_record(&rec.id, &rec.data)
        .map(Assigned::Created)
        .map_err(decode_error)
}

/// Point `grant` at `new_role` (a role definition was renamed).
///
/// When its holder already has a grant naming `new_role` the unique index
/// refuses the rewrite, and `grant` is revoked instead: the holder keeps the
/// role through the grant they already had, and ends up with exactly the one
/// grant a rename leaves everyone else with.
pub async fn rename_role(
    ctx: &dyn Context,
    grant: &UserRoleRow,
    new_role: &str,
) -> Result<(), WaferError> {
    let mut data = HashMap::new();
    data.insert("role".to_string(), json!(new_role));
    data.insert("updated_at".to_string(), json!(crate::util::now_rfc3339()));
    match db::update(ctx, TABLE, &grant.id, data).await {
        Ok(_) => Ok(()),
        Err(e) if e.code == ErrorCode::AlreadyExists => remove(ctx, &grant.id).await,
        Err(e) => Err(e),
    }
}

/// Revoke every grant of `role`, in one statement, and say how many there
/// were. For a role that is being deleted: unlike a remove per row read
/// beforehand, this also takes a grant written after that read.
pub async fn revoke_role(ctx: &dyn Context, role: &str) -> Result<i64, WaferError> {
    db::delete_by_filters_count(ctx, TABLE, vec![eq("role", role)]).await
}

/// Revoke the grant with `id`. `NotFound` when there is none.
pub async fn remove(ctx: &dyn Context, id: &str) -> Result<(), WaferError> {
    db::delete(ctx, TABLE, id).await
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestContext;

    /// The codec: every column `assign` writes comes back through
    /// `list_for_user`, and a second grant of the same role is reported
    /// rather than duplicated.
    #[tokio::test]
    async fn assign_and_list_for_user_round_trip_and_are_idempotent() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let created = match assign(&ctx, "u-1", "editor", "admin_1")
            .await
            .expect("assign")
        {
            Assigned::Created(row) => row,
            Assigned::AlreadyAssigned => panic!("first grant must create the row"),
        };
        assert!(created.id.starts_with("ur_"), "{}", created.id);
        assert_eq!(created.user_id, "u-1");
        assert_eq!(created.role, "editor");
        assert!(created
            .assigned_at
            .as_deref()
            .is_some_and(|at| !at.is_empty()));
        assert_eq!(created.assigned_by, "admin_1");
        assert!(!created.created_at.is_empty());
        assert_eq!(created.created_at, created.updated_at);

        let rows = list_for_user(&ctx, "u-1").await.expect("list");
        assert_eq!(rows, vec![created.clone()]);

        let again = UserRoleRow::from_record(&created.id, &created.to_data()).expect("decode");
        assert_eq!(again, created);

        assert!(matches!(
            assign(&ctx, "u-1", "editor", "admin_2")
                .await
                .expect("second grant"),
            Assigned::AlreadyAssigned
        ));
        assert_eq!(
            list_for_user(&ctx, "u-1").await.expect("list").len(),
            1,
            "a repeated grant must not add a row"
        );
    }

    /// `TokenGrant::resolve` grants with no admin behind it; the column keeps
    /// its empty default.
    #[tokio::test]
    async fn assign_by_the_system_leaves_assigned_by_empty() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let Assigned::Created(row) = assign(&ctx, "u-1", "admin", "").await.expect("assign") else {
            panic!("first grant must create the row");
        };
        assert_eq!(row.assigned_by, "");
    }

    /// The bulk lookup behind the admin user list buckets every requested
    /// user in one query, and asks nothing for no users.
    #[tokio::test]
    async fn list_for_users_covers_every_requested_user() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        assign(&ctx, "u-1", "editor", "").await.expect("assign");
        assign(&ctx, "u-1", "auditor", "").await.expect("assign");
        assign(&ctx, "u-2", "editor", "").await.expect("assign");
        assign(&ctx, "u-3", "editor", "").await.expect("assign");

        let mut rows = list_for_users(&ctx, &["u-1", "u-2"]).await.expect("list");
        rows.sort_by(|a, b| (&a.user_id, &a.role).cmp(&(&b.user_id, &b.role)));
        let pairs: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.user_id.as_str(), r.role.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![("u-1", "auditor"), ("u-1", "editor"), ("u-2", "editor")]
        );
        assert!(list_for_users(&ctx, &[]).await.expect("empty").is_empty());
    }

    /// A role rename carries every grant naming the old value with it.
    #[tokio::test]
    async fn rename_role_moves_a_grant_and_list_by_role_finds_it() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let Assigned::Created(row) = assign(&ctx, "u-1", "editor", "").await.expect("assign")
        else {
            panic!("first grant must create the row");
        };
        assign(&ctx, "u-2", "viewer", "").await.expect("assign");

        let editors = list_by_role(&ctx, "editor").await.expect("list");
        assert_eq!(editors.len(), 1);
        rename_role(&ctx, &row, "editor-v2").await.expect("rename");
        assert!(list_by_role(&ctx, "editor").await.expect("list").is_empty());
        let renamed = list_by_role(&ctx, "editor-v2").await.expect("list");
        assert_eq!(renamed.len(), 1);
        assert_eq!(renamed[0].id, row.id);
        assert_eq!(list_all(&ctx).await.expect("all").rows.len(), 2);
    }

    /// A role rename has to carry EVERY grant of that role, so the read
    /// behind it is exhaustive rather than capped.
    ///
    /// A grant this read stopped short of would keep naming a role that no
    /// longer exists, and its holder would keep a token minted under the old
    /// name. The `db::list_all` assertion is the witness that the ceiling is
    /// real at this table size — it is what the capped read this replaced
    /// would have returned.
    #[tokio::test]
    async fn list_by_role_returns_every_grant_past_the_unpaged_ceiling() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let past_the_ceiling = i64::from(crate::db_read::UNPAGED_LIMIT) + 1;
        db::exec_raw(
            &ctx,
            "WITH RECURSIVE seq(n) AS ( \
                 SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < ? \
             ) \
             INSERT INTO impresspress__admin__user_roles \
                 (id, user_id, role, assigned_by, assigned_at, created_at, updated_at) \
             SELECT 'ur_' || printf('%06d', n), 'u_' || printf('%06d', n), 'editor', '', \
                    '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z' \
             FROM seq",
            &[json!(past_the_ceiling)],
        )
        .await
        .expect("seed grants");

        let editors = list_by_role(&ctx, "editor").await.expect("list");
        assert_eq!(editors.len() as i64, past_the_ceiling);
        let one_shot = crate::db_read::list_capped(&ctx, TABLE, vec![])
            .await
            .expect("one-shot read");
        assert_eq!(one_shot.rows.len(), crate::db_read::UNPAGED_LIMIT as usize);
        assert!(
            one_shot.truncated,
            "a one-shot read of this table stops one grant short, which is \
             what the cascade used to act on"
        );
    }

    /// The unfiltered listing is capped, and says so rather than presenting a
    /// prefix as the whole grant list.
    #[tokio::test]
    async fn list_all_reports_that_it_is_a_prefix() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let past_the_ceiling = i64::from(crate::db_read::UNPAGED_LIMIT) + 1;
        db::exec_raw(
            &ctx,
            "WITH RECURSIVE seq(n) AS ( \
                 SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < ? \
             ) \
             INSERT INTO impresspress__admin__user_roles \
                 (id, user_id, role, assigned_by, assigned_at, created_at, updated_at) \
             SELECT 'ur_' || printf('%06d', n), 'u_' || printf('%06d', n), 'viewer', '', \
                    '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z' \
             FROM seq",
            &[json!(past_the_ceiling)],
        )
        .await
        .expect("seed grants");

        let listed = list_all(&ctx).await.expect("all");
        assert!(listed.truncated);
        assert_eq!(listed.rows.len(), crate::db_read::UNPAGED_LIMIT as usize);
        assert_eq!(count_all(&ctx).await.expect("count"), past_the_ceiling);
    }

    /// A role's grants go in one statement, every holder's, and only that
    /// role's.
    #[tokio::test]
    async fn revoke_role_takes_every_grant_of_that_role_and_nothing_else() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        for (user, role) in [("u-1", "editor"), ("u-2", "editor"), ("u-1", "viewer")] {
            assign(&ctx, user, role, "").await.expect("assign");
        }
        assert_eq!(revoke_role(&ctx, "editor").await.expect("revoke"), 2);
        assert!(list_by_role(&ctx, "editor").await.expect("list").is_empty());
        assert_eq!(list_by_role(&ctx, "viewer").await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn get_and_remove() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let Assigned::Created(row) = assign(&ctx, "u-1", "editor", "").await.expect("assign")
        else {
            panic!("first grant must create the row");
        };
        assert_eq!(get(&ctx, &row.id).await.expect("get"), Some(row.clone()));
        remove(&ctx, &row.id).await.expect("remove");
        assert_eq!(get(&ctx, &row.id).await.expect("get"), None);
        let err = remove(&ctx, &row.id)
            .await
            .expect_err("removing a gone grant is NotFound");
        assert_eq!(err.code, ErrorCode::NotFound);
    }

    #[test]
    fn a_record_without_a_user_or_role_does_not_decode() {
        for missing in ["user_id", "role"] {
            let mut data = HashMap::new();
            data.insert("user_id".to_string(), serde_json::json!("u-1"));
            data.insert("role".to_string(), serde_json::json!("editor"));
            data.remove(missing);
            let err = UserRoleRow::from_record("ur_1", &data).expect_err(missing);
            assert!(err.contains(missing) && err.contains("ur_1"), "{err}");
        }
    }
}
